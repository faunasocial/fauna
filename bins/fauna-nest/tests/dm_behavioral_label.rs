//! Tests for behavioral anomaly label production on DM delivery.

use fauna_nest::db::CacheDb;

/// Seed a sender that looks like an established, socially-connected actor:
/// a registered user with a mutual contact relationship to the recipient.
///
/// Without this, a fresh in-memory DB makes `get_behavioral_profile` report
/// `social_distance: None` and `account_age_days: 0` — i.e. a brand-new,
/// disconnected account — which the anomaly scorer (correctly) penalises by
/// +0.35 before any DM volume is considered. These tests want to isolate the
/// DM-fanout signal, so they start from a benign social baseline.
async fn seed_established_sender(db: &CacheDb, sender: &[u8; 32], recipient: &[u8; 32]) {
    db.create_user(sender, "free", "test").await.unwrap();
    db.accept_contact(sender, recipient).await.unwrap();
    db.accept_contact(recipient, sender).await.unwrap();
}

#[tokio::test]
async fn high_fanout_sender_gets_behavioral_label() {
    let db = CacheDb::open_in_memory().unwrap();
    let sender = [1u8; 32];
    let recipient = [99u8; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;
    seed_established_sender(&db, &sender, &recipient).await;

    // Simulate spammer: 25 DMs in last hour
    for i in 0..25u8 {
        let mut peer = [0u8; 32];
        peer[0] = i + 10;
        db.record_sender_event(&sender, "dm_sent", Some(&peer), now)
            .await
            .unwrap();
    }

    let profile = db
        .get_behavioral_profile(&sender, &recipient, now)
        .await
        .unwrap();
    let score = fauna_core::behavioral::compute_behavioral_anomaly(&profile);
    assert!(
        score >= 0.3,
        "25 DM recipients should score >= 0.3, got {score}"
    );
}

#[tokio::test]
async fn normal_sender_gets_no_behavioral_label() {
    let db = CacheDb::open_in_memory().unwrap();
    let sender = [1u8; 32];
    let recipient = [99u8; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;
    seed_established_sender(&db, &sender, &recipient).await;

    db.record_sender_event(&sender, "dm_sent", Some(&[2u8; 32]), now)
        .await
        .unwrap();
    db.record_sender_event(&sender, "dm_sent", Some(&[3u8; 32]), now)
        .await
        .unwrap();

    let profile = db
        .get_behavioral_profile(&sender, &recipient, now)
        .await
        .unwrap();
    let score = fauna_core::behavioral::compute_behavioral_anomaly(&profile);
    assert!(
        score < 0.3,
        "normal sender should score below the 0.3 label threshold, got {score}"
    );
}

/// The honest-user guard demands, at the level
/// that can express it.
///
/// Division of labour, deliberate: the *writer* is guarded by a flow test
/// (`conformance_conversations_channel.rs::a_reply_feeds_the_original_senders_dm_response_rate`)
/// which drives the real send path, because that finding was precisely a SELECT whose
/// writer never existed — a DB-level test would have stayed green throughout.
/// This test guards the *consequence*: given replies, an honest sender with
/// high 7-day fanout is not labelled. It needs backdated timestamps to isolate
/// the 7-day rule (a flow test stamps every send at `now`, which trips the 1 h
/// and 24 h rules too and drowns the signal under test).
#[tokio::test]
async fn honest_high_fanout_sender_whose_peers_replied_is_not_labelled() {
    let db = CacheDb::open_in_memory().unwrap();
    let sender = [1u8; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;
    const HOUR_US: i64 = 3_600_000_000;

    // 11 correspondents spread across the 7-day window — over the 7 d
    // threshold, under the 1 h and 24 h ones.
    let mut peers = Vec::new();
    for i in 0..11u8 {
        let mut peer = [0u8; 32];
        peer[0] = i + 10;
        let when = now - (i as i64 + 2) * 12 * HOUR_US;
        db.record_sender_event(&sender, "dm_sent", Some(&peer), when)
            .await
            .unwrap();
        // …and every one of them wrote back.
        db.record_dm_reply(&sender, &peer, when + HOUR_US)
            .await
            .unwrap();
        peers.push(peer);
    }
    seed_established_sender(&db, &sender, &peers[0]).await;

    let profile = db
        .get_behavioral_profile(&sender, &peers[0], now)
        .await
        .unwrap();
    assert_eq!(profile.unique_dm_recipients_7d, 11);
    assert!(
        (profile.dm_response_rate - 1.0).abs() < f64::EPSILON,
        "all 11 correspondents replied, expected rate 1.0, got {}",
        profile.dm_response_rate
    );
    let score = fauna_core::behavioral::compute_behavioral_anomaly(&profile);
    assert!(
        score < 0.3,
        "an honest sender whose correspondents all replied must stay below the \
         label threshold, got {score}"
    );
}

/// The same shape with no replies: the rule still bites what it was written for.
#[tokio::test]
async fn high_fanout_sender_whose_peers_never_replied_is_labelled() {
    let db = CacheDb::open_in_memory().unwrap();
    let sender = [1u8; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;
    const HOUR_US: i64 = 3_600_000_000;

    let mut peers = Vec::new();
    for i in 0..11u8 {
        let mut peer = [0u8; 32];
        peer[0] = i + 10;
        db.record_sender_event(
            &sender,
            "dm_sent",
            Some(&peer),
            now - (i as i64 + 2) * 12 * HOUR_US,
        )
        .await
        .unwrap();
        peers.push(peer);
    }
    seed_established_sender(&db, &sender, &peers[0]).await;

    let profile = db
        .get_behavioral_profile(&sender, &peers[0], now)
        .await
        .unwrap();
    let score = fauna_core::behavioral::compute_behavioral_anomaly(&profile);
    assert!(
        score >= 0.3,
        "unanswered high fanout is exactly what the rule is for, got {score}"
    );
}

/// The one-row-per-pair-per-window rule, asserted at **row** level.
///
/// Its sibling below asserts through `dm_response_rate`, which provably cannot
/// see this property: the reader counts `COUNT(DISTINCT target_actor)`, so
/// duplicate `dm_replied` rows are already collapsed before the rate is
/// computed. Deleting `record_dm_reply`'s whole `NOT EXISTS` clause therefore
/// leaves every rate-level test green (verified by mutation) — which makes the clause exactly the kind a later reader deletes as
/// dead weight, leaving the invariant resting on that single `COUNT(DISTINCT …)`.
/// This test is the pin that goes red instead.
#[tokio::test]
async fn a_repeated_reply_writes_one_row_per_correspondent_per_window() {
    let db = CacheDb::open_in_memory().unwrap();
    let a = [1u8; 32];
    let b = [2u8; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;

    db.record_sender_event(&a, "dm_sent", Some(&b), now)
        .await
        .unwrap();
    for _ in 0..5 {
        db.record_dm_reply(&a, &b, now).await.unwrap();
    }

    let profile = db.get_behavioral_profile(&a, &b, now).await.unwrap();
    // The rate is 1.0 either way — that is the whole point, and why the row
    // assertion below is the one that can fail.
    assert!(
        (profile.dm_response_rate - 1.0).abs() < f64::EPSILON,
        "five replies from one correspondent must collapse to one, got rate {}",
        profile.dm_response_rate
    );
    let replied_rows = db.count_dm_replied_rows(&a, &b, now).await.unwrap();
    assert_eq!(
        replied_rows, 1,
        "the NOT EXISTS guard must write exactly one dm_replied row per pair per window"
    );
}

/// A reply is only a reply if the other party messaged first, and it counts
/// once per correspondent per window — the two preconditions that keep
/// `dm_response_rate` a fraction rather than an inflatable counter.
#[tokio::test]
async fn a_reply_needs_a_prior_send_and_counts_once_per_correspondent() {
    let db = CacheDb::open_in_memory().unwrap();
    let a = [1u8; 32];
    let b = [2u8; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;

    // No prior outreach from A: B "replying" records nothing.
    db.record_dm_reply(&a, &b, now).await.unwrap();
    let profile = db.get_behavioral_profile(&a, &b, now).await.unwrap();
    assert_eq!(
        profile.dm_response_rate, 0.0,
        "a reply to a message that was never sent must not count"
    );

    // A messages B, B replies twice — still one reply row for the pair.
    db.record_sender_event(&a, "dm_sent", Some(&b), now)
        .await
        .unwrap();
    db.record_dm_reply(&a, &b, now).await.unwrap();
    db.record_dm_reply(&a, &b, now).await.unwrap();
    let profile = db.get_behavioral_profile(&a, &b, now).await.unwrap();
    assert!(
        (profile.dm_response_rate - 1.0).abs() < f64::EPSILON,
        "one correspondent, replied: rate 1.0, got {}",
        profile.dm_response_rate
    );
}
