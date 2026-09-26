use super::Effect;
use crate::agent::AgentKind;
use crate::app::{App, RenameSubmit, SessionsLayout};
use crate::config;
use crate::{models, spawn, title};

/// Sessions-tab commands, one per former `bin/src/keys.rs` arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionsCommand {
    NavUp,
    NavDown,
    NavLeft,
    NavRight,
    /// `i` — session-detail popup.
    OpenDetailPopup,
    /// `H` — toggle inactive sessions.
    ToggleShowInactive,
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
    /// `[agents.<id>].hotkey` — new session with that specific agent in the
    /// selected session's cwd, ignoring the runtime default agent. The id
    /// borrows from the process-wide config so the command stays `Copy`.
    SpawnAgentHereWith {
        agent_id: &'static str,
    },
    /// `N` — model picker for a new session in the selected session's cwd.
    OpenModelPicker,
    /// `A` — choose the default agent used by subsequent new sessions.
    OpenAgentPicker,
    /// `R` — respawn the selected session on another subscription account,
    /// continuing from its transcript ([`crate::respawn`]).
    OpenRespawnPicker,
    /// `p` — places picker.
    OpenPlacesPicker,
    /// `M` — bookmarks picker.
    OpenBookmarksPicker,
    /// `L` — task-link picker: group the selected session under a task.
    OpenTaskLinkPicker,
    /// `/` — archive-wide session finder: fuzzy-search every transcript on
    /// disk and reopen the pick.
    OpenSessionFinder,
    /// SessionFinder Enter.
    ConfirmSessionFinder,
    /// `r` — rename input.
    OpenRenameSession,
    /// RenameSession Enter.
    SubmitRename,
}

impl App {
    pub(super) fn execute_sessions(&mut self, cmd: SessionsCommand) -> Vec<Effect> {
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
                let agent_id = self.default_session_agent_id.clone();
                self.spawn_agent_here(agent_id);
                Vec::new()
            }
            SpawnAgentHereWith { agent_id } => {
                self.spawn_agent_here(agent_id.to_string());
                Vec::new()
            }
            OpenModelPicker => {
                self.enter_model_picker();
                Vec::new()
            }
            OpenAgentPicker => {
                self.enter_agent_picker();
                Vec::new()
            }
            OpenRespawnPicker => {
                if !self.enter_respawn_picker() {
                    let msg = if self.selected_session_info().is_none() {
                        "no session selected"
                    } else {
                        "no accounts configured — see ~/.cc-hub/resources.toml"
                    };
                    self.set_status(msg.into());
                }
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
            OpenSessionFinder => {
                self.enter_session_finder();
                vec![Effect::BuildSessionIndex]
            }
            ConfirmSessionFinder => self.confirm_session_finder(),
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
            OpenRenameSession => {
                if !self.enter_rename_session() {
                    self.set_status("no session selected to rename".into());
                }
                Vec::new()
            }
            SubmitRename => {
                match self.submit_session_rename() {
                    RenameSubmit::Persist { sid, title } => {
                        match title::persist_title(&sid, &title) {
                            Ok(()) => self.set_status(format!("renamed to “{}”", title)),
                            Err(e) => {
                                log::warn!("rename: persist failed for {}: {}", sid, e);
                                self.set_status(format!("rename failed: {}", e));
                            }
                        }
                    }
                    RenameSubmit::Deferred { title } => self.set_status(format!(
                        "named “{}” — applies when the session loads",
                        title
                    )),
                    RenameSubmit::Cancelled => {
                        self.set_status("rename cancelled — empty title".into())
                    }
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
            // Guard against duplicate live processes on one transcript. Pi
            // sessions resume by file (`pi --session <file>`), so a second
            // spawn puts two live `pi` processes on the SAME JSONL — they share
            // the transcript-derived session id, collapse into one flickering
            // card, and the loser gets mis-paired to an unrelated transcript by
            // the external-process scan. If a live session already owns this
            // transcript, attach to its pane instead of spawning a duplicate.
            // (Claude forks a fresh session id per resume, so it never collides
            // this way — but keying on the transcript path is correct for both.)
            if let Some(path) = session.jsonl_path.as_ref() {
                if let Some(tmux) = self
                    .sessions
                    .last_sessions
                    .iter()
                    .find(|s| {
                        s.state != models::SessionState::Inactive
                            && s.jsonl_path.as_ref() == Some(path)
                    })
                    .and_then(|s| s.tmux_session.clone())
                {
                    self.set_status(format!(
                        "attached live {} [{}]",
                        models::short_sid(&session.session_id),
                        tmux
                    ));
                    return vec![Effect::OpenTmuxPane { tmux, owned: false }];
                }
            }
            let resume = match session.agent_kind {
                // Claude and Codex both resume by session id (`codex resume
                // <uuid>`); Pi resumes by transcript file path.
                AgentKind::Claude | AgentKind::Codex => {
                    Some(spawn::SessionTarget::Resume(session.session_id.clone()))
                }
                AgentKind::Pi => session
                    .jsonl_path
                    .clone()
                    .map(spawn::SessionTarget::ResumeFile),
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

    /// `n`: fresh session using the current default agent in the selected
    /// session's cwd, with the spawn watchdog armed.
    fn spawn_agent_here(&mut self, agent_id: String) {
        let Some(sess) = self.selected_session_info().cloned() else {
            return;
        };
        let status = match self
            .runtime
            .spawn_session(&agent_id, &sess.cwd, None, None, None, false)
        {
            Ok(name) => {
                let label = config::get()
                    .agent(&agent_id)
                    .map(|agent| agent.display_label())
                    .unwrap_or_else(|| agent_id.clone());
                let status = format!("started {} [{}]", label, name);
                self.watch_spawn(name, agent_id, sess.cwd.clone());
                status
            }
            Err(e) => format!("spawn failed: {}", e),
        };
        self.set_status(status);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::{app_with, session, status};
    use crate::app::Command;
    use crate::models::SessionState;

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
            assert_eq!(spawns[0].resume.as_deref(), Some("Resume(\"sid-abc\")"));
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
    fn focus_inactive_pi_attaches_to_live_sibling_sharing_transcript() {
        crate::test_util::with_temp_home(|| {
            // A live pi pane and a stale "inactive" card both point at the SAME
            // transcript — the shape that made codex/pi sessions flicker. Two
            // `pi --session <file>` processes would write one JSONL and collapse
            // into a single churning card. Focusing the dead card must attach to
            // the live pane, never spawn a duplicate.
            let shared = std::path::PathBuf::from("/x/shared.jsonl");
            let mut live = session("sid-live", SessionState::Processing, Some("cc-pi-live"));
            live.agent_kind = AgentKind::Pi;
            live.jsonl_path = Some(shared.clone());
            let mut dead = session("sid-dead", SessionState::Inactive, None);
            dead.agent_kind = AgentKind::Pi;
            dead.jsonl_path = Some(shared.clone());

            let (mut app, runtime) = app_with(vec![live, dead]);
            // Park the cursor on the inactive card regardless of grid ordering.
            let (g, i) = app
                .sessions
                .groups
                .iter()
                .enumerate()
                .find_map(|(gi, grp)| {
                    grp.sessions
                        .iter()
                        .position(|s| s.session_id == "sid-dead")
                        .map(|si| (gi, si))
                })
                .expect("inactive card present");
            app.sessions.sel_group = g;
            app.sessions.sel_in_group = i;

            let effects = app.execute(Command::Sessions(SessionsCommand::FocusSelected));
            assert_eq!(
                effects,
                vec![Effect::OpenTmuxPane {
                    tmux: "cc-pi-live".into(),
                    owned: false
                }]
            );
            assert!(
                runtime.spawns.lock().unwrap().is_empty(),
                "must not spawn a duplicate pi process on the shared transcript"
            );
            assert!(
                status(&app).starts_with("attached live"),
                "got: {}",
                status(&app)
            );
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
    fn spawn_agent_here_with_uses_named_agent_not_default() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![session(
                "sid-1",
                SessionState::Processing,
                Some("cc-agent-1"),
            )]);
            app.execute(Command::Sessions(SessionsCommand::SpawnAgentHereWith {
                agent_id: "claude",
            }));
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].agent_id, "claude");
            assert!(status(&app).starts_with("started"), "got: {}", status(&app));
        });
    }

    #[test]
    fn selected_default_agent_drives_subsequent_new_sessions() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![session("sid-1", SessionState::Idle, None)]);
            app.agent_picker = Some(crate::app::AgentPickerState::new(
                "claude",
                vec![
                    crate::agent::AgentConfig {
                        id: "claude".into(),
                        kind: AgentKind::Claude,
                        command: "claude".into(),
                        use_bridge: false,
                        models: Vec::new(),
                    },
                    crate::agent::AgentConfig {
                        id: "pi-codex".into(),
                        kind: AgentKind::Pi,
                        command: "pi".into(),
                        use_bridge: true,
                        models: Vec::new(),
                    },
                ],
            ));
            app.view = crate::app::View::AgentPicker;
            app.agent_picker_move(1);
            app.confirm_default_session_agent();

            assert_eq!(app.default_session_agent_id(), "pi-codex");
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.agent_picker.is_none());

            app.execute(Command::Sessions(SessionsCommand::SpawnAgentHere));
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].agent_id, "pi-codex");
            assert_eq!(spawns[0].cwd, "/tmp/proj");
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
            assert_eq!(app.view, crate::app::View::Popup);

            let (mut app, _rt) = app_with(vec![]);
            let effects = app.execute(Command::Sessions(SessionsCommand::OpenDetailPopup));
            assert!(effects.is_empty());
        });
    }
}
