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
//! events.jsonl        harness log: runs, poll failures, halts, hub edits
//! inbox/              events: new files, then processing/ done/ failed/
//! log/YYYY-MM-DD.jsonl raw stream-json per tick
//! ```
//!
//! `agent.rs` (singular) is the coding-agent *backend* registry; this module
//! is the harness that runs unattended agents on top of one of them.
//!
//! - `spec`: parses `agent.toml`.
//! - `settings`: the spec fields the Agents tab edits in place.
//! - `trigger`: inbox, poll and interval event sources.
//! - `supervisor`: the per-agent tokio loops inside the TUI.
//! - `tick`: one tick end to end, folded into `state.json`, plus budget gates.
//! - `runner`: spawns `claude -p` for a tick and parses its stream-json.
//! - `tools`: turns the spec's tool list into CLI allow/deny rules.
//! - `state`: `state.json` bookkeeping under `state.lock`.
//! - `journal`: `notes.jsonl` (to the user) and `events.jsonl` (harness log).
//! - `snapshot`: what the Agents tab renders.
//! - `scaffold`: creates a new agent directory.

mod journal;
pub mod runner;
mod scaffold;
pub mod settings;
mod snapshot;
pub mod spec;
mod state;
pub mod supervisor;
mod tick;
pub mod tools;
pub mod trigger;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use journal::{
    append_note, events_path, log_event, notes_path, read_events, read_notes, LogLine, Note,
};
pub use runner::Tick;
pub use scaffold::{scaffold, TEMPLATE_SPEC};
pub use snapshot::{agent_dirs, scan, snapshot, AgentSessions, AgentSnapshot, AgentStatus, Run};
pub use spec::Spec;
use state::today_utc;
pub use state::{
    load_state, now_unix, reset, set_paused, state_path, update_state, AgentState, TickRecord,
    Ticking,
};
pub(crate) use tick::truncate;
pub use tick::{budget_block, tick_once, MAX_FAILURES_IN_A_ROW, MAX_HISTORY};
pub use trigger::Event;

/// `~/.cc-hub/agents/`
pub fn root() -> Option<PathBuf> {
    crate::platform::paths::cc_hub_home().map(|h| h.join("agents"))
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

pub fn inbox_path(dir: &Path) -> PathBuf {
    dir.join("inbox")
}

pub fn log_path(dir: &Path) -> PathBuf {
    dir.join("log").join(format!("{}.jsonl", today_utc()))
}

/// Drop an event into the agent's inbox. Works for every trigger kind: the
/// inbox is always checked first.
pub fn poke(dir: &Path, payload: &str) -> io::Result<String> {
    trigger::drop_event(&inbox_path(dir), "poke", payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated() {
        assert!(valid_name("bb-prs_2"));
        assert!(!valid_name("../x"));
        assert!(!valid_name(""));
    }
}
