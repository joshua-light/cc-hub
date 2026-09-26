use super::{agent_dir, inbox_path, spec, trigger, valid_name};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const TEMPLATE_SPEC: &str = r#"# cc-hub persistent agent. Edits apply on the next tick.
description = "What this agent watches and does"
enabled = true
# workdir = "~/somewhere"        # default: <this dir>/work — keep it clean

[trigger]
kind = "inbox"                   # inbox | poll | interval
# command = "./watch.sh"         # poll: non-empty stdout is one event (deduped)
# interval_s = 300               # poll / interval cadence

[run]
tools = ["Read", "Write", "Glob", "Bash(cc-hub agent *)"]
# model = "sonnet"
# effort = "low"
window_pct = 32                  # ~32k context; fresh session each tick
max_turns = 40
max_budget_usd = 0.50            # per tick
daily_budget_usd = 5.0           # then halts until tomorrow

[prompt]
append = """
Describe the agent's job and its state files here. Report findings with
`cc-hub agent note --text "..."`."""

instruction = """
The event below describes one thing that happened.
1. Read state/handled.md (create it if missing). If this event id is listed, reply DONE and stop.
2. Handle it.
3. Append the event id to state/handled.md. Stop."""
"#;

/// Create `<root>/<name>/` with a template spec. Errors if it exists.
pub fn scaffold(name: &str, from: Option<&Path>) -> io::Result<PathBuf> {
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "agent name: letters, digits, - and _ only",
        ));
    }
    let dir = agent_dir(name).ok_or_else(|| io::Error::other("no home dir"))?;
    if dir.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", dir.display()),
        ));
    }
    fs::create_dir_all(dir.join("work"))?;
    trigger::ensure_inbox(&inbox_path(&dir))?;
    match from {
        Some(src) => copy_tree(src, &dir)?,
        None => fs::write(dir.join(spec::SPEC_FILE), TEMPLATE_SPEC)?,
    }
    Ok(dir)
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    for entry in fs::read_dir(src)?.flatten() {
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&to)?;
            copy_tree(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_spec_parses() {
        let s = spec::parse(Path::new("/tmp/agents/demo"), TEMPLATE_SPEC).unwrap();
        assert_eq!(s.name, "demo");
        assert!(s
            .run
            .tools
            .iter()
            .any(|t| t.starts_with("Bash(cc-hub agent")));
    }
}
