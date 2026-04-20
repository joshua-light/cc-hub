//! Thin wrapper around the `gh` CLI for repo-related actions invoked from the
//! folder picker. Each function is synchronous and intended to be called from
//! `tokio::task::spawn_blocking`.

use std::process::Command;

/// Create an empty GitHub repo named `name` and clone it as a subfolder of
/// `cwd`. Returns the repo URL on success, or the first non-empty line of
/// stderr on failure.
pub fn create_repo(cwd: &str, name: &str, private: bool) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("repo name is empty".into());
    }

    let visibility = if private { "--private" } else { "--public" };
    let out = Command::new("gh")
        .args(["repo", "create", name, visibility, "--clone"])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("gh not available: {}", e))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(first_line(&stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(first_line(&stdout).to_string())
}

fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s).trim()
}
