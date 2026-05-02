//! End-to-end tests for the orchestrator's git mechanics. Each test stands
//! up a real git repo in a tempdir, exercises `create_worktree` /
//! `merge_branch` against it, and asserts on the resulting state.
//!
//! These run only when `git` is on `PATH` — skipped silently otherwise so
//! the suite stays passable on bare CI images.

use cc_hub_lib::orchestrator::{self, MergeOutcome};
use std::fs;
use std::path::Path;
use std::process::Command;

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("spawn git");
    if !out.status.success() {
        panic!(
            "git {} failed: stdout={} stderr={}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    run(root, &["init", "-q", "-b", "main"]);
    run(root, &["config", "user.email", "cc-hub-test@example.com"]);
    run(root, &["config", "user.name", "cc-hub-test"]);
    run(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("seed.txt"), "seed\n").unwrap();
    run(root, &["add", "."]);
    run(root, &["commit", "-q", "-m", "seed"]);
    dir
}

#[test]
fn create_worktree_makes_branch_and_dir() {
    if !git_available() {
        eprintln!("skipping: git not on PATH");
        return;
    }
    let repo = init_repo();
    let root = repo.path();

    let path =
        orchestrator::create_worktree(root, "t-1", "feature", "main").expect("create_worktree");
    assert!(path.exists(), "worktree path should exist");
    assert_eq!(
        path,
        orchestrator::worktree_path(root, "t-1", "feature"),
        "path should match the documented convention",
    );

    // Branch should exist at the expected name.
    let branch = orchestrator::worktree_branch("t-1", "feature");
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", &branch])
        .output()
        .expect("git rev-parse");
    assert!(
        out.status.success(),
        "branch {} should exist after create_worktree",
        branch,
    );
}

#[test]
fn create_worktree_is_idempotent() {
    if !git_available() {
        return;
    }
    let repo = init_repo();
    let root = repo.path();

    let p1 = orchestrator::create_worktree(root, "t-2", "x", "main").unwrap();
    let p2 = orchestrator::create_worktree(root, "t-2", "x", "main").unwrap();
    assert_eq!(p1, p2, "second call should reuse the same path, not error");
}

#[test]
fn merge_branch_clean_succeeds() {
    if !git_available() {
        return;
    }
    let repo = init_repo();
    let root = repo.path();

    let wt = orchestrator::create_worktree(root, "t-3", "edit", "main").unwrap();
    fs::write(wt.join("new.txt"), "hello\n").unwrap();
    run(&wt, &["add", "."]);
    run(&wt, &["commit", "-q", "-m", "add new.txt"]);

    let branch = orchestrator::worktree_branch("t-3", "edit");
    let (outcome, _stdout, _stderr) = orchestrator::merge_branch(root, "main", &branch).unwrap();
    assert!(
        matches!(outcome, MergeOutcome::Ok),
        "expected Ok, got {:?}",
        outcome
    );
    assert!(
        root.join("new.txt").exists(),
        "merged file should be in main"
    );
}

#[test]
fn merge_branch_conflict_returns_conflict_outcome() {
    if !git_available() {
        return;
    }
    let repo = init_repo();
    let root = repo.path();

    // Worktree edits seed.txt one way…
    let wt = orchestrator::create_worktree(root, "t-4", "fork", "main").unwrap();
    fs::write(wt.join("seed.txt"), "from worktree\n").unwrap();
    run(&wt, &["add", "."]);
    run(&wt, &["commit", "-q", "-m", "wt edit"]);

    // …and main edits the same file the other way.
    fs::write(root.join("seed.txt"), "from main\n").unwrap();
    run(root, &["add", "."]);
    run(root, &["commit", "-q", "-m", "main edit"]);

    let branch = orchestrator::worktree_branch("t-4", "fork");
    let (outcome, _stdout, _stderr) = orchestrator::merge_branch(root, "main", &branch).unwrap();
    match outcome {
        MergeOutcome::Conflict { detail } => {
            assert!(!detail.is_empty(), "conflict detail should be populated");
        }
        other => panic!("expected Conflict, got {:?}", other),
    }
}

#[test]
fn merge_branch_blocks_when_dirty_overlap() {
    // Pre-flight: an uncommitted edit on `main` to a file the feature
    // branch also modified must abort the merge before any tree mutation.
    if !git_available() {
        return;
    }
    let repo = init_repo();
    let root = repo.path();

    // Branch modifies new.txt
    let wt = orchestrator::create_worktree(root, "t-5", "overlap", "main").unwrap();
    fs::write(wt.join("new.txt"), "from branch\n").unwrap();
    run(&wt, &["add", "."]);
    run(&wt, &["commit", "-q", "-m", "branch edit"]);

    // Main has the same file with an uncommitted edit
    fs::write(root.join("new.txt"), "uncommitted on main\n").unwrap();

    let branch = orchestrator::worktree_branch("t-5", "overlap");
    let (outcome, _, _) = orchestrator::merge_branch(root, "main", &branch).unwrap();
    match outcome {
        MergeOutcome::BlockedByDirtyTree { overlap } => {
            assert_eq!(overlap, vec!["new.txt".to_string()]);
        }
        other => panic!("expected BlockedByDirtyTree, got {:?}", other),
    }

    // The dirty edit on main is preserved verbatim — pre-flight must not
    // touch the working tree.
    let body = fs::read_to_string(root.join("new.txt")).unwrap();
    assert_eq!(body, "uncommitted on main\n");
}

#[test]
fn merge_branch_proceeds_when_dirty_non_overlap() {
    // A dirty tree on a file the branch does NOT touch is fine — git
    // carries it through the checkout/merge. No stash needed, no refusal.
    if !git_available() {
        return;
    }
    let repo = init_repo();
    let root = repo.path();

    // Seed a second tracked file so we have something to dirty on main.
    fs::write(root.join("untouched.txt"), "v1\n").unwrap();
    run(root, &["add", "."]);
    run(root, &["commit", "-q", "-m", "add untouched"]);

    // Branch modifies branch_only.txt
    let wt = orchestrator::create_worktree(root, "t-6", "disjoint", "main").unwrap();
    fs::write(wt.join("branch_only.txt"), "branch wrote this\n").unwrap();
    run(&wt, &["add", "."]);
    run(&wt, &["commit", "-q", "-m", "branch only"]);

    // Main has an uncommitted edit to a DIFFERENT file
    fs::write(root.join("untouched.txt"), "uncommitted change\n").unwrap();

    let branch = orchestrator::worktree_branch("t-6", "disjoint");
    let (outcome, _, _) = orchestrator::merge_branch(root, "main", &branch).unwrap();
    assert!(
        matches!(outcome, MergeOutcome::Ok),
        "expected Ok, got {:?}",
        outcome
    );

    // Branch's file is now in main
    assert!(root.join("branch_only.txt").exists());
    // Dirty edit on main survived the merge intact
    let body = fs::read_to_string(root.join("untouched.txt")).unwrap();
    assert_eq!(body, "uncommitted change\n");
}

#[test]
fn detect_main_branch_picks_master_when_only_master_exists() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init", "-q", "-b", "master"]);
    run(root, &["config", "user.email", "x@y.z"]);
    run(root, &["config", "user.name", "x"]);
    run(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("a.txt"), "a").unwrap();
    run(root, &["add", "."]);
    run(root, &["commit", "-q", "-m", "init"]);

    assert_eq!(orchestrator::detect_main_branch(root), "master");
}
