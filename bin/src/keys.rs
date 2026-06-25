//! Key-event dispatch extracted from `run()`'s event loop.
//!
//! [`handle_key`] is the ~85-arm `(View, KeyCode)` match that used to live
//! inline in `run()`. It is a mechanical move — every arm preserves its exact
//! original behavior and ordering. Arms that used to `continue` the outer
//! loop return [`KeyOutcome::Continue`]; arms that fell through return
//! [`KeyOutcome::Proceed`]. Since `run()` switched to draining whole input
//! bursts before its per-pass scan drain, it treats both outcomes the same;
//! the variants survive as documentation of each arm's original intent.

use crate::ScanMsg;
use cc_hub_lib::app::{App, Tab, View};
use cc_hub_lib::folder_picker::PickerMode;
use cc_hub_lib::{config, focus, live_view, models, platform, send, spawn, title, tmux_pane};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use tokio::sync::mpsc;

/// Historically: whether `run()` should skip the rest of its loop iteration
/// (the scan drain + pending-dispatch poll). `Continue` mirrors the
/// `continue` statements the match arms used when they lived inline. `run()`
/// now handles both variants identically — kept because the distinction
/// still documents which arms fully consumed their key.
pub(crate) enum KeyOutcome {
    Continue,
    Proceed,
}

/// Dispatch a single key press. `spawn_metrics` is the run()-local closure
/// that kicks the background metrics scan; it is threaded through as a
/// callback so the three arms that need it keep their exact behavior without
/// pulling the closure (which captures `run()` locals) out of `run()`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    terminal: &Terminal<CrosstermBackend<io::Stdout>>,
    scan_tx_main: &mpsc::Sender<ScanMsg>,
    detail_tx: &mpsc::Sender<String>,
    state_debug_tx: &mpsc::Sender<String>,
    spawn_metrics: &impl Fn(),
    on_sessions: bool,
    on_metrics: bool,
    on_projects: bool,
    on_tasks: bool,
) -> KeyOutcome {
    match (&app.view, key.code) {
        // Quit
        (View::Grid, KeyCode::Char('q')) => {
            app.should_quit = true;
        }
        // Shift+K / Tab cycles forward, Shift+J / BackTab cycles backward.
        (View::Grid, KeyCode::Tab | KeyCode::Char('K')) => {
            let was_metrics = app.current_tab == Tab::Metrics;
            app.cycle_tab();
            if !was_metrics && app.current_tab == Tab::Metrics && app.metrics.analysis.is_none() {
                spawn_metrics();
            }
        }
        (View::Grid, KeyCode::BackTab | KeyCode::Char('J')) => {
            let was_metrics = app.current_tab == Tab::Metrics;
            app.cycle_tab_back();
            if !was_metrics && app.current_tab == Tab::Metrics && app.metrics.analysis.is_none() {
                spawn_metrics();
            }
        }
        (View::Grid, KeyCode::Char('m')) if on_sessions => {
            let needs_compute = app.metrics.analysis.is_none();
            app.set_tab(Tab::Metrics);
            if needs_compute {
                spawn_metrics();
            }
        }
        (View::Grid, KeyCode::Right | KeyCode::Char('l')) if on_sessions => {
            app.sessions.move_right()
        }
        (View::Grid, KeyCode::Left | KeyCode::Char('h')) if on_sessions => app.sessions.move_left(),
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_sessions => app.sessions.move_down(),
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_sessions => app.sessions.move_up(),
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_metrics => {
            app.metrics.nav_down();
        }
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_metrics => {
            app.metrics.nav_up();
        }
        // Tasks board: j/k moves within the focused column; h/l switches
        // column. Same muscle memory as the Projects kanban.
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_tasks => {
            app.tasks.row_down();
        }
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_tasks => {
            app.tasks.row_up();
        }
        (View::Grid, KeyCode::Right | KeyCode::Char('l')) if on_tasks => {
            app.tasks.col_right();
        }
        (View::Grid, KeyCode::Left | KeyCode::Char('h')) if on_tasks => {
            app.tasks.col_left();
        }
        (View::Grid, KeyCode::Char('a') | KeyCode::Char('n')) if on_tasks => {
            app.enter_task_input();
        }
        // Space is status-aware: approves a Planning card's plan (tells the
        // agent to proceed, card → In Progress), toggles Done elsewhere.
        (View::Grid, KeyCode::Char(' ')) if on_tasks => {
            match app.task_space_action() {
                Some(msg) => app.set_status(msg),
                None => app.set_status("no task focused".into()),
            }
        }
        (View::Grid, KeyCode::Char('s')) if on_tasks => {
            if !app.enter_task_assign_picker() {
                app.set_status("focus an unfinished task to assign an agent".into());
            }
        }
        // `S`: skip the picker — spawn the agent in $HOME for broad,
        // not-yet-project-shaped questions.
        (View::Grid, KeyCode::Char('S')) if on_tasks => match app.assign_selected_task_at_home() {
            Some(msg) => app.set_status(msg),
            None => app.set_status("focus a To-Do/In Progress task to start an agent".into()),
        },
        (View::Grid, KeyCode::Char('r')) if on_tasks => {
            if !app.enter_task_rename() {
                app.set_status("no task focused".into());
            }
        }
        // `t` opens the inline tag editor for the focused task.
        (View::Grid, KeyCode::Char('t')) if on_tasks => {
            if !app.enter_task_tags() {
                app.set_status("no task focused".into());
            }
        }
        // `1`–`4` set the focused task's priority; the column re-sorts P1-first
        // and the cursor follows the card.
        (View::Grid, KeyCode::Char(c @ ('1' | '2' | '3' | '4'))) if on_tasks => {
            use cc_hub_lib::tasks::TaskPriority;
            let priority = match c {
                '1' => TaskPriority::P1,
                '2' => TaskPriority::P2,
                '3' => TaskPriority::P3,
                _ => TaskPriority::P4,
            };
            match app.set_selected_task_priority(priority) {
                Some(msg) => app.set_status(msg),
                None => app.set_status("no task focused".into()),
            }
        }
        (View::Grid, KeyCode::Char('x')) if on_tasks => match app.delete_selected_task() {
            Some(msg) => app.set_status(msg),
            None => app.set_status("no task focused".into()),
        },
        (View::Grid, KeyCode::Char('c')) if on_tasks => {
            app.clear_done_tasks();
        }
        // `f`/Enter on a task mirrors the Sessions tab: attach the bound
        // agent's live tmux in the embedded pane; if the tmux died but the
        // session id is known, respawn with resume and rebind; otherwise
        // hint at `s`.
        (View::Grid, KeyCode::Char('f') | KeyCode::Enter) if on_tasks => {
            let Some(task) = app.selected_board_task().cloned() else {
                app.set_status("no task focused".into());
                return KeyOutcome::Continue;
            };
            let live_tmux = task
                .tmux
                .as_deref()
                .filter(|n| send::tmux_session_exists(n));
            if let Some(tmux_name) = live_tmux {
                let (cols, rows) = crate::popup_pane_size(terminal);
                match tmux_pane::TmuxPaneView::spawn(tmux_name, rows, cols) {
                    Ok(pane) => app.enter_tmux_pane(pane),
                    Err(e) => app.set_status(format!("tmux attach failed: {}", e)),
                }
            } else if let (Some(sid), Some(cwd)) = (task.session_id.clone(), task.cwd.clone()) {
                let agent_id = task.agent_id.clone().unwrap_or_else(|| "claude".into());
                match spawn::spawn_agent_session(
                    &agent_id,
                    &cwd,
                    Some(spawn::ResumeTarget::SessionId(sid.clone())),
                    None,
                    false,
                ) {
                    Ok(new_tmux) => {
                        app.rebind_task_tmux(&task.id, &new_tmux);
                        let (cols, rows) = crate::popup_pane_size(terminal);
                        match tmux_pane::TmuxPaneView::spawn(&new_tmux, rows, cols) {
                            Ok(pane) => {
                                app.set_status(format!(
                                    "resumed {} [{}]",
                                    models::short_sid(&sid),
                                    new_tmux
                                ));
                                app.enter_tmux_pane(pane);
                            }
                            Err(e) => app.set_status(format!(
                                "resumed [{}] but attach failed: {}",
                                new_tmux, e
                            )),
                        }
                    }
                    Err(e) => app.set_status(format!("resume failed: {}", e)),
                }
            } else if task.tmux.is_some() {
                app.set_status(
                    "agent session is gone and its session id was never seen — press s to re-assign"
                        .into(),
                );
            } else {
                app.set_status("no agent assigned — press s to assign one".into());
            }
        }
        (View::TaskInput, KeyCode::Esc) => {
            app.close_task_input();
        }
        (View::TaskInput, KeyCode::Backspace) => {
            app.tasks.input.pop();
        }
        (View::TaskInput, KeyCode::Enter) => {
            let renaming = app.tasks.renaming.is_some();
            if !app.submit_task_input() {
                app.set_status(if renaming {
                    "empty task — rename cancelled".into()
                } else {
                    "empty task — nothing added".into()
                });
            }
        }
        (View::TaskInput, KeyCode::Char(c)) => {
            app.tasks.input.push(c);
        }
        (View::TaskTags, KeyCode::Esc) => {
            app.close_task_tags();
        }
        (View::TaskTags, KeyCode::Backspace) => {
            app.tasks.input.pop();
        }
        (View::TaskTags, KeyCode::Enter) => {
            app.submit_task_tags();
        }
        (View::TaskTags, KeyCode::Char(c)) => {
            app.tasks.input.push(c);
        }
        // Kanban: j/k moves the row cursor within the focused
        // column; h/l switches column; H/L (or [/]) cycles project chips.
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_projects => {
            app.projects.task_next();
        }
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_projects => {
            app.projects.task_prev();
        }
        (View::Grid, KeyCode::Right | KeyCode::Char('l')) if on_projects => {
            app.projects.col_right();
        }
        (View::Grid, KeyCode::Left | KeyCode::Char('h')) if on_projects => {
            app.projects.col_left();
        }
        (View::Grid, KeyCode::Char(']') | KeyCode::Char('L')) if on_projects => {
            app.projects.move_down();
        }
        (View::Grid, KeyCode::Char('[') | KeyCode::Char('H')) if on_projects => {
            app.projects.move_up();
        }
        (View::Grid, KeyCode::Char(' ')) if on_projects => {
            use cc_hub_lib::app::ApproveOutcome;
            let target = app.selected_project_task().cloned();
            match app.approve_review_task() {
                ApproveOutcome::NotReviewTask => {
                    app.set_status("nothing to approve (focus a Review task)".into());
                }
                ApproveOutcome::DoneNoPr | ApproveOutcome::Failed => {}
                ApproveOutcome::PrApproved => {
                    let Some(task) = target else {
                        return KeyOutcome::Continue;
                    };
                    let short = cc_hub_lib::orchestrator::short_task_id(&task.task_id);
                    let Some(tmux_name) = task.orchestrator_tmux.clone() else {
                        // No orchestrator to drive the merge — roll
                        // the card back to Review so it stays
                        // actionable (re-approve / resurrect).
                        app.rollback_merging_to_review(&task.project_id, &task.task_id);
                        app.set_status(format!(
                            "approved {} but no live orchestrator — back to Review (press f to resurrect)",
                            short
                        ));
                        return KeyOutcome::Continue;
                    };
                    if !send::tmux_session_exists(&tmux_name) {
                        app.rollback_merging_to_review(&task.project_id, &task.task_id);
                        app.set_status(format!(
                            "approved {} but orchestrator [{}] is not live — back to Review (press f to resurrect)",
                            short, tmux_name
                        ));
                        return KeyOutcome::Continue;
                    }
                    let prompt = cc_hub_lib::orchestrator::build_review_approval_prompt(
                        &task.task_id,
                        &cc_hub_lib::orchestrator::resolve_cc_hub_bin(),
                    );
                    if send::pane_ready_for_input(&tmux_name) {
                        crate::spawn_dispatch(
                            scan_tx_main.clone(),
                            tmux_name.clone(),
                            prompt,
                            format!(
                                "approved {} and notified orchestrator [{}] to continue merge flow",
                                short, tmux_name
                            ),
                            format!("approved {} but orchestrator notify failed", short),
                        );
                    } else {
                        app.queue_pending_dispatch(tmux_name.clone(), prompt);
                        app.set_status(format!(
                            "approved {} — queued notify for orchestrator [{}] when idle",
                            short, tmux_name
                        ));
                    }
                }
            }
        }
        (View::Grid, KeyCode::Char('r')) if on_projects => {
            if !app.enter_projects_result() {
                app.set_status("no task selected".into());
            }
        }
        (View::ProjectsResult, KeyCode::Esc | KeyCode::Char('r') | KeyCode::Char('q')) => {
            app.close_projects_result();
        }
        (View::ProjectsResult, KeyCode::Down | KeyCode::Char('j')) => {
            app.projects.result_artifact_next();
        }
        (View::ProjectsResult, KeyCode::Up | KeyCode::Char('k')) => {
            app.projects.result_artifact_prev();
        }
        (View::ProjectsResult, KeyCode::PageDown) => {
            app.projects.result_scroll_by(10);
        }
        (View::ProjectsResult, KeyCode::PageUp) => {
            app.projects.result_scroll_by(-10);
        }
        (View::ProjectsResult, KeyCode::Char('c')) => {
            match app.selected_result_artifact().map(|a| a.path.clone()) {
                None => app.set_status("no artifact to copy".into()),
                Some(path) => match cc_hub_lib::clipboard::copy(&path) {
                    Ok(()) => app.set_status(format!("copied: {}", path)),
                    Err(e) => app.set_status(format!("copy failed: {}", e)),
                },
            }
        }
        (View::ProjectsResult, KeyCode::Char('e')) => {
            app.projects.toggle_result_artifact_expanded();
        }
        (View::ProjectsResult, KeyCode::Char('o')) => {
            match app.selected_result_artifact().map(|a| a.path.clone()) {
                None => app.set_status("no artifact to open".into()),
                Some(path) => {
                    let result = crate::open_path_detached(&path);
                    match result {
                        Ok(()) => app.set_status(format!("opening {}", path)),
                        Err(e) => app.set_status(format!("open failed: {}", e)),
                    }
                }
            }
        }
        (View::Grid, KeyCode::Char('c')) if on_projects => {
            match app.selected_project_task().map(|t| t.task_id.clone()) {
                None => app.set_status("no task selected".into()),
                Some(task_id) => match cc_hub_lib::clipboard::copy(&task_id) {
                    Ok(()) => app.set_status(format!("copied task id: {}", task_id)),
                    Err(e) => app.set_status(format!("copy failed: {}", e)),
                },
            }
        }
        (View::Grid, KeyCode::Char('N')) if on_projects => {
            // Register a project (folder picker), no task spawn.
            // Use `n` to start a task on an existing project.
            app.enter_folder_picker_for_register_only();
        }
        (View::Grid, KeyCode::Char('n')) if on_projects => {
            // Start a new task on the currently-selected project.
            if !app.enter_project_task_prompt_for_selected() {
                app.set_status("no project selected — press N to register one".into());
            }
        }
        (View::Grid, KeyCode::Enter) if on_projects => {
            // Open the orchestrator's tmux session embedded in the
            // TUI — same mechanism the Sessions view uses for `f`
            // / Enter on a live session.
            if let Some(task) = app.selected_project_task().cloned() {
                match task.orchestrator_tmux.as_deref() {
                    None => {
                        app.set_status("task has no orchestrator tmux session yet".into());
                    }
                    Some(tmux_name) => {
                        let (cols, rows) = crate::popup_pane_size(terminal);
                        match tmux_pane::TmuxPaneView::spawn(tmux_name, rows, cols) {
                            Ok(pane) => app.enter_tmux_pane(pane),
                            Err(e) => app.set_status(format!("open orchestrator failed: {}", e)),
                        }
                    }
                }
            } else {
                app.set_status("no task selected — focus a task on the kanban first".into());
            }
        }
        (View::Grid, KeyCode::Char('f')) if on_projects => {
            if let Some(task) = app.selected_project_task().cloned() {
                let live_tmux = task
                    .orchestrator_tmux
                    .as_deref()
                    .filter(|n| send::tmux_session_exists(n));
                let resurrectable = if live_tmux.is_none()
                    && matches!(
                        task.status,
                        cc_hub_lib::orchestrator::TaskStatus::Running
                            | cc_hub_lib::orchestrator::TaskStatus::Review
                            // The merge-approval prompt is idempotent,
                            // so re-spawning the orchestrator and
                            // re-pinging a Merging task is safe.
                            | cc_hub_lib::orchestrator::TaskStatus::Merging
                    ) {
                    cc_hub_lib::scanner::find_orchestrator_session(
                        &task.project_root,
                        &task.task_id,
                        task.orchestrator_agent_kind,
                        task.orchestrator_session_id.as_deref(),
                    )
                } else {
                    None
                };
                if let Some(tmux_name) = live_tmux {
                    let (cols, rows) = crate::popup_pane_size(terminal);
                    match tmux_pane::TmuxPaneView::spawn(tmux_name, rows, cols) {
                        Ok(pane) => app.enter_tmux_pane(pane),
                        Err(e) => app.set_status(format!("open orchestrator failed: {}", e)),
                    }
                } else if let Some(resume) = resurrectable {
                    let cwd = task.project_root.to_string_lossy().into_owned();
                    match spawn::spawn_agent_session(
                        &task.orchestrator_agent_id,
                        &cwd,
                        Some(resume.resume.clone()),
                        None,
                        false,
                    ) {
                        Ok(new_tmux) => {
                            if let Err(e) = cc_hub_lib::orchestrator::update_task_state(
                                &task.project_id,
                                &task.task_id,
                                |s| {
                                    s.orchestrator_tmux = Some(new_tmux.clone());
                                    s.orchestrator_session_id = Some(resume.session_id.clone());
                                },
                            ) {
                                app.set_status(format!(
                                    "resurrected [{}] but state write failed: {}",
                                    new_tmux, e
                                ));
                            }
                            // A Merging task's orchestrator died
                            // mid-merge; re-ping the (idempotent)
                            // approval prompt so the resumed session
                            // picks the merge flow back up. Fires
                            // once the session reports Idle.
                            if task.status == cc_hub_lib::orchestrator::TaskStatus::Merging {
                                let prompt = cc_hub_lib::orchestrator::build_review_approval_prompt(
                                    &task.task_id,
                                    &cc_hub_lib::orchestrator::resolve_cc_hub_bin(),
                                );
                                app.queue_pending_dispatch(new_tmux.clone(), prompt);
                            }
                            let (cols, rows) = crate::popup_pane_size(terminal);
                            match tmux_pane::TmuxPaneView::spawn(&new_tmux, rows, cols) {
                                Ok(pane) => {
                                    app.set_status(format!(
                                        "resumed orchestrator {} [{}]",
                                        models::short_sid(&resume.session_id),
                                        new_tmux
                                    ));
                                    app.enter_tmux_pane(pane);
                                }
                                Err(e) => app.set_status(format!(
                                    "resurrected [{}] but attach failed: {}",
                                    new_tmux, e
                                )),
                            }
                        }
                        Err(e) => app.set_status(format!("resurrect failed: {}", e)),
                    }
                } else if let Some(log_path) = cc_hub_lib::orchestrator::task_orchestrator_log_path(
                    &task.project_id,
                    &task.task_id,
                )
                .filter(|p| p.exists())
                {
                    let (cols, rows) = crate::popup_pane_size(terminal);
                    match spawn::spawn_log_viewer_tmux_session(&log_path) {
                        Ok(name) => match tmux_pane::TmuxPaneView::spawn_owned(&name, rows, cols) {
                            Ok(pane) => app.enter_tmux_pane(pane),
                            Err(e) => app.set_status(format!("log viewer attach failed: {}", e)),
                        },
                        Err(e) => app.set_status(format!("log viewer spawn failed: {}", e)),
                    }
                } else if matches!(
                    task.status,
                    cc_hub_lib::orchestrator::TaskStatus::Running
                        | cc_hub_lib::orchestrator::TaskStatus::Review
                ) {
                    let session_store = match task.orchestrator_agent_kind {
                        cc_hub_lib::agent::AgentKind::Claude => "~/.claude/projects/",
                        cc_hub_lib::agent::AgentKind::Pi => "~/.pi/agent/sessions/",
                    };
                    let detail = match task.orchestrator_session_id.as_deref() {
                        Some(sid) => format!(
                            "orchestrator dead — sid {} not found under {} (cwd {}); no JSONL contains orchestrator prompt for task {}",
                            models::short_sid(sid),
                            session_store,
                            task.project_root.display(),
                            &task.task_id,
                        ),
                        None => format!(
                            "orchestrator dead — no JSONL under {} contains orchestrator prompt for task {} (cwd {})",
                            session_store,
                            &task.task_id,
                            task.project_root.display(),
                        ),
                    };
                    app.set_status(detail);
                } else {
                    app.set_status("no orchestrator log available".into());
                }
            } else {
                app.set_status("no task selected — focus a task on the kanban first".into());
            }
        }
        (View::Grid, KeyCode::Char('R')) if on_projects => {
            app.enter_confirm_task_restart();
        }
        (View::Grid, KeyCode::Char('x')) if on_projects => {
            app.enter_confirm_task_delete();
        }
        (View::Grid, KeyCode::Char('X')) if on_projects => {
            app.enter_confirm_project_delete();
        }
        (View::Grid, KeyCode::Char('b')) if on_projects => {
            app.open_backlog();
        }
        (View::Backlog, KeyCode::Esc | KeyCode::Char('q')) => {
            app.close_backlog();
        }
        (View::Backlog, KeyCode::Down | KeyCode::Char('j')) => {
            app.projects.backlog_down();
        }
        (View::Backlog, KeyCode::Up | KeyCode::Char('k')) => {
            app.projects.backlog_up();
        }
        (View::Backlog, KeyCode::Char('x')) => {
            app.enter_confirm_backlog_task_delete();
        }
        // `s`/Enter starts the selected backlog task. Only bound
        // inside the Backlog popup: backlog tasks are filtered out
        // of every kanban column (see `App::kanban_column_tasks`),
        // so a Grid 's' alias could never find one to start and
        // only ever showed a "not in backlog" toast. The kanban's
        // own start-affordance is `b` (open the Backlog popup).
        (View::Backlog, KeyCode::Char('s') | KeyCode::Enter) => {
            let Some(p) = app.selected_project().cloned() else {
                app.set_status("no project selected".into());
                return KeyOutcome::Continue;
            };
            let Some(task) = app.selected_backlog_task().cloned() else {
                app.set_status("no task selected".into());
                return KeyOutcome::Continue;
            };
            if task.status != cc_hub_lib::orchestrator::TaskStatus::Backlog {
                app.set_status(format!(
                    "task is not in backlog (status = {:?})",
                    task.status
                ));
                return KeyOutcome::Continue;
            }
            match cc_hub_lib::orchestrator::start_backlog_task(&p.id, &task.task_id, None) {
                Ok((state, tmux_name, orch_prompt)) => {
                    if let Some(prompt) = orch_prompt {
                        app.queue_pending_dispatch(tmux_name.clone(), prompt);
                    }
                    log::info!(
                        "project task: started backlog {} orchestrator [{}]",
                        state.task_id,
                        tmux_name
                    );
                    app.set_status(format!(
                        "task started [{}], orchestrator [{}] starting…",
                        state.task_id, tmux_name
                    ));
                    app.close_backlog();
                    app.projects.request_focus(state.task_id.clone());
                }
                Err(e) => {
                    log::warn!("project task: start backlog failed: {}", e);
                    app.set_status(format!("start backlog failed: {}", e));
                }
            }
        }
        (View::Grid, KeyCode::Enter) if on_metrics => {
            if let Some(row) = app.selected_metrics_session().cloned() {
                let agent_kind = if platform::paths::pi_sessions_dir()
                    .as_ref()
                    .is_some_and(|dir| row.jsonl_path.starts_with(dir))
                {
                    cc_hub_lib::agent::AgentKind::Pi
                } else {
                    cc_hub_lib::agent::AgentKind::Claude
                };
                let lv = live_view::LiveView::review(
                    row.jsonl_path.clone(),
                    agent_kind,
                    row.peak_timestamp_ms,
                );
                if lv.messages.is_empty() {
                    app.set_status(format!(
                        "can't open {}: {} missing or empty",
                        models::short_sid(&row.session_id),
                        row.jsonl_path.display()
                    ));
                } else {
                    app.enter_live_tail(lv);
                }
            }
        }
        (View::Grid, KeyCode::Char('r')) if on_metrics => {
            app.metrics.analysis = None;
            spawn_metrics();
        }
        // 'i' for info popup (old Enter behavior)
        (View::Grid, KeyCode::Char('i')) if on_sessions => {
            if let Some(id) = app.selected_session_id() {
                let _ = detail_tx.send(id).await;
                app.enter_popup();
            }
        }
        (View::Grid, KeyCode::Char('D')) if on_sessions => {
            if let Some(id) = app.selected_session_id() {
                let _ = state_debug_tx.send(id).await;
                app.enter_state_debug();
            }
        }
        (View::StateDebug, KeyCode::Esc | KeyCode::Char('q')) => {
            app.close_state_debug();
        }
        (View::StateDebug, KeyCode::Down | KeyCode::Char('j')) => {
            app.debug_scroll_down();
        }
        (View::StateDebug, KeyCode::Up | KeyCode::Char('k')) => {
            app.debug_scroll_up();
        }
        (View::Grid, KeyCode::Char('H')) if on_sessions => {
            app.toggle_show_inactive();
            let state = if app.sessions.show_inactive {
                "shown"
            } else {
                "hidden"
            };
            app.set_status(format!("inactive sessions {}", state));
        }
        (View::Grid, KeyCode::Char('W')) if on_sessions => {
            app.toggle_show_orch_workers();
            let state = if app.sessions.show_orch_workers {
                "shown"
            } else {
                "hidden"
            };
            app.set_status(format!("orchestrator/worker sessions {}", state));
        }
        (View::Grid, KeyCode::Char('f') | KeyCode::Enter) if on_sessions => {
            if let Some(session) = app.selected_session_info().cloned() {
                if session.state == models::SessionState::Inactive {
                    let resume = match session.agent_kind {
                        cc_hub_lib::agent::AgentKind::Claude => {
                            Some(spawn::ResumeTarget::SessionId(session.session_id.clone()))
                        }
                        cc_hub_lib::agent::AgentKind::Pi => session
                            .jsonl_path
                            .clone()
                            .map(spawn::ResumeTarget::SessionFile),
                    };
                    let status = match resume {
                        Some(target) => match spawn::spawn_agent_session(
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
                    app.set_status(status);
                } else if let Some(tmux_name) = session.tmux_session.clone() {
                    let (cols, rows) = crate::popup_pane_size(terminal);
                    match tmux_pane::TmuxPaneView::spawn(&tmux_name, rows, cols) {
                        Ok(pane) => {
                            app.enter_tmux_pane(pane);
                        }
                        Err(e) => {
                            app.set_status(format!("tmux attach failed: {}", e));
                        }
                    }
                } else {
                    match focus::focus_window(session.pid) {
                        focus::FocusOutcome::Focused => {}
                        focus::FocusOutcome::NeedsReattach(name) => {
                            let msg = match spawn::attach_tmux_session(&name, &session.cwd) {
                                Ok(_) => format!("reattached terminal to {}", name),
                                Err(e) => format!("reattach failed: {}", e),
                            };
                            app.set_status(msg);
                        }
                        focus::FocusOutcome::Failed(msg) => {
                            app.set_status(msg);
                        }
                    }
                }
            }
        }
        (View::Grid, KeyCode::Char('o')) if on_sessions => {
            if let Some(session) = app.selected_session_info().cloned() {
                let (cols, rows) = crate::popup_pane_size(terminal);
                match spawn::spawn_shell_tmux_session(&session.cwd) {
                    Ok(tmux_name) => {
                        match tmux_pane::TmuxPaneView::spawn_owned(&tmux_name, rows, cols) {
                            Ok(pane) => app.enter_tmux_pane(pane),
                            Err(e) => app.set_status(format!("shell attach failed: {}", e)),
                        }
                    }
                    Err(e) => {
                        app.set_status(format!("shell spawn failed: {}", e));
                    }
                }
            }
        }
        (View::TmuxPane, KeyCode::F(1)) => {
            app.close_tmux_pane();
        }
        (View::TmuxPane, KeyCode::Char(c))
            if (c == 'v' || c == 'V')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let status = match cc_hub_lib::clipboard::paste() {
                Ok(text) if text.is_empty() => Some("clipboard empty".to_string()),
                Ok(text) => match app.tmux_pane.as_ref() {
                    Some(pane) => pane
                        .paste_text(&text)
                        .err()
                        .map(|e| format!("paste failed: {}", e)),
                    None => None,
                },
                Err(e) => Some(format!("paste failed: {}", e)),
            };
            if let Some(msg) = status {
                app.set_status(msg);
            }
        }
        (View::TmuxPane, _) => {
            if let Some(pane) = app.tmux_pane.as_mut() {
                pane.send_key(key);
            }
        }
        (View::Grid, KeyCode::Char('x')) if on_sessions => {
            app.enter_confirm_close();
        }
        (View::ConfirmClose, KeyCode::Char('y') | KeyCode::Char('Y')) => {
            if let Some(pending) = app.take_pending_project_delete() {
                let msg = match cc_hub_lib::orchestrator::remove_project(&pending.project_id) {
                    Ok(()) => format!("removed {}", pending.display),
                    Err(e) => format!("remove failed: {}", e),
                };
                // Selection may dangle past the now-removed project
                // until the next scan tick lands; reset to 0 so
                // we don't render one bad frame.
                app.projects.reset_project_cursor();
                app.set_status(msg);
            } else if let Some(pending) = app.take_pending_task_delete() {
                let msg = match cc_hub_lib::orchestrator::delete_task(
                    &pending.project_id,
                    &pending.task_id,
                ) {
                    Ok(d) => {
                        let mut segs: Vec<String> = Vec::new();
                        segs.push(
                            if d.orchestrator_killed {
                                "orch killed"
                            } else {
                                "no orch"
                            }
                            .to_string(),
                        );
                        if !d.worktrees_removed.is_empty() {
                            let n = d.worktrees_removed.len();
                            segs.push(format!(
                                "{} worktree{} removed",
                                n,
                                if n == 1 { "" } else { "s" }
                            ));
                        }
                        if !d.worktree_errors.is_empty() {
                            segs.push(format!("{} worktree error(s)", d.worktree_errors.len()));
                        }
                        if d.lock_released {
                            segs.push("lock released".into());
                        }
                        format!("deleted {} ({})", pending.display, segs.join(", "))
                    }
                    Err(e) => {
                        log::warn!("task delete: {}", e);
                        format!("delete failed: {}", e)
                    }
                };
                app.set_status(msg);
                if pending.from_backlog {
                    // Model may still include the just-removed task
                    // until the next scan tick; clamp so we don't
                    // render a stale out-of-range selection.
                    app.projects.clamp_backlog_cursor();
                }
            } else if let Some(pending) = app.take_pending_task_restart() {
                match cc_hub_lib::orchestrator::restart_task(
                    &pending.project_id,
                    &pending.task_id,
                    None,
                ) {
                    Ok((state, tmux_name, orch_prompt)) => {
                        if let Some(prompt) = orch_prompt {
                            app.queue_pending_dispatch(tmux_name.clone(), prompt);
                        }
                        log::info!(
                            "project task: restarted {} orchestrator [{}]",
                            state.task_id,
                            tmux_name
                        );
                        app.set_status(format!(
                            "restarted [{}], orchestrator [{}] starting…",
                            state.task_id, tmux_name
                        ));
                        app.projects.request_focus(state.task_id.clone());
                    }
                    Err(e) => {
                        log::warn!("project task: restart failed: {}", e);
                        app.set_status(format!("restart failed: {}", e));
                    }
                }
            } else if let Some(pending) = app.take_pending_close() {
                let ok = focus::close_window(pending.pid);
                let msg = if ok {
                    format!("closed {}", pending.display)
                } else {
                    format!("failed to close {}", pending.display)
                };
                app.set_status(msg);
            }
        }
        (
            View::ConfirmClose,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q'),
        ) => {
            app.cancel_confirm_close();
        }
        // Space: force selected session to display as Idle until new
        // activity advances its watermark.
        (View::Grid, KeyCode::Char(' ')) if on_sessions => {
            app.ack_selected();
        }
        (View::Grid, KeyCode::Char('n')) if on_sessions => {
            if let Some(sess) = app.selected_session_info().cloned() {
                let status = match spawn::spawn_agent_session(
                    &sess.agent_id,
                    &sess.cwd,
                    None,
                    None,
                    false,
                ) {
                    Ok(name) => format!("started {} [{}]", sess.agent_badge(), name),
                    Err(e) => format!("spawn failed: {}", e),
                };
                app.set_status(status);
            }
        }
        (View::Grid, KeyCode::Char('N')) if on_sessions => {
            app.enter_session_places_picker();
        }
        (View::Grid, KeyCode::Char('M')) if on_sessions => {
            if !app.enter_bookmarks_picker() {
                app.set_status("no bookmarks — press N then m on a folder to add one".into());
            }
        }
        // Places mode (task assign): printable keys type into the fuzzy
        // filter, so the generic picker bindings below (j/k/q/m/. etc.)
        // must not fire. Navigation is arrows plus ctrl-j/k and ctrl-n/p.
        (View::FolderPicker, code)
            if app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Places) =>
        {
            match code {
                KeyCode::Esc => app.close_folder_picker(),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    // Don't let an empty match list cancel the picker —
                    // pick_from_folder_picker closes on no selection.
                    if app
                        .folder_picker
                        .as_ref()
                        .is_some_and(|p| p.selected_path().is_some())
                    {
                        crate::pick_from_folder_picker(app);
                    }
                }
                KeyCode::Tab => app.toggle_places_picker_mode(),
                KeyCode::Down => {
                    if let Some(p) = app.folder_picker.as_mut() {
                        p.move_down();
                    }
                }
                KeyCode::Up => {
                    if let Some(p) = app.folder_picker.as_mut() {
                        p.move_up();
                    }
                }
                KeyCode::Backspace => {
                    if let Some(p) = app.folder_picker.as_mut() {
                        p.pop_filter();
                    }
                }
                KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(p) = app.folder_picker.as_mut() {
                        match c {
                            'j' | 'n' => p.move_down(),
                            'k' | 'p' => p.move_up(),
                            _ => {}
                        }
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(p) = app.folder_picker.as_mut() {
                        p.push_filter(c);
                    }
                }
                _ => {}
            }
        }
        (View::FolderPicker, KeyCode::Esc | KeyCode::Char('q')) => {
            app.close_folder_picker();
        }
        // Browse → back to the places list (no-op in the Projects flows).
        (View::FolderPicker, KeyCode::Tab) => {
            app.toggle_places_picker_mode();
        }
        (View::FolderPicker, KeyCode::Down | KeyCode::Char('j')) => {
            if let Some(p) = app.folder_picker.as_mut() {
                p.move_down();
            }
        }
        (View::FolderPicker, KeyCode::Up | KeyCode::Char('k')) => {
            if let Some(p) = app.folder_picker.as_mut() {
                p.move_up();
            }
        }
        (View::FolderPicker, KeyCode::Char('m')) => match app.toggle_selected_bookmark() {
            Some((true, path)) => app.set_status(format!("bookmarked {}", path)),
            Some((false, path)) => app.set_status(format!("unbookmarked {}", path)),
            None => app.set_status("no folder selected".into()),
        },
        (View::FolderPicker, KeyCode::Enter | KeyCode::Right | KeyCode::Char('l')) => {
            let bookmarks_mode = app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Bookmarks);
            if bookmarks_mode {
                crate::pick_from_folder_picker(app);
            } else if let Some(p) = app.folder_picker.as_mut() {
                p.descend();
            }
        }
        (View::FolderPicker, KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h')) => {
            if let Some(p) = app.folder_picker.as_mut() {
                p.ascend();
            }
        }
        (View::FolderPicker, KeyCode::Char(' ')) => {
            crate::pick_from_folder_picker(app);
        }
        (View::FolderPicker, KeyCode::Char('.')) => {
            // Bookmarks mode has no meaningful "current dir" —
            // the entries are absolute paths from disk — so
            // collapse `.` into the same action as space/Enter.
            let bookmarks_mode = app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Bookmarks);
            if bookmarks_mode {
                crate::pick_from_folder_picker(app);
            } else {
                let cwd = app
                    .folder_picker
                    .as_ref()
                    .map(|p| p.current_dir.display().to_string());
                if let Some(cwd) = cwd {
                    crate::dispatch_picked_cwd(app, &cwd);
                } else {
                    app.close_folder_picker();
                }
            }
        }
        (View::FolderPicker, KeyCode::Char('c')) => {
            if !app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Bookmarks)
            {
                app.enter_gh_create_input(false);
            }
        }
        (View::FolderPicker, KeyCode::Char('C')) => {
            if !app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Bookmarks)
            {
                app.enter_gh_create_input(true);
            }
        }
        (View::GhCreateInput, KeyCode::Esc) => {
            app.close_gh_create_input();
        }
        (View::GhCreateInput, KeyCode::Tab) => {
            if let Some(input) = app.gh_create_input.as_mut() {
                input.private = !input.private;
            }
        }
        (View::GhCreateInput, KeyCode::Backspace) => {
            if let Some(input) = app.gh_create_input.as_mut() {
                input.name.pop();
            }
        }
        (View::GhCreateInput, KeyCode::Char(c)) => {
            if let Some(input) = app.gh_create_input.as_mut() {
                input.name.push(c);
            }
        }
        (View::GhCreateInput, KeyCode::Enter) => {
            let name_empty = app
                .gh_create_input
                .as_ref()
                .is_none_or(|i| i.name.trim().is_empty());
            if name_empty {
                app.set_status("repo name cannot be empty".into());
                return KeyOutcome::Continue;
            }
            if let Some((cwd, name, private)) = app.submit_gh_create_input() {
                let trimmed = name.trim().to_string();
                let tx = scan_tx_main.clone();
                let name_for_msg = trimmed.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        cc_hub_lib::gh::create_repo(&cwd, &trimmed, private)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("task panicked: {}", e)));
                    let _ = tx
                        .send(ScanMsg::GhCreateDone {
                            name: name_for_msg,
                            result,
                        })
                        .await;
                });
                app.set_status(format!(
                    "creating {} repo {}…",
                    if private { "private" } else { "public" },
                    name
                ));
            }
        }
        (View::Grid, KeyCode::Char('p')) if on_sessions => {
            app.enter_prompt_input();
        }
        (View::Grid, KeyCode::Char('t')) if on_sessions => {
            app.enter_todo_panel();
        }
        (View::Grid, KeyCode::Char('r')) if on_sessions => {
            if !app.enter_rename_session() {
                app.set_status("no session selected to rename".into());
            }
        }
        (View::RenameSession, KeyCode::Esc) => {
            app.close_rename_session();
        }
        (View::RenameSession, KeyCode::Backspace) => {
            app.rename_buffer.pop();
        }
        (View::RenameSession, KeyCode::Char(c)) => {
            app.rename_buffer.push(c);
        }
        (View::RenameSession, KeyCode::Enter) => match app.submit_session_rename() {
            Some((sid, title)) => match title::persist_title(&sid, &title) {
                Ok(()) => app.set_status(format!("renamed to “{}”", title)),
                Err(e) => {
                    log::warn!("rename: persist failed for {}: {}", sid, e);
                    app.set_status(format!("rename failed: {}", e));
                }
            },
            None => app.set_status("rename cancelled — empty title".into()),
        },
        (View::PromptInput, KeyCode::Esc) => {
            app.close_prompt_input();
        }
        (View::PromptInput, KeyCode::Tab) => {
            app.cycle_pending_agent_id();
        }
        (View::PromptInput, KeyCode::Backspace) => {
            app.prompt_buffer.pop();
        }
        (View::PromptInput, KeyCode::Char(c)) => {
            app.prompt_buffer.push(c);
        }
        (View::PromptInput, KeyCode::Enter) => {
            if app.prompt_buffer.trim().is_empty() {
                app.close_prompt_input();
                app.set_status("empty prompt — dispatch cancelled".into());
                return KeyOutcome::Continue;
            }

            // Projects-tab flow: create task, spawn orchestrator,
            // queue the orchestrator prompt for dispatch when Idle.
            if app.prompt_input_for_project() {
                let Some((cwd, prompt, agent_id)) = app.submit_project_task() else {
                    app.set_status("project task: missing cwd".into());
                    return KeyOutcome::Continue;
                };
                let project_root = std::path::Path::new(&cwd);
                let project_name = project_root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| cwd.clone());
                match cc_hub_lib::orchestrator::spawn_orchestrator_for_new_task(
                    project_root,
                    &project_name,
                    prompt,
                    agent_id.as_deref(),
                ) {
                    Ok((state, tmux_name, orch_prompt)) => {
                        if let Some(prompt) = orch_prompt {
                            app.queue_pending_dispatch(tmux_name.clone(), prompt);
                        }
                        log::info!(
                            "project task: created {} in {}, orchestrator [{}]",
                            state.task_id,
                            cwd,
                            tmux_name
                        );
                        app.set_status(format!(
                            "task created [{}], orchestrator [{}] starting…",
                            state.task_id, tmux_name
                        ));
                    }
                    Err(e) => {
                        log::warn!("project task: spawn failed: {}", e);
                        app.set_status(format!("project task failed: {}", e));
                    }
                }
                return KeyOutcome::Continue;
            }

            let target = app.dispatch_target().cloned();
            let prompt = app.submit_prompt_input();

            if let Some((pid, name, tmux)) = target {
                log::info!(
                    "dispatch: idle target {} (PID {}) [{}] prompt_len={}",
                    name,
                    pid,
                    tmux,
                    prompt.len()
                );
                crate::spawn_dispatch(
                    scan_tx_main.clone(),
                    tmux.clone(),
                    prompt,
                    format!("dispatched to {} (PID {}) [{}]", name, pid, tmux),
                    "dispatch failed".to_string(),
                );
                return KeyOutcome::Continue;
            }

            let Some(cwd) = app.default_spawn_cwd() else {
                app.set_status("no idle agent and no cwd to spawn in".into());
                return KeyOutcome::Continue;
            };
            let agent_id = config::get().default_session_agent_id();
            let agent = config::get().agent(&agent_id);
            let supports_initial_prompt =
                agent.as_ref().is_some_and(|a| a.supports_initial_prompt());
            match spawn::spawn_agent_session(
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
                            tmux_name, cwd, prompt.len()
                        );
                        app.set_status(format!(
                            "no idle agent — spawned {} [{}]",
                            agent_id, tmux_name
                        ));
                    } else {
                        log::info!(
                            "dispatch: no idle agent, spawned [{}] in {} — queueing prompt (len={})",
                            tmux_name, cwd, prompt.len()
                        );
                        app.queue_pending_dispatch(tmux_name.clone(), prompt);
                        app.set_status(format!(
                            "no idle agent — spawned {} [{}], prompt queued",
                            agent_id, tmux_name
                        ));
                    }
                }
                Err(e) => {
                    log::warn!("dispatch: auto-spawn failed: {}", e);
                    app.set_status(format!("auto-spawn failed: {}", e));
                }
            }
        }
        // To-do side panel — add-task input mode (these guarded arms come
        // first so typed characters edit the buffer instead of triggering
        // the navigation commands below).
        (View::TodoPanel, KeyCode::Esc) if app.todo.adding => {
            app.todo.cancel_add();
        }
        (View::TodoPanel, KeyCode::Enter) if app.todo.adding => {
            app.todo.commit_add();
        }
        (View::TodoPanel, KeyCode::Backspace) if app.todo.adding => {
            app.todo.input.pop();
        }
        (View::TodoPanel, KeyCode::Char(c)) if app.todo.adding => {
            app.todo.input.push(c);
        }
        // To-do side panel — navigation / commands (only reached when not
        // in add mode).
        (View::TodoPanel, KeyCode::Down | KeyCode::Char('j')) => {
            app.todo.move_down();
        }
        (View::TodoPanel, KeyCode::Up | KeyCode::Char('k')) => {
            app.todo.move_up();
        }
        (View::TodoPanel, KeyCode::Char(' ') | KeyCode::Enter) => {
            app.todo.toggle_selected();
        }
        (View::TodoPanel, KeyCode::Char('a') | KeyCode::Char('i')) => {
            app.todo.begin_add();
        }
        (View::TodoPanel, KeyCode::Char('d') | KeyCode::Char('x')) => {
            app.todo.delete_selected();
        }
        (View::TodoPanel, KeyCode::Char('c')) => {
            app.todo_clear_completed();
        }
        (View::TodoPanel, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('t')) => {
            app.close_todo_panel();
        }
        // Popup navigation
        (View::Popup, KeyCode::Esc | KeyCode::Char('q')) => app.close_popup(),
        (View::Popup, KeyCode::Down | KeyCode::Char('j')) => app.scroll_down(),
        (View::Popup, KeyCode::Up | KeyCode::Char('k')) => app.scroll_up(),
        // Live tail view
        (View::LiveTail, KeyCode::Esc | KeyCode::Char('q')) => {
            app.close_live_tail();
        }
        (View::LiveTail, KeyCode::Down | KeyCode::Char('j')) => {
            if let Some(ref mut lv) = app.live_view {
                lv.scroll_down();
            }
        }
        (View::LiveTail, KeyCode::Up | KeyCode::Char('k')) => {
            if let Some(ref mut lv) = app.live_view {
                lv.scroll_up();
            }
        }
        (View::LiveTail, KeyCode::Char('G')) => {
            if let Some(ref mut lv) = app.live_view {
                lv.scroll_bottom();
            }
        }
        _ => {}
    }
    KeyOutcome::Proceed
}
