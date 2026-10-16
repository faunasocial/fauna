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

use std::sync::Arc;

use fauna_client_snapshots::{SnapshotEntryReader, SnapshotSet};
use fauna_client_sync::row_judge::ReaderSeat;
use fauna_client_sync::{SetNonceSource, SyncClient};
use fauna_core::data::ContentHash;
use fauna_core::file_download::{FileDownloadKeys, WasmPublicChunkFetcher};
use fauna_core::identity::ActorKeypair;

/// Download one file's **decrypted** bytes out of a snapshot — the web leg of
/// `snapshot-file-download-button`, exported as
/// `WsRpcClient::downloadSnapshotFileBytes` (`rpc.rs`), since the licence a
/// plaintext entry restores under is read over the WS-RPC control plane.
///
/// `manifest_hash` is the file entry's `manifest_hash` from
/// `fauna.filesync.snapshot.get` (raw 32-byte digest, as it rides the wire);
/// `relative_path` is that entry's `path`; `secret_hex` is the 64-char hex
/// Ed25519 identity seed the owner `BackupKey` derives from (the same argument
/// `process_and_seal_library` takes); `folder` / `folder_hash_hex` name the set
/// the snapshot was taken of, as the Backups machine's detail carries them.
/// The SPA saves the returned bytes as a browser blob download.
///
/// The read is the ONE snapshot entry reader every app drives
/// (`fauna_client_snapshots::entry_reader`; `backup-restore.md` § Security
/// Properties → Integrity): the manifest hash is the home nest's unsigned
/// snapshot word and its seal is not a licence, so the entry — sealed or
/// plaintext — opens under the derived `BackupKey` only once the judged
/// change row of the set that named exactly it under this path licenses it,
/// judged as this seat under the custody the account runtime holds.
pub(crate) async fn download_snapshot_file_bytes(
    client: &fauna_rpc_wasm::WsRpcClient,
    manifest_hash: &[u8],
    relative_path: &str,
    secret_hex: &str,
    nest_url: &str,
    folder: &str,
    folder_hash_hex: Option<&str>,
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
    let keypair = ActorKeypair::from_secret(seed);

    let fetcher = WasmPublicChunkFetcher::new(nest_url.trim_end_matches('/'));
    let keys = FileDownloadKeys::owner(fauna_core::crypto::BackupKey::derive(&seed));
    // The seat the licence read is judged as: this identity, each set's nonce
    // through the same resolver-backed custody every other wasm reader uses.
    let resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver> =
        Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
            client.clone(),
            crate::account_runtime::folder_key_store(keypair.actor_id_hex()),
        ));
    let seat = ReaderSeat {
        own: Some(keypair.actor_id().0),
        nonces: Some(SetNonceSource::Resolver(resolver)),
        ..Default::default()
    };
    let sync = SyncClient::new(client.clone());
    let set = SnapshotSet::from_named(folder, folder_hash_hex);
    SnapshotEntryReader::new(&sync, &seat, &set)
        .download_bytes(
            &fetcher,
            &keys,
            relative_path,
            ContentHash::from_digest_raw(digest),
        )
        .await
}
