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

use super::{App, SessionsLayout, Tab};
use crate::agent::AgentKind;
use crate::orchestrator::TaskPriority;
use crate::{models, spawn, title};

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
    /// `v` — cycle the Sessions layout (card grid ↔ compact list).
    ToggleLayout,
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
    /// `N` — model picker for a new session in the selected session's cwd.
    OpenModelPicker,
    /// `p` — places picker.
    OpenPlacesPicker,
    /// `M` — bookmarks picker.
    OpenBookmarksPicker,
    /// `L` — task-link picker: group the selected session under a task.
    OpenTaskLinkPicker,
    /// `t` — todo side panel.
    OpenTodoPanel,
    /// `r` — rename input.
    OpenRenameSession,
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
    /// `v` — open the Task Info popup (prompt + attachments) for the
    /// focused card.
    OpenTaskInfo,
    /// `A` on the grid / `a` inside the Task Info popup — open the attach
    /// input (paste a file path or URL). `from_info` routes submit/cancel
    /// back to where it was opened.
    OpenAttachInput {
        from_info: bool,
    },
    /// Attach-input Enter.
    SubmitAttach,
    /// `x` inside the Task Info popup — remove the selected attachment.
    RemoveAttachment,
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
            // In the list layout every row is one column wide, so
            // left/right degenerate to up/down instead of cycling within
            // the group (a jump that reads as random on a linear list).
            NavRight => {
                match self.sessions.layout {
                    SessionsLayout::Grid => self.sessions.move_right(),
                    SessionsLayout::List => self.sessions.move_down(1),
                }
                Vec::new()
            }
            NavLeft => {
                match self.sessions.layout {
                    SessionsLayout::Grid => self.sessions.move_left(),
                    SessionsLayout::List => self.sessions.move_up(1),
                }
                Vec::new()
            }
            NavDown => {
                let cols = self.sessions_nav_cols();
                self.sessions.move_down(cols);
                Vec::new()
            }
            NavUp => {
                let cols = self.sessions_nav_cols();
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
            ToggleLayout => {
                self.toggle_sessions_layout();
                self.set_status(format!("sessions layout: {}", self.sessions.layout.label()));
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
            OpenModelPicker => {
                self.enter_model_picker();
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
            OpenTaskLinkPicker => {
                if !self.enter_task_link_picker() {
                    let msg = if self.selected_session_info().is_none() {
                        "no session selected"
                    } else {
                        "no tasks to link — add one on the Tasks tab first"
                    };
                    self.set_status(msg.into());
                }
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
        let status =
            match self
                .runtime
                .spawn_session(&sess.agent_id, &sess.cwd, None, None, None, false)
            {
                Ok(name) => {
                    let status = format!("started {} [{}]", sess.agent_badge(), name);
                    self.watch_spawn(name, sess.agent_id.clone(), sess.cwd.clone());
                    status
                }
                Err(e) => format!("spawn failed: {}", e),
            };
        self.set_status(status);
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
            OpenTaskInfo => {
                if !self.enter_task_info() {
                    self.set_status("no task focused".into());
                }
                Vec::new()
            }
            OpenAttachInput { from_info } => {
                if !self.enter_task_attach(from_info) {
                    self.set_status("no task focused".into());
                }
                Vec::new()
            }
            SubmitAttach => {
                if !self.submit_task_attach() {
                    let msg = self
                        .tasks
                        .take_persistence_error()
                        .unwrap_or_else(|| "attach cancelled — empty input".into());
                    self.set_status(msg);
                }
                Vec::new()
            }
            RemoveAttachment => {
                match self.remove_selected_attachment() {
                    Some(msg) => self.set_status(msg),
                    None => self.set_status("no task focused".into()),
                }
                Vec::new()
            }
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
    fn open_model_picker_captures_cwd_and_spawn_uses_selected_model() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![session(
                "sid-1",
                SessionState::Processing,
                Some("cc-agent-1"),
            )]);
            let effects = app.execute(Command::Sessions(SessionsCommand::OpenModelPicker));
            assert!(effects.is_empty());
            assert_eq!(app.view, crate::app::View::ModelPicker);
            let picker = app.model_picker.as_ref().expect("picker state");
            assert_eq!(picker.cwd, "/tmp/proj");
            assert_eq!(picker.selected, 0);

            app.model_picker_move(1);
            app.spawn_from_model_picker();
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.model_picker.is_none());
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(
                spawns[0].model.as_deref(),
                Some(crate::app::SPAWN_MODELS[1].1)
            );
            assert_eq!(spawns[0].cwd, "/tmp/proj");
            assert!(status(&app).starts_with("started"), "got: {}", status(&app));
        });
    }

    #[test]
    fn model_picker_move_clamps_to_list() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![]);
            app.execute(Command::Sessions(SessionsCommand::OpenModelPicker));
            app.model_picker_move(-1);
            assert_eq!(app.model_picker.as_ref().unwrap().selected, 0);
            app.model_picker_move(100);
            assert_eq!(
                app.model_picker.as_ref().unwrap().selected,
                crate::app::SPAWN_MODELS.len() - 1
            );
        });
    }

    #[test]
    fn model_picker_fuzzy_filters_labels_and_model_ids() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![]);
            app.execute(Command::Sessions(SessionsCommand::OpenModelPicker));
            let picker = app.model_picker.as_mut().unwrap();

            for c in "sn5".chars() {
                picker.push_filter(c);
            }
            assert_eq!(picker.rows.len(), 1);
            assert_eq!(
                picker.selected_model(),
                Some((
                    crate::app::SPAWN_MODELS[1].0,
                    Some(crate::app::SPAWN_MODELS[1].1)
                ))
            );
            assert!(!picker.rows[0].label_indices.is_empty());

            for _ in 0..3 {
                picker.pop_filter();
            }
            for c in "-f".chars() {
                picker.push_filter(c);
            }
            assert_eq!(picker.rows.len(), 1);
            assert_eq!(
                picker.selected_model(),
                Some((
                    crate::app::SPAWN_MODELS[2].0,
                    Some(crate::app::SPAWN_MODELS[2].1)
                ))
            );
            assert!(!picker.rows[0].detail_indices.is_empty());
        });
    }

    #[test]
    fn model_picker_tab_cycles_agents_and_their_model_choices() {
        use crate::agent::{default_claude_models, AgentConfig, AgentKind, AgentModel};

        let mut picker = crate::app::ModelPickerState::new(
            "/tmp/proj".into(),
            "claude".into(),
            vec![
                AgentConfig {
                    id: "pi-codex".into(),
                    kind: AgentKind::Pi,
                    command: "pi --provider openai-codex".into(),
                    use_bridge: true,
                    models: vec![
                        AgentModel {
                            label: "GPT-5.6".into(),
                            id: "gpt-5.6".into(),
                        },
                        AgentModel {
                            label: "Sol".into(),
                            id: "sol".into(),
                        },
                    ],
                },
                AgentConfig {
                    id: "claude".into(),
                    kind: AgentKind::Claude,
                    command: "claude".into(),
                    use_bridge: false,
                    models: default_claude_models(),
                },
            ],
        );
        picker.push_filter('s');

        picker.cycle_agent();

        assert_eq!(picker.agent_id, "pi-codex");
        assert!(picker.filter.is_empty());
        assert_eq!(picker.rows.len(), 2);
        assert_eq!(picker.selected_model(), Some(("GPT-5.6", Some("gpt-5.6"))));

        picker.cycle_agent();
        assert_eq!(picker.agent_id, "claude");
        assert_eq!(picker.rows.len(), crate::app::SPAWN_MODELS.len());
    }

    #[test]
    fn configured_agent_spawns_without_a_claude_model_override() {
        use crate::agent::{AgentConfig, AgentKind};

        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![]);
            app.model_picker = Some(crate::app::ModelPickerState::new(
                "/tmp/proj".into(),
                "pi-codex".into(),
                vec![AgentConfig {
                    id: "pi-codex".into(),
                    kind: AgentKind::Pi,
                    command: "pi --provider openai-codex --model gpt-5.5".into(),
                    use_bridge: true,
                    models: Vec::new(),
                }],
            ));
            app.view = crate::app::View::ModelPicker;

            app.spawn_from_model_picker();

            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].agent_id, "pi-codex");
            assert_eq!(spawns[0].model, None);
        });
    }

    #[test]
    fn configured_pi_model_is_forwarded_to_spawn() {
        use crate::agent::{AgentConfig, AgentKind, AgentModel};

        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![]);
            app.model_picker = Some(crate::app::ModelPickerState::new(
                "/tmp/proj".into(),
                "pi-codex".into(),
                vec![AgentConfig {
                    id: "pi-codex".into(),
                    kind: AgentKind::Pi,
                    command: "pi --provider openai-codex".into(),
                    use_bridge: true,
                    models: vec![AgentModel {
                        label: "GPT-5.6".into(),
                        id: "gpt-5.6".into(),
                    }],
                }],
            ));
            app.view = crate::app::View::ModelPicker;

            app.spawn_from_model_picker();

            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].agent_id, "pi-codex");
            assert_eq!(spawns[0].model.as_deref(), Some("gpt-5.6"));
        });
    }

    #[test]
    fn model_picker_no_match_does_not_spawn_or_close() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![]);
            app.execute(Command::Sessions(SessionsCommand::OpenModelPicker));
            for c in "xyz".chars() {
                app.model_picker.as_mut().unwrap().push_filter(c);
            }

            app.spawn_from_model_picker();

            assert_eq!(app.view, crate::app::View::ModelPicker);
            assert!(app.model_picker.is_some());
            assert!(runtime.spawns.lock().unwrap().is_empty());
        });
    }

    #[test]
    fn task_link_picker_links_then_unlinks_selected_session() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![session("sid-1", SessionState::Idle, None)]);
            let tid = app.tasks.board.add("fix the auth flow").unwrap().unwrap();

            // First open: no link yet, so no unlink row; Enter links.
            let effects = app.execute(Command::Sessions(SessionsCommand::OpenTaskLinkPicker));
            assert!(effects.is_empty());
            assert_eq!(app.view, crate::app::View::TaskLinkPicker);
            {
                let picker = app.task_link_picker.as_ref().expect("picker state");
                assert!(picker
                    .choices
                    .iter()
                    .all(|c| c.action != crate::app::TaskLinkAction::Unlink));
            }
            app.confirm_task_link_picker();
            assert_eq!(app.view, crate::app::View::Grid);
            assert_eq!(
                crate::session_tasks::load().get("sid-1").unwrap().task_id,
                tid
            );
            // The card badge resolves live (non-stale) from the board task;
            // the grid's groups are untouched by links.
            let badge = app.task_badge("sid-1").expect("badge");
            assert_eq!(badge.task_id, tid);
            assert!(!badge.stale);
            assert!(
                status(&app).starts_with("linked to"),
                "got: {}",
                status(&app)
            );

            // Second open: the unlink row leads and the linked task is
            // pre-selected; picking unlink drops the link and the badge.
            app.execute(Command::Sessions(SessionsCommand::OpenTaskLinkPicker));
            {
                let picker = app.task_link_picker.as_ref().expect("picker state");
                assert_eq!(picker.choices[0].action, crate::app::TaskLinkAction::Unlink);
                assert!(matches!(
                    picker.selected_action(),
                    Some(crate::app::TaskLinkAction::Link { task_id, .. }) if *task_id == tid
                ));
            }
            app.task_link_picker.as_mut().unwrap().move_selection(-100);
            app.confirm_task_link_picker();
            assert!(crate::session_tasks::load().is_empty());
            assert!(app.task_badge("sid-1").is_none());
            assert_eq!(status(&app), "task link removed");
        });
    }

    #[test]
    fn task_link_picker_without_tasks_reports_instead_of_opening() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![session("sid-1", SessionState::Idle, None)]);
            app.execute(Command::Sessions(SessionsCommand::OpenTaskLinkPicker));
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.task_link_picker.is_none());
            assert!(
                status(&app).starts_with("no tasks to link"),
                "got: {}",
                status(&app)
            );
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

    // ---- Task attachments ----

    /// A throwaway source file outside the (redirected) home, like a real
    /// research doc an agent left in some working dir.
    fn temp_doc(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cchub-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn attach_copies_file_into_personal_store() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("research the cache").unwrap().unwrap();
            app.focus_task(&id);
            let doc = temp_doc("research.md", "# findings\ncache is cold\n");

            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            assert_eq!(app.view, crate::app::View::TaskAttachInput);
            // The input opens in note mode; Tab over to file/URL mode.
            app.toggle_task_attach_mode();
            app.tasks.input = doc.display().to_string();
            tasks(&mut app, TasksCommand::SubmitAttach);

            assert_eq!(app.view, crate::app::View::Grid);
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            let a = &t.artifacts[0];
            assert_eq!(a.kind, "file");
            assert_eq!(a.original, doc.display().to_string());
            let stored = std::path::PathBuf::from(&a.path);
            assert!(
                stored.starts_with(dirs::home_dir().unwrap().join(".cc-hub/tasks").join(&id)),
                "copy must land inside the task's own dir, got {}",
                a.path
            );
            assert_eq!(
                std::fs::read_to_string(&stored).unwrap(),
                "# findings\ncache is cold\n"
            );
            assert!(
                status(&app).starts_with("attached research.md"),
                "got: {}",
                status(&app)
            );
            // Disk round-trip: a fresh board load sees the attachment too.
            assert_eq!(
                crate::tasks::PersonalBoard::load()
                    .get(&id)
                    .unwrap()
                    .artifacts
                    .len(),
                1
            );
            std::fs::remove_file(&doc).ok();
        });
    }

    #[test]
    fn attach_url_records_verbatim() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("read the RFC").unwrap().unwrap();
            app.focus_task(&id);

            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            app.toggle_task_attach_mode();
            // Trailing whitespace mimics a sloppy paste; submit trims it.
            app.tasks.input = "https://example.com/rfc  ".into();
            tasks(&mut app, TasksCommand::SubmitAttach);

            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            assert_eq!(t.artifacts[0].kind, "url");
            assert_eq!(t.artifacts[0].path, "https://example.com/rfc");
        });
    }

    #[test]
    fn attach_default_mode_types_note() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("jot it down").unwrap().unwrap();
            app.focus_task(&id);

            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            assert!(app.tasks.attach_note, "attach input must open in note mode");
            app.tasks.input = "check the cache TTL first".into();
            tasks(&mut app, TasksCommand::SubmitAttach);

            assert_eq!(app.view, crate::app::View::Grid);
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            let a = &t.artifacts[0];
            assert_eq!(a.kind, "note");
            assert_eq!(a.original, "typed");
            assert_eq!(a.caption.as_deref(), Some("check the cache TTL first"));
            assert_eq!(
                std::fs::read_to_string(&a.path).unwrap(),
                "check the cache TTL first"
            );
            assert!(
                status(&app).starts_with("attached note — “check the cache TTL first”"),
                "got: {}",
                status(&app)
            );
        });
    }

    #[test]
    fn attach_mode_toggles_and_resets_on_reopen() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("mode memory").unwrap().unwrap();
            app.focus_task(&id);

            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            app.tasks.input = "half-typed".into();
            app.toggle_task_attach_mode();
            assert!(!app.tasks.attach_note);
            assert_eq!(app.tasks.input, "half-typed", "buffer survives the flip");
            app.toggle_task_attach_mode();
            assert!(app.tasks.attach_note);

            // A path-mode session doesn't leak into the next open.
            app.toggle_task_attach_mode();
            app.close_task_attach();
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            assert!(app.tasks.attach_note, "reopen must start back in note mode");
            app.close_task_attach();
        });
    }

    #[test]
    fn attach_empty_input_cancels() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("nothing to see").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            tasks(&mut app, TasksCommand::SubmitAttach);
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.tasks.board.get(&id).unwrap().artifacts.is_empty());
            assert_eq!(status(&app), "attach cancelled — empty input");
        });
    }

    #[test]
    fn attach_missing_file_surfaces_error() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("bad path").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            app.toggle_task_attach_mode();
            app.tasks.input = "/nonexistent/research.md".into();
            tasks(&mut app, TasksCommand::SubmitAttach);
            assert!(app.tasks.board.get(&id).unwrap().artifacts.is_empty());
            assert!(
                status(&app).starts_with("task attach failed"),
                "got: {}",
                status(&app)
            );
        });
    }

    #[test]
    fn task_info_remove_deletes_file_and_fixes_lead() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("two docs").unwrap().unwrap();
            app.focus_task(&id);
            let doc1 = temp_doc("first.md", "one\n");
            let doc2 = temp_doc("second.md", "two\n");
            for doc in [&doc1, &doc2] {
                tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
                app.toggle_task_attach_mode();
                app.tasks.input = doc.display().to_string();
                tasks(&mut app, TasksCommand::SubmitAttach);
            }
            // Mark the second attachment as lead directly in the store; the
            // removal below must shift the designation, not drop it.
            crate::orchestrator::update_personal_task(&id, |s| s.lead_artifact = Some(1)).unwrap();
            app.tasks.reload();
            app.focus_task(&id);

            tasks(&mut app, TasksCommand::OpenTaskInfo);
            assert_eq!(app.view, crate::app::View::TaskInfo);
            assert_eq!(app.tasks.info_sel, 0);
            let stored0 =
                std::path::PathBuf::from(&app.tasks.board.get(&id).unwrap().artifacts[0].path);

            tasks(&mut app, TasksCommand::RemoveAttachment);
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            assert!(t.artifacts[0].original.ends_with("second.md"));
            assert_eq!(
                t.lead_artifact,
                Some(0),
                "lead must follow the shifted slot"
            );
            assert!(!stored0.exists(), "stored copy must be deleted");
            assert!(
                status(&app).starts_with("removed first.md"),
                "got: {}",
                status(&app)
            );
            std::fs::remove_file(&doc1).ok();
            std::fs::remove_file(&doc2).ok();
        });
    }

    #[test]
    fn attach_from_info_returns_to_popup_and_selects_new() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("popup flow").unwrap().unwrap();
            app.focus_task(&id);
            let doc1 = temp_doc("a.md", "a\n");
            let doc2 = temp_doc("b.md", "b\n");
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            app.toggle_task_attach_mode();
            app.tasks.input = doc1.display().to_string();
            tasks(&mut app, TasksCommand::SubmitAttach);

            tasks(&mut app, TasksCommand::OpenTaskInfo);
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: true });
            assert_eq!(app.view, crate::app::View::TaskAttachInput);
            app.toggle_task_attach_mode();
            app.tasks.input = doc2.display().to_string();
            tasks(&mut app, TasksCommand::SubmitAttach);
            assert_eq!(app.view, crate::app::View::TaskInfo);
            assert_eq!(app.tasks.info_sel, 1, "cursor lands on the new attachment");

            // Esc from an info-opened attach input returns to the popup too.
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: true });
            app.close_task_attach();
            assert_eq!(app.view, crate::app::View::TaskInfo);
            std::fs::remove_file(&doc1).ok();
            std::fs::remove_file(&doc2).ok();
        });
    }

    #[test]
    fn paste_text_attaches_note() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("collect findings").unwrap().unwrap();
            app.focus_task(&id);

            let msg = app
                .attach_text_to_selected("# Cache research\nthe cache is cold\n")
                .unwrap();
            assert!(msg.starts_with("attached note"), "got: {}", msg);

            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            let a = &t.artifacts[0];
            assert_eq!(a.kind, "note");
            assert_eq!(a.original, "clipboard");
            assert_eq!(a.caption.as_deref(), Some("# Cache research"));
            let stored = std::path::PathBuf::from(&a.path);
            assert!(
                stored.starts_with(dirs::home_dir().unwrap().join(".cc-hub/tasks").join(&id)),
                "note must land inside the task's own dir, got {}",
                a.path
            );
            assert!(stored.extension().is_some_and(|e| e == "md"));
            assert_eq!(
                std::fs::read_to_string(&stored).unwrap(),
                "# Cache research\nthe cache is cold\n"
            );
            // Round-trip through a fresh load.
            assert_eq!(
                crate::tasks::PersonalBoard::load()
                    .get(&id)
                    .unwrap()
                    .artifacts
                    .len(),
                1
            );
        });
    }

    #[test]
    fn paste_empty_clipboard_attaches_nothing() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("empty paste").unwrap().unwrap();
            app.focus_task(&id);
            let msg = app.attach_text_to_selected("   \n").unwrap();
            assert_eq!(msg, "clipboard empty — nothing attached");
            assert!(app.tasks.board.get(&id).unwrap().artifacts.is_empty());
        });
    }

    #[test]
    fn paste_notes_in_popup_get_distinct_files_and_selection() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("two notes").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::OpenTaskInfo);

            app.attach_text_to_selected("first note").unwrap();
            app.attach_text_to_selected("second note").unwrap();
            assert_eq!(app.tasks.info_sel, 1, "cursor follows the newest note");

            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 2);
            // Both notes were almost certainly written in the same second —
            // the collision probe must have kept their files apart.
            assert_ne!(t.artifacts[0].path, t.artifacts[1].path);
            assert_eq!(
                std::fs::read_to_string(&t.artifacts[0].path).unwrap(),
                "first note"
            );
            assert_eq!(
                std::fs::read_to_string(&t.artifacts[1].path).unwrap(),
                "second note"
            );
        });
    }

    #[test]
    fn task_info_without_focus_reports() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            tasks(&mut app, TasksCommand::OpenTaskInfo);
            assert_eq!(app.view, crate::app::View::Grid);
            assert_eq!(status(&app), "no task focused");
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
