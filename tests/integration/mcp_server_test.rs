use anyhow::Result;
use rstest::*;
use serde_json::{json, Value};
// Removed serial_test as tests use independent workspaces
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;

// Import common test utilities
#[path = "../common/mod.rs"]
mod common;
use common::{fixtures, test_client::MCPTestClient};

/// Helper to wait for rust-analyzer to be ready with smart polling
async fn wait_for_analyzer_ready(client: &mut MCPTestClient, _workspace: &PathBuf) -> Result<()> {
    let start = std::time::Instant::now();
    let max_wait = Duration::from_secs(10);

    while start.elapsed() < max_wait {
        // Try to get symbols to check if analyzer is ready
        if let Ok(response) = client.get_symbols("src/main.rs") {
            if let Some(content) = response.get("content") {
                if let Some(text) = content[0].get("text") {
                    // Check if we got actual symbols, not null or empty
                    if text.as_str() != Some("null") && text.as_str() != Some("[]") {
                        if let Ok(symbols) =
                            serde_json::from_str::<Vec<Value>>(text.as_str().unwrap_or("[]"))
                        {
                            if !symbols.is_empty() {
                                return Ok(()); // Analyzer is ready
                            }
                        }
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    Err(anyhow::anyhow!(
        "Timeout waiting for rust-analyzer to be ready"
    ))
}

#[fixture]
fn test_workspace() -> PathBuf {
    let temp_dir = tempfile::tempdir().unwrap();
    let project = fixtures::TestProject::simple();
    project.create_in(temp_dir.path()).unwrap();
    temp_dir.into_path()
}

#[rstest]
#[tokio::test]
async fn test_server_initialization(test_workspace: PathBuf) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;

    // Initialize the server
    let init_response = timeout(Duration::from_secs(10), async { client.initialize() }).await??;

    // Check server info
    assert!(init_response.get("serverInfo").is_some());
    let server_info = &init_response["serverInfo"];
    assert_eq!(server_info["name"], "rust-analyzer-mcp");
    assert!(server_info["version"].is_string());

    // Check capabilities
    assert!(init_response.get("capabilities").is_some());
    let capabilities = &init_response["capabilities"];
    assert!(capabilities.get("tools").is_some());

    Ok(())
}

#[rstest]
#[tokio::test]
async fn test_symbols_tool(test_workspace: PathBuf) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;
    client.initialize()?;

    // Wait for rust-analyzer to be ready with smart polling
    wait_for_analyzer_ready(&mut client, &test_workspace).await?;

    // Get symbols for main.rs
    let response = timeout(Duration::from_secs(10), async {
        client.get_symbols("src/main.rs")
    })
    .await??;

    // Parse response
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let symbols: Vec<Value> = serde_json::from_str(text.as_str().unwrap_or("[]"))?;

            // Check that we have symbols
            assert!(!symbols.is_empty(), "Should have symbols in main.rs");

            // Check for expected symbols
            let symbol_names: Vec<String> = symbols
                .iter()
                .filter_map(|s| s.get("name")?.as_str().map(String::from))
                .collect();

            assert!(
                symbol_names.contains(&"main".to_string()),
                "Should have main function"
            );
            assert!(
                symbol_names.contains(&"greet".to_string()),
                "Should have greet function"
            );
            assert!(
                symbol_names.contains(&"Calculator".to_string()),
                "Should have Calculator struct"
            );
        }
    }

    Ok(())
}

#[rstest]
#[tokio::test]
async fn test_definition_tool(test_workspace: PathBuf) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;
    client.initialize()?;

    // Wait for rust-analyzer to be ready with smart polling
    wait_for_analyzer_ready(&mut client, &test_workspace).await?;

    // Get definition for 'greet' call in main function - retry with short delays
    let mut response = Value::Null;
    for attempt in 0..5 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        response = timeout(Duration::from_secs(10), async {
            client.get_definition("src/main.rs", 1, 20)
        })
        .await??;

        // Check if we got a valid response
        if let Some(content) = response.get("content") {
            if let Some(text) = content[0].get("text") {
                if text.as_str() != Some("null") {
                    break; // Got a valid response
                }
            }
        }
    }

    // Parse response
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            // Handle the response - it might be "null" string initially
            let definitions: Vec<Value> = if text.is_null() || text.as_str() == Some("null") {
                vec![]
            } else {
                serde_json::from_str(text.as_str().unwrap_or("[]"))?
            };

            // Check that we have a definition
            assert!(!definitions.is_empty(), "Should find definition for greet");

            // Check definition points to the right location
            if let Some(def) = definitions.first() {
                assert!(def.get("targetUri").is_some());
                let uri = def["targetUri"].as_str().unwrap();
                assert!(uri.contains("main.rs"), "Definition should be in main.rs");
            }
        }
    }

    Ok(())
}

#[rstest]
#[tokio::test]
async fn test_references_tool(test_workspace: PathBuf) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;
    client.initialize()?;

    // Wait for rust-analyzer to be ready with smart polling
    wait_for_analyzer_ready(&mut client, &test_workspace).await?;

    // Get references for 'greet' function - retry with short delays
    let mut response = Value::Null;
    for attempt in 0..5 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        response = timeout(Duration::from_secs(10), async {
            client.get_references("src/main.rs", 9, 3)
        })
        .await??;

        // Check if we got a valid response
        if let Some(content) = response.get("content") {
            if let Some(text) = content[0].get("text") {
                if text.as_str() != Some("null") {
                    break; // Got a valid response
                }
            }
        }
    }

    // Parse response
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            // Handle the response - it might be "null" string initially
            let references: Vec<Value> = if text.is_null() || text.as_str() == Some("null") {
                vec![]
            } else {
                serde_json::from_str(text.as_str().unwrap_or("[]"))?
            };

            // Check that we have references
            assert!(
                !references.is_empty(),
                "Should find references for greet function"
            );

            // At least one reference should be the call in main
            let has_call_reference = references.iter().any(|r| {
                if let Some(range) = r.get("range") {
                    if let Some(start) = range.get("start") {
                        if let Some(line) = start.get("line").and_then(|l| l.as_u64()) {
                            return line == 1; // Line where greet is called
                        }
                    }
                }
                false
            });

            assert!(has_call_reference, "Should find the call to greet in main");
        }
    }

    Ok(())
}

#[rstest]
#[tokio::test]
async fn test_hover_tool(test_workspace: PathBuf) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;
    client.initialize()?;

    // Wait for rust-analyzer to be ready with smart polling
    wait_for_analyzer_ready(&mut client, &test_workspace).await?;

    // Get hover information for 'Calculator' struct - retry with short delays
    let mut response = Value::Null;
    for attempt in 0..5 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        response = timeout(Duration::from_secs(10), async {
            client.get_hover("src/main.rs", 13, 7)
        })
        .await??;

        // Check if we got a valid response
        if let Some(content) = response.get("content") {
            if let Some(text) = content[0].get("text") {
                if text.as_str() != Some("null") {
                    break; // Got a valid response
                }
            }
        }
    }

    // Parse response
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            // Handle the response - it might be "null" string initially
            let hover: Value = if text.is_null() || text.as_str() == Some("null") {
                json!({})
            } else {
                serde_json::from_str(text.as_str().unwrap_or("{}"))?
            };

            // Check that we have hover content
            assert!(
                hover.get("contents").is_some(),
                "Should have hover contents"
            );

            let contents = &hover["contents"];
            if let Some(value) = contents.get("value") {
                let hover_text = value.as_str().unwrap_or("");
                assert!(!hover_text.is_empty(), "Hover should have content");
                // Hover text should mention Calculator
                assert!(
                    hover_text.contains("Calculator") || hover_text.contains("struct"),
                    "Hover should show information about Calculator struct"
                );
            }
        }
    }

    Ok(())
}

#[rstest]
#[tokio::test]
async fn test_completion_tool(test_workspace: PathBuf) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;
    client.initialize()?;

    // Wait for rust-analyzer to be ready with smart polling
    wait_for_analyzer_ready(&mut client, &test_workspace).await?;

    // Get completions in main function
    let response = timeout(Duration::from_secs(10), async {
        client.get_completion("src/main.rs", 2, 5)
    })
    .await??;

    // Parse response
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let completions: Value = serde_json::from_str(text.as_str().unwrap_or("{}"))?;

            // Check for completion items
            if let Some(items) = completions.get("items") {
                if let Some(items_array) = items.as_array() {
                    assert!(!items_array.is_empty(), "Should have completion items");
                }
            } else if completions.is_array() {
                // Alternative format - just an array of completions
                let items = completions.as_array().unwrap();
                assert!(!items.is_empty(), "Should have completion items");
            }
        }
    }

    Ok(())
}

#[rstest]
#[tokio::test]
async fn test_format_tool(test_workspace: PathBuf) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;
    client.initialize()?;

    // Wait for rust-analyzer to be ready with smart polling
    wait_for_analyzer_ready(&mut client, &test_workspace).await?;

    // Format main.rs
    let response = timeout(Duration::from_secs(10), async {
        client.format("src/main.rs")
    })
    .await??;

    // Parse response
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let edits: Vec<Value> = serde_json::from_str(text.as_str().unwrap_or("[]"))?;

            // Code might already be formatted, so empty edits is OK
            // Just verify the response is valid
            assert!(
                edits.is_empty()
                    || edits
                        .iter()
                        .all(|e| e.get("range").is_some() && e.get("newText").is_some()),
                "Format response should be valid text edits"
            );
        }
    }

    Ok(())
}

#[rstest]
#[tokio::test]
async fn test_workspace_change(test_workspace: PathBuf) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;
    client.initialize()?;

    // Create a second workspace
    let second_workspace = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(second_workspace.path())?;

    // Change workspace
    let response = timeout(Duration::from_secs(10), async {
        client.set_workspace(second_workspace.path())
    })
    .await??;

    // Verify workspace change succeeded
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            assert!(
                text.as_str().unwrap_or("").contains("changed")
                    || text.as_str().unwrap_or("").contains("set"),
                "Workspace change should be acknowledged"
            );
        }
    }

    Ok(())
}

#[rstest]
#[case::invalid_file("non_existent.rs")]
#[case::invalid_path("../../../etc/passwd")]
#[tokio::test]
async fn test_error_handling_invalid_file(
    test_workspace: PathBuf,
    #[case] file_path: &str,
) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;
    client.initialize()?;

    // Try to get symbols for invalid file
    let result = client.get_symbols(file_path);

    // Should either error or return empty/null
    if let Ok(response) = result {
        if let Some(content) = response.get("content") {
            if let Some(text) = content[0].get("text") {
                let symbols: Vec<Value> =
                    serde_json::from_str(text.as_str().unwrap_or("[]")).unwrap_or_default();
                assert!(
                    symbols.is_empty(),
                    "Should not have symbols for invalid file"
                );
            }
        }
    }

    Ok(())
}

#[rstest]
#[case::negative_line(u32::MAX, 0)]
#[case::huge_column(0, 999999)]
#[case::both_invalid(u32::MAX, u32::MAX)]
#[tokio::test]
async fn test_error_handling_invalid_position(
    test_workspace: PathBuf,
    #[case] line: u32,
    #[case] character: u32,
) -> Result<()> {
    let mut client = MCPTestClient::start(&test_workspace)?;
    client.initialize()?;

    // Try to get definition at invalid position
    let result = client.get_definition("src/main.rs", line, character);

    // Should either error or return empty/null
    if let Ok(response) = result {
        if let Some(content) = response.get("content") {
            if let Some(text) = content[0].get("text") {
                let definitions: Vec<Value> =
                    serde_json::from_str(text.as_str().unwrap_or("[]")).unwrap_or_default();
                assert!(
                    definitions.is_empty(),
                    "Should not have definition at invalid position"
                );
            }
        }
    }

    Ok(())
}
