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
  src/cli/          # `cc-hub board | build | open | agent | resource | usage | wake`
lib/                # cc-hub-lib — everything else, behind a stable API
  src/lib.rs        # module wiring
  src/*.rs          # state, scanners, UI, platform, agents, task store…
  src/platform/     # OS-specific shims (process, mux, window, paths, terminal)
  tests/            # tmux smoke tests
```

Why split? `bin/` owns the runtime (tokio, terminal, key dispatch); `lib/`
owns state and rendering, so tests can build an `App` and draw it without a
terminal. `bin/src/main.rs` draws through `cc_hub_lib::ui::render`.

## Runtime flow (TUI)

`bin/src/main.rs` drives everything that happens at runtime:

1. **Bootstrap** (`main` at `bin/src/main.rs:377`). Parse argv; if it's a CLI
   verb, hand off to `cli::dispatch` and exit. Otherwise enable raw mode +
   alt screen, push kitty keyboard flags + bracketed paste, install a panic
   hook that restores the terminal.
2. **Background workers** (`run` at `bin/src/main.rs:455`). Spawn tokio
   tasks for:
   - `usage::fetch_usage` on `[scan].usage_refresh_interval_secs`
   - `watcher::spawn_fs_watcher` (notify-debouncer on agent state), which
     invalidates the Sessions scan. Sessions also use cheap process-liveness
     fallback ticks plus a slower full recovery scan
   - on-demand `metrics::analyze_with_progress` when the user opens the
     Metrics tab
   - per-session `title::generate_title_blocking` workers, gated
     by a shared semaphore (`config.title.concurrency`)
3. **Event loop** (`bin/src/main.rs:619`). Each iteration: poll `LiveView`,
   reap exited tmux panes, toggle mouse capture if the embedded pane is
   visible, draw via `ui::render`, then `event::poll(50ms)` (16 ms when a
   tmux pane is foregrounded). Keys are matched against `(View, KeyCode)`
   tuples. Dominant feature handlers such as Tasks live under `bin/src/keys/`
   instead of adding workflow logic to the root dispatcher.
4. **State updates**. Channel messages (`ScanMsg::SessionList`,
   `Detail`, `Usage`, `Metrics`, `Harness`, …) are drained and applied to
   `App` via methods like `update_sessions` and `update_usage`.

The TUI runs entirely off these snapshots — disk is the source of truth, and
every keystroke that mutates state writes through to disk so the next scan
re-derives a fresh snapshot.

## Major modules

### Sessions and tasks

Sessions come from scanners over the agents' own files (`~/.claude/sessions`,
`~/.pi/agent/sessions`, `~/.codex/sessions`, JSONL transcripts) and land in
`App` (`lib/src/app/`) as `Vec<SessionInfo>` (`lib/src/models.rs`). `App`
groups per-tab state into `SessionsView` / `TasksView` / `MetricsView` /
`HarnessView` sub-structs — cursor mutations go through their clamping
methods.

A Tasks-board card is a `task_store::TaskState`, stored per task at
`~/.cc-hub/tasks/<tid>/state.json` behind a per-task lock and atomic writes,
with every status change gated by `task_store::validate_status_transition`.
`PersonalBoard` in `lib/src/tasks.rs` is the in-memory snapshot the TUI
mutates through; board-level metadata lives in `~/.cc-hub/board.json`.
Malformed files are surfaced to the Tasks status bar instead of silently
overwriting or resetting state.

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
- **`conversation/`** — JSONL parsing + state classification. Reads a
  growing tail until at least one assistant entry is in window
  (`read_jsonl_tail_for_state` in `lib/src/conversation/io.rs`), then
  extracts the current state (`Processing | WaitingForInput | Question |
  Idle | Inactive`) and
  decorates `SessionInfo` with last user message, current tool, model,
  context tokens. The state *semantics* live once in
  `conversation/classify.rs`: both backends implement the
  `TranscriptDialect` format adapter and run the same shared `classify`
  decision procedure, pinned by a cross-backend parity test matrix.
  Unknown stop reasons and malformed JSONL lines warn once per file
  instead of silently misclassifying.
- **`models.rs`** — `SessionInfo`, `SessionState`, `SessionDetail`,
  `ProjectGroup`, plus `short_sid` truncation.
- **`title.rs`** — background `claude -p` (Haiku) titler. Concurrency-gated
  by a semaphore. Persists results onto `SessionInfo.title`. Cooldowns
  prevent re-titling.

### Tasks

- **`task_store.rs`** — schema + on-disk helpers for board cards:
  `TaskState`, `TaskStatus` (`Backlog → Planning → Running → Review →
  Done`), `Artifact` (notes and attachments). `read_task_state` /
  `write_task_state` (tempfile + rename) and `update_task(tid, |s| ...)`
  for locked read-mutate-write. Task id format: `tk-<unix-nanos>`.
- **`tasks.rs`** — `PersonalBoard`, the board snapshot the TUI mutates.
- **`ops/`** — the single implementation of compound task and link
  operations (attach a note, `cc-hub open`). Typed parameters in, typed
  outcome out; `OpError` maps 1:1 onto the CLI's `CliError`. Both
  `bin/src/cli/` and the TUI (`app/`) call these, so the two front-ends
  cannot drift.

- **`harness/`** — persistent agents (the Agents tab). `spec.rs` loads
  `~/.cc-hub/agents/<name>/agent.toml`; `tools.rs` turns a tool allow-list
  into the deny-everything-else rules that keep the prompt prefix small;
  `trigger.rs` is the at-least-once inbox plus poll/interval sources;
  `wake.rs` (top level) is the other half of the pacing: a spec's
  `trigger.wake` names files under `~/.cc-hub/wake/` that make the next poll
  run within a second, which is how a card moved to In Progress reaches the
  task router without waiting out its interval;
  `runner.rs` builds the `claude -p` argv and folds stream-json into a
  `Tick`; `supervisor.rs` is the per-agent tokio loop spawned from
  `main.rs::run`; `settings.rs` is the Settings section's in-place,
  comment-preserving `agent.toml` editor; `mod.rs` owns `state.json`,
  `notes.jsonl`, the `events.jsonl` harness log, the snapshot the TUI
  renders, and `tick_once` (shared with `cc-hub agent once`).
  Not to be confused with `agent.rs`, the backend registry.

### Builds

- **`builds/`** — the Builds tab's domain. `mod.rs` is the store
  (`~/.cc-hub/builds/<id>/build.json` + `output.log`, locked updates, and
  `all()`, which fails a build whose runner died) plus the asks: `start`
  (which prunes a recipe to its last twenty finished builds), `run` (the
  checkout as it is now), `cancel` (a flag). `recipe.rs` reads
  `[builds.recipes]` and expands its argv templates. `runner.rs` is
  `cc-hub build _run`: the only code that moves a build forward, one recipe
  step after another. `hold.rs` is
  the resource claim that outlives builds: a detached `cc-hub build _hold`
  that claims as the broker guest `Builds` and lives until released.
  `app/builds_view.rs` holds the tab's state — a card per recipe, and which
  of its builds the card shows (`shown`) — and `ui/builds.rs` draws it.

### Spawning + dispatching

- **`agent.rs`** — `AgentKind` (`Claude | Pi`) and `AgentConfig` (resolved
  from `[agents.*]` in config). Determines whether `--resume`, initial
  prompts, and the Pi heartbeat bridge apply.
- **`agent_runtime.rs`** — application-facing process-control interface.
  Sessions/Tasks controllers depend on `AgentRuntime`; production delegates to
  `spawn.rs`/`send.rs`, while tests inject a recording runtime.
- **`spawn.rs`** — `spawn_agent_session(agent_id, cwd, resume,
  initial_prompt, readonly)` builds the agent command and hands it to
  `platform::mux::spawn_detached`. Returns the new tmux session name.
  Claude sessions go through `ensure_path_trusted` first (writes to
  Claude's per-cwd trust store).
- **`respawn.rs`** — continuation planning for `R` on the Sessions grid:
  decides whether a session moving to another subscription account resumes
  natively (Claude→Claude, transcript carried into the target home) or
  hands off (fresh session pointed at the old transcript). Mirrors the
  resource broker's worker-replacement rules.
- **`send.rs`** — dispatches a prompt into a running agent. Walks the
  PID's ancestor chain to find the tmux pane, then `tmux send-keys`. Used
  for the "queued prompt" delivered after a fresh session reaches Idle.
- **`platform/mux.rs`** — single CLI shim that calls `tmux` (or psmux's
  `tmux.exe` on Windows). Module docstring (`lib/src/platform/mux.rs:1`)
  explains the two real divergences: Windows can't take an initial
  command in `new-session` and ignores `list-clients -F` format strings.

### CLI subcommands

`bin/src/cli/` holds one module per top-level verb (`board.rs`, `link.rs`,
`agent.rs`, …) with dispatch, `Flags` parsing, and the `CliError` JSON
contract in `mod.rs`. Argument parsing is hand-rolled; compound verb bodies
live in `lib/src/ops/` — the CLI modules parse flags, call the op, and
render one JSON line so a calling agent can parse the outcome. `cc-hub help
<verb>` documents each verb.

### TUI rendering

- **`ui/`** — `render(frame, app)` entry in `ui/mod.rs` (the function
  `bin/main.rs` draws through), dispatched on `app.view`
  and `app.current_tab` into `sessions.rs` (grid + cards + detail popup),
  `tasks.rs` (board + Task Info popup), `agents.rs`, `metrics.rs`, and
  `popups.rs` (pickers, inputs, live tail, embedded tmux pane). Shared helpers live in `common.rs`,
  named colors in `palette.rs`. Reads from `App`; the only render-time
  writes are scroll clamping and the documented renderer-synced metrics
  fields.
- **`tmux_pane.rs`** — embeds a tmux session as an interactive pane via
  portable-pty + vt100 + tui-term. Auto-replies to psmux's DSR query so
  Windows attach doesn't deadlock (`lib/src/tmux_pane.rs:11`).
- **`live_view.rs`** — incremental-tail JSONL viewer for the LiveTail
  popup. Polls only while visible.
- **`folder_picker.rs`** — places / bookmarks / browse picker for the task-assign and spawn-in-cwd flows.
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
| Per-task state | `~/.cc-hub/tasks/<tid>/state.json` (+ `board.json`, `tasks-archive-v2.json`) | `task_store`, `tasks::PersonalBoard` |
| Builds, their output and holds | `~/.cc-hub/builds/<id>/{build.json,output.log}`, `~/.cc-hub/builds/holds/<resource>.json` | `lib/src/builds/` |
| Persistent agent spec / state | `~/.cc-hub/agents/<name>/{agent.toml,state.json,notes.jsonl,inbox/,log/,work/}` | `lib/src/harness/` (`state.lock` guards `state.json`) |
| Pi bridge heartbeats | `~/.cc-hub/pi-heartbeats/<sid>.json` | `lib/src/pi_bridge.rs` |
| Claude sessions | `~/.claude/sessions/*.json` | (Claude Code, read-only) |
| Claude transcripts | `~/.claude/projects/<encoded-cwd>/<sid>.jsonl` | (Claude Code, read-only) |
| Pi sessions | `~/.pi/agent/sessions/*` | (Pi, read-only) |
| Logs | `$XDG_CACHE_HOME/cc-hub/cc-hub_*.log` (Linux) | `bin/src/main.rs:init_logging` |

## Key integration points (for changes)

- **Adding a new agent backend.** Implement an `AgentKind` variant in
  `lib/src/agent.rs`, teach `spawn::build_agent_command` how to construct
  its command, and (if it has its own JSONL layout) add a scanner like
  `pi_scanner.rs` and wire it into `scanner::scan_sessions`. The TUI
  renders backends generically via `agent_badge()`.
- **Adding a new CLI verb.** Put the body in `lib/src/ops/` (typed
  parameters in, typed outcome out, mutate state via
  `task_store::update_task`), then add a branch to
  `cli::dispatch` (`bin/src/cli/mod.rs:35`) with a thin parse → call →
  print-JSON wrapper in the matching `bin/src/cli/<verb>.rs` module.
- **Adding a new task field.** Extend `TaskState` in
  `lib/src/task_store.rs` with `#[serde(default)]` for back-compat
  with older `state.json` files; `read_task_state` returns `InvalidData`
  on parse errors so schema drift is loud.
- **Adding a new view / popup.** Add a `View` variant in
  `lib/src/app/`, render it in the matching `lib/src/ui/` module, and
  add the keybind branches in the `(View, KeyCode)` match in
  `bin/src/keys.rs`.
- **Adding a new key action (Sessions/Tasks).** Add a variant to the
  matching enum in `lib/src/app/command.rs`, implement it in
  `App::execute` (state mutation + status there; anything needing the
  terminal, run()'s channels, or the window manager is returned as an
  `Effect`, interpreted in `bin/src/effects.rs`), map the key in
  `map_command` in `bin/src/keys.rs`, and pin the behavior with a command
  test (always inside `with_temp_home` — constructing an `App` touches the
  on-disk task store). Renderer-computed layout state lives on
  `App::render` (`lib/src/app/render_state.rs`): `ui/` is its only
  render-time writer.
- **Adding a persistent-agent verb.** Body in `lib/src/harness/mod.rs`
  (it must work from both the TUI supervisor and the CLI, so it takes the
  agent dir and writes through `update_state`), a `HarnessCommand` variant
  in `lib/src/app/command.rs` for the key, and a thin arm in
  `bin/src/cli/agent.rs`. Anything the agent itself calls from a tick must
  resolve the agent from `CC_HUB_AGENT`.
- **Adding a new background tick.** Spawn a tokio task in
  `bin/src/main.rs:run`; emit a `ScanMsg` variant for results; drain it in
  the same big `select!`. The fs-watcher fallback timer is the reference
  pattern.

## Tests

`lib/tests/`:

- `mux_smoke.rs`, `pane_smoke.rs` — smoke-test that the multiplexer is
  callable and that an embedded pane can be spawned (skipped when tmux
  isn't available).

Several `lib/src/*.rs` modules also have `#[cfg(test)]` blocks with unit
tests; the `test_util::HOME_TEST_LOCK` mutex in `lib/src/lib.rs:23` exists
because some tests redirect `$HOME` and would otherwise race.
