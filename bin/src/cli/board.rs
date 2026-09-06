//! `cc-hub board ...` — the personal Tasks board (`lib/src/tasks.rs`).
//!
//! One verb today: `add`, which mints a card in To-Do the way the `a` key
//! does in the TUI. It exists so a script can hand the user a card — the
//! escalation record — and then open it with a `cc-hub://task` link, which
//! only ever addresses a card that already exists.

use super::{parse_flags, print_json, CliError};
use cc_hub_lib::orchestrator::{self, TaskPriority};
use cc_hub_lib::tasks::{parse_tags, PersonalBoard};

const VERBS: &str = "`add`";

pub(crate) fn board_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage(format!("board <verb>: missing verb (try {})", VERBS)))?;
    match verb.as_str() {
        "add" => board_add(rest),
        other => Err(CliError::Usage(format!(
            "unknown board verb: {} (try {})",
            other, VERBS
        ))),
    }
}

/// `cc-hub board add --text TEXT [--title TEXT] [--tags "a b"] [--priority p1..p4]`
///
/// The card lands in To-Do with no session; nothing is spawned. Emits
/// `{"ok":true,"task_id":"tk-…"}`.
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

    let mut board = PersonalBoard::load_result()
        .map_err(|e| CliError::Other(format!("load board: {}", e)))?;
    let task_id = board
        .add_configured(text, tags, priority)
        .map_err(|e| CliError::Other(format!("write card: {}", e)))?
        .expect("non-empty text yields a card");
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
