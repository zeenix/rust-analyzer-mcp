/// Configuration structure for the application.
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
