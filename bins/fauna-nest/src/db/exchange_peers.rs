//! Prior exchange partners for the federation exchange originator plane
//! (`federation.md` § the exchange originator plane).
//!
//! A partner row is recorded at **origination** time only, after a successful
//! exchange cycle with a peer — the channel handshake carries no origin URL,
//! so an inbound peer cannot be recorded (it records us when its own
//! originator dials back; data still flows both ways in one cycle because the
//! originator both pushes `*.exchange` and pulls `*.export`). The table is a
//! bounded memory: most-recent-success rows are kept, the tail evicted.

use anyhow::{Context, Result};

use super::{CacheDb, now_epoch_millis};

/// Bound on remembered exchange partners (most-recent-success kept). Far above
/// any plausible honest peer set; a hard stop against unbounded growth if this
/// nest is fed an endless stream of contributor URLs.
pub const MAX_EXCHANGE_PEERS: i64 = 256;

impl CacheDb {
    /// Record (or refresh) a peer this nest just successfully exchanged with.
    /// `nest_url` is the normalized base URL the originator dialed;
    /// `peer_nest_id` the channel-verified peer identity.
    pub async fn record_exchange_peer(
        &self,
        nest_url: &str,
        peer_nest_id: &[u8; 32],
    ) -> Result<()> {
        let url = nest_url.trim_end_matches('/').to_string();
        let peer = *peer_nest_id;
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        conn.execute(
            "INSERT INTO exchange_peers (nest_url, nest_id, last_success_at, created_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(nest_url) DO UPDATE SET
                nest_id = excluded.nest_id,
                last_success_at = excluded.last_success_at",
            rusqlite::params![url, &peer[..], now],
        )
        .context("upsert exchange peer")?;
        conn.execute(
            "DELETE FROM exchange_peers
             WHERE nest_url NOT IN (
                 SELECT nest_url FROM exchange_peers
                 ORDER BY last_success_at DESC
                 LIMIT ?1)",
            rusqlite::params![MAX_EXCHANGE_PEERS],
        )
        .context("enforce exchange-peer cap")?;
        Ok(())
    }

    /// All remembered partner URLs, most-recent-success first.
    pub async fn list_exchange_peer_urls(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT nest_url FROM exchange_peers
                 ORDER BY last_success_at DESC",
            )
            .context("prepare list exchange peers")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("query exchange peers")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect exchange peers")?;
        Ok(rows)
    }

    /// Distinct contributor nest URLs across every feed — the discovery-derived
    /// peer source for the exchange originator's peer set.
    pub async fn list_contributor_nest_urls(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT DISTINCT nest_url FROM feed_contributors")
            .context("prepare distinct contributor nests")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("query distinct contributor nests")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect distinct contributor nests")?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_lists_most_recent_first_and_caps() {
        let db = CacheDb::open_in_memory().unwrap();
        db.record_exchange_peer("https://a.test/", &[1u8; 32])
            .await
            .unwrap();
        db.record_exchange_peer("https://b.test", &[2u8; 32])
            .await
            .unwrap();
        // Refresh a — it becomes most recent, and the trailing slash was
        // normalized away so this hits the same row.
        db.record_exchange_peer("https://a.test", &[1u8; 32])
            .await
            .unwrap();
        let urls = db.list_exchange_peer_urls().await.unwrap();
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://a.test");

        // The cap holds: after MAX_EXCHANGE_PEERS + 10 inserts, only the cap
        // remains (oldest-success evicted).
        for i in 0..(MAX_EXCHANGE_PEERS + 10) {
            db.record_exchange_peer(&format!("https://peer{i}.test"), &[3u8; 32])
                .await
                .unwrap();
        }
        let urls = db.list_exchange_peer_urls().await.unwrap();
        assert_eq!(urls.len(), MAX_EXCHANGE_PEERS as usize);
    }

    #[tokio::test]
    async fn contributor_urls_are_distinct_across_feeds() {
        let db = CacheDb::open_in_memory().unwrap();
        db.upsert_contributor("f1", "https://n1.test", None, "manual")
            .await
            .unwrap();
        db.upsert_contributor("f2", "https://n1.test", None, "manual")
            .await
            .unwrap();
        db.upsert_contributor("f1", "https://n2.test", None, "manual")
            .await
            .unwrap();
        let mut urls = db.list_contributor_nest_urls().await.unwrap();
        urls.sort();
        assert_eq!(urls, vec!["https://n1.test", "https://n2.test"]);
    }
}
