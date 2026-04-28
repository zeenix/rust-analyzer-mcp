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
pub const CRATES_URI: &str = "workspace://crates";
pub const CRATE_MANIFEST_PREFIX: &str = "workspace://crate/";
pub const CRATE_MANIFEST_SUFFIX: &str = "/Cargo.toml";
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

pub fn list_resources(_workspace_root: &Path, fetch_metadata: &dyn Fn() -> Option<Value>) -> Value {
    let mut resources = vec![json!({
        "uri": FILES_URI,
        "name": "Workspace files",
        "description": "Recursive file tree of the workspace root. Skips target/, .git/, node_modules/ and similar build/VCS dirs. Capped at 5000 entries and 16 levels deep; symlinks are reported but not followed.",
        "mimeType": "application/json",
    })];

    if let Some(metadata) = fetch_metadata() {
        resources.push(json!({
            "uri": CRATES_URI,
            "name": "Workspace crates",
            "description": "Summary of crates in this Cargo workspace (name, version, manifest path, targets, declared dependencies). Sourced from `cargo metadata --no-deps`.",
            "mimeType": "application/json",
        }));

        if let Some(packages) = metadata["packages"].as_array() {
            for pkg in packages {
                let Some(name) = pkg["name"].as_str() else {
                    continue;
                };
                resources.push(json!({
                    "uri": format!("{CRATE_MANIFEST_PREFIX}{name}{CRATE_MANIFEST_SUFFIX}"),
                    "name": format!("{name} Cargo.toml"),
                    "description": format!("Cargo manifest for the `{name}` crate."),
                    "mimeType": "application/toml",
                }));
            }
        }
    }

    json!({ "resources": resources })
}

pub fn read_resource(
    workspace_root: &Path,
    uri: &str,
    fetch_metadata: &dyn Fn() -> Option<Value>,
) -> Result<Value> {
    if uri == FILES_URI {
        let tree = build_file_tree(workspace_root);
        return Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&tree)?,
            }]
        }));
    }

    if uri == CRATES_URI {
        let metadata = fetch_metadata().ok_or_else(|| {
            anyhow!("cargo metadata failed (workspace root is not a Cargo workspace?)")
        })?;
        let summary = reshape_metadata(&metadata);
        return Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&summary)?,
            }]
        }));
    }

    if let Some(name) = uri
        .strip_prefix(CRATE_MANIFEST_PREFIX)
        .and_then(|rest| rest.strip_suffix(CRATE_MANIFEST_SUFFIX))
    {
        // Look up the crate's manifest path via cargo metadata so we never
        // dereference a caller-supplied path component — guards against
        // path-traversal via crafted crate names.
        let metadata = fetch_metadata().ok_or_else(|| anyhow!("cargo metadata failed"))?;
        let manifest_path = find_manifest_for_crate(&metadata, name)
            .ok_or_else(|| anyhow!("Unknown crate: {name}"))?;
        let content = std::fs::read_to_string(&manifest_path)
            .map_err(|e| anyhow!("Failed to read {}: {e}", manifest_path))?;
        return Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/toml",
                "text": content,
            }]
        }));
    }

    Err(anyhow!("Unknown resource: {uri}"))
}

/// Collapse the full `cargo metadata` output into a smaller summary suited
/// to LLM consumption: per-package name/version/manifest, target list, and
/// declared dependency names. Drops registry metadata, license fields,
/// resolution graph, etc.
fn reshape_metadata(metadata: &Value) -> Value {
    let workspace_root = metadata["workspace_root"].as_str().unwrap_or("");
    let workspace_members: Vec<&str> = metadata["workspace_members"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let packages: Vec<Value> = metadata["packages"]
        .as_array()
        .map(|pkgs| {
            pkgs.iter()
                .filter_map(|p| {
                    let name = p["name"].as_str()?;
                    let version = p["version"].as_str()?;
                    let manifest_path = p["manifest_path"].as_str()?;
                    let id = p["id"].as_str().unwrap_or("");
                    let is_workspace_member = workspace_members.contains(&id);

                    let targets: Vec<Value> = p["targets"]
                        .as_array()
                        .map(|ts| {
                            ts.iter()
                                .filter_map(|t| {
                                    Some(json!({
                                        "name": t["name"].as_str()?,
                                        "kind": t["kind"].clone(),
                                        "src_path": t["src_path"].as_str()?,
                                    }))
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    let dependencies: Vec<&str> = p["dependencies"]
                        .as_array()
                        .map(|ds| ds.iter().filter_map(|d| d["name"].as_str()).collect())
                        .unwrap_or_default();

                    Some(json!({
                        "name": name,
                        "version": version,
                        "manifest_path": manifest_path,
                        "is_workspace_member": is_workspace_member,
                        "targets": targets,
                        "dependencies": dependencies,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "workspace_root": workspace_root,
        "packages": packages,
    })
}

fn find_manifest_for_crate(metadata: &Value, name: &str) -> Option<String> {
    metadata["packages"].as_array()?.iter().find_map(|p| {
        if p["name"].as_str()? == name {
            p["manifest_path"].as_str().map(String::from)
        } else {
            None
        }
    })
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
    use crate::mcp::workspace::run_cargo_metadata;
    use std::fs;
    use tempfile::TempDir;

    /// Test helper: real `cargo metadata` provider, no caching.
    fn fetch(root: &Path) -> impl Fn() -> Option<Value> + '_ {
        move || run_cargo_metadata(root)
    }

    #[test]
    fn list_advertises_files_resource_in_non_cargo_dir() {
        // A plain temp dir is not a Cargo workspace, so cargo metadata fails
        // and only the static file-tree resource is advertised.
        let dir = TempDir::new().unwrap();
        let v = list_resources(dir.path(), &fetch(dir.path()));
        let arr = v["resources"].as_array().unwrap();
        let uris: Vec<&str> = arr.iter().filter_map(|r| r["uri"].as_str()).collect();
        assert!(uris.contains(&FILES_URI));
        assert!(!uris.contains(&CRATES_URI));
    }

    #[test]
    fn list_includes_crates_for_cargo_workspace() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "// empty\n").unwrap();

        let v = list_resources(dir.path(), &fetch(dir.path()));
        let arr = v["resources"].as_array().unwrap();
        let uris: Vec<&str> = arr.iter().filter_map(|r| r["uri"].as_str()).collect();
        assert!(uris.contains(&FILES_URI));
        assert!(uris.contains(&CRATES_URI));
        assert!(uris.iter().any(|u| u.contains("/crate/demo/Cargo.toml")));
    }

    #[test]
    fn read_crates_returns_workspace_summary() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "// empty\n").unwrap();

        let v = read_resource(dir.path(), CRATES_URI, &fetch(dir.path())).unwrap();
        let text = v["contents"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(text).unwrap();
        let pkgs = body["packages"].as_array().unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0]["name"], "demo");
        assert_eq!(pkgs[0]["is_workspace_member"], true);
        assert!(pkgs[0]["manifest_path"]
            .as_str()
            .unwrap()
            .ends_with("Cargo.toml"));
    }

    #[test]
    fn read_crate_manifest_returns_toml() {
        let dir = TempDir::new().unwrap();
        let manifest = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
        fs::write(dir.path().join("Cargo.toml"), manifest).unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "// empty\n").unwrap();

        let v = read_resource(
            dir.path(),
            "workspace://crate/demo/Cargo.toml",
            &fetch(dir.path()),
        )
        .unwrap();
        assert_eq!(v["contents"][0]["mimeType"], "application/toml");
        assert_eq!(v["contents"][0]["text"], manifest);
    }

    #[test]
    fn read_crate_manifest_unknown_name_errors() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "// empty\n").unwrap();

        let err = read_resource(
            dir.path(),
            "workspace://crate/nonexistent/Cargo.toml",
            &fetch(dir.path()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("Unknown crate"));
    }

    #[test]
    fn read_crate_manifest_path_traversal_blocked() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "// empty\n").unwrap();

        // Crafted name with ../ — must NOT escape the workspace; it should
        // simply fail to match any package.
        let err = read_resource(
            dir.path(),
            "workspace://crate/..%2F..%2Fetc%2Fpasswd/Cargo.toml",
            &fetch(dir.path()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("Unknown crate"));
    }

    #[test]
    fn read_unknown_uri_errors() {
        let dir = TempDir::new().unwrap();
        let err = read_resource(dir.path(), "workspace://nope", &fetch(dir.path())).unwrap_err();
        assert!(err.to_string().contains("Unknown resource"));
    }

    #[test]
    fn file_tree_basic_shape() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "fn x() {}").unwrap();

        let v = read_resource(dir.path(), FILES_URI, &fetch(dir.path())).unwrap();
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

        let v = read_resource(dir.path(), FILES_URI, &fetch(dir.path())).unwrap();
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

        let v = read_resource(dir.path(), FILES_URI, &fetch(dir.path())).unwrap();
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
        let root = Path::new("/this/does/not/exist/xyz123");
        let v = read_resource(root, FILES_URI, &fetch(root)).unwrap();
        let text = v["contents"][0]["text"].as_str().unwrap();
        let tree: Value = serde_json::from_str(text).unwrap();
        assert_eq!(tree["tree"]["type"], "unknown");
    }
}
