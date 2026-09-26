use super::sections::{render_artifacts, render_log, render_runs, render_settings};
use super::status_indicator;
use super::text::{event_label, wrap_text};
use super::SELECTED_BG;
use crate::app::{App, Section};
use crate::harness::{AgentSnapshot, AgentStatus};
use crate::models::first_line_truncated;
use crate::ui::common::{age, centered_rect, fmt_cost, popup_block, truncate_line};
use crate::ui::palette::{DIM_TEXT, FAINT_TEXT, LABEL_GRAY, SEP_GRAY};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

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

/// Split a section body into the list and the pane that spells out the
/// selected item, right under the list's last row. A long list scrolls and
/// leaves the pane at least a third of the height.
pub(super) fn split_body(body: Rect, rows: usize) -> (Rect, Rect) {
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
pub(super) fn render_rows(
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
pub(super) fn render_pane(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
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

pub(super) fn render_empty(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}", text),
            Style::default().fg(DIM_TEXT),
        )))
        .wrap(Wrap { trim: false }),
        area,
    );
}

// `App::new()` loads from `$HOME`, which only unix tests can redirect.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::{Detail, View};
    use crate::harness::Ticking;
    use crate::ui::agents::fixtures::{snap, ticked, NOW};
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

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

    #[test]
    fn detail_says_off_and_how_to_turn_it_on() {
        let mut agent = snap("bb-prs", ticked(true));
        if let Ok(spec) = agent.spec.as_mut() {
            spec.enabled = false;
        }
        let out = render_detail(agent, Detail::default(), 120, 36);
        assert!(out.contains("Off — space turns it on"), "{out}");
    }

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
}
