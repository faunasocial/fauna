//! Wire types for `fauna.atproto.oracle.service_auth` — the oracle's ATProto
//! door (TP11, `docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle*, the `atproto.service_auth` row,
//! and *The bridge-held custodian*).
//!
//! Principal-only: a third-party principal holding a live
//! `identity.op:atproto.service_auth` grant asks for a short-lived ATProto
//! service JWT — for one `(aud, lxm)` pair its consented manifest declared —
//! signed with the account's repo signing key. The nest decides; the
//! first-party `atproto.pds` bridge, the key's only holder, mints. The caller
//! carries the token to the service itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Value;

/// The kind.
pub const KIND_SERVICE_AUTH: &str = "fauna.atproto.oracle.service_auth";

/// Request for [`KIND_SERVICE_AUTH`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceAuthRequest {
    /// The target service's DID (fragment included, as the manifest declared
    /// it) — matched whole against the principal's declared set.
    pub aud: String,
    /// The one method the token authorizes — matched whole against the
    /// declared set's methods for `aud`.
    pub lxm: String,
    /// The requested expiry, absolute unix seconds. Absent, not in the
    /// future, or past the class's cap
    /// (`fauna_bridge_atproto::service_auth::MAX_LIFETIME_SECS`) → the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<i64>,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for [`KIND_SERVICE_AUTH`]: the signed service JWT.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceAuthReply {
    /// The compact ES256K JWS (`iss` the account's DID, `aud`, `lxm`, `exp`,
    /// `jti`).
    pub token: String,
    /// Forward-compat catch-all.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    #[test]
    fn a_request_round_trips_with_and_without_exp() {
        for exp in [None, Some(1_800_000_000)] {
            let req = ServiceAuthRequest {
                aud: "did:web:feed.example#bsky_fg".into(),
                lxm: "app.bsky.feed.getFeedSkeleton".into(),
                exp,
                ..Default::default()
            };
            let back: ServiceAuthRequest = decode_strict(&encode_canonical(&req).unwrap()).unwrap();
            assert_eq!(back, req);
        }
        let reply = ServiceAuthReply {
            token: "a.b.c".into(),
            ..Default::default()
        };
        let back: ServiceAuthReply = decode_strict(&encode_canonical(&reply).unwrap()).unwrap();
        assert_eq!(back, reply);
    }
}
