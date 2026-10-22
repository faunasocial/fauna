//! The snapshot **entry reader** — the ONE reader every snapshot door drives,
//! the full restore and the per-file download alike, on all 7 apps
//! (`backup-restore.md` § Security Properties → Integrity, ruled 2026-10-13).
//!
//! A snapshot entry's `manifest_hash` is the home nest's unsigned word, and a
//! seal licenses nothing by itself, so every entry is read through the shared
//! **licensed manifest read** ([`fauna_client_sync::licensed_read`]): the
//! judged change row of the snapshot's set that named exactly this manifest
//! under the hash of the path the entry restores to is the licence, read
//! BEFORE any byte; an entry no row names is refused unfetched with the typed
//! `ENTRY_UNLICENSED`, a withheld history refuses it transiently. The archive
//! import's folder reads go through the same reader — one licence, never a
//! copy. Nothing else licenses a snapshot entry — not the seal, not the
//! folder's owner-attested audience (it attests the audience *now*; a
//! snapshot is history), not the nest's listing.

/// The set a snapshot was taken of, as its listing named it
/// ([`SetAddress::of`] for a `SnapshotGetReply`, [`SetAddress::from_named`]
/// for a backups-machine detail) — the address the licence read asks the
/// version history by.
pub use fauna_client_sync::licensed_read::SetAddress as SnapshotSet;

/// The reader of one snapshot's entries: the shared licensed read over the
/// snapshot's set.
pub use fauna_client_sync::licensed_read::LicensedManifestReader as SnapshotEntryReader;

pub use fauna_client_sync::licensed_read::{ManifestLicence, SetAddress};

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use fauna_client_sync::row_judge::ReaderSeat;
    use fauna_client_sync::{SetNonceSource, SyncClient};
    use fauna_client_testkit::{RejectingRequester, block_on};
    use fauna_core::apply_failure::PermanentApplyFailure;
    use fauna_core::apply_failure::permanent_reason;
    use fauna_core::data::ContentHash;
    use fauna_core::file_download::{BlobFetcher, FileDownloadKeys, RecordSigner};
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::files::{FileVersionInfo, FilesVersionsListReply};
    use fauna_protocol::folders::{FolderSummary, FoldersListReply, KIND_FOLDERS_LIST};
    use fauna_protocol::sync::SyncChange;
    use fauna_protocol::sync_writer_sig::ChangeSigner;

    const KIND_VERSIONS_LIST: &str = "fauna.files.versions.list";
    const NONCE: [u8; 32] = [7; 32];
    const OTHER_NONCE: [u8; 32] = [9; 32];
    const PATH: &str = "site/index.html";
    const OTHER_PATH: &str = "elsewhere/other.html";

    fn owner() -> ActorKeypair {
        ActorKeypair::from_secret([0x51; 32])
    }

    fn owner_key() -> fauna_core::crypto::BackupKey {
        fauna_core::crypto::BackupKey::derive(&[0x52; 32])
    }

    /// A blob store in a `HashMap` — the walk end to end with no nest, the
    /// shape it runs in on wasm — counting every manifest asked for, so a
    /// test can pin that a refused entry cost no byte.
    #[derive(Default)]
    struct MapFetcher {
        blobs: Mutex<HashMap<[u8; 32], Vec<u8>>>,
        manifests_fetched: Mutex<usize>,
    }

    impl MapFetcher {
        fn stock(&self, blob: &fauna_core::blob_seal::SealedBlob) {
            let mut blobs = self.blobs.lock().unwrap();
            blobs.insert(blob.manifest_hash.digest(), blob.manifest_bytes.clone());
            for (key, body) in &blob.chunks {
                blobs.insert(key.digest(), body.clone());
            }
        }

        fn get(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
            self.blobs
                .lock()
                .unwrap()
                .get(&hash.digest())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404"))
        }

        fn manifests_fetched(&self) -> usize {
            *self.manifests_fetched.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl BlobFetcher for MapFetcher {
        async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
            *self.manifests_fetched.lock().unwrap() += 1;
            self.get(hash)
        }

        async fn fetch_chunks(
            &self,
            store_keys: &[ContentHash],
            _path: &str,
        ) -> anyhow::Result<Vec<Vec<u8>>> {
            store_keys.iter().map(|k| self.get(k)).collect()
        }
    }

    fn plaintext_blob(bytes: &[u8]) -> fauna_core::blob_seal::SealedBlob {
        let blob = fauna_core::blob_seal::seal_blob(bytes, None).unwrap();
        assert!(blob.manifest.stored_hashes.is_none(), "a plaintext blob");
        blob
    }

    /// `bytes` sealed under the owner's root — as a private set's file rests.
    fn sealed_blob(bytes: &[u8]) -> fauna_core::blob_seal::SealedBlob {
        let blob = fauna_core::blob_seal::seal_blob(
            bytes,
            Some((owner_key().convergent_chunk_root(), None)),
        )
        .unwrap();
        assert!(blob.manifest.stored_hashes.is_some(), "a sealed blob");
        blob
    }

    /// A row of `author`'s naming `manifest` under `path`, signed under
    /// `nonce` — as the version-history projection serves it.
    fn version(
        author: &ActorKeypair,
        path: &str,
        manifest: &ContentHash,
        nonce: [u8; 32],
    ) -> FileVersionInfo {
        let mut row = SyncChange {
            seq: 3,
            path_hash: fauna_core::hex32::encode(&fauna_core::sync::path_hash(path)),
            manifest_hash: Some(fauna_core::hex32::encode(&manifest.digest())),
            size_bytes: 10,
            change_type: "create".into(),
            created_at: 1_000,
            device_id: Some(fauna_core::hex32::encode(&[4; 32])),
            author_actor_id: Some(author.actor_id().to_hex()),
            ..Default::default()
        };
        ChangeSigner::direct(author)
            .sign_row(&mut row, nonce)
            .unwrap();
        FileVersionInfo {
            path_hash: fauna_core::sync::path_hash(path).to_vec().into(),
            version_num: row.seq,
            manifest_hash: manifest.digest().to_vec().into(),
            size_bytes: row.size_bytes,
            created_at: row.created_at,
            folder: Some("site".into()),
            author_actor_id: author.actor_id().to_hex(),
            device_id: Some([4; 32].to_vec().into()),
            change_type: Some("create".into()),
            signature: row.signature.clone(),
            signer_key: row.signer_key.clone(),
            ..Default::default()
        }
    }

    fn folders_list() -> FoldersListReply {
        FoldersListReply {
            folders: vec![FolderSummary {
                name: "site".into(),
                role: Some("owner".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn nest(versions: Vec<FileVersionInfo>) -> RejectingRequester {
        RejectingRequester::new()
            .reply(KIND_FOLDERS_LIST, &folders_list())
            .reply(
                KIND_VERSIONS_LIST,
                &FilesVersionsListReply {
                    versions,
                    ..Default::default()
                },
            )
    }

    fn seat() -> ReaderSeat {
        ReaderSeat {
            own: Some(owner().actor_id().0),
            nonces: Some(SetNonceSource::by_folder(
                [("site".to_string(), NONCE)].into_iter().collect(),
            )),
            ..Default::default()
        }
    }

    fn set() -> SnapshotSet {
        SnapshotSet {
            folder: "site".into(),
            folder_hash: None,
        }
    }

    fn read(
        nest: &RejectingRequester,
        fetcher: &MapFetcher,
        manifest: ContentHash,
    ) -> anyhow::Result<Vec<u8>> {
        let sync = SyncClient::new(nest);
        let seat = seat();
        let set = set();
        let reader = SnapshotEntryReader::new(&sync, &seat, &set);
        block_on(reader.download_bytes(
            fetcher,
            &FileDownloadKeys::owner(owner_key()),
            PATH,
            manifest,
        ))
    }

    fn is_unlicensed(e: &anyhow::Error) -> bool {
        permanent_reason(e) == Some(PermanentApplyFailure::ENTRY_UNLICENSED.reason)
    }

    /// The licence: an admitted row of the set naming exactly this manifest
    /// under this path restores a plaintext entry — one history read judged
    /// over the folder list, then the open.
    #[test]
    fn a_plaintext_entry_is_licensed_by_the_admitted_row_naming_it() {
        let blob = plaintext_blob(b"<html>public era</html>");
        let fetcher = MapFetcher::default();
        fetcher.stock(&blob);
        let nest = nest(vec![version(&owner(), PATH, &blob.manifest_hash, NONCE)]);

        let bytes = read(&nest, &fetcher, blob.manifest_hash).expect("licensed");
        assert_eq!(bytes, b"<html>public era</html>");
        assert_eq!(nest.kinds(), [KIND_VERSIONS_LIST, KIND_FOLDERS_LIST]);
    }

    /// The same licence, the same road, for a SEALED entry: the row is read
    /// first, and the open under the owner's root follows it.
    #[test]
    fn a_sealed_entry_is_licensed_by_the_admitted_row_naming_it() {
        let blob = sealed_blob(b"sealed under the owner's root");
        let fetcher = MapFetcher::default();
        fetcher.stock(&blob);
        let nest = nest(vec![version(&owner(), PATH, &blob.manifest_hash, NONCE)]);

        let bytes = read(&nest, &fetcher, blob.manifest_hash).expect("licensed");
        assert_eq!(bytes, b"sealed under the owner's root");
        assert_eq!(nest.kinds(), [KIND_VERSIONS_LIST, KIND_FOLDERS_LIST]);
        assert_eq!(fetcher.manifests_fetched(), 1);
    }

    /// No row at all: the entry is refused, typed, before any byte is asked
    /// for — a walk names it unverified.
    #[test]
    fn an_entry_no_row_names_is_refused_before_any_fetch() {
        let blob = plaintext_blob(b"bytes the nest made up");
        let fetcher = MapFetcher::default();
        fetcher.stock(&blob);
        let nest = nest(vec![]);

        let err = read(&nest, &fetcher, blob.manifest_hash).expect_err("refused");
        assert!(is_unlicensed(&err), "{err:#}");
        assert_eq!(nest.kinds(), [KIND_VERSIONS_LIST]);
        assert_eq!(
            fetcher.manifests_fetched(),
            0,
            "no byte for an unlicensed entry"
        );
    }

    /// The seal is not a licence: an owner-sealed manifest the nest lists in
    /// place of another — here an older version of the path, one no row
    /// names any more than the nest's own word — is refused unopened.
    #[test]
    fn a_sealed_entry_no_row_names_is_refused_unopened() {
        let current = sealed_blob(b"the version the owner wrote last");
        let older = sealed_blob(b"the version the nest would rather restore");
        let fetcher = MapFetcher::default();
        fetcher.stock(&older);
        let nest = nest(vec![version(&owner(), PATH, &current.manifest_hash, NONCE)]);

        let err = read(&nest, &fetcher, older.manifest_hash).expect_err("refused");
        assert!(is_unlicensed(&err), "{err:#}");
        assert_eq!(fetcher.manifests_fetched(), 0);
    }

    /// A row of the path naming another manifest licenses nothing: the nest
    /// cannot launder a forged manifest under a genuine row's path.
    #[test]
    fn a_row_naming_another_manifest_licenses_nothing() {
        let genuine = plaintext_blob(b"the owner's own page");
        let forged = plaintext_blob(b"the nest's substitute");
        let fetcher = MapFetcher::default();
        fetcher.stock(&forged);
        let nest = nest(vec![version(&owner(), PATH, &genuine.manifest_hash, NONCE)]);

        let err = read(&nest, &fetcher, forged.manifest_hash).expect_err("refused");
        assert!(is_unlicensed(&err), "{err:#}");
    }

    /// A row naming this manifest under ANOTHER path licenses nothing: the
    /// hash the licence is looked up under is the restore path's own, so a
    /// row the nest serves under a foreign path still names its own path —
    /// and a sealed file of the owner's cannot be moved by a lying listing.
    #[test]
    fn a_row_under_another_path_licenses_nothing() {
        let blob = sealed_blob(b"moved by a lying listing");
        let fetcher = MapFetcher::default();
        fetcher.stock(&blob);
        let nest = nest(vec![version(
            &owner(),
            OTHER_PATH,
            &blob.manifest_hash,
            NONCE,
        )]);

        let err = read(&nest, &fetcher, blob.manifest_hash).expect_err("refused");
        assert!(is_unlicensed(&err), "{err:#}");
        assert_eq!(fetcher.manifests_fetched(), 0);
    }

    /// A row that does not verify is not a row (ruling (3)): signed under
    /// another set's nonce, it licenses nothing here.
    #[test]
    fn a_row_that_does_not_verify_licenses_nothing() {
        let blob = plaintext_blob(b"copied from another set");
        let fetcher = MapFetcher::default();
        fetcher.stock(&blob);
        let nest = nest(vec![version(
            &owner(),
            PATH,
            &blob.manifest_hash,
            OTHER_NONCE,
        )]);

        let err = read(&nest, &fetcher, blob.manifest_hash).expect_err("refused");
        assert!(is_unlicensed(&err), "{err:#}");
    }

    /// A history the nest withholds refuses the entry — sealed or not — as a
    /// transient failure a re-run retries, never a permanent one and never
    /// an open on the seal alone.
    #[test]
    fn a_history_the_nest_withholds_refuses_the_entry_unopened() {
        let blob = sealed_blob(b"sealed, and the nest serves no history");
        let fetcher = MapFetcher::default();
        fetcher.stock(&blob);
        let nest = RejectingRequester::new().reply(KIND_FOLDERS_LIST, &folders_list());

        let err = read(&nest, &fetcher, blob.manifest_hash).expect_err("refused");
        assert!(permanent_reason(&err).is_none(), "transient: {err:#}");
        assert_eq!(nest.kinds(), [KIND_VERSIONS_LIST]);
        assert_eq!(fetcher.manifests_fetched(), 0);
    }

    /// The licensed open binds the row's signer: the owner's own row is the
    /// current identity, another writer's is `Other`, and the seal requirement
    /// is lifted for exactly that open.
    #[test]
    fn licensed_keys_bind_the_rows_signer() {
        let nest = nest(vec![]);
        let sync = SyncClient::new(&nest);
        let seat = seat();
        let set = set();
        let reader = SnapshotEntryReader::new(&sync, &seat, &set);
        let keys = FileDownloadKeys::owner(owner_key()).sealed_only();

        let own = reader.licensed_keys(
            &keys,
            &ManifestLicence {
                signed_as: Some(owner().actor_id().0),
                signed_as_own: true,
                content_key_version: None,
                version_num: 3,
            },
        );
        assert!(!own.sealed_only);
        assert!(own.owner_signed_record);
        assert_eq!(own.record_signer, RecordSigner::Current);

        let stranger = reader.licensed_keys(
            &keys,
            &ManifestLicence {
                signed_as: Some([0xEE; 32]),
                signed_as_own: false,
                content_key_version: Some(2),
                version_num: 4,
            },
        );
        assert!(!stranger.sealed_only);
        assert!(!stranger.owner_signed_record);
        assert_eq!(stranger.record_signer, RecordSigner::Other);
    }
}
