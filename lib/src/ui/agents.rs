//! Agents tab: a row per persistent agent, and the detail popup with the
//! tick timeline and notes. Follows the Sessions list grammar: the status
//! icon says the state, the name reads as a name, and the columns to the
//! right line up so a glance down the tab compares agents, not layouts.

use crate::app::{App, View};
use crate::harness::{AgentSnapshot, AgentStatus, TickRecord};
use crate::models::first_line_truncated;
use crate::ui::common::{centered_rect, fmt_cost, format_tokens, popup_block, Cell, COL_SEP};
use crate::ui::palette::{DIM_TEXT, FAINT_TEXT, LABEL_GRAY, MUTED_TEXT, PURPLE, SEP_GRAY};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

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

// ---- table ----------------------------------------------------------------
//
// One row per agent, in the Sessions list's grammar: a flexible left region
// (name, then the agent's latest word) and a right cluster of fixed-width
// columns, so values line up vertically. Columns that don't fit the
// terminal are dropped for every row at once, lowest-value first, to keep
// that alignment.

/// Agent name; longer ones truncate.
const NAME_W: usize = 18;
/// "poll 30s", "every 5m", "inbox".
const TRIGGER_W: usize = 12;
/// Queued inbox events: "+12".
const QUEUE_W: usize = 4;
/// Last tick: "#128 ✓".
const TICK_W: usize = 8;
/// Context at the end of that tick: "󰍛 128k".
const CTX_W: usize = 8;
/// Spent today: "$12.34".
const COST_W: usize = 7;
/// Since the last tick, or since this one started: "󰔟 12m".
const AGE_W: usize = 7;
/// Selection marker(1) + status icon(1) + space(1).
const LEFT_FIXED: usize = 3;
/// The agent's latest word never shrinks below this; columns drop first.
const MIN_SAY: usize = 20;

/// Which optional columns fit the current terminal width. Decided once per
/// frame so every row shows the same ones. The queue column also needs at
/// least one agent with events waiting — a column of blanks earns nothing.
struct Columns {
    trigger: bool,
    queue: bool,
    tick: bool,
    ctx: bool,
}

fn plan_columns(width: usize, any_queued: bool) -> Columns {
    let mut avail =
        width.saturating_sub(LEFT_FIXED + NAME_W + MIN_SAY + COST_W + AGE_W + 3 * COL_SEP);
    let mut cols = Columns {
        trigger: false,
        queue: false,
        tick: false,
        ctx: false,
    };
    if avail >= TRIGGER_W + COL_SEP {
        cols.trigger = true;
        avail -= TRIGGER_W + COL_SEP;
    }
    if avail >= TICK_W + COL_SEP {
        cols.tick = true;
        avail -= TICK_W + COL_SEP;
    }
    if any_queued && avail >= QUEUE_W + COL_SEP {
        cols.queue = true;
        avail -= QUEUE_W + COL_SEP;
    }
    if avail >= CTX_W + COL_SEP {
        cols.ctx = true;
    }
    cols
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

    // Keep the selected row on screen.
    let selected = app.harness.selected as u16;
    if selected < app.render.agents_scroll {
        app.render.agents_scroll = selected;
    } else if selected >= app.render.agents_scroll + area.height {
        app.render.agents_scroll = selected + 1 - area.height;
    }

    let now = crate::harness::now_unix();
    let any_queued = app.harness.agents.iter().any(|a| a.inbox_pending > 0);
    let cols = plan_columns(area.width as usize, any_queued);
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

/// What the agent has to say for itself, most useful first: why it halted,
/// what it last reported, what its last tick returned, what it is for.
fn say(agent: &AgentSnapshot) -> (String, Style) {
    if let Err(e) = &agent.spec {
        return (format!("spec: {}", e), Style::default().fg(Color::Red));
    }
    if let Some(reason) = &agent.state.stopped_reason {
        return (reason.clone(), Style::default().fg(Color::Yellow));
    }
    if let Some(note) = agent.notes.first() {
        let color = if note.level == "warn" {
            Color::Yellow
        } else {
            FAINT_TEXT
        };
        return (note.text.clone(), Style::default().fg(color));
    }
    if !agent.state.last_result.is_empty() {
        return (
            agent.state.last_result.clone(),
            Style::default().fg(FAINT_TEXT),
        );
    }
    (
        agent.description().to_string(),
        Style::default().fg(DIM_TEXT),
    )
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
    let (glyph, color) = status_indicator(status);
    let glyph = if status == AgentStatus::Ticking {
        crate::ui::sessions::spinner_frame(crate::ui::now_ms())
    } else {
        glyph
    };

    let mut cluster: Vec<Cell> = Vec::new();
    if cols.trigger {
        let text = match &agent.spec {
            Ok(spec) => spec.trigger_label().to_string(),
            Err(_) => String::new(),
        };
        cluster.push(Cell {
            text: first_line_truncated(&text, TRIGGER_W),
            target: TRIGGER_W,
            style: Style::default().fg(MUTED_TEXT),
            right_align: false,
        });
    }
    if cols.queue {
        let text = if agent.inbox_pending > 0 {
            format!("+{}", agent.inbox_pending)
        } else {
            String::new()
        };
        cluster.push(Cell {
            text,
            target: QUEUE_W,
            style: Style::default().fg(Color::Yellow),
            right_align: true,
        });
    }
    if cols.tick {
        let last = agent.state.history.last();
        let (text, style) = match last {
            Some(rec) => (
                format!("#{} {}", agent.state.ticks, if rec.ok { "✓" } else { "✗" }),
                Style::default().fg(if rec.ok { Color::Green } else { Color::Red }),
            ),
            None => (String::new(), Style::default()),
        };
        cluster.push(Cell {
            text,
            target: TICK_W,
            style,
            right_align: true,
        });
    }
    if cols.ctx {
        let text = match agent.state.history.last().map(|r| r.context_end) {
            Some(ctx) if ctx > 0 => format!("󰍛 {}", format_tokens(ctx)),
            _ => String::new(),
        };
        cluster.push(Cell {
            text,
            target: CTX_W,
            style: Style::default().fg(Color::DarkGray),
            right_align: true,
        });
    }
    {
        let spent = agent.state.today_cost();
        let text = if spent > 0.0 {
            fmt_cost(spent)
        } else {
            String::new()
        };
        cluster.push(Cell {
            text,
            target: COST_W,
            style: Style::default().fg(DIM_TEXT),
            right_align: true,
        });
    }
    {
        let at = match &agent.state.ticking {
            Some(t) => Some(t.since),
            None => agent.state.last_tick_at,
        };
        let text = match at {
            Some(at) => format!("󰔟 {}", age(now, at)),
            None => String::new(),
        };
        cluster.push(Cell {
            text,
            target: AGE_W,
            style: Style::default().fg(Color::DarkGray),
            right_align: true,
        });
    }
    let cluster_width: usize = cluster.iter().map(|c| c.target + COL_SEP).sum();

    let name_style = if status.needs_attention() {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else if status == AgentStatus::Disabled {
        Style::default().fg(SEP_GRAY)
    } else {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    };
    let name = first_line_truncated(&agent.name, NAME_W);
    let (say_text, say_style) = say(agent);
    let say_budget = width.saturating_sub(LEFT_FIXED + NAME_W + COL_SEP + cluster_width);
    let say_text = first_line_truncated(&say_text.replace('\n', " "), say_budget);

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(if selected {
        Span::styled("▌", Style::default().fg(Color::White))
    } else {
        Span::raw(" ")
    });
    spans.push(Span::styled(
        format!("{} ", glyph),
        Style::default().fg(color),
    ));
    let name_pad = NAME_W.saturating_sub(name.chars().count()) + COL_SEP;
    spans.push(Span::styled(name, name_style));
    spans.push(Span::raw(" ".repeat(name_pad)));
    let say_pad = say_budget.saturating_sub(say_text.chars().count());
    spans.push(Span::styled(say_text, say_style));
    spans.push(Span::raw(" ".repeat(say_pad)));

    for cell in cluster {
        cell.push_spans(&mut spans);
    }

    let mut row = Paragraph::new(Line::from(spans));
    if selected {
        row = row.style(Style::default().bg(Color::Rgb(40, 40, 52)));
    }
    frame.render_widget(row, area);
}

/// One row only: cut the spans so the line never wraps.
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

// ---- detail popup ---------------------------------------------------------

pub(crate) fn render_agent_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(agent) = app.harness.selected() else {
        return;
    };
    let now = crate::harness::now_unix();
    let popup = centered_rect(area, 0.9);
    frame.render_widget(Clear, popup);

    let status = agent.status();
    let (glyph, color) = status_indicator(status);
    let mut title = vec![Span::styled(
        format!(" {} {} ", glyph, agent.name),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )];
    if let Ok(spec) = &agent.spec {
        title.push(Span::styled(
            format!(
                "{} · {} · {} window · {} ",
                spec.trigger_label(),
                spec.run.model.as_deref().unwrap_or("default model"),
                format_tokens(spec.approx_window_tokens()),
                if spec.run.persistent_session {
                    "persistent"
                } else {
                    "fresh session"
                }
            ),
            Style::default().fg(DIM_TEXT),
        ));
    }
    let block = popup_block(Line::from(title));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(inner);

    render_ticks(frame, cols[0], agent, now, app.render.agent_detail_scroll);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(9)])
        .split(cols[1]);
    render_notes(frame, right[0], agent, now);
    render_spec_summary(frame, right[1], agent);
}

fn pane<'a>(title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SEP_GRAY))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default().fg(LABEL_GRAY),
        ))
}

fn tick_line(rec: &TickRecord, n: u64, now: i64, width: usize) -> Line<'static> {
    let mark = if rec.ok {
        Span::styled("✓", Style::default().fg(Color::Green))
    } else {
        Span::styled("✗", Style::default().fg(Color::Red))
    };
    let ev = rec.event.clone().unwrap_or_else(|| "—".into());
    let outcome = if rec.ok {
        format!("{} turns · {}", rec.turns, fmt_cost(rec.cost_usd))
    } else {
        format!(
            "{} · {}",
            rec.subtype.clone().unwrap_or_default(),
            fmt_cost(rec.cost_usd)
        )
    };
    let line = Line::from(vec![
        mark,
        Span::styled(
            format!(" {:>4} ", age(now, rec.at)),
            Style::default().fg(DIM_TEXT),
        ),
        Span::styled(format!("#{:<4}", n), Style::default().fg(LABEL_GRAY)),
        Span::styled(ev, Style::default().fg(FAINT_TEXT)),
        Span::styled(format!("  {}", outcome), Style::default().fg(DIM_TEXT)),
    ]);
    truncate_line(line, width)
}

fn render_ticks(frame: &mut Frame, area: Rect, agent: &AgentSnapshot, now: i64, scroll: u16) {
    let title = format!("ticks · {}", agent.state.ticks);
    let block = pane(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    if let Some(t) = &agent.state.ticking {
        lines.push(truncate_line(
            Line::from(vec![
                Span::styled("󰒓", Style::default().fg(Color::Green)),
                Span::styled(
                    format!(" {:>4} ", age(now, t.since)),
                    Style::default().fg(DIM_TEXT),
                ),
                Span::styled(
                    format!("#{:<4}", agent.state.ticks + 1),
                    Style::default().fg(LABEL_GRAY),
                ),
                Span::styled(
                    t.event.clone().unwrap_or_else(|| "—".into()),
                    Style::default().fg(FAINT_TEXT),
                ),
                Span::styled("  running", Style::default().fg(Color::Green)),
            ]),
            width,
        ));
    }
    let total = agent.state.history.len() as u64;
    let base = agent.state.ticks.saturating_sub(total);
    for (i, rec) in agent.state.history.iter().enumerate().rev() {
        lines.push(tick_line(rec, base + i as u64 + 1, now, width));
        if !rec.result.is_empty() {
            lines.push(truncate_line(
                Line::from(Span::styled(
                    format!("        {}", rec.result),
                    Style::default().fg(DIM_TEXT),
                )),
                width,
            ));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no ticks yet — press p to poke",
            Style::default().fg(DIM_TEXT),
        )));
    }
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

fn render_notes(frame: &mut Frame, area: Rect, agent: &AgentSnapshot, now: i64) {
    let title = format!("notes · {}", agent.notes.len());
    let block = pane(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines: Vec<Line> = Vec::new();
    for n in &agent.notes {
        let lvl = if n.level == "warn" {
            Span::styled("● ", Style::default().fg(Color::Yellow))
        } else {
            Span::styled("· ", Style::default().fg(DIM_TEXT))
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:>4} ", age(now, n.at)),
                Style::default().fg(DIM_TEXT),
            ),
            lvl,
            Span::styled(n.text.clone(), Style::default().fg(FAINT_TEXT)),
        ]));
        if let Some(r) = &n.r#ref {
            lines.push(Line::from(Span::styled(
                format!("       {}", r),
                Style::default().fg(DIM_TEXT),
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "nothing reported — agents write here with `cc-hub agent note`",
            Style::default().fg(DIM_TEXT),
        )));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_spec_summary(frame: &mut Frame, area: Rect, agent: &AgentSnapshot) {
    let block = pane("spec");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let label = Style::default().fg(LABEL_GRAY);
    let text = Style::default().fg(FAINT_TEXT);
    let mut lines: Vec<Line> = Vec::new();
    match &agent.spec {
        Ok(spec) => {
            lines.push(Line::from(vec![
                Span::styled("workdir  ", label),
                Span::styled(spec.workdir.display().to_string(), text),
            ]));
            lines.push(Line::from(vec![
                Span::styled("tools    ", label),
                Span::styled(spec.run.tools.join(" "), text),
            ]));
            let mut budget = format!("{} / tick", fmt_cost(spec.run.max_budget_usd));
            if let Some(d) = spec.run.daily_budget_usd {
                budget.push_str(&format!(" · ${:.2} / day", d));
            }
            if let Some(t) = spec.run.budget_usd_total {
                budget.push_str(&format!(" · ${:.2} lifetime", t));
            }
            lines.push(Line::from(vec![
                Span::styled("budget   ", label),
                Span::styled(budget, text),
            ]));
            lines.push(Line::from(vec![
                Span::styled("spent    ", label),
                Span::styled(
                    format!(
                        "{} today · {} lifetime · {} compactions",
                        fmt_cost(agent.state.today_cost()),
                        fmt_cost(agent.state.cost_usd),
                        agent.state.compactions
                    ),
                    text,
                ),
            ]));
            if !spec.description.is_empty() {
                lines.push(Line::from(Span::styled(
                    spec.description.clone(),
                    Style::default().fg(DIM_TEXT),
                )));
            }
        }
        Err(e) => lines.push(Line::from(Span::styled(
            e.clone(),
            Style::default().fg(Color::Red),
        ))),
    }
    lines.push(Line::from(Span::styled(
        agent.dir.display().to_string(),
        Style::default().fg(DIM_TEXT),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Key hints for the Agents tab and its popup.
pub(crate) fn hints(view: &View) -> &'static str {
    match view {
        View::AgentDetail => "j/k:scroll  f:transcript  p:poke  space:pause/resume  R:reset  esc/enter:close",
        _ => "enter:detail  f:transcript  p:poke  space:pause/resume  R:reset  j/k:nav  r:refresh  tab:next  q:quit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{AgentState, Note, TickRecord};
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
                r#ref: None,
                tick: 3,
            }],
            inbox_pending: 2,
        }
    }

    fn ticked() -> AgentState {
        AgentState {
            ticks: 3,
            cost_usd: 0.31,
            last_tick_at: Some(NOW - 300),
            last_result: "NOCHANGE".into(),
            history: vec![TickRecord {
                at: NOW - 300,
                event: Some("poll-abc".into()),
                ok: true,
                subtype: Some("success".into()),
                turns: 4,
                compactions: 0,
                cost_usd: 0.07,
                context_start: 4000,
                context_end: 13_000,
                duration_s: 12,
                session_id: Some("sid-1".into()),
                result: "NOCHANGE".into(),
            }],
            ..Default::default()
        }
    }

    fn render_row_to_string(agent: &AgentSnapshot, selected: bool, w: u16) -> String {
        let backend = TestBackend::new(w, 1);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let cols = plan_columns(w as usize, agent.inbox_pending > 0);
        terminal
            .draw(|f| render_row(f, f.area(), agent, &cols, selected, NOW))
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn row_shows_name_note_trigger_tick_and_spend() {
        let out = render_row_to_string(&snap("bb-prs", ticked()), false, 120);
        assert!(out.contains("bb-prs"), "{out}");
        assert!(out.contains("poll 5m"), "{out}");
        assert!(out.contains("+2"), "{out}");
        assert!(out.contains("PR #418 lint fails"), "{out}");
        assert!(out.contains("#3 ✓"), "{out}");
        assert!(out.contains("13.0k"), "{out}");
    }

    #[test]
    fn halted_row_leads_with_the_reason() {
        let mut st = ticked();
        st.stopped_reason = Some("5 consecutive failed ticks".into());
        let out = render_row_to_string(&snap("jira", st), false, 100);
        assert!(out.contains("5 consecutive failed ticks"), "{out}");
    }

    #[test]
    fn broken_spec_is_visible_not_hidden() {
        let mut s = snap("broken", AgentState::default());
        s.spec = Err("agent.toml: expected `=`".into());
        let out = render_row_to_string(&s, true, 100);
        assert!(out.contains("spec: agent.toml"), "{out}");
    }

    #[test]
    fn narrow_rows_drop_columns_and_do_not_panic() {
        // Everything fits at 120 columns; at 40 only the essentials remain.
        let wide = plan_columns(120, true);
        assert!(wide.trigger && wide.tick && wide.queue && wide.ctx);
        let narrow = plan_columns(40, true);
        assert!(!narrow.ctx && !narrow.queue);
        let _ = render_row_to_string(&snap("x", ticked()), false, 40);
        let _ = render_row_to_string(&snap("x", ticked()), false, 12);
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
