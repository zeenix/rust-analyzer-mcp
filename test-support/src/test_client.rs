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

use crate::timeouts;

/// MCP test client for integration testing - properly manages process lifecycle
pub struct MCPTestClient {
    process: Arc<Mutex<Option<Child>>>,
    stdin: Mutex<tokio::process::ChildStdin>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    request_id: AtomicU64,
    shutdown: AtomicBool,
}

impl MCPTestClient {
    /// Start a new MCP server process
    pub async fn start(workspace: &Path) -> Result<Self> {
        // Use the built binary directly instead of cargo run for speed and isolation
        let binary = if std::path::Path::new("target/release/rust-analyzer-mcp").exists() {
            "target/release/rust-analyzer-mcp"
        } else if std::path::Path::new("target/debug/rust-analyzer-mcp").exists() {
            "target/debug/rust-analyzer-mcp"
        } else {
            // Fall back to cargo run if binary not built
            return Self::start_with_cargo(workspace).await;
        };

        let mut process = Command::new(binary)
            .arg(workspace.to_str().unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());

        Ok(Self {
            process: Arc::new(Mutex::new(Some(process))),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            request_id: AtomicU64::new(1),
            shutdown: AtomicBool::new(false),
        })
    }

    /// Start using cargo run (fallback)
    async fn start_with_cargo(workspace: &Path) -> Result<Self> {
        let mut process = Command::new("cargo")
            .args(&["run", "--", workspace.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());

        Ok(Self {
            process: Arc::new(Mutex::new(Some(process))),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            request_id: AtomicU64::new(1),
            shutdown: AtomicBool::new(false),
        })
    }

    /// Explicitly shut down the client and its process
    pub async fn shutdown(&self) -> Result<()> {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            // Already shutdown
            return Ok(());
        }

        let mut process_lock = self.process.lock().await;
        if let Some(mut process) = process_lock.take() {
            // Try graceful shutdown first
            let _ = process.kill().await;
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

    /// Initialize and wait for rust-analyzer to be ready
    pub async fn initialize_and_wait(&self, _workspace: &Path) -> Result<()> {
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

    async fn check_symbols_ready(&self) -> bool {
        let Ok(response) = self
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/main.rs"}))
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

        // We must be in a Tokio context for this Drop to be called
        // If we're not, the test framework has a bug
        if let Ok(handle) = Handle::try_current() {
            // Schedule async cleanup
            let process = Arc::clone(&self.process);
            handle.spawn(async move {
                if let Some(mut process) = process.lock().await.take() {
                    let _ = process.kill().await;
                }
            });
        }
    }
}
