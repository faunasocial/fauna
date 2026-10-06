//! UniFFI façade for the `fauna.posts.*` WS-RPC kinds — the create / get /
//! interact plane the feed composer + post-interaction affordances drive.
//!
//! [`FfiPostsClient`] wraps `fauna_client_posts::PostsClient` (which in turn
//! wraps the shared `NestClient`); the mirror record below is the FFI-visible
//! shape of `fauna_protocol::posts::PostInteractReply`. The Rust-native Linux
//! app calls the same `PostsClient` directly — this seam gives Apple /
//! Windows / Android the identical surface over UniFFI. Construct via
//! [`crate::nest_client::FfiNestClient::posts`].
//!
//! Wire convention: this crate is **transport only**. `posts_create` takes the
//! already-built signed-post bytes (the embed-as-bytes wire of a signed `Post`:
//! canonical dag-cbor inner body, sign-over-CID, wrapped `{envelope, bytes}` —
//! see `docs/goal/architecture/serialization.md` § Sign-over-CID / Embed-as-
//! bytes); building + signing those bytes is the caller's job. Hex ids
//! (`post_id`) cross as `String` — the posts protocol already hex-encodes them.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_posts::PostsClient;

use crate::{FfiError, stringify};

// ── interact reply mirror ────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::posts::PostInteractReply`]. The
/// protocol-specific result rides as a JSON string the caller deserializes
/// per `action` (the heterogeneous HTTP-twin reply bodies, round-tripped
/// byte-for-byte).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiPostInteractReply {
    /// Resolved action (`like` / `unlike` / `reply` / `repost` / `unrepost` /
    /// `quote`).
    pub action: String,
    /// The post's origin protocol (`fauna` / `bluesky` / `nostr` /
    /// `activitypub` / `email`).
    pub source: String,
    /// Protocol-specific result payload as a JSON string.
    pub result: String,
}

/// FFI mirror of [`fauna_protocol::posts::PostDeleteReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiPostDeleteReply {
    /// Hex-encoded 32-byte post digest the tombstone named (echo).
    pub post_id: String,
    /// `true` when this call newly removed the post; `false` when the post
    /// was already gone — the idempotent success, never an error.
    pub deleted: bool,
}

// ── FfiPostsClient ─────────────────────────────────────────────────────────

/// UniFFI handle for the `fauna.posts.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::posts`]; methods are exposed to Swift
/// as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiPostsClient {
    nest: Arc<NestClient>,
    /// This login's content-index arm — the posts **trickle chokepoint**'s
    /// path to the builder (`content-index.md` § Ingest triggers, v1 → the
    /// posts ruling). Shared with [`crate::nest_client::FfiNestClient`]'s own
    /// holder, so a posts handle minted before login still sees the arm once a
    /// conversations session lands — the same late-population contract as
    /// `attach_local_search_index`. `None` (no session yet, a phone that never
    /// builds) drops the trickle silently: the walk is the correctness
    /// carrier, so the cost is freshness only.
    #[cfg(feature = "conversations-session")]
    index_arm: crate::index_launch::IndexArmHolder,
}

impl FfiPostsClient {
    #[cfg(feature = "conversations-session")]
    pub(crate) fn from_nest(
        nest: Arc<NestClient>,
        index_arm: crate::index_launch::IndexArmHolder,
    ) -> Arc<Self> {
        Arc::new(Self { nest, index_arm })
    }

    #[cfg(not(feature = "conversations-session"))]
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> PostsClient<Arc<NestClient>> {
        PostsClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiPostsClient {
    /// `fauna.posts.create` — store a post. `body` is the raw signed-post bytes
    /// (embed-as-bytes wire; built + signed by the caller). Returns the
    /// content-addressed hex `post_id` (`blake3(body)`).
    pub async fn posts_create(&self, body: Vec<u8>) -> Result<String, FfiError> {
        // Extract the index text before the send consumes the bytes — from the
        // bytes themselves (`Post::body_text`, the nest's own extraction),
        // never from any composer state the caller holds.
        #[cfg(feature = "conversations-session")]
        let index_text =
            fauna_core::data::Post::decode_resolved_bytes(&body).map(|p| p.body_text());
        let reply = self.client().posts_create(body).await.map_err(stringify)?;
        // The posts trickle chokepoint, FFI flavor: the create is
        // nest-confirmed, so the four UniFFI apps' just-composed post becomes
        // locally searchable now instead of at the next reconcile walk — with
        // zero per-app glue, because the arm holder is the factory's.
        #[cfg(feature = "conversations-session")]
        if let Some(text) = index_text {
            let arm = self.index_arm.lock().unwrap().clone();
            if let Some(arm) = arm {
                arm.launcher.observe_own_post(&reply.post_id, &text);
            }
        }
        Ok(reply.post_id)
    }

    /// `fauna.posts.get` — fetch the raw resolved post bytes for `post_id`
    /// (hex `[u8; 32]`). A missing / quarantine-gated post surfaces as a
    /// `fauna.posts.not_found` error.
    pub async fn posts_get(&self, post_id: String) -> Result<Vec<u8>, FfiError> {
        let reply = self.client().posts_get(post_id).await.map_err(stringify)?;
        Ok(reply.body.into_vec())
    }

    /// `fauna.posts.delete` — destroy the caller's own post. `tombstone` is
    /// the raw signed-tombstone bytes (embed-as-bytes wire; build + sign
    /// with `sign_and_pack`, exactly as `posts_create` takes already-signed
    /// post bytes — signing lives with the caller). `deleted: false` is the
    /// idempotent already-gone success, never an error.
    pub async fn posts_delete(&self, tombstone: Vec<u8>) -> Result<FfiPostDeleteReply, FfiError> {
        let reply = self
            .client()
            .posts_delete(tombstone)
            .await
            .map_err(stringify)?;
        Ok(FfiPostDeleteReply {
            post_id: reply.post_id,
            deleted: reply.deleted,
        })
    }

    /// `fauna.posts.interact` — like / unlike / reply / repost / unrepost /
    /// quote on `post_id` (hex `[u8; 32]`). `body` is the optional text for
    /// reply / quote.
    pub async fn posts_interact(
        &self,
        post_id: String,
        action: String,
        body: Option<String>,
    ) -> Result<FfiPostInteractReply, FfiError> {
        let reply = self
            .client()
            .posts_interact(post_id, action, body)
            .await
            .map_err(stringify)?;
        Ok(FfiPostInteractReply {
            action: reply.action,
            source: reply.source,
            result: reply.result,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interact_reply_record_constructs() {
        let r = FfiPostInteractReply {
            action: "like".into(),
            source: "fauna".into(),
            result: r#"{"ok":true}"#.into(),
        };
        assert_eq!(r.action, "like");
        assert_eq!(r.source, "fauna");
        assert_eq!(r.result, r#"{"ok":true}"#);
    }
}
