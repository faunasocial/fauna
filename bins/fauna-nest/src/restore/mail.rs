//! Replay a MailPlacementManifest into the actor's bridge_imap_* tables
//! inside an open SQLite transaction. Pure in-process; no I/O.
//!
//! The manifest carries the compacted current state per spec § D3.
//! Replay writes one row per manifest entry; no event-by-event
//! re-execution.
//!
//! Caller (filesync_handlers.rs::restore_mail in T4) holds the
//! transaction, performs the pre-replay DELETE of existing rows, and
//! commits after rebuild + replay are both done.

use anyhow::{Context, Result};
use fauna_mail::segments::placement::MailPlacementManifest;
use rusqlite::Transaction;

/// Replay the compacted manifest state into the actor's bridge_imap_*
/// tables. Writes one row per manifest entry; no event-by-event replay.
///
/// Caller is responsible for:
/// 1. The enclosing SQLite transaction (`tx`).
/// 2. The pre-replay DELETE of existing rows for this actor (so this
///    function is idempotent when called after a clean slate).
/// 3. Committing (or rolling back) the transaction.
pub fn replay_mail_manifest_into_sqlite(
    tx: &Transaction<'_>,
    actor: &[u8; 32],
    manifest: &MailPlacementManifest,
) -> Result<()> {
    // bridge_imap_mailbox_state: (actor_id, mailbox, uid_validity, uid_next,
    // highestmodseq, pruned_modseq). The prune floor rides the restore: the
    // tombstones replayed below are only those the retention prune kept, so
    // the rebuilt expunge log is incomplete below it (imap-server.md § QRESYNC).
    // Note: manifest's MailboxState.attrs is manifest-only — no attrs column in the DB.
    for m in &manifest.mailboxes {
        tx.execute(
            "INSERT INTO bridge_imap_mailbox_state
                (actor_id, mailbox, uid_validity, uid_next, highestmodseq, pruned_modseq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                actor.as_slice(),
                &m.name,
                m.uid_validity as i64,
                m.uid_next as i64,
                m.highestmodseq as i64,
                m.pruned_modseq as i64,
            ],
        )
        .context("INSERT bridge_imap_mailbox_state")?;
    }

    // bridge_imap_messages: flags are space-separated tokens — the same encoding
    // that apply_append / place_inbound_mail / COPY use. No JSON here.
    // message_id maps from manifest's content_record_id (the routing pointer).
    // created_at synthesized as 0 — consumers order by internal_date.
    for p in &manifest.placements {
        let flags_str = p.flags.join(" ");
        tx.execute(
            "INSERT INTO bridge_imap_messages
                (actor_id, mailbox, uid, message_id, flags, modseq,
                 internal_date, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
            rusqlite::params![
                actor.as_slice(),
                &p.mailbox,
                p.uid as i64,
                p.content_record_id.as_slice(),
                flags_str,
                p.modseq as i64,
                p.internal_date,
            ],
        )
        .context("INSERT bridge_imap_messages")?;
    }

    // bridge_imap_expunged: (actor_id, mailbox, uid, modseq, expunged_at)
    // expunged_at from the tombstone's `deleted_at`.
    for t in &manifest.tombstones {
        tx.execute(
            "INSERT INTO bridge_imap_expunged
                (actor_id, mailbox, uid, modseq, expunged_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                actor.as_slice(),
                &t.mailbox,
                t.uid as i64,
                t.modseq as i64,
                t.deleted_at,
            ],
        )
        .context("INSERT bridge_imap_expunged")?;
    }

    // bridge_imap_subscriptions: (actor_id, mailbox)
    for sub in &manifest.subscriptions {
        tx.execute(
            "INSERT INTO bridge_imap_subscriptions (actor_id, mailbox)
             VALUES (?1, ?2)",
            rusqlite::params![actor.as_slice(), sub],
        )
        .context("INSERT bridge_imap_subscriptions")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use fauna_mail::segments::placement::{MailboxState, RecordPlacement, TombstoneRef};

    /// Build a populated manifest on actor 0x70 and return it.
    fn populated_manifest() -> MailPlacementManifest {
        let mut manifest = MailPlacementManifest::new();
        manifest.mailboxes.push(MailboxState {
            name: "INBOX".to_string(),
            uid_validity: 1_700_000_000,
            uid_next: 42,
            highestmodseq: 100,
            attrs: vec![],
            pruned_modseq: 50,
        });
        manifest.placements.push(RecordPlacement {
            mailbox: "INBOX".to_string(),
            uid: 1,
            modseq: 2,
            flags: vec!["\\Seen".to_string()],
            content_record_id: vec![0x11u8; 32],
            internal_date: 1_700_000_500,
        });
        manifest.tombstones.push(TombstoneRef {
            mailbox: "INBOX".to_string(),
            uid: 99,
            modseq: 99,
            deleted_at: 1_752_000_000,
        });
        manifest.subscriptions.push("INBOX".to_string());
        manifest
    }

    #[tokio::test]
    async fn replays_compacted_manifest_into_empty_tables() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x70u8; 32];
        let manifest = populated_manifest();

        // write phase — guard dropped at end of block
        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            replay_mail_manifest_into_sqlite(&tx, &actor, &manifest).expect("replay");
            tx.commit().expect("commit");
        }

        // read phase — safe to re-acquire
        let conn = db.conn().await;
        let mb_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_mailbox_state WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count mailbox_state");
        assert_eq!(mb_count, 1);
        let floor: i64 = conn
            .query_row(
                "SELECT pruned_modseq FROM bridge_imap_mailbox_state WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("read prune floor");
        assert_eq!(floor, 50, "the manifest's prune floor rides the restore");

        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count messages");
        assert_eq!(msg_count, 1);

        let exp_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_expunged WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count expunged");
        assert_eq!(exp_count, 1);

        let sub_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_subscriptions WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count subs");
        assert_eq!(sub_count, 1);
    }

    #[tokio::test]
    async fn empty_manifest_writes_zero_rows() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x71u8; 32];
        let manifest = MailPlacementManifest::new();

        // write phase — guard dropped at end of block
        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            replay_mail_manifest_into_sqlite(&tx, &actor, &manifest).expect("replay");
            tx.commit().expect("commit");
        }

        // read phase — single guard, no re-entrancy
        let conn = db.conn().await;
        let total: i64 = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM bridge_imap_mailbox_state WHERE actor_id = ?1)
                  + (SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?1)
                  + (SELECT COUNT(*) FROM bridge_imap_expunged WHERE actor_id = ?1)
                  + (SELECT COUNT(*) FROM bridge_imap_subscriptions WHERE actor_id = ?1)",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("aggregate count");
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn round_trips_specific_field_values() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x70u8; 32];
        let manifest = populated_manifest();

        // write phase — guard dropped at end of block
        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            replay_mail_manifest_into_sqlite(&tx, &actor, &manifest).expect("replay");
            tx.commit().expect("commit");
        }

        // read phase — single guard for all assertions
        let conn = db.conn().await;

        // bridge_imap_mailbox_state
        let (mailbox_name, uid_validity, uid_next, highestmodseq): (String, i64, i64, i64) = conn
            .query_row(
                "SELECT mailbox, uid_validity, uid_next, highestmodseq
                 FROM bridge_imap_mailbox_state WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("mailbox_state row");
        assert_eq!(mailbox_name, "INBOX");
        assert_eq!(uid_validity, 1_700_000_000_i64);
        assert_eq!(uid_next, 42_i64);
        assert_eq!(highestmodseq, 100_i64);

        // bridge_imap_messages
        // Flags convention: space-separated tokens (same as apply_append / COPY paths).
        // Vec["\\Seen"].join(" ") = "\\Seen"
        let (msg_mailbox, uid, modseq, message_id_blob, flags_str): (
            String,
            i64,
            i64,
            Vec<u8>,
            String,
        ) = conn
            .query_row(
                "SELECT mailbox, uid, modseq, message_id, flags
                 FROM bridge_imap_messages WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .expect("messages row");
        assert_eq!(msg_mailbox, "INBOX");
        assert_eq!(uid, 1_i64);
        assert_eq!(modseq, 2_i64);
        assert_eq!(message_id_blob, vec![0x11u8; 32]);
        // Space-separated flag tokens — must match how apply-path reads them back.
        assert_eq!(flags_str, "\\Seen");

        // bridge_imap_expunged
        let (exp_mailbox, exp_uid, exp_modseq, exp_at): (String, i64, i64, i64) = conn
            .query_row(
                "SELECT mailbox, uid, modseq, expunged_at
                 FROM bridge_imap_expunged WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("expunged row");
        assert_eq!(exp_mailbox, "INBOX");
        assert_eq!(exp_uid, 99_i64);
        assert_eq!(exp_modseq, 99_i64);
        assert_eq!(
            exp_at, 1_752_000_000,
            "restored from the tombstone's deleted_at"
        );

        // bridge_imap_subscriptions
        let sub_mailbox: String = conn
            .query_row(
                "SELECT mailbox FROM bridge_imap_subscriptions WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("subscriptions row");
        assert_eq!(sub_mailbox, "INBOX");
    }
}
