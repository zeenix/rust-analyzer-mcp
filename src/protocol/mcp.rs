use serde::{Deserialize, Serialize};
use serde_json::Value;

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
