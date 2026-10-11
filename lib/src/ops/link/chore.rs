use crate::config;
use crate::link::{BoardTaskId, Chore};
use crate::ops::OpError;
use crate::tasks::store::{self, TaskStatus};
use crate::tasks::PersonalBoard;

/// File a chore as a card on the Tasks board: its standing orders as the
/// text, its session title as the title, its kind, and its brief as the
/// first note. The card is left in To-Do — it moves to Running through
/// [`start_card`] once a session actually holds it, so a chore that failed
/// to start is visible as a card with a brief and nobody on it, not as one
/// that claims to be running.
pub fn file_chore(chore: &dyn Chore) -> Result<BoardTaskId, OpError> {
    let kind = chore_kind(chore)?;
    let mut board =
        PersonalBoard::load_result().map_err(|e| OpError::Other(format!("load board: {}", e)))?;
    let task_id = board
        .add(&chore.prompt())
        .map_err(|e| OpError::Other(format!("write card: {}", e)))?
        .expect("a chore prompt is never empty");
    board
        .set_kind(&task_id, kind)
        .map_err(|e| OpError::Other(format!("write kind: {}", e)))?;
    store::set_task_title(&task_id, &chore.session_title())
        .map_err(|e| OpError::Other(format!("write title: {}", e)))?;
    crate::ops::task::task_artifact_add_text(&task_id, &chore.brief(), "link")?;
    Ok(task_id.parse().expect("the board mints tk- ids"))
}

/// The kind a chore card is filed under, checked against the board's list so
/// `--dry-run` refuses a link the browser button got wrong before anything
/// is filed. `None` is a card without a kind, as the board allows.
pub(super) fn chore_kind(chore: &dyn Chore) -> Result<Option<String>, OpError> {
    chore
        .kind()
        .map(|k| config::get().tasks.known_kind(k))
        .transpose()
        .map_err(|why| OpError::Usage(format!("link kind: {}", why)))
}

/// The card's session is up: move it to Running. Called after the binding,
/// never before, so the task router — which hands out cards that are Running
/// with no session — never sees this one as its own.
pub fn start_card(task_id: &str) -> Result<(), OpError> {
    PersonalBoard::load_result()
        .map_err(|e| OpError::Other(format!("load board: {}", e)))?
        .set_status(task_id, TaskStatus::Running)
        .map(|_| ())
        .map_err(|e| {
            OpError::Other(format!(
                "card {} is bound but could not start: {}",
                task_id, e
            ))
        })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::link::{FixLink, Link};
    use crate::ops::link::target::{board_card, without_a_brief};

    fn fix_link(query: &str) -> FixLink {
        let raw = format!(
            "cc-hub://fix?pr=https://bitbucket.example.com/projects/APP/repos/sample-project/pull-requests/11280{}",
            query
        );
        match raw.parse::<Link>() {
            Ok(Link::Fix(fix)) => fix,
            other => panic!("expected a fix link, got {:?}", other),
        }
    }

    #[test]
    fn a_fix_is_filed_as_a_card_with_its_brief() {
        crate::test_util::with_temp_home(|| {
            let fix = fix_link("&title=Fix%20the%20parser");
            let id = file_chore(&fix).expect("file");
            let card = board_card(id.as_str()).expect("card");
            assert_eq!(card.status, TaskStatus::Backlog);
            assert_eq!(card.title.as_deref(), Some("Fix: Fix the parser"));
            assert_eq!(card.prompt, fix.prompt());
            assert_eq!(card.kind, None);
            assert!(card.tmux.is_none());
            let notes = crate::ops::task::notes_of(&card);
            assert_eq!(notes.len(), 1);
            assert!(
                notes[0].text.starts_with("Brief\nProblem: "),
                "{}",
                notes[0].text
            );
            assert!(notes[0].text.contains("pull-requests/11280"));
            assert!(notes[0].text.contains("Verification:"));
            // With a note on it, a role link would pass the hand-over gate.
            without_a_brief(&card).expect("the brief is the note");
        });
    }

    #[test]
    fn a_filed_fix_starts_into_running() {
        crate::test_util::with_temp_home(|| {
            let id = file_chore(&fix_link("")).expect("file");
            start_card(id.as_str()).expect("start");
            assert_eq!(
                board_card(id.as_str()).expect("card").status,
                TaskStatus::Running
            );
        });
    }

    #[test]
    fn a_chore_kind_must_be_one_the_board_offers() {
        crate::test_util::with_temp_home(|| {
            // A temp home has no [tasks].kinds, so any kind is unknown.
            assert!(matches!(
                file_chore(&fix_link("&kind=tps")),
                Err(OpError::Usage(why)) if why.contains("link kind")
            ));
        });
    }
}
