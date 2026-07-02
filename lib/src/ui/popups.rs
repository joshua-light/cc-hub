//! Overlay views: folder picker, gh-create / prompt / rename inputs, confirm
//! dialogs, the to-do side panel, the embedded tmux pane, the state-debug
//! popup, and the live transcript tail.

use crate::app::{App, PendingConfirm};
use crate::config;
use crate::conversation::{StateExplanation, Verdict};
use crate::folder_picker::{FolderPicker, PickerMode, PlaceSource};
use crate::models::SessionInfo;
use crate::ui::common::{centered_fixed, centered_rect, format_tokens, popup_block, state_color};
use crate::ui::main_layout;
use crate::ui::palette::{ACCENT_BLUE, CONTEXT_GRAY, DIM_TEXT, GRAY_80};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

pub(crate) fn render_folder_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.folder_picker.as_ref() else {
        return;
    };
    let assigning = app.tasks.pending_assign.is_some();
    if picker.mode == PickerMode::Places {
        render_places_picker(frame, area, picker, assigning);
        return;
    }
    let bookmarks_mode = picker.mode == PickerMode::Bookmarks;

    let popup = centered_fixed(area, 80, 24);
    frame.render_widget(Clear, popup);

    let (title_text, footer_text, empty_text) = if bookmarks_mode {
        (
            " New session · bookmarks ",
            " j/k:move · enter/space:pick · m:unbookmark · esc:cancel ",
            "  (no bookmarks — press N to browse, then m on a folder)",
        )
    } else if assigning {
        (
            " Assign task · pick folder ",
            " enter:descend · bksp:parent · space/.:pick · tab:projects · esc:cancel ",
            "  (no subdirectories)",
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

/// Places mode of the assign / new-session picker: a flat, fuzzy-filterable
/// list of known directories (registered projects, bookmarks, recent cwds).
/// The top row is the live filter; matched chars are highlighted in each row.
fn render_places_picker(frame: &mut Frame, area: Rect, picker: &FolderPicker, assigning: bool) {
    let popup = centered_fixed(area, 80, 24);
    frame.render_widget(Clear, popup);

    let title = if assigning {
        " Assign task · pick project "
    } else {
        " New session · pick project "
    };
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " type:filter · ↑/↓:move · enter/space:pick · tab:browse · esc:cancel ",
        Style::default().fg(DIM_TEXT),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 {
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
            format!("{}/{} ", picker.rows.len(), picker.places.len()),
            Style::default().fg(DIM_TEXT),
        )))
        .alignment(Alignment::Right),
        filter_area,
    );

    let list_h = inner.height - 2;
    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, list_h);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches — backspace to widen, tab to browse)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_h as usize;
        let start = picker.selection.saturating_sub(visible.saturating_sub(1));
        for (i, row) in picker.rows.iter().enumerate().skip(start).take(visible) {
            let Some(place) = picker.places.get(row.place) else {
                continue;
            };
            let selected = i == picker.selection;
            // Selected rows render as a solid white bar, so every span
            // needs the bg set or the bar shows gaps.
            let bar = if selected {
                Style::default().bg(Color::White)
            } else {
                Style::default()
            };
            let (name_base, name_hl, path_base, path_hl) = if selected {
                (
                    bar.fg(Color::Black).add_modifier(Modifier::BOLD),
                    bar.fg(Color::Blue).add_modifier(Modifier::BOLD),
                    bar.fg(Color::Rgb(90, 90, 100)),
                    bar.fg(Color::Blue),
                )
            } else {
                (
                    Style::default().fg(Color::Rgb(200, 200, 210)),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::Cyan),
                )
            };
            let cursor = Span::styled(
                if selected { "▶ " } else { "  " },
                bar.fg(Color::Black).add_modifier(Modifier::BOLD),
            );
            let badge = match place.source {
                PlaceSource::Project => Span::styled("◆ ", bar.fg(Color::Cyan)),
                PlaceSource::Bookmark => Span::styled("★ ", bar.fg(Color::Yellow)),
                PlaceSource::Recent => Span::styled("· ", bar.fg(Color::DarkGray)),
            };
            let mut spans = vec![cursor, badge];
            spans.extend(highlight_spans(
                &place.name,
                &row.name_indices,
                name_base,
                name_hl,
            ));
            spans.push(Span::styled("  ", bar));
            spans.extend(highlight_spans(
                &place.display_path,
                &row.path_indices,
                path_base,
                path_hl,
            ));
            lines.push(Line::from(spans));
        }
    }
    frame.render_widget(Paragraph::new(lines), list_area);
}

/// Split `text` into spans alternating `base`/`hl` style, with `hl` on the
/// chars at `indices` (sorted, as produced by the fuzzy matcher).
fn highlight_spans(text: &str, indices: &[usize], base: Style, hl: Style) -> Vec<Span<'static>> {
    if indices.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let mut spans = Vec::new();
    let mut cur = String::new();
    let mut cur_hl = false;
    for (i, ch) in text.chars().enumerate() {
        let is_hl = indices.binary_search(&i).is_ok();
        if is_hl != cur_hl && !cur.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut cur),
                if cur_hl { hl } else { base },
            ));
        }
        cur_hl = is_hl;
        cur.push(ch);
    }
    if !cur.is_empty() {
        spans.push(Span::styled(cur, if cur_hl { hl } else { base }));
    }
    spans
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
            .projects
            .pending_cwd
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
        let need = if cur_len == 0 {
            wlen
        } else {
            cur_len + 1 + wlen
        };
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

/// Visual rows one logical `Line` occupies when a `Paragraph` with
/// `Wrap { trim: false }` renders it into `width` columns. ratatui scrolls a
/// wrapped paragraph by these *wrapped rows*, not by logical lines, so every
/// scroll clamp and jump target has to be summed in this unit — counting
/// logical lines leaves the last wrapped screenful permanently unreachable.
///
/// Delegates to ratatui's own `Paragraph::line_count` (the
/// `unstable-rendered-line-info` feature) so the count is by construction the
/// renderer's: a hand-rolled mirror of `WordWrapper` diverged on
/// whitespace-led rows, trailing spaces, tabs, and wide (CJK/emoji) chars —
/// under-counts made the last screenful unreachable again, over-counts let
/// auto-follow scroll past the bottom into blank rows.
pub(crate) fn wrapped_total_rows(lines: &[Line], width: u16) -> u16 {
    if width == 0 {
        return lines.len().min(u16::MAX as usize) as u16;
    }
    Paragraph::new(Text::from(lines.to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(lines.len().min(1))
        .min(u16::MAX as usize) as u16
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

    let done = app.todo.list.items().iter().filter(|i| i.done).count();
    let total = app.todo.list.len();
    let block = popup_block(Span::styled(
        format!(" To-Do · {}/{} done ", done, total),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        if app.todo.adding {
            " enter add · esc cancel "
        } else {
            " a add · space toggle · d delete · c clear done · esc close "
        },
        Style::default().fg(DIM_TEXT),
    ));

    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Reserve the bottom two rows (spacer + input) in add mode so a long list
    // can never push the input line off-screen.
    let input_rows = if app.todo.adding { 2usize } else { 0 };
    let list_rows = (inner.height as usize).saturating_sub(input_rows);

    let mut lines: Vec<Line> = Vec::new();
    if total == 0 && !app.todo.adding {
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
        let sel = app.todo.selected.min(total.saturating_sub(1));

        let mut rows: Vec<Line> = Vec::new();
        let mut item_start: Vec<usize> = Vec::with_capacity(total);
        for (i, item) in app.todo.list.items().iter().enumerate() {
            item_start.push(rows.len());
            let selected = !app.todo.adding && i == sel;
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

    if app.todo.adding {
        let mut input = app.todo.input.clone();
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

/// Centered single-line input for a new Tasks-tab task. Same shape as the
/// rename popup: type, enter commits, esc cancels.
pub(crate) fn render_task_input(frame: &mut Frame, area: Rect, app: &App) {
    let mut input_line = app.tasks.input.clone();
    input_line.push('▎');
    let renaming = app.tasks.renaming.is_some();

    let desired_w = 70u16.min(area.width);
    // Borders (2) plus the "  + " prefix (4) leave this much for the text, so
    // the height estimate matches what the wrapped Paragraph will occupy.
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

    let (title, hint) = if renaming {
        (" Rename task ", " edits the text in place ")
    } else {
        (" New task ", " lands in To-Do · #tag !1–!4 inline ")
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
        Span::styled(
            if renaming { " rename   " } else { " add   " },
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
            Span::styled("  + ", Style::default().fg(Color::Green)),
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

pub(crate) fn render_state_debug(frame: &mut Frame, area: Rect, app: &mut App) {
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

    // Too small to host content plus the bottom-border indicator; bail before
    // `popup_area.height - 1` below can underflow on a 1-row terminal.
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // The Paragraph wraps, so scroll counts wrapped rows. Clamp the stored
    // scroll to the real bottom each frame — the key handler only
    // saturating_adds, so without this `j` runs off into blank space while the
    // N/N indicator pins.
    let total_rows = wrapped_total_rows(&app.state_debug_lines, inner.width);
    let max_scroll = total_rows.saturating_sub(inner.height);
    if app.state_debug_scroll > max_scroll {
        app.state_debug_scroll = max_scroll;
    }

    let scroll_info = format!(
        " {}/{} ",
        (app.state_debug_scroll as usize).min(total_rows.saturating_sub(1) as usize) + 1,
        total_rows
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
    // The Paragraph wraps, so `.scroll` counts wrapped rows, not logical lines.
    // Any line wider than the popup adds rows the logical count misses, which
    // is what left the bottom rows unreachable and the highlight off-target.
    let wrap_w = content_area.width;
    let total_rows = wrapped_total_rows(&lines, wrap_w);

    lv.total_content_lines = total_rows;

    if lv.auto_scroll && total_rows > content_area.height {
        lv.scroll = total_rows.saturating_sub(content_area.height);
    }

    // One-shot: consuming the flag lets manual scrolls stick afterwards.
    // If the highlight didn't resolve, clear the flag anyway so we don't
    // keep retrying on every frame.
    if lv.scroll_to_highlight.is_some() {
        if let Some((start, _end)) = highlight_range {
            let h = content_area.height.max(1);
            // `start` is a logical-line index; translate it to the wrapped-row
            // offset the scroll actually operates in.
            let target_row = wrapped_total_rows(&lines[..start.min(lines.len())], wrap_w);
            let target = target_row.saturating_sub(h / 3);
            let max_scroll = total_rows.saturating_sub(h);
            lv.scroll = target.min(max_scroll);
            lv.scroll_to_highlight = None;
        } else if !lv.messages.is_empty() {
            lv.scroll_to_highlight = None;
        }
    }

    let max_scroll = total_rows.saturating_sub(content_area.height);
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
        (lv.scroll as usize).min(total_rows.saturating_sub(1) as usize) + 1,
        total_rows
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
/// format is defined by `extract_text_content` in `conversation/render.rs` — keep
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

#[cfg(test)]
mod wrapped_rows_tests {
    use super::wrapped_total_rows;
    use ratatui::text::{Line, Span};

    fn rows(s: &str, w: u16) -> usize {
        wrapped_total_rows(&[Line::from(Span::raw(s.to_string()))], w) as usize
    }

    #[test]
    fn empty_and_short_lines_are_one_row() {
        assert_eq!(rows("", 10), 1);
        assert_eq!(rows("hello", 10), 1);
        assert_eq!(rows("hello", 5), 1); // exactly fills one row
    }

    #[test]
    fn breaks_on_word_boundaries() {
        assert_eq!(rows("the quick brown fox", 9), 2);
        assert_eq!(rows("hello world", 5), 2);
    }

    #[test]
    fn hard_splits_a_word_wider_than_the_row() {
        assert_eq!(rows("abcdefgh", 3), 3); // abc/def/gh
    }

    #[test]
    fn zero_width_never_divides_by_zero() {
        assert_eq!(rows("abc", 0), 1); // degenerate area; renderer shows nothing
    }

    // The cases where the previous hand-rolled WordWrapper mirror diverged
    // from the renderer (fuzz-verified against Paragraph rendering). These pin
    // the renderer's actual behavior so a future reimplementation can't
    // silently drift again.
    #[test]
    fn matches_renderer_on_divergent_shapes() {
        // Leading whitespace before an overflowing token: WordWrapper packs
        // the whitespace + word-head onto the first row.
        assert_eq!(rows(" leading", 4), 2);
        // Trailing space at exact row boundary is dropped, not wrapped.
        assert_eq!(rows("aaaa ", 4), 1);
        // Wide chars (CJK, emoji) occupy two columns each.
        assert_eq!(rows("日本語のテキスト", 8), 2);
        assert_eq!(rows("emoji 🚀🚀🚀 line", 8), 3);
    }

    #[test]
    fn spans_concatenate_and_lines_sum() {
        // Two spans concatenate into one logical line for wrap purposes.
        let wide = Line::from(vec![Span::raw("the quick "), Span::raw("brown fox")]);
        assert_eq!(wrapped_total_rows(&[wide], 9), 2);

        let lines = vec![
            Line::from(Span::raw("short")),           // 1 row
            Line::from(Span::raw("the quick brown")), // 2 rows at width 9
        ];
        assert_eq!(wrapped_total_rows(&lines, 9), 3);
    }
}

// Unix-only: `with_temp_home` isolates `$HOME` for `App::new()`'s loads,
// same as the todo-panel suite below.
#[cfg(all(test, unix))]
mod places_picker_tests {
    use crate::app::{App, View};
    use crate::folder_picker::{FolderPicker, Place, PlaceSource};
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    #[test]
    fn renders_filter_match_count_and_surviving_rows() {
        with_temp_home(|| {
            let mut app = App::new();
            let mut picker = FolderPicker::new_places(vec![
                Place::new(
                    Some("cc-hub".into()),
                    PathBuf::from("/g/self/cc-hub"),
                    PlaceSource::Project,
                ),
                Place::new(None, PathBuf::from("/g/self/reddit"), PlaceSource::Recent),
            ]);
            for c in "hub".chars() {
                picker.push_filter(c);
            }
            app.folder_picker = Some(picker);
            app.view = View::FolderPicker;

            let backend = TestBackend::new(90, 26);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_folder_picker(f, f.area(), &app))
                .expect("render");
            let rendered = buffer_to_string(terminal.backend().buffer());

            assert!(rendered.contains("cc-hub"), "name row:\n{}", rendered);
            assert!(rendered.contains("/g/self/cc-hub"), "path:\n{}", rendered);
            assert!(rendered.contains("hub▎"), "filter line:\n{}", rendered);
            assert!(rendered.contains("1/2"), "match count:\n{}", rendered);
            assert!(
                !rendered.contains("reddit"),
                "filtered-out row must not render:\n{}",
                rendered
            );
        });
    }
}

// Unix-only: `with_temp_home` isolates `$HOME` for `App::new()`'s loads,
// same as the todo-panel suite below.
#[cfg(all(test, unix))]
mod task_input_tests {
    use crate::app::{App, View};
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn popup_grows_to_fit_long_input_instead_of_clipping() {
        with_temp_home(|| {
            let mut app = App::new();
            // Longer than the fixed 9-row popup could hold once wrapped (>4
            // rows at the ~64-column text width), so the popup must grow or
            // the footer hints get pushed out the bottom.
            let long = "investigate why the orchestrator retry backoff logic \
                keeps hammering the upstream scheduler after a transient \
                network partition and document every observed failure mode \
                plus the exact sequence of reconnect attempts so we can \
                finally write a deterministic regression test for it";
            app.tasks.input = long.to_string();
            app.view = View::TaskInput;

            let backend = TestBackend::new(90, 26);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_task_input(f, f.area(), &app))
                .expect("render");
            let rendered = buffer_to_string(terminal.backend().buffer());

            // The tail of the text would vanish first if the input clipped.
            for word in long.split_whitespace() {
                assert!(
                    rendered.contains(word),
                    "word {:?} should appear in the popup:\n{}",
                    word,
                    rendered
                );
            }
            // The footer sits below the input, so it's the first casualty of
            // a popup that didn't grow.
            for hint in ["[enter]", "add", "[esc]", "cancel"] {
                assert!(
                    rendered.contains(hint),
                    "footer hint {:?} should stay visible:\n{}",
                    hint,
                    rendered
                );
            }
        });
    }
}

// Unix-only: `with_temp_home` redirects `$HOME` so the todo add doesn't touch
// the real `~/.cc-hub` — the same isolation the todo module's own tests rely on.
#[cfg(all(test, unix))]
mod todo_panel_tests {
    use crate::app::{App, View};
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn long_item_wraps_instead_of_clipping() {
        with_temp_home(|| {
            let mut app = App::new();
            // Longer than the panel's ~38-column text area, so it must wrap to
            // a second row instead of clipping the tail off the right edge.
            let long = "remember to refactor the orchestrator retry backoff logic today";
            app.todo.list.add(long);
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
