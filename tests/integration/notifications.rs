//! Tests for the JSON-RPC framing of the MCP layer itself.
//!
//! These drive the server binary directly instead of going through the shared test daemon: what
//! matters here is the exact set of lines the server writes, which the daemon's request/response
//! pairing hides.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

/// How long the server gets to answer; none of these requests starts rust-analyzer.
const TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn notifications_are_not_answered() -> Result<()> {
    // The `notifications/initialized` of the MCP handshake, and a notification the server knows
    // nothing about: neither may draw a response, not even a "method not found" error.
    let responses = talk_to_server(&[
        r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":0}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    ])
    .await?;

    let ids: Vec<&Value> = responses.iter().map(|response| &response["id"]).collect();
    assert_eq!(
        ids,
        vec![&Value::from(0), &Value::from(1)],
        "only the two requests may be answered, got: {responses:?}"
    );
    for response in &responses {
        assert!(
            response.get("error").is_none(),
            "unexpected error response: {response}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn ping_is_answered() -> Result<()> {
    // The client's liveness check, which it may send before `initialize` and at any point after.
    let responses = talk_to_server(&[r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#]).await?;

    assert_eq!(responses.len(), 1, "got: {responses:?}");
    assert_eq!(responses[0]["id"], 3);
    assert_eq!(responses[0]["result"], serde_json::json!({}));

    Ok(())
}

#[tokio::test]
async fn a_null_id_is_not_a_request() -> Result<()> {
    // `id: null` is a request in JSON-RPC but malformed in MCP, where the id "MUST NOT be null".
    // Answering it would mean sending a response with an id no client can match up.
    let responses =
        talk_to_server(&[r#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#]).await?;

    assert!(responses.is_empty(), "got: {responses:?}");

    Ok(())
}

#[tokio::test]
async fn unknown_requests_still_get_an_error() -> Result<()> {
    let responses =
        talk_to_server(&[r#"{"jsonrpc":"2.0","id":7,"method":"no/such/method"}"#]).await?;

    assert_eq!(responses.len(), 1, "got: {responses:?}");
    assert_eq!(responses[0]["id"], 7);
    assert_eq!(responses[0]["error"]["code"], -32601);

    Ok(())
}

/// Feeds `messages` to a fresh server, one per line, and returns everything it wrote back.
///
/// Closing stdin afterwards makes the server exit, so reading its stdout to EOF is enough to see
/// every response it had to give — including the ones it should not have written.
async fn talk_to_server(messages: &[&str]) -> Result<Vec<Value>> {
    let mut server = Command::new(server_binary()?)
        .arg(workspace())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    let mut stdin = server.stdin.take().expect("stdin was piped");
    for message in messages {
        stdin.write_all(message.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }
    stdin.flush().await?;
    drop(stdin);

    let mut stdout = server.stdout.take().expect("stdout was piped");
    let mut output = String::new();
    tokio::time::timeout(TIMEOUT, stdout.read_to_string(&mut output))
        .await
        .map_err(|_| anyhow!("the server did not exit within {TIMEOUT:?}"))??;

    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|e| anyhow!("{e}, in: {line}")))
        .collect()
}

/// The workspace the server is pointed at; no test here gets as far as analysing it.
fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-project")
}

fn server_binary() -> Result<PathBuf> {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let release = target.join("release/rust-analyzer-mcp");
    let debug = target.join("debug/rust-analyzer-mcp");

    // Prefer whichever matches the profile the tests themselves were built with.
    let (first, second) = if cfg!(debug_assertions) {
        (debug, release)
    } else {
        (release, debug)
    };
    if first.exists() {
        return Ok(first);
    }
    if second.exists() {
        return Ok(second);
    }
    Err(anyhow!(
        "no rust-analyzer-mcp binary in {}; run `cargo build` first",
        target.display()
    ))
}
