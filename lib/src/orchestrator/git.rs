//! Git worktree + merge primitives for the orchestrator layer.

use super::MergeOutcome;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Standard worktree path for `<task>-<name>` under `<root>/.cc-hub-wt/`.
pub fn worktree_path(project_root: &Path, task_id: &str, name: &str) -> PathBuf {
    project_root
        .join(".cc-hub-wt")
        .join(format!("{}-{}", task_id, name))
}

/// Branch name for a worktree. Mirrors the dir name so `git worktree list`
/// is readable.
pub fn worktree_branch(task_id: &str, name: &str) -> String {
    format!("cc-hub/{}-{}", task_id, name)
}

/// Whether `name` is a safe `--worktree` value. The name flows into a derived
/// branch (`worktree_branch`), into `git worktree add -b <branch>`, and into
/// the auto-reviewer's shell instructions an unattended LLM runs — so a
/// leading dash would be read as a git flag (argv injection) and exotic chars
/// open a prompt-injection / shell-quoting hole. We require it to start with
/// an alphanumeric and otherwise contain only `[A-Za-z0-9._-]`, matching
/// `^[A-Za-z0-9][A-Za-z0-9._-]*$`. This also rejects path traversal (`../x`),
/// whitespace, and empty names.
pub fn is_valid_worktree_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Detect the project's primary branch. Tries `origin/HEAD`, then `main`,
/// then `master`, falling back to `"main"` (which lets the caller's git
/// command surface the real failure rather than us inventing one).
pub fn detect_main_branch(root: &Path) -> String {
    if let Ok(out) = run_git(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        if out.status_ok {
            if let Some(name) = out.stdout.trim().strip_prefix("origin/") {
                return name.to_string();
            }
        }
    }
    for candidate in ["main", "master"] {
        let exists = run_git(root, &["rev-parse", "--verify", "--quiet", candidate])
            .map(|o| o.status_ok)
            .unwrap_or(false);
        if exists {
            return candidate.to_string();
        }
    }
    "main".to_string()
}

/// Result of running `git -C <root> <args>`. Distinguishes "non-zero exit"
/// (error in the command) from "couldn't even invoke git" (env problem).
pub struct GitOutput {
    pub status_ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_git(root: &Path, args: &[&str]) -> io::Result<GitOutput> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    Ok(GitOutput {
        status_ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Create a worktree at `<root>/.cc-hub-wt/<task>-<name>` on a fresh branch
/// `cc-hub/<task>-<name>` based on `<base>`. Idempotent: a pre-existing
/// worktree is detected from git's stderr and reused so a re-running
/// orchestrator doesn't trip on its own previous attempt.
pub fn create_worktree(
    project_root: &Path,
    task_id: &str,
    name: &str,
    base_branch: &str,
) -> io::Result<PathBuf> {
    let path = worktree_path(project_root, task_id, name);
    let branch = worktree_branch(task_id, name);
    let out = run_git(
        project_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &path.to_string_lossy(),
            base_branch,
        ],
    )?;
    if out.status_ok {
        return Ok(path);
    }
    // git's "already exists" / "is already checked out" messages mean a
    // previous run left this worktree behind; reuse rather than fail.
    let stderr = out.stderr.trim();
    let already = stderr.contains("already exists") || stderr.contains("already checked out");
    if already {
        log::info!(
            "create_worktree: {} already exists, reusing",
            path.display()
        );
        return Ok(path);
    }
    Err(io::Error::other(format!(
        "git worktree add failed: {}",
        stderr
    )))
}

/// Files modified-but-uncommitted in the working tree, from `git status
/// --porcelain -z`. Repo-relative paths, sorted, deduped. Used by the
/// merge pre-flight to detect whether an in-flight merge would clobber
/// the user's local edits.
pub fn dirty_paths(project_root: &Path) -> io::Result<Vec<String>> {
    let out = run_git(project_root, &["status", "--porcelain", "-z"])?;
    if !out.status_ok {
        return Err(io::Error::other(format!(
            "git status failed: {}",
            out.stderr.trim()
        )));
    }
    // -z output: NUL-terminated entries, each starting with two status
    // chars + space, then the path. Renames / copies emit an additional
    // NUL-separated source path with no leading status code; we keep both
    // sides so an overlap on either blocks the merge.
    let mut paths = Vec::new();
    let mut iter = out.stdout.split('\0').filter(|s| !s.is_empty()).peekable();
    while let Some(entry) = iter.next() {
        if entry.len() < 3 {
            continue;
        }
        let code = entry.as_bytes()[0];
        let path = entry[3..].to_string();
        paths.push(path);
        if matches!(code, b'R' | b'C') {
            if let Some(src) = iter.next() {
                paths.push(src.to_string());
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Files changed by `feature_branch` relative to `main_branch`, from
/// `git diff <main>...<feature> --name-only -z` (three-dot — the merge
/// base, matching what git would actually pull in).
pub fn branch_changed_paths(
    project_root: &Path,
    main_branch: &str,
    feature_branch: &str,
) -> io::Result<Vec<String>> {
    let range = format!("{}...{}", main_branch, feature_branch);
    let out = run_git(project_root, &["diff", "--name-only", "-z", &range])?;
    if !out.status_ok {
        return Err(io::Error::other(format!(
            "git diff {} failed: {}",
            range,
            out.stderr.trim()
        )));
    }
    Ok(out
        .stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// Merge `<branch>` into `<main>` from the project root. Performs one
/// pre-flight check before any tree mutation: if any uncommitted
/// working-tree change overlaps a file the feature branch also modified,
/// returns [`MergeOutcome::BlockedByDirtyTree`]. `overlap` lists the
/// repo-relative paths the user must commit, stash, or revert before
/// retrying.
///
/// Cross-orchestrator serialization is enforced one level up by the
/// project-wide merge lock (`merge_lock` module) — `pr merge` acquires it
/// before invoking this function.
///
/// Returns [`MergeOutcome::Conflict`] for the classical content-conflict
/// case where git started the merge and hit overlapping edits in
/// committed history.
///
/// Why we don't auto-stash anymore: an earlier version stashed before
/// merge and popped after, but a popping conflict left raw conflict
/// markers in source files, broke the build, and shifted resolution onto
/// the user without warning. Refusing up front is safer; the user's
/// recipe is one git command (`git stash`, `git commit`, or
/// `git checkout --`) followed by re-running the merge.
pub fn merge_branch(
    project_root: &Path,
    main_branch: &str,
    feature_branch: &str,
) -> io::Result<(MergeOutcome, String, String)> {
    let changed = branch_changed_paths(project_root, main_branch, feature_branch)?;

    // Preflight: refuse if dirty tree overlaps the branch's file set.
    // BTreeSet so the overlap list is stable for tests.
    let dirty: std::collections::BTreeSet<String> =
        dirty_paths(project_root)?.into_iter().collect();
    if !dirty.is_empty() {
        let branch_files: std::collections::BTreeSet<String> = changed.iter().cloned().collect();
        let overlap: Vec<String> = dirty.intersection(&branch_files).cloned().collect();
        if !overlap.is_empty() {
            return Ok((
                MergeOutcome::BlockedByDirtyTree { overlap },
                String::new(),
                String::new(),
            ));
        }
        // Dirty in non-overlapping files only — git carries those changes
        // through the checkout and merge cleanly. No stash needed.
    }

    let checkout = run_git(project_root, &["checkout", main_branch])?;
    if !checkout.status_ok {
        return Err(io::Error::other(format!(
            "git checkout {} failed: {}",
            main_branch,
            checkout.stderr.trim()
        )));
    }
    let msg = format!("cc-hub: merge {} into {}", feature_branch, main_branch);
    let out = run_git(
        project_root,
        &["merge", "--no-ff", "-m", &msg, feature_branch],
    )?;
    let outcome = if out.status_ok {
        MergeOutcome::Ok
    } else {
        let detail = if !out.stderr.trim().is_empty() {
            out.stderr.clone()
        } else {
            out.stdout.clone()
        };
        MergeOutcome::Conflict {
            detail: detail.trim().to_string(),
        }
    };
    Ok((outcome, out.stdout, out.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_name_validation_rejects_unsafe_and_accepts_safe() {
        // Argv-injection / traversal / quoting hazards must be rejected.
        for bad in [
            "", "-foo", "--bar", "../x", "a/b", "a b", ".hidden", "a$b", "a;b",
        ] {
            assert!(!is_valid_worktree_name(bad), "{:?} should be rejected", bad);
        }
        for good in ["fix", "a", "fix-123", "v1.2_x", "FixIt"] {
            assert!(
                is_valid_worktree_name(good),
                "{:?} should be accepted",
                good
            );
        }
    }

    #[test]
    fn worktree_path_includes_task_id() {
        let p = worktree_path(Path::new("/repo"), "t-123", "edit");
        assert_eq!(p, PathBuf::from("/repo/.cc-hub-wt/t-123-edit"));
    }
}
