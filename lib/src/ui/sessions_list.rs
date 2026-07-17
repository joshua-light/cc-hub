//! Sessions tab, list layout: one compact row per session under the same
//! per-project group headers as the card grid. An experimental alternative
//! layout, toggled at runtime with `v` (see [`crate::app::SessionsLayout`]).
//!
//! Rows are laid out as a table: a flexible title region on the left and a
//! cluster of fixed-width metadata columns (linked task, branch, model,
//! tool odometer, last-activity clock, context %) on the right, so values
//! line up vertically across rows. Columns that don't fit the terminal
//! width are dropped for every row at once, lowest-value first, to keep
//! alignment.
//!
//! All width math counts terminal *advance* — `chars().count()`, one cell
//! per glyph. Nerd Font icons visually bleed into the following cell but
//! still advance the cursor by one, so every icon is followed by a space
//! the bleed can safely overlap; budgeting them as two cells (as the card
//! renderer's right-edge math does) would skew rows whose cells are blank
//! or whose padding bottoms out at zero.

use crate::app::App;
use crate::models::{first_line_truncated, SessionInfo, SessionState};
use crate::ui::common::{
    context_window_size, ctx_color, format_elapsed, short_model, state_indicator, task_color,
};
use crate::ui::palette::{CONTEXT_GRAY, MUTED_TEXT};
use crate::ui::sessions::{
    render_group_header, render_no_sessions, role_prefix, spinner_frame, GROUP_GAP,
    GROUP_HEADER_HEIGHT,
};
use crate::ui::now_ms;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

// Fixed advance widths of the metadata columns: icon(1) + space(1) + value.
/// Linked-task mark: icon + 20 chars of task title.
const TASK_W: usize = 22;
const BRANCH_W: usize = 22;
const MODEL_W: usize = 10;
/// Tool odometer: up to 5 digits.
const TOOLS_W: usize = 7;
/// Last-activity clock: widest realistic value is "23h 59m" / "30d 12h".
const ELAPSED_W: usize = 10;
/// Context bar: "100%" at the widest.
const CTX_W: usize = 6;
const COL_SEP: usize = 2;
/// Selection marker(1) + state icon(1) + space(1).
const LEFT_FIXED: usize = 3;
/// The title region never shrinks below this; metadata columns drop first.
const MIN_TITLE: usize = 24;

/// Which optional metadata columns fit the current terminal width. Decided
/// once per frame so every row shows the same columns. The task column also
/// requires at least one visible session to carry a task link — a column of
/// blanks would waste a quarter of the row for nothing.
struct ListColumns {
    task: bool,
    branch: bool,
    model: bool,
    tools: bool,
}

fn plan_columns(width: usize, any_task: bool) -> ListColumns {
    let mut avail =
        width.saturating_sub(LEFT_FIXED + MIN_TITLE + ELAPSED_W + CTX_W + 2 * COL_SEP);
    let mut cols = ListColumns {
        task: false,
        branch: false,
        model: false,
        tools: false,
    };
    if any_task && avail >= TASK_W + COL_SEP {
        cols.task = true;
        avail -= TASK_W + COL_SEP;
    }
    if avail >= BRANCH_W + COL_SEP {
        cols.branch = true;
        avail -= BRANCH_W + COL_SEP;
    }
    if avail >= TOOLS_W + COL_SEP {
        cols.tools = true;
        avail -= TOOLS_W + COL_SEP;
    }
    if avail >= MODEL_W + COL_SEP {
        cols.model = true;
    }
    cols
}

pub(crate) fn render_list(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.sessions.groups.is_empty() {
        render_no_sessions(frame, area);
        return;
    }

    // Content-space y offset of each group: header + one row per session
    // + gap. Same bookkeeping as the grid with a cell height of 1.
    let mut group_offsets: Vec<u16> = Vec::new();
    let mut y_acc: u16 = 0;
    for group in &app.sessions.groups {
        group_offsets.push(y_acc);
        y_acc =
            y_acc.saturating_add(GROUP_HEADER_HEIGHT + group.sessions.len() as u16 + GROUP_GAP);
    }

    // Auto-scroll to keep the selected row visible (prefer its header too).
    {
        let g_offset = group_offsets[app.sessions.sel_group];
        let row_y = g_offset + GROUP_HEADER_HEIGHT + app.sessions.sel_in_group as u16;
        let row_bottom = row_y + 1;
        if row_bottom.saturating_sub(g_offset) <= area.height {
            if g_offset < app.render.grid_scroll {
                app.render.grid_scroll = g_offset;
            } else if row_bottom > app.render.grid_scroll + area.height {
                app.render.grid_scroll = row_bottom.saturating_sub(area.height);
            }
        } else if row_y < app.render.grid_scroll {
            app.render.grid_scroll = row_y;
        } else if row_bottom > app.render.grid_scroll + area.height {
            app.render.grid_scroll = row_bottom.saturating_sub(area.height);
        }
    }

    let scroll = app.render.grid_scroll;
    let now = now_ms();
    let any_task = app
        .sessions
        .groups
        .iter()
        .flat_map(|g| &g.sessions)
        .any(|s| app.task_badge(&s.session_id).is_some());
    let cols = plan_columns(area.width as usize, any_task);
    let roles_by_tmux = app.projects.snapshot.roles_by_tmux();

    for (gi, group) in app.sessions.groups.iter().enumerate() {
        let g_y = group_offsets[gi];

        let header_sy = g_y as i32 - scroll as i32;
        if header_sy >= 0 && header_sy < area.height as i32 {
            let hy = area.y + header_sy as u16;
            render_group_header(frame, Rect::new(area.x, hy, area.width, 1), group);
        }

        for (si, session) in group.sessions.iter().enumerate() {
            let row_sy = (g_y + GROUP_HEADER_HEIGHT + si as u16) as i32 - scroll as i32;
            if row_sy < 0 || row_sy >= area.height as i32 {
                continue;
            }
            let row_area = Rect::new(area.x, area.y + row_sy as u16, area.width, 1);
            let selected = gi == app.sessions.sel_group && si == app.sessions.sel_in_group;
            let role = session
                .tmux_session
                .as_deref()
                .and_then(|t| roles_by_tmux.get(t));
            let badge = app.task_badge(&session.session_id);
            render_row(
                frame,
                row_area,
                session,
                role,
                badge.as_ref(),
                &cols,
                selected,
                now,
            );
        }
    }
}

/// A right-cluster cell, padded to `target` advance columns. Blank cells
/// (`text.is_empty()`) still occupy the full column so rows stay aligned.
struct Cell {
    text: String,
    target: usize,
    style: Style,
    right_align: bool,
}

impl Cell {
    fn push_spans(self, spans: &mut Vec<Span<'static>>) {
        spans.push(Span::raw(" ".repeat(COL_SEP)));
        let pad = self.target.saturating_sub(self.text.chars().count());
        if self.right_align {
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(self.text, self.style));
        } else {
            spans.push(Span::styled(self.text, self.style));
            spans.push(Span::raw(" ".repeat(pad)));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    frame: &mut Frame,
    area: Rect,
    session: &SessionInfo,
    role: Option<&crate::projects_scan::SessionRole>,
    badge: Option<&crate::models::TaskBadge>,
    cols: &ListColumns,
    selected: bool,
    now: u64,
) {
    let width = area.width as usize;
    let (indicator, ind_color) = state_indicator(&session.state);
    let indicator = if session.state == SessionState::Processing {
        spinner_frame(now)
    } else {
        indicator
    };

    let mut cluster: Vec<Cell> = Vec::new();
    if cols.task {
        let (text, style) = match badge {
            Some(b) => {
                let color = if b.stale {
                    Color::DarkGray
                } else {
                    task_color(&b.task_id)
                };
                (
                    format!("󰓹 {}", first_line_truncated(&b.title, TASK_W - 2)),
                    Style::default().fg(color),
                )
            }
            None => (String::new(), Style::default()),
        };
        cluster.push(Cell {
            text,
            target: TASK_W,
            style,
            right_align: false,
        });
    }
    if cols.branch {
        let text = match session.git_branch.as_deref().filter(|b| !b.is_empty()) {
            Some(b) => format!("󰘦 {}", first_line_truncated(b, BRANCH_W - 2)),
            None => String::new(),
        };
        cluster.push(Cell {
            text,
            target: BRANCH_W,
            style: Style::default().fg(Color::Cyan),
            right_align: false,
        });
    }
    if cols.model {
        let text = match session.model.as_deref().filter(|m| !m.is_empty()) {
            Some(m) => format!("󰧑 {}", first_line_truncated(short_model(m), MODEL_W - 2)),
            None => String::new(),
        };
        cluster.push(Cell {
            text,
            target: MODEL_W,
            style: Style::default().fg(Color::DarkGray),
            right_align: false,
        });
    }
    if cols.tools {
        let text = if session.tool_uses_count > 0 {
            format!("󰖷 {}", session.tool_uses_count)
        } else {
            String::new()
        };
        cluster.push(Cell {
            text,
            target: TOOLS_W,
            style: Style::default().fg(Color::DarkGray),
            right_align: true,
        });
    }
    {
        let text = match session.last_activity {
            Some(ts) => format!("󰔟 {}", format_elapsed(now, ts)),
            None => String::new(),
        };
        cluster.push(Cell {
            text,
            target: ELAPSED_W,
            style: Style::default().fg(Color::DarkGray),
            right_align: true,
        });
    }
    {
        let (text, color) = match session.context_tokens {
            Some(ctx) => {
                let window = context_window_size(session.model.as_deref().unwrap_or(""));
                let pct = ((ctx as f64 / window as f64) * 100.0).min(999.0);
                let pct_u8 = (pct as u64).min(100) as u8;
                (format!("󰍛 {:.0}%", pct), ctx_color(pct_u8))
            }
            None => (String::new(), Color::DarkGray),
        };
        cluster.push(Cell {
            text,
            target: CTX_W,
            style: Style::default().fg(color),
            right_align: true,
        });
    }
    let cluster_width: usize = cluster.iter().map(|c| c.target + COL_SEP).sum();

    // Title region: role prefix + agent badge + Haiku title (falling back
    // to the last user message, same priority as the card body).
    let title_budget = width.saturating_sub(LEFT_FIXED + cluster_width);
    let prefix = role_prefix(role).unwrap_or_default();
    let agent_badge = if session.agent_id == "claude" {
        String::new()
    } else {
        format!("[{}] ", session.agent_badge())
    };
    let prefix_w = prefix.chars().count() + agent_badge.chars().count();

    let attention = session.needs_attention();
    let (text, text_style) = match session.title.as_deref() {
        Some(t) if !t.is_empty() => {
            let style = if attention {
                Style::default().fg(ind_color).add_modifier(Modifier::BOLD)
            } else if session.state == SessionState::Inactive {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            (t.to_string(), style)
        }
        _ if session.titling => ("✎ …".to_string(), Style::default().fg(MUTED_TEXT)),
        _ => {
            let msg = session
                .last_user_message
                .as_ref()
                .or(session.summary.as_ref())
                .map(|m| m.replace('\n', " "))
                .unwrap_or_default();
            (msg, Style::default().fg(CONTEXT_GRAY))
        }
    };
    let text = first_line_truncated(&text, title_budget.saturating_sub(prefix_w));
    let used = prefix_w + text.chars().count();

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(if selected {
        Span::styled("▌", Style::default().fg(Color::White))
    } else {
        Span::raw(" ")
    });
    spans.push(Span::styled(
        format!("{} ", indicator),
        Style::default().fg(ind_color),
    ));
    if prefix_w > 0 {
        spans.push(Span::styled(
            format!("{}{}", prefix, agent_badge),
            Style::default().fg(MUTED_TEXT),
        ));
    }
    spans.push(Span::styled(text, text_style));
    spans.push(Span::raw(" ".repeat(title_budget.saturating_sub(used))));

    for cell in cluster {
        cell.push_spans(&mut spans);
    }

    let mut row = Paragraph::new(Line::from(spans));
    if selected {
        // Full-row background highlight is the list's selection cue — the
        // grid's double border has no one-row-tall equivalent.
        row = row.style(Style::default().bg(Color::Rgb(40, 40, 52)));
    }
    frame.render_widget(row, area);
}
