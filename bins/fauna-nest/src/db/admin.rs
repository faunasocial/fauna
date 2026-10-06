//! Admin, session, tier, user, eviction, handle, invite code, quota, and audit methods.

use super::{AuditRow, InviteCodeRow, InviteRequestRow, Stats, TierRow, UserRow};
use super::{CacheDb, now_epoch_millis, now_epoch_secs};
use crate::db::chain_version::{self, ChainVersion};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

/// The authority-relevant columns of a `users` row, read in one query by the
/// central dispatch gate ([`crate::bridge_method_allowlist::caller_class_for_actor`]).
///
/// The *absence* of the row is itself an answer, so [`CacheDb::actor_authority`]
/// returns `Option<ActorAuthority>`: `None` means no `users` row — an unknown or
/// deleted actor — which the gate maps to "no caller class at all".
///
/// The two fields are the two ways a *known* actor loses its authority:
/// `suspended` (the admin cut-off, indefinite, exited only by
/// `fauna.admin.users.cancel_eviction`) and `locked_until` (the emergency
/// `fauna.sessions.lockout`, timed, lapsing on its own).
///
/// They are read *together* on purpose. "May this actor act right now?" is one
/// question; answering it in two queries lets the two answers disagree across a
/// concurrent write, and historically let lockout be enforced at token mint only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorAuthority {
    pub suspended: bool,
    /// Unix **seconds**; `None` when never locked or the lock was cleared.
    pub locked_until: Option<i64>,
}

impl ActorAuthority {
    /// Whether this actor's authority is revoked at `now_secs` — the single
    /// predicate the dispatch gate asks. A lapsed lockout (`locked_until` in the
    /// past) is not a revocation: the lock expires without anyone clearing it.
    pub fn is_revoked_at(&self, now_secs: i64) -> bool {
        self.suspended || self.locked_until.is_some_and(|until| until > now_secs)
    }
}

/// Outcome of a roster-shrinking write ([`CacheDb::remove_admin_actor`],
/// [`CacheDb::set_admin_role`]) — forces every caller to face the superadmin
/// floor explicitly instead of reading a bool that conflates "done" with
/// "refused". The floor: at least one superadmin always remains, enforced at
/// the writer itself because the scheduling door's check is schedule-time-only
/// on a 24 h-delayed action (`admin.md` § 2 Users → *Cutting a user off*,
/// ratified 2026-08-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterWrite {
    /// The write landed.
    Applied,
    /// The target holds no admin row — nothing to do. Callers treat this as
    /// idempotent completion (a replayed removal must not retry forever).
    NotAnAdmin,
    /// The write would have left zero superadmins and was refused. The
    /// pending-action executor parks the action pending-and-retryable (the
    /// `admin.add` posture); adding another superadmin unblocks it.
    RefusedLastSuperadmin,
}

/// The superadmin floor, asked with the write lock already held: does no
/// superadmin other than `target` exist? True means removing or demoting
/// `target` would empty the tier — the write must be refused.
fn no_other_superadmin(conn: &rusqlite::Connection, target: &[u8]) -> rusqlite::Result<bool> {
    let others: i64 = conn.query_row(
        "SELECT COUNT(*) FROM admin_actor_ids WHERE role = 'superadmin' AND actor_id != ?1",
        rusqlite::params![target],
        |row| row.get(0),
    )?;
    Ok(others == 0)
}

/// The `audit_log` rows this binary can verify, split by the preimage version
/// each row records — see [`CacheDb::audit_entry_version_census`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuditVersionCensus {
    /// Rows recording the length-framed preimage.
    pub v2: i64,
    /// Rows this binary will not verify: the version is one it does not
    /// implement. Counted, never guessed at.
    pub unverifiable: i64,
}

/// Insert an audit log entry on an already-held connection (or transaction —
/// [`rusqlite::Transaction`] derefs to [`rusqlite::Connection`]). The sync core
/// of [`CacheDb::audit`], split out so multi-write operations (e.g. the
/// legal-takedown flag + obligation + audit transaction) can chain the audit
/// row into their own transaction instead of committing it separately.
///
/// **An audit row never stores a bearer credential** in `target` or `detail` —
/// no invite code, token, password, or key. Every row keyed to an actor rides
/// that actor's own account export (the `audit_log` verdict in
/// `db::actor_tables`), which is retrievable with an eviction export token, so
/// a credential written here is a new resting place for it that outlives the
/// writer's removal. Store a keyed fingerprint instead
/// (`crate::admin::invite_code_audit_fingerprint`).
pub(crate) fn audit_on_conn(
    conn: &rusqlite::Connection,
    actor_id: Option<&[u8]>,
    action: &str,
    target: Option<&str>,
    detail: Option<&str>,
) -> Result<()> {
    let now = now_epoch_millis();

    // Get the previous entry's hash (or genesis)
    let prev_hash: String = conn
        .query_row(
            "SELECT entry_hash FROM audit_log ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(b"fauna-audit-genesis-v1"))
        });

    // Insert first (with empty entry_hash) to get the autoincrement id.
    // `entry_hash_version` is written in the same statement rather than left to
    // a column default: the version a row was hashed under is a fact its
    // writer knows, and the whole point of the column is that no later reader
    // has to infer it.
    conn.execute(
        "INSERT INTO audit_log \
             (ts, actor_id, action, target, detail, prev_hash, entry_hash, entry_hash_version) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7)",
        rusqlite::params![
            now,
            actor_id,
            action,
            target,
            detail,
            prev_hash,
            ChainVersion::CURRENT.as_i64()
        ],
    )
    .context("insert audit log")?;
    let row_id = conn.last_insert_rowid();

    // entry_hash = SHA256 over the domain-separated, LENGTH-FRAMED field
    // sequence. The framing is the whole point: a preimage that concatenated
    // `actor_id`, `action`, `target` and `detail` raw would bind their
    // CONCATENATION and not their column boundaries — bytes moved across a
    // boundary (`action="role.grant.superadmin", target="mallory"` re-split as
    // `action="role.grant", target=".superadminmallory"`) would recompute to
    // the identical hash, and the admin verification `succession-aftermath.md`
    // leans on would find nothing. The chain LINKS are separate — each row's
    // `prev_hash` is the previous row's stored `entry_hash`.
    //
    // The hash goes through [`chain_version::audit_entry_hash`] rather than
    // being spelled out here, so this writer and any verifier are the same
    // code reading the same recorded version.
    let entry_hash = chain_version::audit_entry_hash(
        ChainVersion::CURRENT,
        &chain_version::AuditPreimage {
            id: row_id,
            ts: now,
            actor_id,
            action,
            target,
            detail,
            prev_hash: &prev_hash,
        },
    );

    // Update with computed hash
    conn.execute(
        "UPDATE audit_log SET entry_hash = ?1 WHERE id = ?2",
        rusqlite::params![entry_hash, row_id],
    )
    .context("update audit entry_hash")?;
    Ok(())
}

impl CacheDb {
    // ==================== Admin Actor IDs ====================

    /// Add an actor_id as an admin.
    pub async fn add_admin_actor(&self, actor_id: &[u8]) -> Result<()> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT OR IGNORE INTO admin_actor_ids (actor_id, added_at) VALUES (?1, ?2)",
            rusqlite::params![actor_id, now],
        )
        .context("add admin actor")?;
        Ok(())
    }

    /// Remove an actor_id from the admin list — **floor-guarded**: the write
    /// that would delete the last superadmin is refused here, atomically with
    /// the write (every `CacheDb` call serializes on `self.conn`, so the check
    /// and the delete cannot interleave with another roster write). The
    /// scheduling door's synchronous 409 is a courtesy a 24 h-delayed action
    /// can outrun; this is the line that holds
    /// (`admin.md` § 2 Users → *Cutting a user off*).
    pub async fn remove_admin_actor(&self, actor_id: &[u8]) -> Result<RosterWrite> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let role: Option<String> = conn
            .query_row(
                "SELECT role FROM admin_actor_ids WHERE actor_id = ?1",
                rusqlite::params![actor_id],
                |row| row.get(0),
            )
            .optional()
            .context("read admin role before removal")?;
        let Some(role) = role else {
            return Ok(RosterWrite::NotAnAdmin);
        };
        if role == "superadmin"
            && no_other_superadmin(&conn, &actor_id).context("count remaining superadmins")?
        {
            return Ok(RosterWrite::RefusedLastSuperadmin);
        }
        conn.execute(
            "DELETE FROM admin_actor_ids WHERE actor_id = ?1",
            rusqlite::params![actor_id],
        )
        .context("remove admin actor")?;
        Ok(RosterWrite::Applied)
    }

    /// Check if an actor_id is an admin.
    pub async fn is_admin(&self, actor_id: &[u8]) -> Result<bool> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM admin_actor_ids WHERE actor_id = ?1",
                rusqlite::params![actor_id],
                |row| row.get(0),
            )
            .context("is_admin")?;
        Ok(count > 0)
    }

    /// The local predecessors of `actor_id` (`successions::local_predecessors`)
    /// that hold an `admin_actor_ids` row.
    ///
    /// A deletion of `actor_id` takes its whole local chain with it — the
    /// purge walk drops each predecessor's `admin_actor_ids` row
    /// (`Policy::Purge`) and `delete_user` its `users` row — so an admin row
    /// held by a predecessor would leave without `admin.remove`'s quorum or the
    /// superadmin floor (`can_remove_admin`). `finalize_user_deletion` and the
    /// deletion doors refuse while this is non-empty, the chain-wide face of
    /// their `is_admin(actor_id)` refusal. Succession moves the role to the
    /// successor, so a predecessor holds one only through a grant made before
    /// the add door refused retired keys.
    pub async fn local_predecessors_holding_admin(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let chain = super::successions::local_predecessors(&conn, actor_id)?;
        let mut stmt = conn
            .prepare("SELECT 1 FROM admin_actor_ids WHERE actor_id = ?1")
            .context("prepare predecessor admin check")?;
        let mut holders = Vec::new();
        for id in chain {
            if stmt
                .exists(rusqlite::params![&id[..]])
                .context("predecessor admin check")?
            {
                holders.push(id);
            }
        }
        Ok(holders)
    }

    /// List all admin actor_ids with their added_at timestamps.
    pub async fn list_admin_actors(&self) -> Result<Vec<(Vec<u8>, i64)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT actor_id, added_at FROM admin_actor_ids ORDER BY added_at")
            .context("prepare list admin actors")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .context("query admin actors")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read admin actor row")?);
        }
        Ok(results)
    }

    /// Count the number of admin actors.
    pub async fn admin_count(&self) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT COUNT(*) FROM admin_actor_ids", [], |row| row.get(0))
            .context("admin_count")
    }

    /// Get the role for an admin actor. Returns None if the actor is not an admin.
    pub async fn get_admin_role(&self, actor_id: &[u8]) -> Result<Option<String>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT role FROM admin_actor_ids WHERE actor_id = ?1",
            rusqlite::params![actor_id],
            |row| row.get(0),
        );
        match result {
            Ok(role) => Ok(Some(role)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get_admin_role"),
        }
    }

    /// Set the role for an admin actor — floor-guarded like
    /// [`Self::remove_admin_actor`]: demoting the last superadmin empties the
    /// tier exactly as deleting them would, so that write is refused here.
    /// Promotions, and any write on a non-superadmin, pass untouched. This is
    /// an UPDATE — it never mints an admin row (`NotAnAdmin` for a stranger).
    pub async fn set_admin_role(&self, actor_id: &[u8], role: &str) -> Result<RosterWrite> {
        let actor_id = actor_id.to_vec();
        let role = role.to_string();
        let conn = self.conn.lock().await;
        let current: Option<String> = conn
            .query_row(
                "SELECT role FROM admin_actor_ids WHERE actor_id = ?1",
                rusqlite::params![actor_id],
                |row| row.get(0),
            )
            .optional()
            .context("read admin role before role change")?;
        let Some(current) = current else {
            return Ok(RosterWrite::NotAnAdmin);
        };
        if current == "superadmin"
            && role != "superadmin"
            && no_other_superadmin(&conn, &actor_id).context("count remaining superadmins")?
        {
            return Ok(RosterWrite::RefusedLastSuperadmin);
        }
        conn.execute(
            "UPDATE admin_actor_ids SET role = ?1 WHERE actor_id = ?2",
            rusqlite::params![role, actor_id],
        )
        .context("set_admin_role")?;
        Ok(RosterWrite::Applied)
    }

    /// Count admins with a specific role.
    pub async fn admin_count_by_role(&self, role: &str) -> Result<i64> {
        let role = role.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM admin_actor_ids WHERE role = ?1",
            rusqlite::params![role],
            |row| row.get(0),
        )
        .context("admin_count_by_role")
    }

    /// The scheduling door's synchronous half of the superadmin floor: may
    /// `target` be removed (or demoted) right now without leaving zero
    /// superadmins? **Target-aware** — a non-superadmin admin, or a
    /// non-admin, never threatens the floor. Advisory by design: the 24 h
    /// action delay means the roster can change before execution, which is
    /// why the writers above are the authority and this read only shapes the
    /// door's immediate 409. (Replaced the global-count
    /// `can_remove_superadmin`, which ignored the target and so refused
    /// legitimate non-superadmin removals while missing the delayed-execution
    /// hole — row 119.)
    pub async fn can_remove_admin(&self, target: &[u8]) -> Result<bool> {
        let target = target.to_vec();
        let conn = self.conn.lock().await;
        let role: Option<String> = conn
            .query_row(
                "SELECT role FROM admin_actor_ids WHERE actor_id = ?1",
                rusqlite::params![target],
                |row| row.get(0),
            )
            .optional()
            .context("read admin role for removal check")?;
        match role.as_deref() {
            Some("superadmin") => {
                Ok(!no_other_superadmin(&conn, &target).context("count remaining superadmins")?)
            }
            _ => Ok(true),
        }
    }

    /// Suspend a user **now** — the immediate arm of the eviction state machine.
    ///
    /// Suspension is not a flag of its own: it is the machine's `suspended`
    /// state with **no delete timeline**. `eviction_delete_at` stays NULL, so
    /// `transition_evictions`' phase-2 predicate (`eviction_delete_at <= now`)
    /// never fires and a suspension never auto-deletes. Because the row carries
    /// `eviction_status = 'suspended'`, `cancel_eviction` — already reachable
    /// from the admin UI's `admin-users-cancel-eviction-button` — restores the
    /// user, so a suspended user is always recoverable *by a client*
    /// (`docs/goal/architecture/nest/common.md` § Client-state recoverability).
    /// Keeping `suspended` single-writer is what makes the unrecoverable
    /// `suspended = 1, eviction_status = ''` half-state unrepresentable.
    ///
    /// Accepted from `eviction_status IN ('', 'warning')`: suspending a user who
    /// is mid-eviction promotes them immediately and clears the pending delete
    /// timer (the safe direction — nothing is destroyed; the admin re-runs
    /// evict to schedule deletion). Returns false when no row matched — an
    /// unknown user, or one already suspended.
    pub async fn suspend_user_now(
        &self,
        actor_id: &[u8],
        reason: &str,
        category: &str,
    ) -> Result<bool> {
        let actor_id = actor_id.to_vec();
        let reason = reason.to_string();
        let category = category.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let updated = conn
            .execute(
                "UPDATE users SET eviction_status = 'suspended', suspended = 1,
                 eviction_reason = ?1, eviction_category = ?2,
                 eviction_warned_at = ?3, eviction_suspend_at = ?3,
                 eviction_delete_at = NULL
                 WHERE actor_id = ?4 AND eviction_status IN ('', 'warning')",
                rusqlite::params![reason, category, now, actor_id],
            )
            .context("suspend_user_now")?;
        Ok(updated > 0)
    }

    /// Set the locked_until timestamp for a user (unix seconds). Use None to clear.
    pub async fn set_locked_until(&self, actor_id: &[u8], locked_until: Option<i64>) -> Result<()> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE users SET locked_until = ?1 WHERE actor_id = ?2",
            rusqlite::params![locked_until, actor_id],
        )
        .context("set_locked_until")?;
        Ok(())
    }

    /// Get the locked_until timestamp for a user. Returns None if not locked.
    pub async fn get_locked_until(&self, actor_id: &[u8]) -> Result<Option<i64>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT locked_until FROM users WHERE actor_id = ?1",
            rusqlite::params![actor_id],
            |row| row.get(0),
        );
        match result {
            Ok(val) => Ok(val),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get_locked_until"),
        }
    }

    /// Returns true if at least one user row exists in the users table.
    pub async fn has_any_user(&self) -> Result<bool> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users LIMIT 1", [], |row| row.get(0))
            .context("has_any_user")?;
        Ok(count > 0)
    }

    // ==================== Actor Last IP ====================

    /// UPSERT the last-seen IP address for an actor.
    ///
    /// Returns `true` if the IP **changed** from a previously recorded value
    /// (i.e. this is not the first insert and the IP differs from what was stored).
    /// Returns `false` on first insert or when the IP is unchanged.
    pub async fn update_actor_last_ip(&self, actor_id: &[u8], ip: &str) -> Result<bool> {
        let actor_id = actor_id.to_vec();
        let ip = ip.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();

        // Read the existing IP (if any) before upserting.
        let existing: Option<String> = conn
            .query_row(
                "SELECT ip_address FROM actor_last_ip WHERE actor_id = ?1",
                rusqlite::params![actor_id],
                |row| row.get(0),
            )
            .optional()
            .context("read actor_last_ip")?;

        conn.execute(
            "INSERT INTO actor_last_ip (actor_id, ip_address, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET ip_address = excluded.ip_address, updated_at = excluded.updated_at",
            rusqlite::params![actor_id, ip, now],
        )
        .context("upsert actor_last_ip")?;

        // Changed = had a previous value AND it differs from the new value.
        Ok(matches!(existing, Some(prev) if prev != ip))
    }

    // ==================== Tiers ====================

    /// List all tier definitions.
    pub async fn list_tiers(&self) -> Result<Vec<TierRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT name, max_inbox_bytes, max_storage_bytes, max_devices, max_blob_size, max_feeds FROM tiers ORDER BY name")
            .context("prepare list tiers")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(TierRow {
                    name: row.get(0)?,
                    max_inbox_bytes: row.get(1)?,
                    max_storage_bytes: row.get(2)?,
                    max_devices: row.get(3)?,
                    max_blob_size: row.get(4)?,
                    max_feeds: row.get(5)?,
                })
            })
            .context("query tiers")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read tier row")?);
        }
        Ok(results)
    }

    /// Get a tier by name.
    pub async fn get_tier(&self, name: &str) -> Result<Option<TierRow>> {
        let name = name.to_string();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT name, max_inbox_bytes, max_storage_bytes, max_devices, max_blob_size, max_feeds FROM tiers WHERE name = ?1",
            rusqlite::params![name],
            |row| {
                Ok(TierRow {
                    name: row.get(0)?,
                    max_inbox_bytes: row.get(1)?,
                    max_storage_bytes: row.get(2)?,
                    max_devices: row.get(3)?,
                    max_blob_size: row.get(4)?,
                    max_feeds: row.get(5)?,
                })
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get tier"),
        }
    }

    /// Update a tier's limits.
    pub async fn update_tier(&self, tier: &TierRow) -> Result<bool> {
        let tier = tier.clone();
        let conn = self.conn.lock().await;
        let updated = conn
            .execute(
                "UPDATE tiers SET max_inbox_bytes = ?1, max_storage_bytes = ?2, max_devices = ?3, max_blob_size = ?4, max_feeds = ?5 WHERE name = ?6",
                rusqlite::params![tier.max_inbox_bytes, tier.max_storage_bytes, tier.max_devices, tier.max_blob_size, tier.max_feeds, tier.name],
            )
            .context("update tier")?;
        Ok(updated > 0)
    }

    /// Create a new tier. Returns an error if the name already exists.
    pub async fn create_tier(&self, tier: &TierRow) -> Result<()> {
        let tier = tier.clone();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO tiers (name, max_inbox_bytes, max_storage_bytes, max_devices, max_blob_size, max_feeds) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![tier.name, tier.max_inbox_bytes, tier.max_storage_bytes, tier.max_devices, tier.max_blob_size, tier.max_feeds],
        ).context("create tier")?;
        Ok(())
    }

    /// Look up the max_feeds limit for the tier assigned to the given user.
    pub async fn get_user_tier_max_feeds(&self, actor_id: &[u8; 32]) -> Result<i64> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT t.max_feeds FROM users u JOIN tiers t ON u.tier = t.name WHERE u.actor_id = ?1",
            rusqlite::params![actor_id],
            |row| row.get(0),
        )
        .context("get user tier max_feeds")
    }

    /// Look up the `max_devices` cap for the tier assigned to the given user —
    /// the per-actor device quota `fauna.sync.register` enforces (the tier *is*
    /// the quota; `docs/goal/behavior/admin.md` § 2 Users). Mirrors
    /// [`Self::get_user_tier_max_feeds`] deliberately: both are hard caps on a
    /// row count, both refuse at `count >= max`, and `0` means *none* in both
    /// (the shipped `backup` tier's `max_feeds = 0`, `db/migrations.rs`
    /// `SEED_TIERS`, is what fixes that reading — an admin's "no
    /// allowance", never "unlimited").
    ///
    /// Errors when the actor has no `users` row, and the caller refuses rather
    /// than falling open — the feed cap's shape, and safe here because every
    /// actor reaching a handler has that row (`caller_class_for_actor` resolves
    /// no class without it).
    pub async fn get_user_tier_max_devices(&self, actor_id: &[u8; 32]) -> Result<i64> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT t.max_devices FROM users u JOIN tiers t ON u.tier = t.name WHERE u.actor_id = ?1",
            rusqlite::params![actor_id],
            |row| row.get(0),
        )
        .context("get user tier max_devices")
    }

    /// Look up the `max_storage_bytes` cap for the tier assigned to the given
    /// user — the per-actor storage quota the manifest-record / backup-custody
    /// paths enforce (the tier *is* the quota; `docs/goal/behavior/admin.md`
    /// § 2 Users). `None` when the actor has no `users` row: a folder owner is
    /// always a registered user in production (creating a set requires the `User`
    /// class), so the metering callers treat `None` as "no cap" (fail open),
    /// preserving the unmetered behaviour for that can't-happen edge rather than
    /// erroring the record path.
    pub async fn get_user_tier_max_storage_bytes(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<i64>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT t.max_storage_bytes FROM users u JOIN tiers t ON u.tier = t.name \
             WHERE u.actor_id = ?1",
            rusqlite::params![actor_id],
            |row| row.get(0),
        )
        .optional()
        .context("get user tier max_storage_bytes")
    }

    /// The actor's storage meter and tier ceiling in one read —
    /// `(users.storage_bytes_used, tier max_storage_bytes)` — the pair the
    /// metered record path compares (`record_sync_change_metered`), reported
    /// over WebDAV as RFC 4331 quota. `None` when the actor has no `users` row;
    /// the ceiling is `None` when the actor's tier row is missing, the same
    /// fail-open [`Self::get_user_tier_max_storage_bytes`] documents.
    pub async fn get_user_storage_quota(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<(i64, Option<i64>)>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT u.storage_bytes_used, t.max_storage_bytes FROM users u \
             LEFT JOIN tiers t ON u.tier = t.name WHERE u.actor_id = ?1",
            rusqlite::params![actor_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("get user storage quota")
    }

    // ==================== Users ====================

    /// List all registered users, in [`Self::list_users_paginated`]'s order.
    pub async fn list_users(&self) -> Result<Vec<UserRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT actor_id, tier, label, suspended, created_at,
                        inbox_bytes_used, storage_bytes_used,
                        eviction_status, eviction_reason, eviction_category,
                        eviction_warned_at, eviction_suspend_at, eviction_delete_at, handle
                 FROM users ORDER BY created_at DESC, rowid DESC",
            )
            .context("prepare list users")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(UserRow {
                    actor_id: row.get(0)?,
                    tier: row.get(1)?,
                    label: row.get(2)?,
                    suspended: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                    inbox_bytes_used: row.get(5)?,
                    storage_bytes_used: row.get(6)?,
                    eviction_status: row.get(7)?,
                    eviction_reason: row.get(8)?,
                    eviction_category: row.get(9)?,
                    eviction_warned_at: row.get(10)?,
                    eviction_suspend_at: row.get(11)?,
                    eviction_delete_at: row.get(12)?,
                    handle: row.get(13)?,
                })
            })
            .context("query users")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read user row")?);
        }
        Ok(results)
    }

    /// List registered users with pagination. Returns (users, total_count).
    ///
    /// Newest first, and accounts created in the same second (`created_at` is
    /// second-granular) newest-inserted first. Without the `rowid` tiebreak
    /// SQLite leaves ties unordered, so consecutive offsets could repeat one
    /// account and skip another — and the admin actor pickers page through this
    /// list to its total (`admin.md` § Where logic lives).
    pub async fn list_users_paginated(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<UserRow>, i64)> {
        let conn = self.conn.lock().await;
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .context("count users")?;
        let mut stmt = conn
            .prepare(
                "SELECT actor_id, tier, label, suspended, created_at,
                        inbox_bytes_used, storage_bytes_used,
                        eviction_status, eviction_reason, eviction_category,
                        eviction_warned_at, eviction_suspend_at, eviction_delete_at, handle
                 FROM users ORDER BY created_at DESC, rowid DESC LIMIT ?1 OFFSET ?2",
            )
            .context("prepare list users paginated")?;
        let rows = stmt
            .query_map(rusqlite::params![limit, offset], |row| {
                Ok(UserRow {
                    actor_id: row.get(0)?,
                    tier: row.get(1)?,
                    label: row.get(2)?,
                    suspended: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                    inbox_bytes_used: row.get(5)?,
                    storage_bytes_used: row.get(6)?,
                    eviction_status: row.get(7)?,
                    eviction_reason: row.get(8)?,
                    eviction_category: row.get(9)?,
                    eviction_warned_at: row.get(10)?,
                    eviction_suspend_at: row.get(11)?,
                    eviction_delete_at: row.get(12)?,
                    handle: row.get(13)?,
                })
            })
            .context("query users paginated")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read user row")?);
        }
        Ok((results, total))
    }

    /// Create a registered user.
    pub async fn create_user(&self, actor_id: &[u8; 32], tier: &str, label: &str) -> Result<()> {
        let actor_id = *actor_id;
        let tier = tier.to_string();
        let label = label.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT INTO users (actor_id, tier, label, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![actor_id.as_slice(), tier, label, now],
        )
        .context("create user")?;
        Ok(())
    }

    /// Update a user's tier and label.
    pub async fn update_user(&self, actor_id: &[u8; 32], tier: &str, label: &str) -> Result<bool> {
        let actor_id = *actor_id;
        let tier = tier.to_string();
        let label = label.to_string();
        let conn = self.conn.lock().await;
        let updated = conn
            .execute(
                "UPDATE users SET tier = ?1, label = ?2 WHERE actor_id = ?3",
                rusqlite::params![tier, label, actor_id.as_slice()],
            )
            .context("update user")?;
        Ok(updated > 0)
    }

    /// Assign a user's quota tier, leaving the label untouched — the membership
    /// admission/renewal primitive (monetization.md § Pillar 4 Rail C step 2:
    /// "admission at membership tier M assigns `users.tier = link(M).admin_tier`").
    /// Distinct from [`update_user`](Self::update_user), which also rewrites the
    /// label: a payment must never clobber an admin-set label. Returns `false`
    /// when no such user exists. `tier` must name a real `tiers` row (the
    /// `users.tier` FK enforces it; membership admission validated the designation
    /// at `set` time, so the linked `admin_tier` always exists).
    pub async fn set_user_tier(&self, actor_id: &[u8; 32], tier: &str) -> Result<bool> {
        let actor_id = *actor_id;
        let tier = tier.to_string();
        let conn = self.conn.lock().await;
        let updated = conn
            .execute(
                "UPDATE users SET tier = ?1 WHERE actor_id = ?2",
                rusqlite::params![tier, actor_id.as_slice()],
            )
            .context("set user tier")?;
        Ok(updated > 0)
    }

    /// Delete a registered user — the account row of `actor_id` AND of every
    /// local predecessor this nest's successions retired into it, in one
    /// transaction. Returns whether `actor_id`'s own row existed.
    ///
    /// **The chain goes with the account** (`account-data-plane.md` § Nest-side
    /// requirements item 1, *Deletion reaches the account's predecessors*). A
    /// home-nest ceremony keeps the retired identity's `users` row handle-less,
    /// and a predecessor is the same person under an earlier key, so its row is
    /// under the same verdict as the deleted id's own: nothing declares a
    /// foreign key onto `users`, the `Retain` rows that still name a retired id
    /// (`actor_successions`, `audit_log`) name the deleted account's own id the
    /// same way once its row is gone, and a row left behind is a handle-less
    /// ghost in the admin's user list that only a by-hand deletion removes.
    ///
    /// **Order is load-bearing: this runs AFTER `purge_orphaned_actor_rows`**
    /// (`pending_actions::finalize_user_deletion`). The purge walk's locality
    /// test IS the predecessors' `users` rows
    /// (`successions::local_predecessors` joins them), so the chain's rows go
    /// last and atomically: a crash before this transaction leaves every row
    /// and the walk finds the whole chain again; a crash after it leaves
    /// nothing under those ids for a walk to find. The refusal of a retired
    /// key at the registration doors does not rest on these rows either — it
    /// is `actor_successions`' (`auth_core::successor_of`), which
    /// `Policy::Retain` keeps.
    pub async fn delete_user(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction().context("begin tx")?;
        let chain = super::successions::local_predecessors(&tx, &actor_id)?;
        let mut deleted = false;
        for id in std::iter::once(actor_id).chain(chain) {
            let n = tx
                .execute(
                    "DELETE FROM users WHERE actor_id = ?1",
                    rusqlite::params![id.as_slice()],
                )
                .context("delete user")?;
            if id == actor_id {
                deleted = n > 0;
            }
            // A deleted account is not findable by the handle it held
            // (`fts::sync_profile_row` — the profile row mirrors no `content`
            // row, so no content purge would reach it).
            super::fts::sync_profile_row(&tx, &id)?;
            // The account's own feature-gate state goes with it. Only the SELF
            // tier is keyed by an account — region and admin documents are
            // nest-wide and must survive (`db::feature_gate`'s subject-key
            // rule). The usage buckets would age out on their own within the
            // prune horizon; deleting them here is what stops a re-registered
            // actor id inheriting a stranger's spent quota. ⚠ This method does
            // NOT purge the rest of this account's per-user rows; these two are handled
            // here so the feature plane does not add to it.
            tx.execute(
                "DELETE FROM feature_policies WHERE subject_id = ?1",
                rusqlite::params![id.as_slice()],
            )
            .context("delete user's self feature policies")?;
            tx.execute(
                "DELETE FROM feature_usage WHERE actor_id = ?1",
                rusqlite::params![id.as_slice()],
            )
            .context("delete user's feature usage buckets")?;
        }
        tx.commit().context("commit user deletion")?;
        Ok(deleted)
    }

    /// Get a user by actor_id.
    pub async fn get_user(&self, actor_id: &[u8; 32]) -> Result<Option<UserRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT actor_id, tier, label, suspended, created_at,
                    inbox_bytes_used, storage_bytes_used,
                    eviction_status, eviction_reason, eviction_category,
                    eviction_warned_at, eviction_suspend_at, eviction_delete_at, handle
             FROM users WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
            |row| {
                Ok(UserRow {
                    actor_id: row.get(0)?,
                    tier: row.get(1)?,
                    label: row.get(2)?,
                    suspended: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                    inbox_bytes_used: row.get(5)?,
                    storage_bytes_used: row.get(6)?,
                    eviction_status: row.get(7)?,
                    eviction_reason: row.get(8)?,
                    eviction_category: row.get(9)?,
                    eviction_warned_at: row.get(10)?,
                    eviction_suspend_at: row.get(11)?,
                    eviction_delete_at: row.get(12)?,
                    handle: row.get(13)?,
                })
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get user"),
        }
    }

    // ==================== Eviction ====================

    /// Start the eviction flow for a user. Sets status to "warning" and
    /// computes suspend_at and delete_at from the given durations.
    pub async fn start_eviction(
        &self,
        actor_id: &[u8; 32],
        reason: &str,
        category: &str,
        warning_days: i64,
        suspension_days: i64,
    ) -> Result<bool> {
        let actor_id = *actor_id;
        let reason = reason.to_string();
        let category = category.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let suspend_at = now + warning_days * 86400;
        let delete_at = suspend_at + suspension_days * 86400;
        let updated = conn
            .execute(
                "UPDATE users SET eviction_status = 'warning', eviction_reason = ?1,
             eviction_category = ?2, eviction_warned_at = ?3,
             eviction_suspend_at = ?4, eviction_delete_at = ?5
             WHERE actor_id = ?6 AND eviction_status = ''",
                rusqlite::params![
                    reason,
                    category,
                    now,
                    suspend_at,
                    delete_at,
                    actor_id.as_slice()
                ],
            )
            .context("start eviction")?;
        Ok(updated > 0)
    }

    /// Cancel an active eviction, restoring the user to normal.
    ///
    /// Refuses (`false`) an account already in `deleting`: its deletion has
    /// begun (`transition_evictions`), and a restore landing between the
    /// finalize's steps would hand back a half-reclaimed account.
    pub async fn cancel_eviction(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let updated = conn
            .execute(
                "UPDATE users SET eviction_status = '', eviction_reason = '',
             eviction_category = '', eviction_warned_at = NULL,
             eviction_suspend_at = NULL, eviction_delete_at = NULL,
             suspended = 0
             WHERE actor_id = ?1 AND eviction_status NOT IN ('', 'deleting')",
                rusqlite::params![actor_id.as_slice()],
            )
            .context("cancel eviction")?;
        Ok(updated > 0)
    }

    /// Advance the eviction state machine one tick: `warning -> suspended`, and
    /// `suspended -> deleting` once the suspension window has run out.
    ///
    /// Returns `(newly_suspended, deleting)`: the actors this tick suspended,
    /// and EVERY actor now in `deleting` — the ones this tick moved there plus
    /// any whose finalize an earlier tick saw refused or failed — so the caller
    /// retries each until it goes (`eviction::run_eviction_tick`).
    ///
    /// Phase 3 deletes nothing here. Marking the row `deleting` is the one
    /// atomic decision point (`nest/common.md` § Client-state recoverability):
    /// from it on `cancel_eviction` refuses, so no restore races the multi-step
    /// finalize the caller runs next — `pending_actions::finalize_user_deletion`,
    /// the one production deletion path, with its post retraction, per-actor
    /// legs, purge walk over the account's local predecessors, and
    /// admin/guardian refusals (`account-data-plane.md` § Nest-side
    /// requirements item 1). This used to run a raw `DELETE FROM users` inline,
    /// stranding every `Purge` row of the evicted account under an id with no
    /// `users` row.
    pub async fn transition_evictions(&self) -> Result<(Vec<Vec<u8>>, Vec<Vec<u8>>)> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();

        conn.execute_batch("BEGIN")?;
        let result = (|| -> Result<(Vec<Vec<u8>>, Vec<Vec<u8>>)> {
            // Phase 1 -> 2: warning -> suspended
            let mut stmt = conn.prepare(
                "SELECT actor_id FROM users
                 WHERE eviction_status = 'warning' AND eviction_suspend_at <= ?1",
            )?;
            let to_suspend: Vec<Vec<u8>> = stmt
                .query_map(rusqlite::params![now], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);

            for actor_id in &to_suspend {
                conn.execute(
                    "UPDATE users SET eviction_status = 'suspended', suspended = 1
                     WHERE actor_id = ?1",
                    rusqlite::params![actor_id.as_slice()],
                )?;
            }

            // Phase 2 -> 3: suspended -> deleting. An actor suspended by this
            // same tick waits for the next one, so its suspension teardown
            // always runs before its deletion.
            let mut stmt = conn.prepare(
                "SELECT actor_id FROM users
                 WHERE eviction_status = 'suspended' AND eviction_delete_at <= ?1",
            )?;
            let mut to_mark: Vec<Vec<u8>> = stmt
                .query_map(rusqlite::params![now], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);
            to_mark.retain(|id| !to_suspend.contains(id));
            for actor_id in &to_mark {
                conn.execute(
                    "UPDATE users SET eviction_status = 'deleting' WHERE actor_id = ?1",
                    rusqlite::params![actor_id.as_slice()],
                )?;
            }

            let mut stmt =
                conn.prepare("SELECT actor_id FROM users WHERE eviction_status = 'deleting'")?;
            let deleting: Vec<Vec<u8>> = stmt
                .query_map([], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);

            Ok((to_suspend, deleting))
        })();

        match &result {
            Ok(_) => conn.execute_batch("COMMIT")?,
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
        result
    }

    /// List all users with an active eviction (status != "").
    pub async fn list_evictions(&self) -> Result<Vec<UserRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT actor_id, tier, label, suspended, created_at,
                    inbox_bytes_used, storage_bytes_used,
                    eviction_status, eviction_reason, eviction_category,
                    eviction_warned_at, eviction_suspend_at, eviction_delete_at, handle
             FROM users WHERE eviction_status != ''",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(UserRow {
                    actor_id: row.get(0)?,
                    tier: row.get(1)?,
                    label: row.get(2)?,
                    suspended: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                    inbox_bytes_used: row.get(5)?,
                    storage_bytes_used: row.get(6)?,
                    eviction_status: row.get(7)?,
                    eviction_reason: row.get(8)?,
                    eviction_category: row.get(9)?,
                    eviction_warned_at: row.get(10)?,
                    eviction_suspend_at: row.get(11)?,
                    eviction_delete_at: row.get(12)?,
                    handle: row.get(13)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ==================== Eviction Export Tokens ====================

    /// Create an eviction export token for a user.
    pub async fn create_eviction_token(
        &self,
        token: &str,
        actor_id: &[u8; 32],
        expires_at: i64,
    ) -> Result<()> {
        let actor_id = *actor_id;
        let token = token.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT OR REPLACE INTO eviction_tokens (token, actor_id, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![token, actor_id.as_slice(), now, expires_at],
        )
        .context("create eviction token")?;
        Ok(())
    }

    /// Validate an eviction export token. Returns the actor_id if valid.
    pub async fn validate_eviction_token(&self, token: &str) -> Result<Option<[u8; 32]>> {
        let token = token.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let result = conn.query_row(
            "SELECT actor_id FROM eviction_tokens
             WHERE token = ?1 AND expires_at > ?2",
            rusqlite::params![token, now],
            |row| {
                let bytes: Vec<u8> = row.get(0)?;
                Ok(bytes)
            },
        );
        match result {
            Ok(bytes) => {
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("bad actor_id"))?;
                Ok(Some(arr))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get the eviction export token for a user (if any, and not expired).
    pub async fn get_eviction_token_for_actor(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<String>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let result = conn.query_row(
            "SELECT token FROM eviction_tokens
             WHERE actor_id = ?1 AND expires_at > ?2
             ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![actor_id.as_slice(), now],
            |row| row.get(0),
        );
        match result {
            Ok(token) => Ok(Some(token)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete eviction tokens for a user (on eviction cancel or account deletion).
    pub async fn delete_eviction_tokens(&self, actor_id: &[u8; 32]) -> Result<()> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM eviction_tokens WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
        )?;
        Ok(())
    }

    // ==================== Handle Resolution ====================

    /// Resolve a handle (e.g. "alice") to an ActorId.
    pub async fn resolve_handle(&self, handle: &str) -> Result<Option<[u8; 32]>> {
        let handle = handle.to_string();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT actor_id FROM users WHERE handle = ?1 AND handle != ''",
            rusqlite::params![handle],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(bytes) => {
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("invalid actor_id length"))?;
                Ok(Some(arr))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("resolve handle"),
        }
    }

    /// Get the handle for an actor (if set).
    pub async fn get_handle(&self, actor_id: &[u8; 32]) -> Result<Option<String>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT handle FROM users WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
            |row| row.get::<_, Option<String>>(0),
        );
        match result {
            Ok(h) => Ok(h),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get handle"),
        }
    }

    /// Set (or clear) a user's handle. Empty string clears it.
    pub async fn set_handle(&self, actor_id: &[u8; 32], handle: &str) -> Result<()> {
        let actor_id = *actor_id;
        let handle = handle.to_string();
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction().context("begin set handle")?;
        tx.execute(
            "UPDATE users SET handle = ?1 WHERE actor_id = ?2",
            rusqlite::params![handle, actor_id.as_slice()],
        )
        .context("set handle")?;
        // The Search corpus's profile row follows the handle — renamed, found
        // by the new name only; cleared, gone (`fts::sync_profile_row`).
        super::fts::sync_profile_row(&tx, actor_id.as_slice())?;
        tx.commit().context("commit set handle")?;
        Ok(())
    }

    // ==================== Invite Codes ====================

    /// Create an invite code. `uses_left` is how many times it can be redeemed.
    pub async fn create_invite_code(&self, code: &str, tier: &str, uses_left: i64) -> Result<()> {
        self.create_invite_code_with_guardian(code, tier, uses_left, None, None)
            .await
    }

    /// Create an invite code, optionally carrying a supervised-admission
    /// guardian designation and its age band (`family-safety.md` § Wire &
    /// data shape + § The account age band). The caller validates the
    /// guardian (`check_guardian_admissible`) and the band token + its
    /// band-requires-guardian rule first (the mint handler).
    pub async fn create_invite_code_with_guardian(
        &self,
        code: &str,
        tier: &str,
        uses_left: i64,
        guardian_actor: Option<&[u8]>,
        age_band: Option<&str>,
    ) -> Result<()> {
        let code = code.to_string();
        let tier = tier.to_string();
        let guardian = guardian_actor.map(|g| g.to_vec());
        let age_band = age_band.map(|b| b.to_string());
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT INTO invite_codes (code, tier, uses_left, created_at, guardian_actor, age_band) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![code, tier, uses_left, now, guardian, age_band],
        ).context("create invite code")?;
        Ok(())
    }

    /// Look up an invite code WITHOUT consuming a use. Returns the grant if
    /// the code exists and has uses remaining, `None` otherwise. Used by the
    /// `fauna.account.invite_code.verify` ceremony so the wizard can show
    /// "valid" (and the supervised + band disclosures) before Continue →
    /// register (which then calls `validate_invite_code` and actually
    /// decrements).
    pub async fn peek_invite_code(&self, code: &str) -> Result<Option<super::InviteCodeGrant>> {
        let code = code.to_string();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT tier, uses_left, guardian_actor, age_band FROM invite_codes WHERE code = ?1",
            rusqlite::params![code],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        );
        match result {
            Ok((tier, uses_left, guardian_actor, age_band)) if uses_left > 0 => {
                Ok(Some(super::InviteCodeGrant {
                    tier,
                    guardian_actor,
                    age_band,
                }))
            }
            Ok(_) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("peek invite code"),
        }
    }

    /// Validate and consume one use of an invite code. Returns the grant if
    /// valid and has uses remaining, `None` otherwise. Atomically decrements
    /// `uses_left`.
    pub async fn validate_invite_code(&self, code: &str) -> Result<Option<super::InviteCodeGrant>> {
        let code = code.to_string();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT tier, uses_left, guardian_actor, age_band FROM invite_codes WHERE code = ?1",
            rusqlite::params![code],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        );
        match result {
            Ok((tier, uses_left, guardian_actor, age_band)) if uses_left > 0 => {
                conn.execute(
                    "UPDATE invite_codes SET uses_left = uses_left - 1 WHERE code = ?1",
                    rusqlite::params![code],
                )
                .context("decrement invite code")?;
                Ok(Some(super::InviteCodeGrant {
                    tier,
                    guardian_actor,
                    age_band,
                }))
            }
            Ok(_) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("validate invite code"),
        }
    }

    /// List all invite codes.
    pub async fn list_invite_codes(&self) -> Result<Vec<InviteCodeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT code, tier, uses_left, created_at, guardian_actor, age_band FROM invite_codes ORDER BY created_at DESC"
        ).context("prepare list invite codes")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(InviteCodeRow {
                    code: row.get(0)?,
                    tier: row.get(1)?,
                    uses_left: row.get(2)?,
                    created_at: row.get(3)?,
                    guardian_actor: row.get(4)?,
                    age_band: row.get(5)?,
                })
            })
            .context("query invite codes")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read invite code row")?);
        }
        Ok(results)
    }

    /// Delete an invite code. Returns true if the code existed.
    pub async fn delete_invite_code(&self, code: &str) -> Result<bool> {
        let code = code.to_string();
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM invite_codes WHERE code = ?1",
                rusqlite::params![code],
            )
            .context("delete invite code")?;
        Ok(deleted > 0)
    }

    // ==================== Atomic Registration ====================

    /// Create a user and set their handle in a single transaction.
    /// If either the actor_id or handle is already taken, rolls back completely.
    ///
    /// The display `label` defaults to the `handle` — every handle-registered
    /// actor (the box claimer via `claim_core`, an invite-approved user, a
    /// self-registered user) has a usable display name in `fauna.admin.users.list`
    /// out of the box (the admin-users hub + the `admin-dns` catch-all actor
    /// picker render it; a blank default left both showing only the raw actor-id).
    /// An admin can override it via `update_user` (`fauna.admin.users.update`'s
    /// `label`) — e.g. the invite-approval optional-label tail in
    /// `admin_ws_handlers`.
    /// `guardian`: a supervised admission links the new account to this
    /// guardian actor in the SAME transaction — the single atomic decision
    /// point (`family-safety.md` § Wire & data shape): after any crash the
    /// account either exists supervised or does not exist. The caller
    /// validates the guardian first (`check_guardian_admissible` + the
    /// guardian ≠ admitted-actor check).
    pub async fn create_user_with_handle(
        &self,
        actor_id: &[u8; 32],
        tier: &str,
        handle: &str,
        guardian: Option<&[u8]>,
    ) -> Result<()> {
        self.create_user_with_handle_and_age(actor_id, tier, handle, guardian, None)
            .await
    }

    /// [`Self::create_user_with_handle`] plus the account age band
    /// (`family-safety.md` § The account age band): `age` is
    /// `(band wire token, provenance wire token)`, written as the
    /// `account_age_bands` row in the SAME transaction, and — for a
    /// supervised admission — keying the fresh `guardian_policies` row on
    /// `ReachPolicy::age_band_defaults(band)` (the defaults dial). `None`
    /// writes no band row (the by-construction `18+`/`none` for a link-less
    /// account; band-unknown for a band-less supervised admission).
    ///
    /// A **minor band with no guardian is refused here** as the last line of
    /// defense (make-unrepresentable at the write site): every admission path
    /// refuses it earlier with its own typed error (`public-mode.md` § Age at
    /// registration — the store-says-minor refusal; the mint/approve
    /// band-requires-guardian rule).
    pub async fn create_user_with_handle_and_age(
        &self,
        actor_id: &[u8; 32],
        tier: &str,
        handle: &str,
        guardian: Option<&[u8]>,
        age: Option<(&str, &str)>,
    ) -> Result<()> {
        let parsed_band = match age {
            Some((band, _)) => Some(
                fauna_protocol::age::AgeBand::from_wire(band)
                    .ok_or_else(|| anyhow::anyhow!("unknown age band token: {band}"))?,
            ),
            None => None,
        };
        if let Some(band) = parsed_band
            && band.is_minor()
            && guardian.is_none()
        {
            anyhow::bail!("a minor age band requires a guardianship link (family-safety.md)");
        }
        let banded_defaults =
            parsed_band.map(fauna_protocol::family::ReachPolicy::age_band_defaults);

        let actor_id = *actor_id;
        let tier = tier.to_string();
        let handle = handle.to_string();
        let guardian = guardian.map(|g| g.to_vec());
        let age = age.map(|(b, p)| (b.to_string(), p.to_string()));
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let tx = conn.unchecked_transaction().context("begin tx")?;
        // label defaults to the handle (see doc comment) — a later admin
        // `update_user` can override it.
        tx.execute(
            "INSERT INTO users (actor_id, tier, label, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![actor_id.as_slice(), tier, handle, now],
        )
        .context("create user")?;
        tx.execute(
            "UPDATE users SET handle = ?1 WHERE actor_id = ?2",
            rusqlite::params![handle, actor_id.as_slice()],
        )
        .context("set handle")?;
        if let Some(g) = guardian.as_deref() {
            super::family::insert_guardianship_tx(
                &tx,
                actor_id.as_slice(),
                g,
                banded_defaults.as_ref(),
            )?;
        }
        if let Some((band, provenance)) = &age {
            super::family::set_age_band_tx(&tx, actor_id.as_slice(), band, provenance)?;
        }
        // The account is findable by its handle from the moment it exists —
        // every admission door (claim, registration, invite approval,
        // membership payment) lands here, so none indexes it on its own.
        super::fts::sync_profile_row(&tx, actor_id.as_slice())?;
        tx.commit().context("commit registration")?;
        Ok(())
    }

    // ==================== Handle Cooldowns ====================

    /// Record a 72-hour cooldown when a handle is released.
    pub async fn release_handle_with_cooldown(
        &self,
        actor_id: &[u8; 32],
        handle: &str,
    ) -> Result<()> {
        let actor_id = *actor_id;
        let handle = handle.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let tx = conn
            .unchecked_transaction()
            .context("begin handle release")?;
        tx.execute(
            "INSERT OR REPLACE INTO handle_cooldowns (handle, old_actor_id, released_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![handle, actor_id.as_slice(), now],
        ).context("release handle cooldown")?;
        // Clear the handle from the users table
        tx.execute(
            "UPDATE users SET handle = '' WHERE actor_id = ?1 AND handle = ?2",
            rusqlite::params![actor_id.as_slice(), handle],
        )
        .context("clear handle")?;
        // A released handle leaves the Search corpus with it.
        super::fts::sync_profile_row(&tx, actor_id.as_slice())?;
        tx.commit().context("commit handle release")?;
        Ok(())
    }

    /// Check if a handle is available considering cooldown.
    /// Returns true if the handle can be claimed by the given actor_id.
    /// During the 72-hour cooldown, only the original owner can reclaim.
    pub async fn check_handle_cooldown(&self, handle: &str, actor_id: &[u8; 32]) -> Result<bool> {
        let handle = handle.to_string();
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT old_actor_id, released_at FROM handle_cooldowns WHERE handle = ?1",
            rusqlite::params![handle],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
        );
        match result {
            Ok((old_actor, released_at)) => {
                let cooldown_secs = 72 * 3600; // 72 hours
                let now = now_epoch_secs();
                if now - released_at < cooldown_secs {
                    // During cooldown: only original owner can reclaim
                    let old: [u8; 32] = old_actor
                        .try_into()
                        .map_err(|_| anyhow::anyhow!("bad actor_id"))?;
                    Ok(old == actor_id)
                } else {
                    // Cooldown expired — anyone can claim; clean up
                    conn.execute(
                        "DELETE FROM handle_cooldowns WHERE handle = ?1",
                        rusqlite::params![handle],
                    )
                    .ok();
                    Ok(true)
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(true),
            Err(e) => Err(e).context("check handle cooldown"),
        }
    }

    // ==================== User Counting ====================

    /// Count all registered users.
    pub async fn count_all_users(&self) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .context("count all users")
    }

    /// Count users with a given tier.
    pub async fn count_users_by_tier(&self, tier: &str) -> Result<i64> {
        let tier = tier.to_string();
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE tier = ?1",
                rusqlite::params![tier],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count)
    }

    /// Check whether an actor_id is already registered.
    ///
    /// Counts **every** `users` row, a suspended one included — deliberately,
    /// by ruling (`login.md` § Errors, the registration doors' accepted
    /// exception to the opaque `not_registered` code): the two registration
    /// doors (`invite_core::submit_invite_request_core`,
    /// `account_core::register_core`) read this to refuse an actor that already
    /// holds an account with `fauna.account.actor_exists`, and a suspended
    /// account is still an account, whose one way back is the admin's Restore,
    /// never a second admission. Standing is a separate question —
    /// `actor_suspended` / `check_actor_active`.
    pub async fn is_actor_registered(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE actor_id = ?1",
                rusqlite::params![actor],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count > 0)
    }

    // ==================== Quota Checks ====================

    /// Check if a user can receive a payload of given size.
    /// Returns Ok(()) if allowed, Err with reason if not.
    pub async fn check_quota(&self, actor_id: &[u8; 32], payload_size: usize) -> Result<()> {
        let conn = self.conn.lock().await;
        Self::check_quota_in_tx(&conn, actor_id, payload_size)
    }

    /// [`Self::check_quota`]'s rule, on a connection the caller already holds —
    /// the single source of truth both the ordinary push paths
    /// (`routes.rs`'s `inbox.deliver`, `federation_handlers.rs`'s Welcome
    /// delivery) and a room invitation's in-transaction check
    /// (`db/rooms.rs::record_room_invite_and_deliver`) enforce identically.
    ///
    /// Takes the raw connection rather than `&self` because `get_user`/
    /// `get_tier` lock `self.conn` themselves: a caller already inside its
    /// own transaction on that same (non-reentrant) mutex would deadlock
    /// calling through them, so this queries the `users`/`tiers` tables
    /// directly — the same shape [`Self::ack_and_refund_row`] (`db/inbox.rs`)
    /// uses for the same reason.
    pub(super) fn check_quota_in_tx(
        conn: &rusqlite::Connection,
        actor_id: &[u8; 32],
        payload_size: usize,
    ) -> Result<()> {
        let (tier_name, suspended, inbox_bytes_used): (String, bool, i64) = conn
            .query_row(
                "SELECT tier, suspended, inbox_bytes_used FROM users WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0, row.get(2)?)),
            )
            .optional()
            .context("check_quota_in_tx: read user")?
            .ok_or_else(|| anyhow::anyhow!("user not registered"))?;

        if suspended {
            anyhow::bail!("user is suspended");
        }

        let (max_blob_size, max_inbox_bytes): (i64, i64) = conn
            .query_row(
                "SELECT max_blob_size, max_inbox_bytes FROM tiers WHERE name = ?1",
                rusqlite::params![tier_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("check_quota_in_tx: read tier")?
            .ok_or_else(|| anyhow::anyhow!("tier not found"))?;

        let size = payload_size as i64;
        if size > max_blob_size {
            anyhow::bail!("payload exceeds max blob size");
        }
        if inbox_bytes_used + size > max_inbox_bytes {
            anyhow::bail!("inbox quota exceeded");
        }

        Ok(())
    }

    /// Check if actor_id is registered and not suspended. Returns the tier name.
    pub async fn check_actor_active(&self, actor_id: &[u8; 32]) -> Result<String> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT tier, suspended FROM users WHERE actor_id = ?1")?;
        let row = stmt
            .query_row([&actor], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
            })
            .with_context(|| "user not registered")?;
        if row.1 {
            anyhow::bail!("user is suspended");
        }
        Ok(row.0)
    }

    /// This actor's suspension bit, or `None` when it has **no `users` row**.
    ///
    /// The missing-row policy is deliberately left to the caller, because the
    /// two authority gates that read this need opposite answers and silently
    /// inheriting the wrong one is a security bug either way:
    ///
    /// - `family_handlers` screens the *acting* guardian on every
    ///   authority-bearing `fauna.family.*` call and takes `unwrap_or(true)` —
    ///   **fail-closed**, since a guardian must be a real user. This preserves
    ///   the admission-time property "a suspended actor may not be a guardian"
    ///   for the life of the link (`family-safety.md` § Lifecycle gates — a
    ///   suspended guardian leaves the ward's approvals queue *unattended*).
    ///   (`check_guardian_admissible` screens the *candidate* guardian at
    ///   designation time; this screens the acting one.)
    /// - `caller_class_for_actor` (`bridge_method_allowlist.rs`) tests
    ///   `== Some(true)` — it denies only a *known* suspended actor, because a
    ///   missing row there means "bridge or admin", both of which legitimately
    ///   have none. (That a plain actor with no row still falls through to
    ///   `CallerClass::User` is a separate, pre-existing finding — see that
    ///   function's note.)
    ///
    /// Reading it there is what makes suspension bite at **dispatch** rather
    /// than only at token mint: a suspended actor's already-open WebSocket
    /// cannot dispatch, and an open-registration nest — whose auth handshake
    /// never runs `check_actor_active` — is covered too.
    pub async fn actor_suspended(&self, actor: &[u8]) -> Result<Option<bool>> {
        let a = actor.to_vec();
        let conn = self.conn.lock().await;
        let row = conn.query_row(
            "SELECT suspended FROM users WHERE actor_id = ?1",
            rusqlite::params![a],
            |row| row.get::<_, i64>(0),
        );
        match row {
            Ok(s) => Ok(Some(s != 0)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("check actor suspended"),
        }
    }

    /// Read both authority columns of an actor's `users` row in one query.
    ///
    /// `Ok(None)` = **no row**: an actor that never registered, or one whose row
    /// `finalize_user_deletion` dropped. The dispatch gate denies it outright;
    /// contrast [`CacheDb::actor_suspended`], whose `None` two callers interpret
    /// with deliberately *opposite* null policies.
    pub async fn actor_authority(&self, actor: &[u8]) -> Result<Option<ActorAuthority>> {
        let a = actor.to_vec();
        let conn = self.conn.lock().await;
        let row = conn.query_row(
            "SELECT suspended, locked_until FROM users WHERE actor_id = ?1",
            rusqlite::params![a],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
        );
        match row {
            Ok((suspended, locked_until)) => Ok(Some(ActorAuthority {
                suspended: suspended != 0,
                locked_until,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("read actor authority"),
        }
    }

    // ==================== Audit Log ====================

    /// Insert an audit log entry.
    pub async fn audit(
        &self,
        actor_id: Option<&[u8]>,
        action: &str,
        target: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        audit_on_conn(&conn, actor_id, action, target, detail)
    }

    /// List audit log entries, ordered by most recent first.
    /// Optionally paginate with `before_id` and limit results.
    pub async fn list_audit(&self, limit: i64, before_id: Option<i64>) -> Result<Vec<AuditRow>> {
        let conn = self.conn.lock().await;
        let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql + Send>>) = if let Some(bid) =
            before_id
        {
            (
                "SELECT id, ts, actor_id, action, target, detail, prev_hash, entry_hash, entry_hash_version FROM audit_log WHERE id < ?1 ORDER BY id DESC LIMIT ?2",
                vec![Box::new(bid), Box::new(limit)],
            )
        } else {
            (
                "SELECT id, ts, actor_id, action, target, detail, prev_hash, entry_hash, entry_hash_version FROM audit_log ORDER BY id DESC LIMIT ?1",
                vec![Box::new(limit)],
            )
        };
        let mut stmt = conn.prepare(sql).context("prepare list_audit")?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok(AuditRow {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    actor_id: row.get(2)?,
                    action: row.get(3)?,
                    target: row.get(4)?,
                    detail: row.get(5)?,
                    prev_hash: row.get(6)?,
                    entry_hash: row.get(7)?,
                    entry_hash_version: row.get(8)?,
                })
            })
            .context("query audit_log")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read audit row")?);
        }
        Ok(results)
    }

    /// Returns (count, head_id, head_hash, first_ts, last_ts) for the audit log hash chain.
    pub async fn audit_integrity(&self) -> Result<(i64, i64, String, i64, i64)> {
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))?;
        if count == 0 {
            return Ok((0, 0, String::new(), 0, 0));
        }
        let (head_id, head_hash, last_ts): (i64, String, i64) = conn.query_row(
            "SELECT id, entry_hash, ts FROM audit_log ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let first_ts: i64 = conn.query_row(
            "SELECT ts FROM audit_log ORDER BY id ASC LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        Ok((count, head_id, head_hash, first_ts, last_ts))
    }

    /// How many `audit_log` rows this binary could verify, by the preimage
    /// version each row **records**.
    ///
    /// The chain walk itself is not built yet (`succession-aftermath.md`
    /// § Implementation status today says so plainly). This is the part of the
    /// answer that does not need it, and it is the part the version record
    /// makes expressible at all: how many rows each format reaches is a number
    /// an admin can read, rather than "every row, forever, because nothing
    /// says otherwise".
    ///
    /// `unverifiable` is the refuse rule counted rather than silently absorbed:
    /// a row recording a version this binary does not implement (a v3 row read
    /// by a v2 binary) lands here instead of being tried against some format
    /// until one fits.
    pub async fn audit_entry_version_census(&self) -> Result<AuditVersionCensus> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT entry_hash_version, COUNT(*) FROM audit_log GROUP BY entry_hash_version",
            )
            .context("prepare audit entry-version census")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .context("query audit entry-version census")?;
        let mut census = AuditVersionCensus::default();
        for row in rows {
            let (recorded, n) = row.context("read audit entry-version census row")?;
            match ChainVersion::from_recorded(recorded) {
                Some(ChainVersion::V2) => census.v2 += n,
                None => census.unverifiable += n,
            }
        }
        Ok(census)
    }

    // ==================== Worker Replication ====================

    /// Record that a payload has been replicated to a worker.
    pub async fn mark_replicated(
        &self,
        payload_type: &str,
        payload_key: &[u8],
        inbox_row_id: Option<i64>,
    ) -> Result<()> {
        let payload_type = payload_type.to_string();
        let payload_key = payload_key.to_vec();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT OR IGNORE INTO worker_replication (payload_type, payload_key, inbox_row_id, replicated_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![payload_type, payload_key, inbox_row_id.unwrap_or(-1), now],
        )
        .context("mark replicated")?;
        Ok(())
    }

    /// Remove a payload's replication-tracking row — the delete twin of
    /// [`Self::mark_replicated`], called after the paired-public-nest replica
    /// removes a deleted post (`spawn_replicate_delete`), so
    /// [`Self::replication_count`] stays truthful and never over-counts posts
    /// that no longer exist. Idempotent (a no-op DELETE when no row matches).
    pub async fn unmark_replicated(
        &self,
        payload_type: &str,
        payload_key: &[u8],
        inbox_row_id: Option<i64>,
    ) -> Result<()> {
        let payload_type = payload_type.to_string();
        let payload_key = payload_key.to_vec();
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM worker_replication
             WHERE payload_type = ?1 AND payload_key = ?2 AND inbox_row_id = ?3",
            rusqlite::params![payload_type, payload_key, inbox_row_id.unwrap_or(-1)],
        )
        .context("unmark replicated")?;
        Ok(())
    }

    /// Every payload key of `payload_type` still marked replicated with no
    /// inbox row (`inbox_row_id = -1`, the post shape) — the
    /// `worker_replication` rows a post-delete re-drive scans for a replica
    /// whose original is gone (`post_delete_redrive`). Uncapped on purpose:
    /// a cap here would let live rows starve the gone ones behind them; the
    /// re-drive caps the work it does, not the scan.
    pub async fn list_replicated_keys(&self, payload_type: &str) -> Result<Vec<Vec<u8>>> {
        let payload_type = payload_type.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT payload_key FROM worker_replication
              WHERE payload_type = ?1 AND inbox_row_id = -1
              ORDER BY replicated_at ASC",
        )?;
        let keys = stmt
            .query_map(rusqlite::params![payload_type], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<Vec<u8>>>>()
            .context("list replicated keys")?;
        Ok(keys)
    }

    /// Check if a payload has been replicated.
    pub async fn is_replicated(
        &self,
        payload_type: &str,
        payload_key: &[u8],
        inbox_row_id: Option<i64>,
    ) -> Result<bool> {
        let payload_type = payload_type.to_string();
        let payload_key = payload_key.to_vec();
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM worker_replication
                 WHERE payload_type = ?1 AND payload_key = ?2 AND inbox_row_id = ?3",
                rusqlite::params![payload_type, payload_key, inbox_row_id.unwrap_or(-1)],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count > 0)
    }

    /// Count total replicated payloads.
    pub async fn replication_count(&self) -> Result<i64> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM worker_replication", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);
        Ok(count)
    }

    // ==================== Stats ====================

    /// Get dashboard statistics.
    pub async fn get_stats(&self) -> Result<Stats> {
        let conn = self.conn.lock().await;

        let total_users: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .unwrap_or(0);

        let suspended_users: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE suspended = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let total_inbox_bytes: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM( \
                    CASE WHEN c.blob_hash IS NOT NULL \
                         THEN (SELECT bm.size_bytes FROM blob_metadata bm WHERE bm.hash = c.blob_hash) \
                         ELSE LENGTH(c.payload) \
                    END \
                 ), 0) FROM content_links cl \
                 JOIN content c ON c.id = cl.source_id \
                 WHERE cl.link_type = 'delivery' AND cl.status = 'undelivered'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let total_storage_bytes: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(storage_bytes_used), 0) FROM users",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let mut stmt = conn
            .prepare("SELECT tier, COUNT(*) FROM users GROUP BY tier ORDER BY tier")
            .context("prepare users_by_tier")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .context("query users_by_tier")?;
        let mut users_by_tier = Vec::new();
        for row in rows {
            users_by_tier.push(row.context("read tier count")?);
        }

        Ok(Stats {
            total_users,
            users_by_tier,
            suspended_users,
            total_inbox_bytes,
            total_storage_bytes,
        })
    }

    // ==================== Invite Requests ====================

    /// Create a pending invite request. Returns the new row id.
    /// Fails with a UNIQUE error if the actor already has a non-deleted row.
    pub async fn create_invite_request(
        &self,
        actor_id: &[u8; 32],
        handle: &str,
        message: &str,
        age: Option<(&str, &str)>,
    ) -> Result<i64> {
        let actor_id = *actor_id;
        let handle = handle.to_string();
        let message = message.to_string();
        // The applicant's age claim as `(band, provenance)` — recorded for
        // absence-as-signal on the admin's request row (`public-mode.md`
        // § Age at registration); `None` when the submit carried no claim.
        // The submit core validates the band token and verifies any
        // attestation before this is called.
        let age = age.map(|(b, p)| (b.to_string(), p.to_string()));
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT INTO invite_requests (actor_id, handle, message, status, created_at, age_band, age_provenance)
             VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6)",
            rusqlite::params![
                actor_id.as_slice(),
                handle,
                message,
                now,
                age.as_ref().map(|(b, _)| b.clone()),
                age.as_ref().map(|(_, p)| p.clone())
            ],
        )
        .context("create invite request")?;
        Ok(conn.last_insert_rowid())
    }

    /// Fetch an invite request by actor_id.
    pub async fn get_invite_request_by_actor(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<InviteRequestRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, actor_id, handle, message, status, created_at,
                    decided_at, decided_by, denial_reason, age_band, age_provenance
             FROM invite_requests WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
            invite_request_row_from,
        )
        .optional()
        .context("get invite request by actor")
    }

    /// Fetch an invite request by numeric id.
    pub async fn get_invite_request(&self, id: i64) -> Result<Option<InviteRequestRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, actor_id, handle, message, status, created_at,
                    decided_at, decided_by, denial_reason, age_band, age_provenance
             FROM invite_requests WHERE id = ?1",
            rusqlite::params![id],
            invite_request_row_from,
        )
        .optional()
        .context("get invite request")
    }

    /// List all invite requests, newest first.
    pub async fn list_invite_requests(&self) -> Result<Vec<InviteRequestRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, actor_id, handle, message, status, created_at,
                        decided_at, decided_by, denial_reason, age_band, age_provenance
                 FROM invite_requests ORDER BY created_at DESC, id DESC",
            )
            .context("prepare list invite requests")?;
        let rows = stmt
            .query_map([], invite_request_row_from)
            .context("query invite requests")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read invite request row")?);
        }
        Ok(out)
    }

    /// Count the invite-request rows currently in the `pending` state. Used by
    /// `invite_core::submit_invite_request_core` to enforce a global cap on
    /// outstanding requests — the hard disk/clutter backstop against a flood that
    /// rotates keypairs to evade the per-`actor_id` dedup and the per-source rate
    /// limit (security review § D6). Decided rows (`approved`/`denied`) don't
    /// count: they're consumed (approve deletes the row) or retained for audit
    /// and don't represent unbounded attacker-driven growth.
    pub async fn count_pending_invite_requests(&self) -> Result<u64> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM invite_requests WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .context("count pending invite requests")?;
        Ok(count.max(0) as u64)
    }

    /// Delete an invite request by actor_id (user cancellation).
    /// Returns true if a row was removed.
    pub async fn delete_invite_request_by_actor(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM invite_requests WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
            )
            .context("delete invite request by actor")?;
        Ok(deleted > 0)
    }

    /// Delete an invite request by id (admin approve — request is consumed).
    /// Returns true if a row was removed.
    pub async fn delete_invite_request(&self, id: i64) -> Result<bool> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM invite_requests WHERE id = ?1",
                rusqlite::params![id],
            )
            .context("delete invite request")?;
        Ok(deleted > 0)
    }

    /// Mark an invite request as denied. Returns true if a pending row was updated.
    pub async fn deny_invite_request(
        &self,
        id: i64,
        admin_actor_id: &[u8; 32],
        reason: Option<&str>,
    ) -> Result<bool> {
        let admin_actor_id = *admin_actor_id;
        let reason_owned = reason.map(|s| s.to_string());
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let updated = conn
            .execute(
                "UPDATE invite_requests
                 SET status = 'denied', decided_at = ?1, decided_by = ?2, denial_reason = ?3
                 WHERE id = ?4 AND status = 'pending'",
                rusqlite::params![now, admin_actor_id.as_slice(), reason_owned, id],
            )
            .context("deny invite request")?;
        Ok(updated > 0)
    }

    /// Delete `invite_requests` rows that were **denied** more than
    /// `cutoff_decided_at` (Unix seconds) ago. `pending` rows are never
    /// touched — they're bounded by `MAX_PENDING_INVITE_REQUESTS` and an
    /// admin's own approve/deny action, not by age (only a decided row
    /// persists forever today — approve already deletes its row). Returns the
    /// number of rows deleted. Backs
    /// [`spawn_invite_request_retention_sweeper`]; pruning a row also clears
    /// `invite_core::submit_invite_request_core`'s one-row-per-actor block, so
    /// the requester can submit fresh with no `cancel` needed — the same
    /// outcome as today's post-cancel path.
    pub async fn prune_denied_invite_requests_older_than(
        &self,
        cutoff_decided_at: i64,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM invite_requests WHERE status = 'denied' AND decided_at < ?1",
                rusqlite::params![cutoff_decided_at],
            )
            .context("prune denied invite requests")?;
        Ok(n)
    }
}

/// Default retention for a **denied** `invite_requests` row, from
/// `decided_at` (`onboarding.md` § The pending-invite surface —
/// "Row retention/GC on the nest side is a separate hygiene track"). Generous
/// on purpose: the requester's `Denied{reason}` read (the pending-invite
/// surface's poll) must comfortably survive a vacation, and a `pending` row
/// never reaches this path at all. Mirrors `bridge_audit::DEFAULT_AUDIT_RETENTION`.
pub const DEFAULT_INVITE_REQUEST_RETENTION: std::time::Duration =
    std::time::Duration::from_secs(90 * 24 * 60 * 60);

/// Spawns a tokio task that periodically deletes denied `invite_requests`
/// rows past `retention` (by `decided_at`). Cadence is 1/24 of `retention`
/// (a 90-day window sweeps every ~3.75 days), mirroring
/// `bridge_audit::spawn_audit_retention_sweeper`.
pub fn spawn_invite_request_retention_sweeper(
    db: std::sync::Arc<CacheDb>,
    retention: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    super::spawn_retention_sweeper(retention, move || {
        let db = db.clone();
        async move {
            let cutoff = now_epoch_secs().saturating_sub(retention.as_secs() as i64);
            match db.prune_denied_invite_requests_older_than(cutoff).await {
                Ok(n) if n > 0 => tracing::info!(
                    target: "invite_requests",
                    pruned = n,
                    cutoff,
                    "pruned denied invite requests"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    target: "invite_requests",
                    error = %e,
                    "invite-request retention sweep failed"
                ),
            }
        }
    })
}

fn invite_request_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<InviteRequestRow> {
    Ok(InviteRequestRow {
        id: row.get(0)?,
        actor_id: row.get(1)?,
        handle: row.get(2)?,
        message: row.get(3)?,
        status: row.get(4)?,
        created_at: row.get(5)?,
        decided_at: row.get(6)?,
        decided_by: row.get(7)?,
        denial_reason: row.get(8)?,
        age_band: row.get(9)?,
        age_provenance: row.get(10)?,
    })
}

#[cfg(test)]
mod invite_request_retention_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn prune_drops_only_denied_rows_past_cutoff() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin = [1u8; 32];

        let pending_actor = [10u8; 32];
        let old_denied_actor = [11u8; 32];
        let recent_denied_actor = [12u8; 32];
        db.create_invite_request(&pending_actor, "pending-h", "", None)
            .await
            .unwrap();
        let old_id = db
            .create_invite_request(&old_denied_actor, "old-h", "", None)
            .await
            .unwrap();
        let recent_id = db
            .create_invite_request(&recent_denied_actor, "recent-h", "", None)
            .await
            .unwrap();
        db.deny_invite_request(old_id, &admin, Some("no"))
            .await
            .unwrap();
        db.deny_invite_request(recent_id, &admin, Some("no"))
            .await
            .unwrap();

        // Backdate only the "old" row's decided_at — simulates TTL elapsed.
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE invite_requests SET decided_at = 1 WHERE id = ?1",
                rusqlite::params![old_id],
            )
            .unwrap();
        }

        let pruned = db
            .prune_denied_invite_requests_older_than(1_000_000)
            .await
            .unwrap();
        assert_eq!(pruned, 1, "only the backdated denied row should be pruned");

        // Pending rows are never touched by age, and a recently-denied row
        // inside the TTL still serves its status + reason.
        let pending = db
            .get_invite_request_by_actor(&pending_actor)
            .await
            .unwrap();
        assert!(pending.is_some(), "pending rows are not aged out");
        let recent = db
            .get_invite_request_by_actor(&recent_denied_actor)
            .await
            .unwrap()
            .expect("recently-denied row survives inside the TTL");
        assert_eq!(recent.status, "denied");
        assert_eq!(recent.denial_reason.as_deref(), Some("no"));

        // The aged-out denied row is gone, which also clears the
        // one-row-per-actor block — the actor can submit fresh with no cancel.
        assert!(
            db.get_invite_request_by_actor(&old_denied_actor)
                .await
                .unwrap()
                .is_none()
        );
        let resubmitted_id = db
            .create_invite_request(&old_denied_actor, "old-h-again", "", None)
            .await
            .unwrap();
        assert!(resubmitted_id > 0);
    }

    #[tokio::test]
    async fn spawn_retention_sweeper_prunes_aged_denied_row_after_tick() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let admin = [2u8; 32];
        let actor = [13u8; 32];
        let id = db
            .create_invite_request(&actor, "aged-h", "", None)
            .await
            .unwrap();
        db.deny_invite_request(id, &admin, Some("no"))
            .await
            .unwrap();
        {
            let conn = db.conn.lock().await;
            conn.execute("UPDATE invite_requests SET decided_at = 1", [])
                .unwrap();
        }

        // Deadline poll, not a settle-sleep (testing.md § point 14) — sized far
        // above any non-pathological scheduling delay so a green run pays only
        // for the first poll.
        const PRUNE_BUDGET: Duration = Duration::from_secs(30);
        let handle = spawn_invite_request_retention_sweeper(db.clone(), Duration::from_millis(60));
        let deadline = std::time::Instant::now() + PRUNE_BUDGET;
        loop {
            if db
                .get_invite_request_by_actor(&actor)
                .await
                .unwrap()
                .is_none()
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "sweeper did not prune the aged denied row within {PRUNE_BUDGET:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        handle.abort();
    }
}

#[cfg(test)]
mod users_list_order_tests {
    use super::*;

    /// `created_at` is second-granular, so accounts created in one second tie.
    /// The list breaks those ties newest-inserted first, so single-row offset
    /// pages walk a tie group without repeating or skipping an account — the
    /// admin actor pickers page this list to its total (`admin.md` § Where logic
    /// lives) — and the unpaged list keeps the same order.
    #[tokio::test]
    async fn accounts_created_in_one_second_list_newest_inserted_first() {
        let db = CacheDb::open_in_memory().unwrap();
        {
            let conn = db.conn.lock().await;
            for n in 1u8..=5 {
                conn.execute(
                    "INSERT INTO users (actor_id, tier, label, created_at) \
                     VALUES (?1, 'free', '', 1000)",
                    rusqlite::params![vec![n; 32]],
                )
                .unwrap();
            }
        }

        let mut paged = Vec::new();
        for offset in 0..5 {
            let (rows, total) = db.list_users_paginated(1, offset).await.unwrap();
            assert_eq!(total, 5);
            paged.extend(rows.into_iter().map(|row| row.actor_id[0]));
        }
        assert_eq!(paged, vec![5, 4, 3, 2, 1]);

        let listed: Vec<u8> = db
            .list_users()
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.actor_id[0])
            .collect();
        assert_eq!(listed, vec![5, 4, 3, 2, 1]);
    }
}
