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

    // Skip the titler's/triage scratch project dir: its JSONLs are one-shot
    // `claude -p` runs cc-hub itself spawns, not real sessions — the same
    // exclusion scanner.rs applies. Sharing the predicate keeps both aligned.
    let scratch_proj_dir = crate::scanner::scratch_project_dir_name();

    let mut counts = SessionCounts::default();
    for proj in proj_iter.flatten() {
        if let Some(skip) = scratch_proj_dir.as_deref() {
            if proj.file_name().to_str() == Some(skip) {
                continue;
            }
        }
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
            let created: SystemTime = meta
                .created()
                .or_else(|_| meta.modified())
                .unwrap_or(now.into());
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

// `$HOME` redirection only isolates `dirs::home_dir()` on unix; on Windows it
// resolves via the profile API and ignores `$HOME`, so gate the test there.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;

    #[test]
    fn scratch_summary_sessions_are_not_counted() {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        // CLAUDE_CONFIG_DIR would override the HOME-derived layout — clear it
        // so claude_home() points at the temp home for the duration.
        let prev_cfg = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::env::set_var("HOME", home.path());

        let projects = paths::claude_home().expect("claude_home").join("projects");
        let real = projects.join("-home-me-proj");
        // The scratch project dir is exactly what scanner.rs skips.
        let scratch = projects
            .join(crate::scanner::scratch_project_dir_name().expect("scratch project dir name"));
        fs::create_dir_all(&real).unwrap();
        fs::create_dir_all(&scratch).unwrap();
        // Both JSONLs are created now, so both land in today's window.
        fs::write(real.join("a.jsonl"), "{}\n").unwrap();
        fs::write(scratch.join("b.jsonl"), "{}\n").unwrap();

        let counts = count_recent_sessions();

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_cfg {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }

        // Only the real session is counted; the scratch one is excluded.
        assert_eq!(counts.today, 1);
        assert_eq!(counts.week, 1);
    }
}
