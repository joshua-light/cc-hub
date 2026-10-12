//! Header bands: the title bar and the tab strip.

use crate::app::{visible_tabs, App};
use crate::ui::palette::{ACCENT_BLUE, SEP_GRAY};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Tab names at rest; the active one is white.
const TAB_GRAY: Color = Color::Rgb(120, 120, 140);
/// The key hint after the last tab.
const HINT_GRAY: Color = Color::Rgb(80, 80, 95);

/// The tab strip, on the terminal's own background: ` icon Label ` per tab,
/// over a hairline rule the width of the screen.
/// The active tab is white and stands on a heavy accent stretch of that
/// rule; the rest are gray.
pub(super) fn render_tab_strip(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 2 {
        return;
    }
    let tabs_y = area.y + area.height - 2;
    let rule_y = area.y + area.height - 1;

    let mut names: Vec<Span<'static>> = vec![Span::raw("  ")];
    let mut rule: Vec<Span<'static>> = vec![Span::styled("──", Style::default().fg(SEP_GRAY))];
    for tab in visible_tabs() {
        let active = tab == app.current_tab;
        let name = format!(" {} {} ", tab.icon(), tab.label());
        let width = name.chars().count();
        let (name_style, rule_span) = if active {
            (
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
                Span::styled("━".repeat(width), Style::default().fg(ACCENT_BLUE)),
            )
        } else {
            (
                Style::default().fg(TAB_GRAY),
                Span::styled("─".repeat(width), Style::default().fg(SEP_GRAY)),
            )
        };
        names.push(Span::styled(name, name_style));
        rule.push(rule_span);
    }
    names.push(Span::styled(
        "   ⇥/K next · ⇧⇥/J prev tab",
        Style::default().fg(HINT_GRAY),
    ));
    let drawn: usize = rule.iter().map(|s| s.content.chars().count()).sum();
    rule.push(Span::styled(
        "─".repeat((area.width as usize).saturating_sub(drawn)),
        Style::default().fg(SEP_GRAY),
    ));

    let row = |y| Rect::new(area.x, y, area.width, 1);
    frame.render_widget(Paragraph::new(Line::from(names)), row(tabs_y));
    frame.render_widget(Paragraph::new(Line::from(rule)), row(rule_y));
}

pub(super) fn render_title_bar(frame: &mut Frame, area: Rect, app: &App) {
    // Who is waiting on you is the tabs' to say; the bar names the app.
    let left_spans = vec![
        Span::styled(
            " 󰚩 hal ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            concat!("v", env!("CARGO_PKG_VERSION")),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    let bg = Style::default().fg(Color::White);
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::Tab;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    // The active tab stands on the heavy stretch of the rule, exactly as
    // wide as its name; the band itself paints no background.
    #[test]
    fn the_active_tab_stands_on_the_accent_rule() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            app.current_tab = Tab::Sessions;
            let mut terminal = Terminal::new(TestBackend::new(100, 3)).expect("terminal");
            terminal
                .draw(|f| render_tab_strip(f, f.area(), &app))
                .expect("render");
            let buf = terminal.backend().buffer();
            let row = |y: u16| -> String { (0..100).map(|x| buf[(x, y)].symbol()).collect() };
            let (names, rule) = (row(1), row(2));

            let start = names.find(" 󰆍 Sessions ").expect("sessions tab");
            let col = names[..start].chars().count();
            let heavy: String = rule.chars().skip(col).take_while(|c| *c == '━').collect();
            assert_eq!(heavy.chars().count(), " 󰆍 Sessions ".chars().count());
            assert!((0..100).all(|x| buf[(x, 1)].bg == Color::Reset));
        });
    }
}
