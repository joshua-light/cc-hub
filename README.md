# hal

An AI-first personal task board in the terminal. hal hands your tasks to
coding agents, and finds every Claude Code, Pi and Codex session on your
machine to show them in one grid, with each session's state, running tool,
context use and a live transcript tail.

hal used to be called cc-hub. Its first run moves `~/.cc-hub` to `~/.hal`
and the cache directory from `cc-hub` to `hal`, leaving symlinks at the old
paths on Linux and macOS. Rename your `cc-hub-new` alias (or `$PROFILE`
function) to `hal-new` yourself: until you do, new sessions exit at once.

## Tabs

- **Tasks**: a kanban board (To-Do, In Progress, Review, Done). Hand a card to
  an agent, approve its plan, attach to its session.
- **Sessions**: every agent session in a list, the selected one's tmux pane
  live beside it. A dot shows each session's state: blinking white while it
  works, green when it is done and waits for you, blue when it asks a
  question, gray when it idles or is on hold. Spawn, attach, resume, rename,
  link to a task, or continue on another account.
- **Builds**: one card per build recipe; run and cancel its steps. Shown once
  `config.toml` has a recipe.
- **Agents**: persistent agents that wake on events and run unattended. Shown
  once `~/.hal/agents/` exists.
- **Metrics**: tokens, cost, interrupted tools and context growth across
  every transcript on disk.

hal also has JSON CLI verbs (`board`, `open`, `agent`, `build`, `resource`,
`usage`, `wake`) for scripts and agents. Run `hal help`.

## Requirements

| | Linux / macOS | Windows |
|---|---|---|
| Rust | 1.88+ | 1.88+ |
| Multiplexer | [`tmux`](https://github.com/tmux/tmux) on `PATH` | [`psmux`](https://github.com/psmux/psmux), with its `tmux.exe` on `PATH` |
| Agents | any of `claude`, `pi`, `codex` on `PATH` | `claude.exe` on `PATH` |
| Launch command | `hal-new` in your interactive shell | `hal-new` in PowerShell `$PROFILE` |
| Font | a Nerd Font, for the state glyphs | a Nerd Font |
| Terminal (optional) | `$TERMINAL`, else `kitty`, `foot`, `alacritty`, `wezterm` or `ghostty`, to reattach a detached session in a new window | not used |
| Window control (optional) | Hyprland, X11 with `xdotool`, or macOS with Accessibility permission | not supported |
| Resource broker (optional) | Python 3.11+, for `hal resource` | not supported: named accounts need Unix |

hal runs every session it spawns in a detached multiplexer session. That
lets it type prompts into an agent without stealing focus, and keeps the agent
alive when you close its terminal.

hal starts Claude with one shell command, `hal-new`. The name differs
from `hal` so the alias cannot shadow the binary. Define it with the flags
you want:

```sh
# ~/.bashrc or ~/.zshrc
alias hal-new='claude --dangerously-skip-permissions'
```

```powershell
# $PROFILE
function hal-new { claude --dangerously-skip-permissions @args }
```

## Build and run

```sh
cargo build --release
target/release/hal            # the TUI
target/release/hal --no-tui   # print discovered sessions and exit
```

Logs go to `$XDG_CACHE_HOME/hal/` (Linux), `~/Library/Caches/hal/`
(macOS) or `%LOCALAPPDATA%\hal\` (Windows). hal prints the path on exit.

## Multiple Claude accounts

`--claude-config-dir <path>` is shorthand for Claude Code's
`CLAUDE_CONFIG_DIR`. hal reads that account's sessions, usage and metrics,
and every `claude` it launches runs under that account. Run one hal per
account:

```sh
hal                                         # default ~/.claude
hal --claude-config-dir ~/.claude-personal  # second terminal
```

Both instances share one usage store, `~/.hal/usage.json`, keyed by
account.

To route task sessions across several accounts automatically, and move them
when an account runs out, set up the resource broker
([resource-management.md](docs/resource-management.md)).

## Platform notes

- **Windows** has no window focus; psmux ignores the
  `list-clients -F` format hal would need. Use the embedded pane (`f` on a
  session or terminal) instead of an external window. psmux also takes
  no command in `new-session`, so hal types `hal-new` into the new
  pane.
- **macOS** asks for Accessibility permission the first time hal focuses
  a window. Until you grant it, focus fails with a "no window" status.
- **`hal-new` must resolve in an interactive shell.** hal runs
  `$SHELL -ic hal-new` (Unix) or types it into PowerShell (Windows). If
  your rc file does not define it, the pane prints "command not found".
- **Cleared sessions.** Claude's `/clear` starts a new transcript under a new
  session id. hal follows the chain by matching timestamps, which is
  best-effort.

## Documentation

- [How hal works](docs/guide.md): the task board lifecycle, deep links,
  persistent agents, builds and account moves.
- [Keybindings](docs/keybindings.md): every key, per tab and popup.
- [Configuration](docs/configuration.md): every `~/.hal/config.toml` key
  and its default.
- [CLI reference](docs/cli-reference.md): output contracts and the `agent`
  verb.
- [Resource broker](docs/resource-management.md): subscription accounts,
  task routing and shared resources.
- [Architecture](docs/architecture.md): the source layout, for contributors.

## License

MIT; see [LICENSE](LICENSE).
