# Subscription routing and proactive handoff

The resource broker is bundled in `cc-hub`. It uses Python 3.11+ and tmux on
Unix; the supplied OS supervisor installer targets macOS. The configuration
is `~/.cc-hub/resources.toml`; see [the example](../contrib/resources.toml).

Accounts identify separate Claude/Codex homes. Execution profiles pair an
exact model and effort with eligible accounts. Routing policies select profiles
by task kind and role, with optional hard account pins and required capabilities.
Project uses distinct `qa-editor` and `qa-build` workers, both inheriting the `qa` routing
policy. Editor QA passes the candidate before root hands journal ownership to Build QA,
which alone queues builds (except recorded build-only requirements).
Project Dev and QA are independently configured for Sonnet 5 Medium or GPT-5.6
Luna Medium. No unlisted model/effort is substituted.

## Install

```sh
cargo build --release
python3 contrib/install-resources.py --binary target/release/cc-hub --share-tools --marketplace example-tools --watchdog
```

`--marketplace` selects which plugin marketplaces to share (none by default).
`--share-tools` adds missing selected plugin/local tool configuration to the second
profiles and shares versioned plugin code. It preserves their existing settings,
backs up changed files and does not copy subscription credentials, bearer tokens
or account-bound connector state. Login remains independent. Plugins may still
need their own setup when an account does not have the required integration.

Custom Task Agent definitions and workflow scripts are local extensions and are
not distributed in this repository. Install and maintain them separately. A local
router can invoke `cc-hub resource start` to allocate a managed Dev role.
CLI task deep links with a known kind (explicit or already managed) also route
through the broker. New unclassified links retain their manual workflow. Explicit `--agent`
deep links remain manual backend overrides.

The launchd job `local.cc-hub.resources` reconciles every 30 seconds even when
the TUI is closed. Reopen an already-running hub to load account discovery and
the new task status labels. Native agent sessions survive closing the TUI.

## Accounts and scheduled agents

```sh
cc-hub resource accounts --refresh
cc-hub resource select --kind project --role qa
cc-hub resource status
```

Account probes return login health, quota windows/reset times and freshness.
Codex models/efforts are checked against the account's model list. Claude's
allowed models/efforts are declared in its account configuration. Claude uses
its profile credential file or macOS Keychain; an explicit `keychain_service`
can override the installed CLI's profile-hash naming convention. A usage 401
requires refreshing that profile's login; it is not available capacity.

Selection filters unavailable models, missing capabilities, stale/unknown
telemetry, exhausted windows and cooldowns. Subscription fingerprints share
reservations across duplicate profiles. A lock prevents competing allocations
from exceeding configured worker slots. Headroom, configured account weights
and an in-flight penalty are heuristics; percentages are not equivalent work
budgets across providers. No fallback to API billing is introduced.

Account IDs also appear as ordinary hub backends. Existing backend definitions
can set `account = "cc-1"` and `effort = "medium"` in `[agents.NAME]`.
Configured account homes are scanned and sessions receive account labels.

Scheduled Claude agents can pin `[run].account = "cc-1"`. The compatible form
`[run.env] CC_HUB_ACCOUNT = "cc-1"` is also recognized by the new runner and
can be stored while an older hub is open. Meetings should retain that hard pin:
Fathom credentials/capabilities are account-specific. Scheduled tick execution
still uses Claude's protocol; Codex is supported for interactive managed roles,
not as a drop-in replacement for the existing scheduled Claude tick protocol.

## Worker contract

```sh
cc-hub resource start --task tk-ID --kind project --role dev --cwd /repo --prompt 'Task brief'
cc-hub resource start --task tk-ID --kind project --role qa --cwd /repo --prompt 'QA assignment'
cc-hub resource checkpoint --file /path/handoff.md
cc-hub resource handoff --file /path/handoff.md --reason quota-reserve
cc-hub resource complete
```

Every managed worker gets the resource instructions and a PreToolUse hook.
Native child spawning is rejected on supported tool paths: independent roles
must use `resource start`. The same Task workflow guard applies to both providers.
Helpers are children for permission/QA gating and cannot act as the task root.
Logical root/actor IDs remain stable while provider UUIDs and execution
generations change. Old generations are rejected at tool boundaries.

Defaults are **80% warning**, **85% reserve**, **95% emergency handoff**.
At the warning the worker should finish only the current safe step, record its
checkpoint and request replacement. At the reserve boundary ordinary tool calls
are stopped; recovery controls remain usable. These thresholds are configurable.
`handoff` without `--file` can capture terminal/git state when the worker cannot
write a fresh summary. Recovery never requires another model response.

The supervisor marks the old owner stopping and kills its tmux session before
allocating the replacement. The worktree, dirty files, journal, checkpoint,
transcripts and device leases are retained. No permitted capacity means a durable
queue, surfaced on the task card. Attempt count is retained and bounded across
replacements. Authentication/network/process exits are not guessed to be quota
errors; unexplained exits are blocked for inspection.

QA replacement idempotently issues a new durable assignment and clears its
greenlight, keeping prior candidate/scenario evidence. It must reconcile and
acknowledge that assignment before continuing. Durable-job notifications follow
the root's new tmux. Worker messages persist until acknowledged and idle workers
are nudged to read their inbox. Critical QA instructions still use `qa-instruct`.

Handoff does not roll back an in-flight external operation or guarantee
exactly-once external writes. The broker does not replay commands; the replacement
must inspect pending jobs and remote results before retrying an uncertain action.
The hooks enforce supported tool paths and the existing workflow gates; they
are not a sandbox for arbitrary programs started outside the managed workflow.

## Verification

```sh
python3 -m unittest discover -s lib/tests -p 'test_resource*.py'
cargo test --workspace --no-fail-fast
```

Tests cover concurrent allocation, pins, capacity/freshness/model filtering,
duplicate subscription reservations, stale generations, recovery command
boundaries, emergency replacement without a final model response, profile setup
without credential copying, preserved board status, and real tmux launch/replacement on a private test socket. The tmux
test uses fake provider executables and consumes no subscription quota.
