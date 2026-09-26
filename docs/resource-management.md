# Resource broker

The resource broker decides which subscription account runs the session
working a task, and keeps that session alive across account limits. It ships
inside `cc-hub` as `cc-hub resource …` and needs Python 3.11+ and tmux. Its
configuration is `~/.cc-hub/resources.toml`;
[contrib/resources.toml](../contrib/resources.toml) is the annotated example.

- **Accounts** are separate Claude or Codex homes, each with its own login.
- **Profiles** pair an exact model and effort with the accounts that may run
  it.
- **Routing** picks profiles by task kind and role. The `task` skill has two
  roles, `implementation` and `verification`. A kind with no route of its
  own for a role falls back to `[routing.default.<role>]`; the broker
  refuses a role only when neither exists.
- **Hosts** are where commands run (`main`, `wh`). **Resources** are things
  on them only one session may use at a time, such as a checkout or a phone.
  Routing ignores resources: a session claims what its work needs.

## One session per task

A task has one worker at a time. `cc-hub resource start` returns the live
worker when one exists in the same role and folder. When the live worker has
another role or folder, `start` is a hand-over: it stops the old worker and
launches the new one. The card's notes carry the context
between them.

```sh
cc-hub resource start --task tk-ID --kind tps --role implementation --cwd /repo --prompt '…' [--title TEXT]
cc-hub resource status [--worker ID]
cc-hub resource stop --worker ID [--reason TEXT]
cc-hub resource retry --worker ID     # a blocked worker, once you know why it exited
cc-hub resource select --kind KIND --role ROLE   # preview an allocation
```

A worker's session is named at birth: `--title`, else the card's title,
else `Task: <card text>`. A Claude session is named before launch, a Codex
session once its id is known. cc-hub never overwrites a name, so a session
resumed on another account keeps a name you gave it.

A `cc-hub://task?…` link reaches the broker the same way: with accounts
configured, no `--agent`, and a known kind (`&kind=`, or an earlier broker
worker on the card), `cc-hub open` starts it through `resource start`.

## Resources

```toml
[hosts.main]
[hosts.wh]
ssh = "workhorse"

[resources.main-tps]
host = "main"
description = "TPS checkout + Unity Editor on this machine"
[resources.wh-tps]
host = "wh"
description = "TPS checkout + Unity Editor on the workhorse"
[resources.android]
host = "wh"
description = "Android phone, for installing and running a client build"
```

The config lists what exists and where, not who needs it. Only the session
doing the work knows that: a Grafana check and an on-device test are both
`tps` verification, and only one needs a phone. So a worker starts holding
nothing and claims what it needs:

```sh
cc-hub resource claim android --wait 300   # the complete set the work needs
cc-hub resource release [NAME...] [--as NAME]   # as soon as it is done, or to stop waiting
cc-hub resource list                       # who holds what, and the queue
```

A claim is all or nothing, first come first served: it is granted only when
every resource in it is free and nobody asked earlier for any of them.
Partial grants are how two sessions deadlock. For the same reason a claim
names the complete set; claiming a different set gives back what the worker
holds until the new set can be granted whole. A waiting card reads
`waiting for a resource`.

Holding follows the live sessions; there is no stored lock or lease. A
session that ends holds nothing, even if it forgot to release. A quota
replacement keeps what its predecessor held, because it continues the same
work.

Write each `description` for someone who knows the work, not the hardware:
it is the only thing that tells a session whether it needs the resource. A
resource on a remote host comes with the host's `ssh` name; the session still
runs in tmux here and reaches the host over SSH. Holding `android` does not
hold `wh-tps`, although both live on `wh`.

### Sessions the hub did not launch

A terminal, an editor's agent or a hand-started `claude` claims as a
**guest**. It names itself once; after that, its process is its identity:

```sh
cc-hub resource claim main-tps --as 'reviving TPS-21146' --wait 300
cc-hub resource release
```

A guest shares the workers' queue and shows in `resource list` under its
name. It gets no account, no card and no replacement; the hub only lends it
a resource. The hold ends with the claiming process: the broker walks up past
shells and `cc-hub` itself to find the session's process, and `--pid` names
it when that walk would guess wrong.

A release is found the same way, and when it runs from some other process —
an agent's commands do not always come from the one that claimed — `--as`
finds the guest by the name it claimed under instead. A release names what it
hands back and what it no longer waits for alike, so a claim still in the
queue is withdrawn by the same command that would have handed it back.

## Replacement from the transcript

Each worker runs with the broker's `PreToolUse` hook. It records the
provider session id and transcript path, and notes the account's quota for
the agent. It blocks nothing.

When the account reaches `stop_percent`, or the transcript shows a native
usage-limit error, the supervisor stops the session, puts the account's
quota pool on cooldown until its window resets, picks another account and
relaunches:

- **Claude to Claude** resumes the same session. The transcript is copied
  into the new account's `projects/`, and `--resume` continues it.
- **Every other pairing** (Codex, or across providers) hands off: a fresh
  session whose opening prompt names the old transcript. A generation that
  died before writing a transcript passes on the one it was given.

There are no checkpoint files and no model call to prepare. `cc-hub resource
supervise` runs one supervisor pass; the TUI's `R` key applies the same rules
by hand.

Defaults: warn the worker at 80%, start nothing new on an account at 85%,
replace at 95%. An unexplained exit marks the worker `blocked` rather than
retrying: inspect it, then `retry`. `max_attempts` bounds relaunches.

## Account health

`cc-hub resource accounts` shows each account's health and quota. A Claude
account can take a worker when `cc-hub usage --home DIR` reports it `ready`,
its usage is below `start_percent`, and fewer than `max_workers` of its
workers are live. A route may set its own `start_percent`; the higher of the
two applies:

```toml
[routing.prod-alert.implementation]
profiles = ["sonnet-medium-work", "sonnet-medium"]
start_percent = 94   # a page may spend the reserve ordinary work leaves alone
```

That only raises the start ceiling. It never lowers an account's reserve,
and `stop_percent` still replaces the worker.

The usage store reads Claude Code's OAuth access token and never refreshes
it; Claude Code does that on its next real request. So an idle account's
token lapses and would read `login_required` forever. The broker tells a
lapsed token from a missing login by `token_expires_at`: a lapsed token gets
one trivial `claude -p` on the cheapest model, and the account reads
`unknown` until the store asks again. A missing login stays
`login_required`.

## On the card

While it matters, the task card shows the broker's state:
`waiting for subscription capacity`, `changing worker account` or
`worker needs recovery`. A running worker shows nothing, and only you move
a card to Done.

## Setup

```sh
cargo build --release
python3 contrib/install-resources.py --binary target/release/cc-hub --share-tools --marketplace example-tools --watchdog
```

The installer copies the example config to `~/.cc-hub/resources.toml` if
none exists. `--share-tools` copies plugin and tool settings from the
default profiles to the second ones (`--marketplace` names which plugin
marketplaces to share; repeat it for more). It never copies credentials, so
each account keeps its own login. `--watchdog` (macOS only) installs the
launchd job `local.cc-hub.resources`, which runs `cc-hub resource supervise`
every 30 seconds, so replacement works while the TUI is closed.

Every account also appears as a hub agent under its own name. An
`[agents.<id>]` table pins an agent to an account with `account = "cc-1"`,
and a persistent agent pins one with `[run].account`.

## Tests

```sh
python3 -m unittest discover -s broker/tests -p 'test_resource*.py'
cargo test --workspace --no-fail-fast
```

The tmux test launches fake providers on a private socket and spends no
quota. It checks profile isolation and a real replacement that resumes the
first generation's transcript.
