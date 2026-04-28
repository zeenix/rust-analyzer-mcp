use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC 2.0 standard error codes (subset we use).
pub mod error_codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MCPRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MCPResponse {
    Success {
        jsonrpc: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<Value>,
        result: Value,
    },
    Error {
        jsonrpc: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<Value>,
        error: MCPError,
    },
}

impl MCPResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self::Success {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self::Error {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            error: MCPError {
                code,
                message: message.into(),
                data: None,
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MCPError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ContentItem>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ContentItem {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

impl ContentItem {
    /// The overwhelmingly common case — a text-typed content item — without
    /// having to spell out the `content_type: "text".to_string()` field.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content_type: "text".to_string(),
            text: text.into(),
        }
    }
}
