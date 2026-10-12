//! The inline rename (`r`): the name is typed in the session's own title
//! slot, including for a session still booting behind a spawn placeholder,
//! and the names it wrote that no scan has read back yet.

use super::spawn::{is_codex_process_only, spawning_session_id, spawning_tmux_of};
use crate::app::{App, View};
use crate::models::SessionInfo;
use std::collections::HashMap;

/// Outcome of committing the rename, so the command layer knows where
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

/// Names this hub wrote to the title file that no scan has read back yet.
/// A scan reads the file when it starts and lands when it ends, and the
/// liveness pass re-sends the last full scan until the next one, so a
/// snapshot taken before a rename can arrive after it — without these, it
/// would put the old name back on the card.
#[derive(Default)]
pub(crate) struct UnscannedTitles(HashMap<String, String>);

impl UnscannedTitles {
    pub(crate) fn insert(&mut self, sid: &str, title: &str) {
        self.0.insert(sid.to_string(), title.to_string());
    }

    /// Put each name on its session in `sessions`. A name the snapshot
    /// already carries has been read back and is forgotten, as is one whose
    /// session the snapshot no longer has.
    pub(crate) fn stamp(&mut self, sessions: &mut [SessionInfo]) {
        self.0.retain(|sid, title| {
            let Some(session) = sessions.iter_mut().find(|s| s.session_id == *sid) else {
                return false;
            };
            if session.title.as_deref() == Some(title.as_str()) {
                return false;
            }
            session.title = Some(title.clone());
            session.titling = false;
            true
        });
    }
}

impl App {
    /// Open the inline rename on the currently selected session, prefilling the edit buffer with its existing title. Returns `false`
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

    /// Whether the open rename names a session still booting — the prompt a
    /// spawn opens. Naming is the last step before using the new session,
    /// so submitting it also focuses the session.
    pub fn names_a_spawn(&self) -> bool {
        self.rename_target
            .as_deref()
            .is_some_and(|target| spawning_tmux_of(target).is_some())
    }

    /// The name being typed for `session`, while the rename is open on it.
    /// A name typed at boot targets the spawn placeholder, and follows the
    /// session that replaces it by the tmux name they share.
    pub fn rename_draft(&self, session: &SessionInfo) -> Option<&str> {
        if self.view != View::RenameSession {
            return None;
        }
        let target = self.rename_target.as_deref()?;
        let is_target = target == session.session_id
            || spawning_tmux_of(target)
                .is_some_and(|tmux| session.tmux_session.as_deref() == Some(tmux));
        is_target.then_some(self.rename_buffer.as_str())
    }

    /// Commit the rename: apply the trimmed buffer to the in-memory session
    /// for instant feedback and hand the caller a [`RenameSubmit`] telling it
    /// where the title needs to go. An empty title is [`RenameSubmit::Cancelled`];
    /// either way the rename closes.
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
                    // real id. The placeholder shows it from this frame on —
                    // left to the next scan, the card would fall back to its
                    // "starting…" label for a tick once the draft closes.
                    self.pending_spawn_names.insert(tmux, title.clone());
                    self.rebuild_groups();
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
        let tmux = "hal-w-boot";
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
            Some(&"auth refactor".to_string())
        );
        assert_eq!(app.view, View::Grid);
    }

    // If the real session has already landed by the time the user hits Enter,
    // the name resolves straight to its real id — no deferral.
    #[test]
    fn boot_time_name_persists_to_real_id_when_already_loaded() {
        let mut app = App::new();
        let tmux = "hal-w-loaded";
        let mut real = fake_session("real-id-xyz", SessionState::Idle);
        real.tmux_session = Some(tmux.into());
        app.sessions.last_sessions = vec![real];
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
        let tmux = "hal-w-codex";
        let mut process_only = fake_session("codex-123", SessionState::Idle);
        process_only.agent_id = "codex".into();
        process_only.kind = crate::models::SessionKind::Agent(crate::agent::AgentKind::Codex);
        process_only.tmux_session = Some(tmux.into());
        app.sessions.last_sessions = vec![process_only.clone()];
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
            Some(&"stable name".to_string())
        );
        assert_eq!(
            app.sessions.last_sessions[0].title.as_deref(),
            Some("stable name")
        );
    }

    // Cancelling the boot-time prompt (empty title) stashes nothing: the
    // session lands unnamed.
    #[test]
    fn empty_boot_time_name_cancels_and_stashes_nothing() {
        let mut app = App::new();
        let tmux = "hal-w-cancel";
        app.rename_target = Some(spawning_session_id(tmux));
        app.rename_buffer = "   ".into();
        app.view = View::RenameSession;

        assert_eq!(app.submit_session_rename(), RenameSubmit::Cancelled);
        assert!(app.pending_spawn_names.is_empty());
    }

    // A name submitted while the session still boots is on the placeholder
    // the moment the draft closes — never the "starting…" label in between.
    #[test]
    fn a_boot_time_name_replaces_the_starting_label_at_once() {
        let mut app = App::new();
        let tmux = "hal-w-named";
        app.watch_spawn(tmux.into(), "claude".into(), "/tmp".into());
        assert!(app.enter_rename_session());
        app.rename_buffer = "Polish".into();

        app.submit_session_rename();
        assert_eq!(
            app.selected_session_info().and_then(|s| s.title.as_deref()),
            Some("Polish")
        );
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
        app.watch_spawn("hal-w-ph".into(), "claude".into(), "/tmp".into());
        // Idempotent whether or not watch_spawn already auto-opened it.
        assert!(app.enter_rename_session());
        assert_eq!(app.rename_buffer, "");
    }

    // The draft is drawn on the row being renamed: a boot-time name follows
    // the placeholder to the real session that replaces it, and no other row
    // shows it.
    #[test]
    fn the_draft_follows_the_placeholder_to_the_real_session() {
        let mut app = App::new();
        let tmux = "hal-w-draft";
        app.watch_spawn(tmux.into(), "claude".into(), "/tmp".into());
        assert!(app.enter_rename_session());
        app.rename_buffer = "Perf".into();

        let mut real = fake_session("real-id", SessionState::Idle);
        real.tmux_session = Some(tmux.into());
        let other = fake_session("other", SessionState::Idle);
        assert_eq!(app.rename_draft(&real), Some("Perf"));
        assert_eq!(app.rename_draft(&other), None);

        app.close_rename_session();
        assert_eq!(app.rename_draft(&real), None);
    }

    #[test]
    fn rename_on_codex_process_only_card_stays_deferred() {
        let mut app = App::new();
        let tmux = "hal-w-codex-manual";
        let mut process_only = fake_session("codex-456", SessionState::Idle);
        process_only.agent_id = "codex".into();
        process_only.kind = crate::models::SessionKind::Agent(crate::agent::AgentKind::Codex);
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

    // A boot-time name adopted on one scan survives the next, which read the
    // title file before the adoption wrote it.
    #[test]
    fn adopted_boot_time_name_survives_a_scan_that_predates_it() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            let tmux = "hal-w-race";
            app.watch_spawn(tmux.into(), "claude".into(), "/tmp".into());
            app.rename_target = Some(spawning_session_id(tmux));
            app.rename_buffer = "Performance".into();
            app.submit_session_rename();
            let landed = fake_session(tmux, SessionState::Idle);

            app.update_sessions(vec![landed.clone()]);
            app.update_sessions(vec![landed]);

            assert_eq!(crate::title::load()[tmux], "Performance");
            assert_eq!(
                app.sessions.last_sessions[0].title.as_deref(),
                Some("Performance")
            );
        });
    }
}
