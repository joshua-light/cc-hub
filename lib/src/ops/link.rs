//! `cc-hub open` body: act on a parsed [`Link`].
//!
//! A review link becomes a fresh agent session in the local checkout of the
//! pull request's repository, named `PR: <title>` and opened with the link's
//! prompt. The checkout is found by name among the folders the hub already
//! knows — registered projects, then bookmarks, then the cwds of scanned
//! sessions — so a repo the user has ever worked in from the hub needs no
//! extra mapping.
//!
//! Naming happens *before* the spawn when the backend lets us pick the
//! session id (Claude's `--session-id`): the title is on disk before the
//! session exists, so the hub never sees it nameless and never opens the
//! rename prompt for it. Backends that mint their own id get named as soon
//! as the scanner can see them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::agent::AgentKind;
use crate::bookmarks::Bookmarks;
use crate::link::Link;
use crate::ops::worker::{wait_until_idle_and_send, PromptStatus, DEFAULT_PROMPT_WAIT_SECS};
use crate::ops::OpError;
use crate::spawn::SessionTarget;
use crate::{config, projects_scan, scanner, spawn, title};

/// Options for [`open`].
#[derive(Default)]
pub struct OpenOpts {
    /// Agent backend id; `None` → `[projects].default_session_agent`.
    pub agent: Option<String>,
    pub wait_secs: Option<u64>,
}

/// Where a link lands, resolved without side effects: the folder the session
/// starts in, the name and prompt it starts with, and the agent that runs
/// it. This is what `--dry-run` prints.
pub struct LinkTarget {
    pub cwd: PathBuf,
    pub title: String,
    pub prompt: String,
    pub agent_id: String,
}

/// Result of [`open`].
pub struct Opened {
    pub target: LinkTarget,
    pub tmux: String,
    pub session_id: Option<String>,
    pub prompt_status: PromptStatus,
}

/// Resolve `link` to its [`LinkTarget`] without spawning anything.
pub fn target(link: &Link, agent: Option<&str>) -> Result<LinkTarget, OpError> {
    let agent_id = agent
        .map(str::to_string)
        .unwrap_or_else(|| config::get().default_session_agent_id());
    config::get()
        .agent(&agent_id)
        .ok_or_else(|| OpError::Usage(format!("unknown agent id: {}", agent_id)))?;

    match link {
        Link::Review(review) => {
            let repo = review.pr.repo();
            let cwd = folder_named(repo).ok_or_else(|| {
                OpError::NotFound(format!(
                    "no known folder named `{}` — bookmark the checkout in cc-hub (or register it as a project) and retry",
                    repo
                ))
            })?;
            Ok(LinkTarget {
                cwd,
                title: review.session_title(),
                prompt: review.prompt(),
                agent_id,
            })
        }
    }
}

/// Spawn the session `link` asks for, name it, and deliver its prompt.
pub fn open(link: &Link, opts: OpenOpts) -> Result<Opened, OpError> {
    let target = self::target(link, opts.agent.as_deref())?;
    let agent = config::get()
        .agent(&target.agent_id)
        .ok_or_else(|| OpError::Usage(format!("unknown agent id: {}", target.agent_id)))?;
    let cwd = target.cwd.to_string_lossy().into_owned();
    let wait = Duration::from_secs(opts.wait_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS));

    // Claude lets us choose the id, so the name lands first.
    let chosen_id = (agent.kind == AgentKind::Claude).then(|| uuid::Uuid::new_v4().to_string());
    if let Some(sid) = &chosen_id {
        title::persist_title(sid, &target.title)
            .map_err(|e| OpError::Other(format!("persist title: {}", e)))?;
    }

    let initial_prompt = agent
        .supports_initial_prompt()
        .then_some(target.prompt.as_str());
    let session = chosen_id.clone().map(SessionTarget::Fresh);
    let tmux = spawn::spawn_agent_session(
        &target.agent_id,
        &cwd,
        session,
        initial_prompt,
        None,
        false,
    )
    .map_err(|e| OpError::Other(format!("spawn session: {}", e)))?;

    let session_id = match chosen_id {
        Some(sid) => Some(sid),
        None => name_once_visible(&tmux, &target.title, wait),
    };

    let prompt_status = if initial_prompt.is_some() {
        PromptStatus::Sent
    } else {
        match wait_until_idle_and_send(&tmux, &target.prompt, wait) {
            Ok(()) => PromptStatus::Sent,
            Err(e) => {
                log::warn!("open {}: prompt dispatch failed: {}", link.kind(), e);
                PromptStatus::Deferred(format!("prompt dispatch failed ({}), session is up", e))
            }
        }
    };

    Ok(Opened {
        target,
        tmux,
        session_id,
        prompt_status,
    })
}

/// For backends that mint their own session id: poll the scanner until the
/// session behind `tmux` shows up, then persist `title` under its id. Returns
/// the id, or `None` if the session never surfaced within `timeout` (the
/// session may still be fine — it just stays nameless).
fn name_once_visible(tmux: &str, title: &str, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let found = scanner::scan_sessions()
            .into_iter()
            .find(|s| s.tmux_session.as_deref() == Some(tmux))
            .map(|s| s.session_id);
        if let Some(sid) = found {
            if let Err(e) = title::persist_title(&sid, title) {
                log::warn!("open: persist title for {}: {}", sid, e);
            }
            return Some(sid);
        }
        if Instant::now() >= deadline {
            log::warn!("open: {} never surfaced in a scan; left nameless", tmux);
            return None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The first known folder whose name is `repo` (case-insensitive) and that
/// still exists on disk. Precedence follows the hub's folder picker:
/// registered projects, bookmarks, then session cwds newest-first.
fn folder_named(repo: &str) -> Option<PathBuf> {
    known_folders()
        .into_iter()
        .find(|path| is_named(path, repo) && path.is_dir())
}

fn is_named(path: &Path, name: &str) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(name))
}

fn known_folders() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    out.extend(projects_scan::scan().projects.into_iter().map(|p| p.root));
    out.extend(Bookmarks::load().list());

    let mut sessions = scanner::scan_sessions();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity.unwrap_or(s.started_at)));
    out.extend(sessions.into_iter().map(|s| PathBuf::from(s.cwd)));

    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_name_match_is_case_insensitive_and_exact() {
        let p = Path::new("/g/plarium/tps/tps-project");
        assert!(is_named(p, "tps-project"));
        assert!(is_named(p, "TPS-Project"));
        assert!(!is_named(p, "tps"));
        assert!(!is_named(p, "project"));
    }
}
