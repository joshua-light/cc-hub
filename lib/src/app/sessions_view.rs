use crate::acks::Acks;
use crate::models::{ProjectGroup, SessionInfo};
use std::collections::HashSet;

/// Sessions-tab state: the grouped-session grid plus its cursor and
/// view-filter toggles. Pulled out of [`App`] so the grid cursor can't be
/// moved out of range without going through the clamping methods here.
pub struct SessionsView {
    pub groups: Vec<ProjectGroup>,
    pub sel_group: usize,
    pub sel_in_group: usize,
    pub grid_scroll: u16,
    pub grid_cols: u16,
    pub show_inactive: bool,
    /// When false, the Sessions view hides any session whose tmux name is
    /// claimed by an orchestrator or worker in the current projects
    /// snapshot. Toggled with `W` so the user can drop into the raw view
    /// when something looks off.
    pub show_orch_workers: bool,
    pub acks: Acks,
    /// Latest scan snapshot; drives [`App::rebuild_groups`].
    pub(crate) last_sessions: Vec<SessionInfo>,
    /// Session ids seen on the previous scan tick. `None` means the first
    /// scan hasn't happened yet — used to skip cursor-jump on initial load.
    pub(crate) known_session_ids: Option<HashSet<String>>,
}

impl SessionsView {
    pub(crate) fn new() -> Self {
        Self {
            groups: Vec::new(),
            sel_group: 0,
            sel_in_group: 0,
            grid_scroll: 0,
            grid_cols: 3,
            show_inactive: false,
            show_orch_workers: false,
            acks: Acks::new(),
            last_sessions: Vec::new(),
            known_session_ids: None,
        }
    }

    pub fn move_right(&mut self) {
        if let Some(group) = self.groups.get(self.sel_group) {
            if group.sessions.is_empty() {
                return;
            }
            self.sel_in_group = (self.sel_in_group + 1) % group.sessions.len();
        }
    }

    pub fn move_left(&mut self) {
        if let Some(group) = self.groups.get(self.sel_group) {
            if group.sessions.is_empty() {
                return;
            }
            if self.sel_in_group == 0 {
                self.sel_in_group = group.sessions.len() - 1;
            } else {
                self.sel_in_group -= 1;
            }
        }
    }

    pub fn move_down(&mut self) {
        if self.groups.is_empty() {
            return;
        }
        let cols = self.grid_cols as usize;
        let group = &self.groups[self.sel_group];
        let current_col = self.sel_in_group % cols;
        let next = self.sel_in_group + cols;
        if next < group.sessions.len() {
            self.sel_in_group = next;
        } else if self.sel_group + 1 < self.groups.len() {
            self.sel_group += 1;
            let new_group = &self.groups[self.sel_group];
            self.sel_in_group = current_col.min(new_group.sessions.len().saturating_sub(1));
        }
    }

    pub fn move_up(&mut self) {
        if self.groups.is_empty() {
            return;
        }
        let cols = self.grid_cols as usize;
        let current_col = self.sel_in_group % cols;
        if self.sel_in_group >= cols {
            self.sel_in_group -= cols;
        } else if self.sel_group > 0 {
            self.sel_group -= 1;
            let prev_group = &self.groups[self.sel_group];
            let last_row_start = prev_group.sessions.len().saturating_sub(1) / cols * cols;
            self.sel_in_group =
                (last_row_start + current_col).min(prev_group.sessions.len().saturating_sub(1));
        }
    }

    pub fn selected_session_info(&self) -> Option<&SessionInfo> {
        self.groups
            .get(self.sel_group)
            .and_then(|g| g.sessions.get(self.sel_in_group))
    }

    pub fn selected_session_id(&self) -> Option<String> {
        self.selected_session_info().map(|s| s.session_id.clone())
    }

    pub fn session_count(&self) -> usize {
        self.groups.iter().map(|g| g.sessions.len()).sum()
    }

    pub fn attention_count(&self) -> usize {
        self.groups
            .iter()
            .flat_map(|g| &g.sessions)
            .filter(|s| s.needs_attention())
            .count()
    }
}
