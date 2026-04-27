//! Reservations: declare/upgrade/race/stale + merge_branch preflight.
//!
//! These tests use `*_in(home, ...)` variants of the reservations API so the
//! real `~/.cc-hub` is never touched — every test gets its own tempdir-rooted
//! fake home.

use cc_hub_lib::orchestrator::{self, MergeOutcome};
use cc_hub_lib::reservations::{
    self, paths_overlap, Phase, RESERVATION_TTL_SECS,
};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier, Mutex, OnceLock};
use std::thread;

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
fn declare_creates_intended_then_merges_paths() {
    let home = tempfile::tempdir().expect("tempdir");
    let r1 = reservations::declare_in(
        home.path(),
        "p",
        "t-1",
        &["a.rs".into(), "b.rs".into()],
        "owner-1",
        false,
    )
    .expect("declare 1");
    assert_eq!(r1.phase, Phase::Intended);
    assert_eq!(r1.paths, vec!["a.rs", "b.rs"]);
    assert_eq!(r1.worker_id, None);

    // Second declare with replace=false MERGES — `b.rs` is already present
    // and must not duplicate; `c.rs` joins.
    let r2 = reservations::declare_in(
        home.path(),
        "p",
        "t-1",
        &["b.rs".into(), "c.rs".into()],
        "owner-1",
        false,
    )
    .expect("declare 2");
    assert_eq!(r2.paths, vec!["a.rs", "b.rs", "c.rs"]);
}

#[test]
fn declare_replace_overwrites_paths() {
    let home = tempfile::tempdir().expect("tempdir");
    reservations::declare_in(home.path(), "p", "t-1", &["a.rs".into()], "owner", false)
        .expect("declare 1");
    let r = reservations::declare_in(
        home.path(),
        "p",
        "t-1",
        &["x.rs".into(), "y.rs".into()],
        "owner",
        true,
    )
    .expect("declare 2 replace");
    assert_eq!(r.paths, vec!["x.rs", "y.rs"]);
}

#[test]
fn upgrade_to_active_succeeds_when_no_overlap() {
    let home = tempfile::tempdir().expect("tempdir");
    reservations::declare_in(
        home.path(),
        "p",
        "t-1",
        &["lib/foo.rs".into()],
        "owner",
        false,
    )
    .unwrap();
    let outcome = reservations::upgrade_to_active_in(
        home.path(),
        "p",
        "t-1",
        "worker-A",
        &["lib/foo.rs".into()],
    )
    .unwrap();
    match outcome {
        reservations::UpgradeOutcome::Ok(r) => {
            assert_eq!(r.phase, Phase::Active);
            assert_eq!(r.worker_id.as_deref(), Some("worker-A"));
        }
        reservations::UpgradeOutcome::Conflict { task_id, .. } => {
            panic!("expected Ok, got Conflict on {}", task_id);
        }
    }
}

#[test]
fn upgrade_returns_conflict_when_other_task_holds_active() {
    let home = tempfile::tempdir().expect("tempdir");
    // Task A holds an active reservation on lib/foo.rs.
    let r = reservations::upgrade_to_active_in(
        home.path(),
        "p",
        "t-A",
        "worker-A",
        &["lib/foo.rs".into()],
    )
    .unwrap();
    assert!(matches!(r, reservations::UpgradeOutcome::Ok(_)));

    // Task B tries to upgrade with overlapping paths → conflict.
    let outcome = reservations::upgrade_to_active_in(
        home.path(),
        "p",
        "t-B",
        "worker-B",
        &["lib/foo.rs".into()],
    )
    .unwrap();
    match outcome {
        reservations::UpgradeOutcome::Conflict { task_id, paths } => {
            assert_eq!(task_id, "t-A");
            assert_eq!(paths, vec!["lib/foo.rs"]);
        }
        other => panic!("expected Conflict, got {:?}", other),
    }
}

#[test]
fn upgrade_race_only_one_thread_wins() {
    // Two threads race upgrade with overlapping paths against the same
    // home directory. flock serializes them; exactly one must win.
    let home = tempfile::tempdir().expect("tempdir");
    let home_path = home.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(2));

    let h1 = {
        let home = home_path.clone();
        let b = barrier.clone();
        thread::spawn(move || {
            b.wait();
            reservations::upgrade_to_active_in(
                &home,
                "p",
                "t-A",
                "worker-A",
                &["shared.rs".into()],
            )
            .unwrap()
        })
    };
    let h2 = {
        let home = home_path.clone();
        let b = barrier.clone();
        thread::spawn(move || {
            b.wait();
            reservations::upgrade_to_active_in(
                &home,
                "p",
                "t-B",
                "worker-B",
                &["shared.rs".into()],
            )
            .unwrap()
        })
    };

    let r1 = h1.join().unwrap();
    let r2 = h2.join().unwrap();

    let oks = [&r1, &r2]
        .iter()
        .filter(|o| matches!(o, reservations::UpgradeOutcome::Ok(_)))
        .count();
    let conflicts = [&r1, &r2]
        .iter()
        .filter(|o| matches!(o, reservations::UpgradeOutcome::Conflict { .. }))
        .count();
    assert_eq!(oks, 1, "exactly one upgrade should succeed (got {} oks)", oks);
    assert_eq!(conflicts, 1, "the other should be Conflict");
}

#[test]
fn stale_reservations_are_filtered_from_list_and_dont_block_upgrade() {
    let home = tempfile::tempdir().expect("tempdir");
    // Create an active reservation, then forge its last_heartbeat to 0
    // (well past TTL) by editing the JSON directly.
    let _ = reservations::upgrade_to_active_in(
        home.path(),
        "p",
        "t-old",
        "worker-X",
        &["shared.rs".into()],
    )
    .unwrap();

    let path = home.path().join("projects/p/reservations.json");
    let mut file: reservations::ReservationsFile =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for r in file.reservations.iter_mut() {
        r.last_heartbeat = 0;
    }
    fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();

    // list() filters stale by default.
    let visible = reservations::list_in(home.path(), "p", false).unwrap();
    assert!(
        visible.is_empty(),
        "stale reservation should be invisible to list, got {:?}",
        visible
    );
    // include_stale=true surfaces it again.
    let with_stale = reservations::list_in(home.path(), "p", true).unwrap();
    assert_eq!(with_stale.len(), 1, "include_stale should surface it");

    // A fresh upgrade on overlapping paths must succeed (stale doesn't block).
    let outcome = reservations::upgrade_to_active_in(
        home.path(),
        "p",
        "t-new",
        "worker-Y",
        &["shared.rs".into()],
    )
    .unwrap();
    assert!(
        matches!(outcome, reservations::UpgradeOutcome::Ok(_)),
        "stale reservation should not block, got {:?}",
        outcome
    );
}

#[test]
fn overlapping_active_handles_exact_and_directory_prefix() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ = reservations::upgrade_to_active_in(
        home.path(),
        "p",
        "t-A",
        "worker-A",
        &["lib/src/".into(), "bin/exact.rs".into()],
    )
    .unwrap();

    // Exact-file overlap.
    let exact = reservations::overlapping_active_in(
        home.path(),
        "p",
        "t-B",
        &["bin/exact.rs".into()],
    )
    .unwrap();
    assert_eq!(exact.len(), 1, "exact-file overlap should match");
    assert_eq!(exact[0].0, "t-A");

    // Directory-prefix overlap (reservation has trailing slash, query is a
    // file under it).
    let dir_match = reservations::overlapping_active_in(
        home.path(),
        "p",
        "t-B",
        &["lib/src/foo.rs".into()],
    )
    .unwrap();
    assert_eq!(dir_match.len(), 1);
    assert_eq!(dir_match[0].0, "t-A");

    // No overlap.
    let none = reservations::overlapping_active_in(
        home.path(),
        "p",
        "t-B",
        &["unrelated/x.rs".into()],
    )
    .unwrap();
    assert!(none.is_empty());

    // Self is excluded — querying with t-A as excluded should return empty.
    let self_excluded = reservations::overlapping_active_in(
        home.path(),
        "p",
        "t-A",
        &["bin/exact.rs".into()],
    )
    .unwrap();
    assert!(self_excluded.is_empty());
}

#[test]
fn refresh_heartbeat_updates_last_heartbeat() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ = reservations::declare_in(
        home.path(),
        "p",
        "t-1",
        &["a.rs".into()],
        "owner",
        false,
    )
    .unwrap();
    // Set an old heartbeat by hand so the refresh produces a measurable
    // delta.
    let path = home.path().join("projects/p/reservations.json");
    let mut file: reservations::ReservationsFile =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let before = orchestrator::now_unix_secs() - 100;
    for r in file.reservations.iter_mut() {
        r.last_heartbeat = before;
    }
    fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();

    reservations::refresh_heartbeat_in(home.path(), "p", "t-1").unwrap();

    let after_file: reservations::ReservationsFile =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let entry = after_file.reservations.first().unwrap();
    assert!(
        entry.last_heartbeat > before,
        "heartbeat should advance: {} > {}",
        entry.last_heartbeat,
        before
    );
}

#[test]
fn release_with_worker_keeps_other_entries_release_without_drops_all() {
    let home = tempfile::tempdir().expect("tempdir");
    reservations::declare_in(
        home.path(),
        "p",
        "t-1",
        &["a.rs".into()],
        "owner",
        false,
    )
    .unwrap();
    reservations::upgrade_to_active_in(
        home.path(),
        "p",
        "t-1",
        "worker-1",
        &["a.rs".into()],
    )
    .unwrap();

    // Release just the worker — intended umbrella entry stays.
    reservations::release_in(home.path(), "p", "t-1", Some("worker-1")).unwrap();
    let after = reservations::list_in(home.path(), "p", false).unwrap();
    assert_eq!(after.len(), 1, "intended umbrella should remain");
    assert_eq!(after[0].phase, Phase::Intended);

    // Release everything for the task.
    reservations::release_in(home.path(), "p", "t-1", None).unwrap();
    let empty = reservations::list_in(home.path(), "p", false).unwrap();
    assert!(empty.is_empty(), "all entries should be gone");
}

#[test]
fn paths_overlap_helper_directly() {
    assert!(paths_overlap("a.rs", "a.rs"));
    assert!(paths_overlap("lib/", "lib/foo.rs"));
    assert!(paths_overlap("lib/foo.rs", "lib/"));
    assert!(!paths_overlap("lib", "lib/foo.rs"));
    assert!(!paths_overlap("lib/", "src/foo.rs"));
}

#[test]
fn ttl_constant_matches_doc() {
    // Sanity: the design doc commits to 600 seconds (10 minutes). If we
    // ever tune this value, the prompt + recipes need to stay coherent.
    assert_eq!(RESERVATION_TTL_SECS, 600);
}

// ─── merge_branch preflight integration ──────────────────────────────────
//
// These exercise the new BlockedByActiveOrchestrator path. They need git +
// they need to land a reservation in the *real* `~/.cc-hub` because
// merge_branch's reservation lookup goes through the public API (which
// uses cc_hub_home(), not an injectable home). HOME is overridden to a
// tempdir so we never write into the user's actual state.

/// Tests that override `HOME` must serialise — env vars are process-global,
/// and cargo's default test runner is multi-threaded. Without this lock,
/// two tests racing to set `HOME` will read each other's reservations.
fn home_mutex() -> &'static Mutex<()> {
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(()))
}

fn with_fake_home<F: FnOnce()>(f: F) {
    let _g = home_mutex().lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().expect("tempdir HOME");
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());
    f();
    match prev {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn merge_branch_blocked_by_active_orchestrator() {
    if !git_available() {
        eprintln!("skipping: git not on PATH");
        return;
    }
    with_fake_home(|| {
        let repo = init_repo();
        let root = repo.path();

        // Build a feature branch that touches `seed.txt`.
        let wt = orchestrator::create_worktree(root, "t-feat", "edit", "main").unwrap();
        fs::write(wt.join("seed.txt"), "feature edit\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "-q", "-m", "edit seed"]);

        // Another task (`t-other`) holds an active reservation on the same
        // file.
        let _ = reservations::upgrade_to_active(
            "test-proj",
            "t-other",
            "worker-X",
            &["seed.txt".into()],
        )
        .expect("seed reservation");

        let branch = orchestrator::worktree_branch("t-feat", "edit");
        let (outcome, _stdout, _stderr) = orchestrator::merge_branch(
            root,
            "main",
            &branch,
            "test-proj",
            "t-feat",
        )
        .unwrap();

        match outcome {
            MergeOutcome::BlockedByActiveOrchestrator { task_id, paths } => {
                assert_eq!(task_id, "t-other");
                assert!(
                    paths.iter().any(|p| p == "seed.txt"),
                    "blocker paths should include seed.txt, got {:?}",
                    paths
                );
            }
            other => panic!("expected BlockedByActiveOrchestrator, got {:?}", other),
        }

        // The merge must NOT have run — main should still point at the
        // original seed contents.
        let on_main = fs::read_to_string(root.join("seed.txt")).unwrap();
        assert_eq!(on_main, "seed\n", "main should be untouched by a blocked merge");
    });
}

#[test]
fn merge_branch_proceeds_when_only_self_holds_reservation() {
    if !git_available() {
        return;
    }
    with_fake_home(|| {
        let repo = init_repo();
        let root = repo.path();

        let wt = orchestrator::create_worktree(root, "t-self", "edit", "main").unwrap();
        fs::write(wt.join("seed.txt"), "self edit\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "-q", "-m", "self edit"]);

        // The task's own reservation must NOT block its own merge.
        let _ = reservations::upgrade_to_active(
            "test-proj",
            "t-self",
            "worker-self",
            &["seed.txt".into()],
        )
        .unwrap();

        let branch = orchestrator::worktree_branch("t-self", "edit");
        let (outcome, _stdout, _stderr) =
            orchestrator::merge_branch(root, "main", &branch, "test-proj", "t-self").unwrap();
        assert!(matches!(outcome, MergeOutcome::Ok), "expected Ok, got {:?}", outcome);
    });
}
