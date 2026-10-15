//! Wire types for `fauna.activitypub.oracle.http_sig` — the oracle's
//! ActivityPub door (TP11, `docs/goal/architecture/key-material-hierarchy.md`
//! § Audience: deployment infrastructure → *The oracle*, the
//! `activitypub.http_sig` row).
//!
//! Principal-only: a third-party principal holding a live
//! `identity.op:activitypub.http_sig` grant asks the account's actor-key
//! custodian for an HTTP Signature over one outbound ActivityPub request. The
//! nest chooses the `Date` (its own clock) and computes the `Digest` from the
//! body it was shown, so the reply hands both back with the `Signature`
//! value: the caller sends exactly the headers the signature covers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::Value;

/// The kind.
pub const KIND_HTTP_SIG: &str = "fauna.activitypub.oracle.http_sig";

/// Request for [`KIND_HTTP_SIG`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpSigRequest {
    /// `get` (an authorized fetch, no body) or `post` (an inbox delivery).
    pub method: String,
    /// The absolute `https` URL the request goes to; its host must be one the
    /// principal's manifest declared in `activitypub_peers`.
    pub target: String,
    /// The `post` body — the AS2 activity itself, at most the inbox's 1 MB
    /// cap. The custodian reads it: it signs only this account's own
    /// activities, never a sovereign shape, and computes the `Digest` from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<ByteBuf>,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for [`KIND_HTTP_SIG`]: the `Signature` header value and the header
/// values it covers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpSigReply {
    /// The `Signature` header value (draft-cavage; `rsa-sha256`).
    pub signature: String,
    /// The `keyId` it names — the account's actor URL plus `#main-key`.
    pub key_id: String,
    /// The `Host` header value the signature covers.
    pub host: String,
    /// The `Date` header value the signature covers — the nest's clock.
    pub date: String,
    /// The `Digest` header value the signature covers (`SHA-256=…`), for a
    /// `post`; absent for a `get`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Forward-compat catch-all.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    #[test]
    fn a_get_round_trips_without_body_or_digest() {
        let req = HttpSigRequest {
            method: "get".into(),
            target: "https://mastodon.example/users/bob".into(),
            ..Default::default()
        };
        let back: HttpSigRequest = decode_strict(&encode_canonical(&req).unwrap()).unwrap();
        assert_eq!(back, req);
        let reply = HttpSigReply {
            signature: "keyId=\"k\"".into(),
            key_id: "k".into(),
            host: "mastodon.example".into(),
            date: "Fri, 09 Oct 2026 00:00:00 GMT".into(),
            ..Default::default()
        };
        let back: HttpSigReply = decode_strict(&encode_canonical(&reply).unwrap()).unwrap();
        assert_eq!(back, reply);
    }
}
