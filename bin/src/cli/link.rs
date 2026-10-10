//! `cc-hub open <url>` — act on a `cc-hub://` deep link.
//!
//! This is the verb a locally configured OS URL-scheme handler calls,
//! so a browser button can start a hub session — and the verb a persistent
//! agent calls to hand a board card to a real session. A task link whose
//! card already has a live session in that directory reaches that session
//! (`"reused": true`) rather than starting a second one. A link naming a
//! `role` is a hand-over: it starts a fresh session and closes the card's old
//! one in that directory after this command has reported. With accounts
//! configured the broker also treats a change of directory as a hand-over
//! and stops the old worker itself. A chore link — a fix or a merge of the
//! target branch — files a board card first and is then worked like a task
//! link's card: through the broker when accounts are configured, so the chore
//! runs on whichever subscription has room, and the card moves to Running
//! once its worker is bound. It is also handy by hand:
//! `cc-hub open 'cc-hub://review?depth=light&pr=…' --dry-run` shows where a
//! link would land without spawning anything.

use super::{parse_flags, print_json, report_prompt_status, CliError};
use cc_hub_lib::link::Link;
use cc_hub_lib::ops;

pub(crate) fn open(args: &[String]) -> Result<(), CliError> {
    let (url, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("open <url>: missing url".into()))?;
    let f = parse_flags(rest)?;
    let link: Link = url
        .parse()
        .map_err(|e| CliError::Usage(format!("open {}: {}", url, e)))?;

    if f.dry_run {
        let target = ops::link::target(&link, f.agent.as_deref())?;
        print_json(&serde_json::json!({
            "ok": true,
            "dry_run": true,
            "kind": link.kind(),
            "cwd": target.cwd,
            "agent_id": target.agent_id,
            "title": target.title,
            "prompt": target.prompt,
        }));
        return Ok(());
    }

    // A hand-over closes the card's previous session, but only after this
    // command has reported: the caller is usually that session.
    let superseded = match &link {
        Link::Task(task) if task.is_handover() => ops::link::session_to_supersede(task)?,
        _ => None,
    };
    let close_superseded = || {
        if let Some(tmux) = &superseded {
            if let Err(e) = cc_hub_lib::send::kill_tmux_session(tmux) {
                log::warn!("open: closing superseded session {} failed: {}", tmux, e);
            }
        }
    };

    if f.agent.is_none() && !cc_hub_lib::resources::accounts().is_empty() {
        if let Some(chore) = link.chore() {
            let target = ops::link::target(&link, None)?;
            let card = ops::link::file_chore(chore)?;
            let started = broker_start(
                card.to_string(),
                chore.kind().unwrap_or("basic").into(),
                "implementation".into(),
                target.cwd.to_string_lossy().into(),
                chore.prompt_for(&card),
                target.title,
            );
            if started.is_ok() {
                ops::link::start_card(card.as_str())?;
            }
            return started;
        }
        if let Link::Task(task) = &link {
            if let Some(kind) = task
                .kind
                .clone()
                .or_else(|| cc_hub_lib::resources::task_kind(task.id.as_str()))
            {
                let target = ops::link::target(&link, None)?;
                let started = broker_start(
                    task.id.to_string(),
                    kind,
                    task.role.clone().unwrap_or_else(|| "implementation".into()),
                    target.cwd.to_string_lossy().into(),
                    format!(
                        "Read ~/.claude/skills/task/SKILL.md and follow it for:\n{}",
                        target.prompt
                    ),
                    target.title,
                );
                if started.is_ok() {
                    close_superseded();
                }
                return started;
            }
        }
    }
    let opened = ops::link::open(
        &link,
        ops::link::OpenOpts {
            agent: f.agent.clone(),
            wait_secs: f.wait_secs,
        },
    )?;
    let prompt_status = report_prompt_status(&opened.prompt_status);

    print_json(&serde_json::json!({
        "ok": true,
        "kind": link.kind(),
        "tmux": opened.tmux,
        "reused": opened.reused,
        "session_id": opened.session_id,
        "task_id": opened.task_id,
        "cwd": opened.target.cwd,
        "agent_id": opened.target.agent_id,
        "title": opened.target.title,
        "prompt": opened.target.prompt,
        "prompt_status": prompt_status,
        "superseded": opened.superseded,
    }));
    close_superseded();
    Ok(())
}

/// Start a worker for `task` through the resource broker.
fn broker_start(
    task: String,
    kind: String,
    role: String,
    cwd: String,
    prompt: String,
    title: String,
) -> Result<(), CliError> {
    super::resource::resource(&[
        "start".into(),
        "--task".into(),
        task,
        "--kind".into(),
        kind,
        "--role".into(),
        role,
        "--cwd".into(),
        cwd,
        "--prompt".into(),
        prompt,
        "--title".into(),
        title,
    ])
}

#[cfg(test)]
mod tests {
    use crate::cli::dispatch;
    use crate::test_util::argv;

    #[test]
    fn open_without_url_is_a_usage_error() {
        assert_eq!(dispatch(&argv(&["open"])), Some(2));
    }

    #[test]
    fn open_with_foreign_scheme_is_a_usage_error() {
        assert_eq!(
            dispatch(&argv(&["open", "https://example.com", "--dry-run"])),
            Some(2)
        );
    }

    #[test]
    fn open_with_unknown_kind_is_a_usage_error() {
        assert_eq!(
            dispatch(&argv(&["open", "cc-hub://deploy?pr=x", "--dry-run"])),
            Some(2)
        );
    }
}
