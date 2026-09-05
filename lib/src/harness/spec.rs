//! Agent specs: the one TOML file that says everything the supervisor needs
//! to run a persistent agent. The harness knows nothing about what an agent
//! does — only which tools it gets, how much context, what it is told, and
//! what wakes it.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::platform::paths::expand_home;

/// `--autocompact` accepts 100k–1M; `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` scales
/// it down and can only lower the trigger, never raise it. A small window is
/// therefore a percentage of this floor.
pub const AUTOCOMPACT_FLOOR: u64 = 100_000;

/// Replaces Claude Code's preset entirely. The preset carries coding guidance
/// and environment context an unattended worker does not need (~1.8k
/// tokens). Dropping it drops its safety instructions too, so the essentials
/// are restated here.
pub const BASE_SYSTEM_PROMPT: &str = "\
You are an autonomous worker process. You run unattended on a loop; no human is \
watching this turn, and nothing you say reaches a person unless you write it to a \
file or report it with `cc-hub agent note`.

Operating rules:
- Your conversation memory is discarded without warning. Files are the only memory \
that survives. Never rely on recalling what you already did — read your state.
- Do exactly the task you were given. Do not expand scope, do not act outside the \
directories and tools you were given.
- Prefer doing nothing to doing something irreversible. If an action would be hard \
to undo and you were not explicitly told to take it, record it instead of doing it.
- If a tool is unavailable or an action is denied, record that fact and continue. \
Do not work around a restriction.
- Report outcomes truthfully, including failures and work you skipped.";

pub const SPEC_FILE: &str = "agent.toml";

/// `--permission-mode` values. The default, `dontAsk`, denies whatever the
/// tool list did not allow instead of prompting into the void;
/// `bypassPermissions` is the only one that leaves nothing to allow, and it
/// is the only way to reach MCP tools, which take no wildcard allow rule.
pub const PERMISSION_MODES: &[&str] = &[
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "dontAsk",
    "manual",
    "plan",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerKind {
    /// Only the inbox wakes the agent (every agent has one).
    Inbox,
    /// Run `command` every `interval_s`; non-empty stdout is an event.
    Poll,
    /// Fire unconditionally every `interval_s`.
    Interval,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriggerCfg {
    pub kind: TriggerKind,
    /// Poll trigger only. Runs with the agent dir as cwd.
    pub command: Option<String>,
    pub interval_s: u64,
    /// Poll trigger only: skip an event whose payload hashes like the last one.
    pub dedupe: bool,
    /// Poll command timeout.
    pub timeout_s: u64,
}

impl Default for TriggerCfg {
    fn default() -> Self {
        Self {
            kind: TriggerKind::Inbox,
            command: None,
            interval_s: 60,
            dedupe: true,
            timeout_s: 120,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunCfg {
    /// Only these tools exist for the agent; everything else is denied,
    /// which removes its schema from the request. Bare names, scoped rules
    /// (`Bash(git *)`) and MCP patterns (`mcp__srv__*`) all work; a lone
    /// `"*"` asks for everything the CLI offers.
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Percentage of [`AUTOCOMPACT_FLOOR`]: 8 → ~8k, 32 → ~32k.
    pub window_pct: u8,
    pub max_turns: Option<u32>,
    /// Cap for one tick.
    pub max_budget_usd: f64,
    /// Halts the agent for the rest of the day once reached.
    pub daily_budget_usd: Option<f64>,
    /// Halts the agent for good once reached.
    pub budget_usd_total: Option<f64>,
    /// Resume the same session every tick. Measured 5× more expensive than
    /// fresh sessions in the reference harness; off by default.
    pub persistent_session: bool,
    /// `--setting-sources` value. Empty isolates from user/project settings.
    pub setting_sources: String,
    /// One of [`PERMISSION_MODES`]. Anything but `bypassPermissions` needs
    /// every action covered by `tools` or by the loaded settings.
    pub permission_mode: String,
    /// Give the agent the machine's MCP servers, as an interactive session
    /// has them. Off by default: a watcher pays for every server it never
    /// calls, in startup time and in tool schemas.
    pub mcp: bool,
    /// Wall-clock cap for one tick.
    pub timeout_s: u64,
    pub env: BTreeMap<String, String>,
}

impl Default for RunCfg {
    fn default() -> Self {
        Self {
            tools: vec!["Read".into()],
            model: None,
            effort: None,
            window_pct: 32,
            max_turns: None,
            max_budget_usd: 1.0,
            daily_budget_usd: None,
            budget_usd_total: None,
            persistent_session: false,
            setting_sources: String::new(),
            permission_mode: "dontAsk".into(),
            mcp: false,
            timeout_s: 3600,
            env: BTreeMap::new(),
        }
    }
}

/// A prompt fragment given inline or as a path relative to the agent dir.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TextOrFile {
    Text(String),
    File { file: String },
}

impl TextOrFile {
    fn resolve(&self, dir: &Path) -> Result<String, String> {
        match self {
            TextOrFile::Text(s) => Ok(s.clone()),
            TextOrFile::File { file } => {
                let p = dir.join(file);
                std::fs::read_to_string(&p).map_err(|e| format!("{}: {}", p.display(), e))
            }
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PromptCfg {
    /// Replaces [`BASE_SYSTEM_PROMPT`] outright.
    pub system: Option<TextOrFile>,
    /// Appended to the system prompt (base or `system`).
    pub append: Option<TextOrFile>,
    /// Sent every tick, with the event appended (or placed at `{event}`).
    pub instruction: Option<TextOrFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpecFile {
    /// Defaults to the directory name.
    pub name: Option<String>,
    pub description: String,
    pub enabled: bool,
    /// The agent's whole world. Defaults to `<agent dir>/work`. Keep it
    /// clean: an agent with Glob treats anything instruction-shaped in it
    /// as instructions.
    pub workdir: Option<String>,
    pub trigger: TriggerCfg,
    pub run: RunCfg,
    pub prompt: PromptCfg,
}

impl Default for SpecFile {
    fn default() -> Self {
        Self {
            name: None,
            description: String::new(),
            enabled: true,
            workdir: None,
            trigger: TriggerCfg::default(),
            run: RunCfg::default(),
            prompt: PromptCfg::default(),
        }
    }
}

/// A loaded, validated spec with every file reference resolved.
#[derive(Debug, Clone)]
pub struct Spec {
    pub name: String,
    pub dir: PathBuf,
    pub description: String,
    pub enabled: bool,
    pub workdir: PathBuf,
    pub trigger: TriggerCfg,
    pub run: RunCfg,
    pub system_prompt: String,
    pub instruction: String,
}

impl Spec {
    pub fn approx_window_tokens(&self) -> u64 {
        AUTOCOMPACT_FLOOR * self.run.window_pct as u64 / 100
    }

    /// One-line trigger description for cards: `poll 5m`, `every 6h`, `inbox`.
    pub fn trigger_label(&self) -> String {
        match self.trigger.kind {
            TriggerKind::Inbox => "inbox".into(),
            TriggerKind::Poll => format!("poll {}", fmt_secs(self.trigger.interval_s)),
            TriggerKind::Interval => format!("every {}", fmt_secs(self.trigger.interval_s)),
        }
    }
}

pub(crate) fn fmt_secs(s: u64) -> String {
    if s >= 3600 && s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else if s >= 60 && s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{}s", s)
    }
}

/// Load `<dir>/agent.toml`. Errors are strings so the TUI can show a broken
/// spec on its card instead of hiding the agent.
pub fn load(dir: &Path) -> Result<Spec, String> {
    let path = dir.join(SPEC_FILE);
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
    parse(dir, &raw)
}

pub fn parse(dir: &Path, raw: &str) -> Result<Spec, String> {
    let file: SpecFile = toml::from_str(raw).map_err(|e| format!("agent.toml: {}", e))?;
    let name = file
        .name
        .clone()
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .ok_or("agent.toml: cannot derive a name")?;

    if !(1..=100).contains(&file.run.window_pct) {
        return Err("run.window_pct must be 1–100".into());
    }
    if !PERMISSION_MODES.contains(&file.run.permission_mode.as_str()) {
        return Err(format!(
            "run.permission_mode must be one of {}",
            PERMISSION_MODES.join(", ")
        ));
    }
    if file.trigger.kind == TriggerKind::Poll && file.trigger.command.is_none() {
        return Err("trigger.kind = \"poll\" needs trigger.command".into());
    }
    super::tools::scope(&file.run.tools)?;

    let workdir = match &file.workdir {
        Some(w) => expand_home(w),
        None => dir.join("work"),
    };

    let mut system_prompt = match &file.prompt.system {
        Some(s) => s.resolve(dir)?,
        None => BASE_SYSTEM_PROMPT.to_string(),
    };
    if let Some(extra) = &file.prompt.append {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(extra.resolve(dir)?.trim());
    }
    let instruction = match &file.prompt.instruction {
        Some(i) => i.resolve(dir)?.trim().to_string(),
        None => String::new(),
    };
    if instruction.is_empty() {
        return Err("prompt.instruction is required".into());
    }

    Ok(Spec {
        name,
        dir: dir.to_path_buf(),
        description: file.description,
        enabled: file.enabled,
        workdir,
        trigger: file.trigger,
        run: file.run,
        system_prompt,
        instruction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
[prompt]
instruction = "Do the thing."
"#;

    #[test]
    fn spec_loads_from_disk() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("example");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join(SPEC_FILE), MINIMAL).unwrap();
        let spec = load(&dir).expect("load spec");
        assert_eq!(spec.name, "example");
        assert_eq!(spec.instruction, "Do the thing.");
    }

    #[test]
    fn minimal_spec_gets_defaults() {
        let dir = Path::new("/tmp/agents/bb-prs");
        let s = parse(dir, MINIMAL).unwrap();
        assert_eq!(s.name, "bb-prs");
        assert_eq!(s.workdir, dir.join("work"));
        assert_eq!(s.trigger.kind, TriggerKind::Inbox);
        assert_eq!(s.run.tools, vec!["Read".to_string()]);
        assert!(s.system_prompt.starts_with("You are an autonomous worker"));
        assert_eq!(s.instruction, "Do the thing.");
        assert!(s.enabled);
    }

    #[test]
    fn append_extends_base_prompt() {
        let raw = r#"
[prompt]
append = "You watch PRs."
instruction = "Go."
"#;
        let s = parse(Path::new("/tmp/x"), raw).unwrap();
        assert!(s.system_prompt.starts_with(BASE_SYSTEM_PROMPT));
        assert!(s.system_prompt.ends_with("You watch PRs."));
    }

    #[test]
    fn instruction_required() {
        let err = parse(Path::new("/tmp/x"), "enabled = true").unwrap_err();
        assert!(err.contains("instruction"), "{err}");
    }

    #[test]
    fn permission_mode_is_checked() {
        let raw = "[run]\npermission_mode=\"yolo\"\n[prompt]\ninstruction=\"x\"";
        assert!(parse(Path::new("/tmp/x"), raw)
            .unwrap_err()
            .contains("permission_mode"));
        let raw = "[run]\npermission_mode=\"bypassPermissions\"\n[prompt]\ninstruction=\"x\"";
        assert!(parse(Path::new("/tmp/x"), raw).is_ok());
    }

    #[test]
    fn poll_needs_command() {
        let raw = "[trigger]\nkind = \"poll\"\n[prompt]\ninstruction = \"x\"";
        assert!(parse(Path::new("/tmp/x"), raw)
            .unwrap_err()
            .contains("trigger.command"));
    }

    #[test]
    fn unknown_tool_rejected() {
        let raw = "[run]\ntools = [\"Frobnicate\"]\n[prompt]\ninstruction = \"x\"";
        assert!(parse(Path::new("/tmp/x"), raw)
            .unwrap_err()
            .contains("Frobnicate"));
    }

    #[test]
    fn unknown_field_rejected() {
        let raw = "bogus = 1\n[prompt]\ninstruction = \"x\"";
        assert!(parse(Path::new("/tmp/x"), raw).is_err());
    }

    #[test]
    fn trigger_labels() {
        let raw = "[trigger]\nkind = \"poll\"\ncommand = \"true\"\ninterval_s = 300\n[prompt]\ninstruction = \"x\"";
        assert_eq!(
            parse(Path::new("/tmp/x"), raw).unwrap().trigger_label(),
            "poll 5m"
        );
        assert_eq!(fmt_secs(7200), "2h");
        assert_eq!(fmt_secs(90), "90s");
    }
}
