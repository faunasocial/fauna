//! The bearer mints — each opens a fresh anonymous WS connection, runs one
//! ceremony, and **graduates the TLS channel binding** (security.md § Transport
//! trust, Axis 1) before returning the token:
//!
//! - [`mint_bearer_over_silent_challenge`] — `fauna.auth.challenge` +
//!   `fauna.auth.verify`, the mint for **every bearer an app holds**
//!   (`login.md` § When to use which: no client timestamp, so a device whose
//!   clock is hours wrong still mints and refreshes). `fauna-client`'s
//!   `WsChallengeBearer` (the UniFFI apps' `FfiNestClient`, the `fauna-ffi`
//!   `mint_bearer` export) rides it.
//! - [`mint_bearer_over_handshake`] — `fauna.auth.handshake`, for tests,
//!   scripts and machine-to-machine callers, plus `fauna-onboarding-machine`'s
//!   one-shot native `recovery_config` read (no held deadline).
//! - [`mint_bearer_over_device_handshake`] / [`mint_bearer_over_custody_handshake`]
//!   — the sync agent's renewal grant and the custodian's ceremony.
//!
//! Lifted here from `fauna-client` so they sit next to the anonymous connect
//! ([`AnonymousNestClient`]) + graduation ([`crate::graduate_handshake`],
//! [`crate::graduate_verify_path`]) they compose — this crate is the leaf home
//! of all three (priority #2/#4), reachable from `fauna-onboarding-machine`
//! without the documented `fauna-client → fauna-nest-http →
//! fauna-launch-machine` Cargo cycle.
//!
//! **Every mint anchors its expiry on the client's own clock at receipt**
//! ([`MintedBearer::expires_at`]; `login.md` § Token lifetime on the client's
//! clock) — the one place the conversion happens, so no holder above can
//! compare the nest's absolute deadline against a device clock by mistake.
//!
//! **Per-request `client_nonce`:** a fresh nonce is folded into the handshake
//! signature (replay-guard uniqueness for concurrent same-actor sign-ins) or the
//! verify reply's `cert_binding` proof (the NT-1 hardening), and graduated
//! against the captured TLS cert on `https://` before the minted bearer is
//! trusted.

use ed25519_dalek::{Signer, SigningKey};

use fauna_protocol::auth::{HandshakeReply, HandshakeRequest};

use crate::AnonymousNestClient;
use crate::error::AnonClientError;

/// The pre-identity kind that mints a bearer (≡ the retired HTTP
/// `POST /api/v1/auth/token`). Allowlisted on the anonymous connection
/// (transport.md § Pre-identity).
const HANDSHAKE_KIND: &str = "fauna.auth.handshake";

/// A freshly-minted bearer, its session id, and its expiry — the result of one
/// mint ceremony.
pub struct MintedBearer {
    pub token: String,
    /// Short 16-hex id of the session this mint created
    /// ([`fauna_protocol::auth::HandshakeReply::token_id`]). **Kept, not
    /// dropped**: it is how a holder names its own session — every mint path
    /// decoded it off the wire and threw it away until 2026-09-20, so no app
    /// could mark "this session" or send `revoke_all`'s `keep_token_id`
    /// (`docs/goal/behavior/devices.md` § The client's own session). The
    /// holders above this (`fauna_client::token_cache::TokenCache`,
    /// `fauna_launch_machine::LaunchMachine`) fold it into a
    /// [`fauna_protocol::auth::OwnSessionIds`] set; this type carries the one
    /// id this mint produced.
    pub token_id: String,
    /// Unix **seconds** at which the bearer expires, **on this client's own
    /// clock**: `now_client + expires_in`, anchored at receipt by
    /// [`fauna_protocol::auth::deadline_on_own_clock`] (`login.md` § Token
    /// lifetime on the client's clock). Falls back to the nest's absolute
    /// `expires_at` only when the client's clock cannot be read. Every holder
    /// above compares it against its own clock, so it must never carry the
    /// nest's.
    pub expires_at: u64,
}

impl MintedBearer {
    /// Build from a mint reply, anchoring the expiry at receipt — the one
    /// conversion every mint in this module makes.
    fn at_receipt(token: String, token_id: String, expires_in: u64, expires_at: u64) -> Self {
        Self::at_receipt_on(now_client_secs(), token, token_id, expires_in, expires_at)
    }

    /// [`Self::at_receipt`] with the client's `now` passed in, so a test can
    /// stand in a clock hours off the nest's.
    fn at_receipt_on(
        now_client_secs: Option<u64>,
        token: String,
        token_id: String,
        expires_in: u64,
        expires_at: u64,
    ) -> Self {
        Self {
            token,
            token_id,
            expires_at: fauna_protocol::auth::deadline_on_own_clock(
                now_client_secs,
                expires_in,
                expires_at,
            ),
        }
    }
}

/// The client's own clock in unix seconds — the ONE client clock
/// ([`fauna_protocol::client_clock`], e2e offset included) every holder above
/// compares this anchor against. `None` when it reads before the epoch —
/// `deadline_on_own_clock` then keeps the nest's deadline.
fn now_client_secs() -> Option<u64> {
    fauna_protocol::client_clock::now_secs()
}

/// **Graduate the TLS channel binding** before a minted bearer is trusted — the
/// tail shared byte-for-byte by [`mint_bearer_over_handshake`] and
/// [`mint_bearer_over_device_handshake`] (security.md § Transport trust, Axis 1).
/// Only for `https://` (a TLS connection has a cert to bind); a plaintext
/// loopback dev nest keeps the network-trust posture. On failure the binding is
/// untrusted — the caller must NOT return the token (security.md §
/// Connection-teardown rule), which is why this returns `Err` rather than
/// silently downgrading.
async fn graduate_bearer_channel_binding(
    client: &AnonymousNestClient,
    nest_url: &str,
    client_nonce: &[u8; 32],
    cert_binding: Option<&fauna_protocol::auth::CertBinding>,
) -> Result<(), AnonClientError> {
    if nest_url.starts_with("https://") {
        let host = crate::authority_of(nest_url);
        crate::graduate_handshake(
            client,
            &host,
            &client.captured_cert(),
            client_nonce,
            cert_binding,
        )
        .await
        .map_err(|e| AnonClientError::WebSocket(format!("nest channel binding: {e}")))?;
    }
    Ok(())
}

/// Mint a bearer over a single `fauna.auth.handshake` round trip on a fresh
/// anonymous WS connection: read the nest's identity off the connection
/// ([`crate::read_login_binding`] — the identity every login signature binds,
/// `login.md` § Binding the nest), sign
/// [`handshake_signed_message`](fauna_protocol::auth::handshake_signed_message)`(actor_id,
/// timestamp, nest_id, client_nonce)`, send the handshake, **graduate the TLS
/// channel binding on `https://`** (which pins the served SPKI for a
/// self-signed / DNS-`self=` nest and is a no-op for a WebPKI-valid one), then
/// return the token + expiry.
///
/// The graduation is what makes a subsequent authed connection to a self-signed
/// nest possible *and* MITM-safe: [`crate::TokenNestClient`] (and
/// `fauna-client`'s `connect_with_subprotocol_bearer`) read the pin this writes
/// via [`crate::pinned_spki`]. On graduation failure the binding is untrusted —
/// the token is **not** returned (security.md § Connection-teardown rule).
pub async fn mint_bearer_over_handshake(
    nest_url: &str,
    actor_id: [u8; 32],
    signing_key: &SigningKey,
) -> Result<MintedBearer, AnonClientError> {
    let client = AnonymousNestClient::connect(nest_url).await?;
    let nest_id = crate::read_login_binding(&client).await?;

    let timestamp = fauna_core::data::Timestamp::now_millis();

    // Fresh per-request nonce: folded into the auth signature so the
    // deterministic Ed25519 signature is unique per request (concurrent
    // same-actor handshakes don't collide on the nest's replay guard) AND used
    // for the TLS channel binding below (security.md § Transport trust, Axis 1).
    let nonce = crate::fresh_nonce();
    let msg =
        fauna_protocol::auth::handshake_signed_message(&actor_id, timestamp, &nest_id, &nonce);
    let signature = signing_key.sign(&msg);

    let req = HandshakeRequest {
        actor_id: fauna_core::hex32::encode(&actor_id),
        timestamp,
        signature: hex::encode(signature.to_bytes()),
        client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
        nest_id: fauna_core::hex32::encode(&nest_id),
        extra: Default::default(),
    };

    let reply: HandshakeReply = client.request(HANDSHAKE_KIND, req).await?;

    graduate_bearer_channel_binding(&client, nest_url, &nonce, reply.cert_binding.as_ref()).await?;

    Ok(MintedBearer::at_receipt(
        reply.token,
        reply.token_id,
        reply.expires_in,
        reply.expires_at,
    ))
}

/// Mint an **app-held** bearer over the silent challenge — `fauna.auth.challenge`
/// then `fauna.auth.verify` on one fresh anonymous WS connection — the mint
/// `login.md` § When to use which assigns to every bearer an app holds: launch,
/// TTL refresh, 401-reactive refresh. The ceremony
/// carries no client timestamp (the server's nonce is freshness), so a device
/// whose clock is hours wrong still refreshes, where the handshake's ±30 s
/// window refuses it.
///
/// Signs with `signing_key`, whose public half **is** the actor id, binding
/// the nest's identity read off the connection first
/// ([`crate::read_login_binding`]; `login.md` § Binding the nest). A fresh
/// client nonce is folded into the verify reply's `cert_binding` proof (the
/// NT-1 hardening) and, on `https://`, the binding is graduated through
/// [`crate::graduate_verify_path`] — the same graduation the launch machine's
/// silent challenge runs — before the token is trusted. A graduation failure
/// surfaces as [`AnonClientError::Trust`] **unflattened**, so a holder can tell
/// a changed pinned identity (`classify_identity_changed`) from ordinary
/// binding trouble.
///
/// Every nest refusal arrives as [`AnonClientError::Rpc`] verbatim —
/// `fauna.auth.not_registered`, `fauna.auth.superseded` with its successor,
/// `fauna.nest.outdated` — never pre-classified, for the same reason: the
/// holder's own taxonomy decides what each means to it.
pub async fn mint_bearer_over_silent_challenge(
    nest_url: &str,
    signing_key: &SigningKey,
) -> Result<MintedBearer, AnonClientError> {
    use fauna_protocol::auth::ChallengeVerifyFailure;

    let client = AnonymousNestClient::connect(nest_url).await?;
    let nest_id = crate::read_login_binding(&client).await?;
    let client_nonce = crate::fresh_nonce();

    let (reply, challenge_nonce) =
        fauna_protocol::auth::challenge_verify(&client, signing_key, Some(&client_nonce), &nest_id)
            .await
            .map_err(|failure| match failure {
                ChallengeVerifyFailure::Challenge(e)
                | ChallengeVerifyFailure::Verify { error: e, .. } => e,
                ChallengeVerifyFailure::MalformedNonce => {
                    AnonClientError::Decode("invalid challenge nonce hex from nest".into())
                }
            })?;

    if nest_url.starts_with("https://") {
        let host = crate::authority_of(nest_url);
        crate::graduate_verify_path(
            &client,
            &host,
            &client.captured_cert(),
            &challenge_nonce,
            &client_nonce,
            reply.cert_binding.as_deref(),
        )
        .await
        .map_err(AnonClientError::Trust)?;
    }

    Ok(MintedBearer::at_receipt(
        reply.token,
        reply.token_id,
        reply.expires_in,
        reply.expires_at,
    ))
}

/// The renewal-grant sibling of [`mint_bearer_over_handshake`]
/// (`docs/goal/architecture/apps/sync-agent.md` § Credential model): mint a
/// bearer over a single `fauna.auth.device_handshake` round trip, signing the
/// domain-tagged
/// [`device_handshake_signed_message`](fauna_protocol::auth::device_handshake_signed_message)
/// with the **renewal device key** — never the identity keypair, which the
/// bearer-only sync agent does not hold. Same anonymous connect, same fresh
/// per-request nonce, same TLS channel-binding graduation before the token is
/// trusted; only the signing key and the kind differ.
///
/// A refused request fails typed (for example `fauna.protocol.unknown_kind`,
/// an ordinary error); the app's live immediate path is the app-pushed
/// `RefreshBearer`.
pub async fn mint_bearer_over_device_handshake(
    nest_url: &str,
    actor_id: [u8; 32],
    device_signing_key: &SigningKey,
) -> Result<MintedBearer, AnonClientError> {
    use fauna_protocol::auth::{DEVICE_HANDSHAKE_KIND, DeviceHandshakeRequest};

    let client = AnonymousNestClient::connect(nest_url).await?;
    let nest_id = crate::read_login_binding(&client).await?;

    let device_key = device_signing_key.verifying_key().to_bytes();
    let timestamp = fauna_core::data::Timestamp::now_millis();
    let nonce = crate::fresh_nonce();
    let msg = fauna_protocol::auth::device_handshake_signed_message(
        &actor_id,
        &device_key,
        timestamp,
        &nest_id,
        &nonce,
    );
    let signature = device_signing_key.sign(&msg);

    let req = DeviceHandshakeRequest {
        actor_id: fauna_core::hex32::encode(&actor_id),
        device_key: fauna_core::hex32::encode(&device_key),
        timestamp,
        signature: hex::encode(signature.to_bytes()),
        client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
        nest_id: fauna_core::hex32::encode(&nest_id),
        extra: Default::default(),
    };

    let reply: HandshakeReply = client.request(DEVICE_HANDSHAKE_KIND, req).await?;

    graduate_bearer_channel_binding(&client, nest_url, &nonce, reply.cert_binding.as_ref()).await?;

    Ok(MintedBearer::at_receipt(
        reply.token,
        reply.token_id,
        reply.expires_in,
        reply.expires_at,
    ))
}

/// The custody sibling (W8.6 (account-data-plane.md § Workstreams) — `account-data-plane.md` § Replica posture →
/// *The custody grant + ceremony*): mint a CUSTODIAN session bearer over one
/// `fauna.auth.custody_handshake` round trip, signing the domain-tagged
/// [`custody_handshake_signed_message`](fauna_protocol::auth::custody_handshake_signed_message)
/// with the custodian's device-principal key and carrying the custody-grant
/// witness inline (self-contained carriage). The custodian holds NO account
/// credential on the owner's nest — this is its whole auth path there. Same
/// anonymous connect, fresh nonce, TLS channel-binding graduation.
pub async fn mint_bearer_over_custody_handshake(
    nest_url: &str,
    owner_actor_id: [u8; 32],
    custodian_signing_key: &SigningKey,
    witness: &fauna_core::encoding::EmbedAsBytes,
) -> Result<MintedBearer, AnonClientError> {
    use fauna_protocol::auth::{CUSTODY_HANDSHAKE_KIND, CustodyHandshakeRequest};

    let client = AnonymousNestClient::connect(nest_url).await?;
    let nest_id = crate::read_login_binding(&client).await?;

    let custodian_key = custodian_signing_key.verifying_key().to_bytes();
    let timestamp = fauna_core::data::Timestamp::now_millis();
    let nonce = crate::fresh_nonce();
    let msg = fauna_protocol::auth::custody_handshake_signed_message(
        &owner_actor_id,
        &custodian_key,
        timestamp,
        &nest_id,
        &nonce,
    );
    let signature = custodian_signing_key.sign(&msg);

    let req = CustodyHandshakeRequest {
        owner_actor_id: fauna_core::hex32::encode(&owner_actor_id),
        custodian_key: fauna_core::hex32::encode(&custodian_key),
        timestamp,
        signature: hex::encode(signature.to_bytes()),
        client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
        witness: witness.clone(),
        nest_id: fauna_core::hex32::encode(&nest_id),
        extra: Default::default(),
    };

    let reply: HandshakeReply = client.request(CUSTODY_HANDSHAKE_KIND, req).await?;

    graduate_bearer_channel_binding(&client, nest_url, &nonce, reply.cert_binding.as_ref()).await?;

    Ok(MintedBearer::at_receipt(
        reply.token,
        reply.token_id,
        reply.expires_in,
        reply.expires_at,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect the anchor exists for (`login.md` § Token lifetime on the
    /// client's clock): a device six hours ahead of the nest reads the nest's
    /// absolute deadline as five hours *past*, and every holder above would
    /// re-mint in a hot loop. Anchored at receipt, the deadline is the
    /// device's own now plus the lifetime the nest granted.
    #[test]
    fn a_client_hours_ahead_anchors_on_its_own_clock() {
        let nest_now = 1_800_000_000u64;
        let client_now = nest_now + 6 * 3600;
        let minted = MintedBearer::at_receipt_on(
            Some(client_now),
            "t".into(),
            "id".into(),
            3600,
            nest_now + 3600,
        );
        assert_eq!(minted.expires_at, client_now + 3600);
    }

    /// The behind direction: served long after it died on the nest.
    #[test]
    fn a_client_hours_behind_anchors_on_its_own_clock() {
        let nest_now = 1_800_000_000u64;
        let client_now = nest_now - 6 * 3600;
        let minted = MintedBearer::at_receipt_on(
            Some(client_now),
            "t".into(),
            "id".into(),
            3600,
            nest_now + 3600,
        );
        assert_eq!(minted.expires_at, client_now + 3600);
    }

    /// With no readable client clock the nest deadline is kept — the one
    /// remaining fallback (the older-nest one retired 2026-09-24).
    #[test]
    fn an_unreadable_clock_keeps_the_nest_deadline() {
        let minted =
            MintedBearer::at_receipt_on(None, "t".into(), "id".into(), 3600, 1_800_003_600);
        assert_eq!(minted.expires_at, 1_800_003_600);
    }

    /// The anchor reads the ONE client clock (`fauna_protocol::client_clock`),
    /// so the e2e offset the wrong-clock witnesses seed reaches every mint —
    /// on a UniFFI app the held bearer's anchor is here, not in the launch
    /// machine, and a case-M witness on an anchor reading the real clock would
    /// pass without testing anything. The offset is process-global and held
    /// for one read only (no other test in this crate reads the live clock).
    #[test]
    fn the_anchor_reads_the_shared_client_clock() {
        use fauna_protocol::client_clock;
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                client_clock::set_clock_offset_secs(0);
            }
        }
        let _reset = Reset;
        let real = fauna_core::data::Timestamp::now_secs_or_zero() as u64;
        client_clock::set_clock_offset_secs(6 * 3600);
        let seen = now_client_secs().expect("six hours ahead is readable");
        client_clock::set_clock_offset_secs(0);
        let ahead_by = seen.saturating_sub(real);
        assert!(
            (6 * 3600 - 10..=6 * 3600 + 10).contains(&ahead_by),
            "the mint anchor must read the skewed client clock; ahead by {ahead_by} s"
        );
    }
}
