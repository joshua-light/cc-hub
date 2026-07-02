//! Metrics tab: the scrollable cost/usage report and its bar-chart helpers.

use crate::app::App;
use crate::metrics::{MetricsAnalysis, ModelStats, SessionSummary, ToolStats};
use crate::models;
use crate::models::short_sid;
use crate::ui::common::{fmt_cost, format_tokens, short_model};
use crate::ui::palette::{DOT_IDLE, FAINT_TEXT};
use chrono::Duration as ChronoDuration;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

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
            let current = app.metrics.scroll;
            if line < current {
                line
            } else if body_height > 0 && line >= current + body_height {
                line + 1 - body_height
            } else {
                current
            }
        }
        None => app.metrics.scroll,
    }
    .min(max_scroll);

    // Hand the row offsets and viewport height to the key handler so a downward
    // press can tell "engage the first on-screen session" from "scroll toward
    // the lists". Done after reading `row_lines` above (this moves it).
    app.metrics.view_height = body_height;
    app.metrics.row_lines = row_lines;
    app.metrics.scroll = scroll;

    let scroll_info = format!(
        " {}/{} ",
        (scroll as usize).min(total_lines.saturating_sub(1) as usize) + 1,
        total_lines
    );
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

/// Returns the rendered line buffer plus the logical-line index of every
/// selectable session row, in the same canonical order as
/// [`MetricsAnalysis::selectable_sessions`]. `selected` (an index into that
/// flat list) controls which row, if any, gets highlighted.
pub(crate) fn build_metrics_content(
    m: &MetricsAnalysis,
    selected: Option<usize>,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row_lines: Vec<usize> = Vec::new();
    let mut global_row: usize = 0;
    let dim = Style::default().fg(Color::DarkGray);
    let label = Style::default().fg(DOT_IDLE);
    let val = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    lines.push(section_header("Overview"));
    lines.push(Line::from(vec![
        Span::styled("  Total cost   ", label),
        Span::styled(fmt_cost(m.total_cost), val.fg(Color::Green)),
        Span::styled("    Sessions ", label),
        Span::styled(format!("{}", m.total_sessions), val),
        Span::styled("    Messages ", label),
        Span::styled(format!("{}", m.total_messages), val),
        Span::styled("    Cache hit ", label),
        Span::styled(format!("{:.0}%", m.cache_hit_rate * 100.0), val),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Tokens      ", label),
        Span::styled(
            format!(
                "{} in / {} out / {} cache_r / {} cache_w",
                format_tokens(m.total_tokens.input),
                format_tokens(m.total_tokens.output),
                format_tokens(m.total_tokens.cache_read),
                format_tokens(m.total_tokens.cache_creation),
            ),
            val,
        ),
    ]));
    lines.push(Line::raw(""));

    lines.push(section_header("Cost breakdown"));
    let breakdown = [
        (
            "input        ",
            m.total_tokens.input,
            Color::Rgb(120, 200, 240),
        ),
        (
            "output       ",
            m.total_tokens.output,
            Color::Rgb(240, 180, 120),
        ),
        (
            "cache read   ",
            m.total_tokens.cache_read,
            Color::Rgb(160, 220, 160),
        ),
        (
            "cache create ",
            m.total_tokens.cache_creation,
            Color::Rgb(220, 160, 200),
        ),
    ];
    let max_tokens = breakdown
        .iter()
        .map(|(_, t, _)| *t)
        .max()
        .unwrap_or(0)
        .max(1);
    for (name, toks, col) in breakdown {
        let bar_w = ((toks as f64 / max_tokens as f64) * 30.0).round() as usize;
        let bar: String = "━".repeat(bar_w);
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", name), label),
            Span::styled(bar, Style::default().fg(col)),
            Span::raw(" "),
            Span::styled(format_tokens(toks), dim),
        ]));
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Cost by model"));
    let mut models: Vec<(&String, &ModelStats)> = m.by_model.iter().collect();
    models.sort_by(|a, b| {
        b.1.cost
            .partial_cmp(&a.1.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let max_model_cost = models.first().map(|(_, s)| s.cost).unwrap_or(0.0).max(0.01);
    for (name, s) in models.iter().take(8) {
        let pct = if m.total_cost > 0.0 {
            s.cost / m.total_cost * 100.0
        } else {
            0.0
        };
        let bar_w = ((s.cost / max_model_cost) * 26.0).round() as usize;
        let short = short_model(name);
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<22}", models::first_line_truncated(short, 22)),
                label,
            ),
            Span::styled("━".repeat(bar_w), Style::default().fg(model_color(name))),
            Span::raw(" "),
            Span::styled(fmt_cost(s.cost), val),
            Span::styled(format!(" {:>4.1}%", pct), dim),
            Span::styled(format!("  {} msgs", s.messages), dim),
        ]));
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Daily spending (last 30 days)"));
    let today = chrono::Local::now().date_naive();
    let mut days: Vec<f64> = (0..30)
        .rev()
        .map(|n| {
            let day = today - ChronoDuration::days(n as i64);
            m.by_day.get(&day).map(|d| d.cost).unwrap_or(0.0)
        })
        .collect();
    let day_max = days.iter().cloned().fold(0f64, f64::max).max(0.01);
    let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let spark: String = days
        .iter_mut()
        .map(|c| {
            if *c <= 0.0 {
                ' '
            } else {
                let idx = ((*c / day_max) * 7.0).round().clamp(0.0, 7.0) as usize;
                blocks[idx]
            }
        })
        .collect();
    let last_7_total: f64 = days.iter().rev().take(7).sum();
    let last_30_total: f64 = days.iter().sum();
    lines.push(Line::from(vec![
        Span::styled("  ", dim),
        Span::styled(spark, Style::default().fg(Color::Rgb(150, 200, 240))),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  last 7d ", label),
        Span::styled(fmt_cost(last_7_total), val),
        Span::styled("    last 30d ", label),
        Span::styled(fmt_cost(last_30_total), val),
        Span::styled("    peak day ", label),
        Span::styled(fmt_cost(day_max), val),
    ]));
    lines.push(Line::raw(""));

    lines.push(section_header("Top projects"));
    let max_proj = m
        .top_projects
        .first()
        .map(|(_, s)| s.cost)
        .unwrap_or(0.0)
        .max(0.01);
    for (name, s) in &m.top_projects {
        let bar_w = ((s.cost / max_proj) * 24.0).round() as usize;
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<26}", models::first_line_truncated(name, 26)),
                label,
            ),
            Span::styled(
                "━".repeat(bar_w),
                Style::default().fg(Color::Rgb(120, 180, 220)),
            ),
            Span::raw(" "),
            Span::styled(fmt_cost(s.cost), val),
            Span::styled(format!("  {} sess", s.sessions), dim),
            Span::styled(format!("  {} msgs", s.messages), dim),
        ]));
    }
    lines.push(Line::raw(""));

    let styles = MetricsStyles { dim, label, val };
    render_bar_chart_section(
        &mut lines,
        "Tool usage",
        "tool calls",
        "tools",
        &m.by_tool,
        &styles,
    );
    render_bar_chart_section(
        &mut lines,
        "Shell commands",
        "shell commands",
        "commands",
        &m.by_shell,
        &styles,
    );
    render_bar_chart_section(
        &mut lines,
        "MCP servers",
        "MCP calls",
        "servers",
        &m.by_mcp,
        &styles,
    );

    lines.push(section_header("Interruptions (Esc'd mid-tool-call)"));
    let i = &m.interruptions;
    if i.total_interrupted_turns == 0 {
        lines.push(Line::from(Span::styled("  (none detected)", dim)));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  Wasted ", label),
            Span::styled(
                fmt_cost(i.total_wasted_cost),
                val.fg(Color::Rgb(220, 140, 140)),
            ),
            Span::styled("    Turns ", label),
            Span::styled(format!("{}", i.total_interrupted_turns), val),
            Span::styled("    Sessions ", label),
            Span::styled(format!("{}", i.sessions_affected), val),
        ]));
        for entry in i.by_session.iter() {
            let sid = short_sid(&entry.session_id).to_string();
            let (marker, sid_style) = selection_row_style(selected == Some(global_row));
            row_lines.push(lines.len());
            lines.push(Line::from(vec![
                Span::styled(format!("{}{:<10}", marker, sid), sid_style),
                Span::styled(
                    format!("{:>8}", fmt_cost(entry.wasted_cost)),
                    val.fg(Color::Rgb(220, 140, 140)),
                ),
                Span::styled(format!("  {:>3} orphan", entry.orphan_count), dim),
                Span::raw("  "),
                Span::styled(
                    format!(
                        "{:<18}",
                        models::first_line_truncated(&entry.last_tool_name, 18)
                    ),
                    Style::default().fg(FAINT_TEXT),
                ),
                Span::styled(
                    format!("{:<24}", models::first_line_truncated(&entry.project, 24)),
                    Style::default().fg(FAINT_TEXT),
                ),
            ]));
            global_row += 1;
        }
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Peak context reached"));
    let pc = &m.peak_context;
    if pc.findings.is_empty() {
        lines.push(Line::from(Span::styled("  (no sessions)", dim)));
    } else {
        for f in pc.findings.iter() {
            let sid = short_sid(&f.session_id).to_string();
            let (marker, sid_style) = selection_row_style(selected == Some(global_row));
            row_lines.push(lines.len());
            lines.push(Line::from(vec![
                Span::styled(format!("{}{:<10}", marker, sid), sid_style),
                Span::styled(
                    format!("{:>8} ctx", format_tokens(f.peak_ctx_tokens)),
                    val.fg(Color::Rgb(220, 180, 130)),
                ),
                Span::styled(
                    format!("  {:>8}", fmt_cost(f.total_cost)),
                    val.fg(Color::Green),
                ),
                Span::styled(
                    format!("  @ turn {}/{}", f.peak_turn_index, f.assistant_turns),
                    dim,
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<24}", models::first_line_truncated(&f.project, 24)),
                    Style::default().fg(FAINT_TEXT),
                ),
            ]));
            global_row += 1;
        }
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Token spikes (outlier single-turn deltas)"));
    let g = &m.context_growth;
    lines.push(Line::from(vec![
        Span::styled("  Scored ", label),
        Span::styled(format!("{}", g.sessions_scored), val),
        Span::styled("    Spikes ", label),
        Span::styled(format!("{}", g.findings.len()), val),
        Span::styled("    Cost in flagged sessions ", label),
        Span::styled(
            fmt_cost(g.anomalous_cost),
            val.fg(Color::Rgb(220, 180, 130)),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "  score = peak turn delta / median turn delta — flags one-shot bursts, not total growth",
        dim,
    )));
    if g.findings.is_empty() {
        lines.push(Line::from(Span::styled("  (no spikes)", dim)));
    } else {
        for f in g.findings.iter() {
            let sid = short_sid(&f.session_id).to_string();
            let (marker, sid_style) = selection_row_style(selected == Some(global_row));
            row_lines.push(lines.len());
            lines.push(Line::from(vec![
                Span::styled(format!("{}{:<10}", marker, sid), sid_style),
                Span::styled(
                    format!("{:>5.1}x", f.score),
                    val.fg(Color::Rgb(220, 180, 130)),
                ),
                Span::styled(
                    format!("  {:>8}", fmt_cost(f.total_cost)),
                    val.fg(Color::Green),
                ),
                Span::styled(
                    format!(
                        "  +{:>8} @ turn {}/{}",
                        format_tokens(f.peak_delta_tokens),
                        f.peak_turn_index,
                        f.assistant_turns
                    ),
                    dim,
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<24}", models::first_line_truncated(&f.project, 24)),
                    Style::default().fg(FAINT_TEXT),
                ),
            ]));
            global_row += 1;
        }
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Top sessions"));
    lines.push(Line::from(Span::styled(
        format!(
            "  {:<10} {:>8} {:>10} {:<22} {:<24}",
            "session", "cost", "tokens", "model", "project"
        ),
        dim,
    )));
    for s in &m.top_sessions {
        let is_sel = selected == Some(global_row);
        row_lines.push(lines.len());
        lines.push(format_session_row(s, dim, val, is_sel));
        global_row += 1;
    }

    (lines, row_lines)
}

pub(crate) const TOOLS_DISPLAY_LIMIT: usize = 15;

pub(crate) struct MetricsStyles {
    dim: Style,
    label: Style,
    val: Style,
}

pub(crate) fn render_bar_chart_section(
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

pub(crate) fn tool_color(name: &str) -> Color {
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

pub(crate) fn selection_row_style(selected: bool) -> (&'static str, Style) {
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

pub(crate) fn format_session_row(
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

pub(crate) fn section_header(title: &str) -> Line<'static> {
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

pub(crate) fn model_color(model: &str) -> Color {
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
