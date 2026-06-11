//! Tiny fuzzy matcher for the places picker: case-insensitive subsequence
//! match with a score that prefers consecutive runs, word-boundary starts
//! (after `-`, `_`, `/`, `.`, space), and matches near the front. Greedy
//! left-to-right, so it can miss a better alignment further right — fine
//! for ranking a few dozen directory names, not a general search engine.

/// A successful match: ranking score (higher is better; can go negative
/// for late matches in long strings) plus the matched char indices into
/// the haystack, for highlight rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i32,
    pub indices: Vec<usize>,
}

const CONSECUTIVE_BONUS: i32 = 5;
const BOUNDARY_BONUS: i32 = 10;

fn lower1(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn is_boundary(prev: char) -> bool {
    matches!(prev, '-' | '_' | '/' | '.' | ' ')
}

/// Match `query` as a subsequence of `haystack`, case-insensitively.
/// Returns `None` when some query char never appears. An empty query
/// matches everything with score 0 and no indices.
pub fn fuzzy_match(query: &str, haystack: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: Vec::new(),
        });
    }
    let q: Vec<char> = query.chars().map(lower1).collect();
    let h: Vec<char> = haystack.chars().map(lower1).collect();

    let mut indices = Vec::with_capacity(q.len());
    let mut score = 0i32;
    let mut qi = 0usize;
    let mut last: Option<usize> = None;
    for (i, &hc) in h.iter().enumerate() {
        if qi == q.len() {
            break;
        }
        if hc != q[qi] {
            continue;
        }
        score += 1;
        if last == Some(i.wrapping_sub(1)) {
            score += CONSECUTIVE_BONUS;
        }
        if i == 0 || is_boundary(h[i - 1]) {
            score += BOUNDARY_BONUS;
        }
        indices.push(i);
        last = Some(i);
        qi += 1;
    }
    if qi < q.len() {
        return None;
    }
    // Matches that start late in the string rank below front matches.
    score -= indices.first().copied().unwrap_or(0) as i32;
    Some(FuzzyMatch { score, indices })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(q: &str, h: &str) -> i32 {
        fuzzy_match(q, h).expect("should match").score
    }

    #[test]
    fn empty_query_matches_anything() {
        let m = fuzzy_match("", "whatever").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.indices.is_empty());
    }

    #[test]
    fn substring_matches_with_contiguous_indices() {
        let m = fuzzy_match("hub", "cc-hub").unwrap();
        assert_eq!(m.indices, vec![3, 4, 5]);
    }

    #[test]
    fn match_is_case_insensitive() {
        assert!(fuzzy_match("CCH", "cc-hub").is_some());
        assert!(fuzzy_match("cch", "CC-HUB").is_some());
    }

    #[test]
    fn subsequence_matches_across_separators() {
        let m = fuzzy_match("cchub", "cc-hub").unwrap();
        assert_eq!(m.indices, vec![0, 1, 3, 4, 5]);
    }

    #[test]
    fn missing_chars_yield_none() {
        assert!(fuzzy_match("xyz", "cc-hub").is_none());
        assert!(fuzzy_match("hubb", "cc-hub").is_none());
    }

    #[test]
    fn boundary_start_outranks_mid_word() {
        // "hub" after the dash in cc-hub is a word start; inside
        // "github-stuff" it is mid-word at the same offset.
        assert!(score("hub", "cc-hub") > score("hub", "github-stuff"));
    }

    #[test]
    fn front_match_outranks_late_match() {
        assert!(score("redd", "reddit") > score("redd", "old-stuff-reddit"));
    }

    #[test]
    fn consecutive_run_outranks_scattered() {
        assert!(score("hub", "cc-hub") > score("hub", "hxuxbx"));
    }
}
