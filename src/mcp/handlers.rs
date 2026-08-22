use anyhow::{anyhow, Result};
use log::debug;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    config::WORKSPACE_LOAD_TIMEOUT_SECS,
    diagnostics::format_diagnostics,
    position,
    protocol::mcp::{ContentItem, ToolResult},
    uri,
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

pub async fn handle_tool_call(
    server: &mut RustAnalyzerMCPServer,
    tool_name: &str,
    args: Value,
) -> Result<ToolResult> {
    server.ensure_client_started().await?;

    match tool_name {
        "rust_analyzer_hover" => handle_hover(server, args).await,
        "rust_analyzer_definition" => handle_definition(server, args).await,
        "rust_analyzer_references" => handle_references(server, args).await,
        "rust_analyzer_completion" => handle_completion(server, args).await,
        "rust_analyzer_symbols" => handle_symbols(server, args).await,
        "rust_analyzer_format" => handle_format(server, args).await,
        "rust_analyzer_code_actions" => handle_code_actions(server, args).await,
        "rust_analyzer_rename" => handle_rename(server, args).await,
        "rust_analyzer_set_workspace" => handle_set_workspace(server, args).await,
        "rust_analyzer_diagnostics" => handle_diagnostics(server, args).await,
        "rust_analyzer_workspace_diagnostics" => handle_workspace_diagnostics(server, args).await,
        _ => Err(anyhow!("Unknown tool: {}", tool_name)),
    }
}

async fn handle_hover(server: &mut RustAnalyzerMCPServer, args: Value) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.hover(&uri, line, character).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_definition(server: &mut RustAnalyzerMCPServer, args: Value) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.definition(&uri, line, character).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_references(server: &mut RustAnalyzerMCPServer, args: Value) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.references(&uri, line, character).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_completion(server: &mut RustAnalyzerMCPServer, args: Value) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.completion(&uri, line, character).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_symbols(server: &mut RustAnalyzerMCPServer, args: Value) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;

    debug!("Getting symbols for file: {}", file_path);
    let uri = server.open_document_if_needed(&file_path).await?;
    debug!("Document opened with URI: {}", uri);

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.document_symbols(&uri).await?;
    debug!("Document symbols result: {:?}", result);

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_format(server: &mut RustAnalyzerMCPServer, args: Value) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.formatting(&uri).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_code_actions(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character, end_line, end_character) = ToolParams::extract_range(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client
        .code_actions(&uri, line, character, end_line, end_character)
        .await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_rename(server: &mut RustAnalyzerMCPServer, args: Value) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;
    let Some(new_name) = args["new_name"].as_str() else {
        return Err(anyhow!("Missing new_name"));
    };

    let uri = server.open_document_if_needed(&file_path).await?;
    // A rename is worked out across every file rust-analyzer holds, so every one of them has to
    // be the file that is actually there.
    server.refresh_open_documents().await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    // A rename is worked out from everything rust-analyzer has loaded, so one worked out while
    // it is still loading can miss the references it has not reached yet -- and half a rename
    // applied leaves the code worse than it was found. There is no reporting that with the
    // edits, either: whoever asked would have every reason to take them for the whole rename.
    if !client
        .wait_until_loaded(Duration::from_secs(WORKSPACE_LOAD_TIMEOUT_SECS))
        .await
    {
        return Err(anyhow!(
            "rust-analyzer is still loading the workspace after {}s. A rename worked out now \
             could miss references, so it is not worth having; ask again once it has settled.",
            WORKSPACE_LOAD_TIMEOUT_SECS
        ));
    }

    // What is about to be renamed is asked for first: its answer names the symbol, and it
    // explains a position with nothing to rename at it better than the rename itself would.
    let renaming = client.prepare_rename(&uri, line, character).await?;
    let edit = client.rename(&uri, line, character, new_name).await?;
    if edit.is_null() {
        return Err(anyhow!(
            "Nothing to rename at {}:{}:{}",
            file_path,
            line,
            character
        ));
    }

    let old_name = renamed_symbol(server, &file_path, &renaming).await;
    let result = describe_rename(&edit, old_name, new_name).await;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

/// The text `prepareRename` pointed at, which is the name being replaced.
async fn renamed_symbol(
    server: &RustAnalyzerMCPServer,
    file_path: &str,
    renaming: &Value,
) -> Option<String> {
    // rust-analyzer answers with a bare range; the specification also allows one wrapped
    // alongside a placeholder.
    let range = renaming.get("range").unwrap_or(renaming);
    let content = tokio::fs::read_to_string(server.resolve_path(file_path))
        .await
        .ok()?;

    Some(text_of(&content, range)?.to_string())
}

/// An account of a workspace edit that can be applied without working any of it out again.
async fn describe_rename(edit: &Value, old_name: Option<String>, new_name: &str) -> Value {
    let mut changes = Vec::new();
    let mut file_operations = Vec::new();
    let mut edit_count = 0;

    for change in document_changes(edit) {
        match change {
            Change::Text { uri, edits } => {
                let path = uri::uri_to_path(&uri);
                let content = match &path {
                    Some(path) => tokio::fs::read_to_string(path).await.ok(),
                    None => None,
                };

                let mut edits: Vec<Value> = edits
                    .iter()
                    .map(|edit| describe_edit(edit, content.as_deref()))
                    .collect();
                // Descending, so that applying them one after another needs no arithmetic: every
                // edit's range still means what it says once the ones after it have been made.
                edits.sort_by_key(|edit| {
                    std::cmp::Reverse((
                        edit["line"].as_u64().unwrap_or(0),
                        edit["character"].as_u64().unwrap_or(0),
                    ))
                });

                edit_count += edits.len();
                changes.push(json!({
                    "file": display_path(&path, &uri),
                    "edits": edits,
                }));
            }
            Change::Resource(operation) => file_operations.push(operation),
        }
    }

    json!({
        "applied": false,
        "old_name": old_name,
        "new_name": new_name,
        "position_encoding": "utf-16",
        "summary": {
            "files_changed": changes.len(),
            "edits": edit_count,
            "file_operations": file_operations.len(),
        },
        "changes": changes,
        "file_operations": file_operations,
        // Everything above is this, worked out. Whoever would rather work it out themselves can.
        "workspace_edit": edit,
    })
}

/// One entry of a workspace edit.
enum Change {
    /// Edits to one file.
    Text { uri: String, edits: Vec<Value> },
    /// Something done to a file itself, such as the rename of a module's file.
    Resource(Value),
}

/// The changes a workspace edit is made of, however it spells them.
fn document_changes(edit: &Value) -> Vec<Change> {
    // `documentChanges` is what a client that understands file operations gets, and the only
    // form that can carry them; `changes` is the older shape, kept for the rust-analyzer that
    // answers with it.
    if let Some(document_changes) = edit.get("documentChanges").and_then(|it| it.as_array()) {
        return document_changes
            .iter()
            .map(
                |change| match change.get("edits").and_then(|it| it.as_array()) {
                    Some(edits) => Change::Text {
                        uri: change["textDocument"]["uri"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        edits: edits.clone(),
                    },
                    None => Change::Resource(change.clone()),
                },
            )
            .collect();
    }

    edit.get("changes")
        .and_then(|it| it.as_object())
        .map(|changes| {
            changes
                .iter()
                .map(|(uri, edits)| Change::Text {
                    uri: uri.clone(),
                    edits: edits.as_array().cloned().unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One text edit, with the offsets and the text it replaces spelled out.
fn describe_edit(edit: &Value, content: Option<&str>) -> Value {
    let range = edit.get("range").cloned().unwrap_or(json!(null));
    let mut described = json!({
        "line": range["start"]["line"],
        "character": range["start"]["character"],
        "end_line": range["end"]["line"],
        "end_character": range["end"]["character"],
        "new_text": edit.get("newText").cloned().unwrap_or(json!("")),
    });

    // Byte offsets and the text being replaced, so that an edit can be applied to the file as
    // bytes and checked before it is: the columns above are UTF-16 code units, which are neither.
    if let Some(content) = content {
        if let Some((start, end)) = byte_range(content, &range) {
            described["byte_range"] = json!([start, end]);
            described["old_text"] = json!(&content[start..end]);
        }
    }

    described
}

/// What `range` covers in `content`.
fn text_of<'a>(content: &'a str, range: &Value) -> Option<&'a str> {
    let (start, end) = byte_range(content, range)?;

    Some(&content[start..end])
}

/// The byte offsets `range` covers in `content`.
fn byte_range(content: &str, range: &Value) -> Option<(usize, usize)> {
    let at = |end: &str, of: &str| -> Option<u32> {
        range.get(end)?.get(of)?.as_u64().map(|it| it as u32)
    };

    let start = position::byte_offset(content, at("start", "line")?, at("start", "character")?);
    let end = position::byte_offset(content, at("end", "line")?, at("end", "character")?);

    (start <= end).then_some((start, end))
}

/// The path a URI names, or the URI itself when it names none.
fn display_path(path: &Option<PathBuf>, uri: &str) -> String {
    path.as_ref()
        .map_or_else(|| uri.to_string(), |path| path.display().to_string())
}

async fn handle_set_workspace(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let Some(workspace_path) = args["workspace_path"].as_str() else {
        return Err(anyhow!("Missing workspace_path"));
    };

    // Shutdown existing client.
    if let Some(client) = &mut server.client {
        client.shutdown().await?;
    }
    server.client = None;

    // Set new workspace with proper absolute path handling, taking a `file:` URI as readily as
    // a path.
    let workspace_root = uri::uri_to_path(workspace_path).unwrap_or_else(|| workspace_path.into());
    server.workspace_root = uri::absolute(&workspace_root);

    // Start the new client automatically.
    server.ensure_client_started().await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: format!("Workspace set to: {}", server.workspace_root.display()),
        }],
    })
}

async fn handle_diagnostics(server: &mut RustAnalyzerMCPServer, args: Value) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let fresh = client.fresh_diagnostics(&uri).await?;
    let mut diagnostics = format_diagnostics(&file_path, &fresh.items);
    if !fresh.complete {
        // Saying so beats either waiting longer or passing off what rust-analyzer had got to so
        // far as the state of the code -- least of all when that is an empty list.
        diagnostics["note"] = json!(
            "rust-analyzer had not finished loading the workspace or checking it when this was \
             reported, so these diagnostics may be incomplete. Ask again for the rest."
        );
    }

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&diagnostics)?,
        }],
    })
}

async fn handle_workspace_diagnostics(
    server: &mut RustAnalyzerMCPServer,
    _args: Value,
) -> Result<ToolResult> {
    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.workspace_diagnostics().await?;

    // Format workspace diagnostics.
    let formatted = format_workspace_diagnostics(&server.workspace_root, &result);

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&formatted)?,
        }],
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

    // Fallback format (diagnostics per URI).
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
