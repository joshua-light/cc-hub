use crate::todo::TodoList;

/// Sessions-tab scratch to-do side panel ([`View::TodoPanel`]). The cursor
/// here can only be moved through the clamping methods so it never points
/// past the end of the list after a remove or external reload.
pub struct TodoPanelState {
    /// Persistent scratch to-do list shown by the Sessions-tab side panel
    /// (toggled with `t`). Reloaded from disk each time the panel opens (so
    /// external edits show up); mutations persist immediately.
    pub list: TodoList,
    /// Cursor into [`Self::list`] while the panel is open. Clamped to the
    /// list length whenever the panel is entered or an item is removed.
    pub selected: usize,
    /// True while the panel is in add-task input mode: char keys append to
    /// [`Self::input`] instead of acting as navigation commands.
    pub adding: bool,
    /// In-progress text for the task being added. Committed on Enter,
    /// discarded on Esc.
    pub input: String,
}

impl TodoPanelState {
    pub(crate) fn new() -> Self {
        Self {
            list: TodoList::default(),
            selected: 0,
            adding: false,
            input: String::new(),
        }
    }

    /// Reload the list from disk (picking up external edits) and clamp the
    /// cursor in case the list shrank. Starts in navigation mode.
    pub fn reload(&mut self) {
        self.list = TodoList::load();
        self.adding = false;
        self.input.clear();
        self.clamp_selection();
    }

    /// Discard any in-progress add and leave add mode.
    pub fn reset_add(&mut self) {
        self.adding = false;
        self.input.clear();
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        let last = self.list.len().saturating_sub(1);
        self.selected = (self.selected + 1).min(last);
    }

    /// Enter add-task input mode with an empty buffer.
    pub fn begin_add(&mut self) {
        self.adding = true;
        self.input.clear();
    }

    /// Leave add-task input mode without committing.
    pub fn cancel_add(&mut self) {
        self.adding = false;
        self.input.clear();
    }

    /// Commit the in-progress task. Empty input just exits add mode. On
    /// success the cursor moves to the freshly-added item.
    pub fn commit_add(&mut self) {
        if let Some(idx) = self.list.add(&self.input) {
            self.selected = idx;
        }
        self.adding = false;
        self.input.clear();
    }

    /// Flip done/undone on the selected item.
    pub fn toggle_selected(&mut self) {
        if self.selected < self.list.len() {
            self.list.toggle(self.selected);
        }
    }

    /// Delete the selected item, keeping the cursor in range.
    pub fn delete_selected(&mut self) {
        if self.selected < self.list.len() {
            self.list.remove(self.selected);
            self.clamp_selection();
        }
    }

    /// Remove every done item, keeping the cursor in range. Returns how many
    /// were removed so the caller can phrase its status line.
    pub fn clear_completed(&mut self) -> usize {
        let removed = self.list.clear_completed();
        if removed > 0 {
            self.clamp_selection();
        }
        removed
    }

    fn clamp_selection(&mut self) {
        let last = self.list.len().saturating_sub(1);
        if self.selected > last {
            self.selected = last;
        }
    }
}
