use anyhow::Result;
use serde_json::json;
use test_support::SharedMCPClient;

#[tokio::test]
async fn test_shared_singleton() -> Result<()> {
    // Create two clients for the same project
    let client1 = SharedMCPClient::get_or_create("test-project").await?;
    let client2 = SharedMCPClient::get_or_create("test-project").await?;

    // Both should work
    let response1 = client1
        .call_tool(
            "rust_analyzer_symbols",
            json!({
                "file_path": "src/lib.rs"
            }),
        )
        .await?;

    let response2 = client2
        .call_tool(
            "rust_analyzer_symbols",
            json!({
                "file_path": "src/main.rs"
            }),
        )
        .await?;

    // Check we got responses
    assert!(response1.get("content").is_some());
    assert!(response2.get("content").is_some());

    println!("✓ Shared singleton works!");

    Ok(())
}

#[tokio::test]
async fn test_shared_concurrent() -> Result<()> {
    use futures::future::join_all;

    // Create multiple clients concurrently
    let tasks: Vec<_> = (0..5)
        .map(|i| async move {
            let client = SharedMCPClient::get_or_create("test-project").await?;

            let response = client
                .call_tool(
                    "rust_analyzer_symbols",
                    json!({
                        "file_path": "src/lib.rs"
                    }),
                )
                .await?;

            println!("Client {} got response", i);
            Ok::<_, anyhow::Error>(response)
        })
        .collect();

    let results = join_all(tasks).await;

    // All should succeed
    for result in results {
        assert!(result?.get("content").is_some());
    }

    println!("✓ Concurrent access works!");

    Ok(())
}
