use crate::blob_store::BlobStoreBackend;
use crate::db::CacheDb;
use anyhow::Result;
use fauna_core::data::ContentHash;
use std::sync::Arc;

pub struct PayloadStore {
    blob_store: Arc<dyn BlobStoreBackend>,
    db: Arc<CacheDb>,
    threshold: usize,
}

impl PayloadStore {
    pub fn new(blob_store: Arc<dyn BlobStoreBackend>, db: Arc<CacheDb>, threshold: usize) -> Self {
        Self {
            blob_store,
            db,
            threshold,
        }
    }

    /// Store a payload. Returns (bytes_for_db, optional_blob_hash).
    /// If payload exceeds threshold, writes to blob store and returns
    /// (empty vec, Some(hash)). Otherwise returns (payload, None).
    ///
    /// A spilled payload's `blob_metadata` row is written in the same breath
    /// as its bytes, and a failure to write it fails the store — that table is
    /// the blob sweep's world, so bytes without a row would be neither collected
    /// nor counted, and the ack of an inbox link that records no charge (one
    /// written before links recorded it) reads its refund from the row
    /// (`backup-restore.md` § 9, the sweep's premise). The row is safe
    /// only because the sweep pins every live `content.blob_hash` (step 2i): the
    /// caller names the hash in its content row immediately, and the creation
    /// grace covers the gap.
    pub async fn store(&self, payload: &[u8]) -> Result<(Vec<u8>, Option<ContentHash>)> {
        if payload.len() > self.threshold {
            let hash = ContentHash::of_raw(payload);
            self.blob_store.put(&hash, payload).await?;
            self.db
                .put_blob_metadata(
                    &hash.digest(),
                    payload.len() as i64,
                    "application/octet-stream",
                    None,
                    None,
                )
                .await?;
            Ok((Vec::new(), Some(hash)))
        } else {
            Ok((payload.to_vec(), None))
        }
    }

    /// Resolve a payload. If blob_hash is provided, fetch from blob store.
    /// Otherwise return the inline payload.
    ///
    /// Named uniquely, not `resolve`, because it is a read through a handle
    /// this struct holds for the life of the process: `backup::service`'s
    /// blob-store partition keys on the literal to classify every caller
    /// (`moderation.md` § Legal takedown → *The blob-serve door*).
    pub async fn resolve_payload(
        &self,
        payload: &[u8],
        blob_hash: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        match blob_hash {
            Some(hash_bytes) if hash_bytes.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(hash_bytes);
                let hash = ContentHash::from_digest_raw(arr);
                self.blob_store
                    .get(&hash)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("blob not found: {}", hex::encode(arr)))
            }
            _ => Ok(payload.to_vec()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::DiskBlobStore;

    fn setup() -> (PayloadStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let ps = PayloadStore::new(store, db, 64 * 1024);
        (ps, tmp)
    }

    #[tokio::test]
    async fn small_payload_stays_inline() {
        let (ps, _tmp) = setup();
        let data = b"small payload";
        let (stored, hash) = ps.store(data).await.unwrap();
        assert!(hash.is_none());
        assert_eq!(stored, data);
    }

    #[tokio::test]
    async fn large_payload_extracted_to_blob() {
        let (ps, _tmp) = setup();
        let data = vec![0xABu8; 65 * 1024]; // > 64 KB
        let (stored, hash) = ps.store(&data).await.unwrap();
        assert!(hash.is_some());
        assert!(stored.is_empty()); // sentinel
        let meta = ps
            .db
            .get_blob_metadata(&hash.unwrap().digest())
            .await
            .unwrap()
            .expect("a spilled payload carries its blob_metadata row");
        assert_eq!(
            meta.size_bytes,
            data.len() as i64,
            "the row records the body's own length"
        );
    }

    #[tokio::test]
    async fn resolve_inline() {
        let (ps, _tmp) = setup();
        let data = b"inline";
        let resolved = ps.resolve_payload(data, None).await.unwrap();
        assert_eq!(resolved, data);
    }

    #[tokio::test]
    async fn resolve_from_blob_store() {
        let (ps, _tmp) = setup();
        let data = vec![0xCDu8; 65 * 1024];
        let (_, hash) = ps.store(&data).await.unwrap();
        let hash = hash.unwrap();
        let resolved = ps
            .resolve_payload(b"", Some(hash.digest().as_slice()))
            .await
            .unwrap();
        assert_eq!(resolved, data);
    }

    #[tokio::test]
    async fn store_is_idempotent() {
        let (ps, _tmp) = setup();
        let data = vec![0xEFu8; 65 * 1024];
        let (_, h1) = ps.store(&data).await.unwrap();
        let (_, h2) = ps.store(&data).await.unwrap();
        assert_eq!(h1, h2); // same hash, deduplicated
    }
}
