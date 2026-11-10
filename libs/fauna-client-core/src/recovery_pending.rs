//! **The pending-replacement alert's projection** — a seed-alone RecoveryKey
//! replacement window, read at any nest the identity holds an account on,
//! made into the standing critical alert (`identity-succession.md` § The
//! RecoveryKey → *Replacement*; the feeder is `critical-alerts.md` § Feeders).
//!
//! **Several readers, one alert.** A seed-alone replacement is requested at
//! every nest the identity is linked to, and each runs its own window
//! (`identity-succession.md` § Enforcement on the home nest → *Every nest the
//! identity is linked to*, clause (c)), so a window can be open at a nest no
//! device is bound to. Two readers feed the one per-identity alert: the
//! session-start sweep reads the bound nest ([`BOUND_SOURCE`]), and the
//! runtime's secondary leg reads each linked nest it reaches
//! ([`linked_source`]). Each posts or clears **its own reading**
//! ([`fauna_client_alerts::CriticalAlerts::post_from`]), so the bound nest
//! answering "nothing pending" never clears an alert a linked nest raised: the
//! alert clears only once every nest that raised it reads nothing pending.
//!
//! **How a linked nest's reading reaches the alert.** The leg runs in the
//! account runtime, which holds no alert registry; the sweep holds the
//! registry and reaches no linked nest. So the leg records what it read in
//! this process's **linked readings** ([`record_linked_readings`], keyed by
//! identity), and every sweep pass posts them beside the bound nest's reading
//! ([`sync_linked_readings`]) — one shared hand-off, no per-app wiring. And
//! the hand-off wakes the sweep: the readings carry a per-identity generation
//! that moves only when they change ([`linked_readings_generation`],
//! [`LinkedReadingsChanged`]), and the sweep loop's wait between passes also
//! ends on that change and posts the readings at once, with no nest read — so
//! a window found at a linked nest reaches the banner the moment the leg has
//! read it, not at the next pass of the 6 h clock (`critical-alerts.md`
//! § Mechanism → *How often the detector runs*, ruled 2026-11-07). A nest the
//! leg could not reach keeps its last reading (unreachable is not resolved); a
//! nest no longer linked drops its reading.
//!
//! **The readings are read by Settings and settled by the veto.** The veto
//! button renders only while a window is open *somewhere* — at the bound
//! nest, or at a linked nest alone, which is the window a seed thief would
//! open — so the Settings status reads the bound nest's answer **and** these
//! readings, the soonest-landing window winning
//! ([`earliest_linked_window`]; the read is
//! `fauna_client_recovery::status::kit_status`). And a veto that reached a
//! nest leaves no window there, so the gesture settles that nest's reading at
//! once ([`settle_linked_readings`], a merge that keeps every nest it did not
//! reach) and re-syncs the alert in the same call, instead of leaving the
//! banner to outlive the windows until the leg's next pass and the sweep's
//! next pass after it.
//!
//! **Why this crate.** The ceremonies' home is `fauna-client-recovery`, which
//! re-exports all of this; it sits above the secondary leg
//! (`fauna-account-plane`) in the dependency graph, as `recovery_chain`'s
//! module note explains.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

use fauna_client_alerts::CriticalAlerts;
use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;

/// i18n key for the headline — what happened and what it means.
pub const PENDING_ALARM_KEY: &str = "critical_alerts.recovery_replacement_pending";
/// i18n key for the identifying line — the key that would land, and the
/// deadline. Split from the headline so a user who *did* request the
/// replacement can check the fingerprint against the kit in their hand
/// without re-reading the alarm.
pub const PENDING_ALARM_DETAIL_KEY: &str = "critical_alerts.recovery_replacement_pending_detail";

/// How many leading hex characters of the pending key the alert shows.
///
/// Enough to compare against a kit in hand, short enough to read aloud. It is
/// a *comparison* aid, not a security check — the load-bearing decision is
/// "did I request this at all?", which needs no fingerprint.
const FINGERPRINT_CHARS: usize = 16;

/// Seconds in a day, for the whole-days countdown.
const SECS_PER_DAY: u64 = 86_400;

/// The source name of the bound nest's reading (the session-start sweep's).
pub const BOUND_SOURCE: &str = "bound";

/// The source name of one linked nest's reading (the secondary leg's).
#[must_use]
pub fn linked_source(nest_id: &[u8; 32]) -> String {
    format!("linked:{}", hex::encode(nest_id))
}

/// One pending seed-alone replacement, as a nest serves it — the standing
/// banner's model, re-exported as
/// `fauna_client_recovery::replacement::PendingReplacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReplacement {
    /// The key that would replace the current one, as 64-hex. Rendered so the
    /// user can compare it against a kit they hold: if it is not theirs, the
    /// request was not theirs either.
    pub new_recovery_pubkey_hex: String,
    /// Unix seconds the request was accepted.
    pub requested_at: i64,
    /// Unix seconds it lands if uncontested.
    pub lands_at: i64,
}

impl PendingReplacement {
    /// Seconds left before it lands, given the caller's `now` (unix seconds).
    ///
    /// Saturating: a window already past reads `0` rather than wrapping. The
    /// caller passes `now` rather than this reading a clock, so the projection
    /// stays pure and testable on every platform.
    #[must_use]
    pub fn remaining_secs(&self, now: i64) -> u64 {
        self.lands_at.saturating_sub(now).max(0) as u64
    }

    /// Whole days left, rounded **up** so a window in its final hour still
    /// reads N days, not 0 — the countdown rule every surface shares
    /// (`fauna_client_recovery::status::RecoveryKitStatus::status_line`, the
    /// FFI door `recovery_pending_days_remaining`, and the alert below).
    #[must_use]
    pub fn days_remaining(&self, now: i64) -> u64 {
        days_remaining_from(self.lands_at, now)
    }
}

/// [`PendingReplacement::days_remaining`], usable from just the raw
/// `lands_at` timestamp — what the FFI door has, without constructing a full
/// [`PendingReplacement`] for its other, irrelevant fields.
#[must_use]
pub fn days_remaining_from(lands_at: i64, now: i64) -> u64 {
    let remaining_secs = lands_at.saturating_sub(now).max(0) as u64;
    remaining_secs.div_ceil(SECS_PER_DAY)
}

impl From<fauna_protocol::recovery::ReplacementPendingInfo> for PendingReplacement {
    fn from(info: fauna_protocol::recovery::ReplacementPendingInfo) -> Self {
        Self {
            new_recovery_pubkey_hex: hex::encode(&info.new_recovery_pubkey),
            requested_at: info.requested_at,
            lands_at: info.lands_at,
        }
    }
}

/// The alert's registry key for `actor_id`.
///
/// Feeder-scoped and **identity-scoped**, per `critical-alerts.md` § Mechanism:
/// a re-check updates its own alert instead of stacking duplicates, and an
/// alert belonging to a departed identity can never be mistaken for the
/// incoming one's after an account switch. Per identity, **not** per nest:
/// windows at several nests are one condition, one alert.
#[must_use]
pub fn alert_key(actor_id: &ActorId) -> String {
    format!("recovery-replacement-pending:{}", actor_id.to_hex())
}

/// The alert's lines for an open window, as of `now` (unix seconds).
///
/// Pure — no clock read, no I/O. The countdown is rendered in **whole days,
/// rounded up**: "1 day" must not appear while there are still 47 hours left,
/// and a window in its final hours must never round down to "0 days" and read
/// as already lost.
#[must_use]
pub fn pending_replacement_alert_lines(
    pending: &PendingReplacement,
    now: i64,
) -> Vec<LocalizedText> {
    let fingerprint: String = pending
        .new_recovery_pubkey_hex
        .chars()
        .take(FINGERPRINT_CHARS)
        .collect();

    let mut detail = LocalizedText::key(PENDING_ALARM_DETAIL_KEY);
    detail.args.insert("fingerprint".into(), fingerprint);
    detail
        .args
        .insert("days".into(), pending.days_remaining(now).to_string());

    vec![LocalizedText::key(PENDING_ALARM_KEY), detail]
}

/// Post or clear `source`'s reading of this account's pending replacement.
///
/// `pending: None` **clears that source's reading only** — the alert itself
/// clears once no source holds it. The most urgent window (the one that lands
/// first) is the one the alert shows.
pub fn sync_pending_replacement_from(
    alerts: &CriticalAlerts,
    actor_id: &ActorId,
    source: &str,
    pending: Option<&PendingReplacement>,
    now: i64,
) {
    let key = alert_key(actor_id);
    match pending {
        Some(p) => alerts.post_from(
            key,
            source,
            p.lands_at,
            pending_replacement_alert_lines(p, now),
        ),
        None => alerts.clear_from(&key, source),
    }
}

/// Drop the readings of linked nests no longer in `linked` — a nest that was
/// unlinked can never be re-read to clear its own reading. The bound nest's
/// reading is untouched.
pub fn retain_linked_sources(alerts: &CriticalAlerts, actor_id: &ActorId, linked: &[[u8; 32]]) {
    let keep: Vec<String> = linked.iter().map(linked_source).collect();
    alerts.retain_sources(&alert_key(actor_id), |source| {
        source == BOUND_SOURCE || keep.iter().any(|k| k == source)
    });
}

/// What the secondary leg learned of one linked nest's window this pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedReading {
    /// The nest answered: a window is open there, or (`None`) nothing pends.
    Read(Option<PendingReplacement>),
    /// The nest could not be read (unreachable, another identity, a failed
    /// read): its previous reading stands — unreachable is not resolved, and
    /// clearing on error would let a nest that went offline silence a live
    /// compromise warning.
    Unread,
}

/// The open windows the leg last read at each linked nest, per identity.
type LinkedWindows = BTreeMap<[u8; 32], BTreeMap<[u8; 32], PendingReplacement>>;

/// This process's linked readings — the leg writes, the sweep reads (module
/// docs). In-memory, like the alerts it feeds.
static LINKED_WINDOWS: Mutex<LinkedWindows> = Mutex::new(BTreeMap::new());

/// Per identity, the readings' generation and the wakers waiting on it. A
/// write bumps it only when it CHANGED the readings, so a leg pass that
/// re-reads the same windows (the 300 s backstop, every few minutes for the
/// life of the session) moves nothing and wakes nobody. Taken after
/// [`LINKED_WINDOWS`]'s lock is released, never under it.
type Generations = BTreeMap<[u8; 32], (u64, Vec<Waker>)>;
static GENERATION: Mutex<Generations> = Mutex::new(BTreeMap::new());

/// The generation of `actor_id`'s linked readings: moves by one on every
/// write that changed them ([`record_linked_readings`],
/// [`settle_linked_readings`]). Read it, then wait on
/// [`LinkedReadingsChanged::since`] it — a change between the two still
/// resolves the wait.
#[must_use]
pub fn linked_readings_generation(actor_id: &ActorId) -> u64 {
    GENERATION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&actor_id.0)
        .map_or(0, |(generation, _)| *generation)
}

/// Move `actor_id`'s generation and wake everyone waiting on it. The wakers
/// are drained here, so a waiter registers again on its next poll and a
/// dropped waiter's entry lives only until the next change.
fn bump_generation(actor_id: &ActorId) {
    let wakers = {
        let mut all = GENERATION.lock().unwrap_or_else(|e| e.into_inner());
        let (generation, wakers) = all.entry(actor_id.0).or_insert((0, Vec::new()));
        *generation += 1;
        std::mem::take(wakers)
    };
    for waker in wakers {
        waker.wake();
    }
}

/// Resolves, with the generation then current, once `actor_id`'s linked
/// readings have changed since a generation read by
/// [`linked_readings_generation`] — at once when the change came between that
/// read and the first poll, so no change is lost to the gap. Runtime-free (a
/// plain [`Waker`] list), so it is polled by whatever drives the sweep loop on
/// each platform, and woken from the leg's own thread. One waker per task is
/// kept: a task polling it again re-uses its entry.
#[must_use = "a future does nothing until polled"]
pub struct LinkedReadingsChanged {
    actor: [u8; 32],
    seen: u64,
}

impl LinkedReadingsChanged {
    /// The change after `seen`, `actor_id`'s generation as the caller read it.
    pub fn since(actor_id: &ActorId, seen: u64) -> Self {
        Self {
            actor: actor_id.0,
            seen,
        }
    }
}

impl Future for LinkedReadingsChanged {
    type Output = u64;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u64> {
        let mut all = GENERATION.lock().unwrap_or_else(|e| e.into_inner());
        let (generation, wakers) = all.entry(self.actor).or_insert((0, Vec::new()));
        if *generation > self.seen {
            return Poll::Ready(*generation);
        }
        if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

/// Record one leg run's readings for `actor_id`: `readings` names every
/// linked nest the run visited, so a nest absent from it is no longer linked
/// and its reading is dropped.
pub fn record_linked_readings(actor_id: &ActorId, readings: &[([u8; 32], LinkedReading)]) {
    let changed = {
        let mut all = LINKED_WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
        let previous = all.remove(&actor_id.0).unwrap_or_default();
        let mut windows = BTreeMap::new();
        for (nest, reading) in readings {
            let window = match reading {
                LinkedReading::Read(window) => window.clone(),
                LinkedReading::Unread => previous.get(nest).cloned(),
            };
            if let Some(window) = window {
                windows.insert(*nest, window);
            }
        }
        let changed = windows != previous;
        if !windows.is_empty() {
            all.insert(actor_id.0, windows);
        }
        changed
    };
    if changed {
        bump_generation(actor_id);
    }
}

/// The open windows the leg last read at `actor_id`'s linked nests.
#[must_use]
pub fn linked_windows(actor_id: &ActorId) -> Vec<([u8; 32], PendingReplacement)> {
    LINKED_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&actor_id.0)
        .map(|w| w.iter().map(|(n, p)| (*n, p.clone())).collect())
        .unwrap_or_default()
}

/// Drop the readings of `settled` nests — the ones a veto reached, which
/// hold no window afterwards — and keep every other nest's reading as it
/// stands. A merge, unlike [`record_linked_readings`]'s replace-all: the veto
/// visited only the nests it could dial, and an unreachable nest's window
/// must stay loud until a veto reaches it.
pub fn settle_linked_readings(actor_id: &ActorId, settled: &[[u8; 32]]) {
    let changed = {
        let mut all = LINKED_WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(windows) = all.get_mut(&actor_id.0) else {
            return;
        };
        let mut changed = false;
        for nest in settled {
            changed |= windows.remove(nest).is_some();
        }
        if windows.is_empty() {
            all.remove(&actor_id.0);
        }
        changed
    };
    if changed {
        bump_generation(actor_id);
    }
}

/// The linked nest whose window lands first, with that window — what the
/// Settings status shows beside the bound nest's own answer, so the countdown
/// there and the banner's agree. `None` when no linked nest's last reading
/// holds a window — on web, a tab whose account runtime has not run the leg
/// has no readings, and reads the bound nest alone.
#[must_use]
pub fn earliest_linked_window(actor_id: &ActorId) -> Option<([u8; 32], PendingReplacement)> {
    linked_windows(actor_id)
        .into_iter()
        .min_by_key(|(_, window)| window.lands_at)
}

/// Post the linked readings into `alerts` beside the bound nest's — each
/// window as its nest's own source — and drop the sources of nests with no
/// open window. The sweep's half of the hand-off (module docs).
pub fn sync_linked_readings(alerts: &CriticalAlerts, actor_id: &ActorId, now: i64) {
    let windows = linked_windows(actor_id);
    for (nest, window) in &windows {
        sync_pending_replacement_from(alerts, actor_id, &linked_source(nest), Some(window), now);
    }
    let open: Vec<[u8; 32]> = windows.iter().map(|(n, _)| *n).collect();
    retain_linked_sources(alerts, actor_id, &open);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(lands_at: i64) -> PendingReplacement {
        PendingReplacement {
            new_recovery_pubkey_hex: "ab".repeat(32),
            requested_at: 0,
            lands_at,
        }
    }

    /// A window raised from a linked nest is never cleared by the bound nest
    /// reading nothing pending — the alert clears only once every nest that
    /// raised it reads nothing.
    #[test]
    fn the_bound_nest_reading_nothing_never_clears_a_linked_nests_window() {
        let alerts = CriticalAlerts::new();
        let actor = ActorId([7; 32]);
        let linked = linked_source(&[0x4c; 32]);

        sync_pending_replacement_from(&alerts, &actor, &linked, Some(&window(9_000)), 0);
        sync_pending_replacement_from(&alerts, &actor, BOUND_SOURCE, None, 0);
        assert_eq!(
            alerts.active().len(),
            1,
            "the linked nest's window still stands"
        );
        assert_eq!(alerts.active()[0].key, alert_key(&actor));

        sync_pending_replacement_from(&alerts, &actor, &linked, None, 0);
        assert!(
            alerts.active().is_empty(),
            "every nest reads nothing: the alert clears"
        );
    }

    /// Two windows are one alert, showing the one that lands first.
    #[test]
    fn two_nests_windows_are_one_alert_showing_the_sooner() {
        let alerts = CriticalAlerts::new();
        let actor = ActorId([7; 32]);
        let late = window(30 * 86_400);
        let soon = window(2 * 86_400);
        sync_pending_replacement_from(&alerts, &actor, BOUND_SOURCE, Some(&late), 0);
        sync_pending_replacement_from(&alerts, &actor, &linked_source(&[1; 32]), Some(&soon), 0);
        let active = alerts.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].lines[1].args["days"], "2");
    }

    /// The leg-to-sweep hand-off: a window read at a linked nest reaches the
    /// alert at the next sync, an unreachable nest keeps its reading, and the
    /// reading clears only when that nest reads nothing — or is unlinked.
    #[test]
    fn a_linked_reading_reaches_the_alert_and_unreachable_is_not_resolved() {
        let alerts = CriticalAlerts::new();
        // A distinct identity per test: the readings are process-wide.
        let actor = ActorId([0xa1; 32]);
        let nest = [0x4c; 32];

        record_linked_readings(&actor, &[(nest, LinkedReading::Read(Some(window(9_000))))]);
        sync_pending_replacement_from(&alerts, &actor, BOUND_SOURCE, None, 0);
        sync_linked_readings(&alerts, &actor, 0);
        assert_eq!(alerts.active().len(), 1);

        record_linked_readings(&actor, &[(nest, LinkedReading::Unread)]);
        sync_linked_readings(&alerts, &actor, 0);
        assert_eq!(alerts.active().len(), 1, "unreachable keeps the window");

        record_linked_readings(&actor, &[(nest, LinkedReading::Read(None))]);
        sync_linked_readings(&alerts, &actor, 0);
        assert!(
            alerts.active().is_empty(),
            "nothing pends anywhere: cleared"
        );

        record_linked_readings(&actor, &[(nest, LinkedReading::Read(Some(window(9_000))))]);
        record_linked_readings(&actor, &[]);
        sync_linked_readings(&alerts, &actor, 0);
        assert!(
            alerts.active().is_empty(),
            "an unlinked nest's window is dropped"
        );
    }

    /// The veto's settlement is a merge: the nests it reached lose their
    /// reading, the nest it could not reach keeps its window.
    #[test]
    fn settling_drops_the_reached_nests_and_keeps_the_unreached() {
        let actor = ActorId([0xa2; 32]);
        let (reached, unreached) = ([0x11; 32], [0x22; 32]);
        record_linked_readings(
            &actor,
            &[
                (reached, LinkedReading::Read(Some(window(5_000)))),
                (unreached, LinkedReading::Read(Some(window(9_000)))),
            ],
        );

        settle_linked_readings(&actor, &[reached, [0x33; 32]]);

        assert_eq!(
            linked_windows(&actor),
            vec![(unreached, window(9_000))],
            "the unreached nest's window stands"
        );
        settle_linked_readings(&actor, &[unreached]);
        assert!(linked_windows(&actor).is_empty());
        // Settling an identity with no readings is a no-op, not a panic.
        settle_linked_readings(&ActorId([0xa3; 32]), &[reached]);
    }

    /// Settings reads the soonest-landing linked window, and none when every
    /// reading is clear — the same window the alert shows.
    #[test]
    fn the_earliest_linked_window_is_the_one_that_lands_first() {
        let actor = ActorId([0xa4; 32]);
        assert_eq!(earliest_linked_window(&actor), None);
        let (late, soon) = ([0x11; 32], [0x22; 32]);
        record_linked_readings(
            &actor,
            &[
                (late, LinkedReading::Read(Some(window(30 * 86_400)))),
                (soon, LinkedReading::Read(Some(window(2 * 86_400)))),
            ],
        );
        assert_eq!(
            earliest_linked_window(&actor),
            Some((soon, window(2 * 86_400)))
        );
        record_linked_readings(&actor, &[]);
        assert_eq!(earliest_linked_window(&actor), None);
    }

    /// An unlinked nest's reading is dropped; the bound reading stays.
    #[test]
    fn retaining_drops_unlinked_nests_and_keeps_the_bound_reading() {
        let alerts = CriticalAlerts::new();
        let actor = ActorId([7; 32]);
        let (kept, gone) = ([1; 32], [2; 32]);
        sync_pending_replacement_from(&alerts, &actor, &linked_source(&gone), Some(&window(10)), 0);
        retain_linked_sources(&alerts, &actor, &[kept]);
        assert!(alerts.active().is_empty());

        sync_pending_replacement_from(&alerts, &actor, BOUND_SOURCE, Some(&window(10)), 0);
        retain_linked_sources(&alerts, &actor, &[]);
        assert_eq!(
            alerts.sources(&alert_key(&actor)),
            vec![BOUND_SOURCE.to_string()]
        );
    }

    /// The generation moves only when the readings change: a leg pass that
    /// re-reads the same windows — the backstop, every few minutes — wakes
    /// nobody, so the sweep loop's wait is never spun by it.
    #[test]
    fn the_generation_moves_only_when_the_readings_change() {
        let actor = ActorId([0xb1; 32]);
        let nest = [0x41; 32];
        let g0 = linked_readings_generation(&actor);

        record_linked_readings(&actor, &[(nest, LinkedReading::Read(None))]);
        assert_eq!(
            linked_readings_generation(&actor),
            g0,
            "nothing pending read into nothing: no change"
        );

        record_linked_readings(&actor, &[(nest, LinkedReading::Read(Some(window(500))))]);
        let g1 = linked_readings_generation(&actor);
        assert!(g1 > g0, "a window appeared");

        record_linked_readings(&actor, &[(nest, LinkedReading::Read(Some(window(500))))]);
        assert_eq!(
            linked_readings_generation(&actor),
            g1,
            "the same window again: no change"
        );
        record_linked_readings(&actor, &[(nest, LinkedReading::Unread)]);
        assert_eq!(
            linked_readings_generation(&actor),
            g1,
            "unread keeps the reading: no change"
        );
        settle_linked_readings(&actor, &[[0x42; 32]]);
        assert_eq!(
            linked_readings_generation(&actor),
            g1,
            "settling a nest with no reading: no change"
        );

        settle_linked_readings(&actor, &[nest]);
        assert!(linked_readings_generation(&actor) > g1, "the window went");
        assert_eq!(
            linked_readings_generation(&ActorId([0xb3; 32])),
            0,
            "another identity's generation is its own"
        );
    }

    /// A waiter is woken by the change, and a change that came between the
    /// generation read and the first poll resolves the wait at once.
    #[test]
    fn a_change_wakes_the_waiter_and_a_change_already_made_resolves_at_once() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::task::Wake;

        struct Flag(AtomicBool);
        impl Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let actor = ActorId([0xb2; 32]);
        let nest = [0x43; 32];
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&flag));
        let mut cx = Context::from_waker(&waker);

        let seen = linked_readings_generation(&actor);
        let mut changed = std::pin::pin!(LinkedReadingsChanged::since(&actor, seen));
        assert!(changed.as_mut().poll(&mut cx).is_pending());
        assert!(
            changed.as_mut().poll(&mut cx).is_pending(),
            "polling again registers once"
        );
        assert!(!flag.0.load(Ordering::SeqCst));

        record_linked_readings(&actor, &[(nest, LinkedReading::Read(Some(window(9))))]);
        assert!(flag.0.load(Ordering::SeqCst), "the change woke the waiter");
        assert!(matches!(changed.as_mut().poll(&mut cx), Poll::Ready(g) if g > seen));

        let seen = linked_readings_generation(&actor);
        settle_linked_readings(&actor, &[nest]);
        let mut changed = std::pin::pin!(LinkedReadingsChanged::since(&actor, seen));
        assert!(
            changed.as_mut().poll(&mut cx).is_ready(),
            "a change made before the first poll is not lost"
        );
    }
}
