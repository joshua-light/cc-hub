//! `cc-hub open` body: act on a parsed [`Link`].
//!
//! A review runs in the local checkout of the pull request's repository,
//! found by name among bookmarks and scanned session cwds. Without one it
//! runs from the home directory against the pull request alone, and its
//! prompt says there is no working tree.
//!
//! A fix writes and pushes, so it needs a checkout. It is filed as a
//! Tasks-board card first ([`file_fix`]), with its brief as the first note.
//! The comments are the brief, so the card skips Planning and moves to
//! Running once its session is bound ([`start_card`]). The card also gives
//! the resource broker something to hold a worker against.
//!
//! A task link starts a session in the directory it names, running the
//! `task` skill on one card, bound to the card and linked back to it so the
//! Sessions grid shows the card's badge.
//!
//! A card has at most one session per place. A link to a directory where
//! the card already has a live session delivers the prompt there instead of
//! spawning, so the link that started a queued task can also wake it. A
//! link naming a `role` is a hand-over: it always starts fresh and reports
//! the old session as `superseded`, and it is refused when the card has no
//! note to hand over.
//!
//! When the backend lets cc-hub pick the session id (Claude's
//! `--session-id`), the title is persisted before the spawn, so the session
//! is never seen nameless. Other backends are named once a scan finds them.
//!
//! - `target`: resolve a link to its [`LinkTarget`] without side effects.
//! - `fix`: file a fix link as a board card and start it.

mod fix;
mod target;

pub use fix::{file_fix, start_card};
pub use target::{session_to_supersede, target, LinkTarget};

use std::path::Path;
use std::time::{Duration, Instant};

use crate::agent::AgentKind;
use crate::link::{BoardTaskId, Link};
use crate::ops::prompt::{wait_until_idle_and_send, PromptStatus, DEFAULT_PROMPT_WAIT_SECS};
use crate::ops::OpError;
use crate::sessions::scanner;
use crate::spawn::SessionTarget;
use crate::tasks::session_links;
use crate::tasks::store;
use crate::{config, send, spawn, title};
use target::{board_card, live_session_in};

/// Options for [`open`].
#[derive(Default)]
pub struct OpenOpts {
    /// Agent backend id; `None` → `[projects].default_session_agent`.
    pub agent: Option<String>,
    pub wait_secs: Option<u64>,
}

/// Result of [`open`].
pub struct Opened {
    pub target: LinkTarget,
    pub tmux: String,
    pub session_id: Option<String>,
    pub prompt_status: PromptStatus,
    /// The board card the session is bound to: the link's own for a task,
    /// the one [`file_fix`] minted for a fix, none for a review.
    pub task_id: Option<String>,
    /// The card already had a live session where the link pointed, and the
    /// prompt went there instead of to a new one.
    pub reused: bool,
    /// The card's previous live session, when this open was a hand-over. The
    /// caller closes it *after* reporting, because the caller may be running
    /// inside it.
    pub superseded: Option<String>,
}

/// Bind a freshly-spawned session to the card it was started for: the same
/// three fields the Tasks tab writes when it assigns an agent by hand, minus
/// the status move — a card reached In Progress before the link was opened,
/// and a link never moves a card. `session_id` is left for the first scan
/// that sees the mux session to resolve, as it is for a board assignment.
fn bind_card(task_id: &str, cwd: &Path, agent_id: &str, tmux: &str) -> Result<(), OpError> {
    store::update_task(task_id, |s| {
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
    let mut target = self::target(link, opts.agent.as_deref())?;
    let agent = config::get()
        .agent(&target.agent_id)
        .ok_or_else(|| OpError::Usage(format!("unknown agent id: {}", target.agent_id)))?;
    let cwd = target.cwd.to_string_lossy().into_owned();
    let wait = Duration::from_secs(opts.wait_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS));

    let mut superseded = None;
    if let Link::Task(task) = link {
        let card = board_card(task.id.as_str())?;
        let live = live_session_in(&card, &target.cwd, send::tmux_session_exists);
        if task.is_handover() {
            superseded = live;
        } else if let Some(tmux) = live {
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
                task_id: Some(task.id.to_string()),
                reused: true,
                superseded: None,
            });
        }
    }

    // The card the session will be bound to. A fix mints its own here, and
    // from then on is worked exactly like a task link's card.
    let filed = match link {
        Link::Fix(fix) => {
            let card = file_fix(fix)?;
            target.prompt = fix.prompt_for(&card);
            Some(card)
        }
        _ => None,
    };
    let card_id = match link {
        Link::Task(task) => Some(task.id.as_str()),
        _ => filed.as_ref().map(BoardTaskId::as_str),
    };

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

    if let Some(card) = card_id {
        bind_card(card, &target.cwd, &target.agent_id, &tmux)?;
    }

    let session_id = match chosen_id {
        Some(sid) => Some(sid),
        None => name_once_visible(&tmux, &target.title, wait),
    };

    if let (Some(card), Some(sid)) = (card_id, session_id.as_deref()) {
        link_session_to_card(sid, card);
    }
    if let Some(card) = &filed {
        start_card(card.as_str())?;
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
        task_id: card_id.map(str::to_string),
        reused: false,
        superseded,
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
    let link = session_links::TaskLink {
        task_id: task_id.to_string(),
        title,
    };
    if let Err(e) = session_links::link(session_id, link) {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tasks::store::TaskState;

    pub(super) fn card(cwd: Option<&str>) -> String {
        let mut state = TaskState::new("Semantic Linter".into());
        state.task_id = "tk-1".into();
        state.cwd = cwd.map(str::to_string);
        store::write_task_state(&state).expect("write card");
        state.task_id
    }

    #[test]
    fn the_session_wears_the_cards_badge() {
        crate::test_util::with_temp_home(|| {
            let id = card(None);
            link_session_to_card("sid-1", &id);
            let links = session_links::load();
            let link = links.get("sid-1").expect("linked");
            assert_eq!(link.task_id, id);
            assert_eq!(link.title, "Semantic Linter");
        });
    }
}
