//! The review list's "use the other version" over a group set's file
//! ([`SyncEngine::restore_retained_version`], decision 9 — `group-set.md`
//! § Build design — the group set) and the one disk it must never open: a
//! cloud-only placeholder (`on-demand-files.md` § On-Demand Files, *Nothing the
//! serve loop waits on holds up the root*).
//!
//! The restore is an engine command, and on an on-demand root the command arm
//! runs on the thread that serves the root
//! ([`crate::always_resident::answer_engine_command`]). On cfapi the sync
//! service is the provider, so its own read of its own placeholder is answered
//! by nobody (`crate::placeholder`): the restore's read of a freed file stalled
//! the command for cfapi's 60 s timeout and then failed with os error 426.
//!
//! Tier 1 (`architecture/testing.md` § The four-tier taxonomy): in-process, no
//! nest, no driver.

use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderRef;

use crate::db::{RetainedLoser, SyncState};
use crate::engine::SyncEngine;

const SCOPE: [u8; 32] = [0x5c; 32];

/// A group set's seat: the scope's ring, no nest leg — the only engine that
/// keeps retained versions.
fn group_seat(watch: &std::path::Path) -> SyncEngine {
    let mut engine = crate::download_file_bytes_test::build_test_engine(
        "http://127.0.0.1:9",
        watch.to_path_buf(),
        None,
        None,
        None,
        None,
    )
    .with_folder_ref(FolderRef::Group(SCOPE))
    .with_nest_leg_absent();
    engine.set_group_keys(Some(crate::group_seat_test::ring()));
    engine
}

/// The head every member holds at `rel`, settled on this seat (`Synced`, its
/// bytes the tracked local hash), and this replica's superseded edit retained
/// against it on the review list. Answers the review row's id.
async fn head_with_a_retained_loser(
    engine: &SyncEngine,
    watch: &std::path::Path,
    rel: &str,
    head: &[u8],
    retained: &[u8],
) -> i64 {
    std::fs::write(watch.join(rel), head).unwrap();
    engine.upload_file(rel).await.unwrap();
    let entry = engine.db().get_entry(rel).unwrap().unwrap();
    assert_eq!(entry.state, SyncState::Synced, "precondition: settled");
    assert_eq!(entry.local_hash, Some(ContentHash::of_raw(head)));

    let retained_manifest = ContentHash::of_raw(b"the retained edit's manifest");
    let target = engine.retained_version_path(&retained_manifest);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, retained).unwrap();
    engine
        .db()
        .record_retained_loser(&RetainedLoser {
            path: rel.to_string(),
            winner_author: Some(hex::encode([0xa1; 32])),
            winner_device: Some(hex::encode([0xd1; 16])),
            winner_created_at: 1_760_000_000_000,
            winner_manifest: entry.manifest_hash.map(|m| hex::encode(m.digest())),
            retained_manifest: hex::encode(retained_manifest.digest()),
            superseded: false,
        })
        .unwrap()
        .expect("a new review row")
}

/// Over a local file at the head the restore lands: the retained bytes are
/// written back as a local edit, minted as this seat's own row, and the review
/// row resolves. The ground the cloud-only refusal below stands on — the same
/// seat and row, with only the disk's attributes changed.
#[tokio::test]
async fn using_the_other_version_over_the_head_restores_it() {
    let watch = tempfile::tempdir().unwrap();
    let engine = group_seat(watch.path());
    let rel = "notes.txt";
    let retained = b"this replica's edit the group's later writer superseded\n";
    let id = head_with_a_retained_loser(
        &engine,
        watch.path(),
        rel,
        b"the head every member holds\n",
        retained,
    )
    .await;

    engine.restore_retained_version(id).await.unwrap();

    assert_eq!(std::fs::read(watch.path().join(rel)).unwrap(), retained);
    assert!(
        engine.db().retained_losers().unwrap().is_empty(),
        "the review row resolved"
    );
    assert_eq!(
        engine.db().get_entry(rel).unwrap().unwrap().local_hash,
        Some(ContentHash::of_raw(retained)),
        "the restored bytes are this seat's head"
    );
}

/// Over a cloud-only placeholder the restore refuses, saying why, and never
/// opens the file: nothing is written, the review row and the retained bytes
/// stay, and the restore runs once the file's bytes are back.
///
/// The file here is held open with no sharing, so an open by the restore fails
/// (it would wedge on a live root); `FILE_ATTRIBUTE_OFFLINE` is the half of the
/// cloud-only attributes an ordinary process may set (`crate::placeholder`'s own
/// pin of the predicate). The row is the one the restore otherwise accepts —
/// `Synced` at the bytes the file held before the platform freed it.
#[cfg(windows)]
#[tokio::test]
async fn using_the_other_version_over_a_cloud_only_file_never_opens_it() {
    use std::os::windows::fs::OpenOptionsExt;

    let watch = tempfile::tempdir().unwrap();
    let engine = group_seat(watch.path());
    let rel = "notes.txt";
    let head = b"the head every member holds\n";
    let retained = b"this replica's edit the group's later writer superseded\n";
    let id = head_with_a_retained_loser(&engine, watch.path(), rel, head, retained).await;
    let pending = |engine: &SyncEngine| {
        engine
            .db()
            .own_pending_changes(100)
            .unwrap()
            .into_iter()
            .map(|r| (r.path, r.change_type, r.manifest_hash))
            .collect::<Vec<_>>()
    };
    let pending_before = pending(&engine);

    let abs = watch.path().join(rel);
    crate::placeholder::set_offline_for_test(&abs, true);
    assert!(
        crate::placeholder::path_is_cloud_placeholder(&abs),
        "precondition: the file reads as cloud-only"
    );
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&abs)
        .expect("hold the placeholder with no sharing");

    let refused = engine
        .restore_retained_version(id)
        .await
        .expect_err("a cloud-only file is not the restore's to replace");
    drop(held);

    assert_eq!(
        format!("{refused:#}"),
        crate::engine::RESTORE_OVER_CLOUD_ONLY,
        "refused for the honest reason, never for a failed open of the file"
    );
    assert_eq!(std::fs::read(&abs).unwrap(), head, "nothing was written");
    assert!(
        crate::placeholder::path_is_cloud_placeholder(&abs),
        "the platform's file stands"
    );
    assert_eq!(
        engine
            .db()
            .retained_losers()
            .unwrap()
            .into_iter()
            .map(|(row_id, _)| row_id)
            .collect::<Vec<_>>(),
        vec![id],
        "the review row stays"
    );
    assert_eq!(
        engine
            .read_retained_version(&hex::encode(
                ContentHash::of_raw(b"the retained edit's manifest").digest()
            ))
            .as_deref(),
        Some(&retained[..]),
        "the retained bytes stay"
    );
    assert_eq!(pending(&engine), pending_before, "nothing was minted");

    // The bytes back on this disk (the platform's hydration, here the
    // attribute cleared): the same row restores.
    crate::placeholder::set_offline_for_test(&abs, false);
    engine.restore_retained_version(id).await.unwrap();
    assert_eq!(std::fs::read(&abs).unwrap(), retained);
    assert!(engine.db().retained_losers().unwrap().is_empty());
}
