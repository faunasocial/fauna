use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// A peer contact representing another Fauna user for P2P communication.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerContact {
    #[serde(with = "serde_bytes")]
    pub actor_id: [u8; 32],
    pub display_name: String,
    pub p2p_enabled: bool,
    pub last_endpoint: Option<String>,
    pub last_connected: Option<i64>,
    pub success_rate: f32,
    pub backoff_level: u8,
    pub met_in_person: bool,
    /// The peer's IP address inside the WireGuard tunnel (e.g. "10.0.0.2").
    pub tunnel_ip: Option<String>,
    /// LAN endpoints advertised by the peer (JSON-encoded list of "ip:port" strings).
    pub lan_endpoints: Vec<String>,
    /// Peer's STUN-discovered public endpoint for hole punching.
    pub stun_endpoint: Option<String>,
    /// Whether to sync this peer's feed posts over the P2P tunnel.
    pub feed_sync_enabled: bool,
}

/// SQLite-backed storage for peer contacts.
pub struct PeerDb {
    conn: Connection,
}

impl PeerDb {
    /// Open (or create) a peer database at the given path.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS peer_contacts (
                actor_id         BLOB PRIMARY KEY,
                display_name     TEXT NOT NULL,
                p2p_enabled      INTEGER NOT NULL DEFAULT 1,
                last_endpoint    TEXT,
                last_connected   INTEGER,
                success_rate     REAL NOT NULL DEFAULT 0.0,
                backoff_level    INTEGER NOT NULL DEFAULT 0,
                met_in_person    INTEGER NOT NULL DEFAULT 0,
                tunnel_ip        TEXT,
                lan_endpoints    TEXT,
                stun_endpoint    TEXT,
                feed_sync_enabled INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(Self { conn })
    }

    /// Insert or replace a peer contact.
    pub fn upsert_contact(&self, contact: &PeerContact) -> Result<()> {
        let lan_json = serde_json::to_string(&contact.lan_endpoints).unwrap_or_default();
        self.conn.execute(
            "INSERT OR REPLACE INTO peer_contacts (
                actor_id, display_name, p2p_enabled,
                last_endpoint, last_connected, success_rate, backoff_level, met_in_person,
                tunnel_ip, lan_endpoints, stun_endpoint, feed_sync_enabled
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                contact.actor_id.as_slice(),
                contact.display_name,
                contact.p2p_enabled as i32,
                contact.last_endpoint,
                contact.last_connected,
                contact.success_rate as f64,
                contact.backoff_level as i32,
                contact.met_in_person as i32,
                contact.tunnel_ip,
                lan_json,
                contact.stun_endpoint,
                contact.feed_sync_enabled as i32,
            ],
        )?;
        Ok(())
    }

    /// Get a peer contact by actor_id.
    pub fn get_contact(&self, actor_id: &[u8; 32]) -> Result<Option<PeerContact>> {
        let result = self
            .conn
            .query_row(
                "SELECT actor_id, display_name, p2p_enabled,
                        last_endpoint, last_connected, success_rate, backoff_level, met_in_person,
                        tunnel_ip, lan_endpoints, stun_endpoint, feed_sync_enabled
                 FROM peer_contacts WHERE actor_id = ?1",
                params![actor_id.as_slice()],
                row_to_contact,
            )
            .optional()?;
        Ok(result)
    }

    /// List all contacts with p2p_enabled = true.
    pub fn list_p2p_enabled(&self) -> Result<Vec<PeerContact>> {
        let mut stmt = self.conn.prepare(
            "SELECT actor_id, display_name, p2p_enabled,
                    last_endpoint, last_connected, success_rate, backoff_level, met_in_person,
                    tunnel_ip, lan_endpoints, stun_endpoint, feed_sync_enabled
             FROM peer_contacts WHERE p2p_enabled = 1",
        )?;
        let contacts = stmt
            .query_map([], row_to_contact)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(contacts)
    }

    /// Delete a contact by actor_id.
    pub fn delete_contact(&self, actor_id: &[u8; 32]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM peer_contacts WHERE actor_id = ?1",
            params![actor_id.as_slice()],
        )?;
        Ok(())
    }

    /// Update the last known working endpoint for a contact.
    pub fn update_last_endpoint(&self, actor_id: &[u8; 32], endpoint: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE peer_contacts SET last_endpoint = ?1 WHERE actor_id = ?2",
            params![endpoint, actor_id.as_slice()],
        )?;
        Ok(())
    }

    /// Update backoff state after a probe attempt.
    pub fn record_probe_result(&self, actor_id: &[u8; 32], success: bool) -> Result<()> {
        if success {
            // Reset backoff to 0, bump success rate
            self.conn.execute(
                "UPDATE peer_contacts SET last_connected = unixepoch(), backoff_level = 0,
                 success_rate = MIN(1.0, success_rate + 0.05) WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
            )?;
        } else {
            // Decrease success rate, recompute backoff level
            self.conn.execute(
                "UPDATE peer_contacts SET success_rate = MAX(0.0, success_rate - 0.05) WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
            )?;
            let rate: f64 = self.conn.query_row(
                "SELECT success_rate FROM peer_contacts WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get(0),
            )?;
            let level = crate::backoff::compute_backoff_level(rate as f32);
            self.conn.execute(
                "UPDATE peer_contacts SET backoff_level = ?1 WHERE actor_id = ?2",
                rusqlite::params![level, actor_id.as_slice()],
            )?;
        }
        Ok(())
    }

    /// Return contacts whose backoff timer has expired.
    pub fn contacts_ready_to_probe(&self) -> Result<Vec<PeerContact>> {
        let now = fauna_core::data::Timestamp::now_secs_or_zero();
        let contacts = self.list_p2p_enabled()?;
        Ok(contacts
            .into_iter()
            .filter(|c| {
                let interval = crate::backoff::probe_interval(c.backoff_level);
                if interval == std::time::Duration::MAX {
                    return false;
                }
                let last = c.last_connected.unwrap_or(0);
                // Signed first, then narrow — never `(now - last) as u64`. A
                // `last_connected` in the FUTURE makes the difference negative,
                // and that cast wraps it to ~1.8e19, which reads as *maximally
                // overdue*: backoff is bypassed and the contact is probed on
                // every tick. Two ordinary ways to get there — the clock
                // stepping backwards, and `now_secs_or_zero()` folding a failed
                // clock read to `0`, which puts every stored contact in the
                // future at once, precisely when the device is already degraded.
                // A negative elapsed means "not yet", the same semantics the
                // feed-sync twin pins (`feed_sync::contacts_needing_sync`).
                let Ok(elapsed) = u64::try_from(now.saturating_sub(last)) else {
                    return false;
                };
                elapsed >= interval.as_secs()
            })
            .collect())
    }
}

/// Extract a `PeerContact` from a SQLite row.
fn row_to_contact(row: &rusqlite::Row<'_>) -> rusqlite::Result<PeerContact> {
    let actor_id_blob: Vec<u8> = row.get(0)?;
    let p2p_int: i32 = row.get(2)?;
    let success_rate_f64: f64 = row.get(5)?;
    let backoff_int: i32 = row.get(6)?;
    let met_int: i32 = row.get(7)?;

    let actor_id: [u8; 32] = actor_id_blob.try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(0, "actor_id".into(), rusqlite::types::Type::Blob)
    })?;

    // Late columns (indices 8, 9, 10, 11) — may be NULL.
    let tunnel_ip: Option<String> = row.get(8)?;
    let lan_json: Option<String> = row.get(9)?;
    let stun_endpoint: Option<String> = row.get(10)?;
    let feed_sync_int: i32 = row.get(11)?;

    let lan_endpoints: Vec<String> = lan_json
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();

    Ok(PeerContact {
        actor_id,
        display_name: row.get(1)?,
        p2p_enabled: p2p_int != 0,
        last_endpoint: row.get(3)?,
        last_connected: row.get(4)?,
        success_rate: success_rate_f64 as f32,
        backoff_level: backoff_int as u8,
        met_in_person: met_int != 0,
        tunnel_ip,
        lan_endpoints,
        stun_endpoint,
        feed_sync_enabled: feed_sync_int != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_contact(id_byte: u8, name: &str, p2p: bool) -> PeerContact {
        let mut actor_id = [0u8; 32];
        actor_id[0] = id_byte;
        PeerContact {
            actor_id,
            display_name: name.to_string(),
            p2p_enabled: p2p,
            last_endpoint: Some("192.168.1.1:51820".to_string()),
            last_connected: Some(1700000000),
            success_rate: 0.95,
            backoff_level: 0,
            met_in_person: true,
            tunnel_ip: None,
            lan_endpoints: vec![],
            stun_endpoint: None,
            feed_sync_enabled: false,
        }
    }

    /// A contact whose `last_connected` sits in the FUTURE is not due for a
    /// probe — its backoff window cannot have elapsed yet. This is the same
    /// semantics the feed-sync twin already pins
    /// (`feed_sync::contacts_needing_sync`, whose test names the far-future
    /// timestamp explicitly): the comparison is signed, so "negative elapsed"
    /// reads as *not yet*, never as *long overdue*.
    ///
    /// Two ways a future value is reached in production, neither exotic: the
    /// device clock stepping backwards (NTP correction, VM suspend/resume,
    /// dual boot), and — the wider one — `Timestamp::now_secs_or_zero()`
    /// folding a FAILED clock read to `0`, which puts every stored contact in
    /// the future at once.
    #[test]
    fn a_future_last_connected_is_not_ready_to_probe() {
        let tmp = TempDir::new().unwrap();
        let store = PeerDb::open(tmp.path().join("peers.db")).unwrap();

        let mut future = make_contact(1, "future", true);
        // Comfortably ahead of any wall clock this test can observe, and of a
        // `now` that folded to 0.
        future.last_connected = Some(i64::MAX / 2);
        future.backoff_level = 0; // 30s window — would be long past if elapsed were positive
        store.upsert_contact(&future).unwrap();

        let mut stale = make_contact(2, "stale", true);
        stale.last_connected = Some(0); // epoch: genuinely overdue
        stale.backoff_level = 0;
        store.upsert_contact(&stale).unwrap();

        let ready = store.contacts_ready_to_probe().unwrap();
        let ids: Vec<u8> = ready.iter().map(|c| c.actor_id[0]).collect();

        assert!(
            ids.contains(&2),
            "a contact last connected at the epoch is overdue and must be probed; got {ids:?}"
        );
        assert!(
            !ids.contains(&1),
            "a contact whose last_connected is in the FUTURE must NOT be probed: its \
             backoff window has not elapsed. A signed difference cast to u64 wraps to \
             ~1.8e19 here, which reads as *maximally overdue* and bypasses backoff \
             entirely — probing it on every single tick. Got {ids:?}"
        );
    }

    #[test]
    fn insert_and_get_contact() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let contact = make_contact(1, "Alice", true);
        db.upsert_contact(&contact).unwrap();

        let fetched = db.get_contact(&contact.actor_id).unwrap();
        assert!(fetched.is_some(), "contact should exist after insert");
        let fetched = fetched.unwrap();
        assert_eq!(fetched.actor_id, contact.actor_id);
        assert_eq!(fetched.display_name, "Alice");
        assert!(fetched.p2p_enabled);
        assert_eq!(fetched.last_endpoint.as_deref(), Some("192.168.1.1:51820"));
        assert_eq!(fetched.last_connected, Some(1700000000));
        assert!((fetched.success_rate - 0.95).abs() < f32::EPSILON);
        assert_eq!(fetched.backoff_level, 0);
        assert!(fetched.met_in_person);
    }

    #[test]
    fn list_p2p_enabled_contacts() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let alice = make_contact(1, "Alice", true);
        let bob = make_contact(2, "Bob", false);
        let carol = make_contact(3, "Carol", true);

        db.upsert_contact(&alice).unwrap();
        db.upsert_contact(&bob).unwrap();
        db.upsert_contact(&carol).unwrap();

        let enabled = db.list_p2p_enabled().unwrap();
        assert_eq!(enabled.len(), 2, "only Alice and Carol have p2p enabled");

        let names: Vec<&str> = enabled.iter().map(|c| c.display_name.as_str()).collect();
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Carol"));
        assert!(!names.contains(&"Bob"));
    }

    #[test]
    fn delete_contact() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let contact = make_contact(1, "Alice", true);
        db.upsert_contact(&contact).unwrap();

        // Verify it exists
        assert!(db.get_contact(&contact.actor_id).unwrap().is_some());

        // Delete
        db.delete_contact(&contact.actor_id).unwrap();

        // Verify it's gone
        assert!(
            db.get_contact(&contact.actor_id).unwrap().is_none(),
            "contact should be gone after delete"
        );
    }

    #[test]
    fn upsert_updates_existing() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let mut contact = make_contact(1, "Alice", true);
        db.upsert_contact(&contact).unwrap();

        // Update display name and success_rate
        contact.display_name = "Alice Updated".to_string();
        contact.success_rate = 0.5;
        db.upsert_contact(&contact).unwrap();

        let fetched = db.get_contact(&contact.actor_id).unwrap().unwrap();
        assert_eq!(fetched.display_name, "Alice Updated");
        assert!((fetched.success_rate - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let missing_id = [0xFFu8; 32];
        let result = db.get_contact(&missing_id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_nonexistent_is_ok() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let missing_id = [0xFFu8; 32];
        // Should not error
        db.delete_contact(&missing_id).unwrap();
    }

    #[test]
    fn contacts_ready_to_probe_respects_backoff() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let mut c = make_contact(1, "Alice", true);
        c.backoff_level = 0;
        c.last_connected = Some(0); // long ago — will exceed 30s interval
        db.upsert_contact(&c).unwrap();

        let ready = db.contacts_ready_to_probe().unwrap();
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn dormant_contacts_not_probed() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let mut c = make_contact(1, "Alice", true);
        c.backoff_level = 4; // dormant — Duration::MAX
        db.upsert_contact(&c).unwrap();

        let ready = db.contacts_ready_to_probe().unwrap();
        assert_eq!(ready.len(), 0);
    }

    #[test]
    fn feed_sync_flag_persists() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let mut c = make_contact(1, "Alice", true);
        c.feed_sync_enabled = true;
        db.upsert_contact(&c).unwrap();
        let loaded = db.get_contact(&c.actor_id).unwrap().unwrap();
        assert!(loaded.feed_sync_enabled);
    }

    #[test]
    fn record_probe_success_resets_backoff() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("peers.db");
        let db = PeerDb::open(&db_path).unwrap();

        let mut c = make_contact(1, "Alice", true);
        c.backoff_level = 3;
        c.success_rate = 0.1;
        db.upsert_contact(&c).unwrap();

        db.record_probe_result(&c.actor_id, true).unwrap();

        let updated = db.get_contact(&c.actor_id).unwrap().unwrap();
        assert_eq!(updated.backoff_level, 0);
    }
}
