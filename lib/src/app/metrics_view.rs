use crate::metrics::{MetricsAnalysis, SelectableSession};

/// Metrics-tab state: the analysis result, the selectable session rows, and
/// the scroll/selection cursors. The renderer-synced fields at the bottom are
/// written by the renderer each frame and read by the key handler, so they
/// live together to make that render→input coupling visible in one place.
pub struct MetricsView {
    /// Latest completed analysis. `None` while a scan is in flight.
    pub analysis: Option<MetricsAnalysis>,
    pub scroll: u16,
    pub rows: Vec<SelectableSession>,
    pub selected: Option<usize>,
    /// (scanned, total) for the in-flight metrics scan, shown while
    /// [`Self::analysis`] is `None`. Cleared once analysis completes.
    pub progress: Option<(usize, usize)>,
    // renderer-synced: the following are written by `ui::metrics` each frame
    // and read by the key handler on the next tick. They are NOT set by the
    // input path — `row_lines` is the logical-line offset of every selectable
    // session row and `view_height` the body height, both used to decide
    // whether a downward press should engage the selection cursor (a session
    // row is already on screen) or keep free-scrolling.
    pub row_lines: Vec<usize>,
    pub view_height: u16,
}

impl MetricsView {
    pub(crate) fn new() -> Self {
        Self {
            analysis: None,
            scroll: 0,
            rows: Vec::new(),
            selected: None,
            progress: None,
            row_lines: Vec::new(),
            view_height: 0,
        }
    }

    pub fn update(&mut self, m: MetricsAnalysis) {
        let prev_sid = self
            .selected
            .and_then(|i| self.rows.get(i))
            .map(|r| r.session_id.clone());
        self.rows = m.selectable_sessions();
        self.analysis = Some(m);
        self.progress = None;
        // Keep an existing selection pinned to its session across refreshes, but
        // never auto-select: the tab opens at the top (Overview) and stays
        // freely scrollable until the user navigates into the session lists.
        self.selected = prev_sid.and_then(|sid| self.rows.iter().position(|r| r.session_id == sid));
    }

    pub fn update_progress(&mut self, scanned: usize, total: usize) {
        if self.analysis.is_some() {
            return;
        }
        self.progress = Some((scanned, total));
    }

    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(3);
    }

    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(3);
    }

    /// Down/`j` on the Metrics tab. With a row selected, advance the cursor
    /// (the renderer keeps it on screen). With nothing selected, engage the
    /// first session row already visible — so selection only kicks in once the
    /// lists scroll into view — otherwise keep free-scrolling toward them.
    pub fn nav_down(&mut self) {
        match self.selected {
            Some(i) if !self.rows.is_empty() => {
                self.selected = Some((i + 1).min(self.rows.len() - 1));
            }
            _ => match self.first_visible_row() {
                Some(idx) => self.selected = Some(idx),
                None => self.scroll_down(),
            },
        }
    }

    /// Up/`k` on the Metrics tab. Walk the cursor back up; pressing up past the
    /// first session row releases the selection so free-scrolling (and reaching
    /// the Overview at the very top) resumes.
    pub fn nav_up(&mut self) {
        match self.selected {
            Some(0) => self.selected = None,
            Some(i) => self.selected = Some(i - 1),
            None => self.scroll_up(),
        }
    }

    /// Index (into [`Self::rows`]) of the first selectable session row
    /// currently inside the viewport, using the offsets/height the renderer
    /// last synced. `None` when no session row is on screen.
    fn first_visible_row(&self) -> Option<usize> {
        let h = self.view_height;
        if h == 0 {
            return None;
        }
        let top = self.scroll;
        self.row_lines
            .iter()
            .position(|&l| (l as u16) >= top && (l as u16) < top.saturating_add(h))
    }

    pub fn selected_session(&self) -> Option<&SelectableSession> {
        self.selected.and_then(|i| self.rows.get(i))
    }
}
