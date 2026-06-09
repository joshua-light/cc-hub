//! `cc-hub worker wait` — block until worker sessions finish.

use super::{parse_flags, print_json, require_task, resolve_project_id, CliError};
use cc_hub_lib::ops;
use cc_hub_lib::orchestrator;
use std::time::Duration;

// ─── worker ──────────────────────────────────────────────────────────────
//
// `cc-hub worker wait --task ID [--tmux NAME ...] [--worktree NAME ...] [--all] [--timeout-secs N]`
//
// Blocks until the named worker tmux session(s) reach a terminal-for-the-
// orchestrator state — WaitingForInput (Claude end_turn), Question (agent
// blocked on AskUserQuestion), or Inactive (process gone). Replaces the
// orchestrator's tmux capture-pane polling loop, which paid 60–90s of
// LLM-driven latency per spawn; this verb polls scan_sessions() at 500 ms
// and returns within seconds.

pub(crate) fn worker_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("worker <verb>: missing verb (try `wait`)".into()))?;
    match verb.as_str() {
        "wait" => worker_wait(rest),
        other => Err(CliError::Usage(format!(
            "unknown worker verb: {} (try `wait`)",
            other
        ))),
    }
}

/// Resolve the tmux-name targets for `worker wait` from the various
/// selection flags. Returns a deduped Vec of tmux session names.
///
/// Selection is the union of:
///   * each `--tmux NAME`  (must exist in state.workers as a tmux_name)
///   * each `--worktree NAME` (must exist in state.workers as a worktree)
///   * `--all` (every worker on the task)
///
/// Passing none of the three is a usage error. `--all` on a task with
/// zero workers yields an empty Vec (preserved existing behavior — the
/// caller emits ok=true / all_done=true / workers=[]).
fn resolve_wait_targets(
    state: &orchestrator::TaskState,
    tmux_targets: &[String],
    worktree_targets: &[String],
    all: bool,
) -> Result<Vec<String>, CliError> {
    Ok(ops::worker::resolve_wait_targets(
        state,
        tmux_targets,
        worktree_targets,
        all,
    )?)
}

fn worker_wait(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;

    let targets = resolve_wait_targets(&state, &f.tmux_targets, &f.worktree_targets, f.all)?;

    if targets.is_empty() {
        print_json(&serde_json::json!({
            "ok": true,
            "all_done": true,
            "timed_out": false,
            "elapsed_secs": 0,
            "workers": [],
        }));
        return Ok(());
    }

    let timeout = Duration::from_secs(f.timeout_secs.unwrap_or(1800));

    let progress_interval = if f.progress {
        Some(Duration::from_secs(
            f.progress_interval_secs.unwrap_or(5).max(1),
        ))
    } else {
        None
    };

    let outcome = ops::worker::worker_wait(&targets, timeout, progress_interval, |p| {
        print_json(&serde_json::json!({
            "event": "progress",
            "elapsed_secs": p.elapsed_secs,
            "pending": p.pending,
            "done": p.done,
        }));
    });

    print_json(&serde_json::json!({
        "ok": true,
        "all_done": outcome.all_done,
        "timed_out": outcome.timed_out,
        "elapsed_secs": outcome.elapsed_secs,
        "workers": outcome.workers,
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_hub_lib::orchestrator::TaskState;
    use std::path::PathBuf;

    fn make_state_with_three_workers() -> TaskState {
        let mut state = TaskState::new(
            "p-wait".to_string(),
            PathBuf::from("/tmp/proj"),
            "do thing".into(),
        );
        state.task_id = "t-wait".to_string();
        state.workers.push(orchestrator::Worker {
            agent_id: "claude".into(),
            agent_kind: cc_hub_lib::agent::AgentKind::Claude,
            tmux_name: "cchub-1".into(),
            cwd: PathBuf::from("/tmp"),
            worktree: Some("fix".into()),
            readonly: false,
            spawned_at: 0,
        });
        state.workers.push(orchestrator::Worker {
            agent_id: "claude".into(),
            agent_kind: cc_hub_lib::agent::AgentKind::Claude,
            tmux_name: "cchub-2".into(),
            cwd: PathBuf::from("/tmp"),
            worktree: Some("docs".into()),
            readonly: false,
            spawned_at: 0,
        });
        state.workers.push(orchestrator::Worker {
            agent_id: "claude".into(),
            agent_kind: cc_hub_lib::agent::AgentKind::Claude,
            tmux_name: "cchub-ro".into(),
            cwd: PathBuf::from("/tmp"),
            worktree: None,
            readonly: true,
            spawned_at: 0,
        });
        state
    }

    #[test]
    fn resolve_wait_targets_resolves_worktree_to_tmux() {
        let state = make_state_with_three_workers();
        let targets = resolve_wait_targets(&state, &[], &["fix".to_string()], false).expect("ok");
        assert_eq!(targets, vec!["cchub-1".to_string()]);
    }

    #[test]
    fn resolve_wait_targets_unknown_worktree_errors() {
        let state = make_state_with_three_workers();
        let err = resolve_wait_targets(&state, &[], &["missing".to_string()], false)
            .expect_err("must error on unknown worktree");
        match err {
            CliError::Usage(msg) => {
                assert!(
                    msg.contains("missing"),
                    "error must mention the unknown worktree name: {}",
                    msg
                );
                assert!(
                    msg.contains("fix") && msg.contains("docs"),
                    "error must list known worktrees: {}",
                    msg
                );
            }
            other => panic!("expected CliError::Usage, got {:?}", other),
        }
    }

    #[test]
    fn resolve_wait_targets_union_of_flags_deduped() {
        let state = make_state_with_three_workers();
        let targets = resolve_wait_targets(
            &state,
            &["cchub-1".to_string()],
            &["fix".to_string(), "docs".to_string()],
            false,
        )
        .expect("ok");
        assert_eq!(
            targets,
            vec!["cchub-1".to_string(), "cchub-2".to_string()],
            "tmux first, then worktree-resolved tmux names, with dedup preserving first occurrence"
        );
    }

    #[test]
    fn resolve_wait_targets_empty_selection_errors() {
        let state = make_state_with_three_workers();
        let err = resolve_wait_targets(&state, &[], &[], false)
            .expect_err("must error when no selection flags given");
        assert!(matches!(err, CliError::Usage(_)));
    }
}
