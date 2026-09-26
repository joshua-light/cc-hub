//! Building blocks shared by the popups in this module.

use crate::ui::common::{popup_block, CURSOR};
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

/// Split `text` into spans alternating `base`/`hl` style, with `hl` on the
/// chars at `indices` (sorted, as produced by the fuzzy matcher).
pub(super) fn highlight_spans(
    text: &str,
    indices: &[usize],
    base: Style,
    hl: Style,
) -> Vec<Span<'static>> {
    if indices.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let mut spans = Vec::new();
    let mut cur = String::new();
    let mut cur_hl = false;
    for (i, ch) in text.chars().enumerate() {
        let is_hl = indices.binary_search(&i).is_ok();
        if is_hl != cur_hl && !cur.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut cur),
                if cur_hl { hl } else { base },
            ));
        }
        cur_hl = is_hl;
        cur.push(ch);
    }
    if !cur.is_empty() {
        spans.push(Span::styled(cur, if cur_hl { hl } else { base }));
    }
    spans
}

/// Rows a string occupies once the popup's Paragraph wraps it at `width`.
pub(super) fn wrapped_rows(text: &str, width: usize) -> u16 {
    if width == 0 {
        return 1;
    }
    text.chars()
        .count()
        .div_ceil(width)
        .max(1)
        .try_into()
        .unwrap_or(u16::MAX)
}

/// Top row of a fuzzy picker: the live filter with its cursor, and `count`
/// right-aligned on the same row.
pub(super) fn render_filter_row(frame: &mut Frame, inner: Rect, filter: &str, count: String) {
    let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let mut filter_line = filter.to_string();
    filter_line.push(CURSOR);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " ❯ ",
                Style::default()
                    .fg(ACCENT_BLUE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                filter_line,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        filter_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            count,
            Style::default().fg(DIM_TEXT),
        )))
        .alignment(Alignment::Right),
        filter_area,
    );
}

/// Styles for one fuzzy-picker row: a label and a detail, each with a
/// highlight style for the matched chars.
pub(super) struct RowStyles {
    /// Background every span of the row carries. A selected row renders as
    /// a solid white bar, so a span without it leaves a gap in the bar.
    pub(super) bar: Style,
    pub(super) label: Style,
    pub(super) label_hl: Style,
    pub(super) detail: Style,
    pub(super) detail_hl: Style,
}

impl RowStyles {
    /// `label` colours the label of an unselected row; a selected row always
    /// uses the same bar palette.
    pub(super) fn new(selected: bool, label: Color) -> Self {
        if selected {
            let bar = Style::default().bg(Color::White);
            Self {
                bar,
                label: bar.fg(Color::Black).add_modifier(Modifier::BOLD),
                label_hl: bar.fg(Color::Blue).add_modifier(Modifier::BOLD),
                detail: bar.fg(Color::Rgb(90, 90, 100)),
                detail_hl: bar.fg(Color::Blue),
            }
        } else {
            Self {
                bar: Style::default(),
                label: Style::default().fg(label),
                label_hl: Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                detail: Style::default().fg(Color::DarkGray),
                detail_hl: Style::default().fg(Color::Cyan),
            }
        }
    }

    /// The row's leading `▶ ` selection marker, blank when unselected.
    pub(super) fn cursor(&self, selected: bool) -> Span<'static> {
        Span::styled(
            if selected { "▶ " } else { "  " },
            self.bar.fg(Color::Black).add_modifier(Modifier::BOLD),
        )
    }
}

/// Key row closing the input popups: `[enter]` and `[tab]` with their verbs,
/// then `[esc] cancel`. `tab` is `None` for popups without a second mode.
pub(super) fn key_footer(enter: &str, tab: Option<&str>) -> Line<'static> {
    let key = |key: &'static str, color: Color| {
        Span::styled(key, Style::default().fg(color).add_modifier(Modifier::BOLD))
    };
    let verb = |verb: &str| {
        Span::styled(
            format!(" {}   ", verb),
            Style::default().fg(Color::DarkGray),
        )
    };
    let mut spans = vec![Span::raw("  "), key("[enter]", Color::Green), verb(enter)];
    if let Some(tab) = tab {
        spans.push(key("[tab]", Color::White));
        spans.push(verb(tab));
    }
    spans.push(key("[esc]", Color::White));
    spans.push(Span::styled(
        " cancel",
        Style::default().fg(Color::DarkGray),
    ));
    Line::from(spans)
}

/// A single-line editor in `popup`: `title` on the top border, an italic
/// `hint` on the bottom one, then the `prefix`ed `input` and the `footer`.
pub(super) fn render_line_input(
    frame: &mut Frame,
    popup: Rect,
    title: &str,
    hint: &str,
    prefix: Span<'_>,
    input: String,
    footer: Line<'_>,
) {
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        hint,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            prefix,
            Span::styled(
                input,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        footer,
    ];

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
