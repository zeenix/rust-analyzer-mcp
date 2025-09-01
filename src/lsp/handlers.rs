use anyhow::Result;
use log::info;
use serde_json::{json, Value};

use super::client::RustAnalyzerClient;

impl RustAnalyzerClient {
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

    pub async fn diagnostics(&mut self, uri: &str) -> Result<Value> {
        // First check if we have stored diagnostics from publishDiagnostics.
        let diag_lock = self.diagnostics.lock().await;
        info!("Looking for diagnostics for URI: {}", uri);
        info!(
            "Available URIs with diagnostics: {:?}",
            diag_lock.keys().collect::<Vec<_>>()
        );
        if let Some(diags) = diag_lock.get(uri) {
            info!("Found {} stored diagnostics for {}", diags.len(), uri);
            return Ok(json!(diags));
        }
        drop(diag_lock);

        info!("No stored diagnostics for {}, trying pull model", uri);
        // If no stored diagnostics, try the pull model as fallback.
        let params = json!({
            "textDocument": { "uri": uri }
        });

        let response = self
            .send_request("textDocument/diagnostic", Some(params))
            .await?;

        // Extract diagnostics from the response.
        if let Some(items) = response.get("items") {
            Ok(items.clone())
        } else {
            Ok(json!([]))
        }
    }

    pub async fn workspace_diagnostics(&mut self) -> Result<Value> {
        // Try workspace/diagnostic if available, otherwise collect from all open documents.
        let params = json!({
            "identifier": "rust-analyzer",
            "previousResultId": null
        });

        match self
            .send_request("workspace/diagnostic", Some(params))
            .await
        {
            Ok(response) => Ok(response),
            Err(_) => {
                // Fallback: return diagnostics for all open documents.
                let mut all_diagnostics = json!({});
                let open_docs = self.open_documents.lock().await.clone();

                for doc_uri in open_docs.iter() {
                    if let Ok(diag) = self.diagnostics(doc_uri).await {
                        all_diagnostics[doc_uri] = diag;
                    }
                }

                Ok(all_diagnostics)
            }
        }
    }

    pub async fn code_actions(
        &mut self,
        uri: &str,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
    ) -> Result<Value> {
        // First, try to get diagnostics for this range.
        let diagnostics = self.diagnostics(uri).await.unwrap_or(json!([]));

        // Filter diagnostics to only those in the requested range.
        let filtered_diagnostics = filter_diagnostics_in_range(&diagnostics, start_line, end_line);

        let params = json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": start_line, "character": start_char },
                "end": { "line": end_line, "character": end_char }
            },
            "context": {
                "diagnostics": filtered_diagnostics,
                "only": ["quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source"]
            }
        });

        self.send_request("textDocument/codeAction", Some(params))
            .await
    }
}

fn filter_diagnostics_in_range(diagnostics: &Value, start_line: u32, end_line: u32) -> Value {
    let Some(diag_array) = diagnostics.as_array() else {
        return json!([]);
    };

    let filtered: Vec<Value> = diag_array
        .iter()
        .filter(|d| {
            let Some(range) = d.get("range") else {
                return false;
            };
            let Some(start) = range.get("start") else {
                return false;
            };
            let Some(end) = range.get("end") else {
                return false;
            };

            let diag_start_line = start.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as u32;
            let diag_end_line = end.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as u32;

            // Check if diagnostic overlaps with requested range.
            diag_start_line <= end_line && diag_end_line >= start_line
        })
        .cloned()
        .collect();

    json!(filtered)
}
