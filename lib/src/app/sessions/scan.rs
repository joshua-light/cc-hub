//! Applying scan snapshots to the grid, plus the grid's view toggles and
//! acks.

use super::spawn::is_codex_process_only;
use super::SessionsLayout;
use crate::app::{App, Tab};
use crate::models::{SessionInfo, SessionState};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

impl App {
    pub fn toggle_show_inactive(&mut self) {
        self.sessions.show_inactive = !self.sessions.show_inactive;
        self.rebuild_groups();
    }

    pub fn toggle_sessions_layout(&mut self) {
        self.sessions.layout = match self.sessions.layout {
            SessionsLayout::Grid => SessionsLayout::List,
            SessionsLayout::List => SessionsLayout::Grid,
        };
        // The two layouts measure content height differently, so a
        // carried-over offset can strand the viewport past the content;
        // the renderer re-clamps onto the selection from zero next frame.
        self.render.grid_scroll = 0;
    }

    /// Column count the Sessions nav methods should move by: the derived
    /// grid column count, or 1 in the linear list layout.
    pub(crate) fn sessions_nav_cols(&self) -> u16 {
        match self.sessions.layout {
            SessionsLayout::Grid => self.render.grid_cols,
            SessionsLayout::List => 1,
        }
    }

    /// `tmux_session_name → SessionInfo` over the latest scan. Built fresh
    /// per call so it always reflects [`Self::last_sessions`]. Used by the
    /// Projects view to enrich task cards with live agent state (context
    /// tokens, current tool, idle/processing/waiting).
    pub fn sessions_by_tmux(&self) -> HashMap<&str, &SessionInfo> {
        let mut out = HashMap::new();
        for s in &self.sessions.last_sessions {
            if let Some(name) = s.tmux_session.as_deref() {
                out.insert(name, s);
            }
        }
        out
    }

    /// Stamp an ack for the currently-selected session, forcing it to display
    /// as Idle until new activity advances its watermark. Works for any
    /// non-Idle state (WaitingForInput or Processing).
    /// Returns true if an ack was recorded.
    pub fn ack_selected(&mut self) -> bool {
        let Some(session) = self.sessions.selected_session_info() else {
            return false;
        };
        if session.state == SessionState::Idle {
            return false;
        }
        let id = session.session_id.clone();
        let watermark = session.last_activity;
        self.sessions.acks.ack(&id, watermark);
        // Apply the downgrade to the snapshot rebuild_groups derives from, so
        // a rebuild before the next scan (filter toggle, projects snapshot)
        // doesn't revert the ack. The next scan re-derives it via is_acked.
        if let Some(s) = self
            .sessions
            .last_sessions
            .iter_mut()
            .find(|s| s.session_id == id)
        {
            s.state = SessionState::Idle;
        }
        // Rebuild immediately: the badge flips to idle and the card re-slots
        // into the idle bucket without waiting for the next scan tick.
        // adopt_groups re-anchors the cursor on the acked session's id, so
        // the selection follows the card to its new slot.
        self.rebuild_groups();
        true
    }

    /// Apply a fresh scan snapshot. Returns true when anything the renderer
    /// shows actually changed, so the caller can skip the repaint — and, more
    /// importantly, so unchanged ticks never rewrite the selection.
    ///
    /// Three tiers, cheapest first:
    /// - identical snapshot → no-op;
    /// - same cards in the same slots → swap card contents in place, leaving
    ///   the cursor alone (a state flip updates the badge, it does not
    ///   reshuffle the grid);
    /// - membership changed → full rebuild with restore-selection-by-id and
    ///   the new-session focus jump.
    pub fn update_sessions(&mut self, mut sessions: Vec<SessionInfo>) -> bool {
        // Tick transcripts belong to the Agents tab. They are headless
        // `claude -p` runs, so a card here could never be attached — `f`
        // would have nothing to open.
        let agent_sessions = crate::harness::AgentSessions::of(&self.harness.agents);
        if !agent_sessions.is_empty() {
            sessions.retain(|s| {
                agent_sessions
                    .owner(&s.session_id, std::path::Path::new(&s.cwd))
                    .is_none()
            });
        }
        // Refresh the session→task sidecar so links written by another
        // instance (or the CLI) regroup the grid without a restart — the
        // same per-tick re-read the scanner does for the title sidecar.
        self.session_task_links = crate::tasks::session_links::load();
        let acks_active = !self.sessions.acks.is_empty();
        if acks_active {
            // Apply user acks: if a non-Idle session is still at its acked
            // watermark, downgrade it to Idle. Any advance in last_activity clears
            // the ack inside is_acked(), so the real state takes over next tick.
            for s in &mut sessions {
                if s.state != SessionState::Idle
                    && s.state != SessionState::Inactive
                    && self.sessions.acks.is_acked(&s.session_id, s.last_activity)
                {
                    s.state = SessionState::Idle;
                }
            }
            let live_ids: HashSet<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
            self.sessions.acks.retain_existing(&live_ids);
        }

        self.last_refresh = Instant::now();

        let spawn_watch_fired = self.check_spawn_watches(&sessions, Instant::now());

        // Carry any name typed while a session was still booting onto the real
        // card the scanner has now produced — before grouping, so the title is
        // in place the first frame the real session renders.
        self.adopt_pending_spawn_names(&mut sessions);

        // Resolve task-board agent bindings: a freshly-assigned task knows
        // only its tmux name until the scanner sees the session; learning the
        // session id here is what lets `f` resume after the tmux dies.
        let task_bindings_changed = match self.tasks.board.bind_sessions(&sessions) {
            Ok(changed) => changed,
            Err(e) => {
                let message = format!("task session binding failed: {e}");
                self.tasks.persistence_error = Some(message.clone());
                if self.current_tab == Tab::Tasks {
                    self.set_status(message);
                }
                false
            }
        };

        let new_groups = self.build_groups(&sessions);
        let same_structure = new_groups.len() == self.sessions.groups.len()
            && new_groups.iter().zip(&self.sessions.groups).all(|(n, o)| {
                n.cwd == o.cwd
                    && n.sessions.len() == o.sessions.len()
                    && n.sessions
                        .iter()
                        .zip(&o.sessions)
                        .all(|(a, b)| a.session_id == b.session_id)
            });
        if same_structure {
            // Same cards in the same slots: refresh their contents and leave
            // the cursor untouched. No restore, no focus jump — a scan tick
            // that changes nothing the user can act on must not move the
            // selection out from under an in-flight keypress.
            let changed = new_groups != self.sessions.groups;
            self.sessions.groups = new_groups;
            self.sessions.last_sessions = sessions;
            return changed || task_bindings_changed || spawn_watch_fired;
        }

        self.sessions.last_sessions = sessions;
        self.adopt_groups(new_groups);

        let current_ids = self.sessions.visible_ids();
        // First tick seeds known ids without hijacking the cursor; later ticks
        // jump selection to a freshly-appeared session so it gets focus.
        let new_selection = self.sessions.known_session_ids.as_ref().and_then(|known| {
            self.sessions
                .position_of(|s| !known.contains(&s.session_id))
        });
        self.sessions.known_session_ids = Some(current_ids);
        if let Some((gi, si)) = new_selection {
            log::debug!(
                "scan: new session appeared, focus jump ({}, {}) -> ({}, {})",
                self.sessions.sel_group,
                self.sessions.sel_in_group,
                gi,
                si
            );
            self.sessions.sel_group = gi;
            self.sessions.sel_in_group = si;
            self.maybe_autoprompt_rename_for_new_session();
        }

        // A prompted spawn that has now materialised no longer needs its
        // suppression entry: the load-time autoprompt above has already been
        // held back for it this tick, and any name it carried was adopted
        // before grouping. Entries for spawns still booting are kept.
        let live_tmux: HashSet<String> = self
            .sessions
            .last_sessions
            .iter()
            .filter_map(|s| s.tmux_session.clone())
            .collect();
        self.pending_spawn_names.retain(|tmux, _| {
            if !live_tmux.contains(tmux) {
                return true;
            }
            self.sessions
                .last_sessions
                .iter()
                .any(|s| s.tmux_session.as_deref() == Some(tmux) && is_codex_process_only(s))
        });
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::fake_session;
    use crate::app::{Command, SessionsCommand};

    #[test]
    fn ack_survives_rebuild() {
        let mut app = App::new();
        let mut s = fake_session("only", SessionState::WaitingForInput);
        s.tmux_session = None;
        assert!(app.update_sessions(vec![s]));
        app.sessions.sel_group = 0;
        app.sessions.sel_in_group = 0;

        assert!(app.ack_selected());
        assert_eq!(
            app.selected_session_info().map(|s| s.state.clone()),
            Some(SessionState::Idle)
        );

        // A rebuild before the next scan (here via a filter toggle) must not
        // revert the ack — the mutation is mirrored onto last_sessions.
        app.toggle_show_inactive();
        assert_eq!(
            app.selected_session_info().map(|s| s.state.clone()),
            Some(SessionState::Idle),
            "ack must survive a rebuild before the next scan"
        );
    }

    #[test]
    fn ack_reslots_card_into_idle_bucket_immediately() {
        let mut app = App::new();
        let mut a = fake_session("A", SessionState::WaitingForInput);
        a.tmux_session = None;
        let mut b = fake_session("B", SessionState::WaitingForInput);
        b.tmux_session = None;
        assert!(app.update_sessions(vec![a, b]));
        assert_eq!(app.selected_session_id().as_deref(), Some("A"));

        // Acking A downgrades it to Idle, which must push it behind the
        // still-active B right away — not on the next scan tick — with the
        // cursor following the card.
        assert!(app.ack_selected());
        let order: Vec<&str> = app.sessions.groups[0]
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(order, vec!["B", "A"]);
        assert_eq!(app.selected_session_id().as_deref(), Some("A"));
        assert_eq!(
            app.selected_session_info().map(|s| s.state.clone()),
            Some(SessionState::Idle)
        );
    }

    #[test]
    fn toggle_sessions_layout_cycles_and_resets_scroll() {
        let mut app = App::new();
        // The app opens on the compact table, not the card wall.
        assert_eq!(app.sessions.layout, SessionsLayout::List);
        app.render.grid_scroll = 7;
        app.toggle_sessions_layout();
        assert_eq!(app.sessions.layout, SessionsLayout::Grid);
        assert_eq!(
            app.render.grid_scroll, 0,
            "a carried-over offset can strand the other layout's viewport"
        );
        app.toggle_sessions_layout();
        assert_eq!(app.sessions.layout, SessionsLayout::List);
    }

    #[test]
    fn list_layout_nav_is_linear() {
        let mut app = App::new();
        let mut a = fake_session("A", SessionState::Processing);
        a.tmux_session = None;
        let mut b = fake_session("B", SessionState::Processing);
        b.tmux_session = None;
        let mut c = fake_session("C", SessionState::Processing);
        c.tmux_session = None;
        assert!(app.update_sessions(vec![a, b, c]));
        app.sessions.layout = SessionsLayout::List;
        // Even with the grid reporting 3 columns, list nav moves one row at
        // a time — down and right are the same linear step.
        app.render.grid_cols = 3;
        app.execute(Command::Sessions(SessionsCommand::NavDown));
        assert_eq!(app.sessions.sel_in_group, 1);
        app.execute(Command::Sessions(SessionsCommand::NavRight));
        assert_eq!(app.sessions.sel_in_group, 2);
        app.execute(Command::Sessions(SessionsCommand::NavLeft));
        assert_eq!(app.sessions.sel_in_group, 1);
        app.execute(Command::Sessions(SessionsCommand::NavUp));
        assert_eq!(app.sessions.sel_in_group, 0);
    }

    #[test]
    fn rebuild_revealed_session_does_not_teleport_cursor() {
        let mut app = App::new();
        let mut a = fake_session("A", SessionState::WaitingForInput);
        a.tmux_session = None;
        let mut b = fake_session("B", SessionState::Inactive);
        b.tmux_session = None;

        // First scan: B is inactive and hidden, so only A is visible/known.
        assert!(app.update_sessions(vec![a.clone(), b.clone()]));
        assert_eq!(app.selected_session_id().as_deref(), Some("A"));

        // Toggling show_inactive reveals B via a rebuild (no scan). B must get
        // registered as known so it isn't mistaken for a fresh arrival later.
        app.toggle_show_inactive();

        // Next scan adds a genuinely new session C. The focus jump must land
        // on C, not teleport onto B — which the user has already been seeing.
        let mut c = fake_session("C", SessionState::WaitingForInput);
        c.tmux_session = None;
        assert!(app.update_sessions(vec![a, b, c]));
        assert_eq!(
            app.selected_session_id().as_deref(),
            Some("C"),
            "focus jump should target the new session, not the revealed one"
        );
    }

    /// Three sessions in one group, in scanner order. `fake_session` keys
    /// session_id off the tmux name, so ids are the given names.
    fn seed_three(app: &mut App) {
        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Idle),
            fake_session("b", SessionState::Idle),
            fake_session("c", SessionState::Idle),
        ]);
        assert!(changed, "first snapshot is a structure change");
    }

    fn grid_ids(app: &App) -> Vec<String> {
        app.sessions.groups[0]
            .sessions
            .iter()
            .map(|s| s.session_id.clone())
            .collect()
    }

    #[test]
    fn state_flip_updates_card_in_place_without_touching_selection() {
        let mut app = App::new();
        seed_three(&mut app);
        app.sessions.move_right();
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));

        // b flips between active flavors: same bucket, same slots —
        // content-only update. (Waking from Idle *does* re-slot; that case is
        // covered by waking_session_moves_ahead_of_idle_ones.)
        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Processing),
            fake_session("b", SessionState::WaitingForInput),
            fake_session("c", SessionState::Processing),
        ]);
        assert!(changed, "a state flip is a visible change");
        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Processing),
            fake_session("b", SessionState::Processing),
            fake_session("c", SessionState::Processing),
        ]);
        assert!(changed, "a state flip is a visible change");
        assert_eq!(grid_ids(&app), ["a", "b", "c"], "order must not change");
        assert_eq!(app.sessions.sel_in_group, 1, "cursor must not move");
        assert_eq!(
            app.sessions.groups[0].sessions[1].state,
            SessionState::Processing,
            "card content must refresh"
        );
    }

    #[test]
    fn waking_session_moves_ahead_of_idle_ones() {
        let mut app = App::new();
        seed_three(&mut app);
        app.sessions.move_right();
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));

        // b wakes up: it leaves the idle bucket and re-slots ahead of the
        // still-idle a and c; the cursor follows b's id to its new slot.
        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Idle),
            fake_session("b", SessionState::Processing),
            fake_session("c", SessionState::Idle),
        ]);
        assert!(changed, "waking is a visible change");
        assert_eq!(grid_ids(&app), ["b", "a", "c"]);
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));
    }

    #[test]
    fn identical_snapshot_reports_no_change_and_keeps_selection() {
        let mut app = App::new();
        seed_three(&mut app);
        app.sessions.move_right();

        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Idle),
            fake_session("b", SessionState::Idle),
            fake_session("c", SessionState::Idle),
        ]);
        assert!(!changed, "identical snapshot must not request a repaint");
        assert_eq!(app.sessions.sel_in_group, 1, "cursor must not move");
    }

    #[test]
    fn membership_change_rebuilds_and_follows_selected_id() {
        let mut app = App::new();
        seed_three(&mut app);
        app.sessions.move_right();
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));

        // a disappears: structure change → rebuild, selection follows b's id
        // to its new slot.
        let changed = app.update_sessions(vec![
            fake_session("b", SessionState::Idle),
            fake_session("c", SessionState::Idle),
        ]);
        assert!(changed);
        assert_eq!(grid_ids(&app), ["b", "c"]);
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));
        assert_eq!(app.sessions.sel_in_group, 0);
    }
}
