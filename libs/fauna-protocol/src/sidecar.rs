//! The internal sidecar WS-RPC channel's handshake — `fauna.sidecar.hello`.
//!
//! The FIRST frame on any internal sidecar channel, in the sidecar→nest
//! (dialer→listener) direction. **Channel-generic**: the sidecar proves
//! possession of its nest-minted bearer token and declares the scope it wants
//! the channel bound to; the nest verifies the token against its
//! `sidecar-token-*` map and binds the scope. **One-way auth** — the nest is the
//! loopback listener the co-located sidecar dials, so it needs no counter-proof.
//! `transport.md` § Future directions is the transport authority.
//!
//! Today's one rider is the iroh relay ([`crate::relay`]), which attests its
//! X25519 public key in [`SidecarHello::extra`]. The channel-generic log-plane
//! kind rides beside it ([`crate::log_plane`]).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

/// Reserved handshake kind: the first Request on any sidecar channel, in the
/// sidecar→nest direction. No other kind flows until it verifies.
pub const KIND_SIDECAR_HELLO: &str = "fauna.sidecar.hello";

/// Wire payload of the `fauna.sidecar.hello` Request (sidecar → nest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SidecarHello {
    /// The nest-minted 64-char-hex bearer token the sidecar received via
    /// `FAUNA_SIDECAR_TOKEN`.
    pub token: String,
    /// The scope the sidecar requests this channel be bound to (e.g.
    /// `"relay"`). Must parse to a `SidecarScope` the token grants — the
    /// nest verifies this against its token map (see `sidecar_channel`).
    pub scope: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Wire payload of the hello Reply (nest → sidecar). Success is carried by the
/// Reply's `ok = true`; an `ok = false` Reply (an `RpcError` payload) means the
/// handshake was rejected and the connection is torn down.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SidecarHelloReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    /// Round-trip a value through canonical DAG-CBOR and assert it decodes back
    /// identically. `encode_canonical` errors on any float, so a successful
    /// encode also proves the type is float-free (the wire invariant).
    fn round_trip<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let bytes = encode_canonical(value).expect("canonical encode (no floats)");
        let decoded: T = decode_strict(&bytes).expect("strict decode");
        assert_eq!(&decoded, value);
    }

    #[test]
    fn sidecar_hello_round_trips() {
        round_trip(&SidecarHello {
            token: "ab".repeat(32), // 64-char hex, the token shape
            scope: "relay".into(),
            extra: BTreeMap::new(),
        });
        round_trip(&SidecarHelloReply::default());
    }
}
