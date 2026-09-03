//! Event sources. The harness stays domain-agnostic by making the event
//! boundary a file or a command's stdout: whatever watches BitBucket, Slack
//! or a clock expresses an event by dropping a file into the agent's inbox
//! or printing to stdout. Nothing here knows what a payload means.

use super::spec::Spec;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Event {
    pub id: String,
    pub payload: String,
    /// `inbox`, `poll`, `interval`, `once`.
    pub source: &'static str,
    /// The inbox file while it sits in `processing/`; acked by moving it.
    pub processing: Option<PathBuf>,
}

impl Event {
    pub fn synthetic(
        id: impl Into<String>,
        payload: impl Into<String>,
        source: &'static str,
    ) -> Self {
        Self {
            id: id.into(),
            payload: payload.into(),
            source,
            processing: None,
        }
    }
}

// ---- inbox ----------------------------------------------------------------
//
// One file = one event. Durable and at-least-once: a new file is moved to
// processing/ while the tick runs, then to done/ or failed/ with a timestamp
// prefix. A crash mid-tick leaves it in processing/; `requeue_stale` puts it
// back on the next start, so an agent can see the same event twice and its
// instruction must make that harmless.

pub fn ensure_inbox(inbox: &Path) -> io::Result<()> {
    for sub in ["processing", "done", "failed"] {
        fs::create_dir_all(inbox.join(sub))?;
    }
    Ok(())
}

pub fn requeue_stale(inbox: &Path) -> io::Result<usize> {
    let mut n = 0;
    let Ok(rd) = fs::read_dir(inbox.join("processing")) else {
        return Ok(0);
    };
    for entry in rd.flatten() {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            fs::rename(entry.path(), inbox.join(entry.file_name()))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Pending inbox files, oldest first, so a burst is handled in arrival order.
fn pending(inbox: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(inbox) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((mtime, e.path()))
        })
        .collect();
    files.sort();
    files.into_iter().map(|(_, p)| p).collect()
}

pub fn pending_count(inbox: &Path) -> usize {
    pending(inbox).len()
}

/// Claim the oldest pending file: move it to `processing/` and read it.
pub fn take(inbox: &Path) -> io::Result<Option<Event>> {
    let Some(src) = pending(inbox).into_iter().next() else {
        return Ok(None);
    };
    let name = src.file_name().unwrap_or_default().to_os_string();
    let work = inbox.join("processing").join(&name);
    fs::rename(&src, &work)?;
    let payload = fs::read_to_string(&work).unwrap_or_else(|_| {
        String::from_utf8_lossy(&fs::read(&work).unwrap_or_default()).into_owned()
    });
    Ok(Some(Event {
        id: name.to_string_lossy().into_owned(),
        payload,
        source: "inbox",
        processing: Some(work),
    }))
}

/// File the processed event under `done/` or `failed/`. `failed/` is the
/// dead-letter queue — read it.
pub fn ack(event: &Event, ok: bool) {
    let Some(work) = &event.processing else {
        return;
    };
    let Some(inbox) = work.parent().and_then(|p| p.parent()) else {
        return;
    };
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let name = work
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let dest = inbox
        .join(if ok { "done" } else { "failed" })
        .join(format!("{}-{}", stamp, name));
    let _ = fs::rename(work, dest);
}

/// Drop a payload into the inbox. Returns the event id (the file name).
pub fn drop_event(inbox: &Path, label: &str, payload: &str) -> io::Result<String> {
    ensure_inbox(inbox)?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let safe: String = label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = format!("{}-{}", stamp, safe);
    let tmp = inbox.join(format!(".{}", name));
    fs::write(&tmp, payload)?;
    // Rename so a reader never sees a half-written file.
    fs::rename(&tmp, inbox.join(&name))?;
    Ok(name)
}

// ---- poll -----------------------------------------------------------------

/// Run the poll command; non-empty trimmed stdout is a candidate event.
/// `None` on timeout, spawn failure, or empty output.
pub fn run_poll(command: &str, cwd: &Path, timeout: Duration) -> Option<String> {
    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = crate::title::run_with_timeout(cmd, timeout)?;
    if !out.status.success() {
        log::warn!(
            "harness poll: {:?} exit={} stderr={}",
            command,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Short stable digest for poll dedupe and event ids.
pub fn digest(payload: &str) -> String {
    // FNV-1a: no crypto needed, just stability across runs.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in payload.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{:012x}", hash & 0xffff_ffff_ffff)
}

// ---- prompt ---------------------------------------------------------------

/// Put the event in front of the agent. A spec may place `{event}` /
/// `{event_id}` itself; otherwise the payload is appended as its own section.
pub fn render_prompt(spec: &Spec, event: Option<&Event>) -> String {
    let Some(ev) = event.filter(|e| !e.payload.is_empty()) else {
        return spec.instruction.clone();
    };
    if spec.instruction.contains("{event}") {
        return spec
            .instruction
            .replace("{event}", &ev.payload)
            .replace("{event_id}", &ev.id);
    }
    format!(
        "{}\n\n## Event ({})\n{}",
        spec.instruction, ev.id, ev.payload
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::spec;

    #[test]
    fn inbox_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        let id = drop_event(&inbox, "poke", "hello").unwrap();
        assert!(id.ends_with("-poke"));
        assert_eq!(pending_count(&inbox), 1);

        let ev = take(&inbox).unwrap().unwrap();
        assert_eq!(ev.id, id);
        assert_eq!(ev.payload, "hello");
        assert_eq!(pending_count(&inbox), 0);
        assert!(inbox.join("processing").join(&id).exists());

        ack(&ev, false);
        let failed: Vec<_> = fs::read_dir(inbox.join("failed"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].file_name().to_string_lossy().ends_with(&id));
    }

    #[test]
    fn stale_processing_is_requeued() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        ensure_inbox(&inbox).unwrap();
        fs::write(inbox.join("processing").join("ev1"), "x").unwrap();
        assert_eq!(requeue_stale(&inbox).unwrap(), 1);
        assert_eq!(pending_count(&inbox), 1);
    }

    #[test]
    fn prompt_placement() {
        let s = spec::parse(
            Path::new("/tmp/x"),
            "[prompt]\ninstruction=\"Handle {event} ({event_id})\"",
        )
        .unwrap();
        let ev = Event::synthetic("e1", "payload", "once");
        assert_eq!(render_prompt(&s, Some(&ev)), "Handle payload (e1)");

        let s = spec::parse(Path::new("/tmp/x"), "[prompt]\ninstruction=\"Do it.\"").unwrap();
        assert_eq!(
            render_prompt(&s, Some(&ev)),
            "Do it.\n\n## Event (e1)\npayload"
        );
        assert_eq!(render_prompt(&s, None), "Do it.");
    }

    #[test]
    fn digest_is_stable() {
        assert_eq!(digest("abc"), digest("abc"));
        assert_ne!(digest("abc"), digest("abd"));
        assert_eq!(digest("abc").len(), 12);
    }
}
