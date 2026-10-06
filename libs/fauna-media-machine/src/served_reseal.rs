//! **The flipping client's served-set walk** — `webdav-server.md` § Key model,
//! the app-side seams bullet, part (c): right after the app serves a set, it
//! converges the set's nest-resident heads onto the served content key itself,
//! so a file no driven engine holds — every file of an owner whose apps run no
//! sync agent, and any file a Media page recorded before the flip — opens
//! through the mount.
//!
//! **Why here.** The walk moves exactly the two at-rest shapes this crate
//! already reads and writes: a Media blob primary (a pre-serve upload sealed
//! under the owner's bare `BackupKey`, [`MediaMachine::download_file`]'s blob
//! arm) and an owner-keyed chunk manifest (its walk arm). The byte seams it
//! needs — the blob-store GET, the chunk/manifest GETs, the chunk/manifest
//! POSTs — and the signed record through the seat's own writer key are the
//! Media seam's, on both targets, so nothing is re-bound per app.
//!
//! **Scope: a served, group-less set only.** A set bound to a sharing group is
//! the M2 pre-bind pass's (`mls-group-key-material.md` § M2 → *Pre-bind re-seal
//! migration* (C)/(D)), run where the owner's engine lives; this walk refuses
//! it rather than become a second driver of that pass.
//!
//! **Per head, in the engine walk's own order** (chunks → manifest → record →
//! verify → supersede — the verify here runs before the record, see
//! [`ServedSetWalk::reseal_head`]):
//!
//! - **Which heads.** The set's change log folded latest-per-path, a head owed
//!   when its stamp is not the set's current generation.
//! - **Which heads may be opened.** Every head is judged by the one shared
//!   verified-row judge first (`mls-group-key-material.md` § M2 → *Writer-signed
//!   change records*, ruling (3)); a row that does not verify is skipped,
//!   warned and counted, never re-sealed. An **unstamped** head opens under the
//!   owner root only when it verified as this account's own signed record of
//!   this set (ruling (5), the (D) enabler —
//!   [`FileDownloadKeys::owner_signed_record`]): the signature under the set's
//!   nonce is what tells this set's pre-serve file from one a nest steered in
//!   from another of the owner's sets. Anything less — unsigned, no nonce,
//!   another writer — is skipped, since the owner root would open it and the
//!   re-seal would publish it to the mount.
//! - **The byte move.** A blob primary is fetched, address-checked, opened and
//!   re-sealed whole through [`fauna_core::blob_seal::seal_blob`]; a manifest is
//!   re-sealed a window at a time through the shared
//!   [`fauna_core::nest_reseal::reseal_nest_copy_windowed`], the same function
//!   the engine's nest-bytes source runs. Either way the artifacts are
//!   byte-identical to what the engine, the MDA and a content-keyed Media upload
//!   produce for the same bytes — so the agent's own pass, where one runs too,
//!   converges on the same store keys: dedup, never conflict.
//!
//! Idempotent under the deterministic seal: a crash mid-walk leaves the heads
//! it did not record unstamped, and the next run re-drives them (the chunks it
//! already stored dedup).

use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_client_sync::SyncClient;
use fauna_client_sync::row_judge::{ProjectedRow, ReaderSeat};
use fauna_core::crypto::{BackupKey, decrypt_backup_chunk};
use fauna_core::data::ContentHash;
use fauna_core::file_download::{BlobFetcher, FileDownloadKeys, RecordSigner};
use fauna_core::folder_keys::FolderKeyResolver;
use fauna_core::nest_reseal::{ChunkStoreSink, ServedSetConvergence};
use fauna_protocol::sync::SyncChange;
use fauna_protocol::sync_row_verify::RowVerdict;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::blob_fetcher::MediaBlobFetcher;
use crate::blob_uploader::{MediaBlobUploader, UploaderSink};
use crate::nest_api::MediaApiError;

/// One served set's walk, over one seat's session: the change log and the
/// signed record through `sync`, the verified-row judge through `nest`, the
/// set's custody through `resolver`, the bytes through the three Media byte
/// seams. Built per target by the `served_set_walk` builders below.
pub struct ServedSetWalk<R: RpcRequester> {
    nest: R,
    /// Signs every record it writes (the seat's own writer key, each record's
    /// nonce through `resolver`) — birth-shape signing, ruling (4).
    sync: SyncClient<R>,
    /// Who reads: this seat's actor id + the nonce source the judge verifies
    /// the set's rows under.
    seat: ReaderSeat,
    resolver: Arc<dyn FolderKeyResolver>,
    /// The owner root a pre-serve file rests under — read only, and only for a
    /// head that verified as this account's own.
    owner: BackupKey,
    /// The recording device (the same one the app's Media gestures record
    /// under).
    device_id: String,
    fetcher: Arc<dyn BlobFetcher>,
    primaries: Arc<dyn MediaBlobFetcher>,
    uploader: Arc<dyn MediaBlobUploader>,
}

/// One owed head, judged and placed.
struct OwedHead {
    row: SyncChange,
    path: String,
    manifest_hash: ContentHash,
    /// Verified as this account's own signed record of this set — the (D)
    /// enabler's per-record fact.
    owner_signed: bool,
}

impl<R> ServedSetWalk<R>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    /// `keypair` is the seat's identity: it signs the records, names the seat
    /// to the judge, and derives the owner root. `resolver` answers the set's
    /// custody and nonce.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        nest: R,
        keypair: &fauna_core::identity::ActorKeypair,
        resolver: Arc<dyn FolderKeyResolver>,
        device_id: String,
        fetcher: Arc<dyn BlobFetcher>,
        primaries: Arc<dyn MediaBlobFetcher>,
        uploader: Arc<dyn MediaBlobUploader>,
    ) -> Self {
        let nonces = fauna_client_sync::SetNonceSource::Resolver(resolver.clone());
        let sync =
            SyncClient::new(nest.clone()).with_record_signing(fauna_client_sync::RecordSigning {
                signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
                    keypair,
                )),
                set_nonce: nonces.clone(),
            });
        Self {
            nest,
            sync,
            seat: ReaderSeat {
                own: Some(keypair.actor_id().0),
                nonces: Some(nonces),
                ..Default::default()
            },
            resolver,
            owner: BackupKey::derive(keypair.secret_bytes()),
            device_id,
            fetcher,
            primaries,
            uploader,
        }
    }

    /// Judge with the account's attested predecessor ids
    /// (`AccountRegistry::attested_predecessor_actor_ids`), so a head a
    /// retired identity signed is admitted as the account's own. Admitted is
    /// not *owner-signed*: [`Self::place`] arms the owner-root open only for a
    /// head signed as the CURRENT identity.
    #[must_use]
    pub fn with_predecessors(mut self, predecessors: Vec<[u8; 32]>) -> Self {
        self.seat.predecessors = predecessors;
        self
    }

    /// The attested predecessor ids this walk's seat judges with (empty: the
    /// statement-walk fallback).
    #[must_use]
    pub fn predecessors(&self) -> &[[u8; 32]] {
        &self.seat.predecessors
    }

    /// Converge `folder`'s nest-resident heads onto its current served
    /// generation. A set that is not served-and-group-less answers an empty
    /// tally; a served set whose keys are not in this seat's custody yet is an
    /// error (retry once custody syncs). Per-head failures are counted and
    /// warned, never fatal to the walk.
    pub async fn converge(&self, folder: &str) -> Result<ServedSetConvergence> {
        let folder_r = fauna_core::log_redact::log_folder_name(folder);
        let (keys, foreign) = fauna_core::label_custody::LabelCustody::new(
            Some(self.resolver.clone()),
            Some(self.owner.clone()),
        )
        .keys_for(folder)
        .await;
        if foreign.is_some() || !keys.served || keys.mls_group_id.is_some() {
            tracing::debug!(folder = %folder_r, "served-set walk: not a served, unshared set — nothing to do");
            return Ok(ServedSetConvergence::default());
        }
        let Some(content_keys) = keys.content_keys.clone() else {
            anyhow::bail!(
                "served-set walk: the served set's content keys are not in this seat's custody yet"
            );
        };
        let current = content_keys.current_version();
        let seal_root = (*content_keys.current_key(), Some(current));

        let (heads, certs) = self.heads(folder).await?;
        let owed: Vec<SyncChange> = heads
            .into_iter()
            .filter(|row| row.content_key_version != Some(current))
            .collect();
        let mut tally = ServedSetConvergence::default();
        if owed.is_empty() {
            return Ok(tally);
        }
        let verdicts = {
            let projected: Vec<ProjectedRow<'_>> = owed
                .iter()
                .map(|row| ProjectedRow {
                    folder,
                    folder_hash: None,
                    row: Some(row.clone()),
                })
                .collect();
            self.seat
                .judge(&self.nest)
                .judge(&projected, &certs)
                .await
                .map_err(|e| anyhow::anyhow!("served-set walk: judging the set's heads: {e}"))?
        };
        for (row, verdict) in owed.into_iter().zip(verdicts) {
            let Some(head) = self.place(row, &verdict, &keys) else {
                tally.skipped += 1;
                continue;
            };
            let path_r = fauna_core::log_redact::log_path(&head.path);
            match self.reseal_head(folder, &keys, head, seal_root).await {
                Ok(()) => tally.resealed += 1,
                Err(e) => {
                    tally.failed += 1;
                    tracing::warn!(
                        folder = %folder_r,
                        path = %path_r,
                        error = %format!("{e:#}"),
                        "served-set walk: a pre-serve file was not re-sealed; the next walk retries it"
                    );
                }
            }
        }
        Ok(tally)
    }

    /// The set's live heads, latest per path, from the whole change log — plus
    /// every page's `signer_certs` for the judge.
    async fn heads(
        &self,
        folder: &str,
    ) -> Result<(Vec<SyncChange>, Vec<fauna_core::encoding::EmbedAsBytes>)> {
        let mut latest: std::collections::HashMap<String, SyncChange> =
            std::collections::HashMap::new();
        let mut certs = Vec::new();
        let mut since = 0i64;
        loop {
            let page = self
                .sync
                .changes_list(Some(folder.to_string()), None, since)
                .await
                .map_err(|e| anyhow::anyhow!("served-set walk: fauna.sync.changes.list: {e}"))?;
            let Some(top) = page.changes.iter().map(|c| c.seq).max() else {
                break;
            };
            certs.extend(page.signer_certs);
            for change in page.changes {
                let key = change.path_hash.to_ascii_lowercase();
                if latest.get(&key).is_none_or(|held| held.seq < change.seq) {
                    latest.insert(key, change);
                }
            }
            if top <= since {
                break;
            }
            since = top;
        }
        Ok((
            latest
                .into_values()
                .filter(|c| c.change_type != "delete" && c.manifest_hash.is_some())
                .collect(),
            certs,
        ))
    }

    /// Decide whether one owed head may be opened, and name it. `None` =
    /// skipped (warned): a row the judge did not admit, an unstamped row that
    /// is not this account's own verified record, or a path that does not open.
    fn place(
        &self,
        row: SyncChange,
        verdict: &RowVerdict,
        keys: &FileDownloadKeys,
    ) -> Option<OwedHead> {
        // Signed AS the current identity — never merely attributed to it
        // (ruling (8)(c), the interlock's first half): a head a predecessor
        // signed verifies as this account's own, and must still never arm the
        // owner-root open, which offers the current root first.
        let owner_signed = verdict.signed_as(self.seat.own.as_ref());
        let admitted = if row.content_key_version.is_none() {
            // The owner root opens it, so only the owner's own signature under
            // this set's nonce may license the open (ruling (5)).
            owner_signed
        } else {
            verdict.admits()
        };
        if !admitted {
            tracing::warn!(
                seq = row.seq,
                verdict = ?verdict,
                "served-set walk: a head did not verify as a record this seat may re-seal — skipped"
            );
            return None;
        }
        // …and its label opens without the current owner root too.
        let label_keys = FileDownloadKeys {
            record_signer: if owner_signed {
                RecordSigner::Current
            } else {
                RecordSigner::Other
            },
            ..keys.clone()
        };
        let path = match fauna_core::label_custody::open_change_path(
            |generation| label_keys.label_open_roots(generation),
            row.path_sealed.as_ref().map(|b| &b[..]),
            &row.path_hash,
        ) {
            fauna_core::label_custody::ChangePathOpen::Opened(path) => path,
            other => {
                // A plaintext path is trusted only when it hashes to the
                // row's own (signed) `path_hash`.
                match row.path.clone().filter(|p| {
                    hex::encode(fauna_core::sync::path_hash(p)).eq_ignore_ascii_case(&row.path_hash)
                }) {
                    Some(path) => path,
                    None => {
                        tracing::warn!(
                            seq = row.seq,
                            verdict = ?other,
                            "served-set walk: a head's path did not open — skipped"
                        );
                        return None;
                    }
                }
            }
        };
        let manifest_hash = row
            .manifest_hash
            .as_deref()
            .and_then(|h| fauna_core::hex32::decode(h).ok())
            .map(ContentHash::from_digest_raw)?;
        Some(OwedHead {
            row,
            path,
            manifest_hash,
            owner_signed,
        })
    }

    /// Move one head's seal: the byte move (chunks → manifest), the verify
    /// under the current generation **alone**, the signed record, the
    /// best-effort supersede. The verify precedes the record here (the engine
    /// records first because its local row is the resume anchor; this walk's
    /// only anchor is the nest head, so nothing is recorded that has not
    /// opened).
    async fn reseal_head(
        &self,
        folder: &str,
        keys: &FileDownloadKeys,
        head: OwedHead,
        seal_root: ([u8; 32], Option<u64>),
    ) -> Result<()> {
        let sink = UploaderSink(self.uploader.as_ref());
        let resealed = match self.primary_bytes(&head).await? {
            Some(plaintext) => {
                let sealed = fauna_core::blob_seal::seal_blob(&plaintext, Some(seal_root))
                    .context("sealing the pre-serve upload under the served key")?;
                sink.put_chunks(sealed.chunks, &head.path).await?;
                sink.put_manifest(sealed.manifest_hash, sealed.manifest_bytes)
                    .await?;
                (sealed.manifest_hash, sealed.manifest.total_size)
            }
            None => {
                let record_keys = FileDownloadKeys {
                    owner_signed_record: head.owner_signed,
                    record_signer: if head.owner_signed {
                        RecordSigner::Current
                    } else {
                        RecordSigner::Other
                    },
                    ..keys.clone()
                };
                let resealed = fauna_core::nest_reseal::reseal_nest_copy_windowed(
                    self.fetcher.as_ref(),
                    &record_keys,
                    head.manifest_hash,
                    head.row.content_key_version,
                    &head.path,
                    Some(seal_root),
                    &sink,
                )
                .await?;
                (resealed.manifest_hash, resealed.manifest.total_size)
            }
        };
        let (manifest_hash, total_size) = resealed;
        let generation = seal_root.1;

        // Verified under the served generation alone — what the mount holds.
        let verify_keys = FileDownloadKeys {
            backup_key: None,
            predecessor_backup_keys: Vec::new(),
            retired_content_keys: None,
            owner_signed_record: false,
            ..keys.clone()
        };
        fauna_core::file_download::verify_file_by_manifest(
            self.fetcher.as_ref(),
            &verify_keys,
            manifest_hash,
            generation,
            &head.path,
        )
        .await
        .context("verifying the re-sealed copy under the served key alone")?;

        // The path seals under the same generation the chunks did.
        let path_sealed = fauna_core::label_custody::seal_path_from_keys(keys, &head.path).ok();
        let manifest_hex = hex::encode(manifest_hash.digest());
        self.sync
            .changes_record(
                folder,
                self.device_id.clone(),
                head.path.clone(),
                Some(manifest_hex.clone()),
                total_size as i64,
                "modify",
                generation,
                None,
                path_sealed,
                // A proven reissue of this very head (the engine's nest-sourced
                // re-seal stamps the same): no novel content, so a receiver
                // whose frontier already passed the head skips it.
                Some(head.row.seq),
                Some(true),
            )
            .await
            .map_err(|e| anyhow::anyhow!("recording the re-sealed head: {e}"))?;

        // Reclaim — best-effort: the pre-serve rows' chunks become garbage only
        // now that the re-sealed copy verified and recorded.
        if let Err(e) = self
            .sync
            .changes_supersede(
                folder,
                self.device_id.clone(),
                head.path.clone(),
                manifest_hex,
            )
            .await
        {
            tracing::warn!(error = %e, "served-set walk: supersede failed; reclaim deferred");
        }
        Ok(())
    }

    /// A pre-serve Media blob primary's plaintext, or `None` when the head
    /// names a chunk manifest instead (the blob store answers not-found). Only
    /// an unstamped, owner-signed head can be a primary — a stamped head was
    /// written by a content-key writer, which records manifests.
    async fn primary_bytes(&self, head: &OwedHead) -> Result<Option<Vec<u8>>> {
        if head.row.content_key_version.is_some() || !head.owner_signed {
            return Ok(None);
        }
        let hash_hex = hex::encode(head.manifest_hash.digest());
        let sealed = match self.primaries.fetch_blob(hash_hex.clone()).await {
            Ok(sealed) => sealed,
            Err(MediaApiError::NotFound { .. }) => return Ok(None),
            Err(e) => anyhow::bail!("fetching the pre-serve upload: {e}"),
        };
        // The address check first: the backup-chunk frame carries no AAD, so
        // only the address proves *this* blob.
        if !blake3::hash(&sealed)
            .to_hex()
            .as_str()
            .eq_ignore_ascii_case(&hash_hex)
        {
            anyhow::bail!("the nest served a different blob for the pre-serve upload");
        }
        decrypt_backup_chunk(&self.owner, &sealed)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("opening the pre-serve upload under the owner key: {e}"))
    }
}

// ── Native (`Arc<NestClient>`) ──────────────────────────────────────────────
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use fauna_client::{NestClient, NestPublicChunkFetcher};

    #[async_trait::async_trait]
    impl fauna_core::nest_reseal::ServedSetConverge for ServedSetWalk<Arc<NestClient>> {
        async fn converge_served_set(&self, folder: &str) -> Result<ServedSetConvergence> {
            self.converge(folder).await
        }
    }

    /// The walk over `nest`'s authenticated session, with the same three byte
    /// seams [`crate::build_media_machine_with_folder_keys`] injects.
    pub fn served_set_walk(
        nest: Arc<NestClient>,
        keypair: &fauna_core::identity::ActorKeypair,
        resolver: Arc<dyn FolderKeyResolver>,
        device_id: String,
    ) -> ServedSetWalk<Arc<NestClient>> {
        let uploader: Arc<dyn MediaBlobUploader> =
            Arc::new(crate::blob_uploader::NativeBlobUploader::new(&nest));
        let primaries: Arc<dyn MediaBlobFetcher> =
            Arc::new(crate::blob_fetcher::NativeBlobFetcher::new(&nest));
        let fetcher: Arc<dyn BlobFetcher> = Arc::new(NestPublicChunkFetcher::new(&nest));
        ServedSetWalk::new(
            nest, keypair, resolver, device_id, fetcher, primaries, uploader,
        )
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::served_set_walk;

// ── Wasm (`WsRpcClient`) ────────────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use fauna_rpc_wasm::WsRpcClient;

    #[async_trait::async_trait(?Send)]
    impl fauna_core::nest_reseal::ServedSetConverge for ServedSetWalk<WsRpcClient> {
        async fn converge_served_set(&self, folder: &str) -> Result<ServedSetConvergence> {
            self.converge(folder).await
        }
    }

    /// The walk over the SPA's browser session — the wasm twin of the native
    /// builder, with the seams the wasm Media builder injects.
    pub fn served_set_walk(
        nest: WsRpcClient,
        keypair: &fauna_core::identity::ActorKeypair,
        resolver: Arc<dyn FolderKeyResolver>,
        device_id: String,
    ) -> ServedSetWalk<WsRpcClient> {
        let uploader: Arc<dyn MediaBlobUploader> =
            Arc::new(crate::blob_uploader::WasmBlobUploader::new(nest.clone()));
        let primaries: Arc<dyn MediaBlobFetcher> =
            Arc::new(crate::blob_fetcher::WasmBlobFetcher::new(nest.clone()));
        let fetcher: Arc<dyn BlobFetcher> = Arc::new(
            fauna_core::file_download::WasmPublicChunkFetcher::new(nest.nest_url()),
        );
        ServedSetWalk::new(
            nest, keypair, resolver, device_id, fetcher, primaries, uploader,
        )
    }
}
#[cfg(target_arch = "wasm32")]
pub use wasm::served_set_walk;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use fauna_client_testkit::{ClassifiedError, block_on};
    use fauna_core::folder_keys::{FolderContentKeys, ResolvedCustody, ResolvedFolderKeys};
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::Value;
    use fauna_protocol::folders::{FolderSummary, FoldersListReply, KIND_FOLDERS_LIST};
    use fauna_protocol::sync::{
        SyncChangeRecordReply, SyncChangeRecordRequest, SyncChangesListReply,
        SyncChangesSupersedeReply,
    };
    use fauna_protocol::sync_writer_sig::ChangeSigner;

    const NONCE: [u8; 32] = [7; 32];
    const OTHER_NONCE: [u8; 32] = [9; 32];
    const SET: &str = "photos";
    const DEVICE: &str = "0404040404040404040404040404040404040404040404040404040404040404";

    /// A nest answering a per-kind table and keeping every request's payload
    /// — the walk's contract is what it RECORDS, so the double must show it.
    #[derive(Default)]
    struct TableNest {
        replies: HashMap<&'static str, Value>,
        calls: Mutex<Vec<(&'static str, Vec<u8>)>>,
    }

    impl TableNest {
        fn reply<T: serde::Serialize>(mut self, kind: &'static str, value: &T) -> Self {
            let bytes = fauna_protocol::encode_canonical(value).expect("encode");
            self.replies
                .insert(kind, fauna_protocol::decode_strict(&bytes).expect("value"));
            self
        }

        fn records(&self) -> Vec<SyncChangeRecordRequest> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| *k == "fauna.sync.changes.record")
                .map(|(_, p)| fauna_protocol::decode_strict(p).expect("record request"))
                .collect()
        }

        fn kinds(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().iter().map(|(k, _)| *k).collect()
        }
    }

    impl RpcRequester for TableNest {
        type Error = ClassifiedError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            self.calls.lock().unwrap().push((kind, bytes.to_vec()));
            let Some(v) = self.replies.get(kind) else {
                return Err(ClassifiedError::Transport(format!("no mock for {kind}")));
            };
            let bytes = fauna_protocol::encode_canonical(v).expect("encode reply");
            Ok(fauna_protocol::decode_strict(&bytes).expect("decode reply"))
        }
    }

    /// The nest's two byte stores in memory: the chunk/manifest store (read by
    /// the walk and by the MDA's walk, written by the re-seal) and the blob
    /// store (the Media primaries).
    #[derive(Default)]
    struct Store {
        manifests: Mutex<HashMap<ContentHash, Vec<u8>>>,
        chunks: Mutex<HashMap<ContentHash, Vec<u8>>>,
        blobs: Mutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl BlobFetcher for Store {
        async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
            self.manifests
                .lock()
                .unwrap()
                .get(hash)
                .cloned()
                .context("no such manifest")
        }

        async fn fetch_chunks(
            &self,
            store_keys: &[ContentHash],
            _relative_path: &str,
        ) -> Result<Vec<Vec<u8>>> {
            let chunks = self.chunks.lock().unwrap();
            store_keys
                .iter()
                .map(|k| chunks.get(k).cloned().context("no such chunk"))
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl MediaBlobFetcher for Store {
        async fn fetch_blob(&self, hash: String) -> Result<Vec<u8>, MediaApiError> {
            self.blobs
                .lock()
                .unwrap()
                .get(&hash)
                .cloned()
                .ok_or(MediaApiError::NotFound {
                    detail: "no such blob".into(),
                })
        }
    }

    #[async_trait::async_trait]
    impl MediaBlobUploader for Store {
        async fn post_blob(
            &self,
            _sidecar_cbor: Vec<u8>,
            sealed_bytes: Vec<u8>,
        ) -> Result<String, MediaApiError> {
            let hash = blake3::hash(&sealed_bytes).to_hex().to_string();
            self.blobs
                .lock()
                .unwrap()
                .insert(hash.clone(), sealed_bytes);
            Ok(hash)
        }

        async fn post_chunk(
            &self,
            store_key: [u8; 32],
            body: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            self.chunks
                .lock()
                .unwrap()
                .insert(ContentHash::from_digest_raw(store_key), body);
            Ok(())
        }

        async fn post_manifest(
            &self,
            manifest_hash: [u8; 32],
            manifest_bytes: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            self.manifests
                .lock()
                .unwrap()
                .insert(ContentHash::from_digest_raw(manifest_hash), manifest_bytes);
            Ok(())
        }
    }

    /// The served set's custody as the resolver answers it after the flip:
    /// content-keyed, no group, this set's nonce.
    struct ServedResolver(FolderContentKeys);

    #[async_trait::async_trait]
    impl FolderKeyResolver for ServedResolver {
        async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
            assert_eq!(*name_hash, fauna_core::path_crypto::set_name_hash(SET));
            Ok(ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
                mls_group_id: None,
                content_keys: Some(self.0.clone()),
                home_nest_url: None,
                home_nest_actor_id: None,
            }))
        }

        async fn set_nonce(&self, _folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
            Ok(Some(NONCE))
        }
    }

    fn owner() -> ActorKeypair {
        ActorKeypair::from_secret([1; 32])
    }

    /// A pre-serve head as the Media page (or an owner-only engine) recorded
    /// it: unstamped, its path sealed under the owner root, signed by `signer`
    /// under `nonce` (`None` = unsigned).
    fn head(
        seq: i64,
        path: &str,
        manifest_hex: String,
        signer: Option<(&ActorKeypair, [u8; 32])>,
    ) -> SyncChange {
        let owner = owner();
        let backup = BackupKey::derive(owner.secret_bytes());
        let mut row = SyncChange {
            seq,
            path_hash: hex::encode(fauna_core::sync::path_hash(path)),
            manifest_hash: Some(manifest_hex),
            size_bytes: 10,
            change_type: "create".into(),
            created_at: 1_000,
            device_id: Some(DEVICE.into()),
            author_actor_id: Some(owner.actor_id().to_hex()),
            path_sealed: Some(
                fauna_core::label_custody::seal_path(
                    &fauna_core::path_crypto::LabelRoot::owner_of(&backup),
                    path,
                )
                .expect("seal path")
                .into(),
            ),
            ..Default::default()
        };
        if let Some((kp, nonce)) = signer {
            ChangeSigner::direct(kp)
                .sign_row(&mut row, nonce)
                .expect("sign");
        }
        row
    }

    /// Put an owner-keyed chunk manifest of `bytes` into the store; its hash.
    fn owner_manifest(store: &Store, bytes: &[u8]) -> String {
        let backup = BackupKey::derive(owner().secret_bytes());
        let sealed =
            fauna_core::blob_seal::seal_blob(bytes, Some((backup.convergent_chunk_root(), None)))
                .expect("seal");
        let mut chunks = store.chunks.lock().unwrap();
        for (k, body) in sealed.chunks {
            chunks.insert(k, body);
        }
        store
            .manifests
            .lock()
            .unwrap()
            .insert(sealed.manifest_hash, sealed.manifest_bytes);
        hex::encode(sealed.manifest_hash.digest())
    }

    /// Put a Media blob primary of `bytes` (Library audience, the owner's bare
    /// key) into the blob store; its hash.
    fn owner_primary(store: &Store, bytes: &[u8]) -> String {
        let backup = BackupKey::derive(owner().secret_bytes());
        let sealed = fauna_core::crypto::encrypt_backup_chunk(&backup, bytes).expect("seal");
        let hash = blake3::hash(&sealed).to_hex().to_string();
        store.blobs.lock().unwrap().insert(hash.clone(), sealed);
        hash
    }

    fn nest_over(rows: Vec<SyncChange>) -> Arc<TableNest> {
        Arc::new(
            TableNest::default()
                .reply(
                    "fauna.sync.changes.list",
                    &SyncChangesListReply {
                        changes: rows,
                        ..Default::default()
                    },
                )
                .reply(
                    KIND_FOLDERS_LIST,
                    &FoldersListReply {
                        folders: vec![FolderSummary {
                            name: SET.into(),
                            role: Some("owner".into()),
                            webdav_enabled: true,
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                )
                .reply(
                    "fauna.sync.changes.record",
                    &SyncChangeRecordReply {
                        seq: 9,
                        extra: Default::default(),
                    },
                )
                .reply(
                    "fauna.sync.changes.supersede",
                    &SyncChangesSupersedeReply {
                        superseded: 1,
                        extra: Default::default(),
                    },
                ),
        )
    }

    /// `webdav-server.md` § Key model (c): after the flip, the walk re-seals the
    /// owner's own pre-serve files — a Media blob primary and an owner-keyed
    /// manifest — under the served generation, records each stamped and signed,
    /// and the MDA's walk (the content keys ALONE, no owner root) opens both
    /// byte-identically. An unsigned head and one signed under another set's
    /// nonce are skipped, never re-sealed: the owner root would open them, and
    /// only the owner's signature under this set's nonce says they are this
    /// set's (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (5)).
    #[test]
    fn the_walk_reseals_only_the_owners_verified_pre_serve_heads_for_the_mount() {
        let owner = owner();
        let keys = FolderContentKeys::genesis([42; 32], 1_000);
        let generation = keys.current_version();
        let store = Arc::new(Store::default());

        let uploaded = b"put here before serving, through the Media page\n".repeat(300);
        let synced = b"an engine-recorded file, owner-keyed\n".repeat(5_000);
        let unsigned = b"an unsigned head\n".repeat(10);
        let steered = b"a head signed for another set\n".repeat(10);
        let nest = nest_over(vec![
            head(
                1,
                "before.txt",
                owner_primary(&store, &uploaded),
                Some((&owner, NONCE)),
            ),
            head(
                2,
                "docs/synced.bin",
                owner_manifest(&store, &synced),
                Some((&owner, NONCE)),
            ),
            head(3, "unsigned.txt", owner_manifest(&store, &unsigned), None),
            head(
                4,
                "steered.txt",
                owner_manifest(&store, &steered),
                Some((&owner, OTHER_NONCE)),
            ),
        ]);
        let walk = ServedSetWalk::new(
            nest.clone(),
            &owner,
            Arc::new(ServedResolver(keys.clone())),
            DEVICE.into(),
            store.clone(),
            store.clone(),
            store.clone(),
        );

        let tally = block_on(walk.converge(SET)).expect("walk");
        assert_eq!(
            tally,
            ServedSetConvergence {
                resealed: 2,
                skipped: 2,
                failed: 0,
            }
        );

        // What the mount holds: the served generation, nothing else.
        let mda = FileDownloadKeys {
            served: true,
            content_keys: Some(keys),
            ..Default::default()
        };
        let mut recorded: Vec<(String, Vec<u8>)> = Vec::new();
        for req in nest.records() {
            assert!(fauna_protocol::folders::SetAddressed::addresses(&req, SET));
            assert_eq!(req.change_type, "modify");
            assert_eq!(
                req.content_key_version,
                Some(generation),
                "stamped at the served generation"
            );
            assert!(req.signature.is_some(), "the re-record is writer-signed");
            assert_eq!(req.is_resolution, Some(true), "a proven reissue");
            let hash = ContentHash::from_digest_raw(
                fauna_core::hex32::decode(req.manifest_hash.as_deref().expect("manifest"))
                    .expect("hex"),
            );
            let bytes = block_on(fauna_core::file_download::download_file_bytes_by_manifest(
                store.as_ref(),
                &mda,
                hash,
                Some(generation),
                &req.path,
            ))
            .expect("the mount's walk opens the re-sealed copy under the served key alone");
            recorded.push((req.path, bytes));
        }
        recorded.sort();
        assert_eq!(
            recorded,
            vec![
                ("before.txt".to_string(), uploaded.clone()),
                ("docs/synced.bin".to_string(), synced.clone()),
            ]
        );
        assert_eq!(
            nest.kinds()
                .iter()
                .filter(|k| **k == "fauna.sync.changes.supersede")
                .count(),
            2,
            "each re-sealed head's pre-serve rows are superseded"
        );

        // Idempotent under the deterministic seal: a second walk over the same
        // (still-unstamped, as served) heads records the same manifests. Compared
        // as sets — the walk folds the change log through a `HashMap`, so two
        // walks may visit the heads in different orders.
        let mut first: Vec<_> = nest
            .records()
            .iter()
            .map(|r| (r.path.clone(), r.manifest_hash.clone()))
            .collect();
        block_on(walk.converge(SET)).expect("second walk");
        let mut second: Vec<_> = nest.records()[first.len()..]
            .iter()
            .map(|r| (r.path.clone(), r.manifest_hash.clone()))
            .collect();
        first.sort();
        second.sort();
        assert_eq!(first, second);
    }

    /// `mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(c) — the interlock's first half. A head a PREDECESSOR signed
    /// verifies as this account's own (the walk's seat names the predecessor),
    /// and must still never arm the owner-root open: that open offers the
    /// CURRENT root first, so a retired seed and a lying nest could name here
    /// a manifest the successor sealed in another of its sets, and the walk
    /// would re-seal it for the mount. Asserted through what the walk opens and
    /// records, not through the flag: the planted head is skipped, nothing is
    /// re-sealed, nothing recorded — while the identical head signed as the
    /// current identity is re-sealed.
    #[test]
    fn a_predecessor_signed_head_never_arms_the_owner_root_open() {
        let owner = owner();
        let predecessor = ActorKeypair::from_secret([2; 32]);
        let keys = FolderContentKeys::genesis([42; 32], 1_000);
        let store = Arc::new(Store::default());
        let planted = b"sealed by the successor, after the ceremony\n".repeat(20);

        // Signed by the predecessor under this set's nonce; served, as the
        // nest serves a moved row, with the successor as author.
        let mut by_predecessor = head(1, "planted.txt", owner_manifest(&store, &planted), None);
        by_predecessor.author_actor_id = Some(predecessor.actor_id().to_hex());
        ChangeSigner::direct(&predecessor)
            .sign_row(&mut by_predecessor, NONCE)
            .expect("sign");
        by_predecessor.author_actor_id = Some(owner.actor_id().to_hex());

        let walk = |rows: Vec<SyncChange>| {
            let nest = nest_over(rows);
            let walk = ServedSetWalk::new(
                nest.clone(),
                &owner,
                Arc::new(ServedResolver(keys.clone())),
                DEVICE.into(),
                store.clone(),
                store.clone(),
                store.clone(),
            )
            .with_predecessors(vec![predecessor.actor_id().0]);
            (block_on(walk.converge(SET)).expect("walk"), nest)
        };

        let (tally, nest) = walk(vec![by_predecessor]);
        assert_eq!(
            tally,
            ServedSetConvergence {
                resealed: 0,
                skipped: 1,
                failed: 0,
            }
        );
        assert!(nest.records().is_empty(), "nothing recorded for the mount");

        // The control: the same manifest under the current identity's own
        // signature is this set's pre-serve file, and is re-sealed.
        let by_owner = head(
            1,
            "planted.txt",
            owner_manifest(&store, &planted),
            Some((&owner, NONCE)),
        );
        let (tally, nest) = walk(vec![by_owner]);
        assert_eq!(tally.resealed, 1);
        assert_eq!(nest.records().len(), 1);
    }

    /// The resume half of `webdav-server.md` § Key model (c): a walk
    /// interrupted between two heads left one re-sealed (stamped at the served
    /// generation) and one still pre-serve. The next walk — the launch resume
    /// (`FoldersAuthor::converge_served_sets`) drives exactly this call —
    /// re-records the unstamped head alone; the stamped one is not owed.
    #[test]
    fn an_interrupted_walk_resumes_on_the_unstamped_head_alone() {
        let owner = owner();
        let keys = FolderContentKeys::genesis([42; 32], 1_000);
        let generation = keys.current_version();
        let store = Arc::new(Store::default());

        let done = b"re-sealed before the crash\n".repeat(40);
        let remaining = b"still under the owner root\n".repeat(40);
        let mut stamped = head(
            5,
            "done.txt",
            owner_manifest(&store, &done),
            Some((&owner, NONCE)),
        );
        stamped.content_key_version = Some(generation);
        let nest = nest_over(vec![
            stamped,
            head(
                6,
                "remaining.txt",
                owner_manifest(&store, &remaining),
                Some((&owner, NONCE)),
            ),
        ]);
        let walk = ServedSetWalk::new(
            nest.clone(),
            &owner,
            Arc::new(ServedResolver(keys)),
            DEVICE.into(),
            store.clone(),
            store.clone(),
            store,
        );

        let tally = block_on(walk.converge(SET)).expect("resumed walk");
        assert_eq!(
            tally,
            ServedSetConvergence {
                resealed: 1,
                skipped: 0,
                failed: 0,
            }
        );
        let records = nest.records();
        assert_eq!(records.len(), 1, "exactly one head re-recorded");
        assert_eq!(records[0].path, "remaining.txt");
        assert_eq!(records[0].content_key_version, Some(generation));
    }

    /// A set bound to a sharing group is the M2 pre-bind pass's, not this
    /// walk's: nothing is read or recorded.
    #[test]
    fn a_shared_set_is_left_to_the_pre_bind_pass() {
        struct Bound;
        #[async_trait::async_trait]
        impl FolderKeyResolver for Bound {
            async fn resolve(&self, _name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
                Ok(ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
                    mls_group_id: Some(vec![1; 16]),
                    content_keys: Some(FolderContentKeys::genesis([42; 32], 1_000)),
                    home_nest_url: None,
                    home_nest_actor_id: None,
                }))
            }
        }
        let nest = nest_over(Vec::new());
        let store = Arc::new(Store::default());
        let walk = ServedSetWalk::new(
            nest.clone(),
            &owner(),
            Arc::new(Bound),
            DEVICE.into(),
            store.clone(),
            store.clone(),
            store,
        );
        let tally = block_on(walk.converge(SET)).expect("walk");
        assert_eq!(tally, ServedSetConvergence::default());
        assert!(nest.kinds().is_empty(), "no read, no record");
    }
}
