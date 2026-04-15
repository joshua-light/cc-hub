use crate::conversation;
use crate::models::{RawSession, SessionDetail, SessionInfo, SessionState};
use std::path::{Path, PathBuf};

fn claude_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

fn sessions_dir() -> Option<PathBuf> {
    claude_dir().map(|d| d.join("sessions"))
}

fn projects_dir() -> Option<PathBuf> {
    claude_dir().map(|d| d.join("projects"))
}

fn encode_path(path: &str) -> String {
    path.replace('/', "-").replace('.', "-")
}

fn find_jsonl(cwd: &str, session_id: &str) -> Option<PathBuf> {
    let projects = projects_dir()?;
    let encoded = encode_path(cwd);
    let jsonl_path = projects.join(&encoded).join(format!("{}.jsonl", session_id));
    if jsonl_path.exists() {
        Some(jsonl_path)
    } else {
        None
    }
}

fn is_pid_alive(pid: u32) -> bool {
    // kill(pid, 0) checks process existence without sending a signal
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn read_raw_sessions() -> Vec<RawSession> {
    let dir = match sessions_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(raw) = serde_json::from_str::<RawSession>(&contents) {
                sessions.push(raw);
            }
        }
    }

    sessions
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

pub fn scan_sessions() -> Vec<SessionInfo> {
    let raw_sessions = read_raw_sessions();

    let mut sessions: Vec<SessionInfo> = raw_sessions
        .into_iter()
        .map(|raw| {
            let alive = is_pid_alive(raw.pid);
            let jsonl_path = find_jsonl(&raw.cwd, &raw.session_id);

            let (state, last_user_message, last_activity, git_branch, model, version) =
                match &jsonl_path {
                    Some(path) => {
                        let entries = conversation::read_jsonl_tail(path, 8192);
                        let state = if alive {
                            conversation::extract_state(&entries)
                        } else {
                            SessionState::Dead
                        };
                        let last_msg = conversation::extract_last_user_message(&entries);
                        let last_act = conversation::extract_last_activity(&entries);
                        let (branch, mdl, ver) = conversation::extract_metadata(&entries);
                        (state, last_msg, last_act, branch, mdl, ver)
                    }
                    None => {
                        let state = if alive {
                            SessionState::Idle
                        } else {
                            SessionState::Dead
                        };
                        (state, None, None, None, None, None)
                    }
                };

            SessionInfo {
                pid: raw.pid,
                session_id: raw.session_id,
                project_name: project_name(&raw.cwd),
                cwd: raw.cwd,
                started_at: raw.started_at,
                last_activity,
                state,
                alive,
                last_user_message,
                model,
                git_branch,
                version,
            }
        })
        .collect();

    sessions.sort_by(|a, b| {
        a.state
            .sort_key()
            .cmp(&b.state.sort_key())
            .then_with(|| {
                let a_time = a.last_activity.unwrap_or(a.started_at);
                let b_time = b.last_activity.unwrap_or(b.started_at);
                b_time.cmp(&a_time)
            })
    });

    sessions
}

pub fn load_detail(session_id: &str, sessions: &[SessionInfo]) -> Option<SessionDetail> {
    let info = sessions.iter().find(|s| s.session_id == session_id)?;
    let jsonl_path = find_jsonl(&info.cwd, session_id)?;
    let entries = conversation::read_jsonl_tail(&jsonl_path, 65536);

    let recent_messages = conversation::extract_messages(&entries, 15);
    let (total_input_tokens, total_output_tokens) = conversation::extract_token_totals(&entries);

    Some(SessionDetail {
        info: info.clone(),
        recent_messages,
        total_input_tokens,
        total_output_tokens,
    })
}
