use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};

use crate::{
    diagnostics::format_diagnostics,
    lsp::RustAnalyzerClient,
    protocol::mcp::{ContentItem, ToolResult},
};

use super::server::RustAnalyzerMCPServer;

/// Helper struct for extracting common tool parameters.
struct ToolParams;

impl ToolParams {
    fn extract_file_path(args: &Value) -> Result<String> {
        let Some(file_path) = args["file_path"].as_str() else {
            return Err(anyhow!("Missing file_path"));
        };
        Ok(file_path.to_string())
    }

    fn extract_position(args: &Value) -> Result<(u32, u32)> {
        let Some(line) = args["line"].as_u64() else {
            return Err(anyhow!("Missing line"));
        };
        let Some(character) = args["character"].as_u64() else {
            return Err(anyhow!("Missing character"));
        };
        Ok((line as u32, character as u32))
    }

    fn extract_range(args: &Value) -> Result<(u32, u32, u32, u32)> {
        let (line, character) = Self::extract_position(args)?;
        let Some(end_line) = args["end_line"].as_u64() else {
            return Err(anyhow!("Missing end_line"));
        };
        let Some(end_character) = args["end_character"].as_u64() else {
            return Err(anyhow!("Missing end_character"));
        };
        Ok((line, character, end_line as u32, end_character as u32))
    }
}

fn wrap(value: Value) -> Result<ToolResult> {
    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&value)?,
        }],
    })
}

/// Common path: extract file_path, ensure the document is open, then run the LSP call.
async fn with_doc<F, Fut>(
    server: &Arc<RustAnalyzerMCPServer>,
    args: &Value,
    f: F,
) -> Result<ToolResult>
where
    F: FnOnce(Arc<RustAnalyzerClient>, String) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let file_path = ToolParams::extract_file_path(args)?;
    let uri = server.open_document_if_needed(&file_path).await?;
    let client = server.current_client().await?;
    let value = f(client, uri).await?;
    wrap(value)
}

pub async fn handle_tool_call(
    server: &Arc<RustAnalyzerMCPServer>,
    tool_name: &str,
    args: Value,
) -> Result<ToolResult> {
    server.ensure_client_started().await?;

    match tool_name {
        "rust_analyzer_hover" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.hover(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_definition" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.definition(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_references" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.references(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_completion" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.completion(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_symbols" => {
            with_doc(server, &args, move |c, uri| async move {
                c.document_symbols(&uri).await
            })
            .await
        }
        "rust_analyzer_format" => {
            with_doc(server, &args, move |c, uri| async move {
                c.formatting(&uri).await
            })
            .await
        }
        "rust_analyzer_code_actions" => {
            let (l1, c1, l2, c2) = ToolParams::extract_range(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.code_actions(&uri, l1, c1, l2, c2).await
            })
            .await
        }
        "rust_analyzer_rename" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            let new_name = args["new_name"]
                .as_str()
                .ok_or_else(|| anyhow!("Missing new_name"))?
                .to_string();
            with_doc(server, &args, move |c, uri| async move {
                c.rename(&uri, line, ch, &new_name).await
            })
            .await
        }
        "rust_analyzer_prepare_rename" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.prepare_rename(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_signature_help" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.signature_help(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_inlay_hints" => {
            let (l1, c1, l2, c2) = ToolParams::extract_range(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.inlay_hints(&uri, l1, c1, l2, c2).await
            })
            .await
        }
        "rust_analyzer_workspace_symbol" => handle_workspace_symbol(server, args).await,
        "rust_analyzer_set_workspace" => handle_set_workspace(server, args).await,
        "rust_analyzer_diagnostics" => handle_diagnostics(server, args).await,
        "rust_analyzer_workspace_diagnostics" => handle_workspace_diagnostics(server, args).await,
        _ => Err(anyhow!("Unknown tool: {}", tool_name)),
    }
}

async fn handle_workspace_symbol(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let query = args["query"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing query"))?;
    let client = server.current_client().await?;
    let value = client.workspace_symbol(query).await?;
    wrap(value)
}

async fn handle_set_workspace(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let Some(workspace_path) = args["workspace_path"].as_str() else {
        return Err(anyhow!("Missing workspace_path"));
    };

    server
        .set_workspace_root(std::path::PathBuf::from(workspace_path))
        .await?;

    // Start the new client automatically.
    server.ensure_client_started().await?;

    let new_root = server.workspace_root_clone().await;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: format!("Workspace set to: {}", new_root.display()),
        }],
    })
}

async fn handle_diagnostics(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let uri = server.open_document_if_needed(&file_path).await?;
    let client = server.current_client().await?;

    // Poll briefly for diagnostics — rust-analyzer needs time to run cargo check after didSave.
    // Stop early as soon as we see any diagnostics; otherwise return whatever's available
    // (possibly empty) once the timeout elapses. Transient errors (request cancelled by the
    // server while indexing, etc.) are swallowed within the loop so the next poll can retry.
    let mut result = json!([]);
    let start = std::time::Instant::now();
    let timeout = tokio::time::Duration::from_secs(8);
    let poll_interval = tokio::time::Duration::from_millis(500);
    while start.elapsed() < timeout {
        match client.diagnostics(&uri).await {
            Ok(v) => result = v,
            Err(e) => {
                tracing::debug!("Transient diagnostics error, retrying: {}", e);
            }
        }
        if result.as_array().is_some_and(|a| !a.is_empty()) {
            break;
        }
        tokio::time::sleep(poll_interval).await;
    }

    let diagnostics = format_diagnostics(&file_path, &result);
    wrap(diagnostics)
}

async fn handle_workspace_diagnostics(
    server: &Arc<RustAnalyzerMCPServer>,
    _args: Value,
) -> Result<ToolResult> {
    let client = server.current_client().await?;
    let result = client.workspace_diagnostics().await?;
    let workspace_root = server.workspace_root_clone().await;
    let formatted = format_workspace_diagnostics(&workspace_root, &result);
    wrap(formatted)
}

fn format_workspace_diagnostics(workspace_root: &Path, result: &Value) -> Value {
    if !result.is_object() {
        // Handle unexpected format.
        if let Some(items) = result.get("items") {
            return json!({
                "workspace": workspace_root.display().to_string(),
                "diagnostics": items,
                "summary": {
                    "total_diagnostics": items.as_array().map(|a| a.len()).unwrap_or(0),
                    "by_severity": {}
                }
            });
        }

        return json!({
            "workspace": workspace_root.display().to_string(),
            "diagnostics": result,
            "summary": {
                "note": "Unexpected response format from rust-analyzer"
            }
        });
    }

    let mut output = json!({
        "workspace": workspace_root.display().to_string(),
        "files": {},
        "summary": {
            "total_files": 0,
            "total_errors": 0,
            "total_warnings": 0,
            "total_information": 0,
            "total_hints": 0
        }
    });

    let mut total_errors = 0;
    let mut total_warnings = 0;
    let mut total_information = 0;
    let mut total_hints = 0;
    let mut file_count = 0;

    let Some(obj) = result.as_object() else {
        return output;
    };

    for (uri, diagnostics) in obj {
        let Some(diag_array) = diagnostics.as_array() else {
            continue;
        };

        if diag_array.is_empty() {
            continue;
        }

        file_count += 1;
        let mut file_errors = 0;
        let mut file_warnings = 0;
        let mut file_information = 0;
        let mut file_hints = 0;

        for diag in diag_array {
            let Some(severity) = diag.get("severity").and_then(|s| s.as_u64()) else {
                continue;
            };

            match severity {
                1 => {
                    file_errors += 1;
                    total_errors += 1;
                }
                2 => {
                    file_warnings += 1;
                    total_warnings += 1;
                }
                3 => {
                    file_information += 1;
                    total_information += 1;
                }
                4 => {
                    file_hints += 1;
                    total_hints += 1;
                }
                _ => {}
            }
        }

        output["files"][uri] = json!({
            "diagnostics": diagnostics,
            "summary": {
                "errors": file_errors,
                "warnings": file_warnings,
                "information": file_information,
                "hints": file_hints
            }
        });
    }

    output["summary"]["total_files"] = json!(file_count);
    output["summary"]["total_errors"] = json!(total_errors);
    output["summary"]["total_warnings"] = json!(total_warnings);
    output["summary"]["total_information"] = json!(total_information);
    output["summary"]["total_hints"] = json!(total_hints);

    output
}
