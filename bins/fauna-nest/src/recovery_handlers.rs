//! The RecoveryKey registration-chain handlers — identity-succession slice 2
//! (`docs/goal/behavior/identity-succession.md` § The RecoveryKey).
//!
//! It also wires the **seed-escrow** plane (§ Seed escrow): the opaque blob the
//! identity seed rests in, and the pre-identity, RecoveryKey-challenge-gated
//! fetch that reopens it after total device loss. The put is USER-class; the
//! `challenge`/`fetch` pair is pre-identity by necessity — a user who lost every
//! device has no session to authenticate with, which is the entire scenario.
//! The `status` read is the plane's other USER-class kind, and exists because
//! the two audiences do not overlap: the signed-in device that can *repair* a
//! missing blob is the one party the RecoveryKey gate keeps from *seeing* it.
//!
//! - [`register_recovery_handlers`] wires the `fauna.recovery.*` kinds, of
//!   which the two registration-chain ones set the pattern:
//!   - **`fauna.recovery.registration.submit`** — USER class. The owner
//!     registers (or replaces) its own RecoveryKey. The account is taken from
//!     the authenticated connection, never from the wire, and a record whose
//!     `actor_id` disagrees is refused: this kind can only ever write into the
//!     caller's own chain.
//!   - **`fauna.recovery.registration.chain`** — **pre-identity** (gated by
//!     `pre_identity_allowlist` + the dispatcher gate in
//!     `routes::dispatch_request`) and rate-limited. A peer verifying a
//!     succession statement holds no account here
//!     (`identity-succession.md:56`), so the chain must answer anonymously; the
//!     binding it returns is public by construction — the same coupled head
//!     rides the actor's signed `Profile` as `recovery_head`
//!     (`identity-succession.md:34`).
//!
//! **The nest never authorizes, only enforces and distributes**
//! (`identity-succession.md:104`). `submit` accepts a record solely on
//! `SignedRecoveryKeyRegistration::verify` — signatures by keys the nest does
//! not hold — and the replacement arm is gated on the *prior* RecoveryKey's
//! co-signature, which is precisely what a seed thief cannot produce. There is
//! no nest-side path that mints or overrides a registration.
//!
//! The submitted bytes are stored **verbatim** and replayed verbatim by
//! `chain`, so the serve path never re-encodes a record the nest did not author
//! (`transport.md` § SignedEnvelope and embed-as-bytes).

use std::time::Duration;

use fauna_core::encoding::canonical_decode;
use fauna_core::identity::ActorId;
use fauna_core::recovery::{
    ChainHead, EscrowChallenge, RECOVERY_REPLACE_GRACE_SECS, ReplacementVeto,
    SignedIdentitySuccession, SignedRecoveryKeyRegistration,
};
use fauna_protocol::RpcError;
use fauna_protocol::decode_strict as decode;
use fauna_protocol::recovery::{
    EscrowChallengeReply, EscrowChallengeRequest, EscrowFetchReply, EscrowFetchRequest,
    EscrowPutReply, EscrowPutRequest, EscrowStatusReply, EscrowStatusRequest, OwedNest,
    RegistrationChainReply, RegistrationChainRequest, RegistrationSubmitReply,
    RegistrationSubmitRequest, ReplacementChallengeReply, ReplacementChallengeRequest,
    ReplacementPendingInfo, ReplacementRequestReply, ReplacementRequestRequest,
    ReplacementStatusReply, ReplacementStatusRequest, ReplacementVetoReply, ReplacementVetoRequest,
    SUCCESSION_OWED_SETTLE_KIND, SUCCESSION_STATUS_KIND, SuccessionLookupReply,
    SuccessionLookupRequest, SuccessionOwedSettleReply, SuccessionOwedSettleRequest,
    SuccessionStatusReply, SuccessionStatusRequest, SuccessionSubmitReply, SuccessionSubmitRequest,
};
use serde_bytes::ByteBuf;

use crate::bridge_routing_handlers::{encode_reply, internal, malformed, require_class};
use crate::db::recovery_escrow::RECOVERY_ESCROW_MAX_LEN;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// An actor id is 32 bytes; the chain lookup refuses any other length rather
/// than querying with a malformed key.
const ACTOR_ID_LEN: usize = 32;

fn registration_submit_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.recovery.registration.submit").await?;
            let req: RegistrationSubmitRequest = decode(&payload).map_err(malformed)?;

            let signed: SignedRecoveryKeyRegistration =
                canonical_decode(req.registration.as_ref()).map_err(malformed)?;

            // The account is the authenticated connection's actor. Without this
            // check a user could append to *another* identity's chain — the
            // record's own signatures would still verify, since a first
            // registration is self-signed by whatever seed minted it.
            if signed.registration.actor_id.0 != actor_id {
                return Err(malformed(
                    "registration actor_id does not match the authenticated actor",
                ));
            }

            let head = state
                .db
                .recovery_registration_head(&actor_id[..])
                .await
                .map_err(internal)?;

            // A first registration verifies with no prior; a replacement must
            // carry a co-signature by the prior RecoveryKey and advance the
            // chain. This is the seed-thief gate: the thief holds the seed but
            // not the registered RecoveryKey, so cannot produce
            // `prior_recovery_sig`.
            //
            // The pubkey and the seq travel as one `ChainHead` built from this
            // single store read, so no call site can present one without the
            // other.
            let prior: Option<ChainHead> = match &head {
                Some(h) => Some(ChainHead::new(
                    h.recovery_pubkey.as_slice().try_into().map_err(|_| {
                        internal("stored recovery pubkey is not 32 bytes".to_string())
                    })?,
                    h.seq,
                )),
                None => None,
            };
            signed
                .verify(prior.as_ref())
                .map_err(|e| malformed(format!("registration verification failed: {e}")))?;

            let seq = signed.registration.seq;
            state
                .db
                .append_recovery_registration(
                    &actor_id[..],
                    seq,
                    &signed.registration.recovery_pubkey,
                    req.registration.as_ref(),
                )
                .await
                .map_err(internal)?;

            tracing::info!(
                target: "recovery",
                seq,
                replacement = head.is_some(),
                "recovery key registered"
            );

            // A RecoveryKey-authorized registration landing IS the "override"
            // arm of veto/override (`identity-succession.md:37`): any pending
            // seed-initiated replacement is cancelled — its `seq` no longer
            // advances the head, so the sweep would drop it anyway; doing it
            // here makes the contest immediate and the notification prompt.
            if state
                .db
                .delete_pending_replacement(&actor_id[..])
                .await
                .map_err(internal)?
            {
                tracing::info!(
                    target: "recovery",
                    "pending seed-initiated replacement overridden by an authorized registration"
                );
                notify_detached(
                    &state,
                    actor_id,
                    crate::security_notify::SecurityEvent::RecoveryReplacementCancelled {
                        cancelled_by: "a RecoveryKey-authorized registration (override)"
                            .to_string(),
                    },
                );
            }

            encode_reply(&RegistrationSubmitReply {
                seq,
                extra: Default::default(),
            })
        })
    })
}

fn registration_chain_handler() -> RpcHandler {
    Box::new(|state, _actor_id, payload| {
        Box::pin(async move {
            let req: RegistrationChainRequest = decode(&payload).map_err(malformed)?;
            if req.actor_id.len() != ACTOR_ID_LEN {
                return Err(malformed(format!(
                    "actor_id must be {ACTOR_ID_LEN} bytes, got {}",
                    req.actor_id.len()
                )));
            }

            let rows = state
                .db
                .list_recovery_registrations(req.actor_id.as_ref())
                .await
                .map_err(internal)?;

            // An identity that registered no RecoveryKey gets an empty chain,
            // not an error — "no succession capability" is an honest, expected
            // answer (`identity-succession.md:46`), and erroring would also
            // turn this into a sharper account-existence oracle than the
            // handle-directory kinds already are.
            encode_reply(&RegistrationChainReply {
                registrations: rows.into_iter().map(|r| ByteBuf::from(r.record)).collect(),
                extra: Default::default(),
            })
        })
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Seed escrow (`identity-succession.md` § Seed escrow)
// ─────────────────────────────────────────────────────────────────────────────

fn invalid_nonce() -> RpcError {
    RpcError::new(
        "fauna.recovery.invalid_nonce",
        "error.recovery.invalid_nonce",
    )
}

fn signature_failed() -> RpcError {
    crate::rpc_errors::bare_signature_failed_ns("recovery")
}

fn escrow_put_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.recovery.escrow.put").await?;
            let req: EscrowPutRequest = decode(&payload).map_err(malformed)?;

            // Caller-error shape checks live here so the client gets an
            // actionable refusal; the store re-checks them as its own
            // storage-layer guard, where a trip would mean a nest bug (hence
            // `internal` below, not a second `malformed`).
            if req.blob.is_empty() {
                return Err(malformed("escrow blob must not be empty"));
            }
            if req.blob.len() > RECOVERY_ESCROW_MAX_LEN {
                return Err(malformed(format!(
                    "escrow blob must be at most {RECOVERY_ESCROW_MAX_LEN} bytes, got {}",
                    req.blob.len()
                )));
            }

            // The account is the authenticated connection's actor and the blob
            // is never parsed: the nest holds no half of the key it is sealed
            // to, so there is nothing here it *could* validate about the
            // contents (`identity-succession.md:42`).
            let updated_at = state
                .db
                .put_recovery_escrow(&actor_id[..], req.blob.as_ref())
                .await
                .map_err(internal)?;

            tracing::info!(target: "recovery", bytes = req.blob.len(), "seed escrow stored");

            encode_reply(&EscrowPutReply {
                updated_at,
                extra: Default::default(),
            })
        })
    })
}

fn escrow_challenge_handler() -> RpcHandler {
    Box::new(|state, _actor_id, payload| {
        Box::pin(async move {
            let req: EscrowChallengeRequest = decode(&payload).map_err(malformed)?;
            let actor = actor_id_from(&req.actor_id)?;

            // Issued **unconditionally** — for an identity with no registration,
            // no escrow blob, or no account at all. Making the nonce conditional
            // would turn this kind into an "is this account recoverable?" probe
            // that costs no signature; as it stands the reply carries no
            // information about the account, and every refusal happens at
            // `fetch`, behind the RecoveryKey signature.
            let (nonce, expires_at) = state.auth.escrow_challenge_store.issue(actor).await;

            encode_reply(&EscrowChallengeReply {
                nonce: ByteBuf::from(nonce.to_vec()),
                expires_at,
                extra: Default::default(),
            })
        })
    })
}

fn escrow_fetch_handler() -> RpcHandler {
    Box::new(|state, _actor_id, payload| {
        Box::pin(async move {
            let req: EscrowFetchRequest = decode(&payload).map_err(malformed)?;
            let actor = actor_id_from(&req.actor_id)?;
            let nonce: [u8; 32] = crate::rpc_errors::require_bytes32("nonce", req.nonce.as_ref())
                .map_err(malformed)?;

            // Consume the nonce **before** verifying the signature, so one
            // issued challenge buys exactly one attempt. Verifying first would
            // let an attacker who somehow learned a live nonce grind unlimited
            // signature candidates against it; and since `consume` removes the
            // entry only on a full (unexpired, right-actor) match, a wrong
            // actor cannot burn someone else's outstanding challenge.
            if !state
                .auth
                .escrow_challenge_store
                .consume(&actor, &nonce)
                .await
            {
                return Err(invalid_nonce());
            }

            // The only authorizer is the RecoveryKey at the **head** of the
            // chain — which is what retires a superseded kit: the old key still
            // produces valid bytes, and they stop authorizing anything the
            // moment a replacement registration lands.
            //
            // An identity with no registration is refused here, before any
            // signature check. That is not a new oracle: whether an actor has a
            // registered RecoveryKey is already public through
            // `fauna.recovery.registration.chain`, deliberately
            // (`identity-succession.md:34`).
            let head = state
                .db
                .recovery_registration_head(&actor[..])
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    RpcError::new(
                        "fauna.recovery.not_registered",
                        "error.recovery.not_registered",
                    )
                })?;
            let recovery_pubkey: [u8; 32] = head
                .recovery_pubkey
                .as_slice()
                .try_into()
                .map_err(|_| internal("stored recovery pubkey is not 32 bytes".to_string()))?;

            EscrowChallenge::new(ActorId(actor), nonce)
                .verify(&recovery_pubkey, req.signature.as_ref())
                .map_err(|_| signature_failed())?;

            // Supersession consult, uniform with the auth ceremonies: a
            // succeeded identity's escrow row died in the succession
            // transaction, but the retired kit — whose chain is untouched, so
            // it still verifies above — gets the same routing signal every
            // other ceremony gives it: the successor to import, not a bare
            // `no_escrow`. This is also the backstop that makes any
            // pre-lifecycle orphaned row unreachable. Supersession is public
            // through `succession.lookup`, so this reveals nothing new; the
            // consult still sits behind the signature so every account-shaped
            // refusal on this kind stays there.
            if let Some(row) = state
                .db
                .succession_for(&actor[..])
                .await
                .map_err(internal)?
            {
                let new_actor: [u8; 32] = row.new_actor_id.as_slice().try_into().map_err(|_| {
                    internal("stored successor actor id is not 32 bytes".to_string())
                })?;
                return Err(RpcError::superseded(&new_actor));
            }

            // Only a proven RecoveryKey holder reaches this line, so "you have
            // no escrow blob" is an honest answer to its owner rather than a
            // probe — the caller has already demonstrated it is them.
            let row = state
                .db
                .get_recovery_escrow(&actor[..])
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    RpcError::new("fauna.recovery.no_escrow", "error.recovery.no_escrow")
                })?;

            tracing::info!(target: "recovery", "seed escrow served to a recovery-key holder");

            encode_reply(&EscrowFetchReply {
                blob: ByteBuf::from(row.blob),
                extra: Default::default(),
            })
        })
    })
}

/// Parse a wire `actor_id`, refusing any length but 32 rather than querying the
/// store with a malformed key.
fn actor_id_from(bytes: &ByteBuf) -> Result<[u8; 32], RpcError> {
    bytes.as_ref().try_into().map_err(|_| {
        malformed(format!(
            "actor_id must be {ACTOR_ID_LEN} bytes, got {}",
            bytes.len()
        ))
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Seed-initiated replacement — the pending window
// (`identity-succession.md:37`: 30 days, loudly notified, vetoable instantly)
// ─────────────────────────────────────────────────────────────────────────────

/// Fan a landed succession statement out to every peer holding residue about
/// the superseded identity (`identity-succession.md:81`).
///
/// The peer **dials** are detached and best-effort: the ceremony's guarantee
/// is that the *home* nest has re-pointed the account, which the transaction
/// already did. Blocking the owner's recovery on a peer's availability would
/// make an unreachable third party able to slow down — or, with enough peers,
/// effectively stall — the remedy for a theft in progress.
///
/// The **targets** are not read here. They are `targets`, read by the handler
/// through [`succession_push_targets_for_reply`] before it replies, and this
/// function cannot be called without them.
///
/// Only the statement rides; the receiver verifies by pulling from the
/// anchored home nest (see `FedSuccessionPushRequest`).
pub(crate) fn push_succession_detached(
    state: &std::sync::Arc<crate::routes::AppState>,
    targets: Vec<String>,
    statement: Vec<u8>,
) {
    if targets.is_empty() {
        return;
    }
    let scope = state.clone();
    let state = state.clone();
    scope.spawn_scoped(async move {
        let req = crate::federation_handlers::FedSuccessionPushRequest { statement };
        for peer_url in targets {
            match crate::federation_pool::originate_succession_push(
                &state.federation_pool,
                &state,
                &peer_url,
                &req,
            )
            .await
            {
                Ok(reply) => tracing::info!(
                    target: "recovery",
                    peer = %peer_url,
                    recorded = reply.recorded,
                    "succession pushed to peer"
                ),
                Err(e) => tracing::warn!(
                    target: "recovery",
                    peer = %peer_url,
                    "succession push failed: {e}"
                ),
            }
        }
    });
}

/// The peers a landed succession is pushed to, read under the **retired** id
/// by the ceremony's handler, after its transaction committed and before its
/// reply is sent.
///
/// The order is the point (`writer-signed-change-records.md` ruling
/// (8)(j)(2)). `succession_push_targets` finds its audience through the
/// retired id's roster rows, and the Welcome that seats the successor deletes
/// those rows (`CacheDb::register_successor_carrying_seat`). That Welcome is
/// sent by a device that has seen the ceremony succeed, so a read that
/// precedes the reply precedes every such Welcome; a read left to a detached
/// task would race them, and losing the race means no peer is told. One local
/// indexed read — nothing here waits on a peer.
///
/// A failed read is logged and yields no targets: the push is a hint, and the
/// hourly pull is the backstop for a peer it misses.
pub(crate) async fn succession_push_targets_for_reply(
    db: &crate::db::CacheDb,
    old: &[u8; 32],
) -> Vec<String> {
    db.succession_push_targets(old).await.unwrap_or_else(|e| {
        tracing::warn!(target: "recovery", "succession push targets: {e}");
        Vec::new()
    })
}

/// Fire a security notification without holding up the reply (the notifier's
/// push/email channels do network I/O).
fn notify_detached(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: [u8; 32],
    event: crate::security_notify::SecurityEvent,
) {
    let notifier = state.security_notifier.clone();
    let scope = state.clone();
    let state = state.clone();
    scope.spawn_scoped(async move {
        notifier.notify(&state, &actor, &event).await;
    });
}

/// Read the chain head as a [`ChainHead`], or `None` for an unregistered
/// identity — one store read, both halves together.
async fn chain_head(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: &[u8; 32],
) -> Result<Option<ChainHead>, RpcError> {
    let head = state
        .db
        .recovery_registration_head(&actor[..])
        .await
        .map_err(internal)?;
    match head {
        Some(h) => Ok(Some(ChainHead::new(
            h.recovery_pubkey
                .as_slice()
                .try_into()
                .map_err(|_| internal("stored recovery pubkey is not 32 bytes".to_string()))?,
            h.seq,
        ))),
        None => Ok(None),
    }
}

fn replacement_request_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.recovery.replacement.request").await?;
            let req: ReplacementRequestRequest = decode(&payload).map_err(malformed)?;

            let signed: SignedRecoveryKeyRegistration =
                canonical_decode(req.registration.as_ref()).map_err(malformed)?;

            // Same connection binding as `registration.submit`: this kind can
            // only ever park a replacement of the caller's OWN key.
            if signed.registration.actor_id.0 != actor_id {
                return Err(malformed(
                    "replacement actor_id does not match the authenticated actor",
                ));
            }

            // A seed-alone replacement needs something to replace — and a veto
            // authority to wait out. With no chain, a first registration is
            // `registration.submit`'s job, immediately and windowless.
            let head = chain_head(&state, &actor_id).await?.ok_or_else(|| {
                RpcError::new(
                    "fauna.recovery.not_registered",
                    "error.recovery.not_registered",
                )
            })?;

            // The pending arm's verification — NOT `verify`: no prior
            // co-signature exists (the prior key is what was lost), and
            // passing here lands nothing. The sweep re-runs this same check
            // against the then-current head at landing time.
            signed
                .verify_seed_alone(&head)
                .map_err(|e| malformed(format!("replacement verification failed: {e}")))?;

            let now = crate::db::now_epoch_secs();
            let requested_at = state
                .db
                .upsert_pending_replacement(
                    &actor_id[..],
                    req.registration.as_ref(),
                    &signed.registration.recovery_pubkey,
                    signed.registration.seq,
                    now,
                )
                .await
                .map_err(internal)?;
            let lands_at = requested_at + RECOVERY_REPLACE_GRACE_SECS as i64;

            // The one-shot alarm half of "loudly notified on every device";
            // the standing banner half is the apps' `status` read. Only on
            // a genuinely new window (a replayed request keeps the original
            // clock and must not re-alarm).
            if requested_at == now {
                notify_detached(
                    &state,
                    actor_id,
                    crate::security_notify::SecurityEvent::RecoveryReplacementPending {
                        new_key: hex::encode(signed.registration.recovery_pubkey),
                        lands_at,
                    },
                );
            }

            tracing::info!(
                target: "recovery",
                seq = signed.registration.seq,
                lands_at,
                "seed-initiated recovery-key replacement parked"
            );

            encode_reply(&ReplacementRequestReply {
                lands_at,
                extra: Default::default(),
            })
        })
    })
}

fn replacement_challenge_handler() -> RpcHandler {
    Box::new(|state, _actor_id, payload| {
        Box::pin(async move {
            let req: ReplacementChallengeRequest = decode(&payload).map_err(malformed)?;
            let actor = actor_id_from(&req.actor_id)?;

            // Unconditional for the same no-probe reason as the escrow
            // challenge: whether anything is pending is not disclosed to an
            // unauthenticated caller.
            let (nonce, expires_at) = state.auth.veto_challenge_store.issue(actor).await;

            encode_reply(&ReplacementChallengeReply {
                nonce: ByteBuf::from(nonce.to_vec()),
                expires_at,
                extra: Default::default(),
            })
        })
    })
}

fn replacement_veto_handler() -> RpcHandler {
    Box::new(|state, _actor_id, payload| {
        Box::pin(async move {
            let req: ReplacementVetoRequest = decode(&payload).map_err(malformed)?;
            let actor = actor_id_from(&req.actor_id)?;
            let nonce: [u8; 32] = crate::rpc_errors::require_bytes32("nonce", req.nonce.as_ref())
                .map_err(malformed)?;

            // Consume before verifying — one challenge, one attempt (the
            // escrow-fetch rule, same reasoning).
            if !state
                .auth
                .veto_challenge_store
                .consume(&actor, &nonce)
                .await
            {
                return Err(invalid_nonce());
            }

            // Only the CURRENT RecoveryKey vetoes — the head, never a
            // historical link. An unregistered identity cannot have a pending
            // replacement (request refuses without a chain), so refusing here
            // reveals nothing new.
            let head = chain_head(&state, &actor).await?.ok_or_else(|| {
                RpcError::new(
                    "fauna.recovery.not_registered",
                    "error.recovery.not_registered",
                )
            })?;

            ReplacementVeto::new(ActorId(actor), nonce)
                .verify(&head.recovery_pubkey, req.signature.as_ref())
                .map_err(|_| signature_failed())?;

            // A live veto contests whatever pends now; vetoing nothing is an
            // idempotent success, honestly reported to the proven key holder.
            let cancelled = state
                .db
                .delete_pending_replacement(&actor[..])
                .await
                .map_err(internal)?;

            if cancelled {
                tracing::info!(target: "recovery", "pending replacement vetoed by the recovery key");
                notify_detached(
                    &state,
                    actor,
                    crate::security_notify::SecurityEvent::RecoveryReplacementCancelled {
                        cancelled_by: "the current recovery key (veto)".to_string(),
                    },
                );
            }

            encode_reply(&ReplacementVetoReply {
                cancelled,
                extra: Default::default(),
            })
        })
    })
}

/// Does an escrow blob rest for the authenticated actor? USER class.
///
/// The one escrow kind that is **not** pre-identity, and the asymmetry is the
/// point: `escrow.{challenge,fetch}` serve the phrase holder with no session,
/// while this serves a signed-in device asking about its own account — the only
/// party that can *repair* a missing blob, and the one the RecoveryKey's
/// offline-only custody rule leaves unable to call `fetch` at all. Taking the
/// account from the connection (never a request field) is what keeps it from
/// being the actor-id→"holds escrow" oracle that an unauthenticated presence
/// answer would be.
///
/// Reads presence only. The blob never leaves this path.
fn escrow_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.recovery.escrow.status").await?;
            let _req: EscrowStatusRequest = decode(&payload).map_err(malformed)?;

            let row = state
                .db
                .get_recovery_escrow(&actor_id[..])
                .await
                .map_err(internal)?;

            encode_reply(&EscrowStatusReply {
                present: row.is_some(),
                updated_at: row.map(|r| r.updated_at),
                extra: Default::default(),
            })
        })
    })
}

fn replacement_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.recovery.replacement.status").await?;
            let _req: ReplacementStatusRequest = decode(&payload).map_err(malformed)?;

            let pending = state
                .db
                .get_pending_replacement(&actor_id[..])
                .await
                .map_err(internal)?
                .map(|row| ReplacementPendingInfo {
                    new_recovery_pubkey: ByteBuf::from(row.new_recovery_pubkey),
                    requested_at: row.requested_at,
                    lands_at: row.requested_at + RECOVERY_REPLACE_GRACE_SECS as i64,
                    extra: Default::default(),
                });

            encode_reply(&ReplacementStatusReply {
                pending,
                extra: Default::default(),
            })
        })
    })
}

/// Land every pending replacement whose window has elapsed uncontested —
/// called from `main.rs`'s periodic sweep, and from tests with an injected
/// `now` (convention 14: no wall-clock waits).
///
/// Landing re-runs `verify_seed_alone` against the **then-current** head: a
/// RecoveryKey-authorized registration landing during the window moved the
/// head past the pending `seq`, which invalidates the record here — the
/// "override" arm of veto/override. (The veto arm deletes the row directly;
/// slice 3's succession transaction will do the same.)
///
/// Returns `(landed, cancelled)`.
pub async fn land_due_replacements(
    state: &std::sync::Arc<crate::routes::AppState>,
    now: i64,
) -> anyhow::Result<(usize, usize)> {
    let due = state
        .db
        .list_due_pending_replacements(now, RECOVERY_REPLACE_GRACE_SECS)
        .await?;
    let mut landed = 0usize;
    let mut cancelled = 0usize;

    for row in due {
        let actor: [u8; 32] = match row.actor_id.as_slice().try_into() {
            Ok(a) => a,
            Err(_) => {
                tracing::error!(target: "recovery", "pending row with malformed actor_id; dropping");
                state.db.delete_pending_replacement(&row.actor_id).await?;
                continue;
            }
        };

        let verdict = async {
            let head = chain_head(state, &actor)
                .await
                .map_err(|e| anyhow::anyhow!("chain head: {}", e.code))?
                .ok_or_else(|| anyhow::anyhow!("chain vanished under a pending replacement"))?;
            let signed: SignedRecoveryKeyRegistration = canonical_decode(&row.record)
                .map_err(|e| anyhow::anyhow!("pending record no longer decodes: {e}"))?;
            signed
                .verify_seed_alone(&head)
                .map_err(|e| anyhow::anyhow!("superseded or invalid at landing: {e}"))?;
            Ok::<_, anyhow::Error>(signed)
        }
        .await;

        match verdict {
            Ok(signed) => {
                state
                    .db
                    .append_recovery_registration(
                        &row.actor_id,
                        signed.registration.seq,
                        &signed.registration.recovery_pubkey,
                        &row.record,
                    )
                    .await?;
                state.db.delete_pending_replacement(&row.actor_id).await?;
                landed += 1;
                tracing::info!(
                    target: "recovery",
                    seq = signed.registration.seq,
                    "uncontested seed-initiated replacement landed"
                );
                notify_detached(
                    state,
                    actor,
                    crate::security_notify::SecurityEvent::RecoveryReplacementLanded {
                        new_key: hex::encode(signed.registration.recovery_pubkey),
                    },
                );
            }
            Err(e) => {
                // The head moved during the window (the RecoveryKey-authorized
                // override) or the row is unreadable — either way the pending
                // record no longer represents a landable intent.
                state.db.delete_pending_replacement(&row.actor_id).await?;
                cancelled += 1;
                tracing::info!(target: "recovery", reason = %e, "pending replacement dropped at landing");
                notify_detached(
                    state,
                    actor,
                    crate::security_notify::SecurityEvent::RecoveryReplacementCancelled {
                        cancelled_by: "a RecoveryKey-authorized registration that landed during \
                                       the window"
                            .to_string(),
                    },
                );
            }
        }
    }
    Ok((landed, cancelled))
}

// ─────────────────────────────────────────────────────────────────────────────
// Succession (`identity-succession.md` § Enforcement on the home nest)
// ─────────────────────────────────────────────────────────────────────────────

fn succession_submit_handler() -> RpcHandler {
    Box::new(|state, _actor_id, payload| {
        Box::pin(async move {
            let req: SuccessionSubmitRequest = decode(&payload).map_err(malformed)?;

            let signed: SignedIdentitySuccession =
                canonical_decode(req.statement.as_ref()).map_err(malformed)?;
            let old = signed.statement.old_actor_id.0;
            let new = signed.statement.new_actor_id.0;

            // The account is named by the statement itself, not by a session —
            // this kind is pre-identity precisely because the owner may have
            // none (`identity-succession.md:66`). That is safe *only* because
            // the next few lines check the statement against the chain the
            // named identity itself registered: the statement cannot name an
            // account whose RecoveryKey the submitter does not hold.
            let head = chain_head(&state, &old).await?.ok_or_else(|| {
                // No registered RecoveryKey ⇒ no succession capability. The
                // design refuses a seed-only succession path outright
                // (`identity-succession.md:109`): between two seed holders
                // there is no winner, only DoS or a coin flip.
                RpcError::new(
                    "fauna.recovery.not_registered",
                    "error.recovery.not_registered",
                )
            })?;

            // The whole authorization, in one call: `recovery_sig` under the
            // key this identity registered, `new_sig` under the successor, and
            // `seq` advancing the chain head. `old_sig` is deliberately not
            // consulted — a thief can always produce it.
            //
            // Both halves of the head travel from ONE store read, so the
            // pubkey-without-seq state cannot occur.
            signed.verify(&head).map_err(|e| {
                tracing::warn!(target: "recovery", error = %e, "succession statement refused");
                signature_failed()
            })?;

            // Everything above is verification; everything below is the atomic
            // consequence (`identity-succession.md:66` — "then in one
            // transaction").
            let applied = state
                .db
                .record_succession(
                    &old[..],
                    &new[..],
                    req.statement.as_ref(),
                    signed.statement.seq,
                )
                .await
                .map_err(internal)?
                .map_err(|refusal| {
                    use crate::db::successions::SuccessionRefusal as R;
                    let code = match refusal {
                        R::OldNotRegistered => "fauna.recovery.not_registered",
                        R::AlreadySucceeded => "fauna.recovery.already_succeeded",
                        R::NewAlreadyRegistered => "fauna.recovery.successor_exists",
                        // Unreachable from this handler — `record_succession`
                        // refuses a *non*-local identity with `OldNotRegistered`
                        // and never inspects locality the other way. Mapped
                        // rather than `unreachable!()` so a future refactor that
                        // routes both writers through one entry point degrades
                        // to an honest wire error instead of a panic.
                        R::OldIsLocal => "fauna.recovery.old_is_local",
                        // Peer-leg only (`record_peer_succession`); the twin of
                        // `NewAlreadyRegistered`, so the same wire code if it
                        // ever arrives here.
                        R::NewAlreadySucceeded => "fauna.recovery.successor_exists",
                    };
                    RpcError::new(code, "error.recovery.succession_refused")
                        .with_details_text(refusal.to_string())
                })?;

            // Strip the old identity's live authority — BOTH halves
            // (`transport.md` § Connection lifecycle → *Revocation teardown*).
            // These live in memory, not in the database, so they cannot ride
            // the transaction; the sweep runs immediately after the commit.
            //
            // ⚠ This was `token_store.revoke_actor` alone until 2026-08-23, and
            // the argument for that being enough does not survive contact with
            // the transport: it reasoned that "a bearer that somehow outlived
            // this line still cannot re-authenticate, and the consult refuses it
            // on its next handshake" — but a WebSocket the thief ALREADY HOLDS
            // never has a next handshake. The nest validates a bearer exactly
            // once, at the upgrade (`routes.rs`, `handle_ws`), then bakes the
            // actor into `RpcConnection` for the connection's lifetime;
            // `dispatch_core` never re-reads the token store. Killing tokens
            // therefore ends only the NEXT connection. Nor does the per-RPC
            // authority gate cover it: `caller_class_for_actor` reads
            // `users.suspended` / `users.locked_until`, and this ceremony
            // deliberately KEEPS the old `users` row unsuspended and unlocked
            // (`db/successions.rs`, the row table), so it still resolves to
            // `Some(CallerClass::User)`.
            //
            // Every other authority-stripping path already paired the halves
            // (lockout, suspend, the eviction ladder, the pending-action
            // executor, the bridge doors). This one — the path whose entire
            // purpose is evicting a thief who holds the seed — was the lone
            // exception, so the thief kept full `User`-class dispatch on an open
            // socket after the ceremony that was supposed to undo them.
            // `revoke_actor_authority` does both halves plus the bridge-registry
            // purge; the `auth_core` supersession consult remains the backstop
            // for anything that re-authenticates later.
            // ⚠ The *sparing* form, and this is the only call site of it. The
            // unsparing one closed the socket this very request arrived on
            // before its Reply could be written, so the ceremony could never
            // tell its own client that it had committed: `succeed_with_held_kit`
            // took its `Unconfirmed` arm on every succession an app performed
            // while signed in (all seven do — the RecoveryClient wraps the
            // live NestClient), logged "succession submit failed after the
            // successor was minted — outcome unknown", and left the app
            // reconciling a ceremony that had in fact landed. The thief's OTHER
            // sockets are still torn down unsparingly, which is the whole point
            // of the teardown; only the answer already in flight is spared.
            state.revoke_actor_authority_sparing_caller(&old).await;

            // The transaction moved the admin role to the successor and burned
            // the old identity's pairings — both inputs of the dialer's
            // pairing-target table (`private-mode.md` § Pairing Flow): rebuild
            // it, as every other admin-roster change does.
            crate::nest_sync_worker::refresh_pairing_targets(&state).await;

            // The filesystem half of the ownership move: the transaction
            // re-pointed the actor-scoped segment *rows*, and the files live
            // under `<data_dir>/__<kind>/<actor_hex>/` — renamed here, right
            // after commit, because fs ops cannot join a SQL transaction. A
            // crash between the two is healed by the boot heal
            // (`succession_ownership::heal_at_boot`). A park
            // is impossible on this path (`NewAlreadyRegistered` means the
            // successor had no directories), but the helper logs one anyway.
            crate::succession_ownership::heal_segment_dirs(
                &[
                    &*state.mail_segments,
                    &*state.post_segments,
                    &*state.cal_segments,
                    &*state.card_segments,
                    // The placement journals move with the corpus they index —
                    // a successor whose mailboxes/collections stayed under the
                    // refused identity would come up looking empty.
                    &*state.mail_placement,
                    &*state.cal_placement,
                    &*state.card_placement,
                ],
                &old,
                &new,
            );

            // The other filesystem half: the transaction BURNED the retired
            // identity's `export_sessions` rows (`db::actor_tables`), and each
            // named a sealed whole-mailbox blob whose key is wrapped for the
            // identity that was just retired. A burn cannot unlink, so the
            // files are collected as orphans now rather than left resting
            // until the next boot, which runs the same reclaim as the backstop.
            crate::mail_export_blobs::reclaim_orphaned_export_blobs_best_effort(
                &state.db,
                &state.config.nest.db_path,
                "succession",
            )
            .await;

            tracing::info!(
                target: "recovery",
                seq = signed.statement.seq,
                handle_moved = applied.handle.is_some(),
                admin_moved = applied.was_admin,
                grants_revoked = applied.capability_grants_revoked,
                pending_replacement_cancelled = applied.cancelled_pending_replacement,
                corpus_rows_repointed = applied.corpus_rows_repointed,
                "identity succeeded"
            );
            notify_detached(
                &state,
                old,
                crate::security_notify::SecurityEvent::IdentitySucceeded {
                    new_actor_id: hex::encode(new),
                },
            );

            // Tell the peers holding residue about this identity
            // (`identity-succession.md:81` § Propagation → *Federation peers*).
            // The dials are detached and best-effort on purpose: the account is
            // already re-pointed and the old key already refused *here*, so
            // nothing about the owner's recovery may wait on a peer being
            // reachable. A peer this misses is the residual the doc declares,
            // and the pull leg is what eventually closes it. The targets are
            // read here, before the reply, because the successor's first
            // Welcome retires the roster rows the read depends on.
            let targets = succession_push_targets_for_reply(&state.db, &old).await;
            push_succession_detached(&state, targets, req.statement.as_ref().to_vec());

            encode_reply(&SuccessionSubmitReply {
                new_actor_id: ByteBuf::from(new.to_vec()),
                succeeded_at: applied.succeeded_at,
                extra: Default::default(),
            })
        })
    })
}

fn succession_lookup_handler() -> RpcHandler {
    Box::new(|state, _actor_id, payload| {
        Box::pin(async move {
            let req: SuccessionLookupRequest = decode(&payload).map_err(malformed)?;
            let actor = actor_id_from(&req.actor_id)?;

            let path = state
                .db
                .succession_path(&actor[..])
                .await
                .map_err(internal)?;

            // An identity that was never succeeded gets an empty list, not an
            // error — same non-oracle shape as `registration.chain`, and the
            // answer a peer needs in the overwhelmingly common case.
            encode_reply(&SuccessionLookupReply {
                statements: path
                    .into_iter()
                    .map(|row| ByteBuf::from(row.statement))
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.recovery.succession.status` — the caller's own commit stamp.
///
/// The **authenticated** counterpart to `succession_lookup_handler`, and the
/// separation is the point. `lookup` is pre-identity and answers for whatever
/// `req.actor_id` names, so a server-observed timestamp served there would be
/// an anonymous oracle for the second at which any account's
/// recovery-from-compromise committed (`SUCCESSION_STATUS_KIND`'s docs carry
/// the full argument). This one takes no actor at all: the class gate resolves
/// the caller and the row is looked up by that identity, so "actor-scoped" is
/// structural rather than a check to forget.
///
/// Not a succession's *successor* — including every ordinary account that never
/// ran the ceremony — gets `None`, never an error, exactly as `lookup` answers
/// an unsucceeded id with an empty list.
fn succession_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, SUCCESSION_STATUS_KIND).await?;
            let _req: SuccessionStatusRequest = decode(&payload).map_err(malformed)?;

            let succeeded_at = state
                .db
                .succession_committed_at(&actor_id[..])
                .await
                .map_err(internal)?;

            // The owed nests ride the same self-scoped read: the destinations
            // of the pairings each succession on the caller's predecessor path
            // burned, until the caller settles them
            // (`identity-succession.md` § Enforcement on the home nest →
            // *Every nest the identity is linked to*). Keyed on the caller
            // like the stamp, so a bystander and the retired identity both
            // read an empty list.
            let owed_nests = state
                .db
                .owed_nests_for_successor(&actor_id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|row| OwedNest {
                    old_actor_id: ByteBuf::from(row.old_actor_id),
                    nest_id: ByteBuf::from(row.nest_id),
                    nest_url: row.nest_url,
                    extra: Default::default(),
                })
                .collect();

            // The statement path that ends at the caller, oldest hop first:
            // what the link action submits at the nest it is about to link
            // before it signs in there (*The road*'s last sentence). The
            // statements are the public artifacts `lookup` serves anyway; this
            // only spares the caller knowing its own first predecessor.
            let predecessor_statements = state
                .db
                .succession_statements_into(&actor_id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(ByteBuf::from)
                .collect();

            encode_reply(&SuccessionStatusReply {
                succeeded_at,
                owed_nests,
                predecessor_statements,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.recovery.succession.owed_settle` — clear one of the caller's owed
/// nests.
///
/// The successor's device calls this once the statement has landed at the
/// nest, or that nest turned out to hold no account for the retired identity.
/// The nest takes its word: an entry is a reminder for the caller's own
/// devices, and clearing one early costs the caller a delivery, nobody else
/// anything. What it does not take on trust is **whose** entry it is — only
/// the identity the account now belongs to may clear an entry kept for one of
/// its predecessors, so neither a bystander nor the retired key (the one a
/// thief holds) can hide a nest the retired identity still signs in at.
///
/// Idempotent: an entry that is already gone, or never existed, is a success.
fn succession_owed_settle_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, SUCCESSION_OWED_SETTLE_KIND).await?;
            let req: SuccessionOwedSettleRequest = decode(&payload).map_err(malformed)?;
            let old = actor_id_from(&req.old_actor_id)?;

            let on_path = state
                .db
                .settle_owed_nest(&actor_id, &old, req.nest_id.as_ref())
                .await
                .map_err(internal)?;
            if !on_path {
                return Err(crate::rpc_errors::permission_denied_ns(
                    "recovery",
                    "the caller is not the successor of that identity",
                ));
            }

            encode_reply(&SuccessionOwedSettleReply::default())
        })
    })
}

/// Register the RecoveryKey registration-chain and seed-escrow kinds.
///
/// `submit` is `forbid_replay = false` @10 s — it is naturally idempotent
/// under replay: a re-delivered submit carries the same `seq`, which the store
/// refuses as non-advancing, so a retry can never fork a chain. `chain` is a
/// replay-safe pure read @5 s, matching the discovery reads it sits beside.
/// `escrow.put` is an upsert of the same bytes under replay, so also
/// `forbid_replay = false`; `escrow.challenge` merely mints another coexisting
/// nonce; `escrow.fetch` is the one kind here that forbids replay (its nonce is
/// single-use — see the `add` below).
///
/// `KindRegistry::register_recovery_kinds` carries the client-side metadata
/// twin, and the two are held in lockstep by the parity test in
/// `rpc_router.rs`. (Until 2026-07-31 no `register_*_kinds` method was wired
/// into any constructor, so clients silently fell back to the spec defaults for
/// every kind; `KindRegistry::full` is now the production constructor.)
pub fn register_recovery_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.recovery.registration.submit",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: registration_submit_handler(),
        },
    );
    b.add(
        "fauna.recovery.registration.chain",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: registration_chain_handler(),
        },
    );
    b.add(
        "fauna.recovery.escrow.put",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: escrow_put_handler(),
        },
    );
    b.add(
        "fauna.recovery.escrow.challenge",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: escrow_challenge_handler(),
        },
    );
    b.add(
        "fauna.recovery.escrow.fetch",
        RpcKindMeta {
            // Forbids replay: the nonce is single-use, so a blind auto-retry
            // over a reconnect cannot succeed — it would surface a confusing
            // `invalid_nonce` instead of the real transport failure. The
            // client must re-challenge.
            forbid_replay: true,
            default_deadline: Duration::from_secs(10),
            handler: escrow_fetch_handler(),
        },
    );
    b.add(
        "fauna.recovery.escrow.status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: escrow_status_handler(),
        },
    );
    // The seed-initiated replacement window. `request` is replay-safe because
    // the pending store keeps the ORIGINAL `requested_at` for an unchanged
    // record digest — a re-delivered request cannot extend the window (pinned
    // in `db::recovery_pending`). `veto` mirrors `escrow.fetch`'s single-use
    // nonce; `challenge` mints another coexisting nonce; `status` is a pure
    // read.
    b.add(
        "fauna.recovery.replacement.request",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: replacement_request_handler(),
        },
    );
    b.add(
        "fauna.recovery.replacement.challenge",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: replacement_challenge_handler(),
        },
    );
    b.add(
        "fauna.recovery.replacement.veto",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(10),
            handler: replacement_veto_handler(),
        },
    );
    b.add(
        "fauna.recovery.replacement.status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: replacement_status_handler(),
        },
    );
    // Succession (slice 3). `submit` is replay-safe *by outcome*: the second
    // delivery of the same statement finds `actor_successions` already holding
    // the old id and gets the typed `already_succeeded` refusal — it can never
    // apply twice. `lookup` is a pure read.
    b.add(
        "fauna.recovery.succession.submit",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: succession_submit_handler(),
        },
    );
    b.add(
        "fauna.recovery.succession.lookup",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: succession_lookup_handler(),
        },
    );
    // `status` is the authenticated half — a pure read of the caller's own row,
    // User-class in `bridge_method_allowlist` and deliberately absent from
    // `pre_identity_allowlist` (both pinned there).
    b.add(
        SUCCESSION_STATUS_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: succession_status_handler(),
        },
    );
    // `owed_settle` is its write half: User-class and absent from
    // `pre_identity_allowlist` for the same reason, and replay-safe because a
    // second settle of the same entry is a success.
    b.add(
        SUCCESSION_OWED_SETTLE_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: succession_owed_settle_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::recovery_registrations::RECOVERY_PUBKEY_LEN;

    #[test]
    fn a_recovery_pubkey_is_the_length_the_store_enforces() {
        // The handler hands `signed.registration.recovery_pubkey` (a
        // fixed-size `[u8; 32]`) straight to the store, whose runtime check
        // uses this constant. Pin that the two agree, so a future widening of
        // either is caught here rather than at a failed append.
        assert_eq!(RECOVERY_PUBKEY_LEN, 32);
        assert_eq!(ACTOR_ID_LEN, 32);
    }

    /// The property — "the submit reply serves the transaction's own
    /// stamp, never a fresh clock read" is NOT reliably behaviourally
    /// observable: both reads land in the same wall-clock second on any fast
    /// test run, so a behavioural assertion here would pass against the bug
    /// it exists to catch (proven empirically before this pin was written —
    /// `conformance_succession.rs`'s equality assert stayed green against the
    /// unfixed handler). Source-level guard instead, the same class as row
    /// 135's ordering guard and row 120/121's additive-column/class guards.
    #[test]
    fn the_submit_reply_never_reads_the_clock_directly() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/recovery_handlers.rs");
        let src = std::fs::read_to_string(&path).expect("read own source");

        let start = src
            .find("fn succession_submit_handler()")
            .expect("succession_submit_handler must exist");
        // Bounded to the next top-level `fn`, not end-of-file — the same trap
        // the guard fell into (a haystack left open to EOF can match
        // itself in this very test module).
        let after_start = &src[start..];
        let next_fn = after_start[1..]
            .find("\nfn ")
            .map(|i| i + 1)
            .expect("another top-level fn must follow");
        let body = &after_start[..next_fn];

        assert!(
            !body.contains("now_epoch_secs"),
            "succession_submit_handler must not read the clock directly for \
             `SuccessionSubmitReply::succeeded_at` — thread \
             `applied.succeeded_at` (the transaction's own committed stamp) \
             instead, or the reply drifts from `actor_successions.succeeded_at` \
             across a wall-clock second boundary"
        );
        assert!(
            body.contains("applied.succeeded_at"),
            "succession_submit_handler must serve `applied.succeeded_at` — \
             the value `record_succession` committed — in the reply"
        );
    }

    /// The teardown this handler performs must be the **sparing** one.
    ///
    /// `conformance_revocation_teardown.rs`'s property-4 tests prove the
    /// mechanism over a real socket, but they drive a synthetic kind — so a
    /// future simplification of *this* handler back to the plain
    /// `revoke_actor_authority` would leave them green while restoring the
    /// defect in full. That is the same un-adopted-mechanism shape the census in
    /// that file exists for, one layer down: the mechanism works, the path that
    /// needs it stopped calling it.
    ///
    /// What the defect looked like: the unsparing form closes the socket the
    /// submit arrived on (4401, no drain) before the handler's own
    /// `SuccessionSubmitReply` can be written, so every app performing a
    /// succession while signed in — all seven wrap the live `NestClient` in the
    /// `RecoveryClient` — took `succeed_with_held_kit`'s `Unconfirmed` arm on a
    /// ceremony that had committed.
    #[test]
    fn the_ceremony_spares_the_connection_it_is_answering() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/recovery_handlers.rs");
        let src = std::fs::read_to_string(&path).expect("read own source");

        let start = src
            .find("fn succession_submit_handler()")
            .expect("succession_submit_handler must exist");
        let after_start = &src[start..];
        let next_fn = after_start[1..]
            .find("\nfn ")
            .map(|i| i + 1)
            .expect("another top-level fn must follow");
        let body = &after_start[..next_fn];

        // Code only, never comments — the neighbouring census learned this the
        // hard way: its first draft was satisfied by the prose in this very
        // handler's comment, and stayed green under the mutation it guards.
        let code: String = body
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !(t.starts_with("//") || t.starts_with("*") || t.starts_with("/*"))
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            code.contains("revoke_actor_authority_sparing_caller"),
            "succession_submit_handler must strip the retired identity's \
             authority with `revoke_actor_authority_sparing_caller`: the \
             unsparing form closes the socket this request arrived on before \
             the ceremony's own reply can be written, and the client then \
             cannot tell that the account moved (`identity-succession.md` \
             § Enforcement on the home nest)"
        );
    }
}
