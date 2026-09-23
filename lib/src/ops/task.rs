//! Task note and attachment ops: attach a file, URL or free-text note to a
//! card, read its notes back, remove one. The CLI (`cc-hub board`) keeps flag
//! parsing and JSON rendering; everything that mutates on-disk task state
//! lives here.

use std::path::PathBuf;

use crate::ops::OpError;
use crate::task_store::{self, Artifact, TaskState, TaskStatus};

fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

fn update_task<F>(task_id: &str, f: F) -> Result<TaskState, OpError>
where
    F: FnOnce(&mut TaskState),
{
    task_store::update_task(task_id, f).map_err(|e| OpError::Other(format!("persist state: {}", e)))
}

/// The Tasks board's attach action: copy a file (or record a URL) into the
/// task's artifacts dir and append an `Artifact` record. Returns the
/// persisted state.
pub fn task_artifact_add(
    task_id: &str,
    raw_path: &str,
    kind: Option<String>,
    caption: Option<String>,
    lead: bool,
) -> Result<TaskState, OpError> {
    // Confirm the task exists before doing any filesystem work, so we don't
    // copy files into a directory that points at a nonexistent task.
    let _ = task_store::read_task_state(task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;

    let (kind, stored_path) = if looks_like_url(raw_path) {
        let kind = kind.unwrap_or_else(|| "url".into());
        (kind, raw_path.to_string())
    } else {
        let kind = kind.unwrap_or_else(|| "file".into());
        let src = std::fs::canonicalize(raw_path).map_err(|e| {
            OpError::Other(format!(
                "resolve source path {:?}: {} (does the file exist?)",
                raw_path, e
            ))
        })?;
        let meta = std::fs::metadata(&src)
            .map_err(|e| OpError::Other(format!("stat {}: {}", src.display(), e)))?;
        if meta.is_dir() {
            return Err(OpError::Other(format!(
                "{} is a directory; only single files are supported",
                src.display()
            )));
        }
        let basename = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| OpError::Other(format!("{} has no file name", src.display())))?;

        let dest_dir = task_store::task_dir(task_id)
            .ok_or_else(|| OpError::Other("no home dir".into()))?
            .join("artifacts");
        std::fs::create_dir_all(&dest_dir)
            .map_err(|e| OpError::Other(format!("create {}: {}", dest_dir.display(), e)))?;

        let ts = task_store::now_unix_secs();
        let dest = dest_dir.join(format!("{}-{}", ts, basename));
        std::fs::copy(&src, &dest).map_err(|e| {
            OpError::Other(format!(
                "copy {} -> {}: {}",
                src.display(),
                dest.display(),
                e
            ))
        })?;
        (kind, dest.to_string_lossy().into_owned())
    };

    let artifact = Artifact {
        kind: kind.clone(),
        path: stored_path.clone(),
        original: raw_path.to_string(),
        caption,
        added_at: task_store::now_unix_secs(),
    };
    let mark_lead = lead;
    update_task(task_id, |s| {
        s.artifacts.push(artifact.clone());
        if mark_lead {
            s.lead_artifact = Some(s.artifacts.len() - 1);
        }
    })
}

/// Attach free text to a task: write it as a fresh `<ts>-note.md` inside
/// the task's artifacts dir (there is no source file to copy) and append a
/// `note` Artifact record whose caption is the text's first line, so the
/// card header says what the note is about. `origin` lands in the record's
/// `original` slot — the text never had a path, so callers pass where it
/// came from instead (`"clipboard"` for a paste, `"typed"` for the attach
/// input's note mode). Returns the persisted state.
pub fn task_artifact_add_text(
    task_id: &str,
    text: &str,
    origin: &str,
) -> Result<TaskState, OpError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(OpError::Usage("no text to attach".into()));
    }
    // Confirm the task exists before touching the filesystem, mirroring
    // `task_artifact_add`.
    let state = task_store::read_task_state(task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    // The notes are the card's record, and a record does not say the same
    // thing twice in a row: a session that writes its wait again has not
    // asked anything new, and the second copy only rings a second bell.
    if newest_note_says(&state, trimmed) {
        return Err(OpError::Usage(format!(
            "the card's newest note already says that: {}",
            crate::models::first_line_truncated(trimmed, 48)
        )));
    }

    let dest_dir = task_store::task_dir(task_id)
        .ok_or_else(|| OpError::Other("no home dir".into()))?
        .join("artifacts");
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| OpError::Other(format!("create {}: {}", dest_dir.display(), e)))?;

    // Every note shares the `note.md` basename, so a same-second double
    // paste would silently overwrite (and removal of one record would
    // delete the other's file) — probe for a free name instead.
    let ts = task_store::now_unix_secs();
    let mut dest = dest_dir.join(format!("{}-note.md", ts));
    let mut n = 2;
    while dest.exists() {
        dest = dest_dir.join(format!("{}-note-{}.md", ts, n));
        n += 1;
    }
    std::fs::write(&dest, text)
        .map_err(|e| OpError::Other(format!("write {}: {}", dest.display(), e)))?;

    let artifact = Artifact {
        kind: "note".into(),
        path: dest.to_string_lossy().into_owned(),
        original: origin.into(),
        caption: Some(crate::models::first_line_truncated(trimmed, 48)),
        added_at: ts,
    };
    // The notes are the card's record, so the column follows them: the `PR:`
    // a session writes when it opens a pull request is the moment the card
    // stops being work in progress and starts waiting to be read. Saying it
    // once — in the note — is the point; a second command to move the card is
    // one an agent forgets, and then the board lies.
    let opens_pr = matches!(
        crate::task_activity::Caption::read(trimmed.lines().next().unwrap_or_default()),
        crate::task_activity::Caption::PullRequest(_)
    );
    update_task(task_id, |s| {
        s.artifacts.push(artifact.clone());
        if opens_pr && matches!(s.status, TaskStatus::Planning | TaskStatus::Running) {
            s.status = TaskStatus::Review;
        }
    })
}

/// Whether the card's newest note is, word for word, `text`.
fn newest_note_says(task: &TaskState, text: &str) -> bool {
    task.artifacts
        .iter()
        .rev()
        .find(|a| a.kind == "note")
        .and_then(|a| std::fs::read_to_string(&a.path).ok())
        .is_some_and(|newest| newest.trim() == text)
}

/// One `note` attachment of a card, with its text read back from the file
/// `task_artifact_add_text` wrote. The notes of a card, in attach order, are
/// its record: the brief the implementation session agreed with the user,
/// the branch it built, what verification found. A session that takes the
/// card over reads them before anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub added_at: i64,
    pub caption: Option<String>,
    pub path: String,
    pub text: String,
}

/// The card's notes in attach order. A note whose file is gone or unreadable
/// is skipped rather than failing the read: the card still has its other
/// notes, and the missing one is visible on the board as an attachment with
/// nothing behind it.
pub fn notes_of(task: &TaskState) -> Vec<Note> {
    task.artifacts
        .iter()
        .filter(|a| a.kind == "note")
        .filter_map(|a| {
            let text = std::fs::read_to_string(&a.path).ok()?;
            Some(Note {
                added_at: a.added_at,
                caption: a.caption.clone(),
                path: a.path.clone(),
                text: text.trim().to_string(),
            })
        })
        .collect()
}

/// `cc-hub board notes` body: the notes of a card.
pub fn task_notes(task_id: &str) -> Result<Vec<Note>, OpError> {
    let state = task_store::read_task_state(task_id)
        .map_err(|e| OpError::NotFound(format!("no board task {}: {}", task_id, e)))?;
    Ok(notes_of(&state))
}

/// Remove artifact `index` from a task: drop the record (fixing
/// `lead_artifact` if it pointed at or past the removed slot) and best-effort
/// delete the stored copy — but only when it lives inside the task's own
/// artifacts dir, so a URL or a hand-attached external path is never touched.
/// Returns the persisted state plus the removed record.
pub fn task_artifact_remove(task_id: &str, index: usize) -> Result<(TaskState, Artifact), OpError> {
    let state = task_store::read_task_state(task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    if index >= state.artifacts.len() {
        return Err(OpError::NotFound(format!(
            "task {} has no artifact #{}",
            task_id, index
        )));
    }
    // State first, file second: a crash in between leaves an orphan file
    // (harmless), while the reverse order would leave a record pointing at
    // nothing.
    let mut removed: Option<Artifact> = None;
    let state = update_task(task_id, |s| {
        if index < s.artifacts.len() {
            removed = Some(s.artifacts.remove(index));
            s.lead_artifact = match s.lead_artifact {
                Some(lead) if lead == index => None,
                Some(lead) if lead > index => Some(lead - 1),
                other => other,
            };
        }
    })?;
    let removed = removed.ok_or_else(|| OpError::Conflict {
        msg: format!(
            "artifact #{} vanished before removal (concurrent edit?)",
            index
        ),
        recipe: None,
    })?;

    let artifacts_dir = task_store::task_dir(task_id)
        .ok_or_else(|| OpError::Other("no home dir".into()))?
        .join("artifacts");
    let stored = PathBuf::from(&removed.path);
    if stored.starts_with(&artifacts_dir) {
        if let Err(e) = std::fs::remove_file(&stored) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "artifact remove: file cleanup failed for {}: {}",
                    removed.path,
                    e
                );
            }
        }
    }
    Ok((state, removed))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_note_identical_to_the_newest_one_is_refused() {
        crate::test_util::with_temp_home(|| {
            let mut state = TaskState::new("Semantic Linter".into());
            state.task_id = "tk-dup".into();
            task_store::write_task_state(&state).expect("write card");

            task_artifact_add_text("tk-dup", "Waiting: which branch?", "cli").expect("first");
            match task_artifact_add_text("tk-dup", "Waiting: which branch?\n", "cli") {
                Err(OpError::Usage(msg)) => assert!(msg.contains("already says that"), "{msg}"),
                other => panic!(
                    "the same note twice must be refused, got {:?}",
                    other.map(|s| s.artifacts.len())
                ),
            }
            // An older note saying the same thing is history, not a repeat.
            task_artifact_add_text("tk-dup", "Posted: asked", "cli").expect("posted");
            let state = task_artifact_add_text("tk-dup", "Waiting: which branch?", "cli")
                .expect("a wait after a Posted is a new note");
            assert_eq!(state.artifacts.len(), 3);
        });
    }

    /// The card's record is what moves it: a session that writes the `PR:`
    /// does not also have to remember to move the card, and a board that
    /// shows the PR in Review is one the note alone kept honest.
    #[test]
    fn a_pr_note_carries_a_board_card_into_review() {
        crate::test_util::with_temp_home(|| {
            let mut state = TaskState::new("Semantic Linter".into());
            state.task_id = "tk-pr".into();
            state.status = TaskStatus::Running;
            task_store::write_task_state(&state).expect("write card");

            let after = task_artifact_add_text(
                "tk-pr",
                "PR: https://example.com/pull-requests/42\n\nDraft, tests green.",
                "cli",
            )
            .expect("note");
            assert_eq!(after.status, TaskStatus::Review);
        });
    }

    /// Only the PR word moves a card. Everything else a session writes —
    /// a wait, a candidate branch, a failure — is still work in progress.
    #[test]
    fn any_other_note_leaves_the_card_where_it_is() {
        crate::test_util::with_temp_home(|| {
            let mut state = TaskState::new("Semantic Linter".into());
            state.task_id = "tk-plain".into();
            state.status = TaskStatus::Running;
            task_store::write_task_state(&state).expect("write card");

            let after =
                task_artifact_add_text("tk-plain", "Candidate: fix/linter", "cli").expect("note");
            assert_eq!(after.status, TaskStatus::Running);
        });
    }
}
