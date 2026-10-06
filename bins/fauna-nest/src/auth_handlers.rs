//! Pre-identity auth-bootstrap WS-RPC handlers —
//! `fauna.auth.{handshake,challenge,verify}`. A behavior-preserving transport
//! migration of the HTTP routes `POST /api/v1/auth/{token,challenge,verify}`;
//! the ceremony itself lives in the shared `auth_core` (the HTTP twins call the
//! same fns). These kinds run **only** on the anonymous WS connection
//! (`GET /api/v1/ws`, no bearer) — enforced by `pre_identity_allowlist` +
//! the dispatcher gate in `routes::dispatch_request`. Track A1 of
//! the WS-RPC-everywhere migration (tracked internally).
//!
//! The connection's bearer-actor is irrelevant here (there is none) — each
//! handler authenticates the actor from the **request payload** via the signed
//! message, exactly as the HTTP twins did. The dispatcher's `actor_id`
//! argument is therefore ignored.
//!
//! Error codes (`fauna.auth.*`): `invalid_request`, `timestamp_drift`,
//! `signature_failed`, `not_registered`, `account_locked` (detail =
//! `locked_until` Unix seconds), `invalid_nonce`; malformed payloads and
//! server faults reuse the `fauna.protocol.*` infra codes.

use std::time::Duration;

use fauna_protocol::auth::{
    CLIENT_NONCE_LEN, CertBinding, ChallengeReply, ChallengeRequest, HandshakeReply,
    HandshakeRequest, NestHandshakeReply, NestHandshakeRequest, VerifyReply, VerifyRequest,
};
use fauna_protocol::nest_rotation::{
    ROTATION_CHAIN_KIND, RotationChainReply, RotationChainRequest,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode};

use crate::auth_core::{self, AuthError};
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, internal, malformed};

/// Map the transport-agnostic `AuthError` to a `fauna.auth.*` (or infra)
/// `RpcError`. This is now the sole `AuthError` mapping — every HTTP auth twin
/// (challenge/verify and, at the endgame, the `/auth/token` bearer-bootstrap)
/// has been deleted, so the shared `auth_core` is reached only over WS-RPC.
fn auth_error_to_rpc(e: AuthError) -> RpcError {
    match e {
        AuthError::InvalidRequest(field) => {
            let mut r = RpcError::new("fauna.auth.invalid_request", "error.auth.invalid_request");
            r.details = Some(Box::new(Value::String(format!("invalid {field}"))));
            r
        }
        AuthError::TimestampDrift => {
            RpcError::new("fauna.auth.timestamp_drift", "error.auth.timestamp_drift")
        }
        AuthError::SignatureFailed => {
            RpcError::new("fauna.auth.signature_failed", "error.auth.signature_failed")
        }
        AuthError::NotRegistered(msg) => {
            let mut r = RpcError::new("fauna.auth.not_registered", "error.auth.not_registered");
            r.details = Some(Box::new(Value::String(msg)));
            r
        }
        AuthError::AccountLocked { locked_until } => {
            let mut r = RpcError::new("fauna.auth.account_locked", "error.auth.account_locked");
            r.details = Some(Box::new(Value::Integer(locked_until as i128)));
            r
        }
        AuthError::InvalidNonce => {
            RpcError::new("fauna.auth.invalid_nonce", "error.auth.invalid_nonce")
        }
        // The one auth refusal that is deliberately *informative*: it names the
        // successor and the kind that serves the proof, because the client
        // hitting it is normally the succeeded user's own device and its next
        // step is to import that identity (`identity-succession.md` § Propagation).
        AuthError::Superseded { new_actor_id } => RpcError::superseded(&new_actor_id),
        AuthError::Internal(msg) => {
            let mut r = RpcError::new("fauna.protocol.internal", "error.protocol.internal");
            r.details = Some(Box::new(Value::String(msg)));
            r
        }
    }
}

// ── fauna.auth.handshake (≡ POST /auth/token, direct auth) ──────────────────

/// Build the TLS channel-binding proof for a handshake reply
/// (`docs/goal/architecture/security.md` § Transport trust, Axis 1). Returns
/// `Some` only when **all three** preconditions hold: the request carried a
/// `client_nonce`, the nest holds a deployment signing key, and it is serving a
/// cert whose SPKI it can read. The nest signs the SPKI of the cert **it itself
/// serves** (`state.served_cert_spki`, read live) — never a client-reported
/// value (the load-bearing subtlety: signing a client SPKI would let a MITM get
/// its own cert's SPKI signed and the binding would be worthless).
fn build_cert_binding(
    state: &AppState,
    client_nonce: Option<&[u8]>,
    kind: BindingKind,
) -> Option<CertBinding> {
    let nonce = client_nonce?;
    let signing_key = state.nest_signing_key.as_ref()?;
    let spki = state.served_cert_spki.as_ref()?.current_spki_sha256()?;
    sign_channel_binding(signing_key, &spki, nonce, kind)
}

/// Which of [`sign_channel_binding`]'s three producers is signing. A caller
/// names *which producer it is* rather than an accept set directly, so it
/// cannot pass a wrong (or merely stale) set of nonce lengths — only
/// mis-name which producer it is, which [`BindingKind::accepts`] cannot get
/// wrong without lying about its own variant.
///
/// **`CertHandshake` and `VerifyFold` are split, not one `Cert` variant**: [`build_cert_binding`]'s three bearer-minting callers
/// (`handshake`, `device_handshake`, `custody_handshake`) can only ever
/// legitimately produce a bare `client_nonce`; only the verify-path caller
/// legitimately produces the 64-byte NT-1 fold. A single `Cert` variant whose
/// accept set was the union let the three bearer-minting kinds inherit a
/// 64-byte width they could never produce themselves — the width that made a
/// genuine 96-byte verify-path message re-splittable as `spki(64) ‖
/// nonce(32)` at the *verifying* end (closed independently, and more
/// fundamentally, by the wire-side length check in
/// `fauna_client_core::nest_trust::verify_cert_binding_possession` — this
/// split is the *producer*-side half of the same fix, applying rule #8
/// guard 1's own "an accept set per producer" principle one level further).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingKind {
    /// [`build_identity_binding`]'s shape: exactly one canonical
    /// `client_nonce`. `fauna.auth.nest_handshake` folds nothing onto it.
    Handshake,
    /// [`build_cert_binding`]'s shape at the three bearer-minting handshake
    /// call sites (`handshake`, `device_handshake`, `custody_handshake`): a
    /// bare `client_nonce`, the only shape these callers can ever produce.
    CertHandshake,
    /// [`build_cert_binding`]'s shape at the verify-path call site: the bare
    /// 32-byte `challenge_nonce` for a client that contributes no nonce (one
    /// that will not consult the binding), or the NT-1 fold `challenge_nonce
    /// ‖ client_nonce` (64 bytes). Both halves are separately length-checked
    /// upstream (`auth_core::verify_core` decodes the challenge nonce as
    /// `[u8; 32]`), so 64 is the only fold width reachable.
    VerifyFold,
}

impl BindingKind {
    /// The byte lengths this producer's callers can legitimately pass —
    /// rule #8 guard 1's policy (`key-material-hierarchy.md` § Architectural
    /// rules #8), owned here rather than supplied by the caller. The match
    /// has no wildcard arm: a third producer fails to compile here until its
    /// accept set is decided.
    fn accepts(self) -> &'static [usize] {
        match self {
            BindingKind::Handshake => &[CLIENT_NONCE_LEN],
            BindingKind::CertHandshake => &[CLIENT_NONCE_LEN],
            BindingKind::VerifyFold => &[CLIENT_NONCE_LEN, 2 * CLIENT_NONCE_LEN],
        }
    }
}

/// Sign a deployment-key channel binding over `spki ‖ nonce` — the single
/// producer behind both [`build_cert_binding`] and [`build_identity_binding`],
/// and the one place rule #8's length guard (`key-material-hierarchy.md`
/// § Architectural rules #8) lives. The signature itself comes from the shared
/// `fauna_protocol::auth::CertBinding::sign`, so every fixture that stands in
/// for a nest signs byte-identical bytes.
///
/// Emits ONLY the domain-tagged signature (`sig_domain::CERT_BINDING_V1`).
/// Tagging is what makes cross-context confusion structural rather than
/// incidental (key-material-hierarchy.md § Architectural
/// rules #8). The untagged compat half a pre-tag client once verified — a
/// deployment-key signature over raw bytes, which needed a second guard
/// ("emit it only behind the nest-controlled SPKI prefix") to stay out of the
/// KeyBlob's bare-CID context — was removed 2026-09-24 by the compat-remnant
/// sweep, so that argument is no longer load-bearing: no untagged
/// channel-binding signature exists anywhere.
///
/// **Guard 1 — the nonce length is enforced, not assumed.** `kind.accepts()`
/// names the byte lengths this producer's callers can legitimately pass; any
/// other length is refused (`None`) rather than signed. The wire fields are
/// `ByteBuf`s a caller can make *any* length; bounding them at the signer,
/// per producer, is rule #8's "an accept set per producer" principle and what
/// keeps a genuine signature un-re-sliceable at the verifying end.
fn sign_channel_binding(
    signing_key: &ed25519_dalek::SigningKey,
    spki: &[u8],
    nonce: &[u8],
    kind: BindingKind,
) -> Option<CertBinding> {
    if !kind.accepts().contains(&nonce.len()) {
        return None;
    }

    Some(CertBinding::sign(signing_key, spki, nonce))
}

/// Build a **nest-identity** binding for the pre-identity nest handshake — the
/// proof the succession-anchor check consumes. Unlike [`build_cert_binding`] this
/// signs with the nest's **identity** key and tolerates the absence of a served
/// cert, so it can prove identity over the in-process/plaintext e2e path a peer
/// pull may reach as well as production TLS:
///
/// - **Key:** the reconciled `nest_signing_key` when present, else the
///   `nest_identity` signing key. In production these are the *same* deployment
///   key (`box-recovery.md` § Single-identity unification), so the emitted
///   binding is byte-identical to what [`build_cert_binding`] produced; the
///   fallback only adds capability for a keyless (test-harness) nest.
/// - **SPKI:** the served cert's SPKI when present, else empty — so the message
///   is `SPKI ‖ nonce` on TLS (relay-defeating, the caller SPKI-compares) and
///   `nonce` alone on plaintext (possession-only, which is all a loopback nest
///   with no cert can offer). The reported `nest_actor_id` is always the key's,
///   which equals the id `nest.info` reports and residue records.
///
/// `None` only when the request carried no `client_nonce`.
fn build_identity_binding(state: &AppState, client_nonce: Option<&[u8]>) -> Option<CertBinding> {
    let nonce = client_nonce?;
    let signing_key = state
        .nest_signing_key
        .clone()
        .unwrap_or_else(|| state.nest_identity.signing_key.clone());
    let spki: Vec<u8> = state
        .served_cert_spki
        .as_ref()
        .and_then(|s| s.current_spki_sha256())
        .map(|s| s.to_vec())
        .unwrap_or_default();
    sign_channel_binding(&signing_key, &spki, nonce, BindingKind::Handshake)
}

fn handshake_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: HandshakeRequest = decode(&payload).map_err(malformed)?;
            // The client address the dispatcher recorded for this connection
            // feeds new-IP `SecurityEvent` detection, as the HTTP twin's
            // `X-Forwarded-For` / TCP peer once did.
            let client_nonce = req.client_nonce.as_slice();
            let mint = auth_core::direct_auth_core(
                &state,
                &req.actor_id,
                req.timestamp,
                &req.signature,
                client_nonce,
                &req.nest_id,
                crate::dispatch_core::current_caller_ip(),
            )
            .await
            .map_err(auth_error_to_rpc)?;
            // Sign the TLS channel binding over the SPKI of the cert we serve ‖
            // the client's nonce, so the client can prove the channel terminates
            // at this nest's identity (security.md § Transport trust). Same nonce
            // that was folded into the verified auth signature above.
            let cert_binding =
                build_cert_binding(&state, Some(client_nonce), BindingKind::CertHandshake);
            encode_reply(&HandshakeReply {
                token: mint.token,
                token_id: mint.token_id,
                expires_at: mint.expires_at,
                expires_in: mint.expires_in,
                cert_binding,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.auth.device_handshake (renewal-grant bearer mint) ──────────────────

/// Mint a session bearer against a stored `RenewBearer`-scoped
/// `DeviceAuthorization` — the sync agent's app-dead renewal path
/// (`sync-agent.md` § Credential model; additive 2026-07-19). The verification
/// body is `auth_core::device_auth_core`; the reply mirrors the direct-auth
/// handshake exactly, TLS channel binding included (the agent's anonymous
/// connect to a self-signed nest needs the same graduation leg as any client's).
fn device_handshake_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: fauna_protocol::auth::DeviceHandshakeRequest =
                decode(&payload).map_err(malformed)?;
            // The failed-credential throttle, keyed `(source × claimed renewal
            // key)`: a bucket already full of this key's refusals from this
            // source is refused before the work, and every refusal spends it
            // (`transport-connection.md` § Abuse posture → *The failed-credential
            // throttle*). Only refusals fill it and a key with a live grant is
            // never refused, so a legitimate renewal is not what it stops.
            let source = crate::dispatch_core::current_caller().and_then(|c| c.peer_ip);
            let claimed = crate::routes::parse_32_bytes(&req.device_key).unwrap_or([0u8; 32]);
            let throttle = &state.failed_credential_throttle;
            let surface = crate::failed_credential_throttle::Surface::DeviceHandshake;
            if throttle.is_exhausted(surface, source, &claimed) {
                return Err(RpcError::new(
                    "fauna.protocol.rate_limited",
                    "error.protocol.rate_limited",
                ));
            }
            let mint = match auth_core::device_auth_core(
                &state,
                &req.actor_id,
                &req.device_key,
                req.timestamp,
                &req.signature,
                req.client_nonce.as_slice(),
                &req.nest_id,
            )
            .await
            {
                Ok(mint) => mint,
                Err(e) => {
                    // A server fault is not the caller's refusal.
                    if !matches!(e, AuthError::Internal(_)) {
                        throttle.note_refusal(surface, source, &claimed);
                    }
                    return Err(auth_error_to_rpc(e));
                }
            };
            let cert_binding = build_cert_binding(
                &state,
                Some(req.client_nonce.as_slice()),
                BindingKind::CertHandshake,
            );
            encode_reply(&HandshakeReply {
                token: mint.token,
                token_id: mint.token_id,
                expires_at: mint.expires_at,
                expires_in: mint.expires_in,
                cert_binding,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.auth.custody_handshake (custody-session bearer mint) ───────────────

/// Mint a CUSTODIAN session bearer against a custody-grant witness + a live
/// capability row (W8.6 (account-data-plane.md § Workstreams) — `account-data-plane.md` § Replica posture → *The
/// custody grant + ceremony*). The verification body is
/// `auth_core::custody_auth_core`; the reply mirrors the device handshake,
/// TLS channel binding included (a custodian's anonymous connect to a
/// self-signed owner nest needs the same graduation leg).
fn custody_handshake_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: fauna_protocol::auth::CustodyHandshakeRequest =
                decode(&payload).map_err(malformed)?;
            let mint = auth_core::custody_auth_core(
                &state,
                &req.owner_actor_id,
                &req.custodian_key,
                req.timestamp,
                &req.signature,
                req.client_nonce.as_slice(),
                &req.witness,
                &req.nest_id,
            )
            .await
            .map_err(auth_error_to_rpc)?;
            let cert_binding = build_cert_binding(
                &state,
                Some(req.client_nonce.as_slice()),
                BindingKind::CertHandshake,
            );
            encode_reply(&HandshakeReply {
                token: mint.token,
                token_id: mint.token_id,
                expires_at: mint.expires_at,
                expires_in: mint.expires_in,
                cert_binding,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.auth.nest_handshake (pre-identity nest-identity handshake) ─────────

/// The nest proves *its* identity over the live TLS channel before any actor
/// exists: sign a channel binding over `served_SPKI ‖ client_nonce` with the
/// deployment key and return it. No DB, no actor lookup, no side effects —
/// replay-safe by construction (the client-chosen nonce uniquifies each
/// signature; nothing is mutated). The first-contact trust leg of the
/// client-provisioned-box flow (security.md § Transport trust, Axis 2 —
/// client-provisioned row): the onboarding machine runs this as the opening
/// step of every pre-claim connection and graduates the binding against the
/// deployment seed it injected at provision. `cert_binding: None` (a plain-HTTP
/// dev nest with no served SPKI, or a keyless nest) tells the client to fall
/// back to its non-provisioned trust path.
fn nest_handshake_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: NestHandshakeRequest = decode(&payload).map_err(malformed)?;
            // The nest-identity binding (signed by the identity key, cert-
            // optional) — so a peer's succession-anchor check can prove this
            // box's identity over the in-process/plaintext path as well as TLS.
            let cert_binding = build_identity_binding(&state, Some(req.client_nonce.as_slice()));
            encode_reply(&NestHandshakeReply {
                cert_binding,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.auth.rotation_chain (the deployment-seed rotation chain) ───────────

/// Serve this box's full rotation chain (`box-recovery.md` § Client acceptance).
///
/// The one kind in this module that is not an auth ceremony: it authenticates
/// nobody, mints nothing, and reads an append-only log whose every row is a
/// signed statement meant to be walked by anyone. A client calls it at exactly
/// one moment — the identity it was presented does not match its pin — and its
/// answer decides whether that mismatch is a licensed rotation or an
/// impersonation.
///
/// ⚠ **The whole chain is served, unconditionally and unfiltered, and that is
/// the design rather than an omission.** Two properties follow from it, both
/// load-bearing. First, the caller never names the identity it holds, so the box
/// learns nothing about who is asking — a client pinned five rotations back and
/// one pinned at the head send the identical request. Second, the walk's start is
/// located caller-side (`verify_chain`), so one reply converges every client
/// however far back it is pinned; a nest-side "just the hops since X" filter
/// would buy nothing (rotations are deployment-rare) and would turn this read
/// into an oracle that confirms whether a guessed identity is in the log.
///
/// ⚠ **Serving the chain is not, and must never become, a re-pin instruction.**
/// A chain is public and replayable, so acceptance additionally requires the
/// live channel binding to prove the presenter holds the head — a check only the
/// client holding that binding can make (`nest_rotation` module docs, and
/// `box-recovery.md` § Client acceptance, condition 2). Nothing here may grow an
/// "and you should trust me" field.
fn rotation_chain_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            // Decoded, not ignored: the request is empty today, but a payload
            // that does not parse as one is a client bug worth naming rather
            // than silently serving.
            let _req: RotationChainRequest = decode(&payload).map_err(malformed)?;
            let chain = state.db.nest_rotation_chain().await.map_err(internal)?;
            // An empty chain is the honest answer for a box that never rotated,
            // never an error — see `RotationChainReply`.
            encode_reply(&RotationChainReply {
                chain,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.auth.challenge (≡ POST /auth/challenge, nonce issuance) ────────────

fn challenge_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: ChallengeRequest = decode(&payload).map_err(malformed)?;
            let issued = auth_core::issue_challenge_core(&state, &req.actor_id)
                .await
                .map_err(auth_error_to_rpc)?;
            encode_reply(&ChallengeReply {
                nonce: issued.nonce_hex,
                expires_in: issued.expires_in,
                expires_at: issued.expires_at,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.auth.verify (≡ POST /auth/verify, nonce-signed token) ──────────────

fn verify_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: VerifyRequest = decode(&payload).map_err(malformed)?;
            let mint = auth_core::verify_core(
                &state,
                &req.actor_id,
                &req.nonce,
                &req.signature,
                &req.nest_id,
                crate::dispatch_core::current_caller_ip(),
            )
            .await
            .map_err(auth_error_to_rpc)?;
            // Sign the TLS channel binding over the SPKI of the cert we serve ‖
            // the binding nonce — symmetric with the handshake path, so a client
            // that authenticates via the challenge/verify ceremony (web's launch
            // fast path) can possession-verify + TOFU-pin the nest's identity
            // (security.md § Transport trust, Axis 1). The binding nonce is the
            // challenge nonce (already validated by `verify_core` above), with the
            // client's fresh `client_nonce` appended when present — so the web
            // pin's binding rides a *client*-chosen value and a harvested binding
            // can't be replayed to a victim with a different fresh nonce (NT-1).
            // A client that will not consult the binding sends no `client_nonce`
            // → the binding signs `spki ‖ challenge_nonce`, the shape it asked
            // for. `build_cert_binding` returns `None` on a plain-HTTP dev nest
            // (no served-cert SPKI) or a keyless nest.
            let binding_nonce = hex::decode(&req.nonce).ok().map(|mut nonce| {
                if let Some(cn) = req.client_nonce.as_ref() {
                    nonce.extend_from_slice(cn);
                }
                nonce
            });
            // `Box`ed: VerifyReply.cert_binding is boxed (it rides the
            // SilentChallengeOutcome enum — see its doc); the wire is identical.
            let cert_binding =
                build_cert_binding(&state, binding_nonce.as_deref(), BindingKind::VerifyFold)
                    .map(Box::new);
            encode_reply(&VerifyReply {
                token: mint.token,
                token_id: mint.token_id,
                handle: mint.handle,
                domain: mint.domain,
                tier: mint.tier,
                expires_at: mint.expires_at,
                expires_in: mint.expires_in,
                cert_binding,
                extra: Default::default(),
            })
        })
    })
}

/// Register the silent challenge alone — `fauna.auth.nest_handshake` (the
/// opening identity read every login binds, `login.md` § Binding the nest) +
/// `fauna.auth.challenge` + `fauna.auth.verify`, the triple every app-held
/// bearer is minted over (`login.md` § When to use which). Part of
/// [`register_auth_handlers`]; on its own it is what a tier_3 witness
/// registers to prove a client mints its bearer without `fauna.auth.handshake`
/// (a nest serving only this triple refuses the handshake as an unknown kind).
pub fn register_silent_challenge_handlers(b: &mut RpcRouterBuilder) {
    let deadline = Duration::from_secs(5);
    b.add(
        fauna_protocol::auth::NEST_HANDSHAKE_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: deadline,
            handler: nest_handshake_handler(),
        },
    );
    b.add(
        "fauna.auth.challenge",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: deadline,
            handler: challenge_handler(),
        },
    );
    b.add(
        "fauna.auth.verify",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: deadline,
            handler: verify_handler(),
        },
    );
}

/// Register the pre-identity auth-bootstrap kinds. All three are replay-safe
/// at 5 s — the per-connection idempotency cache replays a retried reply
/// (same token; verify does not re-consume the spent nonce).
pub fn register_auth_handlers(b: &mut RpcRouterBuilder) {
    let deadline = Duration::from_secs(5);
    b.add(
        "fauna.auth.handshake",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: deadline,
            handler: handshake_handler(),
        },
    );
    register_silent_challenge_handlers(b);
    b.add(
        fauna_protocol::auth::DEVICE_HANDSHAKE_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: deadline,
            handler: device_handshake_handler(),
        },
    );
    b.add(
        fauna_protocol::auth::CUSTODY_HANDSHAKE_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: deadline,
            handler: custody_handshake_handler(),
        },
    );
    // Registered here — beside the pre-identity kinds — rather than with the
    // Admin-class `deployment_seed.rotate` that produces the log, because the
    // gate that matters for this kind is the anonymous door, and a reader
    // auditing that door reads this file.
    b.add(
        ROTATION_CHAIN_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: deadline,
            handler: rotation_chain_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    //! The deployment key's channel-binding signer — the *untagged* half of
    //! context A (`key-material-hierarchy.md` § Architectural rules #8).
    //!
    //! Rule #8's KeyBlob exclusion argument rests on exactly one property of
    //! this module: **no untagged deployment-key signature over a 36-byte
    //! message is ever produced.** A `SignedEnvelope` verifies as
    //! `Ed25519_verify(sig, cid.bytes, pk)` over a 36-byte dag-cbor CID
    //! (`fauna_core::encoding`), so such a signature *is* a valid envelope
    //! signature for whatever value those bytes are the CID of — and the
    //! subscription/archival KeyBlob is meant to be the sole bare-CID
    //! deployment-key context. These tests pin the two structural guards that
    //! make that true by construction rather than by chance: a fixed nonce
    //! length, and no untagged half where the message has no nest-controlled
    //! SPKI prefix.

    use super::*;
    // verify-ok(test-owned-keypair): every check below runs against
    // `key().verifying_key()` — a fixed `SigningKey::from_bytes(&[9u8; 32])`
    // this module constructs and holds BOTH halves of, at all nine call sites
    // (`let k = key()`, never a parameter, never a wire value). The permissive
    // trait's hazard is an attacker-CHOSEN key, and there is no key here an
    // attacker can reach: these tests assert the SHAPE of a signature this
    // module itself produced, so routing them through the strict primitive
    // would test the primitive instead of the shape.
    use ed25519_dalek::{SigningKey, Verifier};
    use fauna_protocol::sig_domain::{CERT_BINDING_V1, domain_separated};

    /// The 36-byte length of a dag-cbor CID (`0x01 0x71 0x1e 0x20 ‖ blake3`) —
    /// the message length an untagged deployment-key signature must never have.
    const CID_LEN: usize = 36;

    /// Every [`BindingKind`], for the exhaustive sweeps below — never a
    /// hand-maintained list a third producer could be added without joining.
    /// `_binding_kind_is_exhaustive` right below is the enforcement: its match
    /// has no wildcard arm, so a new variant fails to compile there (and,
    /// since it is right beside this array, is added here in the same edit)
    /// before it could silently go untested.
    const ALL_BINDING_KINDS: [BindingKind; 3] = [
        BindingKind::Handshake,
        BindingKind::CertHandshake,
        BindingKind::VerifyFold,
    ];

    #[allow(dead_code)]
    fn _binding_kind_is_exhaustive(k: BindingKind) {
        match k {
            BindingKind::Handshake | BindingKind::CertHandshake | BindingKind::VerifyFold => {}
        }
    }

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }

    /// Does `binding.tagged_sig` verify as a *bare, untagged* signature over
    /// `msg` — the pre-tag shape no producer may emit any more?
    fn untagged_verifies(k: &SigningKey, binding: &CertBinding, msg: &[u8]) -> bool {
        let Ok(sig) = ed25519_dalek::Signature::from_slice(binding.tagged_sig.as_slice()) else {
            return false;
        };
        k.verifying_key().verify(msg, &sig).is_ok()
    }

    fn tagged_verifies(k: &SigningKey, binding: &CertBinding, msg: &[u8]) -> bool {
        let Ok(sig) = ed25519_dalek::Signature::from_slice(binding.tagged_sig.as_slice()) else {
            return false;
        };
        k.verifying_key()
            .verify(&domain_separated(CERT_BINDING_V1, msg), &sig)
            .is_ok()
    }

    /// (a) A nonce that is not the canonical 32 bytes is refused outright — most
    /// pointedly a 4-byte one, which on the served-SPKI path would make the
    /// signed message exactly `CID_LEN`.
    #[test]
    fn a_non_canonical_nonce_is_refused_by_every_producer() {
        let k = key();
        let spki = [7u8; 32];
        for bad in [0usize, 1, 4, 31, 33, 36, 96, 1024] {
            let nonce = vec![0xABu8; bad];
            for kind in ALL_BINDING_KINDS {
                assert!(
                    sign_channel_binding(&k, &spki, &nonce, kind).is_none(),
                    "{kind:?} signed a {bad}-byte nonce"
                );
            }
        }
        // 64 is the verify path's fold — refused by Handshake and CertHandshake
        // (neither ever legitimately sees one; CertHandshake refusing it is the
        // producer-side half of the fix), accepted only by
        // VerifyFold.
        let fold = vec![0xABu8; 2 * CLIENT_NONCE_LEN];
        assert!(sign_channel_binding(&k, &spki, &fold, BindingKind::Handshake).is_none());
        assert!(sign_channel_binding(&k, &spki, &fold, BindingKind::CertHandshake).is_none());
    }

    /// The verify path's `challenge_nonce ‖ client_nonce` fold (64 bytes) stays
    /// accepted on the VerifyFold producer — the guard bounds the shape, it
    /// does not break NT-1.
    #[test]
    fn the_challenge_fold_stays_accepted_on_the_verify_fold_producer() {
        let k = key();
        let spki = [7u8; 32];
        let nonce = vec![0x5Au8; 2 * CLIENT_NONCE_LEN];
        assert!(sign_channel_binding(&k, &spki, &nonce, BindingKind::VerifyFold).is_some());
    }

    /// (b) Every producer, on every shape it signs — served SPKI or none —
    /// emits ONLY the tagged signature: nothing the deployment key signs here
    /// verifies as a bare, untagged signature over the message, so the
    /// KeyBlob's bare-CID context has no sibling to be confused with (rule #8;
    /// the untagged compat half retired 2026-09-24). `CID_LEN` is named so the
    /// property this replaces stays legible: the old untagged half needed a
    /// "never over a 36-byte message" guard; with no untagged half there is
    /// nothing to bound.
    #[test]
    fn no_producer_emits_an_untagged_signature() {
        let k = key();
        for spki in [Vec::new(), vec![7u8; 32]] {
            for kind in ALL_BINDING_KINDS {
                for len in [CLIENT_NONCE_LEN, 2 * CLIENT_NONCE_LEN, CID_LEN] {
                    let nonce = vec![0xC1u8; len];
                    let Some(b) = sign_channel_binding(&k, &spki, &nonce, kind) else {
                        continue;
                    };
                    let mut msg = spki.clone();
                    msg.extend_from_slice(&nonce);
                    assert!(
                        !untagged_verifies(&k, &b, &msg),
                        "{kind:?} signed spki={} nonce={len} untagged — the oracle is open",
                        spki.len()
                    );
                    assert!(
                        tagged_verifies(&k, &b, &msg),
                        "the tagged proof must survive"
                    );
                    assert_eq!(b.spki_sha256.as_slice(), spki.as_slice());
                }
            }
        }
    }

    /// End-to-end against the real client verifier: the shipped consumer of a
    /// `nest_handshake` binding accepts a cert-less nest's tagged-only proof
    /// (`fauna_client_core::nest_trust`, the shared core behind both
    /// `graduate_first_contact` and web's possession proof).
    #[test]
    fn the_shipped_client_verifier_still_accepts_a_bare_nonce_binding() {
        let k = key();
        let nonce = [0x11u8; CLIENT_NONCE_LEN];
        let b = sign_channel_binding(&k, &[], &nonce, BindingKind::Handshake).expect("signed");
        assert_eq!(
            fauna_client_core::nest_trust::verify_cert_binding_possession(&nonce, &b),
            Ok(k.verifying_key().to_bytes()),
        );
    }

    /// A tampered nonce still fails, so the possession proof has not been
    /// weakened into a constant.
    #[test]
    fn the_shipped_client_verifier_rejects_a_replayed_bare_nonce_binding() {
        let k = key();
        let nonce = [0x11u8; CLIENT_NONCE_LEN];
        let b = sign_channel_binding(&k, &[], &nonce, BindingKind::Handshake).expect("signed");
        let other = [0x22u8; CLIENT_NONCE_LEN];
        assert!(fauna_client_core::nest_trust::verify_cert_binding_possession(&other, &b).is_err());
    }

    /// The signer is a pure function of its inputs — the same key, SPKI and
    /// nonce give a byte-identical binding, which is what lets
    /// `build_identity_binding` and `build_cert_binding` be documented as
    /// producing the same proof in production (both hold the deployment key).
    #[test]
    fn the_two_producers_agree_byte_for_byte_on_a_served_nest() {
        let k = key();
        let spki = [7u8; 32];
        let nonce = [0x11u8; CLIENT_NONCE_LEN];
        assert_eq!(
            sign_channel_binding(&k, &spki, &nonce, BindingKind::Handshake),
            sign_channel_binding(&k, &spki, &nonce, BindingKind::CertHandshake),
        );
    }

    /// Sanity: the tagged half is under the registered cert-binding context and
    /// no other, so nothing here silently re-mints a context (`sig_domain`'s
    /// registry test owns pairwise-distinctness).
    #[test]
    fn the_tagged_half_uses_the_registered_cert_binding_context() {
        let k = key();
        let nonce = [0x11u8; CLIENT_NONCE_LEN];
        let b = sign_channel_binding(&k, &[], &nonce, BindingKind::Handshake).expect("signed");
        let t = ed25519_dalek::Signature::from_slice(b.tagged_sig.as_slice()).expect("well-formed");
        assert!(
            k.verifying_key()
                .verify(&domain_separated(CERT_BINDING_V1, &nonce), &t)
                .is_ok()
        );
        for other in [
            fauna_protocol::sig_domain::FEDERATION_HELLO_V1,
            fauna_protocol::sig_domain::KEYBLOB_V1,
            fauna_protocol::sig_domain::NEST_ROTATION_V1,
        ] {
            assert!(
                k.verifying_key()
                    .verify(&domain_separated(other, &nonce), &t)
                    .is_err()
            );
        }
    }
}
