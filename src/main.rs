use anyhow::Result;
use std::path::PathBuf;

use rust_analyzer_mcp::RustAnalyzerMCPServer;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Get workspace path from command line or use current directory.
    let workspace_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    // Create and run the server.
    let mut server = RustAnalyzerMCPServer::with_workspace(workspace_path);
    server.run().await?;

    Ok(())
}
