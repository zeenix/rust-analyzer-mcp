use anyhow::Result;
use serde_json::json;
use test_support::SharedMCPClient;

#[tokio::test]
async fn test_shared_singleton() -> Result<()> {
    // Create two clients for the same project - use unique ID for this test
    let client1 = SharedMCPClient::get_or_create("test-project-singleton").await?;
    let client2 = SharedMCPClient::get_or_create("test-project-singleton").await?;

    // Both should work
    let lib_path = client1.workspace_path().join("src/lib.rs");
    let response1 = client1
        .call_tool(
            "rust_analyzer_symbols",
            json!({
                "file_path": lib_path.to_str().unwrap()
            }),
        )
        .await?;

    let main_path = client2.workspace_path().join("src/main.rs");
    let response2 = client2
        .call_tool(
            "rust_analyzer_symbols",
            json!({
                "file_path": main_path.to_str().unwrap()
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

    // Create multiple clients concurrently - use unique ID for this test
    let tasks: Vec<_> = (0..5)
        .map(|i| async move {
            let client = SharedMCPClient::get_or_create("test-project-concurrent").await?;
            let lib_path = client.workspace_path().join("src/lib.rs");

            let response = client
                .call_tool(
                    "rust_analyzer_symbols",
                    json!({
                        "file_path": lib_path.to_str().unwrap()
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
