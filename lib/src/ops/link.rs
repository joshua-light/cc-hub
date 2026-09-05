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
//!
//! A task link becomes a session in the directory the link names, running
//! the `task` skill against one Tasks-board card. The card is bound to that
//! session the moment it exists, so `f` on the card attaches to it exactly
//! as if the board had assigned it — and the session is linked back to the
//! card (the sidecar behind `L`), so on the Sessions grid it wears the
//! card's badge and clusters with the card's other sessions instead of
//! looking like a stray session in somebody's project.
//!
//! A card has at most one session per place. When the card already owns a
//! live session in the directory the link names, the link *is* that session:
//! the prompt is delivered to it and nothing is spawned. A session cannot
//! change its own cwd, so a link naming a different directory is a hand-over
//! and starts fresh there. This is what lets a queued task be woken by the
//! same link that started it, without the two-sessions-one-journal failure
//! of 2026-09-04.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::agent::AgentKind;
use crate::bookmarks::Bookmarks;
use crate::link::Link;
use crate::ops::worker::{wait_until_idle_and_send, PromptStatus, DEFAULT_PROMPT_WAIT_SECS};
use crate::ops::OpError;
use crate::orchestrator::{self, TaskState};
use crate::platform::paths::expand_home;
use crate::session_tasks;
use crate::spawn::SessionTarget;
use crate::{config, projects_scan, scanner, send, spawn, title};

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
    /// The card already had a live session where the link pointed, and the
    /// prompt went there instead of to a new one.
    pub reused: bool,
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
        Link::Task(task) => {
            let card = board_card(task.id.as_str())?;
            let cwd = task
                .dir
                .clone()
                .map(|d| expand_home(&d.to_string_lossy()))
                .or_else(|| card.cwd.as_deref().map(PathBuf::from))
                .ok_or_else(|| {
                    OpError::Usage(format!(
                        "task {} has no recorded cwd — add `&dir=<path>` to the link",
                        task.id
                    ))
                })?;
            if !cwd.is_dir() {
                return Err(OpError::NotFound(format!(
                    "not a directory: {}",
                    cwd.display()
                )));
            }
            let brief = card.title.as_deref().unwrap_or(&card.prompt);
            Ok(LinkTarget {
                cwd,
                title: task.session_title(brief),
                prompt: task.prompt(&card.prompt),
                agent_id,
            })
        }
    }
}

/// The board card a task link addresses. Personal-board tasks are the only
/// ones a `tk-` id can name, so the project store is never consulted.
fn board_card(task_id: &str) -> Result<TaskState, OpError> {
    orchestrator::read_task_state_for(None, task_id)
        .map_err(|e| OpError::NotFound(format!("no board task {}: {}", task_id, e)))
}

/// The session `card` already runs in `cwd`, if it has one and it is alive.
/// `alive` is the multiplexer's word on the tmux name; a card can keep a
/// name long after the session behind it is gone.
fn live_session_in(card: &TaskState, cwd: &Path, alive: impl Fn(&str) -> bool) -> Option<String> {
    let tmux = card.tmux.as_deref()?;
    let same_place = card.cwd.as_deref().is_some_and(|c| Path::new(c) == cwd);
    (same_place && alive(tmux)).then(|| tmux.to_string())
}

/// Bind a freshly-spawned session to the card it was started for: the same
/// three fields the Tasks tab writes when it assigns an agent by hand, minus
/// the status move — a card reached In Progress before the link was opened,
/// and a link never moves a card. `session_id` is left for the first scan
/// that sees the mux session to resolve, as it is for a board assignment.
fn bind_card(task_id: &str, cwd: &Path, agent_id: &str, tmux: &str) -> Result<(), OpError> {
    orchestrator::update_personal_task(task_id, |s| {
        s.cwd = Some(cwd.to_string_lossy().into_owned());
        s.agent_id = Some(agent_id.to_string());
        s.tmux = Some(tmux.to_string());
        s.session_id = None;
    })
    .map(|_| ())
    .map_err(|e| {
        OpError::Other(format!(
            "session {} is up, but binding it to task {} failed: {}",
            tmux, task_id, e
        ))
    })
}

/// Spawn the session `link` asks for, name it, and deliver its prompt.
pub fn open(link: &Link, opts: OpenOpts) -> Result<Opened, OpError> {
    let target = self::target(link, opts.agent.as_deref())?;
    let agent = config::get()
        .agent(&target.agent_id)
        .ok_or_else(|| OpError::Usage(format!("unknown agent id: {}", target.agent_id)))?;
    let cwd = target.cwd.to_string_lossy().into_owned();
    let wait = Duration::from_secs(opts.wait_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS));

    if let Link::Task(task) = link {
        let card = board_card(task.id.as_str())?;
        if let Some(tmux) = live_session_in(&card, &target.cwd, send::tmux_session_exists) {
            let prompt_status = match wait_until_idle_and_send(&tmux, &target.prompt, wait) {
                Ok(()) => PromptStatus::Sent,
                Err(e) => {
                    log::warn!("open task: prompt to live session {} failed: {}", tmux, e);
                    PromptStatus::Deferred(format!(
                        "card already has a live session {} here; prompt dispatch failed ({})",
                        tmux, e
                    ))
                }
            };
            return Ok(Opened {
                target,
                tmux,
                session_id: card.session_id,
                prompt_status,
                reused: true,
            });
        }
    }

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
    let tmux =
        spawn::spawn_agent_session(&target.agent_id, &cwd, session, initial_prompt, None, false)
            .map_err(|e| OpError::Other(format!("spawn session: {}", e)))?;

    if let Link::Task(task) = link {
        bind_card(task.id.as_str(), &target.cwd, &target.agent_id, &tmux)?;
    }

    let session_id = match chosen_id {
        Some(sid) => Some(sid),
        None => name_once_visible(&tmux, &target.title, wait),
    };

    if let (Link::Task(task), Some(sid)) = (link, session_id.as_deref()) {
        link_session_to_card(sid, task.id.as_str());
    }

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
        reused: false,
    })
}

/// Record the session→card link the Sessions grid reads for badges and
/// task clustering. Cosmetic, unlike [`bind_card`]: the card already owns
/// the session, so a failed sidecar write costs a badge, not the binding —
/// it is logged and the open still succeeds.
fn link_session_to_card(session_id: &str, task_id: &str) {
    let title = match board_card(task_id) {
        Ok(card) => card.title.unwrap_or(card.prompt),
        Err(e) => {
            log::warn!("open task: card {} vanished before linking: {}", task_id, e);
            return;
        }
    };
    let link = session_tasks::TaskLink {
        task_id: task_id.to_string(),
        project_id: None,
        title,
    };
    if let Err(e) = session_tasks::link(session_id, link) {
        log::warn!(
            "open task: linking session {} to card {} failed: {}",
            session_id,
            task_id,
            e
        );
    }
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

    #[cfg(unix)]
    fn card(cwd: Option<&str>) -> String {
        let mut state = TaskState::new_personal("Semantic Linter".into());
        state.task_id = "tk-1".into();
        state.cwd = cwd.map(str::to_string);
        orchestrator::write_task_state(&state).expect("write card");
        state.task_id
    }

    #[test]
    #[cfg(unix)]
    fn a_task_link_lands_where_it_says() {
        crate::test_util::with_temp_home(|| {
            let dir = std::env::temp_dir();
            let id = card(None);
            let link: Link = format!("cc-hub://task?id={}&dir={}&kind=basic", id, dir.display())
                .parse()
                .expect("parse");
            let target = target(&link, Some("claude")).expect("target");
            assert_eq!(target.cwd, dir);
            assert!(target
                .prompt
                .starts_with("/task --task tk-1 Semantic Linter"));
            assert_eq!(target.title, "Task: Semantic Linter");
        });
    }

    #[test]
    #[cfg(unix)]
    fn without_a_dir_the_card_says_where_it_lives() {
        crate::test_util::with_temp_home(|| {
            let dir = std::env::temp_dir();
            let id = card(Some(&dir.to_string_lossy()));
            let link: Link = format!("cc-hub://task?id={}", id).parse().expect("parse");
            assert_eq!(target(&link, Some("claude")).expect("target").cwd, dir);
        });
    }

    #[test]
    #[cfg(unix)]
    fn a_card_with_nowhere_to_run_is_a_usage_error() {
        crate::test_util::with_temp_home(|| {
            card(None);
            let link: Link = "cc-hub://task?id=tk-1".parse().expect("parse");
            assert!(matches!(
                target(&link, Some("claude")),
                Err(OpError::Usage(_))
            ));
        });
    }

    #[test]
    #[cfg(unix)]
    fn an_unknown_card_is_not_found() {
        crate::test_util::with_temp_home(|| {
            let link: Link = "cc-hub://task?id=tk-404".parse().expect("parse");
            assert!(matches!(
                target(&link, Some("claude")),
                Err(OpError::NotFound(_))
            ));
        });
    }

    #[test]
    #[cfg(unix)]
    fn the_session_wears_the_cards_badge() {
        crate::test_util::with_temp_home(|| {
            let id = card(None);
            link_session_to_card("sid-1", &id);
            let links = session_tasks::load();
            let link = links.get("sid-1").expect("linked");
            assert_eq!(link.task_id, id);
            assert_eq!(link.project_id, None);
            assert_eq!(link.title, "Semantic Linter");
        });
    }

    fn card_with_session(cwd: &str, tmux: Option<&str>) -> TaskState {
        let mut state = TaskState::new_personal("Semantic Linter".into());
        state.cwd = Some(cwd.into());
        state.tmux = tmux.map(str::to_string);
        state
    }

    #[test]
    fn a_live_session_in_the_same_place_is_the_link_target() {
        let card = card_with_session("/g/sample", Some("cchub-1"));
        assert_eq!(
            live_session_in(&card, Path::new("/g/sample"), |_| true),
            Some("cchub-1".into())
        );
    }

    #[test]
    fn a_link_to_another_place_starts_fresh() {
        let card = card_with_session("/g/cc-hub", Some("cchub-1"));
        assert_eq!(
            live_session_in(&card, Path::new("/g/sample"), |_| true),
            None
        );
    }

    #[test]
    fn a_dead_or_missing_session_starts_fresh() {
        let dead = card_with_session("/g/sample", Some("cchub-1"));
        assert_eq!(
            live_session_in(&dead, Path::new("/g/sample"), |_| false),
            None
        );
        let none = card_with_session("/g/sample", None);
        assert_eq!(
            live_session_in(&none, Path::new("/g/sample"), |_| true),
            None
        );
    }

    #[test]
    fn folder_name_match_is_case_insensitive_and_exact() {
        let p = Path::new("/g/company/apps/sample-project");
        assert!(is_named(p, "sample-project"));
        assert!(is_named(p, "Sample-Project"));
        assert!(!is_named(p, "sample"));
        assert!(!is_named(p, "project"));
    }
}
