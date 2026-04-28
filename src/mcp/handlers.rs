use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};

use crate::{
    diagnostics::format_diagnostics,
    lsp::RustAnalyzerClient,
    protocol::mcp::{ContentItem, ToolResult},
};

use super::{
    server::RustAnalyzerMCPServer,
    truncate::{
        paginate_workspace_diagnostics, paginate_workspace_symbol, parse_cursor, resolve_limit,
        truncate_completion, truncate_hover, COMPLETION_DEFAULT_LIMIT, HOVER_MAX_BYTES,
        WORKSPACE_DIAGNOSTICS_DEFAULT_LIMIT, WORKSPACE_SYMBOL_DEFAULT_LIMIT,
    },
    workspace::WorkspaceEntry,
};

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

    fn extract_verbose(args: &Value) -> bool {
        args.get("verbose")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    fn extract_limit(args: &Value) -> Option<usize> {
        args.get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
    }

    fn extract_cursor(args: &Value) -> Option<&str> {
        args.get("cursor").and_then(|v| v.as_str())
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

/// Common path: resolve workspace from args, extract file_path, ensure the
/// document is open, then run the LSP call.
async fn with_doc<F, Fut>(
    server: &Arc<RustAnalyzerMCPServer>,
    args: &Value,
    f: F,
) -> Result<ToolResult>
where
    F: FnOnce(Arc<RustAnalyzerClient>, String) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let ws = server.resolve_workspace(args).await?;
    let file_path = ToolParams::extract_file_path(args)?;
    let uri = ws.open_document_if_needed(&file_path).await?;
    let client = ws.current_client().await?;
    let value = f(client, uri).await?;
    wrap(value)
}

pub async fn handle_tool_call(
    server: &Arc<RustAnalyzerMCPServer>,
    tool_name: &str,
    args: Value,
) -> Result<ToolResult> {
    match tool_name {
        "rust_analyzer_hover" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            let verbose = ToolParams::extract_verbose(&args);
            let file_path = ToolParams::extract_file_path(&args)?;
            let ws = server.resolve_workspace(&args).await?;
            let uri = ws.open_document_if_needed(&file_path).await?;
            let client = ws.current_client().await?;
            let value = client.hover(&uri, line, ch).await?;
            wrap(truncate_hover(value, HOVER_MAX_BYTES, verbose))
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
            let verbose = ToolParams::extract_verbose(&args);
            let limit = resolve_limit(
                ToolParams::extract_limit(&args),
                verbose,
                COMPLETION_DEFAULT_LIMIT,
            );
            let file_path = ToolParams::extract_file_path(&args)?;
            let ws = server.resolve_workspace(&args).await?;
            let uri = ws.open_document_if_needed(&file_path).await?;
            let client = ws.current_client().await?;
            let value = client.completion(&uri, line, ch).await?;
            wrap(truncate_completion(value, limit, verbose))
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
        "rust_analyzer_type_definition" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.type_definition(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_implementation" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.implementation(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_expand_macro" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.expand_macro(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_parent_module" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.parent_module(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_runnables" => {
            let position = ToolParams::extract_position(&args).ok();
            with_doc(server, &args, move |c, uri| async move {
                c.runnables(&uri, position).await
            })
            .await
        }
        "rust_analyzer_related_tests" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.related_tests(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_open_docs" => {
            let (line, ch) = ToolParams::extract_position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.open_docs(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_add_workspace" => handle_add_workspace(server, args).await,
        "rust_analyzer_remove_workspace" => handle_remove_workspace(server, args).await,
        "rust_analyzer_list_workspaces" => handle_list_workspaces(server, args).await,
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
    let verbose = ToolParams::extract_verbose(&args);
    let cursor = parse_cursor(ToolParams::extract_cursor(&args));
    let limit = resolve_limit(
        ToolParams::extract_limit(&args),
        verbose,
        WORKSPACE_SYMBOL_DEFAULT_LIMIT,
    );
    let ws = server.resolve_workspace(&args).await?;
    let client = ws.ensure_client_started().await?;
    let value = client.workspace_symbol(query).await?;
    wrap(paginate_workspace_symbol(value, cursor, limit, verbose))
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

    // Start the new default client automatically.
    let ws = server.default_workspace().await?;
    ws.ensure_client_started().await?;

    let new_root = ws.root_clone();

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
    let ws = server.resolve_workspace(&args).await?;
    let uri = ws.open_document_if_needed(&file_path).await?;
    let client = ws.current_client().await?;

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
    args: Value,
) -> Result<ToolResult> {
    let verbose = ToolParams::extract_verbose(&args);
    let cursor = parse_cursor(ToolParams::extract_cursor(&args));
    let limit = resolve_limit(
        ToolParams::extract_limit(&args),
        verbose,
        WORKSPACE_DIAGNOSTICS_DEFAULT_LIMIT,
    );
    let ws = server.resolve_workspace(&args).await?;
    let client = ws.ensure_client_started().await?;
    let result = client.workspace_diagnostics().await?;
    let workspace_root = ws.root_clone();
    let formatted = format_workspace_diagnostics(&workspace_root, &result);
    wrap(paginate_workspace_diagnostics(
        formatted, cursor, limit, verbose,
    ))
}

async fn handle_add_workspace(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let Some(path) = args["path"].as_str() else {
        return Err(anyhow!("Missing path"));
    };

    let ws = server.add_workspace(std::path::PathBuf::from(path)).await;
    // Start the client eagerly so the next tool call doesn't pay the boot cost.
    ws.ensure_client_started().await?;

    wrap(json!({
        "workspace_id": ws.id(),
        "root": ws.root_clone().display().to_string(),
    }))
}

async fn handle_remove_workspace(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let Some(id) = args["workspace_id"].as_str() else {
        return Err(anyhow!("Missing workspace_id"));
    };
    let removed = server.remove_workspace(id).await;
    if !removed {
        return Err(anyhow!("Unknown workspace_id: {}", id));
    }
    wrap(json!({ "removed": id }))
}

async fn handle_list_workspaces(
    server: &Arc<RustAnalyzerMCPServer>,
    _args: Value,
) -> Result<ToolResult> {
    let entries = server.list_workspaces().await;
    let default_id = entries.first().map(|e| e.id().to_string());
    let list: Vec<Value> = entries
        .iter()
        .map(|e| describe_workspace(e, default_id.as_deref()))
        .collect();
    wrap(json!({ "workspaces": list }))
}

fn describe_workspace(ws: &Arc<WorkspaceEntry>, default_id: Option<&str>) -> Value {
    json!({
        "workspace_id": ws.id(),
        "root": ws.root_clone().display().to_string(),
        "default": default_id == Some(ws.id()),
    })
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
