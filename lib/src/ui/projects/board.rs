//! The kanban board: column layout, column headers, and per-column card
//! dispatch (active vs collapsed cards, merge-lock banner wiring).

use crate::app::App;
use crate::models::SessionInfo;
use crate::ui::now_ms;
use crate::ui::palette::{DIM_TEXT, DOT_IDLE, LABEL_GRAY, PURPLE};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::{render_task_card_active, render_task_card_collapsed, MergeLockBanner};

pub(crate) fn render_kanban_board(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 3 || area.width < 30 {
        let hint = Paragraph::new("(terminal too narrow — resize or switch to Sessions)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(DOT_IDLE))
            .wrap(Wrap { trim: false });
        frame.render_widget(hint, area);
        return;
    }
    // Five columns: Planning · Running · Review · Merging · Done.
    // Running takes the most space; Planning, Review, Merging, and Done are
    // roughly equal.
    // Merging is project-wide-serialized, so at most one card lives in it
    // at a time — narrow column suffices. Ratio totals to 11.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(2, 11), // Planning
            Constraint::Ratio(3, 11), // Running
            Constraint::Ratio(2, 11), // Review
            Constraint::Ratio(2, 11), // Merging
            Constraint::Ratio(2, 11), // Done
        ])
        .split(area);

    let sessions_by_tmux = app.sessions_by_tmux();
    let now_secs = now_ms() / 1000;
    let pr_summaries = &app.projects.snapshot.pr_summaries;

    for col_idx in 0..5 {
        render_kanban_column(
            frame,
            cols[col_idx],
            app,
            col_idx,
            &sessions_by_tmux,
            pr_summaries,
            now_secs,
        );
    }
}

pub(crate) fn kanban_column_meta(col: usize) -> (&'static str, &'static str, Color) {
    // (label, status icon, accent color). Indices match `kanban_column_tasks`.
    let (icon, accent) = match col {
        0 => ("󰟶", PURPLE),
        1 => ("󰒓", Color::LightYellow),
        2 => ("󱋲", Color::LightCyan),
        3 => ("", Color::LightMagenta),
        _ => ("󰸞", Color::LightGreen),
    };
    (crate::app::kanban_col_name(col), icon, accent)
}

pub(crate) fn render_kanban_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    col_idx: usize,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
    pr_summaries: &std::collections::HashMap<String, crate::projects_scan::PrCardSummary>,
    now_secs: u64,
) {
    let (label, icon, accent) = kanban_column_meta(col_idx);
    let tasks = app.kanban_column_tasks(col_idx);
    let count = tasks.len();
    let col_focused = app.projects.col == col_idx;
    // Planning + Running show tall rich cards (orchestrator is alive,
    // there's live state to display); Review/Merging/Done get compact
    // cards since they're terminal states from the UI's POV.
    let card_height: u16 = if col_idx <= 1 { 6 } else { 4 };
    let gap: u16 = 1;
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let max_cards =
        ((inner.height as u32 + gap as u32) / (card_height as u32 + gap as u32)) as usize;
    let sel = if count == 0 {
        0
    } else if col_focused {
        app.projects.task_sel.min(count - 1)
    } else {
        0
    };
    let scroll_top = if count == 0 || max_cards == 0 || sel < max_cards {
        0
    } else {
        sel + 1 - max_cards
    };
    let hidden_below = count.saturating_sub(scroll_top.saturating_add(max_cards));
    // Only the Merging column needs the lock-holder lookup — collapsed
    // cards in other columns ignore it.
    let merging_holder_banner: Option<MergeLockBanner<'_>> = if col_idx == 3 {
        app.selected_project().and_then(|p| {
            let holder = app
                .projects
                .snapshot
                .merge_lock_holders
                .get(&p.id)
                .and_then(|h| h.as_ref())?;
            let title = app
                .projects
                .snapshot
                .tasks
                .get(&p.id)
                .and_then(|ts| ts.iter().find(|t| t.task_id == holder.task_id))
                .and_then(|t| t.title.as_deref());
            let pr_id = app
                .projects
                .snapshot
                .merge_lock_holder_pr_ids
                .get(&p.id)
                .and_then(|v| *v);
            Some(MergeLockBanner {
                task_id: holder.task_id.as_str(),
                title,
                acquired_at: holder.acquired_at,
                phase: holder.phase,
                pr_id,
            })
        })
    } else {
        None
    };

    // Column border. Focused column gets the accent color + Double border so
    // it stands out without changing the layout.
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
    let title = Line::from(title_spans);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .title(title);
    frame.render_widget(block, area);

    if tasks.is_empty() {
        let empty_hint = match col_idx {
            0 => "No planning tasks",
            1 => "No active workers",
            2 => "No reviews pending",
            3 => "No merges queued",
            _ => "No completed tasks",
        };
        let hint = Paragraph::new(Line::from(Span::styled(
            empty_hint,
            Style::default().fg(Color::Rgb(70, 70, 90)),
        )))
        .alignment(Alignment::Center);
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
        let titling_in_flight = app.projects.snapshot.is_titling(&t.task_id);
        let pr_summary = pr_summaries.get(&t.task_id);
        if col_idx <= 1 {
            render_task_card_active(
                frame,
                card_area,
                t,
                selected,
                col_idx,
                sessions_by_tmux,
                pr_summary,
                now_secs,
                titling_in_flight,
            );
        } else {
            render_task_card_collapsed(
                frame,
                card_area,
                t,
                selected,
                col_idx,
                sessions_by_tmux,
                pr_summary,
                now_secs,
                titling_in_flight,
                merging_holder_banner.as_ref(),
            );
        }
        y = y.saturating_add(card_height + gap);
        if y >= inner.y + inner.height {
            break;
        }
    }
}
