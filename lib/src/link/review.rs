use std::fmt;
use std::str::FromStr;

use super::pull_request::{pull_request_titled, PullRequestUrl};
use super::LinkError;

/// `cc-hub://review?depth=<light|full>&pr=<url>[&title=<text>][&post=<n>]`:
/// review a pull request in a fresh agent session. `title` is the pull
/// request's own title, as the page shows it; it names the session. `post`
/// lets the review post its confident findings on its own — without it the
/// review asks before posting anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLink {
    pub depth: ReviewDepth,
    pub pr: PullRequestUrl,
    pub title: Option<String>,
    pub post: Option<PostThreshold>,
}

impl ReviewLink {
    /// The opening prompt of the review session. The wording is what the
    /// `pr-review` skill triggers on, so "light review" / "full review" stay
    /// verbatim.
    pub fn prompt(&self) -> String {
        let review = format!("Let's do {} review of this PR: {}", self.depth, self.pr);
        match self.post {
            Some(post) => format!(
                "{} Post automatically all comments and questions with confidence >= {}.",
                review, post
            ),
            None => review,
        }
    }

    /// The opening prompt when the machine has no checkout of the repository.
    /// The review is the same review — the diff, the files and the comments
    /// all come over the wire — so the only thing worth saying is that there
    /// is no working tree, before the session spends its turns looking for
    /// one.
    pub fn prompt_without_checkout(&self) -> String {
        format!(
            "{} There is no local checkout of `{}` on this machine: read the diff, the files \
             and the comments over Bitbucket, and skip the steps that need a working tree.",
            self.prompt(),
            self.pr.repo()
        )
    }

    /// The name the session is born with: `PR: <title>`, or `PR: <repo>#<n>`
    /// when the link carried no title.
    pub fn session_title(&self) -> String {
        pull_request_titled("PR", &self.pr, self.title.as_deref())
    }
}

/// The confidence at or above which a review posts a finding without asking
/// first — a percentage, so 1–100. It is a number in the prompt, nothing
/// more: the review skill decides what its own confidence means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostThreshold(u8);

impl PostThreshold {
    pub fn percent(self) -> u8 {
        self.0
    }
}

impl fmt::Display for PostThreshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for PostThreshold {
    type Err = LinkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u8>()
            .ok()
            .filter(|percent| (1..=100).contains(percent))
            .map(PostThreshold)
            .ok_or_else(|| LinkError::BadPostThreshold(s.to_string()))
    }
}

/// How deep a review goes. The depth is a word in the prompt, nothing more:
/// the agent's review skill decides what "light" and "full" mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDepth {
    Light,
    Full,
}

impl ReviewDepth {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewDepth::Light => "light",
            ReviewDepth::Full => "full",
        }
    }
}

impl fmt::Display for ReviewDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReviewDepth {
    type Err = LinkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "light" => Ok(ReviewDepth::Light),
            "full" => Ok(ReviewDepth::Full),
            _ => Err(LinkError::BadDepth(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::tests::PR;
    use crate::link::Link;

    fn review(depth: &str) -> ReviewLink {
        let raw = format!(
            "cc-hub://review?depth={}&pr=https%3A%2F%2Fbitbucket.example.com%2Fprojects%2FAPP%2Frepos%2Fsample-project%2Fpull-requests%2F11280",
            depth
        );
        match raw.parse::<Link>() {
            Ok(Link::Review(r)) => r,
            Ok(other) => panic!("{}: parsed as {}", raw, other.kind()),
            Err(e) => panic!("{}: {}", raw, e),
        }
    }

    #[test]
    fn parses_light_review() {
        let r = review("light");
        assert_eq!(r.depth, ReviewDepth::Light);
        assert_eq!(r.pr.as_str(), PR);
        assert_eq!(r.pr.repo(), "sample-project");
    }

    #[test]
    fn prompt_names_depth_and_url() {
        assert_eq!(
            review("full").prompt(),
            format!("Let's do full review of this PR: {}", PR)
        );
        assert_eq!(
            review("light").prompt(),
            format!("Let's do light review of this PR: {}", PR)
        );
    }

    #[test]
    fn post_threshold_licenses_the_review_to_post() {
        let r: Link = format!("cc-hub://review?depth=light&pr={}&post=80", PR)
            .parse()
            .unwrap();
        let Link::Review(r) = r else {
            panic!("expected a review link")
        };
        assert_eq!(r.post.map(PostThreshold::percent), Some(80));
        assert_eq!(
            r.prompt(),
            format!(
                "Let's do light review of this PR: {} Post automatically all comments and questions with confidence >= 80.",
                PR
            )
        );
    }

    #[test]
    fn without_post_the_prompt_is_unchanged() {
        assert_eq!(review("light").post, None);
        assert!(!review("light").prompt().contains("Post automatically"));
    }

    #[test]
    fn post_must_be_a_percentage() {
        for bad in ["0", "101", "80%", "high", "-1", ">=80", ""] {
            let raw = format!("cc-hub://review?depth=light&pr={}&post={}", PR, bad);
            // An empty value reads as absent, like every other parameter.
            if bad.is_empty() {
                let Ok(Link::Review(r)) = raw.parse::<Link>() else {
                    panic!("{}: expected a link", raw);
                };
                assert_eq!(r.post, None);
                continue;
            }
            assert!(
                matches!(raw.parse::<Link>(), Err(LinkError::BadPostThreshold(p)) if p == bad),
                "{}: expected a threshold error",
                raw
            );
        }
        assert_eq!("100".parse::<PostThreshold>().unwrap().percent(), 100);
        assert_eq!("1".parse::<PostThreshold>().unwrap().percent(), 1);
    }

    #[test]
    fn title_param_names_the_session() {
        let r: Link = format!(
            "cc-hub://review?depth=light&pr={}&title=APP-15883%20Fix%20%20the%0Aflaky%20test",
            PR
        )
        .parse()
        .unwrap();
        let Link::Review(r) = r else {
            panic!("expected a review link")
        };
        assert_eq!(r.title.as_deref(), Some("APP-15883 Fix  the\nflaky test"));
        assert_eq!(r.session_title(), "PR: APP-15883 Fix the flaky test");
    }

    #[test]
    fn session_title_falls_back_to_repo_and_number() {
        assert_eq!(review("light").session_title(), "PR: sample-project#11280");
        let r: Link = format!("cc-hub://review?depth=light&pr={}&title=%20%20", PR)
            .parse()
            .unwrap();
        let Link::Review(r) = r else {
            panic!("expected a review link")
        };
        assert_eq!(r.session_title(), "PR: sample-project#11280");
    }

    #[test]
    fn session_title_is_capped() {
        let long = "x".repeat(200);
        let r: Link = format!("cc-hub://review?depth=light&pr={}&title={}", PR, long)
            .parse()
            .unwrap();
        let Link::Review(r) = r else {
            panic!("expected a review link")
        };
        let title = r.session_title();
        assert!(title.starts_with("PR: xxx"));
        assert!(title.ends_with('…'));
        assert_eq!(title.chars().count(), 61);
    }

    #[test]
    fn unencoded_pr_is_fine_too() {
        let r: Link = format!("cc-hub://review?pr={}&depth=light", PR)
            .parse()
            .unwrap();
        assert_eq!(r.kind(), "review");
    }
}
