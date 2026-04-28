//! Detect a project's currently-shipped version from its on-disk manifest.
//!
//! Runs on the `cc-hub task report` hot path the moment a task transitions
//! out of Running, so it must be cheap, sync, and infallible from the
//! caller's perspective: any I/O or parse failure is logged and treated as
//! "no version found" rather than propagated.

use std::fs;
use std::io;
use std::path::Path;

/// Detect the currently-shipped version of a project rooted at `root`, by
/// inspecting common manifest files. Returns the first hit in this priority
/// order:
///
/// 1. `Cargo.toml` (with workspace inheritance support)
/// 2. `package.json`
/// 3. `pyproject.toml` (`[project]`, then `[tool.poetry]`)
/// 4. `VERSION` (plain text, first non-empty line)
///
/// Returns `None` if no recognised manifest yields a version. Errors
/// (unreadable files, parse failures) are logged at debug level and treated
/// as "no version at this candidate" — never propagated, since this runs on
/// the task-report hot path and a failure here must not break task reporting.
///
/// A `Cargo.toml` whose `package.version` cannot be resolved (e.g. workspace
/// inheritance with no parent workspace) returns `None` — we do *not* fall
/// through to `package.json`, since a Rust project shouldn't suddenly look
/// like a Node project just because the workspace root is missing.
pub fn detect(root: &Path) -> Option<String> {
    if let Some(doc) = read_toml(&root.join("Cargo.toml")) {
        return read_cargo_version(&root.join("Cargo.toml"), &doc);
    }
    if let Some(doc) = read_json(&root.join("package.json")) {
        return clean(doc.get("version")?.as_str()?);
    }
    if let Some(doc) = read_toml(&root.join("pyproject.toml")) {
        if let Some(v) = read_pyproject_version(&doc) {
            return Some(v);
        }
    }
    if let Some(raw) = try_read(&root.join("VERSION")) {
        return raw.lines().map(str::trim).find(|l| !l.is_empty()).and_then(clean);
    }
    None
}

fn clean(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

/// Read a file as a String, returning `None` for both "not present" and any
/// other I/O error. NotFound is silent (the caller's whole point is "is this
/// kind of manifest here?"); other errors log at debug.
fn try_read(path: &Path) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            log::debug!("version::detect: read {}: {}", path.display(), e);
            None
        }
    }
}

fn read_toml(path: &Path) -> Option<toml::Value> {
    let raw = try_read(path)?;
    toml::from_str(&raw)
        .map_err(|e| log::debug!("version::detect: parse {}: {}", path.display(), e))
        .ok()
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let raw = try_read(path)?;
    serde_json::from_str(&raw)
        .map_err(|e| log::debug!("version::detect: parse {}: {}", path.display(), e))
        .ok()
}

fn read_cargo_version(path: &Path, doc: &toml::Value) -> Option<String> {
    if let Some(ver) = doc.get("package").and_then(|p| p.get("version")) {
        if let Some(s) = ver.as_str() {
            return clean(s);
        }
        // Workspace inheritance: `version = { workspace = true }` (or `version.workspace = true`).
        // Walk up parent dirs looking for a Cargo.toml with [workspace.package.version].
        if ver
            .as_table()
            .and_then(|t| t.get("workspace"))
            .and_then(|w| w.as_bool())
            .unwrap_or(false)
        {
            return resolve_workspace_version(path);
        }
        return None;
    }
    // Pure workspace manifest (no [package] block at this root, e.g. a
    // virtual workspace whose members live in subdirs): take the version
    // straight from [workspace.package]. Without this, projects whose root
    // is the workspace root (cc-hub itself, plenty of others) would report
    // no version.
    doc.get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .and_then(clean)
}

fn resolve_workspace_version(member_manifest: &Path) -> Option<String> {
    // Start from the *parent* of the member manifest's dir — workspace roots
    // sit above member crates.
    let mut dir = member_manifest.parent()?.parent();
    while let Some(d) = dir {
        if let Some(doc) = read_toml(&d.join("Cargo.toml")) {
            if let Some(v) = doc
                .get("workspace")
                .and_then(|w| w.get("package"))
                .and_then(|p| p.get("version"))
                .and_then(|v| v.as_str())
                .and_then(clean)
            {
                return Some(v);
            }
        }
        dir = d.parent();
    }
    None
}

fn read_pyproject_version(doc: &toml::Value) -> Option<String> {
    if let Some(c) = doc
        .get("project")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .and_then(clean)
    {
        return Some(c);
    }
    doc.get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .and_then(clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn cargo_explicit_version() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
        assert_eq!(detect(d.path()), Some("1.2.3".into()));
    }

    #[test]
    fn cargo_workspace_inheritance() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"lib\"]\n\n[workspace.package]\nversion = \"0.42.0\"\n",
        )
        .unwrap();
        let member = root.path().join("lib");
        fs::create_dir_all(&member).unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = { workspace = true }\n",
        )
        .unwrap();
        assert_eq!(detect(&member), Some("0.42.0".into()));
    }

    #[test]
    fn cargo_virtual_workspace_root() {
        // detect() called on a workspace root that has no [package] of its own
        // should return [workspace.package].version (this is cc-hub's own shape).
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"lib\"]\n\n[workspace.package]\nversion = \"0.25.0\"\n",
        )
        .unwrap();
        assert_eq!(detect(d.path()), Some("0.25.0".into()));
    }

    #[test]
    fn cargo_workspace_inheritance_dotted() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"lib\"]\n\n[workspace.package]\nversion = \"7.0.0\"\n",
        )
        .unwrap();
        let member = root.path().join("lib");
        fs::create_dir_all(&member).unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion.workspace = true\n",
        )
        .unwrap();
        assert_eq!(detect(&member), Some("7.0.0".into()));
    }

    #[test]
    fn package_json() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("package.json"),
            "{\"name\":\"x\",\"version\":\"2.0.1\"}",
        )
        .unwrap();
        assert_eq!(detect(d.path()), Some("2.0.1".into()));
    }

    #[test]
    fn pyproject_pep621() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("pyproject.toml"),
            "[project]\nname = \"x\"\nversion = \"3.4.5\"\n",
        )
        .unwrap();
        assert_eq!(detect(d.path()), Some("3.4.5".into()));
    }

    #[test]
    fn pyproject_poetry() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"x\"\nversion = \"9.9.9\"\n",
        )
        .unwrap();
        assert_eq!(detect(d.path()), Some("9.9.9".into()));
    }

    #[test]
    fn version_file_first_nonempty_line() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("VERSION"), "\n   \n  4.5.6  \nignored\n").unwrap();
        assert_eq!(detect(d.path()), Some("4.5.6".into()));
    }

    #[test]
    fn empty_dir_returns_none() {
        let d = tempdir().unwrap();
        assert_eq!(detect(d.path()), None);
    }

    #[test]
    fn cargo_takes_priority_over_package_json() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        fs::write(
            d.path().join("package.json"),
            "{\"name\":\"x\",\"version\":\"2.0.0\"}",
        )
        .unwrap();
        assert_eq!(detect(d.path()), Some("1.0.0".into()));
    }

    #[test]
    fn cargo_workspace_unresolved_does_not_fall_through() {
        // Member with workspace inheritance but no parent workspace manifest:
        // we must NOT fall through to package.json.
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = { workspace = true }\n",
        )
        .unwrap();
        fs::write(
            d.path().join("package.json"),
            "{\"name\":\"x\",\"version\":\"2.0.0\"}",
        )
        .unwrap();
        assert_eq!(detect(d.path()), None);
    }

    #[test]
    fn empty_string_version_is_rejected() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"\"\n",
        )
        .unwrap();
        assert_eq!(detect(d.path()), None);
    }
}
