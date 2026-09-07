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

## Replacement from the transcript

Each worker carries the `PreToolUse` hook, which records the provider session
id and transcript path and notes the account's quota in the tool context.
It blocks nothing. When the worker's account reaches `stop_percent` (or the
transcript shows a native usage-limit error) the supervisor stops the
session, puts the pool on cooldown until the window resets, selects another
account, and relaunches. A Claude worker resumes its own session: the
transcript is copied into the new account's `projects/` and the session id is
kept for the worker's whole life, so `--resume` continues where it stopped. A
Codex worker starts fresh with the transcript path in its prompt. No
checkpoint files, no model call to prepare.

Defaults: **80% warning** (noted to the worker), **85% start ceiling** (no new
allocation on that account), **95% stop** (replace). An unexplained exit is
`blocked`, not retried: inspect it, then `retry`. Attempts are bounded by
`max_attempts`.

## The card

The board shows the broker's state on the task card while it matters:
`waiting for subscription capacity`, `changing worker account`, `worker
needs recovery`. A running worker shows nothing; the card's Done state is the
user's.

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
