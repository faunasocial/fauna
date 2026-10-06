//! Wiring pin for [`SyncEngine::upload_file`]'s post-record
//! `commit_recorded_head` call sites — the exact call the live 2026-07-17
//! regression fixed. `pull_remote_changes_test.rs`'s
//! `a_recorded_upload_head_is_not_stale_on_the_next_fold` calls
//! `commit_recorded_head` **directly**, so it pins the method's own behavior but
//! not the wiring FROM `upload_file`'s `Ok(_seq)` arm — proven by hand: deleting the
//! `commit_recorded_head` call from that arm leaves the whole suite green.
//!
//! This file drives `upload_file` end-to-end and asserts the **failure** half:
//! a record that never reaches the nest must leave the row's `manifest_hash` at
//! the pre-upload merge base (the fail-closed property `commit_recorded_head`
//! must not weaken — mirrors `a_genuinely_moved_nest_head_still_classifies_stale`
//! one layer up). The **success** half needs a genuinely connected `nest_client`
//! (a real WS-RPC round trip so `record_change`'s `Ok(_seq)` arm is reached for
//! real, not simulated) — this crate has no such harness (every engine test here
//! deliberately runs an unconnected `NestClient`, `download_file_bytes_test`'s
//! doc explains why), so that arm lives in
//! `bins/fauna-nest/tests/conformance_sync_engine_record_commit.rs`, which spins
//! up a real in-process nest per the `conformance_content_key_chunk_route.rs`
//! pattern.
//!
//! Lives under `src/` (gated `#[cfg(test)]` in `lib.rs`), mirroring
//! `pull_remote_changes_test.rs`.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::db::{SyncDb, SyncState};
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

const DEVICE_ID: [u8; 32] = [0u8; 32];

/// A `SyncClient` whose `AuthClient` points at `server_uri` (a real HTTP
/// wiremock server, so the byte-plane chunk/manifest uploads succeed) and a
/// `NestClient` pointed at the same URI — which never completes a WS-RPC
/// handshake against a plain HTTP server, so `record_change` always fails
/// after the default 30s RPC deadline (mirrors `upload_thumbnail_test`'s
/// `test_engine`; `record_change` is best-effort in `upload_file`, so the
/// upload itself still succeeds).
fn test_engine(server_uri: &str, watch_dir: std::path::PathBuf) -> SyncEngine {
    let db = SyncDb::open_in_memory().unwrap();
    let http_bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let http_auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server_uri.to_string(),
        ActorKeypair::generate(),
        http_bearer,
        reqwest::Client::new(),
    ));
    let client = SyncClient::new(http_auth, &DEVICE_ID);
    let nest_client =
        fauna_client::NestClient::new(server_uri.to_string(), ActorKeypair::generate());
    let transfer_pool = TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None);

    SyncEngine::new(
        watch_dir,
        db,
        client,
        Some("__test".to_string()), // folder
        DEVICE_ID,
        None,                                           // mls
        None,                                           // epoch_secret
        Some(BackupKey::from_bytes([0x11; 32]).into()), // backup_key — an owner-only set needs a key to upload
        None,                                           // mls_group_id
        None,                                           // content_keys
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        transfer_pool,
        nest_client,
        crate::config::SyncMode::Sync,
    )
}

/// Seed a `Synced` row at `rel` whose manifest is the pre-upload merge base
/// (`merge_base`) — the steady state `upload_file` preserves in `base_manifest`
/// when it later re-reads the same row (engine.rs: "manifest_hash is only
/// updated in download_and_write_file ... it always points to the common
/// ancestor"). The seeded `local_hash` deliberately does not match the bytes
/// `upload_file` will write next, so the "already synced, skipping"
/// short-circuit does not fire.
fn seed_merge_base(engine: &SyncEngine, rel: &str, merge_base: ContentHash) {
    engine
        .db()
        .upsert_entry(
            rel,
            Some(ContentHash::from_digest_raw([0x00; 32])), // stale local_hash
            Some(merge_base),
            Some(merge_base),
            SyncState::Synced,
            0,
            0,
            0,
            1,
            None,
        )
        .unwrap();
}

// ─────────────────────────────────────────────────────────────────────
// Arm (b) — a failed record must not move the row off the merge base
// ─────────────────────────────────────────────────────────────────────

/// Driving `upload_file` end-to-end (not calling `commit_recorded_head`
/// directly): when the record never reaches the nest, the row's
/// `manifest_hash` must stay exactly the pre-upload merge base — proving
/// `commit_recorded_head` is wired ONLY behind the record's `Ok(_seq)` arm, not
/// called unconditionally after the byte-plane upload succeeds.
#[tokio::test]
async fn a_record_failure_leaves_the_row_at_the_pre_upload_merge_base() {
    let server = MockServer::start().await;
    MockNest::new().mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    let rel = "notes.txt";
    std::fs::write(dir.path().join(rel), b"fresh local edit").unwrap();

    let engine = test_engine(&server.uri(), dir.path().to_path_buf());
    let merge_base = ContentHash::from_digest_raw([0xAB; 32]);
    seed_merge_base(&engine, rel, merge_base);

    let outcome = engine.upload_file(rel).await.expect(
        "upload_file must still succeed on the byte plane even though the \
         control-plane record fails (record_change is best-effort)",
    );
    assert!(
        !outcome.recorded,
        "the unconnected nest_client must never complete the WS-RPC record"
    );

    let entry = engine.db().get_entry(rel).unwrap().unwrap();
    assert_eq!(
        entry.manifest_hash,
        Some(merge_base),
        "a FAILED record must leave the row's manifest_hash at the pre-upload \
         merge base — commit_recorded_head must run only behind the record's \
         Ok(_seq) arm, never unconditionally after the chunk/manifest upload"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The dehydration gate must be record-blind-SAFE: a failed record leaves a
// `Synced` row whose `local_hash` matches disk but whose recorded head
// (`manifest_hash`) is the OLD merge base. Freeing such a file would anchor the
// placeholder on the OLD manifest — a later re-hydration would download the OLD
// bytes and the local edit would be destroyed. `is_dehydration_safe` must
// therefore refuse it, even though disk == local_hash.
// ─────────────────────────────────────────────────────────────────────

/// The live 2026-07-17 data-loss window, as a test: after an upload whose record
/// FAILED, the row is `Synced` with `local_hash == disk` but its recorded head
/// still points at the pre-upload merge base. `is_dehydration_safe` must return
/// `false` — freeing the bytes would strand the edit behind the OLD manifest.
#[tokio::test]
async fn a_record_failed_edit_is_not_dehydration_safe() {
    let server = MockServer::start().await;
    MockNest::new().mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    let rel = "notes.txt";
    std::fs::write(dir.path().join(rel), b"fresh local edit").unwrap();

    let engine = test_engine(&server.uri(), dir.path().to_path_buf());
    let merge_base = ContentHash::from_digest_raw([0xAB; 32]);
    seed_merge_base(&engine, rel, merge_base);

    let outcome = engine
        .upload_file(rel)
        .await
        .expect("upload succeeds on the byte plane even when the record fails");
    assert!(
        !outcome.recorded,
        "the record must have failed for this test"
    );

    // Sanity: the row IS `Synced` with disk == local_hash — the exact shape that
    // fooled the record-blind gate (so the assertion below is not vacuous).
    let entry = engine.db().get_entry(rel).unwrap().unwrap();
    assert_eq!(entry.state, SyncState::Synced);
    assert_eq!(
        entry.local_hash,
        Some(fauna_core::data::ContentHash::of_raw(b"fresh local edit")),
        "local_hash must have advanced to the fresh edit — the record-blind trap"
    );

    assert!(
        !engine.is_dehydration_safe(rel),
        "a Synced row whose recorded head (manifest_hash) is the OLD merge base \
         must NEVER be dehydration-safe: freeing it re-hydrates the OLD bytes and \
         destroys the un-recorded local edit"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The own-PENDING retention mint (B2.5) — the offline-authored leg
// ─────────────────────────────────────────────────────────────────────

/// A fully-offline upload (nothing listens on the nest URL at all) still
/// mints the path's own-PENDING retention row — the mint sits between the
/// local seal and the first network call, so "authored in the cabin" leaves
/// exactly the serveable row behind. And it is minted from local plaintext:
/// the row's manifest equals a fresh deterministic seal of the on-disk bytes
/// under the same root — never a synthesis from `sync_entries`. Nothing
/// enters the sequenced read (a pending row has no seq until a nest speaks).
#[tokio::test]
async fn a_fully_offline_upload_mints_a_pending_retention_row() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "cabin/draft.txt";
    std::fs::create_dir_all(dir.path().join("cabin")).unwrap();
    std::fs::write(dir.path().join(rel), b"authored in the cabin").unwrap();

    // Port 9 (discard) — nothing listens; the first network call refuses.
    let engine = test_engine("http://127.0.0.1:9", dir.path().to_path_buf());

    engine
        .upload_file(rel)
        .await
        .expect_err("fully offline, the network half must fail");

    let pending = engine.db().own_pending_changes(10).unwrap();
    assert_eq!(pending.len(), 1, "the mint precedes the first network call");
    let row = &pending[0];
    assert_eq!(row.seq, None);
    assert_eq!(row.path, rel);
    assert_eq!(row.change_type, "create");
    assert!(
        row.path_sealed.is_some(),
        "the sealed path sibling rides the pending row too"
    );

    let root = BackupKey::from_bytes([0x11; 32]).convergent_chunk_root();
    let expect = crate::seal::seal_blob(b"authored in the cabin", Some((root, None))).unwrap();
    assert_eq!(
        row.manifest_hash,
        Some(hex::encode(expect.manifest_hash.digest())),
        "the pending manifest is computed from local plaintext at mint time"
    );

    assert!(
        engine.db().own_changes_since(0, 10).unwrap().is_empty(),
        "a pending row never serves from the sequenced read"
    );
}
