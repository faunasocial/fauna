//! Mailbox-export **reclaim** on the three lifecycle events no session RPC is
//! part of (`docs/goal/behavior/mail-export.md` § Reclaim, § Expiry): account
//! deletion, the orphan sweep that follows a succession burn, and the periodic
//! expiry tick.
//!
//! An export session is a DB row *and* a file — a sealed whole-mailbox snapshot
//! under `<data-dir>/exports/`. The actor-table registry deletes rows; nothing
//! it does can unlink. So "the account was deleted" has to be pinned on both
//! halves, and on the order between them: the file goes while its row still
//! says where it is, so that an unlink which fails leaves a deletion that can
//! be retried rather than a blob nothing can find. The expiry tick is held to
//! the same order.
//!
//! Every test plants its own fixture file at the path its row names, so these
//! pin the reclaim paths independently of the blob writer.
//!
//! Tier: tier_3 (real `AppState` + real `CacheDb` + a real tempdir data dir —
//! no mocks).

mod common;
use common::config_with_db_path;

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;

use fauna_nest::db::CacheDb;
use fauna_nest::db::mail_export::CreateExportSessionOutcome;
use fauna_nest::db::mail_policy::ExportCeilings;
use fauna_nest::mail_export_blobs::{
    ExpirySweep, reclaim_orphaned_export_blobs, run_export_expiry_tick, sweep_expired_export_blobs,
};
use fauna_nest::pending_actions::finalize_user_deletion;
use fauna_nest::routes::AppState;

const ALICE: [u8; 32] = [0xA1; 32];
const BOB: [u8; 32] = [0xB0; 32];

/// `AppState` over a tempdir-backed `db_path`, so the data dir the deletion
/// path derives (`db_path`'s parent) is one this test owns.
async fn fixture() -> (Arc<AppState>, TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    let db_path = data_dir
        .path()
        .join("nest.db")
        .to_string_lossy()
        .into_owned();
    let state = Arc::new(AppState {
        config: config_with_db_path(db_path),
        ..AppState::for_test(db)
    });
    std::fs::create_dir(data_dir.path().join("exports")).unwrap();
    (state, data_dir)
}

async fn user(db: &CacheDb, actor: &[u8; 32], handle: &str) {
    db.create_user_with_handle(actor, "free", handle, None)
        .await
        .unwrap();
}

/// Open a session for `actor` and plant the blob file its row names.
async fn session_with_blob(db: &CacheDb, data_dir: &Path, actor: &[u8; 32], id: &str) -> String {
    let blob_path = format!("exports/{id}.zip.zst");
    let outcome = db
        .create_export_session(
            actor,
            id,
            "mbox",
            b"scope",
            &blob_path,
            Some(b"wrapped-session-key"),
            &ExportCeilings::default(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, CreateExportSessionOutcome::Created);
    std::fs::write(data_dir.join(&blob_path), b"sealed whole-mailbox bytes").unwrap();
    blob_path
}

#[tokio::test]
async fn a_deleted_account_leaves_neither_export_row_nor_blob_file() {
    let (state, data_dir) = fixture().await;
    let dir = data_dir.path();
    user(&state.db, &ALICE, "alice").await;
    user(&state.db, &BOB, "bob").await;
    let alice_1 = session_with_blob(&state.db, dir, &ALICE, "a1").await;
    let alice_2 = session_with_blob(&state.db, dir, &ALICE, "a2").await;
    let bob_1 = session_with_blob(&state.db, dir, &BOB, "b1").await;

    finalize_user_deletion(&state, &ALICE).await.unwrap();

    assert!(
        state
            .db
            .export_blob_paths_for_actor(&ALICE)
            .await
            .unwrap()
            .is_empty(),
        "the deleted account's export_sessions rows must be purged"
    );
    for gone in [&alice_1, &alice_2] {
        assert!(
            !dir.join(gone).exists(),
            "{gone} is a sealed snapshot of a deleted user's whole mailbox and must not \
             survive the account"
        );
    }
    // The deletion is scoped to its actor on BOTH halves.
    assert_eq!(
        state.db.export_blob_paths_for_actor(&BOB).await.unwrap(),
        vec![bob_1.clone()]
    );
    assert!(
        dir.join(&bob_1).exists(),
        "another user's blob was unlinked"
    );
}

/// The ordering pin. If the rows went first, this deletion would "succeed" and
/// the blob would be unreachable for good; unlink-first means the failure is
/// loud, the row survives it, and the retry completes.
#[tokio::test]
async fn a_blob_that_will_not_unlink_fails_the_deletion_with_its_row_intact() {
    let (state, data_dir) = fixture().await;
    let dir = data_dir.path();
    user(&state.db, &ALICE, "alice").await;
    let blob_path = session_with_blob(&state.db, dir, &ALICE, "a1").await;
    // Make the unlink fail without touching permissions (which root ignores):
    // `remove_file` on a directory is an error on every platform.
    std::fs::remove_file(dir.join(&blob_path)).unwrap();
    std::fs::create_dir(dir.join(&blob_path)).unwrap();

    let err = finalize_user_deletion(&state, &ALICE)
        .await
        .expect_err("a blob that cannot be unlinked must fail the deletion");
    assert!(
        format!("{err:#}").contains(&blob_path),
        "the failure must name the blob: {err:#}"
    );
    assert_eq!(
        state.db.export_blob_paths_for_actor(&ALICE).await.unwrap(),
        vec![blob_path.clone()],
        "the row that names the stuck blob must outlive the failed pass — it is the only \
         record of where the blob is"
    );
    assert!(
        state.db.get_user(&ALICE).await.unwrap().is_some(),
        "the account must not be half-deleted past a blob it still owns"
    );

    // The executor retries a failed action every tick; once the obstacle is
    // gone the same call completes.
    std::fs::remove_dir(dir.join(&blob_path)).unwrap();
    finalize_user_deletion(&state, &ALICE).await.unwrap();
    assert!(
        state
            .db
            .export_blob_paths_for_actor(&ALICE)
            .await
            .unwrap()
            .is_empty()
    );
}

/// A stored path outside the minted `exports/<file>` shape is never turned
/// into an unlink — and does not make the account undeletable either.
#[tokio::test]
async fn a_blob_path_outside_the_export_directory_is_never_unlinked() {
    let (state, data_dir) = fixture().await;
    let dir = data_dir.path();
    user(&state.db, &ALICE, "alice").await;
    let bystander = dir.join("nest.db-bystander");
    std::fs::write(&bystander, b"not an export blob").unwrap();
    state
        .db
        .create_export_session(
            &ALICE,
            "evil",
            "mbox",
            b"",
            "exports/../nest.db-bystander",
            None,
            &ExportCeilings::default(),
        )
        .await
        .unwrap();

    finalize_user_deletion(&state, &ALICE).await.unwrap();

    assert!(
        bystander.exists(),
        "a traversal-shaped blob_path reached outside exports/"
    );
    assert!(
        state
            .db
            .export_blob_paths_for_actor(&ALICE)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The succession half. The ceremony BURNS the retired identity's rows inside
/// its transaction and can unlink nothing, so the files are collected by the
/// rule this test pins: a blob no row names is garbage, and one a row names is
/// not.
#[tokio::test]
async fn the_orphan_reclaim_unlinks_exactly_the_blobs_no_row_names() {
    let (state, data_dir) = fixture().await;
    let dir = data_dir.path();
    let live = session_with_blob(&state.db, dir, &BOB, "b1").await;
    let burned = session_with_blob(&state.db, dir, &ALICE, "a1").await;
    // What the burn leg does: the row goes, the file stays.
    assert!(
        state
            .db
            .delete_export_session(&ALICE, "a1", &burned)
            .await
            .unwrap()
    );
    // Not a file, so not a blob — the reclaim owns files only.
    std::fs::create_dir(dir.join("exports/subdir")).unwrap();

    assert_eq!(
        reclaim_orphaned_export_blobs(&state.db, dir).await.unwrap(),
        1
    );

    assert!(!dir.join(&burned).exists(), "the orphan must be reclaimed");
    assert!(
        dir.join(&live).exists(),
        "a blob a live session names must survive"
    );
    assert!(dir.join("exports/subdir").is_dir());
    // Idempotent, and a nest that never exported has no directory at all.
    assert_eq!(
        reclaim_orphaned_export_blobs(&state.db, dir).await.unwrap(),
        0
    );
    let never_exported = tempfile::tempdir().unwrap();
    assert_eq!(
        reclaim_orphaned_export_blobs(&state.db, never_exported.path())
            .await
            .unwrap(),
        0
    );
}

// ── § Expiry — the periodic tick ──────────────────────────────────────────

/// [`fixture`] over a real database file, so a test can age a session past its
/// 30 days through a second connection: the tick reads the clock, and nothing
/// a client can call sets `expires_at` into the past.
async fn fixture_on_disk() -> (Arc<AppState>, TempDir, String) {
    let data_dir = tempfile::tempdir().unwrap();
    let db_path = data_dir
        .path()
        .join("nest.db")
        .to_string_lossy()
        .into_owned();
    let db = Arc::new(CacheDb::open(&db_path).unwrap());
    let state = Arc::new(AppState {
        config: config_with_db_path(db_path.clone()),
        ..AppState::for_test(db)
    });
    std::fs::create_dir(data_dir.path().join("exports")).unwrap();
    (state, data_dir, db_path)
}

/// Age one session past its 30-day window.
fn expire(db_path: &str, session_id: &str) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let changed = conn
        .execute(
            "UPDATE export_sessions SET expires_at = 1 WHERE session_id = ?1",
            [session_id],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

/// One tick closes § Expiry's window for the user who exports once and never
/// comes back — row AND file — and collects a blob no row names, while every
/// live session, the same user's and another's, keeps both halves.
#[tokio::test]
async fn one_expiry_tick_reclaims_expired_and_unnamed_blobs_and_nothing_live() {
    let (state, data_dir, db_path) = fixture_on_disk().await;
    let dir = data_dir.path();
    let expired = session_with_blob(&state.db, dir, &ALICE, "a1").await;
    let alice_live = session_with_blob(&state.db, dir, &ALICE, "a2").await;
    let bob_live = session_with_blob(&state.db, dir, &BOB, "b1").await;
    expire(&db_path, "a1");
    // What a cold resume racing its own old-generation upload leaves behind.
    let unnamed = "exports/a2.zip.zst.sealed";
    std::fs::write(dir.join(unnamed), b"stale frame 0").unwrap();

    run_export_expiry_tick(&state.db, &db_path).await;

    assert!(!dir.join(&expired).exists(), "the expired blob must go");
    assert_eq!(
        state.db.export_blob_paths_for_actor(&ALICE).await.unwrap(),
        vec![alice_live.clone()],
        "…and its row after it, leaving the same user's live session"
    );
    assert!(!dir.join(unnamed).exists(), "a blob no row names must go");
    for live in [&alice_live, &bob_live] {
        assert!(dir.join(live).exists(), "{live} is live and must survive");
    }
    assert_eq!(
        state.db.export_blob_paths_for_actor(&BOB).await.unwrap(),
        vec![bob_live]
    );
}

/// § Reclaim rule 2 on the tick: an expired blob that will not unlink keeps
/// the row that names it, so the next tick retries it rather than the blob
/// becoming an orphan only a restart could find.
#[tokio::test]
async fn an_expired_blob_that_will_not_unlink_keeps_its_row_for_the_next_tick() {
    let (state, data_dir, db_path) = fixture_on_disk().await;
    let dir = data_dir.path();
    let blob_path = session_with_blob(&state.db, dir, &ALICE, "a1").await;
    expire(&db_path, "a1");
    // `remove_file` on a directory fails on every platform, as root too.
    std::fs::remove_file(dir.join(&blob_path)).unwrap();
    std::fs::create_dir(dir.join(&blob_path)).unwrap();

    assert_eq!(
        sweep_expired_export_blobs(&state.db, dir).await.unwrap(),
        ExpirySweep {
            reclaimed: 0,
            stuck: 1
        }
    );
    assert_eq!(
        state.db.export_blob_paths_for_actor(&ALICE).await.unwrap(),
        vec![blob_path.clone()],
        "the row is the only record of where the stuck blob is"
    );

    std::fs::remove_dir(dir.join(&blob_path)).unwrap();
    std::fs::write(dir.join(&blob_path), b"sealed whole-mailbox bytes").unwrap();
    assert_eq!(
        sweep_expired_export_blobs(&state.db, dir).await.unwrap(),
        ExpirySweep {
            reclaimed: 1,
            stuck: 0
        }
    );
    assert!(!dir.join(&blob_path).exists());
    assert!(
        state
            .db
            .export_blob_paths_for_actor(&ALICE)
            .await
            .unwrap()
            .is_empty()
    );
}
