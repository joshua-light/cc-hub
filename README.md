# cc-hub

A terminal (TUI) hub for coding agents. cc-hub discovers every agent session
on your machine — Claude Code sessions from `~/.claude/...`, Pi sessions from
`~/.pi/agent/sessions`, Codex sessions from `~/.codex/sessions` — and shows
them in one grid. Each card shows the
session's state (processing / waiting / idle), the running tool or pending
approval/question, a live tool-call counter, context-window usage, and a live
tail of the JSONL transcript.

From the grid you can:

- spawn new agent sessions in any folder,
- respawn a session on another subscription account (`R`) — say, when its
  account hits a usage limit: Claude→Claude carries the transcript over and
  resumes it natively, every other pairing starts fresh with the transcript
  path in the opening prompt (the broker's replacement rules, by hand),
- send a prompt to the first idle agent (auto-spawning one if none exist),
- embed a session's terminal pane inside the TUI,
- focus the real terminal window of a detached session (Unix only),
- browse per-session metrics and Anthropic API usage.

## Tasks board

The personal layer: a board — **To-Do · In Progress · Review · Done** —
stored one file per task under `~/.cc-hub/tasks/`. Jot tasks and check
them off by hand, or hand one to an agent:

1. `s` picks a folder and spawns a detached agent session there, prompted to
   investigate the task and present a plan first. The card moves to
   **Planning**.
2. `Space` approves the plan — "Proceed with the implementation." is sent to
   the agent — and the card moves to **In Progress**.
3. `f` / `Enter` attaches the agent's pane, exactly like the Sessions tab,
   including resume after the tmux session dies.
4. The agent opens a pull request and writes it on the card as a note that
   opens with `PR:`. That note moves the card to **Review**, so what waits
   on a reading of yours never sits among what waits on an answer.

A script mints a card with `cc-hub board add --text "…" [--title T] [--tags
"a b"] [--priority p1..p4]`; it lands in To-Do with no session, and a
`cc-hub://task` link (below) opens it. `cc-hub board note --task ID --text "…"`
appends a note to a card (the same attachment the `p` key pastes) and
`cc-hub board notes --task ID` reads them back in order. A task session's
record — the brief it agreed with the user, the branch it built, what
verification found — is those notes, on the card, where you already look.
The first word of a note is read by the board: `Waiting:` / `Needs you:`
makes the card say `waiting on you: <question>` in yellow, and `PR:` moves it
to Review and makes it say `PR ready: <link>` in the column's cyan. A later
note supersedes an earlier one, so a question asked after the PR went up
brings the card back to asking for an answer. A card still asking one does
not go to Done on the first press: the status line repeats the question, and
either a note answers it (`Decided: …`) or a second press closes the card
anyway — a task nobody answered is not a task done by accident.

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

## Deep links

cc-hub owns the `cc-hub://` URL scheme, so a browser button, a shell `open`
or a persistent agent can start a session. Three link kinds exist today:

```
cc-hub://review?depth=<light|full>&pr=<pull request url>[&title=<text>][&post=<confidence>]
cc-hub://fix?pr=<pull request url>[&title=<text>][&kind=<word>]
cc-hub://task?id=<task id>[&dir=<path>][&kind=<word>]
```

`cc-hub open <url>` spawns the default session agent in the local checkout of
the pull request's repository, names the session `PR: <title>` (from the
optional `title` parameter, else `PR: <repo>#<n>`) before it starts, and opens
it with `Let's do <depth> review of this PR: <url>`. The checkout is found by repo name among registered projects,
bookmarks, and the cwds of known sessions; a repository none of them names is
reviewed from the home directory, off the pull request alone. `--dry-run`
shows where a link would land without spawning anything. `--agent <id>` runs the review under a
backend other than the default one.

The optional `post` parameter is a confidence percentage. It adds `Post
automatically all comments and questions with confidence >= <n>.` to the
prompt. A person clicking a browser button needs no such licence, because the
review can just ask them. A caller with nobody watching does need it, which is
why a persistent agent that starts its own reviews sends `post=80`.

A `fix` link is the other side of a review, and it is a task: opening it
files a card on the Tasks board named `Fix: <title>`, whose first note is the
brief (the pull request's comments are the problem, working them is the
solution, the reviewer is the verification) and whose kind is the link's
`kind`, one of `[tasks].kinds`. The session lands in the same checkout as a
review would and opens with the standing orders for working through the
comments: switch to the branch, address every comment, track each one as a
Bitbucket task and mark it done once the fix is committed and pushed, answer
questions, ask when a comment is ambiguous, skip what is already resolved,
sign every reply as written by Claude/Codex, and note `Pushed: …` on the card
when done. With subscription accounts configured the session starts through
the resource broker, so it runs on whichever account has room and is replaced
when that account hits its limit, like any routed card. The card moves to
Running the moment its session is bound and skips Planning: the plan gate
exists to produce a brief, and a fix arrives with one.

A `task` link works one card of the Tasks board: it spawns a session in
`dir` (default: the card's own cwd), names it `Task: <card>`, opens it with
`/task --task <id> [--kind <kind>] [--role <role>] <card text>`, and binds the
card to that session, so `f` attaches to it exactly as if the board had
assigned it. The card's status is left alone — a link starts work, it never
moves a card. `kind` and `role` are words passed through to the prompt; the
`task` skill decides what they mean. A link with a `role` is a hand-over: it
always starts a fresh session and closes the card's previous one once the
command has reported, which is how the skill's implementation session passes
a task to its verification session. A hand-over is refused for a card with
no note: the notes are the brief the next session works from, so there is no
hand-over without one. Local agents can use these links to hand tasks to
interactive sessions.
An OS URL-scheme handler or browser integration can forward links to
`cc-hub open`; platform integrations are configured separately.

## Agents

Persistent agents watch for events and react without an interactive session
open. Each is a directory under `~/.cc-hub/agents/<name>/` with an
`agent.toml`. The Hub supervises enabled agents: an inbox file, poll command's
output, or timer becomes a bounded `claude -p` tick with configured tools,
permissions, and budgets. Between events the agent costs nothing.

```
cc-hub agent new hello                             # scaffold a spec
cc-hub agent once hello --event "hi"                # run one tick
cc-hub agent poke hello --event "hi"                # queue an event
cc-hub wake board                                   # "look now" to agents watching the board
```

A polling agent can also subscribe to a wake — `trigger.wake = ["board"]` —
and then its poll runs within a second of the thing happening instead of at
the end of `interval_s`, which is how a card moved to In Progress reaches the
task router immediately. A wake carries no payload: it says "look now", and
the poll command still decides what an event is, so the interval stays a
floor under it and a wake that never lands costs latency only. The board
wakes `board` on every card status change; anything else says so with
`cc-hub wake <name>`.

Agents report through `cc-hub agent note`, shown on the Agents tab and detail
popup. Press `f` to view a running or recent tick's transcript. Halted agents
show a "needs you" indicator. See `cc-hub help agent` for configuration fields,
or copy a local spec with `cc-hub agent new <name> --from <directory>`.

Custom agents and skills remain local: `contrib/` is ignored, and installed
agents live under `~/.cc-hub/agents/`. The repository contains the shared
agent runtime and interfaces.

A local watcher can open `cc-hub://task` links when cards become ready.
Opening a link for a card with a live session in that directory delivers the
prompt to it (`"reused": true`) instead of starting another session. A link
naming a different directory, or a role, starts a new session.

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
# Optional Sessions-tab hotkey: one character that spawns this agent in the
# selected session's cwd, like `n` but with a fixed agent. Custom hotkeys
# shadow built-in Sessions keys, so binding `N` here replaces the model
# picker for this run. Duplicate or multi-character bindings are dropped
# with a warning in the log.
hotkey = "N"
# Optional picker entries. A label/id table gives a friendly display name;
# a bare string uses the model id as its label. The selected id is passed as
# `--model`. Claude keeps these three defaults when `models` is omitted.
models = [
  { label = "Opus 4.8", id = "claude-opus-4-8" },
  { label = "Sonnet 5", id = "claude-sonnet-5" },
  { label = "Fable 5", id = "claude-fable-5" },
]

[harness]
# Persistent agents (Agents tab). The supervisor runs inside the TUI and only
# ticks agents that exist under ~/.cc-hub/agents/ and are enabled.
enabled = true
show_tab = true
refresh_secs = 3

[agents.pi-codex]
kind = "pi"
command = "pi --provider openai-codex --thinking xhigh"
use_bridge = true
# Pi and other command-configured agents use their command's built-in model
# when this is omitted. When present, keep `--model` out of `command`.
models = ["gpt-5.6", "sol"]

[agents.codex]
# OpenAI's `codex` CLI as a native backend: discovered from
# `~/.codex/sessions/**/rollout-*.jsonl`, liveness via a `codex` process scan
# (it writes no status file). cc-hub adds `-m <model>` from the picker for new
# sessions and `resume <uuid>` on resume — keep `-m`/`resume` out of `command`;
# fixed flags like reasoning effort belong here.
kind = "codex"
command = "codex -c model_reasoning_effort=medium --yolo"
models = [{ label = "GPT 5.6 Sol", id = "gpt-5.6-sol" }]
hotkey = "C"

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

Completed cards show **Task Stats**: tokens consumed and USD cost. Press `v` for
exact input, output, cache-read, and cache-write totals. Stats refresh in the
background every 30 seconds and persist with the task, including earlier agent
assignments, explicitly linked sessions, and Claude subagents. Existing tasks
are backfilled when their transcripts are available. `~$` means an estimate at
the Metrics tab's model rates; Pi's reported costs are used when available.
Unsupported model pricing (including Codex) shows `cost unavailable`; missing
transcripts show unavailable usage or retain the last complete saved snapshot.
These are session usage costs, not subscription charges or an invoice.

Every card carries a priority badge on its top-right (`P1` red · `P2` yellow
· `P3` green · `P4` blue; press `1`–`4` to set). Columns sort by priority
first, so the most urgent cards float to the top.

The add popup understands a quick syntax: `#tag` tokens become tags and
`!1`–`!4` sets the priority, so `fix the parser #bug !1` lands a tagged P1
card in one round-trip (rename leaves such tokens as literal text). Under the
task line sits a context box: `Tab` moves into it, and a paste that carries
newlines lands there whichever field has the cursor. Whatever it holds is
saved as the new card's first note attachment — the same thing `p` pastes
onto an existing card — so the agent later assigned with `s` reads it before
it plans.

`/` filters the board — the query fuzzy-matches card text and `#tag`s across
all columns; Enter keeps it applied, Esc clears it.

Deletions are recoverable: `u` restores the last `x`/`c` removal, and every
removed task is also appended to `~/.cc-hub/tasks-archive-v2.json`.

A card can also carry a **kind** — the deliverable it produces, picked with
`T` from `[tasks].kinds` and shown as a chip in the card's top-left corner:

```toml
[tasks]
kinds = ["tps", "ai-plugin", "tool", "hub", "basic", "repair"]
```

cc-hub never interprets a kind; it stores the word and hands it to whoever
opens the session — `cc-hub://task?...&kind=<word>`, the same query parameter
a link already carries. For an agent that routes the board that is the
difference between a card it classifies and one it merely places: a card with
a kind is worked where that word says, and can't come back asking which kind
it is. Leave a card's kind unset and routing works exactly as before.

The **Planning** column is opt-in — set
`[ui] show_planning_column = true` to show it. By default its cards fold into
**In Progress** (still showing `● plan ready`), and `Space` still approves
the plan, so the plan-first workflow works with one fewer column.

| Key | Action |
|---|---|
| `h` / `l` (or arrows) | Switch column |
| `j` / `k` (or arrows) | Move within the column |
| `H` / `L` | Move the focused card one column left/right by hand. Planning is agent-owned, so manual moves skip it (To-Do ↔ In Progress ↔ Review ↔ Done); moving a Planning card right lands in In Progress *without* telling the agent to proceed. Review is normally reached by the card's own `PR:` note. Into Done closes the live agent session like `Space` — and, on a card still asking a question, is refused once, exactly like `Space`; out of Done reopens into Review |
| `a` / `n` | Add a task (lands in To-Do; `#tag` and `!1`–`!4` tokens set tags/priority inline; `Tab` — or a multi-line paste — fills the context box, saved as the card's first note) |
| `/` | Filter the board (fuzzy over text and `#tag`s; Enter keeps it applied, Esc clears — also from the board) |
| `1` – `4` | Set priority P1–P4 (sorts the column P1-first; P1 red · P2 yellow · P3 green · P4 blue) |
| `T` | Pick the card's **kind** — the deliverable it produces (`[tasks].kinds`). The task router places the card by that word instead of guessing one, and can no longer hand it back asking which kind it is; the first row clears it back to router-chosen |
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
| `n` | Spawn a new session with the current default agent in the selected session's cwd |
| `A` | Choose the default agent used by subsequent `n` and folder-picker session spawns (for the current run) |
| `[agents.<id>].hotkey` | User-defined per-agent keys (e.g. `C` → Codex): spawn that agent in the selected session's cwd regardless of the `A` default. Shadows the built-in key it collides with |
| `N` | Fuzzy model/agent picker → choose a model, use `Tab` to cycle configured coding agents/providers, and spawn in the selected session's cwd |
| `p` | Project/folder picker → spawn the current default agent there (`c` / `C` in the picker creates a public/private GitHub repo via `gh`) |
| `M` | Bookmarks picker → spawn the current default agent in a bookmarked folder (add one with `m` on a folder in the `p` picker) |
| `L` | Link the selected session to a task from the Tasks board (fuzzy picker, banded by status in board-column order with the board's status colors; tasks assigned to the session's cwd lead their band). A linked session's card carries a `󰓹 task` badge on its bottom border, colored per task (stable hash of the task id), so cards of the same task share a mark without regrouping the grid; press `L` again to switch tasks or pick `✕ unlink`. A Done/deleted task keeps the group but dims the header. Links live in `~/.cc-hub/session-tasks.json` |
| `x` | Close the selected session's window (Unix WM only) |
| `Space` | Ack / mark selected session idle |
| `m` | Jump to Metrics tab |
| `q` | Quit |
| `F1` (in embedded pane) | Close the pane, return to grid |

### Agents tab

Shown once `~/.cc-hub/agents/` exists. One row per agent, in the same table
grammar as the Sessions list: the status icon carries the state (green
ticking, yellow halted, red broken spec, purple paused), then the name, the
agent's latest word (halt reason, newest note, last tick result, or the spec
description), and the columns — trigger, queued events, last tick, context,
today's spend, age.

A tick is a headless `claude -p` run with no terminal behind it, so its
session never appears on the Sessions tab — there would be nothing for `f` to
attach to. It lives here instead: `f` opens the transcript of the tick in
flight (or the last one), tailing live while it runs.

| Key | Action |
|---|---|
| `j` / `k` (or arrows) | Move between agents |
| `Enter` / `i` | Detail popup: tick timeline, notes, spec summary (`j`/`k` scroll) |
| `f` | Open the tick's transcript, tailing live |
| `p` | Poke: drop an empty event into the agent's inbox |
| `Space` | Pause a running agent; resume a paused or halted one |
| `R` | Reset the harness bookkeeping (ticks, spend); the workdir is untouched |

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

## License

MIT — see [LICENSE](LICENSE).

Account profiles, session-claimed resources (checkouts, devices), capacity-aware Task Agent roles and proactive handoff are described in [Resource management](docs/resource-management.md).
