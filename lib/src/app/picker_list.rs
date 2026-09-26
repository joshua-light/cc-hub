//! The fuzzy-filtered list shared by the model, task-link and session-finder
//! pickers: one row type, one ranking, one cursor clamp.

use crate::fuzzy::fuzzy_match;

/// One visible picker row: the index of its choice plus the character
/// indices highlighted in the label or detail. At most one side is
/// highlighted: the one whose match won the row its score.
#[derive(Clone, Debug, Default)]
pub struct PickerRow {
    pub choice: usize,
    pub label_indices: Vec<usize>,
    pub detail_indices: Vec<usize>,
}

/// The searchable text of one choice. `id` is matched but never displayed,
/// so a row it wins highlights nothing.
pub(super) struct Searchable<'a> {
    pub label: &'a str,
    pub detail: &'a str,
    pub id: Option<&'a str>,
}

/// Rank `choices` against `filter`. An empty filter keeps every choice in
/// order. Otherwise a label match scores double, choices matching nothing
/// drop out, and rows sort by score descending, ties by choice order.
pub(super) fn rank_rows<'a>(
    filter: &str,
    choices: impl Iterator<Item = Searchable<'a>>,
) -> Vec<PickerRow> {
    if filter.is_empty() {
        return choices
            .enumerate()
            .map(|(choice, _)| PickerRow {
                choice,
                ..Default::default()
            })
            .collect();
    }
    let mut scored = Vec::new();
    for (choice, item) in choices.enumerate() {
        let label_match = fuzzy_match(filter, item.label);
        let detail_match = fuzzy_match(filter, item.detail);
        let id_score = item
            .id
            .and_then(|id| fuzzy_match(filter, id))
            .map(|m| m.score);
        let label_score = label_match.as_ref().map(|m| m.score * 2);
        let detail_score = detail_match.as_ref().map(|m| m.score);
        let Some(score) = label_score.max(detail_score).max(id_score) else {
            continue;
        };
        let row = if label_score == Some(score) {
            PickerRow {
                choice,
                label_indices: label_match.map(|m| m.indices).unwrap_or_default(),
                detail_indices: Vec::new(),
            }
        } else if detail_score == Some(score) {
            PickerRow {
                choice,
                label_indices: Vec::new(),
                detail_indices: detail_match.map(|m| m.indices).unwrap_or_default(),
            }
        } else {
            PickerRow {
                choice,
                ..Default::default()
            }
        };
        scored.push((score, row));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.choice.cmp(&b.1.choice)));
    scored.into_iter().map(|(_, row)| row).collect()
}

/// Move a list cursor by `delta`, clamped to `0..len` (to 0 when empty).
pub(super) fn step(selected: usize, delta: isize, len: usize) -> usize {
    selected
        .saturating_add_signed(delta)
        .min(len.saturating_sub(1))
}
