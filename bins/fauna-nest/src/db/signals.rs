//! Layer-B engagement-cue contribution — the `signal:*` k-anon aggregates
//! (`docs/goal/behavior/engagement-cues.md` § Layer B nest legs). The opt-in
//! Layer-B sibling of distributed report sharing (`db/reports.rs`): a coarse
//! per-item cue verdict ("I watched this to the end" / "I skipped this") from
//! an opted-in client rides the SAME `content_reports` table, k-gate, count
//! curve, and `fauna.federation.reports.{exchange,export}` pair as a spam flag
//! — no new tables, no new federation kinds. This module holds only the
//! signal-specific capture, the independent `share_signals` opt-in, the
//! public-post write gate, and the verdict-flip semantics; the shared (now
//! factor-parametric) writer / export / opt-out-sweep primitives live in
//! `db/reports.rs`.

use anyhow::{Context, Result};
use fauna_core::scoring::factor;
use rusqlite::OptionalExtension;

use super::CacheDb;
use super::reports::{ReportKey, SIGNAL_FACTOR_PREFIX, SIGNAL_PREF_COLUMN};

/// A single derived cue verdict a client contributes for one public post
/// (`fauna.moderation.signal_contribute.signal`). A user has ONE verdict per
/// item (last-wins), so `watch-complete` and `skip` are mutually exclusive per
/// `(contributor, content)`: a flip withdraws the old factor's row and inserts
/// the new; `Withdraw` retracts whichever is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalVerdict {
    WatchComplete,
    Skip,
    Withdraw,
}

impl SignalVerdict {
    /// Parse the wire string; `None` for an unrecognised verdict (the handler
    /// rejects it as `invalid_params`).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "watch-complete" => Some(Self::WatchComplete),
            "skip" => Some(Self::Skip),
            "withdraw" => Some(Self::Withdraw),
            _ => None,
        }
    }

    /// The `content_reports.factor` this verdict asserts, or `None` for
    /// `Withdraw` (which asserts nothing — it retracts both factors).
    fn asserts(self) -> Option<&'static str> {
        match self {
            Self::WatchComplete => Some(factor::SIGNAL_WATCH_COMPLETE),
            Self::Skip => Some(factor::SIGNAL_SKIP),
            Self::Withdraw => None,
        }
    }
}

/// The two mutually-exclusive signal factors — a contribute touches BOTH (it
/// asserts one and withdraws the other), and a `Withdraw` retracts both.
const SIGNAL_FACTORS: [&str; 2] = [factor::SIGNAL_WATCH_COMPLETE, factor::SIGNAL_SKIP];

impl CacheDb {
    /// Whether the actor opted into engagement-signal sharing
    /// (`spam_preferences.share_signals`, default off) — the independent
    /// Layer-B sibling of `share_reports`.
    pub async fn share_signals_enabled(&self, actor_id: &[u8; 32]) -> Result<bool> {
        self.share_pref_enabled(
            "SELECT share_signals FROM spam_preferences WHERE actor_id = ?1",
            actor_id,
            "read share_signals",
        )
        .await
    }

    /// Set the engagement-signal-sharing opt-in. Opting OUT deletes every
    /// `signal:*` row this contributor produced (a withdrawn judgment leaves no
    /// residue) and recomputes each affected aggregate — which may fall below k
    /// and withdraw its bus rows. Scoped to the `signal:` factor family, so it
    /// never touches the contributor's INDEPENDENT `report:spam` rows.
    pub async fn set_share_signals(&self, actor_id: &[u8; 32], enabled: bool) -> Result<()> {
        self.set_share_pref(
            "INSERT INTO spam_preferences (actor_id, share_signals, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET
                share_signals = excluded.share_signals,
                updated_at = excluded.updated_at",
            actor_id,
            enabled,
            SIGNAL_FACTOR_PREFIX,
            "set share_signals",
        )
        .await
    }

    /// Whether a post is a **public** post this nest holds (`content_meta`
    /// exists AND `gated_tier IS NULL`) — the write-path gate for signal
    /// contribution (`engagement-cues.md` § Layer B — "public posts only; a
    /// verdict about restricted content is rejected at the handler, its
    /// existence would leak readership"). A gated OR unseen post is not
    /// eligible (both are indistinguishable to the caller, so the rejection
    /// leaks no existence signal). Mirrors the trending scorer's public probe
    /// (`db/trends.rs`).
    pub async fn content_is_public(&self, content_id: &[u8; 32]) -> Result<bool> {
        let id = *content_id;
        let conn = self.conn.lock().await;
        let public: Option<bool> = conn
            .query_row(
                "SELECT gated_tier IS NULL FROM content_meta WHERE content_id = ?1",
                rusqlite::params![&id[..]],
                |row| row.get::<_, i64>(0).map(|v| v != 0),
            )
            .optional()
            .context("probe post audience for signal contribution")?;
        Ok(public == Some(true))
    }

    /// Capture one engagement-cue verdict for a public post
    /// (`engagement-cues.md` § Layer B — the write path). Verdicts are
    /// per-`(contributor, content)`, last-wins:
    ///
    /// - `WatchComplete` / `Skip` (iff opted in): insert the asserted factor's
    ///   row AND withdraw the OTHER factor's row for this contributor (the
    ///   flip), recomputing each aggregate that changed.
    /// - `Withdraw` (opt-in-independent, exactly like a ham-correction): delete
    ///   both factors' rows for this contributor, recomputing each that changed.
    ///
    /// Reuses the shared report primitives verbatim (`insert_content_report`,
    /// `delete_content_report`, `recompute_report_score`) — the aggregate math,
    /// the k-gate, and the bus writer are byte-identical to `report:spam`. The
    /// caller has already confirmed the post is public (`content_is_public`);
    /// this method does not re-check (a `Withdraw` must succeed even if the post
    /// later went gated — user-controls-their-data).
    ///
    /// A positive verdict's insert is gated on `share_signals` INSIDE
    /// `insert_content_report`'s own statement, not by a separate check here
    /// — an opt-out committing after this call starts can no longer let the
    /// asserted factor's row land. A withdrawal always applies (a
    /// contributor who opted out can still retract an earlier verdict — the
    /// report-sharing ham-correction rule), and `Withdraw` asserts nothing so
    /// the loop below only ever deletes for it.
    pub async fn capture_signal(
        &self,
        contributor: &[u8; 32],
        content_hash: &[u8; 32],
        verdict: SignalVerdict,
    ) -> Result<()> {
        let asserted = verdict.asserts();
        for &fac in &SIGNAL_FACTORS {
            let key = ReportKey {
                content_hash: *content_hash,
                factor: fac.to_string(),
                content_kind: "post".to_string(),
            };
            let changed = if Some(fac) == asserted {
                self.insert_content_report(&key, contributor, SIGNAL_PREF_COLUMN)
                    .await?
            } else {
                self.delete_content_report(&key, contributor).await?
            };
            if changed {
                self.recompute_report_score(&key).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::reports::{ReportKey, capture_report};

    /// The bus-row score (per-mille) for `content_id` under `factor`, or `None`.
    async fn bus_score(db: &CacheDb, content_id: &[u8; 32], fac: &str) -> Option<i64> {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT score FROM content_scores WHERE content_id = ?1 AND factor = ?2",
            rusqlite::params![&content_id[..], fac],
            |row| row.get(0),
        )
        .optional()
        .expect("read bus row")
    }

    /// Seed a PUBLIC post (NULL gated_tier) so the signal bus row can attach.
    async fn seed_public_post(db: &CacheDb, id: &[u8; 32]) {
        let conn = db.conn.lock().await;
        crate::db::meta::upsert_meta(&conn, id, 0.0, false, false, None, None, None).unwrap();
    }

    /// Seed a GATED post (non-NULL gated_tier).
    async fn seed_gated_post(db: &CacheDb, id: &[u8; 32]) {
        let conn = db.conn.lock().await;
        crate::db::meta::upsert_meta(&conn, id, 0.0, false, false, Some("followers"), None, None)
            .unwrap();
    }

    async fn opt_in(db: &CacheDb, actor: &[u8; 32]) {
        db.set_share_signals(actor, true).await.expect("opt in");
    }

    #[tokio::test]
    async fn watch_complete_k_gate_ramp() {
        let db = CacheDb::open_in_memory().unwrap();
        let post = [0xAA; 32];
        seed_public_post(&db, &post).await;

        // Below k: nothing readable on the bus.
        for (i, c) in [[0x21u8; 32], [0x22; 32]].iter().enumerate() {
            opt_in(&db, c).await;
            db.capture_signal(c, &post, SignalVerdict::WatchComplete)
                .await
                .unwrap();
            assert_eq!(
                bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
                None,
                "no bus row below k (contributor {})",
                i + 1
            );
        }
        // The third distinct opted-in contributor crosses k=3 → 200‰.
        let third = [0x23u8; 32];
        opt_in(&db, &third).await;
        db.capture_signal(&third, &post, SignalVerdict::WatchComplete)
            .await
            .unwrap();
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            Some(200),
            "k=3 signal:watch-complete → 200‰ (same curve as report:spam)"
        );
    }

    /// The signal-family twin of `reports::insert_leg_alone_is_gated_the_race_dxxvi_names`
    /// — the identical check/insert race against `share_signals`, reproduced
    /// the same way: drive `insert_content_report` directly after an opt-out
    /// commits, as it runs after `capture_signal`'s own pref read already
    /// returned "opted in" a moment before.
    #[tokio::test]
    async fn insert_leg_alone_is_gated_the_race_dxxvi_names() {
        let db = CacheDb::open_in_memory().unwrap();
        let post = [0xFF; 32];
        seed_public_post(&db, &post).await;
        let contributor = [0x81u8; 32];
        opt_in(&db, &contributor).await;
        db.set_share_signals(&contributor, false).await.unwrap();

        let key = ReportKey {
            content_hash: post,
            factor: factor::SIGNAL_WATCH_COMPLETE.to_string(),
            content_kind: "post".to_string(),
        };
        let inserted = db
            .insert_content_report(&key, &contributor, SIGNAL_PREF_COLUMN)
            .await
            .unwrap();
        assert!(
            !inserted,
            "an opt-out that lands before the insert must block it, not just an earlier check"
        );
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            None,
            "the withdrawn contribution must never reach the exported aggregate"
        );
    }

    #[tokio::test]
    async fn pref_off_captures_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let post = [0xBB; 32];
        seed_public_post(&db, &post).await;
        // Default-off: three NOT-opted-in contributors capture nothing.
        for c in [[0x31u8; 32], [0x32; 32], [0x33; 32]] {
            db.capture_signal(&c, &post, SignalVerdict::WatchComplete)
                .await
                .unwrap();
        }
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            None,
            "opt-in default off — a contribution without opt-in captures nothing"
        );
    }

    #[tokio::test]
    async fn verdict_flip_migrates_the_factor() {
        let db = CacheDb::open_in_memory().unwrap();
        let post = [0xCC; 32];
        seed_public_post(&db, &post).await;
        let contributors = [[0x41u8; 32], [0x42; 32], [0x43; 32]];

        // Three watch-complete → wc 200‰, no skip.
        for c in &contributors {
            opt_in(&db, c).await;
            db.capture_signal(c, &post, SignalVerdict::WatchComplete)
                .await
                .unwrap();
        }
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            Some(200)
        );
        assert_eq!(bus_score(&db, &post, factor::SIGNAL_SKIP).await, None);

        // All three flip to skip → the whole aggregate migrates: wc withdrawn,
        // skip now at k=3 (each flip withdraws the old row AND inserts the new).
        for c in &contributors {
            db.capture_signal(c, &post, SignalVerdict::Skip)
                .await
                .unwrap();
        }
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            None,
            "flip withdraws the old factor's rows → wc below k → withdrawn"
        );
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_SKIP).await,
            Some(200),
            "flip inserts the new factor's rows → skip reaches k=3"
        );
    }

    #[tokio::test]
    async fn withdraw_retracts_and_is_opt_in_independent() {
        let db = CacheDb::open_in_memory().unwrap();
        let post = [0xDD; 32];
        seed_public_post(&db, &post).await;
        let contributors = [[0x51u8; 32], [0x52; 32], [0x53; 32]];
        for c in &contributors {
            opt_in(&db, c).await;
            db.capture_signal(c, &post, SignalVerdict::WatchComplete)
                .await
                .unwrap();
        }
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            Some(200)
        );

        // One contributor withdraws — even after opting out (a retraction must
        // always succeed). wc falls to 2 < k → withdrawn.
        db.set_share_signals(&contributors[0], false).await.unwrap();
        db.capture_signal(&contributors[0], &post, SignalVerdict::Withdraw)
            .await
            .unwrap();
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            None,
            "withdraw applies regardless of opt-in state → below k → withdrawn"
        );
    }

    #[tokio::test]
    async fn opt_out_is_factor_scoped_reports_and_signals_are_independent() {
        // THE independence guarantee: opting out of ONE family must never
        // delete the other's rows for the same actor. Both prefs on, both a
        // report:spam flag AND a signal on the same post; then opt out of each
        // family in turn and confirm only that family's aggregate withdraws.
        //
        // A fourth reporter (report-only, no signal) keeps the report
        // aggregate at 4 so it survives the first report opt-out (4→3, still
        // ≥ k) and stays OBSERVABLE across the later signal opt-out — a
        // widened signals-site sweep that also destroyed report rows would
        // otherwise be indistinguishable from correct behavior, because with
        // only three reporters the aggregate is already `None` by the time
        // signals opt out ().
        let db = CacheDb::open_in_memory().unwrap();
        let post = [0xEE; 32];
        seed_public_post(&db, &post).await;
        let actors = [[0x61u8; 32], [0x62; 32], [0x63; 32]];
        let actor4 = [0x64u8; 32];

        for a in &actors {
            db.set_share_reports(a, true).await.unwrap();
            db.set_share_signals(a, true).await.unwrap();
            // report:spam via the shared capture entry point (a post's id IS
            // its report-hash).
            let report_key = ReportKey {
                content_hash: post,
                factor: factor::REPORT_SPAM.to_string(),
                content_kind: "post".to_string(),
            };
            capture_report(&db, a, &report_key, true).await.unwrap();
            db.capture_signal(a, &post, SignalVerdict::WatchComplete)
                .await
                .unwrap();
        }
        db.set_share_reports(&actor4, true).await.unwrap();
        let report_key4 = ReportKey {
            content_hash: post,
            factor: factor::REPORT_SPAM.to_string(),
            content_kind: "post".to_string(),
        };
        capture_report(&db, &actor4, &report_key4, true)
            .await
            .unwrap();

        // Reports at n=4 (200 + (4-3)*17), signals at k=3 (200).
        assert_eq!(bus_score(&db, &post, factor::REPORT_SPAM).await, Some(217));
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            Some(200)
        );

        // Opt ONE actor out of report sharing → reporters drop 4→3, still ≥
        // k (falls to the floor score, not withdrawn); the signal aggregate
        // is untouched.
        db.set_share_reports(&actors[0], false).await.unwrap();
        assert_eq!(
            bus_score(&db, &post, factor::REPORT_SPAM).await,
            Some(200),
            "one reporter opts out but three remain ≥ k → floor score, not withdrawn"
        );
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            Some(200),
            "report opt-out must NOT touch the same actor's signal rows"
        );

        // Symmetrically: opt an actor out of signals → signal drops below k
        // (withdrawn). The report aggregate — still observable at k=3
        // thanks to the fourth reporter — must be UNAFFECTED by this call:
        // this is the assertion the old two-actor form could never make,
        // since by this point its report aggregate was already `None`.
        db.set_share_signals(&actors[1], false).await.unwrap();
        assert_eq!(
            bus_score(&db, &post, factor::SIGNAL_WATCH_COMPLETE).await,
            None,
            "signal opt-out withdraws the signal aggregate (3→2)"
        );
        assert_eq!(
            bus_score(&db, &post, factor::REPORT_SPAM).await,
            Some(200),
            "signal opt-out must NOT touch the same actor's report rows"
        );
    }

    #[tokio::test]
    async fn content_is_public_gate() {
        let db = CacheDb::open_in_memory().unwrap();
        let public = [0x01; 32];
        let gated = [0x02; 32];
        let unseen = [0x03; 32];
        seed_public_post(&db, &public).await;
        seed_gated_post(&db, &gated).await;
        assert!(db.content_is_public(&public).await.unwrap());
        assert!(
            !db.content_is_public(&gated).await.unwrap(),
            "gated → not eligible"
        );
        assert!(
            !db.content_is_public(&unseen).await.unwrap(),
            "unseen (no content_meta row) → not eligible"
        );
    }
}
