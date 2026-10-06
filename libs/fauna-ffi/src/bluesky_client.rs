//! UniFFI façade for the one protocol-unique consume-side Bluesky WS-RPC
//! kind — `bluesky.feed.thread`. The crossposted-post thread view the
//! android (and apple / windows) post-detail surface shows for a Bluesky post.
//!
//! [`FfiBlueskyClient`] wraps `fauna_client_bluesky::BlueskyClient` (which in
//! turn wraps the shared `NestClient`); the records below are the FFI-visible
//! flat **display projection** of `fauna_protocol::bluesky::{BlueskyThreadReply,
//! BlueskyPost}` (the rich nested facet/image/video/quote sub-objects no client
//! renders are collapsed to `has_media` + `viewer_*` booleans, the same style
//! as `feed_client::FfiFeedPostItem`). The Rust-native Linux app calls the
//! same `BlueskyClient` directly on the proto types — this seam gives the
//! UniFFI apps the equivalent surface (priority #2). Construct via
//! [`crate::nest_client::FfiNestClient::bluesky`].
//!
//! This is the faithful transport migration of the two deleted HTTP twins
//! (`GET /api/v1/bluesky/feed/thread/{uri}` + `GET /api/v1/bluesky/thread?post_id={hex}`)
//! per `docs/goal/behavior/bridges.md` § Bluesky-native thread view and
//! `docs/goal/architecture/api-layers.md` § Bluesky.
//!
//! **Focal identification.** The reply is a *flat* list (ancestors oldest-first,
//! the focal post, then its direct replies); the nest names the focal post on
//! the wire (`BlueskyThreadReply::focal_index`) and the seam passes it through
//! as [`FfiBlueskyThreadReply::focal_index`], so every UniFFI consumer splits
//! the list into parent/focal/replies off that index without guessing.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_bluesky::BlueskyClient;
use fauna_client_bluesky::bluesky::{BlueskyPost, BlueskyThreadReply, BlueskyThreadRequest};

use crate::{FfiError, stringify};

// ── BlueskyPost projection ─────────────────────────────────────────────────

/// Flat FFI display projection of [`fauna_protocol::bluesky::BlueskyPost`]. The
/// rich nested sub-objects (`facets` / `images` / `video` / `external` /
/// `quote`) are collapsed to `has_media` (any image or video present) and the
/// `viewer_*` booleans (the presence of the viewer's like / repost records) —
/// no UniFFI app renders the nested content today, and `quote` is recursive
/// (`Option<Box<BlueskyPost>>`), which the flat record sidesteps. `created_at`
/// crosses as the raw ISO-8601 string (the consumer parses to its own time
/// type). Mirrors the `feed_client::FfiFeedPostItem` flattening convention.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiBlueskyPost {
    /// Opaque post identifier (the bridge-side id; used by the UI for keys).
    pub id: String,
    /// AT-URI of the post record (`at://did/.../rkey`).
    pub at_uri: String,
    pub cid: String,
    pub author_did: String,
    pub author_handle: String,
    pub author_display_name: Option<String>,
    pub author_avatar: Option<String>,
    pub text: String,
    /// Any image attachment or a video is present.
    pub has_media: bool,
    /// AT-URI of the parent post if this is a reply.
    pub reply_parent: Option<String>,
    /// AT-URI of the thread root if this is a reply.
    pub reply_root: Option<String>,
    pub like_count: u64,
    pub repost_count: u64,
    pub reply_count: u64,
    /// The viewer has liked this post (a `viewer_like` record exists).
    pub viewer_liked: bool,
    /// The viewer has reposted this post (a `viewer_repost` record exists).
    pub viewer_reposted: bool,
    /// ISO-8601 creation timestamp (raw; the consumer parses it).
    pub created_at: String,
    /// Source tag, e.g. "bluesky".
    pub source: String,
}

impl From<BlueskyPost> for FfiBlueskyPost {
    fn from(p: BlueskyPost) -> Self {
        FfiBlueskyPost {
            has_media: !p.images.is_empty() || p.video.is_some(),
            viewer_liked: p.viewer_like.is_some(),
            viewer_reposted: p.viewer_repost.is_some(),
            id: p.id,
            at_uri: p.at_uri,
            cid: p.cid,
            author_did: p.author_did,
            author_handle: p.author_handle,
            author_display_name: p.author_display_name,
            author_avatar: p.author_avatar,
            text: p.text,
            reply_parent: p.reply_parent,
            reply_root: p.reply_root,
            like_count: p.like_count,
            repost_count: p.repost_count,
            reply_count: p.reply_count,
            created_at: p.created_at,
            source: p.source,
        }
    }
}

// ── BlueskyThreadReply mirror ───────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::bluesky::BlueskyThreadReply`] — the
/// translated thread as a flat list of posts (ancestors oldest-first, the
/// focal post, then its direct replies), plus the nest-named `focal_index`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiBlueskyThreadReply {
    pub posts: Vec<FfiBlueskyPost>,
    /// Index into `posts` of the focal post, as named by the nest on the wire.
    /// Lets every UniFFI consumer split into parent/focal/replies without
    /// guessing (priority #2). The consumer guards an empty list.
    pub focal_index: u32,
}

impl From<BlueskyThreadReply> for FfiBlueskyThreadReply {
    fn from(r: BlueskyThreadReply) -> Self {
        FfiBlueskyThreadReply {
            focal_index: r.focal_index,
            posts: r.posts.into_iter().map(Into::into).collect(),
        }
    }
}

// ── FfiBlueskyClient ────────────────────────────────────────────────────────

/// UniFFI handle for the `bluesky.feed.thread` kind. Construct via
/// [`crate::nest_client::FfiNestClient::bluesky`]; methods are exposed to Swift
/// as `async throws` and Kotlin as `suspend fun`. Caller-scoped by
/// construction (the connection knows its caller; the handler restores that
/// actor's Bluesky OAuth agent).
#[derive(uniffi::Object)]
pub struct FfiBlueskyClient {
    nest: Arc<NestClient>,
}

impl FfiBlueskyClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> BlueskyClient<Arc<NestClient>> {
        BlueskyClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiBlueskyClient {
    /// `bluesky.feed.thread` via the `PostId` request variant — fetch the
    /// thread for a **crossposted Fauna post** addressed by its hex `[u8; 32]`
    /// id (what the unified feed surfaces as `FfiFeedPostItem.post_id`). The
    /// handler hex-decodes server-side and resolves the AT-URI through the
    /// `bluesky_posts` crosspost mapping (a missing mapping is a
    /// `bluesky.feed.not_found` error). This is the variant the android +
    /// linux post-detail surfaces use (they hold the Fauna post id, not the
    /// AT-URI).
    pub async fn thread_by_post_id(
        &self,
        post_id_hex: String,
    ) -> Result<FfiBlueskyThreadReply, FfiError> {
        let post_id = hex::decode(&post_id_hex).map_err(|e| FfiError::General {
            msg: format!("invalid post_id hex: {e}"),
        })?;
        let reply = self
            .client()
            .thread(BlueskyThreadRequest::PostId { post_id })
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// `bluesky.feed.thread` via the `AtUri` request variant — fetch the thread
    /// for a post addressed by its Bluesky AT-URI directly (`at://did/.../rkey`).
    /// Provided for surfaces that hold the AT-URI rather than a crossposted
    /// Fauna post id.
    pub async fn thread_by_at_uri(&self, uri: String) -> Result<FfiBlueskyThreadReply, FfiError> {
        let reply = self
            .client()
            .thread(BlueskyThreadRequest::AtUri { uri })
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_proto() -> BlueskyPost {
        BlueskyPost {
            id: "post-1".into(),
            at_uri: "at://did:plc:abc/app.bsky.feed.post/xyz".into(),
            cid: "bafy".into(),
            author_did: "did:plc:abc".into(),
            author_handle: "alice.bsky.social".into(),
            author_display_name: Some("Alice".into()),
            author_avatar: None,
            text: "hello".into(),
            facets: vec![],
            images: vec![],
            video: None,
            external: None,
            quote: None,
            reply_parent: Some("at://did:plc:abc/app.bsky.feed.post/parent".into()),
            reply_root: Some("at://did:plc:abc/app.bsky.feed.post/root".into()),
            reposted_by: None,
            like_count: 3,
            repost_count: 1,
            reply_count: 2,
            viewer_like: Some("at://did:plc:me/app.bsky.feed.like/abc".into()),
            viewer_repost: None,
            labels: vec![],
            created_at: "2026-06-06T00:00:00Z".into(),
            source: "bluesky".into(),
            extra: Default::default(),
        }
    }

    #[test]
    fn post_projection_collapses_media_and_viewer_state() {
        let ffi: FfiBlueskyPost = sample_proto().into();
        assert_eq!(ffi.id, "post-1");
        assert_eq!(ffi.at_uri, "at://did:plc:abc/app.bsky.feed.post/xyz");
        assert_eq!(ffi.author_display_name.as_deref(), Some("Alice"));
        assert!(!ffi.has_media); // no images, no video
        assert!(ffi.viewer_liked); // viewer_like present
        assert!(!ffi.viewer_reposted); // viewer_repost absent
        assert_eq!(ffi.like_count, 3);
        assert_eq!(
            ffi.reply_parent.as_deref(),
            Some("at://did:plc:abc/app.bsky.feed.post/parent")
        );
        assert_eq!(ffi.created_at, "2026-06-06T00:00:00Z");
    }

    #[test]
    fn post_projection_flags_media_for_video_only() {
        let mut proto = sample_proto();
        proto.images = vec![];
        proto.video = Some(fauna_protocol::bluesky::BlueskyVideo {
            thumb: None,
            playlist: "https://cdn/video.m3u8".into(),
            alt: None,
            extra: Default::default(),
        });
        let ffi: FfiBlueskyPost = proto.into();
        assert!(ffi.has_media);
    }

    #[test]
    fn thread_reply_maps_posts_and_passes_focal_index_through() {
        let proto = BlueskyThreadReply {
            posts: vec![sample_proto(), sample_proto()],
            focal_index: 0,
            extra: Default::default(),
        };
        let ffi: FfiBlueskyThreadReply = proto.into();
        assert_eq!(ffi.posts.len(), 2);
        assert_eq!(ffi.focal_index, 0);
    }

    #[test]
    fn focal_index_is_the_wire_value_not_derived_from_links() {
        // The wire names index 2 although no post links to another: the seam
        // carries exactly that, not a positional or branch-point guess.
        let proto = BlueskyThreadReply {
            posts: vec![sample_proto(), sample_proto(), sample_proto()],
            focal_index: 2,
            extra: Default::default(),
        };
        let ffi: FfiBlueskyThreadReply = proto.into();
        assert_eq!(ffi.focal_index, 2);
    }
}
