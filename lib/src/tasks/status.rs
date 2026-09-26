use serde::{Deserialize, Serialize};

/// A card's column. Backlog ("To-Do") → Planning → Running ("In Progress")
/// → Review → Done; see [`validate_status_transition`] for every legal edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Backlog,
    /// An agent was assigned and told to present a plan first; the card
    /// waits for the user to approve it (Space → Running).
    Planning,
    Running,
    /// The card's session wrote a `PR:` note, which carries the card here
    /// (see [`crate::ops::task::task_artifact_add_text`]) — so the board
    /// separates a card that wants a review from one that wants an answer.
    Review,
    Done,
}

impl TaskStatus {
    /// Lowercase wire name. Must match `#[serde(rename_all = "lowercase")]`
    /// above so JSON round-trips agree.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Backlog => "backlog",
            TaskStatus::Planning => "planning",
            TaskStatus::Running => "running",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
        }
    }

    /// Human label as shown on the Tasks-board columns ("To-Do",
    /// "In Progress", …). Distinct from [`Self::as_str`], the wire name.
    pub fn board_label(&self) -> &'static str {
        match self {
            TaskStatus::Backlog => "To-Do",
            TaskStatus::Planning => "Planning",
            TaskStatus::Running => "In Progress",
            TaskStatus::Review => "Review",
            TaskStatus::Done => "Done",
        }
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "backlog" => Ok(TaskStatus::Backlog),
            "planning" => Ok(TaskStatus::Planning),
            "running" => Ok(TaskStatus::Running),
            "review" => Ok(TaskStatus::Review),
            "done" => Ok(TaskStatus::Done),
            _ => Err(()),
        }
    }
}

/// Task priority (P1 highest … P4 lowest). Variants are declared in
/// ascending order (`P1 < P2 < P3 < P4`) so a plain ascending sort puts the
/// most urgent first. `P3` (the default) is skipped during serialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    P1,
    P2,
    #[default]
    P3,
    P4,
}

impl TaskPriority {
    /// True for the priority new tasks get; used by `skip_serializing_if`.
    pub fn is_default(&self) -> bool {
        *self == TaskPriority::default()
    }

    /// Short badge label shown on the card (`P1`–`P4`).
    pub fn label(self) -> &'static str {
        match self {
            TaskPriority::P1 => "P1",
            TaskPriority::P2 => "P2",
            TaskPriority::P3 => "P3",
            TaskPriority::P4 => "P4",
        }
    }
}

/// Single source of truth for legal task-status transitions. Enforced
/// centrally by [`super::store::update_task`], so no CLI verb or TUI keybind can invent an
/// edge the board doesn't have. Self-transitions are always allowed.
///
/// | from → to           | produced by                                         |
/// |---------------------|-----------------------------------------------------|
/// | Backlog ↔ Running   | manual move                                         |
/// | Backlog ↔ Planning  | assign (`s`/`S`) / manual move back                 |
/// | Running → Planning  | re-assign a stalled In-Progress card                |
/// | Done → Planning     | re-assign a finished card (reopen with agent)       |
/// | Planning → Running  | plan approved (Space), manual move                  |
/// | Running → Done      | finish                                              |
/// | Backlog → Done      | Space checks off a To-Do card directly              |
/// | Planning → Done     | finish an assigned card without approving the plan  |
/// | Done → Backlog      | reopen (Space on a Done card)                       |
/// | Done → Running      | manual move off Done                                |
/// | Running → Review    | a `PR:` note                                        |
/// | Planning → Review   | a `PR:` note from a card still in the plan gate     |
/// | Review  → Running   | manual move                                         |
/// | Review  → Planning  | re-assign a card whose PR needs another round       |
/// | Review  → Done      | finish                                              |
/// | Done → Review       | manual move off Done                                |
pub fn validate_status_transition(from: &TaskStatus, to: &TaskStatus) -> Result<(), String> {
    use TaskStatus::*;
    let legal = from == to
        || matches!(
            (from, to),
            (Backlog, Running)
                | (Running, Backlog)
                | (Running, Done)
                | (Backlog, Planning)
                | (Planning, Backlog)
                | (Planning, Running)
                // Re-assigning a stalled or finished card spawns a fresh
                // planning agent: any column can (re-)enter Planning.
                | (Running, Planning)
                | (Done, Planning)
                // Space checks off a card regardless of phase: a To-Do
                // that never started, or a Planning card whose agent the
                // user abandoned.
                | (Backlog, Done)
                | (Planning, Done)
                | (Done, Backlog)
                | (Done, Running)
                // The review gate: a `PR:` note carries the card here,
                // and it leaves either finished or back into another
                // round of work.
                | (Running, Review)
                | (Planning, Review)
                | (Review, Running)
                | (Review, Planning)
                | (Review, Done)
                | (Done, Review)
        );
    if legal {
        Ok(())
    } else {
        Err(format!(
            "illegal task status transition {:?} → {:?} (the board flows Backlog → Planning → \
             Running → Review → Done; Review can bounce back to Running/Planning, and Done can \
             reopen to Backlog/Running/Review)",
            from, to
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_edges_pass() {
        use TaskStatus::*;
        for (from, to) in [
            (Backlog, Running),
            (Running, Backlog),
            (Running, Done),
            (Backlog, Backlog),
            // The plan gate: assign → approve.
            (Backlog, Planning),
            (Planning, Backlog),
            (Planning, Running),
            // Re-assign flows re-enter Planning from anywhere.
            (Running, Planning),
            (Done, Planning),
            // Space checks off a card regardless of phase.
            (Backlog, Done),
            (Planning, Done),
            // Done reopens on the board.
            (Done, Backlog),
            (Done, Running),
            // The review gate: a `PR:` note carries the card in, and it
            // leaves finished or into another round.
            (Running, Review),
            (Planning, Review),
            (Review, Running),
            (Review, Planning),
            (Review, Done),
            (Done, Review),
        ] {
            assert!(
                validate_status_transition(&from, &to).is_ok(),
                "{:?} → {:?} should be legal",
                from,
                to
            );
        }
    }

    #[test]
    fn illegal_edges_fail() {
        use TaskStatus::*;
        for (from, to) in [(Backlog, Review), (Review, Backlog)] {
            assert!(
                validate_status_transition(&from, &to).is_err(),
                "{:?} → {:?} should be illegal",
                from,
                to
            );
        }
    }

    #[test]
    fn task_status_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Backlog).unwrap(),
            "\"backlog\""
        );
    }

    #[test]
    fn priority_orders_p1_before_p4() {
        assert!(TaskPriority::P1 < TaskPriority::P2);
        assert!(TaskPriority::P2 < TaskPriority::P3);
        assert!(TaskPriority::P3 < TaskPriority::P4);
    }
}
