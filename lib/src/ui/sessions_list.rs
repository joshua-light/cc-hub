//! Sessions tab, list layout: one compact row per session under the same
//! per-project group headers as the card grid. An experimental alternative
//! layout, toggled at runtime with `v` (see [`crate::app::SessionsLayout`]).
//!
//! Rows are laid out as a table: a flexible title region on the left and a
//! cluster of fixed-width metadata columns (linked task, branch, model,
//! tool odometer, last-activity clock, context %) on the right, so values
//! line up vertically across rows. Columns that don't fit the terminal
//! width are dropped for every row at once, lowest-value first, to keep
//! alignment. The task column alone is elastic: it widens into leftover
//! row space until the longest visible task name fits.
//!
//! Within a group, a blank separator row splits runs of rows linked to
//! different tasks (the unlinked tail counts as one run of its own), so
//! the task clusters the sort already builds read as visual blocks.
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
    COLD_CACHE_ICON,
};
use crate::ui::now_ms;
use crate::ui::palette::{CONTEXT_GRAY, ICE_BLUE, MUTED_TEXT};
use crate::ui::sessions::{
    render_group_header, render_no_sessions, role_prefix, spinner_frame, starting_frame, GROUP_GAP,
    GROUP_HEADER_HEIGHT,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

// Fixed advance widths of the metadata columns: icon(1) + space(1) + value.
/// Linked-task mark, minimum width: icon + 20 chars of task title. The
/// column grows into leftover row space (up to [`TASK_W_MAX`]) until the
/// longest visible task name fits — see [`plan_columns`].
const TASK_W: usize = 22;
/// Growth cap for the task column: past this, extra width pads titles again.
/// Keeps one very long task name from squeezing every row's title down to
/// [`MIN_TITLE`].
const TASK_W_MAX: usize = 42;
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
    /// Advance width of the linked-task column; 0 when the column is hidden.
    task_w: usize,
    branch: bool,
    model: bool,
    tools: bool,
}

/// `task_need` is the advance width the widest visible task name would take
/// untruncated (`None` when no session carries a task link).
fn plan_columns(width: usize, task_need: Option<usize>) -> ListColumns {
    let mut avail = width.saturating_sub(LEFT_FIXED + MIN_TITLE + ELAPSED_W + CTX_W + 2 * COL_SEP);
    let mut cols = ListColumns {
        task_w: 0,
        branch: false,
        model: false,
        tools: false,
    };
    if task_need.is_some() && avail >= TASK_W + COL_SEP {
        cols.task_w = TASK_W;
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
        avail -= MODEL_W + COL_SEP;
    }
    // Leftover space would otherwise just pad the title region; spend it on
    // the task column instead, up to what the longest task name actually
    // needs (capped so one huge name can't starve the titles).
    if cols.task_w > 0 {
        let need = task_need.unwrap_or(0).clamp(TASK_W, TASK_W_MAX);
        cols.task_w += need.saturating_sub(TASK_W).min(avail);
    }
    cols
}

/// Body-row offset of each session under its group header (0 = the row
/// right below it): the session index plus one blank separator row at
/// every task boundary — adjacent rows linked to different tasks, with
/// unlinked rows all counting as one shared run.
fn body_row_offsets<'a>(tasks: impl Iterator<Item = Option<&'a str>>) -> Vec<u16> {
    let mut offsets = Vec::new();
    let mut y: u16 = 0;
    let mut prev: Option<Option<&'a str>> = None;
    for task in tasks {
        if prev.is_some_and(|p| p != task) {
            y = y.saturating_add(1);
        }
        offsets.push(y);
        y = y.saturating_add(1);
        prev = Some(task);
    }
    offsets
}

pub(crate) fn render_list(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.sessions.groups.is_empty() {
        render_no_sessions(frame, area);
        return;
    }

    // Content-space y offset of each group: header + one row per session
    // + separators at task boundaries + gap. Same bookkeeping as the grid
    // with a cell height of 1, plus the separator rows.
    let mut group_offsets: Vec<u16> = Vec::new();
    let mut row_offsets: Vec<Vec<u16>> = Vec::new();
    let mut y_acc: u16 = 0;
    for group in &app.sessions.groups {
        group_offsets.push(y_acc);
        let offsets = body_row_offsets(group.sessions.iter().map(|s| {
            app.session_task_links
                .get(&s.session_id)
                .map(|l| l.task_id.as_str())
        }));
        let body_h = offsets.last().map_or(0, |&o| o + 1);
        row_offsets.push(offsets);
        y_acc = y_acc.saturating_add(GROUP_HEADER_HEIGHT + body_h + GROUP_GAP);
    }

    // Auto-scroll to keep the selected row visible (prefer its header too).
    {
        let g_offset = group_offsets[app.sessions.sel_group];
        let row_y = g_offset
            + GROUP_HEADER_HEIGHT
            + row_offsets[app.sessions.sel_group][app.sessions.sel_in_group];
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
    // Widest task name across every row (not just scrolled-into-view ones),
    // so the column width can't shift while scrolling.
    let task_need = app
        .sessions
        .groups
        .iter()
        .flat_map(|g| &g.sessions)
        .filter_map(|s| app.task_badge(&s.session_id))
        .map(|b| b.title.lines().next().unwrap_or("").chars().count() + 2)
        .max();
    let cols = plan_columns(area.width as usize, task_need);
    let roles_by_tmux = app.projects.snapshot.roles_by_tmux();

    for (gi, group) in app.sessions.groups.iter().enumerate() {
        let g_y = group_offsets[gi];

        let header_sy = g_y as i32 - scroll as i32;
        if header_sy >= 0 && header_sy < area.height as i32 {
            let hy = area.y + header_sy as u16;
            render_group_header(frame, Rect::new(area.x, hy, area.width, 1), group);
        }

        for (si, session) in group.sessions.iter().enumerate() {
            let row_sy = (g_y + GROUP_HEADER_HEIGHT + row_offsets[gi][si]) as i32 - scroll as i32;
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
    let indicator = match session.state {
        SessionState::Processing => spinner_frame(now),
        SessionState::Starting => starting_frame(now),
        _ => indicator,
    };

    let mut cluster: Vec<Cell> = Vec::new();
    if cols.task_w > 0 {
        let (text, style) = match badge {
            Some(b) => {
                let color = if b.stale {
                    Color::DarkGray
                } else {
                    task_color(&b.task_id)
                };
                (
                    format!("󰓹 {}", first_line_truncated(&b.title, cols.task_w - 2)),
                    Style::default().fg(color),
                )
            }
            None => (String::new(), Style::default()),
        };
        cluster.push(Cell {
            text,
            target: cols.task_w,
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
        // Same swap as the card footer: past the prompt-cache TTL the clock
        // becomes the ice-blue snowflake — restarting beats resuming.
        let (icon, color) = if session.cache_cold(now) {
            (COLD_CACHE_ICON, ICE_BLUE)
        } else {
            ("󰔟", Color::DarkGray)
        };
        let text = match session.last_activity {
            Some(ts) => format!("{} {}", icon, format_elapsed(now, ts)),
            None => String::new(),
        };
        cluster.push(Cell {
            text,
            target: ELAPSED_W,
            style: Style::default().fg(color),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Width where every column fits at its base width with `extra` cells
    /// left over.
    fn width_with_leftover(extra: usize) -> usize {
        LEFT_FIXED
            + MIN_TITLE
            + ELAPSED_W
            + CTX_W
            + TASK_W
            + BRANCH_W
            + TOOLS_W
            + MODEL_W
            + 6 * COL_SEP
            + extra
    }

    #[test]
    fn task_column_stays_at_base_width_without_leftover() {
        let cols = plan_columns(width_with_leftover(0), Some(40));
        assert_eq!(cols.task_w, TASK_W);
        assert!(cols.branch && cols.tools && cols.model);
    }

    #[test]
    fn task_column_grows_into_leftover_up_to_need() {
        // Need 30 cells, 100 spare: grow exactly to the need.
        let cols = plan_columns(width_with_leftover(100), Some(30));
        assert_eq!(cols.task_w, 30);
        // Need 30 cells, 5 spare: grow only as far as the leftover allows.
        let cols = plan_columns(width_with_leftover(5), Some(30));
        assert_eq!(cols.task_w, TASK_W + 5);
    }

    #[test]
    fn task_column_growth_is_capped() {
        let cols = plan_columns(width_with_leftover(200), Some(120));
        assert_eq!(cols.task_w, TASK_W_MAX);
    }

    #[test]
    fn task_column_hidden_without_any_task() {
        let cols = plan_columns(width_with_leftover(100), None);
        assert_eq!(cols.task_w, 0);
    }

    #[test]
    fn separator_rows_split_task_runs() {
        // tk-1, tk-1 | tk-2 | unlinked, unlinked: one blank row at each
        // run boundary, none inside a run and none between unlinked rows.
        let offsets =
            body_row_offsets([Some("tk-1"), Some("tk-1"), Some("tk-2"), None, None].into_iter());
        assert_eq!(offsets, vec![0, 1, 3, 5, 6]);
    }

    #[test]
    fn no_separators_without_task_links() {
        let offsets = body_row_offsets([None, None, None].into_iter());
        assert_eq!(offsets, vec![0, 1, 2]);
    }
}
