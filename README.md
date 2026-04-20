# cc-hub

A TUI-based Claude Code agentic hub. cc-hub reads Claude Code's own on-disk
session files (`~/.claude/sessions`, `~/.claude/projects`) and gives you a grid
of every session on the box: what state it's in (processing / waiting /
idle), the last user prompt, token usage, and a live tail of the JSONL
transcript. From the grid you can:

- spawn new `cc-hub-new` sessions in any folder,
- dispatch a prompt to the first idle agent (or auto-spawn one if none
  exist),
- embed an existing session's terminal pane inside the TUI,
- focus the real terminal window of a detached session (Unix only), and
- browse per-session metrics and Anthropic API usage.

## Requirements

| | Linux / macOS | Windows |
|---|---|---|
| Rust | 1.75+ (2021 edition) | 1.75+ (2021 edition) |
| Multiplexer | [`tmux`](https://github.com/tmux/tmux) on `PATH` | [`psmux`](https://github.com/psmux/psmux) on `PATH`, invoked as `tmux` |
| Claude Code | `claude` on `PATH` | `claude.exe` on `PATH` |
| Launch command | `cc-hub-new` resolvable in an interactive shell (alias/function in your rc) | `cc-hub-new` resolvable in PowerShell (function in `$PROFILE`) |
| Terminal font | Nerd Font (for state glyphs `󰑮 󰂞 󰒲`) | Nerd Font (for state glyphs `󰑮 󰂞 󰒲`) |
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
```

Logs are written to `$XDG_CACHE_HOME/cc-hub/` (Linux), `~/Library/Caches/cc-hub/`
(macOS), or `%LOCALAPPDATA%\cc-hub\` (Windows). The path is printed on exit.

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

## Keybindings (grid view)

| Key | Action |
|---|---|
| `Tab` / `BackTab` | Cycle tabs (Sessions / Metrics) |
| `h j k l` / arrows | Navigate the grid |
| `Enter` | Live-tail the selected session's JSONL |
| `i` | Session info popup |
| `f` | Attach: embedded pane if the session is in a mux, else focus its terminal window. For an inactive session, spawn a new tmux session running `cc-hub-new --resume <id>` |
| `H` | Toggle visibility of inactive sessions (hidden by default; window is 3 days) |
| `o` | Open an embedded shell pane in the selected session's cwd |
| `n` | Spawn a new `cc-hub-new` session in the selected session's cwd |
| `N` | Folder picker → spawn a new `cc-hub-new` session (`c` / `C` in the picker creates a public/private GitHub repo via `gh`) |
| `p` | Dispatch a prompt to the first idle agent (auto-spawns if none) |
| `x` | Close the selected session's window (Unix WM only) |
| `Space` | Ack / mark selected session idle |
| `D` | State-debug popup (why is this session in this state?) |
| `m` | Jump to Metrics tab |
| `q` | Quit |
| `F1` (in embedded pane) | Close the pane, return to grid |

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
- **Usage cache path is fixed.** Anthropic usage is cached at
  `/tmp/claude-statusline-usage.json` — a cross-process contract with an
  external statusline helper. Changing this path is a breaking change.
- **Cleared sessions.** Claude Code's `/clear` command starts a new JSONL
  under a new session-id without updating the session metadata. cc-hub
  follows the `/clear` chain by matching clear-event timestamps against
  new JSONL creation times; this is best-effort.
- **Hot reload is dev-only.** Requires the `hot-reload` feature; don't
  ship release builds with it.

## License

MIT — see [LICENSE](LICENSE).
