use crate::app::App;
use crate::metrics::{MetricsAnalysis, SelectableSession};

/// Metrics-tab state: the analysis result, the selectable session rows, and
/// the selection cursor. The scroll offset and the renderer-synced viewport
/// geometry (`metrics_scroll`, `metrics_view_height`, `metrics_row_lines`) now
/// live in [`crate::app::RenderState`]; the nav methods that read them moved to
/// [`crate::app::App`] (`metrics_nav_down` / `metrics_nav_up`).
pub struct MetricsView {
    /// Latest completed analysis. `None` while a scan is in flight.
    pub analysis: Option<MetricsAnalysis>,
    pub rows: Vec<SelectableSession>,
    pub selected: Option<usize>,
    /// (scanned, total) for the in-flight metrics scan, shown while
    /// [`Self::analysis`] is `None`. Cleared once analysis completes.
    pub progress: Option<(usize, usize)>,
}

impl MetricsView {
    pub(crate) fn new() -> Self {
        Self {
            analysis: None,
            rows: Vec::new(),
            selected: None,
            progress: None,
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

    pub fn selected_session(&self) -> Option<&SelectableSession> {
        self.selected.and_then(|i| self.rows.get(i))
    }
}

impl App {
    pub fn update_metrics(&mut self, m: MetricsAnalysis) {
        self.metrics.update(m);
    }

    pub fn update_metrics_progress(&mut self, scanned: usize, total: usize) {
        self.metrics.update_progress(scanned, total);
    }

    pub fn selected_metrics_session(&self) -> Option<&SelectableSession> {
        self.metrics.selected_session()
    }

    fn metrics_scroll_down(&mut self) {
        self.render.metrics_scroll = self.render.metrics_scroll.saturating_add(3);
    }

    fn metrics_scroll_up(&mut self) {
        self.render.metrics_scroll = self.render.metrics_scroll.saturating_sub(3);
    }

    /// Down/`j` on the Metrics tab. With a row selected, advance the cursor
    /// (the renderer keeps it on screen). With nothing selected, engage the
    /// first session row already visible — so selection only kicks in once the
    /// lists scroll into view — otherwise keep free-scrolling toward them.
    /// Reads the viewport geometry the renderer synced into [`crate::app::RenderState`].
    pub fn metrics_nav_down(&mut self) {
        match self.metrics.selected {
            Some(i) if !self.metrics.rows.is_empty() => {
                self.metrics.selected = Some((i + 1).min(self.metrics.rows.len() - 1));
            }
            _ => match self.first_visible_metrics_row() {
                Some(idx) => self.metrics.selected = Some(idx),
                None => self.metrics_scroll_down(),
            },
        }
    }

    /// Up/`k` on the Metrics tab. Walk the cursor back up; pressing up past the
    /// first session row releases the selection so free-scrolling (and reaching
    /// the Overview at the very top) resumes.
    pub fn metrics_nav_up(&mut self) {
        match self.metrics.selected {
            Some(0) => self.metrics.selected = None,
            Some(i) => self.metrics.selected = Some(i - 1),
            None => self.metrics_scroll_up(),
        }
    }

    /// Index (into `MetricsView::rows`) of the first selectable session row
    /// currently inside the viewport, using the offsets/height the renderer
    /// last synced into [`crate::app::RenderState`]. `None` when no session row is on
    /// screen.
    fn first_visible_metrics_row(&self) -> Option<usize> {
        let h = self.render.metrics_view_height;
        if h == 0 {
            return None;
        }
        let top = self.render.metrics_scroll;
        self.render
            .metrics_row_lines
            .iter()
            .position(|&l| (l as u16) >= top && (l as u16) < top.saturating_add(h))
    }
}
