# cc-hub CLI reference

Reference for every verb of the `cc-hub` command line. Each section covers one
top-level verb: what it does, its subcommands, their flags and defaults, and
what they print.

Conventions used throughout:

- `ID` values are opaque strings. Task ids start with `t-` (project-born) or
  `tk-` (promoted from the personal board).
- `--project-id` is optional almost everywhere; it defaults to the id derived
  from the current directory, so running inside a project root is enough.
- Unless a verb is documented as printing plain text, it prints a single JSON
  object on stdout with `"ok": true`.
- Success exits 0. Bad flags, missing tasks, and failed operations print an
  error to stderr and exit non-zero.

---

## task

Manage tasks: create, run, inspect, report on, and tear down.

### task create

```
cc-hub task create --prompt TEXT [--project-id ID] [--name NAME] [--backlog]
```

Create a task without going through the TUI's `N → folder → prompt` flow.
Intended for tests and tooling.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--prompt` | text | required | The task prompt. |
| `--project-id` | id | cwd-derived | Project that owns the task. |
| `--name` | text | none | Display name for the task. |
| `--backlog` | — | off | Create in Backlog instead of the default starting status. |

Prints: `task_id`, `project_id`, `status`.

### task start

```
cc-hub task start --task ID [--project-id ID] [--agent ID] [--wait-secs N]
```

Flip a Backlog task to Running and spawn its orchestrator. Errors if the task
is not in Backlog. Mirrors `orchestrate start`.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to start. |
| `--project-id` | id | cwd-derived | Owning project. |
| `--agent` | id | project default | Agent to run as the orchestrator. |
| `--wait-secs` | seconds | agent default | How long to wait for the agent to accept the prompt. |

Prints: `agent_id`, `agent_kind`, `cwd`, `tmux`, `prompt_status`, `task_id`,
`project_id`. The envelope deliberately matches `spawn-worker`'s output shape.

A deferred prompt warns on stderr but still exits 0 with `ok: true`; the task is
Running either way. Read `prompt_status` to know whether the orchestrator got
its instructions.

### task list

```
cc-hub task list [--status STATUS] [--project-id ID] [--json]
```

Enumerate a project's tasks, newest update first. Reads task state from disk
directly, so unregistered/ad-hoc projects work.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--status` | `backlog`\|`planning`\|`running`\|`review`\|`merging`\|`done` | no filter | Keep only tasks in this status. Any other value is a usage error. |
| `--project-id` | id | cwd-derived | Project to list. |
| `--json` | — | off | Emit JSON instead of plain rows. |

Prints without `--json`: one tab-separated row per task —
`task_id`, `status`, title (or the prompt's first line, truncated to 60 chars
with `…`), short relative age.
Prints with `--json`: `tasks[]`, each with `task_id`, `status`, `title`,
`prompt`, `note`, `updated_at`, `shipped_version`.

Unreadable task dirs are skipped with a `warning:` line on stderr. A missing
tasks directory yields an empty list, not an error.

### task show

```
cc-hub task show --task ID [--project-id ID] [--json]
```

Read-only inspection of one task.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to show. |
| `--project-id` | id | cwd-derived | Owning project. |
| `--json` | — | off | Emit JSON instead of key/value lines. |

Prints without `--json`: `status`, `prompt` (first line, 80 chars),
`note`, `summary` (first line, 120 chars), `created_at` and `updated_at` as
relative ages, worker count, artifact count, `todos` as `done/total`,
`shipped_version`, `orchestrator_tmux`. Absent values render as `-`.
Prints with `--json`: the full task state at the top level, plus `ok` and a
`pr` field holding the PR object or `null`.

### task report

```
cc-hub task report --task ID [--project-id ID] [--status STATUS] [--note TEXT] [--summary TEXT]
```

Update a task's status, note, and summary — how an orchestrator reports
progress.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to update. |
| `--project-id` | id | cwd-derived | Owning project. |
| `--status` | `backlog`\|`planning`\|`running`\|`review`\|`merging`\|`done` | unchanged | Requested new status. Any other value is a usage error. |
| `--note` | text | unchanged | Short progress note. |
| `--summary` | text | unchanged | Result summary. |

Prints: `task_id`, `project_id`, `status` (what was actually applied),
`requested_status`, `note`, `summary`, `shipped_version`, `updated_at`.

Illegal transitions are rejected without mutating state; `backlog` → `running`
fails with a usage error pointing at `task start`. `status` and
`requested_status` can differ when the requested transition is adjusted.

### task delete

```
cc-hub task delete --task ID [--project-id ID] [--force]
```

End-to-end teardown: kill the orchestrator tmux session (best-effort), remove
every worktree the task owns, then delete its on-disk state directory.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to delete. |
| `--project-id` | id | cwd-derived | Owning project. |
| `--force` | — | off | Required for `Running`, `Review`, and `Merging` tasks. |

Prints: `task_id`, `project_id`, `orchestrator_killed`, `lock_released`,
`state_removed`, `worktrees_removed`, `worktree_errors[]` (each `path` +
`error`).

Deleting a `Merging` task under `--force` releases the project merge lock, so a
task wedged by a dead orchestrator does not block the project forever. A
refused delete leaves state untouched.

### task gc

```
cc-hub task gc [--project-id ID] [--dry-run]
```

Sweep orphaned worktrees under `<root>/.cc-hub-wt/`. Worktrees and their
`cc-hub/*` branches are otherwise torn down only on the Done path, so Review,
abandoned, and wedged tasks leak them. Removes every worktree no live
(present, non-Done) task owns, deletes the dangling branches, then runs
`git worktree prune`.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--project-id` | id | cwd-derived | Project to sweep. |
| `--dry-run` | — | off | Print the plan; change nothing. |

Prints: `project_id`, `dry_run`, `orphans[]` and `live[]` (each `dir_name`,
`path`, `branch`), `worktrees_removed`, `branches_removed`, `errors[]`,
`pruned`.

Takes no `--task`. The project root resolves from the registry, or from the
current directory when its derived id matches; otherwise the verb errors and
asks for a `--project-id` naming a registered project.

### task auto-review

```
cc-hub task auto-review --task ID [--project-id ID]
```

Re-arm the auto-reviewer for the current Review round by clearing
`last_auto_reviewed_at`, so the next auto-review tick picks the task again.
Use it after fixing a misconfiguration instead of waiting for a fresh round.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to re-arm. |
| `--project-id` | id | cwd-derived | Owning project. |

Prints: `task_id`, `project_id`, `cleared`.

### task artifact add

```
cc-hub task artifact add --task ID --path PATH [--project-id ID] [--kind KIND] [--caption TEXT] [--lead]
```

Attach an artifact (file produced by the task) to the task record.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to attach to. |
| `--path` | path | required | Artifact source path. |
| `--project-id` | id | cwd-derived | Owning project. |
| `--kind` | text | inferred | Artifact kind. |
| `--caption` | text | none | Caption shown with the artifact. |
| `--lead` | — | off | Mark this artifact as the task's lead artifact. |

Prints: `task_id`, `project_id`, `artifact` (`kind`, `path`, `original`,
`caption`, `added_at`, `lead`), `count`, `lead_index`.

### task artifact list

```
cc-hub task artifact list --task ID [--project-id ID]
```

List a task's artifacts.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to read. |
| `--project-id` | id | cwd-derived | Owning project. |

Prints: `artifacts[]`, each with `kind`, `path`, `original`, `caption`,
`added_at`, `lead`.

### task todos set

```
cc-hub task todos set --task ID --items TEXT [--project-id ID]
```

Replace the task's todo list wholesale.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to update. |
| `--items` | newline-separated text | required | One todo per line. Replaces the existing list. |
| `--project-id` | id | cwd-derived | Owning project. |

Prints: `task_id`, `project_id`, `todos[]` (each `text`, `done`).

### task todos check / task todos uncheck

```
cc-hub task todos check   --task ID --index N [--project-id ID]
cc-hub task todos uncheck --task ID --index N [--project-id ID]
```

Mark one todo done (`check`) or not done (`uncheck`).

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to update. |
| `--index` | 0-based integer | required | Todo position in the list. |
| `--project-id` | id | cwd-derived | Owning project. |

Prints: `task_id`, `project_id`, `todos[]` (each `text`, `done`).

### task todos clear

```
cc-hub task todos clear --task ID [--project-id ID]
```

Remove every todo from the task.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to clear. |
| `--project-id` | id | cwd-derived | Owning project. |

Prints: `task_id`, `project_id`, `todos[]` (empty).

---

## agent

Persistent agents: one directory under `~/.cc-hub/agents/<name>/` with an
`agent.toml`. The TUI supervises enabled agents; these verbs scaffold,
drive, and inspect them. The agent name is given positionally, via
`--agent NAME`, or, inside a tick, by the `CC_HUB_AGENT` environment
variable the runner sets.

### agent list

```
cc-hub agent list [--json]
```

Prints one tab-separated row per agent: name, status
(`sleeping`|`ticking`|`halted`|`paused`|`disabled`|`broken`), trigger,
tick count, today's and lifetime spend, then the halt reason or spec error
if any. `--json` emits `{ok, root, agents: [...]}` with the same fields plus
`inbox_pending`, `last_result`, and `notes`.

### agent new

```
cc-hub agent new NAME [--from DIR]
```

Creates `~/.cc-hub/agents/NAME/` with `work/`, `inbox/`, and either the
built-in template `agent.toml` or a copy of `DIR` (see `contrib/agents/`).
Errors if the directory exists or the name has characters outside
`[A-Za-z0-9_-]`. Prints `name`, `dir`, `spec`.

### agent once

```
cc-hub agent once NAME [--event TEXT | --event-file PATH] [--force]
```

Runs one tick synchronously and records it like the supervisor would.
Ignores `enabled`, pause, and halt state (it exists to iterate on a spec),
but honours the daily/total budget unless `--force`. Prints `ok`, `tick`,
`subtype`, `turns`, `compactions`, `cost_usd`, `context_start`,
`context_end`, `duration_s`, `session_id`, `result`, `log`. A failed tick
exits 1 after printing the same line.

### agent poke

```
cc-hub agent poke NAME [--event TEXT | --event-file PATH]
```

Drops an event file into the agent's `inbox/`. Every trigger kind checks the
inbox first, so this wakes any running agent on its next loop pass. Prints
`event` (the file name, which becomes the event id) and `inbox`.

### agent pause / resume / reset

```
cc-hub agent pause NAME
cc-hub agent resume NAME
cc-hub agent reset NAME
```

`pause` makes the supervisor skip the agent; `resume` lifts that and also
clears a budget/failure halt. `reset` deletes `state.json` (ticks, spend,
history); the workdir, notes, and inbox are untouched.

### agent show

```
cc-hub agent show NAME
```

The `list --json` fields for one agent plus `history` (the last 50 ticks)
and `recent_notes`.

### agent note

```
cc-hub agent note --text TEXT [--level info|warn] [--ref URL]
```

Agent-facing. Appends a line to `notes.jsonl`; the Agents tab shows the
newest note on the card and the last twenty in the detail popup. Needs
`CC_HUB_AGENT` (set inside a tick) or `--agent`.

---

## project

Inspect registered projects.

### project list

```
cc-hub project list [--json]
```

Enumerate the projects registered in `~/.cc-hub/projects.toml`, sorted by name
case-insensitively so the listing is stable across machines. Unregistered
projects never appear, even when they have tasks on disk.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--json` | — | off | Emit JSON instead of plain rows. |

Prints without `--json`: one tab-separated row per project — `id`, `name`,
`root`. Tabs, not spaces, so consumers can split names containing spaces.
Prints with `--json`: `projects[]`, each with `id`, `name`, `root`, and
`task_counts` (`backlog`, `running`, `review`, `merging`, `done`).

`task_counts` folds `planning` tasks into `running`; that status belongs to the
personal board and never appears in a project scan. Takes no `--project-id`.

---

## pr

Drive the local PR review and merge flow. Every verb takes `--task ID`
(required) and `--project-id ID` (cwd-derived); flag tables below list only
what is specific to each verb.

**Lifecycle.** The orchestrator opens a PR with `pr create`, the user reviews
in the TUI or from the CLI, and merging is serialized through the per-project
merge lock:

| Verb | Task status | PR review state |
|---|---|---|
| `pr create` | Running → Review | new, Open |
| `pr request-changes` | Review → Running | ChangesRequested |
| `pr reopen` | Running → Review | ChangesRequested → Open |
| `pr approve` | stays Review | Open\|ChangesRequested → Approved |
| `pr merge` | Review → Merging | unchanged; acquires `merge.lock` and holds it |
| `pr finalize` | Merging → Done | Merged; releases `merge.lock` |
| `pr close` | → Done | Closed; releases the lock if held |

**The PR object.** Every verb prints its result under a `pr` key with the same
shape: `id`, `task_id`, `project_id`, `branch`, `base`, `title`,
`description`, `review_state`, `comments[]`, `comments_total`,
`comments_returned`, `approved_at_branch_sha`, `approved_at_base_sha`,
`created_at`, `updated_at`. Sections below say "the PR object" rather than
repeating it.

**Failure envelopes.** `pr merge`, `pr continue`, and `pr finalize` can fail in
ways the caller is expected to act on. They still print one JSON object, but
with `"ok": false`, a `phase` or `reason` naming where it stopped, and a
`recipe` string spelling out the fix — then exit non-zero. Each such verb lists
its outcomes in an outcome table.

### pr create

```
cc-hub pr create --task ID --worktree NAME --title TEXT [--project-id ID] [--description TEXT]
```

Open a PR for a task's feature branch and move the task from Running to
Review.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--worktree` | name | required | Worktree holding the feature branch. |
| `--title` | text | required | PR title. |
| `--description` | text | empty | PR body. |

Prints: `pr` — the PR object, `review_state: "open"`.

### pr show

```
cc-hub pr show --task ID [--project-id ID] [--comments-since UNIX_SECS]
```

Read-only inspection, for the TUI and for scripting. Errors with "no PR for
this task" when the task has none.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--comments-since` | unix seconds | all comments | Return only comments added at or after this timestamp. |

Prints: `pr` — the PR object. `comments_total` always counts every comment;
`comments_returned` counts what the filter let through, so a caller polling
with `--comments-since` can still see the full size.

### pr approve

```
cc-hub pr approve --task ID [--project-id ID]
```

Approve the PR. The task stays in Review; approval records the branch and base
SHAs so a later merge can tell whether the approved code still matches.

Prints: `pr` — the PR object, `review_state: "approved"`.

### pr request-changes

```
cc-hub pr request-changes --task ID --comment TEXT [--project-id ID] [--author NAME]
```

Send the PR back for rework: review state becomes ChangesRequested and the
task returns to Running.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--comment` | text | required | Why changes are needed. Recorded on the PR. |
| `--author` | name | `user` | Comment author. |

Prints: `pr` — the PR object.

### pr reopen

```
cc-hub pr reopen --task ID [--project-id ID] [--comment TEXT] [--author NAME]
```

Re-submit after rework: review state goes ChangesRequested → Open and the task
returns to Review.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--comment` | text | none | Optional note recorded on the PR. |
| `--author` | name | `orchestrator` | Comment author. |

Prints: `pr` — the PR object.

### pr comment

```
cc-hub pr comment --task ID --comment TEXT [--project-id ID] [--author NAME]
```

Append a comment without changing the review state.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--comment` | text | required | Comment body. |
| `--author` | name | `orchestrator` | Comment author. |

Prints: `pr` — the PR object.

### pr merge

```
cc-hub pr merge --task ID [--project-id ID] [--wait] [--timeout-secs N]
```

Acquire the project merge lock, merge base into the feature branch, then the
branch into base, and move the task to Merging. The lock stays held after a
successful merge — `pr finalize` releases it.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--wait` | — | off | Block until the merge lock frees instead of failing immediately. |
| `--timeout-secs` | seconds | no timeout | Cap how long `--wait` blocks. |

Outcomes:

| Outcome | `ok` | Key fields |
|---|---|---|
| merged | true | `phase: "merged"`, `branch`, `base`, `stdout`, `restored_ref`, `next` |
| lock held | false | `locked: true`, `holder_task`, `since`, `phase`, `age_seconds`, plus `timed_out: true` when `--wait` expired |
| worktree gone | false | `phase: "preflight"`, `kind: "missing_worktree"`, `worktree` |
| worktree dirty | false | `phase: "preflight"`, `kind: "dirty_worktree"`, `worktree`, `dirty[]` |
| target tree dirty | false | `phase: "preflight"`, `blocked_by_dirty_tree: true`, `overlap[]` |
| conflict, base → branch | false | `phase: "merge_main_into_branch"`, `demoted_to: "open"`, `conflicting_paths[]`, `stdout`, `stderr` |
| conflict, branch → base | false | `phase: "merge_branch_into_main"`, `conflicting_paths[]`, `stdout`, `stderr` |

Every failure outcome except "lock held" releases the merge lock before
returning, and says so in its `recipe`. A conflict merging base into the
branch demotes the PR back to Open, so it must be re-approved before another
merge attempt. A conflict merging into base should be impossible under the
lock; its recipe says to investigate rather than retry.

### pr lock-phase

```
cc-hub pr lock-phase --task ID --phase PHASE [--project-id ID]
```

Update the phase recorded on the held merge lock, so other tasks polling the
lock can see how far the holder has got.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--phase` | `merging`\|`simplify`\|`bump`\|`finalize-pending` | required | New phase. Any other value is a usage error. |

Prints: `task_id`, `project_id`, `phase`.

### pr finalize

```
cc-hub pr finalize --task ID [--project-id ID] [--build-cmd CMD] [--skip-build] [--keep-tmux]
```

Close out a merged PR: run the build gate, release the merge lock, then mark
the PR Merged and the task Done. The lock is released *before* the terminal
flip — if the release fails the task stays in Merging so a re-run can finish,
rather than stranding a Done task as lock holder.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--build-cmd` | command | project `build_cmd` | Build gate to run on the merged base. |
| `--skip-build` | — | off | Skip the build gate. |
| `--keep-tmux` | — | off | Leave the task's tmux sessions running instead of tearing them down. |

Outcomes:

| Outcome | `ok` | Key fields |
|---|---|---|
| finalized | true | `released`, `task_id`, `status: "done"`, `build_skipped`, `tmux_kept` |
| already merged | true | `noop: true`, `task_id`, `status: "done"`, `reason` |
| build failed | false | `phase: "build"`, `command`, `stderr` (tail) |

A failed build leaves the task in Merging with the lock still held; fix, commit
on the base branch, and re-run.

### pr continue

```
cc-hub pr continue --task ID [--project-id ID]
```

Re-ping the task's orchestrator with the merge-flow prompt the TUI sends on
approval. Recovers the case where a PR was approved but the orchestrator never
picked up the merge — it was busy, the notification was dropped, or the session
restarted. Idempotent: re-running only re-pings.

Outcomes:

| Outcome | `ok` | Key fields |
|---|---|---|
| prompt sent | true | `orchestrator_tmux`, `orchestrator_alive: true`, `sent: true` |
| pane busy | true | `orchestrator_tmux`, `orchestrator_alive: true`, `sent: false`, `pane_busy: true` |
| no tmux recorded | false | `orchestrator_alive: false`, `reason: "no_orchestrator_tmux"` |
| session dead | false | `orchestrator_tmux`, `orchestrator_alive: false`, `reason: "orchestrator_dead"` |

"Pane busy" exits 0 — the orchestrator is healthy, just mid-turn; retry once
idle. The dead-session recipes point at `orchestrate start` to resurrect, or
`task delete --force` to tear down a wedged merge and release the lock.

### pr close

```
cc-hub pr close --task ID [--project-id ID] [--comment TEXT] [--author NAME]
```

Abandon a PR without deleting anything: review state becomes Closed, the task
goes Done, the merge lock drops if held, and sessions are torn down. The review
record — comments and history — survives, unlike `task delete`.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--comment` | text | none | Optional closing note recorded on the PR. |
| `--author` | name | `user` | Comment author. |

Prints: `pr` — the PR object, plus `status: "done"`.

---

## worker

Coordinate a task's worker sessions.

### worker wait

```
cc-hub worker wait --task ID [--project-id ID] [--tmux NAME ...] [--worktree NAME ...] [--all] [--timeout-secs N] [--progress] [--progress-interval-secs N]
```

Block until the selected worker tmux sessions stop needing the orchestrator's
attention — Claude ended its turn (WaitingForInput), the agent is blocked on a
question (Question), or the process is gone (Inactive). Replaces the
orchestrator's capture-pane polling loop; this polls every 500 ms and returns
in seconds.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task owning the workers. |
| `--project-id` | id | cwd-derived | Owning project. |
| `--tmux` | name, repeatable | — | Wait on this tmux session. Must be a worker of the task. |
| `--worktree` | name, repeatable | — | Wait on the worker in this worktree. Must be a worker of the task. |
| `--all` | — | off | Wait on every worker of the task. |
| `--timeout-secs` | seconds | 1800 | Give up waiting after this long. |
| `--progress` | — | off | Emit progress events while waiting. |
| `--progress-interval-secs` | seconds | 5 | Seconds between progress events; values below 1 are raised to 1. Ignored without `--progress`. |

Selection is the union of `--tmux`, `--worktree`, and `--all`. Passing none of
the three is a usage error; naming a session or worktree the task does not own
is an error too.

Prints, with `--progress`, zero or more lines of `{"event": "progress",
"elapsed_secs", "pending", "done"}` before the result.
Prints, as the final object: `all_done`, `timed_out`, `elapsed_secs`, and
`workers` (per-session final states).

A timeout is not an error: the verb exits 0 with `timed_out: true` and
`all_done: false`, so the caller decides what to do. `--all` on a task with no
workers returns immediately with `all_done: true` and an empty `workers`.

---

## orchestrate

Run orchestrator sessions for existing tasks.

### orchestrate start

```
cc-hub orchestrate start --task ID [--project-id ID] [--agent ID] [--wait-secs N] [--dry-run]
```

Spawn the orchestrator backend in the project root, wait for the new session to
reach Idle, then dispatch the orchestrator prompt as its first user message.
Records the resulting tmux name in the task's `state.json`.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task to orchestrate. |
| `--project-id` | id | cwd-derived | Owning project. |
| `--agent` | id | project default | Agent backend to spawn. |
| `--wait-secs` | seconds | 60 | How long to wait for the session to reach Idle before dispatching the prompt. |
| `--dry-run` | — | off | Print the orchestrator prompt and exit; spawn nothing. |

Prints with `--dry-run`: the prompt as plain text, no JSON. Use it to check
prompt content without paying for a session.
Prints otherwise: `agent_id`, `agent_kind`, `tmux`, `cwd`, `prompt_status`,
`task_id`, `project_id`. A deferred prompt warns on stderr but still exits 0
with `ok: true`; read `prompt_status` to know whether the orchestrator got its
instructions.

Takes any task, whatever its status. Use `task start` instead when the task is
in Backlog and should move to Running — that verb refuses tasks in other
states, this one does not check.

---

## spawn-worker

```
cc-hub spawn-worker --task ID [--project-id ID] [--worktree NAME] [--readonly] [--prompt TEXT] [--agent ID] [--wait-secs N]
```

Spawn a worker session for a task — an agent alongside the orchestrator, either
read-only or bound to a worktree. Waits for the new session to come up, then
dispatches `--prompt` as its first user message. Leaves the task's status alone.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task the worker belongs to. |
| `--project-id` | id | cwd-derived | Owning project. |
| `--worktree` | name | none | Run the worker in this worktree. |
| `--readonly` | — | off | Spawn the worker read-only. |
| `--prompt` | text | none | First user message sent to the worker. |
| `--agent` | id | project default | Agent backend to spawn as the worker. |
| `--wait-secs` | seconds | agent default | How long to wait for the agent to accept the prompt. |

Prints: `agent_id`, `agent_kind`, `tmux`, `cwd`, `worktree`, `readonly`,
`prompt_status`, `task_id`, `project_id`. The envelope is `task start`'s plus
`worktree` and `readonly`, so one output shape covers both spawn paths.

An undelivered prompt is not an error. When the prompt is deferred rather than
accepted, the verb writes `warning: …` to stderr, records the outcome in
`prompt_status`, and still prints `ok: true` and exits 0 — read `prompt_status`,
not the exit code, to know whether the worker got its instructions. The `tmux`
and `worktree` values printed here are what `worker wait` selects on.

---

## merge-worktree

```
cc-hub merge-worktree --task ID --worktree NAME [--project-id ID]
```

Merge one worktree's branch into the project's main branch directly. Legacy
helper: it takes no merge lock and does not move the task, so `pr merge` is the
supported path for a task under review.

| Flag | Argument | Default | Meaning |
|---|---|---|---|
| `--task` | id | required | Task owning the worktree. |
| `--worktree` | name | required | Worktree whose branch is merged. Omitting it is a usage error. |
| `--project-id` | id | cwd-derived | Owning project. |

| Outcome | `ok` | Key fields |
|---|---|---|
| Merged | `true` | `worktree`, `branch`, `main`, `stdout`, `stderr` |
| Conflicts | `false` | `worktree`, `branch`, `main`, `stdout`, `stderr` |
| Blocked by a dirty tree | `false` | `blocked_by_dirty_tree`, `overlap` |

Every outcome prints `stdout` and `stderr` — the underlying git output, the only
detail on what actually happened. `overlap` lists the paths edited on both the
target branch and the working tree; only that envelope carries a `recipe`.
Conflicts are left in place to resolve in the worktree or on main.

Both failures exit non-zero after printing their JSON, and the message goes to
stderr on a separate line — one JSON line per call still holds, so piping to
`jq` is safe.
