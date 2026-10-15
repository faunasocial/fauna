//! A folder's **inbox segment** — third-party deposits parked sealed until a
//! seat adopts them.
//!
//! Owner: `docs/goal/behavior/folder-deposit.md`. The
//! table's own comment (`migrations.rs`, `MIGRATIONS_FOLDER_DEPOSIT_INBOX`)
//! argues its shape. This module is storage only: the door that seals and
//! the gate in front of it are `crate::folder_deposit`.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::CacheDb;

/// One parked deposit, as the inbox holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderDepositRow {
    pub id: i64,
    pub folder_id: i64,
    pub principal_id: Vec<u8>,
    /// The recipient-sealed `fauna_protocol::folders::DepositEnvelope`.
    pub sealed: Vec<u8>,
    pub received_at: i64,
}

/// What a commit of a staged upload found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepositCommit {
    /// The item is now parked and adoptable.
    Committed,
    /// The upload was committed already — a retried commit, answered alike.
    AlreadyCommitted,
    /// The staged parts are not exactly `0..parts`; nothing changed.
    PartsMismatch,
    /// No staged upload by this token (never begun, or expired).
    Missing,
}

/// The sealed bytes the inbox holds for `actor`'s folders — every item's
/// header and every part, staged uploads included — summed on `conn`.
fn parked_bytes(conn: &rusqlite::Connection, actor: &[u8]) -> Result<u64> {
    let total: i64 = conn
        .query_row(
            "SELECT
                 COALESCE((SELECT SUM(i.byte_length) FROM folder_deposit_inbox i
                            JOIN folders f ON f.id = i.folder_id
                           WHERE f.actor_id = ?1), 0)
               + COALESCE((SELECT SUM(p.byte_length) FROM folder_deposit_parts p
                            JOIN folder_deposit_inbox i ON i.id = p.deposit_id
                            JOIN folders f ON f.id = i.folder_id
                           WHERE f.actor_id = ?1), 0)",
            [actor],
            |r| r.get(0),
        )
        .context("sum parked deposit bytes")?;
    Ok(u64::try_from(total).unwrap_or(0))
}

/// Would `adding` more sealed bytes keep `actor`'s inbox within `limit`? An
/// unrepresentable sum is over (`file-sync.md`'s `checked_add` rule).
fn within_limit(
    conn: &rusqlite::Connection,
    actor: &[u8],
    adding: usize,
    limit: u64,
) -> Result<bool> {
    let used = parked_bytes(conn, actor)?;
    Ok(u64::try_from(adding)
        .ok()
        .and_then(|a| used.checked_add(a))
        .is_some_and(|total| total <= limit))
}

/// The account that owns `folder_id`.
fn folder_owner(conn: &rusqlite::Connection, folder_id: i64) -> Result<Vec<u8>> {
    conn.query_row(
        "SELECT actor_id FROM folders WHERE id = ?1",
        [folder_id],
        |r| r.get(0),
    )
    .context("read the deposit folder's owner")
}

impl CacheDb {
    /// Park one sealed single-message deposit in `folder_id`'s inbox segment
    /// when the owner's parked bytes stay within `limit`: the item's id, or
    /// `None` when the inbox is full. The check and the insert hold the one
    /// connection, so two deposits cannot both squeeze under the limit.
    pub async fn insert_folder_deposit(
        &self,
        folder_id: i64,
        principal_id: &[u8],
        sealed: &[u8],
        received_at: i64,
        limit: u64,
    ) -> Result<Option<i64>> {
        let conn = self.conn.lock().await;
        if !within_limit(&conn, &folder_owner(&conn, folder_id)?, sealed.len(), limit)? {
            return Ok(None);
        }
        conn.execute(
            "INSERT INTO folder_deposit_inbox
                 (folder_id, principal_id, sealed, byte_length, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                folder_id,
                principal_id,
                sealed,
                i64::try_from(sealed.len()).context("deposit length")?,
                received_at,
            ],
        )
        .context("insert folder deposit")?;
        Ok(Some(conn.last_insert_rowid()))
    }

    /// Open a staged upload for a deposit larger than one message: an inbox
    /// row no list shows until [`Self::commit_folder_deposit_upload`]. Returns
    /// its id.
    pub async fn begin_folder_deposit_upload(
        &self,
        folder_id: i64,
        principal_id: &[u8],
        token: &[u8],
        now: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO folder_deposit_inbox
                 (folder_id, principal_id, sealed, byte_length, received_at,
                  committed, upload_token)
             VALUES (?1, ?2, x'', 0, ?3, 0, ?4)",
            rusqlite::params![folder_id, principal_id, now, token],
        )
        .context("begin folder deposit upload")?;
        Ok(conn.last_insert_rowid())
    }

    /// The upload `token` that `principal_id` began in `folder_id`: its row id
    /// and whether it is committed. A token another principal or another
    /// folder holds answers `None`, as one never begun does.
    pub async fn find_folder_deposit_upload(
        &self,
        folder_id: i64,
        principal_id: &[u8],
        token: &[u8],
    ) -> Result<Option<(i64, bool)>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, committed FROM folder_deposit_inbox
              WHERE folder_id = ?1 AND principal_id = ?2 AND upload_token = ?3",
            rusqlite::params![folder_id, principal_id, token],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .optional()
        .context("find folder deposit upload")
    }

    /// Stage sealed part `idx` of upload `deposit_id` — replacing a part
    /// already staged under that index — when the owner's parked bytes stay
    /// within `limit`, and mark the upload active at `now`. `false` when the
    /// inbox is full (nothing staged) or the upload is no longer staged.
    pub async fn put_folder_deposit_part(
        &self,
        deposit_id: i64,
        idx: u32,
        sealed: &[u8],
        now: i64,
        limit: u64,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin deposit part transaction")?;
        let folder_id: Option<i64> = tx
            .query_row(
                "SELECT folder_id FROM folder_deposit_inbox WHERE id = ?1 AND committed = 0",
                [deposit_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(folder_id) = folder_id else {
            return Ok(false);
        };
        // A replaced part gives its bytes back before the new one is counted.
        tx.execute(
            "DELETE FROM folder_deposit_parts WHERE deposit_id = ?1 AND idx = ?2",
            rusqlite::params![deposit_id, idx],
        )?;
        if !within_limit(&tx, &folder_owner(&tx, folder_id)?, sealed.len(), limit)? {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO folder_deposit_parts (deposit_id, idx, sealed, byte_length)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                deposit_id,
                idx,
                sealed,
                i64::try_from(sealed.len()).context("part length")?,
            ],
        )?;
        tx.execute(
            "UPDATE folder_deposit_inbox SET received_at = ?2 WHERE id = ?1",
            rusqlite::params![deposit_id, now],
        )?;
        tx.commit().context("commit deposit part")?;
        Ok(true)
    }

    /// Commit upload `deposit_id` with its sealed header envelope when exactly
    /// the parts `0..parts` are staged: one statement flips it adoptable, so a
    /// crash leaves it staged or whole. The header's bytes count against no
    /// limit — it is a name and a count, and its parts were counted on entry.
    pub async fn commit_folder_deposit_upload(
        &self,
        deposit_id: i64,
        parts: u32,
        sealed_header: &[u8],
        now: i64,
    ) -> Result<DepositCommit> {
        let conn = self.conn.lock().await;
        let committed: Option<i64> = conn
            .query_row(
                "SELECT committed FROM folder_deposit_inbox WHERE id = ?1",
                [deposit_id],
                |r| r.get(0),
            )
            .optional()?;
        match committed {
            None => return Ok(DepositCommit::Missing),
            Some(c) if c != 0 => return Ok(DepositCommit::AlreadyCommitted),
            Some(_) => {}
        }
        let (count, max): (i64, Option<i64>) = conn.query_row(
            "SELECT COUNT(*), MAX(idx) FROM folder_deposit_parts WHERE deposit_id = ?1",
            [deposit_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // Indices are distinct (the primary key) and non-negative, so a count
        // of `parts` whose largest is `parts - 1` is exactly `0..parts`.
        if parts == 0 || count != i64::from(parts) || max != Some(i64::from(parts) - 1) {
            return Ok(DepositCommit::PartsMismatch);
        }
        conn.execute(
            "UPDATE folder_deposit_inbox
                SET sealed = ?2, byte_length = ?3, received_at = ?4, committed = 1
              WHERE id = ?1 AND committed = 0",
            rusqlite::params![
                deposit_id,
                sealed_header,
                i64::try_from(sealed_header.len()).context("header length")?,
                now,
            ],
        )
        .context("commit folder deposit upload")?;
        Ok(DepositCommit::Committed)
    }

    /// Drop staged upload `deposit_id` and its parts — a commit refused for
    /// its name. Never touches a committed item.
    pub async fn discard_folder_deposit_upload(&self, deposit_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        // I1 (b): a staged upload is not yet an accepted item — the
        // principal still holds the file and its deposit was never answered.
        conn.execute(
            "DELETE FROM folder_deposit_inbox WHERE id = ?1 AND committed = 0",
            [deposit_id],
        )
        .context("discard folder deposit upload")?;
        Ok(())
    }

    /// Drop every staged upload idle since before `cutoff`, with its parts;
    /// returns how many. A principal that vanished mid-upload gives its bytes
    /// back to the inbox this way.
    pub async fn expire_staged_folder_deposits(&self, cutoff: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        // I1 (b): staged, never committed — nothing the user was ever handed.
        conn.execute(
            "DELETE FROM folder_deposit_inbox WHERE committed = 0 AND received_at < ?1",
            [cutoff],
        )
        .context("expire staged folder deposits")
    }

    /// Sealed part `idx` of the committed item `id` in `folder_id`'s inbox.
    pub async fn get_folder_deposit_part(
        &self,
        folder_id: i64,
        id: i64,
        idx: u32,
    ) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT p.sealed FROM folder_deposit_parts p
               JOIN folder_deposit_inbox i ON i.id = p.deposit_id
              WHERE i.folder_id = ?1 AND i.id = ?2 AND i.committed = 1 AND p.idx = ?3",
            rusqlite::params![folder_id, id, idx],
            |r| r.get(0),
        )
        .optional()
        .context("get folder deposit part")
    }

    /// The tier's `max_inbox_bytes` for `actor` — `None` without a user or
    /// tier row.
    pub async fn get_user_tier_max_inbox_bytes(&self, actor: &[u8; 32]) -> Result<Option<i64>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT t.max_inbox_bytes FROM users u JOIN tiers t ON u.tier = t.name
              WHERE u.actor_id = ?1",
            [actor.as_slice()],
            |r| r.get(0),
        )
        .optional()
        .context("get user tier max_inbox_bytes")
    }

    /// Every item parked in `folder_id`'s inbox segment, oldest first.
    pub async fn list_folder_deposits(&self, folder_id: i64) -> Result<Vec<FolderDepositRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, folder_id, principal_id, sealed, received_at
               FROM folder_deposit_inbox WHERE folder_id = ?1 AND committed = 1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([folder_id], |r| {
                Ok(FolderDepositRow {
                    id: r.get(0)?,
                    folder_id: r.get(1)?,
                    principal_id: r.get(2)?,
                    sealed: r.get(3)?,
                    received_at: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("list folder deposits")?;
        Ok(rows)
    }

    /// One page of `folder_id`'s inbox past item `after`, oldest first: the
    /// first item always, then items while the sealed bytes stay within
    /// `budget`. The flag is whether more items lie past the page.
    pub async fn list_folder_deposits_page(
        &self,
        folder_id: i64,
        after: i64,
        budget: usize,
    ) -> Result<(Vec<FolderDepositRow>, bool)> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, folder_id, principal_id, sealed, received_at
               FROM folder_deposit_inbox
              WHERE folder_id = ?1 AND id > ?2 AND committed = 1 ORDER BY id",
        )?;
        let mut rows = stmt.query(rusqlite::params![folder_id, after])?;
        let mut page = Vec::new();
        let mut spent = 0usize;
        while let Some(r) = rows.next()? {
            let row = FolderDepositRow {
                id: r.get(0)?,
                folder_id: r.get(1)?,
                principal_id: r.get(2)?,
                sealed: r.get(3)?,
                received_at: r.get(4)?,
            };
            if !page.is_empty() && spent + row.sealed.len() > budget {
                return Ok((page, true));
            }
            spent += row.sealed.len();
            page.push(row);
        }
        Ok((page, false))
    }

    /// Retire item `id` from `folder_id`'s inbox — adoption's drain once the
    /// item's change row is durable. `false` when no such item was parked
    /// (another seat retired it first).
    pub async fn retire_folder_deposit(&self, folder_id: i64, id: i64) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM folder_deposit_inbox
                  WHERE folder_id = ?1 AND id = ?2 AND committed = 1",
                rusqlite::params![folder_id, id],
            )
            .context("retire folder deposit")?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_parked_deposit_lists_back_under_its_folder_only() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0xA1; 32];
        db.create_user(&actor, "free", "test").await.unwrap();
        let a = db.create_folder("inbox-a", &actor).await.unwrap();
        let b = db.create_folder("inbox-b", &actor).await.unwrap();
        let id = db
            .insert_folder_deposit(a, b"principal", b"sealed-bytes", 10, u64::MAX)
            .await
            .unwrap()
            .unwrap();
        let rows = db.list_folder_deposits(a).await.unwrap();
        assert_eq!(
            rows,
            vec![FolderDepositRow {
                id,
                folder_id: a,
                principal_id: b"principal".to_vec(),
                sealed: b"sealed-bytes".to_vec(),
                received_at: 10,
            }]
        );
        assert!(db.list_folder_deposits(b).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_page_keeps_its_first_item_and_stops_at_the_budget() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0xA1; 32];
        db.create_user(&actor, "free", "test").await.unwrap();
        let f = db.create_folder("inbox", &actor).await.unwrap();
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(
                db.insert_folder_deposit(f, b"p", &[0; 10], 1, u64::MAX)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        // A budget under one item still yields that item.
        let (page, more) = db.list_folder_deposits_page(f, 0, 5).await.unwrap();
        assert_eq!(page.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[0]]);
        assert!(more);
        let (page, more) = db.list_folder_deposits_page(f, ids[0], 20).await.unwrap();
        assert_eq!(
            page.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ids[1], ids[2]]
        );
        assert!(!more);
    }

    #[tokio::test]
    async fn a_retire_drops_one_item_once_and_only_in_its_folder() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0xA1; 32];
        db.create_user(&actor, "free", "test").await.unwrap();
        let a = db.create_folder("inbox-a", &actor).await.unwrap();
        let b = db.create_folder("inbox-b", &actor).await.unwrap();
        let id = db
            .insert_folder_deposit(a, b"p", b"s", 1, u64::MAX)
            .await
            .unwrap()
            .unwrap();
        assert!(!db.retire_folder_deposit(b, id).await.unwrap());
        assert!(db.retire_folder_deposit(a, id).await.unwrap());
        assert!(!db.retire_folder_deposit(a, id).await.unwrap());
        assert!(db.list_folder_deposits(a).await.unwrap().is_empty());
    }

    async fn inbox() -> (CacheDb, i64) {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0xA1; 32];
        db.create_user(&actor, "free", "test").await.unwrap();
        let f = db.create_folder("inbox", &actor).await.unwrap();
        (db, f)
    }

    #[tokio::test]
    async fn a_staged_upload_lists_only_once_committed_with_exactly_its_parts() {
        let (db, f) = inbox().await;
        let id = db
            .begin_folder_deposit_upload(f, b"p", b"tok", 1)
            .await
            .unwrap();
        assert_eq!(
            db.find_folder_deposit_upload(f, b"p", b"tok")
                .await
                .unwrap(),
            Some((id, false))
        );
        // Another principal's or another folder's view of the token is none.
        assert_eq!(
            db.find_folder_deposit_upload(f, b"q", b"tok")
                .await
                .unwrap(),
            None
        );
        for idx in [0, 1] {
            assert!(
                db.put_folder_deposit_part(id, idx, b"part", 2, u64::MAX)
                    .await
                    .unwrap()
            );
        }
        // Staged: not listed, and its parts are not served.
        assert!(db.list_folder_deposits(f).await.unwrap().is_empty());
        assert_eq!(db.get_folder_deposit_part(f, id, 0).await.unwrap(), None);
        assert_eq!(
            db.commit_folder_deposit_upload(id, 3, b"hdr", 3)
                .await
                .unwrap(),
            DepositCommit::PartsMismatch
        );
        assert_eq!(
            db.commit_folder_deposit_upload(id, 2, b"hdr", 3)
                .await
                .unwrap(),
            DepositCommit::Committed
        );
        assert_eq!(
            db.commit_folder_deposit_upload(id, 2, b"hdr", 4)
                .await
                .unwrap(),
            DepositCommit::AlreadyCommitted
        );
        let rows = db.list_folder_deposits(f).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sealed, b"hdr");
        assert_eq!(
            db.get_folder_deposit_part(f, id, 1).await.unwrap(),
            Some(b"part".to_vec())
        );
        // A committed item takes no more parts, and retiring takes its parts.
        assert!(
            !db.put_folder_deposit_part(id, 2, b"late", 5, u64::MAX)
                .await
                .unwrap()
        );
        assert!(db.retire_folder_deposit(f, id).await.unwrap());
        assert_eq!(db.get_folder_deposit_part(f, id, 0).await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_limit_counts_every_parked_byte_and_a_replaced_part_once() {
        let (db, f) = inbox().await;
        assert!(
            db.insert_folder_deposit(f, b"p", &[0; 4], 1, 10)
                .await
                .unwrap()
                .is_some()
        );
        let id = db
            .begin_folder_deposit_upload(f, b"p", b"t", 1)
            .await
            .unwrap();
        assert!(
            db.put_folder_deposit_part(id, 0, &[0; 4], 1, 10)
                .await
                .unwrap()
        );
        // Re-sending part 0 replaces it: still 8 of 10.
        assert!(
            db.put_folder_deposit_part(id, 0, &[0; 4], 1, 10)
                .await
                .unwrap()
        );
        assert!(
            !db.put_folder_deposit_part(id, 1, &[0; 4], 1, 10)
                .await
                .unwrap()
        );
        assert!(
            db.insert_folder_deposit(f, b"p", &[0; 3], 1, 10)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.insert_folder_deposit(f, b"p", &[0; 2], 1, 10)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn an_idle_staged_upload_expires_and_a_committed_item_never_does() {
        let (db, f) = inbox().await;
        let staged = db
            .begin_folder_deposit_upload(f, b"p", b"a", 10)
            .await
            .unwrap();
        db.put_folder_deposit_part(staged, 0, b"x", 10, u64::MAX)
            .await
            .unwrap();
        let kept = db
            .insert_folder_deposit(f, b"p", b"s", 1, u64::MAX)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(db.expire_staged_folder_deposits(10).await.unwrap(), 0);
        assert_eq!(db.expire_staged_folder_deposits(11).await.unwrap(), 1);
        assert_eq!(
            db.find_folder_deposit_upload(f, b"p", b"a").await.unwrap(),
            None
        );
        assert_eq!(
            db.list_folder_deposits(f)
                .await
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![kept]
        );
        // The expired upload's part went with it: the budget is whole again.
        assert!(
            db.insert_folder_deposit(f, b"p", &[0; 9], 1, 10)
                .await
                .unwrap()
                .is_some()
        );
    }
}
