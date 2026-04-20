use crate::conversation;
use crate::models::{short_sid, RawSession, SessionDetail, SessionInfo, SessionState};
use crate::platform::paths;
use crate::platform::process::{Process, ProcessInfo};
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Seconds since `path` was last modified, or `None` if stat fails.
fn mtime_age_secs(path: &Path) -> Option<u64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
        .map(|d| d.as_secs())
}

/// Keep dead sessions visible if their JSONL was touched within this window.
/// Hardcoded for now — surface as a CLI/env knob once the UX settles.
const INACTIVE_WINDOW_SECS: u64 = 3 * 86_400;

/// Cap on how many inactive sessions we surface per project (cwd), ranked by
/// most recent activity. Keeps the grid navigable when a single project has
/// dozens of touched JSONLs in the window.
const INACTIVE_MAX_PER_PROJECT: usize = 5;

fn claude_dir() -> Option<PathBuf> {
    paths::claude_home()
}

fn sessions_dir() -> Option<PathBuf> {
    claude_dir().map(|d| d.join("sessions"))
}

fn projects_dir() -> Option<PathBuf> {
    claude_dir().map(|d| d.join("projects"))
}

fn encode_path(path: &str) -> String {
    path.replace('/', "-").replace('.', "-")
}

pub fn find_jsonl(cwd: &str, session_id: &str) -> Option<PathBuf> {
    let projects = projects_dir()?;
    let encoded = encode_path(cwd);
    let jsonl_path = projects.join(&encoded).join(format!("{}.jsonl", session_id));
    if jsonl_path.exists() {
        Some(jsonl_path)
    } else {
        None
    }
}

/// Check if a process has a real parent (not reparented to init).
fn has_real_parent(pid: u32) -> bool {
    Process::parent_pid(pid).is_some_and(|ppid| ppid > 1)
}

/// Read /clear events from the tail of ~/.claude/history.jsonl.
/// Returns session_id → latest clear timestamp (ms).
fn read_clears_from_history() -> HashMap<String, u64> {
    let path = match claude_dir() {
        Some(d) => d.join("history.jsonl"),
        None => return Default::default(),
    };
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Default::default(),
    };

    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = std::io::BufReader::new(file);
    let tail_bytes = 128 * 1024;
    if len > tail_bytes {
        use std::io::{Seek, SeekFrom};
        let _ = reader.seek(SeekFrom::Start(len - tail_bytes));
        let mut discard = String::new();
        let _ = std::io::BufRead::read_line(&mut reader, &mut discard);
    }

    let mut clears = HashMap::new();
    for line in std::io::BufRead::lines(reader) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if !line.contains("/clear") {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
            if obj.get("display").and_then(|d| d.as_str()) == Some("/clear") {
                if let (Some(sid), Some(ts)) = (
                    obj.get("sessionId").and_then(|s| s.as_str()),
                    obj.get("timestamp").and_then(|t| t.as_u64()),
                ) {
                    let entry = clears.entry(sid.to_string()).or_insert(0u64);
                    if ts > *entry {
                        *entry = ts;
                    }
                }
            }
        }
    }
    clears
}

/// Resolve the JSONL path for every alive session.
///
/// After `/clear`, Claude Code creates a new JSONL with a fresh session ID
/// but never updates the session metadata file.  This means the metadata
/// still points to the OLD session ID whose JSONL contains stale data.
///
/// Resolution strategy:
///   1. Non-cleared sessions → use their own JSONL (session_id match).
///   2. Cleared sessions → follow the /clear chain.  Each /clear creates a
///      new JSONL whose first entry timestamp matches the clear event within
///      a few milliseconds.  If THAT JSONL was also cleared, follow the
///      chain until we reach one that wasn't.
fn resolve_jsonl_paths(
    sessions: &[RawSession],
    clears: &HashMap<String, u64>,
    claimed: &HashSet<String>,
) -> HashMap<String, Option<PathBuf>> {
    let mut result: HashMap<String, Option<PathBuf>> = HashMap::new();

    // Pass 1: non-cleared sessions use their own JSONL.
    for raw in sessions {
        if !clears.contains_key(&raw.session_id) {
            let path = find_jsonl(&raw.cwd, &raw.session_id);
            let sid_short = short_sid(&raw.session_id);
            debug!(
                "resolve sid={} (not cleared) → {}",
                sid_short,
                path.as_ref().map_or("not found", |_| "found")
            );
            result.insert(raw.session_id.clone(), path);
        }
    }

    // Pass 2: cleared sessions — follow the /clear chain.
    // Build the orphan index once per project directory.
    let mut orphan_index: HashMap<String, OrphanIndex> = HashMap::new();
    for raw in sessions.iter().filter(|r| clears.contains_key(&r.session_id)) {
        orphan_index
            .entry(raw.cwd.clone())
            .or_insert_with(|| OrphanIndex::build(&raw.cwd, claimed));
    }

    for raw in sessions.iter().filter(|r| clears.contains_key(&r.session_id)) {
        let sid_short = short_sid(&raw.session_id);
        let clear_ts = clears.get(&raw.session_id).copied().unwrap_or(0);
        let path = orphan_index
            .get(&raw.cwd)
            .and_then(|idx| idx.follow_chain(&raw.session_id, clears));
        debug!(
            "resolve sid={} (cleared at {}) → {}",
            sid_short, clear_ts,
            path.as_ref().map_or("chain broken".to_string(), |p| p.display().to_string())
        );
        result.insert(raw.session_id.clone(), path);
    }

    result
}

/// Index of orphan JSONLs in a project directory, keyed by first-entry
/// timestamp for fast lookup during /clear chain resolution.
struct OrphanIndex {
    /// (first_entry_timestamp_ms, session_id, path)
    entries: Vec<(u64, String, PathBuf)>,
}

impl OrphanIndex {
    fn build(cwd: &str, claimed: &HashSet<String>) -> Self {
        let proj_dir = match projects_dir().map(|p| p.join(encode_path(cwd))) {
            Some(d) => d,
            None => return Self { entries: Vec::new() },
        };
        let dir_entries = match std::fs::read_dir(&proj_dir) {
            Ok(e) => e,
            Err(_) => return Self { entries: Vec::new() },
        };

        let mut entries = Vec::new();
        for entry in dir_entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let sid = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if claimed.contains(&sid) {
                continue;
            }
            if let Some(ts) = read_first_timestamp(&path) {
                entries.push((ts, sid, path));
            }
        }
        Self { entries }
    }

    /// Find the orphan whose first entry is within `max_delta` ms of `clear_ts`.
    fn find_by_clear_ts(&self, clear_ts: u64) -> Option<(&str, &Path)> {
        let max_delta = 30_000u64;
        let mut best: Option<(usize, u64)> = None;
        for (i, (first_ts, _, _)) in self.entries.iter().enumerate() {
            if *first_ts < clear_ts {
                continue;
            }
            let delta = first_ts - clear_ts;
            if delta > max_delta {
                continue;
            }
            if best.as_ref().map_or(true, |(_, d)| delta < *d) {
                best = Some((i, delta));
            }
        }
        best.map(|(i, _)| {
            let (_, ref sid, ref path) = self.entries[i];
            (sid.as_str(), path.as_path())
        })
    }

    /// Follow the /clear chain starting from `start_sid` until we reach a
    /// session ID that was NOT cleared.  Returns the JSONL path at the end
    /// of the chain, or None if the chain is broken.
    fn follow_chain(
        &self,
        start_sid: &str,
        clears: &HashMap<String, u64>,
    ) -> Option<PathBuf> {
        let mut current_sid = start_sid.to_string();
        let mut visited = HashSet::new();

        loop {
            let clear_ts = match clears.get(&current_sid) {
                Some(&ts) => ts,
                None => {
                    // current_sid was NOT cleared — it's the head of the chain.
                    // Find its JSONL path in our index.
                    return self
                        .entries
                        .iter()
                        .find(|(_, sid, _)| sid == &current_sid)
                        .map(|(_, _, p)| p.clone());
                }
            };

            visited.insert(current_sid.clone());

            match self.find_by_clear_ts(clear_ts) {
                Some((next_sid, _)) => {
                    if visited.contains(next_sid) {
                        return None; // cycle
                    }
                    current_sid = next_sid.to_string();
                }
                None => return None, // broken chain
            }
        }
    }
}

/// Read the first timestamp (epoch ms) from a JSONL file.
/// Only reads the first ~4 KB to keep it fast.
fn read_first_timestamp(path: &Path) -> Option<u64> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    // Only check first ~20 lines (well within 4 KB).
    for line in reader.lines().take(20) {
        let line = line.ok()?;
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(ts) = obj.get("timestamp") {
                return conversation::parse_timestamp_ms(ts);
            }
        }
    }
    None
}

fn is_pid_alive(pid: u32) -> bool {
    // Check that the process exists AND is actually a claude process.
    // This avoids false positives from PID reuse (another process gets the
    // same PID after claude exits).
    if !Process::is_alive(pid) {
        debug!("pid {} not alive (kill(0) failed)", pid);
        return false;
    }
    if !Process::is_claude(pid) {
        debug!("pid {} alive but not claude (name={})", pid, Process::name(pid));
        return false;
    }
    // A claude process reparented to init (ppid=1) is an orphan from a
    // killed terminal — it will never receive input again, so treat it
    // as dead.
    if !has_real_parent(pid) {
        debug!("pid {} is claude but orphaned (reparented to init)", pid);
        return false;
    }
    true
}

fn read_raw_sessions() -> Vec<RawSession> {
    let dir = match sessions_dir() {
        Some(d) => d,
        None => {
            warn!("sessions dir not found");
            return Vec::new();
        }
    };

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("cannot read sessions dir: {}", e);
            return Vec::new();
        }
    };

    // Collect all sessions, then deduplicate by session_id.
    // When a session is resumed, Claude Code creates a new file with the same
    // session_id but a different PID. We keep the entry whose process is still
    // alive, or the most recently started one if both are dead.
    let mut by_session_id = HashMap::<String, RawSession>::new();
    let mut file_count = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        file_count += 1;
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(raw) = serde_json::from_str::<RawSession>(&contents) {
                debug!(
                    "session file: pid={} sid={} cwd={}",
                    raw.pid, short_sid(&raw.session_id), raw.cwd
                );
                let keep_existing = match by_session_id.get(&raw.session_id) {
                    Some(existing) => {
                        let existing_alive = is_pid_alive(existing.pid);
                        let new_alive = is_pid_alive(raw.pid);
                        let result = match (existing_alive, new_alive) {
                            (false, true) => false, // new is alive, replace
                            (true, false) => true,  // existing is alive, skip
                            // Both alive: prefer the one with a real parent
                            // (i.e. still attached to a terminal window).
                            (true, true) => match (has_real_parent(existing.pid), has_real_parent(raw.pid)) {
                                (true, false) => true,
                                (false, true) => false,
                                _ => existing.started_at >= raw.started_at,
                            },
                            // Both dead: keep the most recently started.
                            (false, false) => existing.started_at >= raw.started_at,
                        };
                        debug!(
                            "dedup sid={}: existing pid={} alive={} vs new pid={} alive={} → {}",
                            short_sid(&raw.session_id),
                            existing.pid, existing_alive,
                            raw.pid, new_alive,
                            if result { "keep existing" } else { "replace" }
                        );
                        result
                    }
                    None => false,
                };
                if !keep_existing {
                    by_session_id.insert(raw.session_id.clone(), raw);
                }
            }
        }
    }

    debug!("read_raw_sessions: {} files, {} unique sessions", file_count, by_session_id.len());
    by_session_id.into_values().collect()
}

/// Extracted JSONL data for a session, avoiding a 7-element tuple.
struct JsonlData {
    state: SessionState,
    last_user_message: Option<String>,
    last_activity: Option<u64>,
    git_branch: Option<String>,
    model: Option<String>,
    version: Option<String>,
    summary: Option<String>,
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Build a [`SessionInfo`] from an orphan JSONL alone — no metadata file, no
/// live process. `cwd` is taken from the first JSONL entry that carries one;
/// the encoded directory name isn't losslessly decodable, so files without a
/// usable `cwd` entry are skipped.
fn synthesize_inactive_from_jsonl(path: &Path) -> Option<SessionInfo> {
    let session_id = path.file_stem().and_then(|s| s.to_str())?.to_string();
    let head_entries = conversation::read_jsonl_head(path, 4096);

    let cwd = head_entries
        .iter()
        .find_map(|e| e.get("cwd").and_then(|c| c.as_str()))?
        .to_string();

    let started_at = head_entries
        .iter()
        .find_map(|e| e.get("timestamp").and_then(conversation::parse_timestamp_ms))
        .unwrap_or(0);

    let tail_entries = conversation::read_jsonl_tail_for_state(path);
    let last_user_message = conversation::extract_last_user_message(&tail_entries);
    let last_activity = conversation::extract_last_activity(&tail_entries);
    let (git_branch, model, version) = conversation::extract_metadata(&tail_entries);
    let summary = conversation::extract_first_user_message(&head_entries);

    Some(SessionInfo {
        pid: 0,
        session_id,
        project_name: project_name(&cwd),
        cwd,
        started_at,
        last_activity,
        state: SessionState::Inactive,
        last_user_message,
        summary,
        model,
        git_branch,
        version,
        jsonl_path: Some(path.to_path_buf()),
        tmux_session: None,
    })
}

/// Walk `~/.claude/projects/**/*.jsonl` and synthesize Inactive sessions for
/// any JSONL touched within `INACTIVE_WINDOW_SECS` whose path isn't already
/// represented by an alive session. Caps each project at
/// [`INACTIVE_MAX_PER_PROJECT`] (ranked by mtime) before parsing JSONLs so a
/// project with dozens of touched files doesn't dominate a scan tick.
///
/// Returns `(sessions, total_in_window)` — the count reflects how many files
/// were eligible before the per-project cap.
fn scan_orphan_jsonls(claimed_paths: &HashSet<PathBuf>) -> (Vec<SessionInfo>, usize) {
    let Some(projects) = projects_dir() else {
        return (Vec::new(), 0);
    };
    let Ok(project_dirs) = std::fs::read_dir(&projects) else {
        return (Vec::new(), 0);
    };

    let mut out = Vec::new();
    let mut total_in_window = 0usize;
    for proj in project_dirs.flatten() {
        let Ok(files) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        let mut candidates: Vec<(PathBuf, SystemTime)> = Vec::new();
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if claimed_paths.contains(&path) {
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
            if age > INACTIVE_WINDOW_SECS {
                continue;
            }
            candidates.push((path, mtime));
        }
        total_in_window += candidates.len();
        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        for (path, _) in candidates.into_iter().take(INACTIVE_MAX_PER_PROJECT) {
            if let Some(info) = synthesize_inactive_from_jsonl(&path) {
                out.push(info);
            }
        }
    }
    (out, total_in_window)
}

pub fn scan_sessions() -> Vec<SessionInfo> {
    let raw_sessions = read_raw_sessions();
    let clears = read_clears_from_history();
    // Derive claimed session IDs from the sessions we already read,
    // avoiding a redundant second pass over the session metadata files.
    let claimed: HashSet<String> = raw_sessions.iter().map(|r| r.session_id.clone()).collect();

    debug!(
        "scan_sessions: {} raw sessions, {} clears, {} claimed ids",
        raw_sessions.len(), clears.len(), claimed.len()
    );

    let alive_by_pid: HashMap<u32, bool> = raw_sessions
        .iter()
        .map(|r| (r.pid, is_pid_alive(r.pid)))
        .collect();
    let alive_count = alive_by_pid.values().filter(|&&v| v).count();

    info!(
        "scan_sessions: {} raw → {} alive, {} dead (subject to inactive window)",
        raw_sessions.len(),
        alive_count,
        raw_sessions.len() - alive_count
    );

    let jsonl_map = resolve_jsonl_paths(&raw_sessions, &clears, &claimed);

    // Snapshot tmux once per scan so we can tag each session with its hosting
    // tmux session name (if any) without reshelling per pid.
    let tmux_panes = crate::send::tmux_panes();

    let mut sessions: Vec<SessionInfo> = raw_sessions
        .into_iter()
        .filter_map(|raw| {
            let is_alive = alive_by_pid.get(&raw.pid).copied().unwrap_or(false);
            let jsonl_path = jsonl_map.get(&raw.session_id).cloned().flatten();
            if !is_alive {
                let within_window = jsonl_path
                    .as_deref()
                    .and_then(mtime_age_secs)
                    .is_some_and(|s| s <= INACTIVE_WINDOW_SECS);
                if !within_window {
                    return None;
                }
            }
            Some((raw, is_alive, jsonl_path))
        })
        .map(|(raw, is_alive, jsonl_path)| {
            let sid_short = short_sid(&raw.session_id);

            let was_cleared = clears.contains_key(&raw.session_id);
            debug!(
                "pid={} sid={} cwd={} alive={} cleared={} jsonl={}",
                raw.pid, sid_short, raw.cwd, is_alive, was_cleared,
                jsonl_path.as_ref().map_or("none".to_string(), |p| p.display().to_string())
            );

            let mut data = match &jsonl_path {
                Some(path) => {
                    let entries = conversation::read_jsonl_tail_for_state(path);
                    let mtime_age_secs = path.metadata().ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok())
                        .map(|d| d.as_secs());
                    let mut state = conversation::extract_state(&entries, mtime_age_secs);
                    let last_msg = conversation::extract_last_user_message(&entries);
                    let last_act = conversation::extract_last_activity(&entries);
                    let (branch, mdl, ver) = conversation::extract_metadata(&entries);
                    let head_entries = conversation::read_jsonl_head(path, 4096);
                    let summary = conversation::extract_first_user_message(&head_entries);

                    debug!(
                        "  sid={} tail_entries={} raw_state={} last_activity={:?}",
                        sid_short, entries.len(), state, last_act
                    );

                    // If the JSONL was modified very recently but state
                    // reads as Idle, the assistant likely hasn't written
                    // its first response yet (e.g. right after a slash
                    // command). Upgrade to Processing.
                    if is_alive
                        && state == SessionState::Idle
                        && mtime_age_secs.is_some_and(|s| s < 30)
                    {
                        debug!(
                            "  sid={} upgrading Idle→Processing (mtime age={}s)",
                            sid_short,
                            mtime_age_secs.unwrap()
                        );
                        state = SessionState::Processing;
                    }

                    debug!(
                        "  sid={} final_state={} model={:?} branch={:?}",
                        sid_short, state, mdl, branch
                    );

                    JsonlData { state, last_user_message: last_msg, last_activity: last_act, git_branch: branch, model: mdl, version: ver, summary }
                }
                None => {
                    debug!("  sid={} no jsonl → Idle", sid_short);
                    JsonlData { state: SessionState::Idle, last_user_message: None, last_activity: None, git_branch: None, model: None, version: None, summary: None }
                }
            };

            if !is_alive {
                data.state = SessionState::Inactive;
            }

            let tmux_session = if is_alive {
                crate::send::tmux_session_for_pid_in(raw.pid, &tmux_panes)
            } else {
                None
            };

            SessionInfo {
                pid: raw.pid,
                session_id: raw.session_id,
                project_name: project_name(&raw.cwd),
                cwd: raw.cwd,
                started_at: raw.started_at,
                last_activity: data.last_activity,
                state: data.state,
                last_user_message: data.last_user_message,
                summary: data.summary,
                model: data.model,
                git_branch: data.git_branch,
                version: data.version,
                jsonl_path,
                tmux_session,
            }
        })
        .collect();

    let claimed_paths: HashSet<PathBuf> = sessions
        .iter()
        .filter_map(|s| s.jsonl_path.clone())
        .collect();
    let (orphans, total_in_window) = scan_orphan_jsonls(&claimed_paths);
    info!(
        "scan_sessions: {} from metadata + {} orphan JSONLs (of {} within window, capped at {} per project)",
        sessions.len(),
        orphans.len(),
        total_in_window,
        INACTIVE_MAX_PER_PROJECT
    );
    sessions.extend(orphans);

    sessions.sort_by(|a, b| {
        a.state
            .sort_key()
            .cmp(&b.state.sort_key())
            .then_with(|| b.started_at.cmp(&a.started_at))
    });

    sessions
}

pub fn load_detail(session_id: &str, sessions: &[SessionInfo]) -> Option<SessionDetail> {
    let info = sessions.iter().find(|s| s.session_id == session_id)?;
    let jsonl_path = info.jsonl_path.as_ref()?;
    let entries = conversation::read_jsonl_tail(&jsonl_path, 65536);

    let recent_messages = conversation::extract_messages(&entries, 15);
    let (total_input_tokens, total_output_tokens) = conversation::extract_token_totals(&entries);

    Some(SessionDetail {
        info: info.clone(),
        recent_messages,
        total_input_tokens,
        total_output_tokens,
    })
}

pub fn load_state_explanation(
    session_id: &str,
    sessions: &[SessionInfo],
) -> Option<(SessionInfo, conversation::StateExplanation)> {
    let info = sessions.iter().find(|s| s.session_id == session_id)?;
    let jsonl_path = info.jsonl_path.as_ref()?;
    let entries = conversation::read_jsonl_tail_for_state(jsonl_path);
    let mtime_age_secs = jsonl_path
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs());
    Some((info.clone(), conversation::explain_state(&entries, mtime_age_secs)))
}
