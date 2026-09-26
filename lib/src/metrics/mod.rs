//! Usage analytics for Claude Code sessions.
//!
//! Walks `~/.claude/projects/<encoded-cwd>/*.jsonl` (and subagent JSONLs
//! under `<session-uuid>/subagents/`), parses token usage from each
//! `assistant` line, and aggregates cost/tokens by model, project, day,
//! and session.
//!
//! Dedup mirrors cc-metrics: Claude Code writes one JSONL line per content
//! block, all sharing a `requestId` and cumulative `usage`. We keep one
//! entry per `requestId`, redirecting via `message.id` when two
//! `requestId`s share the same canonical API response.

mod analyze;
mod context;
mod cost;
mod discover;
mod parse;
mod task_usage;

pub use analyze::{analyze, analyze_with_progress};
pub use cost::{ModelPricing, Tokens};
pub(crate) use task_usage::{task_usage, task_usage_files};

use chrono::NaiveDate;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

#[derive(Default, Clone, Debug)]
pub struct ModelStats {
    pub cost: f64,
    pub tokens: Tokens,
    pub sessions: usize,
    pub messages: usize,
}

#[derive(Default, Clone, Debug)]
pub struct ProjectStats {
    pub cost: f64,
    pub tokens: Tokens,
    pub sessions: usize,
    pub messages: usize,
}

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub session_id: String,
    pub project: String,
    pub cwd: String,
    pub jsonl_path: PathBuf,
    pub model: String,
    pub cost: f64,
    pub tokens: Tokens,
    pub message_count: usize,
    pub end_time_ms: u64,
    pub is_subagent: bool,
}

#[derive(Default, Clone, Debug)]
pub struct DayStats {
    pub cost: f64,
}

#[derive(Default, Clone, Debug)]
pub struct ToolStats {
    pub count: u64,
    pub sessions: usize,
}

#[derive(Default, Clone, Debug)]
pub struct SessionInterruption {
    pub session_id: String,
    pub project: String,
    pub cwd: String,
    pub jsonl_path: PathBuf,
    pub orphan_count: usize,
    pub wasted_cost: f64,
    pub last_tool_name: String,
}

#[derive(Default, Clone, Debug)]
pub struct InterruptionAnalysis {
    pub total_interrupted_turns: usize,
    pub total_wasted_cost: f64,
    pub sessions_affected: usize,
    pub by_session: Vec<SessionInterruption>,
}

#[derive(Clone, Debug)]
pub struct ContextGrowthFinding {
    pub session_id: String,
    pub project: String,
    pub cwd: String,
    pub jsonl_path: PathBuf,
    pub score: f64,
    pub total_cost: f64,
    pub peak_delta_tokens: u64,
    pub peak_turn_index: usize,
    pub peak_timestamp_ms: u64,
    pub assistant_turns: usize,
}

#[derive(Default, Clone, Debug)]
pub struct ContextGrowthAnalysis {
    pub sessions_scored: usize,
    pub anomalous_cost: f64,
    pub findings: Vec<ContextGrowthFinding>,
}

/// Per-session record of the largest absolute context size reached,
/// i.e. max(input + cache_read + cache_creation) across assistant calls.
/// This is the direct answer to "how big did this session's context get",
/// independent of how the growth was shaped.
#[derive(Clone, Debug)]
pub struct PeakContextFinding {
    pub session_id: String,
    pub project: String,
    pub cwd: String,
    pub jsonl_path: PathBuf,
    pub peak_ctx_tokens: u64,
    pub peak_turn_index: usize,
    pub peak_timestamp_ms: u64,
    pub assistant_turns: usize,
    pub total_cost: f64,
}

#[derive(Default, Clone, Debug)]
pub struct PeakContextAnalysis {
    pub findings: Vec<PeakContextFinding>,
}

/// A session reference surfaced in the Metrics tab that the user can
/// select and resume. The flat index used by the UI walks the analysis
/// in the order shown on screen: Top sessions → Interruptions →
/// Context-growth findings.
#[derive(Clone, Debug)]
pub struct SelectableSession {
    pub session_id: String,
    pub cwd: String,
    pub project: String,
    pub jsonl_path: PathBuf,
    /// Timestamp (ms) of the assistant turn worth highlighting when the
    /// transcript opens — currently only populated for context-growth
    /// findings, where it marks the peak-delta turn.
    pub peak_timestamp_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct MetricsAnalysis {
    pub total_cost: f64,
    pub total_sessions: usize,
    pub total_messages: usize,
    pub total_tokens: Tokens,
    pub cache_hit_rate: f64,
    pub by_model: BTreeMap<String, ModelStats>,
    pub by_project: HashMap<String, ProjectStats>,
    pub by_day: BTreeMap<NaiveDate, DayStats>,
    pub top_sessions: Vec<SessionSummary>,
    pub top_projects: Vec<(String, ProjectStats)>,
    pub by_tool: BTreeMap<String, ToolStats>,
    pub by_shell: BTreeMap<String, ToolStats>,
    pub by_mcp: BTreeMap<String, ToolStats>,
    pub interruptions: InterruptionAnalysis,
    pub context_growth: ContextGrowthAnalysis,
    pub peak_context: PeakContextAnalysis,
}

impl MetricsAnalysis {
    /// Flat list of every session the user can select from the Metrics tab.
    /// Canonical order matches the sections in [`crate::ui`]: interruption
    /// offenders, peak-context sessions, token-spike findings, then top
    /// sessions.
    pub fn selectable_sessions(&self) -> Vec<SelectableSession> {
        let mut out = Vec::with_capacity(
            self.interruptions.by_session.len()
                + self.peak_context.findings.len()
                + self.context_growth.findings.len()
                + self.top_sessions.len(),
        );
        for s in &self.interruptions.by_session {
            out.push(SelectableSession {
                session_id: s.session_id.clone(),
                cwd: s.cwd.clone(),
                project: s.project.clone(),
                jsonl_path: s.jsonl_path.clone(),
                peak_timestamp_ms: None,
            });
        }
        for f in &self.peak_context.findings {
            out.push(SelectableSession {
                session_id: f.session_id.clone(),
                cwd: f.cwd.clone(),
                project: f.project.clone(),
                jsonl_path: f.jsonl_path.clone(),
                peak_timestamp_ms: (f.peak_timestamp_ms > 0).then_some(f.peak_timestamp_ms),
            });
        }
        for f in &self.context_growth.findings {
            out.push(SelectableSession {
                session_id: f.session_id.clone(),
                cwd: f.cwd.clone(),
                project: f.project.clone(),
                jsonl_path: f.jsonl_path.clone(),
                peak_timestamp_ms: (f.peak_timestamp_ms > 0).then_some(f.peak_timestamp_ms),
            });
        }
        for s in &self.top_sessions {
            out.push(SelectableSession {
                session_id: s.session_id.clone(),
                cwd: s.cwd.clone(),
                project: s.project.clone(),
                jsonl_path: s.jsonl_path.clone(),
                peak_timestamp_ms: None,
            });
        }
        out
    }
}
