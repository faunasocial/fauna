//! Nest transport/abuse-policy WS-RPC payload types — the
//! `fauna.transport.{get,put}_policy` plane.
//!
//! These configure nest's **own** client-facing TLS listener abuse caps
//! (today: the per-source-IP concurrent-connection cap the `serve_tls`
//! accept loop enforces), as opposed to the mail-scoped bridge policies in
//! [`crate::bridge_routing`] that nest serves to the Go mail bridge over
//! `fetch_config`. By the product invariant an admin-tunable abuse knob
//! (same class as spam thresholds) is **client-set nest config**, not a
//! deployment env/CLI knob — so the cap lives in this nest-owned policy
//! domain. See `docs/goal/architecture/transport-connection.md` § Abuse posture
//! item (2). Both kinds are **Admin-class** (`bridge_method_allowlist.rs`).
//!
//! A `put` is hot-reloaded onto the live listeners; an unset cap is the
//! constant default, and no environment variable names it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use fauna_cbor::Value;

/// WS-RPC kind: read the effective nest transport/abuse policy (Admin).
/// One source of truth for the client wrapper, the nest router, and the
/// caller-class allowlist.
pub const KIND_GET_TRANSPORT_POLICY: &str = "fauna.transport.get_policy";
/// WS-RPC kind: set the nest transport/abuse policy overrides (Admin).
pub const KIND_PUT_TRANSPORT_POLICY: &str = "fauna.transport.put_policy";

/// `fauna.transport.get_policy` (Admin) — no fields; the policy is
/// deployment-wide, not per-actor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GetTransportPolicyRequest {}

/// `fauna.transport.get_policy` reply — the **resolved** (override-or-
/// default) transport policy the TLS accept loop enforces. Non-optional:
/// every field is a concrete effective value (the override overlaid onto
/// the catalog default), so an admin form reads back exactly what binds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TransportPolicyView {
    /// Per-source-IP concurrent-connection ceiling on nest's TLS listener
    /// (keyed on the PROXY-v2-resolved real client IP; loopback-exempt).
    pub max_conns_per_ip: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.transport.put_policy` (Admin) — each `Some(v)` sets the override,
/// each `None` keeps the catalog default. A full PUT (the admin form
/// submits the complete policy, so this replaces, it does not merge).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PutTransportPolicyRequest {
    pub max_conns_per_ip: Option<u32>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.transport.put_policy` reply — `{ ok: true }` on success; the
/// typed wrapper discards it (success/failure rides the `Result`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PutTransportPolicyReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict as decode, encode_canonical};

    #[test]
    fn get_request_and_view_round_trip() {
        let req = GetTransportPolicyRequest {};
        let bytes = encode_canonical(&req).unwrap();
        let back: GetTransportPolicyRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        let view = TransportPolicyView {
            max_conns_per_ip: 256,
            ..Default::default()
        };
        let bytes = encode_canonical(&view).unwrap();
        let back: TransportPolicyView = decode(&bytes).unwrap();
        assert_eq!(back, view);
    }

    #[test]
    fn put_request_round_trips_some_and_none() {
        // `Some` override.
        let req = PutTransportPolicyRequest {
            max_conns_per_ip: Some(64),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: PutTransportPolicyRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        // `None` (keep catalog default) — a single Option, not the
        // nested-Option footgun the dag-cbor wire can't represent.
        let req = PutTransportPolicyRequest {
            max_conns_per_ip: None,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: PutTransportPolicyRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn put_reply_round_trips() {
        let reply = PutTransportPolicyReply {
            ok: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: PutTransportPolicyReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }
}
