//! This core chunk's own [`fauna_client_alerts::CriticalAlerts`] registry —
//! web's **second** critical-alerts source, after `fauna-wasm-atproto-settings`
//! (feeder #1's home). `apps/fauna-web/src/lib/critical-alerts.ts`'s
//! aggregation layer exists precisely for this: each wasm chunk that hosts a
//! feeder is a separately compiled binary with its own linear memory, so a
//! feeder's `Arc<CriticalAlerts>` can only live inside the chunk that holds
//! it (`fauna-wasm-atproto-settings`'s own doc comment explains the split in
//! full) — this module is the twin registry for the feeders that live in
//! *this* chunk instead: the session-start sweep's `runCriticalAlertSweep`
//! (`src/rpc.rs`), which is why it lives in the core chunk rather than a
//! page-specific one (`critical-alerts.md` § Mechanism → *Who runs the
//! detector*: loaded at session start, not on a settings page).
//!
//! Same exported names as the atproto-settings chunk
//! (`subscribeCriticalAlerts`/`criticalAlertsActive`/`clearAllCriticalAlerts`)
//! — safe, since each wasm module is its own JS namespace; the aggregation
//! layer wires both chunks' triples into one shared store the shell renders.

use std::sync::Arc;

use fauna_client_alerts::CriticalAlerts;
use fauna_client_alerts::wasm_glue::{AlertsRegistryCell, JsCriticalAlertsObserver};
use wasm_bindgen::prelude::*;

static ALERTS_REGISTRY: AlertsRegistryCell = AlertsRegistryCell::new();

thread_local! {
    /// The identity the running re-sweep LOOP serves, and the registry's
    /// teardown epoch it started under — so a same-identity re-establish runs
    /// one pass instead of stacking a second loop (see [`loop_is_live_for`]).
    static LIVE_LOOP: std::cell::Cell<Option<(fauna_core::identity::ActorId, u64)>> =
        const { std::cell::Cell::new(None) };
}

/// Whether a re-sweep loop is already running for `actor_id`: one was started
/// for it and no identity teardown (`clear_all`, which bumps the epoch the loop
/// stops on) has happened since. The SPA re-fires `runCriticalAlertSweep` on
/// every session establishment, a same-actor re-establish included, and the
/// loop the first one started is still running then — the one-shot pass is the
/// re-check that establishment owes, as tui's and linux's converge arms run it
/// (`critical-alerts.md` § Implementation status today).
pub(crate) fn loop_is_live_for(actor_id: &fauna_core::identity::ActorId) -> bool {
    let epoch = alerts_registry().teardown_epoch();
    LIVE_LOOP.with(|live| live.get()) == Some((*actor_id, epoch))
}

/// Record that a loop is starting for `actor_id` under the current epoch.
pub(crate) fn mark_loop_live(actor_id: &fauna_core::identity::ActorId) {
    let epoch = alerts_registry().teardown_epoch();
    LIVE_LOOP.with(|live| live.set(Some((*actor_id, epoch))));
}

#[cfg(feature = "test-helpers")]
thread_local! {
    /// What ends the current identity's re-sweep WAIT early — the e2e seam behind
    /// `fauna_e2e_agent::ALERT_SWEEP_WAKE` (tui's `SweepWake` is the reference).
    /// Minted fresh for each loop [`crate::rpc`] starts, replacing the last, so a
    /// departed identity's lingering loop holds a handle nothing fires any more.
    /// `test-helpers` only: a production bundle's loop runs on its clock alone.
    static SWEEP_WAKE: std::cell::RefCell<Option<std::rc::Rc<tokio::sync::Notify>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "test-helpers")]
pub(crate) fn mint_sweep_wake() -> std::rc::Rc<tokio::sync::Notify> {
    let notify = std::rc::Rc::new(tokio::sync::Notify::new());
    SWEEP_WAKE.with_borrow_mut(|w| *w = Some(std::rc::Rc::clone(&notify)));
    notify
}

/// End the current identity's re-sweep wait now. `false` when no loop is live
/// for any identity (pre-auth, or torn down since) — the caller refuses loudly
/// (convention 11). A wake landing mid-pass is kept and ends the next wait.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = alertSweepWakeForTest)]
pub fn alert_sweep_wake_for_test() -> bool {
    let live = LIVE_LOOP.with(|live| live.get());
    let epoch = alerts_registry().teardown_epoch();
    if !live.is_some_and(|(_, started)| started == epoch) {
        return false;
    }
    SWEEP_WAKE.with_borrow(|w| match w {
        Some(notify) => {
            notify.notify_one();
            true
        }
        None => false,
    })
}

/// The registry this chunk's feeders post to; constructed once per loaded
/// module instance (an app session, on web).
pub(crate) fn alerts_registry() -> Arc<CriticalAlerts> {
    ALERTS_REGISTRY.get()
}

/// Repaint hook: fires on every post/clear against this chunk's registry.
/// `$lib/critical-alerts.ts` calls this once the chunk loads.
#[wasm_bindgen(js_name = subscribeCriticalAlerts)]
pub fn subscribe_critical_alerts(observer: JsCriticalAlertsObserver) {
    fauna_client_alerts::wasm_glue::subscribe(&ALERTS_REGISTRY, observer);
}

/// The active alerts as a JSON array of `{ key, lines: [{ key, args }] }` —
/// same shape as the atproto-settings chunk's own export, parsed by the
/// caller and each line resolved through `resolveLocalized`.
#[wasm_bindgen(js_name = criticalAlertsActive)]
pub fn critical_alerts_active() -> String {
    fauna_client_alerts::wasm_glue::active_json(&ALERTS_REGISTRY)
}

/// Drop every active alert — the identity-teardown boundary (sign-out,
/// account switch, nest-untrust, factory reset; `critical-alerts.md`
/// § Mechanism → *Lifetime*). Safe to call even with no alert ever posted.
#[wasm_bindgen(js_name = clearAllCriticalAlerts)]
pub fn clear_all_critical_alerts() {
    fauna_client_alerts::wasm_glue::clear_all(&ALERTS_REGISTRY);
}

/// The sweep's pass counters as `[started, completed]` — convention 14's causal
/// barrier for the sweep's negative asserts
/// (`fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY` owns the cross-app contract;
/// `CriticalAlerts::sweep_passes_started` owns the reasoning).
///
/// ⚠ **This chunk's registry is the one that counts, and that is not
/// interchangeable with the identically-named export next door.** The
/// session-start sweep runs from *this* core chunk (`runCriticalAlertSweep` in
/// `src/rpc.rs`), so its passes are bumped here; `fauna-wasm-atproto-settings`
/// hosts feeder #1's settings-page path against its own separate registry,
/// whose counters would read `[0, 0]` forever — reading them instead would make
/// every wait on this barrier time out, and every negative assert behind it
/// unreachable. Unlike the `active()` triple, these counters are therefore
/// deliberately NOT aggregated across chunks by `$lib/critical-alerts.ts`.
///
/// A pair rather than one number because both halves are load-bearing; the
/// contract doc explains why a bare completion count cannot support the
/// conclusion callers draw.
#[wasm_bindgen(js_name = criticalAlertSweepPasses)]
pub fn critical_alert_sweep_passes() -> Vec<u64> {
    let alerts = alerts_registry();
    vec![
        alerts.sweep_passes_started(),
        alerts.sweep_passes_completed(),
    ]
}

/// How many `on_changed` hand-offs against *this chunk's* registry have
/// faulted rather than returned (`CriticalAlerts::observer_faults` owns the
/// reasoning) — a separate export, not folded into
/// [`critical_alert_sweep_passes`]'s pair: the pass counters are a stable
/// two-element contract several callers already destructure positionally
/// (`fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY`), and this crate's own JS
/// observer trampoline never throws in production, so widening that tuple
/// for a value only a test would read is not worth the shape change. Not
/// aggregated across chunks, for the same reason
/// [`critical_alert_sweep_passes`] is not.
#[wasm_bindgen(js_name = criticalAlertObserverFaults)]
pub fn critical_alert_observer_faults() -> u64 {
    alerts_registry().observer_faults()
}

/// Test-only: point *this chunk's* directory-backed feeders (the session-start
/// sweep, `runCriticalAlertSweep` in `src/rpc.rs`) at a fake PLC directory —
/// this chunk's own copy of the wasm twin of native's
/// `FAUNA_ATPROTO_PLC_DIRECTORY_URL` env var. `fauna-wasm-atproto-settings`
/// exports the SAME-NAMED hook for feeder #1's settings-page path, but each
/// wasm chunk is a separately compiled binary with its own copy of
/// `fauna_client_atproto::genesis_verify`'s `thread_local` override (this
/// module's own doc comment explains the split for the alerts registry;
/// the directory override has the identical shape) — setting one chunk's
/// override left the OTHER chunk's `plc_directory_base_url()` pointed at the
/// real `https://plc.directory`, so a sweep-driven feeder run from THIS chunk
/// 404s and reports "unreadable, inconclusive" instead of reading the test's
/// fake directory. `apps/fauna-web/src/lib/e2e-automation.ts`'s single
/// `window.__fauna_enableFakePlcDirectoryForTest` hook now calls both chunks'
/// exports so callers need no change. Gated on this crate's `test-helpers`
/// feature (testing.md § convention 15), so it does NOT ship in production
/// wasm. Found via `test_alert_sweep_directory_feeders_e2e.py --app
/// web`.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = enableFakePlcDirectoryForTest)]
pub fn enable_fake_plc_directory_for_test(url: String) {
    fauna_client_alert_sweep::enable_fake_plc_directory_for_test(url);
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::wasm_bindgen_test;

    // `run_in_browser`, not the wasm-bindgen-test default of Node — this
    // project's toolchain has no Node.js (same reason `tests/subscription.rs`
    // needs it). This is the only `wasm_bindgen_test_configure!` call in this
    // crate's `--lib` test binary (the three `tests/*.rs` integration files
    // each compile as their own separate binary and carry their own call).
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    /// A `JsCriticalAlertsObserver` whose `onChanged` throws — the shape a
    /// misbehaving JS observer takes. Exercises
    /// `AlertsObserverShim::on_changed`'s `catch` arm
    /// (`fauna_client_alerts::wasm_glue`, `wasm_glue.rs:36-51`), the only
    /// place a faulted hand-off on THIS target is observable at all: wasm's
    /// panic=abort means a real Rust panic here would take the whole module
    /// down, so the JS throw is caught and swallowed on the JS side of the
    /// boundary before it ever becomes one — `CriticalAlerts::notify`'s
    /// native `catch_unwind` arm never runs on wasm32.
    fn throwing_observer() -> JsCriticalAlertsObserver {
        let obj = js_sys::Object::new();
        let on_changed = js_sys::Function::new_no_args("throw new Error('deliberate test throw')");
        js_sys::Reflect::set(&obj, &"onChanged".into(), &on_changed)
            .expect("Reflect::set on a fresh plain object cannot fail");
        obj.unchecked_into()
    }

    /// Pins the gap: before this test, deleting `wasm_glue.rs`'s
    /// `note_observer_fault()` bump reddened nothing on this target — zero
    /// `wasm_bindgen_test`s exercised the critical-alerts wasm path at all
    /// (there are `wasm_bindgen_test`s elsewhere in this crate, just none on
    /// this path). Asserts the before/after DELTA, not a bare `1`:
    /// `ALERTS_REGISTRY` is a per-binary static shared by every test in this
    /// wasm test binary, so an earlier test may already have bumped it.
    #[wasm_bindgen_test]
    fn a_throwing_js_observer_bumps_this_chunks_fault_counter() {
        let before = critical_alert_observer_faults();

        subscribe_critical_alerts(throwing_observer());
        alerts_registry().post(
            "critical-alerts-test:wasm-observer-fault",
            vec![fauna_core::localized::LocalizedText::key(
                "critical-alerts-test",
            )],
        );

        assert_eq!(
            critical_alert_observer_faults() - before,
            1,
            "a throwing onChanged must be counted via note_observer_fault, \
             not silently swallowed by the JS `catch` boundary",
        );
    }
}
