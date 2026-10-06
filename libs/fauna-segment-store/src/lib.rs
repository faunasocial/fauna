//! `fauna-segment-store` — shared primitives for fauna's append-only,
//! per-actor-per-kind segment stores.
//!
//! Consumed by `libs/fauna-index` (KindManifest only — Tantivy-specific
//! segment serialization stays in fauna-index) and by the per-kind
//! storage wrappers `libs/fauna-mail`, `libs/fauna-conversations`, etc.
//! (the full framed-records segment store).
//!
//! Design rationale tracked internally.

pub mod atomic;
pub mod codec;
pub mod compaction;
pub mod kind_manifest;
pub mod manager;
pub mod manifest;
pub mod pin;
pub mod segment;
pub mod store;
pub(crate) mod version;
pub mod versioned;

pub use atomic::{atomic_save, read_optional};
pub use compaction::{CompactionPlan, SegmentStats, compact, pick_compaction_inputs};
pub use kind_manifest::KindManifest;
pub use manager::{
    ActorScopedStore, AppendOutcome, MAX_COUNTER_FLOOR, ManagerError, ScopeRenameOutcome,
    SegmentBackupMeta, SegmentManager, SegmentPairBytes, bucket_for, check_counter_floor,
    raise_counter, rename_scope_dir,
};
pub use manifest::{Manifest, ManifestError};
pub use pin::PinSet;
pub use segment::{FramedSegment, RecordEntry, SegmentHeader};
pub use store::FramedSegmentStore;
pub use versioned::VersionedManifest;

/// Errors returned by this crate. Distinct from `fauna_index::IndexError`
/// because the two crates have different audiences (this crate is consumed
/// by every per-kind store; fauna-index is one consumer).
#[derive(Debug, thiserror::Error)]
pub enum SegmentStoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("encoding error: {0}")]
    Encoding(String),
    #[error("schema mismatch: {0}")]
    SchemaMismatch(String),
    #[error("invalid segment: {0}")]
    InvalidSegment(String),
    #[error("record not found: segment {segment_id} cid {cid}")]
    RecordNotFound {
        segment_id: u32,
        cid: fauna_cbor::Cid,
    },
}

impl SegmentStoreError {
    /// True for the ONE [`segment::FramedSegment::open`] failure that means
    /// the segment was never finalized — a crash between the last append and
    /// `finalize()`, which leaves a `.dat` with no `.meta` sidecar. Every
    /// other `open()` failure (a damaged sidecar, a drifted CARv2 block
    /// count, a corrupt `floor_metadata` length, …) is corruption of a
    /// segment that *was* finalized, and does **not** bound the loss to "the
    /// unfinalized tail" — a caller distinguishing the two (a replay's
    /// tail-skip, `bins/fauna-nest/src/segments/mod.rs::open_replay_segment`)
    /// must not claim the bound on any of them.
    ///
    /// `InvalidSegment` carries no structured discriminator (it is a plain
    /// message shared by several failure shapes), so this matches the exact
    /// text `open()` writes for the missing-sidecar case — coupling that
    /// stays inside this crate, the one place both sides can be kept in
    /// sync.
    pub fn is_unfinalized_crash_tail(&self) -> bool {
        matches!(self, Self::InvalidSegment(msg) if msg.starts_with("missing sidecar at "))
    }
}
