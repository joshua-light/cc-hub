//! Orchestrator prompt construction + session spawn/lifecycle flows.

use super::{
    ensure_project_registered, read_task_state, update_task_state, write_task_state, TaskState,
    TaskStatus,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// First user message dispatched to a freshly-spawned orchestrator session.
///
/// This is *the* contract between cc-hub and the orchestrator role: it
/// teaches a stock Claude Code session about the four CLI primitives, sets
/// expectations (decompose, don't impl; parallelize reads; serialize edits),
/// and embeds the user's actual task verbatim. Keep it concise — the
/// orchestrator pays for it on every turn.
///
/// `cc_hub_bin` is the absolute path to the cc-hub binary running this
/// process — pre-substituted into every example so the orchestrator's Bash
/// shell doesn't need cc-hub on `PATH` (a real failure mode observed in
/// the first end-to-end run, where the orch had to guess the path).
/// Stable prefix of every orchestrator prompt. Shared with the resurrect
/// path so a JSONL whose first user message starts with this is unambiguously
/// the orchestrator's session — not a sibling Claude session that happens to
/// run in the same cwd.
pub fn orchestrator_prompt_prefix(task_id: &str) -> String {
    format!("You are the cc-hub orchestrator for task `{}`", task_id)
}

/// Resolve the running cc-hub binary path, stripping Linux's ` (deleted)`
/// suffix. The kernel appends that suffix after the on-disk inode is replaced
/// (e.g. a fresh `cargo build` while this process keeps running), and the
/// suffixed string is not a path that resolves anywhere.
pub fn resolve_cc_hub_bin() -> PathBuf {
    let raw = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return PathBuf::from("cc-hub"),
    };
    resolve_cc_hub_bin_from(raw, |p| p.exists())
}

fn resolve_cc_hub_bin_from(raw: PathBuf, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if let Some(stripped) = raw.to_string_lossy().strip_suffix(" (deleted)") {
        let candidate = PathBuf::from(stripped);
        if exists(&candidate) {
            return candidate;
        }
    }
    raw
}

pub fn build_review_approval_prompt(task_id: &str, cc_hub_bin: &Path) -> String {
    let bin = cc_hub_bin.display();
    format!(
        "The user approved the PR for task `{task_id}` in cc-hub's Review column.

Continue the merge flow now:
1. Run `{bin} pr show --task {task_id}` to confirm the PR is still `approved`.
2. If it is, run `{bin} pr merge --task {task_id}`.
3. While the merge lock is held, run `/simplify` and `/bump`. `pr finalize` runs the build itself — do not pre-staple `cargo build`.
4. Finish with `{bin} pr finalize --task {task_id}`.

If `pr merge` reports conflicts or a dirty-tree refusal, follow that recipe instead of forcing the merge. Do not ask the user for another approval unless the PR was demoted back to `open` and needs re-review."
    )
}

pub fn build_orchestrator_prompt(state: &TaskState, cc_hub_bin: &Path) -> String {
    let TaskState {
        task_id,
        project_id,
        project_root,
        prompt,
        orchestrator_agent_id,
        ..
    } = state;
    let bin = cc_hub_bin.display();
    let prefix = orchestrator_prompt_prefix(task_id);
    format!(
        "{prefix} in project `{project_id}` at `{root}`.

Your job is to deliver the user's task end-to-end via a Pull Request: explore, decompose, dispatch workers into a worktree, open a PR, iterate on review feedback, and merge when the user approves. **You never edit `main` directly.** Every change lands through the PR flow so the user sees a reviewable diff before anything touches their working branch.

The cc-hub binary is at `{bin}`. Always invoke it by absolute path; it is not necessarily on PATH inside worker shells.
You're currently running as agent `{orchestrator_agent_id}`. Workers you spawn inherit this agent by default; pass `--agent <id>` when you want a different backend.

# Start here

1. **Explore.** Read the files the prompt actually touches. Get a real picture of the work before deciding how to do it.

2. **Decompose.** Break the task into sub-tasks. Note which can run in parallel (read-only research, or edits to disjoint files) and which must run serially. A trivial task may be one sub-task; that's fine.

3. **Open a status report** so the user sees you've started:
   `{bin} task report --task {task_id} --status running --note \"<one line — what you're doing>\"`

# Working in a worktree

All edits happen inside a worktree branch. You **do not** edit the project's main branch from inside this orchestrator session.

- Spin up an editing worker with:
  `{bin} spawn-worker --task {task_id} --worktree NAME --prompt \"…\"`
  cc-hub creates a fresh worktree at `.cc-hub-wt/{task_id}-NAME` on a new branch off main. Multiple worktree workers may run in parallel only if they edit disjoint files; otherwise serialise them.

- Spin up read-only research with:
  `{bin} spawn-worker --task {task_id} --readonly --prompt \"…\"`
  No edits, no worktree, runs in the project root. Many can run at once.

- Each `spawn-worker` emits one JSON line on stdout — `worktree` is what you pass to subsequent verbs like `worker wait --worktree NAME` (use `tmux` for read-only spawns, which have no worktree).

- If a worker creates or edits `.gitignore`, instruct it to include `.cc-hub-wt/` so cc-hub's worktree dirs don't pollute future commits.

# Waiting for workers to finish

The fastest way to know a worker is done is to block on `cc-hub worker wait`. It polls the scanner at sub-second cadence and returns as soon as the worker reaches `WaitingForInput` (turn ended), `Question` (agent blocked on AskUserQuestion), or `Inactive` (process gone). Use this instead of shell sleep loops or repeated tmux captures — those add 60–90s of LLM-driven latency per spawn.

- Wait on a single worker (most common):
  `{bin} worker wait --task {task_id} --worktree NAME`

- Spawn N workers in parallel, then block until all finish:
  `{bin} worker wait --task {task_id} --all`
  (`--all` waits on every worker recorded on the task. Pass repeated `--worktree NAME` flags — or `--tmux NAME` for read-only workers — to wait on a subset.)

- Default timeout is 1800s (30 min). Override with `--timeout-secs N` for unusually long-running workers.

The verb prints one JSON line summarising each target's final `state` and `last_user_message`. `all_done: false` with `timed_out: true` means a worker is still busy past the deadline — use this as a signal to investigate, not to silently retry.

When you really do need to peek inside a still-running worker (debugging, mid-task interventions), `tmux capture-pane -t <tmux>:0 -p` shows the worker's current screen, and the on-disk session transcript (`~/.claude/projects/<sanitised-cwd>/<sid>.jsonl` for Claude, `~/.pi/agent/sessions/--encoded-cwd--/*.jsonl` for Pi) has full history. These are debugging tools, not the wait mechanism.

Never write `until [ -f X ]; do sleep …; done` shell loops or `sleep 60 && tmux capture-pane …` chains — they hide stuck workers behind long timeouts and waste turns on idle waiting.

# Opening the PR

Once the worktree branch has the change you want the user to review:

1. **Verify the worktree builds and tests pass** before you open the PR. A red PR wastes the user's review cycle. Run `cargo build`, `pnpm test`, etc. inside the worktree (not on main).

2. **Gather proof of work** (see *Proof of work* below) — at minimum one `--lead` artifact.

3. **Open the PR**:
   `{bin} pr create --task {task_id} --worktree NAME --title \"<headline>\" --description \"$(cat <<'EOF'
<one or two short paragraphs: what changed, why, and what to look at first>
EOF
)\"`
   This transitions the task `Running → Review`, allocates a PR id, and surfaces a card in the user's Review column. Your tmux stays alive through Review so you can iterate on feedback.

# Iterating on review feedback

The user reviews the PR in the TUI. Two outcomes:

- **Changes requested.** The task transitions back to `Running` and the PR's `review_state` becomes `changes_requested`. Poll for it with `{bin} pr show --task {task_id}` — inspect the latest PR state and any new comments. On the first poll after PR open, capture `pr.updated_at` from the response. On every subsequent poll, pass `--comments-since <previous pr.updated_at>` so only NEW comments come back, then stash the new `pr.updated_at` for the next round. The response also returns `comments_total` (all comments on the PR) and `comments_returned` (how many came back after filtering) — use those to sanity-check the filter is doing what you expect. Push the fix to the worktree branch (never main), then run `{bin} pr reopen --task {task_id} --comment \"<reply explaining the fix>\"`. This flips the PR back to Open, transitions the task `Running → Review`, and re-arms auto-review on the new commits.
- **Approved.** When `review_state` becomes `approved`, proceed to **Merging** below.

# Merging (the only path edits reach main)

Merging is **serialized project-wide** by the merge lock — at most one task is in the Merging state at a time. cc-hub handles the lock automatically; you just call the verbs in order.

1. **Acquire the lock and run the merge**:
   `{bin} pr merge --task {task_id} --wait`
   This:
   - Acquires the project's merge lock. **Pass `--wait` so the verb blocks in-process until the lock is free** (default 30 min cap, override with `--timeout-secs N`); without it, you get `ok=false, locked=true` and have to reinvent polling. Do not write Monitor tasks, until-loops, or sleep+capture chains for this — `--wait` already does it correctly and inherits stale-lock recovery.
   - Merges `main` into the feature branch first, so any conflicts with main's recent landings are resolved on the *feature branch* (not on main itself).
   - On clean merge, fast-forwards the feature branch into main.
   - On conflict during the main → branch merge, **the PR is auto-demoted to Open**, the lock is released, and a comment is appended explaining what happened. You then need to spawn a worker to resolve conflicts in the worktree, push the resolution, and ask the user to re-approve. (cc-hub's auto-approve rule only accepts *clean* resolutions; substantive conflict resolutions need a fresh review.)
   - On dirty-tree refusal (the user has uncommitted edits on main overlapping the branch's files), the lock is released and you surface the recipe verbatim — do NOT touch the user's working tree.

2. **Run `/simplify`** via the Skill tool while the merge lock is still held. This cleans up the just-merged code on main; it may add follow-up commits.

3. **Run `/bump`** to cut a version commit reflecting the final tree.

4. **Skip pre-stapled builds.** `pr finalize` runs the build itself before releasing the merge lock — do not pre-staple `cargo build`. The default build command is `cargo build --release`; override per-project via `projects.toml` `build_cmd`, or per-invocation via `--build-cmd CMD`. Pass `--skip-build` only when the project has no fast build to run.

5. **Finalize**:
   `{bin} pr finalize --task {task_id}`
   Releases the merge lock, marks the PR `Merged`, transitions the task `Merging → Done`, and tears down the orchestrator tmux. Your job ends here.

# Reporting progress

After each meaningful step (worker spawned, worker finished, PR opened, changes requested, merge attempted, etc.):
`{bin} task report --task {task_id} --status running --note \"<one line>\"`

Keep notes terse — milestones, not play-by-play.

# Todos (optional)

For tasks with 3+ logical steps, a checklist surfaces `done/total ✓` on the active task card. Set once with a heredoc; mark by 0-based index:
`{bin} task todos set --task {task_id} --items \"$(cat <<'EOF'
plan worktree split
spawn worker A
spawn worker B
open PR
EOF
)\"`
- `{bin} task todos check --task {task_id} --index 1` — mark item done.
- `{bin} task todos uncheck --task {task_id} --index 1` — undo.
- `{bin} task todos clear --task {task_id}` — empty the list.

Don't pre-list every micro-step; aim for a checklist the user could read in one breath.

# Proof of work

**Progressive disclosure**: the user reads the title + lead artifact first; description is an appendix. Attach evidence with `{bin} task artifact add --task {task_id} --path PATH [--kind KIND] [--caption TEXT] [--lead]` (file or URL); list with `{bin} task artifact list --task {task_id}`. Pass `--lead` on exactly one artifact — the strongest single piece of proof; re-passing it on a later add moves the designation.

Rule of thumb: lead with a screenshot/recording for UI; a log (or recording) for CLI/backend; the green build log for refactors; the after-log or new test file for bug fixes.

# Queuing follow-up work

If you spot substantive follow-up work — a separate problem out of scope here — create a Backlog task instead of expanding scope:
`{bin} task create --backlog --prompt \"<scoped prompt for the follow-up>\" [--project-id ID]`

Writes a new task with status `backlog`; does NOT spawn an orchestrator. The user reviews and starts it manually. Keep the prompt self-contained — the future orchestrator won't have your context.

# Rules

- **Never edit `main` directly.** All changes flow worktree → PR → user-approved merge. The merge lock is the only thing that mutates main, and only `pr merge` acquires it.
- Don't ask the user clarifying questions. If the task is ambiguous, pick the most reasonable interpretation and note your assumption in the first status report.
- Each worktree owns its files. Don't run two parallel worktree workers whose files overlap.
- If you hit an unrecoverable issue, leave a note via `{bin} task report --task {task_id} --note \"<why>\"` and stop — the user will pick it up from the kanban.

# Your task

{prompt}

Begin by exploring the relevant files, then open with your first `{bin} task report`. Spin up worktree workers as needed, open the PR when ready, iterate on feedback, and merge once approved.",
        task_id = task_id,
        project_id = project_id,
        root = project_root.display(),
        bin = bin,
        prompt = prompt,
        orchestrator_agent_id = orchestrator_agent_id,
    )
}

/// Create + persist a fresh task and spawn its orchestrator session.
///
/// Returns the `(TaskState, tmux_session_name, prompt_to_dispatch)` so callers
/// can queue the orchestrator prompt only when the chosen backend needs a
/// follow-up tmux paste. Pi can consume the initial prompt directly, so its
/// `prompt_to_dispatch` is `None`.
///
/// Concretely:
/// 1. registers the project (if new) in `~/.cc-hub/projects.toml`
/// 2. writes the initial task state
/// 3. spawns the configured orchestrator backend via the existing detached-tmux pathway
/// 4. records the resulting tmux name back on the state
///
/// This mirrors what `cc-hub orchestrate start` does, minus the synchronous
/// idle-poll/dispatch — the TUI prefers async dispatch so the keystroke
/// returns instantly.
/// Resolve the orchestrator agent, stamp its identity onto `state`, build the
/// orchestrator prompt, and spawn the detached agent session in the task's
/// project root. Returns `(tmux_session_name, prompt_to_dispatch)` where
/// `prompt_to_dispatch` is `Some` only for backends that can't take an initial
/// prompt at spawn (they need a follow-up tmux paste); `None` when the prompt
/// was delivered at spawn time. The caller is responsible for recording
/// `tmux_name` onto `state.orchestrator_tmux`, `touch()`-ing, and persisting —
/// this lets `restart_task` slot an old-tmux kill in between a successful spawn
/// and the state commit.
///
/// Shared by `spawn_orchestrator_for_new_task`, `start_backlog_task`, and
/// `restart_task` so the prompt build and the `supports_initial_prompt`
/// dispatch logic can't drift apart.
fn launch_orchestrator_session(
    state: &mut TaskState,
    agent_id_override: Option<&str>,
) -> io::Result<(String, Option<String>)> {
    let agent_id = agent_id_override
        .map(str::to_string)
        .unwrap_or_else(|| crate::config::get().default_orchestrator_agent_id());
    let agent = crate::config::get()
        .agent(&agent_id)
        .ok_or_else(|| io::Error::other(format!("unknown orchestrator agent: {}", agent_id)))?;

    let cc_hub_bin = resolve_cc_hub_bin();
    state.orchestrator_agent_id = agent_id.clone();
    state.orchestrator_agent_kind = agent.kind;
    let orchestrator_prompt = build_orchestrator_prompt(state, &cc_hub_bin);

    let cwd = state.project_root.to_string_lossy().into_owned();
    let supports_initial = agent.supports_initial_prompt();
    let prompt_to_dispatch = if supports_initial {
        None
    } else {
        Some(orchestrator_prompt.clone())
    };
    let tmux_name = crate::spawn::spawn_agent_session(
        &agent_id,
        &cwd,
        None,
        supports_initial.then_some(orchestrator_prompt.as_str()),
        false,
    )?;

    Ok((tmux_name, prompt_to_dispatch))
}

pub fn spawn_orchestrator_for_new_task(
    project_root: &Path,
    project_name: &str,
    user_prompt: String,
    agent_id_override: Option<&str>,
) -> io::Result<(TaskState, String, Option<String>)> {
    let project_id = ensure_project_registered(project_root, project_name)?;
    let canonical_root =
        fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let mut state = TaskState::new(project_id, canonical_root, user_prompt);

    let (tmux_name, prompt_to_dispatch) =
        launch_orchestrator_session(&mut state, agent_id_override)?;

    state.orchestrator_tmux = Some(tmux_name.clone());
    state.touch();
    write_task_state(&state)?;

    Ok((state, tmux_name, prompt_to_dispatch))
}

/// User-initiated transition from Backlog to Running. Mirrors
/// spawn_orchestrator_for_new_task but operates on an existing Backlog task
/// instead of creating a new one. Called from the TUI when the user hits the
/// start-task keybind, and from \ on the CLI.
///
/// Claim-first: the Backlog → Running flip is committed under the lock BEFORE
/// the seconds-long agent spawn, so a concurrent second start sees a non-
/// Backlog status and bails instead of spawning a duplicate orchestrator that
/// would silently overwrite the first's tmux. If the spawn then fails, the
/// claim is rolled back to Backlog so the task stays retryable.
pub fn start_backlog_task(
    project_id: &str,
    task_id: &str,
    agent_id_override: Option<&str>,
) -> io::Result<(TaskState, String, Option<String>)> {
    // Claim the task under the lock, re-verifying the precondition there. A
    // captured flag distinguishes "we claimed it" from "someone else already
    // did" — Running → Running is a legal self-transition, so the status flip
    // alone can't detect a lost race.
    let mut lost_race = false;
    let mut claimed = update_task_state(project_id, task_id, |s| {
        if s.status != TaskStatus::Backlog {
            lost_race = true;
            return;
        }
        s.status = TaskStatus::Running;
        // A Backlog task has no live orchestrator; clearing the runtime refs
        // here makes `orchestrator_tmux == None` a reliable "claim not yet
        // fulfilled" marker for the rollback guard below.
        s.orchestrator_session_id = None;
        s.orchestrator_tmux = None;
    })?;
    if lost_race {
        return Err(io::Error::other(format!(
            "task already started (status = {:?}); refusing to spawn a second orchestrator",
            claimed.status
        )));
    }

    let (tmux_name, prompt_to_dispatch) =
        match launch_orchestrator_session(&mut claimed, agent_id_override) {
            Ok(v) => v,
            Err(e) => {
                // Spawn failed after the claim — roll the status back so the
                // task returns to Backlog instead of stranding in Running
                // with no orchestrator. The tmux guard keeps the rollback from
                // stomping a competitor (e.g. a concurrent `restart_task`)
                // that has since claimed the task and recorded its own live
                // orchestrator.
                let _ = update_task_state(project_id, task_id, |s| {
                    if s.status == TaskStatus::Running && s.orchestrator_tmux.is_none() {
                        s.status = TaskStatus::Backlog;
                    }
                });
                return Err(e);
            }
        };

    // Record the tmux + resolved agent identity in a second locked update.
    // Re-reading here (rather than writing the pre-spawn snapshot back
    // wholesale) means a concurrent `task report`/artifact write isn't
    // clobbered.
    let agent_id = claimed.orchestrator_agent_id.clone();
    let agent_kind = claimed.orchestrator_agent_kind;
    let state = update_task_state(project_id, task_id, |s| {
        s.status = TaskStatus::Running;
        s.orchestrator_agent_id = agent_id;
        s.orchestrator_agent_kind = agent_kind;
        s.orchestrator_tmux = Some(tmux_name.clone());
    })?;

    Ok((state, tmux_name, prompt_to_dispatch))
}

/// Restart a task's orchestrator from scratch using the original prompt.
/// Kills any live orchestrator tmux, clears the recorded session, forces
/// status back to Running, and spawns a fresh orchestrator. Workers,
/// merges, artifacts, and the user prompt are preserved as history — only
/// the orchestrator-side runtime state is reset. Refuses to interrupt Review,
/// Done, or an in-progress merge.
///
/// Claim-first, like [`start_backlog_task`]: the flip to Running (and the
/// clearing of the old runtime state) is committed under the lock BEFORE the
/// spawn, re-verifying the task is still restartable there. The replacement
/// session is spawned before the old tmux is killed, so a spawn failure rolls
/// the claim back and leaves the old orchestrator intact and tracked.
pub fn restart_task(
    project_id: &str,
    task_id: &str,
    agent_id_override: Option<&str>,
) -> io::Result<(TaskState, String, Option<String>)> {
    // Read once for a friendly early error on the guarded states and to grab
    // the old tmux. The authoritative precondition re-check happens inside
    // the claim below, under the lock.
    let pre = read_task_state(project_id, task_id)?;
    match pre.status {
        TaskStatus::Done => {
            return Err(io::Error::other(
                "task is Done — restart would re-run a finished task; use a new task instead",
            ));
        }
        TaskStatus::Review => {
            return Err(io::Error::other(
                "task is Review — restart would ignore the existing PR; request changes or create a new task instead",
            ));
        }
        TaskStatus::Merging => {
            return Err(io::Error::other(
                "task is Merging — restart would interrupt the merge flow",
            ));
        }
        TaskStatus::Backlog | TaskStatus::Running => {}
    }
    let old_tmux = pre.orchestrator_tmux.clone();

    // Claim under the lock: re-verify still-restartable, flip to Running, and
    // clear the orchestrator runtime state. Committing the claim before the
    // seconds-long spawn closes the window where a concurrent transition (or
    // a second restart) could slip in and strand an orchestrator.
    // `claimed_from` records the actual pre-claim status so a spawn failure
    // rolls back to exactly where it started.
    let mut lost_race = false;
    let mut claimed_from: Option<TaskStatus> = None;
    let mut claimed = update_task_state(project_id, task_id, |s| {
        match s.status {
            TaskStatus::Backlog | TaskStatus::Running => {}
            _ => {
                lost_race = true;
                return;
            }
        }
        claimed_from = Some(s.status.clone());
        s.status = TaskStatus::Running;
        s.orchestrator_session_id = None;
        s.orchestrator_tmux = None;
    })?;
    if lost_race {
        return Err(io::Error::other(format!(
            "task is no longer restartable (status = {:?})",
            claimed.status
        )));
    }

    let (tmux_name, prompt_to_dispatch) =
        match launch_orchestrator_session(&mut claimed, agent_id_override) {
            Ok(v) => v,
            Err(e) => {
                // Roll back to the pre-claim status and restore the old tmux
                // ref: the previous orchestrator is still alive (we only kill
                // it after a successful spawn), so it must stay tracked. Both
                // restores are gated on the claim still being ours (status
                // Running, no tmux recorded) — a competitor that re-claimed
                // and recorded its own orchestrator must not be stomped.
                let restore_status = claimed_from.unwrap_or(TaskStatus::Running);
                let restore_tmux = old_tmux.clone();
                let _ = update_task_state(project_id, task_id, |s| {
                    if s.status == TaskStatus::Running && s.orchestrator_tmux.is_none() {
                        s.status = restore_status;
                        s.orchestrator_tmux = restore_tmux;
                    }
                });
                return Err(e);
            }
        };

    // Spawn succeeded — now it's safe to kill the previous orchestrator.
    if let Some(tmux) = old_tmux.as_deref() {
        if crate::send::tmux_session_exists(tmux) {
            let _ = crate::send::kill_tmux_session(tmux);
        }
    }

    // Second locked update records the new tmux; re-reading avoids clobbering
    // a concurrent state write during the spawn.
    let agent_id = claimed.orchestrator_agent_id.clone();
    let agent_kind = claimed.orchestrator_agent_kind;
    let state = update_task_state(project_id, task_id, |s| {
        s.status = TaskStatus::Running;
        s.orchestrator_session_id = None;
        s.orchestrator_agent_id = agent_id;
        s.orchestrator_agent_kind = agent_kind;
        s.orchestrator_tmux = Some(tmux_name.clone());
    })?;

    Ok((state, tmux_name, prompt_to_dispatch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_cc_hub_bin_strips_deleted_suffix_when_sibling_exists() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("cc-hub");
        fs::write(&real, b"#!/bin/sh\necho cc-hub\n").unwrap();

        let suffixed = PathBuf::from(format!("{} (deleted)", real.display()));
        let resolved = resolve_cc_hub_bin_from(suffixed, |p| p.exists());
        assert_eq!(resolved, real);
    }

    #[test]
    fn resolve_cc_hub_bin_leaves_clean_path_alone() {
        let p = PathBuf::from("/usr/bin/cc-hub");
        let resolved = resolve_cc_hub_bin_from(p.clone(), |_| true);
        assert_eq!(resolved, p);
    }

    #[test]
    fn resolve_cc_hub_bin_falls_back_to_raw_when_stripped_missing() {
        let raw = PathBuf::from("/nonexistent/cc-hub (deleted)");
        let resolved = resolve_cc_hub_bin_from(raw.clone(), |_| false);
        assert_eq!(resolved, raw);
    }

    #[test]
    fn review_approval_prompt_contains_merge_flow() {
        let bin = Path::new("/opt/cc-hub/bin/cc-hub");
        let p = build_review_approval_prompt("t-123", bin);
        let bin_s = bin.display().to_string();

        for cmd in [
            format!("{} pr show --task t-123", bin_s),
            format!("{} pr merge --task t-123", bin_s),
            format!("{} pr finalize --task t-123", bin_s),
        ] {
            assert!(p.contains(&cmd), "approval prompt missing command: {}", cmd);
        }
        assert!(p.contains("/simplify"), "approval prompt missing /simplify");
        assert!(p.contains("/bump"), "approval prompt missing /bump");
        assert!(
            p.contains("dirty-tree refusal") || p.contains("conflicts"),
            "approval prompt missing merge-failure guidance"
        );
    }

    #[test]
    fn orchestrator_prompt_substitutes_ids_and_user_prompt() {
        let state = TaskState::new(
            "myproj-42".into(),
            PathBuf::from("/work/myproj"),
            "redo the import pipeline so it streams".into(),
        );
        let bin = Path::new("/opt/cc-hub/bin/cc-hub");
        let p = build_orchestrator_prompt(&state, bin);

        // Identity substitutions.
        assert!(p.contains(&state.task_id), "missing task_id");
        assert!(p.contains("myproj-42"), "missing project_id");
        assert!(p.contains("/work/myproj"), "missing project_root");
        assert!(
            p.contains("redo the import pipeline so it streams"),
            "user prompt missing"
        );

        // PR-flow primitives — every command the orchestrator is expected
        // to invoke must appear in the prompt with the absolute binary path
        // pre-substituted. If any of these drift, the orchestrator's Bash
        // shell would have to guess the path (a real failure mode).
        let bin_s = bin.display().to_string();
        let expected_primitives = [
            format!("{} spawn-worker --task {}", bin_s, state.task_id),
            format!("{} worker wait --task {}", bin_s, state.task_id),
            format!("{} pr create --task {}", bin_s, state.task_id),
            format!("{} pr show --task {}", bin_s, state.task_id),
            format!("{} pr reopen --task {}", bin_s, state.task_id),
            format!("{} pr merge --task {}", bin_s, state.task_id),
            format!("{} pr finalize --task {}", bin_s, state.task_id),
            format!("{} task report --task {}", bin_s, state.task_id),
        ];
        for cmd in &expected_primitives {
            assert!(p.contains(cmd), "primitive missing from prompt: {}", cmd);
        }

        // Load-bearing rules — keep these concise checks so wording can drift.
        assert!(p.contains("decompose"), "missing decomposition framing");
        assert!(
            p.contains("clarifying"),
            "missing 'don't ask clarifying questions' rule"
        );
        assert!(
            p.contains(".cc-hub-wt/"),
            "missing .cc-hub-wt/ gitignore guidance"
        );
        assert!(
            p.contains("tmux capture-pane"),
            "missing capture-pane monitor guidance"
        );

        // Core PR-flow framing. The orchestrator must *never* edit main
        // directly — every change flows through a worktree branch and a PR.
        assert!(
            p.contains("Pull Request") || p.contains("PR"),
            "missing PR-flow framing"
        );
        assert!(
            p.contains("Never edit `main` directly")
                || p.contains("never edit `main` directly")
                || p.contains("You **do not** edit"),
            "missing 'never edit main directly' rule"
        );
        assert!(
            p.contains("merge lock"),
            "missing merge-lock framing — the prompt must explain that merges \
             are serialized project-wide"
        );
        assert!(p.contains("Merging"), "missing Merging state reference");
        assert!(
            p.contains("auto-demoted") || p.contains("auto-approve"),
            "missing auto-approve / auto-demote conflict-resolution policy"
        );

        // Proof-of-work guidance — done isn't done without evidence.
        assert!(
            p.contains("Proof of work"),
            "missing proof-of-work section header"
        );
        assert!(
            p.contains(&format!(
                "{} task artifact add --task {}",
                bin_s, state.task_id
            )),
            "missing artifact-add primitive in proof-of-work section"
        );
        assert!(
            p.contains("--lead"),
            "missing --lead guidance in proof-of-work section"
        );
        assert!(
            p.contains("Progressive disclosure"),
            "missing progressive-disclosure framing"
        );

        // Section is kept terse — this prompt is paid every orchestrator
        // turn. If you re-expand it, raise the bound below deliberately.
        let after_header = p
            .split_once("# Proof of work")
            .expect("Proof of work header present")
            .1;
        let proof_section = after_header.split("\n# ").next().unwrap();
        let proof_line_count = proof_section.lines().count();
        assert!(
            proof_line_count < 8,
            "Proof of work section grew to {} lines; keep it terse",
            proof_line_count
        );

        // Post-merge automation: each completed task lands on a green,
        // simplified, version-stamped main.
        for skill in ["/simplify", "/bump"] {
            assert!(
                p.contains(skill),
                "missing post-merge `{}` step in prompt",
                skill
            );
        }

        // Old-flow words that *must* be absent — the prompt rewrite is the
        // only place that referenced these, and leaving them in would
        // teach orchestrators verbs that no longer exist as CLI subcommands.
        for forbidden in [
            "merge-worktree",
            "reservations declare",
            "reservations upgrade",
            "reservations list",
            "reservations release",
            "blocked_by_active_orchestrator",
        ] {
            assert!(
                !p.contains(forbidden),
                "prompt still references removed concept `{}` — \
                 this is the PR-flow rewrite; reservations and \
                 merge-worktree are gone",
                forbidden
            );
        }

        // The PR-reopen verb collapsed the iteration dance into one call. If the
        // prompt still teaches the old `pr comment` + `task report --status review`
        // workaround, the orchestrator will execute a two-step that no longer
        // matches the auto-review semantics.
        for forbidden in [
            format!("pr comment --task {} --author orchestrator", state.task_id),
            format!("task report --task {} --status review", state.task_id),
        ] {
            assert!(
                !p.contains(&forbidden),
                "prompt still references obsolete iteration step `{}` — \
                 use `pr reopen` instead",
                forbidden
            );
        }
    }
}
