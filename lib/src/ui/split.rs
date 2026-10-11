//! The sidebar-and-page layout the Sessions, Agents and Builds tabs share:
//! a narrow list on the left, the selected item's page on the right. One
//! side has the keyboard; the other is drawn dim, as an editor's inactive
//! pane is, so where typing goes reads at a glance.

use crate::ui::palette::{SELECTED_ROW_BG, SEP_GRAY};
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

    /// Dim the side without the keyboard. Call it last, over both sides.
    pub fn dim_unfocused(&self, frame: &mut Frame) {
        let unfocused = match self.page_focused {
            true => self.sidebar,
            false => self.page,
        };
        frame
            .buffer_mut()
            .set_style(unfocused, Style::default().add_modifier(Modifier::DIM));
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

/// One sidebar row of the Agents and Builds tabs: ` mark name`, and a fact
/// right-aligned at the far end. The cursor is the row's background, and its
/// name alone is bold.
pub(crate) struct SidebarRow {
    pub mark: Span<'static>,
    pub name: String,
    pub name_style: Style,
    pub aside: Span<'static>,
}

pub(crate) fn render_row(frame: &mut Frame, area: Rect, row: SidebarRow, selected: bool) {
    let name_style = match selected {
        true => row.name_style.add_modifier(Modifier::BOLD),
        false => row.name_style,
    };
    let width = area.width as usize;
    let aside_w = row.aside.content.chars().count();
    // ` ● ` ahead of the name, a space before the aside and one after it.
    let room = width.saturating_sub(3 + aside_w + 2);
    let name = crate::models::first_line_truncated(&row.name, room);
    let gap = width.saturating_sub(3 + name.chars().count() + aside_w + 1);
    let line = Line::from(vec![
        Span::raw(" "),
        row.mark,
        Span::raw(" "),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(gap)),
        row.aside,
    ]);
    let mut row = Paragraph::new(line);
    if selected {
        row = row.style(Style::default().bg(SELECTED_ROW_BG));
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

    #[test]
    fn a_wide_screen_puts_the_page_beside_the_list() {
        let split = Split::of(Rect::new(0, 0, 120, 10), false);
        assert_eq!(split.sidebar, Rect::new(0, 0, 32, 10));
        assert_eq!(split.page, Rect::new(33, 0, 87, 10));
    }
}
