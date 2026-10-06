//! Internal relay-sidecar WS-RPC channel payloads — `fauna.relay.*`.
//!
//! The wire shape of the **iroh P2P relay sidecar's** half of the internal
//! sidecar channel (`bins/fauna-iroh-relay`; `transport.md` § Future directions
//! is the transport authority, `behavior/p2p.md` § Architecture the relay
//! authority). The relay is a co-located sidecar: it dials nest's loopback
//! `/internal/relay/ws`, runs the `fauna.sidecar.hello` token handshake
//! ([`crate::sidecar::SidecarHello`]), then holds that channel for its whole life:
//! it originates the cert fetch and the admission question below, and the nest
//! originates the one push ([`KIND_RELAY_CERT_CHANGED`]).
//!
//! **The relay carries no `/data` access** (`security.md` § UID isolation): it
//! never reads `/data/acme`. Instead it obtains its `relay.<apex>` TLS cert as an
//! HPKE-sealed blob it opens with its own X25519 — exactly how the mail bridge
//! gets `mail.<apex>`. Two pieces make that work:
//!
//! - **X25519 attestation in the handshake.** The relay puts its X25519 *public*
//!   key (the seal recipient) in the `SidecarHello.extra` flatten-map under
//!   [`HELLO_EXTRA_X25519`], **hex-encoded** — 32-byte ids ride as hex `String`
//!   on this channel, keeping this protocol
//!   crate's lean default build `hex`-free (the hex⇄bytes conversion lives on the
//!   nest/relay handler boundary). The token (loopback, nest-minted) authenticates
//!   *who* may declare an X25519; HPKE itself is the proof-of-possession (only the
//!   holder of the matching secret can open the sealed cert).
//! - **The fetch kind.** [`KIND_RELAY_FETCH_TLS_CERT`] — the relay originates it;
//!   the nest seals the on-disk `relay.<apex>` cert to the connection's attested
//!   X25519 and returns it. The nest determines the `(role, id, domain)` of the
//!   seal, so the request body is empty (forward-compat `extra` only).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{ByteBuf, Value};

/// `SidecarHello.extra` key under which the relay sidecar declares its X25519
/// **public** key (hex-encoded, 64 chars) in the handshake — the recipient the
/// nest HPKE-seals the `relay.<apex>` TLS cert to. The nest records it for the
/// connection and uses it on every [`KIND_RELAY_FETCH_TLS_CERT`] this channel
/// serves; it never persists or trusts an X25519 from any other source.
pub const HELLO_EXTRA_X25519: &str = "x25519";

/// Relay → nest: fetch the `relay.<apex>` TLS cert, HPKE-sealed to the X25519 the
/// relay attested in its `fauna.sidecar.hello`. The relay opens it with its own
/// X25519 secret (`fauna_mls::wrapped_blob::unseal_tls_cert`) and serves it.
pub const KIND_RELAY_FETCH_TLS_CERT: &str = "fauna.relay.fetch_tls_cert";

/// Request payload of [`KIND_RELAY_FETCH_TLS_CERT`]. Empty: the nest derives the
/// seal's `(role, id, domain)` from its own state and the connection's attested
/// X25519, so the relay declares nothing here (the bridge `fetch_tls_cert_blob`
/// passes `(bridge_role, bridge_id, domain)` because *several* bridges share the
/// channel; there is exactly one relay per nest, on the apex).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RelayFetchTlsCertRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode (`transport.md` § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply payload of [`KIND_RELAY_FETCH_TLS_CERT`]: the HPKE-sealed
/// `fauna_mls::wrapped_blob::TlsCertBlob` as canonical bytes, or `None` when the
/// nest has no cert on disk yet (the relay retries on its refresh timer).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RelayFetchTlsCertReply {
    pub blob: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode (`transport.md` § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Relay → nest: may this endpoint use the relay? The relay asks once per
/// connecting endpoint, before it serves it, and refuses the endpoint unless the
/// nest answers `admitted: true` — no channel, no answer or an error is a refusal
/// (`behavior/p2p.md` § The relay: the devices of the nest's own members, and
/// nobody else).
pub const KIND_RELAY_ADMIT: &str = "fauna.relay.admit";

/// Request payload of [`KIND_RELAY_ADMIT`]: the connecting endpoint's Ed25519
/// public key, **hex-encoded** (64 chars) like every 32-byte id on this channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RelayAdmitRequest {
    pub endpoint_key: String,
    /// Forward-compat catch-all (`transport.md` § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply payload of [`KIND_RELAY_ADMIT`]. `admitted` is the whole answer: the
/// nest does not say *why* a key is known, so the relay learns nothing about an
/// account from asking.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RelayAdmitReply {
    pub admitted: bool,
    /// Forward-compat catch-all (`transport.md` § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Nest → relay: the certificate the nest serves has changed on disk — fetch it
/// again now ([`KIND_RELAY_FETCH_TLS_CERT`]). Carries no certificate: the relay
/// re-fetches over the same channel, so seal-on-read stays the only way key
/// material leaves the nest. Empty request and reply.
pub const KIND_RELAY_CERT_CHANGED: &str = "fauna.relay.cert_changed";

/// Request payload of [`KIND_RELAY_CERT_CHANGED`] — empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RelayCertChangedRequest {
    /// Forward-compat catch-all (`transport.md` § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply payload of [`KIND_RELAY_CERT_CHANGED`] — empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RelayCertChangedReply {
    /// Forward-compat catch-all (`transport.md` § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};

    fn round_trip<T>(v: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let bytes = encode_canonical(v).expect("encode");
        let back: T = decode_strict(&bytes).expect("decode");
        assert_eq!(&back, v);
    }

    #[test]
    fn fetch_request_and_reply_round_trip() {
        round_trip(&RelayFetchTlsCertRequest::default());
        round_trip(&RelayFetchTlsCertReply {
            blob: Some(ByteBuf::from(vec![1u8, 2, 3, 4])),
            extra: Default::default(),
        });
        round_trip(&RelayFetchTlsCertReply {
            blob: None,
            extra: Default::default(),
        });
    }

    #[test]
    fn admit_and_cert_changed_round_trip() {
        round_trip(&RelayAdmitRequest {
            endpoint_key: "ab".repeat(32),
            extra: Default::default(),
        });
        round_trip(&RelayAdmitReply {
            admitted: true,
            extra: Default::default(),
        });
        round_trip(&RelayCertChangedRequest::default());
        round_trip(&RelayCertChangedReply::default());
    }

    /// A relay hello carries its X25519 pubkey as hex under the agreed extra key,
    /// and the whole hello round-trips through the canonical codec.
    #[test]
    fn hello_extra_carries_x25519_hex() {
        let mut hello = crate::sidecar::SidecarHello {
            token: "f".repeat(64),
            scope: "relay".into(),
            extra: Default::default(),
        };
        let x25519_hex = "ab".repeat(32);
        hello.extra.insert(
            HELLO_EXTRA_X25519.to_string(),
            Value::String(x25519_hex.clone()),
        );
        let bytes = encode_canonical(&hello).expect("encode");
        let back: crate::sidecar::SidecarHello = decode_strict(&bytes).expect("decode");
        assert_eq!(
            back.extra.get(HELLO_EXTRA_X25519),
            Some(&Value::String(x25519_hex))
        );
    }
}
