use anyhow::{bail, Result};
use std::path::PathBuf;

use rust_analyzer_mcp::RustAnalyzerMCPServer;

const USAGE: &str = "\
MCP server for rust-analyzer integration.

Usage: rust-analyzer-mcp [--] [WORKSPACE]

Arguments:
  [WORKSPACE]  Path to the workspace root [default: current directory]

Options:
  -h, --help     Print help
  -V, --version  Print version
";

fn main() -> Result<()> {
    // Initialize logging.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Get workspace path from command line or use current directory. args_os() instead of
    // args() so a non-UTF-8 workspace path is passed through rather than panicking.
    let mut args = std::env::args_os().skip(1);
    let workspace_path = match args.next() {
        // A lone "--" ends option parsing, allowing a workspace path that starts with '-'.
        Some(arg) if arg == "--" => args.next().map(PathBuf::from),
        Some(arg) => match arg.to_str() {
            Some("--help") | Some("-h") => {
                print!("{USAGE}");
                return Ok(());
            }
            Some("--version") | Some("-V") => {
                println!("rust-analyzer-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some(opt) if opt.starts_with('-') => bail!("unknown option '{opt}'\n\n{USAGE}"),
            _ => Some(PathBuf::from(arg)),
        },
        None => None,
    };
    let workspace_path = workspace_path
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));
    if let Some(extra) = args.next() {
        bail!(
            "unexpected extra argument '{}'\n\n{USAGE}",
            extra.to_string_lossy()
        );
    }

    // Create and run the server, catching panics so an unwind cannot bypass the runtime
    // shutdown below. This only matters for debug builds and tests: release builds abort on
    // panic.
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
