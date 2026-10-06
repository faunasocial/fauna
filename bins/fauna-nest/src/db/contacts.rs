//! Contact, inbox mode, and knock methods.

use super::{CacheDb, now_epoch_millis, now_epoch_secs};
use super::{ContactRow, KnockRow};
use anyhow::{Context, Result};

impl CacheDb {
    // ==================== Contacts ====================

    /// Get the contact status between actor and peer, or None if no relationship exists.
    pub async fn get_contact_status(
        &self,
        actor_id: &[u8; 32],
        peer_id: &[u8; 32],
    ) -> Result<Option<String>> {
        let actor_id = *actor_id;
        let peer_id = *peer_id;
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT status FROM contacts WHERE actor_id = ?1 AND peer_id = ?2",
            rusqlite::params![actor_id.as_slice(), peer_id.as_slice()],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(status) => Ok(Some(status)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get contact status"),
        }
    }

    /// Insert or update a contact relationship. Sets accepted_at when status is "accepted".
    pub async fn upsert_contact(
        &self,
        actor_id: &[u8; 32],
        peer_id: &[u8; 32],
        status: &str,
    ) -> Result<()> {
        let actor_id = *actor_id;
        let peer_id = *peer_id;
        let status = status.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let accepted_at: Option<i64> = if status == "accepted" {
            Some(now)
        } else {
            None
        };
        conn.execute(
            "INSERT INTO contacts (actor_id, peer_id, status, accepted_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(actor_id, peer_id) DO UPDATE SET status = excluded.status, accepted_at = COALESCE(excluded.accepted_at, contacts.accepted_at)",
            rusqlite::params![actor_id.as_slice(), peer_id.as_slice(), status, accepted_at, now],
        )
        .context("upsert contact")?;
        Ok(())
    }

    /// Accept a contact (shorthand for upsert with status "accepted").
    pub async fn accept_contact(&self, actor_id: &[u8; 32], peer_id: &[u8; 32]) -> Result<()> {
        self.upsert_contact(actor_id, peer_id, "accepted").await
    }

    /// Promote an accepted contact to confirmed.
    /// Only updates if current status is "accepted".
    pub async fn promote_to_confirmed(
        &self,
        actor_id: &[u8; 32],
        peer_id: &[u8; 32],
    ) -> Result<()> {
        let actor_id = *actor_id;
        let peer_id = *peer_id;
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE contacts SET status = 'confirmed' WHERE actor_id = ?1 AND peer_id = ?2 AND status = 'accepted'",
            rusqlite::params![actor_id.as_slice(), peer_id.as_slice()],
        )
        .context("promote to confirmed")?;
        Ok(())
    }

    /// Block a contact (shorthand for upsert with status "blocked").
    pub async fn block_contact(&self, actor_id: &[u8; 32], peer_id: &[u8; 32]) -> Result<()> {
        self.upsert_contact(actor_id, peer_id, "blocked").await
    }

    /// Delete a contact relationship entirely.
    pub async fn delete_contact(&self, actor_id: &[u8; 32], peer_id: &[u8; 32]) -> Result<()> {
        let actor_id = *actor_id;
        let peer_id = *peer_id;
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM contacts WHERE actor_id = ?1 AND peer_id = ?2",
            rusqlite::params![actor_id.as_slice(), peer_id.as_slice()],
        )
        .context("delete contact")?;
        Ok(())
    }

    /// Unblock a contact: clear a `blocked` edge entirely, returning the
    /// relationship to no-edge (`get_contact_status` → `None`). The inverse of
    /// [`block_contact`], but **guarded on `status = 'blocked'`** so it only
    /// ever removes a block — a no-op on an `accepted`/`confirmed`/`pending`
    /// edge (or no edge at all). The guard is what makes a stale client
    /// snapshot's "Unblock" click safe: it can never delete a live
    /// relationship. A block stores no pre-block status (`block_contact`
    /// overwrites `status` with no history), so clearing the edge is the only
    /// available inverse — re-establishing contact goes through a fresh knock.
    /// Semantics: `docs/goal/ui/contacts.md` § Where logic lives → Unblock.
    pub async fn unblock_contact(&self, actor_id: &[u8; 32], peer_id: &[u8; 32]) -> Result<()> {
        let actor_id = *actor_id;
        let peer_id = *peer_id;
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM contacts WHERE actor_id = ?1 AND peer_id = ?2 AND status = 'blocked'",
            rusqlite::params![actor_id.as_slice(), peer_id.as_slice()],
        )
        .context("unblock contact")?;
        Ok(())
    }

    /// List all contacts for an actor. Returns (peer_id, status) pairs.
    pub async fn list_contacts(&self, actor_id: &[u8; 32]) -> Result<Vec<(Vec<u8>, String)>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT peer_id, status FROM contacts WHERE actor_id = ?1 ORDER BY created_at")
            .context("prepare list_contacts")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice()], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })
            .context("query contacts")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read contact row")?);
        }
        Ok(results)
    }

    /// List all contacts for an actor with full row data.
    pub async fn list_contacts_full(&self, actor_id: &[u8; 32]) -> Result<Vec<ContactRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        // `LEFT JOIN` the peer's local `users` row to enrich each contact with
        // the peer's handle. `u.handle` is NULL when the peer is not a local
        // user (a federated peer has no `users` row), `''` for a local user
        // with no handle set — `docs/goal/ui/contacts.md` § State & data shape.
        // One query keeps the list read cheap (the contacts-page-open path).
        let mut stmt = conn
            .prepare(
                "SELECT c.peer_id, c.status, c.accepted_at, c.created_at, u.handle
                 FROM contacts c
                 LEFT JOIN users u ON u.actor_id = c.peer_id
                 WHERE c.actor_id = ?1 ORDER BY c.created_at",
            )
            .context("prepare list_contacts_full")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice()], |row| {
                Ok(ContactRow {
                    peer_id: row.get(0)?,
                    status: row.get(1)?,
                    accepted_at: row.get(2)?,
                    created_at: row.get(3)?,
                    handle: row.get(4)?,
                })
            })
            .context("query contacts_full")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read contact row")?);
        }
        Ok(results)
    }

    /// Delete accepted contacts whose accepted_at is older than ttl_secs ago.
    /// Returns the number of expired contacts deleted.
    pub async fn expire_accepted_contacts(&self, ttl_secs: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        let cutoff = now_epoch_secs() - ttl_secs;
        let deleted = conn
            .execute(
                "DELETE FROM contacts WHERE status = 'accepted' AND accepted_at < ?1",
                rusqlite::params![cutoff],
            )
            .context("expire accepted contacts")?;
        Ok(deleted)
    }

    // ==================== Inbox Mode ====================

    /// Get the inbox mode for an actor. Defaults to "allow_knock".
    pub async fn get_inbox_mode(&self, actor_id: &[u8; 32]) -> Result<String> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mode: Option<String> = conn
            .query_row(
                "SELECT mode FROM inbox_modes WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get(0),
            )
            .ok();
        Ok(mode.unwrap_or_else(|| "allow_knock".to_string()))
    }

    /// Set the inbox mode for an actor.
    pub async fn set_inbox_mode(&self, actor_id: &[u8; 32], mode: &str) -> Result<()> {
        const VALID_INBOX_MODES: &[&str] = &["open", "allow_knock", "contacts_only", "closed"];
        if !VALID_INBOX_MODES.contains(&mode) {
            anyhow::bail!("invalid inbox mode: {mode}");
        }
        let actor_id = *actor_id;
        let mode = mode.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO inbox_modes (actor_id, mode) VALUES (?1, ?2)
             ON CONFLICT(actor_id) DO UPDATE SET mode = ?2",
            rusqlite::params![actor_id.as_slice(), mode],
        )
        .context("set inbox mode")?;
        Ok(())
    }

    // ==================== Knocks ====================

    /// Push a knock notification. Returns the knock row ID.
    ///
    /// `payload` is the signed `(ContactRequest, Post)` tuple the knocking
    /// arrival carried, held here until the recipient — or, for a supervised
    /// account, their guardian — accepts the sender, at which point
    /// [`Self::pending_knock_payload`] hands it back for delivery. An empty
    /// slice means "nothing to release" (a bare knock, or an arrival too large
    /// to hold). The knock IS the pending state; nothing else records it
    /// (`family-safety.md` § Don't do these — no parallel pending-state machine).
    pub async fn push_knock(
        &self,
        actor_id: &[u8; 32],
        sender_id: &[u8; 32],
        sender_node: &[u8],
        summary: &str,
        payload: &[u8],
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        Self::insert_knock(&conn, actor_id, sender_id, sender_node, summary, payload)
    }

    /// [`Self::push_knock`], unless `actor_id` already holds `max_knocks` knock
    /// rows — then `Ok(None)` and nothing is written. Count and insert run under
    /// one connection lock, so concurrent arrivals cannot overshoot the cap.
    pub async fn push_knock_within_cap(
        &self,
        actor_id: &[u8; 32],
        sender_id: &[u8; 32],
        sender_node: &[u8],
        summary: &str,
        payload: &[u8],
        max_knocks: usize,
    ) -> Result<Option<i64>> {
        let conn = self.conn.lock().await;
        let held: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM knocks WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get(0),
            )
            .context("count knocks")?;
        if held as usize >= max_knocks {
            return Ok(None);
        }
        Self::insert_knock(&conn, actor_id, sender_id, sender_node, summary, payload).map(Some)
    }

    fn insert_knock(
        conn: &rusqlite::Connection,
        actor_id: &[u8; 32],
        sender_id: &[u8; 32],
        sender_node: &[u8],
        summary: &str,
        payload: &[u8],
    ) -> Result<i64> {
        let now = now_epoch_millis();
        conn.execute(
            "INSERT INTO knocks (actor_id, sender_id, sender_node, summary, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                actor_id.as_slice(),
                sender_id.as_slice(),
                sender_node,
                summary,
                payload,
                now
            ],
        )
        .context("push knock")?;
        Ok(conn.last_insert_rowid())
    }

    /// The held payload of the oldest undelivered knock from `sender_id` to
    /// `actor_id`, or `None` when there is no such knock. An empty payload
    /// reads as `Some(vec![])` — the caller treats it as nothing to release.
    ///
    /// Read *before* [`Self::dismiss_knock`], which is what accept/block call to
    /// retire the knock row.
    pub async fn pending_knock_payload(
        &self,
        actor_id: &[u8; 32],
        sender_id: &[u8; 32],
    ) -> Result<Option<Vec<u8>>> {
        let actor_id = *actor_id;
        let sender_id = *sender_id;
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT payload FROM knocks
             WHERE actor_id = ?1 AND sender_id = ?2 AND delivered = 0
             ORDER BY id LIMIT 1",
            rusqlite::params![actor_id.as_slice(), sender_id.as_slice()],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("pending knock payload"),
        }
    }

    /// Poll undelivered knocks for an actor.
    pub async fn poll_knocks(&self, actor_id: &[u8; 32]) -> Result<Vec<KnockRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, sender_id, sender_node, summary, created_at
                 FROM knocks WHERE actor_id = ?1 AND delivered = 0 ORDER BY id",
            )
            .context("prepare poll_knocks")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice()], |row| {
                let sender_bytes: Vec<u8> = row.get(1)?;
                let sender_id: [u8; 32] = sender_bytes.try_into().map_err(|_| {
                    rusqlite::Error::InvalidColumnType(
                        1,
                        "sender_id".into(),
                        rusqlite::types::Type::Blob,
                    )
                })?;
                Ok(KnockRow {
                    id: row.get(0)?,
                    sender_id,
                    sender_node: row.get(2)?,
                    summary: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .context("query knocks")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read knock row")?);
        }
        Ok(results)
    }

    /// Dismiss (delete) all knocks from a specific sender to a specific actor.
    pub async fn dismiss_knock(&self, actor_id: &[u8; 32], sender_id: &[u8; 32]) -> Result<()> {
        let actor_id = *actor_id;
        let sender_id = *sender_id;
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM knocks WHERE actor_id = ?1 AND sender_id = ?2",
            rusqlite::params![actor_id.as_slice(), sender_id.as_slice()],
        )
        .context("dismiss knock")?;
        Ok(())
    }

    /// Check if there is a pending (undelivered) knock from sender to actor.
    pub async fn has_pending_knock(
        &self,
        actor_id: &[u8; 32],
        sender_id: &[u8; 32],
    ) -> Result<bool> {
        let actor_id = *actor_id;
        let sender_id = *sender_id;
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM knocks WHERE actor_id = ?1 AND sender_id = ?2 AND delivered = 0",
                rusqlite::params![actor_id.as_slice(), sender_id.as_slice()],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count > 0)
    }

    /// Delete knocks older than ttl_secs, and with each one the two rows
    /// `store_knock` wrote beside it: its `knock` notification row — the
    /// doorbell lives exactly as long as the knock it announces
    /// (`behavior/notifications.md` § Retention, rule 3; a doorbell whose
    /// knock is already gone, i.e. an accepted request's, has no knock row to
    /// join and stays) — and its `pending` contact edge (`ui/contacts.md`
    /// § Persistence: the edge that made the sender's every later knock
    /// refuse as still-pending, with no knock left for the recipient to act
    /// on). Returns the number of knocks deleted.
    pub async fn expire_old_knocks(&self, ttl_secs: i64) -> Result<usize> {
        let mut conn = self.conn.lock().await;
        let cutoff = now_epoch_millis() - (ttl_secs * 1000);
        let tx = conn.transaction().context("expire old knocks: begin")?;
        tx.execute(
            "DELETE FROM notifications
             WHERE notif_type = 'knock'
               AND EXISTS (SELECT 1 FROM knocks k
                           WHERE k.actor_id = notifications.actor_id
                             AND k.sender_id = notifications.sender_id
                             AND k.created_at < ?1)",
            rusqlite::params![cutoff],
        )
        .context("expire old knocks: doorbells")?;
        tx.execute(
            "DELETE FROM contacts
             WHERE status = 'pending'
               AND EXISTS (SELECT 1 FROM knocks k
                           WHERE k.actor_id = contacts.actor_id
                             AND k.sender_id = contacts.peer_id
                             AND k.created_at < ?1)",
            rusqlite::params![cutoff],
        )
        .context("expire old knocks: pending edges")?;
        let deleted = tx
            .execute(
                "DELETE FROM knocks WHERE created_at < ?1",
                rusqlite::params![cutoff],
            )
            .context("expire old knocks")?;
        tx.commit().context("expire old knocks: commit")?;
        Ok(deleted)
    }
}
