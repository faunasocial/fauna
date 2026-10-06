//! `nest_nat_mode` singleton accessors. The client-set NAT axis
//! (`public` / `private`) persistence — the authoritative source
//! `AppState.node_mode` resolves from at boot (falling back to the
//! `config.nest.mode` seed when absent). **Mutable**: a public↔private flip
//! touches no at-rest data, so `set_nat_mode` upserts
//! (`ON CONFLICT(id) DO UPDATE`) — the admin toggle re-sets it freely.
//! Design tracked internally, § 2.

use anyhow::{Context, Result};

use crate::config::NodeMode;

impl crate::db::CacheDb {
    /// Read the client-set NAT mode, or `None` if no client has set it yet
    /// (the never-set state on a pre-claim box — the caller falls back to the
    /// `config.nest.mode` seed).
    pub async fn get_nat_mode(&self) -> Result<Option<NodeMode>> {
        let raw: Option<String> = self.get_singleton_column("nest_nat_mode", "mode").await?;
        match raw {
            None => Ok(None),
            Some(s) => NodeMode::from_wire_str(&s)
                .map(Some)
                .with_context(|| format!("invalid nat mode {s:?} in nest_nat_mode table")),
        }
    }

    /// Set the NAT mode (mutable upsert). `INSERT … ON CONFLICT(id) DO UPDATE`
    /// — the first set inserts, every later set overwrites: the admin panel
    /// flips the NAT axis at runtime, and the single upsert is the atomic
    /// decision point for crash-atomicity (a crash before/after leaves a
    /// coherent value — never a half-state).
    pub async fn set_nat_mode(&self, mode: NodeMode) -> Result<()> {
        self.set_singleton_column("nest_nat_mode", "mode", mode.as_str())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use std::sync::Arc;

    #[tokio::test]
    async fn round_trip_and_upsert_overwrites() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        // Unset on a fresh DB — the caller falls back to the config seed.
        assert_eq!(db.get_nat_mode().await.unwrap(), None);

        // First set inserts.
        db.set_nat_mode(NodeMode::Private).await.unwrap();
        assert_eq!(db.get_nat_mode().await.unwrap(), Some(NodeMode::Private));

        // Mutable: a second set overwrites.
        db.set_nat_mode(NodeMode::Public).await.unwrap();
        assert_eq!(db.get_nat_mode().await.unwrap(), Some(NodeMode::Public));

        // Idempotent re-set of the same value.
        db.set_nat_mode(NodeMode::Public).await.unwrap();
        assert_eq!(db.get_nat_mode().await.unwrap(), Some(NodeMode::Public));
    }
}
