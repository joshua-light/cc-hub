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

/// A note that opens with one of these is a session saying it cannot go on
/// without the user. `Blocked:` is deliberately not among them: it names a
/// host, and a host is fixed by whoever is nearest, not by answering.
const WAITS: [&str; 2] = ["Waiting:", "Needs you:"];

/// The wait the running session wrote on the card itself.
///
/// `clarification.json` belongs to the router, which writes it before any
/// session exists; a session that starts and then needs an answer has only
/// the card's notes, so that is where the board has to read it. The newest
/// note wins and the next note clears it — a `Candidate:` or a `PR:` is a
/// session that stopped waiting. Only the note's caption is read, which is
/// its first line as the board already stores it.
fn waiting(root: &Path, task: &str) -> Option<Activity> {
    #[derive(Deserialize)]
    struct Card {
        #[serde(default)]
        artifacts: Vec<Attachment>,
    }
    #[derive(Deserialize)]
    struct Attachment {
        kind: String,
        caption: Option<String>,
    }
    let text = std::fs::read_to_string(root.join(task).join("state.json")).ok()?;
    let card = serde_json::from_str::<Card>(&text).ok()?;
    let latest = card.artifacts.iter().rev().find(|a| a.kind == "note")?;
    let caption = latest.caption.as_deref()?.trim();
    let detail = WAITS
        .iter()
        .find_map(|prefix| caption.strip_prefix(prefix))?;
    Some(Activity {
        stage: "waiting".into(),
        detail: detail.trim().to_string(),
    })
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
    // An open question outranks the broker's own state, whoever asked it.
    let activity = read("clarification.json")
        .or_else(|| waiting(root, task))
        .or_else(|| read("resources.json"))?;
    let label = match activity.stage.as_str() {
        "clarification" => "needs clarification",
        "waiting" => "waiting on you",
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
        blocked_on_user: matches!(activity.stage.as_str(), "clarification" | "waiting"),
    })
}

pub fn label(task: &str) -> Option<Label> {
    label_at(&dirs::home_dir()?.join(".cc-hub/tasks"), task)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card directory as the board leaves it: a `state.json` whose notes
    /// are captions in attach order.
    fn card(root: &Path, task: &str, captions: &[&str]) {
        let notes: Vec<String> = captions
            .iter()
            .map(|c| format!(r#"{{"kind":"note","caption":"{}"}}"#, c))
            .collect();
        let dir = root.join(task);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("state.json"),
            format!(r#"{{"artifacts":[{}]}}"#, notes.join(",")),
        )
        .unwrap();
    }

    fn side_file(root: &Path, task: &str, name: &str, body: &str) {
        std::fs::create_dir_all(root.join(task)).unwrap();
        std::fs::write(root.join(task).join(name), body).unwrap();
    }

    #[test]
    fn a_waiting_note_reads_as_a_wait_on_the_user() {
        let tmp = tempfile::tempdir().unwrap();
        card(tmp.path(), "tk-1", &["Waiting: which base branch?"]);

        let label = label_at(tmp.path(), "tk-1").expect("a wait");
        assert_eq!(label.text, "waiting on you: which base branch?");
        assert!(label.blocked_on_user);
    }

    #[test]
    fn needs_you_is_the_same_wait() {
        let tmp = tempfile::tempdir().unwrap();
        card(tmp.path(), "tk-1", &["Needs you: approve the announcement"]);

        let label = label_at(tmp.path(), "tk-1").expect("a wait");
        assert_eq!(label.text, "waiting on you: approve the announcement");
    }

    #[test]
    fn the_next_note_clears_the_wait() {
        let tmp = tempfile::tempdir().unwrap();
        card(
            tmp.path(),
            "tk-1",
            &["Waiting: which base branch?", "Candidate: fix/thing"],
        );

        assert!(label_at(tmp.path(), "tk-1").is_none());
    }

    /// `Blocked:` names a host, not a question: it is a note like any other
    /// here, and the board keeps showing what the session is doing.
    #[test]
    fn a_blocked_note_is_not_a_wait() {
        let tmp = tempfile::tempdir().unwrap();
        card(
            tmp.path(),
            "tk-1",
            &["Blocked: Unity Editor will not start"],
        );

        assert!(label_at(tmp.path(), "tk-1").is_none());
    }

    #[test]
    fn a_wait_outranks_the_brokers_own_stage() {
        let tmp = tempfile::tempdir().unwrap();
        card(tmp.path(), "tk-1", &["Waiting: PR or direct push?"]);
        side_file(
            tmp.path(),
            "tk-1",
            "resources.json",
            r#"{"stage":"resource_wait","detail":"main-tps"}"#,
        );

        let label = label_at(tmp.path(), "tk-1").expect("a wait");
        assert_eq!(label.text, "waiting on you: PR or direct push?");
    }

    /// The router asks before a session exists; if both somehow speak, the
    /// unrouted card is the one nobody can act on.
    #[test]
    fn the_routers_question_still_comes_first() {
        let tmp = tempfile::tempdir().unwrap();
        card(tmp.path(), "tk-1", &["Waiting: PR or direct push?"]);
        side_file(
            tmp.path(),
            "tk-1",
            "clarification.json",
            r#"{"stage":"clarification","detail":"which repository?"}"#,
        );

        let label = label_at(tmp.path(), "tk-1").expect("a question");
        assert_eq!(label.text, "needs clarification: which repository?");
    }

    #[test]
    fn a_card_with_no_notes_and_no_side_files_says_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        card(tmp.path(), "tk-1", &[]);

        assert!(label_at(tmp.path(), "tk-1").is_none());
    }

    #[test]
    fn a_task_id_cannot_walk_out_of_the_board() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(label_at(tmp.path(), "..").is_none());
        assert!(label_at(tmp.path(), "tk-1/../tk-2").is_none());
    }
}
