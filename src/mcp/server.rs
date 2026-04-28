use anyhow::{anyhow, Result};
use serde_json::json;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::{Mutex, RwLock},
};
use tracing::{debug, error, info, warn};

use crate::{
    config::{MAX_RESTART_COUNT, RESTART_WINDOW_SECS},
    lsp::RustAnalyzerClient,
    protocol::mcp::{MCPError, MCPRequest, MCPResponse},
};

pub struct RustAnalyzerMCPServer {
    pub(super) client: RwLock<Option<Arc<RustAnalyzerClient>>>,
    pub(super) workspace_root: RwLock<PathBuf>,
    /// Timestamps of recent automatic restarts, oldest first. Used to back off
    /// when rust-analyzer keeps crashing (see `MAX_RESTART_COUNT`).
    restart_history: parking_lot::Mutex<VecDeque<Instant>>,
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

        // Skip disk read entirely if rust-analyzer already has this document open.
        if client.is_open(&uri).await {
            return Ok(uri);
        }

        let content = tokio::fs::read_to_string(&absolute_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read file {}: {}", file_path, e))?;

        client.open_document(&uri, &content).await?;
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

                    let server = Arc::clone(&self);
                    let stdout = Arc::clone(&stdout);
                    tokio::spawn(async move {
                        server.handle_one_request(request, stdout).await;
                    });
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
                        "tools": {}
                    }
                }),
            },
            "tools/list" => MCPResponse::Success {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: super::tools::tools_list_result().clone(),
            },
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
