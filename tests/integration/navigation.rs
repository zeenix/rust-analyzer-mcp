//! Tests the questions that are about the code rather than about the text of it.
//!
//! References says where a name appears; a call hierarchy says which functions the appearances
//! are in. Implementation says which types carry a trait, which nothing reading the text can work
//! out at all, because the answer is not written at the place being asked about.

use anyhow::Result;
use serde_json::{json, Value};
use test_support::IpcClient;

#[tokio::test]
async fn incoming_calls_name_the_calling_function() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let utils = client.workspace_path().join("src/utils.rs");

    // `pub fn process`, on the name itself.
    let answer = call(
        &mut client,
        "rust_analyzer_incoming_calls",
        json!({ "file_path": utils.to_str().unwrap(), "line": 3, "character": 7 }),
    )
    .await?;

    assert_eq!(
        answer["item"]["name"], "process",
        "the position is expected to resolve to `process`: {answer}"
    );

    let callers: Vec<&str> = answer["calls"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|call| call["from"]["name"].as_str())
        .collect();
    assert!(
        callers.contains(&"run"),
        "`run` calls `process`, so it is expected among the callers: {callers:?}"
    );

    Ok(())
}

#[tokio::test]
async fn outgoing_calls_name_what_the_function_calls() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let utils = client.workspace_path().join("src/utils.rs");

    let answer = call(
        &mut client,
        "rust_analyzer_outgoing_calls",
        json!({ "file_path": utils.to_str().unwrap(), "line": 3, "character": 7 }),
    )
    .await?;

    let called: Vec<&str> = answer["calls"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|call| call["to"]["name"].as_str())
        .collect();
    assert!(
        called.contains(&"validate"),
        "`process` calls `validate`: {called:?}"
    );

    Ok(())
}

#[tokio::test]
async fn a_position_that_calls_nothing_says_so() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let utils = client.workspace_path().join("src/utils.rs");

    // The doc comment above `process`, where an empty answer would be indistinguishable from a
    // function nothing calls.
    let complaint = client
        .call_tool(
            "rust_analyzer_incoming_calls",
            json!({ "file_path": utils.to_str().unwrap(), "line": 2, "character": 4 }),
        )
        .await
        .expect_err("a doc comment is not callable");

    assert!(
        complaint.to_string().contains("Nothing callable"),
        "the error is expected to say the position is not on anything callable: {complaint}"
    );

    Ok(())
}

#[tokio::test]
async fn an_implementation_is_found_by_the_trait_it_implements() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let types = client.workspace_path().join("src/types.rs");

    // `impl Default for Config`, on `Default`.
    let found = call(
        &mut client,
        "rust_analyzer_implementation",
        json!({ "file_path": types.to_str().unwrap(), "line": 23, "character": 5 }),
    )
    .await?;

    let implementations = found
        .as_array()
        .unwrap_or_else(|| panic!("expected a list of implementations, got: {found}"));
    assert!(
        !implementations.is_empty(),
        "`Default` is implemented in this project, so the answer is not empty: {found}"
    );

    Ok(())
}

/// A reference is where a name appears, and the line it appears in says which appearance it is.
#[tokio::test]
async fn references_carry_the_line_they_point_at() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let utils = client.workspace_path().join("src/utils.rs");

    // `pub fn process`, on the name itself.
    let answer = call(
        &mut client,
        "rust_analyzer_references",
        json!({ "file_path": utils.to_str().unwrap(), "line": 3, "character": 7 }),
    )
    .await?;

    let hits = answer["locations"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a list of locations, got: {answer}"));
    assert!(
        !hits.is_empty(),
        "`process` is called in this project: {answer}"
    );
    assert_eq!(
        answer["count"].as_u64(),
        Some(hits.len() as u64),
        "the count is expected to be of the hits reported: {answer}"
    );

    for hit in hits {
        assert!(
            hit["file"].is_string() && hit["line"].is_number() && hit["character"].is_number(),
            "every hit is expected to say where it is: {hit}"
        );
        // The path is the one a reader reads, not the temporary directory the test runs in.
        assert!(
            !hit["file"].as_str().unwrap_or_default().starts_with('/'),
            "a hit inside the workspace is expected to be named relative to it: {hit}"
        );
        assert!(
            hit["text"]
                .as_str()
                .is_some_and(|text| text.contains("process")),
            "the line quoted is expected to be the line the hit points at: {hit}"
        );
    }

    Ok(())
}

/// LSP counts a symbol's declaration among its references, so a count read as "callers" is one
/// too many. The hit that is the declaration says so.
#[tokio::test]
async fn references_say_which_hit_is_the_declaration() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let utils = client.workspace_path().join("src/utils.rs");

    // `pub fn process`, on the name itself.
    let answer = call(
        &mut client,
        "rust_analyzer_references",
        json!({ "file_path": utils.to_str().unwrap(), "line": 3, "character": 7 }),
    )
    .await?;

    let hits = answer["locations"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a list of locations, got: {answer}"));

    let declarations: Vec<_> = hits
        .iter()
        .filter(|hit| hit["declaration"] == json!(true))
        .collect();
    assert_eq!(
        declarations.len(),
        1,
        "exactly one hit is expected to be the declaration: {answer}"
    );

    let declaration = declarations[0];
    assert_eq!(
        declaration["line"].as_u64(),
        Some(3),
        "the declaration is expected to be the one at `pub fn process`: {declaration}"
    );
    assert!(
        declaration["text"]
            .as_str()
            .is_some_and(|text| text.contains("fn process")),
        "the hit marked as the declaration is expected to be the declaring line: {declaration}"
    );

    assert!(
        hits.len() > declarations.len(),
        "`process` is used as well as declared, so not every hit is the declaration: {answer}"
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
