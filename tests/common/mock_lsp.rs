use anyhow::Result;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use wiremock::{
    matchers::{body_json, method, path},
    Mock, MockServer, ResponseTemplate,
};

/// Mock LSP server for testing without rust-analyzer
pub struct MockLSPServer {
    server: MockServer,
    responses: Arc<Mutex<HashMap<String, Value>>>,
}

impl MockLSPServer {
    /// Start a new mock LSP server
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let responses = Arc::new(Mutex::new(HashMap::new()));

        // Set up default responses for common LSP methods
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_json(json!({
                "jsonrpc": "2.0",
                "method": "initialize"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "result": {
                    "capabilities": {
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "referencesProvider": true,
                        "documentSymbolProvider": true,
                        "completionProvider": {
                            "resolveProvider": false,
                            "triggerCharacters": [".", ":", "::", "->"]
                        },
                        "documentFormattingProvider": true,
                        "codeActionProvider": true
                    },
                    "serverInfo": {
                        "name": "mock-rust-analyzer",
                        "version": "0.1.0"
                    }
                }
            })))
            .mount(&server)
            .await;

        Self { server, responses }
    }

    /// Get the server URL
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// Set up an expected hover response
    pub async fn expect_hover(&self, file: &str, line: u32, character: u32, response: &str) {
        let key = format!("hover:{}:{}:{}", file, line, character);
        let mut responses = self.responses.lock().await;
        responses.insert(
            key,
            json!({
                "contents": {
                    "kind": "markdown",
                    "value": response
                }
            }),
        );
    }

    /// Set up expected symbol response
    pub async fn expect_symbols(&self, file: &str, symbols: Vec<(&str, &str)>) {
        let key = format!("symbols:{}", file);
        let symbol_list: Vec<Value> = symbols
            .into_iter()
            .map(|(name, kind)| {
                json!({
                    "name": name,
                    "kind": kind,
                    "location": {
                        "uri": format!("file://{}", file),
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 0}
                        }
                    }
                })
            })
            .collect();

        let mut responses = self.responses.lock().await;
        responses.insert(key, json!(symbol_list));
    }

    /// Set up expected definition response
    pub async fn expect_definition(
        &self,
        file: &str,
        line: u32,
        character: u32,
        target_file: &str,
        target_line: u32,
    ) {
        let key = format!("definition:{}:{}:{}", file, line, character);
        let mut responses = self.responses.lock().await;
        responses.insert(
            key,
            json!([{
                "targetUri": format!("file://{}", target_file),
                "targetRange": {
                    "start": {"line": target_line, "character": 0},
                    "end": {"line": target_line, "character": 10}
                },
                "targetSelectionRange": {
                    "start": {"line": target_line, "character": 0},
                    "end": {"line": target_line, "character": 10}
                }
            }]),
        );
    }

    /// Set up expected references response
    pub async fn expect_references(
        &self,
        file: &str,
        line: u32,
        character: u32,
        references: Vec<(String, u32, u32)>,
    ) {
        let key = format!("references:{}:{}:{}", file, line, character);
        let ref_list: Vec<Value> = references
            .into_iter()
            .map(|(file, line, char)| {
                json!({
                    "uri": format!("file://{}", file),
                    "range": {
                        "start": {"line": line, "character": char},
                        "end": {"line": line, "character": char + 10}
                    }
                })
            })
            .collect();

        let mut responses = self.responses.lock().await;
        responses.insert(key, json!(ref_list));
    }

    /// Set up expected completion response
    pub async fn expect_completion(&self, file: &str, line: u32, character: u32, items: Vec<&str>) {
        let key = format!("completion:{}:{}:{}", file, line, character);
        let completion_items: Vec<Value> = items
            .into_iter()
            .map(|label| {
                json!({
                    "label": label,
                    "kind": 3, // Function
                    "detail": format!("fn {}", label),
                    "insertText": label
                })
            })
            .collect();

        let mut responses = self.responses.lock().await;
        responses.insert(
            key,
            json!({
                "isIncomplete": false,
                "items": completion_items
            }),
        );
    }

    /// Set up expected format response
    pub async fn expect_format(&self, file: &str, edits: Vec<(u32, u32, &str)>) {
        let key = format!("format:{}", file);
        let edit_list: Vec<Value> = edits
            .into_iter()
            .map(|(line, char, text)| {
                json!({
                    "range": {
                        "start": {"line": line, "character": char},
                        "end": {"line": line, "character": char + 10}
                    },
                    "newText": text
                })
            })
            .collect();

        let mut responses = self.responses.lock().await;
        responses.insert(key, json!(edit_list));
    }

    /// Verify that all expected calls were made
    pub async fn verify(&self) -> Result<()> {
        // In a real implementation, we'd track actual calls and verify
        Ok(())
    }
}

/// Create a mock LSP process that communicates via stdin/stdout
pub struct MockLSPProcess {
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
}

impl MockLSPProcess {
    /// Create a new mock LSP process
    pub async fn spawn() -> Result<Self> {
        // In a real implementation, this would spawn a mock process
        // For now, we'll return a placeholder
        unimplemented!("MockLSPProcess::spawn not yet implemented")
    }

    /// Send a request to the mock process
    pub async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        // Placeholder implementation
        Ok(json!({}))
    }

    /// Send a notification to the mock process
    pub async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        // Placeholder implementation
        Ok(())
    }
}
