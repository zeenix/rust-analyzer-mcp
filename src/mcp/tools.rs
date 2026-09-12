use crate::protocol::mcp::ToolDefinition;
use serde_json::json;

pub fn get_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "rust_analyzer_hover".to_string(),
            description: "Get hover information for a symbol at a specific position in a Rust file"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
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
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
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
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_completion".to_string(),
            description: "Get code completion suggestions at a specific position".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
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
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" }
                },
                "required": ["file_path"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_implementation".to_string(),
            description: "Find what implements the trait, or the trait method, at a position. \
                          The one question no text search can answer even in principle: which \
                          type an implementation belongs to is decided by dispatch, not by \
                          anything written at the position, so grep and structural search both \
                          fail on it. Asked on a trait, it finds the types implementing it; on a \
                          trait method, the bodies that implement that method."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_type_definition".to_string(),
            description: "Go to the definition of the TYPE of the thing at a position, rather \
                          than the thing itself. On a variable or a field, this lands on the \
                          type it holds; plain definition would land on the variable's own \
                          declaration."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_incoming_calls".to_string(),
            description: "Find the FUNCTIONS that call the function at a position, each with the \
                          places inside it where the call is made. Different from references, \
                          which lists every mention of the name and leaves working out what \
                          contains each one to you. The position must be on something callable; \
                          the call-hierarchy item it resolves to is returned alongside the \
                          calls, so you can see what was actually asked about."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_outgoing_calls".to_string(),
            description: "Find the functions called BY the function at a position. Expect hits \
                          outside the workspace: a call into a dependency or the standard \
                          library resolves to that crate's own sources."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_expand_macro".to_string(),
            description: "Expand the macro call at a position and return the code it becomes. \
                          Macro-generated code exists nowhere in the source, so reading the \
                          files -- by eye, by grep, or by structural search -- cannot show it; \
                          hover does not show it either. Use it when a type, method or trait \
                          impl appears to come from nowhere."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based), on the macro's name" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_related_tests".to_string(),
            description: "Find the tests that exercise the symbol at a position. An empty answer \
                          means rust-analyzer found no test reaching this symbol, which is worth \
                          knowing before changing it -- but it is worked out from calls, so a \
                          test reaching it only through a trait object or a macro may not \
                          appear."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" }
                },
                "required": ["file_path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_runnables".to_string(),
            description: "Get the exact cargo commands that run what is in a file -- its tests, \
                          its binary, its doctests -- each with the arguments and environment \
                          rust-analyzer would use. Saves guessing at a test filter or a package \
                          name. Give a position to narrow it to the single test or binary at \
                          that position; omit it for everything in the file."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based). Optional: with a position the answer covers only what is at it" },
                    "character": { "type": "number", "description": "Character position (0-based). Optional, and only used together with line" }
                },
                "required": ["file_path"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_workspace_symbols".to_string(),
            description: "Search the whole workspace for symbols by name, and get the position \
                          of each one -- the only way here to turn a name into the file, line \
                          and character the other tools need. DISCOVERY, NOT A CENSUS: the \
                          query is matched fuzzily (its letters need only appear in order), the \
                          results are scored and only the best few dozen come back, and \
                          rust-analyzer searches its own index rather than the text of the \
                          files. So a symbol missing from these results is NOT evidence that it \
                          does not exist, and unrelated symbols scoring on scattered letters are \
                          expected. Use it to find a symbol you can name; never to prove one \
                          absent, and never to count anything."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Name, or part of one, to search for. An empty query is not a listing: rust-analyzer answers it with nothing" }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_format".to_string(),
            description: "Format a Rust file using rust-analyzer".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" }
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
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Start line number (0-based)" },
                    "character": { "type": "number", "description": "Start character position (0-based)" },
                    "end_line": { "type": "number", "description": "End line number (0-based)" },
                    "end_character": { "type": "number", "description": "End character position (0-based)" }
                },
                "required": ["file_path", "line", "character", "end_line", "end_character"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_rename".to_string(),
            description: "Rename the Rust symbol at a position and return the edits that carry \
                          the rename out across the whole workspace. Changes no files: apply the \
                          returned edits yourself. Renaming a module also renames its file, \
                          which is reported under file_operations. Fails with an explanation if \
                          there is nothing renameable at the position or the symbol is defined \
                          in a dependency, or if rust-analyzer has not finished loading the \
                          workspace -- a rename worked out before then could miss references."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" },
                    "line": { "type": "number", "description": "Line number (0-based)" },
                    "character": { "type": "number", "description": "Character position (0-based)" },
                    "new_name": { "type": "string", "description": "The new name. For a lifetime, include the leading apostrophe" }
                },
                "required": ["file_path", "line", "character", "new_name"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_set_workspace".to_string(),
            description: "Set the workspace root directory for rust-analyzer".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_path": { "type": "string", "description": "Path to the workspace root: relative to the current directory, absolute, or a file:// URI" }
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
                    "file_path": { "type": "string", "description": "Path to the Rust file: relative to the workspace root, absolute, or a file:// URI" }
                },
                "required": ["file_path"]
            }),
        },
        ToolDefinition {
            name: "rust_analyzer_workspace_diagnostics".to_string(),
            description: "Get all compiler diagnostics across the entire workspace".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
    ]
}
