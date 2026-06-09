//! Shared fixtures for CLI verb tests: a serialised $HOME sandbox and
//! git-repo/task seeding helpers used by both the `task` and `pr` suites.

use cc_hub_lib::orchestrator::{self, TaskState, TaskStatus};

pub(crate) fn with_tempdir_home<F: FnOnce()>(f: F) {
    // $HOME / CLAUDE_CONFIG_DIR are process-global; serialise on the
    // crate-wide lock so these tests don't race env-mutating tests in
    // other modules (e.g. `extract_claude_config_dir`'s).
    let _g = crate::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());
    f();
    match prev {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

pub(crate) fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub(crate) fn git_run(root: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {} failed: stdout={} stderr={}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

pub(crate) fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git_run(root, &["init", "-q", "-b", "main"]);
    git_run(root, &["config", "user.email", "cc-hub-test@example.com"]);
    git_run(root, &["config", "user.name", "cc-hub-test"]);
    git_run(root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
    git_run(root, &["add", "."]);
    git_run(root, &["commit", "-q", "-m", "seed"]);
    dir
}

pub(crate) fn seed_task_with_worktree(
    project_root: &std::path::Path,
    status: TaskStatus,
) -> (String, String, std::path::PathBuf) {
    let project_id = orchestrator::ensure_project_registered(project_root, "test-proj")
        .expect("register project");
    let task_id = "t-delete-test".to_string();
    let worktree_name = "wt".to_string();

    let wt_path = orchestrator::create_worktree(project_root, &task_id, &worktree_name, "main")
        .expect("create worktree");

    let mut state = TaskState::new(
        project_id.clone(),
        project_root.to_path_buf(),
        "do thing".into(),
    );
    state.task_id = task_id.clone();
    state.status = status;
    state.workers.push(orchestrator::Worker {
        agent_id: "claude".into(),
        agent_kind: cc_hub_lib::agent::AgentKind::Claude,
        tmux_name: "cc-hub-test-wkr".into(),
        cwd: wt_path.clone(),
        worktree: Some(worktree_name),
        readonly: false,
        spawned_at: 0,
    });
    orchestrator::write_task_state(&state).expect("write state");

    (project_id, task_id, wt_path)
}
