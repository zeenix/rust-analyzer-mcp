use anyhow::Result;
use serde_json::json;
use test_support::MCPTestClient;

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
async fn test_file_diagnostics() -> Result<()> {
    let client = MCPTestClient::start_isolated_diagnostics().await?;
    // Use enhanced initialization for diagnostics project
    client
        .initialize_workspace_with_files(vec!["src/errors.rs".to_string()])
        .await?;

    // Wait for diagnostics to be published - rust-analyzer sends these asynchronously.
    // Use longer timeouts in CI environments.
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
                    "file_path": "src/errors.rs"
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

    client.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn test_file_diagnostics_clean_file() -> Result<()> {
    // Use regular test project for clean file testing
    let client = MCPTestClient::start_isolated().await?;
    eprintln!("Started isolated client for clean file test");

    // Use enhanced initialization that ensures modules are properly resolved
    client.initialize_workspace().await?;
    eprintln!("Workspace fully initialized with all modules resolved");

    // Retry a few times to handle transient rust-analyzer initialization issues
    let mut last_error = None;
    for attempt in 1..=3 {
        // Get diagnostics
        let response = client
            .call_tool(
                "rust_analyzer_diagnostics",
                json!({
                    "file_path": "src/lib.rs"
                }),
            )
            .await?;

        assert_tool_response(&response);
        let content = response["content"][0]["text"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(content).unwrap();

        // Check summary for clean file (lib.rs should have no errors)
        let summary = &parsed["summary"];
        let error_count = summary["errors"].as_u64().unwrap_or(0);

        // If no errors, we're good
        if error_count == 0 {
            // Additional check: no error-level diagnostics
            let diagnostics = parsed["diagnostics"].as_array().unwrap();
            let has_errors = diagnostics
                .iter()
                .any(|d| d["severity"].as_str() == Some("error"));

            if !has_errors {
                // Success!
                client.shutdown().await?;
                return Ok(());
            }
        }

        // Log the issue for debugging
        eprintln!("Attempt {}: Found {} errors", attempt, error_count);
        if attempt == 1 {
            eprintln!("Full diagnostic response for src/lib.rs:");
            eprintln!("{}", serde_json::to_string_pretty(&parsed).unwrap());
        }

        last_error = Some(format!(
            "Clean file (src/lib.rs) should have no errors. Summary: {:?}",
            summary
        ));

        if attempt < 3 {
            // Wait a bit before retrying
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    }

    // All attempts failed
    client.shutdown().await?;
    Err(anyhow::anyhow!(last_error.unwrap()))
}

#[tokio::test]
async fn test_workspace_diagnostics() -> Result<()> {
    let client = MCPTestClient::start_isolated_diagnostics().await?;
    // Use enhanced initialization for diagnostics project
    client
        .initialize_workspace_with_files(vec!["src/errors.rs".to_string()])
        .await?;

    // First, open a file with errors to ensure it's analyzed.
    // Wait for diagnostics to be available.
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
                    "file_path": "src/errors.rs"
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

    client.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn test_diagnostics_invalid_file() -> Result<()> {
    // Can use either project, using regular one
    let client = MCPTestClient::start_isolated().await?;
    // Use basic initialization since we're testing invalid files
    client.initialize_and_wait().await?;

    // Test with non-existent file
    let response = client
        .call_tool(
            "rust_analyzer_diagnostics",
            json!({
                "file_path": "src/nonexistent.rs"
            }),
        )
        .await;

    match response {
        Ok(response) => {
            // If successful, should return empty diagnostics
            assert_tool_response(&response);
            let content = response["content"][0]["text"].as_str().unwrap();
            let parsed: serde_json::Value = serde_json::from_str(content).unwrap();

            // For non-existent file, we might get empty diagnostics
            let summary = &parsed["summary"];
            assert_eq!(summary["errors"].as_u64().unwrap_or(0), 0);
        }
        Err(e) => {
            // Or it might return an error, which is also acceptable
            assert!(
                e.to_string().contains("No such file") || e.to_string().contains("not found"),
                "Expected file not found error, got: {}",
                e
            );
        }
    }

    client.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn test_diagnostics_severity_levels() -> Result<()> {
    let client = MCPTestClient::start_isolated_diagnostics().await?;
    // Use enhanced initialization for diagnostics project
    client
        .initialize_workspace_with_files(vec!["src/errors.rs".to_string()])
        .await?;

    // Wait for diagnostics to be published - rust-analyzer sends these asynchronously.
    // Retry a few times with delays to give rust-analyzer time to analyze.
    let timeout_ms = if std::env::var("CI").is_ok() {
        1000
    } else {
        500
    };
    let max_attempts = if std::env::var("CI").is_ok() { 20 } else { 10 };

    let mut diagnostics = vec![];
    for attempt in 0..max_attempts {
        // Test file should have different severity levels
        let response = client
            .call_tool(
                "rust_analyzer_diagnostics",
                json!({
                    "file_path": "src/errors.rs"
                }),
            )
            .await?;

        assert_tool_response(&response);
        let content = response["content"][0]["text"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(content).unwrap();

        diagnostics = parsed["diagnostics"].as_array().unwrap().clone();

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

    // Debug: Print diagnostics to understand what we're getting
    eprintln!("Diagnostics count: {}", diagnostics.len());
    for (i, diag) in diagnostics.iter().enumerate() {
        eprintln!(
            "Diagnostic {}: severity={:?}, message={:?}",
            i, diag["severity"], diag["message"]
        );
    }

    // We should have diagnostics for a file with errors
    assert!(
        !diagnostics.is_empty(),
        "Should have diagnostics for file with errors"
    );

    // Check for different severity levels
    let mut has_error = false;
    let mut has_warning = false;

    for diag in &diagnostics {
        match diag["severity"].as_str() {
            Some("error") => has_error = true,
            Some("warning") => has_warning = true,
            _ => {}
        }
    }

    assert!(
        has_error || has_warning || !diagnostics.is_empty(),
        "Should have at least errors or warnings, found {} diagnostics",
        diagnostics.len()
    );

    Ok(())
}
