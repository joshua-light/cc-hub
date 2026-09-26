//! Single-line editors: task attachment, task tags, session rename.

use crate::app::App;
use crate::ui::common::{centered_fixed, popup_block};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

/// Centered single-line input for attaching to the focused board card. Same
/// shape as the task input popup; the buffer accepts a paste (bracketed paste
/// routes here via `App::paste_into_input`), enter attaches, esc cancels.
/// Opens in note mode — the buffer becomes a typed `note` attachment; Tab
/// flips to file-path/URL mode and back, keeping the buffer.
pub(crate) fn render_task_attach_input(frame: &mut Frame, area: Rect, app: &App) {
    let note_mode = app.tasks.attach_note;
    let mut input_line = app.tasks.input.clone();
    input_line.push('▎');

    let desired_w = 70u16.min(area.width);
    let wrap_width = desired_w.saturating_sub(6) as usize;
    let input_rows: u16 = if wrap_width == 0 {
        1
    } else {
        let w = input_line.chars().count();
        w.div_ceil(wrap_width).max(1).try_into().unwrap_or(u16::MAX)
    };
    let desired_h = 5u16.saturating_add(input_rows).max(9).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let (title, hint) = if note_mode {
        (" Attach note ", " saved as a .md note inside the task ")
    } else {
        (
            " Attach file or URL ",
            " files are copied into the task — safe to clean the original ",
        )
    };
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        hint,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let footer_spans = vec![
        Span::raw("  "),
        Span::styled(
            "[enter]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" attach   ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "[tab]",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if note_mode {
                " file/URL   "
            } else {
                " note   "
            },
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            "[esc]",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ];
    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                if note_mode { "  ✎ " } else { "  📎 " },
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                input_line,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(footer_spans),
    ];

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Centered single-line editor for the focused task's tags. Same shape as the
/// task input popup: the buffer is prefilled with the current tags, typing
/// edits the whole set (space/comma separated), enter saves, esc cancels.
pub(crate) fn render_task_tags(frame: &mut Frame, area: Rect, app: &App) {
    let mut input_line = app.tasks.input.clone();
    input_line.push('▎');

    let desired_w = 70u16.min(area.width);
    let wrap_width = desired_w.saturating_sub(6) as usize;
    let input_rows: u16 = if wrap_width == 0 {
        1
    } else {
        let w = input_line.chars().count();
        w.div_ceil(wrap_width).max(1).try_into().unwrap_or(u16::MAX)
    };
    let desired_h = 5u16.saturating_add(input_rows).max(9).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " Edit tags ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " space or comma separates tags ",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let footer_spans = vec![
        Span::raw("  "),
        Span::styled(
            "[enter]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" save   ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "[esc]",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ];
    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  # ", Style::default().fg(Color::Cyan)),
            Span::styled(
                input_line,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(footer_spans),
    ];

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

pub(crate) fn render_rename_session(frame: &mut Frame, area: Rect, app: &App) {
    let mut input_line = app.rename_buffer.clone();
    input_line.push('▎');

    let desired_w = 70u16.min(area.width);
    let desired_h = 9u16.min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let original = app.rename_original_title();
    let subtitle = match original {
        Some(t) if !t.is_empty() => format!(" was “{}” ", t),
        _ => " untitled ".to_string(),
    };

    let block = popup_block(Span::styled(
        " Rename session ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        subtitle,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let footer_spans = vec![
        Span::raw("  "),
        Span::styled(
            "[enter]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" rename   ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "[esc]",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ];
    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                input_line,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(footer_spans),
    ];

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
