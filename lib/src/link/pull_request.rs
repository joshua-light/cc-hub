use std::fmt;
use std::str::FromStr;

use super::{titled, LinkError};

/// `<prefix>: <title>` for a session about a pull request, falling back to
/// `<prefix>: <repo>#<n>` when the link carried no usable title.
pub(super) fn pull_request_titled(
    prefix: &str,
    pr: &PullRequestUrl,
    title: Option<&str>,
) -> String {
    let subject = title
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match pr.number() {
            Some(n) => format!("{}#{}", pr.repo(), n),
            None => pr.repo().to_string(),
        });
    titled(prefix, &subject)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::tests::PR;

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
}
