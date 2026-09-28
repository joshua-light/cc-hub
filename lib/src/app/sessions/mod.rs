//! The Sessions tab: the grouped session grid, its cursor, and every flow
//! that starts from a session card.
//!
//! - `scan`: applies scan snapshots; view toggles and acks (`z`).
//! - `groups`: turns a snapshot into ordered, task-clustered groups.
//! - `spawn`: spawn watchdogs, placeholder cards, boot-time naming.
//! - `hold`: put a session on hold, parking it in the On hold section (Space).
//! - `handoff`: mark a session whose last reply the next spawn opens with (`h`).
//! - `rename`: the rename modal (`r`).
//! - `model_picker`: model and agent choice for new sessions (`N`, `A`).
//! - `respawn_picker`: continue a session on another account (`R`).
//! - `task_link_picker`: link a session to a board task (`L`).
//! - `session_finder`: archive-wide session search (`/`).

use crate::acks::Acks;
use crate::handoff::Handoff;
use crate::holds::Holds;
use crate::models::{ProjectGroup, SessionInfo};
use std::collections::HashSet;

mod groups;
mod handoff;
mod hold;
mod model_picker;
mod rename;
mod respawn_picker;
mod scan;
mod session_finder;
mod spawn;
mod task_link_picker;

pub use model_picker::{AgentPickerState, ModelPickerChoice, ModelPickerState};
pub use rename::RenameSubmit;
pub use respawn_picker::{RespawnChoice, RespawnPickerState};
pub use session_finder::{SessionFinderChoice, SessionFinderState};
pub(super) use spawn::SpawnWatch;
pub use task_link_picker::{TaskLinkAction, TaskLinkChoice, TaskLinkPickerState};

/// How the Sessions tab lays out its sessions. `List` renders one compact
/// row per session, table-style, and is what the app opens on — it fits far
/// more sessions on screen than the cards do; `Grid` is the classic card
/// wall. Runtime toggle (`v`), in-memory only — resets to `List` on
/// relaunch, same lifetime as the `show_*` filters on [`SessionsView`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionsLayout {
    Grid,
    #[default]
    List,
}

impl SessionsLayout {
    pub fn label(self) -> &'static str {
        match self {
            SessionsLayout::Grid => "grid",
            SessionsLayout::List => "list",
        }
    }
}

/// Sessions-tab state: the grouped-session grid plus its cursor and
/// view-filter toggles. The cursor only moves through the clamping methods
/// here, so it can't leave the grid.
pub struct SessionsView {
    pub groups: Vec<ProjectGroup>,
    pub sel_group: usize,
    pub sel_in_group: usize,
    pub layout: SessionsLayout,
    pub show_inactive: bool,
    pub acks: Acks,
    pub holds: Holds,
    /// The session marked with `h`, whose last reply the next fresh session
    /// opens with. At most one; the spawn that takes it clears it.
    pub handoff: Option<Handoff>,
    /// Latest scan snapshot; drives [`crate::app::App::rebuild_groups`].
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
            layout: SessionsLayout::default(),
            show_inactive: false,
            acks: Acks::new(),
            holds: Holds::new(),
            handoff: None,
            last_sessions: Vec::new(),
            known_session_ids: None,
        }
    }

    /// Whether `session_id` is the session marked for a handoff.
    pub fn hands_off(&self, session_id: &str) -> bool {
        self.handoff
            .as_ref()
            .is_some_and(|h| h.session_id == session_id)
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

    /// `cols` is the current grid column count, owned by
    /// [`crate::app::RenderState::grid_cols`] and passed in by the caller so
    /// this cursor logic doesn't reach across into render state.
    pub fn move_down(&mut self, cols: u16) {
        if self.groups.is_empty() {
            return;
        }
        let cols = cols as usize;
        let group = &self.groups[self.sel_group];
        let len = group.sessions.len();
        let current_col = self.sel_in_group % cols;
        let next = self.sel_in_group + cols;
        if next < len {
            self.sel_in_group = next;
        } else if self.sel_in_group / cols < len.saturating_sub(1) / cols {
            // The cell straight below is off the end, but a partial last row
            // sits below the cursor's row — clamp onto its last card instead
            // of skipping the group (the mirror of move_up entering a group
            // from below). Only fall through to the next group when the cursor
            // is already on this group's last row.
            self.sel_in_group = len - 1;
        } else if self.sel_group + 1 < self.groups.len() {
            self.sel_group += 1;
            let new_group = &self.groups[self.sel_group];
            self.sel_in_group = current_col.min(new_group.sessions.len().saturating_sub(1));
        }
    }

    /// `cols` is the current grid column count; see [`Self::move_down`].
    pub fn move_up(&mut self, cols: u16) {
        if self.groups.is_empty() {
            return;
        }
        let cols = cols as usize;
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

    /// Pull the cursor back inside the grid after the groups changed under
    /// it: onto the last group, then onto that group's last card.
    pub(super) fn clamp_cursor(&mut self) {
        if self.groups.is_empty() {
            self.sel_group = 0;
            self.sel_in_group = 0;
            return;
        }
        self.sel_group = self.sel_group.min(self.groups.len() - 1);
        let max_in = self.groups[self.sel_group].sessions.len().saturating_sub(1);
        self.sel_in_group = self.sel_in_group.min(max_in);
    }

    pub fn selected_session_info(&self) -> Option<&SessionInfo> {
        self.groups
            .get(self.sel_group)
            .and_then(|g| g.sessions.get(self.sel_in_group))
    }

    pub fn selected_session_id(&self) -> Option<String> {
        self.selected_session_info().map(|s| s.session_id.clone())
    }

    /// `(group, index)` of the first visible session matching `pred`.
    pub(super) fn position_of(
        &self,
        pred: impl Fn(&SessionInfo) -> bool,
    ) -> Option<(usize, usize)> {
        self.groups
            .iter()
            .enumerate()
            .find_map(|(gi, group)| group.sessions.iter().position(&pred).map(|si| (gi, si)))
    }

    /// Ids of every session currently on the grid.
    pub(super) fn visible_ids(&self) -> HashSet<String> {
        self.groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|s| s.session_id.clone()))
            .collect()
    }

    pub fn session_count(&self) -> usize {
        self.groups.iter().map(|g| g.sessions.len()).sum()
    }

    /// Sessions waiting on the user, held ones aside: a hold is the user
    /// saying "not now".
    pub fn attention_count(&self) -> usize {
        self.groups
            .iter()
            .filter(|g| !g.held)
            .flat_map(|g| &g.sessions)
            .filter(|s| s.needs_attention())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::fake_session;
    use crate::models::SessionState;

    fn session(id: &str) -> SessionInfo {
        let mut s = fake_session(id, SessionState::Idle);
        s.tmux_session = None;
        s
    }

    fn group(name: &str, n: usize) -> ProjectGroup {
        ProjectGroup {
            name: name.into(),
            cwd: format!("/tmp/{name}"),
            sessions: (0..n).map(|i| session(&format!("{name}-{i}"))).collect(),
            held: false,
        }
    }

    /// Grid column count, passed to the nav helpers the way `RenderState`
    /// would.
    const COLS: u16 = 3;

    fn view(groups: Vec<ProjectGroup>) -> SessionsView {
        let mut v = SessionsView::new();
        v.groups = groups;
        v
    }

    #[test]
    fn move_down_clamps_into_partial_last_row() {
        // 3 cols, 5 cards: row0 = 0,1,2 ; row1 = 3,4 (partial). From the
        // rightmost card of row0 (idx 2) Down must clamp onto the last card of
        // the partial row (idx 4), not hop to the next group or dead-key.
        let mut v = view(vec![group("a", 5), group("b", 3)]);
        v.sel_in_group = 2;
        v.move_down(COLS);
        assert_eq!((v.sel_group, v.sel_in_group), (0, 4));
    }

    #[test]
    fn move_down_straight_below_when_cell_exists() {
        // 3 cols, 6 cards: the cell directly below idx 1 (idx 4) exists.
        let mut v = view(vec![group("a", 6)]);
        v.sel_in_group = 1;
        v.move_down(COLS);
        assert_eq!((v.sel_group, v.sel_in_group), (0, 4));
    }

    #[test]
    fn move_down_from_last_row_hops_to_next_group() {
        // On the group's last row, Down moves to the next group at the same
        // (clamped) column.
        let mut v = view(vec![group("a", 5), group("b", 3)]);
        v.sel_in_group = 4; // row1, group a's last row
        v.move_down(COLS);
        assert_eq!((v.sel_group, v.sel_in_group), (1, 1));
    }

    #[test]
    fn move_down_last_group_last_row_is_dead_key() {
        let mut v = view(vec![group("a", 5)]);
        v.sel_in_group = 4;
        v.move_down(COLS);
        assert_eq!((v.sel_group, v.sel_in_group), (0, 4));
    }

    #[test]
    fn move_up_clamps_into_partial_row_entering_group_from_below() {
        // Entering group a (partial last row) from col 2 of group b clamps to
        // a's last card — the symmetric partner of move_down's clamp.
        let mut v = view(vec![group("a", 5), group("b", 3)]);
        v.sel_group = 1;
        v.sel_in_group = 2;
        v.move_up(COLS);
        assert_eq!((v.sel_group, v.sel_in_group), (0, 4));
    }

    #[test]
    fn move_down_then_up_stays_in_column() {
        // Down into the partial row then Up returns to the starting column.
        let mut v = view(vec![group("a", 5)]);
        v.sel_in_group = 2; // row0 col2
        v.move_down(COLS); // clamps to idx 4 (row1 col1)
        assert_eq!(v.sel_in_group, 4);
        v.move_up(COLS); // row1 col1 -> row0 col1
        assert_eq!(v.sel_in_group, 1);
    }
}
