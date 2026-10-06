//! Restore-divergence writer. Spec § D6 (γ).
//!
//! The (ε) crash-window placement-vs-SQLite reconciliation lives
//! in a sibling module — that's T9/T10's territory.

use anyhow::{Context, Result};
use rusqlite::Transaction;

/// Insert one `bridge_restore_divergence` row keyed to the most recent
/// `restore_history.snapshot_id` for `actor`. Returns the inserted row id.
///
/// If no `restore_history` row exists for the actor (the MUA-ahead state
/// predates any restore — not a divergence, just clock-skew or buggy
/// client), this is a no-op and returns `Ok(0)`.
///
/// `lost_event_count` is `max(client_modseq - server_modseq, 0)`.
pub fn write_divergence_row(
    tx: &Transaction<'_>,
    actor: &[u8; 32],
    protocol: &str,
    collection: &str,
    mua_id: Option<&str>,
    client_modseq: i64,
    server_modseq: i64,
    observed_at: i64,
) -> Result<i64> {
    let snapshot_id: Option<i64> = tx
        .query_row(
            "SELECT snapshot_id FROM restore_history
             WHERE actor_id = ?1
             ORDER BY completed_at DESC LIMIT 1",
            rusqlite::params![actor.as_slice()],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| {
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(e)
            }
        })
        .context("lookup most recent restore_history row")?;

    let Some(snapshot_id) = snapshot_id else {
        return Ok(0);
    };

    let lost_event_count = (client_modseq - server_modseq).max(0);

    tx.execute(
        "INSERT INTO bridge_restore_divergence
            (snapshot_id, observed_at, actor_id, protocol, collection,
             mua_id, client_modseq, server_modseq, lost_event_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            snapshot_id,
            observed_at,
            actor.as_slice(),
            protocol,
            collection,
            mua_id,
            client_modseq,
            server_modseq,
            lost_event_count,
        ],
    )
    .context("INSERT bridge_restore_divergence")?;

    Ok(tx.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    #[tokio::test]
    async fn writes_row_when_restore_history_exists() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x76u8; 32];

        // Seed a folder + snapshot + restore_history row.
        let fs_id = db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .expect("get_or_create_reserved_folder");
        let snap_id = db
            .create_message_kind_snapshot_row(fs_id, "mail", None, None)
            .await
            .expect("create_message_kind_snapshot_row");
        db.insert_restore_history(&actor, snap_id, "mail", None)
            .await
            .expect("insert_restore_history");

        let id = {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            let id = write_divergence_row(
                &tx,
                &actor,
                "imap",
                "INBOX",
                Some("Apple Mail/16.0"),
                500,
                420,
                1_700_000_000,
            )
            .expect("write_divergence_row");
            tx.commit().expect("commit");
            id
        };
        assert!(id > 0);

        let conn = db.conn().await;
        let (count, lost, snapshot_id, mua): (i64, i64, i64, Option<String>) = conn
            .query_row(
                "SELECT COUNT(*),
                        coalesce(MAX(lost_event_count), -1),
                        coalesce(MAX(snapshot_id), -1),
                        MAX(mua_id)
                 FROM bridge_restore_divergence
                 WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("count");
        assert_eq!(count, 1);
        assert_eq!(lost, 80);
        assert_eq!(snapshot_id, snap_id);
        assert_eq!(mua.as_deref(), Some("Apple Mail/16.0"));
    }

    #[tokio::test]
    async fn noop_when_no_restore_history() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x77u8; 32];

        let id = {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            let id =
                write_divergence_row(&tx, &actor, "imap", "INBOX", None, 100, 50, 1_700_000_000)
                    .expect("write_divergence_row");
            tx.commit().expect("commit");
            id
        };
        assert_eq!(id, 0);

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_restore_divergence WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn clamps_negative_lost_event_count_to_zero() {
        // If client_modseq < server_modseq (degenerate case — shouldn't
        // happen in the wild, but worth pinning), lost_event_count is 0.
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x76u8; 32];

        let fs_id = db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .expect("fs");
        let snap_id = db
            .create_message_kind_snapshot_row(fs_id, "mail", None, None)
            .await
            .expect("snap");
        db.insert_restore_history(&actor, snap_id, "mail", None)
            .await
            .expect("history");

        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            write_divergence_row(
                &tx,
                &actor,
                "imap",
                "INBOX",
                None,
                /* client_modseq */ 5,
                /* server_modseq */ 10,
                1_700_000_000,
            )
            .expect("write");
            tx.commit().expect("commit");
        }

        let conn = db.conn().await;
        let lost: i64 = conn
            .query_row(
                "SELECT lost_event_count FROM bridge_restore_divergence WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("lost");
        assert_eq!(lost, 0);
    }
}
