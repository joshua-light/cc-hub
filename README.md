# cc-hub

A TUI-based coding-agent hub. cc-hub can manage multiple backends: Claude
Code sessions from `~/.claude/...` and Pi sessions from `~/.pi/agent/sessions`.
It gives you a grid of every discovered session on the box: what state it's in
(processing / waiting / idle), the running tool or pending approval/question,
a live tool-call counter, context-window usage, and a live tail of the JSONL
transcript. From the grid you can:

- spawn new configured agent sessions in any folder,
- dispatch a prompt to the first idle agent (or auto-spawn one if none
  exist),
- embed an existing session's terminal pane inside the TUI,
- focus the real terminal window of a detached session (Unix only), and
- browse per-session metrics and Anthropic API usage.

A **Tasks** tab is the personal layer: a flat four-column board (**To-Do ·
Planning · In Progress · Done**, stored at `~/.cc-hub/tasks.json`) where you
jot tasks and either check them off by hand or hand one to an agent — `s`
picks a folder, spawns a detached agent session there prompted to
investigate the task and present a plan first, and binds it to the card,
which sits in **Planning**. `Space` approves the plan ("Proceed with the
implementation." is sent to the agent) and moves the card to In Progress.
`f`/`Enter` on a bound card attaches the agent's pane exactly like the
Sessions tab, including resume after the tmux session dies.

A separate **Projects** tab (WIP — hidden by default, enable with
`[ui] show_projects_tab = true`) adds a higher-level layer: register a
directory as a project, file a free-form *task* against it, and cc-hub
spawns an *orchestrator* session that decomposes the task and dispatches
*worker* sessions (read-only research workers, or worktree-isolated edit
workers) via scriptable CLI primitives:

- `cc-hub task create --prompt "…" [--backlog]` / `cc-hub task start --task ID [--agent AGENT]`
- `cc-hub orchestrate start --task ID [--agent AGENT] [--dry-run]`
- `cc-hub spawn-worker --task ID [--agent AGENT] [--worktree NAME | --readonly] [--prompt P]`
- `cc-hub worker wait --task ID [--tmux NAME ... | --worktree NAME ... | --all]`
- `cc-hub task report --task ID [--status S] [--note N] [--summary S]`
- `cc-hub task show --task ID [--json]` / `cc-hub task list [--status S] [--json]`
- `cc-hub task delete --task ID [--force]` (kills the orchestrator, removes worktrees + state)
- `cc-hub task gc [--project-id ID] [--dry-run]` (prune worktrees + branches no live task still owns)
- `cc-hub task auto-review --task ID` (re-arm the background auto-reviewer for the current Review round)
- `cc-hub task artifact add/list ...` and `cc-hub task todos set/check/uncheck/clear ...`
- `cc-hub pr create/show/approve/request-changes/reopen/comment/close/merge/lock-phase/continue/finalize ...` (`continue` re-pings a stuck orchestrator)
- `cc-hub project list [--json]`

Project state lives at `~/.cc-hub/projects.toml` and
`~/.cc-hub/projects/<id>/tasks/<id>/state.json`; worktrees are placed under
`<project-root>/.cc-hub-wt/` (add this to `.gitignore`).

## Requirements

| | Linux / macOS | Windows |
|---|---|---|
| Rust | 1.75+ (2021 edition) | 1.75+ (2021 edition) |
| Multiplexer | [`tmux`](https://github.com/tmux/tmux) on `PATH` | [`psmux`](https://github.com/psmux/psmux) on `PATH`, invoked as `tmux` |
| Claude Code | `claude` on `PATH` | `claude.exe` on `PATH` |
| Launch command | `cc-hub-new` resolvable in an interactive shell (alias/function in your rc) | `cc-hub-new` resolvable in PowerShell (function in `$PROFILE`) |
| Terminal font | Nerd Font (for state glyphs `󰒓 󰂞 󰒲`) | Nerd Font (for state glyphs `󰒓 󰂞 󰒲`) |
| Terminal emulator | one of `kitty`, `foot`, `alacritty`, `wezterm`, `ghostty` on `PATH` (only used for external reattach) | any ConPTY-capable terminal; reattach is embedded, not external |
| Window manager (optional) | Hyprland (`HYPRLAND_INSTANCE_SIGNATURE` set) or `xdotool` for focus/close | — |

### Why these?

- **Multiplexer.** Every session cc-hub spawns is wrapped in a detached
  multiplexer session so the hub can inject prompts via `send-keys` without
  stealing focus, and so the agent survives an accidentally-closed terminal.
  On Unix this is tmux; on Windows it's psmux, a tmux-compatible mux that
  uses ConPTY and ships a `tmux.exe` shim. The code calls both via the
  `tmux` binary name — make sure the Windows `psmux` install exposes
  `tmux.exe` on `PATH`.
- **`cc-hub-new`.** cc-hub launches Claude with the single shell command
  `cc-hub-new`. Define it however you like, but it needs to resolve inside
  the shell that the multiplexer pane starts. A common pattern is:
  - bash/zsh (`~/.bashrc` or `~/.zshrc`):
    ```sh
    alias cc-hub-new='claude --dangerously-skip-permissions'
    ```
  - PowerShell (`$PROFILE`):
    ```powershell
    function cc-hub-new { claude --dangerously-skip-permissions @args }
    ```

  The name is deliberately distinct from the `cc-hub` binary so the alias
  doesn't shadow the TUI on `PATH`. Use whatever flags you want — yolo mode
  is a suggestion, not a requirement.
- **Nerd Font.** State indicators and role markers in the UI use
  Nerd-Font private-use-area glyphs. Without a Nerd Font you'll see tofu
  boxes where icons should be. Any Nerd-Font patched font works
  (JetBrainsMono Nerd Font, FiraCode Nerd Font, etc.).
- **Terminal emulator (Unix).** Only consulted when you press `f` on a
  detached session whose original terminal was closed — cc-hub opens a new
  window of your emulator and runs `tmux attach` inside it. The selection
  order is `$TERMINAL` first, then the first available of `kitty`, `foot`,
  `alacritty`, `wezterm`, `ghostty`.

## Build & run

```bash
# build
cargo build --release

# TUI
cargo run --release

# plain text listing of current sessions, no TUI
cargo run --release -- --no-tui

# point this instance at a non-default Claude account / config dir
cargo run --release -- --claude-config-dir ~/.claude-personal
```

### Running multiple accounts in parallel

`--claude-config-dir <path>` mirrors Claude Code's own `CLAUDE_CONFIG_DIR`
environment variable: when set, Claude relocates its entire user-data tree
(`sessions/`, `projects/`, `history.jsonl`, `.credentials.json`) **and** its
`.claude.json` state file into that directory. cc-hub honours the same variable
for both *reading* (the session grid, usage, metrics, weekly counts) and
*spawning* (every launched `claude` — interactive sessions, titles, backlog,
auto-review — runs against that account), so one cc-hub instance maps cleanly to
one account.

To run two accounts side by side, launch one cc-hub per account:

```bash
# work (default ~/.claude)
cc-hub

# personal, in a second terminal
cc-hub --claude-config-dir ~/.claude-personal
```

The flag is just sugar for the env var, so `CLAUDE_CONFIG_DIR=~/.claude-personal
cc-hub` is equivalent. Each instance namespaces its `/tmp` usage cache by config
dir, so the two don't clobber each other's cached usage numbers.

Logs are written to `$XDG_CACHE_HOME/cc-hub/` (Linux), `~/Library/Caches/cc-hub/`
(macOS), or `%LOCALAPPDATA%\cc-hub\` (Windows). The path is printed on exit.

## Configuration

cc-hub reads `~/.cc-hub/config.toml` once at startup. The file is optional —
every field falls back to the default below, and a missing file is equivalent
to an empty one. Unknown fields are rejected so typos surface in the log
instead of being silently ignored.

Full schema with defaults:

```toml
[spawn]
# Legacy default Claude backend command. If you don't configure [agents],
# this becomes the implicit `claude` agent.
command = "cc-hub-new"

[agents.claude]
kind = "claude"
command = "cc-hub-new"

[agents.pi-codex]
kind = "pi"
command = "pi --provider openai-codex --model gpt-5.5 --thinking xhigh"
use_bridge = true

[projects]
default_orchestrator_agent = "claude"
default_session_agent = "claude"

[title]
# Master switch for the background Haiku titler. When false, cards fall back
# to the first-user-message summary instead of a generated 2-3 word title.
enabled = true
# Passed as `--model <model>` to the resolved spawn command.
model = "haiku"
# Clamp on the sanitized Haiku output (utf8-safe).
max_length = 40
# Per-call subprocess timeout. A hung `claude -p` is killed past this.
run_timeout_secs = 45
# One-time shell alias resolution timeout (paid once per process).
resolve_timeout_secs = 10
# Max simultaneous `-p` subprocesses. Keeps the first scan from fork-storming.
concurrency = 2
# Prompt prepended to the first user message. Keep the trailing `Request:`
# marker so Haiku has a cue.
prompt = """Output a 2 or 3 word title summarizing this coding-agent user request. Output only the title — no quotes, no punctuation, no prefix like "Title:". Just the words.

Request:
"""

[inactive]
# How long a dead session's JSONL stays visible after its last touch.
window_secs = 259200  # 3 days
# Per-cwd cap on inactive sessions, ranked by mtime.
max_per_project = 5

[scan]
# Fallback timer that catches PID deaths and missed fs events.
fs_fallback_interval_secs = 2
# How often to re-fetch the Anthropic usage API.
usage_refresh_interval_secs = 60
# How long the on-disk usage response is trusted before re-fetching.
usage_cache_ttl_secs = 60

[ui]
# How long status-bar messages (spawn/dispatch toasts) stay visible.
status_msg_ttl_secs = 5
# How long an auto-spawned session has to become Idle before the queued
# prompt is abandoned.
pending_dispatch_timeout_secs = 60
# Grid cell dimensions (rows, columns of terminal cells per card). At 6 the
# card body is payload + branch + model + footer; 5 and below merge the
# identity rows into one compact line.
cell_height = 6
cell_width = 42
# The Projects tab (orchestrator kanban) is WIP and hidden from the tab
# strip + Tab cycle by default. Set true to bring it back.
show_projects_tab = false
# The Planning column on the Tasks board. On by default. Set false to drop
# it; its cards fold into In Progress (Space still approves a plan-ready
# card — the action keys off the card's status, not the column).
show_planning_column = true

[metrics]
# Minimum assistant turns before a session is eligible for context-growth
# scoring.
min_growth_turns = 20
# Anomaly threshold: peak delta >= this many times the median absolute delta.
growth_threshold = 6.0
# How many rows of each finding to retain after sorting.
top_interruptions = 10
top_growth_findings = 10
top_peak_context_findings = 10

[backlog]
# Background backlog triager. When enabled, every interval cc-hub asks a
# short Claude session whether one of the pending backlog tasks is ready to
# be promoted to Running. Off by default — the tick spawns a Claude
# subprocess and you probably don't want surprise billed calls.
enabled = false
# Passed as `--model <model>` to the resolved spawn command.
model = "sonnet"
# How often the triager runs.
interval_secs = 8
# Per-call subprocess timeout for the triage Claude call.
run_timeout_secs = 120
# How long a triage decision sticks before a task becomes eligible again.
# Caps the worst-case re-ask cadence per dormant task to one per ttl_secs.
ttl_secs = 300

[auto_review]
# Background autonomous reviewer. When enabled, every interval cc-hub picks
# the oldest task in Review whose current review round hasn't been auto-
# reviewed yet, and spawns a read-only reviewer agent session. The reviewer
# inspects the diff, runs build/tests, and either approves the PR via
# `cc-hub pr approve` or asks a clarifying question via
# `cc-hub pr request-changes` (which flips the task back to Running so the
# orchestrator iterates). Each Review round gets exactly one auto-review
# pass — once the orchestrator addresses feedback and re-enters Review,
# the next tick reviews again. Off by default — every tick may spawn a
# billed agent session.
enabled = false
# Reviewer backend. None → fall back to [projects].default_orchestrator_agent.
# agent = "claude"
# How often the auto-reviewer runs.
interval_secs = 30
# Belt-and-braces gate alongside the per-round clear-on-re-entry: don't
# re-review a task whose last_auto_reviewed_at is within this many seconds.
ttl_secs = 600
# Reviewer session has up to this long to issue its verdict before cc-hub
# forgets it (the session itself is not killed; this only bounds the
# blocking-spawn timeout when applicable).
run_timeout_secs = 1800
# Max PR comments rendered into the reviewer briefing.
max_comments_in_prompt = 8
```

Only the sections/fields you want to override need to be present — omit
everything else to inherit defaults.

### Hot reload (development)

```bash
cargo run --features hot-reload
```

Rebuilds of `cc-hub-lib` are picked up without restarting the TUI. Only
useful while hacking on UI code.

## Platform differences

cc-hub tries to behave the same everywhere, but a few things genuinely
differ:

| Feature | Unix | Windows |
|---|---|---|
| Spawn a detached session with initial `cc-hub-new` | one-shot `new-session … CMD` | bare `new-session`, then `send-keys cc-hub-new Enter` (psmux ignores trailing-arg commands) |
| Embed a session pane in the TUI (`f` / `o`) | yes | yes |
| Open an external terminal attached to a detached session | yes — opens `kitty`/`foot`/etc. | no — use the embedded pane |
| Focus / close the OS window hosting a session | Hyprland or X11 (`xdotool`) | no-op |
| Claude process detection | Linux: `comm == claude`; macOS: path contains `/claude/versions/` | exe name `claude.exe` |
| POSIX session-id ancestor fallback | yes | n/a (Windows processes don't have one) |

## Keybindings

`Tab` / `BackTab` cycles the top-level tabs: **Tasks → Sessions → Metrics**
(plus **Projects**, after Tasks, when `[ui] show_projects_tab = true`).

### Tasks tab

A personal task board: **To-Do · Planning · In Progress · Done**. Cards
live at `~/.cc-hub/tasks.json` (hand-editable). Assigning a task to an
agent spawns a detached session and delivers the task text — wrapped in
plan-first framing (investigate, present a plan, hold) — once the agent is
idle; the card sits in **Planning** showing the live session state
(`⟳ working`, `󰂞 needs input`, `● plan ready`), the agent's folder, and
age. `Space` on a Planning card sends "Proceed with the implementation."
to the agent and moves the card to In Progress. Planning and In Progress
float cards whose agent is blocked on input to the top of the column; the
order settles when you open the tab and stays put while you navigate (state
flips update a card's badge in place, never its row). Done cards keep their
agent binding — `󰚩 claude · <dir>` marks a task an agent ran, and `f`
still reopens its transcript. Every card carries a priority badge on its
top-right (`P1` red · `P2` yellow · `P3` green · `P4` blue, `1`–`4` to set);
columns sort by priority first, so the most urgent cards float to the top.

The **Planning** column is optional — set `[ui] show_planning_column = false`
to drop it. Its cards then fold into **In Progress** (still showing
`● plan ready`), and `Space` on such a card still approves the plan, so the
plan-first workflow keeps working with one fewer column.

| Key | Action |
|---|---|
| `h` / `l` (or arrows) | Switch column |
| `j` / `k` (or arrows) | Move within the column |
| `a` / `n` | Add a task (lands in To-Do) |
| `1` – `4` | Set priority P1–P4 (sorts the column P1-first; P1 red · P2 yellow · P3 green · P4 blue) |
| `s` | Assign an agent: project picker (registered projects · bookmarks · recent dirs, fuzzy-filtered by typing — `Tab` flips to a plain folder browser; the last-assigned folder is preselected) → spawn session there prompted to plan first → card moves to Planning |
| `Enter` / `f` | Attach the bound agent's pane (embedded); resumes the session if its tmux died; hints `s` when unassigned |
| `Space` | On a Planning card: approve the plan — the agent is told to proceed and the card moves to In Progress (resumes the session first if its tmux died). Elsewhere: toggle Done / reopen (completing closes the live agent session; the transcript binding is kept) |
| `x` | Delete the task (a bound agent session is left running — close it from Sessions) |
| `c` | Clear all Done tasks |

### Sessions tab (grid view)

| Key | Action |
|---|---|
| `h j k l` / arrows | Navigate the grid |
| `i` | Session info popup |
| `Enter` / `f` | Attach: embedded pane if the session is in a mux, else focus its terminal window. For an inactive session, spawn a new tmux session running `cc-hub-new --resume <id>` |
| `H` | Toggle visibility of inactive sessions (hidden by default; window is 3 days) |
| `W` | Toggle visibility of orchestrator/worker sessions (hidden by default — these belong to the Projects tab) |
| `o` | Open an embedded shell pane in the selected session's cwd |
| `n` | Spawn a new `cc-hub-new` session in the selected session's cwd |
| `N` | Folder picker → spawn a new `cc-hub-new` session (`c` / `C` in the picker creates a public/private GitHub repo via `gh`) |
| `M` | Bookmarks picker → spawn a new `cc-hub-new` session in a bookmarked folder (add one with `m` on a folder in the `N` picker) |
| `p` | Dispatch a prompt to the first idle agent (auto-spawns if none) |
| `x` | Close the selected session's window (Unix WM only) |
| `Space` | Ack / mark selected session idle |
| `D` | State-debug popup (why is this session in this state?) |
| `m` | Jump to Metrics tab |
| `q` | Quit |
| `F1` (in embedded pane) | Close the pane, return to grid |

### Projects tab

> WIP — hidden by default; enable with `[ui] show_projects_tab = true`.

The Projects tab is laid out as a horizontal strip of project chips above a
five-column kanban: **Planning · Running · Review · Merging · Done**. Backlog
tasks live off the kanban — open the Backlog popup with `b` to view and
start them. A project's chip surfaces a small amber `󰒲 N` token after the
kanban counts when that project has `N` queued backlog tasks, so you have a
chip-level signal when there's pending work to triage.

| Key | Action |
|---|---|
| `H` / `L` (or `[` / `]`) | Cycle the focused project chip |
| `h` / `l` (or arrows) | Switch kanban column |
| `j` / `k` (or arrows) | Move the cursor within the focused column |
| `Enter` | Focus the orchestrator session for the selected task |
| `f` | Embed the orchestrator's tmux pane; if the pane died (PC reboot), resume the orchestrator's Claude/Pi session from disk and embed the new pane |
| `R` | Confirm, then restart the selected Running/Backlog task's orchestrator from the original prompt (blocked for Review/Done/Merging tasks) |
| `Space` | Approve the focused Review PR → Merging/queued; PR-less Review tasks go Done |
| `r` | Open the Result popup (artifacts + summary) for the focused task |
| `c` | Copy the selected task's id to the clipboard |
| `b` | Open the Backlog popup (`s`/`Enter` starts the selected backlog task; `x` deletes it) |
| `n` | New task in the current project (prompt input — `Tab` cycles the orchestrator agent when more than one is configured) |
| `N` | Folder picker → register a project, then prompt for a task |
| `x` | Delete the selected task (also works in the Backlog popup; kills its orchestrator, removes state) |
| `X` | Remove the focused project from the hub (does not delete the repo) |

## Known limitations

- **Windows focus/close is a no-op.** psmux's `list-clients -F` ignores the
  format string, so cc-hub can't resolve the attached-client PID chain
  needed for Hyprland/xdotool-style window operations. Use the embedded
  pane (`f` on a session with a mux session, or `o` for a fresh shell)
  instead — this is the intended Windows flow.
- **No native macOS window manager.** `focus` / `close` only work under
  Hyprland or X11 (via `xdotool`). On a plain macOS desktop those keys
  no-op; attach via the embedded pane instead.
- **`cc-hub-new` must be defined in your interactive shell.** cc-hub runs
  it as the pane's inaugural command via `$SHELL -ic cc-hub-new` (Unix) or
  by piping `cc-hub-new<Enter>` into the freshly-opened PowerShell
  (Windows). If your rc/profile doesn't define it, the pane will just
  print "command not found".
- **Usage cache path is fixed (default account).** Anthropic usage is cached
  at `/tmp/claude-statusline-usage.json` — a cross-process contract with an
  external statusline helper. Changing this path is a breaking change, so it's
  left untouched for the default account; a non-default `--claude-config-dir`
  gets a per-account suffix instead, so parallel instances don't collide.
- **Cleared sessions.** Claude Code's `/clear` command starts a new JSONL
  under a new session-id without updating the session metadata. cc-hub
  follows the `/clear` chain by matching clear-event timestamps against
  new JSONL creation times; this is best-effort.
- **Hot reload is dev-only.** Requires the `hot-reload` feature; don't
  ship release builds with it.

## License

MIT — see [LICENSE](LICENSE).
