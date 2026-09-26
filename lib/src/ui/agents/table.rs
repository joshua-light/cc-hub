//! One row per agent: status icon, name, last run, what the agent has to
//! say, and a right cluster (trigger, spend today) that drops at narrow
//! widths for every row at once so the columns stay aligned.

use super::status_indicator;
use super::text::one_line;
use crate::app::App;
use crate::harness::{AgentSnapshot, AgentStatus};
use crate::models::first_line_truncated;
use crate::ui::common::{
    age, fmt_cost, pad, selection_stripe, spinner_frame, truncate_line, Cell, COL_SEP,
};
use crate::ui::palette::{DIM_TEXT, FAINT_TEXT, LABEL_GRAY, MUTED_TEXT, SELECTED_ROW_BG};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

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
        spinner_frame(crate::ui::now_ms())
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
        selection_stripe(selected),
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
        row = row.style(Style::default().bg(SELECTED_ROW_BG));
    }
    frame.render_widget(row, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::AgentState;
    use crate::ui::agents::fixtures::{snap, ticked, NOW};
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render_row_to_string(agent: &AgentSnapshot, w: u16) -> String {
        let backend = TestBackend::new(w, 1);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let cols = plan_columns(w as usize);
        terminal
            .draw(|f| render_row(f, f.area(), agent, &cols, false, NOW))
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
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
}
