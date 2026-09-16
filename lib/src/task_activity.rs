//! Task progress is distinct from session liveness and the user's Done action.
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct Record {
    /// A null stage is how a publisher says it has nothing to report.
    stage: Option<String>,
    #[serde(default)]
    detail: String,
}

struct Activity {
    stage: String,
    detail: String,
}

/// What the board shows for a task, and whether the task can still move
/// without the user. A stage nobody but the user can clear is worth a
/// different colour, so the caller does not have to re-match the text.
pub struct Label {
    pub text: String,
    pub blocked_on_user: bool,
}

pub fn label_at(root: &Path, task: &str) -> Option<Label> {
    if task.contains('/') || task.contains('\\') || task == ".." {
        return None;
    }
    let read = |file| {
        let text = std::fs::read_to_string(root.join(task).join(file)).ok()?;
        let record = serde_json::from_str::<Record>(&text).ok()?;
        Some(Activity {
            stage: record.stage?,
            detail: record.detail,
        })
    };
    // An open question outranks the broker's own state.
    let activity = read("clarification.json").or_else(|| read("resources.json"))?;
    let label = match activity.stage.as_str() {
        "clarification" => "needs clarification",
        "capacity_wait" => "waiting for subscription capacity",
        "resource_wait" => "waiting for a resource",
        "resource_handoff" => "changing worker account",
        "resource_blocked" => "worker needs recovery",
        _ => return None,
    };
    Some(Label {
        text: if activity.detail.is_empty() {
            label.to_string()
        } else {
            format!("{}: {}", label, activity.detail)
        },
        blocked_on_user: activity.stage == "clarification",
    })
}

pub fn label(task: &str) -> Option<Label> {
    label_at(&dirs::home_dir()?.join(".cc-hub/tasks"), task)
}
