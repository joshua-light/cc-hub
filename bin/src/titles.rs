use cc_hub_lib::{models, title};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long to suppress re-titling a session after a successful run. Long
/// enough to outlast any in-flight scan snapshot captured before the title
/// hit disk, so the same id doesn't get titled twice when the snap is
/// drained after the persist.
const TITLE_SUCCESS_COOLDOWN: Duration = Duration::from_secs(30);
/// How long to suppress re-titling after a failed run. Long enough to
/// avoid re-spawning a failing subprocess every scan tick, short enough
/// that a transient fault clears within a few minutes.
const TITLE_FAILURE_COOLDOWN: Duration = Duration::from_secs(300);
/// Initial deadline written when a titler is dispatched. Tightened to one
/// of the two cooldowns above when the spawned task finishes; this just
/// ensures concurrent scans see the id as "in flight" until then.
const TITLE_INFLIGHT_SENTINEL: Duration = Duration::from_secs(3600);

/// Background session titler state. `inflight` is a deadline map: each sid
/// is suppressed from re-kickoff until its `Instant` passes, so the
/// titler's persist + the next scan can settle without racing. `active` is
/// the narrower set of sids whose subprocess is actually running right
/// now, and drives the UI spinner. `gate` caps concurrent subprocesses.
pub(crate) struct Titles {
    inflight: Arc<Mutex<HashMap<String, Instant>>>,
    active: Arc<Mutex<HashSet<String>>>,
    gate: Arc<tokio::sync::Semaphore>,
}

impl Titles {
    pub(crate) fn new(concurrency: usize) -> Self {
        Self {
            inflight: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(HashSet::new())),
            gate: Arc::new(tokio::sync::Semaphore::new(concurrency)),
        }
    }

    /// Spawn a background `cc-hub-new -p` per session that has a first user
    /// message but no cached title yet, then stamp `titling` on every
    /// session whose titler is running.
    pub(crate) fn queue_missing(&self, sessions: &mut [models::SessionInfo]) {
        for session in sessions.iter() {
            if session.title.is_some() {
                continue;
            }
            // Skip Inactive sessions — they're synthesized from orphan JSONLs of
            // dead processes, so spending Haiku tokens to title them only pays
            // off cosmetically and re-burns every scan if the title fails.
            if session.state == models::SessionState::Inactive {
                continue;
            }
            let Some(first_msg) = session.summary.clone() else {
                continue;
            };
            let sid = session.session_id.clone();
            {
                let mut lock = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(&deadline) = lock.get(&sid) {
                    if deadline > Instant::now() {
                        continue;
                    }
                }
                // Sentinel deadline while in flight; the spawned task tightens
                // this to a success/failure cooldown when it finishes.
                lock.insert(sid.clone(), Instant::now() + TITLE_INFLIGHT_SENTINEL);
            }
            let inflight = Arc::clone(&self.inflight);
            let active = Arc::clone(&self.active);
            let gate = Arc::clone(&self.gate);
            tokio::spawn(async move {
                // Hold the permit across the blocking subprocess call so only
                // `[title].concurrency` children ever exist at once. The permit
                // drops at task end, freeing a slot for the next queued title.
                let _permit = gate.acquire_owned().await.ok();

                // Mark active only around the real work — the UI spinner is
                // driven by this narrower set, so a pending task still gated
                // on the semaphore doesn't flash ✎ on its card.
                active
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(sid.clone());

                let title_result = tokio::task::spawn_blocking({
                    let msg = first_msg.clone();
                    move || title::generate_title_blocking(&msg)
                })
                .await
                .ok()
                .flatten();

                active
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&sid);

                let Some(t) = title_result else {
                    log::warn!(
                        "title: generation failed for {}, retrying after cooldown",
                        models::short_sid(&sid)
                    );
                    inflight
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(sid.clone(), Instant::now() + TITLE_FAILURE_COOLDOWN);
                    return;
                };

                let sid_for_persist = sid.clone();
                let t_for_persist = t.clone();
                let persist = tokio::task::spawn_blocking(move || {
                    title::persist_title(&sid_for_persist, &t_for_persist)
                })
                .await;
                match persist {
                    Ok(Ok(())) => log::info!("title: sid={} → {:?}", models::short_sid(&sid), t),
                    Ok(Err(e)) => log::warn!("title: persist failed for {}: {}", sid, e),
                    Err(e) => log::warn!("title: persist task panicked for {}: {}", sid, e),
                }

                // Success cooldown outlasts any in-flight scan that captured
                // the pre-persist `title: None` snapshot, so the next drain
                // observes the cooldown and skips re-titling.
                inflight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(sid.clone(), Instant::now() + TITLE_SUCCESS_COOLDOWN);
            });
        }

        // Second pass: stamp titling on every session whose titler is running
        // (not just queued). Reading the active set after the spawns above
        // lets the UI show the spinner the instant a subprocess starts. A
        // titler still waiting for a permit shows no indicator; that window
        // is brief.
        let set = self.active.lock().unwrap_or_else(|e| e.into_inner());
        for session in sessions.iter_mut() {
            session.titling = set.contains(&session.session_id);
        }
    }
}
