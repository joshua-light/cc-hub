use super::pull_request::PullRequestUrl;
use super::task::BoardTaskId;

/// Work on a pull request's own branch, asked for by a link and filed as a
/// Tasks-board card: [`FixLink`](super::FixLink) works through the review
/// comments, [`MergeTargetLink`](super::MergeTargetLink) brings the target
/// branch in. The hub files, starts and routes every chore the same way;
/// a chore only owns its words.
pub trait Chore {
    /// The pull request whose branch the work lands on.
    fn pr(&self) -> &PullRequestUrl;

    /// The deliverable kind the card is filed under — one of the board's
    /// configured kinds — and the key the resource broker routes the
    /// session's account by.
    fn kind(&self) -> Option<&str>;

    /// The name the session and the card are born with.
    fn session_title(&self) -> String;

    /// The standing orders: the card's text, and the session's opening
    /// prompt before the card exists.
    fn prompt(&self) -> String;

    /// The card's first note: what a task session would have agreed with
    /// the user in the plan gate, written down without asking.
    fn brief(&self) -> String;

    /// When the session is done, as the clause after "When": the moment it
    /// writes its one note on the card.
    fn done_when(&self) -> &'static str;

    /// The opening prompt once the chore is filed as board card `card`: the
    /// standing orders, then where the outcome is written. The card is the
    /// user's view of the work, so the session closes it out with one note.
    fn prompt_for(&self, card: &BoardTaskId) -> String {
        format!(
            "{}\n\nThis work is card {} on the Tasks board. \
             When {}, write one note on it: \
             cc-hub board note --task {} --text \"Pushed: <what changed, one line>\"",
            self.prompt(),
            card,
            self.done_when(),
            card
        )
    }
}
