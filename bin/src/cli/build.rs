//! `cc-hub build …` — the Builds tab from a script or an agent session.
//! Everything a key on the tab does, a verb here does too, through the same
//! `cc_hub_lib::builds` calls. The `_run` and `_hold` verbs are the
//! detached processes those calls start, not for typing.

use super::{print_json, CliError};
use cc_hub_lib::builds::{self, hold, recipe, runner, Build, BuildStatus};
use std::time::Duration;

pub(crate) fn build(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("build <run|start|list|cancel|reserve|release>".into()))?;
    let flags = Flags::parse(rest)?;
    match verb.as_str() {
        "run" => {
            let build = builds::run(&recipe_name(flags.recipe.clone())?).map_err(other)?;
            finish(build, flags.wait)
        }
        "start" => start(flags),
        "list" => {
            print_json(&serde_json::json!({ "ok": true, "builds": builds::all() }));
            Ok(())
        }
        "cancel" => {
            let build = builds::cancel(&flags.build()?).map_err(other)?;
            print_json(&serde_json::json!({ "ok": true, "build": build }));
            Ok(())
        }
        "reserve" => {
            let mut holds = Vec::new();
            for resource in resources(flags.resource) {
                holds.push(hold::ensure(&resource).map_err(other)?);
            }
            print_json(&serde_json::json!({ "ok": true, "holds": holds }));
            Ok(())
        }
        "release" => {
            let resources = resources(flags.resource);
            let mut released = Vec::new();
            for resource in resources {
                if hold::release(&resource).map_err(other)? {
                    released.push(resource);
                }
            }
            print_json(&serde_json::json!({ "ok": true, "released": released }));
            Ok(())
        }
        "_run" => {
            runner::run(flags.positional()?).map_err(other)?;
            Ok(())
        }
        "_hold" => hold::keep(flags.positional()?).map_err(other),
        other => Err(CliError::Usage(format!("unknown build verb: {}", other))),
    }
}

/// `--recipe`, which may be left out when there is only one.
fn recipe_name(named: Option<String>) -> Result<String, CliError> {
    if let Some(name) = named {
        return Ok(name);
    }
    let names: Vec<&str> = recipe::all().map(|(name, _)| name).collect();
    match names.as_slice() {
        [only] => Ok(only.to_string()),
        [] => Err(CliError::Usage("no [builds.recipes] in config.toml".into())),
        many => Err(CliError::Usage(format!(
            "--recipe is one of {}",
            many.join(", ")
        ))),
    }
}

fn start(flags: Flags) -> Result<(), CliError> {
    let recipe = recipe_name(flags.recipe)?;
    let cwd = match flags.cwd {
        Some(dir) => dir,
        None => std::env::current_dir()
            .map_err(other)?
            .display()
            .to_string(),
    };
    let build = Build::new(&recipe, &cwd, flags.r#ref, flags.route);
    let build = builds::start(build).map_err(|e| CliError::Usage(e.to_string()))?;
    finish(build, flags.wait)
}

/// Print the build now, or once it has finished when `--wait` asks for that.
/// A wait that ends in anything but success exits non-zero.
fn finish(build: Build, wait: bool) -> Result<(), CliError> {
    if !wait {
        print_json(&serde_json::json!({ "ok": true, "build": build }));
        return Ok(());
    }
    let id = build.id;
    loop {
        let build = builds::load(&id).map_err(other)?;
        if build.status.is_finished() {
            let ok = build.status == BuildStatus::Succeeded;
            let output = builds::output_path(&id).map_err(other)?;
            print_json(&serde_json::json!({ "ok": ok, "build": build, "output": output }));
            return if ok {
                Ok(())
            } else {
                Err(CliError::Reported(format!(
                    "build {}",
                    build.status.label()
                )))
            };
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// `--resource`, else every resource a recipe names.
fn resources(named: Option<String>) -> Vec<String> {
    match named {
        Some(r) => vec![r],
        None => recipe::all()
            .filter_map(|(_, r)| r.resource.clone())
            .collect(),
    }
}

fn other(e: impl std::fmt::Display) -> CliError {
    CliError::Other(e.to_string())
}

#[derive(Default)]
struct Flags {
    recipe: Option<String>,
    cwd: Option<String>,
    r#ref: Option<String>,
    route: Option<String>,
    build: Option<String>,
    resource: Option<String>,
    wait: bool,
    positional: Option<String>,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Flags, CliError> {
        let mut f = Flags::default();
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let mut value = |name: &str| {
                it.next()
                    .filter(|v| !v.starts_with("--"))
                    .cloned()
                    .ok_or_else(|| CliError::Usage(format!("{} requires a value", name)))
            };
            match arg.as_str() {
                "--recipe" => f.recipe = Some(value("--recipe")?),
                "--cwd" => f.cwd = Some(value("--cwd")?),
                "--ref" => f.r#ref = Some(value("--ref")?),
                "--route" => f.route = Some(value("--route")?),
                "--build" => f.build = Some(value("--build")?),
                "--resource" => f.resource = Some(value("--resource")?),
                "--wait" => f.wait = true,
                "--json" => {}
                flag if flag.starts_with("--") => {
                    return Err(CliError::Usage(format!("unknown flag: {}", flag)))
                }
                word if f.positional.is_none() => f.positional = Some(word.to_string()),
                word => return Err(CliError::Usage(format!("unexpected argument: {}", word))),
            }
        }
        Ok(f)
    }

    fn build(&self) -> Result<String, CliError> {
        self.build
            .clone()
            .or_else(|| self.positional.clone())
            .ok_or_else(|| CliError::Usage("--build <id> is required".into()))
    }

    fn positional(&self) -> Result<&str, CliError> {
        self.positional
            .as_deref()
            .ok_or_else(|| CliError::Usage("missing argument".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_verb_is_required() {
        assert!(matches!(build(&[]), Err(CliError::Usage(_))));
    }

    #[test]
    fn a_flag_without_its_value_is_refused() {
        let err = Flags::parse(&s(&["--ref", "--wait"])).err().unwrap();
        assert!(matches!(err, CliError::Usage(m) if m.contains("--ref")));
    }

    #[test]
    fn flags_read_what_start_needs() {
        let f = Flags::parse(&s(&["--ref", "origin/main", "--route", "swap", "--wait"])).unwrap();
        assert_eq!(f.r#ref.as_deref(), Some("origin/main"));
        assert_eq!(f.route.as_deref(), Some("swap"));
        assert!(f.wait);
    }
}
