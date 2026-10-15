# Keybindings reference

Every key hal binds, per tab and per popup. The status bar shows the
common ones for the current view.

## Global

These work on every tab when no popup is open. `Ctrl+L` also works in popups.

| Key | Action |
|---|---|
| `Tab` / `K` | Next tab |
| `Shift+Tab` / `J` | Previous tab |
| `q` | Quit |
| `Ctrl+L` | Repaint the screen |

Tabs run Tasks → Sessions → Agents → Metrics; hal opens on Sessions.
Agents shows once `~/.hal/agents/` exists, unless
`[harness].show_tab = false`.

## Tasks

| Key | Action |
|---|---|
| `h` `l` / `←` `→` | Previous or next column |
| `j` `k` / `↓` `↑` | Next or previous card |
| `H` / `L` | Move the card one column left or right. Skips Planning: `L` on a Planning card does not approve the plan (`Space` does); `H` from Done reopens into Review |
| `a` / `n` | Add a task |
| `r` | Rename the card |
| `t` | Edit its tags |
| `T` | Pick its kind (`[tasks].kinds`) |
| `1`–`4` | Set priority P1–P4 |
| `s` | Assign an agent: pick a folder, spawn a planning session there |
| `S` | Assign an agent in your home folder |
| `Enter` / `f` | Attach the card's agent; resume it if its session died |
| `Space` | Planning card: approve the plan. Otherwise: toggle Done |
| `v` | Task Info: prompt, notes and attachments |
| `A` | Attach a file path or URL |
| `p` | Attach the clipboard text as a note |
| `/` | Filter cards by text or `#tag` |
| `Esc` | Clear the filter |
| `x` | Delete the card (archived); its agent session keeps running |
| `c` | Clear the Done column (archived) |
| `u` | Undo the last delete or clear |

## Sessions

| Key | Action |
|---|---|
| `j` `k` `l` / arrows | Move (`h` is handoff; `←` moves left) |
| `Enter` / `f` | Type into the session's pane (docked in the split layout, a popup elsewhere), focus its window, or resume an inactive session |
| `i` | Session info |
| `v` | Cycle the layout: split (list beside the selected session's live pane), full-width list, card grid |
| `H` | Show or hide inactive sessions |
| `/` | Find any session on disk, however old, and reopen it |
| `n` | New session with the default agent, in this session's folder |
| `N` | Pick a model (and agent), then start a session in this folder |
| `A` | Choose the default agent until restart |
| `p` | Pick a folder, start a session there |
| `M` | Pick a bookmarked folder, start a session there |
| `h` | Mark the session for a handoff, or drop the mark: the next session you start opens with its last reply, named after it: `Plan` → `Plan (2)` → `Plan (3)` |
| `R` | Continue the session on another subscription account |
| `r` | Rename the session. The name a new session asks for at boot takes you into it on Enter |
| `L` | Link the session to a task, or unlink it |
| `t` | Terminal in this session's folder: a shell listed below the project's agents under its own icon. It asks for a name like a new session; Enter opens it |
| `x` | End the session (asks first): kills its tmux session, else closes its window and terminates the agent |
| `Space` | Put the session on hold, or release it. Held sessions move to the On hold section below every project, keep their real state, and stop counting toward attention; activity does not release them |
| `z` | Acknowledge: mark the session idle until its next activity |
| `m` | Go to Metrics |

`[agents.<id>].hotkey` adds a key that starts that agent in the selected
session's folder. It overrides a built-in key it collides with.

## Agents

The agents run down the side; the selected one's page beside them leads
with how it is doing.

| Key | Action |
|---|---|
| `j` `k` / `↓` `↑` | Move |
| `Enter` / `f` / `i` | Move the keyboard to the agent's page |
| `Space` | Turn the agent off or on (on also clears a halt) |
| `p` | Run now |
| `n` | Interactive Claude session in the agent's folder |

On the page:

| Key | Action |
|---|---|
| `Tab` / `l` / `→` | Next section |
| `Shift+Tab` / `h` / `←` | Previous section |
| `1`–`4` | Runs, Artifacts, Log, Settings |
| `j` `k` / `↓` `↑` | Move within the section |
| `Enter` / `f` / `o` | Open the run's transcript or the artifact; change the setting |
| `e` | Type a new value for the setting (`Enter` saves, `Esc` cancels) |
| `Space`, `p`, `n` | As on the list |
| `R` | Reset the agent's runs, spend and history (the workdir stays) |
| `Esc` / `F1` / `q` | Back to the list |

## Metrics

| Key | Action |
|---|---|
| `j` `k` / `↓` `↑` | Select a session |
| `Enter` | Open its transcript at its peak context |
| `r` | Rerun the analysis |

## Embedded pane

Every key goes to the pane, except:

| Key | Action |
|---|---|
| `F1` | Close the pane; in the split layout, give the keyboard back to the list |
| `Ctrl+Shift+V` | Paste the host clipboard |

## Mouse

hal takes the mouse while a tab is on screen with no popup over it. To
select text, drag with your terminal's bypass modifier held: usually
`Shift`, `Option` in iTerm2.

| Where | Click | Wheel |
|---|---|---|
| Tab strip | Open the tab, with the keyboard on its list | |
| Sidebar (Sessions split, Agents) | Put the cursor on the row and the keyboard on the list | Move the cursor |
| Page beside it | Give the page the keyboard; once the Sessions dock has it, clicks go to its pane | Scroll the page |
| Sessions list and cards | Put the cursor on the session | Move the cursor |
| Tasks board | Put the cursor on the card; on a column's empty space, on that column | Move the cursor in the column |
| Metrics | | Move the cursor |

## Popups

`Esc` cancels every popup. Type-to-filter popups move with `↑`/`↓` or
`Ctrl+J`/`Ctrl+K`/`Ctrl+N`/`Ctrl+P`, because letters go to the filter.
List popups move with `j`/`k` or `↑`/`↓`.

| Popup | Keys |
|---|---|
| Session info | `j`/`k` scroll, `q` close |
| Transcript tail | `j`/`k` scroll, `G` jump to the end, `q` close |
| Close confirmation | `y` close, `n`/`q` cancel |
| Model picker (`N`) | type to filter, `Tab` next agent, `Enter`/`Space` start |
| Session finder (`/`) | type to filter, `Enter` open |
| Task link picker (`L`) | type to filter, `Enter`/`Space` link |
| Agent, respawn, kind pickers | `Enter`/`Space` choose |
| Rename, tags | type, `Enter` save; tags split on space or comma |
| Add task | type, `Tab` switch to the context box, `Enter` add |
| Attach | type, `Tab` switch between file/URL and note, `Enter` attach |
| Filter | type, `Enter` keep the filter, `Esc` clear it |
| Task Info | `j`/`k` attachment, `a` attach, `p` paste note, `c` copy path, `o` open, `x` remove, `PgUp`/`PgDn` scroll, `v`/`q` close |

### Folder picker

The picker opens on a fuzzy list of places (bookmarks and recent folders), or in Browse mode when there are none.

| Mode | Keys |
|---|---|
| Places | type to filter, `Enter`/`Space` pick, `Tab` browse folders |
| Browse | `Enter`/`l`/`→` open folder, `Backspace`/`h`/`←` parent, `Space` pick, `.` pick the current folder, `m` bookmark, `c`/`C` create a public/private GitHub repo here, `Tab` places, `q` cancel |
| Bookmarks (`M`) | `Enter`/`Space`/`.` pick, `m` remove bookmark, `q` cancel |

GitHub repo name: type, `Tab` toggle public/private, `Enter` create.
