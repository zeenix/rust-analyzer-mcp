use anyhow::Result;

use rust_analyzer_mcp::{
    cli::{self, Action, USAGE},
    RustAnalyzerMCPServer,
};

fn main() -> Result<()> {
    // Initialize logging.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // args_os() rather than args() so a workspace path that is not valid UTF-8 is passed through
    // rather than panicking.
    let (workspace_path, settings) = match cli::parse(std::env::args_os().skip(1))? {
        Action::Help => {
            print!("{USAGE}");
            return Ok(());
        }
        Action::Version => {
            println!("rust-analyzer-mcp {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Action::Serve {
            workspace,
            settings,
        } => (workspace, settings),
    };
    let workspace_path = workspace_path
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    // Create and run the server, catching panics so an unwind cannot bypass the runtime
    // shutdown below. This only matters for debug builds and tests: release builds abort on
    // panic.
    let mut server = RustAnalyzerMCPServer::with_workspace(workspace_path).with_settings(settings);
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
