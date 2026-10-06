//! Page-level Media state machine.
//!
//! Mirrors `fauna-devices-machine`: a shared-Rust `#[uniffi::Object]` state
//! machine (`MediaMachine`) driving an observer-rendered page, exposed to native
//! apps via UniFFI (`libs/fauna-ffi`) and to web via WASM. Where the Devices
//! machine owns the *control plane* (device / folder management), this owns the
//! Media *content plane*: the cross-set all-media browse (`docs/goal/ui/media.md`
//! — folders are the substrate, Media is their media-optimized view). It
//! realizes `media.md` Architectural rule 2 ("observer-driven rendering off the
//! shared `media_snapshot()` cross-set snapshot") so all 7 apps render the
//! same explorer off one surface, not 5 hand-rolled ones (priority #1/#4).
//!
//! Under the `no-http-ws-rpc-everywhere` directive the control plane consumes
//! only WS-RPC kinds (`fauna.media.list` read + `fauna.sync.changes.record`
//! upload/delete writes) through the shared `fauna-client-media` /
//! `fauna-client-sync` adapters. The one exception is the upload's blob POST,
//! which rides the bulk-binary `POST /api/v1/blob` carve-out via the separate
//! [`blob_uploader::MediaBlobUploader`] seam — there is no other HTTP impl.
//!
//! Configuration-free by design (rule 4 — Media reads folders, never
//! *configures* them: no mode / retention / device knobs): the gestures are the
//! cross-set `refresh()`, the client-held view-state setters (`media-sort-select`
//! / `media-folder-filter` / `media-view-toggle`), and the content-plane
//! `upload()` (`file-upload` / `upload-button`) + `delete()`. Upload seals in
//! shared Rust (`fauna_core::blob_seal::seal_blob` under the folder's custody
//! root — the one at-rest shape, `media.md` § Encryption at rest) then POSTs the
//! chunks + manifest and records the member — the whole gesture lives here, not hand-rolled per
//! client (priority #2).
//!
//! See `docs/goal/ui/media.md` §§ State & data shape / Where logic lives.

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_media_machine");

pub mod blob_fetcher;
pub mod blob_uploader;
pub mod folder_keys;
pub mod machine;
pub mod nest_api;
pub mod observer;
#[cfg(feature = "rpc-glue")]
pub mod served_reseal;
pub mod snapshots;

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use blob_fetcher::FakeMediaBlobFetcher;
pub use blob_fetcher::MediaBlobFetcher;
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use blob_uploader::FakeMediaBlobUploader;
pub use blob_uploader::MediaBlobUploader;
pub use fauna_core::followed_media::{
    FollowedFetchError, FollowedFileEntry, FollowedMediaScope, FollowedMediaSource,
};
pub use folder_keys::{FolderKeyResolver, ResolvedCustody, ResolvedFolderKeys};
pub use machine::MediaMachine;
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use nest_api::{FakeMediaNestApi, MediaNestCall};
pub use nest_api::{MediaApiError, MediaNestApi};
#[cfg(feature = "rpc-glue")]
pub use nest_api::{build_media_machine, build_media_machine_with_folder_keys};
pub use observer::MediaObserver;
pub use snapshots::*;

// Re-export the sort key + item model clients consume through this one crate, so
// a consumer reaches the whole Media surface here (the select-value mapping +
// the shared snapshot/view logic live in fauna-client-media).
pub use fauna_client_media::{
    MediaFolder, MediaSnapshot, MediaSortKey, display_name, media::MediaItem,
};
