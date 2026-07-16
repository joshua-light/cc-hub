//! State behind [`crate::app::View::TaskLinkPicker`]: the fuzzy task
//! selector `L` opens on the Sessions tab. The App builds the candidate list
//! (personal board + orchestrated tasks, session-local ones first); this
//! module owns the live filter, rows, and selection — the same shape as
//! [`crate::app::ModelPickerState`].

use crate::fuzzy;
use crate::orchestrator::TaskStatus;

/// What picking a row does: drop the session's current link, or point it at
/// a task. `Link` carries everything the sidecar record needs so the confirm
/// path never has to re-resolve the task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskLinkAction {
    Unlink,
    Link {
        task_id: String,
        project_id: Option<String>,
        title: String,
    },
}

/// One candidate row: `label` is the task title (or prompt first line),
/// `detail` the status's board label (so typing "in progress" narrows to
/// that band). `status` drives the renderer's status coloring — `None` on
/// the unlink row.
#[derive(Clone, Debug)]
pub struct TaskLinkChoice {
    pub label: String,
    pub detail: String,
    pub status: Option<TaskStatus>,
    pub action: TaskLinkAction,
}

/// One visible row plus the character indices highlighted in the label or
/// detail. Only the better-scoring side is highlighted.
#[derive(Clone, Debug, Default)]
pub struct TaskLinkRow {
    pub choice: usize,
    pub label_indices: Vec<usize>,
    pub detail_indices: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct TaskLinkPickerState {
    /// Session being linked, captured at open time so a rescan can't move
    /// the target under the popup.
    pub session_id: String,
    /// Display name for the popup footer (session title or short id).
    pub session_label: String,
    pub selected: usize,
    pub filter: String,
    pub rows: Vec<TaskLinkRow>,
    pub choices: Vec<TaskLinkChoice>,
}

impl TaskLinkPickerState {
    /// `current_task_id` pre-selects the row of the task the session is
    /// already linked to, so `L` opens focused on the status quo.
    pub(crate) fn new(
        session_id: String,
        session_label: String,
        choices: Vec<TaskLinkChoice>,
        current_task_id: Option<&str>,
    ) -> Self {
        let mut picker = Self {
            session_id,
            session_label,
            selected: 0,
            filter: String::new(),
            rows: Vec::new(),
            choices,
        };
        picker.refilter();
        if let Some(current) = current_task_id {
            if let Some(idx) = picker.rows.iter().position(|row| {
                matches!(
                    &picker.choices[row.choice].action,
                    TaskLinkAction::Link { task_id, .. } if task_id == current
                )
            }) {
                picker.selected = idx;
            }
        }
        picker
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

    pub fn selected_action(&self) -> Option<&TaskLinkAction> {
        self.rows
            .get(self.selected)
            .and_then(|row| self.choices.get(row.choice))
            .map(|choice| &choice.action)
    }

    fn refilter(&mut self) {
        if self.filter.is_empty() {
            self.rows = (0..self.choices.len())
                .map(|choice| TaskLinkRow {
                    choice,
                    ..Default::default()
                })
                .collect();
        } else {
            let mut scored = Vec::new();
            for (choice, task) in self.choices.iter().enumerate() {
                let label_match = fuzzy::fuzzy_match(&self.filter, &task.label);
                let detail_match = fuzzy::fuzzy_match(&self.filter, &task.detail);
                let label_score = label_match.as_ref().map(|m| m.score * 2);
                let detail_score = detail_match.as_ref().map(|m| m.score);
                let Some(score) = label_score.max(detail_score) else {
                    continue;
                };
                let row = if label_score >= detail_score {
                    TaskLinkRow {
                        choice,
                        label_indices: label_match.map(|m| m.indices).unwrap_or_default(),
                        detail_indices: Vec::new(),
                    }
                } else {
                    TaskLinkRow {
                        choice,
                        label_indices: Vec::new(),
                        detail_indices: detail_match.map(|m| m.indices).unwrap_or_default(),
                    }
                };
                scored.push((score, row));
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.choice.cmp(&b.1.choice)));
            self.rows = scored.into_iter().map(|(_, row)| row).collect();
        }
        self.selected = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link_choice(label: &str, task_id: &str) -> TaskLinkChoice {
        TaskLinkChoice {
            label: label.into(),
            detail: "In Progress".into(),
            status: Some(TaskStatus::Running),
            action: TaskLinkAction::Link {
                task_id: task_id.into(),
                project_id: None,
                title: label.into(),
            },
        }
    }

    #[test]
    fn preselects_current_link_after_the_unlink_row() {
        let choices = vec![
            TaskLinkChoice {
                label: "✕ unlink".into(),
                detail: String::new(),
                status: None,
                action: TaskLinkAction::Unlink,
            },
            link_choice("Fix auth", "tk-1"),
            link_choice("Ship parser", "tk-2"),
        ];
        let picker =
            TaskLinkPickerState::new("sid".into(), "sid".into(), choices, Some("tk-2"));
        assert_eq!(picker.selected, 2);
        assert!(matches!(
            picker.selected_action(),
            Some(TaskLinkAction::Link { task_id, .. }) if task_id == "tk-2"
        ));
    }

    #[test]
    fn filter_narrows_and_highlights_labels() {
        let choices = vec![link_choice("Fix auth", "tk-1"), link_choice("Ship it", "tk-2")];
        let mut picker = TaskLinkPickerState::new("sid".into(), "sid".into(), choices, None);
        assert_eq!(picker.rows.len(), 2);
        for c in "fixa".chars() {
            picker.push_filter(c);
        }
        assert_eq!(picker.rows.len(), 1);
        assert!(!picker.rows[0].label_indices.is_empty());
        assert!(matches!(
            picker.selected_action(),
            Some(TaskLinkAction::Link { task_id, .. }) if task_id == "tk-1"
        ));
        picker.pop_filter();
        picker.pop_filter();
        picker.pop_filter();
        picker.pop_filter();
        assert_eq!(picker.rows.len(), 2);
    }

    #[test]
    fn no_match_yields_no_action() {
        let mut picker = TaskLinkPickerState::new(
            "sid".into(),
            "sid".into(),
            vec![link_choice("Fix auth", "tk-1")],
            None,
        );
        for c in "zzz".chars() {
            picker.push_filter(c);
        }
        assert!(picker.rows.is_empty());
        assert!(picker.selected_action().is_none());
    }
}
