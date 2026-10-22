//! The **re-seed delivery leg** — phase 2 of the standalone-restore ceremony,
//! driven from the custodian device against a nest that holds no corpus for the
//! owner.
//!
//! Owner: `docs/goal/behavior/backup-destinations.md` § Third destination kind →
//! *Re-seed* (the ceremony, its authorization shape and the empty-target rule);
//! the wire mechanics this module implements are
//! `docs/goal/architecture/message-segment-store.md` § Client-device custodian
//! (pull) → *Restore*.
//!
//! # What this is, in one sentence
//!
//! Take the corpus a [`CustodianStore`] already holds — sealed on this device
//! under the owner's **client-only** [`fauna_core::crypto::BackupKey`] — and put
//! it on a target nest as an **ordinary backup destination's** custody, sealed
//! under the owner's `NestBackupKey` root.
//!
//! # Why a re-seal, and why it is not a re-encoding
//!
//! Both roots are seed-derived on this device, but only one may ever cross to a
//! nest: the local store rests under `OwnerSealKey::Client(BackupKey)`
//! ([`crate::custodian_host`]), which is client-only key material, while a nest
//! destination's corpus rests under the `NestBackupKey` the owner grants it
//! (`backup-restore.md` § Security Properties — the two-key model). So the leg
//! opens each held path locally and re-seals it through the **same**
//! [`crate::seal`] pipeline the nest coordinator uses, under the nest root.
//!
//! Because that pipeline is deterministic, what lands is **byte-identical** to
//! what the (now dead) source nest's own coordinator would have pushed to a nest
//! destination: same store keys, same manifest bytes, same manifest hash. That is
//! the property the whole ceremony rests on — it is why the target becomes
//! indistinguishable from an ordinary destination after this leg, and therefore
//! why materialization can be **one** design serving both restore sources rather
//! than a custodian-shaped special case. The test module pins it directly against
//! the nest arm's own seal rather than trusting the prose.
//!
//! # No new door
//!
//! Every write here rides a surface that already exists and already
//! authenticates the owner:
//!
//! - bytes → the bulk-byte plane's owner-session arm (`POST /chunks/check`,
//!   `/chunks`, `/manifests` — `bins/fauna-nest/src/auth.rs`'s `BulkWriteAuth`),
//!   behind [`BlobPushSink`] so a test can drive the leg with no nest at all;
//! - custody → the client-authed `fauna.sync.changes.record` **reserved-set**
//!   arm, which routes on `is_reserved_custody_copy` into the latest-per-path
//!   `backup_custody` projection (`bins/fauna-nest/src/sync_handlers.rs`).
//!
//! Phase 1 of the ceremony (ordinary enrollment on the target) is the caller's:
//! there is deliberately no pre-enrollment write surface, so this leg assumes an
//! authenticated owner session and a write-capable registered device, exactly as
//! any other client write does.
//!
//! # Crash-safety
//!
//! Per path: bytes first, custody second — the same ordering, and the same
//! reason, as the nest coordinator's pass
//! (`bins/fauna-nest/src/segment_backup.rs`). A tear before the record re-pushes
//! next run at chunk-dedup cost and re-records; a tear after leaves a valid,
//! auditable, GC-safe destination. Nothing here deletes anything, on either side.
//!
//! # Scope: two planes, two different pushes
//!
//! [`ReseedDelivery::run_all`] delivers everything this store holds, over both
//! planes the custodian pulls:
//!
//! - **The reserved per-kind segment sets** ([`ReseedDelivery::run_kind`] over
//!   [`crate::segment_backup::BACKED_UP_KINDS`] — `mail`, `post`, `calendar`,
//!   `card`), each from its own set within the store: each segment as its
//!   **pair**, `.dat` then `.meta` (the sidecar is what makes the segment
//!   reopenable; a store that never pulled one is reported, see
//!   [`ReseedReport::sidecarless_segments`]), then the `manifest.<kind>` mirror
//!   — for the kind's content family, and then the same again for its
//!   **placement journal**, into the same set
//!   ([`crate::segment_backup::SegmentFamily`]).
//! - **The covered-folder mirror sets** ([`ReseedDelivery::run_covered_folders`]
//!   over the `__folder/<source-nest-hex>/<folder-id>` names the pull stored
//!   under): each mirrored path, pushed **as-is**.
//!
//! The two arms differ in the one way that matters, and it is not a detail of
//! plumbing:
//!
//! | | reserved segments | covered folders |
//! |---|---|---|
//! | held under | the device's client-only `BackupKey` | the **source folder's own** audience seal |
//! | this leg does | open, then re-seal under `NestBackupKey` | **nothing** — the bytes go out verbatim |
//! | store read | [`CustodianStore::open`] (decrypts) | [`CustodianStore::read_as_is`] |
//! | custody `path_sealed` | `None`, deliberately (a machine-authored routing key) | the source path's sealed name |
//!
//! A folder mirror is already the source's at-rest ciphertext — the pull stores
//! it with no seal step, because the head arrives sealed for the folder's
//! audience and this device holds no key for it. So there is nothing to open,
//! and re-sealing would land a *different* corpus than the one the source
//! nest's own coordinator mirrors, breaking the byte-identity the shared
//! materialize design rests on.
//!
//! The corpus gap that once blocked this arm closed 2026-08-23: mirror rows
//! carry the source path's sealed name
//! ([`crate::segment_backup::FolderHeadEntry::path_sealed`] →
//! [`crate::custodian_store::HeldRow::path_sealed`]), and the owner-authed
//! provisioning door admits `__folder/<nest>/<id>` set names
//! ([`fauna_core::data::parse_folder_backup_set_name`]).
//!
//! What this leg lands is **delivered, not yet live**: phase 3,
//! `fauna.backup.custody.materialize`, flips it, and the one place the three
//! phases are put in order is the ceremony driver
//! `fauna_client_backup::reseed::run_reseed`, which drives this leg through the
//! [`ReseedDeliveryLeg`] impl at the bottom of this file.

use anyhow::{Context, Result};
use fauna_client_backup::reseed::{
    DeliveredCorpus, DeliveredSet, FolderRehome, ReseedDeliveryLeg, SetAxis,
};
use fauna_core::crypto::{NestBackupKey, OwnerSealKey};
use fauna_core::data::ContentHash;
use fauna_core::file_download::FileDownloadKeys;
use fauna_protocol::RpcRequester;
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};

use crate::custodian_store::{CustodianStore, held_path, path_in_set};
use crate::seal::{SealedBlob, seal_blob};
use crate::segment_backup::{
    BACKED_UP_KINDS, SegmentFamily, SegmentHalf, landing_set_name, reserved_backup_set_name,
};
use fauna_core::data::parse_folder_backup_set_name;

/// The target nest's **bulk-byte** plane — the seam lives in
/// [`fauna_client_backup::recover_leg`] since 2026-10-10, beside the lived-in
/// recovery's leg that went wasm-clean; re-exported here so every leg keeps
/// its import path. The production implementation over this crate's pooled
/// transfer client is [`SyncClientSink`], immediately below.
pub use fauna_client_backup::recover_leg::BlobPushSink;

/// [`BlobPushSink`] over the real byte plane.
pub struct SyncClientSink<'a> {
    /// Authenticated to the **target** nest as the owner.
    pub client: &'a crate::nest_client::SyncClient,
}

#[async_trait::async_trait]
impl BlobPushSink for SyncClientSink<'_> {
    async fn missing_chunks(&self, store_keys: &[ContentHash]) -> Result<Vec<ContentHash>> {
        self.client.check_chunks(store_keys).await
    }

    async fn put_chunk(&self, store_key: &ContentHash, body: &[u8]) -> Result<()> {
        self.client.upload_chunk(store_key, body).await
    }

    async fn put_manifest(&self, manifest_bytes: &[u8]) -> Result<()> {
        self.client.upload_manifest(manifest_bytes).await
    }
}

/// The pieces both delivery legs share — the report a pass counts in, the
/// bytes-then-custody landing, the folder re-home's signature — live in
/// [`fauna_client_sync::reseed_leg`] since 2026-10-18, beside the nest-held
/// pull-back that went wasm-clean; re-exported so this leg keeps its import
/// paths.
pub use fauna_client_sync::reseed_leg::{
    RehomeRow, ReseedReport, is_path_hash_hex, push_sealed, record_custody, rehome_nonce,
    sign_rehome_rows,
};

/// One re-seed delivery pass over a custodian store.
///
/// All borrows, like [`crate::custodian_pull::CustodianPull`]: a pass is a view
/// over a store, a byte sink and a nest surface, and the ceremony driver owns
/// where those three live.
pub struct ReseedDelivery<'a, P: BlobPushSink, R: RpcRequester> {
    store: &'a CustodianStore,
    bytes: &'a P,
    nest: &'a R,
    /// The backup scope — the owner's own actor for `mail`.
    scope_id: [u8; 32],
    /// Hex device id, registered write-capable on the **target**.
    device_id: String,
    /// Opens the local store: the client-only [`fauna_core::crypto::BackupKey`].
    local_keys: FileDownloadKeys,
    /// The convergent chunk root every delivered chunk seals under.
    nest_root: [u8; 32],
    /// Writer signing for the folder custody rows this pass records
    /// ([`Self::with_record_signing`]); `None` records them unsigned. The
    /// reserved `__*` segment sets are out of the signature's scope by set
    /// class and are never signed.
    signing: Option<fauna_client_sync::RecordSigning>,
}

impl<'a, P: BlobPushSink, R: RpcRequester> ReseedDelivery<'a, P, R> {
    /// Build a pass.
    ///
    /// `local` is the store's own seal key — client-only, and used here solely to
    /// *open*. `nest_key` is the root the target's custody rests under; it is the
    /// same key the owner grants the target at enrollment, which is what makes
    /// the delivered corpus openable by the materialize verb later.
    ///
    /// The two keys are taken as their distinct types rather than as two
    /// `[u8; 32]` roots on purpose: swapping them would compile, and the failure
    /// would be silent — a corpus sealed under a key no nest may ever hold,
    /// delivered to a nest, which reads as a permanently un-openable backup
    /// rather than as an error.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: &'a CustodianStore,
        bytes: &'a P,
        nest: &'a R,
        scope_id: [u8; 32],
        device_id: impl Into<String>,
        local: OwnerSealKey,
        nest_key: &NestBackupKey,
    ) -> Self {
        Self {
            store,
            bytes,
            nest,
            scope_id,
            device_id: device_id.into(),
            local_keys: FileDownloadKeys {
                backup_key: Some(local),
                ..Default::default()
            },
            nest_root: nest_key.convergent_chunk_root(),
            signing: None,
        }
    }

    /// Sign every folder custody row this pass records (writer-signed change
    /// records — the target's nest refuses an unsigned folder row). The host
    /// resolves each folder's nonce from custody it holds.
    #[must_use]
    pub fn with_record_signing(mut self, signing: fauna_client_sync::RecordSigning) -> Self {
        self.signing = Some(signing);
        self
    }

    /// Deliver everything this store holds: every backed-up kind's reserved
    /// segment set, then every covered folder's mirror.
    ///
    /// Failures are **not** swallowed, on either plane. Unlike a periodic pull —
    /// which logs and continues because the next pass will retry — this is a
    /// one-shot recovery ceremony a human is watching, and a partial corpus
    /// reported as delivered is exactly what would make the following
    /// materialize look complete when it is not.
    ///
    /// The segment sets go first, deliberately: they are the account rails
    /// (`__mail`, `__post`, `__calendar`, `__card`) whose absence makes the
    /// seeded account unusable, while a folder is user content the owner can watch arrive. A tear part-way
    /// therefore leaves the more load-bearing half on the target, and every
    /// tear leaves an ordinary, auditable, GC-safe destination either way.
    pub async fn run_all(&self) -> Result<ReseedReport> {
        let mut report = ReseedReport::default();
        for kind in BACKED_UP_KINDS {
            report.absorb(
                self.run_kind(kind)
                    .await
                    .with_context(|| format!("re-seed delivery for kind {kind}"))?,
            );
        }
        report.absorb(
            self.run_covered_folders()
                .await
                .context("re-seed delivery for the covered-folder mirror plane")?,
        );
        Ok(report)
    }

    /// Deliver one kind's reserved segment set, **both families of it**: the
    /// content segments, then the kind's placement journal
    /// ([`SegmentFamily::PASS_ORDER`]). Per family: every live segment — its
    /// `.dat`, then its `.meta` sidecar — then that family's mirror.
    ///
    /// Each mirror goes **last** in its family on purpose. It is the target's
    /// view of which segments are live, and a mirror naming a segment whose
    /// bytes have not landed is the one ordering that makes a torn delivery look
    /// whole. The sidecar follows its `.dat` for the same reason in miniature:
    /// the materialize verb reopens the pair, and a sidecar naming records whose
    /// container has not landed is the half-state to avoid.
    ///
    /// Both families land in the **one** set, so the set is named once in
    /// [`ReseedReport::delivered_sets`] and one materialize call flips both
    /// halves together (`segment-backup-protocol.md` § Client-device custodian
    /// (pull) → *Restore* → *The placement journal rides the set*). A store
    /// that holds no journal — filled from a source with no placement segments
    /// yet, or whose journal the cap skipped — delivers its content alone, which the target restores unfiled.
    pub async fn run_kind(&self, kind: &str) -> Result<ReseedReport> {
        let set = reserved_backup_set_name(kind, &self.scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no backup surface: {kind}"))?;
        // Two names, derived and trusted neither: the store holds the copy
        // under its own set's name, and the target is recorded — and the verb
        // named — under the landing name, never the rail's, which the target
        // mints as soon as it holds mail (`reserved-folders.md` § __mail Sync →
        // *The landing set*).
        let landing = landing_set_name(kind, &self.scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no landing set: {kind}"))?;
        let scope_hex = hex::encode(self.scope_id);
        let rows = self.store.held().await?;

        let mut report = ReseedReport::default();
        for family in SegmentFamily::PASS_ORDER {
            if family.serve_kind(kind).is_none() {
                continue;
            }
            // Live segment ids of this family, keyed by their `.dat`, in
            // segment order — a stable order so a torn pass resumes over the
            // same prefix rather than over a hash walk's arbitrary one. A live
            // `.meta` with no live `.dat` is not a segment and is not delivered.
            // Only this kind's set is read: every kind's content family shares
            // one within-set grammar, so the set is what tells them apart.
            let mut segments: Vec<u32> = rows
                .iter()
                .filter(|r| CustodianStore::live_at(&rows, &r.path).is_some())
                .filter_map(|r| {
                    match path_in_set(&set, &r.path).and_then(|p| family.parse(&scope_hex, p)) {
                        Some((id, SegmentHalf::Dat)) => Some(id),
                        _ => None,
                    }
                })
                .collect();
            segments.sort_unstable();
            segments.dedup();

            for id in segments {
                report.absorb(
                    self.deliver_path(&set, &landing, kind, &family.dat_path(&scope_hex, id))
                        .await?,
                );
                let meta = family.meta_path(&scope_hex, id);
                if CustodianStore::live_at(&rows, &held_path(&set, &meta)).is_some() {
                    report.absorb(self.deliver_path(&set, &landing, kind, &meta).await?);
                } else {
                    tracing::warn!(
                        kind,
                        family = ?family,
                        segment_id = id,
                        "re-seed delivery: this store holds no sidecar for the segment — its \
                         .dat was delivered, but the target cannot reopen it until a pull pass \
                         backfills the sidecar and delivery runs again"
                    );
                    report.sidecarless_segments.push(id);
                }
            }
            let mirror = family.mirror_path(&scope_hex, kind);
            if CustodianStore::live_at(&rows, &held_path(&set, &mirror)).is_some() {
                report.absorb(self.deliver_path(&set, &landing, kind, &mirror).await?);
            }
        }
        if !report.delivered_paths.is_empty() {
            report.delivered_sets.push(DeliveredSet {
                set_name: landing,
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
            chunks_uploaded = report.chunks_uploaded,
            chunks_deduped = report.chunks_deduped,
            "re-seed delivery: kind complete",
        );
        Ok(report)
    }

    /// Deliver every **covered-folder mirror** this store holds, set by set.
    ///
    /// The second of the two planes a custodian pulls
    /// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
    /// Unlike the segment arm this one is not driven by a list of kinds: which
    /// folders a custodian covers is the *source's* coverage table, which a
    /// re-seed by definition can no longer ask. So the sets are read back out
    /// of the corpus itself — the store's own paths are
    /// `{folder_set}/{path_hash_hex}`, and
    /// [`parse_folder_backup_set_name`] is the admission rule for the prefix.
    ///
    /// That parser is doing real work here, not a formality: it is the same
    /// door the target's owner-authed provisioning applies
    /// (`bins/fauna-nest/src/sync_handlers.rs`'s
    /// `writable_or_provisioned_backup_set`), so a set name this leg would push
    /// under is exactly one the target will mint — no delivery can strand
    /// custody in an unprovisionable set. The leaf must be a 64-hex path hash
    /// for the same reason the segment arm parses its own paths: the target
    /// re-hashes the custody path, so a leaf of any other shape would mint a
    /// row keyed on something no materialize could ever match.
    ///
    /// Per-set ordering is lexicographic and stable, so a torn pass resumes
    /// over the same prefix rather than over a hash walk's arbitrary one.
    /// There is no mirror-last rule to mind on this plane: a folder set has no
    /// `manifest.<kind>` anchor naming its members — each path is independently
    /// complete once its bytes and its custody row have landed.
    pub async fn run_covered_folders(&self) -> Result<ReseedReport> {
        let rows = self.store.held().await?;
        // The one source of a restored folder's name: what the pull recorded
        // off the owner's coverage listing (`CustodianStore::put_folder_name`).
        // Custody carries no label, so a set the store never learned a name
        // for is delivered nameless and the driver reports it unnamed.
        let names = self.store.folder_names().await?;
        let labels = self.store.folder_labels().await?;

        // Every live mirror row as `(set, leaf, its live generation)`,
        // deduplicated and sorted. The row is carried rather than re-read per
        // path: `live_at` is the store's one liveness derivation, and asking it
        // twice for the same path is how two answers start to differ.
        let mut mirrors: Vec<(String, String, &crate::custodian_store::HeldRow)> = rows
            .iter()
            .filter_map(|r| {
                let (set, leaf) = r.path.rsplit_once('/')?;
                parse_folder_backup_set_name(set)?;
                if !is_path_hash_hex(leaf) {
                    return None;
                }
                let live = CustodianStore::live_at(&rows, &r.path)?;
                Some((set.to_string(), leaf.to_string(), live))
            })
            .collect();
        mirrors.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        mirrors.dedup_by(|a, b| (&a.0, &a.1) == (&b.0, &b.1));

        let mut report = ReseedReport::default();
        for (set, leaf, row) in &mirrors {
            report.absorb(self.deliver_folder_path(set, leaf, row).await?);
            // `mirrors` is sorted by set, so a set's paths are contiguous and
            // comparing with the last entry is the whole dedup.
            if report.delivered_sets.last().map(|s| &s.set_name) != Some(set) {
                report.delivered_sets.push(DeliveredSet {
                    set_name: set.clone(),
                    axis: SetAxis::Folder,
                    folder_display_name: names.get(set).cloned(),
                    folder_label: labels.get(set).cloned(),
                });
            }
        }

        tracing::info!(
            sets = mirrors
                .iter()
                .map(|(s, _, _)| s)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            delivered = report.delivered_paths.len(),
            without_seal = report.folder_paths_without_seal.len(),
            chunks_uploaded = report.chunks_uploaded,
            chunks_deduped = report.chunks_deduped,
            "re-seed delivery: covered-folder plane complete",
        );
        Ok(report)
    }

    /// Push one mirrored folder path **as-is** and record its custody.
    ///
    /// No open and no re-seal — see the module doc's table for why. The held
    /// bytes are the source's at-rest ciphertext, so what lands on the target
    /// is bit-for-bit what the source nest's own coordinator mirrored
    /// (`bins/fauna-nest/src/segment_backup.rs`'s folder pass), which is the
    /// same byte-identity property the segment arm buys with a re-seal.
    ///
    /// Bytes first, custody second, exactly as [`Self::deliver_path`]: the
    /// target derives the custody charge from bytes it actually holds and
    /// answers `backup_bytes_not_held` to a record that runs ahead of them.
    async fn deliver_folder_path(
        &self,
        set: &str,
        leaf: &str,
        row: &crate::custodian_store::HeldRow,
    ) -> Result<ReseedReport> {
        let held_path = held_path(set, leaf);
        let as_is = self
            .store
            .read_as_is(&held_path)
            .await
            .with_context(|| format!("re-seed: reading mirrored path {held_path} as-is"))?;

        let mut report = self.push(&as_is).await?;
        self.record_folder(
            set,
            leaf,
            &as_is,
            row.source_size_bytes,
            row.path_sealed.as_deref(),
        )
        .await?;

        if row.path_sealed.is_none() {
            tracing::warn!(
                set,
                path = leaf,
                "re-seed delivery: this store holds no sealed name for the mirrored path — its \
                 bytes and custody landed, but the folder cannot be materialized until a pull \
                 pass backfills the name and delivery runs again"
            );
            report.folder_paths_without_seal.push(held_path.clone());
        }
        report.delivered_paths.push(held_path);
        report.plaintext_bytes = row.source_size_bytes;
        Ok(report)
    }

    /// Custody for one mirrored folder path — the same client-authed
    /// reserved-set arm the segment records ride, differing in exactly two
    /// fields.
    ///
    /// **`path_sealed` is carried**, where [`Self::record`]'s is deliberately
    /// `None`. A mirror row's path key is a machine-authored hash like the
    /// segment arm's, but the *name behind it* is user content that only the
    /// owner's keys open, and the folder materialize arm re-homes live rows
    /// from `(path_hash, path_sealed, manifest_hash)` — live rows minted
    /// without a sealed name are refused outright by `record_change_core`'s
    /// `path_seal_required`, so a mirror that dropped the name could never be
    /// re-homed at all. The target stores the bytes verbatim and holds no key
    /// that opens them.
    ///
    /// **No `folder_id`.** The source nest's coordinator sends one because it
    /// rides the *federated* door, whose set name is derived server-side from
    /// the authenticated `origin_nest_id`. This leg rides the client-authed
    /// door, which has no such field and takes the set name as declared —
    /// safe here because the authenticated writer is the owner aiming only at
    /// their own actor-scoped custody, and the folder id is carried by the
    /// `__folder/<nest>/<id>` name itself
    /// (`docs/goal/architecture/message-segment-store.md` § Client-device
    /// custodian (pull) → *Restore*).
    ///
    /// `path` is the leaf alone — the source row's `path_hash`, hex-spelled,
    /// which is precisely what the source nest's own folder pass records
    /// (`record_folder_custody`'s `rest_path`). Delivering the store's full
    /// local path instead would key the target's row on a hash of the *set name
    /// plus* the leaf, and the two corpora would silently disagree.
    async fn record_folder(
        &self,
        set: &str,
        leaf: &str,
        as_is: &SealedBlob,
        source_size_bytes: u64,
        path_sealed: Option<&str>,
    ) -> Result<()> {
        let sealed_bytes = path_sealed
            .map(|hex_name| {
                hex::decode(hex_name).with_context(|| {
                    format!("re-seed: mirrored path {set}/{leaf} has a malformed sealed name")
                })
            })
            .transpose()?
            .map(fauna_protocol::ByteBuf::from);

        let mut req = SyncChangeRecordRequest {
            folder: set.to_string(),
            device_id: self.device_id.clone(),
            path: leaf.to_string(),
            manifest_hash: Some(hex::encode(as_is.manifest_hash.digest())),
            // The source's declared logical size, as the mirror recorded it —
            // the target derives the charge from the bytes it holds and never
            // meters this figure, so a delivered row reads identically to the
            // one the source nest's own coordinator wrote.
            size_bytes: source_size_bytes as i64,
            change_type: "create".to_string(),
            path_sealed: sealed_bytes,
            ..Default::default()
        };
        // Signed before the address funnel takes the set's name off — the
        // nonce resolves by that name (the signature binds no address field).
        if let Some(signing) = &self.signing {
            signing.sign(&mut req).await;
        }
        req = fauna_protocol::folders::addressed(req);
        let _: SyncChangeRecordReply = self
            .nest
            .request("fauna.sync.changes.record", req)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("re-seed: recording folder custody for {set}/{leaf}"))?;
        Ok(())
    }

    /// Open one held path locally, re-seal it under the nest root, push its
    /// bytes, then record its custody.
    ///
    /// `path` is the path **within** `set` — what the custody record carries,
    /// byte-identical to a nest destination's; the store row it is read from is
    /// that path qualified by the set ([`held_path`]), and the record lands in
    /// `landing`, the kind's landing set on the target.
    async fn deliver_path(
        &self,
        set: &str,
        landing: &str,
        kind: &str,
        path: &str,
    ) -> Result<ReseedReport> {
        let held = held_path(set, path);
        let plain = self
            .store
            .open(&held, &self.local_keys)
            .await
            .with_context(|| format!("re-seed: opening held path {held}"))?;
        let plaintext_bytes = plain.len() as u64;

        // The re-seal. `None` for the content-key generation: an owner-scoped
        // backup corpus has no M2 generations, exactly as the nest coordinator's
        // own engine is built (`bins/fauna-nest/src/segment_backup.rs`).
        let sealed = seal_blob(&plain, Some((self.nest_root, None)))
            .with_context(|| format!("re-seed: sealing {path} under the nest backup root"))?;

        let mut report = self.push(&sealed).await?;
        self.record(landing, kind, path, &sealed, plaintext_bytes)
            .await?;

        report.delivered_paths.push(path.to_string());
        report.plaintext_bytes = plaintext_bytes;
        Ok(report)
    }

    /// Bytes first: dedup-check, upload what is missing, then the manifest.
    ///
    /// The manifest lands **after** its chunks so the target never holds a
    /// manifest naming bytes it does not have — the same rule the custodian
    /// store's own `put` follows locally.
    async fn push(&self, sealed: &SealedBlob) -> Result<ReseedReport> {
        push_sealed(self.bytes, sealed).await
    }

    /// Custody second — the client-authed reserved-set arm.
    ///
    /// `path_sealed` is `None`, and must be: a reserved backup destination's
    /// custody paths are machine-authored routing keys, which is exactly the
    /// class `record_change_core` exempts from the S9 seal requirement. Sending a
    /// seal here would not be safer — it would be a different path key than the
    /// one every other arm of the stack derives.
    async fn record(
        &self,
        set: &str,
        kind: &str,
        path: &str,
        sealed: &SealedBlob,
        plaintext_bytes: u64,
    ) -> Result<()> {
        record_custody(
            self.nest,
            set,
            &self.device_id,
            path,
            sealed,
            plaintext_bytes,
        )
        .await
        .with_context(|| format!("re-seed: recording custody for {kind} path {path}"))
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// The lived-in recovery's delivery leg — destination → regressed source
// ═════════════════════════════════════════════════════════════════════════════

/// The lived-in recovery's delivery leg and its seams — [`RecoveryDelivery`],
/// its two ends, the accepted-regression record, the walk — live in
/// [`fauna_client_backup::recover_leg`] since 2026-10-10, so web runs the same
/// leg (`segment-backup-protocol.md` § Client-device custodian (pull) →
/// *Restore* → *Recovery into the lived-in nest that regressed*: "web-capable
/// like the pull-back"). What stays here is what only a native host has: the
/// store end ([`RecoveryStore`], [`CustodianStoreRegressionRecord`]) and the
/// `Send` leg the driver runs natively ([`NativeRecoveryLeg`]).
pub use fauna_client_backup::recover_leg::{
    AcceptedRegressionRecord, AuditStoreRegressionRecord, DestGeneration,
    NoAcceptedRegressionRecord, NoCustodyRecorder, RecoveryDelivery, RecoveryDestination,
    RecoveryReport, RecoverySource, parse_manifest_hex, segment_set_order,
};

/// The [`AcceptedRegressionRecord`] of a **client-device custodian's own
/// store** (`segment-backup-protocol.md` § Client-device custodian (pull) →
/// *Restore* → *Recovery from this device's own store*, question (4)): the
/// store's `AuditRecord::source_regressions` is that row's record — the shell
/// folds it into the row each audit pass — so the duty clears the recovered
/// kind's entries there, the content ledger's and the journal's alike, keyed
/// as the pull keys them (the ledger's held path).
pub struct CustodianStoreRegressionRecord<'a> {
    pub store: &'a CustodianStore,
    /// The owner scope whose reserved sets were recovered.
    pub scope_id: [u8; 32],
}

#[async_trait::async_trait]
impl AcceptedRegressionRecord for CustodianStoreRegressionRecord<'_> {
    async fn clear(&self, kind: &str) -> Result<()> {
        let set = reserved_backup_set_name(kind, &self.scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no backup surface: {kind}"))?;
        let scope_hex = hex::encode(self.scope_id);
        for family in SegmentFamily::PASS_ORDER {
            if family.serve_kind(kind).is_none() {
                continue;
            }
            self.store
                .clear_source_regression(&held_path(&set, &family.mirror_path(&scope_hex, kind)))
                .await?;
        }
        Ok(())
    }
}

/// The **store end** of the recovery leg: this device's own custodian store,
/// read where it lives (`segment-backup-protocol.md` § Client-device custodian
/// (pull) → *Restore* → *Recovery from this device's own store*).
///
/// What differs from the nest end ([`RecoveryDestination`]) is only where the
/// copy is read and where the post-recovery tombstones land: the generations
/// are the store's held rows of the set, live and retained alike; each is
/// opened under the **client-only** `BackupKey` the store rests under (the
/// re-seed leg's two-key seal — [`RecoveryDelivery`] re-seals it under the
/// `NestBackupKey` root as for the nest end); and the duty tombstones in the
/// store the set's live segment paths the source's own live listing no longer
/// names. The caller is the store's one writer for the run (the re-seed's
/// one-writer rule: the agent's store-job slot, the phone's pass guard).
pub struct RecoveryStore<'a> {
    store: &'a CustodianStore,
    keys: FileDownloadKeys,
    /// Unix seconds the duty's tombstones are stamped with — the local grace
    /// clock they age out on.
    now: i64,
}

impl<'a> RecoveryStore<'a> {
    /// `local` is the store's own seal key — client-only, used here solely to
    /// open, as [`ReseedDelivery::new`] takes it.
    pub fn new(store: &'a CustodianStore, local: OwnerSealKey, now: i64) -> Self {
        Self {
            store,
            keys: FileDownloadKeys::owner(local),
            now,
        }
    }
}

#[async_trait::async_trait]
impl fauna_client_backup::recover_leg::HeldCopy for RecoveryStore<'_> {
    /// Every generation the store holds of `set`: tombstones hold no bytes and
    /// are left out; a path's live row orders after its retained ones.
    async fn generations(&self, set: &str) -> Result<Vec<DestGeneration>> {
        let rows = self.store.held().await?;
        Ok(rows
            .iter()
            .filter(|r| !r.deleted)
            .filter_map(|r| {
                let live =
                    CustodianStore::live_at(&rows, &r.path).is_some_and(|l| std::ptr::eq(l, r));
                Some(DestGeneration {
                    path: path_in_set(set, &r.path)?.to_string(),
                    manifest_hash: parse_manifest_hex(&r.manifest_hash)?,
                    order: (live, r.stored_at),
                })
            })
            .collect())
    }

    async fn read(&self, set: &str, g: &DestGeneration) -> Result<Vec<u8>> {
        self.store
            .open_generation(&held_path(set, &g.path), g.manifest_hash, &self.keys)
            .await
    }

    /// Tombstone, in the store, every live segment path of `kind`'s families in
    /// `set` whose id `named` (the source's live listing per family) does not
    /// carry.
    async fn retire_unnamed(
        &self,
        kind: &str,
        set: &str,
        scope_hex: &str,
        named: &[(SegmentFamily, std::collections::HashSet<u32>)],
    ) -> Result<Vec<String>> {
        let rows = self.store.held().await?;
        let mut live: Vec<&str> = rows
            .iter()
            .filter(|r| {
                CustodianStore::live_at(&rows, &r.path).is_some_and(|l| std::ptr::eq(l, *r))
            })
            .filter_map(|r| path_in_set(set, &r.path))
            .collect();
        live.sort_unstable();
        live.dedup();
        let mut retired = Vec::new();
        for (family, ids) in named {
            if family.serve_kind(kind).is_none() {
                continue;
            }
            for path in &live {
                let Some((id, _half)) = family.parse(scope_hex, path) else {
                    continue;
                };
                if ids.contains(&id) {
                    continue;
                }
                self.store
                    .put_tombstone(&held_path(set, path), self.now)
                    .await
                    .with_context(|| format!("recovery: retiring {path} in this device's store"))?;
                retired.push((*path).to_string());
            }
        }
        Ok(retired)
    }
}

/// The lived-in recovery's leg as the shared driver
/// (`fauna_client_backup::recover::run_recovery`) runs it **natively** — over
/// [`fauna_client::NestClient`] and its self-healing record arm, whose futures
/// are `Send` (the [`ReseedDeliveryLeg`] impl below says why that matters), so
/// the run can go on a spawned task. A newtype because the leg's generic body
/// cannot promise `Send` for an arbitrary transport, and the seam is the
/// driver's; on wasm `fauna_client_backup` impls the seam for any transport
/// itself.
///
/// `D` is the destination's record arm: [`NestRecords`] for the nest end,
/// [`NoCustodyRecorder`] for the store end.
pub struct NativeRecoveryLeg<
    'a,
    P: BlobPushSink,
    D: fauna_client_backup::recover_leg::CustodyRecorder,
>(pub RecoveryDelivery<'a, P, fauna_client::NestClient, NestRecords<'a>, D>);

/// The self-healing reserved-set arm over a native connection.
pub type NestRecords<'a> = fauna_client_sync::SyncClient<&'a fauna_client::NestClient>;

#[async_trait::async_trait]
impl<P: BlobPushSink> fauna_client_backup::recover::RecoveryLeg
    for NativeRecoveryLeg<'_, P, NestRecords<'_>>
{
    async fn recover_set(&self, set_name: &str) -> fauna_client_backup::recover::LegAnswer {
        self.0.recover_set(set_name).await
    }
}

#[async_trait::async_trait]
impl<P: BlobPushSink> fauna_client_backup::recover::RecoveryLeg
    for NativeRecoveryLeg<'_, P, NoCustodyRecorder>
{
    async fn recover_set(&self, set_name: &str) -> fauna_client_backup::recover::LegAnswer {
        self.0.recover_set(set_name).await
    }
}

impl<P: BlobPushSink, R: RpcRequester> ReseedDelivery<'_, P, R> {
    /// Phase 2 of the ceremony as the driver sees it: one
    /// [`ReseedDelivery::run_all`], projected onto the terms the owner is
    /// shown. The chain of `anyhow` context is kept in the flattened text,
    /// since it names the path that failed.
    ///
    /// Generic over any transport, where the [`ReseedDeliveryLeg`] impl below
    /// is not: that seam's future must be `Send` on native, and an
    /// async-fn-in-trait `RpcRequester::request` future is `Send` only for a
    /// concrete transport that makes it so.
    pub async fn deliver_corpus(&self) -> std::result::Result<DeliveredCorpus, String> {
        let report = self.run_all().await.map_err(|e| format!("{e:#}"))?;
        Ok(DeliveredCorpus {
            sets: report.delivered_sets,
            sidecarless_segments: report.sidecarless_segments,
            folder_paths_without_seal: report.folder_paths_without_seal,
            plaintext_bytes: report.plaintext_bytes,
        })
    }

    /// The owner's signature over every row the folder materialize arm will
    /// re-home from `set` (`writer-signed-change-records.md` ruling
    /// (7)(a)(ii)): one [`fauna_protocol::sync_writer_sig::SignedChange::for_rehome`] per live mirror row this
    /// store holds with a sealed name — the source `path_hash` (the row's
    /// leaf), the mirrored manifest, the source's declared size and sealed name
    /// verbatim, exactly what [`Self::record_folder`] delivered and the arm
    /// reads back off custody — under the nonce of the live set named by the
    /// set's display name, resolved through the host's
    /// [`fauna_client_sync::SetNonceSource`] as every other record's is.
    ///
    /// A row without a sealed name is not signed: the arm refuses the set
    /// `custody_unsealed` before it reads a signature, and the delivery already
    /// reported it. No signer, no display name, or no nonce for the target →
    /// [`FolderRehome::Unsigned`] with the reason, never an unsigned request.
    pub async fn sign_folder_rehome(&self, set: &DeliveredSet) -> FolderRehome {
        let unsigned = |reason: String| FolderRehome::Unsigned { reason };
        let (signing, nonce) = match rehome_nonce(self.signing.as_ref(), set).await {
            Ok(resolved) => resolved,
            Err(unsigned) => return unsigned,
        };
        let rows = match self.store.held().await {
            Ok(rows) => rows,
            Err(e) => return unsigned(format!("reading the store: {e:#}")),
        };
        let prefix = format!("{}/", set.set_name);
        let mut leaves: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.path.strip_prefix(&prefix))
            .filter(|leaf| is_path_hash_hex(leaf))
            .collect();
        leaves.sort_unstable();
        leaves.dedup();

        let mut rehomed = Vec::with_capacity(leaves.len());
        for leaf in leaves {
            let held = held_path(&set.set_name, leaf);
            let Some(row) = CustodianStore::live_at(&rows, &held) else {
                continue;
            };
            let Some(sealed_hex) = row.path_sealed.as_deref() else {
                continue;
            };
            let (Ok(path_hash), Ok(manifest_hash), Ok(path_sealed)) = (
                fauna_core::hex32::decode(leaf),
                fauna_core::hex32::decode(&row.manifest_hash),
                hex::decode(sealed_hex),
            ) else {
                return unsigned(format!("the mirrored row {held} is malformed in the store"));
            };
            // The statement's size is the manifest's `total_size`, read from
            // the manifest the statement already names — never the row's
            // declared `source_size_bytes`, which the target never sees (it
            // holds only its derived charge). Both halves read the one figure
            // both hold (`docs/goal/architecture/writer-signed-change-records.md`
            // ruling (7)(a)(ii), *Where the statement reads its size*).
            let manifest = match self.store.read_manifest(&row.manifest_hash).await {
                Ok(manifest) => manifest,
                Err(e) => {
                    return unsigned(format!(
                        "the mirrored row {held}'s manifest is not readable in the store: {e:#}"
                    ));
                }
            };
            rehomed.push(RehomeRow {
                path_hash,
                manifest_hash,
                total_size: manifest.total_size,
                path_sealed,
            });
        }
        sign_rehome_rows(signing, nonce, &rehomed)
    }
}

/// The production seam: the owner's own authed WS-RPC connection to the
/// target, whose request future is `Send` (`fauna_client::NestClient`'s
/// `RpcRequester` impl says so), so the ceremony can run on a spawned task.
#[async_trait::async_trait]
impl<P: BlobPushSink> ReseedDeliveryLeg for ReseedDelivery<'_, P, fauna_client::NestClient> {
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
    use crate::custodian_store::SourceFacts;
    use fauna_core::crypto::BackupKey;
    use fauna_protocol::sync_writer_sig::SignedChange;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    const SCOPE: [u8; 32] = [0xA7; 32];
    const DEVICE: &str = "1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d";

    fn local_key() -> BackupKey {
        BackupKey::from_bytes([3u8; 32])
    }

    fn nest_key() -> NestBackupKey {
        NestBackupKey::from_bytes([9u8; 32])
    }

    fn body(tag: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| tag ^ (i % 251) as u8).collect()
    }

    fn hex_of(h: &ContentHash) -> String {
        hex::encode(h.digest())
    }

    /// How the source nest's own coordinator would have sealed this corpus for a
    /// nest destination — the independent reference the byte-identity pin is
    /// measured against. It goes through the same public [`seal_blob`] the nest
    /// arm calls, under the same root, and is deliberately NOT a call into the
    /// module under test.
    fn as_the_nest_would_seal(plain: &[u8]) -> SealedBlob {
        seal_blob(plain, Some((nest_key().convergent_chunk_root(), None))).unwrap()
    }

    /// The seal a covered folder's own audience rests under on the source —
    /// a root that is **neither** of the two the delivery leg holds. A mirror
    /// pulled from such a folder is opaque to this device, which is exactly why
    /// its delivery must move the bytes without touching them.
    fn as_the_source_folder_rests(plain: &[u8]) -> SealedBlob {
        seal_blob(
            plain,
            Some((
                BackupKey::from_bytes([0x5E; 32]).convergent_chunk_root(),
                None,
            )),
        )
        .unwrap()
    }

    /// The device's local store seal — the client-only root, which is what the
    /// delivery must NOT put on the wire.
    fn as_the_device_holds_it(plain: &[u8]) -> SealedBlob {
        seal_blob(plain, Some((local_key().convergent_chunk_root(), None))).unwrap()
    }

    /// One ordered log both fakes append to, so a test can assert what happened
    /// *in what order* across the two planes — which is where this leg's
    /// crash-safety lives.
    type Log = Arc<Mutex<Vec<String>>>;

    struct FakeSink {
        log: Log,
        /// Store keys the target already holds — the dedup answer.
        already: Mutex<HashSet<ContentHash>>,
        chunks: Mutex<Vec<(ContentHash, Vec<u8>)>>,
        manifests: Mutex<Vec<Vec<u8>>>,
    }

    impl FakeSink {
        fn new(log: Log) -> Self {
            Self {
                log,
                already: Mutex::new(HashSet::new()),
                chunks: Mutex::new(Vec::new()),
                manifests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl BlobPushSink for FakeSink {
        async fn missing_chunks(&self, store_keys: &[ContentHash]) -> Result<Vec<ContentHash>> {
            let held = self.already.lock().unwrap();
            Ok(store_keys
                .iter()
                .copied()
                .filter(|k| !held.contains(k))
                .collect())
        }

        async fn put_chunk(&self, store_key: &ContentHash, body: &[u8]) -> Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("chunk {}", hex_of(store_key)));
            self.already.lock().unwrap().insert(*store_key);
            self.chunks
                .lock()
                .unwrap()
                .push((*store_key, body.to_vec()));
            Ok(())
        }

        async fn put_manifest(&self, manifest_bytes: &[u8]) -> Result<()> {
            self.log.lock().unwrap().push(format!(
                "manifest {}",
                hex_of(&ContentHash::of_raw(manifest_bytes))
            ));
            self.manifests.lock().unwrap().push(manifest_bytes.to_vec());
            Ok(())
        }
    }

    struct FakeNest {
        log: Log,
        records: Mutex<Vec<SyncChangeRecordRequest>>,
    }

    impl FakeNest {
        fn new(log: Log) -> Self {
            Self {
                log,
                records: Mutex::new(Vec::new()),
            }
        }
        fn records(&self) -> Vec<SyncChangeRecordRequest> {
            self.records.lock().unwrap().clone()
        }
    }

    impl RpcRequester for FakeNest {
        type Error = anyhow::Error;

        async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            anyhow::ensure!(
                kind == "fauna.sync.changes.record",
                "the delivery leg opened a door it has no business on: {kind}"
            );
            // Round-trip through the wire encoder so a shape break surfaces here
            // rather than as a silently-dropped field.
            let raw = serde_json::to_vec(&payload)?;
            let req: SyncChangeRecordRequest = serde_json::from_slice(&raw)?;
            self.log
                .lock()
                .unwrap()
                .push(format!("record {}", req.path));
            self.records.lock().unwrap().push(req);
            let reply = serde_json::to_vec(&SyncChangeRecordReply {
                seq: 0,
                extra: Default::default(),
            })?;
            Ok(serde_json::from_slice(&reply)?)
        }
    }

    struct Rig {
        _dir: tempfile::TempDir,
        store: CustodianStore,
        sink: FakeSink,
        nest: FakeNest,
        log: Log,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = CustodianStore::at(dir.path().join("custody"));
            let log: Log = Arc::new(Mutex::new(Vec::new()));
            Self {
                _dir: dir,
                store,
                sink: FakeSink::new(Arc::clone(&log)),
                nest: FakeNest::new(Arc::clone(&log)),
                log,
            }
        }

        /// Put one mail path (within the `__mail` set) into the local store
        /// exactly as a pull pass would: sealed under the device's own
        /// client-only root, at the set-qualified row path.
        async fn hold(&self, path: &str, plain: &[u8], stored_at: i64) {
            self.hold_in("mail", path, plain, stored_at).await;
        }

        /// [`Self::hold`], for `kind`'s set.
        async fn hold_in(&self, kind: &str, path: &str, plain: &[u8], stored_at: i64) {
            let set = reserved_backup_set_name(kind, &SCOPE).unwrap();
            self.hold_at(&held_path(&set, path), plain, stored_at).await;
        }

        /// [`Self::hold`], at a raw store row path.
        async fn hold_at(&self, held: &str, plain: &[u8], stored_at: i64) {
            self.store
                .put(
                    held,
                    &as_the_device_holds_it(plain),
                    SourceFacts {
                        size_bytes: plain.len() as u64,
                        record_count: 1,
                    },
                    stored_at,
                )
                .await
                .unwrap();
        }

        /// Put one covered-folder mirror row into the local store exactly as
        /// the pull's folder pass would: sealed under the **source folder's
        /// own** audience root — a third key this device has no business
        /// holding — and stored verbatim, with no seal step of its own.
        async fn hold_folder(
            &self,
            set: &str,
            leaf: &str,
            plain: &[u8],
            path_sealed: Option<String>,
            stored_at: i64,
        ) {
            self.store
                .put_folder_row(
                    &held_path(set, leaf),
                    &as_the_source_folder_rests(plain),
                    SourceFacts {
                        size_bytes: plain.len() as u64,
                        record_count: 1,
                    },
                    path_sealed,
                    stored_at,
                )
                .await
                .unwrap();
        }

        fn delivery(&self) -> ReseedDelivery<'_, FakeSink, FakeNest> {
            ReseedDelivery::new(
                &self.store,
                &self.sink,
                &self.nest,
                SCOPE,
                DEVICE,
                OwnerSealKey::Client(local_key()),
                &nest_key(),
            )
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    // The helpers below name paths WITHIN the `__mail` set — what the wire
    // carries; [`mail_held`] qualifies one into the store row that holds it.

    fn mail_held(path_in_set: &str) -> String {
        held_path("__mail", path_in_set)
    }

    fn seg(id: u32) -> String {
        SegmentFamily::Content.dat_path(&hex::encode(SCOPE), id)
    }

    fn meta(id: u32) -> String {
        SegmentFamily::Content.meta_path(&hex::encode(SCOPE), id)
    }

    fn mirror() -> String {
        SegmentFamily::Content.mirror_path(&hex::encode(SCOPE), "mail")
    }

    /// The journal family's three paths, the same way.
    fn jseg(id: u32) -> String {
        SegmentFamily::Placement.dat_path(&hex::encode(SCOPE), id)
    }

    fn jmeta(id: u32) -> String {
        SegmentFamily::Placement.meta_path(&hex::encode(SCOPE), id)
    }

    fn jmirror() -> String {
        SegmentFamily::Placement.mirror_path(&hex::encode(SCOPE), "mail")
    }

    // ── one store, several kinds' sets (the set-qualified store, 2026-09-29) ─

    /// **Each kind is delivered into its own set, the within-set path
    /// unchanged** — although every kind's content family spells its segments
    /// identically (`{scope_hex}/seg-00000001.dat`), so only the set tells the
    /// held rows apart. What reaches the target is the path a nest destination
    /// would carry, never the store's qualified row path.
    #[tokio::test]
    async fn each_kind_is_delivered_into_its_own_set_with_its_path_unchanged() {
        let rig = Rig::new();
        let (mail, post, calendar) = (body(0x11, 9_000), body(0x22, 7_000), body(0x33, 5_000));
        rig.hold_in("mail", &seg(1), &mail, 100).await;
        rig.hold_in("post", &seg(1), &post, 100).await;
        rig.hold_in("calendar", &seg(1), &calendar, 100).await;

        let report = rig.delivery().run_all().await.unwrap();

        assert_eq!(
            report
                .delivered_sets
                .iter()
                .map(|s| s.set_name.as_str())
                .collect::<Vec<_>>(),
            vec!["__landing/mail", "__landing/post", "__landing/calendar"],
            "one set per held kind, in the sweep's order, and none for a kind held nowhere"
        );
        assert_eq!(report.delivered_paths, vec![seg(1), seg(1), seg(1)]);
        let records = rig.nest.records();
        for (set, plain) in [
            ("__landing/mail", &mail),
            ("__landing/post", &post),
            ("__landing/calendar", &calendar),
        ] {
            let rec = records
                .iter()
                .find(|r| r.folder == set)
                .unwrap_or_else(|| panic!("nothing recorded into {set}"));
            assert_eq!(rec.path, seg(1), "{set} was recorded under another path");
            assert_eq!(
                rec.manifest_hash,
                Some(hex_of(&as_the_nest_would_seal(plain).manifest_hash)),
                "{set} carries another kind's bytes"
            );
        }
    }

    // ── the kind's other family: the placement journal (2026-09-26) ──────────

    /// **Both families are delivered, content first, into the ONE set.**
    ///
    /// The order is the contract: within a family, pairs in segment order and
    /// the mirror last; across families, all of the content before any of the
    /// journal. And the set is named once, because one materialize call flips
    /// both halves — a set named twice would be materialized twice, and the
    /// second call answers `target_not_empty`.
    #[tokio::test]
    async fn the_journal_is_delivered_after_the_content_into_the_same_set() {
        let rig = Rig::new();
        // Held out of order, and the journal first, so the order asserted below
        // is the leg's and not the store's.
        rig.hold(&jmirror(), &body(0x63, 256), 100).await;
        rig.hold(&jseg(1), &body(0x61, 2_000), 100).await;
        rig.hold(&jmeta(1), &body(0x6A, 200), 100).await;
        rig.hold(&seg(1), &body(0x11, 9_000), 100).await;
        rig.hold(&meta(1), &body(0x1A, 300), 100).await;
        rig.hold(&mirror(), &body(0x33, 512), 100).await;

        let report = rig.delivery().run_kind("mail").await.unwrap();

        assert_eq!(
            report.delivered_paths,
            vec![seg(1), meta(1), mirror(), jseg(1), jmeta(1), jmirror()],
        );
        assert!(report.sidecarless_segments.is_empty());
        assert_eq!(
            report
                .delivered_sets
                .iter()
                .map(|s| s.set_name.as_str())
                .collect::<Vec<_>>(),
            vec!["__landing/mail"],
            "one set, named once"
        );
        for record in rig.nest.records() {
            assert_eq!(
                record.folder, "__landing/mail",
                "{} was recorded into another set",
                record.path
            );
        }
    }

    /// Segment 1 of the journal is re-sealed from the JOURNAL's bytes, not from
    /// the content's segment 1 that sits beside it in the same flat store.
    #[tokio::test]
    async fn a_journal_segment_is_delivered_from_its_own_bytes() {
        let rig = Rig::new();
        let (mail, journal) = (body(0x11, 9_000), body(0x61, 2_000));
        rig.hold(&seg(1), &mail, 100).await;
        rig.hold(&jseg(1), &journal, 100).await;

        rig.delivery().run_kind("mail").await.unwrap();

        let hash_of = |path: String| {
            rig.nest
                .records()
                .iter()
                .find(|r| r.path == path)
                .and_then(|r| r.manifest_hash.clone())
                .expect("a custody record")
        };
        assert_eq!(
            hash_of(seg(1)),
            hex_of(&as_the_nest_would_seal(&mail).manifest_hash)
        );
        assert_eq!(
            hash_of(jseg(1)),
            hex_of(&as_the_nest_would_seal(&journal).manifest_hash)
        );
    }

    /// A store filled from a source with no placement segments yet (or whose
    /// journal the cap skipped) holds content only. It delivers exactly what it delivered before — the honest bound,
    /// and the case the target restores unfiled.
    #[tokio::test]
    async fn a_store_that_holds_no_journal_delivers_its_content_alone() {
        let rig = Rig::new();
        rig.hold(&seg(1), &body(0x11, 9_000), 100).await;
        rig.hold(&meta(1), &body(0x1A, 300), 100).await;
        rig.hold(&mirror(), &body(0x33, 512), 100).await;

        let report = rig.delivery().run_kind("mail").await.unwrap();

        assert_eq!(report.delivered_paths, vec![seg(1), meta(1), mirror()]);
    }

    // ── the sidecar half (2026-08-29) ────────────────────────────────────────

    /// A held pair is delivered as a pair, `.dat` before `.meta`, segment by
    /// segment, mirror last — and the report calls the corpus whole.
    #[tokio::test]
    async fn a_segments_sidecar_is_delivered_right_after_its_dat() {
        let rig = Rig::new();
        rig.hold(&seg(2), &body(0x22, 9_000), 100).await;
        rig.hold(&meta(2), &body(0x2A, 300), 100).await;
        rig.hold(&seg(1), &body(0x11, 9_000), 100).await;
        rig.hold(&meta(1), &body(0x1A, 300), 100).await;
        rig.hold(&mirror(), &body(0x33, 512), 100).await;

        let report = rig.delivery().run_kind("mail").await.unwrap();

        assert_eq!(
            report.delivered_paths,
            vec![seg(1), meta(1), seg(2), meta(2), mirror()],
            "pairs in segment order, each .meta right after its .dat, mirror last"
        );
        assert!(report.sidecarless_segments.is_empty());
        let records = rig.nest.records();
        let meta_rec = records.iter().find(|r| r.path == meta(1)).unwrap();
        assert_eq!(meta_rec.folder, "__landing/mail");
        assert_eq!(meta_rec.size_bytes, 300);
        assert!(
            meta_rec.path_sealed.is_none(),
            "a sidecar path is the same machine-authored class as its .dat"
        );
    }

    /// A store that holds a `.dat` with no sidecar (a torn custodian pull: the `.dat` put
    /// landed, the `.meta` put did not) still delivers the `.dat` — it is real custody — but says so:
    /// the target cannot reopen that segment until a later pull backfills it.
    #[tokio::test]
    async fn a_segment_held_without_its_sidecar_is_delivered_and_reported() {
        let rig = Rig::new();
        rig.hold(&seg(1), &body(0x11, 9_000), 100).await;
        rig.hold(&seg(2), &body(0x22, 9_000), 100).await;
        rig.hold(&meta(2), &body(0x2A, 300), 100).await;

        let report = rig.delivery().run_kind("mail").await.unwrap();

        assert_eq!(report.delivered_paths, vec![seg(1), seg(2), meta(2)]);
        assert_eq!(report.sidecarless_segments, vec![1]);
    }

    /// A live sidecar whose `.dat` is not live is not a segment: a compacted-out
    /// segment's orphaned or superseded sidecar must not be delivered on its
    /// own, or the target would hold a footer file naming records it has no
    /// container for.
    #[tokio::test]
    async fn a_sidecar_without_a_live_dat_is_not_delivered() {
        let rig = Rig::new();
        rig.hold(&seg(1), &body(0x11, 9_000), 100).await;
        rig.hold(&meta(1), &body(0x1A, 300), 100).await;
        rig.store
            .put_tombstone(&mail_held(&seg(1)), 300)
            .await
            .unwrap();
        rig.hold(&meta(7), &body(0x7A, 300), 100).await;

        let report = rig.delivery().run_kind("mail").await.unwrap();

        assert!(
            report.delivered_paths.is_empty(),
            "{:?}",
            report.delivered_paths
        );
        assert!(report.sidecarless_segments.is_empty());
    }

    /// **The property the whole ceremony rests on.** What the device delivers is
    /// byte-for-byte what the dead source nest's own coordinator would have
    /// pushed: same store keys, same manifest bytes, same manifest hash — and
    /// demonstrably NOT the bytes resting locally, so the re-seal really happened
    /// and the client-only `BackupKey` never crossed.
    #[tokio::test]
    async fn delivered_custody_is_byte_identical_to_the_nest_arms_own_seal() {
        let rig = Rig::new();
        let plain = body(0x5C, 40_000);
        rig.hold(&seg(1), &plain, 100).await;

        rig.delivery().run_kind("mail").await.unwrap();

        let reference = as_the_nest_would_seal(&plain);
        let pushed = rig.sink.chunks.lock().unwrap().clone();
        assert_eq!(
            pushed, reference.chunks,
            "delivered chunk store keys and bodies must equal the nest arm's own seal"
        );
        assert_eq!(
            *rig.sink.manifests.lock().unwrap(),
            vec![reference.manifest_bytes.clone()],
            "the delivered manifest bytes must equal the nest arm's own"
        );
        assert_eq!(
            rig.nest.records()[0].manifest_hash.as_deref(),
            Some(hex_of(&reference.manifest_hash).as_str()),
            "the custody row must name the nest arm's manifest hash"
        );

        // …and the local corpus is genuinely different bytes, so the assertion
        // above is a re-seal rather than a pass-through that happened to match.
        let local = as_the_device_holds_it(&plain);
        assert_ne!(
            local.manifest_hash, reference.manifest_hash,
            "the two roots must produce different artifacts, or this test proves nothing"
        );
        let local_keys: HashSet<ContentHash> = local.chunks.iter().map(|(k, _)| *k).collect();
        for (key, _) in &pushed {
            assert!(
                !local_keys.contains(key),
                "a locally-sealed chunk address reached the wire: {}",
                hex_of(key)
            );
        }
    }

    /// Crash-safety ordering, per path and across the pass: every chunk, then the
    /// manifest, then the custody record — and the `manifest.<kind>` mirror only
    /// after every segment it names has landed.
    #[tokio::test]
    async fn bytes_land_before_custody_and_the_mirror_lands_last() {
        let rig = Rig::new();
        rig.hold(&seg(1), &body(0x11, 9_000), 100).await;
        rig.hold(&seg(2), &body(0x22, 9_000), 100).await;
        rig.hold(&mirror(), &body(0x33, 512), 100).await;

        rig.delivery().run_kind("mail").await.unwrap();

        let log = rig.log();
        let record_at = |path: &str| {
            log.iter()
                .position(|l| l == &format!("record {path}"))
                .unwrap_or_else(|| panic!("no custody record for {path} in {log:?}"))
        };
        let last_byte_before = |idx: usize| {
            log[..idx]
                .iter()
                .rev()
                .find(|l| l.starts_with("manifest "))
                .is_some()
        };

        for path in [seg(1), seg(2), mirror()] {
            let at = record_at(&path);
            assert!(
                last_byte_before(at),
                "custody for {path} was recorded before its manifest landed: {log:?}"
            );
        }
        assert!(
            record_at(&mirror()) > record_at(&seg(2)),
            "the manifest.<kind> mirror must be recorded after the segments it names: {log:?}"
        );
    }

    /// A re-run after a tear converges: the target already holds every chunk, so
    /// nothing is re-uploaded — but custody is recorded again, because a record
    /// that never landed is exactly what a re-run exists to repair.
    #[tokio::test]
    async fn a_second_pass_dedups_every_chunk_and_still_records_custody() {
        let rig = Rig::new();
        rig.hold(&seg(1), &body(0x5C, 40_000), 100).await;

        let first = rig.delivery().run_kind("mail").await.unwrap();
        assert!(first.chunks_uploaded > 0);
        assert_eq!(first.chunks_deduped, 0);

        let second = rig.delivery().run_kind("mail").await.unwrap();
        assert_eq!(
            second.chunks_uploaded, 0,
            "a converged pass uploads nothing"
        );
        assert_eq!(second.chunks_deduped, first.chunks_uploaded);
        assert_eq!(second.delivered_paths, vec![seg(1)]);
        assert_eq!(rig.nest.records().len(), 2, "custody is re-recorded");
    }

    /// Only the **live** generation of a path is delivered. A superseded one is
    /// local retention the grace window governs, and a tombstoned path holds
    /// nothing — delivering either would re-home bytes the owner's own store has
    /// already moved past.
    #[tokio::test]
    async fn superseded_and_tombstoned_paths_are_not_delivered() {
        let rig = Rig::new();
        let newer = body(0xBB, 9_000);
        rig.hold(&seg(1), &body(0xAA, 9_000), 100).await;
        rig.hold(&seg(1), &newer, 200).await;
        rig.hold(&seg(2), &body(0xCC, 9_000), 100).await;
        rig.store
            .put_tombstone(&mail_held(&seg(2)), 300)
            .await
            .unwrap();

        let report = rig.delivery().run_kind("mail").await.unwrap();

        assert_eq!(report.delivered_paths, vec![seg(1)]);
        assert_eq!(
            rig.nest.records()[0].manifest_hash.as_deref(),
            Some(hex_of(&as_the_nest_would_seal(&newer).manifest_hash).as_str()),
            "the live generation, not the superseded one"
        );
    }

    /// The custody row's shape — the half a materialize verb will read back.
    /// The landing name from the one shared derivation — never the rail's,
    /// which the target mints as soon as it holds mail — no `path_sealed` (the
    /// exempt machine-authored-path class), plaintext `size_bytes`, and no key
    /// material anywhere on the wire.
    #[tokio::test]
    async fn the_custody_row_names_the_shared_landing_set_and_carries_no_seal() {
        let rig = Rig::new();
        let plain = body(0x77, 12_345);
        rig.hold(&seg(7), &plain, 100).await;

        rig.delivery().run_kind("mail").await.unwrap();

        let rec = &rig.nest.records()[0];
        assert_eq!(rec.folder, landing_set_name("mail", &SCOPE).unwrap());
        assert_eq!(rec.folder, "__landing/mail");
        assert_eq!(rec.path, seg(7));
        assert_eq!(rec.change_type, "create");
        assert_eq!(rec.device_id, DEVICE);
        assert_eq!(rec.size_bytes, plain.len() as i64);
        assert!(
            rec.path_sealed.is_none(),
            "a reserved backup destination's custody paths are the seal-exempt class"
        );
        assert!(
            rec.content_key_version.is_none(),
            "an owner-scoped backup corpus has no M2 generations"
        );
        assert!(
            rec.nest_url.is_none() && rec.channel_id.is_none(),
            "delivery is a direct owner-authed write, never a cross-nest relay"
        );
    }

    /// A store holding nothing delivers nothing and refuses nothing — the state
    /// right after enrollment, and the state a device that pulled nothing is in.
    /// It must not look like a failure, and it must not look like a delivery.
    #[tokio::test]
    async fn an_empty_store_delivers_nothing_without_erroring() {
        let rig = Rig::new();
        let report = rig.delivery().run_all().await.unwrap();
        assert_eq!(report, ReseedReport::default());
        assert!(rig.log().is_empty(), "an empty store touches no door");
    }

    /// Paths belonging to another plane are not swept into a kind's delivery: a
    /// covered-folder mirror row (which this leg deliberately does not deliver —
    /// see the module doc) and another scope's segment both stay put.
    #[tokio::test]
    async fn only_this_scopes_segment_paths_are_delivered() {
        let rig = Rig::new();
        rig.hold(&seg(1), &body(0x11, 4_000), 100).await;
        rig.hold_at("__folder/aa/7/dead", &body(0x22, 4_000), 100)
            .await;
        rig.hold(
            &SegmentFamily::Content.dat_path(&hex::encode([0x11u8; 32]), 1),
            &body(0x33, 4_000),
            100,
        )
        .await;
        rig.hold(
            &SegmentFamily::Content.meta_path(&hex::encode([0x11u8; 32]), 1),
            &body(0x3A, 300),
            100,
        )
        .await;
        // A non-canonical spelling of this scope's own segment path: the rest of
        // the stack would never address it, so neither may the delivery.
        rig.hold(
            &format!("{}/seg-1.dat", hex::encode(SCOPE)),
            &body(0x44, 4_000),
            100,
        )
        .await;

        let report = rig.delivery().run_kind("mail").await.unwrap();
        assert_eq!(report.delivered_paths, vec![seg(1)]);
    }

    /// A kind with no backup surface is a typed refusal, not a silent no-op: a
    /// caller that asked for a corpus that cannot be delivered must hear so.
    #[tokio::test]
    async fn a_kind_with_no_backup_surface_is_refused() {
        let rig = Rig::new();
        let err = rig.delivery().run_kind("nonesuch").await.unwrap_err();
        assert!(
            err.to_string().contains("no backup surface"),
            "unexpected error: {err}"
        );
    }

    // ── the covered-folder mirror plane ──────────────────────────────────────

    const SOURCE_NEST: [u8; 32] = [0xC5; 32];

    fn folder_set(folder_id: i64) -> String {
        fauna_core::data::folder_backup_set_name(&SOURCE_NEST, folder_id)
    }

    fn path_hash_hex(tag: u8) -> String {
        hex::encode([tag; 32])
    }

    /// **The property the folder plane rests on.** What the device delivers is
    /// the source's at-rest ciphertext, moved untouched: same store keys, same
    /// manifest bytes, same manifest hash as what rests locally — and
    /// demonstrably NOT what either of this leg's own two roots would produce,
    /// so no re-seal happened and no key was needed.
    #[tokio::test]
    async fn a_covered_folder_mirror_is_delivered_as_is() {
        let rig = Rig::new();
        let set = folder_set(7);
        let leaf = path_hash_hex(0x11);
        let plain = body(0x11, 9_000);
        let sealed_name = hex::encode(b"sealed-name-blob");
        rig.hold_folder(&set, &leaf, &plain, Some(sealed_name.clone()), 100)
            .await;

        let report = rig.delivery().run_covered_folders().await.unwrap();

        assert_eq!(report.delivered_paths, vec![format!("{set}/{leaf}")]);
        assert!(report.folder_paths_without_seal.is_empty());

        let held = as_the_source_folder_rests(&plain);
        let manifests = rig.sink.manifests.lock().unwrap().clone();
        assert_eq!(
            manifests,
            vec![held.manifest_bytes.clone()],
            "the mirror's manifest bytes must cross untouched"
        );
        let pushed: Vec<(ContentHash, Vec<u8>)> = rig.sink.chunks.lock().unwrap().clone();
        assert_eq!(
            pushed, held.chunks,
            "the mirror's chunk bodies must cross untouched"
        );

        // And it is genuinely as-is: neither of this leg's own roots produces
        // these bytes, so the assertion above cannot be passing by accident.
        assert_ne!(
            held.manifest_bytes,
            as_the_nest_would_seal(&plain).manifest_bytes
        );
        assert_ne!(
            held.manifest_bytes,
            as_the_device_holds_it(&plain).manifest_bytes
        );
    }

    /// The custody record's two folder-specific fields: the sealed name rides
    /// (the segment arm's is deliberately `None`), and the recorded path is the
    /// LEAF alone — the source row's `path_hash`, hex-spelled, which is exactly
    /// what the source nest's own coordinator records.
    #[tokio::test]
    async fn a_mirror_custody_record_carries_the_sealed_name_and_the_leaf_path() {
        let rig = Rig::new();
        let set = folder_set(7);
        let leaf = path_hash_hex(0x11);
        let name_blob = b"sealed-name-blob".to_vec();
        rig.hold_folder(
            &set,
            &leaf,
            &body(0x11, 4_000),
            Some(hex::encode(&name_blob)),
            100,
        )
        .await;

        rig.delivery().run_covered_folders().await.unwrap();

        let records = rig.nest.records();
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(
            rec.folder, set,
            "the set is the held `__folder/<nest>/<id>` name"
        );
        assert_eq!(
            rec.path, leaf,
            "the path is the leaf, not the store's full local path"
        );
        assert_eq!(
            rec.path_sealed.as_ref().map(|b| b.to_vec()),
            Some(name_blob),
            "the sealed name is what makes the row re-homable"
        );
        assert_eq!(rec.size_bytes, 4_000);
        assert_eq!(rec.change_type, "create");
    }

    /// **The leg signs exactly the row the folder arm will mint.** Rebuilding
    /// the re-home statement from the custody the leg recorded — as the nest
    /// reads it back — under the target's nonce verifies byte-exact against the
    /// leg's signature (`writer-signed-change-records.md` ruling (7)(a)(ii)).
    #[tokio::test]
    async fn the_rehome_signature_covers_the_delivered_custody_row() {
        let rig = Rig::new();
        let set = folder_set(7);
        let leaf = path_hash_hex(0x11);
        let plain = body(0x11, 4_000);
        let sealed = as_the_source_folder_rests(&plain);
        // The source's declared size deliberately differs from the manifest's
        // `total_size`: the statement signs the manifest's figure, the one the
        // target can read, never the declared one it cannot.
        const DECLARED: u64 = 39;
        assert_ne!(sealed.manifest.total_size, DECLARED);
        rig.store
            .put_folder_row(
                &held_path(&set, &leaf),
                &sealed,
                SourceFacts {
                    size_bytes: DECLARED,
                    record_count: 1,
                },
                Some(hex::encode(b"sealed-name-blob")),
                100,
            )
            .await
            .unwrap();
        // A nameless row is not signed: the arm refuses it before signatures.
        rig.hold_folder(&set, &path_hash_hex(0x22), &body(0x22, 10), None, 100)
            .await;

        let root = fauna_core::identity::ActorKeypair::from_secret([0x44; 32]);
        let owner = root.actor_id().0;
        let nonce = [0x6E; 32];
        let signing = |nonces: std::collections::HashMap<String, [u8; 32]>| {
            fauna_client_sync::RecordSigning {
                signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(&root)),
                set_nonce: fauna_client_sync::SetNonceSource::by_folder(nonces),
            }
        };
        let delivery = rig
            .delivery()
            .with_record_signing(signing([("Photos".to_string(), nonce)].into()));
        delivery.run_covered_folders().await.unwrap();
        let rec = rig
            .nest
            .records()
            .into_iter()
            .find(|r| r.path == leaf)
            .expect("the named row's custody");

        let target = DeliveredSet {
            set_name: set.clone(),
            axis: SetAxis::Folder,
            folder_display_name: Some("Photos".into()),
            folder_label: None,
        };
        let FolderRehome::Signed {
            signer_key,
            signatures,
        } = delivery.sign_folder_rehome(&target).await
        else {
            panic!("a host with a signer and the target's nonce signs");
        };
        assert_eq!(
            signer_key,
            owner.to_vec(),
            "a direct signer's key is the owner"
        );
        assert_eq!(signatures.len(), 1, "the nameless row is not signed");
        assert_eq!(
            signatures[0].path_hash.to_vec(),
            hex::decode(&leaf).unwrap()
        );

        // The nest's side: the statement rebuilt from custody as stored, its
        // size read from the manifest the custody row names.
        assert_eq!(rec.size_bytes, DECLARED as i64);
        let rebuilt = |size_bytes: i64| {
            SignedChange::for_rehome(
                nonce,
                owner,
                fauna_core::hex32::decode(&rec.path).unwrap(),
                fauna_core::hex32::decode(rec.manifest_hash.as_deref().unwrap()).unwrap(),
                size_bytes,
                rec.path_sealed.as_deref().unwrap(),
            )
        };
        let verify = |statement: &SignedChange| {
            fauna_protocol::sync_writer_sig::verify_statement(
                statement,
                &signatures[0].signature,
                &signer_key,
                &fauna_protocol::sync_writer_sig::SignerCertCache::new(),
                fauna_core::data::Timestamp::now(),
            )
        };
        verify(&rebuilt(sealed.manifest.total_size as i64))
            .expect("the signature covers the manifest's size, the row the arm mints");
        assert!(
            verify(&rebuilt(DECLARED as i64)).is_err(),
            "the source's declared size is not what the statement signs"
        );

        // No nonce for the target → held, with the reason.
        let no_nonce = rig
            .delivery()
            .with_record_signing(signing(Default::default()));
        assert!(matches!(
            no_nonce.sign_folder_rehome(&target).await,
            FolderRehome::Unsigned { reason } if reason.contains("Photos")
        ));
        // No signer at all → held.
        assert!(matches!(
            rig.delivery().sign_folder_rehome(&target).await,
            FolderRehome::Unsigned { .. }
        ));
    }

    /// Bytes before custody, per path — the target derives the custody charge
    /// from bytes it holds and refuses a record that runs ahead of them.
    #[tokio::test]
    async fn a_mirror_pushes_its_bytes_before_its_custody() {
        let rig = Rig::new();
        let set = folder_set(7);
        let leaf = path_hash_hex(0x11);
        rig.hold_folder(
            &set,
            &leaf,
            &body(0x11, 4_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;

        rig.delivery().run_covered_folders().await.unwrap();

        let log = rig.log();
        let record_at = log.iter().position(|l| l.starts_with("record ")).unwrap();
        let manifest_at = log.iter().position(|l| l.starts_with("manifest ")).unwrap();
        assert!(log[0].starts_with("chunk "), "chunks first: {log:?}");
        assert!(manifest_at < record_at, "manifest before custody: {log:?}");
    }

    /// A mirror row pulled before the sealed name existed still delivers — its
    /// bytes are real custody — but the report says so, because the folder
    /// materialize arm cannot re-home a row with no name.
    #[tokio::test]
    async fn a_mirror_row_without_a_sealed_name_is_delivered_and_reported() {
        let rig = Rig::new();
        let set = folder_set(7);
        let named = path_hash_hex(0x11);
        let unnamed = path_hash_hex(0x22);
        rig.hold_folder(
            &set,
            &named,
            &body(0x11, 4_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;
        rig.hold_folder(&set, &unnamed, &body(0x22, 4_000), None, 100)
            .await;

        let report = rig.delivery().run_covered_folders().await.unwrap();

        assert_eq!(
            report.delivered_paths,
            vec![format!("{set}/{named}"), format!("{set}/{unnamed}")],
            "both land; the sealed name is not a delivery precondition"
        );
        assert_eq!(
            report.folder_paths_without_seal,
            vec![format!("{set}/{unnamed}")]
        );
        let records = rig.nest.records();
        let unnamed_rec = records.iter().find(|r| r.path == unnamed).unwrap();
        assert!(unnamed_rec.path_sealed.is_none());
    }

    /// Several sets, several paths: every set is discovered from the corpus
    /// itself (a re-seed can no longer ask the dead source which folders it
    /// covered), and the walk is stable so a torn pass resumes over the same
    /// prefix.
    #[tokio::test]
    async fn every_held_folder_set_is_discovered_and_walked_in_a_stable_order() {
        let rig = Rig::new();
        let (a, b) = (folder_set(2), folder_set(11));
        rig.hold_folder(
            &b,
            &path_hash_hex(0xB2),
            &body(0xB2, 2_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;
        rig.hold_folder(
            &a,
            &path_hash_hex(0xA2),
            &body(0xA2, 2_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;
        rig.hold_folder(
            &a,
            &path_hash_hex(0xA1),
            &body(0xA1, 2_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;

        let first = rig.delivery().run_covered_folders().await.unwrap();
        assert_eq!(first.delivered_paths.len(), 3);
        let mut expected = vec![
            format!("{a}/{}", path_hash_hex(0xA1)),
            format!("{a}/{}", path_hash_hex(0xA2)),
            format!("{b}/{}", path_hash_hex(0xB2)),
        ];
        expected.sort();
        assert_eq!(first.delivered_paths, expected);
        let mut sets = vec![a.clone(), b.clone()];
        sets.sort();
        assert_eq!(
            first
                .delivered_sets
                .iter()
                .map(|s| (s.set_name.clone(), s.axis))
                .collect::<Vec<_>>(),
            sets.into_iter()
                .map(|s| (s, SetAxis::Folder))
                .collect::<Vec<_>>(),
            "each folder set named once, however many paths it holds"
        );

        // Same corpus, same order — the walk is not a hash walk.
        let rig2 = Rig::new();
        rig2.hold_folder(
            &a,
            &path_hash_hex(0xA1),
            &body(0xA1, 2_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;
        rig2.hold_folder(
            &b,
            &path_hash_hex(0xB2),
            &body(0xB2, 2_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;
        rig2.hold_folder(
            &a,
            &path_hash_hex(0xA2),
            &body(0xA2, 2_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;
        assert_eq!(
            rig2.delivery()
                .run_covered_folders()
                .await
                .unwrap()
                .delivered_paths,
            expected
        );
    }

    /// A tombstoned mirror path is not delivered: the source dropped it, and
    /// re-pushing it would re-mint custody the grace clock is retiring.
    #[tokio::test]
    async fn a_tombstoned_mirror_path_is_not_delivered() {
        let rig = Rig::new();
        let set = folder_set(7);
        let gone = path_hash_hex(0x11);
        let live = path_hash_hex(0x22);
        rig.hold_folder(
            &set,
            &gone,
            &body(0x11, 4_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;
        rig.hold_folder(
            &set,
            &live,
            &body(0x22, 4_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;
        rig.store
            .put_tombstone(&format!("{set}/{gone}"), 300)
            .await
            .unwrap();

        let report = rig.delivery().run_covered_folders().await.unwrap();
        assert_eq!(report.delivered_paths, vec![format!("{set}/{live}")]);
    }

    /// The set-name parser is the delivery door's admission rule, and it is the
    /// same one the target's owner-authed provisioning applies: a path under a
    /// name that derivation could not have produced is not a mirror row, and
    /// pushing it would strand custody in a set the target refuses to mint.
    #[tokio::test]
    async fn only_a_derivable_folder_set_name_is_delivered() {
        let rig = Rig::new();
        let leaf = path_hash_hex(0x11);
        let nest = hex::encode(SOURCE_NEST);
        for set in [
            "__folder/tooshort/1".to_string(),
            // `folders.id` is a rowid starting at 1 — the derivation renders
            // `-1` and it round-trips, which is why the parser floors it.
            format!("__folder/{nest}/-1"),
            format!("__folder/{nest}/007"),
            "__mail".to_string(),
        ] {
            rig.hold_folder(
                &set,
                &leaf,
                &body(0x11, 2_000),
                Some(hex::encode(b"n")),
                100,
            )
            .await;
        }
        // …and one that is derivable, so the test cannot pass by delivering
        // nothing at all.
        let good = folder_set(7);
        rig.hold_folder(
            &good,
            &leaf,
            &body(0x99, 2_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;

        let report = rig.delivery().run_covered_folders().await.unwrap();
        assert_eq!(report.delivered_paths, vec![format!("{good}/{leaf}")]);
    }

    /// The leaf must be a 32-byte path hash, hex-spelled and lowercase: the
    /// target re-hashes the custody path, so any other shape would key the row
    /// on something no materialize could match.
    #[tokio::test]
    async fn only_a_path_hash_shaped_leaf_is_delivered() {
        let rig = Rig::new();
        let set = folder_set(7);
        let good = path_hash_hex(0x11);
        for leaf in [
            "notahash".to_string(),
            hex::encode([0x22u8; 31]),
            // `0xAB`, not a digits-only byte: `hex::encode([0x33; 32])` is all
            // `3`s, so `.to_uppercase()` on it is a no-op and the "uppercase"
            // case would silently be a *valid* lowercase leaf. (It was, until
            // this test caught it.)
            hex::encode([0xABu8; 32]).to_uppercase(),
        ] {
            rig.hold_folder(
                &set,
                &leaf,
                &body(0x44, 2_000),
                Some(hex::encode(b"n")),
                100,
            )
            .await;
        }
        rig.hold_folder(
            &set,
            &good,
            &body(0x99, 2_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;

        let report = rig.delivery().run_covered_folders().await.unwrap();
        assert_eq!(report.delivered_paths, vec![format!("{set}/{good}")]);
    }

    /// The segment plane's own paths are not folder rows and must not be
    /// delivered twice — once re-sealed by `run_kind` and once as-is here.
    #[tokio::test]
    async fn the_folder_arm_leaves_the_segment_plane_alone() {
        let rig = Rig::new();
        rig.hold(&seg(1), &body(0x11, 9_000), 100).await;
        rig.hold(&meta(1), &body(0x1A, 300), 100).await;
        rig.hold(&mirror(), &body(0x33, 512), 100).await;

        let report = rig.delivery().run_covered_folders().await.unwrap();
        assert!(
            report.delivered_paths.is_empty(),
            "{:?}",
            report.delivered_paths
        );
        assert!(rig.nest.records().is_empty());
    }

    /// `run_all` is the whole corpus: the account rails first, then the user's
    /// covered folders — the honest bound the ceremony used to carry (reserved
    /// rails and NOT covered folders) is closed at the delivery leg.
    #[tokio::test]
    async fn run_all_delivers_both_planes_rails_first() {
        let rig = Rig::new();
        rig.hold(&seg(1), &body(0x11, 9_000), 100).await;
        rig.hold(&meta(1), &body(0x1A, 300), 100).await;
        rig.hold(&mirror(), &body(0x33, 512), 100).await;
        let set = folder_set(7);
        let leaf = path_hash_hex(0xF0);
        rig.hold_folder(
            &set,
            &leaf,
            &body(0xF0, 6_000),
            Some(hex::encode(b"n")),
            100,
        )
        .await;

        let report = rig.delivery().run_all().await.unwrap();

        assert_eq!(
            report.delivered_paths,
            vec![seg(1), meta(1), mirror(), format!("{set}/{leaf}")],
            "reserved rails first, then the covered-folder plane"
        );
        assert!(report.sidecarless_segments.is_empty());
        assert!(report.folder_paths_without_seal.is_empty());
        assert_eq!(report.manifests_uploaded, 4);
        assert_eq!(
            report.delivered_sets,
            vec![
                DeliveredSet {
                    set_name: landing_set_name("mail", &SCOPE).unwrap(),
                    axis: SetAxis::Segment,
                    folder_display_name: None,
                    folder_label: None,
                },
                DeliveredSet {
                    set_name: set.clone(),
                    axis: SetAxis::Folder,
                    folder_display_name: None,
                    folder_label: None,
                },
            ],
            "the ceremony materializes exactly the sets this pass delivered into"
        );
    }

    /// A delivered folder set carries the display name the pull recorded in
    /// this store — the driver's one source for the label the materialize arm
    /// requires — and a set the store holds no name for is delivered nameless,
    /// never under a guessed or borrowed one.
    #[tokio::test]
    async fn a_delivered_folder_set_carries_the_stores_name_for_it() {
        let rig = Rig::new();
        let (named, nameless) = (folder_set(2), folder_set(11));
        rig.store.put_folder_name(&named, "Photos").await.unwrap();
        for set in [&named, &nameless] {
            rig.hold_folder(
                set,
                &path_hash_hex(0xC1),
                &body(0xC1, 2_000),
                Some(hex::encode(b"n")),
                100,
            )
            .await;
        }

        let report = rig.delivery().run_covered_folders().await.unwrap();

        assert_eq!(
            report.delivered_sets,
            vec![
                DeliveredSet {
                    set_name: nameless.clone(),
                    axis: SetAxis::Folder,
                    folder_display_name: None,
                    folder_label: None,
                },
                DeliveredSet {
                    set_name: named.clone(),
                    axis: SetAxis::Folder,
                    folder_display_name: Some("Photos".into()),
                    folder_label: None,
                },
            ],
            "sets in lexicographic order, each with exactly the name the store holds"
        );
    }

    /// The seam the ceremony driver calls is `run_all`, projected — same sets,
    /// same gaps — so the driver can never materialize a set this leg did not
    /// deliver, nor report whole a corpus this leg reported holed.
    #[tokio::test]
    async fn the_driver_seam_projects_run_all() {
        let rig = Rig::new();
        rig.hold(&seg(1), &body(0x11, 9_000), 100).await;
        rig.hold(&mirror(), &body(0x33, 512), 100).await;

        let corpus = rig.delivery().deliver_corpus().await.unwrap();

        assert_eq!(
            corpus.sets,
            vec![DeliveredSet {
                set_name: landing_set_name("mail", &SCOPE).unwrap(),
                axis: SetAxis::Segment,
                folder_display_name: None,
                folder_label: None,
            }]
        );
        assert_eq!(
            corpus.sidecarless_segments,
            vec![1],
            "the held segment had no sidecar, and the driver must see that"
        );
    }

    /// The production transport satisfies the driver's seam, `Send` future
    /// included — a compile-time pin, since nothing in this crate calls
    /// `run_reseed` and a lost `Send` would first surface in an app.
    #[test]
    fn the_production_delivery_is_a_driver_leg() {
        fn is_leg<T: ReseedDeliveryLeg>() {}
        is_leg::<ReseedDelivery<'static, SyncClientSink<'static>, fauna_client::NestClient>>();
    }

    /// A store holding nothing names no set — phase 3 is never asked to
    /// materialize a set phase 2 left empty.
    #[tokio::test]
    async fn an_empty_store_names_no_set() {
        let rig = Rig::new();
        let corpus = rig.delivery().deliver_corpus().await.unwrap();
        assert!(corpus.sets.is_empty());
    }

    // ── the recovery's store end (`segment-backup-protocol.md` § Client-device
    //    custodian (pull) → *Restore* → *Recovery from this device's own store*)

    mod recovery_store_end {
        use super::*;
        use fauna_protocol::backup::{
            CustodyItem, CustodyListReply, CustodyRecoverReply, GenerationItem, GenerationListReply,
        };
        use fauna_protocol::segments::{SegmentRef, SegmentsListReply, SegmentsListRequest};
        use std::collections::HashMap;

        /// A fake's error: a transport fault, never a typed refusal.
        #[derive(Debug)]
        struct FakeErr(String);

        impl std::fmt::Display for FakeErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fauna_protocol::RpcErrorClass for FakeErr {
            fn is_rejection(&self) -> bool {
                false
            }
        }

        fn wire<T: serde::Serialize, U: serde::de::DeserializeOwned>(
            v: &T,
        ) -> std::result::Result<U, FakeErr> {
            serde_json::to_vec(v)
                .and_then(|raw| serde_json::from_slice(&raw))
                .map_err(|e| FakeErr(e.to_string()))
        }

        /// The regressed source: its live listing per serve tag (one before the
        /// verb, one after — the verb re-files the lost records under new
        /// ids), its record arm, and the verb.
        #[derive(Default)]
        struct Source {
            before: HashMap<String, Vec<SegmentRef>>,
            after: HashMap<String, Vec<SegmentRef>>,
            /// The post-verb listing fails: the duty must retire nothing.
            listing_fails_after: bool,
            recovered: Mutex<bool>,
            records: Mutex<Vec<SyncChangeRecordRequest>>,
        }

        impl RpcRequester for Source {
            type Error = FakeErr;

            async fn request<Req, Reply>(
                &self,
                kind: &'static str,
                payload: Req,
            ) -> std::result::Result<Reply, FakeErr>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                match kind {
                    "fauna.segments.list" => {
                        let req: SegmentsListRequest = wire(&payload)?;
                        let after = *self.recovered.lock().unwrap();
                        if after && self.listing_fails_after {
                            return Err(FakeErr("the source dropped the connection".into()));
                        }
                        let map = if after { &self.after } else { &self.before };
                        wire(&SegmentsListReply {
                            segments: map.get(&req.kind).cloned().unwrap_or_default(),
                            next_segment_id: 0,
                            extra: Default::default(),
                        })
                    }
                    "fauna.sync.changes.record" => {
                        self.records.lock().unwrap().push(wire(&payload)?);
                        wire(&SyncChangeRecordReply {
                            seq: 0,
                            extra: Default::default(),
                        })
                    }
                    "fauna.backup.custody.recover" => {
                        *self.recovered.lock().unwrap() = true;
                        wire(&CustodyRecoverReply {
                            recovered: 1,
                            ..Default::default()
                        })
                    }
                    other => Err(FakeErr(format!("the leg asked the source for {other}"))),
                }
            }
        }

        /// A nest destination holding the same corpus the store holds, as the
        /// source's own coordinator would have pushed it: live custody, the
        /// retained window, and the bytes its open routes serve.
        #[derive(Default)]
        struct Destination {
            live: Mutex<Vec<CustodyItem>>,
            retained: Mutex<Vec<GenerationItem>>,
            blobs: Mutex<HashMap<ContentHash, Vec<u8>>>,
        }

        impl Destination {
            fn hold(&self, sealed: &SealedBlob) {
                let mut blobs = self.blobs.lock().unwrap();
                blobs.insert(sealed.manifest_hash, sealed.manifest_bytes.clone());
                for (key, chunk) in &sealed.chunks {
                    blobs.insert(*key, chunk.clone());
                }
            }

            fn live(&self, path: &str, plain: &[u8], at: i64) {
                let sealed = as_the_nest_would_seal(plain);
                self.hold(&sealed);
                self.live.lock().unwrap().push(CustodyItem {
                    folder_name: "__mail".into(),
                    path: Some(path.into()),
                    path_hash: "00".repeat(32),
                    manifest_hash: hex_of(&sealed.manifest_hash),
                    updated_at: at,
                    ..Default::default()
                });
            }

            /// A live row whose copy the destination built itself, in
            /// **plaintext** — self-consistent at the address it lists, and
            /// sealed under nothing.
            fn live_plaintext(&self, path: &str, plain: &[u8], at: i64) {
                let forged = seal_blob(plain, None).unwrap();
                self.hold(&forged);
                self.live.lock().unwrap().push(CustodyItem {
                    folder_name: "__mail".into(),
                    path: Some(path.into()),
                    path_hash: "00".repeat(32),
                    manifest_hash: hex_of(&forged.manifest_hash),
                    updated_at: at,
                    ..Default::default()
                });
            }

            fn retained(&self, path: &str, plain: &[u8], superseded_at: i64) {
                let sealed = as_the_nest_would_seal(plain);
                self.hold(&sealed);
                self.retained.lock().unwrap().push(GenerationItem {
                    folder_name: "__mail".into(),
                    path: Some(path.into()),
                    path_hash: "00".repeat(32),
                    manifest_hash: hex_of(&sealed.manifest_hash),
                    superseded_at,
                    ..Default::default()
                });
            }
        }

        struct DestLink(Arc<Destination>);

        impl RpcRequester for DestLink {
            type Error = FakeErr;

            async fn request<Req, Reply>(
                &self,
                kind: &'static str,
                _payload: Req,
            ) -> std::result::Result<Reply, FakeErr>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                match kind {
                    "fauna.backup.custody.list" => wire(&CustodyListReply {
                        items: self.0.live.lock().unwrap().clone(),
                        ..Default::default()
                    }),
                    "fauna.backup.generation.list" => wire(&GenerationListReply {
                        generations: self.0.retained.lock().unwrap().clone(),
                        ..Default::default()
                    }),
                    "fauna.sync.changes.record" => wire(&SyncChangeRecordReply {
                        seq: 0,
                        extra: Default::default(),
                    }),
                    other => Err(FakeErr(format!(
                        "the leg asked the destination for {other}"
                    ))),
                }
            }
        }

        fauna_client_backup::impl_backup_nest_seam!(struct DestSeam<DestLink>);

        struct DestBytes(Arc<Destination>);

        #[async_trait::async_trait]
        impl fauna_core::file_download::BlobFetcher for DestBytes {
            async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
                self.0
                    .blobs
                    .lock()
                    .unwrap()
                    .get(hash)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("404"))
            }

            async fn fetch_chunks(
                &self,
                keys: &[ContentHash],
                _path: &str,
            ) -> Result<Vec<Vec<u8>>> {
                let blobs = self.0.blobs.lock().unwrap();
                keys.iter()
                    .map(|k| blobs.get(k).cloned().ok_or_else(|| anyhow::anyhow!("404")))
                    .collect()
            }
        }

        /// The source's self-healing record arm, as the production leg builds it.
        type SourceRecords<'a> = fauna_client_sync::SyncClient<&'a Source>;

        /// What the store end of the leg borrows: the store, and the source's
        /// record arm.
        struct StoreEnd<'a> {
            store: RecoveryStore<'a>,
            records: SourceRecords<'a>,
            rig: &'a Rig,
            source: &'a Source,
        }

        fn store_end<'a>(rig: &'a Rig, source: &'a Source) -> StoreEnd<'a> {
            StoreEnd {
                store: RecoveryStore::new(&rig.store, OwnerSealKey::Client(local_key()), 900),
                records: fauna_client_sync::SyncClient::new(source),
                rig,
                source,
            }
        }

        impl<'a> StoreEnd<'a> {
            fn leg<'b>(
                &'b self,
                regression: &'b dyn AcceptedRegressionRecord,
            ) -> RecoveryDelivery<'b, FakeSink, Source, SourceRecords<'a>, NoCustodyRecorder>
            {
                RecoveryDelivery::from_store(
                    &self.store,
                    RecoverySource {
                        bytes: &self.rig.sink,
                        nest: self.source,
                        records: &self.records,
                        device_id: DEVICE.into(),
                    },
                    SCOPE,
                    &nest_key(),
                    regression,
                )
            }
        }

        fn listed(id: u32, dat: &[u8], meta: &[u8]) -> SegmentRef {
            SegmentRef {
                segment_id: id,
                blake3_hex: hex::encode(blake3::hash(dat).as_bytes()),
                meta_blake3_hex: hex::encode(blake3::hash(meta).as_bytes()),
                ..Default::default()
            }
        }

        /// The corpus both ends hold: segment 1 whole, segment 2 whose id the
        /// source reused (a retained lost generation beside the live one), the
        /// journal's segment 1, and both ledgers. Returns the lost generation's
        /// plaintext.
        async fn hold_corpus(rig: &Rig, dest: Option<&Destination>) -> Vec<u8> {
            let lost2 = body(0x21, 3_000);
            let rows: [(String, Vec<u8>, i64); 9] = [
                (seg(1), body(0x11, 4_000), 100),
                (meta(1), body(0x12, 300), 101),
                (seg(2), lost2.clone(), 102),
                (meta(2), body(0x23, 200), 103),
                (seg(2), body(0x22, 2_500), 200),
                (jseg(1), body(0x31, 800), 104),
                (jmeta(1), body(0x32, 90), 105),
                (mirror(), body(0x41, 120), 106),
                (jmirror(), body(0x42, 60), 107),
            ];
            for (path, plain, at) in &rows {
                rig.hold(path, plain, *at).await;
            }
            if let Some(dest) = dest {
                dest.retained(&seg(2), &lost2, 200);
                for (path, plain, at) in rows.iter().filter(|(_, _, at)| *at != 102) {
                    dest.live(path, plain, *at);
                }
            }
            lost2
        }

        /// **Both ends land the same corpus the same way** — the store end
        /// differs only in where the copy is read: every generation, the
        /// retained lost one before the reused id's live one, in the shared
        /// order, re-sealed byte-identical to the nest end's landing (the same
        /// paths, the same manifest hashes, in the same sequence).
        #[tokio::test]
        async fn the_store_end_lands_what_the_nest_end_lands_in_the_shared_order() {
            let rig = Rig::new();
            let dest = Arc::new(Destination::default());
            let lost2 = hold_corpus(&rig, Some(&dest)).await;

            let from_store_source = Source::default();
            let from_store = store_end(&rig, &from_store_source)
                .leg(&NoAcceptedRegressionRecord)
                .deliver("mail", "__mail")
                .await
                .unwrap();

            let other = Rig::new();
            let from_nest_source = Source::default();
            let seam = DestSeam {
                client: fauna_client_backup::BackupClient::new(DestLink(Arc::clone(&dest))),
            };
            let link = DestLink(Arc::clone(&dest));
            let dest_records = fauna_client_sync::SyncClient::new(&link);
            let source_records = fauna_client_sync::SyncClient::new(&from_nest_source);
            let bytes = DestBytes(Arc::clone(&dest));
            let from_nest = RecoveryDelivery::new(
                RecoveryDestination {
                    seam: &seam,
                    bytes: &bytes,
                    records: &dest_records,
                    device_id: DEVICE.into(),
                },
                RecoverySource {
                    bytes: &other.sink,
                    nest: &from_nest_source,
                    records: &source_records,
                    device_id: DEVICE.into(),
                },
                SCOPE,
                &nest_key(),
                &NoAcceptedRegressionRecord,
            )
            .deliver("mail", "__mail")
            .await
            .unwrap();

            assert_eq!(from_store.delivered, from_nest.delivered);
            let paths: Vec<String> = from_store
                .delivered
                .iter()
                .map(|(p, _)| p.clone())
                .collect();
            assert_eq!(
                paths,
                [
                    seg(1),
                    meta(1),
                    seg(2),
                    seg(2),
                    meta(2),
                    jseg(1),
                    jmeta(1),
                    mirror(),
                    jmirror()
                ]
            );
            assert_eq!(
                from_store.delivered[2].1,
                hex_of(&as_the_nest_would_seal(&lost2).manifest_hash),
                "the retained lost generation lands first, under the nest root"
            );
            assert_eq!(
                from_store_source.records.lock().unwrap().len(),
                9,
                "every landed generation is recorded on the source"
            );
        }

        /// Deliver over the nest end from `dest` into a fresh source; returns
        /// the outcome, the source's records and the bytes landed on it.
        async fn deliver_from(dest: &Arc<Destination>) -> (Result<RecoveryReport>, usize, usize) {
            let target = Rig::new();
            let source = Source::default();
            let seam = DestSeam {
                client: fauna_client_backup::BackupClient::new(DestLink(Arc::clone(dest))),
            };
            let link = DestLink(Arc::clone(dest));
            let dest_records = fauna_client_sync::SyncClient::new(&link);
            let source_records = fauna_client_sync::SyncClient::new(&source);
            let bytes = DestBytes(Arc::clone(dest));
            let outcome = RecoveryDelivery::new(
                RecoveryDestination {
                    seam: &seam,
                    bytes: &bytes,
                    records: &dest_records,
                    device_id: DEVICE.into(),
                },
                RecoverySource {
                    bytes: &target.sink,
                    nest: &source,
                    records: &source_records,
                    device_id: DEVICE.into(),
                },
                SCOPE,
                &nest_key(),
                &NoAcceptedRegressionRecord,
            )
            .deliver("mail", "__mail")
            .await;
            let records = source.records.lock().unwrap().len();
            let landed = target.sink.chunks.lock().unwrap().len()
                + target.sink.manifests.lock().unwrap().len();
            (outcome, records, landed)
        }

        fn assert_plaintext_refused(outcome: &Result<RecoveryReport>) {
            let Err(err) = outcome else {
                panic!("a plaintext copy must be refused");
            };
            assert_eq!(
                fauna_core::apply_failure::permanent_reason(err),
                Some(fauna_core::apply_failure::PermanentApplyFailure::PLAINTEXT_REFUSED.reason),
                "expected the typed sealed-only refusal, got: {err:#}"
            );
        }

        /// The leg reseals what it opens under the owner's
        /// `nest_root`, so a destination serving a self-built **plaintext**
        /// record at the address it lists would be laundered into an owner
        /// seal on the source. The nest end is a sealed-only reader: the
        /// record is refused, loudly, and nothing lands.
        #[tokio::test]
        async fn the_nest_end_refuses_a_plaintext_record_and_lands_nothing() {
            let dest = Arc::new(Destination::default());
            dest.live_plaintext(&seg(1), &body(0x11, 4_000), 100);
            dest.live(&mirror(), &body(0x41, 120), 106);
            let (outcome, records, landed) = deliver_from(&dest).await;
            assert_plaintext_refused(&outcome);
            assert_eq!((records, landed), (0, 0), "nothing lands on the source");
        }

        /// The same for the set's ledger: a plaintext `manifest.<kind>` the
        /// destination built is refused, never resealed onto the source.
        #[tokio::test]
        async fn the_nest_end_refuses_a_plaintext_ledger_and_lands_nothing() {
            let dest = Arc::new(Destination::default());
            dest.live_plaintext(&mirror(), &body(0x41, 120), 106);
            let (outcome, records, landed) = deliver_from(&dest).await;
            assert_plaintext_refused(&outcome);
            assert_eq!((records, landed), (0, 0), "nothing lands on the source");
        }

        /// **The skip:** a half the source already holds byte-identical (its
        /// own `fauna.segments.list`) is not landed — the verb reads the
        /// source's own copy.
        #[tokio::test]
        async fn the_store_end_skips_what_the_source_already_holds() {
            let rig = Rig::new();
            hold_corpus(&rig, None).await;
            let tag = SegmentFamily::Content
                .serve_kind("mail")
                .unwrap()
                .to_string();
            let source = Source {
                before: HashMap::from([(
                    tag,
                    vec![listed(1, &body(0x11, 4_000), &body(0x12, 300))],
                )]),
                ..Default::default()
            };
            let report = store_end(&rig, &source)
                .leg(&NoAcceptedRegressionRecord)
                .deliver("mail", "__mail")
                .await
                .unwrap();
            assert_eq!(report.skipped, [seg(1), meta(1)]);
            assert!(
                !report
                    .delivered
                    .iter()
                    .any(|(p, _)| *p == seg(1) || *p == meta(1)),
                "a held half is never landed"
            );
        }

        /// **The duties are store-backed:** after the whole reply the store's
        /// entries for the recovered kind clear — both families', nothing of
        /// another kind's — and the set's live segment paths the source's own
        /// post-verb listing does not name are tombstoned in the store, the
        /// ledgers and the named paths left live.
        #[tokio::test]
        async fn the_duties_tombstone_exactly_the_unnamed_live_paths_and_clear_the_entries() {
            let rig = Rig::new();
            hold_corpus(&rig, None).await;
            let content = SegmentFamily::Content
                .serve_kind("mail")
                .unwrap()
                .to_string();
            let journal = SegmentFamily::Placement
                .serve_kind("mail")
                .unwrap()
                .to_string();
            let source = Source {
                after: HashMap::from([
                    (content, vec![listed(1, b"x", b"y"), listed(7, b"z", b"w")]),
                    (journal, vec![listed(1, b"x", b"y")]),
                ]),
                ..Default::default()
            };
            let post_ledger = held_path(
                "__post",
                &SegmentFamily::Content.mirror_path(&hex::encode(SCOPE), "post"),
            );
            for ledger in [
                mail_held(&mirror()),
                mail_held(&jmirror()),
                post_ledger.clone(),
            ] {
                rig.store
                    .record_source_regression(&ledger, 9, 0, 50)
                    .await
                    .unwrap();
            }

            let record = CustodianStoreRegressionRecord {
                store: &rig.store,
                scope_id: SCOPE,
            };
            let report = store_end(&rig, &source)
                .leg(&record)
                .run_kind("mail")
                .await
                .unwrap();

            assert_eq!(report.tombstoned_at_destination, [seg(2), meta(2)]);
            let rows = rig.store.held().await.unwrap();
            for gone in [seg(2), meta(2)] {
                assert!(CustodianStore::live_at(&rows, &mail_held(&gone)).is_none());
            }
            for kept in [seg(1), meta(1), jseg(1), jmeta(1), mirror(), jmirror()] {
                assert!(
                    CustodianStore::live_at(&rows, &mail_held(&kept)).is_some(),
                    "{kept} stays live"
                );
            }
            let standing: Vec<String> = rig
                .store
                .audit_record()
                .await
                .source_regressions
                .into_keys()
                .collect();
            assert_eq!(
                standing,
                [post_ledger],
                "only the recovered kind's entries clear"
            );
        }

        /// **A listing the duty cannot read retires nothing** — a family read
        /// as empty would tombstone the whole held copy of it.
        #[tokio::test]
        async fn a_source_listing_that_fails_after_the_verb_retires_nothing() {
            let rig = Rig::new();
            hold_corpus(&rig, None).await;
            let source = Source {
                listing_fails_after: true,
                ..Default::default()
            };
            let record = CustodianStoreRegressionRecord {
                store: &rig.store,
                scope_id: SCOPE,
            };
            assert!(
                store_end(&rig, &source)
                    .leg(&record)
                    .run_kind("mail")
                    .await
                    .is_err()
            );
            let rows = rig.store.held().await.unwrap();
            for kept in [seg(1), meta(1), seg(2), meta(2), jseg(1), jmeta(1)] {
                assert!(CustodianStore::live_at(&rows, &mail_held(&kept)).is_some());
            }
        }
    }
}
