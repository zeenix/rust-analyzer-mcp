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
async fn a_workspace_with_no_manifest_is_not_reported_as_empty() -> Result<()> {
    // The door set_workspace cannot close: the workspace a server starts in is its working
    // directory, so a session opened outside a Rust project begins in the state set_workspace
    // refuses to move into -- and the file asked about is *inside* that root, so being outside
    // the workspace is not what is wrong with it.
    let not_a_workspace = TempDir::new()?;
    // Where a crate would keep it, so the readiness poll has a file to find -- and so the point
    // stands that nothing but the missing manifest is wrong here.
    let orphan = not_a_workspace.path().join("src/lib.rs");
    std::fs::create_dir_all(orphan.parent().unwrap())?;
    std::fs::write(&orphan, "pub fn stranded() -> u32 {\n    7\n}\n")?;

    let client = MCPTestClient::start(not_a_workspace.path()).await?;
    client.initialize_and_wait().await?;

    let complaint = client
        .get_hover(orphan.to_str().unwrap(), 0, 7)
        .await
        .expect_err("a workspace with no Cargo.toml has loaded nothing to answer from");
    let complaint = complaint.to_string();

    assert!(
        complaint.contains("not a Rust workspace"),
        "the error is expected to say what is wrong with the loaded root: {complaint}"
    );
    assert!(
        complaint.contains(&not_a_workspace.path().display().to_string()),
        "the error is expected to name the root it is complaining about: {complaint}"
    );

    Ok(())
}

/// A call that names its workspace is answered by that one, and leaves the default where it is.
#[tokio::test]
async fn a_call_can_name_the_workspace_it_is_about() -> Result<()> {
    let default = IsolatedProject::new()?;
    let elsewhere = IsolatedProject::new()?;
    let client = MCPTestClient::start(default.path()).await?;
    client.initialize_and_wait().await?;

    // `pub fn process`, on the name, in the project that is not the default one.
    let answer = client
        .call_tool(
            "rust_analyzer_hover",
            json!({
                "workspace_path": elsewhere.path().to_str().unwrap(),
                "file_path": elsewhere.path().join("src/utils.rs").to_str().unwrap(),
                "line": 3,
                "character": 7,
            }),
        )
        .await?;
    let text = answer["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("process"),
        "the named workspace is expected to answer for its own file: {answer}"
    );

    // And the default is still the default: a file in it answers without being asked for by
    // workspace, which is what a second caller of this server would be relying on.
    let unmoved = client
        .get_hover(default.path().join("src/utils.rs").to_str().unwrap(), 3, 7)
        .await?;
    assert!(
        unmoved["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("process")),
        "naming a workspace for one call is not expected to move the default: {unmoved}"
    );

    Ok(())
}

/// A workspace named by a call is checked the way one set as the default is.
#[tokio::test]
async fn a_call_cannot_name_a_workspace_that_is_not_one() -> Result<()> {
    let project = IsolatedProject::new()?;
    let client = MCPTestClient::start(project.path()).await?;
    client.initialize_and_wait().await?;

    let not_a_workspace = TempDir::new()?;
    let refusal = client
        .call_tool(
            "rust_analyzer_hover",
            json!({
                "workspace_path": not_a_workspace.path().to_str().unwrap(),
                "file_path": project.path().join("src/utils.rs").to_str().unwrap(),
                "line": 3,
                "character": 7,
            }),
        )
        .await
        .expect_err("a directory with no Cargo.toml is not a workspace to answer from");

    assert!(
        refusal.to_string().contains("not a Rust workspace"),
        "the refusal is expected to say what is wrong with the path: {refusal}"
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
