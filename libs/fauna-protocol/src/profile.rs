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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfileGetReply {
    /// Raw stored `Profile` content bytes (CBOR `bstr`) — served verbatim,
    /// decoded client-side (`fauna-client-profile::decode_profile`). A
    /// missing profile surfaces as a `fauna.profile.not_found` `RpcError`,
    /// not an empty `body`.
    pub body: ByteBuf,
    /// The profile's **home** nest's identity (hex 32-byte `nest_actor_id`),
    /// stamped by the reader's own nest on a `fauna.profile.get_remote`
    /// relay: the `peer_nest_id` it dialed the federation channel to the
    /// home as — its own verified knowledge, never the home's claim relayed,
    /// and never the actor-signed `Profile.nests[0].nest_id`. The reader's
    /// foreign byte plane graduates a pin against it before fetching the
    /// profile's avatar and banner from the home — the post arm's stamp,
    /// `PostGetReply::home_nest_actor_id`'s twin (`transport-trust.md`
    /// § Mechanism for the federation-granted row → *The post arm* → *A
    /// relayed profile's pictures*). Absent (and omitted from the wire) on a
    /// same-nest `fauna.profile.get`, so that reply is byte-identical to
    /// before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_nest_actor_id: Option<String>,
    /// The owner's latest **identity-signed** profile (raw stored bytes, as
    /// `body`), served only on the owner's OWN `fauna.profile.get` and only
    /// while `body` is delegated — signed by the nest-held authoring sub-key
    /// after an external app's edit. It is the owner's last word the writers'
    /// base admission takes `audience`, `nests` and `recovery_head` from
    /// (`profile.md` § Persistence → *After an identity succession, the
    /// successor RE-PUBLISHES* → base-admission rule 3; § Where logic lives →
    /// *Profile publish/edit* → *Write semantics*). Absent (and omitted from
    /// the wire) on every other read — another actor's, the federation serve,
    /// the relay — so those replies are byte-identical to before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_direct_body: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.profile.get_remote ────────────────────────────────────────────

/// The same read as [`ProfileGetRequest`], for an actor homed on **another
/// nest**: the caller's own nest originates `fauna.federation.profile.get` to
/// `nest_url` and hands back the home's signed bytes, unchanged and unstored
/// (`docs/goal/ui/profile.md` § Where logic lives → *The remote read*). The
/// home serves only an actor whose latest signed profile opted in
/// (`Profile.audience = Everyone`); every other case — not hosted, no
/// profile, not opted in — is the same `fauna.profile.not_found` the
/// same-nest read answers.
///
/// A **distinct kind** rather than an additive `nest_url` on `profile.get`,
/// for the `posts.get_remote` reason: an old own-nest that ignored the field
/// would answer `not_found` from its own store — the very answer that sent
/// the client to the relay — so the door would stay silently shut. An
/// unknown kind fails loud.
///
/// The reply is [`ProfileGetReply`]: `body` is the home's stored bytes,
/// byte-identical to the home's own `profile.get`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfileGetRemoteRequest {
    /// Hex-encoded `actor_id` (32 bytes) of the profile to fetch.
    pub actor_id: String,
    /// The actor's home nest — the nest that stores the profile.
    pub nest_url: String,
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
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: ProfileGetReply = decode_strict(&bytes).unwrap();
        assert_eq!(reply, back);
        assert_eq!(back.body.as_ref(), &[1u8, 2, 3, 4]);
    }

    #[test]
    fn profile_get_reply_home_nest_actor_id_round_trips_and_is_additive() {
        // A same-nest reply omits the stamp: its wire is byte-identical to a
        // reply built before `home_nest_actor_id` existed, and decodes to None.
        let same_nest = ProfileGetReply {
            body: ByteBuf::from(vec![1u8, 2, 3, 4]),
            ..Default::default()
        };
        let same_nest_bytes = encode_canonical(&same_nest).unwrap();
        #[derive(Serialize)]
        struct Before {
            body: ByteBuf,
        }
        let before = encode_canonical(&Before {
            body: ByteBuf::from(vec![1u8, 2, 3, 4]),
        })
        .unwrap();
        assert_eq!(same_nest_bytes, before, "None adds nothing to the wire");
        let decoded: ProfileGetReply = decode_strict(&same_nest_bytes).unwrap();
        assert!(decoded.home_nest_actor_id.is_none());

        let relayed = ProfileGetReply {
            home_nest_actor_id: Some("ab".repeat(32)),
            ..same_nest.clone()
        };
        let relayed_bytes = encode_canonical(&relayed).unwrap();
        assert_ne!(same_nest_bytes, relayed_bytes, "Some adds the field");
        let decoded: ProfileGetReply = decode_strict(&relayed_bytes).unwrap();
        assert_eq!(relayed, decoded);
    }

    #[test]
    fn profile_get_reply_last_direct_body_round_trips_and_is_additive() {
        // Every read but the owner's own over a delegated latest omits the
        // owner's last identity-signed row: that wire is byte-identical to a
        // reply built before `last_direct_body` existed, and decodes to None.
        let plain = ProfileGetReply {
            body: ByteBuf::from(vec![1u8, 2, 3, 4]),
            ..Default::default()
        };
        let plain_bytes = encode_canonical(&plain).unwrap();
        #[derive(Serialize)]
        struct Before {
            body: ByteBuf,
        }
        let before = encode_canonical(&Before {
            body: ByteBuf::from(vec![1u8, 2, 3, 4]),
        })
        .unwrap();
        assert_eq!(plain_bytes, before, "None adds nothing to the wire");
        let decoded: ProfileGetReply = decode_strict(&plain_bytes).unwrap();
        assert!(decoded.last_direct_body.is_none());

        let own = ProfileGetReply {
            last_direct_body: Some(ByteBuf::from(vec![5u8, 6, 7])),
            ..plain.clone()
        };
        let own_bytes = encode_canonical(&own).unwrap();
        assert_ne!(plain_bytes, own_bytes, "Some adds the field");
        let decoded: ProfileGetReply = decode_strict(&own_bytes).unwrap();
        assert_eq!(own, decoded);
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
