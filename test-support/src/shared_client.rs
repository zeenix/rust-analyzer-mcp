use anyhow::Result;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, RwLock},
};

// Global singleton pool of shared MCP servers
static SHARED_SERVERS: Lazy<Arc<RwLock<HashMap<String, Arc<SharedMCPServer>>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

/// A shared MCP server instance that multiple tests can use.
struct SharedMCPServer {
    process: Arc<Mutex<Option<Child>>>,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    stdout: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
    request_id: AtomicU64,
    workspace_path: PathBuf,
    initialized: AtomicBool,
    client_count: Arc<AtomicU64>,
}

impl SharedMCPServer {
    async fn new(workspace_path: PathBuf, project_type: &str) -> Result<Arc<Self>> {
        eprintln!(
            "[SharedMCPServer] Creating new server for {} at {:?}",
            project_type, workspace_path
        );

        // Use the built binary directly
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let project_root = std::path::Path::new(&manifest_dir);

        let release_binary = project_root.join("target/release/rust-analyzer-mcp");
        let debug_binary = project_root.join("target/debug/rust-analyzer-mcp");

        let binary = if release_binary.exists() {
            release_binary
        } else if debug_binary.exists() {
            debug_binary
        } else {
            return Err(anyhow::anyhow!("rust-analyzer-mcp binary not found"));
        };

        let mut process = Command::new(&binary)
            .arg(workspace_path.to_str().unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());
        let stderr = process.stderr.take().unwrap();

        // Spawn task to consume stderr
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut stderr_reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = stderr_reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                if !line.trim().is_empty() {
                    eprintln!("[shared-mcp stderr] {}", line.trim());
                }
                line.clear();
            }
        });

        let server = Arc::new(Self {
            process: Arc::new(Mutex::new(Some(process))),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(stdout)),
            request_id: AtomicU64::new(1),
            workspace_path,
            initialized: AtomicBool::new(false),
            client_count: Arc::new(AtomicU64::new(0)),
        });

        // Initialize the server once
        if !server.initialized.load(Ordering::SeqCst) {
            server.initialize().await?;
            server.initialized.store(true, Ordering::SeqCst);
        }

        Ok(server)
    }

    async fn initialize(&self) -> Result<()> {
        let response = self
            .send_request(
                "initialize",
                Some(json!({
                    "protocolVersion": "0.1.0",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "shared-test-client",
                        "version": "1.0.0"
                    }
                })),
            )
            .await?;

        eprintln!(
            "[SharedMCPServer] Initialized: {}",
            response.get("serverInfo").is_some()
        );
        Ok(())
    }

    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);

        let mut request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method
        });

        if let Some(params) = params {
            request["params"] = params;
        }

        // Send request
        let request_str = serde_json::to_string(&request)?;
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(request_str.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }

        // Read response
        let response_line = {
            let mut line = String::new();
            let mut stdout = self.stdout.lock().await;
            stdout.read_line(&mut line).await?;
            line
        };

        let response: Value = serde_json::from_str(&response_line)?;

        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("MCP error: {}", error));
        }

        Ok(response.get("result").cloned().unwrap_or(json!(null)))
    }

    fn add_client(&self) {
        let count = self.client_count.fetch_add(1, Ordering::SeqCst) + 1;
        eprintln!("[SharedMCPServer] Client added, total: {}", count);
    }

    fn remove_client(&self) -> u64 {
        let count = self.client_count.fetch_sub(1, Ordering::SeqCst) - 1;
        eprintln!("[SharedMCPServer] Client removed, remaining: {}", count);
        count
    }
}

impl Drop for SharedMCPServer {
    fn drop(&mut self) {
        eprintln!("[SharedMCPServer] Dropping server instance");
        // Process cleanup will be handled by tokio Child's Drop
    }
}

/// A client that connects to a shared MCP server instance.
pub struct SharedMCPClient {
    server: Arc<SharedMCPServer>,
    project_type: String,
}

impl SharedMCPClient {
    /// Get or create a shared client for the test project.
    pub async fn get_or_create(project_type: &str) -> Result<Self> {
        let workspace_path = match project_type {
            "test-project" => {
                let manifest_dir =
                    std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
                Path::new(&manifest_dir).join("test-project")
            }
            "test-project-diagnostics" => {
                let manifest_dir =
                    std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
                Path::new(&manifest_dir).join("test-project-diagnostics")
            }
            _ => return Err(anyhow::anyhow!("Unknown project type: {}", project_type)),
        };

        // Check if server already exists
        let server = {
            let servers = SHARED_SERVERS.read().await;
            servers.get(project_type).cloned()
        };

        let server = match server {
            Some(s) => {
                eprintln!(
                    "[SharedMCPClient] Reusing existing server for {}",
                    project_type
                );
                s
            }
            None => {
                eprintln!("[SharedMCPClient] Creating new server for {}", project_type);
                let mut servers = SHARED_SERVERS.write().await;

                // Double-check in case another thread created it
                if let Some(s) = servers.get(project_type) {
                    s.clone()
                } else {
                    let new_server = SharedMCPServer::new(workspace_path, project_type).await?;
                    servers.insert(project_type.to_string(), new_server.clone());
                    new_server
                }
            }
        };

        server.add_client();

        Ok(Self {
            server,
            project_type: project_type.to_string(),
        })
    }

    /// Send a request to the shared server.
    pub async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        self.server.send_request(method, params).await
    }

    /// Call a tool on the shared server.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        self.send_request(
            "tools/call",
            Some(json!({
                "name": name,
                "arguments": arguments
            })),
        )
        .await
    }
}

impl Drop for SharedMCPClient {
    fn drop(&mut self) {
        let remaining = self.server.remove_client();

        // If this was the last client, remove the server from the pool
        if remaining == 0 {
            eprintln!(
                "[SharedMCPClient] Last client for {}, scheduling server removal",
                self.project_type
            );

            let project_type = self.project_type.clone();
            let servers = SHARED_SERVERS.clone();

            // Schedule cleanup in the background
            tokio::spawn(async move {
                // Give a small grace period in case another test starts immediately
                tokio::time::sleep(Duration::from_millis(100)).await;

                let mut servers = servers.write().await;

                // Check if server still has no clients and remove if so
                let server_to_kill = servers
                    .get(&project_type)
                    .filter(|s| s.client_count.load(Ordering::SeqCst) == 0)
                    .cloned();

                if let Some(server) = server_to_kill {
                    eprintln!("[SharedMCPClient] Removing server for {}", project_type);
                    servers.remove(&project_type);

                    // Kill the process after removing from map
                    if let Some(mut process) = server.process.lock().await.take() {
                        let _ = process.kill().await;
                        let _ = process.wait().await;
                    }
                }
            });
        }
    }
}
