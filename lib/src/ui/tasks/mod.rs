//! Tasks tab: a kanban board (To-Do · In Progress · Review · Done by
//! default) over the personal task store. Each card is a task, annotated
//! with its bound agent session's live state when it has one (matched by
//! tmux name).
//!
//! Planning holds cards whose agent is drafting a plan; Space approves it and
//! the card moves to In Progress. The column is opt-in
//! (`ui.show_planning_column = true`); hidden, its cards fold into In
//! Progress. Review holds cards whose session wrote a `PR:` note, so a card
//! that wants a reading is never mistaken for one that wants an answer.
//!
//! - `column`: one board column and its scroll window
//! - `card`: one task card and its meta row
//! - `info`: the Task Info popup

use crate::app::{visible_task_columns, App, Targets, View};
use crate::models;
use crate::ui::common::CURSOR;
use crate::ui::now_ms;
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT, DOT_IDLE, LABEL_GRAY};
use column::render_task_column;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

mod card;
mod column;
mod info;

pub(crate) use info::render_task_info;

pub fn render_tasks_body(frame: &mut Frame, area: Rect, app: &mut App) {
    app.render.task_columns.clear();
    app.render.task_cards.clear();
    if area.height < 3 || area.width < 30 {
        let hint = Paragraph::new("(terminal too narrow — resize or switch to Sessions)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(DOT_IDLE))
            .wrap(Wrap { trim: false });
        frame.render_widget(hint, area);
        return;
    }
    // Filter bar: one row above the columns, shown while editing (`/`,
    // live cursor) and as long as a committed filter narrows the board —
    // an invisible filter would read as vanished tasks.
    let mut area = area;
    let editing = app.view == View::TaskFilter;
    if editing || !app.tasks.filter.is_empty() {
        let bar = Rect { height: 1, ..area };
        area = Rect {
            y: area.y + 1,
            height: area.height - 1,
            ..area
        };
        render_filter_bar(frame, bar, app, editing);
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

    let mut cards = Targets::default();
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
            &mut cards,
        );
    }
    drop(sessions_by_tmux);
    for (col_idx, col_area) in cols.iter().enumerate() {
        app.render.task_columns.push(*col_area, col_idx);
    }
    app.render.task_cards = cards;
}

/// The one-row filter strip: query (with a cursor while editing), how many
/// cards survive across the visible columns, and the key hints for the
/// current mode.
fn render_filter_bar(frame: &mut Frame, area: Rect, app: &App, editing: bool) {
    let matches: usize = (0..visible_task_columns().len())
        .map(|c| app.tasks.column_len(c))
        .sum();
    let mut query = app.tasks.filter.clone();
    if editing {
        query.push(CURSOR);
    }
    let hint = if editing {
        "  enter:apply  esc:clear"
    } else {
        "  /:edit  esc:clear"
    };
    let line = Line::from(vec![
        Span::styled(" / ", Style::default().fg(ACCENT_BLUE)),
        Span::styled(
            query,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "  ({} match{})",
                matches,
                if matches == 1 { "" } else { "es" }
            ),
            Style::default().fg(LABEL_GRAY),
        ),
        Span::styled(hint, Style::default().fg(DIM_TEXT)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// `3m ago` for a unix-seconds timestamp; one in the future reads `0s ago`.
pub(super) fn ago(now_secs: u64, at: i64) -> String {
    models::relative_age(now_secs.saturating_sub(at.max(0) as u64))
}

/// Greedy word wrap to `width` columns (char-counted). Line breaks fold into
/// spaces and a word wider than the line is truncated; `ui::agents`' wrap
/// differs on both counts.
pub(super) fn wrap_text(text: &str, width: usize) -> Vec<String> {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    // Every column is a click target, and every card drawn in one is a
    // target for its place in that column.
    #[test]
    fn the_board_records_its_columns_and_cards() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            app.tasks.board.add("first").unwrap();
            app.tasks.board.add("second").unwrap();
            let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("terminal");
            terminal
                .draw(|f| render_tasks_body(f, f.area(), &mut app))
                .expect("render");

            let columns: Vec<_> = (0..100)
                .filter_map(|x| app.render.task_columns.at(x, 10))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            assert_eq!(
                columns,
                (0..visible_task_columns().len()).collect::<Vec<_>>()
            );

            let mut cards: Vec<_> = (0..20)
                .filter_map(|y| app.render.task_cards.at(2, y))
                .collect();
            cards.dedup();
            assert_eq!(cards, vec![(0, 0), (0, 1)]);
        });
    }
}
