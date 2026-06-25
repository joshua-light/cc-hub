//! Named colour constants for literal `Color::Rgb` values that recur across
//! the UI. Consolidating only *exact* duplicate values keeps the palette a
//! pure de-duplication of what the code already used — no RGB value is
//! altered. Names describe the role where one is obvious, otherwise the value.

use ratatui::style::Color;

/// Low-priority/dim body text and footers (the most common gray in the file).
pub(crate) const DIM_TEXT: Color = Color::Rgb(110, 110, 130);

/// Faint secondary text — captions, prompt previews, queued-card body.
pub(crate) const FAINT_TEXT: Color = Color::Rgb(180, 180, 200);

/// Muted icon/prefix gray used ahead of labels in popup/card bodies.
pub(crate) const MUTED_TEXT: Color = Color::Rgb(100, 100, 120);

/// Mid gray for inline labels (`agents`, `merges`, chip prefix).
pub(crate) const LABEL_GRAY: Color = Color::Rgb(150, 150, 170);

/// Metadata gray for card footers / queued-merge glyphs.
pub(crate) const META_GRAY: Color = Color::Rgb(110, 120, 135);

/// Dim gray used for skip verdicts, decision-tree separators, scroll info.
pub(crate) const GRAY_80: Color = Color::Rgb(80, 80, 90);

/// Separator gray for `│` dividers and unfocused card borders.
pub(crate) const SEP_GRAY: Color = Color::Rgb(60, 60, 70);

/// Diff/context and tail-entry gray.
pub(crate) const CONTEXT_GRAY: Color = Color::Rgb(160, 160, 170);

/// Soft blue accent (cwd path, note icon, selected to-do marker).
pub(crate) const ACCENT_BLUE: Color = Color::Rgb(180, 200, 230);

/// Planning/thinking purple accent.
pub(crate) const PURPLE: Color = Color::Rgb(170, 140, 210);

/// Faint purple-tinged gray for fallback/video bodies and artifact counts.
pub(crate) const FAINT_PURPLE_GRAY: Color = Color::Rgb(160, 160, 180);

/// Idle/overflow dot and narrow-board hint gray.
pub(crate) const DOT_IDLE: Color = Color::Rgb(140, 140, 160);

/// Backlog accent blue (chip backlog count, backlog popup arrows/title).
pub(crate) const BACKLOG_BLUE: Color = Color::Rgb(120, 140, 200);

/// Muted slate for task `#tag` badges — distinct from the priority hues and
/// column accents, reads as secondary metadata on the card border.
pub(crate) const TAG_SLATE: Color = Color::Rgb(150, 170, 200);
