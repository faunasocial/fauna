//! Tier-1 pins for the re-seal's paged write (`file-sync.md` § Paged
//! manifests → *One walk, used by every reader*, *Compatibility*): a result
//! over the inline bound is posted as pages then a root, and a destination
//! without `paged-manifests` refuses before any chunk is sent.

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;
use crate::manifest_paging::{ManifestPage, PagingRule};

/// A plaintext file's blobs (manifest + framed chunks) in a map.
struct Blobs(HashMap<ContentHash, Vec<u8>>);

#[async_trait::async_trait]
impl BlobFetcher for Blobs {
    async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        self.0
            .get(hash)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("404 {}", hex::encode(hash.digest())))
    }

    async fn fetch_chunks(&self, keys: &[ContentHash], _path: &str) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            out.push(self.fetch_manifest(k).await?);
        }
        Ok(out)
    }
}

/// A plaintext file of `n` distinct chunks, stored as a single manifest.
fn plaintext_file(n: usize) -> (Blobs, ContentHash) {
    let plain: Vec<Vec<u8>> = (0..n)
        .map(|i| format!("chunk body number {i:05}").into_bytes())
        .collect();
    let hashes: Vec<ContentHash> = plain.iter().map(|c| ContentHash::of_raw(c)).collect();
    let file = crate::chunker::reassemble_chunks(&plain);
    let manifest = ChunkManifest {
        file_hash: ContentHash::of_raw(&file),
        total_size: file.len() as u64,
        chunk_hashes: hashes.clone(),
        chunk_sizes: plain.iter().map(|c| c.len() as u64).collect(),
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
        pages: None,
    };
    let bytes = crate::encoding::canonical_encode(&manifest.wire_form(None).unwrap()).unwrap();
    let hash = ContentHash::of_raw(&bytes);
    let mut map = HashMap::new();
    map.insert(hash, bytes);
    for (h, c) in hashes.iter().zip(&plain) {
        map.insert(
            *h,
            crate::chunk_seal::FramedChunk::frame(h, c)
                .unwrap()
                .into_body(),
        );
    }
    (Blobs(map), hash)
}

/// A sink recording every leg in order, under the small paging rule.
struct RecordingSink {
    supports_paged: bool,
    legs: Mutex<Vec<String>>,
    pages: Mutex<Vec<Vec<u8>>>,
    root: Mutex<Option<(ContentHash, Vec<u8>)>>,
}

impl RecordingSink {
    fn new(supports_paged: bool) -> Self {
        Self {
            supports_paged,
            legs: Mutex::new(Vec::new()),
            pages: Mutex::new(Vec::new()),
            root: Mutex::new(None),
        }
    }

    fn legs(&self) -> Vec<String> {
        self.legs.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ChunkStoreSink for RecordingSink {
    async fn put_chunks(
        &self,
        bodies: Vec<(ContentHash, Vec<u8>)>,
        _relative_path: &str,
    ) -> Result<usize> {
        self.legs.lock().unwrap().push("chunks".into());
        Ok(bodies.len())
    }

    fn paging_rule(&self) -> PagingRule {
        PagingRule::with_thresholds(16, 4, 8, 2)
    }

    async fn admit_paged_manifest(&self, chunks: usize) -> Result<()> {
        self.legs.lock().unwrap().push("admit".into());
        if self.supports_paged {
            Ok(())
        } else {
            Err(anyhow::Error::new(FileTooLargeForNest {
                chunks: Some(chunks),
            }))
        }
    }

    async fn put_manifest(
        &self,
        manifest_hash: ContentHash,
        manifest_bytes: Vec<u8>,
        pages: Vec<Vec<u8>>,
    ) -> Result<()> {
        let mut legs = self.legs.lock().unwrap();
        for _ in &pages {
            legs.push("page".into());
        }
        legs.push("root".into());
        *self.pages.lock().unwrap() = pages;
        *self.root.lock().unwrap() = Some((manifest_hash, manifest_bytes));
        Ok(())
    }
}

const NEW_ROOT: [u8; 32] = [0x37u8; 32];

#[tokio::test]
async fn a_paged_result_posts_its_pages_then_the_root() {
    let (fetcher, source) = plaintext_file(40);
    let sink = RecordingSink::new(true);
    let resealed = reseal_nest_copy_windowed(
        &fetcher,
        &FileDownloadKeys::default(),
        source,
        None,
        "big/disk.img",
        Some((NEW_ROOT, None)),
        &sink,
    )
    .await
    .unwrap();

    let legs = sink.legs();
    assert_eq!(legs.first().map(String::as_str), Some("admit"), "{legs:?}");
    let first_page = legs.iter().position(|l| l == "page").expect("pages posted");
    assert!(
        legs[1..first_page].iter().all(|l| l == "chunks"),
        "chunks before pages: {legs:?}"
    );
    assert_eq!(legs.last().map(String::as_str), Some("root"), "{legs:?}");

    let (root_hash, root_bytes) = sink.root.lock().unwrap().clone().unwrap();
    assert_eq!(root_hash, resealed.manifest_hash);
    assert_eq!(ContentHash::of_raw(&root_bytes), root_hash);
    let root: ChunkManifest = crate::encoding::canonical_decode(&root_bytes).unwrap();
    assert!(root.is_paged(), "the re-sealed result is a paged root");
    let named: Vec<ContentHash> = root.pages.unwrap().iter().map(|p| p.hash).collect();
    let posted: Vec<ContentHash> = sink
        .pages
        .lock()
        .unwrap()
        .iter()
        .map(|p| ContentHash::of_raw(p))
        .collect();
    assert_eq!(
        named, posted,
        "the root names exactly the posted pages, in order"
    );
    let entries: usize = sink
        .pages
        .lock()
        .unwrap()
        .iter()
        .map(|p| ManifestPage::decode_checked(p).unwrap().entry_count())
        .sum();
    assert_eq!(entries, 40);
    // The returned view stays the plaintext one the change record needs.
    assert_eq!(resealed.manifest.chunk_hashes.len(), 40);
    assert!(!resealed.manifest.is_paged());
}

#[tokio::test]
async fn a_destination_without_the_token_refuses_before_any_chunk() {
    let (fetcher, source) = plaintext_file(40);
    let sink = RecordingSink::new(false);
    let err = reseal_nest_copy_windowed(
        &fetcher,
        &FileDownloadKeys::default(),
        source,
        None,
        "big/disk.img",
        Some((NEW_ROOT, None)),
        &sink,
    )
    .await
    .expect_err("a nest without paged-manifests cannot carry the result");
    assert!(is_file_too_large_for_nest(&err), "{err:#}");
    assert_eq!(sink.legs(), vec!["admit".to_string()], "nothing sent");
}

#[tokio::test]
async fn a_result_under_the_inline_bound_asks_nothing_and_posts_one_manifest() {
    let (fetcher, source) = plaintext_file(10);
    let sink = RecordingSink::new(false);
    let resealed = reseal_nest_copy_windowed(
        &fetcher,
        &FileDownloadKeys::default(),
        source,
        None,
        "small.txt",
        Some((NEW_ROOT, None)),
        &sink,
    )
    .await
    .unwrap();
    let legs = sink.legs();
    assert_eq!(legs.last().map(String::as_str), Some("root"), "{legs:?}");
    assert!(
        legs[..legs.len() - 1].iter().all(|l| l == "chunks"),
        "no gate asked, no page posted: {legs:?}"
    );
    let (_, bytes) = sink.root.lock().unwrap().clone().unwrap();
    let posted: ChunkManifest = crate::encoding::canonical_decode(&bytes).unwrap();
    assert!(!posted.is_paged());
    // Byte for byte the single manifest's wire form, as before.
    let wire = resealed.manifest.wire_form(Some(&NEW_ROOT)).unwrap();
    assert_eq!(bytes, crate::encoding::canonical_encode(&wire).unwrap());
}
