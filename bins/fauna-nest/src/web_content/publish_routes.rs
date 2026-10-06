//! Web-content publish/unpublish core logic — shared by the
//! `fauna.web.publish.{set,unset,list}` WS-RPC handlers (`web_handlers.rs`).
//!
//! The HTTP twins (`handle_publish`/`handle_unpublish`/`handle_list_published`)
//! were deleted in the orphaned-twin residue sweep
//! (no remaining consumer); these
//! transport-agnostic cores are the surviving surface.

use crate::db::CacheDb;

// ==================== Core logic ====================

/// Publish a post for an actor under the given slug.
/// Creates or updates the `web_published` content_links row.
/// Returns the link id.
pub async fn publish_post(
    db: &CacheDb,
    actor_id: &[u8; 32],
    post_id: &[u8; 32],
    slug: Option<&str>,
) -> anyhow::Result<i64> {
    // Default slug: hex encoding of the post_id
    let default_slug;
    let effective_slug = match slug {
        Some(s) if !s.is_empty() => s,
        _ => {
            default_slug = hex::encode(post_id);
            &default_slug
        }
    };
    db.publish_web_post(actor_id, post_id, effective_slug).await
}

/// Unpublish a post: remove the `web_published` row.
pub async fn unpublish_post(
    db: &CacheDb,
    actor_id: &[u8; 32],
    post_id: &[u8; 32],
) -> anyhow::Result<()> {
    db.unpublish_web_post(actor_id, post_id).await
}

/// List all published posts for an actor.
/// Returns `(post_id_bytes, slug, gated_tier)` triples — the tier is the
/// `content_meta` LEFT JOIN that tells the management surface which rows carry
/// the *Copy paywall link* affordance (`web-content-hosting.md`
/// § Published-post management).
#[allow(clippy::type_complexity)]
pub async fn get_published_posts(
    db: &CacheDb,
    actor_id: &[u8; 32],
) -> anyhow::Result<Vec<(Vec<u8>, String, Option<String>)>> {
    db.list_web_published(actor_id).await
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    fn actor() -> [u8; 32] {
        [0xAAu8; 32]
    }

    fn post1() -> [u8; 32] {
        [0x01u8; 32]
    }

    fn post2() -> [u8; 32] {
        [0x02u8; 32]
    }

    fn db() -> CacheDb {
        CacheDb::open_in_memory().unwrap()
    }

    #[tokio::test]
    async fn publish_post_creates_link() {
        let db = db();
        let actor = actor();
        let post = post1();

        let link_id = publish_post(&db, &actor, &post, Some("hello-world"))
            .await
            .unwrap();
        assert!(link_id > 0);

        // Verify the row exists by listing
        let published = get_published_posts(&db, &actor).await.unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, post.as_slice());
        assert_eq!(published[0].1, "hello-world");
    }

    #[tokio::test]
    async fn unpublish_post_removes_link() {
        let db = db();
        let actor = actor();
        let post = post1();

        publish_post(&db, &actor, &post, Some("my-slug"))
            .await
            .unwrap();

        // Verify it exists
        let before = get_published_posts(&db, &actor).await.unwrap();
        assert_eq!(before.len(), 1);

        unpublish_post(&db, &actor, &post).await.unwrap();

        // Verify it's gone
        let after = get_published_posts(&db, &actor).await.unwrap();
        assert!(after.is_empty());
    }

    #[tokio::test]
    async fn list_published_posts() {
        let db = db();
        let actor = actor();
        let p1 = post1();
        let p2 = post2();

        publish_post(&db, &actor, &p1, Some("first-post"))
            .await
            .unwrap();
        publish_post(&db, &actor, &p2, Some("second-post"))
            .await
            .unwrap();

        let published = get_published_posts(&db, &actor).await.unwrap();
        assert_eq!(published.len(), 2);

        let slugs: Vec<&str> = published.iter().map(|(_, s, _)| s.as_str()).collect();
        assert!(slugs.contains(&"first-post"));
        assert!(slugs.contains(&"second-post"));
        assert!(
            published.iter().all(|(_, _, tier)| tier.is_none()),
            "posts with no content_meta row are ungated, not dropped by the join"
        );
    }

    #[tokio::test]
    async fn publish_uses_default_slug_when_none() {
        let db = db();
        let actor = actor();
        let post = post1();

        publish_post(&db, &actor, &post, None).await.unwrap();

        let published = get_published_posts(&db, &actor).await.unwrap();
        assert_eq!(published.len(), 1);
        // Default slug is the hex-encoded post_id
        assert_eq!(published[0].1, hex::encode(post));
    }

    #[tokio::test]
    async fn publish_upserts_on_second_call() {
        let db = db();
        let actor = actor();
        let post = post1();

        let id1 = publish_post(&db, &actor, &post, Some("original-slug"))
            .await
            .unwrap();
        let id2 = publish_post(&db, &actor, &post, Some("updated-slug"))
            .await
            .unwrap();

        // Same underlying row — same id
        assert_eq!(id1, id2);

        let published = get_published_posts(&db, &actor).await.unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].1, "updated-slug");
    }

    #[tokio::test]
    async fn unpublish_is_idempotent() {
        let db = db();
        let actor = actor();
        let post = post1();

        // Unpublishing something that was never published should not error
        unpublish_post(&db, &actor, &post).await.unwrap();

        // Publish and unpublish twice
        publish_post(&db, &actor, &post, Some("slug"))
            .await
            .unwrap();
        unpublish_post(&db, &actor, &post).await.unwrap();
        unpublish_post(&db, &actor, &post).await.unwrap();

        let published = get_published_posts(&db, &actor).await.unwrap();
        assert!(published.is_empty());
    }
}
