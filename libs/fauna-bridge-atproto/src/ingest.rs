//! Consume-side feed ingestion: an AppView **view** of a post → the Fauna post
//! shape the nest stores (`bridges.md` § Unified feed ingestion → *Bridge
//! ingestion*).
//!
//! The fourth direction in this crate's table, and deliberately built out of
//! the other three rather than beside them:
//!
//! | Module | Direction | Purpose |
//! |---|---|---|
//! | [`crate::outbound`] | Fauna `Post` → ATProto **record** | the one-way projection |
//! | [`crate::reverse_translate`] | ATProto **record** → [`IntermediatePost`] | external writes coming back in |
//! | [`crate::translate`] | ATProto **view** → `Bluesky*` display types | the live read path (thread view) |
//! | **this module** | ATProto **view** → [`IntermediatePost`] + view facts → Fauna `Post` | feed ingestion |
//!
//! A `PostView` carries the record verbatim (`record`), so the record half of
//! the translation IS [`crate::reverse_translate::intermediate_from_record`] —
//! text, facets, reply refs, quote ref, `createdAt`, with the same mention-drop
//! and the same parsed-not-trusted timestamp. What the view adds, and the
//! record cannot supply, is the author's DID and handle, the record's CID, the
//! CDN URLs of its images and its labels; those are lifted here.
//!
//! # DB-free, like every translator in the fleet
//!
//! The nest resolves every reference through its `bluesky_posts` map and
//! decides what to store; this module only says what a view *is*. The one
//! ordering rule it owns is [`ingestables_from_feed_item`]'s: the posts a feed
//! item carries *inside* it — the reply parent view, the quoted record — come
//! out BEFORE the item's own post, so a nest that stores them in order finds
//! the parent already resting when the reply asks for it (ruling 3).
//!
//! # The synthetic author
//!
//! [`synthetic_actor_id`] is the ActivityPub construction
//! (`fauna_bridge_activitypub::identity::synthetic_actor_id`) with this bridge's
//! own domain constant, keyed on the DID and never the handle (ruling 1).

use atrium_api::app::bsky::embed::images as embed_images;
use atrium_api::app::bsky::embed::record as embed_record;
use atrium_api::app::bsky::embed::record_with_media as embed_record_with_media;
use atrium_api::app::bsky::feed::defs::{
    FeedViewPost, PostView, PostViewEmbedRefs, ReplyRefParentRefs,
};
use atrium_api::app::bsky::feed::post::Record as PostRecord;
use atrium_api::types::{TryFromUnknown, Union};
use fauna_core::data::{ContentHash, Dimensions, MediaItem, Post, PostBody, Reference, Timestamp};
use fauna_core::identity::ActorId;

use crate::reverse_translate::{IntermediatePost, intermediate_from_record};
use crate::translate::rewrite_media_url;

/// The domain-separation constant of this bridge's synthetic authors. Its
/// siblings: `fauna-activitypub-synthetic` (AP), `fauna-nostr-synthetic`
/// (nostr). Changing it re-keys every ingested post's author at rest.
const SYNTHETIC_DOMAIN: &str = "fauna-bluesky-synthetic";

/// Derive the deterministic Fauna [`ActorId`] a Bluesky account DID ingests under.
///
/// `blake3::derive_key` over the DID string with [`SYNTHETIC_DOMAIN`] — the
/// same construction the ActivityPub bridge applies to an actor URI. The DID is
/// the identity; a handle is reassignable and would split one account's posts
/// across two authors the day it changed.
pub fn synthetic_actor_id(did: &str) -> ActorId {
    ActorId(blake3::derive_key(SYNTHETIC_DOMAIN, did.as_bytes()))
}

/// One image of a post's embed, as the view served it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IngestImage {
    /// The full-size CDN URL, already rewritten through the bridge's privacy
    /// proxy ([`rewrite_media_url`]) — what `MediaItem::remote_url` carries.
    pub url: String,
    /// The per-image alt text (empty when none).
    pub alt: String,
    /// The declared `aspectRatio`, when present and within `u32`.
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// A post as the nest ingests it: the record half plus the facts only the
/// view knows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IngestablePost {
    /// `at://did/app.bsky.feed.post/rkey` — the map key and the dedupe key.
    pub at_uri: String,
    /// The record's CID as the view served it (ruling 2 — stored, never
    /// re-fetched).
    pub cid: String,
    /// The author's DID — what [`synthetic_actor_id`] keys on.
    pub author_did: String,
    /// The author's handle at view time — display and Search-corpus data only,
    /// never identity.
    pub author_handle: String,
    /// The author's `displayName` at view time, when the profile has one —
    /// the bridged-author face (`bridges.md` § Unified feed ingestion →
    /// *Bridged authors*), display data only.
    pub author_display_name: Option<String>,
    /// The author's avatar at view time, **already rewritten through the
    /// privacy proxy** ([`crate::translate::rewrite_media_url`]) — the same
    /// lane the post's images ride; never the CDN origin.
    pub author_avatar: Option<String>,
    /// The record: text, facets, reply/quote refs, `createdAt`.
    pub record: IntermediatePost,
    /// The images of the view's embed, in order.
    pub images: Vec<IngestImage>,
    /// The labels the view carried, in order (`val` only).
    pub labels: Vec<String>,
}

impl IngestablePost {
    /// The AT-URI of the post this one replies to, if any.
    pub fn reply_parent_uri(&self) -> Option<&str> {
        self.record.reply.as_ref().map(|r| r.parent_uri.as_str())
    }

    /// The AT-URI of the post this one quotes, if any.
    pub fn quote_uri(&self) -> Option<&str> {
        self.record.quote.as_ref().map(|q| q.uri.as_str())
    }
}

/// Lift a `PostView` into an [`IngestablePost`]. `None` when the view's
/// `record` is not a decodable `app.bsky.feed.post` (an AppView never serves
/// one for this collection, but the type admits it).
pub fn ingestable_from_post_view(view: &PostView) -> Option<IngestablePost> {
    let record = PostRecord::try_from_unknown(view.record.clone()).ok()?;
    let record = intermediate_from_record(&record).ok()?;

    let mut images = Vec::new();
    if let Some(embed) = &view.embed {
        match embed {
            Union::Refs(PostViewEmbedRefs::AppBskyEmbedImagesView(v)) => {
                images = lift_images(v);
            }
            Union::Refs(PostViewEmbedRefs::AppBskyEmbedRecordWithMediaView(v)) => {
                if let Union::Refs(
                    embed_record_with_media::ViewMediaRefs::AppBskyEmbedImagesView(img),
                ) = &v.media
                {
                    images = lift_images(img);
                }
            }
            _ => {}
        }
    }

    Some(IngestablePost {
        at_uri: view.uri.clone(),
        cid: view.cid.as_ref().to_string(),
        author_did: view.author.did.to_string(),
        author_handle: view.author.handle.to_string(),
        author_display_name: view.author.display_name.clone(),
        author_avatar: view.author.avatar.as_deref().map(rewrite_media_url),
        record,
        images,
        labels: labels_of(view.labels.as_deref()),
    })
}

/// Lift the `ViewRecord` a record embed carries (the quoted post, as the
/// AppView served it alongside the quoting one).
pub fn ingestable_from_view_record(rec: &embed_record::ViewRecord) -> Option<IngestablePost> {
    let record = PostRecord::try_from_unknown(rec.value.clone()).ok()?;
    let record = intermediate_from_record(&record).ok()?;

    let mut images = Vec::new();
    if let Some(embeds) = &rec.embeds {
        for embed in embeds {
            match embed {
                Union::Refs(embed_record::ViewRecordEmbedsItem::AppBskyEmbedImagesView(v)) => {
                    images = lift_images(v);
                }
                Union::Refs(
                    embed_record::ViewRecordEmbedsItem::AppBskyEmbedRecordWithMediaView(v),
                ) => {
                    if let Union::Refs(
                        embed_record_with_media::ViewMediaRefs::AppBskyEmbedImagesView(img),
                    ) = &v.media
                    {
                        images = lift_images(img);
                    }
                }
                _ => {}
            }
        }
    }

    Some(IngestablePost {
        at_uri: rec.uri.clone(),
        cid: rec.cid.as_ref().to_string(),
        author_did: rec.author.did.to_string(),
        author_handle: rec.author.handle.to_string(),
        author_display_name: rec.author.display_name.clone(),
        author_avatar: rec.author.avatar.as_deref().map(rewrite_media_url),
        record,
        images,
        labels: labels_of(rec.labels.as_deref()),
    })
}

/// Every post one feed item carries, **referenced posts first**: the reply
/// parent (when the AppView served it as a viewable post), then the quoted
/// record (when the embed is a viewable record), then the item's own post.
/// A repost item yields the reposted post itself, under its author — the
/// repost relation is not carried (ruling 3).
///
/// The order is the contract: the nest stores in sequence and resolves each
/// post's references against what already rests, so a parent listed after
/// its reply would leave that reply top-level for good.
pub fn ingestables_from_feed_item(item: &FeedViewPost) -> Vec<IngestablePost> {
    let mut out = Vec::with_capacity(3);

    if let Some(reply) = &item.reply
        && let Union::Refs(ReplyRefParentRefs::PostView(parent)) = &reply.parent
        && let Some(parent) = ingestable_from_post_view(parent)
    {
        out.push(parent);
    }

    if let Some(quoted) = quoted_view_record(&item.post)
        && let Some(quoted) = ingestable_from_view_record(quoted)
    {
        out.push(quoted);
    }

    if let Some(post) = ingestable_from_post_view(&item.post) {
        out.push(post);
    }

    out
}

/// Build the Fauna [`Post`] an [`IngestablePost`] stores as, with the
/// references the nest resolved (ruling 3: only targets that already rest —
/// an unresolved reference is simply absent, never a synthetic id).
///
/// The author is [`synthetic_actor_id`] of the DID; `created_at` is the
/// record's own `createdAt` (the nest bounds it going forward before the
/// write); media rides as `remote_url` items behind the privacy proxy
/// (ruling 4), each carrying its image's own alt text; the first label becomes
/// the content warning.
pub fn build_fauna_post(ing: &IngestablePost, references: Vec<Reference>) -> Post {
    let items: Vec<MediaItem> = ing
        .images
        .iter()
        .map(|img| MediaItem {
            blob_hash: ContentHash::from_digest_raw([0u8; 32]),
            media_type: media_type_of(&img.url),
            size_bytes: 0,
            dimensions: match (img.width, img.height) {
                (Some(width), Some(height)) => Some(Dimensions { width, height }),
                _ => None,
            },
            thumbnail: None,
            remote_url: Some(img.url.clone()),
            alt: fauna_core::data::media_alt(Some(&img.alt)),
        })
        .collect();

    let body = if items.is_empty() {
        PostBody::Text {
            content: ing.record.text.clone(),
            facets: ing.record.facets.clone(),
        }
    } else {
        PostBody::TextWithMedia {
            content: ing.record.text.clone(),
            facets: ing.record.facets.clone(),
            items,
        }
    };

    Post {
        author: synthetic_actor_id(&ing.author_did),
        created_at: Timestamp(ing.record.created_at_micros),
        body,
        references,
        expires_at: None,
        gated: None,
        content_warning: ing.labels.first().cloned(),
        origin: None,
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn quoted_view_record(post: &PostView) -> Option<&embed_record::ViewRecord> {
    let record_view = match post.embed.as_ref()? {
        Union::Refs(PostViewEmbedRefs::AppBskyEmbedRecordView(v)) => v,
        Union::Refs(PostViewEmbedRefs::AppBskyEmbedRecordWithMediaView(v)) => &v.record,
        _ => return None,
    };
    match &record_view.record {
        Union::Refs(embed_record::ViewRecordRefs::ViewRecord(rec)) => Some(rec),
        _ => None,
    }
}

fn lift_images(view: &embed_images::View) -> Vec<IngestImage> {
    view.images
        .iter()
        .map(|img| {
            let (width, height) = img
                .aspect_ratio
                .as_ref()
                .and_then(|ar| {
                    Some((
                        u32::try_from(u64::from(ar.width)).ok()?,
                        u32::try_from(u64::from(ar.height)).ok()?,
                    ))
                })
                .map_or((None, None), |(w, h)| (Some(w), Some(h)));
            IngestImage {
                url: rewrite_media_url(&img.fullsize),
                alt: img.alt.clone(),
                width,
                height,
            }
        })
        .collect()
}

fn labels_of(labels: Option<&[atrium_api::com::atproto::label::defs::Label]>) -> Vec<String> {
    labels
        .map(|ls| ls.iter().map(|l| l.val.clone()).collect())
        .unwrap_or_default()
}

/// The MIME type a bsky CDN image URL names by its `@<format>` suffix
/// (`…/<cid>@jpeg`); `image/jpeg` when the suffix is absent or unfamiliar. The
/// URL here is already proxy-rewritten, so the suffix is URL-encoded and read
/// through the encoding.
fn media_type_of(url: &str) -> String {
    let format = url
        .rsplit_once("%40")
        .or_else(|| url.rsplit_once('@'))
        .map(|(_, f)| f)
        .unwrap_or("");
    match format {
        "jpeg" | "jpg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/jpeg",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::FacetFeature;
    use serde_json::json;

    fn post_view_json(
        uri: &str,
        did: &str,
        text: &str,
        extra: serde_json::Value,
    ) -> serde_json::Value {
        let mut record = json!({
            "$type": "app.bsky.feed.post",
            "text": text,
            "createdAt": "2026-09-26T10:00:00.000Z",
        });
        if let Some(obj) = extra.get("record").and_then(|r| r.as_object()) {
            for (k, v) in obj {
                record[k] = v.clone();
            }
        }
        let mut view = json!({
            "uri": uri,
            "cid": crate::test_support::test_cid(1),
            "author": { "did": did, "handle": "alice.test" },
            "record": record,
            "indexedAt": "2026-09-26T10:00:01.000Z",
        });
        if let Some(obj) = extra.get("author").and_then(|a| a.as_object()) {
            for (k, v) in obj {
                view["author"][k] = v.clone();
            }
        }
        if let Some(embed) = extra.get("embed") {
            view["embed"] = embed.clone();
        }
        if let Some(labels) = extra.get("labels") {
            view["labels"] = labels.clone();
        }
        view
    }

    /// A `PostView` inside a union slot (a `ReplyRef`'s parent/root) needs
    /// its `$type` discriminator; a top-level one does not.
    fn tagged_post_view(mut view: serde_json::Value) -> serde_json::Value {
        view["$type"] = json!("app.bsky.feed.defs#postView");
        view
    }

    fn post_view(uri: &str, did: &str, text: &str, extra: serde_json::Value) -> PostView {
        serde_json::from_value(post_view_json(uri, did, text, extra)).expect("a PostView")
    }

    #[test]
    fn synthetic_actor_id_is_deterministic_and_keyed_on_the_did() {
        let a = synthetic_actor_id("did:plc:alice");
        assert_eq!(a, synthetic_actor_id("did:plc:alice"));
        assert_ne!(a, synthetic_actor_id("did:plc:bob"));
        // The AP construction under a different domain never collides with
        // this one for the same bytes.
        assert_ne!(
            a.0,
            blake3::derive_key("fauna-activitypub-synthetic", b"did:plc:alice")
        );
    }

    /// The bridged-author face rides the view's `author` (`bridges.md`
    /// § Unified feed ingestion → *Bridged authors*): the display name verbatim,
    /// the avatar behind the same privacy proxy the images ride.
    #[test]
    fn a_post_view_lifts_the_authors_display_name_and_proxied_avatar() {
        let view = post_view(
            "at://did:plc:alice/app.bsky.feed.post/1",
            "did:plc:alice",
            "hi",
            json!({
                "author": {
                    "displayName": "Alice A.",
                    "avatar": "https://cdn.bsky.app/img/avatar/plain/did:plc:alice/bafy@jpeg"
                }
            }),
        );
        let ing = ingestable_from_post_view(&view).expect("ingestable");
        assert_eq!(ing.author_display_name.as_deref(), Some("Alice A."));
        let avatar = ing.author_avatar.expect("the avatar is lifted");
        assert!(
            avatar.starts_with("/api/v1/bluesky/media?url=") && avatar.contains("avatar"),
            "the avatar rides behind the privacy proxy, never the CDN origin: {avatar}"
        );
    }

    #[test]
    fn a_post_view_lifts_the_record_and_the_view_facts() {
        let view = post_view(
            "at://did:plc:alice/app.bsky.feed.post/1",
            "did:plc:alice",
            "hello #fauna",
            json!({
                "record": {
                    "facets": [{
                        "index": {"byteStart": 6, "byteEnd": 12},
                        "features": [{"$type": "app.bsky.richtext.facet#tag", "tag": "fauna"}]
                    }]
                },
                "embed": {
                    "$type": "app.bsky.embed.images#view",
                    "images": [{
                        "thumb": "https://cdn.bsky.app/img/feed_thumbnail/plain/did:plc:alice/bafy@jpeg",
                        "fullsize": "https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:alice/bafy@jpeg",
                        "alt": "a cat",
                        "aspectRatio": {"width": 800, "height": 600}
                    }]
                },
                "labels": [{"src": "did:plc:alice", "uri": "at://x", "val": "nudity", "cts": "2026-09-26T10:00:00.000Z"}]
            }),
        );
        let ing = ingestable_from_post_view(&view).expect("ingestable");
        assert_eq!(ing.at_uri, "at://did:plc:alice/app.bsky.feed.post/1");
        assert_eq!(ing.author_did, "did:plc:alice");
        assert_eq!(ing.author_handle, "alice.test");
        assert!(
            ing.author_display_name.is_none() && ing.author_avatar.is_none(),
            "a basic profile view with neither field lifts neither"
        );
        assert_eq!(ing.record.text, "hello #fauna");
        assert_eq!(ing.record.facets.len(), 1);
        assert_eq!(ing.images.len(), 1);
        assert!(
            ing.images[0].url.starts_with("/api/v1/bluesky/media?url="),
            "the CDN URL rides behind the privacy proxy: {}",
            ing.images[0].url
        );
        assert_eq!(
            (ing.images[0].width, ing.images[0].height),
            (Some(800), Some(600))
        );
        assert_eq!(ing.labels, vec!["nudity"]);

        let post = build_fauna_post(&ing, vec![]);
        assert_eq!(post.author, synthetic_actor_id("did:plc:alice"));
        assert_eq!(post.created_at, Timestamp(1_790_416_800_000_000));
        assert_eq!(post.content_warning.as_deref(), Some("nudity"));
        match &post.body {
            PostBody::TextWithMedia {
                content,
                facets,
                items,
            } => {
                assert_eq!(content, "hello #fauna");
                assert!(
                    matches!(&facets[0].feature, FacetFeature::Tag { name } if name == "fauna")
                );
                assert_eq!(items[0].media_type, "image/jpeg");
                assert_eq!(
                    items[0].remote_url.as_deref(),
                    Some(ing.images[0].url.as_str())
                );
                assert_eq!(
                    items[0].dimensions,
                    Some(Dimensions {
                        width: 800,
                        height: 600
                    })
                );
            }
            other => panic!("expected TextWithMedia, got {other:?}"),
        }
    }

    /// The embed's per-image `alt` rests as the item's `alt`; the empty string
    /// the lexicon sends for "none" rests as `None`.
    #[test]
    fn an_embed_images_alt_rests_as_the_items_alt() {
        let image = |name: &str, alt: &str| {
            json!({
                "thumb": format!("https://cdn.bsky.app/img/feed_thumbnail/plain/did:plc:alice/{name}@jpeg"),
                "fullsize": format!("https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:alice/{name}@jpeg"),
                "alt": alt,
            })
        };
        let view = post_view(
            "at://did:plc:alice/app.bsky.feed.post/2",
            "did:plc:alice",
            "two pictures",
            json!({
                "embed": {
                    "$type": "app.bsky.embed.images#view",
                    "images": [image("bafya", "a cat"), image("bafyb", "")]
                }
            }),
        );
        let ing = ingestable_from_post_view(&view).expect("ingestable");
        let post = build_fauna_post(&ing, vec![]);
        let alts: Vec<Option<&str>> = post
            .body
            .media_items()
            .iter()
            .map(|item| item.alt.as_deref())
            .collect();
        assert_eq!(alts, vec![Some("a cat"), None]);
    }

    #[test]
    fn a_text_only_post_builds_a_text_body_and_no_warning() {
        let view = post_view(
            "at://did:plc:alice/app.bsky.feed.post/2",
            "did:plc:alice",
            "plain",
            json!({}),
        );
        let ing = ingestable_from_post_view(&view).unwrap();
        let post = build_fauna_post(&ing, vec![]);
        assert!(matches!(post.body, PostBody::Text { .. }));
        assert!(post.content_warning.is_none());
        assert!(post.references.is_empty());
    }

    /// The ordering contract: parent view, quoted record, then the post — so
    /// a nest storing in sequence resolves the reply and the quote in one
    /// pass.
    #[test]
    fn a_feed_item_yields_its_parent_and_its_quoted_record_before_the_post() {
        let quoted_record = json!({
            "$type": "app.bsky.feed.post",
            "text": "the quoted one",
            "createdAt": "2026-09-26T09:00:00.000Z",
        });
        let post = post_view_json(
            "at://did:plc:carol/app.bsky.feed.post/reply",
            "did:plc:carol",
            "a reply that also quotes",
            json!({
                "record": {
                    "reply": {
                        "parent": {"uri": "at://did:plc:alice/app.bsky.feed.post/parent", "cid": crate::test_support::test_cid(2)},
                        "root": {"uri": "at://did:plc:alice/app.bsky.feed.post/parent", "cid": crate::test_support::test_cid(2)}
                    },
                    "embed": {
                        "$type": "app.bsky.embed.record",
                        "record": {"uri": "at://did:plc:bob/app.bsky.feed.post/quoted", "cid": crate::test_support::test_cid(3)}
                    }
                },
                "embed": {
                    "$type": "app.bsky.embed.record#view",
                    "record": {
                        "$type": "app.bsky.embed.record#viewRecord",
                        "uri": "at://did:plc:bob/app.bsky.feed.post/quoted",
                        "cid": crate::test_support::test_cid(3),
                        "author": {"did": "did:plc:bob", "handle": "bob.test"},
                        "value": quoted_record,
                        "indexedAt": "2026-09-26T09:00:01.000Z"
                    }
                }
            }),
        );
        let item: FeedViewPost = serde_json::from_value(json!({
            "post": post,
            "reply": {
                "root": tagged_post_view(post_view_json("at://did:plc:alice/app.bsky.feed.post/parent", "did:plc:alice", "the parent", json!({}))),
                "parent": tagged_post_view(post_view_json("at://did:plc:alice/app.bsky.feed.post/parent", "did:plc:alice", "the parent", json!({}))),
            }
        }))
        .expect("a FeedViewPost");

        let out = ingestables_from_feed_item(&item);
        let uris: Vec<&str> = out.iter().map(|p| p.at_uri.as_str()).collect();
        assert_eq!(
            uris,
            vec![
                "at://did:plc:alice/app.bsky.feed.post/parent",
                "at://did:plc:bob/app.bsky.feed.post/quoted",
                "at://did:plc:carol/app.bsky.feed.post/reply",
            ]
        );
        let reply = &out[2];
        assert_eq!(
            reply.reply_parent_uri(),
            Some("at://did:plc:alice/app.bsky.feed.post/parent")
        );
        assert_eq!(
            reply.quote_uri(),
            Some("at://did:plc:bob/app.bsky.feed.post/quoted")
        );
        assert_eq!(out[1].record.text, "the quoted one");
        assert_eq!(out[1].author_did, "did:plc:bob");
    }

    /// A parent the AppView could not serve (deleted, blocked) is no
    /// ingestable — the reply still comes out, top-level for the nest to
    /// decide.
    #[test]
    fn an_unviewable_parent_yields_nothing_and_the_reply_still_comes_out() {
        let item: FeedViewPost = serde_json::from_value(json!({
            "post": post_view_json("at://did:plc:carol/app.bsky.feed.post/r", "did:plc:carol", "orphan", json!({
                "record": {"reply": {
                    "parent": {"uri": "at://did:plc:alice/app.bsky.feed.post/gone", "cid": crate::test_support::test_cid(4)},
                    "root": {"uri": "at://did:plc:alice/app.bsky.feed.post/gone", "cid": crate::test_support::test_cid(4)}
                }}
            })),
            "reply": {
                "root": {"$type": "app.bsky.feed.defs#notFoundPost", "uri": "at://did:plc:alice/app.bsky.feed.post/gone", "notFound": true},
                "parent": {"$type": "app.bsky.feed.defs#notFoundPost", "uri": "at://did:plc:alice/app.bsky.feed.post/gone", "notFound": true}
            }
        }))
        .unwrap();
        let out = ingestables_from_feed_item(&item);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].at_uri, "at://did:plc:carol/app.bsky.feed.post/r");
    }

    #[test]
    fn media_type_follows_the_cdn_suffix_through_the_proxy_encoding() {
        assert_eq!(
            media_type_of("/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg%2Fx%40png"),
            "image/png"
        );
        assert_eq!(
            media_type_of("https://cdn.bsky.app/img/x@webp"),
            "image/webp"
        );
        assert_eq!(media_type_of("https://cdn.bsky.app/img/x"), "image/jpeg");
    }
}
