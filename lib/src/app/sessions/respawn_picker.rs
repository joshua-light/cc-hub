//! The respawn picker (`R`): continue a session on another subscription
//! account.

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
        let last = self.choices.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
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
