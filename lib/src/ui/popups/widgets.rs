//! Building blocks shared by the popups in this module.

use ratatui::style::Style;
use ratatui::text::Span;

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

/// The cursor block both task-popup fields draw at their end.
pub(super) const CURSOR: char = '▎';

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
