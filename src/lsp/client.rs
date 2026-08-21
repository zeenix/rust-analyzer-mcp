use anyhow::{anyhow, Result};
use log::info;
use serde_json::{json, Value};
use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    process::{Child, Command},
    sync::{oneshot, watch, Mutex},
    task::JoinHandle,
};

use crate::{
    config::{
        DOCUMENT_OPEN_DELAY_MILLIS, GRACEFUL_SHUTDOWN_TIMEOUT_SECS, LSP_REQUEST_TIMEOUT_SECS,
    },
    protocol::lsp::LSPRequest,
    uri,
};

pub struct RustAnalyzerClient {
    pub(super) process: Option<Child>,
    pub(super) request_id: Arc<Mutex<u64>>,
    pub(super) workspace_root: PathBuf,
    pub(super) stdin: Option<BufWriter<tokio::process::ChildStdin>>,
    pub(super) pending_requests: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    pub(super) initialized: bool,
    /// What rust-analyzer was last told about each open document, keyed by normalized URI.
    pub(super) open_documents: Arc<Mutex<HashMap<String, OpenDocument>>>,
    pub(super) diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    /// Whether rust-analyzer last reported itself quiescent, i.e. with no background work such
    /// as loading the workspace in flight. Fed by its `experimental/serverStatus` notifications.
    pub(super) quiescent: watch::Sender<bool>,
    /// Open documents whose `didSave` has been sent, see [`Self::open_document`].
    pub(super) saved_documents: HashSet<String>,
    /// The task reading rust-analyzer's stdout; it finishing means rust-analyzer is gone.
    pub(super) reader: Option<JoinHandle<()>>,
}

impl RustAnalyzerClient {
    pub fn new(workspace_root: PathBuf) -> Self {
        let workspace_root = uri::absolute(&workspace_root);

        Self {
            process: None,
            request_id: Arc::new(Mutex::new(1)),
            workspace_root,
            stdin: None,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            initialized: false,
            open_documents: Arc::new(Mutex::new(HashMap::new())),
            diagnostics: Arc::new(Mutex::new(HashMap::new())),
            quiescent: watch::channel(false).0,
            saved_documents: HashSet::new(),
            reader: None,
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
            .stderr(Stdio::piped())
            // So that a start failing halfway cannot leave an orphaned rust-analyzer behind.
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

        self.stdin = Some(BufWriter::new(stdin));

        // Start connection handlers, with a pending-request map of their own: the reader of an
        // earlier process fails whatever is left in its map when it finishes.
        self.pending_requests = Arc::new(Mutex::new(HashMap::new()));
        self.reader = Some(super::connection::start_handlers(
            stdout,
            stderr,
            Arc::clone(&self.pending_requests),
            Arc::clone(&self.diagnostics),
            self.quiescent.clone(),
        ));

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

        // Register the response channel before writing the request: a response arriving
        // between the write and the registration would be dropped by the reader task,
        // turning into a spurious request timeout.
        let (tx, rx) = oneshot::channel();
        let pending_requests = self.pending_requests.clone();
        pending_requests.lock().await.insert(id, tx);

        let Some(stdin) = &mut self.stdin else {
            pending_requests.lock().await.remove(&id);
            return Err(anyhow!("No stdin available"));
        };

        let mut written = stdin.write_all(message.as_bytes()).await;
        if written.is_ok() {
            written = stdin.flush().await;
        }
        if let Err(e) = written {
            pending_requests.lock().await.remove(&id);
            return Err(e.into());
        }

        // Wait for response with timeout. The channel only closes unanswered when the reader
        // task gave up on rust-analyzer's stdout, i.e. rust-analyzer is gone.
        match tokio::time::timeout(Duration::from_secs(LSP_REQUEST_TIMEOUT_SECS), rx).await {
            Ok(response) => response.map_err(|_| anyhow!("rust-analyzer exited before responding")),
            Err(_) => {
                // Unregister so an abandoned request cannot leak its pending entry.
                pending_requests.lock().await.remove(&id);
                Err(anyhow!("Request timeout"))
            }
        }
    }

    async fn initialize(&mut self) -> Result<()> {
        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": uri::path_to_uri(&self.workspace_root)?,
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
                // Opt into `experimental/serverStatus` notifications, which report whether
                // rust-analyzer is quiescent.
                "experimental": {
                    "serverStatusNotification": true
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

    /// Tells rust-analyzer about `content`, opening the document or updating it as needed.
    ///
    /// An open document's content belongs to us for as long as it stays open: rust-analyzer
    /// refuses to re-read one from disk, so an edit anyone else makes is invisible to it until
    /// this sends the new content along. Every request for a document that has been edited since
    /// it was opened -- which, with an agent at the other end, is most of them -- was answered
    /// from the content it had when it was first looked at.
    pub async fn open_document(&mut self, uri: &str, content: &str) -> Result<()> {
        let key = uri::normalize(uri);
        let content_hash = hash(content);
        let known = self
            .open_documents
            .lock()
            .await
            .get(&key)
            .map(|document| (document.version, document.content_hash));

        match known {
            None => {
                info!("Opening document: {}", uri);
                let params = json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "rust",
                        "version": FIRST_DOCUMENT_VERSION,
                        "text": content
                    }
                });
                self.send_notification("textDocument/didOpen", Some(params))
                    .await?;
                self.open_documents.lock().await.insert(
                    key.clone(),
                    OpenDocument {
                        version: FIRST_DOCUMENT_VERSION,
                        content_hash,
                    },
                );
            }
            Some((_, known_hash)) if known_hash == content_hash => {
                info!("Document already open and unchanged: {}", uri);
            }
            Some((version, _)) => {
                // Whole-document changes are what the LSP calls a content change with no range,
                // and what rust-analyzer's handler looks for first. Sending the file as one is
                // both simpler and safer than working out a diff nobody asked us for.
                let version = version + 1;
                info!("Document changed, sending version {} of {}", version, uri);
                let params = json!({
                    "textDocument": {
                        "uri": uri,
                        "version": version
                    },
                    "contentChanges": [{ "text": content }]
                });
                self.send_notification("textDocument/didChange", Some(params))
                    .await?;
                self.open_documents.lock().await.insert(
                    key.clone(),
                    OpenDocument {
                        version,
                        content_hash,
                    },
                );

                // Whatever was reported about the content just replaced is no longer about
                // anything: drop it, and let the didSave below ask for a check of what is there
                // now.
                self.diagnostics.lock().await.remove(&key);
                self.saved_documents.remove(&key);
            }
        }

        // A didSave makes rust-analyzer run cargo check for the document's package. It has to
        // wait until rust-analyzer is quiescent, though: during a workspace load the freshly
        // opened document has no source root yet, and rust-analyzer's didSave handler then panics
        // and takes the whole process down (seen with 1.97 and 1.98). So hold it back while busy
        // and send it on the document's next use instead; in the meantime the workspace-wide
        // cargo check rust-analyzer runs on its own once quiescent covers the document anyway.
        // The flag is only a snapshot, so this narrows the window rather than closing it.
        if self.saved_documents.contains(&key) {
            return Ok(());
        }
        if !*self.quiescent.borrow() {
            info!("rust-analyzer is busy, holding back didSave for {}", uri);
            return Ok(());
        }

        // Drop the diagnostics stored so far, so that what gets reported next comes from the cargo
        // check this didSave triggers rather than from before it.
        self.diagnostics.lock().await.remove(&key);
        let save_params = json!({
            "textDocument": {
                "uri": uri
            }
        });
        self.send_notification("textDocument/didSave", Some(save_params))
            .await?;
        self.saved_documents.insert(key);

        // Give rust-analyzer time to get cargo check going.
        tokio::time::sleep(Duration::from_millis(DOCUMENT_OPEN_DELAY_MILLIS)).await;

        Ok(())
    }

    /// Shuts rust-analyzer down, attempting the graceful LSP handshake first.
    pub async fn shutdown(&mut self) -> Result<()> {
        if self.initialized {
            // Bound the handshake so a wedged rust-analyzer cannot stall the shutdown.
            let handshake = async {
                let _ = self.send_request("shutdown", None).await;
                let _ = self.send_notification("exit", None).await;
            };
            let timeout = Duration::from_secs(GRACEFUL_SHUTDOWN_TIMEOUT_SECS);
            if tokio::time::timeout(timeout, handshake).await.is_err() {
                info!("Graceful shutdown timed out");
            }
        }

        self.force_kill().await;
        Ok(())
    }

    /// Kills rust-analyzer immediately, without the LSP shutdown handshake.
    ///
    /// Meant for when a graceful [`Self::shutdown`] was aborted, so the process must not be left
    /// behind.
    pub async fn force_kill(&mut self) {
        if let Some(mut process) = self.process.take() {
            // Kill the process and wait for it to actually exit.
            let _ = process.kill().await;
            let _ = process.wait().await;
        }

        // Clear open documents and diagnostics.
        self.open_documents.lock().await.clear();
        self.saved_documents.clear();
        self.diagnostics.lock().await.clear();
        self.initialized = false;
    }

    /// Whether rust-analyzer is gone, i.e. its stdout has closed because it exited or is about to.
    pub fn is_gone(&self) -> bool {
        self.reader.as_ref().is_some_and(JoinHandle::is_finished)
    }

    /// The exit status of the rust-analyzer process, if it has exited.
    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        self.process.as_mut()?.try_wait().ok().flatten()
    }
}

/// What rust-analyzer was last told about a document, so that the next thing it is told about
/// it can follow on.
pub(super) struct OpenDocument {
    /// The version last sent. rust-analyzer wants these to climb, and echoes the current one
    /// back with every diagnostic it publishes.
    version: u64,
    /// Fingerprint of the content last sent, which is how an edit is told from a re-read. A
    /// hash rather than the content itself: an agent works its way through a lot of files, and
    /// nothing here needs the old text back.
    content_hash: u64,
}

/// The version a document is opened at, which every later change counts up from.
const FIRST_DOCUMENT_VERSION: u64 = 1;

fn hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
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

#[cfg(test)]
mod tests {
    use super::*;

    // Some of these stand a real process up in rust-analyzer's place, which takes shell tooling
    // this repository only assumes on Unix.
    #[cfg(unix)]
    use tokio::io::AsyncReadExt;

    #[cfg(unix)]
    const URI: &str = "file:///tmp/lib.rs";

    #[cfg(unix)]
    #[tokio::test]
    async fn did_save_is_held_back_while_rust_analyzer_is_busy() {
        let (mut client, mut child) = client_with_fake_stdin();

        open(&mut client).await;
        open(&mut client).await;

        let sent = written(&mut client, &mut child).await;
        assert_eq!(sent.matches("textDocument/didOpen").count(), 1, "{sent}");
        assert_eq!(sent.matches("textDocument/didSave").count(), 0, "{sent}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn held_back_did_save_is_sent_once_rust_analyzer_is_quiescent() {
        let (mut client, mut child) = client_with_fake_stdin();

        open(&mut client).await;
        client.quiescent.send_replace(true);
        open(&mut client).await;
        open(&mut client).await;

        let sent = written(&mut client, &mut child).await;
        assert_eq!(sent.matches("textDocument/didOpen").count(), 1, "{sent}");
        assert_eq!(sent.matches("textDocument/didSave").count(), 1, "{sent}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn did_save_follows_did_open_while_rust_analyzer_is_quiescent() {
        let (mut client, mut child) = client_with_fake_stdin();
        client.quiescent.send_replace(true);

        open(&mut client).await;
        open(&mut client).await;

        let sent = written(&mut client, &mut child).await;
        assert_eq!(sent.matches("textDocument/didOpen").count(), 1, "{sent}");
        assert_eq!(sent.matches("textDocument/didSave").count(), 1, "{sent}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_edited_document_is_sent_as_a_change() {
        let (mut client, mut child) = client_with_fake_stdin();
        client.quiescent.send_replace(true);

        client.open_document(URI, "fn main() {}").await.unwrap();
        client
            .open_document(URI, "fn main() { let x = 1; }")
            .await
            .unwrap();

        let sent = written(&mut client, &mut child).await;
        assert_eq!(sent.matches("textDocument/didOpen").count(), 1, "{sent}");
        assert_eq!(sent.matches("textDocument/didChange").count(), 1, "{sent}");
        assert!(sent.contains(r#"let x = 1;"#), "{sent}");
        // The version climbs, which is what rust-analyzer stamps its diagnostics with.
        assert!(sent.contains(r#""version":2"#), "{sent}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_document_that_did_not_change_is_not_sent_again() {
        // rust-analyzer drops a change whose text it already has, so the only thing sending one
        // achieves is a check that never reports anything back.
        let (mut client, mut child) = client_with_fake_stdin();
        client.quiescent.send_replace(true);

        open(&mut client).await;
        open(&mut client).await;

        let sent = written(&mut client, &mut child).await;
        assert_eq!(sent.matches("textDocument/didOpen").count(), 1, "{sent}");
        assert_eq!(sent.matches("textDocument/didChange").count(), 0, "{sent}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn every_edit_gets_the_next_version() {
        let (mut client, mut child) = client_with_fake_stdin();
        client.quiescent.send_replace(true);

        for content in ["fn main() {}", "fn main() { 1; }", "fn main() { 2; }"] {
            client.open_document(URI, content).await.unwrap();
        }

        let sent = written(&mut client, &mut child).await;
        assert!(sent.contains(r#""version":1"#), "{sent}");
        assert!(sent.contains(r#""version":2"#), "{sent}");
        assert!(sent.contains(r#""version":3"#), "{sent}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_edit_drops_what_was_reported_about_the_old_content() {
        let (mut client, mut child) = client_with_fake_stdin();
        client.quiescent.send_replace(true);

        client.open_document(URI, "fn main() {}").await.unwrap();
        client
            .diagnostics
            .lock()
            .await
            .insert(URI.to_string(), vec![json!({ "message": "stale" })]);
        client
            .open_document(URI, "fn main() { let x = 1; }")
            .await
            .unwrap();

        assert!(!client.diagnostics.lock().await.contains_key(URI));
        // And the check that reports on the new content is asked for again.
        let sent = written(&mut client, &mut child).await;
        assert_eq!(sent.matches("textDocument/didSave").count(), 2, "{sent}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_status_reflects_whether_rust_analyzer_is_alive() {
        let mut client = RustAnalyzerClient::new(PathBuf::from("."));
        // The shell lives until its stdin closes, then exits with 3.
        let mut child = Command::new("sh")
            .args(["-c", "read _; exit 3"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        client.process = Some(child);

        assert!(client.exit_status().is_none());

        drop(stdin);
        client.process.as_mut().unwrap().wait().await.unwrap();
        assert_eq!(
            client.exit_status().and_then(|status| status.code()),
            Some(3)
        );
    }

    #[tokio::test]
    async fn is_gone_once_rust_analyzer_closes_its_stdout() {
        let mut client = RustAnalyzerClient::new(PathBuf::from("."));
        let (stdout, rust_analyzer) = tokio::io::duplex(64);
        client.reader = Some(super::super::connection::start_handlers(
            stdout,
            tokio::io::empty(),
            Arc::clone(&client.pending_requests),
            Arc::clone(&client.diagnostics),
            client.quiescent.clone(),
        ));
        tokio::task::yield_now().await;
        assert!(!client.is_gone());

        drop(rust_analyzer);
        tokio::time::timeout(Duration::from_secs(5), client.reader.as_mut().unwrap())
            .await
            .expect("reader must finish once stdout closes")
            .unwrap();
        assert!(client.is_gone());
    }

    #[tokio::test]
    async fn workspace_diagnostics_fails_once_rust_analyzer_is_gone() {
        let mut client = RustAnalyzerClient::new(PathBuf::from("."));
        let mut reader = tokio::spawn(async {});
        (&mut reader).await.unwrap();
        client.reader = Some(reader);

        // Must not fall back to an empty, i.e. clean-looking, report.
        assert!(client.workspace_diagnostics().await.is_err());
    }

    /// A client whose "rust-analyzer" is a `cat` process, so that everything the client writes
    /// to its stdin can be read back from the child's stdout. Starts out non-quiescent, like a
    /// freshly started rust-analyzer.
    #[cfg(unix)]
    fn client_with_fake_stdin() -> (RustAnalyzerClient, Child) {
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut client = RustAnalyzerClient::new(PathBuf::from("."));
        client.stdin = Some(BufWriter::new(child.stdin.take().unwrap()));
        (client, child)
    }

    #[cfg(unix)]
    async fn open(client: &mut RustAnalyzerClient) {
        client.open_document(URI, "fn main() {}").await.unwrap();
    }

    /// Closes the client's stdin and returns everything it wrote.
    #[cfg(unix)]
    async fn written(client: &mut RustAnalyzerClient, child: &mut Child) -> String {
        client.stdin.take();
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .await
            .unwrap();
        child.wait().await.unwrap();
        output
    }
}
