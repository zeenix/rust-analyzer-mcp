/// Timeout for LSP requests in seconds.
pub const LSP_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Timeout for the graceful part of the rust-analyzer shutdown, in seconds.
pub const GRACEFUL_SHUTDOWN_TIMEOUT_SECS: u64 = 5;

/// Delay after opening a document to allow rust-analyzer to process it.
pub const DOCUMENT_OPEN_DELAY_MILLIS: u64 = 200;
