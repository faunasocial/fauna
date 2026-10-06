//! The web binding of the shared client-side file-download walk — the browser
//! twin of the sync engine's `EngineBlobFetcher`.
//!
//! `fauna_core::file_download` owns the walk itself (manifest + chunks by content
//! address → open under the owner root → decompress → reassemble → verify the
//! whole-file address). This module supplies only the two platform legs the web
//! has and shared Rust cannot: the browser `fetch`, and the save (a blob download
//! the SPA performs on the `Uint8Array` returned here).
//!
//! # Why this exists
//!
//! Every snapshot file is sealed and the nest holds no opening key, so the nest
//! cannot reassemble a file for the browser — no server-side reassembly route
//! exists. So the Backups per-file download runs the walk here
//! (`docs/goal/ui/backups.md` § Where logic lives → *Single-file byte download*,
//! re-ratified 2026-07-14; `docs/goal/behavior/backup-restore.md` § 3). The
//! legacy plaintext snapshot routes were removed 2026-09-27.

use fauna_core::data::ContentHash;
use fauna_core::file_download::{FileDownloadKeys, WasmPublicChunkFetcher};
use wasm_bindgen::prelude::*;

/// Download one file's **decrypted** bytes out of a snapshot — the web leg of
/// `snapshot-file-download-button`.
///
/// `manifest_hash` is the file entry's `manifest_hash` from
/// `fauna.filesync.snapshot.get` (raw 32-byte digest, as it rides the wire);
/// `relative_path` is that entry's `path`; `secret_hex` is the 64-char hex
/// Ed25519 identity seed the owner `BackupKey` derives from (the same argument
/// `process_and_seal_library` takes). The SPA saves the returned bytes as a
/// browser blob download.
///
/// A snapshot is an **owner-only** backup, so the walk opens under the derived
/// `BackupKey` and no content-key generation applies — hence
/// `content_key_version: None`. A bound shared folder would stamp one; that is
/// the same shared code path, unused here.
#[wasm_bindgen(js_name = downloadSnapshotFileBytes)]
pub async fn download_snapshot_file_bytes(
    manifest_hash: &[u8],
    relative_path: &str,
    secret_hex: &str,
    nest_url: &str,
) -> Result<Vec<u8>, JsValue> {
    download_snapshot_file_bytes_inner(manifest_hash, relative_path, secret_hex, nest_url)
        .await
        .map_err(crate::rpc::err_to_js)
}

async fn download_snapshot_file_bytes_inner(
    manifest_hash: &[u8],
    relative_path: &str,
    secret_hex: &str,
    nest_url: &str,
) -> anyhow::Result<Vec<u8>> {
    let digest: [u8; 32] = manifest_hash.try_into().map_err(|_| {
        anyhow::anyhow!(
            "manifest_hash must be 32 bytes, got {}",
            manifest_hash.len()
        )
    })?;
    let seed_bytes =
        hex::decode(secret_hex.trim()).map_err(|e| anyhow::anyhow!("bad secret hex: {e}"))?;
    let seed: [u8; 32] = seed_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("secret must be 32 bytes"))?;

    let fetcher = WasmPublicChunkFetcher::new(nest_url.trim_end_matches('/'));
    let keys = FileDownloadKeys::owner(fauna_core::crypto::BackupKey::derive(&seed));

    fauna_core::file_download::download_file_bytes_by_manifest(
        &fetcher,
        &keys,
        ContentHash::from_digest_raw(digest),
        None,
        relative_path,
    )
    .await
}
