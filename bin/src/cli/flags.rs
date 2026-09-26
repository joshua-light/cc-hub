use super::CliError;

#[derive(Default)]
pub(super) struct Flags {
    pub(super) task: Option<String>,
    pub(super) agent: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) wait_secs: Option<u64>,
    pub(super) dry_run: bool,
    /// `board add` flags: the card's text, its tags (one string, split like
    /// the TUI's quick-capture field) and its priority (`p1`..`p4`).
    pub(super) text: Option<String>,
    pub(super) tags: Option<String>,
    pub(super) priority: Option<String>,
    pub(super) title: Option<String>,
    pub(super) json: bool,
}

pub(super) fn parse_flags(args: &[String]) -> Result<Flags, CliError> {
    let mut f = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--task" => {
                f.task = Some(next_value(args, &mut i, "--task")?);
            }
            "--agent" => {
                f.agent = Some(next_value(args, &mut i, "--agent")?);
            }
            "--kind" => {
                f.kind = Some(next_value(args, &mut i, "--kind")?);
            }
            "--wait-secs" => {
                let v = next_value(args, &mut i, "--wait-secs")?;
                f.wait_secs = Some(
                    v.parse()
                        .map_err(|e| CliError::Usage(format!("--wait-secs: {}", e)))?,
                );
            }
            "--dry-run" => {
                f.dry_run = true;
                i += 1;
            }
            "--title" => {
                f.title = Some(next_value(args, &mut i, "--title")?);
            }
            "--text" => {
                f.text = Some(next_value(args, &mut i, "--text")?);
            }
            "--tags" => {
                f.tags = Some(next_value(args, &mut i, "--tags")?);
            }
            "--priority" => {
                f.priority = Some(next_value(args, &mut i, "--priority")?);
            }
            "--json" => {
                f.json = true;
                i += 1;
            }
            other => {
                return Err(CliError::Usage(format!("unknown flag: {}", other)));
            }
        }
    }
    Ok(f)
}

/// Free-text flags whose value may legitimately begin with `--`
/// (e.g. `--text "--wip notes"`). For these we accept the next token
/// verbatim.
const FREE_TEXT_FLAGS: &[&str] = &["--title", "--text"];

fn next_value(args: &[String], i: &mut usize, name: &str) -> Result<String, CliError> {
    *i += 1;
    // For STRUCTURED flags (ids, enums, numbers, paths), a value that looks like
    // another flag means the caller omitted the value (e.g. `--task --json`
    // would otherwise bind task="--json") — reject it. Free-text
    // flags may legitimately take a `--`-prefixed value, so don't filter those.
    let reject_flag_like = !FREE_TEXT_FLAGS.contains(&name);
    let Some(v) = args
        .get(*i)
        .cloned()
        .filter(|v| !(reject_flag_like && v.starts_with("--")))
    else {
        return Err(CliError::Usage(format!("{} requires a value", name)));
    };
    *i += 1;
    Ok(v)
}

/// Boolean flags that take no value — the no-argument arms of [`parse_flags`].
/// Every other recognized `--flag` consumes the following token as its value.
const BOOL_FLAGS: &[&str] = &["--dry-run", "--json"];

/// Flag-aware scan for a help request among a verb's args.
///
/// A naive `args.iter().any(|a| a == "-h" || a == "--help")` misfires when a
/// flag VALUE equals `-h`/`--help` — e.g. `board note --text "-h"` would
/// silently reroute to help and exit 0, which the caller reads as a success
/// no-op. So mirror [`next_value`]'s tokenizer: a value-consuming flag
/// swallows its next token (free-text flags take any token; structured flags
/// reject a `--`-prefixed one, treating it as an omitted value). Only a
/// `-h`/`--help` in true argument position triggers help.
pub(super) fn args_request_help(args: &[String]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if matches!(a, "-h" | "--help") {
            return true;
        }
        // Any `--flag` that isn't a known boolean — including flags this scan
        // doesn't recognize — swallows the following token as its value, so a
        // `-h`-shaped value can't masquerade as a help request. (For unknown
        // flags that means `--bogus -h` surfaces the unknown-flag usage error
        // rather than help — the more informative failure.)
        if a.starts_with("--") && !BOOL_FLAGS.contains(&a) {
            if let Some(next) = args.get(i + 1) {
                if FREE_TEXT_FLAGS.contains(&a) || !next.starts_with("--") {
                    i += 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    false
}

pub(super) fn require_task(f: &Flags) -> Result<String, CliError> {
    f.task
        .clone()
        .ok_or_else(|| CliError::Usage("--task is required".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_scan_ignores_free_text_flag_value() {
        // Regression: `board note --text "-h"` — the `-h` is the note VALUE,
        // not a help request. Rerouting to help here is a silent exit-0 no-op
        // the caller misreads as success.
        assert!(!args_request_help(&[
            "note".to_string(),
            "--text".to_string(),
            "-h".to_string(),
        ]));
        // A `--help`-shaped free-text value is equally inert.
        assert!(!args_request_help(&[
            "note".to_string(),
            "--text".to_string(),
            "--help".to_string(),
        ]));
        // A positional -h / --help still triggers help.
        assert!(args_request_help(&["note".to_string(), "-h".to_string()]));
        assert!(args_request_help(&[
            "note".to_string(),
            "--help".to_string()
        ]));
        // Structured flag consuming a `-h` value stays tokenizer-consistent:
        // the value binds to the flag (parse_flags would too), so no help.
        assert!(!args_request_help(&[
            "--task".to_string(),
            "-h".to_string()
        ]));
    }

    #[test]
    fn next_value_rejects_flag_shaped_value() {
        // `--task --json` must error (missing value for --task) rather than
        // binding task="--json".
        let args = vec!["--task".to_string(), "--json".to_string()];
        match parse_flags(&args) {
            Err(CliError::Usage(msg)) => assert!(
                msg.contains("--task"),
                "expected --task missing-value error, got: {msg}"
            ),
            Err(other) => panic!("expected CliError::Usage, got {other:?}"),
            Ok(f) => panic!("expected --task to error, but parsed task={:?}", f.task),
        }
    }

    #[test]
    fn next_value_accepts_flag_shaped_value_for_free_text_flags() {
        // Free-text flags must accept a `--`-prefixed value verbatim.
        let args = vec!["--text".to_string(), "--foo bar".to_string()];
        match parse_flags(&args) {
            Ok(f) => assert_eq!(f.text.as_deref(), Some("--foo bar")),
            Err(e) => panic!("expected --text to accept '--foo bar', got {e:?}"),
        }
    }
}
