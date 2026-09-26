use super::rows::{
    format_session_row, model_color, render_bar_chart_section, section_header, selection_row_style,
    DIM, LABEL, VAL,
};
use crate::metrics::{MetricsAnalysis, ModelStats};
use crate::models::short_sid;
use crate::ui::common::{fmt_cost, format_tokens, pad, short_model};
use crate::ui::palette::FAINT_TEXT;
use chrono::Duration as ChronoDuration;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// The report under construction. Sections whose rows open a session push
/// them through [`Report::push_row`], so `row_lines[i]` is the line of the
/// `i`-th selectable row.
struct Report {
    lines: Vec<Line<'static>>,
    row_lines: Vec<usize>,
    selected: Option<usize>,
}

impl Report {
    /// Whether the next row given to [`Report::push_row`] is the selected one.
    fn next_row_selected(&self) -> bool {
        self.selected == Some(self.row_lines.len())
    }

    fn push_row(&mut self, line: Line<'static>) {
        self.row_lines.push(self.lines.len());
        self.lines.push(line);
    }
}

/// Returns the rendered line buffer plus the logical-line index of every
/// selectable session row, in the same canonical order as
/// [`MetricsAnalysis::selectable_sessions`]. `selected` (an index into that
/// flat list) controls which row, if any, gets highlighted.
pub(super) fn build_metrics_content(
    m: &MetricsAnalysis,
    selected: Option<usize>,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut r = Report {
        lines: Vec::new(),
        row_lines: Vec::new(),
        selected,
    };
    overview(&mut r.lines, m);
    cost_breakdown(&mut r.lines, m);
    cost_by_model(&mut r.lines, m);
    daily_spending(&mut r.lines, m);
    top_projects(&mut r.lines, m);
    render_bar_chart_section(
        &mut r.lines,
        "Tool usage",
        "tool calls",
        "tools",
        &m.by_tool,
    );
    render_bar_chart_section(
        &mut r.lines,
        "Shell commands",
        "shell commands",
        "commands",
        &m.by_shell,
    );
    render_bar_chart_section(
        &mut r.lines,
        "MCP servers",
        "MCP calls",
        "servers",
        &m.by_mcp,
    );
    interruptions(&mut r, m);
    peak_context(&mut r, m);
    token_spikes(&mut r, m);
    top_sessions(&mut r, m);
    (r.lines, r.row_lines)
}

fn overview(lines: &mut Vec<Line<'static>>, m: &MetricsAnalysis) {
    lines.push(section_header("Overview"));
    lines.push(Line::from(vec![
        Span::styled("  Total cost   ", LABEL),
        Span::styled(fmt_cost(m.total_cost), VAL.fg(Color::Green)),
        Span::styled("    Sessions ", LABEL),
        Span::styled(format!("{}", m.total_sessions), VAL),
        Span::styled("    Messages ", LABEL),
        Span::styled(format!("{}", m.total_messages), VAL),
        Span::styled("    Cache hit ", LABEL),
        Span::styled(format!("{:.0}%", m.cache_hit_rate * 100.0), VAL),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Tokens      ", LABEL),
        Span::styled(
            format!(
                "{} in / {} out / {} cache_r / {} cache_w",
                format_tokens(m.total_tokens.input),
                format_tokens(m.total_tokens.output),
                format_tokens(m.total_tokens.cache_read),
                format_tokens(m.total_tokens.cache_creation),
            ),
            VAL,
        ),
    ]));
    lines.push(Line::raw(""));
}

fn cost_breakdown(lines: &mut Vec<Line<'static>>, m: &MetricsAnalysis) {
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
            Span::styled(format!("  {}", name), LABEL),
            Span::styled(bar, Style::default().fg(col)),
            Span::raw(" "),
            Span::styled(format_tokens(toks), DIM),
        ]));
    }
    lines.push(Line::raw(""));
}

fn cost_by_model(lines: &mut Vec<Line<'static>>, m: &MetricsAnalysis) {
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
            Span::styled(format!("  {}", pad(short, 22)), LABEL),
            Span::styled("━".repeat(bar_w), Style::default().fg(model_color(name))),
            Span::raw(" "),
            Span::styled(fmt_cost(s.cost), VAL),
            Span::styled(format!(" {:>4.1}%", pct), DIM),
            Span::styled(format!("  {} msgs", s.messages), DIM),
        ]));
    }
    lines.push(Line::raw(""));
}

fn daily_spending(lines: &mut Vec<Line<'static>>, m: &MetricsAnalysis) {
    lines.push(section_header("Daily spending (last 30 days)"));
    let today = chrono::Local::now().date_naive();
    let days: Vec<f64> = (0..30)
        .rev()
        .map(|n| {
            let day = today - ChronoDuration::days(n as i64);
            m.by_day.get(&day).map(|d| d.cost).unwrap_or(0.0)
        })
        .collect();
    let day_max = days.iter().cloned().fold(0f64, f64::max).max(0.01);
    let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let spark: String = days
        .iter()
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
        Span::styled("  ", DIM),
        Span::styled(spark, Style::default().fg(Color::Rgb(150, 200, 240))),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  last 7d ", LABEL),
        Span::styled(fmt_cost(last_7_total), VAL),
        Span::styled("    last 30d ", LABEL),
        Span::styled(fmt_cost(last_30_total), VAL),
        Span::styled("    peak day ", LABEL),
        Span::styled(fmt_cost(day_max), VAL),
    ]));
    lines.push(Line::raw(""));
}

fn top_projects(lines: &mut Vec<Line<'static>>, m: &MetricsAnalysis) {
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
            Span::styled(format!("  {}", pad(name, 26)), LABEL),
            Span::styled(
                "━".repeat(bar_w),
                Style::default().fg(Color::Rgb(120, 180, 220)),
            ),
            Span::raw(" "),
            Span::styled(fmt_cost(s.cost), VAL),
            Span::styled(format!("  {} sess", s.sessions), DIM),
            Span::styled(format!("  {} msgs", s.messages), DIM),
        ]));
    }
    lines.push(Line::raw(""));
}

fn interruptions(r: &mut Report, m: &MetricsAnalysis) {
    r.lines
        .push(section_header("Interruptions (Esc'd mid-tool-call)"));
    let i = &m.interruptions;
    if i.total_interrupted_turns == 0 {
        r.lines
            .push(Line::from(Span::styled("  (none detected)", DIM)));
    } else {
        r.lines.push(Line::from(vec![
            Span::styled("  Wasted ", LABEL),
            Span::styled(
                fmt_cost(i.total_wasted_cost),
                VAL.fg(Color::Rgb(220, 140, 140)),
            ),
            Span::styled("    Turns ", LABEL),
            Span::styled(format!("{}", i.total_interrupted_turns), VAL),
            Span::styled("    Sessions ", LABEL),
            Span::styled(format!("{}", i.sessions_affected), VAL),
        ]));
        for entry in i.by_session.iter() {
            let sid = short_sid(&entry.session_id).to_string();
            let (marker, sid_style) = selection_row_style(r.next_row_selected());
            r.push_row(Line::from(vec![
                Span::styled(format!("{}{:<10}", marker, sid), sid_style),
                Span::styled(
                    format!("{:>8}", fmt_cost(entry.wasted_cost)),
                    VAL.fg(Color::Rgb(220, 140, 140)),
                ),
                Span::styled(format!("  {:>3} orphan", entry.orphan_count), DIM),
                Span::raw("  "),
                Span::styled(
                    pad(&entry.last_tool_name, 18),
                    Style::default().fg(FAINT_TEXT),
                ),
                Span::styled(pad(&entry.project, 24), Style::default().fg(FAINT_TEXT)),
            ]));
        }
    }
    r.lines.push(Line::raw(""));
}

fn peak_context(r: &mut Report, m: &MetricsAnalysis) {
    r.lines.push(section_header("Peak context reached"));
    let pc = &m.peak_context;
    if pc.findings.is_empty() {
        r.lines
            .push(Line::from(Span::styled("  (no sessions)", DIM)));
    } else {
        for f in pc.findings.iter() {
            let sid = short_sid(&f.session_id).to_string();
            let (marker, sid_style) = selection_row_style(r.next_row_selected());
            r.push_row(Line::from(vec![
                Span::styled(format!("{}{:<10}", marker, sid), sid_style),
                Span::styled(
                    format!("{:>8} ctx", format_tokens(f.peak_ctx_tokens)),
                    VAL.fg(Color::Rgb(220, 180, 130)),
                ),
                Span::styled(
                    format!("  {:>8}", fmt_cost(f.total_cost)),
                    VAL.fg(Color::Green),
                ),
                Span::styled(
                    format!("  @ turn {}/{}", f.peak_turn_index, f.assistant_turns),
                    DIM,
                ),
                Span::raw("  "),
                Span::styled(pad(&f.project, 24), Style::default().fg(FAINT_TEXT)),
            ]));
        }
    }
    r.lines.push(Line::raw(""));
}

fn token_spikes(r: &mut Report, m: &MetricsAnalysis) {
    r.lines
        .push(section_header("Token spikes (outlier single-turn deltas)"));
    let g = &m.context_growth;
    r.lines.push(Line::from(vec![
        Span::styled("  Scored ", LABEL),
        Span::styled(format!("{}", g.sessions_scored), VAL),
        Span::styled("    Spikes ", LABEL),
        Span::styled(format!("{}", g.findings.len()), VAL),
        Span::styled("    Cost in flagged sessions ", LABEL),
        Span::styled(
            fmt_cost(g.anomalous_cost),
            VAL.fg(Color::Rgb(220, 180, 130)),
        ),
    ]));
    r.lines.push(Line::from(Span::styled(
        "  score = peak turn delta / median turn delta — flags one-shot bursts, not total growth",
        DIM,
    )));
    if g.findings.is_empty() {
        r.lines.push(Line::from(Span::styled("  (no spikes)", DIM)));
    } else {
        for f in g.findings.iter() {
            let sid = short_sid(&f.session_id).to_string();
            let (marker, sid_style) = selection_row_style(r.next_row_selected());
            r.push_row(Line::from(vec![
                Span::styled(format!("{}{:<10}", marker, sid), sid_style),
                Span::styled(
                    format!("{:>5.1}x", f.score),
                    VAL.fg(Color::Rgb(220, 180, 130)),
                ),
                Span::styled(
                    format!("  {:>8}", fmt_cost(f.total_cost)),
                    VAL.fg(Color::Green),
                ),
                Span::styled(
                    format!(
                        "  +{:>8} @ turn {}/{}",
                        format_tokens(f.peak_delta_tokens),
                        f.peak_turn_index,
                        f.assistant_turns
                    ),
                    DIM,
                ),
                Span::raw("  "),
                Span::styled(pad(&f.project, 24), Style::default().fg(FAINT_TEXT)),
            ]));
        }
    }
    r.lines.push(Line::raw(""));
}

fn top_sessions(r: &mut Report, m: &MetricsAnalysis) {
    r.lines.push(section_header("Top sessions"));
    r.lines.push(Line::from(Span::styled(
        format!(
            "  {:<10} {:>8} {:>10} {:<22} {:<24}",
            "session", "cost", "tokens", "model", "project"
        ),
        DIM,
    )));
    for s in &m.top_sessions {
        let line = format_session_row(s, r.next_row_selected());
        r.push_row(line);
    }
}
