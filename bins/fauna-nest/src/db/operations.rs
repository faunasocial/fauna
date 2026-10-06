//! Operation lock and upload lease methods.

use super::{CacheDb, now_epoch_secs};
use anyhow::Result;

/// One folder's live (unexpired) upload lease, as [`CacheDb::live_upload_leases_for`]
/// reads it.
#[derive(Debug, Clone)]
pub struct LiveLease {
    pub device_id: Vec<u8>,
    /// The account that took the lease.
    pub actor_id: Vec<u8>,
    pub expires_at: i64,
}

impl CacheDb {
    /// Returns the set of lock types that conflict with the given lock type.
    fn conflicting_lock_types(lock_type: &str) -> &'static [&'static str] {
        match lock_type {
            "gc" => &["prune", "check", "restore"],
            "prune" => &["gc"],
            "check" => &["gc"],
            "restore" => &["gc"],
            _ => &[],
        }
    }

    /// Try to acquire an operation lock. Returns true if acquired.
    /// Cleans up expired locks before attempting acquisition.
    pub async fn try_acquire_op_lock(
        &self,
        lock_type: &str,
        folder_id: i64,
        holder: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let expires_at = now + 3600; // 1 hour default
        conn.execute(
            "DELETE FROM operation_locks WHERE expires_at < ?1",
            rusqlite::params![now],
        )?;
        // Check conflict matrix before attempting INSERT
        let conflicts = Self::conflicting_lock_types(lock_type);
        if !conflicts.is_empty() {
            let placeholders: Vec<&str> = conflicts.iter().map(|_| "?").collect();
            let sql = format!(
                "SELECT COUNT(*) FROM operation_locks WHERE lock_type IN ({}) AND (folder_id = ?{} OR folder_id = -1 OR ?{} = -1) AND expires_at > ?{}",
                placeholders.join(","),
                conflicts.len() + 1,
                conflicts.len() + 2,
                conflicts.len() + 3,
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            for c in conflicts {
                params.push(Box::new(c.to_string()));
            }
            params.push(Box::new(folder_id));
            params.push(Box::new(folder_id));
            params.push(Box::new(now));
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let count: i64 = stmt.query_row(param_refs.as_slice(), |r| r.get(0))?;
            if count > 0 {
                return Ok(false);
            }
        }
        let result = conn.execute(
            "INSERT OR IGNORE INTO operation_locks (lock_type, folder_id, holder, acquired_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![lock_type, folder_id, holder, now, expires_at],
        )?;
        Ok(result > 0)
    }

    /// Whether a live (unexpired) lock of `lock_type` is held for any file
    /// set. Debug-assertion support: lets a rebuild path enforce its
    /// caller-held-lock precondition structurally instead of by doc comment.
    pub async fn op_lock_live(&self, lock_type: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let live: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM operation_locks
              WHERE lock_type = ?1 AND expires_at > ?2)",
            rusqlite::params![lock_type, now],
            |row| row.get(0),
        )?;
        Ok(live)
    }

    /// Release an operation lock.
    pub async fn release_op_lock(&self, lock_type: &str, folder_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM operation_locks WHERE lock_type = ?1 AND folder_id = ?2",
            rusqlite::params![lock_type, folder_id],
        )?;
        Ok(())
    }

    /// Try to acquire an upload lease. Returns true if acquired.
    /// The same `(actor, device)` pair can renew; an expired lease can be taken
    /// over by anyone.
    ///
    /// ⚠ **The holder is the ACTOR plus its device, never the device alone.** A
    /// sync device id is client-asserted and bound to nothing, and the lease
    /// is taken by a writer MEMBER of another account as readily as by the
    /// owner — so a same-device acquire from a different actor is a refused
    /// takeover, not a renewal.
    pub async fn try_acquire_upload_lease(
        &self,
        folder_id: i64,
        actor_id: &[u8; 32],
        device_id: &[u8; 32],
        ttl_secs: i64,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        // Clean up expired leases for this folder
        conn.execute(
            "DELETE FROM upload_leases WHERE folder_id = ?1 AND expires_at < ?2",
            rusqlite::params![folder_id, now],
        )?;
        let expires_at = now + ttl_secs;
        // Try insert — fails if another (actor, device) holds it
        let result = conn.execute(
            "INSERT OR REPLACE INTO upload_leases (folder_id, device_id, acquired_at, expires_at, heartbeat_at, actor_id) \
             SELECT ?1, ?2, ?3, ?4, ?3, ?5 \
             WHERE NOT EXISTS (SELECT 1 FROM upload_leases \
                               WHERE folder_id = ?1 AND (device_id != ?2 OR actor_id != ?5))",
            rusqlite::params![
                folder_id,
                device_id.as_slice(),
                now,
                expires_at,
                actor_id.as_slice()
            ],
        )?;
        Ok(result > 0)
    }

    /// The **live** (unexpired) upload leases over `folder_ids`, keyed by
    /// folder id — the read behind `FolderSummary::lease`
    /// (`file-sync.md` § Exclusive editing).
    ///
    /// ⚠ **Filters on `expires_at` rather than trusting the table.** Expired
    /// rows are swept lazily — only [`Self::try_acquire_upload_lease`] deletes
    /// them, and only for the folder it was asked about — so a folder nobody
    /// has tried to acquire since a holder went away still carries its stale
    /// row. Projecting that row would tell every seat the folder is locked by a
    /// device that let go hours ago, and nothing would ever correct it.
    ///
    /// A pure read: it never sweeps. Sweeping here would make an ordinary
    /// `fauna.folders.list` a writer, and a read that writes is how a list call
    /// ends up contending with the acquire path it is only reporting on.
    pub async fn live_upload_leases_for(
        &self,
        folder_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, LiveLease>> {
        let mut out = std::collections::HashMap::new();
        if folder_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let mut stmt = conn.prepare(
            "SELECT folder_id, device_id, expires_at, actor_id FROM upload_leases \
             WHERE folder_id = ?1 AND expires_at > ?2",
        )?;
        for id in folder_ids {
            let mut rows = stmt.query(rusqlite::params![id, now])?;
            if let Some(row) = rows.next()? {
                let folder_id: i64 = row.get(0)?;
                let device_id: Vec<u8> = row.get(1)?;
                let expires_at: i64 = row.get(2)?;
                let actor_id: Vec<u8> = row.get(3)?;
                out.insert(
                    folder_id,
                    LiveLease {
                        device_id,
                        actor_id,
                        expires_at,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Release an upload lease. The DELETE is scoped to
    /// `(folder_id, actor_id, device_id)` — a holder releases only the lease its
    /// own actor's device holds, never another's (Phase 1
    /// widened release from owner-only-self-scoped to writer members, so an
    /// unscoped delete let any writer drop another actor's active lease; the
    /// device id alone, being client-asserted, did not close that — the actor
    /// column does). There is no
    /// holder-blind clear: the device-id-less arm (an old client, or an owner
    /// force-releasing a crash-stuck lease) was retired 2026-09-24 under the
    /// compat-remnant sweep — a crash-stuck lease lapses on its TTL.
    pub async fn release_upload_lease(
        &self,
        folder_id: i64,
        actor_id: &[u8; 32],
        device_id: &[u8; 32],
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM upload_leases WHERE folder_id = ?1 AND device_id = ?2 AND actor_id = ?3",
            rusqlite::params![folder_id, device_id.as_slice(), actor_id.as_slice()],
        )?;
        Ok(())
    }

    /// Renew an operation lock's expiry time.
    pub async fn renew_op_lock(
        &self,
        lock_type: &str,
        folder_id: i64,
        new_expires_at: i64,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let rows = conn.execute(
            "UPDATE operation_locks SET expires_at = ?1 WHERE lock_type = ?2 AND folder_id = ?3",
            rusqlite::params![new_expires_at, lock_type, folder_id],
        )?;
        Ok(rows > 0)
    }
}
