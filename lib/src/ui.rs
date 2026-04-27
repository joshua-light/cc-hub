use crate::app::{status_msg_ttl, App, Tab, View, TABS};
use crate::config;
use crate::conversation::{StateExplanation, Verdict};
use crate::metrics::{MetricsAnalysis, ModelStats, SessionSummary, ToolStats};
use crate::models::{short_sid, SessionDetail, SessionInfo, SessionState};
use crate::orchestrator::Artifact;
use crate::reservations::{Phase, Reservation};
use crate::usage::UsageInfo;
use chrono::Duration as ChronoDuration;
use chrono::{DateTime, Local, TimeZone};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::StatefulImage;
use std::path::Path;

fn cell_height() -> u16 {
    config::get().ui.cell_height.max(1)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    app.update_grid_cols(frame.area().width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

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
        View::TmuxPane => render_tmux_pane(frame, frame.area(), app),
        View::FolderPicker => render_folder_picker(frame, frame.area(), app),
        View::GhCreateInput => {
            render_folder_picker(frame, frame.area(), app);
            render_gh_create_input(frame, frame.area(), app);
        }
        View::ProjectsResult => render_projects_result(frame, frame.area(), app),
        View::Backlog => render_backlog(frame, frame.area(), app),
        View::Grid => {}
    }
}

fn render_tab_strip(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let band_bg = Color::Rgb(20, 20, 28);
    let bg = Style::default().bg(band_bg);

    // Paint the full band (top padding + tabs row + bottom padding) so the
    // background colour reads as a continuous header strip.
    frame.render_widget(Paragraph::new("").style(bg), area);

    let mut spans: Vec<Span<'static>> = vec![Span::styled("  ", bg)];
    for (i, tab) in TABS.iter().enumerate() {
        let is_active = *tab == app.current_tab;
        let (fg, bgc, modi) = if is_active {
            (
                Color::Black,
                Color::Rgb(180, 200, 230),
                Modifier::BOLD,
            )
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
        Style::default().fg(Color::Rgb(80, 80, 95)).bg(band_bg),
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

    let popup = centered_fixed(area, 80, 24);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " New session · pick folder ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " enter:descend · bksp:parent · space:pick · .:pick cwd · c/C:gh new · esc:cancel ",
        Style::default().fg(Color::Rgb(110, 110, 130)),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 {
        return;
    }

    let path_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let path_str = picker.current_dir.display().to_string();
    let path_line = Line::from(vec![
        Span::styled(" 󰉋 ", Style::default().fg(Color::Cyan)),
        Span::styled(
            path_str,
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(path_line), path_area);

    let list_h = inner.height - 2;
    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, list_h);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no subdirectories)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_h as usize;
        let start = picker
            .selection
            .saturating_sub(visible.saturating_sub(1));
        for (i, name) in picker.entries.iter().enumerate().skip(start).take(visible) {
            let selected = i == picker.selection;
            let (marker, style) = if selected {
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
            lines.push(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(format!("{}/", name), style),
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
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
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
    let popup = centered_fixed(area, 80, 9);
    frame.render_widget(Clear, popup);

    let project_mode = app.prompt_input_for_project();
    let (title, target_label, title_color) = if project_mode {
        let cwd = app
            .projects_pending_cwd
            .clone()
            .unwrap_or_else(|| "?".into());
        (
            " New project task ",
            format!(" → orchestrator in {} ", cwd),
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
        Style::default().fg(title_color).add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut input_line = app.prompt_buffer.clone();
    input_line.push('▎');

    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                input_line,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "[enter]",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" dispatch   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "[esc]",
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_backlog(frame: &mut Frame, area: Rect, app: &App) {
    let popup = centered_fixed(area, 90, 22);
    frame.render_widget(Clear, popup);

    let project_name = app
        .selected_project()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "no project".to_string());
    let title_text = format!(" Backlog · {} ", project_name);
    let block = popup_block(Span::styled(
        title_text,
        Style::default()
            .fg(Color::Rgb(120, 140, 200))
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " j/k navigate · s/enter start · esc/q close ",
        Style::default().fg(Color::DarkGray),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let tasks = app.backlog_tasks();
    if tasks.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "No backlog tasks for this project.",
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(empty, inner);
        return;
    }

    let max_w = inner.width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(tasks.len() * 3);
    for (i, t) in tasks.iter().enumerate() {
        let selected = i == app.backlog_sel;
        let arrow = if selected { "▌ " } else { "  " };
        let title_text = match t.title.as_deref().filter(|s| !s.is_empty()) {
            Some(name) => name.to_string(),
            None => first_line_preview(&t.prompt, max_w),
        };
        let title_style = if selected {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::styled(arrow, Style::default().fg(Color::Rgb(120, 140, 200))),
            Span::styled(title_text, title_style),
        ]));
        let id_short = crate::orchestrator::short_task_id(&t.task_id);
        let preview = first_line_preview(&t.prompt, max_w.saturating_sub(id_short.len() + 6));
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(id_short, Style::default().fg(Color::DarkGray)),
            Span::styled("  ", Style::default()),
            Span::styled(preview, Style::default().fg(Color::Rgb(110, 110, 130))),
        ]));
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_tmux_pane(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.92);
    frame.render_widget(Clear, popup_area);

    let Some(pane) = app.tmux_pane.as_mut() else {
        return;
    };

    let title = format!(" tmux: {} ", pane.session_name);
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
    // The same view handles two confirmations: closing a session and
    // deleting a project task. Whichever pending struct is set wins; we
    // tweak the title and confirm-action label so the user knows which
    // operation they're agreeing to.
    let (title, display, action_label, action_color) =
        if let Some(pending) = &app.pending_task_delete {
            (
                " Delete task? ",
                pending.display.clone(),
                "delete",
                Color::Red,
            )
        } else if let Some(pending) = &app.pending_close {
            (
                " Close terminal? ",
                pending.display.clone(),
                "close",
                Color::Red,
            )
        } else {
            return;
        };

    let popup = centered_fixed(area, 72, 7);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(action_color))
        .title(Span::styled(
            title,
            Style::default()
                .fg(action_color)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                display,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("[y]", Style::default().fg(action_color).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {}   ", action_label), Style::default().fg(Color::DarkGray)),
            Span::styled("[n/esc]", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
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
    let right_line = app.usage_line.clone();
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
    Some(dt.with_timezone(&Local).format(fmt).to_string().to_lowercase())
}

const GROUP_HEADER_HEIGHT: u16 = 1;
const GROUP_GAP: u16 = 1;

fn render_grid(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.groups.is_empty() {
        let empty =
            Paragraph::new("No sessions found. Start a Claude Code session to see it here.")
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
        let rows = ((group.sessions.len() + cols - 1) / cols) as u16;
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
            let attn = group.sessions.iter().filter(|s| s.needs_attention()).count();

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
    } else if session.needs_attention() {
        Color::Yellow
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
        Some(crate::projects_scan::SessionRole::Orchestrator { task_id, .. }) => {
            Some(format!("★ orch[{}] ", crate::orchestrator::short_task_id(task_id)))
        }
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
            Some(format!("↳ wkr[{}/{}] ", crate::orchestrator::short_task_id(task_id), suffix))
        }
        None => None,
    };
    let prefix = role_prefix.as_deref().unwrap_or("");

    // Border title is the primary skim surface — prepending the Haiku-
    // generated 2-3 word title when available lets users scan what each
    // session is about without having to read the (truncated, often mid-
    // sentence) last user message inside the card body. A `✎` placeholder
    // marks cards with an in-flight Haiku call so the user can tell a
    // pending title from one that's never going to arrive.
    let title = match session.title.as_deref() {
        Some(t) if !t.is_empty() => {
            format!("{}{} {} — {}", prefix, indicator, session.project_name, t)
        }
        _ if session.titling => {
            format!("{}{} {} — ✎ …", prefix, indicator, session.project_name)
        }
        _ => format!("{}{} {}", prefix, indicator, session.project_name),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title,
            Style::default()
                .fg(ind_color)
                .add_modifier(Modifier::BOLD),
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
        Span::styled(
            branch.to_string(),
            Style::default().fg(Color::Cyan),
        ),
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
        Some((format_tool_label(tool, inner_w), state_color(&session.state)))
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
        session.last_user_message.as_ref().or(session.summary.as_ref())
    };
    if let Some(msg) = display_msg {
        let max_w = inner_w.saturating_sub(3); // account for icon prefix
        let chars: Vec<char> = msg.chars().collect();
        if chars.len() <= max_w {
            lines.push(Line::from(vec![
                Span::styled("󰍡 ", Style::default().fg(Color::Rgb(100, 100, 120))),
                Span::styled(
                    msg.clone(),
                    Style::default().fg(Color::Rgb(160, 160, 170)),
                ),
            ]));
        } else {
            let first_line: String = chars[..max_w].iter().collect();
            let remaining: String = chars[max_w..].iter().take(max_w.saturating_sub(3)).collect();
            lines.push(Line::from(vec![
                Span::styled("󰍡 ", Style::default().fg(Color::Rgb(100, 100, 120))),
                Span::styled(
                    first_line,
                    Style::default().fg(Color::Rgb(160, 160, 170)),
                ),
            ]));
            let second = if chars.len() > max_w * 2 - 3 {
                format!("{}...", remaining)
            } else {
                remaining
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    second,
                    Style::default().fg(Color::Rgb(160, 160, 170)),
                ),
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
    let title = format!(
        " {} (PID {}) ",
        session.project_name, session.pid
    );

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
    Url,
    Fallback,
}

/// Body height (excluding the 1-line caption header) for a given card kind.
const CARD_IMAGE_BODY_H: u16 = 10;
const CARD_TEXT_BODY_H: u16 = 12;
const CARD_VIDEO_BODY_H: u16 = 2;
const CARD_URL_BODY_H: u16 = 2;
const CARD_FALLBACK_BODY_H: u16 = 1;

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
        "log" | "build" | "test" | "diff" | "text" | "output" => return CardKind::Text,
        _ => {}
    }
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => CardKind::Image,
        "mp4" | "mov" | "webm" | "mkv" => CardKind::Video,
        "log" | "txt" | "md" | "diff" | "patch" | "json" | "yaml" | "yml" => CardKind::Text,
        _ => CardKind::Fallback,
    }
}

fn card_body_height(kind: CardKind) -> u16 {
    match kind {
        CardKind::Image => CARD_IMAGE_BODY_H,
        CardKind::Text => CARD_TEXT_BODY_H,
        CardKind::Video => CARD_VIDEO_BODY_H,
        CardKind::Url => CARD_URL_BODY_H,
        CardKind::Fallback => CARD_FALLBACK_BODY_H,
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

fn diff_line_style(line: &str) -> Style {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        Style::default().fg(Color::LightGreen)
    } else if line.starts_with('-') {
        Style::default().fg(Color::LightRed)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn evidence_card_header(a: &Artifact, selected: bool) -> Line<'static> {
    let basename = Path::new(&a.path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.path.clone());
    let stripe = if selected { "▌ " } else { "  " };
    let stripe_color = if selected { Color::LightCyan } else { Color::DarkGray };
    let name_style = if selected {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let mut spans = vec![
        Span::styled(stripe, Style::default().fg(stripe_color)),
        Span::styled(a.kind.clone(), Style::default().fg(Color::LightMagenta)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(basename, name_style),
    ];
    if let Some(c) = a.caption.as_deref() {
        if !c.is_empty() {
            spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)),);
            spans.push(Span::styled(c.to_string(), Style::default().fg(Color::Rgb(180, 180, 200))));
        }
    }
    Line::from(spans)
}

fn render_text_card_body(frame: &mut Frame, area: Rect, a: &Artifact, max_bytes: usize) {
    if area.height == 0 {
        return;
    }
    let path = Path::new(&a.path);
    let kind_lower = a.kind.to_ascii_lowercase();
    let ext_lower = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let is_diff = kind_lower == "diff" || ext_lower == "diff" || ext_lower == "patch";
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
        Some((mut content, truncated)) => {
            let body_rows = (area.height as usize).saturating_sub(if truncated > 0 { 1 } else { 0 });
            if content.len() > body_rows {
                content.truncate(body_rows);
            }
            let mut out: Vec<Line<'static>> = content
                .into_iter()
                .map(|s| {
                    let style = if is_diff {
                        diff_line_style(&s)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    Line::from(Span::styled(format!("  {}", s), style))
                })
                .collect();
            if truncated > 0 {
                out.push(Line::from(Span::styled(
                    format!("  … (truncated, {} more line{})", truncated, if truncated == 1 { "" } else { "s" }),
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                )));
            }
            out
        }
    };
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
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
            Style::default().fg(Color::LightBlue).add_modifier(Modifier::UNDERLINED),
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
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
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

/// "Result" popup for the selected Projects task: status + age + truncated
/// prompt at the top, the orchestrator's `summary` next, then per-artifact
/// "evidence cards" inlining the actual content (image, text excerpt, URL,
/// or video hint) so the user sees *why* the task succeeded without clicking
/// out to the filesystem.
fn render_projects_result(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.85);
    frame.render_widget(Clear, popup_area);

    let Some(t) = app.selected_project_task().cloned() else {
        frame.render_widget(popup_block(" Result — no task selected "), popup_area);
        return;
    };

    let (status_label, status_color) = match t.status {
        crate::orchestrator::TaskStatus::Running => ("running", Color::LightYellow),
        crate::orchestrator::TaskStatus::Review => ("review", Color::LightCyan),
        crate::orchestrator::TaskStatus::Done => ("done", Color::LightGreen),
        crate::orchestrator::TaskStatus::Failed => ("failed", Color::LightRed),
        crate::orchestrator::TaskStatus::Backlog => ("backlog", Color::Rgb(120, 140, 200)),
    };
    let title = match t.title.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => format!(
            " Result · {} — {} ",
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
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height == 0 {
        return;
    }

    let now_secs = now_ms() / 1000;
    let age = format_age(now_secs.saturating_sub(t.updated_at as u64));

    // ── Header: status badge + prompt excerpt ──────────────────────────────
    let mut header_lines: Vec<Line<'static>> = Vec::new();
    header_lines.push(Line::from(vec![
        Span::styled(
            format!("[{}]", status_label),
            Style::default().fg(status_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(age, Style::default().fg(Color::Rgb(160, 160, 180))),
        Span::raw("  "),
        Span::styled(
            format!("({} artifact{})", t.artifacts.len(), if t.artifacts.len() == 1 { "" } else { "s" }),
            Style::default().fg(Color::Rgb(150, 130, 200)),
        ),
    ]));
    header_lines.push(Line::raw(""));
    header_lines.push(Line::from(Span::styled(
        "Prompt",
        Style::default().fg(Color::Rgb(120, 160, 220)).add_modifier(Modifier::BOLD),
    )));
    let prompt_lines: Vec<&str> = t.prompt.lines().take(3).collect();
    if prompt_lines.is_empty() {
        header_lines.push(Line::from(Span::styled(
            "  (empty)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for line in &prompt_lines {
            header_lines.push(Line::from(Span::styled(
                format!("  {}", line),
                Style::default().fg(Color::Gray),
            )));
        }
        if t.prompt.lines().count() > 3 {
            header_lines.push(Line::from(Span::styled(
                "  …",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    header_lines.push(Line::raw(""));

    // ── Summary section ────────────────────────────────────────────────────
    let mut summary_lines: Vec<Line<'static>> = Vec::new();
    summary_lines.push(Line::from(Span::styled(
        "Summary",
        Style::default().fg(Color::Rgb(120, 160, 220)).add_modifier(Modifier::BOLD),
    )));
    match t.summary.as_deref() {
        None => summary_lines.push(Line::from(Span::styled(
            "  (no summary yet)",
            Style::default().fg(Color::DarkGray),
        ))),
        Some(s) => {
            for line in s.lines() {
                summary_lines.push(Line::from(Span::styled(
                    format!("  {}", line),
                    Style::default().fg(Color::Gray),
                )));
            }
        }
    }
    summary_lines.push(Line::raw(""));
    summary_lines.push(Line::from(Span::styled(
        "Evidence",
        Style::default().fg(Color::Rgb(120, 160, 220)).add_modifier(Modifier::BOLD),
    )));

    // ── Layout & scrolling ────────────────────────────────────────────────
    let header_h = header_lines.len() as u16;
    let summary_h = summary_lines.len() as u16;
    let body_h = inner.height.saturating_sub(1);
    let body_area = Rect::new(inner.x, inner.y, inner.width, body_h);
    let footer_area = Rect::new(inner.x, inner.y + body_h, inner.width, 1);

    // When the user has hit `e`, the selected card swells to fill most of
    // the visible body area; non-selected cards keep their default heights so
    // the surrounding context (header, summary, neighbours) stays in view.
    let expanded_body_h: u16 = if app.result_artifact_expanded {
        body_h.saturating_sub(6).min(40)
    } else {
        0
    };
    let mut card_meta: Vec<(usize, CardKind, u16)> = Vec::new(); // (idx, kind, body_h)
    for (idx, a) in t.artifacts.iter().enumerate() {
        let kind = classify_artifact(a);
        let default_h = card_body_height(kind);
        let h = if app.result_artifact_expanded && idx == app.result_artifact_sel {
            expanded_body_h.max(default_h)
        } else {
            default_h
        };
        card_meta.push((idx, kind, h));
    }

    let mut canvas_card_tops: Vec<u16> = Vec::with_capacity(card_meta.len());
    let mut next_y = header_h + summary_h;
    for (_, _, body) in &card_meta {
        canvas_card_tops.push(next_y);
        // Card = header(1) + body + spacer(1).
        next_y = next_y.saturating_add(1 + body + 1);
    }
    if t.artifacts.is_empty() {
        next_y = next_y.saturating_add(1); // "(no artifacts)" line
    }
    let total_canvas_h = next_y;

    // Auto-scroll so the selected card stays on-screen.
    if !t.artifacts.is_empty() && body_h > 0 {
        let sel_idx = app.result_artifact_sel.min(t.artifacts.len() - 1);
        let sel_top = canvas_card_tops[sel_idx];
        let sel_h = 1 + card_meta[sel_idx].2 + 1;
        if sel_top < app.result_scroll {
            app.result_scroll = sel_top;
        } else if sel_top + sel_h > app.result_scroll + body_h {
            app.result_scroll = sel_top + sel_h - body_h;
        }
    }
    let max_scroll = total_canvas_h.saturating_sub(body_h);
    if app.result_scroll > max_scroll {
        app.result_scroll = max_scroll;
    }
    let scroll = app.result_scroll;

    // Render header + summary as one paragraph with scroll. Image cards are
    // overlaid on top of placeholder lines we'll write into the lines vec
    // below, so the Paragraph never tries to "draw" the image area itself.
    let mut canvas_lines: Vec<Line<'static>> = Vec::with_capacity(total_canvas_h as usize);
    canvas_lines.extend(header_lines);
    canvas_lines.extend(summary_lines);
    if t.artifacts.is_empty() {
        canvas_lines.push(Line::from(Span::styled(
            "  (no artifacts attached)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (idx, a) in t.artifacts.iter().enumerate() {
            let selected = idx == app.result_artifact_sel;
            canvas_lines.push(evidence_card_header(a, selected));
            // Body placeholder: blank lines so the Paragraph's vertical scroll
            // produces correct y offsets, then card-specific widgets paint on
            // top in the second pass below.
            let body = card_meta[idx].2;
            for _ in 0..body {
                canvas_lines.push(Line::raw(""));
            }
            canvas_lines.push(Line::raw(""));
        }
    }
    let canvas_para = Paragraph::new(canvas_lines).wrap(Wrap { trim: false }).scroll((scroll, 0));
    frame.render_widget(canvas_para, body_area);

    // Per-card widgets, painted on top of the placeholder rows.
    for (idx, a) in t.artifacts.iter().enumerate() {
        let kind = card_meta[idx].1;
        let body = card_meta[idx].2;
        let card_top_canvas = canvas_card_tops[idx];
        let body_top_canvas = card_top_canvas + 1;
        // Card body in screen coordinates after scroll.
        let body_screen_top = body_area.y as i32 + body_top_canvas as i32 - scroll as i32;
        let body_screen_bot = body_screen_top + body as i32;
        let view_top = body_area.y as i32;
        let view_bot = (body_area.y + body_h) as i32;
        // Off-screen entirely → skip.
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
        match kind {
            CardKind::Image => {
                // Kitty/sixel/iterm2 protocols write pixel data tied to a fixed
                // rect; if we let them paint over a partially-clipped rect the
                // terminal leaves residue when the popup scrolls. So we only
                // render the image when the *whole* body is on-screen and the
                // picker actually exists.
                let fully_visible =
                    body_screen_top >= view_top && body_screen_bot <= view_bot;
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
                    let widget = StatefulImage::<ratatui_image::protocol::StatefulProtocol>::default();
                    frame.render_stateful_widget(widget, body_rect, state);
                }
            }
            CardKind::Text => {
                let expanded = app.result_artifact_expanded && idx == app.result_artifact_sel;
                let max_bytes = if expanded { 64 * 1024 } else { 8 * 1024 };
                render_text_card_body(frame, body_rect, a, max_bytes);
            }
            CardKind::Video => render_video_card_body(frame, body_rect, a),
            CardKind::Url => render_url_card_body(frame, body_rect, a),
            CardKind::Fallback => render_fallback_card_body(frame, body_rect, a),
        }
    }

    let hint = " esc/r:close   j/k:artifact   e:expand   PgUp/PgDn:scroll   c:copy path   o:xdg-open ";
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
        " Why is {} (PID {}) in state \"{}\"? ",
        info.project_name, info.pid, exp.final_state
    );
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(state_color(&exp.final_state))
            .add_modifier(Modifier::BOLD),
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

pub fn build_state_debug_content(
    info: &SessionInfo,
    exp: &StateExplanation,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let final_color = state_color(&exp.final_state);

    lines.push(Line::from(vec![
        Span::styled("Final state: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", exp.final_state),
            Style::default().fg(final_color).add_modifier(Modifier::BOLD),
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
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
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
            Span::styled(
                format!("{}  ", ts),
                Style::default().fg(Color::DarkGray),
            ),
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
        Span::styled("  ", Style::default().fg(Color::Rgb(100, 100, 120))),
        Span::styled("Branch:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            session.git_branch.clone().unwrap_or_default(),
            Style::default().fg(Color::Cyan),
        ),
    ];
    if let Some(model) = &session.model {
        meta_spans.push(Span::styled("   󰧑 Model: ", Style::default().fg(Color::DarkGray)));
        meta_spans.push(Span::styled(
            short_model(model).to_string(),
            Style::default().fg(Color::White),
        ));
    }
    if let Some(version) = &session.version {
        meta_spans.push(Span::styled("   v", Style::default().fg(Color::DarkGray)));
        meta_spans.push(Span::styled(version.clone(), Style::default().fg(Color::DarkGray)));
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
                Style::default()
                    .fg(role_color)
                    .add_modifier(Modifier::BOLD),
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
                header_spans.push(Span::styled(
                    "   tools",
                    Style::default().fg(Color::Cyan),
                ));
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
        format!("{}s ago", elapsed)
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
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ));
    } else {
        let keybinds: &str = match app.view {
            View::Grid => match app.current_tab {
                Tab::Projects => "tab:next  j/k:project  J/K:task  enter:focus orch  f:agent terminal  n:new task  N:register project  b:backlog  r:result  x:delete  q:quit",
                Tab::Sessions => "tab:next  h/j/k/l:nav  n:new  N:new in…  i:info  D:why?  enter/f:focus/resume  o:shell  x:close  H:inactive  W:workers  q:quit",
                Tab::Metrics => "tab:next  j/k:select  enter:view transcript  r:refresh  q:quit",
            },
            View::Popup => "j/k:scroll  esc:close  q:close",
            View::LiveTail => "j/k:scroll  G:bottom  esc:close",
            View::ConfirmClose => "y:close  n/esc:cancel",
            View::StateDebug => "j/k:scroll  esc:close  q:close",
            View::PromptInput => "type prompt  enter:dispatch  esc:cancel",
            View::TmuxPane => "forwarding keys to tmux · F1: detach & close",
            View::FolderPicker => "j/k:move  enter:descend  bksp:parent  space:pick  .:pick cwd  c/C:gh new (pub/priv)  esc:cancel",
            View::GhCreateInput => "type name  tab:toggle public/private  enter:create  esc:cancel",
            View::ProjectsResult => "j/k:artifact  e:expand  PgUp/PgDn:scroll  c:copy path  o:xdg-open  esc/r:close",
            View::Backlog => "j/k:select  s/enter:start  esc/q:close",
        };
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
        let age = app
            .pending_dispatch_age()
            .map(|d| d.as_secs())
            .unwrap_or(0);
        spans.push(Span::styled(
            format!(" ↻ dispatch waiting [{}] {}s ", target, age),
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
        SessionState::Processing => ("󰑮", Color::Green),
        SessionState::WaitingForInput => ("󰂞", Color::Yellow),
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
    let leaf = tool.rsplit("__").next().unwrap_or(tool);
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
    let hint_short = truncate_chars(hint, budget.max(6));
    format!("󰖷 {}: {}", name, hint_short)
}

fn truncate_chars(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    let mut out: String = chars.into_iter().take(max - 1).collect();
    out.push('…');
    out
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
                "Press N to pick a folder and start a task.",
                Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD),
            ),
        ]))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title(" Projects "));
        frame.render_widget(empty, area);
        return;
    }

    let contentions = app
        .selected_project()
        .map(|p| snap.contentions_for(&p.id))
        .unwrap_or_default();

    // Top: project chip strip (1 line) + spacer (1 line). Optional 1-line
    // contention strip slots in below the chips so users can spot file-level
    // conflicts without opening any task.
    if contentions.is_empty() {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area);
        render_project_chip_strip(frame, rows[0], app);
        render_kanban_board(frame, rows[1], app);
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
        render_project_chip_strip(frame, rows[0], app);
        render_contention_strip(frame, rows[1], &contentions);
        render_kanban_board(frame, rows[2], app);
    }
}

fn render_contention_strip(
    frame: &mut Frame,
    area: Rect,
    contentions: &[crate::projects_scan::Contention],
) {
    if area.height == 0 || area.width < 8 || contentions.is_empty() {
        return;
    }
    let band = Style::default().bg(Color::Rgb(40, 25, 25));
    frame.render_widget(Paragraph::new("").style(band), area);
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(
        format!("  ⚠ {} contention(s): ", contentions.len()),
        Style::default()
            .fg(Color::Rgb(240, 190, 110))
            .bg(Color::Rgb(40, 25, 25))
            .add_modifier(Modifier::BOLD),
    ));
    let max_inline = 2;
    for (i, c) in contentions.iter().take(max_inline).enumerate() {
        if i > 0 {
            spans.push(Span::styled("   ", band));
        }
        let holder = crate::orchestrator::short_task_id(&c.holder_task);
        let waiter = crate::orchestrator::short_task_id(&c.waiter_task);
        let path = c.overlapping_paths.first().cloned().unwrap_or_default();
        spans.push(Span::styled(
            format!("{} active on {} ← {} (intended)", holder, path, waiter),
            Style::default()
                .fg(Color::Rgb(220, 200, 200))
                .bg(Color::Rgb(40, 25, 25)),
        ));
    }
    let extra = contentions.len().saturating_sub(max_inline);
    if extra > 0 {
        spans.push(Span::styled(
            format!(" … +{} more", extra),
            Style::default()
                .fg(Color::Rgb(180, 160, 160))
                .bg(Color::Rgb(40, 25, 25)),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(band), area);
}

/// Horizontal strip of project "chips". Selected chip is bold/inverse with
/// per-column counts (R·D·F). Cycled with `[` / `]`.
fn render_project_chip_strip(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    // Background band so the strip reads as a header even on dark themes.
    let band = Style::default().bg(Color::Rgb(20, 20, 28));
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
        Style::default().fg(Color::Rgb(150, 150, 170)).bg(Color::Rgb(20, 20, 28)),
    ));
    for (idx, p) in snap.projects.iter().enumerate() {
        let tasks = snap.tasks.get(&p.id);
        let mut planning = 0usize;
        let mut running = 0usize;
        let mut review = 0usize;
        let mut done = 0usize;
        let mut failed = 0usize;
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
                    crate::orchestrator::TaskStatus::Done => done += 1,
                    crate::orchestrator::TaskStatus::Failed => failed += 1,
                    // Backlog tasks haven't started — they don't appear
                    // on the kanban (which starts at Planning) nor in the
                    // chip-strip running totals. Counted-but-not-shown
                    // would mislead; the project chip already conveys
                    // "no active work" via colour when planning+running=0.
                    crate::orchestrator::TaskStatus::Backlog => {}
                }
            }
        }
        let selected = idx == app.projects_sel;
        let label = format!(" {} ", p.name);
        // Compact P·R·Rv·D·F counts. Review squeezed in with `Rv` so the
        // chip fits typical project names without wrapping.
        let counts = format!(" {}·{}·{}·{}·{} ", planning, running, review, done, failed);
        let (chip_fg, chip_bg) = if selected {
            (Color::Black, Color::Rgb(190, 200, 230))
        } else if planning + running > 0 {
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
        } else if planning + running + review > 0 {
            Color::Rgb(160, 220, 180)
        } else {
            Color::Rgb(120, 120, 140)
        };
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(chip_fg)
                .bg(chip_bg)
                .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() }),
        ));
        spans.push(Span::styled(counts, Style::default().fg(counts_fg).bg(counts_bg)));
        spans.push(Span::styled(" ", band));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(band), chip_row);

    if let (Some(row), Some(p)) = (path_row, app.selected_project()) {
        let root = p.root.display().to_string();
        let max = (row.width as usize).saturating_sub(4);
        let root = if root.chars().count() > max {
            let cut = root.chars().count().saturating_sub(max - 1);
            format!("    …{}", root.chars().skip(cut).collect::<String>())
        } else {
            format!("    {}", root)
        };
        let line = Line::from(Span::styled(
            root,
            Style::default()
                .fg(Color::Rgb(110, 110, 130))
                .bg(Color::Rgb(20, 20, 28)),
        ));
        frame.render_widget(Paragraph::new(line).style(band), row);
    }
}

fn render_kanban_board(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 3 || area.width < 30 {
        return;
    }
    // Five columns: Planning · Running · Review · Done · Failed.
    // Running takes the most space (it's where active rich cards live);
    // Done is medium; Planning, Review, and Failed are sidebars — they
    // hold transient or low-volume tasks. Ratio totals to 9.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 9), // Planning
            Constraint::Ratio(3, 9), // Running
            Constraint::Ratio(2, 9), // Review
            Constraint::Ratio(2, 9), // Done
            Constraint::Ratio(1, 9), // Failed
        ])
        .split(area);

    let sessions_by_tmux = app.sessions_by_tmux();
    let now_secs = now_ms() / 1000;

    for col_idx in 0..5 {
        render_kanban_column(
            frame,
            cols[col_idx],
            app,
            col_idx,
            &sessions_by_tmux,
            now_secs,
        );
    }
}

fn kanban_column_meta(col: usize) -> (&'static str, &'static str, Color) {
    // (label, status icon, accent color). Indices match `kanban_column_tasks`.
    match col {
        0 => ("Planning", "󰟶", Color::Rgb(170, 140, 210)),
        1 => ("Running", "󰑮", Color::LightYellow),
        2 => ("Review", "󱋲", Color::LightCyan),
        3 => ("Done", "󰸞", Color::LightGreen),
        _ => ("Failed", "󰅚", Color::LightRed),
    }
}

fn render_kanban_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    col_idx: usize,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
    now_secs: u64,
) {
    let (label, icon, accent) = kanban_column_meta(col_idx);
    let tasks = app.kanban_column_tasks(col_idx);
    let count = tasks.len();
    let col_focused = app.projects_col == col_idx;

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
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{} ", icon), Style::default().fg(accent)),
        Span::styled(label.to_string(), title_style),
        Span::styled(
            format!(" ({}) ", count),
            Style::default().fg(Color::Rgb(140, 140, 165)),
        ),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if tasks.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "— empty —",
            Style::default().fg(Color::Rgb(70, 70, 90)),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(hint, inner);
        return;
    }

    // Planning + Running show tall rich cards (orchestrator is alive,
    // there's live state to display); Review/Done/Failed get compact
    // 3-line cards since they're terminal states from the UI's POV.
    let card_height: u16 = if col_idx <= 1 { 8 } else { 4 };
    let gap: u16 = 1;
    let max_cards = ((inner.height as u32 + gap as u32) / (card_height as u32 + gap as u32)) as usize;

    // Anchor the cursor: keep the selected card visible by scrolling.
    let sel = if col_focused { app.projects_task_sel.min(count - 1) } else { 0 };
    let scroll_top = if max_cards == 0 || sel < max_cards {
        0
    } else {
        sel + 1 - max_cards
    };

    let mut y = inner.y;
    for (rel, t) in tasks.iter().enumerate().skip(scroll_top).take(max_cards) {
        let card_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: card_height,
        };
        let selected = col_focused && rel == sel;
        let reservation = app
            .projects
            .reservation_for_task(&t.project_id, &t.task_id);
        if col_idx <= 1 {
            render_task_card_active(
                frame,
                card_area,
                t,
                selected,
                col_idx,
                sessions_by_tmux,
                now_secs,
                reservation,
            );
        } else {
            render_task_card_collapsed(frame, card_area, t, selected, col_idx, now_secs, reservation);
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
    };

    let mut tool_priority = 0u8; // prefer Processing > WaitingForInput tools
    for s in std::iter::once(orch).chain(workers.iter().copied()).flatten() {
        sum.total += 1;
        match s.state {
            SessionState::Processing => {
                sum.processing += 1;
                sum.alive += 1;
            }
            SessionState::WaitingForInput => {
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
            let cap = context_window_size(s.model.as_deref().unwrap_or("")).max(1) as u64;
            let pct = ((c.saturating_mul(100)) / cap).min(100) as u8;
            if pct > sum.max_ctx_pct {
                sum.max_ctx_pct = pct;
            }
        }
        let pri = match s.state {
            SessionState::Processing => 3,
            SessionState::WaitingForInput => 2,
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
            spans.push(Span::styled((*glyph).to_string(), Style::default().fg(*color)));
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
        w.worktree
            .as_deref()
            .is_some_and(|wn| m.worktree == wn)
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
            spans.push(Span::styled("▱", Style::default().fg(Color::Rgb(90, 90, 110))));
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
        let pad: String = std::iter::repeat('░').take(width - drawn).collect();
        out.push(Span::styled(pad, Style::default().fg(Color::Rgb(50, 50, 65))));
    }
    out
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
    now_secs: u64,
    reservation: Option<&Reservation>,
) {
    let sum = collect_agent_summary(t, sessions_by_tmux);

    // Planning vs Running share the rich layout but differ in accent +
    // title icon so the column matches the card visually.
    let (accent, title_icon) = if col_idx == 0 {
        (Color::Rgb(170, 140, 210), "󰟶")
    } else {
        (Color::LightYellow, "󰑮")
    };
    let (border_type, border_color) = if selected {
        (BorderType::Double, Color::White)
    } else if sum.waiting > 0 {
        (BorderType::Rounded, Color::Rgb(220, 200, 120))
    } else {
        (BorderType::Rounded, Color::Rgb(80, 90, 110))
    };

    let title_id = crate::orchestrator::short_task_id(&t.task_id);
    // Reserve space for the reservation badge on the title row, so the
    // prompt preview shrinks instead of pushing the badge off the right
    // border. Badge is 0 chars when there's no reservation.
    let badge_spans = reservation_badge_spans(reservation, area.width.saturating_sub(14) as usize);
    let badge_w: usize = badge_spans.iter().map(|s| s.content.chars().count()).sum();
    let prompt_max = (area.width as usize).saturating_sub(14 + badge_w);
    let prompt_preview = first_line_preview(&t.prompt, prompt_max);
    let mut title_spans = vec![
        Span::styled(format!(" {} ", title_icon), Style::default().fg(accent)),
        Span::styled(
            format!("[{}] ", title_id),
            Style::default().fg(Color::Rgb(150, 170, 200)),
        ),
        Span::styled(
            prompt_preview,
            Style::default()
                .fg(if selected { Color::White } else { Color::Gray })
                .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() }),
        ),
    ];
    if !badge_spans.is_empty() {
        title_spans.push(Span::raw(" "));
        title_spans.extend(badge_spans);
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

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(inner.height as usize);

    // Row 1: note (orchestrator status) or first line of summary fallback.
    let note_text = t
        .note
        .as_deref()
        .map(|n| first_line_preview(n, inner.width.saturating_sub(4) as usize))
        .unwrap_or_else(|| {
            let s = t.summary.as_deref().unwrap_or("");
            first_line_preview(s, inner.width.saturating_sub(4) as usize)
        });
    if !note_text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("󰍡 ", Style::default().fg(Color::Rgb(150, 170, 200))),
            Span::styled(note_text, Style::default().fg(Color::Rgb(190, 190, 210))),
        ]));
    } else {
        lines.push(Line::from(Span::raw("")));
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

    // Row 3: age · artifacts · ctx bar (right-aligned-ish).
    let age = format_age(now_secs.saturating_sub(t.updated_at as u64));
    let arts = t.artifacts.len();
    let mut row3: Vec<Span<'static>> = vec![
        Span::styled("󰔟 ", Style::default().fg(Color::Rgb(150, 150, 170))),
        Span::styled(age, Style::default().fg(Color::Rgb(180, 180, 200))),
    ];
    if arts > 0 {
        row3.push(Span::styled(
            format!("   󰉂 {}", arts),
            Style::default().fg(Color::Rgb(180, 160, 220)),
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
        row3.push(Span::styled(
            ctx_label,
            Style::default().fg(ctx_color(pct)),
        ));
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
            Some(h) => format!("󰖷 {}: {}", name, truncate_chars(h, max.saturating_sub(name.len() + 4))),
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

/// Compact 3-line card for Done/Failed tasks. Dim border, single-line
/// prompt, footer with age + summary preview + artifact/merge counts.
/// `col_idx` is 1 (Done) or 2 (Failed) — controls accent color.
fn render_task_card_collapsed(
    frame: &mut Frame,
    area: Rect,
    t: &crate::orchestrator::TaskState,
    selected: bool,
    col_idx: usize,
    now_secs: u64,
    reservation: Option<&Reservation>,
) {
    // Review (2) gets a cyan accent, Done (3) green, Failed (4) red. The
    // dim-text color is used for the prompt preview when not selected.
    let (accent, dim_text, icon) = match col_idx {
        2 => (
            Color::LightCyan,
            Color::Rgb(140, 175, 185),
            "󱋲",
        ),
        4 => (
            Color::LightRed,
            Color::Rgb(180, 130, 130),
            "󰅚",
        ),
        _ => (
            Color::LightGreen,
            Color::Rgb(140, 160, 145),
            "󰸞",
        ),
    };
    // Review cards: brighter border so they stand out — they need user
    // attention. Done/Failed cards stay dim.
    let (border_type, border_color) = if selected {
        (BorderType::Double, Color::White)
    } else if col_idx == 2 {
        (BorderType::Rounded, Color::Rgb(110, 170, 180))
    } else {
        (BorderType::Rounded, Color::Rgb(55, 60, 70))
    };

    let title_id = crate::orchestrator::short_task_id(&t.task_id);
    let badge_spans = reservation_badge_spans(reservation, area.width.saturating_sub(14) as usize);
    let badge_w: usize = badge_spans.iter().map(|s| s.content.chars().count()).sum();
    let prompt_max = (area.width as usize).saturating_sub(14 + badge_w);
    let prompt_preview = first_line_preview(&t.prompt, prompt_max);
    let mut title_spans = vec![
        Span::styled(format!(" {} ", icon), Style::default().fg(accent)),
        Span::styled(
            format!("[{}] ", title_id),
            Style::default().fg(Color::Rgb(120, 130, 150)),
        ),
        Span::styled(
            prompt_preview,
            Style::default()
                .fg(if selected { Color::White } else { dim_text })
                .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() }),
        ),
    ];
    if !badge_spans.is_empty() {
        title_spans.push(Span::raw(" "));
        title_spans.extend(badge_spans);
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

    let summary_text = t
        .summary
        .as_deref()
        .or(t.note.as_deref())
        .map(|s| first_line_preview(s, inner.width.saturating_sub(4) as usize))
        .unwrap_or_default();
    let mut lines: Vec<Line<'static>> = Vec::new();
    if !summary_text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("󰍡 ", Style::default().fg(Color::Rgb(110, 120, 135))),
            Span::styled(summary_text, Style::default().fg(Color::Rgb(160, 165, 175))),
        ]));
    }

    let age = format_age(now_secs.saturating_sub(t.updated_at as u64));
    let arts = t.artifacts.len();
    let merged = t.workers.iter().filter(|w| worker_was_merged(w, t)).count();
    let total_w = t.workers.len();
    let mut footer: Vec<Span<'static>> = vec![
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
    lines.push(Line::from(footer));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// Compact badge for the card title row showing a live reservation. Format:
/// `🔒 active <path1> <path2> +N` for `Active`, `⏳ intended …` for
/// `Intended`. Returns an empty Vec when there's nothing to show or the
/// caller has no width budget. Total span width is capped at `max_w` chars.
fn reservation_badge_spans(reservation: Option<&Reservation>, max_w: usize) -> Vec<Span<'static>> {
    let Some(r) = reservation else {
        return Vec::new();
    };
    if max_w < 8 {
        return Vec::new();
    }
    let (glyph, label, color) = match r.phase {
        Phase::Active => ("󰌾", "active", Color::Rgb(240, 190, 90)),
        Phase::Intended => ("󰔟", "intended", Color::Rgb(120, 180, 200)),
    };
    // Reserve space for the glyph + space + label + leading space.
    let prefix_w = glyph.chars().count() + 1 + label.len() + 1;
    if max_w <= prefix_w {
        return vec![Span::styled(
            format!("{} {}", glyph, label),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )];
    }
    let mut budget = max_w.saturating_sub(prefix_w);
    let mut shown_paths: Vec<String> = Vec::new();
    let mut shown = 0usize;
    for p in r.paths.iter().take(2) {
        // 1 char for the leading space.
        let need = p.chars().count() + 1;
        if need > budget {
            break;
        }
        budget -= need;
        shown_paths.push(p.clone());
        shown += 1;
    }
    let remaining = r.paths.len().saturating_sub(shown);
    let mut text = format!("{} {}", glyph, label);
    for p in &shown_paths {
        text.push(' ');
        text.push_str(p);
    }
    if remaining > 0 {
        let suffix = format!(" +{}", remaining);
        if suffix.len() <= budget {
            text.push_str(&suffix);
        }
    }
    vec![Span::styled(
        text,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )]
}

fn first_line_preview(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or("");
    if first.chars().count() <= max {
        first.to_string()
    } else {
        let mut out = String::new();
        for ch in first.chars().take(max - 1) {
            out.push(ch);
        }
        out.push('…');
        out
    }
}

fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

fn render_metrics_body(frame: &mut Frame, area: Rect, app: &App) {
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
                        " Scanning ~/.claude/projects … {} / {} sessions ({}%)",
                        scanned, total, pct
                    )
                }
                _ => " Scanning ~/.claude/projects …".to_string(),
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

    // Auto-scroll so the selected session row sits inside the body. Falls
    // back to whatever raw scroll the user has when no selection is active.
    let body_height = area.height.saturating_sub(1);
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
    };

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
        ("input        ", m.total_tokens.input, Color::Rgb(120, 200, 240)),
        ("output       ", m.total_tokens.output, Color::Rgb(240, 180, 120)),
        ("cache read   ", m.total_tokens.cache_read, Color::Rgb(160, 220, 160)),
        ("cache create ", m.total_tokens.cache_creation, Color::Rgb(220, 160, 200)),
    ];
    let max_tokens = breakdown.iter().map(|(_, t, _)| *t).max().unwrap_or(0).max(1);
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
            Span::styled(format!("  {:<22}", truncate_str(short, 22)), label),
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
    let max_proj = m.top_projects.first().map(|(_, s)| s.cost).unwrap_or(0.0).max(0.01);
    for (name, s) in &m.top_projects {
        let bar_w = ((s.cost / max_proj) * 24.0).round() as usize;
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<26}", truncate_str(name, 26)), label),
            Span::styled("━".repeat(bar_w), Style::default().fg(Color::Rgb(120, 180, 220))),
            Span::raw(" "),
            Span::styled(fmt_cost(s.cost), val),
            Span::styled(format!("  {} sess", s.sessions), dim),
            Span::styled(format!("  {} msgs", s.messages), dim),
        ]));
    }
    lines.push(Line::raw(""));

    let styles = MetricsStyles { dim, label, val };
    render_bar_chart_section(&mut lines, "Tool usage", "tool calls", "tools", &m.by_tool, &styles);
    render_bar_chart_section(&mut lines, "Shell commands", "shell commands", "commands", &m.by_shell, &styles);
    render_bar_chart_section(&mut lines, "MCP servers", "MCP calls", "servers", &m.by_mcp, &styles);

    lines.push(section_header("Interruptions (Esc'd mid-tool-call)"));
    let i = &m.interruptions;
    if i.total_interrupted_turns == 0 {
        lines.push(Line::from(Span::styled("  (none detected)", dim)));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  Wasted ", label),
            Span::styled(fmt_cost(i.total_wasted_cost), val.fg(Color::Rgb(220, 140, 140))),
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
                Span::styled(format!("{:>8}", fmt_cost(entry.wasted_cost)), val.fg(Color::Rgb(220, 140, 140))),
                Span::styled(format!("  {:>3} orphan", entry.orphan_count), dim),
                Span::raw("  "),
                Span::styled(
                    format!("{:<18}", truncate_str(&entry.last_tool_name, 18)),
                    Style::default().fg(Color::Rgb(180, 180, 200)),
                ),
                Span::styled(
                    format!("{:<24}", truncate_str(&entry.project, 24)),
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
                Span::styled(format!("  {:>8}", fmt_cost(f.total_cost)), val.fg(Color::Green)),
                Span::styled(
                    format!("  @ turn {}/{}", f.peak_turn_index, f.assistant_turns),
                    dim,
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<24}", truncate_str(&f.project, 24)),
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
        Span::styled(fmt_cost(g.anomalous_cost), val.fg(Color::Rgb(220, 180, 130))),
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
                Span::styled(format!("{:>5.1}x", f.score), val.fg(Color::Rgb(220, 180, 130))),
                Span::styled(format!("  {:>8}", fmt_cost(f.total_cost)), val.fg(Color::Green)),
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
                    format!("{:<24}", truncate_str(&f.project, 24)),
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
                Span::styled(format!("  {:<22}", truncate_str(name, 22)), label),
                Span::styled("━".repeat(bar_w), Style::default().fg(tool_color(name))),
                Span::raw(" "),
                Span::styled(format!("{:>6} calls", s.count), val),
                Span::styled(format!("  {:>4.1}%", pct), dim),
                Span::styled(format!("  {} sess", s.sessions), dim),
            ]));
        }
        if rows.len() > TOOLS_DISPLAY_LIMIT {
            lines.push(Line::from(Span::styled(
                format!("  … {} more {}", rows.len() - TOOLS_DISPLAY_LIMIT, overflow_noun),
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
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
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
        Span::styled(format!("{:<22}", truncate_str(model, 22)), dim),
        Span::raw(" "),
        Span::styled(
            format!("{:<24}", truncate_str(&s.project, 24)),
            Style::default().fg(Color::Rgb(180, 180, 200)),
        ),
    ])
}

fn truncate_str(s: &str, w: usize) -> String {
    if s.is_ascii() && s.len() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    if s.chars().count() <= w {
        return s.to_string();
    }
    if w == 1 {
        return "…".to_string();
    }
    let mut out = String::with_capacity(w * 4);
    for c in s.chars().take(w - 1) {
        out.push(c);
    }
    out.push('…');
    out
}

fn section_header(title: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "▎ ",
            Style::default().fg(Color::Rgb(120, 140, 180)),
        ),
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
mod result_popup_tests {
    use crate::app::App;
    use crate::orchestrator::{Artifact, Project, TaskState, TaskStatus};
    use crate::projects_scan::ProjectsSnapshot;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn buffer_to_string(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
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
        };
        let mut state = TaskState::new(project.id.clone(), project.root.clone(), "test prompt body".into());
        state.status = TaskStatus::Done;
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

        let mut app = App::new();
        let mut tasks = HashMap::new();
        tasks.insert(project.id.clone(), vec![state]);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks,
            titling: std::collections::HashSet::new(),
            reservations: HashMap::new(),
        };
        app.update_projects(snap);
        assert!(app.enter_projects_result(), "popup should open");

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");

        let buf = terminal.backend().buffer().clone();
        let dump = buffer_to_string(&buf);

        std::fs::write("/tmp/cc-hub-popup-snapshot.txt", &dump).expect("snapshot write");

        assert!(dump.contains("Result"), "should render Result title");
        assert!(
            dump.contains("WHY this works"),
            "summary text should be inlined\n{}",
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
    }

    #[test]
    fn projects_body_renders_reservation_badge_and_contention_panel() {
        use crate::orchestrator::{Worker, short_task_id};
        use crate::reservations::{Phase, Reservation};

        let now = crate::orchestrator::now_unix_secs();
        let project = Project {
            id: "p-resv".into(),
            name: "resv".into(),
            root: PathBuf::from("/tmp/resv"),
            created_at: now,
        };

        // Two Running tasks, both with at least one worker so they land in
        // the Running kanban column (which uses the active card renderer
        // and thus exercises the badge plumbing).
        let worker = Worker {
            tmux_name: "cc-hub-w-1".into(),
            cwd: project.root.clone(),
            worktree: Some(project.root.to_string_lossy().into_owned()),
            readonly: false,
            spawned_at: now,
        };
        let mut task_a = TaskState::new(project.id.clone(), project.root.clone(), "task A prompt".into());
        task_a.task_id = "t-aaaaaa111111".into();
        task_a.status = TaskStatus::Running;
        task_a.workers = vec![worker.clone()];
        let mut task_b = TaskState::new(project.id.clone(), project.root.clone(), "task B prompt".into());
        task_b.task_id = "t-bbbbbb222222".into();
        task_b.status = TaskStatus::Running;
        task_b.workers = vec![worker];

        let resv_active = Reservation {
            task_id: task_a.task_id.clone(),
            worker_id: Some("cc-hub-w-1".into()),
            phase: Phase::Active,
            paths: vec!["lib/src/foo.rs".into()],
            owner_session: "sess-a".into(),
            created_at: now,
            last_heartbeat: now,
        };
        let resv_intended = Reservation {
            task_id: task_b.task_id.clone(),
            worker_id: None,
            phase: Phase::Intended,
            paths: vec!["lib/src/foo.rs".into()],
            owner_session: "sess-b".into(),
            created_at: now,
            last_heartbeat: now,
        };

        let mut tasks = HashMap::new();
        tasks.insert(project.id.clone(), vec![task_a.clone(), task_b.clone()]);
        let mut reservations = HashMap::new();
        reservations.insert(project.id.clone(), vec![resv_active, resv_intended]);
        let snap = ProjectsSnapshot {
            projects: vec![project.clone()],
            tasks,
            titling: std::collections::HashSet::new(),
            reservations,
        };

        let mut app = App::new();
        app.update_projects(snap);
        app.set_tab(crate::app::Tab::Projects);

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_projects_body(f, f.area(), &app))
            .expect("render");

        let buf = terminal.backend().buffer().clone();
        let dump = buffer_to_string(&buf);

        assert!(
            dump.contains("contention"),
            "contention strip should appear\n{}",
            dump
        );
        let short_a = short_task_id(&task_a.task_id);
        assert!(
            dump.contains(&short_a),
            "contention strip should reference holder task id {}\n{}",
            short_a,
            dump
        );
        assert!(
            dump.contains("active") && dump.contains("lib/src/foo.rs"),
            "active reservation badge should render with its path\n{}",
            dump
        );
    }
}


