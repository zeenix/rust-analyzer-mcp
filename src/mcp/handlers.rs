use anyhow::{anyhow, Result};
use log::debug;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{
    diagnostics::format_diagnostics,
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
        "rust_analyzer_set_workspace" => handle_set_workspace(server, args).await,
        "rust_analyzer_diagnostics" => handle_diagnostics(server, args).await,
        "rust_analyzer_workspace_diagnostics" => handle_workspace_diagnostics(server, args).await,
        "rust_analyzer_workspace_symbols" => handle_workspace_symbols(server, args).await,
        "rust_analyzer_implementations" => handle_implementations(server, args).await,
        "rust_analyzer_incoming_calls" => handle_incoming_calls(server, args).await,
        "rust_analyzer_outgoing_calls" => handle_outgoing_calls(server, args).await,
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
    let include_hover = args["include_hover"].as_bool().unwrap_or(false);

    debug!("Getting symbols for file: {}", file_path);
    let uri = server.open_document_if_needed(&file_path).await?;
    debug!("Document opened with URI: {}", uri);

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.document_symbols(&uri).await?;
    debug!("Document symbols result: {:?}", result);

    if !include_hover {
        return Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: serde_json::to_string_pretty(&result)?,
            }],
        });
    }

    // Enhance symbols with hover info
    let enhanced = enhance_symbols_with_hover(client, &uri, result).await;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&enhanced)?,
        }],
    })
}

/// Recursively enhance symbols with hover information.
async fn enhance_symbols_with_hover(
    client: &mut crate::lsp::RustAnalyzerClient,
    uri: &str,
    symbols: Value,
) -> Value {
    match symbols {
        Value::Array(arr) => {
            let mut enhanced = Vec::with_capacity(arr.len());
            for sym in arr {
                enhanced.push(Box::pin(enhance_single_symbol(client, uri, sym)).await);
            }
            Value::Array(enhanced)
        }
        other => other,
    }
}

async fn enhance_single_symbol(
    client: &mut crate::lsp::RustAnalyzerClient,
    uri: &str,
    mut symbol: Value,
) -> Value {
    // Get position from selectionRange (DocumentSymbol), range (DocumentSymbol), 
    // or location.range (SymbolInformation)
    let (line, character) = if let Some(sel_range) = symbol.get("selectionRange") {
        let line = sel_range["start"]["line"].as_u64().unwrap_or(0) as u32;
        let char = sel_range["start"]["character"].as_u64().unwrap_or(0) as u32;
        (line, char)
    } else if let Some(range) = symbol.get("range") {
        let line = range["start"]["line"].as_u64().unwrap_or(0) as u32;
        let char = range["start"]["character"].as_u64().unwrap_or(0) as u32;
        (line, char)
    } else if let Some(location) = symbol.get("location") {
        // SymbolInformation format
        let range = &location["range"];
        let line = range["start"]["line"].as_u64().unwrap_or(0) as u32;
        let char = range["start"]["character"].as_u64().unwrap_or(0) as u32;
        (line, char)
    } else {
        return symbol;
    };

    // Fetch hover info - scan forward to find the symbol name
    // The range start is often on keywords like 'pub', 'fn', etc.
    // Try positions at intervals until we get meaningful hover info
    let mut found_hover = false;
    for offset in (0..24).step_by(4) {
        if found_hover {
            break;
        }
        if let Ok(hover) = client.hover(uri, line, character + offset).await {
            if !hover.is_null() {
                // Extract just the markdown content for cleaner output
                if let Some(contents) = hover.get("contents") {
                    if let Some(value) = contents.get("value") {
                        // Skip if it's just keyword documentation (pub, fn, mod, etc.)
                        let text = value.as_str().unwrap_or("");
                        if !text.contains("Make an item visible") 
                            && !text.contains("A function or function pointer")
                            && !text.contains("Organize code into")
                        {
                            symbol["hover"] = value.clone();
                            found_hover = true;
                        }
                    } else {
                        symbol["hover"] = contents.clone();
                        found_hover = true;
                    }
                } else {
                    symbol["hover"] = hover;
                    found_hover = true;
                }
            }
        }
    }

    // Recursively enhance children
    if let Some(children) = symbol.get("children").cloned() {
        symbol["children"] = Box::pin(enhance_symbols_with_hover(client, uri, children)).await;
    }

    symbol
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

    // Set new workspace with proper absolute path handling.
    let workspace_root = PathBuf::from(workspace_path);
    server.workspace_root = workspace_root.canonicalize().unwrap_or_else(|_| {
        if workspace_root.is_absolute() {
            workspace_root.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&workspace_root)
        }
    });

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

    // First try to get diagnostics from notification-based cache.
    // If not available, wait for notifications with timeout.
    let result = if let Some(diags) = client.wait_for_diagnostics(&uri).await {
        json!(diags)
    } else {
        // Fallback to direct query.
        client.diagnostics(&uri).await?
    };

    let diagnostics = format_diagnostics(&file_path, &result);

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

async fn handle_workspace_symbols(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let Some(query) = args["query"].as_str() else {
        return Err(anyhow!("Missing query"));
    };

    server.ensure_client_started().await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.workspace_symbols(query).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_implementations(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    let result = client.implementations(&uri, line, character).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_incoming_calls(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    // First, prepare the call hierarchy item
    let items = client.prepare_call_hierarchy(&uri, line, character).await?;
    
    let Some(items_array) = items.as_array() else {
        return Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: "No call hierarchy item found at this position".to_string(),
            }],
        });
    };

    if items_array.is_empty() {
        return Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: "No call hierarchy item found at this position".to_string(),
            }],
        });
    }

    // Get incoming calls for the first item
    let item = items_array[0].clone();
    let result = client.incoming_calls(item).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_outgoing_calls(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = &mut server.client else {
        return Err(anyhow!("Client not initialized"));
    };

    // First, prepare the call hierarchy item
    let items = client.prepare_call_hierarchy(&uri, line, character).await?;
    
    let Some(items_array) = items.as_array() else {
        return Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: "No call hierarchy item found at this position".to_string(),
            }],
        });
    };

    if items_array.is_empty() {
        return Ok(ToolResult {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: "No call hierarchy item found at this position".to_string(),
            }],
        });
    }

    // Get outgoing calls for the first item
    let item = items_array[0].clone();
    let result = client.outgoing_calls(item).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
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
