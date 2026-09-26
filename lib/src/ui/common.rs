//! Helpers shared across the UI:
//!
//! - session state, task status, priority and identity colours; the
//!   spinner frames
//! - popup block, centering geometry, the text cursor, wrapped-row count and
//!   scroll-position label
//! - the title-bar usage line
//! - model/tool labels, context-window bars, and time, age, token and cost
//!   formatters
//! - one-line row helpers: padding, truncation, the selection stripe
//! - [`Cell`], one column of a table row's right-hand cluster

use crate::models;
use crate::models::first_line_truncated;
use crate::models::SessionState;
use crate::tasks::store::{TaskPriority, TaskStatus};
use crate::ui::palette::{BACKLOG_BLUE, GRAY_80, MUTED_TEXT, PURPLE, SEP_GRAY};
use crate::usage::UsageInfo;
use chrono::{DateTime, Local, TimeZone};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

/// Canonical (icon, accent color) for a task status: the Tasks-board column
/// palette. Every surface that colors a status (board columns, the task-link picker)
/// goes through here so the hues can't drift apart. The display label lives
/// on [`TaskStatus::board_label`].
pub(crate) fn task_status_meta(status: TaskStatus) -> (&'static str, Color) {
    match status {
        TaskStatus::Backlog => ("󰄱", BACKLOG_BLUE),
        TaskStatus::Planning => ("󰟶", PURPLE),
        TaskStatus::Running => ("󰒓", Color::LightYellow),
        TaskStatus::Review => ("󱋲", Color::LightCyan),
        TaskStatus::Done => ("󰸞", Color::LightGreen),
    }
}

/// Badge colour per priority: P1 red, P2 yellow, P3 green, P4 blue. Light
/// variants so the bold badge stays legible on the dark board. Shared by the
/// Tasks-board chip and the session-card task badge so the hues can't drift.
pub(crate) fn priority_color(p: TaskPriority) -> Color {
    match p {
        TaskPriority::P1 => Color::LightRed,
        TaskPriority::P2 => Color::LightYellow,
        TaskPriority::P3 => Color::LightGreen,
        TaskPriority::P4 => Color::LightBlue,
    }
}

/// Identity palette for task badges on session cards: hues picked to stay
/// apart from each other and from the state accents (yellow = waiting,
/// green = processing, blue = question) that already color card chrome.
const TASK_COLORS: [Color; 8] = [
    Color::Rgb(255, 160, 122), // salmon
    Color::Rgb(135, 206, 250), // sky blue
    Color::Rgb(221, 160, 221), // plum
    Color::Rgb(152, 251, 152), // pale green
    Color::Rgb(240, 230, 140), // khaki
    Color::Rgb(175, 238, 238), // pale turquoise
    Color::Rgb(255, 182, 193), // light pink
    Color::Rgb(222, 184, 135), // tan
];

/// Stable per-task identity color: FNV-1a over the task id into
/// [`TASK_COLORS`], so every card linked to the same task carries the same
/// hue — across groups, scans, and restarts.
pub(crate) fn task_color(task_id: &str) -> Color {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in task_id.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    TASK_COLORS[(hash % TASK_COLORS.len() as u64) as usize]
}

/// The cursor block every text field draws at its end.
pub(crate) const CURSOR: char = '▎';

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

/// Visual rows `lines` occupy when a `Paragraph` with `Wrap { trim: false }`
/// renders them into `width` columns. ratatui scrolls a wrapped paragraph by
/// wrapped rows, not logical lines, so every scroll clamp and jump target must
/// be summed in this unit or the last screenful becomes unreachable.
///
/// Delegates to ratatui's `Paragraph::line_count` (the
/// `unstable-rendered-line-info` feature) so the count is the renderer's own,
/// including word breaks, trailing spaces, tabs and wide chars.
pub(crate) fn wrapped_total_rows(lines: &[Line], width: u16) -> u16 {
    if width == 0 {
        return lines.len().min(u16::MAX as usize) as u16;
    }
    Paragraph::new(Text::from(lines.to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(lines.len().min(1))
        .min(u16::MAX as usize) as u16
}

pub fn build_usage_line(u: &UsageInfo) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    let label_style = Style::default().fg(Color::DarkGray);
    let reset_style = Style::default().fg(Color::Rgb(90, 90, 100));
    let sep_style = Style::default().fg(SEP_GRAY);
    // A reading the store could not refresh is still the best number we
    // have; it is shown, but no longer in white.
    let pct_style = if u.health == crate::usage::Health::Ready {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

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

fn append_bar(spans: &mut Vec<Span<'static>>, pct: u8, width: u16) {
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

fn bar_color(pct: u8) -> Color {
    if pct > 80 {
        Color::Red
    } else if pct >= 50 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn format_reset(iso: &str, fmt: &str) -> Option<String> {
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
        // Static fallback; both session renderers animate Starting with the
        // orbit spinner (see [`starting_frame`]). Magenta keeps
        // "booting" visually apart from the green Processing spinner.
        SessionState::Starting => ("◌", Color::Magenta),
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

/// Braille spinner for work in flight: a Processing session, a ticking agent,
/// a running build. The frame derives from wall-clock time, so it advances
/// on every repaint: at least once a second from the clock tick, faster
/// while scan events stream in. Motion is the point: a turning glyph reads
/// as alive where a static one reads as ambient.
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) fn spinner_frame(now: u64) -> &'static str {
    SPINNER_FRAMES[((now / 120) % SPINNER_FRAMES.len() as u64) as usize]
}

/// Single-dot orbit shown while a spawn placeholder is Starting. Same
/// braille family as [`SPINNER_FRAMES`] but unmistakably different motion
/// (one dot circling vs. a churning cluster), so a booting agent can't be
/// read as one that's already processing.
const STARTING_FRAMES: [&str; 8] = ["⠁", "⠂", "⠄", "⡀", "⢀", "⠠", "⠐", "⠈"];

pub(crate) fn starting_frame(now: u64) -> &'static str {
    STARTING_FRAMES[((now / 120) % STARTING_FRAMES.len() as u64) as usize]
}

/// Snowflake marking a cold prompt cache (see
/// [`crate::models::SessionInfo::cache_cold`]): the session's next turn
/// re-ingests everything anyway, so restarting it is free. Rendered in
/// [`crate::ui::palette::ICE_BLUE`] wherever the surrounding style allows.
pub(crate) const COLD_CACHE_ICON: &str = "󰜗";

pub(crate) fn short_model(model: &str) -> &str {
    model.strip_prefix("claude-").unwrap_or(model)
}

/// Tool names for the card HUD: strip MCP-server prefixes and cap at 18 chars
/// so long names like `mcp__claude_ai_Notion__notion-search` fit in narrow
/// cards.
fn short_tool(tool: &str) -> String {
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
    out.push(Span::styled("━".repeat(filled), Style::default().fg(color)));
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

/// Compact age of `at` relative to `now`, both unix seconds: `45s`, `3m`,
/// `2h`, `5d`. A timestamp in the future reads as `0s`.
pub(crate) fn age(now: i64, at: i64) -> String {
    models::relative_age_short((now - at).max(0) as u64)
}

/// Cut spans so the line never wraps.
pub(crate) fn truncate_line(line: Line<'_>, width: usize) -> Line<'_> {
    let mut used = 0;
    let mut spans = Vec::new();
    for span in line.spans {
        let w = span.content.chars().count();
        if used + w <= width {
            used += w;
            spans.push(span);
        } else {
            let room = width.saturating_sub(used);
            if room > 1 {
                let s: String = span.content.chars().take(room - 1).collect();
                spans.push(Span::styled(format!("{}…", s), span.style));
            }
            break;
        }
    }
    Line::from(spans)
}

/// `s`'s first line, truncated or right-padded to exactly `w` chars.
pub(crate) fn pad(s: &str, w: usize) -> String {
    let s = first_line_truncated(s, w);
    let n = s.chars().count();
    format!("{}{}", s, " ".repeat(w.saturating_sub(n)))
}

/// `s` left-padded to `w` chars; a longer `s` is kept whole.
pub(crate) fn pad_left(s: &str, w: usize) -> String {
    let n = s.chars().count();
    format!("{}{}", " ".repeat(w.saturating_sub(n)), s)
}

/// ` 3/40 `: the 1-based position of `scroll` among `total` rows, clamped to
/// the last row, for a scrollable view's corner.
pub(crate) fn scroll_position(scroll: u16, total: u16) -> String {
    format!(
        " {}/{} ",
        (scroll as usize).min(total.saturating_sub(1) as usize) + 1,
        total
    )
}

/// First cell of a selectable one-line row: a white bar when selected, a
/// blank otherwise, so selected and plain rows stay aligned.
pub(crate) fn selection_stripe(selected: bool) -> Span<'static> {
    if selected {
        Span::styled("▌", Style::default().fg(Color::White))
    } else {
        Span::raw(" ")
    }
}

/// Gap between two columns of a table row's right-hand cluster.
pub(crate) const COL_SEP: usize = 2;

/// One column of a table row's right-hand cluster, padded to `target`
/// advance columns so values line up vertically across rows. A blank cell
/// (`text.is_empty()`) still occupies its column — that is what keeps the
/// rows a table rather than a ragged list.
pub(crate) struct Cell {
    pub text: String,
    pub target: usize,
    pub style: Style,
    pub right_align: bool,
}

impl Cell {
    pub fn push_spans(self, spans: &mut Vec<Span<'static>>) {
        spans.push(Span::raw(" ".repeat(COL_SEP)));
        let pad = self.target.saturating_sub(self.text.chars().count());
        if self.right_align {
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(self.text, self.style));
        } else {
            spans.push(Span::styled(self.text, self.style));
            spans.push(Span::raw(" ".repeat(pad)));
        }
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

#[cfg(test)]
mod wrapped_rows_tests {
    use super::wrapped_total_rows;
    use ratatui::text::{Line, Span};

    fn rows(s: &str, w: u16) -> usize {
        wrapped_total_rows(&[Line::from(Span::raw(s.to_string()))], w) as usize
    }

    #[test]
    fn empty_and_short_lines_are_one_row() {
        assert_eq!(rows("", 10), 1);
        assert_eq!(rows("hello", 10), 1);
        assert_eq!(rows("hello", 5), 1); // exactly fills one row
    }

    #[test]
    fn breaks_on_word_boundaries() {
        assert_eq!(rows("the quick brown fox", 9), 2);
        assert_eq!(rows("hello world", 5), 2);
    }

    #[test]
    fn hard_splits_a_word_wider_than_the_row() {
        assert_eq!(rows("abcdefgh", 3), 3); // abc/def/gh
    }

    #[test]
    fn zero_width_never_divides_by_zero() {
        assert_eq!(rows("abc", 0), 1); // degenerate area; renderer shows nothing
    }

    // Shapes where a naive char-count wrap disagrees with the renderer. These
    // pin the renderer's behaviour so a reimplementation cannot drift from it.
    #[test]
    fn matches_renderer_on_divergent_shapes() {
        // Leading whitespace before an overflowing token: WordWrapper packs
        // the whitespace + word-head onto the first row.
        assert_eq!(rows(" leading", 4), 2);
        // Trailing space at exact row boundary is dropped, not wrapped.
        assert_eq!(rows("aaaa ", 4), 1);
        // Wide chars (CJK, emoji) occupy two columns each.
        assert_eq!(rows("日本語のテキスト", 8), 2);
        assert_eq!(rows("emoji 🚀🚀🚀 line", 8), 3);
    }

    #[test]
    fn spans_concatenate_and_lines_sum() {
        // Two spans concatenate into one logical line for wrap purposes.
        let wide = Line::from(vec![Span::raw("the quick "), Span::raw("brown fox")]);
        assert_eq!(wrapped_total_rows(&[wide], 9), 2);

        let lines = vec![
            Line::from(Span::raw("short")),           // 1 row
            Line::from(Span::raw("the quick brown")), // 2 rows at width 9
        ];
        assert_eq!(wrapped_total_rows(&lines, 9), 3);
    }
}

#[cfg(test)]
mod format_tests {
    use super::age;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn ages_are_compact() {
        assert_eq!(age(NOW, NOW - 5), "5s");
        assert_eq!(age(NOW, NOW - 600), "10m");
        assert_eq!(age(NOW, NOW - 7200), "2h");
        assert_eq!(age(NOW, NOW - 200_000), "2d");
        assert_eq!(age(NOW, NOW + 50), "0s");
    }
}
