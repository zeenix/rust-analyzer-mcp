use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

#[derive(Debug)]
pub struct TestProject {
    files: HashMap<String, String>,
}

impl TestProject {
    /// Creates a simple test project with basic Rust code
    pub fn simple() -> Self {
        let mut files = HashMap::new();

        // Cargo.toml
        files.insert(
            "Cargo.toml".to_string(),
            r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"

[dependencies]
"#
            .to_string(),
        );

        // src/main.rs with various test scenarios
        files.insert(
            "src/main.rs".to_string(),
            r#"fn main() {
    let message = greet("World");
    println!("{}", message);
    
    let calc = Calculator::new();
    let result = calc.add(2, 3);
    println!("Result: {}", result);
}

fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

struct Calculator {
    value: i32,
}

impl Calculator {
    fn new() -> Self {
        Self { value: 0 }
    }
    
    fn add(&self, a: i32, b: i32) -> i32 {
        a + b
    }
    
    fn multiply(&self, a: i32, b: i32) -> i32 {
        a * b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_greet() {
        assert_eq!(greet("Test"), "Hello, Test!");
    }
    
    #[test]
    fn test_calculator() {
        let calc = Calculator::new();
        assert_eq!(calc.add(2, 3), 5);
        assert_eq!(calc.multiply(3, 4), 12);
    }
}
"#
            .to_string(),
        );

        // src/lib.rs with module structure
        files.insert(
            "src/lib.rs".to_string(),
            r#"pub mod utils;
pub mod types;

pub use types::Config;
pub use utils::process;

/// Main library entry point.
pub fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    process(&config)?;
    Ok(())
}
"#
            .to_string(),
        );

        // src/utils.rs
        files.insert(
            "src/utils.rs".to_string(),
            r#"use crate::types::Config;

/// Processes the given configuration.
pub fn process(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    println!("Processing: {}", config.name);
    validate(config)?;
    Ok(())
}

fn validate(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    if config.name.is_empty() {
        return Err("Name cannot be empty".into());
    }
    Ok(())
}
"#
            .to_string(),
        );

        // src/types.rs
        files.insert(
            "src/types.rs".to_string(),
            r#"/// Configuration structure for the application.
#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub version: u32,
    pub enabled: bool,
}

impl Config {
    pub fn new(name: String) -> Self {
        Self {
            name,
            version: 1,
            enabled: true,
        }
    }
    
    pub fn with_version(mut self, version: u32) -> Self {
        self.version = version;
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new("default".to_string())
    }
}
"#
            .to_string(),
        );

        Self { files }
    }

    /// Creates a project with compilation errors for error handling tests
    pub fn with_errors() -> Self {
        let mut files = HashMap::new();

        files.insert(
            "Cargo.toml".to_string(),
            r#"[package]
name = "test-project-errors"
version = "0.1.0"
edition = "2021"
"#
            .to_string(),
        );

        files.insert(
            "src/main.rs".to_string(),
            r#"fn main() {
    // Missing semicolon
    let x = 5
    
    // Undefined variable
    println!("{}", y);
    
    // Type mismatch
    let s: String = 42;
    
    // Missing function
    undefined_function();
}

fn incomplete_function() -> i32 {
    // Missing return
}
"#
            .to_string(),
        );

        Self { files }
    }

    /// Creates a large project for performance testing
    pub fn large_codebase() -> Self {
        let mut files = HashMap::new();

        files.insert(
            "Cargo.toml".to_string(),
            r#"[package]
name = "test-project-large"
version = "0.1.0"
edition = "2021"
"#
            .to_string(),
        );

        // Generate many modules
        for i in 0..50 {
            let module_name = format!("src/module_{}.rs", i);
            let content = format!(
                r#"
pub struct Module{} {{
    value: i32,
}}

impl Module{} {{
    pub fn new() -> Self {{
        Self {{ value: {} }}
    }}
    
    pub fn process(&self, input: i32) -> i32 {{
        self.value + input
    }}
}}

pub fn function_{}(x: i32, y: i32) -> i32 {{
    x * y + {}
}}

#[cfg(test)]
mod tests {{
    use super::*;
    
    #[test]
    fn test_module_{}() {{
        let m = Module{}::new();
        assert_eq!(m.process(10), {});
    }}
}}
"#,
                i,
                i,
                i,
                i,
                i,
                i,
                i,
                i + 10
            );
            files.insert(module_name, content);
        }

        // Main file that uses all modules
        let mut main_content = String::from("fn main() {\n");
        for i in 0..50 {
            main_content.push_str(&format!("    mod module_{};\n", i));
        }
        main_content.push_str("    println!(\"Large project loaded\");\n}\n");
        files.insert("src/main.rs".to_string(), main_content);

        Self { files }
    }

    /// Creates the project files in the given directory
    pub fn create_in(&self, dir: &Path) -> Result<()> {
        for (path, content) in &self.files {
            let full_path = dir.join(path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(full_path, content)?;
        }
        Ok(())
    }

    /// Creates the project in a new temporary directory
    pub fn create_temp() -> Result<(TempDir, Self)> {
        let dir = TempDir::new()?;
        let project = Self::simple();
        project.create_in(dir.path())?;
        Ok((dir, project))
    }
}
