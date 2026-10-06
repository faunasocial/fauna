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
//! ```
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
    /// Create a new segment file (`.dat`) and seed in-memory state for the
    /// companion sidecar; the `.meta` file is only written at `finalize()`.
    pub fn create(path: &Path, header: SegmentHeader) -> Result<Self, SegmentStoreError> {
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)?;
        let writer = fauna_carv2::Writer::new(file, &[])
            .map_err(|e| SegmentStoreError::Encoding(format!("carv2 writer: {e}")))?;
        Ok(Self {
            path: path.to_path_buf(),
            header,
            state: SegmentState::Open {
                writer,
                floor_metadata: Vec::new(),
                record_order: Vec::new(),
                record_lens: std::collections::HashMap::new(),
            },
        })
    }

    /// Append a record. The CID is the consumer-supplied content address
    /// of `record_bytes`; the carv2 writer streams `(cid, bytes)` to the
    /// data section. Duplicate CIDs error (CARv2 indexes by digest, so a
    /// duplicate would silently overwrite the earlier block).
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
            ..
        } = &mut self.state
        else {
            return Err(SegmentStoreError::InvalidSegment(
                "append on finalized segment".into(),
            ));
        };
        if record_lens.contains_key(&cid) {
            return Err(SegmentStoreError::InvalidSegment(format!(
                "duplicate record cid within segment: {cid}"
            )));
        }
        record_lens.insert(cid, record_bytes.len() as u64);
        writer
            .write_block(&cid, record_bytes)
            .map_err(|e| SegmentStoreError::Encoding(format!("carv2 write_block: {e}")))?;
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
        // From here forward any error leaves self.state = Poisoned.
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
        use std::io::Write as _;
        let meta_path = meta_path_for(&self.path);
        let mut meta_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&meta_path)?;
        meta_file.write_all(&sidecar_bytes)?;
        meta_file.sync_all()?;

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
