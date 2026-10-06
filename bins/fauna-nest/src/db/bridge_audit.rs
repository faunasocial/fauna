//! Bridge-reported audit events. Append-only log of `report_auth_event`
//! calls plus session-close reports (the latter is I2b — for now this
//! module handles only auth events).
//!
//! Per spec § Audit-trail caveat: a long-term compromised bridge can
//! falsify entries; the rate-limit gate on fetch_* RPCs is the
//! independent abuse bound.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::domain_hash::{HashField, write_fields};

use super::{CacheDb, blob_to_array, now_epoch_millis};

/// Default retention for bridge_audit_events. Per the spec the audit
/// log is bridge-reported (so a long-term compromised bridge can
/// poison entries anyway); 90 days balances forensic value against
/// table growth on a busy deployment.
pub const DEFAULT_AUDIT_RETENTION: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// Spawns a tokio task that periodically deletes audit rows older
/// than `retention` from the supplied database. Cadence is
/// 1/24 of the retention so a 90-day retention sweeps every ~3.75
/// days, which is rare enough to keep the lock held briefly.
pub fn spawn_audit_retention_sweeper(
    db: Arc<CacheDb>,
    retention: Duration,
) -> tokio::task::JoinHandle<()> {
    super::spawn_retention_sweeper(retention, move || {
        let db = db.clone();
        async move {
            let cutoff_ms = now_epoch_millis().saturating_sub(retention.as_millis() as i64);
            match db.prune_bridge_audit_events_older_than(cutoff_ms).await {
                Ok(n) if n > 0 => tracing::info!(
                    target: "bridge_audit",
                    pruned = n,
                    cutoff_ms,
                    "pruned bridge audit events"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    target: "bridge_audit",
                    error = %e,
                    "audit retention sweep failed"
                ),
            }
        }
    })
}

#[derive(Debug, Clone)]
pub struct BridgeSessionCloseRow {
    pub bridge_actor_id: [u8; 32],
    pub actor_id: [u8; 32],
    pub credential_id: String,
    pub reason: String,
    pub occurred_at: i64,
}

/// Domain-separated idempotency hash for `report_session_close` rows.
fn session_close_idempotency_hash(
    bridge_actor_id: &[u8; 32],
    actor_id: &[u8; 32],
    credential_id: &str,
    reason: &str,
    occurred_at: i64,
) -> [u8; 32] {
    const DST: &[u8] = b"fauna.bridges.report_session_close.v1";
    let mut h = Sha256::new();
    write_fields(
        DST,
        &[
            HashField::Fixed32(bridge_actor_id),
            HashField::Fixed32(actor_id),
            HashField::LenPrefixed(credential_id.as_bytes()),
            HashField::LenPrefixed(reason.as_bytes()),
            HashField::I64(occurred_at),
        ],
        |b| h.update(b),
    );
    h.finalize().into()
}

/// Domain-separated idempotency hash for `report_auth_event` rows.
/// Hashes the natural-key 7-tuple so that a bridge retrying the same
/// payload after a transport failure collapses on the partial UNIQUE
/// index in `bridge_audit_events`.
fn auth_event_idempotency_hash(
    bridge_actor_id: &[u8; 32],
    actor_id: &[u8; 32],
    credential_id: &str,
    result: &str,
    source_ip: &str,
    occurred_at: i64,
    reason: Option<&str>,
) -> [u8; 32] {
    const DST: &[u8] = b"fauna.bridges.report_auth_event.v1";
    let mut h = Sha256::new();
    write_fields(
        DST,
        &[
            HashField::Fixed32(bridge_actor_id),
            HashField::Fixed32(actor_id),
            HashField::LenPrefixed(credential_id.as_bytes()),
            HashField::LenPrefixed(result.as_bytes()),
            HashField::LenPrefixed(source_ip.as_bytes()),
            HashField::I64(occurred_at),
            HashField::OptStr(reason),
        ],
        |b| h.update(b),
    );
    h.finalize().into()
}

#[derive(Debug, Clone)]
pub struct BridgeAuthEventRow {
    pub bridge_actor_id: [u8; 32],
    pub actor_id: [u8; 32],
    pub credential_id: String,
    pub result: String,
    pub source_ip: String,
    pub occurred_at: i64,
    pub reason: Option<String>,
}

impl CacheDb {
    pub async fn append_bridge_auth_event(
        &self,
        bridge_actor_id: &[u8; 32],
        actor_id: &[u8; 32],
        credential_id: &str,
        result: &str,
        source_ip: &str,
        occurred_at: i64,
        reason: Option<&str>,
    ) -> Result<()> {
        let bridge = *bridge_actor_id;
        let actor = *actor_id;
        let hash = auth_event_idempotency_hash(
            &bridge,
            &actor,
            credential_id,
            result,
            source_ip,
            occurred_at,
            reason,
        );
        let credential = credential_id.to_string();
        let result = result.to_string();
        let source_ip = source_ip.to_string();
        let reason = reason.map(|s| s.to_string());
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        // INSERT OR IGNORE because the UNIQUE index on
        // idempotency_hash is partial (NULL-skip); ON CONFLICT(col)
        // requires a full UNIQUE constraint and would refuse to compile.
        conn.execute(
            "INSERT OR IGNORE INTO bridge_audit_events
                (received_at, bridge_actor_id, actor_id, credential_id,
                 result, source_ip, occurred_at, reason, idempotency_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                now,
                &bridge[..],
                &actor[..],
                credential,
                result,
                source_ip,
                occurred_at,
                reason,
                &hash[..]
            ],
        )
        .context("append bridge auth event")?;
        Ok(())
    }

    pub async fn append_bridge_session_close(
        &self,
        bridge_actor_id: &[u8; 32],
        actor_id: &[u8; 32],
        credential_id: &str,
        reason: &str,
        occurred_at: i64,
    ) -> Result<()> {
        let bridge = *bridge_actor_id;
        let actor = *actor_id;
        let hash =
            session_close_idempotency_hash(&bridge, &actor, credential_id, reason, occurred_at);
        let credential = credential_id.to_string();
        let reason = reason.to_string();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO bridge_session_close_events
                (received_at, bridge_actor_id, actor_id, credential_id,
                 reason, occurred_at, idempotency_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                now,
                &bridge[..],
                &actor[..],
                credential,
                reason,
                occurred_at,
                &hash[..]
            ],
        )
        .context("append bridge session close")?;
        Ok(())
    }

    pub async fn list_bridge_session_close_for_actor(
        &self,
        actor_id: &[u8; 32],
        limit: i64,
    ) -> Result<Vec<BridgeSessionCloseRow>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                // `id DESC` tiebreaks when two writes land in the same
                // millisecond; without it SQLite's default rowid order
                // makes ordering non-deterministic and the
                // round_trips_and_dedupes test flakes.
                "SELECT bridge_actor_id, credential_id, reason, occurred_at
                   FROM bridge_session_close_events
                  WHERE actor_id = ?1
                  ORDER BY received_at DESC, id DESC
                  LIMIT ?2",
            )
            .context("prepare list session close")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..], limit], |row| {
                let bridge_v: Vec<u8> = row.get(0)?;
                let cred: String = row.get(1)?;
                let reason: String = row.get(2)?;
                let occurred_at: i64 = row.get(3)?;
                Ok((bridge_v, cred, reason, occurred_at))
            })
            .context("query list session close")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list session close")?;
        let mut out = Vec::with_capacity(rows.len());
        for (bridge_v, credential_id, reason, occurred_at) in rows {
            let bridge: [u8; 32] = blob_to_array(bridge_v.as_slice(), "bridge_actor_id")?;
            out.push(BridgeSessionCloseRow {
                bridge_actor_id: bridge,
                actor_id: actor,
                credential_id,
                reason,
                occurred_at,
            });
        }
        Ok(out)
    }

    /// Deletes audit rows whose `received_at` is strictly less than
    /// `cutoff_received_at_ms`. Returns the number of rows deleted.
    /// Backs the periodic retention sweep (the "audit-log retention"
    /// carry-over).
    pub async fn prune_bridge_audit_events_older_than(
        &self,
        cutoff_received_at_ms: i64,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM bridge_audit_events WHERE received_at < ?1",
                rusqlite::params![cutoff_received_at_ms],
            )
            .context("prune bridge audit events")?;
        Ok(n)
    }

    pub async fn list_bridge_auth_events_for_actor(
        &self,
        actor_id: &[u8; 32],
        limit: i64,
    ) -> Result<Vec<BridgeAuthEventRow>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT bridge_actor_id, credential_id, result, source_ip,
                        occurred_at, reason
                   FROM bridge_audit_events
                  WHERE actor_id = ?1
                  ORDER BY received_at DESC, id DESC
                  LIMIT ?2",
            )
            .context("prepare list bridge auth events")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..], limit], |row| {
                let bridge_v: Vec<u8> = row.get(0)?;
                let credential: String = row.get(1)?;
                let result: String = row.get(2)?;
                let source_ip: String = row.get(3)?;
                let occurred_at: i64 = row.get(4)?;
                let reason: Option<String> = row.get(5)?;
                Ok((bridge_v, credential, result, source_ip, occurred_at, reason))
            })
            .context("query list bridge auth events")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list bridge auth events")?;
        let mut out = Vec::with_capacity(rows.len());
        for (bridge_v, credential, result, source_ip, occurred_at, reason) in rows {
            let bridge: [u8; 32] = blob_to_array(bridge_v.as_slice(), "bridge_actor_id")?;
            out.push(BridgeAuthEventRow {
                bridge_actor_id: bridge,
                actor_id: actor,
                credential_id: credential,
                result,
                source_ip,
                occurred_at,
                reason,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_and_list_round_trip() {
        let db = CacheDb::open_in_memory().unwrap();
        let bridge = [1u8; 32];
        let actor = [2u8; 32];
        for i in 0..3 {
            db.append_bridge_auth_event(
                &bridge,
                &actor,
                &format!("cred-{i}"),
                "ok",
                "10.0.0.1",
                1_700_000_000 + i,
                None,
            )
            .await
            .unwrap();
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
        let rows = db
            .list_bridge_auth_events_for_actor(&actor, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        // Most recent first.
        assert_eq!(rows[0].credential_id, "cred-2");
        assert_eq!(rows[2].credential_id, "cred-0");
    }

    #[tokio::test]
    async fn fail_event_with_reason_round_trips() {
        let db = CacheDb::open_in_memory().unwrap();
        let bridge = [3u8; 32];
        let actor = [4u8; 32];
        db.append_bridge_auth_event(
            &bridge,
            &actor,
            "cred-x",
            "fail",
            "203.0.113.1",
            1_700_000_000,
            Some("AEAD verify failed"),
        )
        .await
        .unwrap();
        let rows = db
            .list_bridge_auth_events_for_actor(&actor, 1)
            .await
            .unwrap();
        assert_eq!(rows[0].result, "fail");
        assert_eq!(rows[0].reason.as_deref(), Some("AEAD verify failed"));
    }

    #[tokio::test]
    async fn duplicate_append_is_idempotent() {
        let db = CacheDb::open_in_memory().unwrap();
        let bridge = [5u8; 32];
        let actor = [6u8; 32];
        for _ in 0..3 {
            db.append_bridge_auth_event(
                &bridge,
                &actor,
                "cred-z",
                "ok",
                "10.0.0.7",
                1_700_000_000,
                None,
            )
            .await
            .unwrap();
        }
        let rows = db
            .list_bridge_auth_events_for_actor(&actor, 5)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "duplicate appends must collapse to one row");
    }

    #[tokio::test]
    async fn prune_drops_rows_older_than_cutoff() {
        let db = CacheDb::open_in_memory().unwrap();
        let bridge = [9u8; 32];
        let actor = [10u8; 32];
        // Three rows whose `received_at` columns we'll override after
        // insert to simulate aged rows. Using distinct credential_ids
        // makes them distinct under the idempotency UNIQUE.
        for i in 0..3 {
            db.append_bridge_auth_event(
                &bridge,
                &actor,
                &format!("cred-{i}"),
                "ok",
                "10.0.0.1",
                1_700_000_000 + i,
                None,
            )
            .await
            .unwrap();
        }
        // Backdate two of them.
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE bridge_audit_events SET received_at = ?1 WHERE credential_id IN ('cred-0','cred-1')",
                rusqlite::params![100_i64],
            )
            .unwrap();
        }

        let pruned = db
            .prune_bridge_audit_events_older_than(1_000_000)
            .await
            .unwrap();
        assert_eq!(pruned, 2);
        let rows = db
            .list_bridge_auth_events_for_actor(&actor, 50)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].credential_id, "cred-2");
    }

    #[tokio::test]
    async fn spawn_audit_retention_sweeper_prunes_after_tick() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let bridge = [11u8; 32];
        let actor = [12u8; 32];
        db.append_bridge_auth_event(
            &bridge,
            &actor,
            "cred-old",
            "ok",
            "10.0.0.1",
            1_700_000_000,
            None,
        )
        .await
        .unwrap();
        // Pretend the row is much older than the retention window.
        {
            let conn = db.conn.lock().await;
            conn.execute("UPDATE bridge_audit_events SET received_at = 1", [])
                .unwrap();
        }
        // Retention = 60ms ⇒ tick = 60/24 ≈ 2ms (tokio::interval floors to 1ms),
        // so on an idle box the prune lands in single-digit milliseconds.
        //
        // The assertion is a DEADLINE POLL, not a settle-sleep (testing.md
        // § point 14). This test previously slept a fixed 30ms and then asserted
        // — a budget that holds only on an idle machine, while the primary dev
        // VM routinely runs 20+ concurrent builds at double-digit load, so it
        // was in the defunct wall-clock class and would fail under exactly the
        // conditions it is normally run in. The budget below is sized far above any
        // non-pathological scheduling delay; a green run pays only for the first
        // poll, because the loop exits as soon as the prune is observed.
        const PRUNE_BUDGET: Duration = Duration::from_secs(30);
        let handle = spawn_audit_retention_sweeper(db.clone(), Duration::from_millis(60));
        let deadline = std::time::Instant::now() + PRUNE_BUDGET;
        loop {
            let rows = db
                .list_bridge_auth_events_for_actor(&actor, 5)
                .await
                .unwrap();
            if rows.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "sweeper did not prune the aged row within {PRUNE_BUDGET:?} \
                 ({} row(s) still present)",
                rows.len()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        handle.abort();
    }

    #[tokio::test]
    async fn session_close_round_trips_and_dedupes() {
        let db = CacheDb::open_in_memory().unwrap();
        let bridge = [13u8; 32];
        let actor = [14u8; 32];
        // Three identical writes — should collapse to one row via the
        // partial UNIQUE on idempotency_hash.
        for _ in 0..3 {
            db.append_bridge_session_close(&bridge, &actor, "cred", "logout", 1_700_000_000)
                .await
                .unwrap();
        }
        // A different reason is a different row.
        db.append_bridge_session_close(&bridge, &actor, "cred", "idle_timeout", 1_700_000_001)
            .await
            .unwrap();
        let rows = db
            .list_bridge_session_close_for_actor(&actor, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].reason, "idle_timeout"); // most-recent first
        assert_eq!(rows[1].reason, "logout");
    }

    #[tokio::test]
    async fn distinct_payloads_each_persist() {
        let db = CacheDb::open_in_memory().unwrap();
        let bridge = [7u8; 32];
        let actor = [8u8; 32];
        // Differ on each meaningful field one at a time; expect 6 distinct rows.
        let base = ("cred-a", "ok", "10.0.0.1", 1_700_000_000i64, None);
        let variants: Vec<(&str, &str, &str, i64, Option<&str>)> = vec![
            base,
            ("cred-b", base.1, base.2, base.3, base.4),
            (base.0, "fail", base.2, base.3, base.4),
            (base.0, base.1, "10.0.0.2", base.3, base.4),
            (base.0, base.1, base.2, 1_700_000_001, base.4),
            (base.0, base.1, base.2, base.3, Some("retry note")),
        ];
        for (cred, result, ip, ts, reason) in &variants {
            db.append_bridge_auth_event(&bridge, &actor, cred, result, ip, *ts, *reason)
                .await
                .unwrap();
        }
        let rows = db
            .list_bridge_auth_events_for_actor(&actor, 50)
            .await
            .unwrap();
        assert_eq!(rows.len(), variants.len());
    }
}
