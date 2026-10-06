//! The `ResolvedApply::KeepLocal` branch — reachable in-crate for the first
//! time, via the [`crate::nest_api::SyncControlApi`] seam.
//!
//! **What was untestable and why.** When the create/modify arm detects that the
//! local file diverges from both the incoming bytes and the cached merge base,
//! it auto-resolves (`file-sync.md` § Conflicts, ratified 2026-07-10:
//! latest-writer-wins for no-base/binary content). A local win commits only if
//! the **resolved report reaches the nest** — `engine.rs`'s
//! `ConflictResolution::LocalWins` arm is literally
//! `Ok(_) => ResolvedApply::KeepLocal { .. }` / `Err(e) => fallback_unresolved!(e)`.
//! That report is WS-RPC (`fauna.sync.conflicts.report`), and this crate's test
//! harness is a stateful **HTTP** wiremock, which no WS-RPC call can reach. So
//! before the seam every in-crate test of this path necessarily degraded to
//! `Unresolved` — `create_arm_local_conflict_test.rs`'s module doc states this
//! outright, and the two outcomes are indistinguishable by the assertion that
//! module makes (both leave the local bytes untouched).
//!
//! **Why that mattered.** `Unresolved` and `KeepLocal` leave the *disk* in the
//! same state but the *database* in opposite ones: `Unresolved` records nothing
//! (the divergence stays outstanding), while `KeepLocal` re-points the row to
//! `Synced` at the LOCAL content's hash, refreshes the merge base, and stamps
//! the dehydration gate's proof — i.e. it declares the local file the propagated
//! head. An engine that silently never reached `KeepLocal` would look correct to
//! every byte-level assertion in the crate while never converging.
//!
//! The first two tests below are a discriminator pair over exactly that fork,
//! driven only by whether the control plane accepts the report. The `failing`
//! case is the pre-seam behavior (and the fail-closed posture the goal doc
//! requires); the `accepting` case is the state no in-crate test could
//! previously observe.
//!
//! The remaining two cover what a *committed* `KeepLocal` row then owes the
//! pull loop — the anchor advance that keeps a superseded change from ever
//! being re-delivered, and the genuine later delete that must still apply.
//! Their doc comments carry the tombstone of the deleted stale-delete
//! reproduction; read them before adding a cross-batch seq guard.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::sync::SyncChange;
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::db::{SyncDb, SyncState};
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_api::FakeSyncControl;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

// ─────────────────────────────────────────────────────────────────────
// Helpers — same trimmed harness as `create_arm_local_conflict_test.rs`
// (that module's helpers are private to it, so this mirrors rather than
// imports; the only difference is the injected control plane).
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

/// A peer `create` naming a real manifest hash served by the mock store,
/// `created_at` fixed well in the past so the local mtime reads as strictly
/// newer — which is what makes latest-writer-wins pick the LOCAL side.
fn create_change_with_manifest(seq: i64, path: &str, manifest_hash: ContentHash) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{path}"),
        manifest_hash: Some(hex::encode(manifest_hash.digest())),
        size_bytes: 0,
        change_type: "create".to_string(),
        created_at: 1_700_000_000_000, // 2023-11-14 — always older than "now"
        path: Some(path.to_string()),
        device_id: Some(hex::encode([0xAAu8; 32])), // a peer device, never OUR_DEVICE
        content_key_version: None,
        thumbnail_hash: None,
        ..Default::default()
    }
}

const REL: &str = "contested.bin";
const STALE_BODY: &[u8] = b"stale body from a peer's create\x00\x01\x02";
const USER_BODY: &[u8] = b"the user's newer content \xff\xfe\xfd";

/// Stand up the shared scenario: a peer's create sits on the (mock) nest, and a
/// fresh untracked device already holds genuinely newer local content at the
/// same path. Returns the engine, its watch dir, and the local content's hash.
async fn scenario(
    server: &MockServer,
) -> (SyncEngine, tempfile::TempDir, ContentHash, ContentHash) {
    let store = MockNest::new().mount(server).await;

    {
        let peer_watch = tempfile::tempdir().unwrap();
        let peer = test_sync_engine(&server.uri(), peer_watch.path().to_path_buf(), [0xAAu8; 32]);
        std::fs::write(peer_watch.path().join(REL), STALE_BODY).unwrap();
        peer.upload_file(REL).await.expect("peer upload_file");
    }
    let peer_manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the peer's upload must have POSTed a manifest");

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), [0u8; 32]);
    std::fs::write(watch.path().join(REL), USER_BODY).unwrap();

    (engine, watch, peer_manifest, ContentHash::of_raw(USER_BODY))
}

// ─────────────────────────────────────────────────────────────────────
// Tests — the discriminator pair
// ─────────────────────────────────────────────────────────────────────

/// The state no in-crate test could previously reach: the resolved report
/// SUCCEEDS, so the local winner is committed as the propagated head — row
/// `Synced` at the LOCAL content's hash, not merely "bytes left alone".
#[tokio::test]
async fn a_successful_resolved_report_lands_keep_local_as_the_propagated_head() {
    let server = MockServer::start().await;
    let (engine, watch, peer_manifest, local_hash) = scenario(&server).await;

    let control = FakeSyncControl::accepting();
    engine.set_control_api(Arc::new(control.clone()));

    engine
        .apply_remote_changes(&[create_change_with_manifest(1, REL, peer_manifest)], 0)
        .await
        .expect("apply_remote_changes");

    // 1. The bytes are the user's (true for `Unresolved` too — necessary, not
    //    sufficient, which is exactly why this test exists).
    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        USER_BODY,
        "the local winner's bytes must survive"
    );

    // 2. The discriminator: the row is re-pointed to `Synced` at the LOCAL
    //    content's hash. `Unresolved` records nothing at all here.
    let entry = engine.db().get_entry(REL).expect("get_entry").expect(
        "KeepLocal must upsert a row for the local winner — an absent row means the \
                 resolution degraded to Unresolved (the pre-seam behavior)",
    );
    assert_eq!(
        entry.state,
        SyncState::Synced,
        "KeepLocal commits the local winner as the propagated head, so the row is Synced"
    );
    assert_eq!(
        entry.local_hash,
        Some(local_hash),
        "the row must point at the LOCAL content's hash, never the incoming peer body's"
    );
    assert_eq!(
        entry.remote_hash,
        Some(local_hash),
        "the local winner IS the new remote head after the resolved report propagates it"
    );

    // 3. The report the nest actually saw: one resolved `latest_wins` conflict.
    let reports = control.conflict_reports();
    assert_eq!(
        reports.len(),
        1,
        "exactly one conflict report is owed for one divergence, got {}",
        reports.len()
    );
    assert_eq!(
        reports[0].resolution.as_deref(),
        Some("latest_wins"),
        "file-sync.md § Conflicts: no base and binary content resolves latest-writer-wins"
    );
    assert_eq!(
        reports[0].conflict_type, "concurrent_edit",
        "a diverging create/modify is a concurrent edit, not a delete-vs-edit"
    );
}

/// 🪦 **Tombstone (2026-07-31) — this replaces the `#[ignore]`d
/// reproduction `a_stale_historical_delete_in_a_later_pull_does_not_undo_a_keep_local_resolution`,
/// deleted in the same commit. Do not re-add it.**
///
/// That test asserted the local winner survives a *stale* delete (seq 3)
/// delivered in a **separate, later** `apply_remote_changes` call after a seq-5
/// `KeepLocal` resolution committed it as head. It failed, and the failure was
/// real at the engine's own altitude: the only seq guard the engine has is
/// `batch_latest_seq_by_path` (`engine.rs:2604`), which is explicitly
/// **intra-batch** — across separate calls there is none, and the row records no
/// seq to compare against.
///
/// **Why it was deleted rather than fixed: the input is unreachable from the
/// production caller, and the guarantee that makes it unreachable was traced end
/// to end** (the decision the item asked for, resolved as its option (a)):
///
/// 1. `apply_remote_changes` is `pub(crate)` with exactly ONE production caller,
///    `pull_remote_changes` (`engine.rs:2681`) — there is no second path into
///    the delete arm.
/// 2. That caller passes `since = db.get_anchor()` (`engine.rs:2665`).
/// 3. Every `changes.list` arm on the nest filters and orders at the source:
///    `WHERE … seq > ?2 AND superseded_at IS NULL ORDER BY seq`
///    (`bins/fauna-nest/src/db/sync_storage.rs:1052`, `:1529`, `:1560`). The
///    cross-nest relay runs the same query and takes an ordered prefix page —
///    "close early, never skip" (`federation_handlers.rs:1063-1071`).
/// 4. The anchor is **monotone**: `max_seq` is seeded from the incoming anchor
///    and only ever increases (`engine.rs:2697`, `:2719-2721`) before
///    `set_anchor(max_seq)` (`:3103`).
///
/// So once seq 5 is applied the anchor is ≥ 5 and no query can return seq 3
/// again. This test pins step 4 — the one link that lives in this crate and is
/// therefore the one a future change here could break.
///
/// **The interleaving worry the item raised dissolves rather than resolves.**
/// It asked "can the real-time forward arm interleave with the reconnect
/// catch-up arm?". In this engine there is no separate forward arm at all: the
/// nudge and the rescan tick are two branches of one `tokio::select!` that both
/// call the same anchored `pull_remote_changes`
/// (`always_resident.rs:488`, `:499`). Because the **nest**, not the caller,
/// decides a batch's contents from `since`, even a hypothetical concurrent pair
/// of pulls cannot deliver a change backwards: either both batches predate the
/// anchor advance and contain seq 3 *and* seq 5 together — where the intra-batch
/// fold skips the delete — or the later fetch starts above 5 and excludes seq 3.
/// Serializing is indeed not ordering; the ordering comes from `seq > anchor`.
///
/// ⚠ **This makes the anchor contract load-bearing, with no defence in depth.**
/// It is stated for future readers in `file-sync.md` § 5 (Offline Catch-Up).
/// Anything that lets a caller hand this function a batch not drawn from the
/// current anchor re-opens the deleted scenario for real.
#[tokio::test]
async fn a_committed_resolution_advances_the_anchor_past_its_own_seq() {
    let server = MockServer::start().await;
    let (engine, watch, peer_manifest, local_hash) = scenario(&server).await;

    engine.set_control_api(Arc::new(FakeSyncControl::accepting()));

    // The resolving pull: a peer create at seq 5 loses to newer local content.
    engine
        .apply_remote_changes(&[create_change_with_manifest(5, REL, peer_manifest)], 0)
        .await
        .expect("apply_remote_changes (the resolving pull)");

    let after_resolve = engine
        .db()
        .get_entry(REL)
        .expect("get_entry")
        .expect("precondition: the resolution must have committed a row");
    assert_eq!(
        after_resolve.state,
        SyncState::Synced,
        "precondition: this pin is only meaningful once KeepLocal actually committed"
    );
    assert_eq!(
        after_resolve.local_hash,
        Some(local_hash),
        "precondition: the committed head is the LOCAL winner"
    );

    // THE PIN. The next `pull_remote_changes` sends this value as `since`, and
    // every nest arm answers `seq > since` — which is the whole reason a delete
    // recorded at seq 3 can never be handed back to the delete arm.
    assert_eq!(
        engine.db().get_anchor().expect("get_anchor"),
        5,
        "the anchor must advance to the batch's max seq — it is the ONLY thing that stops \
         an already-superseded change being re-delivered to the delete arm in a later pull"
    );

    // A change whose seq is below the anchor must not drag it backwards. The
    // production caller never sends one (it fetches from the anchor); this pins
    // the arithmetic that makes `set_anchor(max_seq)` safe regardless.
    let mut unrelated_older = create_change_with_manifest(2, "elsewhere.bin", peer_manifest);
    unrelated_older.change_type = "unknown-to-this-engine".to_string();
    engine
        .apply_remote_changes(&[unrelated_older], 5)
        .await
        .expect("apply_remote_changes (an older, unrelated change)");
    assert_eq!(
        engine.db().get_anchor().expect("get_anchor"),
        5,
        "the anchor must never regress: `max_seq` is seeded from the incoming anchor"
    );

    // And the winner is still the winner — no arm above touched it.
    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        USER_BODY,
        "the local winner's bytes are untouched"
    );
}

/// The other half of the same question, and the reason the deleted reproduction
/// could not simply be "fixed" by teaching the delete arm to distrust low seqs:
/// a **genuine** delete recorded *after* a resolution must still apply.
///
/// A `KeepLocal` resolution re-points the row to `Synced` at the local content's
/// hash, so the disk agrees with the row and the tombstone is honoured — the
/// user deleting the surviving file on another device is an ordinary delete, not
/// a stale one. Any future cross-batch seq guard must keep this green; a naive
/// "ignore anything that looks old" would strand deletes here exactly as it
/// would break the fresh-bind replay from anchor 0.
#[tokio::test]
async fn a_later_delete_still_applies_against_a_committed_resolution() {
    let server = MockServer::start().await;
    let (engine, watch, peer_manifest, _local_hash) = scenario(&server).await;

    engine.set_control_api(Arc::new(FakeSyncControl::accepting()));

    engine
        .apply_remote_changes(&[create_change_with_manifest(5, REL, peer_manifest)], 0)
        .await
        .expect("apply_remote_changes (the resolving pull)");
    assert_eq!(
        engine
            .db()
            .get_entry(REL)
            .expect("get_entry")
            .expect("precondition: a committed row")
            .state,
        SyncState::Synced,
        "precondition: KeepLocal committed"
    );

    // A delete recorded AFTER the resolution — the ordinary case.
    let mut later_delete = create_change_with_manifest(7, REL, peer_manifest);
    later_delete.change_type = "delete".to_string();
    later_delete.manifest_hash = None;
    engine
        .apply_remote_changes(&[later_delete], 5)
        .await
        .expect("apply_remote_changes (the later delete)");

    assert!(
        !watch.path().join(REL).exists(),
        "a delete recorded after the resolution is a genuine delete of the surviving \
         content and must be honoured — the row was settled and the disk agreed with it"
    );
}

/// The fail-closed arm, and the behavior every in-crate test was pinned to
/// before the seam existed: the report fails, so the resolution degrades to
/// `Unresolved` — local bytes still kept, but nothing is declared head.
///
/// Pairing this with the test above is what makes either meaningful: identical
/// scenario, identical disk outcome, opposite database outcome, and the ONLY
/// difference is whether the control plane accepted the report.
#[tokio::test]
async fn a_failed_resolved_report_degrades_to_unresolved_and_still_keeps_the_bytes() {
    let server = MockServer::start().await;
    let (engine, watch, peer_manifest, _local_hash) = scenario(&server).await;

    let control = FakeSyncControl::failing("no WS-RPC transport in this test");
    engine.set_control_api(Arc::new(control.clone()));

    engine
        .apply_remote_changes(&[create_change_with_manifest(1, REL, peer_manifest)], 0)
        .await
        .expect("apply_remote_changes must still succeed — the fallback is not an error");

    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        USER_BODY,
        "fail-closed: an unreportable conflict must never cost the local bytes"
    );

    let entry = engine.db().get_entry(REL).expect("get_entry");
    assert!(
        entry.is_none_or(|e| e.state != SyncState::Synced),
        "a conflict that could not be reported must NOT be recorded as Synced — the \
         divergence is still outstanding"
    );

    // The documented fallback LADDER, which only a recording double can see:
    // `file-sync.md` § Conflicts — "an upload or report failure degrades to the
    // local conflict record + an unresolved report (the same ladder the
    // concurrent-edit auto-resolve uses)" (`conflicts.md`). So the
    // engine makes TWO calls, and the second is the unresolved (fail-closed)
    // shape: the resolved attempt first, then an unresolved report carrying no
    // resolution.
    let reports = control.conflict_reports();
    assert_eq!(
        reports.len(),
        2,
        "expected the documented two-rung ladder (resolved attempt, then the \
         unresolved report), got {} call(s)",
        reports.len()
    );
    assert_eq!(
        reports[0].resolution.as_deref(),
        Some("latest_wins"),
        "rung 1 is the resolved auto-resolve attempt"
    );
    assert_eq!(
        reports[1].resolution, None,
        "rung 2 is the unresolved report — a resolution here would tell the nest \
         to propagate a winner the engine just failed to commit"
    );
}
