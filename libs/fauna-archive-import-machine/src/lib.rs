//! The import-from-an-export-archive wizard's shared machine — one machine,
//! one page, one ID set for all 7 apps (`docs/goal/behavior/archive-import.md`
//! § The wizard and its machine). Mirrors `MailImportMachine`'s shape.
//!
//! The split across the modules:
//!
//! - [`snapshot`] — what the `archive-import` page renders and the actions it
//!   dispatches, FFI-flat so one shape crosses UniFFI and wasm-bindgen alike.
//! - [`state`] — the archive folder's at-rest records (§ Storage). The folder
//!   *is* the session: there is no nest-side twin.
//! - [`nest`] — the two seams, [`ArchiveNest`] (folders, posts, media,
//!   calendar, tiers) and [`ArchiveOpener`] (the zip), plus the in-memory
//!   fakes for both.
//! - [`machine`] — the machine itself: hydrate, the client-side wizard steps,
//!   and the dispatch surface.
//! - [`run`] / [`audience`] — the import run and the audience mapping.
//! - `rpc_glue` — the native WS-RPC implementation of [`ArchiveNest`], behind
//!   the `rpc-glue` feature.
//!
//! Pure shared Rust: no direct `fauna-client-*` dependency, wasm-clean without
//! `rpc-glue` (§ Architectural rules — the app parses and signs, the nest only
//! stores the sealed folder).

#![forbid(unsafe_code)]

// The `uniffi::Record`/`uniffi::Enum` derives in `snapshot` are inert without
// it: `--features uniffi` does not compile at all until the scaffolding is
// registered (`fauna-client-mail-settings` carries the same one line).
#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_archive_import_machine");

pub mod audience;
pub mod machine;
pub mod nest;
pub mod run;
pub mod snapshot;
pub mod state;

#[cfg(all(feature = "rpc-glue", not(target_arch = "wasm32")))]
pub mod rpc_glue;

pub use machine::{ArchiveImportMachine, DispatchError};
pub use nest::{
    ArchiveFolderRef, ArchiveNest, ArchiveNestError, ArchiveOpener, ImportedEvent, MediaSeal,
    SharedSource, TierGate, UploadedMedia,
};
pub use snapshot::*;
pub use state::*;
