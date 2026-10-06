use std::sync::Arc;

use fauna_core::data::*;
use fauna_core::identity::ActorKeypair;

#[tokio::test]
async fn video_post_indexed_correctly() {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().unwrap());

    let kp = ActorKeypair::generate();
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Video {
            manifest: ContentHash::from_digest_raw([1u8; 32]),
            segments: vec![VideoSegment {
                hash: ContentHash::from_digest_raw([2u8; 32]),
                resolution: 720,
                codec: "h264".to_string(),
                bitrate: 2500,
                byte_size: 5_000_000,
            }],
            thumbnail: ContentHash::from_digest_raw([3u8; 32]),
            duration_ms: 15_000,
            aspect_ratio: (9, 16),
            anchors: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let post_bytes = fauna_core::encoding::sign_and_pack(&kp, &post).unwrap();
    let post_id = fauna_core::encoding::compute_post_id(&post).unwrap();
    let post_id_bytes: [u8; 32] = {
        let b = post_id.as_bytes();
        let mut d = [0u8; 32];
        d.copy_from_slice(&b[4..]);
        d
    };

    db.put_post(&post_id_bytes, &post_bytes, None)
        .await
        .unwrap();

    // Verify schema and has_media via raw SQL
    let conn = db.conn().await;
    let schema: String = conn
        .query_row(
            "SELECT schema FROM content WHERE id = ?1",
            [post_id_bytes.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema, "post/video");

    let has_media: i64 = conn
        .query_row(
            "SELECT has_media FROM content_meta WHERE content_id = ?1",
            [post_id_bytes.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(has_media, 1);
}
