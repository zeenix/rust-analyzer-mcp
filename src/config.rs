/// Timeout for LSP requests in seconds.
pub const LSP_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Timeout for the graceful part of the rust-analyzer shutdown, in seconds.
pub const GRACEFUL_SHUTDOWN_TIMEOUT_SECS: u64 = 5;

/// Delay after opening a document to allow rust-analyzer to process it.
pub const DOCUMENT_OPEN_DELAY_MILLIS: u64 = 200;

/// How long to wait for a cargo check to start after asking rust-analyzer for one.
///
/// Long enough to cover a check already running being cancelled to make way for the one asked
/// for, which means waiting for a cargo process to go away.
pub const FLYCHECK_START_TIMEOUT_SECS: u64 = 2;

/// How many times to ask for a cargo check that never starts.
///
/// rust-analyzer works out which checks to restart on a task it abandons whenever its analysis
/// is superseded -- by the very change being asked about, among other things -- so a request can
/// go nowhere through no fault of its own. Asking again is all it takes.
pub const FLYCHECK_REQUEST_ATTEMPTS: usize = 5;

/// How long to wait for a cargo check that has started to finish.
pub const FLYCHECK_TIMEOUT_SECS: u64 = 30;

/// How long to wait for rust-analyzer to finish loading a workspace before asking it something
/// whose answer depends on the whole of it, in seconds.
pub const WORKSPACE_LOAD_TIMEOUT_SECS: u64 = 30;
