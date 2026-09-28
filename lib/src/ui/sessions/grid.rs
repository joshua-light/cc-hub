use super::card::render_card;
use super::{
    hold_header_above, keep_in_view, render_headers, render_no_sessions, CardMarks, GROUP_GAP,
    GROUP_HEADER_HEIGHT,
};
use crate::app::App;
use crate::ui::{cell_height, now_ms};
use ratatui::layout::Rect;
use ratatui::Frame;

pub(super) fn render_grid(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.sessions.groups.is_empty() {
        render_no_sessions(frame, area);
        return;
    }

    let cols = app.render.grid_cols as usize;
    let cell_width = area.width / app.render.grid_cols;

    // Content-space y of each group, before scrolling.
    let mut group_offsets: Vec<u16> = Vec::new();
    let mut y_acc: u16 = 0;
    for (gi, group) in app.sessions.groups.iter().enumerate() {
        y_acc = y_acc.saturating_add(hold_header_above(&app.sessions.groups, gi));
        group_offsets.push(y_acc);
        let rows = group.sessions.len().div_ceil(cols) as u16;
        y_acc = y_acc.saturating_add(GROUP_HEADER_HEIGHT + rows * cell_height() + GROUP_GAP);
    }

    let g_offset = group_offsets[app.sessions.sel_group];
    let card_row = (app.sessions.sel_in_group / cols) as u16;
    let card_y = g_offset + GROUP_HEADER_HEIGHT + card_row * cell_height();
    keep_in_view(
        &mut app.render.grid_scroll,
        g_offset - hold_header_above(&app.sessions.groups, app.sessions.sel_group),
        card_y,
        card_y + cell_height(),
        area.height,
    );

    let scroll = app.render.grid_scroll;
    let now = now_ms();
    for (gi, group) in app.sessions.groups.iter().enumerate() {
        let g_y = group_offsets[gi];
        render_headers(frame, area, &app.sessions.groups, gi, g_y, scroll);

        for (si, session) in group.sessions.iter().enumerate() {
            let col = (si % cols) as u16;
            let row = (si / cols) as u16;

            let card_cy = g_y + GROUP_HEADER_HEIGHT + row * cell_height();
            let card_sy = card_cy as i32 - scroll as i32;

            // A card that would be clipped is skipped.
            if card_sy < 0 || card_sy + cell_height() as i32 > area.height as i32 {
                continue;
            }

            let x = area.x + col * cell_width;
            let cy = area.y + card_sy as u16;
            let w = if col == app.render.grid_cols - 1 {
                area.x + area.width - x
            } else {
                cell_width
            };

            let marks = CardMarks::of(app, gi, si, session);
            let cell_area = Rect::new(x, cy, w, cell_height());
            let badge = app.task_badge(&session.session_id);
            render_card(frame, cell_area, session, badge.as_ref(), marks, now);
        }
    }
}
