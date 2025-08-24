pub mod fixtures;
pub mod mock_lsp;
pub mod test_client;

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tokio::process::{Child, Command};

/// Creates a test server instance with mock LSP
pub async fn create_test_server() -> TestServer {
    TestServer::new()
        .await
        .expect("Failed to create test server")
}

pub struct TestServer {
    pub workspace: TempDir,
    pub process: Option<Child>,
    pub mock_lsp: Option<mock_lsp::MockLSPServer>,
}

impl TestServer {
    pub async fn new() -> Result<Self> {
        let workspace = TempDir::new()?;
        fixtures::TestProject::simple().create_in(&workspace)?;

        Ok(Self {
            workspace,
            process: None,
            mock_lsp: None,
        })
    }

    pub async fn with_mock_lsp() -> Result<Self> {
        let workspace = TempDir::new()?;
        fixtures::TestProject::simple().create_in(&workspace)?;

        let mock_lsp = mock_lsp::MockLSPServer::start().await;

        Ok(Self {
            workspace,
            process: None,
            mock_lsp: Some(mock_lsp),
        })
    }

    pub fn workspace_path(&self) -> &Path {
        self.workspace.path()
    }

    pub fn main_rs_path(&self) -> PathBuf {
        self.workspace.path().join("src/main.rs")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
        }
    }
}

/// Helper to parse symbol responses
pub fn parse_symbols(response: Value) -> Vec<SymbolInfo> {
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            if let Ok(symbols) = serde_json::from_str::<Vec<Value>>(text.as_str().unwrap_or("[]")) {
                return symbols
                    .into_iter()
                    .filter_map(|s| {
                        Some(SymbolInfo {
                            name: s.get("name")?.as_str()?.to_string(),
                            kind: s.get("kind")?.as_str()?.to_string(),
                        })
                    })
                    .collect();
            }
        }
    }
    vec![]
}

#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
}

/// Helper to parse definition responses
pub fn parse_definitions(response: Value) -> Vec<DefinitionInfo> {
    if let Some(content) = response.get("content") {
        if let Some(text) = content[0].get("text") {
            if let Ok(defs) = serde_json::from_str::<Vec<Value>>(text.as_str().unwrap_or("[]")) {
                return defs
                    .into_iter()
                    .filter_map(|d| {
                        let uri = d.get("targetUri")?.as_str()?;
                        let name = uri.split('/').last()?.to_string();
                        Some(DefinitionInfo {
                            name,
                            line: d.get("targetRange")?.get("start")?.get("line")?.as_u64()? as u32,
                        })
                    })
                    .collect();
            }
        }
    }
    vec![]
}

#[derive(Debug, Clone)]
pub struct DefinitionInfo {
    pub name: String,
    pub line: u32,
}

/// Test-specific timeout for operations
pub const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
