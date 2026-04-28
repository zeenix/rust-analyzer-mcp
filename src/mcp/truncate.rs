//! Result-shaping for tool outputs: truncation and pagination.
//!
//! These helpers run in `mcp::handlers` after the LSP call returns, so the LSP
//! layer stays a thin passthrough. Each helper preserves `null` (used by the
//! Hybrid-Error-API to signal "no result / indexer not ready").
//!
//! Output shapes are wrapper objects, not raw LSP responses — the tools are
//! consumed by LLMs that benefit from explicit `total` / `returned` /
//! `next_cursor` fields more than from LSP-spec fidelity.

use serde_json::{json, Value};

pub const HOVER_MAX_BYTES: usize = 5000;
pub const COMPLETION_DEFAULT_LIMIT: usize = 50;
pub const WORKSPACE_SYMBOL_DEFAULT_LIMIT: usize = 100;
pub const WORKSPACE_DIAGNOSTICS_DEFAULT_LIMIT: usize = 50;

/// Maximum value an LLM-supplied `limit` can take, regardless of `verbose`.
/// Acts as a guardrail against accidental enormous responses.
pub const ABSOLUTE_LIMIT_CAP: usize = 1000;

/// Parse a cursor string to a start index. Stale / unparseable cursors map to
/// `0` so callers degrade gracefully rather than erroring.
pub fn parse_cursor(cursor: Option<&str>) -> usize {
    cursor.and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Resolve effective limit: explicit `limit` wins, capped at `ABSOLUTE_LIMIT_CAP`,
/// `verbose=true` removes the default cap (still respects absolute cap).
pub fn resolve_limit(limit: Option<usize>, verbose: bool, default: usize) -> usize {
    let raw = match (limit, verbose) {
        (Some(l), _) => l,
        (None, true) => ABSOLUTE_LIMIT_CAP,
        (None, false) => default,
    };
    raw.clamp(1, ABSOLUTE_LIMIT_CAP)
}

/// Trim hover markdown content above `max_bytes`. Mutates the response in place
/// and adds a `_truncated` sibling describing what happened. Returns the value.
///
/// LSP hover shape: `{ contents: MarkupContent | string | MarkedString[], range? }`.
/// We handle `MarkupContent` (the variant rust-analyzer emits) and the bare
/// string variant. Other shapes are returned unchanged.
pub fn truncate_hover(value: Value, max_bytes: usize, verbose: bool) -> Value {
    if verbose || value.is_null() {
        return value;
    }
    let mut value = value;
    let Some(contents) = value.get_mut("contents") else {
        return value;
    };

    let original_bytes;
    let truncated;

    if let Some(s) = contents.as_str() {
        original_bytes = s.len();
        if original_bytes <= max_bytes {
            return value;
        }
        let trimmed = trim_to_char_boundary(s, max_bytes);
        truncated = true;
        *contents = json!(format!(
            "{trimmed}\n\n_(truncated — {original_bytes} bytes total, set verbose=true for full content)_"
        ));
    } else if let Some(obj) = contents.as_object_mut() {
        let Some(val) = obj.get("value").and_then(|v| v.as_str()) else {
            return value;
        };
        original_bytes = val.len();
        if original_bytes <= max_bytes {
            return value;
        }
        let trimmed = trim_to_char_boundary(val, max_bytes);
        truncated = true;
        obj.insert(
            "value".to_string(),
            json!(format!(
                "{trimmed}\n\n_(truncated — {original_bytes} bytes total, set verbose=true for full content)_"
            )),
        );
    } else {
        return value;
    }

    if truncated {
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "_truncated".to_string(),
                json!({
                    "original_bytes": original_bytes,
                    "shown_bytes": max_bytes,
                    "hint": "set verbose=true for full content"
                }),
            );
        }
    }
    value
}

/// Trim a string to at most `max_bytes`, snapping to the nearest UTF-8
/// character boundary so the result stays valid UTF-8.
fn trim_to_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Reshape a completion response. LSP returns either `CompletionItem[]` or
/// `CompletionList { isIncomplete, items }`. Output is always
/// `{ items, isIncomplete, total, returned, _truncated? }`. Preserves null.
pub fn truncate_completion(value: Value, limit: usize, verbose: bool) -> Value {
    if value.is_null() {
        return value;
    }

    let (items, is_incomplete) = match value {
        Value::Array(a) => (a, false),
        Value::Object(mut o) => {
            let is_incomplete = o
                .get("isIncomplete")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let items = match o.remove("items") {
                Some(Value::Array(a)) => a,
                _ => return Value::Object(o),
            };
            (items, is_incomplete)
        }
        other => return other,
    };

    let mut items = items;
    items.sort_by(|a, b| {
        let key_a = completion_sort_key(a);
        let key_b = completion_sort_key(b);
        key_a.cmp(&key_b)
    });

    let total = items.len();
    let take = if verbose { total } else { limit.min(total) };
    let truncated = take < total;
    items.truncate(take);

    let mut out = json!({
        "items": items,
        "isIncomplete": is_incomplete,
        "total": total,
        "returned": take,
    });
    if truncated {
        out.as_object_mut().unwrap().insert(
            "_truncated".to_string(),
            json!({
                "hidden": total - take,
                "hint": "set verbose=true for all items"
            }),
        );
    }
    out
}

fn completion_sort_key(item: &Value) -> String {
    let sort = item
        .get("sortText")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let label = item
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    sort.unwrap_or_else(|| label.clone()) + "\u{1f}" + &label
}

/// Paginate a workspace_symbol response. LSP returns `SymbolInformation[]` or
/// `WorkspaceSymbol[]` or null. Output: `{ symbols, total, returned, next_cursor? }`.
pub fn paginate_workspace_symbol(
    value: Value,
    cursor: usize,
    limit: usize,
    verbose: bool,
) -> Value {
    if value.is_null() {
        return value;
    }
    let Value::Array(items) = value else {
        return value;
    };

    let total = items.len();
    let start = cursor.min(total);
    let take = if verbose { total - start } else { limit };
    let end = (start + take).min(total);
    let returned = end - start;
    let page: Vec<Value> = items.into_iter().skip(start).take(returned).collect();

    let mut out = json!({
        "symbols": page,
        "total": total,
        "returned": returned,
    });
    if end < total {
        out.as_object_mut()
            .unwrap()
            .insert("next_cursor".to_string(), json!(end.to_string()));
    }
    out
}

/// Paginate a formatted workspace_diagnostics response. The handler has
/// already shaped this into `{ workspace, files: { uri: ... }, summary }`.
/// Pagination slices `files` by sorted-URI order; `summary` totals stay
/// over the *whole* workspace so the LLM still knows how many issues exist.
pub fn paginate_workspace_diagnostics(
    mut value: Value,
    cursor: usize,
    limit: usize,
    verbose: bool,
) -> Value {
    let Some(obj) = value.as_object_mut() else {
        return value;
    };
    let Some(files) = obj.get_mut("files").and_then(|v| v.as_object_mut()) else {
        return value;
    };

    let mut keys: Vec<String> = files.keys().cloned().collect();
    keys.sort();
    let total = keys.len();
    let start = cursor.min(total);
    let take = if verbose { total - start } else { limit };
    let end = (start + take).min(total);
    let returned = end - start;

    if returned < total {
        let kept: std::collections::HashSet<&str> =
            keys[start..end].iter().map(|s| s.as_str()).collect();
        files.retain(|k, _| kept.contains(k.as_str()));
    }

    let mut pagination = json!({
        "total_files": total,
        "returned_files": returned,
    });
    if end < total {
        pagination
            .as_object_mut()
            .unwrap()
            .insert("next_cursor".to_string(), json!(end.to_string()));
    }
    obj.insert("pagination".to_string(), pagination);

    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_short_string_unchanged() {
        let v = json!({ "contents": { "kind": "markdown", "value": "hello" } });
        let out = truncate_hover(v.clone(), 100, false);
        assert_eq!(out, v);
    }

    #[test]
    fn hover_long_markdown_truncated() {
        let big = "x".repeat(10_000);
        let v = json!({ "contents": { "kind": "markdown", "value": big } });
        let out = truncate_hover(v, 1000, false);
        let value = out["contents"]["value"].as_str().unwrap();
        assert!(value.len() < 10_000);
        assert!(value.contains("(truncated"));
        assert!(out.get("_truncated").is_some());
    }

    #[test]
    fn hover_verbose_skips_truncation() {
        let big = "x".repeat(10_000);
        let v = json!({ "contents": { "kind": "markdown", "value": big.clone() } });
        let out = truncate_hover(v, 1000, true);
        assert_eq!(out["contents"]["value"].as_str().unwrap(), big);
        assert!(out.get("_truncated").is_none());
    }

    #[test]
    fn hover_null_passthrough() {
        let out = truncate_hover(json!(null), 1000, false);
        assert!(out.is_null());
    }

    #[test]
    fn hover_truncate_at_char_boundary() {
        // multi-byte chars near the boundary
        let s = "ä".repeat(2000); // 4000 bytes
        let v = json!({ "contents": { "kind": "markdown", "value": s } });
        let out = truncate_hover(v, 1001, false);
        // Should not panic and must be valid UTF-8 (it always is in serde_json)
        let value = out["contents"]["value"].as_str().unwrap();
        assert!(value.contains("(truncated"));
    }

    #[test]
    fn completion_array_form_truncated() {
        let items: Vec<Value> = (0..100)
            .map(|i| json!({ "label": format!("item{i:03}"), "sortText": format!("{i:03}") }))
            .collect();
        let out = truncate_completion(json!(items), 10, false);
        assert_eq!(out["returned"], 10);
        assert_eq!(out["total"], 100);
        assert_eq!(out["items"].as_array().unwrap().len(), 10);
        assert_eq!(out["_truncated"]["hidden"], 90);
    }

    #[test]
    fn completion_sorted_by_sort_text() {
        let items = vec![
            json!({ "label": "z", "sortText": "1" }),
            json!({ "label": "a", "sortText": "0" }),
        ];
        let out = truncate_completion(json!(items), 10, false);
        assert_eq!(out["items"][0]["label"], "a");
    }

    #[test]
    fn completion_list_form_preserves_is_incomplete() {
        let v = json!({
            "isIncomplete": true,
            "items": [{ "label": "foo" }]
        });
        let out = truncate_completion(v, 10, false);
        assert_eq!(out["isIncomplete"], true);
        assert_eq!(out["total"], 1);
    }

    #[test]
    fn completion_verbose_keeps_all() {
        let items: Vec<Value> = (0..100)
            .map(|i| json!({ "label": format!("i{i}") }))
            .collect();
        let out = truncate_completion(json!(items), 10, true);
        assert_eq!(out["returned"], 100);
        assert!(out.get("_truncated").is_none());
    }

    #[test]
    fn completion_null_passthrough() {
        let out = truncate_completion(json!(null), 10, false);
        assert!(out.is_null());
    }

    #[test]
    fn workspace_symbol_paginates() {
        let items: Vec<Value> = (0..150)
            .map(|i| json!({ "name": format!("sym{i}") }))
            .collect();
        let out = paginate_workspace_symbol(json!(items.clone()), 0, 100, false);
        assert_eq!(out["total"], 150);
        assert_eq!(out["returned"], 100);
        assert_eq!(out["next_cursor"], "100");

        let out2 = paginate_workspace_symbol(json!(items), 100, 100, false);
        assert_eq!(out2["returned"], 50);
        assert!(out2.get("next_cursor").is_none());
    }

    #[test]
    fn workspace_symbol_stale_cursor_returns_empty() {
        let items: Vec<Value> = (0..10)
            .map(|i| json!({ "name": format!("s{i}") }))
            .collect();
        let out = paginate_workspace_symbol(json!(items), 999, 100, false);
        assert_eq!(out["returned"], 0);
        assert_eq!(out["symbols"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn workspace_symbol_verbose_returns_all() {
        let items: Vec<Value> = (0..200)
            .map(|i| json!({ "name": format!("s{i}") }))
            .collect();
        let out = paginate_workspace_symbol(json!(items), 0, 50, true);
        assert_eq!(out["returned"], 200);
        assert!(out.get("next_cursor").is_none());
    }

    #[test]
    fn workspace_symbol_null_passthrough() {
        let out = paginate_workspace_symbol(json!(null), 0, 100, false);
        assert!(out.is_null());
    }

    #[test]
    fn workspace_diagnostics_paginates_files() {
        let mut files = serde_json::Map::new();
        for i in 0..10 {
            files.insert(format!("file:///f{i:02}.rs"), json!({ "diagnostics": [] }));
        }
        let v = json!({
            "workspace": "/tmp",
            "files": Value::Object(files),
            "summary": { "total_errors": 0 }
        });
        let out = paginate_workspace_diagnostics(v, 0, 3, false);
        assert_eq!(out["pagination"]["total_files"], 10);
        assert_eq!(out["pagination"]["returned_files"], 3);
        assert_eq!(out["pagination"]["next_cursor"], "3");
        assert_eq!(out["files"].as_object().unwrap().len(), 3);
        // Summary stays for full workspace.
        assert!(out["summary"].is_object());
    }

    #[test]
    fn resolve_limit_explicit_wins() {
        assert_eq!(resolve_limit(Some(5), false, 100), 5);
        assert_eq!(resolve_limit(Some(5), true, 100), 5);
    }

    #[test]
    fn resolve_limit_verbose_default() {
        assert_eq!(resolve_limit(None, true, 100), ABSOLUTE_LIMIT_CAP);
    }

    #[test]
    fn resolve_limit_clamped() {
        assert_eq!(resolve_limit(Some(0), false, 100), 1);
        assert_eq!(resolve_limit(Some(100_000), false, 100), ABSOLUTE_LIMIT_CAP);
    }

    #[test]
    fn parse_cursor_handles_garbage() {
        assert_eq!(parse_cursor(None), 0);
        assert_eq!(parse_cursor(Some("not-a-number")), 0);
        assert_eq!(parse_cursor(Some("42")), 42);
    }
}
