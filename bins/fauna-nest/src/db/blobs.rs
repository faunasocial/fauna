//! Blob metadata methods.

use super::{BlobMetadataRow, BlobStorageStats};
use super::{CacheDb, now_epoch_secs};
use anyhow::{Context, Result};

impl CacheDb {
    // ==================== Blob Metadata ====================

    /// Insert blob metadata (INSERT OR IGNORE — idempotent by hash).
    pub async fn put_blob_metadata(
        &self,
        hash: &[u8; 32],
        size_bytes: i64,
        content_type: &str,
        has_c2pa: Option<bool>,
        thumbnail_hash: Option<&[u8; 32]>,
    ) -> Result<()> {
        let hash = *hash;
        let content_type = content_type.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT OR IGNORE INTO blob_metadata (hash, size_bytes, content_type, created_at, last_accessed, has_c2pa, thumbnail_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![hash.as_slice(), size_bytes, content_type, now, now, has_c2pa.map(|v| v as i32), thumbnail_hash.map(|h| h.as_slice())],
        )
        .context("put blob metadata")?;
        Ok(())
    }

    /// Retrieve blob metadata by hash.
    pub async fn get_blob_metadata(&self, hash: &[u8; 32]) -> Result<Option<BlobMetadataRow>> {
        let hash = *hash;
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT hash, size_bytes, content_type, created_at, last_accessed,
                    storage_local, storage_s3, storage_nodes, ref_count, has_c2pa, thumbnail_hash
             FROM blob_metadata WHERE hash = ?1",
            rusqlite::params![hash.as_slice()],
            |row| {
                Ok(BlobMetadataRow {
                    hash: row.get(0)?,
                    size_bytes: row.get(1)?,
                    content_type: row.get(2)?,
                    created_at: row.get(3)?,
                    last_accessed: row.get(4)?,
                    storage_local: row.get::<_, i64>(5)? != 0,
                    storage_s3: row.get::<_, i64>(6)? != 0,
                    storage_nodes: row.get(7)?,
                    ref_count: row.get(8)?,
                    has_c2pa: row.get::<_, Option<i32>>(9)?.map(|v| v != 0),
                    thumbnail_hash: row.get(10)?,
                })
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get blob metadata"),
        }
    }

    /// The stored sizes of a batch of blobs, keyed by hash — one chunked query
    /// instead of N point reads (the custody-charge derivation walks a
    /// manifest's whole chunk set). A hash with no metadata row is absent from
    /// the map; the caller decides whether absence refuses.
    pub async fn get_blob_sizes(
        &self,
        hashes: &[[u8; 32]],
    ) -> Result<std::collections::HashMap<[u8; 32], i64>> {
        let hashes = hashes.to_vec();
        let conn = self.conn.lock().await;
        let mut out = std::collections::HashMap::with_capacity(hashes.len());
        for batch in hashes.chunks(500) {
            let placeholders = vec!["?"; batch.len()].join(",");
            let sql = format!(
                "SELECT hash, size_bytes FROM blob_metadata WHERE hash IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql).context("prepare blob size batch")?;
            let rows = stmt
                .query_map(
                    rusqlite::params_from_iter(batch.iter().map(|h| h.as_slice())),
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
                )
                .context("query blob size batch")?;
            for row in rows {
                let (hash, size) = row.context("read blob size row")?;
                if let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) {
                    out.insert(hash, size);
                }
            }
        }
        Ok(out)
    }

    /// Mark a blob as stored (or not) in S3.
    pub async fn set_blob_s3(&self, hash: &[u8; 32], stored: bool) -> Result<()> {
        let hash = *hash;
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE blob_metadata SET storage_s3 = ?1 WHERE hash = ?2",
            rusqlite::params![stored as i64, hash.as_slice()],
        )
        .context("set blob s3")?;
        Ok(())
    }

    /// Increment the reference count of a blob.
    pub async fn increment_blob_ref(&self, hash: &[u8; 32]) -> Result<()> {
        let hash = *hash;
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE blob_metadata SET ref_count = ref_count + 1 WHERE hash = ?1",
            rusqlite::params![hash.as_slice()],
        )
        .context("increment blob ref")?;
        Ok(())
    }

    /// Decrement the reference count of a blob.
    pub async fn decrement_blob_ref(&self, hash: &[u8; 32]) -> Result<()> {
        let hash = *hash;
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE blob_metadata SET ref_count = MAX(0, ref_count - 1) WHERE hash = ?1",
            rusqlite::params![hash.as_slice()],
        )
        .context("decrement blob ref")?;
        Ok(())
    }

    /// Delete blob metadata by hash.
    pub async fn delete_blob_metadata(&self, hash: &[u8; 32]) -> Result<()> {
        let hash = *hash;
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM blob_metadata WHERE hash = ?1",
            rusqlite::params![hash.as_slice()],
        )
        .context("delete blob metadata")?;
        Ok(())
    }

    /// Get aggregate storage statistics for all blobs.
    pub async fn blob_storage_stats(&self) -> Result<BlobStorageStats> {
        let conn = self.conn.lock().await;
        let total_blobs: i64 = conn
            .query_row("SELECT COUNT(*) FROM blob_metadata", [], |row| row.get(0))
            .unwrap_or(0);
        let total_bytes: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(size_bytes), 0) FROM blob_metadata",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let local_blobs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM blob_metadata WHERE storage_local = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let s3_blobs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM blob_metadata WHERE storage_s3 = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(BlobStorageStats {
            total_blobs,
            total_bytes,
            local_blobs,
            s3_blobs,
        })
    }

    /// List all blob hashes stored in blob_metadata.
    pub async fn list_all_blob_hashes(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT hash FROM blob_metadata")?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Delete blob metadata entries by hash. Returns count deleted.
    pub async fn delete_blob_metadata_batch(&self, hashes: &[Vec<u8>]) -> Result<u64> {
        let conn = self.conn.lock().await;
        let mut count = 0u64;
        let mut stmt = conn.prepare("DELETE FROM blob_metadata WHERE hash = ?1")?;
        for hash in hashes {
            count += stmt.execute(rusqlite::params![hash])? as u64;
        }
        Ok(count)
    }
}
