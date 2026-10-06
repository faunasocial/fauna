//! Nest-side glue for `libs/fauna-segment-store` + `libs/fauna-mail`'s
//! `nest-segments` feature module.
//!
//! `records_db` owns the SQLite `segment_records` mirror DAO.
//! `mail` owns the mail-kind coordination free functions (append / read /
//! compact) that bridge the kind-agnostic `SegmentManager` and the
//! `segment_records` mirror, which the MTA + MDA paths call.
//! `compaction` runs the per-actor-per-kind pinning compaction sweep
//! (6h scheduled; also reachable on demand from `compact_handler`).
//! `compact_handler` is the `fauna.segments.compact` WS-RPC handler
//! (manual compaction trigger); `list_handler` is the
//! `fauna.segments.list` WS-RPC handler.
//! `backup_source` is the nest-local `SegmentSource` the in-process
//! segment-backup coordinator reads this nest's own segment files through
//! (`docs/goal/architecture/message-segment-store.md`
//! § Cross-location backup protocol).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use fauna_cbor::Cid;
use fauna_mail::segments::bucket_for;
use fauna_segment_store::{
    CompactionPlan, FramedSegment, FramedSegmentStore, KindManifest, ManagerError, SegmentManager,
    SegmentStoreError, VersionedManifest,
};
use tokio::sync::Mutex;

pub mod backup_source;
pub mod cal;
pub mod cal_placement;
pub mod card;
pub mod card_placement;
pub mod compact_handler;
pub mod compaction;
pub mod conv;
pub mod list_handler;
pub mod mail;
pub mod mail_placement;
pub mod post;
pub mod records_db;
pub mod segment_route;

pub use cal::CalAppendOutcome;
pub use cal_placement::CalPlacementSegmentManager;
pub use card::CardAppendOutcome;
pub use card_placement::CardPlacementSegmentManager;
pub use compaction::{CompactScope, CompactionReport, CompactionTrigger, CompactionWorker};
pub use conv::ConvAppendOutcome;
pub use mail::{AppendOutcome, BucketCompactionOutcome, SegmentBackupMeta};
pub use mail_placement::MailPlacementSegmentManager;
pub use post::PostAppendOutcome;
pub use records_db::{SegmentRecordRef, SegmentRecordRow};

/// `seg-NNNNNNNN.dat` files under `root`, sorted ascending by id.
/// Missing/empty directory → empty vec (fresh actor).
///
/// Kind-agnostic: the three placement journals (mail, cal, card) all replay
/// through it when rebuilding a corrupt manifest, so it is homed here rather
/// than in whichever kind happened to need it first.
pub(crate) fn list_segment_files_for_replay(root: &Path) -> Result<Vec<(u32, PathBuf)>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).context("read segment dir"),
    };
    for entry in entries {
        let entry = entry.context("read segment dir entry")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name
            .strip_prefix("seg-")
            .and_then(|s| s.strip_suffix(".dat"))
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        out.push((id, entry.path()));
    }
    out.sort_by_key(|(id, _)| *id);
    Ok(out)
}

/// Open one finalized journal segment during a placement-manifest rebuild,
/// deciding what a refusal to open *means*.
///
/// `Ok(None)` — a skip — is returned for exactly one case, the one the
/// best-effort rebuild was reasoned for: the **crash tail**. A crash between
/// the last append and its `finalize()` leaves a `.dat` with no `.meta`
/// sidecar, and those records cannot be replayed. Segment ids are monotonic
/// and [`list_segment_files_for_replay`] sorts ascending, so that segment is
/// always the *highest id present* — `is_highest_id` is the whole of the case.
///
/// Every other refusal is corruption of a segment the manifest is supposed to
/// describe, and it **fails closed**, matching the `?` the record-level decode
/// beside each call site already uses. `FramedSegment::open` has eight failure
/// exits and only the missing sidecar is the crash tail; the others (a damaged
/// `.meta`, a damaged `.dat`, an I/O error on either, a drifted CARv2 block
/// count, a corrupt `floor_metadata` length) are all reachable on a *middle*
/// segment. Skipping one drops its records from the rebuilt manifest, which is
/// then `save_atomic`'d and decodes cleanly forever after — so nothing ever
/// rebuilds again. For mail that is delivered mail going unreachable over IMAP
/// and expunged mail coming back, silently and terminally, with a
/// `tracing::warn!` as its only witness and no operator to read it
/// (`docs/goal/principles.md` § No user-data loss, § One configuration
/// surface). A loud refusal is repairable; a silent drop is not.
///
/// [`SegmentStoreError::SchemaMismatch`] fails closed at **every** position,
/// the tail included. It is the same variant each manager's `actor_state`
/// turns into a hard refusal on the *manifest* immediately before falling into
/// this rebuild, and for the same reason: a newer binary wrote the file.
/// Rebuilding past it performs precisely the downgrade-strips-fields the
/// manifest arm exists to refuse.
///
/// Ruled 2026-08-30; owner `docs/goal/architecture/message-segment-store.md`
/// § Implementation status today.
pub(crate) fn open_replay_segment(
    kind: &str,
    seg_id: u32,
    path: &Path,
    is_highest_id: bool,
) -> Result<Option<FramedSegment>> {
    match FramedSegment::open(path) {
        Ok(seg) => Ok(Some(seg)),
        // Never swallowed, at any position — see the doc comment.
        Err(e @ SegmentStoreError::SchemaMismatch(_)) => Err(anyhow::anyhow!(
            "{kind} segment {seg_id} at {} was written by a newer binary; refusing to rebuild \
             the manifest without its records — this nest binary must be updated: {e}",
            path.display()
        )),
        Err(e) if is_highest_id && e.is_unfinalized_crash_tail() => {
            tracing::warn!(
                segment = %path.display(),
                error = %e,
                kind,
                "skipping the unreadable NEWEST placement segment during manifest rebuild \
                 (crashed before finalize); only records appended after the last finalize \
                 are lost"
            );
            Ok(None)
        }
        Err(e) if is_highest_id => {
            tracing::warn!(
                segment = %path.display(),
                error = %e,
                kind,
                "skipping the unreadable NEWEST placement segment during manifest rebuild; \
                 this is NOT confirmed to be an unfinalized crash tail, so records appended \
                 before the last finalize may also be lost"
            );
            Ok(None)
        }
        Err(e) => Err(anyhow::anyhow!(
            "{kind} segment {seg_id} at {} cannot be opened and is not the newest segment, so \
             it is not an unfinalized crash tail; refusing to rebuild the manifest without its \
             records: {e}",
            path.display()
        )),
    }
}

// ── The placement journals' kind-agnostic half ─────────────────────────────
//
// `{mail,cal,card}_placement.rs` are a three-way structural parallel, not a
// two-way one: the same nine manager methods in the same order and the same
// journal replay. What follows is the part of that parallel which is
// genuinely kind-*agnostic*, homed once here for the reason
// `list_segment_files_for_replay` above already states — rather than in
// whichever kind happened to need it first. What legitimately differs per
// kind stays per kind, and
// `message-segment-store.md` § *Landed substrate* → Plan 6 records which.

/// Wall-clock seconds — the unit every placement manifest's `deleted_at` and
/// every placement bucket key is in. (The record planes' floors are split:
/// mail/conv/post carry epoch *milliseconds*, calendar/card seconds. The
/// placement plane has no such split — see `compact_bucket_with` for where
/// the units trap actually lives.)
pub(crate) fn placement_now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

/// Segment-local record CID: `seq` (u64 LE) prefixed to the caller's record
/// bytes, hashed once with BLAKE3-256 and wrapped in the canonical Cid prefix.
///
/// `seq` is the per-actor monotonic record counter, and mixing it into the
/// hash input is what guarantees distinct CIDs when the record bytes
/// legitimately repeat — a double SUBSCRIBE, a retried ProvisionCalendar, a
/// retried DeleteCard — which `FramedSegment::append_record` rejects as a
/// duplicate. The pre-CARv2 16-byte `seq || hash8` shape is gone (CARv2
/// indexes by 36-byte Cid only); the retry-disambiguation property is not.
pub(crate) fn compute_placement_cid(seq: u64, record_bytes: &[u8]) -> Cid {
    let mut buf = Vec::with_capacity(8 + record_bytes.len());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(record_bytes);
    Cid::of_dag_cbor(&buf)
}

/// Rebuild a placement manifest by replaying every finalized journal segment
/// under `root` in order (ascending segment id = append order; the per-actor
/// lock serializes appends and buckets rotate monotonically). The manifest is
/// a cache; the journal is the durable source of truth (restore-design spec
/// § D3), so this is what an undecodable manifest falls back to rather than
/// bricking the actor's writes forever.
///
/// **The fail-closed rules are [`open_replay_segment`]'s, and are not
/// parameterized here** — deliberately. All three kinds passed it the
/// identical arguments when they each wrote this loop out, so hoisting the
/// loop cannot loosen any kind's refusal: there is one policy, applied once.
/// Best-effort about exactly ONE thing, a crashed never-finalized tail
/// segment; a middle segment that will not open is corruption, and skipping
/// it would silently drop the records it placed and resurrect the ones it
/// deleted.
///
/// Two things genuinely differ per kind, and both are the caller's:
///
/// * `kind_manifest` — reach the `KindManifest` inside that kind's typed
///   manifest. A one-line field borrow at each call site; the three placement
///   manifests are foreign types with no trait in common that exposes it.
/// * `apply_record` — decode one record frame with that kind's strict
///   `*PlacementRecord::decode` and reduce it into the manifest. This is the
///   one decision a new kind actually has to make.
///
/// The journal label is **not** a parameter: it is `VersionedManifest::LABEL`,
/// the same on-disk-visible constant the manifest's own atomic save uses, so
/// the replay path and the save path can no longer drift apart the way three
/// hand-written string literals could.
pub(crate) fn replay_placement_journal<M: VersionedManifest>(
    root: &Path,
    mut manifest: M,
    kind_manifest: impl Fn(&mut M) -> &mut KindManifest,
    mut apply_record: impl FnMut(&[u8], &mut M) -> Result<()>,
) -> Result<M> {
    let files = list_segment_files_for_replay(root)?;
    // The crash tail is the highest id present — the ONLY unopenable segment
    // this replay may skip (`open_replay_segment`).
    let highest_id = files.last().map(|(id, _)| *id);
    for (seg_id, path) in files {
        let Some(seg) = open_replay_segment(M::LABEL, seg_id, &path, Some(seg_id) == highest_id)?
        else {
            continue;
        };
        for entry in seg.iter_records() {
            let bytes = seg
                .read_record(&entry.cid)?
                .ok_or_else(|| anyhow::anyhow!("segment {seg_id} index lists a missing record"))?;
            apply_record(&bytes, &mut manifest)
                .with_context(|| format!("decode {} record in seg {seg_id}", M::LABEL))?;
        }
        let km = kind_manifest(&mut manifest);
        km.live_segments.push(seg_id);
        km.next_seg_id = km.next_seg_id.max(seg_id + 1);
    }
    Ok(manifest)
}

/// What a placement journal needs to know about its kind.
///
/// `{mail,cal,card}_placement.rs` held three hand-copies of one manager — the
/// same nine methods in the same order, the same per-actor `DashMap` +
/// `Mutex`, the same fail-closed manifest load, differing only in names. This
/// trait is the whole of what actually differs; [`PlacementJournal`] below is
/// the manager, written once.
///
/// The kind's *record semantics* are deliberately NOT here. Reducing a decoded
/// record into the manifest — `apply_versioned_record_to_manifest` and the
/// `apply_put_*` / `apply_delete_*` / `update_modseq` family it drives — is
/// what genuinely differs between a mailbox, a calendar and an addressbook
/// (5, 5 and 10 record variants over different manifest state, keyed by
/// `&str` for mail and `[u8; 32]` for the two DAV kinds). It stays in the
/// per-kind module, where it can be read against that kind's record enum.
pub trait PlacementKind: Send + Sync + 'static {
    /// The kind's typed placement manifest.
    type Manifest: VersionedManifest + Default + Clone + Send + 'static;
    /// The kind's journal record, as its callers hand it to `append_event`.
    type Record;
    /// One entry of `Manifest`'s tombstone vec.
    type Tombstone;

    /// Short kind name for recovery logs and succession park reports
    /// (`ActorScopedStore::kind`), matching the on-disk directory
    /// `<data_dir>/segments/__<SCOPE_KIND>/` and the reserved-folder kind
    /// `db::snapshots::is_high_cadence_reserved_kind` tests for.
    ///
    /// ⚠ **Not the same string as `Manifest::LABEL`, and not derivable from
    /// it.** Calendar's is `calendar-placement` where its label is
    /// `cal-placement`. Both spellings are on-disk-visible and neither may be
    /// "unified" into the other: the label appears in the manifest's
    /// atomic-save tmp extension and its own filename, this one in the
    /// directory name — so collapsing them would rename a live journal
    /// directory out from under every existing actor.
    const SCOPE_KIND: &'static str;

    /// `<data_dir>/segments/__<SCOPE_KIND>/<actor_hex>/`.
    fn segments_root(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf;
    /// That root's `manifest.<Manifest::LABEL>`.
    fn manifest_path(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf;
    /// The record's own canonical dag-cbor encode.
    fn encode_record(record: &Self::Record) -> Result<Vec<u8>, SegmentStoreError>;
    /// The kind's named `rebuild_*_manifest_from_segments` wrapper — the
    /// journal replay an undecodable manifest falls back to.
    fn rebuild_from_segments(root: &Path) -> Result<Self::Manifest>;
    /// The `KindManifest` inside the kind's typed manifest.
    fn kind_manifest_mut(m: &mut Self::Manifest) -> &mut KindManifest;
    /// The kind's tombstone vec.
    fn tombstones_mut(m: &mut Self::Manifest) -> &mut Vec<Self::Tombstone>;
    /// Reduce one appended record into the compacted manifest state — the
    /// kind's `apply_record_to_manifest`, and the one item here that is not
    /// mechanical.
    fn apply_record(record: &Self::Record, m: &mut Self::Manifest);
    /// A tombstone's delete time; `None` for a tombstone the kind records
    /// no delete time for (a mail `Move` tombstone), which is never pruned.
    fn tombstone_deleted_at(t: &Self::Tombstone) -> Option<i64>;
}

/// Per-actor in-memory state. Loaded from disk on first access.
struct PlacementActorState<K: PlacementKind> {
    store: FramedSegmentStore,
    manifest: K::Manifest,
    /// Path to `manifest.<LABEL>` for atomic save.
    manifest_path: PathBuf,
    /// Monotonic per-actor record counter. Mixed into the record CID so that
    /// legitimately-identical record bytes (a double SUBSCRIBE, a retried
    /// ProvisionCalendar, a retried DeleteCard) don't collide on the
    /// segment's record index (`FramedSegment::append_record` rejects
    /// duplicates). Resets on process restart, but `FramedSegmentStore::new`
    /// always opens a fresh segment for the first append after restart, so a
    /// reset counter never aliases an existing on-segment record id.
    next_record_seq: u64,
}

/// One actor-scoped placement journal — the manager all three placement kinds
/// are, differing only by [`PlacementKind`].
///
/// Each kind keeps a named alias (`MailPlacementSegmentManager`,
/// `CalPlacementSegmentManager`, `CardPlacementSegmentManager`) so call sites
/// still say which journal they mean; the aliases exist to name the kind, not
/// to duplicate the manager — the convention `records_db` set one layer down.
pub struct PlacementJournal<K: PlacementKind> {
    data_dir: PathBuf,
    actors: DashMap<[u8; 32], Arc<Mutex<PlacementActorState<K>>>>,
}

impl<K: PlacementKind> PlacementJournal<K> {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            actors: DashMap::new(),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Get or create the per-actor lock + state, created lazily on first
    /// access.
    ///
    /// The manifest load is the fail-closed decision this manager exists to
    /// make exactly once, in one place, for all three kinds — it was written
    /// out three times before. A **future** format version refuses loudly (a
    /// downgraded binary silently rebuilding-as-current would strip the fields
    /// it cannot decode); merely **undecodable** bytes fall back to replaying
    /// the journal, because the manifest is a cache and the journal is the
    /// durable source of truth (restore-design spec § D3) — bricking the
    /// actor's writes forever is the worse answer. The replay's own
    /// fail-closed rules are [`open_replay_segment`]'s.
    async fn actor_state(&self, actor_id: &[u8; 32]) -> Result<Arc<Mutex<PlacementActorState<K>>>> {
        if let Some(existing) = self.actors.get(actor_id) {
            return Ok(existing.clone());
        }
        let label = K::Manifest::LABEL;
        let root = K::segments_root(&self.data_dir, actor_id);
        let manifest_path = K::manifest_path(&self.data_dir, actor_id);
        let manifest = match K::Manifest::load(&manifest_path) {
            Ok(m) => m.unwrap_or_default(),
            Err(e @ SegmentStoreError::SchemaMismatch(_)) => {
                return Err(e).with_context(|| {
                    format!("load {label} manifest at {}", manifest_path.display())
                });
            }
            Err(e) => {
                tracing::warn!(
                    manifest = %manifest_path.display(),
                    error = %e,
                    kind = label,
                    "placement manifest undecodable; rebuilding from journal segments"
                );
                let rebuilt = K::rebuild_from_segments(&root)
                    .with_context(|| format!("rebuild {label} manifest from segments"))?;
                rebuilt
                    .save_atomic(&manifest_path)
                    .map_err(|e| anyhow::anyhow!("persist rebuilt {label} manifest: {e}"))?;
                rebuilt
            }
        };
        let store = FramedSegmentStore::new(root, label, *actor_id)
            .with_context(|| format!("construct FramedSegmentStore for {label}"))?;
        let state = Arc::new(Mutex::new(PlacementActorState::<K> {
            store,
            manifest,
            manifest_path,
            next_record_seq: 0,
        }));
        // Race-safe insert: if another caller raced us, return theirs.
        let entry = self
            .actors
            .entry(*actor_id)
            .or_insert_with(|| state.clone());
        Ok(entry.clone())
    }

    /// Append one placement record. Atomically:
    ///   1. encode + append to the segment store (with empty floor metadata)
    ///   2. update compacted current state in the manifest
    ///   3. `save_atomic` the manifest
    ///
    /// All three under the per-actor mutex. Returns the segment id the record
    /// landed in. (Pre-CARv2 callers also received a byte offset; the CARv2
    /// segment-store API addresses records by CID, not `(offset, length)`, so
    /// the offset is gone — no production caller consumed it.)
    pub async fn append_event(&self, actor_id: &[u8; 32], record: &K::Record) -> Result<u32> {
        let label = K::Manifest::LABEL;
        let record_bytes =
            K::encode_record(record).map_err(|e| anyhow::anyhow!("encode {label} record: {e}"))?;
        let bucket = bucket_for(placement_now_secs());

        let actor_state = self.actor_state(actor_id).await?;
        let mut state = actor_state.lock().await;

        let seq = state.next_record_seq;
        state.next_record_seq = state.next_record_seq.wrapping_add(1);
        let cid = compute_placement_cid(seq, &record_bytes);

        let next_candidate = K::kind_manifest_mut(&mut state.manifest).next_seg_id;
        let seg_id = state
            .store
            .append(&bucket, next_candidate, cid, &record_bytes, &[])
            .with_context(|| format!("append {label} event to FramedSegmentStore"))?;

        if seg_id == next_candidate {
            let assigned = K::kind_manifest_mut(&mut state.manifest).append_segment();
            debug_assert_eq!(
                assigned, seg_id,
                "segment id mismatch between {label} store and manifest"
            );
        }

        K::apply_record(record, &mut state.manifest);

        state
            .manifest
            .save_atomic(&state.manifest_path)
            .map_err(|e| anyhow::anyhow!("save {label} manifest: {e}"))?;

        Ok(seg_id)
    }

    /// Snapshot of the actor's in-memory manifest. Deliberately NOT a re-read
    /// from disk — callers (DR restore, divergence detection) need the
    /// freshest state, which is the in-memory one (the manifest is saved after
    /// every append).
    pub async fn current_manifest(&self, actor_id: &[u8; 32]) -> Result<K::Manifest> {
        let actor_state = self.actor_state(actor_id).await?;
        let state = actor_state.lock().await;
        Ok(state.manifest.clone())
    }

    /// Read-only snapshot of the actor's manifest from the on-disk file,
    /// without acquiring the per-actor mutex. Returns the manifest's
    /// `Default` when no file exists yet (cold actor). Snapshot-pin and
    /// divergence-check paths use this to avoid contending with writers.
    pub async fn load_manifest(&self, actor_id: &[u8; 32]) -> Result<K::Manifest> {
        let path = K::manifest_path(&self.data_dir, actor_id);
        Ok(K::Manifest::load(&path)
            .with_context(|| format!("load {} manifest at {}", K::Manifest::LABEL, path.display()))?
            .unwrap_or_default())
    }

    /// Finalize the actor's currently-open placement segment. Idempotent.
    /// Required before a backup destination pull, before snapshot-pin manifest
    /// serialization, or anywhere a reader opens the segment by id (segment
    /// file footers are written only on finalize).
    pub async fn finalize_open(&self, actor_id: &[u8; 32]) -> Result<()> {
        let actor_state = self.actor_state(actor_id).await?;
        let mut state = actor_state.lock().await;
        state
            .store
            .finalize_open()
            .with_context(|| format!("finalize open {} segment", K::Manifest::LABEL))?;
        Ok(())
    }

    // ── The backup read surface ───────────────────────────────────────────
    //
    // A backed-up message kind's corpus is its content segments AND its
    // placement journal (`backup-destinations.md` § *Where restored mail
    // lands*), so the journal needs the three reads the content store's
    // `SegmentManager` already gives the backup arms. They mirror that
    // manager's own (`describe_segment_for_backup`, `read_segment_pair`) one
    // for one, finalize-on-read and under the per-actor lock, so a journal
    // pair is observed exactly as a content pair is: the two files together,
    // with no append able to rotate between them.

    /// The journal's live segment ids, ascending, and its manifest's saved
    /// `next_seg_id`, after finalizing whatever segment is open. Read from the
    /// in-memory manifest under the lock: it is saved after every append, so
    /// it is never behind the file. The counter is the listing's generation
    /// (`list_handler::segment_listing`); a journal never retires a segment
    /// without minting a higher one, so here it also equals the greatest live
    /// id plus one — carried all the same, so both families ride one rule.
    pub async fn live_segments_for_backup(&self, actor_id: &[u8; 32]) -> Result<(Vec<u32>, u32)> {
        let actor_state = self.actor_state(actor_id).await?;
        let mut state = actor_state.lock().await;
        state
            .store
            .finalize_open()
            .with_context(|| format!("finalize open {} segment", K::Manifest::LABEL))?;
        let km = K::kind_manifest_mut(&mut state.manifest);
        let mut ids = km.live_segments.clone();
        ids.sort_unstable();
        Ok((ids, km.next_seg_id))
    }

    /// One journal segment's backup description — size, both hashes, bucket,
    /// record count. The journal twin of
    /// `SegmentManager::describe_segment_for_backup`.
    pub async fn describe_segment_for_backup(
        &self,
        actor_id: &[u8; 32],
        segment_id: u32,
    ) -> Result<fauna_segment_store::SegmentBackupMeta> {
        let actor_state = self.actor_state(actor_id).await?;
        let segment = {
            let mut state = actor_state.lock().await;
            state
                .store
                .finalize_open()
                .with_context(|| format!("finalize open {} segment", K::Manifest::LABEL))?;
            state
                .store
                .open_segment(segment_id)
                .with_context(|| format!("open {} segment {segment_id}", K::Manifest::LABEL))?
        };
        Ok(fauna_segment_store::SegmentBackupMeta {
            segment_id,
            bucket: segment.header.bucket.clone(),
            byte_size: segment.size_bytes()?,
            file_blake3: segment.file_blake3()?,
            meta_blake3: segment.meta_blake3()?,
            record_count: segment.header.record_count,
            created_at_secs: segment.header.created_at_secs,
        })
    }

    /// A finalized journal segment's pair, both files read under the lock.
    /// The journal twin of `SegmentManager::read_segment_pair`.
    pub async fn read_segment_pair(
        &self,
        actor_id: &[u8; 32],
        segment_id: u32,
    ) -> Result<fauna_segment_store::SegmentPairBytes> {
        let actor_state = self.actor_state(actor_id).await?;
        let mut state = actor_state.lock().await;
        state
            .store
            .finalize_open()
            .with_context(|| format!("finalize open {} segment", K::Manifest::LABEL))?;
        let segment = state
            .store
            .open_segment(segment_id)
            .with_context(|| format!("open {} segment {segment_id}", K::Manifest::LABEL))?;
        let dat = std::fs::read(segment.path())
            .with_context(|| format!("read {}", segment.path().display()))?;
        let meta = std::fs::read(segment.meta_path())
            .with_context(|| format!("read {}", segment.meta_path().display()))?;
        Ok(fauna_segment_store::SegmentPairBytes { dat, meta })
    }

    /// On-disk path of a journal segment's `.dat`.
    pub fn segment_file_path(&self, actor_id: &[u8; 32], segment_id: u32) -> PathBuf {
        K::segments_root(&self.data_dir, actor_id).join(format!("seg-{segment_id:08}.dat"))
    }

    /// On-disk path of a journal segment's `.meta` sidecar.
    pub fn segment_meta_path(&self, actor_id: &[u8; 32], segment_id: u32) -> PathBuf {
        K::segments_root(&self.data_dir, actor_id).join(format!("seg-{segment_id:08}.meta"))
    }

    /// Apply `f` to the actor's manifest under the per-actor lock and
    /// `save_atomic` the result when `f` returns `true`.
    pub async fn update_manifest(
        &self,
        actor_id: &[u8; 32],
        f: impl FnOnce(&mut K::Manifest) -> bool,
    ) -> Result<()> {
        let actor_state = self.actor_state(actor_id).await?;
        let mut state = actor_state.lock().await;
        if f(&mut state.manifest) {
            state
                .manifest
                .save_atomic(&state.manifest_path)
                .map_err(|e| anyhow::anyhow!("save {} manifest: {e}", K::Manifest::LABEL))?;
        }
        Ok(())
    }

    /// The journal twin of `SegmentManager::floor_counter`: raise the
    /// journal's saved counter to at least `floor`, never lower it, under the
    /// per-actor lock — the same raise and the same refusal, so each family
    /// floors on its own key (`segment-backup-protocol.md` § Client-device
    /// custodian (pull) → *Restore* → *Recovery into the lived-in nest that
    /// regressed*, part (0)). Returns the counter as it now stands.
    pub async fn floor_counter(&self, actor_id: &[u8; 32], floor: u32) -> Result<u32> {
        fauna_segment_store::check_counter_floor(floor)?;
        let mut now = 0;
        self.update_manifest(actor_id, |m| {
            let km = K::kind_manifest_mut(m);
            let raised = fauna_segment_store::raise_counter(km, floor);
            now = km.next_seg_id;
            raised
        })
        .await?;
        Ok(now)
    }

    /// S6.8d2 — drop tombstones whose `deleted_at` is older than `cutoff`
    /// (epoch seconds; the caller computes it from the effective
    /// `tombstone_retention_days.max(7)` policy, the same window that kind's
    /// sync serve path enforces on its own expunged table). Past that window a
    /// client is told to full-resync, so a pruned tombstone is unobservable;
    /// the manifest's tombstones have no other production consumer (they are a
    /// DR artifact). A `None` `deleted_at` (a mail `Move` tombstone) is never
    /// pruned. Returns the number pruned.
    pub async fn prune_tombstones(&self, actor_id: &[u8; 32], cutoff: i64) -> Result<u32> {
        let actor_state = self.actor_state(actor_id).await?;
        let mut state = actor_state.lock().await;
        let tombstones = K::tombstones_mut(&mut state.manifest);
        let before = tombstones.len();
        tombstones.retain(|t| K::tombstone_deleted_at(t).is_none_or(|d| d >= cutoff));
        let pruned = before - tombstones.len();
        if pruned > 0 {
            state
                .manifest
                .save_atomic(&state.manifest_path)
                .map_err(|e| anyhow::anyhow!("save {} manifest: {e}", K::Manifest::LABEL))?;
        }
        Ok(pruned as u32)
    }
}

/// A placement journal's own directory layout means it cannot ride
/// `SegmentManager::rename_scope`; the three rules are shared instead, through
/// `rename_scope_dir`.
impl<K: PlacementKind> fauna_segment_store::ActorScopedStore for PlacementJournal<K> {
    fn kind(&self) -> &'static str {
        K::SCOPE_KIND
    }

    fn rename_actor(
        &self,
        old: &[u8; 32],
        new: &[u8; 32],
    ) -> std::io::Result<fauna_segment_store::ScopeRenameOutcome> {
        // Evict BOTH before the rename: a cached actor state holds the
        // pre-rename manifest path and an open `FramedSegmentStore`, so a
        // later save through it would resurrect the old directory beside the
        // moved one. Same rule `SegmentManager::rename_scope` follows.
        self.actors.remove(old);
        self.actors.remove(new);
        fauna_segment_store::rename_scope_dir(
            &K::segments_root(&self.data_dir, old),
            &K::segments_root(&self.data_dir, new),
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Adopting a backed-up journal
//
// A backed-up message kind's corpus is its content segments AND its placement
// journal, and restoring it puts the journal back where the account can serve
// from it (`backup-destinations.md` § Third destination kind → *Where restored
// mail lands*; mechanics `segment-backup-protocol.md` § Client-device custodian
// (pull) → *Restore* → *The placement journal rides the set*). The journal owns
// its lock and its fold, so the adoption lives here, behind both; the caller
// (`backup::materialize`) owns the unseal, the anchor check and the SQLite
// transaction that makes the result live.
// ─────────────────────────────────────────────────────────────────────────────

/// What a placement journal must be able to say about a fold for a backed-up
/// corpus to be adopted over it.
///
/// Its own trait rather than two more items on [`PlacementKind`]: only a kind
/// that is backed up needs them, and a kind gains them when it joins the sweep
/// — all three journals have them since calendar and card joined mail there.
pub trait AdoptableJournal: PlacementKind {
    /// How much **content-bearing history** a fold holds: what is filed, what
    /// was filed and removed, and every UID already spent. Zero means the
    /// journal has never placed anything — it holds mailbox-level state at
    /// most, which is scaffolding.
    ///
    /// Spent UIDs count on their own, because tombstones are pruned after
    /// their retention window and a journal whose every message was removed
    /// long ago would otherwise read as one that never held any.
    fn held_history(fold: &Self::Manifest) -> u64;

    /// Is `current` the fold of `corpus` plus, at most, container-level events
    /// recorded after it? True means the journal on disk is this ceremony's
    /// own earlier adoption of that same corpus.
    fn is_adoption_of(current: &Self::Manifest, corpus: &Self::Manifest) -> bool;
}

/// What a target's journal is, relative to a corpus about to be adopted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptionVerdict {
    /// The journal has never placed anything. Whatever it holds is
    /// scaffolding, and the corpus supersedes it.
    Fresh,
    /// The journal already IS this corpus: an earlier run of this ceremony
    /// adopted it and was interrupted before it could be made live.
    Resume,
    /// The journal carries history of its own. Adopting over it would be a
    /// merge into a lived-in account, which the ceremony refuses.
    LivedIn {
        /// [`AdoptableJournal::held_history`] of the journal as it stands.
        history: u64,
    },
}

/// A corpus's journal segments, written out beside the journal they may
/// replace and folded — staged, and not yet adopted.
///
/// Staging is what lets the caller decide before it writes anything it cannot
/// take back: the fold is what the verdict is judged against, and the staged
/// files are what the adoption renames into place. Dropping it removes the
/// staging directory, so a refusal anywhere between staging and adoption
/// leaves no residue.
pub struct StagedJournal<K: PlacementKind> {
    dir: PathBuf,
    ids: Vec<u32>,
    fold: K::Manifest,
}

impl<K: PlacementKind> StagedJournal<K> {
    /// The corpus's fold: the placement state the adoption will install.
    pub fn fold(&self) -> &K::Manifest {
        &self.fold
    }
}

impl<K: PlacementKind> Drop for StagedJournal<K> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The staging directory's name prefix, inside the journal's own root so a
/// staged file is renamed into place rather than copied across filesystems.
/// The journal's replay lists `seg-*.dat` files of the root itself and never
/// descends, so a staging directory is invisible to it.
const ADOPT_STAGING_PREFIX: &str = ".adopt-staging-";

/// The two file names of one journal segment.
fn segment_file_names(segment_id: u32) -> [String; 2] {
    [
        // `.meta` first: written, and renamed into place, before its `.dat`,
        // so a `.dat` is never on disk ahead of the sidecar that describes it.
        format!("seg-{segment_id:08}.meta"),
        format!("seg-{segment_id:08}.dat"),
    ]
}

/// Write `bytes` to a new file at `path` and flush it to disk.
fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))?;
    Ok(())
}

impl<K: AdoptableJournal> PlacementJournal<K> {
    /// Stage a corpus's journal: write its segment pairs into a staging
    /// directory inside the journal's root, prove each one reopens, and fold
    /// them.
    ///
    /// `pairs` are `(segment_id, dat, meta)`, already verified by the caller
    /// against the corpus's own mirror. Nothing of the live journal is read or
    /// touched here.
    pub async fn stage_corpus(
        &self,
        actor_id: &[u8; 32],
        pairs: &[(u32, Vec<u8>, Vec<u8>)],
    ) -> Result<StagedJournal<K>> {
        let root = K::segments_root(&self.data_dir, actor_id);
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create the journal area at {}", root.display()))?;

        // Residue of a run that died between staging and adoption. It holds
        // nothing the journal serves, so it is cleared rather than classified.
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(ADOPT_STAGING_PREFIX))
                {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }

        let mut token = [0u8; 8];
        getrandom::fill(&mut token).map_err(|e| anyhow::anyhow!("staging token: {e}"))?;
        let dir = root.join(format!("{ADOPT_STAGING_PREFIX}{}", hex::encode(token)));
        std::fs::create_dir(&dir)
            .with_context(|| format!("create the staging directory {}", dir.display()))?;
        // From here on the guard owns the directory, so every early return
        // below removes it.
        let mut staged = StagedJournal::<K> {
            dir,
            ids: Vec::with_capacity(pairs.len()),
            fold: K::Manifest::default(),
        };

        for (segment_id, dat, meta) in pairs {
            let [meta_name, dat_name] = segment_file_names(*segment_id);
            write_new_synced(&staged.dir.join(meta_name), meta)?;
            let dat_path = staged.dir.join(dat_name);
            write_new_synced(&dat_path, dat)?;
            // The replay skips an unopenable HIGHEST segment as a crash tail.
            // That is right for a live journal and wrong for a corpus, where
            // every segment the mirror names must be there — so each one is
            // opened here, where a failure is a refusal and not a skip.
            FramedSegment::open(&dat_path).map_err(|e| {
                anyhow::anyhow!("the corpus's journal segment {segment_id} does not reopen: {e}")
            })?;
            staged.ids.push(*segment_id);
        }
        staged.ids.sort_unstable();
        staged.fold = K::rebuild_from_segments(&staged.dir)
            .with_context(|| format!("fold the corpus's {} journal", K::Manifest::LABEL))?;
        Ok(staged)
    }

    /// What the actor's journal is, relative to `staged`. Reads only.
    ///
    /// A pre-flight: the caller asks before it writes the content family, so a
    /// lived-in account is refused with nothing written in either family.
    /// [`Self::adopt_staged`] asks again under the same lock it writes under,
    /// and that answer is the one that binds.
    pub async fn adoption_verdict(
        &self,
        actor_id: &[u8; 32],
        staged: &StagedJournal<K>,
    ) -> Result<AdoptionVerdict> {
        let actor_state = self.actor_state(actor_id).await?;
        let state = actor_state.lock().await;
        Ok(Self::verdict(&state.manifest, &staged.fold))
    }

    fn verdict(current: &K::Manifest, corpus: &K::Manifest) -> AdoptionVerdict {
        let history = K::held_history(current);
        if history == 0 {
            AdoptionVerdict::Fresh
        } else if K::is_adoption_of(current, corpus) {
            AdoptionVerdict::Resume
        } else {
            AdoptionVerdict::LivedIn { history }
        }
    }

    /// Adopt a staged corpus as the actor's journal, under the journal's own
    /// per-actor lock, and return the fold the journal now holds.
    ///
    /// On a **fresh** journal the corpus supersedes whatever was there:
    /// afterwards the journal area holds the corpus's segments, byte for byte
    /// under their own ids, and nothing else. What it replaces is scaffolding
    /// by the verdict's own definition — a journal that has never placed
    /// anything — and it is regenerable: the standard mailboxes are re-seeded
    /// on first touch, and the corpus brings the owner's real mailbox tree.
    ///
    /// On a **resumed** adoption the corpus's segments are already there and
    /// are kept; only one a crash left incomplete is put back. Segments
    /// recorded *after* the adoption are kept too — they are the journal's own
    /// history now.
    ///
    /// A **lived-in** journal is refused with nothing changed, as
    /// `Ok(Err(history))`: a refusal the caller renders, not a fault.
    ///
    /// Every step is safe to repeat. Each file is renamed into place whole, so
    /// a crash leaves each name holding either what was there or what belongs
    /// there, and the manifest is saved last — so until the adoption is
    /// complete the journal still reads as what it was, and the retry decides
    /// exactly as this run did.
    pub async fn adopt_staged(
        &self,
        actor_id: &[u8; 32],
        staged: StagedJournal<K>,
    ) -> Result<std::result::Result<K::Manifest, u64>> {
        let label = K::Manifest::LABEL;
        let actor_state = self.actor_state(actor_id).await?;
        let mut state = actor_state.lock().await;
        state
            .store
            .finalize_open()
            .with_context(|| format!("finalize open {label} segment before adoption"))?;

        let verdict = Self::verdict(&state.manifest, &staged.fold);
        if let AdoptionVerdict::LivedIn { history } = verdict {
            return Ok(Err(history));
        }

        let root = K::segments_root(&self.data_dir, actor_id);
        let corpus_names: HashSet<String> = staged
            .ids
            .iter()
            .flat_map(|id| segment_file_names(*id))
            .collect();

        // 1 — every corpus file into place, whole. A name already holding
        // exactly these bytes is left alone, which is what makes a resumed
        // adoption rewrite nothing.
        for id in &staged.ids {
            for name in segment_file_names(*id) {
                let (from, to) = (staged.dir.join(&name), root.join(&name));
                let wanted = std::fs::read(&from)
                    .with_context(|| format!("read the staged {}", from.display()))?;
                if std::fs::read(&to).is_ok_and(|held| held == wanted) {
                    continue;
                }
                std::fs::rename(&from, &to)
                    .with_context(|| format!("place {} in the journal", to.display()))?;
            }
        }

        // 2 — on a fresh journal, what the corpus supersedes goes. After the
        // placing above, so at no point does the journal area hold neither.
        if verdict == AdoptionVerdict::Fresh {
            for entry in std::fs::read_dir(&root)
                .with_context(|| format!("read the journal area {}", root.display()))?
            {
                let entry = entry.context("read a journal area entry")?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.starts_with("seg-") && !corpus_names.contains(name) {
                    std::fs::remove_file(entry.path()).with_context(|| {
                        format!("remove superseded scaffolding {}", entry.path().display())
                    })?;
                }
            }
        }

        // 3 — the fold, from the journal as it now stands, saved last.
        let adopted = K::rebuild_from_segments(&root)
            .with_context(|| format!("rebuild the {label} manifest from the adopted journal"))?;
        adopted
            .save_atomic(&state.manifest_path)
            .map_err(|e| anyhow::anyhow!("save the adopted {label} manifest: {e}"))?;
        state.manifest = adopted.clone();
        Ok(Ok(adopted))
    }
}

#[cfg(test)]
pub(crate) mod replay_corruption_support {
    //! The three sidecar-damage shapes the mail/cal/card placement-journal
    //! rebuild tests share, homed once so the three kinds test the *same*
    //! corruption rather than three approximations of it — the drift that let
    //! mail reach 2026-08-23 without the rebuild its twins had.
    //!
    //! Each mirrors a real `FramedSegment::open` failure exit
    //! (`libs/fauna-segment-store/src/segment.rs`); the 2026-08-29 security
    //! review's probe confirmed the first two against a
    //! fully-finalized MIDDLE segment.

    use std::path::{Path, PathBuf};

    fn meta_path(root: &Path, seg_id: u32) -> PathBuf {
        root.join(format!("seg-{seg_id:08}.meta"))
    }

    /// Torn write / damaged `.meta` -> `Encoding("decode sidecar: ...")`.
    pub(crate) fn damage_sidecar(root: &Path, seg_id: u32) {
        let p = meta_path(root, seg_id);
        assert!(p.exists(), "seeding bug: {} does not exist", p.display());
        std::fs::write(&p, b"definitely not dag-cbor").expect("damage sidecar");
    }

    /// Missing `.meta` -> `InvalidSegment("missing sidecar ... (segment not
    /// finalized?)")` — the ONE crash-tail shape the rebuild may skip.
    pub(crate) fn remove_sidecar(root: &Path, seg_id: u32) {
        let p = meta_path(root, seg_id);
        assert!(p.exists(), "seeding bug: {} does not exist", p.display());
        std::fs::remove_file(&p).expect("remove sidecar");
    }

    /// Re-stamp both version numbers past this binary's writer version ->
    /// `SchemaMismatch` — a segment a NEWER binary wrote. Both numbers must
    /// move: `check_format_compatibility` reads a newer file with an
    /// unraised reader floor as merely additive, hence readable.
    pub(crate) fn restamp_sidecar_to_future_version(root: &Path, seg_id: u32) {
        let p = meta_path(root, seg_id);
        let bytes = std::fs::read(&p).expect("read sidecar");
        let mut value: fauna_cbor::Value =
            fauna_cbor::decode_strict(&bytes).expect("sidecar decodes as generic dag-cbor");
        let fauna_cbor::Value::Map(map) = &mut value else {
            panic!("sidecar is not a dag-cbor map");
        };
        for key in ["format_version", "min_reader_format_version"] {
            map.insert(key.to_string(), fauna_cbor::Value::Integer(999));
        }
        let out = fauna_cbor::encode_canonical(&value).expect("re-encode sidecar");
        std::fs::write(&p, out).expect("write re-stamped sidecar");
    }
}

/// Fetch (or lazily create) the per-key serialization lock in a
/// `[u8; 32]`-keyed lock registry — the exact shape [`conv`]'s per-channel
/// `SEQ_LOCKS` and [`mail`]'s per-actor `SEQ_LOCKS` share (both docs called
/// this deferred until a second kind existed to shape it; `conv` is that
/// second kind). Each kind keeps its own registry — the locks must stay
/// per-kind, only the get-or-insert is common — so this takes the registry by
/// reference rather than owning one itself.
pub(crate) fn keyed_seq_lock(
    registry: &DashMap<[u8; 32], Arc<Mutex<()>>>,
    key: &[u8; 32],
) -> Arc<Mutex<()>> {
    registry
        .entry(*key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Per-actor serialization of the lived-in recovery's point-read appends — the
/// `held_ever` read and the append it gates run under it, so two recoveries of
/// one actor never both find a record missing and both append it. The
/// point-read kinds have no `seq` cursor and hence no append lock of their own;
/// this registry exists only for [`append_recovered_point_read_record`].
static RECOVERED_APPEND_LOCKS: std::sync::LazyLock<DashMap<[u8; 32], Arc<Mutex<()>>>> =
    std::sync::LazyLock::new(DashMap::new);

/// Append one **recovered** record of a point-read DAV kind (`calendar`,
/// `card`) — the shared body of [`cal::append_recovered_record`] and
/// [`card::append_recovered_record`], on [`mail::append_recovered_record`]'s
/// shape (`segment-backup-protocol.md` § Client-device custodian (pull) →
/// *Restore* → *Recovery's calendar and contacts arms*):
///
/// - **held means held EVER** — live **or tombstoned** mirror rows; a held
///   record writes nothing and returns `Ok(None)`;
/// - the bytes and floor ride verbatim (a DAV floor carries no local cursor to
///   restamp), filed in the bucket of the floor's own server receive time;
/// - the mirror row (`insert`, the kind's canonical helper) and `place` commit
///   in **one transaction**, so no crash leaves a recovered record live with
///   no row, and the row door's empty-body guard sees the mirror row it needs.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_recovered_point_read_record<T>(
    mgr: &SegmentManager,
    cache_db: &crate::db::CacheDb,
    actor: &[u8; 32],
    kind: &'static str,
    envelope_bytes: &[u8],
    floor_bytes: &[u8],
    created_at: i64,
    insert: fn(&rusqlite::Connection, &[u8; 32], u32, &Cid, &str, i64) -> Result<()>,
    place: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
) -> Result<Option<(u32, T)>> {
    let cid = Cid::of_dag_cbor(envelope_bytes);
    let lock = keyed_seq_lock(&RECOVERED_APPEND_LOCKS, actor);
    let _guard = lock.lock().await;

    let held = {
        let conn = cache_db.conn().await;
        records_db::held_ever(&conn, actor, kind, &cid)?
    };
    if held {
        return Ok(None);
    }
    let bucket = fauna_segment_store::bucket_for(created_at);
    let outcome = mgr
        .append_record_with_bucket(actor, cid, envelope_bytes, floor_bytes, &bucket)
        .await
        .map_err(|e| anyhow::anyhow!("append_record_with_bucket (recovered {kind}): {e}"))?;

    let conn = cache_db.conn().await;
    let tx = conn
        .unchecked_transaction()
        .context("begin the recovered-record transaction")?;
    insert(&tx, actor, outcome.segment_id, &cid, &bucket, created_at)
        .context("insert the recovered record's mirror row")?;
    let placed = place(&tx)?;
    tx.commit()
        .context("commit the recovered record with its row")?;
    Ok(Some((outcome.segment_id, placed)))
}

/// Rewrite one bucket of `scope_id`'s segments, dropping every record the
/// mirror marks tombstoned — the kind-agnostic skeleton **all five** kinds'
/// `compact_bucket` coordinators share (`mail`, `conv`, `post`, `cal`, `card`).
///
/// Everything here is kind-independent and was, until this extraction, written
/// out five times: the live-set pre-fetch, the candidate-id read, the filtered
/// file-level rewrite, the mirror transaction's begin/commit, and the outcome.
/// Exactly two steps genuinely differ per kind, and both are the caller's:
///
/// * `mirror_row` — decode one survivor's floor blob into that kind's mirror-row
///   struct. **The timestamp-unit trap lives here**: `mail`/`conv`/`post` floors
///   carry epoch **milliseconds** (hence their `/ 1000` before `bucket_for`),
///   `calendar`/`card` carry **seconds**. Keeping this decode at the call site
///   is deliberate — it is the one decision a new kind must actually make, and
///   burying it behind a units parameter is how the `/ 1000` gets copied wrong.
/// * `apply_tx` — the kind's named `records_db::apply_*_compaction_tx` wrapper,
///   invoked inside the transaction opened here. Taking the *named wrapper*
///   rather than a kind string keeps the kind tag greppable at the call site,
///   the convention [`records_db`] already sets for its own point-read wrappers
///   ("they exist to name the kind, not to duplicate this statement").
///
/// Ordering is load-bearing and was identical in all five copies: the live set
/// is read **before** `compact_with_filter` (which allocates the new id and
/// swaps the manifest atomically, inside the per-scope mutex), and the mirror
/// transaction lands **after** the file rewrite — so a crash between them
/// leaves the mirror describing the pre-rewrite segments, stale but never
/// pointing at records the rewrite dropped.
///
/// Survivors are exactly the records with a live `segment_records` row, which
/// is why each kind's tombstone producers and orphan reaper had to land before
/// its compaction arm: without them nothing is ever tombstoned and a rewrite
/// copies every record forward, reclaiming zero bytes.
pub(crate) async fn compact_bucket_with<R>(
    mgr: &SegmentManager,
    cache_db: &crate::db::CacheDb,
    scope_id: &[u8; 32],
    kind: &str,
    plan: &CompactionPlan,
    mirror_row: impl Fn(Cid, &[u8]) -> Result<R>,
    apply_tx: impl FnOnce(&rusqlite::Transaction, Option<u32>, &[R]) -> Result<()>,
) -> Result<mail::BucketCompactionOutcome> {
    // Pre-fetch the live (segment_id, record_cid) set for the inputs. The
    // mirror keys on the full Cid, exactly what the `compact_with_filter`
    // closure receives (`Fn(&Cid)`), so the membership test is a direct lookup.
    let alive_set = cache_db
        .segment_records_live_set_for_segments(scope_id, kind, &plan.inputs)
        .await
        .with_context(|| format!("pre-fetch live {kind} record set"))?;
    let alive_cids: HashSet<Cid> = alive_set.iter().map(|(_seg, cid)| *cid).collect();

    // Allocate the next candidate from the manifest's counter (read before
    // `compact_with_filter`, which increments it atomically inside).
    let candidate = {
        let manifest = mgr
            .load_manifest(scope_id)
            .await
            .map_err(|e| anyhow::anyhow!("load manifest for {kind} compact candidate: {e}"))?;
        manifest.kind_manifest.next_seg_id
    };

    // File-level compaction + manifest swap (inside the per-scope mutex).
    let result = mgr
        .compact_with_filter(scope_id, plan.clone(), candidate, |cid: &Cid| {
            alive_cids.contains(cid)
        })
        .await
        .map_err(|e| anyhow::anyhow!("compact_with_filter ({kind}): {e}"))?;

    // Build the new segment's mirror rows from the surviving records.
    // `read_envelopes_bulk` yields (record_cid, envelope_bytes, floor_bytes);
    // the mirror has no byte-offset column, so reads address a record by its
    // Cid through the CARv2 index rather than a mirrored offset.
    let new_records: Vec<R> = if let Some(new_id) = result {
        let rows = mgr
            .read_envelopes_bulk(scope_id, new_id)
            .await
            .map_err(|e| anyhow::anyhow!("read new compacted {kind} segment: {e}"))?;
        let mut out = Vec::with_capacity(rows.len());
        for (rid, _env_bytes, floor_bytes) in rows {
            out.push(mirror_row(rid, &floor_bytes)?);
        }
        out
    } else {
        Vec::new()
    };

    // SQL transaction: tombstone the input segments' rows + INSERT one row per
    // survivor in the rewritten segment.
    let conn = cache_db.conn().await;
    let tx = conn
        .unchecked_transaction()
        .with_context(|| format!("begin {kind} compaction tx"))?;
    apply_tx(&tx, result, &new_records)?;
    tx.commit()
        .with_context(|| format!("commit {kind} compaction tx"))?;
    drop(conn);

    Ok(mail::BucketCompactionOutcome {
        new_segment: result,
        consumed: plan.inputs.clone(),
    })
}
/// Every `record_cid` on `actor`'s live rows in `table` — the live set a
/// point-read kind's orphan reaper measures the `segment_records` mirror
/// against. Shared by `calendar` (`bridge_caldav_events`) and `card`
/// (`bridge_carddav_cards`), whose row shapes are deliberate structural
/// mirrors of one another (`carddav-server.md` § Storage model), so their DR
/// postures must not diverge (priority #4).
///
/// Plain SQL on a **caller-held** connection, deliberately: the read has to
/// share the reaper's critical section, so a concurrent PUT's row INSERT cannot
/// commit a row pointing at a record between this read and the tombstone write.
///
/// **Fails closed on a NULL `record_cid`.** Post-cutover every row is born with
/// one (pre-cutover rows were wiped by the boot reset), so a NULL is
/// corruption. Returning an incomplete live set would make that row's record
/// look orphaned — and an orphan is tombstoned, then physically reclaimed by
/// the next compaction, which is user-irrecoverable loss
/// (`docs/goal/principles.md` § No user-data loss). A loud refusal is
/// repairable; a silent under-read is not. This guard is exactly why the two
/// kinds share one implementation rather than two copies: it is the kind of
/// invariant a fix lands on one side of and forgets on the other.
///
/// `table` is a `&'static str` naming a table in our own schema — never user
/// input. It is interpolated because SQLite cannot bind an identifier as a
/// parameter; the `actor` filter *is* bound.
///
/// Private: the only caller is [`reap_orphan_records`], which always pairs
/// `table` with a same-kind `kind` string via [`SegmentKind`] — a second call
/// site could reintroduce the crossed-pair risk item 1 of
/// [`records_db::reap_orphan_point_read_records`]'s `# Safety` block guards
/// against by construction rather than by discipline
/// ().
fn live_record_cids(
    conn: &rusqlite::Connection,
    table: &'static str,
    actor: &[u8; 32],
) -> Result<HashSet<Cid>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT record_cid FROM {table} WHERE actor_id = ?1"
        ))
        .with_context(|| format!("prepare live_record_cids ({table})"))?;
    let rows = stmt
        .query_map(rusqlite::params![&actor[..]], |r| {
            r.get::<_, Option<Vec<u8>>>(0)
        })
        .with_context(|| format!("query live_record_cids ({table})"))?;
    let mut out = HashSet::new();
    for row in rows {
        let Some(blob) = row.context("read record_cid")? else {
            anyhow::bail!(
                "live_record_cids: a {table} row has NULL record_cid — \
                 refusing to reap against an incomplete live set"
            );
        };
        let arr: [u8; 36] = crate::db::blob_to_array(blob.as_slice(), "record_cid")?;
        let cid = Cid::from_bytes(arr).map_err(|e| anyhow::anyhow!("record_cid not a Cid: {e}"))?;
        out.insert(cid);
    }
    Ok(out)
}

/// Which point-read segment kind an orphan reap targets. The live-CID table
/// and the record kind must always name the *same* segment — crossing them
/// (calendar's live set against card's kind) tombstones a whole unrelated
/// corpus as spurious orphans (; see the
/// `# Safety` block on [`records_db::reap_orphan_point_read_records`]). This
/// type is the one place the pairing is assembled — [`cal::reap_orphan_records`]
/// and [`card::reap_orphan_records`] each pass their own variant and nothing
/// else, so a call site can no longer supply the two as independent strings.
#[derive(Clone, Copy)]
pub(crate) enum SegmentKind {
    Calendar,
    Card,
}

impl SegmentKind {
    fn live_rows_table(self) -> &'static str {
        match self {
            SegmentKind::Calendar => cal::LIVE_ROWS_TABLE,
            SegmentKind::Card => card::LIVE_ROWS_TABLE,
        }
    }

    fn kind(self) -> &'static str {
        match self {
            SegmentKind::Calendar => cal::KIND,
            SegmentKind::Card => card::KIND,
        }
    }
}

/// Tombstone `actor`'s orphaned point-read content records for one segment
/// kind — reads the live-CID set through [`live_record_cids`] and hands it to
/// [`records_db::reap_orphan_point_read_records`] under the same held
/// connection, so a concurrent PUT's row INSERT cannot race the read. Shared
/// by [`cal::reap_orphan_records`] and [`card::reap_orphan_records`], whose
/// own bodies were byte-for-byte identical modulo which [`SegmentKind`] they
/// name before this — `card`'s doc comment already called itself "the
/// structural twin" of `cal`'s.
pub(crate) async fn reap_orphan_records(
    cache_db: &crate::db::CacheDb,
    actor: &[u8; 32],
    now: i64,
    segment: SegmentKind,
) -> Result<u32> {
    let conn = cache_db.conn().await;
    let live = live_record_cids(&conn, segment.live_rows_table(), actor)?;
    records_db::reap_orphan_point_read_records(&conn, actor, segment.kind(), now, &live)
}

/// Whether `cid` still resolves to a live point-read record for `actor` under
/// `kind` — the reaper tests' liveness probe, shared by [`cal::tests`] and
/// [`card::tests`] (their bodies were identical modulo `KIND`).
#[cfg(test)]
pub(crate) async fn is_live(
    cache_db: &crate::db::CacheDb,
    actor: &[u8; 32],
    kind: &str,
    cid: &Cid,
) -> bool {
    cache_db
        .segment_records_lookup_record(actor, kind, cid)
        .await
        .unwrap()
        .is_some()
}

/// Read one point-read record's raw envelope + floor bytes from `actor`'s
/// segment store, resolving which segment holds it through the mirror. Shared
/// by `calendar` and `card`; the caller decodes the bytes into its own kind's
/// envelope/floor types.
///
/// `None` covers two cases, deliberately conflated at this layer because both
/// mean "no body to serve": the mirror has no live row for the CID (the record
/// is genuinely absent), or the mirror points at a record the segment file does
/// not hold. The second is mirror/disk **divergence** — warned and served as
/// absent rather than raised, so one bad record never fails a whole CalDAV
/// REPORT or addressbook-multiget. (`post::read_body_by_cid` and
/// `conv::read_one` make the same call for the same reason.)
pub(crate) async fn read_point_read_record(
    mgr: &SegmentManager,
    cache_db: &crate::db::CacheDb,
    actor: &[u8; 32],
    kind: &str,
    record_cid: &Cid,
) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    let Some(rec) = cache_db
        .segment_records_lookup_record(actor, kind, record_cid)
        .await?
    else {
        return Ok(None);
    };
    match mgr
        .read_record_with_floor_bytes(actor, rec.segment_id, record_cid)
        .await
    {
        Ok(pair) => Ok(Some(pair)),
        Err(ManagerError::RecordNotFound { .. }) => {
            tracing::warn!(
                actor = %hex::encode(actor),
                record_cid = ?record_cid,
                segment_id = rec.segment_id,
                kind,
                "segment_records mirror diverged from segment file (point-read)"
            );
            Ok(None)
        }
        Err(e) => Err(anyhow::anyhow!("read {kind} record: {e}")),
    }
}

/// Bulk-look up many records' CARv2 block byte-lengths, grouped by segment so
/// each segment file opens exactly once (its `MultihashIndexSorted` index is
/// parsed once, then probed per Cid; block bodies are never read). Returns
/// sizes aligned to the input order; `None` for any record whose segment can't
/// be opened or that's absent from its segment. Kind-agnostic — the index is
/// the segment store's, not the kind's — so `mgr` is whichever kind's manager
/// the refs belong to (`mail_segments`, `cal_segments`, `card_segments`).
///
/// The size source for the IMAP STATUS / SEARCH `LARGER`|`SMALLER` /
/// STORAGE-quota hot paths (`imap-server.md` §§ SEARCH, QUOTA;
/// `caldav-server.md` § QUOTA for the calendar/card share) — never a SQL
/// byte column. `refs` is `(segment_id, record_cid)` pairs for one actor, both
/// taken from the `segment_records` mirror row the caller already queried.
pub async fn record_sizes(
    mgr: &SegmentManager,
    actor_id: &[u8; 32],
    refs: &[(u32, Cid)],
) -> Result<Vec<Option<u64>>> {
    // Group input indices by segment so each segment opens exactly once.
    let mut by_seg: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (i, (seg_id, _cid)) in refs.iter().enumerate() {
        by_seg.entry(*seg_id).or_default().push(i);
    }

    let mut output: Vec<Option<u64>> = vec![None; refs.len()];
    for (seg_id, indices) in by_seg {
        // The mirror stores the full Cid the index keys on — no reconstruction.
        let cids: Vec<Cid> = indices.iter().map(|&i| refs[i].1).collect();
        let cid_refs: Vec<&Cid> = cids.iter().collect();
        // A segment that can't be opened (missing/corrupt file the mirror still
        // references) is a divergence: report size-unknown for its records
        // rather than failing the whole IMAP command. Same tolerance as
        // `read_envelopes_bulk`'s RecordNotFound skip — listing/quota are less
        // critical than a body fetch, so they degrade instead of erroring.
        match mgr
            .record_sizes_for_segment(actor_id, seg_id, &cid_refs)
            .await
        {
            Ok(sizes) => {
                for (k, &i) in indices.iter().enumerate() {
                    output[i] = sizes[k];
                }
            }
            Err(e) => {
                tracing::warn!(
                    actor = ?actor_id,
                    segment_id = seg_id,
                    error = %e,
                    "segment_records mirror references an unreadable segment (size lookup)"
                );
            }
        }
    }

    Ok(output)
}

/// Resolve a point-read record's sealed body given its row's (possibly
/// missing) `record_cid` — the shape shared by `cal::load_event_body` and
/// `card::load_card_body` since the record-identity cutover retired their
/// legacy-column fallback (a row without a `record_cid` is now genuinely
/// bodiless, never a signal to fall back to a SQLite column). `warn_missing`
/// logs the kind-specific "no record_cid" line; `read_body` is the caller's
/// own `read_record` + envelope decode, returning just the sealed body.
pub(crate) async fn load_body_via_record_cid<Fut>(
    record_cid: Option<Cid>,
    warn_missing: impl FnOnce(),
    read_body: impl FnOnce(Cid) -> Fut,
) -> Result<Option<Vec<u8>>>
where
    Fut: std::future::Future<Output = Result<Option<Vec<u8>>>>,
{
    let Some(cid) = record_cid else {
        warn_missing();
        return Ok(None);
    };
    read_body(cid).await
}

/// Register every `fauna.segments.*` WS-RPC kind on the dispatcher.
/// Sibling to `register_bridge_*_handlers` in `lib.rs::start_server`.
pub fn register_segments_handlers(b: &mut crate::rpc_router::RpcRouterBuilder) {
    list_handler::register_list_handler(b);
    list_handler::register_counter_floor_handler(b);
    compact_handler::register_compact_handler(b);
}

/// The record-count ceiling of one serve page — what `limit` clamps **down**
/// to, and what a non-positive `limit` means. Every already-shipped client
/// poll loop requests `limit: 0` expecting "the full page"
/// (`poll_inbound_conv`'s documented whole-tail contract), so treating `<= 0`
/// as anything smaller — the old `clamp(1, 500)` turned it into ONE record
/// per round trip — silently starves every production drain and falsifies
/// the gate-less arm-2 reconcile's "complete walk from 0" premise
/// (`devices.md` § Cross-device MLS group-state sync).
pub const SERVE_PAGE_MAX_RECORDS: i64 = 500;

/// Resolve a wire `limit` to the effective page size: non-positive means the
/// full [`SERVE_PAGE_MAX_RECORDS`] page; anything larger clamps down to it.
pub fn effective_fetch_limit(limit: i64) -> i64 {
    if limit <= 0 {
        SERVE_PAGE_MAX_RECORDS
    } else {
        limit.min(SERVE_PAGE_MAX_RECORDS)
    }
}

/// Byte budget for one assembled serve page: the reply must fit the 2 MiB
/// WS frame (`transport.md` § Max frame — one cap for every WS-RPC surface,
/// client and federation channel alike) minus a 64 KiB envelope headroom,
/// the same margin the mail relay budget uses
/// (`fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES`).
pub const SERVE_PAGE_BUDGET_BYTES: usize =
    fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE - 64 * 1024;

/// Per-record framing headroom beside the payload bytes (seq, ids, CBOR
/// map/array overhead, each byte string's own ≤ 9-byte head) — deliberately
/// generous; shared by every budgeted serve page. A caller's `wire_len`
/// measures its payload fields' raw `.len()`: every one is a byte string
/// (`serialization.md` § Canonical IPLD dag-cbor, "Variable-length byte
/// fields"), so raw length plus this headroom bounds the encoded record.
pub const RECORD_WIRE_OVERHEAD: usize = 128;

/// Cut an assembled serve page at [`SERVE_PAGE_BUDGET_BYTES`]: keep records
/// until the next would overflow, then close the page early. Returns
/// `(page, rest)` — `rest` starts with the record that did not fit, so a
/// caller whose page froze (`page.is_empty() && !rest.is_empty()`: the head
/// record alone exceeds the budget) can log it loudly with its own context.
/// Deliberately **never skip-and-continue** — every consumer walks these
/// pages by a contiguous cursor (and `mls_pull`'s ack PURGES the source), so
/// a record the cursor passes unserved is silent, irrecoverable loss; a
/// shorter page is always safe (the puller simply pulls again). The remedy
/// for a frozen head is a targeted heal, never a skip and never a bigger
/// frame. The ratified rule: `deployment-home-with-public-relay.md` § Relay
/// frame budget; this is its kind-agnostic core, shared by the conv serve
/// surfaces and `mail_pull`.
pub fn take_page_within_budget<T>(
    items: Vec<T>,
    wire_len: impl Fn(&T) -> usize,
) -> (Vec<T>, Vec<T>) {
    let mut page_bytes = 0usize;
    let mut items = items.into_iter();
    let mut page = Vec::new();
    let mut rest = Vec::new();
    for item in &mut items {
        let item_bytes = wire_len(&item) + RECORD_WIRE_OVERHEAD;
        if page_bytes + item_bytes > SERVE_PAGE_BUDGET_BYTES {
            rest.push(item);
            break;
        }
        page_bytes += item_bytes;
        page.push(item);
    }
    rest.extend(items);
    (page, rest)
}

#[cfg(test)]
mod page_budget_tests {
    use super::*;

    /// The shared serve-page budget and the mail relay budget are the same
    /// derivation (2 MiB frame − 64 KiB headroom) — `mail_pull` cites their
    /// equality, so pin it against silent drift.
    #[test]
    fn serve_page_budget_equals_the_mail_relay_budget() {
        assert_eq!(
            SERVE_PAGE_BUDGET_BYTES,
            fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES as usize
        );
    }

    #[test]
    fn page_cut_keeps_order_and_skips_nothing() {
        // Three items: two big ones that don't fit together, then a small one.
        let big = SERVE_PAGE_BUDGET_BYTES / 2;
        let (page, rest) = take_page_within_budget(vec![big, big, 64], |len| *len);
        assert_eq!(page, vec![big]);
        assert_eq!(rest, vec![big, 64], "the cut item leads the remainder");
    }

    #[test]
    fn over_budget_head_freezes_the_page_instead_of_skipping() {
        let (page, rest) =
            take_page_within_budget(vec![SERVE_PAGE_BUDGET_BYTES + 1, 64], |len| *len);
        assert!(page.is_empty());
        assert_eq!(rest.len(), 2, "nothing is skipped past");
    }
}

/// Emit one `fauna.segments.changed` push for a segment-id transition.
/// Single helper consumed by the production write paths
/// (`bridge_routing_handlers`, `bridge_imap_handlers`) and the
/// compaction worker — keeps the payload-build in one place so the
/// `extra: BTreeMap::new()` invariant (Plan 5 wire-shape default) can
/// evolve without combing through three open-coded call sites.
pub fn notify_segments_changed(
    ws: &crate::ws::WsState,
    actor_id: &[u8; 32],
    kind: &str,
    segment_id: u32,
    change: fauna_protocol::push_events::SegmentChange,
) {
    ws.notify_push(
        actor_id,
        fauna_protocol::PushEvent::SegmentsChanged(
            fauna_protocol::push_events::SegmentsChangedPayload {
                kind: kind.to_string(),
                actor_id: hex::encode(actor_id),
                segment_id,
                change,
                extra: std::collections::BTreeMap::new(),
            },
        ),
    );
}

/// Emit one `fauna.mail.received` per-record arrival push for the
/// recipient actor. Sibling to [`notify_segments_changed`] (segment
/// *lifecycle*); this is the per-record *arrival* push the mail-ingest
/// path fires on every genuinely-new placement (INBOX / Junk / Sent /
/// filter-target). Single helper so the minimal-payload invariant
/// (`actor_id` only + empty `extra`) lives in one place. Best-effort —
/// `notify_push` drops it if the recipient has no live WS connection.
/// Consumers: the client conversations inbound poll (wakes a prompt
/// `fauna.email.inbox.fetch`, which is INBOX-scoped so a non-INBOX
/// arrival self-filters to a no-op fetch) and the custodian pull's
/// push pump (wakes its pull debounce — it backs up all
/// mailboxes). Per `smtp-server.md` § Inbound client receive.
pub fn notify_mail_received(ws: &crate::ws::WsState, actor_id: &[u8; 32]) {
    ws.notify_push(
        actor_id,
        fauna_protocol::PushEvent::MailReceived(fauna_protocol::push_events::MailReceivedPayload {
            actor_id: hex::encode(actor_id),
            extra: std::collections::BTreeMap::new(),
        }),
    );
}

/// Fire `fauna.mail.flags_changed` to `actor_id`'s own sessions: a flag write
/// changed at least one of their `INBOX` rows, whoever made it (the MDA's
/// `fauna.bridges.store_flags` or an app's `fauna.email.inbox.mark_seen`). A
/// wake, not a payload — the app answers it with one
/// `fauna.email.inbox.flag_changes` call; best-effort, poll-backstopped like
/// [`notify_mail_received`]. Per `mail-app-surface.md` § Read state.
pub fn notify_mail_flags_changed(ws: &crate::ws::WsState, actor_id: &[u8; 32]) {
    ws.notify_push(
        actor_id,
        fauna_protocol::PushEvent::MailFlagsChanged(
            fauna_protocol::push_events::MailFlagsChangedPayload {
                actor_id: hex::encode(actor_id),
                extra: std::collections::BTreeMap::new(),
            },
        ),
    );
}

/// Test helpers shared across segments sub-modules. Not compiled outside
/// `#[cfg(test)]`.
#[cfg(test)]
pub(crate) mod test_helpers {
    use std::sync::Arc;

    use fauna_mail::segments::{
        CONTINUATION_ROLE_NORMAL, MAIL_FLOOR_FORMAT_VERSION, MailFloorMetadata,
    };
    use fauna_segment_store::SegmentManager;
    use tempfile::TempDir;

    use crate::db::CacheDb;
    use crate::routes::AppState;

    /// Build an `AppState` whose `mail_segments` live in a fresh tempdir —
    /// the minimal fixture shape `backup_source`, `list_handler`, and
    /// `segment_route`'s test modules each hand-copied byte-for-byte.
    /// `compact_handler`/`compaction` need more segment kinds wired per test
    /// and keep their own richer builder — genuinely different shapes, not
    /// this one under-built.
    pub fn build_state() -> (TempDir, Arc<AppState>) {
        let tmp = TempDir::new().expect("tempdir");
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let mut state = AppState::for_test(db.clone());
        state.mail_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "mail"));
        (tmp, Arc::new(state))
    }

    /// Build a minimal `MailFloorMetadata` with only `received_at`
    /// meaningful (all other auth-verdict fields set to reasonable
    /// defaults). Used by compaction and mail tests.
    pub fn floor(received_at: i64) -> MailFloorMetadata {
        floor_with_report_hash(received_at, Vec::new())
    }

    /// [`floor`] with an explicit report-hash (report-sharing.md § Content
    /// identity) — mail tests exercising the mirror column use this.
    pub fn floor_with_report_hash(received_at: i64, report_hash: Vec<u8>) -> MailFloorMetadata {
        MailFloorMetadata {
            format_version: MAIL_FLOOR_FORMAT_VERSION,
            received_at,
            timestamp: received_at / 1000,
            ciphertext_size: 0,
            sender_domain: "example.com".to_string(),
            spam_disposition: "accept".to_string(),
            is_own_submission: false,
            spf: "pass".into(),
            dkim: "pass".into(),
            dmarc: "pass".into(),
            dmarc_policy: "reject".into(),
            arc: "pass".into(),
            spam_score: 0,
            // Placeholders — append_record allocates and overwrites the seq, and
            // stamps stored_at from the local clock at the append itself.
            seq: 0,
            stored_at: 0,
            report_hash,
            continuation_role: CONTINUATION_ROLE_NORMAL,
            // Struct-update so the next additive floor field doesn't break this
            // fixture (the reason MailFloorMetadata carries a Default at all).
            ..Default::default()
        }
    }
}
