//! The respawn picker (`R`): continue a session on another subscription
//! account.

use crate::app::picker_list::step;
use crate::app::{App, View};
use crate::models::SessionInfo;

/// One target account on offer in the respawn picker, with the continuation
/// it would use — planned eagerly at open so the row can say `resume` or
/// `handoff` (or why it can't happen) before the user commits.
#[derive(Clone, Debug)]
pub struct RespawnChoice {
    pub account_id: String,
    pub provider: crate::agent::AgentKind,
    pub plan: Result<crate::respawn::Continuation, String>,
}

/// State behind [`View::RespawnPicker`]. The source session is captured at
/// open so a rescan can't move it under the popup; accounts come from
/// `~/.cc-hub/resources.toml` in their registry order.
#[derive(Clone, Debug)]
pub struct RespawnPickerState {
    pub session: SessionInfo,
    pub choices: Vec<RespawnChoice>,
    pub selected: usize,
}

impl RespawnPickerState {
    pub(crate) fn new(session: SessionInfo, choices: Vec<RespawnChoice>) -> Self {
        // Preselect the likeliest target: the first same-provider account
        // that isn't the one the session already runs on.
        let selected = choices
            .iter()
            .position(|c| c.provider == session.agent_kind && c.account_id != session.agent_id)
            .unwrap_or(0);
        Self {
            session,
            choices,
            selected,
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.selected = step(self.selected, delta, self.choices.len());
    }

    pub fn selected_choice(&self) -> Option<&RespawnChoice> {
        self.choices.get(self.selected)
    }
}

impl App {
    /// `R` on the Sessions tab: offer every subscription account as a respawn
    /// target for the selected session. Continuations are planned now
    /// ([`crate::respawn::Continuation::plan`]) so each row already says how
    /// it would continue. Returns `false` when there is no session selected
    /// or no accounts are configured.
    pub fn enter_respawn_picker(&mut self) -> bool {
        let Some(session) = self.selected_session_info().cloned() else {
            return false;
        };
        let accounts = crate::resources::accounts();
        if accounts.is_empty() {
            return false;
        }
        let choices = accounts
            .iter()
            .map(|(id, account)| RespawnChoice {
                account_id: id.clone(),
                provider: account.provider,
                plan: crate::respawn::Continuation::plan(&session, id, account)
                    .map_err(|e| e.to_string()),
            })
            .collect();
        self.respawn_picker = Some(RespawnPickerState::new(session, choices));
        self.view = View::RespawnPicker;
        true
    }

    pub fn close_respawn_picker(&mut self) {
        self.respawn_picker = None;
        self.view = View::Grid;
    }

    pub fn respawn_picker_move(&mut self, delta: isize) {
        if let Some(picker) = self.respawn_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    /// Enter on the respawn picker: carry the transcript when the plan says
    /// so, then spawn the target account's agent continuing the session,
    /// watchdog armed and the old title carried over. The old session is
    /// deliberately left alone — it keeps its own transcript, and `x` closes
    /// it once the replacement is up.
    pub fn confirm_respawn_picker(&mut self) {
        use crate::respawn::Continuation;
        let Some(picker) = self.respawn_picker.take() else {
            return;
        };
        self.view = View::Grid;
        let Some(choice) = picker.choices.get(picker.selected) else {
            return;
        };
        let session = &picker.session;
        let plan = match &choice.plan {
            Ok(plan) => plan.clone(),
            Err(why) => {
                self.set_status(format!("cannot respawn on {}: {}", choice.account_id, why));
                return;
            }
        };
        let (target, prompt) = match plan {
            Continuation::Resume { session_id, carry } => {
                if let Some(carry) = carry {
                    if let Err(e) = carry.perform() {
                        self.set_status(format!("respawn failed: carry transcript: {}", e));
                        return;
                    }
                }
                (
                    Some(crate::spawn::SessionTarget::Resume(session_id)),
                    crate::respawn::RESUMED.to_string(),
                )
            }
            Continuation::Handoff { transcript } => {
                (None, crate::respawn::handoff_prompt(&transcript))
            }
        };
        match self.runtime.spawn_session(
            &choice.account_id,
            &session.cwd,
            target,
            Some(&prompt),
            None,
            false,
        ) {
            Ok(name) => {
                self.set_status(format!(
                    "respawned {} on {} [{}]",
                    crate::models::short_sid(&session.session_id),
                    choice.account_id,
                    name
                ));
                let title = session.title.clone().filter(|t| !t.is_empty());
                self.watch_spawn_titled(
                    name,
                    choice.account_id.clone(),
                    session.cwd.clone(),
                    title,
                );
            }
            Err(e) => self.set_status(format!("respawn failed: {}", e)),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::app::test_support::{app_with, session, status};
    use crate::app::{Command, SessionsCommand};
    use crate::models::{SessionInfo, SessionState};

    /// The four-account registry from contrib/resources.toml, homes
    /// under the (redirected) test $HOME.
    fn write_accounts_registry() {
        let dir = dirs::home_dir().unwrap().join(".cc-hub");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("resources.toml"),
            r#"
[accounts.cc-1]
provider = "claude"
home_mode = "default"

[accounts.cc-2]
provider = "claude"
home = "~/.claude-personal"

[accounts.codex-1]
provider = "codex"

[accounts.codex-2]
provider = "codex"
home = "~/.codex-personal"
"#,
        )
        .unwrap();
    }

    /// An inactive cc-1 Claude session whose transcript really exists under
    /// the test home, so a carry can be performed against it.
    fn respawnable_session() -> SessionInfo {
        let jsonl = dirs::home_dir()
            .unwrap()
            .join(".claude/projects/-tmp-proj/sid-abc.jsonl");
        std::fs::create_dir_all(jsonl.parent().unwrap()).unwrap();
        std::fs::write(&jsonl, "{\"type\":\"user\"}\n").unwrap();
        let mut s = session("sid-abc", SessionState::Inactive, None);
        s.agent_id = "cc-1".into();
        s.title = Some("Fix the auth flow".into());
        s.jsonl_path = Some(jsonl);
        s
    }

    #[test]
    fn respawn_picker_plans_every_account_and_preselects_the_sibling() {
        crate::test_util::with_temp_home(|| {
            write_accounts_registry();
            let (mut app, _rt) = app_with(vec![respawnable_session()]);
            let effects = app.execute(Command::Sessions(SessionsCommand::OpenRespawnPicker));
            assert!(effects.is_empty());
            assert_eq!(app.view, crate::app::View::RespawnPicker);

            let picker = app.respawn_picker.as_ref().expect("picker state");
            let ids: Vec<&str> = picker
                .choices
                .iter()
                .map(|c| c.account_id.as_str())
                .collect();
            assert_eq!(ids, ["cc-1", "cc-2", "codex-1", "codex-2"]);
            // The other Claude account is the likeliest target.
            assert_eq!(picker.choices[picker.selected].account_id, "cc-2");
            let labels: Vec<&str> = picker
                .choices
                .iter()
                .map(|c| c.plan.as_ref().unwrap().label())
                .collect();
            assert_eq!(labels, ["resume", "resume", "handoff", "handoff"]);
        });
    }

    #[test]
    fn respawn_on_claude_account_carries_the_transcript_and_resumes() {
        crate::test_util::with_temp_home(|| {
            write_accounts_registry();
            let (mut app, runtime) = app_with(vec![respawnable_session()]);
            app.execute(Command::Sessions(SessionsCommand::OpenRespawnPicker));
            app.confirm_respawn_picker();

            let carried = dirs::home_dir()
                .unwrap()
                .join(".claude-personal/projects/-tmp-proj/sid-abc.jsonl");
            assert!(carried.is_file(), "transcript must be copied to cc-2");
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].agent_id, "cc-2");
            assert_eq!(spawns[0].cwd, "/tmp/proj");
            assert_eq!(spawns[0].resume.as_deref(), Some("Resume(\"sid-abc\")"));
            assert_eq!(
                spawns[0].initial_prompt.as_deref(),
                Some(crate::respawn::RESUMED)
            );
            assert!(
                status(&app).starts_with("respawned"),
                "got: {}",
                status(&app)
            );
            // The old name travels with the session instead of a rename
            // prompt: it is staged for adoption under the new tmux name.
            assert_eq!(
                app.pending_spawn_names.get("mock-spawn"),
                Some(&Some("Fix the auth flow".into()))
            );
        });
    }

    #[test]
    fn respawn_on_codex_account_hands_off_with_the_transcript_prompt() {
        crate::test_util::with_temp_home(|| {
            write_accounts_registry();
            let (mut app, runtime) = app_with(vec![respawnable_session()]);
            app.execute(Command::Sessions(SessionsCommand::OpenRespawnPicker));
            {
                let picker = app.respawn_picker.as_mut().unwrap();
                picker.selected = picker
                    .choices
                    .iter()
                    .position(|c| c.account_id == "codex-1")
                    .unwrap();
            }
            app.confirm_respawn_picker();

            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].agent_id, "codex-1");
            assert_eq!(spawns[0].resume, None, "cross-provider must not resume");
            let prompt = spawns[0].initial_prompt.as_deref().unwrap_or_default();
            assert!(
                prompt.contains("sid-abc.jsonl"),
                "handoff prompt must point at the transcript, got: {}",
                prompt
            );
        });
    }

    #[test]
    fn respawn_without_accounts_reports_instead_of_opening() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![session("sid-1", SessionState::Idle, None)]);
            app.execute(Command::Sessions(SessionsCommand::OpenRespawnPicker));
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.respawn_picker.is_none());
            assert!(runtime.spawns.lock().unwrap().is_empty());
            assert!(
                status(&app).starts_with("no accounts configured"),
                "got: {}",
                status(&app)
            );
        });
    }
}
