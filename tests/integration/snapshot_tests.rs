use anyhow::Result;
use insta::assert_json_snapshot;
use serde_json::{json, Value};

#[path = "../common/mod.rs"]
mod common;
use common::{fixtures, test_client::MCPTestClient};

/// Normalize response for snapshot testing
fn normalize_response(mut response: Value) -> Value {
    // Remove variable parts like timestamps, IDs, paths
    if let Some(obj) = response.as_object_mut() {
        // Normalize paths
        for (_, value) in obj.iter_mut() {
            normalize_value(value);
        }
    }
    response
}

fn normalize_value(value: &mut Value) {
    match value {
        Value::String(s) => {
            // Normalize file paths
            if s.contains("/") || s.contains("\\") {
                *s = s.split('/').last().unwrap_or(s).to_string();
            }
        }
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                // Remove variable fields
                if key == "id" || key == "timestamp" || key == "pid" {
                    *val = json!("[normalized]");
                } else if key == "uri" || key == "targetUri" {
                    if let Some(s) = val.as_str() {
                        *val = json!(s.split('/').last().unwrap_or(s));
                    }
                } else {
                    normalize_value(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                normalize_value(item);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn test_symbols_snapshot() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(workspace.path())?;

    let mut client = MCPTestClient::start(workspace.path())?;
    client.initialize()?;

    // Wait for rust-analyzer
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let response = client.get_symbols("src/main.rs")?;

    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let mut symbols: Value = serde_json::from_str(text.as_str().unwrap_or("[]"))?;
            normalize_value(&mut symbols);

            assert_json_snapshot!(symbols, {
                "[].location.range" => "[range]",
                "[].location.uri" => "[uri]"
            });
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_hover_snapshot() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(workspace.path())?;

    let mut client = MCPTestClient::start(workspace.path())?;
    client.initialize()?;

    // Wait for rust-analyzer
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let response = client.get_hover("src/main.rs", 13, 7)?;

    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let mut hover: Value = serde_json::from_str(text.as_str().unwrap_or("{}"))?;
            normalize_value(&mut hover);

            assert_json_snapshot!(hover, {
                ".range" => "[range]"
            });
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_completion_snapshot() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(workspace.path())?;

    let mut client = MCPTestClient::start(workspace.path())?;
    client.initialize()?;

    // Wait for rust-analyzer
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let response = client.get_completion("src/main.rs", 2, 5)?;

    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let mut completions: Value = serde_json::from_str(text.as_str().unwrap_or("{}"))?;

            // Normalize completion items
            if let Some(items) = completions.get_mut("items") {
                if let Some(arr) = items.as_array_mut() {
                    // Sort by label for consistent snapshots
                    arr.sort_by(|a, b| {
                        let a_label = a.get("label").and_then(|v| v.as_str()).unwrap_or("");
                        let b_label = b.get("label").and_then(|v| v.as_str()).unwrap_or("");
                        a_label.cmp(b_label)
                    });

                    // Take only first 10 for snapshot
                    arr.truncate(10);
                }
            }

            normalize_value(&mut completions);

            assert_json_snapshot!(completions, {
                ".items[].documentation" => "[docs]",
                ".items[].data" => "[data]"
            });
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_definition_snapshot() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(workspace.path())?;

    let mut client = MCPTestClient::start(workspace.path())?;
    client.initialize()?;

    // Wait for rust-analyzer
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let response = client.get_definition("src/main.rs", 1, 20)?;

    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let mut definitions: Value = serde_json::from_str(text.as_str().unwrap_or("[]"))?;
            normalize_value(&mut definitions);

            assert_json_snapshot!(definitions, {
                "[].targetUri" => "[uri]",
                "[].targetRange" => "[range]",
                "[].targetSelectionRange" => "[range]"
            });
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_references_snapshot() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(workspace.path())?;

    let mut client = MCPTestClient::start(workspace.path())?;
    client.initialize()?;

    // Wait for rust-analyzer
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let response = client.get_references("src/main.rs", 9, 3)?;

    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let mut references: Value = serde_json::from_str(text.as_str().unwrap_or("[]"))?;
            normalize_value(&mut references);

            assert_json_snapshot!(references, {
                "[].uri" => "[uri]",
                "[].range.start.character" => "[char]",
                "[].range.end.character" => "[char]"
            });
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_error_response_snapshot() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(workspace.path())?;

    let mut client = MCPTestClient::start(workspace.path())?;
    client.initialize()?;

    // Try to get symbols for non-existent file
    let response = client.get_symbols("non_existent.rs");

    if let Err(e) = response {
        let error_msg = e.to_string();
        // Normalize error message
        let normalized = if error_msg.contains("not found") {
            "File not found error"
        } else if error_msg.contains("error") {
            "Generic error"
        } else {
            "Unknown error"
        };

        assert_json_snapshot!(json!({
            "error": normalized
        }));
    } else if let Ok(resp) = response {
        // Empty response is also valid
        let mut normalized = resp;
        normalize_value(&mut normalized);
        assert_json_snapshot!(normalized);
    }

    Ok(())
}
