//! Segment file format — standard CARv2 + a small dag-cbor sidecar.
//!
//! On-disk layout, per segment id `N`:
//!
//! ```text
//! seg-NNNNNNNN.dat      // standard CARv2:
//!                       //   [pragma: 11 bytes]
//!                       //   [v2 header: 40 bytes]
//!                       //   [data section: (varint(len) || cid || block_bytes)* per record]
//!                       //   [MultihashIndexSorted index]
//!
//! seg-NNNNNNNN.meta     // dag-cbor (canonical) `SegmentSidecar` blob:
//!                       //   { kind, actor_id, segment_id, bucket, created_at_secs,
//!                       //     record_order: [cid, ...],
//!                       //     floor_metadata: [bytes, ...] }  // parallel to record_order
//!
//! seg-NNNNNNNN.tail     // OPEN segments only — the append journal:
//!                       //   varint(len) || dag-cbor `TailHeader`
//!                       //   (varint(len) || dag-cbor `TailEntry { cid, floor }`)*
//!                       // removed once finalize() has made the `.meta` durable.
//! ```
//!
//! ## Crash recovery — the append journal
//!
//! The `.meta` is written only at `finalize()`, so a crash with a segment open
//! would leave a `.dat` nothing can open. Each append therefore makes its block
//! durable in the `.dat` FIRST, then journals `(cid, floor_metadata)` to the
//! `.tail` and makes that durable, and only then returns: a journal entry
//! implies its block is on disk, and an append that returned `Ok` has both.
//! [`FramedSegment::recover_unfinalized`] rebuilds the finalized pair from the
//! two files — the `.dat`'s data section in write order, paired with the
//! journal's entries — dropping only blocks whose append never returned.
//!
//! The `.dat` file is **byte-identical to what `go-car` / `iroh-car` /
//! `kubo dag import` would produce** for the same records — Layer 3's Track C
//! cross-language conformance test (Task 3.6) feeds Rust-produced `.dat`s into
//! the Go go-car/v2 reader and back to confirm parity. The pre-Layer 3
//! framed format (custom magic + length-prefixed payload + BARE footer +
//! 8-byte trailer) is fully removed by Layer 3 Track A.
//!
//! ## Floor metadata sidecar — chosen approach
//!
//! `RecordEntry.floor_metadata` is opaque kind-specific bytes (mail kinds
//! encode `MailFloorMetadata`; conv kinds encode their own thing). The CARv2
//! file format only stores `(cid, block_bytes)` pairs and an index — there is
//! no per-block metadata slot. The Layer 3 plan task lists three placement
//! options (A: extra dag-cbor block per record with synthetic CID, B: wrap
//! each record into an envelope so the on-disk CID differs from the
//! consumer-visible record id, C: stuff per-record metadata into the
//! kind-level outer manifest).
//!
//! All three lose. A leaks Fauna-specific block-naming convention into the
//! `.dat` file (a third-party CARv2 tool sees synthetic blocks with no
//! provenance). B breaks the design invariant that the carv2 CID = the
//! consumer's record_id (callers query by record_id; if it doesn't match the
//! on-disk block CID, the carv2 reader can't lookup by it directly). C
//! doesn't fit the existing kind_manifest shape — that struct is *per-segment*
//! `(next_seg_id, live_segments, tombstoned_segments)`, not per-record; making
//! it per-record-per-segment would balloon its in-memory size by an order of
//! magnitude (one entry per record × every segment a scope ever held) and is
//! reloaded into RAM on every scope_state access in `SegmentManager`.
//!
//! The right placement is a **per-segment companion sidecar file**. The
//! `.dat` stays pure standard CARv2; segment-level meta (kind, actor_id,
//! bucket, created_at, segment_id) and per-record floor metadata both live
//! in `seg-NNNNNNNN.meta`. Loaded once at `open()`; rewritten at `finalize()`.
//! Tiny (single-digit KB per segment in typical use) and reads as one
//! contiguous dag-cbor blob.

use crate::SegmentStoreError;
use fauna_cbor::Cid;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

/// Per-record entry surfaced by `FramedSegment::iter_records`. The CID
/// is the carv2 block CID (= the consumer-passed record id, hashed under
/// dag-cbor + blake3-256 per `fauna_cbor::Cid::of_dag_cbor`). `floor_metadata` is
/// opaque kind-specific bytes the consumer decodes (mail / conv / cal
/// kinds bring their own deserializer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordEntry {
    pub cid: Cid,
    pub floor_metadata: Vec<u8>,
}

/// Companion sidecar file at `seg-NNNNNNNN.meta` — dag-cbor (canonical).
/// Carries all the segment metadata that doesn't fit in the standard CARv2
/// header + index (which only stores roots, version, characteristics, data
/// offsets, and the digest→offset index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SegmentSidecar {
    /// The `format_version` this binary writes into new sidecars. ⚠ A segment
    /// holds user mail/conv data, so under the alpha no-user-data-loss rule
    /// (`version-compatibility.md` § 1) the sidecar format must evolve
    /// **additively** within a major version; an incompatible change is a
    /// major-version event with a migration (§ I3), never reject-and-recreate.
    /// `open` no longer rejects on `format_version != SIDECAR_SCHEMA_VERSION`: it
    /// applies the two-number verdict of [`crate::version`], tolerating a
    /// newer-*additive* sidecar and erroring only on a newer-*breaking* one.
    /// (Same constraint as `MANIFEST_SCHEMA_VERSION` in `manifest.rs`.)
    format_version: u16,
    /// The oldest binary `format_version` that can still safely read this
    /// sidecar (the at-rest reader floor — § 2.2's `min_reader_version`,
    /// mirrored at-rest). `#[serde(default)]` so existing v1 sidecars written
    /// before this field existed deserialize to the baseline `1` and stay
    /// readable. See [`crate::version`].
    #[serde(default = "crate::version::baseline_min_reader_version")]
    min_reader_format_version: u16,
    /// Caller-defined kind tag (e.g. "mail", "conv"). Not validated here.
    kind: String,
    #[serde(with = "serde_bytes")]
    actor_id: [u8; 32],
    segment_id: u32,
    /// Caller-defined bucket key (e.g. "2026-05"). The store rotates on
    /// bucket-change; finalized segments carry the bucket they were
    /// written under.
    bucket: String,
    created_at_secs: u64,
    /// Record CIDs in append order. CARv2's `MultihashIndexSorted` sorts
    /// by digest, so the index alone can't reconstruct append order;
    /// callers (e.g. compaction) walk records in append order to preserve
    /// at-rest ordering across rewrites.
    record_order: Vec<Cid>,
    /// Per-record opaque floor metadata, in the same positional order as
    /// `record_order` — i.e. `floor_metadata[i]` belongs to
    /// `record_order[i]`. A parallel `Vec` (rather than a map keyed on
    /// Cid) sidesteps `Cid`'s lack of `Ord` and keeps the encoding
    /// trivially deterministic.
    floor_metadata: Vec<serde_bytes::ByteBuf>,
}

const SIDECAR_SCHEMA_VERSION: u16 = 1;

/// First frame of an open segment's `.tail` append journal: the header fields
/// the `.meta` would carry, so a rebuilt sidecar is the one `finalize()` would
/// have written. A journal outlives its binary only across a crash into the
/// next one, but it carries a version for the same additive-evolution reason
/// the sidecar does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TailHeader {
    format_version: u16,
    kind: String,
    #[serde(with = "serde_bytes")]
    actor_id: [u8; 32],
    segment_id: u32,
    bucket: String,
    created_at_secs: u64,
}

/// One appended record's journal frame — what the sidecar keeps per record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TailEntry {
    cid: Cid,
    floor: serde_bytes::ByteBuf,
}

const TAIL_SCHEMA_VERSION: u16 = 1;

/// What [`FramedSegment::recover_unfinalized`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailRecovery {
    /// The `.meta` exists: the segment was finalized and nothing was rebuilt
    /// (a journal left by a crash just after finalize is removed).
    AlreadyFinalized,
    /// The pair was rebuilt from the `.dat` and its journal and is now a
    /// finalized segment.
    Rebuilt {
        /// Records the rebuilt segment holds — every append that returned.
        records: u32,
        /// Blocks in the `.dat` with no journal entry: appends that failed or
        /// crashed before returning, so no caller ever acknowledged them.
        unjournaled_blocks_dropped: u32,
        /// Journal entries with no block in the `.dat`. The append protocol
        /// makes a block durable before its entry, so this is `0` unless the
        /// disk lost acknowledged writes — a LOSS, not a dropped retry.
        journaled_blocks_missing: u32,
    },
    /// No journal beside the `.dat`, so the per-record floor metadata is gone
    /// and the pair cannot be rebuilt; nothing was touched. `blocks` is how
    /// many complete blocks the `.dat` holds.
    NoJournal { blocks: u32 },
}

/// The `min_reader_format_version` this binary stamps into new sidecars: the
/// oldest binary `format_version` that can still read what we write today.
/// Bumped **only** on a breaking, non-additive sidecar change (deferred to a
/// major version per I3); an additive bump of `SIDECAR_SCHEMA_VERSION` leaves
/// this at the prior floor so older binaries keep reading. Mirrors the DB's
/// `MIN_READER_SCHEMA_VERSION` and `MANIFEST_MIN_READER_VERSION`.
const SIDECAR_MIN_READER_VERSION: u16 = 1;

/// In-memory header fields callers read off `FramedSegment.header` (kind,
/// actor_id, segment_id, bucket, created_at_secs, record_count). Mirrors
/// the surface the pre-CARv2 `SegmentHeader` exposed so consumers don't
/// have to relearn the shape; the fixed-width fields (`record_count`)
/// are filled from the carv2 reader after open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentHeader {
    pub kind: String,
    pub actor_id: [u8; 32],
    pub segment_id: u32,
    pub bucket: String,
    pub created_at_secs: u64,
    pub record_count: u32,
}

/// One segment file (the `.dat`) + its `.meta` companion. Created in
/// append mode via [`FramedSegment::create`]; transitions to read-only
/// after [`FramedSegment::finalize`].
pub struct FramedSegment {
    path: PathBuf,
    pub header: SegmentHeader,
    state: SegmentState,
}

impl std::fmt::Debug for FramedSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramedSegment")
            .field("path", &self.path)
            .field("header", &self.header)
            .finish_non_exhaustive()
    }
}

enum SegmentState {
    Open {
        writer: fauna_carv2::Writer<File>,
        /// Per-record floor metadata accumulated in append order; index
        /// matches `record_order`. Moved into the sidecar at `finalize()`.
        floor_metadata: Vec<Vec<u8>>,
        /// Record CIDs in append order; serialized into the sidecar so
        /// `iter_records()` can preserve append order (CARv2's index is
        /// digest-sorted, not append-sorted).
        record_order: Vec<Cid>,
        /// Per-record payload byte-length, keyed by CID. Doubles as the
        /// duplicate-cid guard (two appends with the same CID would
        /// silently overwrite the carv2 block at the same digest offset —
        /// CARv2 indexes by digest — corrupting the segment) and as the
        /// size source for [`FramedSegment::open_record_block_lens`], so
        /// quota/SEARCH sizing never has to finalize the open segment
        /// just to read the CARv2 index.
        record_lens: std::collections::HashMap<Cid, u64>,
        /// The `.tail` append journal (§ Crash recovery). `None` only for the
        /// private rebuild [`FramedSegment::recover_unfinalized`] writes,
        /// whose durability is finalize's alone.
        journal: Option<File>,
    },
    Finalized {
        /// Records in append order — preserves the at-rest sequencing
        /// `iter_records` consumers expect.
        records: Vec<RecordEntry>,
    },
    /// Entered when `finalize()` partially completes and then hits an I/O
    /// error. The segment is unrecoverable: reading or writing it would
    /// produce corrupt results. All methods return `InvalidSegment` on this
    /// state (except `iter_records`, which returns an empty iterator).
    Poisoned,
}

impl FramedSegment {
    /// Create a new segment file (`.dat`) and its `.tail` append journal, and
    /// seed in-memory state for the companion sidecar; the `.meta` file is
    /// only written at `finalize()`.
    pub fn create(path: &Path, header: SegmentHeader) -> Result<Self, SegmentStoreError> {
        Self::create_inner(path, header, true)
    }

    fn create_inner(
        path: &Path,
        header: SegmentHeader,
        journaled: bool,
    ) -> Result<Self, SegmentStoreError> {
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)?;
        let writer = fauna_carv2::Writer::new(file, &[])
            .map_err(|e| SegmentStoreError::Encoding(format!("carv2 writer: {e}")))?;
        let journal = if journaled {
            writer.get_ref().sync_data()?;
            let mut journal = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(tail_path_for(path))?;
            write_frame(
                &mut journal,
                &TailHeader {
                    format_version: TAIL_SCHEMA_VERSION,
                    kind: header.kind.clone(),
                    actor_id: header.actor_id,
                    segment_id: header.segment_id,
                    bucket: header.bucket.clone(),
                    created_at_secs: header.created_at_secs,
                },
            )?;
            journal.sync_data()?;
            Some(journal)
        } else {
            None
        };
        Ok(Self {
            path: path.to_path_buf(),
            header,
            state: SegmentState::Open {
                writer,
                floor_metadata: Vec::new(),
                record_order: Vec::new(),
                record_lens: std::collections::HashMap::new(),
                journal,
            },
        })
    }

    /// Append a record. The CID is the consumer-supplied content address
    /// of `record_bytes`; the carv2 writer streams `(cid, bytes)` to the
    /// data section. Duplicate CIDs error (CARv2 indexes by digest, so a
    /// duplicate would silently overwrite the earlier block).
    ///
    /// Returns only once the record is durable: the block is synced to the
    /// `.dat`, then its journal entry to the `.tail` (§ Crash recovery). An
    /// I/O failure on either poisons the segment — the writer's index and the
    /// in-memory record order would otherwise drift apart — and the next
    /// process's recovery drops the half-written record.
    pub fn append_record(
        &mut self,
        cid: Cid,
        record_bytes: &[u8],
        floor_metadata: &[u8],
    ) -> Result<(), SegmentStoreError> {
        let SegmentState::Open {
            writer,
            floor_metadata: floor_list,
            record_order,
            record_lens,
            journal,
        } = &mut self.state
        else {
            let msg = match self.state {
                SegmentState::Poisoned => "append on poisoned segment",
                _ => "append on finalized segment",
            };
            return Err(SegmentStoreError::InvalidSegment(msg.into()));
        };
        if record_lens.contains_key(&cid) {
            return Err(SegmentStoreError::InvalidSegment(format!(
                "duplicate record cid within segment: {cid}"
            )));
        }
        let durable = (|| -> Result<(), SegmentStoreError> {
            writer
                .write_block(&cid, record_bytes)
                .map_err(|e| SegmentStoreError::Encoding(format!("carv2 write_block: {e}")))?;
            if let Some(journal) = journal {
                writer.get_ref().sync_data()?;
                write_frame(
                    journal,
                    &TailEntry {
                        cid,
                        floor: serde_bytes::ByteBuf::from(floor_metadata.to_vec()),
                    },
                )?;
                journal.sync_data()?;
            }
            Ok(())
        })();
        if let Err(e) = durable {
            self.state = SegmentState::Poisoned;
            return Err(e);
        }
        record_lens.insert(cid, record_bytes.len() as u64);
        record_order.push(cid);
        floor_list.push(floor_metadata.to_vec());
        Ok(())
    }

    /// Finalize: close the carv2 writer (which writes the index + backfills
    /// the header) and persist the companion sidecar file.
    ///
    /// If any I/O step fails after the state has been taken out of `Open`,
    /// the segment transitions to `Poisoned` and all subsequent operations
    /// return `InvalidSegment("segment poisoned by failed finalize")`.
    pub fn finalize(&mut self) -> Result<(), SegmentStoreError> {
        // Take ownership of the current state. Poisoned is the placeholder
        // so an early I/O return leaves the segment in a safe but unusable
        // state rather than silently behaving like a 0-record Finalized.
        let current = std::mem::replace(&mut self.state, SegmentState::Poisoned);
        let SegmentState::Open {
            writer,
            floor_metadata,
            record_order,
            journal,
            ..
        } = current
        else {
            self.state = current;
            let msg = match &self.state {
                SegmentState::Finalized { .. } => "finalize on already-finalized segment",
                SegmentState::Poisoned => "finalize on poisoned segment",
                SegmentState::Open { .. } => unreachable!("just destructured as not-Open"),
            };
            return Err(SegmentStoreError::InvalidSegment(msg.into()));
        };
        // From here forward any error leaves self.state = Poisoned. The journal
        // handle closes first: Windows refuses to unlink an open file.
        drop(journal);
        let file = writer
            .finalize()
            .map_err(|e| SegmentStoreError::Encoding(format!("carv2 finalize: {e}")))?;
        file.sync_all()?;

        // Build + write the sidecar.
        debug_assert_eq!(
            record_order.len(),
            floor_metadata.len(),
            "record_order and floor_metadata must stay parallel; append_record \
             pushes both in lockstep"
        );
        let floor_for_cbor: Vec<serde_bytes::ByteBuf> = floor_metadata
            .iter()
            .map(|bytes| serde_bytes::ByteBuf::from(bytes.clone()))
            .collect();
        let sidecar = SegmentSidecar {
            format_version: SIDECAR_SCHEMA_VERSION,
            min_reader_format_version: SIDECAR_MIN_READER_VERSION,
            kind: self.header.kind.clone(),
            actor_id: self.header.actor_id,
            segment_id: self.header.segment_id,
            bucket: self.header.bucket.clone(),
            created_at_secs: self.header.created_at_secs,
            record_order: record_order.clone(),
            floor_metadata: floor_for_cbor,
        };
        let sidecar_bytes = fauna_cbor::encode_canonical(&sidecar)
            .map_err(|e| SegmentStoreError::Encoding(format!("encode sidecar: {e}")))?;
        // Write + fsync the sidecar through a single *write* handle so the
        // `.dat` + `.meta` pair survives a crash. The fsync MUST go through the
        // write handle: `File::open(..).sync_all()` opens read-only, and
        // `FlushFileBuffers` on a read-only handle is `ERROR_ACCESS_DENIED`
        // (os error 5) on Windows (it succeeds on Unix). Same Windows-fsync
        // hazard the `manifest.rs` parent-dir fsync hit after the 2026-06-15
        // posts segment cutover — there the fix was `#[cfg(unix)]`; here the
        // sidecar fsync is load-bearing on every platform, so we keep it and
        // route it through a writable handle instead.
        let meta_path = meta_path_for(&self.path);
        let mut meta_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&meta_path)?;
        meta_file.write_all(&sidecar_bytes)?;
        meta_file.sync_all()?;
        // The pair is durable; the journal has nothing left to recover.
        remove_if_exists(&tail_path_for(&self.path))?;

        self.header.record_count = record_order.len() as u32;
        let records: Vec<RecordEntry> = record_order
            .into_iter()
            .zip(floor_metadata)
            .map(|(cid, floor)| RecordEntry {
                cid,
                floor_metadata: floor,
            })
            .collect();
        self.state = SegmentState::Finalized { records };
        Ok(())
    }

    /// Open a finalized segment for reading. Errors if the `.dat` is
    /// corrupt, the `.meta` sidecar is missing/corrupt, or the segment
    /// was never finalized.
    pub fn open(path: &Path) -> Result<Self, SegmentStoreError> {
        // Read the sidecar first; it carries the human-meaningful header.
        let meta_path = meta_path_for(path);
        let meta_bytes = std::fs::read(&meta_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SegmentStoreError::InvalidSegment(format!(
                    "missing sidecar at {} (segment not finalized?)",
                    meta_path.display()
                ))
            } else {
                SegmentStoreError::Io(e)
            }
        })?;
        let sidecar: SegmentSidecar = fauna_cbor::decode_strict(&meta_bytes)
            .map_err(|e| SegmentStoreError::Encoding(format!("decode sidecar: {e}")))?;
        if let crate::version::FormatVerdict::IncompatibleNewer {
            file_v,
            file_min,
            bin_v,
        } = crate::version::check_format_compatibility(
            sidecar.format_version,
            sidecar.min_reader_format_version,
            SIDECAR_SCHEMA_VERSION,
        ) {
            return Err(SegmentStoreError::SchemaMismatch(format!(
                "sidecar format_version {file_v} requires a reader at format_version >= \
                 {file_min}, but this binary writes format_version {bin_v} — this nest binary \
                 must be updated"
            )));
        }
        // Probe the `.dat` to ensure it parses; the reader is dropped after
        // open() — read_record reopens its own File handle, matching the
        // pre-CARv2 behaviour.
        let file = File::open(path)?;
        let reader = fauna_carv2::Reader::new(file)
            .map_err(|e| SegmentStoreError::Encoding(format!("open carv2: {e}")))?;
        let record_count = sidecar.record_order.len() as u32;
        // Sanity-check the carv2 block count matches the sidecar's append
        // log (any mismatch indicates the .dat and .meta drifted out of sync,
        // probably a crash between the two writes).
        if reader.len() != sidecar.record_order.len() {
            return Err(SegmentStoreError::InvalidSegment(format!(
                "carv2 block count {} differs from sidecar record_order len {} (crash mid-finalize?)",
                reader.len(),
                sidecar.record_order.len()
            )));
        }
        if sidecar.floor_metadata.len() != sidecar.record_order.len() {
            return Err(SegmentStoreError::InvalidSegment(format!(
                "sidecar floor_metadata len {} != record_order len {} (corrupt sidecar)",
                sidecar.floor_metadata.len(),
                sidecar.record_order.len()
            )));
        }
        let header = SegmentHeader {
            kind: sidecar.kind.clone(),
            actor_id: sidecar.actor_id,
            segment_id: sidecar.segment_id,
            bucket: sidecar.bucket.clone(),
            created_at_secs: sidecar.created_at_secs,
            record_count,
        };
        let records: Vec<RecordEntry> = sidecar
            .record_order
            .iter()
            .zip(sidecar.floor_metadata.iter())
            .map(|(cid, floor)| RecordEntry {
                cid: *cid,
                floor_metadata: floor.to_vec(),
            })
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            header,
            state: SegmentState::Finalized { records },
        })
    }

    /// Rebuild a segment a crash left open — a `.dat` with no `.meta` — into a
    /// finalized pair, from the `.dat`'s data section and the `.tail` append
    /// journal (§ Crash recovery). The caller guarantees no writer holds the
    /// segment open (the scope's first touch in a process).
    ///
    /// The `.dat`'s blocks are paired with the journal's entries in order;
    /// the first block without its entry ends the pairing, and it and every
    /// block after it are appends that never returned. The rebuild is written
    /// beside the segment and renamed over it, `.dat` first, so a crash at any
    /// point leaves either the old open shape or a finalized `.dat` the journal
    /// still describes — a re-run rebuilds from either.
    pub fn recover_unfinalized(path: &Path) -> Result<TailRecovery, SegmentStoreError> {
        let meta_path = meta_path_for(path);
        let tail_path = tail_path_for(path);
        if meta_path.exists() {
            remove_if_exists(&tail_path)?;
            return Ok(TailRecovery::AlreadyFinalized);
        }
        let journal = match std::fs::read(&tail_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let scan = fauna_carv2::scan_data_section(File::open(path)?, |_, _| {
                    Ok(ControlFlow::Continue(()))
                })
                .map_err(|e| SegmentStoreError::Encoding(format!("scan carv2: {e}")))?;
                return Ok(TailRecovery::NoJournal {
                    blocks: scan.blocks as u32,
                });
            }
            Err(e) => return Err(e.into()),
        };
        let (tail_header, entries) = parse_journal(&journal, &tail_path)?;

        let rebuild_path = path.with_extension("dat.rebuild");
        let rebuild_meta = meta_path_for(&rebuild_path);
        remove_if_exists(&rebuild_path)?;
        remove_if_exists(&rebuild_meta)?;
        let header = SegmentHeader {
            kind: tail_header.kind,
            actor_id: tail_header.actor_id,
            segment_id: tail_header.segment_id,
            bucket: tail_header.bucket,
            created_at_secs: tail_header.created_at_secs,
            record_count: 0,
        };
        let mut rebuilt = Self::create_inner(&rebuild_path, header, false)?;
        let mut kept = 0usize;
        let mut unjournaled = 0usize;
        let mut append_err = None;
        let scanned = fauna_carv2::scan_data_section(File::open(path)?, |cid, block| {
            match entries.get(kept) {
                Some(entry) if unjournaled == 0 && entry.cid == cid => {
                    if let Err(e) = rebuilt.append_record(cid, &block, &entry.floor) {
                        append_err = Some(e);
                        return Ok(ControlFlow::Break(()));
                    }
                    kept += 1;
                }
                _ => unjournaled += 1,
            }
            Ok(ControlFlow::Continue(()))
        });
        if let Some(e) = append_err {
            return Err(e);
        }
        match scanned {
            Ok(_) => {}
            // A `.dat` whose very prefix never reached the disk holds nothing;
            // with no journaled entry, nothing was acknowledged either.
            Err(_) if entries.is_empty() => {}
            Err(e) => return Err(SegmentStoreError::Encoding(format!("scan carv2: {e}"))),
        }
        rebuilt.finalize()?;
        drop(rebuilt);
        std::fs::rename(&rebuild_path, path)?;
        std::fs::rename(&rebuild_meta, &meta_path)?;
        remove_if_exists(&tail_path)?;
        Ok(TailRecovery::Rebuilt {
            records: kept as u32,
            unjournaled_blocks_dropped: unjournaled as u32,
            journaled_blocks_missing: (entries.len() - kept) as u32,
        })
    }

    /// Look up a record by CID; returns the record bytes the consumer
    /// passed to `append_record`. `Ok(None)` for unknown CIDs (not an
    /// error — matches the new contract).
    pub fn read_record(&self, cid: &Cid) -> Result<Option<Vec<u8>>, SegmentStoreError> {
        if !matches!(self.state, SegmentState::Finalized { .. }) {
            return Err(SegmentStoreError::InvalidSegment(
                "read on unfinalized or poisoned segment".into(),
            ));
        }
        let file = File::open(&self.path)?;
        let mut reader = fauna_carv2::Reader::new(file)
            .map_err(|e| SegmentStoreError::Encoding(format!("open carv2: {e}")))?;
        match reader.get(cid) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(fauna_carv2::Error::CidNotFound) => Ok(None),
            Err(e) => Err(SegmentStoreError::Encoding(format!("carv2 get: {e}"))),
        }
    }

    /// Look up a record's payload byte-length by CID **without reading the
    /// body**. Returns `Ok(None)` for unknown CIDs (matching `read_record`).
    ///
    /// This is the size source the IMAP RFC822.SIZE / SEARCH `LARGER`|`SMALLER`
    /// / STORAGE-quota path uses (`imap-server.md` §§ SEARCH, QUOTA) once the
    /// interim `segment_records.byte_length` mirror column is retired: the
    /// sealed record's ciphertext byte length, derived from the CARv2
    /// `MultihashIndexSorted` index by Cid rather than mirrored in SQLite. The
    /// returned value equals the bytes passed to `append_record`.
    pub fn record_block_len(&self, cid: &Cid) -> Result<Option<u64>, SegmentStoreError> {
        if !matches!(self.state, SegmentState::Finalized { .. }) {
            return Err(SegmentStoreError::InvalidSegment(
                "size lookup on unfinalized or poisoned segment".into(),
            ));
        }
        let file = File::open(&self.path)?;
        let mut reader = fauna_carv2::Reader::new(file)
            .map_err(|e| SegmentStoreError::Encoding(format!("open carv2: {e}")))?;
        match reader.block_len(cid) {
            Ok(len) => Ok(Some(len)),
            Err(fauna_carv2::Error::CidNotFound) => Ok(None),
            Err(e) => Err(SegmentStoreError::Encoding(format!("carv2 block_len: {e}"))),
        }
    }

    /// Look up many records' payload byte-lengths efficiently with a single
    /// file handle + in-memory index reused across lookups. Returns lengths in
    /// the same order as `cids`, with `Ok(None)` for any CID absent from the
    /// segment. The bulk form for the STORAGE-quota SUM / SEARCH-size walks
    /// (`imap-server.md` §§ SEARCH, QUOTA), which size every record in a
    /// segment — amortizing the carv2 index parse over the whole batch instead
    /// of reopening per record. None of the block bodies are read.
    pub fn record_block_lens(&self, cids: &[&Cid]) -> Result<Vec<Option<u64>>, SegmentStoreError> {
        if !matches!(self.state, SegmentState::Finalized { .. }) {
            return Err(SegmentStoreError::InvalidSegment(
                "size lookup on unfinalized or poisoned segment".into(),
            ));
        }
        let file = File::open(&self.path)?;
        let mut reader = fauna_carv2::Reader::new(file)
            .map_err(|e| SegmentStoreError::Encoding(format!("open carv2: {e}")))?;
        let mut out = Vec::with_capacity(cids.len());
        for cid in cids {
            match reader.block_len(cid) {
                Ok(len) => out.push(Some(len)),
                Err(fauna_carv2::Error::CidNotFound) => out.push(None),
                Err(e) => {
                    return Err(SegmentStoreError::Encoding(format!("carv2 block_len: {e}")));
                }
            }
        }
        Ok(out)
    }

    /// Size lookups against the OPEN segment, served from the in-memory
    /// per-record length map — `None` when the segment is not open (use
    /// [`Self::record_block_lens`] for a finalized one). Sizing (STORAGE
    /// quota / SEARCH `LARGER`/`SMALLER` / RFC822.SIZE) runs on hot paths —
    /// the quota pre-check fires on every inbound delivery — so it must
    /// never force-finalize the open segment just to materialize the CARv2
    /// index: that both defeated the monthly bucket rotation (every
    /// quota-checked ingest closed the open segment → one segment per
    /// message) and silently ate the `Finalized` push the rotation-aware
    /// append emits (`message-segment-store.md` § segment_records mirror —
    /// "segment count is bounded (monthly bucket rotation + compaction)").
    pub fn open_record_block_lens(&self, cids: &[&Cid]) -> Option<Vec<Option<u64>>> {
        let SegmentState::Open { record_lens, .. } = &self.state else {
            return None;
        };
        Some(
            cids.iter()
                .map(|cid| record_lens.get(cid).copied())
                .collect(),
        )
    }

    /// Read multiple records by CID efficiently with a single file handle
    /// reused across lookups. Returns payload bytes in the same order as
    /// `entries`, with `Ok(None)` for any CID absent from the segment.
    pub fn read_records_bulk(
        &self,
        entries: &[&RecordEntry],
    ) -> Result<Vec<Option<Vec<u8>>>, SegmentStoreError> {
        if !matches!(self.state, SegmentState::Finalized { .. }) {
            return Err(SegmentStoreError::InvalidSegment(
                "read on unfinalized or poisoned segment".into(),
            ));
        }
        let file = File::open(&self.path)?;
        let mut reader = fauna_carv2::Reader::new(file)
            .map_err(|e| SegmentStoreError::Encoding(format!("open carv2: {e}")))?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            match reader.get(&entry.cid) {
                Ok(bytes) => out.push(Some(bytes)),
                Err(fauna_carv2::Error::CidNotFound) => out.push(None),
                Err(e) => return Err(SegmentStoreError::Encoding(format!("carv2 get: {e}"))),
            }
        }
        Ok(out)
    }

    /// Walk records in append order. Returns nothing for Open / Poisoned
    /// segments.
    pub fn iter_records(&self) -> impl Iterator<Item = &RecordEntry> {
        let records: &[RecordEntry] = match &self.state {
            SegmentState::Finalized { records } => records,
            // Open: records haven't been finalized into the index yet;
            // matching the pre-CARv2 semantics of returning the in-progress
            // append log would require a parallel mutable copy here. The
            // simpler answer (the only consumer of iter_records is
            // post-finalize work in compaction/manager): yield nothing.
            SegmentState::Open { .. } => &[],
            // Poisoned: the segment is unrecoverable; yield nothing.
            SegmentState::Poisoned => &[],
        };
        records.iter()
    }

    /// On-disk path of the `.dat` file. Exposed so backup / restore
    /// code (Plan 5) can stream the framed file without re-deriving the
    /// `seg-NNNNNNNN.dat` path convention.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Path to the companion `.meta` sidecar. Useful for backup paths
    /// that need to copy both files.
    pub fn meta_path(&self) -> PathBuf {
        meta_path_for(&self.path)
    }

    /// Size of the segment `.dat` on disk in bytes. (The sidecar is not
    /// included — Plan 5 advertises `.dat` size for the chunked-download
    /// integrity hash; sidecar size is tracked separately if needed.)
    pub fn size_bytes(&self) -> Result<u64, SegmentStoreError> {
        Ok(std::fs::metadata(&self.path)?.len())
    }

    /// BLAKE3 hash of the `.dat` file. Streamed so it does not allocate
    /// the whole file. Used by Plan 5's `fauna.segments.list` reply to
    /// advertise an integrity hash the destination can verify after a
    /// chunked download.
    pub fn file_blake3(&self) -> Result<[u8; 32], SegmentStoreError> {
        // The read loop is `fauna-core`'s (`chunker_stream::blake3_of_reader`),
        // which is where the streamed file-hash has claimed to live since the
        // three-copy consolidation — this was the fourth copy. The open stays
        // here so its `io::Error` still becomes `SegmentStoreError::Io`.
        Ok(fauna_core::chunker_stream::blake3_of_reader(File::open(
            &self.path,
        )?)?)
    }

    /// BLAKE3 of the `.meta` sidecar — the pair's other half, advertised
    /// beside [`Self::file_blake3`] so a backup destination can verify the
    /// sidecar it received exactly as it verifies the `.dat`. Only meaningful
    /// on a finalized segment (the sidecar is written at `finalize()`); on an
    /// open one the read fails with the sidecar's `NotFound`.
    pub fn meta_blake3(&self) -> Result<[u8; 32], SegmentStoreError> {
        Ok(fauna_core::chunker_stream::blake3_of_reader(File::open(
            self.meta_path(),
        )?)?)
    }
}

/// Derive the `.meta` sidecar path from a segment `.dat` path. Always a
/// sibling file with the `.meta` extension swapped in (e.g.
/// `seg-00000007.dat` → `seg-00000007.meta`).
pub(crate) fn meta_path_for(dat_path: &Path) -> PathBuf {
    dat_path.with_extension("meta")
}

/// Derive the `.tail` append-journal path from a segment `.dat` path — the
/// third sibling, present only while the segment is open (§ Crash recovery).
pub(crate) fn tail_path_for(dat_path: &Path) -> PathBuf {
    dat_path.with_extension("tail")
}

/// `remove_file`, with an already-absent file counted as removed.
pub(crate) fn remove_if_exists(path: &Path) -> Result<(), SegmentStoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Append one journal frame: `varint(len) || canonical dag-cbor`, in one write.
fn write_frame<T: Serialize>(w: &mut File, value: &T) -> Result<(), SegmentStoreError> {
    let bytes = fauna_cbor::encode_canonical(value)
        .map_err(|e| SegmentStoreError::Encoding(format!("encode journal frame: {e}")))?;
    let mut frame = Vec::with_capacity(bytes.len() + 10);
    fauna_carv2::v1::write_varint_usize(&mut frame, bytes.len())
        .map_err(|e| SegmentStoreError::Encoding(format!("journal frame length: {e}")))?;
    frame.extend_from_slice(&bytes);
    w.write_all(&frame)?;
    Ok(())
}

/// Parse a `.tail` journal: its header, then every entry up to the first frame
/// that does not decode in full — a torn final frame is an append that never
/// returned. An unreadable HEADER is an error: `create` makes it durable
/// before the segment can take an append.
fn parse_journal(
    bytes: &[u8],
    tail_path: &Path,
) -> Result<(TailHeader, Vec<TailEntry>), SegmentStoreError> {
    let mut cursor = std::io::Cursor::new(bytes);
    let header: TailHeader = read_frame(&mut cursor).ok_or_else(|| {
        SegmentStoreError::InvalidSegment(format!(
            "append journal at {} has no readable header",
            tail_path.display()
        ))
    })?;
    if header.format_version > TAIL_SCHEMA_VERSION {
        return Err(SegmentStoreError::SchemaMismatch(format!(
            "append journal format_version {} is newer than this binary's {TAIL_SCHEMA_VERSION} \
             — this nest binary must be updated",
            header.format_version
        )));
    }
    let mut entries = Vec::new();
    while let Some(entry) = read_frame::<TailEntry>(&mut cursor) {
        entries.push(entry);
    }
    Ok((header, entries))
}

/// The next whole frame, or `None` at the end or at a torn/undecodable frame.
fn read_frame<T: serde::de::DeserializeOwned>(cursor: &mut std::io::Cursor<&[u8]>) -> Option<T> {
    let len = fauna_carv2::v1::read_varint_usize(cursor).ok()?;
    let start = cursor.position() as usize;
    let body = cursor.get_ref().get(start..start.checked_add(len)?)?;
    let value = fauna_cbor::decode_strict(body).ok()?;
    cursor.set_position((start + len) as u64);
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn header(seg_id: u32) -> SegmentHeader {
        SegmentHeader {
            kind: "mail".to_string(),
            actor_id: [7u8; 32],
            segment_id: seg_id,
            bucket: "2026-05".to_string(),
            created_at_secs: 1_715_000_000,
            record_count: 0,
        }
    }

    #[test]
    fn create_append_finalize_open_roundtrip() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");

        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body_a = b"hello world";
        let body_b = b"goodbye world";
        let cid_a = Cid::of_dag_cbor(body_a);
        let cid_b = Cid::of_dag_cbor(body_b);
        seg.append_record(cid_a, body_a, b"{\"t\":1}")
            .expect("append a");
        seg.append_record(cid_b, body_b, b"{\"t\":2}")
            .expect("append b");
        seg.finalize().expect("finalize");

        let seg = FramedSegment::open(&path).expect("open");
        assert_eq!(seg.header.record_count, 2);
        let payload = seg.read_record(&cid_a).expect("read msg-1");
        assert_eq!(payload.as_deref(), Some(body_a.as_slice()));
        let payload2 = seg.read_record(&cid_b).expect("read msg-2");
        assert_eq!(payload2.as_deref(), Some(body_b.as_slice()));

        // Unknown CID returns Ok(None) per the new contract.
        let missing = Cid::of_dag_cbor(b"never-appended");
        let res = seg.read_record(&missing).expect("must Ok(None) on unknown");
        assert!(res.is_none());
    }

    #[test]
    fn record_block_len_returns_payload_length() {
        // The dropped `segment_records.byte_length` mirror column was
        // replaced by this index lookup: a record's size (IMAP RFC822.SIZE /
        // SEARCH LARGER|SMALLER / STORAGE quota) is the CARv2 block length
        // looked up by Cid, not a SQL column. It must equal the bytes that
        // were appended, and be derivable without reading the body.
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");

        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body_a = b"hello world";
        let body_b = b"a much longer body of sealed envelope bytes for record b";
        let cid_a = Cid::of_dag_cbor(body_a);
        let cid_b = Cid::of_dag_cbor(body_b);
        seg.append_record(cid_a, body_a, b"{\"t\":1}")
            .expect("append a");
        seg.append_record(cid_b, body_b, b"{\"t\":2}")
            .expect("append b");
        seg.finalize().expect("finalize");

        let seg = FramedSegment::open(&path).expect("open");
        assert_eq!(
            seg.record_block_len(&cid_a).expect("len a"),
            Some(body_a.len() as u64)
        );
        assert_eq!(
            seg.record_block_len(&cid_b).expect("len b"),
            Some(body_b.len() as u64)
        );

        // Unknown CID returns Ok(None), mirroring read_record's contract.
        let missing = Cid::of_dag_cbor(b"never-appended");
        assert_eq!(seg.record_block_len(&missing).expect("missing"), None);
    }

    #[test]
    fn record_block_lens_bulk_returns_sizes_in_order() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");

        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body_a = b"aa";
        let body_b = b"bbbbbbbb";
        let body_c = b"ccc";
        let cid_a = Cid::of_dag_cbor(body_a);
        let cid_b = Cid::of_dag_cbor(body_b);
        let cid_c = Cid::of_dag_cbor(body_c);
        seg.append_record(cid_a, body_a, b"").expect("append a");
        seg.append_record(cid_b, body_b, b"").expect("append b");
        seg.append_record(cid_c, body_c, b"").expect("append c");
        seg.finalize().expect("finalize");

        let seg = FramedSegment::open(&path).expect("open");
        let missing = Cid::of_dag_cbor(b"absent");
        // Query order differs from append order; result must follow the query.
        let lens = seg
            .record_block_lens(&[&cid_c, &cid_a, &missing, &cid_b])
            .expect("bulk lens");
        assert_eq!(
            lens,
            vec![
                Some(body_c.len() as u64),
                Some(body_a.len() as u64),
                None,
                Some(body_b.len() as u64),
            ]
        );
    }

    #[test]
    fn record_block_len_errors_on_open() {
        // Mirror read_records_bulk_errors_on_open: a size lookup on an
        // unfinalized segment is a programming error, not Ok(None).
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body = b"x";
        let cid = Cid::of_dag_cbor(body);
        seg.append_record(cid, body, b"").expect("append");
        let err = seg.record_block_len(&cid).expect_err("must error on Open");
        match err {
            crate::SegmentStoreError::InvalidSegment(_) => {}
            other => panic!("expected InvalidSegment, got {other:?}"),
        }
    }

    #[test]
    fn open_unfinalized_errors() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body = b"x";
        seg.append_record(Cid::of_dag_cbor(body), body, b"")
            .expect("append");
        // Drop without finalize.
        drop(seg);
        let err = FramedSegment::open(&path).expect_err("must error on unfinalized");
        match err {
            crate::SegmentStoreError::InvalidSegment(_) => {}
            other => panic!("expected InvalidSegment, got {other:?}"),
        }
    }

    /// Append `n` records (floor `floor-i`) and drop the segment without
    /// finalizing — what a crash leaves. Returns the bodies appended.
    fn crash_with_open_segment(path: &Path, n: u8) -> Vec<Vec<u8>> {
        let mut seg = FramedSegment::create(path, header(3)).expect("create");
        let bodies: Vec<Vec<u8>> = (0..n).map(|i| vec![i; 8 + i as usize]).collect();
        for (i, body) in bodies.iter().enumerate() {
            seg.append_record(
                Cid::of_dag_cbor(body),
                body,
                format!("floor-{i}").as_bytes(),
            )
            .expect("append");
        }
        drop(seg);
        assert!(!meta_path_for(path).exists(), "a crash leaves no sidecar");
        assert!(
            tail_path_for(path).exists(),
            "an open segment keeps its journal"
        );
        bodies
    }

    /// The rebuilt pair is the one `finalize()` would have written: the same
    /// header, every record in append order with its floor metadata.
    fn assert_rebuilt(path: &Path, bodies: &[Vec<u8>]) {
        let seg = FramedSegment::open(path).expect("open the rebuilt pair");
        assert_eq!(
            seg.header,
            SegmentHeader {
                record_count: bodies.len() as u32,
                ..header(3)
            }
        );
        let records: Vec<RecordEntry> = seg.iter_records().cloned().collect();
        let want: Vec<RecordEntry> = bodies
            .iter()
            .enumerate()
            .map(|(i, body)| RecordEntry {
                cid: Cid::of_dag_cbor(body),
                floor_metadata: format!("floor-{i}").into_bytes(),
            })
            .collect();
        assert_eq!(records, want);
        for body in bodies {
            assert_eq!(
                seg.read_record(&Cid::of_dag_cbor(body)).expect("read"),
                Some(body.clone())
            );
        }
        assert!(
            !tail_path_for(path).exists(),
            "the journal goes with the rebuild"
        );
    }

    #[test]
    fn finalize_removes_the_append_journal() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        assert!(tail_path_for(&path).exists());
        seg.append_record(Cid::of_dag_cbor(b"x"), b"x", b"")
            .expect("append");
        seg.finalize().expect("finalize");
        assert!(!tail_path_for(&path).exists());
    }

    #[test]
    fn recover_unfinalized_rebuilds_the_pair_from_the_append_journal() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-3.dat");
        let bodies = crash_with_open_segment(&path, 3);
        assert_eq!(
            FramedSegment::recover_unfinalized(&path).expect("recover"),
            TailRecovery::Rebuilt {
                records: 3,
                unjournaled_blocks_dropped: 0,
                journaled_blocks_missing: 0,
            }
        );
        assert_rebuilt(&path, &bodies);
        assert_eq!(
            FramedSegment::recover_unfinalized(&path).expect("re-run"),
            TailRecovery::AlreadyFinalized
        );
    }

    #[test]
    fn recover_unfinalized_ignores_a_torn_trailing_block() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-3.dat");
        let bodies = crash_with_open_segment(&path, 2);
        let mut dat = OpenOptions::new().append(true).open(&path).expect("open");
        dat.write_all(&[0x40, 0x01, 0x71, 0x1e])
            .expect("torn record");
        drop(dat);
        assert_eq!(
            FramedSegment::recover_unfinalized(&path).expect("recover"),
            TailRecovery::Rebuilt {
                records: 2,
                unjournaled_blocks_dropped: 0,
                journaled_blocks_missing: 0,
            }
        );
        assert_rebuilt(&path, &bodies);
    }

    /// A block whose journal entry never landed is an append that never
    /// returned — no caller acknowledged it, so the rebuild drops it.
    #[test]
    fn recover_unfinalized_drops_a_block_whose_append_never_returned() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-3.dat");
        let journal_after_one = {
            let mut seg = FramedSegment::create(&path, header(3)).expect("create");
            let body = vec![0u8; 8];
            seg.append_record(Cid::of_dag_cbor(&body), &body, b"floor-0")
                .expect("append");
            let len = std::fs::metadata(tail_path_for(&path)).expect("stat").len();
            let body = vec![1u8; 9];
            seg.append_record(Cid::of_dag_cbor(&body), &body, b"floor-1")
                .expect("append");
            len
        };
        OpenOptions::new()
            .write(true)
            .open(tail_path_for(&path))
            .expect("open journal")
            .set_len(journal_after_one)
            .expect("cut the second entry");
        assert_eq!(
            FramedSegment::recover_unfinalized(&path).expect("recover"),
            TailRecovery::Rebuilt {
                records: 1,
                unjournaled_blocks_dropped: 1,
                journaled_blocks_missing: 0,
            }
        );
        assert_rebuilt(&path, &[vec![0u8; 8]]);
    }

    /// A crash between the rebuild's two renames leaves a FINALIZED `.dat`
    /// with no `.meta` and the journal still beside it; a re-run rebuilds
    /// from that shape too (the scan stops at the data section's end, never
    /// reading the index as a record).
    #[test]
    fn recover_unfinalized_reruns_over_a_rebuild_cut_between_its_renames() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-3.dat");
        let bodies = crash_with_open_segment(&path, 2);
        let journal = std::fs::read(tail_path_for(&path)).expect("read journal");
        FramedSegment::recover_unfinalized(&path).expect("recover");
        std::fs::remove_file(meta_path_for(&path)).expect("cut before the .meta rename");
        std::fs::write(tail_path_for(&path), journal).expect("journal still there");
        assert_eq!(
            FramedSegment::recover_unfinalized(&path).expect("re-run"),
            TailRecovery::Rebuilt {
                records: 2,
                unjournaled_blocks_dropped: 0,
                journaled_blocks_missing: 0,
            }
        );
        assert_rebuilt(&path, &bodies);
    }

    #[test]
    fn recover_unfinalized_without_a_journal_touches_nothing() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-3.dat");
        crash_with_open_segment(&path, 2);
        std::fs::remove_file(tail_path_for(&path)).expect("drop the journal");
        let before = std::fs::read(&path).expect("read");
        assert_eq!(
            FramedSegment::recover_unfinalized(&path).expect("recover"),
            TailRecovery::NoJournal { blocks: 2 }
        );
        assert_eq!(std::fs::read(&path).expect("read"), before);
        assert!(!meta_path_for(&path).exists());
    }

    /// **`is_unfinalized_crash_tail` is a real discriminator, not a constant
    /// function.** A replay's tail-skip
    /// (`bins/fauna-nest/src/segments/mod.rs::open_replay_segment`) spends
    /// this to decide whether the "only the unfinalized tail is lost" bound
    /// is true. `InvalidSegment` carries no structured discriminator, so the
    /// predicate matches on `open()`'s exact missing-sidecar message — this
    /// pins the two directions the earlier `open_unfinalized_errors` test
    /// leaves unchecked: the missing-sidecar shape reads true, and an
    /// unrelated `InvalidSegment` produced by a *different* method on a
    /// *finalized* segment (so genuinely not the crash tail) reads false.
    #[test]
    fn is_unfinalized_crash_tail_discriminates_the_missing_sidecar_shape() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body = b"x";
        seg.append_record(Cid::of_dag_cbor(body), body, b"")
            .expect("append");
        // Drop without finalize: the .dat exists, the .meta never gets
        // written — open()'s missing-sidecar path, exactly.
        drop(seg);
        let missing_sidecar = FramedSegment::open(&path).expect_err("must error on unfinalized");
        assert!(
            missing_sidecar.is_unfinalized_crash_tail(),
            "a missing .meta on the newest segment IS the shape the loss bound covers, \
             got {missing_sidecar:?}"
        );

        // A finalized (NOT unfinalized) segment: append_record on it is a
        // different InvalidSegment shape entirely — not the crash tail.
        let path2 = tmp.path().join("seg-2.dat");
        let mut seg2 = FramedSegment::create(&path2, header(2)).expect("create");
        let body2 = b"y";
        seg2.append_record(Cid::of_dag_cbor(body2), body2, b"")
            .expect("append");
        seg2.finalize().expect("finalize");
        let not_crash_tail = seg2
            .append_record(Cid::of_dag_cbor(b"z"), b"z", b"")
            .expect_err("must error on finalized segment");
        assert!(
            !not_crash_tail.is_unfinalized_crash_tail(),
            "a finalized segment's own InvalidSegment must NOT read as the crash tail, \
             got {not_crash_tail:?}"
        );
    }

    #[test]
    fn append_after_finalize_errors() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body = b"x";
        seg.append_record(Cid::of_dag_cbor(body), body, b"")
            .expect("append");
        seg.finalize().expect("finalize");
        let body2 = b"y";
        let err = seg
            .append_record(Cid::of_dag_cbor(body2), body2, b"")
            .expect_err("must error after finalize");
        match err {
            crate::SegmentStoreError::InvalidSegment(_) => {}
            other => panic!("expected InvalidSegment, got {other:?}"),
        }
    }

    #[test]
    fn iter_records_yields_in_append_order() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let mut expected_cids = Vec::new();
        for i in 0..5u32 {
            let body = format!("msg-{i}");
            let cid = Cid::of_dag_cbor(body.as_bytes());
            seg.append_record(cid, body.as_bytes(), b"")
                .expect("append");
            expected_cids.push(cid);
        }
        seg.finalize().expect("finalize");
        let seg = FramedSegment::open(&path).expect("open");
        let cids: Vec<Cid> = seg.iter_records().map(|r| r.cid).collect();
        assert_eq!(cids, expected_cids, "iter must preserve append order");
    }

    #[test]
    fn read_on_open_segment_errors() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body = b"x";
        seg.append_record(Cid::of_dag_cbor(body), body, b"")
            .expect("append");
        let err = seg
            .read_record(&Cid::of_dag_cbor(body))
            .expect_err("must error on Open state");
        match err {
            crate::SegmentStoreError::InvalidSegment(_) => {}
            other => panic!("expected InvalidSegment, got {other:?}"),
        }
    }

    #[test]
    fn append_duplicate_cid_errors() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body = b"x";
        let cid = Cid::of_dag_cbor(body);
        seg.append_record(cid, body, b"").expect("first");
        // Same cid (even with different body bytes) is rejected — the carv2
        // index keys on digest and would silently overwrite the prior block.
        let err = seg
            .append_record(cid, b"different bytes", b"")
            .expect_err("must error on duplicate cid");
        match err {
            crate::SegmentStoreError::InvalidSegment(_) => {}
            other => panic!("expected InvalidSegment, got {other:?}"),
        }
    }

    #[test]
    fn poisoned_state_methods_return_invalid_segment() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body = b"x";
        seg.append_record(Cid::of_dag_cbor(body), body, b"")
            .expect("append");
        // Poison it before finalizing.
        seg.state = SegmentState::Poisoned;

        let body2 = b"y";
        let err = seg
            .append_record(Cid::of_dag_cbor(body2), body2, b"")
            .expect_err("append on Poisoned");
        match err {
            crate::SegmentStoreError::InvalidSegment(_) => {}
            other => panic!("expected InvalidSegment from append, got {other:?}"),
        }

        let err = seg
            .read_record(&Cid::of_dag_cbor(body))
            .expect_err("read on Poisoned");
        match err {
            crate::SegmentStoreError::InvalidSegment(_) => {}
            other => panic!("expected InvalidSegment from read, got {other:?}"),
        }

        let count = seg.iter_records().count();
        assert_eq!(count, 0, "iter_records on Poisoned must yield nothing");

        let err = seg.finalize().expect_err("finalize on Poisoned");
        match err {
            crate::SegmentStoreError::InvalidSegment(_) => {}
            other => panic!("expected InvalidSegment from finalize, got {other:?}"),
        }
    }

    #[test]
    fn read_records_bulk_returns_in_order() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let bodies: [&[u8]; 3] = [b"alpha", b"beta", b"gamma"];
        let cids: Vec<Cid> = bodies.iter().map(|b| Cid::of_dag_cbor(b)).collect();
        for (cid, body) in cids.iter().zip(bodies.iter()) {
            seg.append_record(*cid, body, b"").expect("append");
        }
        seg.finalize().expect("finalize");
        let seg = FramedSegment::open(&path).expect("open");

        let entries: Vec<&RecordEntry> = seg.iter_records().collect();
        let payloads = seg.read_records_bulk(&entries).expect("bulk read");
        let got: Vec<Vec<u8>> = payloads.into_iter().map(|o| o.expect("present")).collect();
        assert_eq!(
            got,
            vec![b"alpha".to_vec(), b"beta".to_vec(), b"gamma".to_vec()]
        );
    }

    #[test]
    fn size_bytes_and_file_blake3_round_trip() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body_a = b"hello";
        let body_b = b"world";
        seg.append_record(Cid::of_dag_cbor(body_a), body_a, b"")
            .expect("a");
        seg.append_record(Cid::of_dag_cbor(body_b), body_b, b"")
            .expect("b");
        seg.finalize().expect("finalize");

        let size = seg.size_bytes().expect("size");
        let on_disk = std::fs::metadata(&path).expect("stat").len();
        assert_eq!(size, on_disk, "size_bytes matches the actual .dat length");

        let h = seg.file_blake3().expect("blake3");
        let mut expected = blake3::Hasher::new();
        expected.update(&std::fs::read(&path).expect("read"));
        let expected = *expected.finalize().as_bytes();
        assert_eq!(h, expected, "blake3 matches streamed-vs-slurped value");

        // …and agrees with `fauna-core`'s streamed file-hash, which is where the
        // read loop lives. The assertion above would still pass if this method
        // re-rolled its own loop (it did until 2026-08-23, with a different
        // buffer size — invisible here, because block size cannot change a
        // BLAKE3 digest). This one is what a re-split has to get past.
        assert_eq!(
            h,
            fauna_core::chunker_stream::content_hash_streaming(&path)
                .expect("core hash")
                .digest(),
            "file_blake3 must be fauna-core's streamed file-hash, not a twin of it"
        );
    }

    #[test]
    fn read_records_bulk_errors_on_open() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body = b"x";
        seg.append_record(Cid::of_dag_cbor(body), body, b"")
            .expect("a");
        let err = seg.read_records_bulk(&[]).expect_err("must error on Open");
        match err {
            crate::SegmentStoreError::InvalidSegment(_) => {}
            other => panic!("expected InvalidSegment, got {other:?}"),
        }
    }

    #[test]
    fn empty_segment_round_trip() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-empty.dat");
        let mut seg = FramedSegment::create(&path, header(99)).expect("create");
        seg.finalize().expect("finalize");
        let seg = FramedSegment::open(&path).expect("open");
        assert_eq!(seg.header.record_count, 0);
        assert_eq!(seg.iter_records().count(), 0);
    }

    #[test]
    fn floor_metadata_round_trips_per_record() {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-floor.dat");
        let mut seg = FramedSegment::create(&path, header(7)).expect("create");
        let body_a = b"body-a";
        let body_b = b"body-b";
        let cid_a = Cid::of_dag_cbor(body_a);
        let cid_b = Cid::of_dag_cbor(body_b);
        seg.append_record(cid_a, body_a, b"floor-a").expect("a");
        seg.append_record(cid_b, body_b, b"floor-b").expect("b");
        seg.finalize().expect("finalize");

        let seg = FramedSegment::open(&path).expect("open");
        let mut by_cid = std::collections::HashMap::new();
        for r in seg.iter_records() {
            by_cid.insert(r.cid, r.floor_metadata.clone());
        }
        assert_eq!(
            by_cid.get(&cid_a).map(|v| v.as_slice()),
            Some(b"floor-a".as_slice())
        );
        assert_eq!(
            by_cid.get(&cid_b).map(|v| v.as_slice()),
            Some(b"floor-b".as_slice())
        );
    }

    /// Build a finalized one-record segment and return (tmpdir, .dat path).
    fn finalized_segment() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("seg-1.dat");
        let mut seg = FramedSegment::create(&path, header(1)).expect("create");
        let body = b"a sealed record body";
        let cid = Cid::of_dag_cbor(body);
        seg.append_record(cid, body, b"floor").expect("append");
        seg.finalize().expect("finalize");
        (tmp, path)
    }

    #[test]
    fn tolerates_newer_additive_sidecar_on_open() {
        // A newer binary wrote an additively-grown sidecar (format_version 99,
        // min_reader_format_version still 1 ≤ our 1): an older binary must read
        // it (I2 backward-compat). Rewrite only the version pair; the
        // record_order / floor_metadata stay consistent with the .dat.
        let (_tmp, path) = finalized_segment();
        let meta_path = meta_path_for(&path);
        let mut sc: SegmentSidecar =
            fauna_cbor::decode_strict(&std::fs::read(&meta_path).unwrap()).unwrap();
        sc.format_version = 99;
        sc.min_reader_format_version = 1;
        std::fs::write(&meta_path, fauna_cbor::encode_canonical(&sc).unwrap()).unwrap();

        let seg = FramedSegment::open(&path).expect("newer-additive sidecar must open");
        assert_eq!(seg.header.record_count, 1);
    }

    #[test]
    fn rejects_newer_breaking_sidecar_on_open() {
        // A newer binary raised its reader floor past us (format_version 99,
        // min_reader_format_version 99 > our 1) — a genuinely breaking format,
        // honest SchemaMismatch, never a silent orphan.
        let (_tmp, path) = finalized_segment();
        let meta_path = meta_path_for(&path);
        let mut sc: SegmentSidecar =
            fauna_cbor::decode_strict(&std::fs::read(&meta_path).unwrap()).unwrap();
        sc.format_version = 99;
        sc.min_reader_format_version = 99;
        std::fs::write(&meta_path, fauna_cbor::encode_canonical(&sc).unwrap()).unwrap();

        let err = FramedSegment::open(&path).expect_err("newer-breaking sidecar must error");
        assert!(matches!(err, SegmentStoreError::SchemaMismatch(_)));
    }
}
