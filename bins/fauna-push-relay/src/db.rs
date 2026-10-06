use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;

/// How long a pending wake's **nonce** stays resolvable on
/// `/v1/endpoint/{nonce}`.
///
/// This is a capability lifetime, not a bookkeeping interval: anyone holding
/// the nonce can read and set that wake's responder endpoint, so it wants to be
/// short. Keep it that way — the rate limiter no longer depends on it.
pub const NONCE_TTL_SECS: u64 = 60;

/// How long a target stays protected from a second wake.
///
/// Deliberately **not** the same retention as the nonce above, and that is the
/// whole of row 145: the two used to share `pending_wakes`, so the cleanup task
/// deleted at 60 s the very rows `has_recent_wake` looked back 300 s for. The
/// limiter was a ~60 s limiter wearing a 300 s doc comment, and nothing
/// observed it, because the only test asserted an *immediate* duplicate was
/// refused — which passes at either value. `wake_history` now carries this
/// retention on its own.
///
/// Matched by `api::FRESHNESS_WINDOW_SECS` so the relay has one time constant
/// for "how long is a wake interesting", not two that drift apart.
pub const WAKE_RATE_LIMIT_SECS: u64 = 300;

/// The two retentions that used to be one must stay **ordered**: a
/// nonce is a bearer capability and expires first, rate-limit history outlives
/// it. Raising the nonce TTL to the limiter's window would quietly restore the
/// coupling this row exists to break — and extend a capability's life fivefold
/// while looking like a tidying commit. A compile-time assertion rather than a
/// test, because there is no configuration in which it should ever build.
const _: () = assert!(NONCE_TTL_SECS < WAKE_RATE_LIMIT_SECS);

/// Stored push token for an actor.
#[derive(Debug, Clone)]
pub struct PushToken {
    pub platform: String,
    pub push_token: String,
}

/// SQLite-backed storage for push tokens and pending wake requests.
pub struct RelayDb {
    conn: Mutex<Connection>,
}

impl RelayDb {
    /// Open (or create) the database at `path` and run migrations.
    pub fn open(path: PathBuf) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS push_tokens (
                actor_id      BLOB PRIMARY KEY,
                platform      TEXT NOT NULL,
                push_token    TEXT NOT NULL,
                updated_at    INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS pending_wakes (
                nonce              TEXT PRIMARY KEY,
                target_actor_id    BLOB NOT NULL,
                requester_endpoint TEXT NOT NULL,
                responder_endpoint TEXT,
                created_at         INTEGER NOT NULL
            );

            -- Rate-limit history, separate from the nonce above ONLY because
            -- the two need different retentions (row 145): a nonce is a bearer
            -- capability and wants a short life, while the limiter wants to
            -- remember a target for its full window. Sharing one table meant
            -- the nonce's cleanup silently truncated the limiter's memory.
            -- Additive: an existing relay database gains this table on open and
            -- loses nothing.
            CREATE TABLE IF NOT EXISTS wake_history (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                target_actor_id BLOB NOT NULL,
                created_at      INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS wake_history_target_time
                ON wake_history (target_actor_id, created_at);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert or update a push token for the given actor.
    pub fn upsert_token(
        &self,
        actor_id: &[u8; 32],
        platform: &str,
        push_token: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO push_tokens (actor_id, platform, push_token, updated_at)
             VALUES (?1, ?2, ?3, unixepoch())
             ON CONFLICT(actor_id) DO UPDATE SET
                platform = excluded.platform,
                push_token = excluded.push_token,
                updated_at = excluded.updated_at",
            rusqlite::params![actor_id.as_slice(), platform, push_token],
        )?;
        Ok(())
    }

    /// Look up the push token for an actor.
    pub fn get_token(&self, actor_id: &[u8; 32]) -> Result<Option<PushToken>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT platform, push_token FROM push_tokens WHERE actor_id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![actor_id.as_slice()])?;
        match rows.next()? {
            Some(row) => {
                let platform: String = row.get(0)?;
                let push_token_val: String = row.get(1)?;
                Ok(Some(PushToken {
                    platform,
                    push_token: push_token_val,
                }))
            }
            None => Ok(None),
        }
    }

    /// Delete the push token for an actor.
    pub fn delete_token(&self, actor_id: &[u8; 32]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM push_tokens WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
        )?;
        Ok(())
    }

    /// Create a pending wake request, returning the UUID nonce.
    pub fn create_wake(
        &self,
        target_actor_id: &[u8; 32],
        requester_endpoint: &str,
    ) -> Result<String> {
        let nonce = uuid::Uuid::new_v4().to_string();
        let mut conn = self.conn.lock().unwrap();
        // One transaction, because the two rows answer to different clocks but
        // must exist together: a crash between them would either hand out a
        // nonce the limiter never heard of (a free extra wake) or record a
        // limit against a wake that never happened.
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO pending_wakes (nonce, target_actor_id, requester_endpoint, created_at)
             VALUES (?1, ?2, ?3, unixepoch())",
            rusqlite::params![&nonce, target_actor_id.as_slice(), requester_endpoint],
        )?;
        tx.execute(
            "INSERT INTO wake_history (target_actor_id, created_at)
             VALUES (?1, unixepoch())",
            rusqlite::params![target_actor_id.as_slice()],
        )?;
        tx.commit()?;
        Ok(nonce)
    }

    /// Whether `target_actor_id` was woken within the last
    /// [`WAKE_RATE_LIMIT_SECS`] seconds.
    ///
    /// Reads `wake_history`, **not** `pending_wakes`: those rows are collected
    /// at [`NONCE_TTL_SECS`], so asking them about a 300 s window only ever
    /// returned the last ~60 s of it.
    pub fn has_recent_wake(&self, target_actor_id: &[u8; 32]) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM wake_history
             WHERE target_actor_id = ?1
               AND created_at >= unixepoch() - ?2",
            rusqlite::params![target_actor_id.as_slice(), WAKE_RATE_LIMIT_SECS as i64],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Set the responder endpoint for a pending wake.
    /// Returns whether a wake with that nonce existed — `false` means the
    /// report matched nothing (an unknown nonce, or one whose wake has since
    /// expired), which the route turns into a 404 rather than a 200 over a
    /// no-op. The distinction is what makes the nonce's capability boundary
    /// observable, and therefore assertable.
    pub fn set_responder_endpoint(&self, nonce: &str, endpoint: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE pending_wakes SET responder_endpoint = ?1 WHERE nonce = ?2",
            rusqlite::params![endpoint, nonce],
        )?;
        Ok(updated > 0)
    }

    /// Get the responder endpoint for a pending wake, if set.
    pub fn get_responder_endpoint(&self, nonce: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT responder_endpoint FROM pending_wakes WHERE nonce = ?1")?;
        let mut rows = stmt.query(rusqlite::params![nonce])?;
        match rows.next()? {
            Some(row) => {
                let endpoint: Option<String> = row.get(0)?;
                Ok(endpoint)
            }
            None => Ok(None),
        }
    }

    /// Collect both expiring planes, each at **its own** retention, and return
    /// the total rows deleted.
    ///
    /// Two windows on purpose: nonces expire at [`NONCE_TTL_SECS`]
    /// because a nonce is a bearer capability, and rate-limit history at
    /// [`WAKE_RATE_LIMIT_SECS`] because that is how long a target stays
    /// protected. Both are bounded — history is a rolling window, never a log
    /// of every wake the relay has ever seen.
    pub fn cleanup_expired_wakes(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let nonces = conn.execute(
            "DELETE FROM pending_wakes WHERE created_at < unixepoch() - ?1",
            rusqlite::params![NONCE_TTL_SECS as i64],
        )?;
        let history = conn.execute(
            "DELETE FROM wake_history WHERE created_at < unixepoch() - ?1",
            rusqlite::params![WAKE_RATE_LIMIT_SECS as i64],
        )?;
        Ok(nonces + history)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> RelayDb {
        RelayDb::open(PathBuf::from(":memory:")).unwrap()
    }

    #[test]
    fn register_and_lookup_push_token() {
        let db = make_db();
        let actor_id = [1u8; 32];

        db.upsert_token(&actor_id, "apns", "device_token_abc")
            .unwrap();

        let tok = db
            .get_token(&actor_id)
            .unwrap()
            .expect("token should exist");
        assert_eq!(tok.platform, "apns");
        assert_eq!(tok.push_token, "device_token_abc");

        // Upsert again with different platform
        db.upsert_token(&actor_id, "fcm", "new_token").unwrap();
        let tok = db
            .get_token(&actor_id)
            .unwrap()
            .expect("token should exist");
        assert_eq!(tok.platform, "fcm");
        assert_eq!(tok.push_token, "new_token");
    }

    #[test]
    fn delete_token() {
        let db = make_db();
        let actor_id = [3u8; 32];

        db.upsert_token(&actor_id, "apns", "tok").unwrap();
        assert!(db.get_token(&actor_id).unwrap().is_some());

        db.delete_token(&actor_id).unwrap();
        assert!(db.get_token(&actor_id).unwrap().is_none());
    }

    #[test]
    fn pending_wake_lifecycle() {
        let db = make_db();
        let target_id = [10u8; 32];

        // Create wake
        let nonce = db.create_wake(&target_id, "1.2.3.4:51820").unwrap();
        assert!(!nonce.is_empty());

        // No responder yet
        let resp = db.get_responder_endpoint(&nonce).unwrap();
        assert!(resp.is_none());

        // Set responder
        db.set_responder_endpoint(&nonce, "5.6.7.8:51820").unwrap();
        let resp = db.get_responder_endpoint(&nonce).unwrap();
        assert_eq!(resp.as_deref(), Some("5.6.7.8:51820"));

        // Cleanup should not delete fresh wakes
        let cleaned = db.cleanup_expired_wakes().unwrap();
        assert_eq!(cleaned, 0);
    }

    /// Backdate every record of a wake, so age can be asserted without a clock
    /// (e2e-conventions.md convention 14 — a `sleep` here would be a defunct
    /// test, and a 300s one unrunnable).
    fn backdate_all_wakes(db: &RelayDb, secs: u64) {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "UPDATE pending_wakes SET created_at = created_at - ?1",
            rusqlite::params![secs as i64],
        )
        .unwrap();
        conn.execute(
            "UPDATE wake_history SET created_at = created_at - ?1",
            rusqlite::params![secs as i64],
        )
        .unwrap();
    }

    /// **The pin.** A wake old enough that the nonce cleanup has run
    /// must still rate-limit the target.
    ///
    /// This failed before the split, and the way it failed is the whole point:
    /// `has_recent_wake` looked back `WAKE_RATE_LIMIT_SECS` (300) into
    /// `pending_wakes`, but the cleanup task deletes those rows at
    /// `NONCE_TTL_SECS` (60) — so the limiter could only ever see ~60s of
    /// history no matter what its own window said, and a replayed wake got a
    /// fresh push every minute rather than every five.
    #[test]
    fn a_wake_still_rate_limits_after_its_nonce_has_been_collected() {
        let db = make_db();
        let target_id = [30u8; 32];

        db.create_wake(&target_id, "1.2.3.4:51820").unwrap();
        assert!(db.has_recent_wake(&target_id).unwrap());

        // Age it past the nonce TTL but well inside the rate-limit window, then
        // run the cleanup exactly as the relay's own task does.
        backdate_all_wakes(&db, NONCE_TTL_SECS + 30);
        db.cleanup_expired_wakes().unwrap();

        assert!(
            db.has_recent_wake(&target_id).unwrap(),
            "the nonce is gone (correctly), but the target must still be \
             rate-limited for the rest of the {WAKE_RATE_LIMIT_SECS}s window"
        );
    }

    /// The nonce really does expire at its own TTL — the split must not have
    /// quietly extended a bearer capability's life.
    #[test]
    fn the_nonce_still_expires_at_its_own_ttl() {
        let db = make_db();
        let target_id = [31u8; 32];

        let nonce = db.create_wake(&target_id, "1.2.3.4:51820").unwrap();
        db.set_responder_endpoint(&nonce, "5.6.7.8:51820").unwrap();
        assert!(db.get_responder_endpoint(&nonce).unwrap().is_some());

        backdate_all_wakes(&db, NONCE_TTL_SECS + 30);
        db.cleanup_expired_wakes().unwrap();

        assert!(
            db.get_responder_endpoint(&nonce).unwrap().is_none(),
            "a collected nonce must stop resolving"
        );
    }

    /// Rate-limit history is bounded too — it must not become a log of every
    /// wake the relay has ever seen.
    #[test]
    fn rate_limit_history_is_bounded_by_its_own_window() {
        let db = make_db();
        let target_id = [32u8; 32];

        db.create_wake(&target_id, "1.2.3.4:51820").unwrap();
        backdate_all_wakes(&db, WAKE_RATE_LIMIT_SECS + 30);
        db.cleanup_expired_wakes().unwrap();

        assert!(
            !db.has_recent_wake(&target_id).unwrap(),
            "past the rate-limit window the target must be wakeable again"
        );
        let conn = db.conn.lock().unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM wake_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "expired history must be collected, not retained");
    }

    #[test]
    fn has_recent_wake_detects_duplicate() {
        let db = make_db();
        let target_id = [20u8; 32];
        let other_id = [21u8; 32];

        // No wake yet — should return false
        assert!(!db.has_recent_wake(&target_id).unwrap());

        // Create one wake
        db.create_wake(&target_id, "1.2.3.4:51820").unwrap();

        // Now should be detected as recent
        assert!(db.has_recent_wake(&target_id).unwrap());

        // A different target should still be false
        assert!(!db.has_recent_wake(&other_id).unwrap());
    }
}
