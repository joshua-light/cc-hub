//! Agents tab: a card per persistent agent, and the detail popup with the
//! tick timeline and notes. Follows the Sessions card grammar: border says
//! the state, a filled chip title means "needs you", body rows add what the
//! border cannot say.

use crate::app::{App, View};
use crate::harness::{AgentSnapshot, AgentStatus, TickRecord};
use crate::ui::common::{centered_rect, fmt_cost, format_tokens, popup_block};
use crate::ui::palette::{DIM_TEXT, FAINT_TEXT, LABEL_GRAY, MUTED_TEXT, PURPLE, SEP_GRAY};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

const CARD_H: u16 = 7;

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

pub(crate) fn render_agents_body(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.height < 2 || area.width < 10 {
        return;
    }
    let cell_w = crate::config::get().ui.cell_width.max(20);
    let cols = (area.width / cell_w).max(1) as usize;
    app.render.agents_cols = cols;

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

    let now = crate::harness::now_unix();
    let rows = app.harness.agents.len().div_ceil(cols);
    let visible_rows = (area.height / CARD_H).max(1) as usize;
    // Keep the selected row on screen.
    let sel_row = app.harness.selected / cols;
    let first_row = sel_row.saturating_sub(visible_rows.saturating_sub(1));
    let mut y = area.y;
    for row in first_row..rows.min(first_row + visible_rows) {
        let mut x = area.x;
        for col in 0..cols {
            let idx = row * cols + col;
            let Some(agent) = app.harness.agents.get(idx) else {
                break;
            };
            let rect = Rect::new(x, y, cell_w.min(area.right().saturating_sub(x)), CARD_H);
            render_card(frame, rect, agent, idx == app.harness.selected, now);
            x += cell_w;
        }
        y += CARD_H;
    }
}

fn render_card(frame: &mut Frame, area: Rect, agent: &AgentSnapshot, selected: bool, now: i64) {
    let status = agent.status();
    let (glyph, color) = status_indicator(status);
    let attention = status.needs_attention();

    let border_color = if selected {
        Color::White
    } else if attention || status == AgentStatus::Ticking {
        color
    } else {
        SEP_GRAY
    };
    let border_type = if selected {
        BorderType::Double
    } else if attention {
        BorderType::Thick
    } else if status == AgentStatus::Disabled {
        BorderType::LightDoubleDashed
    } else {
        BorderType::Rounded
    };
    let title_text = format!("{} {}", glyph, agent.name);
    let title = if attention {
        Span::styled(
            format!(" {} ", title_text),
            Style::default()
                .fg(Color::Black)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            title_text,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )
    };
    let meta = match status {
        AgentStatus::Ticking => agent
            .state
            .ticking
            .as_ref()
            .map(|t| format!("ticking · {}", age(now, t.since)))
            .unwrap_or_else(|| "ticking".into()),
        _ => match agent.state.last_tick_at {
            Some(at) => format!("{} · {}", status.label(), age(now, at)),
            None => status.label().to_string(),
        },
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(title)
        .title_bottom(
            Line::from(Span::styled(
                format!(" {} ", meta),
                Style::default().fg(DIM_TEXT),
            ))
            .right_aligned(),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let label = Style::default().fg(LABEL_GRAY);
    let text = Style::default().fg(FAINT_TEXT);
    let dim = Style::default().fg(DIM_TEXT);
    let mut lines: Vec<Line> = Vec::new();

    match &agent.spec {
        Ok(spec) => {
            let mut trig = vec![
                Span::styled("trigger ", label),
                Span::styled(spec.trigger_label(), text),
            ];
            if let Some(cmd) = &spec.trigger.command {
                trig.push(Span::styled(format!("  {}", cmd), dim));
            }
            if agent.inbox_pending > 0 {
                trig.push(Span::styled(
                    format!("  +{} queued", agent.inbox_pending),
                    Style::default().fg(Color::Yellow),
                ));
            }
            lines.push(Line::from(trig));
        }
        Err(e) => {
            lines.push(Line::from(Span::styled(
                format!("spec: {}", e),
                Style::default().fg(Color::Red),
            )));
        }
    }

    // What the border can't say: why it halted, or what it last did.
    if let Some(reason) = &agent.state.stopped_reason {
        lines.push(Line::from(vec![
            Span::styled("halted  ", label),
            Span::styled(reason.clone(), Style::default().fg(Color::Yellow)),
        ]));
    } else if let Some(note) = agent.notes.first() {
        lines.push(Line::from(vec![
            Span::styled("note    ", label),
            Span::styled(note.text.clone(), text),
        ]));
    } else if !agent.state.last_result.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("last    ", label),
            Span::styled(agent.state.last_result.clone(), dim),
        ]));
    } else if !agent.description().is_empty() {
        lines.push(Line::from(Span::styled(
            agent.description().to_string(),
            dim,
        )));
    }

    if let Some(last) = agent.state.history.last() {
        let mark = if last.ok {
            Span::styled("✓", Style::default().fg(Color::Green))
        } else {
            Span::styled("✗", Style::default().fg(Color::Red))
        };
        lines.push(Line::from(vec![
            Span::styled("tick    ", label),
            Span::styled(format!("#{} ", agent.state.ticks), text),
            mark,
            Span::styled(
                format!(
                    " {} turns · {} · {}",
                    last.turns,
                    fmt_cost(last.cost_usd),
                    format_tokens(last.context_end)
                ),
                dim,
            ),
        ]));
    }

    let mut today = vec![
        Span::styled("today   ", label),
        Span::styled(
            format!(
                "{} · {} ticks",
                fmt_cost(agent.state.today_cost()),
                agent.state.today_ticks()
            ),
            text,
        ),
    ];
    if let Ok(spec) = &agent.spec {
        if let Some(daily) = spec.run.daily_budget_usd {
            today.push(Span::styled(format!(" / ${:.0}", daily), dim));
        }
    }
    lines.push(Line::from(today));

    let mut out: Vec<Line> = Vec::new();
    for l in lines.into_iter().take(inner.height as usize) {
        out.push(truncate_line(l, inner.width as usize));
    }
    frame.render_widget(Paragraph::new(out), inner);
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
        View::AgentDetail => "j/k:scroll  p:poke  space:pause/resume  R:reset  esc/enter:close",
        _ => "enter:detail  p:poke  space:pause/resume  R:reset  h/j/k/l:nav  r:refresh  tab:next  q:quit",
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
                result: "NOCHANGE".into(),
            }],
            ..Default::default()
        }
    }

    fn render_card_to_string(agent: &AgentSnapshot, selected: bool, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| render_card(f, f.area(), agent, selected, NOW))
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn card_shows_trigger_note_tick_and_spend() {
        let out = render_card_to_string(&snap("bb-prs", ticked()), false, 42, CARD_H);
        assert!(out.contains("bb-prs"), "{out}");
        assert!(out.contains("poll 5m"), "{out}");
        assert!(out.contains("+2 queued"), "{out}");
        assert!(out.contains("PR #418 lint fails"), "{out}");
        assert!(out.contains("#3"), "{out}");
        assert!(out.contains("sleeping"), "{out}");
    }

    #[test]
    fn halted_card_leads_with_the_reason() {
        let mut st = ticked();
        st.stopped_reason = Some("5 consecutive failed ticks".into());
        let out = render_card_to_string(&snap("jira", st), false, 42, CARD_H);
        assert!(out.contains("halted"), "{out}");
        assert!(out.contains("5 consecutive failed ticks"), "{out}");
    }

    #[test]
    fn broken_spec_is_visible_not_hidden() {
        let mut s = snap("broken", AgentState::default());
        s.spec = Err("agent.toml: expected `=`".into());
        let out = render_card_to_string(&s, true, 42, CARD_H);
        assert!(out.contains("spec: agent.toml"), "{out}");
    }

    #[test]
    fn tiny_card_does_not_panic() {
        let _ = render_card_to_string(&snap("x", ticked()), false, 12, 3);
        let _ = render_card_to_string(&snap("x", ticked()), false, 42, 1);
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
