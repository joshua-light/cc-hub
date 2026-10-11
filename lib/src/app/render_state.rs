use ratatui::layout::{Position, Rect};

/// Layout state computed (and clamp-written) by `ui/` during draw.
///
/// `ui/` is the only writer during render; `App` nav methods may read and
/// adjust scroll intents via their named methods; `bin/` never touches these
/// fields directly. `LiveView.scroll` is the one sanctioned exception (a
/// self-contained view object that clamps its own scroll in
/// [`crate::ui::popups`]).
///
/// All writer sites:
/// - `ui/mod.rs` → [`crate::app::App::update_grid_cols`] writes
///   [`Self::grid_cols`].
/// - `ui/sessions/grid.rs` and `ui/sessions/list.rs` write
///   [`Self::grid_scroll`] through `sessions::keep_in_view` (one writer per
///   layout); `ui/sessions/detail.rs` clamps [`Self::popup_scroll`].
/// - `ui/builds.rs` writes [`Self::builds_scroll`].
/// - `ui/sessions/split.rs` writes [`Self::dock_size`]; it,
///   `ui/agents/table.rs` and `ui/builds.rs` write [`Self::split`] for the
///   tab on screen.
/// - `ui/metrics/mod.rs` writes [`Self::metrics_view_height`],
///   [`Self::metrics_row_lines`], and [`Self::metrics_scroll`].
pub struct RenderState {
    /// Vertical scroll offset of the Sessions grid, in rows. The renderer
    /// keeps the selected card visible by writing this each frame.
    pub grid_scroll: u16,
    /// Session-grid column count, derived from the terminal width by
    /// [`crate::app::App::update_grid_cols`] each frame.
    pub grid_cols: u16,
    /// Scroll offset of the session-detail popup; clamped to content by the
    /// renderer.
    pub popup_scroll: u16,
    /// Metrics-tab scroll offset (logical lines). Written by the renderer
    /// after resolving selection-follow vs. free-scroll, read by the key
    /// handler on the next tick.
    pub metrics_scroll: u16,
    /// Metrics body height (rows), synced by the renderer so the key handler
    /// can tell whether a session row is on screen.
    pub metrics_view_height: u16,
    /// Logical-line offset of every selectable metrics session row, synced by
    /// the renderer for the same selection engagement decision.
    pub metrics_row_lines: Vec<usize>,
    /// Scroll offset (lines) of the Tasks-tab Task Info popup body. Clamped
    /// by the renderer (`ui/tasks/info.rs`) to keep the selected attachment
    /// visible.
    pub task_info_scroll: u16,
    /// Scroll offset (rows) of the Agents-tab table, written by the
    /// renderer to keep the selected row on screen.
    pub agents_scroll: u16,
    /// First visible row of the agent detail's open section, written by
    /// the renderer to keep the cursor on screen.
    pub agent_detail_scroll: usize,
    /// Scroll offset (rows) of the Builds-tab list, written by the renderer
    /// to keep the selected recipe on screen.
    pub builds_scroll: u16,
    /// `(rows, cols)` inside the split layout's dock, written by
    /// `ui/sessions/split.rs`: the size the next attach starts at.
    pub dock_size: (u16, u16),
    /// Where the tab on screen last drew its list and page, so a click can
    /// tell which one it landed on.
    pub split: SplitAreas,
}

/// A sidebar-and-page tab as last drawn: the sidebar, the page beside it
/// (either empty when the screen is too narrow for both), and the item each
/// visible sidebar row shows, top to bottom — `None` for headers and gaps.
/// Sessions are `(group, index)`; the flat Agents and Builds lists are
/// `(0, index)`.
#[derive(Default)]
pub struct SplitAreas {
    pub sidebar: Rect,
    pub page: Rect,
    pub rows: Vec<Option<(usize, usize)>>,
}

impl SplitAreas {
    /// The item drawn at screen cell `(x, y)`.
    pub fn item_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        if !self.sidebar.contains(Position::new(x, y)) {
            return None;
        }
        *self.rows.get(usize::from(y - self.sidebar.y))?
    }
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            grid_scroll: 0,
            grid_cols: 3,
            popup_scroll: 0,
            metrics_scroll: 0,
            metrics_view_height: 0,
            metrics_row_lines: Vec::new(),
            task_info_scroll: 0,
            agents_scroll: 0,
            agent_detail_scroll: 0,
            builds_scroll: 0,
            dock_size: (30, 100),
            split: SplitAreas::default(),
        }
    }
}
