//! User-intent commands and their side-effect contract.
//!
//! `bin/` maps key events onto [`Command`]s; [`App::execute`] performs every
//! in-process consequence — App state mutation, board writes, anything behind
//! [`crate::agent_runtime::AgentRuntime`] — and returns the [`Effect`]s that
//! need bin-side machinery: the terminal (pane sizes), tokio channels, or OS
//! window management. That split keeps the whole decision layer testable
//! without a terminal: tests execute commands against a recording runtime and
//! assert on state plus returned effects.
//!
//! Sessions-view commands landed first; Tasks-view commands join in a later
//! phase. Projects/Metrics arms still live in `bin/src/keys.rs`.

use super::{App, Tab};
use crate::agent::AgentKind;
use crate::{config, models, spawn, title};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Global(GlobalCommand),
    Sessions(SessionsCommand),
}

/// Commands available on any tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalCommand {
    Quit,
    /// Tab / Shift+K forward, BackTab / Shift+J backward.
    CycleTab {
        back: bool,
    },
    /// `m` on the Sessions tab.
    SetTabMetrics,
}

/// Sessions-tab commands, one per former `bin/src/keys.rs` arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionsCommand {
    NavUp,
    NavDown,
    NavLeft,
    NavRight,
    /// `i` — session-detail popup.
    OpenDetailPopup,
    /// `D` — state-debug popup.
    OpenStateDebug,
    /// `H` — toggle inactive sessions.
    ToggleShowInactive,
    /// `W` — toggle orchestrator/worker sessions.
    ToggleShowOrchWorkers,
    /// `f`/Enter — resume inactive, attach live tmux, or focus the window.
    FocusSelected,
    /// `o` — shell pane in the selected session's cwd.
    OpenShellHere,
    /// `x` — stage the close confirmation.
    StageConfirmClose,
    /// Space — ack the selected session's attention state.
    AckSelected,
    /// `n` — new agent session in the selected session's cwd.
    SpawnAgentHere,
    /// `N` — places picker.
    OpenPlacesPicker,
    /// `M` — bookmarks picker.
    OpenBookmarksPicker,
    /// `p` — prompt input.
    OpenPromptInput,
    /// `t` — todo side panel.
    OpenTodoPanel,
    /// `r` — rename input.
    OpenRenameSession,
    /// PromptInput Enter (sessions flow; the Projects flow stays in bin).
    SubmitPrompt,
    /// RenameSession Enter.
    SubmitRename,
}

/// Side effects [`App::execute`] cannot perform in-process: they need the
/// terminal, a tokio channel owned by `run()`, or the window manager. The
/// bin-side interpreter (`bin/src/effects.rs`) owns exactly these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Fetch the session-detail payload for the popup (detail channel).
    RequestSessionDetail { session_id: String },
    /// Fetch the state-debug payload (state-debug channel).
    RequestStateDebug { session_id: String },
    /// Kick the background metrics analysis.
    SpawnMetricsScan,
    /// Attach a tmux session as the embedded pane. `owned` panes are killed
    /// with the pane (shell panes); un-owned panes outlive it (agents).
    OpenTmuxPane { tmux: String, owned: bool },
    /// Spawn a shell tmux session in `cwd`, then attach it as an owned pane.
    OpenShell { cwd: String },
    /// Send a prompt to a running agent off-thread; the outcome comes back
    /// as a DispatchResult scan message with these status strings.
    DispatchPrompt {
        tmux: String,
        prompt: String,
        ok_msg: String,
        err_prefix: String,
    },
    /// Focus the OS window hosting `pid`, falling back to a tmux reattach
    /// in `cwd` when the window manager reports the session is detached.
    FocusWindow { pid: u32, cwd: String },
}

impl App {
    /// Execute a user command: apply every in-process consequence and return
    /// the effects bin must interpret. Status messaging happens here so the
    /// interpreter stays a thin IO shim.
    pub fn execute(&mut self, cmd: Command) -> Vec<Effect> {
        match cmd {
            Command::Global(c) => self.execute_global(c),
            Command::Sessions(c) => self.execute_sessions(c),
        }
    }

    fn execute_global(&mut self, cmd: GlobalCommand) -> Vec<Effect> {
        match cmd {
            GlobalCommand::Quit => {
                self.should_quit = true;
                Vec::new()
            }
            GlobalCommand::CycleTab { back } => {
                let was_metrics = self.current_tab == Tab::Metrics;
                if back {
                    self.cycle_tab_back();
                } else {
                    self.cycle_tab();
                }
                self.metrics_scan_effect(was_metrics)
            }
            GlobalCommand::SetTabMetrics => {
                let was_metrics = self.current_tab == Tab::Metrics;
                self.set_tab(Tab::Metrics);
                self.metrics_scan_effect(was_metrics)
            }
        }
    }

    /// The metrics tab computes lazily: landing on it without an analysis
    /// kicks the scan exactly once.
    fn metrics_scan_effect(&self, was_metrics: bool) -> Vec<Effect> {
        if !was_metrics && self.current_tab == Tab::Metrics && self.metrics.analysis.is_none() {
            vec![Effect::SpawnMetricsScan]
        } else {
            Vec::new()
        }
    }

    fn execute_sessions(&mut self, cmd: SessionsCommand) -> Vec<Effect> {
        use SessionsCommand::*;
        match cmd {
            NavRight => {
                self.sessions.move_right();
                Vec::new()
            }
            NavLeft => {
                self.sessions.move_left();
                Vec::new()
            }
            NavDown => {
                let cols = self.render.grid_cols;
                self.sessions.move_down(cols);
                Vec::new()
            }
            NavUp => {
                let cols = self.render.grid_cols;
                self.sessions.move_up(cols);
                Vec::new()
            }
            OpenDetailPopup => match self.selected_session_id() {
                Some(id) => {
                    self.enter_popup();
                    vec![Effect::RequestSessionDetail { session_id: id }]
                }
                None => Vec::new(),
            },
            OpenStateDebug => match self.selected_session_id() {
                Some(id) => {
                    self.enter_state_debug();
                    vec![Effect::RequestStateDebug { session_id: id }]
                }
                None => Vec::new(),
            },
            ToggleShowInactive => {
                self.toggle_show_inactive();
                let state = if self.sessions.show_inactive {
                    "shown"
                } else {
                    "hidden"
                };
                self.set_status(format!("inactive sessions {}", state));
                Vec::new()
            }
            ToggleShowOrchWorkers => {
                self.toggle_show_orch_workers();
                let state = if self.sessions.show_orch_workers {
                    "shown"
                } else {
                    "hidden"
                };
                self.set_status(format!("orchestrator/worker sessions {}", state));
                Vec::new()
            }
            FocusSelected => self.focus_selected_session(),
            OpenShellHere => match self.selected_session_info() {
                Some(session) => vec![Effect::OpenShell {
                    cwd: session.cwd.clone(),
                }],
                None => Vec::new(),
            },
            StageConfirmClose => {
                self.enter_confirm_close();
                Vec::new()
            }
            AckSelected => {
                self.ack_selected();
                Vec::new()
            }
            SpawnAgentHere => {
                self.spawn_agent_here();
                Vec::new()
            }
            OpenPlacesPicker => {
                self.enter_session_places_picker();
                Vec::new()
            }
            OpenBookmarksPicker => {
                if !self.enter_bookmarks_picker() {
                    self.set_status("no bookmarks — press N then m on a folder to add one".into());
                }
                Vec::new()
            }
            OpenPromptInput => {
                self.enter_prompt_input();
                Vec::new()
            }
            OpenTodoPanel => {
                self.enter_todo_panel();
                Vec::new()
            }
            OpenRenameSession => {
                if !self.enter_rename_session() {
                    self.set_status("no session selected to rename".into());
                }
                Vec::new()
            }
            SubmitPrompt => self.submit_prompt_command(),
            SubmitRename => {
                match self.submit_session_rename() {
                    Some((sid, title_text)) => match title::persist_title(&sid, &title_text) {
                        Ok(()) => self.set_status(format!("renamed to “{}”", title_text)),
                        Err(e) => {
                            log::warn!("rename: persist failed for {}: {}", sid, e);
                            self.set_status(format!("rename failed: {}", e));
                        }
                    },
                    None => self.set_status("rename cancelled — empty title".into()),
                }
                Vec::new()
            }
        }
    }

    /// `f`/Enter: resume an inactive session in place (runtime spawn), attach
    /// a live tmux session (pane effect), or focus the hosting OS window.
    fn focus_selected_session(&mut self) -> Vec<Effect> {
        let Some(session) = self.selected_session_info().cloned() else {
            return Vec::new();
        };
        if session.state == models::SessionState::Inactive {
            let resume = match session.agent_kind {
                AgentKind::Claude => {
                    Some(spawn::ResumeTarget::SessionId(session.session_id.clone()))
                }
                AgentKind::Pi => session
                    .jsonl_path
                    .clone()
                    .map(spawn::ResumeTarget::SessionFile),
            };
            let status = match resume {
                Some(target) => match self.runtime.spawn_session(
                    &session.agent_id,
                    &session.cwd,
                    Some(target),
                    None,
                    false,
                ) {
                    Ok(name) => format!(
                        "resumed {} [{}]",
                        models::short_sid(&session.session_id),
                        name
                    ),
                    Err(e) => format!("resume failed: {}", e),
                },
                None => "resume failed: missing session transcript".to_string(),
            };
            self.set_status(status);
            Vec::new()
        } else if let Some(tmux) = session.tmux_session.clone() {
            vec![Effect::OpenTmuxPane { tmux, owned: false }]
        } else {
            vec![Effect::FocusWindow {
                pid: session.pid,
                cwd: session.cwd.clone(),
            }]
        }
    }

    /// `n`: fresh agent session in the selected session's cwd, with the
    /// spawn watchdog armed.
    fn spawn_agent_here(&mut self) {
        let Some(sess) = self.selected_session_info().cloned() else {
            return;
        };
        let status = match self
            .runtime
            .spawn_session(&sess.agent_id, &sess.cwd, None, None, false)
        {
            Ok(name) => {
                let status = format!("started {} [{}]", sess.agent_badge(), name);
                self.watch_spawn(name, sess.agent_badge());
                status
            }
            Err(e) => format!("spawn failed: {}", e),
        };
        self.set_status(status);
    }

    /// PromptInput Enter, sessions flow: dispatch to the idle target when
    /// one was captured at input time, else auto-spawn — inline prompt when
    /// the agent supports it, queued dispatch otherwise.
    fn submit_prompt_command(&mut self) -> Vec<Effect> {
        if self.prompt_buffer.trim().is_empty() {
            self.close_prompt_input();
            self.set_status("empty prompt — dispatch cancelled".into());
            return Vec::new();
        }

        let target = self.dispatch_target().cloned();
        let prompt = self.submit_prompt_input();

        if let Some((pid, name, tmux)) = target {
            log::info!(
                "dispatch: idle target {} (PID {}) [{}] prompt_len={}",
                name,
                pid,
                tmux,
                prompt.len()
            );
            return vec![Effect::DispatchPrompt {
                ok_msg: format!("dispatched to {} (PID {}) [{}]", name, pid, tmux),
                err_prefix: "dispatch failed".to_string(),
                tmux,
                prompt,
            }];
        }

        let Some(cwd) = self.default_spawn_cwd() else {
            self.set_status("no idle agent and no cwd to spawn in".into());
            return Vec::new();
        };
        let agent_id = config::get().default_session_agent_id();
        let agent = config::get().agent(&agent_id);
        let supports_initial_prompt = agent.as_ref().is_some_and(|a| a.supports_initial_prompt());
        match self.runtime.spawn_session(
            &agent_id,
            &cwd,
            None,
            if supports_initial_prompt {
                Some(prompt.as_str())
            } else {
                None
            },
            false,
        ) {
            Ok(tmux_name) => {
                if supports_initial_prompt {
                    log::info!(
                        "dispatch: no idle agent, spawned [{}] in {} with inline prompt (len={})",
                        tmux_name,
                        cwd,
                        prompt.len()
                    );
                    self.set_status(format!(
                        "no idle agent — spawned {} [{}]",
                        agent_id, tmux_name
                    ));
                } else {
                    log::info!(
                        "dispatch: no idle agent, spawned [{}] in {} — queueing prompt (len={})",
                        tmux_name,
                        cwd,
                        prompt.len()
                    );
                    self.queue_pending_dispatch(tmux_name.clone(), prompt);
                    self.set_status(format!(
                        "no idle agent — spawned {} [{}], prompt queued",
                        agent_id, tmux_name
                    ));
                }
            }
            Err(e) => {
                log::warn!("dispatch: auto-spawn failed: {}", e);
                self.set_status(format!("auto-spawn failed: {}", e));
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::testing::RecordingRuntime;
    use crate::models::{SessionInfo, SessionState};
    use std::sync::Arc;

    fn session(id: &str, state: SessionState, tmux: Option<&str>) -> SessionInfo {
        SessionInfo {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            pid: 4242,
            session_id: id.into(),
            cwd: "/tmp/proj".into(),
            project_name: "proj".into(),
            started_at: 0,
            last_activity: None,
            state,
            last_user_message: None,
            summary: None,
            title: None,
            titling: false,
            model: None,
            git_branch: None,
            version: None,
            jsonl_path: None,
            tmux_session: tmux.map(str::to_string),
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: 0,
        }
    }

    fn app_with(sessions: Vec<SessionInfo>) -> (App, Arc<RecordingRuntime>) {
        let runtime = Arc::new(RecordingRuntime::default());
        let mut app = App::new_with_runtime(runtime.clone());
        // The grid hides Inactive sessions by default; command tests select
        // whatever they inject, so make every fixture visible.
        app.sessions.show_inactive = true;
        app.update_sessions(sessions);
        (app, runtime)
    }

    fn status(app: &App) -> String {
        app.status_msg
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default()
    }

    #[test]
    fn focus_inactive_claude_resumes_by_session_id() {
        let (mut app, runtime) = app_with(vec![session("sid-abc", SessionState::Inactive, None)]);
        let effects = app.execute(Command::Sessions(SessionsCommand::FocusSelected));
        assert!(effects.is_empty());
        let spawns = runtime.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].agent_id, "claude");
        assert_eq!(spawns[0].cwd, "/tmp/proj");
        assert_eq!(spawns[0].resume.as_deref(), Some("SessionId(\"sid-abc\")"));
        assert!(status(&app).starts_with("resumed"), "got: {}", status(&app));
    }

    #[test]
    fn focus_inactive_pi_without_transcript_fails_without_spawn() {
        let mut sess = session("sid-pi", SessionState::Inactive, None);
        sess.agent_kind = AgentKind::Pi;
        sess.jsonl_path = None;
        let (mut app, runtime) = app_with(vec![sess]);
        let effects = app.execute(Command::Sessions(SessionsCommand::FocusSelected));
        assert!(effects.is_empty());
        assert!(runtime.spawns.lock().unwrap().is_empty());
        assert_eq!(status(&app), "resume failed: missing session transcript");
    }

    #[test]
    fn focus_live_tmux_session_opens_pane() {
        let (mut app, _rt) = app_with(vec![session(
            "sid-1",
            SessionState::Processing,
            Some("cc-agent-1"),
        )]);
        let effects = app.execute(Command::Sessions(SessionsCommand::FocusSelected));
        assert_eq!(
            effects,
            vec![Effect::OpenTmuxPane {
                tmux: "cc-agent-1".into(),
                owned: false
            }]
        );
    }

    #[test]
    fn focus_detached_session_focuses_window() {
        let (mut app, _rt) = app_with(vec![session("sid-1", SessionState::Idle, None)]);
        let effects = app.execute(Command::Sessions(SessionsCommand::FocusSelected));
        assert_eq!(
            effects,
            vec![Effect::FocusWindow {
                pid: 4242,
                cwd: "/tmp/proj".into()
            }]
        );
    }

    #[test]
    fn submit_prompt_empty_cancels() {
        let (mut app, runtime) = app_with(vec![]);
        app.enter_prompt_input();
        app.prompt_buffer = "   ".into();
        let effects = app.execute(Command::Sessions(SessionsCommand::SubmitPrompt));
        assert!(effects.is_empty());
        assert!(runtime.spawns.lock().unwrap().is_empty());
        assert_eq!(status(&app), "empty prompt — dispatch cancelled");
    }

    #[test]
    fn submit_prompt_with_idle_target_dispatches() {
        let (mut app, _rt) = app_with(vec![session(
            "sid-1",
            SessionState::Idle,
            Some("cc-idle-1"),
        )]);
        app.enter_prompt_input();
        app.dispatch_target = Some((4242, "proj".into(), "cc-idle-1".into()));
        app.prompt_buffer = "do the thing".into();
        let effects = app.execute(Command::Sessions(SessionsCommand::SubmitPrompt));
        assert_eq!(
            effects,
            vec![Effect::DispatchPrompt {
                tmux: "cc-idle-1".into(),
                prompt: "do the thing".into(),
                ok_msg: "dispatched to proj (PID 4242) [cc-idle-1]".into(),
                err_prefix: "dispatch failed".into(),
            }]
        );
    }

    #[test]
    #[cfg(unix)]
    fn submit_prompt_without_target_spawns_and_queues() {
        crate::test_util::with_temp_home(|| {
            // Default agent is claude, which doesn't take an inline initial
            // prompt: the spawn must queue the prompt for post-idle dispatch.
            let (mut app, runtime) = app_with(vec![session(
                "sid-1",
                SessionState::Processing,
                Some("cc-busy-1"),
            )]);
            app.enter_prompt_input();
            app.dispatch_target = None;
            app.prompt_buffer = "do the thing".into();
            let effects = app.execute(Command::Sessions(SessionsCommand::SubmitPrompt));
            assert!(effects.is_empty());
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].initial_prompt, None);
            assert!(app.has_pending_dispatch(), "prompt must be queued");
            assert!(
                status(&app).ends_with("prompt queued"),
                "got: {}",
                status(&app)
            );
        });
    }

    #[test]
    fn submit_prompt_without_sessions_spawns_in_home() {
        // With no selection, default_spawn_cwd falls back to the home dir —
        // the "no cwd" refusal only fires when even that is unavailable.
        let (mut app, runtime) = app_with(vec![]);
        app.enter_prompt_input();
        app.prompt_buffer = "do the thing".into();
        let effects = app.execute(Command::Sessions(SessionsCommand::SubmitPrompt));
        assert!(effects.is_empty());
        let spawns = runtime.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1);
        assert_eq!(
            spawns[0].cwd,
            dirs::home_dir().unwrap().display().to_string()
        );
        assert!(app.has_pending_dispatch());
    }

    #[test]
    fn spawn_agent_here_records_and_watches() {
        let (mut app, runtime) = app_with(vec![session(
            "sid-1",
            SessionState::Processing,
            Some("cc-agent-1"),
        )]);
        let effects = app.execute(Command::Sessions(SessionsCommand::SpawnAgentHere));
        assert!(effects.is_empty());
        let spawns = runtime.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].resume, None);
        assert_eq!(spawns[0].initial_prompt, None);
        assert!(status(&app).starts_with("started"), "got: {}", status(&app));
    }

    #[test]
    fn cycle_tab_into_metrics_requests_scan_once() {
        let (mut app, _rt) = app_with(vec![]);
        // Cycle until we land on Metrics; expect the scan effect there.
        let mut saw_scan = false;
        for _ in 0..4 {
            let effects = app.execute(Command::Global(GlobalCommand::CycleTab { back: false }));
            if app.current_tab == Tab::Metrics {
                assert_eq!(effects, vec![Effect::SpawnMetricsScan]);
                saw_scan = true;
                break;
            }
            assert!(effects.is_empty());
        }
        assert!(saw_scan, "never landed on Metrics tab");
    }

    #[test]
    fn toggles_flip_and_report() {
        let (mut app, _rt) = app_with(vec![]);
        let before = app.sessions.show_inactive;
        app.execute(Command::Sessions(SessionsCommand::ToggleShowInactive));
        assert_eq!(app.sessions.show_inactive, !before);
        assert!(status(&app).starts_with("inactive sessions"));

        let before = app.sessions.show_orch_workers;
        app.execute(Command::Sessions(SessionsCommand::ToggleShowOrchWorkers));
        assert_eq!(app.sessions.show_orch_workers, !before);
        assert!(status(&app).starts_with("orchestrator/worker sessions"));
    }

    #[test]
    fn open_detail_popup_requests_detail_for_selection() {
        let (mut app, _rt) = app_with(vec![session(
            "sid-1",
            SessionState::Processing,
            Some("cc-agent-1"),
        )]);
        let effects = app.execute(Command::Sessions(SessionsCommand::OpenDetailPopup));
        assert_eq!(
            effects,
            vec![Effect::RequestSessionDetail {
                session_id: "sid-1".into()
            }]
        );
        assert_eq!(app.view, super::super::View::Popup);

        let (mut app, _rt) = app_with(vec![]);
        let effects = app.execute(Command::Sessions(SessionsCommand::OpenDetailPopup));
        assert!(effects.is_empty());
    }

    #[test]
    fn quit_sets_flag() {
        let (mut app, _rt) = app_with(vec![]);
        app.execute(Command::Global(GlobalCommand::Quit));
        assert!(app.should_quit);
    }
}
