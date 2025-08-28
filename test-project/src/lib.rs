pub mod types;
pub mod utils;

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

    pub fn birthday(&mut self) {
        self.age += 1;
    }
}

// Function that could be made const
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[derive(Debug, Clone)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub fn process_value(value: i32) -> i32 {
    value * 2
}

// Code that could use if-let
pub fn handle_option(opt: Option<String>) {
    match opt {
        Some(s) => println!("{}", s),
        None => {}
    }
}
