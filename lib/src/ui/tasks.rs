//! Tasks-tab body: a three-column board (To-Do · In Progress · Done) over
//! the personal task store. Visually a sibling of the Projects kanban, but
//! each card is a flat task, optionally annotated with its bound agent
//! session's live state (resolved by tmux name, same as project cards).

use crate::app::{App, TASK_COLUMNS};
use crate::models::{self, SessionInfo, SessionState};
use crate::tasks::TaskItem;
use crate::ui::now_ms;
use crate::ui::palette::{ACCENT_BLUE, BACKLOG_BLUE, DIM_TEXT, DOT_IDLE, LABEL_GRAY, META_GRAY};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;
use std::collections::HashMap;

pub fn render_tasks_body(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 3 || area.width < 30 {
        let hint = Paragraph::new("(terminal too narrow — resize or switch to Sessions)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(DOT_IDLE))
            .wrap(Wrap { trim: false });
        frame.render_widget(hint, area);
        return;
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
        ])
        .split(area);

    let sessions_by_tmux = app.sessions_by_tmux();
    let now_secs = now_ms() / 1000;
    for col_idx in 0..TASK_COLUMNS.len() {
        render_task_column(frame, cols[col_idx], app, col_idx, &sessions_by_tmux, now_secs);
    }
}

fn column_meta(col: usize) -> (&'static str, &'static str, Color) {
    match col {
        0 => ("To-Do", "󰄱", BACKLOG_BLUE),
        1 => ("In Progress", "󰒓", Color::LightYellow),
        _ => ("Done", "󰸞", Color::LightGreen),
    }
}

fn render_task_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    col_idx: usize,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) {
    let (label, icon, accent) = column_meta(col_idx);
    let tasks = app.tasks.board.column(TASK_COLUMNS[col_idx]);
    let count = tasks.len();
    let col_focused = app.tasks.col == col_idx;
    // To-Do and In Progress carry a meta row under two text rows; Done cards
    // are compact (the column already says everything but the when).
    let card_height: u16 = if col_idx == 2 { 4 } else { 5 };
    let gap: u16 = 1;
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let max_cards =
        ((inner.height as u32 + gap as u32) / (card_height as u32 + gap as u32)) as usize;
    let sel = if count == 0 {
        0
    } else if col_focused {
        app.tasks.row.min(count - 1)
    } else {
        0
    };
    let scroll_top = if count == 0 || max_cards == 0 || sel < max_cards {
        0
    } else {
        sel + 1 - max_cards
    };
    let hidden_below = count.saturating_sub(scroll_top.saturating_add(max_cards));

    let (border_type, border_style, title_style) = if col_focused {
        (
            BorderType::Double,
            Style::default().fg(accent),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            BorderType::Rounded,
            Style::default().fg(Color::Rgb(60, 60, 80)),
            Style::default().fg(LABEL_GRAY),
        )
    };
    let mut title_spans = vec![
        Span::raw(" "),
        Span::styled(format!("{} ", icon), Style::default().fg(accent)),
        Span::styled(label.to_string(), title_style),
        Span::styled(
            format!(" ({}) ", count),
            Style::default().fg(Color::Rgb(140, 140, 165)),
        ),
    ];
    if scroll_top > 0 || hidden_below > 0 {
        let last_visible = scroll_top.saturating_add(max_cards).min(count);
        title_spans.push(Span::styled(
            format!(" · {}-{} ", scroll_top + 1, last_visible),
            Style::default().fg(DIM_TEXT),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .title(Line::from(title_spans));
    frame.render_widget(block, area);

    if tasks.is_empty() {
        let empty_hint = match col_idx {
            0 => "No tasks — press a to add one",
            1 => "Nothing assigned — s hands a task to an agent",
            _ => "Nothing done yet",
        };
        let hint = Paragraph::new(Line::from(Span::styled(
            empty_hint,
            Style::default().fg(Color::Rgb(70, 70, 90)),
        )))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });
        frame.render_widget(hint, inner);
        return;
    }

    let mut y = inner.y;
    for (rel, t) in tasks.iter().enumerate().skip(scroll_top).take(max_cards) {
        let card_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: card_height,
        };
        let selected = col_focused && rel == sel;
        render_task_card(
            frame,
            card_area,
            t,
            selected,
            col_idx,
            accent,
            sessions_by_tmux,
            now_secs,
        );
        y = y.saturating_add(card_height + gap);
        if y >= inner.y + inner.height {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_task_card(
    frame: &mut Frame,
    area: Rect,
    t: &TaskItem,
    selected: bool,
    col_idx: usize,
    accent: Color,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) {
    let (border_type, border_style) = if selected {
        (BorderType::Double, Style::default().fg(accent))
    } else {
        (
            BorderType::Rounded,
            Style::default().fg(Color::Rgb(60, 60, 80)),
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let done = col_idx == 2;
    let text_style = if done {
        Style::default().fg(DIM_TEXT).add_modifier(Modifier::ITALIC)
    } else if selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(200, 200, 210))
    };

    let text_rows = (inner.height as usize).saturating_sub(1).max(1);
    let mut lines: Vec<Line> = wrap_text(&t.text, inner.width as usize)
        .into_iter()
        .take(text_rows)
        .map(|seg| Line::from(Span::styled(seg, text_style)))
        .collect();

    // Pin the meta row to the card's bottom edge so short and wrapped texts
    // produce the same silhouette.
    while lines.len() < text_rows {
        lines.push(Line::raw(""));
    }
    lines.push(meta_line(t, col_idx, sessions_by_tmux, now_secs));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row of facts the column/border can't already say: how old the task
/// is, and — once an agent is bound — who runs it, where, and what state the
/// session is in right now.
fn meta_line(
    t: &TaskItem,
    col_idx: usize,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) -> Line<'static> {
    let age_style = Style::default().fg(META_GRAY);
    if col_idx == 2 {
        let when = t.done_at.unwrap_or(t.created_at);
        let mut spans = vec![Span::styled(
            format!("✓ {}", models::relative_age(now_secs.saturating_sub(when))),
            age_style,
        )];
        // A done task that ran through an agent keeps its transcript — `f`
        // still opens/resumes it — so say who ran it and where. A still-live
        // session gets a brighter dot: marking done doesn't close the agent.
        if t.tmux.is_some() || t.session_id.is_some() {
            let agent = t.agent_id.as_deref().unwrap_or("agent");
            spans.push(Span::styled(format!("  󰚩 {}", agent), age_style));
            if let Some(dir) = t.cwd.as_deref().map(dir_basename) {
                spans.push(Span::styled(format!(" · {}", dir), age_style));
            }
            if t.tmux
                .as_deref()
                .is_some_and(|n| sessions_by_tmux.contains_key(n))
            {
                spans.push(Span::styled(
                    "  ● live",
                    Style::default().fg(Color::LightGreen),
                ));
            }
        }
        return Line::from(spans);
    }

    let mut spans: Vec<Span> = Vec::new();
    if let Some(tmux) = t.tmux.as_deref() {
        let (glyph, label, color) = match sessions_by_tmux.get(tmux) {
            Some(s) => match s.state {
                SessionState::Processing => ("⟳", "working", Color::LightYellow),
                SessionState::WaitingForInput | SessionState::Question => {
                    ("󰂞", "needs input", Color::Yellow)
                }
                SessionState::Idle => ("●", "idle", Color::LightGreen),
                SessionState::Inactive => ("○", "inactive", DOT_IDLE),
            },
            // Spawned but not scanned yet, or the tmux died. With a resolved
            // session id `f` resumes; before resolution it can only hint.
            None if t.session_id.is_some() => ("○", "gone — f resumes", DOT_IDLE),
            None => ("○", "starting…", DOT_IDLE),
        };
        spans.push(Span::styled(
            format!("{} {}", glyph, label),
            Style::default().fg(color),
        ));
        if let Some(dir) = t.cwd.as_deref().map(dir_basename) {
            spans.push(Span::styled("  ", age_style));
            spans.push(Span::styled(dir, Style::default().fg(ACCENT_BLUE)));
        }
        spans.push(Span::styled("  ", age_style));
    }
    spans.push(Span::styled(
        models::relative_age(now_secs.saturating_sub(t.created_at)),
        age_style,
    ));
    Line::from(spans)
}

fn dir_basename(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string())
}

/// Greedy word wrap to `width` columns (char-counted). Local copy of the
/// to-do panel's helper — popups' is private and this one doesn't need the
/// continuation-indent variant.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_len = 0usize;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if line_len == 0 {
            line = word.to_string();
            line_len = wlen;
        } else if line_len + 1 + wlen <= width {
            line.push(' ');
            line.push_str(word);
            line_len += 1 + wlen;
        } else {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
            line_len = wlen;
        }
        // A single word longer than the width gets hard-truncated rather
        // than overflowing the card.
        if line_len > width {
            line = models::first_line_truncated(&line, width);
            line_len = width;
        }
    }
    out.push(line);
    out
}
