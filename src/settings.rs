//! The configuration rust-analyzer is asked to run with.
//!
//! rust-analyzer has no command line to speak of: everything it can be told is told to it over
//! the LSP, as one JSON object. This is that object -- the settings this server always wants,
//! with whatever the user asked for on top.

use anyhow::{bail, Result};
use serde_json::{json, Value};

/// The cargo features to analyse and check with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Features {
    /// Whatever the manifest makes default.
    #[default]
    Default,
    /// All of them, which is `--all-features`.
    All,
    /// The named ones, in the spellings cargo takes: a bare feature name, or `package/feature`
    /// in a workspace.
    Named(Vec<String>),
}

/// Everything the command line has to say about how rust-analyzer should run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    features: Features,
    no_default_features: bool,
    /// Settings named outright, as a path through the configuration and the value to put there.
    overrides: Vec<(String, Value)>,
}

impl Settings {
    /// Enables every feature.
    pub fn enable_all_features(&mut self) -> Result<()> {
        if self.features != Features::Default {
            bail!("--all-features cannot be combined with --features");
        }

        self.features = Features::All;
        Ok(())
    }

    /// Enables the features `list` names, comma- or space-separated.
    pub fn enable_features(&mut self, list: &str) -> Result<()> {
        let named: Vec<String> = list
            .split([',', ' ', '\t'])
            .filter(|feature| !feature.is_empty())
            .map(str::to_string)
            .collect();
        if named.is_empty() {
            bail!("--features needs at least one feature name");
        }

        match &mut self.features {
            // rust-analyzer takes either every feature or a list, so the two cannot be asked for
            // together: it would go with all of them and say nothing about the rest.
            Features::All => bail!("--features cannot be combined with --all-features"),
            Features::Named(features) => features.extend(named),
            features @ Features::Default => *features = Features::Named(named),
        }
        Ok(())
    }

    /// Leaves the manifest's default features out.
    pub fn disable_default_features(&mut self) -> Result<()> {
        if self.features == Features::All {
            bail!("--no-default-features cannot be combined with --all-features");
        }

        self.no_default_features = true;
        Ok(())
    }

    /// Sets one rust-analyzer setting outright, from a `key.path=value` as it is spelled on the
    /// command line.
    ///
    /// The value is read as JSON, and taken for a string when it is not any other JSON value --
    /// so `check.command=clippy` means what it looks like it means.
    pub fn set(&mut self, assignment: &str) -> Result<()> {
        let Some((key, value)) = assignment.split_once('=') else {
            bail!("--config needs a KEY=VALUE, such as check.command=clippy");
        };
        if key.is_empty() || key.split('.').any(str::is_empty) {
            bail!("'{key}' is not a setting name");
        }

        let value = serde_json::from_str(value).unwrap_or_else(|_| json!(value));
        self.overrides.push((key.to_string(), value));
        Ok(())
    }

    /// The settings as rust-analyzer wants them.
    pub fn to_json(&self) -> Value {
        let mut settings = json!({
            "cargo": {
                "buildScripts": {
                    "enable": true
                }
            },
            "checkOnSave": true,
            "diagnostics": {
                "enable": true,
                "experimental": {
                    "enable": true
                }
            },
            "procMacro": {
                "enable": true
            }
        });

        // `check.*` falls back to `cargo.*` for each of these, so setting them once covers both
        // what rust-analyzer analyses and what cargo is asked to check.
        match &self.features {
            Features::Default => {}
            Features::All => settings["cargo"]["features"] = json!("all"),
            Features::Named(features) => settings["cargo"]["features"] = json!(features),
        }
        if self.no_default_features {
            settings["cargo"]["noDefaultFeatures"] = json!(true);
        }

        for (key, value) in &self.overrides {
            set_at(&mut settings, key, value.clone());
        }

        settings
    }
}

/// Puts `value` at the dotted `key` in `settings`, making whatever objects it takes to get there.
fn set_at(settings: &mut Value, key: &str, value: Value) {
    let mut at = settings;
    let mut names = key.split('.').peekable();

    while let Some(name) = names.next() {
        if names.peek().is_none() {
            at[name] = value;
            return;
        }

        // An override may name settings this server says nothing about, and may equally
        // contradict one it does: either way what the user asked for wins.
        if !at[name].is_object() {
            at[name] = json!({});
        }
        at = &mut at[name];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_leave_features_to_the_manifest() {
        let settings = Settings::default().to_json();

        assert_eq!(settings["cargo"]["features"], Value::Null);
        assert_eq!(settings["cargo"]["noDefaultFeatures"], Value::Null);
    }

    #[test]
    fn all_features_is_the_word_rust_analyzer_knows() {
        let mut settings = Settings::default();
        settings.enable_all_features().unwrap();

        // Lower case, and the bare string rather than a list containing it: a list would name a
        // feature actually called "all".
        assert_eq!(settings.to_json()["cargo"]["features"], json!("all"));
    }

    #[test]
    fn features_are_named_one_by_one() {
        let mut settings = Settings::default();
        settings.enable_features("serde,tokio").unwrap();
        settings.enable_features("other/thing  extra").unwrap();

        assert_eq!(
            settings.to_json()["cargo"]["features"],
            json!(["serde", "tokio", "other/thing", "extra"])
        );
    }

    #[test]
    fn features_that_name_nothing_are_refused() {
        assert!(Settings::default().enable_features("").is_err());
        assert!(Settings::default().enable_features(" , ").is_err());
    }

    #[test]
    fn all_features_cannot_be_narrowed() {
        // rust-analyzer would quietly go with all of them; better to say so.
        let mut settings = Settings::default();
        settings.enable_all_features().unwrap();

        assert!(settings.enable_features("serde").is_err());
        assert!(settings.disable_default_features().is_err());

        let mut settings = Settings::default();
        settings.enable_features("serde").unwrap();
        assert!(settings.enable_all_features().is_err());
    }

    #[test]
    fn default_features_can_be_left_out() {
        let mut settings = Settings::default();
        settings.disable_default_features().unwrap();
        settings.enable_features("serde").unwrap();

        let settings = settings.to_json();
        assert_eq!(settings["cargo"]["noDefaultFeatures"], json!(true));
        assert_eq!(settings["cargo"]["features"], json!(["serde"]));
    }

    #[test]
    fn any_setting_can_be_named_outright() {
        let mut settings = Settings::default();
        settings.set("check.command=clippy").unwrap();
        settings
            .set("cargo.target=x86_64-unknown-linux-gnu")
            .unwrap();
        settings.set("check.extraArgs=[\"--tests\"]").unwrap();
        settings.set("procMacro.enable=false").unwrap();

        let settings = settings.to_json();
        assert_eq!(settings["check"]["command"], json!("clippy"));
        assert_eq!(
            settings["cargo"]["target"],
            json!("x86_64-unknown-linux-gnu")
        );
        assert_eq!(settings["check"]["extraArgs"], json!(["--tests"]));
        // Including one this server has an opinion about.
        assert_eq!(settings["procMacro"]["enable"], json!(false));
    }

    #[test]
    fn an_override_replaces_what_stands_in_its_way() {
        let mut settings = Settings::default();
        settings.set("checkOnSave.enable=true").unwrap();

        // `checkOnSave` is a bare boolean by default, and a path through it has to make it an
        // object rather than sit next to it.
        assert_eq!(settings.to_json()["checkOnSave"], json!({ "enable": true }));
    }

    #[test]
    fn settings_that_name_nothing_are_refused() {
        assert!(Settings::default().set("check.command").is_err());
        assert!(Settings::default().set("=clippy").is_err());
        assert!(Settings::default().set("check..command=clippy").is_err());
    }
}
