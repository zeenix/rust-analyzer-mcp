use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// MCP test client for integration testing
pub struct MCPTestClient {
    process: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    request_id: AtomicU64,
}

impl MCPTestClient {
    /// Start a new MCP server process
    pub fn start(workspace: &Path) -> Result<Self> {
        let mut process = Command::new("cargo")
            .args(&["run", "--", workspace.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());

        Ok(Self {
            process,
            stdin,
            stdout,
            request_id: AtomicU64::new(1),
        })
    }

    /// Start with a specific binary path
    pub fn start_with_binary(binary: &Path, workspace: &Path) -> Result<Self> {
        let mut process = Command::new(binary)
            .arg(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());

        Ok(Self {
            process,
            stdin,
            stdout,
            request_id: AtomicU64::new(1),
        })
    }

    /// Send a request and wait for response
    pub fn send_request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
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
        writeln!(self.stdin, "{}", request_str)?;
        self.stdin.flush()?;

        // Read response
        let mut response_line = String::new();
        self.stdout.read_line(&mut response_line)?;

        let response: Value = serde_json::from_str(&response_line)?;

        // Check for errors
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("MCP error: {}", error));
        }

        Ok(response.get("result").cloned().unwrap_or(json!(null)))
    }

    /// Send a notification (no response expected)
    pub fn send_notification(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let mut notification = json!({
            "jsonrpc": "2.0",
            "method": method
        });

        if let Some(params) = params {
            notification["params"] = params;
        }

        let notification_str = serde_json::to_string(&notification)?;
        writeln!(self.stdin, "{}", notification_str)?;
        self.stdin.flush()?;

        Ok(())
    }

    /// Initialize the MCP server
    pub fn initialize(&mut self) -> Result<Value> {
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
    }

    /// Call a tool
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.send_request(
            "tools/call",
            Some(json!({
                "name": name,
                "arguments": arguments
            })),
        )
    }

    /// Set workspace
    pub fn set_workspace(&mut self, workspace: &Path) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_set_workspace",
            json!({
                "workspace_path": workspace.to_str().unwrap()
            }),
        )
    }

    /// Get symbols for a file
    pub fn get_symbols(&mut self, file_path: &str) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_symbols",
            json!({
                "file_path": file_path
            }),
        )
    }

    /// Get definition at position
    pub fn get_definition(&mut self, file_path: &str, line: u32, character: u32) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_definition",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
        )
    }

    /// Get references at position
    pub fn get_references(&mut self, file_path: &str, line: u32, character: u32) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_references",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
        )
    }

    /// Get hover information at position
    pub fn get_hover(&mut self, file_path: &str, line: u32, character: u32) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_hover",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
        )
    }

    /// Get completions at position
    pub fn get_completion(&mut self, file_path: &str, line: u32, character: u32) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_completion",
            json!({
                "file_path": file_path,
                "line": line,
                "character": character
            }),
        )
    }

    /// Format a file
    pub fn format(&mut self, file_path: &str) -> Result<Value> {
        self.call_tool(
            "rust_analyzer_format",
            json!({
                "file_path": file_path
            }),
        )
    }

    /// Shutdown the server
    pub fn shutdown(&mut self) -> Result<()> {
        self.send_notification("shutdown", None)?;
        Ok(())
    }
}

impl Drop for MCPTestClient {
    fn drop(&mut self) {
        let _ = self.shutdown();
        let _ = self.process.kill();
    }
}
