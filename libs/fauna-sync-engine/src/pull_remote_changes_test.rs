//! Applying a `changes.list` batch to the local folder:
//! [`SyncEngine::apply_remote_changes`] — specifically its **delete** arm.
//!
//! A delete tombstone reaches *every* device, including the one that recorded
//! it: [`SyncEngine::fetch_changes`] passes `device_id = None`, so the nest's
//! device-exclusion filter is inert and the log echoes our own changes back.
//! Which device recorded a delete therefore does **not** tell you whether the
//! local file is still on disk — only the disk does.
//!
//! That distinction is the whole point of these tests. Two different actors on
//! one device record a delete under the *same* device id:
//!
//! - the **engine** ([`SyncEngine::handle_delete`]), which removes the db entry
//!   and only records the tombstone *after* the file already left the disk; and
//! - a **client UI** (the Media page's `media-delete-button`, which calls
//!   `fauna.sync.changes.record` through `MediaMachine::delete`), which records
//!   the tombstone and **never touches the disk at all**.
//!
//! Skipping every self-echoed delete served the first actor and orphaned the
//! second's file forever — the anchor advances past the tombstone whether or not
//! it was applied (`set_anchor(max_seq)`), so a skipped delete is never retried.
//! The disk check the arm already performs distinguishes the two actors on its
//! own, which is what these tests pin.
//!
//! Pure db + filesystem work against hand-built `SyncChange` fixtures — no
//! network (only the delete arm is exercised; a peer create would download).
//! Lives under `src/` (gated `#[cfg(test)]` in `lib.rs`), mirroring
//! `populate_placeholders_test.rs`.

use std::sync::Arc;

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

/// The device id every engine in this module runs as. `test_engine`'s
/// `SyncClient` derives `device_id_hex()` from these bytes, so a change carrying
/// [`our_device_hex`] is a self-echo and anything else is a peer.
const OUR_DEVICE: [u8; 32] = [0u8; 32];

pub(crate) fn our_device_hex() -> String {
    hex::encode(OUR_DEVICE)
}

/// A `SyncEngine` whose clients point at an unreachable URL — the delete arm is
/// pure db + filesystem work, so these tests never reach the network. (Also the
/// record-failure harness for `delete_ack_test` — against this engine every
/// `record_change` / chunk upload fails, which is that module's subject.)
pub(crate) fn test_engine(watch_dir: std::path::PathBuf) -> SyncEngine {
    test_engine_with_keys(watch_dir, None, None, None)
}

/// [`test_engine`] with the key material that decides which root a seal takes:
/// `backup_key` (the owner arm), `mls_group_id` (the **bound** marker), and
/// `content_keys` (the M2 generation history). The three together are what
/// `content_seal_root` / `effective_backup_key` branch on, so a test that cares
/// about the chunk-seal or label-seal root builds its engine here rather than
/// reaching into private fields.
pub(crate) fn test_engine_with_keys(
    watch_dir: std::path::PathBuf,
    backup_key: Option<fauna_core::crypto::OwnerSealKey>,
    mls_group_id: Option<Vec<u8>>,
    content_keys: Option<fauna_core::folder_keys::FolderContentKeys>,
) -> SyncEngine {
    // NB the default set name `__test` is a RESERVED rail. Harmless for the path
    // seal (paths seal regardless of their set's name) but a set-NAME seal refuses
    // a reserved name by construction, so a set-name test must name its own set —
    // [`test_engine_with_keys_named`].
    test_engine_with_keys_named(watch_dir, "__test", backup_key, mls_group_id, content_keys)
}

/// [`test_engine`] with its progress channel **live**, returning the receiver so
/// a test can read what the engine reported outward.
///
/// [`test_engine`] passes `None` for `progress_tx` (its subjects are db +
/// filesystem work), so an emission is unobservable there — and an unobservable
/// emission is exactly what the mass-delete floor's status surface is made of.
pub(crate) fn test_engine_with_progress(
    watch_dir: std::path::PathBuf,
) -> (
    SyncEngine,
    tokio::sync::mpsc::UnboundedReceiver<crate::progress::ProgressEvent>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (
        test_engine_inner(watch_dir, Some("__test"), None, None, None, Some(tx)),
        rx,
    )
}

/// [`test_engine_with_progress`] with **no bound folder**: `handle_delete`
/// skips the change record (nothing to record against) and completes the local
/// tombstone — which makes the row-tombstoning half of a delete path
/// observable in-process, where every folder-bound fixture's record fails
/// against the unreachable nest and deliberately leaves rows `Synced` for
/// retry. Used by the `apply_held_deletes` pins that assert the drive-through
/// itself; the record-first retry contract is pinned separately on the
/// folder-bound fixture.
pub(crate) fn test_engine_folderless_with_progress(
    watch_dir: std::path::PathBuf,
) -> (
    SyncEngine,
    tokio::sync::mpsc::UnboundedReceiver<crate::progress::ProgressEvent>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (
        test_engine_inner(watch_dir, None, None, None, None, Some(tx)),
        rx,
    )
}

/// [`test_engine_with_keys`] with an explicit folder **name** — needed by the
/// set-name seal tests, whose subject is the name itself.
pub(crate) fn test_engine_with_keys_named(
    watch_dir: std::path::PathBuf,
    folder: &str,
    backup_key: Option<fauna_core::crypto::OwnerSealKey>,
    mls_group_id: Option<Vec<u8>>,
    content_keys: Option<fauna_core::folder_keys::FolderContentKeys>,
) -> SyncEngine {
    test_engine_inner(
        watch_dir,
        Some(folder),
        backup_key,
        mls_group_id,
        content_keys,
        None,
    )
}

/// [`test_engine`] over a **caller-supplied control plane** — the seam a test
/// needs when its subject is what the engine does with the nest, not with the
/// db and the filesystem. Every other helper here points the engine at an
/// unreachable URL on purpose; this one lets a caller hand in a `NestClient`
/// wired to `fauna_client::testing`'s mocked socket, so the two authoritative
/// reads (`fauna.folders.list` + `fauna.folders.members.list`) and the pull
/// can be answered — and their absence observed — in-process at tier_1
/// (`crate::connected_arm_heal_test`).
pub(crate) fn test_engine_with_nest_client(
    watch_dir: std::path::PathBuf,
    folder: &str,
    nest_client: Arc<fauna_client::NestClient>,
) -> SyncEngine {
    test_engine_inner_over(
        watch_dir,
        Some(folder),
        None,
        None,
        None,
        None,
        Some(nest_client),
    )
}

/// The one construction site the three public helpers above funnel through, so
/// a new `SyncEngine::new` argument is added in exactly one place.
fn test_engine_inner(
    watch_dir: std::path::PathBuf,
    folder: Option<&str>,
    backup_key: Option<fauna_core::crypto::OwnerSealKey>,
    mls_group_id: Option<Vec<u8>>,
    content_keys: Option<fauna_core::folder_keys::FolderContentKeys>,
    progress_tx: crate::progress::ProgressTx,
) -> SyncEngine {
    test_engine_inner_over(
        watch_dir,
        folder,
        backup_key,
        mls_group_id,
        content_keys,
        progress_tx,
        None,
    )
}

/// [`test_engine_inner`] with the control plane left open to the caller:
/// `None` keeps the historical unreachable-URL client every other helper
/// wants, `Some(..)` takes the caller's.
#[allow(clippy::too_many_arguments)]
fn test_engine_inner_over(
    watch_dir: std::path::PathBuf,
    folder: Option<&str>,
    backup_key: Option<fauna_core::crypto::OwnerSealKey>,
    mls_group_id: Option<Vec<u8>>,
    content_keys: Option<fauna_core::folder_keys::FolderContentKeys>,
    progress_tx: crate::progress::ProgressTx,
    nest_client: Option<Arc<fauna_client::NestClient>>,
) -> SyncEngine {
    let db = SyncDb::open_in_memory().unwrap();
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        "http://127.0.0.1:9".to_string(),
        ActorKeypair::generate(),
        bearer,
        reqwest::Client::new(),
    ));
    let client = SyncClient::new(auth, &OUR_DEVICE);
    let nest_client = nest_client.unwrap_or_else(|| {
        fauna_client::NestClient::new("http://127.0.0.1:9".to_string(), ActorKeypair::generate())
    });
    let transfer_pool = TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), progress_tx);
    SyncEngine::new(
        watch_dir,
        db,
        client,
        folder.map(str::to_string),
        OUR_DEVICE,
        None, // mls
        None, // epoch_secret
        backup_key,
        mls_group_id,
        content_keys,
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        transfer_pool,
        nest_client,
        crate::config::SyncMode::Sync,
    )
}

/// A delete `SyncChange` for `path` attributed to `device` (the wire contract:
/// a delete carries no manifest — `manifest_hash = None`).
fn delete_change(seq: i64, path: &str, device: &str) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{path}"),
        manifest_hash: None,
        size_bytes: 0,
        change_type: "delete".to_string(),
        created_at: 1_700_000_000_000,
        path: Some(path.to_string()),
        device_id: Some(device.to_string()),
        content_key_version: None,
        thumbnail_hash: None,
        ..Default::default()
    }
}

/// A create `SyncChange` for `path` attributed to `device`.
fn create_change(seq: i64, path: &str, device: &str, fill: u8) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{path}"),
        manifest_hash: Some(hex::encode([fill; 32])),
        size_bytes: 11,
        change_type: "create".to_string(),
        created_at: 1_700_000_000_000,
        path: Some(path.to_string()),
        device_id: Some(device.to_string()),
        content_key_version: None,
        thumbnail_hash: Some(hex::encode([0xEE; 32])),
        ..Default::default()
    }
}

/// The db half of [`seed_tracked_file`] alone: a `Synced` row for `path` with
/// **nothing written to disk** — the already-vanished steady state, reached
/// without the vanishing.
///
/// # Why a fixture that never touches the watched directory exists
///
/// A test that seeds files and removes them again leaves those removals in the
/// filesystem's event history, and a watcher started *afterwards* is not
/// guaranteed to be blind to them: on macOS, FSEvents assigns event ids
/// asynchronously, so a stream created with `SinceNow` can still deliver a
/// removal that had already happened when the stream was created. Measured on
/// macOS, 2026-08-28, with a 30-iteration probe over the real
/// `LocalWrites::start`: **10 of 30** iterations delivered a `Removed` for a
/// file unlinked microseconds earlier. inotify has no such window, which is why
/// a green Linux run does not absolve the shape — the mac heavy tier's
/// Rust-test gate (`mac-rust-test-check`) is where it surfaced, as a flake in
/// `mass_delete_floor_test::the_resident_loop_routes_an_apply_command_to_its_engine`
/// (2026-08-28): the loop's own watcher consumed the pre-start removals through
/// `handle_delete`, which is deliberately floor-free
/// (`docs/goal/behavior/delete-propagation.md` § *A wholesale-vanished folder*),
/// tombstoned the rows, and left the floor with nothing to hold.
///
/// So any test that starts a **real watcher** over the fixture directory seeds
/// rows through here instead: the directory is never written to at all, no
/// event about it can exist on any platform, and the held-set precondition is
/// true by construction rather than by winning a race. Tests that drive
/// `reconcile`/`apply_held_deletes` directly — no watcher — keep using
/// [`seed_tracked_file`], whose on-disk realism is free there.
pub(crate) fn seed_tracked_row(engine: &SyncEngine, path: &str, body: &[u8]) {
    // The same real content hash `seed_tracked_file` records, computed over the
    // bytes rather than read back from a file, so the two fixtures produce
    // byte-identical rows.
    let hash = fauna_core::data::ContentHash::from_digest_raw(
        fauna_core::chunker_stream::blake3_of_reader(body).unwrap(),
    );
    engine
        .db()
        .upsert_entry(
            path,
            Some(hash), // local_hash
            Some(hash), // remote_hash
            Some(hash), // manifest_hash
            SyncState::Synced,
            1_700_000_000, // local_mtime
            1_700_000_000, // remote_mtime
            body.len() as i64,
            1,    // version_num
            None, // content_key_version
        )
        .unwrap();
}

/// Put `path` on disk with a `Synced` db row — the steady state of a tracked
/// file the engine believes agrees with the nest.
///
/// ⚠ Do not use this in a test that starts a real watcher over `watch` and then
/// removes the seeded files: see [`seed_tracked_row`] for the pre-start-removal
/// race that shape carries on macOS.
pub(crate) fn seed_tracked_file(
    engine: &SyncEngine,
    watch: &std::path::Path,
    path: &str,
    body: &[u8],
) {
    let full = watch.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, body).unwrap();
    let meta = std::fs::metadata(&full).unwrap();
    let mtime = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // The REAL content hash, exactly as a production reconcile records it — a
    // fake constant here would make the delete arm's disk-vs-row re-hash
    // (2026-07-29) mismatch for every seeded file, silently declining the very
    // deletes these tests apply.
    let hash = fauna_core::chunker_stream::content_hash_streaming(&full).unwrap();
    engine
        .db()
        .upsert_entry(
            path,
            Some(hash), // local_hash
            Some(hash), // remote_hash
            Some(hash), // manifest_hash
            SyncState::Synced,
            mtime, // local_mtime
            mtime, // remote_mtime
            body.len() as i64,
            1,    // version_num
            None, // content_key_version
        )
        .unwrap();
}

// ─────────────────────────────────────────────────────────────────────
// The recorded upload head — the fold must not call our own head stale
// ─────────────────────────────────────────────────────────────────────

/// After a successful upload + record, [`SyncEngine::commit_recorded_head`]
/// stamps the row's `manifest_hash`/`content_key_version` with the RECORDED
/// head — and the next fold, echoing that very record back, must NOT classify
/// the file as a stale hydrated copy.
///
/// The upload path deliberately writes the row with the *pre-upload merge
/// base* (an in-flight record may still fail), but once the record lands, the
/// new manifest IS the nest head — leaving the base in place made every
/// freshly-synced local edit read as "the nest changed under this hydrated
/// copy" and queued it for dehydration on the very next rescan (live
/// 2026-07-17: `stale_hydrated=2` on two just-uploaded files; only the
/// dirty-file dehydrate refusal protected the edits).
#[tokio::test]
async fn a_recorded_upload_head_is_not_stale_on_the_next_fold() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    // The steady state the upload path leaves: Synced row whose manifest is
    // the OLD merge base (the seeded content hash, via seed_tracked_file).
    seed_tracked_file(&engine, watch.path(), "edited.txt", b"fresh edit!");

    // The record succeeded with the NEW manifest; the engine commits it.
    let new_manifest = fauna_core::data::ContentHash::from_digest_raw([0xCD; 32]);
    engine
        .commit_recorded_head("edited.txt", new_manifest, 11, None)
        .expect("commit recorded head");

    let entry = engine.db().get_entry("edited.txt").unwrap().unwrap();
    assert_eq!(
        entry.manifest_hash,
        Some(new_manifest),
        "the recorded head becomes the row's manifest (hydration anchor + merge base)"
    );

    // The next fold echoes our own record back (device exclusion is inert —
    // this module's doc). `create_change` builds manifest [fill; 32], size 11.
    let fold = engine
        .record_placeholders_from_changes(&[create_change(
            71,
            "edited.txt",
            &our_device_hex(),
            0xCD,
        )])
        .expect("fold");
    assert!(
        fold.stale_hydrated.is_empty(),
        "a freshly-recorded local head must never classify as a stale hydrated copy \
         (it would be queued for dehydration against the user's own edit); got {:?}",
        fold.stale_hydrated
    );
}

// ─────────────────────────────────────────────────────────────────────
// The dehydration gate's proven-head sites: freeing a file's bytes is
// lossless only when the recorded head (`manifest_hash`) provably reassembles
// to the current disk content. Each proven-head write path must stamp
// `recorded_content_hash` so `is_dehydration_safe` allows the free-up; the
// record-FAILED path (record_head_commit_wiring_test.rs) must NOT.
// ─────────────────────────────────────────────────────────────────────

/// Record-**success** side (unit level; the connected-nest integration proof is
/// `conformance_sync_engine_record_commit.rs`). Before the record lands the row
/// is `Synced` with `local_hash == disk` but the head is still the OLD base — so
/// `is_dehydration_safe` must refuse; once `commit_recorded_head` stamps the
/// head, freeing is provably lossless and it must allow.
#[tokio::test]
async fn commit_recorded_head_makes_the_file_dehydration_safe() {
    let watch = tempfile::tempdir().unwrap();
    // A full folder: the nest holds the bytes an own record names.
    let engine = test_engine(watch.path().to_path_buf()).with_metadata_only_residency(false);
    let rel = "edited.txt";
    let body = b"fresh edit!";
    std::fs::write(watch.path().join(rel), body).unwrap();
    let content = fauna_core::data::ContentHash::of_raw(body);
    let old_base = fauna_core::data::ContentHash::from_digest_raw([0xAB; 32]);

    // The post-upload / pre-record steady state: `Synced`, `local_hash == disk`,
    // head still the OLD merge base (the record has not landed yet).
    engine
        .db()
        .upsert_entry(
            rel,
            Some(content),
            Some(content),
            Some(old_base),
            SyncState::Synced,
            0,
            0,
            body.len() as i64,
            1,
            None,
        )
        .unwrap();
    assert!(
        !engine.is_dehydration_safe(rel),
        "pre-record: the head is still the OLD base, so freeing is not yet lossless"
    );

    let new_manifest = fauna_core::data::ContentHash::from_digest_raw([0xCD; 32]);
    engine
        .commit_recorded_head(rel, new_manifest, body.len() as i64, None)
        .expect("commit recorded head");

    assert!(
        engine.is_dehydration_safe(rel),
        "post-record: the head now matches the local content, so freeing is \
         provably lossless — commit_recorded_head must stamp recorded_content_hash"
    );
}

// ─────────────────────────────────────────────────────────────────────
// A holder keeps what it wrote (`file-sync.md` § Relay serving): in a
// metadata-only folder the nest took no bytes, so this device's own record
// is no proof the head can be fetched again. The gate reads the folder's
// residency when the body is freed, and how the row's proof was earned.
// ─────────────────────────────────────────────────────────────────────

/// Put `body` at `rel` and record it as this device's own write: the
/// post-upload row, then the recorded head's commit.
pub(crate) fn record_own_write(
    engine: &SyncEngine,
    watch: &std::path::Path,
    rel: &str,
    body: &[u8],
) {
    std::fs::write(watch.join(rel), body).unwrap();
    let content = fauna_core::data::ContentHash::of_raw(body);
    engine
        .db()
        .upsert_entry(
            rel,
            Some(content),
            Some(content),
            None,
            SyncState::Synced,
            0,
            0,
            body.len() as i64,
            1,
            None,
        )
        .unwrap();
    let manifest = fauna_core::data::ContentHash::from_digest_raw([0xCD; 32]);
    engine
        .commit_recorded_head(rel, manifest, body.len() as i64, None)
        .expect("commit recorded head");
}

/// The data-loss path this gate closes: the record landed, so every hash
/// agrees — and the nest holds no byte of the file.
#[tokio::test]
async fn a_metadata_only_folder_keeps_the_body_of_its_own_record() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf()).with_metadata_only_residency(true);
    record_own_write(&engine, watch.path(), "mine.txt", b"the only copy");

    assert!(
        !engine.is_dehydration_safe("mine.txt"),
        "an own record in a metadata-only folder is the only copy — never freed"
    );
    assert!(
        !engine.dehydrate_off_disk("mine.txt").unwrap(),
        "the off-disk dehydrate takes the same gate"
    );
    assert!(
        !SyncEngine::is_dehydration_safe_in(
            engine.db(),
            "mine.txt",
            &watch.path().join("mine.txt")
        ),
        "a caller holding only the state DB reads the persisted residency"
    );
}

/// The other half of the rule: a body fetched from another holder may be
/// freed as before, which returns the device to where it stood.
#[tokio::test]
async fn a_fetched_body_in_a_metadata_only_folder_may_be_freed() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf()).with_metadata_only_residency(true);
    let rel = "theirs.bin";
    let manifest = fauna_core::data::ContentHash::from_digest_raw([0x77; 32]);
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(manifest),
            SyncState::Placeholder,
            0,
            0,
            10,
            1,
            None,
        )
        .unwrap();
    let body = b"real bytes";
    std::fs::write(watch.path().join(rel), body).unwrap();
    engine
        .mark_hydrated(rel, fauna_core::data::ContentHash::of_raw(body))
        .expect("mark_hydrated");

    assert!(
        engine.is_dehydration_safe(rel),
        "a holder elsewhere served these bytes — freeing them loses nothing"
    );

    // The same seat then edits the file and records it: the proof is now its
    // own, and the body is held.
    record_own_write(&engine, watch.path(), rel, b"edited here");
    assert!(
        !engine.is_dehydration_safe(rel),
        "an own record over a fetched body is an own record"
    );
}

/// The gate reads the residency when the body is freed, not when the proof
/// was earned: a folder flipped to metadata-only stops its seats freeing what
/// they wrote while it was full.
#[tokio::test]
async fn a_folder_flipped_to_metadata_only_keeps_what_was_recorded_while_full() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf()).with_metadata_only_residency(false);
    record_own_write(&engine, watch.path(), "mine.txt", b"recorded while full");
    assert!(
        engine.is_dehydration_safe("mine.txt"),
        "a full folder's nest holds the bytes — unchanged"
    );

    // An unreadable folder list keeps the reading; the nest's answer moves it.
    engine.install_residency(None);
    assert!(engine.is_dehydration_safe("mine.txt"));
    engine.install_residency(Some(true));
    assert!(
        !engine.is_dehydration_safe("mine.txt"),
        "the flip reaches a proof earned before it"
    );
}

/// A state DB no engine has written a residency reading into keeps every
/// own-record body: refusing is the safe direction when the folder's
/// residency is unknown.
#[tokio::test]
async fn an_absent_residency_reading_keeps_an_own_record() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    record_own_write(&engine, watch.path(), "mine.txt", b"residency unknown");
    assert_eq!(engine.db().residency_reading().unwrap(), None);

    assert!(!engine.is_dehydration_safe("mine.txt"));
}

/// A rebuild over a state DB that holds a *full* reading, on a binding whose
/// residency is unknown (a cross-nest record no home nest has stamped),
/// clears that reading rather than read a stale *full*: the seat uploads and
/// keeps what it wrote (`file-sync.md` § Relay serving → *A member on another
/// nest*, step (1)).
#[tokio::test]
async fn an_unknown_residency_at_build_clears_a_stale_full_reading() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf()).with_metadata_only_residency(false);
    record_own_write(&engine, watch.path(), "mine.txt", b"recorded while full");
    assert!(engine.is_dehydration_safe("mine.txt"));

    let engine = engine.with_residency_reading(None);
    assert_eq!(engine.db().residency_reading().unwrap(), None);
    assert!(
        !engine.is_metadata_only_residency(),
        "the seat still uploads"
    );
    assert!(
        !engine.is_dehydration_safe("mine.txt"),
        "an unknown residency keeps an own-record body"
    );
}

/// Hydrate-on-open side (the on-demand root's common case): a placeholder the
/// user opens is served by FETCH_DATA and the host calls `mark_hydrated`. That
/// file is now provably the nest's content, so freeing it again must be allowed
/// — `mark_hydrated` must stamp `recorded_content_hash`, not just `local_hash`.
#[tokio::test]
async fn mark_hydrated_makes_the_file_dehydration_safe() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    let rel = "clip.bin";
    let manifest = fauna_core::data::ContentHash::from_digest_raw([0x77; 32]);

    // A cloud-only placeholder row (no local bytes yet).
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(manifest),
            SyncState::Placeholder,
            0,
            0,
            10,
            1,
            None,
        )
        .unwrap();

    // FETCH_DATA materialized the bytes on disk; the host reports the hydrated
    // content identity — a plain file (no cloud reparse point) under test.
    let body = b"real bytes";
    std::fs::write(watch.path().join(rel), body).unwrap();
    let content = fauna_core::data::ContentHash::of_raw(body);
    engine.mark_hydrated(rel, content).expect("mark_hydrated");

    assert!(
        engine.is_dehydration_safe(rel),
        "a hydrated-on-open placeholder is provably the nest's content — freeing \
         it must be allowed; mark_hydrated must stamp recorded_content_hash"
    );
}

/// **The hydration echo the two-way root must NOT make.** Serving a fetch writes
/// bytes into the root, which the watcher sees as an ordinary `Modified` event,
/// so `upload_file` runs over a file we *just downloaded*. The ONLY thing
/// stopping a re-upload is its "already synced, skipping" short-circuit firing
/// once `mark_hydrated` stamped the served content's identity — otherwise every
/// hydrated file re-uploads straight back and bumps a nest version per download.
///
/// The windows-only `cfapi_live_integration::
/// a_local_edit_by_another_process_is_detected_and_uploaded` pins this through a
/// real cfapi root + fs watcher; the merge-gate janitor runs on a non-Windows
/// host and never runs it, so a regression here is invisible off Windows (it
/// was — this pin exists because
/// that live test went red with no headless twin to catch it). This is that
/// twin at the engine seam: drive the **real** populate→hydrate chain and assert
/// `upload_file` honestly skips without reaching the (unreachable) nest.
///
/// It guards a fact `is_dehydration_safe` (the sibling pin above) does **not**:
/// the skip keys on FOUR row facts and `is_dehydration_safe` checks only three —
/// it never inspects `manifest_hash`. A hydrated placeholder whose manifest went
/// missing passes `is_dehydration_safe` yet falls through the skip's
/// `if let Some(recorded_manifest) = entry.manifest_hash` guard to a re-upload.
#[tokio::test]
async fn a_hydrated_placeholder_is_not_re_uploaded() {
    const ORIGINAL: &[u8] = b"hello world"; // 11 bytes — `create_change`'s size
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    let rel = "notes.txt";

    // The on-demand root learns the file from the nest as a bytes-free
    // Placeholder (the real populate path — manifest carried from the change).
    engine
        .record_placeholders_from_changes(&[create_change(1, rel, "peer-device", 0x77)])
        .unwrap();
    let ph = engine.db().get_entry(rel).unwrap().unwrap();
    assert_eq!(ph.state, SyncState::Placeholder);
    assert!(
        ph.manifest_hash.is_some(),
        "a populated placeholder carries the nest head's manifest"
    );

    // FETCH_DATA served the bytes; the host wrote them into the root and called
    // mark_hydrated with the served content's identity (a plain file under test —
    // no cloud reparse point, so `mark_hydrated` takes its fully-local arm).
    std::fs::write(watch.path().join(rel), ORIGINAL).unwrap();
    let served = fauna_core::data::ContentHash::of_raw(ORIGINAL);
    engine.mark_hydrated(rel, served).expect("mark_hydrated");

    // All four facts the skip keys on, set by the populate+hydrate chain.
    let row = engine.db().get_entry(rel).unwrap().unwrap();
    assert_eq!(row.state, SyncState::Synced, "hydrated → Synced");
    assert_eq!(
        row.local_hash,
        Some(served),
        "mark_hydrated stamps the served identity"
    );
    assert_eq!(
        row.recorded_content_hash,
        Some(served),
        "…and the recorded-head proof for exactly these bytes"
    );
    assert!(
        row.manifest_hash.is_some(),
        "the hydration anchor survives — WITHOUT it upload_file's skip guard \
         (`if let Some(recorded_manifest) = entry.manifest_hash`) falls through \
         to a re-upload even though recorded_content_hash proves the head"
    );

    // The watcher's Modified event drives upload_file. It MUST skip: report
    // `recorded` (a lost-ack retry converges to an ack) and never reach the byte
    // plane. A non-skip seals+chunks+POSTs and errors against http://127.0.0.1:9,
    // failing the `.expect` — exactly the windows regression, caught headlessly.
    let outcome = engine
        .upload_file(rel)
        .await
        .expect("a hydrated file must SKIP the upload, not attempt it against the nest");
    assert!(
        outcome.recorded,
        "the skip fires and honestly reports the record already landed for these bytes"
    );
}

/// The guard the commit must keep: when the record FAILED, the row keeps the
/// old base — and the fold, seeing a nest head that genuinely differs from the
/// row, still reports the true stale case (a remote change under a hydrated
/// copy). `commit_recorded_head` must not weaken that.
#[tokio::test]
async fn a_genuinely_moved_nest_head_still_classifies_stale() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    seed_tracked_file(&engine, watch.path(), "moved.txt", b"local bytes");

    // A remote device recorded a different head; no commit happened here.
    let fold = engine
        .record_placeholders_from_changes(&[create_change(72, "moved.txt", "peer-device", 0x77)])
        .expect("fold");
    assert_eq!(
        fold.stale_hydrated.len(),
        1,
        "a nest head that moved under a hydrated copy is the real stale case"
    );
}

// ─────────────────────────────────────────────────────────────────────
// subtree_fully_synced — the folder-✅ predicate
// ─────────────────────────────────────────────────────────────────────

/// A directory flips to the platform ✅ only when its known subtree is fully
/// synced: at least one live row under it, none in-flight/diverged. An empty
/// (unrepresented) directory stays honestly pending — folders carry
/// directories implicitly via child paths, so an empty dir exists on no other
/// device. (Live 2026-07-17: the folder holding a green-checked file kept the
/// sync-pending arrows forever because no folder flip existed at all.)
#[tokio::test]
async fn subtree_fully_synced_requires_a_live_clean_subtree() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    // Unrepresented dir → false (honest pending; the fleet will never see it).
    assert!(!engine.subtree_fully_synced("empty dir").unwrap());

    // One synced child → true.
    seed_tracked_file(&engine, watch.path(), "d/a.txt", b"a");
    assert!(engine.subtree_fully_synced("d").unwrap());

    // A cloud-only placeholder child is synced by definition — still true.
    seed_tracked_file(&engine, watch.path(), "d/p.txt", b"p");
    engine
        .db()
        .update_state("d/p.txt", SyncState::Placeholder)
        .unwrap();
    assert!(engine.subtree_fully_synced("d").unwrap());

    // An in-flight sibling poisons the subtree.
    seed_tracked_file(&engine, watch.path(), "d/b.txt", b"b");
    engine
        .db()
        .update_state("d/b.txt", SyncState::Uploading)
        .unwrap();
    assert!(!engine.subtree_fully_synced("d").unwrap());

    // Prefix discipline: "d"'s in-flight row must not leak onto "dd".
    seed_tracked_file(&engine, watch.path(), "dd/c.txt", b"c");
    assert!(engine.subtree_fully_synced("dd").unwrap());
}

// ─────────────────────────────────────────────────────────────────────
// The UI-originated delete — the regression this module exists for
// ─────────────────────────────────────────────────────────────────────

/// A delete recorded by a **client UI on the folder-owning device** must still
/// remove the local file.
///
/// The Media page's `media-delete-button` records the tombstone under this
/// device's own id but never touches the disk. Skipping the change because the
/// id matches left the file on disk forever while the nest and the explorer both
/// treated it as deleted — a three-way divergence the user cannot see or undo,
/// and permanent, since the anchor advances past the tombstone regardless.
#[tokio::test]
async fn a_self_echoed_delete_removes_a_file_still_on_disk() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    seed_tracked_file(&engine, watch.path(), "from-laptop.txt", b"yadayada");

    // The tombstone the UI recorded, echoed back under OUR device id.
    let changes = vec![delete_change(70, "from-laptop.txt", &our_device_hex())];
    let applied = engine
        .apply_remote_changes(&changes, 69)
        .await
        .unwrap()
        .applied;

    assert!(
        !watch.path().join("from-laptop.txt").exists(),
        "a self-echoed delete must remove the local file: the UI that recorded \
         it never touched the disk, so nothing else will"
    );
    // `delete_entry` is a soft delete: the row survives as a local tombstone
    // (`purge_tombstones` reclaims it later). What matters is that it leaves
    // `Synced` — reconcile's delete-detection only fires on a `Synced` row whose
    // file is missing (`engine.rs`), so a row still claiming `Synced` against a
    // nest that has tombstoned the path is exactly the divergence to avoid.
    let entry = engine
        .db()
        .get_entry("from-laptop.txt")
        .unwrap()
        .expect("the tombstone row survives the soft delete");
    assert_eq!(
        entry.state,
        SyncState::Deleted,
        "the db row must be tombstoned with the file, not left claiming Synced"
    );
    assert_eq!(applied, 1, "the applied delete must be counted");
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        70,
        "the anchor advances past the tombstone (it does so whether or not the \
         delete is applied — which is why a skipped delete is never retried)"
    );
}

/// **A backup-mode engine keeps the file a peer deleted** — the shared-engine
/// half of the guard, so every desktop's per-user agent is covered
/// (`file-sync.md` § 4; `principles.md` § No user-data loss).
///
/// The contrast with `a_self_echoed_delete_removes_a_file_still_on_disk` above is
/// the whole point: that test's tombstone is applied because the mode is `Sync`.
/// Nothing about the *change* differs here — same seq, same path, same peer
/// attribution — only the set's mode does.
#[tokio::test]
async fn a_backup_mode_engine_keeps_a_file_a_peer_deleted() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    engine.set_sync_mode(crate::config::SyncMode::Backup);
    seed_tracked_file(&engine, watch.path(), "important.txt", b"do not lose this");

    let changes = vec![delete_change(70, "important.txt", "peerpeerpeer")];
    let batch = engine.apply_remote_changes(&changes, 69).await.unwrap();

    assert!(
        watch.path().join("important.txt").exists(),
        "a backup-mode engine must keep the file its source deleted"
    );
    assert_eq!(
        std::fs::read(watch.path().join("important.txt")).unwrap(),
        b"do not lose this",
        "the kept file must be untouched, not merely present"
    );
    assert_eq!(
        batch.applied, 0,
        "nothing was applied, so nothing is counted"
    );
    assert!(
        !batch.deferred,
        "a RESOLVED backup decline is deliberate and permanent — accounted for, \
         never deferred"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        70,
        "a RESOLVED backup seat's anchor still advances past the declined \
         tombstone — re-fetching a forever-declined delete is pure waste. This \
         is one half of the § 4 anchor pair; the UNRESOLVED hold below is the \
         other."
    );
}

/// **An UNRESOLVED seat declines a peer's delete AND holds the anchor** — the leg-2 direction, ratified 2026-08-02 (`file-sync.md` § 4;
/// `principles.md` § No user-data loss ranks the two error directions by
/// reversibility). Before it, a failed `members.list` at engine start resolved
/// to `Sync`, applied the delete, and advanced the anchor — no later pass
/// could have saved the files. Now the decline is provisional: the anchor
/// stays put, so the tombstone re-delivers to a seat that has re-resolved
/// (`run_watch_loop` refreshes the mode each tick, before the pull).
#[tokio::test]
async fn an_unresolved_mode_engine_keeps_the_file_and_holds_the_anchor() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    engine.set_sync_mode(crate::config::ModeResolution::Unresolved);
    seed_tracked_file(&engine, watch.path(), "important.txt", b"do not lose this");

    let changes = vec![delete_change(70, "important.txt", "peerpeerpeer")];
    let batch = engine.apply_remote_changes(&changes, 69).await.unwrap();

    assert!(
        watch.path().join("important.txt").exists(),
        "an unresolved seat must not destroy on a guess"
    );
    assert_eq!(batch.applied, 0);
    assert!(
        batch.deferred,
        "a held delete is a DEFERRED pass — the caller must not stamp the \
         device clean/caught-up on it"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        69,
        "the anchor must NOT pass a delete declined for UNKNOWN — holding it \
         is what re-delivers the tombstone once the role is readable, making \
         both error directions recoverable"
    );
}

/// **The hold is PROVISIONAL, and this is the seam that discharges it.** The
/// test above proves the anchor stays put "so the tombstone re-delivers once
/// the role is readable"; nothing proved the re-delivery actually applies.
/// That second half is the whole value of holding rather than declining
/// outright, and it is what the control-plane-connect healing in
/// `always_resident::run_watch_loop` exists to reach: re-resolve, then pull,
/// and the tombstone the unresolved self declined lands.
///
/// Written 2026-08-28 alongside the live fix for the gap on the *trigger*
/// side. `run_watch_loop` resolved the mode at entry and then only on the
/// rescan tick, so a seat whose first resolution raced its own control-plane
/// connect stayed `Unresolved` — and every peer delete was declined for as
/// long as the tick was away. The re-resolve now also rides the `Connected`
/// transition; this pins the contract that makes doing so sufficient.
#[tokio::test]
async fn a_re_resolved_seat_applies_the_tombstone_its_unresolved_self_held() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    // What the nest last said. The engine's clients point at an unreachable
    // URL and its control plane was never connected, so `refresh_sync_mode`
    // below takes exactly the failed-reads path and the cached answer governs
    // — the same shape as the live heal, where the reads succeed instead.
    engine.db().set_cached_sync_mode("sync").unwrap();
    engine.set_sync_mode(crate::config::ModeResolution::Unresolved);
    seed_tracked_file(&engine, watch.path(), "important.txt", b"do not lose this");

    let changes = vec![delete_change(70, "important.txt", "peerpeerpeer")];

    // 1. Unresolved: the tombstone is declined and the anchor is HELD at it.
    let held = engine.apply_remote_changes(&changes, 69).await.unwrap();
    assert_eq!(held.applied, 0);
    assert!(held.deferred);
    assert!(
        watch.path().join("important.txt").exists(),
        "an unresolved seat must not destroy on a guess"
    );
    assert_eq!(engine.db().get_anchor().unwrap(), 69);

    // 2. The role becomes readable. In production this is the control-plane
    //    connect (or the rescan tick); here the cached answer stands in for
    //    the successful read, which is the same input to the same resolver.
    engine.refresh_sync_mode().await;
    assert_eq!(
        engine.sync_mode_resolution(),
        crate::config::ModeResolution::Resolved(crate::config::SyncMode::Sync),
        "the seat must leave Unresolved once an authoritative answer exists"
    );

    // 3. The SAME tombstone re-delivers — the anchor never passed it — and now
    //    applies. This is the assertion the hold's whole design rests on.
    let applied = engine.apply_remote_changes(&changes, 69).await.unwrap();
    assert_eq!(
        applied.applied, 1,
        "the re-resolved seat applies what it held"
    );
    assert!(
        !watch.path().join("important.txt").exists(),
        "the held tombstone must reach the disk on re-delivery, or holding it \
         merely postponed losing the delete forever"
    );
    assert!(!applied.deferred, "nothing is held any more");
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        70,
        "and the anchor may finally pass the tombstone it applied"
    );
}

/// The unresolved hold uses the same transient-class cap as the sealed-path
/// degrade: everything at and past the first held delete defers to a later
/// pull (which re-resolves first), so the anchor cannot leapfrog the held
/// tombstone via a later change's seq.
#[tokio::test]
async fn a_held_delete_defers_the_batch_tail_too() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    engine.set_sync_mode(crate::config::ModeResolution::Unresolved);
    seed_tracked_file(&engine, watch.path(), "a.txt", b"first");
    seed_tracked_file(&engine, watch.path(), "b.txt", b"second");

    let changes = vec![
        delete_change(70, "a.txt", "peerpeerpeer"),
        delete_change(72, "b.txt", "peerpeerpeer"),
    ];
    let batch = engine.apply_remote_changes(&changes, 69).await.unwrap();

    assert!(watch.path().join("a.txt").exists());
    assert!(watch.path().join("b.txt").exists());
    assert!(batch.deferred);
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        69,
        "the cap is the FIRST held seq — a later change in the same batch must \
         not carry the anchor past the held tombstone"
    );
}

/// **The persisted last-authoritative answer arms the guard when the nest is
/// unreadable** — the leg-2 memory half (contract item 3): a seat
/// whose stored role is `backup` and whose reads fail keeps a peer-deleted
/// file, with the anchor ADVANCING (it is a resolved backup decline, not an
/// unresolved hold). The engine's clients here point at an unreachable URL
/// and its control plane was never connected, so `refresh_sync_mode` takes
/// exactly the failed-reads path a `members.list`-down engine start takes.
#[tokio::test]
async fn a_cached_backup_answer_arms_the_guard_when_the_nest_is_unreadable() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    engine.db().set_cached_sync_mode("backup").unwrap();

    engine.refresh_sync_mode().await;
    assert_eq!(
        engine.sync_mode_resolution(),
        crate::config::ModeResolution::Resolved(crate::config::SyncMode::Backup),
        "a failed read must re-arm from what the nest last said, not from the \
         delete-applying default"
    );

    seed_tracked_file(&engine, watch.path(), "important.txt", b"do not lose this");
    let batch = engine
        .apply_remote_changes(&[delete_change(70, "important.txt", "peerpeerpeer")], 69)
        .await
        .unwrap();

    assert!(watch.path().join("important.txt").exists());
    assert!(!batch.deferred);
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        70,
        "a cache-armed backup seat declines with the anchor advancing — the \
         resolved posture, not the unresolved hold"
    );
}

/// A never-answered seat with no cache resolves UNRESOLVED on a refresh
/// against an unreadable nest — the state the two tests above pivot on.
#[tokio::test]
async fn a_refresh_with_no_cache_and_no_nest_is_unresolved() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    engine.refresh_sync_mode().await;
    assert_eq!(
        engine.sync_mode_resolution(),
        crate::config::ModeResolution::Unresolved
    );
    assert!(!engine.applies_remote_deletes());
}

/// The mode is *correctable* after construction because some hosts only learn the
/// authoritative `folders` row once the engine's control plane is open (the
/// per-user agent). Pin both directions, since a one-way setter would strand a
/// set that flipped back to sync mode.
#[test]
fn the_sync_mode_is_correctable_after_construction() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    assert!(
        engine.applies_remote_deletes(),
        "the constructor value governs until a row corrects it"
    );
    engine.set_sync_mode(crate::config::SyncMode::Backup);
    assert!(!engine.applies_remote_deletes());
    engine.set_sync_mode(crate::config::SyncMode::Sync);
    assert!(engine.applies_remote_deletes());
}

/// The engine's **own** delete stays a no-op: `handle_delete` removed the file
/// and its db row before recording the tombstone, so the echo finds nothing.
/// This is the case the self-echo skip was written for — the disk check covers
/// it without the skip.
#[tokio::test]
async fn an_engine_originated_delete_echo_is_a_no_op() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    // No file on disk, no db row: `handle_delete` already ran.

    let changes = vec![delete_change(70, "gone.txt", &our_device_hex())];
    let applied = engine
        .apply_remote_changes(&changes, 69)
        .await
        .unwrap()
        .applied;

    assert_eq!(
        applied, 0,
        "an already-applied delete must not be counted as work"
    );
    assert_eq!(engine.db().get_anchor().unwrap(), 70);
}

/// A **peer's** delete still removes the local file — the pre-existing
/// behaviour, pinned so the fix cannot regress it.
#[tokio::test]
async fn a_peer_delete_removes_the_local_file() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    seed_tracked_file(&engine, watch.path(), "shared.txt", b"hello");

    let changes = vec![delete_change(70, "shared.txt", &"bb".repeat(32))];
    let applied = engine
        .apply_remote_changes(&changes, 69)
        .await
        .unwrap()
        .applied;

    assert!(!watch.path().join("shared.txt").exists());
    assert_eq!(applied, 1);
}

// ─────────────────────────────────────────────────────────────────────
// The batch-latest guard — the hazard the fix must not introduce
// ─────────────────────────────────────────────────────────────────────

/// A delete **superseded within the same batch** by a later create must not
/// erase the recreated file.
///
/// Deleting a file and immediately recreating it at the same path records
/// `delete@N` then `create@N+1`; both echo back in one batch. Applying the
/// delete on its own would erase the *new* file, and the self-echoed create
/// only updates the merge base (it downloads nothing), so the bytes would be
/// gone. The batch-latest fold is what makes the delete arm safe to run on
/// self-echoes at all.
#[tokio::test]
async fn a_same_batch_delete_then_recreate_keeps_the_recreated_file() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    seed_tracked_file(&engine, watch.path(), "notes.txt", b"the recreated body");

    let me = our_device_hex();
    let changes = vec![
        delete_change(70, "notes.txt", &me),
        create_change(71, "notes.txt", &me, 0xCD),
    ];
    engine.apply_remote_changes(&changes, 69).await.unwrap();

    assert!(
        watch.path().join("notes.txt").exists(),
        "the delete is superseded by a later create in the same batch, so the \
         recreated file must survive"
    );
    assert_eq!(
        std::fs::read(watch.path().join("notes.txt")).unwrap(),
        b"the recreated body",
        "the recreated bytes must be untouched"
    );
    assert_eq!(engine.db().get_anchor().unwrap(), 71);
}

/// The fold is order-insensitive: the log's order must not decide the outcome,
/// only the seqs. Same batch as above with the changes reversed.
#[tokio::test]
async fn the_batch_latest_fold_ignores_log_order() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    seed_tracked_file(&engine, watch.path(), "notes.txt", b"the recreated body");

    let me = our_device_hex();
    let changes = vec![
        create_change(71, "notes.txt", &me, 0xCD),
        delete_change(70, "notes.txt", &me),
    ];
    engine.apply_remote_changes(&changes, 69).await.unwrap();

    assert!(
        watch.path().join("notes.txt").exists(),
        "the higher-seq create wins regardless of the order the log lists them in"
    );
}

/// A delete that is a path's batch-latest change still applies even when the
/// same batch carries an *earlier* create for it — the inverse of the guard
/// above, so the fold cannot be read as "any batch with a create is immune".
#[tokio::test]
async fn a_create_then_delete_in_one_batch_still_deletes() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    seed_tracked_file(&engine, watch.path(), "transient.txt", b"short-lived");

    let me = our_device_hex();
    let changes = vec![
        create_change(70, "transient.txt", &me, 0xCD),
        delete_change(71, "transient.txt", &me),
    ];
    engine.apply_remote_changes(&changes, 69).await.unwrap();

    assert!(
        !watch.path().join("transient.txt").exists(),
        "the delete is the batch-latest change, so it applies"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The fold itself
// ─────────────────────────────────────────────────────────────────────

#[test]
fn batch_latest_seq_by_path_takes_the_highest_seq_per_path() {
    let me = our_device_hex();
    let changes = vec![
        create_change(1, "a.txt", &me, 0x01),
        delete_change(9, "a.txt", &me),
        create_change(4, "b.txt", &me, 0x02),
    ];
    let latest = SyncEngine::batch_latest_seq_by_path(&changes);

    assert_eq!(latest.get("a.txt"), Some(&9));
    assert_eq!(latest.get("b.txt"), Some(&4));
    assert_eq!(latest.len(), 2);
}

// ─────────────────────────────────────────────────────────────────────
// Delete-then-CREATE across batches — the tombstone's subject is the row,
// not the path. `exists()` alone answers "is something here?", never "is
// this the file the tombstone refers to?", so a file created at a
// tombstoned path was destroyed and (having no row) never re-uploaded.
// Found live 2026-07-24: the multiseat linux seat wrote its phase-1 files
// into a folder bound to a set whose history held add-then-delete for those
// exact paths; the replay deleted them and `new=0` followed.
// ─────────────────────────────────────────────────────────────────────

/// A remote delete must not destroy a local file the engine does not track.
///
/// The batch-latest guard covers delete-then-recreate *within one batch*; this
/// is the across-batch case, where the tombstone is legitimately the path's
/// latest remote state and the local file is simply a **different, newer file**
/// the engine has never synced. Applying the tombstone there erases user data
/// that exists nowhere else — and because the row is absent, reconcile's
/// delete-detection never queues an upload either, so the bytes are just gone.
///
/// Declining is safe precisely because the file is untracked: § Files Appear
/// Automatically's "a skipped delete never comes back" hazard is about a
/// *tracked* file stranded against a nest that shows it deleted. An untracked
/// file has no such divergence to strand — reconcile sees an unknown local file
/// and uploads it as a fresh create at a higher seq, which is how every device
/// converges on "the file exists".
#[tokio::test]
async fn a_remote_delete_does_not_destroy_an_untracked_local_file() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());

    // On disk, but never synced — no row. (A user's brand-new file, or a test
    // seat's phase-1 write into a freshly bound folder.)
    let full = watch.path().join("notes.txt");
    std::fs::write(&full, b"the user just wrote this").unwrap();
    assert!(engine.db().get_entry("notes.txt").unwrap().is_none());

    let changes = vec![delete_change(70, "notes.txt", &"bb".repeat(32))];
    let applied = engine
        .apply_remote_changes(&changes, 69)
        .await
        .unwrap()
        .applied;

    assert!(
        full.exists(),
        "a tombstone for a path this engine never tracked must not delete the \
         local file: it is user data that exists nowhere else, and with no row \
         reconcile will not re-upload it either"
    );
    assert_eq!(
        std::fs::read(&full).unwrap(),
        b"the user just wrote this",
        "the untracked file's bytes must be untouched"
    );
    assert_eq!(applied, 0, "declining the delete applies no change");
}

/// A file **recreated after** a tombstone must survive a later replay of it.
///
/// `delete_entry` soft-deletes (`state = Deleted`), so the row outlives the
/// file. A recreation at that path is a new local file sitting on a tombstoned
/// row — and a row-exists test alone would happily delete it on the next pull
/// that carries the old tombstone (a full-history replay after a rebind carries
/// every tombstone the set ever recorded). This is the same "another device may
/// have since recreated it" hazard § Files Appear Automatically names for the
/// re-record loop, reached by a different route.
#[tokio::test]
async fn a_remote_delete_does_not_destroy_a_file_recreated_after_a_tombstone() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    seed_tracked_file(&engine, watch.path(), "notes.txt", b"the original");

    // The delete lands: file gone, row soft-deleted to a tombstone.
    let changes = vec![delete_change(70, "notes.txt", &"bb".repeat(32))];
    engine.apply_remote_changes(&changes, 69).await.unwrap();
    let full = watch.path().join("notes.txt");
    assert!(!full.exists(), "precondition: the original was deleted");
    assert_eq!(
        engine.db().get_entry("notes.txt").unwrap().unwrap().state,
        SyncState::Deleted,
        "precondition: the row survives as a tombstone"
    );

    // The user creates a NEW file at that path.
    std::fs::write(&full, b"a brand new note").unwrap();

    // A later pull replays the same tombstone (a rebind replays all history).
    let replay = vec![delete_change(71, "notes.txt", &"bb".repeat(32))];
    let applied = engine
        .apply_remote_changes(&replay, 70)
        .await
        .unwrap()
        .applied;

    assert!(
        full.exists(),
        "a tombstoned row means the recorded delete already landed; a file at \
         that path now is a RECREATION and must not be erased by replaying it"
    );
    assert_eq!(std::fs::read(&full).unwrap(), b"a brand new note");
    assert_eq!(applied, 0, "declining the delete applies no change");
}

/// A remote delete must not destroy local bytes the nest has never seen, even
/// when the row is live.
///
/// The upload path writes its row **before** the upload lands (`engine.rs`,
/// "Mark as uploading": `local_hash` = the new bytes, `manifest_hash` = the OLD
/// base). So a file recreated at a tombstoned path has a *live* `Uploading` row
/// for the whole in-flight window, and a tracked-ness test alone would delete
/// it — the narrow race left open when the row condition first landed.
///
/// The row's STATE carries the answer: `Uploading` means the bytes are still
/// in flight, so no tombstone can be referring to them. A hash comparison
/// cannot decide this — a mid-upload row's `local_hash` is precisely the hash
/// of the bytes on disk — which is why the arm gates on the state first.
#[tokio::test]
async fn a_remote_delete_does_not_destroy_unsynced_local_bytes_on_a_live_row() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    let full = watch.path().join("notes.txt");
    std::fs::write(&full, b"brand new, mid-upload").unwrap();

    // Exactly what upload_file leaves in the in-flight window: live row, local
    // hash = the new bytes, manifest = the old base the tombstone refers to.
    //
    // `local_hash` must be the REAL hash of the bytes on disk, because that is
    // what "Mark as uploading" records. A fake constant here would let the arm
    // decline for the wrong reason (disk-vs-row mismatch) and hide the fact
    // that a mid-upload row's local identity genuinely DOES match its disk —
    // which is why the arm has to discriminate on the row STATE, not on a hash.
    let new_bytes = fauna_core::chunker_stream::content_hash_streaming(&full).unwrap();
    let old_base = fauna_core::data::ContentHash::from_digest_raw([0xAB; 32]);
    engine
        .db()
        .upsert_entry(
            "notes.txt",
            Some(new_bytes),
            None,
            Some(old_base),
            SyncState::Uploading,
            0,
            0,
            21,
            1,
            None,
        )
        .unwrap();

    let changes = vec![delete_change(70, "notes.txt", &"bb".repeat(32))];
    let applied = engine
        .apply_remote_changes(&changes, 69)
        .await
        .unwrap()
        .applied;

    assert!(
        full.exists(),
        "the row is live but its bytes diverge from the recorded head — they were \
         never synced, so the tombstone cannot be referring to them"
    );
    assert_eq!(std::fs::read(&full).unwrap(), b"brand new, mid-upload");
    assert_eq!(applied, 0);
}

/// A remote delete must not trust the DB row over the DISK: `local_hash` refreshes only when `upload_file` reaches
/// "Mark as uploading", and a locally edited file sits behind the watcher
/// debouncer first — whose timer RESETS on every event, so a continuously
/// written file stays unobserved for the whole write, not 2 s. The nudge-path
/// pull deliberately runs no converge before applying. In that window the row
/// still reads Synced with matching hashes while the disk holds bytes that
/// exist nowhere else; deciding from the row alone bare-unlinks them with no
/// conflict marker and no local trace (`principles.md` § No user-data loss).
/// The disk itself must be re-hashed before the unlink: divergence from
/// `local_hash` means unsynced content, which no tombstone can refer to.
#[tokio::test]
async fn a_remote_delete_does_not_destroy_a_mid_debounce_edit_on_a_synced_row() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    // A genuinely synced file: row Synced, local_hash == manifest_hash == the
    // real hash of the bytes on disk (what reconcile + a completed upload leave).
    seed_tracked_file(&engine, watch.path(), "notes.txt", b"synced v1");
    let full = watch.path().join("notes.txt");

    // The user edits it. The debouncer is holding, so no scan or upload has
    // run: the ROW is untouched and still claims the v1 state.
    std::fs::write(&full, b"the unsynced edit").unwrap();

    // A collaborator's delete arrives (the nest nudges near-instantly).
    let changes = vec![delete_change(80, "notes.txt", &"bb".repeat(32))];
    let applied = engine
        .apply_remote_changes(&changes, 79)
        .await
        .unwrap()
        .applied;

    assert!(
        full.exists(),
        "the row says Synced but the DISK diverges from local_hash — the edit \
         was never synced, so the tombstone cannot be referring to it and \
         unlinking destroys its only copy"
    );
    assert_eq!(std::fs::read(&full).unwrap(), b"the unsynced edit");
    assert_eq!(applied, 0, "declining the delete applies no change");
}

/// A remote delete MUST still apply to a file this device obtained by
/// **download** — the ordinary collaborative case.
///
/// `download_file_bytes` records `local_hash` = the content hash of the bytes
/// it wrote and `manifest_hash` = the **manifest identity**, and those are two
/// different values: engine.rs stamps `manifest_hash = ContentHash::of_raw(
/// &manifest_bytes)` (the hash of the manifest, not of the file), its own
/// comment reads "`entry_manifest` reassembles to `write_hash`", and
/// `db.rs::clear_recorded_content_hash` says the fallback `manifest_hash`
/// "differs from what the OS wrote".
///
/// Every other test in this module seeds through `seed_tracked_file`, which
/// sets `local_hash == remote_hash == manifest_hash ==` the content hash. That
/// shape is convenient but it is NOT what a download leaves, so it cannot
/// exercise the delete arm's `local == head` guard the way production does.
/// This test seeds the real downloaded-row shape.
#[tokio::test]
async fn a_remote_delete_applies_to_a_downloaded_file_whose_manifest_differs_from_its_content() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    let full = watch.path().join("notes.txt");
    std::fs::write(&full, b"downloaded from a peer").unwrap();

    // Exactly what `download_file_bytes` writes: the content hash as the local
    // identity, the manifest's own identity as the hydration anchor.
    let content = fauna_core::chunker_stream::content_hash_streaming(&full).unwrap();
    let manifest = fauna_core::data::ContentHash::of_raw(b"the manifest bytes, not the content");
    assert_ne!(
        content, manifest,
        "precondition: a manifest identity is not its content's hash"
    );
    let mtime = std::fs::metadata(&full)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    engine
        .db()
        .upsert_entry(
            "notes.txt",
            Some(content),  // local_hash: the bytes on disk
            Some(content),  // remote_hash
            Some(manifest), // manifest_hash: the reassembly anchor
            SyncState::Synced,
            mtime,
            mtime,
            b"downloaded from a peer".len() as i64,
            1,
            None,
        )
        .unwrap();

    // A collaborator deletes it.
    let changes = vec![delete_change(90, "notes.txt", &"bb".repeat(32))];
    let applied = engine
        .apply_remote_changes(&changes, 89)
        .await
        .unwrap()
        .applied;

    assert!(
        !full.exists(),
        "the row is genuinely synced and the disk still holds exactly the \
         downloaded bytes — the tombstone refers to precisely these bytes, so \
         the delete must apply. Declining here strands the file on every device \
         that obtained it by download, and reconcile then re-uploads it as a new \
         create, resurrecting it fleet-wide."
    );
    assert_eq!(applied, 1, "the delete was applied");
}

/// A remote delete must not destroy an edit on a row that carries a head but
/// **no local identity** — the placeholder lineage.
///
/// `record_placeholders_from_changes` writes exactly that shape for every
/// non-empty on-demand file (`local_hash = None`, `manifest_hash = Some`), and
/// `mark_hydrated`'s two early returns (a partial hydration, or a file that
/// went away again) set the row `Synced` while leaving `local_hash` `None`.
/// Reconcile then flips a locally edited file to `LocallyModified` **without**
/// writing `local_hash` (it only calls `update_state`), so the row can reach
/// the delete arm holding unsynced bytes and no local identity at all.
///
/// The arm used to answer that shape with a blanket "tracked → delete it",
/// which bare-unlinked the edit (`principles.md` § No user-data loss). With no
/// local identity there is nothing to compare the disk against, so the only
/// sound answer is to keep the bytes; a genuine cloud placeholder — which
/// holds no local bytes to lose — is the one case that still applies.
#[tokio::test]
async fn a_remote_delete_does_not_destroy_an_edit_on_a_row_with_no_local_identity() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    let full = watch.path().join("notes.txt");
    std::fs::write(&full, b"the edit that only exists here").unwrap();

    // The placeholder lineage: a hydration anchor, but no local identity was
    // ever stamped. Then the user edited the materialized file.
    let manifest = fauna_core::data::ContentHash::of_raw(b"the manifest bytes");
    engine
        .db()
        .upsert_entry(
            "notes.txt",
            None,           // local_hash: never stamped
            None,           // remote_hash
            Some(manifest), // manifest_hash: the hydration anchor
            SyncState::LocallyModified,
            0,
            0,
            b"the edit that only exists here".len() as i64,
            1,
            None,
        )
        .unwrap();

    let changes = vec![delete_change(95, "notes.txt", &"bb".repeat(32))];
    let applied = engine
        .apply_remote_changes(&changes, 94)
        .await
        .unwrap()
        .applied;

    assert!(
        full.exists(),
        "the row carries no local identity, so nothing proves the bytes on \
         disk are the synced ones — unlinking them destroys an edit that \
         exists nowhere else"
    );
    assert_eq!(
        std::fs::read(&full).unwrap(),
        b"the edit that only exists here"
    );
    assert_eq!(applied, 0, "declining the delete applies no change");
}

// ─────────────────────────────────────────────────────────────────────
// A declined delete is a RESOLVED CONFLICT, not a silent event
// (file-sync.md § Conflicts, delete-vs-edit — ratified 2026-07-29)
// ─────────────────────────────────────────────────────────────────────

/// A delete declined in favor of locally modified content must be RECORDED as
/// a conflict, never silently swallowed — otherwise the survivor's re-upload
/// makes the file "reappear" on the deleting device with no explanation
/// (file-sync.md § Conflicts: every conflict auto-resolves onto the review
/// list; a declined delete is the delete-vs-edit conflict, and the losing
/// version — the tombstone — is already retained as a recorded change).
///
/// This engine's clients are unreachable, so the WS report + the winner upload
/// both fail here; the pin asserts the LOCAL fallback record (the same degrade
/// ladder `auto_resolve_conflict` uses: a failed report leaves the conflict in
/// the local `SyncDb`). The reported type is `delete_declined`.
#[tokio::test]
async fn a_declined_delete_records_a_conflict_for_the_review_list() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    // The mid-debounce shape: row Synced, disk holds an unobserved edit.
    seed_tracked_file(&engine, watch.path(), "notes.txt", b"synced v1");
    let full = watch.path().join("notes.txt");
    std::fs::write(&full, b"the unsynced edit").unwrap();

    let changes = vec![delete_change(80, "notes.txt", &"bb".repeat(32))];
    let applied = engine
        .apply_remote_changes(&changes, 79)
        .await
        .unwrap()
        .applied;

    assert!(full.exists(), "precondition: the delete was declined");
    assert_eq!(applied, 0);
    let conflicts = engine.db().list_unresolved_conflicts().unwrap();
    assert!(
        conflicts
            .iter()
            .any(|(_, path, kind, _, _)| path == "notes.txt" && kind == "delete_declined"),
        "declining a delete over locally modified content must record a \
         `delete_declined` conflict (locally, when the nest is unreachable) so \
         the user learns their delete was overruled; got {conflicts:?}"
    );
}

/// The inverse boundary: an UNTRACKED file surviving a tombstone is not a
/// conflict — there is no shared lineage for the tombstone to lose to (a fresh
/// bind replaying a set's history meets this constantly), so no row is
/// recorded and the decline stays silent.
#[tokio::test]
async fn an_untracked_decline_is_not_a_conflict() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    let full = watch.path().join("notes.txt");
    std::fs::write(&full, b"never synced").unwrap();

    let changes = vec![delete_change(70, "notes.txt", &"bb".repeat(32))];
    engine.apply_remote_changes(&changes, 69).await.unwrap();

    assert!(full.exists());
    assert!(
        engine.db().list_unresolved_conflicts().unwrap().is_empty(),
        "an untracked decline has no shared lineage — reporting it as a \
         conflict would be noise on every fresh bind"
    );
}

/// A file recreated after its tombstone already applied is not a conflict
/// either: the delete was honored (the row is `Deleted`), and the replayed
/// tombstone declining against the RECREATION is ordinary history replay, not
/// an overruled delete. Reporting here would flag every rebind-replay.
#[tokio::test]
async fn a_recreation_after_an_applied_tombstone_is_not_a_conflict() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    seed_tracked_file(&engine, watch.path(), "notes.txt", b"the original");

    // The delete lands for real (row → tombstone), then the user recreates.
    engine
        .apply_remote_changes(&[delete_change(70, "notes.txt", &"bb".repeat(32))], 69)
        .await
        .unwrap();
    let full = watch.path().join("notes.txt");
    assert!(!full.exists(), "precondition: the original was deleted");
    std::fs::write(&full, b"a brand new note").unwrap();

    // A rebind-style replay of the same tombstone declines against the new file.
    engine
        .apply_remote_changes(&[delete_change(71, "notes.txt", &"bb".repeat(32))], 70)
        .await
        .unwrap();

    assert!(full.exists());
    assert!(
        engine.db().list_unresolved_conflicts().unwrap().is_empty(),
        "the tombstone already applied once — declining its replay against a \
         recreation is not an overruled delete"
    );
}

// ---- the fold evidence bound (3) reads (`crate::succession_drain`) ----

/// A pass that DEFERRED must stamp nothing. The deferred change may itself be a
/// predecessor-sealed label that did not open — i.e. the very evidence that the
/// retired owner keys are still needed — so counting that pass as a completed
/// fold would let the thing the keys are *for* count as proof they are not.
#[tokio::test]
async fn a_deferred_pass_leaves_no_fold_evidence() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    engine.set_sync_mode(crate::config::ModeResolution::Unresolved);
    seed_tracked_file(&engine, watch.path(), "important.txt", b"do not lose this");

    let batch = engine
        .apply_remote_changes(&[delete_change(70, "important.txt", "peerpeerpeer")], 69)
        .await
        .unwrap();
    assert!(batch.deferred, "precondition: an unresolved seat defers");

    engine.note_pull_outcome(batch.deferred);
    assert!(
        !engine.db().has_succession_fold_evidence().unwrap(),
        "a deferred pass did not drain the feed and proves nothing about it"
    );
}

/// The other direction, so the pin above cannot pass by never stamping at all.
#[tokio::test]
async fn a_non_deferred_pass_stamps_fold_evidence() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    assert!(!engine.db().has_succession_fold_evidence().unwrap());

    engine.note_pull_outcome(false);
    assert!(engine.db().has_succession_fold_evidence().unwrap());
}

/// The fold evidence is half of the drain observable, and the sentinel identity
/// is the other guard: evidence alone, under an identity the DB never adopted,
/// is not a drain (`crate::succession_drain` ruling 5's read half).
#[tokio::test]
async fn fold_evidence_alone_is_not_a_drain() {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(watch.path().to_path_buf());
    engine.note_pull_outcome(false);

    let actor = engine.owner_actor_id_hex();
    assert!(
        !engine.db().reseal_drain(&actor).unwrap().drained(),
        "no sentinel identity adopted yet ⇒ not drained"
    );

    engine.adopt_sentinel_root_generation();
    assert!(
        engine.db().reseal_drain(&actor).unwrap().drained(),
        "adopted, folded, and nothing owed ⇒ drained"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The accepts gate (folders re-model § Places, phase 2 slice c)
// ─────────────────────────────────────────────────────────────────────

/// A source-only seat (`accepts: false`) skips the pull WITHOUT touching the
/// network: against this module's unreachable client, an attempted fetch
/// errors — so `Ok(0)` here is proof the gate fired before the fetch. The
/// anchor stays put by the same token (nothing advanced it), which is what
/// lets a later flag flip catch the seat up from exactly where delivery
/// stopped.
#[tokio::test]
async fn a_non_accepting_seat_skips_the_pull_before_any_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());

    engine.set_accepts_remote(false);
    let applied = engine
        .pull_remote_changes()
        .await
        .expect("the gate returns cleanly instead of failing the fetch");
    assert_eq!(applied, 0);

    // The other direction, pinned in the same breath: an accepting seat DOES
    // reach the fetch — which against the unreachable client is an error.
    // This is what keeps the gate from ever silently widening.
    engine.set_accepts_remote(true);
    assert!(
        engine.pull_remote_changes().await.is_err(),
        "an accepting seat must attempt the fetch (unreachable here, so Err)"
    );
}

/// The placeholder-population rail (the on-demand hosts' delivery door) is
/// gated by the same flag — a placeholder IS a remote change landing on the
/// seat, just without its bytes.
#[tokio::test]
async fn a_non_accepting_seat_skips_placeholder_population_too() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());

    engine.set_accepts_remote(false);
    let fold = engine
        .populate_placeholders_from_nest()
        .await
        .expect("the gate returns cleanly instead of failing the fetch");
    assert_eq!(fold.recorded, 0);
    assert!(fold.stale_hydrated.is_empty());

    engine.set_accepts_remote(true);
    assert!(
        engine.populate_placeholders_from_nest().await.is_err(),
        "an accepting seat must attempt the fetch (unreachable here, so Err)"
    );
}

/// The default posture is ACCEPT — every seat's behavior before the flag
/// became real. A fresh engine that never resolved its seat must reach the
/// fetch, not silently stop delivering.
#[tokio::test]
async fn the_default_posture_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    assert!(engine.accepts_remote_changes());
    assert!(
        engine.pull_remote_changes().await.is_err(),
        "the default must attempt the fetch (unreachable here, so Err)"
    );
}
