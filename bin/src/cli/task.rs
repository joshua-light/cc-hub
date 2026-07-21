//! `cc-hub task ...` — create/report/start/list/show/delete/gc tasks, plus
//! the `artifact` and `todos` subgroups.

use super::pr::pr_to_json;
use super::{
    parse_flags, print_json, report_prompt_status, require_task, resolve_project_id, CliError,
};
use cc_hub_lib::models;
use cc_hub_lib::ops;
use cc_hub_lib::orchestrator::{self, TaskState, TaskStatus, TodoItem};
use std::path::PathBuf;

const TASK_VERBS_HELP: &str =
    "`report`, `create`, `start`, `list`, `show`, `delete`, `gc`, `auto-review`, `artifact`, or `todos`";

pub(crate) fn task_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage(format!(
            "task <verb>: missing verb (try {})",
            TASK_VERBS_HELP
        ))
    })?;
    match verb.as_str() {
        "report" => task_report(rest),
        "create" => task_create(rest),
        "start" => task_start(rest),
        "show" => task_show(rest),
        "list" => task_list(rest),
        "delete" => task_delete(rest),
        "gc" => task_gc(rest),
        "auto-review" => task_auto_review(rest),
        "artifact" => task_artifact_subcommand(rest),
        "todos" => task_todos_subcommand(rest),
        other => Err(CliError::Usage(format!(
            "unknown task verb: {} (try {})",
            other, TASK_VERBS_HELP
        ))),
    }
}

/// `cc-hub task delete --task ID [--project-id ID] [--force]`
///
/// End-to-end teardown: kills the orchestrator tmux (best-effort), removes
/// every worktree the task owns, then deletes the on-disk task state dir.
/// Requires `--force` for `Running` / `Review` / `Merging` so the user has to
/// acknowledge they're killing live work. `Merging` is recoverable under
/// `--force`: `delete_task` releases the merge lock so a wedged task (e.g. a
/// dead orchestrator that never ran `pr finalize`) doesn't leave the project
/// merge lock held forever.
fn task_delete(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let deleted = ops::task::task_delete(&project_id, &task_id, f.force)?;

    let worktree_errors: Vec<serde_json::Value> = deleted
        .worktree_errors
        .iter()
        .map(|(path, err)| {
            serde_json::json!({
                "path": path,
                "error": err,
            })
        })
        .collect();

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": deleted.task_id,
        "project_id": deleted.project_id,
        "orchestrator_killed": deleted.orchestrator_killed,
        "lock_released": deleted.lock_released,
        "state_removed": deleted.state_removed,
        "worktrees_removed": deleted.worktrees_removed,
        "worktree_errors": worktree_errors,
    }));
    Ok(())
}

/// `cc-hub task gc [--project-id ID] [--dry-run]`
///
/// Sweep orphaned worktrees under `<root>/.cc-hub-wt/`. Worktrees + their
/// `cc-hub/*` branches are otherwise only torn down on the Done path, so
/// Review / abandoned / wedged tasks leak them. This removes every worktree
/// no live (present, non-Done) task owns, deletes their dangling branches, and
/// runs `git worktree prune`. `--dry-run` prints the plan as JSON without
/// acting.
fn task_gc(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    // No `--task` for gc; resolve_project_id falls back to the cwd-derived id.
    let project_id = resolve_project_id(&f)?;
    let project_root = resolve_project_root(&project_id)?;

    let outcome = ops::task::task_gc(&project_id, &project_root, f.dry_run)?;

    let orphans: Vec<serde_json::Value> = outcome
        .orphans
        .iter()
        .map(|w| {
            serde_json::json!({
                "dir_name": w.dir_name,
                "path": w.path.to_string_lossy(),
                "branch": w.branch,
            })
        })
        .collect();
    let live: Vec<serde_json::Value> = outcome
        .live
        .iter()
        .map(|w| {
            serde_json::json!({
                "dir_name": w.dir_name,
                "path": w.path.to_string_lossy(),
                "branch": w.branch,
            })
        })
        .collect();
    let errors: Vec<serde_json::Value> = outcome
        .errors
        .iter()
        .map(|(path, err)| serde_json::json!({ "path": path, "error": err }))
        .collect();

    print_json(&serde_json::json!({
        "ok": true,
        "project_id": project_id,
        "dry_run": f.dry_run,
        "orphans": orphans,
        "live": live,
        "worktrees_removed": outcome.worktrees_removed,
        "branches_removed": outcome.branches_removed,
        "errors": errors,
        "pruned": outcome.pruned,
    }));
    Ok(())
}

/// Resolve a registered project's root by id, falling back to the cwd when the
/// id matches the cwd-derived id (so `task gc` works from a project dir that
/// hasn't been explicitly registered yet, mirroring `resolve_project_id`'s
/// cwd fallback).
fn resolve_project_root(project_id: &str) -> Result<PathBuf, CliError> {
    if let Some(p) = orchestrator::load_projects()
        .projects
        .into_iter()
        .find(|p| p.id == project_id)
    {
        return Ok(p.root);
    }
    let cwd = std::env::current_dir().map_err(|e| CliError::Other(format!("cwd: {}", e)))?;
    if orchestrator::project_id_for_path(&cwd) == project_id {
        return Ok(cwd);
    }
    Err(CliError::NotFound(format!(
        "project {} is not registered and does not match the current directory; pass --project-id for a registered project",
        project_id
    )))
}

/// `cc-hub task start --task ID [--project-id ID] [--agent ID] [--wait-secs N]`
///
/// Flip a Backlog task to Running and spawn its orchestrator. Mirrors
/// `orchestrate start` but goes through `start_backlog_task` so it errors
/// cleanly if the task isn't in Backlog.
fn task_start(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let spawn = ops::task::task_start(&project_id, &task_id, f.agent.clone(), f.wait_secs)?;

    let prompt_status = report_prompt_status(&spawn.prompt_status);

    // Echo agent_id/agent_kind/cwd alongside the tmux name to match
    // `spawn-worker`'s output shape so orchestrators get a uniform envelope.
    print_json(&serde_json::json!({
        "ok": true,
        "agent_id": spawn.state.orchestrator_agent_id,
        "agent_kind": spawn.state.orchestrator_agent_kind,
        "cwd": spawn.state.project_root.as_deref().unwrap_or(std::path::Path::new("")).to_string_lossy(),
        "tmux": spawn.tmux,
        "prompt_status": prompt_status,
        "task_id": task_id,
        "project_id": project_id,
    }));
    Ok(())
}

/// Seconds since `unix_secs`, clamped at 0 for clock skew.
fn age_secs(unix_secs: i64) -> u64 {
    (orchestrator::now_unix_secs() - unix_secs).max(0) as u64
}

/// Parse an optional `--status` flag into a `TaskStatus`, surfacing a uniform
/// usage error for unknown values. `None` flag → `Ok(None)`.
fn parse_status_flag(raw: Option<&str>) -> Result<Option<TaskStatus>, CliError> {
    match raw {
        None => Ok(None),
        Some(s) => s.parse::<TaskStatus>().map(Some).map_err(|_| {
            CliError::Usage(format!(
                "--status must be backlog|planning|running|review|merging|done (got {})",
                s
            ))
        }),
    }
}

/// `cc-hub task list [--status STATUS] [--project-id ID] [--json]`
///
/// Enumerate tasks for a project by reading `~/.cc-hub/projects/<pid>/tasks/*/state.json`
/// directly — `projects_scan::scan` only sees projects registered in
/// `projects.toml`, so this verb works for ad-hoc/unregistered projects too.
fn task_list(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let project_id = resolve_project_id(&f)?;

    let filter = parse_status_flag(f.status.as_deref())?;

    let Some(project_dir) = orchestrator::project_state_dir(&project_id) else {
        return Err(CliError::Other("no home dir".into()));
    };
    let tasks_dir = project_dir.join("tasks");

    let mut tasks: Vec<TaskState> = Vec::new();
    match std::fs::read_dir(&tasks_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let task_id = name.to_string_lossy().into_owned();
                // `t-` = project-born, `tk-` = promoted from the
                // personal board; both are valid task dirs here.
                if !task_id.starts_with("t-") && !task_id.starts_with("tk-") {
                    continue;
                }
                match orchestrator::read_task_state(&project_id, &task_id) {
                    Ok(state) => tasks.push(state),
                    Err(e) => {
                        eprintln!("warning: skipping {}: {}", task_id, e);
                    }
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(CliError::Other(format!(
                "read tasks dir {}: {}",
                tasks_dir.display(),
                e
            )));
        }
    }

    if let Some(ref s) = filter {
        tasks.retain(|t| &t.status == s);
    }

    tasks.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then(b.task_id.cmp(&a.task_id))
    });

    if f.json {
        let arr: Vec<serde_json::Value> = tasks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "task_id": t.task_id,
                    "status": t.status,
                    "title": t.title,
                    "prompt": t.prompt,
                    "note": t.note,
                    "updated_at": t.updated_at,
                    "shipped_version": t.shipped_version,
                })
            })
            .collect();
        print_json(&serde_json::json!({ "ok": true, "tasks": arr }));
    } else {
        for t in &tasks {
            let preview = match t.title.as_deref() {
                Some(s) => s.to_string(),
                None => {
                    let first = t.prompt.lines().next().unwrap_or("").trim_end();
                    if first.chars().count() > 60 {
                        let truncated: String = first.chars().take(60).collect();
                        format!("{}…", truncated)
                    } else {
                        first.to_string()
                    }
                }
            };
            println!(
                "{}\t{}\t{}\t{}",
                t.task_id,
                t.status.as_str(),
                preview,
                models::relative_age_short(age_secs(t.updated_at))
            );
        }
    }

    Ok(())
}

fn task_report(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let raw_status = parse_status_flag(f.status.as_deref())?;

    let outcome = ops::task::task_report(
        &project_id,
        &task_id,
        ops::task::ReportOpts {
            status: raw_status,
            note: f.note.clone(),
            summary: f.summary.clone(),
        },
    )?;
    let state = outcome.state;

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "status": state.status,
        "requested_status": outcome.requested_status,
        "note": state.note,
        "summary": state.summary,
        "shipped_version": state.shipped_version,
        "updated_at": state.updated_at,
    }));
    Ok(())
}

/// `cc-hub task show --task ID [--project-id ID] [--json]`
///
/// Read-only inspection of a single task. With `--json`, emits one JSON
/// object on stdout containing the full TaskState verbatim at the top level
/// plus `ok: true` and an embedded `pr` field (the PR object, or null).
/// Without `--json`, prints key/value lines suitable for skim-reading.
fn task_show(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;
    let pr = cc_hub_lib::pr::read_pr(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("read pr: {}", e)))?;

    if f.json {
        // Top-level shape: full TaskState fields, plus `ok` and `pr`.
        let mut obj = serde_json::to_value(&state)
            .map_err(|e| CliError::Other(format!("serialize state: {}", e)))?;
        let map = obj
            .as_object_mut()
            .expect("TaskState serializes to a JSON object");
        map.insert("ok".into(), serde_json::Value::Bool(true));
        map.insert(
            "pr".into(),
            match &pr {
                Some(p) => pr_to_json(p),
                None => serde_json::Value::Null,
            },
        );
        print_json(&obj);
        return Ok(());
    }

    let todos_done = state.todos.iter().filter(|t| t.done).count();
    let todos_total = state.todos.len();

    println!("status: {}", state.status.as_str());
    println!(
        "prompt: {}",
        models::first_line_truncated(&state.prompt, 80)
    );
    println!("note: {}", state.note.as_deref().unwrap_or("-"));
    println!(
        "summary: {}",
        state
            .summary
            .as_deref()
            .map(|s| models::first_line_truncated(s, 120))
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "created_at: {}",
        models::relative_age(age_secs(state.created_at))
    );
    println!(
        "updated_at: {}",
        models::relative_age(age_secs(state.updated_at))
    );
    println!("workers: {}", state.workers.len());
    println!("artifacts: {}", state.artifacts.len());
    println!("todos: {}/{}", todos_done, todos_total);
    println!(
        "shipped_version: {}",
        state.shipped_version.as_deref().unwrap_or("-")
    );
    println!(
        "orchestrator_tmux: {}",
        state.orchestrator_tmux.as_deref().unwrap_or("-")
    );
    Ok(())
}

/// `cc-hub task auto-review --task ID [--project-id ID]`
///
/// Re-arm the auto-reviewer for the current Review round by clearing
/// `last_auto_reviewed_at`. The next `auto_review::tick` will then re-pick
/// the task. Useful after fixing a misconfig (e.g. a stale agent setting)
/// when the user wants another pass without waiting for a fresh round.
fn task_auto_review(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    ops::task::task_auto_review(&project_id, &task_id)?;

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": task_id,
        "project_id": project_id,
        "cleared": true,
    }));
    Ok(())
}

// ─── task artifact ───────────────────────────────────────────────────────

fn task_artifact_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage("task artifact <verb>: missing verb (try `add` or `list`)".into())
    })?;
    match verb.as_str() {
        "add" => task_artifact_add(rest),
        "list" => task_artifact_list(rest),
        other => Err(CliError::Usage(format!(
            "unknown task artifact verb: {} (try `add` or `list`)",
            other
        ))),
    }
}

fn task_artifact_add(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let raw_path = f
        .path
        .clone()
        .ok_or_else(|| CliError::Usage("--path is required".into()))?;

    let state = ops::task::task_artifact_add(
        Some(project_id.as_str()),
        &task_id,
        &raw_path,
        f.kind.clone(),
        f.caption.clone(),
        f.lead,
    )?;

    let added_idx = state.artifacts.len() - 1;
    let added = state.artifacts.last().expect("just pushed");
    let is_lead = state.lead_artifact == Some(added_idx);
    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "artifact": {
            "kind": added.kind,
            "path": added.path,
            "original": added.original,
            "caption": added.caption,
            "added_at": added.added_at,
            "lead": is_lead,
        },
        "count": state.artifacts.len(),
        "lead_index": state.lead_artifact,
    }));
    Ok(())
}

fn task_artifact_list(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = ops::task::task_artifact_list(Some(project_id.as_str()), &task_id)?;

    let lead_idx = state.lead_artifact;
    let arr: Vec<serde_json::Value> = state
        .artifacts
        .iter()
        .enumerate()
        .map(|(i, a)| {
            serde_json::json!({
                "kind": a.kind,
                "path": a.path,
                "original": a.original,
                "caption": a.caption,
                "added_at": a.added_at,
                "lead": lead_idx == Some(i),
            })
        })
        .collect();
    print_json(&serde_json::json!({ "ok": true, "artifacts": arr }));
    Ok(())
}

// ─── task todos ──────────────────────────────────────────────────────────

fn task_todos_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage(
            "task todos <verb>: missing verb (try `set`, `check`, `uncheck`, or `clear`)".into(),
        )
    })?;
    match verb.as_str() {
        "set" => task_todos_set(rest),
        "check" => task_todos_mark(rest, true),
        "uncheck" => task_todos_mark(rest, false),
        "clear" => task_todos_clear(rest),
        other => Err(CliError::Usage(format!(
            "unknown task todos verb: {} (try `set`, `check`, `uncheck`, or `clear`)",
            other
        ))),
    }
}

fn todos_to_json(todos: &[TodoItem]) -> Vec<serde_json::Value> {
    todos
        .iter()
        .map(|t| serde_json::json!({ "text": t.text, "done": t.done }))
        .collect()
}

fn print_todos_result(state: &TaskState) {
    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "todos": todos_to_json(&state.todos),
    }));
}

fn task_todos_set(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let raw = f.items.clone().ok_or_else(|| {
        CliError::Usage("--items is required (newline-separated todo texts)".into())
    })?;

    let state = ops::task::task_todos_set(&project_id, &task_id, &raw)?;

    print_todos_result(&state);
    Ok(())
}

fn task_todos_mark(args: &[String], done: bool) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let idx = f
        .index
        .ok_or_else(|| CliError::Usage("--index is required (0-based)".into()))?;

    let state = ops::task::task_todos_mark(&project_id, &task_id, idx, done)?;

    print_todos_result(&state);
    Ok(())
}

fn task_todos_clear(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = ops::task::task_todos_clear(&project_id, &task_id)?;

    print_todos_result(&state);
    Ok(())
}

/// `cc-hub task create --prompt "..." [--project-id ID] [--name NAME]`
///
/// Headless task creation — used by tests and tooling that wants to seed a
/// task without going through the TUI's `N → folder → prompt` flow.
fn task_create(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let name = f.name.clone();
    let prompt = f
        .prompt
        .clone()
        .ok_or_else(|| CliError::Usage("--prompt is required".into()))?;

    let state = ops::task::task_create(f.project_id.as_deref(), name, prompt, f.backlog)?;

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "status": state.status,
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::dispatch;
    use crate::cli::test_util::{
        git_available, init_repo, seed_task_with_worktree, with_tempdir_home,
    };
    use cc_hub_lib::pr;

    #[test]
    fn auto_review_clears_timestamp_on_review_with_open_pr() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-auto".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Review;
            state.last_auto_reviewed_at = Some(123_456);
            orchestrator::write_task_state(&state).expect("write state");

            let created = pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            assert_eq!(created.review_state, pr::ReviewState::Open);

            let args = vec![
                "--task".to_string(),
                task_id.clone(),
                "--project-id".to_string(),
                project_id.clone(),
            ];
            task_auto_review(&args).expect("auto-review ok");

            let after = orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert!(after.last_auto_reviewed_at.is_none());
        });
    }

    #[test]
    fn task_report_rejects_backlog_to_running_transition() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-backlog-guard".to_string();

            let mut state = TaskState::new_backlog(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            orchestrator::write_task_state(&state).expect("write state");

            let args = vec![
                "--task".to_string(),
                task_id.clone(),
                "--project-id".to_string(),
                project_id.clone(),
                "--status".to_string(),
                "running".to_string(),
            ];
            let err = task_report(&args).expect_err("backlog->running must be rejected");
            match err {
                CliError::Usage(msg) => {
                    assert!(
                        msg.contains("task start"),
                        "message should point at the right verb, got: {msg}"
                    );
                }
                other => panic!("expected CliError::Usage, got {other:?}"),
            }

            // Status must not have been mutated.
            let after = orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after.status, TaskStatus::Backlog);
        });
    }

    #[test]
    fn task_show_json_emits_task_id() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-show".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "inspect me".into(),
            );
            state.task_id = task_id.clone();
            orchestrator::write_task_state(&state).expect("write state");

            let code = dispatch(&[
                "task".into(),
                "show".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--json".into(),
            ]);
            assert_eq!(code, Some(0), "dispatch should succeed");
        });
    }

    #[test]
    fn task_create_with_project_id_uses_registered_root() {
        with_tempdir_home(|| {
            let file = orchestrator::ProjectsFile {
                projects: vec![orchestrator::Project {
                    id: "p1".into(),
                    name: "p1".into(),
                    root: PathBuf::from("/tmp/p1"),
                    created_at: 0,
                    build_cmd: None,
                }],
            };
            orchestrator::save_projects(&file).expect("save");

            task_create(&[
                "--backlog".into(),
                "--project-id".into(),
                "p1".into(),
                "--prompt".into(),
                "foo".into(),
            ])
            .expect("task_create ok");

            let tasks_dir = orchestrator::project_state_dir("p1")
                .expect("dir")
                .join("tasks");
            let entries: Vec<_> = std::fs::read_dir(&tasks_dir)
                .expect("read tasks dir")
                .map(|e| e.expect("entry"))
                .collect();
            assert_eq!(entries.len(), 1, "expected exactly one task entry");
            let task_id = entries[0].file_name().to_string_lossy().into_owned();

            let state = orchestrator::read_task_state("p1", &task_id).expect("read state");
            assert_eq!(state.project_root, Some(PathBuf::from("/tmp/p1")));
            assert_eq!(state.project_id.as_deref(), Some("p1"));
            assert_eq!(state.status, TaskStatus::Backlog);

            let projects = orchestrator::load_projects();
            assert_eq!(projects.projects.len(), 1);
            assert_eq!(projects.projects[0].id, "p1");
        });
    }

    #[test]
    fn task_list_plain_emits_one_row_per_task() {
        with_tempdir_home(|| {
            let project_id = "p-list".to_string();

            let mut a = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "first task".into(),
            );
            a.task_id = "t-aaaa".into();
            a.status = TaskStatus::Running;
            a.updated_at = 2_000;
            orchestrator::write_task_state(&a).expect("write a");

            let mut b = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "second task".into(),
            );
            b.task_id = "t-bbbb".into();
            b.status = TaskStatus::Backlog;
            b.updated_at = 1_000;
            orchestrator::write_task_state(&b).expect("write b");

            let args = vec!["--project-id".into(), project_id.clone()];
            task_list(&args).expect("task_list ok");
        });
    }

    #[test]
    fn task_show_function_succeeds_without_pr() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-show-fn".to_string();
            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "hello".into(),
            );
            state.task_id = task_id.clone();
            orchestrator::write_task_state(&state).expect("write state");
            let args = vec![
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--json".into(),
            ];
            task_show(&args).expect("task_show ok");
        });
    }

    #[test]
    fn task_list_status_filter_rejects_garbage() {
        let args = vec!["--status".into(), "nope".into()];
        match task_list(&args) {
            Err(CliError::Usage(_)) => {}
            other => panic!("expected Usage error, got {:?}", other),
        }
    }

    #[test]
    fn task_list_missing_tasks_dir_is_ok() {
        with_tempdir_home(|| {
            let args = vec!["--project-id".into(), "nonexistent".into()];
            task_list(&args).expect("missing tasks dir should be ok");
        });
    }

    #[test]
    fn task_list_dispatch_smoke() {
        let code = dispatch(&["task".into(), "list".into(), "--json".into()]);
        assert_eq!(code, Some(0));
    }

    #[test]
    fn task_delete_removes_state_and_worktree() {
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        with_tempdir_home(|| {
            let repo = init_repo();
            let (project_id, task_id, wt_path) =
                seed_task_with_worktree(repo.path(), TaskStatus::Backlog);

            assert!(wt_path.exists(), "worktree should exist before delete");
            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();
            assert!(state_dir.exists(), "state dir should exist before delete");

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            assert_eq!(code, Some(0), "task delete should exit 0");
            assert!(!state_dir.exists(), "state dir should be gone after delete");
            assert!(
                !wt_path.exists(),
                "worktree dir should be gone after delete"
            );
        });
    }

    #[test]
    fn task_delete_refuses_running_without_force() {
        with_tempdir_home(|| {
            let project_id = "p-running".to_string();
            let task_id = "t-running".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Running;
            orchestrator::write_task_state(&state).expect("write state");

            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();
            assert!(state_dir.exists());

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            assert_ne!(code, Some(0), "expected non-zero exit without --force");
            assert!(
                state_dir.exists(),
                "state dir must survive a refused delete"
            );
        });
    }

    #[test]
    fn task_delete_running_succeeds_with_force() {
        with_tempdir_home(|| {
            let project_id = "p-running-force".to_string();
            let task_id = "t-running-force".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Running;
            orchestrator::write_task_state(&state).expect("write state");

            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();
            assert!(state_dir.exists());

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--force".into(),
            ]);
            assert_eq!(code, Some(0), "task delete --force should exit 0");
            assert!(
                !state_dir.exists(),
                "state dir should be gone after --force delete"
            );
        });
    }

    #[test]
    fn task_delete_refuses_merging_without_force() {
        with_tempdir_home(|| {
            let project_id = "p-merging".to_string();
            let task_id = "t-merging".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            assert_ne!(code, Some(0), "Merging must refuse delete without --force");
            assert!(
                state_dir.exists(),
                "state dir must survive a refused delete"
            );
        });
    }

    #[test]
    fn task_delete_merging_succeeds_with_force_and_releases_lock() {
        // A task wedged in Merging (e.g. its orchestrator died before
        // `pr finalize`) must be deletable under --force, and that delete
        // must release the project merge lock so the project isn't blocked
        // forever.
        with_tempdir_home(|| {
            let project_id = "p-merging-force".to_string();
            let task_id = "t-merging-force".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire lock");
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_some(),
                "lock should be held before delete"
            );

            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--force".into(),
            ]);
            assert_eq!(code, Some(0), "Merging delete --force should exit 0");
            assert!(
                !state_dir.exists(),
                "state dir should be gone after --force delete"
            );
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_none(),
                "merge lock must be released when a Merging task is deleted"
            );
        });
    }

    #[test]
    fn task_delete_releases_merge_lock() {
        with_tempdir_home(|| {
            let project_id = "p-lock-leak".to_string();
            let task_id = "t-lock-leak".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Backlog;
            orchestrator::write_task_state(&state).expect("write state");

            match cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None)
                .expect("acquire lock")
            {
                cc_hub_lib::merge_lock::AcquireOutcome::Acquired => {}
                other => panic!("expected Acquired, got {:?}", other),
            }
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_some(),
                "lock should be held before delete"
            );

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            assert_eq!(code, Some(0), "task delete should exit 0");
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_none(),
                "merge lock must be released by task delete"
            );
        });
    }
}
