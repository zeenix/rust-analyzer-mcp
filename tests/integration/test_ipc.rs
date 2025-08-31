use anyhow::Result;
use serde_json::json;
use test_support::IpcClient;

#[tokio::test]
async fn test_ipc_server_basic() -> Result<()> {
    // Connect to or start the IPC server
    let mut client = IpcClient::get_or_create("test-project").await?;

    // Get workspace path
    let workspace_path = client.workspace_path();
    assert!(workspace_path.exists());

    // Test getting symbols
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_symbols",
            json!({
                "file_path": main_path.to_str().unwrap()
            }),
        )
        .await?;

    // Check we got a response
    assert!(response.get("content").is_some());

    Ok(())
}

#[tokio::test]
async fn test_ipc_server_reuse() -> Result<()> {
    // First client
    {
        let mut client1 = IpcClient::get_or_create("test-project").await?;
        let response = client1
            .call_tool(
                "rust_analyzer_symbols",
                json!({
                    "file_path": "src/main.rs"
                }),
            )
            .await?;
        assert!(response.get("content").is_some());
    }

    // Second client should connect to same server
    {
        let mut client2 = IpcClient::get_or_create("test-project").await?;
        let response = client2
            .call_tool(
                "rust_analyzer_symbols",
                json!({
                    "file_path": "src/main.rs"
                }),
            )
            .await?;
        assert!(response.get("content").is_some());
    }

    Ok(())
}

#[tokio::test]
async fn test_ipc_server_parallel() -> Result<()> {
    use futures::future::join_all;

    // Create multiple clients in parallel
    let futures = (0..5).map(|i| async move {
        let mut client = IpcClient::get_or_create("test-project").await?;
        let response = client
            .call_tool(
                "rust_analyzer_symbols",
                json!({
                    "file_path": "src/main.rs"
                }),
            )
            .await?;

        assert!(response.get("content").is_some());
        eprintln!("Client {} completed", i);
        Ok::<(), anyhow::Error>(())
    });

    let results = join_all(futures).await;
    for result in results {
        result?;
    }

    Ok(())
}
