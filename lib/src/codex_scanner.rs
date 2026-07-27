//! Discovery of Codex CLI sessions from `~/.codex/sessions/**/rollout-*.jsonl`.
//!
//! Codex writes a rich transcript to disk like Claude, so per-session fields
//! (state, model, tokens, current tool) are derived from the rollout by
//! [`crate::codex_conversation`]. Unlike Claude it writes no live status file,
//! so liveness comes from a process scan: enumerate running `codex` processes,
//! then bind each process to the rollout file it holds open (or the explicit
//! UUID in `codex resume`). Cwd is only a validation constraint because many
//! concurrent sessions commonly share one project directory.

use crate::agent::{AgentConfig, AgentKind};
use crate::codex_conversation;
use crate::config;
use crate::conversation;
use crate::models::{SessionDetail, SessionInfo, SessionState};
use crate::platform::paths;
use crate::platform::process;
use crate::send;
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// `session_meta` (rollout line 1) embeds the full codex system prompt in
/// `base_instructions.text` — up to ~15 KiB observed — so the head read must be
/// generous enough to capture that whole first line (else it is dropped as a
/// partial trailing line and cwd/session id go missing).
const HEAD_BYTES: u64 = 64 * 1024;

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn session_root() -> Option<PathBuf> {
    paths::codex_sessions_dir()
}

fn default_codex_agent(agents: &[AgentConfig]) -> Option<AgentConfig> {
    agents.iter().find(|a| a.kind == AgentKind::Codex).cloned()
}

fn mtime_age_secs(path: &Path) -> Option<u64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
        .map(|d| d.as_secs())
}

fn read_head(path: &Path) -> Vec<Value> {
    conversation::read_jsonl_head(path, HEAD_BYTES)
}

/// Recover a session id from a `rollout-<ts>-<uuid>.jsonl` filename when the
/// `session_meta` record is unreadable. The uuid is the trailing five
/// dash-groups (`8-4-4-4-12`).
fn session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() < 5 {
        return None;
    }
    Some(parts[parts.len() - 5..].join("-"))
}

/// Recursively collect `rollout-*.jsonl` files under `root` with their mtimes.
/// Codex nests them three levels deep by date (`YYYY/MM/DD/`), so a flat
/// `read_dir` (as the Pi scanner uses) would miss them.
fn walk_rollouts(root: &Path) -> Vec<(PathBuf, SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            let is_rollout = path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rollout-"));
            if !is_rollout {
                continue;
            }
            if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                out.push((path, mtime));
            }
        }
    }
    out
}

/// Index every rollout modified within the inactive window by the cwd its
/// `session_meta` records, newest first. Live ownership considers every path;
/// this filtered index only controls which unclaimed rollouts become inactive
/// cards.
fn index_recent_by_cwd(
    rollouts: &[(PathBuf, SystemTime)],
    window_secs: u64,
) -> HashMap<String, Vec<(PathBuf, SystemTime)>> {
    let mut by_cwd: HashMap<String, Vec<(PathBuf, SystemTime)>> = HashMap::new();
    for (path, mtime) in rollouts {
        // A live transcript is written "now", so its mtime can equal or slightly
        // lead the wall clock — `elapsed()` then errors. Treat unknown/future
        // mtimes as recent (only drop a file we can positively age past the
        // window), else a freshly-written session gets filtered out.
        let too_old = mtime
            .elapsed()
            .ok()
            .map(|d| d.as_secs())
            .is_some_and(|age| age > window_secs);
        if too_old {
            continue;
        }
        let head = read_head(path);
        if let Some(cwd) = codex_conversation::extract_cwd(&head) {
            by_cwd.entry(cwd).or_default().push((path.clone(), *mtime));
        }
    }
    for files in by_cwd.values_mut() {
        files.sort_by_key(|b| Reverse(b.1));
    }
    by_cwd
}

#[derive(Clone, Debug)]
struct LiveProcess {
    pid: u32,
    cwd: String,
    tmux_session: Option<String>,
    resume_session_id: Option<String>,
}

fn rollout_bindings() -> &'static Mutex<HashMap<u32, PathBuf>> {
    static BINDINGS: OnceLock<Mutex<HashMap<u32, PathBuf>>> = OnceLock::new();
    BINDINGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve live processes without guessing from cwd/mtime ordering.
///
/// Open-file ownership and an explicit `codex resume <uuid>` are authoritative.
/// A prior binding is retained while both process and rollout remain present,
/// covering transient ownership-query failures. An unresolved process gets a
/// process-only card until Codex opens its rollout.
fn resolve_live_rollouts(
    live: &[LiveProcess],
    all_rollouts: &HashSet<PathBuf>,
    open_files: &HashMap<u32, PathBuf>,
    previous: &HashMap<u32, PathBuf>,
) -> HashMap<u32, PathBuf> {
    let mut by_session_id = HashMap::<String, PathBuf>::new();
    for path in all_rollouts {
        if let Some(sid) = session_id_from_filename(path) {
            by_session_id.insert(sid, path.clone());
        }
    }

    let mut resolved = HashMap::new();
    let mut claimed = HashSet::<PathBuf>::new();

    for process in live {
        let Some(path) = open_files.get(&process.pid) else {
            continue;
        };
        if all_rollouts.contains(path) && claimed.insert(path.clone()) {
            resolved.insert(process.pid, path.clone());
        }
    }

    for process in live {
        if resolved.contains_key(&process.pid) {
            continue;
        }
        let Some(path) = process
            .resume_session_id
            .as_ref()
            .and_then(|sid| by_session_id.get(sid))
        else {
            continue;
        };
        if claimed.insert(path.clone()) {
            resolved.insert(process.pid, path.clone());
        }
    }

    for process in live {
        if resolved.contains_key(&process.pid) {
            continue;
        }
        let Some(path) = previous.get(&process.pid) else {
            continue;
        };
        let belongs_to_cwd = all_rollouts.contains(path)
            && codex_conversation::extract_cwd(&read_head(path)).as_deref()
                == Some(process.cwd.as_str());
        if belongs_to_cwd && claimed.insert(path.clone()) {
            resolved.insert(process.pid, path.clone());
        }
    }

    resolved
}

fn build_session_info(
    agent_id: String,
    pid: u32,
    tmux_session: Option<String>,
    state_hint: SessionState,
    path: PathBuf,
) -> Option<SessionInfo> {
    let head = read_head(&path);
    let cwd = codex_conversation::extract_cwd(&head)?;
    let session_id = codex_conversation::extract_session_id(&head)
        .or_else(|| session_id_from_filename(&path))?;
    let started_at = codex_conversation::extract_started_at(&head);
    let summary = codex_conversation::extract_first_user_message(&head);

    let tail = codex_conversation::read_jsonl_tail_for_state(&path);
    let mut state = codex_conversation::extract_state(&tail);
    match state_hint {
        // The scanner's own verdict wins for these two: a dead process forces
        // Inactive; a caller that already knows the turn is running forces
        // Processing. For everything else the transcript-derived state stands.
        SessionState::Inactive => state = SessionState::Inactive,
        SessionState::Processing => state = SessionState::Processing,
        SessionState::Idle
        | SessionState::WaitingForInput
        | SessionState::Question
        | SessionState::Starting => {}
    }

    let last_user_message = codex_conversation::extract_last_user_message(&tail);
    let last_activity = codex_conversation::extract_last_activity(&tail);
    // The latest model lives in the newest `turn_context` (tail); `cli_version`
    // and the git branch live in `session_meta` (head). Fall back to the head's
    // first turn_context for a session whose only turn is still within the head
    // window.
    let (_, model_tail, _) = codex_conversation::extract_metadata(&tail);
    let (git_branch, model_head, version) = codex_conversation::extract_metadata(&head);
    let model = model_tail.or(model_head);

    let tool_uses_count = crate::tool_use_count::count_codex(&path);
    Some(SessionInfo {
        agent_id,
        agent_kind: AgentKind::Codex,
        pid,
        session_id,
        cwd: cwd.clone(),
        project_name: project_name(&cwd),
        started_at,
        last_activity,
        state,
        last_user_message,
        summary,
        title: None,
        titling: false,
        model,
        git_branch,
        version,
        jsonl_path: Some(path),
        tmux_session,
        current_tool: codex_conversation::extract_current_tool(&tail),
        is_thinking: codex_conversation::is_currently_thinking(&tail),
        context_tokens: codex_conversation::extract_context_tokens(&tail),
        tool_uses_count,
    })
}

/// A card for a live interactive codex process that has not written a rollout
/// yet. The session id is synthesized from the pid — stable for the process's
/// lifetime, and superseded by the real rollout uuid the moment a transcript
/// appears (at which point the pairing branch takes over and this card is not
/// produced). The model is recovered from the `-m` launch flag.
fn process_only_card(
    agent_id: &str,
    pid: u32,
    tmux_session: Option<String>,
    cwd: String,
) -> SessionInfo {
    SessionInfo {
        agent_id: agent_id.to_string(),
        agent_kind: AgentKind::Codex,
        pid,
        session_id: format!("codex-{}", pid),
        project_name: project_name(&cwd),
        cwd,
        started_at: 0,
        last_activity: None,
        state: SessionState::Idle,
        last_user_message: None,
        summary: None,
        title: None,
        titling: false,
        model: process::codex_model_arg(pid),
        git_branch: None,
        version: None,
        jsonl_path: None,
        tmux_session,
        current_tool: None,
        is_thinking: false,
        context_tokens: None,
        tool_uses_count: 0,
    }
}

pub fn scan(agents: &[AgentConfig], titles: &HashMap<String, String>) -> Vec<SessionInfo> {
    let Some(default_agent) = default_codex_agent(agents) else {
        return Vec::new();
    };
    let Some(root) = session_root() else {
        return Vec::new();
    };
    let cfg = &config::get().inactive;
    let rollouts = walk_rollouts(&root);
    let by_cwd = index_recent_by_cwd(&rollouts, cfg.window_secs);

    let mut sessions = Vec::new();

    // Discover the live process set before resolving ownership. Several Codex
    // sessions commonly share one cwd, so cwd is only a validation constraint,
    // never an identity signal.
    let tmux_panes = send::tmux_panes();
    let mut pids = process::list_pids();
    pids.sort_unstable_by(|a, b| b.cmp(a));
    let mut live = Vec::new();
    for pid in pids {
        if !process::is_agent_process(AgentKind::Codex, pid) {
            continue;
        }
        let Some(cwd) = process::current_dir(pid) else {
            continue;
        };
        live.push(LiveProcess {
            pid,
            cwd,
            tmux_session: send::tmux_session_for_pid_in(pid, &tmux_panes),
            resume_session_id: process::codex_resume_session_arg(pid),
        });
    }

    let candidate_paths: Vec<PathBuf> = rollouts.iter().map(|(path, _)| path.clone()).collect();
    let all_rollouts: HashSet<PathBuf> = candidate_paths.iter().cloned().collect();
    let live_pids: Vec<u32> = live.iter().map(|process| process.pid).collect();
    let open_files = process::open_codex_rollouts(&live_pids, &candidate_paths);
    let previous = rollout_bindings()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let resolved = resolve_live_rollouts(&live, &all_rollouts, &open_files, &previous);
    *rollout_bindings().lock().unwrap_or_else(|e| e.into_inner()) = resolved.clone();
    let claimed: HashSet<PathBuf> = resolved.values().cloned().collect();

    for process in live {
        match resolved.get(&process.pid) {
            Some(path) => {
                if let Some(info) = build_session_info(
                    default_agent.id.clone(),
                    process.pid,
                    process.tmux_session,
                    SessionState::Idle,
                    path.clone(),
                ) {
                    sessions.push(info);
                }
            }
            // Interactive codex writes no rollout until its first turn, so a
            // freshly-spawned idle session has a live process but nothing on
            // disk. Surface it straight from the process (like Pi's heartbeat
            // card) so it appears immediately and the spawn watchdog clears; a
            // later tick upgrades it to the transcript-derived card once the
            // rollout lands.
            None => sessions.push(process_only_card(
                &default_agent.id,
                process.pid,
                process.tmux_session,
                process.cwd,
            )),
        }
    }

    // Inactive phase: the remaining recent rollouts, capped per cwd like the
    // Claude/Pi orphan walks so one busy directory can't flood the grid.
    for files in by_cwd.values() {
        let mut kept = 0usize;
        for (path, _) in files {
            if claimed.contains(path) {
                continue;
            }
            if kept >= cfg.max_per_project {
                break;
            }
            kept += 1;
            if let Some(info) = build_session_info(
                default_agent.id.clone(),
                0,
                None,
                SessionState::Inactive,
                path.clone(),
            ) {
                sessions.push(info);
            }
        }
    }

    for session in sessions.iter_mut() {
        // PID-backed process-only cards are temporary identities. Never read a
        // persisted title for them: an old `codex-<pid>` cache entry could be
        // inherited by an unrelated process after PID reuse.
        if session.title.is_none() && session.jsonl_path.is_some() {
            session.title = titles.get(&session.session_id).cloned();
        }
    }
    sessions
}

pub fn load_detail(info: &SessionInfo) -> Option<SessionDetail> {
    let path = info.jsonl_path.as_ref()?;
    let entries = conversation::read_jsonl_tail(path, 65536);
    let recent_messages = codex_conversation::extract_messages(&entries, 15);
    let (total_input_tokens, total_output_tokens) =
        codex_conversation::extract_token_totals(&entries);
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
    let entries = codex_conversation::read_jsonl_tail_for_state(path);
    let mtime_age = mtime_age_secs(path);
    Some((
        info.clone(),
        codex_conversation::explain_state(&entries, mtime_age),
    ))
}

/// Resolve a codex session id to resume for a project/task orchestrator. Codex
/// resumes by session UUID (`codex resume <uuid>`), so this returns the id, not
/// a file path. Fast-path trusts a previously recorded sid; the fallback scans
/// rollouts in the project cwd for the orchestrator prompt prefix.
pub fn find_orchestrator_session(
    project_root: &Path,
    task_id: &str,
    stored_sid: Option<&str>,
) -> Option<String> {
    if let Some(sid) = stored_sid {
        // Trust the recorded sid: `codex resume <uuid>` locates the rollout by
        // id regardless of which date directory it lives in.
        return Some(sid.to_string());
    }

    let root = session_root()?;
    let root_cwd = project_root.to_string_lossy().to_string();
    let needle = crate::orchestrator::orchestrator_prompt_prefix(task_id);
    let mut best: Option<(SystemTime, String)> = None;

    use std::io::Read;
    for (path, mtime) in walk_rollouts(&root) {
        if best.as_ref().is_some_and(|(t, _)| mtime <= *t) {
            continue;
        }
        // Cheap pre-filter: read a chunk and require both the prompt prefix and
        // this session's cwd before parsing the head for the id.
        let mut buf = vec![0u8; 64 * 1024];
        let Ok(n) = std::fs::File::open(&path).and_then(|mut f| f.read(&mut buf)) else {
            continue;
        };
        buf.truncate(n);
        let text = String::from_utf8_lossy(&buf);
        if !text.contains(&needle) || !text.contains(&root_cwd) {
            continue;
        }
        let head = read_head(&path);
        if codex_conversation::extract_cwd(&head).as_deref() != Some(root_cwd.as_str()) {
            continue;
        }
        if let Some(sid) = codex_conversation::extract_session_id(&head)
            .or_else(|| session_id_from_filename(&path))
        {
            best = Some((mtime, sid));
        }
    }
    best.map(|(_, sid)| sid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;
    use std::fs;
    use std::io::Write;

    fn with_temp_home<T>(f: impl FnOnce(&Path) -> T) -> T {
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

    fn write_rollout(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", line).unwrap();
        }
        path
    }

    #[test]
    fn session_id_from_filename_takes_trailing_uuid() {
        let p = PathBuf::from(
            "/x/rollout-2026-07-14T16-22-22-019f60ca-f883-7a10-a997-5c17d556ee79.jsonl",
        );
        assert_eq!(
            session_id_from_filename(&p).as_deref(),
            Some("019f60ca-f883-7a10-a997-5c17d556ee79")
        );
    }

    #[test]
    fn walk_rollouts_finds_date_nested_files() {
        with_temp_home(|home| {
            let root = home.join(".codex/sessions");
            write_rollout(
                &root.join("2026/07/14"),
                "rollout-2026-07-14T16-22-22-019f60ca-f883-7a10-a997-5c17d556ee79.jsonl",
                &[r#"{"type":"session_meta","payload":{"session_id":"s1","cwd":"/tmp/p"}}"#],
            );
            // A non-rollout jsonl in the tree must be ignored.
            write_rollout(&root.join("2026/07/14"), "history.jsonl", &["{}"]);
            let found = walk_rollouts(&root);
            assert_eq!(found.len(), 1, "only the rollout file is collected");
        });
    }

    #[test]
    fn inactive_scan_surfaces_disk_session() {
        with_temp_home(|home| {
            let root = home.join(".codex/sessions/2026/07/14");
            write_rollout(
                &root,
                "rollout-2026-07-14T16-22-22-019f60ca-f883-7a10-a997-5c17d556ee79.jsonl",
                &[
                    r#"{"timestamp":"2026-07-14T13:22:22.467Z","type":"session_meta","payload":{"session_id":"019f60ca-f883-7a10-a997-5c17d556ee79","cwd":"/tmp/proj","cli_version":"0.144.3"}}"#,
                    r#"{"timestamp":"2026-07-14T13:22:47.957Z","type":"event_msg","payload":{"type":"user_message","message":"do the thing"}}"#,
                    r#"{"timestamp":"2026-07-14T13:22:48.000Z","type":"turn_context","payload":{"model":"gpt-5.6-luna"}}"#,
                    r#"{"timestamp":"2026-07-14T13:23:05.826Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1"}}"#,
                ],
            );
            let agent = AgentConfig {
                id: "codex".into(),
                kind: AgentKind::Codex,
                command: "codex".into(),
                use_bridge: false,
                models: Vec::new(),
            };
            // Find the disk session by id rather than asserting the total
            // count: scan()'s live phase enumerates real system processes, so a
            // codex session running on the test host would add extra cards.
            let sessions = scan(&[agent], &HashMap::new());
            let s = sessions
                .iter()
                .find(|s| s.session_id == "019f60ca-f883-7a10-a997-5c17d556ee79")
                .expect("disk session surfaced");
            assert_eq!(s.agent_kind, AgentKind::Codex);
            assert_eq!(s.cwd, "/tmp/proj");
            assert_eq!(s.model.as_deref(), Some("gpt-5.6-luna"));
            assert_eq!(s.version.as_deref(), Some("0.144.3"));
            assert_eq!(s.summary.as_deref(), Some("do the thing"));
            // No live process owns this rollout → it surfaces as Inactive
            // (kept for resume within the window), exactly like a dead
            // Claude/Pi session. Live liveness is a process-scan concern the
            // inactive phase deliberately doesn't assert here.
            assert_eq!(s.state, SessionState::Inactive);
        });
    }

    #[test]
    fn scan_returns_empty_without_a_codex_agent() {
        with_temp_home(|_| {
            let agent = AgentConfig {
                id: "claude".into(),
                kind: AgentKind::Claude,
                command: "claude".into(),
                use_bridge: false,
                models: Vec::new(),
            };
            assert!(scan(&[agent], &HashMap::new()).is_empty());
        });
    }

    #[test]
    fn live_rollouts_follow_open_files_not_pid_or_mtime_order() {
        let cwd = "/tmp/shared".to_string();
        let older = PathBuf::from(
            "/x/rollout-2026-07-24T18-00-00-019f94b1-ab14-7b92-acfe-094b7c66d830.jsonl",
        );
        let newer = PathBuf::from(
            "/x/rollout-2026-07-25T00-00-00-019f95ff-234b-7541-8673-53a534e26254.jsonl",
        );
        let live = vec![
            LiveProcess {
                pid: 900,
                cwd: cwd.clone(),
                tmux_session: None,
                resume_session_id: None,
            },
            LiveProcess {
                pid: 100,
                cwd,
                tmux_session: None,
                resume_session_id: None,
            },
        ];
        let open = HashMap::from([(900, older.clone()), (100, newer.clone())]);
        let all = HashSet::from([older.clone(), newer.clone()]);

        let resolved = resolve_live_rollouts(&live, &all, &open, &HashMap::new());

        assert_eq!(resolved.get(&900), Some(&older));
        assert_eq!(resolved.get(&100), Some(&newer));
    }

    #[test]
    fn unresolved_same_cwd_process_is_not_given_someone_elses_rollout() {
        let cwd = "/tmp/shared".to_string();
        let path = PathBuf::from(
            "/x/rollout-2026-07-25T00-00-00-019f95ff-234b-7541-8673-53a534e26254.jsonl",
        );
        let live = vec![
            LiveProcess {
                pid: 900,
                cwd: cwd.clone(),
                tmux_session: None,
                resume_session_id: None,
            },
            LiveProcess {
                pid: 100,
                cwd,
                tmux_session: None,
                resume_session_id: None,
            },
        ];
        let open = HashMap::from([(100, path.clone())]);
        let all = HashSet::from([path.clone()]);

        let resolved = resolve_live_rollouts(&live, &all, &open, &HashMap::new());

        assert_eq!(resolved.get(&100), Some(&path));
        assert!(!resolved.contains_key(&900));
    }
}
