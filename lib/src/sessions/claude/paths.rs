use crate::agent::AgentKind;
use crate::platform::paths;
use std::path::PathBuf;

pub(super) fn claude_dir() -> Option<PathBuf> {
    crate::resources::claude_scan_home().or_else(paths::claude_home)
}

pub(super) fn sessions_dir() -> Option<PathBuf> {
    claude_dir().map(|d| d.join("sessions"))
}

pub(super) fn projects_dir() -> Option<PathBuf> {
    claude_dir().map(|d| d.join("projects"))
}

/// Encode a cwd into the directory name Claude Code uses under
/// `~/.claude/projects/`. Claude replaces the path separators and other
/// non-name characters with `-`: on POSIX that's `/` and `.`, but on Windows
/// the cwd also carries `\` and a drive `:` (e.g. `C:\Users\me` →
/// `C--Users-me`). Without the latter two, transcript lookup never matches on
/// Windows and every session resolves to `jsonl=none`.
pub(crate) fn encode_path(path: &str) -> String {
    path.replace(['/', '.', '\\', ':'], "-")
}

pub fn find_jsonl(cwd: &str, session_id: &str) -> Option<PathBuf> {
    let projects = projects_dir()?;
    let encoded = encode_path(cwd);
    let jsonl_path = projects
        .join(&encoded)
        .join(format!("{}.jsonl", session_id));
    if jsonl_path.exists() {
        Some(jsonl_path)
    } else {
        None
    }
}

/// Find a `<session_id>.jsonl` anywhere under `~/.claude/projects/`. Used as
/// a fallback when the project-root path stored on a task no longer matches
/// the cwd Claude encoded at spawn time (symlinks, trailing slash, the
/// directory was renamed, etc.) so the direct `find_jsonl` lookup misses.
pub fn find_jsonl_anywhere(session_id: &str) -> Option<PathBuf> {
    let target = format!("{}.jsonl", session_id);
    let mut roots: Vec<_> = projects_dir().into_iter().collect();
    roots.extend(
        crate::resources::accounts()
            .values()
            .filter(|a| a.provider == AgentKind::Claude)
            .filter_map(|a| a.home().map(|h| h.join("projects"))),
    );
    for projects in roots {
        if let Ok(entries) = std::fs::read_dir(&projects) {
            for entry in entries.flatten() {
                let candidate = entry.path().join(&target);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

pub(super) fn is_scratch_cwd(cwd: &str) -> bool {
    crate::title::scratch_cwd()
        .to_str()
        .is_some_and(|s| s == cwd)
}

/// The `~/.claude/projects/` subdirectory name that the titler's scratch cwd
/// encodes to (e.g. `-tmp-cc-hub-summaries`), or `None` when that path isn't
/// valid UTF-8. Every JSONL under this dir is a one-shot `cc-hub-new -p`
/// run — not a real session — so callers walking the projects tree skip
/// it. Shared with [`crate::sessions::count`] so the exclusion can't drift.
pub fn scratch_project_dir_name() -> Option<String> {
    crate::title::scratch_cwd().to_str().map(encode_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The encoded cwd must match the directory name Claude Code creates under
    // ~/.claude/projects. POSIX separators and Windows `\`/`:` all map to `-`.
    #[test]
    fn encode_path_matches_claude_projects_dir_naming() {
        // POSIX
        assert_eq!(encode_path("/home/me/proj"), "-home-me-proj");
        assert_eq!(encode_path("/home/me/.config"), "-home-me--config");
        // Windows: drive colon + backslashes, e.g. observed real dirs.
        assert_eq!(
            encode_path("C:\\Users\\ExampleUser"),
            "C--Users-ExampleUser"
        );
        assert_eq!(
            encode_path("C:\\Users\\ExampleUser\\.local\\bin"),
            "C--Users-ExampleUser--local-bin"
        );
        assert_eq!(encode_path("C:\\Projects\\Sample"), "C--Projects-Sample");
    }
}
