use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use super::LinkError;

/// `cc-hub://task?id=<tk-…>[&dir=<path>][&kind=<word>][&role=<word>]`: work
/// a Tasks-board card in an agent session bound to that card — the card's
/// live session in `dir` if it has one, else a fresh one. The `task-runner`
/// agent opens one for every In Progress card without a session.
///
/// `dir` is where the session runs. The caller picks it, because only it
/// knows what kind of task this is; without one the card's recorded cwd is
/// used. `kind` is a word in the prompt and nothing more: the `task` skill
/// owns what its kinds mean, as the review skill owns `light` and `full`.
/// `role` is the same kind of word, with one consequence for the hub: a link
/// that names a role is a hand-over, so it always starts a fresh session and
/// closes the one the card had, instead of reusing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLink {
    pub id: BoardTaskId,
    pub dir: Option<PathBuf>,
    pub kind: Option<String>,
    pub role: Option<String>,
}

impl TaskLink {
    /// The opening prompt: the `task` skill, told which card it is working.
    /// `brief` is the card's own text — the hub reads it from the board,
    /// since the link carries an id rather than a copy of it.
    pub fn prompt(&self, brief: &str) -> String {
        let mut prompt = format!("/task --task {}", self.id);
        if let Some(kind) = &self.kind {
            prompt.push_str(&format!(" --kind {}", kind));
        }
        if let Some(role) = &self.role {
            prompt.push_str(&format!(" --role {}", role));
        }
        format!("{} {}", prompt, brief.trim())
    }

    /// A link that names a role hands the card to a new session.
    pub fn is_handover(&self) -> bool {
        self.role.is_some()
    }

    /// The hand-over forward, to the session that tests the candidate and
    /// opens the pull request. `implementation` is the hand back.
    pub fn is_verification(&self) -> bool {
        self.role.as_deref() == Some("verification")
    }
}

/// A board task id. The board mints `tk-<nanos>`; requiring the prefix here
/// means a link can only ever address a board card.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::Link;

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
        assert!(!t.is_handover());
    }

    #[test]
    fn a_role_makes_the_link_a_handover() {
        let t = task("id=tk-42&kind=tps&role=verification");
        assert_eq!(t.role.as_deref(), Some("verification"));
        assert!(t.is_handover());
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
            "/task --task tk-42 --kind ai-plugin Semantic Linter"
        );
        assert_eq!(
            task("id=tk-42&kind=tps&role=verification").prompt("Fix it"),
            "/task --task tk-42 --kind tps --role verification Fix it"
        );
    }
}
