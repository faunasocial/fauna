//! Transport-agnostic in-band invite-request + invite-code core. The single
//! source of the four public invite ceremonies (submit / status / cancel /
//! invite-code verify), now consumed solely by the pre-identity WS-RPC handlers
//! (`invite_handlers::register_invite_handlers`) — the HTTP twins were all
//! retired (invite-code verify in S4b; submit / status / cancel in S4a2,
//! tracked internally). The handler layer is a thin adapter
//! mapping `InviteError` to an `RpcError` and the success value to the
//! `fauna_protocol::invite` wire types. Mirrors `account_core` / `claim_core`.
//! Originally part of the WS-RPC-everywhere migration (tracked internally).
//!
//! The module is deliberately free of any `fauna_protocol` dependency; the
//! mapping core → wire happens in `invite_handlers`. The success value is the
//! crate's own `db::InviteRequestRow`, which `invite_handlers` serializes via the
//! `InviteRequestStatus` wire type.
//!
//! Rate-limiting note: per-source throttling of `submit` / `invite_code.verify`
//! lives in the dispatcher (`routes::dispatch_request` gates 1b⁗⁗ / 1b‴), keyed
//! on the real client IP (`RpcConnection.peer_addr`) — restored now that the SNI
//! router conveys it (the earlier "no peer IP yet" A1.1 deferral is closed; see
//! `crate::anonymous_rate_limit`). The cores stay transport-agnostic; the one
//! abuse control the *core* owns is the global pending-row cap below
//! (`MAX_PENDING_INVITE_REQUESTS`), the disk backstop a distributed flood can't
//! evade by rotating source IPs past the per-source gate.

use crate::db::InviteRequestRow;
use crate::registration::validate_handle;
use crate::routes::{AppState, parse_actor_id};

/// Maximum clock drift for signed requests, in milliseconds (matches the legacy
/// `invite_requests` handlers).
const MAX_TIMESTAMP_DRIFT_MS: u64 = 30_000;

/// Hard cap on the optional free-text message (matches the legacy handler).
const MAX_MESSAGE_LEN: usize = 500;

/// Global cap on the number of **pending** invite-request rows the nest will
/// hold at once. The per-`actor_id` dedup + the per-source dispatcher throttle
/// both bound a *single* source, but a distributed flood rotating keypairs and
/// source IPs could still grow `invite_requests` without bound (disk on a box
/// already wedged once by disk exhaustion, plus admin-UI clutter — security
/// review § D6). At the cap, new `submit`s are refused with
/// `InviteError::TooManyPending` until an admin approves/denies pending rows.
/// 10 000 is far above any plausible legitimate backlog for a single nest while
/// keeping the table small (each row is a handle + a ≤500-char message).
const MAX_PENDING_INVITE_REQUESTS: u64 = 10_000;

/// Transport-agnostic invite failure. Adapters map each variant to an HTTP
/// status + message (the twin reproduces them exactly) or an `RpcError` code
/// (`fauna.account.*`).
#[derive(Debug)]
pub enum InviteError {
    /// A 400-class validation failure — malformed actor_id, bad/reserved
    /// handle, over-long message, or stale timestamp. HTTP 400 with the carried
    /// message; WS `fauna.account.invalid_request` (detail = the message).
    InvalidRequest(&'static str),
    /// Ed25519 signature verification failed (or the signature/key was
    /// malformed — the twin returned 401 for all of these). HTTP 401 with the
    /// carried message; WS `fauna.account.signature_failed`.
    SignatureFailed(&'static str),
    /// The actor already has a real account (shouldn't be asking for an
    /// invite). HTTP 409 "actor already registered"; WS
    /// `fauna.account.actor_exists`.
    ActorAlreadyRegistered,
    /// The identity was succeeded on this nest and its key registers nothing
    /// here ever again — an invite request is the first step of a registration
    /// (`identity-succession.md` § Enforcement on the home nest, step 4 — the
    /// registration doors; `auth_core::successor_of`). WS
    /// `fauna.auth.superseded`.
    Superseded { new_actor_id: [u8; 32] },
    /// The requested handle is already assigned. HTTP 409 "handle already
    /// taken"; WS `fauna.account.handle_taken`.
    HandleTaken,
    /// This actor already has an invite-request row. HTTP 409: `Some(row)`
    /// returns the existing row body; `None` (the rare `UNIQUE`-race fallback
    /// where the row vanished between create and re-fetch) returns the message
    /// "invite request already exists". WS `fauna.account.invite_request_exists`
    /// in both cases (the row is dropped — the caller re-queries via
    /// `invite_request.status`).
    AlreadyExists(Option<InviteRequestRow>),
    /// No invite-request row found for a status / cancel lookup. HTTP 404 with
    /// the carried message; WS `fauna.account.invite_request_not_found`.
    InviteRequestNotFound(&'static str),
    /// The invite code is invalid or has no uses left (verify). HTTP 404
    /// "invite code is invalid or has no uses left"; WS
    /// `fauna.account.invite_code_invalid`.
    InviteCodeInvalid,
    /// The submit's age claim carried an attestation that failed verification
    /// (`age_attest`). On THIS path the claim is advisory (absence-as-signal
    /// for the deciding admin — it never gates the request), but a claim
    /// *presented* as attested and failing verification is refused rather than
    /// silently downgraded to declared: recording `attested-*` on the request
    /// row is exactly what the admin trusts. WS
    /// `fauna.account.age_attestation_invalid` (detail = the message).
    AgeAttestationInvalid(&'static str),
    /// The nest is already holding `MAX_PENDING_INVITE_REQUESTS` pending rows —
    /// the global flood backstop tripped. The submitter should retry later (once
    /// an admin has cleared the backlog); WS `fauna.protocol.rate_limited` (a
    /// transient capacity refusal, reusing the throttle code clients already
    /// handle — no new client-facing error code).
    TooManyPending,
    /// Server-side failure (`tracing::error!` carries the real cause; the
    /// message is the twin's fixed "internal error"). HTTP 500; WS
    /// `fauna.protocol.internal`.
    Internal(String),
}

fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis()
}

/// Verify an Ed25519 signature over `msg`.
fn verify_sig(actor_bytes: &[u8; 32], msg: &[u8], signature_hex: &str) -> Result<(), InviteError> {
    let sig_bytes = hex::decode(signature_hex)
        .ok()
        .filter(|b| b.len() == 64)
        .ok_or(InviteError::SignatureFailed("invalid signature hex"))?;
    if fauna_core::identity::verify_detached(actor_bytes, msg, &sig_bytes) {
        Ok(())
    } else {
        Err(InviteError::SignatureFailed(
            "signature verification failed",
        ))
    }
}

/// The `fauna.account.invite_request.submit` ceremony (formerly `POST
/// /api/v1/invite-requests`, retired S4a2), minus any per-IP rate-limit.
/// Validates the handle (lowercased), message length, timestamp (±30 s), and
/// the domain-tagged signature over
/// `fauna_protocol::invite::invite_submit_signed_message`, then
/// rejects already-registered actors / taken handles / existing rows and
/// creates the request. Returns the created row.
pub async fn submit_invite_request_core(
    state: &AppState,
    actor_id_hex: &str,
    handle: &str,
    message: &str,
    timestamp_ms: u64,
    signature_hex: &str,
    age_claim: Option<&fauna_protocol::age::AgeClaim>,
) -> Result<InviteRequestRow, InviteError> {
    let actor_bytes =
        parse_actor_id(actor_id_hex).ok_or(InviteError::InvalidRequest("invalid actor_id hex"))?;

    // The applicant's age claim — recorded on the request row for
    // absence-as-signal (`public-mode.md` § Age at registration; it never
    // gates this path — the deciding admin's judgment does). Band tokens are
    // validated closed-set; an attestation, when presented, must verify
    // (`age_attest`) before the row records `attested-*` — one this build
    // cannot check records `none`, exactly as if it were absent.
    let request_age: Option<(
        fauna_protocol::age::AgeBand,
        fauna_protocol::age::AgeBandProvenance,
    )> = match age_claim {
        Some(claim) => {
            let band = fauna_protocol::age::AgeBand::from_wire(&claim.band)
                .ok_or(InviteError::InvalidRequest("unknown age band"))?;
            let provenance = match &claim.attestation {
                Some(attestation) => match crate::age_attest::verify_age_attestation(
                    state,
                    attestation,
                    band,
                    &actor_bytes,
                )
                .await
                .map_err(InviteError::AgeAttestationInvalid)?
                {
                    crate::age_attest::AttestationOutcome::Verified(provenance) => provenance,
                    crate::age_attest::AttestationOutcome::CannotCheck => {
                        fauna_protocol::age::AgeBandProvenance::None
                    }
                },
                None => fauna_protocol::age::AgeBandProvenance::None,
            };
            Some((band, provenance))
        }
        None => None,
    };

    let handle = handle.to_lowercase();
    validate_handle(&handle).map_err(InviteError::InvalidRequest)?;
    if state
        .auth
        .registration
        .reserved_handles
        .iter()
        .any(|r| r == &handle)
    {
        return Err(InviteError::InvalidRequest("handle is reserved"));
    }

    if message.len() > MAX_MESSAGE_LEN {
        return Err(InviteError::InvalidRequest("message too long"));
    }

    if timestamp_ms.abs_diff(now_ms()) > MAX_TIMESTAMP_DRIFT_MS {
        return Err(InviteError::InvalidRequest(
            "timestamp too far from server time",
        ));
    }

    // Signed message: the tagged, length-prefixed submit form via the
    // single-source builder. Verified over the
    // lowercased handle — the exact string being claimed.
    let signed = fauna_protocol::invite::invite_submit_signed_message(
        &actor_bytes,
        &handle,
        message,
        timestamp_ms,
    );
    verify_sig(&actor_bytes, &signed, signature_hex)?;

    // Supersession consult, after the signature and before the account gates
    // (`auth_core::successor_of`): a retired key is refused whether or not its
    // handle-less `users` row still exists.
    match crate::auth_core::successor_of(state, &actor_bytes).await {
        Ok(None) => {}
        Ok(Some(new_actor_id)) => return Err(InviteError::Superseded { new_actor_id }),
        Err(_) => return Err(InviteError::Internal("internal error".into())),
    }

    // Already-registered actors shouldn't be asking for an invite — a suspended
    // one included, by ruling (`login.md` § Errors, the registration doors'
    // accepted exception to the opaque `not_registered` code): a suspended
    // account is still an account, and its one way back is the admin's
    // Restore, never a second admission. `is_actor_registered` reads no
    // standing on purpose.
    match state.db.is_actor_registered(&actor_bytes).await {
        Ok(true) => return Err(InviteError::ActorAlreadyRegistered),
        Err(e) => {
            tracing::error!("invite request db error: {e}");
            return Err(InviteError::Internal("internal error".into()));
        }
        Ok(false) => {}
    }

    // Early rejection on a currently-taken handle (admin re-checks at approval).
    match state.db.resolve_handle(&handle).await {
        Ok(Some(_)) => return Err(InviteError::HandleTaken),
        Err(e) => {
            tracing::error!("resolve handle error: {e}");
            return Err(InviteError::Internal("internal error".into()));
        }
        Ok(None) => {}
    }

    // Does this actor already have a row (pending or decided)?
    match state.db.get_invite_request_by_actor(&actor_bytes).await {
        Ok(Some(existing)) => return Err(InviteError::AlreadyExists(Some(existing))),
        Err(e) => {
            tracing::error!("invite request lookup: {e}");
            return Err(InviteError::Internal("internal error".into()));
        }
        Ok(None) => {}
    }

    // Global pending-row cap (the flood backstop a distributed source-rotating
    // attack can't evade past the per-source dispatcher throttle — § D6). Checked
    // only here, when we're about to create a *new* row: an existing actor's
    // re-submit already returned `AlreadyExists` above, so a full table never
    // blocks a legitimate caller from reading their own pending request.
    match state.db.count_pending_invite_requests().await {
        Ok(n) if n >= MAX_PENDING_INVITE_REQUESTS => {
            tracing::warn!(
                pending = n,
                cap = MAX_PENDING_INVITE_REQUESTS,
                "invite-request submit refused: pending-row cap reached"
            );
            return Err(InviteError::TooManyPending);
        }
        Err(e) => {
            tracing::error!("count pending invite requests: {e}");
            return Err(InviteError::Internal("internal error".into()));
        }
        Ok(_) => {}
    }

    let id = match state
        .db
        .create_invite_request(
            &actor_bytes,
            &handle,
            message,
            request_age.map(|(b, p)| (b.as_str(), p.as_str())),
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            let debug_msg = format!("{e:?}");
            if debug_msg.contains("UNIQUE") {
                // Racing submitter — fetch the existing row.
                if let Ok(Some(existing)) = state.db.get_invite_request_by_actor(&actor_bytes).await
                {
                    return Err(InviteError::AlreadyExists(Some(existing)));
                }
                return Err(InviteError::AlreadyExists(None));
            }
            tracing::error!("create invite request: {e}");
            return Err(InviteError::Internal("internal error".into()));
        }
    };

    tracing::info!(handle = %handle, id = id, "invite request submitted");

    match state.db.get_invite_request(id).await {
        Ok(Some(row)) => Ok(row),
        _ => Err(InviteError::Internal("created row not found".into())),
    }
}

/// The `fauna.account.invite_request.status` ceremony. Public read of an actor's
/// invite-request row.
pub async fn get_invite_request_status_core(
    state: &AppState,
    actor_id_hex: &str,
) -> Result<InviteRequestRow, InviteError> {
    let actor_bytes =
        parse_actor_id(actor_id_hex).ok_or(InviteError::InvalidRequest("invalid actor_id hex"))?;
    match state.db.get_invite_request_by_actor(&actor_bytes).await {
        Ok(Some(row)) => Ok(row),
        Ok(None) => Err(InviteError::InviteRequestNotFound(
            "no invite request for this actor",
        )),
        Err(e) => {
            tracing::error!("get invite request status: {e}");
            Err(InviteError::Internal("internal error".into()))
        }
    }
}

/// The `fauna.account.invite_request.cancel` ceremony. Cancels one's own pending
/// request after verifying the domain-tagged signature over
/// `fauna_protocol::invite::invite_cancel_signed_message`.
pub async fn cancel_invite_request_core(
    state: &AppState,
    actor_id_hex: &str,
    timestamp_ms: u64,
    signature_hex: &str,
) -> Result<(), InviteError> {
    let actor_bytes =
        parse_actor_id(actor_id_hex).ok_or(InviteError::InvalidRequest("invalid actor_id hex"))?;

    if timestamp_ms.abs_diff(now_ms()) > MAX_TIMESTAMP_DRIFT_MS {
        return Err(InviteError::InvalidRequest(
            "timestamp too far from server time",
        ));
    }

    // Signed message: the tagged cancel form via the single-source builder —
    // the registry tag replaced the ad-hoc b"cancel" literal.
    let signed = fauna_protocol::invite::invite_cancel_signed_message(&actor_bytes, timestamp_ms);
    verify_sig(&actor_bytes, &signed, signature_hex)?;

    match state.db.delete_invite_request_by_actor(&actor_bytes).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(InviteError::InviteRequestNotFound(
            "no invite request to cancel",
        )),
        Err(e) => {
            tracing::error!("delete invite request: {e}");
            Err(InviteError::Internal("internal error".into()))
        }
    }
}

/// The `fauna.account.invite_code.verify` ceremony (formerly `POST
/// /api/v1/invite-code/verify`, retired S4b), minus any per-IP rate-limit.
/// Peek-only existence/uses check; returns the opaque `invite_id` (the code
/// itself) the wizard round-trips into `register`, plus — for a supervised
/// (guardian-carrying) code — the guardian's handle, so onboarding renders
/// `invite-code-supervised-notice` BEFORE redemption (`family-safety.md`
/// § Wire & data shape, transparency at creation). `NotFound` on an
/// invalid / exhausted code.
///
/// A code absent from the invite-code table falls through to a **peek-only**
/// membership-claim check (`account_core::peek_membership_claim`, never the
/// redeeming half) — `register_core` already admits a membership payment claim
/// pasted into this same field (monetization.md § Pillar 4 Rail C step 1), and
/// without this fallback the wizard's own pre-submit gate refuses the code
/// before the user can ever reach `register`. Peek-only is deliberate: a user
/// who verifies then abandons the form must not have burned their one-shot
/// claim. A membership claim carries no guardian, so `supervised_by` is always
/// `None` on this arm.
pub async fn verify_invite_code_core(
    state: &AppState,
    code: &str,
) -> Result<(String, Option<String>, Option<String>), InviteError> {
    match state.db.peek_invite_code(code).await {
        // The wizard's OobCodeState::Valid carries an opaque `invite_id` that
        // round-trips into the eventual `register` call — use the code itself,
        // keeping the round-trip 1:1 (matches the HTTP twin). The supervised
        // disclosure gains the band beside the guardian handle
        // (`family-safety.md` § The account age band — transparency BEFORE
        // redemption, exactly like `supervised_by`).
        Ok(Some(grant)) => {
            let supervised_by = match grant.guardian_actor.as_deref().map(<[u8; 32]>::try_from) {
                Some(Ok(id)) => state.db.get_handle(&id).await.ok().flatten(),
                _ => None,
            };
            Ok((code.to_string(), supervised_by, grant.age_band))
        }
        Ok(None) => match crate::account_core::peek_membership_claim(state, code).await {
            Ok(Some(_)) => Ok((code.to_string(), None, None)),
            Ok(None) => Err(InviteError::InviteCodeInvalid),
            Err(e) => {
                tracing::error!("verify invite code (membership-claim peek) db error: {e}");
                Err(InviteError::Internal("internal error".into()))
            }
        },
        Err(e) => {
            tracing::error!("verify invite code db error: {e}");
            Err(InviteError::Internal("internal error".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    /// `count_pending_invite_requests` (the global flood-cap backstop's input,
    /// § D6) must count **only** rows still in the `pending` state — decided
    /// (denied / approved-and-deleted) and cancelled rows don't represent
    /// unbounded attacker-driven growth and must not keep the cap tripped.
    #[tokio::test]
    async fn count_pending_excludes_decided_and_cancelled() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin = [9u8; 32];

        // Three distinct actors each submit a pending request.
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];
        let id_a = db
            .create_invite_request(&a, "alice", "hi", None)
            .await
            .unwrap();
        db.create_invite_request(&b, "bob", "hi", None)
            .await
            .unwrap();
        db.create_invite_request(&c, "carol", "hi", None)
            .await
            .unwrap();
        assert_eq!(db.count_pending_invite_requests().await.unwrap(), 3);

        // Denying one drops it out of the pending count (the row is retained for
        // audit but is no longer "outstanding").
        assert!(db.deny_invite_request(id_a, &admin, None).await.unwrap());
        assert_eq!(db.count_pending_invite_requests().await.unwrap(), 2);

        // Cancelling (deleting) another also drops it.
        assert!(db.delete_invite_request_by_actor(&b).await.unwrap());
        assert_eq!(db.count_pending_invite_requests().await.unwrap(), 1);

        // The one remaining pending row is still counted.
        assert!(db.get_invite_request_by_actor(&c).await.unwrap().is_some());
    }
}
