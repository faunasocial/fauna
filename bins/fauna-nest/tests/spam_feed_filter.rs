//! Integration tests for default spam filter on discovery feeds.
//! Verifies that LabelBelow { "spam", 0.5 } correctly suppresses spam-labeled
//! content while passing clean content through.

use fauna_core::scoring::{FilterCombination, FilterRule};
use fauna_nest::db::CacheDb;

#[tokio::test]
async fn spam_labeled_post_excluded_from_feed() {
    let db = CacheDb::open_in_memory().unwrap();

    let author = [1u8; 32];
    let now = 1_000_000i64;

    // Insert a post that will be labeled as spam
    let post_id = [0xAAu8; 32];
    db.insert_post_index_entry(&post_id, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();

    // Label it as spam with high confidence (0.8 > threshold 0.5)
    let pid_hex = hex::encode(post_id);
    db.upsert_content_label(
        "post", &pid_hex, "spam", 0.8, 1, &[2u8; 32], 1, 0, None, None, now, &[3u8; 32], &[0u8; 64],
    )
    .await
    .unwrap();

    // Query with spam filter — should exclude the spam post
    let rules = vec![FilterRule::LabelBelow {
        category: "spam".into(),
        max_confidence_permille: 500,
    }];
    let results = db
        .query_feed(&rules, FilterCombination::All, &[], None, 50)
        .await
        .unwrap();

    assert!(
        results
            .iter()
            .all(|r| r.post_id.as_slice() != post_id.as_slice()),
        "spam-labeled post should be excluded from feed"
    );
}

#[tokio::test]
async fn clean_post_included_with_spam_filter() {
    let db = CacheDb::open_in_memory().unwrap();

    let author = [1u8; 32];
    let now = 1_000_000i64;

    // Insert a clean post (no spam label)
    let post_id = [0xBBu8; 32];
    db.insert_post_index_entry(&post_id, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();

    // Query with spam filter — clean post should still appear
    let rules = vec![FilterRule::LabelBelow {
        category: "spam".into(),
        max_confidence_permille: 500,
    }];
    let results = db
        .query_feed(&rules, FilterCombination::All, &[], None, 50)
        .await
        .unwrap();

    assert!(
        results
            .iter()
            .any(|r| r.post_id.as_slice() == post_id.as_slice()),
        "clean post should be included when spam filter is active"
    );
}

// ── The mandatory group reads only attributed rows ───────────────────────────
//
// The guard above is pushed onto the ALWAYS-AND group of every feed read on the
// nest (`feed_routes::ensure_spam_filter`) unless the feed sets its own spam
// rule, so its input decides what every reader of every feed may see. A label
// that names no writer must not carry that weight: `fauna.labels.attach` was
// gated on the caller's CLASS alone, so any member could write `spam: 1000`
// about any member's post and take it out of every feed, silently and with no
// remedy — the nest-wide removal `moderation.md` § Categories & enforcement
// item 1 forbids the nest to perform on anyone's opinion. The door now stamps
// its writer; these three pin what the READ does with the distinction,
// including the deliberate carve-out for a feed's own rules.

/// Seed one post and label it `spam` 0.8, stamping `scanner_id` with the given
/// writer id. Returns the post id.
async fn post_labelled_spam_by(db: &CacheDb, post_id: [u8; 32], scanner_id: &[u8]) -> [u8; 32] {
    let author = [1u8; 32];
    let now = 1_000_000i64;
    db.insert_post_index_entry(&post_id, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();
    db.upsert_content_label(
        "post",
        &hex::encode(post_id),
        "spam",
        0.8,
        1,
        &[2u8; 32],
        1,
        0,
        None,
        None,
        now,
        scanner_id,
        &[0u8; 64],
    )
    .await
    .unwrap();
    post_id
}

fn spam_guard() -> Vec<FilterRule> {
    vec![FilterRule::LabelBelow {
        category: "spam".into(),
        max_confidence_permille: 500,
    }]
}

/// **The live arm of the finding.** A row naming no writer — every row the
/// class-only attach door ever wrote — no longer removes a post from every feed
/// on the nest.
#[tokio::test]
async fn an_unattributed_spam_label_does_not_suppress_through_the_mandatory_guard() {
    let db = CacheDb::open_in_memory().unwrap();
    let post_id = post_labelled_spam_by(&db, [0xC1u8; 32], &[0u8; 32]).await;

    let results = db
        .query_feed(&[], FilterCombination::All, &spam_guard(), None, 50)
        .await
        .unwrap();

    assert!(
        results
            .iter()
            .any(|r| r.post_id.as_slice() == post_id.as_slice()),
        "a label that names no writer must not remove a post from every feed"
    );
}

/// …and the guard is **not blinded**: a label that does name its writer still
/// suppresses, exactly as before. Without this, "stop trusting the row" would be
/// indistinguishable from "delete the spam guard".
#[tokio::test]
async fn an_attributed_spam_label_still_suppresses_through_the_mandatory_guard() {
    let db = CacheDb::open_in_memory().unwrap();
    let post_id = post_labelled_spam_by(&db, [0xC2u8; 32], &[0x9Au8; 32]).await;

    let results = db
        .query_feed(&[], FilterCombination::All, &spam_guard(), None, 50)
        .await
        .unwrap();

    assert!(
        results
            .iter()
            .all(|r| r.post_id.as_slice() != post_id.as_slice()),
        "an attributed spam verdict still suppresses"
    );
}

/// The carve-out, pinned: a feed's **own** label rules are the user's filter
/// over the user's own view (`moderation.md` § Categories & enforcement item 1's
/// user-side plane), so they keep reading whatever the plane holds — attribution
/// is a requirement of verdicts a reader did not choose, not of the reader's own.
#[tokio::test]
async fn a_feeds_own_spam_rule_still_reads_an_unattributed_row() {
    let db = CacheDb::open_in_memory().unwrap();
    let post_id = post_labelled_spam_by(&db, [0xC3u8; 32], &[0u8; 32]).await;

    let results = db
        .query_feed(&spam_guard(), FilterCombination::All, &[], None, 50)
        .await
        .unwrap();

    assert!(
        results
            .iter()
            .all(|r| r.post_id.as_slice() != post_id.as_slice()),
        "the user's own filter still aims at the whole plane"
    );
}

/// The badge half of the same rule. `FeedPostRow.labels` — the per-row
/// projection the feed card's `content-label-badge` paints for every reader —
/// is fed by the one function the nest-as-publisher fold also reads
/// (`db::feeds::project_content_labels`), so an unattributed row must vanish
/// from the badge exactly as it vanishes from the fold. The fold half was
/// mutation-verified; this is the badge half, measured on the same clause:
/// remove the provenance predicate from that projection and the unattributed
/// post grows a badge here.
#[tokio::test]
async fn an_unattributed_label_never_reaches_the_feed_badge() {
    let db = CacheDb::open_in_memory().unwrap();
    let unattributed = post_labelled_spam_by(&db, [0xC4u8; 32], &[0u8; 32]).await;
    let attributed = post_labelled_spam_by(&db, [0xC5u8; 32], &[3u8; 32]).await;

    // No rules at all: both posts are served, and only the projection decides
    // what each row's badge shows.
    let results = db
        .query_feed(&[], FilterCombination::All, &[], None, 50)
        .await
        .unwrap();

    let labels_of = |post_id: [u8; 32]| {
        results
            .iter()
            .find(|r| r.post_id.as_slice() == post_id.as_slice())
            .unwrap_or_else(|| panic!("post {} is served", hex::encode(post_id)))
            .labels
            .clone()
    };
    assert!(
        labels_of(unattributed).is_empty(),
        "a row naming no writer paints no badge for anyone"
    );
    let attributed_labels = labels_of(attributed);
    assert_eq!(
        attributed_labels.len(),
        1,
        "the attributed row still paints"
    );
    assert_eq!(attributed_labels[0].category, "spam");
    assert_eq!(attributed_labels[0].confidence_per_mille, 800);
}
