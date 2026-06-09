//! Sessions tab: the card grid and the per-session detail popup.

use crate::app::App;
use crate::models::{short_sid, SessionDetail, SessionInfo, SessionState};
use crate::ui::common::{
    centered_rect, context_window_size, format_datetime, format_elapsed, format_time,
    format_tokens, format_tool_label, popup_block, short_model, state_color, state_indicator,
};
use crate::ui::palette::{CONTEXT_GRAY, MUTED_TEXT, PURPLE, SEP_GRAY};
use crate::ui::{cell_height, now_ms};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

pub(crate) const GROUP_HEADER_HEIGHT: u16 = 1;
pub(crate) const GROUP_GAP: u16 = 1;

pub(crate) fn render_grid(frame: &mut Frame, area: Rect, app: &mut App) {
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
                Style::default().fg(SEP_GRAY),
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

pub(crate) fn render_card(
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
        SEP_GRAY
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
        Span::styled(" ", Style::default().fg(MUTED_TEXT)),
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
        Some(("󰛨 Thinking".to_string(), PURPLE))
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
                Span::styled("󰍡 ", Style::default().fg(MUTED_TEXT)),
                Span::styled(msg.clone(), Style::default().fg(CONTEXT_GRAY)),
            ]));
        } else {
            let first_line: String = chars[..max_w].iter().collect();
            let remaining: String = chars[max_w..]
                .iter()
                .take(max_w.saturating_sub(3))
                .collect();
            lines.push(Line::from(vec![
                Span::styled("󰍡 ", Style::default().fg(MUTED_TEXT)),
                Span::styled(first_line, Style::default().fg(CONTEXT_GRAY)),
            ]));
            let second = if chars.len() > max_w.saturating_mul(2).saturating_sub(3) {
                format!("{}...", remaining)
            } else {
                remaining
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(second, Style::default().fg(CONTEXT_GRAY)),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

pub(crate) fn render_popup(frame: &mut Frame, area: Rect, app: &App) {
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

pub(crate) fn build_popup_content(detail: &SessionDetail, width: u16) -> Vec<Line<'static>> {
    let session = &detail.info;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(MUTED_TEXT)),
        Span::styled("Path:    ", Style::default().fg(Color::DarkGray)),
        Span::styled(session.cwd.clone(), Style::default().fg(Color::White)),
    ]));

    let mut meta_spans = vec![
        Span::styled("󰚩  ", Style::default().fg(MUTED_TEXT)),
        Span::styled("Agent:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(session.agent_badge(), Style::default().fg(Color::White)),
        Span::styled("     ", Style::default().fg(MUTED_TEXT)),
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
        Span::styled("󰆏 ", Style::default().fg(MUTED_TEXT)),
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
            Span::styled("󰍡 ", Style::default().fg(MUTED_TEXT)),
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
