//! SQLite routing table for fauna-router.
//!
//! Maps actor IDs (32-byte blobs) to the nest that hosts them and the
//! user's handle at that nest.  All operations are synchronous and
//! protected by a `Mutex<Connection>` so the struct is `Send + Sync`
//! without requiring an async runtime.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// Schema DDL applied at open time.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS routes (
    actor_id    BLOB PRIMARY KEY,
    nest_id     BLOB NOT NULL,
    handle      TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_routes_handle ON routes(handle) WHERE handle != '';
CREATE INDEX IF NOT EXISTS idx_routes_nest ON routes(nest_id);
";

/// Thread-safe wrapper around a SQLite connection holding the proxy
/// routing table.
pub struct ProxyDb {
    conn: Mutex<Connection>,
}

impl ProxyDb {
    /// Open (or create) a database file at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).context("open sqlite db")?;
        apply_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database.  Useful for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory sqlite db")?;
        apply_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert a route.  `actor_id` and `nest_id` are raw bytes; `handle`
    /// is the plain-text handle string (may be empty for anonymous entries).
    ///
    /// Fails if `actor_id` already exists or `handle` is already taken by
    /// a different actor (SQLite unique constraint).
    ///
    /// No production path calls this today: its only caller was the removed
    /// HTTP register route, and the roaming-nest WS-RPC frontend that will
    /// onboard actors again is not yet designed (`nest/worker.md` § `fauna-router`).
    pub fn insert_route(&self, actor_id: &[u8], nest_id: &[u8], handle: &str) -> Result<()> {
        let now = unix_now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO routes (actor_id, nest_id, handle, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![actor_id, nest_id, handle, now],
        )
        .context("insert route")?;
        Ok(())
    }

    /// Look up which nest hosts `actor_id`.
    ///
    /// Returns `Some((nest_id_bytes, handle))` if found.
    pub fn lookup_route(&self, actor_id: &[u8]) -> Result<Option<(Vec<u8>, String)>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT nest_id, handle FROM routes WHERE actor_id = ?1",
                params![actor_id],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .context("lookup route")?;
        Ok(row)
    }

    /// Return the number of routes that belong to `nest_id`.
    pub fn count_by_nest(&self, nest_id: &[u8]) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM routes WHERE nest_id = ?1",
                params![nest_id],
                |row| row.get(0),
            )
            .context("count_by_nest")?;
        Ok(count as u64)
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("apply schema")?;
    Ok(())
}

fn unix_now() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(n: u8) -> Vec<u8> {
        vec![n; 32]
    }

    fn nest(n: u8) -> Vec<u8> {
        vec![n; 32]
    }

    #[test]
    fn insert_and_lookup() {
        let db = ProxyDb::open_in_memory().unwrap();

        let a = actor(1);
        let nid = nest(10);

        db.insert_route(&a, &nid, "alice").unwrap();

        let result = db.lookup_route(&a).unwrap().expect("route should exist");
        assert_eq!(result.0, nid);
        assert_eq!(result.1, "alice");

        // Non-existent actor returns None.
        assert!(db.lookup_route(&actor(99)).unwrap().is_none());
    }

    #[test]
    fn handle_uniqueness() {
        let db = ProxyDb::open_in_memory().unwrap();

        db.insert_route(&actor(1), &nest(1), "alice").unwrap();

        // Inserting a second actor with the same handle must fail.
        let err = db.insert_route(&actor(2), &nest(1), "alice");
        assert!(err.is_err(), "duplicate handle should be rejected");

        // But a different handle on the same nest is fine.
        db.insert_route(&actor(2), &nest(1), "bob").unwrap();
    }

    #[test]
    fn count_by_nest_works() {
        let db = ProxyDb::open_in_memory().unwrap();

        let n1 = nest(1);
        let n2 = nest(2);

        db.insert_route(&actor(1), &n1, "alice").unwrap();
        db.insert_route(&actor(2), &n1, "bob").unwrap();
        db.insert_route(&actor(3), &n2, "carol").unwrap();

        assert_eq!(db.count_by_nest(&n1).unwrap(), 2);
        assert_eq!(db.count_by_nest(&n2).unwrap(), 1);
        assert_eq!(db.count_by_nest(&nest(99)).unwrap(), 0);
    }
}
