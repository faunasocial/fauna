//! The domain-expiry watch's persisted record.
//!
//! Owner: `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Domain
//! loss → *Detection*. The table is single-row and deployment-scoped; the DDL's
//! own comment (`migrations.rs`, `MIGRATIONS_DOMAIN_EXPIRY`) argues why.
//!
//! This module is storage only. The fetch lives in `crate::domain_expiry`, and
//! the *decision* — what a record means — lives in
//! `fauna_protocol::domain_expiry::evaluate`, shared with the client feeder that
//! actually raises the banner.

use anyhow::{Context, Result};
use fauna_protocol::domain_expiry::DomainExpiryRecord;
use rusqlite::OptionalExtension;

use super::CacheDb;

impl CacheDb {
    /// Overwrite the deployment's domain-expiry record.
    ///
    /// Whole-row replace, not a merge: every field is an observation from one
    /// attempt, and mixing a fresh `outcome` with a stale `expires_at` would
    /// manufacture a record no fetch ever saw. A failed attempt therefore
    /// deliberately *loses* the previous expiry — the client's `Unreachable`
    /// verdict is what keeps a standing alert standing, not a retained value
    /// (`fauna_protocol::domain_expiry::evaluate`).
    pub async fn put_domain_expiry(&self, record: &DomainExpiryRecord) -> Result<()> {
        let statuses = serde_json::to_string(&record.statuses).context("encode rdap statuses")?;
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO domain_expiry (id, domain, expires_at, statuses, fetched_at, outcome, detail)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 domain = excluded.domain,
                 expires_at = excluded.expires_at,
                 statuses = excluded.statuses,
                 fetched_at = excluded.fetched_at,
                 outcome = excluded.outcome,
                 detail = excluded.detail",
            rusqlite::params![
                record.domain,
                record.expires_at,
                statuses,
                record.fetched_at,
                record.outcome,
                record.detail,
            ],
        )
        .context("put domain expiry record")?;
        Ok(())
    }

    /// Read the deployment's domain-expiry record, or `None` if the watch has
    /// not completed a single attempt yet.
    pub async fn get_domain_expiry(&self) -> Result<Option<DomainExpiryRecord>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT domain, expires_at, statuses, fetched_at, outcome, detail
             FROM domain_expiry WHERE id = 1",
            [],
            |row| {
                let statuses_json: String = row.get(2)?;
                Ok(DomainExpiryRecord {
                    domain: row.get(0)?,
                    expires_at: row.get(1)?,
                    // A stored blob we cannot decode folds to "no statuses",
                    // never to an error: the direction is stated because it
                    // matters (`nest/common.md` § Unreadable stored values).
                    // Losing the status arm on a corrupt row is the *quiet*
                    // direction, which is wrong for an alarm — but the row is
                    // rewritten in full by the next watch tick (daily-class),
                    // so the window is one tick and self-healing, whereas
                    // failing the read would take the *whole* feeder down,
                    // including the date arm that is still perfectly readable.
                    statuses: serde_json::from_str(&statuses_json).unwrap_or_default(),
                    fetched_at: row.get(3)?,
                    outcome: row.get(4)?,
                    detail: row.get(5)?,
                    extra: Default::default(),
                })
            },
        )
        .optional()
        .context("get domain expiry record")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::domain_expiry::outcomes;

    fn db() -> CacheDb {
        CacheDb::open_in_memory().unwrap()
    }

    fn record() -> DomainExpiryRecord {
        DomainExpiryRecord {
            domain: "example.org".into(),
            expires_at: Some(1_800_000_000),
            statuses: vec!["client hold".into(), "active".into()],
            fetched_at: 1_799_000_000,
            outcome: outcomes::CHECKED.into(),
            detail: None,
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn absent_before_the_first_attempt() {
        let db = db();
        assert_eq!(db.get_domain_expiry().await.unwrap(), None);
    }

    #[tokio::test]
    async fn round_trips_including_the_status_list() {
        let db = db();
        db.put_domain_expiry(&record()).await.unwrap();
        assert_eq!(db.get_domain_expiry().await.unwrap(), Some(record()));
    }

    /// The upsert must replace, not accumulate — the table is single-row by
    /// design and a second attempt is a *replacement* observation.
    #[tokio::test]
    async fn a_second_attempt_replaces_the_first() {
        let db = db();
        db.put_domain_expiry(&record()).await.unwrap();

        let failed = DomainExpiryRecord {
            domain: "example.org".into(),
            expires_at: None,
            statuses: vec![],
            fetched_at: 1_799_100_000,
            outcome: outcomes::FAILED.into(),
            detail: Some("503".into()),
            extra: Default::default(),
        };
        db.put_domain_expiry(&failed).await.unwrap();

        assert_eq!(db.get_domain_expiry().await.unwrap(), Some(failed));
        let conn = db.conn.lock().await;
        let n: i64 = conn
            .query_row("SELECT count(*) FROM domain_expiry", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the table is single-row by construction");
    }
}
