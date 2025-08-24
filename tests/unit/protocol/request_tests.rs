use proptest::prelude::*;
use serde_json::{from_str, json, to_string, Value};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct MCPRequest {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct MCPResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<MCPError>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct MCPError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[test]
fn test_mcp_request_serialization() {
    let request = MCPRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "initialize".to_string(),
        params: Some(json!({"version": "0.1.0"})),
    };

    let serialized = to_string(&request).unwrap();
    let deserialized: MCPRequest = from_str(&serialized).unwrap();

    assert_eq!(request.method, deserialized.method);
    assert_eq!(request.params, deserialized.params);
    assert_eq!(request.id, deserialized.id);
}

#[test]
fn test_mcp_response_serialization() {
    let response = MCPResponse {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        result: Some(json!({"status": "success"})),
        error: None,
    };

    let serialized = to_string(&response).unwrap();
    let deserialized: MCPResponse = from_str(&serialized).unwrap();

    assert_eq!(response.result, deserialized.result);
    assert!(deserialized.error.is_none());
}

#[test]
fn test_mcp_error_response() {
    let error_response = MCPResponse {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        result: None,
        error: Some(MCPError {
            code: -32601,
            message: "Method not found".to_string(),
            data: Some(json!({"method": "unknown_method"})),
        }),
    };

    let serialized = to_string(&error_response).unwrap();
    let deserialized: MCPResponse = from_str(&serialized).unwrap();

    assert!(deserialized.result.is_none());
    assert!(deserialized.error.is_some());

    let error = deserialized.error.unwrap();
    assert_eq!(error.code, -32601);
    assert_eq!(error.message, "Method not found");
}

#[test]
fn test_notification_without_id() {
    let notification = MCPRequest {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: "textDocument/didOpen".to_string(),
        params: Some(json!({"uri": "file:///test.rs"})),
    };

    let serialized = to_string(&notification).unwrap();
    assert!(!serialized.contains("\"id\""));

    let deserialized: MCPRequest = from_str(&serialized).unwrap();
    assert!(deserialized.id.is_none());
}

#[test]
fn test_tool_call_request() {
    let tool_call = MCPRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(42)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "rust_analyzer_symbols",
            "arguments": {
                "file_path": "src/main.rs"
            }
        })),
    };

    let serialized = to_string(&tool_call).unwrap();
    let deserialized: MCPRequest = from_str(&serialized).unwrap();

    assert_eq!(deserialized.method, "tools/call");
    let params = deserialized.params.unwrap();
    assert_eq!(params["name"], "rust_analyzer_symbols");
    assert_eq!(params["arguments"]["file_path"], "src/main.rs");
}

proptest! {
    #[test]
    fn test_request_roundtrip(
        id in prop::option::of(any::<u64>().prop_map(|v| json!(v))),
        method in "[a-z_/]+",
    ) {
        let request = MCPRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.clone(),
            params: None,
        };

        let serialized = to_string(&request).unwrap();
        let deserialized: MCPRequest = from_str(&serialized).unwrap();

        prop_assert_eq!(request.method, deserialized.method);
        prop_assert_eq!(request.id, deserialized.id);
    }

    #[test]
    fn test_response_roundtrip(
        id in prop::option::of(any::<u64>().prop_map(|v| json!(v))),
        has_error in any::<bool>(),
    ) {
        let response = if has_error {
            MCPResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(MCPError {
                    code: -32000,
                    message: "Test error".to_string(),
                    data: None,
                }),
            }
        } else {
            MCPResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({"test": "data"})),
                error: None,
            }
        };

        let serialized = to_string(&response).unwrap();
        let deserialized: MCPResponse = from_str(&serialized).unwrap();

        prop_assert_eq!(response.id, deserialized.id);
        prop_assert_eq!(response.error.is_some(), deserialized.error.is_some());
        prop_assert_eq!(response.result.is_some(), deserialized.result.is_some());
    }

    #[test]
    fn test_method_names_valid(
        method in prop::string::string_regex("[a-zA-Z][a-zA-Z0-9_/]*").unwrap(),
    ) {
        let request = MCPRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.clone(),
            params: None,
        };

        let serialized = to_string(&request).unwrap();
        let deserialized: MCPRequest = from_str(&serialized).unwrap();

        prop_assert_eq!(request.method, deserialized.method);
    }
}
