//! Overlay views: folder picker, gh-create / prompt / rename inputs, confirm
//! dialogs, the to-do side panel, the embedded tmux pane, the state-debug
//! popup, and the live transcript tail.

use crate::app::{App, PendingConfirm};
use crate::config;
use crate::conversation::{StateExplanation, Verdict};
use crate::folder_picker::PickerMode;
use crate::models::SessionInfo;
use crate::ui::common::{centered_fixed, centered_rect, format_tokens, popup_block, state_color};
use crate::ui::main_layout;
use crate::ui::palette::{ACCENT_BLUE, CONTEXT_GRAY, DIM_TEXT, GRAY_80};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

pub(crate) fn render_folder_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.folder_picker.as_ref() else {
        return;
    };
    let bookmarks_mode = picker.mode == PickerMode::Bookmarks;

    let popup = centered_fixed(area, 80, 24);
    frame.render_widget(Clear, popup);

    let (title_text, footer_text, empty_text) = if bookmarks_mode {
        (
            " New session · bookmarks ",
            " j/k:move · enter/space:pick · m:unbookmark · esc:cancel ",
            "  (no bookmarks — press N to browse, then m on a folder)",
        )
    } else {
        (
            " New session · pick folder ",
            " enter:descend · bksp:parent · space:pick · .:pick cwd · m:bookmark · c/C:gh new · esc:cancel ",
            "  (no subdirectories)",
        )
    };

    let block = popup_block(Span::styled(
        title_text,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(footer_text, Style::default().fg(DIM_TEXT)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 {
        return;
    }

    let path_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let header_line = if bookmarks_mode {
        Line::from(vec![
            Span::styled(" ★ ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{} bookmark(s)", picker.entries.len()),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(" 󰉋 ", Style::default().fg(Color::Cyan)),
            Span::styled(
                picker.current_dir.display().to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    };
    frame.render_widget(Paragraph::new(header_line), path_area);

    let list_h = inner.height - 2;
    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, list_h);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.entries.is_empty() {
        lines.push(Line::from(Span::styled(
            empty_text,
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_h as usize;
        let start = picker.selection.saturating_sub(visible.saturating_sub(1));
        for (i, name) in picker.entries.iter().enumerate().skip(start).take(visible) {
            let selected = i == picker.selection;
            let (cursor_marker, style) = if selected {
                (
                    "▶ ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::Rgb(200, 200, 210)))
            };
            let display = if bookmarks_mode {
                name.clone()
            } else {
                format!("{}/", name)
            };
            // In browse mode, mark already-bookmarked subdirs with a star
            // so the user doesn't re-bookmark by accident.
            let star_span =
                if !bookmarks_mode && app.bookmarks.contains(&picker.current_dir.join(name)) {
                    Span::styled(
                        "★ ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("")
                };
            lines.push(Line::from(vec![
                Span::styled(cursor_marker, style),
                star_span,
                Span::styled(display, style),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), list_area);
}

pub(crate) fn render_gh_create_input(frame: &mut Frame, area: Rect, app: &App) {
    let Some(input) = app.gh_create_input.as_ref() else {
        return;
    };

    let popup = centered_fixed(area, 70, 9);
    frame.render_widget(Clear, popup);

    let (vis_label, vis_color) = if input.private {
        ("private", Color::Rgb(220, 170, 90))
    } else {
        ("public", Color::Rgb(120, 200, 140))
    };

    let block = popup_block(Span::styled(
        " gh repo create ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " type name · tab: toggle public/private · enter: create · esc: cancel ",
        Style::default().fg(DIM_TEXT),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 4 || inner.width == 0 {
        return;
    }

    let cwd_line = Line::from(vec![
        Span::styled(" in ", Style::default().fg(Color::DarkGray)),
        Span::styled(input.cwd.clone(), Style::default().fg(ACCENT_BLUE)),
    ]);

    let mut name_str = input.name.clone();
    name_str.push('▎');
    let name_line = Line::from(vec![
        Span::styled(" name: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            name_str,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let vis_line = Line::from(vec![
        Span::styled(" visibility: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            vis_label,
            Style::default().fg(vis_color).add_modifier(Modifier::BOLD),
        ),
    ]);

    let lines = vec![cwd_line, Line::raw(""), name_line, vis_line];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

pub(crate) fn render_prompt_input(frame: &mut Frame, area: Rect, app: &App) {
    let mut input_line = app.prompt_buffer.clone();
    input_line.push('▎');

    let desired_w = 80u16.min(area.width);
    let wrap_width = desired_w.saturating_sub(4) as usize;
    let prompt_lines: u16 = if wrap_width == 0 {
        1
    } else {
        let total: usize = input_line
            .split('\n')
            .map(|seg| {
                let w = seg.chars().count();
                w.div_ceil(wrap_width).max(1)
            })
            .sum();
        total.try_into().unwrap_or(u16::MAX)
    };
    let desired_h = 5u16.saturating_add(prompt_lines).max(9).min(area.height);

    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let project_mode = app.prompt_input_for_project();
    let (title, target_label, title_color) = if project_mode {
        let cwd = app
            .projects_pending_cwd
            .clone()
            .unwrap_or_else(|| "?".into());
        let agent = app.pending_agent_label().unwrap_or_else(|| "?".into());
        (
            " New project task ",
            format!(" → {} orchestrator in {} ", agent, cwd),
            Color::Cyan,
        )
    } else {
        let target = app.dispatch_target();
        let label = target
            .map(|(pid, name, tmux)| format!(" → {} (PID {}) [{}] ", name, pid, tmux))
            .unwrap_or_else(|| " → no idle agent — will spawn a new one ".to_string());
        let color = if target.is_some() {
            Color::Green
        } else {
            Color::Yellow
        };
        (" Dispatch prompt ", label, color)
    };

    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        target_label,
        Style::default()
            .fg(title_color)
            .add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut footer_spans = vec![
        Span::raw("  "),
        Span::styled(
            "[enter]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" dispatch   ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "[esc]",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ];
    if project_mode && config::get().resolved_agents().len() > 1 {
        footer_spans.push(Span::styled("   ", Style::default().fg(Color::DarkGray)));
        footer_spans.push(Span::styled(
            "[tab]",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ));
        footer_spans.push(Span::styled(
            " cycle agent",
            Style::default().fg(Color::DarkGray),
        ));
    }
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

/// Scratch to-do list, drawn as a right-anchored side panel over the body
/// region so the tab strip and status bar (which carries the panel's own key
/// hints) stay visible behind it.
pub(crate) fn render_todo_panel(frame: &mut Frame, area: Rect, app: &App) {
    // Anchor to the body band of the same split `render` uses, so the panel
    // sits under the header band and above the status row rather than
    // covering the whole screen.
    let body = main_layout(area)[2];

    let width = 46u16.min(body.width);
    if width == 0 || body.height == 0 {
        return;
    }
    let panel = Rect::new(body.x + body.width - width, body.y, width, body.height);
    frame.render_widget(Clear, panel);

    let done = app.todo.items().iter().filter(|i| i.done).count();
    let total = app.todo.len();
    let block = popup_block(Span::styled(
        format!(" To-Do · {}/{} done ", done, total),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        if app.todo_adding {
            " enter add · esc cancel "
        } else {
            " a add · space toggle · d delete · esc close "
        },
        Style::default().fg(DIM_TEXT),
    ));

    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // One row per item (long text is clipped, not wrapped) so the window
    // arithmetic below stays exact: scroll the list to keep the selection
    // visible, and in add mode reserve the bottom two rows (spacer + input)
    // so the input line can never be pushed off-screen by a long list.
    let input_rows = if app.todo_adding { 2usize } else { 0 };
    let list_rows = (inner.height as usize).saturating_sub(input_rows);
    let sel = app.todo_selected.min(total.saturating_sub(1));
    let scroll_top = if total <= list_rows || sel < list_rows {
        0
    } else {
        sel + 1 - list_rows
    };

    let mut lines: Vec<Line> = Vec::new();
    if total == 0 && !app.todo_adding {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "  No tasks yet — press a to add one.",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
    } else {
        for (i, item) in app
            .todo
            .items()
            .iter()
            .enumerate()
            .skip(scroll_top)
            .take(list_rows)
        {
            let selected = !app.todo_adding && i == sel;
            let cursor = if selected { "› " } else { "  " };
            let checkbox = if item.done { "[x] " } else { "[ ] " };
            let text_style = if item.done {
                Style::default().fg(DIM_TEXT).add_modifier(Modifier::ITALIC)
            } else if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(200, 200, 210))
            };
            let marker_style = if item.done {
                Style::default().fg(Color::Green)
            } else if selected {
                Style::default()
                    .fg(ACCENT_BLUE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(Line::from(vec![
                Span::styled(cursor, marker_style),
                Span::styled(checkbox, marker_style),
                Span::styled(item.text.clone(), text_style),
            ]));
        }
    }

    if app.todo_adding {
        let mut input = app.todo_input.clone();
        input.push('▎');
        // Without wrap the line clips on the right, which would hide the
        // cursor on long input — show the tail instead, like an input field.
        let avail = (inner.width as usize).saturating_sub(2); // "+ " prefix
        let chars = input.chars().count();
        if chars > avail && avail > 0 {
            input = std::iter::once('…')
                .chain(input.chars().skip(chars + 1 - avail))
                .collect();
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(
                "+ ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                input,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), inner);
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

pub(crate) fn render_tmux_pane(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.92);
    frame.render_widget(Clear, popup_area);

    let Some(pane) = app.tmux_pane.as_mut() else {
        return;
    };

    let title = format!(" tmux · {} ", pane.session_name);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(120, 140, 180)))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " F1 detach & close ",
            Style::default().fg(DIM_TEXT),
        ));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    pane.resize(inner.height, inner.width);
    pane.set_viewport_origin(inner.x, inner.y);

    let Ok(guard) = pane.parser.lock() else {
        return;
    };
    let term = tui_term::widget::PseudoTerminal::new(guard.screen());
    frame.render_widget(term, inner);
}

pub(crate) fn render_confirm_close(frame: &mut Frame, area: Rect, app: &App) {
    // The same view handles destructive/interrupting confirmations:
    // registry-level project removal, project-task deletion, orchestrator
    // restart, and session close. Project-delete wins precedence because
    // it's the biggest blast radius if multiple actions somehow got staged.
    let (title, display, consequence, action_color) = match app.pending_confirm.as_ref() {
        Some(PendingConfirm::ProjectDelete(pending)) => (
            " Delete project? ",
            pending.display.clone(),
            "Removes this project from cc-hub and deletes its hub state. The repository directory is not deleted.",
            Color::Red,
        ),
        Some(PendingConfirm::TaskDelete(pending)) => (
            " Delete task? ",
            pending.display.clone(),
            "Kills the orchestrator if it is live and removes this task's state directory. Worker sessions are left alone.",
            Color::Red,
        ),
        Some(PendingConfirm::TaskRestart(pending)) => (
            " Restart orchestrator? ",
            pending.display.clone(),
            "Kills the current orchestrator if it is live, then starts a new one from the original task prompt. Task history is preserved.",
            Color::Yellow,
        ),
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

pub(crate) fn render_state_debug(frame: &mut Frame, area: Rect, app: &App) {
    let popup_area = centered_rect(area, 0.9);
    frame.render_widget(Clear, popup_area);

    let Some((info, exp)) = app.state_debug.as_ref() else {
        frame.render_widget(popup_block(" Why this state? — loading… "), popup_area);
        return;
    };

    let title = format!(
        " Why · {} · PID {} · state {} ",
        info.project_name, info.pid, exp.final_state
    );
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(state_color(&exp.final_state))
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " j/k scroll · esc/q close ",
        Style::default().fg(Color::DarkGray),
    ));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let total_lines = app.state_debug_lines.len() as u16;

    let scroll_info = format!(
        " {}/{} ",
        (app.state_debug_scroll as usize).min(total_lines.saturating_sub(1) as usize) + 1,
        total_lines
    );
    let indicator_area = Rect::new(
        inner.x,
        popup_area.y + popup_area.height - 1,
        inner.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            scroll_info,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Right),
        indicator_area,
    );

    let content = Paragraph::new(app.state_debug_lines.clone())
        .wrap(Wrap { trim: false })
        .scroll((app.state_debug_scroll, 0));
    frame.render_widget(content, inner);
}

pub fn build_state_debug_content(info: &SessionInfo, exp: &StateExplanation) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let final_color = state_color(&exp.final_state);

    lines.push(Line::from(vec![
        Span::styled("Final state: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", exp.final_state),
            Style::default()
                .fg(final_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    let path_str = info
        .jsonl_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(no jsonl)".to_string());
    lines.push(Line::from(vec![
        Span::styled("JSONL:       ", Style::default().fg(Color::DarkGray)),
        Span::styled(path_str, Style::default().fg(Color::White)),
    ]));

    lines.push(Line::from(vec![
        Span::styled("Tail size:   ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} entries (last 64 KiB)", exp.entry_count),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("mtime age:   ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            exp.mtime_age_secs
                .map_or("unknown".to_string(), |s| format!("{}s", s)),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "─── decision tree ───",
        Style::default().fg(GRAY_80),
    )));
    lines.push(Line::raw(""));

    for step in &exp.steps {
        let (tag, tag_color) = match &step.verdict {
            Verdict::Decided(s) => (format!("DECIDE → {}", s), state_color(s)),
            Verdict::Passed => ("PASS".to_string(), Color::Green),
            Verdict::Skipped => ("SKIP".to_string(), Color::Rgb(90, 90, 100)),
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("[{}] ", tag),
                Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                step.name.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        for d in &step.details {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(d.clone(), Style::default().fg(Color::Rgb(190, 190, 200))),
            ]));
        }
        lines.push(Line::raw(""));
    }

    lines.push(Line::from(Span::styled(
        "─── tail entries (most recent last) ───",
        Style::default().fg(GRAY_80),
    )));
    lines.push(Line::raw(""));

    for e in &exp.tail {
        let blocks = if e.blocks.is_empty() {
            String::new()
        } else {
            format!(" [{}]", e.blocks.join(", "))
        };
        let stop = e
            .stop_reason
            .as_ref()
            .map(|s| format!(" stop={}", s))
            .unwrap_or_default();
        let ts = e.timestamp.as_deref().unwrap_or("        ");
        lines.push(Line::from(vec![
            Span::styled(format!("  {:>3}  ", e.idx), Style::default().fg(GRAY_80)),
            Span::styled(format!("{}  ", ts), Style::default().fg(Color::DarkGray)),
            Span::styled(e.kind.clone(), Style::default().fg(Color::Cyan)),
            Span::styled(stop, Style::default().fg(Color::Yellow)),
            Span::styled(blocks, Style::default().fg(CONTEXT_GRAY)),
        ]));
    }

    lines
}

pub(crate) fn render_live_tail(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.85);

    frame.render_widget(Clear, popup_area);

    let lv = match &mut app.live_view {
        Some(lv) => lv,
        None => return,
    };

    let (title, status_color) = if lv.review_mode {
        (" Transcript · review ", Color::Rgb(230, 180, 90))
    } else if lv.auto_scroll {
        (" Live Tail · streaming ", Color::Green)
    } else {
        (" Live Tail · paused ", Color::Yellow)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(80, 120, 150)))
        .title(Span::styled(
            title,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 3 || inner.width < 2 {
        return;
    }

    let content_area = Rect::new(
        inner.x + 1,
        inner.y + 1,
        inner.width.saturating_sub(1),
        inner.height.saturating_sub(2),
    );

    let (lines, highlight_range) = build_live_tail_content(&lv.messages, lv.highlight_msg_idx);
    let total_lines = lines.len() as u16;

    lv.total_content_lines = total_lines;

    if lv.auto_scroll && total_lines > content_area.height {
        lv.scroll = total_lines.saturating_sub(content_area.height);
    }

    // One-shot: consuming the flag lets manual scrolls stick afterwards.
    // If the highlight didn't resolve, clear the flag anyway so we don't
    // keep retrying on every frame.
    if lv.scroll_to_highlight.is_some() {
        if let Some((start, _end)) = highlight_range {
            let h = content_area.height.max(1);
            let target = (start as u16).saturating_sub(h / 3);
            let max_scroll = total_lines.saturating_sub(h);
            lv.scroll = target.min(max_scroll);
            lv.scroll_to_highlight = None;
        } else if !lv.messages.is_empty() {
            lv.scroll_to_highlight = None;
        }
    }

    let max_scroll = total_lines.saturating_sub(content_area.height);
    if lv.scroll > max_scroll {
        lv.scroll = max_scroll;
    }

    let content = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((lv.scroll, 0));
    frame.render_widget(content, content_area);

    let bottom_y = popup_area.y + popup_area.height - 1;

    let hint_text = " ↑/↓ scroll · G bottom · esc close ";
    let hint_width = (hint_text.chars().count() as u16).min(inner.width);
    let hint = Paragraph::new(Line::from(Span::styled(
        hint_text,
        Style::default().fg(DIM_TEXT),
    )));
    let hint_area = Rect::new(inner.x, bottom_y, hint_width, 1);
    frame.render_widget(hint, hint_area);

    // Scroll indicator on the right of the bottom border
    let scroll_info = format!(
        " {}/{} ",
        (lv.scroll as usize).min(total_lines.saturating_sub(1) as usize) + 1,
        total_lines
    );
    let indicator = Paragraph::new(Line::from(Span::styled(
        scroll_info,
        Style::default().fg(DIM_TEXT),
    )))
    .alignment(ratatui::layout::Alignment::Right);

    let indicator_area = Rect::new(inner.x, bottom_y, inner.width, 1);
    frame.render_widget(indicator, indicator_area);
}

pub(crate) fn build_live_tail_content(
    messages: &[crate::models::ConversationMessage],
    highlight_msg_idx: Option<usize>,
) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut highlight_range: Option<(usize, usize)> = None;

    if messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "Waiting for messages…",
            Style::default().fg(Color::DarkGray),
        )));
        return (lines, highlight_range);
    }

    let separate = |lines: &mut Vec<Line<'static>>| {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
    };

    // Running session cost. cache_read tokens are the reused prefix — they
    // do count toward the billed input_tokens of the call, but summing them
    // across turns double-counts the same cached prefix over and over, so
    // billed_in tracks only the *new* input contributed by each turn.
    let mut billed_in: u64 = 0;
    let mut total_out: u64 = 0;

    for (idx, msg) in messages.iter().enumerate() {
        if msg.role == "system" {
            continue;
        }

        if msg.role == "user" {
            let content = msg.content_preview.trim();
            if is_placeholder_preview(content) {
                continue;
            }
            separate(&mut lines);
            render_prompt_block(&mut lines, content);
            continue;
        }

        if msg.role == "assistant" {
            let is_peak = highlight_msg_idx == Some(idx);
            let start = lines.len();
            if is_peak {
                separate(&mut lines);
                lines.push(Line::from(Span::styled(
                    "  ◆ peak context-growth turn".to_string(),
                    Style::default()
                        .fg(Color::Rgb(20, 20, 20))
                        .bg(Color::Rgb(230, 180, 90))
                        .add_modifier(Modifier::BOLD),
                )));
            }
            for part in parse_preview(&msg.content_preview) {
                separate(&mut lines);
                match part {
                    PreviewPart::Thinking => render_thinking(&mut lines),
                    PreviewPart::Tool(name) => render_tool_bullet(&mut lines, &name),
                    PreviewPart::Text(text) => render_asst_bullet(&mut lines, &text),
                }
            }

            let input = msg.input_tokens.unwrap_or(0);
            let output = msg.output_tokens.unwrap_or(0);
            let cache_read = msg.cache_read_input_tokens.unwrap_or(0);
            let cache_create = msg.cache_creation_input_tokens.unwrap_or(0);
            let ctx = input + cache_read + cache_create;
            let turn_new = input + cache_create;

            billed_in = billed_in.saturating_add(turn_new);
            total_out = total_out.saturating_add(output);

            if ctx > 0 || output > 0 {
                render_turn_stats(&mut lines, turn_new, output, ctx, billed_in, total_out);
            }

            if is_peak && highlight_range.is_none() {
                highlight_range = Some((start, lines.len()));
            }
        }
    }

    (lines, highlight_range)
}

pub(crate) fn render_turn_stats(
    lines: &mut Vec<Line<'static>>,
    turn_in: u64,
    turn_out: u64,
    ctx: u64,
    cum_in: u64,
    cum_out: u64,
) {
    let dim = Style::default().fg(Color::Rgb(95, 95, 115));
    let accent = Style::default().fg(Color::Rgb(170, 150, 205));
    let ctx_accent = Style::default()
        .fg(Color::Rgb(200, 175, 230))
        .add_modifier(Modifier::BOLD);

    let mut spans = vec![
        Span::styled("  └─ ctx ", dim),
        Span::styled(format_tokens(ctx), ctx_accent),
    ];
    spans.push(Span::styled("  · turn +", dim));
    spans.push(Span::styled(format_tokens(turn_in), accent));
    spans.push(Span::styled(" in / ", dim));
    spans.push(Span::styled(format_tokens(turn_out), accent));
    spans.push(Span::styled(" out  · Σ ", dim));
    spans.push(Span::styled(format_tokens(cum_in), accent));
    spans.push(Span::styled(" in / ", dim));
    spans.push(Span::styled(format_tokens(cum_out), accent));
    spans.push(Span::styled(" out", dim));
    lines.push(Line::from(spans));
}

pub(crate) fn is_placeholder_preview(s: &str) -> bool {
    s.is_empty()
        || s == crate::conversation::NO_CONTENT
        || s == crate::conversation::NO_TEXT_CONTENT
}

#[derive(Debug, Clone)]
pub(crate) enum PreviewPart {
    Thinking,
    Tool(String),
    Text(String),
}

/// Tokenize a preview back into the parts that produced it. The marker
/// format is defined by `extract_text_content` in conversation.rs — keep
/// the two in sync via the shared marker constants.
pub(crate) fn parse_preview(preview: &str) -> Vec<PreviewPart> {
    use crate::conversation::{THINKING_MARKER, TOOL_MARKER_PREFIX};

    let mut out = Vec::new();
    if is_placeholder_preview(preview) {
        return out;
    }
    let mut rest = preview;
    loop {
        let t_idx = rest.find(THINKING_MARKER);
        let u_idx = rest.find(TOOL_MARKER_PREFIX);
        let next = match (t_idx, u_idx) {
            (None, None) => None,
            (Some(a), None) => Some((a, true)),
            (None, Some(b)) => Some((b, false)),
            (Some(a), Some(b)) => Some(if a < b { (a, true) } else { (b, false) }),
        };
        let Some((idx, is_thinking)) = next else {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                out.push(PreviewPart::Text(trimmed.to_string()));
            }
            return out;
        };

        let before = rest[..idx].trim();
        if !is_placeholder_preview(before) && !before.is_empty() {
            out.push(PreviewPart::Text(before.to_string()));
        }

        if is_thinking {
            out.push(PreviewPart::Thinking);
            rest = &rest[idx + THINKING_MARKER.len()..];
        } else {
            let after = &rest[idx + TOOL_MARKER_PREFIX.len()..];
            let Some(end) = after.find(']') else {
                return out;
            };
            let name = after[..end].trim();
            if !name.is_empty() {
                out.push(PreviewPart::Tool(name.to_string()));
            }
            rest = &after[end + 1..];
        }
    }
}

pub(crate) fn render_prompt_block(lines: &mut Vec<Line<'static>>, body: &str) {
    push_bullet_block(
        lines,
        Span::styled(
            "> ",
            Style::default()
                .fg(Color::Rgb(230, 230, 240))
                .add_modifier(Modifier::BOLD),
        ),
        Color::Rgb(230, 230, 240),
        body,
    );
}

pub(crate) fn render_asst_bullet(lines: &mut Vec<Line<'static>>, body: &str) {
    push_bullet_block(
        lines,
        Span::styled("● ", Style::default().fg(Color::Green)),
        Color::Rgb(220, 220, 230),
        body,
    );
}

pub(crate) fn render_tool_bullet(lines: &mut Vec<Line<'static>>, display: &str) {
    let mut spans = vec![Span::styled(
        "● ",
        Style::default().fg(Color::Rgb(140, 180, 210)),
    )];
    match display.find('(') {
        Some(paren) => {
            let (name, rest) = display.split_at(paren);
            spans.push(Span::styled(
                name.to_string(),
                Style::default()
                    .fg(Color::Rgb(200, 215, 230))
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                rest.to_string(),
                Style::default().fg(Color::Rgb(130, 150, 170)),
            ));
        }
        None => {
            spans.push(Span::styled(
                display.to_string(),
                Style::default()
                    .fg(Color::Rgb(200, 215, 230))
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    lines.push(Line::from(spans));
}

pub(crate) fn render_thinking(lines: &mut Vec<Line<'static>>) {
    lines.push(Line::from(vec![
        Span::styled("✻ ", Style::default().fg(Color::Rgb(190, 150, 210))),
        Span::styled(
            "thinking…",
            Style::default()
                .fg(Color::Rgb(160, 140, 180))
                .add_modifier(Modifier::ITALIC),
        ),
    ]));
}

// Continuation lines indent two spaces so body text lines up under the prefix.
pub(crate) fn push_bullet_block(
    lines: &mut Vec<Line<'static>>,
    prefix: Span<'static>,
    body_color: Color,
    body: &str,
) {
    let mut prefix = Some(prefix);
    for body_line in body.lines() {
        let trimmed = body_line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        let prefix_span = prefix.take().unwrap_or_else(|| Span::raw("  "));
        lines.push(Line::from(vec![
            prefix_span,
            Span::styled(trimmed.to_string(), Style::default().fg(body_color)),
        ]));
    }
    if let Some(p) = prefix {
        lines.push(Line::from(p));
    }
}
