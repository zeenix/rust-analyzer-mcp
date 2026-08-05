use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    process::{Child, Command},
    sync::{oneshot, Mutex, Notify},
};
use tracing::{debug, info, warn};

use crate::{
    config::{INDEXING_WAIT_TIMEOUT_SECS, LSP_REQUEST_TIMEOUT_SECS},
    lsp::{
        connection::{PendingEntry, PendingRequests, PendingTracker, ProgressMap, SharedStdin},
        error::LspError,
    },
    protocol::lsp::LSPRequest,
    util::canonicalize_path,
};

tokio::task_local! {
    /// MCP request id (canonical key form) of the request handler that owns
    /// the current task. `send_request` reads this so each LSP call can be
    /// indexed back to the originating MCP call for cancellation.
    pub(crate) static CURRENT_MCP_REQUEST_ID: String;
}

/// Tracks state of a document opened in rust-analyzer so we can avoid
/// redundant disk reads and emit `didChange` only when content actually
/// differs.
#[derive(Debug, Clone)]
pub(super) struct OpenDocState {
    pub(super) version: i32,
    /// File mtime at the time we last (re-)synced. Lets us skip even hashing
    /// when the file hasn't been touched.
    pub(super) mtime: Option<SystemTime>,
    pub(super) content_hash: u64,
}

/// Shared state for an LSP client. All methods take `&self` so the client can be wrapped in
/// `Arc<RustAnalyzerClient>` and reused concurrently across spawned MCP request handlers.
pub struct RustAnalyzerClient {
    pub(super) process: Arc<Mutex<Option<Child>>>,
    pub(super) request_id: AtomicU64,
    pub(super) workspace_root: PathBuf,
    pub(super) stdin: SharedStdin,
    pub(super) pending_requests: PendingRequests,
    pub(super) initialized: AtomicBool,
    pub(super) open_documents: Mutex<HashMap<String, OpenDocState>>,
    pub(super) diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    /// Pulsed by the connection task whenever rust-analyzer publishes new
    /// diagnostics. Lets `wait_for_diagnostics_change` block until something
    /// arrives instead of busy-polling the diagnostics map.
    pub(super) diagnostics_changed: Arc<Notify>,
    pub(super) progress: ProgressMap,
    /// Set by the monitor task when rust-analyzer's process has exited. The MCP
    /// server polls this to decide whether to restart the client.
    pub(super) process_died: Arc<AtomicBool>,
}

impl RustAnalyzerClient {
    pub fn new(workspace_root: PathBuf) -> Self {
        let workspace_root = canonicalize_path(workspace_root);

        Self {
            process: Arc::new(Mutex::new(None)),
            request_id: AtomicU64::new(1),
            workspace_root,
            stdin: Arc::new(Mutex::new(None)),
            pending_requests: Arc::new(PendingTracker::default()),
            initialized: AtomicBool::new(false),
            open_documents: Mutex::new(HashMap::new()),
            diagnostics: Arc::new(Mutex::new(HashMap::new())),
            diagnostics_changed: Arc::new(Notify::new()),
            progress: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            process_died: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn workspace_root(&self) -> &std::path::Path {
        &self.workspace_root
    }

    /// Returns `true` once the monitor has detected that rust-analyzer's
    /// process exited. The MCP server uses this to decide when to restart.
    pub fn is_dead(&self) -> bool {
        self.process_died.load(Ordering::Acquire)
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
            .stderr(Stdio::piped())
            // Backstop for the `Drop` impl: if the client is ever dropped without an
            // explicit `shutdown()` (panic, abandoned future, ctrl-c after a select! race),
            // tokio reaps the child instead of leaving it as a zombie.
            .kill_on_drop(true);

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
            Arc::clone(&self.diagnostics_changed),
            Arc::clone(&self.progress),
        );

        *self.process.lock().await = Some(child);

        // Spawn monitor task — flips `process_died` and drains pending requests
        // when rust-analyzer exits unexpectedly.
        super::connection::spawn_process_monitor(
            Arc::clone(&self.process),
            Arc::clone(&self.process_died),
            Arc::clone(&self.pending_requests),
        );

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

        debug!("Sending LSP notification: {}", method);

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

        debug!("Sending LSP request: {} with params: {:?}", method, params);

        // Register the pending request *before* writing, so a fast response can't race past us.
        // Inherit the current MCP request id (if any) so cancel_mcp can find this LSP call later.
        let mcp_request_id = CURRENT_MCP_REQUEST_ID.try_with(|s| s.clone()).ok();
        let (tx, rx) = oneshot::channel();
        self.pending_requests.insert(
            id,
            PendingEntry {
                method: method.to_string(),
                sender: tx,
                mcp_request_id,
            },
        );

        // Send under the stdin lock so concurrent senders don't interleave headers/bodies.
        {
            let mut stdin_lock = self.stdin.lock().await;
            let Some(stdin) = stdin_lock.as_mut() else {
                self.pending_requests.take(id);
                return Err(LspError::Transport("no stdin available".to_string()));
            };
            if let Err(e) = stdin.write_all(message.as_bytes()).await {
                self.pending_requests.take(id);
                return Err(LspError::Transport(format!("write: {}", e)));
            }
            if let Err(e) = stdin.flush().await {
                self.pending_requests.take(id);
                return Err(LspError::Transport(format!("flush: {}", e)));
            }
        }

        // Wait for response with timeout.
        match tokio::time::timeout(Duration::from_secs(LSP_REQUEST_TIMEOUT_SECS), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(LspError::Cancelled),
            Err(_) => {
                self.pending_requests.take(id);
                Err(LspError::Timeout(method.to_string()))
            }
        }
    }

    /// Cancel every LSP request that was issued under the given MCP request id.
    /// For each outstanding LSP call we (a) resolve its waiting sender with
    /// `LspError::Cancelled` so the in-flight handler unblocks immediately, and
    /// (b) send `$/cancelRequest` to rust-analyzer so it can stop the work
    /// instead of computing a result we'll throw away. Any late response
    /// rust-analyzer eventually delivers finds no entry in the tracker and is
    /// silently dropped.
    pub async fn cancel_mcp(&self, mcp_request_id: &str) {
        let entries = self.pending_requests.take_for_mcp(mcp_request_id);
        if entries.is_empty() {
            return;
        }

        debug!(
            "Cancelling {} LSP requests for MCP request {}",
            entries.len(),
            mcp_request_id
        );

        for (lsp_id, entry) in entries {
            let _ = entry.sender.send(Err(LspError::Cancelled));
            let params = json!({ "id": lsp_id });
            if let Err(e) = self
                .send_notification("$/cancelRequest", Some(params))
                .await
            {
                warn!("Failed to send $/cancelRequest for {}: {}", lsp_id, e);
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
                    "typeDefinition": {
                        "linkSupport": true
                    },
                    "implementation": {
                        "linkSupport": true
                    },
                    "callHierarchy": {
                        "dynamicRegistration": false
                    },
                    "typeHierarchy": {
                        "dynamicRegistration": false
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
                    "rename": {
                        "dynamicRegistration": false,
                        "prepareSupport": true
                    },
                    "signatureHelp": {
                        "dynamicRegistration": false,
                        "signatureInformation": {
                            "documentationFormat": ["markdown", "plaintext"],
                            "parameterInformation": {
                                "labelOffsetSupport": true
                            },
                            "activeParameterSupport": true
                        }
                    },
                    "inlayHint": {
                        "dynamicRegistration": false
                    }
                },
                "workspace": {
                    "didChangeConfiguration": {
                        "dynamicRegistration": false
                    },
                    "symbol": {
                        "dynamicRegistration": false
                    },
                    "fileOperations": {
                        "willRename": true
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

    /// Block until rust-analyzer publishes a `textDocument/publishDiagnostics`
    /// for *any* URI, or `timeout` elapses. Returns `Ok(())` on a publish,
    /// `Err(())` on timeout. Call repeatedly in a loop and re-check the
    /// diagnostics map for the URI you actually care about — `Notify` pulses
    /// don't carry payload, so multiple URIs share the same wake-up.
    pub async fn wait_for_diagnostics_change(&self, timeout: Duration) -> Result<(), ()> {
        // Subscribe BEFORE the await so we don't miss a pulse that arrives
        // between the caller's last map-check and our `notified()` call.
        let notified = self.diagnostics_changed.notified();
        tokio::time::timeout(timeout, notified)
            .await
            .map_err(|_| ())
    }

    pub async fn is_open(&self, uri: &str) -> bool {
        self.open_documents.lock().await.contains_key(uri)
    }

    /// Cheap freshness check: returns true if the document is open *and* the
    /// caller's mtime matches what we have on file. Lets the server skip even
    /// reading the file from disk when nothing has changed.
    pub async fn is_open_and_fresh(&self, uri: &str, mtime: Option<SystemTime>) -> bool {
        match self.open_documents.lock().await.get(uri) {
            Some(state) => match (state.mtime, mtime) {
                (Some(stored), Some(observed)) => stored == observed,
                _ => false,
            },
            None => false,
        }
    }

    /// Sync `content` to rust-analyzer if it differs from what we last sent.
    /// Sends `textDocument/didChange` (full-text variant) when the content
    /// hash changes, otherwise just refreshes the recorded mtime so future
    /// freshness checks short-circuit. Caller must already have established
    /// that the document is open.
    pub async fn update_document(
        &self,
        uri: &str,
        content: &str,
        mtime: Option<SystemTime>,
    ) -> Result<()> {
        let new_hash = hash_content(content);

        let mut open_docs = self.open_documents.lock().await;
        let Some(state) = open_docs.get_mut(uri) else {
            // Lost open state somehow — fall back to opening fresh.
            drop(open_docs);
            return self.open_document(uri, content).await;
        };

        if state.content_hash == new_hash {
            // No textual change; just record the new mtime so we can skip the
            // disk read next time.
            state.mtime = mtime;
            return Ok(());
        }

        let new_version = state.version.saturating_add(1);
        state.version = new_version;
        state.content_hash = new_hash;
        state.mtime = mtime;
        drop(open_docs);

        // Old diagnostics are now stale for this content.
        self.diagnostics.lock().await.remove(uri);

        debug!("Updating document {} to version {}", uri, new_version);
        let params = json!({
            "textDocument": { "uri": uri, "version": new_version },
            "contentChanges": [ { "text": content } ]
        });
        self.send_notification("textDocument/didChange", Some(params))
            .await?;
        Ok(())
    }

    pub async fn open_document(&self, uri: &str, content: &str) -> Result<()> {
        self.open_document_with_mtime(uri, content, None).await
    }

    /// Variant of `open_document` that records the file's mtime so subsequent
    /// freshness checks can short-circuit before reading from disk.
    pub async fn open_document_with_mtime(
        &self,
        uri: &str,
        content: &str,
        mtime: Option<SystemTime>,
    ) -> Result<()> {
        // Hold the open_documents lock across the didOpen send so check-then-insert is atomic
        // per-URI. Concurrent callers for the same URI either bail out early ("already open")
        // or wait their turn here.
        let mut open_docs = self.open_documents.lock().await;
        if open_docs.contains_key(uri) {
            debug!("Document already open: {}", uri);
            return Ok(());
        }

        self.diagnostics.lock().await.remove(uri);

        debug!("Opening document: {}", uri);
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

        open_docs.insert(
            uri.to_string(),
            OpenDocState {
                version: 1,
                mtime,
                content_hash: hash_content(content),
            },
        );
        drop(open_docs);

        // Send didSave to trigger cargo check.
        let save_params = json!({
            "textDocument": {
                "uri": uri
            }
        });
        self.send_notification("textDocument/didSave", Some(save_params))
            .await?;

        // Wait until rust-analyzer has finished priming its symbol cache. This is
        // the modern equivalent of the old "Indexing" token — once `cachePriming`
        // ends, hover/definition/references see resolved symbols. Best-effort:
        // on timeout we fall through, individual tool handlers already cope with
        // null responses during indexing.
        let _ = self
            .wait_for_progress_end(
                "rustAnalyzer/cachePriming",
                Duration::from_secs(INDEXING_WAIT_TIMEOUT_SECS),
            )
            .await;

        Ok(())
    }

    /// Re-sync every already-open document whose on-disk state moved since we
    /// last pushed it.
    ///
    /// rust-analyzer keeps `didOpen`ed files as in-memory overlays and
    /// deliberately discards its own file-watcher events for them — the client
    /// is assumed to own their content. Our "client" is an MCP tool call, which
    /// owns nothing: every edit happens on disk, behind our back. Without this
    /// sweep, any file we ever opened stays frozen at the content we last sent,
    /// which silently corrupts workspace-wide queries *and* the cross-file
    /// resolution of the one file a request does re-read.
    ///
    /// Cheap in the common case: one `stat` per open document, no reads. Files
    /// that vanished from disk are closed so rust-analyzer falls back to its own
    /// view of them. Returns the number of documents that were re-read or closed.
    pub async fn resync_open_documents(&self) -> usize {
        // Snapshot first — the disk I/O and the `update_document` /
        // `close_document` calls below all re-take this lock.
        let snapshot: Vec<(String, Option<SystemTime>)> = self
            .open_documents
            .lock()
            .await
            .iter()
            .map(|(uri, state)| (uri.clone(), state.mtime))
            .collect();

        if snapshot.is_empty() {
            return 0;
        }

        let checks = snapshot.into_iter().map(|(uri, known_mtime)| async move {
            let path = uri_to_local_path(&uri)?;
            let current_mtime = tokio::fs::metadata(&path)
                .await
                .ok()
                .and_then(|m| m.modified().ok());

            // Unchanged mtime is the overwhelmingly common case — skip the read.
            if let (Some(known), Some(current)) = (known_mtime, current_mtime) {
                if known == current {
                    return None;
                }
            }

            match tokio::fs::read_to_string(&path).await {
                Ok(content) => Some(Resync::Changed {
                    uri,
                    content,
                    mtime: current_mtime,
                }),
                // Gone or unreadable: drop the overlay rather than keep serving
                // content that no longer exists on disk.
                Err(_) => Some(Resync::Vanished { uri }),
            }
        });

        let mut resynced = 0;
        for action in futures::future::join_all(checks)
            .await
            .into_iter()
            .flatten()
        {
            let outcome = match &action {
                Resync::Changed {
                    uri,
                    content,
                    mtime,
                } => self.update_document(uri, content, *mtime).await,
                Resync::Vanished { uri } => self.close_document(uri).await,
            };
            match outcome {
                Ok(()) => resynced += 1,
                Err(e) => warn!("Failed to re-sync {} from disk: {}", action.uri(), e),
            }
        }

        if resynced > 0 {
            debug!("Re-synced {} externally modified document(s)", resynced);
        }
        resynced
    }

    /// Send `textDocument/didClose` and forget the document. No-op if the
    /// URI was not open. Used after a file move so rust-analyzer drops its
    /// stale view of the old path.
    pub async fn close_document(&self, uri: &str) -> Result<()> {
        let removed = self.open_documents.lock().await.remove(uri).is_some();
        if !removed {
            return Ok(());
        }
        self.diagnostics.lock().await.remove(uri);
        let params = json!({
            "textDocument": { "uri": uri }
        });
        self.send_notification("textDocument/didClose", Some(params))
            .await?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        if self.initialized.swap(false, Ordering::AcqRel) {
            if let Err(e) = self.send_request("shutdown", None).await {
                debug!(
                    "LSP shutdown request failed (process may already be gone): {}",
                    e
                );
            }
            if let Err(e) = self.send_notification("exit", None).await {
                debug!("LSP exit notification failed: {}", e);
            }
        }

        if let Some(mut process) = self.process.lock().await.take() {
            if let Err(e) = process.kill().await {
                warn!("Failed to kill rust-analyzer process: {}", e);
            }
            if let Err(e) = process.wait().await {
                warn!("Failed to reap rust-analyzer process: {}", e);
            }
        }

        // Drop stdin so the BufWriter releases the FD.
        *self.stdin.lock().await = None;

        // Clear open documents and diagnostics.
        self.open_documents.lock().await.clear();
        self.diagnostics.lock().await.clear();
        Ok(())
    }
}

impl Drop for RustAnalyzerClient {
    /// Backstop for callers that drop the client without `shutdown()`.
    /// `kill_on_drop(true)` on the spawned `Command` already arranges for
    /// tokio to reap the child as soon as the `Child` value drops; here we
    /// just make sure that happens by taking the `Child` out of its mutex.
    /// We intentionally don't `await` anything — `Drop` is sync.
    fn drop(&mut self) {
        // Best effort: try to grab the child without blocking. If the mutex
        // is contended (e.g. shutdown is in flight on another task), the
        // owner of that lock is responsible for cleanup.
        if let Ok(mut guard) = self.process.try_lock() {
            if let Some(mut child) = guard.take() {
                // start_kill is sync; the kernel-level reap happens via the
                // tokio runtime's child-reaper.
                let _ = child.start_kill();
            }
        }
    }
}

/// What `resync_open_documents` decided to do with one open document.
enum Resync {
    Changed {
        uri: String,
        content: String,
        mtime: Option<SystemTime>,
    },
    Vanished {
        uri: String,
    },
}

impl Resync {
    fn uri(&self) -> &str {
        match self {
            Resync::Changed { uri, .. } | Resync::Vanished { uri } => uri,
        }
    }
}

/// Inverse of the `format!("file://{}", path.display())` we use everywhere to
/// build document URIs. Deliberately not a general URI parser — we only ever
/// resolve URIs we minted ourselves, which are never percent-encoded.
fn uri_to_local_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
}

fn hash_content(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn find_rust_analyzer() -> Result<PathBuf> {
    if let Ok(p) = which::which("rust-analyzer") {
        return Ok(p);
    }
    // Fallback: ~/.cargo/bin install location, in case PATH wasn't inherited
    // (e.g. when this binary is launched from a GUI on macOS/Linux).
    if let Some(home) = std::env::var_os("HOME") {
        let cargo_bin = PathBuf::from(home).join(".cargo/bin/rust-analyzer");
        if cargo_bin.exists() {
            return Ok(cargo_bin);
        }
    }
    Err(anyhow!(
        "Failed to find rust-analyzer in PATH or ~/.cargo/bin. Please ensure rust-analyzer is installed."
    ))
}
