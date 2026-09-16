# Subscription routing

The resource broker is bundled in `cc-hub` (`cc-hub resource …`, Python 3.11+
and tmux). It answers one question: **which account does the session working
this task run on**, and it keeps that session alive across account limits.
The configuration is `~/.cc-hub/resources.toml`; see
[the example](../contrib/resources.toml).

- **Accounts** are separate Claude or Codex homes with their own login.
- **Profiles** pair an exact model and effort with the accounts that may run it.
- **Routing** picks profiles by task kind and role: `implementation` or
  `verification`, the two roles of the `task` skill.
- **Hosts** are the places commands can run (`main`, `wh`), and **resources**
  are the things on them that only one session may use at a time (a checkout,
  a phone). Routing says nothing about resources: a session claims what its
  work needs.

## One session per task

A task has one worker at a time. `cc-hub resource start` for a task that
already has a live worker in the *same* role returns it. For a task whose
live worker is in *another* role it is a hand-over: the new worker is
launched and the old one is stopped by the next supervisor tick. That is the
whole protocol between the two roles; the card's notes carry the context.

```sh
cc-hub resource start --task tk-ID --kind tps --role implementation --cwd /repo --prompt '…'
cc-hub resource status [--worker ID]
cc-hub resource stop --worker ID [--reason TEXT]
cc-hub resource retry --worker ID          # a blocked worker, after inspecting why it exited
```

A `cc-hub://task?…&role=…` link is the same hand-over from the board's side:
`cc-hub open` starts the new role through the broker when accounts are
configured, then closes the card's previous session once it has reported.

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

A resource is a thing only one session may use at a time. The config says
which exist and on which host; it does not say who needs one, because only the
session doing the work knows that — a Grafana check and an on-device test are
both `tps` verification, and one of them needs a phone. So a worker launches
holding nothing and asks for what it needs:

```sh
cc-hub resource claim android --wait 300   # the complete set the work needs
cc-hub resource release                    # as soon as it is done
cc-hub resource list                       # who holds what, and the queue
```

A claim is granted only if every resource in it is free and nobody asked
earlier for any of them — first to ask, first served. All of it or none: a
half-granted claim is how two sessions deadlock, each holding what the other
waits for. For the same reason a claim is the *complete* set, so claiming a
different one hands back what the worker holds until the new set can be
granted in full. A waiting card reads `waiting for a resource`.

Holding is derived from the live workers, never stored as a lock: a session
that forgets to release holds nothing the moment it ends, and there is no
lease to expire. A quota replacement keeps what its predecessor held, because
it continues the same work.

The `description` is the only thing that tells a session whether it needs the
thing, so write it for a reader who knows the work and not the hardware. A
resource on a remote host comes with that host's `ssh` name; the session
itself still runs in tmux on this machine and reaches the host over SSH.
Holding `android` does not hold `wh-tps`: the phone hangs off that machine,
but using it does not take the checkout beside it.

## Replacement from the transcript

Each worker carries the `PreToolUse` hook, which records the provider session
id and transcript path and notes the account's quota in the tool context.
It blocks nothing. When the worker's account reaches `stop_percent` (or the
transcript shows a native usage-limit error) the supervisor stops the
session, puts the pool on cooldown until the window resets, selects another
account, and relaunches. What the successor does depends on both sides of
the pairing. Claude→Claude resumes the same session: the transcript is copied
into the new account's `projects/` and the session id is kept, so `--resume`
continues where it stopped. Every other pairing — Codex anywhere, or across
providers — is a hand-off: a fresh session, with a new id, whose opening
prompt names the old transcript. A generation that never wrote a transcript
of its own (it died before its first tool call) passes on the one it was
handed. No checkpoint files, no model call to prepare.

Defaults: **80% warning** (noted to the worker), **85% start ceiling** (no new
allocation on that account), **95% stop** (replace). An unexplained exit is
`blocked`, not retried: inspect it, then `retry`. Attempts are bounded by
`max_attempts`.

## The card

The board shows the broker's state on the task card while it matters:
`waiting for subscription capacity`, `changing worker account`, `worker
needs recovery`. A running worker shows nothing; the card's Done state is the
user's.

## Account health

A Claude account is allocatable when `cc-hub usage --home DIR` reports it
`ready`, its usage sits below `start_percent`, and fewer than `max_workers`
of its workers are live. The store only reads the OAuth access token Claude
Code keeps; it never refreshes one. Claude Code refreshes on its next real
request, so an account nothing has run on for a few hours holds a lapsed
token and would read as `login_required` forever. The probe tells the two
apart by the report's `token_expires_at`: a lapsed token gets one trivial
`claude -p` on that home (the cheapest model, one turn) and the account is
`unknown` until the store re-asks; a missing login stays `login_required`.

## Install

```sh
cargo build --release
python3 contrib/install-resources.py --binary target/release/cc-hub --share-tools --marketplace example-tools --watchdog
```

`--share-tools` copies plugin and tool configuration into the second profiles
without copying credentials; login stays independent. `--watchdog` installs
`local.cc-hub.resources`, a launchd job that runs `supervise` every 30 seconds
so replacement works while the TUI is closed.

Account IDs also appear as ordinary hub backends (`[agents.NAME] account =
"cc-1"`), and scheduled agents can pin `[run].account`.

## Verification

```sh
python3 -m unittest discover -s lib/tests -p 'test_resource*.py'
cargo test --workspace --no-fail-fast
```

The tmux test launches fake providers on a private socket and consumes no
quota: it checks profile isolation and a real replacement that resumes the
first generation's transcript.
