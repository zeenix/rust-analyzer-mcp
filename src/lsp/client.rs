use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    process::{Child, Command},
    sync::{oneshot, Mutex},
};
use tracing::info;

use crate::{
    config::{DOCUMENT_OPEN_DELAY_MILLIS, LSP_REQUEST_TIMEOUT_SECS},
    lsp::{
        connection::{PendingEntry, PendingRequests, ProgressMap, SharedStdin},
        error::LspError,
    },
    protocol::lsp::LSPRequest,
};

/// Shared state for an LSP client. All methods take `&self` so the client can be wrapped in
/// `Arc<RustAnalyzerClient>` and reused concurrently across spawned MCP request handlers.
pub struct RustAnalyzerClient {
    pub(super) process: Mutex<Option<Child>>,
    pub(super) request_id: AtomicU64,
    pub(super) workspace_root: PathBuf,
    pub(super) stdin: SharedStdin,
    pub(super) pending_requests: PendingRequests,
    pub(super) initialized: AtomicBool,
    pub(super) open_documents: Mutex<HashSet<String>>,
    pub(super) diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    pub(super) progress: ProgressMap,
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
            process: Mutex::new(None),
            request_id: AtomicU64::new(1),
            workspace_root,
            stdin: Arc::new(Mutex::new(None)),
            pending_requests: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            initialized: AtomicBool::new(false),
            open_documents: Mutex::new(HashSet::new()),
            diagnostics: Arc::new(Mutex::new(HashMap::new())),
            progress: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    pub fn workspace_root(&self) -> &std::path::Path {
        &self.workspace_root
    }

    pub async fn start(&self) -> Result<()> {
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

        *self.stdin.lock().await = Some(BufWriter::new(stdin));

        // Start connection handlers. The shared `stdin` lets the response loop
        // reply to server-initiated requests like `window/workDoneProgress/create`.
        super::connection::start_handlers(
            stdout,
            stderr,
            Arc::clone(&self.stdin),
            Arc::clone(&self.pending_requests),
            Arc::clone(&self.diagnostics),
            Arc::clone(&self.progress),
        );

        *self.process.lock().await = Some(child);

        // Initialize LSP.
        self.initialize().await?;
        self.initialized.store(true, Ordering::Release);

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
        &self,
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

        let mut stdin_lock = self.stdin.lock().await;
        let stdin = stdin_lock
            .as_mut()
            .ok_or_else(|| anyhow!("No stdin available"))?;

        stdin.write_all(message.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    pub(super) async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, LspError> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);

        let request = LSPRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params: params.clone(),
        };

        let content = serde_json::to_string(&request)
            .map_err(|e| LspError::Transport(format!("serialize: {}", e)))?;
        let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        info!("Sending LSP request: {} with params: {:?}", method, params);

        // Register the pending request *before* writing, so a fast response can't race past us.
        let (tx, rx) = oneshot::channel();
        self.pending_requests.lock().insert(
            id,
            PendingEntry {
                method: method.to_string(),
                sender: tx,
            },
        );

        // Send under the stdin lock so concurrent senders don't interleave headers/bodies.
        {
            let mut stdin_lock = self.stdin.lock().await;
            let Some(stdin) = stdin_lock.as_mut() else {
                self.pending_requests.lock().remove(&id);
                return Err(LspError::Transport("no stdin available".to_string()));
            };
            if let Err(e) = stdin.write_all(message.as_bytes()).await {
                self.pending_requests.lock().remove(&id);
                return Err(LspError::Transport(format!("write: {}", e)));
            }
            if let Err(e) = stdin.flush().await {
                self.pending_requests.lock().remove(&id);
                return Err(LspError::Transport(format!("flush: {}", e)));
            }
        }

        // Wait for response with timeout.
        match tokio::time::timeout(Duration::from_secs(LSP_REQUEST_TIMEOUT_SECS), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(LspError::Cancelled),
            Err(_) => {
                self.pending_requests.lock().remove(&id);
                Err(LspError::Timeout(method.to_string()))
            }
        }
    }

    async fn initialize(&self) -> Result<()> {
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
                },
                "window": {
                    "workDoneProgress": true
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

    /// Wait until rust-analyzer reports `$/progress` `end` for the given token, or
    /// the timeout elapses. If the token has already ended (or never been seen and
    /// then quickly ends), this returns immediately. Returns `Err(())` on timeout.
    pub async fn wait_for_progress_end(&self, token: &str, timeout: Duration) -> Result<(), ()> {
        let mut rx = {
            let mut map = self.progress.lock();
            let sender = map.entry(token.to_string()).or_insert_with(|| {
                let (tx, _) = tokio::sync::watch::channel(false);
                tx
            });
            sender.subscribe()
        };

        if *rx.borrow() {
            return Ok(());
        }

        tokio::time::timeout(timeout, async {
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
        .map_err(|_| ())
    }

    pub async fn is_open(&self, uri: &str) -> bool {
        self.open_documents.lock().await.contains(uri)
    }

    pub async fn open_document(&self, uri: &str, content: &str) -> Result<()> {
        // Hold the open_documents lock across the didOpen send so check-then-insert is atomic
        // per-URI. Concurrent callers for the same URI either bail out early ("already open")
        // or wait their turn here.
        let mut open_docs = self.open_documents.lock().await;
        if open_docs.contains(uri) {
            info!("Document already open: {}", uri);
            return Ok(());
        }

        self.diagnostics.lock().await.remove(uri);

        info!("Opening document: {}", uri);
        let params = json!({
            "textDocument": {
                "uri": uri,
                "languageId": "rust",
                "version": 1,
                "text": content
            }
        });

        self.send_notification("textDocument/didOpen", Some(params))
            .await?;

        open_docs.insert(uri.to_string());
        drop(open_docs);

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

    pub async fn shutdown(&self) -> Result<()> {
        if self.initialized.swap(false, Ordering::AcqRel) {
            let _ = self.send_request("shutdown", None).await;
            let _ = self.send_notification("exit", None).await;
        }

        if let Some(mut process) = self.process.lock().await.take() {
            // Kill the process and wait for it to actually exit.
            let _ = process.kill().await;
            let _ = process.wait().await;
        }

        // Drop stdin so the BufWriter releases the FD.
        *self.stdin.lock().await = None;

        // Clear open documents and diagnostics.
        self.open_documents.lock().await.clear();
        self.diagnostics.lock().await.clear();
        Ok(())
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
