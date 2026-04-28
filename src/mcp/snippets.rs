//! Snippet enrichment for tool outputs that emit `Location`s.
//!
//! When a tool returns one or more LSP `Location` (or `LocationLink`) values,
//! an LLM consumer almost always needs the surrounding source to make use of
//! the result. This module walks the LSP response and attaches a `snippet`
//! sibling to each location so the next reasoning step doesn't need a
//! follow-up tool call to `Read` the file.
//!
//! Output shape, attached as a sibling to the location object:
//! ```json
//! { "snippet": { "start_line": 10, "lines": ["...", "..."] } }
//! ```
//! `start_line` is 0-based (LSP convention). When a snippet is byte-capped,
//! a `truncated: true` field is added.
//!
//! Reads happen synchronously via `std::fs` — file I/O is bounded (default
//! cap of 50 hits per tool call, ~400 bytes per snippet) and the handler
//! already runs in its own spawned task per request, so the worker thread
//! is the only thing blocking.

use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub const SNIPPET_DEFAULT_CONTEXT_LINES: usize = 2;
pub const SNIPPET_DEFAULT_MAX_HITS: usize = 50;
pub const SNIPPET_MAX_BYTES_PER_HIT: usize = 400;

/// Knobs for snippet enrichment. `None` means "skip enrichment entirely".
#[derive(Clone, Copy, Debug)]
pub struct EnrichOpts {
    pub ctx_lines: usize,
    pub max_hits: usize,
}

impl Default for EnrichOpts {
    fn default() -> Self {
        Self {
            ctx_lines: SNIPPET_DEFAULT_CONTEXT_LINES,
            max_hits: SNIPPET_DEFAULT_MAX_HITS,
        }
    }
}

/// Per-call cache of file contents. Lives only for the duration of one
/// enrichment pass — between calls files may have changed, so persisting
/// across calls would invite stale reads for negligible gain.
struct SnippetCtx {
    cache: HashMap<PathBuf, Vec<String>>,
    ctx_lines: usize,
    remaining_budget: usize,
    /// Set to true once we hit `max_hits`, so the wrapper output can advertise
    /// that further locations were not enriched.
    capped: bool,
}

impl SnippetCtx {
    fn new(opts: EnrichOpts) -> Self {
        Self {
            cache: HashMap::new(),
            ctx_lines: opts.ctx_lines,
            remaining_budget: opts.max_hits,
            capped: false,
        }
    }

    fn lines_for(&mut self, path: &Path) -> Option<&[String]> {
        if !self.cache.contains_key(path) {
            let content = std::fs::read_to_string(path).ok()?;
            // `split('\n')` (not `lines()`) so we keep an empty trailing entry
            // when the file ends with a newline — line numbers stay accurate
            // for ranges referring to the last line.
            let lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
            self.cache.insert(path.to_path_buf(), lines);
        }
        self.cache.get(path).map(Vec::as_slice)
    }

    fn snippet_for(&mut self, uri: &str, range: &Value) -> Option<Value> {
        if self.remaining_budget == 0 {
            self.capped = true;
            return None;
        }
        let path = uri_to_path(uri)?;
        let start_line = range.get("start")?.get("line")?.as_u64()? as usize;
        let end_line = range.get("end")?.get("line")?.as_u64()? as usize;
        let ctx = self.ctx_lines;

        let lines = self.lines_for(&path)?;
        if lines.is_empty() {
            return None;
        }

        let from = start_line.saturating_sub(ctx);
        let to = end_line
            .saturating_add(ctx)
            .saturating_add(1)
            .min(lines.len());
        if from >= to {
            return None;
        }

        let mut total_bytes = 0usize;
        let mut out: Vec<String> = Vec::with_capacity(to - from);
        let mut hit_byte_cap = false;
        for line in &lines[from..to] {
            // +1 for the implicit '\n' so the cap reflects rendered size
            let line_bytes = line.len().saturating_add(1);
            if total_bytes.saturating_add(line_bytes) > SNIPPET_MAX_BYTES_PER_HIT && !out.is_empty()
            {
                hit_byte_cap = true;
                break;
            }
            total_bytes = total_bytes.saturating_add(line_bytes);
            out.push(line.clone());
        }

        self.remaining_budget = self.remaining_budget.saturating_sub(1);

        let mut snippet = json!({
            "start_line": from,
            "lines": out,
        });
        if hit_byte_cap {
            snippet
                .as_object_mut()
                .expect("just constructed")
                .insert("truncated".to_string(), json!(true));
        }
        Some(snippet)
    }
}

/// Strip `file://` and decode percent-escapes good enough for the file URIs
/// rust-analyzer emits (we control the encoding side via
/// `WorkspaceEntry::open_document_if_needed`, which just `format!`s the path —
/// no percent-encoding is added there). Stays a thin function so a future
/// move to a proper URL crate is local.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let stripped = uri.strip_prefix("file://")?;
    Some(PathBuf::from(stripped))
}

/// Walk a value and attach a `snippet` sibling to every object that looks like
/// a `Location` (`{ uri, range }`) or `LocationLink`
/// (`{ targetUri, targetRange }`). Idempotent for non-location subtrees.
///
/// Note: pass already-paginated values so the budget isn't burned on items
/// the LLM will never see.
pub fn enrich_locations(value: Value, opts: EnrichOpts) -> Value {
    let mut ctx = SnippetCtx::new(opts);
    let enriched = walk(value, &mut ctx);
    annotate_capped(enriched, ctx.capped)
}

fn walk(value: Value, ctx: &mut SnippetCtx) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(|v| walk(v, ctx)).collect()),
        Value::Object(obj) => {
            // Detect Location / LocationLink shape on the *current* object
            // before consuming it, so the snippet can be inserted after we
            // recurse into children.
            let snippet = location_snippet(&obj, ctx);
            let mut new_obj = serde_json::Map::with_capacity(obj.len() + 1);
            for (k, v) in obj {
                new_obj.insert(k, walk(v, ctx));
            }
            if let Some(snip) = snippet {
                new_obj.insert("snippet".to_string(), snip);
            }
            Value::Object(new_obj)
        }
        other => other,
    }
}

fn location_snippet(obj: &serde_json::Map<String, Value>, ctx: &mut SnippetCtx) -> Option<Value> {
    if let (Some(uri), Some(range)) = (obj.get("uri").and_then(Value::as_str), obj.get("range")) {
        return ctx.snippet_for(uri, range);
    }
    if let (Some(uri), Some(range)) = (
        obj.get("targetUri").and_then(Value::as_str),
        obj.get("targetRange"),
    ) {
        return ctx.snippet_for(uri, range);
    }
    None
}

/// `workspace_diagnostics` has shape `{ files: { uri: { diagnostics: [...] } } }`
/// where each diagnostic carries a `range` but the URI lives in the parent key.
/// The generic walker can't see that pairing, so this one is bespoke.
pub fn enrich_workspace_diagnostics(mut value: Value, opts: EnrichOpts) -> Value {
    let mut ctx = SnippetCtx::new(opts);
    {
        let Some(obj) = value.as_object_mut() else {
            return value;
        };
        let Some(files) = obj.get_mut("files").and_then(|v| v.as_object_mut()) else {
            return value;
        };
        for (uri, file_entry) in files.iter_mut() {
            let Some(diags) = file_entry
                .get_mut("diagnostics")
                .and_then(|v| v.as_array_mut())
            else {
                continue;
            };
            for diag in diags.iter_mut() {
                let Some(diag_obj) = diag.as_object_mut() else {
                    continue;
                };
                let Some(range) = diag_obj.get("range").cloned() else {
                    continue;
                };
                if let Some(snippet) = ctx.snippet_for(uri, &range) {
                    diag_obj.insert("snippet".to_string(), snippet);
                }
            }
        }
    }
    annotate_capped(value, ctx.capped)
}

fn annotate_capped(mut value: Value, capped: bool) -> Value {
    if !capped {
        return value;
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "_snippets_capped".to_string(),
            json!({
                "hint": "max_hits reached; later locations were not enriched. Pass include_snippets=false or raise via the snippet_max_hits arg if exposed."
            }),
        );
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("tmp");
        f.write_all(contents.as_bytes()).expect("write");
        f
    }

    fn uri_for(file: &tempfile::NamedTempFile) -> String {
        format!("file://{}", file.path().display())
    }

    #[test]
    fn enriches_single_location() {
        let f = write_tmp("line0\nline1\nline2\nline3\nline4\n");
        let uri = uri_for(&f);
        let value = json!({
            "uri": uri,
            "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 5 } }
        });
        let out = enrich_locations(value, EnrichOpts::default());
        let snippet = &out["snippet"];
        assert_eq!(snippet["start_line"], 0); // 2 - ctx_lines(2) = 0
        let lines = snippet["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 5); // 0..=4
        assert_eq!(lines[2], "line2");
    }

    #[test]
    fn enriches_array_of_locations() {
        let f = write_tmp("a\nb\nc\nd\ne\n");
        let uri = uri_for(&f);
        let value = json!([
            {
                "uri": uri,
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } }
            },
            {
                "uri": uri,
                "range": { "start": { "line": 3, "character": 0 }, "end": { "line": 3, "character": 1 } }
            }
        ]);
        let out = enrich_locations(value, EnrichOpts::default());
        let arr = out.as_array().unwrap();
        assert!(arr[0].get("snippet").is_some());
        assert!(arr[1].get("snippet").is_some());
    }

    #[test]
    fn enriches_location_link_shape() {
        let f = write_tmp("a\nb\nc\n");
        let uri = uri_for(&f);
        let value = json!({
            "originSelectionRange": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "targetUri": uri,
            "targetRange": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } },
            "targetSelectionRange": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } }
        });
        let out = enrich_locations(value, EnrichOpts::default());
        assert!(out.get("snippet").is_some());
    }

    #[test]
    fn enriches_nested_location_in_workspace_symbol_shape() {
        let f = write_tmp("fn one() {}\nfn two() {}\nfn three() {}\n");
        let uri = uri_for(&f);
        // Mirrors how paginate_workspace_symbol wraps the page
        let value = json!({
            "symbols": [
                {
                    "name": "two",
                    "kind": 12,
                    "location": {
                        "uri": uri,
                        "range": { "start": { "line": 1, "character": 3 }, "end": { "line": 1, "character": 6 } }
                    }
                }
            ],
            "total": 1,
            "returned": 1
        });
        let out = enrich_locations(value, EnrichOpts::default());
        let snippet = &out["symbols"][0]["location"]["snippet"];
        assert!(snippet.is_object(), "expected nested snippet, got {out:#?}");
    }

    #[test]
    fn null_passthrough() {
        let out = enrich_locations(json!(null), EnrichOpts::default());
        assert!(out.is_null());
    }

    #[test]
    fn missing_file_does_not_crash() {
        let value = json!({
            "uri": "file:///does/not/exist/abcxyz.rs",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } }
        });
        let out = enrich_locations(value, EnrichOpts::default());
        assert!(out.get("snippet").is_none());
    }

    #[test]
    fn budget_caps_after_max_hits() {
        let f = write_tmp("a\nb\nc\n");
        let uri = uri_for(&f);
        let mut items = Vec::new();
        for _ in 0..5 {
            items.push(json!({
                "uri": uri,
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } }
            }));
        }
        let opts = EnrichOpts {
            ctx_lines: 1,
            max_hits: 2,
        };
        let out = enrich_locations(json!(items), opts);
        let arr = out.as_array().unwrap();
        let with_snippets = arr.iter().filter(|v| v.get("snippet").is_some()).count();
        assert_eq!(with_snippets, 2);
    }

    #[test]
    fn capped_marker_attached_to_top_level_object() {
        let f = write_tmp("a\nb\nc\n");
        let uri = uri_for(&f);
        let value = json!({
            "symbols": (0..5).map(|_| json!({
                "location": {
                    "uri": uri,
                    "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } }
                }
            })).collect::<Vec<_>>(),
        });
        let opts = EnrichOpts {
            ctx_lines: 0,
            max_hits: 2,
        };
        let out = enrich_locations(value, opts);
        assert!(out.get("_snippets_capped").is_some());
    }

    #[test]
    fn byte_cap_truncates_long_lines() {
        let huge = "x".repeat(SNIPPET_MAX_BYTES_PER_HIT);
        let contents = format!("a\n{huge}\n{huge}\n{huge}\n");
        let f = write_tmp(&contents);
        let uri = uri_for(&f);
        let value = json!({
            "uri": uri,
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 3, "character": 0 } }
        });
        let out = enrich_locations(value, EnrichOpts::default());
        let snippet = &out["snippet"];
        // Exactly one line fit (each line is at the byte cap with the newline)
        assert_eq!(snippet["truncated"], json!(true));
        assert!(snippet["lines"].as_array().unwrap().len() < 4);
    }

    #[test]
    fn workspace_diagnostics_enriches_per_file() {
        let f = write_tmp("err\nok\nstill ok\n");
        let uri = uri_for(&f);
        let value = json!({
            "workspace": "/tmp",
            "files": {
                uri.clone(): {
                    "diagnostics": [
                        {
                            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } },
                            "message": "boom"
                        }
                    ],
                    "summary": {}
                }
            }
        });
        let out = enrich_workspace_diagnostics(value, EnrichOpts::default());
        let diag = &out["files"][&uri]["diagnostics"][0];
        assert!(diag.get("snippet").is_some());
    }

    #[test]
    fn enrichment_idempotent_for_unrelated_subtree() {
        // Object with `uri` but no `range` shouldn't get a snippet
        let value = json!({
            "uri": "file:///foo",
            "other": "data"
        });
        let out = enrich_locations(value.clone(), EnrichOpts::default());
        assert!(out.get("snippet").is_none());
        assert_eq!(out["other"], json!("data"));
    }

    #[test]
    fn caches_file_within_single_call() {
        // If two locations point at the same file, the file should only be
        // read once. We can't easily observe disk reads, but we can at least
        // check both got snippets even when the file is small.
        let f = write_tmp("x\ny\nz\n");
        let uri = uri_for(&f);
        let value = json!([
            { "uri": uri, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } } },
            { "uri": uri, "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 1 } } },
        ]);
        let out = enrich_locations(value, EnrichOpts::default());
        let arr = out.as_array().unwrap();
        assert!(arr[0].get("snippet").is_some());
        assert!(arr[1].get("snippet").is_some());
    }
}
