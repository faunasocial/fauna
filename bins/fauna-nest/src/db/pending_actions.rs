//! DB methods for the pending_actions table.

use anyhow::{Context, Result, bail};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_secs};
use crate::db::chain_version::{self, ChainVersion};
use crate::pending_actions::{ActionType, PendingActionRow};

/// Genesis hash for the pending-actions chain (distinct from the audit log
/// chain). `pub(crate)` for the one-time version classification in
/// [`crate::db::migrations`], which walks the table's `prev_hash` carry from
/// the same seed the writer starts from.
pub(crate) fn genesis_hash() -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(b"fauna-pending-actions-genesis-v1").to_vec()
}

/// The `snapshots` rows an action marked `deletion_pending` when it was created.
///
/// Two producers, two encodings, deliberately read in one place so a third can
/// never be added on only one side of the create/cancel pair:
/// - `snapshot.delete` — one id, in `target` (`filesync_handlers::delete_handler`);
/// - `snapshot.bulk_prune` — a batch, in `payload.snapshot_ids`
///   (`backup::prune::schedule_auto_prune`).
///
/// Any other action type owns no snapshot marks and yields nothing. A malformed
/// row yields nothing rather than erroring: failing to *un*-mark is a leak worth
/// a look, but failing to *cancel* on a bad payload would trap the user in a
/// deletion they asked to call off.
fn snapshot_targets_of(action: &PendingActionRow) -> Vec<i64> {
    snapshot_targets_from(
        &action.action_type,
        action.target.as_deref(),
        action.payload.as_deref(),
    )
}

/// The column-level half of [`snapshot_targets_of`], for callers holding the
/// three columns rather than a parsed row.
///
/// The succession disarm leg (`db::successions`) is one: it works inside a
/// `rusqlite::Transaction` over a `SELECT` of exactly these columns, and a
/// second copy of this match is how the two would drift — a new
/// snapshot-shaped `ActionType` added to one and not the other leaves the mark
/// on, which is precisely the failure the un-mark exists to prevent.
pub(super) fn snapshot_targets_from(
    action_type: &str,
    target: Option<&str>,
    payload: Option<&str>,
) -> Vec<i64> {
    match action_type {
        "snapshot.delete" => target
            .and_then(|t| t.parse::<i64>().ok())
            .into_iter()
            .collect(),
        "snapshot.bulk_prune" => payload
            .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
            .and_then(|v| v["snapshot_ids"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_i64())
            .collect(),
        _ => Vec::new(),
    }
}

/// The `sync_changes` rows (by `seq`) an action marked `prune_pending` when it
/// was created — [`snapshot_targets_from`]'s version-plane twin, under the same
/// one-place rule and the same malformed-row leniency (failing to *un*-mark is
/// a leak worth a look; failing to *cancel* would trap the user in a prune they
/// asked to call off).
///
/// One producer today: `version.bulk_prune`, a batch in `payload.version_seqs`
/// (`backup::version_prune::schedule_version_auto_prune`).
pub(super) fn version_targets_from(action_type: &str, payload: Option<&str>) -> Vec<i64> {
    match action_type {
        "version.bulk_prune" => payload
            .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
            .and_then(|v| v["version_seqs"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_i64())
            .collect(),
        _ => Vec::new(),
    }
}

impl CacheDb {
    // ==================== Chain hash helpers ====================

    /// Return the chain_hash of the most recent pending_actions row,
    /// or the genesis hash if the table is empty.
    pub async fn get_last_pending_action_hash(&self) -> Result<Vec<u8>> {
        let conn = self.conn.lock().await;
        let result: rusqlite::Result<Vec<u8>> = conn.query_row(
            "SELECT chain_hash FROM pending_actions ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        );
        match result {
            Ok(hash) => Ok(hash),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(genesis_hash()),
            Err(e) => Err(e).context("get_last_pending_action_hash"),
        }
    }

    /// Compute a SHA-256 chain hash over the domain-separated, LENGTH-FRAMED
    /// sequence `prev_hash | action_type | actor_id | created_at | status`,
    /// under the version every new row is written at.
    ///
    /// The v1 preimage concatenated the three variable-length fields raw, so
    /// it bound their concatenation rather than their column boundaries — the
    /// same defect [`crate::db::admin::audit_on_conn`] carried, and the same
    /// fix. Both preimages and the rule selecting between them live in
    /// [`crate::db::chain_version`]; the version a row was written under is
    /// recorded in its own `chain_hash_version` column, so no reader ever has
    /// to try one format and then the other. Unlike `audit_log`, this chain
    /// has **no walker at all** — every reference is a write or a
    /// `SELECT … LIMIT 1` seeding the next row (recorded at
    /// [`crate::db::successions`]'s disarm leg).
    pub fn compute_pending_action_hash(
        prev_hash: &[u8],
        action_type: &str,
        actor_id: &[u8],
        created_at: i64,
        status: &str,
    ) -> Vec<u8> {
        chain_version::pending_action_chain_hash(
            ChainVersion::CURRENT,
            &chain_version::PendingActionPreimage {
                prev_hash,
                action_type,
                actor_id,
                created_at,
                status,
            },
        )
    }

    // ==================== Create ====================

    /// Create a new pending action. Returns the newly created row id.
    pub async fn create_pending_action(
        &self,
        action_type: &ActionType,
        actor_id: &[u8],
        target: Option<&str>,
        payload: Option<&str>,
        ip_address: Option<&str>,
    ) -> Result<i64> {
        self.create_pending_action_with_quorum(
            action_type,
            actor_id,
            target,
            payload,
            ip_address,
            action_type.requires_quorum(),
        )
        .await
    }

    /// [`Self::create_pending_action`] with the approvals the row will
    /// actually require spelled out — the person-initiated entry
    /// (`pending_actions::schedule`) passes the roster-capped
    /// [`pending_actions::effective_quorum`] rather than the type's nominal
    /// count, so a sole admin's roster grant is not born unexecutable.
    pub async fn create_pending_action_with_quorum(
        &self,
        action_type: &ActionType,
        actor_id: &[u8],
        target: Option<&str>,
        payload: Option<&str>,
        ip_address: Option<&str>,
        requires_quorum: i64,
    ) -> Result<i64> {
        let action_type_str = action_type.as_str();
        let actor_id_vec = actor_id.to_vec();
        let target = target.map(|s| s.to_string());
        let payload = payload.map(|s| s.to_string());
        let ip_address = ip_address.map(|s| s.to_string());
        let delay = action_type.delay_secs();

        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let execute_after = now + delay;

        // Compute chain hash (while still holding the lock so no concurrent insert can race)
        let prev_hash: Vec<u8> = {
            let result: rusqlite::Result<Vec<u8>> = conn.query_row(
                "SELECT chain_hash FROM pending_actions ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            );
            match result {
                Ok(h) => h,
                Err(rusqlite::Error::QueryReturnedNoRows) => genesis_hash(),
                Err(e) => return Err(e).context("get prev chain hash"),
            }
        };

        let chain_hash = Self::compute_pending_action_hash(
            &prev_hash,
            action_type_str,
            &actor_id_vec,
            now,
            chain_version::PENDING_ACTION_CREATION_STATUS,
        );

        // `chain_hash_version` records what `compute_pending_action_hash` just
        // hashed under, so a verifier selects the preimage from the row rather
        // than trying formats — see [`crate::db::chain_version`].
        conn.execute(
            "INSERT INTO pending_actions
                 (action_type, actor_id, target, payload, status, created_at, execute_after,
                  requires_quorum, approvals, ip_address, chain_hash, chain_hash_version)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7, '[]', ?8, ?9, ?10)",
            rusqlite::params![
                action_type_str,
                actor_id_vec,
                target,
                payload,
                now,
                execute_after,
                requires_quorum,
                ip_address,
                chain_hash,
                ChainVersion::CURRENT.as_i64(),
            ],
        )
        .context("insert pending_action")?;

        let row_id = conn.last_insert_rowid();
        drop(conn);

        // Write audit log entry (takes its own lock)
        let _ = self
            .audit(
                Some(actor_id),
                &format!("pending_action_created:{action_type_str}"),
                target.as_deref(),
                Some(&format!("id={row_id}")),
            )
            .await;

        Ok(row_id)
    }

    // ==================== Fetch ====================

    /// Fetch a single pending action by id.
    pub async fn get_pending_action(&self, id: i64) -> Result<Option<PendingActionRow>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT id, action_type, actor_id, target, payload, status, created_at,
                    execute_after, executed_at, cancelled_by, cancelled_at,
                    requires_quorum, approvals, ip_address, chain_hash
             FROM pending_actions WHERE id = ?1",
            rusqlite::params![id],
            parse_row,
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get_pending_action"),
        }
    }

    // ==================== List ====================

    /// List the pending actions that concern a given actor (all statuses):
    /// the ones they created, plus every admin action **against their
    /// account** ([`ActionType::is_admin_action_against_user`], matched on the
    /// hex target). The second half is what lets the target of an
    /// `admin.delete_user` see it in Settings → Pending actions and cancel it
    /// there — the cancel matrix ([`Self::cancel_pending_action`]) already
    /// admits them; the list is the surface (`ui/settings.md` § Pending
    /// actions). Roster actions naming an admin are the admin console's to
    /// list, not this method's.
    pub async fn list_pending_actions_for_actor(
        &self,
        actor_id: &[u8],
    ) -> Result<Vec<PendingActionRow>> {
        let actor_hex = hex::encode(actor_id);
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, action_type, actor_id, target, payload, status, created_at,
                    execute_after, executed_at, cancelled_by, cancelled_at,
                    requires_quorum, approvals, ip_address, chain_hash
             FROM pending_actions
             WHERE actor_id = ?1
                OR (target = ?2 AND action_type IN (?3, ?4))
             ORDER BY created_at DESC",
            )
            .context("prepare list_pending_actions_for_actor")?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    actor_id,
                    actor_hex,
                    ActionType::AdminDeleteUser.as_str(),
                    ActionType::AdminBulkDeleteUsers.as_str(),
                ],
                parse_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("query list_pending_actions_for_actor")?;
        Ok(rows)
    }

    /// List all pending actions across all actors (all statuses). Admin use only.
    pub async fn list_all_pending_actions(&self) -> Result<Vec<PendingActionRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, action_type, actor_id, target, payload, status, created_at,
                    execute_after, executed_at, cancelled_by, cancelled_at,
                    requires_quorum, approvals, ip_address, chain_hash
             FROM pending_actions
             ORDER BY created_at DESC",
            )
            .context("prepare list_all_pending_actions")?;
        let rows = stmt
            .query_map([], parse_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("query list_all_pending_actions")?;
        Ok(rows)
    }

    /// Snapshot id → the epoch second its cancellable deletion window closes,
    /// for one actor's still-`pending` actions.
    ///
    /// The read half of the mark that [`snapshot_targets_of`] describes: a
    /// snapshot row carries `deletion_pending` but not *when* it fires, because
    /// the deadline lives on the pending action. `fauna.filesync.snapshot.list`
    /// joins through this to populate `SnapshotSummaryRow::execute_after`
    /// (`backup-restore.md` § 2 *Row lifecycle fields*).
    ///
    /// It goes through the **same** [`snapshot_targets_of`] both-shapes parser
    /// the cancel path uses, which is the point: a `snapshot.bulk_prune` marks
    /// its batch through `payload.snapshot_ids` while a `snapshot.delete` marks
    /// one id in `target`, and a reader that knew only the second shape would
    /// render every automatically-pruned row as a pending deletion with no
    /// date — the state a user most needs the date for.
    ///
    /// Scoped to the set **owner** (pending actions are created by the owner,
    /// even on a set a roster member is reading). Several actions naming one
    /// snapshot cannot arise today — the mark is what makes both producers
    /// skip it — but the **earliest** deadline is taken regardless, since that
    /// is the one that would actually fire.
    pub async fn pending_snapshot_deletion_deadlines(
        &self,
        actor_id: &[u8],
    ) -> Result<std::collections::HashMap<i64, i64>> {
        let actions = self.list_pending_actions_for_actor(actor_id).await?;
        let mut out: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
        for action in actions.iter().filter(|a| a.status == "pending") {
            for id in snapshot_targets_of(action) {
                out.entry(id)
                    .and_modify(|e| *e = (*e).min(action.execute_after))
                    .or_insert(action.execute_after);
            }
        }
        Ok(out)
    }

    /// List all pending actions where status='pending' AND execute_after <= now.
    /// These are ready to be executed by the background executor.
    pub async fn list_ready_pending_actions(&self) -> Result<Vec<PendingActionRow>> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let mut stmt = conn
            .prepare(
                "SELECT id, action_type, actor_id, target, payload, status, created_at,
                    execute_after, executed_at, cancelled_by, cancelled_at,
                    requires_quorum, approvals, ip_address, chain_hash
             FROM pending_actions
             WHERE status = 'pending' AND execute_after <= ?1
             ORDER BY execute_after ASC",
            )
            .context("prepare list_ready_pending_actions")?;
        let rows = stmt
            .query_map(rusqlite::params![now], parse_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("query list_ready_pending_actions")?;
        Ok(rows)
    }

    // ==================== Cancel ====================

    /// Cancel a pending action.
    ///
    /// See [`snapshot_targets_of`] for the snapshot rows a cancel must also
    /// un-mark.
    ///
    /// Authorization matrix:
    /// - User actions (HandleChange, SnapshotDelete, SnapshotBulkPrune, AccountDelete):
    ///   only the creator (`actor_id`) may cancel.
    /// - Admin actions against users (AdminDeleteUser, AdminBulkDeleteUsers):
    ///   the creator, the target user, or any admin may cancel.
    /// - Admin management actions (AdminAdd, AdminRemove, AdminChangeRole, AdminBackupPurgeOverride):
    ///   any admin may cancel.
    ///
    /// `cancelled_by` is the actor_id of the canceller.
    pub async fn cancel_pending_action(&self, id: i64, cancelled_by: &[u8]) -> Result<()> {
        // Fetch the action first (takes its own lock)
        let action = self
            .get_pending_action(id)
            .await?
            .with_context(|| format!("pending action {id} not found"))?;

        if action.status != "pending" {
            bail!(
                "pending action {id} is not cancellable (status={})",
                action.status
            );
        }

        let action_type = ActionType::from_str(&action.action_type)
            .with_context(|| format!("unknown action_type: {}", action.action_type))?;

        // Authorization check
        let is_creator = action.actor_id == cancelled_by;

        let authorized = if action_type.is_admin_management() {
            // Any admin can cancel
            self.is_admin(cancelled_by).await.unwrap_or(false)
        } else if action_type.is_admin_action_against_user() {
            // Creator, target user, or any admin
            let is_target = action
                .target
                .as_deref()
                .map(|t| {
                    // target may be stored as hex; compare raw bytes via hex decode
                    hex::decode(t)
                        .ok()
                        .map(|b| b == cancelled_by)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            is_creator || is_target || self.is_admin(cancelled_by).await.unwrap_or(false)
        } else {
            // User actions — only the creator
            is_creator
        };

        if !authorized {
            bail!("not authorized to cancel pending action {id}");
        }

        let cancelled_by_vec = cancelled_by.to_vec();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let updated = conn
            .execute(
                "UPDATE pending_actions
             SET status = 'cancelled', cancelled_by = ?1, cancelled_at = ?2
             WHERE id = ?3 AND status = 'pending'",
                rusqlite::params![cancelled_by_vec, now, id],
            )
            .context("cancel pending_action")?;

        if updated == 0 {
            bail!("pending action {id} was already cancelled or executed");
        }

        // A snapshot-deletion action marked its targets `deletion_pending` when
        // it was created (`filesync_handlers::delete_handler`,
        // `backup::prune::schedule_auto_prune`). Cancelling is the user saying
        // *"keep them"*, so the mark has to come off in the same transaction —
        // nothing else ever clears it (`soft_delete_snapshot` /
        // `undelete_snapshot` both act on a snapshot already past this stage).
        // Left set, the snapshot stays invisible to `count_active_snapshots`
        // forever: it is silently excluded from its own folder's § 7 Layer-1
        // hard floor, and from the auto-pruner's candidate population, on the
        // strength of a deletion the user explicitly called off.
        for snapshot_id in snapshot_targets_of(&action) {
            conn.execute(
                "UPDATE snapshots SET deletion_pending = 0 WHERE id = ?1",
                rusqlite::params![snapshot_id],
            )
            .context("clear deletion_pending on cancel")?;
        }

        // The version-plane twin: a `version.bulk_prune` action marked its
        // target rows `prune_pending` at creation
        // (`backup::version_prune::schedule_version_auto_prune`). Cancelling is
        // the user saying *"keep my versions"* — left set, the rows are
        // silently excluded from the evaluator's population forever, on the
        // strength of a prune the user explicitly called off.
        for seq in version_targets_from(&action.action_type, action.payload.as_deref()) {
            conn.execute(
                "UPDATE sync_changes SET prune_pending = NULL WHERE seq = ?1",
                rusqlite::params![seq],
            )
            .context("clear prune_pending on cancel")?;
        }

        drop(conn);

        let _ = self
            .audit(
                Some(cancelled_by),
                &format!("pending_action_cancelled:{}", action.action_type),
                action.target.as_deref(),
                Some(&format!("id={id}")),
            )
            .await;

        Ok(())
    }

    // ==================== Approve ====================

    /// Add an approval to a pending action.
    ///
    /// Rules:
    /// - Self-approval (approver == actor_id) is blocked.
    /// - Duplicate approvals (same approver_hex already in the JSON array) are ignored.
    /// - The action must still be in 'pending' status.
    pub async fn approve_pending_action(&self, id: i64, approver: &[u8]) -> Result<()> {
        let action = self
            .get_pending_action(id)
            .await?
            .with_context(|| format!("pending action {id} not found"))?;

        if action.status != "pending" {
            bail!("pending action {id} is not in pending status");
        }

        if action.actor_id == approver {
            bail!("self-approval is not allowed for pending action {id}");
        }

        let approver_hex = hex::encode(approver);

        // Parse existing approvals
        let mut approvals: Vec<String> =
            serde_json::from_str(&action.approvals).unwrap_or_default();

        if approvals.contains(&approver_hex) {
            // Duplicate — silently succeed
            return Ok(());
        }

        approvals.push(approver_hex);
        let new_approvals = serde_json::to_string(&approvals).context("serialize approvals")?;

        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE pending_actions SET approvals = ?1 WHERE id = ?2 AND status = 'pending'",
            rusqlite::params![new_approvals, id],
        )
        .context("approve pending_action")?;

        Ok(())
    }

    // ==================== Status transitions ====================

    /// Claim a ready action for the executor: `pending` → `executing`, in one
    /// conditional write. Returns the row as the claim left it, or `None` when
    /// it is no longer `pending` — a cancel, a succession disarm or another
    /// transition landed after the executor's batch read, and that one wins
    /// (`nest/common.md` § Pending Actions System → *The executor claims
    /// before it acts*). Every later step acts on the returned row, never on
    /// the batch read's copy.
    pub async fn claim_pending_action(&self, id: i64) -> Result<Option<PendingActionRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "UPDATE pending_actions SET status = 'executing'
              WHERE id = ?1 AND status = 'pending'
              RETURNING id, action_type, actor_id, target, payload, status, created_at,
                    execute_after, executed_at, cancelled_by, cancelled_at,
                    requires_quorum, approvals, ip_address, chain_hash",
            rusqlite::params![id],
            parse_row,
        )
        .optional()
        .context("claim_pending_action")
    }

    /// Hand a claimed action back to the queue after its run failed:
    /// `executing` → `pending`, so the next tick retries it. A row something
    /// else moved meanwhile (the succession disarm) is left as it is.
    pub async fn release_pending_action_claim(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE pending_actions SET status = 'pending'
              WHERE id = ?1 AND status = 'executing'",
            rusqlite::params![id],
        )
        .context("release_pending_action_claim")?;
        Ok(())
    }

    /// Boot reconcile: return every row a crash left `executing` to `pending`,
    /// so the executor retries it rather than stranding it. Runs before the
    /// executor starts, when no claim can be live. Returns how many it moved.
    pub async fn requeue_stranded_pending_actions(&self) -> Result<usize> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE pending_actions SET status = 'pending' WHERE status = 'executing'",
            [],
        )
        .context("requeue_stranded_pending_actions")
    }

    /// Mark a claimed action executed (called by the background executor).
    /// Moves only an `executing` row, so it never overwrites a terminal
    /// status; returns `false` when the row was no longer `executing` — or no
    /// longer exists, because an account deletion's run purged its own row.
    pub async fn mark_pending_action_executed(&self, id: i64) -> Result<bool> {
        self.finish_claimed_action(id, "executed").await
    }

    /// Mark a claimed action expired (its quorum fell short). Moves only an
    /// `executing` row, as [`Self::mark_pending_action_executed`].
    pub async fn mark_pending_action_expired(&self, id: i64) -> Result<bool> {
        self.finish_claimed_action(id, "expired").await
    }

    async fn finish_claimed_action(&self, id: i64, terminal: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let updated = conn
            .execute(
                "UPDATE pending_actions SET status = ?1, executed_at = ?2
                  WHERE id = ?3 AND status = 'executing'",
                rusqlite::params![terminal, now, id],
            )
            .with_context(|| format!("mark pending action {terminal}"))?;
        Ok(updated == 1)
    }

    // ==================== Test helpers ====================

    /// Override `execute_after` for a pending action.
    /// Used in integration tests to simulate time passing without sleeping.
    #[doc(hidden)]
    pub async fn test_set_execute_after(&self, id: i64, execute_after: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE pending_actions SET execute_after = ?1 WHERE id = ?2",
            rusqlite::params![execute_after, id],
        )
        .context("test_set_execute_after")?;
        Ok(())
    }
}

/// Parse a single `pending_actions` row.
fn parse_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingActionRow> {
    Ok(PendingActionRow {
        id: row.get(0)?,
        action_type: row.get(1)?,
        actor_id: row.get(2)?,
        target: row.get(3)?,
        payload: row.get(4)?,
        status: row.get(5)?,
        created_at: row.get(6)?,
        execute_after: row.get(7)?,
        executed_at: row.get(8)?,
        cancelled_by: row.get(9)?,
        cancelled_at: row.get(10)?,
        requires_quorum: row.get(11)?,
        approvals: row.get(12)?,
        ip_address: row.get(13)?,
        chain_hash: row.get(14)?,
    })
}
