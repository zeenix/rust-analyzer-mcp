# rust-analyzer MCP Server

This is a Model Context Protocol (MCP) server that provides integration with rust-analyzer, allowing AI assistants to analyze Rust code, get hover information, find definitions, references, and more. Written in Rust for optimal performance and native integration.

## Prerequisites

1. **rust-analyzer**: Make sure rust-analyzer is installed and available in your PATH
   ```bash
   # Install via rustup (recommended)
   rustup component add rust-analyzer
   
   # Or install directly
   cargo install rust-analyzer
   
   # Verify installation
   rust-analyzer --version
   ```

2. **Rust**: Version 1.70 or higher with Cargo
3. **A Rust project**: The server works best with a valid Rust workspace (containing `Cargo.toml`)

## Why Rust?

This Rust implementation offers several advantages over alternative implementations:

- **Performance**: Native Rust binary with minimal overhead
- **Memory Safety**: No runtime errors from memory issues
- **Ecosystem Integration**: Perfect fit for Rust development workflows
- **Small Binary Size**: Optimized release builds with LTO
- **Concurrent Safety**: Tokio async runtime handles multiple requests efficiently
- **Native LSP Handling**: Direct integration with rust-analyzer's protocol

## Installation

1. Clone or create the project:
   ```bash
   mkdir rust-analyzer-mcp-server
   cd rust-analyzer-mcp-server
   ```

2. Save the Rust code as `src/main.rs` and the `Cargo.toml`

3. Build the project:
   ```bash
   cargo build --release
   ```

4. The binary will be available at `target/release/rust-analyzer-mcp-server`

## Configuration

### Claude Desktop Configuration

Add this to your Claude Desktop configuration (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "rust-analyzer": {
      "command": "/path/to/rust-analyzer-mcp-server/target/release/rust-analyzer-mcp-server",
      "cwd": "/path/to/your/rust/project"
    }
  }
}
```

### Other MCP Clients

For other MCP clients, run the server with:
```bash
./target/release/rust-analyzer-mcp-server
```

Or during development:
```bash
cargo run
```

The server communicates via stdio and follows the MCP protocol.

## Available Tools

### `rust_analyzer_hover`
Get hover information (documentation, type info) for a symbol at a specific position.

**Parameters:**
- `file_path`: Path to the Rust file (relative to workspace)
- `line`: Line number (0-based)
- `character`: Character position (0-based)

**Example usage:**
```
Get hover information for the symbol at line 10, character 5 in src/main.rs
```

### `rust_analyzer_definition`
Find the definition of a symbol at a specific position.

**Parameters:**
- `file_path`: Path to the Rust file
- `line`: Line number (0-based)  
- `character`: Character position (0-based)

### `rust_analyzer_references`
Find all references to a symbol at a specific position.

**Parameters:**
- `file_path`: Path to the Rust file
- `line`: Line number (0-based)
- `character`: Character position (0-based)

### `rust_analyzer_completion`
Get code completion suggestions at a specific position.

**Parameters:**
- `file_path`: Path to the Rust file
- `line`: Line number (0-based)
- `character`: Character position (0-based)

### `rust_analyzer_symbols`
Get all symbols (functions, structs, enums, etc.) in a file.

**Parameters:**
- `file_path`: Path to the Rust file

### `rust_analyzer_format`
Format a Rust file using rust-analyzer's formatter.

**Parameters:**
- `file_path`: Path to the Rust file

### `rust_analyzer_code_actions`
Get available code actions (quick fixes, refactorings) for a range.

**Parameters:**
- `file_path`: Path to the Rust file
- `line`: Start line number (0-based)
- `character`: Start character position (0-based)
- `end_line`: End line number (0-based)
- `end_character`: End character position (0-based)

### `rust_analyzer_set_workspace`
Change the workspace root directory.

**Parameters:**
- `workspace_path`: Path to the new workspace root

## Usage Examples

Here are some example prompts you can use with Claude when this MCP server is configured:

1. **Code Analysis:**
   ```
   Can you analyze the main function in src/main.rs and tell me what it does? 
   Use the rust analyzer tools to get hover information and symbols.
   ```

2. **Finding Definitions:**
   ```
   I'm looking at a function call on line 25 of src/lib.rs at character position 10. 
   Can you find its definition using rust-analyzer?
   ```

3. **Code Completion:**
   ```
   I'm writing code at line 15, character 8 in src/main.rs. 
   What completion suggestions are available?
   ```

4. **Refactoring Help:**
   ```
   What code actions are available for the code between line 10-15 in src/utils.rs?
   ```

## Project Structure

```
rust-analyzer-mcp-server/
├── src/
│   └── main.rs       # Main MCP server implementation
├── Cargo.toml        # Rust dependencies and metadata
└── README.md         # This file
```

## Development

To run in development mode:
```bash
cargo run
```

To build for release:
```bash
cargo build --release
```

To run tests:
```bash
cargo test
```

To check code without building:
```bash
cargo check
```

For verbose logging during development:
```bash
RUST_LOG=debug cargo run
```

To run with release optimizations in dev:
```bash
cargo run --release
```

## Troubleshooting

### rust-analyzer not found
- Ensure rust-analyzer is in your PATH: `which rust-analyzer`
- Try reinstalling: `rustup component add rust-analyzer`

### Connection errors
- Make sure you're running the server in a valid Rust workspace (with Cargo.toml)
- Check that the file paths are correct and relative to the workspace root

### Permission issues
- Make sure the server has read access to your Rust files
- Check that rust-analyzer has permission to analyze your project

### LSP communication issues
- The server handles LSP protocol automatically
- Check console output for any rust-analyzer errors (use `RUST_LOG=debug` for verbose logging)
- Ensure your Rust project compiles successfully

### Build issues
- Make sure you have Rust 1.70+ installed: `rustc --version`
- Try `cargo clean` and rebuild if you encounter dependency issues

### Performance
- rust-analyzer may take time to initially index large projects
- Subsequent requests should be much faster
- Consider excluding large target/ directories if needed

## Contributing

This is a foundation implementation that covers the most common rust-analyzer features. Contributions are welcome for:

- Additional LSP methods (workspace symbols, rename, etc.)
- Better error handling and diagnostics  
- Configuration options (via CLI args or config files)
- Performance optimizations and async improvements
- Integration tests and benchmarks
- Better LSP message parsing and error recovery
- Support for additional rust-analyzer features

### Development Guidelines

- Use `cargo fmt` for consistent formatting
- Run `cargo clippy` for linting
- Add tests for new functionality  
- Update documentation for new tools
- Follow Rust async best practices with Tokio

## License

MIT License - feel free to use and modify as needed.

