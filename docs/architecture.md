# Architecture

How the source is laid out and how a key press becomes a change on disk. For
contributors; read the [README](../README.md) first for what cc-hub does.
Each module's `//!` doc says more than its entry here.

## Two crates and a broker

```text
bin/     cc-hub: the binary. Runtime, terminal, key dispatch, CLI verbs.
lib/     cc-hub-lib: state, scanners, rendering, platform shims.
broker/  resource_manager.py, the resource broker, bundled into the binary.
```

`bin/` owns everything that needs a terminal, tokio channels or the process.
`lib/` owns the state and draws it, so a test can build an `App`, run
commands against it and render it without a terminal. The broker is Python;
`cli::resource` embeds it with `include_str!` and runs it with `python3`.

Disk is the source of truth. Scanners read the agents' own files and
cc-hub's files under `~/.cc-hub/` into snapshots. Every mutation writes
through to disk, so the next scan re-derives the same state, and the CLI and
the TUI see each other's changes.

## Runtime flow

1. `main` strips `--claude-config-dir` (`args::extract_claude_config_dir`).
   `cli::dispatch` runs a verb and exits. `--no-tui` prints sessions
   (`run_no_tui`). Otherwise `term::enter` sets up the terminal.
2. `event_loop::run` builds the `App` and starts the background workers in
   `workers`: `spawn_usage`, `spawn_harness` (the persistent-agent
   supervisor), `spawn_builds`, `spawn_session_scanner` (file watcher,
   liveness fallback timer, full rescan every 10 s) and `spawn_task_stats`.
   Each worker reports through a `ScanMsg`.
3. Each loop pass draws with `ui::render`, then `input::drain_input` waits
   5 ms (16 ms while the embedded pane shows) and handles the input burst.
4. `keys::handle_key` maps a key to a lib `Command` (`keys::map_command`).
   `App::execute` applies it and returns `Effect`s that need bin-side
   machinery (a pane, a channel, the window manager), which
   `effects::apply_effect` runs. Keys no command covers go to the view's own
   handler in `bin/src/keys/`.
5. The loop drains `ScanMsg`s into `App` (`scan_msg::apply_scan_msg`), and
   `App::poll_pending_dispatch` sends queued prompts to sessions that have
   become idle.

## `lib/src` by directory

- `app/`: `App`, the view model. `view` holds the `View` and `Tab` enums;
  `command/` the per-tab command enums and executors; `sessions/` and
  `board/` the Sessions and Tasks tabs and their pickers; `*_view` the other
  tabs; `render_state` the layout the renderer writes back.
- `ui/`: rendering. `render` in `mod.rs` is the entry; one module per tab,
  `popups/` for overlays, `status_bar` for the key hints.
- `sessions/`: read-only discovery of Claude, Pi and Codex sessions.
  `scanner` merges them into `SessionInfo`s; `index` is the archive the
  session finder searches.
- `conversation/`: transcript parsing. `classify` is the one session-state
  machine; each dialect (Claude, `pi/`, `codex/`) adapts to it through
  `TranscriptDialect`.
- `tasks/`: the board. `store` owns each card's `state.json` and its lock;
  `status` holds the legal moves (`validate_status_transition`);
  `PersonalBoard` is the in-memory board.
- `ops/`: compound task and deep-link operations shared by the CLI and the
  TUI. Typed input, typed outcome, `OpError` for failures.
- `link/`: parses `cc-hub://` URLs and renders their prompts. Pure.
- `harness/`: persistent agents: spec, triggers, supervisor, one tick,
  state and notes.
- `builds/`: the build store, recipes, the runner (`cc-hub build _run`) and
  the resource hold.
- `metrics/`: cost and token analysis over every transcript.
- `spawn/`: builds an agent's command line and starts it in a detached
  multiplexer session.
- `platform/`: OS shims: `mux` (tmux or psmux), `process`, `window`,
  `paths`, `terminal`.
- `config/`: `config.toml`, loaded once through `config::get`.
- `title/`: background session titles.
- `tmux_pane/`: the embedded pane (portable-pty, vt100, tui-term).

Single-file modules: `agent` (backend registry: `AgentKind`, `AgentConfig`),
`agent_runtime` (the process-control trait tests replace), `send` (types a
prompt into a live pane), `respawn` (continue a session on another account),
`resources` (accounts from `resources.toml`), `usage` (the shared quota
store), `wake`, `focus`, `live_view`, `folder_picker`, `bookmarks`, `acks`,
`holds`, `persist` (atomic writes and locks), `models`, `clipboard`, `fuzzy`,
`gh`.

In `bin/src`: `cli/` has one file per verb, plus `error` (the JSON error
contract) and `help`; `keys/` has one file per view; `event_loop/`,
`workers`, `scan_msg`, `effects`, `titles`, `term`, `logging` and `args`
cover the runtime.

## Where state lives

| What | Where | Owner |
|---|---|---|
| Config | `~/.cc-hub/config.toml` | `config` |
| Task cards | `~/.cc-hub/tasks/<id>/state.json`, `board.json`, `tasks-archive-v2.json` | `tasks` |
| Session-to-task links | `~/.cc-hub/session-tasks.json` | `tasks::session_links` |
| Acks, holds, bookmarks | `~/.cc-hub/acks.json`, `holds.json`, `bookmarks.json` | `acks`, `holds`, `bookmarks` |
| Usage readings | `~/.cc-hub/usage.json` | `usage` |
| Persistent agents | `~/.cc-hub/agents/<name>/` | `harness` |
| Wakes | `~/.cc-hub/wake/<name>` | `wake` |
| Builds and holds | `~/.cc-hub/builds/<id>/`, `builds/holds/` | `builds` |
| Accounts, resources | `~/.cc-hub/resources.toml`; broker state in `~/.cc-hub/resources/` | broker, `resources` |
| Pi heartbeats | `~/.cc-hub/pi-heartbeats/<tmux>.json` | `sessions::pi_bridge` |
| Titles, logs | the cache dir: `session-titles.json`, `cc-hub_*.log` | `title`, `logging` |
| Agent transcripts | `~/.claude/`, `~/.pi/agent/sessions/`, `~/.codex/sessions/` | the agents (read-only) |

## Where to change what

- **A key.** Add a variant to the tab's enum in `lib/src/app/command/<tab>.rs`
  and handle it in `App::execute`; return an `Effect` for anything needing
  the terminal or window manager. Map the key in `bin/src/keys/<tab>.rs`,
  update the hint in `ui/status_bar.rs` and
  [keybindings.md](keybindings.md). Pin it with a command test inside
  `with_temp_home`: building an `App` touches the task store.
- **A view or popup.** Add a `View` variant in `app/view.rs`, draw it under
  `ui/` (overlays in `ui/popups/`), and handle its keys in `bin/src/keys/`.
- **A CLI verb.** Put the logic in `lib/src/ops/`, add a
  `bin/src/cli/<verb>.rs` that parses flags, calls the op and prints one JSON
  line, add an arm to `cli::dispatch` and a topic to `cli::help`.
- **A config key.** Add the field and its default in `lib/src/config/`, then
  document it in [configuration.md](configuration.md).
- **A task field.** Extend `TaskState` in `tasks/store.rs` with
  `#[serde(default)]`, so older `state.json` files still load.
- **An agent backend.** Add an `AgentKind` variant, teach
  `spawn::command::build_agent_command` its command line, add a scanner in
  `sessions/` and a dialect in `conversation/`.
- **A persistent-agent action.** Put it in `harness/` taking the agent dir,
  writing through `update_state`, so the supervisor and the CLI share it.
  Add a `HarnessCommand` for the key and an arm in `bin/src/cli/agent.rs`.
- **A background job.** Add a `spawn_*` in `bin/src/workers.rs`, a `ScanMsg`
  variant for its result, and apply it in `scan_msg::apply_scan_msg`.

## Tests

- Unit tests sit next to the code. Tests that redirect `$HOME` go through
  `test_util::with_temp_home`, which serialises them on `HOME_TEST_LOCK`.
- `lib/tests/` has tmux smoke tests; they skip without tmux.
- The broker: `python3 -m unittest discover -s broker/tests -p 'test_resource*.py'`.
