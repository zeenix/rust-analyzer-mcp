//! The command line this server is started with.

use anyhow::{bail, Result};
use std::{ffi::OsString, path::PathBuf};

use crate::settings::Settings;

pub const USAGE: &str = "\
MCP server for rust-analyzer integration.

Usage: rust-analyzer-mcp [OPTIONS] [--] [WORKSPACE]

Arguments:
  [WORKSPACE]  Path to the workspace root [default: current directory]

Options:
      --all-features         Analyse and check with every cargo feature enabled
      --features <LIST>      Cargo features to enable, comma- or space-separated. May be given
                             more than once
      --no-default-features  Leave the manifest's default features out
      --config <KEY=VALUE>   Set any rust-analyzer setting, such as --config check.command=clippy.
                             VALUE is read as JSON, or taken for a string if it is not JSON. May
                             be given more than once
  -h, --help                 Print help
  -V, --version              Print version
";

/// What a command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Help,
    Version,
    Serve {
        /// The workspace to analyse, if the command line named one.
        workspace: Option<PathBuf>,
        settings: Settings,
    },
}

/// Reads `args`, which are the arguments after the program's own name.
///
/// Takes them as [`OsString`]s so that a workspace path that is not valid UTF-8 is passed
/// through rather than rejected out of hand.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action> {
    let mut args = args.into_iter();
    let mut workspace = None;
    let mut settings = Settings::default();
    let mut options_ended = false;

    while let Some(arg) = args.next() {
        // A lone "--" ends the options, which is how a workspace path that starts with '-' gets
        // through.
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }

        // Only an option can be required to be UTF-8; a path is whatever the filesystem says.
        let option = (!options_ended)
            .then(|| arg.to_str())
            .flatten()
            .filter(|arg| arg.starts_with('-'));
        if let Some(option) = option {
            // Both `--features a,b` and `--features=a,b` are how people write these.
            let (name, inline) = match option.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (option, None),
            };

            match name {
                "-h" | "--help" => return Ok(Action::Help),
                "-V" | "--version" => return Ok(Action::Version),
                "--all-features" => settings.enable_all_features()?,
                "--no-default-features" => settings.disable_default_features()?,
                "--features" => settings.enable_features(&value(name, inline, &mut args)?)?,
                "--config" => settings.set(&value(name, inline, &mut args)?)?,
                _ => bail!("unknown option '{option}'\n\n{USAGE}"),
            }
            continue;
        }

        if workspace.replace(PathBuf::from(&arg)).is_some() {
            bail!(
                "unexpected extra argument '{}'\n\n{USAGE}",
                arg.to_string_lossy()
            );
        }
    }

    Ok(Action::Serve {
        workspace,
        settings,
    })
}

/// The value of an option, whether it was written after an `=` or as the next argument.
fn value(
    name: &str,
    inline: Option<String>,
    args: &mut impl Iterator<Item = OsString>,
) -> Result<String> {
    if let Some(value) = inline {
        return Ok(value);
    }

    let Some(value) = args.next() else {
        bail!("{name} needs a value\n\n{USAGE}");
    };
    let Some(value) = value.to_str() else {
        bail!("{name}'s value is not valid UTF-8");
    };

    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nothing_at_all_serves_the_current_directory() {
        assert_eq!(
            parse([]).unwrap(),
            Action::Serve {
                workspace: None,
                settings: Settings::default()
            }
        );
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        for asked in ["-h", "--help"] {
            assert_eq!(parse(args(&[asked, "/ws"])).unwrap(), Action::Help);
        }
        for asked in ["-V", "--version"] {
            assert_eq!(parse(args(&[asked])).unwrap(), Action::Version);
        }
    }

    #[test]
    fn the_workspace_is_whichever_argument_is_not_an_option() {
        let Action::Serve { workspace, .. } = parse(args(&["--all-features", "/ws"])).unwrap()
        else {
            panic!("expected a workspace");
        };

        assert_eq!(workspace, Some(PathBuf::from("/ws")));
    }

    #[test]
    fn a_workspace_can_be_named_after_the_options_end() {
        let Action::Serve { workspace, .. } = parse(args(&["--", "-weird-name"])).unwrap() else {
            panic!("expected a workspace");
        };

        assert_eq!(workspace, Some(PathBuf::from("-weird-name")));
    }

    #[test]
    fn features_are_taken_either_way_round() {
        for spelling in [
            &["--features", "serde,tokio"][..],
            &["--features=serde,tokio"][..],
            &["--features", "serde", "--features", "tokio"][..],
            &["--features", "serde tokio"][..],
        ] {
            assert_eq!(
                settings_of(parse(args(spelling)).unwrap())["cargo"]["features"],
                json!(["serde", "tokio"]),
                "{spelling:?}"
            );
        }
    }

    #[test]
    fn every_feature_can_be_asked_for() {
        let settings = settings_of(parse(args(&["--all-features"])).unwrap());

        assert_eq!(settings["cargo"]["features"], json!("all"));
    }

    #[test]
    fn defaults_can_be_left_out() {
        let settings = settings_of(parse(args(&["--no-default-features"])).unwrap());

        assert_eq!(settings["cargo"]["noDefaultFeatures"], json!(true));
    }

    #[test]
    fn any_setting_can_be_named() {
        let settings = settings_of(
            parse(args(&[
                "--config",
                "check.command=clippy",
                "--config=cargo.allTargets=false",
            ]))
            .unwrap(),
        );

        assert_eq!(settings["check"]["command"], json!("clippy"));
        assert_eq!(settings["cargo"]["allTargets"], json!(false));
    }

    #[test]
    fn a_command_line_that_makes_no_sense_says_so() {
        for nonsense in [
            &["--all-features", "--features", "serde"][..],
            &["--features"][..],
            &["--config"][..],
            &["--config", "nonsense"][..],
            &["--nope"][..],
            &["/ws", "/other"][..],
        ] {
            assert!(parse(args(nonsense)).is_err(), "{nonsense:?}");
        }
    }

    #[test]
    fn an_option_after_the_options_ended_is_a_path() {
        let Action::Serve { workspace, .. } = parse(args(&["--", "--all-features"])).unwrap()
        else {
            panic!("expected a workspace");
        };

        assert_eq!(workspace, Some(PathBuf::from("--all-features")));
    }

    fn args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn settings_of(action: Action) -> serde_json::Value {
        let Action::Serve { settings, .. } = action else {
            panic!("expected a command line asking to serve");
        };

        settings.to_json()
    }
}
