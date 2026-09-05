//! Persistent agents ("Agents" tab): long-lived watchers that wake on
//! events, run one bounded `claude -p` tick, leave findings in files, and
//! ask when they need a human.
//!
//! Layout under `~/.cc-hub/agents/<name>/`:
//!
//! ```text
//! agent.toml          the spec (see spec.rs); edits apply on the next tick
//! work/               the agent's whole world (default workdir)
//! state.json          supervisor-owned: ticks, spend, failures, halt reason
//! notes.jsonl         outbox to the user (`cc-hub agent note`)
//! inbox/              events: new files, then processing/ done/ failed/
//! log/YYYY-MM-DD.jsonl raw stream-json per tick
//! ```
//!
//! `agent.rs` (singular) is the coding-agent *backend* registry; this module
//! is the harness that runs unattended agents on top of one of them.

pub mod runner;
pub mod spec;
pub mod supervisor;
pub mod tools;
pub mod trigger;

use crate::persist::save_json;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use runner::Tick;
pub use spec::Spec;
pub use trigger::Event;

pub const MAX_HISTORY: usize = 50;
pub const MAX_FAILURES_IN_A_ROW: u32 = 5;

/// `~/.cc-hub/agents/`
pub fn root() -> Option<PathBuf> {
    crate::orchestrator::cc_hub_home().map(|h| h.join("agents"))
}

/// Whether the agents root existed at process start. Cached because
/// `visible_tabs()` runs every frame.
pub fn root_exists() -> bool {
    static EXISTS: OnceLock<bool> = OnceLock::new();
    *EXISTS.get_or_init(|| root().map(|r| r.is_dir()).unwrap_or(false))
}

pub fn agent_dir(name: &str) -> Option<PathBuf> {
    root().map(|r| r.join(name))
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ---- state ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TickRecord {
    pub at: i64,
    pub event: Option<String>,
    /// The session this tick ran in, so its transcript can be reopened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub ok: bool,
    pub subtype: Option<String>,
    pub turns: u32,
    pub compactions: u32,
    pub cost_usd: f64,
    pub context_start: u64,
    pub context_end: u64,
    pub duration_s: u64,
    /// First ~200 chars of the result, for the timeline.
    pub result: String,
}

/// Supervisor-owned bookkeeping. The agent's own working state lives in its
/// workdir and is none of the harness's business.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentState {
    pub session_id: Option<String>,
    pub ticks: u64,
    pub compactions: u64,
    pub cost_usd: f64,
    pub failures_in_a_row: u32,
    /// Set when the loop halted itself (budget, failures). Cleared by
    /// `resume` / `reset`.
    pub stopped_reason: Option<String>,
    /// User-requested pause. The supervisor skips a paused agent.
    pub paused: bool,
    pub last_tick_at: Option<i64>,
    pub last_result: String,
    /// `Some` while a tick is in flight: (unix start, event id).
    pub ticking: Option<Ticking>,
    /// Spend for `today` (UTC date); rolls over when the date changes.
    pub today: String,
    pub today_cost_usd: f64,
    pub today_ticks: u64,
    pub history: Vec<TickRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ticking {
    pub since: i64,
    pub event: Option<String>,
    /// Filled the moment the CLI announces it, so the tab can tail a tick
    /// while it is still running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

fn today_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

impl AgentState {
    fn roll_day(&mut self) {
        let t = today_utc();
        if self.today != t {
            self.today = t;
            self.today_cost_usd = 0.0;
            self.today_ticks = 0;
        }
    }

    /// Spend today, correct even if the date rolled since the last write.
    pub fn today_cost(&self) -> f64 {
        if self.today == today_utc() {
            self.today_cost_usd
        } else {
            0.0
        }
    }

    pub fn today_ticks(&self) -> u64 {
        if self.today == today_utc() {
            self.today_ticks
        } else {
            0
        }
    }
}

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

pub fn load_state(dir: &Path) -> AgentState {
    let path = state_path(dir);
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            warn!(
                "harness: {} unreadable ({}), starting fresh",
                path.display(),
                e
            );
            AgentState::default()
        }),
        Err(_) => AgentState::default(),
    }
}

/// Read-mutate-write `state.json` under the agent's `state.lock`, so the
/// TUI supervisor and CLI verbs can't clobber each other.
pub fn update_state<F: FnOnce(&mut AgentState)>(dir: &Path, f: F) -> io::Result<AgentState> {
    use fs2::FileExt;
    fs::create_dir_all(dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("state.lock"))?;
    lock.lock_exclusive()?;
    let mut state = load_state(dir);
    f(&mut state);
    save_json(&state_path(dir), &state)?;
    let _ = lock.unlock();
    Ok(state)
}

// ---- notes ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub at: i64,
    /// `info` | `warn`.
    pub level: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    pub tick: u64,
}

pub fn notes_path(dir: &Path) -> PathBuf {
    dir.join("notes.jsonl")
}

pub fn append_note(dir: &Path, note: &Note) -> io::Result<()> {
    use std::io::Write;
    fs::create_dir_all(dir)?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(notes_path(dir))?;
    writeln!(
        f,
        "{}",
        serde_json::to_string(note).map_err(io::Error::other)?
    )
}

/// The newest `limit` notes, newest first.
pub fn read_notes(dir: &Path, limit: usize) -> Vec<Note> {
    let Ok(raw) = fs::read_to_string(notes_path(dir)) else {
        return Vec::new();
    };
    let mut notes: Vec<Note> = raw
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    notes.reverse();
    notes.truncate(limit);
    notes
}

// ---- paths ----------------------------------------------------------------

pub fn inbox_path(dir: &Path) -> PathBuf {
    dir.join("inbox")
}

pub fn log_path(dir: &Path) -> PathBuf {
    dir.join("log").join(format!("{}.jsonl", today_utc()))
}

// ---- one tick -------------------------------------------------------------

/// Run one tick and fold its outcome into `state.json`. Synchronous; the
/// supervisor calls it from `spawn_blocking`, the CLI directly.
pub fn tick_once(spec: &Spec, event: Option<&Event>) -> io::Result<(Tick, AgentState)> {
    let started = now_unix();
    let event_id = event.map(|e| e.id.clone());
    let state = update_state(&spec.dir, |s| {
        s.roll_day();
        // A persistent agent resumes a session we already know; a fresh one
        // gets its id from the CLI a moment later (see `session_started`).
        let resumed = spec.run.persistent_session.then(|| s.session_id.clone());
        s.ticking = Some(Ticking {
            since: started,
            event: event_id.clone(),
            session_id: resumed.flatten(),
        });
    })?;
    let resume = if spec.run.persistent_session {
        state.session_id.clone()
    } else {
        None
    };
    let prompt = trigger::render_prompt(spec, event);
    let header = serde_json::json!({
        "type": "cc-hub-tick",
        "agent": spec.name,
        "tick": state.ticks + 1,
        "event": event_id,
        "at": started,
    })
    .to_string();
    let tick = runner::run(
        spec,
        &prompt,
        resume.as_deref(),
        Some(&log_path(&spec.dir)),
        &header,
        &|sid| {
            let _ = update_state(&spec.dir, |s| {
                if let Some(t) = &mut s.ticking {
                    t.session_id = Some(sid.to_string());
                }
            });
        },
    );

    let state = update_state(&spec.dir, |s| {
        s.roll_day();
        s.ticking = None;
        s.ticks += 1;
        s.today_ticks += 1;
        s.compactions += tick.compactions as u64;
        s.cost_usd += tick.cost_usd;
        s.today_cost_usd += tick.cost_usd;
        s.last_tick_at = Some(now_unix());
        s.last_result = truncate(&tick.result, 800);
        if spec.run.persistent_session {
            if let Some(id) = &tick.session_id {
                s.session_id = Some(id.clone());
            }
        }
        // A thrashing session can never recover: drop it so the next tick
        // starts clean.
        if tick.thrashed() {
            s.session_id = None;
            s.last_result = "session dropped: autocompact thrashing (window too small)".into();
        } else if tick.compaction_starved() {
            s.last_result = format!(
                "budget spent on {} compactions, no work done — raise run.window_pct (now {}) or max_budget_usd",
                tick.compactions, spec.run.window_pct
            );
        }
        s.failures_in_a_row = if tick.ok { 0 } else { s.failures_in_a_row + 1 };
        s.history.push(TickRecord {
            at: started,
            event: event_id.clone(),
            ok: tick.ok,
            subtype: tick.subtype.clone(),
            turns: tick.turns,
            compactions: tick.compactions,
            cost_usd: (tick.cost_usd * 10_000.0).round() / 10_000.0,
            context_start: tick.context_start,
            context_end: tick.context_end,
            duration_s: tick.duration_s,
            session_id: tick.session_id.clone(),
            result: truncate(&tick.result, 200),
        });
        if s.history.len() > MAX_HISTORY {
            let drop = s.history.len() - MAX_HISTORY;
            s.history.drain(..drop);
        }
        if s.failures_in_a_row >= MAX_FAILURES_IN_A_ROW {
            s.stopped_reason = Some(format!(
                "{} consecutive failed ticks",
                MAX_FAILURES_IN_A_ROW
            ));
        }
    })?;
    info!(
        "harness[{}]: tick={} ok={} turns={} compact={} ${:.3} total=${:.2} {}",
        spec.name,
        state.ticks,
        tick.ok,
        tick.turns,
        tick.compactions,
        tick.cost_usd,
        state.cost_usd,
        tick.subtype.as_deref().unwrap_or("")
    );
    Ok((tick, state))
}

/// Why a tick must not start now, if any. Checked by the supervisor before
/// every tick and by `once` unless forced.
pub fn budget_block(spec: &Spec, state: &AgentState) -> Option<String> {
    if let Some(total) = spec.run.budget_usd_total {
        if state.cost_usd >= total {
            return Some(format!("total budget ${} reached", total));
        }
    }
    if let Some(daily) = spec.run.daily_budget_usd {
        if state.today_cost() >= daily {
            return Some(format!("daily budget ${} reached", daily));
        }
    }
    None
}

pub(crate) fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ---- control verbs --------------------------------------------------------

/// Drop an event into the agent's inbox. Works for every trigger kind: the
/// inbox is always checked first.
pub fn poke(dir: &Path, payload: &str) -> io::Result<String> {
    trigger::drop_event(&inbox_path(dir), "poke", payload)
}

pub fn set_paused(dir: &Path, paused: bool) -> io::Result<AgentState> {
    update_state(dir, |s| {
        s.paused = paused;
        if !paused {
            // Resume also clears a halt so the user has one verb for "go".
            s.stopped_reason = None;
            s.failures_in_a_row = 0;
        }
    })
}

/// Clear harness bookkeeping. The workdir is untouched.
pub fn reset(dir: &Path) -> io::Result<()> {
    let _ = fs::remove_file(state_path(dir));
    Ok(())
}

// ---- snapshot (what the TUI renders) --------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// A tick is running.
    Ticking,
    /// Waiting on its trigger. The normal state; costs nothing.
    Sleeping,
    /// Halted itself: budget or repeated failures. Needs you.
    Halted,
    Paused,
    /// `enabled = false` in the spec.
    Disabled,
    /// `agent.toml` failed to load.
    Broken,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Ticking => "ticking",
            AgentStatus::Sleeping => "sleeping",
            AgentStatus::Halted => "halted",
            AgentStatus::Paused => "paused",
            AgentStatus::Disabled => "disabled",
            AgentStatus::Broken => "broken",
        }
    }

    pub fn needs_attention(self) -> bool {
        matches!(self, AgentStatus::Halted | AgentStatus::Broken)
    }
}

#[derive(Debug, Clone)]
pub struct AgentSnapshot {
    pub name: String,
    pub dir: PathBuf,
    pub spec: Result<Spec, String>,
    pub state: AgentState,
    pub notes: Vec<Note>,
    pub inbox_pending: usize,
}

impl AgentSnapshot {
    pub fn status(&self) -> AgentStatus {
        let Ok(spec) = &self.spec else {
            return AgentStatus::Broken;
        };
        if !spec.enabled {
            AgentStatus::Disabled
        } else if self.state.paused {
            AgentStatus::Paused
        } else if self.state.ticking.is_some() {
            AgentStatus::Ticking
        } else if self.state.stopped_reason.is_some() {
            AgentStatus::Halted
        } else {
            AgentStatus::Sleeping
        }
    }

    pub fn description(&self) -> &str {
        self.spec
            .as_ref()
            .map(|s| s.description.as_str())
            .unwrap_or("")
    }

    pub fn workdir(&self) -> &Path {
        self.spec
            .as_ref()
            .map(|s| s.workdir.as_path())
            .unwrap_or(&self.dir)
    }

    /// The session the agent is in right now, or last ran in — the tick in
    /// flight first, then the persistent session, then the newest tick that
    /// recorded one.
    pub fn session_id(&self) -> Option<&str> {
        self.state
            .ticking
            .as_ref()
            .and_then(|t| t.session_id.as_deref())
            .or(self.state.session_id.as_deref())
            .or_else(|| {
                self.state
                    .history
                    .iter()
                    .rev()
                    .find_map(|r| r.session_id.as_deref())
            })
    }

    /// Every session id this agent is known to have run in.
    pub fn session_ids(&self) -> impl Iterator<Item = &str> {
        self.state
            .ticking
            .as_ref()
            .and_then(|t| t.session_id.as_deref())
            .into_iter()
            .chain(self.state.session_id.as_deref())
            .chain(
                self.state
                    .history
                    .iter()
                    .filter_map(|r| r.session_id.as_deref()),
            )
    }

    /// Transcript of [`Self::session_id`], if Claude Code has written one.
    pub fn transcript(&self) -> Option<PathBuf> {
        let sid = self.session_id()?;
        crate::scanner::find_jsonl(&self.workdir().to_string_lossy(), sid)
            .or_else(|| crate::scanner::find_jsonl_anywhere(sid))
    }
}

/// The sessions agents run in, so the Sessions tab can leave them to the
/// Agents tab. A tick is a headless `claude -p` with no terminal behind it:
/// its card could never be attached, only stared at.
///
/// Two ways to belong to an agent, both of them the agent's own doing: the
/// tick recorded the session id it ran in, or the session runs inside the
/// agent's directory (where the default workdir lives). A spec pointing its
/// `workdir` at one of your repos owns only the sessions it recorded —
/// yours in that repo stay yours.
#[derive(Debug, Default, Clone)]
pub struct AgentSessions {
    by_id: std::collections::HashMap<String, String>,
    dirs: Vec<(PathBuf, String)>,
}

impl AgentSessions {
    pub fn of(agents: &[AgentSnapshot]) -> Self {
        let mut by_id = std::collections::HashMap::new();
        let mut dirs = Vec::new();
        for agent in agents {
            for sid in agent.session_ids() {
                by_id.insert(sid.to_string(), agent.name.clone());
            }
            dirs.push((agent.dir.clone(), agent.name.clone()));
        }
        Self { by_id, dirs }
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.dirs.is_empty()
    }

    /// The agent this session belongs to, if any.
    pub fn owner(&self, session_id: &str, cwd: &Path) -> Option<&str> {
        if let Some(name) = self.by_id.get(session_id) {
            return Some(name);
        }
        self.dirs
            .iter()
            .find(|(dir, _)| cwd.starts_with(dir))
            .map(|(_, name)| name.as_str())
    }
}

pub fn snapshot(dir: &Path) -> AgentSnapshot {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    AgentSnapshot {
        name,
        spec: spec::load(dir),
        state: load_state(dir),
        notes: read_notes(dir, 20),
        inbox_pending: trigger::pending_count(&inbox_path(dir)),
        dir: dir.to_path_buf(),
    }
}

/// Every agent dir (one containing `agent.toml`), sorted by name.
pub fn agent_dirs() -> Vec<PathBuf> {
    let Some(root) = root() else {
        return Vec::new();
    };
    let Ok(rd) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(spec::SPEC_FILE).is_file())
        .collect();
    dirs.sort();
    dirs
}

pub fn scan() -> Vec<AgentSnapshot> {
    agent_dirs().iter().map(|d| snapshot(d)).collect()
}

// ---- scaffold -------------------------------------------------------------

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
    fn state_roundtrip_under_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let s = update_state(tmp.path(), |s| {
            s.ticks = 3;
            s.paused = true;
        })
        .unwrap();
        assert_eq!(s.ticks, 3);
        assert_eq!(load_state(tmp.path()), s);
        let s2 = set_paused(tmp.path(), false).unwrap();
        assert!(!s2.paused);
    }

    fn agent_at(dir: &Path, name: &str, state: AgentState) -> AgentSnapshot {
        AgentSnapshot {
            name: name.into(),
            dir: dir.to_path_buf(),
            spec: Err("not loaded".into()),
            state,
            notes: Vec::new(),
            inbox_pending: 0,
        }
    }

    #[test]
    fn an_agent_owns_the_sessions_it_ran_and_its_own_directory() {
        let dir = Path::new("/home/me/.cc-hub/agents/bb-prs");
        let mut state = AgentState {
            session_id: Some("persistent".into()),
            ..Default::default()
        };
        state.history.push(TickRecord {
            at: 0,
            event: None,
            session_id: Some("tick-7".into()),
            ok: true,
            subtype: None,
            turns: 1,
            compactions: 0,
            cost_usd: 0.0,
            context_start: 0,
            context_end: 0,
            duration_s: 1,
            result: String::new(),
        });
        let owned = AgentSessions::of(&[agent_at(dir, "bb-prs", state)]);

        // Recorded ids belong to the agent wherever they ran …
        assert_eq!(
            owned.owner("tick-7", Path::new("/home/me/code")),
            Some("bb-prs")
        );
        assert_eq!(
            owned.owner("persistent", Path::new("/home/me/code")),
            Some("bb-prs")
        );
        // … as does anything running inside the agent's own directory.
        assert_eq!(owned.owner("unknown", &dir.join("work")), Some("bb-prs"));
        // A session of yours in your own repo stays yours.
        assert_eq!(owned.owner("mine", Path::new("/home/me/code")), None);
    }

    #[test]
    fn the_newest_known_session_wins() {
        let dir = Path::new("/home/me/.cc-hub/agents/a");
        let mut state = AgentState {
            session_id: Some("persistent".into()),
            ..Default::default()
        };
        let agent = agent_at(dir, "a", state.clone());
        assert_eq!(agent.session_id(), Some("persistent"));

        state.ticking = Some(Ticking {
            since: 0,
            event: None,
            session_id: Some("in-flight".into()),
        });
        let agent = agent_at(dir, "a", state);
        assert_eq!(agent.session_id(), Some("in-flight"));
    }

    #[test]
    fn notes_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..3 {
            append_note(
                tmp.path(),
                &Note {
                    at: i,
                    level: "info".into(),
                    text: format!("n{i}"),
                    r#ref: None,
                    tick: 1,
                },
            )
            .unwrap();
        }
        let notes = read_notes(tmp.path(), 2);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "n2");
    }

    #[test]
    fn budget_gates() {
        let spec = spec::parse(
            Path::new("/tmp/x"),
            "[run]\nbudget_usd_total = 1.0\ndaily_budget_usd = 0.5\n[prompt]\ninstruction=\"x\"",
        )
        .unwrap();
        let mut st = AgentState::default();
        assert!(budget_block(&spec, &st).is_none());
        st.today = today_utc();
        st.today_cost_usd = 0.6;
        assert!(budget_block(&spec, &st).unwrap().contains("daily"));
        st.cost_usd = 1.2;
        assert!(budget_block(&spec, &st).unwrap().contains("total"));
    }

    #[test]
    fn snapshot_status_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(spec::SPEC_FILE), "[prompt]\ninstruction=\"x\"").unwrap();
        assert_eq!(snapshot(&dir).status(), AgentStatus::Sleeping);
        update_state(&dir, |s| s.stopped_reason = Some("budget".into())).unwrap();
        assert_eq!(snapshot(&dir).status(), AgentStatus::Halted);
        update_state(&dir, |s| s.paused = true).unwrap();
        assert_eq!(snapshot(&dir).status(), AgentStatus::Paused);
        fs::write(dir.join(spec::SPEC_FILE), "nonsense = [").unwrap();
        assert_eq!(snapshot(&dir).status(), AgentStatus::Broken);
    }

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

    #[test]
    fn names_are_validated() {
        assert!(valid_name("bb-prs_2"));
        assert!(!valid_name("../x"));
        assert!(!valid_name(""));
    }
}
