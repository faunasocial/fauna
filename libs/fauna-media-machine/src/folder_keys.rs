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

    /// The **member door**: a fetcher over `home` for a member of the
    /// cross-nest shared set whose channel is `channel_id`. Its folder reads
    /// carry the member's short-lived byte-plane read token, minted through
    /// the member's OWN nest (`fauna.folders.read_token.get`, which relays
    /// `fauna.federation.folder.read_token.mint` to the home nest behind its
    /// member gate), beside the folder hint — the bearer the home nest's
    /// store-miss arm names the caller by, so a metadata-only folder's chunk
    /// is relayed from a seat instead of missing (`relay-serving.md` § Relay
    /// serving → *A member on another nest*, step (5)). A refused mint — the
    /// member was removed — fails the read with
    /// [`fauna_core::file_download::MembershipRefused`] in its chain.
    ///
    /// Built per download, so every download mints afresh and a removal is
    /// met at its first read. The default is the plain door
    /// ([`Self::fetcher_for`]): a platform with no member door reads only
    /// what rests in the home nest's store.
    fn member_fetcher_for(
        &self,
        home: &fauna_core::folder_keys::ForeignHome,
        channel_id: [u8; 32],
    ) -> std::sync::Arc<dyn fauna_core::file_download::BlobFetcher> {
        let _ = channel_id;
        self.fetcher_for(&home.nest_url, home.nest_actor_id.as_deref())
    }

    /// The member door's **write twin**: an uploader whose chunk and manifest
    /// POSTs address `home` under the short-lived write token the member's OWN
    /// nest mints for the set whose channel is `channel_id`
    /// (`fauna.folders.write_token.get`, which relays
    /// `fauna.federation.folder.write_token.mint` to the home nest behind its
    /// foreign-member + `access == 'writer'` gate), the home's TLS verified
    /// against the grant-delivered identity as the read door's is
    /// (`ui/media.md` § Cross-nest shared sets, *Upload*). A refused mint — the
    /// writer grant is gone — fails the POST.
    ///
    /// `None` (the default) means this platform has no write door: the upload
    /// into a foreign set is refused loudly, never sent to the own nest.
    fn member_uploader_for(
        &self,
        home: &fauna_core::folder_keys::ForeignHome,
        channel_id: [u8; 32],
    ) -> Option<std::sync::Arc<dyn crate::blob_uploader::MediaBlobUploader>> {
        let _ = (home, channel_id);
        None
    }
}
