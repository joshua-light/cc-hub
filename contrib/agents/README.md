# Example persistent agents

Copy one into `~/.cc-hub/agents/` and edit it:

```
cc-hub agent new hello --from contrib/agents/hello
cc-hub agent once hello --event "hi"      # one tick, prints the outcome
```

| dir | trigger | what it shows |
|---|---|---|
| `hello/` | inbox | the smallest working spec: one tool, a note, an answer |
| `pr-watch/` | poll `./watch.sh` every 5m | a watcher script as the event source, state kept in `work/` |

A spec is one `agent.toml`; see `cc-hub help agent` for the layout and the
fields. The hub stays source-agnostic: to watch something new, write a script
that prints when there is something to react to.
