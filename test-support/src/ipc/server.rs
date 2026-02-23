use anyhow::Result;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// Start a standalone MCP server process that listens on Unix socket
pub fn start_server(workspace_path: &Path, project_type: &str) -> Result<()> {
    let socket_path = socket_path(project_type);

    // Remove old socket if exists
    let _ = fs::remove_file(&socket_path);

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

    let mut rust_analyzer = Command::new(&binary)
        .arg(workspace_path.to_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = rust_analyzer.stdin.take().unwrap();
    let mut stdout = BufReader::new(rust_analyzer.stdout.take().unwrap());
    let stderr = rust_analyzer.stderr.take().unwrap();

    // Consume stderr in background
    thread::spawn(move || {
        let stderr_reader = BufReader::new(stderr);
        for line in stderr_reader.lines() {
            if line.is_err() {
                break;
            }
        }
    });

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

    // Create Unix socket listener
    let listener = UnixListener::bind(&socket_path)?;
    eprintln!("MCP server listening on {:?}", socket_path);

    let last_activity = Arc::new(Mutex::new(Instant::now()));
    let shutdown = Arc::new(AtomicBool::new(false));
    let request_id = Arc::new(AtomicU64::new(100)); // Start at 100 to avoid conflicts

    // Spawn idle timeout checker
    let timeout_activity = last_activity.clone();
    let timeout_shutdown = shutdown.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));

        if timeout_shutdown.load(Ordering::SeqCst) {
            break;
        }

        let last = timeout_activity.lock().unwrap();
        if last.elapsed() > Duration::from_secs(15) {
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
            Ok((mut stream, _)) => {
                // Update last activity
                *last_activity.lock().unwrap() = Instant::now();

                // Set stream to blocking mode (listener is non-blocking for shutdown checks)
                stream.set_nonblocking(false)?;

                // Handle client connection
                let mut stream_reader = BufReader::new(stream.try_clone()?);

                loop {
                    let mut request_line = String::new();
                    let bytes = stream_reader.read_line(&mut request_line)?;

                    if bytes == 0 {
                        break; // Client disconnected
                    }

                    // Parse request
                    let request: Value = serde_json::from_str(&request_line)?;

                    // Forward to rust-analyzer
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
                    stdin.write_all(forward_str.as_bytes())?;
                    stdin.write_all(b"\n")?;
                    stdin.flush()?;

                    // Read response from rust-analyzer
                    let mut response_line = String::new();
                    stdout.read_line(&mut response_line)?;

                    // Forward response to client
                    stream.write_all(response_line.as_bytes())?;
                    stream.flush()?;

                    // Update activity
                    *last_activity.lock().unwrap() = Instant::now();
                }
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

    // Cleanup
    let _ = rust_analyzer.kill();
    let _ = fs::remove_file(&socket_path);
    eprintln!("MCP server shutdown");

    Ok(())
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
    let timeout = Duration::from_secs(10);
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
    let socket_dir = std::env::temp_dir().join("ra-mcp");
    let _ = fs::create_dir_all(&socket_dir);
    // Use shorter names to avoid SUN_LEN limit (104 bytes on macOS)
    let socket_name = match project_type {
        "test-project-diagnostics" => "diag.sock",
        "test-project-concurrent" => "conc.sock",
        "test-project-singleton" => "sing.sock",
        _ => "main.sock",
    };
    socket_dir.join(socket_name)
}
