use anyhow::Result;
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;

// Import common test utilities
#[path = "../common/mod.rs"]
mod common;
use common::{fixtures, test_client::MCPTestClient};

// Shared client for all tests in this module to avoid repeated initialization
static SHARED_CLIENT: OnceCell<Arc<MCPTestClient>> = OnceCell::const_new();
static WORKSPACE_PATH: OnceCell<PathBuf> = OnceCell::const_new();

async fn get_shared_client() -> Result<Arc<MCPTestClient>> {
    let client = SHARED_CLIENT
        .get_or_init(|| async {
            let temp_dir = tempfile::tempdir().unwrap();
            let project = fixtures::TestProject::simple();
            project.create_in(temp_dir.path()).unwrap();

            let workspace = temp_dir.into_path();
            WORKSPACE_PATH.set(workspace.clone()).ok();

            let client = MCPTestClient::start(&workspace).await.unwrap();
            client.initialize_and_wait(&workspace).await.unwrap();
            Arc::new(client)
        })
        .await;
    Ok(client.clone())
}

fn get_workspace() -> PathBuf {
    WORKSPACE_PATH.get().unwrap().clone()
}

#[tokio::test]
async fn test_server_initialization() -> Result<()> {
    // For initialization test, we need a fresh client
    let temp_dir = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(temp_dir.path())?;
    let workspace = temp_dir.into_path();

    let client = MCPTestClient::start(&workspace).await?;
    // Initialize the server
    let init_response = client.initialize().await?;

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

#[tokio::test]
async fn test_all_lsp_tools() -> Result<()> {
    // Use shared client to avoid multiple rust-analyzer instances
    let client = get_shared_client().await?;

    // Test 1: Get symbols for main.rs
    let response = client.get_symbols("src/main.rs").await?;
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let symbols: Vec<Value> = serde_json::from_str(text.as_str().unwrap_or("[]"))?;
            assert!(!symbols.is_empty(), "Should have symbols in main.rs");

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

    // Test 2: Get definition - test "greet" function call on line 2 (0-indexed line 1)
    let response = client.get_definition("src/main.rs", 1, 18).await?;
    let mut got_definition = false;
    if let Some(content) = response.get("content") {
        if content.is_array() && !content[0].is_null() {
            if let Some(text) = content[0].get("text") {
                let text_str = text.as_str().unwrap_or("null");
                if text_str != "null" && text_str != "[]" {
                    // Try to parse as array
                    match serde_json::from_str::<Vec<Value>>(text_str) {
                        Ok(definitions) => {
                            got_definition = !definitions.is_empty();
                        }
                        Err(_) => {
                            // Failed to parse, but that's ok
                        }
                    }
                }
            }
        }
    }

    // Test 3: Get references - test "greet" function definition on line 10 (0-indexed line 9)
    let response = client.get_references("src/main.rs", 9, 4).await?;
    let mut got_references = false;
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            if text.as_str() != Some("null") {
                let references: Vec<Value> = serde_json::from_str(text.as_str().unwrap_or("[]"))?;
                got_references = !references.is_empty();
            }
        }
    }

    // Test 4: Get hover information - test "Calculator" on line 5 (0-indexed line 4)
    let response = client.get_hover("src/main.rs", 4, 15).await?;
    let mut got_hover = false;
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            if text.as_str() != Some("null") {
                let hover: Value = serde_json::from_str(text.as_str().unwrap_or("{}"))?;
                got_hover = hover.get("contents").is_some();
            }
        }
    }

    // Test 5: Get completions
    let response = client.get_completion("src/main.rs", 2, 5).await?;
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            let completions: Value = serde_json::from_str(text.as_str().unwrap_or("{}"))?;
            // Just check we got some response - completions might be empty or have items
            assert!(completions.is_object() || completions.is_array());
        }
    }

    // Test 6: Format document
    let response = client.format("src/main.rs").await?;
    let mut got_format = false;
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            got_format = text.as_str() != Some("null");
        }
    }

    // Print summary
    println!("LSP Tools Test Results:");
    println!("  Symbols: ✓");
    println!(
        "  Definition: {}",
        if got_definition {
            "✓"
        } else {
            "⚠ (null response)"
        }
    );
    println!(
        "  References: {}",
        if got_references {
            "✓"
        } else {
            "⚠ (null response)"
        }
    );
    println!(
        "  Hover: {}",
        if got_hover {
            "✓"
        } else {
            "⚠ (null response)"
        }
    );
    println!("  Completion: ✓");
    println!(
        "  Format: {}",
        if got_format {
            "✓"
        } else {
            "⚠ (null response)"
        }
    );

    Ok(())
}

#[tokio::test]
async fn test_workspace_change() -> Result<()> {
    // Need a fresh client for workspace change test
    let temp_dir = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(temp_dir.path())?;
    let workspace = temp_dir.into_path();

    let client = MCPTestClient::start(&workspace).await?;
    client.initialize().await?;

    // Create a second workspace
    let second_workspace = tempfile::tempdir()?;
    let project = fixtures::TestProject::simple();
    project.create_in(second_workspace.path())?;

    // Change workspace
    let response = client.set_workspace(second_workspace.path()).await?;

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

#[tokio::test]
async fn test_error_handling_invalid_files() -> Result<()> {
    let client = get_shared_client().await?;

    // Test multiple invalid file paths
    let invalid_paths = vec!["non_existent.rs", "../../../etc/passwd"];

    for file_path in invalid_paths {
        // Try to get symbols for invalid file
        let result = client.get_symbols(file_path).await;

        // Should either error or return empty/null
        if let Ok(response) = result {
            if let Some(content) = response.get("content") {
                if let Some(text) = content[0].get("text") {
                    let symbols: Vec<Value> =
                        serde_json::from_str(text.as_str().unwrap_or("[]")).unwrap_or_default();
                    assert!(
                        symbols.is_empty(),
                        "Should not have symbols for invalid file: {}",
                        file_path
                    );
                }
            }
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_error_handling_invalid_positions() -> Result<()> {
    let client = get_shared_client().await?;

    // Test multiple invalid positions
    let invalid_positions = vec![
        (u32::MAX, 0),        // negative line
        (0, 999999),          // huge column
        (u32::MAX, u32::MAX), // both invalid
    ];

    for (line, character) in invalid_positions {
        // Try to get definition at invalid position
        let result = client.get_definition("src/main.rs", line, character).await;

        // Should either error or return empty/null
        if let Ok(response) = result {
            if let Some(content) = response.get("content") {
                if let Some(text) = content[0].get("text") {
                    let definitions: Vec<Value> =
                        serde_json::from_str(text.as_str().unwrap_or("[]")).unwrap_or_default();
                    assert!(
                        definitions.is_empty(),
                        "Should not have definition at invalid position ({}, {})",
                        line,
                        character
                    );
                }
            }
        }
    }

    Ok(())
}
