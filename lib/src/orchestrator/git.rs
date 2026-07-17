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
    // previous run left *something* behind. Two distinct states hide here:
    //   1. the worktree dir is still present  → reuse it as-is;
    //   2. only the `cc-hub/*` branch survived (gc, `remove_task_worktrees`,
    //      or a manual rm dropped the dir but branches are deleted
    //      separately) → the dir is gone, so returning Ok would spawn a
    //      worker into a nonexistent cwd. Re-attach a worktree to the
    //      surviving branch instead.
    let stderr = out.stderr.trim();
    let already = stderr.contains("already exists") || stderr.contains("already checked out");
    if already {
        if path.exists() {
            log::info!(
                "create_worktree: {} already exists, reusing",
                path.display()
            );
            return Ok(path);
        }
        return reattach_worktree(project_root, &path, &branch);
    }
    Err(io::Error::other(format!(
        "git worktree add failed: {}",
        stderr
    )))
}

/// Re-attach a worktree at `path` to the pre-existing branch `branch` (no
/// `-b`, so commits already on the branch are preserved). Used when the
/// worktree dir was removed but its `cc-hub/*` branch outlived it. A stale
/// worktree admin entry (dir removed with plain `rm` rather than `git
/// worktree remove`) leaves the path "missing but already registered" / the
/// branch still checked out; we clear it with `git worktree prune` and retry
/// once.
fn reattach_worktree(project_root: &Path, path: &Path, branch: &str) -> io::Result<PathBuf> {
    let path_str = path.to_string_lossy();
    let attach = run_git(project_root, &["worktree", "add", &path_str, branch])?;
    if attach.status_ok {
        log::info!(
            "create_worktree: re-attached {} to surviving branch {}",
            path.display(),
            branch
        );
        return Ok(path.to_path_buf());
    }
    // A stale admin entry surfaces under a few git phrasings depending on
    // version: a plain-rm'd dir is "a missing but already registered
    // worktree"; a still-registered checkout is "already checked out" /
    // "already used by worktree". All clear with a prune.
    let stale = attach.stderr.contains("missing but already registered")
        || attach.stderr.contains("already checked out")
        || attach.stderr.contains("already used by worktree");
    if stale {
        let _ = run_git(project_root, &["worktree", "prune"]);
        let retry = run_git(project_root, &["worktree", "add", &path_str, branch])?;
        if retry.status_ok {
            log::info!(
                "create_worktree: re-attached {} to {} after prune",
                path.display(),
                branch
            );
            return Ok(path.to_path_buf());
        }
        return Err(io::Error::other(format!(
            "git worktree re-attach (after prune) failed: {}",
            retry.stderr.trim()
        )));
    }
    Err(io::Error::other(format!(
        "git worktree re-attach failed: {}",
        attach.stderr.trim()
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

    /// Init a bare-minimum git repo with one commit in a tempdir.
    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run_git(root, &["init", "-q", "-b", "main"]).expect("git init");
        run_git(root, &["config", "user.email", "t@example.com"]).expect("email");
        run_git(root, &["config", "user.name", "t"]).expect("name");
        run_git(root, &["config", "commit.gpgsign", "false"]).expect("gpgsign");
        std::fs::write(root.join("seed.txt"), b"seed").expect("seed");
        run_git(root, &["add", "."]).expect("add");
        run_git(root, &["commit", "-q", "-m", "seed"]).expect("commit");
        tmp
    }

    fn branch_exists(root: &Path, branch: &str) -> bool {
        run_git(root, &["rev-parse", "--verify", "--quiet", branch])
            .map(|o| o.status_ok)
            .unwrap_or(false)
    }

    // The idempotent-reuse path must not hand back a path that no longer
    // exists. When `git worktree remove` drops the dir but leaves the
    // `cc-hub/*` branch, a re-run must re-attach a fresh worktree to that
    // branch rather than returning Ok on a missing cwd.
    #[test]
    fn create_worktree_reattaches_when_only_branch_survives() {
        let repo = init_repo();
        let root = repo.path();

        let p1 = create_worktree(root, "t-x", "edit", "main").expect("first create");
        assert!(p1.exists());
        let branch = worktree_branch("t-x", "edit");

        // Drop the worktree but keep the branch (mirrors gc /
        // remove_task_worktrees, which never delete the branch).
        run_git(
            root,
            &["worktree", "remove", "--force", &p1.to_string_lossy()],
        )
        .expect("worktree remove");
        assert!(!p1.exists(), "dir should be gone after remove");
        assert!(
            branch_exists(root, &branch),
            "branch must survive the remove"
        );

        let p2 = create_worktree(root, "t-x", "edit", "main").expect("re-create");
        assert_eq!(p1, p2);
        assert!(p2.exists(), "re-attached worktree dir must exist");
    }

    // A stale worktree admin entry (dir removed with a plain `rm`, not `git
    // worktree remove`) makes git report the branch as still checked out.
    // The re-attach must prune the dead entry and retry.
    #[test]
    fn create_worktree_reattaches_after_pruning_stale_registration() {
        let repo = init_repo();
        let root = repo.path();

        let p1 = create_worktree(root, "t-y", "edit", "main").expect("first create");
        assert!(p1.exists());

        // Plain rm leaves the admin entry in .git/worktrees behind.
        std::fs::remove_dir_all(&p1).expect("rm -rf worktree dir");
        assert!(!p1.exists());

        let p2 = create_worktree(root, "t-y", "edit", "main").expect("re-create over stale");
        assert_eq!(p1, p2);
        assert!(
            p2.exists(),
            "worktree dir must exist after prune + re-attach"
        );
    }
}
