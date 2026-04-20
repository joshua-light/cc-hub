use crate::app::{App, Tab, View, STATUS_MSG_TTL, TABS};
use crate::conversation::{StateExplanation, Verdict};
use crate::metrics::{MetricsAnalysis, ModelStats, SessionSummary};
use crate::models::{short_sid, SessionDetail, SessionInfo, SessionState};
use crate::usage::UsageInfo;
use chrono::Duration as ChronoDuration;
use chrono::{DateTime, Local, TimeZone};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

const CELL_HEIGHT: u16 = 8;

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

    let target = app.dispatch_target();
    let target_label = target
        .map(|(pid, name, tmux)| format!(" → {} (PID {}) [{}] ", name, pid, tmux))
        .unwrap_or_else(|| " → no idle agent — will spawn a new one ".to_string());
    let title_color = if target.is_some() {
        Color::Green
    } else {
        Color::Yellow
    };

    let block = popup_block(Span::styled(
        " Dispatch prompt ",
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
    let Some(pending) = &app.pending_close else {
        return;
    };

    let popup = centered_fixed(area, 64, 7);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Red))
        .title(Span::styled(
            " Close terminal? ",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                pending.display.clone(),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("[y]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled(" close   ", Style::default().fg(Color::DarkGray)),
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
        y_acc = y_acc.saturating_add(GROUP_HEADER_HEIGHT + rows * CELL_HEIGHT + GROUP_GAP);
    }

    // Auto-scroll to keep selected card visible (prefer showing group header too)
    {
        let g_offset = group_offsets[app.sel_group];
        let card_row = (app.sel_in_group / cols) as u16;
        let card_y = g_offset + GROUP_HEADER_HEIGHT + card_row * CELL_HEIGHT;
        let card_bottom = card_y + CELL_HEIGHT;

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

            let card_cy = g_y + GROUP_HEADER_HEIGHT + row * CELL_HEIGHT;
            let card_sy = card_cy as i32 - scroll as i32;

            // Only render if fully visible within the area
            if card_sy < 0 || card_sy + CELL_HEIGHT as i32 > area.height as i32 {
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
            let cell_area = Rect::new(x, cy, w, CELL_HEIGHT);
            render_card(frame, cell_area, session, is_selected, now);
        }
    }
}

fn render_card(frame: &mut Frame, area: Rect, session: &SessionInfo, selected: bool, now: u64) {
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
    } else {
        BorderType::Rounded
    };

    let title = format!("{} {}", indicator, session.project_name);
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

    // Last-activity elapsed time
    let elapsed_str = session.last_activity.map(|ts| format!("󰔟 {}", format_elapsed(now, ts)));

    let mut state_spans: Vec<Span> = Vec::new();
    if let Some(elapsed) = &elapsed_str {
        state_spans.push(Span::styled(elapsed.clone(), Style::default().fg(Color::DarkGray)));
    }

    lines.push(Line::from(state_spans));

    lines.push(Line::raw(""));

    let display_msg = session.last_user_message.as_ref().or(session.summary.as_ref());
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

    let (title, status_color) = if lv.auto_scroll {
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

    let lines = build_live_tail_content(&lv.messages);
    let total_lines = lines.len() as u16;

    lv.total_content_lines = total_lines;

    if lv.auto_scroll && total_lines > content_area.height {
        lv.scroll = total_lines.saturating_sub(content_area.height);
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

fn build_live_tail_content(messages: &[crate::models::ConversationMessage]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    if messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "Waiting for messages…",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }

    let separate = |lines: &mut Vec<Line<'static>>| {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
    };

    for msg in messages {
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
            for part in parse_preview(&msg.content_preview) {
                separate(&mut lines);
                match part {
                    PreviewPart::Thinking => render_thinking(&mut lines),
                    PreviewPart::Tool(name) => render_tool_bullet(&mut lines, &name),
                    PreviewPart::Text(text) => render_asst_bullet(&mut lines, &text),
                }
            }
        }
    }

    lines
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
        .filter(|(_, ts)| ts.elapsed() < STATUS_MSG_TTL)
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
                Tab::Sessions => "tab:next  h/j/k/l:nav  enter:attach  n:new  N:new in…  i:info  D:why?  f:focus/resume  o:shell  x:close  H:inactive  q:quit",
                Tab::Metrics => "tab:next tab  j/k:scroll  r:refresh  q:quit",
            },
            View::Popup => "j/k:scroll  esc:close  q:close",
            View::LiveTail => "j/k:scroll  G:bottom  esc:close",
            View::ConfirmClose => "y:close  n/esc:cancel",
            View::StateDebug => "j/k:scroll  esc:close  q:close",
            View::PromptInput => "type prompt  enter:dispatch  esc:cancel",
            View::TmuxPane => "forwarding keys to tmux · F1: detach & close",
            View::FolderPicker => "j/k:move  enter:descend  bksp:parent  space:pick  .:pick cwd  c/C:gh new (pub/priv)  esc:cancel",
            View::GhCreateInput => "type name  tab:toggle public/private  enter:create  esc:cancel",
        };
        spans.push(Span::styled(
            format!(" {} ", keybinds),
            Style::default().fg(Color::DarkGray),
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

fn render_metrics_body(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 2 {
        return;
    }

    let m = match &app.metrics {
        Some(m) => m,
        None => {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " Scanning ~/.claude/projects …",
                    Style::default().fg(Color::DarkGray),
                ))),
                area,
            );
            return;
        }
    };

    let lines = build_metrics_content(m);
    let total_lines = lines.len() as u16;

    let scroll_info = format!(
        " {}/{} ",
        (app.metrics_scroll as usize).min(total_lines.saturating_sub(1) as usize) + 1,
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

    let body_area = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    let content = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.metrics_scroll, 0));
    frame.render_widget(content, body_area);
}

fn build_metrics_content(m: &MetricsAnalysis) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
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

    lines.push(section_header("Top sessions"));
    lines.push(Line::from(Span::styled(
        format!(
            "  {:<10} {:>8} {:>10} {:<22} {:<24}",
            "session", "cost", "tokens", "model", "project"
        ),
        dim,
    )));
    for s in &m.top_sessions {
        lines.push(format_session_row(s, dim, val));
    }

    lines
}

fn format_session_row(s: &SessionSummary, dim: Style, val: Style) -> Line<'static> {
    let sid = short_sid(&s.session_id).to_string();
    let mark = if s.is_subagent { "  ⑂" } else { "   " };
    let toks = format_tokens(s.tokens.total());
    let model = short_model(&s.model);
    Line::from(vec![
        Span::styled(format!("{}{:<8}", mark, sid), Style::default().fg(Color::Cyan)),
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


