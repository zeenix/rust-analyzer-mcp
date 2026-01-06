/// Timeout for LSP requests in seconds.
pub const LSP_REQUEST_TIMEOUT_SECS: u64 = 60;

/// Delay after opening a document to allow rust-analyzer to process it.
pub const DOCUMENT_OPEN_DELAY_MILLIS: u64 = 500;

/// Maximum number of restart attempts for rust-analyzer.
pub const MAX_RESTART_ATTEMPTS: u32 = 3;

/// Delay between restart attempts in milliseconds.
pub const RESTART_DELAY_MILLIS: u64 = 1000;

/// Maximum number of open documents to track (LRU eviction after this).
pub const MAX_OPEN_DOCUMENTS: usize = 50;

/// Time to wait for diagnostics notification before falling back to poll (milliseconds).
pub const DIAGNOSTICS_WAIT_MILLIS: u64 = 2000;
