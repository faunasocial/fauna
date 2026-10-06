//! Delivery receipt storage for video CDN tracking.
//!
//! When a nest serves a video segment to another nest, it records a receipt
//! here. Credit scoring is deferred to a later phase.
//!
//! Two tables are maintained:
//! - `delivery_receipts`: peer-to-peer delivery records (binary nest IDs).
//! - `video_cache_sources`: URL-based source registry for cache-on-fetch.

use super::{CacheDb, blob_col_to_array};
use anyhow::{Context, Result};

/// A single delivery receipt row (peer-to-peer, binary nest IDs).
pub struct DeliveryReceiptRow {
    pub content_hash: Vec<u8>,
    pub server_nest: [u8; 32],
    pub requesting_nest: [u8; 32],
    pub bytes_served: u64,
    pub created_at: i64,
}

/// A cache source row — a remote nest URL known to have a given content hash.
pub struct CacheSourceRow {
    pub nest_url: String,
    pub bytes_served: u64,
    pub created_at: i64,
}

impl CacheDb {
    /// Record that this nest served `bytes_served` bytes of `content_hash`
    /// to `requesting_nest` (peer-to-peer, binary nest IDs).
    pub async fn record_peer_delivery_receipt(
        &self,
        content_hash: &[u8],
        server_nest: &[u8; 32],
        requesting_nest: &[u8; 32],
        bytes_served: u64,
    ) -> Result<()> {
        let content_hash = content_hash.to_vec();
        let server_nest = *server_nest;
        let requesting_nest = *requesting_nest;
        let now = super::now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO delivery_receipts (content_hash, server_nest, requesting_nest, bytes_served, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                content_hash,
                server_nest.as_slice(),
                requesting_nest.as_slice(),
                bytes_served as i64,
                now,
            ],
        )
        .context("record peer delivery receipt")?;
        Ok(())
    }

    /// List all delivery receipts for a given content hash, most recent first.
    pub async fn list_delivery_receipts_for_content(
        &self,
        content_hash: &[u8],
    ) -> Result<Vec<DeliveryReceiptRow>> {
        let content_hash = content_hash.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT content_hash, server_nest, requesting_nest, bytes_served, created_at
             FROM delivery_receipts
             WHERE content_hash = ?1
             ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![content_hash], |row| {
                Ok(DeliveryReceiptRow {
                    content_hash: row.get(0)?,
                    server_nest: blob_col_to_array(row.get(1)?, 1, "server_nest")?,
                    requesting_nest: blob_col_to_array(row.get(2)?, 2, "requesting_nest")?,
                    bytes_served: row.get::<_, i64>(3)? as u64,
                    created_at: row.get(4)?,
                })
            })
            .context("list delivery receipts")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Record that a remote nest at `nest_url` served `bytes_served` bytes of
    /// `content_hash` at `created_at` (epoch millis). Uses INSERT OR REPLACE so
    /// repeated observations for the same (hash, url) pair update the record.
    pub async fn record_delivery_receipt(
        &self,
        content_hash: &[u8; 32],
        nest_url: &str,
        bytes_served: u64,
        created_at: i64,
    ) -> Result<()> {
        let hash_slice = content_hash.as_slice().to_vec();
        let nest_url = nest_url.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO video_cache_sources (content_hash, nest_url, bytes_served, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(content_hash, nest_url)
             DO UPDATE SET bytes_served = excluded.bytes_served,
                           created_at  = excluded.created_at",
            rusqlite::params![hash_slice, nest_url, bytes_served as i64, created_at],
        )
        .context("record delivery receipt (url-based)")?;
        Ok(())
    }

    /// Return all known remote nests that have served the given content hash,
    /// ordered by most-recently-seen first. Used by the cache-on-fetch path.
    pub async fn get_delivery_sources(
        &self,
        content_hash: &[u8; 32],
    ) -> Result<Vec<CacheSourceRow>> {
        let hash_slice = content_hash.as_slice().to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT nest_url, bytes_served, created_at
             FROM video_cache_sources
             WHERE content_hash = ?1
             ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![hash_slice], |row| {
                Ok(CacheSourceRow {
                    nest_url: row.get(0)?,
                    bytes_served: row.get::<_, i64>(1)? as u64,
                    created_at: row.get(2)?,
                })
            })
            .context("get delivery sources")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}
