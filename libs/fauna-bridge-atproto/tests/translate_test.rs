//! Tests for content translation from atrium-api types to Fauna bridge types.

use atrium_api::app::bsky::actor::defs::{ProfileView, ProfileViewBasic, ProfileViewDetailed};
use atrium_api::app::bsky::feed::defs::{FeedViewPost, GeneratorView, PostView, ThreadViewPost};
use fauna_bridge_atproto::translate::*;

/// Helper: build a minimal PostView JSON with the given overrides.
fn post_view_json(overrides: serde_json::Value) -> serde_json::Value {
    let mut base = serde_json::json!({
        "uri": "at://did:plc:testuser/app.bsky.feed.post/abc123",
        "cid": "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy",
        "author": {
            "did": "did:plc:testuser",
            "handle": "alice.bsky.social",
            "displayName": "Alice",
            "avatar": "https://cdn.bsky.app/img/avatar/plain/did:plc:testuser/bafkrei/jpeg"
        },
        "record": {
            "$type": "app.bsky.feed.post",
            "text": "Hello world",
            "createdAt": "2026-03-20T12:00:00.000000Z"
        },
        "indexedAt": "2026-03-20T12:00:01.000000Z",
        "likeCount": 5,
        "repostCount": 2,
        "replyCount": 1
    });
    if let (Some(base_map), Some(over_map)) = (base.as_object_mut(), overrides.as_object()) {
        for (k, v) in over_map {
            base_map.insert(k.clone(), v.clone());
        }
    }
    base
}

// ---------------------------------------------------------------------------
// Basic text post
// ---------------------------------------------------------------------------

#[test]
fn translate_basic_text_post() {
    let json = post_view_json(serde_json::json!({}));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert_eq!(post.id, "at://did:plc:testuser/app.bsky.feed.post/abc123");
    assert_eq!(
        post.at_uri,
        "at://did:plc:testuser/app.bsky.feed.post/abc123"
    );
    assert_eq!(post.author_did, "did:plc:testuser");
    assert_eq!(post.author_handle, "alice.bsky.social");
    assert_eq!(post.author_display_name.as_deref(), Some("Alice"));
    assert!(
        post.author_avatar
            .as_ref()
            .unwrap()
            .starts_with("/api/v1/bluesky/media?url="),
        "avatar should be rewritten"
    );
    assert_eq!(post.text, "Hello world");
    assert_eq!(post.like_count, 5);
    assert_eq!(post.repost_count, 2);
    assert_eq!(post.reply_count, 1);
    assert_eq!(post.source, "bluesky");
    assert!(post.images.is_empty());
    assert!(post.video.is_none());
    assert!(post.external.is_none());
    assert!(post.quote.is_none());
    assert!(post.reposted_by.is_none());
}

// ---------------------------------------------------------------------------
// Post with images
// ---------------------------------------------------------------------------

#[test]
fn translate_post_with_images() {
    let json = post_view_json(serde_json::json!({
        "embed": {
            "$type": "app.bsky.embed.images#view",
            "images": [
                {
                    "thumb": "https://cdn.bsky.app/img/feed_thumbnail/plain/did:plc:testuser/bafkrei1/jpeg",
                    "fullsize": "https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:testuser/bafkrei1/jpeg",
                    "alt": "A test image"
                },
                {
                    "thumb": "https://cdn.bsky.app/img/feed_thumbnail/plain/did:plc:testuser/bafkrei2/jpeg",
                    "fullsize": "https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:testuser/bafkrei2/jpeg",
                    "alt": ""
                }
            ]
        }
    }));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert_eq!(post.images.len(), 2);
    assert!(
        post.images[0]
            .thumb
            .starts_with("/api/v1/bluesky/media?url=")
    );
    assert!(
        post.images[0]
            .fullsize
            .starts_with("/api/v1/bluesky/media?url=")
    );
    assert_eq!(post.images[0].alt, "A test image");
    assert!(
        post.images[1]
            .thumb
            .starts_with("/api/v1/bluesky/media?url=")
    );
}

// ---------------------------------------------------------------------------
// Post with video
// ---------------------------------------------------------------------------

#[test]
fn translate_post_with_video() {
    let json = post_view_json(serde_json::json!({
        "embed": {
            "$type": "app.bsky.embed.video#view",
            "cid": "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy",
            "playlist": "https://video.bsky.app/watch/did:plc:testuser/bafyreivid/playlist.m3u8",
            "thumbnail": "https://video.bsky.app/watch/did:plc:testuser/bafyreivid/thumbnail.jpg",
            "alt": "A test video"
        }
    }));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert!(post.video.is_some());
    let vid = post.video.unwrap();
    assert!(vid.playlist.starts_with("/api/v1/bluesky/media?url="));
    assert!(vid.thumb.unwrap().starts_with("/api/v1/bluesky/media?url="));
    assert_eq!(vid.alt.as_deref(), Some("A test video"));
}

// ---------------------------------------------------------------------------
// Post with external link embed
// ---------------------------------------------------------------------------

#[test]
fn translate_post_with_external() {
    let json = post_view_json(serde_json::json!({
        "embed": {
            "$type": "app.bsky.embed.external#view",
            "external": {
                "uri": "https://example.com/article",
                "title": "Example Article",
                "description": "An article about things",
                "thumb": "https://cdn.bsky.app/img/feed_thumbnail/plain/did:plc:testuser/bafkreiext/jpeg"
            }
        }
    }));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert!(post.external.is_some());
    let ext = post.external.unwrap();
    assert_eq!(ext.uri, "https://example.com/article");
    assert_eq!(ext.title, "Example Article");
    assert_eq!(ext.description, "An article about things");
    assert!(ext.thumb.unwrap().starts_with("/api/v1/bluesky/media?url="));
}

// ---------------------------------------------------------------------------
// Post with quote embed
// ---------------------------------------------------------------------------

#[test]
fn translate_post_with_quote() {
    let json = post_view_json(serde_json::json!({
        "embed": {
            "$type": "app.bsky.embed.record#view",
            "record": {
                "$type": "app.bsky.embed.record#viewRecord",
                "uri": "at://did:plc:quoted/app.bsky.feed.post/q1",
                "cid": "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy",
                "author": {
                    "did": "did:plc:quoted",
                    "handle": "bob.bsky.social",
                    "displayName": "Bob"
                },
                "value": {
                    "$type": "app.bsky.feed.post",
                    "text": "I am the quoted post",
                    "createdAt": "2026-03-19T10:00:00.000000Z"
                },
                "indexedAt": "2026-03-19T10:00:01.000000Z",
                "likeCount": 10,
                "repostCount": 3,
                "replyCount": 0
            }
        }
    }));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert!(post.quote.is_some());
    let q = post.quote.unwrap();
    assert_eq!(q.at_uri, "at://did:plc:quoted/app.bsky.feed.post/q1");
    assert_eq!(q.author_did, "did:plc:quoted");
    assert_eq!(q.author_handle, "bob.bsky.social");
    assert_eq!(q.text, "I am the quoted post");
    assert_eq!(q.like_count, 10);
    assert_eq!(q.source, "bluesky");
}

// ---------------------------------------------------------------------------
// Repost (FeedViewPost with reason)
// ---------------------------------------------------------------------------

#[test]
fn translate_repost() {
    let json = serde_json::json!({
        "post": post_view_json(serde_json::json!({})),
        "reason": {
            "$type": "app.bsky.feed.defs#reasonRepost",
            "by": {
                "did": "did:plc:reposter",
                "handle": "carol.bsky.social",
                "displayName": "Carol"
            },
            "indexedAt": "2026-03-20T13:00:00.000000Z"
        }
    });
    let view: FeedViewPost = serde_json::from_value(json).expect("parse FeedViewPost");
    let post = translate_post(&view);

    assert_eq!(post.reposted_by.as_deref(), Some("carol.bsky.social"));
    assert_eq!(post.author_handle, "alice.bsky.social");
}

// ---------------------------------------------------------------------------
// Post with facets
// ---------------------------------------------------------------------------

#[test]
fn translate_post_with_facets() {
    let json = post_view_json(serde_json::json!({
        "record": {
            "$type": "app.bsky.feed.post",
            "text": "Hello @bob.bsky.social check https://example.com #rust",
            "createdAt": "2026-03-20T12:00:00.000000Z",
            "facets": [
                {
                    "index": { "byteStart": 6, "byteEnd": 22 },
                    "features": [
                        {
                            "$type": "app.bsky.richtext.facet#mention",
                            "did": "did:plc:bob"
                        }
                    ]
                },
                {
                    "index": { "byteStart": 29, "byteEnd": 48 },
                    "features": [
                        {
                            "$type": "app.bsky.richtext.facet#link",
                            "uri": "https://example.com"
                        }
                    ]
                },
                {
                    "index": { "byteStart": 49, "byteEnd": 54 },
                    "features": [
                        {
                            "$type": "app.bsky.richtext.facet#tag",
                            "tag": "rust"
                        }
                    ]
                }
            ]
        }
    }));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert_eq!(post.facets.len(), 3);

    // Mention
    assert_eq!(post.facets[0].start, 6);
    assert_eq!(post.facets[0].end, 22);
    match &post.facets[0].facet_type {
        fauna_bridge_atproto::types::BlueskyFacetType::Mention { did } => {
            assert_eq!(did, "did:plc:bob");
        }
        _ => panic!("expected Mention facet"),
    }

    // Link
    match &post.facets[1].facet_type {
        fauna_bridge_atproto::types::BlueskyFacetType::Link { uri } => {
            assert_eq!(uri, "https://example.com");
        }
        _ => panic!("expected Link facet"),
    }

    // Tag
    match &post.facets[2].facet_type {
        fauna_bridge_atproto::types::BlueskyFacetType::Tag { tag } => {
            assert_eq!(tag, "rust");
        }
        _ => panic!("expected Tag facet"),
    }
}

// ---------------------------------------------------------------------------
// Post with viewer state
// ---------------------------------------------------------------------------

#[test]
fn translate_post_with_viewer_state() {
    let json = post_view_json(serde_json::json!({
        "viewer": {
            "like": "at://did:plc:me/app.bsky.feed.like/zzz",
            "repost": "at://did:plc:me/app.bsky.feed.repost/www"
        }
    }));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert_eq!(
        post.viewer_like.as_deref(),
        Some("at://did:plc:me/app.bsky.feed.like/zzz")
    );
    assert_eq!(
        post.viewer_repost.as_deref(),
        Some("at://did:plc:me/app.bsky.feed.repost/www")
    );
}

// ---------------------------------------------------------------------------
// Post with labels
// ---------------------------------------------------------------------------

#[test]
fn translate_post_with_labels() {
    let json = post_view_json(serde_json::json!({
        "labels": [
            {
                "src": "did:plc:labeler",
                "uri": "at://did:plc:testuser/app.bsky.feed.post/abc123",
                "val": "nudity",
                "cts": "2026-03-20T12:00:00.000000Z"
            },
            {
                "src": "did:plc:labeler",
                "uri": "at://did:plc:testuser/app.bsky.feed.post/abc123",
                "val": "sexual",
                "cts": "2026-03-20T12:00:00.000000Z"
            }
        ]
    }));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert_eq!(
        post.labels,
        vec!["nudity".to_string(), "sexual".to_string()]
    );
}

// ---------------------------------------------------------------------------
// Post with reply references
// ---------------------------------------------------------------------------

#[test]
fn translate_post_with_reply_refs() {
    let json = post_view_json(serde_json::json!({
        "record": {
            "$type": "app.bsky.feed.post",
            "text": "This is a reply",
            "createdAt": "2026-03-20T12:00:00.000000Z",
            "reply": {
                "parent": {
                    "uri": "at://did:plc:parent/app.bsky.feed.post/parent1",
                    "cid": "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy"
                },
                "root": {
                    "uri": "at://did:plc:root/app.bsky.feed.post/root1",
                    "cid": "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy"
                }
            }
        }
    }));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert_eq!(
        post.reply_parent.as_deref(),
        Some("at://did:plc:parent/app.bsky.feed.post/parent1")
    );
    assert_eq!(
        post.reply_root.as_deref(),
        Some("at://did:plc:root/app.bsky.feed.post/root1")
    );
}

// ---------------------------------------------------------------------------
// Actor basic (counts default to 0)
// ---------------------------------------------------------------------------

#[test]
fn translate_actor_basic_defaults() {
    let json = serde_json::json!({
        "did": "did:plc:alice",
        "handle": "alice.bsky.social",
        "displayName": "Alice",
        "avatar": "https://cdn.bsky.app/img/avatar/plain/did:plc:alice/bafkreiavatar/jpeg"
    });
    let view: ProfileViewBasic = serde_json::from_value(json).expect("parse ProfileViewBasic");
    let actor = translate_actor_basic(&view);

    assert_eq!(actor.did, "did:plc:alice");
    assert_eq!(actor.handle, "alice.bsky.social");
    assert_eq!(actor.display_name.as_deref(), Some("Alice"));
    assert!(actor.description.is_none());
    assert!(
        actor
            .avatar
            .as_ref()
            .unwrap()
            .starts_with("/api/v1/bluesky/media?url=")
    );
    assert_eq!(actor.followers_count, 0);
    assert_eq!(actor.follows_count, 0);
    assert_eq!(actor.posts_count, 0);
    assert!(actor.viewer_following.is_none());
    assert!(actor.viewer_followed_by.is_none());
}

// ---------------------------------------------------------------------------
// Actor detailed (counts populated)
// ---------------------------------------------------------------------------

#[test]
fn translate_actor_detailed_with_counts() {
    let json = serde_json::json!({
        "did": "did:plc:alice",
        "handle": "alice.bsky.social",
        "displayName": "Alice",
        "description": "Hello I am Alice",
        "avatar": "https://cdn.bsky.app/img/avatar/plain/did:plc:alice/bafkreiavatar/jpeg",
        "followersCount": 1000,
        "followsCount": 500,
        "postsCount": 2000,
        "indexedAt": "2026-03-20T12:00:00.000000Z",
        "viewer": {
            "following": "at://did:plc:me/app.bsky.graph.follow/zzz",
            "followedBy": "at://did:plc:alice/app.bsky.graph.follow/yyy"
        }
    });
    let view: ProfileViewDetailed =
        serde_json::from_value(json).expect("parse ProfileViewDetailed");
    let actor = translate_actor_detailed(&view);

    assert_eq!(actor.did, "did:plc:alice");
    assert_eq!(actor.handle, "alice.bsky.social");
    assert_eq!(actor.description.as_deref(), Some("Hello I am Alice"));
    assert_eq!(actor.followers_count, 1000);
    assert_eq!(actor.follows_count, 500);
    assert_eq!(actor.posts_count, 2000);
    assert_eq!(
        actor.viewer_following.as_deref(),
        Some("at://did:plc:me/app.bsky.graph.follow/zzz")
    );
    assert_eq!(
        actor.viewer_followed_by.as_deref(),
        Some("at://did:plc:alice/app.bsky.graph.follow/yyy")
    );
}

// ---------------------------------------------------------------------------
// Actor from profile view (search results)
// ---------------------------------------------------------------------------

#[test]
fn translate_actor_from_profile_view_works() {
    let json = serde_json::json!({
        "did": "did:plc:bob",
        "handle": "bob.bsky.social",
        "displayName": "Bob",
        "description": "I am Bob"
    });
    let view: ProfileView = serde_json::from_value(json).expect("parse ProfileView");
    let actor = translate_actor_from_profile_view(&view);

    assert_eq!(actor.did, "did:plc:bob");
    assert_eq!(actor.handle, "bob.bsky.social");
    assert_eq!(actor.description.as_deref(), Some("I am Bob"));
    // ProfileView lacks counts
    assert_eq!(actor.followers_count, 0);
    assert_eq!(actor.follows_count, 0);
    assert_eq!(actor.posts_count, 0);
}

// ---------------------------------------------------------------------------
// Feed generator
// ---------------------------------------------------------------------------

#[test]
fn translate_feed_generator_works() {
    let json = serde_json::json!({
        "uri": "at://did:plc:feedmaker/app.bsky.feed.generator/trending",
        "cid": "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy",
        "did": "did:web:feed.example.com",
        "creator": {
            "did": "did:plc:feedmaker",
            "handle": "feedmaker.bsky.social",
            "displayName": "Feed Maker"
        },
        "displayName": "Trending",
        "description": "Trending posts in the last 24 hours",
        "avatar": "https://cdn.bsky.app/img/avatar/plain/did:plc:feedmaker/bafkreifeed/jpeg",
        "likeCount": 42,
        "indexedAt": "2026-03-20T12:00:00.000000Z"
    });
    let view: GeneratorView = serde_json::from_value(json).expect("parse GeneratorView");
    let fg = translate_feed_generator(&view);

    assert_eq!(
        fg.uri,
        "at://did:plc:feedmaker/app.bsky.feed.generator/trending"
    );
    assert_eq!(fg.did, "did:plc:feedmaker");
    assert_eq!(fg.display_name, "Trending");
    assert_eq!(
        fg.description.as_deref(),
        Some("Trending posts in the last 24 hours")
    );
    assert!(
        fg.avatar
            .as_ref()
            .unwrap()
            .starts_with("/api/v1/bluesky/media?url=")
    );
    assert_eq!(fg.like_count, 42);
}

// ---------------------------------------------------------------------------
// Thread translation
// ---------------------------------------------------------------------------

#[test]
fn translate_thread_collects_ancestors_and_replies() {
    let json = serde_json::json!({
        "post": post_view_json(serde_json::json!({
            "uri": "at://did:plc:testuser/app.bsky.feed.post/focal",
            "record": {
                "$type": "app.bsky.feed.post",
                "text": "Focal post",
                "createdAt": "2026-03-20T12:00:00.000000Z"
            }
        })),
        "parent": {
            "$type": "app.bsky.feed.defs#threadViewPost",
            "post": post_view_json(serde_json::json!({
                "uri": "at://did:plc:testuser/app.bsky.feed.post/parent",
                "record": {
                    "$type": "app.bsky.feed.post",
                    "text": "Parent post",
                    "createdAt": "2026-03-20T11:00:00.000000Z"
                }
            })),
            "parent": {
                "$type": "app.bsky.feed.defs#threadViewPost",
                "post": post_view_json(serde_json::json!({
                    "uri": "at://did:plc:testuser/app.bsky.feed.post/grandparent",
                    "record": {
                        "$type": "app.bsky.feed.post",
                        "text": "Grandparent post",
                        "createdAt": "2026-03-20T10:00:00.000000Z"
                    }
                }))
            }
        },
        "replies": [
            {
                "$type": "app.bsky.feed.defs#threadViewPost",
                "post": post_view_json(serde_json::json!({
                    "uri": "at://did:plc:testuser/app.bsky.feed.post/reply1",
                    "record": {
                        "$type": "app.bsky.feed.post",
                        "text": "Reply 1",
                        "createdAt": "2026-03-20T13:00:00.000000Z"
                    }
                }))
            },
            {
                "$type": "app.bsky.feed.defs#threadViewPost",
                "post": post_view_json(serde_json::json!({
                    "uri": "at://did:plc:testuser/app.bsky.feed.post/reply2",
                    "record": {
                        "$type": "app.bsky.feed.post",
                        "text": "Reply 2",
                        "createdAt": "2026-03-20T14:00:00.000000Z"
                    }
                }))
            }
        ]
    });
    let view: ThreadViewPost = serde_json::from_value(json).expect("parse ThreadViewPost");
    let (posts, focal_index) = translate_thread(&view);

    // Should be: grandparent, parent, focal, reply1, reply2
    assert_eq!(posts.len(), 5);
    assert!(
        posts[0].at_uri.contains("grandparent"),
        "first should be grandparent"
    );
    assert!(
        posts[1].at_uri.contains("parent"),
        "second should be parent"
    );
    assert!(posts[2].at_uri.contains("focal"), "third should be focal");
    assert!(
        posts[3].at_uri.contains("reply1"),
        "fourth should be reply1"
    );
    assert!(posts[4].at_uri.contains("reply2"), "fifth should be reply2");

    assert_eq!(posts[0].text, "Grandparent post");
    assert_eq!(posts[2].text, "Focal post");
    assert_eq!(posts[4].text, "Reply 2");

    // The focal index names the requested post, after its two ancestors.
    assert_eq!(focal_index, 2);
    assert_eq!(posts[focal_index].at_uri, view.post.uri);
}

/// A thread view node for `uri` with the given parent/replies JSON.
fn thread_node_json(
    uri: &str,
    parent: Option<serde_json::Value>,
    replies: Vec<serde_json::Value>,
) -> serde_json::Value {
    let mut node = serde_json::json!({
        "$type": "app.bsky.feed.defs#threadViewPost",
        "post": post_view_json(serde_json::json!({ "uri": uri })),
        "replies": replies,
    });
    if let Some(parent) = parent {
        node["parent"] = parent;
    }
    node
}

#[test]
fn translate_thread_focal_index_with_no_replies() {
    // The focal post is the last post: nothing follows it.
    let focal_uri = "at://did:plc:testuser/app.bsky.feed.post/focal";
    let parent = thread_node_json(
        "at://did:plc:testuser/app.bsky.feed.post/parent",
        None,
        vec![],
    );
    let json = thread_node_json(focal_uri, Some(parent), vec![]);
    let view: ThreadViewPost = serde_json::from_value(json).expect("parse ThreadViewPost");
    let (posts, focal_index) = translate_thread(&view);

    assert_eq!(posts.len(), 2);
    assert_eq!(focal_index, 1);
    assert_eq!(posts[focal_index].at_uri, focal_uri);
}

#[test]
fn translate_thread_focal_index_when_ancestor_has_more_replies() {
    // The old client heuristic picked the post with the most direct replies;
    // here an ancestor has two replies in the list's shape (`replyCount`) but
    // the focal post has one, and the focal index must still name the focal.
    let focal_uri = "at://did:plc:testuser/app.bsky.feed.post/focal";
    let mut parent = thread_node_json(
        "at://did:plc:testuser/app.bsky.feed.post/parent",
        None,
        vec![],
    );
    parent["post"]["replyCount"] = serde_json::json!(5);
    let reply = thread_node_json(
        "at://did:plc:testuser/app.bsky.feed.post/reply1",
        None,
        vec![],
    );
    let json = thread_node_json(focal_uri, Some(parent), vec![reply]);
    let view: ThreadViewPost = serde_json::from_value(json).expect("parse ThreadViewPost");
    let (posts, focal_index) = translate_thread(&view);

    assert_eq!(posts.len(), 3);
    assert_eq!(focal_index, 1);
    assert_eq!(posts[focal_index].at_uri, focal_uri);
}

// ---------------------------------------------------------------------------
// Record-with-media embed (quote + images)
// ---------------------------------------------------------------------------

#[test]
fn translate_post_with_record_and_media() {
    let json = post_view_json(serde_json::json!({
        "embed": {
            "$type": "app.bsky.embed.recordWithMedia#view",
            "media": {
                "$type": "app.bsky.embed.images#view",
                "images": [
                    {
                        "thumb": "https://cdn.bsky.app/img/feed_thumbnail/plain/did:plc:testuser/bafkrei1/jpeg",
                        "fullsize": "https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:testuser/bafkrei1/jpeg",
                        "alt": "Media image"
                    }
                ]
            },
            "record": {
                "$type": "app.bsky.embed.record#view",
                "record": {
                    "$type": "app.bsky.embed.record#viewRecord",
                    "uri": "at://did:plc:quoted/app.bsky.feed.post/q2",
                    "cid": "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy",
                    "author": {
                        "did": "did:plc:quoted",
                        "handle": "bob.bsky.social"
                    },
                    "value": {
                        "$type": "app.bsky.feed.post",
                        "text": "Quoted text",
                        "createdAt": "2026-03-19T10:00:00.000000Z"
                    },
                    "indexedAt": "2026-03-19T10:00:01.000000Z"
                }
            }
        }
    }));
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    // Should have both images and a quote
    assert_eq!(post.images.len(), 1);
    assert_eq!(post.images[0].alt, "Media image");
    assert!(post.quote.is_some());
    assert_eq!(post.quote.as_ref().unwrap().text, "Quoted text");
}

// ---------------------------------------------------------------------------
// FeedViewPost without reason (not a repost)
// ---------------------------------------------------------------------------

#[test]
fn translate_feed_view_post_no_reason() {
    let json = serde_json::json!({
        "post": post_view_json(serde_json::json!({}))
    });
    let view: FeedViewPost = serde_json::from_value(json).expect("parse FeedViewPost");
    let post = translate_post(&view);

    assert!(post.reposted_by.is_none());
    assert_eq!(post.text, "Hello world");
}

// ---------------------------------------------------------------------------
// Edge case: missing optional counts
// ---------------------------------------------------------------------------

#[test]
fn translate_post_missing_counts() {
    let json = serde_json::json!({
        "uri": "at://did:plc:testuser/app.bsky.feed.post/nocount",
        "cid": "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy",
        "author": {
            "did": "did:plc:testuser",
            "handle": "alice.bsky.social"
        },
        "record": {
            "$type": "app.bsky.feed.post",
            "text": "No counts",
            "createdAt": "2026-03-20T12:00:00.000000Z"
        },
        "indexedAt": "2026-03-20T12:00:01.000000Z"
    });
    let view: PostView = serde_json::from_value(json).expect("parse PostView");
    let post = translate_post_view(&view);

    assert_eq!(post.like_count, 0);
    assert_eq!(post.repost_count, 0);
    assert_eq!(post.reply_count, 0);
}
