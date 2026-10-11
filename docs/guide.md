# How cc-hub works

How the Tasks board, deep links, persistent agents, builds and account moves
behave. For keys see [keybindings.md](keybindings.md); for settings see
[configuration.md](configuration.md); for verb syntax run `cc-hub help <verb>`.

## Tasks board

The board has four columns: To-Do, In Progress, Review and Done. A fifth,
Planning, appears with `[ui].show_planning_column = true`; hidden, its cards
show under In Progress. Each card is a hand-editable
`~/.cc-hub/tasks/<task-id>/state.json`.

You can check cards off by hand, or hand one to an agent:

1. `s` picks a folder and spawns a detached session there. The agent is told
   to investigate and present a plan first, and the card moves to Planning.
2. `Space` sends "Proceed with the implementation." and moves the card to
   In Progress. If the session died, cc-hub resumes it first.
3. `f` attaches the agent's pane, resuming the session if needed.
4. The agent opens a pull request and notes it on the card with `PR:`. That
   note moves the card to Review.

Moving a card to Done closes its live session but keeps the binding, so `f`
on a Done card still reopens the transcript. Cards whose agent waits on you
float to the top of Planning and In Progress.

### Notes are the record

A note is text attached to a card: `p` pastes one, `cc-hub board note` adds
one from a script, and `cc-hub board notes` reads them back. A task session
writes its brief, its branch and its verification results as notes, so the
next session, or you, reads them on the card. The note's first word matters:

- `Waiting:` or `Needs you:` makes the card say `waiting on you: <question>`.
- `PR:` moves the card to Review and makes it say `PR ready: <link>`.
- A later note supersedes an earlier one.

A card that still asks a question refuses the first press of Done. Answer it
with a note (`Decided: …`), or press Done again to close it anyway.

### Kinds

A kind is a word from `[tasks].kinds` naming the deliverable (`T` picks it).
cc-hub never interprets it: it stores the word and passes it on in `&kind=`,
so a routing agent can place the card without asking. The one exception is
`[tasks].handover_kinds`, the kinds worked by two sessions in sequence (one
builds, one verifies). A `role=verification` link for a card of any other
kind is refused.

### Stats, priority, recovery

Done cards show tokens and cost across every session that worked the task,
refreshed every 30 seconds; `v` shows the exact totals. `~$` marks an
estimate at the Metrics tab's rates, and a model without known pricing shows
`cost unavailable`. These are usage costs, not subscription charges.

Columns sort by priority first (P1–P4). The add popup reads `#tag` and `!1`–`!4`
inline, so `fix the parser #bug !1` makes a tagged P1 card. Its context box
becomes the card's first note.

`u` undoes the last delete or clear. Every removed card is also appended to
`~/.cc-hub/tasks-archive-v2.json`.

## Deep links

cc-hub owns the `cc-hub://` scheme: a browser button, a script or an agent
can start work with `cc-hub open <url>`. There are four links.

- **review** spawns a session in the local checkout of a pull request's
  repository and asks for a light or full review. cc-hub finds the checkout
  by repo name among bookmarks and known session folders, else reviews from
  home. `&post=<n>` lets the review post findings it is at least `n`% sure
  of without asking. A person clicking a button needs no such licence; an
  agent that starts reviews unattended does.
- **fix** files a `Fix: <title>` card whose first note is the brief, then
  spawns a session with standing orders to work through the pull request's
  review comments. It skips Planning, because the comments are the plan.
- **merge-target** is filed and started like a fix, as a `Merge Target:
  <title>` card, with standing orders to merge the pull request's target
  branch into its branch: no rebase, no force-push, conflicts resolved
  keeping both sides' intent, the author asked when one needs a decision.
- **task** starts a session for an existing card, bound to it, with
  `/task --task <id> …` as its prompt. It never changes the card's status. A
  live session for the card in the same folder gets the prompt instead
  (`"reused": true`).

A task link naming a `role` is a hand-over between the `task` skill's
sessions: it starts a fresh session and, once the command has reported,
closes the card's previous session in that folder. It needs at least one
note on the card, because the notes are the next session's brief. A link
naming another folder without a role starts a new session there and leaves
the old one running.

With accounts configured in `resources.toml` and no `--agent`, fix and
merge-target links and task links whose kind is known (`&kind=`, or an earlier broker worker on the
card) start through the resource broker. They run on an account with room
and move when it runs out. The broker also treats a change of folder as a
hand-over ([resource-management.md](resource-management.md)).

## Persistent agents

A persistent agent is a folder `~/.cc-hub/agents/<name>/` with an
`agent.toml`. While the TUI runs, it supervises every enabled agent: an
inbox file, a poll command's output or a timer becomes one bounded
`claude -p` run with the spec's tools, permissions and budget. Between events
an agent costs nothing.

A polling agent can also list wakes (`trigger.wake = ["board"]`). A wake is
a "look now" with no payload: the poll runs within a second instead of at the
end of its interval, and still decides what counts as an event. The board
wakes `board` on every status change; a script wakes anything with
`cc-hub wake <name>`.

Agents report with `cc-hub agent note`. The Agents tab shows the newest note;
the detail view shows:

- **Runs**: each remembered run, with its transcript.
- **Artifacts**: the agent's notes; `f` opens a note's `--ref`.
- **Log**: `events.jsonl`, which records runs, poll failures, halts and edits
  made in the hub. It answers "why didn't it run?".
- **Settings**: model, effort, interval, budgets and more. Edits keep
  `agent.toml`'s comments, are validated, and apply on the next run.

A run has no terminal, so it never shows on the Sessions tab. `n` opens an
interactive session in the agent's folder, primed with why it last failed.
Commands and the folder layout: `cc-hub help agent` and
[cli-reference.md](cli-reference.md).

## Builds

A build is one run of a recipe: its steps, in order, over a checkout at a ref
(see [configuration.md](configuration.md#buildsrecipesname)). The first step
to fail ends the build. Each build is `~/.cc-hub/builds/<id>/` with
`build.json` and `output.log`, driven by a detached `cc-hub build _run <id>`
that outlives the TUI. A recipe runs one build at a time, oldest first, and
keeps its last twenty finished builds.

Any step reports progress with `cc-hub: commit|route|phase …` lines, and a
later step can name the reported commit as `{commit}`. The runner sets
`CC_HUB_BUILD`, so a script also run by hand can stay quiet when nothing
listens. `current` prints the commit the recipe's last run left in place.
When that differs from the last successful build's, something else ran since,
and the card says `now at <commit>`.

A recipe with a `resource` claims it through the broker as the guest
`Builds` and keeps it after the build ends. Builds come in bursts, and
releasing between two of them would let another session in. `Space` on the
tab, or `cc-hub build reserve`/`release`, takes or frees it by hand. A cancel
runs the recipe's `cancel`, then ends the running step if it still runs 15
seconds later.

## Handing off to a fresh session

`h` on the Sessions tab marks a session (light-blue border or gutter) and
takes its last reply. The next session you start — `n`, `N`, an agent
hotkey, `p`, `M` — opens with that reply typed into its input as
`<context>…</context>`, unsent. Add what to do with it and press Enter: a
fresh session that continues where the old one stopped, without compacting.
`h` on the marked session drops the mark.

## Moving a session to another account

`R` on the Sessions tab continues a session on another subscription account,
for example when its account hits a usage limit. Claude to Claude copies the
transcript into the target account and resumes it natively. Every other
pairing starts a fresh session whose opening prompt names the old
transcript. These are the broker's replacement rules, applied by hand.
