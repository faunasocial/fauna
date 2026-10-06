//! User-facing WS-RPC payload types for the profile surface — the
//! per-user *detail* fetch the Profile page (and any contact-row / feed-
//! author tap-through) renders identity from. See `docs/goal/ui/profile.md`
//! § Where logic lives.
//!
//! `fauna.profile.get(actor_id) -> Profile` is the **read** half of the
//! profile shell's one net-new shared-Rust dependency. It lives in the
//! singular `fauna.profile.*` namespace alongside `fauna.profile.handle.change`
//! (do NOT fork a plural `fauna.profiles.*`).
//!
//! - `get` carries the target `actor_id` (hex); the reply carries the raw
//!   stored `Profile` content bytes as a CBOR `bstr` (the nest serves the
//!   bytes it stored verbatim — the same pass-through shape as
//!   `fauna.posts.get`'s `body`). An actor with no stored profile surfaces
//!   as a `fauna.profile.not_found` `RpcError`, not an empty `body`.
//!
//! The reply is shape-agnostic: the stored bytes are decoded client-side by
//! `fauna-client-profile::decode_profile` (the shared signed-only verify helper
//! `fauna_core::encoding::decode_profile`), which accepts both the signed
//! `EmbedAsBytes` wire (verify-on-receipt, the canonical stored shape) and a
//! bare canonical `Profile` (the pre-publish-path `activitypub::actor_routes`
//! read shape).
//!
//! `fauna.profile.set(body) -> ()` is the **write** half (ratified 2026-06-18,
//! `profile.md` § Where logic lives → *Profile publish/edit*). It is a
//! **client-originated own-write** (the nest holds no secret key and cannot
//! sign a `Profile`): the owner's client `sign_and_pack`s its `Profile` into
//! the signed `EmbedAsBytes` wire and uploads it as `ProfileSetRequest::body`;
//! the nest verifies the Ed25519 signature, asserts `profile.actor_id ==`
//! authenticated caller, then stores the signed bytes as the `schema='profile'`
//! content row (immediate; append + keep-latest prune). Mirrors
//! `fauna.posts.create`'s `body: ByteBuf` carrying the signed wire.
//!
//! Kind registry entries live in `kind.rs::register_profile_kinds`.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.profile.get ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileGetRequest {
    /// Hex-encoded `actor_id` (32 bytes) of the profile to fetch — the
    /// viewer's own (own profile) or anyone else's (tap-through). Opaque
    /// hex on the wire; the nest decodes it to the 32-byte author key.
    pub actor_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileGetReply {
    /// Raw stored `Profile` content bytes (CBOR `bstr`) — served verbatim,
    /// decoded client-side (`fauna-client-profile::decode_profile`). A
    /// missing profile surfaces as a `fauna.profile.not_found` `RpcError`,
    /// not an empty `body`.
    pub body: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.profile.set ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileSetRequest {
    /// The signed `EmbedAsBytes` wire of the owner's `Profile` (CBOR `bstr`) —
    /// the same at-rest shape the nest stores and `fauna.profile.get` serves
    /// verbatim. The client builds + signs it (`fauna-client-profile::
    /// build_profile`); the nest verifies the Ed25519 signature and asserts the
    /// inner `Profile.actor_id` matches the authenticated caller before storing.
    pub body: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfileSetReply {
    /// Acknowledgement only — the profile is keyed by the caller's `actor_id`
    /// (which the client already holds), so there is no new id to return. A
    /// successful reply means the signed profile is stored and superseded rows
    /// pruned. Defaulted/flattened `extra` keeps the shape additive.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};

    #[test]
    fn profile_get_request_round_trips() {
        let req = ProfileGetRequest {
            actor_id: "ab".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: ProfileGetRequest = decode_strict(&bytes).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn profile_get_reply_round_trips() {
        let reply = ProfileGetReply {
            body: ByteBuf::from(vec![1u8, 2, 3, 4]),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: ProfileGetReply = decode_strict(&bytes).unwrap();
        assert_eq!(reply, back);
        assert_eq!(back.body.as_ref(), &[1u8, 2, 3, 4]);
    }

    #[test]
    fn profile_set_request_round_trips() {
        let req = ProfileSetRequest {
            body: ByteBuf::from(vec![9u8, 8, 7, 6]),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: ProfileSetRequest = decode_strict(&bytes).unwrap();
        assert_eq!(req, back);
        assert_eq!(back.body.as_ref(), &[9u8, 8, 7, 6]);
    }

    #[test]
    fn profile_set_reply_round_trips() {
        let reply = ProfileSetReply {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: ProfileSetReply = decode_strict(&bytes).unwrap();
        assert_eq!(reply, back);
    }
}
