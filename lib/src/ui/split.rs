//! The sidebar-and-page layout the Sessions, Agents and Builds tabs share:
//! a narrow list on the left, the selected item's page on the right. One
//! side has the keyboard: the page's frame turns white while it is the one,
//! the list's cursor row fades while it is not, and with
//! `[ui].dim_unfocused` the other side is drawn dim, as an editor's inactive
//! pane is.

use crate::ui::palette::{SELECTED_ROW_BG, SEP_GRAY, UNFOCUSED_ROW_BG};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

/// The sidebar takes a quarter of the width, within these bounds.
const SIDEBAR_MIN: u16 = 32;
const SIDEBAR_MAX: u16 = 48;
/// Narrower than this, the page isn't worth drawing beside the list: only
/// the side with the keyboard is shown.
const PAGE_MIN: u16 = 40;

/// Where the two sides go. Either can be empty: on a narrow screen only
/// the side with the keyboard is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Split {
    pub sidebar: Rect,
    pub page: Rect,
    /// Whether the keyboard is on the page.
    pub page_focused: bool,
}

impl Split {
    pub fn of(area: Rect, page_focused: bool) -> Self {
        let sidebar_w = (area.width / 4).clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        if area.width < sidebar_w + 1 + PAGE_MIN {
            let (sidebar, page) = match page_focused {
                true => (Rect::default(), area),
                false => (area, Rect::default()),
            };
            return Self {
                sidebar,
                page,
                page_focused,
            };
        }
        Self {
            sidebar: Rect::new(area.x, area.y, sidebar_w, area.height),
            page: Rect::new(
                area.x + sidebar_w + 1,
                area.y,
                area.width - sidebar_w - 1,
                area.height,
            ),
            page_focused,
        }
    }

    /// Dim the side without the keyboard when `[ui].dim_unfocused` asks
    /// for it. Call it last, over both sides.
    pub fn dim_unfocused(&self, frame: &mut Frame) {
        if crate::config::get().ui.dim_unfocused {
            self.dim(frame.buffer_mut());
        }
    }

    fn dim(&self, buf: &mut Buffer) {
        let unfocused = match self.page_focused {
            true => self.sidebar,
            false => self.page,
        };
        buf.set_style(unfocused, Style::default().add_modifier(Modifier::DIM));
    }

    /// The sidebar's cursor row background: faded while the page has the
    /// keyboard. The list has no frame to light up, so its cursor does.
    pub fn cursor_bg(&self) -> Color {
        match self.page_focused {
            true => UNFOCUSED_ROW_BG,
            false => SELECTED_ROW_BG,
        }
    }

    /// The page's frame: rounded, white while it has the keyboard.
    pub fn page_block(&self, title: Line<'static>) -> Block<'static> {
        let border = match self.page_focused {
            true => Color::White,
            false => SEP_GRAY,
        };
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .title(title)
    }
}

/// ` ● name `, as the dock titles a session: the mark, then the name in
/// bold white.
pub(crate) fn page_title(mark: Span<'static>, name: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        mark,
        Span::styled(
            format!(" {name} "),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

/// One sidebar row of the Agents and Builds tabs: ` mark name`, nothing
/// more. The cursor is the row's background, `cursor` (see
/// [`Split::cursor_bg`]), and its name alone is bold.
pub(crate) struct SidebarRow {
    pub mark: Span<'static>,
    pub name: String,
    pub name_style: Style,
}

pub(crate) fn render_row(frame: &mut Frame, area: Rect, row: SidebarRow, cursor: Option<Color>) {
    let name_style = match cursor {
        Some(_) => row.name_style.add_modifier(Modifier::BOLD),
        None => row.name_style,
    };
    // ` ● ` ahead of the name, a space after it.
    let room = (area.width as usize).saturating_sub(3 + 1);
    let name = crate::models::first_line_truncated(&row.name, room);
    let line = Line::from(vec![
        Span::raw(" "),
        row.mark,
        Span::raw(" "),
        Span::styled(name, name_style),
    ]);
    let mut row = Paragraph::new(line);
    if let Some(bg) = cursor {
        row = row.style(Style::default().bg(bg));
    }
    frame.render_widget(row, area);
}

/// Keep `selected` on screen in a one-row-per-item list `height` rows tall.
pub(crate) fn keep_row_in_view(scroll: &mut u16, selected: usize, height: u16) {
    let selected = selected as u16;
    if selected < *scroll {
        *scroll = selected;
    } else if height > 0 && selected >= *scroll + height {
        *scroll = selected + 1 - height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrow_screen_shows_only_the_side_with_the_keyboard() {
        let area = Rect::new(0, 0, 60, 10);
        let list = Split::of(area, false);
        assert_eq!((list.sidebar, list.page), (area, Rect::default()));
        let page = Split::of(area, true);
        assert_eq!((page.sidebar, page.page), (Rect::default(), area));
    }

    // Only the side with the keyboard keeps full strength: the page dims
    // while the list has it, and the list while the page has it.
    #[test]
    fn dimming_takes_the_side_without_the_keyboard() {
        let area = Rect::new(0, 0, 120, 4);
        let dimmed = |page_focused: bool| {
            let mut buf = Buffer::empty(area);
            Split::of(area, page_focused).dim(&mut buf);
            let dim = |x: u16| buf[(x, 1)].modifier.contains(Modifier::DIM);
            (dim(3), dim(40))
        };
        assert_eq!(dimmed(false), (false, true));
        assert_eq!(dimmed(true), (true, false));
    }

    // The list has no frame, so its cursor row carries the focus: plain
    // while the list has the keyboard, faded while the page has it.
    #[test]
    fn the_cursor_row_fades_while_the_page_has_the_keyboard() {
        let area = Rect::new(0, 0, 120, 4);
        assert_eq!(Split::of(area, false).cursor_bg(), SELECTED_ROW_BG);
        assert_eq!(Split::of(area, true).cursor_bg(), UNFOCUSED_ROW_BG);
    }

    #[test]
    fn a_wide_screen_puts_the_page_beside_the_list() {
        let split = Split::of(Rect::new(0, 0, 120, 10), false);
        assert_eq!(split.sidebar, Rect::new(0, 0, 32, 10));
        assert_eq!(split.page, Rect::new(33, 0, 87, 10));
    }
}
