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

/// Maximum attempts for a tool call whose previous attempts failed on a transient timeout.
const TOOL_CALL_ATTEMPTS: u32 = 3;

/// Client that connects to the IPC MCP server
pub struct IpcClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    request_id: AtomicU64,
    workspace_path: PathBuf,
}

impl IpcClient {
    /// Connect to a server listening on the given socket path.
    fn connect(sock_path: &Path, workspace_path: PathBuf) -> Result<Self> {
        let stream = UnixStream::connect(sock_path)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            stream,
            reader,
            request_id: AtomicU64::new(1),
            workspace_path,
        })
    }

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

        let sock_path = socket_path(project_type);

        // Try to connect to existing server
        if let Ok(client) = Self::connect(&sock_path, workspace_path.clone()) {
            eprintln!("Connected to existing MCP server for {}", project_type);
            return Ok(client);
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

        // Start the server in background, keeping its stderr in a log file next to the socket
        // for post-mortem debugging. Append so racing daemon generations don't truncate each
        // other's logs.
        let log_path = sock_path.with_extension("log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        Command::new(&binary)
            .arg("--workspace")
            .arg(workspace_path.to_str().unwrap())
            .arg("--project-type")
            .arg(project_type)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log_file))
            .spawn()?;

        // Wait for server to start
        let mut attempts = 0;
        loop {
            if let Ok(client) = Self::connect(&sock_path, workspace_path.clone()) {
                eprintln!("Connected to new MCP server for {}", project_type);
                return Ok(client);
            }

            attempts += 1;
            // The server only binds its socket once rust-analyzer is fully initialized, which
            // can take well over 5 seconds on a cold start, so be patient.
            if attempts > 600 {
                return Err(anyhow::anyhow!(
                    "Failed to connect to MCP server after starting; see {}",
                    log_path.display()
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

        // The connection is lockstep (one request in flight), so a mismatched id means the
        // shared daemon pipe lost sync; failing fast beats returning someone else's answer.
        if response["id"] != json!(id) {
            return Err(anyhow::anyhow!(
                "Response id {} does not match request id {}",
                response["id"],
                id
            ));
        }

        // Extract result or error
        if let Some(error) = response.get("error") {
            return Err(anyhow::Error::new(McpError {
                code: error["code"].as_i64().unwrap_or_default(),
                message: error["message"].as_str().unwrap_or_default().to_string(),
            }));
        }

        Ok(response.get("result").cloned().unwrap_or(json!(null)))
    }

    /// Call a tool on the server
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let params = json!({
            "name": name,
            "arguments": arguments
        });

        // The MCP server bounds every LSP request with its own timeout, which can fire even
        // though rust-analyzer is healthy when a loaded machine (CI especially) starves it of
        // CPU. Such timeouts are transient, so retry a few times before declaring failure.
        let mut attempt = 1;
        loop {
            match self.send_request("tools/call", Some(params.clone())).await {
                Err(e)
                    if attempt < TOOL_CALL_ATTEMPTS
                        && e.downcast_ref::<McpError>()
                            .is_some_and(McpError::is_transient_timeout) =>
                {
                    eprintln!(
                        "Tool call {name} timed out (attempt {attempt}/{TOOL_CALL_ATTEMPTS}); \
                         retrying"
                    );
                    tokio::time::sleep(crate::timeouts::tool_retry_delay() * attempt).await;
                    attempt += 1;
                }
                result => return result,
            }
        }
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

/// A JSON-RPC error response from the MCP server.
#[derive(Debug)]
pub struct McpError {
    pub code: i64,
    pub message: String,
}

impl McpError {
    /// Whether this is the MCP server's LSP request timeout, which is transient under load.
    fn is_transient_timeout(&self) -> bool {
        self.code == -1 && self.message == "Request timeout"
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for McpError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// Spawn a fake daemon that answers each incoming request line with the next canned
    /// response (echoing the request id back unless the response carries its own) until the
    /// client disconnects, then returns the requests it served.
    fn fake_daemon(
        listener: UnixListener,
        responses: Vec<Value>,
    ) -> thread::JoinHandle<Vec<Value>> {
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut served = Vec::new();
            for mut response in responses {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let request: Value = serde_json::from_str(&line).unwrap();
                if response.get("id").is_none() {
                    response["id"] = request["id"].clone();
                }
                writer
                    .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                    .unwrap();
                writer.write_all(b"\n").unwrap();
                writer.flush().unwrap();
                served.push(request);
            }
            served
        })
    }

    fn timeout_error() -> Value {
        json!({
            "jsonrpc": "2.0",
            "error": { "code": -1, "message": "Request timeout", "data": null }
        })
    }

    fn fake_client(
        dir: &Path,
        responses: Vec<Value>,
    ) -> (IpcClient, thread::JoinHandle<Vec<Value>>) {
        let sock_path = dir.join("daemon.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let server = fake_daemon(listener, responses);
        let client = IpcClient::connect(&sock_path, dir.to_path_buf()).unwrap();
        (client, server)
    }

    #[tokio::test]
    async fn call_tool_retries_transient_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let success = json!({
            "jsonrpc": "2.0",
            "result": { "content": [{ "type": "text", "text": "[]" }] }
        });
        let (mut client, server) = fake_client(dir.path(), vec![timeout_error(), success]);

        let response = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/lib.rs"}))
            .await
            .unwrap();
        assert!(response.get("content").is_some());
        drop(client);

        // The retry must be a fresh request with a new id but an unchanged payload.
        let served = server.join().unwrap();
        assert_eq!(served.len(), 2);
        assert_ne!(served[0]["id"], served[1]["id"]);
        assert_eq!(served[0]["method"], served[1]["method"]);
        assert_eq!(served[0]["params"], served[1]["params"]);
    }

    #[tokio::test]
    async fn call_tool_gives_up_after_repeated_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let responses = (0..TOOL_CALL_ATTEMPTS).map(|_| timeout_error()).collect();
        let (mut client, server) = fake_client(dir.path(), responses);

        let error = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/lib.rs"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Request timeout"));
        drop(client);
        assert_eq!(server.join().unwrap().len(), TOOL_CALL_ATTEMPTS as usize);
    }

    #[tokio::test]
    async fn call_tool_does_not_retry_other_errors() {
        let dir = tempfile::tempdir().unwrap();
        let invalid = json!({
            "jsonrpc": "2.0",
            "error": { "code": -32602, "message": "Missing tool name", "data": null }
        });
        let (mut client, server) = fake_client(dir.path(), vec![invalid]);

        let error = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/lib.rs"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Missing tool name"));
        drop(client);
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn send_request_rejects_mismatched_response_id() {
        let dir = tempfile::tempdir().unwrap();
        let desynced = json!({
            "jsonrpc": "2.0",
            "id": 999,
            "result": { "content": [] }
        });
        let (mut client, server) = fake_client(dir.path(), vec![desynced]);

        let error = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/lib.rs"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not match request id"));
        drop(client);
        assert_eq!(server.join().unwrap().len(), 1);
    }
}
