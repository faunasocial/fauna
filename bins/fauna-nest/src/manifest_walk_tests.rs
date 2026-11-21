//! The nest walk's tests, and the paged-file seeder every nest reader's
//! paged-manifest test shares.

use super::*;
use crate::blob_store::DiskBlobStore;
use fauna_core::manifest_paging::PagingRule;

/// The small paging rule the nest's tests write with (`test-util`): more
/// than four chunks is paged, pages of two or three entries — so a file of
/// a dozen tiny chunks spans several pages.
pub(crate) fn small_rule() -> PagingRule {
    PagingRule::with_thresholds(4, 2, 3, 1)
}

/// A paged file as [`seed_paged_file`] wrote it.
pub(crate) struct PagedFile {
    /// The root's content address — what a change record names.
    pub root: ContentHash,
    /// The page blobs, in file order.
    pub pages: Vec<ContentHash>,
    /// Each page's chunk count, in file order.
    pub page_chunk_counts: Vec<u32>,
    /// Every chunk's store key, in file order.
    pub chunks: Vec<ContentHash>,
    /// The file's bytes.
    pub content: Vec<u8>,
}

/// Write `content` cut into `piece`-byte chunks as a paged file — chunks,
/// then pages, then the root, each framed by `encode_blob` exactly as the
/// chunk route, the page door and the manifest door store them, with a
/// `blob_metadata` row each when `db` is given. `content_key` seals the
/// chunks and the manifest (`Some`) or leaves the file plaintext (`None`).
pub(crate) async fn seed_paged_file(
    store: &Arc<dyn BlobStoreBackend>,
    db: Option<&crate::db::CacheDb>,
    at_rest_key: Option<&BackupKey>,
    content: &[u8],
    piece: usize,
    content_key: Option<&[u8; 32]>,
) -> PagedFile {
    async fn put(
        store: &Arc<dyn BlobStoreBackend>,
        db: Option<&crate::db::CacheDb>,
        at_rest_key: Option<&BackupKey>,
        key: &ContentHash,
        body: &[u8],
        kind: &str,
    ) {
        let framed = crate::backup::encode_blob(body, at_rest_key, false).unwrap();
        store.put(key, &framed).await.unwrap();
        if let Some(db) = db {
            db.put_blob_metadata(&key.digest(), body.len() as i64, kind, None, None)
                .await
                .unwrap();
        }
    }

    let mut chunk_hashes = Vec::new();
    let mut chunk_sizes = Vec::new();
    let mut stored = Vec::new();
    for data in content.chunks(piece) {
        let hash = ContentHash::of_raw(data);
        let (store_key, body) = match content_key {
            None => (
                hash,
                fauna_core::chunk_seal::FramedChunk::frame(&hash, data)
                    .unwrap()
                    .into_body(),
            ),
            Some(key) => fauna_core::chunk_seal::seal_chunk_body(&hash, data, key).unwrap(),
        };
        put(store, db, at_rest_key, &store_key, &body, "chunk").await;
        chunk_hashes.push(hash);
        chunk_sizes.push(data.len() as u64);
        stored.push(store_key);
    }
    let manifest = ChunkManifest {
        file_hash: ContentHash::of_raw(content),
        total_size: content.len() as u64,
        chunk_hashes,
        chunk_sizes,
        stored_hashes: content_key.map(|_| stored.clone()),
        sealed_hashes: None,
        min_reader: None,
        pages: None,
    };
    let (root_bytes, page_bytes) = manifest
        .into_wire_parts_with(&small_rule(), content_key)
        .unwrap();
    assert!(page_bytes.len() >= 3, "the fixture spans several pages");
    let mut pages = Vec::new();
    let mut page_chunk_counts = Vec::new();
    for bytes in &page_bytes {
        let hash = ContentHash::of_raw(bytes);
        put(store, db, at_rest_key, &hash, bytes, "manifest_page").await;
        pages.push(hash);
        page_chunk_counts.push(
            fauna_core::manifest_paging::ManifestPage::decode_checked(bytes)
                .unwrap()
                .entry_count() as u32,
        );
    }
    let root = ContentHash::of_raw(&root_bytes);
    put(store, db, at_rest_key, &root, &root_bytes, "manifest").await;
    PagedFile {
        root,
        pages,
        page_chunk_counts,
        chunks: stored,
        content: content.to_vec(),
    }
}

/// The root a [`PagedFile`] names, decoded from the store.
pub(crate) async fn stored_root(
    store: &Arc<dyn BlobStoreBackend>,
    file: &PagedFile,
) -> ChunkManifest {
    let raw = store.get(&file.root).await.unwrap().unwrap();
    fauna_core::encoding::canonical_decode(&crate::backup::decode_blob(&raw, None).unwrap())
        .unwrap()
}

fn store() -> (Arc<dyn BlobStoreBackend>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
    (store, dir)
}

/// The store-key walk over a paged root yields every page and every chunk's
/// store key in file order — never the root's own empty chunk list.
#[tokio::test]
async fn store_keys_walks_every_page_of_a_root() {
    let (store, _dir) = store();
    for key in [None, Some([7u8; 32])] {
        let file = seed_paged_file(&store, None, None, &[0x5a; 200], 16, key.as_ref()).await;
        let root = stored_root(&store, &file).await;
        assert!(root.is_paged());
        let src = StorePages {
            store: &store,
            at_rest_key: None,
        };
        let keys = super::store_keys(&root, &src).await.unwrap();
        assert_eq!(keys.page_hashes, file.pages);
        assert_eq!(keys.store_keys, file.chunks);
    }
}

/// A single manifest is its own one page: no fetch, its store keys as is.
#[tokio::test]
async fn store_keys_of_a_single_manifest_fetches_nothing() {
    let (store, _dir) = store();
    let chunk = ContentHash::of_raw(b"one");
    let manifest = ChunkManifest {
        file_hash: chunk,
        total_size: 3,
        chunk_hashes: vec![chunk],
        chunk_sizes: vec![3],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
        pages: None,
    };
    let src = StorePages {
        store: &store,
        at_rest_key: None,
    };
    let keys = super::store_keys(&manifest, &src).await.unwrap();
    assert!(keys.page_hashes.is_empty());
    assert_eq!(keys.store_keys, vec![chunk]);
}

/// An absent page is an unresolved reference, and a page whose bytes do not
/// match its address is invalid — neither is ever read as an empty page.
#[tokio::test]
async fn a_missing_or_corrupt_page_fails_the_walk() {
    let (store, _dir) = store();
    let file = seed_paged_file(&store, None, None, &[0x11; 200], 16, None).await;
    let root = stored_root(&store, &file).await;
    let src = StorePages {
        store: &store,
        at_rest_key: None,
    };

    store.delete(&file.pages[1]).await.unwrap();
    match super::store_keys(&root, &src).await {
        Err(WalkError::MissingPage(h)) => assert_eq!(h, file.pages[1]),
        other => panic!("expected MissingPage, got {other:?}"),
    }

    let framed = crate::backup::encode_blob(b"not the page", None, false).unwrap();
    store.put(&file.pages[1], &framed).await.unwrap();
    assert!(matches!(
        super::store_keys(&root, &src).await,
        Err(WalkError::Invalid(_))
    ));
}
