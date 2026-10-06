//! Inbox storage methods.

use super::{CacheDb, content, links, now_epoch_millis, search};
use anyhow::{Context, Result};

/// The delivery-link metadata key recording the bytes the push charged to the
/// recipient's `inbox_bytes_used` — `0` for an uncharged push. The ack refunds
/// exactly this ([`CacheDb::ack_and_refund_row`]).
pub(super) const CHARGED_BYTES_KEY: &str = "charged_bytes";

/// What an ack of a delivery link refunds: exactly the charge its push
/// recorded; `0` for a link that records none.
pub(super) fn refund_for(metadata: Option<Vec<u8>>) -> i64 {
    recorded_charge(metadata.as_deref()).unwrap_or(0)
}

/// The charge a delivery link's metadata records; `None` when it records none.
pub(super) fn recorded_charge(metadata: Option<&[u8]>) -> Option<i64> {
    serde_json::from_slice::<serde_json::Value>(metadata?)
        .ok()?
        .get(CHARGED_BYTES_KEY)?
        .as_i64()
}

impl CacheDb {
    /// Compute a unique content ID for an inbox message.
    ///
    /// The inbox is an append log, **not** content-addressed storage: two
    /// genuinely-distinct deliveries of the same payload to the same recipient
    /// must each get their own row, even when they land in the same millisecond.
    /// Hashing only `(recipient, created_at_ms, payload)` collides under a tight
    /// same-recipient/same-body loop within one millisecond — and the
    /// `idx_links_unique_delivery (source_id, actor_id)` unique index then
    /// rejects the second delivery link with a UNIQUE violation surfaced as a
    /// generic "storage error". (This is why `deliver_twice_creates_two_inbox_rows`
    /// flaked on fast/warm runs but passed when the two iterations straddled a
    /// millisecond boundary.) A process-global monotonic nonce makes every
    /// content id unique-per-insertion regardless of timing. Nothing recomputes
    /// this id for lookup: the WS push hint is a separate body-only hash
    /// (`ws::inbox_content_id_hex`), so mixing in the nonce is safe.
    fn inbox_content_id(recipient: &[u8; 32], created_at: i64, payload: &[u8]) -> [u8; 32] {
        use std::sync::atomic::{AtomicU64, Ordering};
        static INBOX_NONCE: AtomicU64 = AtomicU64::new(0);
        let nonce = INBOX_NONCE.fetch_add(1, Ordering::Relaxed);
        let mut hasher = blake3::Hasher::new();
        hasher.update(recipient);
        hasher.update(&created_at.to_le_bytes());
        hasher.update(&nonce.to_le_bytes());
        hasher.update(payload);
        *hasher.finalize().as_bytes()
    }

    /// Insert one inbox row on an already-held connection — the content row and
    /// the `undelivered` delivery link that `poll_inbox` reads back — and, when
    /// `charge` is set, charge the payload's length to the recipient's
    /// `inbox_bytes_used`.
    ///
    /// The link records what this insert charged ([`CHARGED_BYTES_KEY`] in its
    /// metadata, `0` for an uncharged row), and the ack refunds exactly that
    /// ([`Self::ack_and_refund_row`]): a refund never exceeds the charge.
    /// Charging and recording in this one function keeps the two from
    /// disagreeing — no caller charges the counter by hand.
    ///
    /// Takes the connection rather than the lock so a caller that must write the
    /// delivery **together with** something else can pass its own transaction
    /// (`rusqlite::Transaction` derefs to `Connection`): a room invitation is
    /// recorded and delivered as one act, because a recorded invitation nobody
    /// can discover and a delivered one the accept door does not know about are
    /// both states no ceremony produced. The two `push_inbox*` methods below are
    /// the lock-taking wrappers.
    pub(super) fn insert_inbox_row(
        conn: &rusqlite::Connection,
        recipient: &[u8; 32],
        payload: &[u8],
        blob_hash: Option<&[u8; 32]>,
        charge: bool,
    ) -> Result<i64> {
        let now = now_epoch_millis();
        // Three different consumers of "now" in this function, and they do NOT
        // share a unit — the reason each is bound separately rather than reused:
        // `content.created_at` is epoch MICROSECONDS (`db/schema.rs`),
        // `content_links.created_at` is milliseconds, and `inbox_content_id`
        // takes the millisecond value as its uniqueness nonce (changing what is
        // fed there would change every id this function mints). Derived from the
        // one clock read rather than calling `now_epoch_micros()` so the id and
        // the timestamp name the same instant.
        let now_us = now * 1_000;
        let content_id = Self::inbox_content_id(recipient, now, payload);
        let db_payload = if blob_hash.is_some() {
            &[][..]
        } else {
            payload
        };
        let zero_author = [0u8; 32];
        content::insert_content(
            conn,
            &content_id,
            "inbox/message",
            &zero_author,
            now_us,
            db_payload,
            None,
            "fauna",
            blob_hash,
        )?;
        let charged = if charge { payload.len() as i64 } else { 0 };
        let meta = serde_json::to_vec(&serde_json::json!({
            "mailbox": "INBOX",
            CHARGED_BYTES_KEY: charged,
        }))
        .unwrap();
        let link_id = links::insert_link(
            conn,
            "delivery",
            Some(content_id.as_slice()),
            None,
            Some(recipient.as_slice()),
            Some("undelivered"),
            Some(&meta),
            now,
        )?;
        if charged > 0 {
            conn.execute(
                "UPDATE users SET inbox_bytes_used = inbox_bytes_used + ?1 WHERE actor_id = ?2",
                rusqlite::params![charged, recipient.as_slice()],
            )
            .context("update inbox_bytes_used")?;
        }
        Ok(link_id)
    }

    /// Insert a message into a recipient's inbox **uncharged**. Returns the
    /// delivery link ID. Its ack refunds nothing.
    pub async fn push_inbox(
        &self,
        recipient: &[u8; 32],
        payload: &[u8],
        blob_hash: Option<&[u8; 32]>,
    ) -> Result<i64> {
        let recipient = *recipient;
        let payload = payload.to_vec();
        let blob_hash = blob_hash.copied();
        let conn = self.conn.lock().await;
        Self::insert_inbox_row(&conn, &recipient, &payload, blob_hash.as_ref(), false)
    }

    /// Insert a message and charge its length to the recipient's inbox quota
    /// under one lock hold. Returns the delivery link ID. Its ack refunds the
    /// same length.
    pub async fn push_inbox_with_quota(
        &self,
        recipient: &[u8; 32],
        payload: &[u8],
        blob_hash: Option<&[u8; 32]>,
    ) -> Result<i64> {
        let recipient = *recipient;
        let payload = payload.to_vec();
        let blob_hash = blob_hash.copied();
        let conn = self.conn.lock().await;
        Self::insert_inbox_row(&conn, &recipient, &payload, blob_hash.as_ref(), true)
    }

    /// Get all undelivered messages for a recipient.
    /// Returns (link_id, payload, blob_hash) tuples.
    pub async fn poll_inbox(
        &self,
        recipient: &[u8; 32],
    ) -> Result<Vec<(i64, Vec<u8>, Option<Vec<u8>>)>> {
        self.poll_inbox_after(recipient, None).await
    }

    /// [`Self::poll_inbox`] from a **skip cursor**: undelivered rows whose
    /// delivery-link id is greater than `after_id` (`None` == from the oldest).
    ///
    /// Backs `fauna.inbox.fetch`'s `after_id`, which lets a client step past an
    /// item it cannot apply so the item stops shadowing the rest of the queue.
    /// The filter is in SQL on purpose: a client walking a long queue one page
    /// at a time must not re-read the prefix on every page.
    ///
    /// Skipping is not delivery — passed-over rows keep `status =
    /// 'undelivered'` and are returned again by the next cursor-less poll.
    pub async fn poll_inbox_after(
        &self,
        recipient: &[u8; 32],
        after_id: Option<i64>,
    ) -> Result<Vec<(i64, Vec<u8>, Option<Vec<u8>>)>> {
        let recipient = *recipient;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT cl.id, c.payload, c.blob_hash FROM content_links cl \
                 JOIN content c ON c.id = cl.source_id \
                 WHERE cl.actor_id = ?1 AND cl.link_type = 'delivery' AND cl.status = 'undelivered' \
                 AND cl.id > ?2 \
                 ORDER BY cl.id"
            )
            .context("prepare poll")?;
        let rows = stmt
            .query_map(
                rusqlite::params![recipient.as_slice(), after_id.unwrap_or(i64::MIN)],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                },
            )
            .context("query poll")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read row")?);
        }
        Ok(results)
    }

    /// Ack one delivery-link row on an already-held connection, refunding its
    /// owner's inbox quota. Returns whether the row actually flipped.
    ///
    /// Extracted from [`Self::ack_inbox`]'s per-id body so a caller already
    /// inside its own transaction (a room invitation's re-delivery, or its
    /// acceptance — `db/rooms.rs`) can consume one specific standing envelope
    /// without going through the ack RPC. Scoped to `status = 'undelivered'`
    /// exactly like `ack_inbox`, so it is idempotent and safe to race: a row
    /// already acked (by the recipient's own `fauna.inbox.ack`, or a previous
    /// call here) changes nothing and refunds nothing twice.
    pub(super) fn ack_and_refund_row(
        conn: &rusqlite::Connection,
        recipient: &[u8; 32],
        link_id: i64,
    ) -> Result<bool> {
        // The quota decrement — what this row's push charged, scoped to this
        // owner + still-undelivered.
        let size: Option<i64> = conn
            .query_row(
                "SELECT cl.metadata FROM content_links cl \
                 WHERE cl.id = ?1 AND cl.link_type = 'delivery' \
                   AND cl.actor_id = ?2 AND cl.status = 'undelivered'",
                rusqlite::params![link_id, recipient.as_slice()],
                |row| Ok(refund_for(row.get(0)?)),
            )
            .ok();
        let changed = conn
            .execute(
                "UPDATE content_links SET status = 'delivered', updated_at = ?3 \
                 WHERE id = ?1 AND link_type = 'delivery' AND actor_id = ?2 \
                   AND status = 'undelivered'",
                rusqlite::params![link_id, recipient.as_slice(), now_epoch_millis()],
            )
            .context("ack_and_refund_row: mark delivered")?;
        if changed > 0
            && let Some(size) = size.filter(|&n| n > 0)
        {
            conn.execute(
                "UPDATE users SET inbox_bytes_used = MAX(0, inbox_bytes_used - ?1) WHERE actor_id = ?2",
                rusqlite::params![size, recipient.as_slice()],
            )
            .ok();
        }
        Ok(changed > 0)
    }

    /// Ack inbox items: mark the given delivery-link ids delivered, but
    /// **only** rows the `recipient` owns that are still `undelivered`.
    /// Returns the number of rows newly flipped.
    ///
    /// This is the caller-scoped, idempotent backend for `fauna.inbox.ack`
    /// (the WS-RPC successor to the HTTP `GET /api/v1/inbox` drain). The
    /// `UPDATE` is scoped to `actor_id = recipient`, so a caller can never ack
    /// another actor's items even if it guesses their link ids. Ids not
    /// owned, unknown, or already delivered are silently skipped (idempotent:
    /// a replayed ack just returns a smaller count). Decrements the owner's
    /// inbox quota per flipped row.
    pub async fn ack_inbox(&self, recipient: &[u8; 32], ids: &[i64]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let recipient = *recipient;
        let conn = self.conn.lock().await;
        let mut flipped = 0u64;
        for &id in ids {
            if Self::ack_and_refund_row(&conn, &recipient, id)? {
                flipped += 1;
            }
        }
        Ok(flipped)
    }

    /// Return ALL inbox entries (delivered and undelivered) for a recipient.
    /// Returns (link_id, payload, blob_hash, created_at, delivered) tuples.
    pub async fn list_inbox_all(
        &self,
        recipient: &[u8; 32],
    ) -> Result<Vec<(i64, Vec<u8>, Option<Vec<u8>>, i64, bool)>> {
        let recipient = *recipient;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT cl.id, c.payload, c.blob_hash, c.created_at, (cl.status = 'delivered') AS delivered \
                 FROM content_links cl \
                 JOIN content c ON c.id = cl.source_id \
                 WHERE cl.actor_id = ?1 AND cl.link_type = 'delivery' \
                 ORDER BY cl.id"
            )
            .context("prepare list_inbox_all")?;
        let rows = stmt
            .query_map(rusqlite::params![recipient.as_slice()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)? != 0,
                ))
            })
            .context("query list_inbox_all")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read inbox row")?);
        }
        Ok(results)
    }

    /// Delete all inbox messages for an actor.
    pub async fn delete_inbox_for_actor(&self, actor_id: &[u8; 32]) -> Result<u64> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;

        // Collect content IDs from delivery links for this actor
        let mut stmt = conn.prepare(
            "SELECT source_id FROM content_links WHERE actor_id = ?1 AND link_type = 'delivery'",
        ).context("prepare delete_inbox_for_actor select")?;
        let content_ids: Vec<Vec<u8>> = stmt
            .query_map(rusqlite::params![actor], |row| row.get(0))
            .context("query delivery links")?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        // Delete label links for this actor
        conn.execute(
            "DELETE FROM content_links WHERE actor_id = ?1 AND link_type = 'label'",
            rusqlite::params![actor_id.as_slice()],
        )
        .context("delete label links")?;

        // Delete delivery links for this actor
        let deleted = conn
            .execute(
                "DELETE FROM content_links WHERE actor_id = ?1 AND link_type = 'delivery'",
                rusqlite::params![actor_id.as_slice()],
            )
            .context("delete delivery links")?;

        // Delete orphaned content rows and FTS entries
        for content_id in &content_ids {
            // Only delete if no other delivery links reference this content
            let remaining: i64 = conn.query_row(
                "SELECT COUNT(*) FROM content_links WHERE source_id = ?1 AND link_type = 'delivery'",
                rusqlite::params![content_id],
                |row| row.get(0),
            ).unwrap_or(0);
            if remaining == 0 && content_id.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(content_id);
                let _ = search::remove_content(&conn, &arr);
                let _ = content::delete_content(&conn, &arr);
            }
        }

        Ok(deleted as u64)
    }
}
