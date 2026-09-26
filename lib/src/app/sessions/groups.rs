//! Scan snapshot to rendered group list: filtering, liveness bucketing,
//! task clustering, and re-anchoring the cursor by session id.

use super::spawn::spawning_placeholder;
use super::task_link_picker::task_display_title;
use crate::app::App;
use crate::models::{ProjectGroup, SessionInfo, SessionState, TaskBadge};
use crate::tasks::store::{TaskPriority, TaskStatus};
use std::collections::HashMap;

/// Reorder one group's sessions so task-linked cards lead the group and
/// cards linked to the same task always sit next to each other. Clusters
/// order by the liveness bucket of their most-live member first — an idle
/// cluster trails an active one just like unlinked cards do — then by task
/// priority (P1 first; a task that's gone has no readable priority and
/// ranks last within its bucket), ties by their first member in the
/// incoming order, and the remaining members are pulled up behind that
/// anchor; unlinked cards follow in their incoming order. The pass adds no
/// churn of its own — links and priorities only change on user action, and
/// liveness flips already reorder the underlying sort.
fn cluster_by_task(
    sessions: Vec<SessionInfo>,
    links: &HashMap<String, crate::tasks::session_links::TaskLink>,
    priorities: &HashMap<String, TaskPriority>,
) -> Vec<SessionInfo> {
    let mut slots: Vec<Option<SessionInfo>> = sessions.into_iter().map(Some).collect();
    let mut clusters: Vec<((u8, u8), Vec<SessionInfo>)> = Vec::new();
    for i in 0..slots.len() {
        let Some(task_id) = slots[i]
            .as_ref()
            .and_then(|s| links.get(&s.session_id))
            .map(|l| l.task_id.clone())
        else {
            continue;
        };
        let mut members = vec![slots[i].take().expect("checked above")];
        for slot in slots.iter_mut().skip(i + 1) {
            let same_task = slot
                .as_ref()
                .and_then(|s| links.get(&s.session_id))
                .is_some_and(|l| l.task_id == task_id);
            if same_task {
                members.push(slot.take().expect("checked above"));
            }
        }
        let liveness = members
            .iter()
            .map(|s| s.state.liveness_rank())
            .min()
            .expect("cluster has at least its anchor");
        let rank = priorities.get(&task_id).map_or(u8::MAX, |p| *p as u8);
        clusters.push(((liveness, rank), members));
    }
    // Stable sort: equal-key clusters keep their incoming (liveness) order.
    clusters.sort_by_key(|(key, _)| *key);
    let mut out: Vec<SessionInfo> = clusters.into_iter().flat_map(|(_, m)| m).collect();
    out.extend(slots.into_iter().flatten());
    out
}

impl App {
    /// Move the cursor onto `session_id` if it's currently visible.
    pub(super) fn select_session_by_id(&mut self, session_id: &str) {
        if let Some((gi, si)) = self.sessions.position_of(|s| s.session_id == session_id) {
            self.sessions.sel_group = gi;
            self.sessions.sel_in_group = si;
        }
    }

    /// Filter, group, and order `sessions` into the rendered group list.
    /// Pure with respect to selection — callers decide whether the result
    /// warrants a selection restore (see [`Self::adopt_groups`]).
    pub(super) fn build_groups(&self, sessions: &[SessionInfo]) -> Vec<ProjectGroup> {
        // Placeholder cards for spawns the scanner hasn't seen yet. Checked
        // against the unfiltered snapshot: once any session is hosted by the
        // watched tmux name, the real card replaces the placeholder (the
        // watch itself is cleared by `check_spawn_watches` on the same tick).
        let placeholders: Vec<SessionInfo> = self
            .spawn_watches
            .iter()
            .filter(|w| {
                !sessions
                    .iter()
                    .any(|s| s.tmux_session.as_deref() == Some(w.tmux_name.as_str()))
            })
            .map(|w| {
                let mut card = spawning_placeholder(w);
                // Once the user has named the spawn in the boot-time prompt,
                // show that name in place of the "starting…" label so the card
                // doesn't visibly revert until the real session lands.
                if let Some(Some(name)) = self.pending_spawn_names.get(&w.tmux_name) {
                    card.title = Some(name.clone());
                }
                card
            })
            .collect();

        let mut sessions: Vec<SessionInfo> = sessions
            .iter()
            .filter(|s| self.sessions.show_inactive || s.state != SessionState::Inactive)
            .cloned()
            .collect();

        // Placeholders ride the same sort/cluster pipeline as scanned
        // sessions so each one occupies the exact slot its real card will
        // take over. Prepended: the real session will be the group's newest,
        // and the scanner sorts newest-first within a liveness bucket.
        sessions.splice(0..0, placeholders);

        // Re-assert the liveness bucketing (active first, idle after,
        // inactive last) on the app-side state. The scanner already sorts by
        // (bucket, started_at desc, session id), but acks downgrade sessions
        // to Idle *after* that sort, so the scan order can be stale for acked
        // cards. The sort is stable and keys on the bucket alone: within a
        // bucket the scanner's order is preserved, and active-state flavors
        // (processing vs waiting) still can't reshuffle cards under the
        // cursor.
        sessions.sort_by_key(|s| s.state.liveness_rank());

        // Group sessions by cwd. HashMap::entry preserves bucket-relative
        // order, so each group comes out in the flat list's order.
        let mut group_map: HashMap<String, Vec<SessionInfo>> = HashMap::new();
        for s in sessions {
            group_map.entry(s.cwd.clone()).or_default().push(s);
        }

        // Cluster ordering needs each linked task's priority; resolve them
        // once per rebuild instead of per group. Gone tasks simply stay out
        // of the map and rank last.
        let priorities: HashMap<String, TaskPriority> = self
            .session_task_links
            .values()
            .filter_map(|l| Some((l.task_id.clone(), self.task_priority(&l.task_id)?)))
            .collect();

        let mut groups: Vec<ProjectGroup> = group_map
            .into_iter()
            .map(|(cwd, sessions)| {
                let name = sessions
                    .first()
                    .map(|s| s.project_name.clone())
                    .unwrap_or_default();
                ProjectGroup {
                    name,
                    cwd,
                    sessions: cluster_by_task(sessions, &self.session_task_links, &priorities),
                }
            })
            .collect();

        // Sort groups by lowercased name, tie-broken by cwd. `name` is only
        // the cwd basename, so distinct projects that share a basename
        // (~/work/api vs ~/personal/api) tie; without the cwd tie-break the
        // stable sort would preserve HashMap's random per-instance iteration
        // order and those groups would swap slots between scan ticks.
        groups.sort_by_key(|a| (a.name.to_lowercase(), a.cwd.clone()));
        groups
    }

    /// Priority of a task while it's still on the board. `None` once the
    /// task is gone.
    fn task_priority(&self, task_id: &str) -> Option<TaskPriority> {
        self.tasks.board.get(task_id).map(|t| t.priority)
    }

    /// Resolve a session's task link (`L`) to its card badge: live title and
    /// status from the board while the task is there, else the sidecar's
    /// title snapshot. `stale` covers both a missing and a Done task; either
    /// way the badge dims. `None` for unlinked sessions.
    pub(crate) fn task_badge(&self, session_id: &str) -> Option<TaskBadge> {
        let link = self.session_task_links.get(session_id)?;
        let task_id = link.task_id.as_str();
        let live = self
            .tasks
            .board
            .get(task_id)
            .map(|t| (task_display_title(t), t.status, t.priority));
        Some(match live {
            Some((title, status, priority)) => TaskBadge {
                task_id: task_id.to_string(),
                title,
                priority: Some(priority),
                stale: status == TaskStatus::Done,
            },
            None => {
                let snapshot = Some(link.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| crate::tasks::store::short_task_id(task_id));
                TaskBadge {
                    task_id: task_id.to_string(),
                    title: snapshot,
                    priority: None,
                    stale: true,
                }
            }
        })
    }

    /// Install a freshly-built group list and re-anchor the selection on the
    /// session id that was selected before, clamping when it's gone.
    pub(super) fn adopt_groups(&mut self, groups: Vec<ProjectGroup>) {
        let prev_id = self.selected_session_id();
        let sel_before = (self.sessions.sel_group, self.sessions.sel_in_group);
        self.sessions.groups = groups;

        // Re-anchor the selection on the previously-selected session id;
        // clamp into range when it's gone.
        let restored = prev_id.and_then(|id| self.sessions.position_of(|s| s.session_id == id));
        match restored {
            Some((gi, si)) => {
                self.sessions.sel_group = gi;
                self.sessions.sel_in_group = si;
            }
            None if self.sessions.groups.is_empty() => {
                self.sessions.sel_group = 0;
                self.sessions.sel_in_group = 0;
            }
            None => {
                self.sessions.sel_group =
                    self.sessions.sel_group.min(self.sessions.groups.len() - 1);
                let max_in = self.sessions.groups[self.sessions.sel_group]
                    .sessions
                    .len()
                    .saturating_sub(1);
                self.sessions.sel_in_group = self.sessions.sel_in_group.min(max_in);
            }
        }

        // Background selection rewrites are the prime suspect whenever "my
        // keypress did nothing" gets reported — make every one traceable.
        let sel_after = (self.sessions.sel_group, self.sessions.sel_in_group);
        if sel_before != sel_after {
            log::debug!(
                "scan: rebuild moved selection {:?} -> {:?} ({})",
                sel_before,
                sel_after,
                if restored.is_some() {
                    "id follow"
                } else {
                    "clamp"
                }
            );
        }
    }

    pub(super) fn rebuild_groups(&mut self) {
        let groups = self.build_groups(&self.sessions.last_sessions);
        self.adopt_groups(groups);
        // A rebuild (a filter toggle) can reveal already-seen sessions
        // without a scan. Register them as known now so the next scan doesn't
        // mistake one for a fresh arrival and jump the cursor onto it.
        self.sync_known_session_ids();
    }

    /// Refresh `known_session_ids` to the currently-visible session ids, but
    /// only once the first scan has seeded it. Rebuilds that happen outside a
    /// scan call this so a later scan's focus-jump fires only for genuinely
    /// new sessions — never for one a rebuild merely made visible. Skipping
    /// the `None` case preserves `update_sessions`' skip-jump-on-first-load.
    fn sync_known_session_ids(&mut self) {
        if self.sessions.known_session_ids.is_none() {
            return;
        }
        self.sessions.known_session_ids = Some(self.sessions.visible_ids());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::fake_session;

    #[test]
    fn build_groups_tie_breaks_equal_names_by_cwd() {
        let app = App::new();
        // Two projects share the basename "api" but live at different paths,
        // so `name` alone ties. The sort must be deterministic (by cwd), not
        // dependent on HashMap iteration order.
        let mut work = fake_session("s-work", SessionState::Idle);
        work.cwd = "/home/work/api".into();
        work.project_name = "api".into();
        let mut personal = fake_session("s-personal", SessionState::Idle);
        personal.cwd = "/home/personal/api".into();
        personal.project_name = "api".into();

        let order = |gs: &[ProjectGroup]| gs.iter().map(|g| g.cwd.clone()).collect::<Vec<_>>();
        let a = app.build_groups(&[work.clone(), personal.clone()]);
        let b = app.build_groups(&[personal, work]);
        // Insertion order must not change the result.
        assert_eq!(order(&a), order(&b));
        assert_eq!(
            order(&a),
            vec![
                "/home/personal/api".to_string(),
                "/home/work/api".to_string()
            ],
        );
    }

    #[test]
    fn task_links_mark_cards_without_splitting_groups() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            let a = fake_session("s-a", SessionState::Idle);
            let b = fake_session("s-b", SessionState::Idle);
            let c = fake_session("s-c", SessionState::Idle);
            crate::tasks::session_links::link(
                "s-b",
                crate::tasks::session_links::TaskLink {
                    task_id: "tk-9".into(),
                    title: "Fix auth".into(),
                },
            )
            .unwrap();
            app.session_task_links = crate::tasks::session_links::load();

            // A link is card metadata, not structure: all three sessions
            // stay in their one cwd group.
            let groups = app.build_groups(&[a, b, c]);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].sessions.len(), 3);

            // The linked session resolves a badge; no live task with this
            // id, so it falls back to the sidecar's title snapshot and
            // marks itself stale. Unlinked sessions resolve none.
            let badge = app.task_badge("s-b").expect("badge");
            assert_eq!(badge.task_id, "tk-9");
            assert_eq!(badge.title, "Fix auth");
            assert!(badge.stale);
            assert!(app.task_badge("s-a").is_none());
        });
    }

    #[test]
    fn linked_sessions_lead_the_group_and_cluster_by_task() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            // An unlinked card leads the liveness order, two tk-1 members
            // straddle a liveness bucket boundary, and another unlinked
            // card trails.
            let b = fake_session("s-b", SessionState::Processing);
            let a = fake_session("s-a", SessionState::Processing);
            let c = fake_session("s-c", SessionState::Idle);
            let d = fake_session("s-d", SessionState::Idle);
            for sid in ["s-a", "s-c"] {
                crate::tasks::session_links::link(
                    sid,
                    crate::tasks::session_links::TaskLink {
                        task_id: "tk-1".into(),
                        title: "Fix auth".into(),
                    },
                )
                .unwrap();
            }
            app.session_task_links = crate::tasks::session_links::load();

            let groups = app.build_groups(&[b, a, c, d]);
            assert_eq!(groups.len(), 1);
            let order: Vec<&str> = groups[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect();
            // The tk-1 cluster jumps ahead of the unlinked s-b even though
            // s-b is first in the liveness order, and s-c is pulled up
            // behind its anchor across the bucket boundary; the unlinked
            // cards follow in their relative order.
            assert_eq!(order, vec!["s-a", "s-c", "s-b", "s-d"]);
        });
    }

    #[test]
    fn task_clusters_order_by_liveness_then_priority() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            let low = app.tasks.board.add("low task").unwrap().unwrap();
            app.tasks
                .board
                .set_priority(&low, TaskPriority::P4)
                .unwrap();
            let high = app.tasks.board.add("high task").unwrap().unwrap();
            app.tasks
                .board
                .set_priority(&high, TaskPriority::P1)
                .unwrap();

            // The high-priority cluster is idle, so it trails both active
            // clusters despite its P1 — liveness buckets first. Within the
            // active bucket priority takes over: P4 beats the unreadable
            // (gone) task, which has no priority and ranks last there.
            let gone_s = fake_session("s-gone", SessionState::Processing);
            let low_s = fake_session("s-low", SessionState::Processing);
            let high_s = fake_session("s-high", SessionState::Idle);
            let unlinked = fake_session("s-plain", SessionState::Processing);
            for (sid, task_id) in [
                ("s-gone", "tk-deleted"),
                ("s-low", low.as_str()),
                ("s-high", high.as_str()),
            ] {
                crate::tasks::session_links::link(
                    sid,
                    crate::tasks::session_links::TaskLink {
                        task_id: task_id.into(),
                        title: String::new(),
                    },
                )
                .unwrap();
            }
            app.session_task_links = crate::tasks::session_links::load();

            let groups = app.build_groups(&[gone_s, low_s, high_s, unlinked]);
            assert_eq!(groups.len(), 1);
            let order: Vec<&str> = groups[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect();
            assert_eq!(order, vec!["s-low", "s-gone", "s-high", "s-plain"]);
        });
    }

    #[test]
    fn task_badge_prefers_live_title_and_dims_done() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("Ship the parser").unwrap().unwrap();
            crate::tasks::session_links::link(
                "s-linked",
                crate::tasks::session_links::TaskLink {
                    task_id: id.clone(),
                    title: "old snapshot".into(),
                },
            )
            .unwrap();
            app.session_task_links = crate::tasks::session_links::load();

            let badge = app.task_badge("s-linked").expect("badge");
            // Live board task wins over the sidecar snapshot.
            assert_eq!(badge.title, "Ship the parser");
            assert!(!badge.stale);

            // A Done task keeps the badge but dims it.
            app.tasks
                .board
                .set_status(&id, crate::tasks::store::TaskStatus::Done)
                .unwrap();
            assert!(app.task_badge("s-linked").unwrap().stale);
        });
    }

    #[test]
    fn build_groups_orders_acked_idle_behind_active_sessions() {
        let app = App::new();
        // Simulate the post-scan ack downgrade: the flat list arrives in
        // scanner order (both were active when sorted), but one is Idle by
        // the time groups are built. build_groups must re-bucket it last.
        let mut idle = fake_session("acked", SessionState::Idle);
        idle.tmux_session = None;
        let mut active = fake_session("active", SessionState::Processing);
        active.tmux_session = None;
        let groups = app.build_groups(&[idle, active]);
        let order: Vec<&str> = groups[0]
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(order, vec!["active", "acked"]);
    }
}
