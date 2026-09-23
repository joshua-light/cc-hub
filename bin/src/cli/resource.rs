//! Subscription routing uses the same Python runtime as the durable Task workflow.
//! Bundle the broker in the binary so installed hubs do not depend on a checkout.
use super::CliError;
use std::hash::{Hash, Hasher};
use std::io::Write;

pub(crate) fn resource(args: &[String]) -> Result<(), CliError> {
    if args.first().map(String::as_str) == Some("_notify") {
        let value: serde_json::Value = serde_json::from_str(
            args.get(1)
                .ok_or_else(|| CliError::Usage("missing notice".into()))?,
        )
        .map_err(|e| CliError::Usage(e.to_string()))?;
        let tmux = value["tmux"]
            .as_str()
            .ok_or_else(|| CliError::Usage("missing tmux".into()))?;
        let text = value["text"]
            .as_str()
            .ok_or_else(|| CliError::Usage("missing text".into()))?;
        let sent = cc_hub_lib::send::pane_ready_for_input(tmux);
        if sent {
            cc_hub_lib::send::send_notice(tmux, text)
                .map_err(|e| CliError::Other(e.to_string()))?;
        }
        super::print_json(&serde_json::json!({"ok": true, "sent": sent}));
        return Ok(());
    }
    // The broker names a Claude session before launching it, so the hub
    // never sees it nameless and never opens the rename prompt for it.
    if args.first().map(String::as_str) == Some("_name") {
        cc_hub_lib::resources::name_session(&worker(args)?)
            .map_err(|e| CliError::Other(format!("name session: {e}")))?;
        super::print_json(&serde_json::json!({"ok": true}));
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("_bind") {
        let value = worker(args)?;
        let field = |key: &str| {
            value[key]
                .as_str()
                .ok_or_else(|| CliError::Usage(format!("missing {key}")))
        };
        let task = field("task")?;
        let sid = value["session_id"].as_str();
        // One worker at a time holds a task, whatever its role, so the card
        // always points at the worker that was bound last.
        cc_hub_lib::tasks::PersonalBoard::load_result()
            .map_err(|e| CliError::Other(e.to_string()))?
            .bind_resource(task, field("cwd")?, field("account")?, field("tmux")?, sid)
            .map_err(|e| CliError::Other(e.to_string()))?;
        if let Some(sid) = sid {
            cc_hub_lib::session_tasks::link(
                sid,
                cc_hub_lib::session_tasks::TaskLink {
                    task_id: task.into(),
                    title: format!("{} / {}", task, field("role")?),
                },
            )
            .map_err(|e| CliError::Other(e.to_string()))?;
            // A Codex session is first seen here, under the id it minted
            // itself; a Claude one was named before launch and keeps it.
            // Cosmetic, unlike the binding: a failure costs the name.
            if let Err(e) = cc_hub_lib::resources::name_session(&value) {
                log::warn!("resource bind: naming session {} failed: {}", sid, e);
            }
        }
        super::print_json(&serde_json::json!({"ok": true}));
        return Ok(());
    }
    let source = include_str!("../../../lib/src/resource_manager.py");
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hash);
    let dir = cc_hub_lib::platform::paths::cache_dir().join("resources");
    std::fs::create_dir_all(&dir).map_err(|e| CliError::Other(e.to_string()))?;
    let path = dir.join(format!("broker-{:x}.py", hash.finish()));
    // Atomic publication: another process can execute the same broker concurrently.
    if !path.exists() {
        let temporary = dir.join(format!("broker-{}.tmp", std::process::id()));
        let mut file =
            std::fs::File::create(&temporary).map_err(|e| CliError::Other(e.to_string()))?;
        file.write_all(source.as_bytes())
            .map_err(|e| CliError::Other(e.to_string()))?;
        std::fs::rename(&temporary, &path).map_err(|e| CliError::Other(e.to_string()))?;
    }
    let executable = std::env::current_exe().map_err(|e| CliError::Other(e.to_string()))?;
    let status = std::process::Command::new("python3")
        .arg(path)
        .args(args)
        .env("CC_HUB_BINARY", executable)
        .status()
        .map_err(|e| CliError::Other(format!("resource broker requires Python 3.11+: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::Reported("resource operation failed".into()))
    }
}

/// The worker record the broker passes to its `_name` / `_bind` callbacks.
fn worker(args: &[String]) -> Result<serde_json::Value, CliError> {
    serde_json::from_str(
        args.get(1)
            .ok_or_else(|| CliError::Usage("missing worker".into()))?,
    )
    .map_err(|e| CliError::Usage(e.to_string()))
}
