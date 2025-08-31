use anyhow::Result;
use futures::future::join_all;
use serde_json::json;
use std::time::{Duration, Instant};

use test_support::{is_ci, timeouts, IpcClient};

#[tokio::test]
async fn test_concurrent_tool_calls() -> Result<()> {
    // Each concurrent task will create its own client connection to the shared server
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
    let mut client = IpcClient::get_or_create("test-project").await?;

    let iterations = if is_ci() { 50 } else { 100 };
    let start = Instant::now();

    for i in 0..iterations {
        let response = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/main.rs"}))
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
    let iterations = if is_ci() { 5 } else { 10 };

    let futures = (0..iterations).map(|i| async move {
        let mut client = IpcClient::get_or_create("test-project").await?;

        // Mix of different operations
        match i % 6 {
            0 => {
                client
                    .call_tool("rust_analyzer_symbols", json!({"file_path": "src/main.rs"}))
                    .await
            }
            1 => {
                client
                    .call_tool(
                        "rust_analyzer_hover",
                        json!({"file_path": "src/main.rs", "line": 1, "character": 10}),
                    )
                    .await
            }
            2 => {
                client
                    .call_tool(
                        "rust_analyzer_completion",
                        json!({"file_path": "src/main.rs", "line": 2, "character": 5}),
                    )
                    .await
            }
            3 => {
                client
                    .call_tool(
                        "rust_analyzer_definition",
                        json!({"file_path": "src/main.rs", "line": 1, "character": 20}),
                    )
                    .await
            }
            4 => {
                client
                    .call_tool(
                        "rust_analyzer_references",
                        json!({"file_path": "src/main.rs", "line": 9, "character": 3}),
                    )
                    .await
            }
            _ => {
                client
                    .call_tool("rust_analyzer_format", json!({"file_path": "src/main.rs"}))
                    .await
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
    let iterations = if is_ci() { 100 } else { 200 };

    for i in 0..iterations {
        let mut client = IpcClient::get_or_create("test-project").await?;

        let response = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/main.rs"}))
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
    // First batch of connections
    for _ in 0..5 {
        let mut client = IpcClient::get_or_create("test-project").await?;
        let response = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/main.rs"}))
            .await?;
        assert!(response.get("content").is_some());
    }

    // Wait a bit
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second batch should reuse the same server
    for _ in 0..5 {
        let mut client = IpcClient::get_or_create("test-project").await?;
        let response = client
            .call_tool("rust_analyzer_symbols", json!({"file_path": "src/main.rs"}))
            .await?;
        assert!(response.get("content").is_some());
    }

    Ok(())
}

#[tokio::test]
async fn test_stress_different_files() -> Result<()> {
    let files = vec!["src/main.rs", "src/lib.rs", "src/types.rs", "src/utils.rs"];

    let futures = files.iter().cycle().take(20).map(|file| {
        let file = *file;
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
