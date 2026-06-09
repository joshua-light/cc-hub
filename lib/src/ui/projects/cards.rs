//! Per-artifact card-body renderers for the result popup: diff, video, url,
//! fallback, image placeholder, and the image-decode cache helper.

use crate::app::App;
use crate::orchestrator::Artifact;
use crate::ui::palette::FAINT_PURPLE_GRAY;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::path::Path;

use super::{build_diff_lines, read_text_excerpt, truncated_footer};

pub(crate) fn render_diff_card_body(
    frame: &mut Frame,
    area: Rect,
    a: &Artifact,
    max_bytes: usize,
    scroll_lines: usize,
) {
    if area.height == 0 {
        return;
    }
    let path = Path::new(&a.path);
    let lines: Vec<Line<'static>> = match read_text_excerpt(path, max_bytes) {
        None => match std::fs::metadata(path) {
            Ok(_) => vec![Line::from(Span::styled(
                "  (binary file — open externally with `o`)",
                Style::default().fg(Color::DarkGray),
            ))],
            Err(_) => vec![Line::from(Span::styled(
                format!("  (cannot read {})", path.display()),
                Style::default().fg(Color::Rgb(220, 100, 100)),
            ))],
        },
        Some((content, truncated)) => {
            let all_rows = build_diff_lines(content, area.width);
            let start = scroll_lines.min(all_rows.len());
            let hidden_below =
                all_rows.len().saturating_sub(start + area.height as usize) + truncated;
            let body_rows =
                (area.height as usize).saturating_sub(if hidden_below > 0 { 1 } else { 0 });
            let end = (start + body_rows).min(all_rows.len());
            let mut visible = all_rows[start..end].to_vec();
            if hidden_below > 0 {
                visible.push(truncated_footer(hidden_below));
            }
            visible
        }
    };
    let p = Paragraph::new(lines);
    frame.render_widget(p, area);
}

pub(crate) fn render_video_card_body(frame: &mut Frame, area: Rect, a: &Artifact) {
    if area.height == 0 {
        return;
    }
    let basename = Path::new(&a.path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.path.clone());
    let lines = vec![
        Line::from(Span::styled(
            "  ▶ press `o` to play in external player",
            Style::default().fg(Color::LightCyan),
        )),
        Line::from(Span::styled(
            format!("  {}", basename),
            Style::default().fg(FAINT_PURPLE_GRAY),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

pub(crate) fn render_url_card_body(frame: &mut Frame, area: Rect, a: &Artifact) {
    if area.height == 0 {
        return;
    }
    let lines = vec![
        Line::from(Span::styled(
            format!("  {}", a.path),
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::UNDERLINED),
        )),
        Line::from(Span::styled(
            "  press `o` to open in browser",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

pub(crate) fn render_fallback_card_body(frame: &mut Frame, area: Rect, a: &Artifact) {
    if area.height == 0 {
        return;
    }
    let basename = Path::new(&a.path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.path.clone());
    let line = Line::from(Span::styled(
        format!("  {}", basename),
        Style::default().fg(FAINT_PURPLE_GRAY),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

pub(crate) fn render_image_placeholder(frame: &mut Frame, area: Rect, msg: &str) {
    if area.height == 0 {
        return;
    }
    let line = Line::from(Span::styled(
        format!("  {}", msg),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

/// Decode `path` into the per-app image cache, using the picker's protocol
/// detection. Returns `true` when the entry is now present (either freshly
/// decoded or already cached); `false` on decode failure (recorded so we
/// don't retry on every frame).
pub(crate) fn ensure_image_decoded(app: &mut App, path: &str) -> bool {
    if app.artifact_images.contains_key(path) {
        return true;
    }
    if app.artifact_image_failed.contains(path) {
        return false;
    }
    let Some(picker) = app.image_picker.as_ref() else {
        app.artifact_image_failed.insert(path.to_string());
        return false;
    };
    let img = match image::ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.decode().ok())
    {
        Some(i) => i,
        None => {
            app.artifact_image_failed.insert(path.to_string());
            return false;
        }
    };
    let proto = picker.new_resize_protocol(img);
    app.artifact_images.insert(path.to_string(), proto);
    true
}
