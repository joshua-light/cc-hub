//! PR-flow ops.
//!
//! Bodies of `pr create`, `pr approve`, `pr request-changes`, `pr reopen`,
//! `pr comment`, `pr close`, `pr merge`, `pr continue`, `pr lock-phase`, and
//! `pr finalize`. The CLI keeps flag parsing and JSON rendering; the
//! side-effect ORDER (update_pr / update_task_state / merge-lock release
//! points) is preserved exactly as it was in the CLI layer.
//!
//! The two big verbs — `pr_merge` (merge-lock acquire/wait, merge, conflict
//! demotion, HEAD restore) and `pr_finalize` (build gate, lock release,
//! terminal flips) — return typed outcome enums; the CLI reconstructs its
//! existing JSON output byte-identically from the returned data.

use crate::ops::task::resolve_worktree_path;
use crate::ops::OpError;
use crate::orchestrator::{self, MergeRecord, TaskStatus};
use crate::pr::{Comment, PullRequest, ReviewState};
use crate::{merge_lock, pr, send};

/// `cc-hub pr create` body: open a PR and transition the task Running → Review.
pub fn pr_create(
    project_id: &str,
    task_id: &str,
    worktree_name: &str,
    title: String,
    description: String,
) -> Result<PullRequest, OpError> {
    let state = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;

    // Validate the Running → Review transition BEFORE allocating a PR id or
    // writing pr.json. A `pr create` against a Backlog/Done task otherwise
    // burns a PR id and strands an orphan pr.json that permanently blocks every
    // future `pr create` for the task ("a PR already exists").
    orchestrator::validate_status_transition(&state.status, &TaskStatus::Review, state.kind())
        .map_err(|e| OpError::Usage(format!("cannot open PR: {}", e)))?;

    let branch = orchestrator::worktree_branch(task_id, worktree_name);
    let (_, create_root) = state
        .require_project()
        .map_err(|e| OpError::Usage(e.to_string()))?;
    let base = orchestrator::detect_main_branch(create_root);

    // Serialize the whole exists-check → create → transition under the per-task
    // advisory lock (the same lock `update_task_state` / `update_pr` take) so
    // two overlapping `pr create`s can't both pass the exists-check and both
    // write pr.json. We write state.json by hand rather than via
    // `update_task_state`: that helper re-takes this same flock, which would
    // deadlock while we hold it.
    let _lock = orchestrator::lock_task_state(Some(project_id), task_id)
        .map_err(|e| OpError::Other(format!("lock task: {}", e)))?;

    if pr::read_pr(project_id, task_id)
        .map_err(|e| OpError::Other(format!("read pr: {}", e)))?
        .is_some()
    {
        return Err(OpError::Other(
            "a PR already exists for this task; use `pr show` to inspect".into(),
        ));
    }

    // Re-read + re-validate under the lock so a status change between the
    // up-front read and here is still caught before we create the PR.
    let mut fresh = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    orchestrator::validate_status_transition(&fresh.status, &TaskStatus::Review, fresh.kind())
        .map_err(|e| OpError::Usage(format!("cannot open PR: {}", e)))?;

    let pr = pr::create_pr(&fresh, branch, base, title, description)
        .map_err(|e| OpError::Other(format!("create pr: {}", e)))?;

    // PR open → task transitions Running → Review. The orchestrator's
    // tmux stays alive so it can iterate when the user requests changes.
    fresh.status = TaskStatus::Review;
    fresh.note = Some(format!("PR #{}: {}", pr.id, pr.title));
    fresh.last_auto_reviewed_at = None;
    fresh.touch();
    orchestrator::write_task_state(&fresh)
        .map_err(|e| OpError::Other(format!("update state: {}", e)))?;

    Ok(pr)
}

/// Guard a review-state mutation against terminal PRs. `pr approve` and
/// `pr request-changes` must refuse a Merged/Closed PR — like their siblings
/// `pr reopen`/`pr merge`/`pr close` — instead of silently resurrecting a
/// finished task (forcing Done → Running and re-opening the merged PR).
fn guard_pr_mutable(verb: &str, state: ReviewState) -> Result<(), OpError> {
    match state {
        ReviewState::Merged => Err(OpError::conflict_with_recipe(
            format!("cannot {} a merged PR", verb),
            "The PR is already merged; open a follow-up task instead of reopening finished work.",
        )),
        ReviewState::Closed => Err(OpError::conflict_with_recipe(
            format!("cannot {} a closed PR", verb),
            "The PR is closed; reopen it via the TUI before changing its review state.",
        )),
        _ => Ok(()),
    }
}

/// `cc-hub pr approve` body: flip the PR to Approved and snapshot the branch /
/// base SHAs at approval time (used by `pr merge`'s auto-approve heuristic).
pub fn pr_approve(project_id: &str, task_id: &str) -> Result<PullRequest, OpError> {
    let state = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    let project_root = state
        .require_project()
        .map_err(|e| OpError::Usage(e.to_string()))?
        .1
        .to_path_buf();

    let pr = pr::read_pr(project_id, task_id)
        .map_err(|e| OpError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| OpError::NotFound("no PR for this task".into()))?;
    guard_pr_mutable("approve", pr.review_state)?;

    // Approval is only meaningful for a task in Review: it's the reviewer
    // signing off on a proposed diff. Approving a Running task (mid-iteration)
    // or a Merging one (already past sign-off) is a mistake — reject it rather
    // than silently stamping the PR. Placed after `guard_pr_mutable` so a
    // Merged/Closed PR still surfaces the conflict error, not this one.
    if state.status != TaskStatus::Review {
        return Err(OpError::Usage(format!(
            "cannot approve: task {} is {} (approval only applies to a task in Review)",
            task_id,
            state.status.as_str()
        )));
    }

    let pr_branch = pr.branch.clone();
    let base_branch = orchestrator::detect_main_branch(&project_root);

    // Snapshot SHAs at approval — used by `pr merge` to detect whether
    // main moved before the merge fired (auto-approve heuristic).
    let branch_sha = git_rev_parse(&project_root, &pr_branch).ok();
    let base_sha = git_rev_parse(&project_root, &base_branch).ok();

    let pr = pr::update_pr(project_id, task_id, |p| {
        p.review_state = ReviewState::Approved;
        p.approved_at_branch_sha = branch_sha;
        p.approved_at_base_sha = base_sha;
    })
    .map_err(|e| OpError::Other(format!("update pr: {}", e)))?;

    Ok(pr)
}

/// `cc-hub pr request-changes` body: append the comment, flip the PR to
/// ChangesRequested, and return the task to Running so the orchestrator can
/// iterate.
pub fn pr_request_changes(
    project_id: &str,
    task_id: &str,
    comment: String,
    author: String,
) -> Result<PullRequest, OpError> {
    let existing = pr::read_pr(project_id, task_id)
        .map_err(|e| OpError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| OpError::NotFound("no PR for this task".into()))?;
    guard_pr_mutable("request changes on", existing.review_state)?;

    let now = orchestrator::now_unix_secs();
    let pr = pr::update_pr(project_id, task_id, |p| {
        p.review_state = ReviewState::ChangesRequested;
        p.comments.push(Comment {
            author: author.clone(),
            at: now,
            body: comment.clone(),
        });
    })
    .map_err(|e| OpError::Other(format!("update pr: {}", e)))?;

    // Changes requested → task goes back to Running so the orchestrator
    // can iterate. Its tmux is still alive (Review keeps it alive).
    orchestrator::update_task_state(project_id, task_id, |s| {
        s.status = TaskStatus::Running;
        s.note = Some(format!("PR #{}: changes requested", pr.id));
    })
    .map_err(|e| OpError::Other(format!("update state: {}", e)))?;

    Ok(pr)
}

/// `cc-hub pr reopen` body: flip a ChangesRequested PR back to Open and the
/// task Running → Review, re-arming auto-review.
pub fn pr_reopen(
    project_id: &str,
    task_id: &str,
    comment: Option<String>,
    author: String,
) -> Result<PullRequest, OpError> {
    let pr = pr::read_pr(project_id, task_id)
        .map_err(|e| OpError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| OpError::NotFound("no PR for this task".into()))?;

    if pr.review_state != ReviewState::ChangesRequested {
        return Err(OpError::Other(format!(
            "PR is not in changes_requested (state: {}); reopen only applies after request-changes",
            pr.review_state.as_str()
        )));
    }

    let now = orchestrator::now_unix_secs();
    let pr = pr::update_pr(project_id, task_id, |p| {
        p.review_state = ReviewState::Open;
        if let Some(body) = comment.clone() {
            p.comments.push(Comment {
                author: author.clone(),
                at: now,
                body,
            });
        }
    })
    .map_err(|e| OpError::Other(format!("update pr: {}", e)))?;

    // Re-opened PR → task transitions Running → Review and auto-review
    // should re-fire on the new commits (mirror pr_create precedent).
    orchestrator::update_task_state(project_id, task_id, |s| {
        s.status = TaskStatus::Review;
        s.note = Some(format!("PR #{}: reopened for re-review", pr.id));
        s.last_auto_reviewed_at = None;
    })
    .map_err(|e| OpError::Other(format!("update state: {}", e)))?;

    Ok(pr)
}

/// `cc-hub pr comment` body: append a comment without changing review state.
pub fn pr_comment(
    project_id: &str,
    task_id: &str,
    body: String,
    author: String,
) -> Result<PullRequest, OpError> {
    let now = orchestrator::now_unix_secs();
    pr::update_pr(project_id, task_id, |p| {
        p.comments.push(Comment {
            author: author.clone(),
            at: now,
            body: body.clone(),
        });
    })
    .map_err(|e| OpError::Other(format!("update pr: {}", e)))
}

/// `cc-hub pr close` body: non-destructive abandon — flip the PR to Closed,
/// mark the task Done, drop the merge lock if held, and tear down sessions.
/// Returns the updated PR (the caller reports `status: "done"`).
pub fn pr_close(
    project_id: &str,
    task_id: &str,
    comment: Option<String>,
    author: String,
) -> Result<PullRequest, OpError> {
    let pr = pr::read_pr(project_id, task_id)
        .map_err(|e| OpError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| OpError::NotFound("no PR for this task".into()))?;

    match pr.review_state {
        ReviewState::Merged => {
            return Err(OpError::Usage(
                "PR is already merged; closing a merged PR is not meaningful — consider opening a follow-up task instead".into(),
            ));
        }
        ReviewState::Closed => {
            return Err(OpError::Usage(
                "PR is already closed; reopen it via the TUI before closing again".into(),
            ));
        }
        _ => {}
    }

    let now = orchestrator::now_unix_secs();
    let pr = pr::update_pr(project_id, task_id, |p| {
        p.review_state = ReviewState::Closed;
        if let Some(body) = comment.clone() {
            p.comments.push(Comment {
                author: author.clone(),
                at: now,
                body,
            });
        }
    })
    .map_err(|e| OpError::Other(format!("update pr: {}", e)))?;

    let state = orchestrator::update_task_state(project_id, task_id, |s| {
        s.status = TaskStatus::Done;
        s.note = Some(format!("PR #{}: closed", pr.id));
    })
    .map_err(|e| OpError::Other(format!("update state: {}", e)))?;

    // No-op if this task isn't the holder.
    let _ = merge_lock::release(project_id, task_id);

    orchestrator::cleanup_task_sessions(&state);

    Ok(pr)
}

/// The lock holder a `pr merge` reported as blocking. Mirrors the JSON fields.
pub struct MergeLockHolder {
    pub holder_task: String,
    pub since: i64,
    pub phase: &'static str,
    pub age_seconds: i64,
    /// True when `--wait` was passed and the wait timed out — the CLI swaps
    /// in the timed-out recipe and emits `timed_out: true`.
    pub timed_out: bool,
}

/// Outcome of [`pr_merge`]. Each variant carries exactly the data the CLI
/// prints today; the CLI reconstructs the byte-identical JSON line and exit
/// behavior from it.
pub enum MergeOutcomeOp {
    /// Merge landed; task is now Merging awaiting /simplify + /bump + finalize.
    Merged {
        branch: String,
        base: String,
        stdout: String,
        /// Always `None`: `pr merge` deliberately leaves HEAD on `base` so the
        /// on-main Merging phase (/simplify, /bump, finalize build gate) runs
        /// on main. The pre-merge ref is stashed in the merge lock and restored
        /// by `pr finalize`. Retained only for the CLI's JSON shape.
        restored_ref: Option<String>,
    },
    /// Another task holds the merge lock.
    Locked(MergeLockHolder),
    /// Preflight: the feature-branch worktree directory no longer exists.
    MissingWorktree { worktree: String, branch: String },
    /// Preflight: the feature-branch worktree has uncommitted changes.
    DirtyWorktree {
        worktree: String,
        dirty: Vec<String>,
    },
    /// Step 1 produced conflicts merging base into the feature branch; the PR
    /// was demoted to Open.
    DemotedConflict {
        conflicting_paths: Vec<String>,
        stdout: String,
        stderr: String,
    },
    /// Preflight: the target-branch working tree has overlapping uncommitted
    /// edits.
    BlockedByDirtyTree { overlap: Vec<String> },
    /// Step 3 produced conflicts merging the feature branch into main.
    ConflictIntoMain {
        conflicting_paths: Vec<String>,
        stdout: String,
        stderr: String,
    },
}

/// `cc-hub pr merge` body: acquire the project merge lock, merge main into the
/// feature branch then the feature branch into main, and transition the task
/// to Merging. Returns a [`MergeOutcomeOp`] describing the result; the lock is
/// held on `Merged` (released later by `pr finalize`) and released on every
/// non-`Merged` outcome. Internal fallible steps release the lock and bubble
/// up as `OpError::Other`.
pub fn pr_merge(
    project_id: &str,
    task_id: &str,
    wait: bool,
    timeout_secs: Option<u64>,
) -> Result<MergeOutcomeOp, OpError> {
    let state = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    let project_root = state
        .require_project()
        .map_err(|e| OpError::Usage(e.to_string()))?
        .1
        .to_path_buf();
    let pr = pr::read_pr(project_id, task_id)
        .map_err(|e| OpError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| OpError::NotFound("no PR for this task".into()))?;

    if pr.review_state != ReviewState::Approved {
        return Err(OpError::Other(format!(
            "PR is not approved (state: {}); approve it first via the TUI or `cc-hub pr approve`",
            pr.review_state.as_str()
        )));
    }

    // Guard the task-status transition up front, before touching the merge lock
    // or git. `pr_merge` only flips the task to Merging at the very end — after
    // the merge has already landed on main. Validating here, through the same
    // `validate_status_transition` the write path enforces, refuses a task from
    // which `→ Merging` is illegal (e.g. one bounced back to Running by
    // `pr request-changes`). Without this the merge lands, then the final
    // transition fails, stranding main mutated with no MergeRecord and a wedged
    // task that retries re-merge on every run.
    orchestrator::validate_status_transition(&state.status, &TaskStatus::Merging, state.kind())
        .map_err(|e| {
            OpError::Usage(format!(
                "cannot merge: {} — merge only applies to an approved task in Review",
                e
            ))
        })?;

    // Stale-detection liveness proxy: only hand the lock the orchestrator's
    // tmux if that session is actually alive right now. A human running
    // `cc-hub pr merge` from a plain shell after the orchestrator died would
    // otherwise record a DEAD tmux, making the lock look stale at age 0 so any
    // concurrent `acquire` steals it mid-merge. Passing None falls staleness
    // back to the TTL. (See merge_lock::is_stale.)
    let live_tmux = state
        .orchestrator_tmux
        .as_deref()
        .filter(|t| send::tmux_session_exists(t));

    // Acquire the project-wide merge lock. Held across the entire merging
    // phase — released by `pr finalize` after /simplify and /bump.
    let acquire = if wait {
        let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(1800));
        merge_lock::acquire_blocking(
            project_id,
            task_id,
            live_tmux,
            timeout,
            std::time::Duration::from_millis(500),
        )
        .map_err(|e| OpError::Other(format!("acquire merge lock: {}", e)))?
    } else {
        merge_lock::acquire(project_id, task_id, live_tmux)
            .map_err(|e| OpError::Other(format!("acquire merge lock: {}", e)))?
    };
    if let merge_lock::AcquireOutcome::Held(holder) = acquire {
        let age_seconds = orchestrator::now_unix_secs().saturating_sub(holder.acquired_at);
        return Ok(MergeOutcomeOp::Locked(MergeLockHolder {
            holder_task: holder.task_id,
            since: holder.acquired_at,
            phase: holder.phase.as_str(),
            age_seconds,
            timed_out: wait,
        }));
    }

    // From here on the merge lock is HELD (released by `pr finalize`, or by the
    // explicit demote/preflight paths below). Any fallible step that bubbles its
    // error up via `?` must release the lock first — otherwise a mid-merge
    // failure (corrupt index, permission error, git exec failure) strands the
    // lock on an exited process and wedges every subsequent `pr merge` for the
    // project. Funnel those errors through `unlock`. (Release is idempotent, so
    // paths that already released explicitly are unaffected.)
    let unlock = |msg: String| -> OpError {
        let _ = merge_lock::release(project_id, task_id);
        OpError::Other(msg)
    };

    // Re-validate now that the lock is held: with `--wait` the pre-lock guards
    // may be up to 30 minutes stale, and a concurrent `pr request-changes` /
    // `pr close` in that window would otherwise reproduce the merge-then-wedge
    // failure the early guard exists to prevent (merge lands on main, final
    // transition fails). Re-reading shrinks the exposure to the moments between
    // this check and the merge itself.
    let state = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| unlock(format!("reload state under lock: {}", e)))?;
    let pr = match pr::read_pr(project_id, task_id) {
        Ok(Some(p)) => p,
        Ok(None) => {
            let _ = merge_lock::release(project_id, task_id);
            return Err(OpError::NotFound(
                "PR disappeared while waiting for the merge lock".into(),
            ));
        }
        Err(e) => return Err(unlock(format!("re-read pr under lock: {}", e))),
    };
    if pr.review_state != ReviewState::Approved {
        let _ = merge_lock::release(project_id, task_id);
        return Err(OpError::Other(format!(
            "PR is no longer approved (state: {}); it changed while waiting for the merge lock",
            pr.review_state.as_str()
        )));
    }
    if let Err(e) =
        orchestrator::validate_status_transition(&state.status, &TaskStatus::Merging, state.kind())
    {
        let _ = merge_lock::release(project_id, task_id);
        return Err(OpError::Usage(format!(
            "cannot merge: {} — the task status changed while waiting for the merge lock",
            e
        )));
    }

    // Step 1: bring main into the feature branch so the conflict
    // resolution happens on the feature branch (where the worker can
    // re-resolve cleanly), not on main itself.
    let worktree_path = match resolve_worktree_path(&state, &pr.branch) {
        Some(p) => p,
        None => {
            let _ = merge_lock::release(project_id, task_id);
            return Err(OpError::Other(format!(
                "could not resolve worktree path for branch {} \
                 (no Worker record matches; was the worktree removed?)",
                pr.branch
            )));
        }
    };

    // The worktree dir must still exist before we operate on it: a gc / manual
    // cleanup may have removed it while the task sat in Review. Treat a missing
    // worktree as a distinct outcome rather than letting `git -C <gone>` fail
    // with an opaque error.
    if !worktree_path.exists() {
        let _ = merge_lock::release(project_id, task_id);
        return Ok(MergeOutcomeOp::MissingWorktree {
            worktree: worktree_path.to_string_lossy().into_owned(),
            branch: pr.branch.clone(),
        });
    }

    // Clean-tree preflight on the worktree itself. Step 1 merges base INTO the
    // feature branch inside this worktree; if it's dirty, git either aborts
    // ("would be overwritten") or auto-stashes and produces conflict markers on
    // pop — which downstream misclassifies as a content conflict and wrongly
    // demotes the approved PR with a misleading "0 files" message. Refuse up
    // front with a distinct outcome so the orchestrator gets an actionable
    // recipe instead of a phantom conflict.
    let worktree_dirty = orchestrator::dirty_paths(&worktree_path)
        .map_err(|e| unlock(format!("git status (worktree): {}", e)))?;
    if !worktree_dirty.is_empty() {
        let _ = merge_lock::release(project_id, task_id);
        return Ok(MergeOutcomeOp::DirtyWorktree {
            worktree: worktree_path.to_string_lossy().into_owned(),
            dirty: worktree_dirty,
        });
    }

    let merge_into_feature = orchestrator::run_git(
        &worktree_path,
        &[
            "merge",
            "--no-ff",
            "-m",
            &format!("cc-hub: merge {} into {}", pr.base, pr.branch),
            &pr.base,
        ],
    )
    .map_err(|e| unlock(format!("git merge {} into branch: {}", pr.base, e)))?;

    if !merge_into_feature.status_ok {
        // Conflicts merging main into the feature branch. By the
        // PR-flow design's auto-approve rule (only *clean* resolutions
        // skip re-review), conflicts demote the PR back to Open: the
        // user must re-approve once the orchestrator commits the
        // resolution, since the diff they previously approved no
        // longer matches what would land. The merge lock is released
        // so other tasks can proceed in the meantime.
        let conflicting = git_conflicting_paths(&worktree_path).unwrap_or_default();

        // Abort the in-progress merge so the worktree returns to a
        // clean state — the orchestrator will re-merge main once the
        // user re-approves. We don't surface abort failures: if abort
        // itself fails, the orchestrator can recover manually.
        let _ = orchestrator::run_git(&worktree_path, &["merge", "--abort"]);

        let comment_body = format!(
            "Auto-demoted to Open: merging `{}` into the feature branch produced conflicts in {} \
             file(s) ({}). cc-hub's auto-approve rule only accepts clean resolutions; resolve in \
             the worktree, push the resolution commit, then ask the reviewer to re-approve.",
            pr.base,
            conflicting.len(),
            conflicting.join(", "),
        );
        let now = orchestrator::now_unix_secs();
        let _ = pr::update_pr(project_id, task_id, |p| {
            p.review_state = ReviewState::Open;
            p.approved_at_branch_sha = None;
            p.approved_at_base_sha = None;
            p.comments.push(Comment {
                author: "cc-hub".into(),
                at: now,
                body: comment_body.clone(),
            });
        });
        let _ = orchestrator::update_task_state(project_id, task_id, |s| {
            s.status = TaskStatus::Review;
            s.note = Some(format!(
                "PR #{}: conflicts during merge — re-review required",
                pr.id
            ));
            s.last_auto_reviewed_at = None;
        });
        let _ = merge_lock::release(project_id, task_id);

        return Ok(MergeOutcomeOp::DemotedConflict {
            conflicting_paths: conflicting,
            stdout: merge_into_feature.stdout,
            stderr: merge_into_feature.stderr,
        });
    }

    // Step 2: dirty-tree preflight on main. Distinct from cross-task
    // conflicts (which the merge lock already handles) — this catches
    // the user's local uncommitted edits.
    let changed = orchestrator::branch_changed_paths(&project_root, &pr.base, &pr.branch)
        .map_err(|e| unlock(format!("diff branch: {}", e)))?;
    let dirty: std::collections::BTreeSet<String> = orchestrator::dirty_paths(&project_root)
        .map_err(|e| unlock(format!("git status: {}", e)))?
        .into_iter()
        .collect();
    let branch_files: std::collections::BTreeSet<String> = changed.iter().cloned().collect();
    let overlap: Vec<String> = dirty.intersection(&branch_files).cloned().collect();
    if !overlap.is_empty() {
        // Release the lock so other tasks can merge while the user
        // cleans up their working tree. The PR remains Approved; the
        // orchestrator simply re-runs `pr merge` once the user has
        // committed/stashed/reverted.
        let _ = merge_lock::release(project_id, task_id);
        return Ok(MergeOutcomeOp::BlockedByDirtyTree { overlap });
    }

    // Capture which ref the project root was on before we check out `base` to
    // run the merge. We deliberately do NOT restore it here: /simplify, /bump,
    // and `pr finalize`'s build gate all run ON MAIN, so HEAD must stay on
    // `base` through the whole Merging phase. Instead we stash the ref in the
    // merge lock (below, once the merge lands) and `pr finalize` restores it
    // after the on-main phase completes. `None` means we couldn't determine it
    // (detached / git error) — finalize then skips the restore rather than
    // guess.
    let prior_ref = capture_head_ref(&project_root);

    // Step 3: merge feature branch into main. Should be conflict-free
    // since we already merged main into the branch in step 1.
    let checkout = orchestrator::run_git(&project_root, &["checkout", &pr.base])
        .map_err(|e| unlock(format!("git checkout: {}", e)))?;
    if !checkout.status_ok {
        return Err(unlock(format!(
            "git checkout {} failed: {}",
            pr.base,
            checkout.stderr.trim()
        )));
    }
    let msg = format!(
        "cc-hub: merge {} into {} (PR #{})",
        pr.branch, pr.base, pr.id
    );
    let merge_into_main =
        orchestrator::run_git(&project_root, &["merge", "--no-ff", "-m", &msg, &pr.branch])
            .map_err(|e| unlock(format!("git merge: {}", e)))?;

    if !merge_into_main.status_ok {
        // Should be rare given step 1, but possible if main moved
        // concurrently inside the lock window (it shouldn't, since
        // the lock serialises merges). Abort to leave main clean,
        // release the lock, and surface to the orchestrator.
        let conflicting = git_conflicting_paths(&project_root).unwrap_or_default();
        let _ = orchestrator::run_git(&project_root, &["merge", "--abort"]);
        let _ = merge_lock::release(project_id, task_id);
        return Ok(MergeOutcomeOp::ConflictIntoMain {
            conflicting_paths: conflicting,
            stdout: merge_into_main.stdout,
            stderr: merge_into_main.stderr,
        });
    }

    // Stash the pre-merge ref in the lock (unless it was already `base`, or we
    // couldn't determine it) so `pr finalize` can restore HEAD after the
    // on-main phase. We intentionally leave the project root ON `base` here.
    // Best-effort: if the lock write fails, the restore is simply skipped — the
    // merge already landed and the user is on main, which is where /simplify
    // and /bump run anyway.
    if let Some(r) = &prior_ref {
        if r != &pr.base {
            if let Err(e) = merge_lock::set_prior_ref(project_id, task_id, Some(r.clone())) {
                log::warn!("pr merge: stash prior ref {} for finalize failed: {}", r, e);
            }
        }
    }

    // Transition task to Merging. /simplify and /bump still need to run;
    // `pr finalize` flips to Done afterwards.
    orchestrator::update_task_state(project_id, task_id, |s| {
        s.status = TaskStatus::Merging;
        s.note = Some(format!("PR #{}: merged; running /simplify + /bump", pr.id));
        s.merges.push(MergeRecord {
            worktree: pr
                .branch
                .strip_prefix(&format!("cc-hub/{}-", task_id))
                .unwrap_or(&pr.branch)
                .to_string(),
            at: orchestrator::now_unix_secs(),
            outcome: orchestrator::MergeOutcome::Ok,
        });
    })
    .map_err(|e| unlock(format!("update state: {}", e)))?;

    // `restored_ref` is now always None: `pr merge` no longer restores HEAD
    // (it stays on `base` for the on-main Merging phase). The field is kept for
    // the CLI's JSON shape; the actual restore is deferred to `pr finalize`.
    Ok(MergeOutcomeOp::Merged {
        branch: pr.branch,
        base: pr.base,
        stdout: merge_into_main.stdout,
        restored_ref: None,
    })
}

/// The branch HEAD points to in `root`, or `None` when detached or git fails.
/// Used by `pr merge` to remember the user's branch before checking out `base`
/// so it can restore it afterward.
fn capture_head_ref(root: &std::path::Path) -> Option<String> {
    let out = orchestrator::run_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
    if !out.status_ok {
        return None;
    }
    let name = out.stdout.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Outcome of [`pr_continue`].
pub enum ContinueOutcome {
    /// No orchestrator tmux recorded on the task.
    NoOrchestratorTmux,
    /// The recorded orchestrator session is dead.
    OrchestratorDead { tmux: String },
    /// The orchestrator is alive but its pane is busy (mid-turn).
    PaneBusy { tmux: String },
    /// The merge-flow prompt was re-sent.
    Sent { tmux: String },
}

/// `cc-hub pr continue` body: re-ping the task's orchestrator with the
/// merge-flow approval prompt (the same nudge the TUI sends on approve).
/// Idempotent. Returns a [`ContinueOutcome`] describing the result; the CLI
/// renders the JSON and exit behavior.
pub fn pr_continue(project_id: &str, task_id: &str) -> Result<ContinueOutcome, OpError> {
    let state = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;

    let Some(tmux_name) = state.orchestrator_tmux.clone() else {
        return Ok(ContinueOutcome::NoOrchestratorTmux);
    };

    if !send::tmux_session_exists(&tmux_name) {
        return Ok(ContinueOutcome::OrchestratorDead { tmux: tmux_name });
    }

    let prompt =
        orchestrator::build_review_approval_prompt(task_id, &orchestrator::resolve_cc_hub_bin());

    if !send::pane_ready_for_input(&tmux_name) {
        return Ok(ContinueOutcome::PaneBusy { tmux: tmux_name });
    }

    match send::send_prompt(&tmux_name, &prompt) {
        Ok(()) => Ok(ContinueOutcome::Sent { tmux: tmux_name }),
        Err(e) => Err(OpError::Other(format!(
            "send merge-flow prompt to [{}]: {}",
            tmux_name, e
        ))),
    }
}

/// `cc-hub pr lock-phase` body: set the merge-lock sub-phase. Returns the
/// phase string; errors if this task doesn't hold the lock.
pub fn pr_lock_phase(
    project_id: &str,
    task_id: &str,
    phase: merge_lock::MergePhase,
) -> Result<&'static str, OpError> {
    let updated = merge_lock::set_phase(project_id, task_id, phase)
        .map_err(|e| OpError::Other(format!("set merge phase: {}", e)))?;
    if !updated {
        return Err(OpError::Other(format!(
            "task {} does not hold the merge lock for project {} (or no lock exists)",
            task_id, project_id
        )));
    }
    Ok(phase.as_str())
}

/// Run a build command inside `project_root` and capture stdout+stderr.
/// `cmd` runs via `sh -c "<cmd>"` so users can pass pipelines / `&&` chains.
/// Returns (status_ok, stdout, stderr).
fn run_build_command(
    project_root: &std::path::Path,
    cmd: &str,
) -> Result<(bool, String, String), OpError> {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(project_root)
        .output()
        .map_err(|e| OpError::Other(format!("run build: {}", e)))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((out.status.success(), stdout, stderr))
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let take = lines.len().saturating_sub(n);
    lines[take..].join("\n")
}

/// Options for [`pr_finalize`].
pub struct FinalizeOpts {
    pub build_cmd: Option<String>,
    pub skip_build: bool,
    pub keep_tmux: bool,
}

/// Outcome of [`pr_finalize`].
pub enum FinalizeOutcome {
    /// PR was already merged — idempotent no-op.
    AlreadyMerged,
    /// Build gate failed. Carries the resolved command and the stderr tail the
    /// CLI prints (and which was already appended as a PR comment).
    BuildFailed {
        command: String,
        stderr_tail: String,
    },
    /// Lock released, PR Merged, task Done.
    Finalized {
        released: bool,
        build_skipped: bool,
        tmux_kept: bool,
    },
}

/// `cc-hub pr finalize` body: run the build gate on main, then (on success)
/// restore HEAD to the stashed prior ref while the lock is still held, release
/// the merge lock, and only then flip PR + task to terminal states. If the
/// release fails the task stays Merging so a re-run can complete the
/// transition. Side-effect ORDER: restore → release → update_pr →
/// update_task_state.
pub fn pr_finalize(
    project_id: &str,
    task_id: &str,
    opts: FinalizeOpts,
) -> Result<FinalizeOutcome, OpError> {
    // Resolve build command: CLI flag > project config > default.
    let build_cmd = opts
        .build_cmd
        .clone()
        .or_else(|| orchestrator::project_build_cmd(project_id))
        .unwrap_or_else(|| "cargo build --release".to_string());

    // Build gate runs on main after /simplify and /bump. If the tree is
    // broken, refuse to release the lock so a follow-up task can't
    // inherit a red main.
    let state = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    let finalize_root = state
        .require_project()
        .map_err(|e| OpError::Usage(e.to_string()))?
        .1
        .to_path_buf();

    // State guards — check PR + status BEFORE any mutation. The Merged check
    // must come before the status check: after a successful finalize, the
    // task is Done (not Merging), so an idempotent retry would otherwise be
    // refused by the Merging guard.
    let existing_pr = pr::read_pr(project_id, task_id)
        .map_err(|e| OpError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| {
            OpError::Usage(format!(
                "no PR exists for task {} — finalize is only meaningful after `cc-hub pr merge`",
                task_id
            ))
        })?;
    if existing_pr.review_state == ReviewState::Merged {
        return Ok(FinalizeOutcome::AlreadyMerged);
    }
    if state.status != TaskStatus::Merging {
        return Err(OpError::Usage(format!(
            "task {} must be in Merging to finalize (currently {}) — run `cc-hub pr merge --task {} --wait` first",
            task_id, state.status.as_str(), task_id
        )));
    }

    if !opts.skip_build {
        let (ok, _stdout, stderr) = run_build_command(&finalize_root, &build_cmd)?;
        if !ok {
            let tail = tail_lines(&stderr, 80);
            let comment_body = format!(
                "`cc-hub pr finalize` build gate failed.\n\nCommand: `{}`\n\nstderr tail:\n```\n{}\n```",
                build_cmd, tail
            );
            let now = orchestrator::now_unix_secs();
            let _ = pr::update_pr(project_id, task_id, |p| {
                p.comments.push(Comment {
                    author: "cc-hub".into(),
                    at: now,
                    body: comment_body,
                });
            });
            return Ok(FinalizeOutcome::BuildFailed {
                command: build_cmd,
                stderr_tail: tail,
            });
        }
    }

    // Read the ref `pr merge` stashed before it checked out `base`, so we can
    // restore HEAD after the on-main phase. Best-effort: a missing/foreign
    // lock means nothing to restore.
    let prior_ref = merge_lock::current_holder(project_id)
        .ok()
        .flatten()
        .filter(|l| l.task_id == task_id)
        .and_then(|l| l.prior_ref);

    // Restore the project root to the ref it was on before `pr merge` checked
    // out `base`. Deferred to here — after the build gate — so the gate and the
    // preceding /simplify + /bump all run ON MAIN, not on the user's prior
    // branch. Crucially this happens while the merge lock is still HELD: a
    // queued `pr merge --wait` acquires the instant we release, and its
    // checkout-base + merge must never interleave with our checkout of the
    // prior ref — that could land the successor's PR on the user's branch.
    // Best-effort: a failure is a warning, not fatal (the merge already landed
    // and main is a sane place to be left).
    if let Some(r) = &prior_ref {
        match orchestrator::run_git(&finalize_root, &["checkout", r]) {
            Ok(o) if o.status_ok => {}
            Ok(o) => log::warn!(
                "pr finalize: restore HEAD to {} failed: {}",
                r,
                o.stderr.trim()
            ),
            Err(e) => log::warn!("pr finalize: restore HEAD to {} errored: {}", r, e),
        }
    }

    // Release the merge lock BEFORE flipping PR and task to terminal states.
    let released = merge_lock::release(project_id, task_id)
        .map_err(|e| OpError::Other(format!("release merge lock: {}", e)))?;

    pr::update_pr(project_id, task_id, |p| {
        p.review_state = ReviewState::Merged;
    })
    .map_err(|e| OpError::Other(format!("update pr: {}", e)))?;

    let state = orchestrator::update_task_state(project_id, task_id, |s| {
        s.status = TaskStatus::Done;
    })
    .map_err(|e| OpError::Other(format!("update state: {}", e)))?;

    if !opts.keep_tmux {
        orchestrator::cleanup_task_sessions(&state);
    }

    Ok(FinalizeOutcome::Finalized {
        released,
        build_skipped: opts.skip_build,
        tmux_kept: opts.keep_tmux,
    })
}

fn git_rev_parse(root: &std::path::Path, rev: &str) -> Result<String, String> {
    let out = orchestrator::run_git(root, &["rev-parse", rev])
        .map_err(|e| format!("git rev-parse: {}", e))?;
    if !out.status_ok {
        return Err(format!(
            "git rev-parse {} failed: {}",
            rev,
            out.stderr.trim()
        ));
    }
    Ok(out.stdout.trim().to_string())
}

/// `git diff --name-only --diff-filter=U` lists files with unresolved
/// conflicts. Repo-relative paths.
fn git_conflicting_paths(root: &std::path::Path) -> Result<Vec<String>, String> {
    let out = orchestrator::run_git(root, &["diff", "--name-only", "--diff-filter=U", "-z"])
        .map_err(|e| format!("git diff (conflicts): {}", e))?;
    if !out.status_ok {
        return Err(format!("git diff failed: {}", out.stderr.trim()));
    }
    Ok(out
        .stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::orchestrator::TaskState;
    use std::path::PathBuf;

    fn seed(project_id: &str, task_id: &str, status: TaskStatus) -> TaskState {
        let mut state = TaskState::new(
            project_id.into(),
            PathBuf::from("/tmp/proj"),
            "do thing".into(),
        );
        state.task_id = task_id.into();
        state.status = status;
        orchestrator::write_task_state(&state).expect("write state");
        state
    }

    #[test]
    fn pr_create_rejects_backlog_and_leaves_no_orphan_pr() {
        // BUG 5: a `pr create` against a non-Running task must be rejected
        // BEFORE burning a PR id / writing pr.json, or the orphan pr.json
        // permanently blocks future `pr create`.
        crate::test_util::with_temp_home(|| {
            let (project_id, task_id) = ("p-create-guard", "t-create-guard");
            let mut state = TaskState::new_backlog(
                project_id.into(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.into();
            orchestrator::write_task_state(&state).expect("write state");

            let err = pr_create(project_id, task_id, "wt", "t".into(), "d".into())
                .expect_err("backlog task must be rejected");
            assert!(matches!(err, OpError::Usage(_)), "got {:?}", err);

            // No orphan pr.json was written.
            assert!(
                pr::read_pr(project_id, task_id)
                    .expect("read pr")
                    .is_none(),
                "a rejected create must not leave a pr.json behind"
            );
        });
    }

    #[test]
    fn pr_create_from_running_succeeds_and_flips_to_review() {
        crate::test_util::with_temp_home(|| {
            let (project_id, task_id) = ("p-create-ok", "t-create-ok");
            seed(project_id, task_id, TaskStatus::Running);

            let pr = pr_create(project_id, task_id, "wt", "title".into(), "desc".into())
                .expect("create ok from Running");
            assert_eq!(pr.review_state, ReviewState::Open);

            let after = orchestrator::read_task_state(project_id, task_id).expect("read state");
            assert_eq!(after.status, TaskStatus::Review);
            assert!(after.last_auto_reviewed_at.is_none());
        });
    }

    #[test]
    fn pr_create_second_time_reports_already_exists() {
        crate::test_util::with_temp_home(|| {
            let (project_id, task_id) = ("p-create-dup", "t-create-dup");
            seed(project_id, task_id, TaskStatus::Running);
            pr_create(project_id, task_id, "wt", "t".into(), "d".into()).expect("first create");
            // Task is now Review; a second create hits the exists-check under
            // the lock and refuses.
            let err = pr_create(project_id, task_id, "wt", "t".into(), "d".into())
                .expect_err("second create must be refused");
            assert!(matches!(err, OpError::Other(_)), "got {:?}", err);
        });
    }

    #[test]
    fn pr_approve_rejects_non_review_task() {
        // BUG 3: approval is only meaningful for a task in Review.
        crate::test_util::with_temp_home(|| {
            let (project_id, task_id) = ("p-approve-guard", "t-approve-guard");
            let state = seed(project_id, task_id, TaskStatus::Running);
            pr::create_pr(&state, "feature".into(), "main".into(), "t".into(), "d".into())
                .expect("create pr");

            let err = pr_approve(project_id, task_id).expect_err("non-Review must be rejected");
            assert!(matches!(err, OpError::Usage(_)), "got {:?}", err);

            // PR untouched (still Open, not Approved).
            let after = pr::read_pr(project_id, task_id)
                .expect("read pr")
                .expect("present");
            assert_eq!(after.review_state, ReviewState::Open);
        });
    }
}
