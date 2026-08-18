//! State behind [`crate::app::View::SessionFinder`]: the archive-wide fuzzy
//! finder `/` opens on the Sessions tab. Where the grid shows the recent
//! window, the finder searches every transcript the
//! [`crate::session_index`] archive knows — by saved title, session id,
//! project, or first message — and Enter reopens the pick (attaching when it
//! is still live, resuming when it is not). Same live-filter shape as
//! [`crate::app::TaskLinkPickerState`].

use crate::agent::AgentKind;
use crate::fuzzy;
use crate::models::{first_line_truncated, short_sid};
use crate::session_index::IndexedSession;
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

/// One visible row plus the character indices highlighted in the label or
/// detail. Only the better-scoring side is highlighted; a match won on the
/// full session id highlights nothing (the id is searchable, not displayed).
#[derive(Clone, Debug, Default)]
pub struct SessionFinderRow {
    pub choice: usize,
    pub label_indices: Vec<usize>,
    pub detail_indices: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct SessionFinderState {
    /// True until the background archive scan delivers its list — the
    /// renderer shows "indexing…" instead of an empty result.
    pub loading: bool,
    pub selected: usize,
    pub filter: String,
    pub rows: Vec<SessionFinderRow>,
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
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn selected_choice(&self) -> Option<&SessionFinderChoice> {
        self.rows
            .get(self.selected)
            .and_then(|row| self.choices.get(row.choice))
    }

    fn refilter(&mut self) {
        if self.filter.is_empty() {
            self.rows = (0..self.choices.len())
                .map(|choice| SessionFinderRow {
                    choice,
                    ..Default::default()
                })
                .collect();
        } else {
            let mut scored = Vec::new();
            for (choice, session) in self.choices.iter().enumerate() {
                let label_match = fuzzy::fuzzy_match(&self.filter, &session.label);
                let detail_match = fuzzy::fuzzy_match(&self.filter, &session.detail);
                // The full id is searchable even though only its short form
                // renders — pasting an id from anywhere must find the session.
                let id_match = fuzzy::fuzzy_match(&self.filter, &session.session_id);
                let label_score = label_match.as_ref().map(|m| m.score * 2);
                let detail_score = detail_match.as_ref().map(|m| m.score);
                let id_score = id_match.as_ref().map(|m| m.score);
                let Some(score) = label_score.max(detail_score).max(id_score) else {
                    continue;
                };
                let row = if label_score == Some(score) {
                    SessionFinderRow {
                        choice,
                        label_indices: label_match.map(|m| m.indices).unwrap_or_default(),
                        detail_indices: Vec::new(),
                    }
                } else if detail_score == Some(score) {
                    SessionFinderRow {
                        choice,
                        label_indices: Vec::new(),
                        detail_indices: detail_match.map(|m| m.indices).unwrap_or_default(),
                    }
                } else {
                    SessionFinderRow {
                        choice,
                        ..Default::default()
                    }
                };
                scored.push((score, row));
            }
            // Equal scores keep archive order — newest first.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.choice.cmp(&b.1.choice)));
            self.rows = scored.into_iter().map(|(_, row)| row).collect();
        }
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
