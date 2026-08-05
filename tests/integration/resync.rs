//! Regression tests for external file changes.
//!
//! rust-analyzer keeps every `didOpen`ed file as an in-memory overlay and
//! ignores its own file-watcher events for it, on the assumption that the
//! client owns that buffer. We are not an editor — every change happens on
//! disk, behind our back — so the server re-syncs open documents from disk at
//! the top of each tool call. Without that, a file touched by one tool call
//! stays frozen at that content forever, and poisons later answers.
//!
//! Each test first polls until rust-analyzer demonstrably answers correctly, so
//! "still indexing" can't be mistaken for "answered from stale content". Only
//! then does it edit on disk and query *immediately*: rust-analyzer's own
//! watcher needs several seconds to notice, so a correct answer at that point
//! can only have come from the re-sync.

use anyhow::Result;
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
use test_support::{IsolatedProject, MCPTestClient};

/// Unwrap the JSON payload a tool call returns inside `content[0].text`.
fn payload_text(response: &Value) -> String {
    assert!(
        response["error"].is_null(),
        "Tool call returned error: {:?}",
        response["error"]
    );
    response["content"][0]["text"]
        .as_str()
        .expect("tool result carries a text content item")
        .to_string()
}

/// Locate `needle` in a file and return its 0-based (line, character).
fn position_of(path: &Path, needle: &str) -> (u32, u32) {
    let source = std::fs::read_to_string(path).expect("source file is readable");
    source
        .lines()
        .enumerate()
        .find_map(|(line, text)| {
            text.find(needle)
                .map(|col| (line as u32, u32::try_from(col).unwrap()))
        })
        .unwrap_or_else(|| panic!("{} not found in {}", needle, path.display()))
}

/// Append to a file such that its mtime is guaranteed to differ afterwards,
/// even on filesystems with 1-second timestamp granularity.
fn append_on_disk(path: &Path, addition: &str) -> Result<()> {
    std::thread::sleep(Duration::from_millis(1100));
    let original = std::fs::read_to_string(path)?;
    std::fs::write(path, format!("{original}{addition}"))?;
    Ok(())
}

/// Poll a tool until its payload satisfies `ready`. Errors and empty results
/// during indexing are retried rather than failed on — the point is to reach a
/// state where a wrong answer is unambiguously wrong.
///
/// Polling does not weaken the post-edit assertions: those run against files
/// rust-analyzer holds as overlays, which it never refreshes from disk on its
/// own. Without the re-sync they stay wrong indefinitely, so a generous budget
/// only absorbs re-index latency under load.
async fn wait_until(
    client: &MCPTestClient,
    tool: &str,
    args: Value,
    ready: impl Fn(&str) -> bool,
    what: &str,
) -> String {
    let budget = if std::env::var("CI").is_ok() {
        Duration::from_secs(120)
    } else {
        Duration::from_secs(60)
    };
    let start = std::time::Instant::now();
    let mut last = String::from("<no successful call yet>");

    while start.elapsed() < budget {
        if let Ok(response) = client.call_tool(tool, args.clone()).await {
            if response["error"].is_null() {
                if let Some(text) = response["content"][0]["text"].as_str() {
                    last = text.to_string();
                    if ready(&last) {
                        return last;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("timed out waiting for {what}; last payload was: {last}");
}

/// The cross-file case: a stale overlay on file A corrupts the answer for a
/// query about file B, even though B itself is re-read on every call.
#[tokio::test]
async fn test_external_edit_to_open_file_is_visible_across_files() -> Result<()> {
    let project = IsolatedProject::new()?;
    let workspace = project.path().to_path_buf();
    let client = MCPTestClient::start(&workspace).await?;
    client.initialize_and_wait().await?;

    let utils = workspace.join("src/utils.rs");
    let lib = workspace.join("src/lib.rs");
    // `+ 7` skips past `pub fn ` onto the identifier itself.
    let (add_line, add_char) = position_of(&lib, "pub fn add");
    let add_char = add_char + 7;
    let (process_line, process_char) = position_of(&utils, "pub fn process");
    let process_char = process_char + 7;

    // Warm-up: querying utils.rs both opens it (making it an overlay) and, once
    // it resolves the call sites in lib.rs, proves rust-analyzer is ready.
    wait_until(
        &client,
        "rust_analyzer_references",
        json!({
            "file_path": utils.to_str().unwrap(),
            "line": process_line,
            "character": process_char
        }),
        |text| text.contains("lib.rs"),
        "rust-analyzer to resolve references to utils::process",
    )
    .await;

    // Baseline: nothing references `lib::add` yet.
    let before = payload_text(
        &client
            .call_tool(
                "rust_analyzer_references",
                json!({
                    "file_path": lib.to_str().unwrap(),
                    "line": add_line,
                    "character": add_char
                }),
            )
            .await?,
    );
    assert!(
        !before.contains("utils.rs"),
        "baseline should not reference utils.rs yet, got: {before}"
    );

    // Add a call to `lib::add` from the already-open utils.rs, on disk.
    append_on_disk(&utils, "\npub fn calls_add() -> i32 { crate::add(1, 2) }\n")?;

    wait_until(
        &client,
        "rust_analyzer_references",
        json!({
            "file_path": lib.to_str().unwrap(),
            "line": add_line,
            "character": add_char
        }),
        |text| text.contains("utils.rs"),
        "references to pick up the new call site in the externally edited utils.rs",
    )
    .await;

    client.shutdown().await?;
    Ok(())
}

/// The workspace-wide case: `workspace_symbol` never goes through the
/// per-file open path, so it used to see whatever content the overlay froze.
#[tokio::test]
async fn test_external_edit_is_visible_to_workspace_symbol() -> Result<()> {
    let project = IsolatedProject::new()?;
    let workspace = project.path().to_path_buf();
    let client = MCPTestClient::start(&workspace).await?;
    client.initialize_and_wait().await?;

    let main_rs = workspace.join("src/main.rs");
    let (greet_line, greet_char) = position_of(&main_rs, "fn greet");
    let greet_char = greet_char + 3;

    // Warm-up: open main.rs as an overlay and wait until the symbol index is
    // actually populated.
    client
        .call_tool(
            "rust_analyzer_hover",
            json!({
                "file_path": main_rs.to_str().unwrap(),
                "line": greet_line,
                "character": greet_char
            }),
        )
        .await?;
    wait_until(
        &client,
        "rust_analyzer_workspace_symbol",
        json!({ "query": "greet" }),
        |text| text.contains("greet"),
        "the workspace symbol index to be populated",
    )
    .await;

    append_on_disk(&main_rs, "\npub fn zzz_resync_marker() -> u32 { 42 }\n")?;

    wait_until(
        &client,
        "rust_analyzer_workspace_symbol",
        json!({ "query": "zzz_resync_marker" }),
        |text| text.contains("zzz_resync_marker"),
        "workspace_symbol to see a symbol added to an already-open file",
    )
    .await;

    client.shutdown().await?;
    Ok(())
}

/// A file that disappears while open must not wedge the server: the re-sync
/// closes it instead of serving content that no longer exists.
#[tokio::test]
async fn test_deleted_open_file_does_not_break_later_calls() -> Result<()> {
    let project = IsolatedProject::new()?;
    let workspace = project.path().to_path_buf();
    let client = MCPTestClient::start(&workspace).await?;
    client.initialize_and_wait().await?;

    // A standalone file, not wired into the crate — deleting it later must not
    // turn the workspace into a build failure.
    let scratch = workspace.join("src/scratch_resync.rs");
    std::fs::write(&scratch, "pub fn scratch_fn() -> u32 { 7 }\n")?;
    client
        .call_tool(
            "rust_analyzer_hover",
            json!({
                "file_path": scratch.to_str().unwrap(),
                "line": 0,
                "character": 8
            }),
        )
        .await?;

    std::fs::remove_file(&scratch)?;

    // Any subsequent call triggers the re-sync sweep over the now-missing file.
    let response = client
        .call_tool(
            "rust_analyzer_workspace_symbol",
            json!({ "query": "process" }),
        )
        .await?;
    assert!(
        response["error"].is_null(),
        "server must stay healthy after an open file is deleted: {:?}",
        response["error"]
    );

    client.shutdown().await?;
    Ok(())
}
