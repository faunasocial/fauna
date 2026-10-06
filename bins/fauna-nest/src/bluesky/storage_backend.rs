//! [`StorageBackend`] adapter for the node's [`CacheDb`].
//!
//! Bridges the generic key-value trait from `fauna-bridge-atproto`
//! to the node's SQLite database.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, bail};
use fauna_bridge_atproto::store::StorageBackend;

use crate::db::CacheDb;

/// Known table names for Bluesky OAuth storage.
///
/// Only these tables are allowed to prevent SQL injection.
const ALLOWED_TABLES: &[&str] = &["bluesky_oauth_states", "bluesky_sessions"];

fn validate_table(table: &str) -> anyhow::Result<()> {
    if ALLOWED_TABLES.contains(&table) {
        Ok(())
    } else {
        bail!("unknown storage table: {table}")
    }
}

/// Wraps an `Arc<CacheDb>` to implement [`StorageBackend`].
pub struct CacheDbBackend {
    db: Arc<CacheDb>,
}

impl CacheDbBackend {
    pub fn new(db: Arc<CacheDb>) -> Self {
        Self { db }
    }
}

impl StorageBackend for CacheDbBackend {
    fn get(
        &self,
        table: &str,
        key: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Vec<u8>>>> + Send + '_>> {
        let table = table.to_string();
        let key = key.to_string();
        Box::pin(async move {
            validate_table(&table)?;
            let conn = self.db.conn().await;
            let sql = format!("SELECT data FROM {table} WHERE key = ?1");
            let mut stmt = conn.prepare(&sql).context("prepare get")?;
            let mut rows = stmt.query(rusqlite::params![key]).context("query get")?;
            match rows.next().context("next row")? {
                Some(row) => {
                    let data: Vec<u8> = row.get(0).context("get data column")?;
                    Ok(Some(data))
                }
                None => Ok(None),
            }
        })
    }

    fn set(
        &self,
        table: &str,
        key: &str,
        value: &[u8],
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let table = table.to_string();
        let key = key.to_string();
        let value = value.to_vec();
        Box::pin(async move {
            validate_table(&table)?;
            let conn = self.db.conn().await;
            if table == "bluesky_oauth_states" {
                conn.execute(
                    &format!(
                        "INSERT OR REPLACE INTO {table} (key, data, expires_at) VALUES (?1, ?2, 0)"
                    ),
                    rusqlite::params![key, value],
                )
                .context("set with expires_at")?;
            } else {
                conn.execute(
                    &format!("INSERT OR REPLACE INTO {table} (key, data) VALUES (?1, ?2)"),
                    rusqlite::params![key, value],
                )
                .context("set")?;
            }
            Ok(())
        })
    }

    fn del(
        &self,
        table: &str,
        key: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let table = table.to_string();
        let key = key.to_string();
        Box::pin(async move {
            validate_table(&table)?;
            let conn = self.db.conn().await;
            conn.execute(
                &format!("DELETE FROM {table} WHERE key = ?1"),
                rusqlite::params![key],
            )
            .context("del")?;
            Ok(())
        })
    }

    fn clear(&self, table: &str) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let table = table.to_string();
        Box::pin(async move {
            validate_table(&table)?;
            let conn = self.db.conn().await;
            conn.execute(&format!("DELETE FROM {table}"), [])
                .context("clear")?;
            Ok(())
        })
    }
}
