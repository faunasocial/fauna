//! Garbage collection: delete blobs not referenced by any live snapshot.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_core::data::ContentHash;

use crate::blob_store::BlobStoreBackend;
use crate::db::CacheDb;

/// Where the reference walk reads live **post bodies** from — the author's
/// `__post` segment store plus the inline `content.payload` fallback
/// [`crate::segments::post::load_post_body`] resolves through (profiles need
/// no source: they rest inline in `content`). Every blob-deleting caller
/// passes the box's real one; there is deliberately no "no posts" variant,
/// because a walk that skips this source deletes every live post's media —
/// the 2026-09-07 finding step 2f of [`collect_reachable_hashes`] closes.
#[derive(Clone, Copy)]
pub struct PostBodySource<'a> {
    pub segments: &'a fauna_segment_store::SegmentManager,
}

/// Result of a garbage collection run.
#[derive(Debug, Default)]
pub struct GcResult {
    pub dry_run: bool,
    pub deleted_blobs: u64,
    pub deleted_bytes: u64,
    pub live_snapshots: usize,
    pub referenced_manifests: usize,
    pub skipped_grace_period: u64,
    /// References classified as direct-blob rail rows (reserved `__` sets):
    /// pinned as blobs, never decoded as manifests.
    pub direct_blob_refs: usize,
    /// Distinct ATProto PDS uploaded blobs pinned by an `atproto_blobs` row
    /// (F2.4). Chunkless by construction, so pinned without the direct-blob
    /// probe; see the walk's step 2e for why row existence is the whole
    /// predicate.
    pub atproto_blob_refs: usize,
    /// `ChunkManifest`-class references that failed to decode. Any non-zero
    /// count **fail-closes the sweep**: reachability is incomputable (the
    /// failed manifest's chunks cannot be enumerated), so deleting anything
    /// could destroy live backup data — the delete phase is skipped entirely
    /// for this run (`docs/goal/behavior/backup-restore.md` § 9).
    pub manifest_decode_failures: u64,
    /// `ChunkManifest`-class references whose blob is hash-INTACT under its
    /// content address but does not decode as a manifest: the bytes provably
    /// never were one, so no chunk was ever enumerable through the reference
    /// and it must NOT fail-close the sweep (`backup-restore.md` § 9 — the
    /// blob itself stays pinned; over-pinning is the safe direction). A junk
    /// or mis-keyed record, surfaced per-reference at warn level.
    pub never_manifest_refs: u64,
    /// Retained superseded custody generations whose grace window `T` elapsed
    /// this run: their rows were dropped and their bytes credited back to the
    /// owner's quota, so their now-exclusive chunks are sweepable from here on.
    pub expired_custody_generations: u64,
    /// Distinct blobs pinned by a live signed record — a post's media items,
    /// thumbnails and video parts, a gated post's sealed body and plaintext
    /// `attachment_refs`, a profile's avatar and banner (step 2f). Chunkless
    /// `POST /api/v1/blob` uploads, pinned by record existence alone.
    pub record_blob_refs: usize,
    /// Post bodies that could not be READ this run. Non-zero **fail-closes
    /// the sweep** exactly like an unreadable manifest: that post's blobs
    /// could not be enumerated, so the reference set is provably incomplete.
    /// Transient by nature (a store read error), heals on the next run.
    pub record_read_failures: u64,
    /// Stored posts or profiles that were read but do not decode. NOT
    /// fail-close: every writer decodes before it stores, so the only such
    /// rows are the documented degenerate inline fallback, which never named
    /// a blob this nest could serve — and a permanent nest-wide fail-close on
    /// one such row would be the client-triggerable stall the manifest arm's
    /// hash-intact split exists to avoid. Warned per record.
    pub undecodable_records: u64,
    /// Distinct blobs pinned by a live conversation record's plaintext
    /// `attachment_refs` (step 2g) — the sealed attachments a sealed body
    /// names, pinnable only because the sender lists their hashes beside the
    /// envelope (`encryption-at-rest.md` § Per-content-kind conformance →
    /// Conversation messages row, 2026-09-08). Chunkless `POST /api/v1/blob`
    /// uploads, pinned by the record's liveness alone.
    pub conv_attachment_refs: usize,
    /// Distinct blobs pinned by a rendered site (step 2h) — the live pages of
    /// both rendered stores plus the bodies a render still holding its site
    /// has staged. Chunkless render output, pinned by row existence alone.
    pub web_rendered_refs: usize,
    /// Distinct blobs pinned as a live `content` row's spilled payload (step
    /// 2i) — an inbox envelope over the payload store's threshold.
    /// Pinned by the row's existence alone.
    pub content_payload_refs: usize,
}

/// **T** — the destination-side custody grace window, the protocol's own
/// constant re-exported under the name this crate's reclaim sweep, handlers
/// and tests have always used. It moved to the shared protocol crate on
/// 2026-09-29 because the client audit's vanished-ledger rule floors the
/// wire-reported window at the same value (`fauna_protocol::backup` owns the
/// rationale).
pub use fauna_protocol::backup::BACKUP_CUSTODY_GRACE_SECS;

/// Decode a stored blob as a [`fauna_core::chunk::ChunkManifest`].
///
/// `decode_blob` (decrypt + decompress) then deserialize.
///
/// This is both the GC's manifest reader **and** its classifier: an `Err` means
/// "these bytes are not a chunk manifest", which is exactly what a direct-blob
/// rail reference (a client-sealed payload, raw segment bytes) yields.
pub(crate) fn decode_manifest(
    raw_bytes: &[u8],
    encryption_key: Option<&fauna_core::crypto::BackupKey>,
) -> anyhow::Result<fauna_core::chunk::ChunkManifest> {
    super::decode_blob(raw_bytes, encryption_key).and_then(|p| {
        fauna_core::encoding::canonical_decode(&p).map_err(|e| anyhow::anyhow!("{e}"))
    })
}

/// Content-address integrity: are these stored bytes an authentic preimage of
/// the reference hash under some legitimate write layer?
///
/// Every production writer into this store keys a blob by a server-computed
/// (or server-verified) BLAKE3 of its bytes at one of two layers: the framed
/// layer — chunk/manifest/mail writers store `encode_blob(body)` keyed by
/// `blake3(body)` — or the raw layer — the rail/blob/video/payload writers
/// store the bytes verbatim keyed by `blake3(stored)` (with an at-rest
/// `encryption_key`, the raw layer is the decrypt-only layer). So for an intact blob at
/// least one layer re-hashes to its key, and — because a content address is
/// immutable — bytes that are hash-intact yet fail to decode as a
/// `ChunkManifest` provably NEVER decoded as one at any point in their
/// existence. A reference to such a blob never had an enumerable chunk set,
/// so there is nothing for the fail-closed sweep to protect. Only a mismatch
/// at every layer means the bytes are not what was written — genuine at-rest
/// corruption, the case the fail-close exists for.
///
/// The framed layer decodes at `decode_blob`'s bound (`MAX_DECODED_BLOB`), so a
/// raw-stored decompression bomb a live post names is refused there rather than
/// expanded inside the sweep, and answers from the raw layer like any other
/// raw-stored blob. No framed writer produces a body above the bound (the pins
/// beside the constant), so the verdict for every real blob is unchanged.
fn stored_bytes_match_reference(
    reference: &[u8; 32],
    raw_bytes: &[u8],
    encryption_key: Option<&fauna_core::crypto::BackupKey>,
) -> bool {
    if let Ok(body) = super::decode_blob(raw_bytes, encryption_key)
        && ContentHash::of_raw(&body).digest() == *reference
    {
        return true;
    }
    match encryption_key {
        None => ContentHash::of_raw(raw_bytes).digest() == *reference,
        Some(key) => fauna_core::crypto::decrypt_backup_chunk(key, raw_bytes)
            .map(|d| ContentHash::of_raw(&d).digest() == *reference)
            .unwrap_or(false),
    }
}

/// Run garbage collection: walk all live snapshots, collect referenced
/// manifest hashes, then delete any blobs not referenced by any snapshot.
///
/// Acquires an advisory lock to prevent concurrent GC runs.
pub async fn garbage_collect(
    db: &Arc<CacheDb>,
    blob_store: &Arc<dyn BlobStoreBackend>,
    records: PostBodySource<'_>,
    grace_period_secs: i64,
    encryption_key: Option<&fauna_core::crypto::BackupKey>,
    dry_run: bool,
) -> Result<GcResult> {
    let holder = format!("gc-{}", std::process::id());
    if !db.try_acquire_op_lock("gc", -1, &holder).await? {
        anyhow::bail!("cannot acquire GC lock — another GC is running");
    }

    let result = do_gc(
        db,
        blob_store,
        records,
        grace_period_secs,
        encryption_key,
        dry_run,
    )
    .await;

    // Always release lock
    if let Err(e) = db.release_op_lock("gc", -1).await {
        tracing::warn!("failed to release GC lock: {e}");
    }

    result
}

/// The box-wide reachable-hash set: every top-level reference (live-snapshot
/// files, live/grace-pinned sync changes, backup custody) **plus the chunk
/// store keys inside every decodable `ChunkManifest`-class reference**.
///
/// **This is THE single reachability oracle.** Every blob-deleting caller
/// sweeps against it — a second, hand-rolled oracle that diverges from this
/// walk is a data-loss bug waiting to happen, demonstrated by the one other
/// consumer this ever had: the `__index` boot purge, whose original
/// column-only keep-check missed chunk-level references and which is itself deleted as of 2026-08-02 (`__index` is now
/// a user at-rest store — `content-index.md` § Before Plan 5b). A caller that
/// deletes blobs MUST fail-close (delete nothing) when
/// `manifest_decode_failures > 0`: the set is provably incomplete.
pub(crate) struct ReachableHashes {
    /// Every reachable blob hash (top-level references + chunk store keys).
    pub referenced: HashSet<Vec<u8>>,
    /// `ChunkManifest`-class references whose chunks could not be enumerated
    /// (decode or store-read failure). Non-zero ⇒ reachability incomputable.
    pub manifest_decode_failures: u64,
    /// Hash-intact references that provably never were manifests (see
    /// [`GcResult::never_manifest_refs`]): pinned, walk-free, NOT fail-close.
    pub never_manifest_refs: u64,
    /// References confirmed (by bytes) as direct-blob rail rows.
    pub direct_blob_refs: usize,
    /// Distinct `atproto_blobs.media_ref` values pinned (F2.4): chunkless
    /// uploaded media, pinned by row existence alone.
    pub atproto_blob_refs: usize,
    /// Distinct top-level references collected from live snapshots.
    pub referenced_manifests: usize,
    /// Live snapshots walked.
    pub live_snapshots: usize,
    /// Distinct blobs pinned by live signed records (posts, profiles) — step
    /// 2f. Chunkless, pinned by record existence alone.
    pub record_blob_refs: usize,
    /// Post bodies that could not be read: their blobs could not be
    /// enumerated. Non-zero ⇒ reachability incomputable (fail-close).
    pub record_read_failures: u64,
    /// Records read but undecodable: warned, nothing pinned, NOT fail-close
    /// (see [`GcResult::undecodable_records`]).
    pub undecodable_records: u64,
    /// Distinct blobs pinned by live conversation records' plaintext
    /// `attachment_refs` — step 2g (see [`GcResult::conv_attachment_refs`]).
    pub conv_attachment_refs: usize,
    /// Distinct blobs pinned by rendered sites — step 2h (see
    /// [`GcResult::web_rendered_refs`]).
    pub web_rendered_refs: usize,
    /// Distinct blobs pinned as live content rows' spilled payloads — step 2i
    /// (see [`GcResult::content_payload_refs`]).
    pub content_payload_refs: usize,
    /// The legal-takedown blob-serve withhold, folded from the SAME steps 2f
    /// and 2g walks that pin: each live record's blobs partitioned by whether
    /// that record is under a takedown
    /// ([`crate::moderation_withhold::WithholdSets`]). Riding this walk is
    /// what keeps the withheld set true as records come and go, at the cost
    /// of one small set per record and no second decode pass. Complete only
    /// when `record_read_failures` is zero — a partial walk must not replace
    /// the stored set (step 4c).
    pub withhold: crate::moderation_withhold::WithholdSets,
}

async fn do_gc(
    db: &Arc<CacheDb>,
    blob_store: &Arc<dyn BlobStoreBackend>,
    records: PostBodySource<'_>,
    grace_period_secs: i64,
    encryption_key: Option<&fauna_core::crypto::BackupKey>,
    dry_run: bool,
) -> Result<GcResult> {
    let mut result = GcResult {
        dry_run,
        ..Default::default()
    };

    // 0. Expire retained custody generations past the destination grace window T
    // — BEFORE the reference set is built, never after. `collect_reachable_hashes`
    // pins every generation row that still exists, so reclaiming first means a
    // generation is either protected or gone, with no cycle in which it is
    // neither. Reversing the order would open exactly the over-delete window the
    // no-user-data-loss invariant forbids. A dry run reclaims nothing.
    if !dry_run {
        let cutoff = crate::db::now_epoch_secs() - BACKUP_CUSTODY_GRACE_SECS;
        let expired = db
            .reclaim_expired_backup_custody_generations(cutoff)
            .await
            .context("reclaiming expired backup custody generations")?;
        if expired > 0 {
            tracing::info!(
                expired_generations = expired,
                grace_secs = BACKUP_CUSTODY_GRACE_SECS,
                "reclaimed superseded backup custody generations past the grace window"
            );
        }
        result.expired_custody_generations = expired;
    }

    let reach = collect_reachable_hashes(
        db,
        blob_store,
        records,
        grace_period_secs,
        encryption_key,
        &[],
    )
    .await?;
    result.live_snapshots = reach.live_snapshots;
    result.referenced_manifests = reach.referenced_manifests;
    result.direct_blob_refs = reach.direct_blob_refs;
    result.atproto_blob_refs = reach.atproto_blob_refs;
    result.manifest_decode_failures = reach.manifest_decode_failures;
    result.never_manifest_refs = reach.never_manifest_refs;
    result.record_blob_refs = reach.record_blob_refs;
    result.record_read_failures = reach.record_read_failures;
    result.undecodable_records = reach.undecodable_records;
    result.conv_attachment_refs = reach.conv_attachment_refs;
    result.web_rendered_refs = reach.web_rendered_refs;
    result.content_payload_refs = reach.content_payload_refs;
    // 4c. Rebuild the legal-takedown blob-serve withhold. The takedown/restore
    // handler rebuilds synchronously so the door is right the instant a flag
    // moves (`crate::moderation_withhold`); this leg is the reconciler that
    // keeps it right afterwards, as records are stored and removed — a new
    // record naming a withheld blob releases it, and the removal of the last
    // unflagged record naming a shared one withholds it.
    //
    // Whether it may RELEASE turns on the walk's completeness: a post whose
    // body could not be read named blobs this run
    // could not enumerate, and releasing on that basis could re-serve a blob
    // whose only remaining namer is exactly that unread record. So a partial
    // walk only adds (`WithholdSets::store`), and a complete one replaces.
    // A dry run still rebuilds: it is an observation of the store as found,
    // and it deletes nothing — withheld is not deleted, and
    // the GC pin above is deliberately untouched by the flag.
    let withheld_blobs = reach
        .withhold
        .store(db, reach.record_read_failures == 0)
        .await
        .context("persisting the legal-takedown blob withhold set")?;
    if withheld_blobs > 0 {
        tracing::info!(
            withheld_blobs,
            "blobs withheld from the blob-serve door by a legal takedown \
             (moderation.md § Legal takedown)"
        );
    }
    let referenced = reach.referenced;

    // 3b. Fail-closed gate: if any ChunkManifest-class reference failed to
    // decode, the reference set is provably incomplete — sweeping against it
    // could delete live chunks of the very data the walk failed to enumerate
    // (a no-user-data-loss violation). Skip the delete phase for this run;
    // the failure is surfaced at error level per manifest above, and the
    // disk-fill counter-risk is independently backstopped by the blob-store
    // min-free guard (`docs/goal/behavior/backup-restore.md` § 12). Direct-blob
    // rail references never trip this — they are excluded from the decode walk
    // by classification, so steady-state runs sweep normally.
    if result.manifest_decode_failures > 0 || result.record_read_failures > 0 {
        tracing::error!(
            manifest_decode_failures = result.manifest_decode_failures,
            record_read_failures = result.record_read_failures,
            "GC fail-closed: undecodable ChunkManifest reference(s) or unreadable post \
             bod(ies) — skipping the delete phase (no blob deleted this run)"
        );
        return Ok(result);
    }

    do_gc_delete_phase(
        db,
        blob_store,
        grace_period_secs,
        dry_run,
        referenced,
        result,
    )
    .await
}

/// The phase-5 **flip-time chunk drop** (`file-sync.md` § Content residency):
/// delete the nest-held chunk bytes of one folder whose owner just consented
/// to metadata-only residency — R10 (account-data-plane.md § The ratified decisions)'s consent arm, applied now rather than on
/// the GC cadence, because the confirm the owner answered names a deletion
/// that happens *now*.
///
/// Runs AFTER the `nest_content_residency` column committed (the single
/// atomic decision point): the reachability oracle above therefore already
/// classifies this folder's manifests pin-only, so its chunk store keys are
/// deletable exactly when **no full-residency source** also references them —
/// content-addressed dedup safety for free, through the same single oracle
/// every blob-deleting caller uses. Manifest blobs are untouched (they ARE
/// the metadata the folder keeps); `blob_metadata` rows go with their bytes.
///
/// Fail-closed like the GC's own sweep: any undecodable manifest reference
/// makes reachability incomputable and the pass deletes nothing. A failed or
/// interrupted pass needs no repair state — the committed column is the one
/// signal, and the GC's residency arm reclaims whatever this pass left, on
/// its next cycle (the boot-reconcile shape).
pub async fn drop_folder_chunk_bytes(
    db: &Arc<CacheDb>,
    blob_store: &Arc<dyn BlobStoreBackend>,
    records: PostBodySource<'_>,
    grace_period_secs: i64,
    encryption_key: Option<&fauna_core::crypto::BackupKey>,
    folder_id: i64,
) -> Result<usize> {
    // The folder's own manifest hashes — live and superseded sync-change
    // generations plus its snapshots' files. These are what the drop walks;
    // they are NOT what it deletes.
    let mut own_manifests = db
        .all_sync_change_manifest_hashes_for_folders(&[folder_id])
        .await
        .context("collecting the folder's sync-change manifest hashes")?;
    let own_snapshot_ids = db
        .list_snapshot_ids_for_folders(&[folder_id])
        .await
        .context("collecting the folder's snapshot ids")?;
    own_manifests.extend(
        db.snapshot_manifest_refs(&own_snapshot_ids)
            .await
            .context("collecting the folder's snapshot manifest hashes")?
            .into_iter()
            .map(|(mh, _)| mh),
    );
    if own_manifests.is_empty() {
        return Ok(0);
    }

    // Everything still reachable — post-flip, so this folder's chunks are
    // already excluded, and anything a full-residency source shares stays.
    let reach = collect_reachable_hashes(
        db,
        blob_store,
        records,
        grace_period_secs,
        encryption_key,
        &[],
    )
    .await
    .context("collecting the reachable set for the residency drop")?;
    if reach.manifest_decode_failures > 0 || reach.record_read_failures > 0 {
        anyhow::bail!(
            "residency drop fail-closed: {} undecodable ChunkManifest reference(s), {} \
             unreadable post bod(ies) — reachability incomputable, nothing deleted (the \
             GC's residency arm retries)",
            reach.manifest_decode_failures,
            reach.record_read_failures
        );
    }

    // Walk the folder's manifests for chunk store keys and delete every one
    // nothing else reaches. The manifest fetch is by content address from our
    // own store; one that is absent (a metadata-only-born folder whose seat
    // never uploaded bytes) or undecodable as a manifest simply contributes
    // no keys.
    let mut candidates: HashSet<Vec<u8>> = HashSet::new();
    for mh in &own_manifests {
        let Ok(hash_arr) = <[u8; 32]>::try_from(mh.as_slice()) else {
            continue;
        };
        let content_hash = ContentHash::from_digest_raw(hash_arr);
        let Ok(Some(raw_bytes)) = blob_store.get(&content_hash).await else {
            continue;
        };
        let Ok(manifest) = decode_manifest(&raw_bytes, encryption_key) else {
            continue; // a rail blob / non-manifest reference — nothing to walk
        };
        for key in &manifest.store_keys() {
            let digest = key.digest().to_vec();
            if !reach.referenced.contains(&digest) {
                candidates.insert(digest);
            }
        }
    }

    let mut deleted: Vec<Vec<u8>> = Vec::new();
    for key in &candidates {
        let Ok(hash_arr) = <[u8; 32]>::try_from(key.as_slice()) else {
            continue;
        };
        let hash = ContentHash::from_digest_raw(hash_arr);
        // Already absent (a residency-aware seat never uploaded it, or an
        // earlier pass dropped it) — nothing to do, and the count stays an
        // honest bytes-removed number so a repeat pass reports 0.
        if !blob_store.exists(&hash).await.unwrap_or(true) {
            continue;
        }
        match blob_store.delete(&hash).await {
            Ok(()) => deleted.push(key.clone()),
            Err(e) => tracing::warn!(
                hash = hex::encode(hash_arr),
                error = %e,
                "residency drop: blob delete failed (the GC's residency arm retries)"
            ),
        }
    }
    db.delete_blob_metadata_batch(&deleted)
        .await
        .context("deleting dropped chunks' metadata rows")?;
    Ok(deleted.len())
}

/// Steps 1–3 of the GC — collect and expand every reference into the
/// reachable-hash set ([`ReachableHashes`]). Shared with the `__index` boot
/// purge so there is exactly one reachability oracle.
/// `exclude_sync_folder_ids` drops those sets' `sync_changes` rows from the
/// top-level reference scan — the `__index` purge passes its own sets (their
/// journal rows reference exactly the blobs it deletes, and supersession is
/// an explicit column the tombstones do not set, so without the exclusion the
/// purge would pin its own delete-set forever); the GC passes none.
pub(crate) async fn collect_reachable_hashes(
    db: &Arc<CacheDb>,
    blob_store: &Arc<dyn BlobStoreBackend>,
    records: PostBodySource<'_>,
    grace_period_secs: i64,
    encryption_key: Option<&fauna_core::crypto::BackupKey>,
    exclude_sync_folder_ids: &[i64],
) -> Result<ReachableHashes> {
    let mut manifest_decode_failures: u64 = 0;
    let mut never_manifest_refs: u64 = 0;
    let mut direct_blob_refs: usize = 0;

    // Phase 5 residency (`file-sync.md` § Content residency): a
    // metadata-only folder's manifests stay pinned — they ARE the metadata
    // the folder keeps — but their chunk store keys are deliberately NOT
    // reachable through them: absent bytes are that folder's contract, an
    // old seat's uploaded bytes must reclaim, and the flip-time drop's
    // residue must not re-pin. The partition is by SOURCE, so a hash a
    // full-residency folder also references keeps chunk-walking through that
    // other source — content-addressed dedup must never lose a full folder's
    // chunks to a metadata-only neighbour.
    let meta_folder_ids = db
        .metadata_only_folder_ids()
        .await
        .context("collecting metadata-only folder ids")?;

    // 1. Collect all snapshot IDs across all folders
    let all_snapshot_ids = db
        .list_all_snapshot_ids()
        .await
        .context("listing snapshot IDs")?;
    let live_snapshots = all_snapshot_ids.len();
    if all_snapshot_ids.is_empty() {
        tracing::info!("no snapshots found, proceeding to clean unreferenced blobs");
    }

    // 2. Collect all referenced manifest hashes from live snapshots,
    // classified: a reference from a reserved (`__`) rail set is the stored
    // blob itself (a client-sealed `__drafts`/`__mls` blob or
    // `__index`/`__conv` segment bytes — never a decodable `ChunkManifest`),
    // so it is pinned as a blob and excluded from the manifest walk below.
    // The snapshot half of the residency partition: a metadata-only
    // folder's snapshots stay (pointer rows, complete in metadata) but are
    // pin-only, exactly like its sync-change manifests below.
    let meta_snapshot_id_set: std::collections::HashSet<i64> = db
        .list_snapshot_ids_for_folders(&meta_folder_ids)
        .await
        .context("collecting metadata-only folders' snapshot ids")?
        .into_iter()
        .collect();
    let (meta_only_snapshot_ids, walk_snapshot_ids): (Vec<i64>, Vec<i64>) = all_snapshot_ids
        .iter()
        .copied()
        .partition(|id| meta_snapshot_id_set.contains(id));

    let snapshot_refs = db
        .snapshot_manifest_refs(&walk_snapshot_ids)
        .await
        .context("collecting manifest hashes")?;
    let meta_pin_only_snapshots: Vec<Vec<u8>> = db
        .snapshot_manifest_refs(&meta_only_snapshot_ids)
        .await
        .context("collecting metadata-only snapshot manifest hashes")?
        .into_iter()
        .map(|(mh, _)| mh)
        .collect();
    let referenced_manifests = snapshot_refs.len() + meta_pin_only_snapshots.len();
    // 2b. Also collect manifest hashes from sync changes — these are live
    // sync chunks that must not be garbage-collected. Rows superseded within
    // the grace window stay pinned so an in-flight reader of a just-superseded
    // manifest gets the same buffer fresh blobs get (blob `created_at` grace
    // can't cover them — old chunks long predate their supersede).
    let superseded_cutoff_millis =
        crate::db::now_epoch_millis() - grace_period_secs.saturating_mul(1000);
    let walk_exclude: Vec<i64> = exclude_sync_folder_ids
        .iter()
        .chain(meta_folder_ids.iter())
        .copied()
        .collect();
    let sync_refs = db
        .sync_change_manifest_refs(superseded_cutoff_millis, &walk_exclude)
        .await
        .context("collecting sync change manifest hashes")?;
    // The pin-only extras: the metadata-only folders' own manifest hashes
    // (live and superseded alike — a superseded manifest blob is still
    // metadata), inserted into `referenced` below without a chunk walk.
    let meta_pin_only_sync = db
        .all_sync_change_manifest_hashes_for_folders(&meta_folder_ids)
        .await
        .context("collecting metadata-only folders' manifest hashes")?;

    // 2c. Cross-location backup custody — on a destination nest, segment-backup
    // chunks + the `manifest.<kind>` mirror are referenced by no snapshot or
    // sync_change row (their `record_change` epilogue is a source-nest concern);
    // they are tracked instead in the latest-per-path `backup_custody`
    // projection, which the `fauna.sync.changes.record` handler maintains for
    // custody-copy sets. Walking these here is what makes a destination's GC
    // never delete a live backup blob (`docs/goal/architecture/message-segment-store.md`
    // § GC-safety; `docs/goal/behavior/backup-restore.md` § Known gap). Custody
    // rows are recorded by the segment-backup coordinator's `upload_bytes`
    // pipeline, so every one references a genuine `ChunkManifest` (uploaded
    // plaintext via `/api/v1/manifests`; `encryption_key` is `None` in
    // production, so the walk reaches each backup manifest's chunk references
    // without the owner's `BackupKey` — the held-for-friends case works
    // unchanged).
    let backup_custody_manifest_hashes = db
        .backup_custody_manifest_hashes()
        .await
        .context("collecting backup custody manifest hashes")?;

    // 2d. Retained superseded custody generations — the grace window T. These
    // are precisely the manifests the latest-per-path projection above no longer
    // names, and pinning them is what makes a rogue source nest's supersede
    // recoverable instead of terminal (`message-segment-store.md`
    // § Cross-location backup protocol → custody grace window). Every row still
    // present is inside T: `do_gc` reclaims the expired ones *before* this walk,
    // so "row exists" and "must survive this sweep" are the same statement.
    // Manifest-class exactly like live custody rows — same producer, same
    // `upload_bytes` pipeline, same plaintext manifest blob.
    let backup_generation_manifest_hashes = db
        .backup_custody_generation_manifest_hashes()
        .await
        .context("collecting retained backup custody generation manifest hashes")?;

    // 2e. ATProto PDS uploaded blobs (F2.4). `com.atproto.repo.uploadBlob`
    // writes bytes + a `blob_metadata` row + an `atproto_blobs` row, and records
    // no `sync_changes` row — a Fauna app's media is pinned by the media
    // library's `changes.record`, and an external app has no such leg. So until
    // this walk existed these bytes were reachable from nothing, and the sweep
    // deleted an external app's live image ~30 min after upload while the repo
    // record still named it and `getBlob` still had to serve it.
    //
    // **Row existence is the whole predicate** — no window arithmetic here. The
    // reference window is the sweeper's (`crate::atproto_blob_sweeper`), which
    // deletes the ROW; the bytes stop being reachable as a consequence, on this
    // walk's next pass. One decision point, so the two can never disagree in the
    // direction that deletes referenced media.
    //
    // Pinned WITHOUT the direct-blob byte probe below, and the reason is the
    // write path rather than a naming convention: these bytes entered through
    // `POST /api/v1/blob`, stored verbatim, with an image MIME **sniffed from
    // the bytes** and non-image bytes refused outright (F2.4 slice 1's
    // sniffed-not-declared ruling). A `ChunkManifest` therefore cannot be in
    // this row set, so there is no chunk set to enumerate and pinning the hash
    // protects the reference completely. Routing them through the probe would
    // also let one unreadable image fail-close the whole box's sweep.
    let atproto_blob_media_refs = db
        .all_atproto_blob_media_refs()
        .await
        .context("collecting atproto uploaded blob refs")?;

    // 3. Pin every reference, then walk the `ChunkManifest`-class ones to
    // collect their chunk store keys. Direct-blob rail references carry no
    // chunks — inserting the hash itself fully protects them. The direct-blob
    // classification must hold across ALL sources referencing a hash (custody
    // rows are always manifest-class), so fold before partitioning: one
    // manifest-class reference makes the hash decode-required.
    let mut classified: std::collections::HashMap<Vec<u8>, bool> = std::collections::HashMap::new();
    for (mh, direct_blob) in snapshot_refs.into_iter().chain(sync_refs) {
        classified
            .entry(mh)
            .and_modify(|d| *d &= direct_blob)
            .or_insert(direct_blob);
    }
    for mh in backup_custody_manifest_hashes
        .into_iter()
        .chain(backup_generation_manifest_hashes)
    {
        classified.insert(mh, false);
    }

    let mut referenced: HashSet<Vec<u8>> = HashSet::new();
    let atproto_blob_refs = {
        let before = referenced.len();
        referenced.extend(atproto_blob_media_refs);
        referenced.len() - before
    };

    // 2f. Live signed records — every blob a stored post or profile names:
    // a feed-compose photo and its thumbnail, a video's manifest/segments/
    // poster, a gated post's sealed body (`encrypted_ref`) and its plaintext
    // `attachment_refs`, a profile's avatar and banner. All of them entered
    // through `POST /api/v1/blob`, which writes bytes + a `blob_metadata` row
    // and records NO `sync_changes` row — the same shape as 2e, and until
    // this arm existed the same fate: reachable from nothing, swept ~30 min
    // after upload while the post kept rendering its teaser over a 404 and
    // the profile its broken avatar (found 2026-09-07; `backup-restore.md`
    // § 9 step 2). The oracle walks the LIVE records rather than a reference
    // table written at ingest: a deleted post leaves `content` first
    // (`feed.md` § Post deletion → removal order), so its blobs reclaim after
    // grace with no delete leg to keep in step; every record already on the
    // box is covered with no backfill; and it is the same shape as every
    // other arm. What the nest cannot see — the media hashes inside a sealed
    // body — it pins through `GatedInfo::attachment_refs`, the floor ruling
    // of 2026-09-08 (`encryption-at-rest.md` § Plaintext floor → Posts row).
    //
    // Chunkless like 2e (raw bytes via `/api/v1/blob`, never a manifest), so
    // pinned without the direct-blob probe below. A body that cannot be READ
    // fail-closes the run exactly like an unreadable manifest (transient,
    // heals next run). A body that does not DECODE is warned and skipped,
    // NOT fail-closed: every writer decodes before it stores
    // (`segments::post::store_post`; ingest validates the signed shape), so
    // the only undecodable rows are the documented degenerate inline
    // fallback — which never named a blob this nest could serve either — and
    // a permanent nest-wide fail-close on one such row is exactly the
    // client-triggerable stall the manifest arm's hash-intact split exists
    // to avoid.
    let mut record_read_failures: u64 = 0;
    let mut undecodable_records: u64 = 0;
    // Rides the same walk (step 4c): every blob a live record names, split on
    // whether that record is under a legal takedown, which is the blob-serve
    // door's predicate (`crate::moderation_withhold`). Pinning and withholding
    // ask the same walk two different questions — the GC pin is deliberately
    // NOT gated on the flag, because withheld is not deleted.
    let mut withhold = crate::moderation_withhold::WithholdSets::default();
    let taken_down_posts = db
        .taken_down_post_ids()
        .await
        .context("listing taken-down post ids for the GC record walk")?;
    let record_blob_refs = {
        let before = referenced.len();
        let post_ids = db
            .list_all_post_ids()
            .await
            .context("listing post ids for the GC record walk")?;
        for post_id in &post_ids {
            let body =
                match crate::segments::post::load_post_body(records.segments, db, post_id).await {
                    Ok(Some(body)) => body,
                    // No live body: tombstoned, or a mirror row whose segment
                    // record is gone — nothing this post can still serve, so
                    // nothing to pin (the delete-then-reclaim path).
                    Ok(None) => continue,
                    Err(e) => {
                        record_read_failures += 1;
                        tracing::error!(
                            post = hex::encode(post_id),
                            error = %e,
                            "failed to READ a post body during GC — its blobs cannot be \
                             enumerated; this run's sweep will be skipped"
                        );
                        continue;
                    }
                };
            match crate::db::posts::decode_stored_post(&body) {
                Some(post) => {
                    referenced.extend(post.blob_refs().iter().map(|h| h.digest().to_vec()));
                    withhold.add_record(
                        taken_down_posts.contains(post_id),
                        post.blob_refs().into_iter().map(|h| h.digest()),
                    );
                }
                None => {
                    undecodable_records += 1;
                    tracing::warn!(
                        post = hex::encode(post_id),
                        "a stored post does not decode — nothing was ever servable through \
                         it; no blob pinned for it and the sweep proceeds"
                    );
                }
            }
        }
        // Profiles rest inline in `content` (`profile_handlers`), newest row
        // per author served; an older row's replaced avatar reclaims.
        let profiles = db
            .list_latest_profile_payloads()
            .await
            .context("listing profile payloads for the GC record walk")?;
        for payload in &profiles {
            match fauna_core::encoding::decode_profile(payload) {
                Ok((profile, _)) => {
                    referenced.extend(
                        profile
                            .avatar
                            .into_iter()
                            .chain(profile.banner)
                            .map(|h| h.digest().to_vec()),
                    );
                }
                Err(e) => {
                    undecodable_records += 1;
                    tracing::warn!(
                        error = %e,
                        "a stored profile does not decode — no blob pinned for it and the \
                         sweep proceeds"
                    );
                }
            }
        }
        referenced.len() - before
    };
    // 2g. Live conversation records — every sealed attachment a live conv
    // record's sender listed in plaintext beside the sealed envelope
    // (`ChannelSendRequest::attachment_refs` → `conv_attachment_refs`, the
    // conversation kind's blob-reachability floor ratified 2026-09-08,
    // `encryption-at-rest.md` § Per-content-kind conformance → Conversation
    // messages row). The attachment bytes enter through `POST /api/v1/blob`
    // sealed under the channel's `derive_blob_key(epoch_secret)` and are
    // named only inside an MLS application message the nest cannot open, so
    // until this arm nothing on the box reached them and the sweep past grace
    // deleted every conversation attachment ~30 min after upload (found
    // 2026-09-08 while closing the posts arm above; `backup-restore.md` § 9
    // step 2). Liveness is the segment mirror's own: a reference whose
    // `(channel, seq)` no longer has a live `segment_records` row (relay-
    // acked and purged, or compacted away) pins nothing, so the attachment
    // reclaims with its record — no separate delete leg to keep in step. A
    // cooperative chat `Delete` is a sealed message, never a store tombstone,
    // so it keeps the bytes exactly as it keeps the envelope
    // (`conversations.md` § Reactions & message delete → At rest). Chunkless
    // like 2e/2f. A message carrying no attachment refs has no rows and
    // pins nothing.
    let conv_attachment_refs = {
        let before = referenced.len();
        let refs = db
            .list_live_conv_attachment_refs()
            .await
            .context("listing live conversation attachment refs for the GC walk")?;
        referenced.extend(refs.into_iter().map(|h| h.to_vec()));
        referenced.len() - before
    };
    // 2h. Live rendered sites — every body a published site's rendered stores
    // name (`web_rendered`, and the paywalled `web_rendered_sealed`), plus every
    // body a render still holding its site has staged (`web_render_staged`):
    // stored and carrying its `blob_metadata` row, not yet on the site, because
    // a render writes its rows in one replacing transaction at its end
    // (`backup-restore.md` § 9 step 2h). Until this arm the rendered stores
    // were nowhere in this walk and the render wrote no metadata row, two gaps
    // that cancelled out to "never touched" — so a row anyone wrote for a live
    // page's public bytes (the blob PUT writes one for any authenticated
    // principal) made that page a deletion candidate, and the next sweep took
    // it to 404 with its rendered row pointing at nothing. A page replaced or cleared, or a superseded render's
    // staged body, is named here no longer and reclaims past grace. Chunkless
    // like 2e–2g: a rendered body is raw bytes, never a manifest.
    let web_rendered_refs = {
        let before = referenced.len();
        let refs = db
            .list_web_render_referenced_hashes()
            .await
            .context("listing rendered-site body refs for the GC walk")?;
        referenced.extend(refs);
        referenced.len() - before
    };
    // 2i. Live content rows' spilled payloads — every blob a `content` row
    // rests its payload in (`content.blob_hash`): an inbox envelope over the
    // payload store's threshold. While the
    // row lives its payload IS that blob — an undelivered envelope the
    // recipient has not drained, a delivered one its history still lists — so
    // it must never be a candidate. Until this arm the payload store wrote no
    // `blob_metadata` row, and the two gaps cancelled out as 2h's did: nothing
    // was collected, but a row anyone wrote for an envelope's bytes would have
    // deleted a user's inbox content. The row goes with the account's inbox
    // (`delete_inbox_for_actor`), and its blob reclaims past grace. Chunkless:
    // a spilled payload is raw bytes.
    let content_payload_refs = {
        let before = referenced.len();
        let refs = db
            .list_content_payload_blob_hashes()
            .await
            .context("listing content rows' spilled payload blobs for the GC walk")?;
        referenced.extend(refs);
        referenced.len() - before
    };
    // The conversation half of step 4c — the same liveness join, split on the
    // record's takedown flag instead of collapsed. Kept as its own read rather
    // than folded into the pin above: the pin deliberately does not care about
    // the flag (withheld is not deleted), and one extra index-free scan of a
    // reference table costs nothing beside the post walk that just ran.
    withhold
        .add_conversations(db)
        .await
        .context("folding live conversation records into the takedown withhold set")?;
    // And the posts whose author deleted them while taken down. Not part of
    // the walk above by construction — such a record has no `content` row to
    // enumerate and a tombstoned body that reads as `None` — so its digests
    // come from what the delete captured (`moderation.md` § Legal takedown →
    // *Posts*). They only ever ADD to the flagged side; a live unflagged
    // record naming the same blob still exempts it.
    withhold
        .add_deleted_taken_down_posts(db)
        .await
        .context("folding deleted taken-down posts into the takedown withhold set")?;
    // Pin the metadata-only folders' manifests WITHOUT chunk-walking them —
    // the residency partition above. A hash also referenced by a full source
    // is in `classified` too and walks through that source.
    referenced.extend(meta_pin_only_sync);
    referenced.extend(meta_pin_only_snapshots);
    let mut manifest_refs: Vec<Vec<u8>> = Vec::new();
    let mut direct_blob_candidates: Vec<Vec<u8>> = Vec::new();
    for (mh, direct_blob) in classified {
        referenced.insert(mh.clone());
        if direct_blob {
            direct_blob_candidates.push(mh);
        } else {
            manifest_refs.push(mh);
        }
    }

    // 3b. Confirm each direct-blob candidate against its BYTES before skipping
    // the chunk walk. The classification above derives from the *names* of the
    // referencing rails (`__`-prefixed ⇒ the reference is the stored blob
    // itself). The write paths enforce that convention, but reachability — the
    // one thing a wrong answer here destroys — must not rest on a name: any
    // reference whose blob actually decodes as a `ChunkManifest` is walked as
    // one, whatever set it was recorded against. A rail blob (client-sealed
    // payload, raw segment bytes) does not decode as a manifest, so this is a
    // no-op for every real rail; a decode failure on hash-INTACT bytes is the
    // EXPECTED case and is NOT a `manifest_decode_failures` — counting it would
    // fail-close the sweep on every cycle (`backup-restore.md` § 9). A decode
    // failure on hash-MISMATCHED bytes is different: a rail blob is keyed by
    // its own bytes, so a mismatch cannot be a healthy rail — it is at-rest
    // corruption of what may be a manifest, and fail-closes like the walk arm.
    //
    // A store READ ERROR is likewise NOT an answer to the question: the blob is
    // there and may well be a manifest whose chunks are live. It is the same
    // class the manifest-walk arm below fail-closes on, and must be treated
    // identically — folding it into "not a manifest" hands back the destructive
    // answer (skip the walk ⇒ sweep the chunks) on a transient EMFILE/EIO. That
    // matters more than a transient corner usually would: this probe is the ONLY
    // protection for the chunked-manifest-under-a-reserved-name rows a live nest
    // already carries (the write-path guards only stop new ones), and it feeds
    // this sweep — the box's one remaining destructive consumer of the oracle
    // since the `__index` boot purge was deleted (2026-08-02).
    for mh in direct_blob_candidates {
        // A malformed (non-32-byte) hash can address no blob at all — same
        // disposition as an absent blob (an already-broken pointer).
        let Ok(hash_arr) = <[u8; 32]>::try_from(mh.as_slice()) else {
            direct_blob_refs += 1;
            continue;
        };
        let probe = blob_store
            .get(&ContentHash::from_digest_raw(hash_arr))
            .await;
        let decodes_as_manifest = match probe {
            Ok(Some(raw_bytes)) => match decode_manifest(&raw_bytes, encryption_key) {
                Ok(_) => true,
                // Hash-intact + undecodable = the EXPECTED rail case (a
                // client-sealed payload or raw segment bytes, keyed by the
                // bytes themselves): a genuine direct blob. Uncounted — that
                // is what keeps steady-state sweeps running.
                Err(_) if stored_bytes_match_reference(&hash_arr, &raw_bytes, encryption_key) => {
                    false
                }
                // Hash-MISMATCHED + undecodable: these bytes are not what was
                // written under this address — at-rest corruption of what may
                // have been a genuine chunked manifest recorded under a
                // reserved name (the pre-enforcement row class the probe
                // exists to protect). Its chunks may be live and cannot be
                // enumerated: fail-close this run, exactly as the walk arm
                // does for the identical state.
                Err(e) => {
                    manifest_decode_failures += 1;
                    let sources = db.manifest_reference_sources(&mh).await.unwrap_or_default();
                    tracing::error!(
                        manifest = hex::encode(&mh),
                        error = %e,
                        referenced_by = ?sources,
                        "a reserved-rail reference is CORRUPT (bytes do not match their \
                         content address) — it cannot be classified and may be a manifest \
                         whose chunks are live; this run's sweep will be skipped"
                    );
                    continue;
                }
            },
            // An already-broken pointer — keep, and let the walk arm's
            // warn-and-continue reasoning apply: nothing is deepened by it.
            Ok(None) => false,
            Err(e) => {
                // Reachability is incomputable for this reference THIS RUN: we
                // cannot tell a rail blob from a manifest whose chunks are live.
                // Fail-close (both consumers bail on a non-zero count) rather
                // than sweep against a provably incomplete reference set. Unlike
                // a decode failure, this heals on the next run.
                manifest_decode_failures += 1;
                tracing::error!(
                    manifest = hex::encode(&mh),
                    error = %e,
                    "failed to READ a reserved-rail reference during GC — it cannot be \
                     classified, so it may be a manifest whose chunks are live; \
                     this run's sweep will be skipped"
                );
                continue;
            }
        };
        if decodes_as_manifest {
            tracing::warn!(
                manifest = hex::encode(&mh),
                "a reference from a reserved (`__`) folder decodes as a ChunkManifest — \
                 walking its chunks. Reserved rails are chunkless by contract, so this is \
                 anomalous state (a pre-enforcement row, or a set whose mode collided); \
                 the walk keeps its chunks reachable either way",
            );
            manifest_refs.push(mh);
        } else {
            direct_blob_refs += 1;
        }
    }

    for mh in &manifest_refs {
        // Deserialize manifest to find chunk hashes
        if let Ok(hash_arr) = <[u8; 32]>::try_from(mh.as_slice()) {
            let content_hash = ContentHash::from_digest_raw(hash_arr);
            match blob_store.get(&content_hash).await {
                Ok(Some(raw_bytes)) => {
                    match decode_manifest(&raw_bytes, encryption_key) {
                        Ok(manifest) => {
                            // Reachability follows the STORE keys: a content-key-
                            // encrypted chunk lives under its ciphertext hash
                            // (`stored_hashes`), not the plaintext hash, so walking
                            // `chunk_hashes` here would orphan the live blob and GC
                            // delete it (data loss). `store_keys()` is the plaintext
                            // hash for unencrypted chunks, the ciphertext hash for
                            // encrypted ones.
                            for ch in &manifest.store_keys() {
                                referenced.insert(ch.digest().to_vec());
                            }
                        }
                        // The bytes are hash-intact under their content address
                        // yet do not decode as a manifest: a content address is
                        // immutable, so they NEVER decoded as one — no chunk
                        // was ever enumerable through this reference, and the
                        // fail-close has nothing to protect. Pin the blob
                        // (already in `referenced`) and keep sweeping: counting
                        // this would hand any single client a permanent,
                        // nest-wide, cross-actor disable of blob
                        // reclamation. Reached today by junk
                        // uploaded via `/api/v1/chunks` and recorded as a
                        // `manifest_hash`, and — with no attacker — by the
                        // android engine recording the whole-file hash, which
                        // for a single-chunk file is the CHUNK's address.
                        Err(e)
                            if stored_bytes_match_reference(
                                &hash_arr,
                                &raw_bytes,
                                encryption_key,
                            ) =>
                        {
                            never_manifest_refs += 1;
                            let sources =
                                db.manifest_reference_sources(mh).await.unwrap_or_default();
                            tracing::warn!(
                                manifest = hex::encode(hash_arr),
                                error = %e,
                                referenced_by = ?sources,
                                "a ChunkManifest-class reference is hash-intact but was never \
                                 a manifest — nothing was ever readable through it; the blob \
                                 stays pinned and the sweep proceeds. This is a junk or \
                                 mis-keyed record: fix the recording client"
                            );
                        }
                        Err(e) => {
                            // The bytes are NOT the preimage of their content
                            // address at any write layer: genuine at-rest
                            // corruption of what may have been a live manifest.
                            // Reachability is incomputable — its chunk set
                            // cannot be enumerated, so any unreferenced blob
                            // might be one of its live chunks. Counted here;
                            // the sweep below fail-closes on a non-zero count.
                            manifest_decode_failures += 1;
                            let sources =
                                db.manifest_reference_sources(mh).await.unwrap_or_default();
                            tracing::error!(
                                manifest = hex::encode(hash_arr),
                                error = %e,
                                referenced_by = ?sources,
                                "a ChunkManifest-class reference is CORRUPT (bytes do not match \
                                 their content address) — its chunks cannot be enumerated; \
                                 this run's sweep will be skipped"
                            );
                        }
                    }
                }
                Ok(None) => {
                    // A referenced-but-absent manifest is an already-broken
                    // pointer: its file is unreadable regardless, so deleting
                    // its (unenumerable) chunks deepens nothing, and the
                    // coordinator's re-drive self-heals by re-uploading. Kept
                    // warn-and-continue — fail-closed here would let one
                    // permanently lost blob disable reclamation forever.
                    tracing::warn!(
                        manifest = hex::encode(hash_arr),
                        "manifest referenced by snapshot but missing from blob store"
                    );
                }
                Err(e) => {
                    // A transient store READ error is the same class as a
                    // decode failure: the manifest's chunks exist but cannot
                    // be enumerated this run — fail-close rather than sweep
                    // against a provably incomplete reference set. Unlike
                    // `Ok(None)`, this heals on the next run.
                    manifest_decode_failures += 1;
                    tracing::error!(
                        manifest = hex::encode(hash_arr),
                        error = %e,
                        "failed to read a referenced manifest during GC — \
                         this run's sweep will be skipped"
                    );
                }
            }
        }
    }

    Ok(ReachableHashes {
        referenced,
        manifest_decode_failures,
        never_manifest_refs,
        direct_blob_refs,
        atproto_blob_refs,
        referenced_manifests,
        live_snapshots,
        record_blob_refs,
        record_read_failures,
        undecodable_records,
        conv_attachment_refs,
        web_rendered_refs,
        content_payload_refs,
        withhold,
    })
}

/// Steps 4–7 of the GC: sweep every `blob_metadata` hash not in `referenced`
/// (respecting the creation-grace window), delete from the store, then drop
/// the metadata of what was actually removed.
async fn do_gc_delete_phase(
    db: &Arc<CacheDb>,
    blob_store: &Arc<dyn BlobStoreBackend>,
    grace_period_secs: i64,
    dry_run: bool,
    referenced: HashSet<Vec<u8>>,
    mut result: GcResult,
) -> Result<GcResult> {
    // 4. Get all blobs in blob_metadata
    let all_blobs = db
        .list_all_blob_hashes()
        .await
        .context("listing all blob hashes")?;

    // 5. Find unreferenced blobs (respecting grace period)
    let grace_cutoff = crate::db::now_epoch_secs() - grace_period_secs;
    let mut to_delete: Vec<Vec<u8>> = Vec::new();
    let mut to_delete_bytes: u64 = 0;

    for blob_hash in &all_blobs {
        if !referenced.contains(blob_hash)
            && let Ok(hash_arr) = <[u8; 32]>::try_from(blob_hash.as_slice())
        {
            match db.get_blob_metadata(&hash_arr).await {
                Ok(Some(meta)) => {
                    if meta.created_at > grace_cutoff {
                        result.skipped_grace_period += 1;
                        continue;
                    }
                    to_delete_bytes += meta.size_bytes as u64;
                    to_delete.push(blob_hash.clone());
                }
                Ok(None) => {
                    tracing::debug!(
                        hash = hex::encode(hash_arr),
                        "blob has no metadata, skipping during GC"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        hash = hex::encode(hash_arr),
                        error = %e,
                        "failed to read blob metadata during GC, skipping"
                    );
                }
            }
        }
    }

    if dry_run {
        // In dry-run mode, report what would be deleted without actually deleting
        result.deleted_blobs = to_delete.len() as u64;
        result.deleted_bytes = to_delete_bytes;
    } else {
        // 6. Delete unreferenced blobs from store, tracking successes
        let mut successfully_deleted: Vec<Vec<u8>> = Vec::new();
        for hash_bytes in &to_delete {
            if let Ok(hash_arr) = <[u8; 32]>::try_from(hash_bytes.as_slice()) {
                let hash = ContentHash::from_digest_raw(hash_arr);
                match blob_store.delete(&hash).await {
                    Ok(()) => {
                        successfully_deleted.push(hash_bytes.clone());
                    }
                    Err(e) => {
                        tracing::warn!("failed to delete blob {}: {e}", hex::encode(hash_arr));
                    }
                }
            }
        }

        // 7. Only delete metadata for blobs we actually removed from the store
        let deleted = db
            .delete_blob_metadata_batch(&successfully_deleted)
            .await
            .context("deleting blob metadata")?;
        result.deleted_blobs = deleted;
        result.deleted_bytes = to_delete_bytes;
    }

    tracing::info!(
        live_snapshots = result.live_snapshots,
        referenced_manifests = result.referenced_manifests,
        direct_blob_refs = result.direct_blob_refs,
        atproto_blob_refs = result.atproto_blob_refs,
        record_blob_refs = result.record_blob_refs,
        undecodable_records = result.undecodable_records,
        conv_attachment_refs = result.conv_attachment_refs,
        web_rendered_refs = result.web_rendered_refs,
        content_payload_refs = result.content_payload_refs,
        never_manifest_refs = result.never_manifest_refs,
        deleted_blobs = result.deleted_blobs,
        deleted_bytes = result.deleted_bytes,
        "garbage collection complete"
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::{BlobStoreBackend, DiskBlobStore};

    /// A throwaway `__post` segment store: the record walk (step 2f) reads it
    /// and finds nothing in tests that seed no posts; [`seed_post`] writes
    /// into it for the ones that do.
    fn post_segments() -> (tempfile::TempDir, Arc<fauna_segment_store::SegmentManager>) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(fauna_segment_store::SegmentManager::new(
            dir.path().to_path_buf(),
            "post",
        ));
        (dir, mgr)
    }

    fn records(segs: &fauna_segment_store::SegmentManager) -> PostBodySource<'_> {
        PostBodySource { segments: segs }
    }

    /// Store `bytes` the way `POST /api/v1/blob` does — bytes in the store +
    /// the `blob_metadata` row that is the blob's only durable trace — and
    /// return its content address.
    async fn put_blob(db: &Arc<CacheDb>, store: &Arc<DiskBlobStore>, bytes: &[u8]) -> ContentHash {
        let hash = ContentHash::of_raw(bytes);
        store.put(&hash, bytes).await.unwrap();
        db.put_blob_metadata(&hash.digest(), bytes.len() as i64, "image/png", None, None)
            .await
            .unwrap();
        hash
    }

    /// File a bare canonical post the way `store_post` does for every writer —
    /// body in the author's `__post` segment, projection row in `content` —
    /// and return its 32-byte id. Bare (unsigned) is the bridge-translated
    /// shape `decode_stored_post` accepts; the walk does not care which.
    async fn seed_post(
        db: &Arc<CacheDb>,
        segs: &fauna_segment_store::SegmentManager,
        post: &fauna_core::data::Post,
    ) -> [u8; 32] {
        let post_id: [u8; 32] = fauna_core::encoding::compute_post_id(post)
            .unwrap()
            .as_bytes()[4..]
            .try_into()
            .unwrap();
        let body = fauna_core::encoding::canonical_encode(post).unwrap();
        crate::segments::post::store_post(segs, db, &post_id, &body, None)
            .await
            .unwrap();
        post_id
    }

    fn post_with_body(author: u8, body: fauna_core::data::PostBody) -> fauna_core::data::Post {
        fauna_core::data::Post {
            author: fauna_core::identity::ActorId([author; 32]),
            created_at: fauna_core::data::Timestamp(1_700_000_000_000_000 + author as u64),
            body,
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    fn photo_body(photo: ContentHash, thumb: Option<ContentHash>) -> fauna_core::data::PostBody {
        fauna_core::data::PostBody::TextWithMedia {
            content: "look".into(),
            facets: vec![],
            items: vec![fauna_core::data::MediaItem {
                blob_hash: photo,
                media_type: "image/png".into(),
                size_bytes: 11,
                dimensions: None,
                thumbnail: thumb,
                ..Default::default()
            }],
        }
    }

    /// A feed-compose photo enters through `POST /api/v1/blob` and is named
    /// only by the signed post — no `sync_changes` row, no snapshot, nothing
    /// the walk reached before step 2f — so the sweep past grace deleted
    /// every post's photo and thumbnail (found 2026-09-07). With the arm a
    /// live post's blobs survive an immediate zero-grace pass while a true
    /// orphan still reclaims: the proof the sweep really ran, so a mutant
    /// that drops the arm reds this on the first assertion.
    #[tokio::test]
    async fn gc_keeps_a_live_posts_photo_and_thumbnail_and_still_reclaims_orphans() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let photo = put_blob(&db, &store, b"photo bytes").await;
        let thumb = put_blob(&db, &store, b"thumbnail bytes").await;
        let orphan = put_blob(&db, &store, b"an upload no record names").await;
        seed_post(
            &db,
            &segs,
            &post_with_body(0x61, photo_body(photo, Some(thumb))),
        )
        .await;

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();

        assert!(
            store.exists(&photo).await.unwrap(),
            "a live post's photo was swept"
        );
        assert!(
            store.exists(&thumb).await.unwrap(),
            "a live post's thumbnail was swept"
        );
        assert_eq!(
            result.record_blob_refs, 2,
            "the photo and its thumbnail are pinned by the post"
        );
        assert_eq!(result.deleted_blobs, 1, "only the orphan is swept");
        assert!(
            !store.exists(&orphan).await.unwrap(),
            "the unreferenced upload must still reclaim — else the pin is a no-op sweep"
        );
    }

    /// A tier-restricted post's whole body is a sealed blob at
    /// `encrypted_ref`, and its photos are sealed blobs whose hashes live
    /// INSIDE that body — the nest cannot enumerate them, which is exactly
    /// what `attachment_refs` (the 2026-09-08 floor ruling) carries in
    /// plaintext. Both halves must survive; the KeyBlob is a DB row and
    /// needs no pin.
    #[tokio::test]
    async fn gc_keeps_a_gated_posts_sealed_body_and_its_plaintext_attachment_refs() {
        use fauna_core::subscription::types::{GatedInfo, KeyAccess};

        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let sealed_body = put_blob(&db, &store, b"sealed full body").await;
        let sealed_photo = put_blob(&db, &store, b"sealed photo").await;
        let sealed_thumb = put_blob(&db, &store, b"sealed thumbnail").await;
        let orphan = put_blob(&db, &store, b"abandoned upload").await;
        let mut post = post_with_body(
            0x62,
            fauna_core::data::PostBody::Text {
                content: "teaser".into(),
                facets: vec![],
            },
        );
        post.gated = Some(GatedInfo {
            encrypted_ref: sealed_body,
            key_access: KeyAccess::Broadcast {
                key_blob_ref: ContentHash::of_raw(b"key blob row"),
            },
            tier: "gold".into(),
            tier_rank: 1,
            seal_id: ContentHash::from_digest_raw([0x5eu8; 32]),
            attachment_refs: vec![sealed_photo, sealed_thumb],
        });
        seed_post(&db, &segs, &post).await;

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();

        assert!(
            store.exists(&sealed_body).await.unwrap(),
            "the sealed body — the whole post, unopenable forever once gone — was swept"
        );
        assert!(
            store.exists(&sealed_photo).await.unwrap()
                && store.exists(&sealed_thumb).await.unwrap(),
            "a sealed attachment named only by attachment_refs was swept"
        );
        assert_eq!(result.record_blob_refs, 3);
        assert_eq!(result.deleted_blobs, 1, "only the orphan is swept");
        assert!(!store.exists(&orphan).await.unwrap());
    }

    /// The pin follows the post's life: `fauna.posts.delete` drops the
    /// `content` projection row first (`feed.md` § Post deletion → removal
    /// order), the walk no longer sees the post, and its photo reclaims on
    /// the next pass — the deleted-post half of the success criterion, with
    /// no delete leg that could fall out of step.
    #[tokio::test]
    async fn gc_reclaims_a_deleted_posts_photo_once_its_projection_row_is_gone() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let photo = put_blob(&db, &store, b"a photo that outlives nothing").await;
        let post_id = seed_post(&db, &segs, &post_with_body(0x63, photo_body(photo, None))).await;

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert!(
            store.exists(&photo).await.unwrap(),
            "pinned while the post lives"
        );

        assert!(db.delete_post_projection(&post_id).await.unwrap());
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.deleted_blobs, 1);
        assert!(
            !store.exists(&photo).await.unwrap(),
            "a deleted post's photo must reclaim — the pin is not a leak"
        );
    }

    /// An undecodable stored post — the documented degenerate inline
    /// fallback — is warned and skipped, never fail-closed: one junk row must
    /// not disable reclamation nest-wide (the client-triggerable stall the
    /// manifest arm's hash-intact split exists to avoid).
    #[tokio::test]
    async fn gc_skips_an_undecodable_post_without_fail_closing() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        db.put_post(&[0x77u8; 32], b"not a post at all", None)
            .await
            .unwrap();
        let orphan = put_blob(&db, &store, b"still an orphan").await;

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.undecodable_records, 1);
        assert_eq!(result.record_read_failures, 0);
        assert_eq!(
            result.deleted_blobs, 1,
            "the sweep proceeded past the junk row"
        );
        assert!(!store.exists(&orphan).await.unwrap());
    }

    /// A profile's avatar and banner are bare `/api/v1/blob` uploads named
    /// only by the signed `Profile` record (`media.md` § Encryption at rest:
    /// the same shape as a public post's photo). The newest row per author
    /// is what `fauna.profile.get` serves, so its images are pinned and a
    /// replaced avatar reclaims.
    #[tokio::test]
    async fn gc_keeps_the_current_profiles_avatar_and_banner_and_reclaims_a_replaced_avatar() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let old_avatar = put_blob(&db, &store, b"the avatar before the edit").await;
        let avatar = put_blob(&db, &store, b"the current avatar").await;
        let banner = put_blob(&db, &store, b"the current banner").await;
        // Signed, as `fauna.profile.set` stores it: `decode_profile` refuses
        // a bare body, and the GC walk reads profiles through it.
        let author_kp = fauna_core::identity::ActorKeypair::from_secret([0x65u8; 32]);
        let author = author_kp.actor_id().0;
        let profile_at =
            |avatar: ContentHash, banner: Option<ContentHash>| fauna_core::data::Profile {
                actor_id: fauna_core::identity::ActorId(author),
                display_name: Some("me".into()),
                bio: None,
                avatar: Some(avatar),
                banner,
                links: vec![],
                nests: vec![],
                admin_nests: vec![],
                load_hint: None,
                inbox_mode: fauna_core::data::InboxMode::default(),
                recovery_head: None,
                updated_at: fauna_core::data::Timestamp(1),
            };
        for (created_at, profile) in [
            (1_000i64, profile_at(old_avatar, None)),
            (2_000i64, profile_at(avatar, Some(banner))),
        ] {
            let bytes = fauna_core::encoding::sign_and_pack(&author_kp, &profile).unwrap();
            let id = *blake3::hash(&bytes).as_bytes();
            let conn = db.conn().await;
            crate::db::content::insert_content(
                &conn, &id, "profile", &author, created_at, &bytes, None, "fauna", None,
            )
            .unwrap();
        }

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert!(
            store.exists(&avatar).await.unwrap() && store.exists(&banner).await.unwrap(),
            "the current profile's avatar/banner were swept"
        );
        assert_eq!(result.record_blob_refs, 2);
        assert_eq!(result.deleted_blobs, 1);
        assert!(
            !store.exists(&old_avatar).await.unwrap(),
            "a replaced avatar (older profile row) must reclaim"
        );
    }

    // -- Step 4c: the legal-takedown blob-serve withhold ------------------
    //
    // The pin arm above and the withhold arm here ask ONE walk two questions.
    // Both are proved through the production writers (`set_post_legal_takedown`
    // / `set_conv_legal_takedown` over rows `store_post` / `append_with_refs`
    // wrote), and every assertion pairs "withheld" with "still on disk" —
    // withheld is not deleted, and a test that only checked the flag would
    // pass on a mutant that deleted the bytes instead.

    /// A taken-down post's photo is withheld from `GET /api/v1/blob/{id}` and
    /// a restore re-serves it — the post half of the door, through the
    /// takedown handler's own rebuild leg.
    #[tokio::test]
    async fn a_taken_down_posts_photo_is_withheld_at_the_door_and_a_restore_re_serves() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let photo = put_blob(&db, &store, b"the compelled photo").await;
        let innocent = put_blob(&db, &store, b"an unrelated photo").await;
        let post = post_with_body(0x71, photo_body(photo, None));
        let post_id = seed_post(&db, &segs, &post).await;
        let other = post_with_body(0x72, photo_body(innocent, None));
        seed_post(&db, &segs, &other).await;
        // Every stored post is indexed by its writer; the flag lives on that row.
        db.index_post(&post_id, &post).await.unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert!(
            !db.blob_is_legally_withheld(&photo.digest()).await.unwrap(),
            "nothing is withheld before a takedown"
        );

        db.set_post_legal_takedown(&post_id, Some("EU-DSA-2026/7"))
            .await
            .unwrap();
        assert_eq!(
            db.get_post_legal_takedown(&post_id)
                .await
                .unwrap()
                .as_deref(),
            Some("EU-DSA-2026/7"),
            "the flag must actually land on a `content_meta` row — the setter does \
             not assert its own row count, so an unindexed post would silently \
             leave this test asserting a withhold that no takedown exists for"
        );
        crate::moderation_withhold::recompute(&db, records(&segs))
            .await
            .unwrap();

        assert!(
            db.blob_is_legally_withheld(&photo.digest()).await.unwrap(),
            "the door still serves a taken-down post's photo — the withhold blanks \
             the text and keeps relaying the picture"
        );
        assert!(
            !db.blob_is_legally_withheld(&innocent.digest())
                .await
                .unwrap(),
            "an unrelated post's photo must be untouched"
        );
        assert!(
            store.exists(&photo).await.unwrap(),
            "withheld is NOT deleted — the GC pin stays flag-blind so a restore re-serves"
        );

        // Tombstone-not-delete: the overturn re-serves the same bytes.
        db.set_post_legal_takedown(&post_id, None).await.unwrap();
        crate::moderation_withhold::recompute(&db, records(&segs))
            .await
            .unwrap();
        assert!(
            !db.blob_is_legally_withheld(&photo.digest()).await.unwrap(),
            "a restore must re-serve the attachment, not only the body"
        );
    }

    /// **The withhold survives the author's own delete**.
    ///
    /// The test above pins the withhold while the flag stands. The flag lives
    /// on `content_meta`, and the author's own delete removes exactly that row
    /// — so before this, a taken-down post the author then deleted dropped out
    /// of the walk entirely (no `content` row to enumerate, and a tombstoned
    /// body that reads as `None`), its photo left the withheld side, and the
    /// door served the compelled picture again until the GC happened to drop
    /// the bytes.
    ///
    /// Nothing recoverable afterwards says the record was compelled: the
    /// permanent `TakenDown` obligation row cannot, because an overturn writes
    /// no counter-row, so *taken down → deleted* and *taken down → restored →
    /// deleted* read identically there. So the delete captures it — the
    /// citation and the digests the record named — and this walk folds those
    /// digests back in.
    ///
    /// Discriminating by construction, the shape the exemption test below
    /// uses: `innocent` belongs to a post that was never taken down and is
    /// deleted the same way, so what keeps the photo withheld is the takedown,
    /// not the deletion.
    #[tokio::test]
    async fn a_taken_down_posts_photo_stays_withheld_after_its_author_deletes_it() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let photo = put_blob(&db, &store, b"the compelled photo").await;
        let innocent = put_blob(&db, &store, b"a photo nobody compelled").await;
        let post = post_with_body(0x81, photo_body(photo, None));
        let post_id = seed_post(&db, &segs, &post).await;
        let other = post_with_body(0x82, photo_body(innocent, None));
        let other_id = seed_post(&db, &segs, &other).await;
        db.index_post(&post_id, &post).await.unwrap();
        db.index_post(&other_id, &other).await.unwrap();

        db.set_post_legal_takedown(&post_id, Some("EU-DSA-2026/9"))
            .await
            .unwrap();
        crate::moderation_withhold::recompute(&db, records(&segs))
            .await
            .unwrap();
        assert!(
            db.blob_is_legally_withheld(&photo.digest()).await.unwrap(),
            "the takedown withholds the photo while the flag stands"
        );

        // The author deletes both posts — the delete's two removal steps, as
        // `delete_post_core` performs them: the projection rows go, then the
        // mirror row is tombstoned while the segment keeps the bytes. The
        // taken-down one carries the capture the delete makes first.
        db.record_taken_down_post_deleted(
            &post_id,
            &post.author.0,
            "EU-DSA-2026/9",
            &[photo.digest()],
            1_700_000_000_000_000,
        )
        .await
        .unwrap();
        for (id, p) in [(&post_id, &post), (&other_id, &other)] {
            db.delete_post_projection_with_witness(id, None)
                .await
                .unwrap();
            let (scope, seg_id) = crate::segments::post::lookup_scope_by_post_id(&db, id)
                .await
                .unwrap()
                .expect("the seeded record is in a segment");
            let cid = fauna_cbor::Cid::from_digest_dag_cbor(*id);
            crate::segments::post::tombstone_by_cid(&db, &scope, seg_id, &cid)
                .await
                .unwrap();
            let _ = p;
        }

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        garbage_collect(&db, &dyn_store, records(&segs), 0, None, true)
            .await
            .unwrap();

        assert!(
            db.blob_is_legally_withheld(&photo.digest()).await.unwrap(),
            "a delete must not hand the compelled photo back: the flag's row is \
             gone, so the walk sees nothing — the fact the delete captured is \
             what keeps the door shut"
        );
        assert!(
            !db.blob_is_legally_withheld(&innocent.digest())
                .await
                .unwrap(),
            "and an ordinary delete withholds nothing — the takedown is what \
             binds, never the deletion"
        );
    }

    /// **The opposite ordering — deleted, then the order arrives**
    /// (`moderation.md` § Legal takedown → *Posts*). The two tests above pin
    /// the takedown-then-delete case; here the author's ORDINARY delete runs
    /// first (no flag exists yet), and the compelled order against that
    /// already-deleted post arrives afterward. Proves the door shuts
    /// **synchronously**, from `post_legal_takedown_of_deleted_post_txn` +
    /// one `recompute` — the same leg the RPC handler wrapper always runs
    /// right before its reply — with **no GC sweep in between**: the DoS
    /// window this arm exists to close is between the compelled order and
    /// the next sweep, so the door must already be shut before any sweep
    /// ever runs.
    #[tokio::test]
    async fn a_photo_is_withheld_when_the_takedown_arrives_after_its_posts_delete() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let photo = put_blob(&db, &store, b"a photo, deleted before the order arrived").await;
        let post = post_with_body(0x91, photo_body(photo, None));
        let post_id = seed_post(&db, &segs, &post).await;
        db.index_post(&post_id, &post).await.unwrap();

        // The author's ORDINARY delete — no takedown flag exists yet, so
        // nothing is captured by the delete's own capture leg.
        db.delete_post_projection_with_witness(&post_id, None)
            .await
            .unwrap();
        let (scope, seg_id) = crate::segments::post::lookup_scope_by_post_id(&db, &post_id)
            .await
            .unwrap()
            .expect("the seeded record is in a segment");
        let cid = fauna_cbor::Cid::from_digest_dag_cbor(post_id);
        crate::segments::post::tombstone_by_cid(&db, &scope, seg_id, &cid)
            .await
            .unwrap();
        assert!(
            db.taken_down_deleted_post_blob_digests()
                .await
                .unwrap()
                .is_empty(),
            "an ordinary delete of a post nobody compelled captures nothing"
        );

        // The compelled order arrives afterward — exactly what
        // `moderation_handlers::legal_takedown_of_deleted_post` does: resolve
        // through the tombstone-inclusive lookup, read the digests through
        // the tombstone-inclusive body reader, then the one atomic write.
        let (author, found_seg) =
            crate::segments::post::lookup_scope_by_post_id_including_tombstoned(&db, &post_id)
                .await
                .unwrap()
                .expect("the tombstoned record is still in the segment, uncompacted");
        assert_eq!(found_seg, seg_id);
        let body =
            crate::segments::post::read_body_by_post_id_including_tombstoned(&segs, &db, &post_id)
                .await
                .unwrap()
                .expect("the tombstone-inclusive reader still finds the bytes");
        let digests: Vec<[u8; 32]> = crate::db::posts::decode_stored_post(&body)
            .map(|p| p.blob_refs().into_iter().map(|h| h.digest()).collect())
            .unwrap();
        assert_eq!(digests, vec![photo.digest()]);

        db.post_legal_takedown_of_deleted_post_txn(
            &post_id,
            &hex::encode(post_id),
            "EU-DSA-2026/716",
            &author,
            [0x2Bu8; 32].as_slice(),
            &digests,
            "admin=… author=… reference=EU-DSA-2026/716",
            1_700_000_002_000_000,
        )
        .await
        .unwrap();

        // The synchronous leg the RPC wrapper always runs before its reply —
        // NO garbage_collect / sweep call anywhere in this test.
        crate::moderation_withhold::recompute(&db, records(&segs))
            .await
            .unwrap();

        assert!(
            db.blob_is_legally_withheld(&photo.digest()).await.unwrap(),
            "the door must already be shut the instant the takedown reply \
             would go out — before any GC sweep ever runs"
        );
        assert!(
            store.exists(&photo).await.unwrap(),
            "withheld is NOT deleted — tombstone-not-delete holds for this \
             ordering too"
        );
    }

    /// The exemption, and why it is not decoration: a takedown is compelled
    /// against ONE record and its transparency triple is per-record, so a blob
    /// a live UNFLAGGED record also names keeps serving. Withholding it would
    /// break that record's media with no tombstone, no appeal handle and no
    /// audit row — the silent, discretionary removal `moderation.md` § Legal
    /// takedown exists to make structurally impossible.
    ///
    /// The second half is the reason the set is REBUILT from a walk rather
    /// than edited at takedown time: once the last unflagged namer goes away,
    /// only a walk can notice that the blob is now named by a taken-down
    /// record alone.
    #[tokio::test]
    async fn a_blob_a_live_unflagged_record_also_names_keeps_serving() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        // One blob, two posts — byte-identical media dedups to one content
        // address, which is exactly how a shared blob arises.
        let shared = put_blob(&db, &store, b"the same bytes, twice posted").await;
        let flagged = post_with_body(0x81, photo_body(shared, None));
        let flagged_id = seed_post(&db, &segs, &flagged).await;
        let innocent = post_with_body(0x82, photo_body(shared, None));
        let innocent_id = seed_post(&db, &segs, &innocent).await;
        db.index_post(&flagged_id, &flagged).await.unwrap();
        db.index_post(&innocent_id, &innocent).await.unwrap();
        db.set_post_legal_takedown(&flagged_id, Some("EU-DSA-2026/8"))
            .await
            .unwrap();

        crate::moderation_withhold::recompute(&db, records(&segs))
            .await
            .unwrap();
        assert!(
            !db.blob_is_legally_withheld(&shared.digest()).await.unwrap(),
            "a blob a live unflagged post also names was withheld — that post's media \
             just broke with no tombstone, no appeal handle and no audit row"
        );

        // ...and once the last unflagged namer is gone, the blob IS withheld.
        db.delete_post_projection(&innocent_id).await.unwrap();
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert!(
            db.blob_is_legally_withheld(&shared.digest()).await.unwrap(),
            "with its last unflagged namer deleted the blob is named only by a \
             taken-down record — the reconciling sweep must withhold it"
        );
    }

    /// The conversation half: a flagged conv record's sealed attachment stops
    /// serving. This is the case the door was blindest to — every member
    /// already holds the sealed cid (it is inside the message they received),
    /// so the withhold blanked the envelope while the nest kept handing the
    /// attachment to exactly the audience it was meant to stop relaying to.
    #[tokio::test]
    async fn a_taken_down_conversation_records_sealed_attachment_is_withheld() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_post_dir, post_segs) = post_segments();
        let (_conv_dir, conv_segs) = conv_segments();

        let sealed = put_blob(&db, &store, b"sealed illegal attachment").await;
        let channel = [0xcau8; 32];
        let envelope = b"sealed-application-envelope".to_vec();
        crate::segments::conv::append_with_refs(
            &conv_segs,
            &db,
            &channel,
            &envelope,
            1_715_000_000_000,
            &[sealed.digest()],
            None,
        )
        .await
        .unwrap();
        let record_cid = fauna_mls::segments::derive_record_cid(&envelope).unwrap();

        assert_eq!(
            db.set_conv_legal_takedown(&record_cid, Some("EU-DSA-2026/9"))
                .await
                .unwrap(),
            1
        );
        crate::moderation_withhold::recompute(&db, records(&post_segs))
            .await
            .unwrap();
        assert!(
            db.blob_is_legally_withheld(&sealed.digest()).await.unwrap(),
            "the members who received the message keep fetching the sealed attachment \
             by hash after the withhold"
        );
        assert!(
            store.exists(&sealed).await.unwrap(),
            "the record is a live mirror row, so its attachment stays pinned — \
             withheld is not deleted"
        );

        db.set_conv_legal_takedown(&record_cid, None).await.unwrap();
        crate::moderation_withhold::recompute(&db, records(&post_segs))
            .await
            .unwrap();
        assert!(
            !db.blob_is_legally_withheld(&sealed.digest()).await.unwrap(),
            "restore must re-serve the attachment"
        );
    }

    /// A dry run still rebuilds the withheld set — deliberately: the set is an
    /// observation of the store as found and it deletes nothing. A dry run
    /// that skipped it would leave the door reading a set the operator's
    /// preview has already proved stale.
    #[tokio::test]
    async fn a_dry_run_still_rebuilds_the_withheld_set() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let photo = put_blob(&db, &store, b"dry-run photo").await;
        let post = post_with_body(0xa1, photo_body(photo, None));
        let post_id = seed_post(&db, &segs, &post).await;
        db.index_post(&post_id, &post).await.unwrap();
        db.set_post_legal_takedown(&post_id, Some("EU-DSA-2026/11"))
            .await
            .unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, true)
            .await
            .unwrap();
        assert_eq!(result.deleted_blobs, 0, "a dry run deletes nothing");
        assert!(
            db.blob_is_legally_withheld(&photo.digest()).await.unwrap(),
            "a dry run must still rebuild the withheld set — it is an observation, \
             and withholding deletes nothing"
        );
        assert!(store.exists(&photo).await.unwrap());
    }

    /// A throwaway `__conv` segment store for the conversation-attachment arm
    /// (step 2g). The GC never reads it — the arm reads the mirror's
    /// `conv_attachment_refs` — but `segments::conv::append_with_refs` writes
    /// the record, its mirror row and its refs exactly as `channel_send_core`
    /// does, so the test seeds through the production writer.
    fn conv_segments() -> (tempfile::TempDir, fauna_segment_store::SegmentManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = fauna_segment_store::SegmentManager::new(dir.path().to_path_buf(), "conv");
        (dir, mgr)
    }

    /// A conversation attachment enters through `POST /api/v1/blob` sealed
    /// under the channel epoch key and is named only inside the MLS-sealed
    /// message — nothing the walk reached before step 2g, so every one was
    /// swept ~30 min after upload (found 2026-09-08). With the sender's
    /// plaintext `attachment_refs` recorded beside the mirror row (the
    /// conversation floor ruling) a live record's attachments survive an
    /// immediate zero-grace pass while a true orphan still reclaims — the
    /// proof the sweep really ran, so a mutant dropping the arm reds the
    /// first assertion.
    #[tokio::test]
    async fn gc_keeps_a_live_conversation_attachment_and_still_reclaims_orphans() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_post_dir, post_segs) = post_segments();
        let (_conv_dir, conv_segs) = conv_segments();

        let sealed_photo = put_blob(&db, &store, b"sealed conversation photo").await;
        let sealed_file = put_blob(&db, &store, b"sealed conversation file").await;
        let orphan = put_blob(&db, &store, b"an upload no record names").await;
        let channel = [0xc9u8; 32];
        crate::segments::conv::append_with_refs(
            &conv_segs,
            &db,
            &channel,
            b"sealed-application-envelope",
            1_715_000_000_000,
            &[sealed_photo.digest(), sealed_file.digest()],
            None,
        )
        .await
        .unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&post_segs), 0, None, false)
            .await
            .unwrap();

        assert!(
            store.exists(&sealed_photo).await.unwrap(),
            "a live conversation record's sealed photo was swept"
        );
        assert!(
            store.exists(&sealed_file).await.unwrap(),
            "a live conversation record's sealed file was swept"
        );
        assert_eq!(
            result.conv_attachment_refs, 2,
            "both attachments are pinned by the record's refs"
        );
        assert_eq!(result.deleted_blobs, 1, "only the orphan is swept");
        assert!(
            !store.exists(&orphan).await.unwrap(),
            "the unreferenced upload must still reclaim — else the pin is a no-op sweep"
        );
    }

    /// The pin follows the RECORD's life, not a delete leg: once the store
    /// tombstones the record (a paired nest's `mls.ack` purge; a compaction
    /// input) the reference points at nothing live and the attachment
    /// reclaims on the next pass. A cooperative chat `Delete` is a sealed
    /// message, not a store tombstone — it keeps the bytes exactly as it keeps
    /// the envelope (`conversations.md` § Reactions & message delete → At
    /// rest), so it is deliberately NOT what this test exercises.
    #[tokio::test]
    async fn gc_reclaims_a_conversation_attachment_once_its_record_is_tombstoned() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_post_dir, post_segs) = post_segments();
        let (_conv_dir, conv_segs) = conv_segments();

        let sealed_photo = put_blob(&db, &store, b"a sealed photo that outlives nothing").await;
        let channel = [0xcau8; 32];
        let appended = crate::segments::conv::append_with_refs(
            &conv_segs,
            &db,
            &channel,
            b"sealed-application-envelope",
            1_715_000_000_000,
            &[sealed_photo.digest()],
            None,
        )
        .await
        .unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        garbage_collect(&db, &dyn_store, records(&post_segs), 0, None, false)
            .await
            .unwrap();
        assert!(
            store.exists(&sealed_photo).await.unwrap(),
            "pinned while the record lives"
        );

        let purged = crate::segments::conv::tombstone_up_to_seq(&db, &[channel], appended.seq)
            .await
            .unwrap();
        assert_eq!(purged, 1);
        let result = garbage_collect(&db, &dyn_store, records(&post_segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(
            result.conv_attachment_refs, 0,
            "a tombstoned record pins nothing"
        );
        assert_eq!(result.deleted_blobs, 1);
        assert!(
            !store.exists(&sealed_photo).await.unwrap(),
            "a let-go record's attachment must reclaim — the pin is not a leak"
        );
    }

    /// Step 2h, the paywalled store: a sealed rendered page's body is pinned by
    /// its `web_rendered_sealed` row exactly as a public page is by its
    /// `web_rendered` row, and reclaims once a render replaces the site without
    /// it. The public store is pinned through the real render and serve door in
    /// `tests/conformance_web.rs`; this is the sealed half, seeded directly.
    #[tokio::test]
    async fn gc_keeps_a_sealed_rendered_page_and_reclaims_it_once_its_row_is_gone() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();
        let actor = [7u8; 32];

        let sealed_page = put_blob(&db, &store, b"sealed full page ciphertext").await;
        let claim = db.begin_web_render(&actor).await.unwrap();
        assert!(
            db.upsert_web_rendered_sealed(
                &claim,
                "post/premium.html",
                &sealed_page.digest(),
                "text/html",
                "gold",
                &[9u8; 32],
            )
            .await
            .unwrap()
        );

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.web_rendered_refs, 1, "the sealed page is referenced");
        assert!(
            store.exists(&sealed_page).await.unwrap(),
            "a live sealed page must survive a zero-grace sweep"
        );

        let next = db.begin_web_render(&actor).await.unwrap();
        assert!(db.replace_web_rendered(&next, &[], &[]).await.unwrap());
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.web_rendered_refs, 0);
        assert!(
            !store.exists(&sealed_page).await.unwrap(),
            "a replaced sealed page's body must reclaim — the pin is not a leak"
        );
    }

    /// Step 2h, the in-flight half: a body a render has staged under its claim
    /// is pinned while that render holds the site — whatever its age against
    /// the grace, here zero — and reclaims once a newer render's listing
    /// supersedes it, since the render that staged it can no longer commit.
    /// A superseded render stages nothing more.
    #[tokio::test]
    async fn gc_keeps_a_staged_render_body_until_its_render_loses_the_site() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();
        let actor = [8u8; 32];

        let claim = db.begin_web_render(&actor).await.unwrap();
        let body = ContentHash::of_raw(b"a page body mid-render");
        assert!(
            db.stage_web_render_body(&claim, &body.digest())
                .await
                .unwrap(),
            "the render holds the site, so it stages"
        );
        let stored = put_blob(&db, &store, b"a page body mid-render").await;
        assert_eq!(stored.digest(), body.digest());

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.web_rendered_refs, 1, "the staged body is referenced");
        assert!(
            store.exists(&body).await.unwrap(),
            "a body its render has staged must survive a zero-grace sweep"
        );

        let newer = db.begin_web_render(&actor).await.unwrap();
        let late = ContentHash::of_raw(b"a page the superseded render goes on to write");
        assert!(
            !db.stage_web_render_body(&claim, &late.digest())
                .await
                .unwrap(),
            "a superseded render stages nothing"
        );
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.web_rendered_refs, 0);
        assert!(
            !store.exists(&body).await.unwrap(),
            "a superseded render's staged body must reclaim — the pin is not a leak"
        );

        // The fail-closed clear supersedes every render in flight the same way.
        let again = put_blob(&db, &store, b"the newer render's page").await;
        assert!(
            db.stage_web_render_body(&newer, &again.digest())
                .await
                .unwrap()
        );
        db.clear_web_rendered_owing_restore(&actor).await.unwrap();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.web_rendered_refs, 0);
        assert!(
            !store.exists(&again).await.unwrap(),
            "a render the fail-closed clear superseded pins nothing either"
        );
    }

    /// Step 2i: an inbox envelope the payload store spilled to the blob store
    /// is kept while its `content` row lives — undelivered, and still after the
    /// ack, since the recipient's history lists delivered envelopes — and
    /// reclaims when the account's inbox goes. The spill writes its
    /// `blob_metadata` row (the sweep's premise); the ack refunds the charge
    /// the push recorded on the delivery link.
    #[tokio::test]
    async fn gc_keeps_a_spilled_inbox_envelope_while_its_row_lives_and_its_ack_refunds_quota() {
        async fn inbox_bytes_used(db: &CacheDb, actor: &[u8; 32]) -> i64 {
            db.conn()
                .await
                .query_row(
                    "SELECT inbox_bytes_used FROM users WHERE actor_id = ?1",
                    rusqlite::params![actor.as_slice()],
                    |row| row.get(0),
                )
                .unwrap()
        }
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let recipient = [5u8; 32];
        db.create_user(&recipient, "free", "recipient")
            .await
            .unwrap();

        // The production two-step every inbox writer takes: spill, then name
        // the hash in the inbox row, charging the envelope's full length.
        let payloads =
            crate::payload_store::PayloadStore::new(dyn_store.clone(), db.clone(), 64 * 1024);
        let envelope = vec![0x5Au8; 80 * 1024];
        let (inline, spilled) = payloads.store(&envelope).await.unwrap();
        assert!(inline.is_empty());
        let spilled = spilled.expect("an envelope over the threshold spills");
        let link = db
            .push_inbox_with_quota(&recipient, &envelope, Some(&spilled.digest()))
            .await
            .unwrap();
        assert_eq!(
            inbox_bytes_used(&db, &recipient).await,
            envelope.len() as i64
        );

        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(
            result.content_payload_refs, 1,
            "the inbox row names the spill"
        );
        assert!(
            store.exists(&spilled).await.unwrap(),
            "an undelivered envelope must survive a zero-grace sweep"
        );

        assert_eq!(db.ack_inbox(&recipient, &[link]).await.unwrap(), 1);
        assert_eq!(
            inbox_bytes_used(&db, &recipient).await,
            0,
            "the ack refunds the charge its push recorded"
        );
        garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert!(
            store.exists(&spilled).await.unwrap(),
            "a delivered envelope is still listed, so it still survives"
        );

        db.delete_inbox_for_actor(&recipient).await.unwrap();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.content_payload_refs, 0);
        assert!(
            !store.exists(&spilled).await.unwrap(),
            "an envelope whose row went with the account's inbox must reclaim"
        );
    }

    #[tokio::test]
    async fn gc_removes_unreferenced_blobs() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let actor_id = [1u8; 32];
        db.create_folder("gc-test", &actor_id).await.unwrap();
        let fs = db.get_folder("gc-test").await.unwrap().unwrap();

        // Create a real manifest that references a chunk
        let chunk_hash = ContentHash::from_digest_raw([0xCCu8; 32]);
        store.put(&chunk_hash, b"chunk data").await.unwrap();
        db.put_blob_metadata(&chunk_hash.digest(), 10, "chunk", None, None)
            .await
            .unwrap();

        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::from_digest_raw([0xFFu8; 32]),
            total_size: 10,
            chunk_hashes: vec![chunk_hash],
            chunk_sizes: vec![10],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);
        store.put(&manifest_hash, &manifest_bytes).await.unwrap();
        db.put_blob_metadata(
            &manifest_hash.digest(),
            manifest_bytes.len() as i64,
            "manifest",
            None,
            None,
        )
        .await
        .unwrap();

        // An orphaned blob not referenced by anything
        let orphaned_hash = ContentHash::from_digest_raw([0xBBu8; 32]);
        store.put(&orphaned_hash, b"orphaned data").await.unwrap();
        db.put_blob_metadata(&orphaned_hash.digest(), 13, "chunk", None, None)
            .await
            .unwrap();

        // Create a snapshot referencing the manifest
        let ph: [u8; 32] = *blake3::hash(b"a.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&manifest_hash.digest()),
            100,
            "create",
            Some(fs.id),
            None,
            Some("a.txt"),
        )
        .await
        .unwrap();
        let _snap = db.create_snapshot(fs.id).await.unwrap();

        // Run GC
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();

        // Orphaned blob deleted, manifest AND its chunk preserved
        assert_eq!(result.deleted_blobs, 1);
        assert!(
            store.exists(&manifest_hash).await.unwrap(),
            "manifest should be kept"
        );
        assert!(
            store.exists(&chunk_hash).await.unwrap(),
            "chunk referenced by manifest should be kept"
        );
        assert!(
            !store.exists(&orphaned_hash).await.unwrap(),
            "orphaned blob should be deleted"
        );
    }

    /// Store a `(manifest → one chunk)` pair as real blobs + metadata rows and
    /// return `(manifest_hash, chunk_hash)`. Chunk bytes differ per `tag` so
    /// each pair is exclusive.
    async fn put_manifest_with_chunk(
        db: &Arc<CacheDb>,
        store: &Arc<DiskBlobStore>,
        tag: u8,
    ) -> (ContentHash, ContentHash) {
        let chunk_bytes = vec![tag; 16];
        let chunk_hash = ContentHash::of_raw(&chunk_bytes);
        store.put(&chunk_hash, &chunk_bytes).await.unwrap();
        db.put_blob_metadata(&chunk_hash.digest(), 16, "chunk", None, None)
            .await
            .unwrap();
        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::from_digest_raw([tag; 32]),
            total_size: 16,
            chunk_hashes: vec![chunk_hash],
            chunk_sizes: vec![16],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
        let manifest_hash = ContentHash::of_raw(&bytes);
        store.put(&manifest_hash, &bytes).await.unwrap();
        db.put_blob_metadata(
            &manifest_hash.digest(),
            bytes.len() as i64,
            "manifest",
            None,
            None,
        )
        .await
        .unwrap();
        (manifest_hash, chunk_hash)
    }

    /// M2 pre-bind re-seal reclaim (Piece B, `mls-group-key-material.md` § M2
    /// bullet B): after the owner supersedes a path's pre-re-seal manifest, the
    /// next GC sweep reclaims the old manifest + its exclusive chunk, while the
    /// verified head's blobs survive. Before the supersede, GC reclaims nothing
    /// (the append-only feed pins every manifest).
    #[tokio::test]
    async fn gc_reclaims_superseded_manifest_chunks_after_supersede() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let owner = [0x44u8; 32];
        db.create_folder("reseal-gc", &owner).await.unwrap();
        let fs = db.get_folder("reseal-gc").await.unwrap().unwrap();

        let (m_old, c_old) = put_manifest_with_chunk(&db, &store, 0x01).await;
        let (m_new, c_new) = put_manifest_with_chunk(&db, &store, 0x02).await;

        let ph: [u8; 32] = *blake3::hash(b"pre-bind.txt").as_bytes();
        for mh in [&m_old, &m_new] {
            db.record_sync_change(
                &owner,
                &ph,
                Some(&mh.digest()),
                16,
                "modify",
                Some(fs.id),
                None,
                Some("pre-bind.txt"),
            )
            .await
            .unwrap();
        }

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();

        // Piece A alone (re-record, no supersede): everything stays pinned.
        let before = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(before.deleted_blobs, 0, "append-only feed pins all history");

        // Owner verified the head, supersedes the pre-re-seal row.
        let outcome = db
            .supersede_sync_changes_for_path(fs.id, &ph, &m_new.digest())
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::db::sync_storage::SupersedeOutcome::Marked(1)
        );

        let after = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(after.deleted_blobs, 2, "old manifest + exclusive chunk");
        assert!(
            !store.exists(&m_old).await.unwrap(),
            "old manifest reclaimed"
        );
        assert!(!store.exists(&c_old).await.unwrap(), "old chunk reclaimed");
        assert!(
            store.exists(&m_new).await.unwrap(),
            "head manifest survives"
        );
        assert!(store.exists(&c_new).await.unwrap(), "head chunk survives");
    }

    /// A just-superseded manifest stays pinned through the grace window (keyed
    /// on `superseded_at`, not blob creation — old blobs long predate their
    /// supersede), so an in-flight reader mid-download isn't yanked.
    #[tokio::test]
    async fn gc_grace_buffers_just_superseded_manifests() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let owner = [0x66u8; 32];
        db.create_folder("grace-buffer", &owner).await.unwrap();
        let fs = db.get_folder("grace-buffer").await.unwrap().unwrap();

        let (m_old, c_old) = put_manifest_with_chunk(&db, &store, 0x05).await;
        let (m_new, _c_new) = put_manifest_with_chunk(&db, &store, 0x06).await;

        let ph: [u8; 32] = *blake3::hash(b"inflight.txt").as_bytes();
        for mh in [&m_old, &m_new] {
            db.record_sync_change(
                &owner,
                &ph,
                Some(&mh.digest()),
                16,
                "modify",
                Some(fs.id),
                None,
                Some("inflight.txt"),
            )
            .await
            .unwrap();
        }
        db.supersede_sync_changes_for_path(fs.id, &ph, &m_new.digest())
            .await
            .unwrap();

        // 30-min grace: the just-superseded manifest + chunk survive this sweep.
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 1800, None, false)
            .await
            .unwrap();
        assert_eq!(result.deleted_blobs, 0, "grace buffers the fresh supersede");
        assert!(store.exists(&m_old).await.unwrap());
        assert!(store.exists(&c_old).await.unwrap());
    }

    /// No-data-loss backstop: a snapshot referencing the superseded manifest
    /// keeps it (and its chunk) alive — the supersede removes only the
    /// sync-feed pin, never a snapshot pin.
    #[tokio::test]
    async fn gc_keeps_superseded_manifest_pinned_by_snapshot() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let owner = [0x55u8; 32];
        db.create_folder("snap-pin", &owner).await.unwrap();
        let fs = db.get_folder("snap-pin").await.unwrap().unwrap();

        let (m_old, c_old) = put_manifest_with_chunk(&db, &store, 0x03).await;
        let (m_new, _c_new) = put_manifest_with_chunk(&db, &store, 0x04).await;

        let ph: [u8; 32] = *blake3::hash(b"kept.txt").as_bytes();
        db.record_sync_change(
            &owner,
            &ph,
            Some(&m_old.digest()),
            16,
            "modify",
            Some(fs.id),
            None,
            Some("kept.txt"),
        )
        .await
        .unwrap();
        // Snapshot taken while m_old was the head → snapshot_files pins it.
        let _snap = db.create_snapshot(fs.id).await.unwrap();

        db.record_sync_change(
            &owner,
            &ph,
            Some(&m_new.digest()),
            16,
            "modify",
            Some(fs.id),
            None,
            Some("kept.txt"),
        )
        .await
        .unwrap();
        db.supersede_sync_changes_for_path(fs.id, &ph, &m_new.digest())
            .await
            .unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.deleted_blobs, 0, "snapshot pin protects everything");
        assert!(
            store.exists(&m_old).await.unwrap(),
            "snapshot-pinned manifest survives supersede"
        );
        assert!(
            store.exists(&c_old).await.unwrap(),
            "its chunk survives too"
        );
    }

    /// A reserved rail set's `sync_changes` reference (`__drafts`/`__mls`/…)
    /// points at the stored blob itself — a client-sealed opaque payload, not a
    /// `ChunkManifest`. GC must pin it as a blob WITHOUT attempting (and
    /// failing) a manifest decode, so the sweep still runs and reclaims true
    /// orphans (the example.com 2026-07-09 finding: 30 rail blobs warned
    /// `NotCanonical {trailing data}` every cycle; under the fail-closed sweep
    /// gate, misclassifying them would disable GC permanently).
    #[tokio::test]
    async fn gc_direct_blob_rail_reference_pins_blob_without_decode() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let actor_id = [7u8; 32];
        db.create_folder("__drafts", &actor_id).await.unwrap();
        let fs = db.get_folder("__drafts").await.unwrap().unwrap();

        // A client-sealed rail blob: 0x01 seal-version byte + opaque bytes —
        // undecodable as a ChunkManifest by construction (mirrors the real
        // `fauna.drafts.put` shape: stored raw, content-addressed on the
        // sealed bytes).
        let sealed: Vec<u8> = std::iter::once(0x01u8)
            .chain((0..64).map(|i| i as u8 ^ 0xA5))
            .collect();
        let sealed_hash = ContentHash::of_raw(&sealed);
        store.put(&sealed_hash, &sealed).await.unwrap();
        db.put_blob_metadata(
            &sealed_hash.digest(),
            sealed.len() as i64,
            "chunk",
            None,
            None,
        )
        .await
        .unwrap();

        let ph: [u8; 32] = *blake3::hash(b"conversations").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&sealed_hash.digest()),
            sealed.len() as i64,
            "update",
            Some(fs.id),
            None,
            Some("conversations"),
        )
        .await
        .unwrap();
        // Snapshot the rail set too — snapshot_files rows carry the same
        // direct-blob hash (the example.com population had both).
        let _snap = db.create_snapshot(fs.id).await.unwrap();

        // A true orphan that the sweep must still reclaim.
        let orphan = ContentHash::from_digest_raw([0xBBu8; 32]);
        store.put(&orphan, b"orphaned data").await.unwrap();
        db.put_blob_metadata(&orphan.digest(), 13, "chunk", None, None)
            .await
            .unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();

        assert_eq!(
            result.manifest_decode_failures, 0,
            "rail blob must be classified direct-blob, not decode-attempted"
        );
        assert!(result.direct_blob_refs >= 1);
        assert_eq!(result.deleted_blobs, 1, "sweep still ran");
        assert!(
            store.exists(&sealed_hash).await.unwrap(),
            "rail blob pinned"
        );
        assert!(!store.exists(&orphan).await.unwrap(), "orphan reclaimed");
    }

    /// An ordinary set's reference whose bytes are NOT the preimage of their
    /// content address — genuine at-rest corruption of what may have been a
    /// live manifest — makes reachability incomputable: the sweep must fail
    /// closed and delete NOTHING this run (a live chunk enumerable only
    /// through the failed manifest must survive), rather than silently
    /// treating the incomplete reference set as authoritative. (Hash-INTACT
    /// undecodable bytes are the opposite case — see
    /// `gc_never_manifest_reference_does_not_fail_close` below.)
    #[tokio::test]
    async fn gc_manifest_decode_failure_fails_closed() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let actor_id = [8u8; 32];
        db.create_folder("ordinary", &actor_id).await.unwrap();
        let fs = db.get_folder("ordinary").await.unwrap().unwrap();

        // Bit-rot: the blob at this address no longer contains the bytes that
        // were written under it (its true preimage was a genuine manifest).
        // Modeled by storing bytes under a key they do not hash to — exactly
        // what corruption looks like to a content-addressed reader.
        let corrupt = b"\x01corrupt-not-a-manifest".to_vec();
        let corrupt_hash = ContentHash::from_digest_raw([0xC0u8; 32]);
        store.put(&corrupt_hash, &corrupt).await.unwrap();
        db.put_blob_metadata(
            &corrupt_hash.digest(),
            corrupt.len() as i64,
            "manifest",
            None,
            None,
        )
        .await
        .unwrap();

        // The chunk only that manifest referenced — unreferenced in the walk
        // once the decode fails.
        let stranded_chunk = ContentHash::from_digest_raw([0xCDu8; 32]);
        store
            .put(&stranded_chunk, b"live chunk bytes")
            .await
            .unwrap();
        db.put_blob_metadata(&stranded_chunk.digest(), 16, "chunk", None, None)
            .await
            .unwrap();

        let ph: [u8; 32] = *blake3::hash(b"file.bin").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&corrupt_hash.digest()),
            100,
            "create",
            Some(fs.id),
            None,
            Some("file.bin"),
        )
        .await
        .unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();

        assert_eq!(result.manifest_decode_failures, 1);
        assert_eq!(
            result.deleted_blobs, 0,
            "fail-closed: nothing may be deleted under an undecodable manifest"
        );
        assert!(
            store.exists(&stranded_chunk).await.unwrap(),
            "a chunk enumerable only through the failed manifest survives"
        );
        assert!(store.exists(&corrupt_hash).await.unwrap());
    }

    /// The unit twin of the tier_3
    /// `gc_sweep_survives_a_recorded_reference_that_was_never_a_manifest`:
    /// a manifest-class reference whose bytes are hash-INTACT under their
    /// content address but do not decode was NEVER a manifest (content
    /// addresses are immutable) — no chunk was ever enumerable through it, so
    /// it must not fail-close the sweep. The blob itself stays pinned.
    #[tokio::test]
    async fn gc_never_manifest_reference_does_not_fail_close() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let actor_id = [9u8; 32];
        db.create_folder("ordinary", &actor_id).await.unwrap();
        let fs = db.get_folder("ordinary").await.unwrap().unwrap();

        // Junk stored exactly as `/api/v1/chunks` stores it: content-addressed
        // on the bytes (hash-intact by construction).
        let junk = b"\x01junk-that-never-was-a-manifest".to_vec();
        let junk_hash = ContentHash::of_raw(&junk);
        store.put(&junk_hash, &junk).await.unwrap();
        db.put_blob_metadata(&junk_hash.digest(), junk.len() as i64, "chunk", None, None)
            .await
            .unwrap();

        let ph: [u8; 32] = *blake3::hash(b"junk.bin").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&junk_hash.digest()),
            100,
            "create",
            Some(fs.id),
            None,
            Some("junk.bin"),
        )
        .await
        .unwrap();

        // The orphan reclamation exists for.
        let orphan = ContentHash::from_digest_raw([0xCEu8; 32]);
        store.put(&orphan, b"reclaimable orphan").await.unwrap();
        db.put_blob_metadata(&orphan.digest(), 18, "chunk", None, None)
            .await
            .unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();

        assert_eq!(
            result.manifest_decode_failures, 0,
            "hash-intact never-a-manifest bytes are not corruption — counting \
             them re-arms the client-triggered nest-wide fail-close DoS"
        );
        assert_eq!(result.never_manifest_refs, 1, "surfaced on its own counter");
        assert!(
            !store.exists(&orphan).await.unwrap(),
            "the sweep still runs for everyone else"
        );
        assert!(
            store.exists(&junk_hash).await.unwrap(),
            "the junk blob stays pinned (over-pinning is the safe direction)"
        );
    }

    /// a genuine chunked manifest recorded
    /// under a reserved rail whose bytes CORRUPT gives the probe
    /// `Ok(Some)` + decode-fail — previously folded into "expected rail blob"
    /// (uncounted) and its live chunks were swept. The content-address split
    /// closes it: a rail blob is keyed by its own bytes, so a probe
    /// decode-failure with a hash MISMATCH cannot be a healthy rail — it is
    /// at-rest corruption of what may be a manifest, and must fail-close.
    #[tokio::test]
    async fn gc_probe_fails_closed_on_a_hash_mismatched_rail_reference() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let actor_id = [10u8; 32];
        db.create_folder("__drafts", &actor_id).await.unwrap();
        let fs = db.get_folder("__drafts").await.unwrap().unwrap();

        // Corrupted bytes under a reserved-rail reference: not the preimage of
        // their address at any layer, and not decodable as a manifest.
        let rotted = b"\x01rotted-bytes".to_vec();
        let ref_hash = ContentHash::from_digest_raw([0xD1u8; 32]);
        store.put(&ref_hash, &rotted).await.unwrap();
        db.put_blob_metadata(&ref_hash.digest(), rotted.len() as i64, "chunk", None, None)
            .await
            .unwrap();

        let ph: [u8; 32] = *blake3::hash(b"config-slot").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&ref_hash.digest()),
            13,
            "update",
            Some(fs.id),
            None,
            Some("config-slot"),
        )
        .await
        .unwrap();

        // The chunk that may only be enumerable through the (possibly-manifest)
        // original bytes — must survive the fail-closed run.
        let stranded = ContentHash::from_digest_raw([0xD2u8; 32]);
        store.put(&stranded, b"possibly-live chunk").await.unwrap();
        db.put_blob_metadata(&stranded.digest(), 19, "chunk", None, None)
            .await
            .unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();

        assert_eq!(
            result.manifest_decode_failures, 1,
            "a hash-mismatched rail reference is corruption, not a healthy rail \
             blob — it must count"
        );
        assert_eq!(
            result.deleted_blobs, 0,
            "fail-closed: the possibly-live chunk may not be swept"
        );
        assert!(store.exists(&stranded).await.unwrap());
    }

    #[tokio::test]
    async fn gc_respects_advisory_lock() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        // Acquire lock
        assert!(db.try_acquire_op_lock("gc", -1, "other").await.unwrap());

        // GC should fail to acquire lock
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("lock"));

        db.release_op_lock("gc", -1).await.unwrap();
    }

    #[tokio::test]
    async fn gc_grace_period_protects_recent_blobs() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        // Create a folder with a snapshot referencing one manifest+chunk
        let actor_id = [1u8; 32];
        db.create_folder("grace-test", &actor_id).await.unwrap();
        let fs = db.get_folder("grace-test").await.unwrap().unwrap();

        let chunk_hash = ContentHash::from_digest_raw([0xAAu8; 32]);
        store.put(&chunk_hash, b"chunk").await.unwrap();
        db.put_blob_metadata(&chunk_hash.digest(), 5, "chunk", None, None)
            .await
            .unwrap();

        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::from_digest_raw([0xFFu8; 32]),
            total_size: 5,
            chunk_hashes: vec![chunk_hash],
            chunk_sizes: vec![5],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);
        store.put(&manifest_hash, &manifest_bytes).await.unwrap();
        db.put_blob_metadata(
            &manifest_hash.digest(),
            manifest_bytes.len() as i64,
            "manifest",
            None,
            None,
        )
        .await
        .unwrap();

        let ph: [u8; 32] = *blake3::hash(b"a.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&manifest_hash.digest()),
            100,
            "create",
            Some(fs.id),
            None,
            Some("a.txt"),
        )
        .await
        .unwrap();
        let _snap = db.create_snapshot(fs.id).await.unwrap();

        // An unreferenced blob created just now (within grace period)
        let recent_hash = ContentHash::from_digest_raw([0xBBu8; 32]);
        store.put(&recent_hash, b"recent orphan").await.unwrap();
        db.put_blob_metadata(&recent_hash.digest(), 13, "chunk", None, None)
            .await
            .unwrap();

        // GC with 1800s grace period — recent blob should survive
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 1800, None, false)
            .await
            .unwrap();
        assert_eq!(
            result.deleted_blobs, 0,
            "recent blob should be protected by grace period"
        );
        assert_eq!(result.skipped_grace_period, 1);
        assert!(store.exists(&recent_hash).await.unwrap());
    }

    #[tokio::test]
    async fn gc_zero_grace_deletes_old_unreferenced() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let actor_id = [1u8; 32];
        db.create_folder("zero-grace", &actor_id).await.unwrap();

        // An orphaned blob (no snapshot references it)
        let orphan = ContentHash::from_digest_raw([0xCCu8; 32]);
        store.put(&orphan, b"orphan").await.unwrap();
        db.put_blob_metadata(&orphan.digest(), 6, "chunk", None, None)
            .await
            .unwrap();

        // GC with 0s grace period — should delete immediately
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.deleted_blobs, 1);
        assert!(!store.exists(&orphan).await.unwrap());
    }

    /// The pre-publish compose window (the 2026-09-08 ruling that every
    /// composer uploads at SUBMIT, never at pick): a stored post pins its
    /// blobs from ingest onward (step 2f), but before the post exists the
    /// only thing between an upload and the sweep is the 30-minute
    /// `created_at` grace, and `put_blob_metadata` is `INSERT OR IGNORE`, so
    /// a content-addressed re-POST at submit renews nothing. A photo POSTed
    /// at pick time and written about for longer than the grace is therefore
    /// swept before its post is ingested — silently and unrecoverably, since
    /// the nest holds no other copy. This test pins that outcome so nobody
    /// re-introduces pick-time upload expecting the grace to cover it; the
    /// positive half (a blob named by a post the moment it is stored survives
    /// even a zero-grace sweep) is
    /// `gc_keeps_a_live_posts_photo_and_thumbnail_and_still_reclaims_orphans`.
    #[tokio::test]
    async fn gc_sweeps_a_pick_time_upload_before_its_post_exists() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();
        // The production grace (`gc_scheduler.rs`, 30 min); the composer
        // "wrote for an hour" between pick and submit.
        const GRACE_SECS: i64 = 1800;

        // Pick time: the photo is uploaded, nothing names it yet.
        let photo = put_blob(&db, &store, b"picked an hour before submit").await;
        {
            let conn = db.conn().await;
            conn.execute(
                "UPDATE blob_metadata SET created_at = ?1 WHERE hash = ?2",
                rusqlite::params![
                    crate::db::now_epoch_secs() - 2 * GRACE_SECS,
                    photo.digest().as_slice()
                ],
            )
            .unwrap();
        }
        // A re-POST of the same bytes at submit does not renew the grace.
        let again = put_blob(&db, &store, b"picked an hour before submit").await;
        assert_eq!(again, photo);
        let meta = db
            .get_blob_metadata(&photo.digest())
            .await
            .unwrap()
            .unwrap();
        assert!(
            meta.created_at <= crate::db::now_epoch_secs() - 2 * GRACE_SECS,
            "a content-addressed re-upload must not refresh created_at — if it does, \
             the ruling's premise moved and media.md § Encryption at rest needs a re-read"
        );

        // The 6-hourly sweep runs while the user is still writing.
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let swept = garbage_collect(&db, &dyn_store, records(&segs), GRACE_SECS, None, false)
            .await
            .unwrap();
        assert_eq!(
            swept.deleted_blobs, 1,
            "past grace and named by nothing: swept"
        );
        assert_eq!(swept.skipped_grace_period, 0);
        assert!(
            !store.exists(&photo).await.unwrap(),
            "the pick-time upload was NOT protected — that is the ruling, not a bug"
        );

        // Submit: the post is ingested naming a blob that no longer exists.
        // The pin now holds, but there is nothing left to pin — the photo is
        // gone for good, which is why no composer may upload at pick.
        seed_post(&db, &segs, &post_with_body(0x62, photo_body(photo, None))).await;
        let after = garbage_collect(&db, &dyn_store, records(&segs), GRACE_SECS, None, false)
            .await
            .unwrap();
        assert_eq!(
            after.record_blob_refs, 1,
            "the stored post does pin the hash"
        );
        assert_eq!(after.deleted_blobs, 0);
        assert!(
            !store.exists(&photo).await.unwrap(),
            "ingesting the post later cannot bring the swept bytes back"
        );
    }

    /// An `atproto_blobs` row is a top-level reference like any other: the bytes
    /// an external app uploaded through `com.atproto.repo.uploadBlob` are named
    /// by a repo record and served by `com.atproto.sync.getBlob`, and nothing
    /// else in this box holds a reference to them (the upload leg writes bytes +
    /// `blob_metadata` + the mapping row, and records no `sync_changes` row).
    /// Reachability that stopped at snapshots and sync changes therefore deleted
    /// live user media on the first sweep past grace.
    #[tokio::test]
    async fn gc_keeps_an_atproto_uploaded_blob_a_record_references() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let actor_id = [7u8; 32];
        let cid = "bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4";

        // The three writes F2.4 slice 1's upload ordering performs.
        let media = ContentHash::from_digest_raw([0xABu8; 32]);
        store.put(&media, b"an external app's image").await.unwrap();
        db.put_blob_metadata(&media.digest(), 23, "image/png", None, None)
            .await
            .unwrap();
        db.upsert_atproto_blob(&actor_id, cid, &media.digest())
            .await
            .unwrap();
        // A record then names it — slice 2's stamp.
        db.stamp_atproto_blob_referenced(&actor_id, cid)
            .await
            .unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();

        assert_eq!(
            result.deleted_blobs, 0,
            "the GC deleted media a committed repo record references"
        );
        assert!(
            store.exists(&media).await.unwrap(),
            "an external app's uploaded image was swept: the record still names \
             it and `getBlob` must still serve it"
        );
    }

    /// The two-stage handoff, end to end: the sweeper retires the ROW, and only
    /// then does the box-wide GC find the bytes unreachable and reclaim them.
    /// Neither stage deletes what the other owns — which is what keeps the
    /// reclaim safe for content-addressed bytes a second account may share.
    #[tokio::test]
    async fn gc_reclaims_an_atproto_blob_only_after_its_row_is_swept() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();

        let actor_id = [7u8; 32];
        let cid = "bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4";
        let media = ContentHash::from_digest_raw([0xCDu8; 32]);
        store.put(&media, b"never referenced").await.unwrap();
        db.put_blob_metadata(&media.digest(), 16, "image/png", None, None)
            .await
            .unwrap();
        db.upsert_atproto_blob(&actor_id, cid, &media.digest())
            .await
            .unwrap();

        // Stage 1 has not run: the row pins the bytes even though no record
        // ever named them.
        garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert!(
            store.exists(&media).await.unwrap(),
            "bytes were reclaimed while their row still existed"
        );

        // Stage 1: the reference window elapses and the sweeper takes the row.
        let swept = db
            .delete_unreferenced_atproto_blobs(crate::db::now_epoch_millis() + 1)
            .await
            .unwrap();
        assert_eq!(swept, 1);

        // Stage 2: now — and only now — the bytes are unreachable.
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(result.deleted_blobs, 1);
        assert!(!store.exists(&media).await.unwrap());
    }

    #[tokio::test]
    async fn gc_with_encryption_decodes_manifests() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let key = fauna_core::crypto::BackupKey::from_bytes([0x55u8; 32]);
        let actor_id = [1u8; 32];
        db.create_folder("enc-gc", &actor_id).await.unwrap();
        let fs = db.get_folder("enc-gc").await.unwrap().unwrap();

        // Create an encrypted manifest referencing a chunk
        let chunk_hash = ContentHash::from_digest_raw([0xEEu8; 32]);
        let chunk_data = b"encrypted chunk data";
        let chunk_encrypted = fauna_core::crypto::encrypt_backup_chunk(&key, chunk_data).unwrap();
        store.put(&chunk_hash, &chunk_encrypted).await.unwrap();
        db.put_blob_metadata(
            &chunk_hash.digest(),
            chunk_encrypted.len() as i64,
            "chunk",
            None,
            None,
        )
        .await
        .unwrap();

        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::from_digest_raw([0xFFu8; 32]),
            total_size: 20,
            chunk_hashes: vec![chunk_hash],
            chunk_sizes: vec![20],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
        let manifest_compressed = fauna_core::compress::compress_chunk(&manifest_bytes);
        let manifest_encrypted =
            fauna_core::crypto::encrypt_backup_chunk(&key, &manifest_compressed).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);
        store
            .put(&manifest_hash, &manifest_encrypted)
            .await
            .unwrap();
        db.put_blob_metadata(
            &manifest_hash.digest(),
            manifest_encrypted.len() as i64,
            "manifest",
            None,
            None,
        )
        .await
        .unwrap();

        // Create snapshot referencing the manifest
        let ph: [u8; 32] = *blake3::hash(b"enc.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&manifest_hash.digest()),
            100,
            "create",
            Some(fs.id),
            None,
            Some("enc.txt"),
        )
        .await
        .unwrap();
        let _snap = db.create_snapshot(fs.id).await.unwrap();

        // An unreferenced orphan
        let orphan = ContentHash::from_digest_raw([0x11u8; 32]);
        store.put(&orphan, b"orphan").await.unwrap();
        db.put_blob_metadata(&orphan.digest(), 6, "chunk", None, None)
            .await
            .unwrap();

        // GC with encryption key — should decode manifest, protect its chunk, delete orphan
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, Some(&key), false)
            .await
            .unwrap();
        assert_eq!(result.deleted_blobs, 1, "orphan should be deleted");
        assert!(
            store.exists(&manifest_hash).await.unwrap(),
            "manifest should survive"
        );
        assert!(
            store.exists(&chunk_hash).await.unwrap(),
            "chunk referenced by manifest should survive"
        );
        assert!(
            !store.exists(&orphan).await.unwrap(),
            "orphan should be deleted"
        );
    }

    #[tokio::test]
    async fn gc_missing_metadata_keeps_blob() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();

        let actor_id = [1u8; 32];
        db.create_folder("no-meta", &actor_id).await.unwrap();

        // A blob in the store but with NO metadata row (simulates mid-upload)
        let orphan = ContentHash::from_digest_raw([0xDDu8; 32]);
        store.put(&orphan, b"mid-upload blob").await.unwrap();
        // Deliberately NOT calling put_blob_metadata

        // GC with 0s grace should still NOT delete (no metadata = can't evaluate grace)
        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();
        assert_eq!(
            result.deleted_blobs, 0,
            "blob with no metadata must not be deleted"
        );
        assert!(
            store.exists(&orphan).await.unwrap(),
            "blob should still exist"
        );
    }

    /// Seed two folders sharing one chunk: `full` references only the shared
    /// chunk; `meta` references the shared chunk plus one exclusive chunk.
    /// Returns (meta folder id, shared chunk, exclusive chunk, meta's manifest
    /// hash, full's manifest hash).
    async fn seed_residency_fixture(
        db: &Arc<CacheDb>,
        store: &Arc<DiskBlobStore>,
    ) -> (i64, ContentHash, ContentHash, ContentHash, ContentHash) {
        let actor_id = [7u8; 32];
        db.create_folder("full-set", &actor_id).await.unwrap();
        db.create_folder("meta-set", &actor_id).await.unwrap();
        let full = db.get_folder("full-set").await.unwrap().unwrap();
        let meta = db.get_folder("meta-set").await.unwrap().unwrap();

        let shared = ContentHash::from_digest_raw([0xAAu8; 32]);
        store.put(&shared, b"shared chunk").await.unwrap();
        db.put_blob_metadata(&shared.digest(), 12, "chunk", None, None)
            .await
            .unwrap();
        let exclusive = ContentHash::from_digest_raw([0xABu8; 32]);
        store.put(&exclusive, b"exclusive chunk").await.unwrap();
        db.put_blob_metadata(&exclusive.digest(), 15, "chunk", None, None)
            .await
            .unwrap();

        let put_manifest = async |chunks: Vec<ContentHash>| -> ContentHash {
            let manifest = fauna_core::chunk::ChunkManifest {
                file_hash: ContentHash::from_digest_raw([0xFEu8; 32]),
                total_size: 27,
                chunk_sizes: vec![9; chunks.len()],
                chunk_hashes: chunks,
                stored_hashes: None,
                sealed_hashes: None,
                min_reader: None,
            };
            let bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
            let hash = ContentHash::of_raw(&bytes);
            store.put(&hash, &bytes).await.unwrap();
            db.put_blob_metadata(&hash.digest(), bytes.len() as i64, "manifest", None, None)
                .await
                .unwrap();
            hash
        };
        let full_manifest = put_manifest(vec![shared]).await;
        let meta_manifest = put_manifest(vec![shared, exclusive]).await;

        let ph_full: [u8; 32] = *blake3::hash(b"full.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph_full,
            Some(&full_manifest.digest()),
            9,
            "create",
            Some(full.id),
            None,
            Some("full.txt"),
        )
        .await
        .unwrap();
        let ph_meta: [u8; 32] = *blake3::hash(b"meta.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph_meta,
            Some(&meta_manifest.digest()),
            18,
            "create",
            Some(meta.id),
            None,
            Some("meta.txt"),
        )
        .await
        .unwrap();

        // The owner's consent flip, committed exactly as the handler commits it.
        assert!(
            db.update_folder_for_user(
                "meta-set",
                &actor_id,
                crate::db::FolderUpdate {
                    residency: Some(Some("metadata_only")),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
        );

        (meta.id, shared, exclusive, meta_manifest, full_manifest)
    }

    /// Phase 5 (`file-sync.md` § Content residency) — the GC's residency arm:
    /// a metadata-only folder's MANIFESTS stay pinned (they are the metadata
    /// the folder keeps) while its chunk store keys are deliberately not
    /// reachable through them, so an old seat's uploaded bytes reclaim on the
    /// ordinary GC cadence — EXCEPT a chunk a full-residency folder also
    /// references, which content-addressed dedup must keep.
    #[tokio::test]
    async fn gc_reclaims_a_metadata_only_folders_chunks_but_keeps_its_manifests() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();
        let (_meta_id, shared, exclusive, meta_manifest, full_manifest) =
            seed_residency_fixture(&db, &store).await;

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = garbage_collect(&db, &dyn_store, records(&segs), 0, None, false)
            .await
            .unwrap();

        assert_eq!(
            result.deleted_blobs, 1,
            "exactly the metadata-only folder's exclusive chunk reclaims"
        );
        assert!(
            !store.exists(&exclusive).await.unwrap(),
            "the exclusive chunk must reclaim — absent bytes are the folder's contract"
        );
        assert!(
            store.exists(&shared).await.unwrap(),
            "a chunk a full folder also references must survive (dedup safety)"
        );
        assert!(
            store.exists(&meta_manifest).await.unwrap(),
            "the metadata-only folder's manifest IS metadata and stays"
        );
        assert!(store.exists(&full_manifest).await.unwrap());
    }

    /// Phase 5 — the flip-time chunk drop (`drop_folder_chunk_bytes`): the
    /// consent flip deletes the folder's nest-held chunk bytes NOW, through
    /// the same single reachability oracle, keeping dedup-shared chunks and
    /// every manifest.
    #[tokio::test]
    async fn the_flip_drop_deletes_exclusive_chunks_and_keeps_shared_and_manifests() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let (_segs_dir, segs) = post_segments();
        let (meta_id, shared, exclusive, meta_manifest, _full_manifest) =
            seed_residency_fixture(&db, &store).await;

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let dropped = drop_folder_chunk_bytes(&db, &dyn_store, records(&segs), 0, None, meta_id)
            .await
            .unwrap();

        assert_eq!(dropped, 1, "exactly the exclusive chunk drops");
        assert!(!store.exists(&exclusive).await.unwrap());
        assert!(
            db.get_blob_metadata(&exclusive.digest())
                .await
                .unwrap()
                .is_none(),
            "the dropped chunk's metadata row goes with its bytes"
        );
        assert!(
            store.exists(&shared).await.unwrap(),
            "a dedup-shared chunk never drops"
        );
        assert!(
            store.exists(&meta_manifest).await.unwrap(),
            "manifests are metadata and stay"
        );
        // Idempotent: a second pass finds nothing left to drop.
        assert_eq!(
            drop_folder_chunk_bytes(&db, &dyn_store, records(&segs), 0, None, meta_id)
                .await
                .unwrap(),
            0
        );
    }
}
