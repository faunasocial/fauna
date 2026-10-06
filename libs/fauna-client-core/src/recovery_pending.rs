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
//! ([`sync_linked_readings`]) — one shared hand-off, no per-app wiring, and a
//! window found at a linked nest reaches the banner at the sweep pass after
//! the leg read it. A nest the leg could not reach keeps its last reading
//! (unreachable is not resolved); a nest no longer linked drops its reading.
//!
//! **Why this crate.** The ceremonies' home is `fauna-client-recovery`, which
//! re-exports all of this; it sits above the secondary leg
//! (`fauna-account-plane`) in the dependency graph, as `recovery_chain`'s
//! module note explains.

use std::collections::BTreeMap;
use std::sync::Mutex;

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

/// Record one leg run's readings for `actor_id`: `readings` names every
/// linked nest the run visited, so a nest absent from it is no longer linked
/// and its reading is dropped.
pub fn record_linked_readings(actor_id: &ActorId, readings: &[([u8; 32], LinkedReading)]) {
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
    if !windows.is_empty() {
        all.insert(actor_id.0, windows);
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
}
