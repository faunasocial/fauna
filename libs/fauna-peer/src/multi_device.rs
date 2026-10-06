//! Per-device endpoint tracking for multi-device contacts.
//!
//! A single P2P contact (e.g. Bob) may own several devices — phone, laptop, etc.
//! This module persists each device's endpoint state so the dialler can try
//! every known device when reaching Bob. Devices carry no separate substrate
//! key: under iroh the `NodeId` *is* the Ed25519 actor key, so the WG-era
//! per-device `wg_public_key` was removed 2026-08-24 (`p2p.md` § Architecture).

use anyhow::Result;
use rusqlite::{Connection, params};

/// One device endpoint record for a contact.
#[derive(Debug, Clone)]
pub struct DeviceEndpoint {
    pub contact_actor_id: [u8; 32],
    pub device_id: String,
    pub last_endpoint: Option<String>,
    pub tunnel_ip: Option<String>,
    pub last_connected: Option<i64>,
    pub success_rate: f32,
    pub backoff_level: u8,
}

/// SQLite-backed store for per-device endpoint state.
pub struct DeviceDb {
    conn: Connection,
}

impl DeviceDb {
    /// Open (or create) a device database at `path`.
    /// Pass `":memory:"` in tests for an in-process database.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS device_endpoints (
                contact_actor_id  BLOB NOT NULL,
                device_id         TEXT NOT NULL,
                last_endpoint     TEXT,
                tunnel_ip         TEXT,
                last_connected    INTEGER,
                success_rate      REAL NOT NULL DEFAULT 0.0,
                backoff_level     INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (contact_actor_id, device_id)
            );",
        )?;
        Ok(Self { conn })
    }

    /// Insert or replace a device record for `contact`.
    ///
    /// Fields not provided here (`tunnel_ip`, `last_connected`, `success_rate`,
    /// `backoff_level`) are set to their default values on insert, or preserved
    /// via a SELECT+INSERT pattern — we use `INSERT OR REPLACE` which resets
    /// them, matching the task spec that treats each upsert as a fresh record
    /// unless the caller explicitly updates those fields separately.
    pub fn upsert_device(
        &self,
        contact: &[u8; 32],
        device_id: &str,
        endpoint: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO device_endpoints
                (contact_actor_id, device_id, last_endpoint,
                 tunnel_ip, last_connected, success_rate, backoff_level)
             VALUES (?1, ?2, ?3, NULL, NULL, 0.0, 0)",
            params![contact.as_slice(), device_id, endpoint,],
        )?;
        Ok(())
    }

    /// Return all device records for `contact`.
    pub fn devices_for(&self, contact: &[u8; 32]) -> Result<Vec<DeviceEndpoint>> {
        let mut stmt = self.conn.prepare(
            "SELECT contact_actor_id, device_id, last_endpoint,
                    tunnel_ip, last_connected, success_rate, backoff_level
             FROM device_endpoints
             WHERE contact_actor_id = ?1
             ORDER BY device_id",
        )?;
        let rows = stmt
            .query_map(params![contact.as_slice()], row_to_device)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Return only those device records for `contact` that have a known
    /// `last_endpoint` (i.e. we have at least one address to try).
    pub fn reachable_devices(&self, contact: &[u8; 32]) -> Result<Vec<DeviceEndpoint>> {
        let mut stmt = self.conn.prepare(
            "SELECT contact_actor_id, device_id, last_endpoint,
                    tunnel_ip, last_connected, success_rate, backoff_level
             FROM device_endpoints
             WHERE contact_actor_id = ?1
               AND last_endpoint IS NOT NULL
             ORDER BY device_id",
        )?;
        let rows = stmt
            .query_map(params![contact.as_slice()], row_to_device)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Remove a single device record.
    pub fn remove_device(&self, contact: &[u8; 32], device_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM device_endpoints
             WHERE contact_actor_id = ?1 AND device_id = ?2",
            params![contact.as_slice(), device_id],
        )?;
        Ok(())
    }

    /// Remove all device records for `contact`.
    pub fn remove_all_devices(&self, contact: &[u8; 32]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM device_endpoints WHERE contact_actor_id = ?1",
            params![contact.as_slice()],
        )?;
        Ok(())
    }
}

fn row_to_device(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceEndpoint> {
    let contact_blob: Vec<u8> = row.get(0)?;
    let success_rate_f64: f64 = row.get(5)?;
    let backoff_int: i32 = row.get(6)?;

    let contact_actor_id: [u8; 32] = contact_blob.try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(
            0,
            "contact_actor_id".into(),
            rusqlite::types::Type::Blob,
        )
    })?;

    Ok(DeviceEndpoint {
        contact_actor_id,
        device_id: row.get(1)?,
        last_endpoint: row.get(2)?,
        tunnel_ip: row.get(3)?,
        last_connected: row.get(4)?,
        success_rate: success_rate_f64 as f32,
        backoff_level: backoff_int as u8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_mem() -> DeviceDb {
        DeviceDb::open(":memory:").unwrap()
    }

    const CONTACT_A: [u8; 32] = [1u8; 32];

    #[test]
    fn add_and_list_devices() {
        let db = open_mem();
        db.upsert_device(&CONTACT_A, "phone", Some("1.2.3.4:51820"))
            .unwrap();
        db.upsert_device(&CONTACT_A, "laptop", None).unwrap();

        let devices = db.devices_for(&CONTACT_A).unwrap();
        assert_eq!(
            devices.len(),
            2,
            "expected 2 devices, got {}",
            devices.len()
        );

        let ids: Vec<&str> = devices.iter().map(|d| d.device_id.as_str()).collect();
        assert!(ids.contains(&"phone"));
        assert!(ids.contains(&"laptop"));
    }

    #[test]
    fn remove_device() {
        let db = open_mem();
        db.upsert_device(&CONTACT_A, "phone", Some("1.2.3.4:51820"))
            .unwrap();

        db.remove_device(&CONTACT_A, "phone").unwrap();

        let devices = db.devices_for(&CONTACT_A).unwrap();
        assert_eq!(devices.len(), 0, "device list should be empty after remove");
    }

    #[test]
    fn remove_all_devices_for_contact() {
        let db = open_mem();
        db.upsert_device(&CONTACT_A, "phone", Some("1.2.3.4:51820"))
            .unwrap();
        db.upsert_device(&CONTACT_A, "laptop", None).unwrap();

        db.remove_all_devices(&CONTACT_A).unwrap();

        let devices = db.devices_for(&CONTACT_A).unwrap();
        assert_eq!(
            devices.len(),
            0,
            "all devices should be gone after remove_all_devices"
        );
    }

    #[test]
    fn reachable_devices() {
        let db = open_mem();
        // phone has a known endpoint; laptop does not
        db.upsert_device(&CONTACT_A, "phone", Some("1.2.3.4:51820"))
            .unwrap();
        db.upsert_device(&CONTACT_A, "laptop", None).unwrap();

        let reachable = db.reachable_devices(&CONTACT_A).unwrap();
        assert_eq!(
            reachable.len(),
            1,
            "only 1 device has a known endpoint, got {}",
            reachable.len()
        );
        assert_eq!(reachable[0].device_id, "phone");
        assert_eq!(reachable[0].last_endpoint.as_deref(), Some("1.2.3.4:51820"));
    }
}
