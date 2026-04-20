use anyhow::{anyhow, Result};
use log::{debug, error, info};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::Mutex,
};
use url::Url;

use crate::{
    config::WORKSPACE_ROOT_ENV,
    lsp::RustAnalyzerClient,
    protocol::mcp::{MCPError, MCPRequest, MCPResponse},
};

pub struct RustAnalyzerMCPServer {
    pub(super) client: Option<RustAnalyzerClient>,
    pub(super) workspace_root: PathBuf,
}

#[derive(Clone, Copy)]
enum TransportMode {
    JsonLine,
    ContentLength,
}

impl Default for RustAnalyzerMCPServer {
    fn default() -> Self {
        Self::new()
    }
}

impl RustAnalyzerMCPServer {
    fn resolve_workspace_root() -> PathBuf {
        if let Ok(workspace) = std::env::var(WORKSPACE_ROOT_ENV) {
            let workspace_path = PathBuf::from(workspace);
            return workspace_path
                .canonicalize()
                .unwrap_or_else(|_| workspace_path);
        }

        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    fn to_file_uri(path: &Path) -> Result<String> {
        #[cfg(windows)]
        let normalized = {
            let path_str = path.to_string_lossy();
            if let Some(stripped) = path_str.strip_prefix(r"\\?\") {
                PathBuf::from(stripped)
            } else {
                path.to_path_buf()
            }
        };
        #[cfg(not(windows))]
        let normalized = path.to_path_buf();

        Url::from_file_path(&normalized)
            .map_err(|_| {
                anyhow!(
                    "Failed to convert path to file URI: {}",
                    normalized.display()
                )
            })
            .map(|u| u.to_string())
    }

    pub fn new() -> Self {
        Self {
            client: None,
            workspace_root: Self::resolve_workspace_root(),
        }
    }

    pub fn with_workspace(workspace_root: PathBuf) -> Self {
        // Ensure the workspace root is absolute.
        let workspace_root = workspace_root.canonicalize().unwrap_or_else(|_| {
            // If canonicalize fails, try to make it absolute.
            if workspace_root.is_absolute() {
                workspace_root.clone()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(&workspace_root)
            }
        });

        Self {
            client: None,
            workspace_root,
        }
    }

    pub(super) async fn ensure_client_started(&mut self) -> Result<()> {
        if self.client.is_none() {
            let mut client = RustAnalyzerClient::new(self.workspace_root.clone());
            client.start().await?;
            self.client = Some(client);
        }
        Ok(())
    }

    /// 在首次启动 rust-analyzer 之前，根据工具参数中的 file_path 自动纠正 workspace。
    /// 这样可以避免 MCP 进程 cwd 与真实 Rust 工程不一致时导致的初始化异常或超时。
    pub(super) fn adjust_workspace_from_file_arg(&mut self, args: &Value) {
        if self.client.is_some() {
            return;
        }

        let Some(file_path) = args.get("file_path").and_then(|v| v.as_str()) else {
            return;
        };
        let requested_path = self.resolve_requested_path(file_path);
        let Some(workspace_root) = find_workspace_root_for_file(&requested_path) else {
            return;
        };

        if workspace_root != self.workspace_root {
            info!(
                "Adjusting workspace root from '{}' to '{}' based on file_path '{}'",
                self.workspace_root.display(),
                workspace_root.display(),
                requested_path.display()
            );
            self.workspace_root = workspace_root;
        }
    }

    pub(super) fn resolve_requested_path(&self, file_path: &str) -> PathBuf {
        let candidate = PathBuf::from(file_path);
        let joined = if candidate.is_absolute() {
            candidate
        } else {
            self.workspace_root.join(file_path)
        };

        joined.canonicalize().unwrap_or(joined)
    }

    pub(super) async fn open_document_if_needed(&mut self, file_path: &str) -> Result<String> {
        let absolute_path = self.resolve_requested_path(file_path);
        let uri = Self::to_file_uri(&absolute_path)?;
        let content = tokio::fs::read_to_string(&absolute_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read file {}: {}", file_path, e))?;

        let Some(client) = &mut self.client else {
            return Err(anyhow::anyhow!("Client not initialized"));
        };

        client.open_document(&uri, &content).await?;
        Ok(uri)
    }

    pub async fn run(&mut self) -> Result<()> {
        info!("Starting rust-analyzer MCP server");

        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut writer = BufWriter::new(stdout);
        let mut transport_mode: Option<TransportMode> = None;

        // Handle shutdown signals.
        let running = Arc::new(Mutex::new(true));
        let running_clone = Arc::clone(&running);

        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Received shutdown signal");
            *running_clone.lock().await = false;
        });

        loop {
            // Check if we should stop.
            if !*running.lock().await {
                break;
            }

            let request = match Self::read_request(&mut reader, &mut transport_mode).await {
                Ok(Some(req)) => req,
                Ok(None) => break, // EOF
                Err(e) => {
                    error!("Error reading MCP request: {}", e);
                    break;
                }
            };

            debug!("Received request: {}", request.method);

            // Notifications do not have `id` and must not receive a JSON-RPC response.
            if request.id.is_none() {
                debug!("Ignoring notification without id: {}", request.method);
                continue;
            }

            let response = self.handle_request(request).await;
            let response_json = serde_json::to_string(&response)?;
            Self::write_response(&mut writer, transport_mode, &response_json).await?;
        }

        // Cleanup.
        info!("Shutting down");
        if let Some(client) = &mut self.client {
            let _ = client.shutdown().await;
        }

        Ok(())
    }

    async fn read_request(
        reader: &mut BufReader<tokio::io::Stdin>,
        transport_mode: &mut Option<TransportMode>,
    ) -> Result<Option<MCPRequest>> {
        loop {
            let mut line = String::new();
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                return Ok(None);
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if trimmed.to_ascii_lowercase().starts_with("content-length:") {
                let len_text = trimmed
                    .split_once(':')
                    .map(|(_, v)| v.trim())
                    .ok_or_else(|| anyhow!("Invalid Content-Length header: {}", trimmed))?;
                let content_length = len_text
                    .parse::<usize>()
                    .map_err(|e| anyhow!("Invalid Content-Length value '{}': {}", len_text, e))?;

                // Read remaining headers until blank line.
                loop {
                    let mut header = String::new();
                    let n = reader.read_line(&mut header).await?;
                    if n == 0 {
                        return Ok(None);
                    }
                    if header.trim().is_empty() {
                        break;
                    }
                }

                let mut body = vec![0_u8; content_length];
                reader.read_exact(&mut body).await?;
                let body_text = String::from_utf8(body)
                    .map_err(|e| anyhow!("Invalid UTF-8 in MCP frame body: {}", e))?;

                match serde_json::from_str::<MCPRequest>(&body_text) {
                    Ok(request) => {
                        *transport_mode = Some(TransportMode::ContentLength);
                        return Ok(Some(request));
                    }
                    Err(e) => {
                        debug!(
                            "Failed to parse framed MCP request: {} | body={}",
                            e, body_text
                        );
                        continue;
                    }
                }
            }

            match serde_json::from_str::<MCPRequest>(trimmed) {
                Ok(request) => {
                    *transport_mode = Some(TransportMode::JsonLine);
                    return Ok(Some(request));
                }
                Err(e) => {
                    debug!("Failed to parse line MCP request: {} | line={}", e, trimmed);
                    continue;
                }
            }
        }
    }

    async fn write_response(
        writer: &mut BufWriter<tokio::io::Stdout>,
        transport_mode: Option<TransportMode>,
        response_json: &str,
    ) -> Result<()> {
        match transport_mode.unwrap_or(TransportMode::JsonLine) {
            TransportMode::JsonLine => {
                writer.write_all(response_json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
            TransportMode::ContentLength => {
                let bytes = response_json.as_bytes();
                let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
                writer.write_all(header.as_bytes()).await?;
                writer.write_all(bytes).await?;
            }
        }
        writer.flush().await?;
        Ok(())
    }

    async fn handle_request(&mut self, request: MCPRequest) -> MCPResponse {
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
                result: json!({
                    "tools": super::tools::get_tools()
                }),
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

fn find_workspace_root_for_file(file_path: &Path) -> Option<PathBuf> {
    let mut cursor = if file_path.is_file() {
        file_path.parent().map(Path::to_path_buf)?
    } else {
        file_path.to_path_buf()
    };

    loop {
        if cursor.join("Cargo.toml").is_file() {
            return Some(cursor);
        }
        let Some(parent) = cursor.parent() else {
            return None;
        };
        cursor = parent.to_path_buf();
    }
}
