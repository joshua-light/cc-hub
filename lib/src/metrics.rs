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

use crate::agent::AgentKind;
use crate::config;
use crate::conversation::parse_timestamp_ms;
use crate::platform::paths;
use chrono::{Local, NaiveDate, TimeZone};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_creation_per_mtok: f64,
}

const DEFAULT_PRICING: ModelPricing = ModelPricing {
    input_per_mtok: 3.0,
    output_per_mtok: 15.0,
    cache_read_per_mtok: 0.30,
    cache_creation_per_mtok: 3.75,
};

fn pricing_for(model: &str) -> ModelPricing {
    // Family match — strip a trailing -YYYYMMDD suffix.
    let family = strip_date_suffix(model);
    match family {
        "claude-opus-4-7" | "claude-opus-4-6" | "claude-opus-4-5" => ModelPricing {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
            cache_read_per_mtok: 0.50,
            cache_creation_per_mtok: 6.25,
        },
        "claude-sonnet-4-7" | "claude-sonnet-4-6" | "claude-sonnet-4-5" => ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.30,
            cache_creation_per_mtok: 3.75,
        },
        "claude-haiku-4-5" | "claude-haiku-4-6" => ModelPricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cache_read_per_mtok: 0.10,
            cache_creation_per_mtok: 1.25,
        },
        _ => DEFAULT_PRICING,
    }
}

fn strip_date_suffix(model: &str) -> &str {
    let bytes = model.as_bytes();
    if bytes.len() >= 9 && bytes[bytes.len() - 9] == b'-' {
        let suffix = &bytes[bytes.len() - 8..];
        if suffix.iter().all(|b| b.is_ascii_digit()) {
            return &model[..model.len() - 9];
        }
    }
    model
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }

    fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
    }
}

fn cost_of(tokens: &Tokens, p: &ModelPricing) -> f64 {
    (tokens.input as f64 * p.input_per_mtok
        + tokens.output as f64 * p.output_per_mtok
        + tokens.cache_read as f64 * p.cache_read_per_mtok
        + tokens.cache_creation as f64 * p.cache_creation_per_mtok)
        / 1_000_000.0
}

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

#[derive(Default)]
struct ToolUse {
    name: String,
    id: String,
    /// Parsed argv-0 basenames of every segment of a `Bash` command.
    /// Empty for non-Bash tools.
    bash_commands: Vec<String>,
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

/// One canonical assistant API call after dedup.
#[derive(Default)]
struct AssistantCall {
    model: String,
    tokens: Tokens,
    timestamp_ms: u64,
    tool_uses: Vec<ToolUse>,
    cost_override: Option<f64>,
    /// Stable cross-file identity: `requestId`, else `message.id`, else the
    /// per-line uuid. Resume/fork copies history verbatim, so the same call
    /// reappears in a new session's JSONL under the original key; aggregation
    /// dedups on this so a resumed session isn't double-billed. Empty for Pi
    /// sessions, which don't carry these ids.
    dedup_key: String,
}

struct ParsedSession {
    session_id: String,
    project: String,
    cwd: String,
    jsonl_path: PathBuf,
    is_subagent: bool,
    end_time_ms: u64,
    calls: Vec<AssistantCall>,
    /// All tool_use_ids that received a tool_result (from user messages).
    tool_result_ids: HashSet<String>,
    /// tool_use ids issued after the transcript's last genuine user turn (a
    /// `type: "user"` entry carrying non-tool_result content — a new prompt or
    /// the interrupt marker). The conversation never moved on past these, so a
    /// missing result means in-flight or abandoned, not interrupted. Note:
    /// Claude Code writes each content block as its OWN entry, so a parallel
    /// tool batch spans several trailing entries, with non-conversational
    /// chatter (file-history-snapshot, mode, …) freely interleaved — neither
    /// may end the exemption; only a real user turn does.
    in_flight_tool_use_ids: HashSet<String>,
}

fn parse_session_file(path: &Path, is_subagent: bool, kind: AgentKind) -> Option<ParsedSession> {
    match kind {
        AgentKind::Claude => parse_claude_session_file(path, is_subagent),
        AgentKind::Pi => parse_pi_session_file(path),
    }
}

fn parse_claude_session_file(path: &Path, is_subagent: bool) -> Option<ParsedSession> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let session_id = path.file_stem()?.to_string_lossy().to_string();
    let mut project: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut end_time_ms: u64 = 0;

    // Dedup: requestId → AssistantCall (latest usage wins).
    let mut by_req: HashMap<String, AssistantCall> = HashMap::new();
    // message.id → canonical requestId for cross-requestId merge.
    let mut msg_id_to_req: HashMap<String, String> = HashMap::new();
    // tool_use_ids that we've already attached to a call — avoids duplicates
    // when the same content block reappears across requestId-redundant lines.
    let mut seen_tool_use_ids: HashSet<String> = HashSet::new();
    // Every tool_use_id that received a tool_result from a user message.
    let mut tool_result_ids: HashSet<String> = HashSet::new();
    // Position of each tool_use in the entry stream, and of the last genuine
    // user turn. An unmatched tool_use only counts as an interruption when a
    // genuine user turn follows it — that's what "the user Esc'd / moved on"
    // actually looks like in the transcript. Everything else (parallel batch
    // entries at the tail, chatter entries between tool_use and result, a
    // hard-killed session) is in-flight/abandoned and must not be charged.
    let mut entry_idx: usize = 0;
    let mut tool_use_pos: HashMap<String, usize> = HashMap::new();
    let mut last_genuine_user_turn: Option<usize> = None;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        entry_idx += 1;

        if cwd.is_none() {
            if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
                project = Some(project_name_from_cwd(c));
                cwd = Some(c.to_string());
            }
        }

        if let Some(ts) = v.get("timestamp").and_then(parse_timestamp_ms) {
            if ts > end_time_ms {
                end_time_ms = ts;
            }
        }

        let entry_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // User messages: harvest tool_result IDs so we can detect orphans, and
        // track genuine user turns (non-tool_result content: a fresh prompt or
        // the "[Request interrupted by user]" marker; `isMeta` entries are
        // harness-injected, not the human moving on).
        if entry_type == "user" {
            let is_meta = v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false);
            let message_content = v.get("message").and_then(|m| m.get("content"));
            let mut genuine = matches!(message_content.and_then(|c| c.as_str()), Some(s) if !s.trim().is_empty());
            if let Some(content) = message_content.and_then(|c| c.as_array()) {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        if let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) {
                            tool_result_ids.insert(id.to_string());
                        }
                    } else {
                        genuine = true;
                    }
                }
            }
            if genuine && !is_meta {
                last_genuine_user_turn = Some(entry_idx);
            }
            continue;
        }

        if entry_type != "assistant" {
            // Chatter entries (file-history-snapshot, mode, last-prompt, …)
            // interleave freely between a tool_use and its result — they say
            // nothing about whether the conversation moved on.
            continue;
        }

        let request_id = v.get("requestId").and_then(|r| r.as_str()).unwrap_or("");
        let req_id = if request_id.is_empty() {
            v.get("uuid").and_then(|u| u.as_str()).unwrap_or("")
        } else {
            request_id
        };
        if req_id.is_empty() {
            continue;
        }

        let inner = v.get("message");
        let usage = inner.and_then(|m| m.get("usage"));
        let model = inner
            .and_then(|m| m.get("model"))
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let msg_id = inner
            .and_then(|m| m.get("id"))
            .and_then(|m| m.as_str())
            .unwrap_or("");

        // Redirect via message.id if a different requestId already owns
        // this canonical API response.
        let canonical_req = if !msg_id.is_empty() {
            match msg_id_to_req.get(msg_id) {
                Some(existing) if existing != req_id => existing.clone(),
                _ => {
                    msg_id_to_req.insert(msg_id.to_string(), req_id.to_string());
                    req_id.to_string()
                }
            }
        } else {
            req_id.to_string()
        };

        let entry = by_req.entry(canonical_req).or_default();
        if entry.dedup_key.is_empty() {
            // Prefer requestId (survives resume/fork copies), fall back to
            // message.id, then the per-line uuid.
            entry.dedup_key = if !request_id.is_empty() {
                request_id.to_string()
            } else if !msg_id.is_empty() {
                msg_id.to_string()
            } else {
                req_id.to_string()
            };
        }
        if entry.model.is_empty() && !model.is_empty() {
            entry.model = model;
        }
        if let Some(u) = usage {
            // Each line carries cumulative usage for the request — overwrite.
            let tokens = Tokens {
                input: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                output: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                cache_read: u
                    .get("cache_read_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
                cache_creation: u
                    .get("cache_creation_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
            };
            if tokens.total() > 0 {
                entry.tokens = tokens;
            }
        }
        if let Some(ts) = v.get("timestamp").and_then(parse_timestamp_ms) {
            if ts > entry.timestamp_ms {
                entry.timestamp_ms = ts;
            }
        }

        // Tool uses live in message.content[] as blocks with type=tool_use
        // (one block per entry — Claude Code splits parallel batches across
        // consecutive entries).
        if let Some(content) = inner
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                    continue;
                }
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                // Record where the tool_use was issued (first sighting wins —
                // requestId-redundant lines repeat blocks) for the in-flight
                // cutoff below, regardless of within-file dedup.
                if !id.is_empty() {
                    tool_use_pos.entry(id.to_string()).or_insert(entry_idx);
                }
                if !id.is_empty() && !seen_tool_use_ids.insert(id.to_string()) {
                    continue;
                }
                let bash_commands = if name == "Bash" {
                    block
                        .get("input")
                        .and_then(|i| i.get("command"))
                        .and_then(|c| c.as_str())
                        .map(extract_bash_commands)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                entry.tool_uses.push(ToolUse {
                    name: name.to_string(),
                    id: id.to_string(),
                    bash_commands,
                });
            }
        }
    }

    // Exempt every tool_use the conversation never moved past: issued after
    // the last genuine user turn (or in a transcript with none). Their missing
    // results are in-flight/abandoned, not user interruptions.
    let in_flight_tool_use_ids: HashSet<String> = tool_use_pos
        .into_iter()
        .filter(|(_, pos)| last_genuine_user_turn.is_none_or(|u| *pos > u))
        .map(|(id, _)| id)
        .collect();

    let calls: Vec<AssistantCall> = by_req
        .into_values()
        .filter(|c| c.tokens.total() > 0)
        .collect();

    if calls.is_empty() {
        return None;
    }

    Some(ParsedSession {
        session_id,
        project: project.unwrap_or_else(|| "unknown".to_string()),
        cwd: cwd.unwrap_or_default(),
        jsonl_path: path.to_path_buf(),
        is_subagent,
        end_time_ms,
        calls,
        tool_result_ids,
        in_flight_tool_use_ids,
    })
}

fn parse_pi_session_file(path: &Path) -> Option<ParsedSession> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut session_id = path.file_stem()?.to_string_lossy().to_string();
    let mut cwd: Option<String> = None;
    let mut project: Option<String> = None;
    let mut end_time_ms = 0u64;
    let mut calls: Vec<AssistantCall> = Vec::new();
    let mut tool_result_ids: HashSet<String> = HashSet::new();
    // Tool call ids at the transcript tail — in-flight, not interrupted.
    let mut in_flight_tool_use_ids: HashSet<String> = HashSet::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(ts) = v.get("timestamp").and_then(parse_timestamp_ms) {
            end_time_ms = end_time_ms.max(ts);
        }

        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "session" => {
                if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                    session_id = id.to_string();
                }
                if let Some(c) = v.get("cwd").and_then(|x| x.as_str()) {
                    cwd = Some(c.to_string());
                    project = Some(project_name_from_cwd(c));
                }
            }
            "message" => {
                let Some(msg) = v.get("message") else {
                    continue;
                };
                match msg.get("role").and_then(|r| r.as_str()) {
                    Some("assistant") => {
                        let usage = msg.get("usage");
                        let tokens = Tokens {
                            input: usage
                                .and_then(|u| u.get("input"))
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0),
                            output: usage
                                .and_then(|u| u.get("output"))
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0),
                            cache_read: usage
                                .and_then(|u| u.get("cacheRead"))
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0),
                            cache_creation: usage
                                .and_then(|u| u.get("cacheWrite"))
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0),
                        };
                        let cost_override = usage
                            .and_then(|u| u.get("cost"))
                            .and_then(|c| c.get("total"))
                            .and_then(|v| v.as_f64());
                        let mut call = AssistantCall {
                            model: msg
                                .get("model")
                                .and_then(|m| m.as_str())
                                .unwrap_or("")
                                .to_string(),
                            tokens,
                            timestamp_ms: v
                                .get("timestamp")
                                .and_then(parse_timestamp_ms)
                                .unwrap_or(0),
                            tool_uses: Vec::new(),
                            cost_override,
                            // Pi sessions carry no requestId/message.id; cross-file
                            // dedup (BUG 5) doesn't apply, so leave the key empty.
                            dedup_key: String::new(),
                        };
                        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                            for block in content {
                                if block.get("type").and_then(|t| t.as_str()) != Some("toolCall") {
                                    continue;
                                }
                                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                if name.is_empty() {
                                    continue;
                                }
                                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                                let bash_commands = if name == "bash" {
                                    block
                                        .get("arguments")
                                        .and_then(|i| i.get("command"))
                                        .and_then(|c| c.as_str())
                                        .map(extract_bash_commands)
                                        .unwrap_or_default()
                                } else {
                                    Vec::new()
                                };
                                call.tool_uses.push(ToolUse {
                                    name: name.to_string(),
                                    id: id.to_string(),
                                    bash_commands,
                                });
                            }
                        }
                        // A trailing assistant toolCall with no result yet is
                        // in-flight; any later entry clears this.
                        in_flight_tool_use_ids = call
                            .tool_uses
                            .iter()
                            .map(|t| t.id.clone())
                            .filter(|id| !id.is_empty())
                            .collect();
                        if call.tokens.total() > 0 || !call.tool_uses.is_empty() {
                            calls.push(call);
                        }
                    }
                    Some("toolResult") => {
                        if let Some(id) = msg.get("toolCallId").and_then(|i| i.as_str()) {
                            tool_result_ids.insert(id.to_string());
                        }
                        in_flight_tool_use_ids.clear();
                    }
                    Some("user") => {
                        in_flight_tool_use_ids.clear();
                    }
                    _ => {
                        in_flight_tool_use_ids.clear();
                    }
                }
            }
            _ => {}
        }
    }

    if calls.is_empty() {
        return None;
    }

    Some(ParsedSession {
        session_id,
        project: project.unwrap_or_else(|| "unknown".to_string()),
        cwd: cwd.unwrap_or_default(),
        jsonl_path: path.to_path_buf(),
        is_subagent: false,
        end_time_ms,
        calls,
        tool_result_ids,
        in_flight_tool_use_ids,
    })
}

fn project_name_from_cwd(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd)
        .to_string()
}

fn discover_session_files() -> Vec<(PathBuf, bool, AgentKind)> {
    let mut out = Vec::new();

    if let Some(projects_dir) = paths::claude_home().map(|d| d.join("projects")) {
        if let Ok(entries) = std::fs::read_dir(&projects_dir) {
            for project in entries.flatten() {
                let pdir = project.path();
                if !pdir.is_dir() {
                    continue;
                }
                let inner = match std::fs::read_dir(&pdir) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for child in inner.flatten() {
                    let p = child.path();
                    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        out.push((p, false, AgentKind::Claude));
                    } else if p.is_dir() {
                        let sub = p.join("subagents");
                        if sub.is_dir() {
                            if let Ok(sa) = std::fs::read_dir(&sub) {
                                for f in sa.flatten() {
                                    let fp = f.path();
                                    if fp.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                                        out.push((fp, true, AgentKind::Claude));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(pi_sessions) = paths::pi_sessions_dir() {
        if let Ok(projects) = std::fs::read_dir(&pi_sessions) {
            for project in projects.flatten() {
                let pdir = project.path();
                if !pdir.is_dir() {
                    continue;
                }
                let Ok(inner) = std::fs::read_dir(&pdir) else {
                    continue;
                };
                for child in inner.flatten() {
                    let p = child.path();
                    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        out.push((p, false, AgentKind::Pi));
                    }
                }
            }
        }
    }

    out
}

pub fn analyze() -> MetricsAnalysis {
    analyze_with_progress(|_, _| {})
}

/// Like [`analyze`], but invokes `on_progress(scanned, total)` after each
/// session file is parsed. Called once with `(0, total)` up front so callers
/// can render an initial "0 / N" state before any file has been opened.
pub fn analyze_with_progress<F: FnMut(usize, usize)>(mut on_progress: F) -> MetricsAnalysis {
    let metrics_cfg = &config::get().metrics;
    let files = discover_session_files();
    let total = files.len();
    on_progress(0, total);
    let mut sessions: Vec<ParsedSession> = Vec::with_capacity(total);
    for (i, (p, is_sub, kind)) in files.into_iter().enumerate() {
        if let Some(s) = parse_session_file(&p, is_sub, kind) {
            sessions.push(s);
        }
        on_progress(i + 1, total);
    }

    let mut total_cost = 0.0;
    let mut total_tokens = Tokens::default();
    let mut total_messages = 0usize;
    let mut by_model: BTreeMap<String, ModelStats> = BTreeMap::new();
    let mut by_project: HashMap<String, ProjectStats> = HashMap::new();
    let mut by_day: BTreeMap<NaiveDate, DayStats> = BTreeMap::new();
    let mut top_sessions: Vec<SessionSummary> = Vec::new();
    let mut by_tool: BTreeMap<String, ToolStats> = BTreeMap::new();
    let mut by_shell: BTreeMap<String, ToolStats> = BTreeMap::new();
    let mut by_mcp: BTreeMap<String, ToolStats> = BTreeMap::new();
    let mut interruptions = InterruptionAnalysis::default();
    let mut growth = ContextGrowthAnalysis::default();
    let mut peak_ctx_findings: Vec<PeakContextFinding> = Vec::new();
    // Cross-file dedup of canonical calls (BUG 5): resume/fork copies history
    // verbatim, so the same call reappears in later files. The first parsed
    // file that carries an id owns its cost/tokens/day; later files skip it.
    let mut global_seen: HashSet<String> = HashSet::new();

    for s in &mut sessions {
        let mut session_tokens = Tokens::default();
        let mut session_cost = 0.0;
        let mut top_model: HashMap<String, u64> = HashMap::new();
        let mut session_tools: HashSet<&str> = HashSet::new();
        let mut session_shell: HashSet<&str> = HashSet::new();
        let mut session_mcp: HashSet<&str> = HashSet::new();
        let mut session_orphans = 0usize;
        let mut session_wasted = 0.0f64;
        let mut session_last_orphan_tool = String::new();
        let mut session_messages = 0usize;
        // Calls this session actually owns after cross-file dedup. Drives the
        // context series so a resumed file's copied prefix (counted by the
        // original file) isn't re-analyzed for peak/growth.
        let mut owned_calls: Vec<&AssistantCall> = Vec::new();

        for call in &s.calls {
            // Skip calls already counted by an earlier file (resume/fork copy).
            if !call.dedup_key.is_empty() && !global_seen.insert(call.dedup_key.clone()) {
                continue;
            }
            session_messages += 1;
            owned_calls.push(call);
            let p = pricing_for(&call.model);
            let c = call
                .cost_override
                .unwrap_or_else(|| cost_of(&call.tokens, &p));
            session_cost += c;
            session_tokens.add(&call.tokens);

            let model_key = if call.model.is_empty() {
                "unknown".to_string()
            } else {
                call.model.clone()
            };
            let m = by_model.entry(model_key.clone()).or_default();
            m.cost += c;
            m.tokens.add(&call.tokens);
            m.messages += 1;

            let proj = by_project.entry(s.project.clone()).or_default();
            proj.cost += c;
            proj.tokens.add(&call.tokens);
            proj.messages += 1;

            if call.timestamp_ms > 0 {
                let secs = (call.timestamp_ms / 1000) as i64;
                if let chrono::LocalResult::Single(dt) = Local.timestamp_opt(secs, 0) {
                    let day = dt.date_naive();
                    by_day.entry(day).or_default().cost += c;
                }
            }

            *top_model.entry(model_key).or_insert(0) += call.tokens.total();

            let mut call_orphans = 0usize;
            let mut call_last_orphan: &str = "";
            for tu in &call.tool_uses {
                let name = &tu.name;
                if let Some(server) = crate::models::mcp_server(name) {
                    let entry = by_mcp.entry(server.to_string()).or_default();
                    entry.count += 1;
                    session_mcp.insert(server);
                } else {
                    let entry = by_tool.entry(name.clone()).or_default();
                    entry.count += 1;
                    session_tools.insert(name.as_str());
                }
                for bc in &tu.bash_commands {
                    let entry = by_shell.entry(bc.clone()).or_default();
                    entry.count += 1;
                    session_shell.insert(bc.as_str());
                }
                if !tu.id.is_empty()
                    && !s.tool_result_ids.contains(&tu.id)
                    && !s.in_flight_tool_use_ids.contains(&tu.id)
                {
                    call_orphans += 1;
                    call_last_orphan = name.as_str();
                }
            }
            if call_orphans > 0 {
                session_orphans += call_orphans;
                // Charge the whole call cost once — Claude paid for the API
                // response even though the tool call was Esc'd.
                session_wasted += c;
                if !call_last_orphan.is_empty() {
                    session_last_orphan_tool.clear();
                    session_last_orphan_tool.push_str(call_last_orphan);
                }
            }
        }

        for tool in &session_tools {
            if let Some(stats) = by_tool.get_mut(*tool) {
                stats.sessions += 1;
            }
        }
        for cmd in &session_shell {
            if let Some(stats) = by_shell.get_mut(*cmd) {
                stats.sessions += 1;
            }
        }
        for server in &session_mcp {
            if let Some(stats) = by_mcp.get_mut(*server) {
                stats.sessions += 1;
            }
        }

        // Build the per-turn context series once; both peak-context and
        // growth-scoring read from it. Sessions with zero timestamped calls
        // contribute nothing to either.
        let mut series_calls: Vec<&AssistantCall> = owned_calls
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
                peak_ctx_findings.push(PeakContextFinding {
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
        if series.len() >= metrics_cfg.min_growth_turns {
            growth.sessions_scored += 1;
            if let Some((score, peak_delta, peak_idx)) = score_growth(&series) {
                if score >= metrics_cfg.growth_threshold && peak_delta > 0 {
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

        if session_orphans > 0 {
            interruptions.total_interrupted_turns += session_orphans;
            interruptions.total_wasted_cost += session_wasted;
            interruptions.sessions_affected += 1;
            interruptions.by_session.push(SessionInterruption {
                session_id: s.session_id.clone(),
                project: s.project.clone(),
                cwd: s.cwd.clone(),
                jsonl_path: s.jsonl_path.clone(),
                orphan_count: session_orphans,
                wasted_cost: session_wasted,
                last_tool_name: session_last_orphan_tool,
            });
        }

        if session_cost > 0.0 {
            // session-level dominant model = highest token total
            let model = top_model
                .into_iter()
                .max_by_key(|(_, n)| *n)
                .map(|(m, _)| m)
                .unwrap_or_else(|| "unknown".to_string());

            top_sessions.push(SessionSummary {
                session_id: std::mem::take(&mut s.session_id),
                project: s.project.clone(),
                cwd: s.cwd.clone(),
                jsonl_path: s.jsonl_path.clone(),
                model,
                cost: session_cost,
                tokens: session_tokens,
                message_count: session_messages,
                end_time_ms: s.end_time_ms,
                is_subagent: s.is_subagent,
            });

            total_cost += session_cost;
            total_messages += session_messages;
            total_tokens.add(&session_tokens);
        }
    }

    interruptions.by_session.sort_by(|a, b| {
        b.wasted_cost
            .partial_cmp(&a.wasted_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    interruptions
        .by_session
        .truncate(metrics_cfg.top_interruptions);

    growth.findings.sort_by(|a, b| {
        let ka = a.score * a.total_cost;
        let kb = b.score * b.total_cost;
        kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
    });
    growth.findings.truncate(metrics_cfg.top_growth_findings);

    peak_ctx_findings.sort_by_key(|b| std::cmp::Reverse(b.peak_ctx_tokens));
    peak_ctx_findings.truncate(metrics_cfg.top_peak_context_findings);
    let peak_context = PeakContextAnalysis {
        findings: peak_ctx_findings,
    };

    // Bump per-model session counts after the per-call loop.
    for s in &top_sessions {
        if let Some(m) = by_model.get_mut(&s.model) {
            m.sessions += 1;
        }
        if let Some(p) = by_project.get_mut(&s.project) {
            p.sessions += 1;
        }
    }

    let cache_hit_rate = {
        let denom = total_tokens.cache_read + total_tokens.cache_creation;
        if denom == 0 {
            0.0
        } else {
            total_tokens.cache_read as f64 / denom as f64
        }
    };

    top_sessions.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top_n: Vec<_> = top_sessions.iter().take(12).cloned().collect();

    let mut top_projects: Vec<(String, ProjectStats)> = by_project
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    top_projects.sort_by(|a, b| {
        b.1.cost
            .partial_cmp(&a.1.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top_projects.truncate(10);

    MetricsAnalysis {
        total_cost,
        total_sessions: top_sessions.len(),
        total_messages,
        total_tokens,
        cache_hit_rate,
        by_model,
        by_project,
        by_day,
        top_sessions: top_n,
        top_projects,
        by_tool,
        by_shell,
        by_mcp,
        interruptions,
        context_growth: growth,
        peak_context,
    }
}

/// Split a Bash invocation into the basenames of its constituent commands.
///
/// Mirrors codeburn's approach: strip quoted strings (so `;` / `|` / `&`
/// inside a literal don't split), tokenize on `;`, `|`, `&`, and take the
/// argv-0 basename of each segment. `cd` and empty segments are dropped.
fn extract_bash_commands(command: &str) -> Vec<String> {
    if command.trim().is_empty() {
        return Vec::new();
    }
    let stripped: String = {
        let mut out = String::with_capacity(command.len());
        let mut quote: Option<char> = None;
        for c in command.chars() {
            match quote {
                Some(q) if c == q => {
                    quote = None;
                    out.push(' ');
                }
                Some(_) => out.push(' '),
                None if c == '"' || c == '\'' => {
                    quote = Some(c);
                    out.push(' ');
                }
                None => out.push(c),
            }
        }
        out
    };
    let mut segments: Vec<&str> = vec![stripped.as_str()];
    for sep in ["&&", "||", ";", "|"] {
        segments = segments.into_iter().flat_map(|s| s.split(sep)).collect();
    }

    let mut cmds = Vec::new();
    for segment in segments {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }
        let first = match seg.split_whitespace().next() {
            Some(t) => t,
            None => continue,
        };
        let base = Path::new(first)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(first);
        if base.is_empty() || base == "cd" {
            continue;
        }
        cmds.push(base.to_string());
    }
    cmds
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

#[cfg(test)]
mod in_flight_tests {
    use super::*;
    use std::io::Write;

    fn assistant_tool_use(req: &str, msg: &str, tool_id: &str) -> String {
        format!(
            r#"{{"type":"assistant","requestId":"{req}","message":{{"id":"{msg}","model":"claude-sonnet-5","usage":{{"input_tokens":100,"output_tokens":10}},"content":[{{"type":"tool_use","id":"{tool_id}","name":"Bash","input":{{"command":"ls"}}}}]}}}}"#
        )
    }

    fn parse(lines: &[String]) -> ParsedSession {
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        for l in lines {
            writeln!(f, "{}", l).expect("write");
        }
        parse_claude_session_file(f.path(), false).expect("parse")
    }

    #[test]
    fn parallel_batch_at_tail_is_in_flight() {
        // Claude Code splits a parallel batch across consecutive entries; a
        // result for one sibling must not mark the still-running other as an
        // interruption (the tool_result-only user entry is not a genuine turn).
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            assistant_tool_use("r1", "m1", "tu_b"),
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_a"}]}}"#.into(),
        ]);
        assert!(s.tool_result_ids.contains("tu_a"));
        assert!(s.in_flight_tool_use_ids.contains("tu_b"));
    }

    #[test]
    fn chatter_after_tool_use_stays_in_flight() {
        // Non-conversational entries interleave between tool_use and result in
        // ~11% of real executions; they must not end the exemption.
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            r#"{"type":"file-history-snapshot","snapshot":{}}"#.into(),
        ]);
        assert!(s.in_flight_tool_use_ids.contains("tu_a"));
    }

    #[test]
    fn genuine_user_turn_after_tool_use_is_interruption() {
        // A real user turn (text content) after an unmatched tool_use is what
        // an interruption actually looks like — no exemption.
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#.into(),
        ]);
        assert!(!s.in_flight_tool_use_ids.contains("tu_a"));
        assert!(!s.tool_result_ids.contains("tu_a"));
    }

    #[test]
    fn killed_session_tail_is_not_interruption() {
        // Transcript ends on the tool_use (hard-killed session): nothing moved
        // on, so it's abandoned, not interrupted — deliberately uncounted.
        let s = parse(&[assistant_tool_use("r1", "m1", "tu_a")]);
        assert!(s.in_flight_tool_use_ids.contains("tu_a"));
    }

    #[test]
    fn meta_and_string_content_user_turns() {
        // isMeta user entries are harness-injected — not the human moving on.
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            r#"{"type":"user","isMeta":true,"message":{"content":"injected caveat"}}"#.into(),
        ]);
        assert!(s.in_flight_tool_use_ids.contains("tu_a"));

        // Plain string content is a genuine prompt.
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            r#"{"type":"user","message":{"content":"new question"}}"#.into(),
        ]);
        assert!(!s.in_flight_tool_use_ids.contains("tu_a"));
    }
}
