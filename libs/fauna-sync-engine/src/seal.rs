//! The **sink-agnostic seal** — chunk → compress → encrypt → re-key → manifest,
//! with no network and no store in sight.
//!
//! **Lives in [`fauna_core::blob_seal`] since 2026-09-26** and is re-exported
//! here verbatim: the module's rationale (two consumers producing
//! byte-identical artifacts, the framing that must never fork), its tests and
//! the [`SealedBlob`] shape all moved down one crate when the Media page's
//! upload into a content-keyed set became a third writer of the same
//! artifacts — a page machine compiled for wasm cannot depend on this engine
//! crate, and a second seal would be exactly the drift the module doc warns
//! of. Every in-crate caller (`SyncEngine`'s upload path, the custodian pull
//! and store) keeps its `crate::seal::` spelling.

pub use fauna_core::blob_seal::{SealedBlob, seal_blob, seal_chunk_body};
