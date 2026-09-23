//! The spec fields the Agents tab edits in place. Every write goes through
//! `toml_edit`, so comments and layout in `agent.toml` survive, and through
//! [`spec::parse`] before it lands, so the hub can never save a spec the
//! supervisor would refuse. The supervisor reloads the spec every loop, so a
//! saved change applies on the next run without a restart.

use super::spec::{self, fmt_secs, Spec, TriggerKind, AUTOCOMPACT_FLOOR, PERMISSION_MODES};
use std::path::Path;
use toml_edit::{Array, DocumentMut, Item, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Enabled,
    Model,
    Effort,
    Interval,
    MaxTurns,
    RunBudget,
    DailyBudget,
    TotalBudget,
    Window,
    Session,
    Permissions,
    Tools,
    Mcp,
    Timeout,
    Description,
}

/// Display order: what people change most, first.
pub const SETTINGS: &[Setting] = &[
    Setting::Enabled,
    Setting::Model,
    Setting::Effort,
    Setting::Interval,
    Setting::MaxTurns,
    Setting::RunBudget,
    Setting::DailyBudget,
    Setting::TotalBudget,
    Setting::Window,
    Setting::Session,
    Setting::Permissions,
    Setting::Tools,
    Setting::Mcp,
    Setting::Timeout,
    Setting::Description,
];

/// Enter cycles through these; `e` accepts any model id. `""` is the CLI
/// default (the key is removed).
pub const MODELS: &[&str] = &["", "haiku", "sonnet", "opus"];
pub const EFFORTS: &[&str] = &["", "low", "medium", "high", "xhigh", "max"];

impl Setting {
    pub fn label(self) -> &'static str {
        match self {
            Setting::Enabled => "enabled",
            Setting::Model => "model",
            Setting::Effort => "effort",
            Setting::Interval => "interval",
            Setting::MaxTurns => "max turns",
            Setting::RunBudget => "budget / run",
            Setting::DailyBudget => "budget / day",
            Setting::TotalBudget => "budget total",
            Setting::Window => "context window",
            Setting::Session => "session",
            Setting::Permissions => "permissions",
            Setting::Tools => "tools",
            Setting::Mcp => "MCP servers",
            Setting::Timeout => "run timeout",
            Setting::Description => "description",
        }
    }

    /// One line on what the field does and how Enter changes it.
    pub fn help(self) -> &'static str {
        match self {
            Setting::Enabled => "Off stops new runs; a run in flight finishes. Enter or space flips it.",
            Setting::Model => "Enter cycles haiku → sonnet → opus → default. e types any model id.",
            Setting::Effort => "Enter cycles low → medium → high → xhigh → max → default.",
            Setting::Interval => "How often it polls or fires. Type 90s, 5m or 2h.",
            Setting::MaxTurns => "Caps tool round-trips per run. Type a number; empty means no limit.",
            Setting::RunBudget => "A run stops once it has spent this much. Type dollars, like 0.50.",
            Setting::DailyBudget => "Reaching it halts the agent until tomorrow (UTC). Empty means no limit.",
            Setting::TotalBudget => "Reaching it halts the agent for good. Empty means no limit.",
            Setting::Window => "Context before autocompact kicks in. Type a size like 32k, up to 100k; bigger costs more per turn.",
            Setting::Session => "Fresh starts clean each run. Persistent resumes one session — about 5× the cost.",
            Setting::Permissions => "Enter cycles the CLI's modes. dontAsk denies whatever tools doesn't allow.",
            Setting::Tools => "Comma-separated: Read, Bash(git *), mcp__server__*. Anything else is denied.",
            Setting::Mcp => "On gives the agent this machine's MCP servers, at a startup and schema cost.",
            Setting::Timeout => "Wall-clock cap per run. Type 10m or 1h.",
            Setting::Description => "One line on what the agent is for, shown in the list.",
        }
    }

    /// `(table, key)` in `agent.toml`; `None` is the root table.
    fn path(self) -> (Option<&'static str>, &'static str) {
        match self {
            Setting::Enabled => (None, "enabled"),
            Setting::Description => (None, "description"),
            Setting::Interval => (Some("trigger"), "interval_s"),
            Setting::Model => (Some("run"), "model"),
            Setting::Effort => (Some("run"), "effort"),
            Setting::MaxTurns => (Some("run"), "max_turns"),
            Setting::RunBudget => (Some("run"), "max_budget_usd"),
            Setting::DailyBudget => (Some("run"), "daily_budget_usd"),
            Setting::TotalBudget => (Some("run"), "budget_usd_total"),
            Setting::Window => (Some("run"), "window_pct"),
            Setting::Session => (Some("run"), "persistent_session"),
            Setting::Permissions => (Some("run"), "permission_mode"),
            Setting::Tools => (Some("run"), "tools"),
            Setting::Mcp => (Some("run"), "mcp"),
            Setting::Timeout => (Some("run"), "timeout_s"),
        }
    }

    /// Why this field can't be changed for this spec, if it can't.
    pub fn locked(self, spec: &Spec) -> Option<&'static str> {
        match self {
            Setting::Interval if spec.trigger.kind == TriggerKind::Inbox => {
                Some("an inbox agent wakes on events, not on a clock")
            }
            _ => None,
        }
    }

    /// The value as the Settings list shows it.
    pub fn show(self, spec: &Spec) -> String {
        let run = &spec.run;
        let money = |v: Option<f64>| match v {
            Some(v) => format!("${:.2}", v),
            None => "no limit".into(),
        };
        match self {
            Setting::Enabled => on_off(spec.enabled).into(),
            Setting::Model => run.model.clone().unwrap_or_else(|| "default".into()),
            Setting::Effort => run.effort.clone().unwrap_or_else(|| "default".into()),
            Setting::Interval => match spec.trigger.kind {
                TriggerKind::Inbox => "—".into(),
                TriggerKind::Poll => format!("poll every {}", fmt_secs(spec.trigger.interval_s)),
                TriggerKind::Interval => format!("every {}", fmt_secs(spec.trigger.interval_s)),
            },
            Setting::MaxTurns => run
                .max_turns
                .map(|n| n.to_string())
                .unwrap_or_else(|| "no limit".into()),
            Setting::RunBudget => money(Some(run.max_budget_usd)),
            Setting::DailyBudget => money(run.daily_budget_usd),
            Setting::TotalBudget => money(run.budget_usd_total),
            Setting::Window => format!("~{}k tokens", spec.approx_window_tokens() / 1000),
            Setting::Session => if run.persistent_session {
                "persistent"
            } else {
                "fresh each run"
            }
            .into(),
            Setting::Permissions => run.permission_mode.clone(),
            Setting::Tools => run.tools.join(", "),
            Setting::Mcp => on_off(run.mcp).into(),
            Setting::Timeout => fmt_secs(run.timeout_s),
            Setting::Description => spec.description.clone(),
        }
    }

    /// The value in the grammar [`Self::parse`] reads, to pre-fill the
    /// edit box.
    pub fn raw(self, spec: &Spec) -> String {
        let run = &spec.run;
        let opt_money = |v: Option<f64>| v.map(|v| v.to_string()).unwrap_or_default();
        match self {
            Setting::Model => run.model.clone().unwrap_or_default(),
            Setting::Effort => run.effort.clone().unwrap_or_default(),
            Setting::Interval => fmt_secs(spec.trigger.interval_s),
            Setting::MaxTurns => run.max_turns.map(|n| n.to_string()).unwrap_or_default(),
            Setting::RunBudget => run.max_budget_usd.to_string(),
            Setting::DailyBudget => opt_money(run.daily_budget_usd),
            Setting::TotalBudget => opt_money(run.budget_usd_total),
            Setting::Window => format!("{}k", run.window_pct),
            Setting::Session => if run.persistent_session {
                "persistent"
            } else {
                "fresh"
            }
            .into(),
            Setting::Timeout => fmt_secs(run.timeout_s),
            _ => self.show(spec),
        }
    }

    /// What Enter changes the value to: the flip of a toggle, the next
    /// entry of a short list. `None` means the field needs typing.
    pub fn step(self, spec: &Spec) -> Option<String> {
        let next = |list: &[&str], cur: Option<&str>| {
            let i = list.iter().position(|v| Some(*v) == cur.or(Some("")));
            // A value off the list (a full model id) steps back to the start.
            list[i.map_or(0, |i| (i + 1) % list.len())].to_string()
        };
        match self {
            Setting::Enabled => Some(on_off(!spec.enabled).into()),
            Setting::Mcp => Some(on_off(!spec.run.mcp).into()),
            Setting::Session => Some(
                if spec.run.persistent_session {
                    "fresh"
                } else {
                    "persistent"
                }
                .into(),
            ),
            Setting::Model => Some(next(MODELS, spec.run.model.as_deref())),
            Setting::Effort => Some(next(EFFORTS, spec.run.effort.as_deref())),
            Setting::Permissions => Some(next(
                PERMISSION_MODES,
                Some(spec.run.permission_mode.as_str()),
            )),
            _ => None,
        }
    }

    /// Read typed input. `Ok(None)` removes the key (back to the default).
    pub fn parse(self, input: &str) -> Result<Option<Value>, String> {
        let s = input.trim();
        let cleared = s.is_empty() || s.eq_ignore_ascii_case("none") || s == "default";
        match self {
            Setting::Enabled | Setting::Mcp => parse_bool(s).map(|b| Some(b.into())),
            Setting::Session => match s {
                "persistent" | "on" | "true" => Ok(Some(true.into())),
                "fresh" | "off" | "false" => Ok(Some(false.into())),
                _ => Err("session: fresh or persistent".into()),
            },
            Setting::Description => Ok(Some(s.into())),
            Setting::Model if cleared => Ok(None),
            Setting::Model => Ok(Some(s.into())),
            Setting::Effort if cleared => Ok(None),
            Setting::Effort if EFFORTS.contains(&s) => Ok(Some(s.into())),
            Setting::Effort => Err(format!("effort: one of {}", EFFORTS[1..].join(", "))),
            Setting::Permissions if PERMISSION_MODES.contains(&s) => Ok(Some(s.into())),
            Setting::Permissions => Err(format!(
                "permissions: one of {}",
                PERMISSION_MODES.join(", ")
            )),
            Setting::Interval | Setting::Timeout => parse_secs(s).map(|n| Some((n as i64).into())),
            Setting::MaxTurns if cleared => Ok(None),
            Setting::MaxTurns => match s.parse::<u32>() {
                Ok(n) if n > 0 => Ok(Some((n as i64).into())),
                _ => Err("max turns: a whole number above 0, or empty for no limit".into()),
            },
            Setting::RunBudget => parse_money(s).map(|v| Some(v.into())),
            Setting::DailyBudget | Setting::TotalBudget if cleared => Ok(None),
            Setting::DailyBudget | Setting::TotalBudget => parse_money(s).map(|v| Some(v.into())),
            Setting::Window => {
                let n = s.trim_end_matches(['k', 'K']).trim();
                match n.parse::<u64>() {
                    Ok(k) if (1..=100).contains(&k) => Ok(Some((k as i64).into())),
                    _ => Err(format!("context window: 1k–{}k", AUTOCOMPACT_FLOOR / 1000)),
                }
            }
            Setting::Tools => {
                let mut arr: Array = split_tools(s).into_iter().collect();
                arr.fmt();
                Ok(Some(Value::Array(arr)))
            }
        }
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s {
        "on" | "true" | "yes" => Ok(true),
        "off" | "false" | "no" => Ok(false),
        _ => Err("on or off".into()),
    }
}

/// `90`, `90s`, `5m`, `2h`.
fn parse_secs(s: &str) -> Result<u64, String> {
    let (num, unit) = match s.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => (&s[..i], s[i..].trim()),
        None => (s, ""),
    };
    let n: u64 = num.parse().map_err(|_| "a duration like 90s, 5m or 2h")?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err("a duration like 90s, 5m or 2h".into()),
    };
    if secs == 0 {
        return Err("a duration above zero".into());
    }
    Ok(secs)
}

fn parse_money(s: &str) -> Result<f64, String> {
    match s.trim_start_matches('$').trim().parse::<f64>() {
        Ok(v) if v > 0.0 && v.is_finite() => Ok(v),
        _ => Err("a dollar amount above 0, like 0.50".into()),
    }
}

/// Comma-separated, but a comma inside a scoped rule's parens belongs to
/// the rule: `Read, Bash(git log, git diff)` is two tools.
fn split_tools(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Parse `input` for `setting`, write it into `<dir>/agent.toml`, and
/// return the spec as it now stands. Nothing is written when the input or
/// the resulting spec is invalid.
pub fn apply(dir: &Path, setting: Setting, input: &str) -> Result<Spec, String> {
    let value = setting.parse(input)?;
    let path = dir.join(spec::SPEC_FILE);
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let updated = set_value(&raw, setting, value)?;
    let spec = spec::parse(dir, &updated)?;
    crate::persist::write_atomic(&path, updated.as_bytes())
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    Ok(spec)
}

/// Turn the agent on or off in its spec.
pub fn set_enabled(dir: &Path, on: bool) -> Result<Spec, String> {
    apply(dir, Setting::Enabled, on_off(on))
}

fn set_value(raw: &str, setting: Setting, value: Option<Value>) -> Result<String, String> {
    let mut doc: DocumentMut = raw.parse().map_err(|e| format!("agent.toml: {}", e))?;
    let (table, key) = setting.path();
    let t = match table {
        None => doc.as_table_mut() as &mut dyn toml_edit::TableLike,
        Some(name) => doc
            .entry(name)
            .or_insert(toml_edit::table())
            .as_table_like_mut()
            .ok_or_else(|| format!("agent.toml: [{}] is not a table", name))?,
    };
    match value {
        None => {
            t.remove(key);
        }
        Some(mut v) => match t.get_mut(key).and_then(Item::as_value_mut) {
            // Keep the old value's trailing comment: `window_pct = 32  # ~32k`.
            Some(old) => {
                *v.decor_mut() = old.decor().clone();
                *old = v;
            }
            None => {
                t.insert(key, Item::Value(v));
            }
        },
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"# header comment
description = "Watches PRs"

[trigger]
kind = "poll"
command = "./watch.sh"
interval_s = 300

[run]
tools = ["Read", "Bash(cc-hub agent *)"]
window_pct = 32                  # ~32k context
max_budget_usd = 0.50            # per tick

[prompt]
instruction = "go"
"#;

    fn agent() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("bb-prs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(spec::SPEC_FILE), SPEC).unwrap();
        (tmp, dir)
    }

    fn file(dir: &Path) -> String {
        std::fs::read_to_string(dir.join(spec::SPEC_FILE)).unwrap()
    }

    #[test]
    fn a_new_key_lands_in_its_table_and_comments_survive() {
        let (_tmp, dir) = agent();
        let spec = apply(&dir, Setting::Model, "opus").unwrap();
        assert_eq!(spec.run.model.as_deref(), Some("opus"));
        let raw = file(&dir);
        assert!(raw.contains("# header comment"), "{raw}");
        assert!(raw.contains("# per tick"), "{raw}");
        let run = raw.split("[run]").nth(1).unwrap();
        assert!(run.contains("model = \"opus\""), "{raw}");
    }

    #[test]
    fn replacing_a_value_keeps_its_trailing_comment() {
        let (_tmp, dir) = agent();
        let spec = apply(&dir, Setting::Window, "64k").unwrap();
        assert_eq!(spec.run.window_pct, 64);
        assert!(file(&dir).contains("window_pct = 64                  # ~32k context"));
    }

    #[test]
    fn empty_input_removes_an_optional_key() {
        let (_tmp, dir) = agent();
        apply(&dir, Setting::Model, "sonnet").unwrap();
        let spec = apply(&dir, Setting::Model, "").unwrap();
        assert_eq!(spec.run.model, None);
        assert!(!file(&dir).contains("model"));
    }

    #[test]
    fn invalid_input_writes_nothing() {
        let (_tmp, dir) = agent();
        assert!(apply(&dir, Setting::Permissions, "yolo").is_err());
        assert!(apply(&dir, Setting::RunBudget, "free").is_err());
        assert!(apply(&dir, Setting::Tools, "Frobnicate").is_err());
        assert_eq!(file(&dir), SPEC);
    }

    #[test]
    fn root_keys_stay_above_the_tables() {
        let (_tmp, dir) = agent();
        let spec = set_enabled(&dir, false).unwrap();
        assert!(!spec.enabled);
        let raw = file(&dir);
        assert!(raw.find("enabled = false").unwrap() < raw.find("[trigger]").unwrap());
    }

    #[test]
    fn durations_and_tools_parse() {
        let (_tmp, dir) = agent();
        assert_eq!(
            apply(&dir, Setting::Interval, "2h")
                .unwrap()
                .trigger
                .interval_s,
            7200
        );
        let spec = apply(&dir, Setting::Tools, "Read, Bash(git log, git diff)").unwrap();
        assert_eq!(spec.run.tools, vec!["Read", "Bash(git log, git diff)"]);
        assert!(parse_secs("0").is_err());
        assert!(parse_secs("5d").is_err());
    }

    #[test]
    fn enter_steps_through_models_and_back_to_default() {
        let (_tmp, dir) = agent();
        let mut spec = spec::load(&dir).unwrap();
        let mut seen = Vec::new();
        for _ in 0..MODELS.len() {
            let next = Setting::Model.step(&spec).unwrap();
            spec = apply(&dir, Setting::Model, &next).unwrap();
            seen.push(Setting::Model.show(&spec));
        }
        assert_eq!(seen, vec!["haiku", "sonnet", "opus", "default"]);
    }

    #[test]
    fn raw_values_round_trip_through_parse() {
        let (_tmp, dir) = agent();
        let spec = spec::load(&dir).unwrap();
        for &s in SETTINGS {
            if s.locked(&spec).is_some() {
                continue;
            }
            let again = apply(&dir, s, &s.raw(&spec)).unwrap_or_else(|e| panic!("{s:?}: {e}"));
            assert_eq!(s.show(&again), s.show(&spec), "{s:?}");
        }
    }
}
