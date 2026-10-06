//! Transport-agnostic auth-bootstrap core. The single source of the
//! direct-auth / challenge / verify ceremonies. The pre-identity WS-RPC
//! handlers (`auth_handlers::register_auth_handlers`) are now the SOLE
//! consumer: the challenge/verify ceremonies ride `fauna.auth.{challenge,
//! verify}` and the direct-auth bearer-bootstrap rides `fauna.auth.handshake`.
//! Every HTTP twin that shared this core — challenge/verify and, at the
//! endgame, the `/auth/token` bootstrap (`routes::post_auth_token`) — was
//! deleted in the WS-RPC-everywhere rip-out, so the only adapter left maps
//! `AuthError` to an `RpcError` (`auth_handlers::auth_error_to_rpc`).
//!
//! The signed-message construction, ±30 s drift window, token TTL, and side
//! effects (new-IP `SecurityEvent`, account lockout, auto-register vs.
//! private-nest reject) are preserved exactly as the HTTP routes did them —
//! see `docs/goal/behavior/login.md` § Two auth endpoints.

use std::collections::HashMap;

use ed25519_dalek::{Signature, VerifyingKey};
use tokio::sync::Mutex;

use crate::routes::{AppState, parse_actor_id};
use std::sync::Arc;

/// Single-use guard for verified pre-identity signatures (2026-06-01 security
/// review § L4, generalized 2026-08-31 — `transport.md` § Pre-identity
/// (anonymous) connection: "single-use is the property of every
/// signature-as-auth kind on this connection, not just direct auth"). Direct
/// auth (`fauna.auth.handshake`), the device handshake, and the device-adopt
/// / device-grant-revoke proof-of-possession gate each authenticate a
/// signature over a domain-tagged payload inside a ± freshness window — so an
/// on-path TLS attacker who captured one valid request could replay it
/// *verbatim* within the window to re-run the authenticated effect. We make a
/// verified signature single-use: an Ed25519 signature is deterministic, so
/// it is a unique token for its signed message; recording it on first use and
/// rejecting a repeat closes the window. The per-request nonce or
/// domain-tagged payload each caller signs is what lets two *legitimate*
/// concurrent same-actor requests coexist (distinct payloads ⇒ distinct
/// signatures ⇒ neither looks like the other's replay); the guard still stops
/// a *verbatim* capture-replay of any of them. The challenge/verify path is
/// already nonce-protected (the server nonce is consumed once) and needs no
/// guard.
#[derive(Default)]
pub struct ReplayGuard {
    /// signature bytes -> expiry (ms since epoch).
    seen: Mutex<HashMap<[u8; 64], u64>>,
}

impl ReplayGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `signature` as used at `now_ms`, given the caller's own **±**
    /// freshness `window_ms` (the same bound its timestamp check enforces).
    /// Returns `true` if it was fresh (now recorded), `false` if it was
    /// already seen and still within memory — i.e. a replay. Expired entries
    /// are swept first, so the map only ever holds signatures inside the
    /// active memory.
    ///
    /// **Remembers for `2 × window_ms`, not `1 ×` — deliberately, and the
    /// doubling lives HERE rather than in each caller's own constant, so a
    /// future caller passing its plain freshness window can never
    /// reintroduce this gap.** `window_ms` is a ± tolerance, so a blob
    /// timestamped one whole window in the *future* (an ordinary client
    /// clock running behind the server) is accepted on arrival and stays
    /// fresh for another window after that. Remembering it for only one
    /// window expires the entry at the moment the blob becomes replayable:
    ///
    /// ```text
    /// blob signed at T is fresh for   now ∈ [T − W, T + W]
    /// first use at now₀ = T − W       1× memory expires at T   → replayable [T, T+W]
    ///                                 2× memory expires at T+W → never replayable
    /// ```
    ///
    /// Direct auth, the device handshake and the device-adopt /
    /// device-grant-revoke proof-of-possession gate all carried exactly this
    /// residual — a captured verbatim signature was replayable, unremembered,
    /// for up to one full window after being forgotten — until they were
    /// moved onto this shared doubling.
    pub async fn check_and_record(
        &self,
        signature: &[u8; 64],
        now_ms: u64,
        window_ms: u64,
    ) -> bool {
        let ttl_ms = window_ms.saturating_mul(2);
        let mut map = self.seen.lock().await;
        map.retain(|_, expiry| *expiry > now_ms);
        if map.contains_key(signature) {
            return false;
        }
        map.insert(*signature, now_ms.saturating_add(ttl_ms));
        true
    }
}

/// TTL of bearer tokens minted by direct-auth (`fauna.auth.handshake`) and by
/// verify (`fauna.auth.verify`). Both ride WS-RPC; the HTTP twins are gone.
///
/// The nest half of a token-lifetime contract with two client-half
/// constants, both owned by `fauna_protocol::auth` and both pinned below: the
/// caches' spend rule ([`fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS`])
/// and the sync agent's app-dead renewal lead
/// ([`fauna_protocol::auth::AGENT_RENEW_LEAD_SECS`]). Each must stay strictly
/// below this value; the pins are what say so.
pub const TOKEN_TTL_SECS: u64 = 3600;

// The contract is an INEQUALITY, and lowering this side is what breaks it: a
// token whose whole life is shorter than the client's pre-expiry buffer is
// *born spent*, so no client cache ever serves it and every request re-mints
// (`fauna-launch-machine`'s refresh loop degenerates further, into a spin —
// it sleeps `expires_at - buffer - now`). Both ends stay internally consistent
// while this happens, which is why no test on either side can see it.
const _: () = assert!(
    TOKEN_TTL_SECS > fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS,
    "a minted bearer must outlive the client's pre-expiry refresh buffer, \
     or every token is born spent and clients re-mint on every request"
);

// The same inequality against the OTHER client-half constant — the sync
// agent's app-dead renewal loop (`bins/fauna-sync-agent/src/renewal.rs`). It is
// the participant that *mints* rather than caches, so it fails differently, and
// it is a second binary: neither crate can see the other, and
// `fauna-protocol::auth` is the only place both meet.
//
// Loud direction: at or above the TTL, every bearer this mint hands back is
// born already inside its own renewal lead, so the loop (which re-plans one
// second after each success) mints forever — ~1 Hz of `device_handshake`, from
// every enrolled machine, with no app running to notice. (The agent always
// knows the deadline it races: every bearer handed to it carries an
// `expires_at` on its own clock, so there is no blind cadence to pin.)
const _: () = assert!(
    TOKEN_TTL_SECS > fauna_protocol::auth::AGENT_RENEW_LEAD_SECS,
    "a minted bearer must outlive the sync agent's renewal lead, or every \
     token is born inside its own lead and the agent mints in a hot loop"
);

/// Maximum tolerated drift between the client-supplied timestamp and server
/// time for direct auth.
pub const MAX_TIMESTAMP_DRIFT_MS: u64 = 30_000;

/// Transport-agnostic auth failure. Adapters map each variant to a status
/// (HTTP) or an `RpcError` code (`fauna.auth.*`).
#[derive(Debug)]
pub enum AuthError {
    /// Malformed request field — bad hex, wrong length, or not a valid key.
    /// HTTP 400; WS `fauna.auth.invalid_request`. Carries a human-readable
    /// field hint.
    InvalidRequest(&'static str),
    /// Client timestamp outside the ±30 s window. HTTP 401; WS
    /// `fauna.auth.timestamp_drift`.
    TimestampDrift,
    /// Ed25519 signature verification failed. HTTP 401; WS
    /// `fauna.auth.signature_failed`.
    SignatureFailed,
    /// Actor not registered (or suspended) on a private nest. HTTP 403 for
    /// direct auth / 404 for verify; WS `fauna.auth.not_registered`.
    NotRegistered(String),
    /// Account is locked until the given Unix-seconds instant. HTTP 423; WS
    /// `fauna.auth.account_locked`.
    AccountLocked { locked_until: i64 },
    /// Challenge nonce invalid, expired, or already consumed (verify only).
    /// HTTP 404; WS `fauna.auth.invalid_nonce`.
    InvalidNonce,
    /// The identity was **succeeded** — its account is re-pointed to
    /// `new_actor_id` and this key authorizes nothing here ever again
    /// (`identity-succession.md` § Enforcement on the home nest, step 4).
    /// WS `fauna.auth.superseded`.
    ///
    /// Deliberately *not* folded into [`Self::NotRegistered`], whose opacity is
    /// a feature (it must not distinguish unknown from suspended). Here the
    /// party hitting the refusal is overwhelmingly the succeeded user's own
    /// device, and its next step — import the successor identity — is only
    /// possible if the error says so.
    Superseded { new_actor_id: [u8; 32] },
    /// Server-side failure. HTTP 500; WS `fauna.protocol.internal`.
    Internal(String),
}

/// The supersession consult — the enforcement point of
/// `identity-succession.md:71`.
///
/// Every ceremony that turns a seed signature into authority calls this. It has
/// to be an explicit table read: signature verification in Fauna is
/// **self-describing** (`verify_envelope` reads the pubkey out of the payload,
/// `identity-succession.md:20`), so a thief's signatures keep verifying forever
/// on their own merits and nothing about the key itself ever expires. Refusing
/// them is a *decision the nest stores*, not a property the crypto provides.
///
/// **Placed after signature verification, before the account gates.** After, so
/// an anonymous caller cannot conscript a database read with an unsigned
/// request (the ordering `direct_auth_core` step 3 already establishes). Before,
/// because supersession outranks every other verdict: a succeeded account that
/// is also locked or suspended must report `superseded`, since that is the only
/// verdict with an action attached — and, load-bearing for the theft case, the
/// thief's own emergency lockout must not mask the refusal that undoes them.
///
/// Fails **closed** is not the right posture here and it is deliberate: a
/// database error yields `Internal`, refusing the mint, rather than letting the
/// ceremony proceed unchecked.
async fn refuse_if_superseded(state: &AppState, actor_id: &[u8; 32]) -> Result<(), AuthError> {
    match successor_of(state, actor_id).await {
        Ok(None) => Ok(()),
        Ok(Some(new_actor_id)) => Err(AuthError::Superseded { new_actor_id }),
        Err(SupersessionConsultError::Db) => Err(AuthError::Internal("database error".into())),
        Err(SupersessionConsultError::MalformedSuccessor) => Err(AuthError::Internal(
            "stored successor id is not 32 bytes".into(),
        )),
    }
}

/// Why [`successor_of`] could not answer. Each door maps both arms to its own
/// `Internal`; the split exists only so the message can name the cause.
#[derive(Debug)]
pub(crate) enum SupersessionConsultError {
    /// The `actor_successions` read failed (already logged).
    Db,
    /// The stored `new_actor_id` is not 32 bytes — a corrupt row.
    MalformedSuccessor,
}

/// The one supersession consult every door shares — `Some(successor)` when
/// `actor_id` was succeeded on this nest, `None` when it was not.
///
/// [`refuse_if_superseded`] is its bearer-minting face; the **registration
/// doors** — `fauna.account.register`, `fauna.account.invite_request.submit`,
/// `fauna.auth.claim_admin` — consult it too, each mapping `Some` to its own
/// `Superseded` variant (`identity-succession.md` § Enforcement on the home
/// nest, step 4). They used to lean on `is_actor_registered` alone: a retired
/// identity's handle-less `users` row answered `actor_exists`, so nothing
/// consulted the succession table at the doors that can CREATE a `users` row.
/// That row goes with the chain when the successor is deleted
/// (`account-data-plane.md` § Nest-side requirements item 1, *Deletion reaches
/// the account's predecessors*), so the refusal has to rest on the row that
/// records the retirement — `actor_successions` is `Policy::Retain` and
/// outlives every deletion.
pub(crate) async fn successor_of(
    state: &AppState,
    actor_id: &[u8; 32],
) -> Result<Option<[u8; 32]>, SupersessionConsultError> {
    match state.db.succession_for(&actor_id[..]).await {
        Ok(None) => Ok(None),
        Ok(Some(row)) => {
            let new_actor_id: [u8; 32] = row
                .new_actor_id
                .as_slice()
                .try_into()
                .map_err(|_| SupersessionConsultError::MalformedSuccessor)?;
            tracing::info!(
                target: "recovery",
                successor = %hex::encode(new_actor_id),
                "refused a superseded identity"
            );
            Ok(Some(new_actor_id))
        }
        Err(e) => {
            tracing::error!("succession consult failed: {e}");
            Err(SupersessionConsultError::Db)
        }
    }
}

/// The nest-binding gate every bearer-minting ceremony runs (`login.md`
/// § Binding the nest): the request's `nest_id` must name THIS nest. Returns
/// the identity the signed message is then built over — always the box's
/// own ([`AppState::bound_identity`]), never the request's field, so a
/// verifier that skipped this gate would still verify bytes the signer did
/// not sign. A blob addressed to another nest is refused *before* any
/// signature work: a verdict about its target, not about its bytes.
fn require_own_nest(state: &AppState, nest_id_hex: &str) -> Result<[u8; 32], AuthError> {
    let bound = parse_actor_id(nest_id_hex).ok_or(AuthError::InvalidRequest("nest_id"))?;
    let own = state.bound_identity();
    if bound != own {
        return Err(AuthError::InvalidRequest("nest_id does not name this nest"));
    }
    Ok(own)
}

/// Result of a successful direct-auth / verify token mint.
pub struct TokenMint {
    pub token: String,
    /// Short 16-hex display id of the minted session (`token_store`'s
    /// `token_id`). Surfaced to the client so it can name its own session in
    /// `fauna.sessions.{revoke,revoke_all}`.
    pub token_id: String,
    /// Unix seconds, on this nest's clock.
    pub expires_at: u64,
    /// The same deadline as seconds from now — the client's scheduling input
    /// (`fauna_protocol::auth::deadline_on_own_clock`; `login.md` § Token
    /// lifetime on the client's clock). Always [`TOKEN_TTL_SECS`] today; a
    /// field rather than a constant the client assumes, so a nest that mints a
    /// different lifetime later does not silently mis-schedule every client.
    pub expires_in: u64,
}

/// Direct auth — the body of the WS `fauna.auth.handshake` kind. (Its HTTP
/// twin, the bearer-bootstrap `POST /api/v1/auth/token`, was deleted at the
/// WS-RPC-everywhere endgame — this core is now reached only over WS-RPC.)
/// Requires the request to name this nest (`nest_id_hex`, `login.md`
/// § Binding the nest), verifies the signature over
/// `fauna_protocol::auth::handshake_signed_message(actor_id, timestamp, nest_id, client_nonce)`
/// (±30 s), enforces registration / lockout, mints a 1-hour bearer, and fires
/// the new-IP `SecurityEvent` when `client_ip` is supplied and changed.
///
/// `client_nonce` is the WS handshake's per-request nonce
/// (`HandshakeRequest.client_nonce`): folding it into the verified message
/// uniquifies the otherwise-deterministic Ed25519 signature, so two clients of
/// one actor signing within the same millisecond produce *distinct* signatures
/// and the single-use replay guard no longer rejects the second as a replay
/// (auth-handshake finding #1).
///
/// `client_ip` is the seam for new-IP detection ([`note_sign_in_address`]). The
/// sole caller — the pre-identity WS handshake handler — passes the address the
/// dispatcher recorded for its anonymous connection
/// (`dispatch_core::current_caller_ip`): the direct TCP peer, or the PROXY-v2
/// source behind the SNI router.
pub async fn direct_auth_core(
    state: &Arc<AppState>,
    actor_id_hex: &str,
    timestamp_ms: u64,
    signature_hex: &str,
    client_nonce: &[u8],
    nest_id_hex: &str,
    client_ip: Option<String>,
) -> Result<TokenMint, AuthError> {
    // 1. Parse actor_id; the blob must be addressed to THIS nest.
    let actor_bytes = parse_actor_id(actor_id_hex).ok_or(AuthError::InvalidRequest("actor_id"))?;
    let own_nest = require_own_nest(state, nest_id_hex)?;

    // 2. Validate timestamp freshness.
    let now_ms = fauna_core::data::Timestamp::now_millis();
    if timestamp_ms.abs_diff(now_ms) > MAX_TIMESTAMP_DRIFT_MS {
        return Err(AuthError::TimestampDrift);
    }

    // 3. Verify signature over (actor_id ‖ timestamp_be) BEFORE any db write
    //    so unauthenticated requests cannot trigger account creation.
    let sig_bytes = match hex::decode(signature_hex) {
        Ok(b) if b.len() == 64 => b,
        _ => return Err(AuthError::InvalidRequest("signature")),
    };
    let signature =
        Signature::from_slice(&sig_bytes).map_err(|_| AuthError::InvalidRequest("signature"))?;
    let msg = fauna_protocol::auth::handshake_signed_message(
        &actor_bytes,
        timestamp_ms,
        &own_nest,
        client_nonce,
    );
    if !fauna_core::identity::verify_detached(&actor_bytes, &msg, &sig_bytes) {
        return Err(AuthError::SignatureFailed);
    }

    // 3b. Replay guard (§ L4): a *verified* signature is single-use within its
    //     drift window. Gating after verification means an attacker can't fill
    //     the guard with junk — only genuinely-signed payloads consume a slot.
    //     A replay returns the opaque `SignatureFailed` (no replay-vs-bad-sig
    //     oracle). A legitimate client retries with a fresh timestamp+signature,
    //     so this only bites an exact-duplicate request inside 30 s. Concurrent
    //     *distinct* same-actor clients no longer collide here: each WS handshake
    //     folds its own `client_nonce` into the signed message, so two legitimate
    //     same-ms requests carry distinct signatures (auth-handshake finding #1).
    if !state
        .auth
        .replay_guard
        .check_and_record(&signature.to_bytes(), now_ms, MAX_TIMESTAMP_DRIFT_MS)
        .await
    {
        return Err(AuthError::SignatureFailed);
    }

    // 3c. Supersession consult (`identity-succession.md:71`). Ahead of the
    //     registration and lockout gates: the succeeded owner needs the
    //     actionable verdict, and the thief's lockout must not mask it.
    refuse_if_superseded(state, &actor_bytes).await?;

    // 4. Check registered + not suspended.
    //
    // An actor with no `users` row is refused in **every** registration mode —
    // holding a valid self-signed token proves key possession, never admission.
    // Accounts come into being only through the ceremony (`fauna.account.register`,
    // which mints a handle), the admin claim (`fauna.auth.claim_admin`, which
    // creates the admin row itself and so keeps a fresh box claimable), or an
    // admin admitting a user. The old `!require_registration` branch here
    // auto-provisioned a handle-less `free` ghost on first handshake — bypassing
    // the invite gate, the free-tier cap, and `Closed` mode alike, and minting an
    // account `public-mode.md` § User Registration says cannot exist (registration
    // *is* choosing a handle). It is gone; `login.md` § Auto-registration retires
    // with it.
    //
    // The 403 stays opaque (`fauna.auth.not_registered` covers "unregistered" and
    // "suspended" alike, `login.md` § Errors), so it is not a suspended-vs-unknown
    // oracle. Refusing a suspended actor here is the mint half of the suspension gate: a
    // suspended user must not simply reconnect and mint a fresh token.
    state
        .db
        .check_actor_active(&actor_bytes)
        .await
        .map_err(|e| AuthError::NotRegistered(e.to_string()))?;

    // 5. Check account lockout.
    if let Ok(Some(locked_until)) = state.db.get_locked_until(&actor_bytes).await {
        let now_secs = now_ms / 1000;
        if (locked_until as u64) > now_secs {
            return Err(AuthError::AccountLocked { locked_until });
        }
    }

    // 6. Issue token.
    let actor_id = fauna_core::identity::ActorId(actor_bytes);
    let crate::token_store::MintedToken { token, token_id } = state
        .auth
        .token_store
        .insert_with_metadata(actor_id, TOKEN_TTL_SECS, None, None)
        .await;
    let expires_at = now_ms / 1000 + TOKEN_TTL_SECS;

    // 7. Detect new-location sign-in and notify.
    note_sign_in_address(state, &actor_bytes, client_ip).await;

    Ok(TokenMint {
        token,
        token_id,
        expires_at,
        expires_in: TOKEN_TTL_SECS,
    })
}

/// New-IP detection — the side effect every bearer mint shares
/// (`login.md` § the handshake's side effects, *New-IP detection*): record
/// `client_ip` as the actor's last sign-in address and, when it differs from a
/// previously recorded one, ring `SecurityEvent::NewTokenIssued`. The first
/// address an actor is ever seen from records silently; `None` (a caller the
/// dispatcher recorded no peer for — an authenticated connection, a direct
/// dispatch in tests) records nothing.
///
/// Both mints call it: the handshake, and the silent challenge's `verify` —
/// which carries every bearer an app holds, launch and refresh alike, so it is
/// where a sign-in from a stolen key actually lands.
async fn note_sign_in_address(state: &Arc<AppState>, actor: &[u8; 32], client_ip: Option<String>) {
    let Some(ip) = client_ip else { return };
    match state.db.update_actor_last_ip(actor, &ip).await {
        Ok(true) => {
            let event = crate::security_notify::SecurityEvent::NewTokenIssued {
                ip_address: Some(ip),
            };
            let notifier = state.security_notifier.clone();
            let scope = state.clone();
            let state = state.clone();
            let actor = *actor;
            scope.spawn_scoped(async move {
                notifier.notify(&state, &actor, &event).await;
            });
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!("update_actor_last_ip failed: {e}");
        }
    }
}

/// Result of issuing a challenge nonce.
pub struct ChallengeIssued {
    pub nonce_hex: String,
    pub expires_in: u64,
    pub expires_at: u64,
}

/// Issue a challenge nonce — the body of `POST /api/v1/auth/challenge`. No
/// security side effects.
pub async fn issue_challenge_core(
    state: &AppState,
    actor_id_hex: &str,
) -> Result<ChallengeIssued, AuthError> {
    let actor_bytes = parse_actor_id(actor_id_hex).ok_or(AuthError::InvalidRequest("actor_id"))?;
    if VerifyingKey::from_bytes(&actor_bytes).is_err() {
        return Err(AuthError::InvalidRequest("public key"));
    }
    let (nonce, expires_at) = state.auth.challenge_store.issue(actor_bytes).await;
    let now = fauna_core::data::Timestamp::now_secs() as u64;
    Ok(ChallengeIssued {
        nonce_hex: hex::encode(nonce),
        expires_in: expires_at.saturating_sub(now),
        expires_at,
    })
}

/// Result of a successful verify — token plus cached metadata.
pub struct VerifyMint {
    pub token: String,
    /// Short 16-hex display id of the minted session (`token_store`'s
    /// `token_id`). Surfaced to the client so it can name its own session in
    /// `fauna.sessions.{revoke,revoke_all}`.
    pub token_id: String,
    pub handle: String,
    pub domain: String,
    pub tier: String,
    /// Unix seconds, on this nest's clock.
    pub expires_at: u64,
    /// Seconds from now — see [`TokenMint::expires_in`].
    pub expires_in: u64,
}

/// Verify a signed challenge nonce and mint a bearer — the body of
/// `fauna.auth.verify`. Requires the request to name this nest (`nest_id_hex`,
/// `login.md` § Binding the nest); signed message is the **domain-tagged**
/// `fauna_protocol::auth::challenge_verify_signed_message(actor_id, nonce, nest_id)`
/// — `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id`, no timestamp (the nonce
/// supplies freshness). Tagged-only since 2026-08-17 and nest-bound-only since
/// 2026-09-23: no other accept path exists. Unregistered actors get
/// `NotRegistered`; an invalid/expired/consumed nonce gets `InvalidNonce`.
///
/// `client_ip` feeds the same new-IP detection the handshake runs
/// ([`note_sign_in_address`]).
pub async fn verify_core(
    state: &Arc<AppState>,
    actor_id_hex: &str,
    nonce_hex: &str,
    signature_hex: &str,
    nest_id_hex: &str,
    client_ip: Option<String>,
) -> Result<VerifyMint, AuthError> {
    let actor_bytes = parse_actor_id(actor_id_hex).ok_or(AuthError::InvalidRequest("actor_id"))?;
    let own_nest = require_own_nest(state, nest_id_hex)?;

    let nonce_bytes: [u8; 32] =
        fauna_core::hex32::decode(nonce_hex).map_err(|_| AuthError::InvalidRequest("nonce"))?;

    let sig_bytes = match hex::decode(signature_hex) {
        Ok(b) if b.len() == 64 => b,
        _ => return Err(AuthError::InvalidRequest("signature")),
    };
    // Signed message is the tagged, nest-bound verify form (no timestamp —
    // nonce supplies freshness), via the single-source builder the client
    // signed with — over OUR identity, never the request's field.
    let msg = fauna_protocol::auth::challenge_verify_signed_message(
        &actor_bytes,
        &nonce_bytes,
        &own_nest,
    );
    if !fauna_core::identity::verify_detached(&actor_bytes, &msg, &sig_bytes) {
        return Err(AuthError::SignatureFailed);
    }

    // Nonce must match the one we issued, and not be used or expired.
    if !state
        .auth
        .challenge_store
        .consume(&actor_bytes, &nonce_bytes)
        .await
    {
        return Err(AuthError::InvalidNonce);
    }

    // Supersession consult, ahead of the user lookup: the old `users` row
    // deliberately survives a succession (it is the FK target and quota home of
    // content that still exists), so `get_user` would happily succeed here and
    // mint for a stolen key.
    refuse_if_superseded(state, &actor_bytes).await?;

    // Look up the user — unregistered actors are the onboarding signal.
    let user = match state.db.get_user(&actor_bytes).await {
        Ok(Some(u)) => u,
        Ok(None) => return Err(AuthError::NotRegistered("actor not registered".into())),
        Err(e) => {
            tracing::error!("auth verify db error: {e}");
            return Err(AuthError::Internal("database error".into()));
        }
    };

    // Refuse a suspended actor — the standing half `check_actor_active` gives
    // `direct_auth_core`, read here off the row already in hand so a database
    // fault stays `Internal` instead of reading as the onboarding signal.
    // Verify is the mint every app-held bearer rides (`login.md` § Silent
    // Challenge), so without this a suspended user's app simply re-minted and
    // reconnected. The code is the same opaque
    // `NotRegistered` the handshake answers — no suspended-vs-unregistered
    // oracle (`login.md` § Errors).
    if user.suspended {
        return Err(AuthError::NotRegistered("user is suspended".into()));
    }

    // Refuse a locked-out actor — the mint half of account lockout, ruled
    // 2026-09-25 (`login.md` § Silent Challenge), after supersession and
    // suspension in the `direct_auth_core` order. Admins are exempt (user ruling
    // 2026-10-01), mirroring the use-time exemption in
    // `auth::check_bearer_session`, until a cannot-lock-the-last-admin guard
    // exists. A database fault reads as no lock, as the handshake's check does.
    if let Ok(Some(locked_until)) = state.db.get_locked_until(&actor_bytes).await {
        let now_secs = fauna_core::data::Timestamp::now_secs() as u64;
        if (locked_until as u64) > now_secs
            && !matches!(state.db.is_admin(&actor_bytes).await, Ok(true))
        {
            return Err(AuthError::AccountLocked { locked_until });
        }
    }

    let handle = match state.db.get_handle(&actor_bytes).await {
        Ok(Some(h)) => h,
        _ => String::new(),
    };
    // The deployment identity domain returned as launch metadata — `handle_domain()`
    // reads the live `identity_domain` cache (the projection of the primary
    // `mail_domains` row set at claim) at top precedence, so a domainless-then-claimed
    // box reports the claimed domain, not the stale `--handle-domain` boot seed.
    // A handle is addressable on every active local domain but is reported under this
    // canonical identity domain (mail-multidomain.md § Multi-domain handles;
    // login.md § Silent Challenge).
    let domain = state.handle_domain();

    let actor_id = fauna_core::identity::ActorId(actor_bytes);
    let crate::token_store::MintedToken { token, token_id } = state
        .auth
        .token_store
        .insert_with_metadata(actor_id, TOKEN_TTL_SECS, None, None)
        .await;
    let now = fauna_core::data::Timestamp::now_secs() as u64;
    let expires_at = now + TOKEN_TTL_SECS;

    tracing::info!(handle = %handle, tier = %user.tier, "challenge-response sign-in");

    note_sign_in_address(state, &actor_bytes, client_ip).await;

    Ok(VerifyMint {
        token,
        token_id,
        handle,
        domain,
        tier: user.tier,
        expires_at,
        expires_in: TOKEN_TTL_SECS,
    })
}

/// Renewal-grant auth — the body of the WS `fauna.auth.device_handshake` kind
/// (`docs/goal/architecture/apps/sync-agent.md` § Credential model; additive
/// 2026-07-19, WS-RPC-native). Verifies a **renewal device key**'s signature
/// over the domain-tagged
/// `fauna_protocol::auth::device_handshake_signed_message(actor_id, device_key,
/// timestamp, nonce)` against the `RenewBearer`-scoped `DeviceAuthorization`
/// stored on the actor's `sync_devices` row, then mints an ordinary 1-hour
/// bearer with the direct-auth side effects (drift window, single-use replay
/// guard, active/lockout checks). The stored grant's root-key envelope is
/// **re-verified at every mint** — the row is just a cache of what the identity
/// client signed, never itself an authority.
///
/// Failure taxonomy is deliberately opaque, mirroring direct auth: a missing /
/// revoked / expired / capability-less grant is `NotRegistered` (the same code
/// the actor-level checks use), and any signature problem is
/// `SignatureFailed` — no oracle distinguishing "no grant" from "suspended".
pub async fn device_auth_core(
    state: &Arc<AppState>,
    actor_id_hex: &str,
    device_key_hex: &str,
    timestamp_ms: u64,
    signature_hex: &str,
    client_nonce: &[u8],
    nest_id_hex: &str,
) -> Result<TokenMint, AuthError> {
    // 1. Parse identities; the blob must be addressed to THIS nest
    //    (`login.md` § Binding the nest).
    let actor_bytes = parse_actor_id(actor_id_hex).ok_or(AuthError::InvalidRequest("actor_id"))?;
    let device_key_bytes =
        parse_actor_id(device_key_hex).ok_or(AuthError::InvalidRequest("device_key"))?;
    let own_nest = require_own_nest(state, nest_id_hex)?;

    // 2. Timestamp freshness — the direct-auth window.
    let now_ms = fauna_core::data::Timestamp::now_millis();
    if timestamp_ms.abs_diff(now_ms) > MAX_TIMESTAMP_DRIFT_MS {
        return Err(AuthError::TimestampDrift);
    }

    // 3. Verify the request signature by the presented device key BEFORE any DB
    //    read beyond the grant lookup — but the grant lookup must come first,
    //    since an unregistered key should not learn whether its signature was
    //    otherwise valid. Load the grant, then check everything.
    let grant_wire = state
        .db
        .get_sync_device_grant(&actor_bytes, &device_key_bytes)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?
        .ok_or_else(|| AuthError::NotRegistered("no renewal grant".into()))?;

    // 3b. Revocation memory — the belt beside the register-side refusal: a
    //     device key tombstoned by `fauna.sync.devices.delete` never mints
    //     again, however its grant row came back (sync-agent.md § Credential
    //     model — device revocation). Opaque like a missing grant: a revoked
    //     key learns nothing a never-registered one would not.
    if state
        .db
        .is_device_grant_revoked(&actor_bytes, &device_key_bytes)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?
    {
        return Err(AuthError::NotRegistered("no renewal grant".into()));
    }

    // 3c. Test-only rendezvous, at the exact instant the window opens:
    //     3b has just observed "not revoked", and everything from here to the
    //     mint at step 7 is work a concurrent `fauna.sync.devices.delete` can
    //     run underneath (three DB round-trips, each taking and releasing the
    //     same `conn` mutex the delete needs). A test arms this to hold a mint
    //     here while it runs an entire revocation, which turns a timing race
    //     into a causal barrier — e2e-conventions.md § convention 14, "assert
    //     latency-independent state, never wall-clock timing". Task-local
    //     rather than a global, so an arming test cannot perturb the other
    //     `device_auth_core` tests running beside it; `#[cfg(test)]` alone, so
    //     no artifact and no integration test can even reach it (§ convention
    //     15 satisfied by construction rather than by a feature flag).
    #[cfg(test)]
    if let Ok(barrier) = mint_race::BARRIER.try_with(std::sync::Arc::clone) {
        barrier.cleared_revocation_check.notify_one();
        barrier.may_mint.notified().await;
    }

    // 4. Re-verify the stored grant: decode the embed-as-bytes wire, check the
    //    root-key envelope signature, and check the authorization's scope.
    let wire: fauna_core::encoding::EmbedAsBytes = fauna_cbor::decode_strict(&grant_wire)
        .map_err(|e| AuthError::Internal(format!("stored grant wire: {e:?}")))?;
    let (auth_bytes, auth_env) = wire
        .into_signed()
        .map_err(|e| AuthError::Internal(format!("stored grant envelope: {e}")))?;
    let auth: fauna_core::data::DeviceAuthorization =
        fauna_core::encoding::decode_signed_bytes(&auth_bytes)
            .map_err(|e| AuthError::Internal(format!("stored grant decode: {e}")))?;
    if auth.actor_id.0 != actor_bytes || auth.device_key != device_key_bytes {
        // A stored grant that doesn't match its own row keys is corrupt state,
        // but surface it opaquely like a missing grant.
        return Err(AuthError::NotRegistered("grant mismatch".into()));
    }
    let has_renew = auth.capabilities.iter().any(|c| {
        matches!(
            c,
            fauna_core::data::Capability::RenewBearer | fauna_core::data::Capability::All
        )
    });
    if !has_renew {
        return Err(AuthError::NotRegistered("grant not renewal-scoped".into()));
    }
    // `Timestamp` is microseconds (`Timestamp::now`); the drift-checked
    // `now_ms` is milliseconds.
    if let Some(expires) = auth.expires_at
        && expires.0 <= now_ms.saturating_mul(1000)
    {
        return Err(AuthError::NotRegistered("grant expired".into()));
    }
    if fauna_core::encoding::verify_envelope(&auth, &auth_bytes, &auth_env).is_err() {
        return Err(AuthError::NotRegistered("grant signature invalid".into()));
    }

    // 5. Verify the request signature by the granted device key over the
    //    domain-tagged message.
    let sig_bytes = match hex::decode(signature_hex) {
        Ok(b) if b.len() == 64 => b,
        _ => return Err(AuthError::InvalidRequest("signature")),
    };
    let signature =
        Signature::from_slice(&sig_bytes).map_err(|_| AuthError::InvalidRequest("signature"))?;
    let msg = fauna_protocol::auth::device_handshake_signed_message(
        &actor_bytes,
        &device_key_bytes,
        timestamp_ms,
        &own_nest,
        client_nonce,
    );
    if !fauna_core::identity::verify_detached(&device_key_bytes, &msg, &sig_bytes) {
        return Err(AuthError::SignatureFailed);
    }

    // 5b. Replay guard — same single-use property as direct auth (§ L4); the
    //     shared map keys on signature bytes, so contexts can't collide.
    if !state
        .auth
        .replay_guard
        .check_and_record(&signature.to_bytes(), now_ms, MAX_TIMESTAMP_DRIFT_MS)
        .await
    {
        return Err(AuthError::SignatureFailed);
    }

    // 6. Actor-level gates, identical to direct auth — including the
    //    supersession consult first. A sync agent holding a renewal grant of
    //    the old identity is exactly the "device registrations and renewal
    //    grants die with the old rows" case (`identity-succession.md:92`): the
    //    grant row may still be there, and this is what stops it minting.
    refuse_if_superseded(state, &actor_bytes).await?;
    state
        .db
        .check_actor_active(&actor_bytes)
        .await
        .map_err(|e| AuthError::NotRegistered(e.to_string()))?;
    if let Ok(Some(locked_until)) = state.db.get_locked_until(&actor_bytes).await {
        let now_secs = now_ms / 1000;
        if (locked_until as u64) > now_secs {
            return Err(AuthError::AccountLocked { locked_until });
        }
    }

    // 7. Mint — an ordinary session token, visible in `fauna.sessions.list`,
    //    tagged with the renewal device key that minted it so device deletion
    //    can revoke exactly these sessions and the sessions list can label
    //    them (sync-agent.md § Credential model — device revocation).
    let actor_id = fauna_core::identity::ActorId(actor_bytes);
    let crate::token_store::MintedToken { token, token_id } = state
        .auth
        .token_store
        .insert_with_metadata(actor_id, TOKEN_TTL_SECS, None, Some(device_key_bytes))
        .await;

    // 8. Re-read the revocation memory, AFTER the insert — the close of the
    //    window. Step 3b's observation is stale by the time we get
    //    here: three DB round-trips sit between them, each taking and releasing
    //    the `conn` mutex `fauna.sync.devices.delete` needs, so a revocation
    //    can commit its tombstone and finish its token sweep entirely inside
    //    that gap. The sweep would then have run before this token existed, and
    //    `TokenStore::validate` consults only `expires_at` — so without this
    //    the user's revocation gesture returns having left a full-actor bearer
    //    alive for its whole TTL, against `sync-agent.md` § Credential model's
    //    "ends every authority the grant conferred, in one gesture".
    //
    //    ⚠ **Ordering is what makes this TOTAL, and it must stay after the
    //    insert.** `devices_delete_handler` commits the tombstone strictly
    //    before it sweeps, so exactly one of two things is true: either the
    //    tombstone is already visible to this read — and we remove the token we
    //    just minted — or it commits after this read, in which case its sweep
    //    necessarily runs after our insert and removes the token for us. Moving
    //    this check before the insert re-opens the gap it closes.
    //
    //    ⚠ **The rejected alternative was holding the `conn` lock across the
    //    mint**, which is cheaper-looking and wrong: it coerces an unrelated
    //    mutex into an ordering primitive, so every future edit to either path
    //    silently depends on a lock neither one names.
    //
    //    A DB error here fails closed for the same reason 3b does: the token is
    //    dropped and the caller gets the opaque refusal, never a bearer minted
    //    under an unknown revocation state.
    let revoked_after_mint = state
        .db
        .is_device_grant_revoked(&actor_bytes, &device_key_bytes)
        .await;
    match revoked_after_mint {
        Ok(false) => {}
        Ok(true) => {
            state.auth.token_store.drop_unissued_token(&token_id).await;
            tracing::info!(
                target: "auth",
                "a device-grant mint raced its own revocation; the token was dropped"
            );
            return Err(AuthError::NotRegistered("no renewal grant".into()));
        }
        Err(e) => {
            state.auth.token_store.drop_unissued_token(&token_id).await;
            return Err(AuthError::Internal(e.to_string()));
        }
    }

    Ok(TokenMint {
        token,
        token_id,
        expires_at: now_ms / 1000 + TOKEN_TTL_SECS,
        expires_in: TOKEN_TTL_SECS,
    })
}

/// The custody-handshake verification body — the mint half of the nest
/// custody door (`account-data-plane.md` § Replica posture → *The
/// custody grant + ceremony*).
///
/// The device-handshake shape, custody-shaped: PoP by the CUSTODIAN key +
/// the self-contained witness + the LIVE capability row. The minted
/// bearer's actor is `custodian_key` itself — never the owner — so the
/// permission allowlist confines the session to `CallerClass::Custodian`'s
/// one kind, and AUTHORIZATION is re-derived from the row on every request
/// (this ceremony authenticates; it stores no session verdict).
///
/// Refusal opacity mirrors the device path: an unknown/revoked grant, a
/// mismatched holder and a dead window all answer the same
/// `NotRegistered("no custody grant")` — a probing custodian learns nothing
/// a never-granted one would not.
pub async fn custody_auth_core(
    state: &Arc<AppState>,
    owner_actor_id_hex: &str,
    custodian_key_hex: &str,
    timestamp_ms: u64,
    signature_hex: &str,
    client_nonce: &[u8],
    witness: &fauna_core::encoding::EmbedAsBytes,
    nest_id_hex: &str,
) -> Result<TokenMint, AuthError> {
    // 1. Parse identities; the blob must be addressed to THIS nest
    //    (`login.md` § Binding the nest).
    let owner_bytes =
        parse_actor_id(owner_actor_id_hex).ok_or(AuthError::InvalidRequest("owner_actor_id"))?;
    let custodian_bytes =
        parse_actor_id(custodian_key_hex).ok_or(AuthError::InvalidRequest("custodian_key"))?;
    let own_nest = require_own_nest(state, nest_id_hex)?;

    // 2. Timestamp freshness — the direct-auth window.
    let now_ms = fauna_core::data::Timestamp::now_millis();
    if timestamp_ms.abs_diff(now_ms) > MAX_TIMESTAMP_DRIFT_MS {
        return Err(AuthError::TimestampDrift);
    }

    // 3. The witness — self-contained, verified against nothing but the
    //    named owner (key-binding + owner + expiry + envelope, the four
    //    seam rules). Before any DB read so an invalid witness conscripts
    //    nothing; opaque so shape probing learns nothing.
    let admission = fauna_core::custody_grant::verify_custody_witness(
        witness,
        &custodian_bytes,
        &fauna_core::identity::ActorId(owner_bytes),
        fauna_core::data::Timestamp(now_ms.saturating_mul(1000)),
    )
    .map_err(|_| AuthError::NotRegistered("no custody grant".into()))?;

    // 4. The LIVE capability row — T13's nest-side revocation store. The
    //    row must exist for exactly (owner, witness.grant_id), be
    //    custody-class, name this holder, and be within its window.
    let row = state
        .db
        .get_capability_grant(&owner_bytes, &admission.grant_id)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?
        .ok_or_else(|| AuthError::NotRegistered("no custody grant".into()))?;
    let blob = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&row)
        .map_err(|_| AuthError::NotRegistered("no custody grant".into()))?;
    let now_secs = now_ms / 1000;
    let is_custody = blob
        .scope
        .iter()
        .all(|t| t.class == fauna_mls::wrapped_blob::ScopeTuple::CLASS_CUSTODY)
        && !blob.scope.is_empty();
    // The window is BOTH bounds, via the one helper every custody-authorization
    // site shares (the storage filter can only express "not
    // expired", so each decoder must ask about the start bound itself).
    if !is_custody
        || blob.holder.as_slice() != custodian_bytes.as_slice()
        || !fauna_mls::wrapped_blob::grant_window_is_open(
            &blob,
            i64::try_from(now_secs).unwrap_or(i64::MAX),
        )
    {
        return Err(AuthError::NotRegistered("no custody grant".into()));
    }

    // 5. The PoP — the domain-tagged signature by the custodian key.
    let sig_bytes = match hex::decode(signature_hex) {
        Ok(b) if b.len() == 64 => b,
        _ => return Err(AuthError::InvalidRequest("signature")),
    };
    let signature =
        Signature::from_slice(&sig_bytes).map_err(|_| AuthError::InvalidRequest("signature"))?;
    let msg = fauna_protocol::auth::custody_handshake_signed_message(
        &owner_bytes,
        &custodian_bytes,
        timestamp_ms,
        &own_nest,
        client_nonce,
    );
    if !fauna_core::identity::verify_detached(&custodian_bytes, &msg, &sig_bytes) {
        return Err(AuthError::SignatureFailed);
    }

    // 5b. Replay guard — single-use signatures, the shared map.
    if !state
        .auth
        .replay_guard
        .check_and_record(&signature.to_bytes(), now_ms, MAX_TIMESTAMP_DRIFT_MS)
        .await
    {
        return Err(AuthError::SignatureFailed);
    }

    // 6. Owner-level gates: the custodied account must be registered, live
    //    and un-superseded — a custody grant of a succeeded identity died
    //    with it (the succession sweep re-mints under the successor).
    refuse_if_superseded(state, &owner_bytes).await?;
    state
        .db
        .check_actor_active(&owner_bytes)
        .await
        .map_err(|e| AuthError::NotRegistered(e.to_string()))?;

    // 7. Mint — an ordinary session token whose ACTOR IS THE CUSTODIAN KEY
    //    (never the owner: the allowlist and the per-request row re-check
    //    are the whole authority model). Tagged with the custodian key so
    //    session listings label it.
    let actor_id = fauna_core::identity::ActorId(custodian_bytes);
    let crate::token_store::MintedToken { token, token_id } = state
        .auth
        .token_store
        .insert_with_metadata(actor_id, TOKEN_TTL_SECS, None, Some(custodian_bytes))
        .await;

    Ok(TokenMint {
        token,
        token_id,
        expires_at: now_secs + TOKEN_TTL_SECS,
        expires_in: TOKEN_TTL_SECS,
    })
}

/// The test-only rendezvous [`device_auth_core`] step 3c offers, so the
/// mint-versus-revocation window can be entered deterministically.
///
/// Two one-shot signals rather than one: the mint must both *announce* that it
/// cleared the revocation check and *wait* to be let through, and a single
/// `Notify` cannot express both without the test guessing which side it woke.
/// `Notify::notify_one` stores a permit when nobody is waiting, so neither side
/// can miss the other by arriving first.
#[cfg(test)]
pub(crate) mod mint_race {
    use tokio::sync::Notify;

    #[derive(Default)]
    pub(crate) struct Barrier {
        /// Raised by the mint once step 3b has observed "not revoked".
        pub(crate) cleared_revocation_check: Notify,
        /// Raised by the test once the whole revocation has run.
        pub(crate) may_mint: Notify,
    }

    tokio::task_local! {
        pub(crate) static BARRIER: std::sync::Arc<Barrier>;
    }
}

#[cfg(test)]
mod device_auth_tests {
    //! `device_auth_core` — the renewal-grant mint (`fauna.auth.device_handshake`,
    //! sync-agent.md § Credential model).
    use super::*;
    use crate::db::CacheDb;
    use ed25519_dalek::Signer;
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::identity::ActorKeypair;

    async fn test_state() -> Arc<AppState> {
        Arc::new(AppState::for_test(Arc::new(
            CacheDb::open_in_memory().unwrap(),
        )))
    }

    fn now_ms() -> u64 {
        fauna_core::data::Timestamp::now_millis()
    }

    /// Register a user + sync device + a `RenewBearer` grant over a fresh
    /// renewal device keypair; returns the renewal signing keypair.
    async fn seed_grant(
        state: &Arc<AppState>,
        identity: &ActorKeypair,
        capabilities: Vec<Capability>,
        expires_at: Option<Timestamp>,
    ) -> ActorKeypair {
        let actor = identity.actor_id().0;
        state.db.create_user(&actor, "free", "test").await.unwrap();
        let device_id = [0xD1u8; 32];
        state
            .db
            .register_sync_device(&actor, &device_id, "agent", None, "read,write")
            .await
            .unwrap();
        let renewal = ActorKeypair::generate();
        let auth = DeviceAuthorization {
            actor_id: fauna_core::identity::ActorId(actor),
            device_key: renewal.actor_id().0,
            capabilities,
            created_at: Timestamp::now(),
            expires_at,
        };
        let wire = fauna_core::encoding::sign_and_pack(identity, &auth).unwrap();
        assert_eq!(
            state
                .db
                .set_sync_device_grant(&actor, &device_id, &renewal.actor_id().0, &wire)
                .await
                .unwrap(),
            crate::db::GrantStoreOutcome::Stored
        );
        renewal
    }

    /// A well-formed device-handshake attempt with the given keys; returns the
    /// mint result.
    async fn attempt(
        state: &Arc<AppState>,
        actor: &[u8; 32],
        device_key: &[u8; 32],
        signer: &ActorKeypair,
        nonce: &[u8],
    ) -> Result<TokenMint, AuthError> {
        let ts = now_ms();
        let nest = state.bound_identity();
        let msg = fauna_protocol::auth::device_handshake_signed_message(
            actor, device_key, ts, &nest, nonce,
        );
        let sig = signer.signing_key().sign(&msg);
        device_auth_core(
            state,
            &hex::encode(actor),
            &hex::encode(device_key),
            ts,
            &hex::encode(sig.to_bytes()),
            nonce,
            &hex::encode(nest),
        )
        .await
    }

    /// Hold a `device_auth_core` call inside the window — between
    /// step 3b's "not revoked" observation and the mint, three DB round-trips
    /// wide — run an entire revocation through the real
    /// `fauna.sync.devices.delete` handler, then let the mint finish.
    ///
    /// ⚠ **The revocation runs through the REAL handler, not through a
    /// re-statement of its two steps here.** The fix's totality argument rests
    /// on that handler's *ordering* — tombstone commit strictly before token
    /// sweep — so a test that hand-rolled the steps would keep passing after an
    /// edit that reordered production and silently stop being about the
    /// property it names.
    async fn race_a_delete_against_a_mint() -> (
        Arc<AppState>,
        fauna_core::identity::ActorId,
        Result<TokenMint, AuthError>,
    ) {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let renewal = seed_grant(&state, &identity, vec![Capability::RenewBearer], None).await;
        let actor = identity.actor_id().0;
        let renewal_key = renewal.actor_id().0;

        let mut b = crate::rpc_router::RpcRouter::builder();
        crate::sync_handlers::register_sync_handlers(&mut b);
        let router = b.build();

        let barrier = std::sync::Arc::new(mint_race::Barrier::default());

        // spawn-ok(test): the held mint, joined below before this helper
        // returns — no caller can finish without it. (The guard walks `src/`
        // and does not skip `#[cfg(test)]` regions, deliberately: the
        // ruling that a region detector which over-claims by one item silently
        // exempts a production spawn.)
        let minting = tokio::spawn(mint_race::BARRIER.scope(std::sync::Arc::clone(&barrier), {
            let state = Arc::clone(&state);
            async move { attempt(&state, &actor, &renewal_key, &renewal, b"race").await }
        }));

        // The mint has observed "not revoked" and is parked in the window.
        barrier.cleared_revocation_check.notified().await;

        let meta = router
            .kind_meta("fauna.sync.devices.delete")
            .expect("kind registered");
        let payload = bytes::Bytes::from(
            fauna_protocol::encode_canonical(&fauna_protocol::sync::SyncDeviceDeleteRequest {
                device_id: hex::encode([0xD1u8; 32]),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        (meta.handler)(Arc::clone(&state), actor, payload)
            .await
            .expect("the device deletes");

        barrier.may_mint.notify_one();
        let outcome = minting.await.expect("the mint task joins");
        (state, fauna_core::identity::ActorId(actor), outcome)
    }

    /// The invariant both windows must satisfy, asserted as the PROPERTY rather
    /// than as an error shape: whether the mint refuses or merely drops the
    /// token it inserted is an implementation choice, but "no live bearer
    /// survives the delete" is the ratified sentence and is what must hold.
    async fn assert_no_bearer_survived(
        state: &Arc<AppState>,
        actor_id: &fauna_core::identity::ActorId,
        outcome: &Result<TokenMint, AuthError>,
    ) {
        if let Ok(minted) = outcome {
            assert!(
                state
                    .auth
                    .token_store
                    .validate(&minted.token)
                    .await
                    .is_none(),
                "the raced mint returned a bearer that still validates after the \
                 user's revocation gesture completed"
            );
        }
        assert!(
            state
                .auth
                .token_store
                .list_sessions(actor_id)
                .await
                .is_empty(),
            "a session survived a delete that returned before it existed — \
             `sync-agent.md` § Credential model promises the gesture ends every \
             authority the grant conferred, in one gesture"
        );
    }

    /// **a mint that cleared the revocation check before the delete
    /// committed must not leave a live bearer behind it.**
    ///
    /// `sync-agent.md` § Credential model ratifies that deleting a device ends
    /// *every* authority the grant conferred "in one gesture". Step 3b's check
    /// and step 7's mint have three DB round-trips between them, and
    /// `devices_delete_handler` commits the tombstone *then* sweeps the token
    /// store — so a mint that clears 3b before the commit and inserts after the
    /// sweep lands a full-actor bearer, tagged to a key the user just revoked,
    /// good for its whole `TOKEN_TTL_SECS`. `TokenStore::validate` consults only
    /// `expires_at`, so nothing downstream catches it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_mint_racing_its_own_revocation_leaves_no_live_bearer() {
        let (state, actor_id, outcome) = race_a_delete_against_a_mint().await;
        assert_no_bearer_survived(&state, &actor_id, &outcome).await;
    }

    /// **Step 8 must sit AFTER the insert — a SOURCE guard, and the reason it
    /// has to be one is worth not re-deriving.**
    ///
    /// Moving the re-read before the mint ("check, then mint") is the tidier
    /// -looking edit and it is wrong: it closes the wide window the test above
    /// covers while leaving a narrow one where the tombstone commits between
    /// the check and the insert, so the delete's sweep runs before the token
    /// exists. The after-the-insert placement is TOTAL instead, because the
    /// delete commits its tombstone strictly before it sweeps.
    ///
    /// ⚠ **That property cannot be pinned behaviourally, and a behavioural test
    /// claiming to was written and deleted before this one landed.** The
    /// residual window a pre-insert check leaves is `[re-read, insert]` — a gap
    /// that exists ONLY in the mutated code, so no barrier placed in production
    /// can park a delete inside it. A test that tried parked at the seam *after*
    /// the insert instead, where the delete's own sweep catches the token in
    /// both variants: it passed against the correct code, against the reordered
    /// code, and against no re-read at all. Vacuous by this crate's own
    /// standard.
    ///
    /// So the ordering is asserted where it is actually decidable: in the
    /// source. Same instrument as this crate's other order-and-shape rulings
    /// (the additive-column comma guard, the mail-toggle class guard, the
    /// generation-scope spawn walk), and for the same reason — a behavioural
    /// pin cannot see a difference that only shows up in an interleaving the
    /// correct code has no state for.
    ///
    /// ⚠ **The exactly-2 count below is ALSO a belt guard**,
    /// not only this test's own non-vacuity check — a fully-deleted 3b drops
    /// the count to 1 (mutation-verified). It is a weaker instrument than
    /// `step_3b_refuses_before_the_barrier_ever_sees_a_tombstoned_key`
    /// (which isolates 3b's own early return
    /// directly and also catches a full deletion — 3c is unconditional, so
    /// it is reached either way): a mutation that neuters 3b while leaving
    /// the call textually present (`&& false`) keeps this count at 2 and
    /// stays green, which is exactly what the barrier test exists to catch.
    /// `a_tombstoned_key_never_mints_even_with_its_grant_row_intact` stays
    /// green against BOTH mutations, since step 8 independently catches the
    /// same tombstone from the caller's side either way (verified by
    /// mutation) — it pins the combined guarantee, not either check alone.
    #[test]
    fn the_revocation_re_read_stays_after_the_mint() {
        let src = include_str!("auth_core.rs");
        let after_sig = src
            .split_once("pub async fn device_auth_core(")
            .expect("device_auth_core is in this file")
            .1;
        // ⚠ Bound the haystack to the FUNCTION, not to the rest of the file.
        // Without this the search runs on past the function into this very
        // test module, where the needle appears in the `find` call and in the
        // assert message below — and the guard reports green by matching
        // itself. `state.rs`'s spawn walk records the same accident; it is the
        // default failure mode of a source guard, not an exotic one.
        let end = after_sig
            .find("\n}\n")
            .expect("device_auth_core ends with a column-0 brace");
        let body = &after_sig[..end];
        let mint_at = body
            .find(".insert_with_metadata(")
            .expect("step 7 mints through insert_with_metadata");
        let re_read_at = body[mint_at..]
            .find(".is_device_grant_revoked(")
            .map(|i| i + mint_at)
            .expect(
                "step 8's `is_device_grant_revoked` re-read must appear AFTER the \
                 `insert_with_metadata` mint — a pre-insert check leaves the \
                 `[re-read, insert]` window open, and the delete's sweep runs \
                 before the token exists there",
            );
        assert!(
            re_read_at > mint_at,
            "the post-insert re-read moved above the mint"
        );
        // Non-vacuity: the needle this guard hunts must genuinely be absent
        // before the mint inside this function, or a later edit that added a
        // *second* pre-insert read would slip past on the post-insert one.
        // (3b's own check sits above `device_auth_core`'s mint too, so the
        // count is what distinguishes "3b plus step 8" from "3b plus a new
        // pre-insert read plus step 8".)
        assert_eq!(
            body.matches(".is_device_grant_revoked(").count(),
            2,
            "device_auth_core must read the revocation memory exactly twice — \
             once at 3b and once after the mint; a third read is either a \
             re-opened window or a duplicate to fold"
        );
    }

    #[tokio::test]
    async fn renewal_grant_mints_a_bearer() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let renewal = seed_grant(&state, &identity, vec![Capability::RenewBearer], None).await;
        let actor = identity.actor_id().0;
        let mint = attempt(&state, &actor, &renewal.actor_id().0, &renewal, b"nonce-1")
            .await
            .expect("grant-backed handshake mints");
        // The minted token is an ordinary session token.
        assert!(!mint.token.is_empty());
        assert!(mint.expires_at > now_ms() / 1000);
        assert!(
            state.auth.token_store.validate(&mint.token).await.is_some(),
            "minted token must validate in the session store"
        );
    }

    #[tokio::test]
    async fn no_grant_is_refused() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let actor = identity.actor_id().0;
        state.db.create_user(&actor, "free", "test").await.unwrap();
        // A device key with no stored grant — even a valid self-signature fails.
        let rogue = ActorKeypair::generate();
        let Err(err) = attempt(&state, &actor, &rogue.actor_id().0, &rogue, b"nonce-2").await
        else {
            panic!("no grant must refuse")
        };
        assert!(matches!(err, AuthError::NotRegistered(_)));
    }

    #[tokio::test]
    async fn wrong_signer_is_refused() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let renewal = seed_grant(&state, &identity, vec![Capability::RenewBearer], None).await;
        let actor = identity.actor_id().0;
        // Signed by a different key than the granted one.
        let imposter = ActorKeypair::generate();
        let Err(err) = attempt(&state, &actor, &renewal.actor_id().0, &imposter, b"nonce-3").await
        else {
            panic!("wrong signer must refuse")
        };
        assert!(matches!(err, AuthError::SignatureFailed));
    }

    #[tokio::test]
    async fn non_renewal_grant_is_refused() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        // A grant that carries Post (not RenewBearer / All) cannot renew.
        let renewal = seed_grant(&state, &identity, vec![Capability::Post], None).await;
        let actor = identity.actor_id().0;
        let Err(err) = attempt(&state, &actor, &renewal.actor_id().0, &renewal, b"nonce-4").await
        else {
            panic!("non-renewal grant must refuse")
        };
        assert!(matches!(err, AuthError::NotRegistered(_)));
    }

    #[tokio::test]
    async fn expired_grant_is_refused() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        // `Timestamp` is microseconds; 1s past the epoch is long expired.
        let renewal = seed_grant(
            &state,
            &identity,
            vec![Capability::RenewBearer],
            Some(Timestamp(1_000_000)),
        )
        .await;
        let actor = identity.actor_id().0;
        let Err(err) = attempt(&state, &actor, &renewal.actor_id().0, &renewal, b"nonce-5").await
        else {
            panic!("expired grant must refuse")
        };
        assert!(matches!(err, AuthError::NotRegistered(_)));
    }

    /// The nest binding on the device handshake (`login.md` § Binding the
    /// nest): a genuine grant's signature addressed to ANOTHER nest — what a
    /// relaying nest forwards — is refused as a verdict about its target, and
    /// re-addressed it fails the signature (the message is built over the
    /// box's own identity, never the request's).
    #[tokio::test]
    async fn a_device_handshake_addressed_to_another_nest_is_refused() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let renewal = seed_grant(&state, &identity, vec![Capability::RenewBearer], None).await;
        let actor = identity.actor_id().0;
        let device_key = renewal.actor_id().0;
        let other_nest = [0x5fu8; 32];
        assert_ne!(other_nest, state.bound_identity());
        let ts = now_ms();
        let nonce = b"nonce-7";
        let msg = fauna_protocol::auth::device_handshake_signed_message(
            &actor,
            &device_key,
            ts,
            &other_nest,
            nonce,
        );
        let sig_hex = hex::encode(renewal.signing_key().sign(&msg).to_bytes());
        let Err(err) = device_auth_core(
            &state,
            &hex::encode(actor),
            &hex::encode(device_key),
            ts,
            &sig_hex,
            nonce,
            &hex::encode(other_nest),
        )
        .await
        else {
            panic!("a device handshake addressed to another nest must not mint")
        };
        assert!(
            matches!(err, AuthError::InvalidRequest(_)),
            "a verdict about the target, before signature work: {err:?}"
        );
        let Err(err) = device_auth_core(
            &state,
            &hex::encode(actor),
            &hex::encode(device_key),
            ts,
            &sig_hex,
            nonce,
            &hex::encode(state.bound_identity()),
        )
        .await
        else {
            panic!("re-addressed: the signature still names the other nest")
        };
        assert!(matches!(err, AuthError::SignatureFailed), "{err:?}");
    }

    #[tokio::test]
    async fn replay_of_the_same_signature_is_refused() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let renewal = seed_grant(&state, &identity, vec![Capability::RenewBearer], None).await;
        let actor = identity.actor_id().0;
        let device_key = renewal.actor_id().0;
        // Build one signed request and present it twice verbatim.
        let ts = now_ms();
        let nonce = b"nonce-6";
        let nest = state.bound_identity();
        let msg = fauna_protocol::auth::device_handshake_signed_message(
            &actor,
            &device_key,
            ts,
            &nest,
            nonce,
        );
        let sig = renewal.signing_key().sign(&msg);
        let sig_hex = hex::encode(sig.to_bytes());
        let first = device_auth_core(
            &state,
            &hex::encode(actor),
            &hex::encode(device_key),
            ts,
            &sig_hex,
            nonce,
            &hex::encode(nest),
        )
        .await;
        assert!(first.is_ok(), "first use mints");
        let second = device_auth_core(
            &state,
            &hex::encode(actor),
            &hex::encode(device_key),
            ts,
            &sig_hex,
            nonce,
            &hex::encode(nest),
        )
        .await;
        assert!(
            matches!(second, Err(AuthError::SignatureFailed)),
            "verbatim replay must be refused (§ L4 single-use)"
        );
    }

    #[tokio::test]
    async fn deleting_the_device_revokes_the_grant() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let renewal = seed_grant(&state, &identity, vec![Capability::RenewBearer], None).await;
        let actor = identity.actor_id().0;
        // The revocation path: fauna.sync.devices.delete removes the row.
        let (deleted, _, _) = state.db.delete_device(&[0xD1u8; 32], &actor).await.unwrap();
        assert!(deleted);
        let Err(err) = attempt(&state, &actor, &renewal.actor_id().0, &renewal, b"nonce-7").await
        else {
            panic!("deleted device's grant must refuse")
        };
        assert!(matches!(err, AuthError::NotRegistered(_)));
    }

    /// Behavioural coverage for "a tombstoned key
    /// never mints" against a grant row that reappeared some other way than
    /// `delete_device` (`is_device_grant_revoked`'s own doc comment names
    /// exactly this case) — every existing revocation test, including
    /// `deleting_the_device_revokes_the_grant` above, revokes via
    /// `delete_device`, which deletes the grant row *along with* writing the
    /// tombstone, so a post-delete mint attempt already refuses at step 3's
    /// grant lookup and never exercises this scenario at all.
    ///
    /// ⚠ **This does NOT isolate step 3b specifically — verified by
    /// mutation, it stays green even with 3b deleted**, because step 8 (the
    /// unconditional post-mint re-read `the_revocation_re_read_stays_after_
    /// the_mint` documents) independently catches the same tombstone. That
    /// is the correct, total behaviour: from the caller's side the two
    /// checks are indistinguishable, so a behavioural test properly pins
    /// their combined guarantee, not either one alone. **3b's own early
    /// return is isolated by `step_3b_refuses_before_the_barrier_ever_sees_
    /// a_tombstoned_key` instead**, which this test's own doc comment
    /// explains further.
    #[tokio::test]
    async fn a_tombstoned_key_never_mints_even_with_its_grant_row_intact() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let renewal = seed_grant(&state, &identity, vec![Capability::RenewBearer], None).await;
        let actor = identity.actor_id().0;
        let device_key = renewal.actor_id().0;

        assert!(
            attempt(&state, &actor, &device_key, &renewal, b"nonce-9a")
                .await
                .is_ok(),
            "baseline: the grant is valid and unrevoked, so the mint succeeds"
        );

        // Tombstone the key directly, without touching the grant row —
        // `delete_device` is not called here, so `sync_devices.auth_grant`
        // stays exactly as `seed_grant` left it.
        {
            let conn = state.db.conn().await;
            conn.execute(
                "INSERT INTO revoked_device_grants (actor_id, auth_device_key, revoked_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![actor.as_slice(), device_key.as_slice(), 0i64],
            )
            .unwrap();
        }

        let Err(err) = attempt(&state, &actor, &device_key, &renewal, b"nonce-9b").await else {
            panic!(
                "a tombstoned key must never mint, even with its grant row \
                 still present and findable — neither 3b nor the register- \
                 side belt applies here, and the mint-side revocation memory \
                 (3b, step 8, or both) must catch it"
            )
        };
        assert!(matches!(err, AuthError::NotRegistered(_)));
    }

    /// A mutation severing 3b with `&& false` must red, and red
    /// *only* that pin. Neither the test above nor the "exactly 2" source
    /// guard satisfies that — both stay green against `&& false`, because
    /// the call is still textually present and step 8 still catches the same
    /// tombstone behaviourally, so a plain "does the mint refuse" assertion
    /// cannot tell 3b's own early return apart from step 8's late one.
    ///
    /// This exploits the same `mint_race` barrier row 135 built, but for the
    /// OTHER direction: step 3c (`BARRIER.try_with`) sits immediately after
    /// 3b and fires only if execution falls through 3b's early return. So
    /// "the barrier is never notified" is observable proof that 3b itself
    /// refused — independent of whatever step 8 would have done if reached.
    #[tokio::test(flavor = "multi_thread")]
    async fn step_3b_refuses_before_the_barrier_ever_sees_a_tombstoned_key() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let renewal = seed_grant(&state, &identity, vec![Capability::RenewBearer], None).await;
        let actor = identity.actor_id().0;
        let device_key = renewal.actor_id().0;

        {
            let conn = state.db.conn().await;
            conn.execute(
                "INSERT INTO revoked_device_grants (actor_id, auth_device_key, revoked_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![actor.as_slice(), device_key.as_slice(), 0i64],
            )
            .unwrap();
        }

        let barrier = std::sync::Arc::new(mint_race::Barrier::default());
        // spawn-ok(test): joined by the `tokio::select!` below, inside one test.
        let mut minting = tokio::spawn(mint_race::BARRIER.scope(std::sync::Arc::clone(&barrier), {
            let state = Arc::clone(&state);
            async move { attempt(&state, &actor, &device_key, &renewal, b"nonce-barrier").await }
        }));

        tokio::select! {
            _ = barrier.cleared_revocation_check.notified() => {
                // 3b let a tombstoned key fall through to 3c — let the parked
                // task finish (it needs `may_mint` or it hangs forever) before
                // failing, so the spawned task doesn't outlive the test.
                barrier.may_mint.notify_one();
                let _ = (&mut minting).await;
                panic!(
                    "step 3b must refuse a tombstoned key BEFORE step 3c's \
                     barrier — it fell through instead, which means 3b's own \
                     early return is gone or bypassed, even though step 8 \
                     would still have caught it downstream"
                );
            }
            result = &mut minting => {
                let outcome = result.expect("mint task joins");
                assert!(
                    matches!(outcome, Err(AuthError::NotRegistered(_))),
                    "3b's early return must produce the opaque no-grant \
                     refusal, matching the belt's own comment"
                );
            }
        }
    }

    #[tokio::test]
    async fn suspended_actor_cannot_mint_via_grant() {
        let state = test_state().await;
        let identity = ActorKeypair::generate();
        let renewal = seed_grant(&state, &identity, vec![Capability::RenewBearer], None).await;
        let actor = identity.actor_id().0;
        state
            .db
            .suspend_user_now(&actor, "suspended by admin", "other")
            .await
            .unwrap();
        let Err(err) = attempt(&state, &actor, &renewal.actor_id().0, &renewal, b"nonce-8").await
        else {
            panic!("suspended actor's agent must not mint (F4 mint half)")
        };
        assert!(matches!(err, AuthError::NotRegistered(_)));
    }
}

#[cfg(test)]
mod replay_guard_tests {
    //! `ReplayGuard` — the § L4 direct-auth single-use signature guard.
    use super::{MAX_TIMESTAMP_DRIFT_MS, ReplayGuard};

    #[tokio::test]
    async fn first_use_records_and_replay_is_rejected() {
        let guard = ReplayGuard::new();
        let sig = [7u8; 64];
        let now = 1_000_000;
        assert!(
            guard
                .check_and_record(&sig, now, MAX_TIMESTAMP_DRIFT_MS)
                .await,
            "first use of a signature must be accepted"
        );
        assert!(
            !guard
                .check_and_record(&sig, now, MAX_TIMESTAMP_DRIFT_MS)
                .await,
            "an immediate replay of the same signature must be rejected"
        );
    }

    #[tokio::test]
    async fn distinct_signatures_are_independent() {
        let guard = ReplayGuard::new();
        let now = 1_000_000;
        assert!(
            guard
                .check_and_record(&[1u8; 64], now, MAX_TIMESTAMP_DRIFT_MS)
                .await
        );
        assert!(
            guard
                .check_and_record(&[2u8; 64], now, MAX_TIMESTAMP_DRIFT_MS)
                .await,
            "a different signature is not a replay"
        );
    }

    #[tokio::test]
    async fn entry_expires_after_ttl_so_signature_is_reusable() {
        let guard = ReplayGuard::new();
        let sig = [9u8; 64];
        assert!(
            guard
                .check_and_record(&sig, 1_000_000, MAX_TIMESTAMP_DRIFT_MS)
                .await
        );
        // Past 2x the drift window the entry is swept; the (now stale-timestamp)
        // signature would in practice be rejected by the timestamp check, but
        // the guard itself no longer holds it.
        assert!(
            guard
                .check_and_record(
                    &sig,
                    1_000_000 + 2 * MAX_TIMESTAMP_DRIFT_MS + 1,
                    MAX_TIMESTAMP_DRIFT_MS
                )
                .await,
            "after 2x the window the swept signature is no longer flagged as a replay"
        );
    }

    /// Row 525: a blob signed with timestamp `T` is FRESH for
    /// `now ∈ [T - W, T + W]` (the ± drift window). The earliest legitimate
    /// use is at `T - W`. Remembering the signature for only `1×` the window
    /// expires the entry at exactly `T`, leaving `now ∈ [T, T + W]` replayable
    /// while the blob is still fresh — this test drives that gap at the
    /// midpoint (`T + W/2`), where a captured, verbatim-replayed signature
    /// must still be rejected.
    #[tokio::test]
    async fn future_dated_signature_stays_replay_protected_past_one_window() {
        let guard = ReplayGuard::new();
        let sig = [11u8; 64];
        let window = MAX_TIMESTAMP_DRIFT_MS;
        let t = 1_000_000u64; // the blob's own signed timestamp
        let earliest_legit_now = t - window; // T - W
        assert!(
            guard
                .check_and_record(&sig, earliest_legit_now, window)
                .await,
            "first use at the earliest legitimate moment must be accepted"
        );

        let mid_window_replay = t + window / 2; // T + W/2 — still fresh
        assert!(
            !guard
                .check_and_record(&sig, mid_window_replay, window)
                .await,
            "a replay while the blob is still within its ±window must be \
             rejected, even though the entry was recorded a full window ago"
        );
    }
}
