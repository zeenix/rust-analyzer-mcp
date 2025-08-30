use anyhow::Result;
use serde_json::{json, Value};
use std::{
    path::Path,
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
    runtime::Handle,
    sync::Mutex,
    time::timeout,
};

use crate::{timeouts, IsolatedProject};

/// MCP test client for integration testing - properly manages process lifecycle
pub struct MCPTestClient {
    process: Arc<Mutex<Option<Child>>>,
    stdin: Mutex<tokio::process::ChildStdin>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    request_id: AtomicU64,
    shutdown: AtomicBool,
    /// Optional isolated project that will be cleaned up when client is dropped.
    _isolated_project: Option<IsolatedProject>,
}

impl MCPTestClient {
    /// Start a new MCP server process with an isolated test project.
    /// This creates a temporary copy of the test-project that will be cleaned up automatically.
    pub async fn start_isolated() -> Result<Self> {
        let isolated_project = IsolatedProject::new()?;
        let workspace = isolated_project.path().to_path_buf();
        eprintln!("[start_isolated] Using workspace: {:?}", workspace);
        eprintln!("[start_isolated] Process ID: {}", std::process::id());

        let client = Self::start_internal(&workspace, Some(isolated_project)).await?;
        Ok(client)
    }

    /// Start a new MCP server process with an isolated diagnostic test project.
    /// This creates a temporary copy of test-project-diagnostics for testing diagnostics.
    pub async fn start_isolated_diagnostics() -> Result<Self> {
        let isolated_project = IsolatedProject::new_diagnostics()?;
        let workspace = isolated_project.path().to_path_buf();
        eprintln!(
            "[start_isolated_diagnostics] Using workspace: {:?}",
            workspace
        );

        let client = Self::start_internal(&workspace, Some(isolated_project)).await?;
        Ok(client)
    }

    /// Start a new MCP server process with a provided workspace path.
    /// Use this when you don't need isolation (e.g., for benchmarks or specific test setups).
    pub async fn start(workspace: &Path) -> Result<Self> {
        Self::start_internal(workspace, None).await
    }

    /// Internal method to start the MCP server.
    async fn start_internal(
        workspace: &Path,
        isolated_project: Option<IsolatedProject>,
    ) -> Result<Self> {
        // Add a small random delay to avoid races when tests start in parallel
        let delay_ms = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 500) as u64;
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        // Use the built binary directly instead of cargo run for speed and isolation
        // CARGO_MANIFEST_DIR points to the crate being tested (rust-analyzer-mcp)
        // The binary is in rust-analyzer-mcp/target/, not in the parent directory
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let project_root = std::path::Path::new(&manifest_dir);

        let release_binary = project_root.join("target/release/rust-analyzer-mcp");
        let debug_binary = project_root.join("target/debug/rust-analyzer-mcp");

        let binary = if release_binary.exists() {
            release_binary
        } else if debug_binary.exists() {
            debug_binary
        } else {
            // Fall back to cargo run if binary not built
            return Self::start_with_cargo_internal(workspace, isolated_project).await;
        };

        // Generate unique IDs for this test instance
        let unique_id = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let temp_dir = format!("/tmp/rust-analyzer-mcp-test-{}", unique_id);

        // Create the temp directories
        std::fs::create_dir_all(&temp_dir).ok();
        std::fs::create_dir_all(format!("{}/cache", temp_dir)).ok();
        std::fs::create_dir_all(format!("{}/target", temp_dir)).ok();

        // Set environment variables to improve isolation
        let mut process = Command::new(&binary)
            .arg(workspace.to_str().unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Set a unique TMPDIR per test to avoid conflicts
            .env("TMPDIR", &temp_dir)
            // Set rust-analyzer cache to a unique directory
            .env("XDG_CACHE_HOME", format!("{}/cache", temp_dir))
            // Disable any global rust-analyzer config that might interfere
            .env("RUST_ANALYZER_CONFIG", "")
            // Disable cargo target directory sharing
            .env("CARGO_TARGET_DIR", format!("{}/target", temp_dir))
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());
        let stderr = process.stderr.take().unwrap();

        // Spawn a task to consume stderr and log any errors
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut stderr_reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = stderr_reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                if !line.trim().is_empty() {
                    eprintln!("[rust-analyzer-mcp stderr] {}", line.trim());
                }
                line.clear();
            }
        });

        Ok(Self {
            process: Arc::new(Mutex::new(Some(process))),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            request_id: AtomicU64::new(1),
            shutdown: AtomicBool::new(false),
            _isolated_project: isolated_project,
        })
    }

    /// Start using cargo run (fallback).
    async fn start_with_cargo_internal(
        workspace: &Path,
        isolated_project: Option<IsolatedProject>,
    ) -> Result<Self> {
        // Generate unique IDs for this test instance
        let unique_id = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let temp_dir = format!("/tmp/rust-analyzer-mcp-test-{}", unique_id);

        // Create the temp directories
        std::fs::create_dir_all(&temp_dir).ok();
        std::fs::create_dir_all(format!("{}/cache", temp_dir)).ok();
        std::fs::create_dir_all(format!("{}/target", temp_dir)).ok();

        let mut process = Command::new("cargo")
            .args(&["run", "--", workspace.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Set environment variables to improve isolation
            .env("TMPDIR", &temp_dir)
            // Set rust-analyzer cache to a unique directory
            .env("XDG_CACHE_HOME", format!("{}/cache", temp_dir))
            .env("RUST_ANALYZER_CONFIG", "")
            // Disable cargo target directory sharing
            .env("CARGO_TARGET_DIR", format!("{}/target", temp_dir))
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());
        let stderr = process.stderr.take().unwrap();

        // Spawn a task to consume stderr and log any errors
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut stderr_reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = stderr_reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                if !line.trim().is_empty() {
                    eprintln!("[rust-analyzer-mcp stderr] {}", line.trim());
                }
                line.clear();
            }
        });

        Ok(Self {
            process: Arc::new(Mutex::new(Some(process))),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            request_id: AtomicU64::new(1),
            shutdown: AtomicBool::new(false),
            _isolated_project: isolated_project,
        })
    }

    /// Explicitly shut down the client and its process.
    pub async fn shutdown(&self) -> Result<()> {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            // Already shutdown
            return Ok(());
        }

        let mut process_lock = self.process.lock().await;
        if let Some(mut process) = process_lock.take() {
            // Try graceful shutdown first
            let _ = process.kill().await;
            // Wait for process to actually exit
            let _ = process.wait().await;
        }
        Ok(())
    }

    /// Send a request and wait for response with timeout
    pub async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        self.send_request_with_timeout(method, params, timeouts::request())
            .await
    }

    /// Send a request with custom timeout
    pub async fn send_request_with_timeout(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_duration: Duration,
    ) -> Result<Value> {
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

        // Read response with timeout
        let response_line = timeout(timeout_duration, async {
            let mut line = String::new();
            let mut stdout = self.stdout.lock().await;
            stdout.read_line(&mut line).await?;
            Ok::<String, anyhow::Error>(line)
        })
        .await
        .map_err(|_| anyhow::anyhow!("Request timeout after {:?}", timeout_duration))??;

        let response: Value = serde_json::from_str(&response_line)?;

        // Check for errors
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("MCP error: {}", error));
        }

        Ok(response.get("result").cloned().unwrap_or(json!(null)))
    }

    /// Initialize the MCP server
    pub async fn initialize(&self) -> Result<Value> {
        self.send_request(
            "initialize",
            Some(json!({
                "protocolVersion": "0.1.0",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            })),
        )
        .await
    }

    /// Initialize and wait for rust-analyzer to be ready.
    /// For isolated clients, uses the internal workspace path automatically.
    pub async fn initialize_and_wait(&self) -> Result<()> {
        self.initialize().await?;

        // rust-analyzer returns null while indexing, so we need to poll.
        let start = std::time::Instant::now();
        let timeout = timeouts::init_wait();
        let poll_interval = timeouts::init_poll();

        loop {
            if start.elapsed() > timeout {
                eprintln!(
                    "Timeout after {:?}. Symbols ready: {}",
                    start.elapsed(),
                    self.check_symbols_ready().await
                );
                return Err(anyhow::anyhow!(
                    "Timeout waiting for rust-analyzer to be ready after {:?}",
                    timeout
                ));
            }

            // Check if symbols are ready - this is the most reliable indicator
            let symbols_ready = self.check_symbols_ready().await;

            if symbols_ready {
                // Give it more time to ensure all features are ready, especially in CI
                tokio::time::sleep(timeouts::init_extra_delay()).await;
                return Ok(());
            }

            // Not ready yet, wait before retrying
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Enhanced initialization that ensures the workspace is fully ready.
    /// This includes verifying that all module imports are resolved and diagnostics are stable.
    pub async fn initialize_workspace(&self) -> Result<()> {
        let readiness = crate::WorkspaceReadiness::new(self);
        readiness.ensure_ready().await
    }

    /// Enhanced initialization with custom critical files to verify.
    pub async fn initialize_workspace_with_files(&self, files: Vec<String>) -> Result<()> {
        let readiness = crate::WorkspaceReadiness::with_files(self, files);
        readiness.ensure_ready().await
    }

    async fn check_symbols_ready(&self) -> bool {
        // Use lib.rs as it exists in all test projects
        let Ok(response) = self
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/lib.rs"}))
            .await
        else {
            return false;
        };

        let Some(content) = response.get("content") else {
            return false;
        };

        let Some(text) = content[0].get("text") else {
            return false;
        };

        let Some(text_str) = text.as_str() else {
            return false;
        };

        // Check if we got null or empty response
        if text_str == "null" || text_str == "[]" {
            return false;
        }

        // Try to parse symbols
        let Ok(symbols) = serde_json::from_str::<Vec<Value>>(text_str) else {
            return false;
        };

        !symbols.is_empty()
    }

    /// Call a tool
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let timeout = timeouts::tool_call();

        // In CI, add retry logic for transient failures
        if crate::is_ci() {
            let mut last_error = None;
            for attempt in 1..=3 {
                match self
                    .send_request_with_timeout(
                        "tools/call",
                        Some(json!({
                            "name": name,
                            "arguments": arguments.clone()
                        })),
                        timeout,
                    )
                    .await
                {
                    Ok(response) => return Ok(response),
                    Err(e) => {
                        last_error = Some(e);
                        if attempt < 3 {
                            eprintln!(
                                "Tool call attempt {} failed, retrying: {:?}",
                                attempt, last_error
                            );
                            tokio::time::sleep(timeouts::tool_retry_delay()).await;
                        }
                    }
                }
            }
            Err(last_error.unwrap())
        } else {
            self.send_request_with_timeout(
                "tools/call",
                Some(json!({
                    "name": name,
                    "arguments": arguments
                })),
                timeout,
            )
            .await
        }
    }

    /// Call a tool with custom timeout
    pub async fn call_tool_with_timeout(
        &self,
        name: &str,
        arguments: Value,
        timeout_duration: Duration,
    ) -> Result<Value> {
        self.send_request_with_timeout(
            "tools/call",
            Some(json!({
                "name": name,
                "arguments": arguments
            })),
            timeout_duration,
        )
        .await
    }

    /// Set workspace
    pub async fn set_workspace(&self, workspace: &Path) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_set_workspace",
            json!({
                "workspace_path": workspace.to_str().unwrap()
            }),
        )
        .await
    }

    /// Get symbols for a file
    pub async fn get_symbols(&self, file_path: &str) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_symbols",
            json!({
                "file_path": file_path
            }),
        )
        .await
    }

    /// Get definition at position
    pub async fn get_definition(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_definition",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
        )
        .await
    }

    /// Get references at position
    pub async fn get_references(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_references",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
        )
        .await
    }

    /// Get hover information at position
    pub async fn get_hover(&self, file_path: &str, line: u32, character: u32) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_hover",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
        )
        .await
    }

    /// Get completions at position
    pub async fn get_completion(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_completion",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
        )
        .await
    }

    /// Format a file
    pub async fn format(&self, file_path: &str) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_format",
            json!({
                "file_path": file_path
            }),
        )
        .await
    }
}

impl Drop for MCPTestClient {
    fn drop(&mut self) {
        if self.shutdown.load(Ordering::SeqCst) {
            // Already shutdown explicitly
            return;
        }

        // Try to kill the process synchronously if possible
        if let Ok(handle) = Handle::try_current() {
            let process = Arc::clone(&self.process);
            // Spawn a task to ensure cleanup happens
            handle.spawn(async move {
                if let Some(mut process) = process.lock().await.take() {
                    // Kill the process
                    let _ = process.kill().await;
                    // Wait for it to actually exit to avoid zombies
                    let _ = process.wait().await;
                }
            });

            // Give the cleanup task a moment to run
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
