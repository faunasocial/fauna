//! User-facing WS-RPC payload types for the **Bluesky-native thread view**
//! — the kind `bluesky.feed.thread`, the one genuinely protocol-unique
//! consume-side Bluesky surface that keeps a `bluesky.*` kind (auth /
//! settings / follows fold into the unified `fauna.bridges.*`; interactions
//! and notifications into the unified `fauna.posts.*` / `fauna.notifications.*`
//! — `docs/goal/behavior/bridges.md` § Bluesky-native thread view,
//! `docs/goal/architecture/api-layers.md` § Bluesky).
//!
//! The kind replaces the two deprecated HTTP twins (`GET /api/v1/bluesky/
//! feed/thread/{uri}` + `GET /api/v1/bluesky/thread?post_id={hex}`). It
//! restores the caller's Bluesky OAuth agent, calls
//! `app.bsky.feed.getPostThread`, and returns the translated thread as a
//! **flat list of posts** (ancestors oldest-first, the focal post, then its
//! direct replies — exactly the `translate_thread` shape, *not* a nested
//! tree).
//!
//! These mirror the bridge-side types in
//! `libs/fauna-bridge-atproto/src/types.rs`, made **float-free** for canonical
//! dag-cbor (counts are `u64`, facet byte-offsets are `u64`; the bridge's
//! `usize`/`u64` widen losslessly). `fauna-protocol` must **not** depend on
//! `fauna-bridge-atproto` (it compiles for wasm, atproto does not), so the
//! `bridge → protocol` conversion lives nest-side in
//! `bins/fauna-nest/src/bluesky/bluesky_handlers.rs` rather than as a `From`
//! impl here (the orphan rule forbids it either way: both types would be
//! foreign to whichever crate hosts the impl).
//!
//! Kind registry entry: `kind.rs::register_bluesky_kinds`. Nest handler +
//! permission gating: `bins/fauna-nest/src/bluesky/bluesky_handlers.rs`.

use crate::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The kind of rich-text annotation on a facet span.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BlueskyFacetType {
    Mention {
        did: String,
    },
    Link {
        uri: String,
    },
    Tag {
        tag: String,
    },
    /// A facet kind a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// carrying). Its span renders as plain text.
    #[serde(untagged)]
    Unknown(fauna_core::carried::CarriedValue),
}

/// A rich-text facet (mention, link, or hashtag) with flat byte-range fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlueskyFacet {
    /// UTF-8 byte offset of the start of the annotated span.
    pub start: u64,
    /// UTF-8 byte offset of the end of the annotated span.
    pub end: u64,
    pub facet_type: BlueskyFacetType,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// An image attached to a post.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlueskyImage {
    pub thumb: String,
    pub fullsize: String,
    pub alt: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A video attached to a post.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlueskyVideo {
    pub thumb: Option<String>,
    pub playlist: String,
    pub alt: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// An external link embed (card preview).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlueskyExternal {
    pub uri: String,
    pub title: String,
    pub description: String,
    pub thumb: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A Bluesky post (thread item), with flat author fields. Mirrors
/// `fauna_bridge_atproto::types::BlueskyPost`, float-free.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlueskyPost {
    /// Opaque post identifier (used by the UI).
    pub id: String,
    /// AT-URI of the post record (at://did/.../rkey).
    pub at_uri: String,
    pub cid: String,
    pub author_did: String,
    pub author_handle: String,
    pub author_display_name: Option<String>,
    pub author_avatar: Option<String>,
    pub text: String,
    pub facets: Vec<BlueskyFacet>,
    pub images: Vec<BlueskyImage>,
    pub video: Option<BlueskyVideo>,
    pub external: Option<BlueskyExternal>,
    pub quote: Option<Box<BlueskyPost>>,
    /// AT-URI of the parent post if this is a reply.
    pub reply_parent: Option<String>,
    /// AT-URI of the thread root if this is a reply.
    pub reply_root: Option<String>,
    /// DID of the account that reposted this, if this is a repost item.
    pub reposted_by: Option<String>,
    pub like_count: u64,
    pub repost_count: u64,
    pub reply_count: u64,
    /// AT-URI of the viewer's like record, if liked.
    pub viewer_like: Option<String>,
    /// AT-URI of the viewer's repost record, if reposted.
    pub viewer_repost: Option<String>,
    pub labels: Vec<String>,
    pub created_at: String,
    /// Source tag, e.g. "bluesky".
    pub source: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `bluesky.feed.thread` — the thread to fetch, addressed either
/// by AT-URI (android, which holds the Bluesky AT-URI directly) or by Fauna
/// post id (linux, which holds a crossposted Fauna post and looks up its
/// AT-URI through the `bluesky_posts` mapping). Externally-tagged enum — one
/// of the two variants is present on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BlueskyThreadRequest {
    /// AT-URI of the post whose thread to fetch (`at://did/.../rkey`).
    AtUri { uri: String },
    /// Raw 32-byte Fauna post id (the hash, not hex). The handler hex-encodes
    /// it and resolves the AT-URI from the `bluesky_posts` crosspost mapping;
    /// a missing mapping is `bluesky.feed.not_found`.
    PostId {
        #[serde(with = "serde_bytes")]
        post_id: Vec<u8>,
    },
}

/// Reply for `bluesky.feed.thread` — the translated thread as a flat list of
/// posts (ancestors oldest-first, the focal post, then its direct replies),
/// matching `fauna_bridge_atproto::translate::translate_thread`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlueskyThreadReply {
    pub posts: Vec<BlueskyPost>,
    /// Index into `posts` of the requested (focal) post, filled by the nest —
    /// the ancestor count.
    pub focal_index: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn sample_post() -> BlueskyPost {
        BlueskyPost {
            id: "post-1".into(),
            at_uri: "at://did:plc:abc/app.bsky.feed.post/xyz".into(),
            cid: "bafy...".into(),
            author_did: "did:plc:abc".into(),
            author_handle: "alice.bsky.social".into(),
            author_display_name: Some("Alice".into()),
            author_avatar: None,
            text: "hello #fauna".into(),
            facets: vec![BlueskyFacet {
                start: 6,
                end: 12,
                facet_type: BlueskyFacetType::Tag {
                    tag: "fauna".into(),
                },
                extra: Default::default(),
            }],
            images: vec![BlueskyImage {
                thumb: "t".into(),
                fullsize: "f".into(),
                alt: "a".into(),
                extra: Default::default(),
            }],
            video: None,
            external: None,
            quote: Some(Box::new(BlueskyPost {
                id: "quoted".into(),
                text: "quoted post".into(),
                like_count: 3,
                ..Default::default()
            })),
            reply_parent: Some("at://parent".into()),
            reply_root: Some("at://root".into()),
            reposted_by: None,
            like_count: 10,
            repost_count: 2,
            reply_count: 1,
            viewer_like: Some("at://my-like".into()),
            viewer_repost: None,
            labels: vec!["nsfw".into()],
            created_at: "2026-06-05T00:00:00Z".into(),
            source: "bluesky".into(),
            extra: Default::default(),
        }
    }

    #[test]
    fn post_round_trips() {
        let post = sample_post();
        let bytes = encode_canonical(&post).unwrap();
        let decoded: BlueskyPost = decode(&bytes).unwrap();
        assert_eq!(post, decoded);
        // The recursive `quote` box survives the round-trip.
        assert_eq!(decoded.quote.unwrap().text, "quoted post");
    }

    #[test]
    fn post_canonical_re_encodes_identically() {
        let post = sample_post();
        let bytes1 = encode_canonical(&post).unwrap();
        let decoded: BlueskyPost = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn thread_reply_round_trips() {
        let reply = BlueskyThreadReply {
            posts: vec![sample_post(), BlueskyPost::default()],
            focal_index: 1,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: BlueskyThreadReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn thread_reply_default_is_empty() {
        let reply = BlueskyThreadReply::default();
        assert!(reply.posts.is_empty());
    }

    #[test]
    fn request_at_uri_round_trips() {
        let req = BlueskyThreadRequest::AtUri {
            uri: "at://did:plc:abc/app.bsky.feed.post/xyz".into(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: BlueskyThreadRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn request_post_id_round_trips() {
        let req = BlueskyThreadRequest::PostId {
            post_id: vec![0xab; 32],
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: BlueskyThreadRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }
}
