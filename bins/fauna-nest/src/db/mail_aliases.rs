//! Per-account alias storage — production source of truth for
//! `(local_domain, pattern, kind) → actor_id` recipient routing.
//!
//! `fauna.bridges.validate_recipient` (the MTA's RCPT TO gate) resolves
//! through `lookup_exact_alias`. Replaces the standalone
//! `recipient_routes` table that a review
//! identified as never-written-in-production: every RCPT TO rejected
//! with "no such recipient" because `put_recipient_route` had only
//! unit-test callers.
//!
//! ## Scope
//!
//! The full alias program (`docs/goal/behavior/mail-aliases.md`,
//! § A2) lives here: exact + wildcard_prefix +
//! disposable kinds, per-alias controls (spam-threshold override / disable /
//! rate-limit / label), the user-facing CRUD + `generate_disposable_alias` +
//! `list_account_alias_hits` writers/readers, and `alias_hits` population at
//! resolve time (A2.4). `put_exact_alias` is the internal `kind='exact'`
//! writer behind the canonical `<handle>@<domain>` address and takes no
//! other kind; the user-tier `create_account_alias` accepts the
//! kinds the resolver understands.
//!
//! Authoritative spec: `docs/goal/behavior/mail-aliases.md`.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use uuid::Uuid;

use super::{CacheDb, blob_to_array, now_epoch_millis};

/// Canonical `account_aliases.kind` strings — re-exported from the shared
/// `fauna_mail::aliases` module so the kind discriminator has a single source
/// of truth across the matcher, the WS-RPC handlers, and this DB layer.
pub use fauna_mail::aliases::{
    ALIAS_KIND_DISPOSABLE, ALIAS_KIND_EXACT, ALIAS_KIND_FORWARDER, ALIAS_KIND_LIST,
    ALIAS_KIND_WILDCARD_PREFIX,
};

/// `(label, spam_threshold_override, rate_limit_per_hour/day)` bundle for
/// the user-tier create/update path. Mirrors the wire `AliasControls`;
/// `None` = inherit / unlimited (stored NULL). The spam threshold is whole
/// spam-points (the `account_aliases.spam_threshold_override` column is
/// `REAL` but only ever holds integer values this slice — see
/// § A2 scope decision 3).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AliasControlsInput {
    pub label: String,
    pub spam_threshold_override: Option<u32>,
    pub rate_limit_per_hour: Option<i64>,
    pub rate_limit_per_day: Option<i64>,
}

/// Outcome of a uniqueness-bearing alias write. `Conflict` is the
/// `UNIQUE (local_domain, pattern, kind)` violation (two aliases of the
/// same kind can't share a pattern on a domain — `mail-aliases.md` § Cross-
/// user uniqueness); `KeyHeldByOtherKind` is the exact-key tier's one-holder
/// rule ([`exact_key_held_by_other_kind`]), which the UNIQUE index cannot see.
/// The handlers map both to `fauna.bridges.conflicts_with_existing_alias`;
/// bulk import tells them apart only to word its per-line outcome.
#[derive(Debug)]
pub enum AliasWriteError {
    Conflict,
    KeyHeldByOtherKind,
    Db(anyhow::Error),
}

impl std::fmt::Display for AliasWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AliasWriteError::Conflict => {
                write!(f, "alias pattern conflicts with an existing alias")
            }
            AliasWriteError::KeyHeldByOtherKind => write!(
                f,
                "alias address is held by an exact alias, forwarder or list"
            ),
            AliasWriteError::Db(e) => write!(f, "{e}"),
        }
    }
}

/// One `account_aliases` row, in-process shape. The handler converts this
/// to the wire `AliasRow` (`bridge_routing.rs`).
#[derive(Debug, Clone, PartialEq)]
pub struct AliasRecord {
    pub alias_id: [u8; 16],
    pub actor_id: [u8; 32],
    pub local_domain: String,
    pub kind: String,
    pub pattern: String,
    /// Forwarder-only: the external destination (`kind='forwarder'`). `None`
    /// for every other kind (`mail-aliases.md` § Storage `:181`).
    pub forward_target: Option<String>,
    pub label: String,
    pub disabled: bool,
    pub spam_threshold_override: Option<u32>,
    pub rate_limit_per_hour: Option<i64>,
    pub rate_limit_per_day: Option<i64>,
    pub uses_remaining: Option<i64>,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub last_hit_at: Option<i64>,
    pub hit_count: i64,
}

/// One `alias_hits` audit row (`mail-aliases.md` § Storage `:180-186`). The
/// handler converts this to the wire `AliasHitRow` (`bridge_routing.rs`); the
/// owning `alias_id` is implied by the query, so it's omitted here.
#[derive(Debug, Clone, PartialEq)]
pub struct AliasHitRecord {
    pub hit_id: [u8; 16],
    pub matched_address: String,
    pub sender_domain: String,
    pub received_at: i64,
}

impl CacheDb {
    /// Upsert a `kind='exact'` alias mapping `(local_domain, pattern)`
    /// to `actor_id`. The (local_domain, pattern, kind) tuple is
    /// unique — re-puts on the same (local_domain, pattern) replace
    /// the actor_id atomically.
    ///
    /// `kind` is parameterised to keep the future-multikind shape
    /// visible at the call site (admin handler decodes a `kind` string
    /// from the wire) but is REJECTED for anything other than
    /// `"exact"` until a later slice lands the rest of
    /// the resolver. A key a forwarder or list already holds is
    /// [`AliasWriteError::KeyHeldByOtherKind`].
    pub async fn put_exact_alias(
        &self,
        local_domain: &str,
        pattern: &str,
        kind: &str,
        actor_id: &[u8; 32],
    ) -> std::result::Result<(), AliasWriteError> {
        if kind != ALIAS_KIND_EXACT {
            return Err(AliasWriteError::Db(anyhow::anyhow!(
                "only kind=exact is implemented yet (got kind={kind}); \
                 +suffix / wildcard_prefix / disposable / catchall + \
                 the resolver + per-alias controls are not built yet"
            )));
        }
        let local_domain = local_domain.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        if exact_key_held_by_other_kind(&conn, &local_domain, &pattern, ALIAS_KIND_EXACT)
            .map_err(AliasWriteError::Db)?
        {
            return Err(AliasWriteError::KeyHeldByOtherKind);
        }
        // Existing row on (local_domain, pattern, kind=exact)? Upsert
        // the actor_id but leave alias_id stable — alias_hits FK ties
        // audit rows to alias_id, so we don't churn the id on every
        // re-put. New rows get a fresh UUID v4.
        let existing_id: Option<Vec<u8>> = conn
            .query_row(
                "SELECT alias_id FROM account_aliases
                 WHERE local_domain = ?1 AND pattern = ?2 AND kind = ?3",
                rusqlite::params![&local_domain, &pattern, ALIAS_KIND_EXACT],
                |row| row.get(0),
            )
            .optional()
            .context("lookup existing exact alias")
            .map_err(AliasWriteError::Db)?;
        match existing_id {
            Some(_) => {
                conn.execute(
                    "UPDATE account_aliases SET actor_id = ?1
                     WHERE local_domain = ?2 AND pattern = ?3 AND kind = ?4",
                    rusqlite::params![&actor[..], &local_domain, &pattern, ALIAS_KIND_EXACT,],
                )
                .context("update existing exact alias")
                .map_err(AliasWriteError::Db)?;
            }
            None => {
                let alias_id = *Uuid::new_v4().as_bytes();
                conn.execute(
                    "INSERT INTO account_aliases
                        (alias_id, actor_id, local_domain, kind, pattern,
                         label, disabled, hit_count, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, '', 0, 0, ?6)",
                    rusqlite::params![
                        &alias_id[..],
                        &actor[..],
                        &local_domain,
                        ALIAS_KIND_EXACT,
                        &pattern,
                        now,
                    ],
                )
                .context("insert exact alias")
                .map_err(AliasWriteError::Db)?;
            }
        }
        Ok(())
    }

    /// Look up the actor_id for an exact alias on `(local_domain,
    /// pattern)`. Single index hit on
    /// `idx_account_aliases_exact (local_domain, pattern) WHERE kind = 'exact'`.
    pub async fn lookup_exact_alias(
        &self,
        local_domain: &str,
        pattern: &str,
    ) -> Result<Option<[u8; 32]>> {
        let local_domain = local_domain.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT actor_id FROM account_aliases
                 WHERE local_domain = ?1 AND pattern = ?2 AND kind = 'exact'",
                rusqlite::params![&local_domain, &pattern],
                |row| {
                    let v: Vec<u8> = row.get(0)?;
                    Ok(v)
                },
            )
            .optional()
            .context("lookup exact alias")?;
        match row {
            None => Ok(None),
            Some(v) => {
                let actor: [u8; 32] = blob_to_array(v.as_slice(), "account_aliases.actor_id")?;
                Ok(Some(actor))
            }
        }
    }

    // ── User-tier alias CRUD (A2.1) ─────────
    //
    // The per-account user surface (`fauna.bridges.{list,create,update,
    // revoke,delete}_account_alias`). Unlike `put_exact_alias` (admin,
    // explicit target actor) these are owner-scoped: every write carries
    // the calling actor and scopes its `WHERE` by `(alias_id, actor_id)`,
    // so a user can only touch their own rows. Slice 1 is exact-kind, but
    // the writers accept any `kind` string (the handler is the kind
    // gatekeeper) so the wildcard/disposable slices reuse them unchanged.

    /// Count an actor's `kind='exact'` aliases across **all** local domains
    /// (the per-account cap is per-actor, not per-domain — `mail-aliases.md:287`).
    pub async fn count_exact_aliases_for_actor(&self, actor_id: &[u8; 32]) -> Result<u32> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM account_aliases
                 WHERE actor_id = ?1 AND kind = 'exact'",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .context("count exact aliases for actor")?;
        Ok(n.max(0) as u32)
    }

    /// List an actor's own alias rows across all local domains (the
    /// account-detail enumeration), newest first. Index hit on
    /// `idx_account_aliases_actor_kind (actor_id, kind)`. Excludes
    /// `kind='forwarder'` — admin external forwarders are deployment routing
    /// config, never a user's personal alias (`mail-aliases.md` § Kind 7
    /// `:111`); the managing admin sees them only via `list_forwarders`.
    pub async fn list_aliases_for_actor(&self, actor_id: &[u8; 32]) -> Result<Vec<AliasRecord>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                // Exclude both deployment-config forwarders and `kind='list'`
                // mailing-list addresses — lists are managed on the separate
                // `mail-lists` page via the `*_account_list` RPCs, not the
                // personal-alias surface (`mail-mass-mailing.md` § List as a
                // sixth alias kind).
                "SELECT {ALIAS_SELECT_COLS} FROM account_aliases
                 WHERE actor_id = ?1
                   AND kind NOT IN ('{ALIAS_KIND_FORWARDER}', '{ALIAS_KIND_LIST}')
                 ORDER BY created_at DESC, rowid DESC"
            ))
            .context("prepare list aliases")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], row_to_alias_record)
            .context("query list aliases")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("read alias row")?);
        }
        Ok(out)
    }

    /// Create one `account_aliases` row owned by `actor_id`. Returns the
    /// new 16-byte UUID. `local_domain` + `pattern` are lower-cased (the
    /// resolver compares `LOWER(...)`); a `UNIQUE (local_domain, pattern,
    /// kind)` collision surfaces as [`AliasWriteError::Conflict`], and an
    /// exact-key-tier `kind` on a key another tier kind holds as
    /// [`AliasWriteError::KeyHeldByOtherKind`].
    ///
    /// This `account_aliases` taxonomy superseded — and fully replaced — the
    /// pre-taxonomy `local_part → target-string` model of the old
    /// `email_aliases` table (the `/api/v1/email/aliases` HTTP surface); that
    /// table + its `CacheDb` accessors were removed, and the dead schema is
    /// dropped in migrations.
    pub async fn create_account_alias(
        &self,
        actor_id: &[u8; 32],
        local_domain: &str,
        kind: &str,
        pattern: &str,
        controls: &AliasControlsInput,
    ) -> std::result::Result<[u8; 16], AliasWriteError> {
        let local_domain = local_domain.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();
        let actor = *actor_id;
        let alias_id = *Uuid::new_v4().as_bytes();
        let now = now_epoch_millis();
        let spam = controls.spam_threshold_override.map(|v| v as f64);
        let conn = self.conn.lock().await;
        if exact_key_held_by_other_kind(&conn, &local_domain, &pattern, kind)
            .map_err(AliasWriteError::Db)?
        {
            return Err(AliasWriteError::KeyHeldByOtherKind);
        }
        conn.execute(
            "INSERT INTO account_aliases
                (alias_id, actor_id, local_domain, kind, pattern, label,
                 disabled, spam_threshold_override, rate_limit_per_hour,
                 rate_limit_per_day, hit_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, 0, ?10)",
            rusqlite::params![
                &alias_id[..],
                &actor[..],
                &local_domain,
                kind,
                &pattern,
                &controls.label,
                spam,
                controls.rate_limit_per_hour,
                controls.rate_limit_per_day,
                now,
            ],
        )
        .map_err(map_alias_write_err)?;
        Ok(alias_id)
    }

    /// Full-overwrite the editable fields of an alias the caller owns
    /// (`pattern` + the four controls; `kind` is immutable). Owner-scoped:
    /// returns `Ok(false)` if no row matches `(alias_id, actor_id)`. A
    /// rename into an existing `(local_domain, pattern, kind)` is
    /// [`AliasWriteError::Conflict`]; renaming an exact-key-tier row onto a
    /// key another tier kind holds is [`AliasWriteError::KeyHeldByOtherKind`].
    pub async fn update_alias_controls(
        &self,
        alias_id: &[u8; 16],
        actor_id: &[u8; 32],
        pattern: &str,
        controls: &AliasControlsInput,
    ) -> std::result::Result<bool, AliasWriteError> {
        let id = *alias_id;
        let actor = *actor_id;
        let pattern = pattern.to_ascii_lowercase();
        let spam = controls.spam_threshold_override.map(|v| v as f64);
        let conn = self.conn.lock().await;
        // `kind` and `local_domain` are immutable, so the row's own values
        // decide which tier the renamed key joins.
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT local_domain, kind FROM account_aliases
                 WHERE alias_id = ?1 AND actor_id = ?2",
                rusqlite::params![&id[..], &actor[..]],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("lookup alias for update")
            .map_err(AliasWriteError::Db)?;
        let Some((local_domain, kind)) = row else {
            return Ok(false);
        };
        if exact_key_held_by_other_kind(&conn, &local_domain, &pattern, &kind)
            .map_err(AliasWriteError::Db)?
        {
            return Err(AliasWriteError::KeyHeldByOtherKind);
        }
        let n = conn
            .execute(
                "UPDATE account_aliases
                 SET pattern = ?1, label = ?2, spam_threshold_override = ?3,
                     rate_limit_per_hour = ?4, rate_limit_per_day = ?5
                 WHERE alias_id = ?6 AND actor_id = ?7",
                rusqlite::params![
                    &pattern,
                    &controls.label,
                    spam,
                    controls.rate_limit_per_hour,
                    controls.rate_limit_per_day,
                    &id[..],
                    &actor[..],
                ],
            )
            .map_err(map_alias_write_err)?;
        Ok(n > 0)
    }

    /// Set `disabled` on an alias the caller owns (the `revoke` soft-off,
    /// `mail-aliases.md:224`). Owner-scoped; `Ok(false)` if no match.
    pub async fn set_alias_disabled(
        &self,
        alias_id: &[u8; 16],
        actor_id: &[u8; 32],
        disabled: bool,
    ) -> Result<bool> {
        let id = *alias_id;
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE account_aliases SET disabled = ?1
                 WHERE alias_id = ?2 AND actor_id = ?3",
                rusqlite::params![disabled as i64, &id[..], &actor[..]],
            )
            .context("set alias disabled")?;
        Ok(n > 0)
    }

    /// Destructively delete an `account_aliases` row the caller owns
    /// (cascades `alias_hits` via the FK). Owner-scoped; `Ok(false)` if no
    /// match. (The legacy `email_aliases` `delete_alias` it replaced is gone.)
    pub async fn delete_account_alias(
        &self,
        alias_id: &[u8; 16],
        actor_id: &[u8; 32],
    ) -> Result<bool> {
        let id = *alias_id;
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM account_aliases WHERE alias_id = ?1 AND actor_id = ?2",
                rusqlite::params![&id[..], &actor[..]],
            )
            .context("delete alias")?;
        Ok(n > 0)
    }

    // ── Admin external forwarders (mail-aliases.md § Kind 7 / § AF) ──
    //
    // `kind='forwarder'` rows: an address with no local mailbox forwarding to
    // an external `forward_target`, attributed to the managing admin actor.
    // Admin-scoped (not owner-scoped like the user CRUD above) — forwarders are
    // deployment routing config. Resolution + the user-list exclusion live in
    // the resolver / `list_aliases_for_actor` above.

    /// Insert one `kind='forwarder'` row attributed to the managing admin.
    /// `forward_target` is the external destination; `pattern` the local-part
    /// on `local_domain` (both lower-cased). A `UNIQUE (local_domain, pattern,
    /// kind)` collision (a forwarder already on this key) surfaces as
    /// [`AliasWriteError::Conflict`]. A key an exact alias or a list already
    /// holds is a *different* `kind` the UNIQUE constraint does not catch —
    /// [`AliasWriteError::KeyHeldByOtherKind`] (`exact_key_held_by_other_kind`).
    pub async fn create_forwarder_alias(
        &self,
        admin_actor_id: &[u8; 32],
        local_domain: &str,
        pattern: &str,
        forward_target: &str,
    ) -> std::result::Result<[u8; 16], AliasWriteError> {
        let local_domain = local_domain.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();
        let actor = *admin_actor_id;
        let alias_id = *Uuid::new_v4().as_bytes();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        if exact_key_held_by_other_kind(&conn, &local_domain, &pattern, ALIAS_KIND_FORWARDER)
            .map_err(AliasWriteError::Db)?
        {
            return Err(AliasWriteError::KeyHeldByOtherKind);
        }
        conn.execute(
            "INSERT INTO account_aliases
                (alias_id, actor_id, local_domain, kind, pattern, forward_target,
                 label, disabled, hit_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', 0, 0, ?7)",
            rusqlite::params![
                &alias_id[..],
                &actor[..],
                &local_domain,
                ALIAS_KIND_FORWARDER,
                &pattern,
                forward_target,
                now,
            ],
        )
        .map_err(map_alias_write_err)?;
        Ok(alias_id)
    }

    /// Every `kind='forwarder'` row across all local domains — the deployment's
    /// admin forwarders (`fauna.bridges.list_forwarders`, Admin), newest first.
    pub async fn list_forwarders(&self) -> Result<Vec<AliasRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {ALIAS_SELECT_COLS} FROM account_aliases
                 WHERE kind = '{ALIAS_KIND_FORWARDER}'
                 ORDER BY created_at DESC, rowid DESC"
            ))
            .context("prepare list forwarders")?;
        let rows = stmt
            .query_map([], row_to_alias_record)
            .context("query list forwarders")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("read forwarder row")?);
        }
        Ok(out)
    }

    /// Destructively delete a `kind='forwarder'` row by `alias_id` (cascades
    /// `alias_hits`). Admin-scoped — any admin may delete any forwarder. The
    /// `kind='forwarder'` guard prevents deleting a user's personal alias by
    /// id through this admin path. `Ok(false)` if no forwarder row matches.
    pub async fn delete_forwarder(&self, alias_id: &[u8; 16]) -> Result<bool> {
        let id = *alias_id;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                &format!(
                    "DELETE FROM account_aliases
                     WHERE alias_id = ?1 AND kind = '{ALIAS_KIND_FORWARDER}'"
                ),
                rusqlite::params![&id[..]],
            )
            .context("delete forwarder")?;
        Ok(n > 0)
    }

    // ── RCPT-TO resolver reads (A2.2) ───────
    //
    // The `resolve_recipient` handler fetches a small candidate set and feeds
    // the pure `fauna_mail::aliases::resolve_recipient` matcher; these are the
    // I/O half. Unlike `lookup_exact_alias` (which returns only the actor),
    // the resolver needs the full row (disabled + per-alias controls).

    /// Full exact-alias row for `(local_domain, pattern)`, via the
    /// `idx_account_aliases_exact` partial index. Returns the disabled flag +
    /// controls the resolver needs (`lookup_exact_alias` returns only the
    /// actor and is kept for the legacy exact-only `validate_recipient`).
    pub async fn lookup_exact_alias_record(
        &self,
        local_domain: &str,
        pattern: &str,
    ) -> Result<Option<AliasRecord>> {
        let local_domain = local_domain.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                &format!(
                    "SELECT {ALIAS_SELECT_COLS} FROM account_aliases
                     WHERE local_domain = ?1 AND pattern = ?2 AND kind = 'exact'"
                ),
                rusqlite::params![&local_domain, &pattern],
                row_to_alias_record,
            )
            .optional()
            .context("lookup exact alias record")?;
        Ok(row)
    }

    /// Full forwarder row for `(local_domain, pattern)` on the same exact key
    /// as `lookup_exact_alias_record`, via the `idx_account_aliases_forwarder`
    /// partial index. The resolver builds the matcher's `ForwarderCandidate`
    /// from it at step 2 (`mail-aliases.md` § Resolution order `:124`); the
    /// exact-create path uses it for the exact↔forwarder collision check
    /// (§ Kind 7 `:114`). `None` = no forwarder on this key.
    pub async fn lookup_forwarder_alias_record(
        &self,
        local_domain: &str,
        pattern: &str,
    ) -> Result<Option<AliasRecord>> {
        let local_domain = local_domain.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                &format!(
                    "SELECT {ALIAS_SELECT_COLS} FROM account_aliases
                     WHERE local_domain = ?1 AND pattern = ?2 AND kind = '{ALIAS_KIND_FORWARDER}'"
                ),
                rusqlite::params![&local_domain, &pattern],
                row_to_alias_record,
            )
            .optional()
            .context("lookup forwarder alias record")?;
        Ok(row)
    }

    /// Every `kind='wildcard_prefix'` row on a local domain (across all
    /// actors). Bounded — at most one wildcard per actor (`mail-aliases.md`
    /// § Kind 3); the matcher picks the longest-prefix winner.
    pub async fn list_wildcard_aliases_for_domain(
        &self,
        local_domain: &str,
    ) -> Result<Vec<AliasRecord>> {
        let local_domain = local_domain.to_ascii_lowercase();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {ALIAS_SELECT_COLS} FROM account_aliases
                 WHERE local_domain = ?1 AND kind = '{ALIAS_KIND_WILDCARD_PREFIX}'"
            ))
            .context("prepare list wildcard aliases")?;
        let rows = stmt
            .query_map(rusqlite::params![&local_domain], row_to_alias_record)
            .context("query list wildcard aliases")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("read wildcard alias row")?);
        }
        Ok(out)
    }

    // ── Wildcard-prefix create-time conflict checks (§ A2.2) ─────────

    /// True iff any **exact** alias on `local_domain` has a pattern that the
    /// wildcard glob `<prefix>*` would match (i.e. starts with `prefix`).
    /// `mail-aliases.md` § Cross-user uniqueness `:284`: a user's wildcard
    /// must not shadow another user's exact alias (exact wins at resolution,
    /// and the create-time check makes the conflict visible).
    pub async fn exact_alias_exists_under_prefix(
        &self,
        local_domain: &str,
        prefix: &str,
    ) -> Result<bool> {
        let local_domain = local_domain.to_ascii_lowercase();
        let prefix = prefix.to_ascii_lowercase();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT pattern FROM account_aliases
                 WHERE local_domain = ?1 AND kind = 'exact'",
            )
            .context("prepare exact-under-prefix scan")?;
        let mut rows = stmt
            .query(rusqlite::params![&local_domain])
            .context("query exact-under-prefix scan")?;
        while let Some(row) = rows.next().context("read exact pattern")? {
            let pattern: String = row.get(0)?;
            if pattern.starts_with(&prefix) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// True iff `actor_id` already owns a wildcard-prefix alias — one wildcard
    /// pattern per actor (`mail-aliases.md` § Kind 3 `:52`). Index hit on
    /// `idx_account_aliases_actor_kind (actor_id, kind)`.
    pub async fn actor_has_wildcard(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM account_aliases
                     WHERE actor_id = ?1 AND kind = '{ALIAS_KIND_WILDCARD_PREFIX}'"
                ),
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .context("count actor wildcard aliases")?;
        Ok(n > 0)
    }

    // ── Disposable mint + resolution (§ A2.3) ────────────────────────

    /// The actor's **canonical** address — the `(local_domain, pattern)` of
    /// their oldest `kind='exact'` alias (the handle the user chose at signup;
    /// `mail-aliases.md` § Kind 1 `:33`). The disposable mint derives both the
    /// `<handle>` and the hosting `<domain>` from this row, so a disposable is
    /// minted on the same domain as the user's canonical address. `None` = the
    /// actor has no exact alias yet (the mint rejects — they need their
    /// canonical address first).
    pub async fn oldest_exact_alias_for_actor(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<(String, String)>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT local_domain, pattern FROM account_aliases
                 WHERE actor_id = ?1 AND kind = 'exact'
                 ORDER BY created_at ASC, rowid ASC
                 LIMIT 1",
                rusqlite::params![&actor[..]],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .context("oldest exact alias for actor")?;
        Ok(row)
    }

    /// Count an actor's `kind='disposable'` rows minted at or after
    /// `since_millis` — the input to the per-day generate cap
    /// (`mail-aliases.md` § Don't `:327`). A rolling 24 h window (not a
    /// calendar day) keyed on `created_at`; counts *live* rows (a mint+delete
    /// loop is not defended here — the cap targets accidental / naive
    /// enumeration, and a deleted disposable no longer routes).
    pub async fn count_recent_disposable_mints(
        &self,
        actor_id: &[u8; 32],
        since_millis: i64,
    ) -> Result<u32> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM account_aliases
                 WHERE actor_id = ?1 AND kind = 'disposable' AND created_at >= ?2",
                rusqlite::params![&actor[..], since_millis],
                |row| row.get(0),
            )
            .context("count recent disposable mints")?;
        Ok(n.max(0) as u32)
    }

    /// Insert one `kind='disposable'` row. `pattern` is the base32 token
    /// (`mail-aliases.md` § Storage `:165`); `uses_remaining` is `None` =
    /// unlimited (the `uses = 0` mint), `Some(n)` = finite. A
    /// `UNIQUE (local_domain, pattern, kind)` collision (token re-use) surfaces
    /// as [`AliasWriteError::Conflict`] so the handler re-mints with a fresh
    /// token. The hard `rate_limit_per_day` default is the caller's
    /// (`DISPOSABLE_RATE_LIMIT_PER_DAY_DEFAULT`).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_disposable_alias(
        &self,
        actor_id: &[u8; 32],
        local_domain: &str,
        token: &str,
        label: &str,
        uses_remaining: Option<i64>,
        expires_at: i64,
        rate_limit_per_day: i64,
    ) -> std::result::Result<[u8; 16], AliasWriteError> {
        let local_domain = local_domain.to_ascii_lowercase();
        let token = token.to_ascii_lowercase();
        let actor = *actor_id;
        let alias_id = *Uuid::new_v4().as_bytes();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO account_aliases
                (alias_id, actor_id, local_domain, kind, pattern, label,
                 disabled, rate_limit_per_day, uses_remaining, expires_at,
                 hit_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, 0, ?10)",
            rusqlite::params![
                &alias_id[..],
                &actor[..],
                &local_domain,
                ALIAS_KIND_DISPOSABLE,
                &token,
                label,
                rate_limit_per_day,
                uses_remaining,
                expires_at,
                now,
            ],
        )
        .map_err(map_alias_write_err)?;
        Ok(alias_id)
    }

    /// Full disposable row for `(local_domain, token)` via the
    /// `idx_account_aliases_disposable` partial index. The resolver handler
    /// uses it to build the matcher's `DisposableCandidate` (aliveness +
    /// controls); `None` = no such token (the resolver then falls through).
    pub async fn lookup_disposable_alias_record(
        &self,
        local_domain: &str,
        token: &str,
    ) -> Result<Option<AliasRecord>> {
        let local_domain = local_domain.to_ascii_lowercase();
        let token = token.to_ascii_lowercase();
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                &format!(
                    "SELECT {ALIAS_SELECT_COLS} FROM account_aliases
                     WHERE local_domain = ?1 AND pattern = ?2 AND kind = 'disposable'"
                ),
                rusqlite::params![&local_domain, &token],
                row_to_alias_record,
            )
            .optional()
            .context("lookup disposable alias record")?;
        Ok(row)
    }

    /// Atomically consume one finite disposable use (`mail-aliases.md`
    /// § Storage `:196` — "decremented atomically at RCPT TO"). The single
    /// guarded `UPDATE ... WHERE uses_remaining > 0` is the double-spend guard:
    /// two concurrent RCPTs racing the last use see exactly one
    /// [`ConsumeDisposableOutcome::Decremented`]; the loser sees
    /// [`ConsumeDisposableOutcome::Exhausted`] (rejected `550 Address
    /// expired`). An unlimited disposable (`uses_remaining IS NULL`) matches
    /// the guard's 0 rows but is alive — distinguished as
    /// [`ConsumeDisposableOutcome::Unlimited`] (no decrement owed).
    pub async fn consume_disposable_use(
        &self,
        alias_id: &[u8; 16],
    ) -> Result<ConsumeDisposableOutcome> {
        let id = *alias_id;
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE account_aliases SET uses_remaining = uses_remaining - 1
                 WHERE alias_id = ?1 AND kind = 'disposable' AND uses_remaining > 0",
                rusqlite::params![&id[..]],
            )
            .context("decrement disposable uses_remaining")?;
        if changed > 0 {
            return Ok(ConsumeDisposableOutcome::Decremented);
        }
        // 0 rows: NULL (unlimited, alive) vs 0 (exhausted) vs gone.
        let remaining: Option<Option<i64>> = conn
            .query_row(
                "SELECT uses_remaining FROM account_aliases
                 WHERE alias_id = ?1 AND kind = 'disposable'",
                rusqlite::params![&id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("read disposable uses_remaining after no-op decrement")?;
        Ok(match remaining {
            None => ConsumeDisposableOutcome::NotFound,
            Some(None) => ConsumeDisposableOutcome::Unlimited,
            Some(Some(_)) => ConsumeDisposableOutcome::Exhausted,
        })
    }

    /// Record one resolve-time hit against a stored alias row (A2.4,
    /// `mail-aliases.md` § Storage `:174,180-186,196`): INSERT an `alias_hits`
    /// audit row + bump the alias's `last_hit_at`/`hit_count`. Called by the
    /// resolver handler for exact / +suffix-base / wildcard / disposable
    /// matches (catch-all has no row → no hit). Both statements run under the
    /// one conn lock (the global mutex serializes; `hit_count + 1` is atomic),
    /// so a concurrent resolve can't lose a count. `received_at` is the
    /// handler's clock. Mirrors the `bridge_audit` append + the
    /// `consume_disposable_use` "matcher signals, handler writes" split.
    pub async fn log_alias_hit(
        &self,
        alias_id: &[u8; 16],
        matched_address: &str,
        sender_domain: &str,
        received_at: i64,
    ) -> Result<()> {
        let id = *alias_id;
        let hit_id = *Uuid::new_v4().as_bytes();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO alias_hits (hit_id, alias_id, matched_address, sender_domain, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                &hit_id[..],
                &id[..],
                matched_address,
                sender_domain,
                received_at
            ],
        )
        .context("insert alias hit")?;
        conn.execute(
            "UPDATE account_aliases SET last_hit_at = ?1, hit_count = hit_count + 1
             WHERE alias_id = ?2",
            rusqlite::params![received_at, &id[..]],
        )
        .context("bump alias last_hit_at/hit_count")?;
        Ok(())
    }

    /// Count this alias's `alias_hits` rows over the two trailing rate-cap
    /// windows (`mail-aliases.md` § Per-alias rate-cap), returning
    /// `(hour_hits, day_hits)`. One indexed range scan over
    /// `idx_alias_hits_alias_time(alias_id, received_at DESC)` serves both:
    /// the day window is the wider of the two, so the hourly count is a
    /// conditional sum inside it. Called at RCPT time by the resolver, and
    /// **only** for an alias that actually carries a cap
    /// (`ResolvedControls::has_rate_cap`) — an uncapped alias pays nothing.
    ///
    /// Counts accepted deliveries only, because that is what `alias_hits`
    /// records: the gate runs ahead of [`Self::log_alias_hit`], so a
    /// tempfailed message leaves no row and a retrying sender does not push
    /// their own window forward.
    ///
    /// Read and hit-write take the conn lock separately, so N recipients
    /// resolving concurrently against a nearly-full window can overshoot the
    /// cap by up to N-1. Deliberate: this is an abuse brake, not a ledger, and
    /// a read-modify-write transaction per RCPT would serialize the resolver's
    /// hot path to buy an exactness the cap's own semantics never promised.
    pub async fn count_alias_hits_in_windows(
        &self,
        alias_id: &[u8; 16],
        hour_since_ms: i64,
        day_since_ms: i64,
    ) -> Result<(i64, i64)> {
        let id = *alias_id;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT COALESCE(SUM(received_at >= ?2), 0), COUNT(*)
                   FROM alias_hits
                  WHERE alias_id = ?1 AND received_at >= ?3",
                rusqlite::params![&id[..], hour_since_ms, day_since_ms],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .context("count alias hits in rate-cap windows")?;
        Ok(row)
    }

    /// Newest-first page of `alias_hits` for an alias the caller **owns**
    /// (A2.4, `mail-aliases.md` § Per-alias-hit audit list `:245`, § Wire
    /// `:301`). Ownership is enforced by joining `alias_hits.alias_id →
    /// account_aliases.actor_id == actor_id`; a non-owner (or unknown
    /// `alias_id`) yields `Ok(None)` (the handler maps it to `not_found`,
    /// never leaking another user's hit existence). `Ok(Some(rows))` for an
    /// owned alias, possibly empty.
    ///
    /// `before_hit_id` is the keyset cursor — the previous page's last
    /// `hit_id`. `hit_id` is a random UUID (not time-sortable), so we resolve
    /// it to its `received_at` and page on the composite `(received_at,
    /// hit_id)` descending; a cursor whose row has aged out of retention
    /// resolves to nothing → empty page (pagination ends gracefully).
    pub async fn list_alias_hits_for_owner(
        &self,
        actor_id: &[u8; 32],
        alias_id: &[u8; 16],
        limit: i64,
        before_hit_id: Option<&[u8; 16]>,
    ) -> Result<Option<Vec<AliasHitRecord>>> {
        let actor = *actor_id;
        let id = *alias_id;
        let conn = self.conn.lock().await;

        // Ownership gate: the alias must exist AND belong to the caller.
        let owned: bool = conn
            .query_row(
                "SELECT 1 FROM account_aliases WHERE alias_id = ?1 AND actor_id = ?2",
                rusqlite::params![&id[..], &actor[..]],
                |_| Ok(()),
            )
            .optional()
            .context("alias ownership check")?
            .is_some();
        if !owned {
            return Ok(None);
        }

        // Resolve the keyset cursor to its (received_at, hit_id). A missing
        // cursor row (GC'd, or not this alias's) → empty page.
        let cursor: Option<(i64, Vec<u8>)> = match before_hit_id {
            Some(hid) => {
                let found: Option<i64> = conn
                    .query_row(
                        "SELECT received_at FROM alias_hits WHERE hit_id = ?1 AND alias_id = ?2",
                        rusqlite::params![&hid[..], &id[..]],
                        |row| row.get(0),
                    )
                    .optional()
                    .context("resolve alias-hit cursor")?;
                match found {
                    Some(rec) => Some((rec, hid.to_vec())),
                    None => return Ok(Some(Vec::new())),
                }
            }
            None => None,
        };

        let rows = match &cursor {
            None => {
                let mut stmt = conn
                    .prepare(
                        "SELECT hit_id, matched_address, sender_domain, received_at
                           FROM alias_hits
                          WHERE alias_id = ?1
                          ORDER BY received_at DESC, hit_id DESC
                          LIMIT ?2",
                    )
                    .context("prepare list alias hits")?;
                stmt.query_map(rusqlite::params![&id[..], limit], row_to_alias_hit)
                    .context("query list alias hits")?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .context("collect list alias hits")?
            }
            Some((cur_rec, cur_hit)) => {
                let mut stmt = conn
                    .prepare(
                        "SELECT hit_id, matched_address, sender_domain, received_at
                           FROM alias_hits
                          WHERE alias_id = ?1
                            AND (received_at < ?2
                                 OR (received_at = ?2 AND hit_id < ?3))
                          ORDER BY received_at DESC, hit_id DESC
                          LIMIT ?4",
                    )
                    .context("prepare list alias hits (cursor)")?;
                stmt.query_map(
                    rusqlite::params![&id[..], cur_rec, &cur_hit[..], limit],
                    row_to_alias_hit,
                )
                .context("query list alias hits (cursor)")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect list alias hits (cursor)")?
            }
        };
        Ok(Some(rows))
    }

    /// Delete `alias_hits` rows older than `cutoff_received_at_ms`. Returns the
    /// number deleted. Backs the 30-day retention sweep (A2.4,
    /// `mail-aliases.md` § Per-alias-hit audit list `:247`), mirroring
    /// `bridge_audit::prune_bridge_audit_events_older_than`.
    pub async fn prune_alias_hits_older_than(&self, cutoff_received_at_ms: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM alias_hits WHERE received_at < ?1",
                rusqlite::params![cutoff_received_at_ms],
            )
            .context("prune alias hits")?;
        Ok(n)
    }
}

/// Outcome of [`CacheDb::consume_disposable_use`] — the atomic
/// `uses_remaining` decrement at RCPT TO time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeDisposableOutcome {
    /// A finite use was consumed (the row had `uses_remaining > 0`).
    Decremented,
    /// `uses_remaining IS NULL` — unlimited; alive, nothing decremented.
    Unlimited,
    /// `uses_remaining = 0` — the last use was already taken (lost the race);
    /// reject `550 Address expired`.
    Exhausted,
    /// The row vanished between resolve and consume (deleted concurrently).
    NotFound,
}

/// The `account_aliases` column list, in [`row_to_alias_record`]'s expected
/// order. Shared by every full-row `SELECT` so the column order can't drift
/// between the readers.
const ALIAS_SELECT_COLS: &str = "alias_id, actor_id, local_domain, kind, pattern, label, \
     disabled, spam_threshold_override, rate_limit_per_hour, rate_limit_per_day, \
     uses_remaining, expires_at, created_at, last_hit_at, hit_count, forward_target";

/// Map a rusqlite write error to [`AliasWriteError`], folding a UNIQUE /
/// constraint violation to `Conflict` and everything else to `Db`.
/// The kinds that share the resolver's exact-key tier — exact, forwarder,
/// list (`mail-aliases.md` § Resolution order). One `(local_domain, pattern)`
/// has at most one holder across them.
const EXACT_KEY_TIER_KINDS: [&str; 3] = [ALIAS_KIND_EXACT, ALIAS_KIND_FORWARDER, ALIAS_KIND_LIST];

/// Whether a write of `kind` on `(local_domain, pattern)` (both already
/// lower-cased) would give the exact-key tier a second holder: `kind` is a
/// tier kind and a row of ANOTHER tier kind is on the key. The
/// `UNIQUE (local_domain, pattern, kind)` index covers the same-kind case
/// but is blind across kinds, and a second holder is a routing capture — an
/// `exact` row shadows the list step and the forwarder behind it, because
/// the resolver matches exact first.
///
/// Every `account_aliases` writer of a tier kind calls this under the same
/// connection lock (or transaction) as its write, so the check and the write
/// are one step and no door — single create, bulk import, rename,
/// forwarder create, list create — can skip it. A non-tier `kind` is never
/// refused here.
pub(crate) fn exact_key_held_by_other_kind(
    conn: &rusqlite::Connection,
    local_domain: &str,
    pattern: &str,
    kind: &str,
) -> Result<bool> {
    if !EXACT_KEY_TIER_KINDS.contains(&kind) {
        return Ok(false);
    }
    let held = conn
        .query_row(
            &format!(
                "SELECT 1 FROM account_aliases
                 WHERE local_domain = ?1 AND pattern = ?2 AND kind != ?3
                   AND kind IN ('{ALIAS_KIND_EXACT}', '{ALIAS_KIND_FORWARDER}', '{ALIAS_KIND_LIST}')
                 LIMIT 1"
            ),
            rusqlite::params![local_domain, pattern, kind],
            |_| Ok(()),
        )
        .optional()
        .context("exact-key tier holder lookup")?;
    Ok(held.is_some())
}

fn map_alias_write_err(e: rusqlite::Error) -> AliasWriteError {
    if let rusqlite::Error::SqliteFailure(f, _) = &e
        && f.code == rusqlite::ErrorCode::ConstraintViolation
    {
        return AliasWriteError::Conflict;
    }
    AliasWriteError::Db(anyhow::Error::new(e))
}

/// `account_aliases` row → [`AliasRecord`]. Column order must match the
/// `SELECT` in [`CacheDb::list_aliases_for_actor`].
fn row_to_alias_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AliasRecord> {
    let alias_id_v: Vec<u8> = row.get(0)?;
    let actor_v: Vec<u8> = row.get(1)?;
    let alias_id: [u8; 16] = alias_id_v.as_slice().try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            alias_id_v.len(),
            rusqlite::types::Type::Blob,
            "account_aliases.alias_id not 16 bytes".into(),
        )
    })?;
    let actor_id: [u8; 32] = actor_v.as_slice().try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            actor_v.len(),
            rusqlite::types::Type::Blob,
            "account_aliases.actor_id not 32 bytes".into(),
        )
    })?;
    let disabled: i64 = row.get(6)?;
    let spam: Option<f64> = row.get(7)?;
    Ok(AliasRecord {
        alias_id,
        actor_id,
        local_domain: row.get(2)?,
        kind: row.get(3)?,
        pattern: row.get(4)?,
        label: row.get(5)?,
        disabled: disabled != 0,
        // Stored as REAL but only ever an integral spam-point count this
        // slice; round defensively before narrowing.
        spam_threshold_override: spam.map(|v| v.round() as u32),
        rate_limit_per_hour: row.get(8)?,
        rate_limit_per_day: row.get(9)?,
        uses_remaining: row.get(10)?,
        expires_at: row.get(11)?,
        created_at: row.get(12)?,
        last_hit_at: row.get(13)?,
        hit_count: row.get(14)?,
        // Appended after the original 15 cols (see `ALIAS_SELECT_COLS`);
        // forwarder-only, NULL for every other kind.
        forward_target: row.get(15)?,
    })
}

/// `alias_hits` row → [`AliasHitRecord`]. Column order must match the
/// `SELECT`s in [`CacheDb::list_alias_hits_for_owner`].
fn row_to_alias_hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<AliasHitRecord> {
    let hit_id_v: Vec<u8> = row.get(0)?;
    let hit_id: [u8; 16] = hit_id_v.as_slice().try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            hit_id_v.len(),
            rusqlite::types::Type::Blob,
            "alias_hits.hit_id not 16 bytes".into(),
        )
    })?;
    Ok(AliasHitRecord {
        hit_id,
        matched_address: row.get(1)?,
        sender_domain: row.get(2)?,
        received_at: row.get(3)?,
    })
}

/// Default retention for `alias_hits` audit rows, derived from the shared
/// [`fauna_mail::aliases::ALIAS_HITS_RETENTION_DAYS`] (30 d). The nest spawns
/// [`spawn_alias_hits_retention_sweeper`] with this.
pub const DEFAULT_ALIAS_HITS_RETENTION: std::time::Duration = std::time::Duration::from_secs(
    fauna_mail::aliases::ALIAS_HITS_RETENTION_DAYS as u64 * 24 * 60 * 60,
);

/// Spawns a tokio task that periodically deletes `alias_hits` rows older than
/// `retention`. Cadence is 1/24 of the retention (a 30-day window sweeps every
/// ~1.25 days), mirroring `bridge_audit::spawn_audit_retention_sweeper` —
/// rare enough to keep the DB lock held briefly.
pub fn spawn_alias_hits_retention_sweeper(
    db: std::sync::Arc<CacheDb>,
    retention: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    super::spawn_retention_sweeper(retention, move || {
        let db = db.clone();
        async move {
            let cutoff_ms = now_epoch_millis().saturating_sub(retention.as_millis() as i64);
            match db.prune_alias_hits_older_than(cutoff_ms).await {
                Ok(n) if n > 0 => tracing::info!(
                    target: "mail_aliases",
                    pruned = n,
                    cutoff_ms,
                    "pruned alias hits"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    target: "mail_aliases",
                    error = %e,
                    "alias-hits retention sweep failed"
                ),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_then_lookup_round_trip() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [4u8; 32];
        db.put_exact_alias("example.com", "alice", ALIAS_KIND_EXACT, &actor)
            .await
            .unwrap();
        assert_eq!(
            db.lookup_exact_alias("example.com", "alice").await.unwrap(),
            Some(actor),
        );
    }

    #[tokio::test]
    async fn lookup_miss_returns_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.lookup_exact_alias("example.com", "ghost")
                .await
                .unwrap()
                .is_none()
        );
    }

    // ── Admin external forwarders (§ Kind 7 / § AF) ─────────────────

    #[tokio::test]
    async fn create_forwarder_round_trips_and_excluded_from_user_list() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin = [1u8; 32];
        // The admin also owns a personal exact alias on the same domain.
        let c = AliasControlsInput::default();
        db.create_account_alias(&admin, "fauna.example", ALIAS_KIND_EXACT, "boss", &c)
            .await
            .unwrap();

        let fwd_id = db
            .create_forwarder_alias(&admin, "Fauna.Example", "Info", "oldaccount@example.net")
            .await
            .unwrap();

        // Lookup on the exact key carries the forward_target + the admin actor.
        let rec = db
            .lookup_forwarder_alias_record("fauna.example", "info")
            .await
            .unwrap()
            .expect("forwarder present");
        assert_eq!(rec.alias_id, fwd_id);
        assert_eq!(rec.actor_id, admin);
        assert_eq!(rec.kind, ALIAS_KIND_FORWARDER);
        assert_eq!(
            rec.forward_target.as_deref(),
            Some("oldaccount@example.net")
        );

        // list_forwarders sees it; list_aliases_for_actor (the user surface)
        // does NOT — the admin's forwarder never shows as a personal alias.
        let forwarders = db.list_forwarders().await.unwrap();
        assert_eq!(forwarders.len(), 1);
        assert_eq!(forwarders[0].alias_id, fwd_id);
        let user_rows = db.list_aliases_for_actor(&admin).await.unwrap();
        assert!(
            user_rows.iter().all(|r| r.kind != ALIAS_KIND_FORWARDER),
            "forwarder must not appear in the user alias list"
        );
        assert_eq!(user_rows.len(), 1, "only the personal exact alias");

        // Delete removes it; idempotent miss returns false.
        assert!(db.delete_forwarder(&fwd_id).await.unwrap());
        assert!(!db.delete_forwarder(&fwd_id).await.unwrap());
        assert!(db.list_forwarders().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn duplicate_forwarder_on_same_key_is_conflict() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin = [1u8; 32];
        db.create_forwarder_alias(&admin, "fauna.example", "info", "a@example.net")
            .await
            .unwrap();
        let err = db
            .create_forwarder_alias(&admin, "fauna.example", "info", "b@example.net")
            .await
            .unwrap_err();
        assert!(matches!(err, AliasWriteError::Conflict), "got {err:?}");
    }

    #[tokio::test]
    async fn delete_forwarder_will_not_delete_a_user_alias_by_id() {
        // The kind='forwarder' guard means the admin delete path can't remove
        // a user's personal exact alias even given its id.
        let db = CacheDb::open_in_memory().unwrap();
        let user = [7u8; 32];
        let c = AliasControlsInput::default();
        let exact_id = db
            .create_account_alias(&user, "fauna.example", ALIAS_KIND_EXACT, "bob", &c)
            .await
            .unwrap();
        assert!(!db.delete_forwarder(&exact_id).await.unwrap());
        // The exact alias is still there.
        assert_eq!(db.list_aliases_for_actor(&user).await.unwrap().len(), 1);
    }

    // ── User-tier CRUD over account_aliases (A2.1) ──────────────────

    #[tokio::test]
    async fn create_account_alias_then_list_round_trips_controls() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [5u8; 32];
        let controls = AliasControlsInput {
            label: "work".into(),
            // Stored in a REAL column; must read back as the same integral
            // spam-point count (scope decision 3).
            spam_threshold_override: Some(8),
            rate_limit_per_hour: Some(100),
            rate_limit_per_day: None,
        };
        let alias_id = db
            .create_account_alias(
                &actor,
                "Example.COM",
                ALIAS_KIND_EXACT,
                "Bob.Smith",
                &controls,
            )
            .await
            .unwrap();

        let rows = db.list_aliases_for_actor(&actor).await.unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.alias_id, alias_id);
        assert_eq!(r.actor_id, actor);
        // local_domain + pattern lower-cased on write.
        assert_eq!(r.local_domain, "example.com");
        assert_eq!(r.pattern, "bob.smith");
        assert_eq!(r.kind, "exact");
        assert_eq!(r.label, "work");
        assert!(!r.disabled);
        assert_eq!(r.spam_threshold_override, Some(8));
        assert_eq!(r.rate_limit_per_hour, Some(100));
        assert_eq!(r.rate_limit_per_day, None);
        assert_eq!(r.uses_remaining, None);
        assert_eq!(r.expires_at, None);
        assert_eq!(r.hit_count, 0);
        assert_eq!(db.count_exact_aliases_for_actor(&actor).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn create_account_alias_duplicate_is_conflict() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = [5u8; 32];
        let b = [6u8; 32];
        let c = AliasControlsInput::default();
        db.create_account_alias(&a, "example.com", ALIAS_KIND_EXACT, "shared", &c)
            .await
            .unwrap();
        // Same actor and a *different* actor both conflict on
        // (local_domain, pattern, kind).
        for actor in [a, b] {
            let err = db
                .create_account_alias(&actor, "example.com", ALIAS_KIND_EXACT, "shared", &c)
                .await
                .unwrap_err();
            assert!(matches!(err, AliasWriteError::Conflict), "got {err:?}");
        }
    }

    #[tokio::test]
    async fn writes_are_owner_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        let alice = [5u8; 32];
        let bob = [6u8; 32];
        let c = AliasControlsInput::default();
        let id = db
            .create_account_alias(&alice, "example.com", ALIAS_KIND_EXACT, "alice", &c)
            .await
            .unwrap();
        // Bob can't update / disable / delete Alice's row → no rows affected.
        assert!(
            !db.update_alias_controls(&id, &bob, "hacked", &c)
                .await
                .unwrap()
        );
        assert!(!db.set_alias_disabled(&id, &bob, true).await.unwrap());
        assert!(!db.delete_account_alias(&id, &bob).await.unwrap());
        // Alice owns it: each succeeds.
        assert!(db.set_alias_disabled(&id, &alice, true).await.unwrap());
        assert!(db.delete_account_alias(&id, &alice).await.unwrap());
        assert!(db.list_aliases_for_actor(&alice).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn put_is_case_insensitive_on_localpart_and_domain() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        db.put_exact_alias("Example.COM", "Alice", ALIAS_KIND_EXACT, &actor)
            .await
            .unwrap();
        // Lookup with mixed case — the table is lower-cased on both
        // axes (matches the resolver-order spec: § Resolution order
        // step 1 is "LOWER(rcpt_to)" / "LOWER(rcpt_domain)").
        assert_eq!(
            db.lookup_exact_alias("example.com", "alice").await.unwrap(),
            Some(actor),
        );
        assert_eq!(
            db.lookup_exact_alias("EXAMPLE.COM", "ALICE").await.unwrap(),
            Some(actor),
        );
    }

    #[tokio::test]
    async fn put_exact_alias_is_upsert_on_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = [1u8; 32];
        let b = [2u8; 32];
        db.put_exact_alias("example.com", "alice", ALIAS_KIND_EXACT, &a)
            .await
            .unwrap();
        db.put_exact_alias("example.com", "alice", ALIAS_KIND_EXACT, &b)
            .await
            .unwrap();
        assert_eq!(
            db.lookup_exact_alias("example.com", "alice").await.unwrap(),
            Some(b),
        );
    }

    #[tokio::test]
    async fn put_rejects_unimplemented_kinds() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [4u8; 32];
        for kind in [
            "wildcard_prefix",
            "disposable",
            "catchall",
            "subaddress",
            "garbage",
        ] {
            let err = db
                .put_exact_alias("example.com", "alice", kind, &actor)
                .await
                .unwrap_err();
            let msg = format!("{err}");
            assert!(
                msg.contains("only kind=exact is implemented yet"),
                "expected explicit unimplemented-kind error, got: {msg}",
            );
        }
        // None of the rejected attempts wrote a row.
        assert!(
            db.lookup_exact_alias("example.com", "alice")
                .await
                .unwrap()
                .is_none()
        );
    }

    // ── A2.2 resolver reads + wildcard conflict checks ──────────────

    #[tokio::test]
    async fn lookup_exact_alias_record_carries_controls_and_disabled() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];
        let controls = AliasControlsInput {
            label: "x".into(),
            spam_threshold_override: Some(9),
            rate_limit_per_hour: Some(50),
            rate_limit_per_day: None,
        };
        let id = db
            .create_account_alias(&actor, "example.com", ALIAS_KIND_EXACT, "bob", &controls)
            .await
            .unwrap();
        let rec = db
            .lookup_exact_alias_record("EXAMPLE.com", "Bob")
            .await
            .unwrap()
            .expect("found");
        assert_eq!(rec.actor_id, actor);
        assert!(!rec.disabled);
        assert_eq!(rec.spam_threshold_override, Some(9));
        assert_eq!(rec.rate_limit_per_hour, Some(50));
        // Revoke flips disabled — the resolver record reflects it.
        db.set_alias_disabled(&id, &actor, true).await.unwrap();
        let rec = db
            .lookup_exact_alias_record("example.com", "bob")
            .await
            .unwrap()
            .expect("found");
        assert!(rec.disabled);
        // Miss is None.
        assert!(
            db.lookup_exact_alias_record("example.com", "ghost")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn list_wildcard_aliases_for_domain_scopes_by_kind_and_domain() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = AliasControlsInput::default();
        db.create_account_alias(&a, "example.com", ALIAS_KIND_WILDCARD_PREFIX, "bob-", &c)
            .await
            .unwrap();
        db.create_account_alias(&b, "example.com", ALIAS_KIND_WILDCARD_PREFIX, "alice-", &c)
            .await
            .unwrap();
        // An exact alias + a wildcard on another domain must not leak in.
        db.create_account_alias(&a, "example.com", ALIAS_KIND_EXACT, "bob", &c)
            .await
            .unwrap();
        db.create_account_alias(&a, "other.test", ALIAS_KIND_WILDCARD_PREFIX, "bob-", &c)
            .await
            .unwrap();
        let rows = db
            .list_wildcard_aliases_for_domain("example.com")
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.kind == ALIAS_KIND_WILDCARD_PREFIX));
        assert!(rows.iter().all(|r| r.local_domain == "example.com"));
    }

    #[tokio::test]
    async fn exact_alias_exists_under_prefix_detects_shadow() {
        let db = CacheDb::open_in_memory().unwrap();
        let other = [3u8; 32];
        let c = AliasControlsInput::default();
        db.create_account_alias(&other, "example.com", ALIAS_KIND_EXACT, "bob-foo", &c)
            .await
            .unwrap();
        // `bob-*` would match the existing exact `bob-foo`.
        assert!(
            db.exact_alias_exists_under_prefix("example.com", "bob-")
                .await
                .unwrap()
        );
        // `alice-*` matches nothing.
        assert!(
            !db.exact_alias_exists_under_prefix("example.com", "alice-")
                .await
                .unwrap()
        );
        // Different domain → no shadow.
        assert!(
            !db.exact_alias_exists_under_prefix("other.test", "bob-")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn actor_has_wildcard_is_per_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = AliasControlsInput::default();
        assert!(!db.actor_has_wildcard(&a).await.unwrap());
        db.create_account_alias(&a, "example.com", ALIAS_KIND_WILDCARD_PREFIX, "bob-", &c)
            .await
            .unwrap();
        assert!(db.actor_has_wildcard(&a).await.unwrap());
        // An exact alias does not count as a wildcard.
        db.create_account_alias(&b, "example.com", ALIAS_KIND_EXACT, "bob", &c)
            .await
            .unwrap();
        assert!(!db.actor_has_wildcard(&b).await.unwrap());
    }

    // ── A2.3 disposable mint + resolution ───────────────────────────

    #[tokio::test]
    async fn oldest_exact_alias_is_the_canonical_handle() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];
        let c = AliasControlsInput::default();
        // No exact alias → no canonical address.
        assert!(
            db.oldest_exact_alias_for_actor(&actor)
                .await
                .unwrap()
                .is_none()
        );
        // First (oldest) exact alias is the canonical handle, even after a
        // second is added.
        db.create_account_alias(&actor, "Example.COM", ALIAS_KIND_EXACT, "Bob", &c)
            .await
            .unwrap();
        db.create_account_alias(&actor, "example.com", ALIAS_KIND_EXACT, "bob.smith", &c)
            .await
            .unwrap();
        let (domain, handle) = db
            .oldest_exact_alias_for_actor(&actor)
            .await
            .unwrap()
            .expect("canonical");
        assert_eq!(domain, "example.com"); // lower-cased on write
        assert_eq!(handle, "bob");
    }

    #[tokio::test]
    async fn create_disposable_then_lookup_by_token() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [4u8; 32];
        let expires = now_epoch_millis() + 86_400_000;
        let id = db
            .create_disposable_alias(
                &actor,
                "Example.COM",
                "A2B3C4",
                "amazon",
                Some(1),
                expires,
                100,
            )
            .await
            .unwrap();
        // Lookup is case-insensitive on domain + token (both lower-cased).
        let rec = db
            .lookup_disposable_alias_record("example.com", "a2b3c4")
            .await
            .unwrap()
            .expect("found");
        assert_eq!(rec.alias_id, id);
        assert_eq!(rec.actor_id, actor);
        assert_eq!(rec.kind, "disposable");
        assert_eq!(rec.pattern, "a2b3c4");
        assert_eq!(rec.label, "amazon");
        assert_eq!(rec.uses_remaining, Some(1));
        assert_eq!(rec.expires_at, Some(expires));
        assert_eq!(rec.rate_limit_per_day, Some(100));
        // Mixed-case token lookup still hits (stored lower-cased).
        assert!(
            db.lookup_disposable_alias_record("EXAMPLE.com", "A2B3C4")
                .await
                .unwrap()
                .is_some()
        );
        // A miss is None.
        assert!(
            db.lookup_disposable_alias_record("example.com", "zzzzz7")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn create_disposable_duplicate_token_is_conflict() {
        let db = CacheDb::open_in_memory().unwrap();
        let expires = now_epoch_millis() + 86_400_000;
        db.create_disposable_alias(
            &[1u8; 32],
            "example.com",
            "a2b3c4",
            "",
            Some(1),
            expires,
            100,
        )
        .await
        .unwrap();
        // Same (local_domain, token) — even a different actor — collides.
        let err = db
            .create_disposable_alias(
                &[2u8; 32],
                "example.com",
                "a2b3c4",
                "",
                Some(1),
                expires,
                100,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AliasWriteError::Conflict), "got {err:?}");
    }

    #[tokio::test]
    async fn consume_disposable_use_finite_then_exhausted() {
        let db = CacheDb::open_in_memory().unwrap();
        let expires = now_epoch_millis() + 86_400_000;
        let id = db
            .create_disposable_alias(
                &[3u8; 32],
                "example.com",
                "a2b3c4",
                "",
                Some(1),
                expires,
                100,
            )
            .await
            .unwrap();
        // First consume succeeds (1 → 0).
        assert_eq!(
            db.consume_disposable_use(&id).await.unwrap(),
            ConsumeDisposableOutcome::Decremented
        );
        let rec = db
            .lookup_disposable_alias_record("example.com", "a2b3c4")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.uses_remaining, Some(0));
        // Second consume: exhausted (the double-spend guard).
        assert_eq!(
            db.consume_disposable_use(&id).await.unwrap(),
            ConsumeDisposableOutcome::Exhausted
        );
        // uses_remaining never goes negative.
        let rec = db
            .lookup_disposable_alias_record("example.com", "a2b3c4")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.uses_remaining, Some(0));
    }

    #[tokio::test]
    async fn consume_disposable_use_unlimited_and_not_found() {
        let db = CacheDb::open_in_memory().unwrap();
        let expires = now_epoch_millis() + 86_400_000;
        // uses = unlimited (NULL): consume is a no-op-but-alive.
        let id = db
            .create_disposable_alias(&[3u8; 32], "example.com", "a2b3c4", "", None, expires, 100)
            .await
            .unwrap();
        assert_eq!(
            db.consume_disposable_use(&id).await.unwrap(),
            ConsumeDisposableOutcome::Unlimited
        );
        // Still unlimited after the call.
        let rec = db
            .lookup_disposable_alias_record("example.com", "a2b3c4")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.uses_remaining, None);
        // A vanished row → NotFound.
        assert_eq!(
            db.consume_disposable_use(&[9u8; 16]).await.unwrap(),
            ConsumeDisposableOutcome::NotFound
        );
    }

    #[tokio::test]
    async fn count_recent_disposable_mints_is_windowed_and_per_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = [3u8; 32];
        let b = [4u8; 32];
        let expires = now_epoch_millis() + 86_400_000;
        for token in ["a2b3c4", "d5e6f7"] {
            db.create_disposable_alias(&a, "example.com", token, "", Some(1), expires, 100)
                .await
                .unwrap();
        }
        db.create_disposable_alias(&b, "example.com", "zzzzz7", "", Some(1), expires, 100)
            .await
            .unwrap();
        // A's window since epoch sees both of A's mints, not B's.
        assert_eq!(db.count_recent_disposable_mints(&a, 0).await.unwrap(), 2);
        assert_eq!(db.count_recent_disposable_mints(&b, 0).await.unwrap(), 1);
        // A future window excludes everything.
        let future = now_epoch_millis() + 1_000_000;
        assert_eq!(
            db.count_recent_disposable_mints(&a, future).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn aliases_are_per_local_domain() {
        let db = CacheDb::open_in_memory().unwrap();
        let alice_on_dom1 = [10u8; 32];
        let bob_on_dom2 = [20u8; 32];
        // Same local-part on two different domains routes to two
        // different actors (per § Cross-user uniqueness — uniqueness is
        // per local_domain, not deployment-wide).
        db.put_exact_alias("domain1.test", "alice", ALIAS_KIND_EXACT, &alice_on_dom1)
            .await
            .unwrap();
        db.put_exact_alias("domain2.test", "alice", ALIAS_KIND_EXACT, &bob_on_dom2)
            .await
            .unwrap();
        assert_eq!(
            db.lookup_exact_alias("domain1.test", "alice")
                .await
                .unwrap(),
            Some(alice_on_dom1),
        );
        assert_eq!(
            db.lookup_exact_alias("domain2.test", "alice")
                .await
                .unwrap(),
            Some(bob_on_dom2),
        );
    }

    // ── alias_hits logging + audit list + retention (A2.4) ──────────

    /// Seed one exact alias and return its id (the owner of any hits we log).
    async fn seed_alias(db: &CacheDb, actor: &[u8; 32]) -> [u8; 16] {
        db.create_account_alias(
            actor,
            "example.com",
            ALIAS_KIND_EXACT,
            "bob",
            &AliasControlsInput::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn log_alias_hit_inserts_row_and_bumps_counters() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [5u8; 32];
        let alias_id = seed_alias(&db, &actor).await;

        db.log_alias_hit(&alias_id, "bob@example.com", "amazon.com", 1_700_000_000)
            .await
            .unwrap();
        db.log_alias_hit(&alias_id, "bob@example.com", "spammer.net", 1_700_000_500)
            .await
            .unwrap();

        // The alias row's counters reflect both hits.
        let rec = db
            .list_aliases_for_actor(&actor)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.alias_id == alias_id)
            .unwrap();
        assert_eq!(rec.hit_count, 2);
        assert_eq!(rec.last_hit_at, Some(1_700_000_500));

        // Both audit rows are listable, newest-first, with the sender domains.
        let hits = db
            .list_alias_hits_for_owner(&actor, &alias_id, 100, None)
            .await
            .unwrap()
            .expect("owned");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].sender_domain, "spammer.net");
        assert_eq!(hits[0].received_at, 1_700_000_500);
        assert_eq!(hits[1].sender_domain, "amazon.com");
    }

    #[tokio::test]
    async fn count_alias_hits_in_windows_splits_the_hour_out_of_the_day() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [5u8; 32];
        let alias_id = seed_alias(&db, &actor).await;
        let other_alias = db
            .create_account_alias(
                &actor,
                "example.com",
                ALIAS_KIND_EXACT,
                "carol",
                &AliasControlsInput::default(),
            )
            .await
            .unwrap();

        let now = 1_700_000_000_000i64;
        let hour = fauna_mail::aliases::RATE_CAP_HOUR_WINDOW_MS;
        let day = fauna_mail::aliases::RATE_CAP_DAY_WINDOW_MS;
        // Two hits inside the hour, three more inside the day but older than
        // an hour, one older than a day, and one on a DIFFERENT alias.
        for age in [0, hour / 2, hour + 1, hour * 5, day - 1, day + 1] {
            db.log_alias_hit(&alias_id, "bob@example.com", "s.net", now - age)
                .await
                .unwrap();
        }
        db.log_alias_hit(&other_alias, "carol@example.com", "s.net", now)
            .await
            .unwrap();

        let (hour_hits, day_hits) = db
            .count_alias_hits_in_windows(&alias_id, now - hour, now - day)
            .await
            .unwrap();
        // Ages 0 and hour/2 are inside the hour; `hour + 1` is not — the
        // window edge is inclusive on `received_at >= since`.
        assert_eq!(hour_hits, 2, "only the sub-hour hits count hourly");
        // Everything but the `day + 1` hit — and never the sibling alias's.
        assert_eq!(day_hits, 5, "the day window excludes only the >24h hit");
    }

    #[tokio::test]
    async fn count_alias_hits_in_windows_is_zero_for_a_fresh_alias() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [5u8; 32];
        let alias_id = seed_alias(&db, &actor).await;
        // COALESCE over the empty SUM — a fresh alias must read (0, 0), not
        // a NULL that would fail the i64 column decode.
        let counts = db
            .count_alias_hits_in_windows(&alias_id, 0, 0)
            .await
            .unwrap();
        assert_eq!(counts, (0, 0));
    }

    #[tokio::test]
    async fn list_alias_hits_rejects_non_owner() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [5u8; 32];
        let other = [6u8; 32];
        let alias_id = seed_alias(&db, &owner).await;
        db.log_alias_hit(&alias_id, "bob@example.com", "amazon.com", 1_700_000_000)
            .await
            .unwrap();

        // A different actor sees `None` (→ not_found), never the hits.
        assert!(
            db.list_alias_hits_for_owner(&other, &alias_id, 100, None)
                .await
                .unwrap()
                .is_none()
        );
        // An unknown alias is also `None`.
        assert!(
            db.list_alias_hits_for_owner(&owner, &[9u8; 16], 100, None)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn list_alias_hits_keyset_pages_oldest_after_cursor() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [5u8; 32];
        let alias_id = seed_alias(&db, &actor).await;
        for i in 0..5 {
            db.log_alias_hit(&alias_id, "bob@example.com", "s.net", 1_700_000_000 + i)
                .await
                .unwrap();
        }

        // First page of 2 (newest-first: received_at 4, 3).
        let page1 = db
            .list_alias_hits_for_owner(&actor, &alias_id, 2, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].received_at, 1_700_000_004);
        assert_eq!(page1[1].received_at, 1_700_000_003);

        // Next page from the last cursor (received_at 2, 1).
        let cursor = page1[1].hit_id;
        let page2 = db
            .list_alias_hits_for_owner(&actor, &alias_id, 2, Some(&cursor))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].received_at, 1_700_000_002);
        assert_eq!(page2[1].received_at, 1_700_000_001);

        // A cursor that doesn't exist (e.g. GC'd) → empty page, not the head.
        let empty = db
            .list_alias_hits_for_owner(&actor, &alias_id, 2, Some(&[0xAB; 16]))
            .await
            .unwrap()
            .unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn prune_alias_hits_drops_rows_older_than_cutoff() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [5u8; 32];
        let alias_id = seed_alias(&db, &actor).await;
        db.log_alias_hit(&alias_id, "bob@example.com", "old.net", 100)
            .await
            .unwrap();
        db.log_alias_hit(&alias_id, "bob@example.com", "new.net", 1_700_000_000)
            .await
            .unwrap();

        let pruned = db.prune_alias_hits_older_than(1_000_000).await.unwrap();
        assert_eq!(pruned, 1);
        let hits = db
            .list_alias_hits_for_owner(&actor, &alias_id, 100, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sender_domain, "new.net");
    }

    #[tokio::test]
    async fn delete_alias_cascades_its_hits() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [5u8; 32];
        let alias_id = seed_alias(&db, &actor).await;
        db.log_alias_hit(&alias_id, "bob@example.com", "amazon.com", 1_700_000_000)
            .await
            .unwrap();
        assert!(db.delete_account_alias(&alias_id, &actor).await.unwrap());
        // The FK `ON DELETE CASCADE` removed the hits; the alias is gone so the
        // owner check now yields `None`.
        assert!(
            db.list_alias_hits_for_owner(&actor, &alias_id, 100, None)
                .await
                .unwrap()
                .is_none()
        );
        // And no orphan rows remain.
        let conn = db.conn.lock().await;
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM alias_hits", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }
}
