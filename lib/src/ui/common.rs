//! Cross-tab UI helpers: popup/centering geometry, the title-bar usage line,
//! state colours/indicators, and the truncation / time / token / cost
//! formatters shared by the session grid, kanban cards, and metrics tables.

use crate::models;
use crate::models::SessionState;
use crate::ui::palette::{GRAY_80, MUTED_TEXT, SEP_GRAY};
use crate::usage::UsageInfo;
use chrono::{DateTime, Local, TimeZone};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};

pub(crate) fn popup_block<'a>(title: impl Into<ratatui::text::Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::White))
        .title(title)
}

pub(crate) fn centered_rect(area: Rect, ratio: f32) -> Rect {
    let w = (area.width as f32 * ratio) as u16;
    let h = (area.height as f32 * ratio) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

pub(crate) fn centered_fixed(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

pub fn build_usage_line(u: &UsageInfo) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    let label_style = Style::default().fg(Color::DarkGray);
    let reset_style = Style::default().fg(Color::Rgb(90, 90, 100));
    let sep_style = Style::default().fg(SEP_GRAY);
    let pct_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    spans.push(Span::styled(" 5h", label_style));
    if let Some(fmt) = u
        .five_hour_resets_at
        .as_deref()
        .and_then(|s| format_reset(s, "%-l%p"))
    {
        spans.push(Span::styled(format!(" {}", fmt), reset_style));
    }
    spans.push(Span::raw(" "));
    append_bar(&mut spans, u.five_hour_pct, 10);
    spans.push(Span::styled(format!(" {}%", u.five_hour_pct), pct_style));

    spans.push(Span::styled(" │ ", sep_style));

    spans.push(Span::styled("wk", label_style));
    if let Some(fmt) = u
        .seven_day_resets_at
        .as_deref()
        .and_then(|s| format_reset(s, "%a %-l%p"))
    {
        spans.push(Span::styled(format!(" {}", fmt), reset_style));
    }
    spans.push(Span::raw(" "));
    append_bar(&mut spans, u.seven_day_pct, 10);
    spans.push(Span::styled(format!(" {}% ", u.seven_day_pct), pct_style));

    Line::from(spans)
}

pub(crate) fn append_bar(spans: &mut Vec<Span<'static>>, pct: u8, width: u16) {
    let pct = pct.min(100);
    let mut filled = (pct as u16 * width) / 100;
    if pct > 0 && filled == 0 {
        filled = 1;
    }
    let empty = width - filled;
    let color = bar_color(pct);
    let filled_s: String = "━".repeat(filled as usize);
    let empty_s: String = "╌".repeat(empty as usize);
    spans.push(Span::styled(filled_s, Style::default().fg(color)));
    spans.push(Span::styled(empty_s, Style::default().fg(color)));
}

pub(crate) fn bar_color(pct: u8) -> Color {
    if pct > 80 {
        Color::Red
    } else if pct >= 50 {
        Color::Yellow
    } else {
        Color::Green
    }
}

pub(crate) fn format_reset(iso: &str, fmt: &str) -> Option<String> {
    let dt = DateTime::parse_from_rfc3339(iso).ok()?;
    Some(
        dt.with_timezone(&Local)
            .format(fmt)
            .to_string()
            .to_lowercase(),
    )
}

pub(crate) fn state_indicator(state: &SessionState) -> (&'static str, Color) {
    match state {
        SessionState::Processing => ("󰒓", Color::Green),
        SessionState::WaitingForInput => ("󰂞", Color::Yellow),
        SessionState::Question => ("󰋗", Color::LightBlue),
        SessionState::Idle => ("󰒲", MUTED_TEXT),
        SessionState::Inactive => ("󰜎", GRAY_80),
    }
}

pub(crate) fn state_color(state: &SessionState) -> Color {
    state_indicator(state).1
}

pub(crate) fn short_model(model: &str) -> &str {
    model.strip_prefix("claude-").unwrap_or(model)
}

/// Tool names for the card HUD: strip MCP-server prefixes and cap at 18 chars
/// so long names like `mcp__claude_ai_Notion__notion-search` fit in narrow
/// cards.
pub(crate) fn short_tool(tool: &str) -> String {
    // `mcp__<server>__<name>` → just the name (the leaf is what's distinctive).
    let leaf = crate::models::mcp_leaf(tool);
    let chars: Vec<char> = leaf.chars().collect();
    if chars.len() <= 18 {
        return leaf.to_string();
    }
    let mut s: String = chars.into_iter().take(17).collect();
    s.push('…');
    s
}

/// Render the in-flight tool as `󰖷 Bash: cargo build` when a hint is
/// available, or just `󰖷 Bash` otherwise. The hint is truncated so the
/// whole label fits on a card activity row `inner_w` columns wide.
pub(crate) fn format_tool_label(tool: &crate::conversation::CurrentTool, inner_w: usize) -> String {
    let name = short_tool(&tool.name);
    let Some(hint) = tool.hint.as_deref().filter(|h| !h.is_empty()) else {
        return format!("󰖷 {}", name);
    };
    // Reserve: icon (2) + space (1) + name + ": " (2).
    let prefix_cols = 2 + 1 + name.chars().count() + 2;
    let budget = inner_w.saturating_sub(prefix_cols);
    let hint_short = models::first_line_truncated(hint, budget.max(6));
    format!("󰖷 {}: {}", name, hint_short)
}

/// Effective context-window size in tokens. The JSONL `model` field is the
/// bare id (`claude-opus-4-7`) and never carries the `[1m]` / `-1m` suffix
/// even when a session is running on the 1M-context variant, so we infer:
/// Opus 4.7+ defaults to 1M; explicit `[1m]` / `-1m` markers force 1M; all
/// other models fall back to the standard 200k window.
pub(crate) fn context_window_size(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.contains("[1m]") || m.contains("-1m") || m.contains("opus-4-7") {
        1_000_000
    } else {
        200_000
    }
}

/// Color ramp for context utilization: green → yellow → orange → red.
pub(crate) fn ctx_color(pct: u8) -> Color {
    if pct >= 90 {
        Color::Rgb(220, 120, 120)
    } else if pct >= 70 {
        Color::Rgb(220, 200, 120)
    } else if pct >= 40 {
        Color::Rgb(180, 200, 140)
    } else {
        Color::Rgb(120, 180, 200)
    }
}

/// Build a unicode bar of `width` columns filled to `pct` (0..=100), drawn
/// with thin line glyphs (`━━━╌╌╌`) to match the title-bar usage bars.
pub(crate) fn ctx_bar(pct: u8, width: usize) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let pct = pct.min(100) as usize;
    let filled = (pct * width + 50) / 100; // round to nearest column
    let color = ctx_color(pct as u8);
    let mut out = Vec::with_capacity(2);
    out.push(Span::styled(
        "━".repeat(filled),
        Style::default().fg(color),
    ));
    if filled < width {
        out.push(Span::styled(
            "╌".repeat(width - filled),
            Style::default().fg(Color::Rgb(50, 50, 65)),
        ));
    }
    out
}

pub(crate) fn format_time(timestamp_ms: u64) -> String {
    let secs = (timestamp_ms / 1000) as i64;
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%l:%M %p").to_string(),
        _ => "??:??".to_string(),
    }
}

pub(crate) fn format_datetime(timestamp_ms: u64) -> String {
    let secs = (timestamp_ms / 1000) as i64;
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%b %d %l:%M %p").to_string(),
        _ => "unknown".to_string(),
    }
}

pub(crate) fn format_elapsed(now: u64, from_ms: u64) -> String {
    let secs = now.saturating_sub(from_ms) / 1000;
    format_duration_secs(secs)
}

pub(crate) fn format_duration_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{}h", h)
        } else {
            format!("{}h {}m", h, m)
        }
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        if h == 0 {
            format!("{}d", d)
        } else {
            format!("{}d {}h", d, h)
        }
    }
}

pub(crate) fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        format!("{}", count)
    }
}

pub(crate) fn fmt_cost(c: f64) -> String {
    if c >= 100.0 {
        format!("${:.0}", c)
    } else if c >= 10.0 {
        format!("${:.1}", c)
    } else {
        format!("${:.2}", c)
    }
}

#[cfg(test)]
pub(crate) fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}
