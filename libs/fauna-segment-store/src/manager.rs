//! `SegmentManager` — kind-agnostic per-scope coordinator on top of
//! `FramedSegmentStore`. Holds the per-scope DashMap of mutex-guarded
//! `ScopeState`s; serializes appends/reads/finalize_open/compact via the
//! per-scope mutex. Records are opaque bytes (envelope + floor metadata
//! blob); kind-specific encoding lives at callers (libs/fauna-mail/segments/ops.rs etc.).
//!
//! read-your-own-writes POLICY (lifted from MailSegmentManager Plan 2 T9).
//! `FramedSegment::read_record` / `read_records_bulk` only work on
//! *finalized* segments. The currently-open segment for a scope finalizes
//! naturally on bucket change, but reads after a write in the same bucket
//! would otherwise miss those records.
//!
//! Policy: every BODY-read entry point on this manager calls
//! `state.inner.finalize_open()` before opening any segment. That call is
//! idempotent — `None` when no segment is open, `Some(seg_id)` on the
//! one-time transition that closes the current bucket's open segment.
//! After the first read-after-write, subsequent reads no-op until the
//! next write opens a new segment.
//!
//! SIZING is exempt (`record_size` / `record_sizes_for_segment`): the open
//! segment answers size lookups from its in-memory per-record length map,
//! never by finalizing. The quota pre-check sizes records on EVERY inbound
//! delivery, so a flush there rotated the open segment once per message —
//! defeating the monthly bucket rotation the goal doc bounds segment count
//! by — and silently swallowed the rotation's `Finalized` push
//! (`fauna.segments.changed`) that the custodian pull wakes on.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use fauna_cbor::Cid;
use tokio::sync::Mutex;

use crate::{CompactionPlan, FramedSegmentStore, Manifest, ManifestError, compact};

/// Result of `SegmentManager::append_record`.
///
/// `byte_offset` / `byte_length` from the pre-CARv2 shape are gone — the new
/// segment file format (CARv2 + `MultihashIndexSorted`) addresses records by
/// CID, not by `(offset, length)`. Consumers look up by `cid` via
/// `read_envelope_bytes`. `byte_length` stays as a billing / backup
/// convenience.
#[derive(Debug, Clone)]
pub struct AppendOutcome {
    /// Segment id the record landed in (live tail of the manifest after this
    /// append).
    pub segment_id: u32,
    /// The cid that was appended (the carv2 block CID, = the consumer-supplied
    /// content address of the envelope bytes).
    pub cid: Cid,
    /// Byte length of the envelope payload as appended to the segment.
    pub byte_length: u32,
    /// `Some(closed_seg_id)` if this append caused a rotation — the
    /// previously-open segment was finalized to disk in the process. `None`
    /// on no-rotation appends.
    pub finalized: Option<u32>,
}

/// Outcome of [`SegmentManager::rename_scope`] and of every other actor-scoped
/// store's rename ([`rename_scope_dir`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRenameOutcome {
    /// The old scope's directory now serves under the new id.
    Moved,
    /// The old scope had no directory — nothing to move.
    NothingToMove,
    /// Both scopes have directories; nothing was touched. The caller decides
    /// what a collision means for its domain.
    Parked,
}

/// The directory half of an actor-scoped rename, factored out of
/// [`SegmentManager::rename_scope`] so the stores that keep their own per-actor
/// directory layout (the nest's mail/cal/card *placement* journals) move an
/// identity's files by the same three rules instead of each re-deciding what a
/// collision means.
///
/// The rules, in order: **no source** → `NothingToMove`; **destination already
/// exists** → `Parked`, touching nothing, because merging two scopes' files
/// would collide their per-scope record ids and only the caller's domain knows
/// what that should mean; otherwise **rename** → `Moved`.
///
/// It deliberately does *not* evict any in-memory cache — a caller holding one
/// must do that itself, and must do it **before** calling: a cached handle
/// keeps the pre-rename manifest path, and a later write through it would
/// resurrect the old directory beside the moved one.
pub fn rename_scope_dir(src: &Path, dst: &Path) -> std::io::Result<ScopeRenameOutcome> {
    if !src.exists() {
        return Ok(ScopeRenameOutcome::NothingToMove);
    }
    if dst.exists() {
        return Ok(ScopeRenameOutcome::Parked);
    }
    std::fs::rename(src, dst)?;
    Ok(ScopeRenameOutcome::Moved)
}

/// Per-segment metadata for backup/list operations.
#[derive(Debug, Clone)]
pub struct SegmentBackupMeta {
    pub segment_id: u32,
    pub bucket: String,
    pub byte_size: u64,
    pub file_blake3: [u8; 32],
    /// BLAKE3 of the `.meta` sidecar — the pair's other half, advertised
    /// beside `file_blake3` so a backup corpus can anchor both files
    /// (`message-segment-store.md` § Cross-location backup protocol, the
    /// 2026-08-29 sidecar widening).
    pub meta_blake3: [u8; 32],
    /// Count of records in the segment (from the CARv2 segment's sidecar).
    /// Plan 6 T6 added this for the segments-list API.
    pub record_count: u32,
    /// Unix timestamp (seconds) when the segment was created (from the
    /// CARv2 segment's sidecar). Plan 6 T6 added this for the
    /// segments-list API.
    pub created_at_secs: u64,
}

/// Both files of one finalized segment, read together
/// ([`SegmentManager::read_segment_pair`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentPairBytes {
    /// The `.dat` — standard CARv2.
    pub dat: Vec<u8>,
    /// The `.meta` sidecar — canonical dag-cbor `SegmentSidecar`.
    pub meta: Vec<u8>,
}

/// Per-scope in-memory state. Loaded from disk on first access.
struct ScopeState {
    inner: FramedSegmentStore,
    manifest: Manifest,
    manifest_path: PathBuf,
}

pub struct SegmentManager {
    data_dir: PathBuf,
    kind: &'static str,
    state: DashMap<[u8; 32], Arc<Mutex<ScopeState>>>,
}

impl SegmentManager {
    pub fn new(data_dir: PathBuf, kind: &'static str) -> Self {
        Self {
            data_dir,
            kind,
            state: DashMap::new(),
        }
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// Derive the manifest path for a scope.
    ///
    /// Layout: `<data_dir>/__<kind>/<scope_hex>/manifest.<kind>`
    fn manifest_path(&self, scope_id: &[u8; 32]) -> PathBuf {
        self.scope_dir(scope_id)
            .join(format!("manifest.{}", self.kind))
    }

    /// Root directory for one scope's segments.
    ///
    /// Layout: `<data_dir>/__<kind>/<scope_hex>/` (kind-agnostic — the manager's
    /// `kind` selects `__mail` / `__conv` / …, `scope_id` the per-scope subdir).
    /// `pub` so restore-from-disk paths (e.g. `restore_conv`) can locate the
    /// pinned `seg-{:08}.dat` files without minting a per-kind path helper.
    pub fn scope_dir(&self, scope_id: &[u8; 32]) -> PathBuf {
        self.data_dir
            .join(format!("__{}", self.kind))
            .join(hex::encode(scope_id))
    }

    /// Move one scope's on-disk directory to a new scope id, evicting any
    /// cached in-memory state for both ids.
    ///
    /// Built for identity succession (`succession-aftermath.md` § Re-key scope:
    /// ownership moves with the account; the actor-scoped segment kinds move
    /// on disk here). The eviction is load-bearing, not hygiene: a cached
    /// `ScopeState` holds the pre-rename manifest path, and a later manifest
    /// rewrite through it would resurrect the old directory beside the moved
    /// one.
    ///
    /// A collision ([`ScopeRenameOutcome::Parked`]) touches nothing — merging
    /// two scopes' segment files would collide their per-scope segment ids,
    /// so what a collision means is the caller's judgment, never this
    /// crate's. Callers rename scopes whose old id is already refused
    /// upstream; a write racing the rename on the old id is the caller's
    /// boot-reconcile to heal.
    pub fn rename_scope(
        &self,
        old_scope: &[u8; 32],
        new_scope: &[u8; 32],
    ) -> std::io::Result<ScopeRenameOutcome> {
        self.state.remove(old_scope);
        self.state.remove(new_scope);
        rename_scope_dir(&self.scope_dir(old_scope), &self.scope_dir(new_scope))
    }

    /// Get or create the per-scope lock + state. The DashMap entry is created
    /// lazily on first access. Loading the manifest from disk happens before
    /// the DashMap insert; if another caller raced us the placeholder we
    /// constructed is dropped and the winning Arc is returned.
    async fn scope_state(
        &self,
        scope_id: &[u8; 32],
    ) -> Result<Arc<Mutex<ScopeState>>, ManagerError> {
        if let Some(existing) = self.state.get(scope_id) {
            return Ok(existing.clone());
        }
        let scope_dir = self.scope_dir(scope_id);
        let manifest_path = self.manifest_path(scope_id);
        let manifest = Manifest::load_or_empty(&manifest_path, self.kind)?;
        let inner = FramedSegmentStore::new(scope_dir, self.kind, *scope_id)
            .map_err(ManagerError::Segment)?;
        let state = Arc::new(Mutex::new(ScopeState {
            inner,
            manifest,
            manifest_path,
        }));
        // Race-safe insert: if another caller raced us, return theirs.
        let entry = self.state.entry(*scope_id).or_insert_with(|| state.clone());
        Ok(entry.clone())
    }

    /// Append one record with a bucket derived from the current UTC month
    /// (`"YYYY-MM"`). Convenience wrapper for callers that don't need
    /// explicit bucket placement.
    pub async fn append_record(
        &self,
        scope_id: &[u8; 32],
        cid: Cid,
        envelope_bytes: &[u8],
        floor_metadata_blob: &[u8],
    ) -> Result<AppendOutcome, ManagerError> {
        let bucket = bucket_for_now();
        self.append_record_with_bucket(scope_id, cid, envelope_bytes, floor_metadata_blob, &bucket)
            .await
    }

    /// Append one record with an explicit bucket key (e.g. `"2026-05"`).
    ///
    /// Coordinates: pass envelope bytes + floor blob to `FramedSegmentStore::append`,
    /// update the manifest, and save atomically. Returns an [`AppendOutcome`]
    /// carrying the new segment id, cid, and whether a previously-open
    /// segment was rotated closed.
    pub async fn append_record_with_bucket(
        &self,
        scope_id: &[u8; 32],
        cid: Cid,
        envelope_bytes: &[u8],
        floor_metadata_blob: &[u8],
        bucket: &str,
    ) -> Result<AppendOutcome, ManagerError> {
        let scope_arc = self.scope_state(scope_id).await?;
        let mut state = scope_arc.lock().await;

        // Detect rotation BEFORE calling `inner.append`. The store rotates
        // internally on bucket mismatch / first append, but its return type
        // only carries the new segment id. To surface the just-closed segment
        // id for a Finalized push, pre-finalize any open segment in a
        // different bucket here and capture its id.
        //
        // CRASH-RECOVERY NOTE: after a process restart the store has no open
        // segment, so this branch leaves `finalized = None`. The store's
        // internal first-append rotation still fires; we treat that as "no
        // previously-open segment to finalize".
        let finalized = match state.inner.open_bucket() {
            Some(open_bucket) if open_bucket != bucket => state.inner.finalize_open()?,
            _ => None,
        };

        // Always pass the *next candidate* id from the manifest; the store
        // uses it only on rotation. On no-rotation, the store ignores the
        // candidate and reuses the open segment's id.
        let next_candidate = state.manifest.kind_manifest.next_seg_id;

        let seg_id = state.inner.append(
            bucket,
            next_candidate,
            cid,
            envelope_bytes,
            floor_metadata_blob,
        )?;

        // If the store actually used the candidate (i.e. it rotated),
        // commit it to the manifest.
        if seg_id == next_candidate {
            let assigned = state.manifest.kind_manifest.append_segment();
            debug_assert_eq!(
                assigned, seg_id,
                "segment id mismatch between store and manifest"
            );
        }

        let byte_length = envelope_bytes.len() as u32;

        // Save manifest atomically. Cheap (low KB).
        state
            .manifest
            .save_atomic(&state.manifest_path)
            .map_err(ManagerError::Manifest)?;

        Ok(AppendOutcome {
            segment_id: seg_id,
            cid,
            byte_length,
            finalized,
        })
    }

    /// Read the envelope bytes for a specific record from a specific segment.
    ///
    /// Applies the read-your-own-writes policy: finalizes any currently-open
    /// segment before reading, so a record appended earlier in the same
    /// process is always observable. Idempotent — no-ops when no segment is
    /// open.
    pub async fn read_envelope_bytes(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
        cid: &Cid,
    ) -> Result<Vec<u8>, ManagerError> {
        let scope_arc = self.scope_state(scope_id).await?;
        // Briefly lock to flush + open segment; don't hold the lock across the
        // file read (read_record opens its own File handle).
        let segment = {
            let mut state = scope_arc.lock().await;
            // Flush-on-read: idempotent, no-op when nothing's open.
            state.inner.finalize_open()?;
            state.inner.open_segment(segment_id)?
        };
        match segment.read_record(cid).map_err(ManagerError::Segment)? {
            Some(bytes) => Ok(bytes),
            None => Err(ManagerError::RecordNotFound {
                segment_id,
                cid: *cid,
            }),
        }
    }

    /// Read all records from a segment, returning `(cid, envelope_bytes,
    /// floor_blob_bytes)` tuples for each record in append order.
    ///
    /// Applies the read-your-own-writes policy.
    pub async fn read_envelopes_bulk(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
    ) -> Result<Vec<(Cid, Vec<u8>, Vec<u8>)>, ManagerError> {
        let scope_arc = self.scope_state(scope_id).await?;
        let segment = {
            let mut state = scope_arc.lock().await;
            // Flush-on-read; idempotent.
            state.inner.finalize_open()?;
            state.inner.open_segment(segment_id)?
        };

        // Collect all entries so we can hold references for the bulk read.
        let entries: Vec<_> = segment.iter_records().collect();
        let payloads = segment.read_records_bulk(&entries)?;

        let mut out = Vec::with_capacity(entries.len());
        for (entry, payload_opt) in entries.iter().zip(payloads) {
            let payload = payload_opt.ok_or_else(|| {
                ManagerError::Segment(crate::SegmentStoreError::InvalidSegment(format!(
                    "record {} present in sidecar but absent from carv2 data section",
                    entry.cid
                )))
            })?;
            out.push((entry.cid, payload, entry.floor_metadata.clone()));
        }
        Ok(out)
    }

    /// Read one record's envelope bytes alongside its floor metadata blob.
    ///
    /// Returns `(envelope_bytes, floor_blob_bytes)`. Callers decode the blobs
    /// with their kind-specific deserializers.
    ///
    /// Applies the read-your-own-writes policy.
    pub async fn read_record_with_floor_bytes(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
        cid: &Cid,
    ) -> Result<(Vec<u8>, Vec<u8>), ManagerError> {
        let scope_arc = self.scope_state(scope_id).await?;
        let segment = {
            let mut state = scope_arc.lock().await;
            // Flush-on-read; idempotent.
            state.inner.finalize_open()?;
            state.inner.open_segment(segment_id)?
        };
        let entry =
            segment
                .iter_records()
                .find(|e| &e.cid == cid)
                .ok_or(ManagerError::RecordNotFound {
                    segment_id,
                    cid: *cid,
                })?;
        let floor_blob = entry.floor_metadata.clone();
        let envelope_bytes = segment
            .read_record(cid)
            .map_err(ManagerError::Segment)?
            .ok_or(ManagerError::RecordNotFound {
                segment_id,
                cid: *cid,
            })?;
        Ok((envelope_bytes, floor_blob))
    }

    /// Look up one record's CARv2 block byte-length — the sealed-ciphertext
    /// size IMAP RFC822.SIZE reports — derived from the segment's
    /// `MultihashIndexSorted` index by Cid, **not** a SQL mirror column
    /// (`imap-server.md` §§ SEARCH, QUOTA). The block body is never read.
    /// Errors `RecordNotFound` when the cid isn't in the segment.
    ///
    /// EXEMPT from the read-your-own-writes flush: the open segment answers
    /// from its in-memory length map (see module doc).
    pub async fn record_size(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
        cid: &Cid,
    ) -> Result<u64, ManagerError> {
        let scope_arc = self.scope_state(scope_id).await?;
        let segment = {
            let state = scope_arc.lock().await;
            // NO flush-on-read — sizing serves the OPEN segment from its
            // in-memory length map and a finalized segment from its on-disk
            // index; forcing a finalize here would rotate the open segment
            // early (see `record_sizes_for_segment`).
            if let Some(lens) = state.inner.open_segment_record_lens(segment_id, &[cid]) {
                return lens[0].ok_or(ManagerError::RecordNotFound {
                    segment_id,
                    cid: *cid,
                });
            }
            state.inner.open_segment(segment_id)?
        };
        segment
            .record_block_len(cid)
            .map_err(ManagerError::Segment)?
            .ok_or(ManagerError::RecordNotFound {
                segment_id,
                cid: *cid,
            })
    }

    /// Look up many records' block byte-lengths within a single segment,
    /// reusing one file handle + parsed index across the batch. Returns sizes
    /// in the same order as `cids`, with `None` for any cid absent from the
    /// segment. The bulk primitive for the STORAGE-quota SUM and SEARCH
    /// `LARGER`|`SMALLER` size walks: the nest groups a mailbox's
    /// `(segment_id, record_cid)` rows by segment and calls this once per
    /// segment. No block bodies are read.
    ///
    /// EXEMPT from the read-your-own-writes flush: the open segment answers
    /// from its in-memory length map (see module doc).
    pub async fn record_sizes_for_segment(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
        cids: &[&Cid],
    ) -> Result<Vec<Option<u64>>, ManagerError> {
        let scope_arc = self.scope_state(scope_id).await?;
        let segment = {
            let state = scope_arc.lock().await;
            // NO flush-on-read here, deliberately — unlike the body-read
            // paths, sizing never needs the on-disk CARv2 index: the OPEN
            // segment answers from its in-memory length map, and a
            // finalized segment's index is already on disk. The quota
            // pre-check sizes records on EVERY inbound delivery, so a
            // finalize here rotated the open segment once per message
            // (defeating the monthly bucket rotation) and silently ate the
            // rotation `Finalized` push (`fauna.segments.changed`) the
            // custodian pull wakes on.
            if let Some(lens) = state.inner.open_segment_record_lens(segment_id, cids) {
                return Ok(lens);
            }
            state.inner.open_segment(segment_id)?
        };
        segment
            .record_block_lens(cids)
            .map_err(ManagerError::Segment)
    }

    /// Finalize the scope's currently-open segment (if any). Idempotent.
    ///
    /// Returns `Some(closed_seg_id)` if a segment was actually finalized;
    /// `None` if nothing was open.
    pub async fn flush(&self, scope_id: &[u8; 32]) -> Result<Option<u32>, ManagerError> {
        let scope_arc = self.scope_state(scope_id).await?;
        let mut state = scope_arc.lock().await;
        Ok(state.inner.finalize_open()?)
    }

    /// Public alias of `flush` for callers that read straight from segment
    /// files and want the finalize-on-read intent spelled out.
    pub async fn finalize_open(&self, scope_id: &[u8; 32]) -> Result<Option<u32>, ManagerError> {
        self.flush(scope_id).await
    }

    /// Read-only snapshot of a scope's `Manifest`. Returns `Manifest::empty`
    /// when no manifest file exists yet.
    pub async fn load_manifest(&self, scope_id: &[u8; 32]) -> Result<Manifest, ManagerError> {
        let path = self.manifest_path(scope_id);
        Ok(Manifest::load_or_empty(&path, self.kind)?)
    }

    /// Adopt segment files that a *different* writer already placed in this
    /// scope's segment area, so the store stops seeing them as reclaimable
    /// orphans.
    ///
    /// The store's rotation unlinks whatever `.dat`/`.meta` already sits at the
    /// id it is about to open, and that reclaim is correct: a *committed*
    /// segment can never legitimately be there, because
    /// [`Self::append_record_with_bucket`] and [`Self::compact_with_filter`]
    /// both advance `next_seg_id` in the same breath as they write. A writer
    /// that lands committed segments WITHOUT advancing it breaks exactly that
    /// invariant, and the next ordinary append then deletes real records while
    /// performing textbook crash recovery.
    ///
    /// The re-seed ceremony is the one such writer: it reconstitutes a corpus
    /// from custody straight onto disk, under ids the source nest chose. It
    /// calls this as soon as the halves land — the ids become live and
    /// `next_seg_id` moves past the highest of them. Idempotent, so a retry
    /// after a crash between the write and this call converges instead of
    /// double-counting.
    ///
    /// Holds the per-scope lock and updates the CACHED state, not merely the
    /// file on disk: a scope whose manifest was already loaded in this process
    /// — the ceremony's own `finalize_open` is enough to load it — would
    /// otherwise keep handing out `next_seg_id = 1` from memory until a
    /// restart, and the collision would fire without any crash at all.
    ///
    /// Assumes the caller's own empty-target rule: these ids are new to the
    /// scope, so none of them is sitting in `tombstoned_segments` awaiting GC.
    ///
    /// `counter_floor` is the **ledger's** generation — the dead source's own
    /// saved `next_seg_id` as its `manifest.<kind>` mirror carried it
    /// (`LiveManifestMirror::next_segment_id_seen`) — and the counter lands at
    /// or above it. Adopting the ids alone would set the counter to the
    /// highest adopted id plus one, which is BELOW the source's counter
    /// whenever its top segment had compacted to nothing: the re-seeded nest
    /// would then mint ids the dead nest already minted and retired, and its
    /// first ledger would read as a rollback to every device that verified
    /// the dead nest's. Pass `0` when there is
    /// no ledger to honour.
    pub async fn adopt_segments(
        &self,
        scope_id: &[u8; 32],
        segment_ids: &[u32],
        counter_floor: u32,
    ) -> Result<(), ManagerError> {
        check_counter_floor(counter_floor)?;
        let scope_arc = self.scope_state(scope_id).await?;
        let mut state = scope_arc.lock().await;
        let km = &mut state.manifest.kind_manifest;
        let before = km.clone();
        for &id in segment_ids {
            if !km.live_segments.contains(&id) {
                let at = km.live_segments.partition_point(|&x| x < id);
                km.live_segments.insert(at, id);
            }
            let past = id.checked_add(1).expect("u32 segment counter exhausted");
            km.next_seg_id = km.next_seg_id.max(past);
        }
        raise_counter(km, counter_floor);
        if *km == before {
            return Ok(());
        }
        state
            .manifest
            .save_atomic(&state.manifest_path)
            .map_err(ManagerError::Manifest)?;
        Ok(())
    }

    /// **The counter floor** — raise the scope's saved `next_seg_id` to at
    /// least `floor`, and never lower it (`segment-backup-protocol.md`
    /// § Client-device custodian (pull) → *Restore* → *Recovery into the
    /// lived-in nest that regressed*, part (0)).
    ///
    /// [`Self::adopt_segments`]' floor half, standing alone: after a source
    /// came back from an older copy of its data directory, a device that
    /// accepts the regression floors the counter to the generation it had
    /// pinned, so every id the lost copy numbered is spent here too and the
    /// destination's not-yet-reused lost segments stop being overwritten.
    ///
    /// Monotonic and idempotent, taken under the scope's lock: an open segment
    /// finishes at its own id and the next rotation takes the floor. A floor
    /// the id space cannot honour ([`MAX_COUNTER_FLOOR`]) is refused
    /// ([`ManagerError::CounterFloorOutOfRange`]) — the caller's worst case
    /// against its own scope is spending ids, never exhausting them. Returns
    /// the counter as it now stands.
    pub async fn floor_counter(
        &self,
        scope_id: &[u8; 32],
        floor: u32,
    ) -> Result<u32, ManagerError> {
        check_counter_floor(floor)?;
        let scope_arc = self.scope_state(scope_id).await?;
        let mut state = scope_arc.lock().await;
        if raise_counter(&mut state.manifest.kind_manifest, floor) {
            state
                .manifest
                .save_atomic(&state.manifest_path)
                .map_err(ManagerError::Manifest)?;
        }
        Ok(state.manifest.kind_manifest.next_seg_id)
    }

    /// Compact one bucket per the supplied plan, with a caller-supplied
    /// liveness filter. The closure decides per-record whether the record
    /// survives compaction. Used by nest's `segments::mail::compact_bucket`
    /// to consult its own SQLite `segment_records` mirror (Plan 6 lift).
    ///
    /// Holds the per-scope lock; finalizes any open segment first
    /// (read-your-own-writes). Layer 3 flipped the filter signature from
    /// `Fn(&[u8])` to `Fn(&Cid)` because the CARv2 segment-store keys on Cid.
    ///
    /// Returns `Some(new_seg_id)` if at least one record survived, `None` if
    /// all inputs were filtered out by `is_alive`.
    pub async fn compact_with_filter<F>(
        &self,
        scope_id: &[u8; 32],
        plan: CompactionPlan,
        new_seg_candidate: u32,
        is_alive: F,
    ) -> Result<Option<u32>, ManagerError>
    where
        F: Fn(&Cid) -> bool,
    {
        let scope_arc = self.scope_state(scope_id).await?;
        let mut state = scope_arc.lock().await;

        // read-your-own-writes: finalize before opening segments for compaction.
        state.inner.finalize_open()?;

        let result = compact(&mut state.inner, plan.clone(), new_seg_candidate, is_alive)
            .map_err(ManagerError::Segment)?;

        // Manifest swap (ordering: file-write-first → manifest-swap-second).
        if let Some(new_id) = result {
            let assigned = state.manifest.kind_manifest.append_segment();
            debug_assert_eq!(
                assigned, new_id,
                "manifest counter desynced from compact_with_filter() new_segment_id"
            );
        }
        for input_seg in &plan.inputs {
            state.manifest.kind_manifest.tombstone_segment(*input_seg);
        }
        state
            .manifest
            .save_atomic(&state.manifest_path)
            .map_err(ManagerError::Manifest)?;

        Ok(result)
    }

    /// Per-segment metadata for backup/list operations.
    ///
    /// Applies the read-your-own-writes policy: finalizes any currently-open
    /// segment before reading, so byte_size and file_blake3 are always
    /// consistent with the on-disk state. Idempotent — no-ops when no segment
    /// is open.
    pub async fn describe_segment_for_backup(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
    ) -> Result<SegmentBackupMeta, ManagerError> {
        let scope_arc = self.scope_state(scope_id).await?;
        let segment = {
            let mut state = scope_arc.lock().await;
            // Flush-on-read: idempotent, no-op when nothing's open.
            state.inner.finalize_open()?;
            state.inner.open_segment(segment_id)?
        };
        let byte_size = segment.size_bytes()?;
        let file_blake3 = segment.file_blake3()?;
        let meta_blake3 = segment.meta_blake3()?;
        Ok(SegmentBackupMeta {
            segment_id,
            bucket: segment.header.bucket.clone(),
            byte_size,
            file_blake3,
            meta_blake3,
            record_count: segment.header.record_count,
            created_at_secs: segment.header.created_at_secs,
        })
    }

    /// Read a finalized segment's **pair** — the `.dat` and its `.meta`
    /// sidecar — as one observation.
    ///
    /// Finalize-on-read like [`Self::describe_segment_for_backup`], and both
    /// files are read **under the scope lock**, so no append can rotate or
    /// re-finalize the scope between the two reads: the bytes returned are the
    /// exact pair that `open()` would accept. This is the backup arm's read
    /// door (`bins/fauna-nest/src/segments/backup_source.rs`), which exists
    /// because a segment shipped without its sidecar cannot be reopened —
    /// `record_order` and the per-record floor metadata live only in the
    /// `.meta` (`message-segment-store.md` § Segment file format).
    pub async fn read_segment_pair(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
    ) -> Result<SegmentPairBytes, ManagerError> {
        let scope_arc = self.scope_state(scope_id).await?;
        let mut state = scope_arc.lock().await;
        state.inner.finalize_open()?;
        let segment = state.inner.open_segment(segment_id)?;
        let dat = std::fs::read(segment.path())?;
        let meta = std::fs::read(segment.meta_path())?;
        Ok(SegmentPairBytes { dat, meta })
    }

    /// On-disk path to `seg-NNNNNNNN.dat` for a scope's segment.
    ///
    /// Layout: `<data_dir>/__<kind>/<scope_hex>/seg-NNNNNNNN.dat`
    pub fn segment_file_path(&self, scope_id: &[u8; 32], segment_id: u32) -> PathBuf {
        self.scope_dir(scope_id)
            .join(format!("seg-{:08}.dat", segment_id))
    }

    /// On-disk path to the companion `seg-NNNNNNNN.meta` sidecar.
    ///
    /// The pair's naming rule lives in [`crate::segment`] and nowhere else —
    /// a caller that needs the sidecar (the segment-pair route serving an
    /// adopting replica, `account-data-plane.md` § the bootstrap contract)
    /// asks for it here rather than swapping the extension itself, so the
    /// layout keeps exactly one owner.
    pub fn segment_meta_path(&self, scope_id: &[u8; 32], segment_id: u32) -> PathBuf {
        crate::segment::meta_path_for(&self.segment_file_path(scope_id, segment_id))
    }
}

/// The highest counter floor a scope accepts: the id space less 2^20 ids of
/// headroom, so a floored scope can still append for longer than any real
/// scope rotates. A constant the binary sets — no human chooses it.
pub const MAX_COUNTER_FLOOR: u32 = u32::MAX - (1 << 20);

/// Refuse a counter floor the id space cannot honour.
pub fn check_counter_floor(floor: u32) -> Result<(), ManagerError> {
    if floor > MAX_COUNTER_FLOOR {
        return Err(ManagerError::CounterFloorOutOfRange { floor });
    }
    Ok(())
}

/// Raise `km`'s counter to `floor` if it is below; `true` when it moved. The
/// one raise both [`SegmentManager::floor_counter`] and
/// [`SegmentManager::adopt_segments`] apply, and the one a placement journal
/// applies to its own manifest.
pub fn raise_counter(km: &mut crate::KindManifest, floor: u32) -> bool {
    if km.next_seg_id >= floor {
        return false;
    }
    km.next_seg_id = floor;
    true
}

/// Calendar-month bucket key for an epoch-seconds timestamp: `"YYYY-MM"` (UTC).
///
/// Pure days/months arithmetic — no chrono — so the bucketing rule is shared
/// by every kind (mail, conv, …) without dragging chrono into the kind crates.
/// The civil-from-days split itself belongs to
/// [`fauna_core::caltime::civil_from_days`]; what lives here is only the
/// bucketing rule laid over it.
/// `libs/fauna-mail/src/segments/paths.rs::bucket_for` delegates here whenever
/// `fauna-segment-store` is a dependency at all, and keeps a byte-identical
/// `bucket_for_pure` copy for the builds where it is not (the wasm content
/// index). The copy MUST produce identical output to this function; that is
/// what `paths.rs::tests::bucket_matches_segment_store` asserts, comparing the
/// copy against this owner over every day boundary to 2100.
///
/// `epoch_secs` is epoch **seconds**. Callers with epoch milliseconds (the
/// nest's `now_epoch_millis()` shape) divide by 1000 first.
pub fn bucket_for(epoch_secs: i64) -> String {
    let days_total = (epoch_secs.max(0) as u64 / 86_400) as i64;
    let (year, month, _day) = fauna_core::caltime::civil_from_days(days_total);
    format!("{year:04}-{month:02}")
}

/// Calendar-month bucket key for the current UTC time: `"YYYY-MM"`.
///
/// Thin wrapper over [`bucket_for`] at the current wall-clock time.
fn bucket_for_now() -> String {
    let now_secs = chrono::Utc::now().timestamp();
    bucket_for(now_secs)
}

impl From<crate::SegmentStoreError> for ManagerError {
    fn from(e: crate::SegmentStoreError) -> Self {
        ManagerError::Segment(e)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    /// A counter floor above [`MAX_COUNTER_FLOOR`] — one the id space cannot
    /// honour and still leave room to append.
    #[error("counter floor {floor} is above the id space's headroom ({max})", max = MAX_COUNTER_FLOOR)]
    CounterFloorOutOfRange { floor: u32 },
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("framed segment: {0}")]
    Segment(crate::SegmentStoreError),
    #[error("segment {0} not found")]
    SegmentNotFound(u32),
    #[error("record {cid} not found in segment {segment_id}")]
    RecordNotFound { segment_id: u32, cid: Cid },
}

/// One identity's actor-scoped on-disk store, as identity succession sees it.
///
/// Succession moves ownership of a whole corpus old→new, and the corpus is
/// spread over stores that share nothing but this shape: a per-actor directory
/// and (usually) an in-memory cache keyed by actor id. `SegmentManager` is one;
/// the nest's mail/cal/card **placement journals** are three more, with their
/// own layout and no `SegmentManager` underneath.
///
/// The trait exists so the succession heal is written **once** over "every
/// actor-scoped store" rather than once per store type — which is also what
/// makes a future store fall into the heal by implementing this, instead of by
/// someone remembering to add it to a list in the nest.
///
/// **`Send + Sync` is load-bearing, not decoration.** The nest holds every one
/// of these in an `Arc` and passes the whole set as `&[&dyn ActorScopedStore]`
/// into the async boot reconcile, so the slice is alive across an `.await`
/// inside the serve future — which `tokio::spawn` requires to be `Send`. Drop
/// the bound and that requirement is still there; it just stops being stated
/// here and resurfaces as an `E0277` against a `tokio::spawn` in a serve-loop
/// *test*, hundreds of lines from the store that actually broke it.
pub trait ActorScopedStore: Send + Sync {
    /// Short kind name for logs and park reports (`"mail"`,
    /// `"mail-placement"`, …).
    fn kind(&self) -> &'static str;

    /// Move this identity's files old→new, evicting any cached per-actor
    /// handle for **both** ids first (a cached handle holds the pre-rename
    /// paths). Implementations delegate the directory rules to
    /// [`rename_scope_dir`].
    fn rename_actor(&self, old: &[u8; 32], new: &[u8; 32]) -> std::io::Result<ScopeRenameOutcome>;
}

impl ActorScopedStore for SegmentManager {
    fn kind(&self) -> &'static str {
        SegmentManager::kind(self)
    }

    fn rename_actor(&self, old: &[u8; 32], new: &[u8; 32]) -> std::io::Result<ScopeRenameOutcome> {
        self.rename_scope(old, new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn manager_in(tmp: &TempDir) -> SegmentManager {
        SegmentManager::new(tmp.path().to_path_buf(), "test")
    }

    const SCOPE: [u8; 32] = [0x77u8; 32];

    fn body(n: u8) -> Vec<u8> {
        format!("opaque-envelope-{n}").into_bytes()
    }

    // --- the saved counter is the ledger's generation ---

    /// A compaction whose every input record is dead retires its inputs and
    /// mints nothing — so the greatest live id can DROP while `next_seg_id`
    /// stands. The backup ledger carries the counter for exactly this reason:
    /// a device pinning "the highest generation I verified" must see the
    /// counter, not the live maximum, or an honest nest alarms.
    #[tokio::test]
    async fn empty_output_compaction_of_the_top_segment_keeps_the_counter() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        // Two buckets → two segments: 1 (2026-05) and 2 (2026-06).
        for (n, bucket) in [(1u8, "2026-05"), (2u8, "2026-06")] {
            let env = body(n);
            mgr.append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", bucket)
                .await
                .expect("append");
        }
        mgr.finalize_open(&SCOPE).await.expect("finalize");
        let km = mgr.load_manifest(&SCOPE).await.expect("load").kind_manifest;
        assert_eq!(km.live_segments, vec![1, 2]);
        assert_eq!(km.next_seg_id, 3);

        // Compact the TOP segment with nothing alive: no output, no new id.
        let plan = CompactionPlan {
            inputs: vec![2],
            bucket: "2026-06".into(),
        };
        let out = mgr
            .compact_with_filter(&SCOPE, plan, 3, |_| false)
            .await
            .expect("compact");
        assert_eq!(out, None, "every record dead ⇒ nothing written");

        let km = mgr.load_manifest(&SCOPE).await.expect("load").kind_manifest;
        assert_eq!(km.live_segments, vec![1], "the top segment retired");
        assert_eq!(
            km.next_seg_id, 3,
            "the counter never decreases: it now sits above the live maximum plus one"
        );
    }

    /// `adopt_segments` raises the counter to the ledger's generation even when
    /// that is above every adopted id plus one — the re-seed path's half of the
    /// same property — and is idempotent.
    #[tokio::test]
    async fn adopt_segments_honours_the_ledger_counter_floor() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        mgr.adopt_segments(&SCOPE, &[4, 2], 9).await.expect("adopt");
        let km = mgr.load_manifest(&SCOPE).await.expect("load").kind_manifest;
        assert_eq!(km.live_segments, vec![2, 4]);
        assert_eq!(km.next_seg_id, 9, "the floor wins over max(id) + 1");

        // A floor below what the ids imply changes nothing; a re-run is a no-op.
        mgr.adopt_segments(&SCOPE, &[4, 2], 1)
            .await
            .expect("adopt again");
        let km = mgr.load_manifest(&SCOPE).await.expect("load").kind_manifest;
        assert_eq!(km.live_segments, vec![2, 4]);
        assert_eq!(km.next_seg_id, 9);

        // No ids at all still honours the floor.
        mgr.adopt_segments(&SCOPE, &[], 12)
            .await
            .expect("floor only");
        let km = mgr.load_manifest(&SCOPE).await.expect("load").kind_manifest;
        assert_eq!(km.next_seg_id, 12);
    }

    /// **The counter floor raises, never lowers, and is idempotent** — and the
    /// next rotation takes it, so every id below the floor stays spent.
    #[tokio::test]
    async fn floor_counter_raises_never_lowers_and_the_next_rotation_takes_it() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);
        let env = body(1);
        mgr.append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", "2026-05")
            .await
            .expect("append");
        assert_eq!(mgr.floor_counter(&SCOPE, 7).await.expect("floor"), 7);
        assert_eq!(
            mgr.floor_counter(&SCOPE, 3).await.expect("lower"),
            7,
            "never lowers"
        );
        assert_eq!(
            mgr.floor_counter(&SCOPE, 7).await.expect("again"),
            7,
            "idempotent"
        );
        assert_eq!(
            mgr.load_manifest(&SCOPE)
                .await
                .expect("load")
                .kind_manifest
                .next_seg_id,
            7,
            "saved, not merely cached"
        );

        let env = body(2);
        let out = mgr
            .append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", "2026-06")
            .await
            .expect("append after the floor");
        assert_eq!(out.segment_id, 7, "the next rotation takes the floor");
    }

    /// **A floor the id space cannot honour is refused**, and changes nothing.
    #[tokio::test]
    async fn floor_counter_refuses_a_floor_beyond_the_headroom() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);
        let err = mgr
            .floor_counter(&SCOPE, MAX_COUNTER_FLOOR + 1)
            .await
            .expect_err("beyond the headroom");
        assert!(
            matches!(err, ManagerError::CounterFloorOutOfRange { .. }),
            "{err}"
        );
        assert_eq!(
            mgr.load_manifest(&SCOPE)
                .await
                .expect("load")
                .kind_manifest
                .next_seg_id,
            1
        );
        assert_eq!(
            mgr.floor_counter(&SCOPE, MAX_COUNTER_FLOOR)
                .await
                .expect("at the limit"),
            MAX_COUNTER_FLOOR
        );
    }

    // --- scope rename (identity succession) ---

    const NEW_SCOPE: [u8; 32] = [0x88u8; 32];

    #[tokio::test]
    async fn a_renamed_scope_serves_its_segments_under_the_new_id() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let env = body(1);
        let cid = Cid::of_dag_cbor(&env);
        mgr.append_record_with_bucket(&SCOPE, cid, &env, b"", "2026-05")
            .await
            .expect("append");

        assert_eq!(
            mgr.rename_scope(&SCOPE, &NEW_SCOPE).expect("rename"),
            ScopeRenameOutcome::Moved
        );

        // The new id reads what the old id wrote, from the moved directory —
        // including through any state cached before the rename.
        let manifest = mgr.load_manifest(&NEW_SCOPE).await.expect("load");
        assert_eq!(manifest.kind_manifest.live_segments, vec![1]);
        assert!(!mgr.scope_dir(&SCOPE).exists(), "old dir must be gone");

        // A post-rename append lands under the new id's directory, not a
        // resurrected old one (the cached manifest path is evicted).
        let env2 = body(2);
        mgr.append_record_with_bucket(&NEW_SCOPE, Cid::of_dag_cbor(&env2), &env2, b"", "2026-05")
            .await
            .expect("append post-rename");
        assert!(!mgr.scope_dir(&SCOPE).exists());
    }

    #[tokio::test]
    async fn renaming_a_scope_with_no_directory_moves_nothing() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);
        assert_eq!(
            mgr.rename_scope(&SCOPE, &NEW_SCOPE).expect("rename"),
            ScopeRenameOutcome::NothingToMove
        );
        assert!(!mgr.scope_dir(&NEW_SCOPE).exists());
    }

    #[tokio::test]
    async fn a_rename_onto_an_occupied_scope_parks_and_touches_nothing() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let a = body(1);
        mgr.append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&a), &a, b"", "2026-05")
            .await
            .expect("append old");
        let b = body(2);
        mgr.append_record_with_bucket(&NEW_SCOPE, Cid::of_dag_cbor(&b), &b, b"", "2026-05")
            .await
            .expect("append new");

        assert_eq!(
            mgr.rename_scope(&SCOPE, &NEW_SCOPE).expect("rename"),
            ScopeRenameOutcome::Parked
        );
        // Both survive untouched — a collision is the caller's judgment call,
        // never a silent merge or overwrite.
        assert!(mgr.scope_dir(&SCOPE).exists());
        assert!(mgr.scope_dir(&NEW_SCOPE).exists());
        let old_manifest = mgr.load_manifest(&SCOPE).await.expect("old manifest");
        assert_eq!(old_manifest.kind_manifest.live_segments, vec![1]);
    }

    // --- kind tag flows through ---

    #[tokio::test]
    async fn kind_tag_flows_through_to_manifest() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let env = body(1);
        mgr.append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", "2026-05")
            .await
            .expect("append");

        let manifest = mgr.load_manifest(&SCOPE).await.expect("load_manifest");
        assert_eq!(manifest.kind, "test", "kind tag must be 'test'");
        assert_eq!(manifest.kind_manifest.live_segments, vec![1]);
    }

    #[tokio::test]
    async fn kind_tag_in_manifest_file_name() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let env = body(1);
        mgr.append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", "2026-05")
            .await
            .expect("append");

        let manifest_path = mgr.manifest_path(&SCOPE);
        assert!(
            manifest_path.to_string_lossy().ends_with("manifest.test"),
            "manifest file name must include kind; got {}",
            manifest_path.display()
        );
        assert!(manifest_path.exists(), "manifest must be on disk");
    }

    // --- append updates the manifest ---

    #[tokio::test]
    async fn append_updates_manifest_live_segments() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let env = body(1);
        let out = mgr
            .append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", "2026-05")
            .await
            .expect("append");
        assert_eq!(out.segment_id, 1);
        assert!(
            out.finalized.is_none(),
            "first append doesn't finalize anything"
        );

        let manifest = mgr.load_manifest(&SCOPE).await.expect("load");
        assert_eq!(manifest.kind_manifest.live_segments, vec![1]);
        assert_eq!(manifest.kind_manifest.next_seg_id, 2);
    }

    /// Sizing (the quota/SEARCH path) must answer for records in the OPEN
    /// segment from its in-memory length map WITHOUT finalizing it — a
    /// flush here rotated the open segment on every quota-checked inbound
    /// delivery (one segment per message) and ate the rotation's
    /// `Finalized` outcome, losing the `fauna.segments.changed` push.
    #[tokio::test]
    async fn sizing_the_open_segment_does_not_finalize_it() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let env_a = body(1);
        let cid_a = Cid::of_dag_cbor(&env_a);
        mgr.append_record_with_bucket(&SCOPE, cid_a, &env_a, b"", "2026-05")
            .await
            .expect("a");

        // Size the open segment: served from memory, absent cid → None.
        let absent = Cid::of_dag_cbor(b"absent");
        let sizes = mgr
            .record_sizes_for_segment(&SCOPE, 1, &[&cid_a, &absent])
            .await
            .expect("sizes");
        assert_eq!(sizes, vec![Some(env_a.len() as u64), None]);
        let single = mgr.record_size(&SCOPE, 1, &cid_a).await.expect("size");
        assert_eq!(single, env_a.len() as u64);

        // The open segment survived the sizing: a same-bucket append still
        // lands in seg 1 with no rotation...
        let env_b = body(2);
        let out_b = mgr
            .append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env_b), &env_b, b"", "2026-05")
            .await
            .expect("b");
        assert_eq!(
            out_b.segment_id, 1,
            "sizing must not close the open segment"
        );
        assert_eq!(out_b.finalized, None);

        // ...and the bucket-change rotation still reports the closed id
        // (the `Finalized` push signal the custodian pull wakes on).
        let env_c = body(3);
        let out_c = mgr
            .append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env_c), &env_c, b"", "2026-06")
            .await
            .expect("c");
        assert_eq!(out_c.segment_id, 2);
        assert_eq!(
            out_c.finalized,
            Some(1),
            "rotation announces the closed segment"
        );

        // Sizing a FINALIZED segment still works, off the on-disk index.
        let sizes = mgr
            .record_sizes_for_segment(&SCOPE, 1, &[&cid_a])
            .await
            .expect("sizes finalized");
        assert_eq!(sizes, vec![Some(env_a.len() as u64)]);
    }

    #[tokio::test]
    async fn append_same_bucket_no_rotation() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let env_a = body(1);
        let env_b = body(2);
        let out_a = mgr
            .append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env_a), &env_a, b"", "2026-05")
            .await
            .expect("a");
        let out_b = mgr
            .append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env_b), &env_b, b"", "2026-05")
            .await
            .expect("b");

        assert_eq!(out_a.segment_id, 1);
        assert_eq!(out_b.segment_id, 1, "same bucket → same segment");
        assert!(
            out_b.finalized.is_none(),
            "no rotation on same-bucket append"
        );

        let manifest = mgr.load_manifest(&SCOPE).await.expect("load");
        assert_eq!(manifest.kind_manifest.live_segments, vec![1]);
    }

    #[tokio::test]
    async fn append_bucket_change_rotates() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let env_a = body(1);
        let env_b = body(2);
        let out_a = mgr
            .append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env_a), &env_a, b"", "2026-05")
            .await
            .expect("a");
        let out_b = mgr
            .append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env_b), &env_b, b"", "2026-06")
            .await
            .expect("b");

        assert_eq!(out_a.segment_id, 1);
        assert_eq!(out_b.segment_id, 2, "new bucket → next segment");
        assert_eq!(
            out_b.finalized,
            Some(1),
            "rotation closed seg 1 → finalized = Some(1)"
        );

        let manifest = mgr.load_manifest(&SCOPE).await.expect("load");
        assert_eq!(manifest.kind_manifest.live_segments, vec![1, 2]);
    }

    // --- read returns bytes verbatim ---

    #[tokio::test]
    async fn read_envelope_bytes_returns_verbatim() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let envelope = b"opaque-envelope-xyz";
        let cid = Cid::of_dag_cbor(envelope);
        let out = mgr
            .append_record_with_bucket(&SCOPE, cid, envelope, b"some-floor-blob", "2026-05")
            .await
            .expect("append");

        let bytes = mgr
            .read_envelope_bytes(&SCOPE, out.segment_id, &cid)
            .await
            .expect("read");
        assert_eq!(bytes, envelope, "read must return the exact bytes appended");
    }

    #[tokio::test]
    async fn read_record_with_floor_bytes_returns_both() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let envelope = b"my-envelope";
        let floor = b"my-floor-blob";
        let cid = Cid::of_dag_cbor(envelope);
        let out = mgr
            .append_record_with_bucket(&SCOPE, cid, envelope, floor, "2026-05")
            .await
            .expect("append");

        let (env_bytes, floor_bytes) = mgr
            .read_record_with_floor_bytes(&SCOPE, out.segment_id, &cid)
            .await
            .expect("read");
        assert_eq!(env_bytes, envelope);
        assert_eq!(floor_bytes, floor);
    }

    #[tokio::test]
    async fn record_size_returns_envelope_length() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let envelope = b"opaque-envelope-of-known-length";
        let cid = Cid::of_dag_cbor(envelope);
        let out = mgr
            .append_record_with_bucket(&SCOPE, cid, envelope, b"floor", "2026-05")
            .await
            .expect("append");

        // Size must equal the appended envelope length (= what byte_length
        // mirrors today) and be derived without reading the body.
        let size = mgr
            .record_size(&SCOPE, out.segment_id, &cid)
            .await
            .expect("size");
        assert_eq!(size, envelope.len() as u64);

        // Unknown cid in a real segment → RecordNotFound.
        let missing = Cid::of_dag_cbor(b"never-appended");
        let err = mgr
            .record_size(&SCOPE, out.segment_id, &missing)
            .await
            .expect_err("missing must error");
        assert!(matches!(err, ManagerError::RecordNotFound { .. }));
    }

    #[tokio::test]
    async fn record_sizes_for_segment_sums_match_payload_lengths() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let mut cids = Vec::new();
        let mut lens = Vec::new();
        for i in 1u8..=4 {
            // Vary length per record so an order/identity bug is visible.
            let env = "x".repeat(i as usize * 3);
            let cid = Cid::of_dag_cbor(env.as_bytes());
            cids.push(cid);
            lens.push(env.len() as u64);
            mgr.append_record_with_bucket(&SCOPE, cid, env.as_bytes(), b"", "2026-05")
                .await
                .expect("append");
        }
        mgr.flush(&SCOPE).await.expect("flush");

        let refs: Vec<&Cid> = cids.iter().collect();
        let sizes = mgr
            .record_sizes_for_segment(&SCOPE, 1, &refs)
            .await
            .expect("bulk sizes");
        assert_eq!(sizes, lens.iter().copied().map(Some).collect::<Vec<_>>());
        // The quota SUM is the sum of these.
        let total: u64 = sizes.into_iter().flatten().sum();
        assert_eq!(total, lens.iter().sum::<u64>());
    }

    #[tokio::test]
    async fn read_envelopes_bulk_returns_all_records() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let mut cids = Vec::new();
        for i in 1u8..=3 {
            let env = format!("envelope-{i}");
            let cid = Cid::of_dag_cbor(env.as_bytes());
            cids.push(cid);
            mgr.append_record_with_bucket(&SCOPE, cid, env.as_bytes(), b"", "2026-05")
                .await
                .expect("append");
        }

        // Finalize so the segment can be opened for reading.
        mgr.flush(&SCOPE).await.expect("flush");

        let records = mgr.read_envelopes_bulk(&SCOPE, 1).await.expect("bulk");
        assert_eq!(records.len(), 3);
        // Verify each record matches the appended envelope.
        for (rid, env, _floor) in &records {
            // The cid must match the original envelope.
            assert!(cids.contains(rid));
            // Cid::of_dag_cbor the envelope round-trips to the same cid.
            assert_eq!(Cid::of_dag_cbor(env), *rid);
        }
    }

    // --- finalize_open closes the open segment without rotating ---

    #[tokio::test]
    async fn finalize_open_closes_without_rotating() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        // Nothing open yet → None.
        assert!(
            mgr.finalize_open(&SCOPE).await.expect("no-op").is_none(),
            "finalize with nothing open must return None"
        );

        // Append one record.
        let env = body(1);
        mgr.append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", "2026-05")
            .await
            .expect("append");

        // finalize_open must return Some(1) and close the segment.
        let closed = mgr.finalize_open(&SCOPE).await.expect("finalize");
        assert_eq!(closed, Some(1), "finalize_open must return Some(1)");

        // Idempotent: second call returns None.
        assert!(
            mgr.finalize_open(&SCOPE)
                .await
                .expect("idempotent")
                .is_none(),
            "second finalize must be a no-op"
        );

        // The manifest still shows seg 1 as live (we didn't compact).
        let manifest = mgr.load_manifest(&SCOPE).await.expect("load");
        assert_eq!(manifest.kind_manifest.live_segments, vec![1]);
    }

    // --- segment_file_path includes kind in the path ---

    #[tokio::test]
    async fn segment_file_path_includes_kind() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let path = mgr.segment_file_path(&SCOPE, 1);
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("__test"),
            "path must include kind; got {path_str}"
        );
        assert!(
            path_str.ends_with("seg-00000001.dat"),
            "segment file must use the standard naming; got {path_str}"
        );
    }

    #[tokio::test]
    async fn segment_file_path_matches_on_disk_layout() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let env = body(1);
        mgr.append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", "2026-05")
            .await
            .expect("append");
        mgr.finalize_open(&SCOPE).await.expect("finalize");

        let path = mgr.segment_file_path(&SCOPE, 1);
        assert!(
            path.exists(),
            "segment_file_path must locate the actual on-disk file; path = {}",
            path.display()
        );
    }

    // --- read-after-write in same bucket doesn't need explicit flush ---

    #[tokio::test]
    async fn read_after_write_same_bucket_no_explicit_flush() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let envelope = b"hot-envelope";
        let cid = Cid::of_dag_cbor(envelope);
        mgr.append_record_with_bucket(&SCOPE, cid, envelope, b"", "2026-05")
            .await
            .expect("append");

        // No explicit flush — read_envelope_bytes applies flush-on-read.
        let bytes = mgr
            .read_envelope_bytes(&SCOPE, 1, &cid)
            .await
            .expect("read");
        assert_eq!(bytes, envelope);
    }

    // --- flush/finalize_open alias ---

    #[tokio::test]
    async fn flush_returns_closed_segment_id() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        // Nothing open → None.
        assert!(mgr.flush(&SCOPE).await.expect("flush none").is_none());

        let env = body(1);
        mgr.append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", "2026-05")
            .await
            .expect("append");

        assert_eq!(mgr.flush(&SCOPE).await.expect("flush"), Some(1));
        // Idempotent.
        assert!(mgr.flush(&SCOPE).await.expect("flush again").is_none());
    }

    // --- describe_segment_for_backup ---

    #[tokio::test]
    async fn describe_segment_for_backup_round_trip() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        for i in 1u8..=3 {
            let env = body(i);
            mgr.append_record_with_bucket(&SCOPE, Cid::of_dag_cbor(&env), &env, b"", "2026-05")
                .await
                .expect("append");
        }
        mgr.finalize_open(&SCOPE).await.expect("finalize");

        let meta = mgr
            .describe_segment_for_backup(&SCOPE, 1)
            .await
            .expect("describe");
        assert_eq!(meta.segment_id, 1);
        assert_eq!(meta.bucket, "2026-05");
        assert!(meta.byte_size > 0);
        // file_blake3 matches the on-disk file's hash.
        let path = mgr.segment_file_path(&SCOPE, 1);
        let bytes = std::fs::read(&path).expect("read seg");
        let expected = blake3::hash(&bytes);
        assert_eq!(meta.file_blake3, *expected.as_bytes());
    }

    // --- concurrent same-scope appends serialize correctly ---

    #[tokio::test]
    async fn concurrent_appends_on_same_scope_serialize() {
        // 20 concurrent appends on the same scope must all commit without
        // racing on next_seg_id. The correctness claim: no records are lost.
        let tmp = TempDir::new().expect("tmp");
        let mgr = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "test"));
        let scope = [0x42u8; 32];

        let mut handles = Vec::new();
        let mut all_cids = Vec::new();
        for i in 0u8..20 {
            let env = vec![i; 8];
            let cid = Cid::of_dag_cbor(&env);
            all_cids.push(cid);
            let mgr = mgr.clone();
            handles.push(tokio::spawn(async move {
                mgr.append_record_with_bucket(&scope, cid, &env, b"", "2026-05")
                    .await
                    .unwrap()
            }));
        }

        let mut outcomes = Vec::new();
        for h in handles {
            outcomes.push(h.await.expect("task panicked"));
        }

        // All 20 appends should have committed (one segment, same bucket).
        assert_eq!(outcomes.len(), 20, "all 20 tasks completed");

        // Flush so the open segment is readable.
        mgr.flush(&scope).await.expect("flush");

        // All 20 records must be retrievable — confirms no loss under contention.
        for (i, cid) in all_cids.iter().enumerate() {
            // segment_id is always 1: all appends land in the same bucket,
            // so FramedSegmentStore never rotates.
            let bytes = mgr
                .read_envelope_bytes(&scope, 1, cid)
                .await
                .unwrap_or_else(|_| panic!("record {i} missing after concurrent appends"));
            let expected = vec![i as u8; 8];
            assert_eq!(bytes, expected, "record {i} payload corrupted");
        }
    }

    // --- bucket_for (lifted from fauna-mail; Plan 7) ---

    #[test]
    fn bucket_for_known_dates() {
        // 2026-05-14 12:00 UTC.
        assert_eq!(super::bucket_for(1_778_760_000), "2026-05");
        // 2025-12-31 23:59 UTC.
        assert_eq!(super::bucket_for(1_767_225_540), "2025-12");
        // Epoch + negative clamp.
        assert_eq!(super::bucket_for(0), "1970-01");
        assert_eq!(super::bucket_for(-1), "1970-01");
        // 2020-03-01 (leap year boundary).
        assert_eq!(super::bucket_for(1_583_020_800), "2020-03");
    }

    // --- data_dir and kind accessors ---

    #[test]
    fn accessors_return_constructor_values() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = SegmentManager::new(tmp.path().to_path_buf(), "mail");
        assert_eq!(mgr.kind(), "mail");
        assert_eq!(mgr.data_dir(), tmp.path());
    }

    // --- multiple scopes are independent ---

    #[tokio::test]
    async fn multiple_scopes_are_independent() {
        let tmp = TempDir::new().expect("tmp");
        let mgr = manager_in(&tmp);

        let scope_a = [0xaau8; 32];
        let scope_b = [0xbbu8; 32];

        let env_a = b"envelope-a";
        let env_b = b"envelope-b";
        let cid_a = Cid::of_dag_cbor(env_a);
        let cid_b = Cid::of_dag_cbor(env_b);
        mgr.append_record_with_bucket(&scope_a, cid_a, env_a, b"", "2026-05")
            .await
            .expect("append a");
        mgr.append_record_with_bucket(&scope_b, cid_b, env_b, b"", "2026-06")
            .await
            .expect("append b");

        let manifest_a = mgr.load_manifest(&scope_a).await.expect("manifest a");
        let manifest_b = mgr.load_manifest(&scope_b).await.expect("manifest b");

        // Each scope gets its own segment counter starting at 1.
        assert_eq!(manifest_a.kind_manifest.live_segments, vec![1]);
        assert_eq!(manifest_b.kind_manifest.live_segments, vec![1]);

        // The envelope bytes are independent.
        mgr.flush(&scope_a).await.expect("flush a");
        mgr.flush(&scope_b).await.expect("flush b");

        let env_a_out = mgr
            .read_envelope_bytes(&scope_a, 1, &cid_a)
            .await
            .expect("read a");
        let env_b_out = mgr
            .read_envelope_bytes(&scope_b, 1, &cid_b)
            .await
            .expect("read b");
        assert_eq!(env_a_out, env_a);
        assert_eq!(env_b_out, env_b);
    }
}
