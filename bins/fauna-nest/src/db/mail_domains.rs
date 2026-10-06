//! Multi-domain mail hosting: one row per current or recently-removed
//! local domain. The `mail.local_domains` flat list referenced by every
//! mail-side doc is the derived projection of `domain_name` where
//! `removed_at IS NULL`. Soft-delete + 30-day recovery window;
//! exactly one row has `is_primary = true` among active rows.
//!
//! Owns the table; companion modules add per-domain DKIM rotation,
//! DNS-record publishing, MTA-STS policy serving, the catch-all and
//! role-address resolvers, etc. (each its own follow-up track, tracked
//! internally).
//!
//! Authoritative spec: `docs/goal/behavior/mail-multidomain.md`
//! § The `mail_domains` model + § Architectural rules.

use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{CacheDb, now_epoch_millis};

/// 30 days in milliseconds. Soft-delete recovery window per
/// `mail-multidomain.md § Removing a local domain` and § Architectural
/// rules: a removed domain may be restored within this window; after
/// it expires a GC job destroys the row entirely.
const SOFT_DELETE_RECOVERY_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// 7 days in milliseconds: how long a domain stays in MTA-STS `testing` before
/// the nest advances its stored mode to `enforce`
/// (`mail-multidomain.md` § The advance). A constant, not a knob — no human
/// chooses the mode or the window.
pub const MTA_STS_TESTING_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// A door that activates a mail domain mints the DKIM key for its active
/// selector, on the connection that wrote the row (`mail-bridge-lifecycle.md`
/// § DKIM provisioning (automatic) → *Custody moves to the nest*): the DNS
/// record exists the moment the domain does. A selector that already has a
/// key (a restored domain's) is left as it is, and one a factory reset carried
/// a key for adopts that key. Logged, never fatal: the domain is added either way, and the boot
/// step seats any key this could not.
fn mint_dkim_key_for_activated_domain(
    conn: &rusqlite::Connection,
    domain: &str,
    selector: Option<&str>,
) {
    let selector = selector.unwrap_or(crate::mail_dkim_key::DEFAULT_SELECTOR);
    if let Err(e) = crate::mail_dkim_key::mint_selector(conn, domain, selector) {
        tracing::error!(
            target: "mail_dkim",
            domain = %domain,
            selector = %selector,
            "could not mint the domain's DKIM key; the next boot retries: {e:#}"
        );
    }
}

/// One row from `mail_domains`. The DKIM key is NOT carried on this
/// struct — it rests sealed in `mail_dkim_keys(domain, selector)` and no
/// RPC returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailDomain {
    #[serde(with = "serde_bytes")]
    pub domain_id: [u8; 16],
    pub domain_name: String,
    pub is_primary: bool,
    pub added_at: i64,
    pub removed_at: Option<i64>,
    pub restored_at: Option<i64>,
    pub dkim_selector: Option<String>,
    pub dkim_rotation_days: Option<i64>,
    /// JSON-encoded `Vec<String>` (SQLite has no native TEXT[]).
    /// Default `["ed25519", "rsa-2048"]`.
    pub dkim_algorithms_json: String,
    pub mta_sts_mode: String,
    pub mta_sts_max_age_seconds: i64,
    pub mta_sts_cert_mode: String,
    #[serde(default, with = "serde_bytes")]
    pub catch_all_actor_id: Option<[u8; 32]>,
    /// JSON-encoded map of `{postmaster, abuse, …}` → actor_id.
    pub role_address_overrides_json: Option<String>,
    /// JSON-encoded per-domain DMARC override map.
    pub dmarc_overrides_json: Option<String>,
    pub spf_record: String,
    /// Epoch-ms when the **active** `dkim_selector` last became active (the
    /// rotation flip stamps it; see `update_mail_domain_config`). `None` until
    /// the first explicit flip — the due computation then falls back to
    /// `added_at` (the first rotation window runs from when the domain was
    /// added). `mail-multidomain.md` § Rotation.
    pub dkim_selector_activated_at: Option<i64>,
    /// Epoch-ms when a succession ceremony or the boot reconcile last
    /// cleared `catch_all_actor_id` because it named a retired identity.
    /// `None` when the catch-all was never set, or was last set/cleared by
    /// an admin (`update_mail_domain_config` clears this stamp on every
    /// admin-driven set/clear — a fresh admin decision supersedes it).
    /// `mail-multidomain.md` § Per-domain catch-all;
    /// `succession-aftermath.md` § Re-key scope.
    pub catch_all_cleared_by_succession_at: Option<i64>,
}

/// Deployment-wide DKIM rotation interval default (quarterly), used when a
/// row's per-domain `dkim_rotation_days` is NULL. Mirrors the `mail.dkim.
/// rotation_days` catalog default (`mail-policy-config.md` § DKIM — "quarterly").
/// There is no readable nest-side policy catalog for it yet, so the default
/// lives here as the single source until that catalog is wired.
pub const DEFAULT_DKIM_ROTATION_DAYS: i64 = 90;

/// Peer-cache warmup window before the scheduled auto-flip activates a freshly-
/// provisioned selector: nest publishes (client-side) the new selector's TXT,
/// then waits this long for peer resolver caches to pick it up before flipping
/// the active signing selector onto it (`mail-multidomain.md` § Rotation,
/// "After 24 h for peer caches"). The emergency `force_rotate_dkim` path passes
/// `0` to skip the wait.
pub const DKIM_ROTATION_CACHE_WARMUP_MS: i64 = 24 * 60 * 60 * 1000;

/// Pure due-computation for the DKIM rotation scheduler: a selector is due once
/// at least `rotation_days` have elapsed since it became active. `now_ms` and
/// `activated_at_ms` are epoch-ms; `rotation_days` is the effective interval
/// (per-domain override, else the deployment default). A non-positive
/// `rotation_days` is treated as "always due" (degenerate; used as a test
/// lever). `mail-multidomain.md` § Rotation.
pub fn dkim_rotation_due(now_ms: i64, activated_at_ms: i64, rotation_days: i64) -> bool {
    let window_ms = rotation_days.saturating_mul(24 * 60 * 60 * 1000);
    now_ms.saturating_sub(activated_at_ms) >= window_ms
}

/// The scheduled-rotation selector name for `now_ms`: `<YYYYMM>` (UTC), e.g.
/// `202606`. The nest-side scheduled rotation-mint
/// (`bridge_routing_handlers::run_scheduled_dkim_rotation_mint`) provisions a
/// due domain's fresh key under this name, then the 24 h auto-flip activates it.
/// Re-derived from the prior client-side `dkim_rotation_selector` shape (removed
/// 2026-06-12 with the client provisioning sweep) so a re-rotation lands on the
/// same monthly cadence. `mail-multidomain.md` § Rotation.
pub fn dkim_rotation_selector(now_ms: i64) -> String {
    let (year, month, _day) = fauna_core::caltime::civil_from_days(now_ms.div_euclid(86_400_000));
    format!("{year:04}{month:02}")
}

impl MailDomain {
    /// When the active selector's rotation window started: the explicit
    /// activation stamp if the selector has ever been flipped, else `added_at`
    /// (the first window runs from when the domain was added). `mail-
    /// multidomain.md` § Rotation, "`added_at` only seeds the first window".
    pub fn effective_dkim_activated_at(&self) -> i64 {
        self.dkim_selector_activated_at.unwrap_or(self.added_at)
    }

    /// The effective rotation interval in days: the per-domain
    /// `dkim_rotation_days` override if set, else the deployment-wide default.
    pub fn effective_dkim_rotation_days(&self, default_days: i64) -> i64 {
        self.dkim_rotation_days.unwrap_or(default_days)
    }

    /// Whether the active DKIM selector is due for rotation as of `now_ms`,
    /// using the per-domain override (else `default_days`). The signal the
    /// admin client reads (projected onto `MailDomainRow.dkim_rotation_due`) and
    /// the seam the deferred 24 h auto-flip iterates. `mail-multidomain.md`
    /// § Rotation.
    pub fn is_dkim_rotation_due(&self, now_ms: i64, default_days: i64) -> bool {
        dkim_rotation_due(
            now_ms,
            self.effective_dkim_activated_at(),
            self.effective_dkim_rotation_days(default_days),
        )
    }
}

/// Partial-update shape for `update_mail_domain_config`. Only fields
/// the admin may mutate post-add (mta_sts max-age + cert mode / catch_all /
/// overrides / spf / dkim_rotation_days). `domain_name`, `is_primary`,
/// `added_at`, `removed_at`, `dkim_algorithms` and `mta_sts_mode` are NOT
/// mutable here (add-time-only or system-managed — the mode's one writer after
/// the add is [`CacheDb::advance_mta_sts_testing_to_enforce_at`]).
///
/// `dkim_selector` is the active-selector flip written by the rotation
/// path (`fauna.bridges.force_rotate_dkim`), NOT a manual admin knob:
/// DKIM is an automatic concern with no manual UI (mail-policy-config.md
/// § Mail-policy ownership), so it rides this internal update shape but
/// is deliberately absent from the wire `UpdateLocalDomainConfigRequest`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MailDomainUpdate {
    pub mta_sts_max_age_seconds: Option<i64>,
    pub mta_sts_cert_mode: Option<String>,
    pub catch_all_actor_id: Option<Option<[u8; 32]>>,
    pub role_address_overrides_json: Option<Option<String>>,
    pub dmarc_overrides_json: Option<Option<String>>,
    pub spf_record: Option<String>,
    pub dkim_rotation_days: Option<Option<i64>>,
    /// `Some(selector)` ⇒ set the active DKIM selector (the rotation flip);
    /// `None` ⇒ leave untouched. Never written to SQL NULL by the flip (the
    /// NULL→"default" projection default is an add-time concern).
    pub dkim_selector: Option<String>,
}

impl CacheDb {
    /// Insert a fresh `mail_domains` row. Generates a UUID v4 for
    /// `domain_id`. Defaults per goal-doc § Row shape (dkim_algorithms
    /// `["ed25519","rsa-2048"]`, mta_sts_max_age 86400, spf_record
    /// `v=spf1 mx ~all`).
    ///
    /// Refuses with a typed error if:
    /// - another active row already carries `domain_name` (unique index),
    /// - `is_primary = true` and another active primary already exists.
    pub async fn add_mail_domain(
        &self,
        domain_name: &str,
        is_primary: bool,
        mta_sts_mode: &str,
        mta_sts_cert_mode: &str,
        catch_all_actor_id: Option<&[u8; 32]>,
        dkim_selector_override: Option<&str>,
    ) -> Result<MailDomain> {
        let domain_id = *Uuid::new_v4().as_bytes();
        let name = domain_name.to_ascii_lowercase();
        let mta_mode = mta_sts_mode.to_string();
        let mta_cert = mta_sts_cert_mode.to_string();
        let catch_all_owned: Option<[u8; 32]> = catch_all_actor_id.copied();
        let selector_owned = dkim_selector_override.map(|s| s.to_string());
        let now = now_epoch_millis();

        {
            let conn = self.conn.lock().await;
            // A domain coming back after a factory reset registers under the
            // selector of the key the reset carried for it, so the key its DNS
            // still publishes is the one it signs with (`nest/common.md`
            // § Factory reset → *The DKIM keys are carried*).
            let selector_owned = match selector_owned {
                Some(selector) => Some(selector),
                None => crate::mail_dkim_key::carried_selector(&conn, &name)
                    .unwrap_or_else(|e| {
                        tracing::error!(
                            target: "mail_dkim",
                            domain = %name,
                            "could not read the carried DKIM selector: {e:#}"
                        );
                        None
                    })
                    .filter(|selector| selector != crate::mail_dkim_key::DEFAULT_SELECTOR),
            };
            if is_primary {
                let existing_primary: Option<Vec<u8>> = conn
                    .query_row(
                        "SELECT domain_id FROM mail_domains
                         WHERE removed_at IS NULL AND is_primary = 1
                         LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .optional()
                    .context("check existing primary")?;
                if existing_primary.is_some() {
                    return Err(anyhow!("another primary domain already exists"));
                }
            }
            conn.execute(
                "INSERT INTO mail_domains
                    (domain_id, domain_name, is_primary, added_at,
                     dkim_selector, mta_sts_mode, mta_sts_cert_mode,
                     catch_all_actor_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    &domain_id[..],
                    &name,
                    if is_primary { 1 } else { 0 },
                    now,
                    selector_owned.as_deref(),
                    &mta_mode,
                    &mta_cert,
                    catch_all_owned.as_ref().map(|a| &a[..]),
                ],
            )
            .context("insert mail_domains row")?;
            mint_dkim_key_for_activated_domain(&conn, &name, selector_owned.as_deref());
        }

        self.lookup_mail_domain_by_id(&domain_id)
            .await?
            .ok_or_else(|| anyhow!("mail_domains row vanished after insert"))
    }

    /// Soft-delete: set `removed_at = NOW()`. Refuses the primary
    /// (per goal-doc § Removing a local domain — `403
    /// cannot_remove_primary_domain`).
    pub async fn soft_delete_mail_domain(&self, domain_name: &str) -> Result<MailDomain> {
        let name = domain_name.to_ascii_lowercase();
        let now = now_epoch_millis();

        let row = self
            .lookup_active_mail_domain(&name)
            .await?
            .ok_or_else(|| anyhow!("no active mail_domains row for {}", name))?;

        if row.is_primary {
            return Err(anyhow!("cannot remove primary domain"));
        }

        {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE mail_domains SET removed_at = ?1
                 WHERE domain_id = ?2 AND removed_at IS NULL",
                rusqlite::params![now, &row.domain_id[..]],
            )
            .context("soft-delete mail_domains row")?;
        }

        self.lookup_mail_domain_by_id(&row.domain_id)
            .await?
            .ok_or_else(|| anyhow!("row vanished after soft-delete"))
    }

    /// Restore a soft-deleted row within the 30-day recovery window.
    /// Past the window, returns a typed error the HTTP layer maps to
    /// 410. Records `restored_at = NOW()` for audit; clears `removed_at`.
    pub async fn restore_mail_domain(&self, domain_name: &str) -> Result<MailDomain> {
        let name = domain_name.to_ascii_lowercase();
        let now = now_epoch_millis();

        let candidate = self.lookup_mail_domain_by_name_any(&name).await?;
        let row = candidate.ok_or_else(|| anyhow!("no mail_domains row for {}", name))?;

        let Some(removed_at) = row.removed_at else {
            return Err(anyhow!("mail_domains row for {} is not soft-deleted", name));
        };

        if now - removed_at > SOFT_DELETE_RECOVERY_WINDOW_MS {
            return Err(anyhow!(
                "mail_domains row for {} is past the 30-day recovery window",
                name
            ));
        }

        {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE mail_domains
                 SET removed_at = NULL, restored_at = ?1
                 WHERE domain_id = ?2",
                rusqlite::params![now, &row.domain_id[..]],
            )
            .context("restore mail_domains row")?;
            mint_dkim_key_for_activated_domain(&conn, &name, row.dkim_selector.as_deref());
        }

        self.lookup_mail_domain_by_id(&row.domain_id)
            .await?
            .ok_or_else(|| anyhow!("row vanished after restore"))
    }

    /// GC every `mail_domains` row whose 30-day soft-delete recovery window
    /// has elapsed (`mail-multidomain.md` § After 30 days + § Architectural
    /// rules). For each expired row this hard-deletes the row **and** its
    /// per-domain dependents in one transaction — the DKIM keys
    /// (`mail_dkim_keys`), the per-domain TLS cert blobs (`bridge_tls_cert_blobs`),
    /// and the (disabled) `account_aliases` rows.
    ///
    /// The goal doc names a `mail_domains` ON DELETE CASCADE for the aliases, but
    /// no such FK exists in SQLite today (`account_aliases.local_domain` is a plain
    /// column), so the dependents are deleted **explicitly**; likewise the DKIM key
    /// lives in `mail_dkim_keys(domain, selector)`, which `mail_domains` does
    /// not reference. The primary can never be soft-deleted, so it is never expired;
    /// `is_primary = 0` is belt-and-suspenders. Returns the GC'd domain names.
    pub async fn gc_expired_soft_deleted_mail_domains(&self) -> Result<Vec<String>> {
        self.gc_expired_soft_deleted_mail_domains_at(now_epoch_millis())
            .await
    }

    /// `now`-injectable core of [`Self::gc_expired_soft_deleted_mail_domains`] for
    /// deterministic tests (a row removed "now" appears expired when GC runs with a
    /// `now_ms` ≥ 30 days later, without sleeping).
    async fn gc_expired_soft_deleted_mail_domains_at(&self, now_ms: i64) -> Result<Vec<String>> {
        let cutoff = now_ms - SOFT_DELETE_RECOVERY_WINDOW_MS;
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin gc transaction")?;

        let expired: Vec<(Vec<u8>, String)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT domain_id, domain_name FROM mail_domains
                     WHERE removed_at IS NOT NULL AND removed_at < ?1 AND is_primary = 0",
                )
                .context("prepare gc select")?;
            let rows = stmt
                .query_map([cutoff], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
                })
                .context("query gc select")?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("collect gc select")?
        };

        for (domain_id, domain_name) in &expired {
            tx.execute(
                "DELETE FROM account_aliases WHERE local_domain = ?1",
                [domain_name],
            )
            .context("gc delete account_aliases")?;
            tx.execute(
                "DELETE FROM mail_dkim_keys WHERE domain = ?1",
                [domain_name],
            )
            .context("gc delete mail_dkim_keys")?;
            tx.execute(
                "DELETE FROM bridge_tls_cert_blobs WHERE domain = ?1",
                [domain_name],
            )
            .context("gc delete bridge_tls_cert_blobs")?;
            tx.execute(
                "DELETE FROM mail_domains WHERE domain_id = ?1",
                [&domain_id[..]],
            )
            .context("gc delete mail_domains row")?;
        }

        // A DKIM key a factory reset carried for a domain nobody re-registered
        // goes on the same window.
        crate::mail_dkim_key::expire_carried(&tx, cutoff)?;

        tx.commit().context("commit gc transaction")?;
        Ok(expired.into_iter().map(|(_, name)| name).collect())
    }

    /// Advance every active domain whose MTA-STS `testing` window has run out
    /// from stored `testing` to stored `enforce`, and return their names
    /// (`mail-multidomain.md` § The advance). The window runs from the later of
    /// `added_at` and `restored_at`: a restore restarts it, because no policy was
    /// served for the domain while it was removed. One-way and idempotent — one
    /// statement, matching only `testing` rows, so a second run (or a crash
    /// between the write and the caller's follow-up) changes nothing.
    ///
    /// This is the clock half only. Whether the mail name is on a trusted
    /// certificate is the caller's to check first
    /// (`crate::mta_sts_advance::advance_mta_sts_modes_at`); `now_ms` is a
    /// parameter so tests move the clock without a knob on the binary.
    pub async fn advance_mta_sts_testing_to_enforce_at(&self, now_ms: i64) -> Result<Vec<String>> {
        let cutoff = now_ms - MTA_STS_TESTING_WINDOW_MS;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "UPDATE mail_domains SET mta_sts_mode = 'enforce'
                 WHERE removed_at IS NULL
                   AND mta_sts_mode = 'testing'
                   AND MAX(added_at, COALESCE(restored_at, added_at)) <= ?1
                 RETURNING domain_name",
            )
            .context("prepare mta-sts advance")?;
        let rows = stmt
            .query_map([cutoff], |row| row.get::<_, String>(0))
            .context("run mta-sts advance")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect mta-sts advance")
    }

    /// Apply a partial config update. Each `Some(value)` field
    /// overrides; `None` leaves the column untouched. Doubly-wrapped
    /// `Option<Option<T>>` fields (`catch_all_actor_id`,
    /// `role_address_overrides_json`, `dmarc_overrides_json`,
    /// `dkim_rotation_days`): outer `Some` ⇒ update; inner `None` ⇒
    /// write SQL NULL.
    pub async fn update_mail_domain_config(
        &self,
        domain_name: &str,
        update: MailDomainUpdate,
    ) -> Result<MailDomain> {
        let name = domain_name.to_ascii_lowercase();

        let row = self
            .lookup_active_mail_domain(&name)
            .await?
            .ok_or_else(|| anyhow!("no active mail_domains row for {}", name))?;

        let mut set_clauses: Vec<&'static str> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql + Send + Sync>> = Vec::new();

        if let Some(v) = update.mta_sts_max_age_seconds {
            set_clauses.push("mta_sts_max_age_seconds = ?");
            params.push(Box::new(v));
        }
        if let Some(m) = &update.mta_sts_cert_mode {
            set_clauses.push("mta_sts_cert_mode = ?");
            params.push(Box::new(m.clone()));
        }
        if let Some(opt) = &update.catch_all_actor_id {
            set_clauses.push("catch_all_actor_id = ?");
            match opt {
                Some(a) => params.push(Box::new(a.to_vec())),
                None => params.push(Box::new(Option::<Vec<u8>>::None)),
            }
            // Any admin-driven set/clear supersedes a prior succession-clear
            // signal — the admin has now made a fresh decision, so the
            // "cleared by a succession" flag no longer applies (mail-multidomain.md § Per-domain catch-all).
            set_clauses.push("catch_all_cleared_by_succession_at = ?");
            params.push(Box::new(Option::<i64>::None));
        }
        if let Some(opt) = &update.role_address_overrides_json {
            set_clauses.push("role_address_overrides = ?");
            params.push(Box::new(opt.clone()));
        }
        if let Some(opt) = &update.dmarc_overrides_json {
            set_clauses.push("dmarc_overrides = ?");
            params.push(Box::new(opt.clone()));
        }
        if let Some(s) = &update.spf_record {
            set_clauses.push("spf_record = ?");
            params.push(Box::new(s.clone()));
        }
        if let Some(opt) = &update.dkim_rotation_days {
            set_clauses.push("dkim_rotation_days = ?");
            params.push(Box::new(*opt));
        }
        if let Some(s) = &update.dkim_selector {
            set_clauses.push("dkim_selector = ?");
            params.push(Box::new(s.clone()));
            // Stamp the activation time on every selector flip so the emergency
            // (`force_rotate_dkim`) and future scheduled paths agree by
            // construction — the rotation due-detector reads this (else
            // `added_at`). `mail-multidomain.md` § Rotation.
            set_clauses.push("dkim_selector_activated_at = ?");
            params.push(Box::new(now_epoch_millis()));
        }

        if !set_clauses.is_empty() {
            let sql = format!(
                "UPDATE mail_domains SET {} WHERE domain_id = ?",
                set_clauses.join(", ")
            );
            params.push(Box::new(row.domain_id.to_vec()));

            let conn = self.conn.lock().await;
            let refs: Vec<&dyn rusqlite::ToSql> = params
                .iter()
                .map(|p| p.as_ref() as &dyn rusqlite::ToSql)
                .collect();
            conn.execute(&sql, refs.as_slice())
                .context("update mail_domains row")?;
        }

        self.lookup_mail_domain_by_id(&row.domain_id)
            .await?
            .ok_or_else(|| anyhow!("row vanished after update"))
    }

    /// Atomically set (or clear) one role's per-domain override actor in the
    /// `role_address_overrides` JSON map, **preserving the other roles**
    /// (`mail-multidomain.md` § Per-domain role-address routing). `role_key` is
    /// one of `fauna_mail::aliases::role_overrides::OVERRIDABLE_ROLE_KEYS`;
    /// `actor_hex` is a 64-char lowercase actor hex to designate, or `None` to
    /// clear (the role then falls back to the deployment admin). The
    /// read-merge-write runs under a **single** connection lock so two concurrent
    /// role sets on the same domain can't lose each other's edit — unlike
    /// catch-all (a single column), the override map is merged, so a generic
    /// `MailDomainUpdate` write would clobber sibling roles. Writes SQL NULL when
    /// the map becomes empty (the canonical "no overrides" state).
    pub async fn set_role_address_override(
        &self,
        domain_name: &str,
        role_key: &str,
        actor_hex: Option<String>,
    ) -> Result<MailDomain> {
        use fauna_mail::aliases::role_overrides::{parse_stored, to_stored};
        let name = domain_name.to_ascii_lowercase();

        let domain_id: [u8; 16] = {
            let conn = self.conn.lock().await;
            let row: Option<(Vec<u8>, Option<String>)> = conn
                .query_row(
                    "SELECT domain_id, role_address_overrides FROM mail_domains
                     WHERE domain_name = ?1 AND removed_at IS NULL",
                    [&name],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .context("read role_address_overrides")?;
            let (id_blob, current_json) =
                row.ok_or_else(|| anyhow!("no active mail_domains row for {}", name))?;

            let mut overrides = parse_stored(current_json.as_deref());
            overrides.set(role_key, actor_hex);
            let new_json = to_stored(&overrides);

            if id_blob.len() != 16 {
                return Err(anyhow!("domain_id must be 16 bytes, got {}", id_blob.len()));
            }
            let mut id = [0u8; 16];
            id.copy_from_slice(&id_blob);
            conn.execute(
                "UPDATE mail_domains SET role_address_overrides = ?1 WHERE domain_id = ?2",
                rusqlite::params![new_json, &id[..]],
            )
            .context("update role_address_overrides")?;
            id
        };

        self.lookup_mail_domain_by_id(&domain_id)
            .await?
            .ok_or_else(|| anyhow!("row vanished after role-address override update"))
    }

    /// Atomically set one domain's published DMARC policy in its stored
    /// `dmarc_overrides` partial, **preserving the partial's other keys** —
    /// the per-domain policy select (`dmarc-reporting.md` § Multi-domain
    /// deployments). The merge rule is the shared
    /// `fauna_mail::dmarc_publish::set_policy_mode_json`: `policy_mode` and
    /// `subdomain_policy_mode` set together, the default (`reject`) clearing
    /// both. Same single-lock read-merge-write as
    /// [`Self::set_role_address_override`], for the same reason.
    pub async fn set_dmarc_policy_mode(
        &self,
        domain_name: &str,
        mode: fauna_mail::dmarc_publish::DmarcMode,
    ) -> Result<MailDomain> {
        use fauna_mail::dmarc_publish::set_policy_mode_json;
        let name = domain_name.to_ascii_lowercase();

        let domain_id: [u8; 16] = {
            let conn = self.conn.lock().await;
            let row: Option<(Vec<u8>, Option<String>)> = conn
                .query_row(
                    "SELECT domain_id, dmarc_overrides FROM mail_domains
                     WHERE domain_name = ?1 AND removed_at IS NULL",
                    [&name],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .context("read dmarc_overrides")?;
            let (id_blob, current_json) =
                row.ok_or_else(|| anyhow!("no active mail_domains row for {}", name))?;
            let new_json = set_policy_mode_json(current_json.as_deref(), mode);

            if id_blob.len() != 16 {
                return Err(anyhow!("domain_id must be 16 bytes, got {}", id_blob.len()));
            }
            let mut id = [0u8; 16];
            id.copy_from_slice(&id_blob);
            conn.execute(
                "UPDATE mail_domains SET dmarc_overrides = ?1 WHERE domain_id = ?2",
                rusqlite::params![new_json, &id[..]],
            )
            .context("update dmarc_overrides")?;
            id
        };

        self.lookup_mail_domain_by_id(&domain_id)
            .await?
            .ok_or_else(|| anyhow!("row vanished after DMARC policy update"))
    }

    /// Active rows only (the `mail.local_domains` projection). Ordered
    /// with the primary first, then by add-time ascending — the
    /// consumer's bridge iterates in this order so the primary is
    /// `[0]` if anyone wants to peek without consulting
    /// `primary_domain` separately (a defence-in-depth alignment with
    /// the explicit `primary_domain` field).
    pub async fn list_active_mail_domains(&self) -> Result<Vec<MailDomain>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT
                    domain_id, domain_name, is_primary, added_at, removed_at,
                    restored_at, dkim_selector,
                    dkim_rotation_days, dkim_algorithms, mta_sts_mode,
                    mta_sts_max_age_seconds, mta_sts_cert_mode,
                    catch_all_actor_id, role_address_overrides,
                    dmarc_overrides, spf_record, dkim_selector_activated_at,
                    catch_all_cleared_by_succession_at
                 FROM mail_domains
                 WHERE removed_at IS NULL
                 ORDER BY is_primary DESC, added_at ASC",
            )
            .context("prepare list_active_mail_domains")?;
        let rows = stmt
            .query_map([], row_to_mail_domain)
            .context("query list_active_mail_domains")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_active_mail_domains")
    }

    /// Active rows whose DKIM selector is **due for rotation** as of now
    /// (`now - activated_at ≥ effective rotation_days`). Isolates the clock +
    /// the deployment-wide default inside the db layer; the scheduled-rotation
    /// tick logs these and feeds them to `flip_to_newest_dkim_selector`.
    /// `mail-multidomain.md` § Rotation.
    pub async fn dkim_rotation_due_domains(&self) -> Result<Vec<MailDomain>> {
        let now = now_epoch_millis();
        Ok(self
            .list_active_mail_domains()
            .await?
            .into_iter()
            .filter(|d| d.is_dkim_rotation_due(now, DEFAULT_DKIM_ROTATION_DAYS))
            .collect())
    }

    /// Flip `row`'s active DKIM selector to its **newest-provisioned** selector,
    /// IFF that selector is (a) different from the current active one and (b) at
    /// least `min_provision_age_ms` old (the peer-cache warmup —
    /// `DKIM_ROTATION_CACHE_WARMUP_MS` for the scheduled path, `0` for the
    /// emergency `force_rotate_dkim` path which skips the wait). Returns the
    /// updated row on a flip, else `None` (nothing newer provisioned, or the
    /// newest isn't aged past the window yet — the rotation then waits for the
    /// client to provision a key, per `mail-multidomain.md` § Rotation). The
    /// **single** flip primitive both rotation paths share, so they agree on
    /// "newest-provisioned" + the `dkim_selector_activated_at` stamp (written by
    /// `update_mail_domain_config`). Idempotent: re-running after a flip is a
    /// no-op (the newest is now the current), so a crashed tick is recoverable.
    pub async fn flip_to_newest_dkim_selector(
        &self,
        row: &MailDomain,
        min_provision_age_ms: i64,
    ) -> Result<Option<MailDomain>> {
        let current = row.dkim_selector.as_deref().unwrap_or("default");
        let provisioned = self.list_dkim_selectors(Some(&row.domain_name)).await?;
        // `list_dkim_selectors` orders by `created_at` ascending → newest is last.
        let Some(newest) = provisioned.last() else {
            return Ok(None);
        };
        if newest.selector == current {
            return Ok(None);
        }
        if now_epoch_millis() - newest.created_at < min_provision_age_ms {
            return Ok(None); // not yet aged past the peer-cache warmup window
        }
        let updated = self
            .update_mail_domain_config(
                &row.domain_name,
                MailDomainUpdate {
                    dkim_selector: Some(newest.selector.clone()),
                    ..Default::default()
                },
            )
            .await?;
        Ok(Some(updated))
    }

    /// Rows soft-deleted within the 30-day recovery window. Used by
    /// the admin list endpoint's "recently removed; can restore" pane.
    pub async fn list_soft_deleted_within_30d_mail_domains(&self) -> Result<Vec<MailDomain>> {
        let now = now_epoch_millis();
        let cutoff = now - SOFT_DELETE_RECOVERY_WINDOW_MS;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT
                    domain_id, domain_name, is_primary, added_at, removed_at,
                    restored_at, dkim_selector,
                    dkim_rotation_days, dkim_algorithms, mta_sts_mode,
                    mta_sts_max_age_seconds, mta_sts_cert_mode,
                    catch_all_actor_id, role_address_overrides,
                    dmarc_overrides, spf_record, dkim_selector_activated_at,
                    catch_all_cleared_by_succession_at
                 FROM mail_domains
                 WHERE removed_at IS NOT NULL AND removed_at > ?1
                 ORDER BY removed_at DESC",
            )
            .context("prepare list_soft_deleted_within_30d")?;
        let rows = stmt
            .query_map([cutoff], row_to_mail_domain)
            .context("query list_soft_deleted_within_30d")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_soft_deleted_within_30d")
    }

    pub async fn lookup_active_mail_domain(&self, domain_name: &str) -> Result<Option<MailDomain>> {
        let name = domain_name.to_ascii_lowercase();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT
                domain_id, domain_name, is_primary, added_at, removed_at,
                restored_at, dkim_selector,
                dkim_rotation_days, dkim_algorithms, mta_sts_mode,
                mta_sts_max_age_seconds, mta_sts_cert_mode,
                catch_all_actor_id, role_address_overrides,
                dmarc_overrides, spf_record, dkim_selector_activated_at,
                catch_all_cleared_by_succession_at
             FROM mail_domains
             WHERE domain_name = ?1 AND removed_at IS NULL",
            [&name],
            row_to_mail_domain,
        )
        .optional()
        .context("lookup_active_mail_domain")
    }

    /// Used by `fetch_config_handler` to surface `primary_domain` on
    /// `FetchConfigReply`. Returns `None` on a fresh nest with no
    /// domains yet.
    pub async fn lookup_primary_mail_domain(&self) -> Result<Option<MailDomain>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT
                domain_id, domain_name, is_primary, added_at, removed_at,
                restored_at, dkim_selector,
                dkim_rotation_days, dkim_algorithms, mta_sts_mode,
                mta_sts_max_age_seconds, mta_sts_cert_mode,
                catch_all_actor_id, role_address_overrides,
                dmarc_overrides, spf_record, dkim_selector_activated_at,
                catch_all_cleared_by_succession_at
             FROM mail_domains
             WHERE removed_at IS NULL AND is_primary = 1
             LIMIT 1",
            [],
            row_to_mail_domain,
        )
        .optional()
        .context("lookup_primary_mail_domain")
    }

    pub async fn lookup_mail_domain_by_id(
        &self,
        domain_id: &[u8; 16],
    ) -> Result<Option<MailDomain>> {
        let id_vec = domain_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT
                domain_id, domain_name, is_primary, added_at, removed_at,
                restored_at, dkim_selector,
                dkim_rotation_days, dkim_algorithms, mta_sts_mode,
                mta_sts_max_age_seconds, mta_sts_cert_mode,
                catch_all_actor_id, role_address_overrides,
                dmarc_overrides, spf_record, dkim_selector_activated_at,
                catch_all_cleared_by_succession_at
             FROM mail_domains
             WHERE domain_id = ?1",
            [&id_vec],
            row_to_mail_domain,
        )
        .optional()
        .context("lookup_mail_domain_by_id")
    }

    /// Lookup by name across active + soft-deleted rows. Used by the
    /// restore path (the soft-deleted row is what we restore).
    async fn lookup_mail_domain_by_name_any(
        &self,
        domain_name: &str,
    ) -> Result<Option<MailDomain>> {
        let name = domain_name.to_ascii_lowercase();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT
                domain_id, domain_name, is_primary, added_at, removed_at,
                restored_at, dkim_selector,
                dkim_rotation_days, dkim_algorithms, mta_sts_mode,
                mta_sts_max_age_seconds, mta_sts_cert_mode,
                catch_all_actor_id, role_address_overrides,
                dmarc_overrides, spf_record, dkim_selector_activated_at,
                catch_all_cleared_by_succession_at
             FROM mail_domains
             WHERE domain_name = ?1
             ORDER BY removed_at IS NULL DESC, removed_at DESC
             LIMIT 1",
            [&name],
            row_to_mail_domain,
        )
        .optional()
        .context("lookup_mail_domain_by_name_any")
    }
}

fn row_to_mail_domain(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailDomain> {
    let domain_id_blob: Vec<u8> = row.get(0)?;
    let mut domain_id = [0u8; 16];
    if domain_id_blob.len() == 16 {
        domain_id.copy_from_slice(&domain_id_blob);
    } else {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Blob,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("domain_id must be 16 bytes, got {}", domain_id_blob.len()),
            )),
        ));
    }

    let catch_all_blob: Option<Vec<u8>> = row.get(12)?;
    let catch_all_actor_id = match catch_all_blob {
        Some(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            Some(arr)
        }
        Some(b) => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Blob,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("catch_all_actor_id must be 32 bytes, got {}", b.len()),
                )),
            ));
        }
        None => None,
    };

    let is_primary_int: i64 = row.get(2)?;
    Ok(MailDomain {
        domain_id,
        domain_name: row.get(1)?,
        is_primary: is_primary_int != 0,
        added_at: row.get(3)?,
        removed_at: row.get(4)?,
        restored_at: row.get(5)?,
        dkim_selector: row.get(6)?,
        dkim_rotation_days: row.get(7)?,
        dkim_algorithms_json: row.get(8)?,
        mta_sts_mode: row.get(9)?,
        mta_sts_max_age_seconds: row.get(10)?,
        mta_sts_cert_mode: row.get(11)?,
        catch_all_actor_id,
        role_address_overrides_json: row.get(13)?,
        dmarc_overrides_json: row.get(14)?,
        spf_record: row.get(15)?,
        dkim_selector_activated_at: row.get(16)?,
        catch_all_cleared_by_succession_at: row.get(17)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn add_then_list_active_returns_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        assert_eq!(row.domain_name, "example.com");
        assert!(row.is_primary);
        assert!(row.removed_at.is_none());
        assert_eq!(row.mta_sts_mode, "enforce");
        assert_eq!(row.mta_sts_cert_mode, "expand_primary");
        assert_eq!(row.mta_sts_max_age_seconds, 86400);
        assert_eq!(row.spf_record, "v=spf1 mx ~all");
        assert_eq!(row.dkim_algorithms_json, r#"["ed25519","rsa-2048"]"#);

        let active = db.list_active_mail_domains().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].domain_name, "example.com");
        assert!(active[0].is_primary);
    }

    #[test]
    fn dkim_rotation_selector_known_values() {
        assert_eq!(dkim_rotation_selector(0), "197001");
        // 2023-11-14 22:13:20 UTC (same instant `ical.rs`'s
        // `epoch_secs_to_ical_utc_known_values` test pins).
        assert_eq!(dkim_rotation_selector(1_700_000_000_000), "202311");
        // Leap-day month, still resolves to the right month.
        assert_eq!(dkim_rotation_selector(1_709_251_199_000), "202402");
    }

    // ── DKIM scheduled-rotation due-detection (mail-multidomain.md § Rotation) ──

    #[test]
    fn dkim_rotation_due_pure_boundary() {
        let day = 24 * 60 * 60 * 1000i64;
        let activated = 1_000_000_000_000i64;
        // Exactly at the window boundary is due (≥).
        assert!(dkim_rotation_due(activated + 90 * day, activated, 90));
        // One ms short is not due.
        assert!(!dkim_rotation_due(activated + 90 * day - 1, activated, 90));
        // Long past is due; well within is not.
        assert!(dkim_rotation_due(activated + 200 * day, activated, 90));
        assert!(!dkim_rotation_due(activated + day, activated, 90));
        // Degenerate "always due" lever (rotation_days = 0).
        assert!(dkim_rotation_due(activated, activated, 0));
    }

    #[tokio::test]
    async fn add_seeds_first_window_from_added_at_no_activation_stamp() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        // No explicit flip yet → the activation stamp is NULL and the window
        // runs from `added_at` (just now → not due under the quarterly default).
        assert!(row.dkim_selector_activated_at.is_none());
        assert_eq!(row.effective_dkim_activated_at(), row.added_at);
        assert!(!row.is_dkim_rotation_due(now_epoch_millis(), DEFAULT_DKIM_ROTATION_DAYS));
        // A row whose first window has elapsed (added 100 days ago) is due.
        assert!(row.is_dkim_rotation_due(row.added_at + 91 * 24 * 60 * 60 * 1000, 90));
    }

    #[tokio::test]
    async fn selector_flip_stamps_activation_time() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let before = now_epoch_millis();
        let updated = db
            .update_mail_domain_config(
                "example.com",
                MailDomainUpdate {
                    dkim_selector: Some("202606".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let stamp = updated
            .dkim_selector_activated_at
            .expect("flip stamps activation time");
        assert!(stamp >= before && stamp <= now_epoch_millis());
        // A non-selector update must NOT re-stamp (only the flip does).
        let again = db
            .update_mail_domain_config(
                "example.com",
                MailDomainUpdate {
                    spf_record: Some("v=spf1 -all".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(again.dkim_selector_activated_at, Some(stamp));
    }

    /// A succession's clear is superseded by the next thing an admin actually
    /// decides — designating a fresh actor, or explicitly picking "none" again
    /// — either way the "cleared by a succession" flag must not linger past a
    /// fresh admin decision (mail-multidomain.md § Per-domain
    /// catch-all).
    #[tokio::test]
    async fn admin_redesignating_catch_all_clears_the_succession_stamp() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        // Simulate what a succession clear leaves behind: `catch_all_actor_id`
        // NULL, succession stamp set. Exercised end-to-end (via a real
        // succession) in `successions.rs`'s
        // `a_succession_clears_a_catch_all_that_named_the_retired_identity`;
        // here we only need the post-clear state as a starting point.
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE mail_domains SET catch_all_cleared_by_succession_at = ?1
                  WHERE domain_name = 'example.com'",
                rusqlite::params![now_epoch_millis()],
            )
            .unwrap();
        }

        let fresh_actor = [0x22u8; 32];
        let updated = db
            .update_mail_domain_config(
                "example.com",
                MailDomainUpdate {
                    catch_all_actor_id: Some(Some(fresh_actor)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.catch_all_actor_id, Some(fresh_actor));
        assert_eq!(
            updated.catch_all_cleared_by_succession_at, None,
            "designating a fresh catch-all is the admin's own decision — the \
             succession-clear flag must not linger and misrepresent it as still \
             succession-cleared"
        );
    }

    #[tokio::test]
    async fn due_set_honours_per_domain_rotation_days_override() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain(
            "fresh.example",
            true,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.add_mail_domain(
            "due.example",
            false,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        // Default-window domain isn't due; nothing flagged yet.
        assert!(db.dkim_rotation_due_domains().await.unwrap().is_empty());
        // Accelerate one domain to rotation_days = 0 (the test lever; no wire
        // surface exposes it, so this exercises the DB-API tri-state directly).
        db.update_mail_domain_config(
            "due.example",
            MailDomainUpdate {
                dkim_rotation_days: Some(Some(0)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let due = db.dkim_rotation_due_domains().await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].domain_name, "due.example");
    }

    #[tokio::test]
    async fn flip_to_newest_dkim_selector_flips_stamps_and_respects_age_gate() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain(
            "example.com",
            true,
            "enforce",
            "expand_primary",
            None,
            Some("default"),
        )
        .await
        .unwrap();
        // Provision the active key, then (a few ms later, so created_at orders) a
        // newer selector — both just-provisioned.
        db.seat_dkim_selector_for_test("example.com", "default", "v=DKIM1; k=ed25519; p=OLD")
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        db.seat_dkim_selector_for_test("example.com", "202606", "v=DKIM1; k=ed25519; p=NEW")
            .await;
        let row = db
            .lookup_active_mail_domain("example.com")
            .await
            .unwrap()
            .unwrap();

        // A huge min-age blocks a just-provisioned newer selector (the warmup gate).
        assert!(
            db.flip_to_newest_dkim_selector(&row, i64::MAX / 2)
                .await
                .unwrap()
                .is_none(),
            "a freshly-provisioned selector must not flip until aged past the window"
        );
        // min_age = 0 (the emergency path) flips immediately + stamps activation.
        let updated = db
            .flip_to_newest_dkim_selector(&row, 0)
            .await
            .unwrap()
            .expect("min_age 0 flips to the newest selector");
        assert_eq!(updated.dkim_selector.as_deref(), Some("202606"));
        assert!(updated.dkim_selector_activated_at.is_some());
        // Idempotent: re-running after the flip is a no-op (newest == current),
        // so a crashed/re-run tick can't double-flip.
        assert!(
            db.flip_to_newest_dkim_selector(&updated, 0)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn flip_to_newest_dkim_selector_none_when_nothing_newer() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let row = db
            .lookup_active_mail_domain("example.com")
            .await
            .unwrap()
            .unwrap();
        // No DKIM blobs provisioned at all → nothing to flip to.
        assert!(
            db.flip_to_newest_dkim_selector(&row, 0)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn lookup_primary_returns_primary_among_active() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(db.lookup_primary_mail_domain().await.unwrap().is_none());

        db.add_mail_domain(
            "primary.example",
            true,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.add_mail_domain(
            "secondary.example",
            false,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();

        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(primary.domain_name, "primary.example");
        assert!(primary.is_primary);

        let active = db.list_active_mail_domains().await.unwrap();
        assert_eq!(active.len(), 2);
        // Primary first per `ORDER BY is_primary DESC, added_at ASC`.
        assert_eq!(active[0].domain_name, "primary.example");
        assert_eq!(active[1].domain_name, "secondary.example");
    }

    #[tokio::test]
    async fn duplicate_active_domain_name_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let err = db
            .add_mail_domain(
                "example.com",
                false,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await;
        assert!(
            err.is_err(),
            "expected duplicate active domain to be refused"
        );
    }

    #[tokio::test]
    async fn second_primary_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain(
            "primary.example",
            true,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        let err = db
            .add_mail_domain(
                "other.example",
                true,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await;
        assert!(
            err.is_err(),
            "second primary should be refused (partial unique index + pre-check)"
        );
    }

    #[tokio::test]
    async fn soft_delete_primary_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain(
            "primary.example",
            true,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        let err = db.soft_delete_mail_domain("primary.example").await;
        assert!(err.is_err());
        assert!(
            format!("{}", err.unwrap_err()).contains("primary"),
            "error should mention primary"
        );
    }

    #[tokio::test]
    async fn soft_delete_then_restore_round_trip() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain(
            "primary.example",
            true,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.add_mail_domain(
            "removable.example",
            false,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();

        let removed = db
            .soft_delete_mail_domain("removable.example")
            .await
            .unwrap();
        assert!(removed.removed_at.is_some());

        // No longer in the active projection.
        let active = db.list_active_mail_domains().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].domain_name, "primary.example");

        // Visible in the soft-deleted-within-30d list.
        let soft = db
            .list_soft_deleted_within_30d_mail_domains()
            .await
            .unwrap();
        assert_eq!(soft.len(), 1);
        assert_eq!(soft[0].domain_name, "removable.example");

        // Restore.
        let restored = db.restore_mail_domain("removable.example").await.unwrap();
        assert!(restored.removed_at.is_none());
        assert!(restored.restored_at.is_some());

        // Back in the active projection.
        let active = db.list_active_mail_domains().await.unwrap();
        assert_eq!(active.len(), 2);
    }

    #[tokio::test]
    async fn restore_non_soft_deleted_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let err = db.restore_mail_domain("example.com").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn restore_unknown_domain_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let err = db.restore_mail_domain("never.heard.of.it").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn update_config_partial_overrides() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain(
            "primary.example",
            true,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.add_mail_domain(
            "configurable.example",
            false,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();

        let updated = db
            .update_mail_domain_config(
                "configurable.example",
                MailDomainUpdate {
                    mta_sts_max_age_seconds: Some(604800),
                    spf_record: Some("v=spf1 mx -all".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // The mode is not an update field: it stays what the row held.
        assert_eq!(updated.mta_sts_mode, "enforce");
        assert_eq!(updated.mta_sts_max_age_seconds, 604800);
        assert_eq!(updated.spf_record, "v=spf1 mx -all");
        // Untouched fields stay at their defaults.
        assert_eq!(updated.mta_sts_cert_mode, "expand_primary");
    }

    #[tokio::test]
    async fn mta_sts_advance_waits_out_the_window_then_moves_testing_to_enforce() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = db
            .add_mail_domain("example.com", true, "testing", "expand_primary", None, None)
            .await
            .unwrap();

        // One millisecond short of the window: nothing moves.
        let early = row.added_at + MTA_STS_TESTING_WINDOW_MS - 1;
        assert!(
            db.advance_mta_sts_testing_to_enforce_at(early)
                .await
                .unwrap()
                .is_empty()
        );
        let still = db.lookup_active_mail_domain("example.com").await.unwrap();
        assert_eq!(still.unwrap().mta_sts_mode, "testing");

        // At the window: advanced, and named.
        let due = row.added_at + MTA_STS_TESTING_WINDOW_MS;
        assert_eq!(
            db.advance_mta_sts_testing_to_enforce_at(due).await.unwrap(),
            vec!["example.com".to_string()]
        );
        let moved = db.lookup_active_mail_domain("example.com").await.unwrap();
        assert_eq!(moved.unwrap().mta_sts_mode, "enforce");

        // Idempotent: a second pass finds nothing left to move.
        assert!(
            db.advance_mta_sts_testing_to_enforce_at(due)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn mta_sts_advance_window_restarts_at_a_restore_and_skips_removed_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let row = db
            .add_mail_domain(
                "extra.example",
                false,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        let past_window = row.added_at + MTA_STS_TESTING_WINDOW_MS;

        // Removed: not advanced, however old.
        db.soft_delete_mail_domain("extra.example").await.unwrap();
        assert!(
            db.advance_mta_sts_testing_to_enforce_at(past_window)
                .await
                .unwrap()
                .is_empty()
        );

        // Restored three days in: the window runs from the restore, not the add.
        let restored_at = row.added_at + 3 * 24 * 60 * 60 * 1000;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE mail_domains SET removed_at = NULL, restored_at = ?1
                 WHERE domain_name = 'extra.example'",
                [restored_at],
            )
            .unwrap();
        }
        assert!(
            db.advance_mta_sts_testing_to_enforce_at(past_window)
                .await
                .unwrap()
                .is_empty(),
            "seven days from the add is only four from the restore"
        );
        assert_eq!(
            db.advance_mta_sts_testing_to_enforce_at(restored_at + MTA_STS_TESTING_WINDOW_MS)
                .await
                .unwrap(),
            vec!["extra.example".to_string()]
        );
    }

    #[tokio::test]
    async fn set_role_address_override_merges_and_clears() {
        use fauna_mail::aliases::role_overrides::parse_stored;
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();

        let postmaster = "ab".repeat(32);
        let abuse = "11".repeat(32);

        // Set postmaster, then abuse — both must survive (atomic merge, not clobber).
        db.set_role_address_override("example.com", "postmaster", Some(postmaster.clone()))
            .await
            .unwrap();
        let row = db
            .set_role_address_override("example.com", "abuse", Some(abuse.clone()))
            .await
            .unwrap();
        let parsed = parse_stored(row.role_address_overrides_json.as_deref());
        assert_eq!(parsed.postmaster.as_deref(), Some(postmaster.as_str()));
        assert_eq!(parsed.abuse.as_deref(), Some(abuse.as_str()));

        // Clear postmaster only — abuse remains.
        let row = db
            .set_role_address_override("example.com", "postmaster", None)
            .await
            .unwrap();
        let parsed = parse_stored(row.role_address_overrides_json.as_deref());
        assert!(parsed.postmaster.is_none());
        assert_eq!(parsed.abuse.as_deref(), Some(abuse.as_str()));

        // Clear the last role — the column goes SQL NULL (canonical "no overrides").
        let row = db
            .set_role_address_override("example.com", "abuse", None)
            .await
            .unwrap();
        assert!(row.role_address_overrides_json.is_none());
    }

    #[tokio::test]
    async fn set_dmarc_policy_mode_merges_and_restores_default() {
        use fauna_mail::dmarc_publish::{DmarcMode, parse_stored_overrides};
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        // A pre-existing key the select does not own must survive.
        db.update_mail_domain_config(
            "example.com",
            MailDomainUpdate {
                dmarc_overrides_json: Some(Some(r#"{"pct":50}"#.into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let row = db
            .set_dmarc_policy_mode("Example.com", DmarcMode::None)
            .await
            .unwrap();
        let parsed = parse_stored_overrides(row.dmarc_overrides_json.as_deref());
        assert_eq!(parsed.policy_mode, Some(DmarcMode::None));
        assert_eq!(parsed.subdomain_policy_mode, Some(DmarcMode::None));
        assert_eq!(parsed.pct, Some(50));

        // Back to the default: both keys go, `pct` stays.
        let row = db
            .set_dmarc_policy_mode("example.com", DmarcMode::Reject)
            .await
            .unwrap();
        assert_eq!(row.dmarc_overrides_json.as_deref(), Some(r#"{"pct":50}"#));

        assert!(
            db.set_dmarc_policy_mode("never.heard.of.it", DmarcMode::None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn set_role_address_override_unknown_domain_errors() {
        let db = CacheDb::open_in_memory().unwrap();
        let err = db
            .set_role_address_override("never.heard.of.it", "postmaster", Some("ab".repeat(32)))
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn gc_destroys_expired_row_and_per_domain_dependents() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain(
            "primary.example",
            true,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.add_mail_domain(
            "gone.example",
            false,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();

        // Seed per-domain dependents for the to-be-removed domain (no cascade FK
        // exists, so the GC must delete these explicitly).
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO account_aliases
                    (alias_id, actor_id, local_domain, kind, pattern, created_at)
                 VALUES (?1, ?2, 'gone.example', 'exact', 'bob', 0)",
                rusqlite::params![&[1u8; 16][..], &[2u8; 32][..]],
            )
            .unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO mail_dkim_keys
                    (domain, selector, alg, key_wrapped, public_dns_value, created_at)
                 VALUES ('gone.example', 'default', 'ed25519', ?1, 'v=DKIM1; p=AAA', 0)",
                [&[9u8; 4][..]],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO bridge_tls_cert_blobs
                    (bridge_role, bridge_id, domain, blob, created_at)
                 VALUES ('mta', 'b1', 'gone.example', ?1, 0)",
                [&[9u8; 4][..]],
            )
            .unwrap();
        }

        db.soft_delete_mail_domain("gone.example").await.unwrap();

        // GC with a clock 30 days + 1s past the removal → the row is expired.
        let gc_now = now_epoch_millis() + SOFT_DELETE_RECOVERY_WINDOW_MS + 1_000;
        let removed = db
            .gc_expired_soft_deleted_mail_domains_at(gc_now)
            .await
            .unwrap();
        assert_eq!(removed, vec!["gone.example".to_string()]);

        // Row gone (not even as a soft-deleted record); primary untouched.
        assert!(
            db.lookup_mail_domain_by_name_any("gone.example")
                .await
                .unwrap()
                .is_none()
        );
        let active = db.list_active_mail_domains().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].domain_name, "primary.example");

        // All per-domain dependents destroyed.
        {
            let conn = db.conn.lock().await;
            let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
            assert_eq!(
                count("SELECT COUNT(*) FROM account_aliases WHERE local_domain='gone.example'"),
                0
            );
            assert_eq!(
                count("SELECT COUNT(*) FROM mail_dkim_keys WHERE domain='gone.example'"),
                0
            );
            assert_eq!(
                count("SELECT COUNT(*) FROM bridge_tls_cert_blobs WHERE domain='gone.example'"),
                0
            );
        }
    }

    #[tokio::test]
    async fn gc_spares_in_window_soft_deleted_row() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain(
            "primary.example",
            true,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.add_mail_domain(
            "recent.example",
            false,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.soft_delete_mail_domain("recent.example").await.unwrap();

        // Real-now GC → the just-removed row is well within the 30-day window.
        let removed = db.gc_expired_soft_deleted_mail_domains().await.unwrap();
        assert!(
            removed.is_empty(),
            "an in-window soft-deleted row must survive GC"
        );
        let soft = db
            .list_soft_deleted_within_30d_mail_domains()
            .await
            .unwrap();
        assert_eq!(soft.len(), 1);
        assert_eq!(soft[0].domain_name, "recent.example");
    }

    #[tokio::test]
    async fn gc_never_touches_active_rows_even_far_future() {
        let db = CacheDb::open_in_memory().unwrap();
        db.add_mail_domain(
            "primary.example",
            true,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.add_mail_domain(
            "active.example",
            false,
            "enforce",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();

        // Active (never-removed) rows have removed_at IS NULL → never expire, even
        // with a clock 10 windows in the future.
        let gc_now = now_epoch_millis() + 10 * SOFT_DELETE_RECOVERY_WINDOW_MS;
        let removed = db
            .gc_expired_soft_deleted_mail_domains_at(gc_now)
            .await
            .unwrap();
        assert!(removed.is_empty());
        assert_eq!(db.list_active_mail_domains().await.unwrap().len(), 2);
    }
}
