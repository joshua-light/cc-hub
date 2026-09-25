/// Layout state computed (and clamp-written) by `ui/` during draw.
///
/// `ui/` is the only writer during render; `App` nav methods may read and
/// adjust scroll intents via their named methods; `bin/` never touches these
/// fields directly. `LiveView.scroll` is the one sanctioned exception (a
/// self-contained view object that clamps its own scroll in
/// [`crate::ui::popups`]).
///
/// All writer sites:
/// - `ui/mod.rs` → [`App::update_grid_cols`] writes [`Self::grid_cols`].
/// - `ui/sessions.rs` and `ui/sessions_list.rs` write [`Self::grid_scroll`]
///   (keep-selection-visible clamp, one writer per layout);
///   `ui/sessions.rs` also clamps [`Self::popup_scroll`].
/// - `ui/builds.rs` writes [`Self::builds_cols`] and [`Self::builds_scroll`].
/// - `ui/metrics.rs` writes [`Self::metrics_view_height`],
///   [`Self::metrics_row_lines`], and [`Self::metrics_scroll`].
pub struct RenderState {
    /// Vertical scroll offset of the Sessions grid, in rows. The renderer
    /// keeps the selected card visible by writing this each frame.
    pub grid_scroll: u16,
    /// Session-grid column count, derived from the terminal width by
    /// [`App::update_grid_cols`] each frame.
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
    /// by the renderer (`ui/tasks.rs`) to keep the selected attachment
    /// visible.
    pub task_info_scroll: u16,
    /// Scroll offset (rows) of the Agents-tab table, written by the
    /// renderer to keep the selected row on screen.
    pub agents_scroll: u16,
    /// First visible row of the agent detail's open section, written by
    /// the renderer to keep the cursor on screen.
    pub agent_detail_scroll: usize,
    /// Builds-tab card columns and scroll (card rows), written by the
    /// renderer; the cursor moves a row by stepping `builds_cols` cards.
    pub builds_cols: u16,
    pub builds_scroll: u16,
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
            builds_cols: 1,
            builds_scroll: 0,
        }
    }
}
