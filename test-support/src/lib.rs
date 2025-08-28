pub mod isolated_project;
pub mod test_client;
pub mod timeouts;

// Re-export commonly used items
pub use isolated_project::IsolatedProject;
pub use test_client::MCPTestClient;

/// Check if running in CI environment.
pub fn is_ci() -> bool {
    std::env::var("CI").is_ok()
}
