use std::path::{Path, PathBuf};

use super::fix::fix_kind;
use crate::bookmarks::Bookmarks;
use crate::link::{Link, PullRequestUrl, ReviewLink, TaskLink};
use crate::ops::OpError;
use crate::platform::paths::expand_home;
use crate::sessions::scanner;
use crate::tasks::store::{self, TaskState};
use crate::{config, send};

/// Where a link lands, resolved without side effects: the folder the session
/// starts in, the name and prompt it starts with, and the agent that runs
/// it. This is what `--dry-run` prints.
pub struct LinkTarget {
    pub cwd: PathBuf,
    pub title: String,
    pub prompt: String,
    pub agent_id: String,
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

/// No hand-over without a brief. The session that hands a card to the next
/// role must have written down what the task is and how it is verified;
/// that record is the card's notes, and a card with none has nothing to hand
/// over. The `task` skill used to keep this rule in its own script; it lives
/// here so that every role link, from any caller, passes the same gate.
pub(super) fn without_a_brief(card: &TaskState) -> Result<(), OpError> {
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
pub(super) fn board_card(task_id: &str) -> Result<TaskState, OpError> {
    store::read_task_state(task_id)
        .map_err(|e| OpError::NotFound(format!("no board task {}: {}", task_id, e)))
}

/// The session `card` already runs in `cwd`, if it has one and it is alive.
/// `alive` is the multiplexer's word on the tmux name; a card can keep a
/// name long after the session behind it is gone.
pub(super) fn live_session_in(
    card: &TaskState,
    cwd: &Path,
    alive: impl Fn(&str) -> bool,
) -> Option<String> {
    let tmux = card.tmux.as_deref()?;
    let same_place = card.cwd.as_deref().is_some_and(|c| Path::new(c) == cwd);
    (same_place && alive(tmux)).then(|| tmux.to_string())
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
    use crate::ops::link::tests::card;

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
