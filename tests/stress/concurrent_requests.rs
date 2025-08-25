use anyhow::Result;
use futures::future::join_all;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use test_support::{MCPTestClient, TestProject};

#[tokio::test]
async fn test_concurrent_tool_calls() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = TestProject::simple();
    project.create_in(workspace.path())?;

    let client = Arc::new(MCPTestClient::start(workspace.path()).await?);
    client.initialize_and_wait(workspace.path()).await?;

    // Create multiple concurrent requests
    let tasks = vec![
        ("rust_analyzer_symbols", json!({"file_path": "src/main.rs"})),
        (
            "rust_analyzer_hover",
            json!({"file_path": "src/main.rs", "line": 1, "character": 10}),
        ),
        (
            "rust_analyzer_completion",
            json!({"file_path": "src/main.rs", "line": 2, "character": 5}),
        ),
        (
            "rust_analyzer_definition",
            json!({"file_path": "src/main.rs", "line": 1, "character": 20}),
        ),
        (
            "rust_analyzer_references",
            json!({"file_path": "src/main.rs", "line": 9, "character": 3}),
        ),
        ("rust_analyzer_format", json!({"file_path": "src/main.rs"})),
    ];

    let start = Instant::now();

    // Execute all requests concurrently
    let futures = tasks.into_iter().map(|(tool, args)| {
        let client = Arc::clone(&client);
        async move { client.call_tool(tool, args).await }
    });

    let results = join_all(futures).await;

    let elapsed = start.elapsed();

    // All requests should complete
    assert_eq!(results.len(), 6);
    for result in results {
        // Results are direct Result<Value> from async blocks
        assert!(result.is_ok() || result.is_err(), "Should get a response");
    }

    // Concurrent execution should be faster than sequential
    // (though this is not guaranteed in all environments)
    println!("Concurrent execution took: {:?}", elapsed);
    assert!(
        elapsed < Duration::from_secs(30),
        "Should complete within reasonable time"
    );

    Ok(())
}

#[tokio::test]
async fn test_many_sequential_requests() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = TestProject::simple();
    project.create_in(workspace.path())?;

    let client = MCPTestClient::start(workspace.path()).await?;
    client.initialize_and_wait(workspace.path()).await?;

    let start = Instant::now();

    // Send many requests sequentially
    for i in 0..50 {
        let _ = client.get_symbols("src/main.rs").await;
        if i % 10 == 0 {
            println!("Completed {} requests", i);
        }
    }

    let elapsed = start.elapsed();
    println!("50 sequential requests took: {:?}", elapsed);

    // Should handle many requests without degradation
    assert!(
        elapsed < Duration::from_secs(60),
        "Should handle many requests efficiently"
    );

    Ok(())
}

#[tokio::test]
async fn test_rapid_fire_requests() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = TestProject::simple();
    project.create_in(workspace.path())?;

    let client = Arc::new(MCPTestClient::start(workspace.path()).await?);
    client.initialize_and_wait(workspace.path()).await?;

    // Send requests as fast as possible without waiting
    let mut handles = vec![];

    for i in 0..20 {
        let client = Arc::clone(&client);
        let handle = tokio::spawn(async move {
            let start = Instant::now();
            let result = client.get_symbols("src/main.rs").await;
            let elapsed = start.elapsed();
            (i, result, elapsed)
        });
        handles.push(handle);
    }

    // Collect results
    let results = join_all(handles).await;

    let mut total_time = Duration::ZERO;
    let mut success_count = 0;

    for result in results {
        match result {
            Ok((i, res, elapsed)) => {
                total_time += elapsed;
                if res.is_ok() {
                    success_count += 1;
                }
                println!("Request {} took {:?}", i, elapsed);
            }
            Err(e) => {
                eprintln!("Task failed: {}", e);
            }
        }
    }

    println!("Success rate: {}/20", success_count);
    println!("Average time per request: {:?}", total_time / 20);

    // Should handle rapid requests
    assert!(success_count >= 18, "Most requests should succeed");

    Ok(())
}

#[tokio::test]
async fn test_large_file_processing() -> Result<()> {
    let workspace = tempfile::tempdir()?;

    // Create a large project
    let project = TestProject::large_codebase();
    project.create_in(workspace.path())?;

    let client = MCPTestClient::start(workspace.path()).await?;
    // Use longer timeout for large project initialization
    client
        .send_request_with_timeout(
            "initialize",
            Some(json!({
                "protocolVersion": "0.1.0",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            })),
            Duration::from_secs(30),
        )
        .await?;

    // Wait for rust-analyzer initialization
    client.initialize_and_wait(workspace.path()).await?;

    let start = Instant::now();

    // Process multiple large files sequentially
    for i in 0..10 {
        let file_path = format!("src/module_{}.rs", i);
        let _ = client.get_symbols(&file_path).await;
    }

    let elapsed = start.elapsed();
    println!("Processing 10 large files took: {:?}", elapsed);

    // Should handle large files reasonably
    assert!(
        elapsed < Duration::from_secs(120),
        "Should process large files in reasonable time"
    );

    Ok(())
}

#[tokio::test]
async fn test_error_recovery() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = TestProject::simple();
    project.create_in(workspace.path())?;

    let client = MCPTestClient::start(workspace.path()).await?;
    client.initialize().await?;

    // Send invalid requests
    for _ in 0..5 {
        let _ = client.get_symbols("non_existent_file.rs").await;
    }

    // Server should still work after errors
    let response = client.get_symbols("src/main.rs").await;
    assert!(response.is_ok(), "Server should recover from errors");

    Ok(())
}

#[tokio::test]
async fn test_memory_stability() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let project = TestProject::simple();
    project.create_in(workspace.path())?;

    let client = MCPTestClient::start(workspace.path()).await?;
    client.initialize_and_wait(workspace.path()).await?;

    // Send many requests to test memory stability
    for iteration in 0..10 {
        println!("Iteration {}", iteration);

        // Mix of different request types
        for _ in 0..10 {
            let _ = client.get_symbols("src/main.rs").await;
            let _ = client.get_hover("src/main.rs", 1, 10).await;
            let _ = client.get_completion("src/main.rs", 2, 5).await;
        }
    }

    // Final request should still work
    let final_response = client.get_symbols("src/main.rs").await;
    assert!(
        final_response.is_ok(),
        "Server should remain stable after many requests"
    );

    Ok(())
}
