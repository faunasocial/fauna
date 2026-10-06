//! Storage for the **primary-domain-rename** state machine — one row per
//! in-flight or historical primary rename (`docs/goal/behavior/
//! mail-primary-domain-rename.md` § Data — the `mail_domain_renames` model).
//!
//! The table + its partial unique index (single-active-rename invariant) are
//! declared in `migrations::MIGRATIONS_MAIL_DOMAIN_RENAMES`. This module owns the
//! typed CRUD + the full state machine: `insert` at `requested`, read
//! (`get_active` / `list` / `lookup_by_id`), the cert-acquisition transitions
//! `advance_rename_to_cert_issuance` + `mark_rename_cert_ready`, the atomic
//! anchor flip `advance_rename_to_grace` + its inverse `abort_rename_from_grace`,
//! the grace-watcher promotion `advance_rename_to_ready_to_complete`, `complete_rename`,
//! `extend_rename_grace`, and the pre-flip-only `mark_rename_aborted`. Each is
//! written for idempotent re-derivation from the cert-lifecycle loop
//! (`mail-primary-domain-rename.md` § Crash recovery — there is no persisted-
//! failure state).
//!
//! **Every state-transition UPDATE is a CAS** — conditional on the expected
//! from-state (`AND state = '…'` / `IN (…)`) with `rows == 1` asserted, mapping
//! a 0-row result to [`RenameTransitionError::WrongState`] rather than silently
//! clobbering. This closes the admin-RPC ↔ cert-lifecycle-loop races (a
//! pre-flip abort racing the flip; the watcher racing an abort) at the SQL
//! level, so a lost race refuses (409) instead of stranding a half-terminal /
//! resurrected row (`mail-primary-domain-rename.md` § Crash recovery — the
//! single atomic decision point holds against concurrent writers). Refusals are
//! [`RenameTransitionError`] so the RPC layer maps them to 409-class codes
//! without string-matching.
//!
//! Ids are 16-byte BLOB UUIDs (matching `mail_domains.domain_id`);
//! `initiated_by_actor_id` is a 32-byte actor BLOB (matching `admin_actors`);
//! timestamps are epoch-millis. The `state` column is the
//! `fauna_mail::RenameState` wire string.

use anyhow::{Context, Result, anyhow};
use fauna_mail::RenameState;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{CacheDb, now_epoch_millis};

/// A typed rename state-machine transition failure. The RPC layer
/// (`bridge_routing_handlers::map_rename_err`) downcasts this to map a
/// not-found / already-in-progress / wrong-state refusal to a **409-class**
/// wire code instead of string-matching the anyhow message — the fragility
/// a review flagged and the new auto-advancing transitions made observable
/// (`mail-primary-domain-rename.md` § Wire shapes — RPC refusal codes). Every
/// fallible pre-transition guard returns one of these (wrapped into
/// `anyhow::Error` on the way out via `?`/`.into()`), so a new transition can't
/// silently fall through to a `500 internal`. Genuinely-internal failures (a
/// "row vanished" post-write re-read, a participant-missing rollback, a
/// rusqlite/context error) stay untyped `anyhow` and map to `internal`.
#[derive(Debug, thiserror::Error)]
pub enum RenameTransitionError {
    /// No `mail_domain_renames` row with the given id.
    #[error("rename not found")]
    NotFound,
    /// A non-terminal rename already exists (the single-active-rename invariant;
    /// the partial unique index is the backstop). Maps to
    /// `rename_already_in_progress`.
    #[error("a primary-domain rename is already in progress")]
    AlreadyInProgress,
    /// The row is already terminal (`completed`/`aborted`) — a pre-flip abort
    /// has nothing to record.
    #[error("rename already terminal")]
    AlreadyTerminal,
    /// The row was not in a state this transition accepts. Either the caller
    /// invoked it in the wrong state (the precondition read caught it), or a
    /// concurrent transition (the cert-lifecycle loop or another Admin RPC)
    /// changed the state between that read and the **state-conditional UPDATE**
    /// (the CAS matched 0 rows — a lost race, never a silent clobber).
    /// `attempted` names the transition; `actual` describes the observed state.
    #[error("cannot {attempted} a rename in state {actual}")]
    WrongState {
        attempted: &'static str,
        actual: String,
    },
}

/// The `actual`-state string a CAS 0-row refusal reports: the transition passed
/// its precondition read but the row left the expected state before the UPDATE
/// (a concurrent transition won the race). Distinct from the precondition-read
/// refusal, which reports the concrete observed `Option<RenameState>`.
const CAS_RACE_ACTUAL: &str = "changed concurrently";

/// The full `SELECT` column list, in the order [`row_to_rename`] reads.
const RENAME_COLUMNS: &str = "
    rename_id, old_primary_domain_id, new_primary_domain_id, state, started_at,
    grace_days, cert_acquired_at, new_cert_fingerprint, flipped_at,
    grace_started_at, grace_ends_at, ready_to_complete_at, completed_at,
    aborted_at, abort_reason, initiated_by_actor_id";

/// One row from `mail_domain_renames`. Field order mirrors the column order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailDomainRename {
    #[serde(with = "serde_bytes")]
    pub rename_id: [u8; 16],
    #[serde(with = "serde_bytes")]
    pub old_primary_domain_id: [u8; 16],
    #[serde(with = "serde_bytes")]
    pub new_primary_domain_id: [u8; 16],
    /// The lifecycle state as a `fauna_mail::RenameState` wire string. Kept a
    /// `String` (not the enum) so a forward-compat state a newer nest wrote round
    /// -trips unchanged; interpret via `RenameState::from_wire`.
    pub state: String,
    pub started_at: i64,
    pub grace_days: i64,
    pub cert_acquired_at: Option<i64>,
    pub new_cert_fingerprint: Option<String>,
    pub flipped_at: Option<i64>,
    pub grace_started_at: Option<i64>,
    pub grace_ends_at: Option<i64>,
    pub ready_to_complete_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub aborted_at: Option<i64>,
    pub abort_reason: Option<String>,
    #[serde(with = "serde_bytes")]
    pub initiated_by_actor_id: [u8; 32],
}

impl MailDomainRename {
    /// The parsed lifecycle state (`None` if a newer nest wrote a state this
    /// binary doesn't know).
    pub fn parsed_state(&self) -> Option<RenameState> {
        RenameState::from_wire(&self.state)
    }
}

fn read_blob<const N: usize>(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<[u8; N]> {
    let v: Vec<u8> = row.get(idx)?;
    <[u8; N]>::try_from(v.as_slice()).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Blob,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("expected {N}-byte blob at col {idx}, got {}", v.len()),
            )),
        )
    })
}

fn row_to_rename(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailDomainRename> {
    Ok(MailDomainRename {
        rename_id: read_blob::<16>(row, 0)?,
        old_primary_domain_id: read_blob::<16>(row, 1)?,
        new_primary_domain_id: read_blob::<16>(row, 2)?,
        state: row.get(3)?,
        started_at: row.get(4)?,
        grace_days: row.get(5)?,
        cert_acquired_at: row.get(6)?,
        new_cert_fingerprint: row.get(7)?,
        flipped_at: row.get(8)?,
        grace_started_at: row.get(9)?,
        grace_ends_at: row.get(10)?,
        ready_to_complete_at: row.get(11)?,
        completed_at: row.get(12)?,
        aborted_at: row.get(13)?,
        abort_reason: row.get(14)?,
        initiated_by_actor_id: read_blob::<32>(row, 15)?,
    })
}

impl CacheDb {
    /// Insert a new rename at state `requested` (`mail-primary-domain-rename.md`
    /// § Lifecycle). Fails if a non-terminal rename already exists (the
    /// single-active-rename invariant — checked under the lock and backstopped by
    /// the partial unique index `idx_mail_domain_renames_active`). No cert / DNS /
    /// binding side effects — slice 1 stops at `requested`.
    pub async fn insert_domain_rename(
        &self,
        old_primary_domain_id: &[u8; 16],
        new_primary_domain_id: &[u8; 16],
        grace_days: i64,
        initiated_by_actor_id: &[u8; 32],
    ) -> Result<MailDomainRename> {
        let rename_id = *Uuid::new_v4().as_bytes();
        let now = now_epoch_millis();
        {
            let conn = self.conn.lock().await;
            // Pre-check under the lock (the unique index is the backstop).
            let active: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT rename_id FROM mail_domain_renames
                     WHERE state NOT IN ('completed', 'aborted') LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .context("check active rename")?;
            if active.is_some() {
                return Err(RenameTransitionError::AlreadyInProgress.into());
            }
            conn.execute(
                "INSERT INTO mail_domain_renames
                    (rename_id, old_primary_domain_id, new_primary_domain_id,
                     state, started_at, grace_days, initiated_by_actor_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    &rename_id[..],
                    &old_primary_domain_id[..],
                    &new_primary_domain_id[..],
                    RenameState::Requested.as_str(),
                    now,
                    grace_days,
                    &initiated_by_actor_id[..],
                ],
            )
            .context("insert mail_domain_renames row")?;
        }
        self.lookup_rename_by_id(&rename_id)
            .await?
            .ok_or_else(|| anyhow!("mail_domain_renames row vanished after insert"))
    }

    /// The single in-flight rename, if any (`state NOT IN completed/aborted`).
    /// The partial unique index guarantees at most one.
    pub async fn get_active_rename(&self) -> Result<Option<MailDomainRename>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {RENAME_COLUMNS} FROM mail_domain_renames
                 WHERE state NOT IN ('completed', 'aborted') LIMIT 1"
            ),
            [],
            row_to_rename,
        )
        .optional()
        .context("get_active_rename")
    }

    /// All renames (in-flight + terminal), newest first — the admin audit
    /// enumeration.
    pub async fn list_domain_renames(&self) -> Result<Vec<MailDomainRename>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {RENAME_COLUMNS} FROM mail_domain_renames
                 ORDER BY started_at DESC"
            ))
            .context("prepare list_domain_renames")?;
        let rows = stmt
            .query_map([], row_to_rename)
            .context("query list_domain_renames")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_domain_renames")
    }

    /// Look up a rename by its id.
    pub async fn lookup_rename_by_id(
        &self,
        rename_id: &[u8; 16],
    ) -> Result<Option<MailDomainRename>> {
        let id_vec = rename_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {RENAME_COLUMNS} FROM mail_domain_renames WHERE rename_id = ?1"),
            [&id_vec],
            row_to_rename,
        )
        .optional()
        .context("lookup_rename_by_id")
    }

    /// Mark a **pre-flip** rename `aborted` (`mail-primary-domain-rename.md`
    /// § Lifecycle) — a pure state change with nothing to unwind (the widened
    /// SAN + advertised DNS row simply stop being re-derived once terminal). The
    /// handler routes a **post-flip** abort to [`abort_rename_from_grace`]
    /// instead (which runs the atomic inverse re-flip); this method must never
    /// touch a flipped row, so its UPDATE is a **CAS on the pre-flip set**
    /// (`requested`/`cert_issuance`/`cert_ready`), not merely "non-terminal".
    /// That is the fix for the admin-abort ↔ flip race: if the loop flips the
    /// row to `grace` between this method's precondition read and its UPDATE, the
    /// CAS matches 0 rows and refuses (`WrongState`) rather than clobbering
    /// `grace → aborted` and stranding `is_primary` flipped under a terminal row
    /// with no inverse run. Refuses a not-found id or an already-terminal row.
    pub async fn mark_rename_aborted(
        &self,
        rename_id: &[u8; 16],
        abort_reason: Option<String>,
    ) -> Result<MailDomainRename> {
        let existing = self
            .lookup_rename_by_id(rename_id)
            .await?
            .ok_or(RenameTransitionError::NotFound)?;
        if existing.parsed_state().is_some_and(|s| s.is_terminal()) {
            return Err(RenameTransitionError::AlreadyTerminal.into());
        }
        let now = now_epoch_millis();
        {
            let conn = self.conn.lock().await;
            // CAS on the pre-flip states — a flip that raced this abort leaves the
            // row `grace`, which this predicate excludes → 0 rows → refuse.
            let rows = conn
                .execute(
                    &format!(
                        "UPDATE mail_domain_renames
                         SET state = ?1, aborted_at = ?2, abort_reason = ?3
                         WHERE rename_id = ?4 AND state IN ('{}', '{}', '{}')",
                        RenameState::Requested.as_str(),
                        RenameState::CertIssuance.as_str(),
                        RenameState::CertReady.as_str(),
                    ),
                    rusqlite::params![
                        RenameState::Aborted.as_str(),
                        now,
                        abort_reason.as_deref(),
                        &rename_id[..],
                    ],
                )
                .context("update mail_domain_renames abort")?;
            if rows != 1 {
                return Err(RenameTransitionError::WrongState {
                    attempted: "abort",
                    actual: CAS_RACE_ACTUAL.into(),
                }
                .into());
            }
        }
        self.lookup_rename_by_id(rename_id)
            .await?
            .ok_or_else(|| anyhow!("mail_domain_renames row vanished after abort"))
    }

    /// Advance a rename `requested` → `cert_issuance` — the entry into cert
    /// widening (`mail-primary-domain-rename.md` § Lifecycle). Called by the
    /// `start_primary_domain_rename` handler right after insert, and re-asserted
    /// by the cert-lifecycle loop for crash recovery (§ Crash recovery). **No
    /// side effects here** — the loop performs the SAN widening; this is the pure
    /// state advance. Idempotent: a no-op that returns the row unchanged when it
    /// is already `cert_issuance`. Refuses a not-found id or a row past
    /// `cert_issuance`.
    pub async fn advance_rename_to_cert_issuance(
        &self,
        rename_id: &[u8; 16],
    ) -> Result<MailDomainRename> {
        let existing = self
            .lookup_rename_by_id(rename_id)
            .await?
            .ok_or(RenameTransitionError::NotFound)?;
        match existing.parsed_state() {
            // Already there — idempotent no-op (the loop re-asserts each tick).
            Some(RenameState::CertIssuance) => return Ok(existing),
            Some(RenameState::Requested) => {}
            other => {
                return Err(RenameTransitionError::WrongState {
                    attempted: "advance to cert_issuance",
                    actual: format!("{other:?}"),
                }
                .into());
            }
        }
        {
            let conn = self.conn.lock().await;
            // CAS `= requested` — a concurrent advance/abort refuses instead of
            // re-writing a row that already left `requested`.
            let rows = conn
                .execute(
                    &format!(
                        "UPDATE mail_domain_renames SET state = ?1
                         WHERE rename_id = ?2 AND state = '{}'",
                        RenameState::Requested.as_str()
                    ),
                    rusqlite::params![RenameState::CertIssuance.as_str(), &rename_id[..]],
                )
                .context("update mail_domain_renames cert_issuance")?;
            if rows != 1 {
                return Err(RenameTransitionError::WrongState {
                    attempted: "advance to cert_issuance",
                    actual: CAS_RACE_ACTUAL.into(),
                }
                .into());
            }
        }
        self.lookup_rename_by_id(rename_id)
            .await?
            .ok_or_else(|| anyhow!("mail_domain_renames row vanished after cert_issuance advance"))
    }

    /// Stamp a rename `cert_issuance` → `cert_ready` with the acquired cert's
    /// SPKI fingerprint + acquisition time (`mail-primary-domain-rename.md` §
    /// Lifecycle). The cert-lifecycle loop calls this once `cert_covers_sans`
    /// confirms the widened single managed cert covers `mail.<new-primary>`.
    /// **Strict forward transition** (only from `cert_issuance`) so a re-observe
    /// on the next tick — where the row is already `cert_ready` — does **not**
    /// overwrite the original `cert_acquired_at`; the loop only calls this while
    /// the row is `cert_issuance`. Refuses a not-found id or any other state.
    pub async fn mark_rename_cert_ready(
        &self,
        rename_id: &[u8; 16],
        new_cert_fingerprint: &str,
        cert_acquired_at: i64,
    ) -> Result<MailDomainRename> {
        let existing = self
            .lookup_rename_by_id(rename_id)
            .await?
            .ok_or(RenameTransitionError::NotFound)?;
        match existing.parsed_state() {
            Some(RenameState::CertIssuance) => {}
            other => {
                return Err(RenameTransitionError::WrongState {
                    attempted: "mark cert_ready",
                    actual: format!("{other:?}"),
                }
                .into());
            }
        }
        {
            let conn = self.conn.lock().await;
            // CAS `= cert_issuance` (strict) — never overwrite the original
            // acquisition stamp on a re-observe, and refuse a raced transition.
            let rows = conn
                .execute(
                    &format!(
                        "UPDATE mail_domain_renames
                         SET state = ?1, cert_acquired_at = ?2, new_cert_fingerprint = ?3
                         WHERE rename_id = ?4 AND state = '{}'",
                        RenameState::CertIssuance.as_str()
                    ),
                    rusqlite::params![
                        RenameState::CertReady.as_str(),
                        cert_acquired_at,
                        new_cert_fingerprint,
                        &rename_id[..],
                    ],
                )
                .context("update mail_domain_renames cert_ready")?;
            if rows != 1 {
                return Err(RenameTransitionError::WrongState {
                    attempted: "mark cert_ready",
                    actual: CAS_RACE_ACTUAL.into(),
                }
                .into());
            }
        }
        self.lookup_rename_by_id(rename_id)
            .await?
            .ok_or_else(|| anyhow!("mail_domain_renames row vanished after cert_ready mark"))
    }

    /// Advance a rename `cert_ready` → `grace` — the **atomic anchor flip**
    /// (`mail-primary-domain-rename.md` § Lifecycle / § Data — Mutations to
    /// mail_domains, SLICE 3). ONE transaction flips `is_primary` from the old to
    /// the new `mail_domains` row **and** sets the rename to `grace` with its
    /// flip/grace stamps, so a crash lands the row either at `cert_ready` (not
    /// flipped) or `grace` (flipped) — a single atomic decision point, never a
    /// half-flip. The old row is demoted **before** the new is promoted, so the
    /// `mail_domains` exactly-one-primary partial unique index
    /// (`WHERE removed_at IS NULL AND is_primary = 1`) never sees two primaries
    /// mid-transaction.
    ///
    /// `grace_ends_at = flipped_at + grace_days × 1 day` (grace_days read from the
    /// row; `flipped_at == grace_started_at` in the steady case). Strict
    /// `cert_ready → grace`: refuses any other state (the cert-lifecycle loop
    /// calls this only when it observes `cert_ready`; a re-tick that finds the row
    /// already `grace` skips the call). The **runtime** side effects — swap the
    /// `identity_domain` projection (`apply_primary_identity`) + push
    /// `config_changed` — are the caller's; this method owns only the atomic DB
    /// transition. If either participant `mail_domains` row is missing (a
    /// soft-delete raced the flip) the transaction rolls back and the row stays
    /// `cert_ready` (recoverable via `abort`).
    pub async fn advance_rename_to_grace(&self, rename_id: &[u8; 16]) -> Result<MailDomainRename> {
        let existing = self
            .lookup_rename_by_id(rename_id)
            .await?
            .ok_or(RenameTransitionError::NotFound)?;
        match existing.parsed_state() {
            Some(RenameState::CertReady) => {}
            other => {
                return Err(RenameTransitionError::WrongState {
                    attempted: "advance to grace",
                    actual: format!("{other:?}"),
                }
                .into());
            }
        }
        let now = now_epoch_millis();
        // grace_days is bounded [1, 30] at insert; ms/day is well within i64.
        let grace_ends_at = now + existing.grace_days * 86_400_000;
        let old_id = existing.old_primary_domain_id;
        let new_id = existing.new_primary_domain_id;
        {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction()
                .context("begin anchor-flip transaction")?;
            // Demote the old primary FIRST, then promote the new — so the
            // exactly-one-primary partial unique index never sees two primaries.
            let demoted = tx
                .execute(
                    "UPDATE mail_domains SET is_primary = 0
                     WHERE domain_id = ?1 AND removed_at IS NULL",
                    rusqlite::params![&old_id[..]],
                )
                .context("demote old primary")?;
            let promoted = tx
                .execute(
                    "UPDATE mail_domains SET is_primary = 1
                     WHERE domain_id = ?1 AND removed_at IS NULL",
                    rusqlite::params![&new_id[..]],
                )
                .context("promote new primary")?;
            if demoted != 1 || promoted != 1 {
                // A participant domain vanished (soft-deleted) between cert_ready
                // and the flip. Drop `tx` here → rollback (no half-flip); the row
                // stays `cert_ready` and is recoverable via `abort`.
                return Err(anyhow!(
                    "anchor flip touched {demoted} old + {promoted} new active mail_domains \
                     rows (expected 1 each) — a participant domain is missing"
                ));
            }
            // CAS `= cert_ready` on the rename row too (the demote/promote guards
            // above protect only the `mail_domains` participants): if a concurrent
            // pre-flip abort marked the row `aborted` between the precondition read
            // and this transaction, the CAS matches 0 rows → return before commit →
            // the whole tx (incl. the demote/promote) rolls back, so the flip never
            // lands on an aborted row.
            let updated = tx
                .execute(
                    &format!(
                        "UPDATE mail_domain_renames
                         SET state = ?1, flipped_at = ?2, grace_started_at = ?2, grace_ends_at = ?3
                         WHERE rename_id = ?4 AND state = '{}'",
                        RenameState::CertReady.as_str()
                    ),
                    rusqlite::params![
                        RenameState::Grace.as_str(),
                        now,
                        grace_ends_at,
                        &rename_id[..],
                    ],
                )
                .context("update mail_domain_renames grace")?;
            if updated != 1 {
                return Err(RenameTransitionError::WrongState {
                    attempted: "advance to grace",
                    actual: CAS_RACE_ACTUAL.into(),
                }
                .into());
            }
            tx.commit().context("commit anchor-flip transaction")?;
        }
        self.lookup_rename_by_id(rename_id)
            .await?
            .ok_or_else(|| anyhow!("mail_domain_renames row vanished after grace advance"))
    }

    /// Advance a rename `grace` → `ready_to_complete` — the grace watcher's
    /// promotion when `NOW() > grace_ends_at` (`mail-primary-domain-rename.md`
    /// § Lifecycle; § Architectural rules — the watcher is source-of-truth past
    /// grace, SLICE 4). Stamps `ready_to_complete_at` + `state`. **No cert/DNS/
    /// identity side effect**: `ready_to_complete` is functionally identical to
    /// `grace` for the keep-alives (both are `is_post_flip_active`) — it is a
    /// marker telling the admin the cache-flush window elapsed and `complete`
    /// may be called. Strict `grace → ready_to_complete` (the loop calls this
    /// only when it observes `grace`); refuses any other state.
    pub async fn advance_rename_to_ready_to_complete(
        &self,
        rename_id: &[u8; 16],
    ) -> Result<MailDomainRename> {
        let existing = self
            .lookup_rename_by_id(rename_id)
            .await?
            .ok_or(RenameTransitionError::NotFound)?;
        match existing.parsed_state() {
            Some(RenameState::Grace) => {}
            other => {
                return Err(RenameTransitionError::WrongState {
                    attempted: "advance to ready_to_complete",
                    actual: format!("{other:?}"),
                }
                .into());
            }
        }
        let now = now_epoch_millis();
        {
            let conn = self.conn.lock().await;
            // CAS `= grace` — if an abort-from-grace committed (→ aborted + re-flip)
            // between the read and here, the watcher's promotion refuses instead of
            // resurrecting a terminal row back to non-terminal.
            let rows = conn
                .execute(
                    &format!(
                        "UPDATE mail_domain_renames
                         SET state = ?1, ready_to_complete_at = ?2
                         WHERE rename_id = ?3 AND state = '{}'",
                        RenameState::Grace.as_str()
                    ),
                    rusqlite::params![RenameState::ReadyToComplete.as_str(), now, &rename_id[..]],
                )
                .context("update mail_domain_renames ready_to_complete")?;
            if rows != 1 {
                return Err(RenameTransitionError::WrongState {
                    attempted: "advance to ready_to_complete",
                    actual: CAS_RACE_ACTUAL.into(),
                }
                .into());
            }
        }
        self.lookup_rename_by_id(rename_id).await?.ok_or_else(|| {
            anyhow!("mail_domain_renames row vanished after ready_to_complete advance")
        })
    }

    /// Complete a rename `grace` | `ready_to_complete` → `completed` (terminal;
    /// `mail-primary-domain-rename.md` § Lifecycle, SLICE 4). Stamps
    /// `completed_at` + `state`. **The force-gate — refusing `grace` without
    /// `force` — is the handler's policy** (§ Wire shapes —
    /// `grace_period_not_expired`); this method accepts both post-flip-active
    /// states so `complete(force=true)` from `grace` works. Once terminal,
    /// `get_active_rename()` returns None → the DNS assembler `mail.<old>`
    /// keep-alive (`dns_handlers.rs`) and the cert-loop SAN keep
    /// (`active_rename_old_mail_host`) both stop (they source from
    /// `get_active_rename` + gate on `is_post_flip_active`), so DNS drops
    /// `mail.<old> A` on the next reconcile and the cert narrows on the next
    /// renewal — the post-rename steady state, recompute-driven (no forced
    /// re-issue here). `is_primary` is untouched (already flipped at SLICE 3).
    /// Strict from `grace`/`ready_to_complete`.
    pub async fn complete_rename(&self, rename_id: &[u8; 16]) -> Result<MailDomainRename> {
        let existing = self
            .lookup_rename_by_id(rename_id)
            .await?
            .ok_or(RenameTransitionError::NotFound)?;
        match existing.parsed_state() {
            Some(RenameState::Grace | RenameState::ReadyToComplete) => {}
            other => {
                return Err(RenameTransitionError::WrongState {
                    attempted: "complete",
                    actual: format!("{other:?}"),
                }
                .into());
            }
        }
        let now = now_epoch_millis();
        {
            let conn = self.conn.lock().await;
            // CAS on the post-flip-active states — refuse (not clobber) if the row
            // was aborted or already completed in a concurrent transition.
            let rows = conn
                .execute(
                    &format!(
                        "UPDATE mail_domain_renames
                         SET state = ?1, completed_at = ?2
                         WHERE rename_id = ?3 AND state IN ('{}', '{}')",
                        RenameState::Grace.as_str(),
                        RenameState::ReadyToComplete.as_str(),
                    ),
                    rusqlite::params![RenameState::Completed.as_str(), now, &rename_id[..]],
                )
                .context("update mail_domain_renames completed")?;
            if rows != 1 {
                return Err(RenameTransitionError::WrongState {
                    attempted: "complete",
                    actual: CAS_RACE_ACTUAL.into(),
                }
                .into());
            }
        }
        self.lookup_rename_by_id(rename_id)
            .await?
            .ok_or_else(|| anyhow!("mail_domain_renames row vanished after complete"))
    }

    /// Abort a rename from a **post-flip** state (`grace` | `ready_to_complete`)
    /// — the atomic **inverse** of [`advance_rename_to_grace`]
    /// (`mail-primary-domain-rename.md` § Data — Mutations to mail_domains,
    /// abort; § Architectural rules — abort from post-flip is expensive but
    /// always valid, SLICE 4). ONE transaction re-flips `is_primary` back from
    /// the new row to the old (demote NEW **first**, then promote OLD —
    /// demote-first so the `mail_domains` exactly-one-primary partial unique
    /// index never sees two primaries) + sets the rename `state = aborted`,
    /// `aborted_at`, `abort_reason`. A crash lands the row either still post-flip
    /// (not re-flipped) or `aborted` (re-flipped) — a single atomic decision
    /// point, never a half-flip; boot re-derives identity from the restored old
    /// primary. The **runtime** side effects (swap the identity projection back
    /// via `apply_primary_identity` + push `config_changed`) are the caller's;
    /// this owns only the atomic DB inverse. If either participant row is missing
    /// (`removed_at` set — the in-flight remove guard should prevent this) the
    /// transaction rolls back (error, no half-flip) and the row stays post-flip
    /// (recoverable). Strict from post-flip-active (a pre-flip abort has nothing
    /// to unwind → the handler routes it to [`mark_rename_aborted`]).
    pub async fn abort_rename_from_grace(
        &self,
        rename_id: &[u8; 16],
        abort_reason: Option<String>,
    ) -> Result<MailDomainRename> {
        let existing = self
            .lookup_rename_by_id(rename_id)
            .await?
            .ok_or(RenameTransitionError::NotFound)?;
        match existing.parsed_state() {
            Some(s) if s.is_post_flip_active() => {}
            other => {
                return Err(RenameTransitionError::WrongState {
                    attempted: "abort-from-grace",
                    actual: format!("{other:?}"),
                }
                .into());
            }
        }
        let now = now_epoch_millis();
        let old_id = existing.old_primary_domain_id;
        let new_id = existing.new_primary_domain_id;
        {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction()
                .context("begin abort-from-grace transaction")?;
            // Demote the NEW primary FIRST, then re-promote the OLD — so the
            // exactly-one-primary partial unique index never sees two primaries
            // (the exact mirror of the forward flip's demote-first ordering).
            let demoted = tx
                .execute(
                    "UPDATE mail_domains SET is_primary = 0
                     WHERE domain_id = ?1 AND removed_at IS NULL",
                    rusqlite::params![&new_id[..]],
                )
                .context("demote new primary")?;
            let promoted = tx
                .execute(
                    "UPDATE mail_domains SET is_primary = 1
                     WHERE domain_id = ?1 AND removed_at IS NULL",
                    rusqlite::params![&old_id[..]],
                )
                .context("re-promote old primary")?;
            if demoted != 1 || promoted != 1 {
                // A participant domain vanished (soft-deleted) during grace. Drop
                // `tx` → rollback (no half-reflip); the row stays post-flip and is
                // recoverable (re-try once the domain is restored / via complete).
                return Err(anyhow!(
                    "abort re-flip touched {demoted} new + {promoted} old active mail_domains \
                     rows (expected 1 each) — a participant domain is missing"
                ));
            }
            // CAS on the post-flip-active states (mirrors the precondition read's
            // `is_post_flip_active()`): if the row left that set concurrently (a
            // racing `complete`, or another abort) the re-flip rolls back rather
            // than double-aborting / clobbering a completed row.
            let updated = tx
                .execute(
                    &format!(
                        "UPDATE mail_domain_renames
                         SET state = ?1, aborted_at = ?2, abort_reason = ?3
                         WHERE rename_id = ?4 AND state IN ('{}', '{}', '{}')",
                        RenameState::AnchorFlip.as_str(),
                        RenameState::Grace.as_str(),
                        RenameState::ReadyToComplete.as_str(),
                    ),
                    rusqlite::params![
                        RenameState::Aborted.as_str(),
                        now,
                        abort_reason.as_deref(),
                        &rename_id[..],
                    ],
                )
                .context("update mail_domain_renames aborted")?;
            if updated != 1 {
                return Err(RenameTransitionError::WrongState {
                    attempted: "abort-from-grace",
                    actual: CAS_RACE_ACTUAL.into(),
                }
                .into());
            }
            tx.commit().context("commit abort-from-grace transaction")?;
        }
        self.lookup_rename_by_id(rename_id)
            .await?
            .ok_or_else(|| anyhow!("mail_domain_renames row vanished after abort-from-grace"))
    }

    /// Push a rename's grace window out (`mail-primary-domain-rename.md`
    /// § Wire shapes — `extend_primary_domain_rename_grace`, SLICE 4).
    /// `grace_ends_at += additional_days × 1 day` and `state = grace` — a
    /// `ready_to_complete` row **reverts to `grace`** (the deadline is in the
    /// future again, so the watcher re-promotes it when the new deadline passes;
    /// leaving it `ready_to_complete` with a future deadline would be self-
    /// contradictory), and its stale `ready_to_complete_at` is cleared. The
    /// original flip stamps (`flipped_at`/`grace_started_at`) are preserved —
    /// only the window end moves. Valid from `grace` | `ready_to_complete`;
    /// `additional_days` is range-validated by the handler. Refuses a not-found
    /// id or a non-post-flip state.
    pub async fn extend_rename_grace(
        &self,
        rename_id: &[u8; 16],
        additional_days: i64,
    ) -> Result<MailDomainRename> {
        let existing = self
            .lookup_rename_by_id(rename_id)
            .await?
            .ok_or(RenameTransitionError::NotFound)?;
        match existing.parsed_state() {
            Some(RenameState::Grace | RenameState::ReadyToComplete) => {}
            other => {
                return Err(RenameTransitionError::WrongState {
                    attempted: "extend grace",
                    actual: format!("{other:?}"),
                }
                .into());
            }
        }
        let current_end = existing
            .grace_ends_at
            .ok_or_else(|| anyhow!("post-flip rename missing grace_ends_at"))?;
        let new_end = current_end + additional_days * 86_400_000;
        {
            let conn = self.conn.lock().await;
            // CAS on the post-flip-active states — a concurrent complete/abort
            // makes this refuse rather than push a terminal row's deadline.
            let rows = conn
                .execute(
                    &format!(
                        "UPDATE mail_domain_renames
                         SET state = ?1, grace_ends_at = ?2, ready_to_complete_at = NULL
                         WHERE rename_id = ?3 AND state IN ('{}', '{}')",
                        RenameState::Grace.as_str(),
                        RenameState::ReadyToComplete.as_str(),
                    ),
                    rusqlite::params![RenameState::Grace.as_str(), new_end, &rename_id[..]],
                )
                .context("update mail_domain_renames extend grace")?;
            if rows != 1 {
                return Err(RenameTransitionError::WrongState {
                    attempted: "extend grace",
                    actual: CAS_RACE_ACTUAL.into(),
                }
                .into());
            }
        }
        self.lookup_rename_by_id(rename_id)
            .await?
            .ok_or_else(|| anyhow!("mail_domain_renames row vanished after grace extend"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    const OLD: [u8; 16] = [1u8; 16];
    const NEW: [u8; 16] = [2u8; 16];
    const OTHER: [u8; 16] = [3u8; 16];
    const ACTOR: [u8; 32] = [9u8; 32];

    #[tokio::test]
    async fn insert_then_get_active_returns_requested_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        assert_eq!(row.state, "requested");
        assert_eq!(row.parsed_state(), Some(RenameState::Requested));
        assert_eq!(row.old_primary_domain_id, OLD);
        assert_eq!(row.new_primary_domain_id, NEW);
        assert_eq!(row.grace_days, 7);
        assert_eq!(row.initiated_by_actor_id, ACTOR);
        assert!(row.aborted_at.is_none() && row.completed_at.is_none());

        let active = db.get_active_rename().await.unwrap().unwrap();
        assert_eq!(active.rename_id, row.rename_id);

        // Round-trips by id.
        let by_id = db.lookup_rename_by_id(&row.rename_id).await.unwrap();
        assert_eq!(by_id, Some(row));
    }

    #[tokio::test]
    async fn second_active_insert_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        db.insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        // A second non-terminal rename is refused (single-active invariant).
        let err = db.insert_domain_rename(&OLD, &OTHER, 7, &ACTOR).await;
        assert!(err.is_err(), "second active rename must be refused");
    }

    #[tokio::test]
    async fn insert_allows_new_after_abort() {
        let db = CacheDb::open_in_memory().unwrap();
        let first = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        db.mark_rename_aborted(&first.rename_id, None)
            .await
            .unwrap();
        // The terminal row no longer blocks a fresh rename.
        let second = db
            .insert_domain_rename(&OLD, &OTHER, 14, &ACTOR)
            .await
            .unwrap();
        let active = db.get_active_rename().await.unwrap().unwrap();
        assert_eq!(active.rename_id, second.rename_id);
    }

    #[tokio::test]
    async fn list_returns_all_including_aborted_newest_first() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        db.mark_rename_aborted(&a.rename_id, None).await.unwrap();
        let b = db
            .insert_domain_rename(&OLD, &OTHER, 7, &ACTOR)
            .await
            .unwrap();
        db.mark_rename_aborted(&b.rename_id, None).await.unwrap();
        let c = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();

        let all = db.list_domain_renames().await.unwrap();
        assert_eq!(all.len(), 3, "all rows including terminal ones");
        // started_at is non-increasing (DESC; ties allowed on same-ms inserts).
        for w in all.windows(2) {
            assert!(w[0].started_at >= w[1].started_at);
        }
        let ids: std::collections::HashSet<_> = all.iter().map(|r| r.rename_id).collect();
        assert!(
            ids.contains(&a.rename_id) && ids.contains(&b.rename_id) && ids.contains(&c.rename_id)
        );
    }

    #[tokio::test]
    async fn mark_aborted_sets_state_and_reason() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        let aborted = db
            .mark_rename_aborted(&row.rename_id, Some("changed my mind".into()))
            .await
            .unwrap();
        assert_eq!(aborted.state, "aborted");
        assert!(aborted.aborted_at.is_some());
        assert_eq!(aborted.abort_reason.as_deref(), Some("changed my mind"));
        // No longer the active rename.
        assert!(db.get_active_rename().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn mark_aborted_on_terminal_row_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        db.mark_rename_aborted(&row.rename_id, None).await.unwrap();
        let err = db.mark_rename_aborted(&row.rename_id, None).await;
        assert!(
            err.is_err(),
            "aborting an already-terminal rename must fail"
        );
    }

    #[tokio::test]
    async fn mark_aborted_unknown_id_not_found() {
        let db = CacheDb::open_in_memory().unwrap();
        let err = db.mark_rename_aborted(&[7u8; 16], None).await;
        assert!(err.is_err(), "aborting an unknown rename must fail");
    }

    #[tokio::test]
    async fn same_domain_ids_rejected_by_check() {
        let db = CacheDb::open_in_memory().unwrap();
        // old == new violates the CHECK constraint.
        let err = db.insert_domain_rename(&OLD, &OLD, 7, &ACTOR).await;
        assert!(err.is_err(), "old==new must be rejected by the CHECK");
    }

    // --- SLICE 2: cert-acquisition transitions ---

    #[tokio::test]
    async fn advance_requested_to_cert_issuance_sets_state() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        let advanced = db
            .advance_rename_to_cert_issuance(&row.rename_id)
            .await
            .unwrap();
        assert_eq!(advanced.parsed_state(), Some(RenameState::CertIssuance));
        // Still the active rename (cert_issuance is non-terminal).
        let active = db.get_active_rename().await.unwrap().unwrap();
        assert_eq!(active.parsed_state(), Some(RenameState::CertIssuance));
    }

    #[tokio::test]
    async fn advance_to_cert_issuance_is_idempotent() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        db.advance_rename_to_cert_issuance(&row.rename_id)
            .await
            .unwrap();
        // Re-asserting on a later tick is a no-op (§ Crash recovery).
        let again = db
            .advance_rename_to_cert_issuance(&row.rename_id)
            .await
            .unwrap();
        assert_eq!(again.parsed_state(), Some(RenameState::CertIssuance));
    }

    #[tokio::test]
    async fn advance_to_cert_issuance_from_terminal_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        db.mark_rename_aborted(&row.rename_id, None).await.unwrap();
        let err = db.advance_rename_to_cert_issuance(&row.rename_id).await;
        assert!(err.is_err(), "cannot advance an aborted rename");
    }

    #[tokio::test]
    async fn mark_cert_ready_stamps_fingerprint_and_time() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        db.advance_rename_to_cert_issuance(&row.rename_id)
            .await
            .unwrap();
        let ready = db
            .mark_rename_cert_ready(&row.rename_id, "abc123deadbeef", 1_700_000_000_000)
            .await
            .unwrap();
        assert_eq!(ready.parsed_state(), Some(RenameState::CertReady));
        assert_eq!(
            ready.new_cert_fingerprint.as_deref(),
            Some("abc123deadbeef")
        );
        assert_eq!(ready.cert_acquired_at, Some(1_700_000_000_000));
    }

    #[tokio::test]
    async fn mark_cert_ready_from_requested_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        // Must pass through cert_issuance first.
        let err = db.mark_rename_cert_ready(&row.rename_id, "fp", 1).await;
        assert!(err.is_err(), "cert_ready requires cert_issuance first");
    }

    #[tokio::test]
    async fn mark_cert_ready_twice_refused_preserves_original_stamp() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        db.advance_rename_to_cert_issuance(&row.rename_id)
            .await
            .unwrap();
        db.mark_rename_cert_ready(&row.rename_id, "first-fp", 111)
            .await
            .unwrap();
        // A second stamp is refused (strict cert_issuance-only) so the original
        // acquisition time/fingerprint is never overwritten by a re-observe.
        let err = db
            .mark_rename_cert_ready(&row.rename_id, "second-fp", 222)
            .await;
        assert!(err.is_err(), "re-stamping a cert_ready row must be refused");
        let row2 = db
            .lookup_rename_by_id(&row.rename_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row2.new_cert_fingerprint.as_deref(), Some("first-fp"));
        assert_eq!(row2.cert_acquired_at, Some(111));
    }

    // --- SLICE 3: the atomic anchor flip (cert_ready → grace) ---

    /// Seed two **real** `mail_domains` rows (old primary + new secondary) and a
    /// rename advanced to `cert_ready`, returning `(rename_id, old_id, new_id)`.
    async fn seed_cert_ready_rename(db: &CacheDb) -> ([u8; 16], [u8; 16], [u8; 16]) {
        let old = db
            .add_mail_domain(
                "old.example.com",
                true,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .expect("old primary");
        let new = db
            .add_mail_domain(
                "new.example.com",
                false,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .expect("new secondary");
        let row = db
            .insert_domain_rename(&old.domain_id, &new.domain_id, 7, &ACTOR)
            .await
            .expect("insert rename");
        db.advance_rename_to_cert_issuance(&row.rename_id)
            .await
            .expect("cert_issuance");
        db.mark_rename_cert_ready(&row.rename_id, "fp", 111)
            .await
            .expect("cert_ready");
        (row.rename_id, old.domain_id, new.domain_id)
    }

    #[tokio::test]
    async fn advance_to_grace_flips_is_primary_and_stamps() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, old_id, new_id) = seed_cert_ready_rename(&db).await;

        let row = db.advance_rename_to_grace(&rid).await.unwrap();
        assert_eq!(row.parsed_state(), Some(RenameState::Grace));
        assert!(row.flipped_at.is_some(), "flipped_at stamped");
        assert_eq!(
            row.grace_started_at, row.flipped_at,
            "grace_started_at == flipped_at in the steady case"
        );
        assert_eq!(
            row.grace_ends_at,
            Some(row.flipped_at.unwrap() + 7 * 86_400_000),
            "grace_ends_at == flipped_at + grace_days days"
        );

        // The is_primary flip is atomic: new is now the sole primary, old is not.
        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(primary.domain_id, new_id, "new domain is the primary");
        let old = db.lookup_mail_domain_by_id(&old_id).await.unwrap().unwrap();
        assert!(!old.is_primary, "old domain is demoted");
        let new = db.lookup_mail_domain_by_id(&new_id).await.unwrap().unwrap();
        assert!(new.is_primary, "new domain is promoted");
    }

    #[tokio::test]
    async fn advance_to_grace_from_non_cert_ready_refused_no_flip() {
        let db = CacheDb::open_in_memory().unwrap();
        let old = db
            .add_mail_domain(
                "old.example.com",
                true,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        let new = db
            .add_mail_domain(
                "new.example.com",
                false,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        let row = db
            .insert_domain_rename(&old.domain_id, &new.domain_id, 7, &ACTOR)
            .await
            .unwrap();
        // Still `requested` — the flip is refused and is_primary is untouched.
        assert!(db.advance_rename_to_grace(&row.rename_id).await.is_err());
        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(
            primary.domain_id, old.domain_id,
            "no flip from a pre-cert_ready state"
        );
    }

    #[tokio::test]
    async fn advance_to_grace_twice_refused_preserves_stamps() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _old, new_id) = seed_cert_ready_rename(&db).await;
        let first = db.advance_rename_to_grace(&rid).await.unwrap();
        // A second call (row already `grace`) is a strict-transition refusal, so
        // the flip stamps never move and the primary stays flipped.
        assert!(db.advance_rename_to_grace(&rid).await.is_err());
        let row = db.lookup_rename_by_id(&rid).await.unwrap().unwrap();
        assert_eq!(row.flipped_at, first.flipped_at);
        assert_eq!(row.grace_ends_at, first.grace_ends_at);
        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(
            primary.domain_id, new_id,
            "still flipped, not double-flipped"
        );
    }

    #[tokio::test]
    async fn advance_to_grace_rolls_back_when_new_domain_missing() {
        let db = CacheDb::open_in_memory().unwrap();
        // A cert_ready rename whose new domain was never a real mail_domains row
        // (dummy ids). The flip touches 1 old + 0 new → rollback, stays cert_ready.
        let old = db
            .add_mail_domain(
                "old.example.com",
                true,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        let row = db
            .insert_domain_rename(&old.domain_id, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        db.advance_rename_to_cert_issuance(&row.rename_id)
            .await
            .unwrap();
        db.mark_rename_cert_ready(&row.rename_id, "fp", 111)
            .await
            .unwrap();

        assert!(db.advance_rename_to_grace(&row.rename_id).await.is_err());
        // Rolled back: the rename stays cert_ready and the old domain stays primary.
        let after = db
            .lookup_rename_by_id(&row.rename_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.parsed_state(), Some(RenameState::CertReady));
        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(primary.domain_id, old.domain_id, "half-flip rolled back");
    }

    // --- SLICE 4: grace watcher + complete + abort-from-flip inverse + extend ---

    /// Seed a rename advanced all the way to `grace` (post-flip), returning
    /// `(rename_id, old_id, new_id)`. `old.example.com` is the old primary,
    /// `new.example.com` the promoted new primary.
    async fn seed_grace_rename(db: &CacheDb) -> ([u8; 16], [u8; 16], [u8; 16]) {
        let (rid, old_id, new_id) = seed_cert_ready_rename(db).await;
        db.advance_rename_to_grace(&rid).await.expect("grace");
        (rid, old_id, new_id)
    }

    #[tokio::test]
    async fn advance_grace_to_ready_to_complete_stamps() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _o, _n) = seed_grace_rename(&db).await;
        let row = db.advance_rename_to_ready_to_complete(&rid).await.unwrap();
        assert_eq!(row.parsed_state(), Some(RenameState::ReadyToComplete));
        assert!(row.ready_to_complete_at.is_some());
        // Still active (ready_to_complete is non-terminal).
        assert!(db.get_active_rename().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn advance_to_ready_to_complete_from_non_grace_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        // Still `cert_ready` (pre-flip) — the watcher promotion is refused.
        let (rid, _o, _n) = seed_cert_ready_rename(&db).await;
        assert!(db.advance_rename_to_ready_to_complete(&rid).await.is_err());
    }

    #[tokio::test]
    async fn complete_from_ready_to_complete_is_terminal() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _o, new_id) = seed_grace_rename(&db).await;
        db.advance_rename_to_ready_to_complete(&rid).await.unwrap();
        let row = db.complete_rename(&rid).await.unwrap();
        assert_eq!(row.parsed_state(), Some(RenameState::Completed));
        assert!(row.completed_at.is_some());
        // Terminal → no longer active; the DNS/cert keeps stop.
        assert!(db.get_active_rename().await.unwrap().is_none());
        // Complete does NOT touch is_primary — the new domain stays primary.
        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(primary.domain_id, new_id);
    }

    #[tokio::test]
    async fn complete_from_grace_works_force_gate_is_handler_side() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _o, _n) = seed_grace_rename(&db).await;
        // Storage accepts grace → completed; refusing grace-without-force is the
        // handler's policy, not the storage layer's.
        let row = db.complete_rename(&rid).await.unwrap();
        assert_eq!(row.parsed_state(), Some(RenameState::Completed));
    }

    #[tokio::test]
    async fn complete_from_pre_flip_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _o, _n) = seed_cert_ready_rename(&db).await;
        assert!(db.complete_rename(&rid).await.is_err());
    }

    #[tokio::test]
    async fn abort_from_grace_reflips_is_primary_back_and_is_terminal() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, old_id, new_id) = seed_grace_rename(&db).await;
        // Pre-abort: new is primary.
        assert_eq!(
            db.lookup_primary_mail_domain()
                .await
                .unwrap()
                .unwrap()
                .domain_id,
            new_id
        );
        let row = db
            .abort_rename_from_grace(&rid, Some("rolling back".into()))
            .await
            .unwrap();
        assert_eq!(row.parsed_state(), Some(RenameState::Aborted));
        assert!(row.aborted_at.is_some());
        assert_eq!(row.abort_reason.as_deref(), Some("rolling back"));
        // is_primary re-flipped back to OLD atomically.
        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(primary.domain_id, old_id, "old domain restored as primary");
        assert!(
            !db.lookup_mail_domain_by_id(&new_id)
                .await
                .unwrap()
                .unwrap()
                .is_primary,
            "new domain demoted back to secondary"
        );
        // Terminal.
        assert!(db.get_active_rename().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn abort_from_grace_from_ready_to_complete_works() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, old_id, _n) = seed_grace_rename(&db).await;
        db.advance_rename_to_ready_to_complete(&rid).await.unwrap();
        let row = db.abort_rename_from_grace(&rid, None).await.unwrap();
        assert_eq!(row.parsed_state(), Some(RenameState::Aborted));
        assert_eq!(
            db.lookup_primary_mail_domain()
                .await
                .unwrap()
                .unwrap()
                .domain_id,
            old_id
        );
    }

    #[tokio::test]
    async fn abort_from_grace_refused_pre_flip() {
        let db = CacheDb::open_in_memory().unwrap();
        // cert_ready = pre-flip — abort_rename_from_grace is not its job (the
        // handler routes a pre-flip abort to `mark_rename_aborted`).
        let (rid, old_id, _n) = seed_cert_ready_rename(&db).await;
        assert!(db.abort_rename_from_grace(&rid, None).await.is_err());
        // is_primary untouched (old is still primary — the flip never happened).
        assert_eq!(
            db.lookup_primary_mail_domain()
                .await
                .unwrap()
                .unwrap()
                .domain_id,
            old_id
        );
    }

    #[tokio::test]
    async fn abort_from_grace_rolls_back_when_old_domain_missing() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _old_id, new_id) = seed_grace_rename(&db).await;
        // Old primary was demoted at flip → now a soft-deletable secondary.
        // Remove it mid-grace: the re-promote touches 0 rows → rollback.
        db.soft_delete_mail_domain("old.example.com").await.unwrap();
        assert!(db.abort_rename_from_grace(&rid, None).await.is_err());
        // Rolled back: the rename stays `grace` and new stays primary.
        let after = db.lookup_rename_by_id(&rid).await.unwrap().unwrap();
        assert_eq!(after.parsed_state(), Some(RenameState::Grace));
        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(
            primary.domain_id, new_id,
            "no half-reflip when the old domain is gone"
        );
    }

    #[tokio::test]
    async fn extend_grace_pushes_deadline_and_reverts_state() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _o, _n) = seed_grace_rename(&db).await;
        db.advance_rename_to_ready_to_complete(&rid).await.unwrap();
        let before = db.lookup_rename_by_id(&rid).await.unwrap().unwrap();
        let old_end = before.grace_ends_at.unwrap();
        let row = db.extend_rename_grace(&rid, 3).await.unwrap();
        assert_eq!(
            row.parsed_state(),
            Some(RenameState::Grace),
            "extend reverts ready_to_complete → grace"
        );
        assert_eq!(row.grace_ends_at, Some(old_end + 3 * 86_400_000));
        assert!(
            row.ready_to_complete_at.is_none(),
            "ready_to_complete_at cleared on revert"
        );
        // The original flip stamps are preserved.
        assert_eq!(row.flipped_at, before.flipped_at);
        assert_eq!(row.grace_started_at, before.grace_started_at);
    }

    #[tokio::test]
    async fn extend_grace_from_grace_keeps_grace() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _o, _n) = seed_grace_rename(&db).await;
        let before = db.lookup_rename_by_id(&rid).await.unwrap().unwrap();
        let row = db.extend_rename_grace(&rid, 5).await.unwrap();
        assert_eq!(row.parsed_state(), Some(RenameState::Grace));
        assert_eq!(
            row.grace_ends_at,
            Some(before.grace_ends_at.unwrap() + 5 * 86_400_000)
        );
    }

    #[tokio::test]
    async fn extend_grace_refused_from_pre_flip() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _o, _n) = seed_cert_ready_rename(&db).await;
        assert!(db.extend_rename_grace(&rid, 1).await.is_err());
    }

    // --- Hardening: state-conditional (CAS) transitions ---

    /// The flagship CAS test — race (a). The pre-flip `mark_rename_aborted` must
    /// refuse a row that was flipped to `grace` (or `ready_to_complete`) after
    /// the handler routed to it, leaving the row post-flip/flipped — **never**
    /// `aborted` with `is_primary` still flipped and no inverse run. The CAS on
    /// the pre-flip set (`requested`/`cert_issuance`/`cert_ready`) is what
    /// enforces this; the precondition read (terminal-only) does NOT — a `grace`
    /// row is non-terminal, so without the CAS it would clobber grace → aborted.
    #[tokio::test]
    async fn mark_aborted_refused_on_flipped_row_leaves_it_flipped() {
        for advance_to_ready in [false, true] {
            let db = CacheDb::open_in_memory().unwrap();
            let (rid, _old_id, new_id) = seed_grace_rename(&db).await;
            let expect_state = if advance_to_ready {
                db.advance_rename_to_ready_to_complete(&rid).await.unwrap();
                RenameState::ReadyToComplete
            } else {
                RenameState::Grace
            };

            let err = db.mark_rename_aborted(&rid, None).await;
            assert!(
                err.is_err(),
                "pre-flip mark_rename_aborted must refuse a flipped ({expect_state:?}) row"
            );
            // The CAS refusal is the typed WrongState (→ 409 rename_wrong_state).
            assert!(
                matches!(
                    err.unwrap_err().downcast_ref::<RenameTransitionError>(),
                    Some(RenameTransitionError::WrongState { .. })
                ),
                "CAS 0-row refusal must be the typed WrongState"
            );
            // The row stays post-flip/flipped — not clobbered to a terminal
            // `aborted` with `is_primary` still on the new domain.
            let row = db.lookup_rename_by_id(&rid).await.unwrap().unwrap();
            assert_eq!(
                row.parsed_state(),
                Some(expect_state),
                "row stays {expect_state:?}, not clobbered to aborted"
            );
            assert!(row.aborted_at.is_none(), "no terminal abort stamp written");
            let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
            assert_eq!(
                primary.domain_id, new_id,
                "still flipped to the new primary — no stranded half-terminal state"
            );
        }
    }

    /// Race (b): after an abort-from-grace commits (→ `aborted` + re-flip to old),
    /// the watcher's `advance_rename_to_ready_to_complete` must not resurrect the
    /// terminal row. (The precondition read also catches this — the CAS is the
    /// belt-and-suspenders that closes the read↔UPDATE gap; both must refuse.)
    #[tokio::test]
    async fn watcher_promotion_refused_on_aborted_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, old_id, _new) = seed_grace_rename(&db).await;
        db.abort_rename_from_grace(&rid, None).await.unwrap();

        let err = db.advance_rename_to_ready_to_complete(&rid).await;
        assert!(err.is_err(), "promoting an aborted row must be refused");
        let row = db.lookup_rename_by_id(&rid).await.unwrap().unwrap();
        assert_eq!(
            row.parsed_state(),
            Some(RenameState::Aborted),
            "stays aborted — not resurrected to ready_to_complete"
        );
        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(
            primary.domain_id, old_id,
            "stays re-flipped to the old primary"
        );
    }

    /// A wrong-state transition surfaces the typed [`RenameTransitionError::WrongState`]
    /// (downcastable) so the RPC layer maps it to `409 rename_wrong_state` instead
    /// of a string-matched `500 internal`.
    #[tokio::test]
    async fn wrong_state_transition_returns_typed_error() {
        let db = CacheDb::open_in_memory().unwrap();
        let (rid, _o, _n) = seed_cert_ready_rename(&db).await;
        // `complete` from `cert_ready` (pre-flip) is a wrong-state refusal.
        let err = db.complete_rename(&rid).await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<RenameTransitionError>(),
                Some(RenameTransitionError::WrongState { .. })
            ),
            "wrong-state transition must surface the typed WrongState error; got: {err:#}"
        );
    }

    /// Not-found and already-in-progress refusals are typed too, so the whole
    /// `map_rename_err` mapping is downcast-based (no string matching left).
    #[tokio::test]
    async fn not_found_and_already_in_progress_are_typed() {
        let db = CacheDb::open_in_memory().unwrap();
        let not_found = db.complete_rename(&[42u8; 16]).await.unwrap_err();
        assert!(matches!(
            not_found.downcast_ref::<RenameTransitionError>(),
            Some(RenameTransitionError::NotFound)
        ));

        db.insert_domain_rename(&OLD, &NEW, 7, &ACTOR)
            .await
            .unwrap();
        let in_progress = db
            .insert_domain_rename(&OLD, &OTHER, 7, &ACTOR)
            .await
            .unwrap_err();
        assert!(matches!(
            in_progress.downcast_ref::<RenameTransitionError>(),
            Some(RenameTransitionError::AlreadyInProgress)
        ));
    }
}
