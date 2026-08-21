//! Tests that the cargo features named on the command line reach rust-analyzer.
//!
//! Nothing else here can tell: a feature-gated function is either analysed or invisible, and
//! which of the two it is only shows in what rust-analyzer says about the code inside it.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

/// Long enough for a cold rust-analyzer to load a one-file crate and run a cargo check over it.
const TIMEOUT: Duration = Duration::from_secs(120);

/// A crate whose only mistake is behind a feature gate.
const CRATE: &str = r#"
#[cfg(feature = "extra")]
pub fn only_with_extra() -> i32 {
    let wrong: i32 = "not a number";
    wrong
}
"#;

#[tokio::test]
async fn feature_gated_code_is_analysed_when_asked_for() -> Result<()> {
    let project = feature_gated_crate()?;

    let without = diagnostics(project.path(), &[]).await?;
    assert_eq!(
        without["summary"]["errors"], 0,
        "the gated code is not part of the crate by default: {without}"
    );

    let with = diagnostics(project.path(), &["--all-features"]).await?;
    assert!(
        with["summary"]["errors"].as_u64().unwrap_or(0) > 0,
        "--all-features must make the gated code count: {with}"
    );

    Ok(())
}

#[tokio::test]
async fn a_named_feature_is_analysed_too() -> Result<()> {
    let project = feature_gated_crate()?;

    let with = diagnostics(project.path(), &["--features", "extra"]).await?;

    assert!(
        with["summary"]["errors"].as_u64().unwrap_or(0) > 0,
        "--features extra must make the gated code count: {with}"
    );

    Ok(())
}

/// Diagnostics for the crate's only file, from a server started with `options`.
async fn diagnostics(workspace: &std::path::Path, options: &[&str]) -> Result<Value> {
    let mut server = Command::new(server_binary()?)
        .args(options)
        .arg(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    let mut stdin = server.stdin.take().expect("stdin was piped");
    for message in [
        r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"rust_analyzer_diagnostics","arguments":{"file_path":"src/lib.rs"}}}"#,
    ] {
        stdin.write_all(message.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }
    stdin.flush().await?;

    let mut stdout = BufReader::new(server.stdout.take().expect("stdout was piped"));
    let mut response = String::new();
    tokio::time::timeout(TIMEOUT, async {
        // The first line answers the initialize; the second carries the diagnostics.
        for _ in 0..2 {
            response.clear();
            stdout.read_line(&mut response).await?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow!("the server did not report diagnostics within {TIMEOUT:?}"))??;

    let response: Value = serde_json::from_str(&response)?;
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow!("No diagnostics in response: {response}"))?;

    Ok(serde_json::from_str(text)?)
}

/// A crate with an `extra` feature, in a directory of its own.
fn feature_gated_crate() -> Result<tempfile::TempDir> {
    let project = tempfile::TempDir::new()?;
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"features\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [features]\nextra = []\n\n[workspace]\n",
    )?;
    std::fs::create_dir(project.path().join("src"))?;
    std::fs::write(project.path().join("src/lib.rs"), CRATE)?;

    Ok(project)
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
