//! Help text for `cc-hub help [topic]` and `--help`.

use super::CliError;

pub(crate) fn print_cli_help(topic: &[String]) -> Result<(), CliError> {
    match topic.first().map(String::as_str) {
        None => print!("{}", GENERAL_HELP),
        Some("spawn-worker") => print!("{}", SPAWN_WORKER_HELP),
        Some("merge-worktree") => print!("{}", MERGE_WORKTREE_HELP),
        Some("task") => print!("{}", TASK_HELP),
        Some("orchestrate") => print!("{}", ORCHESTRATE_HELP),
        Some("pr") => print!("{}", PR_HELP),
        Some("worker") => print!("{}", WORKER_HELP),
        Some("project") => print!("{}", PROJECT_HELP),
        Some("open") => print!("{}", OPEN_HELP),
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

Orchestrator-facing topics:
  spawn-worker      Spawn a readonly or worktree worker for a task
  merge-worktree    Legacy direct worktree merge helper
  task              Create/report/start tasks, artifacts, and todos
  orchestrate       Spawn an orchestrator for an existing task
  pr                Local PR review/merge flow
  worker            Wait for worker sessions to finish
  project           List registered projects

Desktop-facing topics:
  open              Act on a cc-hub:// deep link (start a PR review session)

Examples:
  cc-hub task create --backlog --prompt "Fix the flaky test"
  cc-hub task start --task t-123 --agent claude
  cc-hub spawn-worker --task t-123 --worktree fix --prompt "Implement the fix"
  cc-hub pr show --task t-123
"#;

const OPEN_HELP: &str = r#"cc-hub open

Usage:
  cc-hub open <cc-hub://url> [options]

Links:
  cc-hub://review?depth=<light|full>&pr=<pull request url>[&title=<text>]
      Spawn a session in the local checkout of the pull request's repository,
      name it "PR: <title>" (or "PR: <repo>#<n>"), and open it with "Let's do
      <depth> review of this PR: <url>". The checkout is found by repo name
      among registered projects, bookmarks, and the cwds of known sessions.

Options:
  --agent AGENT        Backend (default: [projects].default_session_agent)
  --wait-secs N        Prompt-dispatch readiness timeout (default: 120)
  --dry-run            Resolve cwd/prompt/agent, spawn nothing

Emits one JSON line with kind/tmux/cwd/agent_id/prompt/prompt_status.
Install the macOS URL-scheme handler with contrib/macos/install-link-handler.sh.
"#;

const SPAWN_WORKER_HELP: &str = r#"cc-hub spawn-worker

Usage:
  cc-hub spawn-worker --task ID (--worktree NAME | --readonly) [options]

Options:
  --project-id ID      Override inferred project id
  --agent AGENT        Worker backend (defaults to task orchestrator agent)
  --prompt TEXT        Initial prompt to send to the worker
  --wait-secs N        Prompt-dispatch readiness timeout (default: 120)

Emits one JSON line with tmux/cwd/worktree/prompt_status.
"#;

const MERGE_WORKTREE_HELP: &str = r#"cc-hub merge-worktree

Usage:
  cc-hub merge-worktree --task ID --worktree NAME [--project-id ID]

Legacy helper that merges a worker branch into the project's main branch and
records a MergeRecord. New PR-flow tasks generally use `cc-hub pr merge`.
"#;

const TASK_HELP: &str = r#"cc-hub task

Usage:
  cc-hub task create --prompt TEXT [--backlog] [--name NAME] [--project-id ID]
  cc-hub task start --task ID [--agent AGENT] [--wait-secs N] [--project-id ID]
  cc-hub task report --task ID [--status running|review|merging|done|backlog] [--note TEXT] [--summary TEXT]
  cc-hub task show --task ID [--project-id ID] [--json]
  cc-hub task delete --task ID [--project-id ID] [--force]
  cc-hub task gc [--project-id ID] [--dry-run]
  cc-hub task auto-review --task ID [--project-id ID]
  cc-hub task list [--status backlog|running|review|merging|done] [--project-id ID] [--json]
  cc-hub task artifact add --task ID --path PATH_OR_URL [--kind KIND] [--caption TEXT] [--lead]
  cc-hub task artifact list --task ID [--project-id ID]
  cc-hub task todos set --task ID --items JSON_ARRAY
  cc-hub task todos check|uncheck --task ID --index N
  cc-hub task todos clear --task ID

All mutating verbs emit one JSON line. `report --status done` routes a running
task into Review first so a human/reviewer can approve it.
"#;

const ORCHESTRATE_HELP: &str = r#"cc-hub orchestrate

Usage:
  cc-hub orchestrate start --task ID [--agent AGENT] [--wait-secs N] [--dry-run]

Spawns the configured orchestrator backend in the task's project root, persists
its tmux session name, and sends the generated orchestrator prompt.
"#;

const PR_HELP: &str = r#"cc-hub pr

Usage:
  cc-hub pr create --task ID --worktree NAME --title TEXT [--description TEXT]
  cc-hub pr show --task ID
  cc-hub pr approve --task ID
  cc-hub pr request-changes --task ID --comment TEXT [--author NAME]
  cc-hub pr reopen --task ID [--comment TEXT] [--author NAME]
  cc-hub pr comment --task ID --comment TEXT [--author NAME]
  cc-hub pr close --task ID [--project-id ID] [--comment TEXT] [--author NAME]
  cc-hub pr merge --task ID [--wait [--timeout-secs N]]
  cc-hub pr continue --task ID [--project-id ID]
  cc-hub pr lock-phase --task ID --phase merging|simplify|bump|finalize-pending
  cc-hub pr finalize --task ID [--build-cmd CMD] [--skip-build] [--keep-tmux]

Local PR records live beside task state. Merges are serialized with the
project merge lock; `finalize` releases the lock and marks the task Done.

`pr merge` acquires the project merge lock. If another task holds it, the
default is to fail fast with `{ok:false, locked:true, ...}`; pass `--wait` to
block until the lock frees (bounded by `--timeout-secs N`, default 1800).

`pr continue` re-pings the task's orchestrator with the merge-flow prompt
(the same one the TUI sends on approve). Idempotent — safe to re-run. If the
orchestrator session is dead it reports `{ok:false, orchestrator_alive:false}`
with a recipe to resurrect or `task delete --force` the wedged task.
"#;

const WORKER_HELP: &str = r#"cc-hub worker

Usage:
  cc-hub worker wait --task ID (--tmux NAME ... | --worktree NAME ... | --all)
                     [--timeout-secs N] [--progress [--progress-interval-secs N]]

Polls cc-hub's session scanner until selected workers reach WaitingForInput,
Question, or Inactive. Emits one JSON line with per-worker completion state.

With --progress, emits one JSON line every N seconds (default 5) describing
which targets are still pending vs. done. The final summary line is unchanged.
"#;

const PROJECT_HELP: &str = r#"cc-hub project

Usage:
  cc-hub project list [--json]

Lists registered projects. Plain output is tab-separated:
  <id>\t<name>\t<root>
With --json, includes per-status task counts.
"#;
