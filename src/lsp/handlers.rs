use anyhow::Result;
use serde_json::{json, Value};
use tracing::debug;

use super::{client::RustAnalyzerClient, error::LspError};

/// Coerce an `LspError` to `Ok(null)` when the predicate matches (used to
/// turn "no symbol here" / "index not ready" into a lookup miss the LLM can
/// recognize), otherwise propagate the error.
fn coerce_null<F>(res: std::result::Result<Value, LspError>, treat_as_null: F) -> Result<Value>
where
    F: Fn(&LspError) -> bool,
{
    match res {
        Ok(v) => Ok(v),
        Err(e) if treat_as_null(&e) => Ok(json!(null)),
        Err(e) => Err(anyhow::Error::new(e)),
    }
}

/// Standard lookup-style coercion: errors that mean "no result for this position".
fn lookup_to_null(res: std::result::Result<Value, LspError>) -> Result<Value> {
    coerce_null(res, LspError::is_no_result)
}

/// Pass through every `LspError` as `anyhow::Error` (no null coercion).
fn strict(res: std::result::Result<Value, LspError>) -> Result<Value> {
    res.map_err(anyhow::Error::new)
}

/// Rename-style coercion: also treats `InvalidParams` as a miss because
/// rust-analyzer reports "no renamable symbol" via -32602 instead of null.
fn rename_lookup_to_null(res: std::result::Result<Value, LspError>) -> Result<Value> {
    coerce_null(res, LspError::is_no_rename_target)
}

impl RustAnalyzerClient {
    pub async fn hover(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(self.send_request("textDocument/hover", Some(params)).await)
    }

    pub async fn definition(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("textDocument/definition", Some(params))
                .await,
        )
    }

    pub async fn references(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true }
        });

        lookup_to_null(
            self.send_request("textDocument/references", Some(params))
                .await,
        )
    }

    pub async fn completion(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("textDocument/completion", Some(params))
                .await,
        )
    }

    pub async fn document_symbols(&self, uri: &str) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri }
        });

        lookup_to_null(
            self.send_request("textDocument/documentSymbol", Some(params))
                .await,
        )
    }

    pub async fn formatting(&self, uri: &str) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "options": {
                "tabSize": 4,
                "insertSpaces": true
            }
        });

        strict(
            self.send_request("textDocument/formatting", Some(params))
                .await,
        )
    }

    pub async fn diagnostics(&self, uri: &str) -> Result<Value> {
        // First check if we have stored diagnostics from publishDiagnostics.
        let diag_lock = self.diagnostics.lock().await;
        if let Some(diags) = diag_lock.get(uri) {
            debug!("Found {} stored diagnostics for {}", diags.len(), uri);
            return Ok(json!(diags));
        }
        drop(diag_lock);

        debug!("No stored diagnostics for {}, trying pull model", uri);
        // If no stored diagnostics, try the pull model as fallback.
        let params = json!({
            "textDocument": { "uri": uri }
        });

        let response = strict(
            self.send_request("textDocument/diagnostic", Some(params))
                .await,
        )?;

        // Extract diagnostics from the response.
        if let Some(items) = response.get("items") {
            Ok(items.clone())
        } else {
            Ok(json!([]))
        }
    }

    pub async fn workspace_diagnostics(&self) -> Result<Value> {
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
                let open_uris: Vec<String> =
                    self.open_documents.lock().await.keys().cloned().collect();

                for doc_uri in &open_uris {
                    if let Ok(diag) = self.diagnostics(doc_uri).await {
                        all_diagnostics[doc_uri] = diag;
                    }
                }

                Ok(all_diagnostics)
            }
        }
    }

    pub async fn rename(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": new_name
        });

        rename_lookup_to_null(self.send_request("textDocument/rename", Some(params)).await)
    }

    /// `workspace/willRenameFiles` — ask rust-analyzer to compute the
    /// `WorkspaceEdit` (mod-decl/import fixes) that should accompany a file
    /// rename. The actual physical rename is the caller's responsibility.
    /// Returns the edit, or `null` if rust-analyzer has no changes to suggest.
    pub async fn will_rename_files(&self, old_uri: &str, new_uri: &str) -> Result<Value> {
        let params = json!({
            "files": [{ "oldUri": old_uri, "newUri": new_uri }]
        });
        lookup_to_null(
            self.send_request("workspace/willRenameFiles", Some(params))
                .await,
        )
    }

    pub async fn prepare_rename(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        rename_lookup_to_null(
            self.send_request("textDocument/prepareRename", Some(params))
                .await,
        )
    }

    pub async fn signature_help(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("textDocument/signatureHelp", Some(params))
                .await,
        )
    }

    pub async fn inlay_hints(
        &self,
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
            }
        });

        lookup_to_null(
            self.send_request("textDocument/inlayHint", Some(params))
                .await,
        )
    }

    pub async fn workspace_symbol(&self, query: &str) -> Result<Value> {
        let params = json!({ "query": query });

        lookup_to_null(self.send_request("workspace/symbol", Some(params)).await)
    }

    pub async fn type_definition(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("textDocument/typeDefinition", Some(params))
                .await,
        )
    }

    pub async fn implementation(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("textDocument/implementation", Some(params))
                .await,
        )
    }

    pub async fn expand_macro(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("rust-analyzer/expandMacro", Some(params))
                .await,
        )
    }

    pub async fn parent_module(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("experimental/parentModule", Some(params))
                .await,
        )
    }

    /// experimental/runnables — list testable / runnable items in a file.
    /// `position` is optional: when `None`, every runnable in the file is
    /// returned; when `Some`, only runnables whose range covers the position.
    pub async fn runnables(&self, uri: &str, position: Option<(u32, u32)>) -> Result<Value> {
        let params = match position {
            Some((line, character)) => json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
            // Field omitted (not null) so rust-analyzer treats it as
            // "all runnables in file" rather than "no position given".
            None => json!({ "textDocument": { "uri": uri } }),
        };

        lookup_to_null(
            self.send_request("experimental/runnables", Some(params))
                .await,
        )
    }

    pub async fn related_tests(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("rust-analyzer/relatedTests", Some(params))
                .await,
        )
    }

    /// rust-analyzer/syntaxTree — returns the parser's view of the source as
    /// a printed syntax tree. With `range = None`, the whole file is rendered;
    /// with `Some((start_line, start_char, end_line, end_char))`, only the
    /// subtree covering that range. Output is a free-form string.
    pub async fn syntax_tree(
        &self,
        uri: &str,
        range: Option<(u32, u32, u32, u32)>,
    ) -> Result<Value> {
        let params = match range {
            Some((sl, sc, el, ec)) => json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": sl, "character": sc },
                    "end":   { "line": el, "character": ec }
                }
            }),
            // Field omitted (not null) so rust-analyzer treats it as
            // "whole-file syntax tree".
            None => json!({ "textDocument": { "uri": uri } }),
        };

        lookup_to_null(
            self.send_request("rust-analyzer/syntaxTree", Some(params))
                .await,
        )
    }

    pub async fn view_hir(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("rust-analyzer/viewHir", Some(params))
                .await,
        )
    }

    pub async fn view_mir(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("rust-analyzer/viewMir", Some(params))
                .await,
        )
    }

    pub async fn prepare_call_hierarchy(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("textDocument/prepareCallHierarchy", Some(params))
                .await,
        )
    }

    pub async fn call_hierarchy_incoming(&self, item: &Value) -> Result<Value> {
        let params = json!({ "item": item });

        lookup_to_null(
            self.send_request("callHierarchy/incomingCalls", Some(params))
                .await,
        )
    }

    pub async fn call_hierarchy_outgoing(&self, item: &Value) -> Result<Value> {
        let params = json!({ "item": item });

        lookup_to_null(
            self.send_request("callHierarchy/outgoingCalls", Some(params))
                .await,
        )
    }

    pub async fn prepare_type_hierarchy(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("textDocument/prepareTypeHierarchy", Some(params))
                .await,
        )
    }

    pub async fn type_hierarchy_supertypes(&self, item: &Value) -> Result<Value> {
        let params = json!({ "item": item });

        lookup_to_null(
            self.send_request("typeHierarchy/supertypes", Some(params))
                .await,
        )
    }

    pub async fn type_hierarchy_subtypes(&self, item: &Value) -> Result<Value> {
        let params = json!({ "item": item });

        lookup_to_null(
            self.send_request("typeHierarchy/subtypes", Some(params))
                .await,
        )
    }

    pub async fn open_docs(&self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        lookup_to_null(
            self.send_request("experimental/externalDocs", Some(params))
                .await,
        )
    }

    pub async fn code_actions(
        &self,
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

        strict(
            self.send_request("textDocument/codeAction", Some(params))
                .await,
        )
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

            let diag_start_line = start
                .get("line")
                .and_then(|l| l.as_u64())
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0);
            let diag_end_line = end
                .get("line")
                .and_then(|l| l.as_u64())
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0);

            // Check if diagnostic overlaps with requested range.
            diag_start_line <= end_line && diag_end_line >= start_line
        })
        .cloned()
        .collect();

    json!(filtered)
}
