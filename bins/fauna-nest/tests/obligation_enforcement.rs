//! Integration tests for obligation enforcement and quarantine.

use fauna_nest::db::CacheDb;

#[tokio::test]
async fn obligation_action_record_crud() {
    let db = CacheDb::open_in_memory().unwrap();

    let post_id_hex = "dd".repeat(32);
    let obligation_id = [5u8; 32];
    let now = 1000000i64;

    // Insert an action record
    let id = db
        .insert_obligation_action(
            "post",
            &post_id_hex,
            "", // author_hex
            &obligation_id,
            0, // rule_index
            "spam",
            0.85,
            1, // Quarantine
            None,
            now,
            &[0u8; 64],
        )
        .await
        .unwrap();
    assert!(id > 0);

    // Retrieve it
    let actions = db
        .get_obligation_actions("post", &post_id_hex)
        .await
        .unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].category, "spam");
    assert_eq!(actions[0].action_taken, 1); // Quarantine
}

#[tokio::test]
async fn quarantine_flag_excludes_from_feeds() {
    use fauna_core::scoring::FilterCombination;

    let db = CacheDb::open_in_memory().unwrap();

    let post_a = [0xAAu8; 32];
    let post_b = [0xBBu8; 32];
    let author = [1u8; 32];
    let now = 1000000i64;

    db.insert_post_index_entry(&post_a, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&post_b, &author, now + 1, false, false, "fauna", &[])
        .await
        .unwrap();

    // Quarantine post_a
    db.set_post_quarantined(&post_a, true).await.unwrap();

    // Feed should only return post_b
    let results = db
        .query_feed(&[], FilterCombination::All, &[], None, 100)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &post_b);
}

#[tokio::test]
async fn suppressed_flag_excludes_from_feeds() {
    use fauna_core::scoring::FilterCombination;

    let db = CacheDb::open_in_memory().unwrap();

    let post_a = [0xCCu8; 32];
    let post_b = [0xDDu8; 32];
    let author = [1u8; 32];
    let now = 1000000i64;

    db.insert_post_index_entry(&post_a, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&post_b, &author, now + 1, false, false, "fauna", &[])
        .await
        .unwrap();

    // Suppress post_a
    db.set_post_suppressed(&post_a, true).await.unwrap();

    let results = db
        .query_feed(&[], FilterCombination::All, &[], None, 100)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &post_b);
}

/// A post taken down under a legal obligation is excluded from feeds (moderation.md
/// § Categories & enforcement item 1 — withheld everywhere it is served); a
/// restore (clearing the flag) brings it back.
#[tokio::test]
async fn legal_takedown_flag_excludes_from_feeds() {
    use fauna_core::scoring::FilterCombination;

    let db = CacheDb::open_in_memory().unwrap();

    let post_a = [0xEEu8; 32];
    let post_b = [0xFFu8; 32];
    let author = [1u8; 32];
    let now = 1000000i64;

    db.insert_post_index_entry(&post_a, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&post_b, &author, now + 1, false, false, "fauna", &[])
        .await
        .unwrap();

    // Take down post_a under a legal obligation.
    db.set_post_legal_takedown(&post_a, Some("EU-DSA-2024/12345"))
        .await
        .unwrap();

    let results = db
        .query_feed(&[], FilterCombination::All, &[], None, 100)
        .await
        .unwrap();
    assert_eq!(
        results.len(),
        1,
        "the taken-down post is excluded from feeds"
    );
    assert_eq!(results[0].post_id.as_slice(), &post_b);

    // Restore (overturned appeal) brings it back into feeds.
    db.set_post_legal_takedown(&post_a, None).await.unwrap();
    let results = db
        .query_feed(&[], FilterCombination::All, &[], None, 100)
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "restoring re-includes the post in feeds");
}

/// The **author-scoped** feed read withholds every moderation flag, not just the
/// takedown (`moderation.md` § Legal takedown → *Posts*). "Author-scoped" names
/// the filter, not the audience: its one caller is the federation feed query,
/// which answers a PEER nest with each candidate's metadata and the references
/// extracted from its body. Until 2026-09-10 it carried the takedown arm alone,
/// so a peer naming an author was handed that author's quarantined and
/// suppressed posts. One post per arm, so reverting any one arm reddens exactly
/// its own assertion.
#[tokio::test]
async fn the_author_scoped_feed_read_withholds_every_moderation_flag() {
    use fauna_core::scoring::FilterCombination;

    let db = CacheDb::open_in_memory().unwrap();
    let author = [2u8; 32];
    let clean = [0x61u8; 32];
    let taken = [0x62u8; 32];
    let quarantined = [0x63u8; 32];
    let suppressed = [0x64u8; 32];
    for (i, post) in [clean, taken, quarantined, suppressed].iter().enumerate() {
        db.insert_post_index_entry(
            post,
            &author,
            1_000_000 + i as i64,
            false,
            false,
            "fauna",
            &[],
        )
        .await
        .unwrap();
    }
    db.set_post_legal_takedown(&taken, Some("EU-DSA-2024/921"))
        .await
        .unwrap();
    db.set_post_quarantined(&quarantined, true).await.unwrap();
    db.set_post_suppressed(&suppressed, true).await.unwrap();

    let served: Vec<Vec<u8>> = db
        .query_feed_for_authors(
            &[],
            FilterCombination::All,
            &[],
            &[author.to_vec()],
            None,
            100,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.post_id)
        .collect();
    assert!(served.contains(&clean.to_vec()), "control: {served:?}");
    assert!(
        !served.contains(&taken.to_vec()),
        "a TAKEN-DOWN post must not reach a peer's author-scoped query"
    );
    assert!(
        !served.contains(&quarantined.to_vec()),
        "a QUARANTINED post must not reach a peer's author-scoped query"
    );
    assert!(
        !served.contains(&suppressed.to_vec()),
        "a SUPPRESSED post must not reach a peer's author-scoped query"
    );
}

#[tokio::test]
async fn quarantine_check_method() {
    let db = CacheDb::open_in_memory().unwrap();

    let post = [0xEEu8; 32];
    let author = [1u8; 32];

    db.insert_post_index_entry(&post, &author, 1000000, false, false, "fauna", &[])
        .await
        .unwrap();

    assert!(!db.is_post_quarantined(&post).await.unwrap());

    db.set_post_quarantined(&post, true).await.unwrap();
    assert!(db.is_post_quarantined(&post).await.unwrap());

    db.set_post_quarantined(&post, false).await.unwrap();
    assert!(!db.is_post_quarantined(&post).await.unwrap());
}

#[tokio::test]
async fn author_actions_query() {
    let db = CacheDb::open_in_memory().unwrap();

    let post_id = [0xFFu8; 32];
    let author = [1u8; 32];
    let now = 1000000i64;

    db.insert_post_index_entry(&post_id, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();

    let post_id_hex = hex::encode(post_id).to_lowercase();
    let author_hex = hex::encode(author).to_lowercase();
    db.insert_obligation_action(
        "post",
        &post_id_hex,
        &author_hex,
        &[5u8; 32],
        0,
        "spam",
        0.9,
        0, // Reject
        None,
        now,
        &[0u8; 64],
    )
    .await
    .unwrap();

    let actions = db
        .get_obligation_actions_for_author(&author_hex)
        .await
        .unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].category, "spam");
    assert_eq!(actions[0].action_taken, 0); // Reject
}
