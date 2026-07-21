use std::collections::{HashMap, HashSet};

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
/// - `ui/metrics.rs` writes [`Self::metrics_view_height`],
///   [`Self::metrics_row_lines`], and [`Self::metrics_scroll`].
/// - `ui/projects/result_popup.rs` clamps [`Self::result_scroll`] and reads
///   [`Self::result_artifact_expanded`].
/// - `ui/popups.rs` clamps [`Self::state_debug_scroll`].
/// - `ui/projects/cards.rs` (`ensure_image_decoded`) populates
///   [`Self::artifact_images`] and [`Self::artifact_image_failed`].
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
    /// Scroll offset (unwrapped lines) of the Projects "Result" popup body.
    /// Clamped by the renderer to keep the selected card visible.
    pub result_scroll: u16,
    /// When true, the selected evidence card in the Result popup renders
    /// enlarged. Read by the renderer; toggled via
    /// [`App::toggle_result_artifact_expanded`].
    pub result_artifact_expanded: bool,
    /// Scroll offset of the state-debug popup; clamped to content by the
    /// renderer.
    pub state_debug_scroll: u16,
    /// Scroll offset (lines) of the Tasks-tab Task Info popup body. Clamped
    /// by the renderer (`ui/tasks.rs`) to keep the selected attachment
    /// visible.
    pub task_info_scroll: u16,
    /// Per-artifact decoded image cache, keyed by `Artifact::path`. Populated
    /// lazily on first popup render so non-image work doesn't pay decode cost;
    /// entries persist for the App lifetime since artifact paths are
    /// content-addressed and don't mutate.
    pub artifact_images: HashMap<String, ratatui_image::protocol::StatefulProtocol>,
    /// Paths whose decode failed once — never retry, since decoding the same
    /// bytes will keep failing and we'd burn CPU on every redraw.
    pub artifact_image_failed: HashSet<String>,
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
            result_scroll: 0,
            result_artifact_expanded: false,
            state_debug_scroll: 0,
            task_info_scroll: 0,
            artifact_images: HashMap::new(),
            artifact_image_failed: HashSet::new(),
        }
    }
}
