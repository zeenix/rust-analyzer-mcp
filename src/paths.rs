use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use url::Url;

pub fn absolute_path(path: PathBuf) -> PathBuf {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };

    normalize_path(path)
}

pub fn file_uri(path: &Path) -> Result<String> {
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| anyhow!("failed to convert path to file URI: {}", path.display()))
}

pub fn directory_uri(path: &Path) -> Result<String> {
    Url::from_directory_path(path)
        .map(|url| url.to_string())
        .map_err(|_| {
            anyhow!(
                "failed to convert path to directory URI: {}",
                path.display()
            )
        })
}

#[cfg(windows)]
fn normalize_path(path: PathBuf) -> PathBuf {
    let path_str = path.to_string_lossy();

    if let Some(rest) = path_str.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = path_str.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

#[cfg(not(windows))]
fn normalize_path(path: PathBuf) -> PathBuf {
    path
}
