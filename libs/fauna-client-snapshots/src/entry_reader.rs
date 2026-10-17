//! The snapshot **entry reader** — the ONE reader every snapshot door drives,
//! the full restore and the per-file download alike, on all 7 apps
//! (`backup-restore.md` § Security Properties → Integrity, ruled 2026-10-13).
//!
//! A snapshot entry's `manifest_hash` is the home nest's unsigned word, and a
//! seal licenses nothing by itself: it proves the owner sealed the bytes under
//! the set's root, never that they are *this path's at this version* — so the
//! nest could list one owner-sealed manifest of the owner's in place of
//! another, and a plaintext one it built itself. What licenses an entry,
//! sealed or plaintext, is what licenses it for the sync engine: the **judged
//! change row** of the snapshot's set that named exactly this manifest under
//! the hash of the path the entry restores to. The reader resolves that row
//! from the signed version history (`fauna.files.versions.list`, soft-pruned
//! and purged rows included — the version-retention purge stamps a snapshot's
//! head row superseded while the snapshot still pins its bytes, and the table
//! is append-only, so an honest nest always holds the row), judges every row
//! through the one shared judge (writer-signed change records, ruling (3))
//! and picks by [`JudgedCandidate::find`]'s rule (ruling (10)(a)): an admitted
//! or history version carrying this manifest under this path. Nothing else
//! licenses — not the seal, not the folder's owner-attested audience (it
//! attests the audience *now*; a snapshot is history), not the nest's listing.
//!
//! The licence is read BEFORE any byte: an entry the history does not name is
//! refused with the typed `ENTRY_UNLICENSED` and its manifest is never
//! fetched; a history the nest withholds (the read fails) refuses the entry
//! too, transiently — a snapshot whose rows the nest withholds is refused,
//! never opened on the seal alone. The licensed open then runs under the
//! row's signed facts: the seal requirement lifted (the writer vouched for
//! that manifest, sealed or not), the row's stamp (ruling (10)(c)) and its
//! signer bound exactly as the engine binds a record it verified (ruling
//! (8)(c)).

use core::fmt::Display;

use fauna_client_sync::restore_branch::JudgedCandidate;
use fauna_client_sync::row_judge::{ReaderSeat, verified_signer};
use fauna_client_sync::{SyncClient, VersionsListing};
use fauna_core::apply_failure::{PermanentApplyFailure, permanent};
use fauna_core::data::ContentHash;
use fauna_core::file_download::{
    BlobFetcher, FileDownloadKeys, RecordSigner, download_file_bytes_by_manifest,
};
use fauna_protocol::filesync::SnapshotGetReply;
use fauna_protocol::{RpcErrorClass, RpcRequester};

/// The set a snapshot was taken of, as its listing named it
/// (`SnapshotGetReply::{folder, folder_hash}`) — the address the licence read
/// asks the version history by. The wire hash rides beside the name so a set
/// whose plaintext name rests scrubbed still addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotSet {
    pub folder: String,
    pub folder_hash: Option<Vec<u8>>,
}

impl SnapshotSet {
    /// The set a snapshot reply names.
    pub fn of(reply: &SnapshotGetReply) -> Self {
        Self {
            folder: reply.folder.clone(),
            folder_hash: reply.folder_hash.as_ref().map(|b| b.to_vec()),
        }
    }

    /// The set as a backups-machine detail names it: the name and the wire
    /// hash in hex (`SnapshotDetail::{folder, folder_hash}`). A hash that is
    /// not 32 hex bytes is dropped, so the read addresses by name alone.
    pub fn from_named(folder: &str, folder_hash_hex: Option<&str>) -> Self {
        Self {
            folder: folder.to_string(),
            folder_hash: folder_hash_hex
                .and_then(|h| fauna_core::hex32::decode(h).ok())
                .map(|a| a.to_vec()),
        }
    }
}

/// What licensed one entry: the judged row's signed facts that the licensed
/// open carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryLicence {
    /// The identity the row verified as signed by; `None` for an exempt
    /// (nest-minted class) row.
    pub signed_as: Option<[u8; 32]>,
    /// Whether an admitted row naming this manifest was signed as the reader's
    /// own identity — what alone licenses the current owner root for a sealed
    /// open (writer-signed change records, ruling (8)(c)).
    pub signed_as_own: bool,
    /// The row's signed generation stamp (ruling (10)(c): a stamped record
    /// opens under its generation and nothing else) — the ONE stamp a
    /// snapshot entry opens under; the listing carries none the reader
    /// honours.
    pub content_key_version: Option<u64>,
    /// The licensing row's `seq`.
    pub version_num: i64,
}

/// A reader of one snapshot's entries: the typed sync surface the licence
/// read goes through, the seat that judges what it lists, and the set the
/// snapshot names.
pub struct SnapshotEntryReader<'a, R: RpcRequester> {
    sync: &'a SyncClient<R>,
    seat: &'a ReaderSeat,
    set: &'a SnapshotSet,
}

impl<'a, R> SnapshotEntryReader<'a, R>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + Display,
{
    pub fn new(sync: &'a SyncClient<R>, seat: &'a ReaderSeat, set: &'a SnapshotSet) -> Self {
        Self { sync, seat, set }
    }

    /// The licence for `manifest` restored to `relative_path`: the judged row
    /// of this set naming exactly that manifest under the hash of that path,
    /// or `None` when the history holds no such row — the nest built the
    /// manifest, or listed another path's or version's, and either way
    /// nothing licenses the bytes. An `Err` is a history the nest did not
    /// serve.
    ///
    /// The path hash is derived from the path the entry will be written to,
    /// never read off the listing: a version's signature covers its own
    /// `path_hash`, so a row served under another path names another path.
    pub async fn licence(
        &self,
        relative_path: &str,
        manifest: &ContentHash,
    ) -> Result<Option<EntryLicence>, R::Error> {
        let path_hash = fauna_core::sync::path_hash(relative_path);
        let listing = VersionsListing {
            folder: Some(self.set.folder.clone()),
            name_hash: self.set.folder_hash.clone(),
            row: None,
            include_pruned: true,
            include_superseded: true,
        };
        let judged = self
            .sync
            .versions_list_judged_at(path_hash, &listing, self.seat)
            .await?;
        Ok(JudgedCandidate::find(
            judged,
            path_hash,
            &fauna_core::hex32::encode(&manifest.digest()),
            self.seat.own.as_ref(),
        )
        .map(|candidate| EntryLicence {
            signed_as: verified_signer(&candidate.verdict),
            signed_as_own: candidate.current_vouches,
            content_key_version: candidate.version.content_key_version,
            version_num: candidate.version.version_num,
        }))
    }

    /// `keys` for one licensed open: the seal requirement lifted — the row
    /// vouched for exactly this manifest, sealed or plaintext — and the row's
    /// signer bound to the open exactly as the engine binds a record it
    /// verified (ruling (8)(c)): the current identity, a proven predecessor
    /// of this seat, or another writer, offered the owner family's roots
    /// accordingly.
    pub fn licensed_keys(
        &self,
        keys: &FileDownloadKeys,
        licence: &EntryLicence,
    ) -> FileDownloadKeys {
        let record_signer = match licence.signed_as {
            Some(id) if Some(id) == self.seat.own => RecordSigner::Current,
            Some(id)
                if self
                    .seat
                    .learned
                    .chain_with(&self.seat.predecessors)
                    .contains(&id) =>
            {
                RecordSigner::Predecessor(fauna_core::identity::ActorId(id))
            }
            _ => RecordSigner::Other,
        };
        FileDownloadKeys {
            sealed_only: false,
            owner_signed_record: licence.signed_as_own,
            record_signer,
            ..keys.clone()
        }
    }

    /// One entry's bytes: licensed by [`Self::licence`] first, then read
    /// under [`Self::licensed_keys`] and the licensing row's stamp. An entry
    /// no judged row names fails with the typed `ENTRY_UNLICENSED` before
    /// its manifest is fetched, so a walk names it among its unverified
    /// files; a history read that fails refuses the entry too, as a transient
    /// error a re-run retries. A sealed and a plaintext entry take the same
    /// road: the row is the licence, the seal never is.
    pub async fn download_bytes(
        &self,
        fetcher: &dyn BlobFetcher,
        keys: &FileDownloadKeys,
        relative_path: &str,
        manifest: ContentHash,
    ) -> anyhow::Result<Vec<u8>> {
        let licence = self.licence(relative_path, &manifest).await.map_err(|e| {
            anyhow::anyhow!("reading the version history for a snapshot entry's licence: {e}")
        })?;
        let Some(licence) = licence else {
            return Err(permanent(
                PermanentApplyFailure::ENTRY_UNLICENSED,
                "no judged change row of this set names this manifest under this path, so \
                 nothing licenses the entry, sealed or not (backup-restore.md § Security \
                 Properties → Integrity)",
            ));
        };
        download_file_bytes_by_manifest(
            fetcher,
            &self.licensed_keys(keys, &licence),
            manifest,
            licence.content_key_version,
            relative_path,
        )
        .await
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use fauna_client_sync::SetNonceSource;
    use fauna_client_testkit::{RejectingRequester, block_on};
    use fauna_core::apply_failure::permanent_reason;
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
            &EntryLicence {
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
            &EntryLicence {
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
