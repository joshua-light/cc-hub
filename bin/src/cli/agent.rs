//! `cc-hub agent ...` — persistent agents (`lib/src/harness/`).
//!
//! User-facing: `list`, `new`, `once`, `poke`, `pause`, `resume`, `reset`,
//! `show`. Agent-facing (run from inside a tick, which sets
//! `CC_HUB_AGENT`): `note`. Flags are parsed locally — the shared `Flags`
//! is orchestrator-shaped and none of its fields fit.

use super::{print_json, CliError};
use cc_hub_lib::harness::{self, spec, AgentSnapshot, Event};
use std::collections::BTreeMap;
use std::path::PathBuf;

const VERBS: &str = "`list`, `new`, `once`, `poke`, `pause`, `resume`, `reset`, `show`, or `note`";

pub(crate) fn agent_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage(format!("agent <verb>: missing verb (try {})", VERBS)))?;
    match verb.as_str() {
        "list" => list(rest),
        "new" => new(rest),
        "once" => once(rest),
        "poke" => poke(rest),
        "pause" => set_paused(rest, true),
        "resume" => set_paused(rest, false),
        "reset" => reset(rest),
        "show" => show(rest),
        "note" => note(rest),
        other => Err(CliError::Usage(format!(
            "unknown agent verb: {} (try {})",
            other, VERBS
        ))),
    }
}

/// `--key value` pairs plus bare positionals. `--flag` with no value (or
/// followed by another `--`) is a boolean.
struct Args {
    kv: BTreeMap<String, String>,
    positional: Vec<String>,
}

fn parse(args: &[String]) -> Result<Args, CliError> {
    let mut kv = BTreeMap::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(key) = a.strip_prefix("--") {
            let (key, inline) = match key.split_once('=') {
                Some((k, v)) => (k, Some(v.to_string())),
                None => (key, None),
            };
            let value = match inline {
                Some(v) => v,
                None if i + 1 < args.len() && !args[i + 1].starts_with("--") => {
                    i += 1;
                    args[i].clone()
                }
                None => "true".into(),
            };
            kv.insert(key.to_string(), value);
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }
    Ok(Args { kv, positional })
}

impl Args {
    fn get(&self, k: &str) -> Option<&str> {
        self.kv.get(k).map(String::as_str)
    }
    fn flag(&self, k: &str) -> bool {
        self.kv.contains_key(k)
    }
}

/// The agent named positionally, by `--agent`, or by `CC_HUB_AGENT` (set
/// inside a tick).
fn resolve_dir(a: &Args) -> Result<(String, PathBuf), CliError> {
    let name = a
        .positional
        .first()
        .cloned()
        .or_else(|| a.get("agent").map(str::to_string))
        .or_else(|| std::env::var("CC_HUB_AGENT").ok())
        .ok_or_else(|| {
            CliError::Usage("agent name required (positional, --agent, or CC_HUB_AGENT)".into())
        })?;
    if !harness::valid_name(&name) {
        return Err(CliError::Usage(format!("invalid agent name {:?}", name)));
    }
    let dir = harness::agent_dir(&name).ok_or_else(|| CliError::Other("no home dir".into()))?;
    if !dir.join(spec::SPEC_FILE).is_file() {
        return Err(CliError::NotFound(format!(
            "no agent {:?} ({} missing)",
            name,
            dir.join(spec::SPEC_FILE).display()
        )));
    }
    Ok((name, dir))
}

fn snapshot_json(a: &AgentSnapshot) -> serde_json::Value {
    let st = &a.state;
    serde_json::json!({
        "name": a.name,
        "dir": a.dir,
        "status": a.status().label(),
        "spec_error": a.spec.as_ref().err(),
        "trigger": a.spec.as_ref().ok().map(|s| s.trigger_label()),
        "enabled": a.spec.as_ref().map(|s| s.enabled).unwrap_or(false),
        "paused": st.paused,
        "stopped_reason": st.stopped_reason,
        "ticks": st.ticks,
        "cost_usd": st.cost_usd,
        "today_cost_usd": st.today_cost(),
        "today_ticks": st.today_ticks(),
        "last_tick_at": st.last_tick_at,
        "last_result": st.last_result,
        "inbox_pending": a.inbox_pending,
        "notes": a.notes.len(),
    })
}

fn list(args: &[String]) -> Result<(), CliError> {
    let a = parse(args)?;
    let agents = harness::scan();
    if a.flag("json") {
        print_json(&serde_json::json!({
            "ok": true,
            "root": harness::root(),
            "agents": agents.iter().map(snapshot_json).collect::<Vec<_>>(),
        }));
        return Ok(());
    }
    if agents.is_empty() {
        println!(
            "no agents under {} (try `cc-hub agent new <name>`)",
            harness::root()
                .map(|r| r.display().to_string())
                .unwrap_or_default()
        );
        return Ok(());
    }
    for a in &agents {
        let trig = a
            .spec
            .as_ref()
            .map(|s| s.trigger_label())
            .unwrap_or_else(|_| "?".into());
        let extra = a
            .state
            .stopped_reason
            .clone()
            .or_else(|| a.spec.as_ref().err().cloned())
            .unwrap_or_default();
        println!(
            "{}\t{}\t{}\tticks={}\ttoday=${:.2}\ttotal=${:.2}\t{}",
            a.name,
            a.status().label(),
            trig,
            a.state.ticks,
            a.state.today_cost(),
            a.state.cost_usd,
            extra
        );
    }
    Ok(())
}

fn show(args: &[String]) -> Result<(), CliError> {
    let a = parse(args)?;
    let (_, dir) = resolve_dir(&a)?;
    let snap = harness::snapshot(&dir);
    let mut v = snapshot_json(&snap);
    v["ok"] = serde_json::Value::Bool(true);
    v["history"] = serde_json::to_value(&snap.state.history).unwrap_or_default();
    v["recent_notes"] = serde_json::to_value(&snap.notes).unwrap_or_default();
    print_json(&v);
    Ok(())
}

fn new(args: &[String]) -> Result<(), CliError> {
    let a = parse(args)?;
    let name = a
        .positional
        .first()
        .ok_or_else(|| CliError::Usage("agent new <name> [--from DIR]".into()))?;
    let from = a.get("from").map(PathBuf::from);
    let dir = harness::scaffold(name, from.as_deref()).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::InvalidInput => {
            CliError::Usage(e.to_string())
        }
        _ => CliError::Other(e.to_string()),
    })?;
    print_json(&serde_json::json!({
        "ok": true,
        "name": name,
        "dir": dir,
        "spec": dir.join(spec::SPEC_FILE),
        "next": "edit the spec, then `cc-hub agent once <name> --event 'hello'` to try a tick",
    }));
    eprintln!(
        "created {} — edit {} then run `cc-hub agent once {}`",
        dir.display(),
        dir.join(spec::SPEC_FILE).display(),
        name
    );
    Ok(())
}

/// One tick, synchronously, for iterating on a spec. Ignores `enabled`,
/// `paused` and halts; honours budgets unless `--force`.
fn once(args: &[String]) -> Result<(), CliError> {
    let a = parse(args)?;
    let (name, dir) = resolve_dir(&a)?;
    let spec = spec::load(&dir).map_err(CliError::Usage)?;
    let state = harness::load_state(&dir);
    if !a.flag("force") {
        if let Some(block) = harness::budget_block(&spec, &state) {
            return Err(CliError::Conflict {
                msg: format!("{}: {}", name, block),
                recipe: Some(
                    "pass --force to run anyway, or raise the budget in agent.toml".into(),
                ),
            });
        }
    }
    let payload = match (a.get("event"), a.get("event-file")) {
        (Some(e), _) => Some(e.to_string()),
        (None, Some(f)) => {
            Some(std::fs::read_to_string(f).map_err(|e| CliError::Usage(format!("{}: {}", f, e)))?)
        }
        (None, None) => None,
    };
    let event = payload.map(|p| Event::synthetic("once", p, "once"));
    eprintln!(
        "[{}] running one tick (model={}, tools={})…",
        name,
        spec.run.model.as_deref().unwrap_or("default"),
        spec.run.tools.join(" ")
    );
    let (tick, state) =
        harness::tick_once(&spec, event.as_ref()).map_err(|e| CliError::Other(e.to_string()))?;
    print_json(&serde_json::json!({
        "ok": tick.ok,
        "agent": name,
        "tick": state.ticks,
        "subtype": tick.subtype,
        "turns": tick.turns,
        "compactions": tick.compactions,
        "cost_usd": tick.cost_usd,
        "context_start": tick.context_start,
        "context_end": tick.context_end,
        "duration_s": tick.duration_s,
        "session_id": tick.session_id,
        "result": tick.result,
        "stderr": if tick.ok { serde_json::Value::Null } else { serde_json::Value::String(tick.stderr) },
        "log": harness::log_path(&dir),
    }));
    if tick.ok {
        Ok(())
    } else {
        Err(CliError::Reported(format!(
            "tick failed: {}",
            tick.subtype.unwrap_or_default()
        )))
    }
}

fn poke(args: &[String]) -> Result<(), CliError> {
    let a = parse(args)?;
    let (name, dir) = resolve_dir(&a)?;
    let payload = match (a.get("event"), a.get("event-file")) {
        (Some(e), _) => e.to_string(),
        (None, Some(f)) => {
            std::fs::read_to_string(f).map_err(|e| CliError::Usage(format!("{}: {}", f, e)))?
        }
        (None, None) => String::new(),
    };
    let id = harness::poke(&dir, &payload).map_err(|e| CliError::Other(e.to_string()))?;
    print_json(
        &serde_json::json!({ "ok": true, "agent": name, "event": id, "inbox": harness::inbox_path(&dir) }),
    );
    Ok(())
}

fn set_paused(args: &[String], paused: bool) -> Result<(), CliError> {
    let a = parse(args)?;
    let (name, dir) = resolve_dir(&a)?;
    let st = harness::set_paused(&dir, paused).map_err(|e| CliError::Other(e.to_string()))?;
    print_json(
        &serde_json::json!({ "ok": true, "agent": name, "paused": st.paused, "stopped_reason": st.stopped_reason }),
    );
    Ok(())
}

fn reset(args: &[String]) -> Result<(), CliError> {
    let a = parse(args)?;
    let (name, dir) = resolve_dir(&a)?;
    harness::reset(&dir).map_err(|e| CliError::Other(e.to_string()))?;
    print_json(&serde_json::json!({ "ok": true, "agent": name }));
    Ok(())
}

/// `cc-hub agent note --text "..." [--level warn] [--ref URL]`. Run by
/// agents; the name comes from `CC_HUB_AGENT`.
fn note(args: &[String]) -> Result<(), CliError> {
    let a = parse(args)?;
    let (name, dir) = resolve_dir(&a)?;
    let text = a
        .get("text")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| CliError::Usage("agent note --text TEXT".into()))?;
    let level = a.get("level").unwrap_or("info");
    if !matches!(level, "info" | "warn") {
        return Err(CliError::Usage("--level must be info or warn".into()));
    }
    let tick = harness::load_state(&dir).ticks + 1;
    let n = harness::Note {
        at: harness::now_unix(),
        level: level.into(),
        text: text.into(),
        r#ref: a.get("ref").map(str::to_string),
        tick,
    };
    harness::append_note(&dir, &n).map_err(|e| CliError::Other(e.to_string()))?;
    print_json(&serde_json::json!({ "ok": true, "agent": name, "tick": tick }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_mixes_positionals_and_flags() {
        let a = parse(&s(&[
            "bb-prs",
            "--event",
            "hi there",
            "--json",
            "--level=warn",
        ]))
        .unwrap();
        assert_eq!(a.positional, vec!["bb-prs"]);
        assert_eq!(a.get("event"), Some("hi there"));
        assert!(a.flag("json"));
        assert_eq!(a.get("level"), Some("warn"));
    }

    #[test]
    fn note_requires_text() {
        let err = note(&s(&["--agent", "nope"])).unwrap_err();
        assert!(matches!(err, CliError::NotFound(_)), "{err:?}");
    }
}
