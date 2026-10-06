//! Feed aggregation types for distributed feed queries.
//!
//! These types support cross-nest feed queries where a user's nest
//! queries remote nests for matching posts and merge-sorts the results.

use serde::{Deserialize, Serialize};

use crate::data::{BodyHint, PostId, Timestamp};
use crate::identity::ActorId;

/// Protocol-agnostic content identifier.
///
/// For Phase 1 (Fauna-only), only the `Fauna` variant is used.
/// Other variants are defined for forward compatibility with
/// cross-protocol feeds (Phase 3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ContentAddress {
    Fauna {
        post_id: PostId,
    },
    ATProto {
        at_uri: String,
    },
    ActivityPub {
        object_uri: String,
    },
    Nostr {
        #[serde(with = "serde_bytes")]
        event_id: [u8; 32],
    },
    RSS {
        guid: String,
    },
}

/// Protocol-agnostic author identity.
///
/// For Phase 1 (Fauna-only), only the `Fauna` variant is used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum UnifiedIdentity {
    Fauna {
        actor_id: ActorId,
    },
    ATProto {
        did: String,
        fauna_link: Option<ActorId>,
    },
    ActivityPub {
        actor_uri: String,
        fauna_link: Option<ActorId>,
    },
    Nostr {
        #[serde(with = "serde_bytes")]
        npub: [u8; 32],
        fauna_link: Option<ActorId>,
    },
    RSS {
        feed_url: String,
    },
}

/// Source protocol tag for UI rendering and interaction routing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceTag {
    Fauna,
    ATProto,
    ActivityPub,
    Nostr,
    RSS,
}

/// Lightweight post metadata extracted during indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateMetadata {
    pub tags: Vec<String>,
    pub has_media: bool,
    pub is_reply: bool,
    pub body_hint: Option<BodyHint>,
}

/// The universal unit of feed content, protocol-agnostic.
///
/// Extends the concept of `ScoredEvent` (from `scoring.rs`) with
/// cross-protocol metadata. For Fauna-native posts, constructed by
/// wrapping fields in `ContentAddress::Fauna` and `UnifiedIdentity::Fauna`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredCandidate {
    pub post_id: ContentAddress,
    pub author: UnifiedIdentity,
    pub source: SourceTag,
    /// The peer's advertised per-platform origin token (`fauna_core::source`
    /// vocabulary — `"fauna"`, `"facebook"`, `"bluesky"`, …), already
    /// normalized-and-bounded by the parser that built this candidate. This is
    /// what the receiving nest indexes locally as the post's `content.source`
    /// (`docs/goal/ui/feed.md` § `PostSummary.source` — the protocol-list
    /// `classify_sources()` reads); `source: SourceTag` above is a coarser
    /// Fauna/ATProto/ActivityPub/Nostr/RSS classification of how the
    /// candidate was *obtained*, not the specific platform token to badge —
    /// the two fields answer different questions and neither substitutes for
    /// the other.
    pub source_token: String,
    pub created_at: Timestamp,
    /// `None` means unscored (scorer unavailable or not requested).
    /// `Some(0.0)` means scorer ran and produced zero.
    pub score: Option<f64>,
    pub scorer_version: Option<u64>,
    pub fetch_url: String,
    pub metadata: CandidateMetadata,
    #[serde(default)]
    pub references: Vec<PostReference>,
}

/// A reference from one post to another (repost, quote, reply).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostReference {
    #[serde(with = "serde_bytes")]
    pub post_id: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub author: Vec<u8>,
    pub nest_url: Option<String>,
    pub ref_type: PostReferenceType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PostReferenceType {
    Repost,
    Quote,
    Reply,
}
