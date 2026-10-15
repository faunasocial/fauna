//! Wire types for `fauna.posts.author.create` / `fauna.posts.author.delete` —
//! the oracle's posting door (TP11, `docs/goal/architecture/key-material-hierarchy.md`
//! § Audience: deployment infrastructure → *The oracle*, the `post.author`
//! row).
//!
//! Principal-only: a third-party principal holding a live
//! `identity.op:post.author` grant hands the account's authoring-key
//! custodian the **unsigned** fields of a public post; the custodian builds
//! the `Post` (author = the account, `created_at` = the nest's clock), signs
//! it with the account's delegated authoring sub-key `K`
//! (`docs/goal/behavior/atproto-pds-full.md` § D10) and ingests it through the
//! own-write path `fauna.posts.create` runs. The delete names a post id; the
//! custodian signs the `Tombstone` only for a post this principal authored.
//!
//! **No audience field exists here, and none is accepted.** The custodian
//! holds neither a period key nor an MLS key, so it authors public posts only:
//! the request has no gate tier, room or sell member, and a request carrying
//! ANY member this build does not name is refused rather than carried — an
//! unknown member may be a restriction a newer client meant, which an older
//! custodian cannot honour, and posting publicly what was meant restricted
//! is the one failure this door must not have.

use std::collections::BTreeMap;

use fauna_core::data::{PostBody, Reference};
use serde::{Deserialize, Serialize};

use crate::Value;

/// The create kind.
pub const KIND_CREATE: &str = "fauna.posts.author.create";

/// The delete kind.
pub const KIND_DELETE: &str = "fauna.posts.author.delete";

/// Request for [`KIND_CREATE`]: the unsigned fields of a public post.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostAuthorCreateRequest {
    /// The post's body, as `fauna.posts.create`'s signed `Post` carries it.
    pub body: PostBody,
    /// Replies, quotes and reposts — each target must be a public post. The
    /// interaction arms (`React`, `Upvote`, `Downvote`) are not this class's.
    #[serde(default)]
    pub references: Vec<Reference>,
    /// Content warning / spoiler text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_warning: Option<String>,
    /// Members this build does not name. The custodian REFUSES a request
    /// where this is non-empty (module doc).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for [`KIND_CREATE`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostAuthorCreateReply {
    /// Hex-encoded `post_id` (`blake3` of the stored wire) — the id
    /// `fauna.posts.create` would have answered for the same bytes.
    pub post_id: String,
    /// Forward-compat catch-all.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for [`KIND_DELETE`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostAuthorDeleteRequest {
    /// Hex-encoded `post_id` of a post this principal authored.
    pub post_id: String,
    /// Forward-compat catch-all.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for [`KIND_DELETE`] — `fauna.posts.delete`'s reply shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostAuthorDeleteReply {
    /// Hex-encoded post id (echo).
    pub post_id: String,
    /// `true` when this call newly removed the post; `false` when it was
    /// already gone — the idempotent success, never an error.
    pub deleted: bool,
    /// Forward-compat catch-all.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    #[test]
    fn a_create_round_trips_and_an_unknown_member_lands_in_extra() {
        let req = PostAuthorCreateRequest {
            body: PostBody::Text {
                content: "hello".into(),
                facets: vec![],
            },
            references: vec![],
            content_warning: None,
            extra: BTreeMap::new(),
        };
        let back: PostAuthorCreateRequest =
            decode_strict(&encode_canonical(&req).unwrap()).unwrap();
        assert_eq!(back, req);

        // A member this build does not name is carried in `extra`, where the
        // custodian sees it and refuses — never dropped silently.
        let mut gated = req.clone();
        gated.extra.insert("gated".into(), Value::Bool(true));
        let back: PostAuthorCreateRequest =
            decode_strict(&encode_canonical(&gated).unwrap()).unwrap();
        assert!(back.extra.contains_key("gated"));

        let del = PostAuthorDeleteRequest {
            post_id: "ab".repeat(32),
            ..Default::default()
        };
        let back: PostAuthorDeleteRequest =
            decode_strict(&encode_canonical(&del).unwrap()).unwrap();
        assert_eq!(back, del);
    }
}
