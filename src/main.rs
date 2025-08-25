use anyhow::{anyhow, Result};
use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};

// MCP Protocol structures
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

// LSP message structures
#[derive(Debug, Serialize, Deserialize)]
struct LSPRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LSPResponse {
    jsonrpc: String,
    id: Option<u64>,
    result: Option<Value>,
    error: Option<Value>,
}

// LSP Client for rust-analyzer
pub struct RustAnalyzerClient {
    process: Option<Child>,
    request_id: Arc<Mutex<u64>>,
    workspace_root: PathBuf,
    stdin: Option<BufWriter<tokio::process::ChildStdin>>,
    pending_requests: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    initialized: bool,
    open_documents: Arc<Mutex<HashSet<String>>>,
}

impl RustAnalyzerClient {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            process: None,
            request_id: Arc::new(Mutex::new(1)),
            workspace_root,
            stdin: None,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            initialized: false,
            open_documents: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        info!(
            "Starting rust-analyzer process in workspace: {}",
            self.workspace_root.display()
        );

        // Find rust-analyzer executable
        let rust_analyzer_path = which::which("rust-analyzer")
            .map_err(|e| anyhow!("Failed to find rust-analyzer in PATH: {}. Please ensure rust-analyzer is installed.", e))?;

        info!("Using rust-analyzer at: {}", rust_analyzer_path.display());

        let mut child = Command::new(rust_analyzer_path)
            .current_dir(&self.workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("Failed to start rust-analyzer: {}", e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to get stderr"))?;

        self.stdin = Some(BufWriter::new(stdin));

        // Log stderr in background
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buffer = String::new();
            loop {
                buffer.clear();
                match reader.read_line(&mut buffer).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if !buffer.trim().is_empty() {
                            debug!("rust-analyzer stderr: {}", buffer.trim());
                        }
                    }
                    Err(e) => {
                        error!("Error reading rust-analyzer stderr: {}", e);
                        break;
                    }
                }
            }
        });

        // Start response handler task
        let pending = Arc::clone(&self.pending_requests);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut buffer = String::new();

            loop {
                buffer.clear();
                match reader.read_line(&mut buffer).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        if buffer.trim().is_empty() {
                            continue;
                        }

                        if buffer.starts_with("Content-Length: ") {
                            let length: usize = buffer[16..].trim().parse().unwrap_or(0);

                            // Read the empty line
                            buffer.clear();
                            let _ = reader.read_line(&mut buffer).await;

                            // Read the JSON content
                            let mut json_buffer = vec![0u8; length];
                            if (tokio::io::AsyncReadExt::read_exact(&mut reader, &mut json_buffer)
                                .await)
                                .is_ok()
                            {
                                let response_str = String::from_utf8_lossy(&json_buffer);
                                info!("Received LSP response: {}", response_str);

                                if let Ok(response) =
                                    serde_json::from_slice::<LSPResponse>(&json_buffer)
                                {
                                    if let Some(id) = response.id {
                                        let mut pending_lock = pending.lock().await;
                                        if let Some(sender) = pending_lock.remove(&id) {
                                            if let Some(error) = response.error {
                                                error!("LSP error for request {}: {}", id, error);
                                                let _ = sender.send(json!(null));
                                            } else {
                                                let result = response.result.unwrap_or(json!(null));
                                                info!(
                                                    "Sending result for request {}: {:?}",
                                                    id, result
                                                );
                                                let _ = sender.send(result);
                                            }
                                        }
                                    }
                                } else {
                                    error!(
                                        "Failed to parse LSP response: {}",
                                        String::from_utf8_lossy(&json_buffer)
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Error reading from rust-analyzer: {}", e);
                        break;
                    }
                }
            }
        });

        self.process = Some(child);

        // Initialize LSP
        self.initialize().await?;
        self.initialized = true;

        info!("rust-analyzer client started and initialized");
        Ok(())
    }

    async fn send_notification(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params.unwrap_or(json!({}))
        });

        let content = serde_json::to_string(&notification)?;
        let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        info!("Sending LSP notification: {}", method);

        if let Some(stdin) = &mut self.stdin {
            stdin.write_all(message.as_bytes()).await?;
            stdin.flush().await?;
            Ok(())
        } else {
            Err(anyhow!("No stdin available"))
        }
    }

    async fn send_request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let mut request_id_lock = self.request_id.lock().await;
        let id = *request_id_lock;
        *request_id_lock += 1;
        drop(request_id_lock);

        let request = LSPRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params: params.clone(),
        };

        let content = serde_json::to_string(&request)?;
        let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        info!("Sending LSP request: {} with params: {:?}", method, params);

        if let Some(stdin) = &mut self.stdin {
            stdin.write_all(message.as_bytes()).await?;
            stdin.flush().await?;
        } else {
            return Err(anyhow!("No stdin available"));
        }

        // Set up response channel
        let (tx, rx) = oneshot::channel();
        self.pending_requests.lock().await.insert(id, tx);

        // Wait for response with timeout
        tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| anyhow!("Request timeout"))?
            .map_err(|_| anyhow!("Request cancelled"))
    }

    async fn initialize(&mut self) -> Result<()> {
        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": format!("file://{}", self.workspace_root.display()),
            "capabilities": {
                "textDocument": {
                    "hover": {
                        "contentFormat": ["markdown", "plaintext"]
                    },
                    "completion": {
                        "completionItem": {
                            "snippetSupport": true
                        }
                    },
                    "definition": {
                        "linkSupport": true
                    },
                    "references": {},
                    "documentSymbol": {},
                    "codeAction": {},
                    "formatting": {}
                },
                "workspace": {
                    "didChangeConfiguration": {
                        "dynamicRegistration": false
                    }
                }
            }
        });

        self.send_request("initialize", Some(init_params)).await?;
        self.send_notification("initialized", Some(json!({})))
            .await?;

        Ok(())
    }

    pub async fn open_document(&mut self, uri: &str, content: &str) -> Result<()> {
        // Check if document is already open
        {
            let open_docs = self.open_documents.lock().await;
            if open_docs.contains(uri) {
                info!("Document already open: {}", uri);
                return Ok(());
            }
        }

        info!("Opening document: {}", uri);
        let params = json!({
            "textDocument": {
                "uri": uri,
                "languageId": "rust",
                "version": 1,
                "text": content
            }
        });

        self.send_notification("textDocument/didOpen", Some(params))
            .await?;

        // Mark document as open
        {
            let mut open_docs = self.open_documents.lock().await;
            open_docs.insert(uri.to_string());
        }

        // Give rust-analyzer time to process the document
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        Ok(())
    }

    pub async fn hover(&mut self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        self.send_request("textDocument/hover", Some(params)).await
    }

    pub async fn definition(&mut self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        self.send_request("textDocument/definition", Some(params))
            .await
    }

    pub async fn references(&mut self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true }
        });

        self.send_request("textDocument/references", Some(params))
            .await
    }

    pub async fn completion(&mut self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        self.send_request("textDocument/completion", Some(params))
            .await
    }

    pub async fn document_symbols(&mut self, uri: &str) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri }
        });

        self.send_request("textDocument/documentSymbol", Some(params))
            .await
    }

    pub async fn formatting(&mut self, uri: &str) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "options": {
                "tabSize": 4,
                "insertSpaces": true
            }
        });

        self.send_request("textDocument/formatting", Some(params))
            .await
    }

    pub async fn code_actions(
        &mut self,
        uri: &str,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
    ) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": start_line, "character": start_char },
                "end": { "line": end_line, "character": end_char }
            },
            "context": { "diagnostics": [] }
        });

        self.send_request("textDocument/codeAction", Some(params))
            .await
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        if self.initialized {
            let _ = self.send_request("shutdown", None).await;
            let _ = self.send_notification("exit", None).await;
        }

        if let Some(mut process) = self.process.take() {
            let _ = process.kill().await;
        }

        // Clear open documents
        self.open_documents.lock().await.clear();
        self.initialized = false;
        Ok(())
    }
}

// Main MCP Server
pub struct RustAnalyzerMCPServer {
    client: Option<RustAnalyzerClient>,
    workspace_root: PathBuf,
}

impl Default for RustAnalyzerMCPServer {
    fn default() -> Self {
        Self::new()
    }
}

impl RustAnalyzerMCPServer {
    pub fn new() -> Self {
        Self {
            client: None,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    pub fn with_workspace(workspace_root: PathBuf) -> Self {
        Self {
            client: None,
            workspace_root,
        }
    }

    async fn ensure_client_started(&mut self) -> Result<()> {
        if self.client.is_none() {
            let mut client = RustAnalyzerClient::new(self.workspace_root.clone());
            client.start().await?;
            self.client = Some(client);
        }
        Ok(())
    }

    async fn open_document_if_needed(&mut self, file_path: &str) -> Result<String> {
        let absolute_path = self.workspace_root.join(file_path);
        let uri = format!("file://{}", absolute_path.display());
        let content = tokio::fs::read_to_string(&absolute_path)
            .await
            .map_err(|e| anyhow!("Failed to read file {}: {}", file_path, e))?;

        if let Some(client) = &mut self.client {
            client.open_document(&uri, &content).await?;
        }

        Ok(uri)
    }

    fn get_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "rust_analyzer_hover".to_string(),
                description:
                    "Get hover information for a symbol at a specific position in a Rust file"
                        .to_string(),
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
                description: "Get code completion suggestions at a specific position".to_string(),
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
        ]
    }

    async fn handle_tool_call(&mut self, tool_name: &str, args: Value) -> Result<ToolResult> {
        self.ensure_client_started().await?;

        match tool_name {
            "rust_analyzer_hover" => self.handle_hover(args).await,
            "rust_analyzer_definition" => self.handle_definition(args).await,
            "rust_analyzer_references" => self.handle_references(args).await,
            "rust_analyzer_completion" => self.handle_completion(args).await,
            "rust_analyzer_symbols" => self.handle_symbols(args).await,
            "rust_analyzer_format" => self.handle_format(args).await,
            "rust_analyzer_code_actions" => self.handle_code_actions(args).await,
            "rust_analyzer_set_workspace" => self.handle_set_workspace(args).await,
            _ => Err(anyhow!("Unknown tool: {}", tool_name)),
        }
    }

    async fn handle_hover(&mut self, args: Value) -> Result<ToolResult> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;
        let line = args["line"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing line"))? as u32;
        let character = args["character"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing character"))? as u32;

        let uri = self.open_document_if_needed(file_path).await?;
        let result = self
            .client
            .as_mut()
            .unwrap()
            .hover(&uri, line, character)
            .await?;

        Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: serde_json::to_string_pretty(&result)?,
            }],
        })
    }

    async fn handle_definition(&mut self, args: Value) -> Result<ToolResult> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;
        let line = args["line"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing line"))? as u32;
        let character = args["character"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing character"))? as u32;

        let uri = self.open_document_if_needed(file_path).await?;
        let result = self
            .client
            .as_mut()
            .unwrap()
            .definition(&uri, line, character)
            .await?;

        Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: serde_json::to_string_pretty(&result)?,
            }],
        })
    }

    async fn handle_references(&mut self, args: Value) -> Result<ToolResult> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;
        let line = args["line"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing line"))? as u32;
        let character = args["character"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing character"))? as u32;

        let uri = self.open_document_if_needed(file_path).await?;
        let result = self
            .client
            .as_mut()
            .unwrap()
            .references(&uri, line, character)
            .await?;

        Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: serde_json::to_string_pretty(&result)?,
            }],
        })
    }

    async fn handle_completion(&mut self, args: Value) -> Result<ToolResult> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;
        let line = args["line"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing line"))? as u32;
        let character = args["character"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing character"))? as u32;

        let uri = self.open_document_if_needed(file_path).await?;
        let result = self
            .client
            .as_mut()
            .unwrap()
            .completion(&uri, line, character)
            .await?;

        Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: serde_json::to_string_pretty(&result)?,
            }],
        })
    }

    async fn handle_symbols(&mut self, args: Value) -> Result<ToolResult> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;

        debug!("Getting symbols for file: {}", file_path);
        let uri = self.open_document_if_needed(file_path).await?;
        debug!("Document opened with URI: {}", uri);

        let result = self.client.as_mut().unwrap().document_symbols(&uri).await?;
        debug!("Document symbols result: {:?}", result);

        Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: serde_json::to_string_pretty(&result)?,
            }],
        })
    }

    async fn handle_format(&mut self, args: Value) -> Result<ToolResult> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;

        let uri = self.open_document_if_needed(file_path).await?;
        let result = self.client.as_mut().unwrap().formatting(&uri).await?;

        Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: serde_json::to_string_pretty(&result)?,
            }],
        })
    }

    async fn handle_code_actions(&mut self, args: Value) -> Result<ToolResult> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;
        let line = args["line"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing line"))? as u32;
        let character = args["character"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing character"))? as u32;
        let end_line = args["end_line"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing end_line"))? as u32;
        let end_character = args["end_character"]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing end_character"))? as u32;

        let uri = self.open_document_if_needed(file_path).await?;
        let result = self
            .client
            .as_mut()
            .unwrap()
            .code_actions(&uri, line, character, end_line, end_character)
            .await?;

        Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: serde_json::to_string_pretty(&result)?,
            }],
        })
    }

    async fn handle_set_workspace(&mut self, args: Value) -> Result<ToolResult> {
        let workspace_path = args["workspace_path"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing workspace_path"))?;

        // Shutdown existing client
        if let Some(client) = &mut self.client {
            client.shutdown().await?;
        }
        self.client = None;

        // Set new workspace
        self.workspace_root = PathBuf::from(workspace_path);

        Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: format!("Workspace set to: {}", self.workspace_root.display()),
            }],
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        info!("Starting rust-analyzer MCP server");

        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut writer = BufWriter::new(stdout);

        // Handle shutdown signals
        let running = Arc::new(Mutex::new(true));
        let running_clone = Arc::clone(&running);

        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Received shutdown signal");
            *running_clone.lock().await = false;
        });

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }

                    if let Ok(request) = serde_json::from_str::<MCPRequest>(line) {
                        debug!("Received request: {}", request.method);
                        let response = self.handle_request(request).await;
                        let response_json = serde_json::to_string(&response)?;
                        writer.write_all(response_json.as_bytes()).await?;
                        writer.write_all(b"\n").await?;
                        writer.flush().await?;
                    } else {
                        debug!("Failed to parse request: {}", line);
                    }
                }
                Err(e) => {
                    error!("Error reading from stdin: {}", e);
                    break;
                }
            }

            // Check if we should stop
            if !*running.lock().await {
                break;
            }
        }

        // Cleanup
        info!("Shutting down");
        if let Some(client) = &mut self.client {
            let _ = client.shutdown().await;
        }

        Ok(())
    }

    async fn handle_request(&mut self, request: MCPRequest) -> MCPResponse {
        match request.method.as_str() {
            "initialize" => MCPResponse::Success {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: json!({
                    "protocolVersion": "0.1.0",
                    "serverInfo": {
                        "name": "rust-analyzer-mcp",
                        "version": "0.1.0"
                    },
                    "capabilities": {
                        "tools": {}
                    }
                }),
            },
            "tools/list" => MCPResponse::Success {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: json!({
                    "tools": Self::get_tools()
                }),
            },
            "tools/call" => {
                if let Some(params) = request.params {
                    let tool_name = params["name"].as_str().unwrap_or("");
                    let args = params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));

                    match self.handle_tool_call(tool_name, args).await {
                        Ok(result) => MCPResponse::Success {
                            jsonrpc: "2.0".to_string(),
                            id: request.id,
                            result: serde_json::to_value(result).unwrap(),
                        },
                        Err(e) => {
                            error!("Tool call error: {}", e);
                            MCPResponse::Error {
                                jsonrpc: "2.0".to_string(),
                                id: request.id,
                                error: MCPError {
                                    code: -1,
                                    message: e.to_string(),
                                    data: None,
                                },
                            }
                        }
                    }
                } else {
                    MCPResponse::Error {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        error: MCPError {
                            code: -32602,
                            message: "Invalid params".to_string(),
                            data: None,
                        },
                    }
                }
            }
            _ => MCPResponse::Error {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                error: MCPError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                    data: None,
                },
            },
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Get workspace path from command line or use current directory
    let workspace_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    // Create and run the server
    let mut server = RustAnalyzerMCPServer::with_workspace(workspace_path);
    server.run().await?;

    Ok(())
}
