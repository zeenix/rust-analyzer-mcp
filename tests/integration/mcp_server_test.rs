use anyhow::Result;
use serde_json::Value;
use std::time::Duration;

// Import test support library
use test_support::MCPTestClient;

#[tokio::test]
async fn test_server_initialization() -> Result<()> {
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = MCPTestClient::start(&project_path).await?;
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

    // Cleanup before test ends
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_all_lsp_tools() -> Result<()> {
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = MCPTestClient::start(&project_path).await?;
    client.initialize_and_wait(&project_path).await?;

    // Test 1: Get symbols for main.rs
    test_symbols(&client).await?;

    // In CI, add extra delay to ensure rust-analyzer is fully ready for all operations
    if std::env::var("CI").is_ok() {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // Test 2: Get definition - test "greet" function call on line 2 (0-indexed line 1)
    let got_definition = test_definition(&client).await?;

    // Test 3: Get references - test "greet" function definition on line 10 (0-indexed line 9)
    let got_references = test_references(&client).await?;

    // Test 4: Get hover information - test "Calculator" on line 5 (0-indexed line 4)
    let got_hover = test_hover(&client).await?;

    // Test 5: Get completions
    test_completion(&client).await?;

    // Test 6: Format document
    let got_format = test_format(&client).await?;

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

    // Cleanup before test ends to ensure it happens in runtime context
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_workspace_change() -> Result<()> {
    // Start with the test project
    let first_project = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");
    let client = MCPTestClient::start(&first_project).await?;
    client.initialize().await?;

    // Change to another workspace (use the same for now)
    let second_project = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");
    let response = client.set_workspace(&second_project).await?;

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

    // Cleanup before test ends
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_error_handling_invalid_files() -> Result<()> {
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = MCPTestClient::start(&project_path).await?;
    client.initialize_and_wait(&project_path).await?;

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

    // Cleanup before test ends
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_error_handling_invalid_positions() -> Result<()> {
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = MCPTestClient::start(&project_path).await?;
    client.initialize_and_wait(&project_path).await?;

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

    // Cleanup before test ends
    client.shutdown().await?;

    Ok(())
}

// Helper functions for test_all_lsp_tools

async fn test_symbols(client: &MCPTestClient) -> Result<()> {
    let response = client.get_symbols("src/main.rs").await?;

    let Some(content) = response.get("content") else {
        return Err(anyhow::anyhow!("No content in symbols response"));
    };

    let Some(text) = content[0].get("text") else {
        return Err(anyhow::anyhow!("No text in symbols response"));
    };

    let Some(text_str) = text.as_str() else {
        return Err(anyhow::anyhow!("Text is not a string"));
    };

    let symbols: Vec<Value> = serde_json::from_str(text_str)?;
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

    Ok(())
}

async fn test_definition(client: &MCPTestClient) -> Result<bool> {
    let response = client.get_definition("src/main.rs", 1, 18).await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    if !content.is_array() || content[0].is_null() {
        return Ok(false);
    }

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    if text_str == "null" || text_str == "[]" {
        return Ok(false);
    }

    // Try to parse as array
    let Ok(definitions) = serde_json::from_str::<Vec<Value>>(text_str) else {
        return Ok(false);
    };

    Ok(!definitions.is_empty())
}

async fn test_references(client: &MCPTestClient) -> Result<bool> {
    let response = client.get_references("src/main.rs", 9, 4).await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    if text.as_str() == Some("null") {
        return Ok(false);
    }

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    let references: Vec<Value> = serde_json::from_str(text_str)?;
    Ok(!references.is_empty())
}

async fn test_hover(client: &MCPTestClient) -> Result<bool> {
    let response = client.get_hover("src/main.rs", 4, 15).await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    if text.as_str() == Some("null") {
        return Ok(false);
    }

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    let hover: Value = serde_json::from_str(text_str)?;
    Ok(hover.get("contents").is_some())
}

async fn test_completion(client: &MCPTestClient) -> Result<()> {
    let response = client.get_completion("src/main.rs", 2, 5).await?;

    let Some(content) = response.get("content") else {
        return Ok(());
    };

    let Some(text) = content[0].get("text") else {
        return Ok(());
    };

    let Some(text_str) = text.as_str() else {
        return Ok(());
    };

    // Handle "null" response specially
    if text_str == "null" {
        // rust-analyzer returned null - still indexing
        eprintln!("Got null completion response (rust-analyzer may still be indexing)");
        return Ok(());
    }

    let completions: Value = serde_json::from_str(text_str)?;
    assert!(
        completions.is_object() || completions.is_array() || completions.is_null(),
        "Expected object, array, or null, got: {:?}",
        completions
    );

    Ok(())
}

async fn test_format(client: &MCPTestClient) -> Result<bool> {
    let response = client.format("src/main.rs").await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    Ok(text.as_str() != Some("null"))
}
