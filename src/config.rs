/// Timeout for LSP requests in seconds.
pub const LSP_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Maximum time to wait for rust-analyzer's `cachePriming` progress to end after
/// opening a document. Falls back to "best effort" on timeout (operations may
/// briefly return null while indexing is still in flight).
pub const INDEXING_WAIT_TIMEOUT_SECS: u64 = 5;
