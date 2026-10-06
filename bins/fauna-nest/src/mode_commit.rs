//! The shared **signed pre-identity mode-commit** ceremony — actor_id parse,
//! timestamp freshness, Ed25519 signature verification over caller-built
//! signed bytes, then claimed+admin authorisation. Factored out of
//! `nat_mode_core` and the since-retired `storage_mode_core` (its
//! validate-then-discard shim left with the compat-remnant sweep, 2026-09-24)
//! along with the error enum and `*_error_to_rpc` mapping; this module owns
//! all three once. The caller keeps its own mode-string parsing, wire
//! signed-message builder, and post-validation effect.

use fauna_protocol::RpcError;

use crate::routes::{AppState, parse_actor_id};

/// Max age of a mode-commit request timestamp (5 minutes), same window as
/// claim-admin. Also the ± freshness window [`crate::auth_core::ReplayGuard`]
/// is given below — the guard remembers a spent signature for **two** of
/// these windows, not one, so a blob timestamped up to one window in the
/// future (an ordinary client clock running behind the server) can never
/// still be replayed after its entry is forgotten. The doubling lives in
/// [`crate::auth_core::ReplayGuard::check_and_record`] — shared with direct
/// auth, the device handshake, and the device-adopt / device-grant-revoke
/// proof-of-possession gate, each of which passes its own plain ± window and
/// lets the guard double it, so no caller can reintroduce that off-by-one by
/// hand.
pub const MAX_MODE_COMMIT_AGE_SECS: u64 = 300;

/// Transport-agnostic failure of a signed mode-commit ceremony. The WS
/// handler maps each variant to an `RpcError` code (`fauna.setup.*`) via
/// [`mode_commit_error_to_rpc`].
#[derive(Debug)]
pub enum ModeCommitError {
    /// Unknown mode, malformed actor_id, stale timestamp, or malformed public
    /// key. WS `fauna.setup.invalid_request`.
    InvalidRequest(&'static str),
    /// The signature was malformed or failed verification. WS
    /// `fauna.setup.signature_failed`.
    SignatureFailed(&'static str),
    /// The nest has not been claimed yet (no users). WS `fauna.setup.not_claimed`.
    NotClaimed,
    /// The signing actor is not the nest admin. WS `fauna.setup.forbidden`.
    NotAdmin,
    /// Server-side failure. WS `fauna.protocol.internal`.
    Internal(&'static str),
}

/// Map a [`ModeCommitError`] to its `fauna.setup.*` / `fauna.protocol.*`
/// `RpcError` code, consumed by `nat_mode_handlers` — locking the codes here
/// locks them on the wire.
pub fn mode_commit_error_to_rpc(e: ModeCommitError) -> RpcError {
    match e {
        ModeCommitError::InvalidRequest(reason) => {
            crate::rpc_errors::invalid_request_ns("setup", reason)
        }
        ModeCommitError::SignatureFailed(reason) => {
            crate::rpc_errors::signature_failed_ns("setup", reason)
        }
        ModeCommitError::NotClaimed => crate::rpc_errors::not_claimed_ns("setup"),
        ModeCommitError::NotAdmin => crate::rpc_errors::bare_forbidden_ns("setup"),
        ModeCommitError::Internal(msg) => crate::rpc_errors::internal(msg),
    }
}

/// Validate a signed pre-identity mode-commit request: `actor_id_hex` parses,
/// `timestamp_ms` is within [`MAX_MODE_COMMIT_AGE_SECS`], the Ed25519
/// signature verifies under the parsed actor over the bytes `build_signed_bytes`
/// returns (called with the canonical lowercase actor hex, since the wire
/// signed-message builders take the *parsed-then-re-encoded* actor, not the
/// caller-supplied string verbatim), the nest is claimed, and the signer is
/// its admin. `log_context` names the caller in the `is_admin`-failure log
/// line (e.g. `"nat-mode commit"`). Returns the parsed
/// actor bytes on success — the caller applies (or discards) its own
/// mode-specific effect.
///
/// **Single-use.** A verified signature authorises **one** commit, not every
/// commit inside its freshness window. These kinds ride the anonymous
/// connection where — in `nat_mode`'s own words — "the signature *is* the auth
/// (no bearer)", so without consumption the wire triple `(actor_id, timestamp,
/// signature)` is a bearer token for the whole ±[`MAX_MODE_COMMIT_AGE_SECS`]:
/// capture one and re-submit it verbatim, and the posture moves — which for
/// `nat_mode` means `apply_node_mode_change`'s supervisor reconcile starts or
/// stops the perimeter SMTP parser. This is the property `transport.md`
/// § Pre-identity (anonymous) connection already ratifies for the sibling
/// signature-as-auth kind on this same connection, applied here to the
/// mode commits. The *cross-nest* arm is
/// a separate, unbuilt half — the signed message still names no nest.
pub async fn validate_mode_commit(
    state: &AppState,
    actor_id_hex: &str,
    timestamp_ms: i64,
    signature_hex: &str,
    build_signed_bytes: impl FnOnce(&str) -> Vec<u8>,
    log_context: &str,
) -> Result<[u8; 32], ModeCommitError> {
    let actor_bytes =
        parse_actor_id(actor_id_hex).ok_or(ModeCommitError::InvalidRequest("invalid actor_id"))?;

    let now_secs = fauna_core::data::Timestamp::now_secs() as u64;
    let ts_secs = if timestamp_ms > 1_000_000_000_000 {
        (timestamp_ms / 1000) as u64
    } else {
        timestamp_ms.max(0) as u64
    };
    if now_secs.abs_diff(ts_secs) > MAX_MODE_COMMIT_AGE_SECS {
        return Err(ModeCommitError::InvalidRequest("timestamp too old"));
    }

    // Verify the Ed25519 signature over the canonical bytes through the one
    // nest-wide primitive (`fauna_core::identity::verify_detached`:
    // small-order key refusal + `verify_strict`). `actor_id` is wire-supplied
    // here, which is exactly the class the permissive `VerifyingKey::verify`
    // does not cover.
    let sig_bytes: [u8; 64] = hex::decode(signature_hex)
        .ok()
        .and_then(|b| <[u8; 64]>::try_from(b).ok())
        .ok_or(ModeCommitError::SignatureFailed("invalid signature hex"))?;
    // Canonical signed bytes via the single-source, domain-tagged builder.
    // actor_hex is lowercase, matching the wizard.
    let actor_hex = hex::encode(actor_bytes);
    let signed = build_signed_bytes(&actor_hex);
    if !fauna_core::identity::verify_detached(&actor_bytes, &signed, &sig_bytes) {
        return Err(ModeCommitError::SignatureFailed(
            "signature verification failed",
        ));
    }

    if !state.db.has_any_user().await.unwrap_or(false) {
        return Err(ModeCommitError::NotClaimed);
    }
    match state.db.is_admin(&actor_bytes).await {
        Ok(true) => {}
        Ok(false) => return Err(ModeCommitError::NotAdmin),
        Err(e) => {
            tracing::error!("is_admin check during {log_context}: {e}");
            return Err(ModeCommitError::Internal("internal error"));
        }
    }

    // Spend the signature. Deliberately the **last** gate: recording only after
    // `is_admin` means an entry can be added exclusively by an actor who could
    // have committed anyway, so an anonymous caller cannot grow the map at all.
    // The store is the one direct auth uses — a single map keyed on signature
    // bytes, which is why contexts sharing it cannot collide
    // (`auth_core.rs`'s device-handshake arm makes the same reuse).
    //
    // The refusal is the **opaque** signature failure, byte-for-byte the reason
    // a genuine verification failure returns, because the reason string reaches
    // the wire as `RpcError::details` (`rpc_errors::signature_failed_ns`): a
    // distinguishable "already used" would turn this pre-identity kind into a
    // replay-vs-forgery oracle. The distinction is kept server-side, in the log.
    //
    // This does not make the kinds `forbid_replay = true`. An honest client's
    // auto-retry re-signs with a fresh timestamp (`build_signed_nat_mode_body`
    // stamps `now_millis` per submit), and the admin surface leaves its save
    // control enabled on error, so the one refused shape — an exact
    // byte-identical re-submission — is the attacker's, and is recoverable.
    let now_ms = fauna_core::data::Timestamp::now_millis();
    if !state
        .auth
        .replay_guard
        .check_and_record(&sig_bytes, now_ms, MAX_MODE_COMMIT_AGE_SECS * 1000)
        .await
    {
        tracing::warn!("replayed signature refused during {log_context}");
        return Err(ModeCommitError::SignatureFailed(
            "signature verification failed",
        ));
    }

    Ok(actor_bytes)
}
