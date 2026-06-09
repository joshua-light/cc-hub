//! `cc-hub spawn-worker` — spawn a readonly or worktree worker for a task.

use super::{
    parse_flags, print_json, report_prompt_status, require_task, resolve_project_id, CliError,
};
use cc_hub_lib::ops;

pub(crate) fn spawn_worker(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let outcome = ops::worker::spawn_worker(
        &project_id,
        &task_id,
        ops::worker::SpawnWorkerOpts {
            worktree: f.worktree.clone(),
            readonly: f.readonly,
            prompt: f.prompt.clone(),
            agent: f.agent.clone(),
            wait_secs: f.wait_secs,
        },
    )?;

    let prompt_status = report_prompt_status(&outcome.prompt_status);

    print_json(&serde_json::json!({
        "ok": true,
        "agent_id": outcome.agent_id,
        "agent_kind": outcome.agent_kind,
        "tmux": outcome.tmux,
        "cwd": outcome.cwd,
        "worktree": outcome.worktree,
        "readonly": outcome.readonly,
        "prompt_status": prompt_status,
        "task_id": task_id,
        "project_id": project_id,
    }));
    Ok(())
}
