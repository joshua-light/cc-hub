//! Agent-specific text formatting.

/// Inbox files are named `<utc stamp>-<label>`, poll events `poll-<hash>`,
/// timer events `tick-<n>`: keep the part a person reads.
pub(super) fn event_label(id: Option<&str>) -> String {
    let Some(id) = id else {
        return "manual".into();
    };
    if id.starts_with("poll-") {
        return "poll".into();
    }
    if id.starts_with("tick-") {
        return "timer".into();
    }
    let stamped =
        id.len() > 16 && id.as_bytes()[8] == b'T' && id[..8].bytes().all(|b| b.is_ascii_digit());
    match id.split_once("Z-") {
        Some((_, rest)) if stamped && !rest.is_empty() => rest.to_string(),
        _ => id.to_string(),
    }
}

pub(super) fn clock(at: i64, now: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(t) = Local.timestamp_opt(at, 0).single() else {
        return String::new();
    };
    let today = Local
        .timestamp_opt(now, 0)
        .single()
        .map(|n| n.date_naive() == t.date_naive())
        .unwrap_or(false);
    if today {
        t.format("%H:%M").to_string()
    } else {
        t.format("%b %d %H:%M").to_string()
    }
}

pub(super) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Greedy word wrap for plain text. Keeps explicit line breaks and hard-cuts
/// a word wider than the line; `ui::tasks`' wrap differs on both counts.
pub(super) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for para in text.lines() {
        let mut cur = String::new();
        for word in para.split_whitespace() {
            let wl = word.chars().count();
            let cl = cur.chars().count();
            if cl > 0 && cl + 1 + wl > width {
                out.push(std::mem::take(&mut cur));
            }
            if wl > width {
                // A long URL or path: hard-cut it.
                let chars: Vec<char> = word.chars().collect();
                for chunk in chars.chunks(width) {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur = chunk.iter().collect();
                }
                continue;
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        out.push(cur);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

pub(super) fn tilde(p: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(&h).ok().map(|r| r.to_path_buf())) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_labels_are_readable() {
        assert_eq!(event_label(Some("20260903T191749.728Z-poke")), "poke");
        assert_eq!(event_label(Some("poll-1f2e3d")), "poll");
        assert_eq!(event_label(Some("tick-4")), "timer");
        assert_eq!(event_label(Some("once")), "once");
        assert_eq!(event_label(None), "manual");
    }
}
