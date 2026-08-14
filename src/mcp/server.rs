use anyhow::Result;
use log::{debug, error, info};
use serde_json::json;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

use crate::{
    lsp::RustAnalyzerClient,
    protocol::mcp::{MCPError, MCPRequest, MCPResponse},
};

pub struct RustAnalyzerMCPServer {
    pub(super) client: Option<RustAnalyzerClient>,
    pub(super) workspace_root: PathBuf,
}

impl Default for RustAnalyzerMCPServer {
    fn default() -> Self {
        Self::new()
    }
}

impl RustAnalyzerMCPServer {
    pub fn new() -> Self {
        Self {
            client: None,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
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

    pub(super) async fn open_document_if_needed(&mut self, file_path: &str) -> Result<String> {
        let absolute_path = self.workspace_root.join(file_path);
        // Ensure we have an absolute path for the URI.
        let absolute_path = absolute_path
            .canonicalize()
            .unwrap_or_else(|_| absolute_path.clone());
        let uri = format!("file://{}", absolute_path.display());
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

        // Created once, up front: the streams buffer signals delivered while a request is being
        // handled, and installing a handler permanently replaces the default disposition, so
        // every signal must be consumed here to have an effect.
        let mut shutdown = ShutdownSignal::new()?;
        // How many shutdown signals were consumed; the second one escalates the cleanup below.
        let mut signals_seen = 0u32;
        // The first fatal I/O error, reported only after the cleanup ran.
        let mut result = Ok(());

        loop {
            let mut line = String::new();
            // read_line() is not cancellation-safe, but the partially read line is only lost when
            // we shut down and discard it anyway.
            let bytes_read = tokio::select! {
                // Biased with the signal arm first: a signal that latched while a request was
                // being handled must win over lines already buffered on stdin, so that no new
                // request is accepted after shutdown was requested.
                biased;
                _ = shutdown.recv() => {
                    info!("Received shutdown signal");
                    signals_seen += 1;
                    break;
                }
                read = reader.read_line(&mut line) => match read {
                    Ok(n) => n,
                    Err(e) => {
                        error!("Error reading from stdin: {}", e);
                        result = Err(e.into());
                        break;
                    }
                },
            };

            if bytes_read == 0 {
                break; // EOF
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let Ok(request) = serde_json::from_str::<MCPRequest>(line) else {
                debug!("Failed to parse request: {}", line);
                continue;
            };

            debug!("Received request: {}", request.method);
            // A shutdown signal must not wait for the request to finish: a tool call that
            // cold-starts rust-analyzer can run for minutes.
            let response = tokio::select! {
                biased;
                _ = shutdown.recv() => {
                    info!("Received shutdown signal");
                    signals_seen += 1;
                    break;
                }
                response = self.handle_request(request) => response,
            };
            // Break on errors instead of returning so rust-analyzer still gets cleaned up.
            let response_json = match serde_json::to_string(&response) {
                Ok(json) => json,
                Err(e) => {
                    error!("Failed to serialize response: {}", e);
                    result = Err(e.into());
                    break;
                }
            };
            // Also raced against the signals: if the host stops reading stdout, a response that
            // fills the pipe would otherwise block here forever with the signals unpolled.
            let written = async {
                writer.write_all(response_json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await
            };
            let written = tokio::select! {
                biased;
                _ = shutdown.recv() => {
                    info!("Received shutdown signal");
                    signals_seen += 1;
                    break;
                }
                written = written => written,
            };
            if let Err(e) = written {
                error!("Error writing to stdout: {}", e);
                result = Err(e.into());
                break;
            }
        }

        // Cleanup. client.shutdown() bounds its own graceful handshake and always ends up
        // killing the process, so this cannot stall. A second signal — counting the one that may
        // have triggered the exit — skips the handshake and kills rust-analyzer immediately.
        info!("Shutting down");
        if let Some(client) = &mut self.client {
            let graceful = {
                let shutting_down = client.shutdown();
                tokio::pin!(shutting_down);
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.recv() => {
                            signals_seen += 1;
                            if signals_seen >= 2 {
                                info!("Received another shutdown signal, killing rust-analyzer");
                                break false;
                            }
                        }
                        res = &mut shutting_down => {
                            let _ = res;
                            break true;
                        }
                    }
                }
            };
            if !graceful {
                client.force_kill().await;
            }
        }

        result
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

/// Merged stream of the signals that request server shutdown.
///
/// SIGINT and SIGTERM on Unix, Ctrl+C events elsewhere.
struct ShutdownSignal {
    #[cfg(unix)]
    sigint: tokio::signal::unix::Signal,
    #[cfg(unix)]
    sigterm: tokio::signal::unix::Signal,
}

impl ShutdownSignal {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};

            Ok(Self {
                sigint: signal(SignalKind::interrupt())?,
                sigterm: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }

    /// Completes when the next shutdown signal arrives. Cancellation-safe.
    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.sigint.recv() => {}
                _ = self.sigterm.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}
