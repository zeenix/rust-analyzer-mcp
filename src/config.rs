/// Timeout for LSP requests in seconds.
pub const LSP_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Maximum time to wait for rust-analyzer's `cachePriming` progress to end after
/// opening a document. Falls back to "best effort" on timeout (operations may
/// briefly return null while indexing is still in flight).
pub const INDEXING_WAIT_TIMEOUT_SECS: u64 = 5;

/// How often the process monitor task checks whether rust-analyzer is still alive.
pub const PROCESS_MONITOR_INTERVAL_MILLIS: u64 = 500;

/// Maximum number of automatic restarts allowed within `RESTART_WINDOW_SECS`. If
/// rust-analyzer crashes more often than this in the window, further requests
/// fail rather than spinning up a new doomed process.
pub const MAX_RESTART_COUNT: usize = 3;
pub const RESTART_WINDOW_SECS: u64 = 60;
