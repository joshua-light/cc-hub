//! Background backlog triage. When `[backlog].enabled` is true, every
//! `interval_secs` cc-hub asks a short Claude session whether one of the
//! pending backlog tasks is ready to be promoted to Running.
//!
//! Per-task `triaged_at` stamps cap re-asks at one per `ttl_secs` so a
//! dormant backlog can't burn Claude calls on a tight loop. At most one
//! task is promoted per tick — leaves the existing single-pending-dispatch
//! invariant intact and lets the next tick re-evaluate after the just-
//! started orchestrator has had time to declare reservations.

use crate::config;
use crate::orchestrator::{
    self, now_unix_secs, read_task_state, write_task_state, TaskState, TaskStatus,
};
use crate::projects_scan;
use crate::title;
use log::{debug, info, warn};

/// A backlog task that the triager promoted. Carried back to the main
/// loop so it can wire the orchestrator into the pending-dispatch queue.
#[derive(Debug, Clone)]
pub struct Promotion {
    pub tmux: String,
    pub orchestrator_prompt: Option<String>,
}

#[derive(Debug, Default)]
pub struct TickOutcome {
    /// `Some` iff the agent picked a task and the orchestrator session
    /// was successfully spawned.
    pub promotion: Option<Promotion>,
    /// One-line status fit for `app.set_status`. `None` when nothing
    /// happened worth reporting (no eligible tasks).
    pub status: Option<String>,
}

/// Largest backlog the triager will describe in one prompt. Beyond this,
/// older tasks are dropped with a "+N more" footer — the next tick re-
/// evaluates with the same cap.
const MAX_TASKS_IN_PROMPT: usize = 50;

/// Run one triage pass. Designed for `tokio::task::spawn_blocking`: every
/// step (filesystem scan, child process spawn, state write) is synchronous.
pub fn tick() -> TickOutcome {
    let cfg = &config::get().backlog;
    if !cfg.enabled {
        return TickOutcome::default();
    }

    let snap = projects_scan::scan();
    let now = now_unix_secs();
    let ttl = cfg.ttl_secs as i64;

    let project_with_work = snap.projects.iter().find_map(|p| {
        let tasks = snap.tasks.get(&p.id)?;
        let eligible: Vec<&TaskState> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Backlog)
            .filter(|t| match t.triaged_at {
                None => true,
                Some(at) => now.saturating_sub(at) >= ttl,
            })
            .map(|t| t.as_ref())
            .collect();
        if eligible.is_empty() {
            None
        } else {
            Some((p.clone(), eligible))
        }
    });

    let Some((project, tasks)) = project_with_work else {
        return TickOutcome::default();
    };

    debug!(
        "triage: project={} eligible_tasks={}",
        project.id,
        tasks.len()
    );

    let decision = ask_claude(&project, &tasks, &cfg.model, cfg.run_timeout());
    let chosen_id = match &decision {
        Decision::Promote(id) => Some(id.clone()),
        Decision::Hold | Decision::Failed => None,
    };

    // Stamp every considered task that wasn't picked, so a Claude failure
    // (or a hold) doesn't put us in a tight retry loop. The picked task is
    // skipped — start_backlog_task rewrites its state with status=Planning,
    // and stamping an already-promoted task is just a wasted disk write.
    // Direct read/write skips the `touch()` in `update_task_state` so the
    // kanban doesn't reshuffle every TTL purely from triage stamping.
    for t in &tasks {
        if Some(&t.task_id) == chosen_id.as_ref() {
            continue;
        }
        if let Ok(mut s) = read_task_state(&project.id, &t.task_id) {
            s.triaged_at = Some(now);
            if let Err(e) = write_task_state(&s) {
                warn!("triage: failed to stamp triaged_at on {}: {}", t.task_id, e);
            }
        }
    }

    match decision {
        Decision::Promote(task_id) => match orchestrator::start_backlog_task(
            &project.id,
            &task_id,
            None,
        ) {
            Ok((state, tmux_name, orch_prompt)) => {
                info!(
                    "triage: promoted backlog {} → orchestrator [{}]",
                    state.task_id, tmux_name
                );
                TickOutcome {
                    promotion: Some(Promotion {
                        tmux: tmux_name.clone(),
                        orchestrator_prompt: orch_prompt,
                    }),
                    status: Some(format!(
                        "triage: promoted [{}], orchestrator [{}] starting…",
                        state.task_id, tmux_name
                    )),
                }
            }
            Err(e) => {
                warn!("triage: start_backlog_task failed for {}: {}", task_id, e);
                TickOutcome {
                    promotion: None,
                    status: Some(format!("triage: failed to promote {}: {}", task_id, e)),
                }
            }
        },
        Decision::Hold => TickOutcome {
            promotion: None,
            status: Some(format!(
                "triage: held {} backlog task(s) in {}",
                tasks.len(),
                project.name
            )),
        },
        Decision::Failed => TickOutcome::default(),
    }
}

/// Outcome of asking Claude which task to promote. Failures (timeout,
/// parse error, unknown id) collapse to `Failed` on purpose — a quiet
/// skip is safer than a wrong promotion.
#[derive(Debug, Clone)]
enum Decision {
    Promote(String),
    Hold,
    Failed,
}

fn ask_claude(
    project: &orchestrator::Project,
    tasks: &[&TaskState],
    model: &str,
    timeout: std::time::Duration,
) -> Decision {
    let prompt = build_prompt(project, tasks);
    let Some(raw) = title::run_claude_blocking(model, &prompt, timeout) else {
        return Decision::Failed;
    };
    let Some(parsed) = parse_response(&raw) else {
        return Decision::Failed;
    };
    let chosen = match parsed {
        ParsedDecision::Hold => return Decision::Hold,
        ParsedDecision::Promote(id) => id,
    };
    if !tasks.iter().any(|t| t.task_id == chosen) {
        warn!(
            "triage: agent picked unknown task_id {:?}, ignoring",
            chosen
        );
        return Decision::Failed;
    }
    Decision::Promote(chosen)
}

fn build_prompt(project: &orchestrator::Project, tasks: &[&TaskState]) -> String {
    let mut buf = String::new();
    buf.push_str(
        "You are the backlog triage agent for cc-hub. Below is the current backlog \
for a project. Decide which ONE task should be started next, considering:\n\
- dependencies (a task that depends on the output of another should wait)\n\
- whether enough context exists to act now without more user input\n\
- obvious sequencing (e.g. a refactor that should land before a feature on top)\n\n\
Output STRICT JSON on a single line, nothing else, no prose, no fences:\n\
  {\"start_task_id\": \"<id>\"}   — to promote that task\n\
  {\"start_task_id\": null}       — to hold the entire backlog this round\n\n",
    );
    buf.push_str(&format!(
        "Project: {}\nRoot: {}\n\nBacklog:\n",
        project.name,
        project.root.display()
    ));
    let visible = tasks.len().min(MAX_TASKS_IN_PROMPT);
    for t in tasks.iter().take(visible) {
        let title = t.title.as_deref().unwrap_or("(untitled)");
        let prompt = if t.prompt.len() > 800 {
            format!("{}…", &t.prompt[..800])
        } else {
            t.prompt.clone()
        };
        buf.push_str(&format!(
            "- id={}\n  title: {}\n  prompt: {}\n",
            t.task_id, title, prompt
        ));
    }
    if tasks.len() > visible {
        buf.push_str(&format!("(+{} more not shown)\n", tasks.len() - visible));
    }
    buf
}

#[derive(Debug, PartialEq)]
enum ParsedDecision {
    Promote(String),
    Hold,
}

/// Pull the chosen task id (or null) out of the agent's stdout. Tolerant
/// of stray prose around the JSON object since `claude -p` occasionally
/// prefixes a thinking line. `None` for any parse failure.
fn parse_response(raw: &str) -> Option<ParsedDecision> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    let blob = &raw[start..=end];
    let v: serde_json::Value = serde_json::from_str(blob).ok()?;
    match v.get("start_task_id") {
        Some(serde_json::Value::Null) => Some(ParsedDecision::Hold),
        Some(serde_json::Value::String(s)) if !s.is_empty() => {
            Some(ParsedDecision::Promote(s.clone()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_picks_id() {
        assert_eq!(
            parse_response("{\"start_task_id\":\"abc123\"}"),
            Some(ParsedDecision::Promote("abc123".into()))
        );
    }

    #[test]
    fn parse_response_handles_null() {
        assert_eq!(
            parse_response("{\"start_task_id\":null}"),
            Some(ParsedDecision::Hold)
        );
    }

    #[test]
    fn parse_response_tolerates_prose() {
        assert_eq!(
            parse_response("Here you go:\n{\"start_task_id\":\"t-42\"}\n"),
            Some(ParsedDecision::Promote("t-42".into()))
        );
    }

    #[test]
    fn parse_response_rejects_garbage() {
        assert_eq!(parse_response(""), None);
        assert_eq!(parse_response("no json here"), None);
        assert_eq!(parse_response("{not json}"), None);
        assert_eq!(parse_response("{\"other\":1}"), None);
        // Empty string is treated as a parse failure, not a real id.
        assert_eq!(parse_response("{\"start_task_id\":\"\"}"), None);
    }
}
