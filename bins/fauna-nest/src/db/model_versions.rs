//! The model-version registry + re-score obligation scan (capability-mediated
//! content processing, design spec § 2.5 "The versioned re-score obligation +
//! drain rendezvous").
//!
//! `model_versions` (table in `migrations::MIGRATIONS_MODEL_VERSIONS`) holds the
//! deployment-wide **current** version of every transparent scorer / index
//! model, keyed by `model_kind` — a scoring-bus factor name
//! (`content_scores.factor` / `fauna_core::scoring::ScoreEntry.factor`: spam /
//! clamav / rspamd / auth_*) or a future index axis key. It is the *durable
//! target* of the re-score obligation; the *durable progress* is the per-content
//! `scorer_version` watermark on `content_scores` (and the sibling
//! `IndexManifest.tokenizer_version` for the FTS axis).
//!
//! **The obligation is the GAP** (design § 2.5 step 3): a content row owes
//! re-processing iff its watermark is behind the registry. No per-user queue is
//! written at bump time — "who still owes" is derived here at drain time by
//! [`CacheDb::content_scores_behind`], which the content-at-rest drain worker
//! (Slice 5(A2)) calls per factor when a capability holder is present. The
//! registry is the durable target, the manifest/score watermark is the durable
//! progress, the gap is the obligation — so a model bump can't be lost even on a
//! box that drains lazily (design § 2.5).
//!
//! Content-free metadata only — this module never reads or holds a content key
//! (`key-material-hierarchy.md` rule #4/#7). The drain unseals content with the
//! *holder's* user-minted capability, never anything derived here.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_secs};

/// One unit of re-score work: a `content_scores` row whose `scorer_version` is
/// behind the model-version registry for its factor (the obligation gap made
/// concrete). The drain fetches the sealed content by `content_id`, unseals it
/// with the owner's capability, re-runs the scorer, writes the new `ScoreEntry`
/// (bumping `scorer_version` to the current registry version), and drops the
/// plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleScore {
    /// The scored item's content id (32 bytes); the drain fetches the sealed
    /// record by it.
    pub content_id: Vec<u8>,
    /// The item's kind (`mail` / …) — selects the sealed store the drain
    /// unseals from.
    pub content_kind: String,
    /// The owner whose capability the drain needs to unseal. `None` for rows
    /// that carry no actor (e.g. own-submission Sent copies) — the drain skips
    /// those (nothing to re-score against a per-user key).
    pub actor_id: Option<Vec<u8>>,
    /// The version the row was last scored at (strictly `< current_version`).
    pub scorer_version: u32,
}

impl CacheDb {
    /// The registry's current version for one model kind, or `None` if the kind
    /// has no row (never seeded/bumped — no obligation for it).
    pub async fn get_model_version(&self, model_kind: &str) -> Result<Option<u32>> {
        let model_kind = model_kind.to_string();
        let conn = self.conn.lock().await;
        let v: Option<i64> = conn
            .query_row(
                "SELECT version FROM model_versions WHERE model_kind = ?1",
                rusqlite::params![model_kind],
                |r| r.get(0),
            )
            .optional()
            .context("get model version")?;
        Ok(v.map(|v| v.clamp(0, u32::MAX as i64) as u32))
    }

    /// Every registry row as `(model_kind, version)`, `model_kind`-ordered — the
    /// drain iterates these to learn each factor's current version before
    /// scanning for stale content.
    pub async fn list_model_versions(&self) -> Result<Vec<(String, u32)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT model_kind, version FROM model_versions ORDER BY model_kind")
            .context("prepare list model versions")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?.clamp(0, u32::MAX as i64) as u32,
                ))
            })
            .context("query model versions")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect model versions")?;
        Ok(rows)
    }

    /// Bump a model kind's current version (a model update — design § 2.5 step
    /// 1: "a model update bumps `version`"). **Monotonic**: never lowers an
    /// existing version, so a stale or racing caller can't roll the obligation
    /// backward. Returns the version now stored (the max of the old and new).
    ///
    /// The built-in scorers are advanced by the boot reconcile
    /// (`migrations::seed_builtin_model_versions`); this is the runtime path a
    /// future community/admin-model load calls.
    pub async fn upsert_model_version(&self, model_kind: &str, version: u32) -> Result<u32> {
        let model_kind = model_kind.to_string();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO model_versions (model_kind, version, updated_at)
                 VALUES (?1, ?2, ?3)
             ON CONFLICT(model_kind) DO UPDATE SET
                 version    = MAX(model_versions.version, excluded.version),
                 updated_at = CASE WHEN excluded.version > model_versions.version
                                   THEN excluded.updated_at
                                   ELSE model_versions.updated_at END",
            rusqlite::params![model_kind, version as i64, now],
        )
        .context("upsert model version")?;
        let stored: i64 = conn
            .query_row(
                "SELECT version FROM model_versions WHERE model_kind = ?1",
                rusqlite::params![model_kind],
                |r| r.get(0),
            )
            .context("read back upserted model version")?;
        Ok(stored.clamp(0, u32::MAX as i64) as u32)
    }

    /// The re-score obligation scan (design § 2.5 step 3): every `content_scores`
    /// row for `factor` whose `scorer_version` is behind `current_version`,
    /// lowest-version-first, capped at `limit`. Uses `idx_content_scores_factor_version`
    /// (on `(factor, scorer_version)`) — the index the scoring bus built for
    /// exactly this scan.
    ///
    /// The drain worker calls it per factor with that factor's registry version
    /// (from [`list_model_versions`] / [`get_model_version`]); each returned row
    /// is a unit of work. `limit` batches the drain, which loops until the scan
    /// returns empty — a `limit`-full result means "more remain", never a silent
    /// truncation of the obligation.
    pub async fn content_scores_behind(
        &self,
        factor: &str,
        current_version: u32,
        limit: usize,
    ) -> Result<Vec<StaleScore>> {
        let factor = factor.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT content_id, content_kind, actor_id, scorer_version
                   FROM content_scores
                  WHERE factor = ?1 AND scorer_version < ?2
                  ORDER BY scorer_version, content_id
                  LIMIT ?3",
            )
            .context("prepare content_scores_behind")?;
        let rows = stmt
            .query_map(
                rusqlite::params![factor, current_version as i64, limit as i64],
                |r| {
                    Ok(StaleScore {
                        content_id: r.get(0)?,
                        content_kind: r.get(1)?,
                        actor_id: r.get::<_, Option<Vec<u8>>>(2)?,
                        scorer_version: r.get::<_, i64>(3)?.clamp(0, u32::MAX as i64) as u32,
                    })
                },
            )
            .context("query content_scores_behind")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect content_scores_behind rows")?;
        Ok(rows)
    }

    /// The owner-scoped obligation scan — [`content_scores_behind`] narrowed to
    /// a single `owner_actor_id`. This is the **worklist confidentiality
    /// boundary**: the `fauna.capabilities.rescore_worklist` handler serves a
    /// holder only work for owners it holds a `content.read{kind}` grant for, so
    /// the nest must never hand a holder the `content_id`s of an owner it can't
    /// unseal. The extra `actor_id = ?` predicate rides `idx_content_scores_factor_version`
    /// (`(factor, scorer_version)`) for the range and filters the owner as a
    /// residual — the same lowest-version-first, `limit`-batched, loop-until-empty
    /// contract as the base scan.
    pub async fn content_scores_behind_for_owner(
        &self,
        factor: &str,
        owner_actor_id: &[u8],
        current_version: u32,
        limit: usize,
    ) -> Result<Vec<StaleScore>> {
        let factor = factor.to_string();
        let owner = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT content_id, content_kind, actor_id, scorer_version
                   FROM content_scores
                  WHERE factor = ?1 AND scorer_version < ?2 AND actor_id = ?3
                  ORDER BY scorer_version, content_id
                  LIMIT ?4",
            )
            .context("prepare content_scores_behind_for_owner")?;
        let rows = stmt
            .query_map(
                rusqlite::params![factor, current_version as i64, owner, limit as i64],
                |r| {
                    Ok(StaleScore {
                        content_id: r.get(0)?,
                        content_kind: r.get(1)?,
                        actor_id: r.get::<_, Option<Vec<u8>>>(2)?,
                        scorer_version: r.get::<_, i64>(3)?.clamp(0, u32::MAX as i64) as u32,
                    })
                },
            )
            .context("query content_scores_behind_for_owner")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect content_scores_behind_for_owner rows")?;
        Ok(rows)
    }

    /// Seed the re-score backlog for a **freshly-registered factor** — the
    /// new-factor gap (design revision 2026-07-07, point 3). The obligation scans
    /// ([`content_scores_behind`]/`_for_owner`) only surface rows that *already*
    /// carry the factor (`WHERE factor = ?1 AND scorer_version < ?2`), so a
    /// just-subscribed `labeler:<id>` — which has no rows — would owe **zero**
    /// drain work even though its version is registered. This inserts a
    /// placeholder `content_scores` row (`score 0`, `tier` COMMUNITY,
    /// `scorer_version 0`) per one of `owner`'s items of `content_kind`, so each
    /// item reads as "behind version 0 < the registered version" and a capability
    /// holder drains it. Returns the number of items seeded.
    ///
    /// Idempotent: `INSERT OR IGNORE` on the `(content_id, factor)` primary key,
    /// so a re-subscribe never clobbers a real drained score (or double-counts a
    /// seed). Owner-scoped: writes only `owner`'s own items — the same
    /// confidentiality boundary the owner-scoped worklist enforces
    /// (`key-material-hierarchy.md` rule #4/#7: content-free metadata, no key).
    ///
    /// **Item universe by kind** (v1): `mail` = every `message_scan_results` row
    /// delivered to `owner` (its `message_id` is the 32-byte content id the drain
    /// unseals; `migrations.rs` § message_scan_results). Every other kind seeds
    /// **nothing** and returns 0 — `post`-content-at-rest does not exist yet (post
    /// drains arrive with their holder, `rescore_drain.go`), and a future
    /// `List`-kind labeler materializes rows by a different path. This is a
    /// general model-version-bus concern (any factor added *after* its content was
    /// ingested hits the same gap), hence a reusable helper rather than a
    /// labeler-only branch.
    pub async fn seed_factor_backlog(
        &self,
        factor: &str,
        content_kind: &str,
        owner: &[u8; 32],
    ) -> Result<usize> {
        // Only `mail` has a content-at-rest item universe today; other kinds have
        // nothing to seed (documented above) — a cheap early return, not a gap.
        if content_kind != "mail" {
            return Ok(0);
        }
        let owner_vec = owner.to_vec();
        let scored_at = now_epoch_secs();
        let conn = self.conn.lock().await;

        // The owner's mail item universe (drop the SELECT statement before the
        // insert loop so it no longer borrows `conn`).
        let ids: Vec<Vec<u8>> = {
            let mut stmt = conn
                .prepare(
                    "SELECT message_id FROM message_scan_results
                       WHERE delivered_to_actor = ?1",
                )
                .context("prepare seed_factor_backlog mail scan")?;
            stmt.query_map(rusqlite::params![&owner_vec[..]], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .context("query message_scan_results for seed")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect seed message ids")?
        };

        let mut seeded = 0usize;
        for cid in &ids {
            seeded += conn
                .execute(
                    "INSERT OR IGNORE INTO content_scores (
                         content_id, content_kind, factor, score, tier,
                         scorer_version, scored_at, actor_id)
                     VALUES (?1, ?2, ?3, 0, ?4, 0, ?5, ?6)",
                    rusqlite::params![
                        cid,
                        content_kind,
                        factor,
                        fauna_core::scoring::TIER_COMMUNITY as i64,
                        scored_at,
                        &owner_vec[..],
                    ],
                )
                .context("seed content_scores backlog row")?;
        }
        Ok(seeded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::scoring::{ScoreEntry, TIER_ADMIN, factor};

    fn clamav_entry(version: u32) -> ScoreEntry {
        ScoreEntry {
            factor: factor::CLAMAV.to_string(),
            score: 0,
            tier: TIER_ADMIN,
            scorer_version: version,
        }
    }

    #[tokio::test]
    async fn get_absent_model_version_is_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_model_version("no-such-model").await.unwrap(), None);
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.upsert_model_version("phishing", 3).await.unwrap(), 3);
        assert_eq!(db.get_model_version("phishing").await.unwrap(), Some(3));
    }

    #[tokio::test]
    async fn upsert_is_monotonic() {
        // A lower version never rolls the registry backward (the obligation can't
        // regress); a higher version advances it.
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.upsert_model_version("nsfw", 5).await.unwrap(), 5);
        assert_eq!(db.upsert_model_version("nsfw", 3).await.unwrap(), 5);
        assert_eq!(db.get_model_version("nsfw").await.unwrap(), Some(5));
        assert_eq!(db.upsert_model_version("nsfw", 8).await.unwrap(), 8);
        assert_eq!(db.get_model_version("nsfw").await.unwrap(), Some(8));
    }

    #[tokio::test]
    async fn list_model_versions_is_seeded_and_ordered() {
        // Fresh DB is already seeded from the built-in constants (boot reconcile);
        // an added kind lists in model_kind order.
        let db = CacheDb::open_in_memory().unwrap();
        db.upsert_model_version("zzz-late", 2).await.unwrap();
        let list = db.list_model_versions().await.unwrap();
        // seeded built-ins present
        assert!(list.iter().any(|(k, _)| k == factor::CLAMAV));
        assert!(list.iter().any(|(k, _)| k == factor::SPAM));
        // the added kind is present and the list is sorted ascending by kind
        assert!(list.iter().any(|(k, v)| k == "zzz-late" && *v == 2));
        let mut sorted = list.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            list, sorted,
            "list_model_versions must be model_kind-ordered"
        );
    }

    #[tokio::test]
    async fn content_scores_behind_returns_only_stale_rows() {
        // Two clamav-scored items: one at v1 (stale vs current v2), one at v2
        // (current). The obligation scan returns only the stale one.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let cid_old = [1u8; 32];
        let cid_current = [2u8; 32];
        db.insert_content_scores(
            &cid_old,
            "mail",
            Some(&actor),
            1_700_000_000,
            &[clamav_entry(1)],
        )
        .await
        .unwrap();
        db.insert_content_scores(
            &cid_current,
            "mail",
            Some(&actor),
            1_700_000_100,
            &[clamav_entry(2)],
        )
        .await
        .unwrap();

        let behind = db
            .content_scores_behind(factor::CLAMAV, 2, 100)
            .await
            .unwrap();
        assert_eq!(behind.len(), 1);
        assert_eq!(behind[0].content_id, cid_old.to_vec());
        assert_eq!(behind[0].content_kind, "mail");
        assert_eq!(behind[0].actor_id.as_deref(), Some(&actor[..]));
        assert_eq!(behind[0].scorer_version, 1);
    }

    #[tokio::test]
    async fn content_scores_behind_for_owner_scopes_by_owner() {
        // The worklist confidentiality boundary: two owners each have a stale
        // clamav row; the owner-scoped scan returns ONLY the queried owner's row,
        // never leaking the other owner's content_id to a holder without a grant.
        let db = CacheDb::open_in_memory().unwrap();
        let owner_a = [0xAAu8; 32];
        let owner_b = [0xBBu8; 32];
        let cid_a = [1u8; 32];
        let cid_b = [2u8; 32];
        db.insert_content_scores(
            &cid_a,
            "mail",
            Some(&owner_a),
            1_700_000_000,
            &[clamav_entry(1)],
        )
        .await
        .unwrap();
        db.insert_content_scores(
            &cid_b,
            "mail",
            Some(&owner_b),
            1_700_000_000,
            &[clamav_entry(1)],
        )
        .await
        .unwrap();

        let for_a = db
            .content_scores_behind_for_owner(factor::CLAMAV, &owner_a, 2, 100)
            .await
            .unwrap();
        assert_eq!(for_a.len(), 1, "only owner A's stale row");
        assert_eq!(for_a[0].content_id, cid_a.to_vec());
        assert_eq!(for_a[0].actor_id.as_deref(), Some(&owner_a[..]));

        // A third owner with no stale content sees an empty worklist.
        let for_c = db
            .content_scores_behind_for_owner(factor::CLAMAV, &[0xCCu8; 32], 2, 100)
            .await
            .unwrap();
        assert!(for_c.is_empty(), "an owner with no stale rows owes nothing");
    }

    #[tokio::test]
    async fn content_scores_behind_is_factor_scoped() {
        // A stale spam row must not surface when scanning for a clamav gap — the
        // obligation is per-factor.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        db.insert_content_scores(
            &[1u8; 32],
            "mail",
            Some(&actor),
            1_700_000_000,
            &[ScoreEntry {
                factor: factor::SPAM.to_string(),
                score: 100,
                tier: fauna_core::scoring::TIER_USER,
                scorer_version: 1,
            }],
        )
        .await
        .unwrap();
        let behind = db
            .content_scores_behind(factor::CLAMAV, 9, 100)
            .await
            .unwrap();
        assert!(
            behind.is_empty(),
            "a spam gap must not surface under clamav"
        );
    }

    #[tokio::test]
    async fn content_scores_behind_empty_when_current() {
        // Everything already at the current version → no obligation.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        db.insert_content_scores(
            &[1u8; 32],
            "mail",
            Some(&actor),
            1_700_000_000,
            &[clamav_entry(2)],
        )
        .await
        .unwrap();
        let behind = db
            .content_scores_behind(factor::CLAMAV, 2, 100)
            .await
            .unwrap();
        assert!(behind.is_empty());
    }

    #[tokio::test]
    async fn content_scores_behind_respects_limit_and_lowest_version_first() {
        // Several stale rows across two old versions; limit caps the batch and the
        // lowest scorer_version drains first (the most-behind content).
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        // three at v1, one at v2, current is v3
        for i in 0..3u8 {
            let mut cid = [0u8; 32];
            cid[0] = i;
            db.insert_content_scores(
                &cid,
                "mail",
                Some(&actor),
                1_700_000_000,
                &[clamav_entry(1)],
            )
            .await
            .unwrap();
        }
        db.insert_content_scores(
            &[9u8; 32],
            "mail",
            Some(&actor),
            1_700_000_000,
            &[clamav_entry(2)],
        )
        .await
        .unwrap();

        // limit 2 → two rows, both the most-behind v1 rows
        let batch = db
            .content_scores_behind(factor::CLAMAV, 3, 2)
            .await
            .unwrap();
        assert_eq!(batch.len(), 2);
        assert!(batch.iter().all(|s| s.scorer_version == 1));

        // unbounded-enough limit → all four stale rows, v1s before the v2
        let all = db
            .content_scores_behind(factor::CLAMAV, 3, 100)
            .await
            .unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].scorer_version, 1);
        assert_eq!(all[3].scorer_version, 2);
    }

    /// A mail delivered row for `owner`, minimal fields (helper for the seed test).
    async fn deliver_mail(db: &CacheDb, message_id: [u8; 32], owner: [u8; 32]) {
        use crate::db::bridge_routing::ScanResultRow;
        db.insert_scan_result(&ScanResultRow {
            message_id,
            received_at: 1_700_000_000,
            scanned_at: 1_700_000_000,
            clamav_verdict: "clean".into(),
            clamav_signature: None,
            rspamd_score_raw: None,
            rspamd_score_scaled: None,
            rspamd_flagged_rules: None,
            rspamd_score_breakdown: None,
            action_taken: "delivered".into(),
            delivered_to_actor: Some(owner),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn seed_factor_backlog_seeds_owner_mail_items_only() {
        // Two owners each with one delivered mail item; seeding owner A's backlog
        // for a labeler factor materializes ONLY A's item as behind-version-0.
        let db = CacheDb::open_in_memory().unwrap();
        let owner_a = [0xAAu8; 32];
        let owner_b = [0xBBu8; 32];
        let cid_a = [1u8; 32];
        let cid_b = [2u8; 32];
        deliver_mail(&db, cid_a, owner_a).await;
        deliver_mail(&db, cid_b, owner_b).await;

        let factor = "labeler:deadbeef";
        let seeded = db
            .seed_factor_backlog(factor, "mail", &owner_a)
            .await
            .unwrap();
        assert_eq!(seeded, 1, "only owner A's one mail item is seeded");

        // A's item is now behind version 0 for the factor …
        let behind_a = db
            .content_scores_behind_for_owner(factor, &owner_a, 1, 100)
            .await
            .unwrap();
        assert_eq!(behind_a.len(), 1);
        assert_eq!(behind_a[0].content_id, cid_a.to_vec());
        assert_eq!(behind_a[0].content_kind, "mail");
        assert_eq!(behind_a[0].scorer_version, 0);
        // … while owner B (never seeded) still owes nothing for this factor.
        let behind_b = db
            .content_scores_behind_for_owner(factor, &owner_b, 1, 100)
            .await
            .unwrap();
        assert!(
            behind_b.is_empty(),
            "B's item was not seeded — no obligation"
        );
    }

    #[tokio::test]
    async fn seed_factor_backlog_is_idempotent_and_preserves_real_scores() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xAAu8; 32];
        let cid = [1u8; 32];
        deliver_mail(&db, cid, owner).await;
        let factor = "labeler:cafe";

        assert_eq!(
            db.seed_factor_backlog(factor, "mail", &owner)
                .await
                .unwrap(),
            1
        );
        // A re-seed (e.g. re-subscribe) inserts nothing new — INSERT OR IGNORE on
        // the (content_id, factor) PK.
        assert_eq!(
            db.seed_factor_backlog(factor, "mail", &owner)
                .await
                .unwrap(),
            0,
            "re-seeding an already-seeded item is a no-op"
        );

        // Simulate the drain having scored the item at version 1, then re-seed:
        // the real score must NOT be clobbered back to the 0 placeholder.
        db.insert_content_scores(
            &cid,
            "mail",
            Some(&owner),
            1_700_000_500,
            &[ScoreEntry {
                factor: factor.to_string(),
                score: 800,
                tier: fauna_core::scoring::TIER_COMMUNITY,
                scorer_version: 1,
            }],
        )
        .await
        .unwrap();
        assert_eq!(
            db.seed_factor_backlog(factor, "mail", &owner)
                .await
                .unwrap(),
            0
        );
        let scored = db.get_content_scores(&cid).await.unwrap();
        let entry = scored.iter().find(|e| e.factor == factor).unwrap();
        assert_eq!(
            entry.score, 800,
            "the real drained score survives a re-seed"
        );
        assert_eq!(entry.scorer_version, 1);
    }

    #[tokio::test]
    async fn seed_factor_backlog_non_mail_kind_seeds_nothing() {
        // `post` (and any kind without a content-at-rest universe) seeds nothing.
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xAAu8; 32];
        deliver_mail(&db, [1u8; 32], owner).await; // a mail row exists …
        // … but a post-kind seed ignores it (post-content-at-rest doesn't exist).
        assert_eq!(
            db.seed_factor_backlog("labeler:beef", "post", &owner)
                .await
                .unwrap(),
            0
        );
    }
}
