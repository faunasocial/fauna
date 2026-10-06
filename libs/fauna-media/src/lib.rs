//! Shared-Rust media upload pipeline.
//!
//! Design tracked internally.
//!
//! Audience-keyed seal layer (`seal_for_audience`) dispatches across the four
//! audience classes named in `docs/goal/architecture/encryption-at-rest.md`'s
//! Media row. `process_and_seal` is the convenience composer per-app wire-up
//! sessions call. `process_media` is the real on-device pipeline (MIME sniff, metadata
//! strip, thumbnail, optional C2PA detection); with its cargo feature off it
//! degrades to an identity pass-through.

pub mod audience;
pub mod pipeline;
pub mod process;
pub mod seal;
pub mod sidecar;
#[cfg(feature = "test-fixtures")]
pub mod test_fixtures;
