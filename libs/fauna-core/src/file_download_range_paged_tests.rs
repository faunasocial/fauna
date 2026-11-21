//! Tier-1 pins for the ranged read of a **paged** file (`file-sync.md`
//! § Paged manifests → *One walk, used by every reader*: `byte_len` lets a
//! ranged read fetch only the pages its range covers).

use super::tests::{FakeFetcher, owner_key};
use super::*;
use crate::manifest_paging::PagingRule;

/// Seal `n` distinct chunks of `size` bytes under `root`, cut the manifest
/// under small thresholds so it pages, and stock a fetcher with the root, its
/// pages and the chunk bodies. Returns the fetcher, the root's address, its
/// pages' addresses in file order, the store keys and the plaintext file.
fn sealed_paged_file(
    root: &[u8; 32],
    n: usize,
    size: usize,
) -> (
    FakeFetcher,
    ContentHash,
    Vec<ContentHash>,
    Vec<ContentHash>,
    Vec<u8>,
) {
    let plaintext: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let mut c = format!("chunk-{i:05}-").into_bytes();
            c.resize(size, (i % 251) as u8);
            c
        })
        .collect();
    let hashes: Vec<ContentHash> = plaintext.iter().map(|c| ContentHash::of_raw(c)).collect();
    let ciphertexts: Vec<Vec<u8>> = hashes
        .iter()
        .zip(&plaintext)
        .map(|(h, c)| {
            crate::chunk_crypto::encrypt_chunk(root, h, &crate::compress::compress_chunk(c))
                .unwrap()
        })
        .collect();
    let store_keys: Vec<ContentHash> = ciphertexts.iter().map(|c| ContentHash::of_raw(c)).collect();
    let file_data = crate::chunker::reassemble_chunks(&plaintext);
    let view = ChunkManifest {
        file_hash: ContentHash::of_raw(&file_data),
        total_size: file_data.len() as u64,
        chunk_hashes: hashes,
        chunk_sizes: plaintext.iter().map(|c| c.len() as u64).collect(),
        stored_hashes: Some(store_keys.clone()),
        sealed_hashes: None,
        min_reader: None,
        pages: None,
    };
    let rule = PagingRule::with_thresholds(16, 4, 8, 2);
    let (root_bytes, pages) = view.into_wire_parts_with(&rule, Some(root)).unwrap();
    assert!(pages.len() >= 3, "the small rule must page {n} chunks");
    let root_hash = ContentHash::of_raw(&root_bytes);
    let mut fetcher = FakeFetcher::default();
    fetcher.put(&root_hash, root_bytes);
    let mut page_hashes = Vec::new();
    for page in pages {
        let hash = ContentHash::of_raw(&page);
        page_hashes.push(hash);
        fetcher.put(&hash, page);
    }
    for (key, body) in store_keys.iter().zip(ciphertexts) {
        fetcher.put(key, body);
    }
    (fetcher, root_hash, page_hashes, store_keys, file_data)
}

/// The byte offset at which each page of `root_hash` starts, from its root.
fn page_starts(fetcher: &FakeFetcher, root_hash: &ContentHash) -> Vec<u64> {
    let root: ChunkManifest =
        crate::encoding::canonical_decode(&fetcher.get(root_hash).unwrap()).unwrap();
    let mut at = 0;
    root.pages
        .unwrap()
        .iter()
        .map(|p| {
            let start = at;
            at += p.byte_len;
            start
        })
        .collect()
}

fn take_manifest_fetches(fetcher: &FakeFetcher) -> Vec<ContentHash> {
    std::mem::take(&mut *fetcher.manifests_fetched.lock().unwrap())
}

#[tokio::test]
async fn a_range_inside_one_page_fetches_the_root_and_that_page_alone() {
    let key = owner_key();
    let (fetcher, root_hash, pages, store_keys, expected) =
        sealed_paged_file(&key.convergent_chunk_root(), 40, 100);
    let keys = FileDownloadKeys::owner(key);
    let starts = page_starts(&fetcher, &root_hash);
    // A range inside the second page, past its first chunk.
    let offset = starts[1] + 150;
    let got = download_file_range_by_manifest(
        &fetcher,
        &keys,
        root_hash,
        None,
        "raw/disk.img",
        offset,
        30,
    )
    .await
    .unwrap();
    assert_eq!(got, &expected[offset as usize..offset as usize + 30]);
    assert_eq!(
        take_manifest_fetches(&fetcher),
        vec![root_hash, pages[1]],
        "only the root and the page the range covers"
    );
    // The chunk fetched is the one holding [offset, offset + 30).
    let chunk = (offset / 100) as usize;
    assert_eq!(fetcher.fetched(), vec![store_keys[chunk..=chunk].to_vec()]);
}

#[tokio::test]
async fn a_range_straddling_two_pages_fetches_exactly_those_two() {
    let key = owner_key();
    let (fetcher, root_hash, pages, _, expected) =
        sealed_paged_file(&key.convergent_chunk_root(), 40, 100);
    let keys = FileDownloadKeys::owner(key);
    let starts = page_starts(&fetcher, &root_hash);
    let offset = starts[2] - 10;
    let got = download_file_range_by_manifest(
        &fetcher,
        &keys,
        root_hash,
        None,
        "raw/disk.img",
        offset,
        20,
    )
    .await
    .unwrap();
    assert_eq!(got, &expected[offset as usize..offset as usize + 20]);
    assert_eq!(
        take_manifest_fetches(&fetcher),
        vec![root_hash, pages[1], pages[2]]
    );
}

#[tokio::test]
async fn a_range_past_the_end_fetches_no_page() {
    let key = owner_key();
    let (fetcher, root_hash, _, _, expected) =
        sealed_paged_file(&key.convergent_chunk_root(), 40, 100);
    let keys = FileDownloadKeys::owner(key);
    let got = download_file_range_by_manifest(
        &fetcher,
        &keys,
        root_hash,
        None,
        "raw/disk.img",
        expected.len() as u64,
        10,
    )
    .await
    .unwrap();
    assert!(got.is_empty());
    assert_eq!(take_manifest_fetches(&fetcher), vec![root_hash]);
    assert!(fetcher.fetched().is_empty());
}

#[tokio::test]
async fn a_ranged_read_refuses_a_page_served_wrong() {
    let key = owner_key();
    let (mut fetcher, root_hash, pages, _, _) =
        sealed_paged_file(&key.convergent_chunk_root(), 40, 100);
    let keys = FileDownloadKeys::owner(key);
    let starts = page_starts(&fetcher, &root_hash);
    let other = fetcher.get(&pages[0]).unwrap();
    fetcher.put(&pages[1], other);
    let err = download_file_range_by_manifest(
        &fetcher,
        &keys,
        root_hash,
        None,
        "raw/disk.img",
        starts[1],
        10,
    )
    .await
    .expect_err("a page that is not the one its root names");
    assert!(format!("{err:#}").contains("page 1"), "{err:#}");
}

#[tokio::test]
async fn a_ranged_read_of_a_paged_root_refuses_a_reader_without_its_root() {
    let key = owner_key();
    let (fetcher, root_hash, _, _, _) = sealed_paged_file(&key.convergent_chunk_root(), 40, 100);
    // Another account's key: the root's sealed file hash opens under none of
    // its roots, so nothing past the root is fetched.
    let keys = FileDownloadKeys::owner(crate::crypto::BackupKey::from_bytes([0x52u8; 32]));
    download_file_range_by_manifest(&fetcher, &keys, root_hash, None, "raw/disk.img", 0, 10)
        .await
        .expect_err("a root sealed under another key does not open");
    assert_eq!(take_manifest_fetches(&fetcher), vec![root_hash]);
}
