//! Spawn watchdogs, the placeholder card that stands in for a session until
//! the scanner first sees it, and naming a session while it boots.

use crate::app::App;
use crate::config;
use crate::models::{SessionInfo, SessionState};
use std::time::{Duration, Instant};

/// One pending spawn-verification: the agent must show up in a scan snapshot
/// hosted by `tmux_name` before `deadline`, else the user gets told. While
/// pending it also backs a placeholder "starting…" card in `cwd`'s group
/// (see [`App::build_groups`]), so the spawn is visible before the scanner
/// first sees the session.
pub(in crate::app) struct SpawnWatch {
    pub(super) tmux_name: String,
    /// Agent id: names the placeholder card and the diagnosis status line.
    agent: String,
    /// Where the session was spawned; parents the placeholder card's group.
    cwd: String,
    deadline: Instant,
}

/// How long a freshly spawned agent gets to appear in a scan snapshot before
/// its watch fires. Claude typically registers its session file within ~3s of
/// spawn; the slack covers slow cold starts. A false alarm costs one status
/// line, so generous beats jumpy.
const SPAWN_WATCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Marks a synthetic placeholder id (see [`spawning_session_id`]) so rename /
/// titling can special-case a card the scanner hasn't issued a real id for.
const SPAWNING_ID_PREFIX: &str = "spawning:";

/// Synthetic session id backing a spawn watch's placeholder card. Prefixed so
/// it can never collide with a real scanner-issued session id, and derived
/// from the tmux name so the same spawn maps to the same id across rebuilds.
pub(super) fn spawning_session_id(tmux_name: &str) -> String {
    format!("{SPAWNING_ID_PREFIX}{tmux_name}")
}

/// The spawning tmux name behind a placeholder id, or `None` for a real
/// scanner id. The tmux name is the stable bridge from a placeholder to the
/// real session, so name prompts routed while a session is still booting can
/// find their target once it lands.
pub(super) fn spawning_tmux_of(id: &str) -> Option<&str> {
    id.strip_prefix(SPAWNING_ID_PREFIX)
}

/// A Codex session the scanner knows only as a live process: Codex opens its
/// rollout file (and so gets its durable UUID) some time after it starts.
/// Until then its card is keyed by a PID-derived id that won't survive.
pub(super) fn is_codex_process_only(s: &SessionInfo) -> bool {
    s.agent_kind == crate::agent::AgentKind::Codex && s.jsonl_path.is_none()
}

/// Loading-state stand-in for a spawn the scanner hasn't seen yet: Starting
/// (its own orbit spinner and color, and the idle liveness rank — the slot
/// the real card first appears in) with a "starting …" title. `tmux_session`
/// is real, so opening the card attaches to the pane where the agent is
/// actually booting.
pub(super) fn spawning_placeholder(watch: &SpawnWatch) -> SessionInfo {
    let agent_kind = config::get()
        .agent(&watch.agent)
        .map(|a| a.kind)
        .unwrap_or(crate::agent::AgentKind::Claude);
    let mut info = SessionInfo {
        agent_id: watch.agent.clone(),
        agent_kind,
        pid: 0,
        session_id: spawning_session_id(&watch.tmux_name),
        cwd: watch.cwd.clone(),
        project_name: std::path::Path::new(&watch.cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| watch.cwd.clone()),
        started_at: 0,
        last_activity: None,
        state: SessionState::Starting,
        last_user_message: None,
        summary: None,
        title: None,
        titling: false,
        model: None,
        git_branch: None,
        version: None,
        jsonl_path: None,
        tmux_session: Some(watch.tmux_name.clone()),
        current_tool: None,
        is_thinking: false,
        context_tokens: None,
        tool_uses_count: 0,
    };
    info.title = Some(format!("starting {}…", info.agent_badge()));
    info
}

impl App {
    /// Register a watchdog for a just-spawned detached agent session and
    /// surface its placeholder card immediately, cursor on it — the spawned
    /// agent takes seconds to write a session file the scanner can see, and
    /// until this rebuild the keypress had no visible effect.
    ///
    /// This is the one way a new card takes the cursor: the real session
    /// inherits it from the placeholder through their shared tmux name (see
    /// `adopt_groups`). Sessions started anywhere else — an agent, the
    /// CLI, a deep link — land on the grid without moving it.
    ///
    /// This is every fresh session the user starts by hand, so it is also
    /// where a pending [`crate::handoff::Handoff`] is taken: its draft is
    /// queued to be typed once the new session is ready.
    pub fn watch_spawn(&mut self, tmux_name: String, agent: String, cwd: String) {
        if let Some(handoff) = self.sessions.handoff.take() {
            self.queue_pending_draft(tmux_name.clone(), handoff.draft);
        }
        self.watch_spawn_titled(tmux_name, agent, cwd, None);
    }

    /// [`Self::watch_spawn`] with a name inherited from a previous session —
    /// a respawn continues one that is usually already named, so the title
    /// is stashed for adoption instead of prompting the user again.
    pub fn watch_spawn_titled(
        &mut self,
        tmux_name: String,
        agent: String,
        cwd: String,
        title: Option<String>,
    ) {
        let placeholder_id = spawning_session_id(&tmux_name);
        self.spawn_watches.push(SpawnWatch {
            tmux_name: tmux_name.clone(),
            agent,
            cwd,
            deadline: Instant::now() + SPAWN_WATCH_TIMEOUT,
        });
        self.rebuild_groups();
        self.select_session_by_id(&placeholder_id);
        if let Some(title) = title {
            self.pending_spawn_names.insert(tmux_name, title);
            return;
        }
        // With Haiku auto-titling off, prompt for the name now — during the
        // boot — so the dead seconds while the agent writes its session file
        // are spent naming it instead of waiting to. The rename targets the
        // placeholder's row; the typed name is carried to the real session id in
        // `adopt_pending_spawn_names` once the scanner sees it.
        if !config::get().title.enabled {
            self.enter_rename_session();
        }
    }

    /// Resolve pending spawn watches against a scan snapshot. A watch clears
    /// when any session is hosted by its tmux name; one that expires first
    /// means the agent never came up (rc prompt, instant crash) — surface a
    /// diagnosis instead of leaving the silent "started" status as the last
    /// word. Returns true when a diagnosis was set, so the caller repaints
    /// even on an otherwise no-change tick.
    pub(super) fn check_spawn_watches(&mut self, sessions: &[SessionInfo], now: Instant) -> bool {
        if self.spawn_watches.is_empty() {
            return false;
        }
        self.spawn_watches.retain(|w| {
            !sessions
                .iter()
                .any(|s| s.tmux_session.as_deref() == Some(w.tmux_name.as_str()))
        });
        let mut fired = false;
        let mut i = 0;
        while i < self.spawn_watches.len() {
            if self.spawn_watches[i].deadline <= now {
                let w = self.spawn_watches.remove(i);
                // The agent never came up, so its real session will never
                // arrive — drop any boot-time name entry that would otherwise
                // linger unresolved, and close a rename still typing into its
                // placeholder, which the next rebuild takes off the screen.
                self.pending_spawn_names.remove(&w.tmux_name);
                if self.rename_target == Some(spawning_session_id(&w.tmux_name)) {
                    self.close_rename_session();
                }
                let msg = crate::spawn::diagnose_stalled_spawn(&w.tmux_name, &w.agent);
                log::warn!("spawn watch: {}", msg);
                self.set_status(msg);
                fired = true;
            } else {
                i += 1;
            }
        }
        fired
    }

    /// Bridge names typed while a session was still booting onto the real
    /// session the scanner has now produced. For each freshly-seen session
    /// whose spawning tmux carries a stashed name, persist it to the real id
    /// and stamp it on this snapshot so the card shows it immediately.
    pub(super) fn adopt_pending_spawn_names(&mut self, sessions: &mut [SessionInfo]) {
        if self.pending_spawn_names.is_empty() {
            return;
        }
        for s in sessions.iter_mut() {
            let Some(tmux) = s.tmux_session.as_deref() else {
                continue;
            };
            let Some(title) = self.pending_spawn_names.get(tmux) else {
                continue;
            };
            let title = title.clone();
            // A process-only Codex card is temporary: show the pending name,
            // but keep the tmux bridge until the durable UUID appears.
            if is_codex_process_only(s) {
                s.title = Some(title);
                continue;
            }
            self.pending_spawn_names.remove(tmux);
            match crate::title::persist_title(&s.session_id, &title) {
                Ok(()) => self.sessions.unscanned_titles.insert(&s.session_id, &title),
                Err(e) => log::warn!(
                    "rename: deferred persist failed for {}: {}",
                    s.session_id,
                    e
                ),
            }
            s.title = Some(title);
            s.titling = false;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::fake_session;
    use crate::app::View;

    // A watch clears as soon as any live session maps to its tmux name —
    // even on the same tick its deadline passes (appearance wins over expiry).
    #[test]
    fn spawn_watch_clears_when_agent_appears() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-1".into(), "claude".into(), "/tmp".into());
        let sessions = vec![fake_session("cchub-w-1", SessionState::Idle)];
        let fired = app.check_spawn_watches(&sessions, Instant::now() + 2 * SPAWN_WATCH_TIMEOUT);
        assert!(!fired);
        assert!(app.spawn_watches.is_empty());
        assert!(app.status_msg.is_none());
    }

    // A spawn that never comes up takes its placeholder off the screen, so a
    // name still being typed into it has nowhere left to show.
    #[test]
    fn a_stalled_spawn_closes_the_rename_typing_into_it() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-7".into(), "claude".into(), "/tmp".into());
        assert!(app.enter_rename_session());
        app.rename_buffer = "half a na".into();

        assert!(app.check_spawn_watches(&[], Instant::now() + 2 * SPAWN_WATCH_TIMEOUT));
        assert_eq!(app.view, View::Grid);
        assert!(app.rename_target.is_none());
    }

    #[test]
    fn spawning_tmux_of_reads_placeholder_ids_only() {
        assert_eq!(spawning_tmux_of("spawning:cchub-w-1"), Some("cchub-w-1"));
        assert_eq!(spawning_tmux_of("real-scanner-id"), None);
    }

    #[test]
    fn spawn_watch_stays_quiet_before_deadline() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-2".into(), "claude".into(), "/tmp".into());
        let fired = app.check_spawn_watches(&[], Instant::now());
        assert!(!fired);
        assert_eq!(app.spawn_watches.len(), 1);
        assert!(app.status_msg.is_none());
    }

    // No session ever mapped to the watched tmux name: past the deadline the
    // watch fires once, sets a diagnosis status, and is dropped.
    #[test]
    fn spawn_watch_fires_diagnosis_after_timeout() {
        let mut app = App::new();
        app.watch_spawn(
            "cchub-watchtest-missing".into(),
            "claude".into(),
            "/tmp".into(),
        );
        let fired = app.check_spawn_watches(&[], Instant::now() + 2 * SPAWN_WATCH_TIMEOUT);
        assert!(fired);
        assert!(app.spawn_watches.is_empty());
        let (msg, _) = app.status_msg.as_ref().expect("diagnosis status");
        assert!(
            msg.contains("cchub-watchtest-missing"),
            "status should name the tmux session: {}",
            msg
        );
    }

    // A spawn is visible (and focused) the moment the watch is armed: a
    // Starting placeholder card in the spawn cwd's group, no scan needed.
    #[test]
    fn watch_spawn_shows_placeholder_immediately() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-3".into(), "claude".into(), "/tmp/proj".into());
        let all: Vec<&SessionInfo> = app
            .sessions
            .groups
            .iter()
            .flat_map(|g| &g.sessions)
            .collect();
        assert_eq!(all.len(), 1);
        let p = all[0];
        assert_eq!(p.session_id, "spawning:cchub-w-3");
        assert_eq!(p.state, SessionState::Starting);
        assert_eq!(p.cwd, "/tmp/proj");
        assert_eq!(p.tmux_session.as_deref(), Some("cchub-w-3"));
        assert_eq!(
            app.selected_session_id().as_deref(),
            Some("spawning:cchub-w-3")
        );
    }

    // The placeholder must sit where the real card will land: after the
    // group's active sessions, at the head of the idle band (a fresh spawn
    // first scans in as Idle and the scanner orders newest-first within a
    // bucket). Anywhere else, the card would jump once the scanner takes
    // over.
    #[test]
    fn placeholder_sorts_into_the_idle_band() {
        let mut app = App::new();
        app.update_sessions(vec![
            fake_session("cchub-active", SessionState::Processing),
            fake_session("cchub-idle", SessionState::Idle),
        ]);
        app.watch_spawn("cchub-w-6".into(), "claude".into(), "/tmp".into());
        let ids: Vec<&str> = app.sessions.groups[0]
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["cchub-active", "spawning:cchub-w-6", "cchub-idle"]
        );
    }

    // The scanner reporting a session hosted by the watched tmux name swaps
    // the placeholder for the real card on the same tick.
    #[test]
    fn placeholder_replaced_by_real_session() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-4".into(), "claude".into(), "/tmp".into());
        let real = fake_session("cchub-w-4", SessionState::Idle);
        assert!(app.update_sessions(vec![real]));
        let ids: Vec<String> = app
            .sessions
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|s| s.session_id.clone()))
            .collect();
        assert_eq!(ids, vec!["cchub-w-4".to_string()]);
        assert!(app.spawn_watches.is_empty());
    }

    // An expired watch takes its placeholder with it — the diagnosis status
    // is the only trace of the failed spawn.
    #[test]
    fn placeholder_dropped_when_watch_expires() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-5".into(), "claude".into(), "/tmp".into());
        app.check_spawn_watches(&[], Instant::now() + 2 * SPAWN_WATCH_TIMEOUT);
        app.rebuild_groups();
        assert!(app.sessions.groups.is_empty());
    }
}
