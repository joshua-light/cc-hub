//! `cc-hub pr ...` — local PR review/merge flow.

use super::{parse_flags, print_json, require_task, resolve_project_id, CliError};
use cc_hub_lib::ops;

// ─── pr ──────────────────────────────────────────────────────────────────
//
// PR-flow primitives. The orchestrator opens a PR with `pr create`, the user
// reviews in the TUI (or via `pr approve` / `pr request-changes` from the
// CLI), and merging is serialized through the per-project merge lock:
//
//   pr create   → task: Running → Review,  PR: (new, Open)
//   pr request-changes → task: Review → Running, PR: ChangesRequested
//   pr reopen   → task: Running → Review, PR: ChangesRequested → Open
//   pr approve  → PR: Open|ChangesRequested → Approved (task stays Review)
//   pr merge    → acquires merge.lock, merges main → branch then branch →
//                 main; lock stays held; task: Review → Merging
//   pr finalize → releases merge.lock; task: Merging → Done; PR: Merged
//
// `pr show` is a read-only convenience for inspection / scripting.

pub(crate) fn pr_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage(
            "pr <verb>: missing verb (try `create`, `show`, `approve`, `request-changes`, `reopen`, `comment`, `close`, `merge`, `continue`, `lock-phase`, `finalize`)".into(),
        )
    })?;
    match verb.as_str() {
        "create" => pr_create(rest),
        "show" => pr_show(rest),
        "approve" => pr_approve(rest),
        "request-changes" => pr_request_changes(rest),
        "reopen" => pr_reopen(rest),
        "comment" => pr_comment(rest),
        "close" => pr_close(rest),
        "merge" => pr_merge(rest),
        "continue" => pr_continue(rest),
        "lock-phase" => pr_lock_phase(rest),
        "finalize" => pr_finalize(rest),
        other => Err(CliError::Usage(format!(
            "unknown pr verb: {} (try `create`, `show`, `approve`, `request-changes`, `reopen`, `comment`, `close`, `merge`, `continue`, `lock-phase`, `finalize`)",
            other
        ))),
    }
}

pub(crate) fn pr_to_json(pr: &cc_hub_lib::pr::PullRequest) -> serde_json::Value {
    pr_to_json_filtered(pr, None)
}

fn pr_to_json_filtered(
    pr: &cc_hub_lib::pr::PullRequest,
    comments_since: Option<i64>,
) -> serde_json::Value {
    let total = pr.comments.len();
    let (comments, returned): (serde_json::Value, usize) = match comments_since {
        Some(threshold) => {
            let filtered: Vec<&cc_hub_lib::pr::Comment> =
                pr.comments.iter().filter(|c| c.at >= threshold).collect();
            let len = filtered.len();
            (serde_json::json!(filtered), len)
        }
        None => (serde_json::json!(pr.comments), total),
    };
    serde_json::json!({
        "id": pr.id,
        "task_id": pr.task_id,
        "project_id": pr.project_id,
        "branch": pr.branch,
        "base": pr.base,
        "title": pr.title,
        "description": pr.description,
        "review_state": pr.review_state.as_str(),
        "comments": comments,
        "comments_total": total,
        "comments_returned": returned,
        "approved_at_branch_sha": pr.approved_at_branch_sha,
        "approved_at_base_sha": pr.approved_at_base_sha,
        "created_at": pr.created_at,
        "updated_at": pr.updated_at,
    })
}

fn pr_create(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let worktree_name = f
        .worktree
        .clone()
        .ok_or_else(|| CliError::Usage("--worktree NAME is required".into()))?;
    let title = f
        .title
        .clone()
        .ok_or_else(|| CliError::Usage("--title is required".into()))?;
    let description = f.description.clone().unwrap_or_default();

    let pr = ops::pr::pr_create(&project_id, &task_id, &worktree_name, title, description)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_show(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let pr = cc_hub_lib::pr::read_pr(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| CliError::NotFound("no PR for this task".into()))?;
    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json_filtered(&pr, f.comments_since),
    }));
    Ok(())
}

fn pr_approve(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let pr = ops::pr::pr_approve(&project_id, &task_id)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_request_changes(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let comment = f
        .comment
        .clone()
        .ok_or_else(|| CliError::Usage("--comment is required".into()))?;
    let author = f.author.clone().unwrap_or_else(|| "user".into());

    let pr = ops::pr::pr_request_changes(&project_id, &task_id, comment, author)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_reopen(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let comment = f.comment.clone();
    let author = f.author.clone().unwrap_or_else(|| "orchestrator".into());

    let pr = ops::pr::pr_reopen(&project_id, &task_id, comment, author)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_comment(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let body = f
        .comment
        .clone()
        .ok_or_else(|| CliError::Usage("--comment is required".into()))?;
    let author = f.author.clone().unwrap_or_else(|| "orchestrator".into());

    let pr = ops::pr::pr_comment(&project_id, &task_id, body, author)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

/// Non-destructive "abandon" path: flip the PR to Closed, mark the task Done,
/// drop the merge lock if held, and tear down sessions. Preserves the review
/// record (comments + history) instead of deleting the task.
fn pr_close(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let comment = f.comment.clone();
    let author = f.author.clone().unwrap_or_else(|| "user".into());

    let pr = ops::pr::pr_close(&project_id, &task_id, comment, author)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
        "status": "done",
    }));
    Ok(())
}

fn pr_merge(args: &[String]) -> Result<(), CliError> {
    use ops::pr::MergeOutcomeOp;

    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let outcome = ops::pr::pr_merge(&project_id, &task_id, f.wait, f.timeout_secs)?;

    match outcome {
        MergeOutcomeOp::Locked(holder) => {
            let mut payload = serde_json::json!({
                "ok": false,
                "locked": true,
                "holder_task": holder.holder_task,
                "since": holder.since,
                "phase": holder.phase,
                "age_seconds": holder.age_seconds,
                "recipe": "Another task currently holds the merge lock. Re-run with `--wait` to block until it releases, or poll `cc-hub pr merge` manually.",
            });
            if holder.timed_out {
                payload["timed_out"] = serde_json::Value::Bool(true);
                payload["recipe"] = serde_json::Value::String(
                    "Merge lock still held after the wait timeout. Re-run `cc-hub pr merge --wait` (optionally with `--timeout-secs N`) or investigate why the holder is stuck.".into(),
                );
            }
            print_json(&payload);
            Err(CliError::Reported(format!(
                "merge lock held by task {}",
                holder.holder_task
            )))
        }
        MergeOutcomeOp::MissingWorktree { worktree, branch } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "preflight",
                "kind": "missing_worktree",
                "worktree": worktree,
                "recipe": "The feature branch worktree no longer exists (removed by gc or manual cleanup). Recreate it (`cc-hub spawn-worker`) on the same branch before re-running `cc-hub pr merge`, or close the PR. The merge lock has been released.",
            }));
            Err(CliError::Reported(format!(
                "worktree for branch {} no longer exists",
                branch
            )))
        }
        MergeOutcomeOp::DirtyWorktree { worktree, dirty } => {
            let dirty_len = dirty.len();
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "preflight",
                "kind": "dirty_worktree",
                "worktree": worktree,
                "dirty": dirty,
                "recipe": "The feature-branch worktree has uncommitted changes; cc-hub won't merge base into a dirty tree. Commit or stash the listed paths in the worktree, then re-run `cc-hub pr merge`. The merge lock has been released.",
            }));
            Err(CliError::Reported(format!(
                "merge blocked: feature-branch worktree has {} uncommitted path(s)",
                dirty_len
            )))
        }
        MergeOutcomeOp::DemotedConflict {
            conflicting_paths,
            stdout,
            stderr,
        } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "merge_main_into_branch",
                "demoted_to": "open",
                "conflicting_paths": conflicting_paths,
                "stdout": stdout,
                "stderr": stderr,
                "recipe": "Resolve conflicts in the worktree, commit the resolution, then ask the reviewer to re-approve before re-running `cc-hub pr merge`. The merge lock has been released.",
            }));
            Err(CliError::Reported(
                "conflict merging main into the feature branch — PR demoted to Open".into(),
            ))
        }
        MergeOutcomeOp::BlockedByDirtyTree { overlap } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "preflight",
                "blocked_by_dirty_tree": true,
                "overlap": overlap,
                "recipe": "Commit, stash, or revert the listed paths on the target branch, then re-run `cc-hub pr merge`. The merge lock has been released.",
            }));
            Err(CliError::Reported(
                "merge blocked: working tree on target branch has overlapping uncommitted edits"
                    .into(),
            ))
        }
        MergeOutcomeOp::ConflictIntoMain {
            conflicting_paths,
            stdout,
            stderr,
        } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "merge_branch_into_main",
                "conflicting_paths": conflicting_paths,
                "stdout": stdout,
                "stderr": stderr,
                "recipe": "Unexpected conflict merging into main (the merge lock should have prevented this — investigate before retrying).",
            }));
            Err(CliError::Reported("conflict merging into main".into()))
        }
        MergeOutcomeOp::Merged {
            branch,
            base,
            stdout,
            restored_ref,
        } => {
            print_json(&serde_json::json!({
                "ok": true,
                "phase": "merged",
                "branch": branch,
                "base": base,
                "stdout": stdout,
                "restored_ref": restored_ref,
                "next": "Run /simplify, then /bump, then `cc-hub pr finalize --task <id>` to release the merge lock and mark the task done.",
            }));
            Ok(())
        }
    }
}

/// `cc-hub pr continue --task ID [--project-id ID]`
///
/// Recovery / re-ping verb. Re-sends [`build_review_approval_prompt`] (the
/// same merge-flow nudge the TUI dispatches when a human approves a PR) to the
/// task's orchestrator. The common failure mode this recovers from: the user
/// approved a PR but the orchestrator never picked up the merge (it was busy,
/// the notify was dropped, or the session was restarted).
///
/// Idempotent — re-running just re-pings; nothing destructive happens. When
/// the orchestrator session is dead it returns
/// `{ok:false, orchestrator_alive:false}` with a recipe telling the user to
/// resurrect the orchestrator or `task delete --force` the wedged task. When
/// the pane is busy the prompt cannot be injected; the verb reports that
/// (`sent:false, pane_busy:true`) so the caller knows to retry once idle.
fn pr_continue(args: &[String]) -> Result<(), CliError> {
    use ops::pr::ContinueOutcome;

    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    match ops::pr::pr_continue(&project_id, &task_id)? {
        ContinueOutcome::NoOrchestratorTmux => {
            print_json(&serde_json::json!({
                "ok": false,
                "task_id": task_id,
                "orchestrator_alive": false,
                "reason": "no_orchestrator_tmux",
                "recipe": "This task has no orchestrator tmux recorded — it was never started or its session name was cleared. Resurrect the orchestrator (`cc-hub orchestrate start --task <id>`), or if the merge is wedged, `cc-hub task delete --force --task <id>` to recover.",
            }));
            Err(CliError::Reported(format!(
                "task {} has no orchestrator tmux to continue",
                task_id
            )))
        }
        ContinueOutcome::OrchestratorDead { tmux } => {
            print_json(&serde_json::json!({
                "ok": false,
                "task_id": task_id,
                "orchestrator_tmux": tmux,
                "orchestrator_alive": false,
                "reason": "orchestrator_dead",
                "recipe": "The orchestrator session is dead. Resurrect it (`cc-hub orchestrate start --task <id>`) then re-run `cc-hub pr continue`, or `cc-hub task delete --force --task <id>` to tear down a wedged merge (releases the merge lock).",
            }));
            Err(CliError::Reported(format!(
                "orchestrator [{}] is not live",
                tmux
            )))
        }
        ContinueOutcome::PaneBusy { tmux } => {
            print_json(&serde_json::json!({
                "ok": true,
                "task_id": task_id,
                "orchestrator_tmux": tmux,
                "orchestrator_alive": true,
                "sent": false,
                "pane_busy": true,
                "recipe": "The orchestrator is alive but its pane is busy (mid-turn). Re-run `cc-hub pr continue` once it's idle.",
            }));
            Ok(())
        }
        ContinueOutcome::Sent { tmux } => {
            print_json(&serde_json::json!({
                "ok": true,
                "task_id": task_id,
                "orchestrator_tmux": tmux,
                "orchestrator_alive": true,
                "sent": true,
            }));
            Ok(())
        }
    }
}

fn pr_lock_phase(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let phase_raw = f.phase.clone().ok_or_else(|| {
        CliError::Usage("--phase is required (merging|simplify|bump|finalize-pending)".into())
    })?;
    let phase = cc_hub_lib::merge_lock::MergePhase::parse(&phase_raw).ok_or_else(|| {
        CliError::Usage(format!(
            "--phase: unknown value '{}' (expected merging|simplify|bump|finalize-pending)",
            phase_raw
        ))
    })?;
    let phase_str = ops::pr::pr_lock_phase(&project_id, &task_id, phase)?;
    print_json(&serde_json::json!({
        "ok": true,
        "task_id": task_id,
        "project_id": project_id,
        "phase": phase_str,
    }));
    Ok(())
}

/// After the build gate passes, release the merge lock BEFORE flipping the PR
/// and task to terminal states. If the release fails the task stays in
/// `Merging` so a re-run can complete the transition — otherwise a transient
/// FS error would strand a `Done` task as the lock holder and block the
/// project's merge queue until `STALE_TTL_SECS`.
fn pr_finalize(args: &[String]) -> Result<(), CliError> {
    use ops::pr::FinalizeOutcome;

    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let outcome = ops::pr::pr_finalize(
        &project_id,
        &task_id,
        ops::pr::FinalizeOpts {
            build_cmd: f.build_cmd.clone(),
            skip_build: f.skip_build,
            keep_tmux: f.keep_tmux,
        },
    )?;

    match outcome {
        FinalizeOutcome::AlreadyMerged => {
            print_json(&serde_json::json!({
                "ok": true,
                "noop": true,
                "task_id": task_id,
                "status": "done",
                "reason": "pr already merged",
            }));
            Ok(())
        }
        FinalizeOutcome::BuildFailed {
            command,
            stderr_tail,
        } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "build",
                "command": command,
                "stderr": stderr_tail,
                "recipe": "Build failed on main after /simplify or /bump; fix in the working tree, commit, then re-run cc-hub pr finalize.",
            }));
            Err(CliError::Reported("build gate failed".into()))
        }
        FinalizeOutcome::Finalized {
            released,
            build_skipped,
            tmux_kept,
        } => {
            print_json(&serde_json::json!({
                "ok": true,
                "released": released,
                "task_id": task_id,
                "status": "done",
                "build_skipped": build_skipped,
                "tmux_kept": tmux_kept,
            }));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::dispatch;
    use crate::cli::test_util::{
        git_available, init_repo, seed_task_with_worktree, with_tempdir_home,
    };
    use cc_hub_lib::orchestrator::{self, TaskState, TaskStatus};
    use cc_hub_lib::pr;
    use std::path::PathBuf;

    #[test]
    fn pr_finalize_build_failure_keeps_lock_and_comments_pr() {
        with_tempdir_home(|| {
            let project_id = "p-build".to_string();
            let task_id = "t-build".to_string();

            // Project root lives outside $HOME so we can drop a build script in it.
            let project_dir = tempfile::tempdir().expect("project tempdir");
            let project_root = project_dir.path().to_path_buf();

            // State: task is Merging, /simplify + /bump nominally complete.
            let mut state =
                TaskState::new(project_id.clone(), project_root.clone(), "do thing".into());
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            // Lock held by this task so we can observe it stays held on failure.
            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None)
                .expect("acquire merge lock");

            // PR exists in Approved state.
            let pr_record = pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Approved;
            })
            .expect("approve pr");
            assert_eq!(pr_record.review_state, pr::ReviewState::Open);

            // Build script that fails with a known stderr signature.
            let build_script = project_root.join("build.sh");
            std::fs::write(
                &build_script,
                "#!/bin/sh\necho 'build broke!' 1>&2\nexit 7\n",
            )
            .expect("write build script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&build_script).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&build_script, perms).unwrap();
            }

            let result = pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--build-cmd".into(),
                build_script.to_string_lossy().into_owned(),
            ]);
            match result {
                Err(CliError::Reported(msg)) => assert!(
                    msg.contains("build gate failed"),
                    "unexpected error: {}",
                    msg
                ),
                other => panic!("expected build gate failure, got {:?}", other),
            }

            // Task stayed in Merging.
            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Merging);

            // PR did NOT flip to Merged; it stays Approved with a new comment.
            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Approved);
            assert!(
                after_pr
                    .comments
                    .iter()
                    .any(|c| c.body.contains("build broke!")),
                "expected PR comment containing build stderr; got {:?}",
                after_pr.comments,
            );

            // Merge lock still held.
            let holder = cc_hub_lib::merge_lock::current_holder(&project_id)
                .expect("read lock")
                .expect("lock still held");
            assert_eq!(holder.task_id, task_id);
        });
    }

    #[test]
    fn pr_finalize_skip_build_releases_lock_even_with_missing_command() {
        with_tempdir_home(|| {
            let project_id = "p-skip".to_string();
            let task_id = "t-skip".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire");
            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
                "--build-cmd".into(),
                "/nonexistent/build-script-does-not-exist".into(),
            ])
            .expect("finalize ok with --skip-build");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Merged);

            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read lock")
                    .is_none(),
                "lock should be released after successful finalize"
            );
        });
    }

    #[test]
    fn pr_finalize_keep_tmux_still_marks_task_done() {
        with_tempdir_home(|| {
            let project_id = "p-keep".to_string();
            let task_id = "t-keep".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            state.orchestrator_tmux = Some("cc-hub-test-orch".into());
            orchestrator::write_task_state(&state).expect("write state");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire");
            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
                "--keep-tmux".into(),
            ])
            .expect("finalize ok with --keep-tmux");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);
            // We don't observe tmux state in unit tests (cleanup happens via
            // detached `sh -c sleep …; tmux …`). Asserting Done + Merged is
            // the user-visible signal; whether cleanup_task_sessions ran is
            // an integration concern.
        });
    }

    #[test]
    fn pr_finalize_refuses_when_task_not_merging() {
        with_tempdir_home(|| {
            let project_id = "p-guard-review".to_string();
            let task_id = "t-guard-review".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Review;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Approved;
            })
            .expect("approve pr");

            let result = pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
            ]);
            match result {
                Err(CliError::Usage(msg)) => {
                    assert!(msg.contains("Merging"), "expected Merging in msg: {}", msg);
                    assert!(
                        msg.contains("cc-hub pr merge"),
                        "expected `cc-hub pr merge` in msg: {}",
                        msg
                    );
                }
                other => panic!("expected Usage error, got {:?}", other),
            }

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Review);

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Approved);
        });
    }

    #[test]
    fn pr_finalize_refuses_when_no_pr_exists() {
        with_tempdir_home(|| {
            let project_id = "p-guard-nopr".to_string();
            let task_id = "t-guard-nopr".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire lock");

            let result = pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
            ]);
            match result {
                Err(CliError::Usage(msg)) => {
                    assert!(msg.contains("no PR"), "expected `no PR` in msg: {}", msg);
                }
                other => panic!("expected Usage error, got {:?}", other),
            }

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Merging);

            let holder = cc_hub_lib::merge_lock::current_holder(&project_id)
                .expect("read lock")
                .expect("lock still held");
            assert_eq!(holder.task_id, task_id);
        });
    }

    #[test]
    fn pr_finalize_is_idempotent_when_pr_already_merged() {
        with_tempdir_home(|| {
            let project_id = "p-guard-merged".to_string();
            let task_id = "t-guard-merged".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Done;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Merged;
            })
            .expect("merge pr");

            pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
            ])
            .expect("idempotent finalize returns Ok");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Merged);

            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read lock")
                    .is_none(),
                "lock should remain unheld on idempotent retry"
            );
        });
    }

    #[test]
    fn pr_finalize_refuses_when_task_backlog() {
        with_tempdir_home(|| {
            let project_id = "p-guard-backlog".to_string();
            let task_id = "t-guard-backlog".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new_backlog(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            assert_eq!(state.status, TaskStatus::Backlog);
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            let result = pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
            ]);
            match result {
                Err(CliError::Usage(msg)) => {
                    assert!(msg.contains("Merging"), "expected Merging in msg: {}", msg);
                    assert!(
                        msg.contains("cc-hub pr merge"),
                        "expected `cc-hub pr merge` in msg: {}",
                        msg
                    );
                }
                other => panic!("expected Usage error, got {:?}", other),
            }

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Backlog);

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Open);

            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read lock")
                    .is_none(),
                "lock should remain unheld when finalize is refused"
            );
        });
    }

    #[test]
    fn pr_close_from_open_marks_task_done() {
        with_tempdir_home(|| {
            let project_id = "p-close-open".to_string();
            let task_id = "t-close-open".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Review;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            // Hold the merge lock so we can assert release on close.
            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire");

            pr_close(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--comment".into(),
                "abandoning this work".into(),
            ])
            .expect("pr close ok");

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Closed);
            let last = after_pr.comments.last().expect("comment appended");
            assert_eq!(last.author, "user");
            assert_eq!(last.body, "abandoning this work");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);
            assert!(
                after_state.note.as_deref().unwrap_or("").contains("closed"),
                "note should mention closed, got {:?}",
                after_state.note
            );

            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read lock")
                    .is_none(),
                "lock should be released after close"
            );
        });
    }

    #[test]
    fn pr_close_from_changes_requested_works() {
        with_tempdir_home(|| {
            let project_id = "p-close-cr".to_string();
            let task_id = "t-close-cr".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Running;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::ChangesRequested;
            })
            .expect("set changes_requested");

            pr_close(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ])
            .expect("pr close ok");

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Closed);

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);
        });
    }

    #[test]
    fn pr_close_refuses_merged_and_closed() {
        with_tempdir_home(|| {
            let project_id = "p-close-refuse".to_string();
            let task_id = "t-close-refuse".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Done;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            // Already-merged PR: refuse.
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Merged;
            })
            .expect("set merged");
            let err = pr_close(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ])
            .expect_err("close on merged should fail");
            match err {
                CliError::Usage(msg) => assert!(
                    msg.contains("merged"),
                    "expected message mentioning merged, got: {msg}"
                ),
                other => panic!("expected CliError::Usage, got {other:?}"),
            }

            // Already-closed PR: refuse.
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Closed;
            })
            .expect("set closed");
            let err = pr_close(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ])
            .expect_err("close on closed should fail");
            match err {
                CliError::Usage(msg) => assert!(
                    msg.contains("closed"),
                    "expected message mentioning closed, got: {msg}"
                ),
                other => panic!("expected CliError::Usage, got {other:?}"),
            }
        });
    }

    #[test]
    fn pr_show_comments_since_filters_old_comments() {
        let pr = pr::PullRequest {
            id: 1,
            task_id: "t-x".into(),
            project_id: "p1".into(),
            branch: "feature".into(),
            base: "main".into(),
            title: "title".into(),
            description: "desc".into(),
            review_state: pr::ReviewState::Open,
            comments: vec![
                pr::Comment {
                    author: "user".into(),
                    at: 100,
                    body: "first".into(),
                },
                pr::Comment {
                    author: "user".into(),
                    at: 200,
                    body: "second".into(),
                },
                pr::Comment {
                    author: "user".into(),
                    at: 300,
                    body: "third".into(),
                },
            ],
            approved_at_branch_sha: None,
            approved_at_base_sha: None,
            created_at: 0,
            updated_at: 300,
        };

        let none = pr_to_json_filtered(&pr, None);
        assert_eq!(none["comments"].as_array().expect("array").len(), 3);
        assert_eq!(none["comments_total"], 3);
        assert_eq!(none["comments_returned"], 3);

        let since_200 = pr_to_json_filtered(&pr, Some(200));
        let kept = since_200["comments"].as_array().expect("array");
        assert_eq!(kept.len(), 2);
        assert_eq!(since_200["comments_total"], 3);
        assert_eq!(since_200["comments_returned"], 2);
        let stamps: Vec<i64> = kept.iter().map(|c| c["at"].as_i64().unwrap()).collect();
        assert_eq!(stamps, vec![200, 300]);

        let since_999 = pr_to_json_filtered(&pr, Some(999));
        assert_eq!(since_999["comments"].as_array().expect("array").len(), 0);
        assert_eq!(since_999["comments_total"], 3);
        assert_eq!(since_999["comments_returned"], 0);
    }

    #[test]
    fn pr_reopen_flips_changes_requested_to_open() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-reopen".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Review;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::ChangesRequested;
            })
            .expect("set changes_requested");
            orchestrator::update_task_state(&project_id, &task_id, |s| {
                s.status = TaskStatus::Running;
            })
            .expect("set running");

            let code = dispatch(&[
                "pr".into(),
                "reopen".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--comment".into(),
                "fixed it".into(),
            ]);
            assert_eq!(code, Some(0), "dispatch should succeed");

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Open);
            let last = after_pr.comments.last().expect("comment appended");
            assert_eq!(last.author, "orchestrator");
            assert_eq!(last.body, "fixed it");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Review);
            assert!(
                after_state
                    .note
                    .as_deref()
                    .unwrap_or("")
                    .contains("reopened for re-review"),
                "note should mention reopen, got {:?}",
                after_state.note
            );
        });
    }

    /// Regression: if `merge_lock::release` fails, `pr_finalize` must bail
    /// without flipping the task to `Done`. Otherwise a transient FS error
    /// strands a terminal-Done task holding the merge lock and blocks the
    /// project's merge queue until `STALE_TTL_SECS`.
    // Unix-only: the failure is simulated by chmod 0o555 to force EACCES on
    // lock removal — Windows has no equivalent permission semantics, and the
    // unix-only APIs below would break `cargo check --all-targets` there.
    #[cfg(unix)]
    #[test]
    fn pr_finalize_keeps_task_merging_when_release_fails() {
        use std::os::unix::fs::PermissionsExt;

        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-finalize-fail".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire lock");

            // Lock unlink requires write+exec on the parent dir; chmod 0o555
            // forces fs::remove_file to fail with EACCES, simulating the
            // transient FS error the bug fix guards against.
            let lock_path =
                cc_hub_lib::merge_lock::merge_lock_path(&project_id).expect("lock path");
            let parent = lock_path.parent().expect("parent dir").to_path_buf();
            let original_perms = std::fs::metadata(&parent).expect("metadata").permissions();
            let mut ro = original_perms.clone();
            ro.set_mode(0o555);
            std::fs::set_permissions(&parent, ro).expect("chmod ro");

            let result = pr_finalize(&[
                "--task".to_string(),
                task_id.clone(),
                "--project-id".to_string(),
                project_id.clone(),
                "--skip-build".to_string(),
            ]);

            // Restore perms before assertions so tempdir teardown works.
            std::fs::set_permissions(&parent, original_perms.clone()).expect("restore perms");

            assert!(
                result.is_err(),
                "pr_finalize should return Err when release fails, got {:?}",
                result
            );

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(
                after_state.status,
                TaskStatus::Merging,
                "task must remain Merging when release fails"
            );

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(
                after_pr.review_state,
                pr::ReviewState::Open,
                "pr must remain Open when release fails (release runs before pr update)"
            );

            let holder = cc_hub_lib::merge_lock::current_holder(&project_id)
                .expect("read holder")
                .expect("lock present");
            assert_eq!(
                holder.task_id, task_id,
                "lock should still name this task — release never completed"
            );

            // Retry now that perms are restored: the second pr_finalize must
            // succeed and drive the task to Done.
            pr_finalize(&[
                "--task".to_string(),
                task_id.clone(),
                "--project-id".to_string(),
                project_id.clone(),
                "--skip-build".to_string(),
            ])
            .expect("retry pr_finalize after restore");

            let final_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(final_state.status, TaskStatus::Done);
            let final_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(final_pr.review_state, pr::ReviewState::Merged);
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read holder")
                    .is_none(),
                "lock must be released after successful retry"
            );
        });
    }

    #[test]
    fn pr_merge_refuses_dirty_worktree_without_demoting() {
        // A dirty feature-branch worktree must trip the step-1 preflight and
        // return a distinct `dirty_worktree` outcome — NOT proceed into the
        // merge (which would misclassify as a phantom conflict and demote the
        // approved PR). The PR stays Approved and the merge lock is released.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        with_tempdir_home(|| {
            let repo = init_repo();
            let (project_id, task_id, wt_path) =
                seed_task_with_worktree(repo.path(), TaskStatus::Review);

            // Approved PR on the worktree's branch.
            let state = orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            let branch = orchestrator::worktree_branch(&task_id, "wt");
            pr::create_pr(&state, branch, "main".into(), "title".into(), "desc".into())
                .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Approved;
            })
            .expect("approve pr");

            // Dirty the worktree: an uncommitted edit to a tracked file.
            std::fs::write(wt_path.join("seed.txt"), "dirtied in worktree\n")
                .expect("dirty worktree");

            let result = pr_merge(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            match result {
                Err(CliError::Reported(msg)) => assert!(
                    msg.contains("worktree") && msg.contains("uncommitted"),
                    "expected dirty-worktree refusal, got: {}",
                    msg
                ),
                other => panic!("expected dirty-worktree refusal, got {:?}", other),
            }

            // PR must remain Approved (not demoted to Open).
            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(
                after_pr.review_state,
                pr::ReviewState::Approved,
                "dirty-worktree refusal must not demote the PR"
            );

            // Merge lock released so the project isn't blocked.
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_none(),
                "merge lock must be released after a dirty-worktree refusal"
            );

            // The dirty edit is untouched — preflight must not mutate the tree.
            let body = std::fs::read_to_string(wt_path.join("seed.txt")).unwrap();
            assert_eq!(body, "dirtied in worktree\n");
        });
    }

    /// Helper: seed a Done task with a Merged PR, returning (project_id, task_id).
    fn seed_merged_pr(project_id: &str, task_id: &str) {
        let mut state = TaskState::new(
            project_id.to_string(),
            PathBuf::from("/tmp/proj"),
            "do thing".into(),
        );
        state.task_id = task_id.to_string();
        state.status = TaskStatus::Done;
        orchestrator::write_task_state(&state).expect("write state");

        pr::create_pr(
            &state,
            "feature".into(),
            "main".into(),
            "title".into(),
            "desc".into(),
        )
        .expect("create pr");
        pr::update_pr(project_id, task_id, |p| {
            p.review_state = pr::ReviewState::Merged;
        })
        .expect("set merged");
    }

    #[test]
    fn pr_approve_refuses_merged_pr_and_leaves_task_unchanged() {
        with_tempdir_home(|| {
            let project_id = "p-approve-merged";
            let task_id = "t-approve-merged";
            seed_merged_pr(project_id, task_id);

            let argv = vec![
                "pr".to_string(),
                "approve".to_string(),
                "--task".to_string(),
                task_id.to_string(),
                "--project-id".to_string(),
                project_id.to_string(),
            ];
            // Nonzero exit via dispatch (exercises the JSON-error handler too).
            let code = dispatch(&argv);
            assert_eq!(code, Some(1), "approve on merged PR must exit nonzero");

            // PR stays Merged, task stays Done.
            let after_pr = pr::read_pr(project_id, task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Merged);
            let after_state =
                orchestrator::read_task_state(project_id, task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);

            // Direct call surfaces a Conflict error.
            let err = pr_approve(&argv[2..]).expect_err("must error");
            assert_eq!(err.kind(), "conflict");
        });
    }

    #[test]
    fn pr_request_changes_refuses_merged_pr_and_leaves_task_unchanged() {
        with_tempdir_home(|| {
            let project_id = "p-rc-merged";
            let task_id = "t-rc-merged";
            seed_merged_pr(project_id, task_id);

            let argv = vec![
                "pr".to_string(),
                "request-changes".to_string(),
                "--task".to_string(),
                task_id.to_string(),
                "--project-id".to_string(),
                project_id.to_string(),
                "--comment".to_string(),
                "please change".to_string(),
            ];
            let code = dispatch(&argv);
            assert_eq!(
                code,
                Some(1),
                "request-changes on merged PR must exit nonzero"
            );

            // Critically: the merged PR must NOT be resurrected, and the Done
            // task must NOT be forced back to Running.
            let after_pr = pr::read_pr(project_id, task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Merged);
            assert!(
                after_pr.comments.is_empty(),
                "no comment should be appended on a refused request-changes"
            );
            let after_state =
                orchestrator::read_task_state(project_id, task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);

            let err = pr_request_changes(&argv[2..]).expect_err("must error");
            assert_eq!(err.kind(), "conflict");
        });
    }
}
