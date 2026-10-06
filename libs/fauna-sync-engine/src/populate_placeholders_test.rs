//! Placeholder population for the on-demand hydration host:
//! [`SyncEngine::record_placeholders_from_changes`].
//!
//! The Windows cfapi host opens a folder that is **on-demand from the start**:
//! its SyncDb is empty (it runs no watcher/reconcile), so nothing can be listed
//! and nothing can hydrate until the folder's entries are learned from the
//! nest. `populate_placeholders_from_nest` pulls the `changes.list` control
//! plane (bearer-only, no MLS) and folds it into `SyncState::Placeholder` rows —
//! path + size + manifest hash, but **no bytes** — so the FETCH_PLACEHOLDERS /
//! FETCH_DATA callbacks have something to list and `download_file_bytes` can
//! resolve a manifest on open.
//!
//! These tests cover the pure folding/recording core
//! (`record_placeholders_from_changes`) with hand-built `SyncChange` fixtures
//! and an in-memory db — no network, no WS-RPC. The thin `changes.list` fetch
//! wrapper is exercised against a real nest, not unit-tested (the crate leaves
//! the WS-RPC plane unconnected in tests; the byte plane is wiremocked, but
//! `changes.list` is not an HTTP route — it moved to WS-RPC).
//!
//! Lives under `src/` (gated `#[cfg(test)]` in `lib.rs`), mirroring
//! `download_file_bytes_test.rs`.

use std::sync::Arc;

use fauna_core::data::ContentHash;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::sync::SyncChange;

use crate::adaptive::AdaptiveConcurrency;
use crate::db::{SyncDb, SyncState};
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::transfer::TransferPool;

// ─────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────

/// A `SyncEngine` whose clients point at an unreachable URL — these tests never
/// touch the network (`record_placeholders_from_changes` is pure DB work).
fn test_engine(watch_dir: std::path::PathBuf) -> SyncEngine {
    let db = SyncDb::open_in_memory().unwrap();
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        "http://127.0.0.1:9".to_string(),
        ActorKeypair::generate(),
        bearer,
        reqwest::Client::new(),
    ));
    let device_id = [0u8; 32];
    let client = SyncClient::new(auth, &device_id);
    let nest_client =
        fauna_client::NestClient::new("http://127.0.0.1:9".to_string(), ActorKeypair::generate());
    let transfer_pool = TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None);
    SyncEngine::new(
        watch_dir,
        db,
        client,
        Some("__test".to_string()), // folder
        device_id,
        None, // mls
        None, // epoch_secret
        None, // backup_key
        None, // mls_group_id
        None, // content_keys
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        transfer_pool,
        nest_client,
        crate::config::SyncMode::Sync,
    )
}

/// 64-hex-char manifest hash distinguished by a single fill byte.
fn manifest_hex(fill: u8) -> String {
    hex::encode([fill; 32])
}

/// A `SyncChange` for `path`: `manifest = Some` is a create/modify, `None` is a
/// delete (the wire contract — `manifest_hash = None` records a delete).
fn change(seq: i64, path: &str, manifest: Option<&str>, size: i64, created_at: i64) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{path}"),
        manifest_hash: manifest.map(str::to_string),
        size_bytes: size,
        change_type: if manifest.is_some() {
            "create".to_string()
        } else {
            "delete".to_string()
        },
        created_at,
        path: Some(path.to_string()),
        device_id: Some("device".to_string()),
        content_key_version: None,
        thumbnail_hash: None,
        ..Default::default()
    }
}

// ─────────────────────────────────────────────────────────────────────
// record_placeholders_from_changes
// ─────────────────────────────────────────────────────────────────────

#[test]
fn records_placeholder_with_manifest_size_mtime_and_no_bytes() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let manifest = manifest_hex(0xAB);
    // The nest stamps `sync_changes.created_at` in MILLISECONDS
    // (`now_epoch_millis()` — `bins/fauna-nest/src/db/sync_storage.rs`), so the
    // fixture must carry a millisecond-scale value or it cannot catch a missing
    // unit conversion. (It previously passed a seconds-scale value, which made the
    // buggy identity pass-through look correct — see the assertion below.)
    let changes = vec![change(
        1,
        "docs/a.txt",
        Some(&manifest),
        100,
        1_700_000_000_000,
    )];

    let n = engine
        .record_placeholders_from_changes(&changes)
        .unwrap()
        .recorded;
    assert_eq!(n, 1, "one placeholder recorded");

    let entry = engine
        .db()
        .get_entry("docs/a.txt")
        .unwrap()
        .expect("placeholder entry recorded");
    assert_eq!(entry.state, SyncState::Placeholder);
    assert_eq!(
        entry.manifest_hash,
        Some(ContentHash::from_digest_raw([0xAB; 32])),
        "manifest hash must match the change so download_file_bytes can resolve it"
    );
    assert_eq!(entry.size_bytes, 100, "placeholder shows the right size");
    assert_eq!(
        entry.remote_mtime, 1_700_000_000,
        "remote_mtime is Unix SECONDS — the nest's millisecond created_at must be \
         normalized on ingest. It sits beside local_mtime (`as_secs()`) and feeds \
         PlaceholderRow.mtime -> cfapi `unix_to_filetime`, which overflows i64 on a \
         millisecond value (and silently wraps to a garbage FILETIME in release)."
    );

    // No bytes hydrated: the on-demand population must never write the file.
    assert!(
        !watch.path().join("docs/a.txt").exists(),
        "population must not download or write any bytes"
    );
}

/// The fold names the rows it CREATED — a path that had no row before — and only those:
/// a re-pointed placeholder is already on disk wherever its directory was listed, and an
/// unchanged head wrote nothing. That list is what an on-demand host materializes eagerly
/// into a directory the OS will never list again (`delete-propagation.md` § *The floor on
/// an on-demand root*, decision (f)).
#[test]
fn the_fold_names_its_newly_created_rows_and_only_those() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let (a, b, b2) = (manifest_hex(0xA1), manifest_hex(0xB1), manifest_hex(0xB2));
    let first = engine
        .record_placeholders_from_changes(&[
            change(1, "a.txt", Some(&a), 5, 1_700_000_000_000),
            change(2, "sub/b.txt", Some(&b), 7, 1_700_000_000_000),
        ])
        .unwrap();
    assert_eq!(
        first.created,
        vec![
            crate::enumerate::PlaceholderRow {
                rel: "a.txt".into(),
                size: 5,
                mtime: 1_700_000_000
            },
            crate::enumerate::PlaceholderRow {
                rel: "sub/b.txt".into(),
                size: 7,
                mtime: 1_700_000_000
            },
        ],
        "a first fold creates every row, sorted by path"
    );

    // Unchanged `a.txt`, re-pointed `sub/b.txt`, brand-new `sub/c.txt`.
    let c = manifest_hex(0xC1);
    let second = engine
        .record_placeholders_from_changes(&[
            change(1, "a.txt", Some(&a), 5, 1_700_000_000_000),
            change(3, "sub/b.txt", Some(&b2), 8, 1_700_000_100_000),
            change(4, "sub/c.txt", Some(&c), 9, 1_700_000_200_000),
        ])
        .unwrap();
    assert_eq!(
        second.recorded, 2,
        "the re-point and the create are both written"
    );
    assert_eq!(
        second.created,
        vec![crate::enumerate::PlaceholderRow {
            rel: "sub/c.txt".into(),
            size: 9,
            mtime: 1_700_000_200
        }],
        "only the brand-new path is created — a re-point is not a create"
    );
}

/// A 0-byte file has no bytes to fetch, so on an on-demand root cfapi fires **no**
/// FETCH_DATA when it is opened (measured on a live cfapi root, 2026-07-15:
/// `cfapi_live_integration::opening_a_zero_byte_placeholder_...` — opening a 0-byte
/// placeholder clears its OFFLINE/RECALL bits, so the file is genuinely present-and-empty
/// on disk, but delivers no callback). Its row therefore can never flip to Synced via the
/// fetch path. The fold records it as a `Placeholder` (it must **not** be Synced-while-absent,
/// or reconcile's delete-detection would erase this never-materialized entry from the nest —
/// the data-loss trap the "record it Synced" cheap fix fell into) carrying the empty-content
/// identity; the overlay reads it `Synced` via [`SyncState::effective_for_size`]. Shared here
/// so a macOS File Provider (identical empty-file case) inherits it (priority #2).
#[test]
fn zero_byte_file_is_recorded_placeholder_with_the_empty_content_identity() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let empty = manifest_hex(0xE0);
    let full = manifest_hex(0xF0);
    let changes = vec![
        change(1, "empty.txt", Some(&empty), 0, 1_700_000_000_000),
        change(2, "full.txt", Some(&full), 11, 1_700_000_000_000),
    ];

    let n = engine
        .record_placeholders_from_changes(&changes)
        .unwrap()
        .recorded;
    assert_eq!(n, 2, "both files are recorded");

    let empty_entry = engine.db().get_entry("empty.txt").unwrap().unwrap();
    // The row stays a Placeholder — it must NOT be Synced-while-absent, or reconcile's
    // delete-detection (which iterates only Synced rows) would erase this never-materialized
    // entry from the nest. The overlay reads it Synced via `effective_for_size` instead.
    assert_eq!(empty_entry.state, SyncState::Placeholder);
    assert_eq!(
        empty_entry.state.effective_for_size(empty_entry.size_bytes),
        SyncState::Synced,
        "a 0-byte placeholder must badge Synced — it is present-and-empty, and cfapi fires no \
         FETCH_DATA to ever flip its row",
    );
    // The empty-content identity keeps reconcile stable once the OS materializes the file
    // present-and-empty (hash==local_hash → no spurious 'modified' upload of an empty file).
    assert_eq!(empty_entry.local_hash, Some(ContentHash::of_raw(b"")));
    assert_eq!(empty_entry.size_bytes, 0);
    assert_eq!(
        empty_entry.manifest_hash,
        Some(ContentHash::from_digest_raw([0xE0; 32])),
    );

    // The non-empty file is an ordinary cloud-only placeholder: no local bytes, badges CloudOnly.
    let full_entry = engine.db().get_entry("full.txt").unwrap().unwrap();
    assert_eq!(full_entry.state, SyncState::Placeholder);
    assert_eq!(
        full_entry.state.effective_for_size(full_entry.size_bytes),
        SyncState::Placeholder,
        "a non-empty file must stay a Placeholder — it has real bytes to hydrate on open",
    );
    assert_eq!(full_entry.local_hash, None);
}

/// A DB written before this fix (or a file that *shrank* to empty) can carry a 0-byte file
/// as a `Placeholder` with `local_hash = None`. A re-fold must re-write it with the empty-
/// content identity, not skip it on the head-matches idempotency short-circuit the non-empty
/// placeholders use — else, once the OS materializes it present-and-empty, reconcile reads it
/// as a local edit and spuriously uploads an empty file.
#[test]
fn an_existing_zero_byte_placeholder_gets_the_empty_content_identity() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    // Seed the state directly: a 0-byte Placeholder row with NO local_hash (what a
    // mark_hydrated early return then eviction, or a shrink, leaves).
    engine
        .db()
        .upsert_entry(
            "empty.txt",
            None,
            None,
            Some(ContentHash::from_digest_raw([0x0E; 32])),
            SyncState::Placeholder,
            0,
            1_700_000_000,
            0,
            1,
            None,
        )
        .unwrap();

    // Re-fold the SAME head. A non-empty placeholder would `continue` here; a 0-byte one must
    // be re-written to carry the empty-content identity.
    let m = manifest_hex(0x0E);
    let n = engine
        .record_placeholders_from_changes(&[change(1, "empty.txt", Some(&m), 0, 1_700_000_000_000)])
        .unwrap()
        .recorded;
    assert_eq!(n, 1, "the re-write counts as a row written");

    let entry = engine.db().get_entry("empty.txt").unwrap().unwrap();
    assert_eq!(
        entry.state,
        SyncState::Placeholder,
        "still a Placeholder — never Synced-while-absent",
    );
    assert_eq!(
        entry.local_hash,
        Some(ContentHash::of_raw(b"")),
        "the empty-content identity must now be stamped so reconcile stays stable",
    );
}

/// The backfill above runs ONCE: a 0-byte row already carrying the empty-content
/// identity is as idempotent as any other unchanged-head row. Before this pin the
/// fold re-wrote (and counted) it on every pass, so `refresh_from_nest` reported
/// "something new" forever for any set holding an empty file — the appex
/// signalled + re-enumerated on every tick, permanently.
#[test]
fn a_stamped_zero_byte_placeholder_refolds_as_a_noop() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let m = manifest_hex(0x0E);
    let fold = |engine: &SyncEngine| {
        engine
            .record_placeholders_from_changes(&[change(
                1,
                "empty.txt",
                Some(&m),
                0,
                1_700_000_000_000,
            )])
            .unwrap()
            .recorded
    };

    assert_eq!(fold(&engine), 1, "first fold stamps the row");
    assert_eq!(
        fold(&engine),
        0,
        "an unchanged-head 0-byte row already carrying the empty-content identity \
         must fold as a no-op, not re-count every pass"
    );
}

#[test]
fn latest_manifest_per_path_wins() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let m1 = manifest_hex(0x01);
    let m2 = manifest_hex(0x02);
    let changes = vec![
        change(1, "a", Some(&m1), 10, 100),
        change(2, "a", Some(&m2), 20, 200),
    ];

    let n = engine
        .record_placeholders_from_changes(&changes)
        .unwrap()
        .recorded;
    assert_eq!(n, 1, "one path → one placeholder");

    let entry = engine.db().get_entry("a").unwrap().unwrap();
    assert_eq!(
        entry.manifest_hash,
        Some(ContentHash::from_digest_raw([0x02; 32])),
        "the highest-seq change's manifest wins"
    );
    assert_eq!(entry.size_bytes, 20, "the highest-seq change's size wins");
}

#[test]
fn deleted_file_is_not_recorded() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let m = manifest_hex(0x01);
    let changes = vec![
        change(1, "a", Some(&m), 10, 100),
        change(2, "a", None, 0, 200), // delete supersedes the create
    ];

    let n = engine
        .record_placeholders_from_changes(&changes)
        .unwrap()
        .recorded;
    assert_eq!(n, 0, "a deleted file gets no placeholder");
    assert!(
        engine.db().get_entry("a").unwrap().is_none(),
        "no entry for a file deleted on the nest"
    );
}

#[test]
fn already_tracked_path_is_left_alone() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    // The always→on-demand *switch* path leaves a populated db: a prior sync
    // recorded this path as Synced. Population must not clobber it.
    engine
        .db()
        .upsert_entry(
            "a",
            Some(ContentHash::from_digest_raw([0x09; 32])),
            None,
            Some(ContentHash::from_digest_raw([0x09; 32])),
            SyncState::Synced,
            5,
            5,
            50,
            1,
            None,
        )
        .unwrap();

    let m = manifest_hex(0x01);
    let changes = vec![change(1, "a", Some(&m), 10, 100)];

    let n = engine
        .record_placeholders_from_changes(&changes)
        .unwrap()
        .recorded;
    assert_eq!(n, 0, "an already-tracked path is not re-recorded");

    let entry = engine.db().get_entry("a").unwrap().unwrap();
    assert_eq!(
        entry.state,
        SyncState::Synced,
        "the existing Synced entry (switch case) is preserved, not overwritten"
    );
    assert_eq!(entry.size_bytes, 50, "the existing entry is untouched");
}

#[test]
fn sets_anchor_to_max_seq() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let m1 = manifest_hex(0x01);
    let m2 = manifest_hex(0x02);
    let changes = vec![
        change(3, "a", Some(&m1), 10, 100),
        change(7, "b", Some(&m2), 20, 200),
    ];

    engine.record_placeholders_from_changes(&changes).unwrap();
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        7,
        "the anchor advances to the max seq so a later incremental pull resumes correctly"
    );
}

#[test]
fn out_of_order_delete_after_create_is_not_resurrected() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let m = manifest_hex(0x01);
    // The slice is NOT in seq order: the higher-seq delete appears first.
    let changes = vec![
        change(2, "a", None, 0, 200),      // delete @ seq 2 (latest state)
        change(1, "a", Some(&m), 10, 100), // create @ seq 1 (superseded)
    ];

    let n = engine
        .record_placeholders_from_changes(&changes)
        .unwrap()
        .recorded;
    assert_eq!(
        n, 0,
        "the latest state (seq 2 delete) wins regardless of slice order"
    );
    assert!(engine.db().get_entry("a").unwrap().is_none());
}

#[test]
fn cross_pull_delete_removes_stale_placeholder() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    // Pull 1: a create records the placeholder.
    let m = manifest_hex(0x01);
    let n1 = engine
        .record_placeholders_from_changes(&[change(1, "a", Some(&m), 10, 100)])
        .unwrap()
        .recorded;
    assert_eq!(n1, 1, "pull 1 records the placeholder");
    assert_eq!(
        engine.db().get_entry("a").unwrap().unwrap().state,
        SyncState::Placeholder,
    );

    // Pull 2: a later-pull delete of the same path must remove the stale
    // placeholder row so the file browser stops listing the gone file. The
    // delete is NOT in the same slice as the create — it's a separate pull over
    // a db the prior pull already populated.
    let n2 = engine
        .record_placeholders_from_changes(&[change(2, "a", None, 0, 200)])
        .unwrap()
        .recorded;
    assert_eq!(n2, 0, "a delete records no new placeholder");
    assert!(
        engine.db().get_entry("a").unwrap().is_none(),
        "the prior Placeholder row is removed when its remote file is deleted across pulls",
    );
}

#[test]
fn cross_pull_delete_leaves_non_placeholder_entry() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    // The always→on-demand *switch* path left a Synced (on-disk) entry. A delete
    // arriving via this bytes-free population fold must NOT remove it — deleting
    // an on-disk file's tracking + bytes is the full reconcile path's job, not
    // the placeholder fold's. Only Placeholder rows are the fold's to drop.
    engine
        .db()
        .upsert_entry(
            "a",
            Some(ContentHash::from_digest_raw([0x09; 32])),
            None,
            Some(ContentHash::from_digest_raw([0x09; 32])),
            SyncState::Synced,
            5,
            5,
            50,
            1,
            None,
        )
        .unwrap();

    let n = engine
        .record_placeholders_from_changes(&[change(2, "a", None, 0, 200)])
        .unwrap()
        .recorded;
    assert_eq!(n, 0, "a delete records no placeholder");

    let entry = engine.db().get_entry("a").unwrap().unwrap();
    assert_eq!(
        entry.state,
        SyncState::Synced,
        "a Synced (switch-case) entry is left to the full reconcile path, not removed by the fold",
    );
}

// ─────────────────────────────────────────────────────────────────────
// Re-pointing an already-tracked placeholder at a moved head
//
// The fold owns `Placeholder` rows and only those — the same rule the delete
// arm already follows ("only a Placeholder row is ours to remove"). A
// Placeholder carries **no local bytes**, so re-pointing it at the nest's
// current head can clobber nothing.
//
// This closes two bugs at once (`docs/goal/behavior/file-sync.md` § Restore):
//   1. a restore interrupted between the nest record and the recording device's
//      local apply left that device's row stale forever;
//   2. a *remote* modify never reached an existing cloud-only placeholder on the
//      Windows on-demand host (it folds `changes.list` once at `prepare()` and
//      never runs `pull_remote_changes`), so opening it hydrated the superseded
//      manifest.
// ─────────────────────────────────────────────────────────────────────

/// A `SyncChange` carrying an explicit content-key generation.
fn change_with_generation(
    seq: i64,
    path: &str,
    manifest: &str,
    size: i64,
    created_at: i64,
    content_key_version: Option<u64>,
) -> SyncChange {
    SyncChange {
        change_type: "modify".to_string(),
        content_key_version,
        ..change(seq, path, Some(manifest), size, created_at)
    }
}

#[test]
fn already_tracked_placeholder_is_repointed_at_the_new_head() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    // Pull 1 records the placeholder at manifest 0x01.
    let old = manifest_hex(0x01);
    engine
        .record_placeholders_from_changes(&[change_with_generation(1, "a", &old, 10, 100, Some(2))])
        .unwrap();

    // The head moves — a restore recorded by this very device, or an ordinary
    // remote modify from another one. Either way the bytes are not local.
    let new = manifest_hex(0x02);
    let n = engine
        .record_placeholders_from_changes(&[change_with_generation(
            9,
            "a",
            &new,
            99,
            900_000,
            Some(5),
        )])
        .unwrap()
        .recorded;
    assert_eq!(n, 1, "the re-pointed row counts as a row written");

    let entry = engine.db().get_entry("a").unwrap().unwrap();
    assert_eq!(
        entry.manifest_hash,
        Some(ContentHash::from_digest_raw([0x02; 32])),
        "the hydration anchor follows the nest's head — otherwise the next open \
         serves the superseded manifest",
    );
    assert_eq!(entry.size_bytes, 99);
    assert_eq!(entry.remote_mtime, 900);
    assert_eq!(
        entry.content_key_version,
        Some(5),
        "the generation must follow the head, or a sealed manifest fails to open",
    );
    assert_eq!(entry.state, SyncState::Placeholder, "still bytes-free");
}

/// After a *successful* restore the local row already equals the nest head, so
/// the next start-up fold must be a no-op — no churn, no spurious write.
#[test]
fn repointing_is_idempotent_when_the_head_already_matches() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let m = manifest_hex(0x07);
    engine
        .record_placeholders_from_changes(&[change_with_generation(1, "a", &m, 10, 100, Some(3))])
        .unwrap();

    // Same head, re-folded (a later pull re-lists it).
    let n = engine
        .record_placeholders_from_changes(&[change_with_generation(1, "a", &m, 10, 100, Some(3))])
        .unwrap()
        .recorded;
    assert_eq!(n, 0, "an unchanged head rewrites nothing");

    let entry = engine.db().get_entry("a").unwrap().unwrap();
    assert_eq!(entry.size_bytes, 10);
    assert_eq!(entry.content_key_version, Some(3));
}

/// A `Synced` row's bytes are **on disk**. Re-pointing it would silently orphan
/// them (and the always→on-demand switch path deliberately leaves such rows), so
/// the fold must keep its hands off — freeing those bytes is the platform
/// placeholder surface's job. It *reports* the row instead, carrying the nest head.
#[test]
fn already_tracked_synced_row_is_never_repointed() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    engine
        .db()
        .upsert_entry(
            "a",
            Some(ContentHash::from_digest_raw([0x09; 32])),
            None,
            Some(ContentHash::from_digest_raw([0x09; 32])),
            SyncState::Synced,
            5,
            5,
            50,
            7, // version_num — must survive onto the report
            None,
        )
        .unwrap();

    let new = manifest_hex(0x02);
    let fold = engine
        .record_placeholders_from_changes(&[change_with_generation(
            9,
            "a",
            &new,
            99,
            900_000,
            Some(5),
        )])
        .unwrap();
    assert_eq!(
        fold.recorded, 0,
        "a Synced row is not the fold's to rewrite"
    );

    let entry = engine.db().get_entry("a").unwrap().unwrap();
    assert_eq!(entry.state, SyncState::Synced);
    assert_eq!(
        entry.manifest_hash,
        Some(ContentHash::from_digest_raw([0x09; 32])),
        "the on-disk file's manifest is untouched",
    );
    assert_eq!(entry.size_bytes, 50);

    // ...but the caller is told, so it can free the stale bytes via cfapi and
    // then re-point. The report carries the *nest's* head, not the row's.
    assert_eq!(fold.stale_hydrated.len(), 1);
    let stale = &fold.stale_hydrated[0];
    assert_eq!(stale.relative_path, "a");
    assert_eq!(
        stale.manifest_hash,
        ContentHash::from_digest_raw([0x02; 32])
    );
    assert_eq!(stale.size_bytes, 99);
    assert_eq!(stale.content_key_version, Some(5));
    assert_eq!(stale.remote_mtime, 900);
    assert_eq!(stale.version_num, 7, "the row's version_num is preserved");
}

/// The invalidation must be idempotent: a hydrated row already at the nest's head
/// is a correct local copy. Reporting it would dehydrate a file on every single
/// pass — throwing away bytes the user just paid to fetch.
#[test]
fn synced_row_at_the_current_head_is_not_reported() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let head = manifest_hex(0x03);
    engine
        .db()
        .upsert_entry(
            "a",
            Some(ContentHash::from_digest_raw([0x03; 32])),
            None,
            Some(ContentHash::from_digest_raw([0x03; 32])),
            SyncState::Synced,
            5,
            5,
            42,
            1,
            Some(2),
        )
        .unwrap();

    let fold = engine
        .record_placeholders_from_changes(&[change_with_generation(
            9,
            "a",
            &head,
            42,
            900_000,
            Some(2),
        )])
        .unwrap();

    assert_eq!(fold.recorded, 0);
    assert!(
        fold.stale_hydrated.is_empty(),
        "a hydrated row already at the head is current — never invalidate it",
    );
}

/// Only `Synced` is a clean hydrated copy. A row mid-conflict (or locally
/// modified, or in flight) carries state some *other* pass owns; reporting it as
/// stale-hydrated would invite a caller to free bytes the user has not reconciled.
#[test]
fn a_non_synced_tracked_row_is_never_reported_as_stale_hydrated() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    for (path, state) in [
        ("conflicted", SyncState::Conflicted),
        ("locally-modified", SyncState::LocallyModified),
        ("uploading", SyncState::Uploading),
    ] {
        engine
            .db()
            .upsert_entry(
                path,
                Some(ContentHash::from_digest_raw([0x09; 32])),
                None,
                Some(ContentHash::from_digest_raw([0x09; 32])),
                state,
                5,
                5,
                50,
                1,
                None,
            )
            .unwrap();
    }

    let new = manifest_hex(0x02);
    let fold = engine
        .record_placeholders_from_changes(&[
            change_with_generation(9, "conflicted", &new, 99, 900_000, None),
            change_with_generation(10, "locally-modified", &new, 99, 900_000, None),
            change_with_generation(11, "uploading", &new, 99, 900_000, None),
        ])
        .unwrap();

    assert_eq!(fold.recorded, 0, "none of these are the fold's to rewrite");
    assert!(
        fold.stale_hydrated.is_empty(),
        "only a Synced row is a clean hydrated copy the caller may invalidate",
    );
}

/// The report is sorted, so the apply order (and the log line) is deterministic
/// despite the fold's internal `HashMap`.
#[test]
fn stale_hydrated_rows_are_reported_in_path_order() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    for path in ["c", "a", "b"] {
        engine
            .db()
            .upsert_entry(
                path,
                Some(ContentHash::from_digest_raw([0x09; 32])),
                None,
                Some(ContentHash::from_digest_raw([0x09; 32])),
                SyncState::Synced,
                5,
                5,
                50,
                1,
                None,
            )
            .unwrap();
    }

    let new = manifest_hex(0x02);
    let fold = engine
        .record_placeholders_from_changes(&[
            change_with_generation(9, "c", &new, 99, 900_000, None),
            change_with_generation(10, "a", &new, 99, 900_000, None),
            change_with_generation(11, "b", &new, 99, 900_000, None),
        ])
        .unwrap();

    let paths: Vec<&str> = fold
        .stale_hydrated
        .iter()
        .map(|r| r.relative_path.as_str())
        .collect();
    assert_eq!(paths, ["a", "b", "c"]);
}

// ─────────────────────────────────────────────────────────────────────
// mark_placeholder (the OS-dehydrate inverse of mark_hydrated)
// ─────────────────────────────────────────────────────────────────────

/// The OS (Explorer "Free up space" / Storage Sense) dehydrated a hydrated file:
/// its local bytes are gone, but its content is unchanged on the nest.
/// [`SyncEngine::mark_placeholder`] records that honestly — it flips the row
/// `Synced` → `Placeholder` while leaving the manifest identity (the hydration
/// anchor) untouched. It is the symmetric inverse of `mark_hydrated`
/// (`Placeholder` → `Synced`), and a **pure state transition** — NOT a re-point
/// (`repoint_hydrated_to_placeholder` is for a *moved* nest head; here the head is
/// unchanged, only the bytes are freed). Shared on the engine so a macOS File
/// Provider host (identical OS-dehydrate case) inherits it (priority #2).
#[test]
fn mark_placeholder_flips_synced_to_placeholder_keeping_the_manifest() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    // Seed a hydrated (Synced) row carrying a manifest, as a served fetch leaves it.
    let manifest = manifest_hex(0xAB);
    engine
        .record_placeholders_from_changes(&[change(
            1,
            "docs/a.txt",
            Some(&manifest),
            100,
            1_700_000_000_000,
        )])
        .unwrap();
    engine
        .db()
        .update_state("docs/a.txt", SyncState::Synced)
        .unwrap();

    engine.mark_placeholder("docs/a.txt").unwrap();

    let entry = engine.db().get_entry("docs/a.txt").unwrap().unwrap();
    assert_eq!(
        entry.state,
        SyncState::Placeholder,
        "an OS dehydrate must record the row back to Placeholder so the badge is honest",
    );
    assert_eq!(
        entry.manifest_hash,
        Some(ContentHash::from_digest_raw([0xAB; 32])),
        "a dehydrate frees bytes, not identity — the manifest anchor must survive so the \
         next open re-hydrates the same content",
    );
}

/// `mark_placeholder` on a path with no row is a benign no-op (the folder is
/// bound but not yet served, or the row was already pruned) — it must not error.
#[test]
fn mark_placeholder_is_a_noop_for_an_unknown_path() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    engine
        .mark_placeholder("never/seen.txt")
        .expect("marking an unknown path Placeholder is a benign no-op");
    assert!(engine.db().get_entry("never/seen.txt").unwrap().is_none());
}

// ─────────────────────────────────────────────────────────────────────
// apply_refresh_fold — the DB-only half of `refresh_from_nest` (the File
// Provider live-refresh tick's primitive: apple's `fauna.sync.changed` push
// handler nudges `NSFileProviderManager.signalEnumerator`, which lands in the
// extension's `enumerateChanges`, which calls `host.refresh()` ->
// `refresh_from_nest` -> this). Split out so the "did anything actually
// change" bool and the stale-hydrated repoint are unit-testable without a
// live nest connection — `refresh_from_nest`'s own fetch half is not (see this
// file's module doc).
// ─────────────────────────────────────────────────────────────────────

/// A fresh create folds `recorded > 0`, so the tick must report "something
/// changed" — the signal the appex uses to decide whether to call
/// `signalEnumerator` again.
#[test]
fn apply_refresh_fold_reports_true_when_a_new_placeholder_was_recorded() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let m = manifest_hex(0x01);
    let fold = engine
        .record_placeholders_from_changes(&[change(1, "a", Some(&m), 10, 100)])
        .unwrap();
    assert_eq!(fold.recorded, 1);

    let changed = engine.apply_refresh_fold(&fold).unwrap();
    assert!(
        changed,
        "a newly-recorded placeholder must report changed=true"
    );
}

/// A true no-op re-fold (unchanged head, no stale-hydrated rows) must report
/// `false` — this is the exact regression the module doc's
/// `a_stamped_zero_byte_placeholder_refolds_as_a_noop` test names for the fold
/// layer: reporting `true` here on every tick would make the File Provider
/// live-refresh loop `signalEnumerator` (and so re-invoke `enumerateChanges`)
/// forever, even with nothing new to enumerate.
#[test]
fn apply_refresh_fold_reports_false_on_a_true_noop() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    let m = manifest_hex(0x01);
    engine
        .record_placeholders_from_changes(&[change(1, "a", Some(&m), 10, 100)])
        .unwrap();

    // Re-fold the SAME head — a later refresh tick re-pulling an unchanged set.
    let fold = engine
        .record_placeholders_from_changes(&[change(1, "a", Some(&m), 10, 100)])
        .unwrap();
    assert_eq!(fold.recorded, 0);
    assert!(fold.stale_hydrated.is_empty());

    let changed = engine.apply_refresh_fold(&fold).unwrap();
    assert!(
        !changed,
        "an unchanged-head re-fold must report changed=false"
    );
}

/// A stale-hydrated row (a remote modify of a file this host had hydrated)
/// must be re-pointed at the nest's new head, marked back to `Placeholder`,
/// have its dehydration proof cleared (so `content_version` changes and the OS
/// re-fetches), and the tick must report `changed=true`.
#[test]
fn apply_refresh_fold_repoints_stale_hydrated_row_and_clears_recorded_content_hash() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    // Seed a hydrated (Synced) row with its dehydration proof stamped, as a
    // served fetch + `stamp_recorded_content_from_local` leaves it.
    engine
        .db()
        .upsert_entry(
            "a",
            Some(ContentHash::from_digest_raw([0x09; 32])),
            None,
            Some(ContentHash::from_digest_raw([0x09; 32])),
            SyncState::Synced,
            5,
            5,
            50,
            7,
            None,
        )
        .unwrap();
    engine
        .db()
        .stamp_recorded_content_from_local("a", crate::db::ProofOrigin::Fetched)
        .unwrap();
    assert!(
        engine
            .db()
            .get_entry("a")
            .unwrap()
            .unwrap()
            .recorded_content_hash
            .is_some(),
        "the dehydration proof must be stamped before this test exercises clearing it"
    );

    // The nest's head moves — a remote modify this on-demand host never watches for.
    let new = manifest_hex(0x02);
    let fold = engine
        .record_placeholders_from_changes(&[change_with_generation(
            9,
            "a",
            &new,
            99,
            900_000,
            Some(5),
        )])
        .unwrap();
    assert_eq!(
        fold.recorded, 0,
        "a Synced row is reported, not rewritten by the fold"
    );
    assert_eq!(fold.stale_hydrated.len(), 1);

    let changed = engine.apply_refresh_fold(&fold).unwrap();
    assert!(
        changed,
        "a re-pointed stale-hydrated row must report changed=true"
    );

    let entry = engine.db().get_entry("a").unwrap().unwrap();
    assert_eq!(
        entry.state,
        SyncState::Placeholder,
        "the row must flip back to Placeholder — no local bytes after the repoint",
    );
    assert_eq!(
        entry.manifest_hash,
        Some(ContentHash::from_digest_raw([0x02; 32])),
        "the row must follow the nest's new head",
    );
    assert_eq!(
        entry.recorded_content_hash, None,
        "the dehydration proof must be cleared so content_version changes and the OS re-fetches",
    );
}

/// **The frontier check** (`mls-group-key-material.md` § M2 → *Writer-signed
/// change records*, ruling (5) residual (i)): a nest that withholds this
/// device's later edit and serves an old genuine row gets nothing past the
/// path's edit-frontier — a row below it is never the head, and a peer row
/// above it whose signed watermark never reached it is a replay. A peer row
/// that incorporated the frontier folds as before.
#[test]
fn the_fold_skips_rows_its_edit_frontier_already_covers() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    // This device recorded `a.txt` at seq 10 (its echo is withheld).
    engine.causal().advance_edit_frontier("a.txt", 10);
    let (old, replay, covering) = (manifest_hex(1), manifest_hex(2), manifest_hex(3));
    let peer = |seq, m: &str, w| SyncChange {
        device_id: Some("peer-device".into()),
        derived_through: Some(w),
        ..change(seq, "a.txt", Some(m), 5, 1_700_000_000_000)
    };
    let fold = |rows: Vec<SyncChange>| {
        engine.record_placeholders_from_changes(&rows).unwrap();
        engine
            .db()
            .get_entry("a.txt")
            .unwrap()
            .and_then(|e| e.manifest_hash)
    };

    assert_eq!(
        fold(vec![
            change(4, "a.txt", Some(&old), 5, 1_700_000_000_000),
            peer(12, &replay, 7),
        ]),
        None,
        "the pre-frontier row and the stale replay above it both fold as absent"
    );
    assert_eq!(
        fold(vec![peer(13, &covering, 10)]),
        Some(ContentHash::from_digest_raw([3; 32])),
        "a peer row that incorporated the frontier folds"
    );
}
