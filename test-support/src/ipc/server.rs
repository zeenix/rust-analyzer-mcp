use anyhow::Result;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// Shared handle to the MCP server's stdio, keeping each request write paired with its
/// response read.
type SharedPipes = Arc<Mutex<(ChildStdin, BufReader<ChildStdout>)>>;

/// Start a standalone MCP server process that listens on Unix socket
pub fn start_server(workspace_path: &Path, project_type: &str) -> Result<()> {
    let socket_path = socket_path(project_type);

    // Bind before the expensive rust-analyzer startup: the bound socket is what arbitrates
    // between daemons racing to serve the same project type. Losers exit quietly, and clients'
    // connect attempts simply queue in the backlog until the accept loop starts.
    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(&socket_path).is_ok() {
                // Another daemon already serves this project type.
                return Ok(());
            }
            // Stale socket left behind by a dead daemon.
            fs::remove_file(&socket_path)?;
            UnixListener::bind(&socket_path)?
        }
        Err(e) => return Err(e.into()),
    };
    eprintln!("MCP server listening on {:?}", socket_path);

    // Start rust-analyzer process
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let project_root = Path::new(&manifest_dir);

    let release_binary = project_root.join("target/release/rust-analyzer-mcp");
    let debug_binary = project_root.join("target/debug/rust-analyzer-mcp");

    let binary = if release_binary.exists() {
        release_binary
    } else if debug_binary.exists() {
        debug_binary
    } else {
        return Err(anyhow::anyhow!("rust-analyzer-mcp binary not found"));
    };

    // Isolate cargo state per project type: several daemons (and other tests) share the same
    // workspace, and serializing on one target-directory lock starves them into LSP timeouts on
    // slow CI runners.
    let isolation_dir = std::env::temp_dir().join(format!("rust-analyzer-mcp-ipc-{project_type}"));
    fs::create_dir_all(isolation_dir.join("target"))?;
    fs::create_dir_all(isolation_dir.join("cache"))?;

    let mut rust_analyzer = Command::new(&binary)
        .arg(workspace_path.to_str().unwrap())
        .env("CARGO_TARGET_DIR", isolation_dir.join("target"))
        .env("XDG_CACHE_HOME", isolation_dir.join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Keep the MCP server's logs in this daemon's log file for post-mortem debugging.
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin = rust_analyzer.stdin.take().unwrap();
    let mut stdout = BufReader::new(rust_analyzer.stdout.take().unwrap());

    // Initialize rust-analyzer
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "0.1.0",
            "capabilities": {},
            "clientInfo": {
                "name": "ipc-server",
                "version": "1.0.0"
            }
        }
    });

    let request_str = serde_json::to_string(&request)?;
    stdin.write_all(request_str.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;

    let mut line = String::new();
    stdout.read_line(&mut line)?;

    let response: Value = serde_json::from_str(&line)?;
    if response.get("error").is_some() {
        return Err(anyhow::anyhow!("Failed to initialize: {:?}", response));
    }

    // Wait for rust-analyzer to be ready
    wait_for_ready(&mut stdin, &mut stdout, workspace_path)?;

    let last_activity = Arc::new(Mutex::new(Instant::now()));
    let shutdown = Arc::new(AtomicBool::new(false));
    let request_id = Arc::new(AtomicU64::new(100)); // Start at 100 to avoid conflicts
    let pipes = Arc::new(Mutex::new((stdin, stdout)));
    let active_clients = Arc::new(AtomicUsize::new(0));

    // Spawn idle timeout checker. Only shut down while no client is connected, so a slow
    // in-flight request or a briefly quiet client cannot get the server killed under them.
    let timeout_activity = last_activity.clone();
    let timeout_shutdown = shutdown.clone();
    let timeout_active = active_clients.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));

        if timeout_shutdown.load(Ordering::SeqCst) {
            break;
        }

        let last = timeout_activity.lock().unwrap();
        if last.elapsed() > Duration::from_secs(15) && timeout_active.load(Ordering::SeqCst) == 0 {
            eprintln!("Server idle for 15 seconds, shutting down");
            timeout_shutdown.store(true, Ordering::SeqCst);
            break;
        }
    });

    // Main server loop
    while !shutdown.load(Ordering::SeqCst) {
        // Set timeout for accept to check shutdown periodically
        listener.set_nonblocking(true)?;

        match listener.accept() {
            Ok((stream, _)) => {
                // Update last activity
                *last_activity.lock().unwrap() = Instant::now();

                // Handle each client on its own thread: a client keeps its connection open for
                // as long as it lives, so serving it inline would starve every later client.
                let pipes = Arc::clone(&pipes);
                let last_activity = Arc::clone(&last_activity);
                let request_id = Arc::clone(&request_id);
                let shutdown = Arc::clone(&shutdown);
                let active_clients = Arc::clone(&active_clients);
                active_clients.fetch_add(1, Ordering::SeqCst);
                thread::spawn(move || {
                    if let Err(e) =
                        handle_client(stream, pipes, last_activity, request_id, &shutdown)
                    {
                        eprintln!("Client connection error: {}", e);
                    }
                    active_clients.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No connection, check if we should shutdown
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                eprintln!("Accept error: {}", e);
                break;
            }
        }
    }

    // Cleanup. Remove the socket first so a retrying client spawns a fresh daemon instead of
    // reaching this dying one.
    let _ = fs::remove_file(&socket_path);
    let _ = rust_analyzer.kill();
    eprintln!("MCP server shutdown");

    Ok(())
}

/// Serve one client connection until it disconnects.
fn handle_client(
    mut stream: UnixStream,
    pipes: SharedPipes,
    last_activity: Arc<Mutex<Instant>>,
    request_id: Arc<AtomicU64>,
    shutdown: &AtomicBool,
) -> Result<()> {
    let mut stream_reader = BufReader::new(stream.try_clone()?);

    loop {
        let mut request_line = String::new();
        let bytes = stream_reader.read_line(&mut request_line)?;

        if bytes == 0 {
            return Ok(()); // Client disconnected.
        }

        // Update last activity.
        *last_activity.lock().unwrap() = Instant::now();

        // Parse request.
        let request: Value = serde_json::from_str(&request_line)?;

        // Refuse requests the MCP server would not answer, instead of wedging the shared pipe
        // waiting for a response that never comes.
        if !request["method"].is_string() {
            let error = json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": { "code": -32600, "message": "Invalid request: missing method" }
            });
            stream.write_all(serde_json::to_string(&error)?.as_bytes())?;
            stream.write_all(b"\n")?;
            stream.flush()?;
            continue;
        }

        // Forward to rust-analyzer.
        let id = request_id.fetch_add(1, Ordering::SeqCst);
        let mut forward_request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": request["method"],
        });

        if let Some(params) = request.get("params") {
            forward_request["params"] = params.clone();
        }

        let forward_str = serde_json::to_string(&forward_request)?;

        // All clients share the single rust-analyzer-mcp pipe, so the request write and the
        // response read must stay paired under one critical section.
        let mut response_line = String::new();
        let forwarded = {
            let mut pipes = pipes.lock().unwrap();
            let (stdin, stdout) = &mut *pipes;
            stdin
                .write_all(forward_str.as_bytes())
                .and_then(|_| stdin.write_all(b"\n"))
                .and_then(|_| stdin.flush())
                .and_then(|_| stdout.read_line(&mut response_line))
        };

        // A pipe failure or EOF means the MCP server is gone; shut the whole IPC server down so
        // it cannot linger as a zombie accepting clients it can no longer serve.
        match forwarded {
            Ok(0) => {
                shutdown.store(true, Ordering::SeqCst);
                anyhow::bail!("MCP server closed its stdout");
            }
            Err(e) => {
                shutdown.store(true, Ordering::SeqCst);
                return Err(anyhow::anyhow!("MCP server pipe error: {}", e));
            }
            Ok(_) => {}
        }

        // An unparseable frame means the shared pipe is corrupt; every client on it would be
        // affected, so retire the whole daemon rather than just this connection.
        let mut response: Value = match serde_json::from_str(&response_line) {
            Ok(response) => response,
            Err(e) => {
                shutdown.store(true, Ordering::SeqCst);
                anyhow::bail!("Unparseable MCP response on the shared pipe: {e}");
            }
        };

        // Refuse to relabel a frame that is not the response to the request just forwarded: a
        // stale or unsolicited frame means the pipe is desynced, and relabeling it would hand
        // this client (and everyone after it) someone else's answers.
        if response["id"] != json!(id) {
            shutdown.store(true, Ordering::SeqCst);
            anyhow::bail!(
                "MCP response id {} does not match forwarded id {}; shutting down desynced pipe",
                response["id"],
                id
            );
        }

        // Forward the response to the client with its original request id restored: the daemon
        // rewrites ids on the shared pipe, and the client correlates responses by the id it
        // sent.
        response["id"] = request["id"].clone();
        stream.write_all(serde_json::to_string(&response)?.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        // Update activity.
        *last_activity.lock().unwrap() = Instant::now();
    }
}

fn wait_for_ready(
    stdin: &mut std::process::ChildStdin,
    stdout: &mut BufReader<std::process::ChildStdout>,
    workspace_path: &Path,
) -> Result<()> {
    let test_file = workspace_path.join("src/lib.rs");
    if !test_file.exists() {
        return Ok(());
    }

    let start = Instant::now();
    // Cold rust-analyzer starts routinely exceed 10 seconds in CI, especially with several
    // instances contending for the same target directory.
    let timeout = Duration::from_secs(60);
    let mut request_id = 10;

    loop {
        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for rust-analyzer"));
        }

        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {
                "name": "rust_analyzer_symbols",
                "arguments": {
                    "file_path": test_file.to_str().unwrap()
                }
            }
        });

        request_id += 1;

        let request_str = serde_json::to_string(&request)?;
        stdin.write_all(request_str.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;

        let mut line = String::new();
        stdout.read_line(&mut line)?;

        let response: Value = serde_json::from_str(&line)?;

        // Check if we have a valid response with non-null content.
        let Some(result) = response.get("result") else {
            thread::sleep(Duration::from_millis(200));
            continue;
        };

        let Some(content) = result.get("content") else {
            thread::sleep(Duration::from_millis(200));
            continue;
        };

        let Some(array) = content.as_array() else {
            thread::sleep(Duration::from_millis(200));
            continue;
        };

        if array.is_empty() {
            thread::sleep(Duration::from_millis(200));
            continue;
        }

        let Some(text) = array[0].get("text") else {
            thread::sleep(Duration::from_millis(200));
            continue;
        };

        if text.as_str() != Some("null") {
            break;
        }

        thread::sleep(Duration::from_millis(200));
    }

    // Give it more time to stabilize
    thread::sleep(Duration::from_secs(1));
    Ok(())
}

pub fn socket_path(project_type: &str) -> PathBuf {
    let socket_dir = std::env::temp_dir().join("rust-analyzer-mcp-sockets");
    let _ = fs::create_dir_all(&socket_dir);
    socket_dir.join(format!("{}.sock", project_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, process::Child};

    /// Spawn a shell standing in for the MCP server on the shared pipe: `script` reads request
    /// lines on stdin and writes response lines on stdout.
    fn fake_mcp_server(script: &str) -> (Child, SharedPipes) {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        (child, Arc::new(Mutex::new((stdin, stdout))))
    }

    /// Run one request through `handle_client` against the given fake MCP server script and
    /// return the handler's result, the shutdown flag, and what the client received.
    fn forward_one_request(script: &str) -> (Result<()>, bool, String) {
        let (mut child, pipes) = fake_mcp_server(script);
        let (daemon_side, client_side) = UnixStream::pair().unwrap();

        // Write the request up front and close the write half so the handler sees a client
        // that sends one request and disconnects.
        (&client_side)
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{}}\n")
            .unwrap();
        client_side.shutdown(std::net::Shutdown::Write).unwrap();

        let shutdown = AtomicBool::new(false);
        let result = handle_client(
            daemon_side,
            pipes,
            Arc::new(Mutex::new(Instant::now())),
            Arc::new(AtomicU64::new(100)),
            &shutdown,
        );

        let mut received = String::new();
        BufReader::new(client_side)
            .read_to_string(&mut received)
            .unwrap();

        let _ = child.kill();
        let _ = child.wait();

        (result, shutdown.load(Ordering::SeqCst), received)
    }

    #[test]
    fn restores_client_id_on_forwarded_response() {
        // The echo server answers with the forwarded request itself, so the response carries
        // the daemon's rewritten id; the client must get its own id back.
        let (result, shutdown, received) =
            forward_one_request(r#"while IFS= read -r line; do printf '%s\n' "$line"; done"#);

        result.unwrap();
        assert!(!shutdown);
        let response: Value = serde_json::from_str(&received).unwrap();
        assert_eq!(response["id"], json!(7));
    }

    #[test]
    fn shuts_down_on_desynced_response_id() {
        // A frame that is not the response to the forwarded request must not be relabeled as
        // one; the daemon has to treat the pipe as desynced and retire itself.
        let (result, shutdown, received) = forward_one_request(
            r#"while IFS= read -r line; do printf '{"jsonrpc":"2.0","id":424242,"result":null}\n'; done"#,
        );

        let error = result.unwrap_err();
        assert!(error.to_string().contains("does not match forwarded id"));
        assert!(shutdown);
        assert!(received.is_empty());
    }

    #[test]
    fn shuts_down_on_unparseable_response() {
        let (result, shutdown, received) =
            forward_one_request(r#"while IFS= read -r line; do printf 'not json\n'; done"#);

        assert!(result.is_err());
        assert!(shutdown);
        assert!(received.is_empty());
    }
}
