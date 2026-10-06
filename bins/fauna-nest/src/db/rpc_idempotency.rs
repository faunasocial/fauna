//! The **durable** idempotency tier (W4 (account-data-plane.md § Workstreams) phase 3) — the cross-connection half
//! of the reply-replay contract.
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § The
//! offline-mutation contract → *Nest-side durable idempotency* (the
//! requirement) + `docs/goal/architecture/transport.md` § Idempotency and
//! reconnect-with-resume (the wire semantics). The per-connection
//! [`crate::ws::IdempotencyCache`] is structurally useless for a replay that
//! crosses a reconnect — it is empty on every fresh connection, and the
//! client's `request_auto_retry` (and the offline outbox's drain) re-issue
//! **after** the reconnect, always onto a fresh connection. This table is the
//! durable second level the per-actor sink consults on an LRU miss.
//!
//! Three deliberate rules, each load-bearing:
//!
//! 1. **Only `ok = true` replies are recorded.** An error Reply implies the
//!    handler applied no effect, so re-running it on a replay is safe and
//!    *wanted* — durably replaying a transient error (`timeout`, a lock
//!    conflict) would wedge the retrying intent on that error for the whole
//!    retention window, which is precisely the outcome a retry exists to
//!    escape. (The pre-existing `fauna.protocol.timeout` ambiguity — the
//!    handler may complete after the timeout Reply was sent — is unchanged by
//!    this tier and owned by the transport doc.)
//! 2. **`Read`-class kinds are never recorded** (`fauna_protocol::
//!    offline_class`): a re-run read has no effect and its freshest answer is
//!    the better reply. Every *mutation* class is recorded — including
//!    `forbid_replay = true` kinds, because the serve side replays "regardless
//!    of the kind's `forbid_replay` flag (the original op succeeded once; the
//!    question is just what did it return)" — transport.md's rule, verbatim.
//! 3. **Rows are actor-scoped.** The key is `(actor_id, idem_key)` — one
//!    actor's key can never fetch (or block) another actor's reply; the
//!    anonymous connection never touches this table at all (its placeholder
//!    actor would alias every anonymous caller into one namespace).
//!
//! Retention: [`DEFAULT_RPC_IDEMPOTENCY_RETENTION`] (lazy expiry on lookup via
//! the caller-passed cutoff + the periodic sweeper). The window a replay must
//! survive is "effect applied, ack lost, next drain pass re-presents the key"
//! — reconnect cycles and outbox passes, i.e. minutes-to-hours; 7 days is the
//! generous ceiling, and the table is a replay cache, never the only copy of
//! anything (dropping a row costs one handler re-run of a naturally-idempotent
//! kind, the pre-tier behavior).

use anyhow::{Context, Result};

use super::CacheDb;

/// Durable retention for a recorded reply, from `created_at`.
pub const DEFAULT_RPC_IDEMPOTENCY_RETENTION: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// A durably recorded Reply for an already-applied `(actor, key)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableReply {
    /// Canonical-CBOR encoding of the Reply's payload `Value`, or `None` for a
    /// too-large marker (the mirror of `CachedReply::too_large`): the effect
    /// happened, but the bytes were not retained, so a replay is answered
    /// `fauna.protocol.replay_too_large` rather than re-running the handler.
    pub payload: Option<Vec<u8>>,
    /// The recorded Reply's `ok` bit. Always `true` today (rule 1 above);
    /// carried explicitly so the replay rebuilds the frame it recorded rather
    /// than assuming.
    pub ok: bool,
}

impl CacheDb {
    /// The recorded reply for `(actor_id, key)`, if one exists and was created
    /// at or after `cutoff_created_at` (Unix seconds — lazy TTL; the sweeper
    /// deletes expired rows on its own cadence).
    pub async fn lookup_rpc_idempotent(
        &self,
        actor_id: &[u8; 32],
        key: &[u8; 16],
        cutoff_created_at: i64,
    ) -> Result<Option<DurableReply>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT reply, ok FROM rpc_idempotency
             WHERE actor_id = ?1 AND idem_key = ?2 AND created_at >= ?3",
            rusqlite::params![actor_id.as_slice(), key.as_slice(), cutoff_created_at],
            |row| {
                Ok(DurableReply {
                    payload: row.get::<_, Option<Vec<u8>>>(0)?,
                    ok: row.get::<_, i64>(1)? != 0,
                })
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .context("lookup rpc idempotency")
    }

    /// Record a reply for `(actor_id, key)`. `payload = None` records a
    /// too-large marker. **First write wins** (`INSERT OR IGNORE`): if a row
    /// already exists, the original reply stands — a second write for the same
    /// key can only be a racing duplicate of the same logical request, and the
    /// contract is "the FIRST outcome".
    pub async fn insert_rpc_idempotent(
        &self,
        actor_id: &[u8; 32],
        key: &[u8; 16],
        kind: &str,
        payload: Option<&[u8]>,
        ok: bool,
        now: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO rpc_idempotency
                 (actor_id, idem_key, kind, reply, ok, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                actor_id.as_slice(),
                key.as_slice(),
                kind,
                payload,
                ok as i64,
                now
            ],
        )
        .context("insert rpc idempotency")?;
        Ok(())
    }

    /// Delete recorded replies created before `cutoff_created_at` (Unix
    /// seconds). Returns the number deleted. Backs
    /// [`spawn_rpc_idempotency_retention_sweeper`].
    pub async fn prune_rpc_idempotency_older_than(&self, cutoff_created_at: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM rpc_idempotency WHERE created_at < ?1",
                rusqlite::params![cutoff_created_at],
            )
            .context("prune rpc idempotency")?;
        Ok(n)
    }
}

/// Spawns a tokio task that periodically deletes recorded replies past
/// `retention` (by `created_at`). Cadence is 1/24 of `retention`, the shared
/// [`super::spawn_retention_sweeper`] shape (mirrors
/// `admin::spawn_invite_request_retention_sweeper`). The caller wires the
/// handle through `AppState::scope_handle` — generation-scoped like every
/// boot worker.
pub fn spawn_rpc_idempotency_retention_sweeper(
    db: std::sync::Arc<CacheDb>,
    retention: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    super::spawn_retention_sweeper(retention, move || {
        let db = db.clone();
        async move {
            let cutoff = super::now_epoch_secs().saturating_sub(retention.as_secs() as i64);
            match db.prune_rpc_idempotency_older_than(cutoff).await {
                Ok(n) if n > 0 => tracing::debug!(
                    target: "rpc_idempotency",
                    pruned = n,
                    cutoff,
                    "pruned durable idempotency rows"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    target: "rpc_idempotency",
                    error = %e,
                    "rpc-idempotency retention sweep failed"
                ),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> CacheDb {
        CacheDb::open_in_memory().expect("in-memory db")
    }

    #[tokio::test]
    async fn a_recorded_reply_reads_back_for_its_actor_and_key() {
        let db = db();
        let actor = [7u8; 32];
        let key = [1u8; 16];
        db.insert_rpc_idempotent(&actor, &key, "fauna.test.kind", Some(b"reply"), true, 1000)
            .await
            .unwrap();
        let hit = db.lookup_rpc_idempotent(&actor, &key, 0).await.unwrap();
        assert_eq!(
            hit,
            Some(DurableReply {
                payload: Some(b"reply".to_vec()),
                ok: true
            })
        );
    }

    /// Rule 3: the row is invisible to any OTHER actor presenting the same
    /// 16-byte key — without the actor in the primary key, one client could
    /// fetch (or pre-empt) another's recorded reply by key collision or probe.
    #[tokio::test]
    async fn another_actor_never_sees_the_row() {
        let db = db();
        let key = [2u8; 16];
        db.insert_rpc_idempotent(&[7u8; 32], &key, "fauna.test.kind", Some(b"a"), true, 1000)
            .await
            .unwrap();
        assert_eq!(
            db.lookup_rpc_idempotent(&[8u8; 32], &key, 0).await.unwrap(),
            None,
            "rows are (actor, key)-scoped"
        );
    }

    /// First write wins: a racing duplicate of the same logical request cannot
    /// replace the recorded first outcome.
    #[tokio::test]
    async fn the_first_recorded_reply_stands() {
        let db = db();
        let actor = [7u8; 32];
        let key = [3u8; 16];
        db.insert_rpc_idempotent(&actor, &key, "fauna.test.kind", Some(b"first"), true, 1000)
            .await
            .unwrap();
        db.insert_rpc_idempotent(&actor, &key, "fauna.test.kind", Some(b"second"), true, 1001)
            .await
            .unwrap();
        let hit = db.lookup_rpc_idempotent(&actor, &key, 0).await.unwrap();
        assert_eq!(hit.unwrap().payload.as_deref(), Some(b"first".as_slice()));
    }

    /// Lazy TTL: a row older than the caller's cutoff reads as absent, so an
    /// expired entry re-runs the handler instead of serving a stale reply the
    /// sweeper merely hasn't collected yet.
    #[tokio::test]
    async fn an_expired_row_reads_as_absent_and_prunes() {
        let db = db();
        let actor = [7u8; 32];
        let key = [4u8; 16];
        db.insert_rpc_idempotent(&actor, &key, "fauna.test.kind", Some(b"old"), true, 1000)
            .await
            .unwrap();
        assert_eq!(
            db.lookup_rpc_idempotent(&actor, &key, 2000).await.unwrap(),
            None,
            "cutoff-expired rows are misses"
        );
        assert_eq!(db.prune_rpc_idempotency_older_than(2000).await.unwrap(), 1);
        assert_eq!(
            db.lookup_rpc_idempotent(&actor, &key, 0).await.unwrap(),
            None,
            "pruned rows are gone even for a cutoff that would admit them"
        );
    }

    /// The too-large marker round-trips as `payload: None` — the effect
    /// happened, the bytes were not retained.
    #[tokio::test]
    async fn a_too_large_marker_round_trips() {
        let db = db();
        let actor = [7u8; 32];
        let key = [5u8; 16];
        db.insert_rpc_idempotent(&actor, &key, "fauna.test.kind", None, true, 1000)
            .await
            .unwrap();
        let hit = db.lookup_rpc_idempotent(&actor, &key, 0).await.unwrap();
        assert_eq!(
            hit,
            Some(DurableReply {
                payload: None,
                ok: true
            })
        );
    }
}
