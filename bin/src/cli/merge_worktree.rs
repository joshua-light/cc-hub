//! `cc-hub merge-worktree` — legacy direct worktree merge helper.

use super::{parse_flags, print_json, require_task, resolve_project_id, CliError};
use cc_hub_lib::ops;
use cc_hub_lib::orchestrator::MergeOutcome;

pub(crate) fn merge_worktree(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let worktree_name = f
        .worktree
        .clone()
        .ok_or_else(|| CliError::Usage("--worktree NAME is required".into()))?;

    let result = ops::worker::merge_worktree(&project_id, &task_id, &worktree_name)?;
    let ops::worker::MergeWorktreeOutcome {
        outcome,
        worktree,
        branch,
        main,
        stdout,
        stderr,
    } = result;

    let mut payload = serde_json::json!({
        "ok": matches!(outcome, MergeOutcome::Ok),
        "worktree": worktree,
        "branch": branch,
        "main": main,
        "stdout": stdout,
        "stderr": stderr,
    });
    if let MergeOutcome::BlockedByDirtyTree { overlap } = &outcome {
        payload["blocked_by_dirty_tree"] = serde_json::json!(true);
        payload["overlap"] = serde_json::json!(overlap);
        payload["recipe"] = serde_json::json!(
            "Commit, stash, or revert the listed paths on the target branch, then re-run `cc-hub merge-worktree`."
        );
    }
    print_json(&payload);

    // The outcome payload was already printed above; use `Reported` so `handle()`
    // doesn't emit a SECOND JSON line on stdout (which would break the
    // one-line-per-call contract for an orchestrator piping to `jq`).
    match outcome {
        MergeOutcome::Ok => Ok(()),
        MergeOutcome::Conflict { .. } => Err(CliError::Reported(
            "merge produced conflicts; resolve in the worktree or main".into(),
        )),
        MergeOutcome::BlockedByDirtyTree { overlap } => Err(CliError::Reported(format!(
            "merge blocked: working tree on `{}` has uncommitted edits in {} file(s) the branch also modified ({}); commit/stash/revert and retry",
            main,
            overlap.len(),
            overlap.join(", ")
        ))),
    }
}
