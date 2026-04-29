# cc-hub codebase tour

A pointer-rich orientation for someone new to the codebase. The
[README](../README.md) covers what cc-hub is for and how to run it; this doc
covers how the source is laid out and where to look when changing it.

## Repo layout

cc-hub is a single Cargo workspace with two crates:

```
Cargo.toml          # workspace root; pins ratatui/crossterm/tokio/chrono
bin/                # cc-hub binary — TUI driver + CLI subcommands
  src/main.rs       # tokio runtime, terminal setup, scan/event loop
  src/cli.rs        # `cc-hub spawn-worker | merge-worktree | task | orchestrate | pr`
lib/                # cc-hub-lib — everything else, behind a stable API
  src/lib.rs        # module wiring + #[no_mangle] render() for hot-reload
  src/*.rs          # state, scanners, UI, platform, agents, orchestrator…
  src/platform/     # OS-specific shims (process, mux, window, paths, terminal)
  tests/            # integration tests for orchestrator git ops + tmux pane
```

Why split? `bin/` owns the runtime; `lib/` is rebuildable as a `cdylib` so
`cargo run --features hot-reload` can swap rendering code without restarting
the TUI. `bin/src/main.rs` calls `hot::render(...)` which routes to
`cc_hub_lib::render` — see `lib/src/lib.rs:46`.

## Runtime flow (TUI)

`bin/src/main.rs` is the orchestrator of everything that happens at runtime:

1. **Bootstrap** (`main` at `bin/src/main.rs:377`). Parse argv; if it's a CLI
   verb, hand off to `cli::dispatch` and exit. Otherwise enable raw mode +
   alt screen, push kitty keyboard flags + bracketed paste, query the image
   picker for cell size, install a panic hook that restores the terminal.
2. **Background workers** (`run` at `bin/src/main.rs:455`). Spawn tokio
   tasks for:
   - `usage::fetch_usage` on `[scan].usage_refresh_interval_secs`
   - `triage::tick` on `[backlog].interval_secs` (gated by config)
   - `watcher::spawn_fs_watcher` (notify-debouncer on `~/.claude`,
     `~/.pi`, `~/.cc-hub/pi-heartbeats`) plus an interval fallback that
     fires `scanner::scan_sessions` + `projects_scan::scan` per tick
   - on-demand `metrics::analyze_with_progress` when the user opens the
     Metrics tab
   - per-session/per-task `title::generate_title_blocking` workers, gated
     by a shared semaphore (`config.title.concurrency`)
3. **Event loop** (`bin/src/main.rs:619`). Each iteration: poll `LiveView`,
   reap exited tmux panes, toggle mouse capture if the embedded pane is
   visible, draw via `hot::render`, then `event::poll(50ms)` (16 ms when a
   tmux pane is foregrounded). Keys are matched against `(View, KeyCode)`
   tuples — most of the file is this match.
4. **State updates**. Channel messages (`ScanMsg::SessionList`,
   `Detail`, `Projects`, `Usage`, `Metrics`, `BacklogTriage`, …) are
   drained and applied to `App` via methods like `update_sessions`,
   `update_projects`, `set_usage`.

The TUI runs entirely off these snapshots — disk is the source of truth, and
every keystroke that mutates state writes through to disk so the next scan
re-derives a fresh snapshot.

## Major modules

### Two layers of state

cc-hub has two coexisting layers, each with its own scanner and snapshot:

| Layer | Owner of truth | Scanner | Snapshot type |
|---|---|---|---|
| **Sessions** (one agent process at a time) | `~/.claude/sessions`, `~/.pi/agent/sessions`, JSONL transcripts | `lib/src/scanner.rs`, `lib/src/pi_scanner.rs` | `Vec<SessionInfo>` (`lib/src/models.rs`) |
| **Projects/Tasks** (orchestrator + workers) | `~/.cc-hub/projects.toml`, `~/.cc-hub/projects/<pid>/tasks/<tid>/state.json` | `lib/src/projects_scan.rs` | `ProjectsSnapshot` |

Both feed into `App` (`lib/src/app.rs:117`). The Sessions tab shows raw
agent processes; the Projects tab is the higher-level kanban over tasks.
A session can be cross-referenced as an "orchestrator" or "worker" by
matching its tmux name against `ProjectsSnapshot::roles_by_tmux`.

### Sessions layer

- **`scanner.rs`** — walks `~/.claude/sessions/*.json` (Claude's own session
  index), pairs each with its JSONL transcript under
  `~/.claude/projects/<encoded-cwd>/<sid>.jsonl`, and probes process
  liveness via `platform::process`. Emits `SessionInfo`. Inactive sessions
  (no live PID) are kept for `[inactive].window_secs` so the user can
  resume.
- **`pi_scanner.rs`** — the equivalent for Pi sessions in
  `~/.pi/agent/sessions`. `pi_bridge.rs` writes/reads heartbeats so cc-hub
  can detect a live Pi session whose PID it doesn't own.
- **`conversation.rs`** — JSONL parsing + state classification. Reads a
  growing tail until at least one assistant entry is in window
  (`read_jsonl_tail_for_state` at `lib/src/conversation.rs:17`), then
  extracts the current state (`Processing | WaitingForInput | Idle`) and
  decorates `SessionInfo` with last user message, current tool, model,
  context tokens.
- **`models.rs`** — `SessionInfo`, `SessionState`, `SessionDetail`,
  `ProjectGroup`, plus `short_sid` truncation.
- **`title.rs`** — background `claude -p` (Haiku) titler. Concurrency-gated
  by a semaphore. Persists results onto `SessionInfo.title` (sessions) or
  `TaskState.title` (tasks). Cooldowns prevent re-titling.

### Projects/Tasks layer

- **`orchestrator.rs`** — schema + on-disk helpers for the Projects layer.
  See the module docstring at `lib/src/orchestrator.rs:1`. Owns:
  - `Project` (registered directory) + `TaskState` (one task)
  - `TaskStatus`: `Backlog → Running → Review → Merging → Done | Failed`
  - `Worker`, `Artifact`, `TodoItem`, `MergeRecord`
  - File layout: `~/.cc-hub/projects.toml` (registry) and
    `~/.cc-hub/projects/<pid>/tasks/<tid>/state.json` (per-task)
  - Atomic IO: `read_task_state` / `write_task_state` (tempfile + rename)
    and `update_task_state(pid, tid, |s| ...)` for read-mutate-write
  - Project id derivation: canonical-path, non-alphanumeric → dashes
  - Task id format: `t-<unix-nanos>`
  - Worktree convention: `<project-root>/.cc-hub-wt/<task-id>-<name>`
- **`projects_scan.rs`** — process-global mtime cache over every task's
  `state.json`. Each scan tick stat()s every file, only re-parses on
  mtime change, and evicts entries no longer on disk. Builds
  `ProjectsSnapshot::roles_by_tmux` so Sessions can look up "is this
  tmux session an orchestrator/worker?" in O(1).
- **`pr.rs`** — PR schema (`pr.json` next to `state.json`). Sequential
  per-project counter at `~/.cc-hub/projects/<pid>/pr-counter`. Review
  states: `Open → ChangesRequested → Approved → Merged | Closed`.
- **`merge_lock.rs`** — project-level lockfile so only one task can be in
  `Merging` per project at a time. fs2 advisory lock + `MergeLock`
  metadata file with the holding task id.
- **`triage.rs`** — optional background backlog promoter. Runs `claude -p`
  on each dormant backlog task and decides whether it's ready to be
  promoted to Running. Off by default.

### Spawning + dispatching

- **`agent.rs`** — `AgentKind` (`Claude | Pi`) and `AgentConfig` (resolved
  from `[agents.*]` in config). Determines whether `--resume`, initial
  prompts, and the Pi heartbeat bridge apply.
- **`spawn.rs`** — `spawn_agent_session(agent_id, cwd, resume,
  initial_prompt, readonly)` builds the agent command and hands it to
  `platform::mux::spawn_detached`. Returns the new tmux session name.
  Claude sessions go through `ensure_path_trusted` first (writes to
  Claude's per-cwd trust store).
- **`send.rs`** — dispatches a prompt into a running agent. Walks the
  PID's ancestor chain to find the tmux pane, then `tmux send-keys`. Used
  for the Sessions-tab `p` prompt and for the orchestrator's "queued
  prompt" delivered after a fresh session reaches Idle.
- **`platform/mux.rs`** — single CLI shim that calls `tmux` (or psmux's
  `tmux.exe` on Windows). Module docstring (`lib/src/platform/mux.rs:1`)
  explains the two real divergences: Windows can't take an initial
  command in `new-session` and ignores `list-clients -F` format strings.

### CLI subcommands (orchestrator-facing)

`bin/src/cli.rs` implements verbs the orchestrator session calls from a
shell to mutate task state. Argument parsing is hand-rolled. Each verb
emits one JSON line so the orchestrator can parse the outcome
programmatically.

| Verb | Purpose |
|---|---|
| `cc-hub task create --prompt "…"` | Register the task in the current project (creating it if needed), put it in Backlog |
| `cc-hub orchestrate start --task ID [--agent A]` | Spawn the orchestrator session; flips Backlog → Running |
| `cc-hub spawn-worker --task ID [--agent A] [--worktree NAME \| --readonly] [--prompt P]` | Spawn a worker session under the orchestrator. `--worktree` does `git -C <root> worktree add -b cc-hub/<branch> <path> main` |
| `cc-hub merge-worktree --task ID --worktree NAME` | Merge the worker's branch back into main; appends `MergeRecord` |
| `cc-hub task report --task ID [--status S] [--note N]` | Update the one-line note + optional status transition |
| `cc-hub task artifact add ...` / `cc-hub task todo ...` | Append evidence / mutate the task checklist |
| `cc-hub pr {create,merge,review,...}` | PR-flow CLI; pairs with `lib/src/pr.rs` schema |

The TUI never invokes these directly — it calls the same `orchestrator::*`
helpers in-process. The CLI exists so the orchestrator (a Claude or Pi
session running under bash) can drive the same state from its tools.

### TUI rendering

- **`ui.rs`** — single big `render(frame, app)` (≈4.7k LoC) dispatched on
  `app.view` and `app.current_tab`. Renders the three tabs (Projects,
  Sessions, Metrics), all popups (Detail, LiveTail, FolderPicker,
  PromptInput, ProjectsResult, Backlog, ConfirmClose, StateDebug,
  GhCreateInput), and the embedded tmux pane. Reads only from `App` and
  is the function `bin/main.rs` resolves through hot-reload.
- **`tmux_pane.rs`** — embeds a tmux session as an interactive pane via
  portable-pty + vt100 + tui-term. Auto-replies to psmux's DSR query so
  Windows attach doesn't deadlock (`lib/src/tmux_pane.rs:11`).
- **`live_view.rs`** — incremental-tail JSONL viewer for the LiveTail
  popup. Polls only while visible.
- **`folder_picker.rs`** — for `N` / register-project / spawn-in-cwd flows.
- **`focus.rs`** + **`platform/window.rs`** — window-manager shims for
  `f` (focus the OS window hosting a session) and `x` (close it).
  Hyprland via socket, X11 via `xdotool`, no-op elsewhere.
- **`metrics.rs`** — token + cost analytics across every JSONL on disk;
  feeds the Metrics tab.
- **`usage.rs`** — Anthropic usage API client; cache lives at
  `/tmp/claude-statusline-usage.json` (a cross-process contract with an
  external statusline helper — see README "Known limitations").

## Where state lives

| What | Where | Owner |
|---|---|---|
| Compiled config | `~/.cc-hub/config.toml` | `lib/src/config.rs` (loads once, deny-unknown) |
| Registered projects | `~/.cc-hub/projects.toml` | `orchestrator::ensure_project_registered` |
| Per-task state | `~/.cc-hub/projects/<pid>/tasks/<tid>/state.json` | `orchestrator::write_task_state` |
| Per-task PR | `~/.cc-hub/projects/<pid>/tasks/<tid>/pr.json` | `lib/src/pr.rs` |
| Per-project PR counter | `~/.cc-hub/projects/<pid>/pr-counter` | `lib/src/pr.rs` |
| Per-project merge lock | `~/.cc-hub/projects/<pid>/merge.lock` (+ `.json`) | `lib/src/merge_lock.rs` |
| Pi bridge heartbeats | `~/.cc-hub/pi-heartbeats/<sid>.json` | `lib/src/pi_bridge.rs` |
| Orchestrator log per task | `~/.cc-hub/projects/<pid>/tasks/<tid>/orchestrator.log` | `orchestrator::task_orchestrator_log_path` |
| Claude sessions | `~/.claude/sessions/*.json` | (Claude Code, read-only) |
| Claude transcripts | `~/.claude/projects/<encoded-cwd>/<sid>.jsonl` | (Claude Code, read-only) |
| Pi sessions | `~/.pi/agent/sessions/*` | (Pi, read-only) |
| Worktrees | `<project-root>/.cc-hub-wt/<task-id>-<name>` | git via `bin/src/cli.rs` |
| Logs | `$XDG_CACHE_HOME/cc-hub/cc-hub_*.log` (Linux) | `bin/src/main.rs:init_logging` |

`.cc-hub-wt/` is in the project's `.gitignore` so worktrees never get
committed to feature branches.

## Key integration points (for changes)

- **Adding a new agent backend.** Implement an `AgentKind` variant in
  `lib/src/agent.rs`, teach `spawn::build_agent_command` how to construct
  its command, and (if it has its own JSONL layout) add a scanner like
  `pi_scanner.rs` and wire it into `scanner::scan_sessions`. The TUI
  renders backends generically via `agent_badge()`.
- **Adding a new CLI verb.** Add a branch to `cli::dispatch`
  (`bin/src/cli.rs:37`) and a sibling helper. Verbs should mutate state
  via `orchestrator::update_task_state` and emit one JSON line on stdout.
- **Adding a new task field.** Extend `TaskState` in
  `lib/src/orchestrator.rs:209` with `#[serde(default)]` for back-compat
  with older `state.json` files; `read_task_state` returns `InvalidData`
  on parse errors so schema drift is loud.
- **Adding a new view / popup.** Add a `View` variant in
  `lib/src/app.rs:21`, render it in `lib/src/ui.rs`, and add the keybind
  branches in the `(View, KeyCode)` match in `bin/src/main.rs`.
- **Adding a new background tick.** Spawn a tokio task in
  `bin/src/main.rs:run`; emit a `ScanMsg` variant for results; drain it in
  the same big `select!`. The fs-watcher fallback timer is the reference
  pattern.
- **Hot-reload-safe code.** `lib/src/lib.rs` re-exports the `render`
  entry point with `#[no_mangle]`. Anything reachable from `render` will
  swap on rebuild; anything in `bin/` will not (the TUI process holds
  state across reloads, and `App` is in `lib/`).

## Tests

`lib/tests/`:

- `orchestrator_git.rs` — exercises `git worktree` ops via real git in a
  tempdir.
- `mux_smoke.rs`, `pane_smoke.rs` — smoke-test that the multiplexer is
  callable and that an embedded pane can be spawned (skipped when tmux
  isn't available).

Several `lib/src/*.rs` modules also have `#[cfg(test)]` blocks with unit
tests; the `test_util::HOME_TEST_LOCK` mutex in `lib/src/lib.rs:23` exists
because some tests redirect `$HOME` and would otherwise race.
