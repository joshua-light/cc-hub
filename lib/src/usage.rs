//! Quota of a Claude account: one reading, one poller, many readers.
//!
//! The usage endpoint rate-limits the whole machine, so nothing in the Hub
//! calls it directly. Every consumer (the statusline, the TUI, the resource
//! broker via `cc-hub usage`) goes through [`read`], which serves the stored
//! reading and asks the endpoint again only when the account is due. A failed
//! ask never erases the last good reading; it only pushes the next ask out.
//!
//! The store is `~/.cc-hub/usage.json`, one [`Usage`] per account home, and
//! every read-probe-write runs under an advisory flock so two statuslines
//! that wake together still make one request.

use crate::config;
use crate::platform::paths;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const CURL_TIMEOUT_SECS: &str = "5";
const BACKOFF_BASE: Duration = Duration::from_secs(60);
const BACKOFF_MAX: Duration = Duration::from_secs(600);
const LOGIN_RETRY: Duration = Duration::from_secs(300);

/// A Claude account, identified by its config directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    home: PathBuf,
}

impl Account {
    /// The account this process would use: `CLAUDE_CONFIG_DIR` or `~/.claude`.
    pub fn current() -> Option<Self> {
        paths::claude_home().map(Self::at)
    }

    pub fn at(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let home = fs::canonicalize(&home).unwrap_or(home);
        Self { home }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    fn key(&self) -> String {
        self.home.to_string_lossy().into_owned()
    }

    fn is_default(&self) -> bool {
        dirs::home_dir().map(|h| h.join(".claude")).as_deref() == Some(self.home.as_path())
    }

    /// Claude Code's Keychain entry: the plain name for `~/.claude`, a
    /// path-hash suffix for any other config dir.
    fn keychain_service(&self) -> String {
        if self.is_default() {
            return "Claude Code-credentials".into();
        }
        let digest = format!("{:x}", Sha256::digest(self.key().as_bytes()));
        format!("Claude Code-credentials-{}", &digest[..8])
    }

    fn token(&self) -> Option<String> {
        let from_file = fs::read_to_string(self.home.join(".credentials.json")).ok();
        let credentials = from_file.or_else(|| {
            if !cfg!(target_os = "macos") {
                return None;
            }
            let output = Command::new("security")
                .args([
                    "find-generic-password",
                    "-s",
                    &self.keychain_service(),
                    "-w",
                ])
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        })?;
        let value: serde_json::Value = serde_json::from_str(credentials.trim()).ok()?;
        value
            .get("claudeAiOauth")?
            .get("accessToken")?
            .as_str()
            .map(String::from)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ready,
    Throttled,
    LoginRequired,
    Unavailable,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    #[serde(default)]
    pub utilization: f64,
    #[serde(default)]
    pub resets_at: Option<String>,
}

/// One successful answer from the endpoint and when it was received.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub five_hour: Window,
    pub seven_day: Window,
    pub observed_at: u64,
}

/// What the store knows about one account: the last good reading, how the
/// last ask went, and when the next one is allowed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub health: Health,
    pub reading: Option<Reading>,
    pub error: Option<String>,
    pub attempted_at: u64,
    pub retry_at: u64,
    pub failures: u32,
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            health: Health::Unavailable,
            reading: None,
            error: None,
            attempted_at: 0,
            retry_at: 0,
            failures: 0,
        }
    }
}

impl Usage {
    fn due(&self, now: u64, ttl: Duration) -> bool {
        now >= self.retry_at && now.saturating_sub(self.attempted_at) >= ttl.as_secs()
    }

    fn absorb(&mut self, outcome: Outcome, now: u64) {
        self.attempted_at = now;
        match outcome {
            Outcome::Answered(reading) => {
                self.health = Health::Ready;
                self.reading = Some(reading);
                self.error = None;
                self.failures = 0;
                self.retry_at = 0;
            }
            Outcome::Throttled(hint) => {
                self.failures += 1;
                self.health = Health::Throttled;
                self.error = Some("usage HTTP 429".into());
                let wait = hint.unwrap_or(Duration::ZERO).max(backoff(self.failures));
                self.retry_at = now + wait.as_secs();
            }
            Outcome::LoginRequired => {
                self.health = Health::LoginRequired;
                self.error = Some("no Claude login for this account".into());
                self.retry_at = now + LOGIN_RETRY.as_secs();
            }
            Outcome::Unavailable(error) => {
                self.failures += 1;
                self.health = Health::Unavailable;
                self.error = Some(error);
                self.retry_at = now + backoff(self.failures).as_secs();
            }
        }
    }

    /// The reading shaped for the TUI, when there ever was one.
    pub fn info(&self) -> Option<UsageInfo> {
        let reading = self.reading.as_ref()?;
        Some(UsageInfo {
            five_hour_pct: clamp_pct(reading.five_hour.utilization),
            five_hour_resets_at: reading.five_hour.resets_at.clone(),
            seven_day_pct: clamp_pct(reading.seven_day.utilization),
            seven_day_resets_at: reading.seven_day.resets_at.clone(),
            health: self.health,
        })
    }

    /// The `cc-hub usage` contract: what a script gets on stdout.
    pub fn report(&self, account: &Account) -> serde_json::Value {
        let now = now();
        let age = self
            .reading
            .as_ref()
            .map(|r| now.saturating_sub(r.observed_at));
        serde_json::json!({
            "ok": true,
            "home": account.key(),
            "health": self.health,
            "error": self.error,
            "observed_at": self.reading.as_ref().map(|r| r.observed_at),
            "age_s": age,
            "retry_at": (self.retry_at > now).then_some(self.retry_at),
            "five_hour": self.reading.as_ref().map(|r| &r.five_hour),
            "seven_day": self.reading.as_ref().map(|r| &r.seven_day),
        })
    }
}

fn backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(4);
    (BACKOFF_BASE * 2u32.pow(doublings)).min(BACKOFF_MAX)
}

/// The store's view of one account, refreshed first when it is due.
pub fn read(account: &Account) -> Usage {
    sync(account, false)
}

/// Like [`read`], but the TTL is ignored. The retry window still holds: a
/// forced refresh must not be the thing that keeps the endpoint angry.
pub fn refresh(account: &Account) -> Usage {
    sync(account, true)
}

/// The current account's reading for the TUI.
pub fn fetch_usage() -> Option<UsageInfo> {
    read(&Account::current()?).info()
}

fn sync(account: &Account, force: bool) -> Usage {
    let ttl = if force {
        Duration::ZERO
    } else {
        config::get().scan.usage_cache_ttl()
    };
    let Some(store) = Store::open() else {
        return Usage::default();
    };
    let _guard = store.lock();
    let mut entries = store.load();
    let entry = entries.entry(account.key()).or_default();
    let now = now();
    if !entry.due(now, ttl) {
        return entry.clone();
    }
    entry.absorb(probe(account), now);
    let refreshed = entry.clone();
    store.save(&entries);
    refreshed
}

struct Store {
    file: PathBuf,
}

impl Store {
    fn open() -> Option<Self> {
        let dir = paths::cc_hub_home()?;
        fs::create_dir_all(&dir).ok()?;
        Some(Self {
            file: dir.join("usage.json"),
        })
    }

    fn lock(&self) -> Option<fs::File> {
        let guard = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file.with_extension("lock"))
            .ok()?;
        guard.lock_exclusive().ok()?;
        Some(guard)
    }

    fn load(&self) -> BTreeMap<String, Usage> {
        fs::read_to_string(&self.file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, entries: &BTreeMap<String, Usage>) {
        let tmp = self
            .file
            .with_extension(format!("tmp.{}", std::process::id()));
        let Ok(body) = serde_json::to_vec_pretty(entries) else {
            return;
        };
        if fs::write(&tmp, body).is_ok() && fs::rename(&tmp, &self.file).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
}

enum Outcome {
    Answered(Reading),
    Throttled(Option<Duration>),
    LoginRequired,
    Unavailable(String),
}

fn probe(account: &Account) -> Outcome {
    let Some(token) = account.token() else {
        return Outcome::LoginRequired;
    };
    let output = Command::new("curl")
        .args([
            "-sS",
            "-i",
            "--max-time",
            CURL_TIMEOUT_SECS,
            ENDPOINT,
            "-H",
            &format!("Authorization: Bearer {token}"),
            "-H",
            "anthropic-beta: oauth-2025-04-20",
            "-H",
            "User-Agent: cc-hub/1.0",
        ])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            parse_response(&String::from_utf8_lossy(&output.stdout), now())
        }
        Ok(_) => Outcome::Unavailable("usage endpoint unreachable".into()),
        Err(_) => Outcome::Unavailable("curl is not installed".into()),
    }
}

/// Reads a `curl -i` transcript: the last header block decides the status,
/// whatever follows it is the body.
fn parse_response(transcript: &str, now: u64) -> Outcome {
    let transcript = transcript.replace("\r\n", "\n");
    let mut blocks = transcript.split("\n\n").filter(|b| !b.trim().is_empty());
    let mut head = match blocks.next() {
        Some(block) => block,
        None => return Outcome::Unavailable("empty response".into()),
    };
    let mut body = "";
    for block in blocks {
        if block.starts_with("HTTP/") {
            head = block;
        } else {
            body = block;
            break;
        }
    }
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let header = |name: &str| {
        head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    };
    match status {
        200 => match serde_json::from_str::<Response>(body) {
            Ok(response) => Outcome::Answered(Reading {
                five_hour: response.five_hour,
                seven_day: response.seven_day,
                observed_at: now,
            }),
            Err(_) => Outcome::Unavailable("usage response was not JSON".into()),
        },
        429 => Outcome::Throttled(
            header("retry-after")
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs),
        ),
        401 | 403 => Outcome::LoginRequired,
        code => Outcome::Unavailable(format!("usage HTTP {code}")),
    }
}

#[derive(Deserialize)]
struct Response {
    five_hour: Window,
    seven_day: Window,
}

/// The reading as the TUI renders it.
#[derive(Debug, Clone)]
pub struct UsageInfo {
    pub five_hour_pct: u8,
    pub five_hour_resets_at: Option<String>,
    pub seven_day_pct: u8,
    pub seven_day_resets_at: Option<String>,
    pub health: Health,
}

fn clamp_pct(v: f64) -> u8 {
    v.clamp(0.0, 100.0) as u8
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = "HTTP/2 200 \r\ncontent-type: application/json\r\n\r\n{\"five_hour\":{\"utilization\":61.0,\"resets_at\":\"2026-09-07T12:19:59+00:00\"},\"seven_day\":{\"utilization\":7.0}}";
    const THROTTLED: &str =
        "HTTP/2 429 \r\nretry-after: 120\r\n\r\n{\"error\":{\"type\":\"rate_limit_error\"}}";

    #[test]
    fn a_good_answer_becomes_a_reading() {
        let mut usage = Usage::default();
        usage.absorb(parse_response(OK, 1_000), 1_000);
        assert_eq!(usage.health, Health::Ready);
        let reading = usage.reading.unwrap();
        assert_eq!(reading.five_hour.utilization, 61.0);
        assert_eq!(reading.seven_day.utilization, 7.0);
        assert_eq!(reading.observed_at, 1_000);
    }

    #[test]
    fn throttling_keeps_the_reading_and_honours_retry_after() {
        let mut usage = Usage::default();
        usage.absorb(parse_response(OK, 1_000), 1_000);
        usage.absorb(parse_response(THROTTLED, 1_060), 1_060);
        assert_eq!(usage.health, Health::Throttled);
        assert_eq!(usage.reading.as_ref().unwrap().five_hour.utilization, 61.0);
        assert_eq!(usage.retry_at, 1_060 + 120);
    }

    #[test]
    fn repeated_failures_back_off_up_to_ten_minutes() {
        let mut usage = Usage::default();
        let mut now = 0;
        let waits: Vec<u64> = (0..6)
            .map(|_| {
                usage.absorb(Outcome::Throttled(Some(Duration::ZERO)), now);
                let wait = usage.retry_at - now;
                now = usage.retry_at;
                wait
            })
            .collect();
        assert_eq!(waits, vec![60, 120, 240, 480, 600, 600]);
    }

    #[test]
    fn success_resets_the_backoff() {
        let mut usage = Usage::default();
        usage.absorb(Outcome::Throttled(None), 0);
        usage.absorb(Outcome::Throttled(None), 60);
        usage.absorb(parse_response(OK, 200), 200);
        assert_eq!(usage.failures, 0);
        assert_eq!(usage.retry_at, 0);
    }

    #[test]
    fn due_waits_for_both_ttl_and_retry_window() {
        let ttl = Duration::from_secs(60);
        let mut usage = Usage::default();
        assert!(
            usage.due(1_000, ttl),
            "a never-asked account is due at once"
        );
        usage.absorb(parse_response(OK, 1_100), 1_100);
        assert!(!usage.due(1_130, ttl));
        assert!(usage.due(1_160, ttl));
        usage.absorb(Outcome::Throttled(Some(Duration::from_secs(300))), 1_160);
        assert!(!usage.due(1_300, ttl));
        assert!(usage.due(1_460, ttl));
        assert!(
            !usage.due(1_300, Duration::ZERO),
            "the retry window survives a forced refresh"
        );
    }

    #[test]
    fn a_redirect_transcript_uses_the_last_header_block() {
        let transcript = format!("HTTP/2 100 \r\n\r\n{}", OK);
        assert!(matches!(
            parse_response(&transcript, 1),
            Outcome::Answered(_)
        ));
        assert!(matches!(parse_response("", 1), Outcome::Unavailable(_)));
        assert!(matches!(
            parse_response("HTTP/2 401 \r\n\r\n", 1),
            Outcome::LoginRequired
        ));
    }

    #[test]
    fn keychain_service_matches_claude_code_naming() {
        let personal = Account {
            home: PathBuf::from("/Users/someone/.claude-personal"),
        };
        let service = personal.keychain_service();
        assert!(service.starts_with("Claude Code-credentials-"));
        assert_eq!(service.len(), "Claude Code-credentials-".len() + 8);
        let default = Account {
            home: dirs::home_dir().unwrap().join(".claude"),
        };
        assert_eq!(default.keychain_service(), "Claude Code-credentials");
    }

    #[test]
    fn report_exposes_reading_and_health_separately() {
        let account = Account {
            home: PathBuf::from("/x"),
        };
        let mut usage = Usage::default();
        usage.absorb(parse_response(OK, 1), 1);
        usage.absorb(Outcome::Throttled(None), 2);
        let report = usage.report(&account);
        assert_eq!(report["health"], "throttled");
        assert_eq!(report["five_hour"]["utilization"], 61.0);
        assert_eq!(report["ok"], true);
    }
}
