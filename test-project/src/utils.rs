use crate::types::Config;

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