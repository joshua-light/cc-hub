//! Pre-accept Claude's folder-trust dialog in `.claude.json`, so a detached
//! session never blocks on it.

use crate::platform::paths;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) fn ensure_path_trusted(cwd: &str) -> io::Result<()> {
    // Must target the SAME `.claude.json` the spawned `claude` will read — i.e.
    // the one under CLAUDE_CONFIG_DIR when set, not always `~/.claude.json`.
    // Marking the wrong file leaves the real config untrusted, so claude blocks
    // on its trust dialog at startup and never writes a session status file —
    // making the session invisible to the hub watching that config dir.
    let Some(config_path) = paths::claude_config_json() else {
        return Ok(());
    };
    ensure_path_trusted_at(cwd, config_path)
}

pub(super) fn ensure_path_trusted_at(cwd: &str, config_path: PathBuf) -> io::Result<()> {
    // Nothing to trust-mark until claude has written its config at least once
    // (the original read short-circuits on NotFound). Bailing here also avoids
    // creating a sidecar lock in a config dir that may not exist yet.
    if !config_path.exists() {
        return Ok(());
    }
    // Serialize cc-hub's own read-modify-write of this account-wide file so a
    // concurrent spawn (e.g. a deep link racing a keypress spawn) can't
    // clobber another project's freshly written trust entry via last-writer-
    // wins. The lock lives in a sidecar file because the store is tempfile+
    // rename — flock follows the inode, so the target itself can't be locked
    // across a replace. Residual limitation: the running `claude` process does
    // NOT take this lock, so it only prevents cc-hub-vs-cc-hub lost updates and
    // narrows — does not close — the window against an external writer.
    use fs2::FileExt;
    let mut lock_path = config_path.clone().into_os_string();
    lock_path.push(".cc-hub.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(lock_path))?;
    lock.lock_exclusive()?;
    // Read AFTER taking the lock so the mutation applies to fresh state, not a
    // snapshot another cc-hub writer has since superseded. `lock` stays live
    // to the end of the function; Drop releases the flock after the rename.
    let canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| Path::new(cwd).to_path_buf());
    let data = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut root: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| io::Error::other(format!("parse {}: {}", config_path.display(), e)))?;
    let root_obj = root.as_object_mut().ok_or_else(|| {
        io::Error::other(format!("{} root is not an object", config_path.display()))
    })?;
    let projects = root_obj
        .entry("projects".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            io::Error::other(format!(
                "{} projects is not an object",
                config_path.display()
            ))
        })?;
    let path_key = canon.to_string_lossy().into_owned();
    let project = projects
        .entry(path_key)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            io::Error::other(format!(
                "{} project entry is not an object",
                config_path.display()
            ))
        })?;
    if project
        .get("hasTrustDialogAccepted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Ok(());
    }
    project.insert(
        "hasTrustDialogAccepted".to_string(),
        serde_json::Value::Bool(true),
    );
    let body = serde_json::to_string_pretty(&root)
        .map_err(|e| io::Error::other(format!("serialize {}: {}", config_path.display(), e)))?;
    let tmp = config_path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &config_path)?;
    log::info!(
        "marked {} trusted in {} before spawning claude",
        canon.display(),
        config_path.display()
    );
    Ok(())
}
