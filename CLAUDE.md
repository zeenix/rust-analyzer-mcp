# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

rust-analyzer-mcp is a Model Context Protocol (MCP) server that provides integration with rust-analyzer LSP, allowing AI assistants to analyze Rust code through standardized tools. The server acts as a bridge between MCP clients and rust-analyzer, translating MCP tool calls into LSP requests.

## Architecture

The codebase follows a modular architecture:

- **Main MCP Server** (`src/main.rs`): Handles MCP protocol, manages rust-analyzer subprocess, and routes tool calls to LSP methods
- **Test Support Library** (`test-support/`): Provides `MCPTestClient` for integration testing with proper process lifecycle management
- **Test Structure**:
  - `tests/integration/`: Core MCP server integration tests
  - `tests/stress/`: Concurrency and performance stress tests
  - `tests/unit/`: Protocol and component unit tests
  - `tests/property/`: Property-based fuzzing tests

Key architectural decisions:
- Uses Tokio async runtime for concurrent request handling
- Maintains persistent rust-analyzer subprocess for performance
- Implements proper LSP initialization sequence with workspace support
- Handles CI environment detection for test reliability

## Development Commands

### Building and Running

```bash
# Development build and run
cargo build
cargo run -- /path/to/workspace

# Release build (optimized with LTO)
cargo build --release

# Run with debug logging
RUST_LOG=debug cargo run -- /path/to/workspace
```

### Testing

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_concurrent_tool_calls

# Run tests in release mode (for stress tests)
cargo test --release

# Run integration tests only
cargo test --test integration_tests

# Run with verbose output to debug failures
cargo test -- --nocapture

# Run tests with specific timeout debugging
RUST_BACKTRACE=1 cargo test --test integration_tests test_all_lsp_tools
```

### Linting and Formatting

```bash
# Format code
cargo +nightly fmt

# Run clippy linter
cargo clippy -- -D warnings

# Check without building
cargo check
```

## CI Considerations

The test suite includes CI-specific handling to ensure reliability in GitHub Actions:

- Tests detect CI environment via `std::env::var("CI")`
- In CI, concurrent tests run in smaller batches to avoid overwhelming the system
- Tool call timeouts are extended from 10s to 30s in CI environments
- The `test_rapid_fire_requests` test adds small delays between spawns in CI only

When debugging CI failures, check for:
- rust-analyzer initialization timeouts (30s timeout in CI)
- Concurrent request handling (batched in CI vs full concurrency locally)
- Success thresholds adjusted for CI reliability

## Test Project

The `test-project/` directory contains a minimal Rust project used for integration testing. It includes:
- Basic functions (`greet`, `Calculator` struct) for testing LSP features
- Positioned specifically to test definition, references, hover, and completion at known locations

## Key Implementation Details

### MCP Protocol Handling
- Implements full MCP initialize sequence with tool discovery
- Returns proper JSON-RPC responses with error handling
- Tools return results wrapped in MCP content items

### rust-analyzer Integration
- Spawns rust-analyzer as subprocess with stdio communication
- Implements proper LSP initialization with workspace capabilities
- Opens documents before LSP operations to ensure proper analysis
- Handles async LSP responses with request ID tracking

### Tool Reliability
- Symbols tool polls until rust-analyzer completes indexing
- Definition/references tools handle null responses during initialization
- Format tool requires document to be opened first
- Completion tool may return null during indexing

## Testing Patterns

### Integration Tests
- Use `MCPTestClient::initialize_and_wait()` to ensure rust-analyzer is ready
- Check for both successful responses and null handling
- Test invalid inputs for error handling

### Stress Tests
- Test concurrent requests with `futures::future::join_all`
- Verify memory stability with repeated operations
- Test rapid-fire sequential requests for throughput

### CI-Specific Testing
```rust
// Pattern for CI-specific behavior
if std::env::var("CI").is_ok() {
    // CI-specific handling (batching, delays, extended timeouts)
} else {
    // Local development (full concurrency, normal timeouts)
}
```
