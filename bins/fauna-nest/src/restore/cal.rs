//! Replay a CalPlacementManifest's collection + tombstone halves into the
//! actor's bridge_caldav_* tables inside an open SQLite transaction. Pure
//! in-process; no I/O.
//!
//! The event *rows* are rebuilt by the caller
//! (`filesync_handlers.rs::restore_calendar`) — they join the placement
//! entries (etag / modseq / `encrypted_fauna_ext`, the SQL-allocated state a
//! content record must not carry) against the on-disk content records'
//! floors + envelopes, which needs segment I/O this module deliberately
//! avoids.
//!
//! Caller holds the transaction and performs the pre-replay DELETE of the
//! actor's rows; commit/rollback is the caller's.

use anyhow::{Context, Result};
use fauna_calendar::segments::placement::CalPlacementManifest;
use rusqlite::Transaction;

/// Replay the compacted manifest state into the actor's
/// bridge_caldav_calendars table. Writes one row per calendar entry.
///
/// Caller is responsible for:
/// 1. The enclosing SQLite transaction (`tx`).
/// 2. The pre-replay DELETE of existing bridge_caldav_calendars rows for
///    this actor (so this function is idempotent when called after a
///    clean slate).
/// 3. Committing (or rolling back) the transaction.
pub fn replay_cal_manifest_into_sqlite(
    tx: &Transaction<'_>,
    actor: &[u8; 32],
    manifest: &CalPlacementManifest,
) -> Result<()> {
    // bridge_caldav_calendars:
    //   (actor_id, calendar_id, encrypted_metadata, ctag, highestmodseq,
    //    created_at)
    // ctag mirrors highestmodseq — the live write path keeps the two in
    // lockstep (`SET highestmodseq = ?1, ctag = ?1`); a restored ctag of 0
    // would tell a PROPFIND-polling MUA "nothing changed" across a revert.
    // created_at synthesized as 0 — consumers use highestmodseq / etag for
    // ordering, not timestamps.
    for c in &manifest.calendars {
        tx.execute(
            "INSERT INTO bridge_caldav_calendars
                (actor_id, calendar_id, encrypted_metadata, ctag, highestmodseq, created_at)
             VALUES (?1, ?2, ?3, ?4, ?4, 0)",
            rusqlite::params![
                actor.as_slice(),
                c.calendar_id.as_slice(),
                &c.encrypted_metadata,
                c.highestmodseq as i64,
            ],
        )
        .context("INSERT bridge_caldav_calendars")?;
    }
    Ok(())
}

/// Replay the manifest's tombstones into bridge_caldav_expunged so
/// WebDAV-Sync serves deletions across a restore. Every tombstone carries
/// the row's `event_id` and delete time (the pre-sweep id-less tombstone and
/// its skip arm were retired by the compat-remnant sweep, 2026-09-24).
pub fn replay_cal_tombstones_into_sqlite(
    tx: &Transaction<'_>,
    actor: &[u8; 32],
    manifest: &CalPlacementManifest,
) -> Result<()> {
    for t in &manifest.tombstones {
        tx.execute(
            "INSERT INTO bridge_caldav_expunged
                (actor_id, calendar_id, event_id, uid_hash, modseq, expunged_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                actor.as_slice(),
                t.calendar_id.as_slice(),
                t.event_id.as_slice(),
                t.uid_hash.as_slice(),
                t.modseq as i64,
                t.deleted_at,
            ],
        )
        .context("INSERT bridge_caldav_expunged")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use fauna_calendar::segments::placement::CalendarState;

    fn populated_manifest() -> CalPlacementManifest {
        let mut m = CalPlacementManifest::new();
        m.calendars.push(CalendarState {
            calendar_id: [0xAAu8; 32],
            encrypted_metadata: vec![0u8; 64],
            highestmodseq: 50,
        });
        m
    }

    #[tokio::test]
    async fn replays_calendar_compacted_manifest() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x72u8; 32];
        let manifest = populated_manifest();

        // write phase — guard dropped at end of block (re-entrant tokio Mutex)
        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            replay_cal_manifest_into_sqlite(&tx, &actor, &manifest).expect("replay");
            tx.commit().expect("commit");
        }

        // read phase
        let conn = db.conn().await;
        let cal_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_calendars WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count calendars");
        assert_eq!(cal_count, 1);

        // Round-trip values: encrypted_metadata + highestmodseq are
        // restored verbatim from the manifest; created_at synthesized as 0.
        let (md_len, hms, created_at): (i64, i64, i64) = conn
            .query_row(
                "SELECT length(encrypted_metadata), highestmodseq, created_at
                 FROM bridge_caldav_calendars
                 WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("row");
        assert_eq!(md_len, 64);
        assert_eq!(hms, 50);
        assert_eq!(created_at, 0);
    }

    #[tokio::test]
    async fn empty_manifest_writes_zero_calendars() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x73u8; 32];
        let manifest = CalPlacementManifest::new();

        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            replay_cal_manifest_into_sqlite(&tx, &actor, &manifest).expect("replay");
            tx.commit().expect("commit");
        }

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_calendars WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn manifest_replay_restores_calendars_but_never_event_rows() {
        // Division of labor (S6.9): this module replays the pure-SQL halves
        // (calendars here; tombstones via replay_cal_tombstones_into_sqlite);
        // the event ROWS are rebuilt by filesync_handlers::restore_calendar,
        // which joins placement entries against on-disk floors/envelopes.
        // This pins that replay_cal_manifest_into_sqlite alone never writes
        // an event row — a change collapsing the division would break loudly.

        use fauna_calendar::segments::placement::{EventPlacement, EventTombstoneRef};

        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x72u8; 32];

        let mut manifest = CalPlacementManifest::new();
        manifest.calendars.push(CalendarState {
            calendar_id: [0xAAu8; 32],
            encrypted_metadata: vec![0u8; 64],
            highestmodseq: 50,
        });
        manifest.events.push(EventPlacement {
            event_id: [0xB1u8; 32],
            encrypted_fauna_ext: None,
            calendar_id: [0xAAu8; 32],
            uid_hash: [0xBBu8; 32],
            etag: "0000000000000005".to_string(),
            modseq: 5,
            ciphertext_size: 1024,
        });
        manifest.tombstones.push(EventTombstoneRef {
            event_id: [0xB2u8; 32],
            deleted_at: 1_752_000_000,
            calendar_id: [0xAAu8; 32],
            uid_hash: [0xCCu8; 32],
            modseq: 10,
        });

        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            replay_cal_manifest_into_sqlite(&tx, &actor, &manifest).expect("replay");
            tx.commit().expect("commit");
        }

        let conn = db.conn().await;
        let cal_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_calendars WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count cal");
        assert_eq!(cal_count, 1, "calendars row restored");

        let evt_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count events");
        assert_eq!(
            evt_count, 0,
            "event rows are the filesync caller's job (floor/envelope join)"
        );
    }

    /// S6.9: tombstones (carrying the row's event_id + deleted_at) replay
    /// into bridge_caldav_expunged.
    #[tokio::test]
    async fn tombstone_replay_restores_entries() {
        use fauna_calendar::segments::placement::EventTombstoneRef;

        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x73u8; 32];
        let mut manifest = CalPlacementManifest::new();
        manifest.tombstones.push(EventTombstoneRef {
            calendar_id: [0xAAu8; 32],
            uid_hash: [0xCCu8; 32],
            modseq: 10,
            event_id: [0xDDu8; 32],
            deleted_at: 1_752_000_000,
        });

        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            replay_cal_tombstones_into_sqlite(&tx, &actor, &manifest).expect("replay");
            tx.commit().expect("commit");
        }

        let conn = db.conn().await;
        let (count, event_id, expunged_at): (i64, Vec<u8>, i64) = conn
            .query_row(
                "SELECT COUNT(*), event_id, expunged_at
                 FROM bridge_caldav_expunged WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("expunged row");
        assert_eq!(count, 1, "the tombstone restored");
        assert_eq!(event_id, vec![0xDDu8; 32]);
        assert_eq!(expunged_at, 1_752_000_000);
    }
}
