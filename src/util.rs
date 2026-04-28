use std::path::PathBuf;

/// Best-effort path absolutization. `canonicalize` is preferred (resolves
/// symlinks + normalizes); on failure (path doesn't exist yet, permission
/// denied, …) we fall back to "absolute" via `current_dir().join(...)`.
///
/// Used by both the LSP client (workspace root passed to rust-analyzer) and
/// the workspace registry — keep them in lockstep so a `set_workspace` and
/// the freshly-spawned client see the same path.
pub fn canonicalize_path(path: PathBuf) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path)
}
