//! Diff parsing and styled diff-row rendering for the result popup's diff cards.

use crate::models;
use crate::ui::palette::CONTEXT_GRAY;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::borrow::Cow;

pub(crate) const DIFF_ADDED_BG: Color = Color::Rgb(34, 92, 43);
pub(crate) const DIFF_REMOVED_BG: Color = Color::Rgb(122, 41, 54);
pub(crate) const DIFF_ADDED_FG: Color = Color::Rgb(180, 235, 190);
pub(crate) const DIFF_REMOVED_FG: Color = Color::Rgb(245, 195, 200);
pub(crate) const DIFF_GUTTER_FG: Color = Color::Rgb(120, 120, 130);
pub(crate) const DIFF_CONTEXT_FG: Color = CONTEXT_GRAY;
pub(crate) const DIFF_HEADER_FG: Color = Color::Rgb(140, 180, 230);

// Dim metadata gray used for low-priority info on task cards (queued-merge
// border + the `#<id>` badge). Sits below every other tone in the card so the
// id reads as background context, not headline.
pub(crate) const TASK_META_DIM: Color = Color::Rgb(95, 100, 115);

// `{old:>3} {new:>3} {marker} ` + 1-cell margin = 11 used cols.
pub(crate) const DIFF_GUTTER_W: usize = 11;

#[derive(Clone, Copy)]
pub(crate) enum DiffRowKind {
    Added,
    Removed,
    Context,
}

impl DiffRowKind {
    fn marker(self) -> &'static str {
        match self {
            DiffRowKind::Added => "+",
            DiffRowKind::Removed => "-",
            DiffRowKind::Context => " ",
        }
    }
    fn palette(self) -> (Option<Color>, Color) {
        match self {
            DiffRowKind::Added => (Some(DIFF_ADDED_BG), DIFF_ADDED_FG),
            DiffRowKind::Removed => (Some(DIFF_REMOVED_BG), DIFF_REMOVED_FG),
            DiffRowKind::Context => (None, DIFF_CONTEXT_FG),
        }
    }
}

pub(crate) fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace();
    parts.next()?;
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let old_start: u32 = old.split(',').next()?.parse().ok()?;
    let new_start: u32 = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

pub(crate) fn diff_row(
    area_width: u16,
    old: Option<u32>,
    new: Option<u32>,
    content: &str,
    kind: DiffRowKind,
) -> Line<'static> {
    let (bg_opt, content_fg) = kind.palette();
    let base = match bg_opt {
        Some(b) => Style::default().bg(b),
        None => Style::default(),
    };
    let fmt_num = |n: Option<u32>| n.map_or_else(|| "   ".to_string(), |v| format!("{:>3}", v));

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
    spans.push(Span::styled(fmt_num(old), base.fg(DIFF_GUTTER_FG)));
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled(fmt_num(new), base.fg(DIFF_GUTTER_FG)));
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled(kind.marker(), base.fg(content_fg)));
    spans.push(Span::styled("  ", base));

    let avail = (area_width as usize).saturating_sub(DIFF_GUTTER_W);
    let normalized: Cow<'_, str> = if content.contains('\t') {
        Cow::Owned(content.replace('\t', "    "))
    } else {
        Cow::Borrowed(content)
    };
    let body = models::first_line_truncated(&normalized, avail);
    let body_w = body.chars().count();

    let mut content_style = base.fg(content_fg);
    if matches!(kind, DiffRowKind::Context) {
        content_style = content_style.add_modifier(Modifier::DIM);
    }
    spans.push(Span::styled(body, content_style));

    if matches!(kind, DiffRowKind::Added | DiffRowKind::Removed) {
        let pad = avail.saturating_sub(body_w);
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), base));
        }
    }
    Line::from(spans)
}

pub(crate) fn diff_separator_row() -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(DIFF_GUTTER_W.saturating_sub(3))),
        Span::styled(
            "...",
            Style::default()
                .fg(DIFF_GUTTER_FG)
                .add_modifier(Modifier::DIM),
        ),
    ])
}

pub(crate) fn is_diff_meta_line(s: &str) -> bool {
    s.starts_with("diff --git")
        || s.starts_with("index ")
        || s.starts_with("--- ")
        || s.starts_with("+++ ")
        || s.starts_with("\\ No newline")
        || s.starts_with("similarity index")
        || s.starts_with("dissimilarity index")
        || s.starts_with("rename from")
        || s.starts_with("rename to")
        || s.starts_with("copy from")
        || s.starts_with("copy to")
        || s.starts_with("new file mode")
        || s.starts_with("deleted file mode")
        || s.starts_with("old mode")
        || s.starts_with("new mode")
        || s.starts_with("Binary files")
        || s.starts_with("GIT binary patch")
}

pub(crate) fn build_diff_lines(content: Vec<String>, area_width: u16) -> Vec<Line<'static>> {
    let mut header_path: Option<String> = None;
    let mut added: u32 = 0;
    let mut removed: u32 = 0;
    let mut body_rows: Vec<Line<'static>> = Vec::new();
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;
    let mut first_hunk = true;

    for raw in content.into_iter() {
        if header_path.is_none() {
            if let Some(rest) = raw.strip_prefix("+++ b/") {
                header_path = Some(rest.to_string());
            } else if let Some(rest) = raw.strip_prefix("+++ ") {
                if !rest.is_empty() && rest != "/dev/null" {
                    header_path = Some(rest.to_string());
                }
            }
        }
        if is_diff_meta_line(&raw) {
            continue;
        }
        if raw.starts_with("@@") {
            if let Some((o, n)) = parse_hunk_header(&raw) {
                old_line = o;
                new_line = n;
            }
            if !first_hunk {
                body_rows.push(diff_separator_row());
            }
            first_hunk = false;
            continue;
        }
        let (kind, body, bump_old, bump_new) = match raw.as_bytes().first() {
            Some(b'+') => (DiffRowKind::Added, &raw[1..], 0, 1),
            Some(b'-') => (DiffRowKind::Removed, &raw[1..], 1, 0),
            Some(b' ') => (DiffRowKind::Context, &raw[1..], 1, 1),
            _ => (DiffRowKind::Context, raw.as_str(), 1, 1),
        };
        match kind {
            DiffRowKind::Added => added += 1,
            DiffRowKind::Removed => removed += 1,
            DiffRowKind::Context => {}
        }
        let (old_n, new_n) = match kind {
            DiffRowKind::Added => (None, Some(new_line)),
            DiffRowKind::Removed => (Some(old_line), None),
            DiffRowKind::Context => (Some(old_line), Some(new_line)),
        };
        body_rows.push(diff_row(area_width, old_n, new_n, body, kind));
        old_line += bump_old;
        new_line += bump_new;
    }

    let mut rows: Vec<Line<'static>> = Vec::with_capacity(body_rows.len() + 3);
    if let Some(p) = header_path {
        rows.push(Line::from(Span::styled(
            p,
            Style::default()
                .fg(DIFF_HEADER_FG)
                .add_modifier(Modifier::BOLD),
        )));
        rows.push(Line::from(Span::styled(
            format!("Added {} lines, removed {} lines", added, removed),
            Style::default()
                .fg(DIFF_GUTTER_FG)
                .add_modifier(Modifier::BOLD),
        )));
    }
    rows.extend(body_rows);
    rows
}
