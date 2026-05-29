//! Counts of Claude Code session JSONLs whose files were created today or
//! this calendar week (ISO, Monday-anchored). Walks `~/.claude/projects/`
//! and reads each JSONL's `created()` metadata — pure file-system work, no
//! parsing.

use crate::platform::paths;
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use std::fs;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, Default)]
pub struct SessionCounts {
    pub today: u32,
    pub week: u32,
}

pub fn count_recent_sessions() -> SessionCounts {
    let Some(projects) = paths::claude_home().map(|d| d.join("projects")) else {
        return SessionCounts::default();
    };
    let Ok(proj_iter) = fs::read_dir(&projects) else {
        return SessionCounts::default();
    };

    let now = Local::now();
    let today: NaiveDate = now.date_naive();
    let week_start: NaiveDate =
        today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);

    let mut counts = SessionCounts::default();
    for proj in proj_iter.flatten() {
        let Ok(files) = fs::read_dir(proj.path()) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = f.metadata() else { continue };
            // `created()` returns birthtime on macOS / creationTime on
            // Windows; on Linux it may be `Err(Unsupported)`, in which
            // case we fall back to mtime.
            let created: SystemTime = meta.created().or_else(|_| meta.modified()).unwrap_or(now.into());
            let Ok(dur) = created.duration_since(SystemTime::UNIX_EPOCH) else {
                continue;
            };
            let Some(local_dt) = Local.timestamp_opt(dur.as_secs() as i64, 0).single() else {
                continue;
            };
            let date = local_dt.date_naive();
            if date == today {
                counts.today += 1;
            }
            if date >= week_start {
                counts.week += 1;
            }
        }
    }
    counts
}
