//! The ingest half of consume-side Bluesky feed ingestion — every decision made
//! about a page of posts once the network handed it over (`bridges.md`
//! § Unified feed ingestion → *Bridge ingestion*, rulings 2, 3, 4 and 7).
//!
//! Split from the network half ([`super::feed_worker::poll_bluesky_feeds`])
//! exactly as the notification poller is split from `notif_sync::ingest_notifications`,
//! and for the same reason: the harness has no consume-side far end, so this
//! half is pinned against a bare [`CacheDb`] and a temp segment store.
//!
//! What one [`IngestablePost`] becomes, in order:
//!
//! 1. **Dedupe** — its `at_uri` already in `bluesky_posts` → nothing (the map's
//!    `UNIQUE(at_uri)` is the dedupe; a re-poll or a second account's overlapping
//!    timeline adds no row).
//! 2. **References** — the reply parent and the quote target resolve through the
//!    map to a local content id, or the post rests top-level. Never a synthetic
//!    id. The shared translator lists a page item's parent and quoted record
//!    BEFORE the item, so a one-level thread resolves within the pass.
//! 3. **The future bound** — `reject_future_bridged_created_at`, silently, before
//!    any write (`feed.md` § The read model).
//! 4. **The store** — `segments::post::store_post` with source `bluesky`: body in
//!    the synthetic author's `__post` segment, the feed-index projection beside it.
//! 5. **The map row** — `insert_ingested_post_mapping` (`at_uri` + `cid` +
//!    `author_did`); a lost race here leaves the content row, which the map's
//!    winner owns identically (same bytes, same id).
//! 6. **The Search corpus** — `index_bridge_content` under `bridge.bluesky`.

use fauna_bridge_atproto::ingest::{IngestablePost, build_fauna_post};
use fauna_core::data::Reference;
use fauna_segment_store::SegmentManager;

use crate::bluesky::db_helpers;
use crate::db::CacheDb;

/// The `bridge_authors` row one ingested post's author projects: the synthetic
/// id over the DID, the handle, the `displayName`, and the avatar the shared
/// translator already put behind the privacy proxy.
fn bridge_author_of(
    ing: &IngestablePost,
    now_micros: i64,
) -> crate::db::bridge_authors::BridgeAuthor {
    crate::db::bridge_authors::BridgeAuthor {
        actor_id: fauna_bridge_atproto::ingest::synthetic_actor_id(&ing.author_did).0,
        bridge: crate::db::bridge_authors::BRIDGE_BLUESKY.into(),
        external_id: ing.author_did.clone(),
        handle: Some(ing.author_handle.clone()),
        display_name: ing.author_display_name.clone(),
        avatar_url: ing.author_avatar.clone(),
        updated_at: now_micros,
    }
}

/// A post this pass stored — what the caller logs or pushes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IngestedPost {
    pub post_id_hex: String,
    pub at_uri: String,
}

/// Store every not-yet-held post in `posts`, in the order given, returning
/// the ones this call stored.
///
/// A post that fails to encode or store is logged and skipped; the next post
/// on the page is unrelated to it.
pub(crate) async fn ingest_feed_posts(
    db: &CacheDb,
    post_segments: &SegmentManager,
    posts: &[IngestablePost],
) -> anyhow::Result<Vec<IngestedPost>> {
    let mut stored = Vec::new();

    // The bridged-author transit point (`bridges.md` § Unified feed ingestion
    // → *Bridged authors*): every author this page carries, once, BEFORE the
    // per-post dedupe — a re-poll of an already-held window still refreshes
    // the face. Stamped with the write instant (a page carries no profile
    // timestamp); the upsert keeps the newest.
    {
        let conn = db.conn().await;
        let now = crate::db::now_epoch_micros();
        let mut seen = std::collections::HashSet::new();
        for ing in posts {
            if !seen.insert(ing.author_did.as_str()) {
                continue;
            }
            if let Err(e) = crate::db::bridge_authors::upsert(&conn, &bridge_author_of(ing, now)) {
                tracing::debug!(did = %ing.author_did, "bluesky feed ingest: author face skipped: {e:#}");
            }
        }
    }

    for ing in posts {
        let references = {
            let conn = db.conn().await;
            if db_helpers::get_post_id_for_at_uri(&conn, &ing.at_uri)?.is_some() {
                continue;
            }
            resolve_references(&conn, ing)?
        };

        let post = build_fauna_post(ing, references);

        // The bridged planes' future bound (`feed.md` § The read model): the
        // record's own `createdAt` becomes the column the local feed sorts on.
        // Refused, never clamped — a re-dated post would change the hashed id
        // every replay re-derives.
        if crate::storage::reject_future_bridged_created_at(post.created_at).is_err() {
            tracing::info!(
                at_uri = %ing.at_uri,
                "bluesky feed ingest: dropping a post dated past this nest's future bound"
            );
            continue;
        }

        let payload = fauna_core::encoding::canonical_encode(&post)?;
        let post_id: [u8; 32] = *blake3::hash(&payload).as_bytes();
        let post_id_hex = hex::encode(post_id);

        if let Err(e) = crate::segments::post::store_post(
            post_segments,
            db,
            &post_id,
            &payload,
            Some("bluesky"),
        )
        .await
        {
            tracing::warn!(at_uri = %ing.at_uri, "bluesky feed ingest: store failed: {e:#}");
            continue;
        }

        let conn = db.conn().await;
        let inserted = db_helpers::insert_ingested_post_mapping(
            &conn,
            &post_id_hex,
            &ing.at_uri,
            &ing.cid,
            &ing.author_did,
        )?;
        if !inserted {
            // A concurrent pass mapped the same URI between our dedupe read and
            // this insert. Same record, same bytes, same content id — the row
            // it holds is ours; nothing to undo.
            continue;
        }
        // The Search-corpus transit point (`content-index.md` § Bridge content
        // in the Search corpus): keyed on the at-uri, so a post seen by two
        // accounts' pages indexes once. `created_at` in epoch micros, the
        // corpus's unit.
        if let Err(e) = crate::db::bridge_search::index_bridge_content(
            &conn,
            "bluesky",
            &ing.at_uri,
            &ing.author_handle,
            &post.body_text(),
            post.created_at.as_i64(),
            Some(&post_id),
        ) {
            tracing::debug!(at_uri = %ing.at_uri, "bluesky feed ingest: corpus index skipped: {e:#}");
        }
        drop(conn);

        stored.push(IngestedPost {
            post_id_hex,
            at_uri: ing.at_uri.clone(),
        });
    }

    Ok(stored)
}

/// The references a post carries INTO the store: its reply parent and its
/// quote target, each only when the map already holds the target (ruling 3).
fn resolve_references(
    conn: &rusqlite::Connection,
    ing: &IngestablePost,
) -> anyhow::Result<Vec<Reference>> {
    let mut refs = Vec::new();
    if let Some(parent) = ing.reply_parent_uri()
        && let Some(post_id) = local_post_id(conn, parent)?
    {
        refs.push(Reference::Reply { post_id });
    }
    if let Some(quoted) = ing.quote_uri()
        && let Some(post_id) = local_post_id(conn, quoted)?
    {
        refs.push(Reference::Quote { post_id });
    }
    Ok(refs)
}

/// The local content id an AT-URI maps to, as the `PostId` a reference names
/// (the dag-cbor CID over the stored post's blake3 digest — the same
/// construction the nostr translator's `synthetic_cid` uses for a resolved
/// parent). A row whose id is not 32 hex bytes maps to nothing.
fn local_post_id(
    conn: &rusqlite::Connection,
    at_uri: &str,
) -> anyhow::Result<Option<fauna_core::data::PostId>> {
    let Some(hex_id) = db_helpers::get_post_id_for_at_uri(conn, at_uri)? else {
        return Ok(None);
    };
    let Ok(bytes) = hex::decode(&hex_id) else {
        return Ok(None);
    };
    let Ok(digest) = <[u8; 32]>::try_from(bytes.as_slice()) else {
        return Ok(None);
    };
    Ok(Some(fauna_cbor::Cid::from_digest_dag_cbor(digest)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::AppState;
    use fauna_bridge_atproto::ingest::synthetic_actor_id;
    use fauna_bridge_atproto::outbound::{QuoteRef, ReplyRefs};
    use fauna_bridge_atproto::reverse_translate::IntermediatePost;
    use std::sync::Arc;

    async fn test_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        crate::bluesky::init_db(&db).await.unwrap();
        Arc::new(AppState::for_test(db))
    }

    fn now_micros() -> u64 {
        fauna_core::data::Timestamp::now().0
    }

    /// One post as the shared translator would hand it over: text, a DID, a
    /// CID, dated a minute ago.
    fn ingestable(uri: &str, did: &str, text: &str) -> IngestablePost {
        IngestablePost {
            at_uri: uri.to_string(),
            cid: format!("bafy-{}", uri.rsplit('/').next().unwrap()),
            author_did: did.to_string(),
            author_handle: "someone.test".to_string(),
            author_display_name: None,
            author_avatar: None,
            record: IntermediatePost {
                text: text.to_string(),
                created_at_micros: now_micros() - 60_000_000,
                ..Default::default()
            },
            images: vec![],
            labels: vec![],
        }
    }

    fn reply_to(mut post: IngestablePost, parent_uri: &str) -> IngestablePost {
        post.record.reply = Some(ReplyRefs {
            parent_uri: parent_uri.to_string(),
            parent_cid: "bafy-parent".into(),
            root_uri: parent_uri.to_string(),
            root_cid: "bafy-parent".into(),
        });
        post
    }

    fn quoting(mut post: IngestablePost, quoted_uri: &str) -> IngestablePost {
        post.record.quote = Some(QuoteRef {
            uri: quoted_uri.to_string(),
            cid: "bafy-quoted".into(),
        });
        post
    }

    async fn stored_post(state: &AppState, post_id_hex: &str) -> fauna_core::data::Post {
        let id: [u8; 32] = hex::decode(post_id_hex).unwrap().try_into().unwrap();
        let body = crate::segments::post::load_post_body(&state.post_segments, &state.db, &id)
            .await
            .unwrap()
            .expect("the body rests in the author's segment");
        crate::db::posts::decode_stored_post(&body).expect("a decodable post")
    }

    /// The bridged-author transit point (`bridges.md` § Unified feed ingestion
    /// → *Bridged authors*): a page's author projects their face under the
    /// synthetic id, once per author, and a re-poll of an already-held window
    /// — which stores no post — still refreshes it.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_ingested_page_projects_each_authors_face_and_a_repoll_refreshes_it() {
        let state = test_state().await;
        let mut first = ingestable(
            "at://did:plc:alice/app.bsky.feed.post/1",
            "did:plc:alice",
            "one",
        );
        first.author_handle = "alice.bsky.social".into();
        first.author_display_name = Some("Alice".into());
        first.author_avatar = Some("/api/v1/bluesky/media?url=https%3A%2F%2Fcdn%2Fa.jpg".into());
        let mut second = ingestable(
            "at://did:plc:alice/app.bsky.feed.post/2",
            "did:plc:alice",
            "two",
        );
        second.author_handle = "alice.bsky.social".into();
        let page = vec![first, second];
        ingest_feed_posts(&state.db, &state.post_segments, &page)
            .await
            .unwrap();

        let id = synthetic_actor_id("did:plc:alice").0;
        {
            let conn = state.db.conn().await;
            let face = crate::db::bridge_authors::get(&conn, &id)
                .unwrap()
                .expect("the page projects the author's face")
                .display();
            assert_eq!(face.handle.as_deref(), Some("alice.bsky.social"));
            assert_eq!(face.display_name.as_deref(), Some("Alice"));
            assert_eq!(
                face.avatar_url.as_deref(),
                Some("/api/v1/bluesky/media?url=https%3A%2F%2Fcdn%2Fa.jpg"),
                "the first view of the author on the page wins within the page"
            );
        }

        // The author renamed themselves; the next poll of the same window
        // stores nothing, yet the face follows.
        let mut renamed = page[0].clone();
        renamed.author_display_name = Some("Alice, renamed".into());
        let again = ingest_feed_posts(&state.db, &state.post_segments, &[renamed])
            .await
            .unwrap();
        assert!(again.is_empty(), "the window was already held");
        let conn = state.db.conn().await;
        assert_eq!(
            crate::db::bridge_authors::get(&conn, &id)
                .unwrap()
                .unwrap()
                .display()
                .display_name
                .as_deref(),
            Some("Alice, renamed")
        );
    }

    /// The thing the feature turns on: a polled post rests as a `bluesky`-source
    /// content row under its synthetic author, with its map row carrying the
    /// CID — and a second pass over the same window stores nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_ingested_post_rests_with_source_bluesky_and_its_map_row_and_is_idempotent() {
        let state = test_state().await;
        let uri = "at://did:plc:alice/app.bsky.feed.post/1";
        let page = vec![ingestable(uri, "did:plc:alice", "hello from bluesky")];

        let stored = ingest_feed_posts(&state.db, &state.post_segments, &page)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        let id: [u8; 32] = hex::decode(&stored[0].post_id_hex)
            .unwrap()
            .try_into()
            .unwrap();

        assert_eq!(
            state.db.get_post_source(&id).await.unwrap().as_deref(),
            Some("bluesky"),
            "the content row carries the bridge's source token"
        );
        let post = stored_post(&state, &stored[0].post_id_hex).await;
        assert_eq!(post.author, synthetic_actor_id("did:plc:alice"));
        assert_eq!(post.body_text(), "hello from bluesky");

        {
            let conn = state.db.conn().await;
            assert_eq!(
                db_helpers::get_post_id_for_at_uri(&conn, uri)
                    .unwrap()
                    .as_deref(),
                Some(stored[0].post_id_hex.as_str())
            );
            assert_eq!(
                db_helpers::get_crosspost_uri_and_cid(&conn, &stored[0].post_id_hex)
                    .unwrap()
                    .unwrap(),
                (uri.to_string(), "bafy-1".to_string()),
                "the map row stores the CID the view served"
            );
        }

        // The feed projection: `query_feed` INNER JOINs `content_meta`, so a
        // post without that row is invisible to every feed.
        let feed = state
            .db
            .list_posts_by_author(&synthetic_actor_id("did:plc:alice").0)
            .await
            .unwrap();
        assert_eq!(feed.len(), 1, "the post is reachable by its author");

        let again = ingest_feed_posts(&state.db, &state.post_segments, &page)
            .await
            .unwrap();
        assert!(
            again.is_empty(),
            "a re-poll of the same window must store nothing: {again:?}"
        );
        let conn = state.db.conn().await;
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM bluesky_posts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        let content_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM content WHERE source = 'bluesky'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content_rows, 1);
    }

    /// The feed's text filters reach an ingested post through its Search-corpus
    /// row (`feed.md` § The read model → *The list-card preview* → Corollary):
    /// the map row carries the rested post's id.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_keyword_feed_lists_an_ingested_post() {
        use fauna_core::scoring::{FilterCombination, FilterRule};
        let state = test_state().await;
        // The corpus policy counts actors; a nest with none indexes nothing.
        state
            .db
            .create_user(&[7u8; 32], "free", "test")
            .await
            .unwrap();
        let page = vec![ingestable(
            "at://did:plc:alice/app.bsky.feed.post/kw",
            "did:plc:alice",
            "a bluesky loquat recipe",
        )];
        let stored = ingest_feed_posts(&state.db, &state.post_segments, &page)
            .await
            .unwrap();

        let matched: Vec<String> = state
            .db
            .query_feed(
                &[FilterRule::BodyContains {
                    terms: vec!["loquat".into()],
                }],
                FilterCombination::All,
                &[],
                None,
                50,
            )
            .await
            .unwrap()
            .into_iter()
            .map(|r| hex::encode(r.post_id))
            .collect();
        assert_eq!(matched, vec![stored[0].post_id_hex.clone()]);
    }

    /// Ruling 3: a parent the page carries (listed first by the shared
    /// translator) threads the reply; a parent the nest never saw leaves the
    /// reply top-level — never a synthetic id.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_carried_parent_threads_the_reply_and_an_unknown_one_rests_top_level() {
        let state = test_state().await;
        let parent_uri = "at://did:plc:alice/app.bsky.feed.post/parent";
        let quoted_uri = "at://did:plc:bob/app.bsky.feed.post/quoted";
        let page = vec![
            ingestable(parent_uri, "did:plc:alice", "the parent"),
            ingestable(quoted_uri, "did:plc:bob", "the quoted one"),
            quoting(
                reply_to(
                    ingestable(
                        "at://did:plc:carol/app.bsky.feed.post/reply",
                        "did:plc:carol",
                        "a reply",
                    ),
                    parent_uri,
                ),
                quoted_uri,
            ),
            reply_to(
                ingestable(
                    "at://did:plc:dave/app.bsky.feed.post/orphan",
                    "did:plc:dave",
                    "an orphan",
                ),
                "at://did:plc:alice/app.bsky.feed.post/never-seen",
            ),
        ];

        let stored = ingest_feed_posts(&state.db, &state.post_segments, &page)
            .await
            .unwrap();
        assert_eq!(stored.len(), 4);

        let parent_id: [u8; 32] = hex::decode(&stored[0].post_id_hex)
            .unwrap()
            .try_into()
            .unwrap();
        let quoted_id: [u8; 32] = hex::decode(&stored[1].post_id_hex)
            .unwrap()
            .try_into()
            .unwrap();

        let reply = stored_post(&state, &stored[2].post_id_hex).await;
        assert_eq!(
            reply.references,
            vec![
                Reference::Reply {
                    post_id: fauna_cbor::Cid::from_digest_dag_cbor(parent_id)
                },
                Reference::Quote {
                    post_id: fauna_cbor::Cid::from_digest_dag_cbor(quoted_id)
                },
            ],
            "both targets rest locally, so both references resolve"
        );
        assert!(reply.is_reply());

        let orphan = stored_post(&state, &stored[3].post_id_hex).await;
        assert!(
            orphan.references.is_empty(),
            "an unresolved parent threads nothing: {:?}",
            orphan.references
        );
        assert!(!orphan.is_reply());
    }

    /// Ruling 2, at the door it exists for: an ingested post makes the
    /// interact door's Bluesky arm reachable. `resolve_uri_and_cid` answers
    /// from the map row with no agent — Bluesky OAuth is unconfigured in this
    /// harness, so a network fallback would fail — and the `like` arm then
    /// fails one step on, at the unconfigured agent, never at the map.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_interact_doors_like_arm_resolves_an_ingested_post_without_the_network() {
        let state = test_state().await;
        let uri = "at://did:plc:alice/app.bsky.feed.post/liked";
        let stored = ingest_feed_posts(
            &state.db,
            &state.post_segments,
            &[ingestable(uri, "did:plc:alice", "like me")],
        )
        .await
        .unwrap();
        let post_id_hex = stored[0].post_id_hex.clone();
        let actor_hex = hex::encode([9u8; 32]);

        let resolved = db_helpers::resolve_uri_and_cid(&state, &post_id_hex)
            .await
            .unwrap()
            .expect("an ingested post resolves");
        assert_eq!(resolved, (uri.to_string(), "bafy-liked".to_string()));

        let err = crate::bluesky::interact_routes::route_unified_interaction(
            &state,
            &actor_hex,
            &post_id_hex,
            "like",
        )
        .await
        .expect_err("no Bluesky OAuth client is configured in this harness");
        assert_ne!(
            err.status,
            axum::http::StatusCode::NOT_FOUND,
            "the arm must get past the map — it used to answer `no Bluesky mapping` for \
             every post, because nothing wrote a bluesky-source row: {}",
            err.message
        );
        assert!(
            err.message.contains("Bluesky not configured"),
            "the arm fails at the agent, one step past the door: {}",
            err.message
        );
    }

    /// The bridged future bound: a post dated past the cushion is dropped
    /// before any write — no content row, no map row.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_future_dated_post_is_refused_before_any_write() {
        let state = test_state().await;
        let mut post = ingestable(
            "at://did:plc:alice/app.bsky.feed.post/future",
            "did:plc:alice",
            "later",
        );
        post.record.created_at_micros = now_micros() + 2 * 3_600_000_000;
        let stored = ingest_feed_posts(&state.db, &state.post_segments, &[post])
            .await
            .unwrap();
        assert!(stored.is_empty());
        let conn = state.db.conn().await;
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM bluesky_posts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }
}
