//! A recipe is how a build is done, written down in `config.toml`:
//!
//! ```toml
//! [builds.recipes.build-server]
//! description = "the game server, built on a second machine from a warm cache"
//! checkout = "~/src/game"
//! resource = "build-box"
//! routes = ["swap", "scripts", "full"]
//! build = ["build-server", "{ref}", "{route}"]
//! cancel = ["build-server", "cancel"]
//! serve = ["ssh", "-o", "RemoteCommand=none", "buildbox", "~/env/serve"]
//! current = ["ssh", "-o", "RemoteCommand=none", "buildbox", "cat ~/build/state/built"]
//! ```
//!
//! The hub knows nothing about what a recipe builds. Each command is an argv
//! run in the build's checkout through a login shell, so it finds what the
//! user's terminal finds. `{ref}`, `{route}` and `{commit}` stand for the
//! build's values, and an argument that is only a placeholder with no value is
//! dropped rather than passed empty: `build-server {ref} {route}` with neither
//! is `build-server`.

use serde::Deserialize;
use std::process::Command;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recipe {
    pub description: String,
    /// The checkout a new build starts in.
    pub checkout: Option<String>,
    /// The resource (`~/.cc-hub/resources.toml`) a build and a serve need.
    /// Held from the first build until it is released by hand.
    pub resource: Option<String>,
    /// Routes a build may ask for. Asking for none lets the recipe choose.
    pub routes: Vec<String>,
    /// Builds. Exit 0 is success; `cc-hub: <key> <value>` lines report
    /// progress ([`super::Report`]).
    pub build: Vec<String>,
    /// Stops a running build, including whatever it started elsewhere. Run by
    /// the runner before it ends the build command itself.
    pub cancel: Vec<String>,
    /// Puts a built commit to use.
    pub serve: Vec<String>,
    /// Prints the commit currently built into what `serve` serves.
    pub current: Vec<String>,
}

/// The build's values, as the placeholders read them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Values<'a> {
    pub r#ref: Option<&'a str>,
    pub route: Option<&'a str>,
    pub commit: Option<&'a str>,
}

impl<'a> Values<'a> {
    pub fn of(build: &'a super::Build) -> Self {
        Self {
            r#ref: build.r#ref.as_deref(),
            route: build.route.as_deref(),
            commit: build.commit.as_deref(),
        }
    }
}

pub fn named(name: &str) -> Option<&'static Recipe> {
    crate::config::get().builds.recipes.get(name)
}

/// The recipes by name, in config order (alphabetical).
pub fn all() -> impl Iterator<Item = (&'static str, &'static Recipe)> {
    crate::config::get()
        .builds
        .recipes
        .iter()
        .map(|(name, recipe)| (name.as_str(), recipe))
}

pub fn expand(template: &[String], values: Values) -> Vec<String> {
    let lookup = |key: &str| match key {
        "ref" => Some(values.r#ref),
        "route" => Some(values.route),
        "commit" => Some(values.commit),
        _ => None,
    };
    template
        .iter()
        .filter_map(|arg| {
            let whole = arg
                .strip_prefix('{')
                .and_then(|a| a.strip_suffix('}'))
                .and_then(lookup);
            match whole {
                Some(None) => None,
                Some(Some(value)) => Some(value.to_string()),
                None => Some(
                    ["ref", "route", "commit"]
                        .iter()
                        .fold(arg.clone(), |acc, key| {
                            let value = lookup(key).flatten().unwrap_or("");
                            acc.replace(&format!("{{{}}}", key), value)
                        }),
                ),
            }
        })
        .collect()
}

/// `argv` in `cwd` through the user's login shell, which is where `PATH`
/// learns about `~/.local/bin` and friends.
pub fn command(argv: &[String], cwd: &str) -> Command {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut command = Command::new(shell);
    command
        .args(["-lc", r#"exec "$@""#, "cc-hub-build"])
        .args(argv)
        .current_dir(crate::platform::paths::expand_home(cwd));
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_placeholder_without_a_value_is_dropped() {
        let template = argv(&["build-server", "{ref}", "{route}"]);
        assert_eq!(
            expand(&template, Values::default()),
            argv(&["build-server"])
        );
        let swap = Values {
            route: Some("swap"),
            ..Values::default()
        };
        assert_eq!(expand(&template, swap), argv(&["build-server", "swap"]));
    }

    #[test]
    fn a_placeholder_inside_an_argument_is_substituted() {
        let template = argv(&["ssh", "buildbox", "serve {commit}"]);
        let values = Values {
            commit: Some("1a2b3c4"),
            ..Values::default()
        };
        assert_eq!(
            expand(&template, values),
            argv(&["ssh", "buildbox", "serve 1a2b3c4"])
        );
    }

    #[test]
    fn an_unknown_brace_is_left_alone() {
        let template = argv(&["awk", "{print $1}"]);
        assert_eq!(expand(&template, Values::default()), template);
    }
}
