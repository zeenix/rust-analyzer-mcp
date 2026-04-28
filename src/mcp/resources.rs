//! MCP resources — read-only views of the workspace.
//!
//! Step 1: `workspace://files` — recursive file tree of the workspace root.
//! Skips common build/VCS dirs, caps at `FILE_TREE_MAX_ENTRIES` entries and
//! `FILE_TREE_MAX_DEPTH` levels. Symlinks are reported but not followed (loop
//! guard). All filesystem IO is synchronous; callers should run this from a
//! `spawn_blocking` task.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::Path;

pub const FILES_URI: &str = "workspace://files";
pub const FILE_TREE_MAX_ENTRIES: usize = 5000;
pub const FILE_TREE_MAX_DEPTH: usize = 16;

const IGNORED_DIRS: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    ".idea",
    ".vscode",
    "dist",
    "build",
    ".direnv",
];

pub fn list_resources() -> Value {
    json!({
        "resources": [
            {
                "uri": FILES_URI,
                "name": "Workspace files",
                "description": "Recursive file tree of the workspace root. Skips target/, .git/, node_modules/ and similar build/VCS dirs. Capped at 5000 entries and 16 levels deep; symlinks are reported but not followed.",
                "mimeType": "application/json",
            }
        ]
    })
}

pub fn read_resource(workspace_root: &Path, uri: &str) -> Result<Value> {
    match uri {
        FILES_URI => {
            let tree = build_file_tree(workspace_root);
            Ok(json!({
                "contents": [
                    {
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": serde_json::to_string_pretty(&tree)?,
                    }
                ]
            }))
        }
        _ => Err(anyhow!("Unknown resource: {uri}")),
    }
}

fn build_file_tree(root: &Path) -> Value {
    let mut count = 0usize;
    let tree = walk(root, 0, &mut count);
    json!({
        "root": root.display().to_string(),
        "tree": tree,
        "stats": {
            "entries": count,
            "max_entries": FILE_TREE_MAX_ENTRIES,
            "max_depth": FILE_TREE_MAX_DEPTH,
            "truncated": count >= FILE_TREE_MAX_ENTRIES,
        }
    })
}

fn walk(path: &Path, depth: usize, count: &mut usize) -> Value {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let meta = match path.symlink_metadata() {
        Ok(m) => m,
        Err(_) => return json!({ "type": "unknown", "name": name }),
    };

    let ft = meta.file_type();

    if ft.is_symlink() {
        let target = std::fs::read_link(path)
            .ok()
            .map(|p| p.display().to_string());
        return json!({ "type": "symlink", "name": name, "target": target });
    }

    if ft.is_file() {
        *count += 1;
        return json!({ "type": "file", "name": name, "size": meta.len() });
    }

    if !ft.is_dir() {
        return json!({ "type": "other", "name": name });
    }

    *count += 1;

    if depth >= FILE_TREE_MAX_DEPTH {
        return json!({
            "type": "dir",
            "name": name,
            "truncated": true,
            "reason": "max-depth"
        });
    }

    let mut entries: Vec<_> = match std::fs::read_dir(path) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(e) => {
            return json!({
                "type": "dir",
                "name": name,
                "children": [],
                "error": format!("read_dir failed: {e}")
            });
        }
    };
    entries.sort_by_key(|e| e.file_name());

    let mut children = Vec::new();
    let mut truncated_reason: Option<&'static str> = None;

    for entry in entries {
        let entry_name = entry.file_name();
        let entry_name_str = entry_name.to_string_lossy();

        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir && IGNORED_DIRS.contains(&entry_name_str.as_ref()) {
            continue;
        }

        if *count >= FILE_TREE_MAX_ENTRIES {
            truncated_reason = Some("max-entries");
            break;
        }

        children.push(walk(&entry.path(), depth + 1, count));
    }

    let mut out = json!({ "type": "dir", "name": name, "children": children });
    if let Some(reason) = truncated_reason {
        let obj = out.as_object_mut().unwrap();
        obj.insert("truncated".to_string(), json!(true));
        obj.insert("reason".to_string(), json!(reason));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn list_advertises_files_resource() {
        let v = list_resources();
        let arr = v["resources"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["uri"], FILES_URI);
        assert_eq!(arr[0]["mimeType"], "application/json");
    }

    #[test]
    fn read_unknown_uri_errors() {
        let dir = TempDir::new().unwrap();
        let err = read_resource(dir.path(), "workspace://nope").unwrap_err();
        assert!(err.to_string().contains("Unknown resource"));
    }

    #[test]
    fn file_tree_basic_shape() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "fn x() {}").unwrap();

        let v = read_resource(dir.path(), FILES_URI).unwrap();
        let text = v["contents"][0]["text"].as_str().unwrap();
        let tree: Value = serde_json::from_str(text).unwrap();

        assert!(tree["root"].as_str().is_some());
        assert_eq!(tree["tree"]["type"], "dir");
        assert_eq!(tree["stats"]["truncated"], false);

        // Children sorted alphabetically: Cargo.toml first (uppercase), then src.
        let children = tree["tree"]["children"].as_array().unwrap();
        let names: Vec<&str> = children.iter().filter_map(|c| c["name"].as_str()).collect();
        assert!(names.contains(&"Cargo.toml"));
        assert!(names.contains(&"src"));

        // src is a dir with one file child.
        let src = children
            .iter()
            .find(|c| c["name"] == "src")
            .expect("src dir present");
        assert_eq!(src["type"], "dir");
        let src_children = src["children"].as_array().unwrap();
        assert_eq!(src_children.len(), 1);
        assert_eq!(src_children[0]["name"], "lib.rs");
        assert_eq!(src_children[0]["type"], "file");
        assert!(src_children[0]["size"].as_u64().unwrap() > 0);
    }

    #[test]
    fn ignored_dirs_are_skipped() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target/junk"), "x").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/HEAD"), "x").unwrap();
        fs::write(dir.path().join("keep.txt"), "x").unwrap();

        let v = read_resource(dir.path(), FILES_URI).unwrap();
        let text = v["contents"][0]["text"].as_str().unwrap();
        let tree: Value = serde_json::from_str(text).unwrap();

        let names: Vec<&str> = tree["tree"]["children"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        assert!(names.contains(&"keep.txt"));
        assert!(!names.contains(&"target"));
        assert!(!names.contains(&".git"));
    }

    #[test]
    fn max_depth_truncates_subtree() {
        let dir = TempDir::new().unwrap();
        // Build a deep path beyond FILE_TREE_MAX_DEPTH.
        let mut p = dir.path().to_path_buf();
        for i in 0..(FILE_TREE_MAX_DEPTH + 3) {
            p.push(format!("d{i}"));
            fs::create_dir(&p).unwrap();
        }
        fs::write(p.join("deep.txt"), "x").unwrap();

        let v = read_resource(dir.path(), FILES_URI).unwrap();
        let text = v["contents"][0]["text"].as_str().unwrap();
        let tree: Value = serde_json::from_str(text).unwrap();

        // Walk into the tree until we hit a "truncated" node.
        let mut node = &tree["tree"];
        let mut found_truncation = false;
        for _ in 0..(FILE_TREE_MAX_DEPTH + 5) {
            if node["truncated"].as_bool() == Some(true) && node["reason"] == "max-depth" {
                found_truncation = true;
                break;
            }
            let Some(children) = node["children"].as_array() else {
                break;
            };
            let Some(next) = children.first() else { break };
            node = next;
        }
        assert!(found_truncation, "expected a max-depth truncation node");
    }

    #[test]
    fn nonexistent_root_yields_unknown() {
        let v = read_resource(Path::new("/this/does/not/exist/xyz123"), FILES_URI).unwrap();
        let text = v["contents"][0]["text"].as_str().unwrap();
        let tree: Value = serde_json::from_str(text).unwrap();
        assert_eq!(tree["tree"]["type"], "unknown");
    }
}
