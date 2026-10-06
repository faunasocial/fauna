//! Tests: registration database methods.

use fauna_nest::db::{CacheDb, InviteCodeGrant};
use std::sync::Arc;

/// What an ordinary code grants: a tier, no guardian, no band. The carry grew
/// from a `(tier, guardian)` tuple into [`InviteCodeGrant`] when the account
/// age band landed (`family-safety.md` § The account age band), so these
/// suites name the shape once rather than five times — a new carry field is
/// then one edit here, which is the whole point of the struct.
fn plain(tier: &str) -> Option<InviteCodeGrant> {
    Some(InviteCodeGrant {
        tier: tier.to_string(),
        guardian_actor: None,
        age_band: None,
    })
}

#[tokio::test]
async fn create_and_validate_invite_code() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_invite_code("TEST-CODE", "personal", 2)
        .await
        .unwrap();

    let tier = db.validate_invite_code("TEST-CODE").await.unwrap();
    assert_eq!(tier, plain("personal"));

    // Second use works
    let tier = db.validate_invite_code("TEST-CODE").await.unwrap();
    assert_eq!(tier, plain("personal"));

    // Third use exhausted
    let tier = db.validate_invite_code("TEST-CODE").await.unwrap();
    assert_eq!(tier, None);
}

#[tokio::test]
async fn invalid_invite_code_returns_none() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tier = db.validate_invite_code("NONEXISTENT").await.unwrap();
    assert_eq!(tier, None);
}

#[tokio::test]
async fn peek_invite_code_does_not_consume() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_invite_code("PEEK-CODE", "personal", 1)
        .await
        .unwrap();

    // Peek twice; both succeed because peek doesn't decrement.
    assert_eq!(
        db.peek_invite_code("PEEK-CODE").await.unwrap(),
        plain("personal"),
    );
    assert_eq!(
        db.peek_invite_code("PEEK-CODE").await.unwrap(),
        plain("personal"),
    );

    // Validate consumes the only use.
    assert_eq!(
        db.validate_invite_code("PEEK-CODE").await.unwrap(),
        plain("personal"),
    );

    // Now peek must report exhausted.
    assert_eq!(db.peek_invite_code("PEEK-CODE").await.unwrap(), None);

    // Unknown code: None.
    assert_eq!(db.peek_invite_code("NEVER-EXISTED").await.unwrap(), None);
}

#[tokio::test]
async fn handle_cooldown_blocks_reuse() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let actor_a = [1u8; 32];
    let actor_b = [2u8; 32];
    db.create_user(&actor_a, "free", "").await.unwrap();
    db.set_handle(&actor_a, "alice").await.unwrap();

    // Release handle with cooldown
    db.release_handle_with_cooldown(&actor_a, "alice")
        .await
        .unwrap();

    // Same actor can reclaim
    assert!(db.check_handle_cooldown("alice", &actor_a).await.unwrap());
    // Different actor blocked
    assert!(!db.check_handle_cooldown("alice", &actor_b).await.unwrap());
}

#[tokio::test]
async fn count_users_by_tier() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_user(&[1u8; 32], "free", "").await.unwrap();
    db.create_user(&[2u8; 32], "free", "").await.unwrap();
    db.create_user(&[3u8; 32], "personal", "").await.unwrap();

    let count = db.count_users_by_tier("free").await.unwrap();
    assert_eq!(count, 2);

    let count = db.count_users_by_tier("personal").await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn actor_already_registered() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let actor = [1u8; 32];
    assert!(!db.is_actor_registered(&actor).await.unwrap());
    db.create_user(&actor, "free", "").await.unwrap();
    assert!(db.is_actor_registered(&actor).await.unwrap());
}

#[tokio::test]
async fn create_user_with_handle_atomic() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let actor = [1u8; 32];

    // Atomic creation succeeds
    db.create_user_with_handle(&actor, "free", "alice", None)
        .await
        .unwrap();
    let resolved = db.resolve_handle("alice").await.unwrap();
    assert_eq!(resolved, Some(actor));

    // Duplicate handle fails without creating user
    let actor2 = [2u8; 32];
    assert!(
        db.create_user_with_handle(&actor2, "free", "alice", None)
            .await
            .is_err()
    );
    assert!(!db.is_actor_registered(&actor2).await.unwrap());
}
