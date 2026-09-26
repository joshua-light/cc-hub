//! The rename modal (`r`), including names typed for a session still
//! booting behind a spawn placeholder.

use super::spawn::{is_codex_process_only, spawning_session_id, spawning_tmux_of};
use crate::app::{App, View};

/// Outcome of committing the rename modal, so the command layer knows where
/// the typed title needs to go without re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameSubmit {
    /// Title bound to a real session id — the caller persists it to the title
    /// sidecar.
    Persist { sid: String, title: String },
    /// Title captured for a session still booting behind a spawn placeholder;
    /// it is persisted to the real id the moment the scanner sees it (see
    /// `App::adopt_pending_spawn_names`). Nothing to write yet.
    Deferred { title: String },
    /// Empty title — treated as a cancel.
    Cancelled,
}

impl App {
    /// Open the rename-title modal for the currently selected session,
    /// prefilling the edit buffer with its existing title. Returns `false`
    /// (and changes nothing) when no session is selected.
    pub fn enter_rename_session(&mut self) -> bool {
        let Some(session) = self.selected_session_info() else {
            return false;
        };
        let sid = if is_codex_process_only(session) {
            session
                .tmux_session
                .as_deref()
                .map(spawning_session_id)
                .unwrap_or_else(|| session.session_id.clone())
        } else {
            session.session_id.clone()
        };
        // A spawn placeholder's "title" is the synthetic "starting…" label,
        // not a real name — open its rename with an empty buffer so the user
        // types onto a clean line instead of clearing the placeholder first.
        let title = if spawning_tmux_of(&session.session_id).is_some() {
            String::new()
        } else {
            session.title.clone().unwrap_or_default()
        };
        self.rename_target = Some(sid);
        self.rename_buffer = title;
        self.view = View::RenameSession;
        true
    }

    pub fn close_rename_session(&mut self) {
        self.rename_buffer.clear();
        self.rename_target = None;
        self.view = View::Grid;
    }

    /// Title currently being edited, for the modal's "was X" subtitle.
    pub fn rename_original_title(&self) -> Option<&str> {
        let sid = self.rename_target.as_deref()?;
        // A spawn placeholder's "starting…" label isn't a real prior title —
        // report it as untitled so the boot-time prompt doesn't claim the
        // session "was starting…".
        if spawning_tmux_of(sid).is_some() {
            return None;
        }
        self.sessions
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter())
            .find(|s| s.session_id == sid)
            .and_then(|s| s.title.as_deref())
    }

    /// Commit the rename: apply the trimmed buffer to the in-memory session
    /// for instant feedback and hand the caller a [`RenameSubmit`] telling it
    /// where the title needs to go. An empty title is [`RenameSubmit::Cancelled`];
    /// either way the modal closes.
    ///
    /// A boot-time prompt targets a spawn placeholder whose synthetic id can't
    /// be persisted against, so it routes by the spawning tmux name: if the
    /// real session has already landed it persists now, otherwise the name is
    /// stashed for `Self::adopt_pending_spawn_names` to apply on arrival.
    pub fn submit_session_rename(&mut self) -> RenameSubmit {
        self.view = View::Grid;
        let Some(sid) = self.rename_target.take() else {
            return RenameSubmit::Cancelled;
        };
        let title = std::mem::take(&mut self.rename_buffer).trim().to_string();
        if title.is_empty() {
            return RenameSubmit::Cancelled;
        }
        if let Some(tmux) = spawning_tmux_of(&sid).map(str::to_string) {
            match self.real_session_id_for_tmux(&tmux) {
                Some(real_sid) => {
                    // The session loaded while the user was still typing — its
                    // real id is known, so persist straight to it.
                    self.pending_spawn_names.remove(&tmux);
                    self.apply_title_in_memory(&real_sid, &title);
                    RenameSubmit::Persist {
                        sid: real_sid,
                        title,
                    }
                }
                None => {
                    // Still booting: hold the name until the scanner issues a
                    // real id (the placeholder card shows it in the meantime).
                    self.pending_spawn_names.insert(tmux, Some(title.clone()));
                    RenameSubmit::Deferred { title }
                }
            }
        } else {
            self.apply_title_in_memory(&sid, &title);
            RenameSubmit::Persist { sid, title }
        }
    }

    /// Stamp `title` on the in-memory copies of session `sid` (the grid and
    /// the snapshot `rebuild_groups` derives from) so the rename shows
    /// instantly and a rebuild before the next scan doesn't revert it (same
    /// hazard as `ack_selected`). The next scan restores it from the sidecar.
    fn apply_title_in_memory(&mut self, sid: &str, title: &str) {
        for session in self
            .sessions
            .groups
            .iter_mut()
            .flat_map(|g| g.sessions.iter_mut())
            .chain(self.sessions.last_sessions.iter_mut())
        {
            if session.session_id == sid {
                session.title = Some(title.to_string());
                session.titling = false;
            }
        }
    }

    /// Real scanner-issued id of the live session hosted by `tmux`, if the
    /// scanner has seen it yet. `None` while a just-spawned session is still
    /// only a placeholder.
    fn real_session_id_for_tmux(&self, tmux: &str) -> Option<String> {
        self.sessions
            .last_sessions
            .iter()
            .find(|s| s.tmux_session.as_deref() == Some(tmux) && !is_codex_process_only(s))
            .map(|s| s.session_id.clone())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::fake_session;
    use crate::models::SessionState;

    // Boot-time naming: submitting a name before the scanner has produced the
    // real session stashes it against the spawning tmux, to be applied once
    // the session lands.
    #[test]
    fn boot_time_name_defers_until_the_session_loads() {
        let mut app = App::new();
        let tmux = "cchub-w-boot";
        app.pending_spawn_names.insert(tmux.into(), None);
        app.rename_target = Some(spawning_session_id(tmux));
        app.rename_buffer = "auth refactor".into();
        app.view = View::RenameSession;

        assert_eq!(
            app.submit_session_rename(),
            RenameSubmit::Deferred {
                title: "auth refactor".into()
            }
        );
        // Held against the tmux, awaiting the real session id.
        assert_eq!(
            app.pending_spawn_names.get(tmux),
            Some(&Some("auth refactor".to_string()))
        );
        assert_eq!(app.view, View::Grid);
    }

    // If the real session has already landed by the time the user hits Enter,
    // the name resolves straight to its real id — no deferral.
    #[test]
    fn boot_time_name_persists_to_real_id_when_already_loaded() {
        let mut app = App::new();
        let tmux = "cchub-w-loaded";
        let mut real = fake_session("real-id-xyz", SessionState::Idle);
        real.tmux_session = Some(tmux.into());
        app.sessions.last_sessions = vec![real];
        app.pending_spawn_names.insert(tmux.into(), None);
        app.rename_target = Some(spawning_session_id(tmux));
        app.rename_buffer = "auth refactor".into();
        app.view = View::RenameSession;

        assert_eq!(
            app.submit_session_rename(),
            RenameSubmit::Persist {
                sid: "real-id-xyz".into(),
                title: "auth refactor".into()
            }
        );
        // Resolved to the real id, so no longer pending.
        assert!(!app.pending_spawn_names.contains_key(tmux));
        // Applied in-memory so the card shows it before the next scan.
        assert_eq!(
            app.sessions.last_sessions[0].title.as_deref(),
            Some("auth refactor")
        );
    }

    #[test]
    fn boot_time_name_waits_through_codex_process_only_card() {
        let mut app = App::new();
        let tmux = "cchub-w-codex";
        let mut process_only = fake_session("codex-123", SessionState::Idle);
        process_only.agent_id = "codex".into();
        process_only.agent_kind = crate::agent::AgentKind::Codex;
        process_only.tmux_session = Some(tmux.into());
        app.sessions.last_sessions = vec![process_only.clone()];
        app.pending_spawn_names.insert(tmux.into(), None);
        app.rename_target = Some(spawning_session_id(tmux));
        app.rename_buffer = "stable name".into();
        app.view = View::RenameSession;

        assert_eq!(
            app.submit_session_rename(),
            RenameSubmit::Deferred {
                title: "stable name".into()
            }
        );
        assert!(app.update_sessions(vec![process_only]));
        assert_eq!(
            app.pending_spawn_names.get(tmux),
            Some(&Some("stable name".to_string()))
        );
        assert_eq!(
            app.sessions.last_sessions[0].title.as_deref(),
            Some("stable name")
        );
    }

    // Cancelling the boot-time prompt (empty title) keeps the `None` marker so
    // the load-time prompt stays suppressed — the user said no once already.
    #[test]
    fn empty_boot_time_name_cancels_but_keeps_suppression() {
        let mut app = App::new();
        let tmux = "cchub-w-cancel";
        app.pending_spawn_names.insert(tmux.into(), None);
        app.rename_target = Some(spawning_session_id(tmux));
        app.rename_buffer = "   ".into();
        app.view = View::RenameSession;

        assert_eq!(app.submit_session_rename(), RenameSubmit::Cancelled);
        assert_eq!(app.pending_spawn_names.get(tmux), Some(&None));
    }

    // Renaming a real (non-placeholder) session is unchanged: persist by its
    // own id, apply in-memory.
    #[test]
    fn renaming_a_real_session_persists_by_its_own_id() {
        let mut app = App::new();
        app.rename_target = Some("real-sid".into());
        app.rename_buffer = "my title".into();
        app.view = View::RenameSession;

        assert_eq!(
            app.submit_session_rename(),
            RenameSubmit::Persist {
                sid: "real-sid".into(),
                title: "my title".into()
            }
        );
    }

    // The boot placeholder opens its rename with an empty buffer, not the
    // synthetic "starting…" label.
    #[test]
    fn enter_rename_on_a_boot_placeholder_starts_empty() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-ph".into(), "claude".into(), "/tmp".into());
        // Idempotent whether or not watch_spawn already auto-opened it.
        assert!(app.enter_rename_session());
        assert_eq!(app.rename_buffer, "");
    }

    #[test]
    fn rename_on_codex_process_only_card_stays_deferred() {
        let mut app = App::new();
        let tmux = "cchub-w-codex-manual";
        let mut process_only = fake_session("codex-456", SessionState::Idle);
        process_only.agent_id = "codex".into();
        process_only.agent_kind = crate::agent::AgentKind::Codex;
        process_only.tmux_session = Some(tmux.into());
        app.update_sessions(vec![process_only]);

        assert!(app.enter_rename_session());
        assert_eq!(
            app.rename_target.as_deref(),
            Some(spawning_session_id(tmux).as_str())
        );
        app.rename_buffer = "manual name".into();
        assert_eq!(
            app.submit_session_rename(),
            RenameSubmit::Deferred {
                title: "manual name".into()
            }
        );
    }
}
