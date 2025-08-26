pub mod utils;
pub mod types;

pub use types::Config;
pub use utils::process;

/// Main library entry point.
pub fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    process(&config)?;
    Ok(())
}