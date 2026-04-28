use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::{process::Command, time::Duration};

use test_support::MCPTestClient;

/// Kills rust-analyzer underneath a running MCP server and verifies the next
/// tool call succeeds because the server transparently restarted it.
#[tokio::test]
async fn test_subprocess_restart_on_crash() -> Result<()> {
    let client = MCPTestClient::start_isolated().await?;
    client.initialize_and_wait().await?;

    // First call to ensure rust-analyzer is fully running.
    let response = client.get_symbols("src/main.rs").await?;
    expect_some_content(&response)?;

    // Find rust-analyzer's PID — it's a child of the MCP server process.
    let server_pid = client
        .server_pid()
        .await
        .ok_or_else(|| anyhow!("MCP server has no PID"))?;
    let ra_pid = find_rust_analyzer_pid(server_pid)?;
    eprintln!("Killing rust-analyzer pid {ra_pid} (parent {server_pid})");

    // SIGKILL — simulate a crash.
    let kill_status = Command::new("kill")
        .arg("-9")
        .arg(ra_pid.to_string())
        .status()?;
    assert!(kill_status.success(), "kill -9 {ra_pid} failed");

    // Monitor polls every 500ms; give it time to detect + the server time to
    // restart rust-analyzer + cachePriming on the next request.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second call should succeed via auto-restart.
    let response = client.get_symbols("src/main.rs").await?;
    expect_some_content(&response)?;

    // PID should differ now.
    let new_ra_pid = find_rust_analyzer_pid(server_pid)?;
    assert_ne!(
        new_ra_pid, ra_pid,
        "rust-analyzer should have been replaced (old={ra_pid}, new={new_ra_pid})"
    );

    Ok(())
}

fn expect_some_content(response: &Value) -> Result<()> {
    let content = response
        .get("content")
        .ok_or_else(|| anyhow!("response missing content: {:?}", response))?;
    let arr = content
        .as_array()
        .ok_or_else(|| anyhow!("content is not an array"))?;
    if arr.is_empty() {
        return Err(anyhow!("content array empty"));
    }
    let _ = arr[0]
        .get("text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("first content item missing text: {:?}", arr[0]))?;
    Ok(())
}

fn find_rust_analyzer_pid(parent_pid: u32) -> Result<u32> {
    // pgrep -P <parent> rust-analyzer — pids of rust-analyzer that are direct
    // children of `parent_pid`. We expect exactly one.
    let output = Command::new("pgrep")
        .args(["-P", &parent_pid.to_string(), "rust-analyzer"])
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "pgrep failed: status {:?}, stderr {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout
        .lines()
        .next()
        .ok_or_else(|| anyhow!("no rust-analyzer child of pid {parent_pid}"))?;
    first
        .trim()
        .parse::<u32>()
        .map_err(|e| anyhow!("bad pid {first:?}: {e}"))
}

// Suppress unused-import warning on platforms that wouldn't reach the helpers.
#[allow(dead_code)]
fn _unused(_: Value) -> Value {
    json!(null)
}
