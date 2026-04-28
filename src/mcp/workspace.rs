use anyhow::{anyhow, Result};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
use tracing::warn;

use crate::{
    config::{MAX_RESTART_COUNT, RESTART_WINDOW_SECS},
    lsp::RustAnalyzerClient,
};

/// One isolated rust-analyzer workspace. Owns its own subprocess (lazy), its
/// own restart-rate-limit window, and its own root path. Multiple entries live
/// inside [`WorkspaceRegistry`] keyed by stable string ids (`ws-1`, `ws-2`, …).
///
/// `root` is interior-mutable so the legacy `set_workspace` semantics — replace
/// the default workspace's root — can be expressed as "update root + drop the
/// existing client" without invalidating the entry's id.
pub(super) struct WorkspaceEntry {
    id: String,
    root: parking_lot::RwLock<PathBuf>,
    client: RwLock<Option<Arc<RustAnalyzerClient>>>,
    /// Timestamps of recent automatic restarts, oldest first. Used to back off
    /// when rust-analyzer keeps crashing (see `MAX_RESTART_COUNT`).
    restart_history: parking_lot::Mutex<VecDeque<Instant>>,
}

impl WorkspaceEntry {
    fn new(id: String, root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            id,
            root: parking_lot::RwLock::new(canonicalize_root(root)),
            client: RwLock::new(None),
            restart_history: parking_lot::Mutex::new(VecDeque::new()),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn root_clone(&self) -> PathBuf {
        self.root.read().clone()
    }

    /// Returns a healthy `RustAnalyzerClient`, starting it if missing or
    /// restarting it if the previous one's process has exited. Restarts are
    /// rate-limited via `restart_history` to avoid hot-loops on a process that
    /// keeps crashing.
    pub async fn ensure_client_started(&self) -> Result<Arc<RustAnalyzerClient>> {
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
            warn!(
                "rust-analyzer process for workspace {} died; restarting",
                self.id
            );
            if let Some(old) = guard.take() {
                // Best-effort cleanup in the background — the process is gone,
                // but stdin/document state still needs to be dropped.
                tokio::spawn(async move {
                    let _ = old.shutdown().await;
                });
            }
        }

        let workspace_root = self.root.read().clone();
        let client = Arc::new(RustAnalyzerClient::new(workspace_root));
        client.start().await?;
        *guard = Some(Arc::clone(&client));
        Ok(client)
    }

    pub async fn current_client(&self) -> Result<Arc<RustAnalyzerClient>> {
        self.client
            .read()
            .await
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| anyhow!("Client not initialized for workspace {}", self.id))
    }

    /// Returns the running client without an error if none. Used by paths that
    /// want to broadcast (e.g. cancellation) without forcing a startup.
    pub async fn maybe_client(&self) -> Option<Arc<RustAnalyzerClient>> {
        self.client.read().await.as_ref().map(Arc::clone)
    }

    pub async fn open_document_if_needed(&self, file_path: &str) -> Result<String> {
        let workspace_root = self.root.read().clone();
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

    /// Replace the workspace root in place: shut down the existing client (if
    /// any) so the next `ensure_client_started` boots rust-analyzer in the new
    /// directory.
    pub async fn replace_root(&self, new_root: PathBuf) {
        if let Some(c) = self.client.write().await.take() {
            let _ = c.shutdown().await;
        }
        *self.root.write() = canonicalize_root(new_root);
    }

    /// Tear down the client (if running). Used when removing a workspace from
    /// the registry.
    pub async fn shutdown_client(&self) {
        if let Some(c) = self.client.write().await.take() {
            let _ = c.shutdown().await;
        }
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
}

/// Owns every `WorkspaceEntry` for the server. The first inserted entry is the
/// default — every tool call without an explicit `workspace_id` resolves
/// there. Insertion order is tracked separately so the default stays stable
/// across removals of non-default entries.
pub(super) struct WorkspaceRegistry {
    by_id: HashMap<String, Arc<WorkspaceEntry>>,
    order: Vec<String>,
    next_id: u64,
}

impl WorkspaceRegistry {
    pub fn with_initial_root(root: PathBuf) -> Self {
        let mut reg = Self {
            by_id: HashMap::new(),
            order: Vec::new(),
            next_id: 1,
        };
        reg.add(root);
        reg
    }

    pub fn add(&mut self, root: PathBuf) -> Arc<WorkspaceEntry> {
        let id = format!("ws-{}", self.next_id);
        self.next_id += 1;
        let entry = WorkspaceEntry::new(id.clone(), root);
        self.by_id.insert(id.clone(), Arc::clone(&entry));
        self.order.push(id);
        entry
    }

    pub fn remove(&mut self, id: &str) -> Option<Arc<WorkspaceEntry>> {
        let entry = self.by_id.remove(id)?;
        self.order.retain(|x| x != id);
        Some(entry)
    }

    pub fn get(&self, id: &str) -> Option<Arc<WorkspaceEntry>> {
        self.by_id.get(id).cloned()
    }

    pub fn default(&self) -> Option<Arc<WorkspaceEntry>> {
        self.order
            .first()
            .and_then(|id| self.by_id.get(id).cloned())
    }

    pub fn list(&self) -> Vec<Arc<WorkspaceEntry>> {
        self.order
            .iter()
            .filter_map(|id| self.by_id.get(id).cloned())
            .collect()
    }
}

fn canonicalize_root(root: PathBuf) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| {
        if root.is_absolute() {
            root.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&root)
        }
    })
}
