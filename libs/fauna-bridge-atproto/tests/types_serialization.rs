//! Verify Bluesky bridge types serialize to the shape expected by bluesky.ts.

use fauna_bridge_atproto::types::*;

#[test]
fn bluesky_post_serializes_flat_author() {
    let post = BlueskyPost {
        id: "abc".into(),
        at_uri: "at://did:plc:xxx/app.bsky.feed.post/yyy".into(),
        cid: "bafyreigz".into(),
        author_did: "did:plc:xxx".into(),
        author_handle: "alice.bsky.social".into(),
        author_display_name: Some("Alice".into()),
        author_avatar: Some("/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg".into()),
        text: "Hello world".into(),
        facets: vec![],
        images: vec![],
        video: None,
        external: None,
        quote: None,
        reply_parent: None,
        reply_root: None,
        reposted_by: None,
        like_count: 5,
        repost_count: 2,
        reply_count: 1,
        viewer_like: Some("at://did:plc:xxx/app.bsky.feed.like/zzz".into()),
        viewer_repost: None,
        labels: vec!["nudity".into()],
        created_at: "2026-03-20T12:00:00Z".into(),
        source: "bluesky".into(),
    };
    let json = serde_json::to_value(&post).unwrap();
    assert_eq!(json["author_did"], "did:plc:xxx");
    assert_eq!(json["author_handle"], "alice.bsky.social");
    assert!(
        json.get("author").is_none(),
        "should not have nested author"
    );
    assert!(json.get("at_uri").is_some());
    assert!(json.get("viewer_like").is_some());
    assert!(json.get("like_count").is_some());
}

#[test]
fn bluesky_facet_serializes_flat() {
    let facet = BlueskyFacet {
        start: 0,
        end: 5,
        facet_type: BlueskyFacetType::Tag { tag: "rust".into() },
    };
    let json = serde_json::to_value(&facet).unwrap();
    assert_eq!(json["start"], 0);
    assert_eq!(json["end"], 5);
    assert!(json.get("index").is_none(), "should not have nested index");
}

#[test]
fn bluesky_actor_has_viewer_record_uris() {
    let actor = BlueskyActor {
        did: "did:plc:xxx".into(),
        handle: "alice.bsky.social".into(),
        display_name: Some("Alice".into()),
        description: None,
        avatar: None,
        followers_count: 100,
        follows_count: 50,
        posts_count: 200,
        viewer_following: Some("at://did:plc:me/app.bsky.graph.follow/zzz".into()),
        viewer_followed_by: None,
    };
    let json = serde_json::to_value(&actor).unwrap();
    assert!(json["viewer_following"].is_string());
    assert!(json["viewer_followed_by"].is_null());
    assert!(
        json.get("followed_by_me").is_none(),
        "old bool field removed"
    );
}

#[test]
fn bluesky_dm_has_flat_sender() {
    let dm = BlueskyDm {
        id: "msg1".into(),
        convo_id: "convo1".into(),
        sender_did: "did:plc:bob".into(),
        sender_handle: "bob.bsky.social".into(),
        sender_display_name: Some("Bob".into()),
        text: "Hey".into(),
        sent_at: "2026-03-20T12:00:00Z".into(),
    };
    let json = serde_json::to_value(&dm).unwrap();
    assert_eq!(json["sender_did"], "did:plc:bob");
    assert_eq!(json["convo_id"], "convo1");
    assert!(
        json.get("sender").is_none(),
        "should not have nested sender"
    );
}
