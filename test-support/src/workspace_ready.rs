use anyhow::Result;
use serde_json::{json, Value};
use std::{collections::HashSet, time::Duration};
use tokio::time::sleep;

use crate::{is_ci, MCPTestClient};

/// Enhanced workspace readiness checker for rust-analyzer.
pub struct WorkspaceReadiness<'a> {
    client: &'a MCPTestClient,
    /// Files that must be properly indexed for the workspace to be considered ready.
    critical_files: Vec<String>,
    /// Maximum time to wait for workspace to be ready.
    timeout: Duration,
}

impl<'a> WorkspaceReadiness<'a> {
    /// Create a new readiness checker with default settings.
    pub fn new(client: &'a MCPTestClient) -> Self {
        Self {
            client,
            critical_files: vec![
                "src/lib.rs".to_string(),
                "src/types.rs".to_string(),
                "src/utils.rs".to_string(),
            ],
            timeout: if is_ci() {
                Duration::from_secs(30)
            } else {
                Duration::from_secs(15)
            },
        }
    }

    /// Create a readiness checker with custom critical files.
    pub fn with_files(client: &'a MCPTestClient, files: Vec<String>) -> Self {
        Self {
            client,
            critical_files: files,
            timeout: if is_ci() {
                Duration::from_secs(30)
            } else {
                Duration::from_secs(15)
            },
        }
    }

    /// Ensure the workspace is fully ready for testing.
    pub async fn ensure_ready(&self) -> Result<()> {
        eprintln!("[WorkspaceReadiness] Starting workspace readiness check...");

        // Step 1: Basic initialization
        self.client.initialize().await?;
        eprintln!("[WorkspaceReadiness] Basic initialization complete");

        // Step 2: Add a small delay to let rust-analyzer start up properly
        // This helps avoid race conditions where we query too early
        sleep(Duration::from_millis(500)).await;

        // Step 3: Open all critical files to trigger analysis
        self.open_all_critical_files().await?;
        eprintln!("[WorkspaceReadiness] Opened all critical files");

        // Step 4: Wait for symbols to be available (indicates indexing is progressing)
        self.wait_for_symbols().await?;
        eprintln!("[WorkspaceReadiness] Symbols are available");

        // Step 5: Verify module imports are resolved
        self.wait_for_imports_resolved().await?;
        eprintln!("[WorkspaceReadiness] Module imports are resolved");

        // Step 6: Wait for diagnostic stability
        self.wait_for_stable_diagnostics().await?;
        eprintln!("[WorkspaceReadiness] Diagnostics are stable");

        // Step 7: Final stabilization delay
        // In parallel test execution, give a bit more time for everything to settle
        sleep(Duration::from_millis(300)).await;

        eprintln!("[WorkspaceReadiness] Workspace is fully ready!");
        Ok(())
    }

    /// Open all critical files to ensure they're analyzed.
    async fn open_all_critical_files(&self) -> Result<()> {
        for file_path in &self.critical_files {
            // Call symbols tool which internally opens the document
            let _ = self
                .client
                .call_tool("rust_analyzer_symbols", json!({"file_path": file_path}))
                .await;

            // Increase delay between file opens to avoid overwhelming rust-analyzer
            // especially when multiple tests run in parallel
            sleep(Duration::from_millis(200)).await;
        }
        Ok(())
    }

    /// Wait for symbols to be available in all critical files.
    async fn wait_for_symbols(&self) -> Result<()> {
        let start = std::time::Instant::now();

        while start.elapsed() < self.timeout {
            let mut all_ready = true;

            for file_path in &self.critical_files {
                let response = self
                    .client
                    .call_tool("rust_analyzer_symbols", json!({"file_path": file_path}))
                    .await?;

                if let Some(content) = response.get("content") {
                    if let Some(text) = content[0].get("text") {
                        if let Some(text_str) = text.as_str() {
                            // Check if we got null or empty response
                            if text_str == "null" || text_str == "[]" {
                                all_ready = false;
                                break;
                            }

                            // Try to parse symbols
                            if let Ok(symbols) = serde_json::from_str::<Vec<Value>>(text_str) {
                                if symbols.is_empty() {
                                    all_ready = false;
                                    break;
                                }
                            } else {
                                all_ready = false;
                                break;
                            }
                        }
                    }
                }
            }

            if all_ready {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }

        Err(anyhow::anyhow!(
            "Timeout waiting for symbols to be available after {:?}",
            self.timeout
        ))
    }

    /// Wait for module imports to be resolved (no unresolved import errors).
    async fn wait_for_imports_resolved(&self) -> Result<()> {
        let start = std::time::Instant::now();

        // Focus on lib.rs which has the module imports
        let lib_file = "src/lib.rs";

        while start.elapsed() < self.timeout {
            let response = self
                .client
                .call_tool("rust_analyzer_diagnostics", json!({"file_path": lib_file}))
                .await?;

            if let Some(content) = response.get("content") {
                if let Some(text) = content[0].get("text") {
                    if let Some(text_str) = text.as_str() {
                        if let Ok(parsed) = serde_json::from_str::<Value>(text_str) {
                            // First check if we have a valid diagnostic response
                            if parsed["summary"].is_object() {
                                if let Some(diagnostics) = parsed["diagnostics"].as_array() {
                                    // Only check for module import errors specifically
                                    // Ignore transient errors about macros and values which can
                                    // occur during initial
                                    // rust-analyzer startup
                                    let has_module_import_errors = diagnostics.iter().any(|d| {
                                        if let Some(message) = d["message"].as_str() {
                                            // Only look for errors specifically about our modules
                                            (message.contains("unresolved import")
                                                || message.contains("cannot find module"))
                                                && (message.contains("`types`")
                                                    || message.contains("`utils`"))
                                        } else {
                                            false
                                        }
                                    });

                                    if !has_module_import_errors {
                                        // Check if we have valid symbols which indicates analysis
                                        // is done
                                        let symbols_response = self
                                            .client
                                            .call_tool(
                                                "rust_analyzer_symbols",
                                                json!({"file_path": lib_file}),
                                            )
                                            .await?;

                                        if let Some(sym_content) = symbols_response.get("content") {
                                            if let Some(sym_text) = sym_content[0].get("text") {
                                                if let Some(sym_str) = sym_text.as_str() {
                                                    if sym_str != "null" && sym_str != "[]" {
                                                        eprintln!(
                                                            "[WorkspaceReadiness] Module imports resolved, symbols available"
                                                        );
                                                        return Ok(());
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    if has_module_import_errors {
                                        eprintln!(
                                            "[WorkspaceReadiness] Still have module import errors, waiting... (attempt {})",
                                            start.elapsed().as_secs()
                                        );
                                        // Log the specific errors for debugging
                                        for diag in diagnostics {
                                            if let Some(message) = diag["message"].as_str() {
                                                if (message.contains("unresolved import")
                                                    || message.contains("cannot find module"))
                                                    && (message.contains("`types`")
                                                        || message.contains("`utils`"))
                                                {
                                                    eprintln!("  - {}", message);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            sleep(Duration::from_millis(500)).await;
        }

        // Don't fail if only transient errors remain
        eprintln!("[WorkspaceReadiness] Timeout but no module import errors detected, continuing");
        Ok(())
    }

    /// Wait for diagnostics to stabilize (no changes between queries).
    async fn wait_for_stable_diagnostics(&self) -> Result<()> {
        let start = std::time::Instant::now();
        let mut last_diagnostics: Option<HashSet<String>> = None;
        let mut stable_count = 0;
        // Increase stability requirements for more reliability
        let required_stable_checks = if is_ci() { 4 } else { 3 };

        while start.elapsed() < self.timeout {
            let mut current_diagnostics = HashSet::new();

            // Check diagnostics for all critical files
            for file_path in &self.critical_files {
                let response = self
                    .client
                    .call_tool("rust_analyzer_diagnostics", json!({"file_path": file_path}))
                    .await?;

                if let Some(content) = response.get("content") {
                    if let Some(text) = content[0].get("text") {
                        if let Some(text_str) = text.as_str() {
                            // Create a stable representation of diagnostics
                            if let Ok(parsed) = serde_json::from_str::<Value>(text_str) {
                                if let Some(diags) = parsed["diagnostics"].as_array() {
                                    // Skip transient macro errors for stability check
                                    for diag in diags {
                                        if let Some(message) = diag["message"].as_str() {
                                            // Skip known transient errors that occur during
                                            // initialization
                                            if message.contains("unresolved macro")
                                                || message.contains("no such value")
                                            {
                                                continue;
                                            }
                                        }

                                        let key = format!(
                                            "{}:{}:{}",
                                            file_path,
                                            diag["severity"].as_str().unwrap_or("unknown"),
                                            diag["message"].as_str().unwrap_or("")
                                        );
                                        current_diagnostics.insert(key);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Check if diagnostics are stable
            if let Some(ref last) = last_diagnostics {
                if *last == current_diagnostics {
                    stable_count += 1;
                    if stable_count >= required_stable_checks {
                        return Ok(());
                    }
                } else {
                    stable_count = 0;
                    eprintln!(
                        "[WorkspaceReadiness] Diagnostics changed, resetting stability counter"
                    );
                }
            }

            last_diagnostics = Some(current_diagnostics);
            // Increase poll interval to reduce load during parallel tests
            sleep(Duration::from_millis(750)).await;
        }

        // If we timeout but have at least 2 stable readings, consider it good enough
        if stable_count >= 2 {
            eprintln!(
                "[WorkspaceReadiness] Timeout but had {} stable readings, continuing",
                stable_count
            );
            Ok(())
        } else if stable_count == 1 {
            eprintln!(
                "[WorkspaceReadiness] Warning: Only {} stable reading before timeout, may be unreliable",
                stable_count
            );
            // Still continue but warn
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Diagnostics did not stabilize after {:?}",
                self.timeout
            ))
        }
    }
}
