//! Connection quality tracking per P2P peer.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionPath {
    Lan,
    WanDirect,
    Relay,
}

#[derive(Debug, Clone)]
pub struct ConnectionQuality {
    pub peer_actor_id: [u8; 32],
    pub path: ConnectionPath,
    pub latency_ms: u32,
    pub bandwidth_bps: u64,
    pub measured_at: i64,
}

pub struct QualityDb {
    conn: Connection,
}

impl QualityDb {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS connection_quality (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                peer_actor_id BLOB NOT NULL,
                path          TEXT NOT NULL,
                latency_ms    INTEGER NOT NULL,
                bandwidth_bps INTEGER NOT NULL,
                measured_at   INTEGER NOT NULL
            );",
        )?;
        Ok(Self { conn })
    }

    pub fn record(
        &self,
        peer: &[u8; 32],
        path: ConnectionPath,
        latency_ms: u32,
        bandwidth_bps: u64,
    ) -> Result<()> {
        let now = now_secs();
        let path_str = path_to_str(path);
        self.conn.execute(
            "INSERT INTO connection_quality
             (peer_actor_id, path, latency_ms, bandwidth_bps, measured_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                peer.as_slice(),
                path_str,
                latency_ms as i64,
                bandwidth_bps as i64,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn latest(&self, peer: &[u8; 32]) -> Result<Option<ConnectionQuality>> {
        let result = self
            .conn
            .query_row(
                "SELECT peer_actor_id, path, latency_ms, bandwidth_bps, measured_at
                 FROM connection_quality
                 WHERE peer_actor_id = ?1
                 ORDER BY measured_at DESC
                 LIMIT 1",
                params![peer.as_slice()],
                row_to_quality,
            )
            .optional()?;
        Ok(result)
    }
}

fn path_to_str(path: ConnectionPath) -> &'static str {
    match path {
        ConnectionPath::Lan => "lan",
        ConnectionPath::WanDirect => "wan_direct",
        ConnectionPath::Relay => "relay",
    }
}

fn str_to_path(s: &str) -> ConnectionPath {
    match s {
        "lan" => ConnectionPath::Lan,
        "wan_direct" => ConnectionPath::WanDirect,
        _ => ConnectionPath::Relay,
    }
}

fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

fn row_to_quality(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConnectionQuality> {
    let peer_blob: Vec<u8> = row.get(0)?;
    let peer_actor_id: [u8; 32] = peer_blob.try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(0, "peer_actor_id".into(), rusqlite::types::Type::Blob)
    })?;
    let path_str: String = row.get(1)?;
    let latency_ms_i64: i64 = row.get(2)?;
    let bandwidth_bps_i64: i64 = row.get(3)?;
    Ok(ConnectionQuality {
        peer_actor_id,
        path: str_to_path(&path_str),
        latency_ms: latency_ms_i64 as u32,
        bandwidth_bps: bandwidth_bps_i64 as u64,
        measured_at: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_db() -> QualityDb {
        QualityDb::open(":memory:").unwrap()
    }

    #[test]
    fn record_and_query_quality() {
        let db = open_db();
        let peer = [1u8; 32];
        db.record(&peer, ConnectionPath::Lan, 5, 100_000_000)
            .unwrap();

        let q = db.latest(&peer).unwrap().expect("should have a record");
        assert_eq!(q.peer_actor_id, peer);
        assert_eq!(q.path, ConnectionPath::Lan);
        assert_eq!(q.latency_ms, 5);
        assert_eq!(q.bandwidth_bps, 100_000_000);
    }

    #[test]
    fn latest_returns_most_recent() {
        let db = open_db();
        let peer = [2u8; 32];

        // Insert relay first with an older timestamp by manipulating measured_at directly.
        // We use raw SQL so we can set explicit measured_at values.
        db.conn
            .execute(
                "INSERT INTO connection_quality (peer_actor_id, path, latency_ms, bandwidth_bps, measured_at)
                 VALUES (?1, 'relay', 50, 1000000, 1000)",
                params![peer.as_slice()],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO connection_quality (peer_actor_id, path, latency_ms, bandwidth_bps, measured_at)
                 VALUES (?1, 'wan_direct', 20, 5000000, 2000)",
                params![peer.as_slice()],
            )
            .unwrap();

        let q = db.latest(&peer).unwrap().expect("should have a record");
        assert_eq!(q.path, ConnectionPath::WanDirect);
        assert_eq!(q.measured_at, 2000);
    }

    #[test]
    fn no_quality_for_unknown_peer() {
        let db = open_db();
        let peer = [3u8; 32];
        let result = db.latest(&peer).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn row_to_quality_errors_on_malformed_actor_id_blob() {
        let db = open_db();
        db.conn
            .execute(
                "INSERT INTO connection_quality (peer_actor_id, path, latency_ms, bandwidth_bps, measured_at)
                 VALUES (X'ffee', 'lan', 5, 100, 9999)",
                [],
            )
            .unwrap();

        let result: rusqlite::Result<ConnectionQuality> = db.conn.query_row(
            "SELECT peer_actor_id, path, latency_ms, bandwidth_bps, measured_at
             FROM connection_quality WHERE measured_at = 9999",
            [],
            row_to_quality,
        );

        assert!(
            result.is_err(),
            "a malformed (non-32-byte) peer_actor_id blob must not silently zero-fill"
        );
    }
}
