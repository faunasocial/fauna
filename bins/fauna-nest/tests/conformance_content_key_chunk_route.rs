//! tier_3: the **real** `/api/v1/chunks` HTTP route accepts + serves a bound
//! shared folder's **content-key-encrypted** chunks — the FS-BIND real-route
//! capstone (§ 6 FOLLOW-ON B).
//!
//! Background. A bound (cross-user shared) folder seals each chunk under its
//! M2 **content key** (`FolderContentKeys::current_key`); the nest holds no key,
//! so it cannot verify the ciphertext against the *plaintext* hash a manifest
//! references. FS-BIND-1/2 (Slice 3, PIECE 6 — ciphertext-hash store keying)
//! resolved this by keying a content-key chunk in the blob store by its
//! **ciphertext** hash: the client uploads the ciphertext with
//! `X-Content-Hash = blake3(ciphertext)`, which the route's existing raw-path
//! check (`chunk_routes::resolve_verified_chunk_hash`, path (a):
//! `blake3(body) == header`) accepts **with no route change** — the F9
//! anti-poisoning defense is untouched.
//!
//! Why this file exists. The piece-7 rotate-on-removal proof
//! (`conformance_shared_folders.rs`) runs the byte plane over a `wiremock`
//! store (the sanctioned tier_3 "byte routes out of scope" split; its fail-closed
//! property is a *client-side* crypto fact). The tier_2
//! `download_file_bytes_test::bound_engine_chunk_upload_round_trips_through_
//! verifying_store` proves the engine keys+sends correctly against a mock that
//! *re-implements* the real F9 check. This file closes the remaining gap: it
//! drives the **real** axum route + a real `BackupService` / `DiskBlobStore`, so
//! the composition "engine sends X ∧ mock-check == real-check" is replaced by a
//! single end-to-end assertion against the production route.
//!
//! Two proofs:
//!  A. a bound `SyncEngine`'s `upload_file` succeeds against the real route
//!     (`Ok` ⟺ every content-key chunk POST returned 2xx ⟺ `resolve_verified_
//!     chunk_hash` accepted the ciphertext; a `400` propagates). This is RED
//!     before FS-BIND-1/2 — the engine then keyed the ciphertext body by the
//!     *plaintext* hash, which the route rejects.
//!  B. the real route accepts a ciphertext body keyed by `blake3(ciphertext)`,
//!     the real GET route serves it back byte-identically, it decrypts under the
//!     content key, AND the *old* plaintext-hash pairing is rejected `400` (F9 —
//!     the exact reason FS-BIND-1/2 keys by the ciphertext hash).
//!
//! Authority: `docs/goal/architecture/mls-group-key-material.md` § M2 content-key
//! mechanism + `docs/goal/behavior/file-sync.md` § Content-Addressed Storage.

use std::sync::Arc;

use fauna_core::chunk_crypto::decrypt_chunk;
use fauna_core::chunk_seal::seal_chunk_body;
use fauna_core::data::ContentHash;
use fauna_nest::routes::AppState;
use fauna_nest::test_support::EngineFixture;
use fauna_sync_engine::engine::SyncEngine;

mod common;

const OWNER_SECRET: [u8; 32] = [0x42; 32];
const DEVICE_ID: [u8; 32] = [0x07; 32];
/// The bound-marker `mls_group_id` (a bound shared set). Its exact bytes don't
/// matter here — `content_keys` supplies the chunk root; the group id only marks
/// the engine as bound so the content-key seal/open path runs (never `backup_key`).
const GROUP_ID: [u8; 32] = [0x11; 32];
/// The set's genesis M2 content key (version 1). Chunks seal under this.
const CONTENT_KEY: [u8; 32] = [0x7c; 32];

// ─────────────────────────────────────────────────────────────────────
// Real destination nest (real chunk routes + BackupService + DiskBlobStore)
// ─────────────────────────────────────────────────────────────────────

/// Start a real in-process nest serving the sync + folder WS-RPC kinds and the
/// chunk-store HTTP routes, backed by a real `DiskBlobStore` (no at-rest
/// encryption / compression → the store is an opaque passthrough, exactly what a
/// content-addressed store is to a client-sealed ciphertext). The owner is
/// registered and an HTTP bearer minted for the chunk plane.
///
/// The nest-standing body is `fauna_nest::test_support::start_test_nest`, shared
/// with `bins/fauna-sync-agent`'s `tier3-nest` harnesses; only the handler set
/// and the blob dir are this file's. The `tempdir` stays here because
/// `tempfile` is a dev-only dependency of `fauna-nest`.
async fn start_test_nest(owner_secret: [u8; 32]) -> (String, Arc<AppState>, String) {
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the process; never deleted under test

    let nest = fauna_nest::test_support::start_test_nest(owner_secret, Some(blob_path), |b| {
        fauna_nest::auth_handlers::register_auth_handlers(b);
        fauna_nest::discovery_handlers::register_discovery_handlers(b);
        fauna_nest::sync_handlers::register_sync_handlers(b);
        fauna_nest::folder_handlers::register_folders_handlers(b);
    })
    .await;
    (nest.base_url, nest.state, nest.http_token)
}

/// This file's engine fixture — the half every builder here shares.
fn fixture<'a>(
    dest_url: &'a str,
    http_token: &'a str,
    watch_path: std::path::PathBuf,
) -> EngineFixture<'a> {
    EngineFixture {
        dest_url,
        http_token,
        owner_secret: OWNER_SECRET,
        device_id: DEVICE_ID,
        watch_path,
    }
}

/// Build a **bound** (content-key) `SyncEngine` pointed at `dest_url`: no
/// `backup_key` (a bound shared set has none — it seals under the content key),
/// `mls_group_id = Some` (the bound marker), `content_keys = Some(genesis)`. The
/// chunk plane rides a static-bearer HTTP `SyncClient`. `watch_path` is the
/// engine's watch dir (kept alive by the caller).
fn bound_engine(dest_url: &str, http_token: &str, watch_path: std::path::PathBuf) -> SyncEngine {
    fauna_nest::test_support::bound_engine(
        fixture(dest_url, http_token, watch_path),
        &GROUP_ID,
        CONTENT_KEY,
    )
}

// ─────────────────────────────────────────────────────────────────────
// Proof A — a bound engine's real upload is accepted by the real route
// ─────────────────────────────────────────────────────────────────────

/// A bound `SyncEngine` uploads a multi-chunk file through the **real**
/// `/api/v1/chunks` route. `upload_file` returns `Ok` **iff** every content-key
/// chunk POST got a 2xx — i.e. iff `resolve_verified_chunk_hash` accepted the
/// AEAD ciphertext. Before FS-BIND-1/2 the engine keyed the ciphertext body by
/// the *plaintext* hash, so the real route rejected it (`400`) and this errored.
///
/// (`record_change` rides the WS control plane and is best-effort in
/// `upload_file` — a failure there only `warn!`s; the `Ok` return is gated solely
/// on the HTTP chunk/manifest uploads, which is exactly the route under test.)
#[tokio::test]
async fn bound_engine_upload_accepted_by_real_chunk_route() {
    let (url, _state, token) = start_test_nest(OWNER_SECRET).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = bound_engine(&url, &token, watch.path().to_path_buf());

    // Multi-chunk payload (>64 KiB forces FastCDC boundaries), under the 64 MiB
    // streaming threshold so the in-memory chunk path runs.
    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(23) % 251) as u8)
        .collect();
    let rel = "shared/blob.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();

    engine.upload_file(rel).await.expect(
        "the real /api/v1/chunks route must accept a bound set's content-key \
         ciphertext chunks (FS-BIND ciphertext-hash keying); a rejection would \
         propagate a 400 out of upload_file",
    );
}

// ─────────────────────────────────────────────────────────────────────
// Proof B — the real route accepts/serves a ciphertext-keyed body; F9 holds
// ─────────────────────────────────────────────────────────────────────

/// Directly exercise the real route with the **exact** bytes a bound engine
/// uploads (sealed through the same `chunk_seal::seal_chunk_body` door, so
/// this is byte-faithful): POST a content-key ciphertext keyed by
/// `blake3(ciphertext)` is accepted, GET serves it back, it decrypts under the
/// content key — and the *pre-FS-BIND-1/2* pairing (ciphertext body claiming the
/// *plaintext* hash) is rejected `400`, proving the F9 anti-poisoning check is
/// untouched and why the ciphertext-hash keying was required.
#[tokio::test]
async fn real_chunk_route_accepts_serves_and_f9_rejects_content_key_ciphertext() {
    let (url, _state, token) = start_test_nest(OWNER_SECRET).await;

    // A single content-key chunk, sealed exactly as the engine's content-key path
    // does: key/nonce derive from the *plaintext* hash; the store key is the
    // *ciphertext* hash.
    let plaintext = b"content-key chunk bytes for a bound shared folder".to_vec();
    let plaintext_hash = ContentHash::of_raw(&plaintext);
    let (store_key, ciphertext) =
        seal_chunk_body(&plaintext_hash, &plaintext, &CONTENT_KEY).unwrap();

    let client = reqwest::Client::new();
    let chunks_url = format!("{url}/api/v1/chunks");

    // (1) POST the ciphertext keyed by blake3(ciphertext) → accepted (201).
    let resp = client
        .post(&chunks_url)
        .bearer_auth(&token)
        .header("X-Content-Hash", hex::encode(store_key.digest()))
        .body(ciphertext.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        201,
        "real route must accept a content-key ciphertext keyed by its own hash \
         (resolve_verified_chunk_hash path (a): blake3(body) == X-Content-Hash)"
    );

    // (2) GET it back byte-identically from the real DiskBlobStore.
    let resp = client
        .get(format!(
            "{url}/api/v1/chunks/{}",
            hex::encode(store_key.digest())
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "stored ciphertext must be served back"
    );
    let served = resp.bytes().await.unwrap();
    assert_eq!(
        &served[..],
        &ciphertext[..],
        "the store is opaque — it serves the client-sealed ciphertext verbatim"
    );

    // (3) It decrypts under the content key back to the plaintext (round-trip)
    // — the frame rides inside the ciphertext, so the opened body is unframed
    // against the plaintext hash exactly as every reader does.
    let recovered = fauna_core::compress::unframe_verified_chunk(
        decrypt_chunk(&CONTENT_KEY, &plaintext_hash, &served).unwrap(),
        &plaintext_hash,
    )
    .expect("the opened body addresses the plaintext hash");
    assert_eq!(
        recovered, plaintext,
        "the served ciphertext must decrypt under the M2 content key"
    );

    // (4) F9: the pre-FS-BIND-1/2 pairing (ciphertext body, but claiming the
    // *plaintext* hash) is rejected — the real route can't verify ciphertext
    // against a plaintext hash, so the poisoning primitive stays closed. This is
    // exactly why a content-key chunk must key by the ciphertext hash.
    let resp = client
        .post(&chunks_url)
        .bearer_auth(&token)
        .header("X-Content-Hash", hex::encode(plaintext_hash.digest()))
        .body(ciphertext.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        400,
        "a ciphertext body claiming the plaintext hash must be rejected (F9); \
         this is the pairing FS-BIND-1/2 replaced with ciphertext-hash keying"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Proof C — the owner backup path round-trips through the real route
// (FS-BIND FOLLOW-ON A, user-ratified convergent keying 2026-07-07)
// ─────────────────────────────────────────────────────────────────────

/// A `backup_key` engine (the segment-backup / cross-location backup shape:
/// unbound, owner-only) builds like [`bound_engine`] but with `backup_key = Some`
/// and no group/content keys — the FS-BIND FOLLOW-ON A production configuration
/// (`segment_backup.rs` builds exactly this).
fn backup_engine(dest_url: &str, http_token: &str, watch_path: std::path::PathBuf) -> SyncEngine {
    fauna_nest::test_support::backup_engine(fixture(dest_url, http_token, watch_path))
}

/// FS-BIND FOLLOW-ON A capstone: the owner-only `backup_key` path (`upload_bytes`
/// — the exact entry point cross-location segment backup and held-for-friends
/// drive) round-trips through the **real** `/api/v1/chunks` route + a real
/// `DiskBlobStore`, and a restore reassembles the original bytes. Pre-fix this
/// was live-broken: the random-nonce framed seal claimed the plaintext hash, the
/// route rejected every chunk (400), and `upload_bytes` silently swallowed the
/// failure — the destination stored nothing while the coordinator reported
/// success. The convergent `BackupKey::convergent_chunk_root()` seal keys each
/// chunk by its ciphertext hash, which the route's path (a) accepts unchanged.
#[tokio::test]
async fn backup_upload_bytes_round_trips_real_chunk_route() {
    let (url, _state, token) = start_test_nest(OWNER_SECRET).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = backup_engine(&url, &token, watch.path().to_path_buf());

    // Multi-chunk payload (>64 KiB forces FastCDC boundaries) — a stand-in for a
    // fetched source segment.
    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(29) % 251) as u8)
        .collect();

    let manifest_hash = engine
        .upload_bytes(original.clone(), "backup/segment-0001.seg", "__backup")
        .await
        .expect(
            "the real /api/v1/chunks route must accept the owner backup path's \
             convergent ciphertext chunks (FS-BIND FOLLOW-ON A); pre-fix every \
             chunk was rejected 400 and the failure silently swallowed",
        );

    // Restore: fetch the manifest + chunks back through the real routes,
    // decrypt under the convergent backup root, reassemble, verify.
    let restored = engine
        .download_file_bytes_by_manifest(manifest_hash, None, "backup/segment-0001.seg")
        .await
        .expect("restore must fetch + decrypt + reassemble through the real routes");
    assert_eq!(
        restored, original,
        "a cross-location backup restore must reassemble the original segment bytes"
    );
}
