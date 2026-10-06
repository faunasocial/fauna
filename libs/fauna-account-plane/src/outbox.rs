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
//! the whole point (`devices.md` § Offline compose). That drainer is T6's
//! consumer, built with the MLS compose surface, not in this module.

use anyhow::{Context, Result, bail};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{IntentDrainer, IntentStatus, NewOutboxIntent};
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
    /// Inconclusive attempts recorded before a transport fault stopped the
    /// pass.
    pub retried: usize,
    /// Why, for everything that did not drain cleanly.
    pub errors: Vec<String>,
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
        if intent.drainer == IntentDrainer::Mls {
            report.held_for_mls += 1;
            continue;
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
                "unknown kind",
            )
            .await;
            continue;
        };
        let payload: Value = match fauna_protocol::decode_strict(&intent.payload) {
            Ok(v) => v,
            Err(e) => {
                let why = format!("payload does not decode: {e}");
                park(store, intent, &mut parked_scopes, &mut report, &why).await;
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
                park(store, intent, &mut parked_scopes, &mut report, &why).await;
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

/// Park one intent and its scope, recording why. A park is durable,
/// user-visible state — the opposite of a drop.
async fn park<B: StoreBackend>(
    store: &AccountStore<B>,
    intent: &fauna_account_store::types::OutboxIntent,
    parked_scopes: &mut std::collections::BTreeSet<String>,
    report: &mut DrainReport,
    why: &str,
) {
    if let Err(e) = store.outbox_mark_failed(&intent.intent_id).await {
        report.errors.push(format!("outbox park: {e:#}"));
    }
    parked_scopes.insert(intent.scope.clone());
    report.newly_parked += 1;
    report
        .errors
        .push(format!("outbox intent parked ({}): {why}", intent.kind));
}
