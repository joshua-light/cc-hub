use super::paths::{is_scratch_cwd, projects_dir, scratch_project_dir_name};
use crate::agent::AgentKind;
use crate::config;
use crate::conversation;
use crate::models::{SessionInfo, SessionState};
use crate::sessions::common::{project_name, recent_unclaimed};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Build a [`SessionInfo`] from an orphan JSONL alone — no metadata file, no
/// live process. `cwd` is taken from the first JSONL entry that carries one;
/// the encoded directory name isn't losslessly decodable, so files without a
/// usable `cwd` entry are skipped. Files whose cwd matches
/// [`crate::title::scratch_cwd`] are also skipped — those are the one-shot
/// `cc-hub-new -p` runs the titler fires, not real sessions.
fn synthesize_inactive_from_jsonl(
    path: &Path,
    titles: &HashMap<String, String>,
) -> Option<SessionInfo> {
    let session_id = path.file_stem().and_then(|s| s.to_str())?.to_string();
    let head_entries = conversation::read_jsonl_head(path, 4096);

    let cwd = head_entries
        .iter()
        .find_map(|e| e.get("cwd").and_then(|c| c.as_str()))?
        .to_string();

    if is_scratch_cwd(&cwd) {
        return None;
    }

    let started_at = head_entries
        .iter()
        .find_map(|e| {
            e.get("timestamp")
                .and_then(conversation::parse_timestamp_ms)
        })
        .unwrap_or(0);

    // Memoized on (path, mtime); the immutable summary is cached permanently.
    let derived = conversation::derive_state_cached(path)?;
    let summary = conversation::first_user_message_cached(path);
    let title = titles.get(&session_id).cloned();

    let tool_uses_count = crate::conversation::tool_count::count_claude(path);
    Some(SessionInfo {
        agent_id: "claude".into(),
        agent_kind: AgentKind::Claude,
        pid: 0,
        session_id,
        project_name: project_name(&cwd),
        cwd,
        started_at,
        last_activity: derived.last_activity,
        state: SessionState::Inactive,
        last_user_message: derived.last_user_message.clone(),
        summary,
        title,
        model: derived.model.clone(),
        git_branch: derived.git_branch.clone(),
        version: derived.version.clone(),
        jsonl_path: Some(path.to_path_buf()),
        tmux_session: None,
        current_tool: None,
        is_thinking: false,
        titling: false,
        context_tokens: derived.context_tokens,
        tool_uses_count,
    })
}

/// Walk `~/.claude/projects/**/*.jsonl` and synthesize Inactive sessions for
/// any JSONL touched within [`config::InactiveConfig::window_secs`] whose path
/// isn't already represented by an alive session. Caps each project at
/// [`config::InactiveConfig::max_per_project`] (ranked by mtime) before
/// parsing JSONLs so a project with dozens of touched files doesn't dominate
/// a scan tick.
///
/// Returns `(sessions, total_in_window)` — the count reflects how many files
/// were eligible before the per-project cap.
pub(super) fn scan_orphan_jsonls(
    claimed_paths: &HashSet<PathBuf>,
    titles: &HashMap<String, String>,
) -> (Vec<SessionInfo>, usize) {
    let cfg = &config::get().inactive;
    let relist_ttl = std::time::Duration::from_secs(cfg.orphan_relist_secs);
    let Some(projects) = projects_dir() else {
        return (Vec::new(), 0);
    };
    let Ok(project_dirs) = std::fs::read_dir(&projects) else {
        return (Vec::new(), 0);
    };

    // Encoded form of the titler's scratch cwd, e.g. `-tmp-cc-hub-summaries`.
    // Skipping this project dir up front avoids reading dozens of one-shot
    // `cc-hub-new -p` JSONLs every scan just to throw them away inside
    // `synthesize_inactive_from_jsonl`.
    let scratch_proj_dir = scratch_project_dir_name();

    let mut out = Vec::new();
    let mut total_in_window = 0usize;
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    for proj in project_dirs.flatten() {
        if let Some(skip) = scratch_proj_dir.as_deref() {
            if proj.file_name().to_str() == Some(skip) {
                continue;
            }
        }
        let proj_path = proj.path();
        // Cached per-dir listing: re-`read_dir`s only when the project dir's
        // mtime changes (a new/removed transcript) or the entry ages past the
        // relist TTL. Per-file mtimes come from the cached listing, so the age
        // filter below may be up to `orphan_relist_secs` stale — within the
        // TTL budget, and identical in every other respect to the old walk.
        let files = crate::sessions::dir_cache::list_jsonl_dir(&proj_path, relist_ttl);
        visited_dirs.insert(proj_path);
        let candidates = recent_unclaimed(&files, claimed_paths, cfg.window_secs);
        total_in_window += candidates.len();
        for path in candidates.into_iter().take(cfg.max_per_project) {
            if let Some(info) = synthesize_inactive_from_jsonl(&path, titles) {
                out.push(info);
            }
        }
    }
    // Drop cache entries for project dirs that disappeared since last tick,
    // scoped to the projects root so the shared cache keeps Pi's entries.
    crate::sessions::dir_cache::retain_under(&projects, &visited_dirs);
    (out, total_in_window)
}
