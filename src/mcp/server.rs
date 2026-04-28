use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::{Mutex, RwLock},
    task::AbortHandle,
};
use tracing::{debug, error, info, warn};

use crate::{
    config::{MAX_RESTART_COUNT, RESTART_WINDOW_SECS},
    lsp::{client::CURRENT_MCP_REQUEST_ID, RustAnalyzerClient},
    protocol::mcp::{MCPError, MCPRequest, MCPResponse},
};

pub struct RustAnalyzerMCPServer {
    pub(super) client: RwLock<Option<Arc<RustAnalyzerClient>>>,
    pub(super) workspace_root: RwLock<PathBuf>,
    /// Timestamps of recent automatic restarts, oldest first. Used to back off
    /// when rust-analyzer keeps crashing (see `MAX_RESTART_COUNT`).
    restart_history: parking_lot::Mutex<VecDeque<Instant>>,
    /// AbortHandles for currently-running tool calls, keyed by the canonical
    /// string form of the MCP request id. `notifications/cancelled` looks up
    /// the matching handle and aborts the task; tasks remove themselves on
    /// completion.
    in_flight: Arc<parking_lot::Mutex<HashMap<String, AbortHandle>>>,
}

impl Default for RustAnalyzerMCPServer {
    fn default() -> Self {
        Self::new()
    }
}

impl RustAnalyzerMCPServer {
    pub fn new() -> Self {
        Self {
            client: RwLock::new(None),
            workspace_root: RwLock::new(
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
            restart_history: parking_lot::Mutex::new(VecDeque::new()),
            in_flight: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    pub fn with_workspace(workspace_root: PathBuf) -> Self {
        let workspace_root = workspace_root.canonicalize().unwrap_or_else(|_| {
            if workspace_root.is_absolute() {
                workspace_root.clone()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(&workspace_root)
            }
        });

        Self {
            client: RwLock::new(None),
            workspace_root: RwLock::new(workspace_root),
            restart_history: parking_lot::Mutex::new(VecDeque::new()),
            in_flight: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    /// Returns a healthy `RustAnalyzerClient`, starting it if missing or
    /// restarting it if the previous one's process has exited. Restarts are
    /// rate-limited via `restart_history` to avoid hot-loops on a process that
    /// keeps crashing.
    pub(super) async fn ensure_client_started(&self) -> Result<Arc<RustAnalyzerClient>> {
        // Fast path: existing healthy client.
        if let Some(c) = self.client.read().await.as_ref() {
            if !c.is_dead() {
                return Ok(Arc::clone(c));
            }
        }

        // Slow path: take write lock and (re)start.
        let mut guard = self.client.write().await;

        // Re-check under write lock.
        if let Some(c) = guard.as_ref() {
            if !c.is_dead() {
                return Ok(Arc::clone(c));
            }
        }

        // If we're replacing a dead client, that counts as a restart.
        if guard.as_ref().is_some_and(|c| c.is_dead()) {
            self.record_restart()?;
            warn!("rust-analyzer process died; restarting");
            if let Some(old) = guard.take() {
                // Best-effort cleanup in the background — the process is gone,
                // but stdin/document state still needs to be dropped.
                tokio::spawn(async move {
                    let _ = old.shutdown().await;
                });
            }
        }

        let workspace_root = self.workspace_root.read().await.clone();
        let client = Arc::new(RustAnalyzerClient::new(workspace_root));
        client.start().await?;
        *guard = Some(Arc::clone(&client));
        Ok(client)
    }

    /// Records a restart attempt; errors out if too many crashes happened
    /// within the rolling window. Synchronous (parking_lot) lock — held only
    /// long enough to push/pop a small VecDeque.
    fn record_restart(&self) -> Result<()> {
        let mut hist = self.restart_history.lock();
        let now = Instant::now();
        let cutoff = now
            .checked_sub(Duration::from_secs(RESTART_WINDOW_SECS))
            .unwrap_or(now);
        while hist.front().is_some_and(|t| *t < cutoff) {
            hist.pop_front();
        }
        if hist.len() >= MAX_RESTART_COUNT {
            return Err(anyhow!(
                "rust-analyzer crashed {} times in the last {}s; refusing to restart again",
                hist.len(),
                RESTART_WINDOW_SECS
            ));
        }
        hist.push_back(now);
        Ok(())
    }

    pub(super) async fn set_workspace_root(&self, workspace_root: PathBuf) -> Result<()> {
        // Shutdown existing client first.
        if let Some(c) = self.client.write().await.take() {
            let _ = c.shutdown().await;
        }
        let workspace_root = workspace_root.canonicalize().unwrap_or_else(|_| {
            if workspace_root.is_absolute() {
                workspace_root.clone()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(&workspace_root)
            }
        });
        *self.workspace_root.write().await = workspace_root;
        Ok(())
    }

    pub(super) async fn workspace_root_clone(&self) -> PathBuf {
        self.workspace_root.read().await.clone()
    }

    pub(super) async fn open_document_if_needed(&self, file_path: &str) -> Result<String> {
        let workspace_root = self.workspace_root.read().await.clone();
        let absolute_path = workspace_root.join(file_path);
        let absolute_path = absolute_path
            .canonicalize()
            .unwrap_or_else(|_| absolute_path.clone());
        let uri = format!("file://{}", absolute_path.display());

        let client = self.ensure_client_started().await?;

        // Cheapest path: open and mtime hasn't moved → skip disk entirely.
        let mtime = tokio::fs::metadata(&absolute_path)
            .await
            .ok()
            .and_then(|m| m.modified().ok());

        if client.is_open_and_fresh(&uri, mtime).await {
            return Ok(uri);
        }

        let content = tokio::fs::read_to_string(&absolute_path)
            .await
            .map_err(|e| anyhow!("Failed to read file {}: {}", file_path, e))?;

        if client.is_open(&uri).await {
            // Already open but the file moved on disk — sync via didChange.
            // update_document only emits a notification when the content hash
            // actually differs.
            client.update_document(&uri, &content, mtime).await?;
        } else {
            client
                .open_document_with_mtime(&uri, &content, mtime)
                .await?;
        }
        Ok(uri)
    }

    pub(super) async fn current_client(&self) -> Result<Arc<RustAnalyzerClient>> {
        self.client
            .read()
            .await
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Client not initialized"))
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        info!("Starting rust-analyzer MCP server");

        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let stdout = Arc::new(Mutex::new(BufWriter::new(tokio::io::stdout())));

        let mut shutdown = std::pin::pin!(tokio::signal::ctrl_c());

        loop {
            let mut line = String::new();
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    info!("Received shutdown signal");
                    break;
                }
                read_res = reader.read_line(&mut line) => {
                    match read_res {
                        Ok(0) => break, // EOF
                        Ok(_) => {}
                        Err(e) => {
                            error!("Error reading from stdin: {}", e);
                            break;
                        }
                    }

                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    let Ok(request) = serde_json::from_str::<MCPRequest>(trimmed) else {
                        debug!("Failed to parse request: {}", trimmed);
                        continue;
                    };

                    // Cancellation notifications are handled inline so we don't
                    // race against the spawn that would otherwise run the
                    // (already-cancelled) request.
                    if request.method == "notifications/cancelled" {
                        let server = Arc::clone(&self);
                        let params = request.params.clone();
                        tokio::spawn(async move {
                            server.handle_cancellation(params.as_ref()).await;
                        });
                        continue;
                    }

                    let server = Arc::clone(&self);
                    let stdout = Arc::clone(&stdout);
                    let in_flight = Arc::clone(&self.in_flight);
                    let id_key = request.id.as_ref().map(canonical_id_key);

                    let handle = match id_key.clone() {
                        Some(key) => tokio::spawn(CURRENT_MCP_REQUEST_ID.scope(key, async move {
                            server.handle_one_request(request, stdout).await;
                        })),
                        None => tokio::spawn(async move {
                            server.handle_one_request(request, stdout).await;
                        }),
                    };

                    if let Some(key) = id_key {
                        let abort = handle.abort_handle();
                        in_flight.lock().insert(key.clone(), abort);

                        // Unregister once the task finishes (normal or aborted).
                        let in_flight_cleanup = Arc::clone(&in_flight);
                        tokio::spawn(async move {
                            let _ = handle.await;
                            in_flight_cleanup.lock().remove(&key);
                        });
                    }
                }
            }
        }

        // Cleanup.
        info!("Shutting down");
        if let Some(c) = self.client.write().await.take() {
            let _ = c.shutdown().await;
        }

        Ok(())
    }

    async fn handle_resources_list(self: &Arc<Self>, request: MCPRequest) -> MCPResponse {
        let workspace_root = self.workspace_root.read().await.clone();
        // list_resources may shell out to `cargo metadata`; offload to a
        // blocking thread so a slow workspace doesn't stall the request reader.
        let listed =
            tokio::task::spawn_blocking(move || super::resources::list_resources(&workspace_root))
                .await;

        match listed {
            Ok(value) => MCPResponse::Success {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: value,
            },
            Err(e) => {
                error!("resources/list join error: {}", e);
                MCPResponse::Error {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    error: MCPError {
                        code: -32603,
                        message: format!("Internal error: {e}"),
                        data: None,
                    },
                }
            }
        }
    }

    async fn handle_resources_read(self: &Arc<Self>, request: MCPRequest) -> MCPResponse {
        let Some(params) = request.params else {
            return MCPResponse::Error {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                error: MCPError {
                    code: -32602,
                    message: "Invalid params".to_string(),
                    data: None,
                },
            };
        };
        let Some(uri) = params["uri"].as_str().map(|s| s.to_string()) else {
            return MCPResponse::Error {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                error: MCPError {
                    code: -32602,
                    message: "Missing uri".to_string(),
                    data: None,
                },
            };
        };

        let workspace_root = self.workspace_root.read().await.clone();

        // Filesystem walk is sync; offload to a blocking thread so we don't
        // stall the request reader on a large workspace.
        let read = tokio::task::spawn_blocking(move || {
            super::resources::read_resource(&workspace_root, &uri)
        })
        .await;

        match read {
            Ok(Ok(value)) => MCPResponse::Success {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: value,
            },
            Ok(Err(e)) => MCPResponse::Error {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                error: MCPError {
                    code: -32602,
                    message: e.to_string(),
                    data: None,
                },
            },
            Err(e) => {
                error!("resources/read join error: {}", e);
                MCPResponse::Error {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    error: MCPError {
                        code: -32603,
                        message: format!("Internal error: {e}"),
                        data: None,
                    },
                }
            }
        }
    }

    async fn handle_cancellation(self: Arc<Self>, params: Option<&Value>) {
        let Some(params) = params else {
            debug!("notifications/cancelled without params, ignoring");
            return;
        };
        let Some(request_id) = params.get("requestId") else {
            debug!("notifications/cancelled without requestId, ignoring");
            return;
        };
        let key = canonical_id_key(request_id);

        // Forward `$/cancelRequest` to rust-analyzer for any LSP calls this
        // MCP request had in flight, so the upstream work actually stops
        // instead of being abandoned. Do this before aborting the spawn so the
        // tracker still has the LSP ids registered.
        if let Some(client) = self.client.read().await.as_ref().map(Arc::clone) {
            client.cancel_mcp(&key).await;
        }

        let abort = self.in_flight.lock().remove(&key);
        match abort {
            Some(handle) => {
                info!("Cancelling MCP request {}", key);
                handle.abort();
            }
            None => debug!("notifications/cancelled for unknown request {}", key),
        }
    }

    async fn handle_one_request(
        self: Arc<Self>,
        request: MCPRequest,
        stdout: Arc<Mutex<BufWriter<tokio::io::Stdout>>>,
    ) {
        debug!("Received request: {}", request.method);

        // Notifications (no id) MUST NOT receive a response per JSON-RPC spec.
        let is_notification = request.id.is_none();

        let response = self.handle_request(request).await;

        if is_notification {
            return;
        }

        let response_json = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to serialize response: {}", e);
                return;
            }
        };

        let mut writer = stdout.lock().await;
        if let Err(e) = writer.write_all(response_json.as_bytes()).await {
            error!("Failed to write response: {}", e);
            return;
        }
        if let Err(e) = writer.write_all(b"\n").await {
            error!("Failed to write newline: {}", e);
            return;
        }
        if let Err(e) = writer.flush().await {
            error!("Failed to flush stdout: {}", e);
        }
    }

    async fn handle_request(self: &Arc<Self>, request: MCPRequest) -> MCPResponse {
        match request.method.as_str() {
            "initialize" => MCPResponse::Success {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "rust-analyzer-mcp",
                        "version": "0.1.0"
                    },
                    "capabilities": {
                        "tools": {},
                        "resources": {}
                    }
                }),
            },
            "tools/list" => MCPResponse::Success {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: super::tools::tools_list_result().clone(),
            },
            "resources/list" => self.handle_resources_list(request).await,
            "resources/read" => self.handle_resources_read(request).await,
            "tools/call" => {
                let Some(params) = request.params else {
                    return MCPResponse::Error {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        error: MCPError {
                            code: -32602,
                            message: "Invalid params".to_string(),
                            data: None,
                        },
                    };
                };

                let Some(tool_name) = params["name"].as_str() else {
                    return MCPResponse::Error {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        error: MCPError {
                            code: -32602,
                            message: "Missing tool name".to_string(),
                            data: None,
                        },
                    };
                };

                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));

                match super::handlers::handle_tool_call(self, tool_name, args).await {
                    Ok(result) => MCPResponse::Success {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        result: serde_json::to_value(result).unwrap(),
                    },
                    Err(e) => {
                        error!("Tool call error: {}", e);
                        MCPResponse::Error {
                            jsonrpc: "2.0".to_string(),
                            id: request.id,
                            error: MCPError {
                                code: -1,
                                message: e.to_string(),
                                data: None,
                            },
                        }
                    }
                }
            }
            _ => MCPResponse::Error {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                error: MCPError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                    data: None,
                },
            },
        }
    }
}

/// Canonicalises a JSON-RPC request id (string, number, or null) into a stable
/// HashMap key. Numbers are stringified consistently so that `1` and `1.0` map
/// to the same task.
fn canonical_id_key(id: &Value) -> String {
    match id {
        Value::String(s) => format!("s:{}", s),
        Value::Number(n) => format!("n:{}", n),
        Value::Null => "null".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "unknown".to_string()),
    }
}
