//! Garbage collection of orphaned worktrees under `<root>/.cc-hub-wt/`.

use super::git::run_git;
use super::{list_task_states, unparsed_task_ids, TaskStatus};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One worktree directory found under `<root>/.cc-hub-wt/` during a gc scan,
/// classified by whether a live task still owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// Directory basename, conventionally `<task-id>-<name>`.
    pub dir_name: String,
    /// Absolute path to the worktree directory.
    pub path: PathBuf,
    /// Derived branch (`cc-hub/<dir_name>`) that `git worktree add -b` created.
    pub branch: String,
    /// `true` when a task whose id prefixes `dir_name` exists and isn't Done.
    /// `false` (an orphan) when the owning task is missing or Done.
    pub live: bool,
}

/// Plan + result of a `task gc` pass over one project's `.cc-hub-wt/` dir.
/// On a dry run only `orphans` / `live` are populated and the `*_removed`
/// / `*_errors` vecs stay empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcOutcome {
    /// Worktree dirs an orphan-owning task no longer keeps alive.
    pub orphans: Vec<WorktreeEntry>,
    /// Worktree dirs a live (non-Done, present) task still owns — left intact.
    pub live: Vec<WorktreeEntry>,
    /// Orphan worktree paths actually removed (git or fs fallback). Empty on
    /// `--dry-run`.
    pub worktrees_removed: Vec<String>,
    /// Dangling `cc-hub/*` branches deleted for removed orphans. Empty on
    /// `--dry-run`.
    pub branches_removed: Vec<String>,
    /// Per-path cleanup failures `(path, error)`. Empty on `--dry-run`.
    pub errors: Vec<(String, String)>,
    /// Whether `git worktree prune` ran (skipped on `--dry-run`).
    pub pruned: bool,
}

/// Classify every directory under `<project_root>/.cc-hub-wt/` as live or
/// orphaned. A worktree dir is named `<task-id>-<name>`; it's "live" when some
/// task in `project_id` whose id is a prefix of the dir name (followed by `-`)
/// exists and is not [`TaskStatus::Done`]. Everything else — task deleted,
/// task finished, or a dir we can't attribute — is an orphan eligible for gc.
///
/// Returns an empty plan (no orphans, no live) when the `.cc-hub-wt/` dir
/// doesn't exist yet.
pub fn scan_worktrees(
    project_id: &str,
    project_root: &Path,
) -> io::Result<(Vec<WorktreeEntry>, Vec<WorktreeEntry>)> {
    let wt_root = project_root.join(".cc-hub-wt");
    if !wt_root.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }

    // Task ids that still own their worktrees: present and not Done.
    let mut live_task_ids: std::collections::HashSet<String> = list_task_states(project_id)?
        .into_iter()
        .filter(|s| s.status != TaskStatus::Done)
        .map(|s| s.task_id)
        .collect();
    // Fail SAFE on corruption: a task whose state.json is present but won't parse
    // is kept alive (its worktree is never orphaned), so a transient/permanent
    // parse error can't make gc destroy in-flight work. Only a genuinely absent
    // or Done task yields an orphan.
    live_task_ids.extend(unparsed_task_ids(project_id));

    let mut orphans = Vec::new();
    let mut live = Vec::new();
    for entry in fs::read_dir(&wt_root)? {
        let entry = entry?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Some(dir_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // The owning task id is the dir name up to (but not including) the
        // `-<name>` suffix. We don't know where the split is, so test every
        // live id as a `<id>-` prefix; ids are `t-<nanos>`, so a real prefix
        // match is unambiguous in practice.
        let is_live = live_task_ids
            .iter()
            .any(|id| dir_name.starts_with(&format!("{}-", id)));
        let wt = WorktreeEntry {
            branch: worktree_branch_from_dir(&dir_name),
            path: entry.path(),
            dir_name,
            live: is_live,
        };
        if is_live {
            live.push(wt);
        } else {
            orphans.push(wt);
        }
    }
    orphans.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
    live.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
    Ok((orphans, live))
}

/// Branch a worktree dir named `<task-id>-<name>` lives on:
/// `cc-hub/<task-id>-<name>`. Mirrors [`worktree_branch`] but works straight
/// off the dir name (gc doesn't have the task/name split).
fn worktree_branch_from_dir(dir_name: &str) -> String {
    format!("cc-hub/{}", dir_name)
}

/// Garbage-collect orphaned worktrees under `<project_root>/.cc-hub-wt/`.
///
/// Worktrees + their `cc-hub/*` branches are only torn down on the Done path
/// (see [`remove_task_worktrees`]), so Review / abandoned / wedged tasks leak
/// them. This sweeps the whole `.cc-hub-wt/` dir: any worktree no live task
/// owns (task missing or Done) is removed via `git worktree remove --force`
/// (falling back to `fs::remove_dir_all`), its dangling `cc-hub/*` branch is
/// deleted, and finally `git worktree prune` clears stale admin entries.
///
/// `dry_run` computes the plan (`orphans` / `live`) without touching anything.
pub fn gc_worktrees(project_id: &str, project_root: &Path, dry_run: bool) -> io::Result<GcOutcome> {
    let (orphans, live) = scan_worktrees(project_id, project_root)?;

    let mut outcome = GcOutcome {
        orphans: orphans.clone(),
        live,
        ..Default::default()
    };
    if dry_run {
        return Ok(outcome);
    }

    for wt in &orphans {
        let path_str = wt.path.to_string_lossy().into_owned();
        let git_result = run_git(project_root, &["worktree", "remove", "--force", &path_str]);
        let removed = match &git_result {
            Ok(out) => {
                let stderr_lower = out.stderr.to_lowercase();
                let not_a_wt = stderr_lower.contains("not a working tree");
                out.status_ok || !wt.path.exists() || not_a_wt
            }
            Err(_) => !wt.path.exists(),
        };
        let removed = if removed {
            true
        } else {
            // git wouldn't take it (e.g. dir exists but was never a real
            // worktree); fall back to a plain recursive delete.
            match fs::remove_dir_all(&wt.path) {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => true,
                Err(e) => {
                    let prefix = match &git_result {
                        Ok(out) => format!("git: {}", out.stderr.trim()),
                        Err(ge) => format!("git invoke: {}", ge),
                    };
                    outcome
                        .errors
                        .push((path_str.clone(), format!("{}; fs: {}", prefix, e)));
                    false
                }
            }
        };
        if !removed {
            continue;
        }
        outcome.worktrees_removed.push(path_str);

        // Delete the dangling branch. `git worktree remove` removes the dir +
        // admin entry but leaves the `cc-hub/*` branch behind, so do it
        // explicitly with `-D` (force, since it may be unmerged). A failure
        // here just means the branch was already gone — not worth surfacing.
        let branch_out = run_git(project_root, &["branch", "-D", &wt.branch]);
        if matches!(&branch_out, Ok(o) if o.status_ok) {
            outcome.branches_removed.push(wt.branch.clone());
        }
    }

    // Clear stale worktree admin entries (e.g. for dirs we rm-rf'd directly).
    if let Ok(out) = run_git(project_root, &["worktree", "prune"]) {
        outcome.pruned = out.status_ok;
    }

    Ok(outcome)
}
