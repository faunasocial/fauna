//! Per-actor forward holding queue (`forward_queue`).
//!
//! Backs the N5 rate-cap (`mail-forwarding.md` § Queue ceiling `:181-185`). A
//! forward that would exceed the per-account hourly rate cap is parked here
//! rather than dispatched, then promoted into `outbound_mail_queue` at the
//! rate-cap cadence by the promotion step folded into `fetch_outbound_due`.
//! Nest is authoritative, so the queue survives an MTA restart.
//!
//! A parked forward stores the **original** envelope (`original_sender`) + the
//! `raw_message`; the SRS rewrite happens after promotion, keyed on the new
//! `outbound_mail_queue` row id, exactly like any other forwarded row (the
//! shipped N3 design — see the table comment in `migrations.rs`).
//!
//! Eviction is FIFO **newest-evicts-oldest** at the ceiling, **among `copy` rows
//! only** (`mail-forwarding.md` § Queue ceiling): on insert, if the actor's queue
//! exceeds `min(cap, ceiling) * 24`, the **oldest** parked `copy` forwards are
//! dropped (lowest `id`) and their destinations returned so the caller can fire
//! the in-app eviction notification. A `copy` row is a second copy — the original
//! rests in the mailbox. A `redirect` row (and one of unknown mode) is the only
//! copy of mail already answered `250`, so it is never evicted: a `redirect`
//! forward the ceiling has no `copy` row left to make room for is refused, and
//! the MTA keeps the mail (local fallback, or a `451`).

use super::{CacheDb, now_epoch_secs};
use anyhow::{Context, Result};
use fauna_protocol::bridge_routing::ForwardCopyMode;
use rusqlite::OptionalExtension;

/// The persisted spelling of a forward's copy mode — `forward_queue.copy_mode`
/// and `outbound_mail_queue.forward_copy_mode` both hold it. Whether a queued
/// forward is a *second* copy (`copy`) or the *only* one (`redirect`) is what a
/// succession's burn and the queue ceiling's eviction are ruled by, so the mode
/// is persisted on the row rather than inferred from the rule class.
pub(crate) fn copy_mode_sql(mode: ForwardCopyMode) -> &'static str {
    match mode {
        ForwardCopyMode::Copy => "copy",
        ForwardCopyMode::Redirect => "redirect",
    }
}

/// Read a persisted copy mode back. `None` for NULL (a row queued before the
/// column existed) and for any spelling this binary does not know — both read
/// as *unknown*, which every consumer treats as possibly the only copy.
pub(crate) fn copy_mode_from_sql(value: Option<&str>) -> Option<ForwardCopyMode> {
    match value {
        Some("copy") => Some(ForwardCopyMode::Copy),
        Some("redirect") => Some(ForwardCopyMode::Redirect),
        _ => None,
    }
}

/// A forward to park in the holding queue.
#[derive(Debug, Clone)]
pub struct NewForwardQueueEntry<'a> {
    pub actor_id: &'a [u8; 32],
    pub source_message_id: &'a str,
    pub original_sender: &'a str,
    pub destination_address: &'a str,
    pub rule_id_or_forward_all: &'a str,
    pub raw_message: &'a [u8],
    /// Whether the original also rests in the mailbox (`Copy`) or this row is
    /// its only copy (`Redirect`) — carried onto the outbound row at promotion.
    pub copy_mode: ForwardCopyMode,
}

/// A parked forward, read back for promotion into `outbound_mail_queue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardQueueRow {
    pub id: i64,
    pub actor_id: [u8; 32],
    pub queued_at: i64,
    pub source_message_id: String,
    pub original_sender: String,
    pub destination_address: String,
    pub rule_id_or_forward_all: String,
    pub raw_message: Vec<u8>,
    /// `None` = unknown (parked before the column existed).
    pub copy_mode: Option<ForwardCopyMode>,
}

/// The result of parking a forward: the new row id if it was kept, plus any
/// **evicted** `copy` destinations (FIFO oldest-dropped at the ceiling) the
/// caller should notify.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ForwardQueueEnqueueOutcome {
    /// `None` = the forward was NOT parked: a `copy` forward that was itself the
    /// oldest copy left at the ceiling (evicted — its destination is in
    /// `evicted_destinations`), or a non-`copy` forward refused because no
    /// `copy` row was left to evict (nothing was written or evicted).
    pub id: Option<i64>,
    pub evicted_destinations: Vec<String>,
}

impl CacheDb {
    /// Park a forward and enforce the per-actor ceiling by evicting the oldest
    /// `copy` rows (lowest `id`) and collecting their destinations for the
    /// eviction notification. A non-`copy` forward is parked only if enough
    /// `copy` rows exist to bring the queue back to the ceiling — otherwise it
    /// is refused before anything is written or evicted (no copy is dropped to
    /// make room for a forward that would not fit anyway). A `ceiling` of 0
    /// therefore parks nothing: a `copy` forward is evicted as it lands, a
    /// `redirect` one refused. One transaction, so a crash never leaves a
    /// half-applied eviction.
    pub async fn enqueue_forward_queue(
        &self,
        entry: NewForwardQueueEntry<'_>,
        ceiling: u32,
    ) -> Result<ForwardQueueEnqueueOutcome> {
        let now = now_epoch_secs();
        let copy = copy_mode_sql(ForwardCopyMode::Copy);
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("enqueue_forward_queue begin")?;
        let count_where = |filter: &str| -> Result<u32> {
            tx.query_row(
                &format!("SELECT COUNT(*) FROM forward_queue WHERE actor_id = ?1{filter}"),
                rusqlite::params![entry.actor_id.as_slice()],
                |row| row.get(0),
            )
            .context("enqueue_forward_queue count")
        };
        if entry.copy_mode != ForwardCopyMode::Copy {
            // How many rows must go for this one to fit; only copies may.
            let over = (count_where("")? + 1).saturating_sub(ceiling);
            if over > 0 && count_where(" AND copy_mode = 'copy'")? < over {
                return Ok(ForwardQueueEnqueueOutcome::default());
            }
        }
        tx.execute(
            "INSERT INTO forward_queue
                (actor_id, queued_at, source_message_id, original_sender,
                 destination_address, rule_id_or_forward_all, raw_message, copy_mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                entry.actor_id.as_slice(),
                now,
                entry.source_message_id,
                entry.original_sender,
                entry.destination_address,
                entry.rule_id_or_forward_all,
                entry.raw_message,
                copy_mode_sql(entry.copy_mode),
            ],
        )
        .context("enqueue_forward_queue insert")?;
        let inserted = tx.last_insert_rowid();

        // FIFO newest-evicts-oldest among copies: while over the ceiling, drop
        // the oldest `copy` row. The new row is the newest, so a `copy` forward
        // is itself evicted only once no older copy is left.
        let mut id = Some(inserted);
        let mut evicted_destinations = Vec::new();
        while count_where("")? > ceiling {
            let oldest: Option<(i64, String)> = tx
                .query_row(
                    "SELECT id, destination_address FROM forward_queue
                     WHERE actor_id = ?1 AND copy_mode = ?2 ORDER BY id ASC LIMIT 1",
                    rusqlite::params![entry.actor_id.as_slice(), copy],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .context("enqueue_forward_queue oldest copy")?;
            let Some((oldest_id, dest)) = oldest else {
                break;
            };
            tx.execute(
                "DELETE FROM forward_queue WHERE id = ?1",
                rusqlite::params![oldest_id],
            )
            .context("enqueue_forward_queue evict")?;
            if oldest_id == inserted {
                id = None;
            }
            evicted_destinations.push(dest);
        }
        tx.commit().context("enqueue_forward_queue commit")?;

        Ok(ForwardQueueEnqueueOutcome {
            id,
            evicted_destinations,
        })
    }

    /// Number of forwards currently parked for `actor_id`.
    pub async fn count_forward_queue(&self, actor_id: &[u8; 32]) -> Result<u32> {
        let conn = self.conn.lock().await;
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM forward_queue WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get(0),
            )
            .context("count_forward_queue")?;
        Ok(count)
    }

    /// The `limit` oldest parked forwards for `actor_id` (FIFO order, lowest
    /// `id` first) — the promotion cursor.
    pub async fn fetch_forward_queue_oldest(
        &self,
        actor_id: &[u8; 32],
        limit: u32,
    ) -> Result<Vec<ForwardQueueRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, actor_id, queued_at, source_message_id, original_sender,
                    destination_address, rule_id_or_forward_all, raw_message, copy_mode
             FROM forward_queue WHERE actor_id = ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![actor_id.as_slice(), limit],
            row_to_forward_queue,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Delete a parked forward by id (called on promotion).
    pub async fn delete_forward_queue(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM forward_queue WHERE id = ?1",
            rusqlite::params![id],
        )
        .context("delete_forward_queue")?;
        Ok(())
    }

    /// Every distinct actor with at least one parked forward — the promotion
    /// step iterates these and applies each actor's rate window.
    pub async fn distinct_forward_queue_actors(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT DISTINCT actor_id FROM forward_queue")?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(actor) = <[u8; 32]>::try_from(r?) {
                out.push(actor);
            }
        }
        Ok(out)
    }
}

fn row_to_forward_queue(row: &rusqlite::Row<'_>) -> rusqlite::Result<ForwardQueueRow> {
    let actor_blob: Vec<u8> = row.get(1)?;
    let actor_id = <[u8; 32]>::try_from(actor_blob).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Blob,
            "forward_queue.actor_id is not 32 bytes".into(),
        )
    })?;
    Ok(ForwardQueueRow {
        id: row.get(0)?,
        actor_id,
        queued_at: row.get(2)?,
        source_message_id: row.get(3)?,
        original_sender: row.get(4)?,
        destination_address: row.get(5)?,
        rule_id_or_forward_all: row.get(6)?,
        raw_message: row.get(7)?,
        copy_mode: copy_mode_from_sql(row.get::<_, Option<String>>(8)?.as_deref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    fn entry<'a>(
        actor: &'a [u8; 32],
        msgid: &'a str,
        dest: &'a str,
        body: &'a [u8],
    ) -> NewForwardQueueEntry<'a> {
        NewForwardQueueEntry {
            actor_id: actor,
            source_message_id: msgid,
            original_sender: "alice@example.com",
            destination_address: dest,
            rule_id_or_forward_all: "forward-all",
            raw_message: body,
            copy_mode: ForwardCopyMode::Copy,
        }
    }

    #[tokio::test]
    async fn enqueue_and_count_and_fetch_fifo() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        for i in 0..3 {
            let out = db
                .enqueue_forward_queue(
                    entry(&actor, &format!("m{i}"), &format!("d{i}@x.test"), b"raw"),
                    100,
                )
                .await
                .unwrap();
            assert!(out.evicted_destinations.is_empty());
        }
        assert_eq!(db.count_forward_queue(&actor).await.unwrap(), 3);
        let oldest = db.fetch_forward_queue_oldest(&actor, 2).await.unwrap();
        assert_eq!(oldest.len(), 2);
        assert_eq!(oldest[0].source_message_id, "m0");
        assert_eq!(oldest[1].source_message_id, "m1");
        assert_eq!(oldest[0].original_sender, "alice@example.com");
        assert_eq!(oldest[0].rule_id_or_forward_all, "forward-all");
        assert_eq!(oldest[0].copy_mode, Some(ForwardCopyMode::Copy));
    }

    #[tokio::test]
    async fn ceiling_evicts_oldest_fifo_and_reports_destination() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [2u8; 32];
        // Ceiling 2: parking a third forward drops the oldest (d0).
        db.enqueue_forward_queue(entry(&actor, "m0", "d0@x.test", b"r"), 2)
            .await
            .unwrap();
        db.enqueue_forward_queue(entry(&actor, "m1", "d1@x.test", b"r"), 2)
            .await
            .unwrap();
        let out = db
            .enqueue_forward_queue(entry(&actor, "m2", "d2@x.test", b"r"), 2)
            .await
            .unwrap();
        assert_eq!(out.evicted_destinations, vec!["d0@x.test".to_string()]);
        assert_eq!(db.count_forward_queue(&actor).await.unwrap(), 2);
        let remaining = db.fetch_forward_queue_oldest(&actor, 10).await.unwrap();
        let msgids: Vec<_> = remaining
            .iter()
            .map(|r| r.source_message_id.clone())
            .collect();
        assert_eq!(msgids, vec!["m1".to_string(), "m2".to_string()]);
    }

    #[tokio::test]
    async fn delete_and_distinct_actors_are_per_actor_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = [3u8; 32];
        let b = [4u8; 32];
        let oa = db
            .enqueue_forward_queue(entry(&a, "ma", "da@x.test", b"r"), 100)
            .await
            .unwrap();
        db.enqueue_forward_queue(entry(&b, "mb", "db@x.test", b"r"), 100)
            .await
            .unwrap();
        let mut actors = db.distinct_forward_queue_actors().await.unwrap();
        actors.sort();
        assert_eq!(actors, vec![a, b]);

        db.delete_forward_queue(oa.id.unwrap()).await.unwrap();
        assert_eq!(db.count_forward_queue(&a).await.unwrap(), 0);
        assert_eq!(db.count_forward_queue(&b).await.unwrap(), 1);
        assert_eq!(
            db.distinct_forward_queue_actors().await.unwrap(),
            vec![b],
            "an emptied actor drops out of the promotion set"
        );
    }

    fn with_mode<'a>(
        mut e: NewForwardQueueEntry<'a>,
        mode: ForwardCopyMode,
    ) -> NewForwardQueueEntry<'a> {
        e.copy_mode = mode;
        e
    }

    async fn parked_msgids(db: &CacheDb, actor: &[u8; 32]) -> Vec<String> {
        db.fetch_forward_queue_oldest(actor, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.source_message_id)
            .collect()
    }

    /// A `redirect` row is the only copy of mail the MTA already answered 250
    /// for, so the ceiling's FIFO eviction passes over it to the oldest `copy`
    /// row, and a `redirect` forward with no copy left to make room for is
    /// refused — nothing written, nothing evicted — so the MTA keeps the mail
    /// (`mail-forwarding.md` § Queue ceiling).
    #[tokio::test]
    async fn ceiling_evicts_only_copy_rows_and_refuses_an_unfittable_redirect() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [5u8; 32];
        let park = |msgid: &'static str, mode| {
            let db = &db;
            async move {
                db.enqueue_forward_queue(with_mode(entry(&actor, msgid, msgid, b"r"), mode), 2)
                    .await
                    .unwrap()
            }
        };
        park("r0", ForwardCopyMode::Redirect).await;
        park("c1", ForwardCopyMode::Copy).await;
        // Over the ceiling: the OLDEST row is the redirect, but only the copy goes.
        let out = park("c2", ForwardCopyMode::Copy).await;
        assert_eq!(out.evicted_destinations, vec!["c1".to_string()]);
        assert!(out.id.is_some());
        assert_eq!(parked_msgids(&db, &actor).await, vec!["r0", "c2"]);
        // A redirect over the ceiling evicts the one copy left…
        let out = park("r3", ForwardCopyMode::Redirect).await;
        assert_eq!(out.evicted_destinations, vec!["c2".to_string()]);
        assert!(out.id.is_some());
        // …and with none left, the next redirect is refused outright.
        let out = park("r4", ForwardCopyMode::Redirect).await;
        assert_eq!(
            out,
            ForwardQueueEnqueueOutcome::default(),
            "refused: no id, nothing evicted"
        );
        assert_eq!(parked_msgids(&db, &actor).await, vec!["r0", "r3"]);
        // A copy arriving at a queue full of redirects is the copy that drops.
        let out = park("c5", ForwardCopyMode::Copy).await;
        assert_eq!(out.id, None);
        assert_eq!(out.evicted_destinations, vec!["c5".to_string()]);
        assert_eq!(parked_msgids(&db, &actor).await, vec!["r0", "r3"]);
    }

    /// A row of unknown mode (parked before the column existed) may be the
    /// only copy, so eviction treats it like a `redirect`: never selected.
    #[tokio::test]
    async fn ceiling_never_evicts_a_row_of_unknown_mode() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [6u8; 32];
        let out = db
            .enqueue_forward_queue(entry(&actor, "legacy", "legacy", b"r"), 1)
            .await
            .unwrap();
        {
            let conn = db.conn().await;
            conn.execute(
                "UPDATE forward_queue SET copy_mode = NULL WHERE id = ?1",
                rusqlite::params![out.id.unwrap()],
            )
            .unwrap();
        }
        let out = db
            .enqueue_forward_queue(entry(&actor, "c1", "c1", b"r"), 1)
            .await
            .unwrap();
        assert_eq!(out.id, None);
        assert_eq!(out.evicted_destinations, vec!["c1".to_string()]);
        assert_eq!(parked_msgids(&db, &actor).await, vec!["legacy"]);
    }

    /// Ceiling 0 parks nothing in either mode — and says so (`id: None`),
    /// where it used to hand back the id of a row it had just deleted.
    #[tokio::test]
    async fn ceiling_zero_parks_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [8u8; 32];
        let out = db
            .enqueue_forward_queue(entry(&actor, "c", "c@x.test", b"r"), 0)
            .await
            .unwrap();
        assert_eq!(out.id, None);
        assert_eq!(out.evicted_destinations, vec!["c@x.test".to_string()]);
        let out = db
            .enqueue_forward_queue(
                with_mode(
                    entry(&actor, "r", "r@x.test", b"r"),
                    ForwardCopyMode::Redirect,
                ),
                0,
            )
            .await
            .unwrap();
        assert_eq!(out, ForwardQueueEnqueueOutcome::default());
        assert_eq!(db.count_forward_queue(&actor).await.unwrap(), 0);
    }
}
