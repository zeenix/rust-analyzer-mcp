use anyhow::Result;
use std::{path::PathBuf, sync::Arc};

use rust_analyzer_mcp::RustAnalyzerMCPServer;
use tracing_subscriber::{fmt, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let workspace_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    let server = Arc::new(RustAnalyzerMCPServer::with_workspace(workspace_path));
    server.run().await?;

    Ok(())
}
