//! The offline outbox — enqueue and the generic drain (W4 (account-data-plane.md § Workstreams) phase 1).
//!
//! Charter: `account-data-plane.md` § The offline-mutation contract, the
//! phase-0 ruling: the outbox is **its own durable component, holding
//! `OfflineQueued` intents only** — an `OfflineSafe` mutation is a store
//! write whose journal row already is its durable replay record
//! (`publish_pending`), and it never comes through here. The store half
//! (rows, FIFO, park, completion-is-deletion) lives in
//! `fauna_account_store`; this module owns the *policy*: what may enqueue,
//! and how undrained intents leave the device.
//!
//! ## Delivery guarantee: exactly-once against a phase-3 nest
//!
//! A drain that crashes between the nest's accept and the ack deletion
//! replays the intent on the next pass — onto a fresh connection whose
//! per-connection `IdempotencyCache` starts empty. The envelope carries the
//! intent id as its idempotency key on every attempt, and the nest's
//! **durable** idempotency table (phase 3, landed 2026-08-13 —
//! `bins/fauna-nest/src/db/rpc_idempotency.rs`; charter § Nest-side
//! requirements item 3) answers the replay with the recorded first outcome,
//! exactly as the phase-1 posture predicted — no client-side change was
//! needed. Against an **older** nest (within-major skew) the posture
//! degrades to the original at-least-once, backed by the per-kind
//! natural-idempotency audit (`offline_class`), so callers should still not
//! imply exactly-once unconditionally.
//!
//! ## Failure policy
//!
//! - A **transport fault** (disconnect, timeout) records an attempt on the
//!   intent and stops the whole drain — the connection is gone; the next
//!   full pump pass retries. Pass cadence is the backoff.
//! - A **server rejection** parks the intent (`mark_failed`) and, with it,
//!   the rest of its scope's queue: FIFO is never reordered around a
//!   failure (`devices.md` § Offline compose owns that law for MLS; the
//!   generic drain applies it uniformly). Parked intents are user-visible
//!   state — never silently dropped — and stay durable until user action
//!   (the phase-4 surfaces).
//! - An intent whose kind this build cannot resolve, or whose payload does
//!   not decode, can never succeed: parked, loudly.
//!
//! ## What this drain refuses to touch
//!
//! [`IntentDrainer::Mls`] intents are listed but never sent here: they
//! drain only through the gated MLS send path, from a process hosting the
//! account's conversations engine — sealing at the then-current epoch is
//! the whole point (`devices.md` § Offline compose). That drain is
//! `AccountStoreHandle::drain_mls_outbox`, which an app hosting the
//! conversations engine runs with the engine's `MlsIntentSender`; the pump
//! never calls it.
//!
//! [`IntentDrainer::Identity`] intents are listed but never sent here either:
//! their payload is the inputs of an identity-bound request, which only a
//! process holding the identity seed composes and signs, at drain, as the
//! then-current identity (`account-offline-mutation.md` § The
//! offline-mutation contract → *Identity-bound intents compose at
//! drain*). That drain is `AccountStoreHandle::drain_identity_outbox`, run by
//! the app with its `IdentityIntentSender`; the sync agent, whose bundle
//! carries no seed, never runs it.

use anyhow::{Context, Result, bail};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{IntentDrainer, IntentStatus, NewOutboxIntent};
use fauna_core::offline_intent::park_code;
use fauna_protocol::offline_class::{OfflineClass, offline_class_keyed};
use fauna_protocol::{KeyedRpcRequester, RpcErrorClass, Value};

/// Enqueue a durable intent: mint its id (the idempotency key it will carry
/// on every drain attempt), enforce the phase-0 boundary, append. Returns
/// the minted id — the composer's receipt.
///
/// The append is durable before this returns; a mutation enqueued here
/// survives a process restart and cannot be lost by any store operation,
/// scope departure included (the store pins that).
pub async fn enqueue_intent<B: StoreBackend>(
    store: &AccountStore<B>,
    kind: &str,
    scope: &str,
    payload: Vec<u8>,
    drainer: IntentDrainer,
) -> Result<[u8; 16]> {
    let Some((_, class)) = offline_class_keyed(kind) else {
        bail!("outbox: {kind:?} is not a registered kind");
    };
    if class != OfflineClass::OfflineQueued {
        bail!(
            "outbox: {kind} is {class:?}, not OfflineQueued — an OfflineSafe \
             write's journal row is its replay record (publish-by-replay), \
             and Read/OnlineOnly kinds must not queue (the phase-0 ruling)"
        );
    }
    let mut intent_id = [0u8; 16];
    getrandom::fill(&mut intent_id).map_err(|e| anyhow::anyhow!("outbox: mint intent id: {e}"))?;
    let inserted = store
        .outbox_append(&NewOutboxIntent {
            intent_id,
            kind: kind.to_string(),
            scope: scope.to_string(),
            payload,
            drainer,
        })
        .await
        .context("outbox append")?;
    debug_assert!(inserted, "a freshly minted intent id collided");
    Ok(intent_id)
}

/// Append a retired identity's intent to `store` VERBATIM and, when `parked`
/// names the predecessor row's stored reason, park it with that reason — the
/// outbox carry's write (`crate::outbox_carry`; `succession-aftermath.md`
/// § Re-key scope, the device-local outbox row). `false` = the intent id is
/// already here (a re-run after a crash between this append and the
/// predecessor's delete): nothing is written, and the row is left as it
/// stands — perhaps retried by the user since, so it is never parked again.
/// No `OfflineQueued` check: the predecessor's enqueue door made it.
pub async fn carry_intent<B: StoreBackend>(
    store: &AccountStore<B>,
    intent: &NewOutboxIntent,
    parked: Option<&str>,
) -> Result<bool> {
    let inserted = store
        .outbox_append(intent)
        .await
        .context("outbox carry append")?;
    if inserted && let Some(code) = parked {
        store
            .outbox_mark_failed(&intent.intent_id, code)
            .await
            .context("outbox carry park")?;
    }
    Ok(inserted)
}

/// What one drain pass did. Mirrors the pump-report idiom: counts, plus the
/// errors that explain every parked or retried intent.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainReport {
    /// Intents acked and deleted (the nest accepted the replay).
    pub drained: usize,
    /// Intents parked this pass (rejection / unresolvable kind / undecodable
    /// payload).
    pub newly_parked: usize,
    /// Intents whose scope was already parked (or became parked earlier in
    /// this pass) — skipped, still durable, still pending.
    pub held_behind_park: usize,
    /// [`IntentDrainer::Mls`] intents — not this leg's to send.
    pub held_for_mls: usize,
    /// [`IntentDrainer::Identity`] intents — not this leg's to send.
    pub held_for_identity: usize,
    /// Inconclusive attempts recorded before a transport fault stopped the
    /// pass.
    pub retried: usize,
    /// Why, for everything that did not drain cleanly.
    pub errors: Vec<String>,
}

/// The environment variable naming the e2e outbox drain hold's file
/// ([`outbox_drain_held`]).
pub const OUTBOX_DRAIN_HOLD_FILE_ENV: &str = "FAUNA_E2E_OUTBOX_DRAIN_HOLD_FILE";

/// The e2e outbox drain hold: `true` while the file
/// [`OUTBOX_DRAIN_HOLD_FILE_ENV`] names exists. While it holds, [`drain_outbox`]
/// sends nothing and every intent stays queued, durable and listed. A test
/// needs this because a queued intent otherwise drains in the first pass that
/// reaches the nest. The succession outbox carry is the case: the identity
/// ceremony needs the nest up, so without a hold a like queued as the
/// predecessor would drain as the predecessor before the ceremony could run,
/// and nothing would be left to carry (`succession-aftermath.md` § Re-key
/// scope, the device-local outbox row;
/// `test_identity_succession_outbox_carry.py`). It is a file, not an
/// in-process flag, because on a desktop the process that pumps is the sync
/// agent, not the app the test drives. The harness sets the variable for each
/// launch (`drivers/tui.py`), pointing at a file that does not exist until a
/// test holds the drain. The app inherits it, and so does the agent it spawns.
/// Deleting the file releases the hold, and the next pump pass drains. It
/// holds both drains: the pump's generic leg here, and the app's MLS and
/// identity legs in `AccountOutbox::drain_app_once`, whose held pass ends the
/// app's drain loop, so releasing an app-only intent is deleting the file and
/// then ringing the app drain (the e2e agent's `APP_OUTBOX_DRAIN_NOW`). The
/// carry itself runs as usual. The outer gate is the compile gate and the inner one
/// is the variable (`fauna_account_plane`'s `FAUNA_E2E_STATED_ROW` shape;
/// convention 15): a release build without `e2e-agent` never reads the
/// variable.
///
/// **The in-process hold beside the file.** A web tab has no environment and
/// no filesystem, and an android app hosts its account runtime in its own
/// process, which inherits nothing from the harness; there the pump and the
/// app drain run in the one process the test drives, so the hold is a flag in
/// it, set and cleared through the e2e agent's `OUTBOX_DRAIN_HOLD` command
/// ([`set_outbox_drain_hold`]). Either one holds.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub(crate) fn outbox_drain_held() -> bool {
    held_now(
        OUTBOX_DRAIN_HELD_IN_PROCESS.load(std::sync::atomic::Ordering::SeqCst),
        std::env::var_os(OUTBOX_DRAIN_HOLD_FILE_ENV),
    )
}

/// The in-process half of [`outbox_drain_held`]; the file is the other.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
static OUTBOX_DRAIN_HELD_IN_PROCESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Set or clear the in-process e2e outbox drain hold
/// ([`outbox_drain_held`]): the seam the e2e agent's `OUTBOX_DRAIN_HOLD`
/// command reaches on an app whose pump runs in the process the test drives
/// (web, the UniFFI apps). Clearing it releases the hold, and the next pump
/// pass drains; an app-only intent then also needs the app drain rung
/// (`APP_OUTBOX_DRAIN_NOW`). Compiled out of release artifacts (convention
/// 15).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn set_outbox_drain_hold(held: bool) {
    OUTBOX_DRAIN_HELD_IN_PROCESS.store(held, std::sync::atomic::Ordering::SeqCst);
    tracing::info!(held, "e2e outbox drain hold set in process");
}

/// [`outbox_drain_held`]'s pure combiner: the in-process flag, or the named
/// file existing.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn held_now(in_process: bool, path: Option<std::ffi::OsString>) -> bool {
    in_process || hold_file_present(path)
}

#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
pub(crate) fn outbox_drain_held() -> bool {
    false
}

/// [`outbox_drain_held`]'s pure half: the named file, when one is named,
/// exists.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn hold_file_present(path: Option<std::ffi::OsString>) -> bool {
    path.is_some_and(|path| !path.is_empty() && std::path::Path::new(&path).exists())
}

/// Drain every drainable intent, per-scope FIFO. Called by the account
/// runtime's full pump passes (reconnect, ticker, `reconcile_now`) right
/// after `publish_pending` — the two "push our writes out" legs.
pub async fn drain_outbox<B, R>(store: &AccountStore<B>, rpc: &R) -> DrainReport
where
    B: StoreBackend,
    R: KeyedRpcRequester,
    R::Error: RpcErrorClass,
{
    let mut report = DrainReport::default();
    if outbox_drain_held() {
        tracing::info!("outbox drain held by the e2e hold file; nothing sent this pass");
        return report;
    }
    let undrained = match store.outbox_undrained().await {
        Ok(rows) => rows,
        Err(e) => {
            report.errors.push(format!("outbox list: {e:#}"));
            return report;
        }
    };

    // A scope with a parked (failed) intent holds everything behind it:
    // FIFO is never reordered around a failure. Seed from the rows, then
    // grow as this pass parks more.
    let mut parked_scopes: std::collections::BTreeSet<String> = undrained
        .iter()
        .filter(|i| i.status == IntentStatus::Failed)
        .map(|i| i.scope.clone())
        .collect();

    // Rows arrive (scope, channel_seq) ascending — scope-FIFO by iteration.
    for intent in &undrained {
        if intent.status == IntentStatus::Failed {
            continue; // the parked row itself; waits for user action
        }
        if parked_scopes.contains(&intent.scope) {
            report.held_behind_park += 1;
            continue;
        }
        match intent.drainer {
            IntentDrainer::Rpc => {}
            IntentDrainer::Mls => {
                report.held_for_mls += 1;
                continue;
            }
            IntentDrainer::Identity => {
                report.held_for_identity += 1;
                continue;
            }
        }

        // Recover the canonical &'static str for the wire seam from the same
        // classification lookup the enqueue door used. A kind this build
        // cannot resolve (an app downgrade reading a newer store) can never
        // drain here — park it loudly rather than spin on it forever.
        let Some((kind, _)) = offline_class_keyed(&intent.kind) else {
            park(
                store,
                intent,
                &mut parked_scopes,
                &mut report,
                park_code::UNKNOWN_KIND,
                "unknown kind",
            )
            .await;
            continue;
        };
        let payload: Value = match fauna_protocol::decode_strict(&intent.payload) {
            Ok(v) => v,
            Err(e) => {
                let why = format!("payload does not decode: {e}");
                park(
                    store,
                    intent,
                    &mut parked_scopes,
                    &mut report,
                    park_code::UNDECODABLE,
                    &why,
                )
                .await;
                continue;
            }
        };

        match rpc
            .request_keyed::<Value, Value>(kind, intent.intent_id, payload)
            .await
        {
            Ok(_reply) => match store.outbox_ack(&intent.intent_id).await {
                Ok(_) => report.drained += 1,
                Err(e) => report.errors.push(format!("outbox ack ({kind}): {e:#}")),
            },
            Err(e) if e.is_rejection() => {
                let why = format!("nest rejected: {e}");
                // The nest's own error code is the stored reason; a
                // rejection without one gets the drain's generic code.
                let code = e
                    .as_rpc_error()
                    .map_or(park_code::REJECTED, |r| r.code.as_str());
                park(store, intent, &mut parked_scopes, &mut report, code, &why).await;
            }
            Err(e) => {
                // Transport fault — the connection is gone; nothing after
                // this can send either. Record and stop; the next full pass
                // retries from here.
                if let Err(rec) = store.outbox_record_attempt(&intent.intent_id).await {
                    report
                        .errors
                        .push(format!("outbox record attempt: {rec:#}"));
                }
                report.retried += 1;
                report
                    .errors
                    .push(format!("outbox drain stopped ({kind}): {e}"));
                break;
            }
        }
    }
    report
}

/// Park one intent and its scope, recording why: `code` is the stable
/// reason stored on the row (the unsent-changes surface renders it), `why`
/// the diagnostic for the report. A park is durable, user-visible state —
/// the opposite of a drop.
async fn park<B: StoreBackend>(
    store: &AccountStore<B>,
    intent: &fauna_account_store::types::OutboxIntent,
    parked_scopes: &mut std::collections::BTreeSet<String>,
    report: &mut DrainReport,
    code: &str,
    why: &str,
) {
    if let Err(e) = store.outbox_mark_failed(&intent.intent_id, code).await {
        report.errors.push(format!("outbox park: {e:#}"));
    }
    parked_scopes.insert(intent.scope.clone());
    report.newly_parked += 1;
    report
        .errors
        .push(format!("outbox intent parked ({}): {why}", intent.kind));
}

#[cfg(test)]
mod drain_hold_tests {
    use super::{held_now, hold_file_present};

    #[test]
    fn the_in_process_hold_holds_with_no_file_and_either_one_holds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("outbox-drain-hold");
        assert!(!held_now(false, None), "neither: not held");
        assert!(
            held_now(true, None),
            "the in-process flag alone holds (web, android)"
        );
        assert!(
            !held_now(false, Some(file.clone().into_os_string())),
            "a named file not yet made, and no flag: not held"
        );
        std::fs::write(&file, b"").unwrap();
        assert!(
            held_now(false, Some(file.clone().into_os_string())),
            "the file alone holds (tui, linux)"
        );
        assert!(held_now(true, Some(file.into_os_string())), "both: held");
    }

    #[test]
    fn the_drain_hold_holds_only_while_its_named_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("outbox-drain-hold");
        assert!(!hold_file_present(None), "no variable: never held");
        assert!(
            !hold_file_present(Some("".into())),
            "an empty variable: never held"
        );
        assert!(
            !hold_file_present(Some(file.clone().into_os_string())),
            "a named file that does not exist yet: the launch's inert default"
        );
        std::fs::write(&file, b"").unwrap();
        assert!(
            hold_file_present(Some(file.clone().into_os_string())),
            "the test holds"
        );
        std::fs::remove_file(&file).unwrap();
        assert!(
            !hold_file_present(Some(file.into_os_string())),
            "deleting releases"
        );
    }
}
