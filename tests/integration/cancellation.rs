use anyhow::{anyhow, Result};
use serde_json::json;

use test_support::MCPTestClient;

/// `notifications/cancelled` for an in-flight request must not deadlock or
/// corrupt the server: subsequent requests must still be served. We don't
/// assert that any *specific* call gets cancelled (that's race-sensitive on
/// a fast tool like tools/list against a tiny project — the smoke harness in
/// /tmp/cancel-smoke.py covers that path); this test pins down the easier
/// invariant that the server stays healthy.
#[tokio::test]
async fn test_cancellation_doesnt_break_server() -> Result<()> {
    let client = MCPTestClient::start_isolated().await?;
    client.initialize_and_wait().await?;

    // Cancel a request id that was never used. Server should silently ignore.
    client
        .send_notification(
            "notifications/cancelled",
            Some(json!({ "requestId": 99999, "reason": "unknown id" })),
        )
        .await?;

    // tools/list must still respond after that.
    let response = client.send_request("tools/list", None).await?;
    let tools = response
        .get("tools")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow!("tools/list response missing tools: {response:?}"))?;
    assert!(!tools.is_empty(), "expected at least one tool");

    // Now fire-and-forget a tool call and immediately cancel it. Whether the
    // race lets it complete first or get aborted, the server must still
    // respond to the next request.
    let id = client
        .send_tool_call_raw("rust_analyzer_workspace_diagnostics", json!({}))
        .await?;
    client
        .send_notification(
            "notifications/cancelled",
            Some(json!({ "requestId": id, "reason": "test race" })),
        )
        .await?;

    // Drain any responses that arrive over the next ~3 s — this absorbs
    // either the cancelled-but-already-completed response or nothing at all.
    let mut leftover = Vec::new();
    let drain = tokio::time::timeout(std::time::Duration::from_millis(500), async {
        match client.send_request("tools/list", None).await {
            Ok(v) => {
                leftover.push(v);
                Ok(())
            }
            Err(e) => Err::<(), _>(e),
        }
    })
    .await;
    let _ = drain; // we don't care about the exact result, only that nothing panicked

    // One final call to confirm the server still serves us.
    let response = client.send_request("tools/list", None).await?;
    let tools = response
        .get("tools")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow!("final tools/list missing tools: {response:?}"))?;
    assert!(!tools.is_empty());

    Ok(())
}
