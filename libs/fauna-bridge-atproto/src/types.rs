//! Bridge-specific types for Bluesky integration.
//!
//! All types serialize to snake_case field names (no rename_all = "camelCase").
//! This matches the TypeScript contract in `apps/fauna-web/src/lib/bluesky.ts`.

use serde::{Deserialize, Serialize};

/// A linked Bluesky account for a Fauna user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyAccount {
    /// Fauna ActorId (hex-encoded).
    pub actor_id: String,
    /// Bluesky account DID (e.g. did:plc:xxx).
    pub bluesky_did: String,
    /// Bluesky handle (e.g. alice.bsky.social).
    pub bluesky_handle: String,
    /// Token expiry as unix timestamp.
    pub token_expires: i64,
}

// ---------------------------------------------------------------------------
// Content types
// ---------------------------------------------------------------------------

/// The kind of rich-text annotation on a facet span.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BlueskyFacetType {
    Mention { did: String },
    Link { uri: String },
    Tag { tag: String },
}

/// A rich-text facet (mention, link, or hashtag) with flat byte-range fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyFacet {
    /// UTF-8 byte offset of the start of the annotated span.
    pub start: usize,
    /// UTF-8 byte offset of the end of the annotated span.
    pub end: usize,
    pub facet_type: BlueskyFacetType,
}

/// An image attached to a post.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyImage {
    pub thumb: String,
    pub fullsize: String,
    pub alt: String,
}

/// A video attached to a post.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyVideo {
    pub thumb: Option<String>,
    pub playlist: String,
    pub alt: Option<String>,
}

/// An external link embed (card preview).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyExternal {
    pub uri: String,
    pub title: String,
    pub description: String,
    pub thumb: Option<String>,
}

/// A Bluesky user profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyActor {
    pub did: String,
    pub handle: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub avatar: Option<String>,
    pub followers_count: u64,
    pub follows_count: u64,
    pub posts_count: u64,
    /// AT-URI of the viewer's follow record if the viewer follows this actor.
    pub viewer_following: Option<String>,
    /// AT-URI of this actor's follow record if this actor follows the viewer.
    pub viewer_followed_by: Option<String>,
}

/// A Bluesky post (feed item), with flat author fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// A direct message in a Bluesky conversation, with flat sender fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyDm {
    pub id: String,
    pub convo_id: String,
    pub sender_did: String,
    pub sender_handle: String,
    pub sender_display_name: Option<String>,
    pub text: String,
    pub sent_at: String,
}

/// A member of a Bluesky DM conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyConvoMember {
    pub did: String,
    pub handle: String,
    pub display_name: Option<String>,
    pub avatar: Option<String>,
}

/// A Bluesky DM conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyConvo {
    pub id: String,
    pub members: Vec<BlueskyConvoMember>,
    pub last_message: Option<BlueskyDm>,
    pub unread_count: u64,
    pub muted: bool,
}

/// A Bluesky notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyNotification {
    /// The AT-URI of the record that *caused* this notification — the like,
    /// the follow, the reply post. Distinct per notification, which is what
    /// makes it the dedup token the unified notifications table needs: that
    /// table dedups on (actor, type, sender, content), so without a
    /// per-notification identifier every Bluesky like for one actor collapses
    /// into a single row forever.
    pub uri: String,
    pub reason: String,
    pub author_did: String,
    pub author_handle: String,
    pub author_display_name: Option<String>,
    pub author_avatar: Option<String>,
    pub subject_uri: Option<String>,
    pub record_text: Option<String>,
    pub indexed_at: String,
    pub is_read: bool,
}

/// A Bluesky feed generator (custom feed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyFeedGenerator {
    pub uri: String,
    pub did: String,
    pub display_name: String,
    pub description: Option<String>,
    pub avatar: Option<String>,
    pub like_count: u64,
}
