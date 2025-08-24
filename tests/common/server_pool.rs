use anyhow::Result;
use once_cell::sync::Lazy;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

use super::{fixtures::TestProject, test_client::MCPTestClient};

/// A pool of pre-initialized test servers to speed up tests
pub struct ServerPool {
    servers: Arc<Mutex<Vec<TestServerInstance>>>,
}

pub struct TestServerInstance {
    pub client: MCPTestClient,
    pub workspace: TempDir,
}

impl ServerPool {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get a pre-initialized server or create a new one
    pub async fn get(&self) -> Result<TestServerInstance> {
        let mut servers = self.servers.lock().await;

        if let Some(server) = servers.pop() {
            Ok(server)
        } else {
            // Create a new server
            let workspace = TempDir::new()?;
            TestProject::simple().create_in(workspace.path())?;

            let mut client = MCPTestClient::start(workspace.path())?;
            client.initialize()?;

            // Wait for rust-analyzer with smart polling
            for _ in 0..10 {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                // Try a simple operation to check if ready
                if let Ok(response) = client.get_symbols("src/main.rs") {
                    if let Some(content) = response.get("content") {
                        if let Some(text) = content[0].get("text") {
                            if text.as_str() != Some("null") && text.as_str() != Some("[]") {
                                break; // Server is ready
                            }
                        }
                    }
                }
            }

            Ok(TestServerInstance { client, workspace })
        }
    }

    /// Return a server to the pool for reuse
    pub async fn return_server(&self, server: TestServerInstance) {
        let mut servers = self.servers.lock().await;
        servers.push(server);
    }
}

// Global server pool
pub static SERVER_POOL: Lazy<ServerPool> = Lazy::new(ServerPool::new);

/// Helper function to wait for rust-analyzer to be ready
pub async fn wait_for_ready(client: &mut MCPTestClient, max_wait_ms: u64) -> Result<()> {
    let start = std::time::Instant::now();

    while start.elapsed().as_millis() < max_wait_ms as u128 {
        // Try a simple operation to check if ready
        if let Ok(response) = client.get_symbols("src/main.rs") {
            if let Some(content) = response.get("content") {
                if let Some(text) = content[0].get("text") {
                    if text.as_str() != Some("null") && text.as_str() != Some("[]") {
                        return Ok(()); // Server is ready
                    }
                }
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    Err(anyhow::anyhow!(
        "Timeout waiting for rust-analyzer to be ready"
    ))
}
