//! Email filter, auto-reply, and domain methods.

use super::{CacheDb, now_epoch_millis, now_epoch_secs};
use super::{DomainUserRow, EmailDomainRow, EmailFilterRow};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

impl CacheDb {
    // ── Email filter CRUD ──────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub async fn create_email_filter(
        &self,
        owner: &[u8; 32],
        name: &str,
        rules: &[u8],
        combination: &str,
        action: &str,
        priority: i32,
        continue_on_match: bool,
        forward_redirect: bool,
    ) -> Result<i64> {
        let owner = owner.to_vec();
        let name = name.to_string();
        let rules = rules.to_vec();
        let combination = combination.to_string();
        let action = action.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        conn.query_row(
            "INSERT INTO email_filters (owner, name, rules, combination, action, priority, created_at, continue_on_match, forward_redirect)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) RETURNING id",
            rusqlite::params![owner, name, rules, combination, action, priority, now, continue_on_match, forward_redirect],
            |row| row.get(0),
        )
        .context("create_email_filter")
    }

    pub async fn list_email_filters(&self, owner: &[u8; 32]) -> Result<Vec<EmailFilterRow>> {
        let owner = owner.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, owner, name, rules, combination, action, priority, created_at, continue_on_match, forward_redirect
                 FROM email_filters WHERE owner = ?1 ORDER BY priority ASC, id ASC",
            )
            .context("prepare list_email_filters")?;
        let rows = stmt
            .query_map(rusqlite::params![owner], |row| {
                Ok(EmailFilterRow {
                    id: row.get(0)?,
                    owner: row.get(1)?,
                    name: row.get(2)?,
                    rules: row.get(3)?,
                    combination: row.get(4)?,
                    action: row.get(5)?,
                    priority: row.get(6)?,
                    created_at: row.get(7)?,
                    continue_on_match: row.get(8)?,
                    forward_redirect: row.get(9)?,
                })
            })
            .context("query list_email_filters")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read email_filter row")?);
        }
        Ok(results)
    }

    pub async fn get_email_filter(&self, id: i64) -> Result<Option<EmailFilterRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, owner, name, rules, combination, action, priority, created_at, continue_on_match, forward_redirect
             FROM email_filters WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id], |row| {
            Ok(EmailFilterRow {
                id: row.get(0)?,
                owner: row.get(1)?,
                name: row.get(2)?,
                rules: row.get(3)?,
                combination: row.get(4)?,
                action: row.get(5)?,
                priority: row.get(6)?,
                created_at: row.get(7)?,
                continue_on_match: row.get(8)?,
                forward_redirect: row.get(9)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_email_filter(
        &self,
        id: i64,
        owner: &[u8; 32],
        name: &str,
        rules: &[u8],
        combination: &str,
        action: &str,
        priority: i32,
        continue_on_match: bool,
        forward_redirect: bool,
    ) -> Result<bool> {
        let owner = owner.to_vec();
        let name = name.to_string();
        let rules = rules.to_vec();
        let combination = combination.to_string();
        let action = action.to_string();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE email_filters SET name=?1, rules=?2, combination=?3, action=?4, priority=?5, continue_on_match=?6, forward_redirect=?7
                 WHERE id = ?8 AND owner = ?9",
                rusqlite::params![name, rules, combination, action, priority, continue_on_match, forward_redirect, id, owner],
            )
            .context("update_email_filter")?;
        Ok(changed > 0)
    }

    pub async fn delete_email_filter(&self, id: i64, owner: &[u8; 32]) -> Result<bool> {
        let owner = owner.to_vec();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "DELETE FROM email_filters WHERE id = ?1 AND owner = ?2",
                rusqlite::params![id, owner],
            )
            .context("delete_email_filter")?;
        Ok(changed > 0)
    }

    // ── Auto-reply rate limiting ─────────────────────────────────────────

    /// Check whether an auto-reply should be sent (no recent reply within interval).
    /// Returns `true` if no auto-reply was sent within the last `interval_hours`.
    pub async fn check_auto_reply(
        &self,
        recipient_id: &[u8; 32],
        sender_hash: &[u8; 32],
        interval_hours: u32,
    ) -> Result<bool> {
        let recipient_id = recipient_id.to_vec();
        let sender_hash = sender_hash.to_vec();
        let conn = self.conn.lock().await;
        let cutoff = now_epoch_millis() - (interval_hours as i64 * 3_600_000);
        let recent: Option<i64> = conn
            .query_row(
                "SELECT last_sent_at FROM auto_reply_log WHERE recipient_id = ?1 AND sender_hash = ?2 AND last_sent_at > ?3",
                rusqlite::params![recipient_id, sender_hash, cutoff],
                |row| row.get(0),
            )
            .optional()
            .context("check_auto_reply")?;
        Ok(recent.is_none())
    }

    /// Record that an auto-reply was sent to a sender.
    pub async fn record_auto_reply(
        &self,
        recipient_id: &[u8; 32],
        sender_hash: &[u8; 32],
    ) -> Result<()> {
        let recipient_id = recipient_id.to_vec();
        let sender_hash = sender_hash.to_vec();
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        conn.execute(
            "INSERT OR REPLACE INTO auto_reply_log (recipient_id, sender_hash, last_sent_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![recipient_id, sender_hash, now],
        )
        .context("record_auto_reply")?;
        Ok(())
    }

    /// Atomically claim an auto-reply slot for `(recipient_id, sender_hash)`:
    /// if no reply was recorded within the last `interval_hours`, record one
    /// now and return `true`; otherwise return `false` and leave the existing
    /// timestamp. The check and the insert run under **one** connection lock so
    /// two concurrent inbound messages from the same sender can't both claim —
    /// the production path for `fauna.bridges.claim_auto_reply` (the separate
    /// `check_auto_reply`/`record_auto_reply` pair would race across two locks).
    pub async fn try_claim_auto_reply(
        &self,
        recipient_id: &[u8; 32],
        sender_hash: &[u8; 32],
        interval_hours: u32,
    ) -> Result<bool> {
        let recipient_id = recipient_id.to_vec();
        let sender_hash = sender_hash.to_vec();
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        let cutoff = now - (interval_hours as i64 * 3_600_000);
        let recent: Option<i64> = conn
            .query_row(
                "SELECT last_sent_at FROM auto_reply_log WHERE recipient_id = ?1 AND sender_hash = ?2 AND last_sent_at > ?3",
                rusqlite::params![recipient_id, sender_hash, cutoff],
                |row| row.get(0),
            )
            .optional()
            .context("try_claim_auto_reply: check")?;
        if recent.is_some() {
            return Ok(false);
        }
        conn.execute(
            "INSERT OR REPLACE INTO auto_reply_log (recipient_id, sender_hash, last_sent_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![recipient_id, sender_hash, now],
        )
        .context("try_claim_auto_reply: record")?;
        Ok(true)
    }

    // ── Email domain CRUD ───────────────────────────────────────────────

    pub async fn create_email_domain(
        &self,
        domain: &str,
        dkim_selector: &str,
        ed25519_selector: &str,
    ) -> Result<()> {
        let domain = domain.to_string();
        let dkim_selector = dkim_selector.to_string();
        let ed25519_selector = ed25519_selector.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT INTO email_domains (domain, dkim_selector, dkim_ed25519_selector, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![domain, dkim_selector, ed25519_selector, now],
        )
        .context("create_email_domain")?;
        Ok(())
    }

    pub async fn list_email_domains(&self) -> Result<Vec<EmailDomainRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT domain, dkim_selector, dkim_ed25519_selector, enabled, created_at
                 FROM email_domains ORDER BY domain ASC",
            )
            .context("prepare list_email_domains")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(EmailDomainRow {
                    domain: row.get(0)?,
                    dkim_selector: row.get(1)?,
                    dkim_ed25519_selector: row.get(2)?,
                    enabled: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                })
            })
            .context("query list_email_domains")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read email_domain row")?);
        }
        Ok(results)
    }

    /// Delete an email domain. Fails if any users are still assigned to it.
    pub async fn delete_email_domain(&self, domain: &str) -> Result<bool> {
        let domain = domain.to_string();
        let conn = self.conn.lock().await;
        let user_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM email_domain_users WHERE domain = ?1",
                rusqlite::params![domain],
                |row| row.get(0),
            )
            .context("count domain users")?;
        if user_count > 0 {
            anyhow::bail!("cannot delete domain with {user_count} assigned user(s)");
        }
        let changed = conn
            .execute(
                "DELETE FROM email_domains WHERE domain = ?1",
                rusqlite::params![domain],
            )
            .context("delete_email_domain")?;
        Ok(changed > 0)
    }

    pub async fn assign_domain_user(
        &self,
        domain: &str,
        actor_id: &[u8; 32],
        local_part: &str,
    ) -> Result<()> {
        let domain = domain.to_string();
        let actor_id = actor_id.to_vec();
        let local_part = local_part.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT INTO email_domain_users (domain, actor_id, local_part, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![domain, actor_id, local_part, now],
        )
        .context("assign_domain_user")?;
        Ok(())
    }

    pub async fn list_domain_users(&self, domain: &str) -> Result<Vec<DomainUserRow>> {
        let domain = domain.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT domain, actor_id, local_part, created_at
                 FROM email_domain_users WHERE domain = ?1 ORDER BY local_part ASC",
            )
            .context("prepare list_domain_users")?;
        let rows = stmt
            .query_map(rusqlite::params![domain], |row| {
                Ok(DomainUserRow {
                    domain: row.get(0)?,
                    actor_id: row.get(1)?,
                    local_part: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .context("query list_domain_users")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read domain_user row")?);
        }
        Ok(results)
    }

    pub async fn remove_domain_user(&self, domain: &str, actor_id: &[u8; 32]) -> Result<bool> {
        let domain = domain.to_string();
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "DELETE FROM email_domain_users WHERE domain = ?1 AND actor_id = ?2",
                rusqlite::params![domain, actor_id],
            )
            .context("remove_domain_user")?;
        Ok(changed > 0)
    }

    pub async fn resolve_email_address_by_domain(
        &self,
        local_part: &str,
        domain: &str,
    ) -> Result<Option<[u8; 32]>> {
        let local_part = local_part.to_string();
        let domain = domain.to_string();
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT actor_id FROM email_domain_users
                 WHERE local_part = ?1 AND domain = ?2 LIMIT 1",
                rusqlite::params![local_part, domain],
                |row| {
                    let blob: Vec<u8> = row.get(0)?;
                    Ok(blob)
                },
            )
            .optional()
            .context("resolve_email_address_by_domain")?;
        match result {
            Some(blob) => {
                let arr: [u8; 32] = blob
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("actor_id is not 32 bytes"))?;
                Ok(Some(arr))
            }
            None => Ok(None),
        }
    }

    pub async fn list_all_email_domains_set(&self) -> Result<std::collections::HashSet<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT domain FROM email_domains WHERE enabled = 1")
            .context("prepare list_all_email_domains_set")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("query list_all_email_domains_set")?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            set.insert(row.context("read domain")?);
        }
        Ok(set)
    }

    pub async fn get_domain_selectors(&self, domain: &str) -> Result<Option<(String, String)>> {
        let domain = domain.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT dkim_selector, dkim_ed25519_selector FROM email_domains WHERE domain = ?1",
            rusqlite::params![domain],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .context("get_domain_selectors")
    }
}
