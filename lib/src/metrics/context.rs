use super::parse::{AssistantCall, ParsedSession};
use super::{ContextGrowthAnalysis, ContextGrowthFinding, PeakContextFinding};
use crate::config::MetricsConfig;

/// Add `s`'s peak-context finding to `peak` and, when it scores as a token
/// spike, its growth finding to `growth`. `owned` is the session's calls after
/// cross-file dedup, so a resumed file's copied prefix isn't re-analyzed.
pub(super) fn context_findings(
    s: &ParsedSession,
    owned: &[&AssistantCall],
    session_cost: f64,
    cfg: &MetricsConfig,
    peak: &mut Vec<PeakContextFinding>,
    growth: &mut ContextGrowthAnalysis,
) {
    // Build the per-turn context series once; both peak-context and
    // growth-scoring read from it. Sessions with zero timestamped calls
    // contribute nothing to either.
    let mut series_calls: Vec<&AssistantCall> = owned
        .iter()
        .copied()
        .filter(|c| c.timestamp_ms > 0)
        .collect();
    series_calls.sort_by_key(|c| c.timestamp_ms);
    let series: Vec<u64> = series_calls
        .iter()
        .map(|c| c.tokens.input + c.tokens.cache_read + c.tokens.cache_creation)
        .collect();

    if let Some((peak_idx, &peak_ctx)) = series.iter().enumerate().max_by_key(|(_, v)| **v) {
        if peak_ctx > 0 {
            let peak_ts = series_calls
                .get(peak_idx)
                .map(|c| c.timestamp_ms)
                .unwrap_or(0);
            peak.push(PeakContextFinding {
                session_id: s.session_id.clone(),
                project: s.project.clone(),
                cwd: s.cwd.clone(),
                jsonl_path: s.jsonl_path.clone(),
                peak_ctx_tokens: peak_ctx,
                peak_turn_index: peak_idx + 1,
                peak_timestamp_ms: peak_ts,
                assistant_turns: series.len(),
                total_cost: session_cost,
            });
        }
    }

    // Token-spike scoring — short sessions skip the scoring entirely.
    if series.len() >= cfg.min_growth_turns {
        growth.sessions_scored += 1;
        if let Some((score, peak_delta, peak_idx)) = score_growth(&series) {
            if score >= cfg.growth_threshold && peak_delta > 0 {
                let peak_ts = series_calls
                    .get(peak_idx)
                    .map(|c| c.timestamp_ms)
                    .unwrap_or(0);
                growth.findings.push(ContextGrowthFinding {
                    session_id: s.session_id.clone(),
                    project: s.project.clone(),
                    cwd: s.cwd.clone(),
                    jsonl_path: s.jsonl_path.clone(),
                    score,
                    total_cost: session_cost,
                    peak_delta_tokens: peak_delta,
                    // 1-based to match the display convention (`@ turn N/M`)
                    // and the peak-context finding; peak_ts already points
                    // at this same turn (series_calls[peak_idx]).
                    peak_turn_index: peak_idx + 1,
                    peak_timestamp_ms: peak_ts,
                    assistant_turns: series.len(),
                });
                growth.anomalous_cost += session_cost;
            }
        }
    }
}

/// Score a per-turn context-size series via `max(delta) / median(|delta|)`.
///
/// Returns `Some((score, peak_delta_tokens, peak_turn_index))` when there is
/// a positive peak, otherwise `None`. The ratio is unitless and
/// self-calibrating: a well-behaved series scores near 1, while a single
/// dramatic spike drives it well above the threshold.
fn score_growth(series: &[u64]) -> Option<(f64, u64, usize)> {
    if series.len() < 2 {
        return None;
    }
    let mut peak: i64 = i64::MIN;
    let mut peak_idx: usize = 0;
    let mut abs_sorted: Vec<u64> = Vec::with_capacity(series.len() - 1);
    for i in 1..series.len() {
        let d = series[i] as i64 - series[i - 1] as i64;
        if d > peak {
            peak = d;
            peak_idx = i;
        }
        abs_sorted.push(d.unsigned_abs());
    }
    if peak <= 0 {
        return None;
    }
    abs_sorted.sort_unstable();
    let n = abs_sorted.len();
    let median_abs: f64 = if n % 2 == 1 {
        abs_sorted[n / 2] as f64
    } else {
        (abs_sorted[n / 2 - 1] as f64 + abs_sorted[n / 2] as f64) / 2.0
    };
    // Floor a near-zero median at 1 — a single jump on an otherwise flat
    // session is itself the anomaly we want to surface.
    let denom = median_abs.max(1.0);
    Some((peak as f64 / denom, peak as u64, peak_idx))
}
