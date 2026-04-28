use crate::protocol::mcp::ToolDefinition;
use serde_json::{json, Value};
use std::sync::OnceLock;

static TOOLS_JSON: OnceLock<Value> = OnceLock::new();

pub fn tools_list_result() -> &'static Value {
    TOOLS_JSON.get_or_init(|| json!({ "tools": build_tools() }))
}

fn build_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "rust_analyzer_hover".to_string(),
            description: "Get hover information for a symbol at a specific position in a Rust file. Markdown output is capped at ~5 KB by default; pass verbose=true for the full content."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" },
                    "verbose": { "type": "boolean", "description": "If true, return the full hover content without truncation. Default: false." }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_definition".to_string(),
            description: "Go to definition of a symbol at a specific position".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_references".to_string(),
            description: "Find all references to a symbol at a specific position".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_completion".to_string(),
            description: "Get code completion suggestions at a specific position. By default, items are sorted by sortText and capped at 50; pass verbose=true to remove the cap or limit=N to override.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" },
                    "verbose": { "type": "boolean", "description": "If true, return all completion items without truncation. Default: false." },
                    "limit": { "type": "number", "description": "Maximum number of items to return. Default: 50, cap: 1000." }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_symbols".to_string(),
            description: "Get document symbols (functions, structs, etc.) for a Rust file"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" }
                },
                "required": ["file_path"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_format".to_string(),
            description: "Format a Rust file using rust-analyzer".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" }
                },
                "required": ["file_path"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_code_actions".to_string(),
            description: "Get available code actions for a range in a Rust file".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Start line number (0-based)" },
                    "character": { "type": "number", "description": "Start character position (0-based)" },
                    "end_line": { "type": "number", "description": "End line number (0-based)" },
                    "end_character": { "type": "number", "description": "End character position (0-based)" }
                },
                "required": ["file_path", "line", "character", "end_line", "end_character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_set_workspace".to_string(),
            description: "Set the workspace root directory for rust-analyzer".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_path": { "type": "string", "description": "Path to the workspace root" }
                },
                "required": ["workspace_path"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_diagnostics".to_string(),
            description: "Get compiler diagnostics (errors, warnings, hints) for a Rust file"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" }
                },
                "required": ["file_path"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_workspace_diagnostics".to_string(),
            description: "Get all compiler diagnostics across the entire workspace. Files are paginated (default 50 files per page); the response includes { workspace, files, summary, pagination: { total_files, returned_files, next_cursor? } }. Use cursor for next page, limit to override, or verbose=true for all files (capped at 1000). The `summary` totals always cover the whole workspace, not just the page.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "verbose": { "type": "boolean", "description": "If true, return all files (subject to a 1000-file absolute cap). Default: false." },
                    "limit": { "type": "number", "description": "Page size in files. Default: 50, cap: 1000." },
                    "cursor": { "type": "string", "description": "Opaque pagination cursor returned by a previous call. Omit to start from the beginning." }
                }
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_rename".to_string(),
            description: "Rename a symbol at a specific position. Returns a WorkspaceEdit with the changes that should be applied.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" },
                    "new_name": { "type": "string", "description": "The new symbol name" }
                },
                "required": ["file_path", "line", "character", "new_name"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_prepare_rename".to_string(),
            description: "Check whether a symbol at a specific position can be renamed; returns the renameable range or null.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_signature_help".to_string(),
            description: "Get parameter/signature information for a function call at a specific position".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_inlay_hints".to_string(),
            description: "Get inlay hints (inferred types, parameter names) for a range in a Rust file".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Start line number (0-based)" },
                    "character": { "type": "number", "description": "Start character position (0-based)" },
                    "end_line": { "type": "number", "description": "End line number (0-based)" },
                    "end_character": { "type": "number", "description": "End character position (0-based)" }
                },
                "required": ["file_path", "line", "character", "end_line", "end_character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_workspace_symbol".to_string(),
            description: "Search for symbols across the entire workspace by name (fuzzy match). Returns { symbols, total, returned, next_cursor? }. Default page size is 100; pass cursor (from a previous next_cursor) to fetch the next page, limit to override page size, or verbose=true to retrieve all matches up to a 1000-item cap. Cursors are best-effort opaque indices and may become stale across re-analysis.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Symbol name query (fuzzy match)" },
                    "verbose": { "type": "boolean", "description": "If true, return all matches (subject to a 1000-item absolute cap). Default: false." },
                    "limit": { "type": "number", "description": "Page size. Default: 100, cap: 1000." },
                    "cursor": { "type": "string", "description": "Opaque pagination cursor returned by a previous call. Omit to start from the beginning." }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_type_definition".to_string(),
            description: "Go to the type definition of a symbol (e.g. for a binding `let x: Foo = ...`, jumps to `Foo`).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_implementation".to_string(),
            description: "Find implementations of a trait, or the trait/struct items at a given position. Returns the locations of `impl` blocks.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_expand_macro".to_string(),
            description: "Expand the macro invocation at the given position and return the resulting source. Useful for understanding `derive`, `format!`, custom proc-macros, etc.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_parent_module".to_string(),
            description: "Locate the parent module(s) of the file or module item at a given position. Returns one or more `Location`s pointing to the `mod foo;` declarations.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_runnables".to_string(),
            description: "List the runnable items (tests, benchmarks, binaries, doctests) in a file. Provide `line`/`character` to limit the result to runnables at that position.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Optional line number (0-based) to limit runnables to a specific position" },
                    "character": { "type": "number", "description": "Optional character position (0-based)" }
                },
                "required": ["file_path"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_related_tests".to_string(),
            description: "Find tests related to the function or item at the given position — typically the tests that exercise that code.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_open_docs".to_string(),
            description: "Resolve the documentation URL (typically docs.rs) for the symbol at the given position. Returns local and/or web URLs, or null if no docs are known.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
    ]
}
