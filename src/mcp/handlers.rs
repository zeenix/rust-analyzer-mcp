use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, sync::Arc};

use crate::{
    diagnostics::format_diagnostics,
    lsp::RustAnalyzerClient,
    protocol::mcp::{ContentItem, ToolResult},
};

use super::{
    server::RustAnalyzerMCPServer,
    snippets::{
        enrich_locations, enrich_workspace_diagnostics, EnrichOpts, SNIPPET_DEFAULT_CONTEXT_LINES,
        SNIPPET_DEFAULT_MAX_HITS,
    },
    truncate::{
        paginate_workspace_diagnostics, paginate_workspace_symbol, parse_cursor, resolve_limit,
        truncate_completion, truncate_hover, COMPLETION_DEFAULT_LIMIT, HOVER_MAX_BYTES,
        WORKSPACE_DIAGNOSTICS_DEFAULT_LIMIT, WORKSPACE_SYMBOL_DEFAULT_LIMIT,
    },
    workspace::WorkspaceEntry,
};

/// Tool-argument extractors. Each one validates a single named argument and
/// returns a typed value, with stable error messages the LLM can correct against.
mod params {
    use super::*;

    pub fn file_path(args: &Value) -> Result<String> {
        args["file_path"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Missing file_path"))
    }

    fn coord(args: &Value, key: &str) -> Result<u32> {
        let n = args[key]
            .as_u64()
            .ok_or_else(|| anyhow!("Missing {}", key))?;
        u32::try_from(n).map_err(|_| anyhow!("{} out of u32 range: {}", key, n))
    }

    pub fn position(args: &Value) -> Result<(u32, u32)> {
        Ok((coord(args, "line")?, coord(args, "character")?))
    }

    pub fn range(args: &Value) -> Result<(u32, u32, u32, u32)> {
        let (line, character) = position(args)?;
        Ok((
            line,
            character,
            coord(args, "end_line")?,
            coord(args, "end_character")?,
        ))
    }

    /// Range coords are all-or-nothing: every coord set or none set. Partial
    /// ranges (e.g. only `line`) are a tool-call error so the LLM can't
    /// silently send an under-specified range and get back the whole-file
    /// fallback.
    pub fn optional_range(args: &Value) -> Result<Option<(u32, u32, u32, u32)>> {
        let coords = ["line", "character", "end_line", "end_character"];
        let present: Vec<bool> = coords.iter().map(|k| args.get(*k).is_some()).collect();
        let any = present.iter().any(|p| *p);
        let all = present.iter().all(|p| *p);
        if !any {
            return Ok(None);
        }
        if !all {
            return Err(anyhow!(
                "Range is all-or-nothing: provide line, character, end_line, end_character together — or omit all four for a whole-file query"
            ));
        }
        range(args).map(Some)
    }

    pub fn verbose(args: &Value) -> bool {
        args.get("verbose")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn limit(args: &Value) -> Option<usize> {
        args.get("limit")
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok())
    }

    pub fn cursor(args: &Value) -> Option<&str> {
        args.get("cursor").and_then(|v| v.as_str())
    }

    /// Resolve snippet-enrichment opts: `None` means "skip enrichment".
    /// Default behavior is "enrich" — LLM consumers benefit from inline
    /// context, and `include_snippets=false` stays available as the opt-out.
    pub fn snippet_opts(args: &Value) -> Option<EnrichOpts> {
        let include = args
            .get("include_snippets")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !include {
            return None;
        }
        let ctx_lines = args
            .get("snippet_context_lines")
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(SNIPPET_DEFAULT_CONTEXT_LINES);
        Some(EnrichOpts {
            ctx_lines,
            max_hits: SNIPPET_DEFAULT_MAX_HITS,
        })
    }
}

/// Apply snippet enrichment if opts are set, otherwise return the value
/// unchanged. Lives at module scope so closures inside `with_doc` can use it.
fn maybe_enrich_locations(value: Value, opts: Option<EnrichOpts>) -> Value {
    match opts {
        Some(o) => enrich_locations(value, o),
        None => value,
    }
}

fn wrap(value: Value) -> Result<ToolResult> {
    Ok(ToolResult {
        content: vec![ContentItem::text(serde_json::to_string_pretty(&value)?)],
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
    let file_path = params::file_path(args)?;
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
            let (line, ch) = params::position(&args)?;
            let verbose = params::verbose(&args);
            let file_path = params::file_path(&args)?;
            let ws = server.resolve_workspace(&args).await?;
            let uri = ws.open_document_if_needed(&file_path).await?;
            let client = ws.current_client().await?;
            let value = client.hover(&uri, line, ch).await?;
            wrap(truncate_hover(value, HOVER_MAX_BYTES, verbose))
        }
        "rust_analyzer_definition" => {
            let (line, ch) = params::position(&args)?;
            let opts = params::snippet_opts(&args);
            with_doc(server, &args, move |c, uri| async move {
                let value = c.definition(&uri, line, ch).await?;
                Ok(maybe_enrich_locations(value, opts))
            })
            .await
        }
        "rust_analyzer_references" => {
            let (line, ch) = params::position(&args)?;
            let opts = params::snippet_opts(&args);
            with_doc(server, &args, move |c, uri| async move {
                let value = c.references(&uri, line, ch).await?;
                Ok(maybe_enrich_locations(value, opts))
            })
            .await
        }
        "rust_analyzer_completion" => {
            let (line, ch) = params::position(&args)?;
            let verbose = params::verbose(&args);
            let limit = resolve_limit(params::limit(&args), verbose, COMPLETION_DEFAULT_LIMIT);
            let file_path = params::file_path(&args)?;
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
            let (l1, c1, l2, c2) = params::range(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.code_actions(&uri, l1, c1, l2, c2).await
            })
            .await
        }
        "rust_analyzer_rename" => {
            let (line, ch) = params::position(&args)?;
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
            let (line, ch) = params::position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.prepare_rename(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_signature_help" => {
            let (line, ch) = params::position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.signature_help(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_inlay_hints" => {
            let (l1, c1, l2, c2) = params::range(&args)?;
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
            let (line, ch) = params::position(&args)?;
            let opts = params::snippet_opts(&args);
            with_doc(server, &args, move |c, uri| async move {
                let value = c.type_definition(&uri, line, ch).await?;
                Ok(maybe_enrich_locations(value, opts))
            })
            .await
        }
        "rust_analyzer_implementation" => {
            let (line, ch) = params::position(&args)?;
            let opts = params::snippet_opts(&args);
            with_doc(server, &args, move |c, uri| async move {
                let value = c.implementation(&uri, line, ch).await?;
                Ok(maybe_enrich_locations(value, opts))
            })
            .await
        }
        "rust_analyzer_expand_macro" => {
            let (line, ch) = params::position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.expand_macro(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_parent_module" => {
            let (line, ch) = params::position(&args)?;
            let opts = params::snippet_opts(&args);
            with_doc(server, &args, move |c, uri| async move {
                let value = c.parent_module(&uri, line, ch).await?;
                Ok(maybe_enrich_locations(value, opts))
            })
            .await
        }
        "rust_analyzer_runnables" => {
            let position = params::position(&args).ok();
            let opts = params::snippet_opts(&args);
            with_doc(server, &args, move |c, uri| async move {
                let value = c.runnables(&uri, position).await?;
                Ok(maybe_enrich_locations(value, opts))
            })
            .await
        }
        "rust_analyzer_related_tests" => {
            let (line, ch) = params::position(&args)?;
            let opts = params::snippet_opts(&args);
            with_doc(server, &args, move |c, uri| async move {
                let value = c.related_tests(&uri, line, ch).await?;
                Ok(maybe_enrich_locations(value, opts))
            })
            .await
        }
        "rust_analyzer_open_docs" => {
            let (line, ch) = params::position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.open_docs(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_syntax_tree" => {
            let range = params::optional_range(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.syntax_tree(&uri, range).await
            })
            .await
        }
        "rust_analyzer_view_hir" => {
            let (line, ch) = params::position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.view_hir(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_view_mir" => {
            let (line, ch) = params::position(&args)?;
            with_doc(server, &args, move |c, uri| async move {
                c.view_mir(&uri, line, ch).await
            })
            .await
        }
        "rust_analyzer_explore_symbol" => handle_explore_symbol(server, args).await,
        "rust_analyzer_call_hierarchy_incoming" => {
            handle_call_hierarchy(server, args, CallDirection::Incoming).await
        }
        "rust_analyzer_call_hierarchy_outgoing" => {
            handle_call_hierarchy(server, args, CallDirection::Outgoing).await
        }
        "rust_analyzer_type_hierarchy" => handle_type_hierarchy(server, args).await,
        "rust_analyzer_impact" => handle_impact(server, args).await,
        "rust_analyzer_get_type_by_name" => handle_get_type_by_name(server, args).await,
        "rust_analyzer_add_workspace" => handle_add_workspace(server, args).await,
        "rust_analyzer_remove_workspace" => handle_remove_workspace(server, args).await,
        "rust_analyzer_list_workspaces" => handle_list_workspaces(server, args).await,
        _ => Err(anyhow!("Unknown tool: {}", tool_name)),
    }
}

/// Composite handler that fans out the five most common follow-up calls a
/// caller would make after locating a symbol — hover, definition,
/// type_definition, parent_module, and a sample of references — in a single
/// round-trip. Drives them concurrently via `tokio::join!`; the document is
/// opened once and the workspace is resolved once. References are wrapped
/// in a 2 s timeout so a single slow workspace-wide search can't block the
/// rest of the composite.
async fn handle_explore_symbol(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let (line, ch) = params::position(&args)?;
    let snippet_opts = params::snippet_opts(&args);
    let file_path = params::file_path(&args)?;

    let ws = server.resolve_workspace(&args).await?;
    let uri = ws.open_document_if_needed(&file_path).await?;
    let client = ws.current_client().await?;

    let refs_timeout = std::time::Duration::from_secs(2);

    let (hover_res, def_res, type_def_res, parent_res, refs_res) = tokio::join!(
        client.hover(&uri, line, ch),
        client.definition(&uri, line, ch),
        client.type_definition(&uri, line, ch),
        client.parent_module(&uri, line, ch),
        tokio::time::timeout(refs_timeout, client.references(&uri, line, ch)),
    );

    // Treat a per-sub-call LSP error as null — explore_symbol is best-effort
    // and one failure shouldn't poison the rest. Real errors are still logged
    // by the LSP layer.
    let hover = hover_res.unwrap_or(Value::Null);
    let definition = maybe_enrich_locations(def_res.unwrap_or(Value::Null), snippet_opts);
    let type_definition = maybe_enrich_locations(type_def_res.unwrap_or(Value::Null), snippet_opts);
    let parent_module = maybe_enrich_locations(parent_res.unwrap_or(Value::Null), snippet_opts);

    let (refs_value, refs_timed_out) = match refs_res {
        Ok(Ok(v)) => (v, false),
        Ok(Err(_)) => (Value::Null, false),
        Err(_) => (Value::Null, true),
    };

    let references_sample = sample_references(refs_value);
    let references_sample = maybe_enrich_locations(references_sample, snippet_opts);

    let mut out = json!({
        "hover": hover,
        "definition": definition,
        "type_definition": type_definition,
        "parent_module": parent_module,
        "references_sample": references_sample,
    });
    if refs_timed_out {
        out.as_object_mut()
            .expect("just constructed")
            .insert("references_timed_out".to_string(), json!(true));
    }
    wrap(out)
}

/// Trim a references response to a small sample plus a `total` count, so the
/// LLM gets enough to navigate without blowing token budget on every consumer
/// of the symbol. The shape mirrors paginate_workspace_symbol's wrapper.
fn sample_references(value: Value) -> Value {
    const SAMPLE_SIZE: usize = 5;
    match value {
        Value::Array(items) => {
            let total = items.len();
            let shown = total.min(SAMPLE_SIZE);
            let sample: Vec<Value> = items.into_iter().take(SAMPLE_SIZE).collect();
            json!({
                "items": sample,
                "total": total,
                "shown": shown,
            })
        }
        Value::Null => Value::Null,
        other => other,
    }
}

/// Name-based symbol lookup. Splits the input on `::`, fuzzy-searches the
/// workspace by the last segment, then narrows to entries whose
/// `containerName` agrees with the path prefix. Drives `hover` +
/// `type_definition` on the first surviving match for the `primary` detail
/// block; the rest are reported in `matches` without enrichment beyond the
/// snippet walker.
async fn handle_get_type_by_name(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    const SAMPLE_SIZE: usize = 10;
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Missing name"))?
        .to_string();
    let path_parts: Vec<&str> = name.split("::").filter(|s| !s.is_empty()).collect();
    if path_parts.is_empty() {
        return Err(anyhow!("name must contain at least one non-empty segment"));
    }
    let last_segment = *path_parts.last().expect("non-empty per check above");
    let snippet_opts = params::snippet_opts(&args);

    let ws = server.resolve_workspace(&args).await?;
    let client = ws.ensure_client_started().await?;

    let raw = client.workspace_symbol(last_segment).await?;
    let all_symbols: Vec<Value> = match raw {
        Value::Array(items) => items,
        _ => Vec::new(),
    };

    let matches: Vec<Value> = all_symbols
        .into_iter()
        .filter(|s| symbol_matches_path(s, &path_parts))
        .collect();

    let total = matches.len();
    let shown = total.min(SAMPLE_SIZE);
    let matches_sample: Vec<Value> = matches.iter().take(SAMPLE_SIZE).cloned().collect();

    let primary = match matches.first() {
        Some(first) => primary_detail(&ws, &client, first).await,
        None => Value::Null,
    };

    let mut out = json!({
        "matches": matches_sample,
        "total": total,
        "shown": shown,
    });
    if !primary.is_null() {
        out.as_object_mut()
            .expect("just constructed")
            .insert("primary".to_string(), primary);
    }
    wrap(maybe_enrich_locations(out, snippet_opts))
}

/// Decide whether a `WorkspaceSymbol`/`SymbolInformation` JSON object satisfies
/// the user-supplied path. Last segment must equal `name`; for multi-segment
/// paths the `containerName` either matches the prefix exactly, ends with it,
/// or contains every prefix segment as a substring (so "auth::User" still
/// matches `containerName: "crate::auth"` even when rust-analyzer reports a
/// shortened container).
fn symbol_matches_path(symbol: &Value, path_parts: &[&str]) -> bool {
    let Some(name) = symbol.get("name").and_then(Value::as_str) else {
        return false;
    };
    let Some(last) = path_parts.last() else {
        return false;
    };
    if name != *last {
        return false;
    }
    if path_parts.len() == 1 {
        return true;
    }
    let prefix_parts = &path_parts[..path_parts.len() - 1];
    let container = symbol
        .get("containerName")
        .and_then(Value::as_str)
        .unwrap_or("");
    let full_prefix = prefix_parts.join("::");
    if container == full_prefix || container.ends_with(&full_prefix) {
        return true;
    }
    prefix_parts.iter().all(|seg| container.contains(seg))
}

/// Open the document for the first match's location and run hover +
/// type_definition concurrently. Per-sub-call errors degrade to null so a
/// single failure doesn't poison the primary block.
async fn primary_detail(
    ws: &Arc<WorkspaceEntry>,
    client: &Arc<RustAnalyzerClient>,
    symbol: &Value,
) -> Value {
    let Some(location) = symbol.get("location") else {
        return Value::Null;
    };
    let Some(uri_str) = location.get("uri").and_then(Value::as_str) else {
        return Value::Null;
    };
    let Some(file_path) = uri_to_path_for_open(uri_str) else {
        return Value::Null;
    };
    let Some(range) = location.get("range") else {
        return Value::Null;
    };
    let Some(line) = range
        .get("start")
        .and_then(|s| s.get("line"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
    else {
        return Value::Null;
    };
    let Some(character) = range
        .get("start")
        .and_then(|s| s.get("character"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
    else {
        return Value::Null;
    };

    let opened_uri = match ws.open_document_if_needed(&file_path).await {
        Ok(u) => u,
        Err(_) => return Value::Null,
    };

    let (hover_res, type_def_res) = tokio::join!(
        client.hover(&opened_uri, line, character),
        client.type_definition(&opened_uri, line, character),
    );

    json!({
        "hover": hover_res.unwrap_or(Value::Null),
        "type_definition": type_def_res.unwrap_or(Value::Null),
        "location": location.clone(),
    })
}

/// Strip `file://` so the resulting absolute path can be passed to
/// `open_document_if_needed` (which `workspace_root.join`s it; an absolute
/// second argument wins, so the workspace root is harmless filler).
fn uri_to_path_for_open(uri: &str) -> Option<String> {
    uri.strip_prefix("file://").map(str::to_string)
}

/// Composite that estimates the blast radius of changing a symbol. Fans out
/// four LSP queries in parallel — references, prepareCallHierarchy +
/// incomingCalls (callers), prepareTypeHierarchy + subtypes (implementors),
/// and relatedTests — and packages each as a `{ items, total, shown,
/// timed_out? }` bucket so the LLM sees a stable shape regardless of which
/// queries were productive. Items are sampled (cap 10) to keep the response
/// scan-able; `total` always reflects the full count.
async fn handle_impact(server: &Arc<RustAnalyzerMCPServer>, args: Value) -> Result<ToolResult> {
    const SAMPLE_SIZE: usize = 10;
    let bucket_timeout = std::time::Duration::from_secs(2);

    let (line, ch) = params::position(&args)?;
    let snippet_opts = params::snippet_opts(&args);
    let file_path = params::file_path(&args)?;

    let ws = server.resolve_workspace(&args).await?;
    let uri = ws.open_document_if_needed(&file_path).await?;
    let client = ws.current_client().await?;

    // Stage 1: four parallel queries — three direct lookups plus the two
    // hierarchy `prepare*` calls. The hierarchy fan-outs in stage 2 depend
    // on stage-1 prepared items, but everything else can race.
    let (refs_res, prep_call_res, prep_type_res, tests_res) = tokio::join!(
        tokio::time::timeout(bucket_timeout, client.references(&uri, line, ch)),
        tokio::time::timeout(
            bucket_timeout,
            client.prepare_call_hierarchy(&uri, line, ch)
        ),
        tokio::time::timeout(
            bucket_timeout,
            client.prepare_type_hierarchy(&uri, line, ch)
        ),
        tokio::time::timeout(bucket_timeout, client.related_tests(&uri, line, ch)),
    );

    let prep_call_items = take_prepared_items(&prep_call_res);
    let prep_type_items = take_prepared_items(&prep_type_res);

    // Stage 2: fan out hierarchy children. A single prepared item is the
    // common case; trait methods with multiple impls return more.
    let callers_fut = collect_caller_items(&client, &prep_call_items, bucket_timeout);
    let implementors_fut = collect_subtype_items(&client, &prep_type_items, bucket_timeout);
    let (callers_collected, implementors_collected) = tokio::join!(callers_fut, implementors_fut);

    let references_bucket = bucket_from_locations(refs_res, SAMPLE_SIZE);
    let callers_bucket = bucket_from_collected(callers_collected, SAMPLE_SIZE);
    let implementors_bucket = bucket_from_collected(implementors_collected, SAMPLE_SIZE);
    let tests_bucket = bucket_from_locations(tests_res, SAMPLE_SIZE);

    let value = json!({
        "references": references_bucket,
        "callers": callers_bucket,
        "implementors": implementors_bucket,
        "tests": tests_bucket,
    });
    wrap(maybe_enrich_locations(value, snippet_opts))
}

/// Pull the array out of a timed `prepareXxx` result. Errors and timeouts
/// degrade silently to empty — stage 2 just gets nothing to fan out and the
/// resulting bucket carries `total: 0` (and `timed_out: true` if relevant —
/// that is set by the bucket helpers based on the same inputs they see).
fn take_prepared_items(
    res: &std::result::Result<Result<Value>, tokio::time::error::Elapsed>,
) -> Vec<Value> {
    match res {
        Ok(Ok(Value::Array(items))) => items.clone(),
        _ => Vec::new(),
    }
}

/// Outcome of a per-bucket fan-out: collected items plus a flag telling the
/// caller whether at least one sub-call ran out of time so we can surface
/// `timed_out: true` to the LLM.
struct CollectedBucket {
    items: Vec<Value>,
    timed_out: bool,
}

async fn collect_caller_items(
    client: &Arc<RustAnalyzerClient>,
    prepared: &[Value],
    timeout: std::time::Duration,
) -> CollectedBucket {
    let mut items = Vec::new();
    let mut timed_out = false;
    for item in prepared {
        match tokio::time::timeout(timeout, client.call_hierarchy_incoming(item)).await {
            Ok(Ok(Value::Array(calls))) => {
                for call in calls {
                    if let Some(from) = call.get("from").cloned() {
                        items.push(from);
                    }
                }
            }
            Ok(Ok(_)) | Ok(Err(_)) => {}
            Err(_) => timed_out = true,
        }
    }
    CollectedBucket { items, timed_out }
}

async fn collect_subtype_items(
    client: &Arc<RustAnalyzerClient>,
    prepared: &[Value],
    timeout: std::time::Duration,
) -> CollectedBucket {
    let mut items = Vec::new();
    let mut timed_out = false;
    for item in prepared {
        match tokio::time::timeout(timeout, client.type_hierarchy_subtypes(item)).await {
            Ok(Ok(Value::Array(subs))) => items.extend(subs),
            Ok(Ok(_)) | Ok(Err(_)) => {}
            Err(_) => timed_out = true,
        }
    }
    CollectedBucket { items, timed_out }
}

/// Wrap a timed `references`/`related_tests`-style result (Vec<Location>)
/// into the bucket shape. Errors degrade to total=0; the timeout case is
/// reflected with `timed_out: true`.
fn bucket_from_locations(
    res: std::result::Result<Result<Value>, tokio::time::error::Elapsed>,
    sample_size: usize,
) -> Value {
    match res {
        Ok(Ok(Value::Array(items))) => sample_to_bucket(items, sample_size, false),
        Ok(Ok(_)) | Ok(Err(_)) => sample_to_bucket(Vec::new(), sample_size, false),
        Err(_) => sample_to_bucket(Vec::new(), sample_size, true),
    }
}

fn bucket_from_collected(collected: CollectedBucket, sample_size: usize) -> Value {
    sample_to_bucket(collected.items, sample_size, collected.timed_out)
}

fn sample_to_bucket(items: Vec<Value>, sample_size: usize, timed_out: bool) -> Value {
    let total = items.len();
    let shown = total.min(sample_size);
    let sample: Vec<Value> = items.into_iter().take(sample_size).collect();
    let mut out = json!({ "items": sample, "total": total, "shown": shown });
    if timed_out {
        out.as_object_mut()
            .expect("just constructed")
            .insert("timed_out".to_string(), json!(true));
    }
    out
}

#[derive(Clone, Copy)]
enum CallDirection {
    Incoming,
    Outgoing,
}

/// Composite for `callHierarchy/{incoming,outgoing}Calls`. Drives
/// `prepareCallHierarchy` first, then fans out one call per prepared item
/// (typically 1; trait methods with multiple impls return more). Each
/// `fromRanges` entry is reshaped into a self-contained `{uri, range}` so
/// the snippet walker enriches every call site, not just the caller header.
async fn handle_call_hierarchy(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
    direction: CallDirection,
) -> Result<ToolResult> {
    let (line, ch) = params::position(&args)?;
    let snippet_opts = params::snippet_opts(&args);
    let file_path = params::file_path(&args)?;

    let ws = server.resolve_workspace(&args).await?;
    let uri = ws.open_document_if_needed(&file_path).await?;
    let client = ws.current_client().await?;

    let prepared = client.prepare_call_hierarchy(&uri, line, ch).await?;
    let prepared_items: Vec<Value> = match prepared {
        Value::Array(items) => items,
        Value::Null => Vec::new(),
        // rust-analyzer should always return an array per spec; treat anything
        // else as empty so the LLM gets a stable shape.
        _ => Vec::new(),
    };

    let mut out_items: Vec<Value> = Vec::with_capacity(prepared_items.len());
    for item in prepared_items {
        let calls_raw = match direction {
            CallDirection::Incoming => client.call_hierarchy_incoming(&item).await?,
            CallDirection::Outgoing => client.call_hierarchy_outgoing(&item).await?,
        };
        let calls = reshape_call_hierarchy_calls(&item, calls_raw, direction);
        out_items.push(match direction {
            CallDirection::Incoming => json!({ "item": item, "incoming": calls }),
            CallDirection::Outgoing => json!({ "item": item, "outgoing": calls }),
        });
    }

    let total = out_items.len();
    let value = json!({ "items": out_items, "total": total });
    wrap(maybe_enrich_locations(value, snippet_opts))
}

/// Convert `{ from|to: Item, fromRanges: [Range] }` to
/// `{ from|to: Item, call_sites: [{uri, range}] }`.
///
/// For incoming calls each `fromRanges[i]` lives in `from.uri` (the caller's
/// file). For outgoing calls they live in the *current* item's file (the
/// place doing the calling), not in `to.uri`.
fn reshape_call_hierarchy_calls(
    current_item: &Value,
    calls: Value,
    direction: CallDirection,
) -> Value {
    let Value::Array(arr) = calls else {
        return json!([]);
    };
    let current_uri = current_item.get("uri").and_then(Value::as_str);
    let reshaped: Vec<Value> = arr
        .into_iter()
        .filter_map(|entry| {
            let mut obj = entry.as_object()?.clone();
            let ranges = obj.remove("fromRanges").unwrap_or(Value::Null);
            let site_uri = match direction {
                CallDirection::Incoming => obj
                    .get("from")
                    .and_then(|v| v.get("uri"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                CallDirection::Outgoing => current_uri.map(str::to_string),
            };
            let call_sites = match (site_uri, ranges) {
                (Some(u), Value::Array(rs)) => rs
                    .into_iter()
                    .map(|r| json!({ "uri": u, "range": r }))
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            obj.insert("call_sites".to_string(), Value::Array(call_sites));
            Some(Value::Object(obj))
        })
        .collect();
    Value::Array(reshaped)
}

/// Composite for `typeHierarchy/{super,sub}types`. Drives
/// `prepareTypeHierarchy`, then fans out the requested directions per
/// prepared item.
async fn handle_type_hierarchy(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let (line, ch) = params::position(&args)?;
    let snippet_opts = params::snippet_opts(&args);
    let file_path = params::file_path(&args)?;
    let direction = args
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("both");
    let (want_super, want_sub) = match direction {
        "supertypes" => (true, false),
        "subtypes" => (false, true),
        "both" => (true, true),
        other => {
            return Err(anyhow!(
                "Invalid direction: {other:?} (expected one of: supertypes, subtypes, both)"
            ))
        }
    };

    let ws = server.resolve_workspace(&args).await?;
    let uri = ws.open_document_if_needed(&file_path).await?;
    let client = ws.current_client().await?;

    let prepared = client.prepare_type_hierarchy(&uri, line, ch).await?;
    let prepared_items: Vec<Value> = match prepared {
        Value::Array(items) => items,
        Value::Null => Vec::new(),
        _ => Vec::new(),
    };

    let mut out_items: Vec<Value> = Vec::with_capacity(prepared_items.len());
    for item in prepared_items {
        let (super_res, sub_res) = tokio::join!(
            async {
                if want_super {
                    Some(client.type_hierarchy_supertypes(&item).await)
                } else {
                    None
                }
            },
            async {
                if want_sub {
                    Some(client.type_hierarchy_subtypes(&item).await)
                } else {
                    None
                }
            },
        );
        let mut entry = serde_json::Map::new();
        entry.insert("item".to_string(), item);
        if let Some(res) = super_res {
            entry.insert("supertypes".to_string(), res?);
        }
        if let Some(res) = sub_res {
            entry.insert("subtypes".to_string(), res?);
        }
        out_items.push(Value::Object(entry));
    }

    let total = out_items.len();
    let value = json!({ "items": out_items, "total": total });
    wrap(maybe_enrich_locations(value, snippet_opts))
}

async fn handle_workspace_symbol(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let query = args["query"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing query"))?;
    let verbose = params::verbose(&args);
    let cursor = parse_cursor(params::cursor(&args));
    let limit = resolve_limit(
        params::limit(&args),
        verbose,
        WORKSPACE_SYMBOL_DEFAULT_LIMIT,
    );
    let snippet_opts = params::snippet_opts(&args);
    let ws = server.resolve_workspace(&args).await?;
    let client = ws.ensure_client_started().await?;
    let value = client.workspace_symbol(query).await?;
    let paginated = paginate_workspace_symbol(value, cursor, limit, verbose);
    wrap(maybe_enrich_locations(paginated, snippet_opts))
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
        content: vec![ContentItem::text(format!(
            "Workspace set to: {}",
            new_root.display()
        ))],
    })
}

async fn handle_diagnostics(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let file_path = params::file_path(&args)?;
    let ws = server.resolve_workspace(&args).await?;
    let uri = ws.open_document_if_needed(&file_path).await?;
    let client = ws.current_client().await?;

    // Try once eagerly — diagnostics may already be cached from a prior call.
    let mut result = match client.diagnostics(&uri).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("Initial diagnostics fetch failed, will wait: {}", e);
            json!([])
        }
    };

    // If nothing's there yet, wait for the connection task to publish something
    // for us. Each wake-up could be for a different URI, so we re-check the
    // map after each pulse and bail out as soon as ours is non-empty or the
    // overall budget expires.
    let total_budget = tokio::time::Duration::from_secs(8);
    let pulse_timeout = tokio::time::Duration::from_secs(2);
    let deadline = tokio::time::Instant::now() + total_budget;
    while result.as_array().is_some_and(|a| a.is_empty()) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = pulse_timeout.min(remaining);
        if client.wait_for_diagnostics_change(wait).await.is_err() {
            // No publish in this window; one more chance to read whatever's there.
            break;
        }
        match client.diagnostics(&uri).await {
            Ok(v) => result = v,
            Err(e) => tracing::debug!("Transient diagnostics error after pulse: {}", e),
        }
    }

    let diagnostics = format_diagnostics(&file_path, &result);
    wrap(diagnostics)
}

async fn handle_workspace_diagnostics(
    server: &Arc<RustAnalyzerMCPServer>,
    args: Value,
) -> Result<ToolResult> {
    let verbose = params::verbose(&args);
    let cursor = parse_cursor(params::cursor(&args));
    let limit = resolve_limit(
        params::limit(&args),
        verbose,
        WORKSPACE_DIAGNOSTICS_DEFAULT_LIMIT,
    );
    let snippet_opts = params::snippet_opts(&args);
    let ws = server.resolve_workspace(&args).await?;
    let client = ws.ensure_client_started().await?;
    let result = client.workspace_diagnostics().await?;
    let workspace_root = ws.root_clone();
    let formatted = format_workspace_diagnostics(&workspace_root, &result);
    let paginated = paginate_workspace_diagnostics(formatted, cursor, limit, verbose);
    let enriched = match snippet_opts {
        Some(o) => enrich_workspace_diagnostics(paginated, o),
        None => paginated,
    };
    wrap(enriched)
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

#[derive(Serialize, Default, Clone, Copy)]
struct SeverityCounts {
    errors: u64,
    warnings: u64,
    information: u64,
    hints: u64,
}

impl SeverityCounts {
    fn count(&mut self, severity: u64) {
        match severity {
            1 => self.errors += 1,
            2 => self.warnings += 1,
            3 => self.information += 1,
            4 => self.hints += 1,
            _ => {}
        }
    }

    fn merge(&mut self, other: &Self) {
        self.errors += other.errors;
        self.warnings += other.warnings;
        self.information += other.information;
        self.hints += other.hints;
    }
}

#[derive(Serialize)]
struct FileDiagnostics<'a> {
    diagnostics: &'a Value,
    summary: SeverityCounts,
}

#[derive(Serialize)]
struct WorkspaceDiagnosticsSummary {
    total_files: u64,
    #[serde(flatten)]
    severity: TotalSeverity,
}

/// Wire-compatible flattened total fields (`total_errors`, `total_warnings`, …).
#[derive(Serialize, Default)]
struct TotalSeverity {
    total_errors: u64,
    total_warnings: u64,
    total_information: u64,
    total_hints: u64,
}

#[derive(Serialize)]
struct WorkspaceDiagnostics<'a> {
    workspace: String,
    files: BTreeMap<String, FileDiagnostics<'a>>,
    summary: WorkspaceDiagnosticsSummary,
}

fn format_workspace_diagnostics(workspace_root: &Path, result: &Value) -> Value {
    let workspace = workspace_root.display().to_string();

    if !result.is_object() {
        // Handle unexpected format. Items shape (`{ items: [...] }`) is the
        // pull-model fallback; everything else is "unknown".
        if let Some(items) = result.get("items") {
            return json!({
                "workspace": workspace,
                "diagnostics": items,
                "summary": {
                    "total_diagnostics": items.as_array().map(|a| a.len()).unwrap_or(0),
                    "by_severity": {}
                }
            });
        }

        return json!({
            "workspace": workspace,
            "diagnostics": result,
            "summary": { "note": "Unexpected response format from rust-analyzer" }
        });
    }

    // The expected shape: a map of `uri -> [diagnostic]`.
    let Some(obj) = result.as_object() else {
        return json!({
            "workspace": workspace,
            "files": {},
            "summary": WorkspaceDiagnosticsSummary {
                total_files: 0,
                severity: TotalSeverity::default(),
            }
        });
    };

    let mut files = BTreeMap::new();
    let mut totals = SeverityCounts::default();

    for (uri, diagnostics) in obj {
        let Some(diag_array) = diagnostics.as_array() else {
            continue;
        };
        if diag_array.is_empty() {
            continue;
        }

        let mut counts = SeverityCounts::default();
        for diag in diag_array {
            if let Some(severity) = diag.get("severity").and_then(|s| s.as_u64()) {
                counts.count(severity);
            }
        }
        totals.merge(&counts);
        files.insert(
            uri.clone(),
            FileDiagnostics {
                diagnostics,
                summary: counts,
            },
        );
    }

    let summary = WorkspaceDiagnosticsSummary {
        total_files: files.len() as u64,
        severity: TotalSeverity {
            total_errors: totals.errors,
            total_warnings: totals.warnings,
            total_information: totals.information,
            total_hints: totals.hints,
        },
    };

    serde_json::to_value(WorkspaceDiagnostics {
        workspace,
        files,
        summary,
    })
    .expect("WorkspaceDiagnostics always serializes to JSON")
}
