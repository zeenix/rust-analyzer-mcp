pub mod types;
pub mod utils;

// Include modules for testing diagnostics
// These are intentionally not behind cfg(test) so they're always analyzed
mod diagnostics_test;
mod simple_error;

pub use types::Config;
pub use utils::process;

/// Main library entry point.
pub fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    process(&config)?;
    Ok(())
}

// Additional test code with issues that should trigger code actions

#[allow(dead_code)]
pub struct Person {
    name: String,
    age: u32,
}

impl Person {
    pub fn new(name: String, age: u32) -> Self {
        Person { name, age }
    }

    // This function could use `&self` instead of `&mut self`
    pub fn get_name(&mut self) -> &str {
        &self.name
    }

    // Unused variable that could trigger a warning
    pub fn birthday(&mut self) {
        let old_age = self.age;
        self.age += 1;
    }
}

// Function that could be made const
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

// Struct that could derive Debug, Clone, etc.
pub struct Point {
    x: f64,
    y: f64,
}

// Function with unnecessary mut
pub fn process_value(mut value: i32) -> i32 {
    value * 2
}

// Code that could use if-let
pub fn handle_option(opt: Option<String>) {
    match opt {
        Some(s) => println!("{}", s),
        None => {}
    }
}
