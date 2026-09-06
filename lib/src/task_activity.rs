//! Task progress is distinct from session liveness and the user's Done action.
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct Activity {
    stage: String,
    #[serde(default)]
    detail: String,
}

pub fn label_at(root: &Path, task: &str) -> Option<String> {
    if task.contains('/') || task.contains('\\') || task == ".." {
        return None;
    }
    let read = |file| {
        let text = std::fs::read_to_string(root.join(task).join(file)).ok()?;
        serde_json::from_str::<Activity>(&text).ok()
    };
    let job = read("activity.json");
    // Delivery is final; otherwise show coordination failures even without a job.
    let activity = if job.as_ref().is_some_and(|item| item.stage == "delivered") {
        job?
    } else {
        read("resources.json")
            .or_else(|| read("coordination.json"))
            .or(job)?
    };
    let label = match activity.stage.as_str() {
        "delivered" => "delivered — awaiting closeout",
        "running" | "starting" => "job running",
        "succeeded" => "job complete — awaiting review",
        "failed" | "lost" | "timed_out" | "cancelled" => "job needs recovery",
        "no_progress" => "no new job output",
        "owner_auth_or_quota_blocked" => "auth/quota blocked",
        "owner_unavailable" => "owner unavailable",
        "waiting_qa_ack" => "waiting for QA acknowledgment",
        "qa_ack_overdue" => "QA acknowledgment overdue",
        "qa_no_progress" => "no recent QA progress",
        "capacity_wait" => "waiting for subscription capacity",
        "resource_handoff" => "changing worker account",
        "resource_blocked" => "worker needs recovery",
        _ => return None,
    };
    Some(
        if activity.detail.is_empty() || activity.stage == "delivered" {
            label.to_string()
        } else {
            format!("{}: {}", label, activity.detail)
        },
    )
}

pub fn label(task: &str) -> Option<String> {
    label_at(&dirs::home_dir()?.join(".cc-hub/tasks"), task)
}

/// The script holds a cross-process lock, so an installed OS watchdog can also run it.
pub fn spawn_supervisor() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let Some(home) = dirs::home_dir() else {
                continue;
            };
            let script = home.join(".claude/skills/task/scripts/jobs");
            if !script.is_file() {
                continue;
            }
            let mut command = tokio::process::Command::new("python3");
            command.arg(script).arg("supervise").kill_on_drop(true);
            match tokio::time::timeout(std::time::Duration::from_secs(25), command.output()).await {
                Ok(Ok(output)) if output.status.success() => {}
                result => log::warn!(
                    "task job supervisor did not finish successfully: {:?}",
                    result
                ),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordination_is_visible_without_a_job_and_clears_after_ack() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("tk-test");
        std::fs::create_dir(&task).unwrap();
        std::fs::write(
            task.join("coordination.json"),
            r#"{"stage":"qa_ack_overdue"}"#,
        )
        .unwrap();
        assert_eq!(
            label_at(root.path(), "tk-test").unwrap(),
            "QA acknowledgment overdue"
        );
        std::fs::write(task.join("activity.json"), r#"{"stage":"running"}"#).unwrap();
        std::fs::write(task.join("coordination.json"), r#"{"stage":null}"#).unwrap();
        assert_eq!(label_at(root.path(), "tk-test").unwrap(), "job running");
        std::fs::write(
            task.join("coordination.json"),
            r#"{"stage":"qa_no_progress"}"#,
        )
        .unwrap();
        std::fs::write(task.join("activity.json"), r#"{"stage":"delivered"}"#).unwrap();
        assert_eq!(
            label_at(root.path(), "tk-test").unwrap(),
            "delivered — awaiting closeout"
        );
    }

    #[test]
    fn delivered_and_blocked_are_not_inferred_from_idle() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("tk-test");
        std::fs::create_dir(&task).unwrap();
        assert_eq!(label_at(root.path(), "tk-test"), None);
        std::fs::write(task.join("activity.json"), r#"{"stage":"delivered"}"#).unwrap();
        assert_eq!(
            label_at(root.path(), "tk-test").unwrap(),
            "delivered — awaiting closeout"
        );
        std::fs::write(
            task.join("activity.json"),
            r#"{"stage":"owner_auth_or_quota_blocked"}"#,
        )
        .unwrap();
        assert_eq!(
            label_at(root.path(), "tk-test").unwrap(),
            "auth/quota blocked"
        );
        assert_eq!(label_at(root.path(), "../tk-test"), None);
    }
}
