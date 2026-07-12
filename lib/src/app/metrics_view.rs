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
