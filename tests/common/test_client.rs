use anyhow::Result;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
    time::timeout,
};

/// MCP test client for integration testing - fully async
pub struct MCPTestClient {
    process: Option<Child>,
    stdin: Mutex<tokio::process::ChildStdin>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    request_id: AtomicU64,
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
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());

        Ok(Self {
            process: Some(process),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            request_id: AtomicU64::new(1),
        })
    }

    /// Start using cargo run (fallback)
    async fn start_with_cargo(workspace: &Path) -> Result<Self> {
        let mut process = Command::new("cargo")
            .args(&["run", "--", workspace.to_str().unwrap()])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());

        Ok(Self {
            process: Some(process),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            request_id: AtomicU64::new(1),
        })
    }

    /// Start with a specific binary path
    pub async fn start_with_binary(binary: &Path, workspace: &Path) -> Result<Self> {
        let mut process = Command::new(binary)
            .arg(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());

        Ok(Self {
            process: Some(process),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            request_id: AtomicU64::new(1),
        })
    }

    /// Send a request and wait for response with timeout
    pub async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        self.send_request_with_timeout(method, params, Duration::from_secs(10))
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

    /// Send a notification (no response expected)
    pub async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<()> {
        let mut notification = json!({
            "jsonrpc": "2.0",
            "method": method
        });

        if let Some(params) = params {
            notification["params"] = params;
        }

        let notification_str = serde_json::to_string(&notification)?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(notification_str.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        Ok(())
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
        // Use small polling intervals to minimize waiting.
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(30);
        let poll_interval = Duration::from_millis(50); // Small interval to minimize wait

        loop {
            if start.elapsed() > timeout {
                return Err(anyhow::anyhow!(
                    "Timeout waiting for rust-analyzer to be ready after 30 seconds"
                ));
            }

            // Try to get symbols
            let symbols_response = self
                .call_tool("rust_analyzer_symbols", json!({"file_path": "src/main.rs"}))
                .await;

            // Check if we got valid symbols (not null or empty)
            if let Ok(response) = symbols_response {
                if let Some(content) = response.get("content") {
                    if let Some(text) = content[0].get("text") {
                        if text.as_str() != Some("null") && text.as_str() != Some("[]") {
                            if let Ok(symbols) =
                                serde_json::from_str::<Vec<Value>>(text.as_str().unwrap_or("[]"))
                            {
                                if !symbols.is_empty() {
                                    // rust-analyzer is ready
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }

            // Not ready yet, wait a small interval before retrying
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Call a tool
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

    /// Get symbols with retry for initialization
    pub async fn get_symbols_with_retry(&self, file_path: &str) -> Result<Value> {
        // Just delegate to get_symbols since rust-analyzer should already be initialized
        self.get_symbols(file_path).await
    }

    /// Get definition at position
    pub async fn get_definition(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        // Try with longer timeout for definition requests
        self.call_tool_with_timeout(
            "rust_analyzer_definition",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
            Duration::from_secs(15),
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
        // Try with longer timeout for references requests
        self.call_tool_with_timeout(
            "rust_analyzer_references",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
            Duration::from_secs(15),
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
        // Try with longer timeout for format requests
        self.call_tool_with_timeout(
            "rust_analyzer_format",
            json!({
                "file_path": file_path
            }),
            Duration::from_secs(15),
        )
        .await
    }

    /// Shutdown the server
    pub async fn shutdown(&self) -> Result<()> {
        self.send_notification("shutdown", None).await?;
        Ok(())
    }
}

impl Drop for MCPTestClient {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
        }
    }
}
