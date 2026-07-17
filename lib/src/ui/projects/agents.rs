//! Agent-state aggregation and the small kanban-card spans built from it:
//! agent dot strips, merge-progress glyphs, ctx bars, PR badges, todo counts.

use crate::models::{SessionInfo, SessionState};
use crate::ui::common::context_window_size;
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT, DOT_IDLE, FAINT_TEXT, LABEL_GRAY, META_GRAY};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Aggregate live agent counters for a task across orchestrator + workers.
pub(crate) struct AgentSummary {
    alive: u32,
    processing: u32,
    pub(super) waiting: u32,
    idle: u32,
    inactive: u32,
    total: u32,
    total_ctx: u64,
    max_ctx: u64,
    /// Worst utilization across alive agents (0..=100).
    pub(super) max_ctx_pct: u8,
    pub(super) current_tool: Option<(String, Option<String>)>,
    pub(super) is_thinking: bool,
    pub(super) tool_uses: u64,
}

pub(crate) fn collect_agent_summary(
    t: &crate::orchestrator::TaskState,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
) -> AgentSummary {
    let orch = t
        .orchestrator_tmux
        .as_deref()
        .and_then(|n| sessions_by_tmux.get(n).copied());
    let workers: Vec<Option<&SessionInfo>> = t
        .workers
        .iter()
        .map(|w| sessions_by_tmux.get(w.tmux_name.as_str()).copied())
        .collect();

    let mut sum = AgentSummary {
        alive: 0,
        processing: 0,
        waiting: 0,
        idle: 0,
        inactive: 0,
        total: 0,
        total_ctx: 0,
        max_ctx: 0,
        max_ctx_pct: 0,
        current_tool: None,
        is_thinking: false,
        tool_uses: 0,
    };

    let mut tool_priority = 0u8; // prefer Processing > WaitingForInput tools
    for s in std::iter::once(orch)
        .chain(workers.iter().copied())
        .flatten()
    {
        sum.total += 1;
        match s.state {
            SessionState::Processing => {
                sum.processing += 1;
                sum.alive += 1;
            }
            // Question is the AskUserQuestion form of waiting — projects view
            // doesn't distinguish, so it rolls up into the waiting bucket.
            SessionState::WaitingForInput | SessionState::Question => {
                sum.waiting += 1;
                sum.alive += 1;
            }
            // Starting is app-synthesized for spawn placeholders and never
            // reaches the orch/worker summary; bucketed with Idle (it shares
            // the idle liveness rank) for exhaustiveness.
            SessionState::Starting | SessionState::Idle => {
                sum.idle += 1;
                sum.alive += 1;
            }
            SessionState::Inactive => {
                sum.inactive += 1;
            }
        }
        if let Some(c) = s.context_tokens {
            sum.total_ctx = sum.total_ctx.saturating_add(c);
            if c > sum.max_ctx {
                sum.max_ctx = c;
            }
            let cap = context_window_size(s.model.as_deref().unwrap_or("")).max(1);
            let pct = ((c.saturating_mul(100)) / cap).min(100) as u8;
            if pct > sum.max_ctx_pct {
                sum.max_ctx_pct = pct;
            }
        }
        sum.tool_uses = sum.tool_uses.saturating_add(s.tool_uses_count);
        let pri = match s.state {
            SessionState::Processing => 3,
            SessionState::WaitingForInput | SessionState::Question => 2,
            SessionState::Starting | SessionState::Idle => 1,
            SessionState::Inactive => 0,
        };
        if pri > tool_priority {
            if let Some(tool) = &s.current_tool {
                sum.current_tool = Some((tool.name.clone(), tool.hint.clone()));
                tool_priority = pri;
            } else if s.is_thinking {
                sum.is_thinking = true;
                tool_priority = pri;
            }
        }
    }
    sum
}

/// Cheap variant of [`collect_agent_summary`] that only sums tool-use counts
/// across the orchestrator + workers. Used by the collapsed card renderer,
/// which needs the count for its footer badge but none of the state /
/// context-window aggregates.
pub(crate) fn sum_tool_uses(
    t: &crate::orchestrator::TaskState,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
) -> u64 {
    let orch = t
        .orchestrator_tmux
        .as_deref()
        .and_then(|n| sessions_by_tmux.get(n).copied());
    let workers = t
        .workers
        .iter()
        .filter_map(|w| sessions_by_tmux.get(w.tmux_name.as_str()).copied());
    std::iter::once(orch)
        .flatten()
        .chain(workers)
        .map(|s| s.tool_uses_count)
        .fold(0u64, |a, b| a.saturating_add(b))
}

/// Compact dot strip showing per-agent state. Up to ~12 dots; overflow
/// shows `+N`. Color: green=processing, yellow=waiting, gray=idle, dim=inactive.
pub(crate) fn agent_dot_strip(sum: &AgentSummary) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let total = sum.total as usize;
    if total == 0 {
        return spans;
    }
    let max_dots = 12usize;
    let shown = total.min(max_dots);
    // We want a stable ordering: processing → waiting → idle → inactive.
    let mut buckets = [
        (sum.processing, Color::LightGreen, "▶"),
        (sum.waiting, Color::LightYellow, "●"),
        (sum.idle, DOT_IDLE, "○"),
        (sum.inactive, Color::Rgb(80, 80, 95), "·"),
    ];
    let mut left = shown;
    for (count, color, glyph) in buckets.iter_mut() {
        let take = (*count as usize).min(left);
        for _ in 0..take {
            spans.push(Span::styled(
                (*glyph).to_string(),
                Style::default().fg(*color),
            ));
        }
        left -= take;
        if left == 0 {
            break;
        }
    }
    if total > max_dots {
        spans.push(Span::styled(
            format!(" +{}", total - max_dots),
            Style::default().fg(DOT_IDLE),
        ));
    }
    spans
}

pub(crate) fn worker_was_merged(
    w: &crate::orchestrator::Worker,
    t: &crate::orchestrator::TaskState,
) -> bool {
    t.merges.iter().any(|m| {
        w.worktree.as_deref().is_some_and(|wn| m.worktree == wn)
            && matches!(m.outcome, crate::orchestrator::MergeOutcome::Ok)
    })
}

/// Merge progress glyph: `▰` per merged worker, `▱` per pending. Caps at
/// 8 segments, with a numeric tail for overflow.
pub(crate) fn merge_progress_spans(t: &crate::orchestrator::TaskState) -> Vec<Span<'static>> {
    let total = t.workers.len();
    if total == 0 {
        return vec![Span::styled(
            "merges —".to_string(),
            Style::default().fg(DIM_TEXT),
        )];
    }
    let merged = t.workers.iter().filter(|w| worker_was_merged(w, t)).count();
    let cap = 8usize;
    let shown = total.min(cap);
    let merged_shown = (merged.min(total) * shown + total / 2) / total;
    let mut spans = Vec::with_capacity(shown + 2);
    spans.push(Span::styled("merges ", Style::default().fg(LABEL_GRAY)));
    for i in 0..shown {
        if i < merged_shown {
            spans.push(Span::styled("▰", Style::default().fg(Color::LightGreen)));
        } else {
            spans.push(Span::styled(
                "▱",
                Style::default().fg(Color::Rgb(90, 90, 110)),
            ));
        }
    }
    spans.push(Span::styled(
        format!(" {}/{}", merged, total),
        Style::default().fg(LABEL_GRAY),
    ));
    spans
}

// Moved to ui::common (the sessions grid renders ctx bars too); re-exported
// here so the `ui::projects::ctx_bar` paths keep resolving.
pub(crate) use crate::ui::common::{ctx_bar, ctx_color};

/// Compact PR status badge for kanban cards. Surfaces the bits a reviewer
/// needs to triage at-a-glance — PR id, review state, comment count — so
/// the orchestrator's iterate-on-feedback loop is visible without opening
/// the PR-details popup. Colour weights:
/// * `changes_requested` is loud (the orchestrator needs attention).
/// * `open` is calm; comments perk it up a notch.
/// * `approved` is positive green.
/// * `merged` / `closed` are muted — terminal states.
pub(crate) fn pr_badge_spans(pr: &crate::projects_scan::PrCardSummary) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    spans.push(Span::styled(
        format!("󰊢 PR #{}", pr.id),
        Style::default().fg(Color::Rgb(150, 170, 200)),
    ));
    match pr.review_state {
        crate::pr::ReviewState::ChangesRequested => {
            spans.push(Span::styled(
                " · changes requested".to_string(),
                Style::default()
                    .fg(Color::Rgb(230, 150, 110))
                    .add_modifier(Modifier::BOLD),
            ));
            if pr.comments > 0 {
                spans.push(Span::styled(
                    format!(" · 󰭹 {}", pr.comments),
                    Style::default().fg(FAINT_TEXT),
                ));
            }
        }
        crate::pr::ReviewState::Open => {
            if pr.comments > 0 {
                spans.push(Span::styled(
                    format!(" · 󰭹 {}", pr.comments),
                    Style::default().fg(ACCENT_BLUE),
                ));
            } else {
                spans.push(Span::styled(
                    " · open".to_string(),
                    Style::default().fg(Color::Rgb(150, 170, 200)),
                ));
            }
        }
        crate::pr::ReviewState::Approved => {
            spans.push(Span::styled(
                " · approved".to_string(),
                Style::default().fg(Color::LightGreen),
            ));
            if pr.comments > 0 {
                spans.push(Span::styled(
                    format!(" · 󰭹 {}", pr.comments),
                    Style::default().fg(FAINT_TEXT),
                ));
            }
        }
        crate::pr::ReviewState::Merged => {
            spans.push(Span::styled(
                " · merged".to_string(),
                Style::default().fg(Color::Rgb(140, 160, 145)),
            ));
        }
        crate::pr::ReviewState::Closed => {
            spans.push(Span::styled(
                " · closed".to_string(),
                Style::default().fg(META_GRAY),
            ));
        }
    }
    spans
}

/// `(done, total)` if the task has a checklist, else `None`. Both card
/// renderers use this to decide whether to draw the `☑ M/N` badge.
pub(crate) fn todos_progress(t: &crate::orchestrator::TaskState) -> Option<(usize, usize)> {
    if t.todos.is_empty() {
        return None;
    }
    let done = t.todos.iter().filter(|i| i.done).count();
    Some((done, t.todos.len()))
}
