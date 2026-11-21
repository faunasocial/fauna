//! Tier-1 pins for [`super::fetch_manifest_blobs`], the keyless walk every
//! reader that moves or checks stored bytes shares (`file-sync.md` § Paged
//! manifests → *One walk, used by every reader*).

use super::*;
use crate::manifest_paging::PagingRule;
use std::collections::HashMap;

struct Blobs(HashMap<ContentHash, Vec<u8>>);

#[async_trait::async_trait]
impl BlobFetcher for Blobs {
    async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        self.0
            .get(hash)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("404 {}", hex::encode(hash.digest())))
    }

    async fn fetch_chunks(&self, _keys: &[ContentHash], _path: &str) -> Result<Vec<Vec<u8>>> {
        anyhow::bail!("no chunks here")
    }
}

/// A sealed-chunk manifest of `n` chunks (store keys distinct from the
/// plaintext hashes), cut under small thresholds so it pages past 16.
fn cut(n: usize) -> (Blobs, ContentHash, Vec<ContentHash>, usize) {
    cut_under(n, Some([9u8; 32]))
}

/// [`cut`] sealed under `seal` (`None`: a plaintext manifest, whose store
/// keys are its plaintext hashes).
fn cut_under(n: usize, seal: Option<[u8; 32]>) -> (Blobs, ContentHash, Vec<ContentHash>, usize) {
    let h = |tag: &str, i: usize| ContentHash::of_raw(format!("{tag}-{i}").as_bytes());
    let sizes: Vec<u64> = (0..n).map(|i| 100 + i as u64 % 5).collect();
    let plain: Vec<ContentHash> = (0..n).map(|i| h("plain", i)).collect();
    let stored: Vec<ContentHash> = match seal {
        Some(_) => (0..n).map(|i| h("stored", i)).collect(),
        None => plain.clone(),
    };
    let view = ChunkManifest {
        file_hash: ContentHash::of_raw(b"file"),
        total_size: sizes.iter().sum(),
        chunk_hashes: plain,
        chunk_sizes: sizes,
        stored_hashes: seal.is_some().then(|| stored.clone()),
        sealed_hashes: None,
        min_reader: None,
        pages: None,
    };
    let rule = PagingRule::with_thresholds(16, 4, 32, 3);
    let (root, pages) = view
        .into_wire_parts_with(&rule, seal.as_ref())
        .expect("cut");
    let page_count = pages.len();
    let root_hash = ContentHash::of_raw(&root);
    let mut map: HashMap<ContentHash, Vec<u8>> = pages
        .into_iter()
        .map(|b| (ContentHash::of_raw(&b), b))
        .collect();
    map.insert(root_hash, root);
    (Blobs(map), root_hash, stored, page_count)
}

#[tokio::test]
async fn a_paged_root_yields_every_page_and_the_files_store_keys() {
    let (fetcher, root_hash, stored, page_count) = cut(100);
    assert!(page_count > 1, "the small rule must page 100 chunks");
    let got = fetch_manifest_blobs(&fetcher, &root_hash).await.unwrap();
    assert!(got.manifest.is_paged());
    assert_eq!(got.store_keys, stored);
    assert_eq!(got.pages.len(), page_count);
    for (hash, bytes) in &got.pages {
        assert_eq!(ContentHash::of_raw(bytes), *hash);
    }
    assert_eq!(ContentHash::of_raw(&got.manifest_bytes), root_hash);
}

#[tokio::test]
async fn a_single_manifest_reads_as_before_with_no_pages() {
    let (fetcher, root_hash, stored, _) = cut(10);
    let got = fetch_manifest_blobs(&fetcher, &root_hash).await.unwrap();
    assert!(!got.manifest.is_paged());
    assert!(got.pages.is_empty());
    assert_eq!(got.store_keys, stored);
}

#[tokio::test]
async fn a_page_served_wrong_is_refused() {
    let (mut fetcher, root_hash, _, _) = cut(100);
    let root: ChunkManifest = crate::encoding::canonical_decode(&fetcher.0[&root_hash]).unwrap();
    let refs = root.pages.as_ref().unwrap();
    let (first, second) = (refs[0].hash, refs[1].hash);
    let other = fetcher.0[&second].clone();
    fetcher.0.insert(first, other);
    let err = fetch_manifest_blobs(&fetcher, &root_hash)
        .await
        .expect_err("a page that is not the one its root names");
    assert!(format!("{err:#}").contains("page 0"), "{err:#}");
}

#[tokio::test]
async fn a_root_served_wrong_is_refused_before_it_is_decoded() {
    let (mut fetcher, root_hash, _, _) = cut(100);
    fetcher.0.insert(root_hash, b"not the root".to_vec());
    let err = fetch_manifest_blobs(&fetcher, &root_hash)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("hash mismatch"), "{err:#}");
}

/// A sealed-only reader — an owner-only copy, a group set — opens a SEALED
/// paged root: a root's own chunk lists are empty, so its sealedness is its
/// sealed file hash (each page's matched to it by the walk), never the
/// `stored_hashes` it does not carry. A plaintext root is still refused.
#[tokio::test]
async fn a_sealed_only_reader_opens_a_sealed_paged_root_and_refuses_a_plaintext_one() {
    let owner = || crate::crypto::BackupKey::from_bytes([0x51u8; 32]);
    let keys = FileDownloadKeys::owner(owner()).sealed_only();
    let (fetcher, root_hash, stored, pages) = cut_under(100, Some(owner().convergent_chunk_root()));
    assert!(pages > 1);
    let opened = fetch_manifest(&fetcher, &keys, &root_hash, None)
        .await
        .expect("a sealed root opens for a sealed-only reader");
    assert_eq!(opened.stored_hashes.as_deref(), Some(&stored[..]));
    assert_eq!(opened.chunk_hashes.len(), 100);

    let (fetcher, root_hash, _, _) = cut_under(100, None);
    let err = fetch_manifest(&fetcher, &keys, &root_hash, None)
        .await
        .expect_err("a plaintext root is refused");
    assert_eq!(
        crate::apply_failure::permanent_reason(&err),
        Some(crate::apply_failure::PermanentApplyFailure::PLAINTEXT_REFUSED.reason),
        "{err:#}"
    );
}
