use crate::metrics::{SessionSummary, ToolStats};
use crate::models;
use crate::models::short_sid;
use crate::ui::common::{fmt_cost, format_tokens, short_model};
use crate::ui::palette::FAINT_TEXT;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub(super) const TOOLS_DISPLAY_LIMIT: usize = 15;

pub(super) struct MetricsStyles {
    pub(super) dim: Style,
    pub(super) label: Style,
    pub(super) val: Style,
}

pub(super) fn render_bar_chart_section(
    lines: &mut Vec<Line<'static>>,
    header: &str,
    empty_noun: &str,
    overflow_noun: &str,
    stats: &std::collections::BTreeMap<String, ToolStats>,
    s: &MetricsStyles,
) {
    let (dim, label, val) = (s.dim, s.label, s.val);
    lines.push(section_header(header));
    let mut rows: Vec<(&String, &ToolStats)> = stats.iter().collect();
    rows.sort_by_key(|t| std::cmp::Reverse(t.1.count));
    let total_calls: u64 = rows.iter().map(|(_, s)| s.count).sum();
    let max_count = rows.first().map(|(_, s)| s.count).unwrap_or(0).max(1);
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  (no {} recorded)", empty_noun),
            dim,
        )));
    } else {
        for (name, s) in rows.iter().take(TOOLS_DISPLAY_LIMIT) {
            let bar_w = ((s.count as f64 / max_count as f64) * 24.0).round() as usize;
            let pct = if total_calls > 0 {
                s.count as f64 / total_calls as f64 * 100.0
            } else {
                0.0
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<22}", models::first_line_truncated(name, 22)),
                    label,
                ),
                Span::styled("━".repeat(bar_w), Style::default().fg(tool_color(name))),
                Span::raw(" "),
                Span::styled(format!("{:>6} calls", s.count), val),
                Span::styled(format!("  {:>4.1}%", pct), dim),
                Span::styled(format!("  {} sess", s.sessions), dim),
            ]));
        }
        if rows.len() > TOOLS_DISPLAY_LIMIT {
            lines.push(Line::from(Span::styled(
                format!(
                    "  … {} more {}",
                    rows.len() - TOOLS_DISPLAY_LIMIT,
                    overflow_noun
                ),
                dim,
            )));
        }
    }
    lines.push(Line::raw(""));
}

pub(super) fn tool_color(name: &str) -> Color {
    // Stable hash → palette so the same tool keeps the same color.
    let mut h: u32 = 0x811c_9dc5;
    for b in name.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    let palette: [Color; 8] = [
        Color::Rgb(120, 200, 240),
        Color::Rgb(240, 180, 120),
        Color::Rgb(160, 220, 160),
        Color::Rgb(220, 160, 200),
        Color::Rgb(200, 180, 240),
        Color::Rgb(240, 220, 140),
        Color::Rgb(140, 220, 220),
        Color::Rgb(220, 160, 140),
    ];
    palette[(h as usize) % palette.len()]
}

pub(super) fn selection_row_style(selected: bool) -> (&'static str, Style) {
    if selected {
        (
            "  ▸ ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("    ", Style::default().fg(Color::Cyan))
    }
}

pub(super) fn format_session_row(
    s: &SessionSummary,
    dim: Style,
    val: Style,
    selected: bool,
) -> Line<'static> {
    let sid = short_sid(&s.session_id).to_string();
    let subagent = if s.is_subagent { "⑂" } else { " " };
    let mark = if selected {
        format!("▸ {}", subagent)
    } else {
        format!("  {}", subagent)
    };
    let toks = format_tokens(s.tokens.total());
    let model = short_model(&s.model);
    let (_, sid_style) = selection_row_style(selected);
    Line::from(vec![
        Span::styled(format!("{}{:<8}", mark, sid), sid_style),
        Span::raw(" "),
        Span::styled(format!("{:>8}", fmt_cost(s.cost)), val.fg(Color::Green)),
        Span::raw(" "),
        Span::styled(format!("{:>10}", toks), dim),
        Span::raw(" "),
        Span::styled(
            format!("{:<22}", models::first_line_truncated(model, 22)),
            dim,
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<24}", models::first_line_truncated(&s.project, 24)),
            Style::default().fg(FAINT_TEXT),
        ),
    ])
}

pub(super) fn section_header(title: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("▎ ", Style::default().fg(Color::Rgb(120, 140, 180))),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

pub(super) fn model_color(model: &str) -> Color {
    let s = short_model(model);
    if s.contains("opus") {
        Color::Rgb(220, 150, 220)
    } else if s.contains("sonnet") {
        Color::Rgb(150, 200, 240)
    } else if s.contains("haiku") {
        Color::Rgb(160, 220, 180)
    } else {
        Color::Rgb(180, 180, 180)
    }
}
