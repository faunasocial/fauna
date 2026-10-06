//! **Destination-side** writer seat store (nest-side segment backup slice 3;
//! `federation.md` § Nest-writer backup plane, `segment-backup-protocol.md`
//! § Cross-location backup protocol → *The writer seat*).
//!
//! **One row per owner** in `backup_writer_grants` (table defined in
//! `migrations::MIGRATIONS_BACKUP_WRITER_GRANTS`): the writer seat. The owner's
//! client, over its **own** authed connection to this (the destination) nest,
//! seats the one **source nest** that may write that owner's segment-backup
//! custody here.
//!
//! **The row is the authorization; the nest signature is only attribution.** A
//! federated backup write arrives with a verified `origin_nest_id` — but a nest
//! id is self-minted and free, so the peer proving "I am nest X" proves nothing
//! about whether X may write. The gate is state *this* nest wrote at the owner's
//! direction, the same shape as the `channel.fetch` foreign-member gate and as
//! `folder_member_access` for cross-nest folder writers.
//!
//! **One seat, because custody carries no source in its name.** An owner's
//! reserved-kind custody copy rests under `(owner, kind)`, so two source boxes
//! writing it would supersede each other's live segments from two unrelated
//! numberings. [`CacheDb::register_backup_writer`] is the one writer of the
//! row, and its arms are the whole rule:
//!
//! - no seat → the writer takes it, and the seat's clock starts;
//! - the holder → a refresh, which re-grants a revoked seat;
//! - another writer while the owner's custody here holds **no** live path →
//!   the seat is replaced and its clock restarts;
//! - another writer while any live path exists → refused, naming the holder;
//! - `succeeds` naming the holder → the seat moves in one transaction: the
//!   writer becomes the holder with the grant in force, the clock is kept, and
//!   the holder's covered-folder mirror sets are renamed under the writer.
//!   Naming anyone else is refused and changes nothing.
//!
//! Revoke = mark the row (idempotent); the seat stays held. That refuses the
//! next `fauna.federation.backup.write_token.mint`; an already-minted token
//! stays valid for its one remaining TTL, the accepted contract restated from
//! the folder write plane. Because revocation is driven from the owner's own
//! client → destination connection, it works with the **source nest fully
//! hostile** — which is the whole freeze-the-backup affordance.
//!
//! The store only: the `fauna.backup.writer_grant.{register,revoke,list}`
//! USER-class handlers (`backup_handlers.rs`) and the two federation kinds'
//! gate (`federation_handlers.rs`) live above this layer.

use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;

use super::{CacheDb, blob_to_array, now_epoch_secs};

/// An owner's writer seat, as the owner's client lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupWriterGrant {
    /// The source nest holding the seat.
    pub writer_nest_id: Vec<u8>,
    /// Unix seconds the grant was (most recently) registered.
    pub granted_at: i64,
    /// The holder's grant is revoked: it holds the seat and may not write.
    pub revoked: bool,
    /// Unix seconds the seat was taken — the seat's clock. A retained
    /// generation superseded before it is a previous writer's numbering.
    pub seated_at: i64,
}

/// What [`CacheDb::register_backup_writer`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterSeatOutcome {
    /// The named writer holds the seat with its grant in force.
    Seated,
    /// Refused, nothing changed: another nest holds the seat (`holder`), or a
    /// `succeeds` named a predecessor where the owner has no seat at all
    /// (`None`).
    Held { holder: Option<[u8; 32]> },
}

impl CacheDb {
    /// Register `writer_nest_id` for the owner's writer seat — the module doc's
    /// arms, decided and written in one transaction.
    ///
    /// Idempotent for the holder: a re-register refreshes `granted_at` (and
    /// clears a revoke), so the client retries enroll freely, and repeating a
    /// completed `succeeds` handover lands on that same refresh
    /// (`nest/common.md` § Client-state recoverability).
    pub async fn register_backup_writer(
        &self,
        owner_actor_id: &[u8],
        writer_nest_id: &[u8; 32],
        succeeds: Option<&[u8; 32]>,
    ) -> Result<WriterSeatOutcome> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin backup writer registration")?;
        let seat: Option<Vec<u8>> = tx
            .query_row(
                "SELECT writer_nest_id FROM backup_writer_grants WHERE owner_actor_id = ?1",
                rusqlite::params![owner_actor_id],
                |r| r.get(0),
            )
            .optional()
            .context("read the backup writer seat")?;
        let holder: Option<[u8; 32]> = seat
            .map(|h| blob_to_array(h.as_slice(), "writer_nest_id"))
            .transpose()?;

        let refresh = "UPDATE backup_writer_grants
                       SET writer_nest_id = ?2, granted_at = ?3, revoked = 0
                       WHERE owner_actor_id = ?1";
        match (holder, succeeds) {
            // The holder registering again, whatever it says it succeeds: a
            // refresh, which is also what a repeated handover is.
            (Some(holder), _) if holder == *writer_nest_id => {
                tx.execute(
                    refresh,
                    rusqlite::params![owner_actor_id, writer_nest_id.as_slice(), now],
                )
                .context("refresh the backup writer seat")?;
            }
            // The handover: the same UPDATE, so the seat's clock is kept, plus
            // the predecessor's mirror sets.
            (Some(holder), Some(predecessor)) if holder == *predecessor => {
                tx.execute(
                    refresh,
                    rusqlite::params![owner_actor_id, writer_nest_id.as_slice(), now],
                )
                .context("move the backup writer seat")?;
                rename_mirror_sets(&tx, owner_actor_id, &holder, writer_nest_id)?;
            }
            // A `succeeds` that does not name the holder.
            (holder, Some(_)) => return Ok(WriterSeatOutcome::Held { holder }),
            (Some(holder), None) => {
                if owner_holds_live_custody(&tx, owner_actor_id)? {
                    return Ok(WriterSeatOutcome::Held {
                        holder: Some(holder),
                    });
                }
                tx.execute(
                    "UPDATE backup_writer_grants
                     SET writer_nest_id = ?2, granted_at = ?3, revoked = 0, seated_at = ?3
                     WHERE owner_actor_id = ?1",
                    rusqlite::params![owner_actor_id, writer_nest_id.as_slice(), now],
                )
                .context("replace the backup writer seat")?;
            }
            (None, None) => {
                tx.execute(
                    "INSERT INTO backup_writer_grants
                         (owner_actor_id, writer_nest_id, granted_at, revoked, seated_at)
                     VALUES (?1, ?2, ?3, 0, ?3)",
                    rusqlite::params![owner_actor_id, writer_nest_id.as_slice(), now],
                )
                .context("take the backup writer seat")?;
            }
        }
        tx.commit().context("commit backup writer registration")?;
        Ok(WriterSeatOutcome::Seated)
    }

    /// The gate the two federation backup kinds run: does `writer_nest_id` hold
    /// `owner_actor_id`'s writer seat here with its grant in force?
    ///
    /// Fails **closed** — a storage error propagates as `Err` and the caller
    /// refuses the write; it never degenerates to "allow".
    pub async fn has_backup_writer_grant(
        &self,
        owner_actor_id: &[u8],
        writer_nest_id: &[u8],
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM backup_writer_grants
                 WHERE owner_actor_id = ?1 AND writer_nest_id = ?2 AND revoked = 0",
                rusqlite::params![owner_actor_id, writer_nest_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow!("has backup writer grant: {e}"))?;
        Ok(count > 0)
    }

    /// Revoke the named writer's grant: mark the seat revoked. The row stays —
    /// the seat is still held, so no other box may take it over live custody.
    /// Returns whether a grant in force was revoked — idempotent, so a double
    /// revoke (or one naming a nest that is not the holder) is success, not an
    /// error.
    pub async fn revoke_backup_writer_grant(
        &self,
        owner_actor_id: &[u8],
        writer_nest_id: &[u8],
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE backup_writer_grants SET revoked = 1
                 WHERE owner_actor_id = ?1 AND writer_nest_id = ?2 AND revoked = 0",
                rusqlite::params![owner_actor_id, writer_nest_id],
            )
            .map_err(|e| anyhow!("revoke backup writer grant: {e}"))?;
        Ok(n > 0)
    }

    /// This owner's writer seat here, revoked or not — at most one row.
    /// **Owner-scoped** — a caller only ever sees its own seat, the same
    /// conservative direction `fauna.admin.membership_tiers.list` took
    /// (widening later is additive; narrowing later is a within-major break).
    pub async fn list_backup_writer_grants(
        &self,
        owner_actor_id: &[u8],
    ) -> Result<Vec<BackupWriterGrant>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT writer_nest_id, granted_at, revoked, seated_at FROM backup_writer_grants
             WHERE owner_actor_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![owner_actor_id], |row| {
            Ok(BackupWriterGrant {
                writer_nest_id: row.get(0)?,
                granted_at: row.get(1)?,
                revoked: row.get::<_, i64>(2)? != 0,
                seated_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Does any custody-copy set of this owner hold a live `backup_custody` path?
/// The test that tells a seat another writer may take (never written, or torn
/// down by a removal) from one it would overwrite.
fn owner_holds_live_custody(conn: &rusqlite::Connection, owner_actor_id: &[u8]) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM backup_custody c
             INNER JOIN folders f ON f.id = c.folder_id
             WHERE f.actor_id = ?1 AND f.custody_copy = 1 AND c.manifest_hash IS NOT NULL)",
        rusqlite::params![owner_actor_id],
        |r| r.get(0),
    )
    .context("look for the owner's live backup custody")
}

/// Rename every covered-folder mirror set of this owner named under `from`
/// (`__folder/<from-hex>/<folder-id>`) under `to` — the handover's second half.
/// Custody rows key on the set's row id, so no row and no byte moves; the
/// reserved-kind sets carry no source in their names and need nothing.
///
/// A set already resting under the successor's name is left where it is and
/// the predecessor's keeps its own: reaching that takes a seat that went from
/// the successor to the predecessor and back, and merging two sets' custody
/// rows is not a rename.
fn rename_mirror_sets(
    conn: &rusqlite::Connection,
    owner_actor_id: &[u8],
    from: &[u8; 32],
    to: &[u8; 32],
) -> Result<()> {
    let prefix = format!("__folder/{}/", hex::encode(from));
    let sets: Vec<(i64, String)> = conn
        .prepare(
            "SELECT id, name FROM folders
             WHERE actor_id = ?1 AND custody_copy = 1 AND substr(name, 1, length(?2)) = ?2",
        )
        .and_then(|mut s| {
            s.query_map(rusqlite::params![owner_actor_id, prefix], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()
        })
        .context("list the predecessor's mirror sets")?;
    for (id, name) in sets {
        let Some((_, folder_id)) = fauna_core::data::parse_folder_backup_set_name(&name) else {
            continue;
        };
        let renamed = crate::db::sync_storage::folder_backup_set_name(to, folder_id);
        let occupied: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM folders WHERE name = ?1 AND actor_id = ?2)",
                rusqlite::params![renamed, owner_actor_id],
                |r| r.get(0),
            )
            .context("look for a set under the successor's name")?;
        if occupied {
            tracing::warn!(
                set = %name,
                "writer seat handover: a mirror set already rests under the successor's name; \
                 the predecessor's set keeps its own"
            );
            continue;
        }
        conn.execute(
            "UPDATE folders SET name = ?1, name_hash = ?2 WHERE id = ?3",
            rusqlite::params![
                renamed,
                fauna_core::path_crypto::set_name_hash(&renamed).to_vec(),
                id
            ],
        )
        .context("rename a mirror set under the successor")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER_A: [u8; 32] = [0xAA; 32];
    const OWNER_B: [u8; 32] = [0xBB; 32];
    const WRITER_1: [u8; 32] = [0x11; 32];
    const WRITER_2: [u8; 32] = [0x22; 32];
    const WRITER_3: [u8; 32] = [0x33; 32];

    async fn register(db: &CacheDb, owner: &[u8; 32], writer: &[u8; 32]) -> WriterSeatOutcome {
        db.register_backup_writer(owner, writer, None)
            .await
            .unwrap()
    }

    async fn seat(db: &CacheDb, owner: &[u8; 32]) -> BackupWriterGrant {
        let mut seats = db.list_backup_writer_grants(owner).await.unwrap();
        assert_eq!(seats.len(), 1, "an owner has one seat");
        seats.remove(0)
    }

    /// A custody-copy set of `owner` named `name`, holding one live path when
    /// `live` and one tombstone otherwise.
    async fn custody_set(db: &CacheDb, owner: &[u8; 32], name: &str, live: bool) -> i64 {
        let id = db
            .create_folder_with_options(
                name,
                owner,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO backup_custody
                 (uploader_actor, folder_id, path_hash, manifest_hash, size_bytes, updated_at)
             VALUES (?1, ?2, x'01', ?3, 0, 0)",
            rusqlite::params![owner.as_slice(), id, live.then_some(vec![0x4Du8; 32])],
        )
        .unwrap();
        id
    }

    async fn set_seated_at(db: &CacheDb, owner: &[u8; 32], at: i64) {
        db.conn
            .lock()
            .await
            .execute(
                "UPDATE backup_writer_grants SET seated_at = ?1 WHERE owner_actor_id = ?2",
                rusqlite::params![at, owner.as_slice()],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn register_gate_revoke_round_trip() {
        let db = CacheDb::open_in_memory().unwrap();

        // Unseated ⇒ the gate refuses.
        assert!(
            !db.has_backup_writer_grant(&OWNER_A, &WRITER_1)
                .await
                .unwrap()
        );

        assert_eq!(
            register(&db, &OWNER_A, &WRITER_1).await,
            WriterSeatOutcome::Seated
        );
        assert!(
            db.has_backup_writer_grant(&OWNER_A, &WRITER_1)
                .await
                .unwrap()
        );

        // Revoke ⇒ the gate refuses again, and a second revoke is a no-op.
        assert!(
            db.revoke_backup_writer_grant(&OWNER_A, &WRITER_1)
                .await
                .unwrap()
        );
        assert!(
            !db.revoke_backup_writer_grant(&OWNER_A, &WRITER_1)
                .await
                .unwrap()
        );
        assert!(
            !db.has_backup_writer_grant(&OWNER_A, &WRITER_1)
                .await
                .unwrap()
        );
        // The seat is still held, marked revoked.
        let held = seat(&db, &OWNER_A).await;
        assert_eq!(held.writer_nest_id, WRITER_1.to_vec());
        assert!(held.revoked);

        // The holder registering again is re-granted.
        assert_eq!(
            register(&db, &OWNER_A, &WRITER_1).await,
            WriterSeatOutcome::Seated
        );
        assert!(
            db.has_backup_writer_grant(&OWNER_A, &WRITER_1)
                .await
                .unwrap()
        );
        assert!(!seat(&db, &OWNER_A).await.revoked);
    }

    #[tokio::test]
    async fn a_seat_authorizes_only_its_holder_for_its_owner() {
        // The load-bearing property: the gate is per-(owner, holder). A seat
        // must not leak across either axis — neither another owner's custody to
        // this writer, nor this owner's custody to another nest.
        let db = CacheDb::open_in_memory().unwrap();
        register(&db, &OWNER_A, &WRITER_1).await;

        assert!(
            db.has_backup_writer_grant(&OWNER_A, &WRITER_1)
                .await
                .unwrap()
        );
        // Same owner, different source nest ⇒ refused.
        assert!(
            !db.has_backup_writer_grant(&OWNER_A, &WRITER_2)
                .await
                .unwrap()
        );
        // Same source nest, different owner ⇒ refused.
        assert!(
            !db.has_backup_writer_grant(&OWNER_B, &WRITER_1)
                .await
                .unwrap()
        );
        // Revoking a nest that is not the holder touches nothing.
        assert!(
            !db.revoke_backup_writer_grant(&OWNER_A, &WRITER_2)
                .await
                .unwrap()
        );
        assert!(
            db.has_backup_writer_grant(&OWNER_A, &WRITER_1)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn another_writer_takes_a_seat_only_over_custody_with_no_live_path() {
        let db = CacheDb::open_in_memory().unwrap();
        register(&db, &OWNER_A, &WRITER_1).await;
        set_seated_at(&db, &OWNER_A, 5).await;

        // No custody at all: the seat is replaced and its clock restarts.
        assert_eq!(
            register(&db, &OWNER_A, &WRITER_2).await,
            WriterSeatOutcome::Seated
        );
        let held = seat(&db, &OWNER_A).await;
        assert_eq!(held.writer_nest_id, WRITER_2.to_vec());
        assert!(held.seated_at > 5, "the seat's clock restarts");
        assert!(
            !db.has_backup_writer_grant(&OWNER_A, &WRITER_1)
                .await
                .unwrap()
        );

        // A torn-down set (tombstones only) is still no live path — and
        // another owner's live custody is not this owner's.
        custody_set(&db, &OWNER_A, "__mail", false).await;
        custody_set(&db, &OWNER_B, "__mail", true).await;
        assert_eq!(
            register(&db, &OWNER_A, &WRITER_3).await,
            WriterSeatOutcome::Seated
        );

        // One live path: the newcomer is refused, naming the holder, and a
        // revoke of the holder changes nothing about that.
        custody_set(&db, &OWNER_A, "__cal", true).await;
        let refused = WriterSeatOutcome::Held {
            holder: Some(WRITER_3),
        };
        assert_eq!(register(&db, &OWNER_A, &WRITER_1).await, refused);
        assert!(
            db.revoke_backup_writer_grant(&OWNER_A, &WRITER_3)
                .await
                .unwrap()
        );
        assert_eq!(register(&db, &OWNER_A, &WRITER_1).await, refused);
        let held = seat(&db, &OWNER_A).await;
        assert_eq!(held.writer_nest_id, WRITER_3.to_vec());
        assert!(held.revoked, "a refused registration changes nothing");
    }

    #[tokio::test]
    async fn succeeds_moves_the_seat_and_the_predecessors_mirror_sets() {
        let db = CacheDb::open_in_memory().unwrap();
        register(&db, &OWNER_A, &WRITER_1).await;
        set_seated_at(&db, &OWNER_A, 5).await;
        let old_name = crate::db::sync_storage::folder_backup_set_name(&WRITER_1, 7);
        let new_name = crate::db::sync_storage::folder_backup_set_name(&WRITER_2, 7);
        let mirror = custody_set(&db, &OWNER_A, &old_name, true).await;
        let mail = custody_set(&db, &OWNER_A, "__mail", true).await;
        // Another owner's set under the same predecessor is not this owner's.
        custody_set(&db, &OWNER_B, &old_name, true).await;

        // Naming anyone but the holder is refused and changes nothing.
        assert_eq!(
            db.register_backup_writer(&OWNER_A, &WRITER_2, Some(&WRITER_3))
                .await
                .unwrap(),
            WriterSeatOutcome::Held {
                holder: Some(WRITER_1)
            }
        );
        assert_eq!(seat(&db, &OWNER_A).await.writer_nest_id, WRITER_1.to_vec());
        // So is naming a predecessor where the owner has no seat.
        assert_eq!(
            db.register_backup_writer(&OWNER_B, &WRITER_2, Some(&WRITER_1))
                .await
                .unwrap(),
            WriterSeatOutcome::Held { holder: None }
        );
        assert!(
            db.list_backup_writer_grants(&OWNER_B)
                .await
                .unwrap()
                .is_empty()
        );

        // Naming the holder moves a revoked seat too: the successor's grant is
        // in force, the clock is kept, and the mirror set is renamed in place.
        db.revoke_backup_writer_grant(&OWNER_A, &WRITER_1)
            .await
            .unwrap();
        assert_eq!(
            db.register_backup_writer(&OWNER_A, &WRITER_2, Some(&WRITER_1))
                .await
                .unwrap(),
            WriterSeatOutcome::Seated
        );
        let held = seat(&db, &OWNER_A).await;
        assert_eq!(held.writer_nest_id, WRITER_2.to_vec());
        assert!(!held.revoked);
        assert_eq!(held.seated_at, 5, "a handover keeps the seat's clock");
        assert!(
            db.get_folder_for_actor(&old_name, &OWNER_A)
                .await
                .unwrap()
                .is_none()
        );
        let renamed = db
            .get_folder_for_actor(&new_name, &OWNER_A)
            .await
            .unwrap()
            .expect("the mirror set rests under the successor's name");
        assert_eq!(renamed.id, mirror, "the same row, so no custody row moved");
        assert!(
            db.get_folder_for_actor_by_name_hash(
                &fauna_core::path_crypto::set_name_hash(&new_name),
                &OWNER_A
            )
            .await
            .unwrap()
            .is_some(),
            "the hash companion follows the name"
        );
        assert_eq!(
            db.get_folder_for_actor("__mail", &OWNER_A)
                .await
                .unwrap()
                .unwrap()
                .id,
            mail,
            "a reserved-kind set carries no source in its name"
        );
        assert!(
            db.get_folder_for_actor(&old_name, &OWNER_B)
                .await
                .unwrap()
                .is_some(),
            "another owner's set is untouched"
        );

        // Repeating the completed handover is a plain refresh.
        assert_eq!(
            db.register_backup_writer(&OWNER_A, &WRITER_2, Some(&WRITER_1))
                .await
                .unwrap(),
            WriterSeatOutcome::Seated
        );
        assert_eq!(seat(&db, &OWNER_A).await.seated_at, 5);
    }

    #[tokio::test]
    async fn list_is_owner_scoped_and_reregister_refreshes() {
        let db = CacheDb::open_in_memory().unwrap();
        register(&db, &OWNER_A, &WRITER_1).await;
        register(&db, &OWNER_B, &WRITER_2).await;

        assert_eq!(
            seat(&db, &OWNER_A).await.writer_nest_id,
            WRITER_1.to_vec(),
            "owner A sees its own seat, never B's"
        );
        assert_eq!(seat(&db, &OWNER_B).await.writer_nest_id, WRITER_2.to_vec());

        // A re-register refreshes rather than duplicating, and keeps the clock.
        set_seated_at(&db, &OWNER_B, 5).await;
        register(&db, &OWNER_B, &WRITER_2).await;
        assert_eq!(seat(&db, &OWNER_B).await.seated_at, 5);
    }
}
