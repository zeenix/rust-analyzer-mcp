//! Tests that the symbols of a file come back in the shape that says where each name is.
//!
//! rust-analyzer answers `textDocument/documentSymbol` in one of two shapes, and which one it
//! picks is decided by what the client said it understood at initialization. The flat shape it
//! falls back to reports each item's whole range, doc comment and all, and nothing else -- so a
//! position taken from it lands above the name, and everything asked at that position is asked
//! about a blank line or a comment. Only the nested shape carries `selectionRange`, which is the
//! name itself.

use anyhow::Result;
use serde_json::{json, Value};
use test_support::IpcClient;

#[tokio::test]
async fn symbols_carry_the_position_of_the_name() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project-symbols").await?;
    let lib_path = client.workspace_path().join("src/lib.rs");
    let source = std::fs::read_to_string(&lib_path)?;

    let symbols = symbols_of(&mut client, lib_path.to_str().unwrap()).await?;
    let symbols = symbols
        .as_array()
        .unwrap_or_else(|| panic!("expected a list of symbols, got: {symbols}"));
    assert!(!symbols.is_empty(), "test-project's lib.rs has symbols");

    for symbol in symbols {
        let name = symbol["name"].as_str().unwrap_or_default();
        let selection = &symbol["selectionRange"];
        assert!(
            selection.is_object(),
            "{name} came back in the flat shape, which has no selectionRange: {symbol}"
        );

        let line = line_of(
            &source,
            selection["start"]["line"].as_u64().unwrap() as usize,
        );
        assert!(
            line.contains(name),
            "selectionRange for {name} points at a line that does not hold it: {line:?}"
        );
    }

    Ok(())
}

/// The doc-commented item is the one that shows the difference, so it gets its own test: its
/// `range` starts at the comment and its `selectionRange` at the name, and a client reading the
/// former is reading a line of prose.
#[tokio::test]
async fn a_doc_comment_does_not_move_the_name() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project-symbols").await?;
    let lib_path = client.workspace_path().join("src/lib.rs");
    let source = std::fs::read_to_string(&lib_path)?;

    let symbols = symbols_of(&mut client, lib_path.to_str().unwrap()).await?;
    let run = symbols
        .as_array()
        .and_then(|symbols| symbols.iter().find(|symbol| symbol["name"] == "run"))
        .cloned()
        .unwrap_or_else(|| panic!("test-project's lib.rs has a doc-commented `run`: {symbols}"));

    let whole = run["range"]["start"]["line"].as_u64().unwrap() as usize;
    let name = run["selectionRange"]["start"]["line"].as_u64().unwrap() as usize;

    assert!(
        line_of(&source, whole).trim_start().starts_with("///"),
        "`run`'s range is expected to start at its doc comment: {:?}",
        line_of(&source, whole)
    );
    assert!(
        line_of(&source, name).contains("fn run"),
        "`run`'s selectionRange is expected to start at its name: {:?}",
        line_of(&source, name)
    );

    let character = run["selectionRange"]["start"]["character"]
        .as_u64()
        .unwrap() as usize;
    assert!(
        line_of(&source, name)[character..].starts_with("run"),
        "selectionRange's character is expected to be the column of the name itself"
    );

    Ok(())
}

/// Nothing else here turns a name into a position, so this is the one tool that can be asked a
/// question without already knowing the answer to it.
#[tokio::test]
async fn a_symbol_can_be_found_by_name_alone() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project-symbols").await?;

    let response = client
        .call_tool(
            "rust_analyzer_workspace_symbols",
            json!({ "query": "Calculator" }),
        )
        .await?;
    let text = response["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a text content item, got: {response}"));
    let found: Value = serde_json::from_str(text)?;

    let calculator = found
        .as_array()
        .and_then(|found| found.iter().find(|symbol| symbol["name"] == "Calculator"))
        .unwrap_or_else(|| panic!("test-project declares a `Calculator`: {found}"));

    // The position has to be the name itself, since the only thing to do with it is ask another
    // question there -- and every other tool answers a question asked at a doc comment with
    // silence.
    let path = calculator["location"]["uri"]
        .as_str()
        .and_then(|uri| uri.strip_prefix("file://"))
        .unwrap_or_else(|| panic!("expected a file URI: {calculator}"));
    let source = std::fs::read_to_string(path)?;
    let start = &calculator["location"]["range"]["start"];
    let line = line_of(&source, start["line"].as_u64().unwrap() as usize);
    let character = start["character"].as_u64().unwrap() as usize;

    assert!(
        line[character..].starts_with("Calculator"),
        "the reported position is expected to be the name itself, found {line:?} at {character}"
    );

    Ok(())
}

/// The symbols of `file_path`, as the tool answers them.
async fn symbols_of(client: &mut IpcClient, file_path: &str) -> Result<Value> {
    let response = client
        .call_tool("rust_analyzer_symbols", json!({ "file_path": file_path }))
        .await?;

    let text = response["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a text content item, got: {response}"));

    Ok(serde_json::from_str(text)?)
}

/// Line `line`, counted from zero as every position in an LSP answer is.
fn line_of(source: &str, line: usize) -> &str {
    source.lines().nth(line).unwrap_or_default()
}
