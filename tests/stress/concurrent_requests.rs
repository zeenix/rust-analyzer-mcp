use anyhow::Result;
use futures::future::join_all;
use serde_json::json;
use std::time::{Duration, Instant};

use test_support::{is_ci, timeouts, IpcClient};

// ============================================================================
// Ping Stress Tests
// ============================================================================

#[tokio::test]
async fn test_ping_rapid_fire() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;

    let iterations = if is_ci() { 100 } else { 500 };
    let start = Instant::now();

    for _ in 0..iterations {
        let response = client.send_request("ping", None).await?;
        assert_eq!(response, json!({}), "Ping should return empty object");
    }

    let elapsed = start.elapsed();
    let rate = iterations as f64 / elapsed.as_secs_f64();
    eprintln!(
        "Ping rapid-fire: {} pings in {:?} ({:.1} pings/s)",
        iterations, elapsed, rate
    );

    Ok(())
}

#[tokio::test]
async fn test_ping_concurrent() -> Result<()> {
    let concurrency = if is_ci() { 10 } else { 50 };
    let start = Instant::now();

    let futures = (0..concurrency).map(|_| async {
        let mut client = IpcClient::get_or_create("test-project").await?;
        client.send_request("ping", None).await
    });

    let results = join_all(futures).await;

    let elapsed = start.elapsed();
    eprintln!(
        "Ping concurrent: {} concurrent pings in {:?}",
        concurrency, elapsed
    );

    // Verify all succeeded with correct response
    for result in results {
        let response = result?;
        assert_eq!(response, json!({}), "Ping should return empty object");
    }

    Ok(())
}

#[tokio::test]
async fn test_ping_interleaved_with_tools() -> Result<()> {
    let temp_client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = temp_client.workspace_path().to_path_buf();
    drop(temp_client);

    let main_path = workspace_path.join("src/main.rs");
    let main_path_str = main_path.to_str().unwrap();

    let mut client = IpcClient::get_or_create("test-project").await?;
    let iterations = if is_ci() { 20 } else { 50 };

    for i in 0..iterations {
        // Alternate between ping and actual tool calls
        if i % 3 == 0 {
            let response = client.send_request("ping", None).await?;
            assert_eq!(response, json!({}));
        } else if i % 3 == 1 {
            let response = client
                .call_tool("rust_analyzer_symbols", json!({"file_path": main_path_str}))
                .await?;
            assert!(response.get("content").is_some());
        } else {
            let response = client.send_request("tools/list", None).await?;
            assert!(response.get("tools").is_some());
        }
    }

    eprintln!("Ping interleaved test completed {} iterations", iterations);
    Ok(())
}

#[tokio::test]
async fn test_ping_mixed_concurrent_workload() -> Result<()> {
    let temp_client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = temp_client.workspace_path().to_path_buf();
    drop(temp_client);

    let main_path = workspace_path.join("src/main.rs");
    let main_path_str = main_path.to_str().unwrap();

    let concurrency = if is_ci() { 15 } else { 30 };

    let futures = (0..concurrency).map(|i| {
        let main_path_str = main_path_str.to_string();
        async move {
            let mut client = IpcClient::get_or_create("test-project").await?;

            // Mix pings with tool calls
            match i % 5 {
                0 | 1 => client.send_request("ping", None).await,
                2 => client
                    .call_tool("rust_analyzer_symbols", json!({"file_path": main_path_str}))
                    .await
                    .map(|_| json!({})),
                3 => client
                    .send_request("tools/list", None)
                    .await
                    .map(|_| json!({})),
                _ => client
                    .call_tool(
                        "rust_analyzer_hover",
                        json!({"file_path": main_path_str, "line": 1, "character": 10}),
                    )
                    .await
                    .map(|_| json!({})),
            }
        }
    });

    let results = join_all(futures).await;

    // Count successes
    let mut success_count = 0;
    for result in results {
        result?;
        success_count += 1;
    }

    eprintln!(
        "Ping mixed workload: {}/{} requests succeeded",
        success_count, concurrency
    );
    assert_eq!(success_count, concurrency);

    Ok(())
}

#[tokio::test]
async fn test_ping_burst_pattern() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;

    let bursts = if is_ci() { 5 } else { 10 };
    let pings_per_burst = if is_ci() { 20 } else { 50 };

    for burst in 0..bursts {
        let start = Instant::now();

        // Send burst of pings
        for _ in 0..pings_per_burst {
            let response = client.send_request("ping", None).await?;
            assert_eq!(response, json!({}));
        }

        let elapsed = start.elapsed();
        eprintln!(
            "Burst {}: {} pings in {:?}",
            burst + 1,
            pings_per_burst,
            elapsed
        );

        // Small pause between bursts
        if burst < bursts - 1 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    eprintln!(
        "Ping burst test completed: {} bursts of {} pings",
        bursts, pings_per_burst
    );
    Ok(())
}

#[tokio::test]
async fn test_ping_connection_stability() -> Result<()> {
    // Test that ping doesn't leak resources or connections
    let iterations = if is_ci() { 50 } else { 100 };

    for i in 0..iterations {
        let mut client = IpcClient::get_or_create("test-project").await?;

        // Do a few pings per connection
        for _ in 0..5 {
            let response = client.send_request("ping", None).await?;
            assert_eq!(response, json!({}));
        }

        // Drop connection
        drop(client);

        if i % 25 == 0 {
            eprintln!("Ping connection stability: {} iterations", i);
        }
    }

    eprintln!(
        "Ping connection stability test completed {} iterations",
        iterations
    );
    Ok(())
}

// ============================================================================
// Original Stress Tests
// ============================================================================

#[tokio::test]
async fn test_concurrent_tool_calls() -> Result<()> {
    // Get workspace path to use absolute paths
    let temp_client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = temp_client.workspace_path().to_path_buf();
    drop(temp_client);

    let main_path = workspace_path.join("src/main.rs");
    let main_path_str = main_path.to_str().unwrap();

    // Each concurrent task will create its own client connection to the shared server
    let tasks = vec![
        ("rust_analyzer_symbols", json!({"file_path": main_path_str})),
        (
            "rust_analyzer_hover",
            json!({"file_path": main_path_str, "line": 1, "character": 10}),
        ),
        (
            "rust_analyzer_completion",
            json!({"file_path": main_path_str, "line": 2, "character": 5}),
        ),
        (
            "rust_analyzer_definition",
            json!({"file_path": main_path_str, "line": 1, "character": 20}),
        ),
        (
            "rust_analyzer_references",
            json!({"file_path": main_path_str, "line": 9, "character": 3}),
        ),
        ("rust_analyzer_format", json!({"file_path": main_path_str})),
    ];

    let start = Instant::now();

    // Execute requests - each with its own client connection
    let results = if is_ci() {
        // In CI: execute in two batches
        let (batch1, batch2) = tasks.split_at(3);

        let futures1 = batch1.iter().map(|(tool, args)| {
            let tool = *tool;
            let args = args.clone();
            async move {
                let mut client = IpcClient::get_or_create("test-project").await?;
                client.call_tool(tool, args).await
            }
        });
        let mut results1 = join_all(futures1).await;

        tokio::time::sleep(timeouts::batch_delay()).await;

        let futures2 = batch2.iter().map(|(tool, args)| {
            let tool = *tool;
            let args = args.clone();
            async move {
                let mut client = IpcClient::get_or_create("test-project").await?;
                client.call_tool(tool, args).await
            }
        });
        let results2 = join_all(futures2).await;

        results1.extend(results2);
        results1
    } else {
        // Not in CI: execute all concurrently
        let futures = tasks.iter().map(|(tool, args)| {
            let tool = *tool;
            let args = args.clone();
            async move {
                let mut client = IpcClient::get_or_create("test-project").await?;
                client.call_tool(tool, args).await
            }
        });
        join_all(futures).await
    };

    let elapsed = start.elapsed();
    eprintln!("Concurrent requests completed in {:?}", elapsed);

    // Check all succeeded
    for result in results {
        result?;
    }

    Ok(())
}

#[tokio::test]
async fn test_rapid_fire_requests() -> Result<()> {
    // Get workspace path to use absolute paths
    let temp_client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = temp_client.workspace_path().to_path_buf();
    drop(temp_client);

    let main_path = workspace_path.join("src/main.rs");
    let main_path_str = main_path.to_str().unwrap();

    let mut client = IpcClient::get_or_create("test-project").await?;

    let iterations = if is_ci() { 50 } else { 100 };
    let start = Instant::now();

    for i in 0..iterations {
        let response = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": main_path_str}))
            .await?;

        assert!(response.get("content").is_some());

        if is_ci() && i % 10 == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    let elapsed = start.elapsed();
    let rate = iterations as f64 / elapsed.as_secs_f64();
    eprintln!(
        "Processed {} requests in {:?} ({:.1} req/s)",
        iterations, elapsed, rate
    );

    Ok(())
}

#[tokio::test]
async fn test_mixed_concurrent_workload() -> Result<()> {
    // Get workspace path to use absolute paths
    let temp_client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = temp_client.workspace_path().to_path_buf();
    drop(temp_client);

    let main_path = workspace_path.join("src/main.rs");
    let main_path_str = main_path.to_str().unwrap();

    let iterations = if is_ci() { 5 } else { 10 };

    let futures = (0..iterations).map(|i| {
        let main_path_str = main_path_str.to_string();
        async move {
            let mut client = IpcClient::get_or_create("test-project").await?;

            // Mix of different operations
            match i % 6 {
                0 => {
                    client
                        .call_tool("rust_analyzer_symbols", json!({"file_path": main_path_str}))
                        .await
                }
                1 => {
                    client
                        .call_tool(
                            "rust_analyzer_hover",
                            json!({"file_path": main_path_str, "line": 1, "character": 10}),
                        )
                        .await
                }
                2 => {
                    client
                        .call_tool(
                            "rust_analyzer_completion",
                            json!({"file_path": main_path_str, "line": 2, "character": 5}),
                        )
                        .await
                }
                3 => {
                    client
                        .call_tool(
                            "rust_analyzer_definition",
                            json!({"file_path": main_path_str, "line": 1, "character": 20}),
                        )
                        .await
                }
                4 => {
                    client
                        .call_tool(
                            "rust_analyzer_references",
                            json!({"file_path": main_path_str, "line": 9, "character": 3}),
                        )
                        .await
                }
                _ => {
                    client
                        .call_tool("rust_analyzer_format", json!({"file_path": main_path_str}))
                        .await
                }
            }
        }
    });

    let results = join_all(futures).await;
    for result in results {
        result?;
    }

    Ok(())
}

#[tokio::test]
async fn test_memory_stability() -> Result<()> {
    // Get workspace path to use absolute paths
    let temp_client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = temp_client.workspace_path().to_path_buf();
    drop(temp_client);

    let main_path = workspace_path.join("src/main.rs");
    let main_path_str = main_path.to_str().unwrap();

    let iterations = if is_ci() { 100 } else { 200 };

    for i in 0..iterations {
        let mut client = IpcClient::get_or_create("test-project").await?;

        let response = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": main_path_str}))
            .await?;

        assert!(response.get("content").is_some());

        if i % 50 == 0 {
            eprintln!("Memory stability test: {} iterations complete", i);
        }
    }

    eprintln!("Memory stability test completed {} iterations", iterations);
    Ok(())
}

#[tokio::test]
async fn test_connection_reuse() -> Result<()> {
    // Get workspace path to use absolute paths
    let temp_client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = temp_client.workspace_path().to_path_buf();
    drop(temp_client);

    let main_path = workspace_path.join("src/main.rs");
    let main_path_str = main_path.to_str().unwrap();

    // First batch of connections
    for _ in 0..5 {
        let mut client = IpcClient::get_or_create("test-project").await?;
        let response = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": main_path_str}))
            .await?;
        assert!(response.get("content").is_some());
    }

    // Wait a bit
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second batch should reuse the same server
    for _ in 0..5 {
        let mut client = IpcClient::get_or_create("test-project").await?;
        let response = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": main_path_str}))
            .await?;
        assert!(response.get("content").is_some());
    }

    Ok(())
}

#[tokio::test]
async fn test_stress_different_files() -> Result<()> {
    // Get workspace path to use absolute paths
    let temp_client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = temp_client.workspace_path().to_path_buf();
    drop(temp_client);

    let files = vec![
        workspace_path
            .join("src/main.rs")
            .to_str()
            .unwrap()
            .to_string(),
        workspace_path
            .join("src/lib.rs")
            .to_str()
            .unwrap()
            .to_string(),
        workspace_path
            .join("src/types.rs")
            .to_str()
            .unwrap()
            .to_string(),
        workspace_path
            .join("src/utils.rs")
            .to_str()
            .unwrap()
            .to_string(),
    ];

    let futures = files.iter().cycle().take(20).map(|file| {
        let file = file.clone();
        async move {
            let mut client = IpcClient::get_or_create("test-project").await?;
            client
                .call_tool("rust_analyzer_symbols", json!({"file_path": file}))
                .await
        }
    });

    let results = join_all(futures).await;
    for result in results {
        result?;
    }

    Ok(())
}
