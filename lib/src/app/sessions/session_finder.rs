//! State behind [`crate::app::View::SessionFinder`]: the archive-wide fuzzy
//! finder `/` opens on the Sessions tab. Where the grid shows the recent
//! window, the finder searches every transcript the
//! [`crate::sessions::index`] archive knows — by saved title, session id,
//! project, or first message — and Enter reopens the pick (attaching when it
//! is still live, resuming when it is not).

use crate::agent::AgentKind;
use crate::app::picker_list::{rank_rows, step, PickerRow, Searchable};
use crate::app::{App, Effect, View};
use crate::models::{first_line_truncated, short_sid, SessionState};
use crate::sessions::index::IndexedSession;
use std::path::PathBuf;

/// One archived session as a finder row: the searchable `label`/`detail`
/// text plus everything Enter needs to reopen it without re-touching disk.
#[derive(Clone, Debug)]
pub struct SessionFinderChoice {
    pub agent_id: String,
    pub agent_kind: AgentKind,
    pub session_id: String,
    pub cwd: String,
    pub jsonl_path: PathBuf,
    pub mtime_ms: u64,
    /// Saved title when the session has one, else its first user message —
    /// the line the user most likely remembers the session by.
    pub label: String,
    pub detail: String,
}

pub type SessionFinderRow = PickerRow;

#[derive(Clone, Debug)]
pub struct SessionFinderState {
    /// True until the background archive scan delivers its list — the
    /// renderer shows "indexing…" instead of an empty result.
    pub loading: bool,
    pub selected: usize,
    pub filter: String,
    pub rows: Vec<PickerRow>,
    pub choices: Vec<SessionFinderChoice>,
}

impl SessionFinderState {
    /// The finder opens before the archive scan finishes, so typing starts
    /// immediately; [`Self::set_index`] fills the list in when it lands.
    pub(crate) fn loading() -> Self {
        Self {
            loading: true,
            selected: 0,
            filter: String::new(),
            rows: Vec::new(),
            choices: Vec::new(),
        }
    }

    /// Adopt the archive scan's result (newest first), re-running whatever
    /// filter the user typed while it was loading.
    pub(crate) fn set_index(&mut self, index: Vec<IndexedSession>) {
        self.choices = index.into_iter().map(choice_of).collect();
        self.loading = false;
        self.refilter();
    }

    pub fn push_filter(&mut self, c: char) {
        self.filter.push(c);
        self.refilter();
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.refilter();
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.selected = step(self.selected, delta, self.rows.len());
    }

    pub fn selected_choice(&self) -> Option<&SessionFinderChoice> {
        self.rows
            .get(self.selected)
            .and_then(|row| self.choices.get(row.choice))
    }

    /// Ties keep archive order: newest first.
    fn refilter(&mut self) {
        self.rows = rank_rows(
            &self.filter,
            self.choices.iter().map(|session| Searchable {
                label: &session.label,
                detail: &session.detail,
                // The full id is searchable though only its short form
                // renders, so a pasted id finds its session.
                id: Some(&session.session_id),
            }),
        );
        self.selected = 0;
    }
}

fn choice_of(session: IndexedSession) -> SessionFinderChoice {
    let label = session
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            session
                .first_message
                .as_deref()
                .map(|m| first_line_truncated(m, 56))
        })
        .unwrap_or_else(|| "(no messages)".into());
    let detail = if session.agent_id == "claude" {
        format!(
            "{} · {}",
            session.project_name,
            short_sid(&session.session_id)
        )
    } else {
        format!(
            "{} · {} · {}",
            session.project_name,
            short_sid(&session.session_id),
            session.agent_id
        )
    };
    SessionFinderChoice {
        agent_id: session.agent_id,
        agent_kind: session.agent_kind,
        session_id: session.session_id,
        cwd: session.cwd,
        jsonl_path: session.jsonl_path,
        mtime_ms: session.mtime_ms,
        label,
        detail,
    }
}

impl App {
    /// `/` on the Sessions tab: open the archive-wide session finder. It
    /// opens empty and typing-ready; the archive itself arrives via
    /// [`Effect::BuildSessionIndex`] → [`Self::update_session_index`].
    pub fn enter_session_finder(&mut self) {
        self.session_finder = Some(SessionFinderState::loading());
        self.view = View::SessionFinder;
    }

    pub fn close_session_finder(&mut self) {
        self.session_finder = None;
        self.view = View::Grid;
    }

    /// Move the finder highlight by `delta` rows, clamped to the live
    /// filtered result list.
    pub fn session_finder_move(&mut self, delta: isize) {
        if let Some(finder) = self.session_finder.as_mut() {
            finder.move_selection(delta);
        }
    }

    /// Adopt a finished archive scan. Ignored when the finder was closed
    /// while the scan ran — the list is rebuilt fresh on every open.
    pub fn update_session_index(&mut self, index: Vec<crate::sessions::index::IndexedSession>) {
        if let Some(finder) = self.session_finder.as_mut() {
            finder.set_index(index);
        }
    }

    /// Enter on the session finder: reopen the highlighted session. A
    /// still-live session is attached (tmux pane) or window-focused — never
    /// resumed into a duplicate process; a dead one is resumed in its cwd
    /// and its fresh pane attached. An empty match list keeps the finder
    /// open, mirroring the model picker.
    pub fn confirm_session_finder(&mut self) -> Vec<Effect> {
        let Some(choice) = self
            .session_finder
            .as_ref()
            .and_then(SessionFinderState::selected_choice)
            .cloned()
        else {
            return Vec::new();
        };
        self.session_finder = None;
        self.view = View::Grid;
        // Live already? Match by session id or by transcript path — a Pi/
        // Codex resume runs on the same file, so a second spawn would put
        // two processes on one JSONL (see `focus_selected_session`).
        let live = self.sessions.last_sessions.iter().find(|s| {
            s.state != SessionState::Inactive
                && (s.session_id == choice.session_id
                    || s.jsonl_path.as_deref() == Some(choice.jsonl_path.as_path()))
        });
        if let Some(live) = live {
            let sid = crate::models::short_sid(&live.session_id).to_string();
            if let Some(tmux) = live.tmux_session.clone() {
                self.set_status(format!("attached live {} [{}]", sid, tmux));
                return vec![Effect::OpenTmuxPane { tmux, owned: false }];
            }
            let (pid, cwd) = (live.pid, live.cwd.clone());
            self.set_status(format!("focused live {}", sid));
            return vec![Effect::FocusWindow { pid, cwd }];
        }
        let resume = match choice.agent_kind {
            // Claude and Codex resume by session id; Pi by transcript path.
            crate::agent::AgentKind::Claude | crate::agent::AgentKind::Codex => {
                crate::spawn::SessionTarget::Resume(choice.session_id.clone())
            }
            crate::agent::AgentKind::Pi => {
                crate::spawn::SessionTarget::ResumeFile(choice.jsonl_path.clone())
            }
        };
        match self.runtime.spawn_session(
            &choice.agent_id,
            &choice.cwd,
            Some(resume),
            None,
            None,
            false,
        ) {
            Ok(tmux) => {
                self.set_status(format!(
                    "resumed {} [{}]",
                    crate::models::short_sid(&choice.session_id),
                    tmux
                ));
                vec![Effect::OpenTmuxPane { tmux, owned: false }]
            }
            Err(e) => {
                self.set_status(format!("resume failed: {}", e));
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed(sid: &str, title: Option<&str>, first: Option<&str>, mtime: u64) -> IndexedSession {
        IndexedSession {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            session_id: sid.into(),
            cwd: "/tmp/proj".into(),
            project_name: "proj".into(),
            jsonl_path: PathBuf::from(format!("/x/{sid}.jsonl")),
            mtime_ms: mtime,
            title: title.map(str::to_string),
            first_message: first.map(str::to_string),
        }
    }

    fn finder(index: Vec<IndexedSession>) -> SessionFinderState {
        let mut state = SessionFinderState::loading();
        state.set_index(index);
        state
    }

    #[test]
    fn empty_filter_keeps_archive_order() {
        let state = finder(vec![
            indexed("new", None, Some("latest work"), 2000),
            indexed("old", None, Some("ancient work"), 1000),
        ]);
        assert!(!state.loading);
        let ids: Vec<&str> = state
            .rows
            .iter()
            .map(|r| state.choices[r.choice].session_id.as_str())
            .collect();
        assert_eq!(ids, vec!["new", "old"]);
    }

    #[test]
    fn title_wins_the_label_and_the_filter_narrows_on_it() {
        let mut state = finder(vec![
            indexed("a", Some("Netcode rollback"), Some("something else"), 2),
            indexed("b", None, Some("kanban polish"), 1),
        ]);
        for c in "rollb".chars() {
            state.push_filter(c);
        }
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.selected_choice().unwrap().session_id, "a");
        assert!(!state.rows[0].label_indices.is_empty());
    }

    #[test]
    fn a_pasted_session_id_finds_its_session_without_highlight() {
        let mut state = finder(vec![
            indexed("f3a9c1d2-7b40-4e8e", Some("Old refactor"), None, 2),
            indexed("aaaa", Some("Noise"), None, 1),
        ]);
        for c in "f3a9c1d2".chars() {
            state.push_filter(c);
        }
        let hit = state.selected_choice().expect("id query matches");
        assert_eq!(hit.session_id, "f3a9c1d2-7b40-4e8e");
        assert!(state.rows[0].label_indices.is_empty());
        assert!(state.rows[0].detail_indices.is_empty());
    }

    #[test]
    fn typing_while_loading_applies_once_the_index_lands() {
        let mut state = SessionFinderState::loading();
        for c in "polish".chars() {
            state.push_filter(c);
        }
        assert!(state.rows.is_empty());
        state.set_index(vec![
            indexed("a", Some("kanban polish"), None, 2),
            indexed("b", Some("unrelated"), None, 1),
        ]);
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.selected_choice().unwrap().session_id, "a");
    }

    #[test]
    fn no_match_yields_no_choice() {
        let mut state = finder(vec![indexed("a", Some("Netcode"), None, 1)]);
        for c in "zzz".chars() {
            state.push_filter(c);
        }
        assert!(state.rows.is_empty());
        assert!(state.selected_choice().is_none());
    }
}

#[cfg(all(test, unix))]
mod app_tests {
    use crate::agent::AgentKind;
    use crate::app::test_support::{app_with, session, status};
    use crate::app::{Command, Effect, SessionsCommand};
    use crate::models::SessionState;

    fn indexed(sid: &str) -> crate::sessions::index::IndexedSession {
        crate::sessions::index::IndexedSession {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            session_id: sid.into(),
            cwd: "/tmp/proj".into(),
            project_name: "proj".into(),
            jsonl_path: std::path::PathBuf::from(format!("/x/{sid}.jsonl")),
            mtime_ms: 0,
            title: Some("Old refactor".into()),
            first_message: None,
        }
    }

    #[test]
    fn session_finder_opens_loading_and_requests_the_index() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![]);
            let effects = app.execute(Command::Sessions(SessionsCommand::OpenSessionFinder));
            assert_eq!(effects, vec![Effect::BuildSessionIndex]);
            assert_eq!(app.view, crate::app::View::SessionFinder);
            assert!(app.session_finder.as_ref().is_some_and(|f| f.loading));
        });
    }

    #[test]
    fn session_finder_confirm_resumes_a_dead_session_and_attaches() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![]);
            app.execute(Command::Sessions(SessionsCommand::OpenSessionFinder));
            app.update_session_index(vec![indexed("sid-old")]);

            let effects = app.execute(Command::Sessions(SessionsCommand::ConfirmSessionFinder));
            assert_eq!(
                effects,
                vec![Effect::OpenTmuxPane {
                    tmux: "mock-spawn".into(),
                    owned: false
                }]
            );
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.session_finder.is_none());
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].cwd, "/tmp/proj");
            assert_eq!(spawns[0].resume.as_deref(), Some("Resume(\"sid-old\")"));
            assert!(status(&app).starts_with("resumed"), "got: {}", status(&app));
        });
    }

    #[test]
    fn session_finder_confirm_attaches_a_live_session_instead_of_respawning() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![session(
                "sid-old",
                SessionState::Processing,
                Some("cc-agent-live"),
            )]);
            app.execute(Command::Sessions(SessionsCommand::OpenSessionFinder));
            app.update_session_index(vec![indexed("sid-old")]);

            let effects = app.execute(Command::Sessions(SessionsCommand::ConfirmSessionFinder));
            assert_eq!(
                effects,
                vec![Effect::OpenTmuxPane {
                    tmux: "cc-agent-live".into(),
                    owned: false
                }]
            );
            assert!(
                runtime.spawns.lock().unwrap().is_empty(),
                "a live session must be attached, never resumed into a duplicate"
            );
        });
    }

    #[test]
    fn session_finder_confirm_with_no_match_keeps_the_finder_open() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![]);
            app.execute(Command::Sessions(SessionsCommand::OpenSessionFinder));
            app.update_session_index(vec![indexed("sid-old")]);
            for c in "zzz".chars() {
                app.session_finder.as_mut().unwrap().push_filter(c);
            }

            let effects = app.execute(Command::Sessions(SessionsCommand::ConfirmSessionFinder));
            assert!(effects.is_empty());
            assert_eq!(app.view, crate::app::View::SessionFinder);
            assert!(app.session_finder.is_some());
            assert!(runtime.spawns.lock().unwrap().is_empty());
        });
    }
}
