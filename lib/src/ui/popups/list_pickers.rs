//! Plain j/k list pickers: default agent, respawn account, task kind.

use crate::app::App;
use crate::ui::common::{centered_fixed, popup_block};
use crate::ui::palette::DIM_TEXT;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

/// `A` on the Sessions tab: choose the coding agent used by later
/// new-session actions.
pub(crate) fn render_agent_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.agent_picker.as_ref() else {
        return;
    };

    let desired_w = 64u16.min(area.width);
    let desired_h = (picker.agents.len().max(4) as u16 + 2).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " Default agent for new sessions ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " j/k:move · enter/space:select · esc:cancel ",
        Style::default().fg(DIM_TEXT),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let label_width = picker
        .agents
        .iter()
        .map(|agent| agent.display_label().chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line<'static>> = picker
        .agents
        .iter()
        .enumerate()
        .map(|(i, agent)| {
            let selected = i == picker.selected;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let label = agent.display_label();
            Line::from(Span::styled(
                format!(
                    "{} {}  {}{}",
                    if selected { "▶" } else { " " },
                    label,
                    " ".repeat(label_width.saturating_sub(label.chars().count())),
                    agent.id
                ),
                style,
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `R` on the Sessions tab: pick the subscription account the selected
/// session respawns on. Each row already says how it would continue —
/// `resume` (native `--resume` under the target home) or `handoff` (fresh
/// session reading the old transcript) — so the choice is honest before
/// Enter commits it.
pub(crate) fn render_respawn_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.respawn_picker.as_ref() else {
        return;
    };

    let desired_w = 64u16.min(area.width);
    let desired_h = (picker.choices.len().max(4) as u16 + 2).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        format!(
            " Respawn {} on account ",
            crate::models::short_sid(&picker.session.session_id)
        ),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " j/k:move · enter/space:respawn · esc:cancel ",
        Style::default().fg(DIM_TEXT),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let id_width = picker
        .choices
        .iter()
        .map(|choice| choice.account_id.chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line<'static>> = picker
        .choices
        .iter()
        .enumerate()
        .map(|(i, choice)| {
            let selected = i == picker.selected;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else if choice.plan.is_err() {
                Style::default().fg(DIM_TEXT)
            } else {
                Style::default().fg(Color::Gray)
            };
            let how = match &choice.plan {
                Ok(plan) => plan.label().to_string(),
                Err(why) => format!("unavailable: {}", why),
            };
            let current = if choice.account_id == picker.session.agent_id {
                "  (current)"
            } else {
                ""
            };
            Line::from(Span::styled(
                format!(
                    "{} {}{}  {} · {}{}",
                    if selected { "▶" } else { " " },
                    choice.account_id,
                    " ".repeat(id_width.saturating_sub(choice.account_id.chars().count())),
                    choice.provider.badge(),
                    how,
                    current,
                ),
                style,
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `T` on a focused board card: the list of deliverable kinds the card can be
/// given. Same chrome and interaction as the agent picker; the first row is
/// the empty pick, which hands the classification back to the task router.
pub(crate) fn render_task_kind_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.task_kind_picker.as_ref() else {
        return;
    };

    let desired_w = 56u16.min(area.width);
    let desired_h = (picker.rows() as u16 + 2).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " Task kind — how the router places this card ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " j/k:move · enter/space:select · esc:cancel ",
        Style::default().fg(DIM_TEXT),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines: Vec<Line<'static>> = (0..picker.rows())
        .map(|i| {
            let selected = i == picker.selected;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let label = match picker.kinds.get(i.wrapping_sub(1)) {
                Some(kind) => kind.as_str(),
                None => "(none — let the router classify)",
            };
            Line::from(Span::styled(
                format!("{} {}", if selected { "▶" } else { " " }, label),
                style,
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}
#[cfg(test)]
mod task_kind_picker_tests {
    use crate::app::TaskKindPickerState;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn picker_lists_every_kind_and_marks_the_current_one() {
        let picker = TaskKindPickerState::new(
            "tk-1".into(),
            vec!["tps".into(), "ai-plugin".into(), "hub".into()],
            Some("ai-plugin"),
        );
        let mut app = crate::app::App::new();
        app.task_kind_picker = Some(picker);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("terminal");
        terminal
            .draw(|f| super::render_task_kind_picker(f, f.area(), &app))
            .expect("render");
        let painted = buffer_to_string(terminal.backend().buffer());
        for kind in ["none", "tps", "ai-plugin", "hub"] {
            assert!(painted.contains(kind), "{} missing:\n{}", kind, painted);
        }
        let marked = painted
            .lines()
            .find(|line| line.contains('▶'))
            .expect("a marked row");
        assert!(
            marked.contains("ai-plugin"),
            "the card's own kind should be under the cursor:\n{}",
            painted
        );
    }
}
