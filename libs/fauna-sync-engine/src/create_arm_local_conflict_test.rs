//! Re-verifies a residue claim carried forward
//! (originally raised by a 2026-07-24 review): *"the create/modify arm
//! downloads a peer change unconditionally when that change is batch-latest,
//! with no compare against locally-newer content."*
//!
//! That claim read [`SyncEngine::apply_remote_changes`]'s match arm, which does
//! call [`SyncEngine::download_and_write_file`] unconditionally for a
//! batch-latest peer create/modify — but the callee is not itself
//! unconditional: since 2026-07-10 ("engine auto-resolve
//! integration"), it reads the local pre-image and, whenever the local file
//! diverges from both the incoming bytes and the cached merge base, routes
//! through [`crate::conflict_resolver::resolve_conflict`] (mtime
//! latest-writer-wins for non-text/no-base content) before ever touching the
//! path (`engine.rs` — the `resolved_apply` match: `WriteMerged` writes merged
//! bytes, `KeepLocal` writes nothing, `Unresolved` writes nothing; only
//! `ApplyIncoming` — local absent, or local already equals the incoming or the
//! cached base — writes the incoming bytes). That predates the 2026-07-24
//! claim by two weeks.
//!
//! This module drives that callee for real (stateful wiremock nest — the same
//! shape `download_file_bytes_test.rs` uses — because the conflict-detection
//! code path is only reached once `fetch_manifest`/`fetch_decoded_chunks`
//! resolve, which needs a server to answer them) and asserts the local file
//! is byte-for-byte unchanged after a batch-latest peer create arrives for a
//! path a genuinely newer local file already occupies. The scenario is
//! deliberately the LEAD's "split-across-incremental-pulls" framing at its
//! most literal: nothing supersedes the incoming change within its own batch
//! (so the `batch_latest_seq` fold — cannot help; it only skips
//! changes superseded *within* the fetched batch), yet content must still
//! survive.
//!
//! `report_conflict_ws` (the WS-RPC conflict report) has no test double in
//! this crate, so resolution here always lands on `Unresolved` (local kept,
//! nothing recorded) rather than `KeepLocal` (local kept, re-pointed to
//! `Synced` as the new head) — both leave the path's bytes untouched, which is
//! the whole of what this module verifies. Distinguishing them needs a WS-RPC
//! double this crate does not have.
//!
//! **Second subject (added 2026-08-02): the fold must not let a SELF-ECHO
//! suppress a peer's concurrent change.** The arm consults a per-path
//! batch-latest seq before downloading anything, and the fold that produced it
//! originally counted *every* row — including the device's own echo. A device
//! that publishes an edit and then pulls a batch holding both its own echo and
//! a peer's genuinely concurrent edit therefore skipped the peer row as
//! "superseded", even though a self-echo writes nothing at all (it only
//! re-bases — [`SyncEngine::apply_self_echo`]). The anchor advances past a
//! skipped change either way, so the peer's edit was dropped **permanently and
//! silently** — no download, no conflict row, no error. Measured on
//! `tests/e2e-unified/tests/test_filesync_twoseat.py` leg 4, 2026-08-02: two
//! app seats editing 80 ms apart converged on one seat's bytes with the other's
//! edit gone (`file-sync.md` § Conflicts requires a three-way merge here).
//! The two tests below are the pin and its control.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::sync::SyncChange;
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::db::SyncDb;
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

// ─────────────────────────────────────────────────────────────────────
// Helpers — trimmed re-derivation of download_file_bytes_test.rs's harness
// (that module's helpers are private to it, so this mirrors rather than
// imports; keeping a WS-RPC-reachable `nest_client` is the only material
// difference, and it still fails closed since the mock serves plain HTTP).
// ─────────────────────────────────────────────────────────────────────

fn test_sync_engine(
    server_uri: &str,
    watch_dir: std::path::PathBuf,
    device_id: [u8; 32],
) -> SyncEngine {
    let kp = ActorKeypair::generate();
    let http = reqwest::Client::new();
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server_uri.to_string(),
        kp,
        bearer,
        http,
    ));
    let client = SyncClient::new(auth, &device_id);
    let db = SyncDb::open_in_memory().unwrap();
    let nest_client =
        fauna_client::NestClient::new(server_uri.to_string(), ActorKeypair::generate());
    let transfer_pool = TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None);
    SyncEngine::new(
        watch_dir,
        db,
        client,
        Some("__test".to_string()),
        device_id,
        None,                                             // mls
        None,                                             // epoch_secret
        Some(BackupKey::from_bytes([0x61u8; 32]).into()), // keyed — uploads refuse plaintext
        None,                                             // mls_group_id
        None,                                             // content_keys
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        transfer_pool,
        nest_client,
        crate::config::SyncMode::Sync,
    )
}

/// The device the engine under test runs as. Anything else is a peer; a change
/// carrying this id is a self-echo.
const OUR_DEVICE: [u8; 32] = [0u8; 32];
/// The one peer device these tests publish from.
const PEER_DEVICE: [u8; 32] = [0xAAu8; 32];

/// A `create` [`SyncChange`] naming a real manifest hash served by the mock
/// store — `created_at` fixed well in the past so any local mtime (recorded
/// "now" by the test) reads as strictly newer.
fn create_change_with_manifest(
    seq: i64,
    path: &str,
    manifest_hash: ContentHash,
    device_id: [u8; 32],
) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{path}"),
        manifest_hash: Some(hex::encode(manifest_hash.digest())),
        size_bytes: 0,
        change_type: "create".to_string(),
        created_at: 1_700_000_000_000, // 2023-11-14 — always older than "now"
        path: Some(path.to_string()),
        device_id: Some(hex::encode(device_id)),
        content_key_version: None,
        thumbnail_hash: None,
        ..Default::default()
    }
}

/// Publish `body` at `rel` from a throwaway engine running as `device_id`, and
/// return the manifest hash the mock nest stored — the shape a real second
/// device's upload leaves behind, which is all a [`SyncChange`] fixture needs
/// to be downloadable. Every engine here shares one `BackupKey` (see
/// [`test_sync_engine`]), so the device under test can open what this sealed.
async fn publish_body(
    server: &MockServer,
    store: &crate::test_support::BlobStore,
    device_id: [u8; 32],
    rel: &str,
    body: &[u8],
) -> ContentHash {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), dir.path().to_path_buf(), device_id);
    std::fs::write(dir.path().join(rel), body).unwrap();
    engine.upload_file(rel).await.expect("publish_body upload");
    store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the upload must have POSTed a manifest")
}

// ─────────────────────────────────────────────────────────────────────
// Test
// ─────────────────────────────────────────────────────────────────────

/// A batch-latest peer `create` must never overwrite a local file whose
/// content already diverges from it — the exact "content" half of the LEAD's
/// residue claim, independent of the `batch_latest_seq` fold (nothing in this
/// batch supersedes the change, so the fold is a no-op either way).
#[tokio::test]
async fn a_batch_latest_peer_create_does_not_overwrite_a_locally_newer_file() {
    const REL: &str = "kept.bin";
    const STALE_BODY: &[u8] = b"stale body from a peer's create\x00\x01\x02";
    const USER_BODY: &[u8] = b"the user's newer content \xff\xfe\xfd";

    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;

    // Seed the mock nest with the STALE content, from a distinct "peer"
    // engine/device — an entirely separate watch dir and db, exactly like the
    // e2e test's two-launch shape. Same BackupKey so the fresh device below
    // (below, sharing the key by construction of `test_sync_engine`) can open
    // what this peer sealed.
    let manifest_hash = publish_body(&server, &store, PEER_DEVICE, REL, STALE_BODY).await;

    // The device under test: a FRESH watch dir + in-memory db (untracked —
    // no row for REL at all, matching a fresh bind), already holding the
    // user's own newer content at the same path before any pull runs.
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), OUR_DEVICE);
    let full = watch.path().join(REL);
    std::fs::write(&full, USER_BODY).unwrap();

    let change = create_change_with_manifest(1, REL, manifest_hash, PEER_DEVICE);
    // `count` increments whenever `download_and_write_file` returns `Ok` —
    // true for every `resolved_apply` branch (`Unresolved`/`KeepLocal` write
    // nothing but still return `Ok(())`), not only `ApplyIncoming`. It is not
    // a proxy for "the incoming bytes landed"; only the disk read below is.
    engine
        .apply_remote_changes(&[change], 0)
        .await
        .expect("apply_remote_changes");

    let on_disk = std::fs::read(&full).unwrap();
    assert_eq!(
        on_disk,
        USER_BODY,
        "a batch-latest peer create overwrote the user's newer local file with \
         the stale incoming body — the LEAD residue reproduces (got {} bytes, \
         expected the {}-byte user body)",
        on_disk.len(),
        USER_BODY.len()
    );
}

// ─────────────────────────────────────────────────────────────────────
// The same-author supersession rung
// ─────────────────────────────────────────────────────────────────────

/// A `delete` [`SyncChange`] — no manifest, carrying the writer's (possibly
/// under-covering) causal stamp.
fn delete_change(
    seq: i64,
    path: &str,
    device_id: [u8; 32],
    derived_through: Option<i64>,
) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{path}"),
        manifest_hash: None,
        size_bytes: 0,
        change_type: "delete".to_string(),
        created_at: 1_700_000_000_000,
        path: Some(path.to_string()),
        device_id: Some(hex::encode(device_id)),
        derived_through,
        ..Default::default()
    }
}

/// **The pin — same-author supersession.** A fresh bind replaying a
/// LINEAR one-device history (`create@1` then `delete@2`, both by the same
/// peer device) onto a folder already holding a newer local file must fold
/// the stale create away SILENTLY: no download, no conflict row — the fold's
/// own observable, exactly as the tier_3 e2e
/// (`test_filesync_bind_history.py`) asserts through the UI.
///
/// The trap this pins: the deleting device stamps `derived_through` with its
/// persisted catch-up anchor (the lower-bound law — it must not claim more),
/// and a device that records create → delete before its own echoes come back
/// stamps `Some(0)`, which does NOT cover its own create's seq. The pure
/// watermark licence then refuses the fold, the stale create downloads, and
/// conflict auto-resolve "rescues" the local file while recording a spurious
/// review row for a file the user never edited concurrently. The receiver
/// needs no watermark for this case: a carrier's own author's earlier
/// same-path rows are causally its ancestors by single-writer linearity —
/// provable from the batch rows alone (`conflicts.md` clause 5, the
/// same-author rung).
///
/// ⚠ The rung is licensed for DELETE carriers ONLY. Its content-carrier
/// variant is fuzzer-refuted (2026-08-04, `merge_convergence_test`'s standing
/// search): folding the carrier author's earlier rows under a content
/// carrier starves the ledger holds the content rung needs to absorb a later
/// stale-watermarked lost-ack reissue — latest-wins then destroys a peer's
/// edit. Do not widen the rung; the fuzzer is its boundary's pin.
#[tokio::test]
async fn a_delete_supersedes_its_own_authors_earlier_create_in_one_batch() {
    const REL: &str = "kept.txt";
    const STALE_BODY: &[u8] = b"stale body v1 - must never land on disk again";
    const USER_BODY: &[u8] = b"the user's newer content - destroying this is data loss";

    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;

    // The history-writer device publishes the stale body, then deletes it —
    // the delete recorded BEFORE its own echoes advanced its anchor, so its
    // stamp honestly under-covers its own create (the e2e's exact shape).
    let stale_manifest = publish_body(&server, &store, PEER_DEVICE, REL, STALE_BODY).await;

    // The device under test: fresh watch dir + fresh db (anchor 0, REL
    // untracked), already holding the user's newer content at the path.
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), OUR_DEVICE);
    let full = watch.path().join(REL);
    std::fs::write(&full, USER_BODY).unwrap();

    let create = create_change_with_manifest(1, REL, stale_manifest, PEER_DEVICE);
    let delete = delete_change(2, REL, PEER_DEVICE, Some(0));
    engine
        .apply_remote_changes(&[create, delete], 0)
        .await
        .expect("apply_remote_changes");

    // The no-user-data-loss floor: the local file survives untouched (the
    // delete arm's untracked-local guard, whatever the fold did).
    let on_disk = std::fs::read(&full).expect("the local file must survive the replay");
    assert_eq!(
        on_disk, USER_BODY,
        "the freshly bound folder's local file no longer holds the user's content"
    );

    // The fold's own observable: the stale create was SKIPPED, not fought
    // off. A recorded conflict here means the fold was refused and the stale
    // body was downloaded against the user's file — the spurious review row
    // the e2e sees as `conflict-file-info`.
    assert!(
        !engine.db().has_unresolved_conflict_for_path(REL).unwrap(),
        "the stale create@1 was downloaded and fought off by conflict \
         auto-resolve instead of being folded away under the same author's \
         delete@2 — the watermark licence has no same-author rung"
    );
}

/// **The pin, second half — an own echo above the carrier must not
/// disable the fold.** The REAL fresh-bind flow (measured live 2026-08-04,
/// the e2e's agent log): the engine's reconcile UPLOADS the local file
/// before the first pull, so the first batch arrives as
/// `[create@3 (peer, stale), delete@5 (peer, w=4 — fully covering),
/// create@7 (OUR OWN just-recorded upload's echo)]`. The raw batch-latest
/// row is the own echo — no carrier, licence refused — and the fully
/// licensed peer delete below it never got to fold the stale create away:
/// the stale body downloaded and conflict auto-resolve manufactured the
/// spurious review row even though the watermark story was perfect.
///
/// The ruling's answer (`conflicts.md` clause 5): the fold's carrier is the
/// path's latest **superseding** row — the same fold (1) already computes,
/// excluding self content echoes — not the raw batch tail. Any row above
/// that carrier is by construction a self create/modify echo (anything else
/// would itself be the superseding max), writes nothing, and neither
/// carries nor blocks the fold.
#[tokio::test]
async fn an_own_echo_above_the_carrier_does_not_disable_the_fold() {
    const REL: &str = "kept.txt";
    const STALE_BODY: &[u8] = b"stale body v1 - must never land on disk again";
    const USER_BODY: &[u8] = b"the user's newer content - destroying this is data loss";

    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;

    let stale_manifest = publish_body(&server, &store, PEER_DEVICE, REL, STALE_BODY).await;
    // Our own upload's manifest — the reconcile-first upload whose echo tops
    // the batch, exactly as the live run recorded it.
    let own_manifest = publish_body(&server, &store, OUR_DEVICE, REL, USER_BODY).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), OUR_DEVICE);
    let full = watch.path().join(REL);
    std::fs::write(&full, USER_BODY).unwrap();

    let create = create_change_with_manifest(3, REL, stale_manifest, PEER_DEVICE);
    let mut delete = delete_change(5, REL, PEER_DEVICE, Some(4));
    delete.derived_through = Some(4); // fully covers the stale create
    let own_echo = create_change_with_manifest(7, REL, own_manifest, OUR_DEVICE);
    engine
        .apply_remote_changes(&[create, delete, own_echo], 0)
        .await
        .expect("apply_remote_changes");

    let on_disk = std::fs::read(&full).expect("the local file must survive the replay");
    assert_eq!(
        on_disk, USER_BODY,
        "the freshly bound folder's local file no longer holds the user's content"
    );
    assert!(
        !engine.db().has_unresolved_conflict_for_path(REL).unwrap(),
        "the stale create@3 was downloaded and fought off instead of being \
         folded under the covering peer delete@5 — the fold treated the raw \
         batch-latest row (our own echo@7) as the only permissible carrier"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The self-echo supersession pin (2026-08-02)
// ─────────────────────────────────────────────────────────────────────

/// The common ancestor both seats edit away from, and the two concurrent
/// edits — each touching only its own line, so `file-sync.md` § Conflicts
/// requires a clean three-way merge and never a silent drop.
const SHARED: &str = "shared.txt";
const BASE_BODY: &[u8] = b"a: base\nb: base\n";
const PEER_BODY: &[u8] = b"a: EDITED\nb: base\n";
const OUR_BODY: &[u8] = b"a: base\nb: EDITED\n";

/// Lay down a diverged device: local holds `OUR_BODY`, the cached merge base
/// holds the ancestor both seats published from.
fn diverged_engine(server_uri: &str) -> (tempfile::TempDir, SyncEngine) {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(server_uri, watch.path().to_path_buf(), OUR_DEVICE);
    std::fs::write(watch.path().join(SHARED), OUR_BODY).unwrap();
    // The engine's OWN merge-base root (scoped per `(device, set)`), never a
    // hand-built flat one — see `arrange_frontiers`.
    let bases = engine.base_dir_for_test();
    std::fs::create_dir_all(&bases).unwrap();
    std::fs::write(bases.join(SHARED), BASE_BODY).unwrap();
    (watch, engine)
}

/// **The pin.** A batch holding a peer's concurrent edit *and* this
/// device's own echo at a higher seq must still consider the peer's edit. A
/// self-echo downloads nothing and writes nothing — it only re-bases — so it
/// supersedes no content, and counting it in the batch-latest fold dropped the
/// peer's change with no download, no conflict row and no error, permanently
/// (the anchor advances past a skipped change, so it is never redelivered).
///
/// The observable is the recorded conflict, not the bytes: with no WS-RPC
/// double the resolved report cannot land, so resolution falls back to
/// `Unresolved` (module doc) — which still records the conflict, and which the
/// bug skips entirely.
#[tokio::test]
async fn a_self_echo_does_not_suppress_a_peers_concurrent_change_in_the_same_batch() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;

    let peer_manifest = publish_body(&server, &store, PEER_DEVICE, SHARED, PEER_BODY).await;
    let our_manifest = publish_body(&server, &store, OUR_DEVICE, SHARED, OUR_BODY).await;

    let (_watch, engine) = diverged_engine(&server.uri());

    // The batch the failing run pulled: the peer's edit first, then our own
    // echo of the edit we published 70 ms earlier — a HIGHER seq, because the
    // nest ordered our publication after theirs on the way back.
    let peer_change = create_change_with_manifest(4, SHARED, peer_manifest, PEER_DEVICE);
    let self_echo = create_change_with_manifest(5, SHARED, our_manifest, OUR_DEVICE);

    engine
        .apply_remote_changes(&[peer_change, self_echo], 0)
        .await
        .expect("apply_remote_changes");

    assert!(
        engine
            .db()
            .has_unresolved_conflict_for_path(SHARED)
            .unwrap(),
        "the peer's concurrent edit was never considered — our own echo at the \
         higher seq suppressed it in the batch-latest fold, so it was dropped \
         with no download and no conflict row (the edit is lost \
         permanently, because the anchor advances past a skipped change)"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The stale-skippable-carrier conjunct
// ─────────────────────────────────────────────────────────────────────

/// Arrange the receiver's per-path causal frontiers through the same public
/// seam production writes (`CausalStore`, rooted beside the merge bases).
///
/// `edit_frontier` ABOVE `frontier` is not a contrived state: `engine.rs`'s
/// `record_change` ack site advances the edit-frontier to this device's own
/// just-recorded seq **immediately**, deliberately ahead of the frontier —
/// which only advances when that row's echo comes back. Every device with an
/// own edit in flight sits here.
/// `None` leaves that frontier UNTRACKED, which is a distinct state from zero:
/// the receiver rules gate every skip on a tracked full frontier.
fn arrange_frontiers(
    engine: &SyncEngine,
    rel: &str,
    frontier: Option<i64>,
    edit_frontier: Option<i64>,
) {
    // Through the ENGINE's own store, not a hand-built one over the watch
    // dir: the causal root is scoped per `(device, set)` (`causal.rs` §
    // scoped_store_dir), so a fixture writing the flat root arranges state
    // the engine under test never reads.
    let causal = engine.causal();
    if let Some(f) = frontier {
        causal.advance_frontier(rel, f);
    }
    if let Some(ef) = edit_frontier {
        causal.advance_edit_frontier(rel, ef);
    }
}

/// A peer resolution row: a `modify` carrying `is_resolution` and the
/// watermark it derived through.
fn resolution_change(
    seq: i64,
    path: &str,
    manifest_hash: ContentHash,
    device_id: [u8; 32],
    derived_through: i64,
) -> SyncChange {
    let mut c = create_change_with_manifest(seq, path, manifest_hash, device_id);
    c.change_type = "modify".to_string();
    c.derived_through = Some(derived_through);
    c.is_resolution = Some(true);
    c
}

/// **The pin — the fold never fires under a carrier that would itself
/// be stale-skipped.** `conflicts.md` clause 5 ratifies the conjunct
/// (fuzzer-measured 2026-08-03) and the tier_1 model implements it at both
/// fold sites, but the shared engine's `fold_licensed` checked only
/// peer-authorship + watermark coverage — so the conjunct was enforced
/// NOWHERE in production (the engine is the only host that folds).
///
/// The shape, and why it is a lost edit rather than a strand: this device
/// holds unechoed own novelty at seq 8, so its effective edit-frontier is 8
/// while its plain frontier is still 3. One batch delivers
/// `[peer edit@6 (w=3), peer resolution@7 (w=6)]`. The carrier's watermark
/// covers the edit's seq, so the licence passes and the edit is folded away
/// **without a download**. The carrier then meets `judge_incoming_before_fetch`
/// rule 2 — a resolution whose `w=6` sits below the effective edit-frontier 8
/// — and is skipped as stale. Net: the peer's edit is never downloaded, never
/// merged, records no conflict row, and the anchor advances past it. Silent
/// and permanent.
#[tokio::test]
async fn a_carrier_that_would_be_stale_skipped_does_not_license_the_fold() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;

    let peer_manifest = publish_body(&server, &store, PEER_DEVICE, SHARED, PEER_BODY).await;
    // The carrier's own bytes — a third body, so nothing about this test can
    // pass through the content-keyed reissue rung.
    let carrier_manifest = publish_body(
        &server,
        &store,
        PEER_DEVICE,
        SHARED,
        b"a: EDITED\nb: EDITED\n",
    )
    .await;

    // `_watch` stays BOUND: dropping the TempDir would delete the watch
    // directory under the engine. The fixtures below reach the engine's own
    // resolved state roots, so the path itself is no longer read here.
    let (_watch, engine) = diverged_engine(&server.uri());
    arrange_frontiers(&engine, SHARED, Some(3), Some(8));

    let mut peer_edit = create_change_with_manifest(6, SHARED, peer_manifest, PEER_DEVICE);
    peer_edit.derived_through = Some(3);
    let carrier = resolution_change(7, SHARED, carrier_manifest, PEER_DEVICE, 6);

    engine
        .apply_remote_changes(&[peer_edit, carrier], 0)
        .await
        .expect("apply_remote_changes");

    // The loss is that the edit's content is never delivered. Assert delivery
    // at the point the engine commits to it — the manifest fetch — because the
    // carrier is stale-skipped on both sides of the fix, so nothing downstream
    // of it can witness the fold.
    assert!(
        store.manifest_was_fetched(peer_manifest),
        "the peer's edit@6 was folded away under a carrier the receiver then \
         stale-skipped (resolution@7, w=6 < effective edit-frontier 8), so its \
         content was consumed undelivered — never fetched, never merged, and \
         the anchor advances past it (`fold_licensed` carries no \
         effective-edit-frontier check, so `conflicts.md` clause 5's \
         stale-carrier conjunct binds nothing in production)"
    );
    // ...and the user-visible half of the same loss: a peer edit concurrent
    // with unpublished local work must reach the review surface.
    assert!(
        engine
            .db()
            .has_unresolved_conflict_for_path(SHARED)
            .unwrap(),
        "the peer's edit@6 was delivered but recorded no conflict against the \
         diverged local file — the fold refusal must route it through \
         auto-resolve exactly as an unfolded peer edit is"
    );
}

/// The control for the pin above, and the guard on its blast radius: the
/// identical batch with a carrier that is NOT stale-skippable (`w=8` reaches
/// the effective edit-frontier) must still fold — the folded row's content
/// never fetched, because the carrier delivers it. Without this, "refuse the
/// fold whenever the carrier is a resolution" would pass the pin while
/// re-opening exactly the spurious-review-row harm the ruling closed
/// one day earlier.
///
/// The observable is the fetch, not the conflict row: this receiver is
/// diverged, so the *carrier itself* is a genuine sibling and records a
/// conflict of its own whatever the fold did. That is correct behaviour and
/// it is why the first draft of this control was red for a reason unrelated
/// to the fold.
#[tokio::test]
async fn a_covering_resolution_carrier_still_licenses_the_fold() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;

    let peer_manifest = publish_body(&server, &store, PEER_DEVICE, SHARED, PEER_BODY).await;
    let carrier_manifest = publish_body(
        &server,
        &store,
        PEER_DEVICE,
        SHARED,
        b"a: EDITED\nb: EDITED\n",
    )
    .await;

    // `_watch` stays BOUND: dropping the TempDir would delete the watch
    // directory under the engine. The fixtures below reach the engine's own
    // resolved state roots, so the path itself is no longer read here.
    let (_watch, engine) = diverged_engine(&server.uri());
    arrange_frontiers(&engine, SHARED, Some(3), Some(8));

    let mut peer_edit = create_change_with_manifest(6, SHARED, peer_manifest, PEER_DEVICE);
    peer_edit.derived_through = Some(3);
    // w=8 covers both the folded edit's seq AND the effective edit-frontier,
    // so rule 2 does not skip this carrier: the fold is licensed and the
    // carrier delivers the folded row's content. The carrier sits at seq 9,
    // ABOVE the own row it claims to cover: a claim at or above a row's own
    // seq is one the log refutes, and the receiver bounds it away (the
    // watermark-bound ruling, 2026-09-21 — this control's first shape, a
    // carrier at 7 claiming 8, was exactly that impossible row and is now
    // rightly stale-skipped like the pin above).
    let carrier = resolution_change(9, SHARED, carrier_manifest, PEER_DEVICE, 8);

    engine
        .apply_remote_changes(&[peer_edit, carrier], 0)
        .await
        .expect("apply_remote_changes");

    assert!(
        !store.manifest_was_fetched(peer_manifest),
        "a fully-covering resolution carrier no longer licenses the fold: the \
         folded edit@6 was fetched and fought off through conflict \
         auto-resolve, manufacturing the spurious review row the \
         ruling closed. The conjunct must refuse ONLY carriers rule 2 \
         would actually stale-skip"
    );
}

/// **The boundary pin — an UNTRACKED full frontier is not a stale
/// carrier.** The receiver rules gate every skip on a tracked full frontier
/// (`judge_incoming_before_fetch`'s outer `if let Some(f) = frontiers.frontier`),
/// so on a path this device has never applied a row to, rule 2 skips nothing
/// and the carrier is delivered — the fold must stay licensed.
///
/// The state is reachable and common, not a corner: the record-ack site
/// advances the EDIT-frontier alone, so a device that creates a file and
/// records it holds `edit_frontier = Some(seq)` with the full frontier still
/// `None` until the echo returns. A fresh bind replaying history is exactly
/// here.
///
/// This pin exists because it caught a mutation the other two could not: the
/// bare `w < effective_edit_frontier()` comparison — the shape the original
/// fix sketch proposed, and the one the tier_1 model uses (soundly, since its
/// seats always track both) — passes the pin and the control while refusing
/// the fold here, downloading the stale row against the user's file and
/// manufacturing the spurious review row the ruling had just closed.
/// Asking `judge_incoming_before_fetch` itself is what makes the licence and
/// the skip agree by construction.
#[tokio::test]
async fn an_untracked_frontier_does_not_make_the_carrier_stale() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;

    let peer_manifest = publish_body(&server, &store, PEER_DEVICE, SHARED, PEER_BODY).await;
    let carrier_manifest = publish_body(
        &server,
        &store,
        PEER_DEVICE,
        SHARED,
        b"a: EDITED\nb: EDITED\n",
    )
    .await;

    // `_watch` stays BOUND: dropping the TempDir would delete the watch
    // directory under the engine. The fixtures below reach the engine's own
    // resolved state roots, so the path itself is no longer read here.
    let (_watch, engine) = diverged_engine(&server.uri());
    // Own novelty recorded at seq 8, echo not yet returned, and this path has
    // never had a row applied to it — full frontier UNTRACKED.
    arrange_frontiers(&engine, SHARED, None, Some(8));

    let mut peer_edit = create_change_with_manifest(6, SHARED, peer_manifest, PEER_DEVICE);
    peer_edit.derived_through = Some(3);
    // w=6 sits below the effective edit-frontier 8 — the raw comparison reads
    // this carrier as stale. Rule 2 does not, because no full frontier is
    // tracked, so the carrier is delivered and the fold is sound.
    let carrier = resolution_change(7, SHARED, carrier_manifest, PEER_DEVICE, 6);

    engine
        .apply_remote_changes(&[peer_edit, carrier], 0)
        .await
        .expect("apply_remote_changes");

    assert!(
        !store.manifest_was_fetched(peer_manifest),
        "the fold was refused on a path with no tracked full frontier, where \
         the receiver rules skip nothing and the carrier is delivered: the \
         stale row@6 downloaded against the local file and conflict \
         auto-resolve manufactured a spurious review row (implemented \
         as a bare `w < effective_edit_frontier` comparison rather than asking \
         `judge_incoming_before_fetch` — it over-refuses exactly where row 156 \
         had just stopped over-refusing)"
    );
}

/// The control for the pin above: the identical divergence, with the self-echo
/// removed from the batch, must record the conflict. It isolates the fold as
/// the cause — if this one ever fails, the harness broke, not the fold.
#[tokio::test]
async fn the_same_peer_change_alone_in_its_batch_records_the_conflict() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;

    let peer_manifest = publish_body(&server, &store, PEER_DEVICE, SHARED, PEER_BODY).await;
    let (_watch, engine) = diverged_engine(&server.uri());

    let peer_change = create_change_with_manifest(4, SHARED, peer_manifest, PEER_DEVICE);
    engine
        .apply_remote_changes(&[peer_change], 0)
        .await
        .expect("apply_remote_changes");

    assert!(
        engine
            .db()
            .has_unresolved_conflict_for_path(SHARED)
            .unwrap(),
        "control failed: a lone peer change against a diverged local file did \
         not record a conflict, so this module's harness — not the fold — is \
         what the pin above is measuring"
    );
}

// ─────────────────────────────────────────────────────────────────────
// What the fold protects the fresh bind FROM
// ─────────────────────────────────────────────────────────────────────

/// **The state every fresh bind judges its first batch in.** Each caller's
/// startup path converges the LOCAL half before the first remote pull
/// (`engine_lifecycle::catch_up_pass` and `fauna-sync-agent`'s
/// `engine_driver`, both above [`crate::always_resident::run_watch_loop`],
/// which holds the eager pull), so a freshly bound folder's local file is
/// already RECORDED — its bytes provably nest-side — while the path's plain
/// frontier is still untracked and its live merge base still holds nothing.
///
/// In that state — while nothing has yet COUNTED the own row — a peer create
/// for the same path judges FAST-FORWARD (the `(Some(w), None)` arm: an
/// untracked frontier leaves `local_matches_base` alone deciding, and the
/// recorded witness makes it true). The leg-4 DEFER cap is what stands
/// between that verdict and the user's newer bytes: it holds the anchor BELOW
/// the row instead of adopting, so nothing is written and nothing is consumed.
///
/// Both halves are asserted here, because the ordinary adopt is not the only
/// way to lose: a cap that CONSUMED its row would leave the same intact file
/// on disk and still be the leg-4 lost line.
///
/// The record ack's edit-frontier advance is deliberately WITHHELD here: this
/// pins the cap's hold-not-consume mechanics in the window before the own
/// row is counted. With the count in place the judge reads the same create
/// as the sibling it is and the cap never fires — the cap-release ruling
/// (2026-09-21), pinned by the two tests after this one.
#[tokio::test]
async fn a_recorded_but_unechoed_local_file_defers_a_stale_peer_create() {
    const REL: &str = "kept.txt";
    const STALE_BODY: &[u8] = b"stale body v1 - must never land on disk again";
    const USER_BODY: &[u8] = b"the user's newer content - destroying this is data loss";

    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let stale_manifest = publish_body(&server, &store, PEER_DEVICE, REL, STALE_BODY).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), OUR_DEVICE);
    let full = watch.path().join(REL);
    std::fs::write(&full, USER_BODY).unwrap();

    // The startup converge, before any pull: the chunks + manifest really do
    // land on the mock. Its `changes.record` leg is WS-RPC, which this crate
    // has no double for (module doc), so the row's recorded witness is
    // stamped through the same `SyncDb` seam
    // [`SyncEngine::commit_recorded_head`] uses when the record lands.
    engine
        .upload_file(REL)
        .await
        .expect("the startup converge's upload");
    engine
        .db()
        .stamp_recorded_content_from_local(REL, crate::db::ProofOrigin::OwnRecord)
        .expect("stamp the recorded head the record's ack would have stamped");

    // ONE stale peer create, with nothing to supersede it: the batch-latest
    // fold is a no-op here by construction, so what the assertions below see
    // is the cap alone.
    let create = create_change_with_manifest(1, REL, stale_manifest, PEER_DEVICE);
    engine
        .apply_remote_changes(&[create], 0)
        .await
        .expect("apply_remote_changes");

    assert_eq!(
        std::fs::read(&full).unwrap(),
        USER_BODY,
        "the stale peer create was ADOPTED over the user's newer local file — \
         the recorded witness licensed the fast-forward and the leg-4 defer \
         cap did not stop it"
    );
    assert!(
        !engine.db().has_unresolved_conflict_for_path(REL).unwrap(),
        "the stale create recorded a conflict row: it reached the merge arm \
         instead of the cap"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        0,
        "the anchor advanced past the deferred row — a cap that consumes its \
         row is the leg-4 lost line (the re-listing is what applies it, so it \
         must still sit BELOW the anchor)"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The cap's release
// ─────────────────────────────────────────────────────────────────────

/// **The fresh-bind park, and its release** (the cap-release ruling,
/// 2026-09-21 — `conflicts.md` clause 5). The state the pin above arranges,
/// one row richer on each side and with the ack's count in place: the
/// peer's stale create at seq 1, a never-deleted peer file Q at seq 2 (the
/// barrier — only the anchor passing seq 1 can write it), and this device's
/// own echo at seq 3. It is the shape every fresh bind onto a set holding a
/// same-named, never-deleted, different file produces, and the shape the
/// tombstoned tier_3 leg produces once its fold is gone (measured
/// 2026-09-20, tier_1 and live).
///
/// Before the ruling the batch parked for ever: the stale create judged
/// fast-forward on the recorded witness alone, the cap held the anchor at 0,
/// and `dyn_cap` skipped every row above it — Q and the own echo included —
/// so the frontier never advanced and every later pull re-judged identically
/// (anchor 0 after two passes; Q never written). Now the judge honours the
/// own row the record ack counted into the edit-frontier
/// (`arrange_frontiers` stands in for the ack: the mock has no
/// `changes.record` double), reads the stale create as the SIBLING it is,
/// and the merge arm consumes it — Q is written, the own echo lands, and the
/// anchor passes all three rows, with the user's bytes never adopted over.
/// (Against this mock the resolver's report cannot land, so the sibling
/// resolves `Unresolved`: local kept, a conflict row recorded, the row
/// consumed — production's wired report resolves it instead.) Run for both
/// stamps a peer create can carry.
async fn a_fresh_bind_progresses_past_a_sibling_create(derived_through: Option<i64>) {
    const REL: &str = "kept.txt";
    const Q: &str = "history-marker.txt";
    const STALE_BODY: &[u8] = b"stale body v1 - must never land on disk again";
    const USER_BODY: &[u8] = b"the user's newer content - destroying this is data loss";
    const Q_BODY: &[u8] = b"history marker - the fresh bind must download me";

    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let stale_manifest = publish_body(&server, &store, PEER_DEVICE, REL, STALE_BODY).await;
    let q_manifest = publish_body(&server, &store, PEER_DEVICE, Q, Q_BODY).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), OUR_DEVICE);
    let full = watch.path().join(REL);
    std::fs::write(&full, USER_BODY).unwrap();

    // The startup converge, before any pull — as in the pin above, plus the
    // one thing the ack does that the pin withholds: it names the row's seq,
    // and `record_change`'s Ok arm counts it into the edit-frontier at once.
    engine
        .upload_file(REL)
        .await
        .expect("the startup converge's upload");
    let own_manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the converge must have POSTed a manifest");
    engine
        .db()
        .stamp_recorded_content_from_local(REL, crate::db::ProofOrigin::OwnRecord)
        .expect("stamp the recorded head the record's ack would have stamped");
    arrange_frontiers(&engine, REL, None, Some(3));

    let mut stale = create_change_with_manifest(1, REL, stale_manifest, PEER_DEVICE);
    stale.derived_through = derived_through;
    let barrier = create_change_with_manifest(2, Q, q_manifest, PEER_DEVICE);
    let own_echo = create_change_with_manifest(3, REL, own_manifest, OUR_DEVICE);

    engine
        .apply_remote_changes(&[stale, barrier, own_echo], 0)
        .await
        .expect("apply_remote_changes");

    assert_eq!(
        std::fs::read(&full).unwrap(),
        USER_BODY,
        "the stale peer create was ADOPTED over the user's newer local file"
    );
    assert_eq!(
        std::fs::read(watch.path().join(Q)).ok().as_deref(),
        Some(Q_BODY),
        "the barrier row never applied — the cap parked the batch below the \
         stale create (the fresh-bind park: the own echo that releases it sat \
         above the cap)"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        3,
        "the anchor did not pass the batch — every later pull re-judges this \
         listing identically"
    );
    assert_eq!(
        engine.causal().frontier(REL),
        Some(3),
        "the own echo was skipped above the cap: the path frontier never \
         started tracking"
    );
}

#[tokio::test]
async fn a_fresh_binds_counted_own_row_releases_the_cap_for_a_watermarked_create() {
    a_fresh_bind_progresses_past_a_sibling_create(Some(0)).await;
}

#[tokio::test]
async fn a_fresh_binds_counted_own_row_releases_the_cap_for_an_unwatermarked_create() {
    a_fresh_bind_progresses_past_a_sibling_create(None).await;
}

/// **A forged watermark does not re-park the fresh bind** (the
/// watermark-bound ruling, 2026-09-21 — `conflicts.md` clause 5; the shared
/// `causal::bounded_watermark` is the rule, this pins the engine consuming
/// it). The same batch with the peer's claim forged: `derived_through` is the
/// writer's word and the nest stores it as sent, so the stale create at seq 1
/// claims `i64::MAX`. Unbounded, that dominated the own row counted at 3, the
/// row read fast-forward, and the cap held the anchor at 0 under the own echo
/// for ever. Bounded by its own nest-assigned seq the row is the sibling its
/// honest twin above is.
#[tokio::test]
async fn a_forged_watermark_does_not_repark_a_fresh_binds_counted_own_row() {
    a_fresh_bind_progresses_past_a_sibling_create(Some(i64::MAX)).await;
}

// ─────────────────────────────────────────────────────────────────────
// The held-bytes release
// ─────────────────────────────────────────────────────────────────────

/// **A revert to held bytes does not park a covering peer edit** (the
/// held-bytes release, 2026-09-21 — `conflicts.md` clause 5; the shared
/// `causal::verbatim_adopt_deferred` is the rule, this pins the engine
/// consuming it). This device edited `notes.txt` (row 1 — its live base and
/// ledger hold), then reverted it on disk to the row-0 version its ledger
/// holds and recorded the revert (the widened proven-reissue proof stamps it
/// resolution-class, so neither frontier counts it). A peer's edit on top of
/// row 1 lists before the revert's echo. Pre-ruling the cap held it — local ≠
/// live base — with the anchor at 0, and the own echo above the cap never
/// processed: the park. The revert's bytes already rest in the log, so the
/// cap protects nothing here: the covering edit is adopted and the anchor
/// passes it (rule 5's trade — the stale revert loses at its author as it is
/// skipped at peers; its echo then re-asserts the current bytes).
#[tokio::test]
async fn a_reverts_own_held_bytes_do_not_park_a_covering_peer_edit() {
    const REL: &str = "notes.txt";
    const BASE0: &[u8] = b"base\n";
    const OWN_EDIT: &[u8] = b"base\na: EDITED\n";
    const PEER_EDIT: &[u8] = b"base\na: EDITED\nb: EDITED\n";

    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let peer_manifest = publish_body(&server, &store, PEER_DEVICE, REL, PEER_EDIT).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), OUR_DEVICE);
    // The path's history at this device: row 0 and its own row 1, echoed.
    engine.causal().hold(REL, 0, BASE0);
    engine.causal().hold(REL, 1, OWN_EDIT);
    arrange_frontiers(&engine, REL, Some(1), Some(1));
    let bases = engine.base_dir_for_test();
    std::fs::create_dir_all(&bases).unwrap();
    std::fs::write(bases.join(REL), OWN_EDIT).unwrap();
    // The revert on disk, recorded (the row's recorded witness through the
    // same seam the fresh-bind pins use — no `changes.record` double here).
    let full = watch.path().join(REL);
    std::fs::write(&full, BASE0).unwrap();
    engine.upload_file(REL).await.expect("the revert's upload");
    engine
        .db()
        .stamp_recorded_content_from_local(REL, crate::db::ProofOrigin::OwnRecord)
        .expect("stamp the recorded head the record's ack would have stamped");

    let mut peer = create_change_with_manifest(2, REL, peer_manifest, PEER_DEVICE);
    peer.change_type = "modify".to_string();
    peer.derived_through = Some(1);
    engine
        .apply_remote_changes(&[peer], 0)
        .await
        .expect("apply_remote_changes");

    assert_eq!(
        std::fs::read(&full).unwrap(),
        PEER_EDIT,
        "the covering peer edit was HELD behind a revert whose bytes the ledger \
         already holds — the park the held-bytes release closes"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        2,
        "the anchor did not pass the covering edit"
    );
}
