use super::store::TaskPriority;

/// Longest a single tag may be after normalization; longer ones are truncated.
const MAX_TAG_LEN: usize = 16;
/// Most tags a task may carry; extras are dropped so the border badge fits.
const MAX_TAGS: usize = 6;

/// Parse a free-text tag line (from the inline editor) into the normalized set
/// stored on a task. Splits on whitespace *and* commas, strips a leading `#`,
/// lowercases, trims, drops empties, and dedupes (keeping first-seen order).
/// Each tag is capped at [`MAX_TAG_LEN`] chars and the set at [`MAX_TAGS`], so
/// the card's border badge always has a sane bound. Editing replaces the whole
/// set, so this is the single chokepoint for what a task's tags can be.
pub fn parse_tags(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in input.split([',', ' ', '\t', '\n']) {
        let tag = raw.trim().trim_start_matches('#').trim().to_lowercase();
        if tag.is_empty() {
            continue;
        }
        let tag: String = tag.chars().take(MAX_TAG_LEN).collect();
        if out.iter().any(|t| t == &tag) {
            continue;
        }
        out.push(tag);
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

/// Parsed form of the add popup's quick syntax: `#word` tokens become tags
/// and a `!1`–`!4` token sets the priority; everything else stays as the
/// task text. Rename leaves text verbatim — the syntax is capture-time
/// sugar only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickAdd {
    pub text: String,
    pub tags: Vec<String>,
    pub priority: Option<TaskPriority>,
}

/// Split the add popup's buffer into text, tags, and priority. Tag tokens
/// run through [`parse_tags`], so the tag editor's normalization and caps
/// apply here too. Several `!N` tokens keep the last one; a bare `#` or an
/// out-of-range `!5` is ordinary text.
pub fn parse_quick_add(input: &str) -> QuickAdd {
    let mut words: Vec<&str> = Vec::new();
    let mut tag_words: Vec<&str> = Vec::new();
    let mut priority = None;
    for tok in input.split_whitespace() {
        if let Some(rest) = tok.strip_prefix('#') {
            if !rest.is_empty() {
                tag_words.push(rest);
                continue;
            }
        }
        match tok {
            "!1" => priority = Some(TaskPriority::P1),
            "!2" => priority = Some(TaskPriority::P2),
            "!3" => priority = Some(TaskPriority::P3),
            "!4" => priority = Some(TaskPriority::P4),
            _ => words.push(tok),
        }
    }
    QuickAdd {
        text: words.join(" "),
        tags: parse_tags(&tag_words.join(" ")),
        priority,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn parse_tags_normalizes_and_caps() {
        // Splits on commas and whitespace, strips '#', lowercases, dedupes.
        assert_eq!(
            parse_tags("#Bug,  api   bug BACKEND"),
            vec!["bug", "api", "backend"]
        );
        assert!(parse_tags("   ").is_empty());
        // Per-tag length cap.
        let long = "a".repeat(40);
        assert_eq!(parse_tags(&long), vec!["a".repeat(MAX_TAG_LEN)]);
        // Set-size cap (seven distinct tags → MAX_TAGS kept).
        let many = "t1 t2 t3 t4 t5 t6 t7";
        assert_eq!(parse_tags(many).len(), MAX_TAGS);
    }

    #[test]
    fn parse_quick_add_splits_text_tags_priority() {
        let q = parse_quick_add("fix the parser #bug #api !2");
        assert_eq!(q.text, "fix the parser");
        assert_eq!(q.tags, vec!["bug", "api"]);
        assert_eq!(q.priority, Some(TaskPriority::P2));
        // Syntax tokens can sit anywhere; the last `!N` wins; a bare `#`
        // and an out-of-range `!5` stay text.
        let q = parse_quick_add("!4 ship #Infra it !1 # !5");
        assert_eq!(q.text, "ship it # !5");
        assert_eq!(q.tags, vec!["infra"]);
        assert_eq!(q.priority, Some(TaskPriority::P1));
        // Plain input passes through untouched.
        let q = parse_quick_add("just words");
        assert_eq!(q.text, "just words");
        assert!(q.tags.is_empty());
        assert_eq!(q.priority, None);
    }
}
