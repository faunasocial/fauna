//! UniFFI access to the cross-page critical-alerts registry
//! (`fauna_client_alerts::CriticalAlerts`) — the native-client (Windows /
//! Apple / Android) twin of `fauna-wasm-atproto-settings`'s wasm-bindgen
//! exports for the same type.
//!
//! One native process = one registry: unlike web, where each lazy-loaded
//! wasm chunk is a separately compiled binary with its own linear memory (so
//! a feeder's `Arc` can only live inside the chunk that hosts it — see that
//! crate's own doc comment), a native FFI process has a single address space,
//! so [`critical_alerts_registry`] alone gives every app the same "one shell,
//! every page" behavior web needs a whole aggregation layer for.
//!
//! `CriticalAlerts` itself is `#[uniffi::export]`ed directly from its home
//! crate (`fauna-client-alerts`, behind its own `uniffi` feature) — this
//! module is only the process-wide singleton accessor, plus the wiring point
//! [`crate::build_atproto_settings_machine`] uses to hand feeder #1 the
//! registry instead of `None`.

use std::sync::{Arc, OnceLock};

use fauna_client_alerts::CriticalAlerts;

use crate::{FfiError, FfiNestClient};

static REGISTRY: OnceLock<Arc<CriticalAlerts>> = OnceLock::new();

/// The process-wide critical-alerts registry. Call once per app session to
/// subscribe a repaint observer (mirrors web's `subscribeCriticalAlerts`);
/// every FFI-exposed machine that hosts a feeder posts to this SAME instance
/// (`critical_alerts_registry()` internal to [`crate::build_atproto_settings_machine`]
/// resolves to it too, so the client never wires the registry by hand).
#[uniffi::export]
pub fn critical_alerts_registry() -> Arc<CriticalAlerts> {
    REGISTRY
        .get_or_init(|| Arc::new(CriticalAlerts::new()))
        .clone()
}

/// Run the feeders that have no page of their own ONCE — the same-identity
/// re-establish arm of the post-auth split (`critical-alerts.md` § Mechanism →
/// *Who runs the detector*): a first or identity-changing sign-in starts
/// [`run_critical_alert_sweep_loop`] instead, and this pass runs only when that
/// identity's loop is already live. tui's
/// `critical_alerts::spawn_session_start_sweep` (`session::establish`) is the
/// reference this mirrors; windows and apple take this arm.
///
/// `secret` is the actor's 32-byte ed25519 secret, used only to derive the
/// actor id the sweep checks conditions against; feeder #1 reads the rotation
/// keyring through this process's account runtime
/// ([`crate::atproto_settings::atproto_identity_store`]). Best-effort/fire-and-forget by design (the goal doc's "sign-in
/// never fails or waits on it"): call without blocking sign-in and log the
/// outcome, the same posture
/// [`crate::deployment_seed::self_heal_deployment_seed_custody`] establishes
/// for this FFI surface. A bad secret is the only error
/// path — unreachable in practice, since launch has already validated it to
/// reach Online by the time a client's post-auth hook fires.
///
/// # Errors
///
/// - [`FfiError::General`] if `secret` is not a 32-byte ed25519 secret.
#[fauna_uniffi_async::export]
pub async fn run_critical_alert_sweep(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
) -> Result<(), FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    let actor_id = keypair.actor_id();
    let nest_arc = nest.nest_arc();
    let custody = crate::atproto_settings::atproto_identity_store();
    let alerts = critical_alerts_registry();
    let report =
        fauna_client_alert_sweep::run_session_start_sweep(nest_arc, &custody, &alerts, &actor_id)
            .await;
    // Branch on failures, NOT `is_complete()`: since the skipped bucket landed
    // (2026-08-02) a healthy deployment with no ATProto identity is legitimately
    // "not complete", and warning about that every session start would train the
    // reader to ignore the line. Skips are still always printed.
    if report.failures.is_empty() {
        tracing::debug!(
            checked = ?report.checked,
            skipped = ?report.skipped,
            "session-start critical-alert sweep: every reachable feeder ran"
        );
    } else {
        // Not user-visible: a standing alert (if any) is left alone and the
        // next session start retries.
        tracing::warn!(
            checked = ?report.checked,
            failures = ?report.failures,
            skipped = ?report.skipped,
            "session-start critical-alert sweep: some feeders were unreachable"
        );
    }
    Ok(())
}

/// [`run_critical_alert_sweep`]'s repeating twin — sweeps immediately, then
/// every `RE_SWEEP_INTERVAL_SECS` for as long as the identity lives
/// (`critical-alerts.md` § Mechanism → *Who runs the detector*, ratified
/// 2026-08-02). A **sibling export, not an in-place change**: the loop is what
/// an app's post-auth hook starts for a first or identity-changing sign-in, and
/// [`run_critical_alert_sweep`] stays the one-shot for a same-identity
/// re-establish, when the loop is already running. android, windows, macOS and
/// iOS start the loop at their post-auth hook, and windows, macOS and iOS take
/// the one-shot arm on a same-identity re-establish (the split tui and linux
/// make in Rust).
///
/// Never returns under normal operation — it stops on the first wake after
/// `CriticalAlerts::clear_all` bumps the teardown epoch this loop watches, so
/// the caller needs no cancellation plumbing, but **does** need a task scope
/// that outlives whatever triggered the sign-in (never a short-lived
/// ViewModel scope that dies on its own lifecycle, or the loop dies with it).
/// Best-effort/fire-and-forget by the same posture as
/// [`run_critical_alert_sweep`]; logging happens inside the shared loop
/// itself.
///
/// # Errors
///
/// - [`FfiError::General`] if `secret` is not a 32-byte ed25519 secret.
#[fauna_uniffi_async::export]
pub async fn run_critical_alert_sweep_loop(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
) -> Result<(), FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    let actor_id = keypair.actor_id();
    let nest_arc = nest.nest_arc();
    let custody = crate::atproto_settings::atproto_identity_store();
    let alerts = critical_alerts_registry();
    // The test flavors run the SAME loop with its wait raceable by
    // [`critical_alert_sweep_wake_for_test`] (convention 14), minting a fresh
    // wake per loop; the shipped flavors have no wake and run the clock alone.
    #[cfg(feature = "test-helpers")]
    {
        let wake = mint_sweep_wake(alerts.teardown_epoch());
        fauna_client_alert_sweep::run_alert_sweep_loop_wakeable(
            nest_arc,
            &custody,
            &alerts,
            &actor_id,
            move || {
                let notify = Arc::clone(&wake);
                async move { notify.notified().await }
            },
        )
        .await;
    }
    #[cfg(not(feature = "test-helpers"))]
    fauna_client_alert_sweep::run_alert_sweep_loop(nest_arc, &custody, &alerts, &actor_id).await;
    Ok(())
}

/// What ends the current identity's re-sweep WAIT early — the e2e seam behind
/// `fauna_e2e_agent::ALERT_SWEEP_WAKE` (tui's `SweepWake` is the reference),
/// with the registry teardown epoch its loop started under. Each
/// [`run_critical_alert_sweep_loop`] replaces it, and an identity change
/// (`CriticalAlerts::clear_all`, which bumps the epoch that loop stops on)
/// retires it, so a departed identity's lingering loop is never the one woken.
#[cfg(feature = "test-helpers")]
static SWEEP_WAKE: std::sync::Mutex<Option<(Arc<tokio::sync::Notify>, u64)>> =
    std::sync::Mutex::new(None);

#[cfg(feature = "test-helpers")]
fn mint_sweep_wake(epoch: u64) -> Arc<tokio::sync::Notify> {
    let notify = Arc::new(tokio::sync::Notify::new());
    *SWEEP_WAKE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((Arc::clone(&notify), epoch));
    notify
}

/// End the current identity's re-sweep wait now, so the loop's own body sweeps
/// again — the app's `alert_sweep_wake` arm calls this, and its barrier is the
/// sweep's pass counters, never this return. `false` when no loop is running
/// for the current identity (none started, or an identity change since), which
/// the app refuses loudly (convention 11). A wake landing mid-pass is kept and
/// ends the next wait.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn critical_alert_sweep_wake_for_test() -> bool {
    let epoch = critical_alerts_registry().teardown_epoch();
    match SWEEP_WAKE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        Some((notify, started)) if *started == epoch => {
            notify.notify_one();
            true
        }
        _ => false,
    }
}
