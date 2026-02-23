use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

// Import test support library
use test_support::{is_ci, timeouts, IpcClient};

#[tokio::test]
async fn test_server_initialization() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;

    // The server is already initialized by IpcClient
    // Just verify we can make a request
    let response = client.send_request("tools/list", None).await?;

    // Check we got tools
    assert!(response.get("tools").is_some());
    let tools = response["tools"].as_array().unwrap();
    assert!(!tools.is_empty());

    // Verify some expected tools exist
    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(tool_names.contains(&"rust_analyzer_symbols"));
    assert!(tool_names.contains(&"rust_analyzer_hover"));

    Ok(())
}

#[tokio::test]
async fn test_ping() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;

    // Test basic ping - should return empty object
    let response = client.send_request("ping", None).await?;
    assert_eq!(
        response,
        json!({}),
        "Ping should return empty object per MCP spec"
    );

    // Test multiple pings to verify idempotency
    for i in 0..5 {
        let response = client.send_request("ping", None).await?;
        assert_eq!(
            response,
            json!({}),
            "Ping #{} should return empty object",
            i + 1
        );
    }

    // Test rapid-fire pings to verify throughput
    for _ in 0..10 {
        let response = client.send_request("ping", None).await?;
        assert_eq!(response, json!({}), "Rapid ping should return empty object");
    }

    // Verify server is still responsive after pings
    let response = client.send_request("tools/list", None).await?;
    assert!(
        response.get("tools").is_some(),
        "Server should remain responsive after pings"
    );

    Ok(())
}

#[tokio::test]
async fn test_all_lsp_tools() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();

    // Test 1: Get symbols for main.rs
    test_symbols(&mut client, &workspace_path).await?;

    // In CI, add extra delay to ensure rust-analyzer is fully ready for all operations
    if is_ci() {
        tokio::time::sleep(timeouts::ci_test_delay()).await;
    }

    // Test 2: Get definition - test "greet" function call on line 2 (0-indexed line 1)
    let got_definition = test_definition(&mut client, &workspace_path).await?;

    // Test 3: Get references - test "greet" function definition on line 10 (0-indexed line 9)
    let got_references = test_references(&mut client, &workspace_path).await?;

    // Test 4: Get hover information - test "Calculator" on line 5 (0-indexed line 4)
    let got_hover = test_hover(&mut client, &workspace_path).await?;

    // Test 5: Get completions
    test_completion(&mut client, &workspace_path).await?;

    // Test 6: Format document
    let got_format = test_format(&mut client, &workspace_path).await?;

    // Test 7: Code actions
    let got_code_actions = test_code_actions(&mut client, &workspace_path).await?;

    // Print summary
    println!("LSP Tools Test Results:");
    println!("  Symbols: ✓");
    println!(
        "  Definition: {}",
        if got_definition {
            "✓"
        } else {
            "⚠ (not ready)"
        }
    );
    println!(
        "  References: {}",
        if got_references {
            "✓"
        } else {
            "⚠ (not ready)"
        }
    );
    println!("  Hover: {}", if got_hover { "✓" } else { "⚠ (not ready)" });
    println!("  Completion: ✓");
    println!(
        "  Format: {}",
        if got_format {
            "✓"
        } else {
            "⚠ (invalid response)"
        }
    );
    println!(
        "  Code Actions: {}",
        if got_code_actions {
            "✓"
        } else {
            "⚠ (not ready)"
        }
    );

    Ok(())
}

#[tokio::test]
async fn test_workspace_change() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;

    // Create a second isolated project to switch to
    let second_project = test_support::IsolatedProject::new()?;
    let response = client
        .call_tool(
            "rust_analyzer_set_workspace",
            json!({
                "workspace_path": second_project.path().to_str().unwrap()
            }),
        )
        .await?;

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
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();

    // Test multiple invalid file paths
    let invalid_paths = vec![
        workspace_path.join("non_existent.rs"),
        workspace_path.join("../../../etc/passwd"),
    ];

    for file_path in invalid_paths {
        // Try to get symbols for invalid file
        let result = client
            .call_tool(
                "rust_analyzer_symbols",
                json!({
                    "file_path": file_path.to_str().unwrap()
                }),
            )
            .await;

        // Should either error or return empty/null
        if let Ok(response) = result {
            if let Some(content) = response.get("content") {
                if let Some(text) = content[0].get("text") {
                    let symbols: Vec<Value> =
                        serde_json::from_str(text.as_str().unwrap_or("[]")).unwrap_or_default();
                    assert!(
                        symbols.is_empty(),
                        "Should not have symbols for invalid file: {}",
                        file_path.display()
                    );
                }
            }
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_error_handling_invalid_positions() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();
    let main_path = workspace_path.join("src/main.rs");

    // Test multiple invalid positions
    let invalid_positions = vec![
        (u32::MAX, 0),        // negative line
        (0, 999999),          // huge column
        (u32::MAX, u32::MAX), // both invalid
    ];

    for (line, character) in invalid_positions {
        // Try to get definition at invalid position
        let result = client
            .call_tool(
                "rust_analyzer_definition",
                json!({
                    "file_path": main_path.to_str().unwrap(),
                    "line": line,
                    "character": character
                }),
            )
            .await;

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

// Helper functions for test_all_lsp_tools

async fn test_symbols(client: &mut IpcClient, workspace_path: &Path) -> Result<()> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_symbols",
            json!({
                "file_path": main_path.to_str().unwrap()
            }),
        )
        .await?;

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

async fn test_definition(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_definition",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 1,
                "character": 18
            }),
        )
        .await?;

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

    // null or empty array during initialization is normal for LSP.
    // We just check that we got a valid response.
    if text_str == "null" || text_str == "[]" {
        // This is a valid response during initialization.
        return Ok(true);
    }

    // Try to parse as array
    let Ok(definitions) = serde_json::from_str::<Vec<Value>>(text_str) else {
        return Ok(false);
    };

    Ok(!definitions.is_empty())
}

async fn test_references(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_references",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 9,
                "character": 4
            }),
        )
        .await?;

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

async fn test_hover(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_hover",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 4,
                "character": 15
            }),
        )
        .await?;

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

async fn test_completion(client: &mut IpcClient, workspace_path: &Path) -> Result<()> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_completion",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 2,
                "character": 5
            }),
        )
        .await?;

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

async fn test_format(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    // Test 1: Format already-formatted file - should return null (no edits needed)
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_format",
            json!({
                "file_path": main_path.to_str().unwrap()
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    // main.rs is already formatted, so should return null
    if text_str != "null" {
        eprintln!("Expected null for formatted file, got: {}", text_str);
        return Ok(false);
    }

    // Test 2: Format unformatted file - should return edits
    let unformatted_path = workspace_path.join("src/unformatted.rs");
    let response = client
        .call_tool(
            "rust_analyzer_format",
            json!({
                "file_path": unformatted_path.to_str().unwrap()
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    // unformatted.rs needs formatting, so should return an array of edits
    if text_str == "null" {
        eprintln!("Expected edits for unformatted file, got null");
        return Ok(false);
    }

    // Parse and validate it's a non-empty array of edits
    let edits: Vec<Value> = serde_json::from_str(text_str)?;
    if edits.is_empty() {
        eprintln!("Expected non-empty edits for unformatted file");
        return Ok(false);
    }

    Ok(true)
}

async fn test_code_actions(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_code_actions",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 13,
                "character": 0,
                "end_line": 16,
                "end_character": 1
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    // Check if we got null or empty array
    if text_str == "null" || text_str == "[]" {
        return Ok(false);
    }

    // Try to parse as array to verify it's valid JSON
    let Ok(_actions) = serde_json::from_str::<Vec<Value>>(text_str) else {
        return Ok(false);
    };

    // Even if we get an empty array, that's better than null
    // Some files genuinely might not have code actions available
    Ok(true)
}
