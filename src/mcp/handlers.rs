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
