//! `cc-hub board ...` — the personal Tasks board (`lib/src/tasks.rs`).
//!
//! `add` mints a card in To-Do the way the `a` key does in the TUI. It exists
//! so a script can hand the user a card — the escalation record — and then
//! open it with a `cc-hub://task` link, which only ever addresses a card that
//! already exists.
//!
//! `note` and `notes` are how a task session writes on the card and reads
//! what was written: the brief it agreed with the user, the branch it built,
//! what verification found. The notes are the same `note` attachments the
//! `p` key pastes, so they show on the board where the user already looks,
//! and `cc-hub open` refuses a hand-over for a card that has none.

use std::io::{IsTerminal, Read};

use super::{parse_flags, print_json, CliError};
use cc_hub_lib::ops;
use cc_hub_lib::orchestrator::{self, TaskPriority};
use cc_hub_lib::tasks::{parse_tags, PersonalBoard};

const VERBS: &str = "`add`, `note` or `notes`";

pub(crate) fn board_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage(format!("board <verb>: missing verb (try {})", VERBS)))?;
    match verb.as_str() {
        "add" => board_add(rest),
        "note" => board_note(rest),
        "notes" => board_notes(rest),
        other => Err(CliError::Usage(format!(
            "unknown board verb: {} (try {})",
            other, VERBS
        ))),
    }
}

/// `cc-hub board add --text TEXT [--title TEXT] [--tags "a b"] [--priority
/// p1..p4] [--kind WORD]`
///
/// The card lands in To-Do with no session; nothing is spawned. Emits
/// `{"ok":true,"task_id":"tk-…"}`. `--kind` is the deliverable the task
/// router places the card by, and must be one the config declares — the same
/// list the board's `T` picker offers.
fn board_add(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let text = f
        .text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| CliError::Usage("--text is required".into()))?;
    let tags = f.tags.as_deref().map(parse_tags).unwrap_or_default();
    let priority = match f.priority.as_deref() {
        None => TaskPriority::default(),
        Some(p) => parse_priority(p)?,
    };
    let kind = match f.kind.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        None => None,
        Some(kind) => Some(known_kind(kind)?),
    };

    let mut board =
        PersonalBoard::load_result().map_err(|e| CliError::Other(format!("load board: {}", e)))?;
    let task_id = board
        .add_configured(text, tags, priority)
        .map_err(|e| CliError::Other(format!("write card: {}", e)))?
        .expect("non-empty text yields a card");
    if let Some(kind) = kind {
        board
            .set_kind(&task_id, Some(kind))
            .map_err(|e| CliError::Other(format!("write kind: {}", e)))?;
    }
    if let Some(title) = f.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        orchestrator::set_task_title(None, &task_id, title)
            .map_err(|e| CliError::Other(format!("write title: {}", e)))?;
    }

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": task_id,
        "status": "todo",
    }));
    Ok(())
}

/// `cc-hub board note --task ID [--text TEXT]`
///
/// Append a note to the card: `--text`, or stdin when there is none, so a
/// long brief can arrive as a heredoc without a temp file. The note lands
/// as a `<ts>-note.md` in the card's artifacts dir, captioned by its first
/// line. Emits `{"ok":true,"task_id":…,"note":{…},"count":N}`.
fn board_note(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = super::require_task(&f)?;
    let text = match f.text.clone() {
        Some(text) => text,
        None => piped_stdin()?,
    };
    let state = ops::task::task_artifact_add_text(None, &task_id, &text, "cli")?;
    let added = state.artifacts.last().expect("just pushed");
    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "note": {
            "path": added.path,
            "caption": added.caption,
            "added_at": added.added_at,
        },
        "count": state.artifacts.iter().filter(|a| a.kind == "note").count(),
    }));
    Ok(())
}

/// The note's text when it was piped in. A terminal on stdin means the
/// caller forgot `--text`, which is a usage error rather than a hang.
fn piped_stdin() -> Result<String, CliError> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(CliError::Usage(
            "--text is required (or pipe the note on stdin)".into(),
        ));
    }
    let mut text = String::new();
    stdin
        .read_to_string(&mut text)
        .map_err(|e| CliError::Other(format!("read stdin: {}", e)))?;
    Ok(text)
}

/// `cc-hub board notes --task ID [--json]`
///
/// The card's notes in attach order: the record a session reads before it
/// acts on a card. Plain output separates notes by a dated rule; `--json`
/// emits `{"ok":true,"notes":[{added_at,caption,path,text}…]}`.
fn board_notes(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = super::require_task(&f)?;
    let notes = ops::task::task_notes(&task_id)?;
    if f.json {
        let arr: Vec<serde_json::Value> = notes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "added_at": n.added_at,
                    "caption": n.caption,
                    "path": n.path,
                    "text": n.text,
                })
            })
            .collect();
        print_json(&serde_json::json!({ "ok": true, "task_id": task_id, "notes": arr }));
        return Ok(());
    }
    for (i, note) in notes.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("── {} · {}", when(note.added_at), note.path);
        println!("{}", note.text);
    }
    Ok(())
}

fn when(unix_secs: i64) -> String {
    chrono::DateTime::from_timestamp(unix_secs, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| unix_secs.to_string())
}

/// A kind is only a kind if `[tasks].kinds` says so: one list behind the
/// board picker, this flag, and the routing table the agent reads.
fn known_kind(kind: &str) -> Result<String, CliError> {
    let kinds = &cc_hub_lib::config::get().tasks.kinds;
    if kinds.iter().any(|k| k == kind) {
        return Ok(kind.to_string());
    }
    Err(CliError::Usage(if kinds.is_empty() {
        "--kind: no task kinds configured — set [tasks].kinds in ~/.cc-hub/config.toml".into()
    } else {
        format!("--kind {}: expected one of {}", kind, kinds.join(", "))
    }))
}

fn parse_priority(s: &str) -> Result<TaskPriority, CliError> {
    match s.to_ascii_lowercase().as_str() {
        "p1" => Ok(TaskPriority::P1),
        "p2" => Ok(TaskPriority::P2),
        "p3" => Ok(TaskPriority::P3),
        "p4" => Ok(TaskPriority::P4),
        other => Err(CliError::Usage(format!(
            "--priority {}: expected p1, p2, p3 or p4",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_util::with_tempdir_home;
    use cc_hub_lib::orchestrator::TaskStatus;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_mints_a_todo_card_with_its_metadata() {
        with_tempdir_home(|| {
            board_add(&argv(&[
                "--text",
                "Repair agent meetings: halted on daily budget",
                "--title",
                "Repair: meetings",
                "--tags",
                "repair agents",
                "--priority",
                "p1",
            ]))
            .expect("add succeeds");

            let board = PersonalBoard::load();
            let card = board.tasks().first().expect("one card");
            assert!(card.task_id.starts_with("tk-"));
            assert_eq!(card.status, TaskStatus::Backlog);
            assert_eq!(card.prompt, "Repair agent meetings: halted on daily budget");
            assert_eq!(card.title.as_deref(), Some("Repair: meetings"));
            assert_eq!(card.tags, vec!["repair", "agents"]);
            assert_eq!(card.priority, TaskPriority::P1);
            assert!(card.tmux.is_none(), "add spawns nothing");
        });
    }

    #[test]
    fn notes_are_read_back_in_attach_order() {
        with_tempdir_home(|| {
            board_add(&argv(&["--text", "Cache research"])).expect("add");
            let id = PersonalBoard::load().tasks()[0].task_id.clone();
            board_note(&argv(&[
                "--task",
                &id,
                "--text",
                "Problem: the cache is cold",
            ]))
            .expect("first note");
            board_note(&argv(&[
                "--task",
                &id,
                "--text",
                "Candidate: branch fix/cache",
            ]))
            .expect("second note");

            let notes = ops::task::task_notes(&id).expect("notes");
            let texts: Vec<&str> = notes.iter().map(|n| n.text.as_str()).collect();
            assert_eq!(
                texts,
                ["Problem: the cache is cold", "Candidate: branch fix/cache"]
            );
            assert_eq!(
                notes[0].caption.as_deref(),
                Some("Problem: the cache is cold")
            );
            assert!(board_notes(&argv(&["--task", &id, "--json"])).is_ok());
        });
    }

    #[test]
    fn a_note_needs_a_card_and_some_text() {
        with_tempdir_home(|| {
            assert!(matches!(
                board_note(&argv(&["--task", "tk-none", "--text", "x"])),
                Err(CliError::Other(_))
            ));
            board_add(&argv(&["--text", "A card"])).expect("add");
            let id = PersonalBoard::load().tasks()[0].task_id.clone();
            assert!(matches!(
                board_note(&argv(&["--task", &id, "--text", "  \n"])),
                Err(CliError::Usage(_))
            ));
            assert!(ops::task::task_notes(&id).expect("notes").is_empty());
        });
    }

    #[test]
    fn add_needs_text_and_a_known_priority() {
        with_tempdir_home(|| {
            assert!(matches!(
                board_add(&argv(&["--text", "  "])),
                Err(CliError::Usage(_))
            ));
            assert!(matches!(
                board_add(&argv(&["--text", "x", "--priority", "urgent"])),
                Err(CliError::Usage(_))
            ));
            assert!(PersonalBoard::load().tasks().is_empty());
        });
    }
}
