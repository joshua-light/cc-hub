use crate::agent::{AgentConfig, AgentKind};
use crate::config;
use crate::conversation;
use crate::models::{SessionDetail, SessionInfo, SessionState};
use crate::pi_bridge::{load_heartbeats, HeartbeatState};
use crate::pi_conversation;
use crate::platform::paths;
use crate::platform::process;
use crate::send;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn mtime_age_secs(path: &Path) -> Option<u64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
        .map(|d| d.as_secs())
}

fn encode_path(path: &str) -> String {
    let trimmed = path.trim_matches('/');
    format!("--{}--", trimmed.replace('/', "-"))
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn session_dirs() -> Option<PathBuf> {
    paths::pi_sessions_dir()
}

fn default_pi_agent(agents: &[AgentConfig]) -> Option<AgentConfig> {
    agents.iter().find(|a| a.kind == AgentKind::Pi).cloned()
}

fn build_session_info(
    agent_id: String,
    pid: u32,
    tmux_session: Option<String>,
    state: SessionState,
    jsonl_path: PathBuf,
    model_override: Option<String>,
) -> Option<SessionInfo> {
    let head = conversation::read_jsonl_head(&jsonl_path, 4096);
    let cwd = head
        .iter()
        .find_map(|e| e.get("cwd").and_then(|c| c.as_str()))?
        .to_string();
    let started_at = head
        .iter()
        .find_map(|e| {
            e.get("timestamp")
                .and_then(conversation::parse_timestamp_ms)
        })
        .unwrap_or(0);
    let tail = pi_conversation::read_jsonl_tail_for_state(&jsonl_path);
    let mut parsed_state = pi_conversation::extract_state(&tail);
    match state {
        SessionState::Inactive => parsed_state = SessionState::Inactive,
        SessionState::Processing => parsed_state = SessionState::Processing,
        SessionState::Idle | SessionState::WaitingForInput => {}
    }
    let last_user_message = pi_conversation::extract_last_user_message(&tail);
    let last_activity = pi_conversation::extract_last_activity(&tail);
    let (git_branch, model, version) = pi_conversation::extract_metadata(&tail);
    let summary = pi_conversation::extract_first_user_message(&head);
    let session_id = head
        .iter()
        .find_map(|e| e.get("id").and_then(|v| v.as_str()))
        .map(str::to_string)
        .or_else(|| {
            jsonl_path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })?;

    let tool_uses_count = crate::tool_use_count::count_pi(&jsonl_path);
    Some(SessionInfo {
        agent_id,
        agent_kind: AgentKind::Pi,
        pid,
        session_id,
        cwd: cwd.clone(),
        project_name: project_name(&cwd),
        started_at,
        last_activity,
        state: parsed_state,
        last_user_message,
        summary,
        title: None,
        titling: false,
        model: model_override.or(model),
        git_branch,
        version,
        jsonl_path: Some(jsonl_path),
        tmux_session,
        current_tool: pi_conversation::extract_current_tool(&tail),
        is_thinking: pi_conversation::is_currently_thinking(&tail),
        context_tokens: pi_conversation::extract_context_tokens(&tail),
        tool_uses_count,
    })
}

fn scan_live_heartbeats(agents: &[AgentConfig]) -> Vec<SessionInfo> {
    let Some(default_agent) = default_pi_agent(agents) else {
        return Vec::new();
    };
    let mut latest: HashMap<String, (u64, SessionInfo)> = HashMap::new();
    for hb in load_heartbeats() {
        if hb.agent.is_empty() {
            continue;
        }
        if !process::is_agent_process(AgentKind::Pi, hb.pid) {
            continue;
        }
        let Some(path) = hb.session_file.clone().filter(|p| p.exists()) else {
            continue;
        };
        let state = match hb.state {
            HeartbeatState::Idle => SessionState::Idle,
            HeartbeatState::Processing => SessionState::Processing,
        };
        let agent_id = if hb.agent.is_empty() {
            default_agent.id.clone()
        } else {
            hb.agent.clone()
        };
        let Some(info) = build_session_info(
            agent_id,
            hb.pid,
            Some(hb.tmux.clone()),
            state,
            path,
            hb.model.clone(),
        ) else {
            continue;
        };
        let ts = hb.updated_at;
        match latest.get(&info.session_id) {
            Some((prev, _)) if *prev >= ts => {}
            _ => {
                latest.insert(info.session_id.clone(), (ts, info));
            }
        }
    }
    latest.into_values().map(|(_, info)| info).collect()
}

fn scan_external_live_sessions(
    agents: &[AgentConfig],
    claimed_paths: &HashSet<PathBuf>,
    claimed_tmux: &HashSet<String>,
) -> Vec<SessionInfo> {
    let Some(default_agent) = default_pi_agent(agents) else {
        return Vec::new();
    };
    let tmux_panes = send::tmux_panes();
    let mut by_cwd: HashMap<String, Vec<(PathBuf, SystemTime)>> = HashMap::new();
    let Some(root) = session_dirs() else {
        return Vec::new();
    };
    let Ok(project_dirs) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    for proj in project_dirs.flatten() {
        let Ok(files) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl")
                || claimed_paths.contains(&path)
            {
                continue;
            }
            let head = conversation::read_jsonl_head(&path, 4096);
            let Some(cwd) = head
                .iter()
                .find_map(|e| e.get("cwd").and_then(|c| c.as_str()))
            else {
                continue;
            };
            let Some(mtime) = path.metadata().ok().and_then(|m| m.modified().ok()) else {
                continue;
            };
            by_cwd
                .entry(cwd.to_string())
                .or_default()
                .push((path, mtime));
        }
    }
    for files in by_cwd.values_mut() {
        files.sort_by(|a, b| b.1.cmp(&a.1));
    }

    let mut out = Vec::new();
    for pid in process::list_pids() {
        if !process::is_agent_process(AgentKind::Pi, pid) {
            continue;
        }
        let Some(cwd) = process::current_dir(pid) else {
            continue;
        };
        let tmux = send::tmux_session_for_pid_in(pid, &tmux_panes);
        if tmux.as_deref().is_some_and(|n| claimed_tmux.contains(n)) {
            continue;
        }
        let Some(files) = by_cwd.get_mut(&cwd) else {
            continue;
        };
        let Some((path, _)) = files.first().cloned() else {
            continue;
        };
        files.remove(0);
        let Some(mut info) = build_session_info(
            default_agent.id.clone(),
            pid,
            tmux,
            SessionState::Idle,
            path,
            None,
        ) else {
            continue;
        };
        if info.state == SessionState::Inactive {
            info.state = SessionState::WaitingForInput;
        }
        out.push(info);
    }
    out
}

fn scan_inactive_sessions(
    agents: &[AgentConfig],
    claimed_paths: &HashSet<PathBuf>,
) -> Vec<SessionInfo> {
    let Some(default_agent) = default_pi_agent(agents) else {
        return Vec::new();
    };
    let cfg = &config::get().inactive;
    let Some(root) = session_dirs() else {
        return Vec::new();
    };
    let Ok(project_dirs) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for proj in project_dirs.flatten() {
        let Ok(files) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        let mut candidates: Vec<(PathBuf, SystemTime)> = Vec::new();
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl")
                || claimed_paths.contains(&path)
            {
                continue;
            }
            let Some((mtime, age)) = path
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok().map(|d| (t, d.as_secs())))
            else {
                continue;
            };
            if age > cfg.window_secs {
                continue;
            }
            candidates.push((path, mtime));
        }
        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        for (path, _) in candidates.into_iter().take(cfg.max_per_project) {
            if let Some(info) = build_session_info(
                default_agent.id.clone(),
                0,
                None,
                SessionState::Inactive,
                path,
                None,
            ) {
                out.push(info);
            }
        }
    }
    out
}

pub fn scan(agents: &[AgentConfig], titles: &HashMap<String, String>) -> Vec<SessionInfo> {
    let mut sessions = scan_live_heartbeats(agents);
    let claimed_paths: HashSet<PathBuf> = sessions
        .iter()
        .filter_map(|s| s.jsonl_path.clone())
        .collect();
    let claimed_tmux: HashSet<String> = sessions
        .iter()
        .filter_map(|s| s.tmux_session.clone())
        .collect();
    sessions.extend(scan_external_live_sessions(
        agents,
        &claimed_paths,
        &claimed_tmux,
    ));
    let claimed_paths: HashSet<PathBuf> = sessions
        .iter()
        .filter_map(|s| s.jsonl_path.clone())
        .collect();
    sessions.extend(scan_inactive_sessions(agents, &claimed_paths));

    for session in sessions.iter_mut() {
        if session.title.is_none() {
            session.title = titles.get(&session.session_id).cloned();
        }
    }
    sessions
}

pub fn load_detail(info: &SessionInfo) -> Option<SessionDetail> {
    let path = info.jsonl_path.as_ref()?;
    let entries = conversation::read_jsonl_tail(path, 65536);
    let recent_messages = pi_conversation::extract_messages(&entries, 15);
    let (total_input_tokens, total_output_tokens) = pi_conversation::extract_token_totals(&entries);
    Some(SessionDetail {
        info: info.clone(),
        recent_messages,
        total_input_tokens,
        total_output_tokens,
    })
}

pub fn load_state_explanation(
    info: &SessionInfo,
) -> Option<(SessionInfo, crate::conversation::StateExplanation)> {
    let path = info.jsonl_path.as_ref()?;
    let entries = pi_conversation::read_jsonl_tail_for_state(path);
    let mtime_age_secs = mtime_age_secs(path);
    Some((
        info.clone(),
        pi_conversation::explain_state(&entries, mtime_age_secs),
    ))
}

pub fn find_orchestrator_session(
    project_root: &Path,
    task_id: &str,
    stored_sid: Option<&str>,
) -> Option<(String, PathBuf)> {
    // Fast path: trust the sid the task already recorded. Look up the file
    // directly under the encoded project dir, then anywhere under the Pi
    // sessions root — same drift-tolerance reasoning as the Claude scanner.
    if let Some(sid) = stored_sid {
        let target = format!("{}.jsonl", sid);
        if let Some(root) = session_dirs() {
            let direct = root
                .join(encode_path(&project_root.to_string_lossy()))
                .join(&target);
            if direct.exists() {
                return Some((sid.to_string(), direct));
            }
            if let Ok(entries) = std::fs::read_dir(&root) {
                for entry in entries.flatten() {
                    let candidate = entry.path().join(&target);
                    if candidate.exists() {
                        return Some((sid.to_string(), candidate));
                    }
                }
            }
        }
    }
    // Final fallback: raw prompt-prefix search. Pi orchestrators run in the
    // project root (not a task worktree), so the task id is not present in the
    // encoded session directory. Searching for the stable orchestrator prompt
    // prefix recovers sessions that crashed before structured message parsing
    // can identify the first user turn, without broad task-id false positives.
    use std::io::Read;

    let root = session_dirs()?;
    let needle = crate::orchestrator::orchestrator_prompt_prefix(task_id);
    let mut best: Option<(SystemTime, String, PathBuf)> = None;
    let Ok(project_dirs) = std::fs::read_dir(&root) else {
        return None;
    };
    for proj_entry in project_dirs.flatten() {
        let proj_path = proj_entry.path();
        if !proj_path.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&proj_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(mtime) = path.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            if best.as_ref().map_or(false, |(t, _, _)| mtime <= *t) {
                continue;
            }
            let mut buf = vec![0u8; 32 * 1024];
            let Ok(n) = std::fs::File::open(&path).and_then(|mut f| f.read(&mut buf)) else {
                continue;
            };
            buf.truncate(n);
            if !String::from_utf8_lossy(&buf).contains(&needle) {
                continue;
            }
            let parsed_head = conversation::read_jsonl_head(&path, 4096);
            let sid = parsed_head
                .iter()
                .find_map(|e| e.get("id").and_then(|v| v.as_str()))
                .map(str::to_string)
                .or_else(|| {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                });
            if let Some(sid) = sid {
                best = Some((mtime, sid, path));
            }
        }
    }
    best.map(|(_, sid, p)| (sid, p))
}
