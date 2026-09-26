//! The close-terminal confirmation dialog.

use crate::app::{App, PendingConfirm};
use crate::ui::common::{centered_fixed, popup_block};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

pub(crate) fn render_confirm_close(frame: &mut Frame, area: Rect, app: &App) {
    let (title, display, consequence, action_color) = match app.pending_confirm.as_ref() {
        Some(PendingConfirm::Close(pending)) => (
            " Close terminal? ",
            pending.display.clone(),
            "Closes the OS terminal window hosting this session when the platform can resolve it. The tmux session may survive.",
            Color::Red,
        ),
        None => {
            return;
        }
    };

    let popup = centered_fixed(area, 76, 8);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(action_color)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(action_color))
    .title_bottom(Span::styled(
        " [Y]es · [N]o · Esc cancel ",
        Style::default().fg(Color::DarkGray),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                display,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(consequence, Style::default().fg(Color::Rgb(170, 170, 185))),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
