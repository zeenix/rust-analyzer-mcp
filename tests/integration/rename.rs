//! Tests for the rename tool.
//!
//! On a server of their own: renaming reaches across the whole workspace, and the shared
//! `test-project` server is pointed at another one partway through its tests.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use test_support::IpcClient;

/// `fn greet` in the test project's `main.rs`, and the call to it above.
const DEFINITION: (u64, u64) = (13, 3);

#[tokio::test]
async fn renaming_a_symbol_says_what_it_would_take() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project-rename").await?;
    let main = client.workspace_path().join("src/main.rs");
    let before = std::fs::read_to_string(&main)?;

    let rename = rename(&mut client, &main, DEFINITION, "welcome").await?;

    assert_eq!(rename["old_name"], "greet", "{rename}");
    assert_eq!(rename["new_name"], "welcome", "{rename}");
    assert_eq!(rename["applied"], false, "{rename}");
    // The definition and the call to it, at least.
    assert!(
        rename["summary"]["edits"].as_u64().unwrap_or(0) >= 2,
        "{rename}"
    );

    for change in rename["changes"].as_array().unwrap_or(&Vec::new()) {
        let edits = change["edits"].as_array().cloned().unwrap_or_default();
        for edit in &edits {
            // Spelled out, so an edit can be applied to the bytes of the file and checked first.
            assert_eq!(edit["old_text"], "greet", "{rename}");
            assert_eq!(edit["new_text"], "welcome", "{rename}");
            assert!(edit["byte_range"].is_array(), "{rename}");
        }

        // Last edit first, so applying them one after another needs no arithmetic.
        let lines: Vec<Value> = edits.iter().map(|edit| edit["line"].clone()).collect();
        let mut last_first = lines.clone();
        last_first.sort_by(|a, b| b.as_u64().cmp(&a.as_u64()));
        assert_eq!(lines, last_first, "{rename}");
    }

    // Nothing was renamed, only worked out.
    assert_eq!(std::fs::read_to_string(&main)?, before);

    Ok(())
}

#[tokio::test]
async fn a_rename_that_cannot_be_done_says_why() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project-rename").await?;
    let main = client.workspace_path().join("src/main.rs");

    let refused = rename(&mut client, &main, DEFINITION, "1")
        .await
        .expect_err("`1` is not a name");

    // rust-analyzer's own words, which are more use than a bare failure.
    let refused = refused.to_string();
    assert!(refused.contains('1'), "{refused}");

    Ok(())
}

#[tokio::test]
async fn renaming_nothing_at_all_says_so() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project-rename").await?;
    let main = client.workspace_path().join("src/main.rs");

    // A blank line, where there is no symbol to rename.
    let refused = rename(&mut client, &main, (12, 0), "welcome")
        .await
        .expect_err("there is nothing on that line to rename");

    assert!(!refused.to_string().is_empty());

    Ok(())
}

async fn rename(
    client: &mut IpcClient,
    file: &std::path::Path,
    (line, character): (u64, u64),
    new_name: &str,
) -> Result<Value> {
    let response = client
        .call_tool(
            "rust_analyzer_rename",
            json!({
                "file_path": file.to_str().unwrap(),
                "line": line,
                "character": character,
                "new_name": new_name
            }),
        )
        .await?;

    let text = response["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow!("No text in rename response: {response}"))?;

    Ok(serde_json::from_str(text)?)
}
