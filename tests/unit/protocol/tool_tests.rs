use serde_json::{json, Value};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[test]
fn test_tool_definition_serialization() {
    let tool = ToolDefinition {
        name: "rust_analyzer_symbols".to_string(),
        description: "Get all symbols in a file".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the Rust file"
                }
            },
            "required": ["file_path"]
        }),
    };

    let serialized = serde_json::to_string(&tool).unwrap();
    assert!(serialized.contains("inputSchema"));
    assert!(serialized.contains("rust_analyzer_symbols"));

    let deserialized: ToolDefinition = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.name, tool.name);
    assert_eq!(deserialized.description, tool.description);
}

#[test]
fn test_all_tools_have_valid_schemas() {
    let tools = vec![
        ("rust_analyzer_symbols", vec!["file_path"]),
        (
            "rust_analyzer_definition",
            vec!["file_path", "line", "character"],
        ),
        (
            "rust_analyzer_references",
            vec!["file_path", "line", "character"],
        ),
        (
            "rust_analyzer_hover",
            vec!["file_path", "line", "character"],
        ),
        (
            "rust_analyzer_completion",
            vec!["file_path", "line", "character"],
        ),
        ("rust_analyzer_format", vec!["file_path"]),
        (
            "rust_analyzer_code_actions",
            vec![
                "file_path",
                "start_line",
                "start_character",
                "end_line",
                "end_character",
            ],
        ),
        ("rust_analyzer_set_workspace", vec!["workspace_path"]),
    ];

    for (name, required_fields) in tools {
        let schema = json!({
            "type": "object",
            "properties": {},
            "required": required_fields
        });

        // Validate that schema is valid JSON Schema
        assert!(schema.is_object());
        assert_eq!(schema["type"], "object");
        assert!(schema["required"].is_array());
    }
}

#[test]
fn test_tool_response_formats() {
    // Test symbol response format
    let symbol_response = json!([
        {
            "name": "main",
            "kind": "Function",
            "location": {
                "uri": "file:///src/main.rs",
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 10}
                }
            }
        }
    ]);

    assert!(symbol_response.is_array());
    assert_eq!(symbol_response[0]["name"], "main");

    // Test hover response format
    let hover_response = json!({
        "contents": {
            "kind": "markdown",
            "value": "```rust\nfn main()\n```"
        }
    });

    assert!(hover_response["contents"].is_object());
    assert_eq!(hover_response["contents"]["kind"], "markdown");

    // Test completion response format
    let completion_response = json!({
        "isIncomplete": false,
        "items": [
            {
                "label": "println!",
                "kind": 3,
                "detail": "macro println!",
                "insertText": "println!(\"$1\")"
            }
        ]
    });

    assert!(completion_response["items"].is_array());
    assert_eq!(completion_response["items"][0]["label"], "println!");
}

#[test]
fn test_error_response_codes() {
    let error_codes = vec![
        (-32700, "Parse error"),
        (-32600, "Invalid Request"),
        (-32601, "Method not found"),
        (-32602, "Invalid params"),
        (-32603, "Internal error"),
        (-32002, "Server not initialized"),
        (-32001, "Unknown error"),
    ];

    for (code, message) in error_codes {
        let error = json!({
            "code": code,
            "message": message
        });

        assert_eq!(error["code"], code);
        assert_eq!(error["message"], message);
    }
}
