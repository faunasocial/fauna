//! The **foreign-nest byte-fetcher** seam for Media downloads (Phase 2 client
//! read-side).
//!
//! The *key* half of this module — `FolderKeyResolver` / `ResolvedFolderKeys`
//! — moved to [`fauna_core::folder_keys`] when the sealed-label read surfaces
//! grew past Media: snapshot browse/diff and the conflict list resolve the same
//! custody, and reaching it through the Media *page machine* to render a file
//! name would have been the forked-second-resolver shape the sealing ruling's
//! read half forbids (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
//! `fauna-media-machine` re-exports both names, so existing callers are
//! unaffected.
//!
//! What stays here is genuinely Media-shaped: fetching *bytes* from a foreign
//! nest. A label needs no such thing — a sealed label travels inside the row,
//! wherever its bytes live.

pub use fauna_core::folder_keys::{FolderKeyResolver, ResolvedCustody, ResolvedFolderKeys};

/// Builds a [`fauna_core::file_download::BlobFetcher`] bound to a **foreign**
/// nest's base URL — the byte-plane leg of a cross-nest download: a foreign
/// shared set's manifest/chunk bytes live on its HOME nest (Phase 2 client
/// read-side), and so do a followed public folder's (`ui/media.md` § Followed
/// public folders, *Downloads*). When [`FolderKeyResolver::resolve`] returns a
/// `home_nest_url`, [`crate::machine::MediaMachine::download_file`] fetches
/// through a factory-built fetcher instead of the machine's own (home-bound)
/// injected one; `download_followed` does the same for a scope homed
/// elsewhere. Platform glue supplies the impl (native:
/// `fauna_client::ForeignPublicChunkFetcher`, the routes are public and
/// integrity is by content address; wasm: `WasmPublicChunkFetcher` over the
/// foreign base, S6 CORS-open). A machine without one refuses cross-nest
/// downloads loudly.
pub trait ForeignBlobFetcherFactory: fauna_core::MaybeSendSync {
    /// A fetcher whose GETs address `base_url` (no trailing slash) instead of
    /// the caller's own nest.
    ///
    /// `home_nest_actor_id` is the home nest's identity as the record delivered
    /// it (a foreign set's `ForeignFolder::home_nest_actor_id`, grant-stamped;
    /// a follow's `FollowedFolder::home_nest_actor_id`, stamped by the home
    /// nest on the public-fetch reply) — the trust root a self-signed
    /// home's TLS is verified against before the first byte is fetched
    /// (`security.md` § Transport trust, the federation-granted row: the caller
    /// holds no account on that nest, so no bearer handshake ever pins it).
    /// `None` keeps the WebPKI floor, never weaker. The browser cannot pin a
    /// certificate at all, so the wasm impl ignores it.
    fn fetcher_for(
        &self,
        base_url: &str,
        home_nest_actor_id: Option<&str>,
    ) -> std::sync::Arc<dyn fauna_core::file_download::BlobFetcher>;
}
