#![cfg(feature = "nostr")]
//! The nostr **inbound sweep plane's** lifecycle — ingest, NIP-09 deletion,
//! NIP-40 expiry — and the Search-corpus hook those arms unblock.
//!
//! Goal docs: `docs/goal/behavior/content-index.md` § Bridge content in the
//! Search corpus (remainder **(2b)** — the sweep was deliberately left
//! unindexed until it had a removal arm) and `docs/goal/ui/feed.md` § State &
//! data shape → *Post deletion* (the teardown a removal owes).
//!
//! tier_3: real `fauna-nest` code over a real SQLite database. The ingest under
//! test is the **production** function the worker calls
//! (`inbound_lifecycle::ingest_translated_event`), not a re-implementation, and
//! the search assertions go through `CacheDb::search_with_scoping` — what
//! `fauna.search.query`'s handler calls — so a row these tests find is a row
//! the Search page shows.
//!
//! **What "removed" is asserted against.** The withdrawal tests assert on
//! `content` itself — the surface `fauna.posts.get` reads via
//! `db::content::get_content` and `db::content::list_authored_posts_page` pages — because
//! that is where a swept post's bytes rest, so a row still present there is a
//! row the removal failed to reach.
//!
//! Until 2026-08-03 this note carried a second half: that `query_feed` was
//! *unusable* as an assertion surface, because the sweep wrote no
//! `content_meta` row and `query_feed` INNER JOINs it — so an empty feed proved
//! nothing. That gap is now closed (the sweep writes the projection beside the
//! content row), and the two tests at the end of this file assert the feed
//! directly: a swept post reaches the feed the `nostr-inbound-to-feed` setting
//! names, and it sorts by its real timestamp among natively-authored posts.
//!
//! **Where the body rests.** A swept post's bytes rest in its author's `__post`
//! segment, with the `content` row a projection carrying an empty payload
//! (`feed.md` § State & data shape → The read model), so every test sweeps
//! through a real on-disk `SegmentManager` and
//! `a_swept_post_rests_in_its_authors_segment_with_its_expiry` pins the shape.

mod common;

use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::{Event, Tag, UnsignedEvent};
use fauna_core::scoring::FilterCombination;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_search;
use fauna_nest::nostr::{db as nostr_db, inbound_lifecycle, store};
use fauna_segment_store::SegmentManager;

const ACTOR: [u8; 32] = [7u8; 32];

fn note(kp: &Keypair, created_at: u64, content: &str) -> Event {
    kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at,
        kind: 1,
        tags: vec![],
        content: content.to_string(),
    })
}

fn deletion(kp: &Keypair, tags: Vec<Tag>) -> Event {
    kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at: 1_700_000_500,
        kind: 5,
        tags,
        content: String::new(),
    })
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// Hits `fauna.search.query` serves, restricted to the nostr bridge corpus.
async fn bridge_hits(db: &CacheDb, q: &str) -> usize {
    db.search_with_scoping(q, &ACTOR, Some("bridge.nostr"), None, None, 50, 0)
        .await
        .expect("search")
        .len()
}

/// Whether the swept post still rests in `content` — the surface
/// `fauna.posts.get` reads (see this file's header on why not `query_feed`).
async fn content_present(db: &CacheDb, post_id_hex: &str) -> bool {
    let digest = fauna_core::hex32::decode(post_id_hex).expect("post id hex");
    let conn = db.conn().await;
    fauna_nest::db::content::get_content(&conn, &digest)
        .expect("get content")
        .is_some()
}

/// A real on-disk `__post` segment manager — the worker's `post_segments` —
/// in a tempdir the caller keeps alive for the test.
fn post_segments() -> (tempfile::TempDir, SegmentManager) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mgr = SegmentManager::new(dir.path().join("post-segments"), "post");
    (dir, mgr)
}

/// Drive the production ingest and return the swept post's id.
async fn sweep_in(db: &CacheDb, segs: &SegmentManager, event: &Event) -> String {
    inbound_lifecycle::ingest_translated_event(db, segs, event)
        .await
        .expect("ingest")
        .expect("the event should have been swept in")
}

/// The post ids `query_feed` serves, newest first — the surface
/// `fauna.feed.posts` reads, so a row these tests find is a row the Feed page
/// shows.
async fn feed_ids(db: &CacheDb) -> Vec<String> {
    db.query_feed(&[], FilterCombination::All, &[], None, 50)
        .await
        .expect("query feed")
        .into_iter()
        .map(|r| hex::encode(r.post_id))
        .collect()
}

/// Ingest a natively-authored post through the real `put_post` path — the
/// writer that runs `extract_post_metadata` → `write_post_index`, i.e. the
/// micros-and-projection reference every other writer is measured against.
/// `created_at` is **microseconds**, per `fauna_core::data::Timestamp`.
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

#[tokio::test]
async fn a_swept_post_from_a_followed_author_appears_in_the_feed() {
    // `nostr-inbound-to-feed` is default-ON and user-visible on all 7 apps, and
    // `nostr.md` § Goal calls this path "inbound follow-and-ingest … into feed".
    // The sweep must therefore write the feed projection, not just a `content`
    // row: `query_feed` INNER JOINs `content_meta` (`feeds.rs`), so a swept post
    // with no meta row is invisible to every feed no matter what else is right.
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    let event = note(&kp, 1_700_000_000, "a rhubarb crumble recipe");

    let post_id = sweep_in(&db, &segs, &event).await;

    assert!(
        feed_ids(&db).await.contains(&post_id),
        "a followed author's swept post must reach the feed the setting names"
    );
}

#[tokio::test]
async fn a_swept_post_and_a_native_post_of_the_same_moment_sort_adjacently() {
    // The unit pin. `content.created_at` is epoch **microseconds** — the unit
    // `fauna_core::data::Timestamp` declares, `feed.md` § The read model states
    // for the chronological ordering, and `search.rs`'s before/after window
    // compares against. A writer storing seconds (or millis) puts its rows at
    // ~1970 relative to everyone else's, so they sort to the bottom of every
    // mixed listing rather than into their true position.
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();

    // Three posts, one wall-clock second apart: native, swept, native.
    // Correct ordering is strictly by time, whoever wrote the row.
    let older = native_post(&db, "the older native post", 1_700_000_000_000_000).await;
    let swept = sweep_in(&db, &segs, &note(&kp, 1_700_000_001, "the swept post")).await;
    let newer = native_post(&db, "the newer native post", 1_700_000_002_000_000).await;

    assert_eq!(
        feed_ids(&db).await,
        vec![newer, swept, older],
        "a swept post must sort by its real timestamp, between the two native \
         posts it falls between — not below both as a seconds-valued row does"
    );
}

#[tokio::test]
async fn the_sweep_indexes_what_it_ingests() {
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    let event = note(&kp, 1_700_000_000, "a rhubarb crumble recipe");

    let post_id = sweep_in(&db, &segs, &event).await;

    // Both halves of a transit point: the post rests, and it is searchable —
    // works out of the box, nobody configured anything.
    assert!(content_present(&db, &post_id).await);
    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);
}

#[tokio::test]
async fn a_followed_authors_nip09_delete_withdraws_the_swept_post() {
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    let event = note(&kp, 1_700_000_000, "rhubarb crumble");
    let post_id = sweep_in(&db, &segs, &event).await;
    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);

    let removed = inbound_lifecycle::apply_inbound_deletion(
        &db,
        &deletion(&kp, vec![Tag::new(vec!["e".into(), event.id.clone()])]),
    )
    .await;

    assert_eq!(removed, 1, "the author's own delete should have applied");
    assert!(
        !content_present(&db, &post_id).await,
        "the translated post must go, not just the search row"
    );
    assert_eq!(bridge_hits(&db, "rhubarb").await, 0);
}

#[tokio::test]
async fn a_nip09_delete_naming_another_authors_event_is_ignored() {
    // NIP-09's own rule, and the guard that keeps one followed author from
    // retracting another's content out of this nest.
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let author = Keypair::generate();
    let stranger = Keypair::generate();
    let event = note(&author, 1_700_000_000, "rhubarb crumble");
    let post_id = sweep_in(&db, &segs, &event).await;

    let removed = inbound_lifecycle::apply_inbound_deletion(
        &db,
        &deletion(
            &stranger,
            vec![Tag::new(vec!["e".into(), event.id.clone()])],
        ),
    )
    .await;

    assert_eq!(removed, 0);
    assert!(content_present(&db, &post_id).await);
    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);
}

#[tokio::test]
async fn an_inbound_delete_cannot_reach_a_locally_authored_outbound_row() {
    // The destructive-verb ownership guard, and the reason the lookups scope to
    // `direction = 'inbound'`: a local user's own post is mapped OUTBOUND when
    // it is materialized onto the relay, and `fauna.posts.delete` (three author
    // checks) must stay the only verb that can destroy it. Constructed with the
    // deleter's OWN pubkey on the outbound row, so the author-scope clause
    // alone would let it through — this pins the direction clause specifically.
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    let event = note(&kp, 1_700_000_000, "rhubarb crumble");
    let post_id = sweep_in(&db, &segs, &event).await;

    {
        let conn = db.conn().await;
        // Retire the inbound row and re-file the same event as OUR OWN
        // materialization, leaving the content row in place.
        nostr_db::delete_inbound_event_map(&conn, &post_id, &event.id).unwrap();
        nostr_db::insert_event_map(&conn, &post_id, &event.id, &kp.public_key_hex(), "outbound")
            .unwrap();
    }

    let removed = inbound_lifecycle::apply_inbound_deletion(
        &db,
        &deletion(&kp, vec![Tag::new(vec!["e".into(), event.id.clone()])]),
    )
    .await;

    assert_eq!(removed, 0, "an outbound row is not remotely retractable");
    assert!(content_present(&db, &post_id).await);
}

#[tokio::test]
async fn a_nip09_a_tag_withdraws_an_addressable_swept_post() {
    // Four of the kinds the inbound subscription asks for are addressable
    // (34550, 30402, 30311, 30009) and their authors delete them by
    // coordinate, not by event id — so an `e`-only arm would silently no-op on
    // exactly the long-form/classified/live content this nest swept.
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    let event = kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at: 1_700_000_000,
        kind: 30402,
        tags: vec![
            Tag::new(vec!["d".into(), "listing-1".into()]),
            Tag::new(vec!["title".into(), "rhubarb plants".into()]),
        ],
        content: "rhubarb plants for sale".into(),
    });
    let post_id = sweep_in(&db, &segs, &event).await;

    let coord = store::replace_key(&event).expect("an addressable kind has a coordinate");
    let removed = inbound_lifecycle::apply_inbound_deletion(
        &db,
        &deletion(&kp, vec![Tag::new(vec!["a".into(), coord])]),
    )
    .await;

    assert_eq!(removed, 1, "an `a` tag must resolve on the sweep plane");
    assert!(!content_present(&db, &post_id).await);
}

#[tokio::test]
async fn an_already_expired_inbound_event_is_never_stored() {
    // NIP-40 at ingest, mirroring the relay store: an expired event must not
    // become a resting row that only the next tick retracts.
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    let event = kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at: now_secs() - 100,
        kind: 1,
        tags: vec![Tag::new(vec![
            "expiration".into(),
            (now_secs() - 10).to_string(),
        ])],
        content: "rhubarb ephemeral".into(),
    });

    let outcome = inbound_lifecycle::ingest_translated_event(&db, &segs, &event)
        .await
        .expect("ingest");

    assert!(outcome.is_none(), "an expired event must not be stored");
    assert_eq!(bridge_hits(&db, "rhubarb").await, 0);
}

#[tokio::test]
async fn the_expiry_sweep_withdraws_a_swept_post_whose_expiration_passed() {
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    let event = kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at: now_secs(),
        kind: 1,
        tags: vec![Tag::new(vec![
            "expiration".into(),
            (now_secs() + 3600).to_string(),
        ])],
        content: "rhubarb ephemeral".into(),
    });
    let post_id = sweep_in(&db, &segs, &event).await;
    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);

    // A live expiration sweeps nothing — the negative half, so the test cannot
    // pass by the sweep simply deleting everything.
    assert_eq!(inbound_lifecycle::sweep_expired_inbound(&db).await, 0);
    assert!(content_present(&db, &post_id).await);

    {
        // Age the row past its expiration without waiting on the wall clock —
        // the deadline is data, so the test asserts state, never timing.
        let digest = fauna_core::hex32::decode(&post_id).unwrap();
        let conn = db.conn().await;
        conn.execute(
            "UPDATE content SET expires_at = ?1 WHERE id = ?2",
            rusqlite::params![(now_secs() - 10) as i64, digest.as_slice()],
        )
        .unwrap();
    }

    assert_eq!(inbound_lifecycle::sweep_expired_inbound(&db).await, 1);
    assert!(
        !content_present(&db, &post_id).await,
        "an expired swept post must leave `content`, not just the corpus"
    );
    assert_eq!(bridge_hits(&db, "rhubarb").await, 0);
}

#[tokio::test]
async fn a_swept_post_rests_in_its_authors_segment_with_its_expiry() {
    // `feed.md` § The read model: every decodable post rests in its author's
    // `__post` segment and is segment-backup-eligible; only an undecodable body
    // rests inline. A swept post decodes, so its body is in the segment, the
    // `content` row is a projection with an empty payload, and the NIP-40
    // expiration still reaches `content.expires_at` for the expiry sweep.
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    let expiration = now_secs() + 3600;
    let event = kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at: now_secs(),
        kind: 1,
        tags: vec![Tag::new(vec!["expiration".into(), expiration.to_string()])],
        content: "rhubarb resting place".into(),
    });
    let post_id = sweep_in(&db, &segs, &event).await;
    let digest = fauna_core::hex32::decode(&post_id).unwrap();

    let (payload, expires_at): (Vec<u8>, Option<i64>) = {
        let conn = db.conn().await;
        conn.query_row(
            "SELECT payload, expires_at FROM content WHERE id = ?1",
            rusqlite::params![digest.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the swept post's content row")
    };
    assert!(
        payload.is_empty(),
        "a swept post's body must rest in its segment, not inline in `content.payload`"
    );
    assert_eq!(expires_at, Some(expiration as i64));

    let body = fauna_nest::segments::post::read_body_by_post_id(&segs, &db, &digest)
        .await
        .expect("segment read")
        .expect("the body rests in the author's `__post` segment");
    assert_eq!(*blake3::hash(&body).as_bytes(), digest);
    let (scope, _seg) = fauna_nest::segments::post::lookup_scope_by_post_id(&db, &digest)
        .await
        .expect("scope lookup")
        .expect("a segment record");
    let post = fauna_nest::db::posts::decode_stored_post(&body).expect("decodes");
    assert_eq!(
        scope, post.author.0,
        "the segment scope is the post's author"
    );

    // The expiry sweep retracts the segment record too, not just the row.
    {
        let conn = db.conn().await;
        conn.execute(
            "UPDATE content SET expires_at = ?1 WHERE id = ?2",
            rusqlite::params![(now_secs() - 10) as i64, digest.as_slice()],
        )
        .unwrap();
    }
    assert_eq!(inbound_lifecycle::sweep_expired_inbound(&db).await, 1);
    assert!(
        fauna_nest::segments::post::load_post_body(&segs, &db, &digest)
            .await
            .expect("load")
            .is_none(),
        "an expired swept post must not stay servable from its segment"
    );
}

#[tokio::test]
async fn an_event_seen_by_both_nostr_planes_indexes_once() {
    // The two transit points key the corpus identically, so a followed author's
    // event that also lands in the relay store surfaces once, not twice.
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    let event = note(&kp, 1_700_000_000, "rhubarb crumble");

    sweep_in(&db, &segs, &event).await;
    {
        let conn = db.conn().await;
        store::store_event(&conn, &event, false).expect("store event");
    }

    assert_eq!(bridge_hits(&db, "rhubarb").await, 1);
}

#[tokio::test]
async fn a_toggled_off_corpus_still_gets_its_swept_post_withdrawn() {
    // Removal must not depend on the search policy: the projection teardown is
    // the user-data obligation, the corpus row is derived. With indexing OFF
    // there is no corpus row to drop, and the delete must still land.
    let db = common::nest_db(ACTOR).await;
    let (_seg_dir, segs) = post_segments();
    let kp = Keypair::generate();
    {
        let conn = db.conn().await;
        bridge_search::set_policy(
            &conn,
            &hex::encode(ACTOR),
            "nostr",
            bridge_search::SearchPolicy {
                show_in_search: false,
                post_limit: fauna_protocol::bridge_search_policy::DEFAULT_SEARCH_POST_LIMIT,
            },
        )
        .unwrap();
    }
    let event = note(&kp, 1_700_000_000, "rhubarb crumble");
    let post_id = sweep_in(&db, &segs, &event).await;
    assert_eq!(bridge_hits(&db, "rhubarb").await, 0, "indexing is off");
    assert!(content_present(&db, &post_id).await);

    let removed = inbound_lifecycle::apply_inbound_deletion(
        &db,
        &deletion(&kp, vec![Tag::new(vec!["e".into(), event.id.clone()])]),
    )
    .await;

    assert_eq!(removed, 1);
    assert!(!content_present(&db, &post_id).await);
}
