use anyhow::{anyhow, Result};
use log::{info, warn};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    process::{Child, Command},
    sync::{oneshot, Mutex, Notify},
};

use crate::{
    config::{
        DIAGNOSTICS_WAIT_MILLIS, DOCUMENT_OPEN_DELAY_MILLIS, LSP_REQUEST_TIMEOUT_SECS,
        MAX_OPEN_DOCUMENTS, MAX_RESTART_ATTEMPTS, RESTART_DELAY_MILLIS,
    },
    protocol::lsp::LSPRequest,
};

/// Tracks pending requests for cancellation support.
pub(super) struct PendingRequest {
    pub sender: oneshot::Sender<Value>,
    pub method: String,
}

pub struct RustAnalyzerClient {
    pub(super) process: Option<Child>,
    pub(super) request_id: Arc<Mutex<u64>>,
    pub(super) workspace_root: PathBuf,
    pub(super) stdin: Option<BufWriter<tokio::process::ChildStdin>>,
    pub(super) pending_requests: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    pub(super) initialized: bool,
    pub(super) open_documents: Arc<Mutex<VecDeque<String>>>, // LRU order
    pub(super) open_documents_set: Arc<Mutex<HashSet<String>>>,
    pub(super) diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    pub(super) diagnostics_notify: Arc<Notify>,
    pub(super) restart_count: u32,
    pub(super) is_crashed: Arc<Mutex<bool>>,
}

impl RustAnalyzerClient {
    pub fn new(workspace_root: PathBuf) -> Self {
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
            open_documents: Arc::new(Mutex::new(VecDeque::new())),
            open_documents_set: Arc::new(Mutex::new(HashSet::new())),
            diagnostics: Arc::new(Mutex::new(HashMap::new())),
            diagnostics_notify: Arc::new(Notify::new()),
            restart_count: 0,
            is_crashed: Arc::new(Mutex::new(false)),
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        info!(
            "Starting rust-analyzer process in workspace: {}",
            self.workspace_root.display()
        );

        self.diagnostics.lock().await.clear();
        *self.is_crashed.lock().await = false;

        let rust_analyzer_path = find_rust_analyzer()?;
        info!("Using rust-analyzer at: {}", rust_analyzer_path.display());

        let mut cmd = Command::new(rust_analyzer_path);
        cmd.current_dir(&self.workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

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

        // Start connection handlers with crash detection.
        let is_crashed = Arc::clone(&self.is_crashed);
        super::connection::start_handlers_with_crash_detection(
            stdout,
            stderr,
            Arc::clone(&self.pending_requests),
            Arc::clone(&self.diagnostics),
            Arc::clone(&self.diagnostics_notify),
            is_crashed,
        );

        self.process = Some(child);

        self.initialize().await?;
        self.initialized = true;

        let config_params = json!({
            "settings": {
                "rust-analyzer": {
                    "cargo": {
                        "features": "all",
                        "allTargets": true
                    },
                    "checkOnSave": true,
                    "check": {
                        "command": "check",
                        "allTargets": true,
                        "allFeatures": true
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

    /// Attempt to restart rust-analyzer after a crash.
    pub async fn try_restart(&mut self) -> Result<()> {
        if self.restart_count >= MAX_RESTART_ATTEMPTS {
            return Err(anyhow!(
                "Max restart attempts ({}) exceeded",
                MAX_RESTART_ATTEMPTS
            ));
        }

        warn!(
            "Attempting rust-analyzer restart (attempt {}/{})",
            self.restart_count + 1,
            MAX_RESTART_ATTEMPTS
        );

        // Clean up old process.
        if let Some(mut process) = self.process.take() {
            let _ = process.kill().await;
            let _ = process.wait().await;
        }
        self.stdin = None;
        self.initialized = false;

        // Cancel all pending requests.
        let mut pending = self.pending_requests.lock().await;
        for (id, req) in pending.drain() {
            warn!("Cancelling pending request {} ({})", id, req.method);
            let _ = req.sender.send(json!(null));
        }
        drop(pending);

        // Wait before restart.
        tokio::time::sleep(Duration::from_millis(RESTART_DELAY_MILLIS)).await;

        self.restart_count += 1;
        self.start().await?;

        // Reopen previously open documents.
        let docs_to_reopen: Vec<String> = self.open_documents.lock().await.iter().cloned().collect();
        for uri in docs_to_reopen {
            if let Some(path) = uri.strip_prefix("file://") {
                if let Ok(content) = tokio::fs::read_to_string(path).await {
                    let _ = self.reopen_document(&uri, &content).await;
                }
            }
        }

        Ok(())
    }

    /// Check if rust-analyzer has crashed and attempt restart.
    pub async fn ensure_healthy(&mut self) -> Result<()> {
        if *self.is_crashed.lock().await {
            self.try_restart().await?;
        }
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
        // Check health before sending request (skip during init to avoid recursion).
        if self.initialized {
            self.ensure_healthy().await?;
        }
        self.send_request_internal(method, params).await
    }

    /// Internal request sender without health check (used during initialization).
    async fn send_request_internal(
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

        let Some(stdin) = &mut self.stdin else {
            return Err(anyhow!("No stdin available"));
        };

        stdin.write_all(message.as_bytes()).await?;
        stdin.flush().await?;

        let (tx, rx) = oneshot::channel();
        self.pending_requests.lock().await.insert(
            id,
            PendingRequest {
                sender: tx,
                method: method.to_string(),
            },
        );

        // Wait for response with timeout, support cancellation.
        let result = tokio::time::timeout(Duration::from_secs(LSP_REQUEST_TIMEOUT_SECS), rx).await;

        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(anyhow!("Request cancelled")),
            Err(_) => {
                // Timeout - send cancellation to rust-analyzer.
                self.cancel_request(id).await;
                self.pending_requests.lock().await.remove(&id);
                Err(anyhow!("Request timeout"))
            }
        }
    }

    /// Send cancellation notification for a request.
    async fn cancel_request(&mut self, id: u64) {
        let params = json!({ "id": id });
        let _ = self.send_notification("$/cancelRequest", Some(params)).await;
    }

    async fn initialize(&mut self) -> Result<()> {
        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": format!("file://{}", self.workspace_root.display()),
            "initializationOptions": {
                "cargo": {
                    "allTargets": true,
                    "autoreload": true,
                    "buildScripts": {
                        "enable": true,
                        "invocationStrategy": "per_workspace",
                        "rebuildOnSave": true,
                        "useRustcWrapper": true
                    },
                    "features": "all",
                    "noDefaultFeatures": false,
                    "sysroot": "discover"
                },
                "checkOnSave": true,
                "check": {
                    "command": "check",
                    "allTargets": true,
                    "allFeatures": true,
                    "workspace": true
                },
                "diagnostics": {
                    "enable": true,
                    "experimental": {
                        "enable": false
                    }
                },
                "procMacro": {
                    "enable": true,
                    "attributes": {
                        "enable": true
                    }
                },
                "cachePriming": {
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
                    "implementation": {
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
                    "formatting": {},
                    "callHierarchy": {
                        "dynamicRegistration": false
                    }
                },
                "workspace": {
                    "didChangeConfiguration": {
                        "dynamicRegistration": false
                    },
                    "symbol": {
                        "dynamicRegistration": false,
                        "symbolKind": {
                            "valueSet": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26]
                        }
                    }
                }
            }
        });

        // Use internal method to avoid health check recursion during init.
        self.send_request_internal("initialize", Some(init_params)).await?;
        self.send_notification("initialized", Some(json!({})))
            .await?;

        self.send_request_internal("rust-analyzer/reloadWorkspace", None)
            .await
            .ok();

        Ok(())
    }

    pub async fn open_document(&mut self, uri: &str, content: &str) -> Result<()> {
        // Check if already open.
        {
            let open_set = self.open_documents_set.lock().await;
            if open_set.contains(uri) {
                // Move to end of LRU queue.
                let mut queue = self.open_documents.lock().await;
                if let Some(pos) = queue.iter().position(|u| u == uri) {
                    queue.remove(pos);
                    queue.push_back(uri.to_string());
                }
                return Ok(());
            }
        }

        // Evict oldest document if at capacity.
        self.evict_oldest_if_needed().await?;

        // Clear any existing diagnostics.
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

        self.send_notification("textDocument/didOpen", Some(params))
            .await?;

        // Track as open.
        {
            let mut queue = self.open_documents.lock().await;
            queue.push_back(uri.to_string());
        }
        {
            let mut open_set = self.open_documents_set.lock().await;
            open_set.insert(uri.to_string());
        }

        // Trigger cargo check.
        let save_params = json!({
            "textDocument": { "uri": uri }
        });
        self.send_notification("textDocument/didSave", Some(save_params))
            .await?;

        tokio::time::sleep(Duration::from_millis(DOCUMENT_OPEN_DELAY_MILLIS)).await;

        Ok(())
    }

    /// Reopen a document without adding to tracking (used during restart).
    async fn reopen_document(&mut self, uri: &str, content: &str) -> Result<()> {
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

        Ok(())
    }

    /// Close a document and notify rust-analyzer.
    pub async fn close_document(&mut self, uri: &str) -> Result<()> {
        // Check if open.
        {
            let open_set = self.open_documents_set.lock().await;
            if !open_set.contains(uri) {
                return Ok(());
            }
        }

        info!("Closing document: {}", uri);
        let params = json!({
            "textDocument": { "uri": uri }
        });

        self.send_notification("textDocument/didClose", Some(params))
            .await?;

        // Remove from tracking.
        {
            let mut queue = self.open_documents.lock().await;
            if let Some(pos) = queue.iter().position(|u| u == uri) {
                queue.remove(pos);
            }
        }
        {
            let mut open_set = self.open_documents_set.lock().await;
            open_set.remove(uri);
        }

        // Clear diagnostics.
        {
            let mut diag_lock = self.diagnostics.lock().await;
            diag_lock.remove(uri);
        }

        Ok(())
    }

    /// Evict the oldest document if at capacity.
    async fn evict_oldest_if_needed(&mut self) -> Result<()> {
        let should_evict = {
            let queue = self.open_documents.lock().await;
            queue.len() >= MAX_OPEN_DOCUMENTS
        };

        if should_evict {
            let oldest_uri = {
                let mut queue = self.open_documents.lock().await;
                queue.pop_front()
            };

            if let Some(uri) = oldest_uri {
                info!("Evicting oldest document: {}", uri);
                let params = json!({
                    "textDocument": { "uri": uri }
                });
                let _ = self
                    .send_notification("textDocument/didClose", Some(params))
                    .await;

                let mut open_set = self.open_documents_set.lock().await;
                open_set.remove(&uri);

                let mut diag_lock = self.diagnostics.lock().await;
                diag_lock.remove(&uri);
            }
        }

        Ok(())
    }

    /// Wait for diagnostics notification with timeout.
    pub async fn wait_for_diagnostics(&self, uri: &str) -> Option<Vec<Value>> {
        let timeout = Duration::from_millis(DIAGNOSTICS_WAIT_MILLIS);
        let start = std::time::Instant::now();

        loop {
            // Check if we have diagnostics.
            {
                let diag_lock = self.diagnostics.lock().await;
                if let Some(diags) = diag_lock.get(uri) {
                    return Some(diags.clone());
                }
            }

            // Check timeout.
            if start.elapsed() >= timeout {
                return None;
            }

            // Wait for notification or timeout.
            let remaining = timeout.saturating_sub(start.elapsed());
            let _ = tokio::time::timeout(remaining, self.diagnostics_notify.notified()).await;
        }
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        if self.initialized {
            // Close all open documents.
            let docs_to_close: Vec<String> =
                self.open_documents.lock().await.iter().cloned().collect();
            for uri in docs_to_close {
                let params = json!({
                    "textDocument": { "uri": uri }
                });
                let _ = self
                    .send_notification("textDocument/didClose", Some(params))
                    .await;
            }

            let _ = self.send_request("shutdown", None).await;
            let _ = self.send_notification("exit", None).await;
        }

        if let Some(mut process) = self.process.take() {
            let _ = process.kill().await;
            let _ = process.wait().await;
        }

        self.open_documents.lock().await.clear();
        self.open_documents_set.lock().await.clear();
        self.diagnostics.lock().await.clear();
        self.initialized = false;
        Ok(())
    }
}

fn find_rust_analyzer() -> Result<PathBuf> {
    which::which("rust-analyzer").or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| String::from("~"));
        let cargo_bin = PathBuf::from(home).join(".cargo/bin/rust-analyzer");
        if cargo_bin.exists() {
            Ok(cargo_bin)
        } else {
            Err(anyhow!(
                "rust-analyzer not found in PATH or ~/.cargo/bin"
            ))
        }
    })
    .map_err(|e| {
        anyhow!(
            "Failed to find rust-analyzer: {}. Please ensure rust-analyzer is installed.",
            e
        )
    })
}
