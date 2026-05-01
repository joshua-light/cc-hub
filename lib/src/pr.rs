//! Pull Request schema for the cc-hub PR-flow design.
//!
//! Every task that produces edits goes through a virtual PR: the
//! orchestrator opens one with `cc-hub pr create`, the user reviews it in
//! the TUI (diff view + approve / request-changes keybinds), and the
//! orchestrator merges via `cc-hub pr merge` once approved. PRs live next
//! to task state at `~/.cc-hub/projects/<pid>/tasks/<tid>/pr.json`.
//!
//! The PR id is a per-project sequential counter persisted at
//! `~/.cc-hub/projects/<pid>/pr-counter` — incremented atomically on
//! creation. Sequential ids match the GitHub mental model ("PR #42") and
//! make TUI rendering and CLI references human-readable.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::orchestrator;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// PR is open, waiting on the reviewer.
    Open,
    /// Reviewer asked for changes; orchestrator should iterate, push more
    /// commits, and re-open the PR for review (transition back to Open).
    ChangesRequested,
    /// Reviewer approved — orchestrator may transition the task to
    /// `Merging` (subject to the project's merge lock).
    Approved,
    /// Merge completed; PR is closed.
    Merged,
    /// PR was closed without merging (e.g. task abandoned).
    Closed,
}

impl ReviewState {
    /// Snake-case rendering matching the on-wire serde representation —
    /// stable for CLI JSON output and human-facing error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewState::Open => "open",
            ReviewState::ChangesRequested => "changes_requested",
            ReviewState::Approved => "approved",
            ReviewState::Merged => "merged",
            ReviewState::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// Free-form: `"user"`, `"orchestrator"`, or a worker tmux name.
    pub author: String,
    pub at: i64,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: u32,
    pub task_id: String,
    pub project_id: String,
    /// Feature branch (e.g. `cc-hub/t-12345-fix`).
    pub branch: String,
    /// Target branch (usually `main`).
    pub base: String,
    pub title: String,
    pub description: String,
    pub review_state: ReviewState,
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// SHAs captured when the reviewer approved. Used by `pr merge` to
    /// detect whether main has moved since approval and run the
    /// auto-approve-after-clean-resolution heuristic. `None` until the
    /// first Approved transition.
    #[serde(default)]
    pub approved_at_branch_sha: Option<String>,
    #[serde(default)]
    pub approved_at_base_sha: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl PullRequest {
    pub fn touch(&mut self) {
        self.updated_at = orchestrator::now_unix_secs();
    }
}

pub fn pr_file_path(project_id: &str, task_id: &str) -> Option<PathBuf> {
    orchestrator::task_state_dir(project_id, task_id).map(|d| d.join("pr.json"))
}

pub fn pr_counter_path(project_id: &str) -> Option<PathBuf> {
    orchestrator::project_state_dir(project_id).map(|d| d.join("pr-counter"))
}

/// Read a task's PR record. Returns `Ok(None)` if no PR has been opened
/// for this task, `Err` for filesystem or schema errors.
pub fn read_pr(project_id: &str, task_id: &str) -> io::Result<Option<PullRequest>> {
    let path = pr_file_path(project_id, task_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {}", path.display(), e),
            )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Atomic write via tempfile + rename. Creates parent dirs on demand.
pub fn write_pr(pr: &PullRequest) -> io::Result<()> {
    let path = pr_file_path(&pr.project_id, &pr.task_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(pr)
        .map_err(|e| io::Error::other(format!("serialise pr.json: {}", e)))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// In-place read → mutate → write helper, mirroring
/// [`orchestrator::update_task_state`]. Returns the post-mutation record.
pub fn update_pr<F>(project_id: &str, task_id: &str, f: F) -> io::Result<PullRequest>
where
    F: FnOnce(&mut PullRequest),
{
    let mut pr = read_pr(project_id, task_id)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no PR for this task"))?;
    f(&mut pr);
    pr.touch();
    write_pr(&pr)?;
    Ok(pr)
}

/// Allocate the next per-project PR id by reading the counter, bumping it,
/// and writing it back atomically. Counter starts at 1 (matches GitHub's
/// 1-indexed PR numbering).
pub fn allocate_pr_id(project_id: &str) -> io::Result<u32> {
    let path = pr_counter_path(project_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let current: u32 = match fs::read_to_string(&path) {
        Ok(raw) => raw.trim().parse().map_err(|e| io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pr-counter parse: {}", e),
        ))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    let next = current.checked_add(1).ok_or_else(|| {
        io::Error::other("pr-counter overflow — recreate the project state")
    })?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, next.to_string())?;
    fs::rename(&tmp, &path)?;
    Ok(next)
}

/// Create a fresh PR record from a TaskState + worktree. Allocates a new
/// id, writes the file, and returns the populated struct.
pub fn create_pr(
    state: &orchestrator::TaskState,
    branch: String,
    base: String,
    title: String,
    description: String,
) -> io::Result<PullRequest> {
    let id = allocate_pr_id(&state.project_id)?;
    let now = orchestrator::now_unix_secs();
    let pr = PullRequest {
        id,
        task_id: state.task_id.clone(),
        project_id: state.project_id.clone(),
        branch,
        base,
        title,
        description,
        review_state: ReviewState::Open,
        comments: Vec::new(),
        approved_at_branch_sha: None,
        approved_at_base_sha: None,
        created_at: now,
        updated_at: now,
    };
    write_pr(&pr)?;
    Ok(pr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;
    use std::path::PathBuf;

    fn with_tempdir<F: FnOnce()>(f: F) {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    fn fake_state(project_id: &str, task_id: &str) -> orchestrator::TaskState {
        let mut s = orchestrator::TaskState::new(
            project_id.into(),
            PathBuf::from("/tmp/proj"),
            "do thing".into(),
        );
        s.task_id = task_id.into();
        s
    }

    #[test]
    fn allocate_pr_id_increments() {
        with_tempdir(|| {
            assert_eq!(allocate_pr_id("p1").unwrap(), 1);
            assert_eq!(allocate_pr_id("p1").unwrap(), 2);
            assert_eq!(allocate_pr_id("p1").unwrap(), 3);
            // Distinct projects have independent counters.
            assert_eq!(allocate_pr_id("p2").unwrap(), 1);
        });
    }

    #[test]
    fn create_and_read_pr_round_trip() {
        with_tempdir(|| {
            let state = fake_state("p1", "t-1");
            // Need to write the task state first so task_state_dir exists.
            orchestrator::write_task_state(&state).unwrap();
            let pr = create_pr(
                &state,
                "cc-hub/t-1-fix".into(),
                "main".into(),
                "Fix the thing".into(),
                "It was broken; now it isn't.".into(),
            )
            .unwrap();
            assert_eq!(pr.id, 1);
            assert_eq!(pr.review_state, ReviewState::Open);

            let back = read_pr("p1", "t-1").unwrap().expect("present");
            assert_eq!(back, pr);
        });
    }

    #[test]
    fn read_pr_missing_returns_none() {
        with_tempdir(|| {
            assert!(read_pr("p1", "t-nope").unwrap().is_none());
        });
    }

    #[test]
    fn update_pr_runs_closure_and_touches() {
        with_tempdir(|| {
            let state = fake_state("p1", "t-1");
            orchestrator::write_task_state(&state).unwrap();
            let original = create_pr(
                &state,
                "cc-hub/t-1-fix".into(),
                "main".into(),
                "T".into(),
                "D".into(),
            )
            .unwrap();
            // Force a different second-resolution timestamp so updated_at
            // can be observed to advance.
            std::thread::sleep(std::time::Duration::from_millis(1100));
            let updated = update_pr("p1", "t-1", |p| {
                p.review_state = ReviewState::Approved;
                p.comments.push(Comment {
                    author: "user".into(),
                    at: orchestrator::now_unix_secs(),
                    body: "lgtm".into(),
                });
            })
            .unwrap();
            assert_eq!(updated.review_state, ReviewState::Approved);
            assert_eq!(updated.comments.len(), 1);
            assert!(updated.updated_at > original.updated_at);
        });
    }

    #[test]
    fn review_state_serialises_snake_case() {
        let s = serde_json::to_string(&ReviewState::ChangesRequested).unwrap();
        assert_eq!(s, "\"changes_requested\"");
        let parsed: ReviewState = serde_json::from_str("\"approved\"").unwrap();
        assert_eq!(parsed, ReviewState::Approved);
    }
}
