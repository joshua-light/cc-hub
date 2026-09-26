use super::card::render_task_card;
use crate::app::App;
use crate::models::SessionInfo;
use crate::tasks::store::TaskStatus;
use crate::ui::common::task_status_meta;
use crate::ui::palette::{DIM_TEXT, LABEL_GRAY};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;
use std::collections::HashMap;

pub(super) fn render_task_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    col_idx: usize,
    status: TaskStatus,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) {
    // Icon/accent come from the shared status palette so the columns and
    // the task-link picker read the same.
    let (icon, accent) = task_status_meta(status);
    let label = status.board_label();
    // Display order, not board order: live columns put needs-input cards
    // first, frozen at tab entry so scan ticks can't reorder cards under
    // the cursor (see `App::task_display_column`); the cursor indexes the
    // same list. With Planning hidden, In Progress also carries its cards.
    let tasks = app.task_display_column(status);
    let count = tasks.len();
    let col_focused = app.tasks.col == col_idx;
    // Done cards use their second content row for usage stats.
    let card_height: u16 = 5;
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let max_cards = (inner.height / card_height) as usize;
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
            TaskStatus::Backlog => "No tasks — press a to add one",
            TaskStatus::Planning => "Nothing planning — s hands a task to an agent",
            TaskStatus::Running => "Nothing running — Space approves a plan",
            TaskStatus::Review => "No PR waiting on you",
            TaskStatus::Done => "Nothing done yet",
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
        y = y.saturating_add(card_height);
        if y >= inner.y + inner.height {
            break;
        }
    }
}
