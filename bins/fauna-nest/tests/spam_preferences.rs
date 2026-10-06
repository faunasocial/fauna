//! Integration tests for the per-user spam preferences plane.
//!
//! The nest-side Bayesian classifier these tests once sat beside is gone: the
//! per-user spam model rests sealed and only a capability holder trains it
//! (`docs/goal/behavior/mail-spam.md` § Encrypted-mode interaction).

use fauna_nest::db::CacheDb;

#[tokio::test]
async fn spam_preferences_crud() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor = [3u8; 32];

    // Default preferences
    let prefs = db.get_spam_preferences(&actor).await.unwrap();
    assert!((prefs.spam_threshold - 0.5).abs() < f64::EPSILON);
    assert!((prefs.phishing_threshold - 0.3).abs() < f64::EPSILON);

    // Update
    let mut updated = prefs.clone();
    updated.spam_threshold = 0.3;
    updated.phishing_threshold = 0.7;
    db.upsert_spam_preferences(&actor, &updated).await.unwrap();

    // Read back
    let loaded = db.get_spam_preferences(&actor).await.unwrap();
    assert!((loaded.spam_threshold - 0.3).abs() < f64::EPSILON);
    assert!((loaded.phishing_threshold - 0.7).abs() < f64::EPSILON);
}

/// Moved from the retired `contact_labels_test.rs` (its other tests pinned the
/// deleted nest-side `ContactStatusClassifier`; this one pins the live
/// spam-preferences plane).
#[tokio::test]
async fn spam_threshold_default() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor = [10u8; 32];

    let prefs = db.get_spam_preferences(&actor).await.unwrap();
    assert!((prefs.spam_threshold - 0.5).abs() < f64::EPSILON);
    // Spam at 0.6 exceeds default threshold
    assert!(0.6 >= prefs.spam_threshold);
    // Spam at 0.3 does not
    assert!(0.3 < prefs.spam_threshold);
}
