use anyhow::Result;
use serde_json::json;
use test_support::IpcClient;

fn assert_tool_response(response: &serde_json::Value) {
    assert!(
        response["error"].is_null(),
        "Tool call returned error: {:?}",
        response["error"]
    );
    assert!(
        response["content"].is_array(),
        "Response should have content array"
    );
    assert!(
        !response["content"].as_array().unwrap().is_empty(),
        "Content array should not be empty"
    );
}

#[tokio::test]
async fn test_file_diagnostics_ipc() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project-diagnostics").await?;
    let workspace_path = client.workspace_path();
    let errors_path = workspace_path.join("src/errors.rs");

    // Wait for diagnostics to be published - rust-analyzer sends these asynchronously
    let timeout_ms = if std::env::var("CI").is_ok() {
        1000
    } else {
        500
    };
    let max_attempts = if std::env::var("CI").is_ok() { 30 } else { 20 };

    let mut parsed = serde_json::Value::Null;
    for attempt in 0..max_attempts {
        // Test getting diagnostics for the test file with errors
        let response = client
            .call_tool(
                "rust_analyzer_diagnostics",
                json!({
                    "file_path": errors_path.to_str().unwrap()
                }),
            )
            .await?;

        assert_tool_response(&response);
        let content = response["content"][0]["text"].as_str().unwrap();
        parsed = serde_json::from_str(content).unwrap();

        let diagnostics = parsed["diagnostics"].as_array().unwrap();
        if !diagnostics.is_empty() {
            break;
        }

        if attempt < max_attempts - 1 {
            eprintln!(
                "Attempt {}: No diagnostics yet, waiting for rust-analyzer...",
                attempt + 1
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(timeout_ms)).await;
        }
    }

    // Check that we have diagnostics
    assert!(parsed["diagnostics"].is_array());
    let diagnostics = parsed["diagnostics"].as_array().unwrap();

    // We should get diagnostics for this file with intentional errors
    assert!(
        !diagnostics.is_empty(),
        "Should have diagnostics for file with errors. Got: {}",
        serde_json::to_string_pretty(&parsed).unwrap()
    );

    // Check summary - we should have at least some diagnostics
    let summary = &parsed["summary"];
    let error_count = summary["errors"].as_u64().unwrap_or(0);
    let warning_count = summary["warnings"].as_u64().unwrap_or(0);
    let hint_count = summary["hints"].as_u64().unwrap_or(0);

    assert!(
        error_count > 0 || warning_count > 0 || hint_count > 0,
        "Should have at least some diagnostics (errors, warnings, or hints). Summary: {:?}",
        summary
    );

    // Check that diagnostic structure is correct
    if !diagnostics.is_empty() {
        let first_diag = &diagnostics[0];
        assert!(first_diag["severity"].is_string());
        assert!(first_diag["message"].is_string());
        assert!(first_diag["range"].is_object());
    }

    Ok(())
}

#[tokio::test]
async fn test_workspace_diagnostics_ipc() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project-diagnostics").await?;
    let workspace_path = client.workspace_path();
    let errors_path = workspace_path.join("src/errors.rs");

    // First, ensure diagnostics are ready for a file
    let timeout_ms = if std::env::var("CI").is_ok() {
        1000
    } else {
        500
    };
    let max_attempts = if std::env::var("CI").is_ok() { 20 } else { 10 };

    for attempt in 0..max_attempts {
        let response = client
            .call_tool(
                "rust_analyzer_diagnostics",
                json!({
                    "file_path": errors_path.to_str().unwrap()
                }),
            )
            .await?;

        let content = response["content"][0]["text"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(content).unwrap();
        let diagnostics = parsed["diagnostics"].as_array().unwrap();

        if !diagnostics.is_empty() {
            break;
        }

        if attempt < max_attempts - 1 {
            eprintln!(
                "Attempt {}: Waiting for initial diagnostics...",
                attempt + 1
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(timeout_ms)).await;
        }
    }

    // Now get workspace diagnostics
    let response = client
        .call_tool("rust_analyzer_workspace_diagnostics", json!({}))
        .await?;

    assert_tool_response(&response);
    let content = response["content"][0]["text"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(content).unwrap();

    // Check that we have workspace info
    assert!(parsed["workspace"].is_string());

    // Check structure based on response format
    if parsed["files"].is_object() {
        // Fallback format
        assert!(parsed["summary"]["total_files"].is_number());
    } else if parsed["diagnostics"].is_array() {
        // Proper workspace diagnostic format
        assert!(parsed["summary"]["total_diagnostics"].is_number());
    }

    Ok(())
}
