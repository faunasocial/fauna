//! Re-exports `fauna-index` for UniFFI binding generation.
//!
//! Mirrors `src/mail.rs` — each consumer language (Swift, Kotlin, C#) gets
//! the exposed-via-UniFFI surface of fauna-index by virtue of fauna-ffi being
//! the aggregation point for `uniffi-bindgen generate`.
//!
//! The Rust crate's broader public API (manifest helpers, encryption,
//! seal/open of segment bytes, master-key rotation) deliberately stays
//! Rust-only — only the minimal query-path surface clients need today is
//! re-exported. Plan 5 will expand this list.
//!
//! **Maintenance note:** when fauna-index adds a new client-facing type or
//! function, add a corresponding `pub use fauna_index::{...}` line here.

pub use fauna_index::{
    ContentId, ContentKind, FieldKind, IndexError, IndexHandle, IndexedDoc, IndexedField, QueryHit,
    TimeRange,
};
