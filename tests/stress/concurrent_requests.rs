use anyhow::Result;
use futures::future::join_all;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use test_support::{is_ci, timeouts, MCPTestClient};

#[tokio::test]
async fn test_concurrent_tool_calls() -> Result<()> {
    let client = Arc::new(MCPTestClient::start_isolated().await?);
    client.initialize_and_wait().await?;

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
    let results = if is_ci() {
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
        tokio::time::sleep(timeouts::batch_delay()).await;

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

    // All requests should complete successfully
    assert_eq!(results.len(), 6);
    for (i, result) in results.into_iter().enumerate() {
        assert!(result.is_ok(), "Request {} failed: {:?}", i, result.err());
    }

    // Concurrent execution should be faster than sequential
    // (though this is not guaranteed in all environments)
    println!("Concurrent execution took: {:?}", elapsed);
    let timeout = timeouts::stress_timeout(timeouts::STRESS_CONCURRENT_BASE_SECS);
    assert!(
        elapsed < timeout,
        "Should complete within {:?} (got {:?})",
        timeout,
        elapsed
    );

    // Cleanup
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_sequential_throughput() -> Result<()> {
    let client = MCPTestClient::start_isolated().await?;
    client.initialize_and_wait().await?;

    let start = Instant::now();

    // Send several requests sequentially to test throughput
    for i in 0..10 {
        let result = client.get_symbols("src/main.rs").await;
        assert!(result.is_ok(), "Request {} failed: {:?}", i, result.err());
    }

    let elapsed = start.elapsed();
    println!("10 sequential requests took: {:?}", elapsed);

    // Should handle many requests without degradation
    let timeout = timeouts::stress_timeout(timeouts::STRESS_SEQUENTIAL_BASE_SECS);
    assert!(
        elapsed < timeout,
        "Should handle many requests efficiently within {:?} (got {:?})",
        timeout,
        elapsed
    );

    // Cleanup
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_rapid_fire_requests() -> Result<()> {
    let client = Arc::new(MCPTestClient::start_isolated().await?);
    client.initialize_and_wait().await?;

    // Send requests concurrently without delays
    let request_count = 10;

    let mut handles = vec![];
    for i in 0..request_count {
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
        "Success rate: {}/{} (failed: {})",
        success_count, request_count, failed_count
    );
    println!(
        "Average time per request: {:?}",
        total_time / request_count as u32
    );

    // All rapid requests should succeed
    assert_eq!(
        success_count, request_count,
        "All {} requests should succeed (got {} successes, {} failures)",
        request_count, success_count, failed_count
    );

    // Cleanup
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_large_file_processing() -> Result<()> {
    let client = MCPTestClient::start_isolated().await?;
    client.initialize_and_wait().await?;

    let start = Instant::now();

    // Process multiple files sequentially
    let files = ["src/main.rs", "src/lib.rs", "src/utils.rs", "src/types.rs"];
    for file_path in &files {
        let _ = client.get_symbols(file_path).await;
    }

    let elapsed = start.elapsed();
    println!("Processing {} files took: {:?}", files.len(), elapsed);

    // Should handle files reasonably
    let timeout = timeouts::stress_timeout(timeouts::STRESS_FILES_BASE_SECS);
    assert!(
        elapsed < timeout,
        "Should process files within {:?} (got {:?})",
        timeout,
        elapsed
    );

    // Cleanup
    client.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn test_error_recovery() -> Result<()> {
    let client = MCPTestClient::start_isolated().await?;
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
async fn test_concurrent_mixed_requests() -> Result<()> {
    let client = Arc::new(MCPTestClient::start_isolated().await?);
    client.initialize_and_wait().await?;

    // Test memory stability with a single iteration of concurrent requests
    // Run mixed request types concurrently
    let mut tasks = vec![];

    // Create 3 sets of concurrent requests (9 total)
    for _ in 0..3 {
        let c1 = Arc::clone(&client);
        tasks.push(tokio::spawn(
            async move { c1.get_symbols("src/main.rs").await },
        ));

        let c2 = Arc::clone(&client);
        tasks.push(tokio::spawn(async move {
            c2.get_hover("src/main.rs", 1, 10).await
        }));

        let c3 = Arc::clone(&client);
        tasks.push(tokio::spawn(async move {
            c3.get_completion("src/main.rs", 2, 5).await
        }));
    }

    // Execute all requests concurrently
    let results = join_all(tasks).await;

    // Verify all requests succeeded
    for (i, result) in results.into_iter().enumerate() {
        assert!(result.is_ok(), "Task {} panicked: {:?}", i, result.err());
        let inner_result = result.unwrap();
        assert!(
            inner_result.is_ok(),
            "Request {} failed: {:?}",
            i,
            inner_result.err()
        );
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
