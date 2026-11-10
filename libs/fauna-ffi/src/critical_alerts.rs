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

/// The post-auth split's process-wide record (`fauna_client_alert_sweep::LoopClaim`):
/// one per native process, like the registry, so every shell that calls
/// [`start_critical_alert_sweep`] decides its arm by the same rule.
static LOOP_CLAIM: fauna_client_alert_sweep::LoopClaim<FfiNestClient> =
    fauna_client_alert_sweep::LoopClaim::new();

/// **The post-auth hook's one critical-alert call** (`critical-alerts.md`
/// § Mechanism → *Who runs the detector* / *How often the detector runs*): it
/// decides the hook's arm of the split and runs it on `nest`, the session's own
/// connection (transport.md: one WebSocket per actor).
///
/// - A first or identity-changing sign-in starts the re-sweep LOOP, its first
///   pass at once.
/// - The same identity on the client its live loop already rides gets ONE pass
///   ([`fauna_client_alert_sweep::run_one_shot_sweep`]) — a second loop would
///   only stack.
/// - The same identity on a client its loop does not ride — a shell that tore
///   the old client down and built `nest` (windows on every re-establish), or a
///   loop that has ended — starts a loop on `nest` that opens on the custody
///   door ([`fauna_client_alert_sweep::run_alert_sweep_loop_reestablished`]): a
///   loop rides one client and ends with it, so the one-shot there would leave
///   the identity unswept.
///
/// The decision is `fauna_client_alert_sweep::LoopClaim`'s, keyed on the actor
/// at `nest_url` (the nest's literal URL — a re-point at another nest needs a
/// loop sweeping that nest) and on `nest`; it forgets an identity by the
/// registry's teardown epoch, which every identity teardown already bumps, so a
/// shell keeps no record and no forget of its own.
///
/// Resolves after the one pass, or when the loop ends (the identity torn down,
/// or `nest` disconnected) — so the caller needs a task scope that outlives
/// whatever triggered the sign-in, never a short-lived ViewModel scope.
/// Best-effort/fire-and-forget (the goal doc's "sign-in never fails or waits on
/// it"): the shared passes log their own outcomes.
///
/// windows, android and apple call this; the two lower-level calls below
/// ([`run_critical_alert_sweep`], [`run_critical_alert_sweep_loop`]) stay for a
/// shell that wants one arm alone.
///
/// # Errors
///
/// - [`FfiError::General`] if `secret` is not a 32-byte ed25519 secret.
#[fauna_uniffi_async::export]
pub async fn start_critical_alert_sweep(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    nest_url: String,
) -> Result<(), FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    let identity = format!("{}@{nest_url}", keypair.actor_id_hex());
    let alerts = critical_alerts_registry();
    let (arm, ticket) = LOOP_CLAIM.claim(&identity, &nest, &alerts);
    match arm {
        fauna_client_alert_sweep::SweepArm::OnePass => one_shot(&nest, &keypair).await,
        fauna_client_alert_sweep::SweepArm::Loop => {
            let _ticket = ticket;
            run_loop(&nest, &keypair, false).await;
        }
        fauna_client_alert_sweep::SweepArm::ReestablishedLoop => {
            let _ticket = ticket;
            run_loop(&nest, &keypair, true).await;
        }
    }
    Ok(())
}

/// Run the feeders that have no page of their own ONCE — the same-identity
/// re-establish arm of the post-auth split, for a shell that makes the split
/// itself; [`start_critical_alert_sweep`] makes it for the
/// shell instead. tui's `critical_alerts::spawn_post_auth_sweep`
/// (`session::establish`) is the reference this mirrors.
///
/// `secret` is the actor's 32-byte ed25519 secret, used only to derive the
/// actor id the sweep checks conditions against; feeder #1 reads the rotation
/// keyring through this process's account runtime
/// ([`crate::atproto_settings::atproto_identity_store`]). The pass is
/// [`fauna_client_alert_sweep::run_one_shot_sweep`]'s, which waits (bounded)
/// for that runtime first: a shell that rebuilds its runtime on a
/// re-establish would otherwise sweep against a door that is refusing.
/// Best-effort/fire-and-forget by design (the goal doc's "sign-in never fails
/// or waits on it"): call without blocking sign-in; the shared pass logs its
/// own outcome, the same posture
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
    one_shot(&nest, &keypair).await;
    Ok(())
}

/// [`run_critical_alert_sweep`]'s repeating twin — sweeps immediately, then
/// every `RE_SWEEP_INTERVAL_SECS` for as long as the identity lives
/// (`critical-alerts.md` § Mechanism → *Who runs the detector*, ratified
/// 2026-08-02) — for a shell that makes the split itself, at
/// a first or identity-changing sign-in.
///
/// Never returns under normal operation — it stops on the first wake after
/// `CriticalAlerts::clear_all` bumps the teardown epoch this loop watches, or
/// at once when `nest` is disconnected (a loop rides one client and ends with
/// it), so the caller needs no cancellation plumbing, but **does** need a task scope
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
    run_loop(&nest, &keypair, false).await;
    Ok(())
}

/// The one-shot arm's body.
async fn one_shot(nest: &FfiNestClient, keypair: &fauna_core::identity::ActorKeypair) {
    let actor_id = keypair.actor_id();
    let custody = crate::atproto_settings::atproto_identity_store();
    let alerts = critical_alerts_registry();
    fauna_client_alert_sweep::run_one_shot_sweep(nest.nest_arc(), &custody, &alerts, &actor_id)
        .await;
}

/// The loop arms' one body; `reestablished` picks the opening.
async fn run_loop(
    nest: &FfiNestClient,
    keypair: &fauna_core::identity::ActorKeypair,
    reestablished: bool,
) {
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
        let wake = move || {
            let notify = Arc::clone(&wake);
            async move { notify.notified().await }
        };
        if reestablished {
            fauna_client_alert_sweep::run_alert_sweep_loop_reestablished_wakeable(
                nest_arc, &custody, &alerts, &actor_id, wake,
            )
            .await;
        } else {
            fauna_client_alert_sweep::run_alert_sweep_loop_wakeable(
                nest_arc, &custody, &alerts, &actor_id, wake,
            )
            .await;
        }
    }
    #[cfg(not(feature = "test-helpers"))]
    if reestablished {
        fauna_client_alert_sweep::run_alert_sweep_loop_reestablished(
            nest_arc, &custody, &alerts, &actor_id,
        )
        .await;
    } else {
        fauna_client_alert_sweep::run_alert_sweep_loop(nest_arc, &custody, &alerts, &actor_id)
            .await;
    }
}

/// What ends the current identity's re-sweep WAIT early — the e2e seam behind
/// `fauna_e2e_agent::ALERT_SWEEP_WAKE` (tui's `SweepWake` is the reference),
/// with the registry teardown epoch its loop started under. Each loop
/// ([`start_critical_alert_sweep`]'s, or [`run_critical_alert_sweep_loop`])
/// replaces it, so a re-established loop takes the wake from the loop its
/// client replaced; and an identity change
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
