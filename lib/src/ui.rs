use crate::app::{status_msg_ttl, App, PendingConfirm, Tab, View, TABS};
use crate::config;
use crate::conversation::{StateExplanation, Verdict};
use crate::folder_picker::PickerMode;
use crate::metrics::{MetricsAnalysis, ModelStats, SessionSummary, ToolStats};
use crate::models;
use crate::models::{short_sid, SessionDetail, SessionInfo, SessionState};
use crate::orchestrator::Artifact;
use crate::usage::UsageInfo;
use chrono::Duration as ChronoDuration;
use chrono::{DateTime, Local, TimeZone};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::StatefulImage;
use std::borrow::Cow;
use std::path::Path;

/// Shared dark band background painted by the tab strip, project chip strip,
/// and contention strip — keeping these identical avoids a visible seam when
/// rows abut.
const BAND_BG: Color = Color::Rgb(20, 20, 28);

fn cell_height() -> u16 {
    config::get().ui.cell_height.max(1)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Top-level vertical split: title bar, tab strip, body, status bar. Shared
/// between `render` and overlays that anchor to the body region (e.g. the
/// to-do side panel) so the band heights are defined in exactly one place.
fn main_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    app.update_grid_cols(frame.area().width);

    let chunks = main_layout(frame.area());

    render_title_bar(frame, chunks[0], app);
    render_tab_strip(frame, chunks[1], app);
    match app.current_tab {
        Tab::Projects => render_projects_body(frame, chunks[2], app),
        Tab::Sessions => render_grid(frame, chunks[2], app),
        Tab::Metrics => render_metrics_body(frame, chunks[2], app),
    }
    render_status_bar(frame, chunks[3], app);

    match app.view {
        View::Popup => render_popup(frame, frame.area(), app),
        View::LiveTail => render_live_tail(frame, frame.area(), app),
        View::ConfirmClose => render_confirm_close(frame, frame.area(), app),
        View::StateDebug => render_state_debug(frame, frame.area(), app),
        View::PromptInput => render_prompt_input(frame, frame.area(), app),
        View::RenameSession => render_rename_session(frame, frame.area(), app),
        View::TmuxPane => render_tmux_pane(frame, frame.area(), app),
        View::FolderPicker => render_folder_picker(frame, frame.area(), app),
        View::GhCreateInput => {
            render_folder_picker(frame, frame.area(), app);
            render_gh_create_input(frame, frame.area(), app);
        }
        View::ProjectsResult => render_projects_result(frame, frame.area(), app),
        View::Backlog => render_backlog(frame, frame.area(), app),
        View::TodoPanel => render_todo_panel(frame, frame.area(), app),
        View::Grid => {}
    }
}

fn render_tab_strip(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let bg = Style::default().bg(BAND_BG);

    // Paint the full band (top padding + tabs row + bottom padding) so the
    // background colour reads as a continuous header strip.
    frame.render_widget(Paragraph::new("").style(bg), area);

    let mut spans: Vec<Span<'static>> = vec![Span::styled("  ", bg)];
    for (i, tab) in TABS.iter().enumerate() {
        let is_active = *tab == app.current_tab;
        let (fg, bgc, modi) = if is_active {
            (Color::Black, Color::Rgb(180, 200, 230), Modifier::BOLD)
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
        if i + 1 < TABS.len() {
            spans.push(Span::styled(" ", bg));
        }
    }
    spans.push(Span::styled(
        "   ⇥ next tab",
        Style::default().fg(Color::Rgb(80, 80, 95)).bg(BAND_BG),
    ));

    // Tabs go on the visual middle row (or first row if the band is shorter).
    let row_y = area.y + area.height / 2;
    let row_area = Rect::new(area.x, row_y, area.width, 1);
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), row_area);
}

fn render_folder_picker(frame: &mut Frame, area: Rect, app: &App) {
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
    .title_bottom(Span::styled(
        footer_text,
        Style::default().fg(Color::Rgb(110, 110, 130)),
    ));
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

fn render_gh_create_input(frame: &mut Frame, area: Rect, app: &App) {
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
        Style::default().fg(Color::Rgb(110, 110, 130)),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 4 || inner.width == 0 {
        return;
    }

    let cwd_line = Line::from(vec![
        Span::styled(" in ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            input.cwd.clone(),
            Style::default().fg(Color::Rgb(180, 200, 230)),
        ),
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

fn render_prompt_input(frame: &mut Frame, area: Rect, app: &App) {
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

/// Word-wrap `text` to `width` columns for the to-do panel: break on
/// whitespace, hard-split any single word longer than `width`, and count each
/// char as one column (matching the add-input's `chars().count()` budget).
/// Always returns at least one line so an empty item still occupies a row.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if wlen > width {
            // Word can't fit on any line — flush what we have, then chop it
            // into width-sized chunks so it still shows in full.
            if cur_len > 0 {
                lines.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
            for ch in word.chars() {
                if cur_len == width {
                    lines.push(std::mem::take(&mut cur));
                    cur_len = 0;
                }
                cur.push(ch);
                cur_len += 1;
            }
            continue;
        }
        let need = if cur_len == 0 { wlen } else { cur_len + 1 + wlen };
        if need > width {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
            cur_len = wlen;
        } else {
            if cur_len > 0 {
                cur.push(' ');
                cur_len += 1;
            }
            cur.push_str(word);
            cur_len += wlen;
        }
    }
    if cur_len > 0 || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Scratch to-do list, drawn as a right-anchored side panel over the body
/// region so the tab strip and status bar (which carries the panel's own key
/// hints) stay visible behind it.
fn render_todo_panel(frame: &mut Frame, area: Rect, app: &App) {
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
            " a add · space toggle · d delete · c clear done · esc close "
        },
        Style::default().fg(Color::Rgb(110, 110, 130)),
    ));

    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Reserve the bottom two rows (spacer + input) in add mode so a long list
    // can never push the input line off-screen.
    let input_rows = if app.todo_adding { 2usize } else { 0 };
    let list_rows = (inner.height as usize).saturating_sub(input_rows);

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
        // Wrap each item to the panel width, indenting continuation rows under
        // the text so they line up past the cursor + checkbox. An item now
        // spans a variable number of screen rows, so the scroll window counts
        // rows (not items) and we track where each item's rows begin.
        const PREFIX_W: usize = 6; // "  " cursor + "[ ] " checkbox
        let text_width = (inner.width as usize).saturating_sub(PREFIX_W);
        let sel = app.todo_selected.min(total.saturating_sub(1));

        let mut rows: Vec<Line> = Vec::new();
        let mut item_start: Vec<usize> = Vec::with_capacity(total);
        for (i, item) in app.todo.items().iter().enumerate() {
            item_start.push(rows.len());
            let selected = !app.todo_adding && i == sel;
            let cursor = if selected { "› " } else { "  " };
            let checkbox = if item.done { "[x] " } else { "[ ] " };
            let text_style = if item.done {
                Style::default()
                    .fg(Color::Rgb(110, 110, 130))
                    .add_modifier(Modifier::ITALIC)
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
                    .fg(Color::Rgb(180, 200, 230))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            for (j, seg) in wrap_text(&item.text, text_width).into_iter().enumerate() {
                if j == 0 {
                    rows.push(Line::from(vec![
                        Span::styled(cursor, marker_style),
                        Span::styled(checkbox, marker_style),
                        Span::styled(seg, text_style),
                    ]));
                } else {
                    rows.push(Line::from(vec![
                        Span::raw(" ".repeat(PREFIX_W)),
                        Span::styled(seg, text_style),
                    ]));
                }
            }
        }

        // Scroll so the selected item is visible: pull its bottom edge into
        // view, but never past its top, so an item taller than the window
        // shows from the top down.
        let total_rows = rows.len();
        let sel_start = item_start.get(sel).copied().unwrap_or(0);
        let sel_end = item_start.get(sel + 1).copied().unwrap_or(total_rows);
        let mut scroll = 0usize;
        if total_rows > list_rows {
            if sel_end > list_rows {
                scroll = sel_end - list_rows;
            }
            scroll = scroll.min(sel_start).min(total_rows - list_rows);
        }
        lines.extend(rows.into_iter().skip(scroll).take(list_rows));
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

fn render_rename_session(frame: &mut Frame, area: Rect, app: &App) {
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

fn render_backlog(frame: &mut Frame, area: Rect, app: &App) {
    let popup = centered_fixed(area, 120, 30);
    frame.render_widget(Clear, popup);

    let tasks = app.backlog_tasks();
    let project_name = app
        .selected_project()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "no project".to_string());
    let title_text = format!(" Backlog · {} ", project_name);
    let selected_label = if tasks.is_empty() {
        "".to_string()
    } else {
        format!(
            " · {}/{} ",
            app.backlog_sel.min(tasks.len() - 1) + 1,
            tasks.len()
        )
    };
    let block = popup_block(Span::styled(
        title_text,
        Style::default()
            .fg(Color::Rgb(120, 140, 200))
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        format!(
            " j/k navigate · s/enter start · x delete · esc/q close{}",
            selected_label
        ),
        Style::default().fg(Color::DarkGray),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if tasks.is_empty() {
        let empty = Paragraph::new(vec![
            Line::from(Span::styled(
                "No backlog tasks for this project.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "Queue one with: cc-hub task create --backlog --prompt \"…\"",
                Style::default().fg(Color::Rgb(80, 80, 90)),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(empty, inner);
        return;
    }

    let (list_area, body_area) = if inner.width >= 60 {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(40), Constraint::Min(0)])
            .split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };

    let max_w = list_area.width.saturating_sub(4) as usize;
    let rows_per_task = 3usize;
    let visible_tasks = ((list_area.height as usize) / rows_per_task).max(1);
    let sel = app.backlog_sel.min(tasks.len() - 1);
    let scroll_top = if tasks.len() <= visible_tasks || sel < visible_tasks {
        0
    } else {
        sel + 1 - visible_tasks
    };
    let mut lines: Vec<Line> = Vec::with_capacity(visible_tasks * rows_per_task);
    let now_secs = now_ms() / 1000;
    for (i, t) in tasks
        .iter()
        .enumerate()
        .skip(scroll_top)
        .take(visible_tasks)
    {
        let selected = i == sel;
        let arrow = if selected { "▌ " } else { "  " };
        let has_title = t.title.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        let id_short = crate::orchestrator::short_task_id(&t.task_id);
        let title_style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let age_secs = now_secs.saturating_sub(t.created_at as u64);
        let age = format!("{:>4}", models::relative_age_short(age_secs));
        if has_title {
            let title_text = t.title.as_deref().unwrap().to_string();
            lines.push(Line::from(vec![
                Span::styled(arrow, Style::default().fg(Color::Rgb(120, 140, 200))),
                Span::styled(title_text, title_style),
            ]));
            let preview = models::first_line_truncated(
                &t.prompt,
                max_w.saturating_sub(id_short.len() + age.len() + 8),
            );
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(id_short, Style::default().fg(Color::DarkGray)),
                Span::styled("  ", Style::default()),
                Span::styled(age, Style::default().fg(TASK_META_DIM)),
                Span::styled("  ", Style::default()),
                Span::styled(preview, Style::default().fg(Color::Rgb(110, 110, 130))),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(arrow, Style::default().fg(Color::Rgb(120, 140, 200))),
                Span::styled(format!("#{}", id_short), title_style),
                Span::styled(" · pending title", Style::default().fg(Color::DarkGray)),
            ]));
            let preview =
                models::first_line_truncated(&t.prompt, max_w.saturating_sub(age.len() + 6));
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(age, Style::default().fg(TASK_META_DIM)),
                Span::styled("  ", Style::default()),
                Span::styled(preview, Style::default().fg(Color::Rgb(110, 110, 130))),
            ]));
        }
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), list_area);

    if let (Some(body_area), Some(task)) = (body_area, tasks.get(sel)) {
        let separator = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(Color::Rgb(60, 60, 80)));
        let body_inner = separator.inner(body_area);
        frame.render_widget(separator, body_area);

        let mut body_lines: Vec<Line> = Vec::new();
        if let Some(title) = task.title.as_deref().filter(|s| !s.is_empty()) {
            body_lines.push(Line::from(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        body_lines.push(Line::from(Span::styled(
            crate::orchestrator::short_task_id(&task.task_id),
            Style::default().fg(Color::DarkGray),
        )));
        body_lines.push(Line::raw(""));
        for line in task.prompt.lines() {
            body_lines.push(Line::from(Span::styled(
                line,
                Style::default().fg(Color::Rgb(180, 180, 200)),
            )));
        }
        frame.render_widget(
            Paragraph::new(body_lines).wrap(Wrap { trim: false }),
            body_inner,
        );
    }
}

fn render_tmux_pane(frame: &mut Frame, area: Rect, app: &mut App) {
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
            Style::default().fg(Color::Rgb(110, 110, 130)),
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

fn render_confirm_close(frame: &mut Frame, area: Rect, app: &App) {
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

fn render_title_bar(frame: &mut Frame, area: Rect, app: &App) {
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
        right_spans.push(Span::styled(
            " │ ",
            Style::default().fg(Color::Rgb(60, 60, 70)),
        ));
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

fn build_session_count_spans(c: &crate::session_count::SessionCounts) -> Vec<Span<'static>> {
    if c.today == 0 && c.week == 0 {
        return Vec::new();
    }
    let label_style = Style::default().fg(Color::DarkGray);
    let num_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let sep_style = Style::default().fg(Color::Rgb(60, 60, 70));
    vec![
        Span::styled(" today ", label_style),
        Span::styled(c.today.to_string(), num_style),
        Span::styled(" │ ", sep_style),
        Span::styled("this wk ", label_style),
        Span::styled(c.week.to_string(), num_style),
    ]
}

pub fn build_usage_line(u: &UsageInfo) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    let label_style = Style::default().fg(Color::DarkGray);
    let reset_style = Style::default().fg(Color::Rgb(90, 90, 100));
    let sep_style = Style::default().fg(Color::Rgb(60, 60, 70));
    let pct_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    spans.push(Span::styled(" 5h", label_style));
    if let Some(fmt) = u
        .five_hour_resets_at
        .as_deref()
        .and_then(|s| format_reset(s, "%-l%p"))
    {
        spans.push(Span::styled(format!(" {}", fmt), reset_style));
    }
    spans.push(Span::raw(" "));
    append_bar(&mut spans, u.five_hour_pct, 10);
    spans.push(Span::styled(format!(" {}%", u.five_hour_pct), pct_style));

    spans.push(Span::styled(" │ ", sep_style));

    spans.push(Span::styled("wk", label_style));
    if let Some(fmt) = u
        .seven_day_resets_at
        .as_deref()
        .and_then(|s| format_reset(s, "%a %-l%p"))
    {
        spans.push(Span::styled(format!(" {}", fmt), reset_style));
    }
    spans.push(Span::raw(" "));
    append_bar(&mut spans, u.seven_day_pct, 10);
    spans.push(Span::styled(format!(" {}% ", u.seven_day_pct), pct_style));

    Line::from(spans)
}

fn append_bar(spans: &mut Vec<Span<'static>>, pct: u8, width: u16) {
    let pct = pct.min(100);
    let mut filled = (pct as u16 * width) / 100;
    if pct > 0 && filled == 0 {
        filled = 1;
    }
    let empty = width - filled;
    let color = bar_color(pct);
    let filled_s: String = "━".repeat(filled as usize);
    let empty_s: String = "╌".repeat(empty as usize);
    spans.push(Span::styled(filled_s, Style::default().fg(color)));
    spans.push(Span::styled(empty_s, Style::default().fg(color)));
}

fn bar_color(pct: u8) -> Color {
    if pct > 80 {
        Color::Red
    } else if pct >= 50 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn format_reset(iso: &str, fmt: &str) -> Option<String> {
    let dt = DateTime::parse_from_rfc3339(iso).ok()?;
    Some(
        dt.with_timezone(&Local)
            .format(fmt)
            .to_string()
            .to_lowercase(),
    )
}

const GROUP_HEADER_HEIGHT: u16 = 1;
const GROUP_GAP: u16 = 1;

fn render_grid(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.groups.is_empty() {
        let empty = Paragraph::new("No sessions found. Start an agent session to see it here.")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, area);
        return;
    }

    let cols = app.grid_cols as usize;
    let cell_width = area.width / app.grid_cols;

    // Compute content-space y offset for each group
    let mut group_offsets: Vec<u16> = Vec::new();
    let mut y_acc: u16 = 0;
    for group in &app.groups {
        group_offsets.push(y_acc);
        let rows = group.sessions.len().div_ceil(cols) as u16;
        y_acc = y_acc.saturating_add(GROUP_HEADER_HEIGHT + rows * cell_height() + GROUP_GAP);
    }

    // Auto-scroll to keep selected card visible (prefer showing group header too)
    {
        let g_offset = group_offsets[app.sel_group];
        let card_row = (app.sel_in_group / cols) as u16;
        let card_y = g_offset + GROUP_HEADER_HEIGHT + card_row * cell_height();
        let card_bottom = card_y + cell_height();

        if card_bottom.saturating_sub(g_offset) <= area.height {
            // Both header and card fit — keep both visible
            if g_offset < app.grid_scroll {
                app.grid_scroll = g_offset;
            } else if card_bottom > app.grid_scroll + area.height {
                app.grid_scroll = card_bottom.saturating_sub(area.height);
            }
        } else {
            // Just ensure the card itself is visible
            if card_y < app.grid_scroll {
                app.grid_scroll = card_y;
            } else if card_bottom > app.grid_scroll + area.height {
                app.grid_scroll = card_bottom.saturating_sub(area.height);
            }
        }
    }

    let scroll = app.grid_scroll;
    let now = now_ms();
    // Build the tmux→role index once per frame; per-card lookup was
    // O(projects × tasks × workers) and dominated re-render cost on hosts
    // with many tasks.
    let roles_by_tmux = app.projects.roles_by_tmux();

    for (gi, group) in app.groups.iter().enumerate() {
        let g_y = group_offsets[gi];

        // Render group header
        let header_sy = g_y as i32 - scroll as i32;
        if header_sy >= 0 && header_sy < area.height as i32 {
            let hy = area.y + header_sy as u16;
            let total = group.sessions.len();
            let attn = group
                .sessions
                .iter()
                .filter(|s| s.needs_attention())
                .count();

            let mut spans = vec![
                Span::styled(
                    format!(" 󰉋 {} ", group.name),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {} sessions", total),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            if attn > 0 {
                spans.push(Span::styled(
                    format!("  󰂞 {}", attn),
                    Style::default().fg(Color::Yellow),
                ));
            }
            // Show cwd path dimmed after the counts
            spans.push(Span::styled(
                format!("  {}", group.cwd),
                Style::default().fg(Color::Rgb(60, 60, 70)),
            ));

            let header = Paragraph::new(Line::from(spans));
            frame.render_widget(header, Rect::new(area.x, hy, area.width, 1));
        }

        // Render cards for this group
        for (si, session) in group.sessions.iter().enumerate() {
            let col = (si % cols) as u16;
            let row = (si / cols) as u16;

            let card_cy = g_y + GROUP_HEADER_HEIGHT + row * cell_height();
            let card_sy = card_cy as i32 - scroll as i32;

            // Only render if fully visible within the area
            if card_sy < 0 || card_sy + cell_height() as i32 > area.height as i32 {
                continue;
            }

            let x = area.x + col * cell_width;
            let cy = area.y + card_sy as u16;
            let w = if col == app.grid_cols - 1 {
                area.x + area.width - x
            } else {
                cell_width
            };

            let is_selected = gi == app.sel_group && si == app.sel_in_group;
            let cell_area = Rect::new(x, cy, w, cell_height());
            let role = session
                .tmux_session
                .as_deref()
                .and_then(|t| roles_by_tmux.get(t));
            render_card(frame, cell_area, session, role, is_selected, now);
        }
    }
}

fn render_card(
    frame: &mut Frame,
    area: Rect,
    session: &SessionInfo,
    role: Option<&crate::projects_scan::SessionRole>,
    selected: bool,
    now: u64,
) {
    let (indicator, ind_color) = state_indicator(&session.state);

    let border_color = if selected {
        Color::White
    } else if session.needs_attention() || session.state == SessionState::Processing {
        // Question gets its own blue accent so it's visually distinct from
        // a generic WaitingForInput card — same source of truth as the
        // state indicator icon. Processing mirrors that (green frame) so
        // active sessions read as "alive" at a glance, not as ambient.
        state_color(&session.state)
    } else {
        Color::Rgb(60, 60, 70)
    };

    let border_type = if selected {
        BorderType::Double
    } else if session.state == SessionState::Inactive {
        BorderType::LightDoubleDashed
    } else {
        BorderType::Rounded
    };

    // Role badge — prepended into the title so a glance tells the user
    // whether a card is an orchestrator or a worker, and which task it's
    // attached to. Workers also get their worktree name (or "RO" for
    // read-only). None for ordinary sessions.
    let role_prefix = match role {
        Some(crate::projects_scan::SessionRole::Orchestrator { task_id, .. }) => Some(format!(
            "★ orch[{}] ",
            crate::orchestrator::short_task_id(task_id)
        )),
        Some(crate::projects_scan::SessionRole::Worker {
            task_id,
            worktree,
            readonly,
            ..
        }) => {
            let suffix = if *readonly {
                "RO".to_string()
            } else {
                worktree.clone().unwrap_or_else(|| "wt".into())
            };
            Some(format!(
                "↳ wkr[{}/{}] ",
                crate::orchestrator::short_task_id(task_id),
                suffix
            ))
        }
        None => None,
    };
    let prefix = role_prefix.as_deref().unwrap_or("");
    // Claude is the ~99% default — labelling every card "[Claude]" is pure
    // noise — so the badge is shown only for non-Claude agents.
    let agent_badge = if session.agent_id == "claude" {
        String::new()
    } else {
        format!("[{}] ", session.agent_badge())
    };

    // Border title is the primary skim surface — prepending the Haiku-
    // generated 2-3 word title when available lets users scan what each
    // session is about without having to read the (truncated, often mid-
    // sentence) last user message inside the card body. A `✎` placeholder
    // marks cards with an in-flight Haiku call so the user can tell a
    // pending title from one that's never going to arrive. The project name
    // is intentionally absent: it's already the header of the card's group.
    //
    // Every branch keeps a space immediately after `indicator`: the state
    // glyph is a Nerd Font icon that renders two columns wide but measures as
    // one, so without a trailing cell its second column collides with the
    // border (the bare no-title case `󰂞` is where this bit).
    let title = match session.title.as_deref() {
        Some(t) if !t.is_empty() => format!("{}{}{} {}", prefix, agent_badge, indicator, t),
        _ if session.titling => format!("{}{}{} ✎ …", prefix, agent_badge, indicator),
        _ => format!("{}{}{} ", prefix, agent_badge, indicator),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title,
            Style::default().fg(ind_color).add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut lines = Vec::new();

    let branch = session.git_branch.as_deref().unwrap_or("");
    lines.push(Line::from(vec![
        Span::styled(" ", Style::default().fg(Color::Rgb(100, 100, 120))),
        Span::styled(branch.to_string(), Style::default().fg(Color::Cyan)),
        Span::styled(
            format!("  {}:{}", session.pid, short_sid(&session.session_id)),
            Style::default().fg(Color::Rgb(50, 50, 60)),
        ),
    ]));

    let model_short = short_model(session.model.as_deref().unwrap_or(""));
    let duration_str = format_elapsed(now, session.started_at);

    let model_display = format!("󰧑 {}", model_short);
    let duration_display = format!("󰥔 {}", duration_str);
    let inner_w = inner.width as usize;
    let model_cols = 2 + 1 + model_short.len();
    let duration_cols = 2 + 1 + duration_str.len();
    let padding = inner_w
        .saturating_sub(model_cols)
        .saturating_sub(duration_cols);

    lines.push(Line::from(vec![
        Span::styled(model_display, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(padding)),
        Span::styled(duration_display, Style::default().fg(Color::DarkGray)),
    ]));

    // Elapsed (left) + context-window utilisation (right). Tool is rendered
    // on its own row below to give the hint enough room.
    let elapsed_raw = session.last_activity.map(|ts| format_elapsed(now, ts));
    let ctx_label: Option<(String, Color)> = session.context_tokens.map(|ctx| {
        let window = context_window_size(session.model.as_deref().unwrap_or(""));
        let pct = ((ctx as f64 / window as f64) * 100.0).min(999.0);
        let color = if pct >= 90.0 {
            Color::Rgb(220, 120, 120)
        } else if pct >= 70.0 {
            Color::Rgb(220, 200, 120)
        } else {
            Color::DarkGray
        };
        (format!("󰍛 {:.0}% ({})", pct, format_tokens(ctx)), color)
    });

    let elapsed_cols = elapsed_raw.as_ref().map(|s| 2 + 1 + s.len()).unwrap_or(0);
    let ctx_cols = ctx_label
        .as_ref()
        .map(|(s, _)| s.chars().count() + 1)
        .unwrap_or(0);

    let mut state_spans: Vec<Span> = Vec::new();
    if let Some(elapsed) = &elapsed_raw {
        state_spans.push(Span::styled(
            format!("󰔟 {}", elapsed),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some((label, color)) = &ctx_label {
        let padding = inner_w
            .saturating_sub(elapsed_cols)
            .saturating_sub(ctx_cols);
        state_spans.push(Span::raw(" ".repeat(padding)));
        state_spans.push(Span::styled(label.clone(), Style::default().fg(*color)));
    }

    lines.push(Line::from(state_spans));

    // In-flight tool (with input hint) on its own row so long Bash commands /
    // file paths have the full card width to breathe. Tool wins over thinking
    // when both are present — a running tool is always more actionable than
    // recent reasoning.
    let activity: Option<(String, Color)> = if let Some(tool) = session.current_tool.as_ref() {
        Some((
            format_tool_label(tool, inner_w),
            state_color(&session.state),
        ))
    } else if session.is_thinking && session.state == SessionState::Processing {
        Some(("󰛨 Thinking".to_string(), Color::Rgb(170, 140, 210)))
    } else {
        None
    };
    if let Some((label, color)) = activity {
        lines.push(Line::from(vec![Span::styled(
            label,
            Style::default().fg(color),
        )]));
    }

    // The Haiku title in the border already summarises the session — repeating
    // the (often truncated mid-sentence) last user message below it is noise.
    // Only render the message body when no title is available to skim against.
    let display_msg = if session.title.as_deref().is_some_and(|t| !t.is_empty()) {
        None
    } else {
        session
            .last_user_message
            .as_ref()
            .or(session.summary.as_ref())
    };
    if let Some(msg) = display_msg {
        let max_w = inner_w.saturating_sub(3); // account for icon prefix
        let chars: Vec<char> = msg.chars().collect();
        if chars.len() <= max_w {
            lines.push(Line::from(vec![
                Span::styled("󰍡 ", Style::default().fg(Color::Rgb(100, 100, 120))),
                Span::styled(msg.clone(), Style::default().fg(Color::Rgb(160, 160, 170))),
            ]));
        } else {
            let first_line: String = chars[..max_w].iter().collect();
            let remaining: String = chars[max_w..]
                .iter()
                .take(max_w.saturating_sub(3))
                .collect();
            lines.push(Line::from(vec![
                Span::styled("󰍡 ", Style::default().fg(Color::Rgb(100, 100, 120))),
                Span::styled(first_line, Style::default().fg(Color::Rgb(160, 160, 170))),
            ]));
            let second = if chars.len() > max_w.saturating_mul(2).saturating_sub(3) {
                format!("{}...", remaining)
            } else {
                remaining
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(second, Style::default().fg(Color::Rgb(160, 160, 170))),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn popup_block<'a>(title: impl Into<ratatui::text::Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::White))
        .title(title)
}

fn centered_rect(area: Rect, ratio: f32) -> Rect {
    let w = (area.width as f32 * ratio) as u16;
    let h = (area.height as f32 * ratio) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn centered_fixed(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn render_popup(frame: &mut Frame, area: Rect, app: &App) {
    let popup_area = centered_rect(area, 0.85);

    frame.render_widget(Clear, popup_area);

    if app.detail_loading {
        frame.render_widget(popup_block(" Loading... "), popup_area);
        return;
    }

    let detail = match &app.detail {
        Some(d) => d,
        None => {
            frame.render_widget(popup_block(" No data "), popup_area);
            return;
        }
    };

    let session = &detail.info;
    let title = format!(" {} (PID {}) ", session.project_name, session.pid);

    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let lines = build_popup_content(detail, inner.width);

    let total_lines = lines.len() as u16;
    let scroll_info = format!(
        " {}/{} ",
        (app.popup_scroll as usize).min(total_lines.saturating_sub(1) as usize) + 1,
        total_lines
    );

    let scroll_span = Paragraph::new(Line::from(Span::styled(
        scroll_info,
        Style::default().fg(Color::DarkGray),
    )));
    if inner.height > 0 {
        let indicator_area = Rect::new(
            inner.x,
            popup_area.y + popup_area.height - 1,
            inner.width,
            1,
        );
        frame.render_widget(
            scroll_span.alignment(ratatui::layout::Alignment::Right),
            indicator_area,
        );
    }

    let content = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.popup_scroll, 0));

    frame.render_widget(content, inner);
}

/// What the popup should render for an artifact. Path/kind hints determine
/// whether we inline an image, an excerpt of a text/log file, or just a card
/// that links to an external resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardKind {
    Image,
    Video,
    Text,
    Diff,
    Url,
    Fallback,
}

/// Body height (excluding the 1-line caption header) for a given card kind.
const CARD_IMAGE_BODY_H: u16 = 10;
const CARD_TEXT_BODY_H: u16 = 12;
const CARD_DIFF_BODY_H: u16 = 14;
const CARD_VIDEO_BODY_H: u16 = 2;
const CARD_URL_BODY_H: u16 = 2;
const CARD_FALLBACK_BODY_H: u16 = 1;

/// Lead-artifact body heights — the headline proof gets more vertical room so
/// the user can actually see it without expanding.
const CARD_IMAGE_LEAD_BODY_H: u16 = 18;
const CARD_TEXT_LEAD_BODY_H: u16 = 16;

fn classify_artifact(a: &Artifact) -> CardKind {
    let kind = a.kind.to_ascii_lowercase();
    let path = &a.path;
    if path.starts_with("http://") || path.starts_with("https://") || kind == "url" {
        return CardKind::Url;
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match kind.as_str() {
        "screenshot" | "image" | "photo" => return CardKind::Image,
        "video" => return CardKind::Video,
        "diff" | "patch" => return CardKind::Diff,
        "log" | "build" | "test" | "text" | "output" => return CardKind::Text,
        _ => {}
    }
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => CardKind::Image,
        "mp4" | "mov" | "webm" | "mkv" => CardKind::Video,
        "diff" | "patch" => CardKind::Diff,
        "log" | "txt" | "md" | "json" | "yaml" | "yml" => CardKind::Text,
        _ => CardKind::Fallback,
    }
}

fn card_body_height(kind: CardKind) -> u16 {
    match kind {
        CardKind::Image => CARD_IMAGE_BODY_H,
        CardKind::Text => CARD_TEXT_BODY_H,
        CardKind::Diff => CARD_DIFF_BODY_H,
        CardKind::Video => CARD_VIDEO_BODY_H,
        CardKind::Url => CARD_URL_BODY_H,
        CardKind::Fallback => CARD_FALLBACK_BODY_H,
    }
}

fn lead_card_body_height(kind: CardKind) -> u16 {
    match kind {
        CardKind::Image => CARD_IMAGE_LEAD_BODY_H,
        CardKind::Text => CARD_TEXT_LEAD_BODY_H,
        _ => card_body_height(kind),
    }
}

/// Reads up to 8 KiB of `path` as lossy UTF-8 and splits into lines. Returns
/// `None` for binary files (>5 % non-text bytes in the leading 1 KiB) so the
/// caller can show "(binary file)" rather than dumping garbage at the user.
fn read_text_excerpt(path: &Path, max_bytes: usize) -> Option<(Vec<String>, usize)> {
    let bytes = std::fs::read(path).ok()?;
    let probe_len = bytes.len().min(1024);
    if probe_len > 0 {
        let non_text = bytes[..probe_len]
            .iter()
            .filter(|&&b| b == 0 || (b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t'))
            .count();
        if non_text * 20 > probe_len {
            return None;
        }
    }
    let take = bytes.len().min(max_bytes);
    let head = String::from_utf8_lossy(&bytes[..take]).into_owned();
    let total_lines = bytes.iter().filter(|&&b| b == b'\n').count() + 1;
    let head_lines: Vec<String> = head.lines().map(|s| s.to_string()).collect();
    let truncated = total_lines.saturating_sub(head_lines.len());
    Some((head_lines, truncated))
}

fn evidence_card_header(a: &Artifact, selected: bool, is_lead: bool) -> Line<'static> {
    let basename = Path::new(&a.path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.path.clone());
    let stripe = if selected { "▌ " } else { "  " };
    let stripe_color = if selected {
        Color::LightCyan
    } else {
        Color::DarkGray
    };
    let name_style = if selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let mut spans = vec![
        Span::styled(stripe, Style::default().fg(stripe_color)),
        Span::styled(a.kind.clone(), Style::default().fg(Color::LightMagenta)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(basename, name_style),
    ];
    if is_lead {
        spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            "lead",
            Style::default().fg(Color::Rgb(150, 195, 160)),
        ));
    }
    if let Some(c) = a.caption.as_deref() {
        if !c.is_empty() {
            spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(Color::Rgb(180, 180, 200)),
            ));
        }
    }
    Line::from(spans)
}

fn render_text_card_body(
    frame: &mut Frame,
    area: Rect,
    a: &Artifact,
    max_bytes: usize,
    scroll_lines: usize,
) {
    if area.height == 0 {
        return;
    }
    let path = Path::new(&a.path);
    let lines: Vec<Line<'static>> = match read_text_excerpt(path, max_bytes) {
        None => match std::fs::metadata(path) {
            Ok(_) => vec![Line::from(Span::styled(
                "  (binary file — open externally with `o`)",
                Style::default().fg(Color::DarkGray),
            ))],
            Err(_) => vec![Line::from(Span::styled(
                format!("  (cannot read {})", path.display()),
                Style::default().fg(Color::Rgb(220, 100, 100)),
            ))],
        },
        Some((content, truncated)) => {
            let start = scroll_lines.min(content.len());
            let hidden_below =
                content.len().saturating_sub(start + area.height as usize) + truncated;
            let body_rows =
                (area.height as usize).saturating_sub(if hidden_below > 0 { 1 } else { 0 });
            let end = (start + body_rows).min(content.len());
            let mut out: Vec<Line<'static>> = content[start..end]
                .iter()
                .map(|s| {
                    Line::from(Span::styled(
                        format!("  {}", s),
                        Style::default().fg(Color::Gray),
                    ))
                })
                .collect();
            if hidden_below > 0 {
                out.push(truncated_footer(hidden_below));
            }
            out
        }
    };
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

fn truncated_footer(n: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "  … (truncated, {} more line{})",
            n,
            if n == 1 { "" } else { "s" }
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ))
}

fn artifact_preview_total_lines(
    a: &Artifact,
    kind: CardKind,
    max_bytes: usize,
    area_width: u16,
) -> Option<usize> {
    let path = Path::new(&a.path);
    match kind {
        CardKind::Text => read_text_excerpt(path, max_bytes)
            .map(|(content, truncated)| content.len() + usize::from(truncated > 0)),
        CardKind::Diff => read_text_excerpt(path, max_bytes).map(|(content, truncated)| {
            build_diff_lines(content, area_width).len() + usize::from(truncated > 0)
        }),
        _ => None,
    }
}

const DIFF_ADDED_BG: Color = Color::Rgb(34, 92, 43);
const DIFF_REMOVED_BG: Color = Color::Rgb(122, 41, 54);
const DIFF_ADDED_FG: Color = Color::Rgb(180, 235, 190);
const DIFF_REMOVED_FG: Color = Color::Rgb(245, 195, 200);
const DIFF_GUTTER_FG: Color = Color::Rgb(120, 120, 130);
const DIFF_CONTEXT_FG: Color = Color::Rgb(160, 160, 170);
const DIFF_HEADER_FG: Color = Color::Rgb(140, 180, 230);

// Dim metadata gray used for low-priority info on task cards (queued-merge
// border + the `#<id>` badge). Sits below every other tone in the card so the
// id reads as background context, not headline.
const TASK_META_DIM: Color = Color::Rgb(95, 100, 115);

// `{old:>3} {new:>3} {marker} ` + 1-cell margin = 11 used cols.
const DIFF_GUTTER_W: usize = 11;

#[derive(Clone, Copy)]
enum DiffRowKind {
    Added,
    Removed,
    Context,
}

impl DiffRowKind {
    fn marker(self) -> &'static str {
        match self {
            DiffRowKind::Added => "+",
            DiffRowKind::Removed => "-",
            DiffRowKind::Context => " ",
        }
    }
    fn palette(self) -> (Option<Color>, Color) {
        match self {
            DiffRowKind::Added => (Some(DIFF_ADDED_BG), DIFF_ADDED_FG),
            DiffRowKind::Removed => (Some(DIFF_REMOVED_BG), DIFF_REMOVED_FG),
            DiffRowKind::Context => (None, DIFF_CONTEXT_FG),
        }
    }
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace();
    parts.next()?;
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let old_start: u32 = old.split(',').next()?.parse().ok()?;
    let new_start: u32 = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

fn diff_row(
    area_width: u16,
    old: Option<u32>,
    new: Option<u32>,
    content: &str,
    kind: DiffRowKind,
) -> Line<'static> {
    let (bg_opt, content_fg) = kind.palette();
    let base = match bg_opt {
        Some(b) => Style::default().bg(b),
        None => Style::default(),
    };
    let fmt_num = |n: Option<u32>| n.map_or_else(|| "   ".to_string(), |v| format!("{:>3}", v));

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
    spans.push(Span::styled(fmt_num(old), base.fg(DIFF_GUTTER_FG)));
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled(fmt_num(new), base.fg(DIFF_GUTTER_FG)));
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled(kind.marker(), base.fg(content_fg)));
    spans.push(Span::styled("  ", base));

    let avail = (area_width as usize).saturating_sub(DIFF_GUTTER_W);
    let normalized: Cow<'_, str> = if content.contains('\t') {
        Cow::Owned(content.replace('\t', "    "))
    } else {
        Cow::Borrowed(content)
    };
    let body = models::first_line_truncated(&normalized, avail);
    let body_w = body.chars().count();

    let mut content_style = base.fg(content_fg);
    if matches!(kind, DiffRowKind::Context) {
        content_style = content_style.add_modifier(Modifier::DIM);
    }
    spans.push(Span::styled(body, content_style));

    if matches!(kind, DiffRowKind::Added | DiffRowKind::Removed) {
        let pad = avail.saturating_sub(body_w);
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), base));
        }
    }
    Line::from(spans)
}

fn diff_separator_row() -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(DIFF_GUTTER_W.saturating_sub(3))),
        Span::styled(
            "...",
            Style::default()
                .fg(DIFF_GUTTER_FG)
                .add_modifier(Modifier::DIM),
        ),
    ])
}

fn is_diff_meta_line(s: &str) -> bool {
    s.starts_with("diff --git")
        || s.starts_with("index ")
        || s.starts_with("--- ")
        || s.starts_with("+++ ")
        || s.starts_with("\\ No newline")
        || s.starts_with("similarity index")
        || s.starts_with("dissimilarity index")
        || s.starts_with("rename from")
        || s.starts_with("rename to")
        || s.starts_with("copy from")
        || s.starts_with("copy to")
        || s.starts_with("new file mode")
        || s.starts_with("deleted file mode")
        || s.starts_with("old mode")
        || s.starts_with("new mode")
        || s.starts_with("Binary files")
        || s.starts_with("GIT binary patch")
}

fn build_diff_lines(content: Vec<String>, area_width: u16) -> Vec<Line<'static>> {
    let mut header_path: Option<String> = None;
    let mut added: u32 = 0;
    let mut removed: u32 = 0;
    let mut body_rows: Vec<Line<'static>> = Vec::new();
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;
    let mut first_hunk = true;

    for raw in content.into_iter() {
        if header_path.is_none() {
            if let Some(rest) = raw.strip_prefix("+++ b/") {
                header_path = Some(rest.to_string());
            } else if let Some(rest) = raw.strip_prefix("+++ ") {
                if !rest.is_empty() && rest != "/dev/null" {
                    header_path = Some(rest.to_string());
                }
            }
        }
        if is_diff_meta_line(&raw) {
            continue;
        }
        if raw.starts_with("@@") {
            if let Some((o, n)) = parse_hunk_header(&raw) {
                old_line = o;
                new_line = n;
            }
            if !first_hunk {
                body_rows.push(diff_separator_row());
            }
            first_hunk = false;
            continue;
        }
        let (kind, body, bump_old, bump_new) = match raw.as_bytes().first() {
            Some(b'+') => (DiffRowKind::Added, &raw[1..], 0, 1),
            Some(b'-') => (DiffRowKind::Removed, &raw[1..], 1, 0),
            Some(b' ') => (DiffRowKind::Context, &raw[1..], 1, 1),
            _ => (DiffRowKind::Context, raw.as_str(), 1, 1),
        };
        match kind {
            DiffRowKind::Added => added += 1,
            DiffRowKind::Removed => removed += 1,
            DiffRowKind::Context => {}
        }
        let (old_n, new_n) = match kind {
            DiffRowKind::Added => (None, Some(new_line)),
            DiffRowKind::Removed => (Some(old_line), None),
            DiffRowKind::Context => (Some(old_line), Some(new_line)),
        };
        body_rows.push(diff_row(area_width, old_n, new_n, body, kind));
        old_line += bump_old;
        new_line += bump_new;
    }

    let mut rows: Vec<Line<'static>> = Vec::with_capacity(body_rows.len() + 3);
    if let Some(p) = header_path {
        rows.push(Line::from(Span::styled(
            p,
            Style::default()
                .fg(DIFF_HEADER_FG)
                .add_modifier(Modifier::BOLD),
        )));
        rows.push(Line::from(Span::styled(
            format!("Added {} lines, removed {} lines", added, removed),
            Style::default()
                .fg(DIFF_GUTTER_FG)
                .add_modifier(Modifier::BOLD),
        )));
    }
    rows.extend(body_rows);
    rows
}

fn render_diff_card_body(
    frame: &mut Frame,
    area: Rect,
    a: &Artifact,
    max_bytes: usize,
    scroll_lines: usize,
) {
    if area.height == 0 {
        return;
    }
    let path = Path::new(&a.path);
    let lines: Vec<Line<'static>> = match read_text_excerpt(path, max_bytes) {
        None => match std::fs::metadata(path) {
            Ok(_) => vec![Line::from(Span::styled(
                "  (binary file — open externally with `o`)",
                Style::default().fg(Color::DarkGray),
            ))],
            Err(_) => vec![Line::from(Span::styled(
                format!("  (cannot read {})", path.display()),
                Style::default().fg(Color::Rgb(220, 100, 100)),
            ))],
        },
        Some((content, truncated)) => {
            let all_rows = build_diff_lines(content, area.width);
            let start = scroll_lines.min(all_rows.len());
            let hidden_below =
                all_rows.len().saturating_sub(start + area.height as usize) + truncated;
            let body_rows =
                (area.height as usize).saturating_sub(if hidden_below > 0 { 1 } else { 0 });
            let end = (start + body_rows).min(all_rows.len());
            let mut visible = all_rows[start..end].to_vec();
            if hidden_below > 0 {
                visible.push(truncated_footer(hidden_below));
            }
            visible
        }
    };
    let p = Paragraph::new(lines);
    frame.render_widget(p, area);
}

fn render_video_card_body(frame: &mut Frame, area: Rect, a: &Artifact) {
    if area.height == 0 {
        return;
    }
    let basename = Path::new(&a.path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.path.clone());
    let lines = vec![
        Line::from(Span::styled(
            "  ▶ press `o` to play in external player",
            Style::default().fg(Color::LightCyan),
        )),
        Line::from(Span::styled(
            format!("  {}", basename),
            Style::default().fg(Color::Rgb(160, 160, 180)),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_url_card_body(frame: &mut Frame, area: Rect, a: &Artifact) {
    if area.height == 0 {
        return;
    }
    let lines = vec![
        Line::from(Span::styled(
            format!("  {}", a.path),
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::UNDERLINED),
        )),
        Line::from(Span::styled(
            "  press `o` to open in browser",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_fallback_card_body(frame: &mut Frame, area: Rect, a: &Artifact) {
    if area.height == 0 {
        return;
    }
    let basename = Path::new(&a.path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.path.clone());
    let line = Line::from(Span::styled(
        format!("  {}", basename),
        Style::default().fg(Color::Rgb(160, 160, 180)),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

fn render_image_placeholder(frame: &mut Frame, area: Rect, msg: &str) {
    if area.height == 0 {
        return;
    }
    let line = Line::from(Span::styled(
        format!("  {}", msg),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

/// Decode `path` into the per-app image cache, using the picker's protocol
/// detection. Returns `true` when the entry is now present (either freshly
/// decoded or already cached); `false` on decode failure (recorded so we
/// don't retry on every frame).
fn ensure_image_decoded(app: &mut App, path: &str) -> bool {
    if app.artifact_images.contains_key(path) {
        return true;
    }
    if app.artifact_image_failed.contains(path) {
        return false;
    }
    let Some(picker) = app.image_picker.as_ref() else {
        app.artifact_image_failed.insert(path.to_string());
        return false;
    };
    let img = match image::ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.decode().ok())
    {
        Some(i) => i,
        None => {
            app.artifact_image_failed.insert(path.to_string());
            return false;
        }
    };
    let proto = picker.new_resize_protocol(img);
    app.artifact_images.insert(path.to_string(), proto);
    true
}

/// "Result" popup for the selected Projects task. Progressive-disclosure
/// layout: header (status · age · count) → one-line note (proof headline) →
/// lead artifact (large) → other artifacts → muted "summary" appendix at the
/// bottom. The note is the headline; the agent's full summary is an appendix
/// the user scrolls into, not the centerpiece.
fn render_projects_result(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.85);
    frame.render_widget(Clear, popup_area);

    let Some(t) = app.selected_project_task().cloned() else {
        frame.render_widget(popup_block(" Result — no task selected "), popup_area);
        return;
    };

    let status_label = t.status.as_str();
    let status_color = match t.status {
        crate::orchestrator::TaskStatus::Running => Color::LightYellow,
        crate::orchestrator::TaskStatus::Review => Color::LightCyan,
        crate::orchestrator::TaskStatus::Merging => Color::LightMagenta,
        crate::orchestrator::TaskStatus::Done => Color::LightGreen,
        crate::orchestrator::TaskStatus::Backlog => Color::Rgb(120, 140, 200),
    };
    let title = match t.title.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => format!(
            " Result · {} · {} ",
            crate::orchestrator::short_task_id(&t.task_id),
            name,
        ),
        None => format!(
            " Result · {} ",
            crate::orchestrator::short_task_id(&t.task_id)
        ),
    };
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height == 0 {
        return;
    }

    let now_secs = now_ms() / 1000;
    let age = models::relative_age(now_secs.saturating_sub(t.updated_at as u64));

    // ── Header (status badge · age · count) + note headline ────────────────
    let mut header_lines: Vec<Line<'static>> = Vec::new();
    let mut header_spans = vec![
        Span::styled(
            format!("[{}]", status_label),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(age, Style::default().fg(Color::Rgb(160, 160, 180))),
    ];
    if let Some(v) = t.shipped_version.as_deref().filter(|s| !s.is_empty()) {
        header_spans.push(Span::raw("  "));
        header_spans.push(shipped_version_span(v));
    }
    header_spans.push(Span::raw("  "));
    header_spans.push(Span::styled(
        format!(
            "({} artifact{})",
            t.artifacts.len(),
            if t.artifacts.len() == 1 { "" } else { "s" }
        ),
        Style::default().fg(Color::Rgb(150, 130, 200)),
    ));
    header_lines.push(Line::from(header_spans));
    header_lines.push(Line::raw(""));

    // Note is the headline proof; falls back to a single-line truncation of
    // the prompt when the orchestrator hasn't written one yet.
    let note_text: String = match t.note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => n.lines().next().unwrap_or("").to_string(),
        None => t.prompt.lines().next().unwrap_or("").trim().to_string(),
    };
    if !note_text.is_empty() {
        header_lines.push(Line::from(Span::styled(
            note_text,
            Style::default()
                .fg(Color::Rgb(220, 220, 230))
                .add_modifier(Modifier::ITALIC),
        )));
        header_lines.push(Line::raw(""));
    }

    // Lead artifact renders first; the rest follow in original order.
    let lead_idx = t.lead_artifact.filter(|&i| i < t.artifacts.len());
    let render_order: Vec<usize> = lead_idx
        .into_iter()
        .chain((0..t.artifacts.len()).filter(|i| Some(*i) != lead_idx))
        .collect();

    // ── Layout & scrolling ────────────────────────────────────────────────
    let header_h = header_lines.len() as u16;
    let body_h = inner.height.saturating_sub(1);
    let body_area = Rect::new(inner.x, inner.y, inner.width, body_h);
    let footer_area = Rect::new(inner.x, inner.y + body_h, inner.width, 1);

    // When the user has hit `e`, the selected card swells to fill most of
    // the visible body area; non-selected cards keep their default heights so
    // the surrounding context stays in view.
    let expanded_body_h: u16 = if app.result_artifact_expanded {
        body_h.saturating_sub(6).min(40)
    } else {
        0
    };
    let mut card_meta: Vec<(usize, CardKind, u16)> = Vec::with_capacity(render_order.len());
    for &art_idx in &render_order {
        let kind = classify_artifact(&t.artifacts[art_idx]);
        let default_h = if lead_idx == Some(art_idx) {
            lead_card_body_height(kind)
        } else {
            card_body_height(kind)
        };
        let h = if app.result_artifact_expanded && art_idx == app.result_artifact_sel {
            expanded_body_h.max(default_h)
        } else {
            default_h
        };
        card_meta.push((art_idx, kind, h));
    }

    let mut canvas_card_tops: Vec<u16> = Vec::with_capacity(card_meta.len());
    let mut next_y = header_h;
    for (_, _, body) in &card_meta {
        canvas_card_tops.push(next_y);
        // Card = header(1) + body + spacer(1).
        next_y = next_y.saturating_add(1 + body + 1);
    }
    if t.artifacts.is_empty() {
        next_y = next_y.saturating_add(1); // "(no artifacts)" line
    }

    let summary_lines: Vec<Line<'static>> = match t
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => Vec::new(),
        Some(s) => {
            let mut out: Vec<Line<'static>> = Vec::new();
            out.push(Line::raw(""));
            out.push(Line::from(Span::styled(
                "summary",
                Style::default().fg(Color::Rgb(120, 120, 140)),
            )));
            for line in s.lines() {
                out.push(Line::from(Span::styled(
                    format!("  {}", line),
                    Style::default().fg(Color::Rgb(160, 160, 180)),
                )));
            }
            out
        }
    };
    let summary_h = summary_lines.len() as u16;
    next_y = next_y.saturating_add(summary_h);
    let total_canvas_h = next_y;

    let expanded_scroll_budget = if app.result_artifact_expanded {
        card_meta
            .iter()
            .find(|(art_idx, _, _)| *art_idx == app.result_artifact_sel)
            .and_then(|(art_idx, kind, body)| {
                let max_bytes = 64 * 1024;
                artifact_preview_total_lines(&t.artifacts[*art_idx], *kind, max_bytes, inner.width)
                    .map(|total| total.saturating_sub(*body as usize).min(u16::MAX as usize) as u16)
            })
            .unwrap_or(0)
    } else {
        0
    };

    // Auto-scroll so the selected card stays on-screen.
    if !t.artifacts.is_empty() && body_h > 0 {
        let sel_art_idx = app.result_artifact_sel.min(t.artifacts.len() - 1);
        let sel_render_pos = render_order
            .iter()
            .position(|&i| i == sel_art_idx)
            .unwrap_or(0);
        let sel_top = canvas_card_tops[sel_render_pos];
        let (_, _, sel_body) = card_meta[sel_render_pos];
        let sel_h = 1 + sel_body + 1;
        if sel_top < app.result_scroll {
            app.result_scroll = sel_top;
        } else if sel_top + sel_h > app.result_scroll + body_h {
            app.result_scroll = sel_top + sel_h - body_h;
        }
    }
    let base_max_scroll = total_canvas_h.saturating_sub(body_h);
    let max_scroll = base_max_scroll.saturating_add(expanded_scroll_budget);
    if app.result_scroll > max_scroll {
        app.result_scroll = max_scroll;
    }
    let scroll = app.result_scroll;

    // The placeholder blank rows below each card header keep y-offsets honest
    // for the Paragraph's vertical scroll; per-card widgets paint over them in
    // the second pass.
    let mut canvas_lines: Vec<Line<'static>> = Vec::with_capacity(total_canvas_h as usize);
    canvas_lines.extend(header_lines);
    if t.artifacts.is_empty() {
        canvas_lines.push(Line::from(Span::styled(
            "  (no artifacts attached)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for &(art_idx, _, body) in &card_meta {
            let a = &t.artifacts[art_idx];
            let selected = art_idx == app.result_artifact_sel;
            let is_lead = lead_idx == Some(art_idx);
            canvas_lines.push(evidence_card_header(a, selected, is_lead));
            for _ in 0..body {
                canvas_lines.push(Line::raw(""));
            }
            canvas_lines.push(Line::raw(""));
        }
    }
    canvas_lines.extend(summary_lines);
    let canvas_para = Paragraph::new(canvas_lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(canvas_para, body_area);

    // Per-card widgets, painted on top of the placeholder rows.
    for (rp, &(art_idx, kind, body)) in card_meta.iter().enumerate() {
        let a = &t.artifacts[art_idx];
        let card_top_canvas = canvas_card_tops[rp];
        let body_top_canvas = card_top_canvas + 1;
        let body_screen_top = body_area.y as i32 + body_top_canvas as i32 - scroll as i32;
        let body_screen_bot = body_screen_top + body as i32;
        let view_top = body_area.y as i32;
        let view_bot = (body_area.y + body_h) as i32;
        if body_screen_bot <= view_top || body_screen_top >= view_bot {
            continue;
        }
        let visible_top = body_screen_top.max(view_top);
        let visible_bot = body_screen_bot.min(view_bot);
        let visible_h = (visible_bot - visible_top).max(0) as u16;
        if visible_h == 0 {
            continue;
        }
        let body_rect = Rect::new(
            body_area.x.saturating_add(2),
            visible_top as u16,
            body_area.width.saturating_sub(2),
            visible_h,
        );
        let clipped_scroll = if body_screen_top < view_top {
            (view_top - body_screen_top) as usize
        } else {
            0
        };
        let overscroll_lines = scroll.saturating_sub(base_max_scroll) as usize;
        let body_scroll_lines = clipped_scroll.saturating_add(overscroll_lines);
        match kind {
            CardKind::Image => {
                // Kitty/sixel/iterm2 protocols write pixel data tied to a fixed
                // rect; partially-clipped rects leave terminal residue when the
                // popup scrolls. Only render when fully visible.
                let fully_visible = body_screen_top >= view_top && body_screen_bot <= view_bot;
                if !fully_visible {
                    render_image_placeholder(frame, body_rect, "[image hidden — scroll to view]");
                    continue;
                }
                if app.image_picker.is_none() {
                    render_image_placeholder(
                        frame,
                        body_rect,
                        "[image preview unavailable — terminal doesn't support graphics]",
                    );
                    continue;
                }
                let path = a.path.clone();
                if !ensure_image_decoded(app, &path) {
                    render_image_placeholder(
                        frame,
                        body_rect,
                        "[image preview unavailable — decode failed; press `o` to open]",
                    );
                    continue;
                }
                if let Some(state) = app.artifact_images.get_mut(&path) {
                    let widget =
                        StatefulImage::<ratatui_image::protocol::StatefulProtocol>::default();
                    frame.render_stateful_widget(widget, body_rect, state);
                }
            }
            CardKind::Text => {
                let expanded = app.result_artifact_expanded && art_idx == app.result_artifact_sel;
                let max_bytes = if expanded { 64 * 1024 } else { 8 * 1024 };
                render_text_card_body(frame, body_rect, a, max_bytes, body_scroll_lines);
            }
            CardKind::Diff => {
                let expanded = app.result_artifact_expanded && art_idx == app.result_artifact_sel;
                let max_bytes = if expanded { 64 * 1024 } else { 8 * 1024 };
                render_diff_card_body(frame, body_rect, a, max_bytes, body_scroll_lines);
            }
            CardKind::Video => render_video_card_body(frame, body_rect, a),
            CardKind::Url => render_url_card_body(frame, body_rect, a),
            CardKind::Fallback => render_fallback_card_body(frame, body_rect, a),
        }
    }

    let artifact_pos = if t.artifacts.is_empty() {
        "artifact —".to_string()
    } else {
        format!(
            "artifact {}/{}",
            app.result_artifact_sel.min(t.artifacts.len() - 1) + 1,
            t.artifacts.len()
        )
    };
    let hint = format!(
        " {}   esc/r:close   j/k:artifact   e:expand   PgUp/PgDn:scroll   c:copy path   o:xdg-open ",
        artifact_pos
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        footer_area,
    );
}

fn render_state_debug(frame: &mut Frame, area: Rect, app: &App) {
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
        Style::default().fg(Color::Rgb(80, 80, 90)),
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
        Style::default().fg(Color::Rgb(80, 80, 90)),
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
            Span::styled(
                format!("  {:>3}  ", e.idx),
                Style::default().fg(Color::Rgb(80, 80, 90)),
            ),
            Span::styled(format!("{}  ", ts), Style::default().fg(Color::DarkGray)),
            Span::styled(e.kind.clone(), Style::default().fg(Color::Cyan)),
            Span::styled(stop, Style::default().fg(Color::Yellow)),
            Span::styled(blocks, Style::default().fg(Color::Rgb(160, 160, 170))),
        ]));
    }

    lines
}

fn render_live_tail(frame: &mut Frame, area: Rect, app: &mut App) {
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
        Style::default().fg(Color::Rgb(110, 110, 130)),
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
        Style::default().fg(Color::Rgb(110, 110, 130)),
    )))
    .alignment(ratatui::layout::Alignment::Right);

    let indicator_area = Rect::new(inner.x, bottom_y, inner.width, 1);
    frame.render_widget(indicator, indicator_area);
}

fn build_live_tail_content(
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

fn render_turn_stats(
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

fn is_placeholder_preview(s: &str) -> bool {
    s.is_empty()
        || s == crate::conversation::NO_CONTENT
        || s == crate::conversation::NO_TEXT_CONTENT
}

#[derive(Debug, Clone)]
enum PreviewPart {
    Thinking,
    Tool(String),
    Text(String),
}

/// Tokenize a preview back into the parts that produced it. The marker
/// format is defined by `extract_text_content` in conversation.rs — keep
/// the two in sync via the shared marker constants.
fn parse_preview(preview: &str) -> Vec<PreviewPart> {
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

fn render_prompt_block(lines: &mut Vec<Line<'static>>, body: &str) {
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

fn render_asst_bullet(lines: &mut Vec<Line<'static>>, body: &str) {
    push_bullet_block(
        lines,
        Span::styled("● ", Style::default().fg(Color::Green)),
        Color::Rgb(220, 220, 230),
        body,
    );
}

fn render_tool_bullet(lines: &mut Vec<Line<'static>>, display: &str) {
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

fn render_thinking(lines: &mut Vec<Line<'static>>) {
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
fn push_bullet_block(
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

fn build_popup_content(detail: &SessionDetail, width: u16) -> Vec<Line<'static>> {
    let session = &detail.info;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(Color::Rgb(100, 100, 120))),
        Span::styled("Path:    ", Style::default().fg(Color::DarkGray)),
        Span::styled(session.cwd.clone(), Style::default().fg(Color::White)),
    ]));

    let mut meta_spans = vec![
        Span::styled("󰚩  ", Style::default().fg(Color::Rgb(100, 100, 120))),
        Span::styled("Agent:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(session.agent_badge(), Style::default().fg(Color::White)),
        Span::styled("     ", Style::default().fg(Color::Rgb(100, 100, 120))),
        Span::styled("Branch:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            session.git_branch.clone().unwrap_or_default(),
            Style::default().fg(Color::Cyan),
        ),
    ];
    if let Some(model) = &session.model {
        meta_spans.push(Span::styled(
            "   󰧑 Model: ",
            Style::default().fg(Color::DarkGray),
        ));
        meta_spans.push(Span::styled(
            short_model(model).to_string(),
            Style::default().fg(Color::White),
        ));
    }
    if let Some(version) = &session.version {
        meta_spans.push(Span::styled("   v", Style::default().fg(Color::DarkGray)));
        meta_spans.push(Span::styled(
            version.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(meta_spans));

    let (state_icon, _) = state_indicator(&session.state);
    let sc = state_color(&session.state);
    lines.push(Line::from(vec![
        Span::styled(format!("{} ", state_icon), Style::default().fg(sc)),
        Span::styled("State:   ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}", session.state), Style::default().fg(sc)),
        Span::styled("   󰥔 Started: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format_datetime(session.started_at),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("󰆏 ", Style::default().fg(Color::Rgb(100, 100, 120))),
        Span::styled("Tokens:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{}in / {}out",
                format_tokens(detail.total_input_tokens),
                format_tokens(detail.total_output_tokens)
            ),
            Style::default().fg(Color::White),
        ),
    ]));

    if let Some(summary) = &session.summary {
        lines.push(Line::from(vec![
            Span::styled("󰍡 ", Style::default().fg(Color::Rgb(100, 100, 120))),
            Span::styled("Topic:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(summary.clone(), Style::default().fg(Color::White)),
        ]));
    }

    let sep: String = "─".repeat(width.saturating_sub(1) as usize);
    lines.push(Line::from(Span::styled(
        sep,
        Style::default().fg(Color::Rgb(50, 50, 60)),
    )));
    lines.push(Line::raw(""));

    for msg in &detail.recent_messages {
        let (role_icon, role_label, role_color) = match msg.role.as_str() {
            "user" => ("", "user", Color::Yellow),
            "assistant" => ("󰧑", "asst", Color::Green),
            "system" => ("", "sys ", Color::DarkGray),
            _ => ("", "??? ", Color::DarkGray),
        };

        let time_str = format_time(msg.timestamp);

        let mut header_spans = vec![
            Span::styled(
                format!("{} {} ", role_icon, role_label),
                Style::default().fg(role_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("󰥔 {}", time_str),
                Style::default().fg(Color::DarkGray),
            ),
        ];

        if let Some(model) = &msg.model {
            let short = short_model(model);
            header_spans.push(Span::styled(
                format!("  󰧑 {}", short),
                Style::default().fg(Color::Rgb(80, 80, 80)),
            ));
        }

        if let Some(stop) = &msg.stop_reason {
            if stop == "tool_use" {
                header_spans.push(Span::styled("   tools", Style::default().fg(Color::Cyan)));
            }
        }

        if let (Some(inp), Some(out)) = (msg.input_tokens, msg.output_tokens) {
            header_spans.push(Span::styled(
                format!("  {}in/{}out", format_tokens(inp), format_tokens(out)),
                Style::default().fg(Color::Rgb(60, 60, 60)),
            ));
        }

        lines.push(Line::from(header_spans));

        for content_line in msg.content_preview.lines() {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    content_line.to_string(),
                    Style::default().fg(Color::Rgb(200, 200, 210)),
                ),
            ]));
        }
        lines.push(Line::raw(""));
    }

    lines
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let elapsed = app.last_refresh.elapsed().as_secs();
    let refresh_text = if elapsed < 2 {
        "just now".to_string()
    } else {
        models::relative_age(elapsed)
    };

    let fresh_status = app
        .status_msg
        .as_ref()
        .filter(|(_, ts)| ts.elapsed() < status_msg_ttl())
        .map(|(msg, _)| msg.as_str());

    let mut spans: Vec<Span> = Vec::new();

    if let Some(msg) = fresh_status {
        spans.push(Span::styled(
            format!(" {} ", msg),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        let keybinds: &str = match app.view {
            View::Grid => match app.current_tab {
                // High-value Review verbs lead so they survive the right-edge
                // truncation at narrow widths (the status bar is one row, no
                // wrap); rare project-management verbs trail. The Space:approve
                // chip is rendered separately *ahead* of this string below so
                // it is never the first thing clipped.
                Tab::Projects => "enter:focus orch  n:new task  r:result  f:agent terminal/resurrect  R:restart  b:backlog  h/l:col  j/k:task  H/L:project  N:register project  c:copy id  x:delete task  X:remove project  tab:next  q:quit",
                Tab::Sessions => "enter/f:focus/resume  n:new  i:info  r:rename  t:to-do  o:shell  N:new in…  M:bookmarks  D:why?  h/j/k/l:nav  x:close  H:inactive  W:workers  tab:next  q:quit",
                Tab::Metrics => "enter:view transcript  j/k:select  r:refresh  tab:next  q:quit",
            },
            View::Popup => "j/k:scroll  esc:close  q:close",
            View::LiveTail => "j/k:scroll  G:bottom  esc:close",
            View::ConfirmClose => "y:confirm  n/esc:cancel",
            View::StateDebug => "j/k:scroll  esc:close  q:close",
            View::PromptInput => "type prompt  enter:dispatch  esc:cancel",
            View::RenameSession => "edit title  enter:rename  esc:cancel",
            View::TodoPanel => {
                if app.todo_adding {
                    "type task  enter:add  esc:cancel"
                } else {
                    "j/k:move  space/enter:toggle  a:add  d:delete  c:clear done  t/esc:close"
                }
            }
            View::TmuxPane => "forwarding keys to tmux · F1: detach & close",
            View::FolderPicker => {
                if app
                    .folder_picker
                    .as_ref()
                    .is_some_and(|p| p.mode == PickerMode::Bookmarks)
                {
                    "j/k:move  enter/space:pick  m:unbookmark  esc:cancel"
                } else {
                    "j/k:move  enter:descend  bksp:parent  space:pick  .:pick cwd  m:bookmark  c/C:gh new (pub/priv)  esc:cancel"
                }
            }
            View::GhCreateInput => "type name  tab:toggle public/private  enter:create  esc:cancel",
            View::ProjectsResult => "j/k:artifact  e:expand  PgUp/PgDn:scroll  c:copy path  o:xdg-open  esc/r:close",
            View::Backlog => "j/k:select  s/enter:start  x:delete  esc/q:close",
        };
        // Render the Space chip *first* so the single highest-value verb
        // (approve on Projects, ack on Sessions) is never the first thing
        // clipped off the right edge of this one-row, no-wrap status bar.
        let space_verb = match (&app.view, app.current_tab) {
            (View::Grid, Tab::Projects) => Some("approve "),
            (View::Grid, Tab::Sessions) => Some("ack "),
            _ => None,
        };
        if let Some(verb) = space_verb {
            spans.push(Span::styled(
                " Space ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                verb,
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(
            format!(" {} ", keybinds),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Pending dispatch indicator — visible when a freshly-spawned session
    // has a queued prompt that hasn't fired yet. Without this the user has
    // no way to tell that "session sitting there empty" actually has a
    // dispatch in flight, or that it's about to time out.
    if let Some(target) = app.pending_dispatch_target() {
        let age = app.pending_dispatch_age().map(|d| d.as_secs()).unwrap_or(0);
        let queued = app.pending_dispatch_count();
        let suffix = if queued > 1 {
            format!(" +{}", queued - 1)
        } else {
            String::new()
        };
        spans.push(Span::styled(
            format!(" ↻ dispatch waiting [{}{}] {}s ", target, suffix, age),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    spans.push(Span::styled(
        format!("refreshed {} ", refresh_text),
        Style::default().fg(Color::DarkGray),
    ));

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(30, 30, 30))),
        area,
    );
}

fn state_indicator(state: &SessionState) -> (&'static str, Color) {
    match state {
        SessionState::Processing => ("󰒓", Color::Green),
        SessionState::WaitingForInput => ("󰂞", Color::Yellow),
        SessionState::Question => ("󰋗", Color::LightBlue),
        SessionState::Idle => ("󰒲", Color::Rgb(100, 100, 120)),
        SessionState::Inactive => ("󰜎", Color::Rgb(80, 80, 90)),
    }
}

fn state_color(state: &SessionState) -> Color {
    state_indicator(state).1
}

fn short_model(model: &str) -> &str {
    model.strip_prefix("claude-").unwrap_or(model)
}

/// Tool names for the card HUD: strip MCP-server prefixes and cap at 18 chars
/// so long names like `mcp__claude_ai_Notion__notion-search` fit in narrow
/// cards.
fn short_tool(tool: &str) -> String {
    // `mcp__<server>__<name>` → just the name (the leaf is what's distinctive).
    let leaf = crate::models::mcp_leaf(tool);
    let chars: Vec<char> = leaf.chars().collect();
    if chars.len() <= 18 {
        return leaf.to_string();
    }
    let mut s: String = chars.into_iter().take(17).collect();
    s.push('…');
    s
}

/// Render the in-flight tool as `󰖷 Bash: cargo build` when a hint is
/// available, or just `󰖷 Bash` otherwise. The hint is truncated so the
/// whole label fits on the activity line of a card `inner_w` columns wide,
/// alongside the `󰔟 …s` elapsed time on the left.
fn format_tool_label(tool: &crate::conversation::CurrentTool, inner_w: usize) -> String {
    let name = short_tool(&tool.name);
    let Some(hint) = tool.hint.as_deref().filter(|h| !h.is_empty()) else {
        return format!("󰖷 {}", name);
    };
    // Reserve: icon (2) + space (1) + name + ": " (2) + min elapsed gutter (8).
    let prefix_cols = 2 + 1 + name.chars().count() + 2;
    let budget = inner_w.saturating_sub(prefix_cols).saturating_sub(8);
    let hint_short = models::first_line_truncated(hint, budget.max(6));
    format!("󰖷 {}: {}", name, hint_short)
}

/// Effective context-window size in tokens. The JSONL `model` field is the
/// bare id (`claude-opus-4-7`) and never carries the `[1m]` / `-1m` suffix
/// even when a session is running on the 1M-context variant, so we infer:
/// Opus 4.7+ defaults to 1M; explicit `[1m]` / `-1m` markers force 1M; all
/// other models fall back to the standard 200k window.
fn context_window_size(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.contains("[1m]") || m.contains("-1m") || m.contains("opus-4-7") {
        1_000_000
    } else {
        200_000
    }
}

fn format_time(timestamp_ms: u64) -> String {
    let secs = (timestamp_ms / 1000) as i64;
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%l:%M %p").to_string(),
        _ => "??:??".to_string(),
    }
}

fn format_datetime(timestamp_ms: u64) -> String {
    let secs = (timestamp_ms / 1000) as i64;
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%b %d %l:%M %p").to_string(),
        _ => "unknown".to_string(),
    }
}

fn format_elapsed(now: u64, from_ms: u64) -> String {
    let secs = now.saturating_sub(from_ms) / 1000;
    format_duration_secs(secs)
}

fn format_duration_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{}h", h)
        } else {
            format!("{}h {}m", h, m)
        }
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        if h == 0 {
            format!("{}d", d)
        } else {
            format!("{}d {}h", d, h)
        }
    }
}

fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        format!("{}", count)
    }
}

fn fmt_cost(c: f64) -> String {
    if c >= 100.0 {
        format!("${:.0}", c)
    } else if c >= 10.0 {
        format!("${:.1}", c)
    } else {
        format!("${:.2}", c)
    }
}

fn render_projects_body(frame: &mut Frame, area: Rect, app: &App) {
    let snap = &app.projects;

    if snap.projects.is_empty() {
        let empty = Paragraph::new(Line::from(vec![
            Span::styled(
                "No projects registered yet. ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "Press N to register a folder, then n to start a task.",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title(" Projects "));
        frame.render_widget(empty, area);
        return;
    }

    // Top: project chip strip (1 line) + spacer (1 line). The PR-flow
    // design serialises merges through the project-level merge lock, so
    // file-level contention strips no longer apply — at most one task is
    // ever in the Merging column.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);
    render_project_chip_strip(frame, rows[0], app);
    render_kanban_board(frame, rows[1], app);
}

/// Horizontal strip of project "chips". Selected chip is bold/inverse with
/// per-column counts (P·R·Rv·M·D·F). Cycled with `[` / `]`.
/// A trailing amber 󰒲N is shown only when backlog > 0.
fn render_project_chip_strip(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    // Background band so the strip reads as a header even on dark themes.
    let band = Style::default().bg(BAND_BG);
    frame.render_widget(Paragraph::new("").style(band), area);

    let chip_row = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let path_row = if area.height >= 2 {
        Some(Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: 1,
        })
    } else {
        None
    };

    let snap = &app.projects;
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(snap.projects.len() * 4 + 2);
    spans.push(Span::styled(
        "  󰉋 ",
        Style::default().fg(Color::Rgb(150, 150, 170)).bg(BAND_BG),
    ));
    spans.push(Span::styled(
        " P·R·Rv·M·D  ",
        Style::default().fg(Color::Rgb(110, 110, 130)).bg(BAND_BG),
    ));
    let mut selected_start: usize = 0;
    let mut selected_end: usize = 0;
    for (idx, p) in snap.projects.iter().enumerate() {
        let tasks = snap.tasks.get(&p.id);
        let mut planning = 0usize;
        let mut running = 0usize;
        let mut review = 0usize;
        let mut merging = 0usize;
        let mut done = 0usize;
        let mut backlog = 0usize;
        if let Some(v) = tasks {
            for t in v {
                match t.status {
                    crate::orchestrator::TaskStatus::Running => {
                        if t.workers.is_empty() {
                            planning += 1;
                        } else {
                            running += 1;
                        }
                    }
                    crate::orchestrator::TaskStatus::Review => review += 1,
                    crate::orchestrator::TaskStatus::Merging => merging += 1,
                    crate::orchestrator::TaskStatus::Done => done += 1,
                    crate::orchestrator::TaskStatus::Backlog => backlog += 1,
                }
            }
        }
        let selected = idx == app.projects_sel;
        let label = format!(" {} ", p.name);
        // Compact P·R·Rv·M·D counts. Review/Merging squeezed to two-letter
        // labels in the column headers; here they're positional.
        let counts = format!(" {}·{}·{}·{}·{} ", planning, running, review, merging, done);
        let (chip_fg, chip_bg) = if selected {
            (Color::Black, Color::Rgb(190, 200, 230))
        } else if planning + running + merging > 0 {
            (Color::Rgb(220, 220, 235), Color::Rgb(40, 50, 70))
        } else {
            (Color::Rgb(150, 150, 165), Color::Rgb(30, 30, 40))
        };
        let counts_bg = if selected {
            Color::Rgb(140, 150, 180)
        } else {
            Color::Rgb(20, 25, 35)
        };
        let counts_fg = if selected {
            Color::Black
        } else if planning + running + review + merging > 0 {
            Color::Rgb(160, 220, 180)
        } else {
            Color::Rgb(120, 120, 140)
        };
        if selected {
            selected_start = spans.iter().map(|s| s.content.chars().count()).sum();
        }
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(chip_fg)
                .bg(chip_bg)
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));
        spans.push(Span::styled(
            counts,
            Style::default().fg(counts_fg).bg(counts_bg),
        ));
        if backlog > 0 {
            let (bl_fg, bl_bg) = if selected {
                (Color::Black, Color::Rgb(200, 160, 80))
            } else {
                (Color::Rgb(220, 175, 95), Color::Rgb(50, 40, 22))
            };
            spans.push(Span::styled(
                format!(" 󰒲 {} ", backlog),
                Style::default().fg(bl_fg).bg(bl_bg),
            ));
        }
        spans.push(Span::styled(" ", band));
        if selected {
            selected_end = spans.iter().map(|s| s.content.chars().count()).sum();
        }
    }
    let visible_w = chip_row.width as usize;
    let chip_scroll = if selected_end > visible_w {
        selected_start.saturating_sub(4).min(u16::MAX as usize) as u16
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .style(band)
            .scroll((0, chip_scroll)),
        chip_row,
    );

    if let (Some(row), Some(p)) = (path_row, app.selected_project()) {
        let root = p.root.display().to_string();
        // The 5-cell prefix ("    …" when truncated, 5 spaces otherwise) keeps
        // the path aligned in the same column either way.
        let max = (row.width as usize).saturating_sub(5);
        let root = if root.chars().count() > max {
            let cut = root.chars().count().saturating_sub(max);
            format!("    …{}", root.chars().skip(cut).collect::<String>())
        } else {
            format!("     {}", root)
        };
        let line = Line::from(Span::styled(
            root,
            Style::default().fg(Color::Rgb(110, 110, 130)).bg(BAND_BG),
        ));
        frame.render_widget(Paragraph::new(line).style(band), row);
    }
}

fn render_kanban_board(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 3 || area.width < 30 {
        let hint = Paragraph::new("(terminal too narrow — resize or switch to Sessions)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Rgb(140, 140, 160)))
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
    let pr_summaries = &app.projects.pr_summaries;

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

fn kanban_column_meta(col: usize) -> (&'static str, &'static str, Color) {
    // (label, status icon, accent color). Indices match `kanban_column_tasks`.
    let (icon, accent) = match col {
        0 => ("󰟶", Color::Rgb(170, 140, 210)),
        1 => ("󰒓", Color::LightYellow),
        2 => ("󱋲", Color::LightCyan),
        3 => ("", Color::LightMagenta),
        _ => ("󰸞", Color::LightGreen),
    };
    (crate::app::kanban_col_name(col), icon, accent)
}

fn render_kanban_column(
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
    let col_focused = app.projects_col == col_idx;
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
        app.projects_task_sel.min(count - 1)
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
                .merge_lock_holders
                .get(&p.id)
                .and_then(|h| h.as_ref())?;
            let title = app
                .projects
                .tasks
                .get(&p.id)
                .and_then(|ts| ts.iter().find(|t| t.task_id == holder.task_id))
                .and_then(|t| t.title.as_deref());
            let pr_id = app
                .projects
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
            Style::default().fg(Color::Rgb(150, 150, 170)),
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
            Style::default().fg(Color::Rgb(110, 110, 130)),
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
        let titling_in_flight = app.projects.is_titling(&t.task_id);
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

/// Aggregate live agent counters for a task across orchestrator + workers.
struct AgentSummary {
    alive: u32,
    processing: u32,
    waiting: u32,
    idle: u32,
    inactive: u32,
    total: u32,
    total_ctx: u64,
    max_ctx: u64,
    /// Worst utilization across alive agents (0..=100).
    max_ctx_pct: u8,
    current_tool: Option<(String, Option<String>)>,
    is_thinking: bool,
    tool_uses: u64,
}

fn collect_agent_summary(
    t: &crate::orchestrator::TaskState,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
) -> AgentSummary {
    let orch = t
        .orchestrator_tmux
        .as_deref()
        .and_then(|n| sessions_by_tmux.get(n).copied());
    let workers: Vec<Option<&SessionInfo>> = t
        .workers
        .iter()
        .map(|w| sessions_by_tmux.get(w.tmux_name.as_str()).copied())
        .collect();

    let mut sum = AgentSummary {
        alive: 0,
        processing: 0,
        waiting: 0,
        idle: 0,
        inactive: 0,
        total: 0,
        total_ctx: 0,
        max_ctx: 0,
        max_ctx_pct: 0,
        current_tool: None,
        is_thinking: false,
        tool_uses: 0,
    };

    let mut tool_priority = 0u8; // prefer Processing > WaitingForInput tools
    for s in std::iter::once(orch)
        .chain(workers.iter().copied())
        .flatten()
    {
        sum.total += 1;
        match s.state {
            SessionState::Processing => {
                sum.processing += 1;
                sum.alive += 1;
            }
            // Question is the AskUserQuestion form of waiting — projects view
            // doesn't distinguish, so it rolls up into the waiting bucket.
            SessionState::WaitingForInput | SessionState::Question => {
                sum.waiting += 1;
                sum.alive += 1;
            }
            SessionState::Idle => {
                sum.idle += 1;
                sum.alive += 1;
            }
            SessionState::Inactive => {
                sum.inactive += 1;
            }
        }
        if let Some(c) = s.context_tokens {
            sum.total_ctx = sum.total_ctx.saturating_add(c);
            if c > sum.max_ctx {
                sum.max_ctx = c;
            }
            let cap = context_window_size(s.model.as_deref().unwrap_or("")).max(1);
            let pct = ((c.saturating_mul(100)) / cap).min(100) as u8;
            if pct > sum.max_ctx_pct {
                sum.max_ctx_pct = pct;
            }
        }
        sum.tool_uses = sum.tool_uses.saturating_add(s.tool_uses_count);
        let pri = match s.state {
            SessionState::Processing => 3,
            SessionState::WaitingForInput | SessionState::Question => 2,
            SessionState::Idle => 1,
            SessionState::Inactive => 0,
        };
        if pri > tool_priority {
            if let Some(tool) = &s.current_tool {
                sum.current_tool = Some((tool.name.clone(), tool.hint.clone()));
                tool_priority = pri;
            } else if s.is_thinking {
                sum.is_thinking = true;
                tool_priority = pri;
            }
        }
    }
    sum
}

/// Cheap variant of [`collect_agent_summary`] that only sums tool-use counts
/// across the orchestrator + workers. Used by the collapsed card renderer,
/// which needs the count for its footer badge but none of the state /
/// context-window aggregates.
fn sum_tool_uses(
    t: &crate::orchestrator::TaskState,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
) -> u64 {
    let orch = t
        .orchestrator_tmux
        .as_deref()
        .and_then(|n| sessions_by_tmux.get(n).copied());
    let workers = t
        .workers
        .iter()
        .filter_map(|w| sessions_by_tmux.get(w.tmux_name.as_str()).copied());
    std::iter::once(orch)
        .flatten()
        .chain(workers)
        .map(|s| s.tool_uses_count)
        .fold(0u64, |a, b| a.saturating_add(b))
}

/// Compact dot strip showing per-agent state. Up to ~12 dots; overflow
/// shows `+N`. Color: green=processing, yellow=waiting, gray=idle, dim=inactive.
fn agent_dot_strip(sum: &AgentSummary) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let total = sum.total as usize;
    if total == 0 {
        return spans;
    }
    let max_dots = 12usize;
    let shown = total.min(max_dots);
    // We want a stable ordering: processing → waiting → idle → inactive.
    let mut buckets = [
        (sum.processing, Color::LightGreen, "▶"),
        (sum.waiting, Color::LightYellow, "●"),
        (sum.idle, Color::Rgb(140, 140, 160), "○"),
        (sum.inactive, Color::Rgb(80, 80, 95), "·"),
    ];
    let mut left = shown;
    for (count, color, glyph) in buckets.iter_mut() {
        let take = (*count as usize).min(left);
        for _ in 0..take {
            spans.push(Span::styled(
                (*glyph).to_string(),
                Style::default().fg(*color),
            ));
        }
        left -= take;
        if left == 0 {
            break;
        }
    }
    if total > max_dots {
        spans.push(Span::styled(
            format!(" +{}", total - max_dots),
            Style::default().fg(Color::Rgb(140, 140, 160)),
        ));
    }
    spans
}

fn worker_was_merged(w: &crate::orchestrator::Worker, t: &crate::orchestrator::TaskState) -> bool {
    t.merges.iter().any(|m| {
        w.worktree.as_deref().is_some_and(|wn| m.worktree == wn)
            && matches!(m.outcome, crate::orchestrator::MergeOutcome::Ok)
    })
}

/// Merge progress glyph: `▰` per merged worker, `▱` per pending. Caps at
/// 8 segments, with a numeric tail for overflow.
fn merge_progress_spans(t: &crate::orchestrator::TaskState) -> Vec<Span<'static>> {
    let total = t.workers.len();
    if total == 0 {
        return vec![Span::styled(
            "merges —".to_string(),
            Style::default().fg(Color::Rgb(110, 110, 130)),
        )];
    }
    let merged = t.workers.iter().filter(|w| worker_was_merged(w, t)).count();
    let cap = 8usize;
    let shown = total.min(cap);
    let merged_shown = (merged.min(total) * shown + total / 2) / total;
    let mut spans = Vec::with_capacity(shown + 2);
    spans.push(Span::styled(
        "merges ",
        Style::default().fg(Color::Rgb(150, 150, 170)),
    ));
    for i in 0..shown {
        if i < merged_shown {
            spans.push(Span::styled("▰", Style::default().fg(Color::LightGreen)));
        } else {
            spans.push(Span::styled(
                "▱",
                Style::default().fg(Color::Rgb(90, 90, 110)),
            ));
        }
    }
    spans.push(Span::styled(
        format!(" {}/{}", merged, total),
        Style::default().fg(Color::Rgb(150, 150, 170)),
    ));
    spans
}

/// Color ramp for context utilization: green → yellow → orange → red.
fn ctx_color(pct: u8) -> Color {
    if pct >= 90 {
        Color::Rgb(220, 120, 120)
    } else if pct >= 70 {
        Color::Rgb(220, 200, 120)
    } else if pct >= 40 {
        Color::Rgb(180, 200, 140)
    } else {
        Color::Rgb(120, 180, 200)
    }
}

/// Build a unicode bar of `width` columns filled to `pct` (0..=100). Uses
/// 1/8-block glyphs so even a short width has visual gradation.
fn ctx_bar(pct: u8, width: usize) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let pct = pct.min(100) as usize;
    let total_eighths = (pct * width * 8 + 50) / 100; // round to nearest eighth
    let full = total_eighths / 8;
    let rem = total_eighths % 8;
    let partial_glyph = match rem {
        1 => Some("▏"),
        2 => Some("▎"),
        3 => Some("▍"),
        4 => Some("▌"),
        5 => Some("▋"),
        6 => Some("▊"),
        7 => Some("▉"),
        _ => None,
    };
    let color = ctx_color(pct as u8);
    let mut s = String::new();
    for _ in 0..full {
        s.push('█');
    }
    if let Some(g) = partial_glyph {
        s.push_str(g);
    }
    let drawn = full + if partial_glyph.is_some() { 1 } else { 0 };
    let mut out = Vec::with_capacity(2);
    out.push(Span::styled(s, Style::default().fg(color)));
    if drawn < width {
        let pad = "░".repeat(width - drawn);
        out.push(Span::styled(
            pad,
            Style::default().fg(Color::Rgb(50, 50, 65)),
        ));
    }
    out
}

/// Compact PR status badge for kanban cards. Surfaces the bits a reviewer
/// needs to triage at-a-glance — PR id, review state, comment count — so
/// the orchestrator's iterate-on-feedback loop is visible without opening
/// the PR-details popup. Colour weights:
/// * `changes_requested` is loud (the orchestrator needs attention).
/// * `open` is calm; comments perk it up a notch.
/// * `approved` is positive green.
/// * `merged` / `closed` are muted — terminal states.
fn pr_badge_spans(pr: &crate::projects_scan::PrCardSummary) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    spans.push(Span::styled(
        format!("󰊢 PR #{}", pr.id),
        Style::default().fg(Color::Rgb(150, 170, 200)),
    ));
    match pr.review_state {
        crate::pr::ReviewState::ChangesRequested => {
            spans.push(Span::styled(
                " · changes requested".to_string(),
                Style::default()
                    .fg(Color::Rgb(230, 150, 110))
                    .add_modifier(Modifier::BOLD),
            ));
            if pr.comments > 0 {
                spans.push(Span::styled(
                    format!(" · 󰭹 {}", pr.comments),
                    Style::default().fg(Color::Rgb(180, 180, 200)),
                ));
            }
        }
        crate::pr::ReviewState::Open => {
            if pr.comments > 0 {
                spans.push(Span::styled(
                    format!(" · 󰭹 {}", pr.comments),
                    Style::default().fg(Color::Rgb(180, 200, 230)),
                ));
            } else {
                spans.push(Span::styled(
                    " · open".to_string(),
                    Style::default().fg(Color::Rgb(150, 170, 200)),
                ));
            }
        }
        crate::pr::ReviewState::Approved => {
            spans.push(Span::styled(
                " · approved".to_string(),
                Style::default().fg(Color::LightGreen),
            ));
            if pr.comments > 0 {
                spans.push(Span::styled(
                    format!(" · 󰭹 {}", pr.comments),
                    Style::default().fg(Color::Rgb(180, 180, 200)),
                ));
            }
        }
        crate::pr::ReviewState::Merged => {
            spans.push(Span::styled(
                " · merged".to_string(),
                Style::default().fg(Color::Rgb(140, 160, 145)),
            ));
        }
        crate::pr::ReviewState::Closed => {
            spans.push(Span::styled(
                " · closed".to_string(),
                Style::default().fg(Color::Rgb(110, 120, 135)),
            ));
        }
    }
    spans
}

/// `(done, total)` if the task has a checklist, else `None`. Both card
/// renderers use this to decide whether to draw the `☑ M/N` badge.
fn todos_progress(t: &crate::orchestrator::TaskState) -> Option<(usize, usize)> {
    if t.todos.is_empty() {
        return None;
    }
    let done = t.todos.iter().filter(|i| i.done).count();
    Some((done, t.todos.len()))
}

/// Sessions-style rich card for a Running task. Mirrors the layout of the
/// Sessions grid card: bordered, multi-row, with status emoji, agent dots,
/// merge glyph, ctx bar, and live tool/thinking line.
#[allow(clippy::too_many_arguments)]
fn render_task_card_active(
    frame: &mut Frame,
    area: Rect,
    t: &crate::orchestrator::TaskState,
    selected: bool,
    col_idx: usize,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
    pr_summary: Option<&crate::projects_scan::PrCardSummary>,
    now_secs: u64,
    titling_in_flight: bool,
) {
    let sum = collect_agent_summary(t, sessions_by_tmux);

    // Planning vs Running share the rich layout but differ in accent +
    // title icon so the column matches the card visually.
    let (accent, title_icon) = if col_idx == 0 {
        (Color::Rgb(170, 140, 210), "󰟶")
    } else {
        (Color::LightYellow, "󰒓")
    };
    let (border_type, border_color) = if selected {
        (BorderType::Double, Color::White)
    } else if sum.waiting > 0 {
        (BorderType::Rounded, Color::Rgb(220, 200, 120))
    } else {
        (BorderType::Rounded, Color::Rgb(80, 90, 110))
    };

    let short_id = crate::orchestrator::short_task_id(&t.task_id);
    let prompt_max = (area.width as usize).saturating_sub(8);
    let header_text = task_card_header_text(t, titling_in_flight, prompt_max);
    let title_spans = vec![
        Span::styled(format!(" {} ", title_icon), Style::default().fg(accent)),
        Span::styled(
            header_text,
            Style::default()
                .fg(if selected { Color::White } else { Color::Gray })
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::raw(" "),
    ];
    let title = Line::from(title_spans);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width < 4 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(inner.height as usize);

    // Row 1: note (orchestrator status) or first line of summary fallback.
    let note_text = t
        .note
        .as_deref()
        .map(|n| models::first_line_truncated(n, inner.width.saturating_sub(4) as usize))
        .unwrap_or_else(|| {
            let s = t.summary.as_deref().unwrap_or("");
            models::first_line_truncated(s, inner.width.saturating_sub(4) as usize)
        });
    if !note_text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("󰍡 ", Style::default().fg(Color::Rgb(150, 170, 200))),
            Span::styled(note_text, Style::default().fg(Color::Rgb(190, 190, 210))),
        ]));
    } else {
        lines.push(Line::from(Span::raw("")));
    }

    // PR badge row — inserted above agent dots so a `changes requested`
    // bounce-back is obvious before the eye scans the rest of the card.
    // The live-tool row at the bottom is allowed to fall off when present
    // (the card height is fixed at 4 inner rows).
    if let Some(pr) = pr_summary {
        lines.push(Line::from(pr_badge_spans(pr)));
    }

    // Row 2: agent dot strip + merge glyph.
    let mut row2: Vec<Span<'static>> = Vec::new();
    row2.push(Span::styled(
        "agents ",
        Style::default().fg(Color::Rgb(150, 150, 170)),
    ));
    row2.extend(agent_dot_strip(&sum));
    row2.push(Span::raw("   "));
    row2.extend(merge_progress_spans(t));
    lines.push(Line::from(row2));

    let age = models::relative_age(now_secs.saturating_sub(t.updated_at as u64));
    let arts = t.artifacts.len();
    let mut row3: Vec<Span<'static>> = vec![
        Span::styled(
            format!("#{}  ", short_id),
            Style::default().fg(TASK_META_DIM),
        ),
        Span::styled("󰔟 ", Style::default().fg(Color::Rgb(150, 150, 170))),
        Span::styled(age, Style::default().fg(Color::Rgb(180, 180, 200))),
    ];
    if arts > 0 {
        row3.push(Span::styled(
            format!("   󰉂 {}", arts),
            Style::default().fg(Color::Rgb(180, 160, 220)),
        ));
    }
    if let Some((done, total)) = todos_progress(t) {
        row3.push(Span::styled(
            format!("   ☑ {}/{}", done, total),
            Style::default().fg(Color::Rgb(180, 180, 200)),
        ));
    }
    if sum.tool_uses > 0 {
        row3.push(Span::styled(
            format!("   󰠰 {}", sum.tool_uses),
            Style::default().fg(Color::Rgb(180, 200, 160)),
        ));
    }
    let left_w: usize = row3.iter().map(|s| s.content.chars().count()).sum();
    let pct = sum.max_ctx_pct;
    let ctx_label = format!("  󰍛 {}% ", pct);
    let bar_label_w = ctx_label.chars().count();
    let bar_w = (inner.width as usize)
        .saturating_sub(left_w + bar_label_w)
        .min(20);
    if bar_w >= 4 {
        row3.push(Span::styled(ctx_label, Style::default().fg(ctx_color(pct))));
        row3.extend(ctx_bar(pct, bar_w));
    } else {
        row3.push(Span::styled(
            format!("  󰍛 {}%", pct),
            Style::default().fg(ctx_color(pct)),
        ));
    }
    lines.push(Line::from(row3));

    // Row 4: live tool / thinking line — only if we have one.
    if let Some((tool, hint)) = &sum.current_tool {
        let name = short_tool(tool);
        let max = inner.width.saturating_sub(4) as usize;
        let txt = match hint.as_deref().filter(|h| !h.is_empty()) {
            Some(h) => format!(
                "󰖷 {}: {}",
                name,
                models::first_line_truncated(h, max.saturating_sub(name.len() + 4))
            ),
            None => format!("󰖷 {}", name),
        };
        lines.push(Line::from(Span::styled(
            txt,
            Style::default().fg(Color::Rgb(180, 200, 160)),
        )));
    } else if sum.is_thinking {
        lines.push(Line::from(Span::styled(
            "󰟶 thinking",
            Style::default().fg(Color::Rgb(170, 140, 210)),
        )));
    }

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// Borrowed view of [`crate::merge_lock::MergeLock`] plus the holder's
/// resolved title, supplied to Merging-column cards so a queued card can
/// distinguish a healthy queue from one stuck behind a wedged orchestrator.
struct MergeLockBanner<'a> {
    task_id: &'a str,
    title: Option<&'a str>,
    acquired_at: i64,
    phase: crate::merge_lock::MergePhase,
    pr_id: Option<u32>,
}

/// Compact 3-line card for Review/Merging/Done tasks. Dim border, single-line
/// prompt, footer with age + summary preview + artifact/merge counts.
/// `col_idx` is one of 2 (Review), 3 (Merging), or 4 (Done).
/// `lock_holder` is the merge-lock holder context for this project, only
/// supplied for col_idx == 3. A Merging card whose id differs from the
/// holder is "queued" and renders with a muted border plus a waiting line
/// naming the holder and lock age, plus a tooltip showing the current phase
/// (e.g. "PR #N in /simplify (4m)") so the user can tell at a glance whether
/// the queue is healthy or stuck.
#[allow(clippy::too_many_arguments)]
fn render_task_card_collapsed(
    frame: &mut Frame,
    area: Rect,
    t: &crate::orchestrator::TaskState,
    selected: bool,
    col_idx: usize,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
    pr_summary: Option<&crate::projects_scan::PrCardSummary>,
    now_secs: u64,
    titling_in_flight: bool,
    lock_holder: Option<&MergeLockBanner<'_>>,
) {
    use crate::merge_lock::MergePhase;
    // Review (2) cyan, Merging (3) magenta, Done (4) green.
    let (accent, dim_text, icon) = match col_idx {
        2 => (Color::LightCyan, Color::Rgb(140, 175, 185), "󱋲"),
        3 => (Color::LightMagenta, Color::Rgb(180, 145, 195), ""),
        _ => (Color::LightGreen, Color::Rgb(140, 160, 145), "󰸞"),
    };
    let queued = col_idx == 3 && lock_holder.is_some_and(|h| h.task_id != t.task_id);
    let merging_self = col_idx == 3 && lock_holder.is_some_and(|h| h.task_id == t.task_id);
    // Review and Merging cards: brighter border so they stand out — they
    // need user attention or are actively mutating main. Done stays dim.
    // A queued Merging card uses a muted gray instead of the bright
    // magenta — approved, but waiting behind another merge.
    let (border_type, border_color) = if selected {
        (BorderType::Double, Color::White)
    } else if col_idx == 2 {
        (BorderType::Rounded, Color::Rgb(110, 170, 180))
    } else if queued {
        (BorderType::Rounded, TASK_META_DIM)
    } else if col_idx == 3 {
        (BorderType::Rounded, Color::Rgb(170, 130, 180))
    } else {
        (BorderType::Rounded, Color::Rgb(55, 60, 70))
    };
    let icon_accent = if queued {
        Color::Rgb(135, 135, 155)
    } else {
        accent
    };

    let short_id = crate::orchestrator::short_task_id(&t.task_id);
    let prompt_max = (area.width as usize).saturating_sub(8);
    let header_text = task_card_header_text(t, titling_in_flight, prompt_max);
    let mut title_spans = vec![
        Span::styled(format!(" {} ", icon), Style::default().fg(icon_accent)),
        Span::styled(
            header_text,
            Style::default()
                .fg(if selected { Color::White } else { dim_text })
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ];
    if queued {
        title_spans.push(Span::styled(
            " queued ",
            Style::default()
                .fg(Color::Rgb(170, 170, 185))
                .bg(Color::Rgb(45, 45, 55)),
        ));
    } else if merging_self {
        title_spans.push(Span::styled(
            " merging ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(190, 145, 210))
                .add_modifier(Modifier::BOLD),
        ));
    }
    title_spans.push(Span::raw(" "));
    let title = Line::from(title_spans);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width < 4 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    if queued {
        // t.summary/t.note for an approved-but-waiting PR is already shown
        // on its Review card; on the Merging card the user wants who is
        // blocking and for how long instead.
        if let Some(h) = lock_holder {
            let body_max = inner.width.saturating_sub(4) as usize;
            let lock_age =
                format_duration_secs(now_secs.saturating_sub(h.acquired_at.max(0) as u64));
            let holder_short = crate::orchestrator::short_task_id(h.task_id);
            let age_suffix = format!(" · {}", lock_age);
            let holder_title = h.title.map(str::trim).filter(|s| !s.is_empty());
            let body = match holder_title {
                Some(title) => {
                    let prefix = format!("behind #{} · ", holder_short);
                    let reserved = prefix.chars().count() + age_suffix.chars().count();
                    let title_room = body_max.saturating_sub(reserved);
                    format!(
                        "{}{}{}",
                        prefix,
                        models::first_line_truncated(title, title_room),
                        age_suffix
                    )
                }
                None => format!("behind #{}{}", holder_short, age_suffix),
            };
            lines.push(Line::from(vec![
                Span::styled("󰔟 ", Style::default().fg(Color::Rgb(110, 120, 135))),
                Span::styled(body, Style::default().fg(Color::Rgb(170, 170, 185))),
            ]));
        }
    } else {
        let summary_text = t
            .summary
            .as_deref()
            .or(t.note.as_deref())
            .map(|s| models::first_line_truncated(s, inner.width.saturating_sub(4) as usize))
            .unwrap_or_default();
        if !summary_text.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("󰍡 ", Style::default().fg(Color::Rgb(110, 120, 135))),
                Span::styled(summary_text, Style::default().fg(Color::Rgb(160, 165, 175))),
            ]));
        }
    }
    if let Some(v) = t.shipped_version.as_deref().filter(|s| !s.is_empty()) {
        lines.push(Line::from(vec![
            Span::styled("󰓹 ", Style::default().fg(Color::Rgb(110, 120, 135))),
            shipped_version_span(v),
        ]));
    }

    if queued {
        let banner = lock_holder.expect("queued implies holder");
        let age = models::relative_age(now_secs.saturating_sub(banner.acquired_at as u64));
        let phase_label = match banner.phase {
            MergePhase::Merging => "merging",
            MergePhase::Simplify => "/simplify",
            MergePhase::Bump => "/bump",
            MergePhase::FinalizePending => "finalizing",
        };
        let leader = match banner.pr_id {
            Some(n) => format!("PR #{} in {} ({})", n, phase_label, age),
            None => format!(
                "behind {} in {} ({})",
                crate::orchestrator::short_task_id(banner.task_id),
                phase_label,
                age
            ),
        };
        lines.push(Line::from(vec![
            Span::styled("⏳ ", Style::default().fg(Color::Rgb(110, 120, 135))),
            Span::styled(leader, Style::default().fg(Color::Rgb(150, 150, 170))),
        ]));
    }

    let age = models::relative_age(now_secs.saturating_sub(t.updated_at as u64));
    let arts = t.artifacts.len();
    let merged = t.workers.iter().filter(|w| worker_was_merged(w, t)).count();
    let total_w = t.workers.len();
    let mut footer: Vec<Span<'static>> = vec![
        Span::styled(
            format!("#{}  ", short_id),
            Style::default().fg(TASK_META_DIM),
        ),
        Span::styled("󰔟 ", Style::default().fg(Color::Rgb(110, 120, 135))),
        Span::styled(age, Style::default().fg(Color::Rgb(140, 145, 160))),
    ];
    if total_w > 0 {
        footer.push(Span::raw("   "));
        footer.push(Span::styled(
            format!("merges {}/{}", merged, total_w),
            Style::default().fg(Color::Rgb(140, 145, 160)),
        ));
    }
    if arts > 0 {
        footer.push(Span::raw("   "));
        footer.push(Span::styled(
            format!("󰉂 {}", arts),
            Style::default().fg(Color::Rgb(160, 145, 195)),
        ));
    }
    if let Some((done, total)) = todos_progress(t) {
        footer.push(Span::raw("   "));
        footer.push(Span::styled(
            format!("☑ {}/{}", done, total),
            Style::default().fg(Color::Rgb(140, 145, 160)),
        ));
    }
    let tool_uses = sum_tool_uses(t, sessions_by_tmux);
    if tool_uses > 0 {
        footer.push(Span::raw("   "));
        footer.push(Span::styled(
            format!("󰠰 {}", tool_uses),
            Style::default().fg(Color::Rgb(180, 200, 160)),
        ));
    }
    if let Some(pr) = pr_summary {
        footer.push(Span::raw("   "));
        footer.extend(pr_badge_spans(pr));
    }
    lines.push(Line::from(footer));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

fn task_card_header_text(
    t: &crate::orchestrator::TaskState,
    titling_in_flight: bool,
    prompt_max: usize,
) -> String {
    match t.title.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => models::first_line_truncated(name, prompt_max),
        None if titling_in_flight => models::first_line_truncated("✎ …", prompt_max),
        None => models::first_line_truncated(&t.prompt, prompt_max),
    }
}

/// Styled `vX.Y.Z` span for the shipped-version display. Idempotent on the
/// `v` prefix so `0.1` and `v0.1` both render as `v0.1`.
fn shipped_version_span(v: &str) -> Span<'static> {
    Span::styled(
        format!("v{}", v.trim_start_matches('v')),
        Style::default().fg(Color::Rgb(150, 200, 165)),
    )
}

fn render_metrics_body(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.height < 2 {
        return;
    }

    let m = match &app.metrics {
        Some(m) => m,
        None => {
            let text = match app.metrics_progress {
                Some((scanned, total)) if total > 0 => {
                    let pct = (scanned as f64 / total as f64 * 100.0).round() as u64;
                    format!(
                        " Scanning agent transcripts … {} / {} sessions ({}%)",
                        scanned, total, pct
                    )
                }
                _ => " Scanning agent transcripts …".to_string(),
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::DarkGray),
                ))),
                area,
            );
            return;
        }
    };

    let (lines, row_lines) = build_metrics_content(m, app.metrics_selected);
    let total_lines = lines.len() as u16;
    let body_height = area.height.saturating_sub(1);
    let max_scroll = total_lines.saturating_sub(body_height);

    // With a row selected, keep it inside the viewport; otherwise honour the
    // user's free scroll. Either way clamp to the content height and write the
    // result back, so selection and scroll stay in sync — releasing the
    // selection (up past the first row) then resumes scrolling from here.
    let scroll = match app.metrics_selected.and_then(|i| row_lines.get(i).copied()) {
        Some(line_idx) => {
            let line = line_idx as u16;
            let current = app.metrics_scroll;
            if line < current {
                line
            } else if body_height > 0 && line >= current + body_height {
                line + 1 - body_height
            } else {
                current
            }
        }
        None => app.metrics_scroll,
    }
    .min(max_scroll);

    // Hand the row offsets and viewport height to the key handler so a downward
    // press can tell "engage the first on-screen session" from "scroll toward
    // the lists". Done after reading `row_lines` above (this moves it).
    app.metrics_view_height = body_height;
    app.metrics_row_lines = row_lines;
    app.metrics_scroll = scroll;

    let scroll_info = format!(
        " {}/{} ",
        (scroll as usize).min(total_lines.saturating_sub(1) as usize) + 1,
        total_lines
    );
    let indicator_area = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            scroll_info,
            Style::default().fg(Color::Rgb(80, 80, 95)),
        )))
        .alignment(Alignment::Right),
        indicator_area,
    );

    let body_area = Rect::new(area.x, area.y, area.width, body_height);
    let content = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(content, body_area);
}

/// Returns the rendered line buffer plus the logical-line index of every
/// selectable session row, in the same canonical order as
/// [`MetricsAnalysis::selectable_sessions`]. `selected` (an index into that
/// flat list) controls which row, if any, gets highlighted.
fn build_metrics_content(
    m: &MetricsAnalysis,
    selected: Option<usize>,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row_lines: Vec<usize> = Vec::new();
    let mut global_row: usize = 0;
    let dim = Style::default().fg(Color::DarkGray);
    let label = Style::default().fg(Color::Rgb(140, 140, 160));
    let val = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    lines.push(section_header("Overview"));
    lines.push(Line::from(vec![
        Span::styled("  Total cost   ", label),
        Span::styled(fmt_cost(m.total_cost), val.fg(Color::Green)),
        Span::styled("    Sessions ", label),
        Span::styled(format!("{}", m.total_sessions), val),
        Span::styled("    Messages ", label),
        Span::styled(format!("{}", m.total_messages), val),
        Span::styled("    Cache hit ", label),
        Span::styled(format!("{:.0}%", m.cache_hit_rate * 100.0), val),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Tokens      ", label),
        Span::styled(
            format!(
                "{} in / {} out / {} cache_r / {} cache_w",
                format_tokens(m.total_tokens.input),
                format_tokens(m.total_tokens.output),
                format_tokens(m.total_tokens.cache_read),
                format_tokens(m.total_tokens.cache_creation),
            ),
            val,
        ),
    ]));
    lines.push(Line::raw(""));

    lines.push(section_header("Cost breakdown"));
    let breakdown = [
        (
            "input        ",
            m.total_tokens.input,
            Color::Rgb(120, 200, 240),
        ),
        (
            "output       ",
            m.total_tokens.output,
            Color::Rgb(240, 180, 120),
        ),
        (
            "cache read   ",
            m.total_tokens.cache_read,
            Color::Rgb(160, 220, 160),
        ),
        (
            "cache create ",
            m.total_tokens.cache_creation,
            Color::Rgb(220, 160, 200),
        ),
    ];
    let max_tokens = breakdown
        .iter()
        .map(|(_, t, _)| *t)
        .max()
        .unwrap_or(0)
        .max(1);
    for (name, toks, col) in breakdown {
        let bar_w = ((toks as f64 / max_tokens as f64) * 30.0).round() as usize;
        let bar: String = "━".repeat(bar_w);
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", name), label),
            Span::styled(bar, Style::default().fg(col)),
            Span::raw(" "),
            Span::styled(format_tokens(toks), dim),
        ]));
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Cost by model"));
    let mut models: Vec<(&String, &ModelStats)> = m.by_model.iter().collect();
    models.sort_by(|a, b| {
        b.1.cost
            .partial_cmp(&a.1.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let max_model_cost = models.first().map(|(_, s)| s.cost).unwrap_or(0.0).max(0.01);
    for (name, s) in models.iter().take(8) {
        let pct = if m.total_cost > 0.0 {
            s.cost / m.total_cost * 100.0
        } else {
            0.0
        };
        let bar_w = ((s.cost / max_model_cost) * 26.0).round() as usize;
        let short = short_model(name);
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<22}", models::first_line_truncated(short, 22)),
                label,
            ),
            Span::styled("━".repeat(bar_w), Style::default().fg(model_color(name))),
            Span::raw(" "),
            Span::styled(fmt_cost(s.cost), val),
            Span::styled(format!(" {:>4.1}%", pct), dim),
            Span::styled(format!("  {} msgs", s.messages), dim),
        ]));
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Daily spending (last 30 days)"));
    let today = chrono::Local::now().date_naive();
    let mut days: Vec<f64> = (0..30)
        .rev()
        .map(|n| {
            let day = today - ChronoDuration::days(n as i64);
            m.by_day.get(&day).map(|d| d.cost).unwrap_or(0.0)
        })
        .collect();
    let day_max = days.iter().cloned().fold(0f64, f64::max).max(0.01);
    let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let spark: String = days
        .iter_mut()
        .map(|c| {
            if *c <= 0.0 {
                ' '
            } else {
                let idx = ((*c / day_max) * 7.0).round().clamp(0.0, 7.0) as usize;
                blocks[idx]
            }
        })
        .collect();
    let last_7_total: f64 = days.iter().rev().take(7).sum();
    let last_30_total: f64 = days.iter().sum();
    lines.push(Line::from(vec![
        Span::styled("  ", dim),
        Span::styled(spark, Style::default().fg(Color::Rgb(150, 200, 240))),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  last 7d ", label),
        Span::styled(fmt_cost(last_7_total), val),
        Span::styled("    last 30d ", label),
        Span::styled(fmt_cost(last_30_total), val),
        Span::styled("    peak day ", label),
        Span::styled(fmt_cost(day_max), val),
    ]));
    lines.push(Line::raw(""));

    lines.push(section_header("Top projects"));
    let max_proj = m
        .top_projects
        .first()
        .map(|(_, s)| s.cost)
        .unwrap_or(0.0)
        .max(0.01);
    for (name, s) in &m.top_projects {
        let bar_w = ((s.cost / max_proj) * 24.0).round() as usize;
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<26}", models::first_line_truncated(name, 26)),
                label,
            ),
            Span::styled(
                "━".repeat(bar_w),
                Style::default().fg(Color::Rgb(120, 180, 220)),
            ),
            Span::raw(" "),
            Span::styled(fmt_cost(s.cost), val),
            Span::styled(format!("  {} sess", s.sessions), dim),
            Span::styled(format!("  {} msgs", s.messages), dim),
        ]));
    }
    lines.push(Line::raw(""));

    let styles = MetricsStyles { dim, label, val };
    render_bar_chart_section(
        &mut lines,
        "Tool usage",
        "tool calls",
        "tools",
        &m.by_tool,
        &styles,
    );
    render_bar_chart_section(
        &mut lines,
        "Shell commands",
        "shell commands",
        "commands",
        &m.by_shell,
        &styles,
    );
    render_bar_chart_section(
        &mut lines,
        "MCP servers",
        "MCP calls",
        "servers",
        &m.by_mcp,
        &styles,
    );

    lines.push(section_header("Interruptions (Esc'd mid-tool-call)"));
    let i = &m.interruptions;
    if i.total_interrupted_turns == 0 {
        lines.push(Line::from(Span::styled("  (none detected)", dim)));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  Wasted ", label),
            Span::styled(
                fmt_cost(i.total_wasted_cost),
                val.fg(Color::Rgb(220, 140, 140)),
            ),
            Span::styled("    Turns ", label),
            Span::styled(format!("{}", i.total_interrupted_turns), val),
            Span::styled("    Sessions ", label),
            Span::styled(format!("{}", i.sessions_affected), val),
        ]));
        for entry in i.by_session.iter() {
            let sid = short_sid(&entry.session_id).to_string();
            let (marker, sid_style) = selection_row_style(selected == Some(global_row));
            row_lines.push(lines.len());
            lines.push(Line::from(vec![
                Span::styled(format!("{}{:<10}", marker, sid), sid_style),
                Span::styled(
                    format!("{:>8}", fmt_cost(entry.wasted_cost)),
                    val.fg(Color::Rgb(220, 140, 140)),
                ),
                Span::styled(format!("  {:>3} orphan", entry.orphan_count), dim),
                Span::raw("  "),
                Span::styled(
                    format!(
                        "{:<18}",
                        models::first_line_truncated(&entry.last_tool_name, 18)
                    ),
                    Style::default().fg(Color::Rgb(180, 180, 200)),
                ),
                Span::styled(
                    format!("{:<24}", models::first_line_truncated(&entry.project, 24)),
                    Style::default().fg(Color::Rgb(180, 180, 200)),
                ),
            ]));
            global_row += 1;
        }
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Peak context reached"));
    let pc = &m.peak_context;
    if pc.findings.is_empty() {
        lines.push(Line::from(Span::styled("  (no sessions)", dim)));
    } else {
        for f in pc.findings.iter() {
            let sid = short_sid(&f.session_id).to_string();
            let (marker, sid_style) = selection_row_style(selected == Some(global_row));
            row_lines.push(lines.len());
            lines.push(Line::from(vec![
                Span::styled(format!("{}{:<10}", marker, sid), sid_style),
                Span::styled(
                    format!("{:>8} ctx", format_tokens(f.peak_ctx_tokens)),
                    val.fg(Color::Rgb(220, 180, 130)),
                ),
                Span::styled(
                    format!("  {:>8}", fmt_cost(f.total_cost)),
                    val.fg(Color::Green),
                ),
                Span::styled(
                    format!("  @ turn {}/{}", f.peak_turn_index, f.assistant_turns),
                    dim,
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<24}", models::first_line_truncated(&f.project, 24)),
                    Style::default().fg(Color::Rgb(180, 180, 200)),
                ),
            ]));
            global_row += 1;
        }
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Token spikes (outlier single-turn deltas)"));
    let g = &m.context_growth;
    lines.push(Line::from(vec![
        Span::styled("  Scored ", label),
        Span::styled(format!("{}", g.sessions_scored), val),
        Span::styled("    Spikes ", label),
        Span::styled(format!("{}", g.findings.len()), val),
        Span::styled("    Cost in flagged sessions ", label),
        Span::styled(
            fmt_cost(g.anomalous_cost),
            val.fg(Color::Rgb(220, 180, 130)),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "  score = peak turn delta / median turn delta — flags one-shot bursts, not total growth",
        dim,
    )));
    if g.findings.is_empty() {
        lines.push(Line::from(Span::styled("  (no spikes)", dim)));
    } else {
        for f in g.findings.iter() {
            let sid = short_sid(&f.session_id).to_string();
            let (marker, sid_style) = selection_row_style(selected == Some(global_row));
            row_lines.push(lines.len());
            lines.push(Line::from(vec![
                Span::styled(format!("{}{:<10}", marker, sid), sid_style),
                Span::styled(
                    format!("{:>5.1}x", f.score),
                    val.fg(Color::Rgb(220, 180, 130)),
                ),
                Span::styled(
                    format!("  {:>8}", fmt_cost(f.total_cost)),
                    val.fg(Color::Green),
                ),
                Span::styled(
                    format!(
                        "  +{:>8} @ turn {}/{}",
                        format_tokens(f.peak_delta_tokens),
                        f.peak_turn_index,
                        f.assistant_turns
                    ),
                    dim,
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<24}", models::first_line_truncated(&f.project, 24)),
                    Style::default().fg(Color::Rgb(180, 180, 200)),
                ),
            ]));
            global_row += 1;
        }
    }
    lines.push(Line::raw(""));

    lines.push(section_header("Top sessions"));
    lines.push(Line::from(Span::styled(
        format!(
            "  {:<10} {:>8} {:>10} {:<22} {:<24}",
            "session", "cost", "tokens", "model", "project"
        ),
        dim,
    )));
    for s in &m.top_sessions {
        let is_sel = selected == Some(global_row);
        row_lines.push(lines.len());
        lines.push(format_session_row(s, dim, val, is_sel));
        global_row += 1;
    }

    (lines, row_lines)
}

const TOOLS_DISPLAY_LIMIT: usize = 15;

struct MetricsStyles {
    dim: Style,
    label: Style,
    val: Style,
}

fn render_bar_chart_section(
    lines: &mut Vec<Line<'static>>,
    header: &str,
    empty_noun: &str,
    overflow_noun: &str,
    stats: &std::collections::BTreeMap<String, ToolStats>,
    s: &MetricsStyles,
) {
    let (dim, label, val) = (s.dim, s.label, s.val);
    lines.push(section_header(header));
    let mut rows: Vec<(&String, &ToolStats)> = stats.iter().collect();
    rows.sort_by_key(|t| std::cmp::Reverse(t.1.count));
    let total_calls: u64 = rows.iter().map(|(_, s)| s.count).sum();
    let max_count = rows.first().map(|(_, s)| s.count).unwrap_or(0).max(1);
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  (no {} recorded)", empty_noun),
            dim,
        )));
    } else {
        for (name, s) in rows.iter().take(TOOLS_DISPLAY_LIMIT) {
            let bar_w = ((s.count as f64 / max_count as f64) * 24.0).round() as usize;
            let pct = if total_calls > 0 {
                s.count as f64 / total_calls as f64 * 100.0
            } else {
                0.0
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<22}", models::first_line_truncated(name, 22)),
                    label,
                ),
                Span::styled("━".repeat(bar_w), Style::default().fg(tool_color(name))),
                Span::raw(" "),
                Span::styled(format!("{:>6} calls", s.count), val),
                Span::styled(format!("  {:>4.1}%", pct), dim),
                Span::styled(format!("  {} sess", s.sessions), dim),
            ]));
        }
        if rows.len() > TOOLS_DISPLAY_LIMIT {
            lines.push(Line::from(Span::styled(
                format!(
                    "  … {} more {}",
                    rows.len() - TOOLS_DISPLAY_LIMIT,
                    overflow_noun
                ),
                dim,
            )));
        }
    }
    lines.push(Line::raw(""));
}

fn tool_color(name: &str) -> Color {
    // Stable hash → palette so the same tool keeps the same color.
    let mut h: u32 = 0x811c_9dc5;
    for b in name.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    let palette: [Color; 8] = [
        Color::Rgb(120, 200, 240),
        Color::Rgb(240, 180, 120),
        Color::Rgb(160, 220, 160),
        Color::Rgb(220, 160, 200),
        Color::Rgb(200, 180, 240),
        Color::Rgb(240, 220, 140),
        Color::Rgb(140, 220, 220),
        Color::Rgb(220, 160, 140),
    ];
    palette[(h as usize) % palette.len()]
}

fn selection_row_style(selected: bool) -> (&'static str, Style) {
    if selected {
        (
            "  ▸ ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("    ", Style::default().fg(Color::Cyan))
    }
}

fn format_session_row(s: &SessionSummary, dim: Style, val: Style, selected: bool) -> Line<'static> {
    let sid = short_sid(&s.session_id).to_string();
    let subagent = if s.is_subagent { "⑂" } else { " " };
    let mark = if selected {
        format!("▸ {}", subagent)
    } else {
        format!("  {}", subagent)
    };
    let toks = format_tokens(s.tokens.total());
    let model = short_model(&s.model);
    let (_, sid_style) = selection_row_style(selected);
    Line::from(vec![
        Span::styled(format!("{}{:<8}", mark, sid), sid_style),
        Span::raw(" "),
        Span::styled(format!("{:>8}", fmt_cost(s.cost)), val.fg(Color::Green)),
        Span::raw(" "),
        Span::styled(format!("{:>10}", toks), dim),
        Span::raw(" "),
        Span::styled(
            format!("{:<22}", models::first_line_truncated(model, 22)),
            dim,
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<24}", models::first_line_truncated(&s.project, 24)),
            Style::default().fg(Color::Rgb(180, 180, 200)),
        ),
    ])
}

fn section_header(title: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("▎ ", Style::default().fg(Color::Rgb(120, 140, 180))),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn model_color(model: &str) -> Color {
    let s = short_model(model);
    if s.contains("opus") {
        Color::Rgb(220, 150, 220)
    } else if s.contains("sonnet") {
        Color::Rgb(150, 200, 240)
    } else if s.contains("haiku") {
        Color::Rgb(160, 220, 180)
    } else {
        Color::Rgb(180, 180, 180)
    }
}

#[cfg(test)]
fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod result_popup_tests {
    use super::buffer_to_string;
    use crate::app::App;
    use crate::orchestrator::{Artifact, Project, TaskState, TaskStatus};
    use crate::projects_scan::ProjectsSnapshot;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Returns the y-coordinate of the first row that contains `needle`, or
    /// `None` when the substring is missing. Used to assert the proof-first
    /// vertical ordering of the redesigned popup (note → lead → others →
    /// summary appendix).
    fn row_of(buf: &Buffer, needle: &str) -> Option<u16> {
        for y in 0..buf.area().height {
            let mut row = String::new();
            for x in 0..buf.area().width {
                row.push_str(buf[(x, y)].symbol());
            }
            if row.contains(needle) {
                return Some(y);
            }
        }
        None
    }

    #[test]
    fn expanded_text_artifact_scrolls_with_popup_scroll() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let doc_path = tmp.path().join("tour.md");
        let body = (1..=80)
            .map(|n| format!("line {:02}", n))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&doc_path, body).unwrap();

        let now = crate::orchestrator::now_unix_secs();
        let project = Project {
            id: "p-scroll".into(),
            name: "scroll".into(),
            root: PathBuf::from("/tmp/scroll"),
            created_at: now,
            build_cmd: None,
        };
        let mut state = TaskState::new(
            project.id.clone(),
            project.root.clone(),
            "scroll prompt".into(),
        );
        state.status = TaskStatus::Done;
        state.note = Some("headline".into());
        state.artifacts = vec![Artifact {
            kind: "file".into(),
            path: doc_path.to_string_lossy().into_owned(),
            original: doc_path.to_string_lossy().into_owned(),
            caption: Some("long doc".into()),
            added_at: now,
        }];
        state.lead_artifact = Some(0);

        let mut app = App::new();
        let mut tasks = HashMap::new();
        tasks.insert(project.id.clone(), vec![std::sync::Arc::new(state)]);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks,
            titling: std::collections::HashSet::new(),
            merge_lock_holders: std::collections::HashMap::new(),
            merge_lock_holder_pr_ids: std::collections::HashMap::new(),
            pr_summaries: std::collections::HashMap::new(),
        };
        app.update_projects(snap);
        assert!(app.enter_projects_result(), "popup should open");
        app.result_artifact_expanded = true;

        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");
        let first = buffer_to_string(terminal.backend().buffer());
        assert!(
            first.contains("line 01"),
            "top of doc should be visible\n{}",
            first
        );

        app.result_scroll_by(30);
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");
        let second = buffer_to_string(terminal.backend().buffer());
        assert!(
            second.contains("line 16") || second.contains("line 17"),
            "scrolling should reveal later lines of the text artifact\n{}",
            second
        );
        assert!(
            !second.contains("line 01"),
            "once scrolled down, the preview should not stay pinned to the first line\n{}",
            second
        );
    }

    #[test]
    fn evidence_inlines_log_url_and_image_fallback() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("build.log");
        std::fs::write(
            &log_path,
            "compiling cc-hub-lib\nfinished release in 12.3s\n",
        )
        .unwrap();

        let now = crate::orchestrator::now_unix_secs();
        let project = Project {
            id: "p-test".into(),
            name: "test".into(),
            root: PathBuf::from("/tmp/test"),
            created_at: now,
            build_cmd: None,
        };
        let mut state = TaskState::new(
            project.id.clone(),
            project.root.clone(),
            "test prompt body".into(),
        );
        state.status = TaskStatus::Done;
        state.note = Some("PROOF-LINE: build is green".into());
        state.summary = Some("WHY this works: build green; popup shows evidence cards.".into());
        state.artifacts = vec![
            Artifact {
                kind: "build".into(),
                path: log_path.to_string_lossy().into_owned(),
                original: log_path.to_string_lossy().into_owned(),
                caption: Some("cargo build --release".into()),
                added_at: now,
            },
            Artifact {
                kind: "url".into(),
                path: "https://example.com/ci/build/42".into(),
                original: "https://example.com/ci/build/42".into(),
                caption: Some("CI build".into()),
                added_at: now,
            },
            Artifact {
                kind: "screenshot".into(),
                path: "/nonexistent/missing-screenshot.png".into(),
                original: "/nonexistent/missing-screenshot.png".into(),
                caption: Some("preview".into()),
                added_at: now,
            },
        ];
        // Designate the build log as the lead artifact so it renders first
        // and at the lead body height.
        state.lead_artifact = Some(0);

        let mut app = App::new();
        let mut tasks = HashMap::new();
        tasks.insert(project.id.clone(), vec![std::sync::Arc::new(state)]);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks,
            titling: std::collections::HashSet::new(),
            merge_lock_holders: std::collections::HashMap::new(),
            merge_lock_holder_pr_ids: std::collections::HashMap::new(),
            pr_summaries: std::collections::HashMap::new(),
        };
        app.update_projects(snap);
        assert!(app.enter_projects_result(), "popup should open");

        // Tall buffer so the entire canvas (note → lead → others → summary
        // appendix) fits without scrolling — the test asserts vertical order,
        // so everything must be visible in one frame.
        let backend = TestBackend::new(120, 60);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");

        let buf = terminal.backend().buffer().clone();
        let dump = buffer_to_string(&buf);

        assert!(dump.contains("Result"), "should render Result title");
        assert!(
            dump.contains("PROOF-LINE: build is green"),
            "note headline should appear above evidence\n{}",
            dump
        );
        assert!(
            dump.contains("WHY this works"),
            "summary text should be inlined as appendix\n{}",
            dump
        );
        assert!(
            dump.contains("cargo build"),
            "log card should inline file caption\n{}",
            dump
        );
        assert!(
            dump.contains("compiling cc-hub-lib"),
            "log card body should inline file content\n{}",
            dump
        );
        assert!(
            dump.contains("lead"),
            "lead artifact card header should carry a `lead` tag\n{}",
            dump
        );
        assert!(
            dump.contains("press"),
            "url/video card should hint at `o`\n{}",
            dump
        );
        assert!(
            dump.contains("https://example.com/ci/build/42"),
            "url card should show URL\n{}",
            dump
        );
        // Image with no decoded data and no picker → falls back to a placeholder
        // (one of the two messages the renderer emits).
        assert!(
            dump.contains("[image preview unavailable") || dump.contains("[image hidden"),
            "image fallback placeholder should appear\n{}",
            dump
        );

        // Vertical ordering: note → lead body → other artifact body → summary.
        let y_note = row_of(&buf, "PROOF-LINE").expect("note row");
        let y_lead_body = row_of(&buf, "compiling cc-hub-lib").expect("lead body row");
        let y_url = row_of(&buf, "https://example.com").expect("url body row");
        let y_summary = row_of(&buf, "WHY this works").expect("summary appendix row");
        assert!(
            y_note < y_lead_body && y_lead_body < y_url && y_url < y_summary,
            "expected order note({}) < lead({}) < url({}) < summary({})\n{}",
            y_note,
            y_lead_body,
            y_url,
            y_summary,
            dump,
        );
    }

    fn buffer_bg_map(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                let c = &buf[(x, y)];
                let m = match c.bg {
                    ratatui::style::Color::Rgb(34, 92, 43) => '+',
                    ratatui::style::Color::Rgb(122, 41, 54) => '-',
                    ratatui::style::Color::Reset => '.',
                    _ => '?',
                };
                out.push(m);
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn diff_artifact_renders_with_claude_style_backgrounds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let patch_path = tmp.path().join("sample.patch");
        let patch = "\
diff --git a/lib/src/ui.rs b/lib/src/ui.rs
index 0000001..0000002 100644
--- a/lib/src/ui.rs
+++ b/lib/src/ui.rs
@@ -42,3 +42,5 @@ fn render_status() {
     let mut spans = Vec::new();
-    spans.push(span_dim(&state.title));
+    spans.push(span_bold(&state.title));
+    if let Some(badge) = &state.badge {
+        spans.push(span_subtle(badge));
+    }
     Line::from(spans)
@@ -101,3 +103,3 @@ fn render_bar() {
     let bar = renderer.bar();
-    bar.draw(area);
+    bar.draw_with_offset(area, offset);
     Ok(())
";
        std::fs::write(&patch_path, patch).unwrap();

        let now = crate::orchestrator::now_unix_secs();
        let project = Project {
            id: "p-diff".into(),
            name: "diff".into(),
            root: PathBuf::from("/tmp/diff"),
            created_at: now,
            build_cmd: None,
        };
        let mut state = TaskState::new(
            project.id.clone(),
            project.root.clone(),
            "diff prompt".into(),
        );
        state.status = TaskStatus::Done;
        state.summary = Some("WHY: diff renderer matches Claude Code style.".into());
        state.artifacts = vec![Artifact {
            kind: "diff".into(),
            path: patch_path.to_string_lossy().into_owned(),
            original: patch_path.to_string_lossy().into_owned(),
            caption: Some("ui.rs: structured diff".into()),
            added_at: now,
        }];

        let mut app = App::new();
        let mut tasks = HashMap::new();
        tasks.insert(project.id.clone(), vec![std::sync::Arc::new(state)]);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks,
            titling: std::collections::HashSet::new(),
            merge_lock_holders: std::collections::HashMap::new(),
            merge_lock_holder_pr_ids: std::collections::HashMap::new(),
            pr_summaries: std::collections::HashMap::new(),
        };
        app.update_projects(snap);
        assert!(app.enter_projects_result(), "popup should open");

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");

        let buf = terminal.backend().buffer().clone();
        let plain = buffer_to_string(&buf);
        let bg_map = buffer_bg_map(&buf);
        assert!(
            plain.contains("Result"),
            "should render Result title\n{}",
            plain
        );
        assert!(
            plain.contains("lib/src/ui.rs"),
            "diff per-file header path should be visible\n{}",
            plain
        );
        assert!(
            plain.contains("Added") && plain.contains("removed"),
            "diff header counts line should be visible\n{}",
            plain
        );
        assert!(
            !plain.contains("@@ -"),
            "raw @@ hunk header should be suppressed\n{}",
            plain
        );
        assert!(
            bg_map.contains('+'),
            "added rows should paint the diffAdded bg across cells\n{}",
            bg_map
        );
        assert!(
            bg_map.contains('-'),
            "removed rows should paint the diffRemoved bg across cells\n{}",
            bg_map
        );
        assert!(
            bg_map.contains("..."),
            "hunk separator marker should render in dim gray\n{}",
            plain
        );
    }
}

#[cfg(test)]
mod kanban_card_tests {
    use super::buffer_to_string;
    use crate::orchestrator::{TaskState, TaskStatus, TodoItem};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn task_with_todos(status: TaskStatus, done: usize, total: usize) -> TaskState {
        let mut t = TaskState::new("p".into(), PathBuf::from("/tmp/p"), "prompt".into());
        t.status = status;
        t.title = Some("test card".into());
        t.todos = (0..total)
            .map(|i| TodoItem {
                text: format!("step {}", i),
                done: i < done,
            })
            .collect();
        t
    }

    #[test]
    fn collapsed_card_shows_todos_badge() {
        let t = task_with_todos(TaskStatus::Review, 2, 4);
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    2,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("2/4"),
            "collapsed card should show 2/4 badge:\n{}",
            plain
        );
    }

    #[test]
    fn collapsed_card_omits_badge_when_no_todos() {
        let t = task_with_todos(TaskStatus::Review, 0, 0);
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    2,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(!plain.contains("☑"), "no todos => no badge:\n{}", plain);
    }

    fn fake_session(tmux: &str, tool_uses: u64) -> super::SessionInfo {
        use crate::agent::AgentKind;
        use crate::models::SessionState;
        super::SessionInfo {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            pid: 0,
            session_id: "sid-x".into(),
            cwd: "/tmp".into(),
            project_name: "p".into(),
            started_at: 0,
            last_activity: None,
            state: SessionState::Processing,
            last_user_message: None,
            summary: None,
            title: None,
            titling: false,
            model: None,
            git_branch: None,
            version: None,
            jsonl_path: None,
            tmux_session: Some(tmux.into()),
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: tool_uses,
        }
    }

    /// Render a session card's title row (y=0) as one string of cell symbols.
    fn title_row(s: &super::SessionInfo, w: u16) -> String {
        let backend = TestBackend::new(w, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_card(f, f.area(), s, None, false, 1_000_000_000))
            .expect("render");
        let buf = terminal.backend().buffer();
        (0..buf.area().width)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect()
    }

    // The state-indicator glyph is a Nerd Font icon that renders two columns
    // wide but measures as one, so the cell at its second-column position must
    // belong to the title (a space), never the border — otherwise a
    // border-only redraw overpaints the glyph's right half and it looks "cut".
    // Worst case is a no-title Claude card, whose title is just the bare icon.
    #[test]
    fn card_title_keeps_a_blank_after_the_state_icon() {
        use crate::models::SessionState;
        let mut s = fake_session("wk-1", 0);
        s.state = SessionState::WaitingForInput; // "󰂞"
        s.project_name = "cc-hub".into();

        for agent in ["claude", "codex"] {
            for (titling, title) in [
                (false, None),
                (true, None),
                (false, Some("Fix the parser".to_string())),
            ] {
                let mut s = s.clone();
                s.agent_id = agent.into();
                s.titling = titling;
                s.title = title.clone();
                for w in [42u16, 24, 16, 13] {
                    let backend = TestBackend::new(w, 6);
                    let mut terminal = Terminal::new(backend).expect("terminal");
                    terminal
                        .draw(|f| super::render_card(f, f.area(), &s, None, false, 1_000_000_000))
                        .expect("render");
                    let buf = terminal.backend().buffer();
                    let gx = (0..buf.area().width)
                        .find(|&x| buf[(x, 0)].symbol() == "󰂞")
                        .unwrap_or_else(|| {
                            panic!(
                                "icon missing (agent={agent}, w={w}):\n{}",
                                buffer_to_string(buf)
                            )
                        });
                    let right = buf[(gx + 1, 0)].symbol();
                    assert_eq!(
                        right, " ",
                        "icon needs a reserved blank to its right \
                         (agent={agent}, titling={titling}, title={title:?}, w={w}, got {right:?}):\n{}",
                        buffer_to_string(buf)
                    );
                }
            }
        }
    }

    // The title drops the redundant "[Claude]" badge and the project name
    // (the project is already the card group's header). Non-Claude agents
    // still get a badge so mixed fleets stay legible.
    #[test]
    fn card_title_omits_claude_badge_and_project_name() {
        let mut s = fake_session("wk-1", 0);
        s.project_name = "cc-hub".into();
        s.title = Some("Fix the parser".into());

        let claude = title_row(&s, 60);
        assert!(
            !claude.contains("Claude") && !claude.contains("cc-hub"),
            "Claude card title must not show the agent badge or project name:\n{claude}"
        );
        assert!(
            claude.contains("Fix the parser"),
            "Claude card title should still show the Haiku title:\n{claude}"
        );

        s.agent_id = "codex".into();
        let codex = title_row(&s, 60);
        assert!(
            codex.contains("codex"),
            "non-Claude agents should still show a badge:\n{codex}"
        );
    }

    fn task_with_worker(status: TaskStatus, worker_tmux: &str) -> TaskState {
        use crate::agent::AgentKind;
        use crate::orchestrator::Worker;
        let mut t = TaskState::new("p".into(), PathBuf::from("/tmp/p"), "prompt".into());
        t.status = status;
        t.title = Some("test card".into());
        t.workers.push(Worker {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            tmux_name: worker_tmux.into(),
            cwd: PathBuf::from("/tmp/p"),
            worktree: None,
            readonly: false,
            spawned_at: 0,
        });
        t
    }

    #[test]
    fn active_card_shows_tool_calls_badge() {
        let t = task_with_worker(TaskStatus::Running, "wk-1");
        let session = fake_session("wk-1", 7);
        let mut sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        sessions.insert("wk-1", &session);
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_active(
                    f,
                    f.area(),
                    &t,
                    false,
                    1,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("󰠰 7") || plain.contains(" 7"),
            "active card should show tool-uses badge with 7:\n{}",
            plain
        );
        assert!(
            plain.contains("󰠰"),
            "active card should show tool glyph:\n{}",
            plain
        );
    }

    #[test]
    fn active_card_omits_tool_calls_when_zero() {
        let t = task_with_todos(TaskStatus::Running, 0, 0);
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_active(
                    f,
                    f.area(),
                    &t,
                    false,
                    1,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(!plain.contains("󰠰"), "no tool uses => no badge:\n{}", plain);
    }

    #[test]
    fn collapsed_card_shows_tool_calls_badge() {
        let t = task_with_worker(TaskStatus::Review, "wk-2");
        let session = fake_session("wk-2", 5);
        let mut sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        sessions.insert("wk-2", &session);
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    2,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("󰠰") && plain.contains("5"),
            "collapsed card should show tool-uses badge with 5:\n{}",
            plain
        );
    }

    #[test]
    fn active_card_shows_todos_badge() {
        let t = task_with_todos(TaskStatus::Running, 2, 4);
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_active(
                    f,
                    f.area(),
                    &t,
                    false,
                    1,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("2/4"),
            "active card should show 2/4 badge:\n{}",
            plain
        );
    }

    fn merging_task(task_id: &str) -> TaskState {
        let mut t = TaskState::new("p".into(), PathBuf::from("/tmp/p"), "prompt".into());
        t.task_id = task_id.to_string();
        t.status = TaskStatus::Merging;
        t.title = Some("waiting card".into());
        t
    }

    fn pr_summary(
        id: u32,
        state: crate::pr::ReviewState,
        comments: u16,
    ) -> crate::projects_scan::PrCardSummary {
        crate::projects_scan::PrCardSummary {
            id,
            review_state: state,
            comments,
        }
    }

    fn render_active(
        t: &TaskState,
        pr: Option<&crate::projects_scan::PrCardSummary>,
        col_idx: usize,
    ) -> String {
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(70, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_active(
                    f,
                    f.area(),
                    t,
                    false,
                    col_idx,
                    &sessions,
                    pr,
                    1_000_000_000,
                    false,
                )
            })
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    fn render_collapsed(
        t: &TaskState,
        pr: Option<&crate::projects_scan::PrCardSummary>,
        col_idx: usize,
    ) -> String {
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(70, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    t,
                    false,
                    col_idx,
                    &sessions,
                    pr,
                    1_000_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn collapsed_card_shows_holder_context_when_queued() {
        let t = merging_task("t-self-000111");
        let now: u64 = 1_700_000_000;
        let banner = super::MergeLockBanner {
            task_id: "t-blocker-123456",
            title: Some("fix flaky tests"),
            acquired_at: (now - 180) as i64,
            phase: crate::merge_lock::MergePhase::Merging,
            pr_id: None,
        };
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner),
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("behind"),
            "queued card should name the holder:\n{}",
            plain
        );
        assert!(
            plain.contains("123456"),
            "queued card should show holder short id:\n{}",
            plain
        );
        assert!(
            plain.contains("3m"),
            "queued card should show 3m lock age:\n{}",
            plain
        );
        assert!(
            plain.contains("queued"),
            "queued card should keep the queued pill:\n{}",
            plain
        );
    }

    #[test]
    fn queued_collapsed_card_shows_holder_phase_and_pr() {
        use crate::merge_lock::MergePhase;
        let t = merging_task("t-self-000222");
        let now: u64 = 1_700_000_000;
        let banner = super::MergeLockBanner {
            task_id: "t-other-000333",
            title: Some("blocking work"),
            acquired_at: (now - 240) as i64,
            phase: MergePhase::Simplify,
            pr_id: Some(7),
        };
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(70, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner),
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("PR #7"),
            "queued card should show holder PR id:\n{}",
            plain
        );
        assert!(
            plain.contains("/simplify"),
            "queued card should show holder phase:\n{}",
            plain
        );
        assert!(
            plain.contains("4m"),
            "queued card should show holder age:\n{}",
            plain
        );
    }

    #[test]
    fn collapsed_card_shows_merging_pill_when_self_is_holder() {
        let t = merging_task("t-self-654321");
        let now: u64 = 1_700_000_000;
        let banner = super::MergeLockBanner {
            task_id: "t-self-654321",
            title: Some("self merge"),
            acquired_at: (now - 5) as i64,
            phase: crate::merge_lock::MergePhase::Merging,
            pr_id: None,
        };
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner),
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("merging"),
            "self-holder card should show the merging pill:\n{}",
            plain
        );
        assert!(
            !plain.contains("behind"),
            "self-holder card must not render a waiting line:\n{}",
            plain
        );
    }

    /// Visual-proof helper — not part of the regression suite. Dumps the
    /// rendered card buffers for three Merging states (queued, holder, no
    /// lock) to /tmp so a screenshot artifact can be attached to the PR.
    /// Run with: `cargo test -p cc-hub-lib dump_merging_states -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_merging_states() {
        let now: u64 = 1_700_000_000;
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();

        let queued = merging_task("t-self-000111");
        let banner_blocking = super::MergeLockBanner {
            task_id: "t-blocker-123456",
            title: Some("fix flaky tests"),
            acquired_at: (now - 180) as i64,
            phase: crate::merge_lock::MergePhase::Merging,
            pr_id: None,
        };
        let backend = TestBackend::new(60, 4);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &queued,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner_blocking),
                )
            })
            .expect("render");
        let queued_render = buffer_to_string(terminal.backend().buffer());

        let holder = merging_task("t-blocker-123456");
        let banner_self = super::MergeLockBanner {
            task_id: "t-blocker-123456",
            title: Some("fix flaky tests"),
            acquired_at: (now - 12) as i64,
            phase: crate::merge_lock::MergePhase::Merging,
            pr_id: None,
        };
        let backend = TestBackend::new(60, 4);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &holder,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner_self),
                )
            })
            .expect("render");
        let holder_render = buffer_to_string(terminal.backend().buffer());

        let alone = merging_task("t-self-000111");
        let backend = TestBackend::new(60, 4);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &alone,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    None,
                )
            })
            .expect("render");
        let alone_render = buffer_to_string(terminal.backend().buffer());

        let dump = format!(
            "=== Merging column · QUEUED (lock held by another task) ===\n\
             {queued}\n\
             === Merging column · ACTIVE (this card holds the lock) ===\n\
             {holder}\n\
             === Merging column · NO LOCK (no holder recorded) ===\n\
             {alone}\n",
            queued = queued_render,
            holder = holder_render,
            alone = alone_render,
        );
        std::fs::write("/tmp/cchub-merging-states.txt", &dump).expect("write dump");
        eprintln!("{}", dump);
    }

    #[test]
    fn collapsed_card_no_holder_context_when_no_lock() {
        let t = merging_task("t-only-999999");
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    3,
                    &sessions,
                    None,
                    1_700_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            !plain.contains("behind"),
            "no holder => no waiting line:\n{}",
            plain
        );
        assert!(
            !plain.contains("queued"),
            "no holder => no queued pill:\n{}",
            plain
        );
    }

    #[test]
    fn active_card_shows_pr_changes_requested_badge() {
        let t = task_with_todos(TaskStatus::Running, 0, 0);
        let pr = pr_summary(42, crate::pr::ReviewState::ChangesRequested, 3);
        let plain = render_active(&t, Some(&pr), 1);
        assert!(
            plain.contains("PR #42") && plain.contains("changes requested"),
            "changes_requested badge missing:\n{}",
            plain
        );
        assert!(plain.contains("3"), "comment count missing:\n{}", plain);
    }

    #[test]
    fn collapsed_card_shows_pr_open_with_comment_count() {
        let t = task_with_todos(TaskStatus::Review, 0, 0);
        let pr = pr_summary(7, crate::pr::ReviewState::Open, 2);
        let plain = render_collapsed(&t, Some(&pr), 2);
        assert!(plain.contains("PR #7"), "PR id missing:\n{}", plain);
        assert!(
            plain.contains("2"),
            "comment count missing on open PR:\n{}",
            plain
        );
    }

    #[test]
    fn collapsed_card_open_without_comments_shows_open_label() {
        let t = task_with_todos(TaskStatus::Review, 0, 0);
        let pr = pr_summary(9, crate::pr::ReviewState::Open, 0);
        let plain = render_collapsed(&t, Some(&pr), 2);
        assert!(plain.contains("PR #9"), "PR id missing:\n{}", plain);
        assert!(
            plain.contains("open"),
            "open label missing when no comments:\n{}",
            plain
        );
    }

    #[test]
    fn active_card_omits_pr_badge_when_no_pr() {
        let t = task_with_todos(TaskStatus::Running, 0, 0);
        let plain = render_active(&t, None, 1);
        assert!(!plain.contains("PR #"), "no PR ⇒ no badge:\n{}", plain);
    }
}

#[cfg(test)]
mod backlog_popup_tests {
    use super::buffer_to_string;
    use crate::app::App;
    use crate::orchestrator::{Project, TaskState};
    use crate::projects_scan::ProjectsSnapshot;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn render_popup(prompts: &[&str], titles: &[Option<&str>]) -> String {
        assert_eq!(prompts.len(), titles.len());
        let now = crate::orchestrator::now_unix_secs();
        let project = Project {
            id: "p-backlog".into(),
            name: "backlog".into(),
            root: PathBuf::from("/tmp/backlog"),
            created_at: now,
            build_cmd: None,
        };
        let tasks: Vec<Arc<TaskState>> = prompts
            .iter()
            .zip(titles.iter())
            .map(|(p, title)| {
                let mut t =
                    TaskState::new_backlog(project.id.clone(), project.root.clone(), (*p).into());
                t.title = title.map(|s| s.to_string());
                Arc::new(t)
            })
            .collect();
        let mut tasks_map = HashMap::new();
        tasks_map.insert(project.id.clone(), tasks);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks: tasks_map,
            titling: HashSet::new(),
            merge_lock_holders: HashMap::new(),
            merge_lock_holder_pr_ids: HashMap::new(),
            pr_summaries: HashMap::new(),
        };
        let mut app = App::new();
        app.update_projects(snap);
        app.open_backlog();
        let backend = TestBackend::new(100, 22);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_backlog(f, f.area(), &app))
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    /// Renders mixed titled + untitled tasks and dumps the buffer to the path
    /// in `CC_HUB_BACKLOG_RENDER_DUMP`. Used to capture a proof-of-fix artifact
    /// for PRs; skipped under normal `cargo test`.
    #[test]
    #[ignore]
    fn dump_render_for_artifact() {
        let Some(dest) = std::env::var_os("CC_HUB_BACKLOG_RENDER_DUMP") else {
            return;
        };
        let prompts = [
            "wire up exporter for daily reports",
            "refactor the merge-lock retry policy",
            "investigate flaky CI on macOS runners",
            "ship the new exporter",
        ];
        let titles: [Option<&str>; 4] = [None, None, None, Some("Exporter rollout")];
        let plain = render_popup(&prompts, &titles);
        std::fs::write(&dest, plain).expect("write dump");
    }

    /// Like `dump_render_for_artifact`, but the focused task carries a
    /// realistic multi-line prompt (bug repro / fix sketch / acceptance
    /// criteria) — the case the split-pane refactor was motivated by.
    /// Dump path comes from `CC_HUB_BACKLOG_RENDER_DUMP_MULTILINE`.
    #[test]
    #[ignore]
    fn dump_render_for_artifact_multiline() {
        let Some(dest) = std::env::var_os("CC_HUB_BACKLOG_RENDER_DUMP_MULTILINE") else {
            return;
        };
        let multiline = "\
In lib/src/ui.rs::render_backlog, each backlog entry only shows a one-line\n\
preview. Explorer-loop tasks carry full repro + fix sketch + acceptance\n\
criteria — none of which is visible.\n\
\n\
Fix: split the popup into list (left) + body (right). Reuse j/k.\n\
\n\
Acceptance:\n\
- Full prompt visible without leaving the popup.\n\
- Narrow terminals clip gracefully.\n\
- Existing keybinds keep working.";
        let prompts = [
            multiline,
            "refactor the merge-lock retry policy",
            "investigate flaky CI on macOS runners",
        ];
        let titles: [Option<&str>; 3] = [Some("Backlog popup: show full prompt"), None, None];
        let plain = render_popup(&prompts, &titles);
        std::fs::write(&dest, plain).expect("write dump");
    }

    #[test]
    fn untitled_entries_do_not_duplicate_prompt_preview() {
        let prompts = [
            "wire up exporter for daily reports",
            "refactor the merge-lock retry policy",
            "investigate flaky CI on macOS runners",
        ];
        let titles: [Option<&str>; 3] = [None, None, None];
        let plain = render_popup(&prompts, &titles);

        // The list pane is 40 cols wide and `first_line_truncated` ellipsises
        // anything past ~26 chars, so these 34-char prompts are truncated in
        // the list for every task. The body pane (right half) renders the
        // *selected* task's full prompt untouched. Therefore:
        //   - selected (index 0, since backlog_sel defaults to 0): count == 1
        //   - non-selected: count == 0
        // This still catches both regressions of interest: the body pane
        // silently dropping the prompt (selected → 0) and a list-pane
        // re-introduction of the original doubling bug (selected → 2+).
        for (i, p) in prompts.iter().enumerate() {
            let count = plain.matches(p).count();
            let expected = if i == 0 { 1 } else { 0 };
            assert_eq!(
                count,
                expected,
                "prompt {:?} (selected={}) should appear {} time(s), got {}:\n{}",
                p,
                i == 0,
                expected,
                count,
                plain
            );
        }
        // The pending-title placeholder should appear once per untitled task.
        let pending = plain.matches("pending title").count();
        assert_eq!(
            pending,
            prompts.len(),
            "expected one 'pending title' hint per untitled task:\n{}",
            plain
        );
    }

    #[test]
    fn titled_entries_keep_title_then_id_prompt_layout() {
        let prompts = ["ship the new exporter"];
        let titles = [Some("Exporter rollout")];
        let plain = render_popup(&prompts, &titles);
        assert!(
            plain.contains("Exporter rollout"),
            "title should render on row 1:\n{}",
            plain
        );
        assert!(
            plain.contains("ship the new exporter"),
            "prompt preview should still render on row 2:\n{}",
            plain
        );
        // No pending-title hint when the title has landed.
        assert!(
            !plain.contains("pending title"),
            "titled entry should not show 'pending title' hint:\n{}",
            plain
        );
    }
}

#[cfg(test)]
mod backlog_age_tests {
    use super::buffer_to_string;
    use crate::app::App;
    use crate::orchestrator::{now_unix_secs, Project, TaskState, TaskStatus};
    use crate::projects_scan::ProjectsSnapshot;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn backlog_entries_show_right_justified_age_column() {
        let now = now_unix_secs();
        let project = Project {
            id: "p-bk".into(),
            name: "demo".into(),
            root: PathBuf::from("/tmp/demo"),
            created_at: now,
            build_cmd: None,
        };
        let ages: [(i64, &str); 4] = [
            (45, "45s"),
            (12 * 60, "12m"),
            (3 * 3600, "3h"),
            (2 * 86400, "2d"),
        ];
        let tasks: Vec<Arc<TaskState>> = ages
            .iter()
            .enumerate()
            .map(|(i, (age_s, _))| {
                let mut s = TaskState::new(
                    project.id.clone(),
                    project.root.clone(),
                    format!("prompt number {}", i),
                );
                s.status = TaskStatus::Backlog;
                s.created_at = now - age_s;
                Arc::new(s)
            })
            .collect();
        let mut app = App::new();
        let mut by_proj = HashMap::new();
        by_proj.insert(project.id.clone(), tasks);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks: by_proj,
            titling: HashSet::new(),
            merge_lock_holders: HashMap::new(),
            merge_lock_holder_pr_ids: HashMap::new(),
            pr_summaries: HashMap::new(),
        };
        app.update_projects(snap);
        app.open_backlog();

        let backend = TestBackend::new(90, 22);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_backlog(f, f.area(), &app))
            .expect("render");
        let rendered = buffer_to_string(terminal.backend().buffer());
        println!(
            "---begin backlog render---\n{}---end backlog render---",
            rendered
        );

        for (_, token) in ages.iter() {
            let padded = format!("{:>4}", token);
            assert!(
                rendered.contains(&padded),
                "expected right-justified age {:?} in backlog render:\n{}",
                padded,
                rendered,
            );
        }
    }
}

#[cfg(test)]
mod wrap_text_tests {
    use super::wrap_text;

    #[test]
    fn wraps_on_word_boundaries() {
        assert_eq!(
            wrap_text("the quick brown fox", 9),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn hard_splits_a_word_longer_than_width() {
        assert_eq!(wrap_text("abcdefgh", 3), vec!["abc", "def", "gh"]);
    }

    #[test]
    fn long_word_breaks_after_flushing_the_current_line() {
        assert_eq!(wrap_text("hi abcdefgh", 3), vec!["hi", "abc", "def", "gh"]);
    }

    #[test]
    fn empty_or_blank_text_yields_one_empty_row() {
        assert_eq!(wrap_text("", 10), vec![String::new()]);
        assert_eq!(wrap_text("   ", 10), vec![String::new()]);
    }

    #[test]
    fn zero_width_is_clamped_to_one_column() {
        assert_eq!(wrap_text("ab", 0), vec!["a", "b"]);
    }
}

// Unix-only: `with_temp_home` redirects `$HOME` so `todo.add` doesn't touch the
// real `~/.cc-hub` — the same isolation the todo module's own tests rely on.
#[cfg(all(test, unix))]
mod todo_panel_tests {
    use super::buffer_to_string;
    use crate::app::{App, View};
    use crate::test_util::with_temp_home;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn long_item_wraps_instead_of_clipping() {
        with_temp_home(|| {
            let mut app = App::new();
            // Longer than the panel's ~38-column text area, so it must wrap to
            // a second row instead of clipping the tail off the right edge.
            let long = "remember to refactor the orchestrator retry backoff logic today";
            app.todo.add(long);
            app.view = View::TodoPanel;

            let backend = TestBackend::new(60, 12);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_todo_panel(f, f.area(), &app))
                .expect("render");
            let rendered = buffer_to_string(terminal.backend().buffer());

            // Every word survives somewhere in the panel — the tail words would
            // be missing if the row were clipped rather than wrapped.
            for word in long.split_whitespace() {
                assert!(
                    rendered.contains(word),
                    "word {:?} should appear in the wrapped panel:\n{}",
                    word,
                    rendered
                );
            }
        });
    }
}
