# cc-hub

A terminal (TUI) hub for coding agents. cc-hub discovers every agent session
on your machine — Claude Code sessions from `~/.claude/...`, Pi sessions from
`~/.pi/agent/sessions` — and shows them in one grid. Each card shows the
session's state (processing / waiting / idle), the running tool or pending
approval/question, a live tool-call counter, context-window usage, and a live
tail of the JSONL transcript.

From the grid you can:

- spawn new agent sessions in any folder,
- send a prompt to the first idle agent (auto-spawning one if none exist),
- embed a session's terminal pane inside the TUI,
- focus the real terminal window of a detached session (Unix only),
- browse per-session metrics and Anthropic API usage.

## Tasks board

The personal layer: a three-column board — **To-Do · In Progress · Done** —
stored one file per task under `~/.cc-hub/tasks/` (a pre-existing
`tasks.json` migrates automatically on first launch; the original is kept as
`tasks.json.migrated-v1` — don't run pre-migration builds against the same
home afterwards, or you'll get a second, empty board). Jot tasks and check
them off by hand, or hand one to an agent:

1. `s` picks a folder and spawns a detached agent session there, prompted to
   investigate the task and present a plan first. The card moves to
   **Planning**.
2. `Space` approves the plan — "Proceed with the implementation." is sent to
   the agent — and the card moves to **In Progress**.
3. `f` / `Enter` attaches the agent's pane, exactly like the Sessions tab,
   including resume after the tmux session dies.

## Projects layer (WIP)

Hidden by default; enable with `[ui] show_projects_tab = true`. This is the
higher-level layer: register a directory as a project and file a free-form
*task* against it. cc-hub spawns an *orchestrator* session that breaks the
task down and dispatches *worker* sessions — read-only research workers, or
worktree-isolated edit workers — through scriptable CLI commands:

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
`~/.cc-hub/projects/<id>/tasks/<id>/state.json`. Worktrees go under
`<project-root>/.cc-hub-wt/` — add that to `.gitignore`.

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

- **Multiplexer.** Every session cc-hub spawns runs in a detached multiplexer
  session. That lets the hub inject prompts via `send-keys` without stealing
  focus, and keeps the agent alive if you close its terminal. Unix uses tmux;
  Windows uses psmux, a tmux-compatible mux built on ConPTY that ships a
  `tmux.exe` shim. cc-hub calls both as `tmux` — make sure your psmux install
  puts `tmux.exe` on `PATH`.
- **`cc-hub-new`.** cc-hub launches Claude with one shell command:
  `cc-hub-new`. Define it however you like, as long as it resolves in the
  shell the multiplexer pane starts. A common setup:
  - bash/zsh (`~/.bashrc` or `~/.zshrc`):
    ```sh
    alias cc-hub-new='claude --dangerously-skip-permissions'
    ```
  - PowerShell (`$PROFILE`):
    ```powershell
    function cc-hub-new { claude --dangerously-skip-permissions @args }
    ```

  The name differs from the `cc-hub` binary on purpose, so the alias doesn't
  shadow the TUI on `PATH`. Use whatever flags you want — yolo mode is a
  suggestion, not a requirement.
- **Nerd Font.** State indicators and role markers in the UI use Nerd-Font
  glyphs. Without one you'll see tofu boxes where icons should be. Any
  Nerd-Font patched font works (JetBrainsMono Nerd Font, FiraCode Nerd Font,
  etc.).
- **Terminal emulator (Unix).** Only used when you press `f` on a detached
  session whose original terminal is gone: cc-hub opens a new emulator window
  and runs `tmux attach` inside it. It tries `$TERMINAL` first, then the
  first available of `kitty`, `foot`, `alacritty`, `wezterm`, `ghostty`.

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
environment variable. When set, Claude moves its whole user-data tree
(`sessions/`, `projects/`, `history.jsonl`, `.credentials.json`) and its
`.claude.json` state file into that directory. cc-hub honours the same
variable for both reading (the session grid, usage, metrics, weekly counts)
and spawning (every launched `claude` — interactive sessions, titles,
backlog, auto-review — runs against that account). One cc-hub instance maps
to one account.

To run two accounts side by side, launch one cc-hub per account:

```bash
# work (default ~/.claude)
cc-hub

# personal, in a second terminal
cc-hub --claude-config-dir ~/.claude-personal
```

The flag is sugar for the env var, so `CLAUDE_CONFIG_DIR=~/.claude-personal
cc-hub` works too. Each instance namespaces its `/tmp` usage cache by config
dir, so the two don't overwrite each other's cached numbers.

Logs go to `$XDG_CACHE_HOME/cc-hub/` (Linux), `~/Library/Caches/cc-hub/`
(macOS), or `%LOCALAPPDATA%\cc-hub\` (Windows). The path is printed on exit.

## Configuration

cc-hub reads `~/.cc-hub/config.toml` once at startup. The file is optional:
every field has a default, and a missing file equals an empty one. Unknown
fields are rejected, so typos show up in the log instead of being silently
ignored.

Full schema with defaults:

```toml
[spawn]
# Legacy default Claude backend command. If you don't configure [agents],
# this becomes the implicit `claude` agent.
command = "cc-hub-new"

[agents.claude]
kind = "claude"
command = "cc-hub-new"
# Optional picker entries. A label/id table gives a friendly display name;
# a bare string uses the model id as its label. The selected id is passed as
# `--model`. Claude keeps these three defaults when `models` is omitted.
models = [
  { label = "Opus 4.8", id = "claude-opus-4-8" },
  { label = "Sonnet 5", id = "claude-sonnet-5" },
  { label = "Fable 5", id = "claude-fable-5" },
]

[agents.pi-codex]
kind = "pi"
command = "pi --provider openai-codex --thinking xhigh"
use_bridge = true
# Pi and other command-configured agents use their command's built-in model
# when this is omitted. When present, keep `--model` out of `command`.
models = ["gpt-5.6", "sol"]

[projects]
default_orchestrator_agent = "claude"
default_session_agent = "claude"

[title]
# Master switch for the background Haiku titler. When false, cards fall back
# to the first-user-message summary instead of a generated 2-3 word title.
enabled = true
# Passed as `--model <model>` to the resolved spawn command.
model = "haiku"
# Max length of the sanitized Haiku output (utf8-safe).
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
# The Planning column on the Tasks board. Off by default. Set true to show
# it; otherwise its cards fold into In Progress (Space still approves a
# plan-ready card — the action keys off the card's status, not the column).
show_planning_column = false

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
# Background backlog triager. Every interval, cc-hub asks a short Claude
# session whether a pending backlog task is ready to be promoted to Running.
# Off by default — each tick spawns a billed Claude subprocess.
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
# Background autonomous reviewer. Every interval, cc-hub picks the oldest
# task in Review whose current round hasn't been auto-reviewed yet and spawns
# a read-only reviewer session. The reviewer inspects the diff, runs
# build/tests, and either approves the PR (`cc-hub pr approve`) or asks for
# changes (`cc-hub pr request-changes`, which flips the task back to Running
# so the orchestrator iterates). Each Review round gets exactly one
# auto-review pass; when the orchestrator addresses feedback and re-enters
# Review, the next tick reviews again. Off by default — each tick may spawn
# a billed agent session.
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

Only include the sections and fields you want to override — everything else
inherits defaults.

### Hot reload (development)

```bash
cargo run --features hot-reload
```

Rebuilds of `cc-hub-lib` are picked up without restarting the TUI. Only
useful while hacking on UI code.

## Platform differences

cc-hub behaves the same everywhere it can, but a few things genuinely differ:

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

A personal task board: **To-Do · In Progress · Done** by default. Each card
is a `state.json` under `~/.cc-hub/tasks/<task-id>/` — the same per-task
format the Projects layer uses, hand-editable — with board-level metadata in
`~/.cc-hub/board.json`.

Assigning a task to an agent spawns a detached session and delivers the task
text once the agent is idle, wrapped in plan-first framing: investigate,
present a plan, hold. The card sits in **Planning**, showing the live session
state (`⟳ working`, `󰂞 needs input`, `● plan ready`), the agent's folder,
and age. `Space` on a Planning card sends "Proceed with the implementation."
and moves it to In Progress. Once that agent goes idle, the card reads
`● review ready` (cyan) — the implementation counterpart of plan ready.

Planning and In Progress float cards whose agent waits on a human to the top
of the column: blocked-on-input first, then idle plan/review-ready. The order
settles when you open the tab and stays put while you navigate — state flips
update a card's badge in place, never its row. Done cards keep their agent
binding: `󰚩 claude · <dir>` marks a task an agent ran, and `f` still reopens
its transcript.

Every card carries a priority badge on its top-right (`P1` red · `P2` yellow
· `P3` green · `P4` blue; press `1`–`4` to set). Columns sort by priority
first, so the most urgent cards float to the top.

The add popup understands a quick syntax: `#tag` tokens become tags and
`!1`–`!4` sets the priority, so `fix the parser #bug !1` lands a tagged P1
card in one round-trip (rename leaves such tokens as literal text). `/`
filters the board — the query fuzzy-matches card text and `#tag`s across all
columns; Enter keeps it applied, Esc clears it.

Deletions are recoverable: `u` restores the last `x`/`c` removal, and every
removed task is also appended to `~/.cc-hub/tasks-archive-v2.json`.

The **Planning** column is opt-in — set
`[ui] show_planning_column = true` to show it. By default its cards fold into
**In Progress** (still showing `● plan ready`), and `Space` still approves
the plan, so the plan-first workflow works with one fewer column.

| Key | Action |
|---|---|
| `h` / `l` (or arrows) | Switch column |
| `j` / `k` (or arrows) | Move within the column |
| `H` / `L` | Move the focused card one column left/right by hand. Planning is agent-owned, so manual moves skip it (To-Do ↔ In Progress ↔ Done); moving a Planning card right lands in In Progress *without* telling the agent to proceed. Into Done closes the live agent session like `Space`; out of Done reopens |
| `a` / `n` | Add a task (lands in To-Do; `#tag` and `!1`–`!4` tokens set tags/priority inline) |
| `/` | Filter the board (fuzzy over text and `#tag`s; Enter keeps it applied, Esc clears — also from the board) |
| `1` – `4` | Set priority P1–P4 (sorts the column P1-first; P1 red · P2 yellow · P3 green · P4 blue) |
| `s` | Assign an agent: project picker (registered projects · bookmarks · recent dirs, fuzzy-filtered by typing — `Tab` flips to a plain folder browser; the last-assigned folder is preselected) → spawn session there prompted to plan first → card moves to Planning |
| `Enter` / `f` | Attach the bound agent's pane (embedded); resumes the session if its tmux died; hints `s` when unassigned |
| `Space` | On a Planning card: approve the plan — the agent is told to proceed and the card moves to In Progress (resumes the session first if its tmux died). Elsewhere: toggle Done / reopen (completing closes the live agent session; the transcript binding is kept) |
| `x` | Delete the task (a bound agent session is left running — close it from Sessions); archived to `tasks-archive-v2.json` |
| `u` | Undo the last `x`/`c` removal (one batch deep, this session only) |
| `c` | Clear all Done tasks (archived; `u` restores) |

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
| `N` | Fuzzy model/agent picker → choose a model, use `Tab` to cycle configured coding agents/providers, and spawn in the selected session's cwd |
| `p` | Project/folder picker → spawn a new `cc-hub-new` session there (`c` / `C` in the picker creates a public/private GitHub repo via `gh`) |
| `M` | Bookmarks picker → spawn a new `cc-hub-new` session in a bookmarked folder (add one with `m` on a folder in the `p` picker) |
| `x` | Close the selected session's window (Unix WM only) |
| `Space` | Ack / mark selected session idle |
| `D` | State-debug popup (why is this session in this state?) |
| `m` | Jump to Metrics tab |
| `q` | Quit |
| `F1` (in embedded pane) | Close the pane, return to grid |

### Projects tab

> WIP — hidden by default; enable with `[ui] show_projects_tab = true`.

A horizontal strip of project chips sits above a five-column kanban:
**Planning · Running · Review · Merging · Done**. Backlog tasks live off the
kanban — press `b` to open the Backlog popup and start them. A chip shows a
small amber `󰒲 N` token after its kanban counts when the project has `N`
queued backlog tasks, so pending work is visible at chip level.

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
  format string, so cc-hub can't resolve the attached-client PID chain that
  Hyprland/xdotool-style window operations need. Use the embedded pane
  instead (`f` on a session with a mux session, or `o` for a fresh shell) —
  that's the intended Windows flow.
- **No native macOS window manager.** `focus` / `close` only work under
  Hyprland or X11 (via `xdotool`). On a plain macOS desktop those keys
  no-op; use the embedded pane instead.
- **`cc-hub-new` must be defined in your interactive shell.** cc-hub runs it
  as the pane's first command via `$SHELL -ic cc-hub-new` (Unix) or by piping
  `cc-hub-new<Enter>` into the freshly-opened PowerShell (Windows). If your
  rc/profile doesn't define it, the pane just prints "command not found".
- **Usage cache path is fixed (default account).** Anthropic usage is cached
  at `/tmp/claude-statusline-usage.json` — a cross-process contract with an
  external statusline helper, so the path stays fixed for the default
  account. A non-default `--claude-config-dir` gets a per-account suffix, so
  parallel instances don't collide.
- **Cleared sessions.** Claude Code's `/clear` command starts a new JSONL
  under a new session id without updating the session metadata. cc-hub
  follows the `/clear` chain by matching clear-event timestamps against new
  JSONL creation times — best-effort.
- **Hot reload is dev-only.** Requires the `hot-reload` feature; don't ship
  release builds with it.

## License

MIT — see [LICENSE](LICENSE).
