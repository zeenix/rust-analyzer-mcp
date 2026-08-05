# rust-analyzer MCP Server

This is a Model Context Protocol (MCP) server that provides integration with rust-analyzer, allowing
AI assistants to analyze Rust code, get hover information, find definitions, references, and more.
Written in Rust for optimal performance and native integration.

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

### From crates.io (Recommended)

Install directly from crates.io:
```bash
cargo install rust-analyzer-mcp
```

The binary will be installed to your Cargo bin directory (usually `~/.cargo/bin/rust-analyzer-mcp`).

### From Source

1. Clone the repository:
   ```bash
   git clone https://github.com/zeenix/rust-analyzer-mcp.git
   cd rust-analyzer-mcp
   ```

2. Build the project:
   ```bash
   cargo build --release
   ```

3. The binary will be available at `target/release/rust-analyzer-mcp`

## Configuration

### Claude Code Configuration

Add an MCP server configuration to one of these locations:

**Option 1: Project-specific** (`.mcp.json` in your Rust project root):
```json
{
  "mcpServers": {
    "rust-analyzer": {
      "command": "rust-analyzer-mcp"
    }
  }
}
```

**Option 2: User-wide** (`~/.claude.json` or `~/.claude/settings.json`):
```json
{
  "mcpServers": {
    "rust-analyzer": {
      "command": "rust-analyzer-mcp"
    }
  }
}
```

**Note:** If you installed from crates.io, the command will be in your PATH. If you built from
source, use the full path to the binary. You can also configure servers using Claude Code's CLI
wizard too.

### Workspace Persistence

When `rust_analyzer_add_workspace` registers a new workspace, the root path is mirrored to
`workspaces.json` so the next server boot replays it automatically. The state directory is
chosen in this order:

1. `$RUST_ANALYZER_MCP_STATE_DIR` if set (empty value disables persistence entirely).
2. `$XDG_STATE_HOME/rust-analyzer-mcp/` if `XDG_STATE_HOME` is set.
3. `$HOME/.local/state/rust-analyzer-mcp/` otherwise.

If the MCP host (Claude Code, Claude Desktop, etc.) spawns the server with a sanitized
environment that drops `HOME` / `XDG_STATE_HOME`, persistence silently no-ops — verify by
running `add_workspace` once and checking that `workspaces.json` exists in the chosen
directory. Set `RUST_ANALYZER_MCP_STATE_DIR` explicitly in the host config if your spawn
environment doesn't preserve `HOME`.

### Claude Desktop Configuration

Add this to your Claude Desktop configuration (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "rust-analyzer": {
      "command": "rust-analyzer-mcp"
    }
  }
}
```

**Note:** If installed from crates.io, the command will be in your PATH. For Claude Desktop, you
may want to specify a `cwd` parameter if you want to analyze a specific project by default.

### Other MCP Clients

For other MCP clients, run the server with:
```bash
./target/release/rust-analyzer-mcp
```

Or during development:
```bash
cargo run
```

The server communicates via stdio and follows the MCP protocol.

## Available Tools

The server exposes **36 tools**, grouped below. Every tool is prefixed `rust_analyzer_`.
All tools that take `line`/`character` use **0-based** coordinates per LSP — if you copy a line
number from a 1-based source (editor, `Read`-style file viewer, `grep -n`), subtract 1. Tools
that emit locations (definition, references, etc.) come pre-enriched with surrounding source
snippets by default; pass `include_snippets: false` to skip the read.

In a multi-workspace setup every tool accepts an optional `workspace_id` argument
(see [Workspace Management](#workspace-management) below).

### Diagnostics & hover

- **`diagnostics`** — Per-file compiler diagnostics. Each item carries the LSP fields plus
  rust-analyzer's extensions: `data.rendered` (cargo-formatted error block with ASCII pointers,
  read this first when fixing a bug), `codeDescription.href` (link to the error-index doc),
  and `tags` (e.g. `[1]` for unused code). Every file the server has already opened is re-synced
  from disk before each tool call, so edits made outside rust-analyzer (Edit tool, git checkout)
  are reflected; see [External edits](#external-edits).
- **`workspace_diagnostics`** — All diagnostics workspace-wide. Files paginated (default 50 per
  page); `summary` totals always cover the full workspace.
- **`hover`** — Hover documentation/type info. Markdown capped at ~5 KB by default,
  `verbose=true` removes the cap.
- **`inlay_hints`** — Inferred-type and parameter-name hints for a range.

### Navigation

- **`definition`** — Go to definition.
- **`references`** — All references to a symbol.
- **`type_definition`** — Jump to the *type* of a binding (`let x: Foo` → `Foo`).
- **`implementation`** — Locate `impl` blocks for a trait or item.
- **`parent_module`** — `Location`s of the `mod foo;` declarations that pull in the current
  file/module (often returns multiple for lib + bin crates).

### Search & discovery

- **`workspace_symbol`** — Fuzzy name search across the workspace. Output:
  `{ symbols, total, returned, next_cursor? }`. Default page size 100, cap 1000. Identical
  entries are deduplicated on `(name, containerName, location)`.
- **`symbols`** — Document symbols for a single file. By default flattens nested members to
  `child_count` and paginates the top-level list (default 100, cap 1000); `verbose=true` keeps
  full subtrees. Output: `{ symbols, total_top_level, returned, verbose, next_cursor? }`.
- **`get_type_by_name`** — Look up a type by name path without a position. Accepts
  `"Calculator"`, `"crate::auth::User"`, `"serde_json::Value"`. Returns up to 10 candidates plus
  a `primary` block (hover + type_definition) for the best match.

### Composite tools (one-shot)

- **`explore_symbol`** — In a single round-trip: hover + definition + type_definition +
  parent_module + a sample of up to 5 references for the symbol at a position. Replaces the
  typical 4-5 follow-up tool calls. References sample wraps as `{ items, total, shown }`; if
  references take longer than 2 s, the rest still returns and `references_timed_out: true` is
  added.
- **`impact`** — Blast-radius estimate before a refactor. Aggregates four buckets:
  `references` (textual usages), `callers` (semantic incoming calls), `implementors` (trait
  subtypes), `tests` (related test items, each with ready-to-run `cargo_args`). Per-bucket 2 s
  timeout and 10-item sample; `total` reflects the full count.

### Hierarchy

- **`call_hierarchy_incoming`** — Every caller of the function at a position, with the actual
  call expressions inside each caller. Filters out comment/string noise that `references` would
  surface.
- **`call_hierarchy_outgoing`** — Every function called by the function at a position.
- **`type_hierarchy`** — Walk supertypes/subtypes. `direction` ∈ `"supertypes" | "subtypes" |
  "both"` (default `"both"`). Use this for trait introspection ("who implements this?") —
  `implementation` targets a specific impl block.

### Refactoring & edits

- **`rename`** — Rename a symbol; returns a `WorkspaceEdit` to apply.
- **`prepare_rename`** — Check whether a symbol is renameable; returns the renameable range or
  null.
- **`code_actions`** — Available quick fixes/refactorings for a range.
- **`move_file`** — Move a file or directory inside the workspace; rust-analyzer computes the
  `WorkspaceEdit` (fixing up `mod`-decls and `use`-imports), the server applies it on disk and
  then physically renames the file. Both paths must stay inside the workspace.
- **`format`** — `rustfmt` via rust-analyzer; returns text edits to apply.

### Runnables (test/bench/run/build/clippy)

- **`runnables`** — Reshaped runnable list for a file (or single position). Each entry has
  `kind`, `label`, optional `fq_name`, `cargo_args` (flat argv), `location`, and
  `can_run_via_mcp`. Pass `cargo_args` straight into `run_runnable`.
- **`run_runnable`** — Execute the `cargo_args` from `runnables` inside the workspace root.
  **Disabled by default** — set `RUST_ANALYZER_MCP_ALLOW_RUN=1` in the host environment to
  enable. Allowed subcommands: `test`, `bench`, `run`, `build`, `check`, `clippy`, `doc`,
  `nextest`. Stdout/stderr each capped at 5 KiB; default timeout 60 s, hard cap 600 s.
  Cancelling the MCP request kills the cargo subprocess (`kill_on_drop`).
- **`related_tests`** — Tests that exercise the function/item at a position.

### Compiler internals (debug / introspection)

- **`expand_macro`** — Expand the macro invocation at a position to its source.
- **`syntax_tree`** — rust-analyzer's parsed syntax tree, whole-file or for a range.
- **`view_hir`** — High-level IR for the function at a position.
- **`view_mir`** — Mid-level IR for the function at a position.

### Other

- **`completion`** — Code completion. Items sorted by `sortText`, capped at 50 by default;
  `verbose=true` removes the cap, `limit=N` overrides.
- **`signature_help`** — Parameter info for a function call at a position.
- **`open_docs`** — Resolve the docs.rs / local-docs URL for the symbol at a position.

### Workspace management

The server holds a registry of workspaces; the first one registered is the default. Every
non-management tool accepts an optional `workspace_id` to target a specific workspace; if
omitted the default is used.

- **`add_workspace`** — Register an additional workspace. Returns `{ workspace_id, root }`.
  Roots are mirrored to `workspaces.json` for replay on the next server boot
  (see [Workspace Persistence](#workspace-persistence)).
- **`list_workspaces`** — Every registered workspace with `id`, `root`, `default` flag.
- **`remove_workspace`** — Unregister and shut down a workspace's rust-analyzer subprocess.
- **`set_workspace`** — Replace the default workspace's root in place (legacy single-workspace
  flow). Prefer `add_workspace` + explicit `workspace_id` for multi-workspace setups.

## Resources

The server exposes three MCP resource URIs for cheap, no-LSP discovery:

- **`workspace://files`** — Recursive file tree of the default workspace root, with size and
  type per entry. Cheap (no rust-analyzer round-trip).
- **`workspace://crates`** — Cargo workspace overview from `cargo metadata`: package list with
  manifest paths and dependency summaries.
- **`workspace://crate/<name>/Cargo.toml`** — Per-crate manifest content. The crate name is
  resolved through `cargo metadata`, which blocks path-traversal — paths outside the workspace
  return an error.

In a multi-workspace setup the resource list also advertises per-workspace variants:
`workspace://<workspace_id>/files`, `workspace://<workspace_id>/crates`, etc. The default
workspace keeps the un-prefixed form for backwards compatibility.

## Operational features

- **Concurrency.** Tool calls run in parallel; an in-flight tracker maps MCP request IDs to
  rust-analyzer request IDs. MCP-side `notifications/cancelled` aborts the tool future *and*
  forwards `$/cancelRequest` to rust-analyzer, freeing the index for other queries.
- **Subprocess auto-restart.** If rust-analyzer crashes mid-session the server detects the
  exit, respawns the subprocess, re-applies the workspace-init handshake, and re-opens the
  documents that were open at crash time.
- **External-edit sync.** When a tool is called for a file that has changed on disk (e.g.
  edited via the host's filesystem tools), the server forwards `textDocument/didChange` so
  rust-analyzer sees the new content. Note: tools that don't open the document themselves
  (`workspace_diagnostics`) can still read stale state — see the staleness caveat in the
  Diagnostics section.
- **Snippet enrichment.** Every location-emitting tool (definition, references, hierarchy,
  composites, …) attaches a `snippet` field with a few lines of surrounding source so a
  follow-up file read isn't needed. Disable per-call with `include_snippets: false`; control
  context size with `snippet_context_lines` (default 2).
- **Token-cost guardrails.** Hover, completion, workspace_symbol, workspace_diagnostics, and
  symbols all paginate or truncate by default to stay under the MCP host's response cap.
  `verbose=true` removes the per-tool default cap (still respects a 1000-item absolute cap);
  explicit `limit` and `cursor` are accepted.
- **Cargo-metadata cache.** Each workspace caches `cargo metadata` for 30 s, keyed on the
  mtimes of `Cargo.toml` and `Cargo.lock`; manual invalidation on `set_workspace` and
  shutdown. Resource reads (`workspace://crates`, per-crate manifests) hit this cache.
- **Concurrent-safe daemon.** Spawn locking ensures the server's `target/<profile>/`
  binary matches the running process — solves the "fixed code, daemon still spawns the old
  binary" surprise during local development.

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

5. **Error Checking:**
   ```
   Can you get the diagnostics for src/main.rs and tell me about any errors or warnings?
   ```

6. **Workspace Analysis:**
   ```
   Show me all the diagnostics across the entire workspace using rust-analyzer.
   ```

## Project Structure

```
rust-analyzer-mcp/
├── src/
│   ├── main.rs              # Binary entry point
│   ├── lsp/                 # rust-analyzer subprocess + LSP client
│   │   ├── client.rs        # capabilities, initialize handshake, didOpen/didChange
│   │   └── handlers.rs      # one thin wrapper per LSP method
│   ├── mcp/                 # MCP server logic
│   │   ├── server.rs        # request dispatch, in-flight tracker, cancellation
│   │   ├── handlers.rs      # tool handlers (one match-arm per tool)
│   │   ├── tools.rs         # tool/schema definitions exposed to MCP
│   │   ├── workspace.rs     # multi-workspace registry, per-ws subprocess
│   │   ├── persistence.rs   # workspaces.json mirror for cross-restart replay
│   │   ├── resources.rs     # workspace:// resource implementations
│   │   ├── snippets.rs      # location → source-snippet enrichment
│   │   ├── truncate.rs      # response shaping (pagination, dedup, hover/completion caps)
│   │   ├── runnables.rs     # runnable reshape + run_runnable subprocess
│   │   └── workspace_edit.rs # WorkspaceEdit application for move_file/rename
│   ├── diagnostics/         # diagnostics formatting (raw RA → MCP shape)
│   └── protocol/            # MCP/JSON-RPC types
├── tests/
│   ├── integration/         # end-to-end MCP server tests
│   ├── stress/              # concurrency and rapid-fire tests
│   ├── unit/                # protocol tests
│   └── property/            # property-based fuzzing
├── test-support/            # IPC test daemon + MCPTestClient helper
├── Cargo.toml
└── README.md
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

### External edits

rust-analyzer keeps every file an LSP client opens as an in-memory overlay and deliberately
ignores its own file-watcher events for it — the client is assumed to own that buffer. An MCP
tool call owns nothing: every edit lands on disk, from the Edit tool, another editor, or a git
checkout. So before dispatching a tool call the server compares each open document's mtime
against disk and pushes a `didChange` for anything that moved (and a `didClose` for anything
that vanished). Cost in the steady state is one `stat` per open document.

Two consequences worth knowing:

- A file the server has **already touched** is always answered from current disk content — this
  holds for workspace-wide tools and for cross-file resolution, not just for the file named in
  the request.
- A file the server has **never opened** is left to rust-analyzer's own watcher, which typically
  needs 5–10 seconds to notice a change. If you edited such a file and need the answer
  immediately, name it in any positional tool call (`symbols` is cheapest) to pull it in.

Detection relies on mtime changing. On filesystems with coarse (1-second) timestamp granularity
an edit made within the same tick as the previous sync can be missed.

## Contributing

The 36-tool surface covers the LSP standards plus rust-analyzer's specials, server-side
composites (`explore_symbol`, `impact`, `get_type_by_name`, `move_file`), and a multi-workspace
registry. Contributions welcome for:

- An FS watcher (plus `workspace/didChangeWatchedFiles`) so that files the server has never
  opened also reflect external edits immediately, instead of waiting out rust-analyzer's own
  watcher — see [External edits](#external-edits).
- A null-result hint for position tools when the position points at whitespace/a keyword
  rather than an identifier.
- More configuration knobs (snippet context lines per workspace, additional `cargo` subcommand
  whitelist entries) via CLI args or a config file.
- Property-based and stress tests for the new composite tools.
- Better triage of crash recovery (the current restart re-opens documents but doesn't
  re-prime cachePriming progress).

### Development Guidelines

- Use `cargo fmt` for consistent formatting
- Run `cargo clippy` for linting
- Add tests for new functionality  
- Update documentation for new tools
- Follow Rust async best practices with Tokio

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

