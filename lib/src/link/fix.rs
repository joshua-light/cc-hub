use super::chore::Chore;
use super::pull_request::{pull_request_titled, PullRequestUrl};

/// `cc-hub://fix?pr=<url>[&title=<text>][&kind=<word>]`: address the review
/// comments of a pull request in a fresh agent session, filed as a card on
/// the Tasks board. `title` is the pull request's own title, as on a review
/// link; it names the session and the card. `kind` is the card's kind (see
/// [`Chore::kind`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixLink {
    pub pr: PullRequestUrl,
    pub title: Option<String>,
    pub kind: Option<String>,
}

impl Chore for FixLink {
    fn pr(&self) -> &PullRequestUrl {
        &self.pr
    }

    fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    /// `Fix: <title>`, or `Fix: <repo>#<n>` when the link carried no title.
    fn session_title(&self) -> String {
        pull_request_titled("Fix", &self.pr, self.title.as_deref())
    }

    /// The standing orders for working through a review. The comments are
    /// tracked as Bitbucket tasks so the reviewer can see what was addressed
    /// and what was answered.
    fn prompt(&self) -> String {
        format!(
            "Switch to the branch of this PR and address all the comments in it: {} \
             Mark each comment as a task; once the fix is committed and pushed, mark that task as done. \
             If a comment asks a question, answer it. \
             If a comment is ambiguous, ask for clarification. \
             If a comment is already resolved or done, skip it. \
             Always add a remark that the response is written by Claude/Codex.",
            self.pr
        )
    }

    /// A review already asked, so the brief writes itself: the comments are
    /// the problem, working them is the solution, and the reviewer is the
    /// verification.
    fn brief(&self) -> String {
        format!(
            "Brief\n\
             Problem: {} has review comments waiting on the author.\n\
             Solution: address them on the pull request's branch — one Bitbucket task per comment, done once its fix is committed and pushed; answer questions, ask when a comment is ambiguous.\n\
             Verification: the reviewer re-reads the pull request. This task has the one role; no hand-over.",
            self.pr
        )
    }

    fn done_when(&self) -> &'static str {
        "every comment is handled and pushed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::task::BoardTaskId;
    use crate::link::tests::PR;
    use crate::link::{Link, LinkError};

    fn fix(query: &str) -> FixLink {
        let raw = format!("cc-hub://fix?{}", query);
        match raw.parse::<Link>() {
            Ok(Link::Fix(x)) => x,
            Ok(other) => panic!("{}: parsed as {}", raw, other.kind()),
            Err(e) => panic!("{}: {}", raw, e),
        }
    }

    #[test]
    fn parses_fix() {
        let x = fix(&format!("pr={}", PR));
        assert_eq!(x.pr.as_str(), PR);
        assert_eq!(x.pr.repo(), "sample-project");
        assert_eq!(x.title, None);
        assert_eq!(x.kind, None);
        assert_eq!(
            fix(&format!("pr={}&kind=tps", PR)).kind.as_deref(),
            Some("tps")
        );
        assert_eq!(
            "cc-hub://fix?title=x".parse::<Link>(),
            Err(LinkError::MissingParam("pr"))
        );
    }

    #[test]
    fn fix_prompt_names_the_pr_and_the_rules() {
        let prompt = fix(&format!("pr={}", PR)).prompt();
        assert!(
            prompt.starts_with("Switch to the branch of this PR"),
            "{}",
            prompt
        );
        assert!(prompt.contains(PR), "{}", prompt);
        for rule in [
            "Mark each comment as a task",
            "mark that task as done",
            "asks a question, answer it",
            "ambiguous, ask for clarification",
            "already resolved or done, skip it",
            "written by Claude/Codex",
        ] {
            assert!(prompt.contains(rule), "missing `{}` in: {}", rule, prompt);
        }
    }

    #[test]
    fn fix_prompt_for_a_card_ends_with_the_note_to_write() {
        let card: BoardTaskId = "tk-42".parse().unwrap();
        let prompt = fix(&format!("pr={}", PR)).prompt_for(&card);
        assert!(prompt.starts_with(&fix(&format!("pr={}", PR)).prompt()));
        assert!(
            prompt.contains("card tk-42 on the Tasks board"),
            "{}",
            prompt
        );
        assert!(
            prompt.contains("cc-hub board note --task tk-42 --text \"Pushed:"),
            "{}",
            prompt
        );
    }

    #[test]
    fn fix_session_title_mirrors_review() {
        assert_eq!(
            fix(&format!("pr={}", PR)).session_title(),
            "Fix: sample-project#11280"
        );
        assert_eq!(
            fix(&format!("pr={}&title=APP-1%20Flaky%20%20test", PR)).session_title(),
            "Fix: APP-1 Flaky test"
        );
    }
}
