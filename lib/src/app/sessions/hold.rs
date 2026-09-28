//! Space: put the selected session on hold, or release it; see
//! [`crate::holds`].

use crate::app::App;

impl App {
    /// Move the selected session into the On hold section, or back out of
    /// it. The cursor keeps its slot rather than following the card: the
    /// card that slides in is the next one to look at.
    pub fn toggle_hold(&mut self) {
        let Some(session) = self.selected_session_info() else {
            return;
        };
        let (id, label) = (session.session_id.clone(), session.label());
        let held = self.sessions.holds.toggle(&id);

        let slot = (self.sessions.sel_group, self.sessions.sel_in_group);
        self.rebuild_groups();
        (self.sessions.sel_group, self.sessions.sel_in_group) = slot;
        self.sessions.clamp_cursor();

        self.set_status(if held {
            format!("{label} on hold")
        } else {
            format!("{label} released")
        });
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::app::test_support::{app_with, session, status};
    use crate::app::{Command, SessionsCommand};
    use crate::models::SessionState;

    fn ids(app: &crate::app::App) -> Vec<(bool, Vec<&str>)> {
        app.sessions
            .groups
            .iter()
            .map(|g| {
                let ids = g.sessions.iter().map(|s| s.session_id.as_str()).collect();
                (g.held, ids)
            })
            .collect()
    }

    #[test]
    fn held_session_trails_every_group_and_the_cursor_keeps_its_slot() {
        crate::test_util::with_temp_home(|| {
            let mut other = session("b-1", SessionState::Idle, None);
            other.cwd = "/tmp/zeta".into();
            other.project_name = "zeta".into();
            let (mut app, _runtime) = app_with(vec![
                session("a-1", SessionState::WaitingForInput, None),
                session("a-2", SessionState::WaitingForInput, None),
                other,
            ]);
            assert_eq!(app.selected_session_id().as_deref(), Some("a-1"));

            app.execute(Command::Sessions(SessionsCommand::ToggleHold));
            assert_eq!(
                ids(&app),
                vec![
                    (false, vec!["a-2"]),
                    (false, vec!["b-1"]),
                    (true, vec!["a-1"]),
                ],
                "a-1 leaves proj for the On hold section, below zeta"
            );
            assert_eq!(app.selected_session_id().as_deref(), Some("a-2"));
            assert_eq!(status(&app), "a-1 on hold");
            assert_eq!(
                app.sessions.attention_count(),
                1,
                "a held session stops asking for attention"
            );
        });
    }

    #[test]
    fn space_again_releases_the_session() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _runtime) = app_with(vec![session("a-1", SessionState::Idle, None)]);

            app.execute(Command::Sessions(SessionsCommand::ToggleHold));
            assert_eq!(ids(&app), vec![(true, vec!["a-1"])]);
            app.execute(Command::Sessions(SessionsCommand::ToggleHold));
            assert_eq!(ids(&app), vec![(false, vec!["a-1"])]);
            assert_eq!(status(&app), "a-1 released");
        });
    }

    #[test]
    fn activity_does_not_release_a_hold() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _runtime) = app_with(vec![session("a-1", SessionState::Idle, None)]);
            app.execute(Command::Sessions(SessionsCommand::ToggleHold));

            let mut busy = session("a-1", SessionState::Processing, None);
            busy.last_activity = Some(u64::MAX);
            app.update_sessions(vec![busy]);
            assert_eq!(ids(&app), vec![(true, vec!["a-1"])]);
        });
    }
}
