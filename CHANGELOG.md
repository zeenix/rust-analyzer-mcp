# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.3.1 - 2026-08-21

### CI
- 👷 Upload the test daemons' logs when the tests fail.

### Changed
- 🔧 Configure release-plz to use the repository's tag names.
- 🔧 Never publish the crate.

### Fixed
- 🥅 Recover from rust-analyzer exiting.
- 🚑️ Only send didSave once rust-analyzer is quiescent.

### Other
- 🔊 Log rust-analyzer's stderr at info level.
- 🔊 Keep the MCP server's stderr in the daemon log.

## 0.3.0 - 2026-08-21

### CI
- 👷 Run the whole workspace's tests.
- 💚 Retry transient tool-call timeouts in the IPC client.

### Changed
- ♻️  Refactor.
- 🔧 Use --locked option for cargo test.

### Fixed
- 🐛 Report the real crate version in initialize.
- 🩹 Cover more shutdown events, document run()'s hazards.
- 🐛 Fix server never exiting on SIGINT/SIGTERM.

### Other
- 🧵 Register response channel before sending the request.
- 🧑‍💻 Handle --help, --version and bad arguments.
- 🤖 Add gimoji info to CLAUDE.md.
- 🚀 Add GitHub release workflow for annotated tags.

### Testing
- ✅ Enable the shared IPC client tests.

## 0.2.0 - 2025-08-31

### Added
- ✨ Add IPC-based test implementations.
- ✨ Implement IPC-based test infrastructure with separate process.
- ✨ Implement self-cleaning service with 15s inactivity timeout.
- ✨ Implement atomic singleton rust-analyzer with filesystem locks.
- ✨ Implement singleton rust-analyzer pattern for tests.
- ✨ Add WorkspaceReadiness helper for robust test initialization.

### Changed
- ♻️  Auto-build test-support-server using Cargo's built-in locking.
- 🔧 Remove unused import warning.
- 🔧 Update gitignore for nested target directories.
- ♻️ Port all tests to use IpcClient instead of SharedMCPClient.
- 🔧 Isolate shared tests with unique project identifiers.
- 🔧 Improve process cleanup to reduce test flakiness.
- ♻️ Update tests to use enhanced workspace initialization.

### Fixed
- 🐛 Fix clean file diagnostics test reliability.
- 🐛 Fix resource leak in test_all_lsp_tools.
- 🐛 Fix parallel test execution reliability.
- 🐛 Remove empty workspace section from test-project.
- 🐛 Fix file copying issue in isolated test projects.
- 🐛 Fix flaky diagnostic tests in CI.

### Other
- 🚀 Bump version to 0.2.0.
- 🔨 Fix repo URL in Cargo.toml.

### Removed
- 🔥 Remove broken SharedMCPClient and its dependencies.

### Security
- 🔒 Add process isolation for test runs.

### Testing
- ✅ Fix flaky diagnostic tests with separate test projects.

## 0.1.0 - 2025-08-28

### Added
- ✨ Add diagnostics tool.

### CI
- 💚 Even longer timeouts for CI.
- 💚 Let's bump timeouts and delays even further for CI.
- 💚 Longer timeouts when in CI.
- 💚 Add a delay in the rapid fire test when running in CI.
- 💚 Install rust-analyzer for tests in the CI.
- 👷 More thorough CI.
- 👷 Don't build (it's implied in test).

### Changed
- 🎨 Some improvements to code.
- 🎨 Fix formatting in test-project.
- 🎨 Fix formatting.
- 🎨 Fix formatting of the code.

### Dependencies
- ➕ Add dep on serial_test.

### Documentation
- 📝 Update README with crates.io installation instructions.
- 📝 Document diagnostic tools in README.
- 📝 Update docs on format tool.
- 📝 Information on how to setup the MCP for Claude Code.
- 📝 Correct information in README.
- 📝 Add CONTRIBUTING.md.

### Fixed
- 🐛 Fix MCP server initialization regression.
- 🐛 Fix flaky diagnostic tests in CI.
- 🐛 Make it a bit more reliable.
- 🚑️ Fix Cargo.toml for test-project used by tests.
- 🐛 Use absolute paths and URIs to be more reliable.
- 🚑️ Make Code actions work.
- 🚑️ Report correct protocol version.
- 🩹 More reliable search for rust-analyzer.

### Other
- 🧵 Isolate env for tests so they can be run in parallel.
- 🚨 Remove an unused import.
- 🧵 Run concurrent_requests tests in isolation.
- 🔊 Change an info log to debug log.
- 🤖 Add CLAUDE.md.
- 🚨 Add rustfmt configuration.
- 🚨 Satisfy clippy.
- 🔨 Add git hooks one can easily setup.
- Make tests faster and reliable.
- Optimize tests.
- Refactor MCPResponse to use enum for better type safety.
- Fix test compilation errors and clean up test suite.
- Convert Python tests to comprehensive Rust test suite.
- Add MIT LICENSE and update documentation.
- Add GitHub Actions workflow for Rust project.
- Fix definition and references functionality.
- Make it work.
- Init.

### Performance
- ⚡ Optimize stress tests for speed and reliability.

### Testing
- ✅ Use constants for timeouts and lower their values.
- ✅ Properly test formatting tool.
- ✅ Use static test-project instead of generating it & simplify the code.
- ✅ More reliable tests.
- ✅ Proper cleanup in tests.
- ✅ Refactor.
