//! The add, rename and tag popups, and bracketed-paste routing into them.

use super::TaskField;
use crate::app::{App, View};

impl App {
    /// Open the add-task popup on the Tasks tab.
    pub fn enter_task_input(&mut self) {
        self.tasks.input.clear();
        self.tasks.context.clear();
        self.tasks.field = TaskField::Text;
        self.tasks.renaming = None;
        self.view = View::TaskInput;
    }

    /// `r` on a focused task: reopen the task popup prefilled with the
    /// current text; Enter commits it in place (id, status and any agent
    /// binding survive). Returns false when no task is focused.
    pub fn enter_task_rename(&mut self) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        let (id, text) = (t.task_id.clone(), t.prompt.clone());
        self.tasks.renaming = Some(id);
        self.tasks.input = text;
        // Rename edits the one line it opened on; existing context lives in
        // the card's notes, added and removed from the Task Info popup.
        self.tasks.context.clear();
        self.tasks.field = TaskField::Text;
        self.view = View::TaskInput;
        true
    }

    pub fn close_task_input(&mut self) {
        self.tasks.input.clear();
        self.tasks.context.clear();
        self.tasks.field = TaskField::Text;
        self.tasks.renaming = None;
        self.view = View::Grid;
    }

    /// Tab in the add popup: move between the task line and the context box.
    /// A no-op while renaming, which has no context field.
    pub fn toggle_task_input_field(&mut self) {
        if self.tasks.renaming.is_some() {
            return;
        }
        self.tasks.field = match self.tasks.field {
            TaskField::Text => TaskField::Context,
            TaskField::Context => TaskField::Text,
        };
    }

    /// The add popup's buffer for the focused field, for the key handler to
    /// push into and pop from.
    pub fn task_input_buffer(&mut self) -> &mut String {
        match self.tasks.field {
            TaskField::Text => &mut self.tasks.input,
            TaskField::Context => &mut self.tasks.context,
        }
    }

    /// Commit the task popup: append to To-Do and move the cursor to the
    /// new card, or — when renaming — replace the task's text in place.
    /// Adds run through the quick syntax ([`crate::tasks::parse_quick_add`]):
    /// `#tag` and `!1`–`!4` tokens set tags/priority without extra
    /// round-trips through `t` and `1`–`4`. An input that is *only* syntax
    /// leaves no text, so nothing is added. Returns false when nothing
    /// changed.
    pub fn submit_task_input(&mut self) -> bool {
        let text = std::mem::take(&mut self.tasks.input);
        let context = std::mem::take(&mut self.tasks.context);
        self.tasks.field = TaskField::Text;
        self.view = View::Grid;
        if let Some(id) = self.tasks.renaming.take() {
            return match self.tasks.board.rename(&id, &text) {
                Ok(changed) => changed,
                Err(e) => {
                    self.tasks.record_persistence_error("rename", e);
                    false
                }
            };
        }
        let quick = crate::tasks::parse_quick_add(&text);
        match self.tasks.board.add_configured(
            &quick.text,
            quick.tags,
            quick.priority.unwrap_or_default(),
        ) {
            Ok(Some(id)) => {
                self.attach_task_context(&id, &context);
                self.focus_task(&id);
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.tasks.record_persistence_error("add", e);
                false
            }
        }
    }

    /// Store the add popup's context box on the freshly created task as its
    /// first `note` attachment — the same shape `p` and the attach popup
    /// produce, so there is one kind of extra text on a card and the agent
    /// spawned by `s` reads it (see [`planning_prompt`](super::assign::planning_prompt)). Empty context attaches
    /// nothing.
    fn attach_task_context(&mut self, id: &str, context: &str) {
        if context.trim().is_empty() {
            return;
        }
        match crate::ops::task::task_artifact_add_text(id, context, "typed") {
            Ok(state) => self.tasks.board.adopt(state),
            Err(e) => self.tasks.record_persistence_error("context attach", e),
        }
    }

    /// `t` on a focused task: open the inline tag editor prefilled with the
    /// task's current tags (space-separated). Returns false when no task is
    /// focused.
    pub fn enter_task_tags(&mut self) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        let (id, prefill) = (t.task_id.clone(), t.tags.join(" "));
        self.tasks.tagging = Some(id);
        self.tasks.input = prefill;
        self.view = View::TaskTags;
        true
    }

    pub fn close_task_tags(&mut self) {
        self.tasks.input.clear();
        self.tasks.tagging = None;
        self.view = View::Grid;
    }

    /// Commit the tag editor: parse the buffer into the normalized tag set and
    /// replace the task's tags (an empty buffer clears them). The cursor
    /// follows the card by id — tags don't reorder columns, but this keeps the
    /// same focus contract as the other task mutations. Returns false when no
    /// task was being edited.
    pub fn submit_task_tags(&mut self) -> bool {
        let text = std::mem::take(&mut self.tasks.input);
        self.view = View::Grid;
        let Some(id) = self.tasks.tagging.take() else {
            return false;
        };
        if let Err(e) = self
            .tasks
            .board
            .set_tags(&id, crate::tasks::parse_tags(&text))
        {
            self.tasks.record_persistence_error("tag update", e);
            return false;
        }
        self.focus_task(&id);
        true
    }

    /// Route a bracketed-paste burst into whichever task input is active
    /// (add/rename, tags, attach — they share one buffer). Newlines fold into
    /// spaces, other control characters are dropped. Returns false when the
    /// active view has no such input, so the caller ignores the paste like
    /// before.
    ///
    /// The add popup is the exception: a paste that carries newlines is
    /// context, not a task line, so it lands verbatim in the context box and
    /// takes the cursor with it. A single-line paste goes to whichever field
    /// is focused.
    pub fn paste_into_input(&mut self, text: &str) -> bool {
        match self.view {
            View::TaskInput | View::TaskTags | View::TaskAttachInput => {}
            _ => return false,
        }
        let multiline = text.contains('\n') || text.contains('\r');
        if self.view == View::TaskInput && self.tasks.renaming.is_none() {
            if multiline {
                self.tasks.field = TaskField::Context;
            }
            let buf = self.task_input_buffer();
            let keep = |c: &char| !c.is_control() || (multiline && matches!(c, '\n' | '\t'));
            buf.extend(
                text.replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .chars()
                    .filter(keep),
            );
            return true;
        }
        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                self.tasks.input.push(' ');
            } else if !ch.is_control() {
                self.tasks.input.push(ch);
            }
        }
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::board::assign::planning_prompt;
    use crate::tasks::store::{TaskPriority, TaskStatus};
    use crate::test_util::with_temp_home;

    #[test]
    fn rename_prefills_commits_in_place_and_empty_keeps_text() {
        with_temp_home(|| {
            let mut app = App::new();
            // Nothing focused: refuse instead of opening an empty popup.
            assert!(!app.enter_task_rename());

            let id = app.tasks.board.add("old text").unwrap().unwrap();
            app.focus_task(&id);
            assert!(app.enter_task_rename());
            assert_eq!(app.view, View::TaskInput);
            assert_eq!(app.tasks.input, "old text");

            app.tasks.input = "new text".into();
            assert!(app.submit_task_input());
            assert_eq!(app.tasks.board.get(&id).unwrap().prompt, "new text");
            assert_eq!(app.view, View::Grid);
            assert!(app.tasks.renaming.is_none());

            // A whitespace-only commit must not wipe the task.
            assert!(app.enter_task_rename());
            app.tasks.input = "   ".into();
            assert!(!app.submit_task_input());
            assert_eq!(app.tasks.board.get(&id).unwrap().prompt, "new text");
        });
    }

    #[test]
    fn rename_does_not_touch_status_or_binding() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("t").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp/proj", "claude", "mux-1")
                .unwrap();
            app.focus_task(&id);
            assert!(app.enter_task_rename());
            app.tasks.input = "sharper wording".into();
            assert!(app.submit_task_input());
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.prompt, "sharper wording");
            assert_eq!(t.status, TaskStatus::Planning);
            assert_eq!(t.tmux.as_deref(), Some("mux-1"));
        });
    }

    #[test]
    fn add_popup_parses_tags_and_priority_inline() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_task_input();
            app.tasks.input = "fix the parser #bug #api !1".into();
            assert!(app.submit_task_input());
            let t = app.selected_board_task().unwrap();
            assert_eq!(t.prompt, "fix the parser");
            assert_eq!(t.tags, vec!["bug", "api"]);
            assert_eq!(t.priority, TaskPriority::P1);
        });
    }

    #[test]
    fn syntax_only_input_adds_nothing_and_rename_is_verbatim() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_task_input();
            app.tasks.input = "#bug !1".into();
            assert!(!app.submit_task_input());
            assert!(app.tasks.board.tasks().is_empty());

            // Rename keeps the syntax as literal text — the sugar is
            // add-only, so hashes in existing task text survive edits.
            let id = app.tasks.board.add("plain").unwrap().unwrap();
            app.focus_task(&id);
            assert!(app.enter_task_rename());
            app.tasks.input = "now with #hash !1".into();
            assert!(app.submit_task_input());
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.prompt, "now with #hash !1");
            assert!(t.tags.is_empty());
        });
    }

    #[test]
    fn multiline_paste_lands_in_the_context_field() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_task_input();
            app.tasks.input = "fix the parser".into();
            assert!(app.paste_into_input("line one\r\nline two"));
            // The task line is untouched and the cursor moved with the
            // paste, so the next keystroke keeps editing the context.
            assert_eq!(app.tasks.input, "fix the parser");
            assert_eq!(app.tasks.context, "line one\nline two");
            assert_eq!(app.tasks.field, TaskField::Context);

            // A single-line paste follows the focus instead.
            assert!(app.paste_into_input(" tail"));
            assert_eq!(app.tasks.context, "line one\nline two tail");
            app.toggle_task_input_field();
            assert!(app.paste_into_input(" now"));
            assert_eq!(app.tasks.input, "fix the parser now");
        });
    }

    #[test]
    fn renaming_still_flattens_a_multiline_paste() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("plain").unwrap().unwrap();
            app.focus_task(&id);
            assert!(app.enter_task_rename());
            app.tasks.input.clear();
            assert!(app.paste_into_input("one\ntwo"));
            assert_eq!(app.tasks.input, "one two");
            assert!(app.tasks.context.is_empty());
        });
    }

    #[test]
    fn submitted_context_becomes_the_task_first_note() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_task_input();
            app.tasks.input = "fix the parser #bug".into();
            app.tasks.context = "repro:\n  cc-hub task create".into();
            assert!(app.submit_task_input());

            let t = app.selected_board_task().unwrap().clone();
            assert_eq!(t.prompt, "fix the parser");
            assert_eq!(t.artifacts.len(), 1);
            assert_eq!(t.artifacts[0].kind, "note");
            assert_eq!(t.artifacts[0].original, "typed");
            let stored = std::fs::read_to_string(&t.artifacts[0].path).unwrap();
            assert_eq!(stored, "repro:\n  cc-hub task create");

            // The note reaches the agent the card is later assigned to.
            let prompt = planning_prompt(&t);
            assert!(prompt.contains("cc-hub task create"), "prompt: {prompt}");

            // And the popup opens clean next time.
            app.enter_task_input();
            assert!(app.tasks.context.is_empty());
            assert_eq!(app.tasks.field, TaskField::Text);
        });
    }

    #[test]
    fn empty_context_attaches_nothing() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_task_input();
            app.tasks.input = "no extras".into();
            app.tasks.context = "   \n".into();
            assert!(app.submit_task_input());
            assert!(app.selected_board_task().unwrap().artifacts.is_empty());
        });
    }
}
