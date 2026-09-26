# Keybindings reference

Every key cc-hub binds, per tab and per popup. The status bar shows the
common ones for the current view.

## Global

These work on every tab when no popup is open. `Ctrl+L` also works in popups.

| Key | Action |
|---|---|
| `Tab` / `K` | Next tab |
| `Shift+Tab` / `J` | Previous tab |
| `q` | Quit |
| `Ctrl+L` | Repaint the screen |

Tabs run Tasks → Sessions → Builds → Agents → Metrics; cc-hub opens on
Sessions. Builds shows once a recipe exists. Agents shows once
`~/.cc-hub/agents/` exists, unless `[harness].show_tab = false`.

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
| `h` `j` `k` `l` / arrows | Move |
| `Enter` / `f` | Attach the session's pane, focus its window, or resume an inactive session |
| `i` | Session info |
| `v` | Switch between card grid and compact list |
| `H` | Show or hide inactive sessions |
| `/` | Find any session on disk, however old, and reopen it |
| `n` | New session with the default agent, in this session's folder |
| `N` | Pick a model (and agent), then start a session in this folder |
| `A` | Choose the default agent until restart |
| `p` | Pick a folder, start a session there |
| `M` | Pick a bookmarked folder, start a session there |
| `R` | Continue the session on another subscription account |
| `r` | Rename the session |
| `L` | Link the session to a task, or unlink it |
| `o` | Shell pane in this session's folder |
| `x` | End the session (asks first): kills its tmux session, else closes its window and terminates the agent |
| `Space` | Acknowledge: mark the session idle |
| `m` | Go to Metrics |

`[agents.<id>].hotkey` adds a key that starts that agent in the selected
session's folder. It overrides a built-in key it collides with.

## Builds

| Key | Action |
|---|---|
| `h` `j` `k` `l` / arrows | Move between recipes |
| `r` | Run the recipe on its checkout as it is now, on the route the recipe picks |
| `n` | Run with a checkout, ref or route of your own (seeded from the last build) |
| `c` | Cancel the recipe's running and queued builds |
| `Enter` / `f` | Build output |
| `Space` | Reserve the recipe's resource, or release it |

New-build form: `Tab`/`↓` and `Shift+Tab`/`↑` change field, `←`/`→` step a
choice, typing edits text, `Enter` runs, `Esc` cancels.

Build output: `j`/`k` scroll, `PgUp`/`PgDn` page, `G`/`End` follow the end,
`Esc`/`q` close.

## Agents

| Key | Action |
|---|---|
| `j` `k` / `↓` `↑` | Move |
| `Enter` / `f` / `i` | Open the agent's detail |
| `Space` | Turn the agent off or on (on also clears a halt) |
| `p` | Run now |
| `n` | Interactive Claude session in the agent's folder |

In the detail:

| Key | Action |
|---|---|
| `Tab` / `l` / `→` | Next section |
| `Shift+Tab` / `h` / `←` | Previous section |
| `1`–`4` | Runs, Artifacts, Log, Settings |
| `j` `k` / `↓` `↑` | Move within the section |
| `Enter` / `f` / `o` | Open the run's transcript or the artifact; change the setting |
| `e` | Type a new value for the setting (`Enter` saves, `Esc` cancels) |
| `Space`, `p`, `n` | As on the tab |
| `R` | Reset the agent's runs, spend and history (the workdir stays) |
| `Esc` / `q` | Close |

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
| `F1` | Close the pane |
| `Ctrl+Shift+V` | Paste the host clipboard |

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
