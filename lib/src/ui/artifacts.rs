//! Card-body helpers for a task's notes and attached files (the Task Info
//! popup on the Tasks tab).

use crate::task_store::Artifact;
use crate::ui::palette::FAINT_TEXT;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;

/// What the popup should render for an artifact. Path/kind hints determine
/// whether we inline an image, an excerpt of a text/log file, or just a card
/// that links to an external resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardKind {
    Image,
    Video,
    Text,
    Diff,
    Url,
    Fallback,
}

pub(crate) fn classify_artifact(a: &Artifact) -> CardKind {
    let kind = a.kind.to_ascii_lowercase();
    let path = &a.path;
    if path.starts_with("http://") || path.starts_with("https://") || kind == "url" {
        return CardKind::Url;
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match kind.as_str() {
        "screenshot" | "image" | "photo" => return CardKind::Image,
        "video" => return CardKind::Video,
        "diff" | "patch" => return CardKind::Diff,
        "log" | "build" | "test" | "text" | "output" | "note" => return CardKind::Text,
        _ => {}
    }
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => CardKind::Image,
        "mp4" | "mov" | "webm" | "mkv" => CardKind::Video,
        "diff" | "patch" => CardKind::Diff,
        "log" | "txt" | "md" | "json" | "yaml" | "yml" => CardKind::Text,
        _ => CardKind::Fallback,
    }
}

/// Reads up to 8 KiB of `path` as lossy UTF-8 and splits into lines. Returns
/// `None` for binary files (>5 % non-text bytes in the leading 1 KiB) so the
/// caller can show "(binary file)" rather than dumping garbage at the user.
pub(crate) fn read_text_excerpt(path: &Path, max_bytes: usize) -> Option<(Vec<String>, usize)> {
    let bytes = std::fs::read(path).ok()?;
    let probe_len = bytes.len().min(1024);
    if probe_len > 0 {
        let non_text = bytes[..probe_len]
            .iter()
            .filter(|&&b| b == 0 || (b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t'))
            .count();
        if non_text * 20 > probe_len {
            return None;
        }
    }
    let take = bytes.len().min(max_bytes);
    let head = String::from_utf8_lossy(&bytes[..take]).into_owned();
    let total_lines = bytes.iter().filter(|&&b| b == b'\n').count() + 1;
    let head_lines: Vec<String> = head.lines().map(|s| s.to_string()).collect();
    let truncated = total_lines.saturating_sub(head_lines.len());
    Some((head_lines, truncated))
}

pub(crate) fn evidence_card_header(a: &Artifact, selected: bool, is_lead: bool) -> Line<'static> {
    let basename = Path::new(&a.path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.path.clone());
    let stripe = if selected { "▌ " } else { "  " };
    let stripe_color = if selected {
        Color::LightCyan
    } else {
        Color::DarkGray
    };
    let name_style = if selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let mut spans = vec![
        Span::styled(stripe, Style::default().fg(stripe_color)),
        Span::styled(a.kind.clone(), Style::default().fg(Color::LightMagenta)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(basename, name_style),
    ];
    if is_lead {
        spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            "lead",
            Style::default().fg(Color::Rgb(150, 195, 160)),
        ));
    }
    if let Some(c) = a.caption.as_deref() {
        if !c.is_empty() {
            spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(c.to_string(), Style::default().fg(FAINT_TEXT)));
        }
    }
    Line::from(spans)
}

pub(crate) fn truncated_footer(n: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "  … (truncated, {} more line{})",
            n,
            if n == 1 { "" } else { "s" }
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ))
}
