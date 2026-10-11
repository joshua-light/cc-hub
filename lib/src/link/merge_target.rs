use super::chore::Chore;
use super::pull_request::{pull_request_titled, PullRequestUrl};

/// `cc-hub://merge-target?pr=<url>[&title=<text>][&kind=<word>]`: merge a
/// pull request's target branch into its source branch in a fresh agent
/// session, filed as a card on the Tasks board — the PR that fell behind,
/// or that conflicts. `title` and `kind` are as on a fix link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeTargetLink {
    pub pr: PullRequestUrl,
    pub title: Option<String>,
    pub kind: Option<String>,
}

impl Chore for MergeTargetLink {
    fn pr(&self) -> &PullRequestUrl {
        &self.pr
    }

    fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    /// `Merge Target: <title>`, or `Merge Target: <repo>#<n>` when the link
    /// carried no title — the button's own words, so the card reads as the
    /// thing that was clicked rather than as merging the pull request.
    fn session_title(&self) -> String {
        pull_request_titled("Merge Target", &self.pr, self.title.as_deref())
    }

    /// The standing orders for a merge. A merge, not a rebase, and no force
    /// push: the branch already has reviewers and comments anchored to its
    /// commits.
    fn prompt(&self) -> String {
        format!(
            "Switch to the branch of this PR and merge its target branch into it: {} \
             Fetch first, so both branches are current. \
             Merge, don't rebase, and never force-push: the branch is already under review. \
             Resolve each conflict keeping the intent of both sides; \
             if one needs a decision only the author can make, stop and ask. \
             Check that the merged result still builds where that can be checked here, \
             then commit the merge and push it.",
            self.pr
        )
    }

    /// Nothing to agree on: the pull request names both branches, and the
    /// pull request page is the verification.
    fn brief(&self) -> String {
        format!(
            "Brief\n\
             Problem: {} is behind its target branch.\n\
             Solution: merge the target branch into the pull request's branch — fetch, merge (no rebase, no force-push), resolve conflicts keeping both sides' intent, ask when one needs the author, push.\n\
             Verification: the pull request shows no conflicts and is not behind its target. This task has the one role; no hand-over.",
            self.pr
        )
    }

    fn done_when(&self) -> &'static str {
        "the merge is pushed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::task::BoardTaskId;
    use crate::link::tests::PR;
    use crate::link::{Link, LinkError};

    fn merge(query: &str) -> MergeTargetLink {
        let raw = format!("cc-hub://merge-target?{}", query);
        match raw.parse::<Link>() {
            Ok(Link::MergeTarget(x)) => x,
            Ok(other) => panic!("{}: parsed as {}", raw, other.kind()),
            Err(e) => panic!("{}: {}", raw, e),
        }
    }

    #[test]
    fn parses_merge_target() {
        let x = merge(&format!("pr={}&kind=tps", PR));
        assert_eq!(x.pr.repo(), "sample-project");
        assert_eq!(x.kind.as_deref(), Some("tps"));
        assert_eq!(
            "cc-hub://merge-target?title=x".parse::<Link>(),
            Err(LinkError::MissingParam("pr"))
        );
    }

    #[test]
    fn merge_target_prompt_names_the_pr_and_the_rules() {
        let prompt = merge(&format!("pr={}", PR)).prompt();
        assert!(
            prompt
                .starts_with("Switch to the branch of this PR and merge its target branch into it"),
            "{}",
            prompt
        );
        assert!(prompt.contains(PR), "{}", prompt);
        for rule in [
            "Fetch first",
            "don't rebase",
            "never force-push",
            "stop and ask",
            "push it",
        ] {
            assert!(prompt.contains(rule), "missing `{}` in: {}", rule, prompt);
        }
    }

    #[test]
    fn merge_target_prompt_for_a_card_ends_with_the_note_to_write() {
        let card: BoardTaskId = "tk-42".parse().unwrap();
        let prompt = merge(&format!("pr={}", PR)).prompt_for(&card);
        assert!(prompt.contains("When the merge is pushed"), "{}", prompt);
        assert!(
            prompt.contains("cc-hub board note --task tk-42 --text \"Pushed:"),
            "{}",
            prompt
        );
    }

    #[test]
    fn merge_target_session_title_wears_the_buttons_words() {
        assert_eq!(
            merge(&format!("pr={}", PR)).session_title(),
            "Merge Target: sample-project#11280"
        );
        assert_eq!(
            merge(&format!("pr={}&title=APP-1%20Flaky%20test", PR)).session_title(),
            "Merge Target: APP-1 Flaky test"
        );
    }
}
