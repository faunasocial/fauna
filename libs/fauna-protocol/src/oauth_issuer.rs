//! Wire types for the nest's OAuth **issuer key** admin surface
//! (`fauna.oauth.*`), per `docs/goal/behavior/authorization-server.md`
//! § The issuer.
//!
//! Three Admin-class kinds — the read/act pair `fauna.tls.{cert_status,
//! publish_cert}` already uses for the deployment's other crypto material,
//! plus the forced arm the compromise case needs:
//!
//! * `fauna.oauth.issuer_key_status` — which keys the issuer serves, which one
//!   is signing, and when a retired one stops being served;
//! * `fauna.oauth.rotate_issuer_key` — mint a new signer and retire the
//!   outgoing key, which keeps being **served** for the retirement horizon so
//!   every token minted before the rotation still verifies (there is no
//!   scheduled rotation: ES256 keys do not wear out, and every rotation risks
//!   JWKS-cache skew at exactly the clients least worth breaking);
//! * `fauna.oauth.force_rotate_issuer_key` — mint a new signer and **drop**
//!   every other key at once, horizon skipped: the **compromise response**
//!   § The issuer → *Two rotation arms* rules rotation exists for. A thief
//!   holding a leaked scalar chooses `exp` themselves, so the access-token
//!   lifetime bounds nothing they mint; the only thing that does is the leaked
//!   `kid` leaving the served JWKS, and the ordinary arm keeps it there for
//!   the whole horizon. The forced arm's cost is the horizon's whole purpose
//!   — every honest token signed by a dropped key stops verifying now — and
//!   the app control states that cost before dispatch;
//! * `fauna.oauth.force_rotate_session_secret` — the forced arm's **sibling
//!   for the second signer**: re-mint the HS256 secret this nest's OAuth
//!   refresh tokens are MACed under, invalidating every outstanding refresh
//!   token at once (clients re-authorize). Both signers rest in the same
//!   store, so a leak that exposes one exposes the other, and a forged refresh
//!   token redeems for an access token signed by the *new* issuer key — the
//!   forced issuer rotation alone leaves the compromise response half done.
//!   It has no ordinary arm because the secret is a single row with no
//!   stranger verifying it: there is no outgoing generation to keep served.
//!
//! # Why the forced arm is a KIND and not a flag on `rotate_issuer_key`
//!
//! An additive `force: bool` on the rotate request would be tolerated by an
//! older nest — carried in `extra` and ignored (transport.md rule 4) — which
//! rotates *softly* and answers success. The admin, responding to a leak,
//! would then read "rotated" while the leaked key stayed served for twenty
//! minutes. A kind an older nest never registered fails loudly instead, and a
//! loud refusal is the only honest answer a compromise response can give when
//! the nest cannot perform it (version-compatibility.md § Dim 1 — a newer app
//! against an older nest).
//!
//! # Why a `fauna.oauth.*` namespace and not `fauna.bridges.*`
//!
//! The AS-key rotation control that shipped in 2026-08-02 is a
//! `fauna.bridges.*` kind on an approved-bridge card, and § The issuer records
//! that this "was forced only by the key being reachable through that bridge".
//! The nest now holds the key itself, so the deployment-crypto namespace is the
//! honest one — the same move that puts the admin control "beside
//! `rotate_srs_secret` and `force_rotate_dkim` in the admin shell" rather than
//! on a bridge's card. Nothing here is bridge-conditional: these kinds answer
//! in every nest flavor, including one that compiles no bridge at all.
//!
//! # Why a new status kind rather than a `kid` on `get_as_key_status`
//!
//! § The issuer once said the (since retired) `get_as_key_status` "**may**
//! report a `kid` once the nest can read the key" — permissive, and written
//! when the only key set was the bridge's. That kind was *per approved
//! bridge*: it answered "the AS key this bridge reaches". The nest's issuer key set is deployment-wide and
//! exists with no bridge enrolled at all, so hanging it off a per-bridge reply
//! would make a deployment-wide fact unreadable exactly when no bridge is
//! there to ask about.

use crate::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Request for `fauna.oauth.issuer_key_status` (Admin). No parameters: the
/// issuer key set is deployment-wide, so there is nothing to select.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuerKeyStatusRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One key in the issuer's served set, as `fauna.oauth.issuer_key_status`
/// reports it. The private half never appears here or anywhere on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuerKeyEntry {
    /// The RFC 7638 JWK thumbprint `/oauth/jwks` publishes for this key, and
    /// that every token it signed names.
    pub kid: String,
    /// Absent while this is the signer; the retirement instant (epoch seconds)
    /// once a rotation has moved past it. A retired key is still **served** —
    /// it still verifies tokens minted before the rotation — until the horizon
    /// below elapses.
    pub retired_at: Option<i64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.oauth.issuer_key_status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuerKeyStatusReply {
    /// The `kid` of the key currently signing. Empty only on a nest that holds
    /// no deployment signing key to seal one under — the same condition that
    /// makes `/oauth/jwks` unable to answer.
    pub active_kid: String,
    /// Every key the JWKS serves, active first. More than one entry means a
    /// rotation is inside its horizon.
    pub keys: Vec<IssuerKeyEntry>,
    /// How long after `retired_at` a key stops being served, in seconds —
    /// `fauna_provisioning::oauth_issuer::issuer_key_retirement_horizon_secs()`,
    /// reported rather than re-derived by the app so an admin sees the number
    /// the nest will actually act on. It is what turns "rotated 2 minutes ago"
    /// into "the old key stops being served in 18 minutes".
    pub retirement_horizon_secs: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.oauth.rotate_issuer_key` (Admin). No parameters: the nest
/// mints the new key itself and the admin neither supplies nor ever reads its
/// private half — the `rotate_srs_secret` posture, for the same reason (this is
/// deployment crypto material, not a value a human chooses).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotateIssuerKeyRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.oauth.rotate_issuer_key`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotateIssuerKeyReply {
    /// The `kid` of the key that now signs. The admin needs it to recognise the
    /// new key in the JWKS — and to tell a rotation that happened from a reply
    /// that merely arrived.
    pub kid: String,
    /// When the rotation landed (epoch seconds). The outgoing key's own
    /// `retired_at`, so `rotated_at + retirement_horizon_secs` is when the
    /// JWKS stops carrying it.
    pub rotated_at: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.oauth.force_rotate_issuer_key` (Admin). No parameters,
/// for `RotateIssuerKeyRequest`'s reason — and deliberately not a `kid` to
/// drop: a leak is "the store was read at time T", which takes every key that
/// existed at T, so the honest response drops **all** of them and the admin is
/// never asked to guess which one the thief has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForceRotateIssuerKeyRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.oauth.force_rotate_issuer_key`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForceRotateIssuerKeyReply {
    /// The `kid` of the key that now signs — the only key the JWKS carries
    /// after this call.
    pub kid: String,
    /// When the forced rotation landed (epoch seconds).
    pub rotated_at: i64,
    /// Every `kid` this call removed from the served set — the active signer
    /// and any retired key still inside its horizon. Reported so the admin
    /// (and the test) can see exactly which keys stopped verifying, rather
    /// than inferring it from a status read that no longer lists them.
    pub dropped_kids: Vec<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.oauth.force_rotate_session_secret` (Admin). No
/// parameters, for `ForceRotateIssuerKeyRequest`'s reason: the nest mints the
/// replacement itself, and there is exactly one secret, so nothing to name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForceRotateSessionSecretRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.oauth.force_rotate_session_secret`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForceRotateSessionSecretReply {
    /// When the replacement secret was minted (epoch seconds) — from this
    /// instant no refresh token minted earlier verifies at `/oauth/token` or
    /// `/oauth/revoke`.
    pub rotated_at: i64,
    /// When the secret this call replaced had been minted (epoch seconds), so
    /// the admin can date the generation that died; absent on a nest that had
    /// never minted one, where the call is the first mint.
    pub replaced_minted_at: Option<i64>,
    /// How many connected-app grants this rotation **ended** — the blast radius
    /// as a count rather than a sentence.
    ///
    /// Re-minting the secret kills every refresh family the nest-hosted AS
    /// MACed under it, so leaving those grant rows alone would list N dead
    /// connections in the user's connected-apps surface until they expire — up
    /// to 180 days for a confidential client — which is the state
    /// `authorization-server.md` § The issuer → *Grants recorded while the flow
    /// is unhonoured…* rejects, reached through a second door. The rotation
    /// therefore ends them, and reports how many so the admin's response has an
    /// audit shape: "N connected apps signed out" is a fact about what happened,
    /// where the instants alone say only when the secret changed.
    ///
    /// Counts what this call ended, not how many grants the nest holds: a grant
    /// already revoked is not this response's to claim. `0` is the ordinary
    /// answer on a nest whose AS has minted nothing yet.
    ///
    /// Required: the nest either ended the grants whole and counts them, or
    /// fails the call, so there is no reply without a count. A reply missing
    /// it is refused at decode rather than read as zero, which would tell an
    /// admin responding to a leak that no app was connected.
    pub grants_ended: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict as decode, encode_canonical};

    #[test]
    fn the_session_secret_reply_dates_the_generation_it_killed() {
        // `replaced_minted_at` is the one field that distinguishes "rotated a
        // live secret" from "minted the first one"; a reply that dropped it on
        // the wire would leave the admin unable to tell the two apart.
        let reply = ForceRotateSessionSecretReply {
            rotated_at: 1_700_000_300,
            replaced_minted_at: Some(1_690_000_000),
            grants_ended: 3,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).expect("encode");
        let back = decode::<ForceRotateSessionSecretReply>(&bytes).expect("decode");
        assert_eq!(back, reply);
        let fresh = ForceRotateSessionSecretReply {
            rotated_at: 1_700_000_300,
            replaced_minted_at: None,
            grants_ended: 0,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&fresh).expect("encode");
        assert_eq!(
            decode::<ForceRotateSessionSecretReply>(&bytes).expect("decode"),
            fresh
        );
    }

    /// A reply without `grants_ended` is refused, never read as a count of
    /// zero: "no outside apps were connected" would be false while N still are.
    #[test]
    fn a_reply_without_a_count_is_refused() {
        // Built by hand rather than from the struct, because the property is
        // exactly that the KEY IS ABSENT.
        let mut countless = BTreeMap::new();
        countless.insert("rotated_at".to_string(), Value::Integer(1_700_000_300));
        countless.insert(
            "replaced_minted_at".to_string(),
            Value::Integer(1_690_000_000),
        );
        let bytes = encode_canonical(&Value::Map(countless)).expect("encode a countless reply");
        assert!(
            decode::<ForceRotateSessionSecretReply>(&bytes).is_err(),
            "an absent count is refused, never defaulted"
        );
    }

    #[test]
    fn the_forced_reply_carries_every_dropped_kid() {
        // The forced arm's one distinguishing field is the list of keys it
        // removed; a reply that dropped it on the wire would leave the admin
        // unable to tell a forced rotation from an ordinary one.
        let reply = ForceRotateIssuerKeyReply {
            kid: "new-kid".into(),
            rotated_at: 1_700_000_000,
            dropped_kids: vec!["old-kid".into(), "older-kid".into()],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).expect("encode");
        let back = decode::<ForceRotateIssuerKeyReply>(&bytes).expect("decode");
        assert_eq!(back, reply);
        assert_eq!(back.dropped_kids.len(), 2);
    }

    #[test]
    fn the_status_reply_round_trips_a_rotation_in_flight() {
        // The shape that matters is two keys at once — one signing, one retired
        // but still served. A single-key fixture would round-trip happily while
        // `retired_at` was dropped on the wire, which is precisely the field an
        // admin reads to know when the old key goes away.
        let reply = IssuerKeyStatusReply {
            active_kid: "new-kid".into(),
            keys: vec![
                IssuerKeyEntry {
                    kid: "new-kid".into(),
                    retired_at: None,
                    extra: Default::default(),
                },
                IssuerKeyEntry {
                    kid: "old-kid".into(),
                    retired_at: Some(1_700_000_000),
                    extra: Default::default(),
                },
            ],
            retirement_horizon_secs: 1200,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).expect("encode");
        let back = decode::<IssuerKeyStatusReply>(&bytes).expect("decode");
        assert_eq!(back, reply);
        assert_eq!(back.keys[1].retired_at, Some(1_700_000_000));
    }

    #[test]
    fn a_newer_peers_added_field_survives_the_round_trip() {
        // transport.md rule 4: these kinds are app-callable, so an older nest
        // must re-emit a newer app's field rather than dropping it.
        let bytes = encode_canonical(&RotateIssuerKeyReply {
            kid: "k".into(),
            rotated_at: 7,
            extra: [("future".to_string(), Value::Integer(1))]
                .into_iter()
                .collect(),
        })
        .expect("encode");
        let back = decode::<RotateIssuerKeyReply>(&bytes).expect("decode");
        assert_eq!(back.extra.get("future"), Some(&Value::Integer(1)));
    }
}
