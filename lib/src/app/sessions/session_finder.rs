//! State behind [`crate::app::View::SessionFinder`]: the archive-wide fuzzy
//! finder `/` opens on the Sessions tab. Where the grid shows the recent
//! window, the finder searches every transcript the
//! [`crate::sessions::index`] archive knows — by saved title, session id,
//! project, or first message — and Enter reopens the pick (attaching when it
//! is still live, resuming when it is not).
//!
//! Unfiltered, the archive reads as a timeline split into [`Period`]s; a
//! filter ranks the matches instead, and the periods step aside.

use crate::agent::AgentKind;
use crate::app::picker_list::{rank_rows, step, PickerRow, Searchable};
use crate::app::{App, Effect, View};
use crate::models::{first_line_truncated, SessionState, TaskBadge};
use crate::sessions::index::IndexedSession;
use chrono::{Datelike, NaiveDate};
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
    /// Whether `label` is a saved title rather than a first message.
    pub titled: bool,
    pub project: String,
    /// The task the session is linked to, if any.
    pub task: Option<TaskBadge>,
    /// `project`, then the task title — the filter's second field. The
    /// renderer splits its highlights back into the two columns at
    /// [`Self::task_offset`].
    pub detail: String,
}

impl SessionFinderChoice {
    pub(crate) fn of(session: IndexedSession, task: Option<TaskBadge>) -> Self {
        let title = session.title.clone().filter(|t| !t.is_empty());
        let titled = title.is_some();
        let label = title
            .or_else(|| {
                session
                    .first_message
                    .as_deref()
                    .map(|m| first_line_truncated(m, 96))
            })
            .unwrap_or_else(|| "(no messages)".into());
        let detail = match &task {
            Some(task) => format!(
                "{} {}",
                session.project_name,
                task.title.lines().next().unwrap_or_default()
            ),
            None => session.project_name.clone(),
        };
        Self {
            agent_id: session.agent_id,
            agent_kind: session.agent_kind,
            session_id: session.session_id,
            cwd: session.cwd,
            jsonl_path: session.jsonl_path,
            mtime_ms: session.mtime_ms,
            label,
            titled,
            project: session.project_name,
            task,
            detail,
        }
    }

    /// Char index in `detail` where the task title starts.
    pub fn task_offset(&self) -> usize {
        self.project.chars().count() + 1
    }
}

/// The stretch of the past a session was last touched in — the headings the
/// unfiltered finder groups its timeline under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Today,
    Yesterday,
    /// Two to six days ago.
    PastWeek,
    Month {
        year: i32,
        month: u32,
    },
}

impl Period {
    pub fn of(day: NaiveDate, today: NaiveDate) -> Self {
        match (today - day).num_days() {
            ..=0 => Period::Today,
            1 => Period::Yesterday,
            2..=6 => Period::PastWeek,
            _ => Period::Month {
                year: day.year(),
                month: day.month(),
            },
        }
    }

    /// The heading, with the year only when it isn't `today`'s.
    pub fn label(self, today: NaiveDate) -> String {
        match self {
            Period::Today => "Today".into(),
            Period::Yesterday => "Yesterday".into(),
            Period::PastWeek => "Past week".into(),
            Period::Month { year, month } => {
                let name = NaiveDate::from_ymd_opt(year, month, 1)
                    .map(|d| d.format("%B").to_string())
                    .unwrap_or_default();
                if year == today.year() {
                    name
                } else {
                    format!("{name} {year}")
                }
            }
        }
    }
}

/// One line of the finder's list: a period heading with its row count, or
/// the row at that index of [`SessionFinderState::rows`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinderLine {
    Period(Period, usize),
    Row(usize),
}

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
    /// immediately; [`Self::set_choices`] fills the list in when it lands.
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
    pub(crate) fn set_choices(&mut self, choices: Vec<SessionFinderChoice>) {
        self.choices = choices;
        self.loading = false;
        self.refilter();
    }

    /// The list as drawn: unfiltered, the rows under a heading per
    /// [`Period`], `day_of` placing each row's session on the calendar;
    /// filtered, the ranked rows alone — headings would only cut a ranking
    /// into pieces.
    pub fn lines(&self, today: NaiveDate, day_of: impl Fn(u64) -> NaiveDate) -> Vec<FinderLine> {
        if !self.filter.is_empty() {
            return (0..self.rows.len()).map(FinderLine::Row).collect();
        }
        let mut lines = Vec::with_capacity(self.rows.len() + 8);
        let mut heading = 0;
        for (i, row) in self.rows.iter().enumerate() {
            let period = Period::of(day_of(self.choices[row.choice].mtime_ms), today);
            match lines.get_mut(heading) {
                Some(FinderLine::Period(current, count)) if *current == period => *count += 1,
                _ => {
                    heading = lines.len();
                    lines.push(FinderLine::Period(period, 1));
                }
            }
            lines.push(FinderLine::Row(i));
        }
        lines
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
                // The id is searchable though it never renders, so a pasted
                // id finds its session.
                id: Some(&session.session_id),
            }),
        );
        self.selected = 0;
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
    ///
    /// Agents' tick transcripts stay out, as they do on the grid: headless
    /// runs nobody types into, and on a busy day the bulk of the archive.
    pub fn update_session_index(&mut self, index: Vec<IndexedSession>) {
        if self.session_finder.is_none() {
            return;
        }
        let agents = crate::harness::AgentSessions::of(&self.harness.agents);
        let choices = index
            .into_iter()
            .filter(|s| {
                agents
                    .owner(&s.session_id, std::path::Path::new(&s.cwd))
                    .is_none()
            })
            .map(|s| {
                let task = self.task_badge(&s.session_id);
                SessionFinderChoice::of(s, task)
            })
            .collect();
        if let Some(finder) = self.session_finder.as_mut() {
            finder.set_choices(choices);
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
                return vec![Effect::OpenTmuxPane { tmux }];
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
                vec![Effect::OpenTmuxPane { tmux }]
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

    fn choices(index: Vec<IndexedSession>) -> Vec<SessionFinderChoice> {
        index
            .into_iter()
            .map(|s| SessionFinderChoice::of(s, None))
            .collect()
    }

    fn finder(index: Vec<IndexedSession>) -> SessionFinderState {
        let mut state = SessionFinderState::loading();
        state.set_choices(choices(index));
        state
    }

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
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
        state.set_choices(choices(vec![
            indexed("a", Some("kanban polish"), None, 2),
            indexed("b", Some("unrelated"), None, 1),
        ]));
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.selected_choice().unwrap().session_id, "a");
    }

    // The task a session is linked to is searchable, and its highlight lands
    // past the project in the shared detail field.
    #[test]
    fn a_task_title_finds_its_sessions() {
        let task = TaskBadge {
            task_id: "tk-1".into(),
            title: "Parser rewrite".into(),
            priority: None,
            stale: false,
        };
        let mut state = SessionFinderState::loading();
        state.set_choices(vec![
            SessionFinderChoice::of(indexed("a", Some("Fix"), None, 2), Some(task)),
            SessionFinderChoice::of(indexed("b", Some("Push"), None, 1), None),
        ]);
        for c in "write".chars() {
            state.push_filter(c);
        }
        let hit = state.selected_choice().expect("task query matches");
        assert_eq!(hit.session_id, "a");
        assert!(state.rows[0]
            .detail_indices
            .iter()
            .all(|&i| i >= hit.task_offset()));
    }

    #[test]
    fn periods_name_the_recent_days_then_the_months() {
        let today = day(12);
        assert_eq!(Period::of(day(12), today), Period::Today);
        assert_eq!(Period::of(day(11), today), Period::Yesterday);
        assert_eq!(Period::of(day(6), today), Period::PastWeek);
        assert_eq!(Period::of(day(5), today).label(today), "October");
        let last_year = NaiveDate::from_ymd_opt(2025, 9, 30).unwrap();
        assert_eq!(Period::of(last_year, today).label(today), "September 2025");
    }

    // Unfiltered, each period opens with a heading counting its rows; a
    // filter drops the headings and leaves the ranking alone.
    #[test]
    fn the_unfiltered_list_reads_as_a_timeline() {
        let mut state = finder(vec![
            indexed("a", Some("Netcode"), None, 12),
            indexed("b", Some("Network"), None, 12),
            indexed("c", Some("Kanban"), None, 11),
            indexed("d", Some("Nested"), None, 1),
        ]);
        // The test's mtimes are October days.
        let day_of = |mtime: u64| day(mtime as u32);

        assert_eq!(
            state.lines(day(12), day_of),
            vec![
                FinderLine::Period(Period::Today, 2),
                FinderLine::Row(0),
                FinderLine::Row(1),
                FinderLine::Period(Period::Yesterday, 1),
                FinderLine::Row(2),
                FinderLine::Period(
                    Period::Month {
                        year: 2026,
                        month: 10
                    },
                    1
                ),
                FinderLine::Row(3),
            ]
        );

        state.push_filter('n');
        assert!(state
            .lines(day(12), day_of)
            .iter()
            .all(|line| matches!(line, FinderLine::Row(_))));
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
