//! The `cc-hub://` deep link — the URL an outside tool opens to make the hub
//! start something.
//!
//! Three kinds exist today: `review`, `fix` and `task`.
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
//! The same extension puts a "Fix" button beside those two. It is the other
//! side of a review: the session it starts works through the comments the
//! pull request already has instead of writing new ones. A fix is filed as a
//! card on the Tasks board the moment the link is opened, and runs through
//! the resource broker like any card; `kind` says which deliverable kind the
//! card is filed under, so the broker can pick the subscription account.
//!
//! ```text
//! cc-hub://fix?pr=https%3A%2F%2Fbitbucket.example.com%2Fprojects%2FAPP%2Frepos%2Fsample-project%2Fpull-requests%2F11280&kind=tps
//! ```
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
//! This module is pure: a string becomes a [`Link`], and each link renders
//! the prompt it stands for. Nothing here reads disk or spawns a process.

mod fix;
mod pull_request;
mod query;
mod review;
mod task;

pub use fix::FixLink;
pub use pull_request::PullRequestUrl;
pub use review::{PostThreshold, ReviewDepth, ReviewLink};
pub use task::{BoardTaskId, TaskLink};

use query::Query;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

pub const SCHEME: &str = "cc-hub";

/// A parsed `cc-hub://` URL. Each variant is one thing the hub can be asked
/// to start from outside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Review(ReviewLink),
    Fix(FixLink),
    Task(TaskLink),
}

impl Link {
    /// The path segment that names this kind of link (`cc-hub://<kind>?…`).
    pub fn kind(&self) -> &'static str {
        match self {
            Link::Review(_) => "review",
            Link::Fix(_) => "fix",
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
            "fix" => Ok(Link::Fix(FixLink {
                pr: query.required("pr")?.parse()?,
                title: query.optional("title").map(str::to_string),
                kind: query.optional("kind").map(str::to_string),
            })),
            "task" => Ok(Link::Task(TaskLink {
                id: query.required("id")?.parse()?,
                dir: query.optional("dir").map(PathBuf::from),
                kind: query.optional("kind").map(str::to_string),
                role: query.optional("role").map(str::to_string),
            })),
            other => Err(LinkError::UnknownKind(other.to_string())),
        }
    }
}

/// `<prefix>: <subject>`, whitespace collapsed and the whole thing capped so
/// a novel of a title still fits a session card.
pub(crate) fn titled(prefix: &str, subject: &str) -> String {
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
                write!(f, "unknown link kind: {} (try `review`, `fix` or `task`)", k)
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

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) const PR: &str =
        "https://bitbucket.example.com/projects/APP/repos/sample-project/pull-requests/11280";

    #[test]
    fn titles_are_prefixed_and_capped() {
        assert_eq!(
            titled("Task", "Add a\n  commit  skill"),
            "Task: Add a commit skill"
        );
        let long = titled("Task", &"word ".repeat(30));
        assert_eq!(long.chars().count(), 61);
        assert!(long.ends_with('…'), "{}", long);
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
}
