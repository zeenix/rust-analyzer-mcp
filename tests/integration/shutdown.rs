use anyhow::Result;
use serde_json::{json, Value};
use std::{ffi::OsStr, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

/// How long the server gets to exit after a shutdown trigger.
///
/// Comfortably covers the server's own graceful-shutdown timeout, after which it force-kills
/// rust-analyzer.
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for a response line from the server.
///
/// Generous because a first tool call cold-starts rust-analyzer, which is slow in CI.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(180);

#[cfg(unix)]
#[tokio::test]
async fn exits_on_sigint() -> Result<()> {
    assert_signal_exit("-INT").await
}

#[cfg(unix)]
#[tokio::test]
async fn exits_on_sigterm() -> Result<()> {
    assert_signal_exit("-TERM").await
}

#[cfg(unix)]
#[tokio::test]
async fn exits_on_sighup() -> Result<()> {
    assert_signal_exit("-HUP").await
}

#[tokio::test]
async fn exits_on_stdin_close() -> Result<()> {
    let mut server = Server::spawn(std::env::temp_dir()).await?;

    drop(server.stdin.take());
    let (status, stderr) = server.wait_for_exit("stdin EOF").await?;
    assert!(
        status.success(),
        "expected graceful exit, got {status}:\n{stderr}"
    );
    Ok(())
}

/// A signal must terminate the server even once rust-analyzer is running, going through the
/// graceful cleanup path (with its force-kill fallback) rather than relying on a stdin EOF.
#[cfg(unix)]
#[tokio::test]
async fn exits_on_signal_with_live_rust_analyzer() -> Result<()> {
    let workspace = concat!(env!("CARGO_MANIFEST_DIR"), "/test-project");
    let mut server = Server::spawn(workspace).await?;

    // The first tool call spawns and initializes rust-analyzer inside the server.
    let response = server
        .request(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "rust_analyzer_symbols",
                "arguments": { "file_path": "src/main.rs" }
            }
        }))
        .await?;
    anyhow::ensure!(
        response.get("result").is_some(),
        "tool call failed, rust-analyzer is not running: {response}"
    );

    server.signal("-TERM")?;
    let (status, stderr) = server.wait_for_exit("SIGTERM").await?;
    assert!(
        status.success(),
        "expected graceful exit, got {status}:\n{stderr}"
    );
    // A single signal must go through the graceful LSP handshake, not the force-kill paths.
    assert!(
        stderr.contains("Shutting down"),
        "cleanup never ran:\n{stderr}"
    );
    assert!(
        !stderr.contains("Graceful shutdown timed out")
            && !stderr.contains("another shutdown signal"),
        "graceful LSP handshake was skipped:\n{stderr}"
    );
    Ok(())
}

#[cfg(unix)]
async fn assert_signal_exit(signal: &str) -> Result<()> {
    // Keep the write end of stdin open: the signal must suffice on its own, without an EOF. This
    // mirrors both an interactive Ctrl+C and an MCP host that signals before closing the pipe.
    let server = Server::spawn(std::env::temp_dir()).await?;

    server.signal(signal)?;
    let (status, stderr) = server.wait_for_exit(signal).await?;
    assert!(
        status.success(),
        "expected graceful exit, got {status}:\n{stderr}"
    );
    Ok(())
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr_file: tempfile::NamedTempFile,
    /// Keeps the isolated cargo target/cache directories alive for the server's lifetime.
    _isolation_dir: tempfile::TempDir,
}

impl Server {
    /// Spawns the server and waits until it answers an `initialize` request: the response proves
    /// the signal handlers are installed, since the server installs them before reading requests.
    async fn spawn(workspace: impl AsRef<OsStr>) -> Result<Self> {
        // Capture stderr to a file so tests can assert on the server's shutdown logs.
        let stderr_file = tempfile::NamedTempFile::new()?;
        // Isolate cargo state so a spawned rust-analyzer does not serialize on the shared
        // workspace's target-directory lock with the other tests.
        let isolation_dir = tempfile::tempdir()?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_rust-analyzer-mcp"))
            .arg(workspace)
            .env("CARGO_TARGET_DIR", isolation_dir.path().join("target"))
            .env("XDG_CACHE_HOME", isolation_dir.path().join("cache"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr_file.reopen()?))
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));

        let mut server = Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr_file,
            _isolation_dir: isolation_dir,
        };
        server
            .request(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
            .await?;
        Ok(server)
    }

    async fn request(&mut self, request: Value) -> Result<Value> {
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;

        let mut response = String::new();
        timeout(RESPONSE_TIMEOUT, self.stdout.read_line(&mut response)).await??;
        Ok(serde_json::from_str(&response)?)
    }

    #[cfg(unix)]
    fn signal(&self, signal: &str) -> Result<()> {
        let Some(pid) = self.child.id() else {
            anyhow::bail!("server exited before the signal was sent");
        };
        let status = std::process::Command::new("kill")
            .arg(signal)
            .arg(pid.to_string())
            .status()?;
        anyhow::ensure!(status.success(), "kill {signal} {pid} failed");
        Ok(())
    }

    /// Waits for the server to exit and returns its status and captured stderr.
    async fn wait_for_exit(mut self, trigger: &str) -> Result<(std::process::ExitStatus, String)> {
        let status = match timeout(EXIT_TIMEOUT, self.child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = self.child.start_kill();
                let _ = self.child.wait().await;
                anyhow::bail!(
                    "server still running {EXIT_TIMEOUT:?} after {trigger}:\n{}",
                    self.stderr()
                );
            }
        };
        let stderr = self.stderr();
        Ok((status, stderr))
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(self.stderr_file.path()).unwrap_or_default()
    }
}
