//! Header bands: the title bar and the tab strip.

use crate::app::{visible_tabs, App};
use crate::ui::palette::{ACCENT_BLUE, SEP_GRAY};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Background of the tab-strip band.
const BAND_BG: Color = Color::Rgb(20, 20, 28);

pub(super) fn render_tab_strip(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let bg = Style::default().bg(BAND_BG);

    // Paint the full band (top padding + tabs row + bottom padding) so the
    // background colour reads as a continuous header strip.
    frame.render_widget(Paragraph::new("").style(bg), area);

    let tabs = visible_tabs();
    let mut spans: Vec<Span<'static>> = vec![Span::styled("  ", bg)];
    for (i, tab) in tabs.iter().enumerate() {
        let is_active = *tab == app.current_tab;
        let (fg, bgc, modi) = if is_active {
            (Color::Black, ACCENT_BLUE, Modifier::BOLD)
        } else {
            (
                Color::Rgb(170, 170, 190),
                Color::Rgb(40, 40, 52),
                Modifier::empty(),
            )
        };
        spans.push(Span::styled(
            format!(" {} ", tab.label()),
            Style::default().fg(fg).bg(bgc).add_modifier(modi),
        ));
        if i + 1 < tabs.len() {
            spans.push(Span::styled(" ", bg));
        }
    }
    spans.push(Span::styled(
        "   ⇥/K next · ⇧⇥/J prev tab",
        Style::default().fg(Color::Rgb(80, 80, 95)).bg(BAND_BG),
    ));

    // Tabs go on the visual middle row (or first row if the band is shorter).
    let row_y = area.y + area.height / 2;
    let row_area = Rect::new(area.x, row_y, area.width, 1);
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), row_area);
}

pub(super) fn render_title_bar(frame: &mut Frame, area: Rect, app: &App) {
    let total = app.session_count();
    let attention = app.attention_count();

    let mut left_spans = vec![
        Span::styled(
            " 󰚩 cc-hub ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} sessions", total),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    if attention > 0 {
        left_spans.push(Span::styled(
            format!("  󰂞 {} need attention", attention),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let bg = Style::default().bg(Color::Rgb(30, 30, 40)).fg(Color::White);
    let left_line = Line::from(left_spans);

    let mut right_spans = build_session_count_spans(&app.session_counts);
    let usage_spans = app.usage_line.spans.iter().cloned();
    if !right_spans.is_empty() && app.usage_line.width() > 0 {
        right_spans.push(Span::styled(" │ ", Style::default().fg(SEP_GRAY)));
    }
    right_spans.extend(usage_spans);
    let right_line = Line::from(right_spans);
    let right_w = right_line.width() as u16;
    let left_w = left_line.width() as u16;

    // If usage would overflow, fall back to just the left line.
    if right_w == 0 || left_w + right_w > area.width {
        frame.render_widget(Paragraph::new(left_line).style(bg), area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_w)])
        .split(area);

    frame.render_widget(Paragraph::new(left_line).style(bg), chunks[0]);
    frame.render_widget(
        Paragraph::new(right_line)
            .style(bg)
            .alignment(Alignment::Right),
        chunks[1],
    );
}

fn build_session_count_spans(c: &crate::sessions::count::SessionCounts) -> Vec<Span<'static>> {
    if c.today == 0 && c.week == 0 {
        return Vec::new();
    }
    let label_style = Style::default().fg(Color::DarkGray);
    let num_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let sep_style = Style::default().fg(SEP_GRAY);
    vec![
        Span::styled(" today ", label_style),
        Span::styled(c.today.to_string(), num_style),
        Span::styled(" │ ", sep_style),
        Span::styled("this wk ", label_style),
        Span::styled(c.week.to_string(), num_style),
    ]
}
