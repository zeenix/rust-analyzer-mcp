use anyhow::Result;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

use super::server::socket_path;

/// Client that connects to the IPC MCP server
pub struct IpcClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    request_id: AtomicU64,
    workspace_path: PathBuf,
}

impl IpcClient {
    /// Connect to or start an IPC MCP server
    pub async fn get_or_create(project_type: &str) -> Result<Self> {
        // Map project types to workspace paths
        let workspace_path = match project_type {
            "test-project" | "test-project-singleton" | "test-project-concurrent" => {
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

        // Canonicalize the workspace path to ensure it's absolute
        let workspace_path = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.clone());

        let sock_path = socket_path(project_type);

        // Try to connect to existing server
        if let Ok(stream) = UnixStream::connect(&sock_path) {
            eprintln!("Connected to existing MCP server for {}", project_type);
            let reader = BufReader::new(stream.try_clone()?);
            return Ok(Self {
                stream,
                reader,
                request_id: AtomicU64::new(1),
                workspace_path,
            });
        }

        // Server not running, start it
        eprintln!("Starting new MCP server for {}", project_type);

        // Always build the server - cargo will handle locking and skip if already built
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let project_root = Path::new(&manifest_dir);

        eprintln!("Ensuring test-support-server is built...");

        // Determine build mode based on current profile
        let (build_args, binary_path) = if cfg!(debug_assertions) {
            (
                vec![
                    "build",
                    "-p",
                    "test-support",
                    "--bin",
                    "test-support-server",
                ],
                project_root.join("target/debug/test-support-server"),
            )
        } else {
            (
                vec![
                    "build",
                    "--release",
                    "-p",
                    "test-support",
                    "--bin",
                    "test-support-server",
                ],
                project_root.join("target/release/test-support-server"),
            )
        };

        let output = Command::new("cargo")
            .current_dir(project_root)
            .args(&build_args)
            .output()?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Failed to build test-support-server: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let binary = binary_path;

        // Start the server in background
        Command::new(&binary)
            .arg("--workspace")
            .arg(workspace_path.to_str().unwrap())
            .arg("--project-type")
            .arg(project_type)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        // Wait for server to start
        let mut attempts = 0;
        loop {
            if let Ok(stream) = UnixStream::connect(&sock_path) {
                eprintln!("Connected to new MCP server for {}", project_type);
                let reader = BufReader::new(stream.try_clone()?);
                return Ok(Self {
                    stream,
                    reader,
                    request_id: AtomicU64::new(1),
                    workspace_path,
                });
            }

            attempts += 1;
            if attempts > 50 {
                return Err(anyhow::anyhow!(
                    "Failed to connect to MCP server after starting"
                ));
            }

            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Send a request to the server
    pub async fn send_request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        // Send request
        let request_str = serde_json::to_string(&request)?;
        self.stream.write_all(request_str.as_bytes())?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()?;

        // Read response
        let mut line = String::new();
        let bytes_read = self.reader.read_line(&mut line)?;
        if bytes_read == 0 {
            return Err(anyhow::anyhow!("Server disconnected"));
        }

        let response: Value = serde_json::from_str(&line)?;

        // Extract result or error
        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!("MCP error: {}", error));
        }

        Ok(response.get("result").cloned().unwrap_or(json!(null)))
    }

    /// Call a tool on the server
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.send_request(
            "tools/call",
            Some(json!({
                "name": name,
                "arguments": arguments
            })),
        )
        .await
    }

    /// Get the workspace path
    pub fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }
}

impl Drop for IpcClient {
    fn drop(&mut self) {
        // Just disconnect, server will auto-shutdown after 15 seconds
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}
