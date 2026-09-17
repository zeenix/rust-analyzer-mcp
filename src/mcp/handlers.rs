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
    locations,
    lsp::{RustAnalyzerClient, WorkspaceDiagnostics},
    position,
    protocol::mcp::{ContentItem, ToolResult},
    uri,
};

use super::server::{manifest_directory, RustAnalyzerMCPServer};

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

/// Refuses to answer from an index rust-analyzer has not finished building.
///
/// Everything rust-analyzer works out from the whole workspace -- what a symbol is, where it is
/// defined, what refers to it -- it answers with `null` or `[]` until it has loaded that
/// workspace. Those are the same answers it gives for a symbol nothing refers to and for a
/// position that is not on a symbol at all, so a caller cannot tell an index that is not ready
/// from code that is genuinely unused, and the wrong one of those is the one people act on.
///
/// Waiting makes the common case right, and saying so when the wait runs out makes the rest
/// loud rather than silent.
async fn ensure_index_ready(client: &RustAnalyzerClient) -> Result<()> {
    if client
        .wait_until_loaded(Duration::from_secs(WORKSPACE_LOAD_TIMEOUT_SECS))
        .await
    {
        return Ok(());
    }

    Err(anyhow!(
        "rust-analyzer is still loading the workspace after {}s. An answer worked out now would \
         be from a partial index, and an empty one could not be told from a symbol that is \
         really unused; ask again once it has settled.",
        WORKSPACE_LOAD_TIMEOUT_SECS
    ))
}

/// Explains an empty answer that the loaded workspace accounts for, and leaves every other
/// answer alone.
///
/// rust-analyzer only knows the workspace it was pointed at. Asked about a file outside it, it
/// does not say so -- it answers `null`, or an empty list, which is also what it answers for a
/// symbol nothing refers to and for a position that is not on a symbol. That is the same silent
/// wrong answer the readiness gate exists to prevent, arriving by a different route: the gate is
/// satisfied, because a workspace with no Rust in it finishes loading immediately.
///
/// A file outside the root is not wrong in itself -- a definition can lead into a dependency's
/// sources, and asking about one there works -- so this only speaks up when the answer was
/// empty anyway.
///
/// The root itself can also be the problem. `set_workspace` refuses a directory with no
/// `Cargo.toml`, but the workspace the server *starts* in is its working directory, which nothing
/// chooses and nothing checks -- so a session opened outside a Rust project begins in exactly the
/// state `set_workspace` refuses to move into, and a file inside that root passes the check below
/// while rust-analyzer has loaded nothing at all.
fn explain_empty_answer(
    server: &RustAnalyzerMCPServer,
    result: &Value,
    file_path: &str,
) -> Result<()> {
    let empty = result.is_null() || result.as_array().is_some_and(|items| items.is_empty());
    if !empty {
        return Ok(());
    }

    if !server.workspace_root.join("Cargo.toml").is_file() {
        return Err(anyhow!(
            "rust-analyzer had nothing to say about {}, and the workspace it has loaded ({}) is \
             not a Rust workspace: there is no Cargo.toml in it, so it has loaded nothing and \
             every question about every file is answered this way. That is the directory the \
             server was started in -- it is not chosen, it is wherever the session began. Point \
             it at the project this file belongs to with rust_analyzer_set_workspace, then ask \
             again.",
            file_path,
            server.workspace_root.display()
        ));
    }

    if server
        .resolve_path(file_path)
        .starts_with(&server.workspace_root)
    {
        return Ok(());
    }

    Err(anyhow!(
        "rust-analyzer had nothing to say about {}, which is outside the workspace it has \
         loaded ({}). Everything it knows comes from that workspace, so a file outside it reads \
         as empty rather than as unknown. Point it at the project this file belongs to with \
         rust_analyzer_set_workspace, then ask again.",
        file_path,
        server.workspace_root.display()
    ))
}

pub async fn handle_tool_call(
    server: &mut RustAnalyzerMCPServer,
    tool_name: &str,
    args: Value,
) -> Result<ToolResult> {
    // A call may name the workspace it is about, and is then answered by that workspace's own
    // rust-analyzer rather than by whichever one this server was last pointed at. Naming it is
    // the only way a caller sharing this server with others can be sure which index answered --
    // and it names it for this call only, so one caller doing so does not move the workspace out
    // from under the next. `set_workspace` names its argument for the opposite reason, to move
    // the default, so it handles its own.
    let default_workspace = server.workspace_root.clone();
    let named_a_workspace = tool_name != "rust_analyzer_set_workspace"
        && args.get("workspace_path").is_some_and(Value::is_string);
    if named_a_workspace {
        server.select_workspace(args["workspace_path"].as_str())?;
    }

    let answer = dispatch(server, tool_name, args).await;

    if named_a_workspace {
        server.workspace_root = default_workspace;
    }
    answer
}

async fn dispatch(
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
        "rust_analyzer_workspace_symbols" => handle_workspace_symbols(server, args).await,
        "rust_analyzer_type_definition" => handle_type_definition(server, args).await,
        "rust_analyzer_implementation" => handle_implementation(server, args).await,
        "rust_analyzer_expand_macro" => handle_expand_macro(server, args).await,
        "rust_analyzer_related_tests" => handle_related_tests(server, args).await,
        "rust_analyzer_runnables" => handle_runnables(server, args).await,
        "rust_analyzer_incoming_calls" => handle_calls(server, args, Calls::Incoming).await,
        "rust_analyzer_outgoing_calls" => handle_calls(server, args, Calls::Outgoing).await,
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

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    let result = client.hover(&uri, line, character).await?;
    explain_empty_answer(server, &result, &file_path)?;

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

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    let result = client.definition(&uri, line, character).await?;
    explain_empty_answer(server, &result, &file_path)?;

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

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    let result = client.references(&uri, line, character).await?;
    // LSP counts the declaration among the references, so the list is one longer than the uses of
    // the symbol. Asking where the declaration is costs one request against an index that has just
    // answered a harder question, and is what lets the answer say which hit it is.
    let definition = client.definition(&uri, line, character).await?;
    explain_empty_answer(server, &result, &file_path)?;
    let result = locations::annotate(&result, &server.workspace_root, &definition);

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

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    let result = client.completion(&uri, line, character).await?;
    explain_empty_answer(server, &result, &file_path)?;

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

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    // Deliberately not waiting for the workspace to load, unlike every other read here: the
    // symbols of one file are worked out from that file alone, so this answers correctly while
    // the rest of rust-analyzer is still catching up -- and it is the one thing left to ask when
    // it is.
    let mut result = client.document_symbols(&uri).await?;

    // Except immediately after the file is opened, when rust-analyzer has yet to parse it and
    // answers `null` -- an answer that reads as a file with nothing in it. Rare when the machine
    // is idle and common when several rust-analyzers are competing for it, which is the sort of
    // difference that turns into a test that fails only in CI. Waiting for the load it did not
    // need is the cheapest way to be sure the second answer means something.
    if result.is_null() {
        debug!("No symbols for {} yet; waiting for rust-analyzer", uri);
        ensure_index_ready(client).await?;
        result = client.document_symbols(&uri).await?;
    }

    debug!("Document symbols result: {:?}", result);
    explain_empty_answer(server, &result, &file_path)?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_type_definition(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    let result = client.type_definition(&uri, line, character).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_implementation(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    let result = client.implementation(&uri, line, character).await?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_expand_macro(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    let result = client.expand_macro(&uri, line, character).await?;
    if result.is_null() {
        return Err(anyhow!(
            "Nothing to expand at {}:{}:{}. The position has to be on a macro call; a macro \
             whose expansion rust-analyzer cannot work out answers the same way.",
            file_path,
            line,
            character
        ));
    }

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_related_tests(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    let result = client.related_tests(&uri, line, character).await?;
    explain_empty_answer(server, &result, &file_path)?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

async fn handle_runnables(server: &mut RustAnalyzerMCPServer, args: Value) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    // A position is optional here, unlike everywhere else: without one the answer covers the
    // whole file, which is what "how do I run this" usually means.
    let position = match (args["line"].as_u64(), args["character"].as_u64()) {
        (Some(line), Some(character)) => Some((line as u32, character as u32)),
        _ => None,
    };

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    let result = client.runnables(&uri, position).await?;
    explain_empty_answer(server, &result, &file_path)?;

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&result)?,
        }],
    })
}

/// Which way round a call hierarchy is being asked about.
#[derive(Clone, Copy)]
enum Calls {
    /// What calls the function at the position.
    Incoming,
    /// What the function at the position calls.
    Outgoing,
}

async fn handle_calls(
    server: &mut RustAnalyzerMCPServer,
    args: Value,
    direction: Calls,
) -> Result<ToolResult> {
    let file_path = ToolParams::extract_file_path(&args)?;
    let (line, character) = ToolParams::extract_position(&args)?;

    let uri = server.open_document_if_needed(&file_path).await?;

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

    // A call hierarchy is about an item rather than a position, and the item has to be asked for
    // first. Doing it here rather than exposing it as a tool of its own keeps the round trip in
    // one place: the item is of no use to anyone except as the argument to these two calls.
    let prepared = client.prepare_call_hierarchy(&uri, line, character).await?;
    let Some(item) = prepared.as_array().and_then(|items| items.first()) else {
        return Err(anyhow!(
            "Nothing callable at {}:{}:{}. A call hierarchy starts at a function, a method or \
             something else that can be called; check the position is on the name of one.",
            file_path,
            line,
            character
        ));
    };
    let item = item.clone();

    let result = match direction {
        Calls::Incoming => client.incoming_calls(&item).await?,
        Calls::Outgoing => client.outgoing_calls(&item).await?,
    };

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&json!({
                "item": item,
                "calls": result,
            }))?,
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

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    // Every symbol in the workspace is the answer to this, so an index that has only reached
    // part of the workspace answers about part of it -- and short of a name is exactly how a
    // complete answer looks too.
    ensure_index_ready(client).await?;

    let result = client.workspace_symbols(query).await?;

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

    let Some(client) = server.client() else {
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

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    ensure_index_ready(client).await?;

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

    let Some(client) = server.client() else {
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

    // Work out the new root before anything is torn down, taking a `file:` URI as readily as a
    // path, and refusing a directory with no manifest rather than loading nothing from it.
    let named = uri::uri_to_path(workspace_path).unwrap_or_else(|| workspace_path.into());
    let workspace_root = manifest_directory(&uri::absolute(&named)).map_err(|e| {
        anyhow!(
            "{e} The workspace is left as it was ({}).",
            server.workspace_root.display()
        )
    })?;

    server.workspace_root = workspace_root;

    // Moving the default says the other workspaces are finished with -- unlike naming one per
    // call, which says it will be named again -- so their rust-analyzers go rather than sit on a
    // gigabyte of index nobody is asking about.
    server.drop_other_workspaces().await;

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

    let Some(client) = server.client() else {
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
    // Every other tool starts rust-analyzer by opening the file it was asked about. This one is
    // asked about no file, so it has to start it itself -- and the workspace it starts in may be
    // one this call named, which nothing has opened a file in.
    server.ensure_client_started().await?;

    let Some(client) = server.client() else {
        return Err(anyhow!("Client not initialized"));
    };

    let reported = client.workspace_diagnostics().await?;
    let formatted = format_workspace_diagnostics(&server.workspace_root, &reported);

    Ok(ToolResult {
        content: vec![ContentItem {
            content_type: "text".to_string(),
            text: serde_json::to_string_pretty(&formatted)?,
        }],
    })
}

/// The whole-workspace report: one entry per faulted file, counted the way the per-file tool
/// counts, and honest about whether anything was analysed at all.
fn format_workspace_diagnostics(workspace_root: &Path, reported: &WorkspaceDiagnostics) -> Value {
    let mut files = serde_json::Map::new();
    let (mut errors, mut warnings, mut information, mut hints) = (0, 0, 0, 0);

    // Sorted, because these come from a map and an answer that reorders itself between two
    // identical calls is one nobody can diff.
    let mut faulted: Vec<_> = reported.files.iter().collect();
    faulted.sort_by_key(|(uri, _)| *uri);

    for (uri, diagnostics) in faulted {
        let shown = uri::uri_to_path(uri)
            .map(|path| {
                path.strip_prefix(workspace_root)
                    .unwrap_or(&path)
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|| uri.to_string());

        // Counted by the same code as the single-file tool, so one fault is not one number here
        // and two there: rust-analyzer and rustc each report it, and only one of them is it.
        let formatted = format_diagnostics(&shown, diagnostics);
        errors += formatted["summary"]["errors"].as_u64().unwrap_or(0);
        warnings += formatted["summary"]["warnings"].as_u64().unwrap_or(0);
        information += formatted["summary"]["information"].as_u64().unwrap_or(0);
        hints += formatted["summary"]["hints"].as_u64().unwrap_or(0);

        files.insert(shown, formatted);
    }

    let mut output = json!({
        "workspace": workspace_root.display().to_string(),
        "summary": {
            "total_files": files.len(),
            "total_errors": errors,
            "total_warnings": warnings,
            "total_information": information,
            "total_hints": hints,
        },
        "complete": reported.complete,
        "files": files,
    });

    if !reported.complete {
        // The zero this would otherwise report is the dangerous one: it reads as a clean
        // workspace, and a caller acting on it acts on a check that never ran.
        output["note"] = json!(
            "rust-analyzer had not finished loading the workspace or checking it when this was \
             reported. These counts are a floor rather than the state of the workspace -- an \
             empty report here means nothing was analysed, not that nothing is wrong. Ask again \
             for the rest."
        );
    }

    output
}
