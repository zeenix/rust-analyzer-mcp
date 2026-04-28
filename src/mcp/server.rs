use anyhow::Result;
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::{Mutex, RwLock},
    task::AbortHandle,
};
use tracing::{debug, error, info};

use crate::{
    lsp::client::CURRENT_MCP_REQUEST_ID,
    protocol::mcp::{MCPError, MCPRequest, MCPResponse},
};

use super::workspace::{WorkspaceEntry, WorkspaceRegistry};

pub struct RustAnalyzerMCPServer {
    pub(super) workspaces: RwLock<WorkspaceRegistry>,
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
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_workspace(cwd)
    }

    pub fn with_workspace(workspace_root: PathBuf) -> Self {
        Self {
            workspaces: RwLock::new(WorkspaceRegistry::with_initial_root(workspace_root)),
            in_flight: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    /// Returns the default (first-registered) workspace. Errors if every
    /// workspace has been removed — callers can recover by adding a fresh one.
    pub(super) async fn default_workspace(&self) -> Result<Arc<WorkspaceEntry>> {
        self.workspaces
            .read()
            .await
            .default()
            .ok_or_else(|| anyhow::anyhow!("No default workspace registered"))
    }

    /// Looks up a workspace by id. Errors with a stable message so handlers
    /// can pass it back to the LLM.
    pub(super) async fn workspace_by_id(&self, id: &str) -> Result<Arc<WorkspaceEntry>> {
        self.workspaces
            .read()
            .await
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("Unknown workspace_id: {}", id))
    }

    /// Resolve the workspace targeted by a tool call: explicit `workspace_id`
    /// from args wins, otherwise default.
    pub(super) async fn resolve_workspace(&self, args: &Value) -> Result<Arc<WorkspaceEntry>> {
        match args.get("workspace_id").and_then(|v| v.as_str()) {
            Some(id) => self.workspace_by_id(id).await,
            None => self.default_workspace().await,
        }
    }

    /// Snapshot of all registered workspaces (in insertion order).
    pub(super) async fn list_workspaces(&self) -> Vec<Arc<WorkspaceEntry>> {
        self.workspaces.read().await.list()
    }

    /// Add a new workspace. Returns the newly-created entry.
    pub(super) async fn add_workspace(&self, root: PathBuf) -> Arc<WorkspaceEntry> {
        self.workspaces.write().await.add(root)
    }

    /// Remove a workspace by id. Shuts down its client. Returns `true` if the
    /// id was known.
    pub(super) async fn remove_workspace(&self, id: &str) -> bool {
        let entry = self.workspaces.write().await.remove(id);
        match entry {
            Some(entry) => {
                entry.shutdown_client().await;
                true
            }
            None => false,
        }
    }

    /// Backward-compatible "set workspace": replaces the *default* workspace's
    /// root in place (preserving its id) and shuts down its client so the next
    /// call boots rust-analyzer in the new directory. If the registry is
    /// empty (every workspace was removed), creates a fresh default.
    pub(super) async fn set_workspace_root(&self, workspace_root: PathBuf) -> Result<()> {
        let default = self.workspaces.read().await.default();
        match default {
            Some(entry) => {
                entry.replace_root(workspace_root).await;
            }
            None => {
                self.workspaces.write().await.add(workspace_root);
            }
        }
        Ok(())
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

        // Cleanup: shutdown every workspace's client.
        info!("Shutting down");
        let entries = self.workspaces.read().await.list();
        for entry in entries {
            entry.shutdown_client().await;
        }

        Ok(())
    }

    async fn handle_resources_list(self: &Arc<Self>, request: MCPRequest) -> MCPResponse {
        // Snapshot every workspace's id + root, then run cargo metadata + the
        // file-tree advert for each one off-thread. Default workspace
        // additionally gets unprefixed aliases (`workspace://files`, etc.) for
        // backward compatibility with single-workspace clients.
        let entries = self.workspaces.read().await.list();
        let snapshots: Vec<(String, PathBuf, bool)> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id().to_string(), e.root_clone(), i == 0))
            .collect();

        let listed = tokio::task::spawn_blocking(move || {
            let mut all = Vec::new();
            for (id, root, is_default) in snapshots {
                let raw = super::resources::list_resources(&root);
                let Some(resources) = raw["resources"].as_array() else {
                    continue;
                };
                for resource in resources {
                    let original = resource["uri"].as_str().unwrap_or("").to_string();
                    let prefixed = prefix_workspace_uri(&original, &id);
                    let mut entry = resource.clone();
                    entry["uri"] = json!(prefixed);
                    if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                        entry["name"] = json!(format!("[{id}] {name}"));
                    }
                    all.push(entry);

                    if is_default {
                        // Backward-compat alias on the default workspace.
                        all.push(resource.clone());
                    }
                }
            }
            json!({ "resources": all })
        })
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

        // Resolve workspace from URI: `workspace://<id>/<rest>` targets a
        // specific workspace; an unprefixed `workspace://...` falls back to
        // the default. The match against known ids is what disambiguates from
        // the legacy paths (e.g. `workspace://crate/<name>/Cargo.toml`).
        let resolved = {
            let registry = self.workspaces.read().await;
            resolve_resource_uri(&uri, &registry)
        };
        let (workspace_root, read_uri) = match resolved {
            Some(pair) => pair,
            None => {
                return MCPResponse::Error {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    error: MCPError {
                        code: -32603,
                        message: "No workspace registered to serve this URI".to_string(),
                        data: None,
                    },
                };
            }
        };

        // Filesystem walk is sync; offload to a blocking thread so we don't
        // stall the request reader on a large workspace.
        let uri_changed = read_uri != uri;
        let read = tokio::task::spawn_blocking(move || {
            super::resources::read_resource(&workspace_root, &read_uri)
        })
        .await;

        match read {
            Ok(Ok(mut value)) => {
                // resources::read_resource stamps each content item with the
                // URI it was called with (the normalized form). Rewrite back
                // to the caller-supplied URI so the response round-trips
                // exactly what the client asked for.
                if uri_changed {
                    if let Some(arr) = value.get_mut("contents").and_then(|v| v.as_array_mut()) {
                        for item in arr {
                            if let Some(obj) = item.as_object_mut() {
                                obj.insert("uri".to_string(), json!(uri));
                            }
                        }
                    }
                }
                MCPResponse::Success {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: value,
                }
            }
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
        // instead of being abandoned. Workspaces only know about their own LSP
        // ids — broadcast across all of them; clients without matching
        // entries no-op cheaply. Do this before aborting the spawn so the
        // tracker still has the LSP ids registered.
        let entries = self.workspaces.read().await.list();
        for entry in entries {
            if let Some(client) = entry.maybe_client().await {
                client.cancel_mcp(&key).await;
            }
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

/// Add a workspace-id segment after `workspace://` so per-workspace resource
/// URIs are unambiguous: `workspace://files` → `workspace://ws-2/files`.
fn prefix_workspace_uri(uri: &str, id: &str) -> String {
    match uri.strip_prefix("workspace://") {
        Some(rest) => format!("workspace://{id}/{rest}"),
        None => uri.to_string(),
    }
}

/// Resolve a `workspace://...` URI to the workspace it targets and the
/// "normalized" form that `resources::read_resource` understands (without the
/// id segment). Returns `None` if the registry is empty.
///
/// - `workspace://<id>/<rest>` → look up `<id>` in the registry; if it exists, return `(root,
///   "workspace://<rest>")`.
/// - Any other `workspace://...` URI (including legacy unprefixed paths like
///   `workspace://crate/foo/Cargo.toml`) → falls through to the default workspace, URI passed
///   through unchanged.
fn resolve_resource_uri(uri: &str, registry: &WorkspaceRegistry) -> Option<(PathBuf, String)> {
    if let Some(rest) = uri.strip_prefix("workspace://") {
        if let Some((maybe_id, suffix)) = rest.split_once('/') {
            if let Some(ws) = registry.get(maybe_id) {
                return Some((ws.root_clone(), format!("workspace://{suffix}")));
            }
        }
    }
    let default = registry.default()?;
    Some((default.root_clone(), uri.to_string()))
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
