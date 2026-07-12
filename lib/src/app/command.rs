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
//! Sessions- and Tasks-view commands are ported; Projects/Metrics arms still
//! live in `bin/src/keys.rs`. Modal buffer-edit keys (task input/tags/filter
//! character editing) stay as thin `bin` arms — only their submit/logic arms
//! became commands.

use super::{App, Tab};
use crate::agent::AgentKind;
use crate::orchestrator::TaskPriority;
use crate::{config, models, spawn, title};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Global(GlobalCommand),
    Sessions(SessionsCommand),
    Tasks(TasksCommand),
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

/// Tasks-tab commands, one per former Tasks-board arm in
/// `bin/src/keys/tasks.rs`. Modal buffer editing (typing into the
/// input/tags/filter buffers) stays in bin; only the Grid actions and the
/// filter/tags/input submit arms are commands here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TasksCommand {
    NavUp,
    NavDown,
    NavLeft,
    NavRight,
    /// `L` — move the focused card one column right.
    MoveTaskRight,
    /// `H` — move the focused card one column left.
    MoveTaskLeft,
    /// `a`/`n` — open the add-task input.
    OpenAddInput,
    /// `/` — open the filter bar.
    OpenFilter,
    /// Esc on a filtered board — drop the filter.
    ClearFilter,
    /// `u` — restore the last delete/clear-done batch.
    UndoDelete,
    /// Space — proceed a Planning card or toggle Done.
    SpaceAction,
    /// `s` — open the agent-assign places picker.
    OpenAssignPicker,
    /// `S` — assign the focused task an agent at `$HOME`.
    AssignAtHome,
    /// `r` — open the rename popup for the focused card.
    OpenRename,
    /// `t` — open the tag editor for the focused card.
    OpenTags,
    /// `1`–`4` — set the focused card's priority.
    SetPriority(TaskPriority),
    /// `x` — delete the focused card (undoable).
    DeleteSelected,
    /// `c` — clear the Done column (undoable).
    ClearDone,
    /// `f`/Enter — attach a live agent, resume a dead one, or explain.
    FocusAgent,
    /// `P` — promote the focused card into a registered project's Backlog.
    PromoteSelected,
    /// Task-input Enter (add or rename).
    SubmitInput,
    /// Task-tags Enter.
    SubmitTags,
    /// Task-filter Enter (keep the query applied).
    ApplyFilter,
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
            Command::Tasks(c) => self.execute_tasks(c),
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

    fn execute_tasks(&mut self, cmd: TasksCommand) -> Vec<Effect> {
        use TasksCommand::*;
        match cmd {
            NavRight => {
                self.tasks.col_right();
                Vec::new()
            }
            NavLeft => {
                self.tasks.col_left();
                Vec::new()
            }
            NavDown => {
                self.tasks.row_down();
                Vec::new()
            }
            NavUp => {
                self.tasks.row_up();
                Vec::new()
            }
            MoveTaskRight => {
                match self.move_selected_task(1) {
                    Some(msg) => self.set_status(msg),
                    None => self.set_status("nothing to move right".into()),
                }
                Vec::new()
            }
            MoveTaskLeft => {
                match self.move_selected_task(-1) {
                    Some(msg) => self.set_status(msg),
                    None => self.set_status("nothing to move left".into()),
                }
                Vec::new()
            }
            OpenAddInput => {
                self.enter_task_input();
                Vec::new()
            }
            OpenFilter => {
                self.enter_task_filter();
                Vec::new()
            }
            ClearFilter => {
                self.clear_task_filter();
                Vec::new()
            }
            UndoDelete => {
                match self.undo_task_delete() {
                    Some(msg) => self.set_status(msg),
                    None => self.set_status("nothing to undo".into()),
                }
                Vec::new()
            }
            SpaceAction => {
                match self.task_space_action() {
                    Some(msg) => self.set_status(msg),
                    None => self.set_status("no task focused".into()),
                }
                Vec::new()
            }
            OpenAssignPicker => {
                if !self.enter_task_assign_picker() {
                    self.set_status("focus an unfinished task to assign an agent".into());
                }
                Vec::new()
            }
            AssignAtHome => {
                match self.assign_selected_task_at_home() {
                    Some(msg) => self.set_status(msg),
                    None => {
                        self.set_status("focus a To-Do/In Progress task to start an agent".into())
                    }
                }
                Vec::new()
            }
            OpenRename => {
                if !self.enter_task_rename() {
                    self.set_status("no task focused".into());
                }
                Vec::new()
            }
            OpenTags => {
                if !self.enter_task_tags() {
                    self.set_status("no task focused".into());
                }
                Vec::new()
            }
            SetPriority(priority) => {
                match self.set_selected_task_priority(priority) {
                    Some(msg) => self.set_status(msg),
                    None => self.set_status("no task focused".into()),
                }
                Vec::new()
            }
            DeleteSelected => {
                match self.delete_selected_task() {
                    Some(msg) => self.set_status(msg),
                    None => self.set_status("no task focused".into()),
                }
                Vec::new()
            }
            ClearDone => {
                self.clear_done_tasks();
                Vec::new()
            }
            FocusAgent => self.focus_task_agent(),
            PromoteSelected => self.promote_selected_task(),
            SubmitInput => {
                let renaming = self.tasks.renaming.is_some();
                if !self.submit_task_input() {
                    let msg = self.tasks.take_persistence_error().unwrap_or_else(|| {
                        if renaming {
                            "empty task — rename cancelled".into()
                        } else {
                            "empty task — nothing added".into()
                        }
                    });
                    self.set_status(msg);
                }
                Vec::new()
            }
            SubmitTags => {
                if !self.submit_task_tags() {
                    if let Some(msg) = self.tasks.take_persistence_error() {
                        self.set_status(msg);
                    }
                }
                Vec::new()
            }
            ApplyFilter => {
                self.apply_task_filter();
                Vec::new()
            }
        }
    }

    /// `f`/Enter on a board card: attach a live agent's tmux pane, resume a
    /// dead-but-resumable session in place (runtime spawn, rebinds `tmux`), or
    /// explain why neither is possible. The live-attach path is the only
    /// effect — pane sizing is bin's job.
    fn focus_task_agent(&mut self) -> Vec<Effect> {
        let Some(task) = self.selected_board_task().cloned() else {
            self.set_status("no task focused".into());
            return Vec::new();
        };
        let live_tmux = task
            .tmux
            .as_deref()
            .filter(|tmux| self.task_session_is_live(tmux));
        if let Some(tmux) = live_tmux {
            if let Some(sid) = task.session_id.as_deref() {
                self.set_status(format!("opened {} [{}]", models::short_sid(sid), tmux));
            }
            return vec![Effect::OpenTmuxPane {
                tmux: tmux.to_string(),
                owned: false,
            }];
        }
        if task.session_id.is_some() && task.cwd.is_some() {
            match self.resume_board_task(&task) {
                Ok(tmux) => {
                    if let Some(sid) = task.session_id.as_deref() {
                        self.set_status(format!("opened {} [{}]", models::short_sid(sid), tmux));
                    }
                    vec![Effect::OpenTmuxPane { tmux, owned: false }]
                }
                Err(e) => {
                    self.set_status(e);
                    Vec::new()
                }
            }
        } else {
            self.set_status(if task.tmux.is_some() {
                "agent session is gone and its session id was never seen — press s to re-assign"
                    .into()
            } else {
                "no agent assigned — press s to assign one".into()
            });
            Vec::new()
        }
    }

    /// `P` on a board card: promote it into the registered project whose root
    /// owns the card's `cwd`. Resolution canonicalizes both sides and accepts
    /// a card sitting *inside* a project root, picking the longest (most
    /// specific) matching root when several nest. On success the on-disk store
    /// already moved the record (`tasks::promote_task` write-then-delete); the
    /// in-memory board reloads to drop the promoted card and the cursor clamps.
    fn promote_selected_task(&mut self) -> Vec<Effect> {
        let Some(task) = self.selected_board_task().cloned() else {
            return Vec::new();
        };
        let Some(cwd) = task.cwd.as_deref() else {
            self.set_status(
                "no registered project matches this card's cwd — register one in the Projects tab first"
                    .into(),
            );
            return Vec::new();
        };
        let Some((project_id, project_name)) = resolve_project_for_cwd(cwd) else {
            self.set_status(
                "no registered project matches this card's cwd — register one in the Projects tab first"
                    .into(),
            );
            return Vec::new();
        };
        match crate::tasks::promote_task(&task.task_id, &project_id) {
            Ok(_) => {
                self.tasks.reload();
                self.tasks.clamp_row();
                self.set_status(format!("promoted to {} backlog", project_name));
            }
            Err(e) => self.set_status(format!("promote failed: {e}")),
        }
        Vec::new()
    }
}

/// Match `cwd` to a registered project: exact canonical-root match, or `cwd`
/// nested under a root. When roots nest, the longest match wins. Returns the
/// project `(id, name)`.
fn resolve_project_for_cwd(cwd: &str) -> Option<(String, String)> {
    let cwd_canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| std::path::PathBuf::from(cwd));
    let projects = crate::orchestrator::load_projects().projects;
    projects
        .into_iter()
        .filter_map(|p| {
            let root_canon = std::fs::canonicalize(&p.root).unwrap_or_else(|_| p.root.clone());
            (cwd_canon == root_canon || cwd_canon.starts_with(&root_canon))
                .then(|| (root_canon.components().count(), p.id, p.name))
        })
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, id, name)| (id, name))
}

// Unix-only: every test constructs an App, which touches the on-disk task
// store — with_temp_home isolation redirects $HOME, which only works on
// unix. Constructing an App in a test WITHOUT with_temp_home is how the
// board migration once ran against a developer's real ~/.cc-hub.
#[cfg(all(test, unix))]
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
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) =
                app_with(vec![session("sid-abc", SessionState::Inactive, None)]);
            let effects = app.execute(Command::Sessions(SessionsCommand::FocusSelected));
            assert!(effects.is_empty());
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].agent_id, "claude");
            assert_eq!(spawns[0].cwd, "/tmp/proj");
            assert_eq!(spawns[0].resume.as_deref(), Some("SessionId(\"sid-abc\")"));
            assert!(status(&app).starts_with("resumed"), "got: {}", status(&app));
        });
    }

    #[test]
    fn focus_inactive_pi_without_transcript_fails_without_spawn() {
        crate::test_util::with_temp_home(|| {
            let mut sess = session("sid-pi", SessionState::Inactive, None);
            sess.agent_kind = AgentKind::Pi;
            sess.jsonl_path = None;
            let (mut app, runtime) = app_with(vec![sess]);
            let effects = app.execute(Command::Sessions(SessionsCommand::FocusSelected));
            assert!(effects.is_empty());
            assert!(runtime.spawns.lock().unwrap().is_empty());
            assert_eq!(status(&app), "resume failed: missing session transcript");
        });
    }

    #[test]
    fn focus_live_tmux_session_opens_pane() {
        crate::test_util::with_temp_home(|| {
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
        });
    }

    #[test]
    fn focus_detached_session_focuses_window() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![session("sid-1", SessionState::Idle, None)]);
            let effects = app.execute(Command::Sessions(SessionsCommand::FocusSelected));
            assert_eq!(
                effects,
                vec![Effect::FocusWindow {
                    pid: 4242,
                    cwd: "/tmp/proj".into()
                }]
            );
        });
    }

    #[test]
    fn submit_prompt_empty_cancels() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![]);
            app.enter_prompt_input();
            app.prompt_buffer = "   ".into();
            let effects = app.execute(Command::Sessions(SessionsCommand::SubmitPrompt));
            assert!(effects.is_empty());
            assert!(runtime.spawns.lock().unwrap().is_empty());
            assert_eq!(status(&app), "empty prompt — dispatch cancelled");
        });
    }

    #[test]
    fn submit_prompt_with_idle_target_dispatches() {
        crate::test_util::with_temp_home(|| {
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
        });
    }

    #[test]
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
        crate::test_util::with_temp_home(|| {
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
        });
    }

    #[test]
    fn spawn_agent_here_records_and_watches() {
        crate::test_util::with_temp_home(|| {
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
        });
    }

    #[test]
    fn cycle_tab_into_metrics_requests_scan_once() {
        crate::test_util::with_temp_home(|| {
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
        });
    }

    #[test]
    fn toggles_flip_and_report() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![]);
            let before = app.sessions.show_inactive;
            app.execute(Command::Sessions(SessionsCommand::ToggleShowInactive));
            assert_eq!(app.sessions.show_inactive, !before);
            assert!(status(&app).starts_with("inactive sessions"));

            let before = app.sessions.show_orch_workers;
            app.execute(Command::Sessions(SessionsCommand::ToggleShowOrchWorkers));
            assert_eq!(app.sessions.show_orch_workers, !before);
            assert!(status(&app).starts_with("orchestrator/worker sessions"));
        });
    }

    #[test]
    fn open_detail_popup_requests_detail_for_selection() {
        crate::test_util::with_temp_home(|| {
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
        });
    }

    #[test]
    fn quit_sets_flag() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![]);
            app.execute(Command::Global(GlobalCommand::Quit));
            assert!(app.should_quit);
        });
    }

    // ---- Tasks-tab command flows ----

    use crate::app::PROCEED_PROMPT;
    use crate::orchestrator::TaskStatus;

    /// App on the Tasks tab wired to a recording runtime, no seeded sessions.
    fn task_app() -> (App, Arc<RecordingRuntime>) {
        let runtime = Arc::new(RecordingRuntime::default());
        let app = App::new_with_runtime(runtime.clone());
        (app, runtime)
    }

    fn tasks(app: &mut App, cmd: TasksCommand) -> Vec<Effect> {
        app.execute(Command::Tasks(cmd))
    }

    #[test]
    fn space_action_walks_planning_running_done_and_reopens() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = task_app();
            let id = app.tasks.board.add("ship it").unwrap().unwrap();
            // assign lands the card in Planning with a live tmux binding.
            app.tasks
                .board
                .assign(&id, "/tmp/proj", "claude", "mux-1")
                .unwrap();
            app.focus_task(&id);

            // Space on a Planning card proceeds: send_prompt to the live tmux,
            // card → Running.
            tasks(&mut app, TasksCommand::SpaceAction);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            assert_eq!(
                runtime.prompts.lock().unwrap().as_slice(),
                &[("mux-1".into(), PROCEED_PROMPT.into())]
            );

            // Space again finishes: card → Done, live session killed.
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::SpaceAction);
            assert_eq!(app.tasks.board.get(&id).unwrap().status, TaskStatus::Done);
            assert_eq!(runtime.kills.lock().unwrap().as_slice(), &["mux-1"]);

            // Space on the Done card reopens it into Backlog (To-Do).
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::SpaceAction);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Backlog
            );
        });
    }

    #[test]
    fn focus_agent_live_tmux_opens_pane() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = task_app();
            let id = app.tasks.board.add("look at it").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp/proj", "claude", "mux-live")
                .unwrap();
            app.focus_task(&id);
            runtime
                .exists
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let effects = tasks(&mut app, TasksCommand::FocusAgent);
            assert_eq!(
                effects,
                vec![Effect::OpenTmuxPane {
                    tmux: "mux-live".into(),
                    owned: false
                }]
            );
            // Live attach never respawns.
            assert!(runtime.spawns.lock().unwrap().is_empty());
        });
    }

    #[test]
    fn focus_agent_dead_tmux_resumes_and_rebinds() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = task_app();
            let id = app.tasks.board.add("resume me").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp/proj", "claude", "mux-dead")
                .unwrap();
            // Give the card a resumable session id (assign clears it) by
            // binding a scanned session that matches its tmux name.
            app.tasks
                .board
                .bind_sessions(&[session("sid-resume", SessionState::Idle, Some("mux-dead"))])
                .unwrap();
            app.focus_task(&id);
            // tmux is dead now; resume spawns a fresh session.
            runtime
                .exists
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let effects = tasks(&mut app, TasksCommand::FocusAgent);
            assert_eq!(
                effects,
                vec![Effect::OpenTmuxPane {
                    tmux: "mock-spawn".into(),
                    owned: false
                }]
            );
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(
                spawns[0].resume.as_deref(),
                Some("SessionId(\"sid-resume\")")
            );
            // The binding now points at the freshly-spawned session.
            assert_eq!(
                app.tasks.board.get(&id).unwrap().tmux.as_deref(),
                Some("mock-spawn")
            );
        });
    }

    #[test]
    fn focus_agent_without_binding_gives_guidance() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = task_app();
            let id = app.tasks.board.add("unassigned").unwrap().unwrap();
            app.focus_task(&id);
            let effects = tasks(&mut app, TasksCommand::FocusAgent);
            assert!(effects.is_empty());
            assert!(runtime.spawns.lock().unwrap().is_empty());
            assert_eq!(status(&app), "no agent assigned — press s to assign one");
        });
    }

    #[test]
    fn delete_then_undo_restores_card() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("delete me").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::DeleteSelected);
            assert!(app.tasks.board.get(&id).is_none());
            tasks(&mut app, TasksCommand::UndoDelete);
            assert!(app.tasks.board.get(&id).is_some());
        });
    }

    #[test]
    fn set_priority_updates_card() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("prioritize me").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::SetPriority(TaskPriority::P1));
            assert_eq!(app.tasks.board.get(&id).unwrap().priority, TaskPriority::P1);
        });
    }

    #[test]
    fn move_task_right_and_left_walk_columns() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("walk me").unwrap().unwrap();
            app.focus_task(&id);
            // Manual moves hop over Planning: Backlog → Running → Done.
            tasks(&mut app, TasksCommand::MoveTaskRight);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            tasks(&mut app, TasksCommand::MoveTaskRight);
            assert_eq!(app.tasks.board.get(&id).unwrap().status, TaskStatus::Done);
            // And back: Done → Running → Backlog.
            tasks(&mut app, TasksCommand::MoveTaskLeft);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            tasks(&mut app, TasksCommand::MoveTaskLeft);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Backlog
            );
        });
    }

    #[test]
    fn promote_selected_moves_card_into_project_backlog() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            // Register a project rooted at a real temp dir so canonicalize
            // resolves both sides identically.
            let root = std::env::temp_dir().join(format!("cchub-promote-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let root_str = root.display().to_string();
            let pid = crate::orchestrator::ensure_project_registered(&root, "promoteproj").unwrap();

            let id = app.tasks.board.add("promote me").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, &root_str, "claude", "mux-p")
                .unwrap();
            app.focus_task(&id);

            let effects = tasks(&mut app, TasksCommand::PromoteSelected);
            assert!(effects.is_empty());
            // Off the personal board.
            assert!(app.tasks.board.get(&id).is_none());
            // state.json landed under the project.
            let state_json = dirs::home_dir()
                .unwrap()
                .join(".cc-hub/projects")
                .join(&pid)
                .join("tasks")
                .join(&id)
                .join("state.json");
            assert!(state_json.exists(), "missing {}", state_json.display());
            assert!(
                status(&app).contains("promoteproj"),
                "status: {}",
                status(&app)
            );

            std::fs::remove_dir_all(&root).ok();
        });
    }

    #[test]
    fn promote_selected_without_matching_project_keeps_card() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("nowhere to go").unwrap().unwrap();
            // Assigned to a cwd no registered project owns.
            app.tasks
                .board
                .assign(&id, "/tmp/unregistered", "claude", "mux-x")
                .unwrap();
            app.focus_task(&id);
            let effects = tasks(&mut app, TasksCommand::PromoteSelected);
            assert!(effects.is_empty());
            // Card stays on the board.
            assert!(app.tasks.board.get(&id).is_some());
            assert!(
                status(&app).contains("no registered project"),
                "status: {}",
                status(&app)
            );
        });
    }
}
