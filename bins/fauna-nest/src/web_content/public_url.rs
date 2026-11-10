//! A post's public web URL, derived nest-side — the same link the apps'
//! *Copy web link* copies (`web-content-hosting.md` § Published-post
//! management), computed by the one shared rule
//! ([`fauna_core::web::site_origin`] + [`fauna_core::web::post_page_url`]) from
//! the inputs the nest holds: the author's `web_published` slug, their active
//! custom domain, their subdomain opt-in, their handle and the nest's web
//! serving domain.
//!
//! Its consumer is the Nostr advertisement of a sold post, whose note carries
//! the URL as its `r` tag (`monetization.md` § The Nostr advertisement of a sold
//! post → *The event*).

use crate::db::CacheDb;

/// The public page URL of `post_id` as published by `author`, or `None` when
/// the post is not web-published by them or their content has no public
/// origin (subdomain off and no active custom domain, a reserved or missing
/// handle, a nest that serves no web content).
///
/// `serving_domain` is `AppState::web_serving_domain()`, read by the caller —
/// verbatim, empty included.
pub async fn public_post_url(
    db: &CacheDb,
    author: &[u8; 32],
    post_id: &[u8; 32],
    serving_domain: &str,
) -> anyhow::Result<Option<String>> {
    let Some(slug) = db.web_published_slug(author, post_id).await? else {
        return Ok(None);
    };
    let Some(origin) = site_origin_of(db, author, serving_domain).await? else {
        return Ok(None);
    };
    Ok(Some(fauna_core::web::post_page_url(&origin, &slug)))
}

/// The author's site origin, or `None` when it has none — the nest-side read
/// of the inputs `fauna_client_web::site_link_view` takes from the apps' Web
/// settings state.
async fn site_origin_of(
    db: &CacheDb,
    author: &[u8; 32],
    serving_domain: &str,
) -> anyhow::Result<Option<String>> {
    // Registration order, as `fauna.web.domain.get` hands the apps the rows.
    let domains = db.get_web_domains_for_actor(author).await?;
    let active = domains
        .iter()
        .find(|row| row.status == "active")
        .map(|row| row.domain.as_str());
    let subdomain_enabled = db.is_subdomain_enabled(author).await?;
    let handle = db.get_handle(author).await?;
    Ok(
        fauna_core::web::site_origin(active, subdomain_enabled, handle.as_deref(), serving_domain)
            .ok(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTHOR: [u8; 32] = [0xA1; 32];
    const POST: [u8; 32] = [0x01; 32];

    async fn db_with_handle(handle: &str) -> CacheDb {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_user_with_handle(&AUTHOR, "free", handle, None)
            .await
            .unwrap();
        db
    }

    #[tokio::test]
    async fn published_post_on_the_subdomain() {
        let db = db_with_handle("alice").await;
        db.publish_web_post(&AUTHOR, &POST, "hello").await.unwrap();
        db.set_subdomain_enabled(&AUTHOR).await.unwrap();
        assert_eq!(
            public_post_url(&db, &AUTHOR, &POST, "example.org")
                .await
                .unwrap()
                .as_deref(),
            Some("https://alice.example.org/post/hello.html")
        );
    }

    #[tokio::test]
    async fn an_active_custom_domain_wins_and_a_pending_one_does_not() {
        let db = db_with_handle("alice").await;
        db.publish_web_post(&AUTHOR, &POST, "hello").await.unwrap();
        db.set_subdomain_enabled(&AUTHOR).await.unwrap();
        db.insert_web_domain(&AUTHOR, "blog.example.com", "tok")
            .await
            .unwrap();
        // Pending: no cert yet, so the subdomain stays the link.
        assert_eq!(
            public_post_url(&db, &AUTHOR, &POST, "example.org")
                .await
                .unwrap()
                .as_deref(),
            Some("https://alice.example.org/post/hello.html")
        );
        db.update_web_domain_status("blog.example.com", "active")
            .await
            .unwrap();
        assert_eq!(
            public_post_url(&db, &AUTHOR, &POST, "example.org")
                .await
                .unwrap()
                .as_deref(),
            Some("https://blog.example.com/post/hello.html")
        );
    }

    #[tokio::test]
    async fn unpublished_or_originless_is_none() {
        let db = db_with_handle("alice").await;
        db.set_subdomain_enabled(&AUTHOR).await.unwrap();
        // Not published.
        assert_eq!(
            public_post_url(&db, &AUTHOR, &POST, "example.org")
                .await
                .unwrap(),
            None
        );
        db.publish_web_post(&AUTHOR, &POST, "hello").await.unwrap();
        // A nest that serves no web content.
        assert_eq!(
            public_post_url(&db, &AUTHOR, &POST, "").await.unwrap(),
            None
        );
        // Subdomain hosting off, no custom domain.
        db.clear_subdomain_enabled(&AUTHOR).await.unwrap();
        assert_eq!(
            public_post_url(&db, &AUTHOR, &POST, "example.org")
                .await
                .unwrap(),
            None
        );
    }
}
