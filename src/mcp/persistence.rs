//! Best-effort persistence for the workspace registry. The registry itself
//! is in-memory; this module mirrors the list of workspace roots to disk so
//! that the next server boot can re-register them without the LLM having to
//! call `add_workspace` again.
//!
//! Only the *roots* are persisted — workspace ids (`ws-1`, `ws-2`, …) are
//! re-issued on each boot in registration order. Persisting the ids would
//! lie: if the registry was empty between sessions, an id from a previous
//! session may not exist anymore.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Override env var: when set, takes precedence over `XDG_STATE_HOME` and
/// `$HOME`. Tests use this to redirect persistence into a tempdir.
pub const STATE_DIR_ENV: &str = "RUST_ANALYZER_MCP_STATE_DIR";

const FILE_NAME: &str = "workspaces.json";

/// Resolve the state directory:
/// - `RUST_ANALYZER_MCP_STATE_DIR=""` (set, empty) → `None` (explicitly off).
/// - `RUST_ANALYZER_MCP_STATE_DIR=<path>` → that path.
/// - `XDG_STATE_HOME=<path>` → `<path>/rust-analyzer-mcp`.
/// - else `$HOME/.local/state/rust-analyzer-mcp`.
/// - if none of those resolve → `None`.
pub fn default_state_dir() -> Option<PathBuf> {
    match std::env::var(STATE_DIR_ENV) {
        Ok(s) if s.is_empty() => return None,
        Ok(s) => return Some(PathBuf::from(s)),
        Err(_) => {}
    }
    if let Ok(s) = std::env::var("XDG_STATE_HOME") {
        if !s.is_empty() {
            return Some(PathBuf::from(s).join("rust-analyzer-mcp"));
        }
    }
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty())?;
    Some(PathBuf::from(home).join(".local/state/rust-analyzer-mcp"))
}

#[derive(Serialize, Deserialize)]
struct OnDisk {
    workspaces: Vec<PathBuf>,
}

/// Load the persisted workspace roots. Missing file → empty list. Malformed
/// file → empty list with a tracing warn (we don't want a corrupt JSON to
/// brick boot). Non-existent paths are silently skipped so a deleted workspace
/// directory doesn't surface as a phantom entry.
pub fn load(dir: &Path) -> Vec<PathBuf> {
    let path = dir.join(FILE_NAME);
    let data = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!("Failed to read {}: {}", path.display(), e);
            return Vec::new();
        }
    };
    let parsed: OnDisk = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Malformed persistence file {}: {}", path.display(), e);
            return Vec::new();
        }
    };
    parsed
        .workspaces
        .into_iter()
        .filter(|p| {
            if p.is_dir() {
                true
            } else {
                tracing::warn!(
                    "Persisted workspace {} no longer exists; skipping",
                    p.display()
                );
                false
            }
        })
        .collect()
}

/// Atomically write the workspace roots: write to a sibling tempfile then
/// rename over the target. Creates `dir` if it doesn't exist.
pub fn save(dir: &Path, roots: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let target = dir.join(FILE_NAME);
    let tmp = dir.join(format!("{FILE_NAME}.tmp"));
    let on_disk = OnDisk {
        workspaces: roots.to_vec(),
    };
    let body = serde_json::to_vec_pretty(&on_disk).context("serializing workspaces.json")?;
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("creating tempfile {}", tmp.display()))?;
        f.write_all(&body)
            .with_context(|| format!("writing tempfile {}", tmp.display()))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, &target)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_returns_empty_when_file_missing() {
        let dir = TempDir::new().unwrap();
        assert!(load(dir.path()).is_empty());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        save(dir.path(), &roots).unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded, roots);
    }

    #[test]
    fn load_skips_nonexistent_paths() {
        let dir = TempDir::new().unwrap();
        let real = TempDir::new().unwrap();
        let phantom = PathBuf::from("/this/does/not/exist/at/all");
        save(dir.path(), &[real.path().to_path_buf(), phantom]).unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], real.path());
    }

    #[test]
    fn load_returns_empty_on_malformed_file() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(dir.path().join(FILE_NAME), "{not valid json").unwrap();
        assert!(load(dir.path()).is_empty());
    }

    #[test]
    fn save_creates_dir_if_missing() {
        let parent = TempDir::new().unwrap();
        let nested = parent.path().join("nested/state");
        let real = TempDir::new().unwrap();
        save(&nested, &[real.path().to_path_buf()]).unwrap();
        assert!(nested.join(FILE_NAME).is_file());
    }

    #[test]
    fn default_state_dir_honors_override_env() {
        let prev = std::env::var(STATE_DIR_ENV).ok();
        std::env::set_var(STATE_DIR_ENV, "/tmp/ramcp-test-override");
        let got = default_state_dir();
        assert_eq!(got, Some(PathBuf::from("/tmp/ramcp-test-override")));
        match prev {
            Some(v) => std::env::set_var(STATE_DIR_ENV, v),
            None => std::env::remove_var(STATE_DIR_ENV),
        }
    }

    #[test]
    fn default_state_dir_disabled_when_env_empty() {
        let prev = std::env::var(STATE_DIR_ENV).ok();
        std::env::set_var(STATE_DIR_ENV, "");
        assert_eq!(default_state_dir(), None);
        match prev {
            Some(v) => std::env::set_var(STATE_DIR_ENV, v),
            None => std::env::remove_var(STATE_DIR_ENV),
        }
    }
}
