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

    // No mailbox leaves the restore with its UID counter behind its own
    // placements. A journal whose mailboxes were seeded without their `Create`
    // records folds to placements with no (or a stale) mailbox state; written
    // verbatim, the next arrival is allocated a UID a restored row already
    // holds, its placement fails on the primary key and rolls back, and every
    // later arrival does the same — the account's inbound mail is put off for
    // good. A missing row takes the seeded `uid_validity = 1`, the value the
    // database's own seeding gives a mailbox it creates.
    tx.execute(
        "INSERT OR IGNORE INTO bridge_imap_mailbox_state
            (actor_id, mailbox, uid_validity, uid_next, highestmodseq)
         SELECT actor_id, mailbox, 1, MAX(uid) + 1, MAX(modseq)
         FROM bridge_imap_messages WHERE actor_id = ?1 GROUP BY mailbox",
        rusqlite::params![actor.as_slice()],
    )
    .context("seed a placed mailbox's missing state")?;
    tx.execute(
        "UPDATE bridge_imap_mailbox_state AS s
         SET uid_next = MAX(s.uid_next, p.max_uid + 1),
             highestmodseq = MAX(s.highestmodseq, p.max_modseq)
         FROM (SELECT mailbox, MAX(uid) AS max_uid, MAX(modseq) AS max_modseq
               FROM bridge_imap_messages WHERE actor_id = ?1 GROUP BY mailbox) AS p
         WHERE s.actor_id = ?1 AND s.mailbox = p.mailbox",
        rusqlite::params![actor.as_slice()],
    )
    .context("put a mailbox's counters past its placements")?;

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

    /// Read `(uid_next, highestmodseq)` of `(actor, mailbox)`, `None` when
    /// the restore wrote no state row for it.
    async fn state_of(db: &CacheDb, actor: &[u8; 32], mailbox: &str) -> Option<(i64, i64)> {
        use rusqlite::OptionalExtension;
        db.conn()
            .await
            .query_row(
                "SELECT uid_next, highestmodseq FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![actor.as_slice(), mailbox],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .expect("read mailbox state")
    }

    fn inbox_placement(uid: u32, modseq: u64, seed: u8) -> RecordPlacement {
        RecordPlacement {
            mailbox: "INBOX".to_string(),
            uid,
            modseq,
            flags: vec!["\\Seen".to_string()],
            content_record_id: vec![seed; 32],
            internal_date: 1_700_000_500,
        }
    }

    /// **A restore never leaves a mailbox's UID counter behind its own
    /// placements.** A journal whose mailboxes were seeded without a `Create`
    /// record (a read path bootstrapping them) folds to a manifest carrying
    /// INBOX placements but no INBOX state. Restored verbatim, the next
    /// arrival seeded INBOX at `uid_next = 1`, collided with the restored
    /// UID 1 and rolled back — every inbound mail to that account was put
    /// off with a 451 for good. The same holds for a state row the manifest
    /// does carry but behind its placements.
    /// Mutation pinned: replaying the manifest's mailbox rows verbatim.
    #[tokio::test]
    async fn restore_puts_every_counter_past_the_mailboxs_own_placements() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x71u8; 32];
        let mut manifest = MailPlacementManifest::new();
        // INBOX: four placements, no state row in the manifest at all.
        for (uid, modseq) in [(1, 2), (2, 3), (3, 4), (4, 5)] {
            manifest
                .placements
                .push(inbox_placement(uid, modseq, uid as u8));
        }
        // Sent: a state row the manifest carries, but behind its placement.
        manifest.mailboxes.push(MailboxState {
            name: "Sent".to_string(),
            uid_validity: 1,
            uid_next: 1,
            highestmodseq: 1,
            attrs: vec![],
            pruned_modseq: 0,
        });
        manifest.placements.push(RecordPlacement {
            mailbox: "Sent".to_string(),
            ..inbox_placement(3, 9, 0x33)
        });

        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            replay_mail_manifest_into_sqlite(&tx, &actor, &manifest).expect("replay");
            tx.commit().expect("commit");
        }

        assert_eq!(state_of(&db, &actor, "INBOX").await, Some((5, 5)));
        assert_eq!(state_of(&db, &actor, "Sent").await, Some((4, 9)));

        // The arrival that used to fail for good now places at the next UID.
        db.ensure_bridge_imap_mailboxes(&actor).await.expect("seed");
        let placed = db
            .place_inbound_mail(
                &actor,
                &[0xA5u8; 32],
                "INBOX",
                1_700_000_900,
                "",
                "external.test",
                true,
            )
            .await
            .expect("an arrival after a restore places");
        assert_eq!(placed.map(|(uid, _)| uid), Some(5));
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
