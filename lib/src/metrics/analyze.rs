use super::context::context_findings;
use super::cost::{cost_of, pricing_for, Tokens};
use super::discover::discover_session_files;
use super::parse::{parse_session_file, AssistantCall, ParsedSession};
use super::{
    ContextGrowthAnalysis, DayStats, InterruptionAnalysis, MetricsAnalysis, ModelStats,
    PeakContextAnalysis, PeakContextFinding, ProjectStats, SessionInterruption, SessionSummary,
    ToolStats,
};
use crate::config;
use chrono::{Local, NaiveDate, TimeZone};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Parse every discovered transcript and aggregate it. Invokes
/// `on_progress(scanned, total)` after each session file is parsed. Called once with `(0, total)` up front so callers
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
            m.messages += 1;

            let proj = by_project.entry(s.project.clone()).or_default();
            proj.cost += c;
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

        context_findings(
            s,
            &owned_calls,
            session_cost,
            metrics_cfg,
            &mut peak_ctx_findings,
            &mut growth,
        );

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

    for s in &top_sessions {
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

    let mut top_projects: Vec<(String, ProjectStats)> = by_project.into_iter().collect();
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
