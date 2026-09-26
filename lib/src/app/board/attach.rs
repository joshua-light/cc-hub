//! The Task Info popup (`v`) and card attachments: notes, files and URLs.

use crate::app::{App, View};

/// Short display name for an attachment in status messages: the URL itself
/// for URL artifacts, the caption for notes (their `original` names an
/// origin like "typed", not a path), the *original* file's basename
/// otherwise (the stored copy's name carries a timestamp prefix nobody
/// typed).
fn attachment_label(a: &crate::tasks::store::Artifact) -> String {
    if a.kind == "url" {
        return a.path.clone();
    }
    if a.kind == "note" {
        return match &a.caption {
            Some(c) => format!("note — “{}”", c),
            None => "note".into(),
        };
    }
    std::path::Path::new(&a.original)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.original.clone())
}

impl App {
    /// `v` on a focused board card: open the Task Info popup (prompt +
    /// attachments). Returns false when no task is focused.
    pub fn enter_task_info(&mut self) -> bool {
        if self.selected_board_task().is_none() {
            return false;
        }
        self.tasks.info_sel = 0;
        self.render.task_info_scroll = 0;
        self.view = View::TaskInfo;
        true
    }

    pub fn close_task_info(&mut self) {
        self.tasks.info_sel = 0;
        self.render.task_info_scroll = 0;
        self.view = View::Grid;
    }

    /// The attachment under the Task Info popup cursor, if any. Used by the
    /// `c`/`o` keybinds to know what path to act on.
    pub fn selected_task_attachment(&self) -> Option<&crate::tasks::store::Artifact> {
        self.selected_board_task()?
            .artifacts
            .get(self.tasks.info_sel)
    }

    pub fn task_info_next(&mut self) {
        let n = self
            .selected_board_task()
            .map(|t| t.artifacts.len())
            .unwrap_or(0);
        if n == 0 {
            self.tasks.info_sel = 0;
            return;
        }
        self.tasks.info_sel = (self.tasks.info_sel + 1).min(n - 1);
    }

    pub fn task_info_prev(&mut self) {
        self.tasks.info_sel = self.tasks.info_sel.saturating_sub(1);
    }

    /// PgUp/PgDn handler for the Task Info popup. Negative steps scroll up;
    /// the renderer clamps the offset against content length.
    pub fn task_info_scroll_by(&mut self, delta: i32) {
        let cur = self.render.task_info_scroll as i32;
        self.render.task_info_scroll = (cur + delta).clamp(0, u16::MAX as i32) as u16;
    }

    /// `A` on a focused card (or `a` inside the Task Info popup): open the
    /// attach input, in note mode — Tab flips it to file-path/URL mode.
    /// Returns false when no task is focused.
    pub fn enter_task_attach(&mut self, from_info: bool) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        self.tasks.attaching = Some(t.task_id.clone());
        self.tasks.attach_from_info = from_info;
        self.tasks.attach_note = true;
        self.tasks.input.clear();
        self.view = View::TaskAttachInput;
        true
    }

    /// Tab inside the attach input: flip between note and file-path/URL mode.
    /// The typed buffer is kept — starting in the wrong mode costs nothing.
    pub fn toggle_task_attach_mode(&mut self) {
        self.tasks.attach_note = !self.tasks.attach_note;
    }

    pub fn close_task_attach(&mut self) {
        self.tasks.input.clear();
        self.tasks.attaching = None;
        self.view = if self.tasks.attach_from_info {
            View::TaskInfo
        } else {
            View::Grid
        };
        self.tasks.attach_from_info = false;
    }

    /// Commit the attach input: in note mode write the text as a `note`
    /// attachment, otherwise copy the file (or record the URL) into the
    /// task's own artifacts dir via the shared op — then adopt the persisted
    /// state into the board. Empty input cancels. Returns false when nothing
    /// was attached (a failure lands in the persistence-error slot).
    pub fn submit_task_attach(&mut self) -> bool {
        let text = std::mem::take(&mut self.tasks.input);
        let raw = text.trim().to_string();
        let from_info = self.tasks.attach_from_info;
        self.tasks.attach_from_info = false;
        self.view = if from_info {
            View::TaskInfo
        } else {
            View::Grid
        };
        let Some(id) = self.tasks.attaching.take() else {
            return false;
        };
        if raw.is_empty() {
            return false;
        }
        let result = if self.tasks.attach_note {
            crate::ops::task::task_artifact_add_text(&id, &raw, "typed")
        } else {
            crate::ops::task::task_artifact_add(&id, &raw, None, None, false)
        };
        match result {
            Ok(state) => {
                let label = state
                    .artifacts
                    .last()
                    .map(attachment_label)
                    .unwrap_or_default();
                let n = state.artifacts.len();
                self.tasks.board.adopt(state);
                if from_info {
                    // Land the popup cursor on what was just attached.
                    self.tasks.info_sel = n.saturating_sub(1);
                }
                self.set_status(format!("attached {}", label));
                true
            }
            Err(e) => {
                self.tasks.record_persistence_error("attach", e);
                false
            }
        }
    }

    /// `x` inside the Task Info popup: remove the selected attachment
    /// (record + stored copy — personal tasks only, the store op refuses
    /// anything else). Returns the status message, or `None` when no task is
    /// focused.
    pub fn remove_selected_attachment(&mut self) -> Option<String> {
        let t = self.selected_board_task()?;
        if t.artifacts.is_empty() {
            return Some("no attachments to remove".into());
        }
        let id = t.task_id.clone();
        let idx = self.tasks.info_sel.min(t.artifacts.len() - 1);
        match crate::ops::task::task_artifact_remove(&id, idx) {
            Ok((state, removed)) => {
                let remaining = state.artifacts.len();
                self.tasks.board.adopt(state);
                self.tasks.info_sel = self.tasks.info_sel.min(remaining.saturating_sub(1));
                Some(format!("removed {}", attachment_label(&removed)))
            }
            Err(e) => Some(format!("attachment remove failed: {}", e)),
        }
    }

    /// `p` on a focused card or inside the Task Info popup: attach `text`
    /// (read from the clipboard by the bin-side key arm, so tests can inject
    /// anything) as a `note` attachment — a fresh `.md` file in the task's
    /// artifacts dir. Returns the status message, or `None` when no task is
    /// focused.
    pub fn attach_text_to_selected(&mut self, text: &str) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.task_id.clone();
        if text.trim().is_empty() {
            return Some("clipboard empty — nothing attached".into());
        }
        match crate::ops::task::task_artifact_add_text(&id, text, "clipboard") {
            Ok(state) => {
                let caption = state
                    .artifacts
                    .last()
                    .and_then(|a| a.caption.clone())
                    .unwrap_or_default();
                let n = state.artifacts.len();
                self.tasks.board.adopt(state);
                if self.view == View::TaskInfo {
                    // Land the popup cursor on what was just pasted.
                    self.tasks.info_sel = n.saturating_sub(1);
                }
                Some(format!("attached note — “{}”", caption))
            }
            Err(e) => Some(format!("attach failed: {}", e)),
        }
    }
}
