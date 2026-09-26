//! Metrics tab: the scrollable cost/usage report.
//!
//! - `report`: the report's lines, section by section
//! - `rows`: line builders the sections share

use crate::app::App;
use crate::ui::common::scroll_position;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use report::build_metrics_content;

mod report;
mod rows;

pub(crate) fn render_metrics_body(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.height < 2 {
        return;
    }

    let m = match &app.metrics.analysis {
        Some(m) => m,
        None => {
            let text = match app.metrics.progress {
                Some((scanned, total)) if total > 0 => {
                    let pct = (scanned as f64 / total as f64 * 100.0).round() as u64;
                    format!(
                        " Scanning agent transcripts … {} / {} sessions ({}%)",
                        scanned, total, pct
                    )
                }
                _ => " Scanning agent transcripts …".to_string(),
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::DarkGray),
                ))),
                area,
            );
            return;
        }
    };

    let (lines, row_lines) = build_metrics_content(m, app.metrics.selected);
    let total_lines = lines.len() as u16;
    let body_height = area.height.saturating_sub(1);
    let max_scroll = total_lines.saturating_sub(body_height);

    // With a row selected, keep it inside the viewport; otherwise honour the
    // user's free scroll. Either way clamp to the content height and write the
    // result back, so selection and scroll stay in sync — releasing the
    // selection (up past the first row) then resumes scrolling from here.
    let scroll = match app.metrics.selected.and_then(|i| row_lines.get(i).copied()) {
        Some(line_idx) => {
            let line = line_idx as u16;
            let current = app.render.metrics_scroll;
            if line < current {
                line
            } else if body_height > 0 && line >= current + body_height {
                line + 1 - body_height
            } else {
                current
            }
        }
        None => app.render.metrics_scroll,
    }
    .min(max_scroll);

    // Hand the row offsets and viewport height to the key handler so a downward
    // press can tell "engage the first on-screen session" from "scroll toward
    // the lists". Done after reading `row_lines` above (this moves it).
    app.render.metrics_view_height = body_height;
    app.render.metrics_row_lines = row_lines;
    app.render.metrics_scroll = scroll;

    let scroll_info = scroll_position(scroll, total_lines);
    let indicator_area = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            scroll_info,
            Style::default().fg(Color::Rgb(80, 80, 95)),
        )))
        .alignment(Alignment::Right),
        indicator_area,
    );

    let body_area = Rect::new(area.x, area.y, area.width, body_height);
    // No wrap: the rows are tabular and `max_scroll`/`row_lines`/`view_height`
    // are all counted in logical lines. Wrapping made `.scroll` count wrapped
    // rows instead, so on a narrow terminal the clamp and the selection-follow
    // targeted the wrong rows. Clipping over-long rows keeps 1 line == 1 row.
    let content = Paragraph::new(lines).scroll((scroll, 0));
    frame.render_widget(content, body_area);
}
