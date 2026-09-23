//! A wake is a named "something happened", at `~/.cc-hub/wake/<name>`.
//! Whoever knows a thing changed touches it; an agent whose spec lists
//! `trigger.wake = ["board"]` runs its poll on the next second instead of
//! at the end of its interval. The board touches [`BOARD`] on every card
//! status change, which is why a card moved to In Progress reaches the task
//! router in about a second rather than up to a poll interval later.
//!
//! Deliberately not a queue: a wake carries no payload and remembers no
//! backlog, only when it last happened. The poll command stays the single
//! place that decides what an event *is* — a publisher can say "look now",
//! never "here is what you found". So a lost wake costs latency and nothing
//! else, and the same poll keeps working for an agent nobody wakes.

use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// The board's wake: every card status change touches it.
pub const BOARD: &str = "board";

/// When a wake last happened. Opaque on purpose — a subscriber compares it
/// with the one it saw before and learns only "same" or "newer", so no
/// reader can grow a second meaning for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp(u128);

pub struct Wake {
    name: String,
    path: PathBuf,
}

impl Wake {
    /// `None` for a name that cannot be a file (the same rule as an agent
    /// name) or when there is no home directory.
    pub fn named(name: &str) -> Option<Self> {
        if !crate::harness::valid_name(name) {
            return None;
        }
        Some(Self {
            name: name.to_string(),
            path: crate::platform::paths::cc_hub_home()?
                .join("wake")
                .join(name),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// It happened. Every subscriber's poll runs within a second.
    pub fn now(&self) -> io::Result<Stamp> {
        let stamp = Stamp(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Written, not touched, so `cat` answers when it last happened —
        // and so the value survives a filesystem with coarse mtimes. Staged
        // and renamed so a reader never parses half a stamp, but not
        // fsynced: a wake lost to a power cut costs one poll interval.
        let tmp = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, stamp.0.to_string())?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(stamp)
    }

    /// When it last happened, or `None` if it never has.
    pub fn last(&self) -> Option<Stamp> {
        std::fs::read_to_string(&self.path)
            .ok()?
            .trim()
            .parse()
            .ok()
            .map(Stamp)
    }
}

// Unix-only: isolation works by redirecting `$HOME`, which
// `dirs::home_dir()` ignores on Windows — same policy as bookmarks.rs.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    #[test]
    fn a_wake_that_never_happened_has_no_stamp() {
        with_temp_home(|| {
            assert_eq!(Wake::named("board").unwrap().last(), None);
        });
    }

    #[test]
    fn each_wake_reads_back_newer_than_the_last() {
        with_temp_home(|| {
            let w = Wake::named("board").unwrap();
            let first = w.now().unwrap();
            assert_eq!(w.last(), Some(first));
            let second = w.now().unwrap();
            assert_ne!(second, first);
            assert_eq!(w.last(), Some(second));
        });
    }

    #[test]
    fn a_name_that_cannot_be_a_file_is_refused() {
        with_temp_home(|| {
            assert!(Wake::named("../escape").is_none());
            assert!(Wake::named("").is_none());
            assert!(Wake::named("board").is_some());
        });
    }
}
