//! Tests for what rust-analyzer is told about files that change while it is running.
//!
//! These edit their workspace, so they get one of their own rather than sharing the daemon the
//! other integration tests talk to.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use test_support::{IsolatedProject, MCPTestClient};

#[tokio::test]
async fn symbols_follow_an_edit() -> Result<()> {
    let project = IsolatedProject::new()?;
    let lib = project.path().join("src/lib.rs");
    let client = MCPTestClient::start(project.path()).await?;
    client.initialize_and_wait().await?;

    let before = symbol_names(&client, &lib).await?;
    assert!(
        !before.contains(&"added_after_opening".to_string()),
        "{before:?}"
    );

    // What an agent does between two tool calls, and what rust-analyzer will not notice on its
    // own: an open document's content is the client's to keep up to date.
    let mut source = std::fs::read_to_string(&lib)?;
    source.push_str("\npub fn added_after_opening() -> u32 {\n    42\n}\n");
    std::fs::write(&lib, source)?;

    let after = symbol_names(&client, &lib).await?;
    assert!(
        after.contains(&"added_after_opening".to_string()),
        "the symbols still describe the file as it was opened: {after:?}"
    );

    client.shutdown().await
}

async fn symbol_names(client: &MCPTestClient, file: &std::path::Path) -> Result<Vec<String>> {
    let response = client
        .call_tool(
            "rust_analyzer_symbols",
            json!({ "file_path": file.to_str().unwrap() }),
        )
        .await?;

    let text = response["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow!("No text in symbols response: {response}"))?;
    let symbols: Vec<Value> = serde_json::from_str(text)?;

    Ok(symbols
        .iter()
        .filter_map(|symbol| Some(symbol.get("name")?.as_str()?.to_string()))
        .collect())
}

#[tokio::test]
async fn diagnostics_follow_an_edit() -> Result<()> {
    let project = IsolatedProject::new_diagnostics()?;
    let errors = project.path().join("src/errors.rs");
    let client = MCPTestClient::start(project.path()).await?;
    client.initialize_and_wait().await?;

    let mut before = diagnostics(&client, &errors).await?;
    for _ in 0..5 {
        if before["note"].is_null() {
            break;
        }
        // rust-analyzer says it had not finished; asking again is what it asks for.
        before = diagnostics(&client, &errors).await?;
    }
    assert!(
        before["summary"]["errors"].as_u64().unwrap_or(0) > 0,
        "the file is full of deliberate errors: {before}"
    );

    // The whole point of the tool, from an agent's side: fix what was reported, then ask again.
    std::fs::write(
        &errors,
        "//! Every error this file had, fixed.\n\npub fn no_longer_broken() -> i32 {\n    42\n}\n",
    )?;

    // The tool says so itself when it could not wait a check out -- another workspace check was
    // already running, say -- and asking again is what it asks for.
    let mut after = diagnostics(&client, &errors).await?;
    for _ in 0..5 {
        if after["summary"]["errors"] == json!(0) || after["note"].is_null() {
            break;
        }
        after = diagnostics(&client, &errors).await?;
    }

    assert_eq!(
        after["summary"]["errors"].as_u64(),
        Some(0),
        "the errors were fixed, but the report still describes the old file: {after}"
    );

    client.shutdown().await
}

async fn diagnostics(client: &MCPTestClient, file: &std::path::Path) -> Result<Value> {
    let response = client
        .call_tool(
            "rust_analyzer_diagnostics",
            json!({ "file_path": file.to_str().unwrap() }),
        )
        .await?;

    let text = response["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow!("No text in diagnostics response: {response}"))?;

    Ok(serde_json::from_str(text)?)
}
