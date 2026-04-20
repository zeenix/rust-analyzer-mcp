use anyhow::{anyhow, Result};
use log::{info, warn};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    process::{Child, Command},
    sync::{oneshot, Mutex},
};

use crate::{
    config::{lsp_request_timeout_secs, DOCUMENT_OPEN_DELAY_MILLIS, RUST_ANALYZER_PATH_ENV},
    protocol::lsp::LSPRequest,
};
use url::Url;

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

        #[cfg(windows)]
        {
            // Windows 下某些 MCP 客户端会裁剪 HOME/USERPROFILE，导致 rust-analyzer 初始化卡死。
            let profile_for_child = std::env::var("USERPROFILE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(resolve_windows_user_profile_from_registry);

            if std::env::var("USERPROFILE").is_err() {
                if let Some(profile) = &profile_for_child {
                    cmd.env("USERPROFILE", profile);
                }
            }
            if std::env::var("HOME").is_err() {
                if let Some(profile) = &profile_for_child {
                    cmd.env("HOME", profile);
                }
            }
            if std::env::var("CARGO_HOME").is_err() {
                if let Some(cargo_home) = collect_registry_cargo_homes().into_iter().next() {
                    cmd.env("CARGO_HOME", cargo_home);
                }
            }
            if std::env::var("RUSTUP_HOME").is_err() {
                if let Some(rustup_home) = collect_registry_rustup_homes().into_iter().next() {
                    cmd.env("RUSTUP_HOME", rustup_home);
                }
            }
        }

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

        // Set up response channel.
        let (tx, rx) = oneshot::channel();
        // 先注册 pending，再写请求，避免极快响应导致竞态丢包。
        self.pending_requests.lock().await.insert(id, tx);

        if let Some(stdin) = &mut self.stdin {
            if let Err(e) = stdin.write_all(message.as_bytes()).await {
                // 写失败时同步清理 pending，避免后续请求被污染。
                self.pending_requests.lock().await.remove(&id);
                return Err(anyhow!(
                    "Failed to write LSP request '{}' (id={}): {}",
                    method,
                    id,
                    e
                ));
            }
            if let Err(e) = stdin.flush().await {
                self.pending_requests.lock().await.remove(&id);
                return Err(anyhow!(
                    "Failed to flush LSP request '{}' (id={}): {}",
                    method,
                    id,
                    e
                ));
            }
        } else {
            self.pending_requests.lock().await.remove(&id);
            return Err(anyhow!("No stdin available"));
        }
        // Wait for response with timeout.
        match tokio::time::timeout(Duration::from_secs(lsp_request_timeout_secs()), rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => {
                self.pending_requests.lock().await.remove(&id);
                Err(anyhow!(
                    "LSP request '{}' (id={}) cancelled before response",
                    method,
                    id
                ))
            }
            Err(_) => {
                self.pending_requests.lock().await.remove(&id);
                let process_status = if let Some(process) = &mut self.process {
                    match process.try_wait() {
                        Ok(Some(status)) => format!("rust-analyzer exited: {}", status),
                        Ok(None) => "rust-analyzer still running".to_string(),
                        Err(e) => format!("rust-analyzer status unavailable: {}", e),
                    }
                } else {
                    "rust-analyzer process missing".to_string()
                };
                Err(anyhow!(
                    "LSP request '{}' (id={}) timed out after {}s ({})",
                    method,
                    id,
                    lsp_request_timeout_secs(),
                    process_status
                ))
            }
        }
    }

    async fn initialize(&mut self) -> Result<()> {
        let root_uri = to_file_uri(&self.workspace_root)?;
        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
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

fn find_rust_analyzer() -> Result<PathBuf> {
    if let Ok(path) = std::env::var(RUST_ANALYZER_PATH_ENV) {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
        warn!(
            "{} is set but points to a missing path: {}",
            RUST_ANALYZER_PATH_ENV,
            candidate.display()
        );
    }

    if let Ok(found) = which::which("rust-analyzer") {
        return Ok(found);
    }

    let candidates = collect_rust_analyzer_candidates();
    for candidate in &candidates {
        if candidate.is_file() {
            info!(
                "Found rust-analyzer via fallback path: {}",
                candidate.display()
            );
            return Ok(candidate.to_path_buf());
        }
    }

    if let Some(via_rustup) = resolve_rust_analyzer_via_rustup() {
        info!(
            "Found rust-analyzer via rustup resolution: {}",
            via_rustup.display()
        );
        return Ok(via_rustup);
    }

    let searched_preview = candidates
        .iter()
        .take(8)
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    Err(anyhow!(
        "Failed to find rust-analyzer. Checked PATH, {}, registry-backed candidates, and rustup. Sample searched paths: [{}]. Please set {} explicitly.",
        RUST_ANALYZER_PATH_ENV,
        searched_preview,
        RUST_ANALYZER_PATH_ENV
    ))
}

fn collect_rust_analyzer_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    #[cfg(windows)]
    let executable_name = "rust-analyzer.exe";
    #[cfg(not(windows))]
    let executable_name = "rust-analyzer";

    let mut home_roots: Vec<PathBuf> = Vec::new();

    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        push_unique_path(&mut home_roots, PathBuf::from(cargo_home));
    }
    if let Ok(home) = std::env::var("HOME") {
        push_unique_path(&mut home_roots, PathBuf::from(home).join(".cargo"));
    }
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        push_unique_path(&mut home_roots, PathBuf::from(&user_profile).join(".cargo"));
    }
    if let (Ok(home_drive), Ok(home_path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH"))
    {
        push_unique_path(
            &mut home_roots,
            PathBuf::from(format!("{}{}", home_drive, home_path)).join(".cargo"),
        );
    }
    #[cfg(windows)]
    {
        for cargo_home in collect_registry_cargo_homes() {
            push_unique_path(&mut home_roots, cargo_home);
        }
    }

    for cargo_home in home_roots {
        push_unique_path(
            &mut candidates,
            cargo_home.join("bin").join(executable_name),
        );
    }

    let mut rustup_homes: Vec<PathBuf> = Vec::new();
    if let Ok(rustup_home) = std::env::var("RUSTUP_HOME") {
        push_unique_path(&mut rustup_homes, PathBuf::from(rustup_home));
    }
    if let Ok(home) = std::env::var("HOME") {
        push_unique_path(&mut rustup_homes, PathBuf::from(home).join(".rustup"));
    }
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        push_unique_path(
            &mut rustup_homes,
            PathBuf::from(user_profile).join(".rustup"),
        );
    }
    if let (Ok(home_drive), Ok(home_path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH"))
    {
        push_unique_path(
            &mut rustup_homes,
            PathBuf::from(format!("{}{}", home_drive, home_path)).join(".rustup"),
        );
    }
    #[cfg(windows)]
    {
        for rustup_home in collect_registry_rustup_homes() {
            push_unique_path(&mut rustup_homes, rustup_home);
        }
    }

    for rustup_home in rustup_homes {
        let toolchains_dir = rustup_home.join("toolchains");
        let Ok(entries) = fs::read_dir(&toolchains_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            push_unique_path(
                &mut candidates,
                entry.path().join("bin").join(executable_name),
            );
        }
    }

    #[cfg(windows)]
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        push_unique_path(
            &mut candidates,
            PathBuf::from(local_app_data)
                .join("Programs")
                .join("Rust Analyzer")
                .join(executable_name),
        );
    }

    candidates
}

fn push_unique_path(target: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !target.iter().any(|existing| existing == &candidate) {
        target.push(candidate);
    }
}

fn resolve_rust_analyzer_via_rustup() -> Option<PathBuf> {
    for rustup_binary in collect_rustup_binary_candidates() {
        if !rustup_binary.is_file() {
            continue;
        }

        let output = StdCommand::new(&rustup_binary)
            .args(["which", "rust-analyzer"])
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }

        let output_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output_path.is_empty() {
            continue;
        }

        let resolved_path = PathBuf::from(output_path);
        if resolved_path.is_file() {
            return Some(resolved_path);
        }
    }
    None
}

fn collect_rustup_binary_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    #[cfg(windows)]
    let rustup_name = "rustup.exe";
    #[cfg(not(windows))]
    let rustup_name = "rustup";

    if let Ok(found) = which::which("rustup") {
        push_unique_path(&mut candidates, found);
    }

    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        push_unique_path(
            &mut candidates,
            PathBuf::from(cargo_home).join("bin").join(rustup_name),
        );
    }
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        push_unique_path(
            &mut candidates,
            PathBuf::from(user_profile)
                .join(".cargo")
                .join("bin")
                .join(rustup_name),
        );
    }
    if let (Ok(home_drive), Ok(home_path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH"))
    {
        push_unique_path(
            &mut candidates,
            PathBuf::from(format!("{}{}", home_drive, home_path))
                .join(".cargo")
                .join("bin")
                .join(rustup_name),
        );
    }
    #[cfg(windows)]
    {
        for cargo_home in collect_registry_cargo_homes() {
            push_unique_path(&mut candidates, cargo_home.join("bin").join(rustup_name));
        }
    }

    candidates
}

#[cfg(windows)]
fn collect_registry_cargo_homes() -> Vec<PathBuf> {
    collect_registry_homes("CARGO_HOME", ".cargo")
}

#[cfg(windows)]
fn collect_registry_rustup_homes() -> Vec<PathBuf> {
    collect_registry_homes("RUSTUP_HOME", ".rustup")
}

#[cfg(windows)]
fn collect_registry_homes(key: &str, fallback_suffix: &str) -> Vec<PathBuf> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let mut homes: Vec<PathBuf> = Vec::new();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    if let Ok(env_key) = hkcu.open_subkey("Environment") {
        if let Ok(value) = env_key.get_value::<String, _>(key) {
            push_unique_path(&mut homes, PathBuf::from(value));
        }
    }

    if let Ok(volatile_key) = hkcu.open_subkey("Volatile Environment") {
        if let Ok(user_profile) = volatile_key.get_value::<String, _>("USERPROFILE") {
            push_unique_path(
                &mut homes,
                PathBuf::from(user_profile).join(fallback_suffix),
            );
        }
    }

    homes
}

#[cfg(windows)]
fn resolve_windows_user_profile_from_registry() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    if let Ok(volatile_key) = hkcu.open_subkey("Volatile Environment") {
        if let Ok(profile) = volatile_key.get_value::<String, _>("USERPROFILE") {
            if !profile.trim().is_empty() {
                return Some(profile);
            }
        }
    }

    if let Ok(env_key) = hkcu.open_subkey("Environment") {
        if let Ok(profile) = env_key.get_value::<String, _>("USERPROFILE") {
            if !profile.trim().is_empty() {
                return Some(profile);
            }
        }
    }

    None
}
