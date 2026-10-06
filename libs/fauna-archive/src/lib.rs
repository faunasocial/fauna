//! Import-from-an-export-archive: the platform-neutral archive model, the
//! frozen external-ID derivation, the streaming zip seam, and one parser per
//! platform. Owner: `docs/goal/behavior/archive-import.md`.
//!
//! Pure shared Rust — no tokio, no async, wasm-clean. The app parses; the
//! nest only ever stores the sealed folder this crate's output is written
//! into (§ Architectural rules).
//!
//! **Reusable as it stands.** This crate depends on crates.io only — no
//! `fauna-*` crate: the model carries its own [`Timestamp`], the platforms
//! their own token constants, and the ID derivation calls the dag-cbor codec
//! directly — so another project can take a Facebook or Instagram export
//! apart with it without taking Fauna along. Everything Fauna-specific (what
//! a record *becomes*, where it is stored, under which audience) lives in
//! `fauna-archive-import-machine`, the one consumer that knows both sides.

#![forbid(unsafe_code)]

pub mod error;
pub mod external_id;
pub mod facebook;
pub mod model;
pub mod parser;
pub mod reader;
pub mod source;
/// The generated fixture corpus as zips — this crate's own tests and the
/// downstream machines' (`archive-import.md` § Parser contract rule 7).
/// Never a real export; compiled only under `cfg(test)` or `test-helpers`.
#[cfg(any(test, feature = "test-helpers"))]
pub mod testing;
pub mod text;

pub use error::{ArchiveError, EntityError};
pub use model::{
    ArchiveAudience, ArchiveSummary, Category, CategoryCounts, Entity, EntityKind,
    ExternalActorRef, ExternalId, PARSER_VERSION, Platform, Timestamp,
};
pub use parser::{ArchiveParser, Detected, ExportFormat, detect, parsers};
pub use reader::{ArchiveReader, ZipDirectory, ZipEntry};
pub use source::{ArchiveSource, SourceCursor, VecSource};
