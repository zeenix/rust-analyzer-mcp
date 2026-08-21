# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1](https://github.com/zeenix/rust-analyzer-mcp/compare/v0.3.0...v0.3.1) - 2026-08-21

### Other

- 🔧 Configure release-plz to use the repository's tag names
- 👷 ci: Upload the test daemons' logs when the tests fail
- 🥅 lsp: Recover from rust-analyzer exiting
- 🚑️ lsp: Only send didSave once rust-analyzer is quiescent
- 🔊 lsp: Log rust-analyzer's stderr at info level

## [0.3.0](https://github.com/zeenix/rust-analyzer-mcp/compare/v0.2.0...v0.3.0) - 2026-08-21

### Other

- 👷 ci: Run the whole workspace's tests
- 🧵 lsp: Register response channel before sending the request
- ✅ Enable the shared IPC client tests
- 🧑‍💻 Handle --help, --version and bad arguments
- 🐛 Report the real crate version in initialize
- 🩹 Cover more shutdown events, document run()'s hazards
- 🐛 Fix server never exiting on SIGINT/SIGTERM
- ♻️  Refactor
- 🤖 Add gimoji info to CLAUDE.md
- 🚀 Add GitHub release workflow for annotated tags
- 🔧 CI: Use --locked option for cargo test
