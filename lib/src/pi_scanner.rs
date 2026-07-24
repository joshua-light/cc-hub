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
        // Pi sessions can't reach Question (no AskUserQuestion tool) and the
        // scanner never emits the app-synthesized Starting, but the match
        // must be exhaustive — fall through like WaitingForInput.
        SessionState::Idle
        | SessionState::WaitingForInput
        | SessionState::Question
        | SessionState::Starting => {}
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

/// Build a minimal [`SessionInfo`] from a heartbeat alone, for a live session
/// whose transcript pi has not written to disk yet (a fresh idle session). The
/// heartbeat carries everything the grid needs to show a stable card — id, cwd,
/// tmux, model, state — so the transcript-derived fields are simply left empty
/// until the file appears and [`build_session_info`] takes over on a later tick
/// (the session id is identical, so the card does not jump).
fn session_from_heartbeat(
    hb: &crate::pi_bridge::Heartbeat,
    agent_id: &str,
    state: SessionState,
) -> Option<SessionInfo> {
    let session_id = hb.session_id.clone().or_else(|| {
        hb.session_file
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(str::to_string)
    })?;
    Some(SessionInfo {
        agent_id: agent_id.to_string(),
        agent_kind: AgentKind::Pi,
        pid: hb.pid,
        session_id,
        cwd: hb.cwd.clone(),
        project_name: project_name(&hb.cwd),
        started_at: 0,
        last_activity: None,
        state,
        last_user_message: None,
        summary: None,
        title: None,
        titling: false,
        model: hb.model.clone(),
        git_branch: None,
        version: None,
        jsonl_path: hb.session_file.clone(),
        tmux_session: Some(hb.tmux.clone()),
        current_tool: None,
        is_thinking: false,
        context_tokens: None,
        tool_uses_count: 0,
    })
}

/// Live Pi sessions from heartbeats, plus the full set of tmux names and
/// session files every live heartbeat claims.
///
/// Multiple live `pi` processes can share one session file — each resume points
/// a fresh process at the same `--session <file>`, and they all carry the same
/// transcript-derived session id. We collapse them to one card, but the two
/// returned claim-sets cover EVERY live heartbeat (dedup losers included) so the
/// caller can stop [`scan_external_live_sessions`] from re-pairing a collapsed
/// duplicate to an unrelated transcript and flickering it into the grid.
fn scan_live_heartbeats(
    agents: &[AgentConfig],
) -> (Vec<SessionInfo>, HashSet<String>, HashSet<PathBuf>) {
    let mut claimed_tmux: HashSet<String> = HashSet::new();
    let mut claimed_paths: HashSet<PathBuf> = HashSet::new();
    let Some(default_agent) = default_pi_agent(agents) else {
        return (Vec::new(), claimed_tmux, claimed_paths);
    };
    // Dedup by highest pid, NOT by newest `updatedAt`: when several live
    // processes shared a session id, the timestamp winner flipped every tick
    // and the surviving card's pid/tmux churned. A pid is stable for a
    // process's lifetime, and dead pids are already filtered out below, so the
    // highest live pid is a deterministic, non-oscillating choice.
    let mut best: HashMap<String, (u32, SessionInfo)> = HashMap::new();
    for hb in load_heartbeats() {
        if hb.agent.is_empty() {
            continue;
        }
        if !process::is_agent_process(AgentKind::Pi, hb.pid) {
            continue;
        }
        // A live heartbeat is itself proof of a live session — pi does not
        // flush a transcript until the first turn, so a brand-new idle session
        // has a heartbeat but no file on disk yet. Claim the tmux (and the
        // transcript path, present or not) up front regardless: without this,
        // the external-process scan re-pairs this very process to some
        // unrelated old transcript in the same cwd, and the card blinks between
        // the two. This claim also covers per-session dedup losers.
        claimed_tmux.insert(hb.tmux.clone());
        let existing = hb.session_file.clone().filter(|p| p.exists());
        if let Some(path) = hb.session_file.clone() {
            claimed_paths.insert(path);
        }
        let state = match hb.state {
            HeartbeatState::Idle => SessionState::Idle,
            HeartbeatState::Processing => SessionState::Processing,
        };
        let agent_id = if hb.agent.is_empty() {
            default_agent.id.clone()
        } else {
            hb.agent.clone()
        };
        // Prefer the richer transcript-derived card; fall back to a minimal
        // card built from the heartbeat alone while the transcript is missing.
        let info = existing
            .and_then(|path| {
                build_session_info(
                    agent_id.clone(),
                    hb.pid,
                    Some(hb.tmux.clone()),
                    state.clone(),
                    path,
                    hb.model.clone(),
                )
            })
            .or_else(|| session_from_heartbeat(&hb, &agent_id, state));
        let Some(info) = info else {
            continue;
        };
        match best.get(&info.session_id) {
            Some((prev_pid, _)) if *prev_pid >= hb.pid => {}
            _ => {
                best.insert(info.session_id.clone(), (hb.pid, info));
            }
        }
    }
    let sessions = best.into_values().map(|(_, info)| info).collect();
    (sessions, claimed_tmux, claimed_paths)
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
        files.sort_by_key(|b| std::cmp::Reverse(b.1));
    }

    // Pair heartbeat-less Pi processes to transcripts DETERMINISTICALLY. Without
    // a heartbeat we can't know which process wrote which transcript, and the
    // platform layer exposes no process start time, so we approximate "newest
    // process first" by descending pid (higher pids are generally started
    // later) and hand each process the next-newest unclaimed transcript in its
    // cwd. The point is a STABLE mapping: the bug was arbitrary list_pids()
    // order flip-flopping the transcript↔pid/tmux pairing between ticks, so
    // focus/send could target a different terminal each tick.
    //
    // Residual ambiguity: two heartbeat-less Pi processes sharing one cwd whose
    // pid order doesn't match their transcripts' creation order stay
    // mis-paired. Only heartbeats (the normal path) resolve this exactly.
    let mut pids = process::list_pids();
    pids.sort_unstable_by(|a, b| b.cmp(a));

    let mut out = Vec::new();
    for pid in pids {
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
    let relist_ttl = std::time::Duration::from_secs(cfg.orphan_relist_secs);
    let Some(root) = session_dirs() else {
        return Vec::new();
    };
    let Ok(project_dirs) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    for proj in project_dirs.flatten() {
        let proj_path = proj.path();
        // Cached per-dir listing shared with the Claude orphan walk: re-lists
        // only on a dir mtime change or TTL expiry. Per-file mtimes come from
        // the cache, so the age filter is at most `orphan_relist_secs` stale.
        let files = crate::dir_cache::list_jsonl_dir(&proj_path, relist_ttl);
        visited_dirs.insert(proj_path);
        let mut candidates: Vec<(PathBuf, SystemTime)> = Vec::new();
        for (path, mtime) in files.iter() {
            if claimed_paths.contains(path) {
                continue;
            }
            let Some(age) = mtime.elapsed().ok().map(|d| d.as_secs()) else {
                continue;
            };
            if age > cfg.window_secs {
                continue;
            }
            candidates.push((path.clone(), *mtime));
        }
        candidates.sort_by_key(|b| std::cmp::Reverse(b.1));
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
    // Evict entries for Pi session dirs gone this tick, scoped to the Pi root
    // so the Claude scanner's entries in the shared cache survive.
    crate::dir_cache::retain_under(&root, &visited_dirs);
    out
}

pub fn scan(agents: &[AgentConfig], titles: &HashMap<String, String>) -> Vec<SessionInfo> {
    // `claimed_*` come pre-seeded with every live heartbeat's tmux + transcript
    // (dedup losers included), then accumulate through each phase so a later
    // phase never re-surfaces a session an earlier one already owns.
    let (mut sessions, claimed_tmux, mut claimed_paths) = scan_live_heartbeats(agents);
    for s in &sessions {
        if let Some(path) = &s.jsonl_path {
            claimed_paths.insert(path.clone());
        }
    }
    let external = scan_external_live_sessions(agents, &claimed_paths, &claimed_tmux);
    for s in &external {
        if let Some(path) = &s.jsonl_path {
            claimed_paths.insert(path.clone());
        }
    }
    sessions.extend(external);
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
            if best.as_ref().is_some_and(|(t, _, _)| mtime <= *t) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;
    use std::fs;

    fn with_temp_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        let out = f(home.path());
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    #[test]
    fn session_from_heartbeat_builds_card_without_transcript_on_disk() {
        // A fresh idle pi session: the heartbeat exists but pi hasn't flushed a
        // transcript yet. The card must still be built, straight from the
        // heartbeat, so the session shows instead of being dropped (and then
        // mis-paired to an old transcript by the external scan → flicker).
        let hb = crate::pi_bridge::Heartbeat {
            agent: "codex".into(),
            pid: 4242,
            tmux: "cchub-1-2".into(),
            cwd: "/tmp/proj".into(),
            session_file: Some(PathBuf::from("/does/not/exist/019f9356.jsonl")),
            session_id: Some("019f9356".into()),
            state: HeartbeatState::Idle,
            model: Some("openai-codex/gpt-5.6-luna".into()),
            updated_at: 1,
        };
        let info = session_from_heartbeat(&hb, "codex", SessionState::Idle)
            .expect("card built from heartbeat");
        assert_eq!(info.session_id, "019f9356");
        assert_eq!(info.cwd, "/tmp/proj");
        assert_eq!(info.agent_kind, AgentKind::Pi);
        assert_eq!(info.tmux_session.as_deref(), Some("cchub-1-2"));
        assert_eq!(info.model.as_deref(), Some("openai-codex/gpt-5.6-luna"));
        assert_eq!(info.state, SessionState::Idle);
        // The (not-yet-existing) transcript path is retained so a later tick
        // can upgrade to the transcript-derived card and resume can target it.
        assert_eq!(
            info.jsonl_path,
            Some(PathBuf::from("/does/not/exist/019f9356.jsonl"))
        );
    }

    #[test]
    fn orchestrator_fallback_matches_prompt_prefix_not_task_id_mentions() {
        with_temp_home(|home| {
            let task_id = "t-pi-scan-1";
            let sessions = home.join(".pi/agent/sessions");
            let parent_dir = sessions.join("parent");
            let orch_dir = sessions.join("orch");
            fs::create_dir_all(&parent_dir).unwrap();
            fs::create_dir_all(&orch_dir).unwrap();
            fs::write(
                parent_dir.join("parent-session.jsonl"),
                format!(r#"{{"id":"pi-parent","type":"assistant","message":"mentioned {task_id} in output"}}"#),
            )
            .unwrap();
            fs::write(
                orch_dir.join("good-session.jsonl"),
                format!(
                    r#"{{"id":"pi-good","type":"system","message":"{} crashed early"}}"#,
                    crate::orchestrator::orchestrator_prompt_prefix(task_id)
                ),
            )
            .unwrap();

            let found =
                find_orchestrator_session(std::path::Path::new("/tmp/project"), task_id, None);
            assert_eq!(found.as_ref().map(|(sid, _)| sid.as_str()), Some("pi-good"));
        });
    }
}
