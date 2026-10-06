//! Cross-page critical-alerts registry — the shared half of the "something is
//! very wrong" surface (`docs/goal/behavior/critical-alerts.md` owns the
//! contract; ui.yaml `global:` owns the `critical-alerts`/`critical-alert[N]`
//! element IDs).
//!
//! One app-wide [`CriticalAlerts`] instance per client process. Feeders (the
//! first: ATProto genesis-seniority verification) `post` keyed alerts when a
//! possible-compromise / data-loss-imminent condition is detected and `clear`
//! them when a re-check finds the condition resolved; the shell renders
//! `active()` as a permanent, non-dismissable banner list on every
//! authenticated page. Alert text is machine-composed [`LocalizedText`] so all
//! seven apps say the same thing.
//!
//! The severity bar is deliberately high and owned by the goal doc: ordinary
//! failures never post here. Lifetime is in-memory per app session — the
//! detector, not a stored flag, is the source of truth.

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_alerts");

/// Shared wasm-bindgen glue for a wasm chunk hosting its own `CriticalAlerts`
/// registry — see the module doc for why the registry itself stays
/// per-chunk while this boilerplate is shared.
#[cfg(target_arch = "wasm32")]
pub mod wasm_glue;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fauna_core::localized::LocalizedText;
use serde::Serialize;

/// One active alert, as the shell renders it (one `critical-alert[N]` row).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CriticalAlertRow {
    /// Stable feeder-scoped key (e.g. `atproto-custody:<did>`).
    pub key: String,
    /// The alert's text lines, rendered verbatim by the client.
    pub lines: Vec<LocalizedText>,
}

/// Repaint callback — the shell re-reads [`CriticalAlerts::active`] on every
/// tick (the `AtprotoSettingsObserver` shape). `with_foreign`: a native
/// FFI client (android/apple/windows) implements this directly in
/// Kotlin/Swift/C#, exactly as it implements `AtprotoSettingsObserver`.
///
/// **`on_changed` is a prompt scheduling hand-off, never the repaint itself**
/// (`critical-alerts.md` § Mechanism owns the dispatch contract). It runs
/// synchronously on the posting feeder's own thread — wake or schedule the
/// platform's repaint (send on a channel, write a `StateFlow`, post to the UI
/// context, dispatch a main-actor task, set a store) and return. It must never
/// block, never do I/O, and never touch UI-affine state inline: an observer
/// that stalls or dies here takes the posting feeder — and the rest of its
/// sweep pass — down with it. Synchronous registry *reads* from inside the
/// callback (`active`, `active_lines`, the pass counters) are allowed and
/// normal; registry *mutation* (`post`, `clear`, `clear_all`) is not.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait CriticalAlertsObserver: Send + Sync {
    fn on_changed(&self);
}

/// The app-wide registry. Construct once per client process and hand an
/// `Arc` to every feeder and to the shell. `uniffi::Object`: native FFI
/// clients get this as a handle whose methods they call directly (see
/// `libs/fauna-ffi/src/critical_alerts.rs`'s process-wide singleton
/// accessor) — the same shape `AtprotoSettingsMachine` uses.
/// One source's reading of a multi-source key: its urgency rank (lowest
/// shows) and its lines.
type SourcedLines = (i64, Vec<LocalizedText>);

#[derive(Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct CriticalAlerts {
    // BTreeMap: deterministic render order across clients.
    alerts: Mutex<BTreeMap<String, Vec<LocalizedText>>>,
    // Per-source readings of a multi-source key — see [`CriticalAlerts::post_from`].
    // Held across the post/clear it derives, so two sources' updates serialize.
    sourced: Mutex<BTreeMap<String, BTreeMap<String, SourcedLines>>>,
    observers: Mutex<Vec<Arc<dyn CriticalAlertsObserver>>>,
    // Bumped by `clear_all` and nothing else — see [`CriticalAlerts::teardown_epoch`].
    teardown_epoch: AtomicU64,
    // The sweep's own pass counters — see [`CriticalAlerts::sweep_passes_started`].
    sweep_started: AtomicU64,
    sweep_completed: AtomicU64,
    // A hand-off that faulted rather than returned — see [`CriticalAlerts::observer_faults`].
    observer_faults: AtomicU64,
}

impl CriticalAlerts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Post (or re-post) the alert under `key`. Idempotent per key: re-posting
    /// identical lines does not notify; changed lines replace and notify.
    ///
    /// Feeder-only (called from shared-Rust machines, never from an app
    /// shell) — not in the `uniffi::export` block below: `impl Into<String>`
    /// is a generic parameter, which UniFFI's FFI ABI cannot express.
    pub fn post(&self, key: impl Into<String>, lines: Vec<LocalizedText>) {
        let key = key.into();
        let changed = {
            let mut alerts = self.alerts.lock().unwrap();
            alerts.get(&key) != Some(&lines) && {
                alerts.insert(key, lines);
                true
            }
        };
        if changed {
            self.notify();
        }
    }

    /// Clear the alert under `key` (the condition re-checked and resolved).
    /// No-op if absent. Feeder-only, same reason as [`Self::post`].
    pub fn clear(&self, key: &str) {
        let changed = self.alerts.lock().unwrap().remove(key).is_some();
        if changed {
            self.notify();
        }
    }

    /// Post `key` as **one source's** reading of a condition several sources
    /// observe — a pending RecoveryKey replacement read at the bound nest and
    /// at every linked nest (`critical-alerts.md` § Feeders) is the case it
    /// exists for. The alert stands while any source holds it and shows the
    /// lines of the source with the **lowest** `rank` (ties by source name):
    /// the most urgent reading, e.g. the window that lands first. Feeder-only,
    /// as [`Self::post`].
    ///
    /// Never mix this with a bare [`Self::post`]/[`Self::clear`] on the same
    /// key: those would overwrite or drop the merged alert behind the other
    /// sources' backs, which is exactly the clear-by-collision this exists to
    /// prevent.
    pub fn post_from(
        &self,
        key: impl Into<String>,
        source: impl Into<String>,
        rank: i64,
        lines: Vec<LocalizedText>,
    ) {
        let key = key.into();
        let mut sourced = self.sourced.lock().unwrap();
        sourced
            .entry(key.clone())
            .or_default()
            .insert(source.into(), (rank, lines));
        self.settle_sourced(&mut sourced, &key);
    }

    /// Withdraw one source's reading of `key`: the alert clears only once no
    /// source holds it, and otherwise falls back to the next most urgent
    /// reading. No-op if that source held nothing. Feeder-only.
    pub fn clear_from(&self, key: &str, source: &str) {
        self.retain_sources(key, |s| s != source);
    }

    /// Keep only the sources of `key` that `keep` accepts — how a feeder drops
    /// the readings of sources that no longer exist (a nest that was
    /// unlinked) without naming each. Feeder-only.
    pub fn retain_sources(&self, key: &str, keep: impl Fn(&str) -> bool) {
        let mut sourced = self.sourced.lock().unwrap();
        let Some(readings) = sourced.get_mut(key) else {
            return;
        };
        let before = readings.len();
        readings.retain(|source, _| keep(source));
        if readings.len() != before {
            self.settle_sourced(&mut sourced, key);
        }
    }

    /// The sources currently holding `key`, in name order — read-only, for a
    /// feeder's tests and logs. Never from inside
    /// [`CriticalAlertsObserver::on_changed`]: a sourced update notifies while
    /// it holds the lock this reads.
    pub fn sources(&self, key: &str) -> Vec<String> {
        self.sourced
            .lock()
            .unwrap()
            .get(key)
            .map(|r| r.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Derive `key`'s alert from its source readings, under the `sourced` lock.
    fn settle_sourced(
        &self,
        sourced: &mut BTreeMap<String, BTreeMap<String, SourcedLines>>,
        key: &str,
    ) {
        let shown = sourced.get(key).and_then(|readings| {
            readings
                .iter()
                .min_by(|(sa, (ra, _)), (sb, (rb, _))| ra.cmp(rb).then_with(|| sa.cmp(sb)))
                .map(|(_, (_, lines))| lines.clone())
        });
        match shown {
            Some(lines) => self.post(key.to_string(), lines),
            None => {
                sourced.remove(key);
                self.clear(key);
            }
        }
    }

    /// How many identity teardowns this registry has seen — a counter bumped by
    /// [`Self::clear_all`] and by nothing else.
    ///
    /// It exists so a **long-lived background task can tell that the identity it
    /// was working for is gone** without every app having to plumb a liveness
    /// token through its own teardown paths. The periodic critical-alert
    /// re-sweep (`fauna_client_alert_sweep::run_alert_sweep_loop`) snapshots
    /// this when it starts and stops as soon as it changes: an app that signs
    /// out, switches account, or factory-resets already calls `clear_all` at
    /// that boundary — the surface's *Lifetime* rule requires it
    /// (`critical-alerts.md` § Mechanism) — so the signal is free and uniform on
    /// all seven apps.
    ///
    /// ⚠ **Therefore `clear_all` means "the identity went away", not "empty the
    /// list".** Do not reach for it to drop alerts for any other reason (a
    /// refresh, a user gesture — alerts are not dismissable anyway): a
    /// non-teardown call would silently stop every sweep loop watching this
    /// registry. Clear a specific condition with [`Self::clear`].
    pub fn teardown_epoch(&self) -> u64 {
        self.teardown_epoch.load(Ordering::Relaxed)
    }

    /// Mark a sweep pass as *begun* — called by
    /// `fauna_client_alert_sweep::run_session_start_sweep_at` before its first
    /// feeder runs. Sweep-only, like [`Self::post`]: nothing else may bump it,
    /// or the barrier below stops meaning what it says.
    pub fn note_sweep_started(&self) {
        self.sweep_started.fetch_add(1, Ordering::Relaxed);
    }

    /// Mark a sweep pass as *finished* — called by the same function after its
    /// last feeder has posted or cleared, which is the ordering the barrier
    /// rests on (see [`Self::sweep_passes_started`]).
    ///
    /// Precisely: at this point every feeder's decision is in the registry
    /// **and every observer's `on_changed` hand-off has returned OR FAULTED**
    /// (dispatch is synchronous — [`Self::notify`]). A hand-off that returned
    /// cleanly has scheduled its shell's repaint, though not necessarily
    /// painted it yet; a hand-off that faulted (native panic, or a JS throw on
    /// wasm) has scheduled nothing, and is counted in
    /// [`Self::observer_faults`] instead of silently passing for "returned".
    /// A pass with a zero fault delta is what makes an e2e read that crosses
    /// the barrier and then queries the app *through its own event loop*
    /// sound: the repaint wake is queued before the query arrives — a
    /// nonzero delta means that guarantee doesn't hold for whichever
    /// observer faulted.
    pub fn note_sweep_completed(&self) {
        self.sweep_completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Dispatch `on_changed` to every observer, synchronously, in the posting
    /// caller's own stack. Kept synchronous **deliberately**: the sweep's
    /// completion barrier ([`Self::note_sweep_completed`]) only means what it
    /// says if a pass's repaint hand-offs have all returned before the pass
    /// counts itself finished — a detached/queued dispatch would put a repaint
    /// in flight past the barrier, re-opening the false-pass the barrier
    /// exists to prevent. What makes synchronous dispatch safe is the observer
    /// contract on [`CriticalAlertsObserver::on_changed`]: the callback is a
    /// prompt scheduling hand-off, never the repaint itself.
    fn notify(&self) {
        // Snapshot under the lock, dispatch outside it: an observer may
        // re-enter the registry with reads (android and apple read `active()`
        // inline; the sweep's tests read the pass counters) and may even
        // `subscribe` — neither can deadlock against a lock this method no
        // longer holds during dispatch.
        let observers: Vec<_> = self.observers.lock().unwrap().clone();
        for obs in &observers {
            // A panicking observer — a foreign exception surfacing through
            // UniFFI included — must not take the posting feeder, and with it
            // the rest of a sweep pass, down; later observers still get their
            // repaint wake. (No-op under panic=abort, e.g. wasm — the wasm
            // shims carry their own `catch` on the JS boundary instead, and
            // count their own faults via [`Self::note_observer_fault`] since
            // this `catch_unwind` cannot see across that boundary.)
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| obs.on_changed())).is_err()
            {
                self.note_observer_fault();
            }
        }
    }

    /// Bump the observer-fault counter — called from [`Self::notify`]'s own
    /// `catch_unwind` `Err` arm (a native panic) and, on wasm, from a chunk's
    /// `wasm_glue::AlertsObserverShim` when its JS `catch` reports a throw
    /// (`wasm_glue.rs`'s `on_changed` cannot go through `notify`'s
    /// `catch_unwind` at all — panic=abort on that target means a real Rust
    /// panic there would take the whole module down, which is exactly why
    /// the JS throw is caught and swallowed before it ever becomes one).
    /// `pub(crate)`, not `pub`: only this crate's own dispatch paths bump it.
    /// See [`Self::observer_faults`].
    pub(crate) fn note_observer_fault(&self) {
        self.observer_faults.fetch_add(1, Ordering::Relaxed);
    }

    /// The active alerts as painted/registered lines, one string per
    /// `critical-alert[N]` row: each alert's [`LocalizedText`] lines resolved
    /// through `lookup` and joined, so every client says the same thing
    /// (`critical-alerts.md` § Mechanism). Empty ⇒ the banner is absent —
    /// callers never test [`Self::active`] a second way. Rust-only, same
    /// reason as [`Self::post`]: `F`/`S` are generic parameters, which
    /// UniFFI's FFI ABI cannot express.
    pub fn active_lines<F, S>(&self, lookup: F) -> Vec<String>
    where
        F: Fn(&str) -> Option<S>,
        S: AsRef<str>,
    {
        self.active()
            .into_iter()
            .map(|row| {
                row.lines
                    .into_iter()
                    .map(|line| line.resolve(&lookup))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }
}

/// The app-shell-facing synchronous surface — subscribe for repaints, read
/// the active list, drop everything at the identity-teardown boundary.
/// `post`/`clear` above stay Rust-only: feeders call them directly, no app
/// shell does.
#[cfg_attr(feature = "uniffi", uniffi::export)]
impl CriticalAlerts {
    /// Register a repaint observer (the shell; test counters). `Arc`, not
    /// `Box`: a foreign (Kotlin/Swift/C#) observer crosses UniFFI as a
    /// reference-counted handle, mirroring `FfiFeedManager::add_observer`.
    ///
    /// The observer's `on_changed` is called synchronously on whatever thread
    /// posts or clears — it must be a prompt, non-blocking scheduling hand-off
    /// and nothing more; see [`CriticalAlertsObserver`] for the full contract
    /// every one of the 7 apps' trampolines follows.
    pub fn subscribe(&self, observer: Arc<dyn CriticalAlertsObserver>) {
        self.observers.lock().unwrap().push(observer);
    }

    /// Drop every active alert — the **session** boundary, not a user gesture
    /// (alerts are never dismissable; `critical-alerts.md` § Mechanism gives
    /// them an in-memory per-app-session lifetime). A client calls this when it
    /// tears down authenticated state: sign-out, account switch, reset. Without
    /// it an alert keyed to the departing identity's DID outlives the identity —
    /// nothing would ever re-check that condition to clear it, so the banner
    /// would accuse an account the user no longer has, which is the crying-wolf
    /// failure the severity bar exists to prevent.
    ///
    /// Also the registry's **identity-teardown signal** — see
    /// [`Self::teardown_epoch`] for what reads it and why that makes this call
    /// teardown-only.
    pub fn clear_all(&self) {
        // Unconditional, unlike the notify below: a teardown that happened to
        // have no active alerts is still a teardown, and a background sweep
        // loop must stop on it just the same.
        self.teardown_epoch.fetch_add(1, Ordering::Relaxed);
        // The source readings go with their alerts: a later source update
        // must not resurrect a reading from the departed identity.
        self.sourced.lock().unwrap().clear();
        let changed = {
            let mut alerts = self.alerts.lock().unwrap();
            let had = !alerts.is_empty();
            alerts.clear();
            had
        };
        if changed {
            self.notify();
        }
    }

    /// How many critical-alert sweep passes have **begun** on this registry.
    ///
    /// With [`Self::sweep_passes_completed`] this is the sweep's causal barrier
    /// for *negative* asserts — "the condition I just planted raises no alarm"
    /// (`e2e-conventions.md` § convention 14, mechanism 2). A test reads
    /// `started` at the moment it plants the condition, then waits for
    /// `completed` to exceed that value; because a pass always bumps `started`
    /// before its first feeder and `completed` after its last, `completed > s₀`
    /// can only be true once a pass that began **after** the plant has finished
    /// — the thing a settle-sleep can only assume.
    ///
    /// ⚠ **Two counters rather than one, and the second is not redundant.** A
    /// lone "passes completed" count cannot support that conclusion, because
    /// several sweep loops can be in flight at once: an app's post-auth hook
    /// spawns a loop per session establishment, and re-establishing without a
    /// teardown (no `clear_all`, so no [`Self::teardown_epoch`] change) leaves
    /// the previous loop running — so a completion observed after the plant may
    /// belong to a pass that read the world *before* it. Comparing against
    /// `started` closes that by pigeonhole and needs no count of the loops.
    pub fn sweep_passes_started(&self) -> u64 {
        self.sweep_started.load(Ordering::Relaxed)
    }

    /// How many sweep passes have **finished** — every feeder posted or cleared.
    /// See [`Self::sweep_passes_started`] for the barrier the pair forms.
    pub fn sweep_passes_completed(&self) -> u64 {
        self.sweep_completed.load(Ordering::Relaxed)
    }

    /// How many `on_changed` hand-offs have **faulted** rather than returned
    /// — a native panic (caught by [`Self::notify`]'s `catch_unwind`) or, on
    /// wasm, a JS throw (caught by a chunk's own shim). Distinguishes a sweep
    /// pass whose containment silently swallowed a broken repaint hand-off
    /// from a genuinely clean one: [`Self::sweep_passes_completed`] counts
    /// both alike, so a test asserting "no banner appeared" after crossing
    /// the barrier should also assert this delta is zero for the pass it
    /// cares about, or it may be green for the wrong reason. See
    /// [`Self::note_sweep_completed`].
    pub fn observer_faults(&self) -> u64 {
        self.observer_faults.load(Ordering::Relaxed)
    }

    /// All active alerts, deterministic order. Empty ⇒ the `critical-alerts`
    /// banner is absent.
    pub fn active(&self) -> Vec<CriticalAlertRow> {
        self.alerts
            .lock()
            .unwrap()
            .iter()
            .map(|(key, lines)| CriticalAlertRow {
                key: key.clone(),
                lines: lines.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Counting(Arc<AtomicU64>);
    impl CriticalAlertsObserver for Counting {
        fn on_changed(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn line(key: &str) -> LocalizedText {
        LocalizedText::key(key)
    }

    #[test]
    fn post_clear_lifecycle_and_notifications() {
        let reg = CriticalAlerts::new();
        let count = Arc::new(AtomicU64::new(0));
        reg.subscribe(Arc::new(Counting(count.clone())));

        assert!(reg.active().is_empty());

        reg.post("atproto-custody:did:plc:abc", vec![line("a")]);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(reg.active().len(), 1);
        assert_eq!(reg.active()[0].key, "atproto-custody:did:plc:abc");

        // Idempotent re-post: no extra notification.
        reg.post("atproto-custody:did:plc:abc", vec![line("a")]);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // Changed lines replace and notify.
        reg.post("atproto-custody:did:plc:abc", vec![line("b")]);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(reg.active()[0].lines, vec![line("b")]);

        reg.clear("atproto-custody:did:plc:abc");
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert!(reg.active().is_empty());

        // Clearing an absent key is silent.
        reg.clear("atproto-custody:did:plc:abc");
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    /// A multi-source key stands while ANY source holds it: one source
    /// reporting "nothing" never clears a reading another source raised —
    /// the pending-replacement feeder's bound and linked reads
    /// (`critical-alerts.md` § Feeders).
    #[test]
    fn a_sourced_key_clears_only_when_its_last_source_does() {
        let reg = CriticalAlerts::new();
        let count = Arc::new(AtomicU64::new(0));
        reg.subscribe(Arc::new(Counting(count.clone())));

        reg.post_from("k", "linked", 200, vec![line("late")]);
        reg.post_from("k", "bound", 100, vec![line("soon")]);
        assert_eq!(reg.active().len(), 1);
        // The most urgent (lowest-rank) reading is the one shown.
        assert_eq!(reg.active()[0].lines, vec![line("soon")]);

        // One source clearing falls back to the other's reading.
        reg.clear_from("k", "bound");
        assert_eq!(reg.active()[0].lines, vec![line("late")]);
        assert_eq!(reg.sources("k"), vec!["linked".to_string()]);

        // Clearing a source that holds nothing is silent.
        let before = count.load(Ordering::SeqCst);
        reg.clear_from("k", "bound");
        assert_eq!(count.load(Ordering::SeqCst), before);

        reg.retain_sources("k", |s| s != "linked");
        assert!(reg.active().is_empty());
        assert!(reg.sources("k").is_empty());
    }

    /// The teardown drops the source readings with the alerts, so a later
    /// update from one source cannot resurrect another's departed reading.
    #[test]
    fn clear_all_drops_source_readings_too() {
        let reg = CriticalAlerts::new();
        reg.post_from("k", "a", 1, vec![line("a")]);
        reg.post_from("k", "b", 2, vec![line("b")]);
        reg.clear_all();
        reg.post_from("k", "b", 2, vec![line("b")]);
        reg.clear_from("k", "b");
        assert!(reg.active().is_empty());
    }

    #[test]
    fn clear_all_drops_every_alert_and_notifies_once() {
        let reg = CriticalAlerts::new();
        let count = Arc::new(AtomicU64::new(0));
        reg.subscribe(Arc::new(Counting(count.clone())));

        reg.post("atproto-custody:did:plc:a", vec![line("a")]);
        reg.post("atproto-custody:did:plc:b", vec![line("b")]);
        assert_eq!(count.load(Ordering::SeqCst), 2);

        // One repaint for the whole teardown, not one per key.
        reg.clear_all();
        assert!(reg.active().is_empty());
        assert_eq!(count.load(Ordering::SeqCst), 3);

        // Idempotent: a second teardown on an empty registry is silent, so a
        // sign-out path may call it unconditionally.
        reg.clear_all();
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    /// The mechanical half of the dispatch contract (`critical-alerts.md`
    /// § Mechanism): a panicking observer — the shape a foreign exception
    /// takes after crossing UniFFI — must neither unwind through the posting
    /// feeder (which would end a sweep pass between its feeders) nor starve
    /// the observers dispatched after it, and the fault must be COUNTED, not
    /// silently swallowed. Mutation-graded: removing `notify`'s
    /// `catch_unwind` fails this at the `post` call; removing the
    /// `note_observer_fault` bump inside it leaves `post` passing but
    /// `observer_faults` at zero.
    #[test]
    fn a_panicking_observer_neither_kills_the_poster_nor_starves_later_ones() {
        struct Panicking;
        impl CriticalAlertsObserver for Panicking {
            fn on_changed(&self) {
                panic!("observer bug (deliberate — this test pins containment)");
            }
        }

        let reg = CriticalAlerts::new();
        reg.subscribe(Arc::new(Panicking));
        let count = Arc::new(AtomicU64::new(0));
        // Subscribed AFTER the panicking one, so it only fires if dispatch
        // survives the panic and keeps walking the list.
        reg.subscribe(Arc::new(Counting(count.clone())));

        assert_eq!(reg.observer_faults(), 0, "no dispatch yet, no fault yet");
        reg.post("atproto-custody:did:plc:abc", vec![line("a")]);

        assert_eq!(reg.active().len(), 1, "the post itself must land");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "observers after the panicking one still get their repaint wake"
        );
        assert_eq!(
            reg.observer_faults(),
            1,
            "the panic must be counted, not silently discarded"
        );

        // A second dispatch on the same registry: the counter accumulates
        // (it is a lifetime total, not a per-pass flag), and a clean
        // dispatch afterwards must not touch it — the property a lone
        // "faulted at least once" boolean could not distinguish.
        reg.clear("atproto-custody:did:plc:abc");
        assert_eq!(
            reg.observer_faults(),
            2,
            "the panicking observer faults again on the clear's dispatch too"
        );
    }

    /// The property [`Self::observer_faults`] exists to give a test: a sweep
    /// pass with a panicking observer must be DISTINGUISHABLE from a clean
    /// one purely by reading the registry — the failure mode found in
    /// `note_sweep_completed`'s prose, which claimed every hand-off scheduled
    /// a repaint even though a panicking one had not. Mutation-graded: folding the fault into "returned"
    /// (dropping the distinct counter) makes the two `post`s below
    /// indistinguishable.
    #[test]
    fn a_faulting_pass_is_distinguishable_from_a_clean_one() {
        struct Panicking;
        impl CriticalAlertsObserver for Panicking {
            fn on_changed(&self) {
                panic!("observer bug (deliberate — this test pins containment)");
            }
        }

        let clean = CriticalAlerts::new();
        clean.subscribe(Arc::new(Counting(Arc::new(AtomicU64::new(0)))));
        clean.post("atproto-custody:did:plc:clean", vec![line("a")]);

        let faulting = CriticalAlerts::new();
        faulting.subscribe(Arc::new(Panicking));
        faulting.post("atproto-custody:did:plc:faulting", vec![line("a")]);

        assert_eq!(
            clean.observer_faults(),
            0,
            "the clean registry saw no fault"
        );
        assert_eq!(
            faulting.observer_faults(),
            1,
            "the faulting registry's hand-off must be visible in the count"
        );
        assert_ne!(
            clean.observer_faults(),
            faulting.observer_faults(),
            "a faulting pass and a clean pass must read differently"
        );
    }

    /// The other mechanical half: dispatch holds no registry lock, so an
    /// observer that re-enters with `subscribe` (or any read) cannot deadlock.
    /// The failure mode under a re-held lock is a hang, not an assert, so the
    /// post runs on its own thread against a generous deadline.
    #[test]
    fn subscribing_from_inside_on_changed_does_not_deadlock() {
        struct SubscribesOnce {
            reg: std::sync::Weak<CriticalAlerts>,
            count: Arc<AtomicU64>,
            done: std::sync::atomic::AtomicBool,
        }
        impl CriticalAlertsObserver for SubscribesOnce {
            fn on_changed(&self) {
                if !self.done.swap(true, Ordering::SeqCst)
                    && let Some(reg) = self.reg.upgrade()
                {
                    reg.subscribe(Arc::new(Counting(self.count.clone())));
                }
            }
        }

        let reg = Arc::new(CriticalAlerts::new());
        let count = Arc::new(AtomicU64::new(0));
        reg.subscribe(Arc::new(SubscribesOnce {
            reg: Arc::downgrade(&reg),
            count: count.clone(),
            done: std::sync::atomic::AtomicBool::new(false),
        }));

        let (tx, rx) = std::sync::mpsc::channel();
        let poster = Arc::clone(&reg);
        std::thread::spawn(move || {
            poster.post("atproto-custody:did:plc:abc", vec![line("a")]);
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(30)).expect(
            "post() deadlocked: dispatch must not hold the observers lock \
             while calling on_changed",
        );

        // The mid-dispatch subscriber is live for the NEXT notification.
        reg.post("atproto-custody:did:plc:abc", vec![line("b")]);
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "an observer subscribed from inside on_changed receives later notifications"
        );
    }

    #[test]
    fn active_order_is_deterministic() {
        let reg = CriticalAlerts::new();
        reg.post("z-feeder:1", vec![line("z")]);
        reg.post("a-feeder:1", vec![line("a")]);
        let keys: Vec<_> = reg.active().into_iter().map(|r| r.key).collect();
        assert_eq!(keys, vec!["a-feeder:1", "z-feeder:1"]);
    }

    #[test]
    fn no_alerts_resolve_to_no_lines() {
        let reg = CriticalAlerts::new();
        assert!(
            reg.active_lines(|_| None::<&str>).is_empty(),
            "an empty registry means the banner is absent, not an empty row"
        );
    }

    #[test]
    fn a_rows_lines_resolve_through_lookup_and_join_with_a_space() {
        let reg = CriticalAlerts::new();
        reg.post(
            "atproto-custody:did:plc:abc",
            vec![LocalizedText::key_arg(
                "critical_alerts.atproto_custody_mismatch",
                "handle",
                "alice@fauna.test",
            )],
        );
        let lookup = |k: &str| match k {
            "critical_alerts.atproto_custody_mismatch" => Some("Handle mismatch: {handle}"),
            _ => None,
        };
        let lines = reg.active_lines(lookup);
        assert_eq!(lines, vec!["Handle mismatch: alice@fauna.test"]);
    }

    #[test]
    fn active_lines_follows_the_same_deterministic_order_as_active() {
        let reg = CriticalAlerts::new();
        reg.post("z-feeder:1", vec![line("zulu")]);
        reg.post("a-feeder:1", vec![line("alpha")]);
        // Unresolved keys fall back to the key itself as their own template.
        assert_eq!(reg.active_lines(|_| None::<&str>), vec!["alpha", "zulu"]);
    }
}
