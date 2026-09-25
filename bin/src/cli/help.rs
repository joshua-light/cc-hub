//! Help text for `cc-hub help [topic]` and `--help`.

use super::CliError;

pub(crate) fn print_cli_help(topic: &[String]) -> Result<(), CliError> {
    match topic.first().map(String::as_str) {
        None => print!("{}", GENERAL_HELP),
        Some("open") => print!("{}", OPEN_HELP),
        Some("agent") => print!("{}", AGENT_HELP),
        Some("board") => print!("{}", BOARD_HELP),
        Some("build") => print!("{}", BUILD_HELP),
        Some("resource") => print!("{}", RESOURCE_HELP),
        Some("usage") => print!("{}", USAGE_HELP),
        Some(other) => {
            return Err(CliError::Usage(format!(
                "unknown help topic: {} (try `cc-hub help`)",
                other
            )));
        }
    }
    Ok(())
}

const GENERAL_HELP: &str = r#"cc-hub

Usage:
  cc-hub                         Start the TUI
  cc-hub --no-tui                Print discovered sessions
  cc-hub help [topic]            Show CLI help

Desktop-facing topics:
  open              Act on a cc-hub:// deep link (a PR review or fix, a board task)

Persistent agents (Agents tab):
  agent             Scaffold, run, poke, pause and inspect persistent agents

Builds (Builds tab):
  build             Start, cancel, serve and list builds of a [builds.recipes] recipe

Tasks board:
  board             Add a card to the personal Tasks board
  resource          Account capacity and the managed session working each task
  wake              Say a named thing happened, so agents watching it poll now
  usage             Quota of a Claude account, from the shared store

Examples:
  cc-hub board add --text "Fix the flaky test" --priority p1
  cc-hub open "cc-hub://task?id=tk-123&dir=$PWD"
"#;

const USAGE_HELP: &str = r#"cc-hub usage [--home DIR] [--refresh]

Print one JSON line with the account's quota windows and the store's health.
Every consumer (statusline, TUI, resource broker) reads through this store, so
the usage endpoint sees one request per account per minute from this machine.

  --home DIR     Claude config dir of the account; default CLAUDE_CONFIG_DIR
                 or ~/.claude
  --refresh      Ask again even if the last reading is younger than the TTL.
                 A pending retry window is still honoured.

Fields: health (ready|throttled|login_required|unavailable), five_hour and
seven_day (utilization, resets_at) from the last good reading or null, age_s
of that reading, retry_at when the endpoint may be asked again, error, and
token_expires_at: when the stored access token lapses. A lapsed token reads
as login_required until any claude run for that home refreshes it.
"#;

const RESOURCE_HELP: &str = r#"cc-hub resource

  accounts [--refresh]                      Show account health and quota windows
  list                                      Resources, who holds each, and the queue
  claim NAME... [--wait SECONDS]            (in a session) the complete set the work needs
  release [NAME...]                         (in a session) hand back some or all of it
  select --kind KIND --role ROLE            Preview a capacity-aware allocation
  start --task ID --kind KIND --role ROLE --cwd DIR --prompt TEXT
                                            Start the session working a task; a task
                                            with a live session in another role is
                                            handed over (new one starts, old one stops)
  status [--worker ID]                      Workers, their accounts and generations
  stop --worker ID [--reason TEXT]          End a worker's session
  retry --worker ID                         Requeue a blocked worker
  supervise                                 Refresh quota and reconcile workers once
  hook                                      PreToolUse: records the transcript, notes quota

Configuration: ~/.cc-hub/resources.toml. Python 3.11+ and tmux required.
One worker holds a task at a time. When its account reaches stop_percent the
supervisor stops it and continues it on another account from its transcript.
Default warning/start/stop ceilings are 80%/85%/95%.

A resource is a thing only one session may use at a time: a checkout, a phone.
[hosts.*] says where commands run, [resources.*] what lives on each host, and
routing says nothing about them — a session claims what its work needs while
it runs and is queued, first to ask first served, when it is taken. Holding is
derived from live workers, so a session that forgets to release holds nothing
once it ends.
"#;

const OPEN_HELP: &str = r#"cc-hub open

Usage:
  cc-hub open <cc-hub://url> [options]

Links:
  cc-hub://review?depth=<light|full>&pr=<pull request url>[&title=<text>]
      Spawn a session in the local checkout of the pull request's repository,
      name it "PR: <title>" (or "PR: <repo>#<n>"), and open it with "Let's do
      <depth> review of this PR: <url>". The checkout is found by repo name
      among bookmarks and the cwds of known sessions;
      without one the review runs from home, off the pull request alone.
      An optional &post=<1-100> lets the review post findings it is at least
      that confident about without asking.

  cc-hub://fix?pr=<pull request url>[&title=<text>][&kind=<word>]
      File the fix as a Tasks-board card — "Fix: <title>" (or "Fix:
      <repo>#<n>"), with the brief as its first note and <kind> as its kind
      (one of [tasks].kinds) — then spawn its session in the same checkout
      and open it with the standing orders for working through the pull
      request's review comments: switch to its branch, address every
      comment, track each as a Bitbucket task and close it once the fix is
      pushed, answer questions, ask when a comment is ambiguous, skip what is
      already done, sign every reply as Claude/Codex, and note "Pushed: …"
      on the card at the end. With subscription accounts configured the
      session is started through the resource broker (`cc-hub resource
      start`, role implementation, kind <word> or basic), so it runs on
      whichever account has room. The card reaches Running once its session
      is bound; it never waits in Planning, because the comments are the plan.

  cc-hub://task?id=<tk-…>[&dir=<path>][&kind=<word>][&role=<word>]
      Spawn a session for one Tasks-board card in <dir> (default: the card's
      own recorded cwd), name it "Task: <card>", open it with "/task --task
      <id> [--kind <word>] [--role <word>] <card text>", and bind the card to
      that session so `f` attaches to it. With a role the link is a hand-over:
      a fresh session starts and the card's previous one is closed once this
      command has reported. A hand-over needs a note on the card (the brief
      the next session works from; see `cc-hub help board`) and is refused
      without one. The card's status is left alone.

Options:
  --agent AGENT        Backend (default: [projects].default_session_agent)
  --wait-secs N        Prompt-dispatch readiness timeout (default: 120)
  --dry-run            Resolve cwd/prompt/agent, spawn nothing

Emits one JSON line with kind/tmux/cwd/agent_id/prompt/prompt_status.
Install the macOS URL-scheme handler with contrib/macos/install-link-handler.sh.
"#;

const AGENT_HELP: &str = r#"cc-hub agent

Persistent agents: a directory under ~/.cc-hub/agents/<name>/ with one
agent.toml. The TUI supervises every enabled agent: an event (a file in its
inbox, a poll command's stdout, or an interval) becomes one bounded
`claude -p` tick. Agents report back with `cc-hub agent note`. A spec may also name wakes
(`trigger.wake = ["board"]`): when one happens the poll runs on the next
second instead of at the end of interval_s, so interval_s is a floor under
the latency and not the latency. The board wakes `board` on every card
status change; anything else says so with `cc-hub wake <name>`.

Usage:
  cc-hub agent list [--json]
  cc-hub agent new <name> [--from DIR]        Scaffold <name>/agent.toml (+ work/, inbox/)
  cc-hub agent once <name> [--event TEXT | --event-file F] [--force]
                                              Run one tick now, print the outcome
  cc-hub agent poke <name> [--event TEXT | --event-file F]
                                              Drop an event into the inbox (any trigger kind)
  cc-hub agent pause <name>                   Skip this agent until resumed
  cc-hub agent resume <name>                  Resume; also clears a budget/failure halt
  cc-hub agent reset <name>                   Clear ticks/spend bookkeeping (workdir untouched)
  cc-hub agent show <name>                    Full state as JSON (history, notes)

Agent-facing (inside a tick, CC_HUB_AGENT is set):
  cc-hub agent note --text TEXT [--level info|warn] [--ref URL]

Layout of ~/.cc-hub/agents/<name>/:
  agent.toml   spec        work/   the agent's world     state.json  bookkeeping
  inbox/       events      notes.jsonl  outbox           log/        stream-json per tick
  events.jsonl harness log: runs, poll failures, halts, edits made in the hub
"#;

const BOARD_HELP: &str = r#"cc-hub board

The personal Tasks board (To-Do · In Progress · Review · Done), one card per
directory under ~/.cc-hub/tasks/.

Usage:
  cc-hub board add --text TEXT [--title TEXT] [--tags "a b"] [--priority p1|p2|p3|p4]
                   [--kind WORD]
      Mint a card in To-Do, exactly as the `a` key does in the TUI. Nothing
      is spawned and no status changes. Emits {"ok":true,"task_id":"tk-…"}.
      --kind is the deliverable the task router places the card by, and must
      be one of [tasks].kinds — the same list the board's `T` picker offers.
  cc-hub board note --task ID [--text TEXT]
      Append a note to the card: --text, or stdin when there is none (a
      heredoc for a long brief). The same `note` attachment the `p` key
      pastes; it shows on the card. Emits
      {"ok":true,"note":{…},"count":N,"status":"…"}.
      A note identical to the card's newest one is refused (exit 2): the
      record does not say the same thing twice in a row.
      A note that opens with `PR:` moves the card to Review — the column for
      work that wants reading, apart from work that wants an answer — which
      is why the status comes back with the note.
  cc-hub board notes --task ID [--json]
      The card's notes in attach order, each under a dated rule.

A card is how a script hands the user something to look at: mint it here,
then open it with `cc-hub open "cc-hub://task?id=<tk-…>&dir=<path>"`, which
only ever addresses a card that already exists (see `cc-hub help open`).

The notes are a task's record. The `task` skill writes the brief it agreed
with the user as one note, the branch it built as another, and what
verification found as a third; a session taking the card over reads them
first. A hand-over link (`&role=…`) is refused for a card with no note.
"#;

const BUILD_HELP: &str = r#"cc-hub build — builds of a `[builds.recipes.<name>]` recipe (the Builds tab)

Usage:
  cc-hub build start [--recipe R] [--cwd DIR] [--ref REF] [--route R] [--serve] [--wait]
  cc-hub build list
  cc-hub build cancel --build ID
  cc-hub build rebuild --build ID [--wait]
  cc-hub build serve --build ID
  cc-hub build reserve [--resource NAME]
  cc-hub build release [--resource NAME]

`start` queues a build and returns at once; `--wait` returns when it has
finished, exiting non-zero unless it succeeded. Without `--ref` the build takes
the working tree of `--cwd` (default: here) as it stands when the build starts.
Without `--route` the recipe picks one. `--recipe` may be left out when only one
recipe exists. `--serve` serves it the moment it succeeds.

`rebuild` builds a build's checkout again as it is now: the working tree and
the recipe's route, whatever ref or route the old build pinned.

A recipe builds one thing at a time, oldest first. A recipe with a `resource`
claims it before its first build, as the guest `Builds`, and keeps it after
the build ends, so a session cannot slip in between two builds. `reserve`
takes it ahead of any build, queueing behind whoever has it; `release` lets it
go. Space on the tab does whichever of the two applies.

Each build lives in ~/.cc-hub/builds/<id>/: build.json and output.log.
Every command prints one JSON line.
"#;
