//! Content translation between Bluesky ATProto types and Fauna types.
//!
//! Converts atrium-api response types into the bridge's own `Bluesky*`
//! view types, rewriting media URLs to go through the privacy proxy.

use atrium_api::app::bsky::actor::defs::{ProfileView, ProfileViewBasic, ProfileViewDetailed};
use atrium_api::app::bsky::embed::external as embed_external;
use atrium_api::app::bsky::embed::images as embed_images;
use atrium_api::app::bsky::embed::record as embed_record;
use atrium_api::app::bsky::embed::record_with_media as embed_record_with_media;
use atrium_api::app::bsky::embed::video as embed_video;
use atrium_api::app::bsky::feed::defs::{
    FeedViewPost, FeedViewPostReasonRefs, GeneratorView, PostView, PostViewEmbedRefs,
    ThreadViewPost, ThreadViewPostParentRefs, ThreadViewPostRepliesItem,
};
use atrium_api::app::bsky::feed::post::Record as PostRecord;
use atrium_api::app::bsky::richtext::facet::MainFeaturesItem;
use atrium_api::types::{TryFromUnknown, Union};

use crate::types::{
    BlueskyActor, BlueskyConvo, BlueskyConvoMember, BlueskyDm, BlueskyExternal, BlueskyFacet,
    BlueskyFacetType, BlueskyFeedGenerator, BlueskyImage, BlueskyNotification, BlueskyPost,
    BlueskyVideo,
};

// ---------------------------------------------------------------------------
// Media URL rewriting
// ---------------------------------------------------------------------------

/// Rewrite a Bluesky CDN URL to go through the node's media proxy.
///
/// Input:  `https://cdn.bsky.app/img/.../jpeg`
/// Output: `/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg%2F...%2Fjpeg`
pub fn rewrite_media_url(cdn_url: &str) -> String {
    format!("/api/v1/bluesky/media?url={}", urlencoding::encode(cdn_url))
}

// ---------------------------------------------------------------------------
// Actor translation
// ---------------------------------------------------------------------------

/// Translate a `ProfileViewBasic` into a `BlueskyActor`.
///
/// Basic profiles lack follower/follow/post counts, so those default to 0.
pub fn translate_actor_basic(view: &ProfileViewBasic) -> BlueskyActor {
    let viewer = view.viewer.as_ref();
    BlueskyActor {
        did: view.did.to_string(),
        handle: view.handle.to_string(),
        display_name: view.display_name.clone(),
        description: None,
        avatar: view.avatar.as_ref().map(|u| rewrite_media_url(u)),
        followers_count: 0,
        follows_count: 0,
        posts_count: 0,
        viewer_following: viewer.and_then(|v| v.following.clone()),
        viewer_followed_by: viewer.and_then(|v| v.followed_by.clone()),
    }
}

/// Translate a `ProfileViewDetailed` into a `BlueskyActor`.
///
/// Detailed profiles have full count fields and viewer state.
pub fn translate_actor_detailed(view: &ProfileViewDetailed) -> BlueskyActor {
    let viewer = view.viewer.as_ref();
    BlueskyActor {
        did: view.did.to_string(),
        handle: view.handle.to_string(),
        display_name: view.display_name.clone(),
        description: view.description.clone(),
        avatar: view.avatar.as_ref().map(|u| rewrite_media_url(u)),
        followers_count: view.followers_count.unwrap_or(0) as u64,
        follows_count: view.follows_count.unwrap_or(0) as u64,
        posts_count: view.posts_count.unwrap_or(0) as u64,
        viewer_following: viewer.and_then(|v| v.following.clone()),
        viewer_followed_by: viewer.and_then(|v| v.followed_by.clone()),
    }
}

/// Translate a `ProfileView` into a `BlueskyActor`.
///
/// Profile views (e.g. from search results) lack count fields, so those default to 0.
pub fn translate_actor_from_profile_view(view: &ProfileView) -> BlueskyActor {
    let viewer = view.viewer.as_ref();
    BlueskyActor {
        did: view.did.to_string(),
        handle: view.handle.to_string(),
        display_name: view.display_name.clone(),
        description: view.description.clone(),
        avatar: view.avatar.as_ref().map(|u| rewrite_media_url(u)),
        followers_count: 0,
        follows_count: 0,
        posts_count: 0,
        viewer_following: viewer.and_then(|v| v.following.clone()),
        viewer_followed_by: viewer.and_then(|v| v.followed_by.clone()),
    }
}

// ---------------------------------------------------------------------------
// Embed extraction helpers
// ---------------------------------------------------------------------------

/// Extract images from an images embed view, rewriting URLs through the media proxy.
fn extract_images(view: &embed_images::View) -> Vec<BlueskyImage> {
    view.images
        .iter()
        .map(|img| BlueskyImage {
            thumb: rewrite_media_url(&img.thumb),
            fullsize: rewrite_media_url(&img.fullsize),
            alt: img.alt.clone(),
        })
        .collect()
}

/// Extract video from a video embed view, rewriting URLs through the media proxy.
fn extract_video(view: &embed_video::View) -> BlueskyVideo {
    BlueskyVideo {
        thumb: view.thumbnail.as_ref().map(|u| rewrite_media_url(u)),
        playlist: rewrite_media_url(&view.playlist),
        alt: view.alt.clone(),
    }
}

/// Extract external link from an external embed view, rewriting the thumb URL.
fn extract_external(view: &embed_external::View) -> BlueskyExternal {
    BlueskyExternal {
        uri: view.external.uri.clone(),
        title: view.external.title.clone(),
        description: view.external.description.clone(),
        thumb: view.external.thumb.as_ref().map(|u| rewrite_media_url(u)),
    }
}

/// Attempt to extract a quoted post from a record embed.
/// Returns `None` if the record is not a viewable post (blocked, not found, etc.).
fn extract_quote_from_record_view(view: &embed_record::View) -> Option<Box<BlueskyPost>> {
    match &view.record {
        Union::Refs(embed_record::ViewRecordRefs::ViewRecord(rec)) => {
            // Extract text from the record value
            let text = PostRecord::try_from_unknown(rec.value.clone())
                .map(|r| r.text.clone())
                .unwrap_or_default();
            let created_at = PostRecord::try_from_unknown(rec.value.clone())
                .map(|r| r.created_at.as_str().to_string())
                .unwrap_or_default();
            let facets = PostRecord::try_from_unknown(rec.value.clone())
                .map(|r| translate_facets(r.facets.as_deref()))
                .unwrap_or_default();

            // Extract embedded media from the quote's own embeds
            let mut images = Vec::new();
            let mut video = None;
            let mut external = None;
            if let Some(embeds) = &rec.embeds {
                for embed in embeds {
                    match embed {
                        Union::Refs(
                            embed_record::ViewRecordEmbedsItem::AppBskyEmbedImagesView(v),
                        ) => {
                            images = extract_images(v);
                        }
                        Union::Refs(embed_record::ViewRecordEmbedsItem::AppBskyEmbedVideoView(
                            v,
                        )) => {
                            video = Some(extract_video(v));
                        }
                        Union::Refs(
                            embed_record::ViewRecordEmbedsItem::AppBskyEmbedExternalView(v),
                        ) => {
                            external = Some(extract_external(v));
                        }
                        _ => {}
                    }
                }
            }

            let labels = rec
                .labels
                .as_ref()
                .map(|ls| ls.iter().map(|l| l.val.clone()).collect())
                .unwrap_or_default();

            Some(Box::new(BlueskyPost {
                id: rec.uri.clone(),
                at_uri: rec.uri.clone(),
                cid: rec.cid.as_ref().to_string(),
                author_did: rec.author.did.to_string(),
                author_handle: rec.author.handle.to_string(),
                author_display_name: rec.author.display_name.clone(),
                author_avatar: rec.author.avatar.as_ref().map(|u| rewrite_media_url(u)),
                text,
                facets,
                images,
                video,
                external,
                quote: None, // No recursive quote-of-quote
                reply_parent: None,
                reply_root: None,
                reposted_by: None,
                like_count: rec.like_count.unwrap_or(0) as u64,
                repost_count: rec.repost_count.unwrap_or(0) as u64,
                reply_count: rec.reply_count.unwrap_or(0) as u64,
                viewer_like: None,
                viewer_repost: None,
                labels,
                created_at,
                source: "bluesky".into(),
            }))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Facet translation
// ---------------------------------------------------------------------------

/// Translate atrium facets into our flat `BlueskyFacet` type.
fn translate_facets(
    facets: Option<&[atrium_api::app::bsky::richtext::facet::Main]>,
) -> Vec<BlueskyFacet> {
    let Some(facets) = facets else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for facet in facets {
        let start = facet.index.byte_start;
        let end = facet.index.byte_end;
        for feature in &facet.features {
            let facet_type = match feature {
                Union::Refs(MainFeaturesItem::Mention(m)) => BlueskyFacetType::Mention {
                    did: m.did.to_string(),
                },
                Union::Refs(MainFeaturesItem::Link(l)) => {
                    BlueskyFacetType::Link { uri: l.uri.clone() }
                }
                Union::Refs(MainFeaturesItem::Tag(t)) => {
                    BlueskyFacetType::Tag { tag: t.tag.clone() }
                }
                Union::Unknown(_) => continue,
            };
            result.push(BlueskyFacet {
                start,
                end,
                facet_type,
            });
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Post translation
// ---------------------------------------------------------------------------

/// Translate a `PostView` (individual post) into a `BlueskyPost`.
///
/// This is used for individual post lookups, quote embeds, and thread nodes.
/// It does not have repost reason or feed-level reply context.
pub fn translate_post_view(view: &PostView) -> BlueskyPost {
    // Extract the post record
    let record = PostRecord::try_from_unknown(view.record.clone()).ok();

    let text = record.as_ref().map(|r| r.text.clone()).unwrap_or_default();
    let created_at = record
        .as_ref()
        .map(|r| r.created_at.as_str().to_string())
        .unwrap_or_default();
    let facets = translate_facets(record.as_ref().and_then(|r| r.facets.as_deref()));

    // Reply references from the record
    let reply_parent = record
        .as_ref()
        .and_then(|r| r.reply.as_ref())
        .map(|rep| rep.parent.uri.clone());
    let reply_root = record
        .as_ref()
        .and_then(|r| r.reply.as_ref())
        .map(|rep| rep.root.uri.clone());

    // Extract embeds
    let mut images = Vec::new();
    let mut video = None;
    let mut external = None;
    let mut quote = None;

    if let Some(embed) = &view.embed {
        extract_embeds(embed, &mut images, &mut video, &mut external, &mut quote);
    }

    // Labels
    let labels = view
        .labels
        .as_ref()
        .map(|ls| ls.iter().map(|l| l.val.clone()).collect())
        .unwrap_or_default();

    // Viewer state
    let viewer_like = view.viewer.as_ref().and_then(|v| v.like.clone());
    let viewer_repost = view.viewer.as_ref().and_then(|v| v.repost.clone());

    BlueskyPost {
        id: view.uri.clone(),
        at_uri: view.uri.clone(),
        cid: view.cid.as_ref().to_string(),
        author_did: view.author.did.to_string(),
        author_handle: view.author.handle.to_string(),
        author_display_name: view.author.display_name.clone(),
        author_avatar: view.author.avatar.as_ref().map(|u| rewrite_media_url(u)),
        text,
        facets,
        images,
        video,
        external,
        quote,
        reply_parent,
        reply_root,
        reposted_by: None,
        like_count: view.like_count.unwrap_or(0) as u64,
        repost_count: view.repost_count.unwrap_or(0) as u64,
        reply_count: view.reply_count.unwrap_or(0) as u64,
        viewer_like,
        viewer_repost,
        labels,
        created_at,
        source: "bluesky".into(),
    }
}

/// Extract all embed types from a post view embed union.
fn extract_embeds(
    embed: &Union<PostViewEmbedRefs>,
    images: &mut Vec<BlueskyImage>,
    video: &mut Option<BlueskyVideo>,
    external: &mut Option<BlueskyExternal>,
    quote: &mut Option<Box<BlueskyPost>>,
) {
    match embed {
        Union::Refs(PostViewEmbedRefs::AppBskyEmbedImagesView(v)) => {
            *images = extract_images(v);
        }
        Union::Refs(PostViewEmbedRefs::AppBskyEmbedVideoView(v)) => {
            *video = Some(extract_video(v));
        }
        Union::Refs(PostViewEmbedRefs::AppBskyEmbedExternalView(v)) => {
            *external = Some(extract_external(v));
        }
        Union::Refs(PostViewEmbedRefs::AppBskyEmbedRecordView(v)) => {
            *quote = extract_quote_from_record_view(v);
        }
        Union::Refs(PostViewEmbedRefs::AppBskyEmbedRecordWithMediaView(v)) => {
            // Record-with-media: extract both the media and the quote
            match &v.media {
                Union::Refs(embed_record_with_media::ViewMediaRefs::AppBskyEmbedImagesView(
                    img,
                )) => {
                    *images = extract_images(img);
                }
                Union::Refs(embed_record_with_media::ViewMediaRefs::AppBskyEmbedVideoView(vid)) => {
                    *video = Some(extract_video(vid));
                }
                Union::Refs(embed_record_with_media::ViewMediaRefs::AppBskyEmbedExternalView(
                    ext,
                )) => {
                    *external = Some(extract_external(ext));
                }
                Union::Unknown(_) => {}
            }
            *quote = extract_quote_from_record_view(&v.record);
        }
        Union::Unknown(_) => {}
    }
}

/// Translate a `FeedViewPost` (feed timeline item) into a `BlueskyPost`.
///
/// This is the primary translation function used for feed/timeline responses.
/// It handles reposts (setting `reposted_by`) and uses the full post view.
pub fn translate_post(view: &FeedViewPost) -> BlueskyPost {
    let mut post = translate_post_view(&view.post);

    // Check for repost reason
    if let Some(reason) = &view.reason
        && let Union::Refs(FeedViewPostReasonRefs::ReasonRepost(repost)) = reason
    {
        post.reposted_by = Some(repost.by.handle.to_string());
    }

    post
}

// ---------------------------------------------------------------------------
// Thread translation
// ---------------------------------------------------------------------------

/// Translate a `ThreadViewPost` into a flat `Vec<BlueskyPost>` plus the index
/// of the focal post (the post the thread was requested for) in that list.
///
/// Returns posts ordered: ancestors (oldest first), the focal post, then
/// replies; the index is the ancestor count.
pub fn translate_thread(view: &ThreadViewPost) -> (Vec<BlueskyPost>, usize) {
    let mut ancestors = Vec::new();
    collect_ancestors(&view.parent, &mut ancestors);
    ancestors.reverse(); // oldest first

    // The focal post sits right after its ancestors.
    let focal_index = ancestors.len();
    ancestors.push(translate_post_view(&view.post));

    // Replies (only direct ThreadViewPost replies, skip blocked/not-found)
    if let Some(replies) = &view.replies {
        for reply in replies {
            if let Union::Refs(ThreadViewPostRepliesItem::ThreadViewPost(tvp)) = reply {
                ancestors.push(translate_post_view(&tvp.post));
            }
        }
    }

    (ancestors, focal_index)
}

/// Walk the parent chain collecting ancestor posts.
fn collect_ancestors(
    parent: &Option<Union<ThreadViewPostParentRefs>>,
    ancestors: &mut Vec<BlueskyPost>,
) {
    let Some(parent) = parent else { return };
    if let Union::Refs(ThreadViewPostParentRefs::ThreadViewPost(tvp)) = parent {
        ancestors.push(translate_post_view(&tvp.post));
        collect_ancestors(&tvp.parent, ancestors);
    }
}

// ---------------------------------------------------------------------------
// Feed generator translation
// ---------------------------------------------------------------------------

/// Translate a `GeneratorView` into a `BlueskyFeedGenerator`.
pub fn translate_feed_generator(view: &GeneratorView) -> BlueskyFeedGenerator {
    BlueskyFeedGenerator {
        uri: view.uri.clone(),
        did: view.creator.did.to_string(),
        display_name: view.display_name.clone(),
        description: view.description.clone(),
        avatar: view.avatar.as_ref().map(|u| rewrite_media_url(u)),
        like_count: view.like_count.unwrap_or(0) as u64,
    }
}

// ---------------------------------------------------------------------------
// Notification translation
// ---------------------------------------------------------------------------

use atrium_api::app::bsky::notification::list_notifications::Notification;

/// Translate a `Notification` into a `BlueskyNotification`.
///
/// Extracts author info using the `ProfileView` pattern, maps `reason_subject`
/// to `subject_uri`, and attempts to extract text from the record for
/// reply/mention/quote notifications.
pub fn translate_notification(notif: &Notification) -> BlueskyNotification {
    // Extract text from the record for reply, mention, and quote notifications.
    let record_text = match notif.reason.as_str() {
        "reply" | "mention" | "quote" => PostRecord::try_from_unknown(notif.record.clone())
            .ok()
            .map(|r| r.text.clone()),
        _ => None,
    };

    BlueskyNotification {
        uri: notif.uri.to_string(),
        reason: notif.reason.clone(),
        author_did: notif.author.did.to_string(),
        author_handle: notif.author.handle.to_string(),
        author_display_name: notif.author.display_name.clone(),
        author_avatar: notif.author.avatar.as_ref().map(|u| rewrite_media_url(u)),
        subject_uri: notif.reason_subject.clone(),
        record_text,
        indexed_at: notif.indexed_at.as_str().to_string(),
        is_read: notif.is_read,
    }
}

// ---------------------------------------------------------------------------
// DM / Convo translation
// ---------------------------------------------------------------------------

use atrium_api::chat::bsky::convo::defs::{ConvoView, ConvoViewLastMessageRefs, MessageView};

/// Translate a chat `MessageView` into a `BlueskyDm`.
///
/// The `convo_id` is passed in because message views do not embed the
/// conversation ID. The `members` slice comes from the parent `ConvoView`
/// and is used to resolve sender handle / display_name (the wire
/// `MessageViewSender` only contains a DID).
pub fn translate_dm(
    view: &MessageView,
    convo_id: &str,
    members: &[atrium_api::chat::bsky::actor::defs::ProfileViewBasic],
) -> BlueskyDm {
    let sender_did = view.sender.did.to_string();

    // Resolve handle + display_name from the convo member list.
    let (sender_handle, sender_display_name) = members
        .iter()
        .find(|m| m.did.as_str() == sender_did)
        .map(|m| (m.handle.to_string(), m.display_name.clone()))
        .unwrap_or_else(|| (sender_did.clone(), None));

    BlueskyDm {
        id: view.id.clone(),
        convo_id: convo_id.to_string(),
        sender_did,
        sender_handle,
        sender_display_name,
        text: view.text.clone(),
        sent_at: view.sent_at.as_str().to_string(),
    }
}

/// Translate a `ConvoView` into a `BlueskyConvo`.
pub fn translate_convo(view: &ConvoView) -> BlueskyConvo {
    let members: Vec<BlueskyConvoMember> = view
        .members
        .iter()
        .map(|m| BlueskyConvoMember {
            did: m.did.to_string(),
            handle: m.handle.to_string(),
            display_name: m.display_name.clone(),
            avatar: m.avatar.as_ref().map(|u| rewrite_media_url(u)),
        })
        .collect();

    let last_message = view.last_message.as_ref().and_then(|lm| match lm {
        Union::Refs(ConvoViewLastMessageRefs::MessageView(msg)) => {
            Some(translate_dm(msg, &view.id, &view.members))
        }
        // Deleted messages are omitted from the last_message field.
        _ => None,
    });

    BlueskyConvo {
        id: view.id.clone(),
        members,
        last_message,
        unread_count: view.unread_count as u64,
        muted: view.muted,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_cdn_url() {
        let input = "https://cdn.bsky.app/img/feed_thumbnail/plain/did:plc:xxx/bafkrei/jpeg";
        let output = rewrite_media_url(input);
        assert!(output.starts_with("/api/v1/bluesky/media?url="));
        assert!(output.contains("cdn.bsky.app"));
        // Round-trip: decoding the URL param should give back the original
        let decoded =
            urlencoding::decode(output.strip_prefix("/api/v1/bluesky/media?url=").unwrap())
                .unwrap();
        assert_eq!(decoded, input);
    }
}
