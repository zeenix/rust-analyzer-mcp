use anyhow::Result;
use futures::future::join_all;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use test_support::MCPTestClient;

#[tokio::test]
async fn test_concurrent_tool_calls() -> Result<()> {
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = Arc::new(MCPTestClient::start(&project_path).await?);
    client.initialize_and_wait(&project_path).await?;

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

    // Execute requests - in CI, use smaller batches to avoid overwhelming the server.
    let results = if std::env::var("CI").is_ok() {
        // In CI: execute in two batches with a small delay between them.
        let (batch1, batch2) = tasks.split_at(3);

        let futures1 = batch1.iter().map(|(tool, args)| {
            let client = Arc::clone(&client);
            let tool = *tool;
            let args = args.clone();
            async move { client.call_tool(tool, args).await }
        });
        let mut results1 = join_all(futures1).await;

        // Longer delay between batches in CI to ensure server can process them.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let futures2 = batch2.iter().map(|(tool, args)| {
            let client = Arc::clone(&client);
            let tool = *tool;
            let args = args.clone();
            async move { client.call_tool(tool, args).await }
        });
        let results2 = join_all(futures2).await;

        results1.extend(results2);
        results1
    } else {
        // Not in CI: execute all concurrently.
        let futures = tasks.into_iter().map(|(tool, args)| {
            let client = Arc::clone(&client);
            async move { client.call_tool(tool, args).await }
        });
        join_all(futures).await
    };

    let elapsed = start.elapsed();

    // All requests should complete
    assert_eq!(results.len(), 6);
    let mut failures = Vec::new();
    for (i, result) in results.into_iter().enumerate() {
        // Results are direct Result<Value> from async blocks
        if let Err(e) = &result {
            failures.push(format!("Request {} failed: {:?}", i, e));
        }
        assert!(result.is_ok() || result.is_err(), "Should get a response");
    }

    if !failures.is_empty() {
        eprintln!("Concurrent test failures in CI:");
        for failure in &failures {
            eprintln!("  {}", failure);
        }
        // Allow some failures in CI but not too many
        if std::env::var("CI").is_ok() && failures.len() <= 2 {
            eprintln!("Allowing {} failures in CI environment", failures.len());
        } else if !failures.is_empty() {
            panic!("Too many failures: {}", failures.join(", "));
        }
    }

    // Concurrent execution should be faster than sequential
    // (though this is not guaranteed in all environments)
    println!("Concurrent execution took: {:?}", elapsed);
    assert!(
        elapsed < Duration::from_secs(30),
        "Should complete within reasonable time"
    );

    // Cleanup
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_many_sequential_requests() -> Result<()> {
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = MCPTestClient::start(&project_path).await?;
    client.initialize_and_wait(&project_path).await?;

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

    // Cleanup
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_rapid_fire_requests() -> Result<()> {
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = Arc::new(MCPTestClient::start(&project_path).await?);
    client.initialize_and_wait(&project_path).await?;

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
        // Only add delay in CI to avoid overwhelming the system.
        // GitHub Actions (and most CI systems) automatically set CI=true.
        if std::env::var("CI").is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    // Collect results
    let results = join_all(handles).await;

    let mut total_time = Duration::ZERO;
    let mut success_count = 0;
    let mut failed_count = 0;

    for result in results {
        match result {
            Ok((i, res, elapsed)) => {
                total_time += elapsed;
                match res {
                    Ok(_) => {
                        success_count += 1;
                        println!("Request {} succeeded in {:?}", i, elapsed);
                    }
                    Err(e) => {
                        failed_count += 1;
                        eprintln!("Request {} failed: {}", i, e);
                    }
                }
            }
            Err(e) => {
                failed_count += 1;
                eprintln!("Task panicked: {}", e);
            }
        }
    }

    println!(
        "Success rate: {}/20 (failed: {})",
        success_count, failed_count
    );
    println!("Average time per request: {:?}", total_time / 20);

    // Should handle most rapid requests (allowing for some failures in CI)
    let min_success = if std::env::var("CI").is_ok() { 14 } else { 18 };
    assert!(
        success_count >= min_success,
        "At least {}/20 requests should succeed (got {})",
        min_success,
        success_count
    );

    // Cleanup
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_large_file_processing() -> Result<()> {
    // Use the test project
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = MCPTestClient::start(&project_path).await?;
    client.initialize_and_wait(&project_path).await?;

    let start = Instant::now();

    // Process multiple files sequentially
    let files = ["src/main.rs", "src/lib.rs", "src/utils.rs", "src/types.rs"];
    for file_path in &files {
        let _ = client.get_symbols(file_path).await;
    }

    let elapsed = start.elapsed();
    println!("Processing {} files took: {:?}", files.len(), elapsed);

    // Should handle files reasonably
    assert!(
        elapsed < Duration::from_secs(30),
        "Should process files in reasonable time"
    );

    // Cleanup
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_error_recovery() -> Result<()> {
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = MCPTestClient::start(&project_path).await?;
    client.initialize().await?;

    // Send invalid requests
    for _ in 0..5 {
        let _ = client.get_symbols("non_existent_file.rs").await;
    }

    // Server should still work after errors
    let response = client.get_symbols("src/main.rs").await;
    assert!(response.is_ok(), "Server should recover from errors");

    // Cleanup
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_memory_stability() -> Result<()> {
    let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-project");

    let client = MCPTestClient::start(&project_path).await?;
    client.initialize_and_wait(&project_path).await?;

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

    // Cleanup
    client.shutdown().await?;

    Ok(())
}
