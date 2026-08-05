use anyhow::Result;
use std::{path::PathBuf, sync::Arc};

use rust_analyzer_mcp::RustAnalyzerMCPServer;
use tracing_subscriber::{fmt, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    // Default to `warn` so routine startup/shutdown INFO doesn't pollute the
    // MCP client's stderr-as-error log channel. Set `RUST_LOG=info` (or
    // `debug`) to opt into verbose logging.
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let workspace_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    let server = Arc::new(RustAnalyzerMCPServer::with_workspace(workspace_path));
    server.run().await?;

    // `run()` has already torn down LSP subprocesses. We can't return normally
    // here because `tokio::io::stdin()` is backed by a blocking thread that
    // sits in a `read(2)` syscall on a pipe the parent hasn't closed; the
    // tokio runtime's `Drop` would block forever waiting for it to finish.
    // Exiting explicitly avoids that hang and lets us return SIGINT-cleanly
    // inside the MCP client's ~100ms grace window.
    std::process::exit(0);
}
