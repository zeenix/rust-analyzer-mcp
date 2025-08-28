use anyhow::Result;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Represents an isolated test project with its own temporary directory.
pub struct IsolatedProject {
    /// The temporary directory containing the project copy.
    _temp_dir: TempDir,
    /// Path to the project root.
    project_path: PathBuf,
}

impl IsolatedProject {
    /// Create a new isolated test project by copying the test-project to a temp directory.
    pub fn new() -> Result<Self> {
        Self::new_from_source("test-project")
    }

    /// Create a new isolated diagnostic test project by copying test-project-diagnostics.
    pub fn new_diagnostics() -> Result<Self> {
        Self::new_from_source("test-project-diagnostics")
    }

    /// Create an isolated project from a specific source directory.
    fn new_from_source(source_dir: &str) -> Result<Self> {
        let temp_dir = TempDir::new()?;
        let project_path = temp_dir.path().to_path_buf();

        // Get the source test-project path - handle both test-support and root manifest dirs.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source_path = if manifest_dir.ends_with("test-support") {
            manifest_dir.parent().unwrap().join(source_dir)
        } else {
            manifest_dir.join(source_dir)
        };

        if !source_path.exists() {
            return Err(anyhow::anyhow!(
                "{} not found at: {}",
                source_dir,
                source_path.display()
            ));
        }

        // Copy the entire test-project directory recursively.
        copy_dir_all(&source_path, &project_path)?;

        // Verify the copy was complete and log what files are in src/
        let src_dir = project_path.join("src");
        if !src_dir.exists() {
            return Err(anyhow::anyhow!(
                "src/ directory not created in isolated project"
            ));
        }

        let entries = std::fs::read_dir(&src_dir)?;
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        files.sort(); // Sort for consistent ordering

        eprintln!(
            "[IsolatedProject] Created from {} with src files: {:?}",
            source_dir, files
        );

        // Verify critical files exist
        if source_dir == "test-project" {
            let required_files = ["lib.rs", "types.rs", "utils.rs"];
            for file in required_files {
                let file_path = src_dir.join(file);
                if !file_path.exists() {
                    return Err(anyhow::anyhow!(
                        "Required file {} missing after copy. Found files: {:?}",
                        file,
                        files
                    ));
                }
            }
        }

        Ok(Self {
            _temp_dir: temp_dir,
            project_path,
        })
    }

    /// Get the path to the isolated project.
    pub fn path(&self) -> &Path {
        &self.project_path
    }

    /// Get a path to a file within the project.
    pub fn file_path(&self, relative_path: &str) -> PathBuf {
        self.project_path.join(relative_path)
    }
}

/// Recursively copy a directory and all its contents.
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    use std::fs;

    fs::create_dir_all(dst)?;

    // Collect all entries first to ensure we don't miss any
    let entries: Vec<_> = fs::read_dir(src)?.collect::<Result<Vec<_>, _>>()?;

    eprintln!(
        "[copy_dir_all] Copying {} entries from {:?} to {:?}",
        entries.len(),
        src,
        dst
    );

    for entry in entries {
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let file_name = entry.file_name();
        let dst_path = dst.join(&file_name);

        if ty.is_dir() {
            // Skip target directory to avoid copying build artifacts.
            if file_name != "target" {
                copy_dir_all(&src_path, &dst_path)?;
            }
        } else {
            fs::copy(&src_path, &dst_path)?;
            // Verify the file was copied
            if !dst_path.exists() {
                return Err(anyhow::anyhow!(
                    "Failed to copy file {:?} to {:?}",
                    src_path,
                    dst_path
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_isolated_project_creation() -> Result<()> {
        let project = IsolatedProject::new()?;

        // Check that basic files exist.
        assert!(project.file_path("Cargo.toml").exists());
        assert!(project.file_path("src/lib.rs").exists());
        assert!(project.file_path("src/main.rs").exists());

        Ok(())
    }

    #[test]
    fn test_multiple_isolated_projects() -> Result<()> {
        // Create multiple isolated projects - they should not interfere.
        let project1 = IsolatedProject::new()?;
        let project2 = IsolatedProject::new()?;

        // Paths should be different.
        assert_ne!(project1.path(), project2.path());

        // Both should have their own copies of files.
        assert!(project1.file_path("Cargo.toml").exists());
        assert!(project2.file_path("Cargo.toml").exists());

        Ok(())
    }
}
