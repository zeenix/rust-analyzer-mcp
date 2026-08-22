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
