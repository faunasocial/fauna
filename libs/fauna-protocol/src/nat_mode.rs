//! NAT-mode commit WS-RPC payload types — `fauna.setup.nat_mode`. The wire
//! surface that makes the nest's **NAT axis** (`public` / `private`) client-set
//! instead of env-baked (`FAUNA_MODE`). The commit ceremony lives in the shared
//! `bins/fauna-nest/src/nat_mode_core.rs`; this kind is its sole transport.
//! The NAT mode is **mutable** (a public↔private flip touches no at-rest
//! data), so any valid admin-signed set upserts the row (no write-once
//! `mode_conflict`); re-setting the same value is idempotent.
//!
//! `nat_mode` runs on the **anonymous** WS connection (`GET /api/v1/ws`, no
//! bearer) — the pre-identity allowlist (`docs/goal/architecture/transport.md`
//! § Pre-identity (anonymous) connection); the admin client also signs the same
//! payload over its authed connection for the post-onboarding admin toggle
//! (`2026-06-15-nest-nat-mode-client-set-design.md` § 5.2). The signature *is*
//! the auth (no bearer): the signing actor must be the committed admin.
//!
//! Design + API contract tracked internally (mechanism) +
//! `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
//! (behavior). The set is reached from the `nat_mode_choice` onboarding wizard
//! step (`onboarding.md` § 3b-bis) + the admin-panel toggle.
//!
//! Wire convention (matching `claim.rs`): identity
//! references (`actor_id`, `signature`) are **hex-encoded `String`**;
//! `timestamp` is a signed `i64` of Unix **milliseconds** (seconds tolerated).
//! No floats; no `Option`. `mode` is the lowercase wire string
//! `"public"` / `"private"`.
//!
//! Kind registry: `kind.rs::register_nat_mode_kind`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.setup.nat_mode ─────────────────────────────────────────────────────

/// The exact bytes a `fauna.setup.nat_mode` request signs (and the nest
/// verifies): `SETUP_NAT_MODE_V2 ‖ mode_wire_str ‖ "\n" ‖ actor_id_hex ‖ "\n" ‖
/// timestamp_decimal ‖ "\n" ‖ nest_id_hex` (the decimal of the **raw**
/// `timestamp`; `nest_id_hex` the 64-hex of the receiving nest's identity — the
/// channel-binding `nest_actor_id` the client read off this very connection).
/// **Single source of the signed-message contract** — the client signer
/// (`fauna-onboarding-machine::build_signed_nat_mode_body`) and the nest
/// verifier (`nat_mode_core::commit_nat_mode_core`) build the message here so
/// they cannot drift. The domain tag
/// ([`crate::sig_domain::SETUP_NAT_MODE_V2`]) is the rule-#8 separation from
/// every other actor-key context; the nest identity is the binding: a blob
/// signed for one nest verifies at no other. The unbound `.v1` form named no
/// nest, so one signature was a bearer token at every nest where the actor
/// was admin (PROBE-482-B); it was retired 2026-09-24 with no accept path
/// (`version-compatibility.md` § Dimension 2, the compat-remnant sweep) and
/// its tag stays registered only so the bytes are never reused. Injective
/// within the context: `mode` comes from a closed enum of newline-free wire
/// strings, and the other three fields (hex, decimal, hex) cannot contain `\n`.
pub fn nat_mode_signed_message(
    mode: &str,
    actor_id_hex: &str,
    timestamp: i64,
    nest_id_hex: &str,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(mode.len() + actor_id_hex.len() + nest_id_hex.len() + 23);
    body.extend_from_slice(mode.as_bytes());
    body.push(b'\n');
    body.extend_from_slice(actor_id_hex.as_bytes());
    body.push(b'\n');
    body.extend_from_slice(timestamp.to_string().as_bytes());
    body.push(b'\n');
    body.extend_from_slice(nest_id_hex.as_bytes());
    crate::sig_domain::domain_separated(crate::sig_domain::SETUP_NAT_MODE_V2, &body)
}

/// Commit the deployment NAT mode, proving admin ownership with a domain-tagged
/// signature over [`nat_mode_signed_message`]`(mode, actor_id, timestamp,
/// nest_id)`. Mutable: any valid admin-signed set upserts the `nest_nat_mode`
/// row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NatModeRequest {
    /// The mode to set: `"public"` or `"private"`.
    pub mode: String,
    /// 64-char hex of the committing admin's 32-byte Ed25519 public key.
    pub actor_id: String,
    /// Client wall-clock — Unix **milliseconds** (seconds tolerated); within
    /// ±300 s of server time. The signature is over its decimal string.
    pub timestamp: i64,
    /// 128-char hex of the 64-byte Ed25519 signature over the canonical bytes.
    pub signature: String,
    /// The receiving nest's identity — 64-char hex of its `nest_actor_id`,
    /// read possession-proven off this connection before signing
    /// (`fauna_client_core::nest_trust::read_login_binding`). **Required**:
    /// the nest requires it to equal its own identity, before any signature
    /// work, and refuses any other with `fauna.setup.invalid_request`. A
    /// request without it is malformed (the unbound V1 form retired
    /// 2026-09-24, no accept path).
    pub nest_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The set mode — `{mode}`. There is no conflicting-set case (the NAT mode is
/// mutable), so a valid set always returns the new value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NatModeReply {
    /// The resolved mode wire string (`"public"` / `"private"`) — equals the
    /// request's `mode` on success.
    pub mode: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    /// Source-level pin on the builder's tag structure (rule #8): the message
    /// carries the nest-bound tag, and two different target nests over
    /// identical (mode, actor, ts) sign different bytes — the whole point of
    /// the binding. Shared-builder symmetry would keep a tag-less regression
    /// green everywhere else.
    #[test]
    fn nat_mode_signed_message_is_tagged_and_binds_the_nest() {
        let actor_hex = "ab".repeat(32);
        let nest_hex = "ef".repeat(32);
        let ts: i64 = 1_700_000_000_000;
        let bound = nat_mode_signed_message("public", &actor_hex, ts, &nest_hex);
        assert!(bound.starts_with(crate::sig_domain::SETUP_NAT_MODE_V2));
        let other_nest = "0d".repeat(32);
        assert_ne!(
            bound,
            nat_mode_signed_message("public", &actor_hex, ts, &other_nest)
        );
        // The retired unbound tag never prefixes a live message.
        assert!(!bound.starts_with(crate::sig_domain::SETUP_NAT_MODE_V1));
    }

    #[test]
    fn nat_mode_request_round_trips() {
        let req = NatModeRequest {
            mode: "private".into(),
            actor_id: "ab".repeat(32),
            timestamp: 1_700_000_000_000,
            signature: "cd".repeat(64),
            nest_id: "ef".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<NatModeRequest>(&bytes).unwrap());
    }

    /// The unbound request shape is not a request any more: `nest_id` is
    /// required, so a signer that predates the binding fails to decode rather
    /// than being verified against bytes it did not sign.
    #[test]
    fn an_unbound_nat_mode_request_does_not_decode() {
        let unbound = crate::Value::Map(
            [
                ("mode", crate::Value::String("private".into())),
                ("actor_id", crate::Value::String("ab".repeat(32))),
                ("timestamp", crate::Value::Integer(1_700_000_000_000)),
                ("signature", crate::Value::String("cd".repeat(64))),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        );
        let bytes = encode_canonical(&unbound).unwrap();
        assert!(decode::<NatModeRequest>(&bytes).is_err());
    }

    #[test]
    fn nat_mode_reply_round_trips_and_re_encodes_identically() {
        let reply = NatModeReply {
            mode: "public".into(),
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let back: NatModeReply = decode(&bytes1).unwrap();
        assert_eq!(reply, back);
        assert_eq!(bytes1, encode_canonical(&back).unwrap());
    }
}
