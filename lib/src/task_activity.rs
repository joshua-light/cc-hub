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

/// The word a session writes when it has opened a pull request.
const PULL_REQUEST: &str = "PR:";

/// Saying you told the user is not the user answering. A `Posted:` follows
/// nearly every wait — it is the record of the message about it — so a card
/// whose newest note is one is still waiting on whatever came before.
const BOOKKEEPING: &str = "Posted:";

/// A note's first line, read by the word it opens with. The card's notes are
/// its record, and that word is how the board knows whose move comes next:
/// a question only the user can answer, a pull request waiting to be read,
/// a line of bookkeeping about an earlier note, or plain progress — which
/// clears whatever the notes before it left open.
///
/// This is the one place the note vocabulary is spelled out: the board reads
/// it for the card's pill, and [`crate::ops::task`] reads it to move a card
/// whose session has just opened a PR into Review.
pub enum Caption<'a> {
    Wait(&'a str),
    PullRequest(&'a str),
    Posted,
    Progress,
}

impl<'a> Caption<'a> {
    pub fn read(line: &'a str) -> Self {
        let line = line.trim();
        if let Some(rest) = WAITS.iter().find_map(|prefix| line.strip_prefix(prefix)) {
            return Caption::Wait(rest.trim());
        }
        if let Some(rest) = line.strip_prefix(PULL_REQUEST) {
            return Caption::PullRequest(rest.trim());
        }
        if line.starts_with(BOOKKEEPING) {
            return Caption::Posted;
        }
        Caption::Progress
    }
}

/// What the running session last wrote on the card itself.
///
/// `clarification.json` belongs to the router, which writes it before any
/// session exists; a session that starts and then needs an answer has only
/// the card's notes, so that is where the board has to read it. The newest
/// note that says something about the work wins, and the next one clears it
/// — a `Candidate:` is a session that stopped waiting. Only the note's
/// caption is read, which is its first line as the board already stores it.
fn newest_note(root: &Path, task: &str) -> Option<Activity> {
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
    let latest = card
        .artifacts
        .iter()
        .rev()
        .filter(|a| a.kind == "note")
        .find_map(|a| match Caption::read(a.caption.as_deref()?) {
            Caption::Posted => None,
            read => Some(read),
        })?;
    let (stage, detail) = match latest {
        Caption::Wait(detail) => ("waiting", detail),
        Caption::PullRequest(detail) => ("pr", detail),
        _ => return None,
    };
    Some(Activity {
        stage: stage.into(),
        detail: detail.to_string(),
    })
}

/// What the label asks of whoever reads the board. A question nobody but the
/// user can answer and a pull request nobody has read yet are both the user's
/// move, but they are not the same errand — one unblocks an agent, the other
/// is the work arriving — and the board colours them apart. Carried on the
/// label so the caller never re-matches the text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Errand {
    /// Only an answer from the user moves the card.
    Answer,
    /// A pull request is open, waiting to be read.
    Review,
    /// Nothing is asked: the card still moves on its own.
    Nothing,
}

/// What the board shows for a task, and what it asks of the reader.
pub struct Label {
    pub text: String,
    pub errand: Errand,
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
        .or_else(|| newest_note(root, task))
        .or_else(|| read("resources.json"))?;
    let label = match activity.stage.as_str() {
        "clarification" => "needs clarification",
        "waiting" => "waiting on you",
        "pr" => "PR ready",
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
        errand: match activity.stage.as_str() {
            "clarification" | "waiting" => Errand::Answer,
            "pr" => Errand::Review,
            _ => Errand::Nothing,
        },
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
        assert_eq!(label.errand, Errand::Answer);
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

    /// The errand a PR leaves is a reading, not an answer — the point of
    /// telling the two apart on the board at all.
    #[test]
    fn a_pr_note_asks_for_a_review_not_an_answer() {
        let tmp = tempfile::tempdir().unwrap();
        card(tmp.path(), "tk-1", &["PR: sample-project#42"]);

        let label = label_at(tmp.path(), "tk-1").expect("a pull request");
        assert_eq!(label.text, "PR ready: sample-project#42");
        assert_eq!(label.errand, Errand::Review);
    }

    /// Opening the PR is what a wait was blocking on, so the `PR:` note
    /// replaces it — and a `Posted:` about the PR does not undo that.
    #[test]
    fn a_pr_note_clears_an_earlier_wait() {
        let tmp = tempfile::tempdir().unwrap();
        card(
            tmp.path(),
            "tk-1",
            &[
                "Waiting: which base branch?",
                "PR: sample-project#42",
                "Posted: told you in #status",
            ],
        );

        let label = label_at(tmp.path(), "tk-1").expect("a pull request");
        assert_eq!(label.errand, Errand::Review);
    }

    /// A question asked after the PR went up is the newer note, so the card
    /// goes back to asking for an answer.
    #[test]
    fn a_wait_after_the_pr_is_the_one_that_shows() {
        let tmp = tempfile::tempdir().unwrap();
        card(
            tmp.path(),
            "tk-1",
            &["PR: sample-project#42", "Waiting: squash or merge?"],
        );

        let label = label_at(tmp.path(), "tk-1").expect("a wait");
        assert_eq!(label.text, "waiting on you: squash or merge?");
        assert_eq!(label.errand, Errand::Answer);
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

    /// Telling the user is not the user answering, and `Posted:` follows
    /// nearly every wait — clearing on it would make the pill blink out the
    /// moment it became true.
    #[test]
    fn saying_you_posted_does_not_clear_the_wait() {
        let tmp = tempfile::tempdir().unwrap();
        card(
            tmp.path(),
            "tk-1",
            &["Waiting: which base branch?", "Posted: asked in #status"],
        );

        let label = label_at(tmp.path(), "tk-1").expect("still waiting");
        assert_eq!(label.text, "waiting on you: which base branch?");
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
