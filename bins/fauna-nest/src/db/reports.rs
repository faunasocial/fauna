//! Distributed report sharing — report capture + the k-gated aggregate
//! writer (`docs/goal/behavior/report-sharing.md` § Report capture, § The
//! k-anonymity choke point, § The aggregate).
//!
//! `content_reports` rows are caller-scoped facts ("this reporter flagged
//! this content-hash") that are NEVER served anywhere: the only read
//! surfaces are the functions below, every one of which routes through the
//! shared k-gate (`fauna_core::scoring::reports::exposed_report_count`)
//! before anything derived from the table becomes observable. No RPC —
//! admin included — SELECTs the table directly.

use anyhow::{Context, Result};
use fauna_core::scoring::{TIER_COMMUNITY, reports, scorer_version};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_millis};

/// Factor-family prefix for the mark-as-spam reports (`report:spam`). Used to
/// scope the opt-out sweep so `share_reports` and `share_signals` stay disjoint.
pub(super) const REPORT_FACTOR_PREFIX: &str = "report:";
/// Factor-family prefix for the Layer-B engagement-cue signals
/// (`signal:watch-complete` / `signal:skip`; engagement-cues.md § Layer B).
pub(super) const SIGNAL_FACTOR_PREFIX: &str = "signal:";

/// `spam_preferences` column gating report-family inserts — folded directly
/// into [`CacheDb::insert_content_report`]'s own SQL so the opt-in read and
/// the row insert are ONE statement, closing the check/insert race an
/// opt-out's separate lock acquisition could otherwise win.
pub(super) const REPORT_PREF_COLUMN: &str = "share_reports";
/// `spam_preferences` column gating signal-family inserts — the
/// `share_signals` sibling of [`REPORT_PREF_COLUMN`].
pub(super) const SIGNAL_PREF_COLUMN: &str = "share_signals";

/// An aggregate key affected by a report mutation — the caller recomputes
/// each after the mutation lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportKey {
    pub content_hash: [u8; 32],
    pub factor: String,
    pub content_kind: String,
}

impl CacheDb {
    /// Shared body for `share_reports_enabled`/`share_signals_enabled` — `sql`
    /// carries the one thing that differs, the `spam_preferences` column name
    /// baked into the `SELECT`. `&'static str` holds callers to a compile-time
    /// literal, so there is no identifier-interpolation shape to worry about.
    /// `what` is the `.context()` label.
    pub(super) async fn share_pref_enabled(
        &self,
        sql: &'static str,
        actor_id: &[u8; 32],
        what: &'static str,
    ) -> Result<bool> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let enabled: Option<i64> = conn
            .query_row(sql, rusqlite::params![&actor[..]], |row| row.get(0))
            .optional()
            .context(what)?;
        Ok(enabled.unwrap_or(0) != 0)
    }

    /// Shared body for `set_share_reports`/`set_share_signals` — `sql` (a
    /// compile-time literal per caller, same reasoning as
    /// [`Self::share_pref_enabled`]) carries the column-specific
    /// `INSERT .. ON CONFLICT`; `prefix` scopes the opt-out sweep to the
    /// caller's own factor family and is likewise held to a compile-time
    /// literal — it is interpolated into a `LIKE` pattern for the sweep's
    /// `DELETE`, so a runtime-built value here could widen it across
    /// factor families.
    pub(super) async fn set_share_pref(
        &self,
        sql: &'static str,
        actor_id: &[u8; 32],
        enabled: bool,
        prefix: &'static str,
        what: &'static str,
    ) -> Result<()> {
        let actor = *actor_id;
        {
            let conn = self.conn.lock().await;
            let now = now_epoch_millis();
            conn.execute(sql, rusqlite::params![&actor[..], enabled as i64, now])
                .context(what)?;
        }
        if !enabled {
            let affected = self.delete_reports_by_reporter(&actor, prefix).await?;
            for key in affected {
                self.recompute_report_score(&key).await?;
            }
        }
        Ok(())
    }

    /// Whether the actor opted into report sharing
    /// (`spam_preferences.share_reports`, default off).
    pub async fn share_reports_enabled(&self, actor_id: &[u8; 32]) -> Result<bool> {
        self.share_pref_enabled(
            "SELECT share_reports FROM spam_preferences WHERE actor_id = ?1",
            actor_id,
            "read share_reports",
        )
        .await
    }

    /// Set the report-sharing opt-in. Opting OUT deletes every existing row
    /// the reporter contributed (user-controls-their-data: a withdrawn
    /// judgment leaves no residue) and recomputes each affected aggregate —
    /// which may fall below k and withdraw its bus rows.
    pub async fn set_share_reports(&self, actor_id: &[u8; 32], enabled: bool) -> Result<()> {
        self.set_share_pref(
            "INSERT INTO spam_preferences (actor_id, share_reports, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET
                share_reports = excluded.share_reports,
                updated_at = excluded.updated_at",
            actor_id,
            enabled,
            REPORT_FACTOR_PREFIX,
            "set share_reports",
        )
        .await
    }

    /// Record one explicit flag (idempotent — flagging twice is one report),
    /// gated on `pref_column` (`REPORT_PREF_COLUMN` / `SIGNAL_PREF_COLUMN`)
    /// **inside the same statement as the insert** — the opt-in read and the
    /// row write are one atomic step, so an opt-out committing between a
    /// caller's own earlier pref read and this call can no longer let the
    /// contribution land anyway. `pref_column` is always a
    /// compile-time `&'static str` literal from the caller, never built at
    /// runtime — there is no identifier-interpolation shape to worry about
    /// (mirrors [`Self::share_pref_enabled`]). Returns `true` when a new row
    /// landed (the caller then recomputes).
    pub async fn insert_content_report(
        &self,
        key: &ReportKey,
        reporter: &[u8; 32],
        pref_column: &'static str,
    ) -> Result<bool> {
        let k = key.clone();
        let rep = *reporter;
        let sql = format!(
            "INSERT OR IGNORE INTO content_reports
                (content_hash, factor, reporter, content_kind, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5
             WHERE (SELECT {pref_column} FROM spam_preferences WHERE actor_id = ?3) = 1"
        );
        let conn = self.conn.lock().await;
        let inserted = conn
            .execute(
                &sql,
                rusqlite::params![
                    &k.content_hash[..],
                    k.factor,
                    &rep[..],
                    k.content_kind,
                    now_epoch_millis(),
                ],
            )
            .context("insert content report")?;
        Ok(inserted > 0)
    }

    /// Withdraw one reporter's flag (ham-correction / undo). Returns `true`
    /// when a row was removed (the caller then recomputes).
    pub async fn delete_content_report(
        &self,
        key: &ReportKey,
        reporter: &[u8; 32],
    ) -> Result<bool> {
        let k = key.clone();
        let rep = *reporter;
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM content_reports
                 WHERE content_hash = ?1 AND factor = ?2 AND reporter = ?3",
                rusqlite::params![&k.content_hash[..], k.factor, &rep[..]],
            )
            .context("delete content report")?;
        Ok(deleted > 0)
    }

    /// Opt-out sweep: delete every row this reporter contributed **within one
    /// factor family** (`factor_prefix`, e.g. `"report:"` or `"signal:"`),
    /// returning the affected aggregate keys for recomputation. The prefix keeps
    /// the two independent opt-ins (`share_reports` / `share_signals`) disjoint —
    /// opting out of report sharing must never wipe a still-opted-in user's
    /// engagement-cue rows, and vice-versa.
    pub(super) async fn delete_reports_by_reporter(
        &self,
        reporter: &[u8; 32],
        factor_prefix: &str,
    ) -> Result<Vec<ReportKey>> {
        let rep = *reporter;
        let like = format!("{factor_prefix}%");
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT content_hash, factor, content_kind
                 FROM content_reports WHERE reporter = ?1 AND factor LIKE ?2",
            )
            .context("prepare reporter sweep")?;
        let keys: Vec<ReportKey> = stmt
            .query_map(rusqlite::params![&rep[..], like], |row| {
                let hash: Vec<u8> = row.get(0)?;
                Ok((hash, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .context("query reporter rows")?
            .filter_map(|r| r.ok())
            .filter_map(|(hash, factor, kind)| {
                let content_hash: [u8; 32] = hash.as_slice().try_into().ok()?;
                Some(ReportKey {
                    content_hash,
                    factor,
                    content_kind: kind,
                })
            })
            .collect();
        drop(stmt);
        conn.execute(
            "DELETE FROM content_reports WHERE reporter = ?1 AND factor LIKE ?2",
            rusqlite::params![&rep[..], like],
        )
        .context("delete reporter rows")?;
        Ok(keys)
    }

    /// Resolve the canonical report-hash for a stored mail message (the
    /// mirror lookup the `\Junk`-train capture uses). `None` for a message
    /// with no hash (an IMAP APPEND stores none) — it cannot aggregate.
    pub async fn report_hash_for_message(&self, message_id: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let cid = fauna_cbor::Cid::from_digest_dag_cbor(*message_id);
        let conn = self.conn.lock().await;
        let hash: Option<Option<Vec<u8>>> = conn
            .query_row(
                "SELECT report_hash FROM segment_records
                 WHERE record_cid = ?1 AND kind = 'mail' AND tombstoned = 0",
                rusqlite::params![&cid.as_bytes()[..]],
                |row| row.get(0),
            )
            .optional()
            .context("lookup report_hash for message")?;
        Ok(hash
            .flatten()
            .and_then(|h| <[u8; 32]>::try_from(h.as_slice()).ok()))
    }

    /// Recompute one aggregate and write/withdraw its bus rows — THE
    /// k-anonymity choke point's writer half. At ≥ k local reporters, one
    /// tier-3 `ScoreEntry` row lands per local item carrying the hash
    /// (`content_scores` keys on the per-copy storage id, so the writer
    /// joins through the `segment_records.report_hash` mirror for mail; a
    /// post IS its hash). Below k — including after opt-outs — every row for
    /// the factor+items is withdrawn.
    pub async fn recompute_report_score(&self, key: &ReportKey) -> Result<()> {
        let k = key.clone();
        let conn = self.conn.lock().await;
        let local: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM content_reports
                 WHERE content_hash = ?1 AND factor = ?2",
                rusqlite::params![&k.content_hash[..], k.factor],
                |row| row.get::<_, i64>(0).map(|c| c.max(0) as u32),
            )
            .context("count local reporters")?;
        // ONE flat corroboration bucket across all peers — presence, never
        // magnitude (report-sharing.md § The aggregate; federation.md
        // hostile-signer invariant).
        let peer_bucket: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM peer_content_reports
                               WHERE content_hash = ?1 AND factor = ?2)",
                rusqlite::params![&k.content_hash[..], k.factor],
                |row| row.get::<_, i64>(0).map(|v| v != 0),
            )
            .context("probe peer bucket")?;
        let score = reports::report_score_pm(local, peer_bucket);
        let exposed = reports::exposed_report_count(local).is_some() || peer_bucket;

        // The per-copy items this aggregate attaches to — kind-agnostic (a
        // peer entry carries no kind): every local mail copy via the
        // `segment_records.report_hash` mirror join, plus the post whose
        // content-addressed id IS the hash iff it actually exists (never a
        // blind row for content this nest has never seen).
        let mut items: Vec<(Vec<u8>, &'static str, Option<Vec<u8>>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT substr(record_cid, 5), scope_id FROM segment_records
                     WHERE report_hash = ?1 AND kind = 'mail' AND tombstoned = 0",
                )
                .context("prepare mail item join")?;
            stmt.query_map(rusqlite::params![&k.content_hash[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    "mail",
                    Some(row.get::<_, Vec<u8>>(1)?),
                ))
            })
            .context("query mail items")?
            .filter_map(|r| r.ok())
            .collect()
        };
        let post_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM content_meta WHERE content_id = ?1)",
                rusqlite::params![&k.content_hash[..]],
                |row| row.get::<_, i64>(0).map(|v| v != 0),
            )
            .context("probe post existence")?;
        if post_exists {
            items.push((k.content_hash.to_vec(), "post", None));
        }

        if exposed {
            let now = now_epoch_millis();
            for (content_id, kind, actor) in &items {
                conn.execute(
                    "INSERT OR REPLACE INTO content_scores (
                         content_id, content_kind, factor, score, tier,
                         scorer_version, scored_at, actor_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        content_id,
                        kind,
                        // Factor-parametric: `report:spam` for the mark-as-spam
                        // path, `signal:{watch-complete,skip}` for the Layer-B
                        // cue path (engagement-cues.md § Layer B nest legs) —
                        // both ride this one writer + the same k-gate/curve.
                        k.factor,
                        score,
                        TIER_COMMUNITY as i64,
                        // Signals share the `report_score_pm` curve, so they
                        // share its version (the AUTH-factors precedent —
                        // `scorer_version::REPORT` doc).
                        scorer_version::REPORT as i64,
                        now,
                        actor,
                    ],
                )
                .context("write report bus row")?;
            }
        } else {
            for (content_id, _, _) in &items {
                conn.execute(
                    "DELETE FROM content_scores
                     WHERE content_id = ?1 AND factor = ?2",
                    rusqlite::params![content_id, k.factor],
                )
                .context("withdraw report bus row")?;
            }
        }
        Ok(())
    }

    /// Test-only read of one item's `report:spam` bus row (`None` = no row).
    /// NOT a production surface — production reads go through the bus
    /// (`get_content_scores`), and aggregate reads through the k-gate.
    #[cfg(test)]
    pub(crate) async fn test_report_row_score(&self, content_id: &[u8; 32]) -> Option<i64> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT score FROM content_scores
             WHERE content_id = ?1 AND factor = 'report:spam'",
            rusqlite::params![&content_id[..]],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .expect("read report bus row")
    }

    /// Test-only count of all `content_reports` rows.
    #[cfg(test)]
    pub(crate) async fn test_content_reports_count(&self) -> i64 {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT COUNT(*) FROM content_reports", [], |row| row.get(0))
            .expect("count content_reports")
    }

    /// The k-gated score for an ingest-time join (`report-sharing.md` § The
    /// aggregate — a late copy of a known campaign is scored on arrival).
    /// `None` when nothing is exposable (below the local floor AND no peer
    /// corroboration bucket).
    pub async fn gated_report_score(
        &self,
        content_hash: &[u8; 32],
        factor_name: &str,
    ) -> Result<Option<i64>> {
        let hash = *content_hash;
        let factor_name = factor_name.to_string();
        let conn = self.conn.lock().await;
        let local: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM content_reports
                 WHERE content_hash = ?1 AND factor = ?2",
                rusqlite::params![&hash[..], factor_name],
                |row| row.get::<_, i64>(0).map(|c| c.max(0) as u32),
            )
            .context("count local reporters (ingest join)")?;
        let peer_bucket: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM peer_content_reports
                               WHERE content_hash = ?1 AND factor = ?2)",
                rusqlite::params![&hash[..], factor_name],
                |row| row.get::<_, i64>(0).map(|v| v != 0),
            )
            .context("probe peer bucket (ingest join)")?;
        let exposed = reports::exposed_report_count(local).is_some() || peer_bucket;
        Ok(exposed.then(|| reports::report_score_pm(local, peer_bucket)))
    }

    /// Import one peer's ≥k aggregate entry (`report-sharing.md` § Federation
    /// exchange): latest-epoch-wins per `(hash, factor, peer)`; a stale epoch
    /// is a harmless replay (ignored). Enforces the per-peer stored-row cap
    /// ([`MAX_PEER_REPORT_ROWS`]) by evicting that peer's oldest-updated rows
    /// — reports are per-item (an unbounded hash universe), so a hostile peer
    /// must not grow the DB without bound. Returns `true` when the row landed
    /// (the caller then recomputes the aggregate).
    pub async fn upsert_peer_report(
        &self,
        peer_nest_id: &[u8; 32],
        content_hash: &[u8; 32],
        factor_name: &str,
        claimed_count: u32,
        epoch: i64,
    ) -> Result<bool> {
        let peer = *peer_nest_id;
        let hash = *content_hash;
        let factor_name = factor_name.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        let changed = conn
            .execute(
                "INSERT INTO peer_content_reports
                    (content_hash, factor, peer_nest_id, claimed_count, epoch, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(content_hash, factor, peer_nest_id) DO UPDATE SET
                    claimed_count = excluded.claimed_count,
                    epoch = excluded.epoch,
                    updated_at = excluded.updated_at
                 WHERE excluded.epoch >= peer_content_reports.epoch",
                rusqlite::params![
                    &hash[..],
                    factor_name,
                    &peer[..],
                    claimed_count as i64,
                    epoch,
                    now
                ],
            )
            .context("upsert peer report")?;
        // Per-peer cap: evict this peer's oldest-updated rows past the cap.
        conn.execute(
            "DELETE FROM peer_content_reports
             WHERE peer_nest_id = ?1
               AND rowid NOT IN (
                   SELECT rowid FROM peer_content_reports
                   WHERE peer_nest_id = ?1
                   ORDER BY updated_at DESC
                   LIMIT ?2)",
            rusqlite::params![&peer[..], MAX_PEER_REPORT_ROWS],
        )
        .context("enforce per-peer report cap")?;
        Ok(changed > 0)
    }

    /// The federation-export view (`report-sharing.md` § Federation exchange):
    /// this nest's LOCAL aggregates only, each count having passed the k-gate
    /// — never a peer-derived value (no laundering), never a below-k count.
    /// Highest-count first, bounded to the exchange entry cap so an export is
    /// always importable whole.
    pub async fn export_report_aggregates(&self) -> Result<Vec<([u8; 32], String, u32)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT content_hash, factor, COUNT(*) AS n FROM content_reports
                 GROUP BY content_hash, factor
                 ORDER BY n DESC
                 LIMIT ?1",
            )
            .context("prepare export view")?;
        let rows: Vec<([u8; 32], String, u32)> = stmt
            .query_map(rusqlite::params![MAX_REPORT_EXCHANGE_ENTRIES], |row| {
                let hash: Vec<u8> = row.get(0)?;
                Ok((hash, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
            })
            .context("query export view")?
            .filter_map(|r| r.ok())
            .filter_map(|(hash, factor, n)| {
                let content_hash: [u8; 32] = hash.as_slice().try_into().ok()?;
                // THE k-gate — a below-k aggregate never leaves the nest.
                let count = reports::exposed_report_count(n.max(0) as u32)?;
                Some((content_hash, factor, count))
            })
            .collect();
        Ok(rows)
    }
}

/// Hard cap on entries in a single `reports.exchange` request AND the export
/// view size — one peer cannot exhaust the DB (or make its export un-importable) in one call.
pub const MAX_REPORT_EXCHANGE_ENTRIES: usize = 1024;

/// Per-peer stored-row cap on `peer_content_reports` (oldest-updated evicted).
pub const MAX_PEER_REPORT_ROWS: i64 = 10_000;

/// The one capture entry point the explicit mark-as-spam paths call
/// (`report-sharing.md` § Report capture): iff the reporter opted in, a
/// spam-flag inserts a report row and a ham-correction/undo withdraws it;
/// either way the affected aggregate recomputes. The opt-in check is folded
/// into the insert itself ([`CacheDb::insert_content_report`]), so there is
/// no separate check/insert window to race. Failures here must never fail
/// the train itself — callers log-and-continue.
pub async fn capture_report(
    db: &CacheDb,
    reporter: &[u8; 32],
    key: &ReportKey,
    is_spam_flag: bool,
) -> Result<()> {
    if is_spam_flag {
        if db
            .insert_content_report(key, reporter, REPORT_PREF_COLUMN)
            .await?
        {
            db.recompute_report_score(key).await?;
        }
    } else {
        // A withdrawal applies regardless of the current pref — a reporter
        // who opted out after flagging can still retract the earlier flag.
        if db.delete_content_report(key, reporter).await? {
            db.recompute_report_score(key).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segments::records_db;
    use fauna_core::scoring::factor;

    fn mail_key(hash: [u8; 32]) -> ReportKey {
        ReportKey {
            content_hash: hash,
            factor: factor::REPORT_SPAM.to_string(),
            content_kind: "mail".to_string(),
        }
    }

    /// Insert a mirror row for one stored mail copy carrying `hash`.
    async fn seed_mail_item(db: &CacheDb, owner: &[u8; 32], rid: &[u8; 32], hash: &[u8; 32]) {
        let cid = fauna_cbor::Cid::from_digest_dag_cbor(*rid);
        let conn = db.conn.lock().await;
        records_db::insert_mail(
            &conn,
            owner,
            1,
            &cid,
            "2026-07",
            1_751_000_000_000,
            "example.com",
            "accept",
            false,
            1,
            Some(&hash[..]),
            0,                 // NORMAL — an ordinary single-record mail item.
            1_751_000_000_000, // stored_at — irrelevant here (no reaping in this test).
        )
        .expect("seed mirror row");
    }

    async fn report_row_score(db: &CacheDb, content_id: &[u8; 32]) -> Option<i64> {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT score FROM content_scores WHERE content_id = ?1 AND factor = ?2",
            rusqlite::params![&content_id[..], factor::REPORT_SPAM],
            |row| row.get(0),
        )
        .optional()
        .expect("read bus row")
    }

    async fn opt_in(db: &CacheDb, actor: &[u8; 32]) {
        db.set_share_reports(actor, true).await.expect("opt in");
    }

    #[tokio::test]
    async fn below_k_nothing_readable_at_k_row_appears() {
        let db = CacheDb::open_in_memory().unwrap();
        let hash = [0xAA; 32];
        let item = [0x01; 32];
        seed_mail_item(&db, &[0x10; 32], &item, &hash).await;
        let key = mail_key(hash);

        for (i, reporter) in [[0x21u8; 32], [0x22; 32]].iter().enumerate() {
            opt_in(&db, reporter).await;
            capture_report(&db, reporter, &key, true).await.unwrap();
            assert_eq!(
                report_row_score(&db, &item).await,
                None,
                "no bus row below k (reporter {})",
                i + 1
            );
            assert_eq!(
                db.gated_report_score(&hash, factor::REPORT_SPAM)
                    .await
                    .unwrap(),
                None
            );
        }

        let third = [0x23u8; 32];
        opt_in(&db, &third).await;
        capture_report(&db, &third, &key, true).await.unwrap();
        assert_eq!(report_row_score(&db, &item).await, Some(200), "k=3 → 200‰");
        assert_eq!(
            db.gated_report_score(&hash, factor::REPORT_SPAM)
                .await
                .unwrap(),
            Some(200)
        );
    }

    /// `capture_report`'s pref check and `insert_content_report`'s row write
    /// used to take the connection lock separately, so an opt-out landing
    /// between them could still let the insert land. Reproduced
    /// deterministically (no timing, e2e convention 14) by driving the
    /// INSERT LEG directly — exactly the call that lands after a caller's
    /// own pref read already returned "opted in" a moment before the
    /// opt-out commits. Red-verifies against the pre-fix `insert_content_report`
    /// (no gate of its own): only folding the gate into the insert's own
    /// statement, not merely an earlier check, can pass this.
    #[tokio::test]
    async fn insert_leg_alone_is_gated_the_race_dxxvi_names() {
        let db = CacheDb::open_in_memory().unwrap();
        let reporter = [0x71u8; 32];
        opt_in(&db, &reporter).await;
        db.set_share_reports(&reporter, false).await.unwrap();

        let hash = [0xEE; 32];
        let key = mail_key(hash);
        let inserted = db
            .insert_content_report(&key, &reporter, REPORT_PREF_COLUMN)
            .await
            .unwrap();
        assert!(
            !inserted,
            "an opt-out that lands before the insert must block it, not just an earlier check"
        );
        assert_eq!(
            db.gated_report_score(&hash, factor::REPORT_SPAM)
                .await
                .unwrap(),
            None,
            "the withdrawn contribution must never reach the exported aggregate"
        );
    }

    #[tokio::test]
    async fn pref_off_captures_nothing_and_double_flag_is_one_report() {
        let db = CacheDb::open_in_memory().unwrap();
        let hash = [0xBB; 32];
        let key = mail_key(hash);

        // Default-off pref: no row.
        let unopted = [0x31u8; 32];
        capture_report(&db, &unopted, &key, true).await.unwrap();
        // Three opted-in reporters, one flagging twice — still 3 distinct.
        for reporter in [[0x41u8; 32], [0x42; 32], [0x43; 32]] {
            opt_in(&db, &reporter).await;
            capture_report(&db, &reporter, &key, true).await.unwrap();
        }
        capture_report(&db, &[0x41u8; 32], &key, true)
            .await
            .unwrap();
        assert_eq!(
            db.gated_report_score(&hash, factor::REPORT_SPAM)
                .await
                .unwrap(),
            Some(200),
            "3 distinct reporters (dup + unopted ignored) → exactly k"
        );
    }

    #[tokio::test]
    async fn ham_correction_and_opt_out_withdraw() {
        let db = CacheDb::open_in_memory().unwrap();
        let hash = [0xCC; 32];
        let item = [0x02; 32];
        seed_mail_item(&db, &[0x11; 32], &item, &hash).await;
        let key = mail_key(hash);
        let reporters = [[0x51u8; 32], [0x52; 32], [0x53; 32], [0x54; 32]];
        for reporter in &reporters {
            opt_in(&db, reporter).await;
            capture_report(&db, reporter, &key, true).await.unwrap();
        }
        assert_eq!(report_row_score(&db, &item).await, Some(217), "4 → 217‰");

        // Ham-correction by one reporter → back to k=3.
        capture_report(&db, &reporters[0], &key, false)
            .await
            .unwrap();
        assert_eq!(report_row_score(&db, &item).await, Some(200));

        // Opt-out of one more → 2 < k → the bus row is WITHDRAWN.
        db.set_share_reports(&reporters[1], false).await.unwrap();
        assert_eq!(
            report_row_score(&db, &item).await,
            None,
            "below k withdraws"
        );
        assert_eq!(
            db.gated_report_score(&hash, factor::REPORT_SPAM)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn aggregate_attaches_to_every_local_copy() {
        let db = CacheDb::open_in_memory().unwrap();
        let hash = [0xDD; 32];
        let (item_a, item_b) = ([0x03; 32], [0x04; 32]);
        seed_mail_item(&db, &[0x12; 32], &item_a, &hash).await;
        seed_mail_item(&db, &[0x13; 32], &item_b, &hash).await;
        let key = mail_key(hash);
        for reporter in [[0x61u8; 32], [0x62; 32], [0x63; 32]] {
            opt_in(&db, &reporter).await;
            capture_report(&db, &reporter, &key, true).await.unwrap();
        }
        assert_eq!(report_row_score(&db, &item_a).await, Some(200));
        assert_eq!(report_row_score(&db, &item_b).await, Some(200));
    }

    #[tokio::test]
    async fn post_aggregate_keys_on_the_post_id() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [0xEE; 32];
        {
            // The attach is existence-gated: seed the post's content_meta row.
            let conn = db.conn.lock().await;
            crate::db::meta::upsert_meta(&conn, &post_id, 0.0, false, false, None, None, None)
                .unwrap();
        }
        let key = ReportKey {
            content_hash: post_id,
            factor: factor::REPORT_SPAM.to_string(),
            content_kind: "post".to_string(),
        };
        for reporter in [[0x71u8; 32], [0x72; 32], [0x73; 32]] {
            opt_in(&db, &reporter).await;
            capture_report(&db, &reporter, &key, true).await.unwrap();
        }
        assert_eq!(report_row_score(&db, &post_id).await, Some(200));
    }

    #[tokio::test]
    async fn peer_bucket_is_flat_never_scales_and_never_exports() {
        let db = CacheDb::open_in_memory().unwrap();
        let hash = [0xF0; 32];
        let item = [0x05; 32];
        seed_mail_item(&db, &[0x14; 32], &item, &hash).await;
        let key = mail_key(hash);
        let peer_a = [0xA1u8; 32];
        let peer_b = [0xA2u8; 32];

        // Peer-only, absurd claimed count: exactly the flat 100‰ — visible
        // corroboration, never consensus.
        assert!(
            db.upsert_peer_report(&peer_a, &hash, factor::REPORT_SPAM, 1_000_000, 1)
                .await
                .unwrap()
        );
        db.recompute_report_score(&key).await.unwrap();
        assert_eq!(report_row_score(&db, &item).await, Some(100));
        assert_eq!(
            db.gated_report_score(&hash, factor::REPORT_SPAM)
                .await
                .unwrap(),
            Some(100)
        );

        // A second peer changes nothing — ONE bucket across all peers.
        db.upsert_peer_report(&peer_b, &hash, factor::REPORT_SPAM, 999, 1)
            .await
            .unwrap();
        db.recompute_report_score(&key).await.unwrap();
        assert_eq!(report_row_score(&db, &item).await, Some(100));

        // k local reporters dominate: 200 + 100 = 300.
        for reporter in [[0x81u8; 32], [0x82; 32], [0x83; 32]] {
            opt_in(&db, &reporter).await;
            capture_report(&db, &reporter, &key, true).await.unwrap();
        }
        assert_eq!(report_row_score(&db, &item).await, Some(300));

        // Export carries the LOCAL gate-passed count only — never the peer
        // bucket, never a claimed count (no laundering).
        let export = db.export_report_aggregates().await.unwrap();
        assert_eq!(export, vec![(hash, factor::REPORT_SPAM.to_string(), 3)]);
    }

    #[tokio::test]
    async fn export_withholds_below_k_aggregates() {
        let db = CacheDb::open_in_memory().unwrap();
        let visible = [0xF1; 32];
        let hidden = [0xF2; 32];
        for reporter in [[0x91u8; 32], [0x92; 32], [0x93; 32]] {
            opt_in(&db, &reporter).await;
            capture_report(&db, &reporter, &mail_key(visible), true)
                .await
                .unwrap();
        }
        capture_report(&db, &[0x91u8; 32], &mail_key(hidden), true)
            .await
            .unwrap();
        let export = db.export_report_aggregates().await.unwrap();
        assert_eq!(export, vec![(visible, factor::REPORT_SPAM.to_string(), 3)]);
    }

    #[tokio::test]
    async fn peer_upsert_is_latest_epoch_wins() {
        let db = CacheDb::open_in_memory().unwrap();
        let hash = [0xF3; 32];
        let peer = [0xA3u8; 32];
        assert!(
            db.upsert_peer_report(&peer, &hash, factor::REPORT_SPAM, 5, 10)
                .await
                .unwrap()
        );
        // A stale-epoch replay is ignored.
        assert!(
            !db.upsert_peer_report(&peer, &hash, factor::REPORT_SPAM, 50, 9)
                .await
                .unwrap()
        );
        // A newer epoch updates.
        assert!(
            db.upsert_peer_report(&peer, &hash, factor::REPORT_SPAM, 7, 11)
                .await
                .unwrap()
        );
    }
}
