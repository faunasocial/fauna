//! Pre-identity one-time admin-claim WS-RPC payload types —
//! `fauna.auth.claim_admin`. The sole transport for admin claim since the public
//! `POST /api/v1/claim-admin` route was removed;
//! the ceremony lives in the shared `bins/fauna-nest/src/claim_core.rs`.
//! `claim_admin` runs on the **anonymous** WS connection (`GET /api/v1/ws`, no
//! bearer) — the "Admin claim" row of the pre-identity allowlist in
//! `docs/goal/architecture/transport.md` § Pre-identity (anonymous) connection.
//! Track A4 of the WS-RPC-everywhere migration (tracked internally).
//!
//! One-time bootstrap: the first caller with a valid claim code becomes the
//! initial admin (admin is a Fauna app per the product invariant). After the
//! claim succeeds the claim-code file is deleted, so a second attempt fails with
//! `fauna.auth.already_claimed` — which is what makes the kind replay-safe.
//!
//! Wire convention (matching `auth.rs` / `account.rs`): identity references
//! (`actor_id`, `signature`) are **hex-encoded `String`**; the dag-cbor wire
//! forbids floats (none here). The `handle` (request and reply) is a **required
//! `String`** — a handle-less admin is a degenerate, unusable state (mail/AUTH
//! login resolves nobody), so claiming-without-a-handle is not a representable
//! outcome at the type level. The reply mirrors `auth::VerifyReply`'s `token` /
//! `domain` / `expires_at` shape (no `tier` — claim does not surface one), with
//! `handle` always present because every claim sets one.
//!
//! Kind registry: `kind.rs::register_claim_admin_kind`.

use fauna_core::secret::SecretString;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

/// The exact bytes a domain-separated `fauna.auth.claim_admin` request signs (and
/// the nest verifies): `CLAIM_ADMIN_V1 ‖ actor_id ‖ timestamp_be` (the **raw**
/// timestamp — the same value the request carries, before any ms→s freshness
/// normalization). **Single source of the tagged-message contract** — the
/// client signer (`fauna-onboarding-machine`'s `WsRpcNestApi::claim_admin`) and
/// the nest verifier (`bins/fauna-nest/src/routes.rs::verify_claim_admin_signature`)
/// both build the message here so they cannot drift.
///
/// The domain tag ([`crate::sig_domain::CLAIM_ADMIN_V1`]) is what separates this
/// context from the byte-identical untagged login handshake / lockout messages.
/// Pure byte assembly — crypto-free, so the lean default
/// protocol build stays so (signing/verifying live in the consumers).
pub fn claim_admin_signed_message(actor_id: &[u8; 32], timestamp: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(40);
    body.extend_from_slice(actor_id);
    body.extend_from_slice(&timestamp.to_be_bytes());
    crate::sig_domain::domain_separated(crate::sig_domain::CLAIM_ADMIN_V1, &body)
}

// ── fauna.auth.claim_admin (≡ POST /api/v1/claim-admin) ─────────────────────

/// Submit the first-boot claim code to become the initial nest admin, proving
/// actor ownership with a domain-tagged signature over
/// [`claim_admin_signed_message`]`(actor_id, timestamp)` (the raw timestamp).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimAdminRequest {
    /// The one-time claim code printed at first boot (case-insensitive; the
    /// nest normalizes hyphens/case at comparison) — 8 characters from a
    /// 32-symbol ambiguity-free alphabet, 40 bits of entropy, displayed
    /// hyphenated as `ABCD-EFGH` (`fauna_core::claim_code`).
    pub claim_code: String,
    /// 64-char hex of the claiming actor's 32-byte Ed25519 public key.
    pub actor_id: String,
    /// Client wall-clock — Unix **seconds or milliseconds** (both accepted; a
    /// value > 1e12 is treated as ms). Must be within ±300 s of server time.
    pub timestamp: u64,
    /// 128-char hex of the 64-byte **domain-separated** Ed25519 signature over
    /// [`claim_admin_signed_message`]`(actor_id, timestamp)` =
    /// `b"fauna.auth.claim-admin.v1\0" ‖ actor_id ‖ timestamp_be` (the **raw**
    /// timestamp value, before any ms→s conversion;
    /// `fauna_protocol::sig_domain::CLAIM_ADMIN_V1`). Because the message
    /// carries the claim-admin tag, a signature from any other actor-key
    /// context (a login handshake, a lockout) can never satisfy it — the
    /// structural cross-context guarantee of rule #8. The
    /// untagged legacy form was deleted outright under the 2026-08-17
    /// no-existing-users ratification; nothing else verifies.
    pub signature: String,
    /// The handle to set during the claim. **A handle is REQUIRED** — making the
    /// handle-less admin unrepresentable at the type level. The admin's handle is
    /// its mail address AND its identity, and the canonical mail-recipient alias
    /// is materialized from it, so a handle-less admin is a degenerate, unusable
    /// state (mail/AUTH login resolves nobody). Claiming the nest atomically sets
    /// the handle. The nest stores only the bare local-part (its `validate_handle`
    /// rejects `@`), so a wizard handle like `alice@nest.example` sends `alice`
    /// here and the `@domain` as `mail_domain`. The nest still runtime-validates
    /// it (`validate_handle` rejects an empty / too-short / malformed `String`).
    pub handle: String,
    /// Optional mail domain to auto-register at claim — the `@domain` suffix the
    /// wizard handle carried (`alice@fauna.test` → `fauna.test`). The nest stores
    /// only the bare local-part as the handle, so the domain travels separately;
    /// registering it here makes the handle a routable email with no manual
    /// admin-dns add-domain step. `None` registers no domain.
    #[serde(default)]
    pub mail_domain: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The bearer + nest coordinates issued on a successful claim — the body of the
/// `{ok, token, expires_at, domain, handle?}` JSON the HTTP twin returned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimAdminReply {
    /// Opaque bearer token (1-hour TTL) — the client opens the authenticated
    /// `GET /api/v1/ws/{actor_id}` with it.
    pub token: String,
    /// Unix seconds at which the token expires.
    pub expires_at: u64,
    /// The nest's handle domain (the HTTP twin's `domain`, default `localhost`).
    pub domain: String,
    /// The handle set during the claim — always present, since a claim cannot
    /// succeed without one (the request `handle` is required).
    pub handle: String,
    /// The nest's **deployment signing seed** — 64-char hex of the raw 32-byte
    /// Ed25519 seed (the preimage of `nest_actor_id`; same hex convention as the
    /// request's `actor_id`). Handed to the claiming admin's client so it can
    /// custody the nest's identity **off-box** (on the account plane, `fauna.state.deployment-seeds`) and re-install it
    /// after **total box loss**, so the rebuilt box re-presents the *same*
    /// `nest_actor_id` and TOFU-pinned clients reconnect without a trust break
    /// (`docs/goal/architecture/nest/box-recovery.md` § Mechanism — claim-an-existing
    /// box). This is the one new nest→client exposure of the seed; it is gated to
    /// the claiming admin over the authenticated claim ceremony — who already holds
    /// full nest-admin authority — so it crosses no new trust boundary.
    /// `Option` because a nest without a loaded signing key omits it; a nest
    /// with one always sends it.
    ///
    /// Held as [`SecretString`] (zeroize-on-drop + redacted `Debug`) — the seed is
    /// the nest's irreplaceable identity, so its in-memory holdings are hardened
    /// uniformly (box-recovery.md § Mechanism). `SecretString`
    /// serializes **byte-for-byte identically to a `String`** (a CBOR text string),
    /// so the wire is unchanged — full bidirectional compat (`version-compatibility.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_seed: Option<SecretString>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Catalog-aligned fixture default so construction sites grow new fields via
/// struct-update syntax (`..Default::default()`) instead of hand-listing every
/// field — two branches independently growing this wire type then merge cleanly
/// (the fixture-shape-conflict discipline).
impl Default for ClaimAdminRequest {
    fn default() -> Self {
        Self {
            claim_code: String::new(),
            actor_id: String::new(),
            timestamp: 0,
            signature: String::new(),
            handle: String::new(),
            mail_domain: None,
            extra: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn claim_admin_request_round_trips() {
        // The handle is a required `String` — a handle-less request is
        // unrepresentable. The optional `mail_domain` still round-trips both ways.
        let req = ClaimAdminRequest {
            claim_code: "A1B2C3".into(),
            actor_id: "ab".repeat(32),
            timestamp: 1_700_000_000_000,
            signature: "cd".repeat(64),
            handle: "admin".into(),
            mail_domain: Some("nest.example".into()),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<ClaimAdminRequest>(&bytes).unwrap());

        let no_domain = ClaimAdminRequest {
            mail_domain: None,
            ..req
        };
        let bytes = encode_canonical(&no_domain).unwrap();
        assert_eq!(no_domain, decode::<ClaimAdminRequest>(&bytes).unwrap());
    }

    #[test]
    fn claim_admin_signed_message_is_domain_tagged() {
        let actor = [0xab_u8; 32];
        let ts: u64 = 1_700_000_000_000;
        let m = claim_admin_signed_message(&actor, ts);
        // Begins with the claim-admin tag, then the same actor ‖ ts_be body the
        // untagged legacy form is — so it is provably NOT the bare 40-byte login
        // shape.
        assert!(m.starts_with(crate::sig_domain::CLAIM_ADMIN_V1));
        let body = &m[crate::sig_domain::CLAIM_ADMIN_V1.len()..];
        assert_eq!(&body[..32], &actor);
        assert_eq!(&body[32..], &ts.to_be_bytes());
        // The untagged login/claim shape (actor ‖ ts_be) is a strict, distinct
        // subsequence — never equal to the tagged message.
        let mut bare = Vec::with_capacity(40);
        bare.extend_from_slice(&actor);
        bare.extend_from_slice(&ts.to_be_bytes());
        assert_ne!(m, bare);
    }

    #[test]
    fn claim_admin_request_missing_handle_key_fails_to_decode() {
        // A non-conforming caller that omits the `handle` key entirely is
        // rejected at the wire layer (the required `String` has no default), so a
        // handle-less claim can't even be decoded — the illegal state is
        // unrepresentable on the wire, not merely runtime-rejected.
        let mut map = BTreeMap::new();
        map.insert("claim_code".to_string(), Value::String("A1B2C3".into()));
        map.insert("actor_id".to_string(), Value::String("ab".repeat(32)));
        map.insert("timestamp".to_string(), Value::Integer(1_700_000_000_000));
        map.insert("signature".to_string(), Value::String("cd".repeat(64)));
        let bytes = encode_canonical(&Value::Map(map)).unwrap();
        assert!(decode::<ClaimAdminRequest>(&bytes).is_err());
    }

    #[test]
    fn claim_admin_reply_round_trips_and_re_encodes_identically() {
        // The reply handle is always present (every claim sets one); the
        // deployment seed is the off-box-recovery hand-off (box-recovery.md § 2).
        let reply = ClaimAdminReply {
            token: "tok.123".into(),
            expires_at: 1_700_000_003_600,
            domain: "nest.example".into(),
            handle: "admin".into(),
            deployment_seed: Some("ab".repeat(32).into()),
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: ClaimAdminReply = decode(&bytes1).unwrap();
        assert_eq!(reply, decoded);
        assert_eq!(
            decoded.deployment_seed.as_deref(),
            Some(&"ab".repeat(32)[..])
        );
        // Canonical re-encode is stable.
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn claim_admin_reply_without_deployment_seed_round_trips() {
        // A nest without a signing key omits the seed; the field is
        // `skip_serializing_if` so it is absent on the wire (not a null), and a
        // client decodes it back to `None`.
        let reply = ClaimAdminReply {
            token: "tok.123".into(),
            expires_at: 1_700_000_003_600,
            domain: "nest.example".into(),
            handle: "admin".into(),
            deployment_seed: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ClaimAdminReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert!(decoded.deployment_seed.is_none());
        // The absent field must not leak into `extra` as a null.
        assert!(!decoded.extra.contains_key("deployment_seed"));
    }
}
