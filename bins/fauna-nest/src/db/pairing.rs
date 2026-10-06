//! Nest pairing, outbox queue, and namespace entry methods.

use super::{CacheDb, NamespaceEntry, NestPairing, OutboxAuthorStatus, OutboxEntry};

/// One local `nest_pairings` row as the private-side workers read it
/// ([`CacheDb::list_pairing_targets`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTargetRow {
    pub actor_id: Vec<u8>,
    /// The paired nest's id (the `private_nest_id` column — on a private nest,
    /// the public nest it pairs with).
    pub peer_nest_id: Vec<u8>,
    /// The paired nest's URL, as the user's app recorded it at `fauna.pair.add`.
    pub nest_url: Option<String>,
    pub capabilities: Vec<String>,
    /// Past its `expires_at`: still an identity, no longer an authorization.
    pub expired: bool,
    /// The row's actor is an admin of this nest at read time.
    pub actor_is_admin: bool,
}

impl PairingTargetRow {
    /// Whether the row carries `capability`.
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}
use anyhow::Result;
use fauna_core::data::Timestamp;
use rusqlite::{Connection, OptionalExtension};

impl CacheDb {
    // ── Nest pairing ────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub async fn store_pairing(
        &self,
        actor_id: &[u8],
        private_nest_id: &[u8],
        capabilities: &[String],
        expires_at: Option<i64>,
        nest_url: Option<&str>,
        label: Option<&str>,
    ) -> Result<()> {
        let caps_json = serde_json::to_string(capabilities)?;
        let now = Timestamp::now().as_i64();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO nest_pairings
             (actor_id, private_nest_id, capabilities, expires_at, created_at, nest_url, label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                actor_id,
                private_nest_id,
                caps_json,
                expires_at,
                now,
                nest_url,
                label
            ],
        )?;
        Ok(())
    }

    pub async fn get_pairing(
        &self,
        actor_id: &[u8],
        private_nest_id: &[u8],
    ) -> Result<Option<NestPairing>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT actor_id, private_nest_id, capabilities, expires_at, created_at, nest_url, label
                 FROM nest_pairings
                 WHERE actor_id = ?1 AND private_nest_id = ?2",
                rusqlite::params![actor_id, private_nest_id],
                |row| {
                    let caps_json: String = row.get(2)?;
                    Ok(NestPairing {
                        actor_id: row.get(0)?,
                        private_nest_id: row.get(1)?,
                        capabilities: serde_json::from_str(&caps_json).unwrap_or_default(),
                        expires_at: row.get(3)?,
                        created_at: row.get(4)?,
                        nest_url: row.get(5)?,
                        label: row.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    pub async fn revoke_pairing(&self, actor_id: &[u8], private_nest_id: &[u8]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM nest_pairings WHERE actor_id = ?1 AND private_nest_id = ?2",
            rusqlite::params![actor_id, private_nest_id],
        )?;
        Ok(())
    }

    pub async fn is_paired(&self, actor_id: &[u8], private_nest_id: &[u8]) -> Result<bool> {
        let now = Timestamp::now().as_i64();
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM nest_pairings
             WHERE actor_id = ?1 AND private_nest_id = ?2
             AND (expires_at IS NULL OR expires_at > ?3)",
            rusqlite::params![actor_id, private_nest_id, now],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// True iff there is a **non-expired** pairing for `(actor_id,
    /// private_nest_id)` whose capabilities include `capability` (exact match
    /// on the parsed JSON array, not a substring `LIKE`). The capability-aware
    /// sibling of [`is_paired`] — the mail relay's `mail_pull` gate, and the
    /// model for any future per-capability channel gate. (`is_paired` is
    /// capability-agnostic; the namespace/MLS sync handlers use it because the
    /// v1 link grants the full self-sync set, but the relay gates the specific
    /// `mail_pull` grant so a pairing without it cannot drain mail.)
    pub async fn pairing_has_capability(
        &self,
        actor_id: &[u8],
        private_nest_id: &[u8],
        capability: &str,
    ) -> Result<bool> {
        let now = Timestamp::now().as_i64();
        let conn = self.conn.lock().await;
        let caps_json: Option<String> = conn
            .query_row(
                "SELECT capabilities FROM nest_pairings
                 WHERE actor_id = ?1 AND private_nest_id = ?2
                 AND (expires_at IS NULL OR expires_at > ?3)",
                rusqlite::params![actor_id, private_nest_id, now],
                |row| row.get(0),
            )
            .optional()?;
        let Some(caps_json) = caps_json else {
            return Ok(false);
        };
        let caps: Vec<String> = serde_json::from_str(&caps_json).unwrap_or_default();
        Ok(caps.iter().any(|c| c == capability))
    }

    /// True iff **any** actor on this box holds a **non-expired** pairing whose
    /// capabilities include `capability` (exact match on the parsed JSON array).
    /// The box-wide, actor-agnostic sibling of [`pairing_has_capability`] — the
    /// serving-availability half of the Nostr split (`nostr_serving_available`,
    /// R8 (account-data-plane.md § The ratified decisions)): a keyless public box serves the relay when some head holds a
    /// `nostr_push` pairing through which it proxies, even with no local nsec
    /// deposit. Evaluated per-request, so a revoked pairing flips the box back
    /// to unavailable with no restart (the predicate is derived, never cached).
    ///
    /// The `LIKE '%cap%'` clause is only an index prefilter to narrow the scan;
    /// **every candidate row is JSON-parsed before it counts**, so a capability
    /// that is a substring of another (or that appears inside a value/label)
    /// can never spuriously satisfy the gate. The spec requires this
    /// parse-verification, not a bare `LIKE` (R8).
    pub async fn any_pairing_with_capability(&self, capability: &str) -> Result<bool> {
        let now = Timestamp::now().as_i64();
        let like = format!("%{capability}%");
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT capabilities FROM nest_pairings
             WHERE (expires_at IS NULL OR expires_at > ?1)
             AND capabilities LIKE ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![now, like], |row| row.get::<_, String>(0))?;
        for caps_json in rows.flatten() {
            let caps: Vec<String> = serde_json::from_str(&caps_json).unwrap_or_default();
            if caps.iter().any(|c| c == capability) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// List every **non-expired** pairing whose capabilities include
    /// `capability` (parse-verified per row, not a bare `LIKE` — the `LIKE`
    /// clause only prefilters the scan). The multi-row sibling of
    /// [`any_pairing_with_capability`]: the head's NIP-46 proxy subscription
    /// (spec R10) iterates the `nostr_push` pairings to learn its paired public
    /// serving boxes' relay URLs (from each row's `nest_url`).
    pub async fn list_pairings_with_capability(
        &self,
        capability: &str,
    ) -> Result<Vec<NestPairing>> {
        let now = Timestamp::now().as_i64();
        let like = format!("%{capability}%");
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT actor_id, private_nest_id, capabilities, expires_at, created_at, nest_url, label
             FROM nest_pairings
             WHERE (expires_at IS NULL OR expires_at > ?1) AND capabilities LIKE ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![now, like], |row| {
                let caps_json: String = row.get(2)?;
                Ok(NestPairing {
                    actor_id: row.get(0)?,
                    private_nest_id: row.get(1)?,
                    capabilities: serde_json::from_str(&caps_json).unwrap_or_default(),
                    expires_at: row.get(3)?,
                    created_at: row.get(4)?,
                    nest_url: row.get(5)?,
                    label: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .filter(|p| p.capabilities.iter().any(|c| c == capability))
            .collect();
        Ok(rows)
    }

    /// True iff `actor_id` holds **any** non-expired pairing whose capabilities
    /// include `capability` (exact match on the parsed JSON array). The
    /// actor-scoped, peer-agnostic sibling of [`pairing_has_capability`] — used
    /// where the caller knows the actor but not which peer nest granted the cap
    /// (the gift-wrap inbox's proxied-recipient gate: a proxied
    /// `nostr_accounts` row serves as an inbox exactly while some head still
    /// holds a `nostr_push` pairing for that actor). An actor may have several
    /// pairing rows (one per peer nest), so this scans them all.
    pub async fn actor_has_pairing_with_capability(
        &self,
        actor_id: &[u8],
        capability: &str,
    ) -> Result<bool> {
        let now = Timestamp::now().as_i64();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT capabilities FROM nest_pairings
             WHERE actor_id = ?1
             AND (expires_at IS NULL OR expires_at > ?2)",
        )?;
        let rows = stmt.query_map(rusqlite::params![actor_id, now], |row| {
            row.get::<_, String>(0)
        })?;
        for caps_json in rows.flatten() {
            let caps: Vec<String> = serde_json::from_str(&caps_json).unwrap_or_default();
            if caps.iter().any(|c| c == capability) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // ── Outbox queue ────────────────────────────────────────────────

    /// Queue `payload` for forward to the paired nest. `author` is the verified
    /// author of the post or tombstone inside — the producer has it in hand
    /// (this fn sees only bytes) — and is the row's one person column: what
    /// [`Self::outbox_purge_for_deleted_author`] finds the rows by.
    pub async fn outbox_enqueue(
        &self,
        author: &[u8; 32],
        payload: &[u8],
        entry_type: &str,
    ) -> Result<()> {
        let now = Timestamp::now().as_i64();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO outbox (payload, entry_type, created_at, author_id)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![payload, entry_type, now, author.as_slice()],
        )?;
        Ok(())
    }

    /// Every queued row's author stamp, in queue order — for the producer tests
    /// outside this module, which cannot reach the connection.
    #[cfg(test)]
    pub(crate) async fn outbox_author_stamps(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT author_id FROM outbox ORDER BY id")?;
        let stamps = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(stamps)
    }

    /// An account deletion's leg over the forward queue: drop everything the
    /// deleted author has queued EXCEPT a tombstone that can still be sent.
    ///
    /// A queued *post* must not be published to the paired nest after its
    /// author is gone. A queued *deletion* is the opposite — it is the message
    /// that removes a copy already published there, so dropping it would make
    /// the account's deletion the reason a post outlives it. It keeps the
    /// retries it has left and leaves when sent or at the retry ceiling
    /// ([`Self::outbox_record_failure`]: an authorless entry gets no retries
    /// past it). One already at the ceiling has had its last chance, so it goes
    /// now rather than rest under a deleted id.
    ///
    /// `pub(super)` and connection-taking: the purge walk calls it under its
    /// own lock (`actor_tables.rs::purge_orphaned_actor_rows`).
    pub(super) fn outbox_purge_for_deleted_author(
        conn: &Connection,
        author: &[u8; 32],
    ) -> rusqlite::Result<usize> {
        conn.execute(
            "DELETE FROM outbox
              WHERE author_id = ?1
                AND (entry_type <> ?2 OR attempts >= ?3)",
            rusqlite::params![
                author.as_slice(),
                crate::outbox::ENTRY_TYPE_FORWARDED_DELETE,
                Self::OUTBOX_MAX_ATTEMPTS
            ],
        )
    }

    /// The retry ceiling: the failure count at which an entry no grant can
    /// unblock leaves the queue (undeliverable bytes, or an author with no
    /// account here). A refusal is retried past it for ever — the grant or
    /// policy that ends one lives on the relay, where this nest cannot see it
    /// change (`private-mode.md` § Post Forwarding). An entry past it that is
    /// still queued is *stuck*: [`Self::outbox_stuck_count`].
    pub const OUTBOX_MAX_ATTEMPTS: i64 = 10;

    /// The backoff's doubling stops here: 30 s × 2^10 ≈ 8.5 h between retries
    /// from the tenth failure on.
    const OUTBOX_BACKOFF_MAX_DOUBLINGS: i64 = 10;

    /// The first backoff step; each failure doubles it, up to
    /// [`Self::OUTBOX_BACKOFF_MAX_DOUBLINGS`].
    const OUTBOX_BACKOFF_BASE_SECS: i64 = 30;

    /// The re-arm's throttle: [`Self::outbox_retry_now_for_author`] makes a
    /// backed-off entry due no sooner than this after its last attempt, so a
    /// user (or a script) re-arming every second retries each entry at most
    /// once a minute (`private-mode.md` § Post Forwarding).
    pub const OUTBOX_REARM_MIN_GAP_SECS: i64 = 60;

    /// The longest failure reason an entry keeps, in characters. Part of it is
    /// the relay's own error code, and it is served to the author's app in
    /// `fauna.pair.list`, so it is bounded and control-stripped before it is
    /// stored (`private-mode.md` § Post Forwarding).
    pub const OUTBOX_LAST_ERROR_MAX_CHARS: usize = 200;

    /// Pull every backed-off entry `author` has queued in to due — the nudge a
    /// user's own `fauna.pair.add` or `fauna.pair.forward_retry` gives, since
    /// the one-action link adds the relay's row (the grant a refusal waits on)
    /// at the same moment. Never the mechanism: past it the entries back off as
    /// before. Throttled per entry: none is made due sooner than
    /// [`Self::OUTBOX_REARM_MIN_GAP_SECS`] after its last attempt, so an entry
    /// attempted just now becomes due at that gap instead. Returns how many
    /// entries it pulled in.
    pub async fn outbox_retry_now_for_author(&self, author: &[u8; 32]) -> Result<usize> {
        let conn = self.conn.lock().await;
        // `attempts > 0` implies `last_attempt_at` is set: the one failure
        // writer (`outbox_record_failure`) stamps both in one statement.
        let rearmed = conn.execute(
            "UPDATE outbox
                SET next_retry = last_attempt_at + ?2
              WHERE author_id = ?1
                AND attempts > 0
                AND next_retry > last_attempt_at + ?2",
            rusqlite::params![
                author.as_slice(),
                Self::OUTBOX_REARM_MIN_GAP_SECS * 1_000_000,
            ],
        )?;
        Ok(rearmed)
    }

    /// Move every recorded failure of `author`'s entries `secs` into the past —
    /// their backoff and the re-arm's gap alike. Integration tests use it to
    /// simulate time passing without sleeping.
    #[doc(hidden)]
    pub async fn test_age_outbox_failures(&self, author: &[u8; 32], secs: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE outbox
                SET next_retry = next_retry - ?2, last_attempt_at = last_attempt_at - ?2
              WHERE author_id = ?1 AND attempts > 0",
            rusqlite::params![author.as_slice(), secs * 1_000_000],
        )?;
        Ok(())
    }

    /// Drop every entry `author` has queued — posts and deletions alike — the
    /// user's own "stop forwarding these" from the Nests page
    /// (`private-mode.md` § Post Forwarding → the queue is the user's to see).
    /// The posts themselves are untouched: only their relay leaves. Returns
    /// how many rows left.
    pub async fn outbox_discard_for_author(&self, author: &[u8; 32]) -> Result<usize> {
        let conn = self.conn.lock().await;
        let discarded = conn.execute(
            "DELETE FROM outbox WHERE author_id = ?1",
            rusqlite::params![author.as_slice()],
        )?;
        Ok(discarded)
    }

    /// One author's view of the queue: how many of their entries are queued
    /// (backed-off ones included), how many of those are stuck past the retry
    /// ceiling, and the most recent failure the worker recorded on any of them
    /// — the reading `fauna.pair.list` carries to the Nests page. "Most
    /// recent" is the row whose `next_retry` is latest: the failure record
    /// stamps both columns together, and the table keeps no failure timestamp
    /// of its own.
    pub async fn outbox_status_for_author(&self, author: &[u8; 32]) -> Result<OutboxAuthorStatus> {
        let conn = self.conn.lock().await;
        let (queued, stuck): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(attempts >= ?2), 0)
               FROM outbox WHERE author_id = ?1",
            rusqlite::params![author.as_slice(), Self::OUTBOX_MAX_ATTEMPTS],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let last_error: Option<String> = conn
            .query_row(
                "SELECT last_error FROM outbox
                  WHERE author_id = ?1 AND last_error IS NOT NULL
                  ORDER BY next_retry DESC, id DESC LIMIT 1",
                rusqlite::params![author.as_slice()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(OutboxAuthorStatus {
            queued,
            stuck,
            last_error,
        })
    }

    /// Every entry due now — including entries past the retry ceiling, which
    /// are refusals still waiting for a grant — taking each author's rows in
    /// turn: every author's oldest due row, then every author's second, and so
    /// on. One author's backlog therefore never fills a pass while another
    /// author's newer row waits (`private-mode.md` § Post Forwarding). Within
    /// one author the order is oldest first, so a post still goes before its
    /// own deletion.
    pub async fn outbox_pending(&self, limit: i64) -> Result<Vec<OutboxEntry>> {
        let now = Timestamp::now().as_i64();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, payload, entry_type, attempts, author_id FROM (
                SELECT id, payload, entry_type, attempts, author_id,
                       ROW_NUMBER() OVER (PARTITION BY author_id ORDER BY id) AS turn
                  FROM outbox
                 WHERE next_retry <= ?1)
             ORDER BY turn ASC, id ASC LIMIT ?2",
        )?;
        let entries = stmt
            .query_map(rusqlite::params![now, limit], |row| {
                Ok(OutboxEntry {
                    id: row.get(0)?,
                    payload: row.get(1)?,
                    entry_type: row.get(2)?,
                    attempts: row.get(3)?,
                    author_id: row.get(4)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }

    pub async fn outbox_mark_sent(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM outbox WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    /// Record one failed send of entry `id`: count it and back off (30 s,
    /// 60 s, 120 s, … capped at about 8.5 hours). At the retry ceiling the
    /// entry LEAVES when no grant can ever deliver it — `undeliverable` (this
    /// nest could not even build the request from its bytes), or its author
    /// has no `users` row (a tombstone kept past its author's deletion, or a
    /// row whose author stamp is missing) — and stays, still retrying, when it
    /// was a refusal a later grant can end. Returns whether it left.
    ///
    /// `error` is what the send failed with, kept on the row (`last_error`) so
    /// the author's app can show why their posts are waiting
    /// ([`Self::outbox_status_for_author`]) — control-stripped and cut to
    /// [`Self::OUTBOX_LAST_ERROR_MAX_CHARS`] first, since part of it is the
    /// relay's own error code.
    pub async fn outbox_record_failure(
        &self,
        id: i64,
        undeliverable: bool,
        error: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let attempts: i64 = conn.query_row(
            "SELECT attempts FROM outbox WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )?;
        let doublings = attempts.clamp(0, Self::OUTBOX_BACKOFF_MAX_DOUBLINGS) as u32;
        let backoff_secs = Self::OUTBOX_BACKOFF_BASE_SECS.saturating_mul(1i64 << doublings);
        let now = Timestamp::now().as_i64();
        let next_retry = now + backoff_secs * 1_000_000;
        conn.execute(
            "UPDATE outbox
                SET attempts = attempts + 1, next_retry = ?1, last_error = ?3,
                    last_attempt_at = ?4
              WHERE id = ?2",
            rusqlite::params![next_retry, id, bounded_failure_reason(error), now],
        )?;
        let left = conn.execute(
            "DELETE FROM outbox
              WHERE id = ?1
                AND attempts >= ?2
                AND (?3
                     OR NOT EXISTS (SELECT 1 FROM users u WHERE u.actor_id = outbox.author_id))",
            rusqlite::params![id, Self::OUTBOX_MAX_ATTEMPTS, undeliverable],
        )?;
        Ok(left > 0)
    }

    /// Total rows still queued, **including** entries backed off to a future
    /// `next_retry` (which `outbox_pending` deliberately hides). The queue-depth
    /// signal: a nest whose peer keeps refusing shows a non-zero depth long
    /// before any entry reaches the retry ceiling.
    pub async fn outbox_depth(&self) -> Result<i64> {
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Entries still queued past the retry ceiling — *stuck* forwards: each is
    /// a refusal that has lasted at least the full backoff (about 8.5 hours)
    /// and is still retrying.
    pub async fn outbox_stuck_count(&self) -> Result<i64> {
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM outbox WHERE attempts >= ?1",
            rusqlite::params![Self::OUTBOX_MAX_ATTEMPTS],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    // ── Namespace entries (encrypted sync) ──────────────────────────

    pub async fn namespace_put(
        &self,
        namespace: &[u8],
        entry_id: &[u8],
        ciphertext: &[u8],
        actor_sig: &[u8],
    ) -> Result<i64> {
        self.namespace_put_with_source(namespace, entry_id, ciphertext, actor_sig, "local")
            .await
    }

    pub async fn namespace_put_with_source(
        &self,
        namespace: &[u8],
        entry_id: &[u8],
        ciphertext: &[u8],
        actor_sig: &[u8],
        source: &str,
    ) -> Result<i64> {
        let now = Timestamp::now().as_i64();
        let conn = self.conn.lock().await;
        let seq = Self::next_namespace_seq(&conn, namespace)?;
        conn.execute(
            "INSERT OR REPLACE INTO namespace_entries
             (namespace, entry_id, seq, ciphertext, actor_sig, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![namespace, entry_id, seq, ciphertext, actor_sig, source, now],
        )?;
        Ok(seq)
    }

    pub async fn namespace_entries_since(
        &self,
        namespace: &[u8],
        since_seq: i64,
        limit: i64,
    ) -> Result<Vec<NamespaceEntry>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT entry_id, seq, ciphertext, actor_sig, source, updated_at
             FROM namespace_entries
             WHERE namespace = ?1 AND seq > ?2
             ORDER BY seq ASC LIMIT ?3",
        )?;
        let entries = stmt
            .query_map(rusqlite::params![namespace, since_seq, limit], |row| {
                Ok(NamespaceEntry {
                    entry_id: row.get(0)?,
                    seq: row.get(1)?,
                    ciphertext: row.get(2)?,
                    actor_sig: row.get(3)?,
                    source: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }

    pub async fn namespace_entries_by_id(
        &self,
        namespace: &[u8],
        entry_id: &[u8],
    ) -> Result<Vec<NamespaceEntry>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT entry_id, seq, ciphertext, actor_sig, source, updated_at
             FROM namespace_entries
             WHERE namespace = ?1 AND entry_id = ?2
             ORDER BY updated_at DESC",
        )?;
        let entries = stmt
            .query_map(rusqlite::params![namespace, entry_id], |row| {
                Ok(NamespaceEntry {
                    entry_id: row.get(0)?,
                    seq: row.get(1)?,
                    ciphertext: row.get(2)?,
                    actor_sig: row.get(3)?,
                    source: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }

    pub async fn namespace_conflicts(
        &self,
        namespace: &[u8],
        entry_id: &[u8],
    ) -> Result<Vec<NamespaceEntry>> {
        // Conflicts = multiple sources for the same (namespace, entry_id)
        self.namespace_entries_by_id(namespace, entry_id).await
    }

    /// List all active pairings for a given actor.
    pub async fn list_pairings_for_actor(&self, actor_id: &[u8]) -> Result<Vec<NestPairing>> {
        let now = Timestamp::now().as_i64();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT actor_id, private_nest_id, capabilities, expires_at, created_at, nest_url, label
             FROM nest_pairings
             WHERE actor_id = ?1 AND (expires_at IS NULL OR expires_at > ?2)",
        )?;
        let pairings = stmt
            .query_map(rusqlite::params![actor_id, now], |row| {
                let caps_json: String = row.get(2)?;
                Ok(NestPairing {
                    actor_id: row.get(0)?,
                    private_nest_id: row.get(1)?,
                    capabilities: serde_json::from_str(&caps_json).unwrap_or_default(),
                    expires_at: row.get(3)?,
                    created_at: row.get(4)?,
                    nest_url: row.get(5)?,
                    label: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(pairings)
    }

    /// List all pairings that authorize post forwarding (the `post_forward`
    /// capability) — the federation pairing set. Re-keyed from the retired
    /// `"federation"` capability string by the per-user-pairing reshape: under
    /// the canonical set (`mls_pull`/`namespace_sync`/`post_forward`), this is
    /// the set of relay nests through which the actor's posts are forwarded.
    pub async fn list_federation_pairings(&self) -> Result<Vec<NestPairing>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT actor_id, private_nest_id, capabilities, expires_at, created_at, nest_url, label
             FROM nest_pairings
             WHERE capabilities LIKE '%post_forward%'",
        )?;
        let pairings = stmt
            .query_map([], |row| {
                let caps_json: String = row.get(2)?;
                Ok(NestPairing {
                    actor_id: row.get(0)?,
                    private_nest_id: row.get(1)?,
                    capabilities: serde_json::from_str(&caps_json).unwrap_or_default(),
                    expires_at: row.get(3)?,
                    created_at: row.get(4)?,
                    nest_url: row.get(5)?,
                    label: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(pairings)
    }

    /// Every local pairing row as the private-side workers read it
    /// (`nest_sync_worker::refresh_pairing_targets`): expired rows included,
    /// because a row's peer identity pins its URL whether or not it still
    /// authorizes anything, and whether the row's actor is an admin of this
    /// nest **now** — the one fact the address-guard exemption follows
    /// (`private-mode.md` § Pairing Flow). On a private nest `peer_nest_id` is
    /// the paired public nest's id.
    pub async fn list_pairing_targets(&self) -> Result<Vec<PairingTargetRow>> {
        self.pairing_target_rows(None).await
    }

    /// [`Self::list_pairing_targets`] for one author's rows — what the post
    /// forwarding producer and the outbox worker consult.
    pub async fn author_pairing_targets(&self, actor_id: &[u8]) -> Result<Vec<PairingTargetRow>> {
        self.pairing_target_rows(Some(actor_id)).await
    }

    async fn pairing_target_rows(&self, actor_id: Option<&[u8]>) -> Result<Vec<PairingTargetRow>> {
        let now = Timestamp::now().as_i64();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT p.actor_id, p.private_nest_id, p.nest_url, p.capabilities,
                    (p.expires_at IS NOT NULL AND p.expires_at <= ?1),
                    EXISTS (SELECT 1 FROM admin_actor_ids a WHERE a.actor_id = p.actor_id)
             FROM nest_pairings p
             WHERE ?2 IS NULL OR p.actor_id = ?2
             ORDER BY p.actor_id, p.private_nest_id",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![now, actor_id], |row| {
                let caps_json: String = row.get(3)?;
                Ok(PairingTargetRow {
                    actor_id: row.get(0)?,
                    peer_nest_id: row.get(1)?,
                    nest_url: row.get(2)?,
                    capabilities: serde_json::from_str(&caps_json).unwrap_or_default(),
                    expired: row.get(4)?,
                    actor_is_admin: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn next_namespace_seq(conn: &Connection, namespace: &[u8]) -> rusqlite::Result<i64> {
        conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM namespace_entries WHERE namespace = ?1",
            rusqlite::params![namespace],
            |row| row.get(0),
        )
    }
}

/// A failure reason as an outbox entry keeps it: control characters dropped
/// and cut to [`CacheDb::OUTBOX_LAST_ERROR_MAX_CHARS`] characters, a cut one
/// ending in `…`. Part of the reason is the relay's own error code, and the
/// author's app shows it on the Nests page.
pub(crate) fn bounded_failure_reason(error: &str) -> String {
    let mut chars = error.chars().filter(|c| !c.is_control());
    let mut reason: String = chars
        .by_ref()
        .take(CacheDb::OUTBOX_LAST_ERROR_MAX_CHARS)
        .collect();
    if chars.next().is_some() {
        reason.push('…');
    }
    reason
}
