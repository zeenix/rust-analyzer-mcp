use anyhow::{anyhow, Result};
use log::{info, warn};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    process::{Child, Command},
    sync::{oneshot, Mutex},
};

use crate::{
    config::{DOCUMENT_OPEN_DELAY_MILLIS, LSP_REQUEST_TIMEOUT_SECS},
    protocol::lsp::LSPRequest,
};

pub struct RustAnalyzerClient {
    pub(super) process: Option<Child>,
    pub(super) request_id: Arc<Mutex<u64>>,
    pub(super) workspace_root: PathBuf,
    pub(super) stdin: Option<BufWriter<tokio::process::ChildStdin>>,
    pub(super) pending_requests: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    pub(super) initialized: bool,
    pub(super) open_documents: Arc<Mutex<HashSet<String>>>,
    pub(super) diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
}

impl RustAnalyzerClient {
    pub fn new(workspace_root: PathBuf) -> Self {
        // Ensure the workspace root is absolute.
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
            process: None,
            request_id: Arc::new(Mutex::new(1)),
            workspace_root,
            stdin: None,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            initialized: false,
            open_documents: Arc::new(Mutex::new(HashSet::new())),
            diagnostics: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        info!(
            "Starting rust-analyzer process in workspace: {}",
            self.workspace_root.display()
        );

        // Clear any existing diagnostics from previous sessions.
        self.diagnostics.lock().await.clear();

        // Find rust-analyzer executable.
        let rust_analyzer_path = find_rust_analyzer()?;
        info!("Using rust-analyzer at: {}", rust_analyzer_path.display());

        let mut cmd = Command::new(rust_analyzer_path);
        cmd.current_dir(&self.workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Pass through isolation environment variables if they're set.
        if let Ok(cache_home) = std::env::var("XDG_CACHE_HOME") {
            cmd.env("XDG_CACHE_HOME", cache_home);
        }
        if let Ok(target_dir) = std::env::var("CARGO_TARGET_DIR") {
            cmd.env("CARGO_TARGET_DIR", target_dir);
        }
        if let Ok(tmpdir) = std::env::var("TMPDIR") {
            cmd.env("TMPDIR", tmpdir);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("Failed to start rust-analyzer: {}", e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to get stderr"))?;

        self.stdin = Some(BufWriter::new(stdin));

        // Start connection handlers.
        super::connection::start_handlers(
            stdout,
            stderr,
            Arc::clone(&self.pending_requests),
            Arc::clone(&self.diagnostics),
        );

        self.process = Some(child);

        // Initialize LSP.
        self.initialize().await?;
        self.initialized = true;

        // Send workspace/didChangeConfiguration to ensure settings are applied.
        let config_params = json!({
            "settings": {
                "rust-analyzer": {
                    "checkOnSave": {
                        "enable": true,
                        "command": "check",
                        "allTargets": true
                    }
                }
            }
        });
        let _ = self
            .send_notification("workspace/didChangeConfiguration", Some(config_params))
            .await;

        info!("rust-analyzer client started and initialized");
        Ok(())
    }

    pub(super) async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<()> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params.unwrap_or(json!({}))
        });

        let content = serde_json::to_string(&notification)?;
        let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        info!("Sending LSP notification: {}", method);

        let Some(stdin) = &mut self.stdin else {
            return Err(anyhow!("No stdin available"));
        };

        stdin.write_all(message.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    pub(super) async fn send_request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value> {
        let mut request_id_lock = self.request_id.lock().await;
        let id = *request_id_lock;
        *request_id_lock += 1;
        drop(request_id_lock);

        let request = LSPRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params: params.clone(),
        };

        let content = serde_json::to_string(&request)?;
        let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        info!("Sending LSP request: {} with params: {:?}", method, params);

        let Some(stdin) = &mut self.stdin else {
            return Err(anyhow!("No stdin available"));
        };

        stdin.write_all(message.as_bytes()).await?;
        stdin.flush().await?;

        // Set up response channel.
        let (tx, rx) = oneshot::channel();
        self.pending_requests.lock().await.insert(id, tx);

        // Wait for response with timeout.
        tokio::time::timeout(Duration::from_secs(LSP_REQUEST_TIMEOUT_SECS), rx)
            .await
            .map_err(|_| anyhow!("Request timeout"))?
            .map_err(|_| anyhow!("Request cancelled"))
    }

    async fn initialize(&mut self) -> Result<()> {
        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": format!("file://{}", self.workspace_root.display()),
            "initializationOptions": {
                "cargo": {
                    "buildScripts": {
                        "enable": true
                    }
                },
                "checkOnSave": {
                    "enable": true,
                    "command": "check",
                    "allTargets": true
                },
                "diagnostics": {
                    "enable": true,
                    "experimental": {
                        "enable": true
                    }
                },
                "procMacro": {
                    "enable": true
                }
            },
            "capabilities": {
                "textDocument": {
                    "hover": {
                        "contentFormat": ["markdown", "plaintext"]
                    },
                    "completion": {
                        "completionItem": {
                            "snippetSupport": true
                        }
                    },
                    "definition": {
                        "linkSupport": true
                    },
                    "references": {},
                    "documentSymbol": {},
                    "codeAction": {
                        "codeActionLiteralSupport": {
                            "codeActionKind": {
                                "valueSet": [
                                    "quickfix",
                                    "refactor",
                                    "refactor.extract",
                                    "refactor.inline",
                                    "refactor.rewrite",
                                    "source",
                                    "source.organizeImports"
                                ]
                            }
                        },
                        "resolveSupport": {
                            "properties": ["edit"]
                        }
                    },
                    "publishDiagnostics": {
                        "relatedInformation": true,
                        "tagSupport": {
                            "valueSet": [1, 2]
                        }
                    },
                    "formatting": {}
                },
                "workspace": {
                    "didChangeConfiguration": {
                        "dynamicRegistration": false
                    }
                }
            }
        });

        self.send_request("initialize", Some(init_params)).await?;
        self.send_notification("initialized", Some(json!({})))
            .await?;

        // Request workspace reload to trigger cargo check.
        self.send_request("rust-analyzer/reloadWorkspace", None)
            .await
            .ok();

        Ok(())
    }

    pub async fn open_document(&mut self, uri: &str, content: &str) -> Result<()> {
        // Check if document is already open.
        {
            let open_docs = self.open_documents.lock().await;
            if open_docs.contains(uri) {
                info!("Document already open: {}", uri);
                return Ok(());
            }
        }

        // Clear any existing diagnostics for this URI to ensure fresh data.
        {
            let mut diag_lock = self.diagnostics.lock().await;
            diag_lock.remove(uri);
        }

        info!("Opening document: {}", uri);
        let params = json!({
            "textDocument": {
                "uri": uri,
                "languageId": "rust",
                "version": 1,
                "text": content
            }
        });

        self.send_notification("textDocument/didOpen", Some(params.clone()))
            .await?;

        // Mark document as open.
        {
            let mut open_docs = self.open_documents.lock().await;
            open_docs.insert(uri.to_string());
        }

        // Send didSave to trigger cargo check.
        let save_params = json!({
            "textDocument": {
                "uri": uri
            }
        });
        self.send_notification("textDocument/didSave", Some(save_params))
            .await?;

        // Give rust-analyzer time to process the document and run cargo check.
        tokio::time::sleep(Duration::from_millis(DOCUMENT_OPEN_DELAY_MILLIS)).await;

        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        if self.initialized {
            let _ = self.send_request("shutdown", None).await;
            let _ = self.send_notification("exit", None).await;
        }

        if let Some(mut process) = self.process.take() {
            // Kill the process and wait for it to actually exit.
            let _ = process.kill().await;
            let _ = process.wait().await;
        }

        // Clear open documents and diagnostics.
        self.open_documents.lock().await.clear();
        self.diagnostics.lock().await.clear();
        self.initialized = false;
        Ok(())
    }
}

impl Drop for RustAnalyzerClient {
    fn drop(&mut self) {
        if let Some(ref mut process) = self.process {
            info!("Killing child rust-analyzer process on drop");
            if let Err(e) = process.start_kill() {
                warn!("Failed to kill rust-analyzer process on drop: {}", e);
            }
            // Best-effort reap to avoid zombies. If the process hasn't exited yet,
            // the brief zombie will be reaped by init/launchd when we exit shortly after.
            let _ = process.try_wait();
        }
    }
}

fn find_rust_analyzer() -> Result<PathBuf> {
    which::which("rust-analyzer").or_else(|_| {
        // Try common installation locations if not in PATH.
        let home = std::env::var("HOME").unwrap_or_else(|_| String::from("~"));
        let cargo_bin = PathBuf::from(home).join(".cargo/bin/rust-analyzer");
        if cargo_bin.exists() {
            Ok(cargo_bin)
        } else {
            which::which("rust-analyzer")
        }
    })
    .map_err(|e| {
        anyhow!(
            "Failed to find rust-analyzer in PATH or ~/.cargo/bin: {}. Please ensure rust-analyzer is installed.",
            e
        )
    })
}
