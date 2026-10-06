//! Integration tests for admin role management (Task 5).

use fauna_nest::db::CacheDb;
use fauna_nest::db::admin::RosterWrite;

/// After adding an admin via add_admin_actor + set_admin_role("superadmin"),
/// get_admin_role returns "superadmin".
#[tokio::test]
async fn default_admin_role_is_superadmin() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor: [u8; 32] = [1u8; 32];

    db.add_admin_actor(&actor).await.unwrap();
    // The column default is 'superadmin', but claim.rs also calls set_admin_role explicitly.
    // Simulate that explicit call here.
    db.set_admin_role(&actor, "superadmin").await.unwrap();

    let role = db.get_admin_role(&actor).await.unwrap();
    assert_eq!(role.as_deref(), Some("superadmin"));
}

/// get_admin_role returns None for a non-admin actor.
#[tokio::test]
async fn get_admin_role_returns_none_for_non_admin() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor: [u8; 32] = [2u8; 32];

    let role = db.get_admin_role(&actor).await.unwrap();
    assert!(role.is_none(), "non-admin actor should return None");
}

/// After set_admin_role("moderator"), get_admin_role returns "moderator".
/// (A peer superadmin keeps the demotion floor-legal — admin.md § 2.)
#[tokio::test]
async fn set_and_get_admin_role() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor: [u8; 32] = [3u8; 32];
    let peer: [u8; 32] = [4u8; 32];

    db.add_admin_actor(&peer).await.unwrap();
    db.add_admin_actor(&actor).await.unwrap();
    db.set_admin_role(&actor, "moderator").await.unwrap();

    let role = db.get_admin_role(&actor).await.unwrap();
    assert_eq!(role.as_deref(), Some("moderator"));
}

/// Add 3 admins, set one to moderator; verify counts by role.
#[tokio::test]
async fn admin_count_by_role() {
    let db = CacheDb::open_in_memory().unwrap();

    let actor1: [u8; 32] = [10u8; 32];
    let actor2: [u8; 32] = [11u8; 32];
    let actor3: [u8; 32] = [12u8; 32];

    db.add_admin_actor(&actor1).await.unwrap();
    db.set_admin_role(&actor1, "superadmin").await.unwrap();

    db.add_admin_actor(&actor2).await.unwrap();
    db.set_admin_role(&actor2, "superadmin").await.unwrap();

    db.add_admin_actor(&actor3).await.unwrap();
    db.set_admin_role(&actor3, "moderator").await.unwrap();

    let superadmin_count = db.admin_count_by_role("superadmin").await.unwrap();
    let moderator_count = db.admin_count_by_role("moderator").await.unwrap();
    let operator_count = db.admin_count_by_role("operator").await.unwrap();

    assert_eq!(superadmin_count, 2, "should have 2 superadmins");
    assert_eq!(moderator_count, 1, "should have 1 moderator");
    assert_eq!(operator_count, 0, "should have 0 operators");
}

/// With only 1 superadmin, can_remove_admin refuses that superadmin.
#[tokio::test]
async fn cannot_remove_last_superadmin() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor: [u8; 32] = [20u8; 32];

    db.add_admin_actor(&actor).await.unwrap();
    db.set_admin_role(&actor, "superadmin").await.unwrap();

    let can_remove = db.can_remove_admin(&actor).await.unwrap();
    assert!(
        !can_remove,
        "should not be able to remove the last superadmin"
    );
}

/// With 2 superadmins, either may be removed.
#[tokio::test]
async fn can_remove_superadmin_with_two() {
    let db = CacheDb::open_in_memory().unwrap();

    let actor1: [u8; 32] = [21u8; 32];
    let actor2: [u8; 32] = [22u8; 32];

    db.add_admin_actor(&actor1).await.unwrap();
    db.set_admin_role(&actor1, "superadmin").await.unwrap();

    db.add_admin_actor(&actor2).await.unwrap();
    db.set_admin_role(&actor2, "superadmin").await.unwrap();

    let can_remove = db.can_remove_admin(&actor1).await.unwrap();
    assert!(
        can_remove,
        "should be able to remove a superadmin when 2 exist"
    );
}

/// The door read is target-aware: a moderator beside a sole
/// superadmin is removable — only the last SUPERADMIN is floor-protected.
#[tokio::test]
async fn a_moderator_is_removable_beside_a_sole_superadmin() {
    let db = CacheDb::open_in_memory().unwrap();

    let superadmin: [u8; 32] = [23u8; 32];
    let moderator: [u8; 32] = [24u8; 32];
    db.add_admin_actor(&superadmin).await.unwrap();
    db.add_admin_actor(&moderator).await.unwrap();
    db.set_admin_role(&moderator, "moderator").await.unwrap();

    assert!(
        db.can_remove_admin(&moderator).await.unwrap(),
        "a non-superadmin admin never threatens the superadmin floor"
    );
    assert!(
        !db.can_remove_admin(&superadmin).await.unwrap(),
        "the sole superadmin stays floor-protected"
    );
}

/// The floor binds at the WRITER: deleting the last superadmin is
/// refused atomically with the write; a peer makes it legal.
#[tokio::test]
async fn remove_admin_actor_refuses_the_last_superadmin() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor: [u8; 32] = [25u8; 32];

    db.add_admin_actor(&actor).await.unwrap();
    assert_eq!(
        db.remove_admin_actor(&actor).await.unwrap(),
        RosterWrite::RefusedLastSuperadmin
    );
    assert!(
        db.is_admin(&actor).await.unwrap(),
        "the refusal must not delete"
    );

    let peer: [u8; 32] = [26u8; 32];
    db.add_admin_actor(&peer).await.unwrap();
    assert_eq!(
        db.remove_admin_actor(&actor).await.unwrap(),
        RosterWrite::Applied
    );
    assert!(!db.is_admin(&actor).await.unwrap());
}

/// Demoting the last superadmin is refused at the writer; with a peer the
/// demotion lands. Promotions are never floor-checked.
#[tokio::test]
async fn set_admin_role_refuses_demoting_the_last_superadmin() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor: [u8; 32] = [27u8; 32];

    db.add_admin_actor(&actor).await.unwrap();
    assert_eq!(
        db.set_admin_role(&actor, "moderator").await.unwrap(),
        RosterWrite::RefusedLastSuperadmin
    );
    assert_eq!(
        db.get_admin_role(&actor).await.unwrap().as_deref(),
        Some("superadmin"),
        "the refusal must not write"
    );

    let peer: [u8; 32] = [28u8; 32];
    db.add_admin_actor(&peer).await.unwrap();
    assert_eq!(
        db.set_admin_role(&actor, "moderator").await.unwrap(),
        RosterWrite::Applied
    );
    assert_eq!(
        db.get_admin_role(&actor).await.unwrap().as_deref(),
        Some("moderator")
    );

    // A stranger's role change mints nothing — set_admin_role is an UPDATE.
    let stranger: [u8; 32] = [29u8; 32];
    assert_eq!(
        db.set_admin_role(&stranger, "moderator").await.unwrap(),
        RosterWrite::NotAnAdmin
    );
    assert!(db.get_admin_role(&stranger).await.unwrap().is_none());
}

/// A succession MOVES an admin's role row to the successor WITH its role
/// (`succession-aftermath.md` § Re-key scope, "future authority" moves). The
/// re-INSERT used to name only `(actor_id, added_at)`, so the column default
/// (`'superadmin'`) applied and a moderator's succession silently promoted the
/// successor to the tier the floor guards protect — a floor-adjacent hole the
/// 2026-08-14 floor change recorded as unreachable-today (the add door mints
/// only superadmins) and the 2026-08-26 verify-back of that change closed
/// rather than carried.
#[tokio::test]
async fn a_succession_moves_the_admin_role_row_without_changing_its_role() {
    let db = CacheDb::open_in_memory().unwrap();
    let superadmin: [u8; 32] = [30u8; 32];
    let moderator: [u8; 32] = [31u8; 32];
    let successor: [u8; 32] = [32u8; 32];

    db.create_user_with_handle(&moderator, "free", "mod", None)
        .await
        .unwrap();
    db.add_admin_actor(&superadmin).await.unwrap();
    db.add_admin_actor(&moderator).await.unwrap();
    assert_eq!(
        db.set_admin_role(&moderator, "moderator").await.unwrap(),
        RosterWrite::Applied
    );

    db.record_succession(&moderator, &successor, b"statement", 1)
        .await
        .unwrap()
        .expect("succession applies");

    assert!(
        db.get_admin_role(&moderator).await.unwrap().is_none(),
        "the retired identity keeps no admin row"
    );
    assert_eq!(
        db.get_admin_role(&successor).await.unwrap().as_deref(),
        Some("moderator"),
        "the successor inherits the moderator's OWN role — never the column \
         default: a succession is not a promotion"
    );
    assert_eq!(
        db.admin_count_by_role("superadmin").await.unwrap(),
        1,
        "the superadmin tier is exactly as it was before the succession"
    );
}
