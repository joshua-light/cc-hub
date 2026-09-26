//! Type-to-filter pickers sharing one chrome: a filter row with a match
//! count, then rows with the matched chars highlighted.

use super::widgets::highlight_spans;
use crate::app::App;
use crate::ui::common::{centered_fixed, popup_block};
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

/// `N` on the Sessions tab: fuzzy-filter the model for the selected coding
/// agent. The footer shows the agent and spawn cwd captured by the picker.
pub(crate) fn render_model_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.model_picker.as_ref() else {
        return;
    };
    let choices = &picker.choices;

    let desired_w = 60u16.min(area.width);
    let desired_h = (crate::app::SPAWN_MODELS.len().max(choices.len()) as u16 + 5).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " New session — pick model / agent ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        format!(" → {} in {} ", picker.agent_id, picker.cwd),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 || inner.width == 0 {
        return;
    }

    let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let mut filter_line = picker.filter.clone();
    filter_line.push('▎');
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " ❯ ",
                Style::default()
                    .fg(ACCENT_BLUE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                filter_line,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        filter_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{}/{} ", picker.rows.len(), choices.len()),
            Style::default().fg(DIM_TEXT),
        )))
        .alignment(Alignment::Right),
        filter_area,
    );

    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches — backspace to widen)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_area.height as usize;
        let start = picker.selected.saturating_sub(visible.saturating_sub(1));
        let label_width = choices
            .iter()
            .map(|choice| choice.label.chars().count())
            .max()
            .unwrap_or(0);
        for (i, row) in picker.rows.iter().enumerate().skip(start).take(visible) {
            let Some(choice) = choices.get(row.choice) else {
                continue;
            };
            let label = &choice.label;
            let detail = &choice.detail;
            let selected = i == picker.selected;
            let bar = if selected {
                Style::default().bg(Color::White)
            } else {
                Style::default()
            };
            let (label_base, label_hl, id_base, id_hl) = if selected {
                (
                    bar.fg(Color::Black).add_modifier(Modifier::BOLD),
                    bar.fg(Color::Blue).add_modifier(Modifier::BOLD),
                    bar.fg(Color::Rgb(90, 90, 100)),
                    bar.fg(Color::Blue),
                )
            } else {
                (
                    Style::default().fg(Color::Gray),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::Cyan),
                )
            };
            let mut spans = vec![Span::styled(
                if selected { "▶ " } else { "  " },
                bar.fg(Color::Black).add_modifier(Modifier::BOLD),
            )];
            spans.extend(highlight_spans(
                label,
                &row.label_indices,
                label_base,
                label_hl,
            ));
            spans.push(Span::styled(
                " ".repeat(label_width.saturating_sub(label.chars().count()) + 2),
                bar,
            ));
            spans.extend(highlight_spans(detail, &row.detail_indices, id_base, id_hl));
            lines.push(Line::from(spans));
        }
    }

    frame.render_widget(Paragraph::new(lines), list_area);
}

/// `L` on the Sessions tab: fuzzy-filter which task the selected session is
/// linked to. Same chrome and interaction as the model picker; the footer
/// names the session being linked.
pub(crate) fn render_task_link_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.task_link_picker.as_ref() else {
        return;
    };
    let choices = &picker.choices;

    // Wide enough for a 48-char title column plus the longest
    // `icon board-label` status chip without truncating it.
    let desired_w = 72u16.min(area.width);
    let desired_h = (choices.len().max(8) as u16 + 5).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " Link session → task ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        format!(" → {} ", picker.session_label),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 || inner.width == 0 {
        return;
    }

    let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let mut filter_line = picker.filter.clone();
    filter_line.push('▎');
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " ❯ ",
                Style::default()
                    .fg(ACCENT_BLUE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                filter_line,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        filter_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{}/{} ", picker.rows.len(), choices.len()),
            Style::default().fg(DIM_TEXT),
        )))
        .alignment(Alignment::Right),
        filter_area,
    );

    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches — backspace to widen)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_area.height as usize;
        let start = picker.selected.saturating_sub(visible.saturating_sub(1));
        let label_width = choices
            .iter()
            .map(|choice| choice.label.chars().count())
            .max()
            .unwrap_or(0);
        for (i, row) in picker.rows.iter().enumerate().skip(start).take(visible) {
            let Some(choice) = choices.get(row.choice) else {
                continue;
            };
            let label = &choice.label;
            let detail = &choice.detail;
            let selected = i == picker.selected;
            let unlink = choice.action == crate::app::TaskLinkAction::Unlink;
            let bar = if selected {
                Style::default().bg(Color::White)
            } else {
                Style::default()
            };
            let (label_base, label_hl, detail_base, detail_hl) = if selected {
                (
                    bar.fg(Color::Black).add_modifier(Modifier::BOLD),
                    bar.fg(Color::Blue).add_modifier(Modifier::BOLD),
                    bar.fg(Color::Rgb(90, 90, 100)),
                    bar.fg(Color::Blue),
                )
            } else if unlink {
                // The destructive row reads differently at a glance.
                (
                    Style::default().fg(Color::Red),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::Red),
                )
            } else {
                (
                    Style::default().fg(Color::Gray),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::Cyan),
                )
            };
            let mut spans = vec![Span::styled(
                if selected { "▶ " } else { "  " },
                bar.fg(Color::Black).add_modifier(Modifier::BOLD),
            )];
            spans.extend(highlight_spans(
                label,
                &row.label_indices,
                label_base,
                label_hl,
            ));
            spans.push(Span::styled(
                " ".repeat(label_width.saturating_sub(label.chars().count()) + 2),
                bar,
            ));
            match choice.status {
                // Task rows end their detail with the status's board label
                // (see `task_link_candidate`); render that tail as a colored
                // status chip — same hue as the Tasks-board column — with an
                // icon the fuzzy filter never sees.
                Some(status) => {
                    let (icon, accent) = crate::ui::common::task_status_meta(status);
                    let status_label = status.board_label();
                    let prefix_chars = detail
                        .chars()
                        .count()
                        .saturating_sub(status_label.chars().count());
                    let prefix: String = detail.chars().take(prefix_chars).collect();
                    let prefix_idx: Vec<usize> = row
                        .detail_indices
                        .iter()
                        .copied()
                        .filter(|i| *i < prefix_chars)
                        .collect();
                    let status_idx: Vec<usize> = row
                        .detail_indices
                        .iter()
                        .copied()
                        .filter(|i| *i >= prefix_chars)
                        .map(|i| i - prefix_chars)
                        .collect();
                    // On the white selection bar the light status hues wash
                    // out — readability wins there; the bar itself already
                    // marks the row.
                    let status_base = if selected {
                        bar.fg(Color::Black).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(accent)
                    };
                    spans.extend(highlight_spans(
                        &prefix,
                        &prefix_idx,
                        detail_base,
                        detail_hl,
                    ));
                    spans.push(Span::styled(format!("{} ", icon), status_base));
                    spans.extend(highlight_spans(
                        status_label,
                        &status_idx,
                        status_base,
                        detail_hl,
                    ));
                }
                None => spans.extend(highlight_spans(
                    detail,
                    &row.detail_indices,
                    detail_base,
                    detail_hl,
                )),
            }
            lines.push(Line::from(spans));
        }
    }

    frame.render_widget(Paragraph::new(lines), list_area);
}

/// `/` on the Sessions tab: fuzzy-search the whole session archive. Same
/// chrome and interaction as the task-link picker; each row is the session's
/// saved title (or first message), its project · short id, and a dim age.
pub(crate) fn render_session_finder(frame: &mut Frame, area: Rect, app: &App) {
    let Some(finder) = app.session_finder.as_ref() else {
        return;
    };
    let choices = &finder.choices;
    let now = crate::ui::now_ms();

    let desired_w = 96u16.min(area.width);
    let desired_h = area.height.saturating_sub(4).max(12).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " Find session ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " title · first message · project · session id ",
        Style::default().fg(DIM_TEXT),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 || inner.width == 0 {
        return;
    }

    let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let mut filter_line = finder.filter.clone();
    filter_line.push('▎');
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " ❯ ",
                Style::default()
                    .fg(ACCENT_BLUE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                filter_line,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        filter_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if finder.loading {
                "indexing… ".to_string()
            } else {
                format!("{}/{} ", finder.rows.len(), choices.len())
            },
            Style::default().fg(DIM_TEXT),
        )))
        .alignment(Alignment::Right),
        filter_area,
    );

    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if finder.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            if finder.loading {
                "  (indexing the session archive…)"
            } else {
                "  (no matches — backspace to widen)"
            },
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_area.height as usize;
        let start = finder.selected.saturating_sub(visible.saturating_sub(1));
        // Pad labels only as wide as the rows on screen need — the archive
        // holds outliers that would otherwise push every detail off-edge.
        let label_width = finder
            .rows
            .iter()
            .skip(start)
            .take(visible)
            .filter_map(|row| choices.get(row.choice))
            .map(|choice| choice.label.chars().count())
            .max()
            .unwrap_or(0);
        for (i, row) in finder.rows.iter().enumerate().skip(start).take(visible) {
            let Some(choice) = choices.get(row.choice) else {
                continue;
            };
            let label = &choice.label;
            let detail = &choice.detail;
            let selected = i == finder.selected;
            let bar = if selected {
                Style::default().bg(Color::White)
            } else {
                Style::default()
            };
            let (label_base, label_hl, detail_base, detail_hl) = if selected {
                (
                    bar.fg(Color::Black).add_modifier(Modifier::BOLD),
                    bar.fg(Color::Blue).add_modifier(Modifier::BOLD),
                    bar.fg(Color::Rgb(90, 90, 100)),
                    bar.fg(Color::Blue),
                )
            } else {
                (
                    Style::default().fg(Color::Gray),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::Cyan),
                )
            };
            let mut spans = vec![Span::styled(
                if selected { "▶ " } else { "  " },
                bar.fg(Color::Black).add_modifier(Modifier::BOLD),
            )];
            spans.extend(highlight_spans(
                label,
                &row.label_indices,
                label_base,
                label_hl,
            ));
            spans.push(Span::styled(
                " ".repeat(label_width.saturating_sub(label.chars().count()) + 2),
                bar,
            ));
            spans.extend(highlight_spans(
                detail,
                &row.detail_indices,
                detail_base,
                detail_hl,
            ));
            // Age is visual metadata only — the fuzzy filter never sees it.
            let age_secs = now.saturating_sub(choice.mtime_ms) / 1000;
            spans.push(Span::styled(
                format!("  {}", crate::models::relative_age_short(age_secs)),
                if selected {
                    bar.fg(Color::Rgb(90, 90, 100))
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
            lines.push(Line::from(spans));
        }
    }

    frame.render_widget(Paragraph::new(lines), list_area);
}

#[cfg(all(test, unix))]
mod task_link_picker_tests {
    use crate::app::{App, TaskLinkAction, TaskLinkChoice, TaskLinkPickerState, View};
    use crate::tasks::store::TaskStatus;
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    fn choice(label: &str, status: TaskStatus) -> TaskLinkChoice {
        TaskLinkChoice {
            label: label.into(),
            detail: status.board_label().to_string(),
            status: Some(status),
            action: TaskLinkAction::Link {
                task_id: format!("tk-{label}"),
                title: label.into(),
            },
        }
    }

    /// Style of the first buffer cell whose symbol is `ch`.
    fn cell_fg(buf: &ratatui::buffer::Buffer, ch: char) -> Option<Color> {
        let area = *buf.area();
        for y in 0..area.height {
            for x in 0..area.width {
                if buf[(x, y)].symbol() == ch.to_string() {
                    return buf[(x, y)].style().fg;
                }
            }
        }
        None
    }

    #[test]
    fn status_chip_uses_the_tasks_board_colors() {
        with_temp_home(|| {
            let mut app = App::new();
            app.task_link_picker = Some(TaskLinkPickerState::new(
                "sid".into(),
                "sid".into(),
                vec![
                    // Row 0 is selected — its chip drops the accent for
                    // readability on the white bar, so the color assertions
                    // target the unselected rows below it (Done's label
                    // shares no capitals with theirs).
                    choice("current", TaskStatus::Done),
                    choice("second", TaskStatus::Running),
                    choice("third", TaskStatus::Backlog),
                ],
                None,
            ));
            app.view = View::TaskLinkPicker;

            let backend = TestBackend::new(80, 20);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_task_link_picker(f, f.area(), &app))
                .expect("render");
            let buf = terminal.backend().buffer().clone();
            let rendered = buffer_to_string(&buf);

            assert!(rendered.contains("In Progress"), "chip text:\n{}", rendered);
            assert!(rendered.contains("To-Do"), "chip text:\n{}", rendered);
            // Board-label capitals are unique to the chips in this frame, so
            // their cells pin the chip color: In *P*rogress → the In Progress
            // column accent, *T*o-Do → the To-Do column accent.
            assert_eq!(cell_fg(&buf, 'P'), Some(Color::LightYellow));
            assert_eq!(cell_fg(&buf, 'T'), Some(crate::ui::palette::BACKLOG_BLUE));
        });
    }
}

#[cfg(all(test, unix))]
mod model_picker_tests {
    use crate::agent::{default_claude_models, AgentConfig, AgentKind, AgentModel};
    use crate::app::{App, ModelPickerState, View};
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn renders_filter_match_count_and_surviving_model() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_model_picker();
            for c in "sn5".chars() {
                app.model_picker.as_mut().unwrap().push_filter(c);
            }
            assert_eq!(app.view, View::ModelPicker);

            let backend = TestBackend::new(70, 16);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_model_picker(f, f.area(), &app))
                .expect("render");
            let rendered = buffer_to_string(terminal.backend().buffer());

            assert!(rendered.contains("sn5▎"), "filter line:\n{}", rendered);
            assert!(rendered.contains("1/3"), "match count:\n{}", rendered);
            assert!(rendered.contains("Sonnet 5"), "matched row:\n{}", rendered);
            assert!(
                !rendered.contains("Opus 4.8") && !rendered.contains("Fable 5"),
                "filtered-out rows must not render:\n{}",
                rendered
            );
        });
    }

    #[test]
    fn renders_cycled_agent_and_its_models() {
        with_temp_home(|| {
            let mut app = App::new();
            app.model_picker = Some(ModelPickerState::new(
                "/tmp/proj".into(),
                "claude".into(),
                vec![
                    AgentConfig {
                        id: "claude".into(),
                        kind: AgentKind::Claude,
                        command: "claude".into(),
                        use_bridge: false,
                        models: default_claude_models(),
                    },
                    AgentConfig {
                        id: "pi-codex".into(),
                        kind: AgentKind::Pi,
                        command: "pi --provider openai-codex".into(),
                        use_bridge: true,
                        models: vec![AgentModel {
                            label: "GPT-5.6".into(),
                            id: "gpt-5.6".into(),
                        }],
                    },
                ],
            ));
            app.model_picker.as_mut().unwrap().cycle_agent();
            app.view = View::ModelPicker;

            let backend = TestBackend::new(80, 16);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_model_picker(f, f.area(), &app))
                .expect("render");
            let rendered = buffer_to_string(terminal.backend().buffer());

            assert!(rendered.contains("pi-codex"), "agent footer:\n{}", rendered);
            assert!(
                rendered.contains("GPT-5.6"),
                "configured model:\n{}",
                rendered
            );
            assert!(rendered.contains("gpt-5.6"), "model id:\n{}", rendered);
        });
    }
}
