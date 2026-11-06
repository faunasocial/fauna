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
//! - [`link`] — phase two: the archive-link signals, `state/links.cbor`, the
//!   announce round, the origin index and the claims (§ Linking and merging).
//! - `profile` — step 6's profile prefill: the offer read at the run's end,
//!   applied only on the user's tap.
//! - [`text`] — the page's lines, composed once as `ComposedText` (the
//!   summaries, the audience sentence, the Progress line and bar, the links).
//! - [`threads`] — the `Archive` rail's read side: the imported message
//!   threads the conversations manager projects read-only.
//! - `timing` — the Progress step's elapsed time and estimate.
//! - `rpc_glue` — the WS-RPC implementation of [`ArchiveNest`] every app
//!   builds (natively, and on web over the SPA's transport and browser plane),
//!   behind the `rpc-glue` feature.
//! - `web_plane` — the browser `ArchivePlane` the web app builds that seam
//!   over (wasm32, behind the `web-plane` feature).
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
pub mod comments;
pub mod link;
pub mod machine;
pub mod nest;
mod offload;
mod profile;
pub mod run;
pub mod snapshot;
pub mod state;
pub mod text;
pub mod threads;
mod timing;

#[cfg(feature = "rpc-glue")]
pub mod rpc_glue;

#[cfg(all(target_arch = "wasm32", feature = "web-plane"))]
pub mod web_plane;

pub use machine::{ArchiveImportDispatchError, ArchiveImportMachine};
pub use nest::{
    ArchiveFolderRef, ArchiveNest, ArchiveNestError, ArchiveOpener, ImportedEvent, MediaSeal,
    MemoryOpener, SharedSource, TierGate, UploadedMedia,
};
pub use snapshot::*;
pub use state::*;
pub use text::{ArchiveImportPageText, ArchiveImportProgressRowText};
