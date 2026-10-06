//! Integration tests for the engagement dedup log and the public counters.
//!
//! `engagement_events` is a **write-only dedup log**: the live explicit-act paths
//! `INSERT OR IGNORE` a stable event id (and `DELETE` it on unlike), and nothing
//! reads the rows back. The per-actor read that used to exist
//! (`get_engagement_events_for_content`, behind `fauna.engagement.list`) was
//! retired with the pre-frame behavioral surface (frame D9 —
//! `docs/goal/behavior/engagement-cues.md` § Retirement), so these tests assert
//! through the surface that survives: the insert's *first-insert* boolean — which
//! is precisely the signal the counter cores gate on — and the counters themselves.

#[tokio::test]
async fn insert_engagement_event_dedups_by_event_id() {
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();

    let content_id = [0xCC; 32];
    let event_id = [0xDD; 32];

    // The returned bool IS the dedup contract: a counter moves only on
    // first-insert, which is what makes re-liking idempotent.
    let first = db
        .insert_engagement_event(&event_id, &content_id, None, "like", None, 5000)
        .await
        .unwrap();
    assert!(
        first,
        "first insert of a fresh event id should report inserted"
    );

    let second = db
        .insert_engagement_event(&event_id, &content_id, None, "like", None, 6000)
        .await
        .unwrap();
    assert!(
        !second,
        "re-inserting the same event id must be ignored (no second counter bump)"
    );

    // A distinct event id on the same content is a distinct event.
    let other = db
        .insert_engagement_event(&[0xEE; 32], &content_id, None, "like", None, 7000)
        .await
        .unwrap();
    assert!(other, "a different event id is a different event");
}

#[tokio::test]
async fn engagement_counts_on_content_meta() {
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();

    // Create a real post so content_meta gets a row
    let kp = fauna_core::identity::ActorKeypair::generate();
    let post = fauna_core::data::Post {
        author: kp.actor_id(),
        created_at: fauna_core::data::Timestamp::now(),
        body: fauna_core::data::PostBody::Text {
            content: "hello engagement".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let (post_bytes, env) = fauna_core::encoding::sign_envelope(&kp, &post).unwrap();
    let wire = fauna_core::encoding::EmbedAsBytes::from_signed(post_bytes, env);
    let bytes = fauna_core::encoding::canonical_encode(&wire).unwrap();
    let post_id = fauna_core::encoding::compute_post_id(&post).unwrap();
    let pid: [u8; 32] = {
        let b = post_id.as_bytes();
        let mut d = [0u8; 32];
        d.copy_from_slice(&b[4..]);
        d
    };

    db.put_post(&pid, &bytes, None).await.unwrap();

    // Counts should start at zero
    let counts = db.get_engagement_counts(&pid).await.unwrap();
    assert_eq!(counts.like_count, 0);
    assert_eq!(counts.reply_count, 0);
    assert_eq!(counts.repost_count, 0);

    // Increment various counts
    db.increment_engagement_count(&pid, "like").await.unwrap();
    db.increment_engagement_count(&pid, "like").await.unwrap();
    db.increment_engagement_count(&pid, "reply").await.unwrap();
    db.increment_engagement_count(&pid, "repost").await.unwrap();

    let counts = db.get_engagement_counts(&pid).await.unwrap();
    assert_eq!(counts.like_count, 2);
    assert_eq!(counts.reply_count, 1);
    assert_eq!(counts.repost_count, 1);
}
