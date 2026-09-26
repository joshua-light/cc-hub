//! The session archive: every transcript on disk, however old.
//!
//! The scanner deliberately looks at a recent window — its job is the live
//! grid. This module is the other half of that decision: when the user wants
//! a session from weeks ago (by saved title, by id, by what it was about),
//! the archive walks the full transcript stores and returns one flat,
//! newest-first list for the session finder to fuzzy-search.
//!
//! Per transcript only the head is read (the same few KiB the scanner reads),
//! so indexing ~1000 sessions is a burst of small reads — run it off the
//! event loop (see `Effect::BuildSessionIndex`), never on a key press.

use crate::agent::AgentKind;
use crate::codex_conversation;
use crate::codex_scanner;
use crate::config;
use crate::conversation;
use crate::pi_conversation;
use crate::platform::paths;
use crate::scanner;
use crate::title;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One archived transcript, with just enough to search it and resume it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedSession {
    pub agent_id: String,
    pub agent_kind: AgentKind,
    pub session_id: String,
    pub cwd: String,
    pub project_name: String,
    pub jsonl_path: PathBuf,
    /// Transcript mtime (epoch ms) — the archive's notion of recency.
    pub mtime_ms: u64,
    /// Saved cc-hub title (Haiku or manual rename), when one exists.
    pub title: Option<String>,
    /// First user message — the cheap "what was this session about" text.
    pub first_message: Option<String>,
}

/// Walk every enabled agent's transcript store and return the full archive,
/// newest first. Sessions whose head carries no cwd are skipped — without a
/// cwd there is nothing to resume them in.
pub fn scan() -> Vec<IndexedSession> {
    let titles = title::load();
    let enabled = config::get().enabled_agent_kinds();
    let mut out = Vec::new();
    if enabled.contains(&AgentKind::Claude) {
        out.extend(scan_claude(&titles));
    }
    if enabled.contains(&AgentKind::Pi) {
        out.extend(scan_pi(&titles));
    }
    if enabled.contains(&AgentKind::Codex) {
        out.extend(scan_codex(&titles));
    }
    out.sort_by(|a, b| {
        b.mtime_ms
            .cmp(&a.mtime_ms)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    out
}

fn mtime_ms(path: &Path) -> Option<u64> {
    let mtime = path.metadata().ok()?.modified().ok()?;
    mtime
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// The configured agent id for `kind`, alphabetically first when several are
/// configured — a deterministic pick mirroring the sorted pickers. `None`
/// means the kind has no agent to resume with, so its store is skipped.
fn agent_id_for(kind: AgentKind) -> Option<String> {
    config::get()
        .resolved_agents()
        .into_values()
        .filter(|a| a.kind == kind)
        .map(|a| a.id)
        .min()
}

fn scan_claude(titles: &HashMap<String, String>) -> Vec<IndexedSession> {
    let Some(projects) = paths::claude_home().map(|d| d.join("projects")) else {
        return Vec::new();
    };
    let Ok(project_dirs) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    let scratch = scanner::scratch_project_dir_name();
    let mut out = Vec::new();
    for proj in project_dirs.flatten() {
        // The titler's one-shot `cc-hub-new -p` runs are not real sessions.
        if scratch.as_deref() == proj.file_name().to_str() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        for entry in files.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let session_id = session_id.to_string();
            let head = conversation::read_jsonl_head(&path, 4096);
            let Some(cwd) = head
                .iter()
                .find_map(|e| e.get("cwd").and_then(|c| c.as_str()))
            else {
                continue;
            };
            let cwd = cwd.to_string();
            out.push(IndexedSession {
                agent_id: "claude".into(),
                agent_kind: AgentKind::Claude,
                title: titles.get(&session_id).cloned(),
                session_id,
                project_name: project_name(&cwd),
                cwd,
                mtime_ms: mtime_ms(&path).unwrap_or(0),
                first_message: conversation::extract_first_user_message(&head),
                jsonl_path: path,
            });
        }
    }
    out
}

fn scan_pi(titles: &HashMap<String, String>) -> Vec<IndexedSession> {
    let Some(agent_id) = agent_id_for(AgentKind::Pi) else {
        return Vec::new();
    };
    let Some(dir) = paths::pi_sessions_dir() else {
        return Vec::new();
    };
    let Ok(files) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in files.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let head = conversation::read_jsonl_head(&path, 4096);
        let Some(cwd) = head
            .iter()
            .find_map(|e| e.get("cwd").and_then(|c| c.as_str()))
        else {
            continue;
        };
        let cwd = cwd.to_string();
        let Some(session_id) = head
            .iter()
            .find_map(|e| e.get("id").and_then(|v| v.as_str()))
            .map(str::to_string)
            .or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            })
        else {
            continue;
        };
        out.push(IndexedSession {
            agent_id: agent_id.clone(),
            agent_kind: AgentKind::Pi,
            title: titles.get(&session_id).cloned(),
            session_id,
            project_name: project_name(&cwd),
            cwd,
            mtime_ms: mtime_ms(&path).unwrap_or(0),
            first_message: pi_conversation::extract_first_user_message(&head),
            jsonl_path: path,
        });
    }
    out
}

fn scan_codex(titles: &HashMap<String, String>) -> Vec<IndexedSession> {
    let Some(agent_id) = agent_id_for(AgentKind::Codex) else {
        return Vec::new();
    };
    let Some(root) = paths::codex_sessions_dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (path, _) in codex_scanner::walk_rollouts(&root) {
        let head = codex_scanner::read_head(&path);
        let Some(cwd) = codex_conversation::extract_cwd(&head) else {
            continue;
        };
        let Some(session_id) = codex_conversation::extract_session_id(&head)
            .or_else(|| codex_scanner::session_id_from_filename(&path))
        else {
            continue;
        };
        out.push(IndexedSession {
            agent_id: agent_id.clone(),
            agent_kind: AgentKind::Codex,
            title: titles.get(&session_id).cloned(),
            session_id,
            project_name: project_name(&cwd),
            cwd,
            mtime_ms: mtime_ms(&path).unwrap_or(0),
            first_message: codex_conversation::extract_first_user_message(&head),
            jsonl_path: path,
        });
    }
    out
}

// Unix-only: with_temp_home isolation redirects $HOME, which only works on
// unix (mirrors the scanner's own test guard).
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;
    use std::fs;

    fn with_temp_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        let prev_cfg = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::env::set_var("HOME", home.path());
        let out = f(home.path());
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_cfg {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        out
    }

    fn write_claude_jsonl(home: &Path, dir: &str, sid: &str, cwd: &str, msg: &str) {
        let proj = home.join(".claude/projects").join(dir);
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join(format!("{sid}.jsonl")),
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{cwd}\",\"timestamp\":1000,\
                 \"message\":{{\"role\":\"user\",\"content\":\"{msg}\"}}}}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn scan_indexes_claude_transcripts_with_titles_and_first_message() {
        with_temp_home(|home| {
            write_claude_jsonl(
                home,
                "-tmp-alpha",
                "sid-alpha",
                "/tmp/alpha",
                "fix the parser",
            );
            write_claude_jsonl(
                home,
                "-tmp-beta",
                "sid-beta",
                "/tmp/beta",
                "ship the release",
            );
            title::persist_title("sid-alpha", "Parser surgery").unwrap();

            let index = scan();
            let claude: Vec<&IndexedSession> = index
                .iter()
                .filter(|s| s.agent_kind == AgentKind::Claude)
                .collect();
            assert_eq!(claude.len(), 2);

            let alpha = claude
                .iter()
                .find(|s| s.session_id == "sid-alpha")
                .expect("alpha indexed");
            assert_eq!(alpha.title.as_deref(), Some("Parser surgery"));
            assert_eq!(alpha.cwd, "/tmp/alpha");
            assert_eq!(alpha.project_name, "alpha");
            assert_eq!(alpha.first_message.as_deref(), Some("fix the parser"));

            let beta = claude
                .iter()
                .find(|s| s.session_id == "sid-beta")
                .expect("beta indexed");
            assert_eq!(beta.title, None);
            assert_eq!(beta.first_message.as_deref(), Some("ship the release"));
        });
    }

    #[test]
    fn scan_skips_transcripts_without_a_cwd() {
        with_temp_home(|home| {
            // A head without any cwd entry can't be resumed anywhere.
            let proj = home.join(".claude/projects/-tmp-x");
            fs::create_dir_all(&proj).unwrap();
            fs::write(proj.join("no-cwd.jsonl"), "{\"type\":\"system\"}\n").unwrap();
            assert!(scan().iter().all(|s| s.session_id != "no-cwd"));
        });
    }

    #[test]
    fn scan_skips_the_titler_scratch_project() {
        with_temp_home(|home| {
            let scratch = scanner::scratch_project_dir_name().expect("scratch dir name");
            write_claude_jsonl(home, &scratch, "sid-scratch", "/tmp/x", "title me");
            assert!(scan().iter().all(|s| s.session_id != "sid-scratch"));
        });
    }
}
