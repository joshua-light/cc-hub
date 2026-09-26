use super::card::render_card;
use super::{
    keep_in_view, render_group_header, render_no_sessions, GROUP_GAP, GROUP_HEADER_HEIGHT,
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

    // Compute content-space y offset for each group
    let mut group_offsets: Vec<u16> = Vec::new();
    let mut y_acc: u16 = 0;
    for group in &app.sessions.groups {
        group_offsets.push(y_acc);
        let rows = group.sessions.len().div_ceil(cols) as u16;
        y_acc = y_acc.saturating_add(GROUP_HEADER_HEIGHT + rows * cell_height() + GROUP_GAP);
    }

    let g_offset = group_offsets[app.sessions.sel_group];
    let card_row = (app.sessions.sel_in_group / cols) as u16;
    let card_y = g_offset + GROUP_HEADER_HEIGHT + card_row * cell_height();
    keep_in_view(
        &mut app.render.grid_scroll,
        g_offset,
        card_y,
        card_y + cell_height(),
        area.height,
    );

    let scroll = app.render.grid_scroll;
    let now = now_ms();
    for (gi, group) in app.sessions.groups.iter().enumerate() {
        let g_y = group_offsets[gi];

        // Render group header
        let header_sy = g_y as i32 - scroll as i32;
        if header_sy >= 0 && header_sy < area.height as i32 {
            let hy = area.y + header_sy as u16;
            render_group_header(frame, Rect::new(area.x, hy, area.width, 1), group);
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
            let w = if col == app.render.grid_cols - 1 {
                area.x + area.width - x
            } else {
                cell_width
            };

            let is_selected = gi == app.sessions.sel_group && si == app.sessions.sel_in_group;
            let cell_area = Rect::new(x, cy, w, cell_height());
            let badge = app.task_badge(&session.session_id);
            render_card(frame, cell_area, session, badge.as_ref(), is_selected, now);
        }
    }
}
