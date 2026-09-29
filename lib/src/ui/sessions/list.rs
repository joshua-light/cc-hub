//! List layout: one row per session under the grid's group headers.
//!
//! Rows form a table: a flexible title on the left and fixed-width metadata
//! columns on the right, so values line up across rows. Columns that don't
//! fit drop for every row at once, lowest value first. The task column alone
//! is elastic: it widens into leftover space until the longest visible task
//! name fits.
//!
//! Within a group, a blank row splits runs linked to different tasks (the
//! unlinked tail is one run), so the sort's task clusters read as blocks.
//!
//! Width math counts terminal advance, one cell per glyph
//! (`chars().count()`). Nerd Font icons bleed into the next cell but advance
//! by one, so each icon is followed by a space the bleed can overlap.
//! Budgeting icons as two cells, as the card's right-edge math does, would
//! skew rows whose cells are blank or whose padding bottoms out at zero.

use super::{
    activity_clock, agent_prefix, animated_indicator, badge_color, context_pct, hold_header_above,
    keep_in_view, render_headers, render_no_sessions, CardMarks, GROUP_GAP, GROUP_HEADER_HEIGHT,
};
use crate::app::App;
use crate::models::{first_line_truncated, SessionInfo, SessionState};
use crate::ui::common::{ctx_color, format_elapsed, selection_stripe, short_model, Cell, COL_SEP};
use crate::ui::now_ms;
use crate::ui::palette::{CONTEXT_GRAY, HANDOFF_BLUE, MUTED_TEXT, SELECTED_ROW_BG};
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

pub(super) fn render_list(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.sessions.groups.is_empty() {
        render_no_sessions(frame, area);
        return;
    }

    // Content-space y of each group: the On hold header if the group opens
    // that section, its own header, one row per session plus task
    // separators, then the gap.
    let mut group_offsets: Vec<u16> = Vec::new();
    let mut row_offsets: Vec<Vec<u16>> = Vec::new();
    let mut y_acc: u16 = 0;
    for (gi, group) in app.sessions.groups.iter().enumerate() {
        y_acc = y_acc.saturating_add(hold_header_above(&app.sessions.groups, gi));
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

    let g_offset = group_offsets[app.sessions.sel_group];
    let row_y = g_offset
        + GROUP_HEADER_HEIGHT
        + row_offsets[app.sessions.sel_group][app.sessions.sel_in_group];
    keep_in_view(
        &mut app.render.grid_scroll,
        g_offset - hold_header_above(&app.sessions.groups, app.sessions.sel_group),
        row_y,
        row_y + 1,
        area.height,
    );

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

    for (gi, group) in app.sessions.groups.iter().enumerate() {
        let g_y = group_offsets[gi];
        render_headers(frame, area, &app.sessions.groups, gi, g_y, scroll);

        for (si, session) in group.sessions.iter().enumerate() {
            let row_sy = (g_y + GROUP_HEADER_HEIGHT + row_offsets[gi][si]) as i32 - scroll as i32;
            if row_sy < 0 || row_sy >= area.height as i32 {
                continue;
            }
            let row_area = Rect::new(area.x, area.y + row_sy as u16, area.width, 1);
            let marks = CardMarks::of(app, gi, si, session);
            let badge = app.task_badge(&session.session_id);
            render_row(frame, row_area, session, badge.as_ref(), &cols, marks, now);
        }
    }
}

fn render_row(
    frame: &mut Frame,
    area: Rect,
    session: &SessionInfo,
    badge: Option<&crate::models::TaskBadge>,
    cols: &ListColumns,
    marks: CardMarks,
    now: u64,
) {
    let CardMarks {
        selected, handoff, ..
    } = marks;
    let width = area.width as usize;
    let (indicator, ind_color) = animated_indicator(&session.state, marks, now);

    let mut cluster: Vec<Cell> = Vec::new();
    if cols.task_w > 0 {
        let (text, style) = match badge {
            Some(b) => (
                format!("󰓹 {}", first_line_truncated(&b.title, cols.task_w - 2)),
                Style::default().fg(badge_color(b)),
            ),
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
        let (icon, color) = activity_clock(session, now);
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
        let (text, color) = match context_pct(session) {
            Some((pct, pct_u8)) => (format!("󰍛 {:.0}%", pct), ctx_color(pct_u8)),
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

    // Title region: agent badge, then the Haiku title, falling back to the
    // last user message as the card body does.
    let title_budget = width.saturating_sub(LEFT_FIXED + cluster_width);
    let agent_badge = agent_prefix(session);
    let prefix_w = agent_badge.chars().count();

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
    // The gutter stripe is the cursor, in white; a handoff mark keeps it lit
    // in its own colour whether or not the cursor is there.
    spans.push(if handoff {
        Span::styled("▌", Style::default().fg(HANDOFF_BLUE))
    } else {
        selection_stripe(selected)
    });
    spans.push(Span::styled(
        format!("{} ", indicator),
        Style::default().fg(ind_color),
    ));
    if prefix_w > 0 {
        spans.push(Span::styled(agent_badge, Style::default().fg(MUTED_TEXT)));
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
        row = row.style(Style::default().bg(SELECTED_ROW_BG));
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

    #[cfg(unix)]
    #[test]
    fn held_groups_sit_under_the_on_hold_header() {
        use crate::models::ProjectGroup;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        crate::test_util::with_temp_home(|| {
            let group = |name: &str, held| ProjectGroup {
                name: name.into(),
                cwd: format!("/tmp/{name}"),
                sessions: vec![crate::models::SessionInfo {
                    session_id: format!("{name}-1"),
                    ..crate::test_util::session_info()
                }],
                held,
            };
            let mut app = App::new();
            app.sessions.groups = vec![group("alpha", false), group("beta", true)];
            app.sessions.holds.toggle("beta-1");

            let mut terminal = Terminal::new(TestBackend::new(100, 8)).expect("terminal");
            terminal
                .draw(|f| render_list(f, f.area(), &mut app))
                .expect("render");
            let buf = terminal.backend().buffer();
            let row = |y: u16| -> String { (0..100).map(|x| buf[(x, y)].symbol()).collect() };

            // alpha's header and row, the gap, then the section header right
            // above beta's own header.
            assert!(row(0).contains("alpha"), "{}", row(0));
            assert!(row(2).trim().is_empty(), "{}", row(2));
            assert!(
                row(3).contains("On hold") && row(3).contains("1 held"),
                "{}",
                row(3)
            );
            assert!(row(4).contains("beta"), "{}", row(4));

            // Only the held session trades its state glyph for the pause.
            assert!(!row(1).contains(super::super::HOLD_ICON), "{}", row(1));
            assert!(row(5).contains(super::super::HOLD_ICON), "{}", row(5));
            let glyph = (0..100)
                .map(|x| &buf[(x, 5)])
                .find(|c| c.symbol() == super::super::HOLD_ICON)
                .expect("pause glyph");
            assert_eq!(glyph.fg, crate::ui::palette::MUTED_TEXT);
        });
    }
}
