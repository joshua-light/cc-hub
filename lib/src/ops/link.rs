//! `cc-hub open` body: act on a parsed [`Link`].
//!
//! A review link becomes a fresh agent session in the local checkout of the
//! pull request's repository, named `PR: <title>` and opened with the link's
//! prompt. The checkout is found by name among the folders the hub already
//! knows — bookmarks, then the cwds of scanned sessions — so a repo the user has ever worked in from the hub needs no
//! extra mapping. A repo none of them names is not a refusal: the review
//! runs from the home directory against the pull request alone, which is
//! all a review needs, and its prompt says there is no working tree.
//!
//! A fix link lands in a checkout under the name `Fix: <title>` — and only
//! in a checkout, because a fix writes and pushes. But it is a task, not a
//! stray session: [`file_fix`] mints a Tasks-board card
//! for it first, with the brief as the card's first note. That note is the
//! record the board's plan gate exists to produce, and a fix has nothing to
//! plan — the pull request's comments are the brief — so the card is born
//! past the gate: it skips Planning and reaches Running as soon as its
//! session is bound ([`start_card`]). Filing the card also gives the resource
//! broker something to hold a worker against, so a fix is placed on a
//! subscription account exactly as a routed card is.
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
//! of 2026-09-04. A link that names a `role` is the exception on purpose: it
//! is a hand-over, so it starts a fresh session and reports the card's old
//! one as `superseded` for the caller to close once it has printed. A
//! hand-over is refused when the card has no note: the notes are the brief
//! the next session works from, and without one there is nothing to hand.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::agent::AgentKind;
use crate::bookmarks::Bookmarks;
use crate::link::{BoardTaskId, FixLink, Link, PullRequestUrl, ReviewLink, TaskLink};
use crate::ops::prompt::{wait_until_idle_and_send, PromptStatus, DEFAULT_PROMPT_WAIT_SECS};
use crate::ops::OpError;
use crate::platform::paths::expand_home;
use crate::session_tasks;
use crate::spawn::SessionTarget;
use crate::task_store::{self, TaskState, TaskStatus};
use crate::tasks::PersonalBoard;
use crate::{config, scanner, send, spawn, title};

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

/// The live session a hand-over for `task` would replace, if any.
pub fn session_to_supersede(task: &crate::link::TaskLink) -> Result<Option<String>, OpError> {
    let target = self::target(&Link::Task(task.clone()), None)?;
    let card = board_card(task.id.as_str())?;
    Ok(live_session_in(
        &card,
        &target.cwd,
        send::tmux_session_exists,
    ))
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
        Link::Review(review) => review_target(review, agent_id),
        Link::Fix(fix) => {
            fix_kind(fix)?;
            fix_target(&fix.pr, fix.session_title(), fix.prompt(), agent_id)
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
            if task.is_handover() {
                without_a_brief(&card)?;
                without_a_verification_role(task, card.kind.as_deref())?;
            }
            Ok(LinkTarget {
                cwd,
                title: card.session_title(),
                prompt: task.prompt(&card.prompt),
                agent_id,
            })
        }
    }
}

/// Where a review lands: the local checkout of the repository the URL names
/// when the hub knows one, else the home directory. A pull request is
/// reviewable without a working tree — the diff, the files and the comments
/// all come from the server — so a repository nobody has cloned here is
/// reviewed from the pull request alone, and the prompt says so.
fn review_target(review: &ReviewLink, agent_id: String) -> Result<LinkTarget, OpError> {
    let checkout = folder_named(review.pr.repo());
    let prompt = match checkout {
        Some(_) => review.prompt(),
        None => review.prompt_without_checkout(),
    };
    let cwd = checkout.or_else(dirs::home_dir).ok_or_else(|| {
        OpError::NotFound("no checkout of the repository and no home directory".to_string())
    })?;
    Ok(LinkTarget {
        cwd,
        title: review.session_title(),
        prompt,
        agent_id,
    })
}

/// Where a fix lands: the local checkout of the repository the URL names,
/// found among the folders the hub knows. Unlike a review, a fix writes,
/// commits and pushes, so without a working tree there is nothing to do.
fn fix_target(
    pr: &PullRequestUrl,
    title: String,
    prompt: String,
    agent_id: String,
) -> Result<LinkTarget, OpError> {
    let repo = pr.repo();
    let cwd = folder_named(repo).ok_or_else(|| {
        OpError::NotFound(format!(
            "no known folder named `{}` — bookmark the checkout in cc-hub and retry",
            repo
        ))
    })?;
    Ok(LinkTarget {
        cwd,
        title,
        prompt,
        agent_id,
    })
}

/// File a fix as a card on the Tasks board: the standing orders as its text,
/// `Fix: <title>` as its title, the link's kind, and the brief as its first
/// note. The card is left in To-Do — it moves to Running through
/// [`start_card`] once a session actually holds it, so a fix that failed to
/// start is visible as a card with a brief and nobody on it, not as one that
/// claims to be running.
pub fn file_fix(fix: &FixLink) -> Result<BoardTaskId, OpError> {
    let kind = fix_kind(fix)?;
    let mut board =
        PersonalBoard::load_result().map_err(|e| OpError::Other(format!("load board: {}", e)))?;
    let task_id = board
        .add(&fix.prompt())
        .map_err(|e| OpError::Other(format!("write card: {}", e)))?
        .expect("a fix prompt is never empty");
    board
        .set_kind(&task_id, kind)
        .map_err(|e| OpError::Other(format!("write kind: {}", e)))?;
    task_store::set_task_title(&task_id, &fix.session_title())
        .map_err(|e| OpError::Other(format!("write title: {}", e)))?;
    crate::ops::task::task_artifact_add_text(&task_id, &fix_brief(fix), "link")?;
    Ok(task_id.parse().expect("the board mints tk- ids"))
}

/// The kind a fix card is filed under, checked against the board's list so
/// `--dry-run` refuses a link the browser button got wrong before anything
/// is filed. `None` is a card without a kind, as the board allows.
fn fix_kind(fix: &FixLink) -> Result<Option<String>, OpError> {
    fix.kind
        .as_deref()
        .map(|k| config::get().tasks.known_kind(k))
        .transpose()
        .map_err(|why| OpError::Usage(format!("fix link kind: {}", why)))
}

/// The brief a fix card starts with. It is what a task session would have
/// agreed with the user in the plan gate, written down without asking,
/// because a review already asked: the comments are the problem, working
/// them is the solution, and the reviewer is the verification.
fn fix_brief(fix: &FixLink) -> String {
    format!(
        "Brief\n\
         Problem: {} has review comments waiting on the author.\n\
         Solution: address them on the pull request's branch — one Bitbucket task per comment, done once its fix is committed and pushed; answer questions, ask when a comment is ambiguous.\n\
         Verification: the reviewer re-reads the pull request. This task has the one role; no hand-over.",
        fix.pr
    )
}

/// The card's session is up: move it to Running. Called after the binding,
/// never before, so the task router — which hands out cards that are Running
/// with no session — never sees this one as its own.
pub fn start_card(task_id: &str) -> Result<(), OpError> {
    PersonalBoard::load_result()
        .map_err(|e| OpError::Other(format!("load board: {}", e)))?
        .set_status(task_id, TaskStatus::Running)
        .map(|_| ())
        .map_err(|e| {
            OpError::Other(format!(
                "card {} is bound but could not start: {}",
                task_id, e
            ))
        })
}

/// No hand-over without a brief. The session that hands a card to the next
/// role must have written down what the task is and how it is verified;
/// that record is the card's notes, and a card with none has nothing to hand
/// over. The `task` skill used to keep this rule in its own script; it lives
/// here so that every role link, from any caller, passes the same gate.
fn without_a_brief(card: &TaskState) -> Result<(), OpError> {
    if crate::ops::task::notes_of(card).is_empty() {
        return Err(OpError::conflict_with_recipe(
            format!(
                "no hand-over without a brief: card {} has no note",
                card.task_id
            ),
            format!(
                "cc-hub board note --task {} --text '<problem, solution, verification>'",
                card.task_id
            ),
        ));
    }
    Ok(())
}

/// No hand-over to a role the kind does not have. Only the kinds in
/// `[tasks].handover_kinds` are worked by two sessions; every other kind has
/// the one session, which tests its own candidate and opens the pull request
/// itself. A verification link for one of those is a link nobody can honour,
/// so it is refused here rather than spawning a session that reads a brief
/// and finds the work already delivered.
fn without_a_verification_role(task: &TaskLink, filed: Option<&str>) -> Result<(), OpError> {
    let kind = task.kind.as_deref().or(filed);
    if !task.is_verification() || config::get().tasks.hands_over(kind) {
        return Ok(());
    }
    Err(OpError::Usage(format!(
        "{} has no verification role: its implementation session tests what it built and opens the pull request",
        kind.expect("a kind the list does not hold")
    )))
}

/// The board card a task link addresses.
fn board_card(task_id: &str) -> Result<TaskState, OpError> {
    task_store::read_task_state(task_id)
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
    task_store::update_task(task_id, |s| {
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
    let link = session_tasks::TaskLink {
        task_id: task_id.to_string(),
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
/// bookmarks, then session cwds newest-first.
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
        let mut state = TaskState::new("Semantic Linter".into());
        state.task_id = "tk-1".into();
        state.cwd = cwd.map(str::to_string);
        task_store::write_task_state(&state).expect("write card");
        state.task_id
    }

    #[cfg(unix)]
    fn fix_link(query: &str) -> FixLink {
        let raw = format!(
            "cc-hub://fix?pr=https://bitbucket.example.com/projects/APP/repos/sample-project/pull-requests/11280{}",
            query
        );
        match raw.parse::<Link>() {
            Ok(Link::Fix(fix)) => fix,
            other => panic!("expected a fix link, got {:?}", other),
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_fix_is_filed_as_a_card_with_its_brief() {
        crate::test_util::with_temp_home(|| {
            let fix = fix_link("&title=Fix%20the%20parser");
            let id = file_fix(&fix).expect("file");
            let card = board_card(id.as_str()).expect("card");
            assert_eq!(card.status, TaskStatus::Backlog);
            assert_eq!(card.title.as_deref(), Some("Fix: Fix the parser"));
            assert_eq!(card.prompt, fix.prompt());
            assert_eq!(card.kind, None);
            assert!(card.tmux.is_none());
            let notes = crate::ops::task::notes_of(&card);
            assert_eq!(notes.len(), 1);
            assert!(
                notes[0].text.starts_with("Brief\nProblem: "),
                "{}",
                notes[0].text
            );
            assert!(notes[0].text.contains("pull-requests/11280"));
            assert!(notes[0].text.contains("Verification:"));
            // With a note on it, a role link would pass the hand-over gate.
            without_a_brief(&card).expect("the brief is the note");
        });
    }

    #[test]
    #[cfg(unix)]
    fn a_filed_fix_starts_into_running() {
        crate::test_util::with_temp_home(|| {
            let id = file_fix(&fix_link("")).expect("file");
            start_card(id.as_str()).expect("start");
            assert_eq!(
                board_card(id.as_str()).expect("card").status,
                TaskStatus::Running
            );
        });
    }

    #[test]
    #[cfg(unix)]
    fn a_fix_kind_must_be_one_the_board_offers() {
        crate::test_util::with_temp_home(|| {
            // A temp home has no [tasks].kinds, so any kind is unknown.
            assert!(matches!(
                file_fix(&fix_link("&kind=tps")),
                Err(OpError::Usage(why)) if why.contains("fix link kind")
            ));
        });
    }

    #[cfg(unix)]
    fn link_of(raw: &str) -> Link {
        raw.parse().expect("parse")
    }

    #[test]
    #[cfg(unix)]
    fn a_review_without_a_checkout_runs_from_home() {
        crate::test_util::with_temp_home(|| {
            let link = link_of(
                "cc-hub://review?depth=light&pr=https://bitbucket.example.com/projects/APP/repos/never-cloned/pull-requests/7",
            );
            let target = target(&link, Some("claude")).expect("target");
            assert_eq!(target.cwd, dirs::home_dir().expect("home"));
            assert!(target.prompt.contains("Let's do light review of this PR"));
            assert!(
                target
                    .prompt
                    .contains("no local checkout of `never-cloned`"),
                "{}",
                target.prompt
            );
        });
    }

    #[test]
    #[cfg(unix)]
    fn a_fix_without_a_checkout_is_refused() {
        crate::test_util::with_temp_home(|| {
            let link = link_of(
                "cc-hub://fix?pr=https://bitbucket.example.com/projects/APP/repos/never-cloned/pull-requests/7",
            );
            assert!(matches!(
                target(&link, Some("claude")),
                Err(OpError::NotFound(why)) if why.contains("never-cloned")
            ));
        });
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
            assert_eq!(
                target.prompt,
                "/task --task tk-1 --kind basic Semantic Linter"
            );
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
    fn a_role_link_needs_a_note_on_the_card() {
        crate::test_util::with_temp_home(|| {
            let dir = std::env::temp_dir();
            let id = card(None);
            let link: Link = format!(
                "cc-hub://task?id={}&dir={}&role=verification",
                id,
                dir.display()
            )
            .parse()
            .expect("parse");
            assert!(matches!(
                target(&link, Some("claude")),
                Err(OpError::Conflict { .. })
            ));

            crate::ops::task::task_artifact_add_text(&id, "Problem: …", "cli").expect("note");
            let target = target(&link, Some("claude")).expect("target");
            assert_eq!(
                target.prompt,
                "/task --task tk-1 --role verification Semantic Linter"
            );
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
            assert_eq!(link.title, "Semantic Linter");
        });
    }

    fn card_with_session(cwd: &str, tmux: Option<&str>) -> TaskState {
        let mut state = TaskState::new("Semantic Linter".into());
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
