//! WS-RPC handler for the **Bluesky-native thread view** — the kind
//! `bluesky.feed.thread`. The successor to the deprecated HTTP twins
//! `GET /api/v1/bluesky/feed/thread/{uri}` + `GET /api/v1/bluesky/thread?post_id={hex}`.
//!
//! The one genuinely protocol-unique consume-side Bluesky surface that keeps a
//! `bluesky.*` kind (`bridges.md` § Bluesky-native thread view): auth /
//! settings / follows fold into the unified `fauna.bridges.*`, interactions
//! and notifications into `fauna.posts.*` / `fauna.notifications.*`. The
//! handler restores the caller's Bluesky OAuth agent, calls
//! `app.bsky.feed.getPostThread`, and returns the translated thread as a flat
//! list of posts (`translate_thread` — ancestors oldest-first, focal post,
//! direct replies).
//!
//! Two address modes share the one kind (`BlueskyThreadRequest`):
//! - `AtUri { uri }` — android holds the Bluesky AT-URI directly.
//! - `PostId { post_id }` — linux holds a crossposted Fauna post; the handler
//!   hex-encodes the raw 32-byte id and resolves the AT-URI through the
//!   `bluesky_posts` crosspost mapping (a missing mapping is `not_found`).
//!
//! Permission gate `User` (Admin inherits) via `bridge_method_allowlist`. The
//! `fauna_protocol` wire types are float-free and crate-independent of
//! `fauna_bridge_atproto`; the `bridge → protocol` conversion (`post_to_proto`)
//! lives here, nest-side, where both crates are in scope.

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::{RpcError, Value, bluesky as proto, decode_strict as decode};

use fauna_bridge_atproto::atrium_api::app::bsky::feed::get_post_thread;
use fauna_bridge_atproto::atrium_api::types::Union;
use fauna_bridge_atproto::translate::translate_thread;
use fauna_bridge_atproto::types as bridge;

use crate::bluesky::db_helpers;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

/// The thread's focal post does not exist (no crosspost mapping, or
/// `NotFoundPost` from upstream).
fn not_found(reason: &str) -> RpcError {
    crate::rpc_errors::not_found_ns("bluesky", reason)
}

/// The post is blocked (upstream `BlockedPost`).
fn blocked() -> RpcError {
    RpcError::new("fauna.bluesky.blocked", "error.bluesky.blocked")
}

/// The agent could not be restored (Bluesky not configured / not linked /
/// session expired) or the upstream XRPC call failed.
fn upstream(reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new("fauna.bluesky.upstream", "error.bluesky.upstream");
    e.details = Some(Box::new(Value::String(format!("{reason}"))));
    e
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// Convert a bridge `BlueskyFacetType` into the protocol mirror.
fn facet_type_to_proto(t: &bridge::BlueskyFacetType) -> proto::BlueskyFacetType {
    match t {
        bridge::BlueskyFacetType::Mention { did } => {
            proto::BlueskyFacetType::Mention { did: did.clone() }
        }
        bridge::BlueskyFacetType::Link { uri } => {
            proto::BlueskyFacetType::Link { uri: uri.clone() }
        }
        bridge::BlueskyFacetType::Tag { tag } => proto::BlueskyFacetType::Tag { tag: tag.clone() },
    }
}

/// Convert a bridge `BlueskyPost` into the float-free protocol wire type
/// (recursive through `quote`). The bridge's `usize` facet offsets widen
/// losslessly to `u64`.
fn post_to_proto(p: &bridge::BlueskyPost) -> proto::BlueskyPost {
    proto::BlueskyPost {
        id: p.id.clone(),
        at_uri: p.at_uri.clone(),
        cid: p.cid.clone(),
        author_did: p.author_did.clone(),
        author_handle: p.author_handle.clone(),
        author_display_name: p.author_display_name.clone(),
        author_avatar: p.author_avatar.clone(),
        text: p.text.clone(),
        facets: p
            .facets
            .iter()
            .map(|f| proto::BlueskyFacet {
                start: f.start as u64,
                end: f.end as u64,
                facet_type: facet_type_to_proto(&f.facet_type),
                extra: Default::default(),
            })
            .collect(),
        images: p
            .images
            .iter()
            .map(|i| proto::BlueskyImage {
                thumb: i.thumb.clone(),
                fullsize: i.fullsize.clone(),
                alt: i.alt.clone(),
                extra: Default::default(),
            })
            .collect(),
        video: p.video.as_ref().map(|v| proto::BlueskyVideo {
            thumb: v.thumb.clone(),
            playlist: v.playlist.clone(),
            alt: v.alt.clone(),
            extra: Default::default(),
        }),
        external: p.external.as_ref().map(|x| proto::BlueskyExternal {
            uri: x.uri.clone(),
            title: x.title.clone(),
            description: x.description.clone(),
            thumb: x.thumb.clone(),
            extra: Default::default(),
        }),
        quote: p.quote.as_ref().map(|q| Box::new(post_to_proto(q))),
        reply_parent: p.reply_parent.clone(),
        reply_root: p.reply_root.clone(),
        reposted_by: p.reposted_by.clone(),
        like_count: p.like_count,
        repost_count: p.repost_count,
        reply_count: p.reply_count,
        viewer_like: p.viewer_like.clone(),
        viewer_repost: p.viewer_repost.clone(),
        labels: p.labels.clone(),
        created_at: p.created_at.clone(),
        source: p.source.clone(),
        extra: Default::default(),
    }
}

/// Resolve the request's target to a Bluesky AT-URI.
///
/// `AtUri` is used as-is; `PostId` is validated to 32 bytes, hex-encoded, and
/// resolved through the `bluesky_posts` crosspost mapping (a missing mapping is
/// `not_found`).
async fn resolve_at_uri(
    state: &Arc<AppState>,
    req: proto::BlueskyThreadRequest,
) -> Result<String, RpcError> {
    match req {
        proto::BlueskyThreadRequest::AtUri { uri } => Ok(uri),
        proto::BlueskyThreadRequest::PostId { post_id } => {
            if post_id.len() != 32 {
                return Err(malformed("post_id must be 32 bytes"));
            }
            let post_hex = hex::encode(&post_id);
            let conn = state.db.conn().await;
            let lookup = db_helpers::get_crosspost_uri(&conn, &post_hex);
            drop(conn);
            match lookup.map_err(internal)? {
                Some(uri) => Ok(uri),
                None => Err(not_found("no Bluesky AT-URI mapping found for this post")),
            }
        }
    }
}

/// `bluesky.feed.thread` — fetch and return a post's full thread context as a
/// flat list of translated posts.
fn thread_handler() -> RpcHandler {
    Box::new(move |state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "bluesky.feed.thread").await?;
            let req: proto::BlueskyThreadRequest = decode(&payload).map_err(malformed)?;
            let at_uri = resolve_at_uri(&state, req).await?;

            let actor_hex = hex::encode(actor_id);
            let agent = db_helpers::get_agent_for_actor(&state, &actor_hex)
                .await
                .map_err(upstream)?;

            let params = get_post_thread::ParametersData {
                uri: at_uri,
                depth: None,
                parent_height: None,
            };
            let output = agent
                .api
                .app
                .bsky
                .feed
                .get_post_thread(params.into())
                .await
                .map_err(|e| {
                    tracing::warn!("getPostThread XRPC error for {actor_hex}: {e}");
                    upstream(format!("Bluesky API error: {e}"))
                })?;

            match &output.thread {
                Union::Refs(get_post_thread::OutputThreadRefs::AppBskyFeedDefsThreadViewPost(
                    tvp,
                )) => {
                    let (translated, focal_index) = translate_thread(tvp);
                    let posts = translated.iter().map(post_to_proto).collect();
                    encode_reply(&proto::BlueskyThreadReply {
                        posts,
                        focal_index: focal_index as u32,
                        extra: Default::default(),
                    })
                }
                Union::Refs(get_post_thread::OutputThreadRefs::AppBskyFeedDefsNotFoundPost(_)) => {
                    Err(not_found("post not found"))
                }
                Union::Refs(get_post_thread::OutputThreadRefs::AppBskyFeedDefsBlockedPost(_)) => {
                    Err(blocked())
                }
                Union::Unknown(_) => Err(upstream("unknown thread response type")),
            }
        })
    })
}

/// Register the Bluesky-native thread-view handler. Feature-gated — only wired
/// into the dispatcher under `--features bluesky` (see `lib.rs`).
pub fn register_bluesky_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "bluesky.feed.thread",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: thread_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge_post(id: &str) -> bridge::BlueskyPost {
        bridge::BlueskyPost {
            id: id.into(),
            at_uri: format!("at://did:plc:abc/app.bsky.feed.post/{id}"),
            cid: "bafy".into(),
            author_did: "did:plc:abc".into(),
            author_handle: "alice.bsky.social".into(),
            author_display_name: Some("Alice".into()),
            author_avatar: None,
            text: "hello #fauna".into(),
            facets: vec![bridge::BlueskyFacet {
                start: 6,
                end: 12,
                facet_type: bridge::BlueskyFacetType::Tag {
                    tag: "fauna".into(),
                },
            }],
            images: vec![bridge::BlueskyImage {
                thumb: "t".into(),
                fullsize: "f".into(),
                alt: "a".into(),
            }],
            video: None,
            external: None,
            quote: None,
            reply_parent: None,
            reply_root: None,
            reposted_by: None,
            like_count: 10,
            repost_count: 2,
            reply_count: 1,
            viewer_like: None,
            viewer_repost: None,
            labels: vec![],
            created_at: "2026-06-05T00:00:00Z".into(),
            source: "bluesky".into(),
        }
    }

    #[test]
    fn post_to_proto_maps_fields_and_recurses_quote() {
        let mut p = bridge_post("focal");
        p.quote = Some(Box::new(bridge_post("quoted")));

        let out = post_to_proto(&p);
        assert_eq!(out.id, "focal");
        assert_eq!(out.author_handle, "alice.bsky.social");
        assert_eq!(out.like_count, 10);
        assert_eq!(out.facets.len(), 1);
        assert_eq!(out.facets[0].start, 6);
        assert_eq!(out.facets[0].end, 12);
        assert_eq!(
            out.facets[0].facet_type,
            proto::BlueskyFacetType::Tag {
                tag: "fauna".into()
            }
        );
        assert_eq!(out.images.len(), 1);
        // The recursive quote box maps through.
        let quote = out.quote.expect("quote present");
        assert_eq!(quote.id, "quoted");
    }
}
