//! Terminal discovery: the plain shells `t` opens, found by the name of the
//! tmux session that hosts each one. A terminal has no transcript, so all it
//! reports is where it started, what it runs now and when it last printed.

use crate::models::{SessionInfo, SessionKind, SessionState};
use crate::platform::mux;
use crate::sessions::common::project_name;
use std::collections::HashMap;

/// Stem of every terminal's tmux session name (`hal-sh-<pid>-<nanos>`).
pub const TMUX_PREFIX: &str = "hal-sh";

/// The `agent_id` a terminal's card carries.
pub const AGENT_ID: &str = "terminal";

/// One pane per line: session, pane pid, start directory, created and last
/// activity (unix seconds), and the pane's foreground command.
const FORMAT: &str = "#{session_name}\t#{pane_pid}\t#{session_path}\t#{session_created}\t#{session_activity}\t#{pane_current_command}";

pub fn scan(titles: &HashMap<String, String>) -> Vec<SessionInfo> {
    parse(&mux::list_panes_as(FORMAT), titles)
}

/// The terminals in `lines` of [`FORMAT`]. A terminal the user split into
/// several panes is one session: its first pane speaks for it.
fn parse(lines: &[String], titles: &HashMap<String, String>) -> Vec<SessionInfo> {
    let mut terminals: Vec<SessionInfo> = Vec::new();
    for line in lines {
        let Some(terminal) = terminal(line, titles) else {
            continue;
        };
        if terminals
            .iter()
            .all(|t| t.session_id != terminal.session_id)
        {
            terminals.push(terminal);
        }
    }
    terminals
}

fn terminal(line: &str, titles: &HashMap<String, String>) -> Option<SessionInfo> {
    let mut fields = line.split('\t');
    let name = fields.next()?;
    if !is_terminal(name) {
        return None;
    }
    let pid = fields.next()?.parse().ok()?;
    let cwd = fields.next()?.to_string();
    let created: u64 = fields.next()?.parse().ok()?;
    let activity: u64 = fields.next()?.parse().ok()?;
    let command = fields.next().filter(|c| !c.is_empty());
    Some(SessionInfo {
        agent_id: AGENT_ID.into(),
        kind: SessionKind::Terminal,
        pid,
        session_id: name.to_string(),
        project_name: project_name(&cwd),
        cwd,
        started_at: created * 1000,
        last_activity: Some(activity * 1000),
        state: SessionState::Idle,
        last_user_message: None,
        summary: command.map(str::to_string),
        title: titles.get(name).cloned(),
        titling: false,
        model: None,
        git_branch: None,
        version: None,
        jsonl_path: None,
        tmux_session: Some(name.to_string()),
        current_tool: None,
        is_thinking: false,
        context_tokens: None,
        tool_uses_count: 0,
    })
}

fn is_terminal(tmux: &str) -> bool {
    tmux.strip_prefix(TMUX_PREFIX)
        .is_some_and(|rest| rest.starts_with('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn a_terminal_is_found_by_its_session_name() {
        let terminals = parse(
            &lines(&[
                "hal-sh-1-2\t4242\t/tmp/proj\t1700000000\t1700000060\tcargo",
                "hal-1-3\t4343\t/tmp/proj\t1700000000\t1700000060\tclaude",
                "hal-shell\t4444\t/tmp/proj\t1700000000\t1700000060\tzsh",
            ]),
            &HashMap::new(),
        );
        assert_eq!(terminals.len(), 1);
        let t = &terminals[0];
        assert_eq!(t.kind, SessionKind::Terminal);
        assert_eq!(t.session_id, "hal-sh-1-2");
        assert_eq!(t.tmux_session.as_deref(), Some("hal-sh-1-2"));
        assert_eq!(t.pid, 4242);
        assert_eq!(t.cwd, "/tmp/proj");
        assert_eq!(t.project_name, "proj");
        assert_eq!(t.started_at, 1_700_000_000_000);
        assert_eq!(t.last_activity, Some(1_700_000_060_000));
        assert_eq!(t.summary.as_deref(), Some("cargo"));
    }

    #[test]
    fn a_split_terminal_is_one_session() {
        let terminals = parse(
            &lines(&[
                "hal-sh-1-2\t4242\t/tmp/proj\t1700000000\t1700000060\tzsh",
                "hal-sh-1-2\t4243\t/tmp/proj\t1700000000\t1700000060\tvim",
            ]),
            &HashMap::new(),
        );
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0].pid, 4242);
    }

    #[test]
    fn a_renamed_terminal_keeps_its_name() {
        let titles = HashMap::from([("hal-sh-1-2".to_string(), "Server".to_string())]);
        let terminals = parse(
            &lines(&["hal-sh-1-2\t4242\t/tmp/proj\t1700000000\t1700000060\tzsh"]),
            &titles,
        );
        assert_eq!(terminals[0].title.as_deref(), Some("Server"));
    }
}
