//! Spawn watchdogs, the placeholder card that stands in for a session until
//! the scanner first sees it, and naming a session while it boots.

use crate::agent::AgentKind;
use crate::app::App;
use crate::config;
use crate::models::{SessionInfo, SessionKind, SessionState};
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
    /// What the placeholder card stands in for.
    kind: SessionKind,
    /// Where the session was spawned; parents the placeholder card's group.
    cwd: String,
    deadline: Instant,
}

/// How long a freshly spawned agent gets to appear in a scan snapshot before
/// its watch fires. Claude typically registers its session file within ~3s of
/// spawn; the slack covers slow cold starts. A false alarm costs one status
/// line, so generous beats jumpy.
const SPAWN_WATCH_TIMEOUT: Duration = Duration::from_secs(15);

/// [`SPAWN_WATCH_TIMEOUT`] for a terminal. A terminal writes no file to
/// wake the scanner, so it surfaces on the 10s full scan: the watch has to
/// outlast two of those, or a healthy terminal would be called stalled.
const TERMINAL_WATCH_TIMEOUT: Duration = Duration::from_secs(25);

/// How often [`App::poll_spawn_watches`] checks that pending spawns' tmux
/// sessions still exist. Each check forks tmux, too costly for every frame.
const SPAWN_PROBE_INTERVAL: Duration = Duration::from_millis(500);

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
    s.kind == SessionKind::Agent(AgentKind::Codex) && s.jsonl_path.is_none()
}

/// Loading-state stand-in for a spawn the scanner hasn't seen yet: Starting
/// (its own orbit spinner and color, and the idle liveness rank — the slot
/// the real card first appears in) with a "starting …" title. `tmux_session`
/// is real, so opening the card attaches to the pane where the agent is
/// actually booting.
///
/// A terminal has nothing to boot — its shell is ready as soon as it is
/// spawned — so its stand-in is a plain idle terminal until a scan finds
/// the real one.
pub(super) fn spawning_placeholder(watch: &SpawnWatch) -> SessionInfo {
    let mut info = SessionInfo {
        agent_id: watch.agent.clone(),
        kind: watch.kind,
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
    match watch.kind {
        SessionKind::Agent(_) => info.title = Some(format!("starting {}…", info.agent_badge())),
        SessionKind::Terminal => info.state = SessionState::Idle,
    }
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
    /// queued to be typed once the new session is ready, and its name, when
    /// the source had one, is the new session's.
    pub fn watch_spawn(&mut self, tmux_name: String, agent: String, cwd: String) {
        let mut title = None;
        if let Some(handoff) = self.sessions.handoff.take() {
            self.queue_pending_draft(tmux_name.clone(), handoff.draft);
            title = handoff.title;
        }
        self.watch_spawn_titled(tmux_name, agent, cwd, title);
    }

    /// [`Self::watch_spawn`] with a name inherited from a previous session —
    /// a respawn or a handoff continues one that is usually already named, so
    /// the title is stashed for adoption instead of prompting the user again.
    pub fn watch_spawn_titled(
        &mut self,
        tmux_name: String,
        agent: String,
        cwd: String,
        title: Option<String>,
    ) {
        let kind = config::get()
            .agent(&agent)
            .map(|a| a.kind)
            .unwrap_or(AgentKind::Claude);
        self.watch(SpawnWatch {
            tmux_name: tmux_name.clone(),
            agent,
            kind: SessionKind::Agent(kind),
            cwd,
            deadline: Instant::now() + SPAWN_WATCH_TIMEOUT,
        });
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

    /// [`Self::watch_spawn`] for a terminal `t` opened: no handoff to take,
    /// and the name is asked for at once, as for an agent with auto-titling
    /// off — nothing would ever name a terminal otherwise.
    pub fn watch_terminal(&mut self, tmux_name: String, cwd: String) {
        self.watch(SpawnWatch {
            tmux_name,
            agent: crate::sessions::terminal::AGENT_ID.into(),
            kind: SessionKind::Terminal,
            cwd,
            deadline: Instant::now() + TERMINAL_WATCH_TIMEOUT,
        });
        self.enter_rename_session();
    }

    fn watch(&mut self, watch: SpawnWatch) {
        let placeholder_id = spawning_session_id(&watch.tmux_name);
        self.spawn_watches.push(watch);
        self.rebuild_groups();
        self.select_session_by_id(&placeholder_id);
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
                self.forget_failed_spawn(&w);
                let msg = crate::spawn::diagnose_stalled_spawn(&w.tmux_name, &w.agent);
                log::warn!("spawn watch: {}", msg);
                self.set_error(msg);
                fired = true;
            } else {
                i += 1;
            }
        }
        fired
    }

    /// Fail a pending spawn the moment its tmux session is gone instead of
    /// at the deadline: a launch command the shell can't find kills the pane
    /// within milliseconds, and without this the placeholder sat on
    /// "Attaching…" for the whole timeout. Why it died takes a shell startup
    /// to find out, so a thread asks and the answer lands on a later call.
    /// Called every loop pass; tmux is asked at most every
    /// [`SPAWN_PROBE_INTERVAL`]. Returns whether anything visible changed.
    pub fn poll_spawn_watches(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.spawn_failures.1.try_recv() {
            log::warn!("spawn watch: {}", msg);
            self.set_error(msg);
            changed = true;
        }
        if self.spawn_watches.is_empty() {
            return changed;
        }
        let due = self
            .last_spawn_probe_at
            .is_none_or(|t| t.elapsed() >= SPAWN_PROBE_INTERVAL);
        if !due {
            return changed;
        }
        self.last_spawn_probe_at = Some(Instant::now());
        let (dead, alive): (Vec<_>, Vec<_>) = std::mem::take(&mut self.spawn_watches)
            .into_iter()
            .partition(|w| !self.runtime.session_exists(&w.tmux_name));
        self.spawn_watches = alive;
        if dead.is_empty() {
            return changed;
        }
        for w in dead {
            log::warn!(
                "spawn watch: {} [{}] exited during startup",
                w.agent,
                w.tmux_name
            );
            self.forget_failed_spawn(&w);
            // A terminal is the shell itself: no launch command to look up.
            if w.kind == SessionKind::Terminal {
                self.set_error(format!("{} exited during startup", w.agent));
                continue;
            }
            self.set_error(format!("{} exited during startup, checking why…", w.agent));
            let runtime = std::sync::Arc::clone(&self.runtime);
            let tx = self.spawn_failures.0.clone();
            std::thread::spawn(move || {
                let _ = tx.send(runtime.explain_exited_spawn(&w.agent));
            });
        }
        self.rebuild_groups();
        true
    }

    /// The agent never came up, so its real session will never arrive — drop
    /// any boot-time name entry that would otherwise linger unresolved, and
    /// close a rename still typing into its placeholder, which the next
    /// rebuild takes off the screen.
    fn forget_failed_spawn(&mut self, w: &SpawnWatch) {
        self.pending_spawn_names.remove(&w.tmux_name);
        if self.rename_target == Some(spawning_session_id(&w.tmux_name)) {
            self.close_rename_session();
        }
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
    use crate::app::test_support::{app_with, fake_session, status};
    use crate::app::View;

    // A watch clears as soon as any live session maps to its tmux name —
    // even on the same tick its deadline passes (appearance wins over expiry).
    #[test]
    fn spawn_watch_clears_when_agent_appears() {
        let mut app = App::new();
        app.watch_spawn("hal-w-1".into(), "claude".into(), "/tmp".into());
        let sessions = vec![fake_session("hal-w-1", SessionState::Idle)];
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
        app.watch_spawn("hal-w-7".into(), "claude".into(), "/tmp".into());
        assert!(app.enter_rename_session());
        app.rename_buffer = "half a na".into();

        assert!(app.check_spawn_watches(&[], Instant::now() + 2 * SPAWN_WATCH_TIMEOUT));
        assert_eq!(app.view, View::Grid);
        assert!(app.rename_target.is_none());
    }

    // `t` puts the terminal on the grid at once, cursor on it, as a ready
    // terminal rather than a session still booting.
    #[test]
    fn a_new_terminal_is_on_the_grid_at_once() {
        let mut app = App::new();
        app.watch_terminal("hal-sh-1-2".into(), "/tmp/proj".into());
        let t = app.selected_session_info().expect("the cursor is on it");
        assert_eq!(t.kind, SessionKind::Terminal);
        assert_eq!(t.state, SessionState::Idle);
        assert_eq!(t.title, None);
        assert_eq!(t.tmux_session.as_deref(), Some("hal-sh-1-2"));
        assert_eq!(
            app.view,
            View::RenameSession,
            "a terminal asks for its name"
        );
    }

    #[test]
    fn spawning_tmux_of_reads_placeholder_ids_only() {
        assert_eq!(spawning_tmux_of("spawning:hal-w-1"), Some("hal-w-1"));
        assert_eq!(spawning_tmux_of("real-scanner-id"), None);
    }

    #[test]
    fn spawn_watch_stays_quiet_before_deadline() {
        let mut app = App::new();
        app.watch_spawn("hal-w-2".into(), "claude".into(), "/tmp".into());
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
            "hal-watchtest-missing".into(),
            "claude".into(),
            "/tmp".into(),
        );
        let fired = app.check_spawn_watches(&[], Instant::now() + 2 * SPAWN_WATCH_TIMEOUT);
        assert!(fired);
        assert!(app.spawn_watches.is_empty());
        let (msg, _) = app.status_msg.as_ref().expect("diagnosis status");
        assert!(
            msg.contains("hal-watchtest-missing"),
            "status should name the tmux session: {}",
            msg
        );
    }

    // A spawn is visible (and focused) the moment the watch is armed: a
    // Starting placeholder card in the spawn cwd's group, no scan needed.
    #[test]
    fn watch_spawn_shows_placeholder_immediately() {
        let mut app = App::new();
        app.watch_spawn("hal-w-3".into(), "claude".into(), "/tmp/proj".into());
        let all: Vec<&SessionInfo> = app
            .sessions
            .groups
            .iter()
            .flat_map(|g| &g.sessions)
            .collect();
        assert_eq!(all.len(), 1);
        let p = all[0];
        assert_eq!(p.session_id, "spawning:hal-w-3");
        assert_eq!(p.state, SessionState::Starting);
        assert_eq!(p.cwd, "/tmp/proj");
        assert_eq!(p.tmux_session.as_deref(), Some("hal-w-3"));
        assert_eq!(
            app.selected_session_id().as_deref(),
            Some("spawning:hal-w-3")
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
            fake_session("hal-active", SessionState::Processing),
            fake_session("hal-idle", SessionState::Idle),
        ]);
        app.watch_spawn("hal-w-6".into(), "claude".into(), "/tmp".into());
        let ids: Vec<&str> = app.sessions.groups[0]
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(ids, vec!["hal-active", "spawning:hal-w-6", "hal-idle"]);
    }

    // The scanner reporting a session hosted by the watched tmux name swaps
    // the placeholder for the real card on the same tick.
    #[test]
    fn placeholder_replaced_by_real_session() {
        let mut app = App::new();
        app.watch_spawn("hal-w-4".into(), "claude".into(), "/tmp".into());
        let real = fake_session("hal-w-4", SessionState::Idle);
        assert!(app.update_sessions(vec![real]));
        let ids: Vec<String> = app
            .sessions
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|s| s.session_id.clone()))
            .collect();
        assert_eq!(ids, vec!["hal-w-4".to_string()]);
        assert!(app.spawn_watches.is_empty());
    }

    // A launch command the shell can't find kills the tmux session at once.
    // The next poll fails the watch then, not at the deadline, takes the
    // placeholder off the grid, and the shell probe's answer replaces the
    // interim error.
    #[test]
    fn a_spawn_whose_session_died_fails_at_once_with_the_reason() {
        let (mut app, runtime) = app_with(Vec::new());
        app.watch_spawn("hal-w-8".into(), "claude".into(), "/tmp".into());
        runtime
            .exists
            .store(false, std::sync::atomic::Ordering::Relaxed);

        assert!(app.poll_spawn_watches());
        assert!(app.spawn_watches.is_empty());
        assert!(app.sessions.groups.is_empty());
        assert!(app.status_is_error);
        assert_eq!(status(&app), "claude exited during startup, checking why…");

        let deadline = Instant::now() + Duration::from_secs(5);
        while status(&app) != "claude explained" && Instant::now() < deadline {
            app.poll_spawn_watches();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(status(&app), "claude explained");
        assert!(app.status_is_error);
    }

    #[test]
    fn a_terminal_whose_shell_died_fails_without_a_probe() {
        let (mut app, runtime) = app_with(Vec::new());
        app.watch_terminal("hal-t-1".into(), "/tmp".into());
        runtime
            .exists
            .store(false, std::sync::atomic::Ordering::Relaxed);

        assert!(app.poll_spawn_watches());
        assert!(app.spawn_watches.is_empty());
        assert!(app.status_is_error);
        assert!(
            status(&app).ends_with("exited during startup"),
            "{}",
            status(&app)
        );
    }

    #[test]
    fn a_spawn_still_booting_keeps_its_watch() {
        let (mut app, _runtime) = app_with(Vec::new());
        app.watch_spawn("hal-w-9".into(), "claude".into(), "/tmp".into());
        assert!(!app.poll_spawn_watches());
        assert_eq!(app.spawn_watches.len(), 1);
        assert!(app.status_msg.is_none());
    }

    // An expired watch takes its placeholder with it — the diagnosis status
    // is the only trace of the failed spawn.
    #[test]
    fn placeholder_dropped_when_watch_expires() {
        let mut app = App::new();
        app.watch_spawn("hal-w-5".into(), "claude".into(), "/tmp".into());
        app.check_spawn_watches(&[], Instant::now() + 2 * SPAWN_WATCH_TIMEOUT);
        app.rebuild_groups();
        assert!(app.sessions.groups.is_empty());
    }
}
