//! The nest's side of the one manifest walk (`file-sync.md` § Paged manifests
//! → *One walk, used by every reader*).
//!
//! Every nest reader of a manifest's chunk list comes through here — garbage
//! collection and the residency drop, `check`, the custody charge, web-content
//! and share serving, and the root door's own verification — so a paged root
//! is never read as a chunkless file. The walk itself
//! ([`fauna_core::manifest_paging::PageWalk`]) is shared Rust; this module
//! only fetches its pages, from whatever [`PageSource`] the reader holds (the
//! raw blob store, or the serving seam that gates on a withhold).
//!
//! A page is fetched and verified against the hash, entry count and byte
//! length its root names before anything in it is used. An absent page is
//! [`WalkError::MissingPage`] — an unresolved reference, which garbage
//! collection fail-closes on (`backup-restore.md` § 9 step 4) — never a page
//! with nothing in it.

use std::future::Future;
use std::ops::Range;
use std::sync::Arc;

use fauna_core::chunk::ChunkManifest;
use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::manifest_paging::{PageWalk, WalkedPage};

use crate::blob_store::BlobStoreBackend;

/// Where a walk reads a page blob's plaintext (the bytes `encode_blob` framed
/// at the door): `Ok(None)` when the page is not held.
pub(crate) trait PageSource: Sync {
    /// The source's own failure — a store read error, a withhold.
    type Error: Send;

    /// The plaintext bytes of the page blob `hash`, or `None` if not held.
    fn page(
        &self,
        hash: &ContentHash,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, Self::Error>> + Send;
}

/// Pages read straight from the blob store, unframed with the at-rest key —
/// what garbage collection, `check`, the custody charge and the door read.
pub(crate) struct StorePages<'a> {
    pub store: &'a Arc<dyn BlobStoreBackend>,
    pub at_rest_key: Option<&'a BackupKey>,
}

impl PageSource for StorePages<'_> {
    type Error = anyhow::Error;

    fn page(
        &self,
        hash: &ContentHash,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, Self::Error>> + Send {
        let hash = *hash;
        async move {
            let Some(raw) = self.store.get(&hash).await? else {
                return Ok(None);
            };
            crate::backup::decode_blob(&raw, self.at_rest_key).map(Some)
        }
    }
}

/// Why a walk could not yield a manifest's chunk entries.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WalkError<E> {
    /// A page the root names is not held: an unresolved reference.
    #[error("manifest page {} is not held", hex::encode(.0.digest()))]
    MissingPage(ContentHash),
    /// The page source failed (a store read error, a withhold).
    #[error("reading a manifest page failed")]
    Source(E),
    /// The manifest or a page failed the walk's checks: a root of the wrong
    /// shape, or a page that does not hash to its address, does not decode as
    /// a page, or does not hold what its root names.
    #[error("{0:#}")]
    Invalid(anyhow::Error),
}

/// A manifest's references as the store-key walk finds them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ManifestKeys {
    /// The page blobs a root names, in file order (empty for a single
    /// manifest).
    pub page_hashes: Vec<ContentHash>,
    /// Every chunk's store key, in file order.
    pub store_keys: Vec<ContentHash>,
}

/// Open pages `pages` of `walk`, each fetched from `src` and verified.
pub(crate) async fn open_pages<S: PageSource>(
    walk: &PageWalk,
    pages: Range<usize>,
    src: &S,
) -> Result<Vec<WalkedPage>, WalkError<S::Error>> {
    let mut out = Vec::with_capacity(pages.len());
    for i in pages {
        out.push(open_page(walk, i, src).await?);
    }
    Ok(out)
}

/// Open page `index` of `walk`, fetched from `src` and verified.
pub(crate) async fn open_page<S: PageSource>(
    walk: &PageWalk,
    index: usize,
    src: &S,
) -> Result<WalkedPage, WalkError<S::Error>> {
    let page = match walk.page_to_fetch(index) {
        None => walk.open_page(index, None),
        Some(hash) => {
            let bytes = src
                .page(&hash)
                .await
                .map_err(WalkError::Source)?
                .ok_or(WalkError::MissingPage(hash))?;
            walk.open_page(index, Some(&bytes))
        }
    };
    page.map_err(WalkError::Invalid)
}

/// Every store key `manifest` references, and the page blobs it names — the
/// walk the reachability readers take (garbage collection, the residency
/// drop, `check`, the custody charge, the root door).
///
/// A single manifest is the walk's one page and needs no fetch; its store
/// keys are read without [`PageWalk::open`]'s shape gate, exactly as these
/// readers read them before paging existed, so a malformed single manifest a
/// client recorded makes garbage collection over-retain its keys rather than
/// fail closed for every account on the nest. A root is opened by the walk
/// (its shape checked) and every page fetched and verified, one at a time.
pub(crate) async fn store_keys<S: PageSource>(
    manifest: &ChunkManifest,
    src: &S,
) -> Result<ManifestKeys, WalkError<S::Error>> {
    if let Ok(keys) = manifest.store_keys() {
        return Ok(ManifestKeys {
            page_hashes: Vec::new(),
            store_keys: keys,
        });
    }
    let walk = PageWalk::open(manifest.clone(), None).map_err(WalkError::Invalid)?;
    let page_hashes: Vec<ContentHash> = walk.page_refs().iter().map(|p| p.hash).collect();
    let mut store_keys = Vec::new();
    for i in 0..walk.page_count() {
        store_keys.extend(open_page(&walk, i, src).await?.store_keys);
    }
    Ok(ManifestKeys {
        page_hashes,
        store_keys,
    })
}

#[cfg(test)]
#[path = "manifest_walk_tests.rs"]
pub(crate) mod tests;
