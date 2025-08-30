use anyhow::Result;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    os::unix::fs::DirBuilderExt,
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

// Lock directory for atomic server creation
static LOCK_DIR: Lazy<PathBuf> = Lazy::new(|| {
    let dir = std::env::temp_dir().join("rust-analyzer-mcp-locks");
    let _ = fs::create_dir_all(&dir);
    dir
});

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
        // Use a lock file to ensure atomic creation
        let lock_path = LOCK_DIR.join(format!("{}.lock", project_type));
        let _lock_file = Self::acquire_lock(&lock_path)?;

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

            // Poll until rust-analyzer is ready by checking symbols
            let test_file = server.workspace_path.join("src/lib.rs");
            if test_file.exists() {
                eprintln!("[SharedMCPServer] Polling for rust-analyzer readiness...");
                let start = tokio::time::Instant::now();
                let timeout = Duration::from_secs(10); // Reduced from 30s
                let poll_interval = Duration::from_millis(200); // Reduced from 500ms

                loop {
                    if start.elapsed() > timeout {
                        return Err(anyhow::anyhow!(
                            "Timeout waiting for rust-analyzer to be ready"
                        ));
                    }

                    let response = server
                        .send_request(
                            "tools/call",
                            Some(json!({
                                "name": "rust_analyzer_symbols",
                                "arguments": {
                                    "file_path": test_file.to_str().unwrap()
                                }
                            })),
                        )
                        .await?;

                    // Check if we got a non-null response
                    if let Some(content) = response.get("content") {
                        if let Some(content_array) = content.as_array() {
                            if !content_array.is_empty() {
                                if let Some(first) = content_array.first() {
                                    if let Some(text) = first.get("text") {
                                        if text.as_str() != Some("null") {
                                            eprintln!("[SharedMCPServer] rust-analyzer is ready!");
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    tokio::time::sleep(poll_interval).await;
                }
            }

            server.initialized.store(true, Ordering::SeqCst);
        }

        // Lock file will be automatically released when _lock_file is dropped
        Ok(server)
    }

    fn acquire_lock(lock_path: &Path) -> Result<File> {
        use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};

        // Try to create lock file with O_EXCL (fails if exists)
        let mut retries = 50; // 5 seconds total
        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(lock_path)
            {
                Ok(mut file) => {
                    // Write PID to lock file
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(file);
                }
                Err(_) if retries > 0 => {
                    // Check if lock file is stale (process doesn't exist)
                    if let Ok(contents) = fs::read_to_string(lock_path) {
                        if let Ok(pid) = contents.trim().parse::<u32>() {
                            // Check if process exists using kill(0)
                            unsafe {
                                if libc::kill(pid as i32, 0) != 0 {
                                    // Process doesn't exist, remove stale lock
                                    let _ = fs::remove_file(lock_path);
                                    continue;
                                }
                            }
                        }
                    }

                    retries -= 1;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(anyhow::anyhow!("Failed to acquire lock: {}", e)),
            }
        }
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
            let bytes_read = stdout.read_line(&mut line).await?;
            if bytes_read == 0 {
                return Err(anyhow::anyhow!("Server process died unexpectedly"));
            }
            line
        };

        if response_line.trim().is_empty() {
            return Err(anyhow::anyhow!("Empty response from server"));
        }

        let response: Value = serde_json::from_str(&response_line)
            .map_err(|e| anyhow::anyhow!("Failed to parse response '{}': {}", response_line, e))?;

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
        // Kill the process explicitly
        if let Ok(mut process_guard) = self.process.try_lock() {
            if let Some(mut process) = process_guard.take() {
                let _ = process.start_kill();
                // Can't await in Drop, but start_kill sends SIGKILL
            }
        }
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

        // Use double-checked locking pattern
        {
            let servers = SHARED_SERVERS.read().await;
            if let Some(server) = servers.get(project_type) {
                eprintln!(
                    "[SharedMCPClient] Reusing existing server for {}",
                    project_type
                );
                server.add_client();
                return Ok(Self {
                    server: server.clone(),
                    project_type: project_type.to_string(),
                });
            }
        }

        // Need to create new server - take write lock
        eprintln!("[SharedMCPClient] Creating new server for {}", project_type);
        let mut servers = SHARED_SERVERS.write().await;

        // Double-check after acquiring write lock
        if let Some(server) = servers.get(project_type) {
            server.add_client();
            return Ok(Self {
                server: server.clone(),
                project_type: project_type.to_string(),
            });
        }

        // Create new server (this will use filesystem lock for atomicity)
        let new_server = SharedMCPServer::new(workspace_path, project_type).await?;
        servers.insert(project_type.to_string(), new_server.clone());
        let server = new_server;

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

    /// Get the workspace path for this client.
    pub fn workspace_path(&self) -> &Path {
        &self.server.workspace_path
    }
}

impl Drop for SharedMCPClient {
    fn drop(&mut self) {
        let remaining = self.server.remove_client();
        eprintln!(
            "[SharedMCPClient] Dropped client for {}, {} remaining",
            self.project_type, remaining
        );

        if remaining == 0 {
            // Remove from global pool so next test gets fresh instance
            let project_type = self.project_type.clone();
            let servers = SHARED_SERVERS.clone();

            // Use blocking spawn to avoid runtime issues
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::try_current();
                if let Ok(handle) = rt {
                    handle.block_on(async {
                        let mut servers = servers.write().await;
                        if let Some(server) = servers.get(&project_type) {
                            if server.client_count.load(Ordering::SeqCst) == 0 {
                                eprintln!("[SharedMCPClient] Removing server for {}", project_type);
                                servers.remove(&project_type);
                                // Server's Drop will kill the process
                            }
                        }
                    });
                }

                // Clean up lock file
                let lock_path = LOCK_DIR.join(format!("{}.lock", project_type));
                let _ = fs::remove_file(lock_path);
            });
        }
    }
}
