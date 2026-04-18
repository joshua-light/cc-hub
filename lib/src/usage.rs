use crate::platform::paths;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

const CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Deserialize)]
struct UsageWindow {
    #[serde(default)]
    utilization: f64,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct UsageResponse {
    five_hour: UsageWindow,
    seven_day: UsageWindow,
}

#[derive(Debug, Clone)]
pub struct UsageInfo {
    pub five_hour_pct: u8,
    pub five_hour_resets_at: Option<String>,
    pub seven_day_pct: u8,
    pub seven_day_resets_at: Option<String>,
}

pub fn fetch_usage() -> Option<UsageInfo> {
    if cache_is_fresh() {
        return read_cache();
    }
    fetch_via_curl().or_else(read_cache)
}

fn cache_is_fresh() -> bool {
    let Ok(meta) = fs::metadata(paths::usage_cache_file()) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age < CACHE_TTL)
        .unwrap_or(false)
}

fn read_cache() -> Option<UsageInfo> {
    let data = fs::read_to_string(paths::usage_cache_file()).ok()?;
    let resp: UsageResponse = serde_json::from_str(&data).ok()?;
    Some(usage_info_from(resp))
}

fn usage_info_from(resp: UsageResponse) -> UsageInfo {
    UsageInfo {
        five_hour_pct: clamp_pct(resp.five_hour.utilization),
        five_hour_resets_at: resp.five_hour.resets_at,
        seven_day_pct: clamp_pct(resp.seven_day.utilization),
        seven_day_resets_at: resp.seven_day.resets_at,
    }
}

fn clamp_pct(v: f64) -> u8 {
    v.clamp(0.0, 100.0) as u8
}

fn fetch_via_curl() -> Option<UsageInfo> {
    let token = read_oauth_token()?;
    let output = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "5",
            "https://api.anthropic.com/api/oauth/usage",
            "-H",
            &format!("Authorization: Bearer {}", token),
            "-H",
            "anthropic-beta: oauth-2025-04-20",
            "-H",
            "Content-Type: application/json",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let resp: UsageResponse = serde_json::from_slice(&output.stdout).ok()?;

    // Cache file lets sibling tools (statusline, etc.) skip their own fetch.
    let cache = paths::usage_cache_file();
    if let Some(parent) = cache.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp: PathBuf = cache.with_extension(format!("tmp.{}", std::process::id()));
    if fs::write(&tmp, &output.stdout).is_ok() {
        let _ = fs::rename(&tmp, &cache);
    }

    Some(usage_info_from(resp))
}

fn read_oauth_token() -> Option<String> {
    let home = dirs::home_dir()?;
    let data = fs::read_to_string(home.join(".claude").join(".credentials.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v.get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .map(String::from)
}
