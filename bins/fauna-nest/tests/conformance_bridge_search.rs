#![cfg(feature = "nostr")]
//! The uniform per-bridge Search-corpus policy, proven end-to-end through the
//! real serving path — `docs/goal/behavior/content-index.md` § Bridge content
//! in the Search corpus (ratified 2026-07-22, user-ruled).
//!
//! tier_3: real `fauna-nest` code over a real SQLite database, driving the
//! actual nostr relay store and the actual `fauna.search.query` storage call —
//! no stubbed backend, so a break anywhere between the transit point and the
//! search reply shows up here. What each test pins is the success criterion
//! verbatim: with show-in-search ON a foreign nostr event **surfaces in
//! `fauna.search.query` results** and **disappears** on NIP-09 delete,
//! toggle-off, and cap-eviction; OFF-by-toggle indexes nothing; the cap holds
//! newest-N.
//!
//! The searches here go through `CacheDb::search_with_scoping`, which is what
//! `fauna.search.query`'s handler calls (`storage/sealed.rs::SealedStorage::
//! search`) — so a bridge row that this file finds is a bridge row the Search
//! page shows, and the phase-1/phase-2 routing is exercised for real rather
//! than asserted.

mod common;

use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::{Event, Tag, UnsignedEvent};
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_search::{self, SearchPolicy};
use fauna_nest::nostr::store;
use fauna_protocol::bridge_search_policy::DEFAULT_SEARCH_POST_LIMIT;
use rusqlite::Connection;

const ACTOR: [u8; 32] = [7u8; 32];

fn signed(kp: &Keypair, created_at: u64, content: &str) -> Event {
    kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at,
        kind: 1,
        tags: vec![],
        content: content.to_string(),
    })
}

/// Store a foreign-authored event the way a transit point does: `derived=false`
/// (a genuinely received event, not a materialization of one of our own posts).
fn store_foreign(conn: &Connection, event: &Event) {
    let outcome = store::store_event(conn, event, false).expect("store event");
    assert!(
        outcome.is_newly_stored(),
        "fixture expected a fresh store, got {outcome:?}"
    );
}

/// Hits `fauna.search.query` serves for `q`, restricted to the nostr bridge
/// corpus — the exact `content_type` a Search-page bridge filter would send.
async fn bridge_hits(db: &CacheDb, q: &str) -> usize {
    db.search_with_scoping(q, &ACTOR, Some("bridge.nostr"), None, None, 50, 0)
        .await
        .expect("search")
        .len()
}

/// Hits for an UNFILTERED query — the default Search-page call. Proves bridge
/// rows reach the corpus a user actually searches, not just a filtered view.
async fn unfiltered_hits(db: &CacheDb, q: &str) -> usize {
    db.search_with_scoping(q, &ACTOR, None, None, None, 50, 0)
        .await
        .expect("search")
        .len()
}

#[tokio::test]
async fn on_by_default_a_foreign_event_surfaces_in_search() {
    let db = common::nest_db(ACTOR).await;
    let kp = Keypair::generate();
    let event = signed(&kp, 1_700_000_000, "a rhubarb crumble recipe");

    {
        let conn = db.conn().await;
        store_foreign(&conn, &event);
    }

    // Works out of the box: nobody configured anything, and the event is
    // searchable — both through a bridge-typed query and the default one.
    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);
    assert_eq!(unfiltered_hits(&db, "rhubarb").await, 1);
}

#[tokio::test]
async fn toggling_off_stops_indexing_and_purges_what_was_indexed() {
    let db = common::nest_db(ACTOR).await;
    let kp = Keypair::generate();
    let actor_hex = hex::encode(ACTOR);

    {
        let conn = db.conn().await;
        store_foreign(&conn, &signed(&kp, 1_700_000_000, "rhubarb one"));
    }
    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);

    // Toggle off: the resting corpus is purged at the setting write, not at the
    // next ingest — a user who turns it off sees it gone on their next search.
    {
        let conn = db.conn().await;
        bridge_search::set_policy(
            &conn,
            &actor_hex,
            "nostr",
            SearchPolicy {
                show_in_search: false,
                post_limit: DEFAULT_SEARCH_POST_LIMIT,
            },
        )
        .unwrap();
        bridge_search::reconcile_bridge_corpus(&conn, "nostr").unwrap();
    }
    assert_eq!(bridge_hits(&db, "rhubarb").await, 0);

    // And OFF means nothing new is indexed either.
    {
        let conn = db.conn().await;
        store_foreign(&conn, &signed(&kp, 1_700_000_100, "rhubarb two"));
    }
    assert_eq!(bridge_hits(&db, "rhubarb").await, 0);
}

#[tokio::test]
async fn a_nip09_delete_removes_the_indexed_row() {
    let db = common::nest_db(ACTOR).await;
    let kp = Keypair::generate();
    let event = signed(&kp, 1_700_000_000, "rhubarb crumble");

    {
        let conn = db.conn().await;
        store_foreign(&conn, &event);
    }
    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);

    // The author's own kind-5 deletion request, applied by the store — the
    // removal arm reaches `content_fts` through the trigger, with no
    // search-specific call anywhere on the deletion path.
    let deletion = kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at: 1_700_000_050,
        kind: 5,
        tags: vec![Tag::new(vec!["e".into(), event.id.clone()])],
        content: String::new(),
    });
    {
        let conn = db.conn().await;
        let removed = store::apply_deletion(&conn, &deletion).unwrap();
        assert_eq!(removed, 1, "the deletion should have removed the event");
    }

    assert_eq!(bridge_hits(&db, "rhubarb").await, 0);
}

#[tokio::test]
async fn a_nip40_expiry_sweep_removes_the_indexed_row() {
    // NIP-40 expiry is a BULK `DELETE FROM nostr_events` that passes through no
    // per-row Rust funnel — the case a hand-written removal hook would miss and
    // the reason the removal arm is a trigger.
    let db = common::nest_db(ACTOR).await;
    let kp = Keypair::generate();
    let now = fauna_core::data::Timestamp::now_secs() as u64;
    let event = kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at: now,
        kind: 1,
        tags: vec![Tag::new(vec!["expiration".into(), (now + 1).to_string()])],
        content: "rhubarb ephemeral".into(),
    });

    {
        let conn = db.conn().await;
        store_foreign(&conn, &event);
    }
    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);

    {
        let conn = db.conn().await;
        // Age the row past its expiration without waiting on the wall clock.
        conn.execute(
            "UPDATE nostr_events SET expiration = ?1 WHERE id = ?2",
            rusqlite::params![(now - 10) as i64, event.id],
        )
        .unwrap();
        let swept = store::sweep_expired(&conn).unwrap();
        assert_eq!(swept, 1);
    }

    assert_eq!(bridge_hits(&db, "rhubarb").await, 0);
}

#[tokio::test]
async fn the_cap_holds_the_newest_n() {
    let db = common::nest_db(ACTOR).await;
    let kp = Keypair::generate();
    let actor_hex = hex::encode(ACTOR);

    {
        let conn = db.conn().await;
        bridge_search::set_policy(
            &conn,
            &actor_hex,
            "nostr",
            SearchPolicy {
                show_in_search: true,
                post_limit: 2,
            },
        )
        .unwrap();

        // Five events, ascending in time; the cap is 2.
        for i in 0..5u64 {
            store_foreign(
                &conn,
                &signed(&kp, 1_700_000_000 + i, &format!("rhubarb number{i}")),
            );
        }
    }

    assert_eq!(
        bridge_hits(&db, "rhubarb").await,
        2,
        "the cap must bound the corpus at ingest"
    );
    // Newest-N, not oldest-N: the two most recent survive.
    assert_eq!(bridge_hits(&db, "number4").await, 1);
    assert_eq!(bridge_hits(&db, "number3").await, 1);
    assert_eq!(bridge_hits(&db, "number0").await, 0);
}

#[tokio::test]
async fn lowering_the_cap_prunes_the_resting_corpus() {
    let db = common::nest_db(ACTOR).await;
    let kp = Keypair::generate();
    let actor_hex = hex::encode(ACTOR);

    {
        let conn = db.conn().await;
        for i in 0..4u64 {
            store_foreign(
                &conn,
                &signed(&kp, 1_700_000_000 + i, &format!("rhubarb number{i}")),
            );
        }
    }
    assert_eq!(bridge_hits(&db, "rhubarb").await, 4);

    {
        let conn = db.conn().await;
        bridge_search::set_policy(
            &conn,
            &actor_hex,
            "nostr",
            SearchPolicy {
                show_in_search: true,
                post_limit: 1,
            },
        )
        .unwrap();
        bridge_search::reconcile_bridge_corpus(&conn, "nostr").unwrap();
    }

    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);
    assert_eq!(bridge_hits(&db, "number3").await, 1, "the newest survives");
}

#[tokio::test]
async fn a_derived_row_never_enters_the_corpus() {
    // The no-double-surfacing rule, structurally: a materialization of one of
    // our own Fauna posts is already in `content_fts` as that post, so it must
    // not also enter as a bridge row.
    let db = common::nest_db(ACTOR).await;
    let kp = Keypair::generate();
    let event = signed(&kp, 1_700_000_000, "rhubarb materialized");

    {
        let conn = db.conn().await;
        let outcome = store::store_event(&conn, &event, true).unwrap();
        assert!(outcome.is_newly_stored());
    }

    assert_eq!(bridge_hits(&db, "rhubarb").await, 0);
}

#[tokio::test]
async fn dm_carriers_never_enter_the_corpus() {
    // The class split: a bridge's PRIVATE content is never eligible for
    // `content_fts`. Both DM carriers are excluded at index time, so the search
    // plane is structurally incapable of holding DM payloads.
    let db = common::nest_db(ACTOR).await;
    let kp = Keypair::generate();

    for kind in [4u64, 1059] {
        let event = kp.sign_event(UnsignedEvent {
            pubkey: kp.public_key_bytes(),
            created_at: 1_700_000_000 + kind,
            kind,
            tags: vec![Tag::new(vec!["p".into(), hex::encode([9u8; 32])])],
            content: format!("rhubarb secret {kind}"),
        });
        let conn = db.conn().await;
        let _ = store::store_event(&conn, &event, false);
    }

    assert_eq!(bridge_hits(&db, "rhubarb").await, 0);
}

#[tokio::test]
async fn a_replacement_leaves_exactly_one_indexed_row() {
    // NIP-01 replacement deletes the superseded row and inserts the new one;
    // the corpus must follow both halves, not accumulate.
    let db = common::nest_db(ACTOR).await;
    let kp = Keypair::generate();

    let first = kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at: 1_700_000_000,
        kind: 30_023, // parameterized-replaceable
        tags: vec![Tag::new(vec!["d".into(), "post-1".into()])],
        content: "rhubarb draft".into(),
    });
    let second = kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at: 1_700_000_100,
        kind: 30_023,
        tags: vec![Tag::new(vec!["d".into(), "post-1".into()])],
        content: "rhubarb final".into(),
    });

    {
        let conn = db.conn().await;
        store_foreign(&conn, &first);
        let outcome = store::store_event(&conn, &second, false).unwrap();
        assert!(matches!(outcome, store::StoreOutcome::Replaced));
    }

    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);
    assert_eq!(bridge_hits(&db, "final").await, 1);
    assert_eq!(bridge_hits(&db, "draft").await, 0);
}

// ── The feed's text filters over bridged posts ───────────────────────
//
// `docs/goal/ui/feed.md` § The read model → *The list-card preview* →
// **Corollary**: `BodyContains` / `BodyExcludes` (and the feed page's search,
// which `feed_routes::search_to_filter` folds into a MANDATORY `BodyContains`)
// reach a bridged post through its Search-corpus row, which
// `bridge_index_map.post_id` links back to the post. So the filters follow the
// corpus policy exactly: while the row stands the post matches; show-in-search
// OFF or past the cap, there is no text to match. No new removal code keeps
// that true — `purge_bridge_content` (toggle-off) and `trim_and_prune` →
// `prune_orphan_index_map` (the cap) drop the map row with the corpus row, as
// do the nostr store/sweep triggers on deletion; these tests pin the outcome.
//
// The posts are swept in through the PRODUCTION nostr sweep
// (`inbound_lifecycle::ingest_translated_event`), the transit point that rests
// a bridged `content` row; the filters run through `CacheDb::query_feed`, what
// `fauna.feed.posts` reads.

use fauna_core::scoring::{FilterCombination, FilterRule};
use fauna_nest::nostr::inbound_lifecycle;
use fauna_segment_store::SegmentManager;

fn post_segments() -> (tempfile::TempDir, SegmentManager) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mgr = SegmentManager::new(dir.path().join("post-segments"), "post");
    (dir, mgr)
}

/// Rest a foreign nostr note as a bridged POST through the sweep, returning the
/// post id (hex).
async fn sweep_in(db: &CacheDb, segs: &SegmentManager, event: &Event) -> String {
    inbound_lifecycle::ingest_translated_event(db, segs, event)
        .await
        .expect("ingest")
        .expect("the event should have been swept in")
}

/// A natively authored post through the real `put_post` writer (its body is a
/// `post/%` corpus row of its own).
async fn native_post(db: &CacheDb, body: &str, created_at_micros: u64) -> String {
    let kp = fauna_core::identity::ActorKeypair::generate();
    let post = fauna_core::data::Post {
        author: kp.actor_id(),
        created_at: fauna_core::data::Timestamp(created_at_micros),
        body: fauna_core::data::PostBody::Text {
            content: body.into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let cid = fauna_core::encoding::compute_post_id(&post).unwrap();
    let digest: [u8; 32] = cid.as_bytes()[4..].try_into().unwrap();
    let bytes = fauna_core::encoding::sign_and_pack(&kp, &post).unwrap();
    db.put_post(&digest, &bytes, None).await.unwrap();
    hex::encode(digest)
}

fn contains(term: &str) -> FilterRule {
    FilterRule::BodyContains {
        terms: vec![term.into()],
    }
}

fn excludes(term: &str) -> FilterRule {
    FilterRule::BodyExcludes {
        terms: vec![term.into()],
    }
}

fn ids(rows: Vec<fauna_nest::db::FeedPostRow>) -> Vec<String> {
    rows.into_iter().map(|r| hex::encode(r.post_id)).collect()
}

/// Post ids a feed with these OWN rules serves.
async fn feed_with(db: &CacheDb, rules: &[FilterRule]) -> Vec<String> {
    ids(db
        .query_feed(rules, FilterCombination::All, &[], None, 50)
        .await
        .expect("query feed"))
}

/// Post ids the feed page's search serves: the term as a MANDATORY
/// `BodyContains`, the shape `feed_routes::search_to_filter` builds.
async fn feed_search(db: &CacheDb, term: &str) -> Vec<String> {
    ids(db
        .query_feed(&[], FilterCombination::All, &[contains(term)], None, 50)
        .await
        .expect("query feed"))
}

async fn turn_off(db: &CacheDb) {
    let conn = db.conn().await;
    bridge_search::set_policy(
        &conn,
        &hex::encode(ACTOR),
        "nostr",
        SearchPolicy {
            show_in_search: false,
            post_limit: DEFAULT_SEARCH_POST_LIMIT,
        },
    )
    .unwrap();
    bridge_search::reconcile_bridge_corpus(&conn, "nostr").unwrap();
}

#[tokio::test]
async fn a_bridged_post_matches_body_contains_and_feed_search_while_indexed() {
    let db = common::nest_db(ACTOR).await;
    let (_d, segs) = post_segments();
    let kp = Keypair::generate();
    let post = sweep_in(&db, &segs, &signed(&kp, 1_700_000_000, "a rhubarb crumble")).await;

    assert_eq!(
        feed_with(&db, &[contains("rhubarb")]).await,
        vec![post.clone()],
        "a keyword feed must list the bridged post its corpus row matches"
    );
    assert_eq!(
        feed_search(&db, "rhubarb").await,
        vec![post.clone()],
        "the feed page's search is the same filter, in the mandatory group"
    );
    assert!(
        !feed_with(&db, &[excludes("rhubarb")]).await.contains(&post),
        "BodyExcludes drops a bridged post whose corpus row matches"
    );
    assert!(
        feed_with(&db, &[contains("gooseberry")]).await.is_empty(),
        "a non-matching term lists nothing"
    );
}

#[tokio::test]
async fn show_in_search_off_means_no_match_and_excludes_no_longer_excludes() {
    let db = common::nest_db(ACTOR).await;
    let (_d, segs) = post_segments();
    let kp = Keypair::generate();
    let post = sweep_in(&db, &segs, &signed(&kp, 1_700_000_000, "a rhubarb crumble")).await;
    assert_eq!(
        feed_with(&db, &[contains("rhubarb")]).await,
        vec![post.clone()]
    );

    turn_off(&db).await;

    assert!(
        feed_with(&db, &[contains("rhubarb")]).await.is_empty(),
        "with the corpus row purged there is no text to match"
    );
    assert!(feed_search(&db, "rhubarb").await.is_empty());
    assert!(
        feed_with(&db, &[excludes("rhubarb")]).await.contains(&post),
        "the same absence of text means BodyExcludes no longer excludes it — \
         the filter reads the corpus, and the corpus is the user's choice"
    );
}

#[tokio::test]
async fn a_post_evicted_past_the_cap_no_longer_matches() {
    let db = common::nest_db(ACTOR).await;
    let (_d, segs) = post_segments();
    let kp = Keypair::generate();
    {
        let conn = db.conn().await;
        bridge_search::set_policy(
            &conn,
            &hex::encode(ACTOR),
            "nostr",
            SearchPolicy {
                show_in_search: true,
                post_limit: 1,
            },
        )
        .unwrap();
    }
    let old = sweep_in(&db, &segs, &signed(&kp, 1_700_000_000, "rhubarb older")).await;
    let new = sweep_in(&db, &segs, &signed(&kp, 1_700_000_100, "rhubarb newer")).await;

    assert_eq!(
        feed_with(&db, &[contains("rhubarb")]).await,
        vec![new],
        "only the newest-N corpus rows stand, so only their posts match"
    );
    assert!(
        feed_with(&db, &[]).await.contains(&old),
        "the evicted post itself still rests in the feed"
    );
}

#[tokio::test]
async fn the_relay_store_seeing_a_swept_event_keeps_its_post_link() {
    // Both nostr transit points key the corpus identically, so an event seen by
    // both indexes once. The relay store rests no post (it passes no post id);
    // its upsert must not erase the link the sweep wrote, in either order.
    let db = common::nest_db(ACTOR).await;
    let (_d, segs) = post_segments();
    let kp = Keypair::generate();

    let first = signed(&kp, 1_700_000_000, "rhubarb sweep first");
    let a = sweep_in(&db, &segs, &first).await;
    {
        let conn = db.conn().await;
        store_foreign(&conn, &first);
    }

    let second = signed(&kp, 1_700_000_100, "rhubarb relay first");
    {
        let conn = db.conn().await;
        store_foreign(&conn, &second);
    }
    let b = sweep_in(&db, &segs, &second).await;

    let mut got = feed_with(&db, &[contains("rhubarb")]).await;
    got.sort();
    let mut want = vec![a, b];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(
        bridge_hits(&db, "rhubarb").await,
        2,
        "and still one corpus row each"
    );
}

#[tokio::test]
async fn a_native_posts_text_filters_are_unchanged() {
    let db = common::nest_db(ACTOR).await;
    let (_d, segs) = post_segments();
    let kp = Keypair::generate();
    let native = native_post(&db, "a native rhubarb tart", 1_700_000_000_000_000).await;
    let bridged = sweep_in(
        &db,
        &segs,
        &signed(&kp, 1_700_000_100, "a bridged damson jam"),
    )
    .await;

    assert_eq!(
        feed_with(&db, &[contains("rhubarb")]).await,
        vec![native.clone()]
    );
    assert_eq!(
        feed_with(&db, &[contains("damson")]).await,
        vec![bridged.clone()]
    );
    let excluded = feed_with(&db, &[excludes("rhubarb")]).await;
    assert!(!excluded.contains(&native) && excluded.contains(&bridged));

    // A native post is untouched by the bridge policy: OFF purges only the
    // bridge corpus.
    turn_off(&db).await;
    assert_eq!(feed_with(&db, &[contains("rhubarb")]).await, vec![native]);
}
