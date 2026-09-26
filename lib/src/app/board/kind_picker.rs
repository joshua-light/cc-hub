//! The deliverable-kind picker (`T`): choose the kind the task router
//! places a card by.

use crate::app::{App, View};
use crate::config;

/// State behind [`View::TaskKindPicker`]: the card being classified and the
/// kinds on offer. `kinds` is `[tasks].kinds` verbatim — declaration order is
/// the user's order — preceded by the "router decides" row, so index 0 always
/// means [`None`].
#[derive(Clone, Debug)]
pub struct TaskKindPickerState {
    pub task: String,
    pub kinds: Vec<String>,
    pub selected: usize,
}

impl TaskKindPickerState {
    pub(crate) fn new(task: String, kinds: Vec<String>, current: Option<&str>) -> Self {
        let selected = current
            .and_then(|kind| kinds.iter().position(|k| k == kind))
            .map_or(0, |i| i + 1);
        Self {
            task,
            kinds,
            selected,
        }
    }

    /// One row per offer: the leading clear row plus every configured kind.
    pub fn rows(&self) -> usize {
        self.kinds.len() + 1
    }

    pub fn move_selection(&mut self, delta: isize) {
        let last = self.rows().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// The kind under the cursor; `None` on the clear row.
    pub fn selected_kind(&self) -> Option<&str> {
        self.kinds
            .get(self.selected.checked_sub(1)?)
            .map(String::as_str)
    }
}

impl App {
    /// `T` on a focused card: open the deliverable-kind picker. Returns false
    /// when no card is focused, or when `[tasks].kinds` declares nothing to
    /// pick — the caller says so rather than opening an empty list.
    pub fn enter_task_kind_picker(&mut self) -> bool {
        let kinds = config::get().tasks.kinds.clone();
        if kinds.is_empty() {
            return false;
        }
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        let (id, current) = (t.task_id.clone(), t.kind.clone());
        self.task_kind_picker = Some(TaskKindPickerState::new(id, kinds, current.as_deref()));
        self.view = View::TaskKindPicker;
        true
    }

    pub fn close_task_kind_picker(&mut self) {
        self.task_kind_picker = None;
        self.view = View::Grid;
    }

    pub fn task_kind_picker_move(&mut self, delta: isize) {
        if let Some(picker) = self.task_kind_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    /// Commit the pick: the highlighted kind lands on the card (the clear row
    /// gives it back to the router). Returns false when nothing was open.
    pub fn confirm_task_kind(&mut self) -> bool {
        let Some(picker) = self.task_kind_picker.take() else {
            self.view = View::Grid;
            return false;
        };
        let (id, kind) = (
            picker.task.clone(),
            picker.selected_kind().map(str::to_string),
        );
        self.view = View::Grid;
        if let Err(e) = self.tasks.board.set_kind(&id, kind.clone()) {
            self.tasks.record_persistence_error("kind update", e);
            return false;
        }
        self.focus_task(&id);
        self.set_status(match kind {
            Some(kind) => format!(
                "kind: {} — the router places this card without asking",
                kind
            ),
            None => "kind cleared — the router classifies this card".into(),
        });
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    fn picker(current: Option<&str>) -> TaskKindPickerState {
        let kinds = vec![
            "tps".to_string(),
            "ai-plugin".to_string(),
            "hub".to_string(),
        ];
        TaskKindPickerState::new("tk-1".into(), kinds, current)
    }

    #[test]
    fn opens_on_the_cards_own_kind_and_clamps_at_both_ends() {
        // Row 0 is the clear row, so a configured kind sits at index+1.
        assert_eq!(picker(None).selected, 0);
        assert_eq!(picker(Some("hub")).selected, 3);
        // A kind the config no longer declares reads as unclassified
        // rather than pointing at a neighbour.
        assert_eq!(picker(Some("retired")).selected, 0);

        let mut p = picker(Some("tps"));
        p.move_selection(-5);
        assert_eq!(p.selected_kind(), None, "top row clears the kind");
        p.move_selection(9);
        assert_eq!(p.selected_kind(), Some("hub"), "last row is the last kind");
    }

    #[test]
    fn confirming_writes_the_kind_and_the_clear_row_removes_it() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("ship the picker").unwrap().unwrap();
            app.task_kind_picker = Some(TaskKindPickerState::new(
                id.clone(),
                vec!["tps".into(), "hub".into()],
                None,
            ));
            app.task_kind_picker_move(2);
            assert!(app.confirm_task_kind());
            assert_eq!(
                app.tasks.board.get(&id).unwrap().kind.as_deref(),
                Some("hub")
            );
            assert_eq!(app.view, View::Grid, "the pick closes the picker");

            app.task_kind_picker = Some(TaskKindPickerState::new(
                id.clone(),
                vec!["tps".into(), "hub".into()],
                Some("hub"),
            ));
            app.task_kind_picker_move(-9);
            assert!(app.confirm_task_kind());
            assert!(app.tasks.board.get(&id).unwrap().kind.is_none());
        });
    }
}
