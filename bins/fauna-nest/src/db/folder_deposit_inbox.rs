//! A folder's **inbox segment** — third-party deposits parked sealed until a
//! seat adopts them.
//!
//! Owner: `docs/goal/behavior/file-sync.md` § Third-party deposit ingress. The
//! table's own comment (`migrations.rs`, `MIGRATIONS_FOLDER_DEPOSIT_INBOX`)
//! argues its shape. This module is storage only: the door that seals and
//! the gate in front of it are `crate::folder_deposit`.

use anyhow::{Context, Result};

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

impl CacheDb {
    /// Park one sealed deposit in `folder_id`'s inbox segment; returns the
    /// item's id.
    pub async fn insert_folder_deposit(
        &self,
        folder_id: i64,
        principal_id: &[u8],
        sealed: &[u8],
        received_at: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
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
        Ok(conn.last_insert_rowid())
    }

    /// Every item parked in `folder_id`'s inbox segment, oldest first.
    pub async fn list_folder_deposits(&self, folder_id: i64) -> Result<Vec<FolderDepositRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, folder_id, principal_id, sealed, received_at
               FROM folder_deposit_inbox WHERE folder_id = ?1 ORDER BY id",
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
               FROM folder_deposit_inbox WHERE folder_id = ?1 AND id > ?2 ORDER BY id",
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
                "DELETE FROM folder_deposit_inbox WHERE folder_id = ?1 AND id = ?2",
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
            .insert_folder_deposit(a, b"principal", b"sealed-bytes", 10)
            .await
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
                db.insert_folder_deposit(f, b"p", &[0; 10], 1)
                    .await
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
        let id = db.insert_folder_deposit(a, b"p", b"s", 1).await.unwrap();
        assert!(!db.retire_folder_deposit(b, id).await.unwrap());
        assert!(db.retire_folder_deposit(a, id).await.unwrap());
        assert!(!db.retire_folder_deposit(a, id).await.unwrap());
        assert!(db.list_folder_deposits(a).await.unwrap().is_empty());
    }
}
