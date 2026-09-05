//! The `cc-hub://` deep link — the URL an outside tool opens to make the hub
//! start something.
//!
//! Two kinds exist today: `review` and `task`.
//!
//! A browser extension puts "Light Review" and "Full Review" buttons on a
//! pull request page; each is a link like
//!
//! ```text
//! cc-hub://review?depth=light&pr=https%3A%2F%2Fbitbucket.example.com%2Fprojects%2FAPP%2Frepos%2Fsample-project%2Fpull-requests%2F11280
//! ```
//!
//! A locally configured OS handler routes the scheme to `cc-hub open <url>`,
//! which parses the string here and acts on it in [`crate::ops::link`].
//!
//! A caller with nobody watching — a persistent agent that starts reviews on
//! its own — adds `&post=80` to let the review post findings it is at least
//! that confident about instead of asking a human who is not there.
//!
//! A `task` link hands a Tasks-board card to a session:
//!
//! ```text
//! cc-hub://task?id=tk-1788509616255974000&dir=%2FUsers%2Fme%2Fgit%2Fself%2Fcc-hub
//! ```
//!
//! The `task-runner` agent opens one for every card that reaches In Progress
//! without a session of its own: it decides where the work belongs, and the
//! hub starts a session there, bound to the card, running the `task` skill.
//!
//! This module is pure: a string becomes a [`Link`], a [`ReviewLink`] renders
//! the prompt it stands for. Nothing here reads disk or spawns a process.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

pub const SCHEME: &str = "cc-hub";

/// A parsed `cc-hub://` URL. Each variant is one thing the hub can be asked
/// to start from outside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Review(ReviewLink),
    Task(TaskLink),
}

impl Link {
    /// The path segment that names this kind of link (`cc-hub://<kind>?…`).
    pub fn kind(&self) -> &'static str {
        match self {
            Link::Review(_) => "review",
            Link::Task(_) => "task",
        }
    }
}

impl FromStr for Link {
    type Err = LinkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let scheme_len = SCHEME.len() + 1;
        let has_scheme = s
            .get(..scheme_len)
            .is_some_and(|head| head.eq_ignore_ascii_case(&format!("{}:", SCHEME)));
        if !has_scheme {
            return Err(LinkError::NotCcHub(s.to_string()));
        }
        let rest = s[scheme_len..].trim_start_matches('/');
        let (kind, query) = rest.split_once('?').unwrap_or((rest, ""));
        let query = Query::parse(query);
        match kind.trim_end_matches('/') {
            "review" => Ok(Link::Review(ReviewLink {
                depth: query.required("depth")?.parse()?,
                pr: query.required("pr")?.parse()?,
                title: query.optional("title").map(str::to_string),
                post: query
                    .optional("post")
                    .map(PostThreshold::from_str)
                    .transpose()?,
            })),
            "task" => Ok(Link::Task(TaskLink {
                id: query.required("id")?.parse()?,
                dir: query.optional("dir").map(PathBuf::from),
                kind: query.optional("kind").map(str::to_string),
            })),
            other => Err(LinkError::UnknownKind(other.to_string())),
        }
    }
}

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

    /// The name the session is born with: `PR: <title>`, or `PR: <repo>#<n>`
    /// when the link carried no title.
    pub fn session_title(&self) -> String {
        let subject = self
            .title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| match self.pr.number() {
                Some(n) => format!("{}#{}", self.pr.repo(), n),
                None => self.pr.repo().to_string(),
            });
        titled("PR", &subject)
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

/// A pull request's web URL, plus what its path names: the repository slug
/// and the PR number — `…/repos/<slug>/pull-requests/<n>` on Bitbucket,
/// `…/<owner>/<slug>/pull/<n>` on GitHub. The slug is how the hub finds the
/// local checkout to review in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestUrl {
    url: String,
    repo: String,
    number: Option<u64>,
}

impl PullRequestUrl {
    pub fn as_str(&self) -> &str {
        &self.url
    }

    /// The repository slug named in the URL path.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// The pull request number, when the path has one.
    pub fn number(&self) -> Option<u64> {
        self.number
    }
}

impl fmt::Display for PullRequestUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.url)
    }
}

impl FromStr for PullRequestUrl {
    type Err = LinkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || LinkError::BadPullRequest(s.to_string());
        let after_scheme = s
            .strip_prefix("https://")
            .or_else(|| s.strip_prefix("http://"))
            .ok_or_else(bad)?;
        let path = after_scheme.split_once('/').map(|(_, p)| p).unwrap_or("");
        let path = path.split(['?', '#']).next().unwrap_or("");
        let segments: Vec<&str> = path.split('/').filter(|seg| !seg.is_empty()).collect();

        let after = |marker: &str| {
            segments
                .iter()
                .position(|seg| *seg == marker)
                .and_then(|i| segments.get(i + 1))
        };
        let before = |marker: &str| {
            segments
                .iter()
                .position(|seg| *seg == marker)
                .filter(|i| *i >= 2)
                .map(|i| &segments[i - 1])
        };
        let repo = after("repos").or_else(|| before("pull")).ok_or_else(bad)?;
        let number = after("pull-requests")
            .or_else(|| after("pull"))
            .and_then(|n| n.parse().ok());

        Ok(PullRequestUrl {
            url: s.to_string(),
            repo: repo.to_string(),
            number,
        })
    }
}

/// `cc-hub://task?id=<tk-…>[&dir=<path>][&kind=<word>]`: work a Tasks-board
/// card in an agent session bound to that card — the card's live session in
/// `dir` if it has one, else a fresh one. `dir` is where the session runs —
/// the caller decides that, because only it knows what kind of task this
/// is; without one the card's own recorded cwd is used. `kind`
/// is a word in the prompt and nothing more: the `task` skill owns what its
/// kinds mean, exactly as the review skill owns what `light` and `full` mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLink {
    pub id: BoardTaskId,
    pub dir: Option<PathBuf>,
    pub kind: Option<String>,
}

impl TaskLink {
    /// The opening prompt: the `task` skill, told which card it is working.
    /// `brief` is the card's own text — the hub reads it from the board,
    /// since the link carries an id rather than a copy of it.
    pub fn prompt(&self, brief: &str) -> String {
        let mut prompt = format!("/task --task {} {}", self.id, brief.trim());
        if let Some(kind) = &self.kind {
            prompt.push_str(&format!(
                "\n\nRouted here as kind `{}` — confirm or correct that in phase 0.",
                kind
            ));
        }
        prompt
    }

    /// The name the session is born with: `Task: <card text>`.
    pub fn session_title(&self, brief: &str) -> String {
        titled("Task", brief)
    }
}

/// A personal-board task id. The board mints `tk-<nanos>`; requiring the
/// prefix here means a link can only ever address a board card, never an
/// orchestrated `t-…` task that lives under a project and plays by other
/// rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardTaskId(String);

impl BoardTaskId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BoardTaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for BoardTaskId {
    type Err = LinkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let ok = s
            .strip_prefix("tk-")
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()));
        ok.then(|| BoardTaskId(s.to_string()))
            .ok_or_else(|| LinkError::BadTaskId(s.to_string()))
    }
}

/// `<prefix>: <subject>`, whitespace collapsed and the whole thing capped so
/// a novel of a title still fits a session card.
fn titled(prefix: &str, subject: &str) -> String {
    const MAX_CHARS: usize = 60;
    let subject = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = format!("{}: {}", prefix, subject);
    match title.char_indices().nth(MAX_CHARS) {
        Some((cut, _)) => format!("{}…", title[..cut].trim_end()),
        None => title,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    NotCcHub(String),
    UnknownKind(String),
    MissingParam(&'static str),
    BadDepth(String),
    BadPostThreshold(String),
    BadPullRequest(String),
    BadTaskId(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::NotCcHub(s) => write!(f, "not a {}:// link: {}", SCHEME, s),
            LinkError::UnknownKind(k) => {
                write!(f, "unknown link kind: {} (try `review` or `task`)", k)
            }
            LinkError::MissingParam(p) => write!(f, "missing `{}` parameter", p),
            LinkError::BadDepth(d) => write!(f, "depth must be `light` or `full`, got: {}", d),
            LinkError::BadPostThreshold(p) => {
                write!(f, "post must be a confidence percentage 1–100, got: {}", p)
            }
            LinkError::BadPullRequest(u) => write!(
                f,
                "not a pull request URL I can place (need `…/repos/<repo>/pull-requests/<n>` or `…/<owner>/<repo>/pull/<n>`): {}",
                u
            ),
            LinkError::BadTaskId(id) => write!(
                f,
                "not a Tasks-board id (expected `tk-<digits>`, as the board mints them): {}",
                id
            ),
        }
    }
}

impl std::error::Error for LinkError {}

/// The `key=value&…` tail of a link, percent-decoded. First occurrence of a
/// key wins.
struct Query(Vec<(String, String)>);

impl Query {
    fn parse(s: &str) -> Self {
        Query(
            s.split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    (percent_decode(k), percent_decode(v))
                })
                .collect(),
        )
    }

    /// The value under `key`, if present and non-empty.
    fn optional(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }

    fn required(&self, key: &'static str) -> Result<&str, LinkError> {
        self.optional(key).ok_or(LinkError::MissingParam(key))
    }
}

/// Undo `encodeURIComponent`: every `%XX` becomes its byte. `+` is left
/// alone — the producers here are URL encoders, not HTML forms, and a `+`
/// inside a URL is a `+`.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PR: &str =
        "https://bitbucket.example.com/projects/APP/repos/sample-project/pull-requests/11280";

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

    fn task(query: &str) -> TaskLink {
        let raw = format!("cc-hub://task?{}", query);
        match raw.parse::<Link>() {
            Ok(Link::Task(t)) => t,
            Ok(other) => panic!("{}: parsed as {}", raw, other.kind()),
            Err(e) => panic!("{}: {}", raw, e),
        }
    }

    #[test]
    fn parses_task_with_dir_and_kind() {
        let t = task("id=tk-1788509616255974000&dir=%2FUsers%2Fme%2Fgit%2Fself%2Fcc-hub&kind=hub");
        assert_eq!(t.id.as_str(), "tk-1788509616255974000");
        assert_eq!(t.dir, Some(PathBuf::from("/Users/me/git/self/cc-hub")));
        assert_eq!(t.kind.as_deref(), Some("hub"));
    }

    #[test]
    fn task_needs_only_an_id() {
        let t = task("id=tk-42");
        assert_eq!(t.dir, None);
        assert_eq!(t.kind, None);
        assert!(matches!(
            "cc-hub://task?dir=%2Ftmp".parse::<Link>(),
            Err(LinkError::MissingParam("id"))
        ));
    }

    #[test]
    fn only_a_board_id_can_be_addressed() {
        for bad in ["t-42", "tk-", "tk-abc", "42"] {
            let raw = format!("cc-hub://task?id={}", bad);
            assert!(
                matches!(raw.parse::<Link>(), Err(LinkError::BadTaskId(_))),
                "{} should not parse as a board id",
                bad
            );
        }
    }

    #[test]
    fn task_prompt_invokes_the_skill_with_the_card() {
        assert_eq!(
            task("id=tk-42").prompt("  Semantic Linter  "),
            "/task --task tk-42 Semantic Linter"
        );
        assert_eq!(
            task("id=tk-42&kind=ai-plugin").prompt("Semantic Linter"),
            "/task --task tk-42 Semantic Linter\n\nRouted here as kind `ai-plugin` \
— confirm or correct that in phase 0."
        );
    }

    #[test]
    fn titles_are_prefixed_and_capped() {
        assert_eq!(
            task("id=tk-42").session_title("Add a\n  commit  skill"),
            "Task: Add a commit skill"
        );
        let long = task("id=tk-42").session_title(&"word ".repeat(30));
        assert_eq!(long.chars().count(), 61);
        assert!(long.ends_with('…'), "{}", long);
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
    fn depth_is_case_insensitive_and_scheme_too() {
        let r: Link = format!("CC-HUB://review?depth=Full&pr={}", PR)
            .parse()
            .unwrap();
        assert_eq!(
            r,
            Link::Review(ReviewLink {
                depth: ReviewDepth::Full,
                pr: PR.parse().unwrap(),
                title: None,
                post: None,
            })
        );
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

    #[test]
    fn rejects_foreign_scheme() {
        assert!(matches!(
            "https://example.com".parse::<Link>(),
            Err(LinkError::NotCcHub(_))
        ));
    }

    #[test]
    fn rejects_unknown_kind() {
        assert!(matches!(
            "cc-hub://deploy?pr=x".parse::<Link>(),
            Err(LinkError::UnknownKind(k)) if k == "deploy"
        ));
    }

    #[test]
    fn rejects_missing_and_bad_params() {
        assert_eq!(
            format!("cc-hub://review?pr={}", PR).parse::<Link>(),
            Err(LinkError::MissingParam("depth"))
        );
        assert_eq!(
            "cc-hub://review?depth=light".parse::<Link>(),
            Err(LinkError::MissingParam("pr"))
        );
        assert!(matches!(
            format!("cc-hub://review?depth=deep&pr={}", PR).parse::<Link>(),
            Err(LinkError::BadDepth(d)) if d == "deep"
        ));
    }

    #[test]
    fn pull_request_url_names_its_repo() {
        let bb: PullRequestUrl = format!("{}/overview?commentId=1", PR).parse().unwrap();
        assert_eq!(bb.repo(), "sample-project");
        assert_eq!(bb.number(), Some(11280));
        let gh: PullRequestUrl = "https://github.com/octo/cc-hub/pull/7/files"
            .parse()
            .unwrap();
        assert_eq!(gh.repo(), "cc-hub");
        assert_eq!(gh.number(), Some(7));
        let unnumbered: PullRequestUrl =
            "https://bitbucket.example.com/projects/X/repos/thing/pull-requests/"
                .parse()
                .unwrap();
        assert_eq!(unnumbered.number(), None);
        assert!(matches!(
            "https://example.com/nothing/here".parse::<PullRequestUrl>(),
            Err(LinkError::BadPullRequest(_))
        ));
        assert!(matches!(
            "ftp://example.com/repos/x/pull-requests/1".parse::<PullRequestUrl>(),
            Err(LinkError::BadPullRequest(_))
        ));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%2Fb%3Fc%3D1"), "a/b?c=1");
        assert_eq!(percent_decode("plus+stays"), "plus+stays");
        assert_eq!(percent_decode("trailing%2"), "trailing%2");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%C3%A9"), "é");
    }
}
