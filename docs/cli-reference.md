# CLI reference

The output contract every `hal` verb shares, and the `agent` verb in
full. The other verbs document themselves: `hal help <verb>`.

## Conventions

- A verb prints one JSON object on one line of stdout, with `"ok": true` on
  success. The exceptions print plain text: `help`, `--no-tui`,
  `agent list` without `--json`, and `board notes` without `--json`.
- A failure prints `{"ok": false, "error": "…", "kind": "…"}` on stdout,
  plus `"recipe"` when the error suggests a fix, and a human line on stderr.
  `kind` is `usage`, `notfound`, `conflict` or `other`.
- Exit codes: 0 on success, 2 for `usage` errors, 1 otherwise.
- A few failures print their own richer `{"ok": false, …}` line instead
  (for example a failed `agent once`); they exit 1.
- `resource` passes the broker's line through: `{"ok": true, "result": …}`
  or `{"ok": false, "error": "…"}`, exit 1 on any failure. `resource hook`
  prints only a note for the agent, when it has one.
- `--help` or `-h` after any verb except `wake` prints its help.

## Verbs

| Verb | Does | Details |
|---|---|---|
| `board` | Add a card, add or read its notes | `hal help board` |
| `open` | Act on a `hal://` link | `hal help open` |
| `agent` | Scaffold, run and inspect persistent agents | below |
| `resource` | Accounts, task sessions and shared resources | `hal help resource`, [resource-management.md](resource-management.md) |
| `usage` | Quota of a Claude account | `hal help usage` |
| `wake` | `hal wake <name>`: agents watching `<name>` poll now. Prints `{"ok": true, "wake": "<name>"}` | none |

## agent

A persistent agent is a folder `~/.hal/agents/<name>/` with an
`agent.toml`. The TUI supervises the enabled ones; these subcommands
scaffold, drive and inspect them. Name the agent positionally, with
`--agent NAME`, or, inside a run, through `HAL_AGENT`, which the runner
sets.

### agent list

```sh
hal agent list [--json]
```

One tab-separated line per agent: name, status (`sleeping`, `ticking`,
`halted`, `paused`, `disabled` or `broken`), trigger, run count, today's and
total spend, then the halt reason or spec error. `--json` prints
`{ok, root, agents: […]}` with the same fields plus `inbox_pending`,
`last_result` and `notes`.

### agent new

```sh
hal agent new NAME [--from DIR]
```

Creates `~/.hal/agents/NAME/` with `work/`, `inbox/` and the built-in
`agent.toml` template, or a copy of `DIR`. Fails if the folder exists or the
name has characters outside `[A-Za-z0-9_-]`. Prints `name`, `dir`, `spec`
and `next`.

### agent once

```sh
hal agent once NAME [--event TEXT | --event-file PATH] [--force]
```

Runs one tick now and records it as the supervisor would. Ignores `enabled`,
pause and halt, so you can iterate on a spec, but honours the daily and total
budgets unless `--force`. Prints `ok`, `tick`, `subtype`, `turns`,
`compactions`, `cost_usd`, `context_start`, `context_end`, `duration_s`,
`session_id`, `result`, `stderr` and `log`. A failed tick prints the same
line with `"ok": false` and exits 1. Without an event, a poll agent runs its
poll first and ticks on what it prints, as the supervisor would; when it
prints nothing there is no tick, and `tick` is `null`.

### agent poke

```sh
hal agent poke NAME [--event TEXT | --event-file PATH]
```

Drops an event file into the agent's `inbox/`. Every trigger kind checks the
inbox first, so this wakes the agent on its next loop pass. Prints `agent`,
`event` (the file name, which becomes the event id) and `inbox`.

Without an event it runs the agent now, the same way `p` on the Agents tab
does. A poll or interval agent's trigger fires on the next pass: a poll agent
polls at once and runs only if the poll prints an event, and output it already
printed before still counts. An inbox agent gets an empty event. Prints `agent`
and `run`.

### agent pause, resume, reset

```sh
hal agent pause NAME
hal agent resume NAME
hal agent reset NAME
```

`pause` makes the supervisor skip the agent. `resume` undoes that and also
clears a budget or failure halt. Both print `agent`, `paused` and
`stopped_reason`. `reset` deletes `state.json` (runs, spend, history) and
leaves the workdir, notes and inbox alone; it prints `agent`.

### agent show

```sh
hal agent show NAME
```

The `list --json` fields for one agent, plus `history` (the last 50 runs)
and `recent_notes` (the last 50 notes).

### agent note

```sh
hal agent note --text TEXT [--level info|warn] [--ref URL]
```

For the agent itself. Appends a line to `notes.jsonl` and prints `agent` and
`tick`. The Agents tab shows the newest note on the agent's row and the rest
under Artifacts; `--ref` is what `f` opens there. Needs `HAL_AGENT` or
`--agent`.
