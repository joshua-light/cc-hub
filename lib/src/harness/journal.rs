use super::now_unix;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub at: i64,
    /// `info` | `warn`.
    pub level: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    pub tick: u64,
}

pub fn notes_path(dir: &Path) -> PathBuf {
    dir.join("notes.jsonl")
}

pub fn append_note(dir: &Path, note: &Note) -> io::Result<()> {
    use std::io::Write;
    fs::create_dir_all(dir)?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(notes_path(dir))?;
    writeln!(
        f,
        "{}",
        serde_json::to_string(note).map_err(io::Error::other)?
    )
}

/// The newest `limit` notes, newest first.
pub fn read_notes(dir: &Path, limit: usize) -> Vec<Note> {
    let Ok(raw) = fs::read_to_string(notes_path(dir)) else {
        return Vec::new();
    };
    let mut notes: Vec<Note> = raw
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    notes.reverse();
    notes.truncate(limit);
    notes
}

// ---- events ---------------------------------------------------------------
//
// The harness's own log, one line per thing worth knowing later: a run
// started or ended, a poll command failed, the agent halted, someone changed
// it from the hub. The Agents tab reads it to answer "why didn't it run?" —
// questions the per-run transcripts can't, because no run happened.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogLine {
    pub at: i64,
    /// `info` | `warn` | `error`.
    pub level: String,
    pub text: String,
}

pub fn events_path(dir: &Path) -> PathBuf {
    dir.join("events.jsonl")
}

/// Past this the log rolls over to `events.1.jsonl`, keeping one old file.
const EVENTS_MAX_BYTES: u64 = 256 * 1024;

/// Append a line to the agent's event log. Best effort: a log that can't be
/// written must never stop a run.
pub fn log_event(dir: &Path, level: &str, text: impl Into<String>) {
    use std::io::Write;
    let path = events_path(dir);
    if fs::metadata(&path).is_ok_and(|m| m.len() > EVENTS_MAX_BYTES) {
        let _ = fs::rename(&path, dir.join("events.1.jsonl"));
    }
    let line = LogLine {
        at: now_unix(),
        level: level.into(),
        text: text.into(),
    };
    let Ok(json) = serde_json::to_string(&line) else {
        return;
    };
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{}", json));
}

/// The newest `limit` log lines, newest first.
pub fn read_events(dir: &Path, limit: usize) -> Vec<LogLine> {
    let Ok(raw) = fs::read_to_string(events_path(dir)) else {
        return Vec::new();
    };
    raw.lines()
        .rev()
        .filter_map(|l| serde_json::from_str(l).ok())
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_log_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        log_event(tmp.path(), "info", "one");
        log_event(tmp.path(), "warn", "two");
        let lines = read_events(tmp.path(), 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "two");
        assert_eq!(lines[0].level, "warn");
    }

    #[test]
    fn notes_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..3 {
            append_note(
                tmp.path(),
                &Note {
                    at: i,
                    level: "info".into(),
                    text: format!("n{i}"),
                    r#ref: None,
                    tick: 1,
                },
            )
            .unwrap();
        }
        let notes = read_notes(tmp.path(), 2);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "n2");
    }
}
