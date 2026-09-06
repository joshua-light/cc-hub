# Account routing and task continuity

Original design proposal, 2026-09-06. See [the implemented interface and its
boundaries](resource-management.md); the illustrative schema below is historical.

## Intended behavior

One hub manages two Claude subscriptions and two ChatGPT subscriptions.
Each task role has an allowed set of account/model/effort combinations.
Meetings always uses `cc-1`, with working Fathom access. Project Dev and QA
can each use Sonnet 5 Medium or GPT-5.6 Luna Medium on an eligible account.
When a worker exhausts quota, its task continues through a replacement
worker without losing the journal, worktree, evidence, or outstanding jobs.

## Example profiles

| Name | Provider | Local state | Status |
| --- | --- | --- | --- |
| `cc-1` | Claude | Default Claude home; unset `CLAUDE_CONFIG_DIR` | Default profile; verify required integrations |
| `cc-2` | Claude | `~/.claude-personal` | Separate profile; authenticate independently |
| `codex-1` | Codex | `~/.codex` | Default profile |
| `codex-2` | Codex | `~/.codex-personal` | Separate profile; authenticate independently |

Named shell aliases are conveniences. Hub launches must resolve structured
account settings directly, including environment removals, rather than rely
on interactive aliases or mutate the hub process's global environment.
In particular, unsetting Claude's override preserves the default
`~/.claude.json` location; explicitly setting it to `~/.claude` is not an
equivalent operation in the current hub path implementation.

Codex supports state isolation through `CODEX_HOME`. The second profile uses
file credential storage under its own home. Do not copy authentication state
between profiles. Login and verify each account separately, and detect when
two configured profiles actually represent the same subscription/workspace.
Count such profiles as one quota pool.

Skills, plugins, MCP configuration and connector authentication need their
own setup per profile. A new login alone does not reproduce the first
profile's tools. Do not symlink whole homes or credentials to share skills.

## Existing integration points

- `lib/src/config.rs`, `agent.rs`: configured backends have a kind, command,
  and model list, but no structured account reference.
- `lib/src/spawn.rs`: Claude launches inherit the hub's account selection.
  Codex launches have no explicit account selection. Keep model and effort
  in structured launch parameters rather than embedded command strings.
- `lib/src/platform/paths.rs`, scanners: Claude is process-profile scoped;
  Codex paths are fixed to `~/.codex`. Both need account-scoped enumeration,
  session ownership, transcript lookup and resume behavior.
- `lib/src/usage.rs`: Claude usage is fetched for the current profile and
  reads a credentials file. macOS Keychain credentials are not covered by
  that reader. Existing USD budgets are distinct from subscription quota.
- `lib/src/harness/spec.rs`, `runner.rs`: scheduled agents have model,
  effort and environment settings, but the runner builds Claude-only argv
  and parses Claude stream JSON. An account field alone cannot add Codex.
- Local Task Agent routers and QA definitions need a hub-managed role interface.
  These custom agents are maintained outside the repository.
- Task journals, `qa-handoff`, durable jobs and device leases already
  provide useful recovery primitives. Today rebinding the root invalidates
  QA state; distinguish logical role identity from replacement session ID.

## Configuration model

Keep four concepts distinct:

1. **Account:** provider, local home, authentication health, verified
   subscription identity, enabled state and usable capabilities.
2. **Execution profile:** provider model ID, reasoning effort, tool and
   permission policy. Validate model/effort against the selected runtime
   and account; preserve the exact requested choices.
3. **Role policy:** allowed profiles/accounts, required capabilities,
   optional hard account pin, selection strategy and quota reserve.
4. **Worker attempt:** task and role ID, selected account/model/effort,
   provider session ID, owner generation, timestamps and handoff reason.

Suggested configuration shape (illustrative; not accepted by today's parser):

```toml
[accounts.cc-1]
provider = "claude"
home_mode = "default"
capabilities = ["fathom"] # Only after verifying the connector works.

[accounts.cc-2]
provider = "claude"
home = "~/.claude-personal"

[accounts.codex-1]
provider = "codex"
home = "~/.codex"

[accounts.codex-2]
provider = "codex"
home = "~/.codex-personal"

[execution_profiles.sonnet-medium]
provider = "claude"
model = "claude-sonnet-5"
effort = "medium"
accounts = ["cc-1", "cc-2"]

[execution_profiles.luna-medium]
provider = "codex"
model = "gpt-5.6-luna"
effort = "medium"
accounts = ["codex-1", "codex-2"]

[routing.agents.meetings]
account = "cc-1"
requires = ["fathom"]
on_exhausted = "wait"

[routing.tasks.project.dev]
profiles = ["sonnet-medium", "luna-medium"]
strategy = "headroom"

[routing.tasks.project.qa]
profiles = ["sonnet-medium", "luna-medium"]
strategy = "headroom"
```

Resolve the most specific role policy first, then task-kind defaults, then
general defaults. Explicit pins and required capabilities remain hard
constraints. The two profile names are allowed alternatives, not a promise
that all subscriptions have those models or that their quota units match.
The task router and task coordinator can have separate policies from Dev/QA.

## Selection and quota tracking

Maintain snapshots per actual quota pool, with timestamps, all applicable
windows, reset times, provider/model-specific limits and current reservations.
Codex's app server exposes `account/rateLimits/read` and update notifications.
Build a Claude adapter around the existing usage reader, handling unavailable
credentials and telemetry explicitly; do not assume a supported equivalent
endpoint or keychain access until verified.

On allocation:

1. Filter by role policy, login health, verified tools, model/effort support
   and capacity. Never substitute an unlisted model or effort silently.
2. Refresh stale readings. Unknown usage is distinct from exhausted usage;
   permit at most a bounded trial when no known eligible capacity exists.
3. Compare headroom within provider quota windows. Across providers, use
   configurable weights and observed task throughput; percentages alone
   cannot tell how much equivalent work two subscriptions can perform.
4. Prefer healthy capacity while accounting for in-flight workers and an
   optional reserve on `cc-1` for meetings. Allocate under a lock/transaction
   so concurrent role requests cannot all reserve the same remaining slot.
5. Keep the current worker while healthy. Do not migrate for small changes
   in account ranking. Recheck exhausted accounts at reset with backoff.

If every permitted option is exhausted, persist `waiting_for_capacity` with
the earliest known reset and wake it automatically. A pinned meetings agent
waits for `cc-1`; it never silently loses Fathom by switching accounts.
Authentication failures need relogin; transient service throttles get bounded
backoff; context overflow and task budget caps are not subscription exhaustion.
Preserve existing task cost/time caps across replacements. Do not silently
fall through to API billing or paid overage.

## Continuing a task after exhaustion

Account changes happen between worker attempts, not by changing credentials
inside a live process. A logical task/role survives any number of attempts.
Native children normally inherit their parent's runtime/account: independently
routed QA/Dev must be hub-managed workers, with task-scoped messaging and
the same workflow gates on both providers.

Persist progress continuously: brief and constraints, phase, repository and
worktree, candidate revision, dirty changes, evidence, pending commands and
jobs, device leases, last completed operation, blockers and next action.
Do not depend on asking an already exhausted model to summarize its work.
Use recorded events and the existing journal as the recovery source.

On a quota error, stop new dispatch, reconcile in-flight tools and durable
jobs, and fence the old owner before granting the role to a replacement.
Use a generation token and an operation ledger to reject stale workers and
avoid repeating builds, PR creation or other external writes. Ambiguous
operations require checking the external result before retrying. Device
ownership and durable job ownership need explicit transfer/reconciliation.

Try native resume only when the runtime can access the original transcript
under the selected profile and that account-to-account path has been tested.
The baseline is a fresh session with the durable checkpoint. Claude-to-Codex
always uses this provider-independent handoff; no claim of native transcript
compatibility. Preserve historical evidence and reviewer attribution, but
require the replacement QA worker to explicitly greenlight the candidate.

## User-visible controls

- Accounts view: login status, verified tools, usage/reset/freshness per
  window, active workers and paused/unknown states.
- Role settings: allowed profiles, account pin and optional preference.
- Agent/task details: current account, model, effort and attempt history.
- Timeline events: e.g. "QA moved from cc-2 to codex-1 after quota exhaustion;
  continuing candidate abc123 from macOS build verification."
- Manual pause/drain and account-specific login commands. Draining prevents
  new assignments while allowing existing work to finish.

## Delivery sequence and acceptance checks

1. **Profiles and visibility:** account registry, explicit launch environment,
   per-account discovery/resume and meeting pin. Verify parallel launches
   use separate profiles and remain visible in one hub; default Claude
   state paths must remain compatible. Verify Fathom through the pinned
   profile before enabling the pin in the live agent.
2. **Role routing:** validated profile pools and hub-owned Dev/QA workers;
   Codex execution adapter and equivalent task tools/guards. Remove hardcoded
   Task Agent backend/model dependencies. Verify both providers carry out
   the same journal workflow and cannot bypass QA gates.
3. **Capacity-aware allocation:** provider probes, cached windows, reservations,
   cooldown/reset scheduling and account dashboard. Test stale/unknown usage,
   duplicate subscriptions, all-exhausted pools and simultaneous allocations.
4. **Recovery:** durable checkpoints and owner fencing, then automatic
   replacement. Inject exhaustion before a tool, during a durable build,
   and after an external operation succeeds but before its result is saved.
   Verify no duplicate operation, concurrent role owner, lost dirty work,
   skipped QA or budget reset. Include application restart during handoff.

## Sources checked

- [Codex authentication and credential stores](https://learn.chatgpt.com/docs/auth)
- [Codex state locations](https://learn.chatgpt.com/docs/config-file/config-advanced)
- [Codex account limit protocol](https://learn.chatgpt.com/docs/app-server)
- [Claude configuration locations](https://code.claude.com/docs/en/settings)
