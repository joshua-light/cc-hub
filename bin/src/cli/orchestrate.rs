//! `cc-hub orchestrate start` — spawn an orchestrator for an existing task.

use super::{
    parse_flags, print_json, report_prompt_status, require_task, resolve_project_id, CliError,
};
use cc_hub_lib::ops;

pub(crate) fn orchestrate_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("orchestrate <verb>: missing verb (try `start`)".into()))?;
    match verb.as_str() {
        "start" => orchestrate_start(rest),
        other => Err(CliError::Usage(format!(
            "unknown orchestrate verb: {} (try `start`)",
            other
        ))),
    }
}

/// `cc-hub orchestrate start --task ID [--project-id ID] [--wait-secs N]`
///
/// Spawns the selected orchestrator backend in the project root, waits up to `--wait-secs` (default
/// 60) for the new session to reach Idle, then dispatches the orchestrator
/// prompt as the first user message. Records the resulting tmux name in
/// state.json.
fn orchestrate_start(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let outcome = ops::task::orchestrate_start(
        &project_id,
        &task_id,
        f.agent.clone(),
        f.wait_secs,
        f.dry_run,
    )?;

    let spawn = match outcome {
        ops::task::OrchestrateStart::DryRun(prompt) => {
            // Useful for verifying prompt content without paying for a session.
            println!("{}", prompt);
            return Ok(());
        }
        ops::task::OrchestrateStart::Spawned(spawn) => spawn,
    };

    let prompt_status = report_prompt_status(&spawn.prompt_status);

    print_json(&serde_json::json!({
        "ok": true,
        "agent_id": spawn.state.orchestrator_agent_id,
        "agent_kind": spawn.state.orchestrator_agent_kind,
        "tmux": spawn.tmux,
        "cwd": spawn.state.project_root.as_deref().unwrap_or(std::path::Path::new("")).to_string_lossy(),
        "prompt_status": prompt_status,
        "task_id": task_id,
        "project_id": project_id,
    }));
    Ok(())
}
