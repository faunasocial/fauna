//! The deployment spam baseline's delta-floor bookkeeping: the inclusion record
//! and the run-state singleton (`mail-spam.md` § Cold start, Path 2 → *The
//! floor applies to every published DELTA* and *Standing publish*, ruled
//! 2026-09-21).
//!
//! The published sum itself (`spam_baseline`) and the per-user models it is
//! summed from stay in `moderation.rs`; this module owns what the delta floor
//! needs on top of them — which contributors the last SERVED publish summed
//! (`spam_baseline_inclusions`), how many of those an account deletion has
//! purged since, and the last run's outcome (`spam_baseline_run_state`). None
//! of it is ever served to a client: the admin read
//! ([`CacheDb::get_spam_baseline_state`]) projects the published row and the
//! run outcome, never a contributor and never a withdrawal time.
//!
//! The orchestration (the holder drain, the two floors, the cadence) is
//! `crate::spam_baseline`.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_millis};

/// Which kind of departure [`super::moderation::withdraw_spam_baseline_if_contributor`]
/// is recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaselineDeparture {
    /// Opt-out, model reset or grant revoke: the account stays, so its
    /// inclusion row is marked departed (a rejoin before the next publish
    /// then counts once, not twice).
    AccountStands,
    /// Account deletion: the row goes with the account, and the purge is
    /// counted so the next publish still sees the departure.
    AccountDeleted,
}

/// One opted-in contributor with a model row, as a publish run reads it.
#[derive(Debug, Clone)]
pub struct BaselineContributor {
    pub actor: [u8; 32],
    pub model: Vec<u8>,
    /// `spam_models.updated_at` — strictly increasing per write.
    pub updated_at: i64,
}

/// One row of the inclusion record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inclusion {
    /// The `spam_models.updated_at` the last served publish summed.
    pub model_updated_at: i64,
    /// A departure that left the account standing.
    pub departed: bool,
}

/// The run-state singleton. A deployment that never ran a publish reads the
/// all-default value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpamBaselineRunState {
    /// Inclusion rows an account deletion purged since the last landed publish.
    pub purged_inclusions_since_publish: u32,
    /// When the last publish that LANDED was built. `Some` = the delta floor
    /// has a reference; never cleared, because whoever fetched a withdrawn
    /// baseline still holds it.
    pub last_served_at: Option<i64>,
    /// When the last run (click or cadence) finished, whatever its outcome.
    pub last_run_at: Option<i64>,
    /// The last run was deferred by the delta floor.
    pub deferred: bool,
    /// The last run's opted-in contributors it could not merge.
    pub skipped_contributors: u32,
    /// Bumped by every departure of a standing or summed contributor.
    pub departures: i64,
}

/// Everything a publish run reads before it merges, under one lock.
#[derive(Debug, Clone)]
pub struct SpamBaselineRunSnapshot {
    pub contributors: Vec<BaselineContributor>,
    pub inclusions: HashMap<[u8; 32], Inclusion>,
    pub state: SpamBaselineRunState,
}

/// A publish that passed both floors, ready to land.
#[derive(Debug, Clone)]
pub struct BaselineLanding<'a> {
    pub model_json: &'a [u8],
    pub ham_count: i64,
    pub spam_count: i64,
    pub contributors: i64,
    pub skipped_contributors: u32,
    /// The new inclusion record: every contributor this publish summed, with
    /// the `updated_at` it summed.
    pub summed: &'a [([u8; 32], i64)],
}

/// The published row, as the admin read projects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishedBaselineRow {
    pub contributors: u32,
    pub sample_count: u32,
    pub published_at: i64,
}

fn ensure_run_state(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO spam_baseline_run_state (id) VALUES (1)",
        [],
    )?;
    Ok(())
}

/// Bump the `departures` generation. Called by the withdrawal function only.
pub(super) fn bump_departures(conn: &rusqlite::Connection) -> Result<()> {
    ensure_run_state(conn)?;
    conn.execute(
        "UPDATE spam_baseline_run_state SET departures = departures + 1 WHERE id = 1",
        [],
    )?;
    Ok(())
}

/// Record one departure in the inclusion record. Called by the withdrawal
/// function only.
pub(super) fn record_departure(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
    departure: BaselineDeparture,
) -> Result<()> {
    let actor = actor_id.as_slice();
    match departure {
        BaselineDeparture::AccountStands => {
            conn.execute(
                "UPDATE spam_baseline_inclusions SET departed = 1 WHERE actor_id = ?1",
                rusqlite::params![actor],
            )?;
        }
        BaselineDeparture::AccountDeleted => {
            // A row already marked departed moves from "departed" to "purged":
            // counted once either way.
            let purged = conn.execute(
                "DELETE FROM spam_baseline_inclusions WHERE actor_id = ?1",
                rusqlite::params![actor],
            )?;
            if purged > 0 {
                ensure_run_state(conn)?;
                conn.execute(
                    "UPDATE spam_baseline_run_state
                     SET purged_inclusions_since_publish = purged_inclusions_since_publish + ?1
                     WHERE id = 1",
                    rusqlite::params![purged as i64],
                )?;
            }
        }
    }
    Ok(())
}

fn read_run_state(conn: &rusqlite::Connection) -> Result<SpamBaselineRunState> {
    let state = conn
        .query_row(
            "SELECT purged_inclusions_since_publish, last_served_at, last_run_at,
                    deferred, skipped_contributors, departures
             FROM spam_baseline_run_state WHERE id = 1",
            [],
            |row| {
                Ok(SpamBaselineRunState {
                    purged_inclusions_since_publish: row.get::<_, i64>(0)?.clamp(0, u32::MAX as i64)
                        as u32,
                    last_served_at: row.get(1)?,
                    last_run_at: row.get(2)?,
                    deferred: row.get::<_, i64>(3)? != 0,
                    skipped_contributors: row.get::<_, i64>(4)?.clamp(0, u32::MAX as i64) as u32,
                    departures: row.get(5)?,
                })
            },
        )
        .optional()?;
    Ok(state.unwrap_or_default())
}

fn record_run_outcome(
    conn: &rusqlite::Connection,
    now: i64,
    deferred: bool,
    skipped_contributors: u32,
) -> Result<()> {
    ensure_run_state(conn)?;
    conn.execute(
        "UPDATE spam_baseline_run_state
         SET last_run_at = ?1, deferred = ?2, skipped_contributors = ?3
         WHERE id = 1",
        rusqlite::params![now, deferred as i64, skipped_contributors as i64],
    )?;
    Ok(())
}

impl CacheDb {
    /// Everything a publish run reads before merging, under one lock: every
    /// opted-in contributor with a model row (`spam_preferences.
    /// contribute_baseline = 1`; an opt-in with no trained model yet has no
    /// row and contributes nothing), the inclusion record, and the run state.
    /// The actor ids stay inside the run — no reply or read carries them.
    pub async fn snapshot_spam_baseline_run(&self) -> Result<SpamBaselineRunSnapshot> {
        let conn = self.conn.lock().await;
        let mut contributors = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT m.actor_id, m.model_json, m.updated_at
                 FROM spam_models m
                 JOIN spam_preferences p ON p.actor_id = m.actor_id
                 WHERE p.contribute_baseline = 1
                 ORDER BY m.actor_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (actor, model, updated_at) = row?;
                let Ok(actor): std::result::Result<[u8; 32], _> = actor.as_slice().try_into()
                else {
                    continue;
                };
                contributors.push(BaselineContributor {
                    actor,
                    model,
                    updated_at,
                });
            }
        }
        let mut inclusions = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT actor_id, model_updated_at, departed FROM spam_baseline_inclusions",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (actor, model_updated_at, departed) = row?;
                let Ok(actor): std::result::Result<[u8; 32], _> = actor.as_slice().try_into()
                else {
                    continue;
                };
                inclusions.insert(
                    actor,
                    Inclusion {
                        model_updated_at,
                        departed: departed != 0,
                    },
                );
            }
        }
        let state = read_run_state(&conn)?;
        Ok(SpamBaselineRunSnapshot {
            contributors,
            inclusions,
            state,
        })
    }

    /// Land a publish that passed both floors: replace the published row,
    /// rewrite the inclusion record to exactly the contributors it summed, zero
    /// the purge count and stamp the reference — one transaction.
    ///
    /// Refuses (returns `false`, writing only the run outcome as deferred)
    /// when the `departures` generation moved since `departures_seen` was
    /// read: a contributor left while the run was merging, and landing would
    /// serve the counts the departure just withdrew. The next run rebuilds
    /// without them.
    pub async fn land_spam_baseline_publish(
        &self,
        landing: &BaselineLanding<'_>,
        departures_seen: i64,
    ) -> Result<bool> {
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        if read_run_state(&tx)?.departures != departures_seen {
            record_run_outcome(&tx, now, true, landing.skipped_contributors)?;
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO spam_baseline (id, model_json, ham_count, spam_count, contributors, published_at)
             VALUES (0, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                model_json = excluded.model_json,
                ham_count = excluded.ham_count,
                spam_count = excluded.spam_count,
                contributors = excluded.contributors,
                published_at = excluded.published_at",
            rusqlite::params![
                landing.model_json,
                landing.ham_count,
                landing.spam_count,
                landing.contributors,
                now
            ],
        )?;
        tx.execute("DELETE FROM spam_baseline_inclusions", [])?;
        {
            let mut insert = tx.prepare(
                "INSERT OR REPLACE INTO spam_baseline_inclusions
                     (actor_id, model_updated_at, departed)
                 VALUES (?1, ?2, 0)",
            )?;
            for (actor, updated_at) in landing.summed {
                insert.execute(rusqlite::params![actor.as_slice(), updated_at])?;
            }
        }
        ensure_run_state(&tx)?;
        tx.execute(
            "UPDATE spam_baseline_run_state
             SET purged_inclusions_since_publish = 0, last_served_at = ?1
             WHERE id = 1",
            rusqlite::params![now],
        )?;
        record_run_outcome(&tx, now, false, landing.skipped_contributors)?;
        tx.commit()?;
        Ok(true)
    }

    /// A run below the contributor floor: serve an empty baseline (withdrawing
    /// any prior one) and record the outcome. The inclusion record and the
    /// reference are untouched — whoever fetched the last served baseline
    /// still holds it, so the next landing is measured against that one.
    pub async fn withhold_spam_baseline(
        &self,
        contributors: i64,
        skipped_contributors: u32,
    ) -> Result<()> {
        let now = now_epoch_millis();
        let empty = fauna_mail::spam::SpamModel::new().to_bytes();
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO spam_baseline (id, model_json, ham_count, spam_count, contributors, published_at)
             VALUES (0, ?1, 0, 0, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                model_json = excluded.model_json,
                ham_count = 0,
                spam_count = 0,
                contributors = excluded.contributors,
                published_at = excluded.published_at",
            rusqlite::params![empty, contributors, now],
        )?;
        record_run_outcome(&tx, now, false, skipped_contributors)?;
        tx.commit()?;
        Ok(())
    }

    /// A run the delta floor deferred: nothing is written but its outcome.
    pub async fn record_deferred_spam_baseline_run(&self, skipped_contributors: u32) -> Result<()> {
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        record_run_outcome(&conn, now, true, skipped_contributors)
    }

    /// Withdraw the published baseline unconditionally — standing publish
    /// turned off (`mail-spam.md` § Cold start Path 2 → *Standing publish*).
    /// The reference and the inclusion record stay: the withdrawn sum is still
    /// in the hands of whoever fetched it. Returns whether a row was removed.
    pub async fn withdraw_spam_baseline(&self) -> Result<bool> {
        let conn = self.conn.lock().await;
        Ok(conn.execute("DELETE FROM spam_baseline WHERE id = 0", [])? > 0)
    }

    /// The admin read's two halves: the published row when a real baseline is
    /// served (a withheld run's empty row counts as none), and the run state.
    pub async fn get_spam_baseline_state(
        &self,
    ) -> Result<(Option<PublishedBaselineRow>, SpamBaselineRunState)> {
        let conn = self.conn.lock().await;
        let published = conn
            .query_row(
                "SELECT contributors, ham_count + spam_count, published_at
                 FROM spam_baseline WHERE id = 0",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
            .filter(|(contributors, _, _)| {
                *contributors >= fauna_mail::spam::BASELINE_MIN_CONTRIBUTORS as i64
            })
            .map(
                |(contributors, samples, published_at)| PublishedBaselineRow {
                    contributors: contributors.clamp(0, u32::MAX as i64) as u32,
                    sample_count: samples.clamp(0, u32::MAX as i64) as u32,
                    published_at,
                },
            );
        let state = read_run_state(&conn)?;
        Ok((published, state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: [u8; 32] = [7u8; 32];

    async fn contributing(db: &CacheDb, actor: &[u8; 32]) {
        db.put_spam_model_with_history(actor, &[0xEEu8; 64], None, None)
            .await
            .unwrap();
        let mut prefs = db.get_spam_preferences(actor).await.unwrap();
        prefs.contribute_baseline = true;
        db.upsert_spam_preferences(actor, &prefs).await.unwrap();
    }

    fn landing(summed: &[([u8; 32], i64)]) -> BaselineLanding<'_> {
        BaselineLanding {
            model_json: b"sum",
            ham_count: 1,
            spam_count: 1,
            contributors: 3,
            skipped_contributors: 0,
            summed,
        }
    }

    /// Each way a run ends — deferred by the delta floor, withheld below the
    /// contributor floor, landed — records that a run finished: the standing
    /// publish cadence reads `last_run_at` to decide the next one is not due
    /// yet. Pinned per path from a fresh database (a shared one would already
    /// hold the earlier path's stamp), so the answer never depends on the
    /// wall clock's granularity.
    #[tokio::test]
    async fn every_way_a_run_ends_records_that_it_finished() {
        async fn last_run(db: &CacheDb) -> (Option<i64>, bool) {
            let (_, state) = db.get_spam_baseline_state().await.unwrap();
            (state.last_run_at, state.deferred)
        }

        let db = CacheDb::open_in_memory().unwrap();
        db.record_deferred_spam_baseline_run(0).await.unwrap();
        let (at, deferred) = last_run(&db).await;
        assert!(at.is_some() && deferred, "a deferred run is still a run");

        let db = CacheDb::open_in_memory().unwrap();
        db.withhold_spam_baseline(1, 0).await.unwrap();
        let (at, deferred) = last_run(&db).await;
        assert!(at.is_some() && !deferred, "a withheld run is still a run");

        let db = CacheDb::open_in_memory().unwrap();
        let seen = db
            .snapshot_spam_baseline_run()
            .await
            .unwrap()
            .state
            .departures;
        let summed = [(ALICE, 1)];
        assert!(
            db.land_spam_baseline_publish(&landing(&summed), seen)
                .await
                .unwrap()
        );
        let (at, deferred) = last_run(&db).await;
        assert!(at.is_some() && !deferred, "a landed run records its finish");
    }

    /// A contributor who departs while a run is merging must not have their
    /// counts landed by that run: the generation it read has moved, so the
    /// landing refuses and writes only a deferred outcome.
    #[tokio::test]
    async fn a_departure_during_a_run_stops_it_landing() {
        let db = CacheDb::open_in_memory().unwrap();
        contributing(&db, &ALICE).await;
        let seen = db
            .snapshot_spam_baseline_run()
            .await
            .unwrap()
            .state
            .departures;

        db.withdraw_spam_baseline_if_contributor(&ALICE)
            .await
            .unwrap();

        let summed = [(ALICE, 1)];
        assert!(
            !db.land_spam_baseline_publish(&landing(&summed), seen)
                .await
                .unwrap()
        );
        assert_eq!(
            db.get_spam_baseline().await.unwrap(),
            None,
            "nothing landed"
        );
        let (published, state) = db.get_spam_baseline_state().await.unwrap();
        assert_eq!(published, None);
        assert!(state.deferred);
        assert_eq!(state.last_served_at, None, "no reference was set");
    }

    /// A departure marks the summed row (account standing) or counts it as
    /// purged (account deleted) — and a model write always moves `updated_at`,
    /// even inside one millisecond.
    #[tokio::test]
    async fn departures_reach_the_inclusion_record() {
        let db = CacheDb::open_in_memory().unwrap();
        let bob = [8u8; 32];
        contributing(&db, &ALICE).await;
        contributing(&db, &bob).await;
        let snap = db.snapshot_spam_baseline_run().await.unwrap();
        let summed: Vec<_> = snap
            .contributors
            .iter()
            .map(|c| (c.actor, c.updated_at))
            .collect();
        assert!(
            db.land_spam_baseline_publish(&landing(&summed), snap.state.departures)
                .await
                .unwrap()
        );

        let before = db.snapshot_spam_baseline_run().await.unwrap();
        db.put_spam_model_with_history(&ALICE, &[0xEEu8; 64], None, None)
            .await
            .unwrap();
        db.put_spam_model_with_history(&ALICE, &[0xEEu8; 64], None, None)
            .await
            .unwrap();
        let after = db.snapshot_spam_baseline_run().await.unwrap();
        let t = |s: &SpamBaselineRunSnapshot| {
            s.contributors
                .iter()
                .find(|c| c.actor == ALICE)
                .unwrap()
                .updated_at
        };
        assert!(t(&after) >= t(&before) + 2, "each write moves updated_at");

        db.withdraw_spam_baseline_if_contributor(&ALICE)
            .await
            .unwrap();
        db.purge_orphaned_actor_rows(&bob).await.unwrap();
        let snap = db.snapshot_spam_baseline_run().await.unwrap();
        assert!(
            snap.inclusions[&ALICE].departed,
            "opt-out marks, keeps the row"
        );
        assert!(
            !snap.inclusions.contains_key(&bob),
            "deletion purges the row"
        );
        assert_eq!(
            snap.state.purged_inclusions_since_publish, 1,
            "and counts it"
        );
        assert_eq!(snap.state.departures, before.state.departures + 2);
        assert!(
            snap.state.last_served_at.is_some(),
            "a withdrawal keeps the reference"
        );
    }

    /// Standing is not enough: an actor with no summed row withdraws nothing,
    /// and a summed one withdraws once.
    #[tokio::test]
    async fn only_a_non_departed_inclusion_row_withdraws_a_recorded_baseline() {
        let db = CacheDb::open_in_memory().unwrap();
        let bob = [8u8; 32];
        contributing(&db, &ALICE).await;
        let seen = db
            .snapshot_spam_baseline_run()
            .await
            .unwrap()
            .state
            .departures;
        let summed = [(ALICE, 1)];
        assert!(
            db.land_spam_baseline_publish(&landing(&summed), seen)
                .await
                .unwrap()
        );
        contributing(&db, &bob).await;

        assert!(
            !db.withdraw_spam_baseline_if_contributor(&bob)
                .await
                .unwrap(),
            "bob stands but was not summed"
        );
        assert!(db.get_spam_baseline().await.unwrap().is_some());

        assert!(
            db.withdraw_spam_baseline_if_contributor(&ALICE)
                .await
                .unwrap(),
            "alice was summed"
        );
        assert_eq!(db.get_spam_baseline().await.unwrap(), None);
    }

    /// A row a previous departure already marked withdraws nothing: a summed
    /// contributor withdraws once per publish they were summed into. The only
    /// `spam_baseline` row that can stand beside a marked row is a later
    /// below-floor withhold (a landing rewrites the record), which serves
    /// nothing — so this pins the rule where it is observable, the table.
    #[tokio::test]
    async fn a_departed_inclusion_row_withdraws_nothing_again() {
        let db = CacheDb::open_in_memory().unwrap();
        contributing(&db, &ALICE).await;
        let seen = db
            .snapshot_spam_baseline_run()
            .await
            .unwrap()
            .state
            .departures;
        let summed = [(ALICE, 1)];
        assert!(
            db.land_spam_baseline_publish(&landing(&summed), seen)
                .await
                .unwrap()
        );
        assert!(
            db.withdraw_spam_baseline_if_contributor(&ALICE)
                .await
                .unwrap()
        );
        db.withhold_spam_baseline(1, 0).await.unwrap();
        assert!(db.get_spam_baseline().await.unwrap().is_some());

        assert!(
            !db.withdraw_spam_baseline_if_contributor(&ALICE)
                .await
                .unwrap(),
            "alice's row is already marked departed"
        );
        assert!(db.get_spam_baseline().await.unwrap().is_some());
    }
}
