# cc-hub

A terminal hub for coding agents. cc-hub finds every Claude Code, Pi and Codex
session on your machine and shows them in one grid, with each session's state,
running tool, context use and a live transcript tail. It also runs a personal
task board that hands tasks to agents.

## Tabs

- **Tasks**: a kanban board (To-Do, In Progress, Review, Done). Hand a card to
  an agent, approve its plan, attach to its session.
- **Sessions**: every agent session as a card. Spawn, attach, resume, rename,
  link to a task, or continue on another account.
- **Builds**: one card per build recipe; build, cancel and serve. Shown once
  `config.toml` has a recipe.
- **Agents**: persistent agents that wake on events and run unattended. Shown
  once `~/.cc-hub/agents/` exists.
- **Metrics**: tokens, cost, interrupted tools and context growth across
  every transcript on disk.

cc-hub also has JSON CLI verbs (`board`, `open`, `agent`, `build`, `resource`,
`usage`, `wake`) for scripts and agents. Run `cc-hub help`.

## Requirements

| | Linux / macOS | Windows |
|---|---|---|
| Rust | 1.88+ | 1.88+ |
| Multiplexer | [`tmux`](https://github.com/tmux/tmux) on `PATH` | [`psmux`](https://github.com/psmux/psmux), with its `tmux.exe` on `PATH` |
| Agents | any of `claude`, `pi`, `codex` on `PATH` | `claude.exe` on `PATH` |
| Launch command | `cc-hub-new` in your interactive shell | `cc-hub-new` in PowerShell `$PROFILE` |
| Font | a Nerd Font, for the state glyphs | a Nerd Font |
| Terminal (optional) | `$TERMINAL`, else `kitty`, `foot`, `alacritty`, `wezterm` or `ghostty`, to reattach a detached session in a new window | not used |
| Window control (optional) | Hyprland, X11 with `xdotool`, or macOS with Accessibility permission | not supported |
| Resource broker (optional) | Python 3.11+, for `cc-hub resource` | not supported: named accounts need Unix |

cc-hub runs every session it spawns in a detached multiplexer session. That
lets it type prompts into an agent without stealing focus, and keeps the agent
alive when you close its terminal.

cc-hub starts Claude with one shell command, `cc-hub-new`. The name differs
from `cc-hub` so the alias cannot shadow the binary. Define it with the flags
you want:

```sh
# ~/.bashrc or ~/.zshrc
alias cc-hub-new='claude --dangerously-skip-permissions'
```

```powershell
# $PROFILE
function cc-hub-new { claude --dangerously-skip-permissions @args }
```

## Build and run

```sh
cargo build --release
target/release/cc-hub            # the TUI
target/release/cc-hub --no-tui   # print discovered sessions and exit
```

Logs go to `$XDG_CACHE_HOME/cc-hub/` (Linux), `~/Library/Caches/cc-hub/`
(macOS) or `%LOCALAPPDATA%\cc-hub\` (Windows). cc-hub prints the path on exit.

## Multiple Claude accounts

`--claude-config-dir <path>` is shorthand for Claude Code's
`CLAUDE_CONFIG_DIR`. cc-hub reads that account's sessions, usage and metrics,
and every `claude` it launches runs under that account. Run one cc-hub per
account:

```sh
cc-hub                                         # default ~/.claude
cc-hub --claude-config-dir ~/.claude-personal  # second terminal
```

Both instances share one usage store, `~/.cc-hub/usage.json`, keyed by
account.

To route task sessions across several accounts automatically, and move them
when an account runs out, set up the resource broker
([resource-management.md](docs/resource-management.md)).

## Platform notes

- **Windows** has no window focus; psmux ignores the
  `list-clients -F` format cc-hub would need. Use the embedded pane (`f` on a
  session, `o` for a shell) instead of an external window. psmux also takes
  no command in `new-session`, so cc-hub types `cc-hub-new` into the new
  pane.
- **macOS** asks for Accessibility permission the first time cc-hub focuses
  a window. Until you grant it, focus fails with a "no window" status.
- **`cc-hub-new` must resolve in an interactive shell.** cc-hub runs
  `$SHELL -ic cc-hub-new` (Unix) or types it into PowerShell (Windows). If
  your rc file does not define it, the pane prints "command not found".
- **Cleared sessions.** Claude's `/clear` starts a new transcript under a new
  session id. cc-hub follows the chain by matching timestamps, which is
  best-effort.

## Documentation

- [How cc-hub works](docs/guide.md): the task board lifecycle, deep links,
  persistent agents, builds and account moves.
- [Keybindings](docs/keybindings.md): every key, per tab and popup.
- [Configuration](docs/configuration.md): every `~/.cc-hub/config.toml` key
  and its default.
- [CLI reference](docs/cli-reference.md): output contracts and the `agent`
  verb.
- [Resource broker](docs/resource-management.md): subscription accounts,
  task routing and shared resources.
- [Architecture](docs/architecture.md): the source layout, for contributors.

## License

MIT; see [LICENSE](LICENSE).
