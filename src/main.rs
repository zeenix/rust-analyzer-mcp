use anyhow::Result;
use std::path::PathBuf;

use rust_analyzer_mcp::RustAnalyzerMCPServer;

fn main() -> Result<()> {
    // Initialize logging.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Get workspace path from command line or use current directory.
    let workspace_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    // Create and run the server, catching panics so an unwind cannot bypass the runtime
    // shutdown below.
    let mut server = RustAnalyzerMCPServer::with_workspace(workspace_path);
    let runtime = tokio::runtime::Runtime::new()?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(server.run())
    }));

    // tokio's stdin reader parks an uncancellable blocking read(2) on a pool thread. A normal
    // runtime drop waits for it to finish, hanging the process when shutdown was triggered by a
    // signal rather than stdin EOF.
    runtime.shutdown_background();

    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
