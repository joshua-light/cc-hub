//! Agents tab: a row per persistent agent, and the detail view behind `f`.
//!
//! The row answers "is it on, and did its last run work?" at a glance. The
//! detail leads with the same answer in words — why it failed, why it is
//! halted — then splits the rest into sections (Runs, Artifacts, Log,
//! Settings), each a list with the selected item spelled out below it.

use crate::app::{App, Detail, Section, View};
use crate::harness::settings::{Setting, SETTINGS};
use crate::harness::{AgentSnapshot, AgentStatus, LogLine, Note, Run, TickRecord};
use crate::models::first_line_truncated;
use crate::ui::common::{centered_rect, fmt_cost, format_tokens, popup_block, Cell, COL_SEP};
use crate::ui::palette::{
    ACCENT_BLUE, DIM_TEXT, FAINT_TEXT, LABEL_GRAY, MUTED_TEXT, PURPLE, SEP_GRAY,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

const SELECTED_BG: Color = Color::Rgb(40, 40, 52);

pub(crate) fn status_indicator(status: AgentStatus) -> (&'static str, Color) {
    match status {
        AgentStatus::Ticking => ("󰒓", Color::Green),
        AgentStatus::Sleeping => ("󰒲", MUTED_TEXT),
        AgentStatus::Halted => ("󰂞", Color::Yellow),
        AgentStatus::Paused => ("󰏤", PURPLE),
        AgentStatus::Disabled => ("󰜎", SEP_GRAY),
        AgentStatus::Broken => ("󰅙", Color::Red),
    }
}

pub(crate) fn age(now: i64, at: i64) -> String {
    let d = (now - at).max(0);
    if d < 60 {
        format!("{}s", d)
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86_400 {
        format!("{}h", d / 3600)
    } else {
        format!("{}d", d / 86_400)
    }
}

/// Inbox files are named `<utc stamp>-<label>`, poll events `poll-<hash>`,
/// timer events `tick-<n>`: keep the part a person reads.
fn event_label(id: Option<&str>) -> String {
    let Some(id) = id else {
        return "manual".into();
    };
    if id.starts_with("poll-") {
        return "poll".into();
    }
    if id.starts_with("tick-") {
        return "timer".into();
    }
    let stamped =
        id.len() > 16 && id.as_bytes()[8] == b'T' && id[..8].bytes().all(|b| b.is_ascii_digit());
    match id.split_once("Z-") {
        Some((_, rest)) if stamped && !rest.is_empty() => rest.to_string(),
        _ => id.to_string(),
    }
}

fn clock(at: i64, now: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(t) = Local.timestamp_opt(at, 0).single() else {
        return String::new();
    };
    let today = Local
        .timestamp_opt(now, 0)
        .single()
        .map(|n| n.date_naive() == t.date_naive())
        .unwrap_or(false);
    if today {
        t.format("%H:%M").to_string()
    } else {
        t.format("%b %d %H:%M").to_string()
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cut spans so the line never wraps.
fn truncate_line(line: Line<'_>, width: usize) -> Line<'_> {
    let mut used = 0;
    let mut spans = Vec::new();
    for span in line.spans {
        let w = span.content.chars().count();
        if used + w <= width {
            used += w;
            spans.push(span);
        } else {
            let room = width.saturating_sub(used);
            if room > 1 {
                let s: String = span.content.chars().take(room - 1).collect();
                spans.push(Span::styled(format!("{}…", s), span.style));
            }
            break;
        }
    }
    Line::from(spans)
}

fn pad(s: &str, w: usize) -> String {
    let s = first_line_truncated(s, w);
    let n = s.chars().count();
    format!("{}{}", s, " ".repeat(w.saturating_sub(n)))
}

fn pad_left(s: &str, w: usize) -> String {
    let n = s.chars().count();
    format!("{}{}", " ".repeat(w.saturating_sub(n)), s)
}

// ---- table ----------------------------------------------------------------
//
// One row per agent: status icon, name, last run, what the agent has to
// say, and a right cluster (trigger, spend today) that drops at narrow
// widths for every row at once so the columns stay aligned.

const NAME_W: usize = 18;
/// Last run: "✓ 12m", "✗ 3d", "▸ 40s".
const LAST_W: usize = 6;
/// "poll 5m ← board +2".
const TRIGGER_W: usize = 16;
const COST_W: usize = 7;
/// Selection stripe(1) + status icon(1) + space(1).
const LEFT_FIXED: usize = 3;
const MIN_SAY: usize = 20;

struct Columns {
    trigger: bool,
    cost: bool,
}

fn plan_columns(width: usize) -> Columns {
    let mut avail = width.saturating_sub(LEFT_FIXED + NAME_W + COL_SEP + LAST_W + MIN_SAY);
    let mut take = |w: usize| {
        let fits = avail >= w + COL_SEP;
        if fits {
            avail -= w + COL_SEP;
        }
        fits
    };
    let trigger = take(TRIGGER_W);
    let cost = take(COST_W);
    Columns { trigger, cost }
}

pub(crate) fn render_agents_body(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.height < 1 || area.width < 10 {
        return;
    }

    if app.harness.agents.is_empty() {
        let msg = if !app.harness.loaded {
            " Reading agents …".to_string()
        } else {
            let root = crate::harness::root()
                .map(|r| r.display().to_string())
                .unwrap_or_else(|| "~/.cc-hub/agents".into());
            format!(
                " No agents yet. Scaffold one with `cc-hub agent new <name>` — specs live in {}",
                root
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(msg, Style::default().fg(DIM_TEXT))))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let selected = app.harness.selected as u16;
    if selected < app.render.agents_scroll {
        app.render.agents_scroll = selected;
    } else if selected >= app.render.agents_scroll + area.height {
        app.render.agents_scroll = selected + 1 - area.height;
    }

    let now = crate::harness::now_unix();
    let cols = plan_columns(area.width as usize);
    let first = app.render.agents_scroll as usize;
    for (row, agent) in app
        .harness
        .agents
        .iter()
        .enumerate()
        .skip(first)
        .take(area.height as usize)
    {
        let y = area.y + (row - first) as u16;
        let rect = Rect::new(area.x, y, area.width, 1);
        render_row(frame, rect, agent, &cols, row == app.harness.selected, now);
    }
}

/// What the agent has to say for itself, most urgent first: a spec that
/// won't load, a halt, a failed last run, then its newest report, its last
/// answer, and finally what it is for.
fn say(agent: &AgentSnapshot) -> (String, Style) {
    if let Err(e) = &agent.spec {
        return (e.clone(), Style::default().fg(Color::Red));
    }
    if let Some(reason) = &agent.state.stopped_reason {
        return (reason.clone(), Style::default().fg(Color::Yellow));
    }
    let off = matches!(agent.status(), AgentStatus::Disabled | AgentStatus::Paused);
    let faint = Style::default().fg(if off { DIM_TEXT } else { FAINT_TEXT });
    if let Some(run) = agent.last_run().filter(|r| !r.ok) {
        let text = format!(
            "{}: {}",
            run.subtype.as_deref().unwrap_or("failed"),
            one_line(&run.result)
        );
        return (text, Style::default().fg(Color::Red));
    }
    if let Some(note) = agent.notes.first() {
        let style = if note.level == "warn" && !off {
            Style::default().fg(Color::Yellow)
        } else {
            faint
        };
        return (note.text.clone(), style);
    }
    if !agent.state.last_result.is_empty() {
        return (agent.state.last_result.clone(), faint);
    }
    (
        agent.description().to_string(),
        Style::default().fg(DIM_TEXT),
    )
}

/// The last-run cell: a running tick's elapsed time, else the last run's
/// mark and age.
fn last_run_spans(agent: &AgentSnapshot, now: i64) -> Vec<Span<'static>> {
    if let Some(t) = &agent.state.ticking {
        return vec![Span::styled(
            pad(&format!("▸ {}", age(now, t.since)), LAST_W),
            Style::default().fg(Color::Green),
        )];
    }
    match agent.last_run() {
        Some(rec) => {
            let (mark, color) = if rec.ok {
                ("✓", Color::Green)
            } else {
                ("✗", Color::Red)
            };
            vec![
                Span::styled(mark, Style::default().fg(color)),
                Span::styled(
                    pad(&format!(" {}", age(now, rec.at)), LAST_W - 1),
                    Style::default().fg(DIM_TEXT),
                ),
            ]
        }
        None => vec![Span::raw(" ".repeat(LAST_W))],
    }
}

fn render_row(
    frame: &mut Frame,
    area: Rect,
    agent: &AgentSnapshot,
    cols: &Columns,
    selected: bool,
    now: i64,
) {
    let width = area.width as usize;
    let status = agent.status();
    let off = matches!(status, AgentStatus::Disabled | AgentStatus::Paused);
    let (glyph, color) = status_indicator(status);
    let glyph = if status == AgentStatus::Ticking {
        crate::ui::sessions::spinner_frame(crate::ui::now_ms())
    } else {
        glyph
    };

    // Waiting events ride on the trigger they will wake: `inbox +2`.
    let trigger_spans = cols.trigger.then(|| {
        let label = match &agent.spec {
            Ok(spec) => spec.trigger_label(),
            Err(_) => String::new(),
        };
        let queued = (agent.inbox_pending > 0).then(|| format!(" +{}", agent.inbox_pending));
        let room = TRIGGER_W.saturating_sub(queued.as_ref().map_or(0, |q| q.chars().count()));
        let label = first_line_truncated(&label, room);
        let used = label.chars().count() + queued.as_ref().map_or(0, |q| q.chars().count());
        let mut spans = vec![
            Span::raw(" ".repeat(COL_SEP)),
            Span::styled(label, Style::default().fg(MUTED_TEXT)),
        ];
        if let Some(q) = queued {
            spans.push(Span::styled(q, Style::default().fg(Color::Yellow)));
        }
        spans.push(Span::raw(" ".repeat(TRIGGER_W.saturating_sub(used))));
        spans
    });
    let cost_cell = cols.cost.then(|| {
        let spent = agent.state.today_cost();
        Cell {
            text: if spent > 0.0 {
                fmt_cost(spent)
            } else {
                String::new()
            },
            target: COST_W,
            style: Style::default().fg(DIM_TEXT),
            right_align: true,
        }
    });
    let cluster_width = trigger_spans.as_ref().map_or(0, |_| TRIGGER_W + COL_SEP)
        + cost_cell.as_ref().map_or(0, |c| c.target + COL_SEP);

    let name_style = if status.needs_attention() {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else if off {
        Style::default().fg(LABEL_GRAY)
    } else {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    };
    let (say_text, say_style) = say(agent);
    let say_budget =
        width.saturating_sub(LEFT_FIXED + NAME_W + COL_SEP + LAST_W + COL_SEP + cluster_width);
    let say_text = first_line_truncated(&one_line(&say_text), say_budget);

    let mut spans: Vec<Span<'static>> = vec![
        if selected {
            Span::styled("▌", Style::default().fg(Color::White))
        } else {
            Span::raw(" ")
        },
        Span::styled(format!("{} ", glyph), Style::default().fg(color)),
        Span::styled(pad(&agent.name, NAME_W), name_style),
        Span::raw(" ".repeat(COL_SEP)),
    ];
    spans.extend(last_run_spans(agent, now));
    spans.push(Span::raw(" ".repeat(COL_SEP)));
    let say_pad = say_budget.saturating_sub(say_text.chars().count());
    spans.push(Span::styled(say_text, say_style));
    spans.push(Span::raw(" ".repeat(say_pad)));
    spans.extend(trigger_spans.into_iter().flatten());
    if let Some(cell) = cost_cell {
        cell.push_spans(&mut spans);
    }

    let mut row = Paragraph::new(truncate_line(Line::from(spans), width));
    if selected {
        row = row.style(Style::default().bg(SELECTED_BG));
    }
    frame.render_widget(row, area);
}

// ---- detail ---------------------------------------------------------------

pub(crate) fn render_agent_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    render_agent_detail_at(frame, area, app, crate::harness::now_unix());
}

fn render_agent_detail_at(frame: &mut Frame, area: Rect, app: &mut App, now: i64) {
    let Some(agent) = app.harness.agents.get(app.harness.selected) else {
        return;
    };
    let Some(detail) = app.harness.detail.as_ref() else {
        return;
    };
    let popup = centered_rect(area, 0.9);
    frame.render_widget(Clear, popup);

    let (glyph, color) = status_indicator(agent.status());
    let block = popup_block(Line::from(Span::styled(
        format!(" {} {} ", glyph, agent.name),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.height < 4 || inner.width < 20 {
        return;
    }
    // One column of breathing room each side.
    let inner = Rect::new(inner.x + 1, inner.y, inner.width - 2, inner.height);
    let width = inner.width as usize;

    let mut head = headline(agent, now, width);
    head.push(Line::default());
    head.push(section_strip(agent, detail.section));
    head.push(Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(SEP_GRAY),
    )));
    let head_h = (head.len() as u16).min(inner.height);
    frame.render_widget(
        Paragraph::new(head),
        Rect::new(inner.x, inner.y, inner.width, head_h),
    );
    let body = Rect::new(
        inner.x,
        inner.y + head_h,
        inner.width,
        inner.height - head_h,
    );
    let scroll = &mut app.render.agent_detail_scroll;
    match detail.section {
        Section::Runs => render_runs(frame, body, agent, detail, scroll, now),
        Section::Artifacts => render_artifacts(frame, body, agent, detail, scroll, now),
        Section::Log => render_log(frame, body, agent, detail, scroll, now),
        Section::Settings => render_settings(frame, body, agent, detail, scroll, now),
    }
}

/// The top of the detail: what the agent is for, then the one or two
/// facts that decide whether you need to do anything.
fn headline(agent: &AgentSnapshot, now: i64, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let desc = agent.description();
    if !desc.is_empty() {
        lines.push(Line::from(Span::styled(
            first_line_truncated(desc, width),
            Style::default().fg(DIM_TEXT),
        )));
        lines.push(Line::default());
    }
    let wrap = |text: &str, style: Style, lines: &mut Vec<Line<'static>>, max: usize| {
        for l in wrap_text(text, width.saturating_sub(2))
            .into_iter()
            .take(max)
        {
            lines.push(Line::from(Span::styled(format!("  {}", l), style)));
        }
    };

    let status = agent.status();
    let (glyph, color) = status_indicator(status);
    let lead = |text: String| {
        Line::from(vec![
            Span::styled(format!("{} ", glyph), Style::default().fg(color)),
            Span::styled(
                text,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ])
    };
    match status {
        AgentStatus::Broken => {
            lines.push(lead(
                "agent.toml doesn't load — n opens Claude in its folder to fix it".into(),
            ));
            if let Err(e) = &agent.spec {
                wrap(e, Style::default().fg(Color::Red), &mut lines, 3);
            }
        }
        AgentStatus::Halted => {
            let reason = agent.state.stopped_reason.clone().unwrap_or_default();
            lines.push(lead(format!("Halted: {} — space resumes it", reason)));
        }
        AgentStatus::Disabled => lines.push(lead("Off — space turns it on".into())),
        AgentStatus::Paused => lines.push(lead("Paused — space resumes it".into())),
        AgentStatus::Ticking => {
            if let Some(t) = &agent.state.ticking {
                lines.push(lead(format!(
                    "Running for {} · {}",
                    age(now, t.since),
                    event_label(t.event.as_deref())
                )));
            }
        }
        AgentStatus::Sleeping => {}
    }

    match agent.last_run() {
        Some(rec) if rec.ok => {
            lines.push(Line::from(vec![
                Span::styled("✓ ", Style::default().fg(Color::Green)),
                Span::styled(
                    format!("Last run {} ago", age(now, rec.at)),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!(
                        " · {} turns · {} · {}",
                        rec.turns,
                        crate::ui::common::format_duration_secs(rec.duration_s),
                        fmt_cost(rec.cost_usd)
                    ),
                    Style::default().fg(DIM_TEXT),
                ),
            ]));
            if !rec.result.is_empty() {
                wrap(&rec.result, Style::default().fg(FAINT_TEXT), &mut lines, 1);
            }
        }
        Some(rec) => {
            lines.push(Line::from(vec![
                Span::styled("✗ ", Style::default().fg(Color::Red)),
                Span::styled(
                    format!("Last run failed {} ago", age(now, rec.at)),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" · {}", rec.subtype.as_deref().unwrap_or("error")),
                    Style::default().fg(DIM_TEXT),
                ),
            ]));
            wrap(&rec.result, Style::default().fg(FAINT_TEXT), &mut lines, 2);
            if let Some(d) = &rec.detail {
                wrap(d, Style::default().fg(DIM_TEXT), &mut lines, 1);
            }
        }
        None if agent.state.ticking.is_none() => lines.push(Line::from(Span::styled(
            "No runs yet — p runs it now",
            Style::default().fg(DIM_TEXT),
        ))),
        None => {}
    }
    lines
}

fn section_strip(agent: &AgentSnapshot, active: Section) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, s) in Section::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("   ", Style::default()));
        }
        let count = match s {
            Section::Runs => agent.runs().len(),
            Section::Artifacts => agent.notes.len(),
            Section::Log | Section::Settings => 0,
        };
        let style = if *s == active {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(LABEL_GRAY)
        };
        spans.push(Span::styled(s.label(), style));
        if count > 0 {
            spans.push(Span::styled(
                format!(" {}", count),
                Style::default().fg(DIM_TEXT),
            ));
        }
    }
    Line::from(spans)
}

/// Greedy word wrap for plain text, preserving explicit line breaks.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for para in text.lines() {
        let mut cur = String::new();
        for word in para.split_whitespace() {
            let wl = word.chars().count();
            let cl = cur.chars().count();
            if cl > 0 && cl + 1 + wl > width {
                out.push(std::mem::take(&mut cur));
            }
            if wl > width {
                // A long URL or path: hard-cut it.
                let chars: Vec<char> = word.chars().collect();
                for chunk in chars.chunks(width) {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur = chunk.iter().collect();
                }
                continue;
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        out.push(cur);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

/// Split a section body into the list and the pane that spells out the
/// selected item, right under the list's last row. A long list scrolls and
/// leaves the pane at least a third of the height.
fn split_body(body: Rect, rows: usize) -> (Rect, Rect) {
    let pane_min = (body.height / 3).max(5);
    let list_h = (rows as u16)
        .min(body.height.saturating_sub(pane_min))
        .max(1);
    let list_h = list_h.min(body.height);
    (
        Rect::new(body.x, body.y, body.width, list_h),
        Rect::new(body.x, body.y + list_h, body.width, body.height - list_h),
    )
}

/// Render one-line rows with `cursor` highlighted and kept on screen.
fn render_list(
    frame: &mut Frame,
    area: Rect,
    rows: Vec<Line<'static>>,
    cursor: usize,
    scroll: &mut usize,
) {
    let h = area.height as usize;
    if h == 0 {
        return;
    }
    let cursor = cursor.min(rows.len().saturating_sub(1));
    if cursor < *scroll {
        *scroll = cursor;
    } else if cursor >= *scroll + h {
        *scroll = cursor + 1 - h;
    }
    *scroll = (*scroll).min(rows.len().saturating_sub(h));
    let width = area.width as usize;
    for (i, line) in rows.into_iter().enumerate().skip(*scroll).take(h) {
        let y = area.y + (i - *scroll) as u16;
        let selected = i == cursor;
        let mut spans = vec![if selected {
            Span::styled("▌", Style::default().fg(Color::White))
        } else {
            Span::raw(" ")
        }];
        spans.extend(line.spans);
        let mut p = Paragraph::new(truncate_line(Line::from(spans), width));
        if selected {
            p = p.style(Style::default().bg(SELECTED_BG));
        }
        frame.render_widget(p, Rect::new(area.x, y, area.width, 1));
    }
}

/// The lower pane: a rule, then wrapped lines.
fn render_pane(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    if area.height < 2 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(SEP_GRAY),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        Rect::new(area.x, area.y + 1, area.width, area.height - 1),
    );
}

fn empty(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}", text),
            Style::default().fg(DIM_TEXT),
        )))
        .wrap(Wrap { trim: false }),
        area,
    );
}

// ---- runs -----------------------------------------------------------------

const RUN_N_W: usize = 6;
const RUN_AGE_W: usize = 9;
const RUN_EVENT_W: usize = 14;
const RUN_DUR_W: usize = 7;
const RUN_COST_W: usize = 7;

fn run_row(run: &Run, now: i64) -> Line<'static> {
    let dim = Style::default().fg(DIM_TEXT);
    match run {
        Run::InFlight { n, ticking } => Line::from(vec![
            Span::styled(
                format!(
                    "{} ",
                    crate::ui::sessions::spinner_frame(crate::ui::now_ms())
                ),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                pad(&format!("#{}", n), RUN_N_W),
                Style::default().fg(LABEL_GRAY),
            ),
            Span::styled(
                pad(&format!("{} ago", age(now, ticking.since)), RUN_AGE_W),
                dim,
            ),
            Span::styled(
                pad(&event_label(ticking.event.as_deref()), RUN_EVENT_W),
                Style::default().fg(FAINT_TEXT),
            ),
            Span::raw(" ".repeat(RUN_DUR_W + RUN_COST_W + 2)),
            Span::styled("running…", Style::default().fg(Color::Green)),
        ]),
        Run::Done { n, rec } => {
            let (mark, color) = if rec.ok {
                ("✓", Color::Green)
            } else {
                ("✗", Color::Red)
            };
            let outcome = if rec.ok {
                Span::styled(one_line(&rec.result), Style::default().fg(FAINT_TEXT))
            } else {
                Span::styled(
                    rec.subtype.clone().unwrap_or_else(|| "failed".into()),
                    Style::default().fg(Color::Red),
                )
            };
            Line::from(vec![
                Span::styled(format!("{} ", mark), Style::default().fg(color)),
                Span::styled(
                    pad(&format!("#{}", n), RUN_N_W),
                    Style::default().fg(LABEL_GRAY),
                ),
                Span::styled(pad(&format!("{} ago", age(now, rec.at)), RUN_AGE_W), dim),
                Span::styled(
                    pad(&event_label(rec.event.as_deref()), RUN_EVENT_W),
                    Style::default().fg(FAINT_TEXT),
                ),
                Span::styled(
                    pad_left(
                        &crate::ui::common::format_duration_secs(rec.duration_s),
                        RUN_DUR_W,
                    ),
                    dim,
                ),
                Span::styled(pad_left(&fmt_cost(rec.cost_usd), RUN_COST_W), dim),
                Span::raw("  "),
                outcome,
            ])
        }
    }
}

fn run_pane(run: &Run, now: i64) -> Vec<Line<'static>> {
    let label = Style::default().fg(LABEL_GRAY);
    let dim = Style::default().fg(DIM_TEXT);
    let mut lines = Vec::new();
    match run {
        Run::InFlight { n, ticking } => {
            lines.push(Line::from(vec![
                Span::styled(format!("#{} ", n), label),
                Span::styled(
                    format!(
                        "running since {} · {}",
                        clock(ticking.since, now),
                        event_label(ticking.event.as_deref())
                    ),
                    Style::default().fg(Color::Green),
                ),
            ]));
            lines.push(Line::from(Span::styled("f follows it live.", dim)));
        }
        Run::Done { n, rec } => {
            lines.push(Line::from(vec![
                Span::styled(format!("#{} ", n), label),
                Span::styled(run_facts(rec, now), dim),
            ]));
            let style = Style::default().fg(if rec.ok { FAINT_TEXT } else { Color::Red });
            if !rec.result.is_empty() {
                lines.push(Line::from(Span::styled(rec.result.clone(), style)));
            }
            if let Some(d) = &rec.detail {
                lines.push(Line::from(Span::styled(format!("stderr: {}", d), dim)));
            }
            if let Some(ev) = &rec.event {
                lines.push(Line::from(Span::styled(format!("event {}", ev), dim)));
            }
        }
    }
    lines
}

fn run_facts(rec: &TickRecord, now: i64) -> String {
    let mut facts = vec![
        clock(rec.at, now),
        crate::ui::common::format_duration_secs(rec.duration_s),
        format!("{} turns", rec.turns),
        fmt_cost(rec.cost_usd),
    ];
    if rec.context_end > 0 {
        facts.push(format!(
            "context {} → {}",
            format_tokens(rec.context_start),
            format_tokens(rec.context_end)
        ));
    }
    if rec.compactions > 0 {
        facts.push(format!("{} compactions", rec.compactions));
    }
    if !rec.ok {
        facts.insert(0, rec.subtype.clone().unwrap_or_else(|| "failed".into()));
    }
    facts.join(" · ")
}

fn render_runs(
    frame: &mut Frame,
    body: Rect,
    agent: &AgentSnapshot,
    d: &Detail,
    scroll: &mut usize,
    now: i64,
) {
    let runs = agent.runs();
    if runs.is_empty() {
        empty(frame, body, "No runs yet. p queues one now.");
        return;
    }
    let (list, pane) = split_body(body, runs.len());
    let rows = runs.iter().map(|r| run_row(r, now)).collect();
    render_list(frame, list, rows, d.run, scroll);
    if let Some(run) = runs.get(d.run.min(runs.len() - 1)) {
        render_pane(frame, pane, run_pane(run, now));
    }
}

// ---- artifacts ------------------------------------------------------------

fn note_row(note: &Note, now: i64) -> Line<'static> {
    let (mark, color) = if note.level == "warn" {
        ("●", Color::Yellow)
    } else {
        ("·", DIM_TEXT)
    };
    let mut spans = vec![
        Span::styled(format!("{} ", mark), Style::default().fg(color)),
        Span::styled(
            pad(&format!("{} ago", age(now, note.at)), RUN_AGE_W),
            Style::default().fg(DIM_TEXT),
        ),
    ];
    if note.r#ref.is_some() {
        spans.push(Span::styled("󰌷 ", Style::default().fg(ACCENT_BLUE)));
    } else {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(
        one_line(&note.text),
        Style::default().fg(FAINT_TEXT),
    ));
    Line::from(spans)
}

fn render_artifacts(
    frame: &mut Frame,
    body: Rect,
    agent: &AgentSnapshot,
    d: &Detail,
    scroll: &mut usize,
    now: i64,
) {
    if agent.notes.is_empty() {
        empty(
            frame,
            body,
            "Nothing reported yet. An agent reports with `cc-hub agent note --text … --ref <url or path>`.",
        );
        return;
    }
    let (list, pane) = split_body(body, agent.notes.len());
    let rows = agent.notes.iter().map(|n| note_row(n, now)).collect();
    render_list(frame, list, rows, d.artifact, scroll);
    let note = &agent.notes[d.artifact.min(agent.notes.len() - 1)];
    let mut lines = vec![
        Line::from(Span::styled(
            format!("run #{} · {}", note.tick, clock(note.at, now)),
            Style::default().fg(DIM_TEXT),
        )),
        Line::from(Span::styled(
            note.text.clone(),
            Style::default().fg(Color::White),
        )),
    ];
    if let Some(r) = &note.r#ref {
        lines.push(Line::from(vec![
            Span::styled("󰌷 ", Style::default().fg(ACCENT_BLUE)),
            Span::styled(r.clone(), Style::default().fg(ACCENT_BLUE)),
            Span::styled("   f opens it", Style::default().fg(DIM_TEXT)),
        ]));
    }
    render_pane(frame, pane, lines);
}

// ---- log ------------------------------------------------------------------

fn level_style(level: &str) -> Style {
    Style::default().fg(match level {
        "error" => Color::Red,
        "warn" => Color::Yellow,
        _ => FAINT_TEXT,
    })
}

/// Wide enough for `Sep 24 14:02` and a gap.
const LOG_TIME_W: usize = 14;

fn log_row(line: &LogLine, now: i64) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            pad(&clock(line.at, now), LOG_TIME_W),
            Style::default().fg(DIM_TEXT),
        ),
        Span::styled(one_line(&line.text), level_style(&line.level)),
    ])
}

fn render_log(
    frame: &mut Frame,
    body: Rect,
    agent: &AgentSnapshot,
    d: &Detail,
    scroll: &mut usize,
    now: i64,
) {
    if agent.events.is_empty() {
        empty(
            frame,
            body,
            "Nothing logged yet. Runs, poll failures, halts and changes made here land in events.jsonl.",
        );
        return;
    }
    let line = &agent.events[d.log_scroll.min(agent.events.len() - 1)];
    // The pane only earns its rows when the selected line doesn't fit.
    let fits = LOG_TIME_W + 1 + one_line(&line.text).chars().count() <= body.width as usize;
    let rows: Vec<Line<'static>> = agent.events.iter().map(|l| log_row(l, now)).collect();
    if fits {
        render_list(frame, body, rows, d.log_scroll, scroll);
        return;
    }
    let (list, pane) = split_body(body, agent.events.len());
    render_list(frame, list, rows, d.log_scroll, scroll);
    render_pane(
        frame,
        pane,
        vec![
            Line::from(Span::styled(
                clock(line.at, now),
                Style::default().fg(DIM_TEXT),
            )),
            Line::from(Span::styled(line.text.clone(), level_style(&line.level))),
        ],
    );
}

// ---- settings -------------------------------------------------------------

const LABEL_W: usize = 16;

fn render_settings(
    frame: &mut Frame,
    body: Rect,
    agent: &AgentSnapshot,
    d: &Detail,
    scroll: &mut usize,
    now: i64,
) {
    let _ = now;
    let spec = match &agent.spec {
        Ok(spec) => spec,
        Err(e) => {
            let lines = vec![
                Line::from(Span::styled(e.clone(), Style::default().fg(Color::Red))),
                Line::default(),
                Line::from(Span::styled(
                    "Settings need a spec that loads. n opens Claude in the agent's folder to fix it.",
                    Style::default().fg(DIM_TEXT),
                )),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
            return;
        }
    };
    let (list, pane) = split_body(body, SETTINGS.len());
    let label = Style::default().fg(LABEL_GRAY);
    let rows: Vec<Line<'static>> = SETTINGS
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let editing = (i == d.setting).then_some(d.editing.as_ref()).flatten();
            let value = match editing {
                Some(buf) => Span::styled(
                    format!("{}▏", buf),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                None if s.locked(spec).is_some() => {
                    Span::styled(s.show(spec), Style::default().fg(DIM_TEXT))
                }
                None => Span::styled(s.show(spec), setting_style(*s, spec)),
            };
            Line::from(vec![Span::styled(pad(s.label(), LABEL_W), label), value])
        })
        .collect();
    render_list(frame, list, rows, d.setting, scroll);

    let setting = SETTINGS[d.setting.min(SETTINGS.len() - 1)];
    let dim = Style::default().fg(DIM_TEXT);
    let mut lines = vec![Line::from(Span::styled(
        setting
            .locked(spec)
            .map(|why| format!("Fixed: {}.", why))
            .unwrap_or_else(|| setting.help().to_string()),
        Style::default().fg(FAINT_TEXT),
    ))];
    if d.editing.is_some() {
        lines.push(Line::from(Span::styled("enter saves · esc cancels", dim)));
    }
    lines.push(Line::default());
    let trigger = match spec.trigger.kind {
        crate::harness::spec::TriggerKind::Poll => format!(
            "poll `{}` every {}",
            spec.trigger.command.as_deref().unwrap_or(""),
            crate::harness::spec::fmt_secs(spec.trigger.interval_s)
        ),
        _ => spec.trigger_label(),
    };
    let st = &agent.state;
    for (k, v) in [
        ("trigger", trigger),
        (
            "spent",
            format!(
                "{} today ({} runs) · {} in total",
                fmt_cost(st.today_cost()),
                st.today_ticks(),
                fmt_cost(st.cost_usd)
            ),
        ),
        ("workdir", tilde(&spec.workdir)),
        ("folder", tilde(&agent.dir)),
    ] {
        lines.push(Line::from(vec![
            Span::styled(pad(k, LABEL_W), dim),
            Span::styled(v, dim),
        ]));
    }
    render_pane(frame, pane, lines);
}

fn setting_style(s: Setting, spec: &crate::harness::Spec) -> Style {
    let on = Style::default().fg(Color::Green);
    match s {
        Setting::Enabled if spec.enabled => on,
        Setting::Enabled => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::White),
    }
}

fn tilde(p: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(&h).ok().map(|r| r.to_path_buf())) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

/// Key hints for the Agents tab and its detail.
pub(crate) fn hints(app: &App) -> &'static str {
    let Some(d) = app
        .harness
        .detail
        .as_ref()
        .filter(|_| app.view == View::AgentDetail)
    else {
        return "f/enter:open  space:on/off  p:run now  n:claude here  j/k:nav  tab:next  q:quit";
    };
    if d.editing.is_some() {
        return "type a value  enter:save  esc:cancel";
    }
    match d.section {
        Section::Runs => "f/enter:transcript  j/k:run  h/l:section  space:on/off  p:run now  n:claude here  R:reset  esc:close",
        Section::Artifacts => "f/enter:open  j/k:select  h/l:section  space:on/off  p:run now  n:claude here  esc:close",
        Section::Log => "j/k:scroll  h/l:section  space:on/off  p:run now  n:claude here  esc:close",
        Section::Settings => "enter:change  e:type value  j/k:field  h/l:section  space:on/off  n:claude here  esc:close",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{AgentState, Note, TickRecord, Ticking};
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::Path;

    const NOW: i64 = 1_800_000_000;

    fn snap(name: &str, state: AgentState) -> AgentSnapshot {
        let dir = Path::new("/tmp/agents").join(name);
        let spec = crate::harness::spec::parse(
            &dir,
            "description = \"Watches things\"\n[trigger]\nkind = \"poll\"\ncommand = \"./watch.sh\"\ninterval_s = 300\n[run]\ndaily_budget_usd = 5.0\n[prompt]\ninstruction = \"go\"",
        );
        AgentSnapshot {
            name: name.into(),
            dir,
            spec,
            state,
            notes: vec![Note {
                at: NOW - 120,
                level: "warn".into(),
                text: "PR #418 lint fails".into(),
                r#ref: Some("https://example.com/pr/418".into()),
                tick: 3,
            }],
            events: vec![LogLine {
                at: NOW - 60,
                level: "warn".into(),
                text: "poll `./watch.sh` failed (exit status: 1): token expired".into(),
            }],
            inbox_pending: 2,
        }
    }

    fn rec(ok: bool, result: &str) -> TickRecord {
        TickRecord {
            at: NOW - 300,
            event: Some("20260903T191749.728Z-poke".into()),
            ok,
            subtype: Some(
                if ok {
                    "success"
                } else {
                    "error_max_budget_usd"
                }
                .into(),
            ),
            turns: 4,
            compactions: 0,
            cost_usd: 0.07,
            context_start: 4000,
            context_end: 13_000,
            duration_s: 12,
            session_id: Some("sid-1".into()),
            result: result.into(),
            detail: (!ok).then(|| "exit 1 · boom".into()),
        }
    }

    fn ticked(ok: bool) -> AgentState {
        AgentState {
            ticks: 3,
            cost_usd: 0.31,
            last_tick_at: Some(NOW - 300),
            last_result: "NOCHANGE".into(),
            history: vec![rec(ok, if ok { "NOCHANGE" } else { "ran out of budget" })],
            ..Default::default()
        }
    }

    fn render_row_to_string(agent: &AgentSnapshot, w: u16) -> String {
        let backend = TestBackend::new(w, 1);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let cols = plan_columns(w as usize);
        terminal
            .draw(|f| render_row(f, f.area(), agent, &cols, false, NOW))
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    // `App::new()` loads from `$HOME`, which only unix tests can redirect.
    #[cfg(unix)]
    fn render_detail(agent: AgentSnapshot, detail: Detail, w: u16, h: u16) -> String {
        let mut out = String::new();
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            app.harness.agents = vec![agent];
            app.harness.detail = Some(detail);
            app.view = View::AgentDetail;
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| render_agent_detail_at(f, f.area(), &mut app, NOW))
                .expect("render");
            out = buffer_to_string(terminal.backend().buffer());
        });
        out
    }

    #[test]
    fn row_shows_last_run_note_trigger_and_queue() {
        let out = render_row_to_string(&snap("bb-prs", ticked(true)), 120);
        assert!(out.contains("bb-prs"), "{out}");
        assert!(out.contains("✓ 5m"), "{out}");
        assert!(out.contains("PR #418 lint fails"), "{out}");
        assert!(out.contains("poll 5m"), "{out}");
        assert!(out.contains("+2"), "{out}");
    }

    #[test]
    fn a_failed_last_run_says_why_in_the_row() {
        let out = render_row_to_string(&snap("bb-prs", ticked(false)), 120);
        assert!(out.contains("✗ 5m"), "{out}");
        assert!(
            out.contains("error_max_budget_usd: ran out of budget"),
            "{out}"
        );
    }

    #[test]
    fn halted_row_leads_with_the_reason() {
        let mut st = ticked(true);
        st.stopped_reason = Some("5 consecutive failed ticks".into());
        let out = render_row_to_string(&snap("jira", st), 100);
        assert!(out.contains("5 consecutive failed ticks"), "{out}");
    }

    #[test]
    fn broken_spec_is_visible_not_hidden() {
        let mut s = snap("broken", AgentState::default());
        s.spec = Err("agent.toml: expected `=`".into());
        let out = render_row_to_string(&s, 100);
        assert!(out.contains("agent.toml: expected"), "{out}");
    }

    #[test]
    fn narrow_rows_drop_columns_and_do_not_panic() {
        let wide = plan_columns(120);
        assert!(wide.trigger && wide.cost);
        let narrow = plan_columns(50);
        assert!(!narrow.trigger && !narrow.cost);
        for w in [12, 40, 60] {
            let _ = render_row_to_string(&snap("x", ticked(true)), w);
        }
    }

    #[cfg(unix)]
    #[test]
    fn detail_leads_with_the_failure_and_its_reason() {
        let out = render_detail(snap("bb-prs", ticked(false)), Detail::default(), 120, 36);
        assert!(out.contains("Last run failed 5m ago"), "{out}");
        assert!(out.contains("ran out of budget"), "{out}");
        assert!(out.contains("Runs 1"), "{out}");
        // The run row shows the readable event label; the pane keeps the
        // inbox file name, to find it under inbox/failed/.
        assert!(out.contains("5m ago   poke"), "{out}");
        assert!(out.contains("event 20260903T191749.728Z-poke"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn detail_says_off_and_how_to_turn_it_on() {
        let mut agent = snap("bb-prs", ticked(true));
        if let Ok(spec) = agent.spec.as_mut() {
            spec.enabled = false;
        }
        let out = render_detail(agent, Detail::default(), 120, 36);
        assert!(out.contains("Off — space turns it on"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn every_section_renders_at_any_size() {
        let mut st = ticked(false);
        st.ticking = Some(Ticking {
            since: NOW - 30,
            event: Some("poll-abc".into()),
            session_id: None,
        });
        for section in Section::ALL {
            for (w, h) in [(120, 40), (60, 16), (24, 6), (10, 3)] {
                let detail = Detail {
                    section,
                    run: 5,
                    artifact: 5,
                    log_scroll: 5,
                    setting: 99,
                    editing: None,
                };
                let _ = render_detail(snap("x", st.clone()), detail, w, h);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn settings_show_values_and_the_edit_box() {
        let detail = Detail {
            section: Section::Settings,
            setting: 1,
            editing: Some("opus".into()),
            ..Default::default()
        };
        let out = render_detail(snap("bb-prs", ticked(true)), detail, 120, 40);
        assert!(out.contains("enabled"), "{out}");
        assert!(out.contains("opus▏"), "{out}");
        assert!(out.contains("enter saves"), "{out}");
        assert!(out.contains("budget / day"), "{out}");
        assert!(out.contains("$5.00"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn log_and_artifacts_show_their_lines() {
        let log = Detail {
            section: Section::Log,
            ..Default::default()
        };
        let out = render_detail(snap("a", ticked(true)), log, 120, 36);
        assert!(out.contains("token expired"), "{out}");
        let art = Detail {
            section: Section::Artifacts,
            ..Default::default()
        };
        let out = render_detail(snap("a", ticked(true)), art, 120, 36);
        assert!(out.contains("https://example.com/pr/418"), "{out}");
    }

    #[test]
    fn event_labels_are_readable() {
        assert_eq!(event_label(Some("20260903T191749.728Z-poke")), "poke");
        assert_eq!(event_label(Some("poll-1f2e3d")), "poll");
        assert_eq!(event_label(Some("tick-4")), "timer");
        assert_eq!(event_label(Some("once")), "once");
        assert_eq!(event_label(None), "manual");
    }

    #[test]
    fn ages_are_compact() {
        assert_eq!(age(NOW, NOW - 5), "5s");
        assert_eq!(age(NOW, NOW - 600), "10m");
        assert_eq!(age(NOW, NOW - 7200), "2h");
        assert_eq!(age(NOW, NOW - 200_000), "2d");
        assert_eq!(age(NOW, NOW + 50), "0s");
    }
}
