//! Tasks-tab body: a kanban board (To-Do · Planning · In Progress · Done)
//! over the personal task store. Visually a sibling of the Projects kanban,
//! but each card is a flat task, optionally annotated with its bound agent
//! session's live state (resolved by tmux name, same as project cards).
//! Planning holds cards whose agent is drafting a plan; Space approves it and
//! the card moves to In Progress. The Planning column is optional
//! (`ui.show_planning_column`); when hidden its cards fold into In Progress.

use crate::app::{visible_task_columns, App};
use crate::models::{self, SessionInfo, SessionState};
use crate::tasks::{TaskItem, TaskItemStatus, TaskPriority};
use crate::ui::now_ms;
use crate::ui::palette::{
    ACCENT_BLUE, BACKLOG_BLUE, DIM_TEXT, DOT_IDLE, LABEL_GRAY, META_GRAY, PURPLE, TAG_SLATE,
};
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
    // Column set is config-driven: Planning is optional, so split the row
    // into equal shares of however many columns are visible.
    let columns = visible_task_columns();
    let n = columns.len() as u32;
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            columns
                .iter()
                .map(|_| Constraint::Ratio(1, n))
                .collect::<Vec<_>>(),
        )
        .split(area);

    let sessions_by_tmux = app.sessions_by_tmux();
    let now_secs = now_ms() / 1000;
    for (col_idx, status) in columns.iter().enumerate() {
        render_task_column(
            frame,
            cols[col_idx],
            app,
            col_idx,
            *status,
            &sessions_by_tmux,
            now_secs,
        );
    }
}

fn column_meta(status: TaskItemStatus) -> (&'static str, &'static str, Color) {
    // Planning borrows the Projects kanban's planning icon/accent so the
    // same phase reads the same across both boards.
    match status {
        TaskItemStatus::Todo => ("To-Do", "󰄱", BACKLOG_BLUE),
        TaskItemStatus::Planning => ("Planning", "󰟶", PURPLE),
        TaskItemStatus::InProgress => ("In Progress", "󰒓", Color::LightYellow),
        TaskItemStatus::Done => ("Done", "󰸞", Color::LightGreen),
    }
}

fn render_task_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    col_idx: usize,
    status: TaskItemStatus,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) {
    let (label, icon, accent) = column_meta(status);
    // Display order, not board order: live columns put needs-input cards
    // first, frozen at tab entry so scan ticks can't reorder cards under
    // the cursor (see `App::task_display_column`); the cursor indexes the
    // same list. With Planning hidden, In Progress also carries its cards.
    let tasks = app.task_display_column(status);
    let count = tasks.len();
    let col_focused = app.tasks.col == col_idx;
    // Live columns carry a meta row under two text rows; Done cards are
    // compact (the column already says everything but the when).
    let card_height: u16 = if status == TaskItemStatus::Done { 4 } else { 5 };
    let gap: u16 = 0;
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
        let empty_hint = match status {
            TaskItemStatus::Todo => "No tasks — press a to add one",
            TaskItemStatus::Planning => "Nothing planning — s hands a task to an agent",
            TaskItemStatus::InProgress => "Nothing running — Space approves a plan",
            TaskItemStatus::Done => "Nothing done yet",
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
            status,
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
    status: TaskItemStatus,
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
    // Priority badge rides the top-right of the border (`P1`–`P4`): black
    // text on a color-filled chip, prominent without stealing a text row. It
    // keeps its own colours even when the selected border turns the accent.
    let priority_badge = Line::from(Span::styled(
        format!(" {} ", t.priority.label()),
        Style::default()
            .fg(Color::Black)
            .bg(priority_color(t.priority))
            .add_modifier(Modifier::BOLD),
    ))
    .alignment(Alignment::Right);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .title(priority_badge);
    // Tags ride the top-left of the border, mirroring the priority chip on the
    // right. Budget the width against what the priority chip leaves (corners +
    // chip + a one-column gap) so the two badges never collide; overflow folds
    // into a `+N` marker.
    let prio_w = t.priority.label().chars().count() + 2;
    let tag_budget = (area.width as usize).saturating_sub(2 + prio_w + 1);
    if let Some(text) = tags_title_text(&t.tags, tag_budget) {
        block = block.title(
            Line::from(Span::styled(text, Style::default().fg(TAG_SLATE)))
                .alignment(Alignment::Left),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let done = status == TaskItemStatus::Done;
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
    lines.push(meta_line(t, status, sessions_by_tmux, now_secs));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row of facts the column/border can't already say: how old the task
/// is, and — once an agent is bound — who runs it, where, and what state the
/// session is in right now.
fn meta_line(
    t: &TaskItem,
    status: TaskItemStatus,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) -> Line<'static> {
    let age_style = Style::default().fg(META_GRAY);
    if status == TaskItemStatus::Done {
        let when = t.done_at.unwrap_or(t.created_at);
        let mut spans = vec![Span::styled(
            format!("✓ {}", models::relative_age(now_secs.saturating_sub(when))),
            age_style,
        )];
        // A done task that ran through an agent keeps its transcript — `f`
        // still opens/resumes it — so say who ran it and where. Marking done
        // closes the live session, but one can outlive that (close failed,
        // board edited by hand), so a survivor still gets the brighter dot.
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
                // An idle planning agent has finished its turn — the plan is
                // sitting in the transcript waiting for a verdict. Keyed off
                // the card's own status so it still reads "plan ready" when a
                // Planning card is folded into a hidden-Planning In Progress
                // column.
                SessionState::Idle if t.status == TaskItemStatus::Planning => {
                    ("●", "plan ready", Color::LightGreen)
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

/// Build the left-border tag badge text (`#a #b`) that fits in `budget`
/// columns. Takes whole tags greedily; if any don't fit, the leftover count
/// folds into a trailing `+N` (reserving room for it, single-digit since the
/// tag set is capped). Returns `None` when there are no tags or no room.
fn tags_title_text(tags: &[String], budget: usize) -> Option<String> {
    if tags.is_empty() || budget < 3 {
        return None;
    }
    // Each piece carries its own leading space so the run insets from the
    // corner like the priority chip's ` P1 `.
    let pieces: Vec<String> = tags.iter().map(|t| format!(" #{}", t)).collect();
    let mut used = 0usize;
    let mut shown = 0usize;
    for (i, piece) in pieces.iter().enumerate() {
        let more_after = i + 1 < pieces.len();
        // Keep room for a ` +N` marker (≤3 cols) whenever tags would remain.
        let reserve = if more_after { 3 } else { 0 };
        if used + piece.chars().count() + reserve > budget {
            break;
        }
        used += piece.chars().count();
        shown += 1;
    }
    let hidden = tags.len() - shown;
    if shown == 0 {
        // Not even one tag fits beside the priority chip — show just the count.
        return Some(format!(" +{}", tags.len()));
    }
    let mut out: String = pieces[..shown].concat();
    if hidden > 0 {
        out.push_str(&format!(" +{}", hidden));
    }
    Some(out)
}

/// Card badge colour per priority: P1 red, P2 yellow, P3 green, P4 blue.
/// Light variants so the bold badge stays legible on the dark board.
fn priority_color(p: TaskPriority) -> Color {
    match p {
        TaskPriority::P1 => Color::LightRed,
        TaskPriority::P2 => Color::LightYellow,
        TaskPriority::P3 => Color::LightGreen,
        TaskPriority::P4 => Color::LightBlue,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::TaskItemStatus;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn card(priority: TaskPriority) -> TaskItem {
        TaskItem {
            id: "tk-1".into(),
            text: "fix the parser".into(),
            status: TaskItemStatus::Todo,
            priority,
            tags: Vec::new(),
            created_at: 1,
            done_at: None,
            cwd: None,
            agent_id: None,
            tmux: None,
            session_id: None,
        }
    }

    /// Render a To-Do card and return its painted buffer.
    fn render(priority: TaskPriority) -> ratatui::buffer::Buffer {
        render_card(&card(priority))
    }

    /// Render an arbitrary card (wide enough for badges) and return its buffer.
    fn render_card(t: &TaskItem) -> ratatui::buffer::Buffer {
        let sessions = HashMap::new();
        let backend = TestBackend::new(28, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                render_task_card(
                    f,
                    f.area(),
                    t,
                    false,
                    TaskItemStatus::Todo,
                    BACKLOG_BLUE,
                    &sessions,
                    1_000,
                );
            })
            .expect("render");
        terminal.backend().buffer().clone()
    }

    #[test]
    fn card_shows_priority_badge() {
        let buf = render(TaskPriority::P2);
        assert!(
            buffer_to_string(&buf).contains("P2"),
            "card should show its priority badge:\n{}",
            buffer_to_string(&buf)
        );
    }

    #[test]
    fn card_shows_tags_alongside_priority() {
        let mut t = card(TaskPriority::P1);
        t.tags = vec!["bug".into(), "api".into()];
        let painted = buffer_to_string(&render_card(&t));
        assert!(
            painted.contains("#bug") && painted.contains("#api"),
            "card should show its tag badges:\n{}",
            painted
        );
        // The priority chip must survive alongside the tags.
        assert!(
            painted.contains("P1"),
            "tags must not displace the priority badge:\n{}",
            painted
        );
    }

    #[test]
    fn tags_title_overflows_to_count_marker() {
        // Plenty of tags but a tiny budget folds the leftovers into `+N`.
        let tags: Vec<String> = vec!["alpha".into(), "bravo".into(), "charlie".into()];
        let text = tags_title_text(&tags, 10).expect("some tags fit");
        assert!(text.contains('+'), "expected overflow marker in {:?}", text);
        // No tags at all, or no room, yields nothing.
        assert_eq!(tags_title_text(&[], 20), None);
        assert_eq!(tags_title_text(&tags, 2), None);
    }

    #[test]
    fn badge_is_color_coded_per_priority() {
        for (p, want) in [
            (TaskPriority::P1, Color::LightRed),
            (TaskPriority::P2, Color::LightYellow),
            (TaskPriority::P3, Color::LightGreen),
            (TaskPriority::P4, Color::LightBlue),
        ] {
            let buf = render(p);
            let area = *buf.area();
            // Find the badge's "P" cell: the hue fills the background, with
            // black text on top.
            let (fg, bg) = (0..area.width)
                .flat_map(|x| (0..area.height).map(move |y| (x, y)))
                .find(|&(x, y)| buf[(x, y)].symbol() == "P")
                .map(|(x, y)| (buf[(x, y)].fg, buf[(x, y)].bg))
                .expect("badge P cell present");
            assert_eq!(bg, want, "{} badge fill", p.label());
            assert_eq!(fg, Color::Black, "{} badge text", p.label());
        }
    }
}
