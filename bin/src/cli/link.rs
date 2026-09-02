//! `cc-hub open <url>` — act on a `cc-hub://` deep link.
//!
//! This is the verb the OS URL-scheme handler calls (see `contrib/macos/`),
//! so a browser button can start a hub session. It is also handy by hand:
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
        "session_id": opened.session_id,
        "cwd": opened.target.cwd,
        "agent_id": opened.target.agent_id,
        "title": opened.target.title,
        "prompt": opened.target.prompt,
        "prompt_status": prompt_status,
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cli::dispatch;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

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
