//! On-demand single-file hydration: [`SyncEngine::download_file_bytes_by_manifest`].
//!
//! Cloud-Files-API (Windows) hydrates a placeholder by asking the sync engine
//! for a file's *bytes* — to hand to `CfExecute(TRANSFER_DATA)` — as opposed to
//! the folder-pull path that writes whole files to disk. This stands up a
//! stateful wiremock nest (it serves back whatever was uploaded) and asserts a
//! round-trip: upload a file through the real chunk/compress/encrypt pipeline,
//! then fetch its bytes back by manifest hash and compare.
//!
//! Lives under `src/` (gated `#[cfg(test)]` in `lib.rs`), mirroring
//! `segment_backup_round_trip_test.rs`.

use std::sync::{Arc, Mutex};

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderContentKeys;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;
use fauna_nest_http::{BearerSource, StaticBearer};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::adaptive::AdaptiveConcurrency;
use crate::db::{SyncDb, SyncState};
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

// ─────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────

/// A `SyncClient` whose `AuthClient` points at `server_uri` and always sends a
/// fixed bearer (no `/auth/token` round trip).
pub(crate) fn test_sync_client(server_uri: &str) -> SyncClient {
    let kp = ActorKeypair::generate();
    let http = reqwest::Client::new();
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server_uri.to_string(),
        kp,
        bearer,
        http,
    ));
    let device_id = [0u8; 32];
    SyncClient::new(auth, &device_id)
}

/// Build a `SyncEngine` rooted at `watch_dir` with full control of its seal
/// inputs: `mls_group_id` is the **bound marker** (a cross-user shared set) and
/// `content_keys` supplies its M2 generation history (chunks seal under
/// [`FolderContentKeys::current_key`], version-stamped — shared folders,
/// Slice 3); `backup_key` enables owner-only per-chunk AEAD. `mls` is kept only
/// for the MLS-dependent surfaces (`device_sync_channel_id`), not chunk keying.
/// All seal inputs `None` ⇒ plaintext; `mls_group_id = Some` + `content_keys =
/// None` ⇒ bound-but-unkeyed → **fail closed**.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_test_engine(
    server_uri: &str,
    watch_dir: std::path::PathBuf,
    mls: Option<Arc<MlsEngine>>,
    backup_key: Option<BackupKey>,
    mls_group_id: Option<Vec<u8>>,
    content_keys: Option<FolderContentKeys>,
) -> SyncEngine {
    build_test_engine_as(
        server_uri,
        watch_dir,
        mls,
        backup_key,
        mls_group_id,
        content_keys,
        ActorKeypair::generate(),
    )
}

/// [`test_sync_engine`] whose seat is `identity` — the engine's own actor id
/// (the one its reader binding calls the current identity) is that keypair's,
/// for a test whose subject is who signed a row.
pub(crate) fn test_sync_engine_as(
    server_uri: &str,
    watch_dir: std::path::PathBuf,
    backup_key: Option<BackupKey>,
    identity: ActorKeypair,
) -> SyncEngine {
    build_test_engine_as(
        server_uri, watch_dir, None, backup_key, None, None, identity,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_test_engine_as(
    server_uri: &str,
    watch_dir: std::path::PathBuf,
    mls: Option<Arc<MlsEngine>>,
    backup_key: Option<BackupKey>,
    mls_group_id: Option<Vec<u8>>,
    content_keys: Option<FolderContentKeys>,
    identity: ActorKeypair,
) -> SyncEngine {
    let db = SyncDb::open_in_memory().unwrap();
    let client = test_sync_client(server_uri);
    let device_id = [0u8; 32];
    let format_registry = FormatRegistry::new();
    let ignore = IgnoreMatcher::default();
    let concurrency = Arc::new(AdaptiveConcurrency::fixed(4));
    let transfer_pool = TransferPool::new(concurrency, None);
    // Unconnected WS-RPC client: this test never drives the conflict/record path.
    let nest_client = fauna_client::NestClient::new(server_uri.to_string(), identity);

    SyncEngine::new(
        watch_dir,
        db,
        client,
        Some("__test".to_string()),
        device_id,
        mls,
        None, // epoch_secret
        backup_key.map(Into::into),
        mls_group_id,
        content_keys,
        fauna_core::format::ConflictPolicy::default(),
        format_registry,
        ignore,
        4,
        transfer_pool,
        nest_client,
        crate::config::SyncMode::Sync,
    )
}

/// A `SyncEngine` with no MLS binding (`mls: None`), exercising the bearer-only
/// hydration path (`download_file_bytes*` never touches MLS). `backup_key`
/// enables per-chunk AEAD (the upload pipeline encrypts; the download path must
/// decrypt to match). These tests assert that path works MLS-less.
fn test_sync_engine(
    server_uri: &str,
    watch_dir: std::path::PathBuf,
    backup_key: Option<BackupKey>,
) -> SyncEngine {
    build_test_engine(server_uri, watch_dir, None, backup_key, None, None)
}

/// Assert a corpus pass did its BYTE-plane work and then reported the change
/// records it could not land — this module's engines run an **unconnected**
/// `NestClient` (`build_test_engine`), so `changes.record` never lands here.
///
/// That fact is not new (`post_succession_reseal_leaves_an_entry_owed_until_…`
/// has always relied on it); what is new is that the three corpus passes stopped
/// *swallowing* it. Until row 29 (2026-08-21) `declassify_owner_corpus`,
/// `reseal_pending_under_current` and `reseal_owner_only_plaintext` discarded
/// `reseal_path_under_current`'s `recorded` bool, so every re-seal test in this
/// module was silently passing with the nest head unchanged — and
/// `converge_corpus_to_audience` stamped `corpus_audience` over it, which its
/// `current == target` short-circuit then made permanent.
///
/// ⚠ **This is the REFUSAL arm.** The success arm — record lands ⇒ marker set ⇒
/// `corpus_audience` stamped ⇒ the next pass is a genuine no-op — needs a real
/// nest and lives in `bins/fauna-nest/tests/conformance_shared_folders.rs`
/// (`public_folder_member_declassify_converges_and_then_steadies`). Same split
/// the post-succession re-seal already documents.
#[track_caller]
fn expect_unrecorded(result: anyhow::Result<usize>, paths: usize) {
    let err =
        result.expect_err("this module's nest client is unconnected — no change record can land");
    let msg = err.to_string();
    assert!(
        msg.contains(&format!("{paths} of {paths} path(s) uploaded")),
        "the pass must name the unrecorded shortfall; got: {msg}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn download_file_bytes_by_manifest_round_trip() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    // Keyed: a folder engine refuses keyless (plaintext) uploads outright
    // so the round-trip runs the sealed owner-only path.
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x51u8; 32])),
    );

    // A multi-chunk payload (>64 KiB forces FastCDC boundaries) but well under
    // the 64 MiB streaming threshold, so the in-memory reassembly path runs.
    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(7) % 251) as u8)
        .collect();
    let rel = "docs/report.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    // Upload populates the mock nest with this file's chunks + manifest.
    engine.upload_file(rel).await.expect("upload_file");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload_file must have POSTed a manifest");

    // On-demand hydration: fetch the bytes back by manifest hash.
    let bytes = engine
        .download_file_bytes_by_manifest(manifest_hash, None, rel)
        .await
        .expect("download_file_bytes_by_manifest");

    assert_eq!(
        bytes, original,
        "hydrated bytes must equal the originally uploaded file"
    );
}

#[tokio::test]
async fn download_file_bytes_resolves_manifest_from_db() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x54u8; 32])),
    );

    let original: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(13) % 251) as u8)
        .collect();
    let rel = "media/clip.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    engine.upload_file(rel).await.expect("upload_file");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload_file must have POSTed a manifest");

    // Model a cloud-only placeholder: the SyncDb entry records the remote
    // manifest hash (this is what `download_and_write_file` stores; a local
    // upload deliberately keeps its merge base instead).
    let file_hash = ContentHash::of_raw(&original);
    engine
        .db()
        .upsert_entry(
            rel,
            Some(file_hash),
            Some(file_hash),
            Some(manifest_hash),
            SyncState::Placeholder,
            1,
            1,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed placeholder entry");
    engine.stamp_own_head_for_test(rel);

    // Path in → bytes out, with the manifest hash resolved from the db.
    let bytes = engine
        .download_file_bytes(rel)
        .await
        .expect("download_file_bytes");

    assert_eq!(
        bytes, original,
        "hydrated bytes must equal the placeholder's file"
    );
}

#[tokio::test]
async fn download_file_bytes_serves_historical_version_after_repoint() {
    // Restore byte round-trip (Slice 2, engine level). A placeholder row
    // re-pointed from a newer version's manifest back to an OLDER version's
    // manifest must re-hydrate the OLDER version's bytes — `file-sync.md`
    // § Restore: "the next open re-hydrates the historical bytes, because the
    // hydration FETCH_DATA resolves its manifest from that same local row."
    // `download_file_bytes(rel)` is exactly what `serve_fetch` calls on a cfapi
    // FETCH_DATA, so proving it here proves the manifest→chunks resolution the
    // full cfapi path relies on — cross-platform, no real nest. (The full cfapi
    // FETCH_DATA against a real-nest byte plane is the tier3-nest
    // `restore_byteplane_tier3` test.)
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x5au8; 32])),
    );

    let rel = "docs/notes.txt";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();

    // Version 1 (the historical version we will restore to).
    let v1: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(13) % 251) as u8)
        .collect();
    std::fs::write(&full, &v1).unwrap();
    engine.upload_file(rel).await.expect("upload v1");
    let manifest_v1 = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("v1 upload must POST a manifest");

    // Version 2 (a later edit — becomes the head). Both versions' chunks +
    // manifests now coexist in the store.
    let v2: Vec<u8> = (0..170_000u32)
        .map(|i| (i.wrapping_mul(29) % 251) as u8)
        .collect();
    std::fs::write(&full, &v2).unwrap();
    engine.upload_file(rel).await.expect("upload v2");
    let manifest_v2 = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("v2 upload must POST a manifest");
    assert_ne!(
        manifest_v1, manifest_v2,
        "two distinct versions must produce two distinct manifests"
    );

    // Restore to v1: re-point the row at v1's manifest and free the bytes —
    // exactly what `pipe_server::repoint_entry` writes on an on-demand client
    // (`manifest_hash` re-pointed, `local/remote_hash = None`, state Placeholder,
    // size = v1's). `download_file_bytes` reads only `manifest_hash` +
    // `content_key_version` from the row, so this is the load-bearing re-point.
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(manifest_v1),
            SyncState::Placeholder,
            0,
            1,
            v1.len() as i64,
            1,
            None,
        )
        .expect("re-point row at v1's manifest");
    engine.stamp_own_head_for_test(rel);

    // The next open resolves v1's manifest from the row → fetches v1's chunks →
    // reassembles v1's bytes, NOT the v2 head still sitting in the store.
    let bytes = engine
        .download_file_bytes(rel)
        .await
        .expect("download_file_bytes after restore re-point");
    assert_eq!(
        bytes, v1,
        "a restore must re-hydrate the historical (v1) bytes"
    );
    assert_ne!(bytes, v2, "the pre-restore head (v2) must NOT be served");
}

#[tokio::test]
async fn mls_less_engine_has_no_device_sync_channel() {
    // A bearer-only hydration host builds the engine with `mls: None`. The
    // device-sync channel id is derived from the MLS identity, so it must be
    // `None` rather than panicking — the hydration host never device-syncs.
    let server = MockServer::start().await;
    MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), None);
    assert!(
        engine.device_sync_channel_id().is_none(),
        "an mls-less engine has no device-sync channel id"
    );
}

#[tokio::test]
async fn download_file_bytes_errors_for_untracked_path() {
    let server = MockServer::start().await;
    MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), None);

    let err = engine
        .download_file_bytes("never/tracked.bin")
        .await
        .expect_err("an untracked path must error, not return bytes");
    assert!(
        err.to_string().contains("not tracked"),
        "error should explain the path is untracked, got: {err}"
    );
}

#[tokio::test]
async fn download_file_bytes_round_trips_encrypted_chunks() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    // With a backup_key the upload pipeline AEAD-encrypts each chunk; the mock
    // stores ciphertext opaquely, so a correct download must decrypt to match.
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x42u8; 32])),
    );

    let original: Vec<u8> = (0..180_000u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    let rel = "secret/payload.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    engine.upload_file(rel).await.expect("upload_file");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload_file must have POSTed a manifest");

    let bytes = engine
        .download_file_bytes_by_manifest(manifest_hash, None, rel)
        .await
        .expect("download_file_bytes_by_manifest (encrypted)");

    assert_eq!(
        bytes, original,
        "decrypted hydrated bytes must equal the originally uploaded file"
    );
}

// A raw group id (the bound marker) — the same `Option<Vec<u8>>` shape the
// nest's `folders.mls_group_id` column persists. Under M2 the engine's chunk
// root comes from `content_keys`, not the group; this id only marks the set as
// bound (so a missing-key case fails closed instead of degrading to plaintext).
fn test_group_id() -> Vec<u8> {
    vec![0x42u8; 24]
}

#[tokio::test]
async fn bound_engine_seals_chunks_under_group_content_key() {
    // Shared folders, Slice 3 (M2): a bound set seals its chunks under the
    // per-set **content key** (`content_keys.current_key`), not the owner-only
    // `backup_key`. We prove it two ways on one mock store:
    //   1. the bound engine round-trips its own content-key-sealed upload, and
    //   2. an *unbound* (plaintext) reader on the same store can NOT recover the
    //      plaintext — so the stored chunks really are sealed, not bare.
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    // Generation 1 content keys. A bound set carries no `backup_key` (Q4).
    let content_keys = FolderContentKeys::genesis([7u8; 32], 1_000);

    let watch = tempfile::tempdir().unwrap();
    let bound = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(content_keys),
    );

    // Multi-chunk payload (>64 KiB forces FastCDC boundaries), under the 64 MiB
    // streaming threshold so the in-memory reassembly path runs.
    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(17) % 251) as u8)
        .collect();
    let rel = "shared/report.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    bound.upload_file(rel).await.expect("upload_file (bound)");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload_file must have POSTed a manifest");

    // (1) The bound engine round-trips under generation 1 (version stamp 1).
    let bytes = bound
        .download_file_bytes_by_manifest(manifest_hash, Some(1), rel)
        .await
        .expect("download_file_bytes_by_manifest (bound)");
    assert_eq!(
        bytes, original,
        "a bound engine must round-trip its own content-key-sealed upload"
    );

    // (2) An unbound (plaintext) reader on the same store must NOT recover the
    // plaintext — the chunks are sealed under the content key, not stored bare.
    let watch2 = tempfile::tempdir().unwrap();
    let unbound = build_test_engine(
        &server.uri(),
        watch2.path().to_path_buf(),
        None,
        None,
        None,
        None,
    );
    let leaked = unbound
        .download_file_bytes_by_manifest(manifest_hash, None, rel)
        .await;
    assert!(
        leaked.map(|b| b != original).unwrap_or(true),
        "an unbound reader must NOT recover plaintext from content-key-sealed chunks"
    );
}

#[tokio::test]
async fn bound_engine_with_backup_key_still_seals_under_content_key() {
    // FS-5DC: the bearer-only hydration
    // service builds the engine with `backup_key = Some` **and** the app-pushed
    // `content_keys`/`mls_group_id` of a bound set. The bound set MUST still seal +
    // open under the M2 **content key** — `backup_key` must NOT shadow it (before the
    // `effective_backup_key` precedence fix it did, so a content-key-sealed chunk was
    // (mis)opened under the owner `BackupKey` → AEAD mismatch → hydration failed, and
    // 5d(c) was inert on the sole production path that loads it).
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let content_keys = FolderContentKeys::genesis([7u8; 32], 1_000);
    // A backup_key is ALSO present (the windows-service reality) — it must be
    // ignored for this bound set.
    let backup_key = BackupKey::from_bytes([9u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let bound = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        Some(backup_key),
        Some(test_group_id()),
        Some(content_keys),
    );

    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(17) % 251) as u8)
        .collect();
    let rel = "shared/report.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    bound
        .upload_file(rel)
        .await
        .expect("upload_file (bound + backup_key)");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload_file must have POSTed a manifest");

    // (1) The bound engine round-trips under generation 1 — proving it sealed under
    // the content key (the version stamp is present), not the owner backup_key.
    let bytes = bound
        .download_file_bytes_by_manifest(manifest_hash, Some(1), rel)
        .await
        .expect("download_file_bytes_by_manifest (bound + backup_key)");
    assert_eq!(
        bytes, original,
        "a bound engine that ALSO holds a backup_key must still round-trip under the content key"
    );

    // (2) An owner-`backup_key`-only reader (unbound, holding the SAME backup_key)
    // must NOT recover the plaintext — proving the chunks were sealed under the
    // content key, not the shadowing backup_key.
    let watch2 = tempfile::tempdir().unwrap();
    let backup_reader = build_test_engine(
        &server.uri(),
        watch2.path().to_path_buf(),
        None,
        Some(BackupKey::from_bytes([9u8; 32])),
        None,
        None,
    );
    let leaked = backup_reader
        .download_file_bytes_by_manifest(manifest_hash, None, rel)
        .await;
    assert!(
        leaked.map(|b| b != original).unwrap_or(true),
        "a backup_key reader must NOT recover content-key-sealed plaintext (backup_key did not seal it)"
    );
}

#[tokio::test]
async fn bound_but_keyless_engine_with_backup_key_fails_closed() {
    // FS-5DC fail-closed: a bound set (`mls_group_id` set) whose `content_keys` are
    // NOT yet loaded must FAIL CLOSED on upload even when a `backup_key` is present —
    // it must never fall through to seal a shared set under the owner `BackupKey` (or
    // plaintext). `effective_backup_key` returns `None` for the bound set, so the
    // seal takes `content_seal_root`, which bails on the missing generation.
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let bound_keyless = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        Some(BackupKey::from_bytes([9u8; 32])), // present, but must be ignored (bound)
        Some(test_group_id()),
        None, // content keys NOT loaded
    );

    let original = vec![0xabu8; 100_000];
    let rel = "shared/keyless.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    let err = bound_keyless
        .upload_file(rel)
        .await
        .expect_err("a bound-but-keyless engine must fail closed, not seal under backup_key");
    let msg = err.to_string();
    assert!(
        msg.contains("no M2 content keys") || msg.contains("refusing to seal"),
        "expected a fail-closed content-key error, got: {msg}"
    );
    // Nothing was uploaded (the seal bailed before any chunk POST).
    assert!(
        store.chunks.lock().unwrap().is_empty(),
        "a fail-closed bound upload must not have stored any chunk"
    );
}

/// FS-BIND (Slice 3, PIECE 6): a bound set's content-key-encrypted chunks must
/// upload to a store that enforces the **real** route's F9 anti-poisoning check
/// (`chunk_routes::resolve_verified_chunk_hash`). The engine AEAD-encrypts each
/// chunk under the content key but the route can't verify ciphertext against the
/// *plaintext* hash, so the engine keys the encrypted store by the **ciphertext**
/// hash (recorded in `manifest.stored_hashes`) and sends
/// `X-Content-Hash = blake3(ciphertext)` — the route's raw-path check then passes
/// with **no route change** (F9 preserved). The plaintext hash stays the AEAD
/// key/nonce + integrity anchor (`manifest.chunk_hashes`). Before PIECE 6 this
/// round-trip fails: the ciphertext is rejected (400), so the chunk is never
/// stored and the download 404s.
#[tokio::test]
async fn bound_engine_chunk_upload_round_trips_through_verifying_store() {
    let server = MockServer::start().await;
    let store = MockNest::new()
        .verifying()
        .with_blob_plane()
        .mount(&server)
        .await;

    // A bound set carries no `backup_key` (Q4); it seals under the content key.
    let content_keys = FolderContentKeys::genesis([7u8; 32], 1_000);

    let watch = tempfile::tempdir().unwrap();
    let bound = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(content_keys),
    );

    // Multi-chunk payload (>64 KiB forces FastCDC boundaries), under the 64 MiB
    // streaming threshold so the in-memory reassembly path runs.
    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(23) % 251) as u8)
        .collect();
    let rel = "shared/verified.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    bound
        .upload_file(rel)
        .await
        .expect("upload_file (bound, F9-verifying store)");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload_file must have POSTed a manifest");

    let bytes = bound
        .download_file_bytes_by_manifest(manifest_hash, Some(1), rel)
        .await
        .expect("round-trip through an F9-verifying chunk store");
    assert_eq!(
        bytes, original,
        "content-key chunks must round-trip through a store that enforces the F9 check"
    );
}

/// A bound
/// (content-key) upload enqueues its resume entries in `transfer_queue` by the
/// **ciphertext** store key (`manifest.stored_hashes`, set in
/// `engine::upload_chunked_bytes` / the streaming path). The drain worker
/// (`transfer_worker::build_processed_chunk_map`) must key its re-chunk map by the
/// **same** ciphertext hash — otherwise `drain_pending_uploads` looks up a
/// ciphertext store key in a plaintext-keyed map, every `get()` misses, and the
/// cleanup loop silently `complete_transfer()`s (DROPS) the never-uploaded chunk,
/// leaving a co-member's shared replica permanently missing chunks (an
/// availability/integrity hazard; the owner keeps local plaintext, so no
/// user-irrecoverable loss). This drives the full drain end-to-end against an
/// F9-verifying store and proves the queued chunks are actually re-uploaded under
/// their ciphertext store key, not dropped.
#[tokio::test]
async fn drain_resumes_bound_content_key_upload_by_ciphertext_store_key() {
    let server = MockServer::start().await;
    let store = MockNest::new()
        .verifying()
        .with_blob_plane()
        .mount(&server)
        .await; // enforces F9 (ciphertext keying)

    // A bound set carries no `backup_key` (Q4); it seals under the content key.
    let content_keys = FolderContentKeys::genesis([7u8; 32], 1_000);

    let watch = tempfile::tempdir().unwrap();
    let bound = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(content_keys),
    );

    // Multi-chunk payload: 10 MiB (> the 8 MiB SINGLE_CHUNK_THRESHOLD forces the
    // FastCDC path and guarantees ≥2 chunks even without cut points), under the
    // 64 MiB streaming threshold so the in-memory reassembly path runs. Well-mixed
    // bytes so FastCDC finds real boundaries (distinct store keys per chunk).
    let original: Vec<u8> = (0..10u32 * 1024 * 1024)
        .map(|i| {
            let mut x = i.wrapping_mul(2_654_435_761);
            x ^= x >> 15;
            x = x.wrapping_mul(2_246_822_519);
            x ^= x >> 13;
            (x & 0xff) as u8
        })
        .collect();
    let rel = "shared/resumed.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    // A real bound upload seals + uploads each chunk by its ciphertext store key
    // and POSTs the manifest — the source of truth for the store keys (no manual
    // pipeline duplication).
    bound.upload_file(rel).await.expect("bound upload_file");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload_file must have POSTed a manifest");
    let manifest_bytes = store
        .manifests
        .lock()
        .unwrap()
        .get(&hex::encode(manifest_hash.digest()))
        .cloned()
        .expect("uploaded manifest present in the store");
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).expect("decode manifest");
    let store_keys = manifest.store_keys();
    assert!(
        store_keys.len() >= 2,
        "need a multi-chunk file to exercise the drain map"
    );
    assert!(
        manifest.stored_hashes.is_some(),
        "a bound content-key upload must record ciphertext store keys in stored_hashes"
    );

    // Model a crash mid-upload: the destination lost the chunks and the queue
    // still holds the ciphertext-store-key entries the upload enqueued.
    store.chunks.lock().unwrap().clear();
    for sk in &store_keys {
        bound
            .db()
            .enqueue_transfer(rel, "upload", *sk, 0)
            .expect("enqueue resume entry");
    }
    assert_eq!(
        bound.db().eligible_transfers("upload").unwrap().len(),
        store_keys.len(),
        "queue must hold one eligible entry per chunk before drain"
    );

    // Drain. Fixed: re-seals each chunk, re-keys by ciphertext, and re-uploads it
    // under its store key. Buggy (plaintext-keyed drain map): every lookup misses,
    // so the cleanup loop silently completes each entry WITHOUT uploading.
    bound
        .drain_pending_uploads()
        .await
        .expect("drain_pending_uploads");

    // Every ciphertext store key must have been re-uploaded to the (F9-verifying)
    // store — proving the drain addressed the store correctly, not dropped it.
    let uploaded = store.chunks.lock().unwrap();
    for sk in &store_keys {
        assert!(
            uploaded.contains_key(&hex::encode(sk.digest())),
            "drain dropped chunk {} instead of re-uploading it",
            hex::encode(sk.digest())
        );
    }
    assert_eq!(
        uploaded.len(),
        store_keys.len(),
        "every queued content-key chunk must be re-uploaded on drain"
    );
}

/// Drain-after-rotation (the FS-DRAIN generation edge): `transfer_queue`
/// entries are enqueued by the ciphertext store key under the generation
/// **current at seal time**, but a rotate-on-removal can land between the
/// enqueue and the drain (the owner's own removal — "owner-sole-writer" never
/// bounded this). Two failure modes bracket the correct behavior:
///
/// - **Drop** (generation mismatch): a drain that
///   seals candidates only under the NEW current generation misses every
///   queued lookup and silently `complete_transfer()`s the never-uploaded
///   chunks — a co-member's replica stays permanently missing chunks the
///   recorded gen-N manifest references. Matching therefore spans **every
///   retained generation** (current-first; store-key equality disambiguates,
///   as the AEAD tag does for `keys_for`).
/// - **Publish**:
///   uploading the prior-generation match as-is publishes content *first
///   published after the removal* under a generation the removed member holds
///   irrevocably (chunk GET is unauthenticated by design) — piercing the
///   rotation boundary for content that never reached the destination
///   pre-removal.
///
/// The correct behavior is **requeue-under-current**: the drain neither drops
/// nor publishes a prior-generation match — the engine re-seals the path under
/// the current generation via the shared re-seal trio (force upload → verify →
/// best-effort supersede of the stale record), re-stamping the change record,
/// and completes the stale queue entries only after the re-seal lands.
#[tokio::test]
async fn drain_after_rotation_requeues_prior_generation_chunks_under_current() {
    let server = MockServer::start().await;
    let store = MockNest::new()
        .verifying()
        .with_blob_plane()
        .mount(&server)
        .await; // enforces F9 (ciphertext keying)

    let gen1_key = [7u8; 32];
    let gen2_key = [9u8; 32];

    // Seal + upload under generation 1 to learn the gen-1 ciphertext store keys.
    let watch = tempfile::tempdir().unwrap();
    let gen1 = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(FolderContentKeys::genesis(gen1_key, 1_000)),
    );

    // Multi-chunk payload (> the 8 MiB SINGLE_CHUNK_THRESHOLD, < the 64 MiB
    // streaming threshold), well-mixed so FastCDC finds real boundaries.
    let original: Vec<u8> = (0..10u32 * 1024 * 1024)
        .map(|i| {
            let mut x = i.wrapping_mul(2_654_435_761);
            x ^= x >> 15;
            x = x.wrapping_mul(2_246_822_519);
            x ^= x >> 13;
            (x & 0xff) as u8
        })
        .collect();
    let rel = "shared/rotated.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    gen1.upload_file(rel)
        .await
        .expect("gen-1 bound upload_file");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload_file must have POSTed a manifest");
    let manifest_bytes = store
        .manifests
        .lock()
        .unwrap()
        .get(&hex::encode(manifest_hash.digest()))
        .cloned()
        .expect("uploaded manifest present in the store");
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).expect("decode manifest");
    let store_keys = manifest.store_keys();
    assert!(
        store_keys.len() >= 2,
        "need a multi-chunk file to exercise the drain map"
    );

    // Rotate-on-removal lands before the drain: gen 2 is now current, gen 1
    // retained in `prior` (the owner keeps the full back-catalogue). Model the
    // relaunch as a fresh engine over the same watch dir holding the rotated
    // custody, whose queue still carries the gen-1 ciphertext store keys.
    let mut rotated_keys = FolderContentKeys::genesis(gen1_key, 1_000);
    rotated_keys.rotate(gen2_key, 2_000);
    let rotated = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(rotated_keys),
    );
    store.chunks.lock().unwrap().clear();
    for sk in &store_keys {
        rotated
            .db()
            .enqueue_transfer(rel, "upload", *sk, 0)
            .expect("enqueue resume entry");
    }

    rotated
        .drain_pending_uploads()
        .await
        .expect("drain_pending_uploads");

    // NO gen-1 ciphertext was published after the rotation — the removed
    // member holds gen 1 irrevocably, so publishing these post-removal would
    // pierce the rotation boundary.
    {
        let uploaded = store.chunks.lock().unwrap();
        for sk in &store_keys {
            assert!(
                !uploaded.contains_key(&hex::encode(sk.digest())),
                "drain published gen-1 chunk {} after the rotation instead of requeuing \
                 the path under the current generation",
                hex::encode(sk.digest())
            );
        }
        assert!(
            !uploaded.is_empty(),
            "the requeue must have re-uploaded the file's chunks under gen 2"
        );
    }

    // The path was re-sealed + re-recorded under the CURRENT generation: a
    // fresh manifest exists whose ciphertext store keys are gen-2 (disjoint
    // from gen-1) and fully present in the store — nothing was dropped.
    let new_manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("requeue must have POSTed a new manifest");
    assert_ne!(
        new_manifest_hash, manifest_hash,
        "the requeue records a NEW manifest (gen-2 store keys), not the gen-1 one"
    );
    let new_manifest: fauna_core::chunk::ChunkManifest = fauna_core::encoding::canonical_decode(
        store
            .manifests
            .lock()
            .unwrap()
            .get(&hex::encode(new_manifest_hash.digest()))
            .expect("new manifest present in the store"),
    )
    .expect("decode new manifest");
    let uploaded = store.chunks.lock().unwrap();
    for sk in new_manifest.store_keys() {
        assert!(
            !store_keys.contains(&sk),
            "a gen-2 store key must differ from every gen-1 key (different generation root)"
        );
        assert!(
            uploaded.contains_key(&hex::encode(sk.digest())),
            "requeued gen-2 chunk {} missing from the store",
            hex::encode(sk.digest())
        );
    }
    drop(uploaded);

    // The stale gen-1 queue entries were completed only after the re-seal
    // landed — the queue is drained, so the next pass has nothing to re-detect.
    assert!(
        rotated
            .db()
            .eligible_transfers("upload")
            .unwrap()
            .is_empty(),
        "stale gen-1 entries must be completed once the requeue lands"
    );
}

/// Slice-3 rotate-on-removal, crypto layer (OBS-1): a member holding **only**
/// generation 1 can still read gen-1 content (history) but **fails closed** on
/// gen-2 content sealed after a rotation — the forward-secrecy property the M2
/// content key provides (`mls-group-key-material.md` § M2; the engine-level
/// precursor to the piece-7 tier_3 "removed member can't read post-removal
/// content" round-trip).
#[tokio::test]
async fn bound_engine_rotation_gen1_readable_gen2_fail_closed() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let key1 = [1u8; 32];
    let key2 = [2u8; 32];

    // Owner at generation 1 uploads file A (sealed under gen 1, version 1).
    let watch_a = tempfile::tempdir().unwrap();
    let owner_gen1 = build_test_engine(
        &server.uri(),
        watch_a.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(FolderContentKeys::genesis(key1, 1_000)),
    );

    let content_a: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(13) % 251) as u8)
        .collect();
    let rel_a = "shared/gen1.bin";
    let full_a = watch_a.path().join(rel_a);
    std::fs::create_dir_all(full_a.parent().unwrap()).unwrap();
    std::fs::write(&full_a, &content_a).unwrap();
    owner_gen1
        .upload_file(rel_a)
        .await
        .expect("upload gen-1 file");
    let manifest_a = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("gen-1 manifest posted");

    // Owner rotates to generation 2 (member removed) and uploads file B (sealed
    // under gen 2, version 2). This holder retains gen 1 in `prior` (history).
    let mut rotated = FolderContentKeys::genesis(key1, 1_000);
    rotated.rotate(key2, 2_000);
    assert_eq!(rotated.current_version(), 2);
    let watch_b = tempfile::tempdir().unwrap();
    let owner_gen2 = build_test_engine(
        &server.uri(),
        watch_b.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(rotated),
    );
    let content_b: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(29) % 251) as u8)
        .collect();
    let rel_b = "shared/gen2.bin";
    let full_b = watch_b.path().join(rel_b);
    std::fs::create_dir_all(full_b.parent().unwrap()).unwrap();
    std::fs::write(&full_b, &content_b).unwrap();
    owner_gen2
        .upload_file(rel_b)
        .await
        .expect("upload gen-2 file");
    let manifest_b = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("gen-2 manifest posted");

    // The removed member holds ONLY generation 1.
    let watch_r = tempfile::tempdir().unwrap();
    let removed_member = build_test_engine(
        &server.uri(),
        watch_r.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(FolderContentKeys::genesis(key1, 1_000)),
    );

    // (1) gen-1 content (version 1) is still readable — history-on-join.
    let bytes_a = removed_member
        .download_file_bytes_by_manifest(manifest_a, Some(1), rel_a)
        .await
        .expect("gen-1-only member must still read gen-1 content");
    assert_eq!(bytes_a, content_a);

    // (2) gen-2 content (version 2) FAILS CLOSED — `key_for(2)` is `None` for a
    // gen-1-only holder, and the read must error rather than fall through.
    let post_removal = removed_member
        .download_file_bytes_by_manifest(manifest_b, Some(2), rel_b)
        .await;
    assert!(
        post_removal.is_err(),
        "a gen-1-only (removed) member MUST fail closed on gen-2 (post-removal) content"
    );

    // (3) The owner who kept both generations reads BOTH.
    let owner_reader = build_test_engine(
        &server.uri(),
        tempfile::tempdir().unwrap().path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some({
            let mut ck = FolderContentKeys::genesis(key1, 1_000);
            ck.rotate(key2, 2_000);
            ck
        }),
    );
    assert_eq!(
        owner_reader
            .download_file_bytes_by_manifest(manifest_a, Some(1), rel_a)
            .await
            .expect("owner reads gen-1"),
        content_a
    );
    assert_eq!(
        owner_reader
            .download_file_bytes_by_manifest(manifest_b, Some(2), rel_b)
            .await
            .expect("owner reads gen-2"),
        content_b
    );
}

/// **A `stat` that fails for any reason OTHER than
/// "it is not there" must not resolve to "the bytes exist nowhere".**
///
/// `path_is_materialized` used to end `Err(_) => false`, so EACCES, EIO, ESTALE
/// on a blipped mount, ELOOP and an unmounted volume all read as *not on this
/// disk*. That was harmless while the answer only meant *abort*. That fix moved
/// it to a choke point where, for a **headless** entry (no recorded head to
/// fetch from the nest), "not on this disk" resolves to
/// `ResealDisposition::Nothing` — deliberately **not** a shortfall, so
/// `require_fully_recorded` passes and the pass returns `Ok` over a path that
/// never converged — every caller counting on `Ok` (the drain requeue; until
/// 2026-09-25 also a cross-device one-shot sentinel the `Ok` cleared
/// **permanently**) treats it as done. One transient fault, once.
///
/// **Why the sibling pin cannot catch it.**
/// `a_reseal_walk_converges_the_entries_behind_an_unmaterialized_one` uses a
/// MISSING file — ENOENT is the one input for which `Err(_) => false` is
/// *correct* — so it exercises the benign half of the conflation and is
/// structurally unable to distinguish it from the harmful half. This pin drives
/// the harmful half: a mode-0 parent directory, i.e. a real EACCES on a path
/// that **does** exist.
///
/// **The observable is the shortfall, not a log line**: the pass must not return
/// `Ok`. Before the fix it returned `Ok(0)` — the entry silently skipped, the
/// sentinel cleared.
#[tokio::test]
#[cfg(unix)]
async fn a_headless_entry_whose_stat_fails_is_not_read_as_existing_nowhere() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let owner = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(FolderContentKeys::genesis([9u8; 32], 1_000)),
    );

    // A HEADLESS pre-bind entry: `manifest_hash = None`, so the choke point has
    // no nest copy to fall back to — the exact door opened.
    let content: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let rel = "locked/headless.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &content).unwrap();
    let local_hash = ContentHash::of_raw(&content);
    owner
        .db()
        .upsert_entry(
            rel,
            Some(local_hash),
            Some(local_hash),
            None, // headless — no recorded head
            SyncState::Synced,
            0,
            0,
            content.len() as i64,
            1,
            None, // pre-bind, so the pass selects it
        )
        .unwrap();

    // Make the parent unreadable: the file is STILL THERE, but `stat` on it now
    // fails EACCES rather than ENOENT.
    let parent = full.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Precondition, asserted rather than assumed: root ignores mode bits, so
    // without this the fixture would silently degrade to "the file is readable"
    // and the test would pass for the wrong reason.
    let probe = std::fs::metadata(&full);
    let precondition_holds = matches!(&probe, Err(e) if e.kind() != std::io::ErrorKind::NotFound);
    if !precondition_holds {
        // Restore first, or the tempdir cannot be cleaned up.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        panic!(
            "fixture precondition: stat under a mode-0 parent must fail with something other \
             than NotFound (are these tests running as root?); got {probe:?}"
        );
    }

    let result = owner.reseal_pending_under_current().await;

    // Restore permissions before asserting, so a failure still leaves a
    // removable tempdir.
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = result.expect_err(
        "an unreadable path must NOT be resolved to `Nothing` — that returns Ok, reporting \
         the pass complete over an unconverged path",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("cannot tell whether its bytes are on this disk"),
        "the refusal must name the real cause — an unknown stat, not a missing file; got: {msg}"
    );
}

/// M2 pre-bind re-seal migration, Piece A (`mls-group-key-material.md` § M2
/// *Pre-bind re-seal migration*): a file recorded **before** the set was bound
/// (`content_key_version = None` — owner-only `BackupKey`/plaintext) is undecryptable
/// by a joiner (no content-key generation opens it). `reseal_pending_under_current`
/// re-seals it under `current`, so a member holding only that generation now decrypts
/// it end-to-end — the read gap closed. Additive (no chunk deleted).
#[tokio::test]
async fn reseal_pending_reseals_prebind_file_so_member_decrypts() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let key1 = [9u8; 32];

    // The owner's now-bound engine holds file F on disk with a Synced SyncDb entry
    // recorded PRE-BIND (`content_key_version = None`) — the state after a set that
    // already contained files is bound to a group (bind_set records the genesis key
    // but re-seals nothing).
    let watch = tempfile::tempdir().unwrap();
    let owner = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(FolderContentKeys::genesis(key1, 1_000)),
    );
    let content: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(17) % 251) as u8)
        .collect();
    let rel = "shared/prebind.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &content).unwrap();
    // Production shape: the original pre-bind upload left `local_hash` equal to
    // the on-disk content (upload_file's final upsert). The re-seal MUST bypass
    // the "already synced, skipping" short-circuit for exactly this row — a
    // `local_hash: None` seed would green even if it didn't (the Piece A skip
    // bug this seeding pins).
    let local_hash = fauna_core::data::ContentHash::of_raw(&content);
    owner
        .db()
        .upsert_entry(
            rel,
            Some(local_hash),
            Some(local_hash),
            None,
            SyncState::Synced,
            0,
            0,
            content.len() as i64,
            1,
            None, // pre-bind: sealed under the owner-only path, no generation
        )
        .unwrap();

    // Re-seal the set: the one pre-bind file is re-uploaded under generation 1.
    // (Its change record cannot land here — see `expect_unrecorded`; the sealed
    // BYTES are what this test is about, and they provably moved below.)
    expect_unrecorded(owner.reseal_pending_under_current().await, 1);

    // The re-sealed manifest is now sealed under the group content key; a member
    // holding ONLY that generation decrypts it end-to-end (the read gap closed).
    let manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("re-sealed manifest posted");
    let member = build_test_engine(
        &server.uri(),
        tempfile::tempdir().unwrap().path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(FolderContentKeys::genesis(key1, 1_000)),
    );
    let bytes = member
        .download_file_bytes_by_manifest(manifest, Some(1), rel)
        .await
        .expect("member decrypts the re-sealed pre-bind file under generation 1");
    assert_eq!(
        bytes, content,
        "re-sealed content round-trips under the content key"
    );
}

/// M2 fail-closed reader guard (`SyncEngine::content_open_roots`, engine.rs — the
/// `version = None` branch, doc'd "A bound chunk with no stamped version … is
/// likewise an `Err` — the reader must not guess a generation"): a **bound**
/// engine that HAS its content keys loaded, asked to open a content-key-sealed
/// chunk that carries **no** stamped content-key version, MUST fail closed — it
/// must not guess a generation and attempt a decrypt under a guessed key. This is
/// the pre-reseal read of a pre-bind file (`content_key_version = None`) by a
/// bound member, the window before `reseal_pending_under_current` re-stamps it
/// (the complement of `reseal_pending_reseals_prebind_file_so_member_decrypts`
/// above, which proves the *post*-reseal `Some(1)` read succeeds). Every other
/// fail-closed branch of `content_open_roots`/`content_seal_root` is pinned
/// (bound-but-keyless seal+open in `bound_but_unkeyed_engine_fails_closed…`,
/// `keys_for`-empty post-rotation); this pins the `content_keys = Some`,
/// `version = None` branch, the one guard that had setup but no assertion.
#[tokio::test]
async fn bound_engine_refuses_to_open_chunk_with_no_version_stamp() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let key1 = [7u8; 32];

    // A bound owner engine (content keys loaded, generation 1) seals a file under
    // `current`, posting a real content-key-sealed manifest to the nest.
    let watch = tempfile::tempdir().unwrap();
    let owner = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(FolderContentKeys::genesis(key1, 1_000)),
    );
    let content: Vec<u8> = (0..120_000u32)
        .map(|i| (i.wrapping_mul(13) % 251) as u8)
        .collect();
    let rel = "shared/no-version.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &content).unwrap();
    owner
        .upload_file(rel)
        .await
        .expect("bound owner seals the file under generation 1");
    let manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("content-key-sealed manifest posted");

    let member = build_test_engine(
        &server.uri(),
        tempfile::tempdir().unwrap().path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(FolderContentKeys::genesis(key1, 1_000)),
    );

    // Positive control: WITH the correct generation stamp the bound member reads
    // the chunk fine — so the manifest is genuinely readable, and the None read
    // below fails *specifically* because the version is missing, not because the
    // manifest is broken.
    assert_eq!(
        member
            .download_file_bytes_by_manifest(manifest, Some(1), rel)
            .await
            .expect("reads fine with the correct generation stamp"),
        content,
    );

    // The guard: the SAME member reading the SAME content-key-sealed chunk with NO
    // version stamp must fail closed — refusing to guess the generation, never
    // attempting a decrypt under a guessed key.
    let err = member
        .download_file_bytes_by_manifest(manifest, None, rel)
        .await
        .expect_err("a bound engine must refuse to open a chunk carrying no version stamp");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("refusing to guess the generation"),
        "fail-closed error must name the missing-version guard, got: {msg}"
    );
}

/// WebDAV serve-enable round-trip (`webdav-server.md` § Key model — slice 2's
/// success line): a **served-but-unshared** set (content keys at the serve
/// pseudo-channel, NO MLS group — the `EngineKeyBinding::ServedUnshared` engine
/// args `(None, Some(keys))`) holding a pre-existing owner-path file
/// (`content_key_version = None`) is re-sealed under `current` by the same
/// Piece-A pass; the MDA side then recovers the generations from a sealed →
/// unsealed `WebdavKeysBlob` (the slice-1b key plane) and decrypts the
/// historical file end-to-end. Group-agnosticism is the load-bearing property:
/// neither the re-seal nor the read needs an MLS group.
#[tokio::test]
async fn served_unshared_reseal_then_mda_decrypts_via_webdav_keys_blob() {
    use fauna_mls::wrapped_blob::{
        ServedSetKeys, WebdavKeysBlob, WebdavKeysPlaintext, seal_webdav_keys_blob,
        unseal_webdav_keys_blob,
    };

    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    // Serve-enable's genesis: content keys exist, no group (custody would key
    // them by `serve_custody_channel_id("__test")` — irrelevant to the engine,
    // which receives the resolved generations).
    let genesis = FolderContentKeys::genesis([0x5d; 32], 1_000);

    // Owner's served engine: group-less + content-keyed, with a pre-serve file
    // recorded on the owner-only path.
    let watch = tempfile::tempdir().unwrap();
    let owner = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        None, // no MLS group — served-but-unshared
        Some(genesis.clone()),
    );
    let content: Vec<u8> = (0..120_000u32)
        .map(|i| (i.wrapping_mul(31) % 249) as u8)
        .collect();
    let rel = "served/history.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &content).unwrap();
    owner
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            None,
            SyncState::Synced,
            0,
            0,
            content.len() as i64,
            1,
            None, // pre-serve: owner-only path, no generation stamp
        )
        .unwrap();

    // The pre-bind re-seal pass (the sync agent's ungated run on every engine
    // start) re-seals the back-catalogue under `current`.
    expect_unrecorded(owner.reseal_pending_under_current().await, 1);

    // MDA side: recover the served set's generations from the MSEK-sealed
    // WebdavKeysBlob (what `reconcile_webdav_keys_blob` provisioned and
    // `fetch_webdav_keys_blob` returns), then decrypt the re-sealed file.
    let msek = [0x77u8; 32];
    let actor = [0xA1u8; 32];
    let plaintext = WebdavKeysPlaintext::new(vec![ServedSetKeys {
        set_name: "__test".into(),
        read_only: false,
        keys: genesis.clone(),
    }])
    .to_canonical_bytes()
    .expect("encode plaintext");
    let wire = seal_webdav_keys_blob(&plaintext, &actor, &msek)
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode blob");
    let recovered = WebdavKeysPlaintext::from_canonical_bytes(
        &unseal_webdav_keys_blob(
            &WebdavKeysBlob::from_canonical_bytes(&wire).expect("decode blob"),
            &msek,
        )
        .expect("unseal"),
    )
    .expect("decode plaintext");
    let mda_keys = recovered.served_sets[0].keys.clone();
    assert_eq!(mda_keys, genesis, "the blob carries the served generations");

    let manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("re-sealed manifest posted");
    // The MDA's reader is group-less too — blob-recovered keys only.
    let mda = build_test_engine(
        &server.uri(),
        tempfile::tempdir().unwrap().path().to_path_buf(),
        None,
        None,
        None,
        Some(mda_keys),
    );
    let bytes = mda
        .download_file_bytes_by_manifest(manifest, Some(1), rel)
        .await
        .expect("MDA decrypts the historical file under the blob-recovered key");
    assert_eq!(
        bytes, content,
        "end-to-end: serve-enable → re-seal → DAV read"
    );
}

/// An **unbound** (owner-only) engine has no content-key generation to re-seal
/// under, so the migration pass is a no-op — it never re-uploads the owner's chunks.
#[tokio::test]
async fn reseal_pending_is_noop_for_unbound_engine() {
    let server = MockServer::start().await;
    MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    // Unbound: mls_group_id = None, content_keys = None (owner-only).
    let owner = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        None,
        None,
    );
    owner
        .db()
        .upsert_entry(
            "a.bin",
            None,
            None,
            None,
            SyncState::Synced,
            0,
            0,
            10,
            1,
            None,
        )
        .unwrap();
    let n = owner
        .reseal_pending_under_current()
        .await
        .expect("no-op succeeds");
    assert_eq!(n, 0, "an unbound engine re-seals nothing");
}

/// FS-BIND-5, under M2: a **bound** engine
/// (`mls_group_id = Some`) with **no content keys loaded** (`content_keys =
/// None`) MUST **fail closed** — the upload errors and stores nothing, never
/// falling through to a plaintext upload of a shared, cross-user, private file
/// set to the untrusted nest. The bound marker, not the key material, drives the
/// decision: a removed member with stale config, or a startup race before the
/// rotate-on-removal orchestration loaded the keys. The presence/absence of an
/// `MlsEngine` is irrelevant to chunk keying under M2, so we cover both.
#[tokio::test]
async fn bound_but_unkeyed_engine_fails_closed_and_uploads_nothing() {
    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    let rel = "shared/secret.bin";
    let group_id = test_group_id();

    // Cover both shapes: with and without an MlsEngine. Under M2 neither supplies
    // the chunk root (that is `content_keys`), so both must fail closed.
    for with_mls in [true, false] {
        let server = MockServer::start().await;
        let store = MockNest::new().with_blob_plane().mount(&server).await;

        let mls =
            with_mls.then(|| Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap()));
        let watch = tempfile::tempdir().unwrap();
        let bound = build_test_engine(
            &server.uri(),
            watch.path().to_path_buf(),
            mls,
            None,
            Some(group_id.clone()),
            None, // bound but no content keys loaded
        );
        let full = watch.path().join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, &original).unwrap();

        let result = bound.upload_file(rel).await;
        assert!(
            result.is_err(),
            "a bound-but-unkeyed engine (with_mls={with_mls}) must FAIL the upload, not seal plaintext"
        );
        assert!(
            store.chunks.lock().unwrap().is_empty(),
            "nothing must be uploaded — a plaintext chunk on the nest is an irrecoverable leak"
        );
    }
}

/// FS-BIND FOLLOW-ON A (user-ratified 2026-07-07): the owner-only `backup_key`
/// segment-backup path (`upload_bytes`) must round-trip through a store that
/// enforces the real chunk route's F9 anti-poisoning check. Pre-fix, the
/// random-nonce `encrypt_backup_chunk` seal kept plaintext-hash keying, every
/// chunk was rejected (400), and the silent swallow reported success anyway —
/// cross-location segment backup stored nothing. The convergent
/// `BackupKey::convergent_chunk_root` seal keys each chunk by its ciphertext
/// hash, so `blake3(body) == X-Content-Hash` holds and the store accepts it.
/// Also pins the convergence property itself: a second pass over the same bytes
/// produces the identical store-key set (dedup / idempotent retry).
#[tokio::test]
async fn backup_key_upload_bytes_round_trips_through_verifying_store() {
    let server = MockServer::start().await;
    let store = MockNest::new()
        .verifying()
        .with_blob_plane()
        .mount(&server)
        .await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x51u8; 32])),
    );

    // Multi-chunk payload (>64 KiB forces FastCDC boundaries).
    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(29) % 251) as u8)
        .collect();

    let manifest_hash = engine
        .upload_bytes(original.clone(), "backup/segment-0001.seg", "__backup")
        .await
        .expect("upload_bytes must succeed against an F9-verifying store");

    // The store actually holds the chunks (the pre-fix bug: it held nothing).
    let first_pass_keys: std::collections::BTreeSet<String> =
        store.chunks.lock().unwrap().keys().cloned().collect();
    assert!(
        !first_pass_keys.is_empty(),
        "the verifying store must have accepted + stored the sealed chunks"
    );

    // Round-trip: the owner decrypts the stored backup end-to-end.
    let bytes = engine
        .download_file_bytes_by_manifest(manifest_hash, None, "backup/segment-0001.seg")
        .await
        .expect("download_file_bytes_by_manifest (convergent backup seal)");
    assert_eq!(
        bytes, original,
        "decrypted backup bytes must equal the originally uploaded segment"
    );

    // Convergence: re-uploading the same bytes seals to the identical
    // ciphertext, so the store-key set is unchanged (dedup-stable address).
    engine
        .upload_bytes(original.clone(), "backup/segment-0001.seg", "__backup")
        .await
        .expect("second upload_bytes pass");
    let second_pass_keys: std::collections::BTreeSet<String> =
        store.chunks.lock().unwrap().keys().cloned().collect();
    assert_eq!(
        first_pass_keys, second_pass_keys,
        "convergent seal: a re-upload must produce the identical store-key set"
    );
}

/// The FOLLOW-ON A silent-swallow pin: `upload_bytes` (the segment-backup path,
/// `enqueue_resume = false` — no `transfer_queue` resume) must return `Err` when
/// the destination rejects chunk uploads, not report the segment as uploaded.
/// Pre-fix it ignored the per-chunk `UploadResult`s and returned `Ok`, letting
/// a backup coordinator advance `segment_backup_state` over a backup that
/// stored nothing (a no-user-data-loss landmine).
#[tokio::test]
async fn upload_bytes_fails_loud_when_store_rejects_chunks() {
    let server = MockServer::start().await;

    // check → everything missing; chunks → always rejected; manifests → accept
    // (must never be reached: the bail fires before the manifest upload).
    Mock::given(method("POST"))
        .and(path("/api/v1/chunks/check"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            let missing = body.get("hashes").cloned().unwrap_or_default();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "missing": missing }))
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chunks"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let manifest_posts = Arc::new(Mutex::new(0usize));
    {
        let manifest_posts = Arc::clone(&manifest_posts);
        Mock::given(method("POST"))
            .and(path("/api/v1/manifests"))
            .respond_with(move |_req: &Request| {
                *manifest_posts.lock().unwrap() += 1;
                ResponseTemplate::new(200)
            })
            .mount(&server)
            .await;
    }

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x52u8; 32])),
    );

    let payload: Vec<u8> = (0..120_000u32)
        .map(|i| (i.wrapping_mul(13) % 251) as u8)
        .collect();
    let result = engine
        .upload_bytes(payload, "backup/segment-0002.seg", "__backup")
        .await;

    assert!(
        result.is_err(),
        "upload_bytes must fail loud when the destination rejects its chunks"
    );
    assert_eq!(
        *manifest_posts.lock().unwrap(),
        0,
        "the manifest must not be uploaded after chunk rejection — nothing may record the pass as done"
    );
}

/// Store a hand-built **plaintext** at-rest shape: chunks stored under their
/// plaintext hash (compressed exactly as the plaintext upload path stores
/// them), manifest with `stored_hashes = None` — the shape a plaintext
/// file rests in (a `public`-audience folder's corpus, or an entry a
/// flip-back re-seal pass has not reached yet).
fn store_plaintext_fixture(store: &crate::test_support::BlobStore, original: &[u8]) -> ContentHash {
    let manifest = fauna_core::chunker::chunk_file(original);
    assert!(manifest.stored_hashes.is_none());
    for (hash, data) in fauna_core::chunker::extract_chunks(original, &manifest) {
        let compressed = fauna_core::compress::compress_chunk_framed(
            &data,
            fauna_core::compress::ChunkFraming::FILE_SYNC,
        );
        store
            .chunks
            .lock()
            .unwrap()
            .insert(hex::encode(hash.digest()), compressed);
    }
    let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    store
        .manifests
        .lock()
        .unwrap()
        .insert(hex::encode(manifest_hash.digest()), manifest_bytes);
    manifest_hash
}

/// The manifest is self-describing (`stored_hashes = None` ⇒ plaintext,
/// **whatever keys the reader holds**): a KEYED engine reading a plaintext
/// manifest passes it through instead of misreading it as a framed
/// seal. This is a repro — the
/// windows on-demand hydration host is exactly this keyed reader, and before
/// the fix every owner-only hydration hard-failed the framed version-byte
/// check. It is also what lets the flip-back re-seal pass read the
/// plaintext entries it re-seals. (The branch this replaced guarded a "framed"
/// corpus that provably never rested anywhere: framed uploads were
/// rejected 400 by the F9 route — FS-BIND FOLLOW-ON A.)
#[tokio::test]
async fn keyed_engine_reads_plaintext_manifest_passthrough() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let original: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(19) % 251) as u8)
        .collect();
    let manifest_hash = store_plaintext_fixture(&store, &original);

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x53u8; 32])),
    );

    let bytes = engine
        .download_file_bytes_by_manifest(manifest_hash, None, "docs/plain.bin")
        .await
        .expect("a keyed reader must pass a plaintext manifest (no stored_hashes) through");
    assert_eq!(bytes, original, "plaintext files must round-trip");
}

/// The fail-closed half of the self-describing read: a SEALED manifest
/// (`stored_hashes` present) read by an engine with **no key material** errors
/// loudly — never a ciphertext passthrough (which would hand raw ciphertext to
/// the reassembler and, on paths that write before verifying, corrupt the
/// local file silently).
#[tokio::test]
async fn keyless_engine_fails_closed_on_sealed_manifest() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    // Seed a sealed owner-only upload.
    let up_watch = tempfile::tempdir().unwrap();
    let uploader = test_sync_engine(
        &server.uri(),
        up_watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x57u8; 32])),
    );
    let original: Vec<u8> = (0..120_000u32)
        .map(|i| (i.wrapping_mul(23) % 251) as u8)
        .collect();
    std::fs::write(up_watch.path().join("doc.bin"), &original).unwrap();
    uploader.upload_file("doc.bin").await.expect("seed upload");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("manifest posted");

    // A keyless reader (no BackupKey, no content keys) must fail closed.
    let watch = tempfile::tempdir().unwrap();
    let reader = test_sync_engine(&server.uri(), watch.path().to_path_buf(), None);
    let err = reader
        .download_file_bytes_by_manifest(manifest_hash, None, "doc.bin")
        .await
        .expect_err("a keyless reader must not pass sealed chunks through");
    assert!(
        err.to_string().contains("fail closed") || format!("{err:#}").contains("fail closed"),
        "the error names the fail-closed posture, got: {err:#}"
    );
}

/// The owner-only plaintext flip-back re-seal pass
/// (`SyncEngine::reseal_owner_only_plaintext`): a `Synced`
/// entry whose recorded manifest is plaintext is re-uploaded sealed
/// (verified end-to-end), and the pass terminates — every entry is marked in
/// the local `SyncDb` so the steady-state pass is a no-op.
#[tokio::test]
async fn reseal_owner_only_plaintext_converges_and_terminates() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x58u8; 32])),
    );

    // The plaintext state: the file on disk, its PLAINTEXT manifest + chunks at
    // rest on the nest, and a Synced entry recording that manifest.
    let original: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(29) % 251) as u8)
        .collect();
    let rel = "docs/plain.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let plain_manifest_hash = store_plaintext_fixture(&store, &original);
    let file_hash = ContentHash::of_raw(&original);
    engine
        .db()
        .upsert_entry(
            rel,
            Some(file_hash),
            Some(file_hash),
            Some(plain_manifest_hash),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed plaintext entry");
    engine.stamp_own_head_for_test(rel);

    // The pass re-seals the one plaintext entry…
    expect_unrecorded(engine.reseal_owner_only_plaintext().await, 1);

    // …the re-uploaded manifest is genuinely sealed (stored_hashes present,
    // chunks keyed by ciphertext hash) and round-trips…
    let sealed_manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("re-seal POSTed a manifest");
    assert_ne!(sealed_manifest_hash, plain_manifest_hash);
    let manifest_bytes = store
        .manifests
        .lock()
        .unwrap()
        .get(&hex::encode(sealed_manifest_hash.digest()))
        .cloned()
        .expect("sealed manifest stored");
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).unwrap();
    assert!(
        manifest.stored_hashes.is_some(),
        "the re-sealed manifest must be sealed"
    );
    let bytes = engine
        .download_file_bytes_by_manifest(sealed_manifest_hash, None, rel)
        .await
        .expect("sealed copy round-trips");
    assert_eq!(bytes, original);

    // …and the entry stays OWED, because its change record did not land. The
    // marker is gated on the record, not the upload: `owner_sealed`
    // means "this path's manifest is known sealed", and while the nest head
    // still names the plaintext manifest that is false — marking it would make
    // the mark-gated pass skip the path forever and strand it plaintext.
    assert!(
        !engine.db().get_entry(rel).unwrap().unwrap().owner_sealed,
        "an unrecorded re-seal leaves the entry owed, not marked done"
    );
    // So the pass RETRIES rather than steadying — the correct behaviour for
    // work that did not land. Termination-once-recorded is the same marker read
    // one step later, and it needs a nest that accepts records: it is pinned in
    // `bins/fauna-nest/tests/conformance_post_succession_reseal.rs` and, for the
    // audience passes, in `conformance_shared_folders.rs`.
    expect_unrecorded(engine.reseal_owner_only_plaintext().await, 1);
}

/// **One un-materialized entry must not decide the fate
/// of the entries behind it.**
///
/// `upload_file_inner`'s choke point refuses a path whose bytes are not on this
/// disk, and refusing is right. What was wrong is what happened next: five of
/// the six passes that reach it `?`'d that `bail!` straight out of their walk,
/// so a single un-materialized entry aborted the whole pass. The owner-only
/// migration left every later entry resting **plaintext** on the nest; the
/// declassify left the published site partly dark; and `reissue_corpus_for_web`
/// — which has no per-entry sentinel — re-force-uploaded and re-recorded the
/// entire prefix ahead of the failing entry once per rescan tick, forever.
///
/// The remedy is the widening `declassify_owner_corpus`' own doc already named
/// and `reseal_predecessor_sealed` was already practising: a path with no local
/// bytes but a recorded head is fetched **from the nest** instead of aborting
/// (`file-sync.md` § the six-state vocabulary — RemoteOnly is "on the nest only
/// … not yet hydrated"). It now lives in the shared trio, so no caller can
/// forget it.
///
/// **Why a missing file is the honest fixture here.** `path_is_materialized` is
/// false for exactly two things — a cloud-only placeholder and a file that is
/// not there — and both took the same abort. Only Windows can mint a real
/// placeholder (`placeholder.rs`'s own OFFLINE-attribute test is `#[cfg(windows)]`),
/// so the missing-file arm is what a cross-platform pin can drive; it is the
/// same branch, the same `bail!`, and the same consequence.
///
/// **Non-vacuity.** The assertion is the shortfall count: `2 of 2` can only be
/// reached if the walk visited BOTH entries. Before the fix it aborted at the
/// un-materialized one with a `stat` error and never counted anything — so this
/// is order-independent, which matters because `list_by_state` carries no
/// `ORDER BY`.
#[tokio::test]
async fn a_reseal_walk_converges_the_entries_behind_an_unmaterialized_one() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x71u8; 32])),
    );

    // TWO plaintext entries, both `Synced` and both resting PLAINTEXT on the nest
    // (the owner-only pass's predicate). They differ in exactly one way: the
    // first has no bytes on this disk.
    let absent_bytes: Vec<u8> = (0..90_000u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    let present_bytes: Vec<u8> = (0..110_000u32)
        .map(|i| (i.wrapping_mul(37) % 241) as u8)
        .collect();
    let absent_rel = "docs/a-not-on-this-disk.bin";
    let present_rel = "docs/b-right-here.bin";

    // The un-materialized one: a recorded head on the nest, no local file.
    let absent_manifest = store_plaintext_fixture(&store, &absent_bytes);
    // The materialized one: a recorded head AND the bytes on disk.
    let present_manifest = store_plaintext_fixture(&store, &present_bytes);
    let present_full = watch.path().join(present_rel);
    std::fs::create_dir_all(present_full.parent().unwrap()).unwrap();
    std::fs::write(&present_full, &present_bytes).unwrap();

    for (rel, manifest, bytes) in [
        (absent_rel, absent_manifest, &absent_bytes),
        (present_rel, present_manifest, &present_bytes),
    ] {
        let hash = ContentHash::of_raw(bytes);
        engine
            .db()
            .upsert_entry(
                rel,
                Some(hash),
                Some(hash),
                Some(manifest),
                SyncState::Synced,
                1,
                1,
                bytes.len() as i64,
                1,
                None,
            )
            .expect("seed plaintext entry");
        engine.stamp_own_head_for_test(rel);
    }
    assert!(
        !watch.path().join(absent_rel).exists(),
        "precondition: the first entry's bytes are genuinely not on this disk"
    );

    // THE SUBJECT: the walk reaches BOTH entries. `2 of 2` is only reachable if
    // the un-materialized one was re-sealed (from the nest) rather than
    // aborting the pass — before the fix this returned the `stat` error instead.
    expect_unrecorded(engine.reseal_owner_only_plaintext().await, 2);

    // Both were genuinely re-sealed on the byte plane: two NEW manifests, each
    // carrying `stored_hashes`, neither equal to the plaintext head it replaced.
    let manifests = store.manifests.lock().unwrap().clone();
    let sealed: Vec<_> = manifests
        .iter()
        .filter_map(|(hex_hash, raw)| {
            let m: fauna_core::chunk::ChunkManifest =
                fauna_core::encoding::canonical_decode(raw).ok()?;
            m.stored_hashes.is_some().then_some(hex_hash.clone())
        })
        .collect();
    assert_eq!(
        sealed.len(),
        2,
        "both entries re-sealed — the un-materialized one from the nest, the other from disk"
    );
    for plain in [absent_manifest, present_manifest] {
        assert!(
            !sealed.contains(&hex::encode(plain.digest())),
            "a re-sealed manifest must not be the plaintext one it replaced"
        );
    }

    // Neither is marked done, because neither change record landed in this
    // harness — the discipline, unchanged by the widening. So the next
    // pass re-drives both rather than stranding either.
    for rel in [absent_rel, present_rel] {
        assert!(
            !engine.db().get_entry(rel).unwrap().unwrap().owner_sealed,
            "an unrecorded re-seal leaves {rel} owed"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// The post-succession corpus re-seal (`succession-aftermath.md` § Re-key scope)
// ─────────────────────────────────────────────────────────────────────

/// Stock the store with `original` sealed under `root` the way the upload path
/// seals it — hash the plaintext, compress, encrypt, address by ciphertext,
/// seal the manifest's hashes — and return the manifest's content hash. This is a *predecessor's*
/// corpus as a successor finds it: the manifest readable, every chunk body under
/// a root the successor does not derive.
pub(crate) fn store_sealed_fixture(
    store: &crate::test_support::BlobStore,
    original: &[u8],
    root: &[u8; 32],
) -> ContentHash {
    let plain = fauna_core::chunker::chunk_file(original);
    let mut stored_hashes = Vec::new();
    for (hash, data) in fauna_core::chunker::extract_chunks(original, &plain) {
        let (store_key, ciphertext) = crate::seal::seal_chunk_body(&hash, &data, root).unwrap();
        stored_hashes.push(store_key);
        store
            .chunks
            .lock()
            .unwrap()
            .insert(hex::encode(store_key.digest()), ciphertext);
    }
    let manifest = fauna_core::chunk::ChunkManifest {
        stored_hashes: Some(stored_hashes),
        ..plain
    };
    let manifest_bytes =
        fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(root)).unwrap()).unwrap();
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    store
        .manifests
        .lock()
        .unwrap()
        .insert(hex::encode(manifest_hash.digest()), manifest_bytes);
    manifest_hash
}

/// Seed a `Synced` entry pointing at `manifest_hash`, the shape the pass scans.
pub(crate) fn seed_synced_entry(
    engine: &SyncEngine,
    rel: &str,
    original: &[u8],
    manifest_hash: ContentHash,
) {
    let file_hash = ContentHash::of_raw(original);
    engine
        .db()
        .upsert_entry(
            rel,
            Some(file_hash),
            Some(file_hash),
            Some(manifest_hash),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed entry");
    engine.stamp_own_head_for_test(rel);
}

/// A successor's materialized file, at rest under the **predecessor's** root, is
/// re-sealed under the successor's own.
///
/// This is the write half of § Re-key scope's `BackupKey` corpus row: until it
/// runs, the retired seed is the only thing that can open the corpus, which is
/// the device-loss race the ratified blockquote describes.
///
/// ⚠ Asserts the *byte plane* only. The entry's `current_root_sealed` sentinel
/// is gated on the change RECORD landing, and every engine test in this crate
/// deliberately runs an unconnected `NestClient` (see this module's doc) — so
/// the marking half lives in `bins/fauna-nest/tests/conformance_post_succession_reseal.rs`
/// against a real nest, exactly as `record_head_commit_wiring_test.rs` splits
/// `commit_recorded_head`'s failure/success arms.
#[tokio::test]
async fn post_succession_reseal_moves_a_predecessor_sealed_file_to_the_current_root() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x11u8; 32]);
    let successor = BackupKey::from_bytes([0x22u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    // The successor's corpus: bytes on disk (this device holds the set), the
    // chunks at rest sealed under the identity it succeeded from.
    let original: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    let rel = "docs/inherited.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());
    seed_synced_entry(&engine, rel, &original, pred_manifest);

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    // The re-sealed copy opens under the successor's own root with NO
    // predecessor material — the property that ends the retired seed's life.
    let resealed_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the re-seal POSTed a manifest");
    assert_ne!(resealed_hash, pred_manifest);
    let mut successor_only = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    successor_only.set_predecessor_backup_keys(vec![]);
    let bytes = successor_only
        .download_file_bytes_by_manifest(resealed_hash, None, rel)
        .await
        .expect("the re-sealed copy opens under the successor's key alone");
    assert_eq!(bytes, original);
}

/// A successor's file whose bytes are **not on this disk** is re-sealed from the
/// NEST's own copy — the case the aftermath actually exists for.
///
/// `reseal_path_under_current`'s local leg cannot touch this entry at all
/// (`upload_file_inner`'s choke point refuses a cloud-only placeholder, and here
/// there is no file at all), so before the nest-sourced leg the pass POSTed
/// nothing and left the entry to rot under a retired root. A successor restoring
/// onto a fresh device holds *nothing* materialized, so this — not the
/// materialized twin above — is the dominant real shape.
///
/// ⚠ Non-vacuity: the assertion is that a manifest was POSTed **and** that it
/// opens under the successor's key ALONE. Removing the nest-sourced arm turns
/// the first half red (nothing is POSTed); sealing the re-upload under anything
/// but the current root turns the second half red. Neither can pass on the
/// arrangement of the fixture.
#[tokio::test]
async fn post_succession_reseal_sources_an_unmaterialized_entry_from_the_nest() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x55u8; 32]);
    let successor = BackupKey::from_bytes([0x66u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    // Recorded + at rest under the predecessor, never materialized here: no
    // file is written into the watch dir at any point in this test.
    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 241) as u8).collect();
    let rel = "media/never-hydrated.bin";
    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());
    seed_synced_entry(&engine, rel, &original, pred_manifest);

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    let resealed_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the nest-sourced re-seal POSTed a manifest for a file this device never held");
    assert_ne!(
        resealed_hash, pred_manifest,
        "a NEW manifest, not the predecessor's"
    );

    let mut successor_only = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    successor_only.set_predecessor_backup_keys(vec![]);
    let bytes = successor_only
        .download_file_bytes_by_manifest(resealed_hash, None, rel)
        .await
        .expect("the re-sealed copy opens under the successor's key alone");
    assert_eq!(
        bytes, original,
        "and it is the same content — the re-seal moved the seal, not the bytes"
    );

    // Nothing was hydrated on the way: re-sealing must not materialize a corpus
    // this device deliberately does not hold.
    assert!(
        !watch.path().join(rel).exists(),
        "the nest-sourced re-seal must not write the file to the watch dir"
    );
}

/// The **WebDAV serve-toggle twin** of the two tests above (`webdav-server.md`
/// § Key model, Revocation ): `serve_disable` rotates a
/// group-less set's content key and drops it from the engine's *live* binding
/// (`content_keys = None`, `mls_group_id = None`), but the owner's own custody
/// still holds the retired M2 generation a served window sealed chunks under.
/// `set_retired_content_keys` offers that generation as a read candidate, and
/// the SAME shared walk that moves a predecessor-sealed file moves this one —
/// no separate pass, no separate sentinel.
///
/// Unmaterialized on purpose (mirroring
/// `post_succession_reseal_sources_an_unmaterialized_entry_from_the_nest`):
/// this is the shape that actually exercises the read wiring end to end — a
/// materialized file re-seals from its own plaintext on disk and never needs
/// the retired candidate at all, so only the nest-sourced leg proves the
/// engine can still OPEN what a served window sealed.
#[tokio::test]
async fn reseal_predecessor_sealed_also_converges_a_served_era_content_key_sealed_file() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let owner = BackupKey::from_bytes([0x77u8; 32]);
    let served_generation = FolderContentKeys::genesis([0x88u8; 32], 1_000);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(owner.clone()),
    );
    // The engine's LIVE binding is owner-only (no `content_keys`,
    // `mls_group_id`) — exactly what `resolve_engine_key_binding` resolves
    // for a group-less, now-unserved summary. Only the retired generation
    // survives, as a read candidate.
    engine.set_retired_content_keys(Some(served_generation.clone()));

    // Recorded + at rest under the served-window content key, never
    // materialized on this device — the fresh-device / on-demand shape that
    // forces the nest-sourced read leg.
    let original: Vec<u8> = (0..40_000u32)
        .map(|i| (i.wrapping_mul(7) % 233) as u8)
        .collect();
    let rel = "docs/served-while-on.bin";
    let served_manifest = store_sealed_fixture(&store, &original, served_generation.current_key());
    // The local row still carries the served-era generation's stamp — exactly
    // what `serve_disable` leaves behind: it flips the binding forward without
    // touching already-recorded rows.
    engine
        .db()
        .upsert_entry(
            rel,
            Some(ContentHash::of_raw(&original)),
            Some(ContentHash::of_raw(&original)),
            Some(served_manifest),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            Some(served_generation.current_version()),
        )
        .expect("seed entry");
    engine.stamp_own_head_for_test(rel);

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    let resealed_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the nest-sourced re-seal POSTed a manifest for a file this device never held");
    assert_ne!(
        resealed_hash, served_manifest,
        "a NEW manifest, not the served-era one"
    );

    // The re-sealed copy opens under the owner's OWN key alone — no retired
    // content-key candidate at all — the property that ends the rotated-out
    // generation's life for this file.
    let owner_only = test_sync_engine(&server.uri(), watch.path().to_path_buf(), Some(owner));
    let bytes = owner_only
        .download_file_bytes_by_manifest(resealed_hash, None, rel)
        .await
        .expect("the owner path opens the re-sealed copy with no served-era key at all");
    assert_eq!(
        bytes, original,
        "and it is the same content — the re-seal moved the seal, not the bytes"
    );

    assert!(
        !watch.path().join(rel).exists(),
        "the nest-sourced re-seal must not write the file to the watch dir"
    );
}

/// A **cloud-only placeholder** is owed a re-seal like any other at-rest entry.
///
/// The observable's first shape listed only `Synced` rows, which made it
/// structurally blind to its own dominant input: a successor restoring onto a
/// fresh device holds nothing *but* placeholders, so the pass reported "nothing
/// owed" for exactly the corpus still sealed to the retired identity — and that
/// is the report `sync-agent.md` bound (3) would have read before dropping the
/// only keys that can open it. Found by the tier_3 journey
/// (`bins/fauna-nest/tests/conformance_post_succession_reseal.rs`), pinned here
/// so a later narrowing of the query reds in seconds rather than in a nest build.
///
/// ⚠ Non-vacuity: this row differs from its sibling above in **state alone** —
/// same shape, same absence of local bytes. Narrowing
/// `list_pending_current_root_reseal` back to `Synced` leaves every other
/// re-seal test green and turns only this one red.
#[tokio::test]
async fn post_succession_reseal_reaches_a_cloud_only_placeholder() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x5bu8; 32]);
    let successor = BackupKey::from_bytes([0x6bu8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 233) as u8).collect();
    let rel = "docs/dehydrated.bin";
    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());

    // The row a dehydrated file leaves: the head is known, `local_hash` is NULL
    // because the bytes are on the nest rather than this disk.
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(pred_manifest),
            SyncState::Placeholder,
            0,
            0,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed placeholder entry");
    engine.stamp_own_head_for_test(rel);

    assert_eq!(
        engine
            .db()
            .list_pending_current_root_reseal()
            .unwrap()
            .len(),
        1,
        "a placeholder's bytes rest under SOME root, so it is owed a re-seal"
    );

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    let resealed_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the pass re-sealed the placeholder's bytes");
    assert_ne!(resealed_hash, pred_manifest);

    let mut successor_only = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    successor_only.set_predecessor_backup_keys(vec![]);
    let bytes = successor_only
        .download_file_bytes_by_manifest(resealed_hash, None, rel)
        .await
        .expect("the re-sealed copy opens under the successor's key alone");
    assert_eq!(bytes, original);
}

/// A re-sealed entry stays **owed** until its change record lands.
///
/// The sentinel means "the nest's head for this path is under the current root",
/// not "we uploaded something": with the record lost, the change log still names
/// the predecessor-sealed manifest, so every other device — and this one after a
/// dehydration — would still hydrate the retired-root copy. Marking it done
/// would let a completion check license `sync-agent.md` bound (3) to drop the
/// only keys that can open it, which is silent, unrecoverable darkness.
///
/// This crate's engines run an unconnected `NestClient`, so `record_change`
/// always fails here — which is exactly the arm under test. The success arm
/// (record lands → marked → the observable drains) needs a real nest and lives
/// in `bins/fauna-nest/tests/conformance_post_succession_reseal.rs`.
///
/// ⚠ Non-vacuity: the byte-plane work provably HAPPENED (a manifest was POSTed),
/// so "unmarked" is a statement about the gate rather than about the pass having
/// done nothing. Dropping the `recorded` gate turns both assertions red.
#[tokio::test]
async fn post_succession_reseal_leaves_an_entry_owed_until_its_change_record_lands() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x5au8; 32]);
    let successor = BackupKey::from_bytes([0x6au8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 239) as u8).collect();
    let rel = "docs/record-lost.bin";
    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());
    seed_synced_entry(&engine, rel, &original, pred_manifest);

    assert_eq!(
        engine
            .reseal_predecessor_sealed()
            .await
            .expect("re-seal pass"),
        0,
        "nothing COMPLETED — the count reports re-seals whose head actually moved"
    );
    assert!(
        store.last_manifest_hash.lock().unwrap().is_some(),
        "…even though the byte-plane re-seal did run (so the assertions below are \
         about the record gate, not about an inert pass)"
    );
    assert!(
        !engine
            .db()
            .get_entry(rel)
            .unwrap()
            .unwrap()
            .current_root_sealed,
        "the entry must NOT be marked: the nest head still names the predecessor-sealed manifest"
    );
    assert_eq!(
        engine
            .db()
            .list_pending_current_root_reseal()
            .unwrap()
            .len(),
        1,
        "and the completion observable still reports it, so bound (3) stays blocked",
    );
}

/// An entry ALREADY under the current root is marked without being re-uploaded.
///
/// This is the filter's real job: a successor's corpus is a mixture — everything
/// written after the ceremony is already current — and re-uploading those would
/// turn a one-time repair into a full-corpus rewrite on every start.
#[tokio::test]
async fn post_succession_reseal_marks_an_already_current_entry_without_reuploading() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let successor = BackupKey::from_bytes([0x33u8; 32]);
    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![BackupKey::from_bytes([0x44u8; 32]).into()]);

    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let rel = "docs/already-mine.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let current_manifest =
        store_sealed_fixture(&store, &original, &successor.convergent_chunk_root());
    seed_synced_entry(&engine, rel, &original, current_manifest);

    assert_eq!(
        engine
            .reseal_predecessor_sealed()
            .await
            .expect("re-seal pass"),
        0,
        "an entry already under the current root is not re-uploaded"
    );
    assert!(
        store.last_manifest_hash.lock().unwrap().is_none(),
        "nothing was POSTed"
    );
    assert!(
        engine
            .db()
            .get_entry(rel)
            .unwrap()
            .unwrap()
            .current_root_sealed,
        "…but it IS marked, so the check is paid once"
    );
}

/// An identity that never succeeded holds no predecessor keys, and the pass
/// returns before looking at anything. This is what lets the step sit
/// unconditionally in every drive shape's catch-up.
///
/// ⚠ The fixture is deliberately one the pass would ACT on if the short-circuit
/// were removed — materialized on disk, and at rest under a root this engine
/// does not hold — so "no manifest was POSTed" is a real observable rather than
/// a restatement of the arrangement. (A fixture that merely lacked local bytes
/// passed identically with and without the guard; that vacuity is the trap this
/// track has already been bitten by twice.)
#[tokio::test]
async fn post_succession_reseal_is_free_for_an_identity_that_never_succeeded() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x77u8; 32])),
    );
    // No `set_predecessor_backup_keys` — the ordinary fleet.

    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let rel = "docs/ordinary.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let manifest = store_sealed_fixture(&store, &original, &[0x99u8; 32]);
    seed_synced_entry(&engine, rel, &original, manifest);

    assert_eq!(
        engine
            .reseal_predecessor_sealed()
            .await
            .expect("re-seal pass"),
        0,
    );
    assert!(
        store.last_manifest_hash.lock().unwrap().is_none(),
        "the pass must not touch the network for an identity that never succeeded"
    );
    // Unmarked: it returned before looking, rather than concluding anything
    // about an entry it never examined.
    assert!(
        !engine
            .db()
            .get_entry(rel)
            .unwrap()
            .unwrap()
            .current_root_sealed,
    );
}

// ─────────────────────────────────────────────────────────────────────
// Sealed manifest hashes
// ─────────────────────────────────────────────────────────────────────

/// The last-uploaded manifest, asserted to be in the **sealed-only** wire
/// shape every writer emits (`mls-group-key-material.md` § M2 *Sealed
/// manifest hashes*: plaintext `file_hash`/`chunk_hashes` blanked,
/// `sealed_hashes` carrying them under `root`, `min_reader = 2`) — the
/// engine-tier pin that the upload path seals, read back through the walk
/// by the tests below. Returns its content hash.
fn uploaded_sealed_manifest(
    store: &crate::test_support::BlobStore,
    root: &[u8; 32],
) -> ContentHash {
    let hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("an upload must have POSTed a manifest");
    let bytes = store
        .manifests
        .lock()
        .unwrap()
        .get(&hex::encode(hash.digest()))
        .cloned()
        .expect("stored manifest bytes");
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&bytes).expect("decode stored manifest");
    assert!(
        manifest.chunk_hashes.is_empty(),
        "the upload left plaintext chunk hashes on the wire"
    );
    assert_eq!(manifest.file_hash, fauna_core::chunk::blank_file_hash());
    assert!(
        manifest.sealed_hashes.is_some(),
        "the upload sealed no hashes"
    );
    assert_eq!(manifest.min_reader, Some(2));
    manifest
        .unseal_hashes(root)
        .expect("the hashes open under the root that sealed the chunks");
    hash
}

#[tokio::test]
async fn sealed_manifest_round_trips_backup_root() {
    // A sealed-only manifest on the owner `BackupKey` path: the engine opens
    // `sealed_hashes` under `BackupKey::convergent_chunk_root()` (the same root
    // that seals the chunks), restores the plaintext hashes, decrypts the
    // chunks, and passes the whole-file verify.
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let backup_key = BackupKey::from_bytes([9u8; 32]);
    let root = backup_key.convergent_chunk_root();
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), Some(backup_key));

    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    let rel = "docs/sealed.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    engine.upload_file(rel).await.expect("upload_file");

    let sealed_hash = uploaded_sealed_manifest(&store, &root);
    let bytes = engine
        .download_file_bytes_by_manifest(sealed_hash, None, rel)
        .await
        .expect("a sealed manifest must open under the owner backup root");
    assert_eq!(bytes, original);
}

#[tokio::test]
async fn sealed_manifest_round_trips_content_key_root() {
    // The M2 twin: a bound set's sealed manifest opens under the generation
    // the change record stamps (`key_for(version)` — the chunk root).
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let content_keys = FolderContentKeys::genesis([7u8; 32], 1_000);
    let root = *content_keys.current_key();
    let watch = tempfile::tempdir().unwrap();
    let bound = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(content_keys),
    );

    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(13) % 251) as u8)
        .collect();
    let rel = "shared/sealed.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    bound.upload_file(rel).await.expect("upload_file (bound)");

    let sealed_hash = uploaded_sealed_manifest(&store, &root);
    let bytes = bound
        .download_file_bytes_by_manifest(sealed_hash, Some(1), rel)
        .await
        .expect("a bound holder of generation 1 must open the sealed manifest");
    assert_eq!(bytes, original);
}

#[tokio::test]
async fn sealed_manifest_fails_closed_without_root() {
    // Fail-closed posture: a holder without the root (a bound-but-keyless
    // engine — e.g. a removed member, or a reader missing the generation)
    // errors on the sealed manifest; it never falls through to the blanked
    // plaintext fields or a bare read.
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let content_keys = FolderContentKeys::genesis([7u8; 32], 1_000);
    let root = *content_keys.current_key();
    let watch = tempfile::tempdir().unwrap();
    let bound = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(content_keys),
    );
    let original = vec![42u8; 70_000];
    let rel = "shared/locked.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    bound.upload_file(rel).await.expect("upload_file (bound)");
    let sealed_hash = uploaded_sealed_manifest(&store, &root);

    // Same group binding, but NO content keys ⇒ no open root ⇒ fail closed.
    let watch2 = tempfile::tempdir().unwrap();
    let keyless = build_test_engine(
        &server.uri(),
        watch2.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        None,
    );
    let err = keyless
        .download_file_bytes_by_manifest(sealed_hash, Some(1), rel)
        .await
        .expect_err("a keyless holder must fail closed on a sealed manifest");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("fail closed")
            || msg.contains("refusing to read")
            || msg.contains("generation"),
        "unexpected error shape: {msg}"
    );
}

#[tokio::test]
async fn manifest_from_future_format_gives_honest_error() {
    // A manifest stamping `min_reader` above this binary's
    // MANIFEST_READER_VERSION surfaces the typed "update this client" error at
    // the decode boundary — not a cryptic hash-mismatch downstream.
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), None);

    let payload = b"future format".to_vec();
    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash: ContentHash::of_raw(&payload),
        total_size: payload.len() as u64,
        chunk_hashes: vec![ContentHash::of_raw(&payload)],
        chunk_sizes: vec![payload.len() as u64],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: Some(fauna_core::chunk::MANIFEST_READER_VERSION + 1),
    };
    let bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
    let hash = ContentHash::of_raw(&bytes);
    store
        .manifests
        .lock()
        .unwrap()
        .insert(hex::encode(hash.digest()), bytes);

    let err = engine
        .download_file_bytes_by_manifest(hash, None, "any/path.bin")
        .await
        .expect_err("a too-new manifest must error");
    assert!(
        format!("{err:#}").contains("update this client"),
        "unexpected error shape: {err:#}"
    );
}

#[tokio::test]
async fn same_version_merge_shadowed_key_still_opens() {
    // Two
    // owner devices rotate gen 1 → gen 2 concurrently with DIFFERENT keys;
    // the custody merge retains both, but a single version→key lookup returns
    // only the winner — so a file the LOSING device sealed (stamped version 2
    // under the loser key) AEAD-failed forever despite the key sitting in
    // `prior`. The open paths must try every same-version candidate.
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    // Device B's fork loses the deterministic merge tiebreak ([5;32] < [9;32]).
    let mut fork_a = FolderContentKeys::genesis([1u8; 32], 1_000);
    fork_a.rotate([9u8; 32], 2_000);
    let mut fork_b = FolderContentKeys::genesis([1u8; 32], 1_000);
    fork_b.rotate([5u8; 32], 2_000);
    let merged = fork_a.merge(&fork_b);
    assert_eq!(
        *merged.current_key(),
        [9u8; 32],
        "precondition: fork B is the losing key"
    );

    // The losing device uploads while still on its own fork: stamped version 2,
    // sealed under [5;32].
    let watch = tempfile::tempdir().unwrap();
    let loser = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(fork_b),
    );
    let original: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(23) % 251) as u8)
        .collect();
    let rel = "shared/collided.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    loser.upload_file(rel).await.expect("upload (losing fork)");
    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload must have POSTed a manifest");

    // A reader holding the MERGED custody must open it: version 2 has two
    // candidates ([9;32] current, [5;32] shadowed in prior) and the open path
    // tries both.
    let watch2 = tempfile::tempdir().unwrap();
    let reader = build_test_engine(
        &server.uri(),
        watch2.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(merged),
    );
    let bytes = reader
        .download_file_bytes_by_manifest(manifest_hash, Some(2), rel)
        .await
        .expect("merged-custody reader must reach the shadowed same-version key");
    assert_eq!(bytes, original);
}

// ─────────────────────────────────────────────────────────────────────
// Conflict auto-resolve — apply-path pins (file-sync.md § Conflicts,
// ratified 2026-07-10). The harness's WS-RPC client is unconnected, so the
// resolved REPORT cannot land — which is exactly the fail-closed leg: the
// local file must survive untouched, with the local version already durably
// uploaded. (The report-lands legs are covered nest-side by
// `conformance_folders::pre_resolved_report_retains_loser_and_propagates_winner`
// and end-to-end by the tier_3 e2e.) Paused tokio time makes the WS deadline
// elapse instantly instead of 30 s per attempt.
// ─────────────────────────────────────────────────────────────────────

/// Divergent local + incoming, and the resolved report cannot land ⇒
/// fail-closed: the local file is NOT overwritten, and the local version's
/// chunks+manifest were uploaded BEFORE anything else (retention-first).
#[tokio::test(start_paused = true)]
async fn divergent_download_fails_closed_when_report_cannot_land() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let incoming_content = b"alpha\nbravo\ncharlie\ndelta\n";
    let local_content = b"zero\nalpha\nbravo\ncharlie\n";
    let base_content = b"alpha\nbravo\ncharlie\n";

    // Seed the incoming version from an uploader engine in its own dir. Both
    // devices hold the same owner BackupKey (same-user devices; a keyless
    // folder engine no longer uploads at all).
    let owner_key = BackupKey::from_bytes([0x55u8; 32]);
    let up_watch = tempfile::tempdir().unwrap();
    let uploader = test_sync_engine(
        &server.uri(),
        up_watch.path().to_path_buf(),
        Some(owner_key.clone()),
    );
    std::fs::write(up_watch.path().join("notes.txt"), incoming_content).unwrap();
    uploader
        .upload_file("notes.txt")
        .await
        .expect("seed upload");
    let manifest_a = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("manifest posted");

    // Victim: divergent local content + a stale merge base (≠ local, so this
    // is a genuine divergence, not a fast-forward).
    let watch = tempfile::tempdir().unwrap();
    let victim = test_sync_engine(&server.uri(), watch.path().to_path_buf(), Some(owner_key));
    std::fs::write(watch.path().join("notes.txt"), local_content).unwrap();
    // The engine's OWN merge-base root: it is scoped per `(device, set)`
    // (`causal.rs` § scoped_store_dir), so a flat one written here would be a
    // base the engine never reads.
    let base_dir = victim.base_dir_for_test();
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::write(base_dir.join("notes.txt"), base_content).unwrap();

    victim
        .download_and_write_file(
            "notes.txt",
            manifest_a,
            None,
            Some("22".repeat(32)),
            1_700_000_000_000,
            None,
            None,
            None,
            None,
            None,
            // leg-4 ruling: no listing context in this fixture — conservative claim
            false,
        )
        .await
        .expect("fail-closed apply reports Ok (divergence handed to the chooser flow)");

    // The local file survives byte-identical.
    assert_eq!(
        std::fs::read(watch.path().join("notes.txt")).unwrap(),
        local_content,
        "fail-closed: local bytes untouched when the resolved report cannot land"
    );
    // The base was NOT advanced (a later retry must still see the divergence).
    assert_eq!(
        std::fs::read(base_dir.join("notes.txt")).unwrap(),
        base_content,
        "merge base unchanged on the unresolved path"
    );
    // Retention-first: the local version's manifest was uploaded before the
    // report was attempted (the store's last POSTed manifest is the LOCAL
    // version's, not the seeded incoming one).
    let last = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("local version uploaded");
    assert_ne!(
        last, manifest_a,
        "the divergent local version was uploaded as a retention candidate"
    );
}

/// Local == cached base ⇒ a plain fast-forward: the incoming version applies
/// with no conflict machinery (no divergence, nothing uploaded by the victim).
#[tokio::test(start_paused = true)]
async fn fast_forward_applies_incoming_without_conflict() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let incoming_content = b"alpha\nbravo\ncharlie\ndelta\n";
    let base_content = b"alpha\nbravo\ncharlie\n";

    let owner_key = BackupKey::from_bytes([0x56u8; 32]);
    let up_watch = tempfile::tempdir().unwrap();
    let uploader = test_sync_engine(
        &server.uri(),
        up_watch.path().to_path_buf(),
        Some(owner_key.clone()),
    );
    std::fs::write(up_watch.path().join("notes.txt"), incoming_content).unwrap();
    uploader
        .upload_file("notes.txt")
        .await
        .expect("seed upload");
    let manifest_a = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("manifest posted");

    // Victim's local file matches its base — no local edit since last sync.
    let watch = tempfile::tempdir().unwrap();
    let victim = test_sync_engine(&server.uri(), watch.path().to_path_buf(), Some(owner_key));
    std::fs::write(watch.path().join("notes.txt"), base_content).unwrap();
    // The engine's OWN merge-base root: it is scoped per `(device, set)`
    // (`causal.rs` § scoped_store_dir), so a flat one written here would be a
    // base the engine never reads.
    let base_dir = victim.base_dir_for_test();
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::write(base_dir.join("notes.txt"), base_content).unwrap();

    victim
        .download_and_write_file(
            "notes.txt",
            manifest_a,
            None,
            Some("22".repeat(32)),
            1_700_000_000_000,
            None,
            None,
            None,
            None,
            None,
            // leg-4 ruling: no listing context in this fixture — conservative claim
            false,
        )
        .await
        .expect("fast-forward apply");

    assert_eq!(
        std::fs::read(watch.path().join("notes.txt")).unwrap(),
        incoming_content,
        "fast-forward: incoming version applied"
    );
    assert_eq!(
        std::fs::read(base_dir.join("notes.txt")).unwrap(),
        incoming_content,
        "base advances to the applied version"
    );
    assert_eq!(
        store.last_manifest_hash.lock().unwrap().unwrap(),
        manifest_a,
        "no conflict upload happened — the last POSTed manifest is still the seed"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Full restore: the client-side walk into a directory
// ([`SyncEngine::restore_snapshot_files_to_dir`] + [`build_restore_engine`]).
//
// The macOS `MacRestoreView` leg of the client-side full restore
// (`docs/goal/behavior/backup-restore.md` § 4): a *sealed* snapshot — which the
// server-side ZIP route now refuses loudly — restores correctly here, because the
// plaintext only materializes where the owner `BackupKey` lives.
// ─────────────────────────────────────────────────────────────────────

use crate::engine::{RestoreSummary, SnapshotFileToRestore};

/// A fresh engine holding only the owner `BackupKey` derived from `seed` — the
/// same shape `engine_lifecycle::build_restore_engine` produces in production
/// (in-memory throwaway DB, no watch dir / state used by the download walk). Built
/// via `test_sync_engine` so the restore-method tests run under the crate's
/// default feature set (the `build_restore_engine` builder itself lives behind the
/// `engine-lifecycle` feature and is covered by its own gated test below).
fn restore_engine(server_uri: &str, seed: &[u8; 32]) -> SyncEngine {
    // The walk never reads `watch_dir`; a stable dummy avoids a tempdir lifetime.
    test_sync_engine(
        server_uri,
        std::env::temp_dir(),
        Some(BackupKey::derive(seed)),
    )
}

/// Seal `bytes` at `rel` under `engine`'s BackupKey into the mock nest and return
/// the manifest hash — the pointer a snapshot listing carries per file.
async fn seal_and_capture(
    engine: &SyncEngine,
    watch: &std::path::Path,
    store: &crate::test_support::BlobStore,
    rel: &str,
    bytes: &[u8],
) -> ContentHash {
    let full = watch.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, bytes).unwrap();
    engine.upload_file(rel).await.expect("upload_file");
    store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("upload_file must have POSTed a manifest")
}

#[tokio::test]
async fn restore_snapshot_files_to_dir_restores_a_sealed_snapshot() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    // Seal under the owner BackupKey *derived from a seed* — the exact key path a
    // production restore rebuilds from the identity seed, not a raw `from_bytes` key.
    let seed = [0x42u8; 32];
    let watch = tempfile::tempdir().unwrap();
    let uploader = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::derive(&seed)),
    );

    // Two sealed files: one nested, one multi-chunk (>64 KiB forces FastCDC
    // boundaries), so the restore covers the subdir-create + reassembly paths.
    let small = b"hello restore".to_vec();
    let large: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(7) % 251) as u8)
        .collect();
    let m_small =
        seal_and_capture(&uploader, watch.path(), &store, "notes/hello.txt", &small).await;
    let m_large = seal_and_capture(&uploader, watch.path(), &store, "data/large.bin", &large).await;

    let files = vec![
        SnapshotFileToRestore {
            relative_path: "notes/hello.txt".into(),
            manifest_hash: m_small,
            content_key_version: None,
        },
        SnapshotFileToRestore {
            relative_path: "data/large.bin".into(),
            manifest_hash: m_large,
            content_key_version: None,
        },
    ];

    let out = tempfile::tempdir().unwrap();
    let summary = restore_engine(&server.uri(), &seed)
        .restore_snapshot_files_to_dir(&files, out.path())
        .await
        .expect("restore_snapshot_files_to_dir");

    assert_eq!(
        summary,
        RestoreSummary {
            files_restored: 2,
            bytes_written: (small.len() + large.len()) as u64,
            skipped: 0,
            ..Default::default()
        }
    );
    assert_eq!(
        std::fs::read(out.path().join("notes/hello.txt")).unwrap(),
        small,
        "nested sealed file restored byte-identical"
    );
    assert_eq!(
        std::fs::read(out.path().join("data/large.bin")).unwrap(),
        large,
        "multi-chunk sealed file restored byte-identical"
    );
}

#[tokio::test]
async fn restore_snapshot_files_to_dir_skips_unsafe_paths() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let seed = [0x43u8; 32];
    let watch = tempfile::tempdir().unwrap();
    let uploader = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::derive(&seed)),
    );

    let good = b"safe content".to_vec();
    let m_good = seal_and_capture(&uploader, watch.path(), &store, "safe.txt", &good).await;

    // A `..`-escaping entry (corrupt/malicious snapshot) is skipped + counted,
    // never written, and does NOT abort the good file's restore.
    let files = vec![
        SnapshotFileToRestore {
            relative_path: "../escape.txt".into(),
            manifest_hash: m_good,
            content_key_version: None,
        },
        SnapshotFileToRestore {
            relative_path: "safe.txt".into(),
            manifest_hash: m_good,
            content_key_version: None,
        },
    ];

    let out = tempfile::tempdir().unwrap();
    let summary = restore_engine(&server.uri(), &seed)
        .restore_snapshot_files_to_dir(&files, out.path())
        .await
        .expect("restore succeeds, skipping the unsafe entry");

    assert_eq!(
        summary,
        RestoreSummary {
            files_restored: 1,
            bytes_written: good.len() as u64,
            skipped: 1,
            ..Default::default()
        }
    );
    assert_eq!(
        std::fs::read(out.path().join("safe.txt")).unwrap(),
        good,
        "the safe file restored despite the sibling unsafe entry"
    );
    let escaped = out.path().parent().unwrap().join("escape.txt");
    assert!(
        !escaped.exists(),
        "the `..`-escaping entry must never be written outside the output dir"
    );
}

/// The restore walk must not follow a **dangling
/// final-component symlink** planted in the user-chosen output directory.
///
/// `resolved_target_within_root` deliberately does not canonicalize the target's
/// final component, and it is right not to: the restored path legitimately does
/// not exist yet. A dangling link is the one shape that survives that gap —
/// `canonicalize` fails on it, so the deepest-existing-ancestor walk falls back
/// to `output_dir` and permits the write. (A *resolvable* final symlink is
/// caught: canonicalize follows it out and the containment test fails.)
///
/// Containment therefore rests entirely on the terminal write **replacing** the
/// link rather than following it — what `atomic_write_file`'s rename does and a
/// direct `std::fs::write` does not. Before this door moved onto
/// `atomic_write_file` the assert below measured the escape: the snapshot bytes
/// landed at the link's outside target, counted as `files_restored`, silently.
///
/// The path is nest-supplied (`fauna-ffi/src/sync_engine_host.rs` ←
/// `get_for_restore`), so a hostile nest picks which planted name to aim at.
/// Unix-only: creating the symlink needs `std::os::unix`, and on Windows an
/// unprivileged symlink isn't a given.
#[cfg(unix)]
#[tokio::test]
async fn restore_snapshot_files_to_dir_does_not_follow_a_dangling_final_symlink() {
    use std::os::unix::fs::symlink;

    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let seed = [0x45u8; 32];
    let watch = tempfile::tempdir().unwrap();
    let uploader = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::derive(&seed)),
    );
    let content = b"snapshot bytes that must stay inside the output dir".to_vec();
    let m = seal_and_capture(&uploader, watch.path(), &store, "note.txt", &content).await;

    // The victim's restore destination, holding a pre-existing dangling symlink
    // at the very name the snapshot listing carries.
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let planted = outside.join("planted.txt");
    symlink(&planted, out.join("note.txt")).expect("plant the dangling symlink");

    let files = vec![SnapshotFileToRestore {
        relative_path: "note.txt".into(),
        manifest_hash: m,
        content_key_version: None,
    }];

    let summary = restore_engine(&server.uri(), &seed)
        .restore_snapshot_files_to_dir(&files, &out)
        .await
        .expect("restore_snapshot_files_to_dir");

    assert!(
        !planted.exists(),
        "the restore walk must never create a file outside the output dir by \
         following a planted dangling symlink"
    );
    assert_eq!(summary.files_restored, 1);
    assert!(
        !std::fs::symlink_metadata(out.join("note.txt"))
            .expect("the restored path exists")
            .file_type()
            .is_symlink(),
        "the terminal write must REPLACE the planted link, not follow it"
    );
    assert_eq!(
        std::fs::read(out.join("note.txt")).unwrap(),
        content,
        "the bytes land at the in-root path the listing named"
    );
}

/// The **production** restore builder end-to-end: `build_restore_engine` re-derives
/// the owner `BackupKey` from the identity-seed hex and decrypts a sealed snapshot
/// — the exact path the FFI `restore_snapshot_to_dir` drives for `MacRestoreView`.
/// Gated on `engine-lifecycle` (the FFI enables it); the non-gated tests above
/// cover the walk itself.
#[cfg(feature = "engine-lifecycle")]
#[tokio::test]
async fn build_restore_engine_restores_a_sealed_snapshot() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let seed = [0x44u8; 32];
    let secret_hex = hex::encode(seed);
    let watch = tempfile::tempdir().unwrap();
    let uploader = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::derive(&seed)),
    );
    let content = b"restored by the production builder".to_vec();
    let m = seal_and_capture(&uploader, watch.path(), &store, "doc.txt", &content).await;

    let auth = {
        let kp = ActorKeypair::generate();
        let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
        Arc::new(fauna_client::AuthClient::with_bearer_source(
            server.uri(),
            kp,
            bearer,
            reqwest::Client::new(),
        ))
    };
    let engine = crate::engine_lifecycle::build_restore_engine(
        auth,
        fauna_client::NestClient::new(server.uri(), ActorKeypair::generate()),
        [0u8; 32],
        &secret_hex,
    );

    let files = vec![SnapshotFileToRestore {
        relative_path: "doc.txt".into(),
        manifest_hash: m,
        content_key_version: None,
    }];
    let out = tempfile::tempdir().unwrap();
    let summary = engine
        .restore_snapshot_files_to_dir(&files, out.path())
        .await
        .expect("build_restore_engine restore");

    assert_eq!(summary.files_restored, 1);
    assert_eq!(std::fs::read(out.path().join("doc.txt")).unwrap(), content);
}

// ─────────────────────────────────────────────────────────────────────
// The raw-AEAD Library plane — the post-succession re-seal's SECOND arm
//
// A Media-page upload's blob-store primary is a single framed
// `decrypt_backup_chunk` blob under the BARE `BackupKey` (`Audience::Library`),
// never the convergent chunk root — so `FileDownloadKeys` does not reach it and
// it did NOT move with the chunk plane. Its hash nonetheless lands in the same
// `manifest_hash` column, and resolving it through the chunk-manifest route
// (a different store) is what used to leave such an entry owed forever, which
// is why `list_pending_current_root_reseal` could not drain at all on any
// account holding one (`sync-agent.md` § Implementation status → A8).
//
// ⚠ These assert the BYTE plane. Every engine test in this module runs an
// unconnected `NestClient`, so `changes.record` never lands and the sentinel —
// deliberately gated on the RECORD — stays unmarked. The marking half belongs
// to the tier_3 journey, exactly as the chunk plane's split already is.
// ─────────────────────────────────────────────────────────────────────

/// Seal `plaintext` as a Media-page upload's blob-store primary would be —
/// framed `encrypt_backup_chunk` under the bare key — store it in the mock's
/// BLOB store, and return the hash the change row would carry.
fn store_library_blob(
    store: &crate::test_support::BlobStore,
    plaintext: &[u8],
    key: &BackupKey,
) -> ContentHash {
    let sealed = fauna_core::crypto::encrypt_backup_chunk(key, plaintext).expect("seal");
    let hex = blake3::hash(&sealed).to_hex().to_string();
    store.blobs.lock().unwrap().insert(hex.clone(), sealed);
    ContentHash::from_digest_raw(fauna_core::hex32::decode(&hex).expect("hex32"))
}

/// A successor's inherited Library blob is re-sealed under the successor's own
/// bare key — the piece that actually unblocks `sync-agent.md` bound (3).
///
/// ⚠ Non-vacuity: the assertion is not "a blob was POSTed" but "the POSTed blob
/// opens under the SUCCESSOR key and its plaintext is the original", which a
/// pass that merely re-uploaded the inherited ciphertext would fail. Deleting
/// the blob arm from `reseal_one_entry` turns this red at the first assert
/// (nothing is POSTed at all — the manifest fetch just errors).
#[tokio::test]
async fn post_succession_reseal_moves_a_library_blob_off_the_retired_root() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x7au8; 32]);
    let successor = BackupKey::from_bytes([0x8bu8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    // A Media-page upload the predecessor made: the blob is sealed under the
    // retired bare key, and nothing is materialized on this device.
    let original: Vec<u8> = (0..30_000u32).map(|i| (i % 241) as u8).collect();
    let rel = "media/holiday.jpg";
    let blob_hash = store_library_blob(&store, &original, &predecessor);
    seed_synced_entry(&engine, rel, &original, blob_hash);

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    let posted = store
        .last_blob_hash
        .lock()
        .unwrap()
        .clone()
        .expect("the Library-blob arm must have POSTed a re-sealed primary");
    assert_ne!(
        posted,
        hex::encode(blob_hash.digest()),
        "a re-seal under a different root changes the ciphertext, so it changes the hash"
    );
    let stored = store.blobs.lock().unwrap().get(&posted).cloned().unwrap();
    assert_eq!(
        fauna_core::crypto::decrypt_backup_chunk(&successor, &stored).expect(
            "the re-sealed primary must open under the SUCCESSOR's bare key — the whole point"
        ),
        original,
        "…and carry the original plaintext, byte for byte"
    );
    assert!(
        fauna_core::crypto::decrypt_backup_chunk(&predecessor, &stored).is_err(),
        "the retired root must no longer open it, or the seed stays load-bearing"
    );
}

/// A blob failing its content address is never re-sealed, re-recorded or marked.
///
/// The backup-chunk frame carries no AAD, so a tag proves only
/// *sealed-under-some-root-we-hold*, never *these bytes* — the address is the
/// only anchor this plane has. Without it a malicious nest serves any blob it
/// holds under a retired root and this pass re-seals it under the CURRENT key
/// and re-records it at the victim's path, making the substitution the
/// account's own authenticated content.
///
/// ⚠ Non-vacuity, verified by mutation: the substituted blob IS validly sealed
/// under a root this device holds, so deleting the address check turns this red
/// — the pass adopts the attacker's bytes.
///
/// ⚠ **What this test does NOT pin, deliberately: the check's ORDERING relative
/// to the candidate opens.** Moving the check after the candidate loop keeps
/// every assertion here green, because the check still bails before anything is
/// uploaded or recorded — at this call site the ordering has no observable. It
/// is load-bearing on the *read* plane (`MediaMachine::open_library_blob`),
/// where the plaintext reaches the user; here it is defense-in-depth. Recorded
/// so a later session does not read this test as the ordering's guard and
/// "confirm" a property nothing checks.
#[tokio::test]
async fn post_succession_reseal_rejects_a_substituted_library_blob() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x11u8; 32]);
    let successor = BackupKey::from_bytes([0x22u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    let wanted: Vec<u8> = (0..20_000u32).map(|i| (i % 233) as u8).collect();
    let other: Vec<u8> = (0..20_000u32).map(|i| (i % 199) as u8).collect();
    let rel = "media/receipt.jpg";

    // The row names `wanted`'s hash, but the nest serves a DIFFERENT blob —
    // validly sealed under the predecessor root, so the candidate loop alone
    // would accept it.
    let wanted_hash = store_library_blob(&store, &wanted, &predecessor);
    let substituted =
        fauna_core::crypto::encrypt_backup_chunk(&predecessor, &other).expect("seal other");
    store
        .blobs
        .lock()
        .unwrap()
        .insert(hex::encode(wanted_hash.digest()), substituted);
    seed_synced_entry(&engine, rel, &wanted, wanted_hash);

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("the pass reports per-entry failures without aborting");

    assert!(
        store.last_blob_hash.lock().unwrap().is_none(),
        "nothing may be re-sealed or re-recorded from a blob that failed its content address"
    );
    assert!(
        !engine
            .db()
            .get_entry(rel)
            .unwrap()
            .unwrap()
            .current_root_sealed,
        "and the entry stays owed — a rejected substitution is not progress"
    );
}

/// Ruling (10)(c), the stamp binds the root, on the engine's Library-blob arm:
/// an entry whose record carries a `content_key_version` is never opened under
/// an owner key — current or retired — so a stamped row naming a blob primary
/// (which an honest writer records unstamped) is neither re-sealed nor
/// re-recorded, and stays owed rather than being adopted. The same blob named
/// unstamped moves ([`post_succession_reseal_moves_a_library_blob_off_the_retired_root`]).
#[tokio::test]
async fn post_succession_reseal_never_opens_a_stamped_entrys_library_blob() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x7au8; 32]);
    let successor = BackupKey::from_bytes([0x8bu8; 32]);
    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    let original: Vec<u8> = (0..30_000u32).map(|i| (i % 241) as u8).collect();
    let rel = "media/stamped.jpg";
    let blob_hash = store_library_blob(&store, &original, &predecessor);
    engine
        .db()
        .upsert_entry(
            rel,
            Some(ContentHash::of_raw(&original)),
            Some(ContentHash::of_raw(&original)),
            Some(blob_hash),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            Some(3),
        )
        .expect("seed a stamped entry");
    engine.stamp_own_head_for_test(rel);

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("the pass reports per-entry failures without aborting");

    assert!(
        store.last_blob_hash.lock().unwrap().is_none(),
        "a stamped entry's blob is never opened under an owner key, so nothing is re-sealed"
    );
    assert!(
        !engine
            .db()
            .get_entry(rel)
            .unwrap()
            .unwrap()
            .current_root_sealed,
        "and the entry stays owed"
    );
}

/// A Library blob already under the successor's own key is marked without being
/// re-uploaded.
///
/// The filter's real job on this plane: a successor's media corpus is a mixture,
/// and everything uploaded after the ceremony is already current. Re-uploading
/// those would turn a one-time repair into a full re-upload of the library on
/// every catch-up.
///
/// ⚠ This is also the one arm whose SENTINEL is assertable at tier_1: the
/// already-current path marks directly, with no change record to gate it.
#[tokio::test]
async fn post_succession_reseal_marks_an_already_current_library_blob_without_reuploading() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let successor = BackupKey::from_bytes([0x33u8; 32]);
    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![BackupKey::from_bytes([0x44u8; 32]).into()]);

    let original: Vec<u8> = (0..15_000u32).map(|i| (i % 251) as u8).collect();
    let rel = "media/after-the-ceremony.jpg";
    let blob_hash = store_library_blob(&store, &original, &successor);
    seed_synced_entry(&engine, rel, &original, blob_hash);

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    assert!(
        store.last_blob_hash.lock().unwrap().is_none(),
        "nothing was re-uploaded"
    );
    assert!(
        engine
            .db()
            .get_entry(rel)
            .unwrap()
            .unwrap()
            .current_root_sealed,
        "…but it IS marked, so the check is paid once and the observable can drain"
    );
}

/// A hash in NEITHER store surfaces the manifest failure, not a blob-shaped one,
/// and leaves the entry owed.
///
/// The blob arm is reached by a manifest MISS, so it must not swallow a real
/// manifest error: an entry whose chunk manifest is genuinely missing (or whose
/// fetch failed) has to keep reporting that, or the diagnosis for a broken
/// chunk-plane entry silently becomes "not a blob either".
#[tokio::test]
async fn post_succession_reseal_reports_the_manifest_failure_when_neither_store_has_it() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let successor = BackupKey::from_bytes([0x55u8; 32]);
    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![BackupKey::from_bytes([0x66u8; 32]).into()]);

    let original: Vec<u8> = (0..9_000u32).map(|i| (i % 211) as u8).collect();
    let rel = "docs/vanished.bin";
    // A hash neither store holds.
    let orphan = ContentHash::of_raw(b"neither-store-holds-this");
    seed_synced_entry(&engine, rel, &original, orphan);

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("the pass survives a per-entry failure");

    assert!(
        store.last_blob_hash.lock().unwrap().is_none(),
        "nothing was POSTed for an entry no store can serve"
    );
    assert_eq!(
        engine
            .db()
            .list_pending_current_root_reseal()
            .unwrap()
            .len(),
        1,
        "and it stays owed rather than being marked done — the observable must drain on merit"
    );
}

// ── The recorded thumbnail MOVES with its file ────────────────────────
//
// The last piece of the raw-AEAD Library plane's write half
// (`identity-succession.md` § Implementation status today). A re-seal that
// regenerates the thumbnail from plaintext it holds moves that seal for free —
// but `maybe_upload_thumbnail` / `process_and_seal` yield **nothing** when this
// build cannot render one, and recording `None` over a head that names a
// thumbnail DROPS the pointer: the blob stays at rest under the root the
// aftermath is about to retire, referenced by nothing. It does not block the
// drain (the entry still records and still marks) — it silently costs the user
// a tile. So: open the existing thumbnail and re-seal those same pixels, which
// needs no thumbnailer at all.
//
// ⚠ **How these tests induce "cannot regenerate", and why it is the same
// branch.** The test build has `process_media` ON (default features), so a real
// image WOULD regenerate. The payloads here are therefore non-image bytes, for
// which `process_media` yields `thumbnail_bytes: None` — the identical
// `regenerated.is_none()` branch a thumbnailer-less build takes for an image.
// The distinction the code makes is never "is this build a producer" but "did
// the producer yield something, and does the recorded head name a thumbnail
// anyway", and both causes reach it the same way.
//
// ⚠ These assert the BYTE plane, like every sibling above: the record cannot
// land on an unconnected `NestClient`.
// ─────────────────────────────────────────────────────────────────────

/// A recorded thumbnail this build cannot regenerate is MOVED onto the current
/// root, not dropped.
///
/// ⚠ Non-vacuity: the assertion is not "a blob was POSTed" but "a blob was
/// POSTed that opens under the SUCCESSOR key and carries the ORIGINAL thumbnail
/// pixels" — which a pass that re-uploaded the inherited ciphertext, or that
/// invented a thumbnail, would both fail. Deleting the move (returning
/// `regenerated` unchanged from `thumbnail_hash_for_reseal`) turns it red at the
/// first assert: nothing is POSTed to the blob store at all, because the chunk
/// plane's own bytes go to `/chunks`.
#[tokio::test]
async fn post_succession_reseal_moves_a_recorded_thumbnail_it_cannot_regenerate() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x31u8; 32]);
    let successor = BackupKey::from_bytes([0x42u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    // A file the predecessor recorded WITH a thumbnail, never materialized here
    // (the fresh-device successor shape), so the re-seal sources it from the
    // nest and re-thumbnails from the plaintext it fetches.
    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 241) as u8).collect();
    let thumb_pixels: Vec<u8> = (0..1_200u32).map(|i| (i % 97) as u8).collect();
    let rel = "media/inherited-with-thumb.bin";

    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());
    let thumb_hash = store_library_blob(&store, &thumb_pixels, &predecessor);
    let thumb_hex = hex::encode(thumb_hash.digest());
    seed_synced_entry(&engine, rel, &original, pred_manifest);
    engine
        .db()
        .set_thumbnail_hash(rel, Some(&thumb_hex))
        .expect("seed the recorded head's thumbnail pointer");

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    let moved = store
        .last_blob_hash
        .lock()
        .unwrap()
        .clone()
        .expect("the thumbnail move must have POSTed a re-sealed thumbnail blob");
    assert_ne!(
        moved, thumb_hex,
        "a re-seal under a different root changes the ciphertext, so it changes the hash"
    );
    let stored = store.blobs.lock().unwrap().get(&moved).cloned().unwrap();
    assert_eq!(
        fauna_core::crypto::decrypt_backup_chunk(&successor, &stored)
            .expect("the moved thumbnail must open under the SUCCESSOR's bare key"),
        thumb_pixels,
        "…and carry the ORIGINAL thumbnail pixels — a move, not a re-render"
    );
    assert!(
        fauna_core::crypto::decrypt_backup_chunk(&predecessor, &stored).is_err(),
        "the retired root must no longer open it, or the seed stays load-bearing"
    );
    // ⚠ The row's cached pointer following the move is NOT assertable here, and
    // deliberately so: that stamp sits behind the change record's success arm —
    // the same gate the `current_root_sealed` sentinel sits behind, for the same
    // reason (an unrecorded move must not update the row's view of a head the
    // nest never accepted). Every test in this module runs an unconnected
    // `NestClient`, so the record cannot land and the stamp cannot run. Its
    // coverage is the tier_3 journey
    // (`bins/fauna-nest/tests/conformance_post_succession_reseal.rs`), against a
    // real nest. Asserting it here would mean moving the stamp out from behind
    // the record — buying a green line at the cost of the property.
    let _ = moved;
}

/// A substituted thumbnail blob is never re-sealed or re-recorded.
///
/// The same anchor argument as the primary plane: the backup-chunk frame carries
/// no AAD, so a tag proves only *sealed-under-some-root-we-hold*,
/// never *these bytes*. Without the address check a malicious nest serves any
/// blob it holds under a retired root and this pass re-seals it under the
/// CURRENT key and re-records it as the user's own thumbnail — the picture they
/// actually look at.
///
/// ⚠ Non-vacuity, verified by mutation: the substituted blob IS validly sealed
/// under a root this device holds, so deleting the address check in
/// `move_recorded_thumbnail` turns this red — the pass adopts the attacker's
/// pixels.
#[tokio::test]
async fn post_succession_reseal_rejects_a_substituted_thumbnail() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x51u8; 32]);
    let successor = BackupKey::from_bytes([0x62u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let wanted_thumb: Vec<u8> = (0..1_000u32).map(|i| (i % 89) as u8).collect();
    let other_thumb: Vec<u8> = (0..1_000u32).map(|i| (i % 71) as u8).collect();
    let rel = "media/substituted-thumb.bin";

    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());
    // The row names `wanted_thumb`'s hash, but the nest serves a DIFFERENT
    // thumbnail — validly sealed under the retired root, so the candidate loop
    // alone would accept it.
    let thumb_hash = store_library_blob(&store, &wanted_thumb, &predecessor);
    let thumb_hex = hex::encode(thumb_hash.digest());
    let substituted =
        fauna_core::crypto::encrypt_backup_chunk(&predecessor, &other_thumb).expect("seal other");
    store
        .blobs
        .lock()
        .unwrap()
        .insert(thumb_hex.clone(), substituted);
    seed_synced_entry(&engine, rel, &original, pred_manifest);
    engine
        .db()
        .set_thumbnail_hash(rel, Some(&thumb_hex))
        .expect("seed the recorded head's thumbnail pointer");

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("a rejected thumbnail is contained — the file's own re-seal still runs");

    assert!(
        store.last_blob_hash.lock().unwrap().is_none(),
        "nothing may be re-sealed from a thumbnail that failed its content address"
    );
    // ⚠ The file's own bytes MUST still have moved: a bad thumbnail is not a
    // reason to strand the corpus this pass exists to free.
    assert!(
        store.last_manifest_hash.lock().unwrap().is_some(),
        "the file's own re-seal proceeds — the thumbnail is best-effort, the corpus is not"
    );
}

/// A thumbnail already under the successor's own key keeps its pointer and is
/// NOT re-uploaded.
///
/// The steady state after one pass. Re-sealing an already-current thumbnail
/// would burn a blob per file to produce equivalent ciphertext, turning a
/// one-time repair into a permanent re-upload of every thumbnail in the library.
#[tokio::test]
async fn post_succession_reseal_keeps_an_already_current_thumbnail_unmoved() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x71u8; 32]);
    let successor = BackupKey::from_bytes([0x82u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 239) as u8).collect();
    let thumb_pixels: Vec<u8> = (0..800u32).map(|i| (i % 83) as u8).collect();
    let rel = "media/current-thumb.bin";

    // Chunks under the retired root (so the file is owed), thumbnail already
    // under the successor's own key (so only it is current).
    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());
    let thumb_hash = store_library_blob(&store, &thumb_pixels, &successor);
    let thumb_hex = hex::encode(thumb_hash.digest());
    seed_synced_entry(&engine, rel, &original, pred_manifest);
    engine
        .db()
        .set_thumbnail_hash(rel, Some(&thumb_hex))
        .expect("seed the recorded head's thumbnail pointer");

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    assert!(
        store.last_blob_hash.lock().unwrap().is_none(),
        "an already-current thumbnail must not be re-uploaded"
    );
    assert_eq!(
        engine.db().get_entry(rel).unwrap().unwrap().thumbnail_hash,
        Some(thumb_hex),
        "and its pointer is kept exactly as recorded"
    );
}

/// A head that never named a thumbnail is never even LOOKED UP — the
/// discriminator this leg turns on.
///
/// `regenerated == None` has two causes that are indistinguishable at the call
/// site — "this file has no thumbnail" and "this build cannot render one" — and
/// only the recorded head tells them apart.
///
/// ⚠ **Assert the FETCH, not the outcome — the outcome is vacuous here, and was
/// caught being so.** The first version of this test asserted "no blob was
/// POSTed", which stays green with the discriminator deleted: a move driven off
/// a hash the head never named simply 404s and returns `None`, POSTing nothing.
/// The mechanism's real content is that a thumbnail-less entry costs **no
/// round-trip at all** — otherwise every text file in a corpus buys a `GET
/// /blob` per pass — so that is what this counts. Same family as the
/// findings: an assertion that would have held with the mechanism removed is an
/// assertion about the fixture.
#[tokio::test]
async fn post_succession_reseal_never_looks_up_a_thumbnail_the_head_never_had() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x91u8; 32]);
    let successor = BackupKey::from_bytes([0xa2u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 227) as u8).collect();
    let rel = "docs/plain.txt";
    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());
    seed_synced_entry(&engine, rel, &original, pred_manifest);
    // No `set_thumbnail_hash` — the head never named one.

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    let blob_gets = server
        .received_requests()
        .await
        .expect("the mock records its requests")
        .iter()
        .filter(|r| {
            r.method == wiremock::http::Method::GET && r.url.path().starts_with("/api/v1/blob/")
        })
        .count();
    assert_eq!(
        blob_gets, 0,
        "a head naming no thumbnail must cost NO blob round-trip — the recorded \
         pointer is the discriminator, not a 404"
    );
    assert!(
        store.last_blob_hash.lock().unwrap().is_none(),
        "and nothing is invented and POSTed"
    );
    assert!(
        store.last_manifest_hash.lock().unwrap().is_some(),
        "while the file's own re-seal is unaffected"
    );
}

/// **The pass reports itself** — `succession-aftermath.md` § Re-key scope also
/// requires the re-seal be *"surfaced with progress, resumed until complete"*,
/// and on desktop the surface is in another process, so the only way the app can
/// learn anything is a record the pass leaves in its own `SyncDb`.
///
/// ⚠ This is the pin the *shape* of this leg most needs. The projection that
/// renders the line and the fold that aggregates it are both unit-tested against
/// hand-built rows — which is exactly the vacuity that let
/// `FileDownloadKeys::predecessor_backup_keys` ship with **zero production
/// writers**: a funnel nothing feeds passes every test
/// that starts from the funnel. So this starts from the production pass and
/// asserts the record exists and carries the pass's real counts.
///
/// The counts are asserted as a *pair* rather than "some record is present":
/// a `Settled { resealed: 0, owed: 0 }` is the projection's **silent** arm, so a
/// pass that recorded zeros for work it actually did would render nothing at all
/// and look identical to an account with no aftermath.
#[tokio::test]
async fn post_succession_reseal_records_what_the_pass_did() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x41u8; 32]);
    let successor = BackupKey::from_bytes([0x42u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    assert_eq!(
        engine.db().corpus_reseal_pass().unwrap(),
        None,
        "no pass has run yet, so there is nothing to report"
    );

    // One entry this device holds, sealed under the retired identity.
    let original: Vec<u8> = (0..90_000u32).map(|i| (i % 253) as u8).collect();
    let rel = "docs/reported.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());
    seed_synced_entry(&engine, rel, &original, pred_manifest);

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    // ⚠ `owed: 1`, not 0, and that is the honest reading rather than a fixture
    // artefact: every engine test in this crate runs an UNCONNECTED
    // `NestClient` (see this module's doc), so the change record cannot land,
    // and an unrecorded re-seal deliberately leaves the entry owed. The record
    // therefore reports exactly what happened — which is the property under
    // test, since a pass that claimed success here would license
    // `sync-agent.md` bound (3) to drop the only keys that open those bytes.
    assert_eq!(
        engine.db().corpus_reseal_pass().unwrap(),
        Some(crate::succession_progress::CorpusResealPass::Settled {
            resealed: 0,
            owed: 1,
        }),
        "the pass must record its own counts where a cross-process reader can see them"
    );
}

/// The other half of the same contract: an identity that **never succeeded**
/// must record nothing at all.
///
/// Not a micro-optimization — it is what keeps the aftermath line off the
/// overwhelmingly common fleet's Settings page. Every `sync_entries` row starts
/// with `current_root_sealed = 0`, so a pass that ran (and recorded) for a
/// non-successor would report a corpus-sized backlog to a user who never had an
/// aftermath at all.
#[tokio::test]
async fn an_identity_that_never_succeeded_records_no_pass() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x43u8; 32])),
    );
    // No `set_predecessor_backup_keys` — the ordinary account.

    let original: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let rel = "docs/ordinary.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let manifest = store_sealed_fixture(
        &store,
        &original,
        &BackupKey::from_bytes([0x43u8; 32]).convergent_chunk_root(),
    );
    seed_synced_entry(&engine, rel, &original, manifest);
    assert_eq!(
        engine
            .db()
            .list_pending_current_root_reseal()
            .unwrap()
            .len(),
        1,
        "the row is unmarked like every freshly recorded row — which is precisely \
         why the pass, not the row state, must decide whether anything is reported"
    );

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    assert_eq!(
        engine.db().corpus_reseal_pass().unwrap(),
        None,
        "an account with no predecessors owes no aftermath and must report none"
    );
}

/// **The root-generation guard, driven through the pass that consumes the
/// sentinels** (`crate::succession_drain`, ruling 5).
///
/// A second succession (B→C) leaves every B-era entry stamped
/// `current_root_sealed` while its bytes rest under the root B retired. Nothing
/// cleared that boolean, so the pass skipped exactly the corpus that needed it
/// and `list_pending_current_root_reseal` drained **vacuously** — handing
/// `sync-agent.md` bound (3) a proof that is false and licensing the drop of the
/// only keys that could still open those bytes.
///
/// The guard runs at the head of the pass itself rather than in the drive loops
/// that call it, so this test is also what pins the wiring: unwiring it is not
/// possible without deleting the line this asserts the effect of.
#[tokio::test]
async fn a_root_generation_change_re_examines_entries_a_stale_sentinel_marked_done() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let predecessor = BackupKey::from_bytes([0x33u8; 32]);
    let successor = BackupKey::from_bytes([0x44u8; 32]);

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor.clone()),
    );
    engine.set_predecessor_backup_keys(vec![predecessor.clone().into()]);

    let original: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(37) % 251) as u8)
        .collect();
    let rel = "docs/twice-inherited.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let pred_manifest =
        store_sealed_fixture(&store, &original, &predecessor.convergent_chunk_root());
    seed_synced_entry(&engine, rel, &original, pred_manifest);

    // The state a previous identity left behind: the entry is stamped done, and
    // the sentinels belong to an actor this engine is no longer running as.
    engine.db().mark_current_root_sealed(rel).unwrap();
    engine
        .db()
        .adopt_sentinel_root_actor(&hex::encode([0xEEu8; 32]))
        .unwrap();
    assert!(
        engine
            .db()
            .list_pending_current_root_reseal()
            .unwrap()
            .is_empty(),
        "precondition: the stale sentinel makes the corpus look drained"
    );

    engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");

    // The proof that the entry was EXAMINED rather than skipped: the pass moved
    // its bytes off the retired root and POSTed the re-sealed manifest. (The
    // entry stays *owed* here on purpose - this harness's nest does not complete
    // the `changes.record` leg, and a ruling settled that the sentinel follows the
    // record, not the upload. What this pin is about is reachability, and a
    // skipped entry POSTs nothing at all.)
    let resealed_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the re-examined entry POSTed a re-sealed manifest");
    assert_ne!(
        resealed_hash, pred_manifest,
        "the manifest must be the moved seal, not the predecessor's"
    );

    // ...and the sentinels now belong to the identity that did the work, so the
    // next pass reads them as its own.
    assert_eq!(
        engine.db().sentinel_root_actor().unwrap(),
        Some(engine.owner_actor_id_hex()),
        "the pass adopts the identity it re-sealed under"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Phase 4 — the `public`-audience write arm (folders re-model;
// `principles.md` § The user always controls their data, the one
// deliberate exception)
// ─────────────────────────────────────────────────────────────────────

/// A `public`-audience engine uploads PLAINTEXT **despite holding an owner
/// `BackupKey`** — the flag, never a missing key, is what selects the arm —
/// and the result is world-readable: the manifest carries no `stored_hashes`
/// (the self-describing plaintext shape every reader passes through), the
/// chunks rest keyed by their plaintext hashes, and a KEYLESS reader
/// round-trips the bytes. This is the engine half of the latent-404 closure:
/// the web serve path serves exactly this shape
/// (`bins/fauna-nest/src/web_content/file_bytes.rs`, the
/// `stored_hashes.is_none()` arm).
#[tokio::test]
async fn public_engine_uploads_plaintext_a_keyless_reader_round_trips() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        // The key is PRESENT — a private folder's engine would seal under it.
        Some(BackupKey::from_bytes([0x60u8; 32])),
    )
    .with_public_audience(true);

    let original: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    std::fs::write(watch.path().join("index.html"), &original).unwrap();
    engine
        .upload_file("index.html")
        .await
        .expect("a public engine uploads without any seal root");

    let manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("manifest posted");
    let manifest_bytes = store
        .manifests
        .lock()
        .unwrap()
        .get(&hex::encode(manifest_hash.digest()))
        .cloned()
        .expect("manifest stored");
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).unwrap();
    assert!(
        manifest.stored_hashes.is_none(),
        "a public upload is the self-describing plaintext shape"
    );
    assert!(
        manifest.sealed_hashes.is_none(),
        "a public upload seals nothing, hash siblings included"
    );
    // The chunks rest under their PLAINTEXT hashes (blake3 content addressing
    // — integrity without confidentiality).
    {
        let chunks = store.chunks.lock().unwrap();
        for hash in &manifest.chunk_hashes {
            assert!(
                chunks.contains_key(&hex::encode(hash.digest())),
                "chunk must rest under its plaintext hash"
            );
        }
    }

    // World-readable: a reader holding NO key material round-trips the bytes.
    let reader_watch = tempfile::tempdir().unwrap();
    let keyless_reader = test_sync_engine(&server.uri(), reader_watch.path().to_path_buf(), None);
    let bytes = keyless_reader
        .download_file_bytes_by_manifest(manifest_hash, None, "index.html")
        .await
        .expect("a public file must be readable with no keys at all");
    assert_eq!(bytes, original, "the public bytes round-trip verbatim");
}

/// The owner-only plaintext re-seal pass is a NO-OP on a `public`-audience
/// engine: the whole corpus deliberately IS the plaintext shape that pass
/// exists to migrate away, and without this gate every catch-up pass would
/// seal the site dark, fighting the owner's ratified declassification forever.
#[tokio::test]
async fn public_engine_reseal_owner_only_pass_is_a_noop() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x61u8; 32])),
    )
    .with_public_audience(true);

    // A Synced entry whose recorded manifest is plaintext — exactly what the
    // pass re-seals on a private folder.
    let original: Vec<u8> = (0..90_000u32)
        .map(|i| (i.wrapping_mul(37) % 251) as u8)
        .collect();
    let rel = "site/page.html";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let plain_manifest_hash = store_plaintext_fixture(&store, &original);
    let file_hash = ContentHash::of_raw(&original);
    engine
        .db()
        .upsert_entry(
            rel,
            Some(file_hash),
            Some(file_hash),
            Some(plain_manifest_hash),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed public entry");
    engine.stamp_own_head_for_test(rel);

    assert_eq!(
        engine
            .reseal_owner_only_plaintext()
            .await
            .expect("the pass must return cleanly"),
        0,
        "a public folder's plaintext corpus is deliberate — nothing to migrate"
    );
    assert!(
        store.last_manifest_hash.lock().unwrap().is_none(),
        "the pass must not have re-uploaded (re-sealed) anything"
    );
}

/// Phase 4 slice 4b — the DECLASSIFY pass: a public-armed engine moves its
/// sealed back-catalogue to the world-readable plaintext shape, a keyless
/// reader then round-trips it, the path's stale `owner_sealed` marker is
/// cleared, and the folder-level `corpus_audience` gate records the converged
/// shape so the steady-state pass is one meta-row read.
#[tokio::test]
async fn converge_corpus_declassifies_a_sealed_corpus_for_a_public_engine() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    // The pre-declassify life: a SEALED engine uploads the file.
    let key = BackupKey::from_bytes([0x62u8; 32]);
    let up_watch = tempfile::tempdir().unwrap();
    let uploader = test_sync_engine(
        &server.uri(),
        up_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let original: Vec<u8> = (0..120_000u32)
        .map(|i| (i.wrapping_mul(41) % 251) as u8)
        .collect();
    let rel = "site/index.html";
    let full = up_watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    uploader.upload_file(rel).await.expect("sealed seed upload");
    let sealed_manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("sealed manifest posted");

    // The owner declassifies: the engine rebuilds PUBLIC-armed (same key held
    // — it still opens the sealed corpus), pointing at the same local file.
    let engine = test_sync_engine(&server.uri(), up_watch.path().to_path_buf(), Some(key))
        .with_public_audience(true);
    let file_hash = ContentHash::of_raw(&original);
    engine
        .db()
        .upsert_entry(
            rel,
            Some(file_hash),
            Some(file_hash),
            Some(sealed_manifest_hash),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed entry");
    engine.stamp_own_head_for_test(rel);
    // A stale sealed-marker from the pre-declassify life — the declassify must
    // clear it, else a later flip-back's re-seal would skip this path forever.
    engine.db().mark_owner_sealed(rel).unwrap();

    expect_unrecorded(engine.converge_corpus_to_audience().await, 1);

    // The re-recorded manifest is the plaintext shape, a KEYLESS reader
    // round-trips it, and the bookkeeping settled.
    let new_manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("declassify re-recorded a manifest");
    assert_ne!(new_manifest_hash, sealed_manifest_hash);
    let manifest_bytes = store
        .manifests
        .lock()
        .unwrap()
        .get(&hex::encode(new_manifest_hash.digest()))
        .cloned()
        .expect("plaintext manifest stored");
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).unwrap();
    assert!(manifest.stored_hashes.is_none(), "declassified = plaintext");
    let reader_watch = tempfile::tempdir().unwrap();
    let keyless = test_sync_engine(&server.uri(), reader_watch.path().to_path_buf(), None);
    let bytes = keyless
        .download_file_bytes_by_manifest(new_manifest_hash, None, rel)
        .await
        .expect("world-readable after declassify");
    assert_eq!(bytes, original);
    // Case (B), pinned: the record did NOT land, so the corpus has not
    // converged and nothing may claim it did.
    let entry = engine.db().get_entry(rel).unwrap().expect("entry");
    assert!(
        entry.owner_sealed,
        "an unrecorded declassify leaves the sealed marker standing — the nest \
         head still names the sealed manifest, so the marker is still TRUE"
    );
    assert_eq!(
        engine.db().corpus_audience().unwrap(),
        None,
        "the `corpus_audience` stamp is withheld: `current == target` would \
         short-circuit every later pass and make the miss permanent"
    );
    // …so the next pass RE-DRIVES rather than short-circuiting. That retry is
    // the whole point of withholding the stamp.
    expect_unrecorded(engine.converge_corpus_to_audience().await, 1);
}

/// Phase 4 slice 4b — the FLIP-BACK: a sealed-posture engine re-seals a corpus
/// left plaintext by a public window — INCLUDING a path whose `owner_sealed`
/// marker survived stale-true (set before the window on this or another
/// schedule). Without `clear_all_owner_sealed` the mark-gated pass would skip
/// it and the path would rest plaintext forever after the owner re-sealed.
#[tokio::test]
async fn converge_corpus_reseals_after_a_public_window_despite_stale_markers() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x63u8; 32])),
    );

    // The public-window residue: a plaintext manifest at the head, the local
    // file on disk, a STALE sealed-marker, and the corpus marked plaintext.
    let original: Vec<u8> = (0..100_000u32)
        .map(|i| (i.wrapping_mul(43) % 251) as u8)
        .collect();
    let rel = "site/page.html";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let plain_manifest_hash = store_plaintext_fixture(&store, &original);
    let file_hash = ContentHash::of_raw(&original);
    engine
        .db()
        .upsert_entry(
            rel,
            Some(file_hash),
            Some(file_hash),
            Some(plain_manifest_hash),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed entry");
    engine.stamp_own_head_for_test(rel);
    engine.db().mark_owner_sealed(rel).unwrap(); // the stale-true marker
    engine.db().set_corpus_audience("plaintext").unwrap();

    // The stale marker must not shield the plaintext path from the re-seal —
    // the pass reaches it (the shortfall is 1 of 1, not 0 of 0, and the sealed
    // manifest below provably posted).
    expect_unrecorded(engine.converge_corpus_to_audience().await, 1);

    let sealed_manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("re-seal posted a manifest");
    assert_ne!(sealed_manifest_hash, plain_manifest_hash);
    let manifest_bytes = store
        .manifests
        .lock()
        .unwrap()
        .get(&hex::encode(sealed_manifest_hash.digest()))
        .cloned()
        .unwrap();
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).unwrap();
    assert!(
        manifest.stored_hashes.is_some(),
        "flipped back = sealed again"
    );
    // Case (B) on the flip-back arm: the sealed bytes are up, but the nest
    // head still names the plaintext manifest, so the corpus has NOT converged
    // and the meta row stays where it was — leaving the next pass to re-drive.
    assert_eq!(
        engine.db().corpus_audience().unwrap().as_deref(),
        Some("plaintext"),
        "an unrecorded flip-back must not advance the stamp to \"sealed\""
    );
}

/// The BOUND flip-back: a group-bound engine whose folder was
/// flipped `public` and back must re-seal its public-window plaintext under
/// the set's CURRENT content key — on every seat, owner's and member's alike,
/// off the projected audience alone (no custody sentinel: phase 4's own ruling,
/// and the sentinel is per-actor so it structurally cannot reach a member's
/// engine). Before the fix, `converge_corpus_to_audience`'s sealed branch
/// reached only the owner-only pass, which correctly no-ops on a bound engine —
/// so the corpus stayed world-readable while the pass stamped it "sealed", and
/// the `current == target` short-circuit made the miss permanent.
#[tokio::test]
async fn converge_corpus_reseals_a_bound_folder_after_a_public_window() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let content_keys = FolderContentKeys::genesis([7u8; 32], 1_000);
    let watch = tempfile::tempdir().unwrap();
    let engine = build_test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        None,
        None,
        Some(test_group_id()),
        Some(content_keys),
    );

    // The public-window residue on this seat: the local file, a plaintext
    // manifest at the head with `content_key_version: None` — exactly what the
    // declassify pass and the public-armed upload arm record — and the corpus
    // marked plaintext.
    let original: Vec<u8> = (0..100_000u32)
        .map(|i| (i.wrapping_mul(47) % 251) as u8)
        .collect();
    let rel = "shared/page.html";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let plain_manifest_hash = store_plaintext_fixture(&store, &original);
    let file_hash = ContentHash::of_raw(&original);
    engine
        .db()
        .upsert_entry(
            rel,
            Some(file_hash),
            Some(file_hash),
            Some(plain_manifest_hash),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed entry");
    engine.stamp_own_head_for_test(rel);
    engine.db().set_corpus_audience("plaintext").unwrap();

    // A bound engine must re-seal its public-window entries at flip-back — it
    // reaches all 1 of them (the sealed manifest below provably posted); only
    // the change record cannot land in this harness.
    expect_unrecorded(engine.converge_corpus_to_audience().await, 1);

    // Sealed for real: the new head manifest carries stored hashes, the bound
    // engine round-trips it under the current generation, and a keyless reader
    // recovers nothing.
    let sealed_manifest_hash = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the flip-back must have re-recorded a sealed manifest");
    assert_ne!(sealed_manifest_hash, plain_manifest_hash);
    let manifest_bytes = store
        .manifests
        .lock()
        .unwrap()
        .get(&hex::encode(sealed_manifest_hash.digest()))
        .cloned()
        .unwrap();
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).unwrap();
    assert!(
        manifest.stored_hashes.is_some(),
        "flipped back = sealed under the content key again"
    );
    let bytes = engine
        .download_file_bytes_by_manifest(sealed_manifest_hash, Some(1), rel)
        .await
        .expect("the bound engine round-trips its own re-seal");
    assert_eq!(bytes, original);
    let reader_watch = tempfile::tempdir().unwrap();
    let keyless = test_sync_engine(&server.uri(), reader_watch.path().to_path_buf(), None);
    let leaked = keyless
        .download_file_bytes_by_manifest(sealed_manifest_hash, None, rel)
        .await;
    assert!(
        leaked.map(|b| b != original).unwrap_or(true),
        "a keyless reader must NOT recover plaintext after the flip-back re-seal"
    );
    // Case (B): the re-sealed bytes are up but their record did not land,
    // so the stamp stays put and the next pass re-drives.
    assert_eq!(
        engine.db().corpus_audience().unwrap().as_deref(),
        Some("plaintext"),
        "an unrecorded flip-back must not advance the stamp to \"sealed\""
    );
}

/// Phase 5 (`file-sync.md` § Content residency) — the seat-side byte gate: a
/// metadata-only-armed engine records METADATA exactly as always — the
/// manifest posts, the local row lands — but uploads NO chunk bytes: they
/// deliberately never rest on the nest (the armed flag rides the same
/// folder-list read as the audience, installed at build and per refresh
/// tick). The disarmed twin below the assertions proves the gate is the flag,
/// not an accident of the fixture.
#[tokio::test]
async fn a_metadata_only_seat_posts_the_manifest_but_no_chunk_bytes() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x65u8; 32])),
    )
    .with_metadata_only_residency(true);

    let original: Vec<u8> = (0..150_000u32)
        .map(|i| (i.wrapping_mul(53) % 251) as u8)
        .collect();
    let rel = "vault/doc.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    engine.upload_file(rel).await.expect("metadata-only upload");

    assert!(
        store.chunks.lock().unwrap().is_empty(),
        "a metadata-only seat must upload NO chunk bytes"
    );
    assert!(
        store.last_manifest_hash.lock().unwrap().is_some(),
        "the manifest IS metadata and must still post"
    );
    let entry = engine.db().get_entry(rel).unwrap().expect("row recorded");
    assert_eq!(entry.state, SyncState::Synced, "metadata records as always");

    // The disarmed twin: the same engine shape with the flag off uploads
    // bytes — the gate is the residency flag, nothing else.
    let watch2 = tempfile::tempdir().unwrap();
    let full_seat = test_sync_engine(
        &server.uri(),
        watch2.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x65u8; 32])),
    );
    let full2 = watch2.path().join(rel);
    std::fs::create_dir_all(full2.parent().unwrap()).unwrap();
    std::fs::write(&full2, &original).unwrap();
    full_seat.upload_file(rel).await.expect("full upload");
    assert!(
        !store.chunks.lock().unwrap().is_empty(),
        "a full-residency seat uploads bytes as always"
    );
}

/// Phase 4 slice 4b — the fleet pays NOTHING for folders whose audience never
/// flipped: an absent `corpus_audience` meta row reads as the sealed
/// shape (a folder that never flipped), so a sealed-posture engine's convergence is a no-op that fetches no
/// manifests and re-uploads nothing.
#[tokio::test]
async fn converge_corpus_is_a_noop_for_a_never_flipped_folder() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x64u8; 32])),
    );
    assert_eq!(engine.converge_corpus_to_audience().await.unwrap(), 0);
    assert!(
        store.last_manifest_hash.lock().unwrap().is_none(),
        "steady state must touch nothing"
    );
    assert_eq!(
        engine.db().corpus_audience().unwrap(),
        None,
        "the no-op writes nothing — absent keeps reading as the sealed shape"
    );
}

/// Row 184 — the WEBSITE corpus convergence: a SEALED folder whose owner turns
/// the website toggle on AFTER the content synced re-records its back-catalogue,
/// because the nest's own enable-time backfill structurally cannot reach it (a
/// sealed head rests `path = NULL`, S9). The re-record is what carries each file
/// into `web_files` at arrival — proved end-to-end against a real nest by
/// `bins/fauna-nest/tests/conformance_web_files_sealed_backfill.rs`; this pins
/// the client half: the walk runs, the marker settles, steady state is free.
///
/// Authority: `docs/goal/behavior/web-content-hosting.md` § Content model.
#[tokio::test]
async fn converge_corpus_to_website_reissues_a_sealed_back_catalogue() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    // The pre-toggle life: a sealed engine syncs the site's files.
    let key = BackupKey::from_bytes([0x84u8; 32]);
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), Some(key));
    let original: Vec<u8> = (0..90_000u32)
        .map(|i| (i.wrapping_mul(29) % 251) as u8)
        .collect();
    let rel = "site/chapter-one.pdf";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    engine.upload_file(rel).await.expect("pre-toggle sync");
    let synced_manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the pre-toggle sync posted a manifest");

    // Toggle still OFF: the pass is one meta-row read and writes nothing, so a
    // folder that serves no website never pays for this mechanism.
    assert_eq!(
        engine
            .converge_corpus_to_website()
            .await
            .expect("the toggle-off pass must return cleanly"),
        0
    );
    assert_eq!(
        engine.db().corpus_website().unwrap(),
        None,
        "the fleet pays nothing for folders serving no website: absent already \
         reads as `off`, so the no-op writes not even a marker"
    );

    // The owner turns the website on. The engine learns it off the folder list
    // (`SeatResolution::website_enabled`); here it is armed directly.
    *store.last_manifest_hash.lock().unwrap() = None;
    let served = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x84u8; 32])),
    )
    .with_website_enabled(true);
    let file_hash = ContentHash::of_raw(&original);
    served
        .db()
        .upsert_entry(
            rel,
            Some(file_hash),
            Some(file_hash),
            Some(synced_manifest),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed the already-synced back-catalogue entry");
    served.stamp_own_head_for_test(rel);

    // The already-synced head is re-recorded so the nest can project it — and
    // since the record itself cannot land in this harness, the pass reports the
    // shortfall rather than claiming the projection gained the path. That is
    // the discipline: this pass's PRODUCT is the change
    // record, so a refused one means it achieved nothing at rest.
    expect_unrecorded(served.converge_corpus_to_website().await, 1);
    // The seal is unchanged — this pass buys the change RECORD, not a re-seal —
    // so the convergent re-upload lands the byte-identical manifest.
    assert_eq!(
        store
            .last_manifest_hash
            .lock()
            .unwrap()
            .expect("the convergence re-recorded a manifest"),
        synced_manifest,
        "a no-novel-content reissue must publish the same sealed manifest"
    );
    assert_eq!(
        served.db().corpus_website().unwrap(),
        None,
        "an unrecorded reissue must not stamp the marker: the projection never \
         gained the path, and the stamp's short-circuit would make that permanent"
    );

    // …so the next pass RE-DRIVES instead of steadying. Withholding the stamp
    // is what buys that retry.
    *store.last_manifest_hash.lock().unwrap() = None;
    expect_unrecorded(served.converge_corpus_to_website().await, 1);
    assert!(
        store.last_manifest_hash.lock().unwrap().is_some(),
        "the re-drive really re-ran the walk"
    );
}

/// Row 184 — the pass is a SEALED folder's mechanism alone. A `public`-audience
/// folder rests plaintext, which is exactly the class the nest's enable-time
/// backfill folds (`web-content-hosting.md` § Content model, row 182), so
/// re-recording its corpus on every device would duplicate that work for
/// nothing. The toggle being ON is not enough — the corpus must be sealed.
#[tokio::test]
async fn converge_corpus_to_website_does_nothing_for_a_public_folder() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x85u8; 32])),
    )
    .with_public_audience(true)
    .with_website_enabled(true);
    let original = b"<!doctype html><title>hello</title>".to_vec();
    let rel = "index.html";
    std::fs::write(watch.path().join(rel), &original).unwrap();
    engine.upload_file(rel).await.expect("public sync");
    *store.last_manifest_hash.lock().unwrap() = None;

    assert_eq!(
        engine
            .converge_corpus_to_website()
            .await
            .expect("the pass must return cleanly"),
        0,
        "a public folder's back-catalogue is the nest-side backfill's job"
    );
    assert!(
        store.last_manifest_hash.lock().unwrap().is_none(),
        "the pass must not have re-recorded anything"
    );
    assert_eq!(
        engine.db().corpus_website().unwrap(),
        None,
        "public + toggle-on IS the no-work shape — it reads as already converged, \
         so the pass writes nothing at all"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Part (D) — an owner with no resident replica
// (`mls-group-key-material.md` § M2 → *Pre-bind re-seal migration*)
// ─────────────────────────────────────────────────────────────────────

/// Store a file sealed under `root` as an arbitrary list of chunks (the
/// chunking a re-seal must preserve, and small enough that several windows of
/// `ManifestWalk::WINDOW` are walked without a multi-megabyte fixture).
fn store_sealed_parts_fixture(
    store: &crate::test_support::BlobStore,
    parts: &[Vec<u8>],
    root: &[u8; 32],
) -> (ContentHash, Vec<u8>) {
    let original: Vec<u8> = parts.concat();
    let mut chunk_hashes = Vec::new();
    let mut stored_hashes = Vec::new();
    for part in parts {
        let hash = ContentHash::of_raw(part);
        let (store_key, ciphertext) = crate::seal::seal_chunk_body(&hash, part, root).unwrap();
        chunk_hashes.push(hash);
        stored_hashes.push(store_key);
        store
            .chunks
            .lock()
            .unwrap()
            .insert(hex::encode(store_key.digest()), ciphertext);
    }
    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash: ContentHash::of_raw(&original),
        total_size: original.len() as u64,
        chunk_hashes,
        chunk_sizes: parts.iter().map(|p| p.len() as u64).collect(),
        stored_hashes: Some(stored_hashes),
        sealed_hashes: None,
        min_reader: None,
    };
    let manifest_bytes =
        fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(root)).unwrap()).unwrap();
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    store
        .manifests
        .lock()
        .unwrap()
        .insert(hex::encode(manifest_hash.digest()), manifest_bytes);
    (manifest_hash, original)
}

/// Seed the row a cloud-only pre-bind file leaves on an on-demand replica: the
/// recorded head is known, the bytes are not on this disk, and the record
/// carries no generation stamp.
fn seed_unstamped_placeholder(engine: &SyncEngine, rel: &str, size: usize, head: ContentHash) {
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(head),
            SyncState::Placeholder,
            0,
            0,
            size as i64,
            1,
            None,
        )
        .expect("seed placeholder entry");
    engine.stamp_own_head_for_test(rel);
}

fn bound_engine_for(
    server_uri: &str,
    watch: &std::path::Path,
    backup_key: Option<BackupKey>,
    key1: [u8; 32],
    set_owner: bool,
) -> SyncEngine {
    build_test_engine(
        server_uri,
        watch.to_path_buf(),
        None,
        backup_key,
        Some(test_group_id()),
        Some(FolderContentKeys::genesis(key1, 1_000)),
    )
    .owned_if(set_owner)
}

trait OwnedIf {
    fn owned_if(self, owner: bool) -> Self;
}

impl OwnedIf for SyncEngine {
    fn owned_if(self, owner: bool) -> Self {
        if owner {
            self.into_verified_owner_for_test()
        } else {
            self
        }
    }
}

/// The owner's bound on-demand engine still hydrates a pre-bind file — the
/// record carries no stamp and rests under the owner root — while a member's
/// bound engine holding the same generation cannot open it (decision 4 of
/// `on-demand-files.md` § Shared sets on a capability host: every file stays
/// openable on the owner's device through the share).
#[tokio::test]
async fn the_owners_bound_engine_still_hydrates_an_unstamped_prebind_file() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let owner_key = BackupKey::from_bytes([0x3au8; 32]);
    let key1 = [0x3bu8; 32];
    let parts: Vec<Vec<u8>> = (0u8..3).map(|i| vec![i ^ 0x5a; 3_000]).collect();
    let (head, original) =
        store_sealed_parts_fixture(&store, &parts, &owner_key.convergent_chunk_root());

    let watch = tempfile::tempdir().unwrap();
    let owner = bound_engine_for(
        &server.uri(),
        watch.path(),
        Some(owner_key.clone()),
        key1,
        true,
    );
    let bytes = owner
        .download_file_bytes_by_manifest(head, None, "shared/pre.bin")
        .await
        .expect("the owner's bound engine opens its own unstamped pre-bind record");
    assert_eq!(bytes, original);

    let member_watch = tempfile::tempdir().unwrap();
    let member = bound_engine_for(
        &server.uri(),
        member_watch.path(),
        Some(BackupKey::from_bytes([0x3cu8; 32])),
        key1,
        false,
    );
    member
        .download_file_bytes_by_manifest(head, None, "shared/pre.bin")
        .await
        .expect_err("a member's bound engine never opens an unstamped record");
}

/// The pass on an owner whose replica is on-demand: a pre-bind file held only
/// as a placeholder is re-sealed **from the nest**, window by window, under the
/// set's current generation — without hydrating it — and a member holding only
/// that generation then decrypts it. The re-sealed manifest keeps the source's
/// chunk list and is the one a whole-buffer seal of the same bytes produces
/// (so a re-run converges on the same head).
#[tokio::test]
async fn an_owners_pass_reseals_an_unstamped_placeholder_from_the_nest_so_a_member_decrypts() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let owner_key = BackupKey::from_bytes([0x4au8; 32]);
    let key1 = [0x4bu8; 32];
    // Nine chunks: three windows of `ManifestWalk::WINDOW`.
    let parts: Vec<Vec<u8>> = (0u8..9)
        .map(|i| {
            (0..2_000u32)
                .map(|j| (j as u8).wrapping_mul(i + 1))
                .collect()
        })
        .collect();
    let (head, original) =
        store_sealed_parts_fixture(&store, &parts, &owner_key.convergent_chunk_root());

    let watch = tempfile::tempdir().unwrap();
    let rel = "shared/cloud-only.bin";
    let owner = bound_engine_for(
        &server.uri(),
        watch.path(),
        Some(owner_key.clone()),
        key1,
        true,
    );
    seed_unstamped_placeholder(&owner, rel, original.len(), head);

    // The change record cannot land on this module's unconnected nest client
    // (`expect_unrecorded`); the byte plane is what this pins.
    expect_unrecorded(owner.reseal_pending_under_current().await, 1);

    assert!(
        !watch.path().join(rel).exists(),
        "the nest-sourced re-seal never hydrates the placeholder"
    );
    let resealed = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the pass posted a re-sealed manifest");
    assert_ne!(resealed, head);

    let resealed_manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(
            store
                .manifests
                .lock()
                .unwrap()
                .get(&hex::encode(resealed.digest()))
                .expect("manifest stored"),
        )
        .unwrap();
    assert!(
        resealed_manifest.chunk_hashes.is_empty() && resealed_manifest.sealed_hashes.is_some(),
        "the re-sealed manifest names its plaintext hashes only sealed"
    );
    let resealed_manifest = resealed_manifest
        .unseal_hashes(&key1)
        .expect("the re-sealed hashes open under the current generation's key");
    assert_eq!(
        resealed_manifest.chunk_hashes.len(),
        9,
        "the source's chunk list is kept"
    );
    assert_eq!(
        resealed_manifest.stored_hashes,
        Some(original_parts_store_keys(&parts, &key1)),
        "every chunk is re-sealed under the current generation's key"
    );
    assert_eq!(resealed_manifest.file_hash, ContentHash::of_raw(&original));

    let member_watch = tempfile::tempdir().unwrap();
    let member = bound_engine_for(&server.uri(), member_watch.path(), None, key1, false);
    let bytes = member
        .download_file_bytes_by_manifest(resealed, Some(1), rel)
        .await
        .expect("a member holding generation 1 decrypts the re-sealed pre-bind file");
    assert_eq!(bytes, original);
}

fn original_parts_store_keys(parts: &[Vec<u8>], key: &[u8; 32]) -> Vec<ContentHash> {
    parts
        .iter()
        .map(|p| {
            crate::seal::seal_chunk_body(&ContentHash::of_raw(p), p, key)
                .unwrap()
                .0
        })
        .collect()
}

/// A member's engine walks materialized rows only: an unstamped placeholder
/// there is the owner's pre-bind content, which the member can neither open
/// nor is its to move — so the pass touches nothing and succeeds, rather than
/// failing on the first such row and stranding every row behind it.
#[tokio::test]
async fn a_members_pass_leaves_an_unstamped_placeholder_alone() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let owner_key = BackupKey::from_bytes([0x5au8; 32]);
    let key1 = [0x5bu8; 32];
    let (head, original) = store_sealed_parts_fixture(
        &store,
        &[b"the owner's pre-bind file".to_vec()],
        &owner_key.convergent_chunk_root(),
    );

    let watch = tempfile::tempdir().unwrap();
    let member = bound_engine_for(
        &server.uri(),
        watch.path(),
        Some(BackupKey::from_bytes([0x5cu8; 32])),
        key1,
        false,
    );
    seed_unstamped_placeholder(&member, "shared/theirs.bin", original.len(), head);

    assert_eq!(
        member
            .reseal_pending_under_current()
            .await
            .expect("the member's pass succeeds"),
        0
    );
    assert!(
        store.last_manifest_hash.lock().unwrap().is_none(),
        "nothing was re-sealed"
    );
}

/// A nest that serves bytes which do not address the recorded head gets
/// nothing recorded: the windowed re-seal checks the whole-file address before
/// the new manifest posts, so a swapped chunk under a manifest whose file hash
/// names other content never becomes the path's head.
#[tokio::test]
async fn the_windowed_reseal_posts_nothing_when_the_nest_copy_does_not_address_its_head() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let owner_key = BackupKey::from_bytes([0x6au8; 32]);
    let key1 = [0x6bu8; 32];
    let (head, original) = store_sealed_parts_fixture(
        &store,
        &[vec![1u8; 500], vec![2u8; 500]],
        &owner_key.convergent_chunk_root(),
    );
    // Re-point a manifest at the same chunks with a lying file hash.
    let root = owner_key.convergent_chunk_root();
    let manifest: fauna_core::chunk::ChunkManifest = fauna_core::encoding::canonical_decode(
        store
            .manifests
            .lock()
            .unwrap()
            .get(&hex::encode(head.digest()))
            .unwrap(),
    )
    .unwrap();
    let mut manifest = manifest.unseal_hashes(&root).unwrap();
    manifest.file_hash = ContentHash::of_raw(b"a different file");
    let lying =
        fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(&root)).unwrap()).unwrap();
    let lying_head = ContentHash::of_raw(&lying);
    store
        .manifests
        .lock()
        .unwrap()
        .insert(hex::encode(lying_head.digest()), lying);

    let watch = tempfile::tempdir().unwrap();
    let owner = bound_engine_for(&server.uri(), watch.path(), Some(owner_key), key1, true);
    seed_unstamped_placeholder(&owner, "shared/lie.bin", original.len(), lying_head);
    let chunks_before = store.chunks.lock().unwrap().len();

    owner
        .reseal_pending_under_current()
        .await
        .expect_err("a nest copy that does not address its head fails the pass");
    assert!(
        store.last_manifest_hash.lock().unwrap().is_none(),
        "no manifest posts over content that is not the recorded head's"
    );
    assert_eq!(
        store.chunks.lock().unwrap().len(),
        chunks_before,
        "and not one chunk is re-sealed under the set's generation and uploaded: a stored \
         group-sealed chunk is readable by every member whether or not a manifest names it"
    );
}

/// One row whose nest copy will not open (a withheld chunk) does not end a
/// capability host's drive: the step logs it, leaves it owed, and the next
/// step moves the row behind it.
#[tokio::test]
async fn the_one_file_step_continues_past_a_row_that_will_not_open() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let owner_key = BackupKey::from_bytes([0x8au8; 32]);
    let key1 = [0x8bu8; 32];
    let watch = tempfile::tempdir().unwrap();
    let owner = bound_engine_for(
        &server.uri(),
        watch.path(),
        Some(owner_key.clone()),
        key1,
        true,
    );
    let (bad_head, bad) = store_sealed_parts_fixture(
        &store,
        &[vec![0xaau8; 900]],
        &owner_key.convergent_chunk_root(),
    );
    // Withhold the bad file's only chunk.
    let withheld = crate::seal::seal_chunk_body(
        &ContentHash::of_raw(&bad),
        &bad,
        &owner_key.convergent_chunk_root(),
    )
    .unwrap()
    .0;
    store
        .chunks
        .lock()
        .unwrap()
        .remove(&hex::encode(withheld.digest()));
    seed_unstamped_placeholder(&owner, "a/withheld.bin", bad.len(), bad_head);
    let (good_head, good) = store_sealed_parts_fixture(
        &store,
        &[vec![0xbbu8; 900]],
        &owner_key.convergent_chunk_root(),
    );
    seed_unstamped_placeholder(&owner, "b/fine.bin", good.len(), good_head);

    let mut attempted = std::collections::HashSet::new();
    while owner
        .reseal_next_pending_under_current(&mut attempted)
        .await
        .expect("a row failure is not a drive failure")
    {}
    assert_eq!(attempted.len(), 2, "both rows were attempted");
    let resealed = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the row that opens was re-sealed despite the one that did not");
    let member_watch = tempfile::tempdir().unwrap();
    let member = bound_engine_for(&server.uri(), member_watch.path(), None, key1, false);
    assert_eq!(
        member
            .download_file_bytes_by_manifest(resealed, Some(1), "b/fine.bin")
            .await
            .unwrap(),
        good
    );
}

/// The one-file step a capability host drives between OS requests walks the
/// same owed rows as the whole pass: each call attempts one row it has not
/// attempted yet, and reports `false` once none is left — so a row whose
/// record did not land is not re-driven in a tight loop within one drive.
#[tokio::test]
async fn the_one_file_step_walks_each_owed_row_once_then_reports_done() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let owner_key = BackupKey::from_bytes([0x7au8; 32]);
    let key1 = [0x7bu8; 32];
    let watch = tempfile::tempdir().unwrap();
    let owner = bound_engine_for(
        &server.uri(),
        watch.path(),
        Some(owner_key.clone()),
        key1,
        true,
    );
    for (i, rel) in ["shared/a.bin", "shared/b.bin"].into_iter().enumerate() {
        let (head, original) = store_sealed_parts_fixture(
            &store,
            &[vec![i as u8 + 1; 700]],
            &owner_key.convergent_chunk_root(),
        );
        seed_unstamped_placeholder(&owner, rel, original.len(), head);
    }

    let mut attempted = std::collections::HashSet::new();
    assert!(
        owner
            .reseal_next_pending_under_current(&mut attempted)
            .await
            .unwrap()
    );
    assert!(
        owner
            .reseal_next_pending_under_current(&mut attempted)
            .await
            .unwrap()
    );
    assert!(
        !owner
            .reseal_next_pending_under_current(&mut attempted)
            .await
            .unwrap(),
        "both rows attempted — the drive is done"
    );
    assert_eq!(attempted.len(), 2);
    assert!(store.last_manifest_hash.lock().unwrap().is_some());
}
