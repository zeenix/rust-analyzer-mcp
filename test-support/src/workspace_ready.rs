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

        sleep(Duration::from_millis(500)).await;

        self.open_all_critical_files().await?;
        eprintln!("[WorkspaceReadiness] Opened all critical files");

        self.wait_for_symbols().await?;
        eprintln!("[WorkspaceReadiness] Symbols are available");

        self.wait_for_imports_resolved().await?;
        eprintln!("[WorkspaceReadiness] Module imports are resolved");

        self.wait_for_stable_diagnostics().await?;
        eprintln!("[WorkspaceReadiness] Diagnostics are stable");

        sleep(Duration::from_millis(300)).await;

        eprintln!("[WorkspaceReadiness] Workspace is fully ready!");
        Ok(())
    }

    /// Open all critical files to ensure they're analyzed.
    async fn open_all_critical_files(&self) -> Result<()> {
        for file_path in &self.critical_files {
            let _ = self
                .client
                .call_tool("rust_analyzer_symbols", json!({"file_path": file_path}))
                .await;

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

                let Some(content) = response.get("content") else {
                    all_ready = false;
                    break;
                };
                let Some(text) = content[0].get("text") else {
                    all_ready = false;
                    break;
                };
                let Some(text_str) = text.as_str() else {
                    all_ready = false;
                    break;
                };

                if text_str == "null" || text_str == "[]" {
                    all_ready = false;
                    break;
                }

                let Ok(symbols) = serde_json::from_str::<Vec<Value>>(text_str) else {
                    all_ready = false;
                    break;
                };

                if symbols.is_empty() {
                    all_ready = false;
                    break;
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
        let lib_file = "src/lib.rs";

        while start.elapsed() < self.timeout {
            let response = self
                .client
                .call_tool("rust_analyzer_diagnostics", json!({"file_path": lib_file}))
                .await?;

            let Some(content) = response.get("content") else {
                sleep(Duration::from_millis(500)).await;
                continue;
            };
            let Some(text) = content[0].get("text") else {
                sleep(Duration::from_millis(500)).await;
                continue;
            };
            let Some(text_str) = text.as_str() else {
                sleep(Duration::from_millis(500)).await;
                continue;
            };
            let Ok(parsed) = serde_json::from_str::<Value>(text_str) else {
                sleep(Duration::from_millis(500)).await;
                continue;
            };

            if !parsed["summary"].is_object() {
                sleep(Duration::from_millis(500)).await;
                continue;
            }

            let Some(diagnostics) = parsed["diagnostics"].as_array() else {
                sleep(Duration::from_millis(500)).await;
                continue;
            };

            let has_module_import_errors = diagnostics.iter().any(|d| {
                let Some(message) = d["message"].as_str() else {
                    return false;
                };
                (message.contains("unresolved import") || message.contains("cannot find module"))
                    && (message.contains("`types`") || message.contains("`utils`"))
            });

            if !has_module_import_errors {
                let symbols_response = self
                    .client
                    .call_tool("rust_analyzer_symbols", json!({"file_path": lib_file}))
                    .await?;

                let Some(sym_content) = symbols_response.get("content") else {
                    sleep(Duration::from_millis(500)).await;
                    continue;
                };
                let Some(sym_text) = sym_content[0].get("text") else {
                    sleep(Duration::from_millis(500)).await;
                    continue;
                };
                let Some(sym_str) = sym_text.as_str() else {
                    sleep(Duration::from_millis(500)).await;
                    continue;
                };

                if sym_str != "null" && sym_str != "[]" {
                    eprintln!("[WorkspaceReadiness] Module imports resolved, symbols available");
                    return Ok(());
                }
            }

            if has_module_import_errors {
                eprintln!(
                    "[WorkspaceReadiness] Still have module import errors, waiting... (attempt {})",
                    start.elapsed().as_secs()
                );
                for diag in diagnostics {
                    let Some(message) = diag["message"].as_str() else {
                        continue;
                    };
                    if (message.contains("unresolved import")
                        || message.contains("cannot find module"))
                        && (message.contains("`types`") || message.contains("`utils`"))
                    {
                        eprintln!("  - {}", message);
                    }
                }
            }

            sleep(Duration::from_millis(500)).await;
        }

        eprintln!("[WorkspaceReadiness] Timeout but no module import errors detected, continuing");
        Ok(())
    }

    /// Wait for diagnostics to stabilize (no changes between queries).
    async fn wait_for_stable_diagnostics(&self) -> Result<()> {
        let start = std::time::Instant::now();
        let mut last_diagnostics: Option<HashSet<String>> = None;
        let mut stable_count = 0;
        let required_stable_checks = if is_ci() { 4 } else { 3 };

        while start.elapsed() < self.timeout {
            let mut current_diagnostics = HashSet::new();

            for file_path in &self.critical_files {
                let response = self
                    .client
                    .call_tool("rust_analyzer_diagnostics", json!({"file_path": file_path}))
                    .await?;

                let Some(content) = response.get("content") else {
                    continue;
                };
                let Some(text) = content[0].get("text") else {
                    continue;
                };
                let Some(text_str) = text.as_str() else {
                    continue;
                };
                let Ok(parsed) = serde_json::from_str::<Value>(text_str) else {
                    continue;
                };
                let Some(diags) = parsed["diagnostics"].as_array() else {
                    continue;
                };

                for diag in diags {
                    let Some(message) = diag["message"].as_str() else {
                        continue;
                    };
                    if message.contains("unresolved macro") || message.contains("no such value") {
                        continue;
                    }

                    let key = format!(
                        "{}:{}:{}",
                        file_path,
                        diag["severity"].as_str().unwrap_or("unknown"),
                        message
                    );
                    current_diagnostics.insert(key);
                }
            }

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
            sleep(Duration::from_millis(750)).await;
        }
        if stable_count >= 2 {
            eprintln!(
                "[WorkspaceReadiness] Timeout but had {} stable readings, continuing",
                stable_count
            );
            return Ok(());
        }

        if stable_count == 1 {
            eprintln!(
                "[WorkspaceReadiness] Warning: Only {} stable reading before timeout, may be unreliable",
                stable_count
            );
            return Ok(());
        }

        Err(anyhow::anyhow!(
            "Diagnostics did not stabilize after {:?}",
            self.timeout
        ))
    }
}
