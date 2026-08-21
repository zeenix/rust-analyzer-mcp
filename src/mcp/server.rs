use anyhow::Result;
use log::{debug, error, info, warn};
use serde_json::json;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

use crate::{
    lsp::RustAnalyzerClient,
    protocol::mcp::{MCPError, MCPRequest, MCPResponse},
    settings::Settings,
    uri,
};

pub struct RustAnalyzerMCPServer {
    pub(super) client: Option<RustAnalyzerClient>,
    pub(super) workspace_root: PathBuf,
    /// What rust-analyzer is asked to run with, for every rust-analyzer this server starts.
    pub(super) settings: Settings,
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
            settings: Settings::default(),
        }
    }

    pub fn with_workspace(workspace_root: PathBuf) -> Self {
        Self {
            client: None,
            workspace_root: uri::absolute(&workspace_root),
            settings: Settings::default(),
        }
    }

    /// Runs rust-analyzer with `settings`, whatever workspace it is pointed at.
    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    pub(super) async fn ensure_client_started(&mut self) -> Result<()> {
        // rust-analyzer does die on occasion (it panics on some requests, see open_document());
        // keeping a dead client around would fail every tool call for the rest of this server's
        // life, so respawn it instead.
        if let Some(client) = &mut self.client {
            if client.is_gone() {
                match client.exit_status() {
                    Some(status) => warn!("rust-analyzer exited ({status}), restarting it"),
                    None => warn!("rust-analyzer closed its connection, restarting it"),
                }
                self.client = None;
            }
        }

        if self.client.is_none() {
            let mut client =
                RustAnalyzerClient::new(self.workspace_root.clone(), self.settings.to_json());
            client.start().await?;
            self.client = Some(client);
        }
        Ok(())
    }

    pub(super) async fn open_document_if_needed(&mut self, file_path: &str) -> Result<String> {
        let path = self.resolve_path(file_path);
        let uri = uri::path_to_uri(&path)?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read file {}: {}", path.display(), e))?;

        let Some(client) = &mut self.client else {
            return Err(anyhow::anyhow!("Client not initialized"));
        };

        client.open_document(&uri, &content).await?;
        Ok(uri)
    }

    /// Brings rust-analyzer up to date with every document it has been told about.
    ///
    /// One document at a time is enough for a question about one file, but a rename reaches
    /// across the workspace and is worked out from whatever rust-analyzer holds for each file it
    /// touches. Anything stale in there comes back as an edit to a line that has moved.
    pub(super) async fn refresh_open_documents(&mut self) -> Result<()> {
        let Some(client) = &mut self.client else {
            return Err(anyhow::anyhow!("Client not initialized"));
        };

        for uri in client.open_document_uris().await {
            let Some(path) = uri::uri_to_path(&uri) else {
                continue;
            };

            match tokio::fs::read_to_string(&path).await {
                Ok(content) => client.open_document(&uri, &content).await?,
                // Gone from disk, which a rename of a module's file does to it. Left open, it
                // would go on existing as far as rust-analyzer is concerned.
                Err(_) => client.close_document(&uri).await?,
            }
        }

        Ok(())
    }

    /// The file a tool call's `file_path` argument names.
    ///
    /// Clients spell that argument every way they have one to hand: relative to the workspace
    /// root, absolute, or as the `file:` URI our own results are full of.
    pub(super) fn resolve_path(&self, file_path: &str) -> PathBuf {
        let path = match uri::uri_to_path(file_path) {
            Some(path) => path,
            // Joining an absolute path onto the root yields that path, so this covers both.
            None => self.workspace_root.join(file_path),
        };

        uri::absolute(&path)
    }

    /// Runs the server until its stdin reaches EOF or a shutdown signal arrives.
    ///
    /// Installs process-wide signal handlers that remain in effect after this returns. Reads
    /// stdin through [`tokio::io::stdin`], whose parked blocking read cannot be cancelled: after
    /// a signal-triggered exit the caller must not wait for the runtime to shut down on its own.
    /// See this crate's `main.rs`, which uses [`tokio::runtime::Runtime::shutdown_background`].
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

            // A message without an `id` is a JSON-RPC notification, which must never be answered,
            // not even with an error: a client that receives a response it did not ask for treats
            // it as a protocol violation and closes the transport. `notifications/initialized` is
            // part of every MCP handshake, so this used to break every spec-compliant client.
            if request.id.is_none() {
                debug!("Ignoring notification: {}", request.method);
                continue;
            }

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
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {
                        "tools": {}
                    }
                }),
            },
            // A liveness check the client may send at any point, including before `initialize`.
            // Its result is empty; what matters is that it comes back at all.
            "ping" => MCPResponse::Success {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: json!({}),
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
/// SIGINT, SIGTERM and SIGHUP on Unix; Ctrl+C and console-close events on Windows. The streams
/// are persistent, so signals delivered while no `recv()` is pending stay latched instead of
/// falling through to the default disposition. Note that registering SIGHUP also overrides an
/// inherited SIG_IGN disposition (e.g. from nohup), so a hangup always shuts the server down.
struct ShutdownSignal {
    #[cfg(unix)]
    sigint: tokio::signal::unix::Signal,
    #[cfg(unix)]
    sigterm: tokio::signal::unix::Signal,
    #[cfg(unix)]
    sighup: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
}

impl ShutdownSignal {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};

            Ok(Self {
                sigint: signal(SignalKind::interrupt())?,
                sigterm: signal(SignalKind::terminate())?,
                sighup: signal(SignalKind::hangup())?,
            })
        }
        #[cfg(windows)]
        {
            use tokio::signal::windows;

            Ok(Self {
                ctrl_c: windows::ctrl_c()?,
                ctrl_close: windows::ctrl_close()?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        Ok(Self {})
    }

    /// Completes when the next shutdown signal arrives. Cancellation-safe.
    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.sigint.recv() => {}
                _ = self.sigterm.recv() => {}
                _ = self.sighup.recv() => {}
            }
        }
        #[cfg(windows)]
        {
            tokio::select! {
                _ = self.ctrl_c.recv() => {}
                _ = self.ctrl_close.recv() => {}
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            // No signal support; only a stdin EOF stops the server.
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_paths_are_resolved_however_they_are_spelled() {
        let server = RustAnalyzerMCPServer::with_workspace(workspace());
        let absolute = server.workspace_root.join("src/lib.rs");
        let uri = uri::path_to_uri(&absolute).unwrap();

        for spelling in ["src/lib.rs", &absolute.display().to_string(), &uri] {
            let resolved = server.resolve_path(spelling);

            assert_eq!(resolved, absolute, "{spelling}");
            // Equality alone would not catch the Windows extended-length form, which compares
            // equal to the path meant while being unusable.
            assert!(std::fs::read_to_string(&resolved).is_ok(), "{spelling}");
        }
    }

    #[test]
    fn a_path_that_does_not_exist_still_resolves() {
        // Nothing to canonicalize against, but the error belongs to whoever reads the file.
        let server = RustAnalyzerMCPServer::with_workspace(workspace());
        let missing = server.workspace_root.join("src/nowhere.rs");

        assert_eq!(server.resolve_path("src/nowhere.rs"), missing);
    }

    /// A real directory, so that `canonicalize()` has something to work with.
    fn workspace() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-project")
    }
}
