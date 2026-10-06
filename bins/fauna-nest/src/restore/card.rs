//! Replay a CardPlacementManifest's collection + tombstone halves into the
//! actor's bridge_carddav_* tables inside an open SQLite transaction.
//! Structural twin of [`super::cal`] — see it for the division of labor
//! (the card ROWS are rebuilt by `filesync_handlers.rs::restore_card`,
//! joining placement entries against on-disk floors/envelopes).

use anyhow::{Context, Result};
use fauna_contacts::segments::placement::CardPlacementManifest;
use rusqlite::Transaction;

/// Replay the compacted manifest state into the actor's
/// bridge_carddav_addressbooks table. Writes one row per address book.
/// Caller owns the transaction + the pre-replay DELETE.
pub fn replay_card_manifest_into_sqlite(
    tx: &Transaction<'_>,
    actor: &[u8; 32],
    manifest: &CardPlacementManifest,
) -> Result<()> {
    // ctag mirrors highestmodseq — the live write path keeps the two in
    // lockstep; created_at synthesized as 0 (twin of the calendar replay).
    for b in &manifest.addressbooks {
        tx.execute(
            "INSERT INTO bridge_carddav_addressbooks
                (actor_id, addressbook_id, encrypted_metadata, ctag, highestmodseq, created_at)
             VALUES (?1, ?2, ?3, ?4, ?4, 0)",
            rusqlite::params![
                actor.as_slice(),
                b.addressbook_id.as_slice(),
                &b.encrypted_metadata,
                b.highestmodseq as i64,
            ],
        )
        .context("INSERT bridge_carddav_addressbooks")?;
    }
    Ok(())
}

/// Replay the manifest's tombstones into bridge_carddav_expunged. Every
/// tombstone carries the row's `card_id` and delete time — see the calendar
/// twin.
pub fn replay_card_tombstones_into_sqlite(
    tx: &Transaction<'_>,
    actor: &[u8; 32],
    manifest: &CardPlacementManifest,
) -> Result<()> {
    for t in &manifest.tombstones {
        tx.execute(
            "INSERT INTO bridge_carddav_expunged
                (actor_id, addressbook_id, card_id, uid_hash, modseq, expunged_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                actor.as_slice(),
                t.addressbook_id.as_slice(),
                t.card_id.as_slice(),
                t.uid_hash.as_slice(),
                t.modseq as i64,
                t.deleted_at,
            ],
        )
        .context("INSERT bridge_carddav_expunged")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use fauna_contacts::segments::placement::{AddressbookState, CardTombstoneRef};

    #[tokio::test]
    async fn replays_addressbooks_and_tombstones() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        let actor = [0x74u8; 32];
        let mut manifest = CardPlacementManifest::new();
        manifest.addressbooks.push(AddressbookState {
            addressbook_id: [0xAAu8; 32],
            encrypted_metadata: vec![0u8; 48],
            highestmodseq: 50,
        });
        manifest.tombstones.push(CardTombstoneRef {
            addressbook_id: [0xAAu8; 32],
            uid_hash: [0xCCu8; 32],
            modseq: 10,
            card_id: [0xDDu8; 32],
            deleted_at: 1_752_000_000,
        });

        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().expect("tx");
            replay_card_manifest_into_sqlite(&tx, &actor, &manifest).expect("books");
            replay_card_tombstones_into_sqlite(&tx, &actor, &manifest).expect("tombstones");
            tx.commit().expect("commit");
        }

        let conn = db.conn().await;
        let (ctag, hms): (i64, i64) = conn
            .query_row(
                "SELECT ctag, highestmodseq FROM bridge_carddav_addressbooks
                 WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("book row");
        assert_eq!(hms, 50);
        assert_eq!(ctag, 50, "ctag mirrors highestmodseq across a restore");

        let (count, card_id, at): (i64, Vec<u8>, i64) = conn
            .query_row(
                "SELECT COUNT(*), card_id, expunged_at
                 FROM bridge_carddav_expunged WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("expunged row");
        assert_eq!(count, 1, "the tombstone restored");
        assert_eq!(card_id, vec![0xDDu8; 32]);
        assert_eq!(at, 1_752_000_000);
    }
}
