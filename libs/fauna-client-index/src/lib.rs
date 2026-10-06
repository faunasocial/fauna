//! **The client-side content-index builder** — the capability-position leg of
//! `docs/goal/behavior/content-index.md` (rollout slice S3, the first real
//! writer the format has ever had).
//!
//! A user's client is one of the two places the index may be built, because it
//! is one of the two places the content is readable in plaintext (the other is
//! the MDA bridge during a MUA session, which reaches mail/calendar only —
//! § Where the index is built). This crate owns the orchestration around that:
//! taking decrypted content off the shared receive paths, tokenizing it into
//! Tantivy segments, **sealing** them under the ratified per-kind key, and
//! publishing them into the `__index` reserved folder for fauna-sync to
//! replicate to every location the user owns.
//!
//! # What lives here vs. in `fauna-index`
//!
//! `fauna-index` owns the *format* — segments, manifests, seal/open, query
//! primitives. It has no idea where content comes from or where blobs go.
//! This crate owns the *orchestration*: the sink implementation, backfill,
//! cursoring, publishing, and (next slice) the synced-replica refresh and the
//! local query handle the Search seam calls. App glue registers the sink and
//! renders; it holds no index logic at all (§ Architectural rules 5).
//!
//! # Native only
//!
//! Tantivy cannot run in a browser, so the web SPA is the documented exception
//! and keeps floor search (§ Where queries run). There is deliberately no wasm
//! build of this crate — a fake or empty in-browser index is forbidden outright
//! (§ Don't do these).
//!
//! # Status
//!
//! S3 in progress. Built here so far: the mail-kind sink (receive hook →
//! sealed segment → publisher), its manifest bookkeeping, the production
//! publisher over the `__index` rail (blob PUT + `fauna.index.record`), the
//! synced-replica refresh that resumes a builder from what the nest already
//! holds, the local query handle the Search seam calls, and the flush debounce
//! that drives a resumed builder in production. There is **no** mail backfill
//! walk and there is not supposed to be one — the receive loop re-walks the
//! whole mailbox every launch, so the seam already sees it all
//! (`content-index.md` § Ingest triggers, v1, the 2026-08-03 correction). Still
//! owed by S3: the Search snapshot seam (piece 5) that calls
//! [`open_mail_reader`].

mod compaction;
mod index_builder;
mod lifecycle;
mod local_search;
mod rail_publisher;

pub use compaction::{COMPACTION_LIVE_SEGMENT_THRESHOLD, COMPACTION_TOMBSTONED_DOC_FRACTION};

/// Re-exported because they are in this crate's own public signatures
/// ([`resume_master_builder`], [`IndexBuilder::master`]): a caller must be able
/// to name a builder's kind and hand it a master key without taking its own
/// `fauna-index` dependency.
// `KindClass` rides along because the launcher's arm bookkeeping is keyed by
// class, not by kind — one builder per class is what keeps two of them from
// clobbering the class's single manifest (`IndexBuilder`'s type doc).
pub use fauna_index::{ContentKind, IndexMasterKey, KindClass};

pub use index_builder::{
    IndexBuildError, IndexBuilder, IndexableContact, IndexableFile, IndexablePost,
    MAX_DOCS_PER_SEGMENT, MAX_SEGMENT_BYTES, RailEntry, SealedSegment, SegmentRail,
};
pub use lifecycle::{DirectStager, spawn_flush_debounce};
pub use local_search::{
    ContactCorpusRead, FileCorpusRead, LocatedContact, LocatedDraft, LocatedFile, LocatedMessage,
    MailContentLookup, MailLocalSearch, MasterLocalSearch, PostCorpusRead,
};
pub use rail_publisher::{
    IndexRailPublisher, MAILCAL_KINDS, MailIndexReader, MailcalKeyRing, MasterIndexReader,
    open_mail_reader, open_master_reader, resume_mail_builder, resume_master_builder,
};
