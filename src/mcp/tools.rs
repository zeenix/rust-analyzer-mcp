use crate::protocol::mcp::ToolDefinition;
use serde_json::{json, Value};
use std::sync::OnceLock;

static TOOLS_JSON: OnceLock<Value> = OnceLock::new();

pub fn tools_list_result() -> &'static Value {
    TOOLS_JSON.get_or_init(|| json!({ "tools": build_tools() }))
}

/// Names of tools whose semantics are workspace-scoped (i.e. they accept an
/// optional `workspace_id` to target a non-default registered workspace).
/// Workspace-management tools (`set_workspace`/`add_workspace`/etc.) operate
/// on the registry itself and are intentionally excluded.
const WORKSPACE_MANAGEMENT_TOOLS: &[&str] = &[
    "rust_analyzer_set_workspace",
    "rust_analyzer_add_workspace",
    "rust_analyzer_remove_workspace",
    "rust_analyzer_list_workspaces",
];

/// Tools whose output contains one or more LSP `Location`s and that benefit
/// from inline source snippets. The handler post-processes their output via
/// `snippets::enrich_locations` (or `enrich_workspace_diagnostics` for the
/// custom shape) when `include_snippets` is true.
pub(super) const SNIPPET_ENRICHED_TOOLS: &[&str] = &[
    "rust_analyzer_definition",
    "rust_analyzer_references",
    "rust_analyzer_type_definition",
    "rust_analyzer_implementation",
    "rust_analyzer_parent_module",
    "rust_analyzer_runnables",
    "rust_analyzer_related_tests",
    "rust_analyzer_workspace_symbol",
    "rust_analyzer_workspace_diagnostics",
    "rust_analyzer_explore_symbol",
    "rust_analyzer_call_hierarchy_incoming",
    "rust_analyzer_call_hierarchy_outgoing",
    "rust_analyzer_type_hierarchy",
    "rust_analyzer_impact",
    "rust_analyzer_get_type_by_name",
];

fn build_tools() -> Vec<ToolDefinition> {
    let mut tools = build_tools_raw();
    for tool in &mut tools {
        if !WORKSPACE_MANAGEMENT_TOOLS.contains(&tool.name.as_str()) {
            inject_workspace_id(&mut tool.input_schema);
        }
        if SNIPPET_ENRICHED_TOOLS.contains(&tool.name.as_str()) {
            inject_snippet_opts(&mut tool.input_schema);
        }
    }
    tools
}

fn inject_workspace_id(schema: &mut Value) {
    let Some(props) = schema.get_mut("properties").and_then(|p| p.as_object_mut()) else {
        return;
    };
    props.insert(
        "workspace_id".to_string(),
        json!({
            "type": "string",
            "description": "Optional workspace id (from rust_analyzer_list_workspaces). Defaults to the first registered workspace."
        }),
    );
}

fn inject_snippet_opts(schema: &mut Value) {
    let Some(props) = schema.get_mut("properties").and_then(|p| p.as_object_mut()) else {
        return;
    };
    props.insert(
        "include_snippets".to_string(),
        json!({
            "type": "boolean",
            "description": "If true (default), each returned location is enriched with a `snippet` sibling containing surrounding source lines so a follow-up file read is not needed. Set to false to skip the file reads when only positions matter."
        }),
    );
    props.insert(
        "snippet_context_lines".to_string(),
        json!({
            "type": "number",
            "description": "Lines of context around each location's range when include_snippets is true. Default: 2."
        }),
    );
}

// ─── Schema helpers ───────────────────────────────────────────────────────────

fn file_path_prop() -> Value {
    json!({ "type": "string", "description": "Path to the Rust file" })
}

fn line_prop(desc: &str) -> Value {
    json!({ "type": "number", "description": desc })
}

fn char_prop(desc: &str) -> Value {
    json!({ "type": "number", "description": desc })
}

/// Schema with just a file_path (e.g. document_symbols, format).
fn file_only_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "file_path": file_path_prop() },
        "required": ["file_path"]
    })
}

/// Schema with file_path + (line, character).
fn position_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file_path": file_path_prop(),
            "line": line_prop("Line number (0-based)"),
            "character": char_prop("Character position (0-based)")
        },
        "required": ["file_path", "line", "character"]
    })
}

/// Schema with file_path + start (line, character) + end (end_line, end_character).
fn range_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file_path": file_path_prop(),
            "line": line_prop("Start line number (0-based)"),
            "character": char_prop("Start character position (0-based)"),
            "end_line": line_prop("End line number (0-based)"),
            "end_character": char_prop("End character position (0-based)")
        },
        "required": ["file_path", "line", "character", "end_line", "end_character"]
    })
}

fn tool(name: &str, description: &str, schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: schema,
    }
}

fn build_tools_raw() -> Vec<ToolDefinition> {
    vec![
        tool(
            "rust_analyzer_hover",
            "Get hover information for a symbol at a specific position in a Rust file. Markdown output is capped at ~5 KB by default; pass verbose=true for the full content.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": file_path_prop(),
                    "line": line_prop("Line number (0-based)"),
                    "character": char_prop("Character position (0-based)"),
                    "verbose": { "type": "boolean", "description": "If true, return the full hover content without truncation. Default: false." }
                },
                "required": ["file_path", "line", "character"]
            }),
        ),
        tool(
            "rust_analyzer_definition",
            "Go to definition of a symbol at a specific position",
            position_schema(),
        ),
        tool(
            "rust_analyzer_references",
            "Find all references to a symbol at a specific position",
            position_schema(),
        ),
        tool(
            "rust_analyzer_completion",
            "Get code completion suggestions at a specific position. By default, items are sorted by sortText and capped at 50; pass verbose=true to remove the cap or limit=N to override.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": file_path_prop(),
                    "line": line_prop("Line number (0-based)"),
                    "character": char_prop("Character position (0-based)"),
                    "verbose": { "type": "boolean", "description": "If true, return all completion items without truncation. Default: false." },
                    "limit": { "type": "number", "description": "Maximum number of items to return. Default: 50, cap: 1000." }
                },
                "required": ["file_path", "line", "character"]
            }),
        ),
        tool(
            "rust_analyzer_symbols",
            "Get document symbols (functions, structs, etc.) for a Rust file",
            file_only_schema(),
        ),
        tool(
            "rust_analyzer_format",
            "Format a Rust file using rust-analyzer",
            file_only_schema(),
        ),
        tool(
            "rust_analyzer_code_actions",
            "Get available code actions for a range in a Rust file",
            range_schema(),
        ),
        tool(
            "rust_analyzer_set_workspace",
            "Set the workspace root directory for rust-analyzer",
            json!({
                "type": "object",
                "properties": {
                    "workspace_path": { "type": "string", "description": "Path to the workspace root" }
                },
                "required": ["workspace_path"]
            }),
        ),
        tool(
            "rust_analyzer_diagnostics",
            "Get compiler diagnostics (errors, warnings, hints) for a Rust file",
            file_only_schema(),
        ),
        tool(
            "rust_analyzer_workspace_diagnostics",
            "Get all compiler diagnostics across the entire workspace. Files are paginated (default 50 files per page); the response includes { workspace, files, summary, pagination: { total_files, returned_files, next_cursor? } }. Use cursor for next page, limit to override, or verbose=true for all files (capped at 1000). The `summary` totals always cover the whole workspace, not just the page.",
            json!({
                "type": "object",
                "properties": {
                    "verbose": { "type": "boolean", "description": "If true, return all files (subject to a 1000-file absolute cap). Default: false." },
                    "limit": { "type": "number", "description": "Page size in files. Default: 50, cap: 1000." },
                    "cursor": { "type": "string", "description": "Opaque pagination cursor returned by a previous call. Omit to start from the beginning." }
                }
            }),
        ),
        tool(
            "rust_analyzer_rename",
            "Rename a symbol at a specific position. Returns a WorkspaceEdit with the changes that should be applied.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": file_path_prop(),
                    "line": line_prop("Line number (0-based)"),
                    "character": char_prop("Character position (0-based)"),
                    "new_name": { "type": "string", "description": "The new symbol name" }
                },
                "required": ["file_path", "line", "character", "new_name"]
            }),
        ),
        tool(
            "rust_analyzer_prepare_rename",
            "Check whether a symbol at a specific position can be renamed; returns the renameable range or null.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_signature_help",
            "Get parameter/signature information for a function call at a specific position",
            position_schema(),
        ),
        tool(
            "rust_analyzer_inlay_hints",
            "Get inlay hints (inferred types, parameter names) for a range in a Rust file",
            range_schema(),
        ),
        tool(
            "rust_analyzer_workspace_symbol",
            "Search for symbols across the entire workspace by name (fuzzy match). Returns { symbols, total, returned, next_cursor? }. Default page size is 100; pass cursor (from a previous next_cursor) to fetch the next page, limit to override page size, or verbose=true to retrieve all matches up to a 1000-item cap. Cursors are best-effort opaque indices and may become stale across re-analysis.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Symbol name query (fuzzy match)" },
                    "verbose": { "type": "boolean", "description": "If true, return all matches (subject to a 1000-item absolute cap). Default: false." },
                    "limit": { "type": "number", "description": "Page size. Default: 100, cap: 1000." },
                    "cursor": { "type": "string", "description": "Opaque pagination cursor returned by a previous call. Omit to start from the beginning." }
                },
                "required": ["query"]
            }),
        ),
        tool(
            "rust_analyzer_type_definition",
            "Go to the type definition of a symbol (e.g. for a binding `let x: Foo = ...`, jumps to `Foo`).",
            position_schema(),
        ),
        tool(
            "rust_analyzer_implementation",
            "Find implementations of a trait, or the trait/struct items at a given position. Returns the locations of `impl` blocks.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_expand_macro",
            "Expand the macro invocation at the given position and return the resulting source. Useful for understanding `derive`, `format!`, custom proc-macros, etc.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_parent_module",
            "Locate the parent module(s) of the file or module item at a given position. Returns one or more `Location`s pointing to the `mod foo;` declarations.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_runnables",
            "List the runnable items (tests, benchmarks, binaries, doctests) in a file, reshaped for direct consumption. Returns { runnables: [{ kind, label, fq_name?, cargo_args, location, can_run_via_mcp }], total, can_run_via_mcp }. `kind` is the leading word from the rust-analyzer label (e.g. \"test\", \"test-mod\", \"bench\", \"run\", \"doctest\"). `cargo_args` is a flat argv ready for `rust_analyzer_run_runnable` — pass it through verbatim. `can_run_via_mcp` reflects the RUST_ANALYZER_MCP_ALLOW_RUN env gate; when false, copy `cargo_args` into the host's shell tool instead. Provide `line`/`character` to filter to runnables at that position.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": file_path_prop(),
                    "line": { "type": "number", "description": "Optional line number (0-based) to limit runnables to a specific position" },
                    "character": { "type": "number", "description": "Optional character position (0-based)" }
                },
                "required": ["file_path"]
            }),
        ),
        tool(
            "rust_analyzer_run_runnable",
            "Execute a cargo runnable (test, bench, run, build, check, clippy, doc, nextest) inside the workspace root. Pass the `cargo_args` array from rust_analyzer_runnables verbatim. Disabled by default; set RUST_ANALYZER_MCP_ALLOW_RUN=1 in the MCP host environment to enable. Output: { exit_code, stdout, stderr, elapsed_secs, timed_out, stdout_truncated?, stderr_truncated? }. Stdout/stderr are each capped at 5 KiB. Default timeout 60 s, override via `timeout_secs` (max 600). Cancellation flows through MCP — cancelling the request kills the cargo subprocess.",
            json!({
                "type": "object",
                "properties": {
                    "cargo_args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Argv for `cargo`. First element must be one of: test, bench, run, build, check, clippy, doc, nextest."
                    },
                    "timeout_secs": {
                        "type": "number",
                        "description": "Subprocess timeout in seconds. Default: 60, hard cap: 600."
                    }
                },
                "required": ["cargo_args"]
            }),
        ),
        tool(
            "rust_analyzer_related_tests",
            "Find tests related to the function or item at the given position — typically the tests that exercise that code.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_open_docs",
            "Resolve the documentation URL (typically docs.rs) for the symbol at the given position. Returns local and/or web URLs, or null if no docs are known.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_syntax_tree",
            "Render rust-analyzer's parsed syntax tree for a Rust file as a printed string. Without a range, returns the whole-file tree; passing all four range coords (line/character/end_line/end_character) narrows to the subtree covering that range. Useful for debugging parser behavior, macro expansion shape, attribute placement.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": file_path_prop(),
                    "line": { "type": "number", "description": "Optional range-start line (0-based). Provide all four range coords or none." },
                    "character": { "type": "number", "description": "Optional range-start character (0-based)." },
                    "end_line": { "type": "number", "description": "Optional range-end line (0-based)." },
                    "end_character": { "type": "number", "description": "Optional range-end character (0-based)." }
                },
                "required": ["file_path"]
            }),
        ),
        tool(
            "rust_analyzer_view_hir",
            "Return rust-analyzer's HIR (high-level IR) for the function or item at the given position, as a debug-printed string. Position must be inside a function body for non-null output.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_view_mir",
            "Return rust-analyzer's MIR (mid-level IR) for the function at the given position, as a debug-printed string. Position must be inside a function body for non-null output.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_explore_symbol",
            "One-shot symbol exploration: in a single round-trip returns hover, definition, type_definition, parent_module, and a sample of up to 5 references for the symbol at the given position. Use this whenever you want to *understand* a symbol — it replaces the typical 4-5 follow-up tool calls. Locations come pre-enriched with source snippets (toggle via include_snippets). The references sample is wrapped as { items, total, shown }; if references take longer than 2 seconds, the rest of the response still returns and `references_timed_out: true` is added.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_call_hierarchy_incoming",
            "Find every caller of the function/method at the given position. Returns { items: [{ item, incoming: [{ from, call_sites: [{uri, range}] }] }], total } where `item` is the prepared call-hierarchy node (one item is typical, multiple appear for trait methods with several impls), `from` is each caller, and `call_sites` are the actual call expressions inside the caller. Locations come pre-enriched with snippets. Prefer this over `references` when you need callers specifically — it filters out string-match noise from comments/strings.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_call_hierarchy_outgoing",
            "Find every function/method called by the function at the given position. Returns { items: [{ item, outgoing: [{ to, call_sites: [{uri, range}] }] }], total } where `to` is each callee and `call_sites` are the call expressions inside the current item. Locations come pre-enriched with snippets.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_type_hierarchy",
            "Walk the type hierarchy at the given position (traits, impls, structurally related types). Returns { items: [{ item, supertypes?, subtypes? }], total }. `direction` defaults to \"both\"; pass \"supertypes\" for parents only or \"subtypes\" for children/implementors only. Use this for trait introspection (\"who supertypes/implements this trait?\") — distinct from `implementation`, which targets a specific impl block.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": file_path_prop(),
                    "line": line_prop("Line number (0-based)"),
                    "character": char_prop("Character position (0-based)"),
                    "direction": {
                        "type": "string",
                        "enum": ["supertypes", "subtypes", "both"],
                        "description": "Which direction to walk. Default: \"both\"."
                    }
                },
                "required": ["file_path", "line", "character"]
            }),
        ),
        tool(
            "rust_analyzer_get_type_by_name",
            "Look up a type or item by its name path without needing a file/position. Accepts a `name` like \"Calculator\" or \"crate::auth::User\" or \"serde_json::Value\". Resolves via workspace_symbol on the last segment, filters to entries whose container matches the path prefix, then runs hover + type_definition on the first match. Returns { matches: [...up to 10 candidates], total, shown, primary?: { hover, type_definition, location } }. The `primary` block is omitted when no match is found. Useful for code generation: \"can I use X, what methods does it have?\" without already knowing where it's defined.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Symbol path to look up (e.g. \"Calculator\", \"crate::auth::User\", \"serde_json::Value\"). Segments are split on \"::\"."
                    }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "rust_analyzer_impact",
            "Estimate the blast radius of changing the symbol at the given position. In a single round-trip aggregates four buckets: `references` (textual usages), `callers` (semantic incoming calls), `implementors` (type-hierarchy subtypes — for traits, who implements them), and `tests` (related test items). Each bucket has shape { items, total, shown, timed_out? }; items are capped at 10 per bucket but `total` reflects the full count. Per-bucket 2 s timeouts ensure one slow workspace-wide search can't block the others. Use this before any non-trivial refactor.",
            position_schema(),
        ),
        tool(
            "rust_analyzer_move_file",
            "Move a file or directory inside the workspace, fixing up `mod`-declarations and `use`-imports along the way. Sequence: rust-analyzer computes the WorkspaceEdit via workspace/willRenameFiles, the server applies that edit on disk, then physically renames the file. Returns { moved: { from, to }, edits: { files_changed: [{ uri, edits_applied }], total_edits } }. Both paths are relative to the workspace root unless absolute; both must stay inside the workspace. Fails if the destination already exists.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Source path (relative to workspace root or absolute)." },
                    "to": { "type": "string", "description": "Destination path (relative to workspace root or absolute). Parent directories are created if missing." }
                },
                "required": ["from", "to"]
            }),
        ),
        tool(
            "rust_analyzer_add_workspace",
            "Register an additional rust-analyzer workspace. Each workspace runs its own rust-analyzer subprocess and is addressed by an opaque `workspace_id`. The first registered workspace stays the default — every tool call without a `workspace_id` resolves there. Returns `{ workspace_id, root }`.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or relative path to the new workspace root" }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "rust_analyzer_remove_workspace",
            "Unregister a workspace and shut down its rust-analyzer subprocess. The default workspace can be removed; subsequent tool calls without an explicit `workspace_id` will fail with 'No default workspace registered' until a new one is added.",
            json!({
                "type": "object",
                "properties": {
                    "workspace_id": { "type": "string", "description": "Id returned by rust_analyzer_add_workspace or rust_analyzer_list_workspaces" }
                },
                "required": ["workspace_id"]
            }),
        ),
        tool(
            "rust_analyzer_list_workspaces",
            "List every registered workspace with its id, root path, and whether it is the default. Output: { workspaces: [{ workspace_id, root, default }] }.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
    ]
}
