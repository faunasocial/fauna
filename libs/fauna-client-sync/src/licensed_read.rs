//! The **licensed manifest read** — the ONE reader every owner-only door
//! drives whose `(path, manifest)` addresses are a party's unsigned word: the
//! snapshot entry reader (`fauna_client_snapshots::entry_reader`, every
//! snapshot door on all 7 apps) and the archive import's reads of its own
//! archive folder (`fauna_archive_import_machine::rpc_glue`)
//! (`backup-restore.md` § Security Properties → Integrity;
//! `archive-import.md` § Storage).
//!
//! A listed `manifest_hash` — a snapshot entry's, a head projection's — is the
//! home nest's unsigned word, and a seal licenses nothing by itself: it proves
//! the owner sealed the bytes under the set's root, never that they are *this
//! path's at this version* — so the nest could list one owner-sealed manifest
//! of the owner's in place of another, and a plaintext one it built itself.
//! What licenses a read, sealed or plaintext, is what licenses it for the sync
//! engine: the **judged change row** of the set that named exactly this
//! manifest under the hash of the path being read. The reader resolves that
//! row from the signed version history (`fauna.files.versions.list`,
//! soft-pruned and purged rows included — the version-retention purge stamps a
//! snapshot's head row superseded while the snapshot still pins its bytes, and
//! the table is append-only, so an honest nest always holds the row), judges
//! every row through the one shared judge (writer-signed change records,
//! ruling (3)) and picks by [`JudgedCandidate::find`]'s rule (ruling (10)(a)):
//! an admitted or history version carrying this manifest under this path.
//! Nothing else licenses — not the seal, not the folder's owner-attested
//! audience, not the nest's listing.
//!
//! The licence is read BEFORE any byte: a read the history does not name is
//! refused with the typed `ENTRY_UNLICENSED` and its manifest is never
//! fetched; a history the nest withholds (the read fails) refuses the read
//! too, transiently — never an open on the seal alone. The licensed open then
//! runs under the row's signed facts: the seal requirement lifted (the writer
//! vouched for that manifest, sealed or not — a door whose set is never
//! plaintext re-applies it), the row's stamp (ruling (10)(c)) and its signer
//! bound exactly as the engine binds a record it verified (ruling (8)(c)).

use core::fmt::Display;

use crate::restore_branch::JudgedCandidate;
use crate::row_judge::{ReaderSeat, verified_signer};
use crate::{SyncClient, VersionsListing};
use fauna_core::apply_failure::{PermanentApplyFailure, permanent};
use fauna_core::data::ContentHash;
use fauna_core::file_download::{
    BlobFetcher, FileDownloadKeys, RecordSigner, download_file_bytes_by_manifest,
};
use fauna_protocol::filesync::SnapshotGetReply;
use fauna_protocol::{RpcErrorClass, RpcRequester};

/// The set a licensed read asks the version history by: its name and, when
/// the caller has it, its wire hash — riding beside the name so a set whose
/// plaintext name rests scrubbed still addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetAddress {
    pub folder: String,
    pub folder_hash: Option<Vec<u8>>,
}

impl SetAddress {
    /// The set a snapshot reply names (`SnapshotGetReply::{folder,
    /// folder_hash}`).
    pub fn of(reply: &SnapshotGetReply) -> Self {
        Self {
            folder: reply.folder.clone(),
            folder_hash: reply.folder_hash.as_ref().map(|b| b.to_vec()),
        }
    }

    /// The set by name and, optionally, its wire hash in hex (a
    /// backups-machine `SnapshotDetail::{folder, folder_hash}`). A hash that
    /// is not 32 hex bytes is dropped, so the read addresses by name alone.
    pub fn from_named(folder: &str, folder_hash_hex: Option<&str>) -> Self {
        Self {
            folder: folder.to_string(),
            folder_hash: folder_hash_hex
                .and_then(|h| fauna_core::hex32::decode(h).ok())
                .map(|a| a.to_vec()),
        }
    }
}

/// What licensed one read: the judged row's signed facts that the licensed
/// open carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestLicence {
    /// The identity the row verified as signed by; `None` for an exempt
    /// (nest-minted class) row.
    pub signed_as: Option<[u8; 32]>,
    /// Whether an admitted row naming this manifest was signed as the reader's
    /// own identity — what alone licenses the current owner root for a sealed
    /// open (writer-signed change records, ruling (8)(c)).
    pub signed_as_own: bool,
    /// The row's signed generation stamp (ruling (10)(c): a stamped record
    /// opens under its generation and nothing else) — the ONE stamp a
    /// licensed read opens under; a listing carries none the reader honours.
    pub content_key_version: Option<u64>,
    /// The licensing row's `seq`.
    pub version_num: i64,
}

/// A licensed reader over one set: the typed sync surface the licence read
/// goes through, the seat that judges what it lists, and the set it reads.
pub struct LicensedManifestReader<'a, R: RpcRequester> {
    sync: &'a SyncClient<R>,
    seat: &'a ReaderSeat,
    set: &'a SetAddress,
}

impl<'a, R> LicensedManifestReader<'a, R>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + Display,
{
    pub fn new(sync: &'a SyncClient<R>, seat: &'a ReaderSeat, set: &'a SetAddress) -> Self {
        Self { sync, seat, set }
    }

    /// The licence for `manifest` at `relative_path`: the judged row of this
    /// set naming exactly that manifest under the hash of that path, or
    /// `None` when the history holds no such row — the nest built the
    /// manifest, or listed another path's or version's, and either way
    /// nothing licenses the bytes. An `Err` is a history the nest did not
    /// serve.
    ///
    /// The path hash is derived from the path being read, never read off a
    /// listing: a version's signature covers its own `path_hash`, so a row
    /// served under another path names another path.
    pub async fn licence(
        &self,
        relative_path: &str,
        manifest: &ContentHash,
    ) -> Result<Option<ManifestLicence>, R::Error> {
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
        .map(|candidate| ManifestLicence {
            signed_as: verified_signer(&candidate.verdict),
            signed_as_own: candidate.current_vouches,
            content_key_version: candidate.version.content_key_version,
            version_num: candidate.version.version_num,
        }))
    }

    /// [`Self::licence`] as a read's gate: the licence, or the typed refusal
    /// a walk names among its unverified files — the permanent
    /// `ENTRY_UNLICENSED` when no judged row names the manifest under the
    /// path, a transient error when the nest withheld the history. Never a
    /// fallback to the listing's word.
    pub async fn require_licence(
        &self,
        relative_path: &str,
        manifest: &ContentHash,
    ) -> anyhow::Result<ManifestLicence> {
        let licence = self.licence(relative_path, manifest).await.map_err(|e| {
            anyhow::anyhow!("reading the version history for a read's licence: {e}")
        })?;
        licence.ok_or_else(|| {
            permanent(
                PermanentApplyFailure::ENTRY_UNLICENSED,
                "no judged change row of this set names this manifest under this path, so \
                 nothing licenses the read, sealed or not (backup-restore.md § Security \
                 Properties → Integrity)",
            )
        })
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
        licence: &ManifestLicence,
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

    /// One file's bytes: licensed by [`Self::require_licence`] first, then
    /// read under [`Self::licensed_keys`] and the licensing row's stamp. A
    /// sealed and a plaintext file take the same road: the row is the
    /// licence, the seal never is.
    pub async fn download_bytes(
        &self,
        fetcher: &dyn BlobFetcher,
        keys: &FileDownloadKeys,
        relative_path: &str,
        manifest: ContentHash,
    ) -> anyhow::Result<Vec<u8>> {
        let licence = self.require_licence(relative_path, &manifest).await?;
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
