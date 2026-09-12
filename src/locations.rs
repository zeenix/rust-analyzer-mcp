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
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use crate::uri;

/// Where a symbol is declared, from a `textDocument/definition` answer: a `Location`, a list of
/// them, or a list of `LocationLink`s, all of which rust-analyzer may send.
///
/// A declaration is matched by file and line rather than by column, because the two requests
/// describe the same identifier from different ends -- a `LocationLink` reports the whole item
/// where a reference reports the name.
fn declared_at(definition: &Value) -> HashSet<(String, u64)> {
    let one = |item: &Value| -> Option<(String, u64)> {
        let uri = item
            .get("uri")
            .or_else(|| item.get("targetUri"))
            .and_then(Value::as_str)?;
        let line = item
            .pointer("/range/start/line")
            .or_else(|| item.pointer("/targetSelectionRange/start/line"))
            .or_else(|| item.pointer("/targetRange/start/line"))
            .and_then(Value::as_u64)?;
        Some((uri.to_string(), line))
    };

    match definition.as_array() {
        Some(items) => items.iter().filter_map(one).collect(),
        None => one(definition).into_iter().collect(),
    }
}

/// `locations` with each hit named by path and position, carrying the line of source it points
/// at. Anything that is not a list of locations is passed through untouched.
///
/// `definition` is the same symbol's declaration, which LSP includes among its references: the
/// hit it matches is marked, so that a `count` of three against two call sites is readable
/// rather than one off.
pub fn annotate(locations: &Value, workspace_root: &Path, definition: &Value) -> Value {
    let Some(items) = locations.as_array() else {
        return locations.clone();
    };

    // A symbol's references cluster in a handful of files, so the file each hit is in is usually
    // one already read.
    let mut sources: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut annotated = Vec::with_capacity(items.len());
    let declarations = declared_at(definition);

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

        let mut hit = json!({
            "file": shown,
            "line": line,
            "character": character,
            // Same coordinates the tools take, so a hit can be asked about without arithmetic.
            "text": text,
        });

        // The declaration is one of the hits and is not a use of the symbol. Saying which one it
        // is costs a field on a single entry; leaving it unsaid costs every reader an off-by-one.
        if line.is_some_and(|line| declarations.contains(&(uri_str.to_string(), line))) {
            hit["declaration"] = json!(true);
        }

        annotated.push(hit);
    }

    json!({ "count": annotated.len(), "locations": annotated })
}
