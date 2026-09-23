# cc-hub CLI reference

Reference for the `cc-hub agent` verb: what each subcommand does, its flags
and defaults, and what it prints. The other verbs (`board`, `open`,
`resource`, `usage`, `wake`) document themselves: run `cc-hub help <verb>`.

Conventions:

- Unless a subcommand is documented as printing plain text, it prints a
  single JSON object on stdout with `"ok": true`.
- Success exits 0. Bad flags and failed operations print an error to stderr
  and exit non-zero.

---

## agent

Persistent agents: one directory under `~/.cc-hub/agents/<name>/` with an
`agent.toml`. The TUI supervises enabled agents; these verbs scaffold,
drive, and inspect them. The agent name is given positionally, via
`--agent NAME`, or, inside a tick, by the `CC_HUB_AGENT` environment
variable the runner sets.

### agent list

```
cc-hub agent list [--json]
```

Prints one tab-separated row per agent: name, status
(`sleeping`|`ticking`|`halted`|`paused`|`disabled`|`broken`), trigger,
tick count, today's and lifetime spend, then the halt reason or spec error
if any. `--json` emits `{ok, root, agents: [...]}` with the same fields plus
`inbox_pending`, `last_result`, and `notes`.

### agent new

```
cc-hub agent new NAME [--from DIR]
```

Creates `~/.cc-hub/agents/NAME/` with `work/`, `inbox/`, and either the
built-in template `agent.toml` or a copy of `DIR` (see `contrib/agents/`).
Errors if the directory exists or the name has characters outside
`[A-Za-z0-9_-]`. Prints `name`, `dir`, `spec`.

### agent once

```
cc-hub agent once NAME [--event TEXT | --event-file PATH] [--force]
```

Runs one tick synchronously and records it like the supervisor would.
Ignores `enabled`, pause, and halt state (it exists to iterate on a spec),
but honours the daily/total budget unless `--force`. Prints `ok`, `tick`,
`subtype`, `turns`, `compactions`, `cost_usd`, `context_start`,
`context_end`, `duration_s`, `session_id`, `result`, `log`. A failed tick
exits 1 after printing the same line.

### agent poke

```
cc-hub agent poke NAME [--event TEXT | --event-file PATH]
```

Drops an event file into the agent's `inbox/`. Every trigger kind checks the
inbox first, so this wakes any running agent on its next loop pass. Prints
`event` (the file name, which becomes the event id) and `inbox`.

### agent pause / resume / reset

```
cc-hub agent pause NAME
cc-hub agent resume NAME
cc-hub agent reset NAME
```

`pause` makes the supervisor skip the agent; `resume` lifts that and also
clears a budget/failure halt. `reset` deletes `state.json` (ticks, spend,
history); the workdir, notes, and inbox are untouched.

### agent show

```
cc-hub agent show NAME
```

The `list --json` fields for one agent plus `history` (the last 50 ticks)
and `recent_notes`.

### agent note

```
cc-hub agent note --text TEXT [--level info|warn] [--ref URL]
```

Agent-facing. Appends a line to `notes.jsonl`; the Agents tab shows the
newest note on the card and the last twenty in the detail popup. Needs
`CC_HUB_AGENT` (set inside a tick) or `--agent`.
