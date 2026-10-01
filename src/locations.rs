//! Turning a list of LSP `Location`s into something a reader learns anything from.
//!
//! A `Location` is a URI and a range: nine lines of JSON, per hit, saying where to look and
//! nothing about what is there. A symbol with thirty references costs a few hundred lines that
//! have to be followed one file open at a time, and the answer to "is any of these the one I
//! mean" is in none of them.
//!
//! The line of source at each hit is what makes the list readable, and it is smaller than the
//! JSON it replaces.

use serde_json::{json, Value};
use std::{collections::HashMap, path::Path};

use crate::uri;

/// `locations` with each hit named by path and position, carrying the line of source it points
/// at. Anything that is not a list of locations is passed through untouched.
pub fn annotate(locations: &Value, workspace_root: &Path) -> Value {
    let Some(items) = locations.as_array() else {
        return locations.clone();
    };

    // A symbol's references cluster in a handful of files, so the file each hit is in is usually
    // one already read.
    let mut sources: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut annotated = Vec::with_capacity(items.len());

    for item in items {
        let Some(uri_str) = item.get("uri").and_then(Value::as_str) else {
            annotated.push(item.clone());
            continue;
        };
        let Some(path) = uri::uri_to_path(uri_str) else {
            annotated.push(item.clone());
            continue;
        };

        let start = item.pointer("/range/start");
        let line = start.and_then(|s| s.get("line")).and_then(Value::as_u64);
        let character = start
            .and_then(|s| s.get("character"))
            .and_then(Value::as_u64);

        let text = line.and_then(|line| {
            sources
                .entry(uri_str.to_string())
                .or_insert_with(|| {
                    std::fs::read_to_string(&path)
                        .ok()
                        .map(|body| body.lines().map(str::to_string).collect())
                })
                .as_ref()
                .and_then(|lines| lines.get(line as usize))
                .map(|text| text.trim_end().to_string())
        });

        let shown = path
            .strip_prefix(workspace_root)
            .unwrap_or(&path)
            .display()
            .to_string();

        annotated.push(json!({
            "file": shown,
            "line": line,
            "character": character,
            // Same coordinates the tools take, so a hit can be asked about without arithmetic.
            "text": text,
        }));
    }

    json!({ "count": annotated.len(), "locations": annotated })
}
