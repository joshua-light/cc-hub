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

use crate::conversation::parse_timestamp_ms;
use crate::platform::paths;
use chrono::{Local, NaiveDate, TimeZone};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
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
}

/// One canonical assistant API call after dedup.
#[derive(Default)]
struct AssistantCall {
    model: String,
    tokens: Tokens,
    timestamp_ms: u64,
}

struct ParsedSession {
    session_id: String,
    project: String,
    is_subagent: bool,
    end_time_ms: u64,
    calls: Vec<AssistantCall>,
}

fn parse_session_file(path: &Path, is_subagent: bool) -> Option<ParsedSession> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let session_id = path.file_stem()?.to_string_lossy().to_string();
    let mut project: Option<String> = None;
    let mut end_time_ms: u64 = 0;

    // Dedup: requestId → AssistantCall (latest usage wins).
    let mut by_req: HashMap<String, AssistantCall> = HashMap::new();
    // message.id → canonical requestId for cross-requestId merge.
    let mut msg_id_to_req: HashMap<String, String> = HashMap::new();

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

        if project.is_none() {
            if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
                project = Some(project_name_from_cwd(cwd));
            }
        }

        if let Some(ts) = v.get("timestamp").and_then(parse_timestamp_ms) {
            if ts > end_time_ms {
                end_time_ms = ts;
            }
        }

        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }

        let req_id = v
            .get("requestId")
            .and_then(|r| r.as_str())
            .or_else(|| v.get("uuid").and_then(|u| u.as_str()))
            .unwrap_or("");
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
        if entry.model.is_empty() && !model.is_empty() {
            entry.model = model;
        }
        if let Some(u) = usage {
            // Each line carries cumulative usage for the request — overwrite.
            let tokens = Tokens {
                input: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                output: u
                    .get("output_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
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
    }

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
        is_subagent,
        end_time_ms,
        calls,
    })
}

fn project_name_from_cwd(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd)
        .to_string()
}

fn discover_session_files() -> Vec<(PathBuf, bool)> {
    let projects_dir = match paths::claude_home() {
        Some(d) => d.join("projects"),
        None => return Vec::new(),
    };
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
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
                out.push((p, false));
            } else if p.is_dir() {
                let sub = p.join("subagents");
                if sub.is_dir() {
                    if let Ok(sa) = std::fs::read_dir(&sub) {
                        for f in sa.flatten() {
                            let fp = f.path();
                            if fp.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                                out.push((fp, true));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

pub fn analyze() -> MetricsAnalysis {
    let files = discover_session_files();
    let mut sessions: Vec<ParsedSession> = files
        .into_iter()
        .filter_map(|(p, is_sub)| parse_session_file(&p, is_sub))
        .collect();

    let mut total_cost = 0.0;
    let mut total_tokens = Tokens::default();
    let mut total_messages = 0usize;
    let mut by_model: BTreeMap<String, ModelStats> = BTreeMap::new();
    let mut by_project: HashMap<String, ProjectStats> = HashMap::new();
    let mut by_day: BTreeMap<NaiveDate, DayStats> = BTreeMap::new();
    let mut top_sessions: Vec<SessionSummary> = Vec::new();

    for s in &mut sessions {
        let mut session_tokens = Tokens::default();
        let mut session_cost = 0.0;
        let mut top_model: HashMap<String, u64> = HashMap::new();

        for call in &s.calls {
            let p = pricing_for(&call.model);
            let c = cost_of(&call.tokens, &p);
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
                model,
                cost: session_cost,
                tokens: session_tokens,
                message_count: s.calls.len(),
                end_time_ms: s.end_time_ms,
                is_subagent: s.is_subagent,
            });

            total_cost += session_cost;
            total_messages += s.calls.len();
            total_tokens.add(&session_tokens);
        }
    }

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

    top_sessions.sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));
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
    }
}
