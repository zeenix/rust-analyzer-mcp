//! Tests that a question about a file rust-analyzer has not loaded is answered with a reason.
//!
//! rust-analyzer knows one workspace, and about anything outside it it says `null` -- the same
//! `null` it says for a symbol nothing refers to. The workspace is therefore the quietest way to
//! get a wrong answer: point the server somewhere it finds no Rust, and every question about
//! every file comes back looking like dead code.

use anyhow::Result;
use serde_json::json;
use tempfile::TempDir;
use test_support::{IsolatedProject, MCPTestClient};

#[tokio::test]
async fn a_directory_with_no_manifest_is_refused_as_a_workspace() -> Result<()> {
    let project = IsolatedProject::new()?;
    let client = MCPTestClient::start(project.path()).await?;
    client.initialize_and_wait().await?;

    let not_a_workspace = TempDir::new()?;
    let refusal = client
        .set_workspace(not_a_workspace.path())
        .await
        .expect_err("a directory with no Cargo.toml is not a workspace");
    let refusal = refusal.to_string();

    assert!(
        refusal.contains("not a Rust workspace"),
        "the refusal is expected to say what is wrong with the path: {refusal}"
    );
    assert!(
        refusal.contains(&project.path().display().to_string()),
        "the refusal is expected to name the workspace left in place: {refusal}"
    );

    // And the workspace it refused to leave still answers, so a typo costs the call rather than
    // the session.
    let symbols = client
        .get_symbols(project.path().join("src/lib.rs").to_str().unwrap())
        .await?;
    assert!(
        symbols["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("run")),
        "the original workspace is expected to still answer: {symbols}"
    );

    Ok(())
}

#[tokio::test]
async fn a_file_outside_the_workspace_is_not_reported_as_empty() -> Result<()> {
    let project = IsolatedProject::new()?;
    let client = MCPTestClient::start(project.path()).await?;
    client.initialize_and_wait().await?;

    // Rust, and real enough to hover, but in no workspace rust-analyzer has loaded.
    let elsewhere = TempDir::new()?;
    let stray = elsewhere.path().join("stray.rs");
    std::fs::write(&stray, "pub fn orphaned() -> u32 {\n    7\n}\n")?;

    let complaint = client
        .get_hover(stray.to_str().unwrap(), 0, 7)
        .await
        .expect_err("a file outside the loaded workspace cannot be answered for");
    let complaint = complaint.to_string();

    assert!(
        complaint.contains("outside the workspace"),
        "the error is expected to say the file is not in the loaded workspace: {complaint}"
    );
    assert!(
        complaint.contains(&project.path().display().to_string()),
        "the error is expected to name the workspace actually loaded: {complaint}"
    );

    Ok(())
}
