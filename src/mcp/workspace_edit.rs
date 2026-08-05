//! Server-side application of LSP `WorkspaceEdit`s.
//!
//! Used by `move_file` to materialize the import/mod-decl fixes that
//! `workspace/willRenameFiles` returns. Reads each affected file, applies its
//! `TextEdit`s in reverse start-position order (so earlier edits stay
//! position-stable), and writes the file back.
//!
//! Two safety constraints:
//! - Every edit URI must resolve to a path inside `workspace_root` — guards against an upstream
//!   that returns edits for files outside the project.
//! - Position offsets are computed assuming UTF-8 byte offsets within the line text. LSP's default
//!   position encoding is UTF-16, but for the ASCII-identifier `mod`/`use` edits that
//!   `willRenameFiles` produces this is indistinguishable in practice; the
//!   `apply_workspace_edit_str` unit tests cover the byte-offset path directly.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, serde::Serialize)]
pub(super) struct ApplyReport {
    /// Per-file summary entries, in the order they were applied.
    pub files: Vec<FileApplyEntry>,
    pub total_edits: usize,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct FileApplyEntry {
    pub uri: String,
    pub edits_applied: usize,
}

/// Apply a `WorkspaceEdit` value (as returned by an LSP server) on disk. Edits
/// are taken from `documentChanges` (preferred) or `changes` (fallback). Other
/// `documentChanges` kinds (`CreateFile`/`RenameFile`/`DeleteFile`) are
/// ignored — `move_file` does the physical rename itself.
pub(super) fn apply_workspace_edit(edit: &Value, workspace_root: &Path) -> Result<ApplyReport> {
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

    let ops = collect_edits(edit);
    let mut report = ApplyReport::default();

    for op in ops {
        let path = uri_to_path(&op.uri)
            .ok_or_else(|| anyhow!("WorkspaceEdit URI is not a file:// URI: {}", op.uri))?;
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !canonical_path.starts_with(&canonical_root) {
            return Err(anyhow!(
                "Refusing to apply edit outside workspace: {}",
                op.uri
            ));
        }

        let original = std::fs::read_to_string(&canonical_path)
            .map_err(|e| anyhow!("Failed to read {}: {}", canonical_path.display(), e))?;
        let updated = apply_workspace_edit_str(&original, &op.edits)?;
        if updated != original {
            std::fs::write(&canonical_path, &updated)
                .map_err(|e| anyhow!("Failed to write {}: {}", canonical_path.display(), e))?;
        }
        report.total_edits += op.edits.len();
        report.files.push(FileApplyEntry {
            uri: op.uri,
            edits_applied: op.edits.len(),
        });
    }
    Ok(report)
}

/// Apply a list of TextEdit JSON values to the given source string. Edits are
/// sorted by start position descending so applying one doesn't shift the
/// offsets of any not-yet-applied edits.
pub(super) fn apply_workspace_edit_str(source: &str, edits: &[Value]) -> Result<String> {
    let mut typed = Vec::with_capacity(edits.len());
    for e in edits {
        typed.push(parse_text_edit(e)?);
    }
    typed.sort_by(|a, b| {
        b.start_line
            .cmp(&a.start_line)
            .then_with(|| b.start_char.cmp(&a.start_char))
    });

    let mut buf = source.to_string();
    for edit in typed {
        let start = position_to_byte(&buf, edit.start_line, edit.start_char).ok_or_else(|| {
            anyhow!(
                "Edit start position {}:{} out of range",
                edit.start_line,
                edit.start_char
            )
        })?;
        let end = position_to_byte(&buf, edit.end_line, edit.end_char).ok_or_else(|| {
            anyhow!(
                "Edit end position {}:{} out of range",
                edit.end_line,
                edit.end_char
            )
        })?;
        if end < start {
            return Err(anyhow!("Inverted TextEdit range"));
        }
        buf.replace_range(start..end, &edit.new_text);
    }
    Ok(buf)
}

struct TextEdit {
    start_line: u32,
    start_char: u32,
    end_line: u32,
    end_char: u32,
    new_text: String,
}

fn parse_text_edit(v: &Value) -> Result<TextEdit> {
    let range = v
        .get("range")
        .ok_or_else(|| anyhow!("TextEdit missing range"))?;
    let start = range
        .get("start")
        .ok_or_else(|| anyhow!("range missing start"))?;
    let end = range
        .get("end")
        .ok_or_else(|| anyhow!("range missing end"))?;
    Ok(TextEdit {
        start_line: pos_field(start, "line")?,
        start_char: pos_field(start, "character")?,
        end_line: pos_field(end, "line")?,
        end_char: pos_field(end, "character")?,
        new_text: v
            .get("newText")
            .and_then(|s| s.as_str())
            .ok_or_else(|| anyhow!("TextEdit missing newText"))?
            .to_string(),
    })
}

fn pos_field(p: &Value, key: &str) -> Result<u32> {
    p.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .ok_or_else(|| anyhow!("Position missing {}", key))
}

struct EditOp {
    uri: String,
    edits: Vec<Value>,
}

fn collect_edits(edit: &Value) -> Vec<EditOp> {
    if let Some(doc_changes) = edit.get("documentChanges").and_then(|v| v.as_array()) {
        let mut ops = Vec::new();
        for change in doc_changes {
            if change.get("kind").is_some() {
                // CreateFile / RenameFile / DeleteFile — not our concern.
                continue;
            }
            let Some(uri) = change
                .get("textDocument")
                .and_then(|td| td.get("uri"))
                .and_then(|u| u.as_str())
            else {
                continue;
            };
            let Some(arr) = change.get("edits").and_then(|v| v.as_array()) else {
                continue;
            };
            ops.push(EditOp {
                uri: uri.to_string(),
                edits: arr.clone(),
            });
        }
        return ops;
    }

    if let Some(changes) = edit.get("changes").and_then(|v| v.as_object()) {
        return changes
            .iter()
            .filter_map(|(uri, edits)| {
                edits.as_array().map(|arr| EditOp {
                    uri: uri.clone(),
                    edits: arr.clone(),
                })
            })
            .collect();
    }

    Vec::new()
}

/// LSP `Position { line, character }` → byte offset into `text`. `character` is
/// interpreted as a count of `chars` within the line; this matches UTF-8
/// byte-offset positions for the ASCII edits we expect from
/// `willRenameFiles`. Returns `None` if the position points past EOF.
fn position_to_byte(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut byte_offset = 0usize;
    let mut current_line = 0u32;
    for line_text in text.split_inclusive('\n') {
        if current_line == line {
            let trimmed = line_text.strip_suffix('\n').unwrap_or(line_text);
            let char_byte = trimmed
                .char_indices()
                .nth(character as usize)
                .map(|(b, _)| b)
                .unwrap_or(trimmed.len());
            return Some(byte_offset + char_byte);
        }
        byte_offset += line_text.len();
        current_line += 1;
    }
    // Position at EOF (one past last line, character 0) is valid.
    if current_line == line && character == 0 {
        return Some(text.len());
    }
    None
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let stripped = uri.strip_prefix("file://")?;
    Some(PathBuf::from(stripped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn position_to_byte_simple() {
        let s = "abc\ndef\nghi";
        assert_eq!(position_to_byte(s, 0, 0), Some(0));
        assert_eq!(position_to_byte(s, 0, 3), Some(3));
        assert_eq!(position_to_byte(s, 1, 0), Some(4));
        assert_eq!(position_to_byte(s, 1, 2), Some(6));
        assert_eq!(position_to_byte(s, 2, 3), Some(11));
        // EOF position
        assert_eq!(position_to_byte(s, 3, 0), Some(11));
    }

    #[test]
    fn apply_str_single_edit() {
        let src = "mod foo;\nfn main() {}\n";
        let edits = vec![json!({
            "range": {
                "start": { "line": 0, "character": 4 },
                "end": { "line": 0, "character": 7 }
            },
            "newText": "bar"
        })];
        let out = apply_workspace_edit_str(src, &edits).unwrap();
        assert_eq!(out, "mod bar;\nfn main() {}\n");
    }

    #[test]
    fn apply_str_multiple_edits_reverse_order() {
        // Two edits in the same line — applied in reverse order so the second
        // doesn't shift the first.
        let src = "use crate::a; use crate::b;\n";
        let edits = vec![
            json!({
                "range": {
                    "start": { "line": 0, "character": 11 },
                    "end": { "line": 0, "character": 12 }
                },
                "newText": "AA"
            }),
            json!({
                "range": {
                    "start": { "line": 0, "character": 25 },
                    "end": { "line": 0, "character": 26 }
                },
                "newText": "BB"
            }),
        ];
        let out = apply_workspace_edit_str(src, &edits).unwrap();
        assert_eq!(out, "use crate::AA; use crate::BB;\n");
    }

    #[test]
    fn collect_edits_prefers_document_changes() {
        let edit = json!({
            "documentChanges": [{
                "textDocument": { "uri": "file:///x.rs", "version": null },
                "edits": [{
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 0 }
                    },
                    "newText": "// new\n"
                }]
            }],
            "changes": {
                "file:///stale.rs": []
            }
        });
        let ops = collect_edits(&edit);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].uri, "file:///x.rs");
        assert_eq!(ops[0].edits.len(), 1);
    }

    #[test]
    fn collect_edits_skips_create_rename_delete() {
        let edit = json!({
            "documentChanges": [
                { "kind": "create", "uri": "file:///new.rs" },
                {
                    "textDocument": { "uri": "file:///x.rs" },
                    "edits": []
                },
                { "kind": "rename", "oldUri": "file:///a.rs", "newUri": "file:///b.rs" }
            ]
        });
        let ops = collect_edits(&edit);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].uri, "file:///x.rs");
    }

    #[test]
    fn apply_writes_file_inside_workspace() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("foo.rs");
        fs::write(&target, "mod foo;\n").unwrap();

        let edit = json!({
            "documentChanges": [{
                "textDocument": { "uri": format!("file://{}", target.display()) },
                "edits": [{
                    "range": {
                        "start": { "line": 0, "character": 4 },
                        "end": { "line": 0, "character": 7 }
                    },
                    "newText": "bar"
                }]
            }]
        });
        let report = apply_workspace_edit(&edit, dir.path()).unwrap();
        assert_eq!(report.total_edits, 1);
        assert_eq!(report.files.len(), 1);
        assert_eq!(fs::read_to_string(&target).unwrap(), "mod bar;\n");
    }

    #[test]
    fn apply_rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("evil.rs");
        fs::write(&target, "x").unwrap();

        let edit = json!({
            "documentChanges": [{
                "textDocument": { "uri": format!("file://{}", target.display()) },
                "edits": [{
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 1 }
                    },
                    "newText": "Y"
                }]
            }]
        });
        let err = apply_workspace_edit(&edit, dir.path()).unwrap_err();
        assert!(err.to_string().contains("outside workspace"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "x");
    }
}
