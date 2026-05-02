//! Background auto-reviewer. When `[auto_review].enabled` is true, every
//! `interval_secs` cc-hub picks the oldest task in `Review` whose current
//! review round hasn't been auto-reviewed yet, and spawns one **read-only
//! reviewer session** against the project root.
//!
//! The reviewer is briefed via its initial prompt to: read the PR, build,
//! test, and either approve via `cc-hub pr approve` or post a clarification
//! request via `cc-hub pr request-changes` (which flips the task back to
//! Running so the orchestrator iterates). Once the orchestrator closes the
//! loop and the task re-enters Review, `last_auto_reviewed_at` is cleared
//! and the next tick reviews again.
//!
//! Eligibility: status == Review, PR exists with review_state ∈ {Open,
//! ChangesRequested}, and `last_auto_reviewed_at` is None or older than
//! `ttl_secs`. At most one reviewer is spawned per tick — keeps cost
//! bounded under busy review queues.

use crate::config;
use crate::orchestrator::{
    self, now_unix_secs, read_task_state, write_task_state, TaskState, TaskStatus,
};
use crate::pr;
use crate::projects_scan;
use log::{debug, info, warn};

/// One auto-reviewer session that the tick spawned. Carried back to the
/// main loop so it can dispatch the review briefing via `tmux send-keys`
/// for backends that ignore initial prompts (Claude), and so the loop can
/// surface a status string. `prompt_to_dispatch` is `None` when the agent
/// consumed the briefing as its initial prompt (Pi).
#[derive(Debug, Clone)]
pub struct Spawn {
    pub task_id: String,
    pub tmux: String,
    pub prompt_to_dispatch: Option<String>,
}

#[derive(Debug, Default)]
pub struct TickOutcome {
    pub spawn: Option<Spawn>,
    pub status: Option<String>,
}

pub fn tick() -> TickOutcome {
    let cfg = &config::get().auto_review;
    if !cfg.enabled {
        return TickOutcome::default();
    }

    let snap = projects_scan::scan();
    let now = now_unix_secs();
    let ttl = cfg.ttl_secs as i64;

    // Walk every project's task list, filter to Review-state tasks with an
    // open/changes-requested PR that hasn't been auto-reviewed in this
    // round (or whose stamp is past TTL — defence in depth against missed
    // clears). Pick the oldest by `updated_at` so a long-stale Review
    // beats a freshly-opened one — encourages the queue to drain.
    let mut eligible: Vec<(orchestrator::Project, &TaskState)> = Vec::new();
    for p in &snap.projects {
        let Some(tasks) = snap.tasks.get(&p.id) else {
            continue;
        };
        for t in tasks {
            if t.status != TaskStatus::Review {
                continue;
            }
            // Skip if PR is missing or not in a reviewable state.
            let pr_state = match pr::read_pr(&p.id, &t.task_id) {
                Ok(Some(p)) => p.review_state,
                _ => continue,
            };
            if !matches!(
                pr_state,
                pr::ReviewState::Open | pr::ReviewState::ChangesRequested
            ) {
                continue;
            }
            let already_reviewed_this_round = match t.last_auto_reviewed_at {
                None => false,
                Some(at) => now.saturating_sub(at) < ttl,
            };
            if already_reviewed_this_round {
                continue;
            }
            eligible.push((p.clone(), t.as_ref()));
        }
    }

    if eligible.is_empty() {
        return TickOutcome::default();
    }

    eligible.sort_by_key(|(_, t)| t.updated_at);
    let (project, task) = eligible.into_iter().next().unwrap();
    let task_id = task.task_id.clone();

    debug!(
        "auto_review: picked task={} project={} updated_at={}",
        task_id, project.id, task.updated_at
    );

    // Stamp before spawn so a slow spawn doesn't expose us to a second
    // tick re-picking the same task. Direct read/write avoids `touch()`,
    // which would shuffle the kanban ordering on every tick.
    let stamped = match read_task_state(&project.id, &task_id) {
        Ok(mut s) => {
            s.last_auto_reviewed_at = Some(now);
            if let Err(e) = write_task_state(&s) {
                warn!(
                    "auto_review: stamp last_auto_reviewed_at failed for {}: {}",
                    task_id, e
                );
                return TickOutcome::default();
            }
            s
        }
        Err(e) => {
            warn!("auto_review: read state failed for {}: {}", task_id, e);
            return TickOutcome::default();
        }
    };

    let pr_record = match pr::read_pr(&project.id, &task_id) {
        Ok(Some(p)) => p,
        Ok(None) => {
            warn!(
                "auto_review: PR vanished between scan and spawn for {}",
                task_id
            );
            return TickOutcome::default();
        }
        Err(e) => {
            warn!("auto_review: read pr failed for {}: {}", task_id, e);
            return TickOutcome::default();
        }
    };

    let cc_hub_bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!("auto_review: resolve cc-hub binary path: {}", e);
            return TickOutcome::default();
        }
    };

    let agent_id = cfg
        .agent
        .clone()
        .unwrap_or_else(|| config::get().default_orchestrator_agent_id());
    let agent = match config::get().agent(&agent_id) {
        Some(a) => a,
        None => {
            warn!("auto_review: unknown agent id: {}", agent_id);
            return TickOutcome::default();
        }
    };

    let prompt = build_review_prompt(&stamped, &pr_record, &cc_hub_bin);

    let cwd = stamped.project_root.to_string_lossy().into_owned();
    // Claude ignores `initial_prompt` (see spawn::build_agent_command); for
    // those backends the prompt has to be dispatched after the session
    // reaches Idle, same pattern start_backlog_task uses. Pi consumes
    // initial_prompt directly.
    let prompt_to_dispatch = if agent.supports_initial_prompt() {
        None
    } else {
        Some(prompt.clone())
    };
    let initial = if agent.supports_initial_prompt() {
        Some(prompt.as_str())
    } else {
        None
    };
    let tmux = match crate::spawn::spawn_agent_session(
        &agent_id, &cwd, None, initial, /* readonly */ true,
    ) {
        Ok(name) => name,
        Err(e) => {
            warn!("auto_review: spawn reviewer for {} failed: {}", task_id, e);
            return TickOutcome {
                spawn: None,
                status: Some(format!(
                    "auto-review: spawn failed for {}: {}",
                    task_id, e
                )),
            };
        }
    };

    info!(
        "auto_review: spawned reviewer [{}] for task {} (PR #{})",
        tmux, task_id, pr_record.id
    );
    TickOutcome {
        spawn: Some(Spawn {
            task_id: task_id.clone(),
            tmux: tmux.clone(),
            prompt_to_dispatch,
        }),
        status: Some(format!(
            "auto-review: reviewing PR #{} on task {} in [{}]",
            pr_record.id, task_id, tmux
        )),
    }
}

fn build_review_prompt(
    state: &TaskState,
    pr: &pr::PullRequest,
    cc_hub_bin: &std::path::Path,
) -> String {
    let bin = cc_hub_bin.display().to_string();
    let cap = config::get().auto_review.max_comments_in_prompt as usize;
    format!(
        "You are an autonomous code reviewer for cc-hub. A task you have NEVER seen before just opened a pull request, and your job is to either APPROVE it or REQUEST CHANGES with a precise question. There is no human in this loop — your verdict drives whether the orchestrator merges or iterates.

# Context
- Project root (your cwd): {root}
- Task id: {task_id}
- Project id: {project_id}
- PR #{pr_id}: {pr_title}
- Feature branch: {branch}
- Base branch: {base}

# Original task prompt
{user_prompt}

# PR description
{pr_desc}

# Existing comment thread (may be empty)
{comments}

# Tools available to you
You are running in **read-only** mode — your tool layer cannot edit files. That is by design. You can:
- inspect the diff: `git -C {root_q} diff {base}...{branch}` or `git -C {root_q} log {base}..{branch}`
- check out files at the feature branch: `git -C {root_q} show {branch}:<path>` or `git -C {root_q} ls-tree -r {branch}`
- run builds and tests *against the feature branch worktree* if it exists at `.cc-hub-wt/`. Find it via `git -C {root_q} worktree list`.
- read any file under the project root.

# Your verdict — exactly ONE of these CLI calls

1. **Approve** — only if the diff cleanly delivers the original task, builds, tests pass, and no clarification is needed:
   `{bin} pr approve --task {task_id}`

2. **Request changes** — if anything is unclear, broken, missing, or you need the orchestrator to clarify intent. Always include a focused question or actionable change request in --comment:
   `{bin} pr request-changes --task {task_id} --author auto-reviewer --comment \"<your question or change request>\"`
   This flips the task back to Running so the orchestrator can address your comment. Once the orchestrator pushes a fix and re-enters Review, you will run again automatically.

# Rules
- Do NOT post freeform `pr comment` messages — they don't drive the loop. Always use `pr approve` or `pr request-changes` so the kanban moves.
- Make exactly ONE verdict call. Stop after it succeeds.
- If you genuinely cannot determine whether to approve (e.g. the build environment is missing a dependency that's not your fault), prefer `request-changes` with a comment naming the obstacle — never silently approve to avoid stalling.
- Be terse in your comment. The orchestrator is another agent: one paragraph max, concrete asks, file:line references where useful.
- Do not edit files, push commits, or merge. Your job is verdict only.

Begin by inspecting the diff, then run any build/test you need, then issue your verdict.
",
        root = state.project_root.display(),
        root_q = state.project_root.display(),
        task_id = state.task_id,
        project_id = state.project_id,
        pr_id = pr.id,
        pr_title = pr.title,
        branch = pr.branch,
        base = pr.base,
        user_prompt = state.prompt,
        pr_desc = if pr.description.trim().is_empty() {
            "(no description)".to_string()
        } else {
            pr.description.clone()
        },
        comments = format_comments(&pr.comments, cap),
        bin = bin,
    )
}

fn format_comments(comments: &[pr::Comment], cap: usize) -> String {
    if comments.is_empty() {
        return "(none)".to_string();
    }
    let skipped = comments.len().saturating_sub(cap);
    let mut out = String::new();
    if skipped > 0 {
        out.push_str(&format!("(+{} older comments not shown)\n", skipped));
    }
    for c in comments.iter().skip(skipped) {
        out.push_str(&format!("- [{}] {}\n", c.author, c.body));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_state() -> TaskState {
        let mut s = TaskState::new(
            "proj".into(),
            PathBuf::from("/tmp/proj"),
            "Add a foo helper".into(),
        );
        s.task_id = "t-42".into();
        s
    }

    fn fake_pr() -> pr::PullRequest {
        pr::PullRequest {
            id: 7,
            task_id: "t-42".into(),
            project_id: "proj".into(),
            branch: "cc-hub/t-42-foo".into(),
            base: "main".into(),
            title: "Add foo helper".into(),
            description: "Adds Foo for X.".into(),
            review_state: pr::ReviewState::Open,
            comments: vec![pr::Comment {
                author: "orchestrator".into(),
                at: 0,
                body: "ready for review".into(),
            }],
            approved_at_branch_sha: None,
            approved_at_base_sha: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn prompt_mentions_verdict_verbs() {
        let s = fake_state();
        let p = fake_pr();
        let bin = PathBuf::from("/usr/local/bin/cc-hub");
        let out = build_review_prompt(&s, &p, &bin);
        assert!(out.contains("pr approve --task t-42"));
        assert!(out.contains("pr request-changes --task t-42"));
        assert!(out.contains("--author auto-reviewer"));
        assert!(out.contains("Add a foo helper"));
        assert!(out.contains("PR #7"));
    }

    #[test]
    fn prompt_handles_empty_description_and_comments() {
        let s = fake_state();
        let mut p = fake_pr();
        p.description = String::new();
        p.comments.clear();
        let out = build_review_prompt(&s, &p, &std::path::Path::new("/cc-hub"));
        assert!(out.contains("(no description)"));
        assert!(out.contains("(none)"));
    }

    #[test]
    fn format_comments_caps_to_last_n_with_footer() {
        let comments: Vec<pr::Comment> = (0..20)
            .map(|i| pr::Comment {
                author: format!("author{}", i),
                at: i as i64,
                body: format!("body{}", i),
            })
            .collect();
        let out = format_comments(&comments, 5);
        let body_lines = out.lines().filter(|l| l.starts_with("- [")).count();
        assert_eq!(body_lines, 5);
        assert!(out.contains("(+15 older comments not shown)"));
        assert!(out.contains("- [author19] body19"));
        assert!(out.contains("- [author15] body15"));
        assert!(!out.contains("- [author14]"));
    }
}
