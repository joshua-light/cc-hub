use super::{
    inbox_path, load_state, read_events, read_notes, root, spec, trigger, AgentState, LogLine,
    Note, Spec, TickRecord, Ticking,
};
use std::fs;
use std::path::{Path, PathBuf};

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
    /// Newest first; see [`super::log_event`].
    pub events: Vec<LogLine>,
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

    /// Transcript of session `sid`, if Claude Code has written one.
    pub fn transcript(&self, sid: &str) -> Option<PathBuf> {
        crate::sessions::scanner::find_jsonl(&self.workdir().to_string_lossy(), sid)
            .or_else(|| crate::sessions::scanner::find_jsonl_anywhere(sid))
    }

    /// Every run the agent remembers, newest first: the one in flight, then
    /// the history. Numbers count from the agent's first run, so they stay
    /// put as old history rolls off.
    pub fn runs(&self) -> Vec<Run<'_>> {
        let done = self.state.history.len() as u64;
        let base = self.state.ticks.saturating_sub(done);
        let mut runs = Vec::with_capacity(self.state.history.len() + 1);
        if let Some(t) = &self.state.ticking {
            runs.push(Run::InFlight {
                n: self.state.ticks + 1,
                ticking: t,
            });
        }
        for (i, rec) in self.state.history.iter().enumerate().rev() {
            runs.push(Run::Done {
                n: base + i as u64 + 1,
                rec,
            });
        }
        runs
    }

    /// The newest finished run.
    pub fn last_run(&self) -> Option<&TickRecord> {
        self.state.history.last()
    }
}

/// One row of the Runs list.
#[derive(Debug, Clone, Copy)]
pub enum Run<'a> {
    InFlight { n: u64, ticking: &'a Ticking },
    Done { n: u64, rec: &'a TickRecord },
}

impl Run<'_> {
    pub fn n(&self) -> u64 {
        match self {
            Run::InFlight { n, .. } | Run::Done { n, .. } => *n,
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Run::InFlight { ticking, .. } => ticking.session_id.as_deref(),
            Run::Done { rec, .. } => rec.session_id.as_deref(),
        }
    }
}

/// The sessions agents run in, so the Sessions tab can leave them to the
/// Agents tab. A tick is a headless `claude -p` with no terminal behind it:
/// its card could never be attached, only stared at.
///
/// Two ways to belong to an agent, both of them the agent's own doing: the
/// tick recorded the session id it ran in, or the session runs inside the
/// agent's default workdir. A spec pointing its `workdir` at one of your
/// repos owns only the sessions it recorded — yours in that repo stay
/// yours. So does a session you open in the agent's own directory to edit
/// it (`n` on the Agents tab): that one is a normal, attachable session.
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
            dirs.push((agent.dir.join("work"), agent.name.clone()));
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
        notes: read_notes(dir, 50),
        events: read_events(dir, 200),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::update_state;

    fn agent_at(dir: &Path, name: &str, state: AgentState) -> AgentSnapshot {
        AgentSnapshot {
            name: name.into(),
            dir: dir.to_path_buf(),
            spec: Err("not loaded".into()),
            state,
            notes: Vec::new(),
            events: Vec::new(),
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
            detail: None,
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
        // … as does anything running inside the agent's default workdir …
        assert_eq!(owned.owner("unknown", &dir.join("work")), Some("bb-prs"));
        // … but not a session opened in the agent's directory to edit it.
        assert_eq!(owned.owner("editing", dir), None);
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
    fn runs_are_numbered_from_the_first_and_newest_first() {
        let dir = Path::new("/tmp/agents/a");
        let rec = |at| TickRecord {
            at,
            event: None,
            session_id: None,
            ok: true,
            subtype: None,
            turns: 1,
            compactions: 0,
            cost_usd: 0.0,
            context_start: 0,
            context_end: 0,
            duration_s: 1,
            result: String::new(),
            detail: None,
        };
        // 10 runs so far, the last two still in history, an eleventh running.
        let state = AgentState {
            ticks: 10,
            history: vec![rec(1), rec(2)],
            ticking: Some(Ticking {
                since: 3,
                event: None,
                session_id: Some("live".into()),
            }),
            ..Default::default()
        };
        let agent = agent_at(dir, "a", state);
        let runs = agent.runs();
        let ns: Vec<u64> = runs.iter().map(Run::n).collect();
        assert_eq!(ns, vec![11, 10, 9]);
        assert_eq!(runs[0].session_id(), Some("live"));
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
}
