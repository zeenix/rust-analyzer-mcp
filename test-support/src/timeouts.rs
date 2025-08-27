use std::time::Duration;

use crate::is_ci;

// Timeout constants

/// Default request timeout.
pub const REQUEST_DEFAULT_SECS: u64 = 10;

/// Tool call timeout.
pub const TOOL_CALL_SECS: u64 = 10;
pub const TOOL_CALL_CI_SECS: u64 = 45;

/// rust-analyzer initialization timeout.
pub const INIT_WAIT_SECS: u64 = 30;
pub const INIT_WAIT_CI_SECS: u64 = 90;

/// Polling interval for initialization.
pub const INIT_POLL_MILLIS: u64 = 200;

/// Extra delay after initialization.
pub const INIT_EXTRA_DELAY_MILLIS: u64 = 500;
pub const INIT_EXTRA_DELAY_CI_SECS: u64 = 2;

/// Retry delay for tools.
pub const TOOL_RETRY_DELAY_MILLIS: u64 = 500;

/// LSP request timeout in main.rs.
pub const LSP_REQUEST_SECS: u64 = 30;

/// Document open delay.
pub const DOCUMENT_OPEN_DELAY_MILLIS: u64 = 200;

/// Stress test timeouts.
pub const STRESS_CONCURRENT_BASE_SECS: u64 = 10;
pub const STRESS_SEQUENTIAL_BASE_SECS: u64 = 20;
pub const STRESS_FILES_BASE_SECS: u64 = 10;

/// Stress test delays.
pub const STRESS_BATCH_DELAY_MILLIS: u64 = 500;
pub const STRESS_RAPID_DELAY_MILLIS: u64 = 10;
pub const STRESS_RAPID_DELAY_CI_MILLIS: u64 = 100;

/// CI delay between tests.
pub const CI_TEST_DELAY_SECS: u64 = 1;

// Helper functions

/// Get request timeout.
pub fn request() -> Duration {
    Duration::from_secs(REQUEST_DEFAULT_SECS)
}

/// Get tool call timeout based on environment.
pub fn tool_call() -> Duration {
    if is_ci() {
        Duration::from_secs(TOOL_CALL_CI_SECS)
    } else {
        Duration::from_secs(TOOL_CALL_SECS)
    }
}

/// Get initialization wait timeout.
pub fn init_wait() -> Duration {
    if is_ci() {
        Duration::from_secs(INIT_WAIT_CI_SECS)
    } else {
        Duration::from_secs(INIT_WAIT_SECS)
    }
}

/// Get initialization polling interval.
pub fn init_poll() -> Duration {
    Duration::from_millis(INIT_POLL_MILLIS)
}

/// Get extra delay after initialization.
pub fn init_extra_delay() -> Duration {
    if is_ci() {
        Duration::from_secs(INIT_EXTRA_DELAY_CI_SECS)
    } else {
        Duration::from_millis(INIT_EXTRA_DELAY_MILLIS)
    }
}

/// Get tool retry delay.
pub fn tool_retry_delay() -> Duration {
    Duration::from_millis(TOOL_RETRY_DELAY_MILLIS)
}

/// Get stress test timeout with multiplier for CI.
pub fn stress_timeout(base_secs: u64) -> Duration {
    let secs = if is_ci() { base_secs * 3 } else { base_secs };
    Duration::from_secs(secs)
}

/// Get rapid fire delay based on environment.
pub fn rapid_delay() -> Duration {
    if is_ci() {
        Duration::from_millis(STRESS_RAPID_DELAY_CI_MILLIS)
    } else {
        Duration::from_millis(STRESS_RAPID_DELAY_MILLIS)
    }
}

/// Get batch delay for stress tests.
pub fn batch_delay() -> Duration {
    Duration::from_millis(STRESS_BATCH_DELAY_MILLIS)
}

/// Get CI test delay.
pub fn ci_test_delay() -> Duration {
    Duration::from_secs(CI_TEST_DELAY_SECS)
}
