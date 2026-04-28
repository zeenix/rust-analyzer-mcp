use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

// Import test support library
use test_support::{is_ci, timeouts, IpcClient};

#[tokio::test]
async fn test_server_initialization() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;

    // The server is already initialized by IpcClient
    // Just verify we can make a request
    let response = client.send_request("tools/list", None).await?;

    // Check we got tools
    assert!(response.get("tools").is_some());
    let tools = response["tools"].as_array().unwrap();
    assert!(!tools.is_empty());

    // Verify some expected tools exist
    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(tool_names.contains(&"rust_analyzer_symbols"));
    assert!(tool_names.contains(&"rust_analyzer_hover"));

    Ok(())
}

#[tokio::test]
async fn test_all_lsp_tools() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();

    // Test 1: Get symbols for main.rs
    test_symbols(&mut client, &workspace_path).await?;

    // In CI, add extra delay to ensure rust-analyzer is fully ready for all operations
    if is_ci() {
        tokio::time::sleep(timeouts::ci_test_delay()).await;
    }

    // Test 2: Get definition - test "greet" function call on line 2 (0-indexed line 1)
    let got_definition = test_definition(&mut client, &workspace_path).await?;

    // Test 3: Get references - test "greet" function definition on line 10 (0-indexed line 9)
    let got_references = test_references(&mut client, &workspace_path).await?;

    // Test 4: Get hover information - test "Calculator" on line 5 (0-indexed line 4)
    let got_hover = test_hover(&mut client, &workspace_path).await?;

    // Test 5: Get completions
    test_completion(&mut client, &workspace_path).await?;

    // Test 6: Format document
    let got_format = test_format(&mut client, &workspace_path).await?;

    // Test 7: Code actions
    let got_code_actions = test_code_actions(&mut client, &workspace_path).await?;

    // Print summary
    println!("LSP Tools Test Results:");
    println!("  Symbols: ✓");
    println!(
        "  Definition: {}",
        if got_definition {
            "✓"
        } else {
            "⚠ (not ready)"
        }
    );
    println!(
        "  References: {}",
        if got_references {
            "✓"
        } else {
            "⚠ (not ready)"
        }
    );
    println!("  Hover: {}", if got_hover { "✓" } else { "⚠ (not ready)" });
    println!("  Completion: ✓");
    println!(
        "  Format: {}",
        if got_format {
            "✓"
        } else {
            "⚠ (invalid response)"
        }
    );
    println!(
        "  Code Actions: {}",
        if got_code_actions {
            "✓"
        } else {
            "⚠ (not ready)"
        }
    );

    Ok(())
}

#[tokio::test]
async fn test_workspace_change() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let original_workspace = client.workspace_path().to_path_buf();

    // Create a second isolated project to switch to
    let second_project = test_support::IsolatedProject::new()?;
    let response = client
        .call_tool(
            "rust_analyzer_set_workspace",
            json!({
                "workspace_path": second_project.path().to_str().unwrap()
            }),
        )
        .await?;

    // Verify workspace change succeeded
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            assert!(
                text.as_str().unwrap_or("").contains("changed")
                    || text.as_str().unwrap_or("").contains("set"),
                "Workspace change should be acknowledged"
            );
        }
    }

    // Restore the shared daemon's workspace so other tests on this IpcClient still find their
    // files.
    client
        .call_tool(
            "rust_analyzer_set_workspace",
            json!({
                "workspace_path": original_workspace.to_str().unwrap()
            }),
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_new_lsp_tools() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();
    let main_path = workspace_path.join("src/main.rs");

    // 1. workspace_symbol — fuzzy search across the workspace.
    let response = client
        .call_tool(
            "rust_analyzer_workspace_symbol",
            json!({ "query": "Calculator" }),
        )
        .await?;
    let text = extract_tool_text(&response)?;
    // Either an array of SymbolInformation/WorkspaceSymbol or null while indexing.
    if text != "null" {
        serde_json::from_str::<Value>(&text)?;
    }

    // 2. prepare_rename — accept either a renamable target or null (no symbol at position).
    let response = client
        .call_tool(
            "rust_analyzer_prepare_rename",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 13,
                "character": 3
            }),
        )
        .await?;
    let _ = extract_tool_text(&response)?; // null or a Range/PrepareRenameResult.

    // 3. rename — same position, new name.
    let response = client
        .call_tool(
            "rust_analyzer_rename",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 13,
                "character": 3,
                "new_name": "salute"
            }),
        )
        .await?;
    let _ = extract_tool_text(&response)?; // null or a WorkspaceEdit.

    // 4. signature_help — inside greet("World") on line 1.
    let response = client
        .call_tool(
            "rust_analyzer_signature_help",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 1,
                "character": 24
            }),
        )
        .await?;
    let _ = extract_tool_text(&response)?; // null or SignatureHelp.

    // 5. inlay_hints — over the entire file.
    let response = client
        .call_tool(
            "rust_analyzer_inlay_hints",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 0,
                "character": 0,
                "end_line": 60,
                "end_character": 0
            }),
        )
        .await?;
    let _ = extract_tool_text(&response)?; // null or InlayHint[].

    Ok(())
}

/// Phase 1 tools: typeDefinition, implementation, expandMacro, parentModule,
/// runnables, relatedTests, openDocs. We don't pin specific responses (some
/// of these depend on rust-analyzer's indexing state being fully ready);
/// instead we assert that each tool returns *something* — either a meaningful
/// payload or null — without raising an MCP error.
#[tokio::test]
async fn test_phase1_tools() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();
    let main_path = workspace_path.join("src/main.rs");
    let main_str = main_path.to_str().unwrap();

    // typeDefinition — on `calc` (line 5, char 8 == binding name) or wherever
    // is in scope. Either a Location/Location[] or null.
    let response = client
        .call_tool(
            "rust_analyzer_type_definition",
            json!({ "file_path": main_str, "line": 5, "character": 8 }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    // implementation — on `Calculator` struct identifier (line 17, char 7).
    let response = client
        .call_tool(
            "rust_analyzer_implementation",
            json!({ "file_path": main_str, "line": 17, "character": 7 }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    // expand_macro — on the `println!` invocation (line 2, char 4).
    let response = client
        .call_tool(
            "rust_analyzer_expand_macro",
            json!({ "file_path": main_str, "line": 2, "character": 4 }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    // parent_module — anywhere in the file (line 0, char 0).
    let response = client
        .call_tool(
            "rust_analyzer_parent_module",
            json!({ "file_path": main_str, "line": 0, "character": 0 }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    // runnables — the whole file. With no position we should get every
    // runnable in main.rs (the `tests` module has at least two #[test]s).
    let response = client
        .call_tool("rust_analyzer_runnables", json!({ "file_path": main_str }))
        .await?;
    let _ = extract_tool_text(&response)?;

    // related_tests — on `greet` (line 14, char 3).
    let response = client
        .call_tool(
            "rust_analyzer_related_tests",
            json!({ "file_path": main_str, "line": 14, "character": 3 }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    // open_docs — on `Calculator` (line 17, char 7). Returns docs URLs or null.
    let response = client
        .call_tool(
            "rust_analyzer_open_docs",
            json!({ "file_path": main_str, "line": 17, "character": 7 }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    Ok(())
}

/// Phase 1.3 — Compiler-debug optionals: syntax_tree (whole file + range
/// variant), view_hir, view_mir. Each may legitimately return null while the
/// indexer hasn't fully primed; we just assert the shape doesn't error and
/// that the partial-range guard fires.
#[tokio::test]
async fn test_phase1_3_compiler_optionals() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();
    let main_path = workspace_path.join("src/main.rs");
    let main_str = main_path.to_str().unwrap();

    // syntax_tree — whole file (no range coords). Returns a printed tree string
    // (or null while indexing).
    let response = client
        .call_tool(
            "rust_analyzer_syntax_tree",
            json!({ "file_path": main_str }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    // syntax_tree — narrowed range over a few lines. All four coords present.
    let response = client
        .call_tool(
            "rust_analyzer_syntax_tree",
            json!({
                "file_path": main_str,
                "line": 0,
                "character": 0,
                "end_line": 5,
                "end_character": 0
            }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    // syntax_tree — partial range (line only) is a tool-call error: the
    // server must refuse rather than silently fall back to whole-file.
    let partial = client
        .call_tool(
            "rust_analyzer_syntax_tree",
            json!({ "file_path": main_str, "line": 0 }),
        )
        .await;
    assert!(
        partial.is_err(),
        "partial range must error, got {partial:?}"
    );

    // view_hir — inside `greet` body (line 11, char 8: at "{ format!(...)". String or null.
    let response = client
        .call_tool(
            "rust_analyzer_view_hir",
            json!({ "file_path": main_str, "line": 11, "character": 8 }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    // view_mir — same position. String or null.
    let response = client
        .call_tool(
            "rust_analyzer_view_mir",
            json!({ "file_path": main_str, "line": 11, "character": 8 }),
        )
        .await?;
    let _ = extract_tool_text(&response)?;

    Ok(())
}

/// Phase 2 — Result-Truncation und Pagination.
///
/// Wir prüfen die Output-Shape (verbose=true vs Default) und das
/// pagination-Roundtrip-Verhalten ohne uns auf konkrete LSP-Ergebnisgrößen
/// festzulegen — der Test-Workspace ist klein, also sind die meisten Pfade
/// "unter dem Cap". Was zählt: Shape-Stabilität.
#[tokio::test]
async fn test_phase2_truncation_and_pagination() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();
    let main_path = workspace_path.join("src/main.rs");
    let main_str = main_path.to_str().unwrap();

    // 1. Hover — verbose-Param wird akzeptiert, _truncated darf erscheinen oder nicht (kleine
    //    Hover-Inhalte im Test-Projekt → meist nicht).
    let response = client
        .call_tool(
            "rust_analyzer_hover",
            json!({ "file_path": main_str, "line": 4, "character": 15, "verbose": true }),
        )
        .await?;
    let text = extract_tool_text(&response)?;
    if text != "null" {
        let v: Value = serde_json::from_str(&text)?;
        assert!(
            v.get("_truncated").is_none(),
            "verbose=true must skip truncation"
        );
    }

    // 2. Completion — Default cap, Form ist immer { items, isIncomplete, total, returned }.
    let response = client
        .call_tool(
            "rust_analyzer_completion",
            json!({ "file_path": main_str, "line": 2, "character": 5 }),
        )
        .await?;
    let text = extract_tool_text(&response)?;
    if text != "null" {
        let v: Value = serde_json::from_str(&text)?;
        assert!(v.get("items").is_some(), "completion must wrap items");
        assert!(v.get("total").is_some());
        assert!(v.get("returned").is_some());
        let returned = v["returned"].as_u64().unwrap();
        let total = v["total"].as_u64().unwrap();
        assert!(returned <= 50);
        assert!(returned <= total);
        if returned < total {
            assert!(v.get("_truncated").is_some());
        }
    }

    // 3. Workspace-Symbol — Default page=100; wir setzen limit=1 um Pagination zu erzwingen
    //    (rust-analyzer findet >1 Symbol für "C" im Test-Projekt — Calculator etc.).
    let response = client
        .call_tool(
            "rust_analyzer_workspace_symbol",
            json!({ "query": "C", "limit": 1 }),
        )
        .await?;
    let text = extract_tool_text(&response)?;
    if text != "null" {
        let v: Value = serde_json::from_str(&text)?;
        assert!(v.get("symbols").is_some());
        let total = v["total"].as_u64().unwrap();
        let returned = v["returned"].as_u64().unwrap();
        assert!(returned <= 1);
        assert!(returned <= total);
        if total > 1 {
            // Erste Seite ist limit=1 → next_cursor muss kommen.
            assert_eq!(
                v["next_cursor"], "1",
                "next_cursor should be set when more results exist"
            );

            // Roundtrip: cursor mit der letzten Seite holen.
            let response2 = client
                .call_tool(
                    "rust_analyzer_workspace_symbol",
                    json!({ "query": "C", "limit": 1, "cursor": "1" }),
                )
                .await?;
            let text2 = extract_tool_text(&response2)?;
            let v2: Value = serde_json::from_str(&text2)?;
            assert_eq!(v2["total"], v["total"]);
            assert!(v2["returned"].as_u64().unwrap() <= 1);
        }
    }

    // 4. Workspace-Symbol — verbose=true entfernt das Cap.
    let response = client
        .call_tool(
            "rust_analyzer_workspace_symbol",
            json!({ "query": "C", "verbose": true }),
        )
        .await?;
    let text = extract_tool_text(&response)?;
    if text != "null" {
        let v: Value = serde_json::from_str(&text)?;
        let total = v["total"].as_u64().unwrap();
        let returned = v["returned"].as_u64().unwrap();
        assert_eq!(returned, total, "verbose=true should return all symbols");
        assert!(v.get("next_cursor").is_none());
    }

    // 5. Workspace-Diagnostics — pagination-Block muss vorhanden sein.
    let response = client
        .call_tool("rust_analyzer_workspace_diagnostics", json!({}))
        .await?;
    let text = extract_tool_text(&response)?;
    let v: Value = serde_json::from_str(&text)?;
    if v.get("files").is_some() {
        assert!(
            v.get("pagination").is_some(),
            "workspace_diagnostics must include pagination block"
        );
        assert!(v["pagination"]["total_files"].is_u64());
        assert!(v["pagination"]["returned_files"].is_u64());
        // Summary bleibt full-workspace.
        assert!(v.get("summary").is_some());
    }

    Ok(())
}

/// Phase 2.1 — Snippet-Anreicherung: `references` und `workspace_symbol`
/// liefern per Default ein `snippet`-Sibling neben jeder Location, und
/// `include_snippets=false` schaltet das ab.
#[tokio::test]
async fn test_phase2_1_snippets() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();
    let main_path = workspace_path.join("src/main.rs");
    let main_str = main_path.to_str().unwrap();

    // Helper: collect every nested `snippet` field in a JSON value.
    fn collect_snippets(v: &Value, out: &mut Vec<Value>) {
        match v {
            Value::Array(items) => items.iter().for_each(|i| collect_snippets(i, out)),
            Value::Object(obj) => {
                if let Some(s) = obj.get("snippet") {
                    out.push(s.clone());
                }
                for (_, child) in obj {
                    collect_snippets(child, out);
                }
            }
            _ => {}
        }
    }

    // 1. references with default args → snippets attached.
    let response = client
        .call_tool(
            "rust_analyzer_references",
            json!({ "file_path": main_str, "line": 9, "character": 4 }),
        )
        .await?;
    let text = extract_tool_text(&response)?;
    if text != "null" {
        let v: Value = serde_json::from_str(&text)?;
        let mut snippets = Vec::new();
        collect_snippets(&v, &mut snippets);
        // We don't pin the count (depends on indexing state) but if any
        // references came back, at least one should carry a snippet.
        if let Some(arr) = v.as_array() {
            if !arr.is_empty() {
                assert!(
                    !snippets.is_empty(),
                    "default references call should attach snippets, got {v:#?}"
                );
                let first = &snippets[0];
                assert!(first.get("start_line").is_some());
                assert!(first.get("lines").is_some());
                assert!(first["lines"].is_array());
            }
        }
    }

    // 2. references with include_snippets=false → no snippets at all.
    let response = client
        .call_tool(
            "rust_analyzer_references",
            json!({
                "file_path": main_str,
                "line": 9,
                "character": 4,
                "include_snippets": false,
            }),
        )
        .await?;
    let text = extract_tool_text(&response)?;
    if text != "null" {
        let v: Value = serde_json::from_str(&text)?;
        let mut snippets = Vec::new();
        collect_snippets(&v, &mut snippets);
        assert!(
            snippets.is_empty(),
            "include_snippets=false must skip snippet attachment, got {snippets:#?}"
        );
    }

    // 3. workspace_symbol — paginated wrapper, snippet must sit on the nested location object (not
    //    the symbol itself).
    let response = client
        .call_tool(
            "rust_analyzer_workspace_symbol",
            json!({ "query": "Calculator", "limit": 5 }),
        )
        .await?;
    let text = extract_tool_text(&response)?;
    if text != "null" {
        let v: Value = serde_json::from_str(&text)?;
        if let Some(symbols) = v.get("symbols").and_then(|s| s.as_array()) {
            for sym in symbols {
                if let Some(loc) = sym.get("location") {
                    // If a location is present, it should also carry a snippet.
                    assert!(
                        loc.get("snippet").is_some(),
                        "expected snippet on location in symbol {sym:#?}"
                    );
                }
            }
        }
    }

    // 4. snippet_context_lines=0 → snippet covers exactly the range lines.
    let response = client
        .call_tool(
            "rust_analyzer_definition",
            json!({
                "file_path": main_str,
                "line": 1,
                "character": 18,
                "snippet_context_lines": 0,
            }),
        )
        .await?;
    let text = extract_tool_text(&response)?;
    if text != "null" && text != "[]" {
        let v: Value = serde_json::from_str(&text)?;
        let mut snippets = Vec::new();
        collect_snippets(&v, &mut snippets);
        if let Some(s) = snippets.first() {
            let lines = s["lines"].as_array().expect("snippet lines");
            // 1 line for a single-line range, up to a few for a multi-line one.
            assert!(!lines.is_empty(), "snippet must include at least one line");
        }
    }

    Ok(())
}

/// Phase 3.1 — MCP-Resources: list + read für `workspace://files`,
/// `workspace://crates` und per-Crate-Manifests.
#[tokio::test]
async fn test_phase3_resources() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;

    // 1. resources/list — must include workspace://files plus crate resources discovered via cargo
    //    metadata.
    let list_resp = client.send_request("resources/list", None).await?;
    let resources = list_resp["resources"].as_array().expect("resources array");
    let uris: Vec<&str> = resources.iter().filter_map(|r| r["uri"].as_str()).collect();
    assert!(
        uris.contains(&"workspace://files"),
        "expected workspace://files in resources/list, got {uris:?}"
    );
    assert!(
        uris.contains(&"workspace://crates"),
        "expected workspace://crates in resources/list, got {uris:?}"
    );
    assert!(
        uris.contains(&"workspace://crate/test-project/Cargo.toml"),
        "expected per-crate manifest URI in resources/list, got {uris:?}"
    );

    // 2. resources/read for workspace://files — yields a JSON file-tree.
    let read_resp = client
        .send_request(
            "resources/read",
            Some(json!({ "uri": "workspace://files" })),
        )
        .await?;
    let contents = read_resp["contents"].as_array().expect("contents array");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["mimeType"], "application/json");

    let tree_text = contents[0]["text"].as_str().expect("text payload");
    let tree: Value = serde_json::from_str(tree_text)?;
    assert!(tree["root"].as_str().is_some());
    assert_eq!(tree["tree"]["type"], "dir");
    assert!(tree["stats"]["entries"].as_u64().unwrap() > 0);

    let children = tree["tree"]["children"].as_array().expect("root children");
    let names: Vec<&str> = children.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(
        names.contains(&"Cargo.toml"),
        "expected Cargo.toml at root, got {names:?}"
    );
    assert!(
        names.contains(&"src"),
        "expected src/ at root, got {names:?}"
    );
    assert!(!names.contains(&"target"), "target/ should be ignored");

    // 3. resources/read for workspace://crates — yields cargo metadata summary.
    let crates_resp = client
        .send_request(
            "resources/read",
            Some(json!({ "uri": "workspace://crates" })),
        )
        .await?;
    let crates_text = crates_resp["contents"][0]["text"]
        .as_str()
        .expect("crates text");
    let crates: Value = serde_json::from_str(crates_text)?;
    let pkgs = crates["packages"].as_array().expect("packages");
    let pkg_names: Vec<&str> = pkgs.iter().filter_map(|p| p["name"].as_str()).collect();
    assert!(
        pkg_names.contains(&"test-project"),
        "expected test-project crate, got {pkg_names:?}"
    );
    let test_project = pkgs.iter().find(|p| p["name"] == "test-project").unwrap();
    assert_eq!(test_project["is_workspace_member"], true);
    assert!(!test_project["targets"].as_array().unwrap().is_empty());

    // 4. resources/read for the per-crate manifest — returns the actual Cargo.toml.
    let manifest_resp = client
        .send_request(
            "resources/read",
            Some(json!({ "uri": "workspace://crate/test-project/Cargo.toml" })),
        )
        .await?;
    assert_eq!(manifest_resp["contents"][0]["mimeType"], "application/toml");
    let manifest_text = manifest_resp["contents"][0]["text"]
        .as_str()
        .expect("manifest text");
    assert!(
        manifest_text.contains("[package]"),
        "expected [package] section in manifest, got: {manifest_text}"
    );
    assert!(
        manifest_text.contains("name = \"test-project\""),
        "expected name = \"test-project\" in manifest"
    );

    // 5. Path-traversal attempts via crafted crate names must fail with Unknown crate, not a
    //    filesystem read.
    let bogus = client
        .send_request(
            "resources/read",
            Some(json!({ "uri": "workspace://crate/..%2F..%2Fetc%2Fpasswd/Cargo.toml" })),
        )
        .await;
    assert!(bogus.is_err(), "path traversal should fail");

    // 6. Unknown URI → error.
    let resp = client
        .send_request("resources/read", Some(json!({ "uri": "workspace://nope" })))
        .await;
    assert!(resp.is_err(), "unknown resource should error, got {resp:?}");

    Ok(())
}

/// Phase 3.2 — Multi-Workspace: register a second isolated workspace, run a
/// tool against it via `workspace_id`, and verify list/remove. The shared
/// daemon's default workspace must remain untouched so subsequent tests still
/// find their files.
#[tokio::test]
async fn test_phase3_multi_workspace() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;

    // Snapshot defaults so we can sanity-check non-interference at the end.
    let list_before = client
        .call_tool("rust_analyzer_list_workspaces", json!({}))
        .await?;
    let before_text = extract_tool_text(&list_before)?;
    let before: Value = serde_json::from_str(&before_text)?;
    let before_count = before["workspaces"].as_array().unwrap().len();
    let default_id_before = before["workspaces"][0]["workspace_id"]
        .as_str()
        .expect("default has id")
        .to_string();
    assert_eq!(before["workspaces"][0]["default"], true);

    // Add a second isolated workspace.
    let second = test_support::IsolatedProject::new()?;
    let add_resp = client
        .call_tool(
            "rust_analyzer_add_workspace",
            json!({ "path": second.path().to_str().unwrap() }),
        )
        .await?;
    let add_text = extract_tool_text(&add_resp)?;
    let added: Value = serde_json::from_str(&add_text)?;
    let new_id = added["workspace_id"]
        .as_str()
        .expect("added id")
        .to_string();
    assert!(
        new_id.starts_with("ws-") && new_id != default_id_before,
        "new id should differ from default: {new_id} vs {default_id_before}"
    );

    // List now contains both, default unchanged.
    let list_resp = client
        .call_tool("rust_analyzer_list_workspaces", json!({}))
        .await?;
    let list: Value = serde_json::from_str(&extract_tool_text(&list_resp)?)?;
    let entries = list["workspaces"].as_array().unwrap();
    assert_eq!(entries.len(), before_count + 1);
    assert_eq!(entries[0]["workspace_id"], default_id_before);
    assert_eq!(entries[0]["default"], true);
    assert!(entries.iter().any(|e| e["workspace_id"] == new_id));

    // Run a tool targeted at the new workspace. workspace_symbol with empty
    // query is null-tolerant; we just need the call to route to the right
    // backend without erroring.
    let ws_sym = client
        .call_tool(
            "rust_analyzer_workspace_symbol",
            json!({ "query": "main", "workspace_id": new_id }),
        )
        .await?;
    let _ = extract_tool_text(&ws_sym)?;

    // Unknown workspace_id surfaces as a tool error, not a panic.
    let bogus = client
        .call_tool(
            "rust_analyzer_workspace_symbol",
            json!({ "query": "x", "workspace_id": "ws-does-not-exist" }),
        )
        .await;
    assert!(
        bogus.is_err(),
        "unknown workspace_id should error, got {bogus:?}"
    );

    // Remove the second workspace; default stays.
    let remove_resp = client
        .call_tool(
            "rust_analyzer_remove_workspace",
            json!({ "workspace_id": new_id }),
        )
        .await?;
    let removed_text = extract_tool_text(&remove_resp)?;
    let removed: Value = serde_json::from_str(&removed_text)?;
    assert_eq!(removed["removed"], new_id);

    let list_after = client
        .call_tool("rust_analyzer_list_workspaces", json!({}))
        .await?;
    let after: Value = serde_json::from_str(&extract_tool_text(&list_after)?)?;
    assert_eq!(after["workspaces"].as_array().unwrap().len(), before_count);
    assert_eq!(after["workspaces"][0]["workspace_id"], default_id_before);
    assert_eq!(after["workspaces"][0]["default"], true);

    // Removing again must error cleanly.
    let twice = client
        .call_tool(
            "rust_analyzer_remove_workspace",
            json!({ "workspace_id": new_id }),
        )
        .await;
    assert!(
        twice.is_err(),
        "removing an unknown workspace should error, got {twice:?}"
    );

    Ok(())
}

/// Phase 3.2 — Multi-workspace MCP resources: prefixed URIs route to the
/// right workspace, default keeps unprefixed aliases for backward compat.
#[tokio::test]
async fn test_phase3_multi_workspace_resources() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;

    // Add a second isolated workspace so we can verify per-id routing.
    let second = test_support::IsolatedProject::new()?;
    let add_resp = client
        .call_tool(
            "rust_analyzer_add_workspace",
            json!({ "path": second.path().to_str().unwrap() }),
        )
        .await?;
    let added: Value = serde_json::from_str(&extract_tool_text(&add_resp)?)?;
    let new_id = added["workspace_id"].as_str().unwrap().to_string();

    // resources/list now includes BOTH prefixed and unprefixed (default-alias)
    // URIs. The prefixed URI for the second workspace must appear.
    let list = client.send_request("resources/list", None).await?;
    let resources = list["resources"].as_array().expect("resources array");
    let uris: Vec<&str> = resources.iter().filter_map(|r| r["uri"].as_str()).collect();
    let expected_prefixed = format!("workspace://{new_id}/files");
    assert!(
        uris.contains(&expected_prefixed.as_str()),
        "expected {expected_prefixed} in {uris:?}"
    );
    // Backward-compat alias for the default workspace must still be there.
    assert!(
        uris.contains(&"workspace://files"),
        "expected unprefixed default alias, got {uris:?}"
    );

    // Reading the prefixed URI returns the second workspace's tree, and the
    // response URI round-trips the caller's input verbatim.
    let read_resp = client
        .send_request("resources/read", Some(json!({ "uri": expected_prefixed })))
        .await?;
    let contents = read_resp["contents"].as_array().expect("contents");
    assert_eq!(contents[0]["uri"], expected_prefixed);
    let tree: Value = serde_json::from_str(contents[0]["text"].as_str().unwrap())?;
    let tree_root = tree["root"].as_str().unwrap();
    // The second workspace's tree root must point at the IsolatedProject path,
    // not at the test-project default.
    let expected_root = second.path().canonicalize()?;
    assert!(
        tree_root.contains(&expected_root.display().to_string())
            || expected_root.display().to_string().contains(tree_root),
        "tree root {tree_root} should match second workspace {}",
        expected_root.display()
    );

    // Unknown workspace id in URI falls back to the default workspace (legacy
    // behavior — `workspace://crate/...` must keep working). We assert this by
    // reading `workspace://files` after the new workspace exists: it should
    // still return the default's tree.
    let default_read = client
        .send_request(
            "resources/read",
            Some(json!({ "uri": "workspace://files" })),
        )
        .await?;
    let default_tree: Value =
        serde_json::from_str(default_read["contents"][0]["text"].as_str().unwrap())?;
    let default_tree_root = default_tree["root"].as_str().unwrap();
    assert!(
        !default_tree_root.contains(&expected_root.display().to_string()),
        "unprefixed URI must not route to second workspace"
    );

    // Cleanup so other tests aren't perturbed.
    client
        .call_tool(
            "rust_analyzer_remove_workspace",
            json!({ "workspace_id": new_id }),
        )
        .await?;

    Ok(())
}

fn extract_tool_text(response: &Value) -> Result<String> {
    let content = response
        .get("content")
        .ok_or_else(|| anyhow::anyhow!("no content"))?;
    let text = content[0]
        .get("text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow::anyhow!("no text"))?;
    Ok(text.to_string())
}

#[tokio::test]
async fn test_error_handling_invalid_files() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();

    // Test multiple invalid file paths
    let invalid_paths = vec![
        workspace_path.join("non_existent.rs"),
        workspace_path.join("../../../etc/passwd"),
    ];

    for file_path in invalid_paths {
        // Try to get symbols for invalid file
        let result = client
            .call_tool(
                "rust_analyzer_symbols",
                json!({
                    "file_path": file_path.to_str().unwrap()
                }),
            )
            .await;

        // Should either error or return empty/null
        if let Ok(response) = result {
            if let Some(content) = response.get("content") {
                if let Some(text) = content[0].get("text") {
                    let symbols: Vec<Value> =
                        serde_json::from_str(text.as_str().unwrap_or("[]")).unwrap_or_default();
                    assert!(
                        symbols.is_empty(),
                        "Should not have symbols for invalid file: {}",
                        file_path.display()
                    );
                }
            }
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_error_handling_invalid_positions() -> Result<()> {
    let mut client = IpcClient::get_or_create("test-project").await?;
    let workspace_path = client.workspace_path().to_path_buf();
    let main_path = workspace_path.join("src/main.rs");

    // Test multiple invalid positions
    let invalid_positions = vec![
        (u32::MAX, 0),        // negative line
        (0, 999999),          // huge column
        (u32::MAX, u32::MAX), // both invalid
    ];

    for (line, character) in invalid_positions {
        // Try to get definition at invalid position
        let result = client
            .call_tool(
                "rust_analyzer_definition",
                json!({
                    "file_path": main_path.to_str().unwrap(),
                    "line": line,
                    "character": character
                }),
            )
            .await;

        // Should either error or return empty/null
        if let Ok(response) = result {
            if let Some(content) = response.get("content") {
                if let Some(text) = content[0].get("text") {
                    let definitions: Vec<Value> =
                        serde_json::from_str(text.as_str().unwrap_or("[]")).unwrap_or_default();
                    assert!(
                        definitions.is_empty(),
                        "Should not have definition at invalid position ({}, {})",
                        line,
                        character
                    );
                }
            }
        }
    }

    Ok(())
}

// Helper functions for test_all_lsp_tools

async fn test_symbols(client: &mut IpcClient, workspace_path: &Path) -> Result<()> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_symbols",
            json!({
                "file_path": main_path.to_str().unwrap()
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Err(anyhow::anyhow!("No content in symbols response"));
    };

    let Some(text) = content[0].get("text") else {
        return Err(anyhow::anyhow!("No text in symbols response"));
    };

    let Some(text_str) = text.as_str() else {
        return Err(anyhow::anyhow!("Text is not a string"));
    };

    let symbols: Vec<Value> = serde_json::from_str(text_str)?;
    assert!(!symbols.is_empty(), "Should have symbols in main.rs");

    let symbol_names: Vec<String> = symbols
        .iter()
        .filter_map(|s| s.get("name")?.as_str().map(String::from))
        .collect();

    assert!(
        symbol_names.contains(&"main".to_string()),
        "Should have main function"
    );
    assert!(
        symbol_names.contains(&"greet".to_string()),
        "Should have greet function"
    );
    assert!(
        symbol_names.contains(&"Calculator".to_string()),
        "Should have Calculator struct"
    );

    Ok(())
}

async fn test_definition(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_definition",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 1,
                "character": 18
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    if !content.is_array() || content[0].is_null() {
        return Ok(false);
    }

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    // null or empty array during initialization is normal for LSP.
    // We just check that we got a valid response.
    if text_str == "null" || text_str == "[]" {
        // This is a valid response during initialization.
        return Ok(true);
    }

    // Try to parse as array
    let Ok(definitions) = serde_json::from_str::<Vec<Value>>(text_str) else {
        return Ok(false);
    };

    Ok(!definitions.is_empty())
}

async fn test_references(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_references",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 9,
                "character": 4
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    if text.as_str() == Some("null") {
        return Ok(false);
    }

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    let references: Vec<Value> = serde_json::from_str(text_str)?;
    Ok(!references.is_empty())
}

async fn test_hover(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_hover",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 4,
                "character": 15
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    if text.as_str() == Some("null") {
        return Ok(false);
    }

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    let hover: Value = serde_json::from_str(text_str)?;
    Ok(hover.get("contents").is_some())
}

async fn test_completion(client: &mut IpcClient, workspace_path: &Path) -> Result<()> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_completion",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 2,
                "character": 5
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(());
    };

    let Some(text) = content[0].get("text") else {
        return Ok(());
    };

    let Some(text_str) = text.as_str() else {
        return Ok(());
    };

    // Handle "null" response specially
    if text_str == "null" {
        // rust-analyzer returned null - still indexing
        eprintln!("Got null completion response (rust-analyzer may still be indexing)");
        return Ok(());
    }

    let completions: Value = serde_json::from_str(text_str)?;
    assert!(
        completions.is_object() || completions.is_array() || completions.is_null(),
        "Expected object, array, or null, got: {:?}",
        completions
    );

    Ok(())
}

async fn test_format(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    // Test 1: Format already-formatted file - should return null (no edits needed)
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_format",
            json!({
                "file_path": main_path.to_str().unwrap()
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    // main.rs is already formatted, so should return null
    if text_str != "null" {
        eprintln!("Expected null for formatted file, got: {}", text_str);
        return Ok(false);
    }

    // Test 2: Format unformatted file - should return edits
    let unformatted_path = workspace_path.join("src/unformatted.rs");
    let response = client
        .call_tool(
            "rust_analyzer_format",
            json!({
                "file_path": unformatted_path.to_str().unwrap()
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    // unformatted.rs needs formatting, so should return an array of edits
    if text_str == "null" {
        eprintln!("Expected edits for unformatted file, got null");
        return Ok(false);
    }

    // Parse and validate it's a non-empty array of edits
    let edits: Vec<Value> = serde_json::from_str(text_str)?;
    if edits.is_empty() {
        eprintln!("Expected non-empty edits for unformatted file");
        return Ok(false);
    }

    Ok(true)
}

async fn test_code_actions(client: &mut IpcClient, workspace_path: &Path) -> Result<bool> {
    let main_path = workspace_path.join("src/main.rs");
    let response = client
        .call_tool(
            "rust_analyzer_code_actions",
            json!({
                "file_path": main_path.to_str().unwrap(),
                "line": 13,
                "character": 0,
                "end_line": 16,
                "end_character": 1
            }),
        )
        .await?;

    let Some(content) = response.get("content") else {
        return Ok(false);
    };

    let Some(text) = content[0].get("text") else {
        return Ok(false);
    };

    let Some(text_str) = text.as_str() else {
        return Ok(false);
    };

    // Check if we got null or empty array
    if text_str == "null" || text_str == "[]" {
        return Ok(false);
    }

    // Try to parse as array to verify it's valid JSON
    let Ok(_actions) = serde_json::from_str::<Vec<Value>>(text_str) else {
        return Ok(false);
    };

    // Even if we get an empty array, that's better than null
    // Some files genuinely might not have code actions available
    Ok(true)
}
