//! The **nest-held pull-back** — the re-seed ceremony's delivery leg when the
//! surviving copy of a lost nest's corpus is held by **another nest**.
//!
//! Owner: `docs/goal/architecture/segment-backup-protocol.md` § Client-device
//! custodian (pull) → *Restore* → *The nest-held pull-back* (the design, its
//! mechanics and its proof obligation); the ceremony is
//! `docs/goal/behavior/backup-destinations.md` § Third destination kind →
//! *Re-seed*.
//!
//! # What this is, in one sentence
//!
//! The owner's app lists the surviving destination's live custody over its own
//! authenticated connection there, fetches every listed path's manifest and
//! chunks off the destination's open content-addressed routes, and lands them
//! **as-is** on the rebuilt nest over the same two surfaces the custodian leg
//! lands on — so the rebuilt nest ends up holding exactly what the destination
//! holds, and the driver's materialize flips it.
//!
//! # Why as-is, on both planes
//!
//! What a nest destination holds already IS the nest-posture corpus: the
//! source's own coordinator sealed the reserved segment sets under the granted
//! `NestBackupKey` root and mirrored each covered folder's at-rest ciphertext
//! verbatim. Both are exactly what the target's materialize reads, so this leg
//! opens nothing, derives neither `BackupKey` nor `NestBackupKey`, and
//! byte-identity with the destination is the identity function — the same
//! store keys, the same manifest bytes, the same manifest hash. A manifest
//! names its chunks' store keys and its plaintext `total_size` in the clear
//! (only the plaintext chunk hashes are sealed), so the walk needs no key to
//! address what it moves. Every fetched byte is verified against the hash
//! that addressed it before it is pushed.
//!
//! # Order and crash-safety
//!
//! The account rails first, a kind at a time (`mail`, `post`, `calendar`,
//! `card`), each in [`crate::reseed::segment_set_order`]'s walk — the one
//! [`crate::reseed::RecoveryDelivery`] lands in: per family, every segment
//! `.dat` then its `.meta`, then the `manifest.<kind>` mirrors last; then every
//! covered-folder mirror set, path by path in a stable order. Per path: bytes
//! first, custody second. A tear anywhere leaves ordinary, GC-safe custody on
//! the target, and a re-run resumes by content address. Nothing here deletes
//! anything, on either side.
//!
//! # Honest bounds
//!
//! Only the destination's **live head** moves (`custody.list` is
//! latest-per-path): an owner who wants an older generation rolls it back at
//! the destination first (`fauna.backup.generation.restore`) and pulls after.
//! A row the list cannot address — no plaintext `path` — is refused by name,
//! `custody_unaddressable`, never skipped: skipping it would report a corpus
//! with a hole as delivered.

use std::collections::BTreeMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use fauna_client_backup::reseed::{
    DeliveredCorpus, DeliveredSet, FolderLabel, FolderRehome, SetAxis,
};
use fauna_client_backup::trust::BackupNestSeam;
use fauna_core::data::{BackupDestination, ContentHash, parse_folder_backup_set_name};
use fauna_core::file_download::{BlobFetcher, FileDownloadKeys};
use fauna_protocol::RpcRequester;
use fauna_protocol::backup::CustodyItem;
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};

use crate::reseed::{
    BlobPushSink, DestGeneration, RehomeRow, ReseedReport, is_path_hash_hex, parse_manifest_hex,
    push_sealed, record_custody, rehome_nonce, segment_set_order, sign_rehome_rows,
};
use crate::seal::SealedBlob;
use crate::segment_backup::{
    BACKED_UP_KINDS, SegmentFamily, SegmentHalf, reserved_backup_set_name,
};

/// The refusal a custody row the list cannot address earns — no plaintext
/// `path`, or a covered-folder row whose leaf is not a 64-hex `path_hash`.
pub const CUSTODY_UNADDRESSABLE: &str = "custody_unaddressable";

/// The surviving destination, as the owner's own connection reaches it.
pub struct PullBackDestination<'a> {
    /// `fauna.backup.custody.list`, walked to completion
    /// ([`fauna_client_backup::audit::read_full_custody`]) — the audit loop's
    /// read, which the source nest's death does not touch.
    pub seam: &'a dyn BackupNestSeam,
    /// The destination's open, bearer-less content-addressed routes
    /// (`/api/v1/manifests/{hash}`, `/api/v1/chunks/{hash}`).
    pub bytes: &'a dyn BlobFetcher,
}

/// The rebuilt nest, as the owner's own connection reaches it — the custodian
/// leg's own landing surfaces.
pub struct PullBackTarget<'a, P: BlobPushSink, R: RpcRequester> {
    /// The owner-session bulk-byte plane.
    pub bytes: &'a P,
    /// The client-authed `fauna.sync.changes.record` reserved-set arm.
    pub nest: &'a R,
    /// Hex device id, registered write-capable on the **target**.
    pub device_id: String,
}

/// A covered folder's name as the leg hands it to the driver: the display
/// name, and the address + sealed label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderNaming {
    pub display_name: Option<String>,
    pub label: Option<FolderLabel>,
}

/// Every covered folder's name at `destination_id`, read off the account
/// plane's coverage rows (`fauna.state.backup` — the lost box's own list,
/// which survives with the account): the one place the label lives for a
/// folder a nest destination holds, since destination custody never carries
/// it (`segment-backup-protocol.md` § *Where a restored folder's name comes
/// from*).
///
/// A row that recorded the sealed label but no plaintext name (a set sealed
/// past its name) is opened under `keys` — the owner's own seal, read on the
/// owner's own device, as the custodian's pull opens it; a label `keys` cannot
/// open names nothing, and the driver reports the set `folder_unnamed`.
pub fn folder_names_from_coverage(
    rows: &[BackupDestination],
    destination_id: &str,
    keys: Option<&FileDownloadKeys>,
) -> BTreeMap<String, FolderNaming> {
    rows.iter()
        .filter(|row| row.destination_id == destination_id)
        .filter(|row| parse_folder_backup_set_name(&row.folder_name).is_some())
        .map(|row| {
            let label = row.folder_label.as_ref().map(|l| FolderLabel {
                name_hash: l.name_hash,
                name_sealed: l.name_sealed.clone(),
            });
            let display_name = row
                .folder_display_name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .or_else(|| {
                    let (keys, label) = (keys?, label.as_ref()?);
                    fauna_core::label_custody::render_set_name(
                        keys,
                        Some(&label.name_sealed[..]),
                        "",
                        Some(&label.name_hash[..]),
                    )
                    .text()
                    .map(str::to_string)
                });
            (
                row.folder_name.clone(),
                FolderNaming {
                    display_name,
                    label,
                },
            )
        })
        .collect()
}

/// **The nest-held pull-back leg** — moves a surviving nest destination's
/// live custody onto the rebuilt nest, for the shared driver
/// `fauna_client_backup::reseed::run_reseed` to materialize.
///
/// All borrows, like [`crate::reseed::ReseedDelivery`]: the ceremony's host
/// owns where the two connections live.
pub struct NestPullBack<'a, P: BlobPushSink, R: RpcRequester> {
    destination: PullBackDestination<'a>,
    target: PullBackTarget<'a, P, R>,
    /// The backup scope — the owner's own actor.
    scope_id: [u8; 32],
    /// Each covered folder's name, by set ([`folder_names_from_coverage`]).
    names: BTreeMap<String, FolderNaming>,
    /// The host's change signer and set-nonce source — what
    /// [`Self::sign_folder_rehome`] signs the re-homed rows with. Custody
    /// records need none: a reserved set is outside the writer signature by
    /// set class.
    signing: Option<fauna_client_sync::RecordSigning>,
    /// Every sealed folder row this leg delivered, by set — what the re-home
    /// signs, read back off the delivery rather than off the destination a
    /// second time, so the signed rows are exactly the delivered ones.
    delivered_rows: Mutex<BTreeMap<String, Vec<RehomeRow>>>,
}

impl<'a, P: BlobPushSink, R: RpcRequester> NestPullBack<'a, P, R> {
    pub fn new(
        destination: PullBackDestination<'a>,
        target: PullBackTarget<'a, P, R>,
        scope_id: [u8; 32],
    ) -> Self {
        Self {
            destination,
            target,
            scope_id,
            names: BTreeMap::new(),
            signing: None,
            delivered_rows: Mutex::new(BTreeMap::new()),
        }
    }

    /// The covered folders' names, by set ([`folder_names_from_coverage`]). A
    /// folder set with no entry is delivered nameless and reported
    /// `folder_unnamed` by the driver — never restored under a guessed name.
    #[must_use]
    pub fn with_folder_names(mut self, names: BTreeMap<String, FolderNaming>) -> Self {
        self.names = names;
        self
    }

    /// Sign the folder re-homes with the host's signer, under the nonce of
    /// each restored folder's prepared target set.
    #[must_use]
    pub fn with_rehome_signing(mut self, signing: fauna_client_sync::RecordSigning) -> Self {
        self.signing = Some(signing);
        self
    }

    /// Deliver everything the destination holds live for this owner: every
    /// backed-up kind's reserved segment set, then every covered-folder mirror
    /// set. Failures are not swallowed — a one-shot recovery ceremony a human
    /// is watching must never report a part-delivered corpus as delivered.
    pub async fn run_all(&self) -> Result<ReseedReport> {
        let live = fauna_client_backup::audit::read_full_custody(self.destination.seam)
            .await
            .map_err(|e| anyhow::anyhow!("pull-back: the destination's custody list: {e}"))?;

        let mut report = ReseedReport::default();
        for kind in BACKED_UP_KINDS {
            report.absorb(
                self.run_kind(kind, &live)
                    .await
                    .with_context(|| format!("pull-back for kind {kind}"))?,
            );
        }
        report.absorb(
            self.run_covered_folders(&live)
                .await
                .context("pull-back for the covered-folder mirror plane")?,
        );
        Ok(report)
    }

    /// One kind's reserved segment set, both families, in the shared walk.
    async fn run_kind(&self, kind: &str, live: &[CustodyItem]) -> Result<ReseedReport> {
        let set = reserved_backup_set_name(kind, &self.scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no backup surface: {kind}"))?;
        let scope_hex = hex::encode(self.scope_id);
        let mut generations = Vec::new();
        for item in live.iter().filter(|i| i.folder_name == set) {
            let path = addressable(item)?;
            let manifest_hash = parse_manifest_hex(&item.manifest_hash).ok_or_else(|| {
                anyhow::anyhow!("pull-back: {set}/{path} names a malformed manifest hash")
            })?;
            generations.push(DestGeneration {
                path: path.to_string(),
                manifest_hash,
                order: (true, item.updated_at),
            });
        }

        let ordered = segment_set_order(kind, &scope_hex, &generations);
        let mut report = ReseedReport::default();
        for g in &ordered {
            let sealed = fetch_as_is(self.destination.bytes, g.manifest_hash, &g.path).await?;
            report.absorb(
                push_sealed(self.target.bytes, &sealed)
                    .await
                    .with_context(|| format!("pull-back: landing {}'s bytes", g.path))?,
            );
            record_custody(
                self.target.nest,
                &set,
                &self.target.device_id,
                &g.path,
                &sealed,
                sealed.manifest.total_size,
            )
            .await
            .with_context(|| format!("pull-back: recording {} on the target", g.path))?;
            report.delivered_paths.push(g.path.clone());
            report.plaintext_bytes = report
                .plaintext_bytes
                .saturating_add(sealed.manifest.total_size);
        }

        // A segment the destination holds without its sidecar landed (it is
        // real custody) but cannot be reopened — reported, as the custodian
        // leg reports its own, so the driver never calls the corpus whole.
        for family in SegmentFamily::PASS_ORDER {
            if family.serve_kind(kind).is_none() {
                continue;
            }
            let halves = |want: SegmentHalf| -> std::collections::BTreeSet<u32> {
                ordered
                    .iter()
                    .filter_map(|g| family.parse(&scope_hex, &g.path))
                    .filter(|(_, half)| *half == want)
                    .map(|(id, _)| id)
                    .collect()
            };
            let metas = halves(SegmentHalf::Meta);
            report.sidecarless_segments.extend(
                halves(SegmentHalf::Dat)
                    .into_iter()
                    .filter(|id| !metas.contains(id)),
            );
        }

        if !report.delivered_paths.is_empty() {
            report.delivered_sets.push(DeliveredSet {
                set_name: set.clone(),
                axis: SetAxis::Segment,
                folder_display_name: None,
                folder_label: None,
            });
        }
        tracing::info!(
            kind,
            set = %set,
            delivered = report.delivered_paths.len(),
            sidecarless = report.sidecarless_segments.len(),
            "pull-back: kind complete",
        );
        Ok(report)
    }

    /// Every covered-folder mirror set the destination holds, path by path.
    ///
    /// Which folders were covered is read off custody itself — the source that
    /// knew is gone — admitted by [`parse_folder_backup_set_name`], the same
    /// door the target's provisioning applies, so every set this leg names is
    /// one the target will mint.
    async fn run_covered_folders(&self, live: &[CustodyItem]) -> Result<ReseedReport> {
        let mut rows: Vec<(&str, &str, &CustodyItem)> = Vec::new();
        for item in live {
            if parse_folder_backup_set_name(&item.folder_name).is_none() {
                continue;
            }
            let leaf = addressable(item)?;
            if !is_path_hash_hex(leaf) {
                anyhow::bail!(
                    "{CUSTODY_UNADDRESSABLE}: {}/{leaf} is not a path-hash leaf the target \
                     could re-home",
                    item.folder_name
                );
            }
            rows.push((item.folder_name.as_str(), leaf, item));
        }
        rows.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));

        let mut report = ReseedReport::default();
        let mut signable: BTreeMap<String, Vec<RehomeRow>> = BTreeMap::new();
        for (set, leaf, item) in rows {
            let manifest_hash = parse_manifest_hex(&item.manifest_hash).ok_or_else(|| {
                anyhow::anyhow!("pull-back: {set}/{leaf} names a malformed manifest hash")
            })?;
            let sealed = fetch_as_is(self.destination.bytes, manifest_hash, leaf).await?;
            report.absorb(
                push_sealed(self.target.bytes, &sealed)
                    .await
                    .with_context(|| format!("pull-back: landing {set}/{leaf}'s bytes"))?,
            );
            let path_sealed = item.path_sealed.as_ref().map(|b| b.as_slice());
            self.record_folder(set, leaf, &sealed, path_sealed).await?;

            let held = format!("{set}/{leaf}");
            match path_sealed {
                Some(path_sealed) => signable
                    .entry(set.to_string())
                    .or_default()
                    .push(RehomeRow {
                        path_hash: fauna_core::hex32::decode(leaf).map_err(|_| {
                            anyhow::anyhow!("pull-back: {held} has a malformed leaf")
                        })?,
                        manifest_hash: manifest_hash.digest(),
                        total_size: sealed.manifest.total_size,
                        path_sealed: path_sealed.to_vec(),
                    }),
                None => {
                    tracing::warn!(
                        set,
                        path = leaf,
                        "pull-back: the destination holds no sealed name for this mirrored \
                         path — its bytes and custody landed, but the folder cannot be \
                         materialized until it does"
                    );
                    report.folder_paths_without_seal.push(held.clone());
                }
            }
            report.delivered_paths.push(held);
            report.plaintext_bytes = report
                .plaintext_bytes
                .saturating_add(sealed.manifest.total_size);
            if report.delivered_sets.last().map(|s| s.set_name.as_str()) != Some(set) {
                let naming = self.names.get(set).cloned().unwrap_or_default();
                report.delivered_sets.push(DeliveredSet {
                    set_name: set.to_string(),
                    axis: SetAxis::Folder,
                    folder_display_name: naming.display_name,
                    folder_label: naming.label,
                });
            }
        }
        *self.delivered_rows.lock().expect("delivered rows") = signable;
        Ok(report)
    }

    /// Custody for one mirrored folder path on the target: the leaf as the
    /// path (what the source's own folder pass recorded), the sealed name the
    /// destination carried, and no `folder_id` — the client-authed door takes
    /// the `__folder/<nest>/<id>` set name as declared.
    async fn record_folder(
        &self,
        set: &str,
        leaf: &str,
        sealed: &SealedBlob,
        path_sealed: Option<&[u8]>,
    ) -> Result<()> {
        let req = SyncChangeRecordRequest {
            folder: set.to_string(),
            device_id: self.target.device_id.clone(),
            path: leaf.to_string(),
            manifest_hash: Some(hex::encode(sealed.manifest_hash.digest())),
            size_bytes: sealed.manifest.total_size as i64,
            change_type: "create".to_string(),
            path_sealed: path_sealed.map(|b| fauna_protocol::ByteBuf::from(b.to_vec())),
            ..Default::default()
        };
        let _: SyncChangeRecordReply = self
            .target
            .nest
            .request(
                "fauna.sync.changes.record",
                fauna_protocol::folders::addressed(req),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("pull-back: recording folder custody for {set}/{leaf}"))?;
        Ok(())
    }

    /// Phase 2 as the driver sees it: one [`Self::run_all`], projected onto
    /// the terms the owner is shown.
    ///
    /// Generic over any transport, where the `ReseedDeliveryLeg` impl below is
    /// not — for the reason `ReseedDelivery::deliver_corpus` gives.
    pub async fn deliver_corpus(&self) -> std::result::Result<DeliveredCorpus, String> {
        let report = self.run_all().await.map_err(|e| format!("{e:#}"))?;
        Ok(DeliveredCorpus {
            sets: report.delivered_sets,
            sidecarless_segments: report.sidecarless_segments,
            folder_paths_without_seal: report.folder_paths_without_seal,
            plaintext_bytes: report.plaintext_bytes,
        })
    }

    /// The owner's signature over every sealed row this leg delivered into
    /// `set`, under the nonce of the live set the folder re-homes into
    /// (`writer-signed-change-records.md` ruling (7)(a)(ii)) — the same
    /// statements the custodian leg signs, over the rows exactly as they
    /// landed on the target.
    pub async fn sign_folder_rehome(&self, set: &DeliveredSet) -> FolderRehome {
        let (signing, nonce) = match rehome_nonce(self.signing.as_ref(), set).await {
            Ok(resolved) => resolved,
            Err(unsigned) => return unsigned,
        };
        let rows = self
            .delivered_rows
            .lock()
            .expect("delivered rows")
            .get(&set.set_name)
            .cloned()
            .unwrap_or_default();
        sign_rehome_rows(signing, nonce, &rows)
    }
}

/// The plaintext `path` a custody row is addressed by, or the
/// [`CUSTODY_UNADDRESSABLE`] refusal naming the row.
fn addressable(item: &CustodyItem) -> Result<&str> {
    item.path.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "{CUSTODY_UNADDRESSABLE}: a row in {} (path hash {}) carries no path to land it under",
            item.folder_name,
            item.path_hash
        )
    })
}

/// Fetch one custody path off the destination's open routes **as it rests
/// there** — the manifest by its hash, then its chunks by their store keys —
/// verifying every byte against the hash that addressed it.
pub(crate) async fn fetch_as_is(
    fetcher: &dyn BlobFetcher,
    manifest_hash: ContentHash,
    path: &str,
) -> Result<SealedBlob> {
    let manifest_hex = hex::encode(manifest_hash.digest());
    let manifest_bytes = fetcher
        .fetch_manifest(&manifest_hash)
        .await
        .with_context(|| format!("pull-back: fetching {path}'s manifest {manifest_hex}"))?;
    anyhow::ensure!(
        ContentHash::of_raw(&manifest_bytes) == manifest_hash,
        "pull-back: manifest {manifest_hex} for {path} failed hash verification"
    );
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).map_err(|e| {
            anyhow::anyhow!("pull-back: manifest {manifest_hex} does not decode: {e}")
        })?;
    let keys = manifest.store_keys();
    let bodies = fetcher
        .fetch_chunks(&keys, path)
        .await
        .with_context(|| format!("pull-back: fetching {path}'s chunks"))?;
    anyhow::ensure!(
        bodies.len() == keys.len(),
        "pull-back: the destination returned {} of {path}'s {} chunks",
        bodies.len(),
        keys.len()
    );
    let mut chunks = Vec::with_capacity(keys.len());
    for (key, body) in keys.into_iter().zip(bodies) {
        anyhow::ensure!(
            ContentHash::of_raw(&body) == key,
            "pull-back: chunk {} of {path} failed hash verification",
            hex::encode(key.digest())
        );
        chunks.push((key, body));
    }
    Ok(SealedBlob {
        manifest,
        manifest_bytes,
        manifest_hash,
        chunks,
        content_key_version: None,
    })
}

/// The production seam: the owner's own authed WS-RPC connection to the
/// target, whose request future is `Send`, so the ceremony can run on a
/// spawned task (as `ReseedDelivery`'s).
#[async_trait::async_trait]
impl<P: BlobPushSink> fauna_client_backup::reseed::ReseedDeliveryLeg
    for NestPullBack<'_, P, fauna_client::NestClient>
{
    async fn deliver(&self) -> std::result::Result<DeliveredCorpus, String> {
        self.deliver_corpus().await
    }

    async fn sign_rehome(&self, set: &DeliveredSet) -> FolderRehome {
        self.sign_folder_rehome(set).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::seal_blob;
    use fauna_core::crypto::NestBackupKey;
    use fauna_protocol::backup::{CustodyListReply, CustodyListRequest};
    use fauna_protocol::sync_writer_sig::SignedChange;
    use std::collections::HashMap;
    use std::sync::Arc;

    const SCOPE: [u8; 32] = [0xA7; 32];
    const SOURCE_NEST: [u8; 32] = [0x5C; 32];
    /// The rebuilt nest's write device, hex-spelled as the record arm takes it.
    fn device() -> String {
        hex::encode([0x1D; 32])
    }

    fn body(tag: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| tag ^ (i % 251) as u8).collect()
    }

    fn scope_hex() -> String {
        hex::encode(SCOPE)
    }

    fn folder_set(id: i64) -> String {
        fauna_core::data::folder_backup_set_name(&SOURCE_NEST, id)
    }

    /// One ordered log every fake appends to — the order across the two
    /// connections is the contract.
    type Log = Arc<Mutex<Vec<String>>>;

    /// The surviving destination: custody rows plus the bytes its open routes
    /// serve, keyed by hash. `tamper` serves one chunk with a byte flipped.
    #[derive(Default)]
    struct Destination {
        items: Mutex<Vec<CustodyItem>>,
        blobs: Mutex<HashMap<ContentHash, Vec<u8>>>,
        tamper: Mutex<Option<ContentHash>>,
    }

    impl Destination {
        /// Hold `sealed` at `path` in `set`, as the source's coordinator left it.
        fn hold(
            &self,
            set: &str,
            path: Option<&str>,
            sealed: &SealedBlob,
            path_sealed: Option<&[u8]>,
        ) {
            let mut blobs = self.blobs.lock().unwrap();
            blobs.insert(sealed.manifest_hash, sealed.manifest_bytes.clone());
            for (key, chunk) in &sealed.chunks {
                blobs.insert(*key, chunk.clone());
            }
            self.items.lock().unwrap().push(CustodyItem {
                folder_name: set.to_string(),
                path: path.map(str::to_string),
                path_hash: "00".repeat(32),
                manifest_hash: hex::encode(sealed.manifest_hash.digest()),
                path_sealed: path_sealed.map(|b| fauna_protocol::ByteBuf::from(b.to_vec())),
                ..Default::default()
            });
        }
    }

    /// The owner's connection to the destination: `custody.list` alone.
    struct DestLink(Arc<Destination>);

    impl RpcRequester for DestLink {
        type Error = anyhow::Error;

        async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            anyhow::ensure!(
                kind == "fauna.backup.custody.list",
                "the pull-back asked the destination for {kind}"
            );
            let _: CustodyListRequest =
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&payload)?)?;
            let reply = CustodyListReply {
                items: self.0.items.lock().unwrap().clone(),
                ..Default::default()
            };
            Ok(fauna_protocol::decode_strict(
                &fauna_protocol::encode_canonical(&reply)?,
            )?)
        }
    }

    fauna_client_backup::impl_backup_nest_seam!(struct DestSeam<DestLink>);

    struct DestBytes(Arc<Destination>);

    #[async_trait::async_trait]
    impl BlobFetcher for DestBytes {
        async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
            self.0
                .blobs
                .lock()
                .unwrap()
                .get(hash)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404"))
        }

        async fn fetch_chunks(&self, keys: &[ContentHash], _path: &str) -> Result<Vec<Vec<u8>>> {
            let blobs = self.0.blobs.lock().unwrap();
            let tamper = *self.0.tamper.lock().unwrap();
            keys.iter()
                .map(|k| {
                    let mut body = blobs
                        .get(k)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("404"))?;
                    if Some(*k) == tamper {
                        body[0] ^= 1;
                    }
                    Ok(body)
                })
                .collect()
        }
    }

    /// The rebuilt nest: its byte plane and its record arm, logging in order.
    struct Target {
        log: Log,
        chunks: Mutex<HashMap<ContentHash, Vec<u8>>>,
        records: Mutex<Vec<SyncChangeRecordRequest>>,
    }

    #[async_trait::async_trait]
    impl BlobPushSink for Target {
        async fn missing_chunks(&self, keys: &[ContentHash]) -> Result<Vec<ContentHash>> {
            let held = self.chunks.lock().unwrap();
            Ok(keys
                .iter()
                .copied()
                .filter(|k| !held.contains_key(k))
                .collect())
        }

        async fn put_chunk(&self, key: &ContentHash, body: &[u8]) -> Result<()> {
            self.chunks.lock().unwrap().insert(*key, body.to_vec());
            Ok(())
        }

        async fn put_manifest(&self, bytes: &[u8]) -> Result<()> {
            let hash = ContentHash::of_raw(bytes);
            self.log
                .lock()
                .unwrap()
                .push(format!("bytes {}", hex::encode(&hash.digest()[..4])));
            Ok(())
        }
    }

    impl RpcRequester for Target {
        type Error = anyhow::Error;

        async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            anyhow::ensure!(
                kind == "fauna.sync.changes.record",
                "the pull-back opened {kind} on the target"
            );
            let req: SyncChangeRecordRequest =
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&payload)?)?;
            self.log
                .lock()
                .unwrap()
                .push(format!("record {} {}", req.folder, req.path));
            self.records.lock().unwrap().push(req);
            Ok(fauna_protocol::decode_strict(
                &fauna_protocol::encode_canonical(&SyncChangeRecordReply {
                    seq: 0,
                    extra: Default::default(),
                })?,
            )?)
        }
    }

    struct Rig {
        dest: Arc<Destination>,
        seam: DestSeam,
        bytes: DestBytes,
        target: Target,
        log: Log,
    }

    impl Rig {
        fn new() -> Self {
            let dest = Arc::new(Destination::default());
            let log: Log = Arc::new(Mutex::new(Vec::new()));
            Self {
                seam: DestSeam {
                    client: fauna_client_backup::BackupClient::new(DestLink(dest.clone())),
                },
                bytes: DestBytes(dest.clone()),
                target: Target {
                    log: log.clone(),
                    chunks: Mutex::new(HashMap::new()),
                    records: Mutex::new(Vec::new()),
                },
                dest,
                log,
            }
        }

        fn leg(&self) -> NestPullBack<'_, Target, Target> {
            NestPullBack::new(
                PullBackDestination {
                    seam: &self.seam,
                    bytes: &self.bytes,
                },
                PullBackTarget {
                    bytes: &self.target,
                    nest: &self.target,
                    device_id: device(),
                },
                SCOPE,
            )
        }

        /// The source coordinator's seal of a segment-plane path: the granted
        /// `NestBackupKey` root.
        fn hold_segment(&self, path: &str, plain: &[u8]) -> SealedBlob {
            let root = NestBackupKey::from_bytes([9; 32]).convergent_chunk_root();
            let sealed = seal_blob(plain, Some((root, None))).unwrap();
            self.dest.hold("__mail", Some(path), &sealed, None);
            sealed
        }

        /// A covered folder's at-rest ciphertext, mirrored as-is — under a
        /// root neither the leg nor the target holds.
        fn hold_folder(
            &self,
            set: &str,
            leaf: &str,
            plain: &[u8],
            name: Option<&[u8]>,
        ) -> SealedBlob {
            let sealed = seal_blob(plain, Some(([0x5E; 32], None))).unwrap();
            self.dest.hold(set, Some(leaf), &sealed, name);
            sealed
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    fn short(sealed: &SealedBlob) -> String {
        format!("bytes {}", hex::encode(&sealed.manifest_hash.digest()[..4]))
    }

    fn content(id: u32) -> (String, String) {
        let f = SegmentFamily::Content;
        (f.dat_path(&scope_hex(), id), f.meta_path(&scope_hex(), id))
    }

    /// **Both planes land as-is, in the shared walk, bytes before custody.**
    /// Every segment `.dat` before its `.meta`, the journal family after the
    /// content family, the mirrors last; then the covered folder. What lands
    /// is the destination's own bytes — the same manifest hashes, store keys
    /// and bodies — and every custody row names its set and path as the
    /// destination held them.
    #[tokio::test]
    async fn both_planes_land_as_is_in_the_shared_walk_bytes_before_custody() {
        let rig = Rig::new();
        let (dat2, meta2) = content(2);
        let (dat1, meta1) = content(1);
        let mirror = SegmentFamily::Content.mirror_path(&scope_hex(), "mail");
        let jdat = SegmentFamily::Placement.dat_path(&scope_hex(), 1);
        let jmeta = SegmentFamily::Placement.meta_path(&scope_hex(), 1);
        let jmirror = SegmentFamily::Placement.mirror_path(&scope_hex(), "mail");
        // Held in an order the walk must not follow.
        let s_mirror = rig.hold_segment(&mirror, &body(0x10, 300));
        let s_jmirror = rig.hold_segment(&jmirror, &body(0x11, 200));
        let s_meta2 = rig.hold_segment(&meta2, &body(0x12, 100));
        let s_jdat = rig.hold_segment(&jdat, &body(0x13, 900));
        let s_dat2 = rig.hold_segment(&dat2, &body(0x14, 9_000));
        let s_dat1 = rig.hold_segment(&dat1, &body(0x15, 7_000));
        let s_meta1 = rig.hold_segment(&meta1, &body(0x16, 120));
        let s_jmeta = rig.hold_segment(&jmeta, &body(0x17, 80));
        let set = folder_set(7);
        let leaf = hex::encode([0x22; 32]);
        let s_file = rig.hold_folder(&set, &leaf, &body(0x18, 4_000), Some(b"sealed-name"));

        let names: BTreeMap<String, FolderNaming> = [(
            set.clone(),
            FolderNaming {
                display_name: Some("Photos".into()),
                label: None,
            },
        )]
        .into();
        let report = rig.leg().with_folder_names(names).run_all().await.unwrap();

        let step = |sealed: &SealedBlob, set: &str, path: &str| {
            vec![short(sealed), format!("record {set} {path}")]
        };
        let expected: Vec<String> = [
            step(&s_dat1, "__mail", &dat1),
            step(&s_meta1, "__mail", &meta1),
            step(&s_dat2, "__mail", &dat2),
            step(&s_meta2, "__mail", &meta2),
            step(&s_jdat, "__mail", &jdat),
            step(&s_jmeta, "__mail", &jmeta),
            step(&s_mirror, "__mail", &mirror),
            step(&s_jmirror, "__mail", &jmirror),
            step(&s_file, &set, &leaf),
        ]
        .concat();
        assert_eq!(rig.log(), expected);

        // Byte identity: every chunk the target holds is the destination's,
        // under the destination's own store key.
        let held = rig.dest.blobs.lock().unwrap().clone();
        for (key, chunk) in rig.target.chunks.lock().unwrap().iter() {
            assert_eq!(held.get(key), Some(chunk), "a chunk landed re-sealed");
        }
        // The folder row carries the destination's sealed name; no segment
        // row carries one.
        let records = rig.target.records.lock().unwrap().clone();
        let file = records.iter().find(|r| r.path == leaf).unwrap();
        assert_eq!(
            file.path_sealed.as_ref().map(|b| b.as_slice()),
            Some(&b"sealed-name"[..])
        );
        assert!(
            records
                .iter()
                .filter(|r| r.path != leaf)
                .all(|r| r.path_sealed.is_none())
        );

        assert_eq!(
            report
                .delivered_sets
                .iter()
                .map(|s| (s.set_name.clone(), s.axis, s.folder_display_name.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("__mail".to_string(), SetAxis::Segment, None),
                (set, SetAxis::Folder, Some("Photos".to_string())),
            ]
        );
        assert!(report.sidecarless_segments.is_empty());
        assert!(report.folder_paths_without_seal.is_empty());
    }

    /// A row the list carries no path for is refused by name — never skipped,
    /// which would report a corpus with a hole as delivered.
    #[tokio::test]
    async fn a_row_with_no_path_is_refused_by_name() {
        let rig = Rig::new();
        let (dat1, _) = content(1);
        rig.hold_segment(&dat1, &body(1, 100));
        let root = NestBackupKey::from_bytes([9; 32]).convergent_chunk_root();
        let orphan = seal_blob(&body(2, 100), Some((root, None))).unwrap();
        rig.dest.hold("__mail", None, &orphan, None);

        let err = rig.leg().run_all().await.unwrap_err();
        assert!(
            format!("{err:#}").contains(CUSTODY_UNADDRESSABLE),
            "{err:#}"
        );
        assert!(
            rig.target.records.lock().unwrap().is_empty(),
            "nothing is recorded before the set is known addressable"
        );
    }

    /// A segment the destination holds without its sidecar lands and is
    /// reported, so the driver never calls the corpus whole.
    #[tokio::test]
    async fn a_segment_held_without_its_sidecar_is_delivered_and_reported() {
        let rig = Rig::new();
        let (dat1, meta1) = content(1);
        let (dat2, _) = content(2);
        rig.hold_segment(&dat1, &body(1, 100));
        rig.hold_segment(&meta1, &body(2, 10));
        rig.hold_segment(&dat2, &body(3, 100));

        let corpus = rig.leg().deliver_corpus().await.unwrap();
        assert_eq!(corpus.sidecarless_segments, vec![2]);
        assert_eq!(rig.target.records.lock().unwrap().len(), 3);
    }

    /// A byte that fails the hash it was addressed by is refused before it
    /// lands, and nothing is recorded for its path.
    #[tokio::test]
    async fn a_chunk_that_fails_its_hash_is_refused_before_it_lands() {
        let rig = Rig::new();
        let (dat1, _) = content(1);
        let sealed = rig.hold_segment(&dat1, &body(1, 5_000));
        *rig.dest.tamper.lock().unwrap() = Some(sealed.chunks[0].0);

        let err = rig.leg().run_all().await.unwrap_err();
        assert!(
            format!("{err:#}").contains("failed hash verification"),
            "{err:#}"
        );
        assert!(rig.target.chunks.lock().unwrap().is_empty());
        assert!(rig.target.records.lock().unwrap().is_empty());
    }

    /// A folder row the destination holds no sealed name for lands and is
    /// reported — the folder cannot be re-homed without it — and a set the
    /// coverage rows do not name is delivered nameless.
    #[tokio::test]
    async fn a_folder_row_without_a_sealed_name_is_delivered_and_reported() {
        let rig = Rig::new();
        let set = folder_set(3);
        let leaf = hex::encode([0x33; 32]);
        rig.hold_folder(&set, &leaf, &body(4, 100), None);

        let corpus = rig.leg().deliver_corpus().await.unwrap();
        assert_eq!(
            corpus.folder_paths_without_seal,
            vec![format!("{set}/{leaf}")]
        );
        assert_eq!(corpus.sets[0].folder_display_name, None);
    }

    /// **The re-home signs every sealed row the leg delivered**, under the
    /// target set's nonce, over the manifest's own `total_size` — the
    /// statement the target's folder materialize rebuilds from its custody.
    /// No nonce, or no signer, holds the set with the reason.
    #[tokio::test]
    async fn the_rehome_signs_every_delivered_sealed_row_under_the_targets_nonce() {
        let rig = Rig::new();
        let set = folder_set(7);
        let leaf = hex::encode([0x22; 32]);
        let sealed = rig.hold_folder(&set, &leaf, &body(5, 4_000), Some(b"sealed-name"));
        rig.hold_folder(&set, &hex::encode([0x44; 32]), &body(6, 10), None);

        let root = fauna_core::identity::ActorKeypair::from_secret([0x44; 32]);
        let owner = root.actor_id().0;
        let nonce = [0x6E; 32];
        let signing = |nonces: HashMap<String, [u8; 32]>| fauna_client_sync::RecordSigning {
            signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(&root)),
            set_nonce: fauna_client_sync::SetNonceSource::by_folder(nonces),
        };
        let target = DeliveredSet {
            set_name: set.clone(),
            axis: SetAxis::Folder,
            folder_display_name: Some("Photos".into()),
            folder_label: None,
        };

        let leg = rig
            .leg()
            .with_rehome_signing(signing([("Photos".to_string(), nonce)].into()));
        leg.run_all().await.unwrap();
        let FolderRehome::Signed {
            signer_key,
            signatures,
        } = leg.sign_folder_rehome(&target).await
        else {
            panic!("a host with a signer and the target's nonce signs");
        };
        assert_eq!(signatures.len(), 1, "the nameless row is not signed");
        let statement = SignedChange::for_rehome(
            nonce,
            owner,
            [0x22; 32],
            sealed.manifest_hash.digest(),
            sealed.manifest.total_size as i64,
            b"sealed-name",
        );
        fauna_protocol::sync_writer_sig::verify_statement(
            &statement,
            &signatures[0].signature,
            &signer_key,
            &fauna_protocol::sync_writer_sig::SignerCertCache::new(),
            fauna_core::data::Timestamp::now(),
        )
        .expect("the signature covers the delivered row");

        let no_nonce = rig.leg().with_rehome_signing(signing(HashMap::new()));
        no_nonce.run_all().await.unwrap();
        assert!(matches!(
            no_nonce.sign_folder_rehome(&target).await,
            FolderRehome::Unsigned { reason } if reason.contains("Photos")
        ));
        assert!(matches!(
            rig.leg().sign_folder_rehome(&target).await,
            FolderRehome::Unsigned { .. }
        ));
    }

    /// The names come from the coverage rows of the destination pulled from —
    /// never another destination's, never the enrollment row's.
    #[test]
    fn folder_names_come_from_this_destinations_coverage_rows() {
        let set = folder_set(7);
        let label = fauna_core::data::CoveredFolderLabel {
            name_hash: [0x4E; 32],
            name_sealed: b"sealed".to_vec(),
        };
        let rows = vec![
            BackupDestination {
                destination_id: "dest-1".into(),
                folder_name: "__mail".into(),
                ..Default::default()
            },
            BackupDestination {
                destination_id: "dest-1".into(),
                folder_name: set.clone(),
                folder_display_name: Some("Photos".into()),
                folder_label: Some(label.clone()),
                ..Default::default()
            },
            BackupDestination {
                destination_id: "dest-2".into(),
                folder_name: folder_set(8),
                folder_display_name: Some("Elsewhere".into()),
                ..Default::default()
            },
        ];
        let names = folder_names_from_coverage(&rows, "dest-1", None);
        assert_eq!(names.len(), 1);
        assert_eq!(
            names[&set],
            FolderNaming {
                display_name: Some("Photos".into()),
                label: Some(FolderLabel {
                    name_hash: label.name_hash,
                    name_sealed: label.name_sealed,
                }),
            }
        );
    }
}
