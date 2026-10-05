//! Tests the rust-analyzer extensions, which are not LSP and are not all advertised.
//!
//! rust-analyzer answers `rust-analyzer/expandMacro` and `rust-analyzer/relatedTests` without
//! listing either in its capabilities, and answers runnables under `experimental/` while
//! refusing the same name under `rust-analyzer/`. Neither fact is derivable from anything it
//! reports about itself, so these tests are what says the wiring is still right.

use anyhow::Result;
use serde_json::{json, Value};
use test_support::IpcClient;

#[tokio::test]
async fn a_macro_call_expands_to_the_code_it_becomes() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let utils = client.workspace_path().join("src/utils.rs");

    // The `println!` inside `process`.
    let expansion = call(
        &mut client,
        "rust_analyzer_expand_macro",
        json!({ "file_path": utils.to_str().unwrap(), "line": 4, "character": 6 }),
    )
    .await?;

    assert_eq!(
        expansion["name"], "println!",
        "the position is expected to resolve to the `println!` call: {expansion}"
    );
    assert!(
        expansion["expansion"]
            .as_str()
            .is_some_and(|code| code.contains("_print")),
        "the expansion is expected to be the code `println!` becomes: {expansion}"
    );

    Ok(())
}

#[tokio::test]
async fn a_position_with_no_macro_at_it_says_so() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let utils = client.workspace_path().join("src/utils.rs");

    // The name `process`, which is not a macro call.
    let complaint = client
        .call_tool(
            "rust_analyzer_expand_macro",
            json!({ "file_path": utils.to_str().unwrap(), "line": 3, "character": 7 }),
        )
        .await
        .expect_err("a function name expands to nothing");

    assert!(
        complaint.to_string().contains("Nothing to expand"),
        "the error is expected to say there is no macro at the position: {complaint}"
    );

    Ok(())
}

#[tokio::test]
async fn runnables_give_the_cargo_command_for_a_file() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let lib = client.workspace_path().join("src/lib.rs");

    let runnables = call(
        &mut client,
        "rust_analyzer_runnables",
        json!({ "file_path": lib.to_str().unwrap() }),
    )
    .await?;

    let labels: Vec<&str> = runnables
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|runnable| runnable["label"].as_str())
        .collect();
    assert!(
        labels.iter().any(|label| label.contains("test-project")),
        "a runnable naming the package is expected: {labels:?}"
    );

    Ok(())
}

/// `relatedTests` is an unadvertised extension, so what is being tested here is that it is
/// answered at all -- an unknown request would fail instead. The fixture has no tests for the
/// answer to name.
#[tokio::test]
async fn related_tests_is_answered() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let utils = client.workspace_path().join("src/utils.rs");

    let tests = call(
        &mut client,
        "rust_analyzer_related_tests",
        json!({ "file_path": utils.to_str().unwrap(), "line": 3, "character": 7 }),
    )
    .await?;

    assert!(
        tests.is_array(),
        "rust-analyzer is expected to answer with a list of tests: {tests}"
    );

    Ok(())
}

/// The parsed answer of a tool that replies with JSON in a text content item.
async fn call(client: &mut IpcClient, tool: &str, arguments: Value) -> Result<Value> {
    let response = client.call_tool(tool, arguments).await?;
    let text = response["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a text content item, got: {response}"));

    Ok(serde_json::from_str(text)?)
}
