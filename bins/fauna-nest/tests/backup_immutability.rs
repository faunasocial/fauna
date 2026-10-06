//! Tests for snapshot soft-delete lifecycle and hard floor minimum.

use fauna_nest::db::CacheDb;

/// After soft_delete_snapshot, fields are set correctly.
#[tokio::test]
async fn soft_delete_snapshot_sets_fields() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor: [u8; 32] = [1u8; 32];

    // Create folder and snapshot
    let fs_id = db.create_folder("test-set", &actor).await.unwrap();
    let snap = db.create_snapshot(fs_id).await.unwrap();
    assert!(
        !snap.soft_deleted,
        "snapshot should not be soft_deleted initially"
    );
    assert!(
        !snap.deletion_pending,
        "snapshot should not be deletion_pending initially"
    );
    assert!(
        snap.purge_after.is_none(),
        "purge_after should be None initially"
    );

    // Soft-delete it
    db.soft_delete_snapshot(snap.id).await.unwrap();

    // Verify
    let updated = db.get_snapshot(snap.id).await.unwrap().unwrap();
    assert!(
        updated.soft_deleted,
        "snapshot should be soft_deleted after soft_delete_snapshot"
    );
    assert!(
        !updated.deletion_pending,
        "deletion_pending should be cleared to 0"
    );
    assert!(
        updated.purge_after.is_some(),
        "purge_after should be set after soft_delete_snapshot"
    );
    // purge_after should be roughly now + 30 days (within a small tolerance)
    let now = fauna_core::data::Timestamp::now_secs();
    let purge_after = updated.purge_after.unwrap();
    let thirty_days = 30 * 24 * 3600i64;
    assert!(
        purge_after >= now + thirty_days - 5 && purge_after <= now + thirty_days + 5,
        "purge_after should be approximately 30 days from now, got {purge_after}"
    );
}

/// After undelete_snapshot, all deletion state is cleared.
#[tokio::test]
async fn undelete_snapshot_clears_soft_delete() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor: [u8; 32] = [2u8; 32];

    let fs_id = db.create_folder("test-set-2", &actor).await.unwrap();
    let snap = db.create_snapshot(fs_id).await.unwrap();

    // Soft-delete then undelete
    db.soft_delete_snapshot(snap.id).await.unwrap();

    // Confirm it is soft-deleted first
    let deleted = db.get_snapshot(snap.id).await.unwrap().unwrap();
    assert!(
        deleted.soft_deleted,
        "should be soft_deleted before undelete"
    );

    db.undelete_snapshot(snap.id).await.unwrap();

    let restored = db.get_snapshot(snap.id).await.unwrap().unwrap();
    assert!(
        !restored.soft_deleted,
        "soft_deleted should be cleared after undelete"
    );
    assert!(
        !restored.deletion_pending,
        "deletion_pending should be cleared after undelete"
    );
    assert!(
        restored.purge_after.is_none(),
        "purge_after should be NULL after undelete"
    );
}

/// With exactly 3 active snapshots, check_snapshot_delete_allowed returns false.
#[tokio::test]
async fn cannot_delete_below_hard_floor() {
    let db = CacheDb::open_in_memory().unwrap();
    let actor: [u8; 32] = [3u8; 32];

    let fs_id = db.create_folder("test-set-3", &actor).await.unwrap();

    // Insert 3 snapshots with distinct created_at values to avoid UNIQUE(folder_id, created_at)
    let base_ts = 1_700_000_000i64;
    db.insert_snapshot_at(fs_id, base_ts).await.unwrap();
    db.insert_snapshot_at(fs_id, base_ts + 1).await.unwrap();
    db.insert_snapshot_at(fs_id, base_ts + 2).await.unwrap();

    let active = db.count_active_snapshots(fs_id).await.unwrap();
    assert_eq!(active, 3, "should have exactly 3 active snapshots");

    let allowed = db.check_snapshot_delete_allowed(fs_id).await.unwrap();
    assert!(
        !allowed,
        "check_snapshot_delete_allowed should return false when active count == 3"
    );

    // With 4 snapshots, deletion should be allowed
    db.insert_snapshot_at(fs_id, base_ts + 3).await.unwrap();
    let allowed4 = db.check_snapshot_delete_allowed(fs_id).await.unwrap();
    assert!(
        allowed4,
        "check_snapshot_delete_allowed should return true when active count == 4"
    );
}
