//! Bridge service-user enrollment: pubkey → role + status. Admin
//! pre-registers a (pubkey, role, bridge_id) row with status='pending'
//! via HTTP. The bridge connects with that pubkey; its first call to
//! `register_service_user` is treated as a self-attestation and waits
//! for admin approval (status flipped to 'approved' on the admin's
//! HTTP approve action). Approved bridges may call kinds permitted
//! by their role (see bridge_method_allowlist).

use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;

use super::{CacheDb, blob_to_array, now_epoch_millis, now_epoch_secs};

/// Returned by [`CacheDb::upsert_bridge_x25519`] when a bridge whose x25519
/// public key is already attested attempts to bind a *different* key. The
/// x25519 binding is **set-once**: a co-resident attacker holding the
/// bridge's ed25519 identity still cannot swap the sealing target after
/// enrollment. Callers downcast it at the RPC boundary
/// (`bridge_blob_handlers::register_service_user_handler`) to a
/// permission-denied. Mirrors the `migrations::SchemaIncompatible` marker idiom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("x25519 key already attested for this bridge; rebinding a different key is not permitted")]
pub struct BridgeX25519Frozen;

/// Returned by [`CacheDb::upsert_bridge_mlkem_ek`] when a bridge whose ML-KEM-768
/// encapsulation key is already published attempts to bind a *different* one
/// (PQ-CAP-2). Like [`BridgeX25519Frozen`], the ML-KEM ek binding is
/// **set-once**: it is derived deterministically from the bridge's Ed25519 seed
/// (`fauna.bridge.service-user-mlkem.v1`), so a genuine re-publish is
/// byte-identical (idempotent); a *changed* value can only come from a
/// compromised/co-resident bridge trying to redirect the ML-KEM half of future
/// X-Wing grant seals, so it is rejected — the post-quantum sibling of the
/// x25519 freeze. Callers downcast it at the RPC boundary
/// (`bridge_blob_handlers::register_service_user_handler`) to a
/// permission-denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("ML-KEM ek already published for this bridge; rebinding a different key is not permitted")]
pub struct BridgeMlkemEkFrozen;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeRole {
    Mta,
    Mda,
    /// A generic content-processing bridge (a scorer / FTS-indexer — the dual
    /// of the mail MDA), the holder class for user-minted capability grants
    /// (`fauna.capabilities.*`; design § Phase 2 Step 2 § 2.4). The *role* is
    /// only the coarse method-allowlist key ("may this bridge call
    /// `fauna.capabilities.fetch` at all"); the fine-grained boundary is the
    /// wrapped-key **scope**, which is cryptographically self-enforcing (§ 2.2).
    /// So one generic role admits every content-processor variant — the
    /// capability scope, not the role, distinguishes a spam-scorer from an
    /// FTS-indexer. (The MDA keeps its own `Mda` role *and* is a grant holder;
    /// fetch is holder-pubkey-scoped, not role-scoped, beyond this coarse gate.)
    ContentProcessor,
    /// The out-of-process ATProto **PDS** bridge — the "host" direction of
    /// Bluesky integration (Fauna *is* the user's Bluesky home server), on the
    /// mail-bridge model (`docs/goal/behavior/atproto-pds-bridge.md` §
    /// Architecture, role `atproto.pds`). It is the first **non-mail**
    /// out-of-process bridge role: it enrolls + `whoami`s + attests x25519 over
    /// the same lifecycle contract as the mail bridge, but is **never**
    /// auto-approved by the mail/DAV enable toggles — it always takes the manual
    /// admin approval card (`mail-bridge-lifecycle.md:169`, § Onboarding
    /// auto-approval, Scope). Its method reach is its own minimal `CallerClass`
    /// (the bridge-lifecycle RPCs only), not the mail bridge's DKIM/TLS/outbound
    /// surface.
    AtprotoPds,
}

impl BridgeRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            BridgeRole::Mta => "mta",
            BridgeRole::Mda => "mda",
            BridgeRole::ContentProcessor => "content-processor",
            BridgeRole::AtprotoPds => "atproto.pds",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mta" => Some(BridgeRole::Mta),
            "mda" => Some(BridgeRole::Mda),
            "content-processor" => Some(BridgeRole::ContentProcessor),
            "atproto.pds" => Some(BridgeRole::AtprotoPds),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeStatus {
    Pending,
    Approved,
    Revoked,
}

impl BridgeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            BridgeStatus::Pending => "pending",
            BridgeStatus::Approved => "approved",
            BridgeStatus::Revoked => "revoked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(BridgeStatus::Pending),
            "approved" => Some(BridgeStatus::Approved),
            "revoked" => Some(BridgeStatus::Revoked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BridgeServiceUser {
    pub ed25519_pubkey: [u8; 32],
    pub x25519_pubkey: Option<[u8; 32]>,
    pub role: BridgeRole,
    pub bridge_id: String,
    /// `true` for an **in-process** holder — its secret key material lives
    /// inside the nest process itself (today: the web-serve paywall holder,
    /// whose seed rests in the nest data dir). An in-process holder is a
    /// legitimate grant target for its own ratified readable class, but it
    /// must never be resolved as a seal target for content that must rest
    /// nest-opaque, and it can never answer a WS poke. Marked by the holder's
    /// own self-enrollment ([`CacheDb::mark_bridge_service_user_in_process`]);
    /// external enrollment paths never set it.
    pub in_process: bool,
    pub status: BridgeStatus,
    pub created_at: i64,
    pub approved_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub approved_by_actor_id: Option<[u8; 32]>,
    /// The bridge's last confinement self-probe, or `None` if it has never
    /// reported one (a pre-probe binary, or a row that has never connected).
    /// See [`BridgeConfinementRow`].
    pub confinement: Option<BridgeConfinementRow>,
}

/// What a bridge reported about **its own** sandbox at its last cold boot
/// (`security.md` § Co-resident process trust boundary → *Confinement
/// self-probe*). Stored so an admin — and a live e2e — can read a *deployed*
/// box's isolation facts over the wire instead of over SSH, which a provisioned
/// box has no key for (`testing.md` § Gap 3).
///
/// ⚠ **Provisioning diagnostics, never an attestation.** A compromised bridge
/// self-reports whatever it likes. Nothing may gate a security decision on
/// these; they catch the honest misconfiguration (a compose bypassing
/// `fauna-sandbox`, a Landlock-less kernel, a seccomp policy blocking the
/// landlock syscalls). The trust-bearing proof stays tier_4.
///
/// The strings are a small open vocabulary rather than an enum: additive
/// everywhere (`version-compatibility.md`) means a newer bridge may report a
/// state this build predates, and that must survive rather than be rejected.
/// Nest bounds them on admission (charset + length) and stores them verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfinementRow {
    /// `getuid()` of the bridge process; `-1` if the column was never written.
    pub uid: i64,
    /// The bridge's own attempted read of the sealed store:
    /// `denied` / `readable` / `absent` / `unknown`.
    pub sealed_store: String,
    /// Landlock enforcement relayed by the wrapper:
    /// `fully` / `partial` / `off` / `unknown`.
    pub landlock: String,
    /// `filter` / `strict` / `off` / `unknown`.
    pub seccomp: String,
    /// Epoch-millis this report was received.
    pub reported_at: i64,
}

impl CacheDb {
    /// Admin HTTP path: pre-register a bridge as pending. Idempotent
    /// on (ed25519_pubkey) — re-registering an existing pubkey is an
    /// error (use revoke + re-register, or update via approve).
    pub async fn create_pending_bridge_service_user(
        &self,
        ed25519_pubkey: &[u8; 32],
        role: BridgeRole,
        bridge_id: &str,
    ) -> Result<()> {
        let pk = *ed25519_pubkey;
        let bridge_id_owned = bridge_id.to_string();
        let now = now_epoch_millis();
        {
            let conn = self.conn.lock().await;
            conn.execute(
                "INSERT INTO bridge_service_users
                    (ed25519_pubkey, x25519_pubkey, role, bridge_id, status, created_at)
                 VALUES (?1, NULL, ?2, ?3, 'pending', ?4)",
                rusqlite::params![&pk[..], role.as_str(), bridge_id_owned, now],
            )
            .context("create pending bridge service user")?;
        }
        tracing::info!(
            target: "bridge_service_users",
            actor_prefix = hex::encode(&pk[..4]),
            role = role.as_str(),
            bridge_id,
            "bridge enrollment: pending"
        );
        Ok(())
    }

    /// Bridge self-attestation path: bind the x25519 key to an existing
    /// row (pending or approved). Returns the row's status so the
    /// register_service_user handler can shape the reply.
    ///
    /// **Set-once:** the binding is bound when unset, accepts an
    /// idempotent re-attestation of the *same* key, and rejects a *changed*
    /// key with [`BridgeX25519Frozen`] — so a compromised/co-resident bridge
    /// holding the ed25519 identity cannot swap the sealing target after
    /// enrollment. The guard lives in the `UPDATE` itself, so two racing
    /// registrations cannot both win.
    pub async fn upsert_bridge_x25519(
        &self,
        ed25519_pubkey: &[u8; 32],
        x25519_pubkey: &[u8; 32],
    ) -> Result<Option<BridgeStatus>> {
        let pk = *ed25519_pubkey;
        let xpk = *x25519_pubkey;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE bridge_service_users
                    SET x25519_pubkey = ?1
                  WHERE ed25519_pubkey = ?2
                    AND status != 'revoked'
                    AND (x25519_pubkey IS NULL OR x25519_pubkey = ?1)",
                rusqlite::params![&xpk[..], &pk[..]],
            )
            .context("upsert bridge x25519")?;
        if n == 0 {
            // No row updated: either there is no (non-revoked) enrollment row,
            // or one exists with a *different* x25519 already bound (frozen).
            // A NULL x25519 would have matched the guard above, so a surviving
            // non-revoked row here necessarily has a conflicting bound key.
            let row_exists = conn
                .query_row(
                    "SELECT 1 FROM bridge_service_users
                      WHERE ed25519_pubkey = ?1 AND status != 'revoked'",
                    rusqlite::params![&pk[..]],
                    |_| Ok(()),
                )
                .optional()
                .context("probe bridge row after no-op x25519 upsert")?
                .is_some();
            if row_exists {
                tracing::warn!(
                    target: "bridge_service_users",
                    actor_prefix = hex::encode(&pk[..4]),
                    "bridge x25519 rebind rejected: binding is frozen (set-once)"
                );
                return Err(anyhow::Error::new(BridgeX25519Frozen));
            }
            return Ok(None);
        }
        let status: String = conn
            .query_row(
                "SELECT status FROM bridge_service_users WHERE ed25519_pubkey = ?1",
                rusqlite::params![&pk[..]],
                |row| row.get(0),
            )
            .context("read bridge status after upsert")?;
        let status = BridgeStatus::parse(&status)
            .ok_or_else(|| anyhow!("unrecognized bridge status: {status}"))?;
        Ok(Some(status))
    }

    /// Bridge self-attestation path (PQ-CAP-2): publish the holder's 1184-byte
    /// ML-KEM-768 encapsulation key onto an existing (pending or approved) row,
    /// alongside its x25519 key. Stored so the client mint can seal capability
    /// grants X-Wing to `from_parts(mlkem_ek, x25519_pubkey)`.
    ///
    /// **Set-once**, mirroring [`upsert_bridge_x25519`]: binds when unset,
    /// accepts an idempotent re-publish of the *same* ek, rejects a *changed* ek
    /// with [`BridgeMlkemEkFrozen`]. The ek is seed-derived and deterministic, so
    /// a legitimate bridge always re-publishes the identical value; a changed
    /// value is a co-resident attacker trying to redirect the ML-KEM half of
    /// future X-Wing grant wraps, frozen exactly like the x25519 half. The guard
    /// lives in the `UPDATE` itself, so two racing publishes cannot both win.
    ///
    /// `mlkem_ek` length is validated by the caller (the handler gates it to
    /// `fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN` = 1184 before calling), which also
    /// confirms a non-revoked enrollment row exists via a prior successful
    /// [`upsert_bridge_x25519`] — so the no-row case never reaches production
    /// here. Returns `Ok(())` on bind / idempotent re-bind; [`BridgeMlkemEkFrozen`]
    /// on a changed key.
    pub async fn upsert_bridge_mlkem_ek(
        &self,
        ed25519_pubkey: &[u8; 32],
        mlkem_ek: &[u8],
    ) -> Result<()> {
        let pk = *ed25519_pubkey;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE bridge_service_users
                    SET mlkem_ek = ?1
                  WHERE ed25519_pubkey = ?2
                    AND status != 'revoked'
                    AND (mlkem_ek IS NULL OR mlkem_ek = ?1)",
                rusqlite::params![mlkem_ek, &pk[..]],
            )
            .context("upsert bridge mlkem_ek")?;
        if n == 0 {
            // No row updated. A NULL ek would have matched the guard above, so a
            // surviving non-revoked row here necessarily has a *conflicting* ek
            // bound (frozen). If no non-revoked row exists at all — impossible on
            // the production path, since the handler's x25519 upsert already
            // confirmed one — this is a benign no-op.
            let row_exists = conn
                .query_row(
                    "SELECT 1 FROM bridge_service_users
                      WHERE ed25519_pubkey = ?1 AND status != 'revoked'",
                    rusqlite::params![&pk[..]],
                    |_| Ok(()),
                )
                .optional()
                .context("probe bridge row after no-op mlkem_ek upsert")?
                .is_some();
            if row_exists {
                tracing::warn!(
                    target: "bridge_service_users",
                    actor_prefix = hex::encode(&pk[..4]),
                    "bridge mlkem_ek rebind rejected: binding is frozen (set-once)"
                );
                return Err(anyhow::Error::new(BridgeMlkemEkFrozen));
            }
        }
        Ok(())
    }

    /// Record a bridge's confinement self-probe on its enrollment row
    /// (`security.md` § Co-resident process trust boundary → *Confinement
    /// self-probe*). Called from the `register_service_user` handler, which has
    /// already bounded the strings.
    ///
    /// **Last-write-wins, deliberately** — the opposite of the set-once
    /// [`upsert_bridge_x25519`] / [`upsert_bridge_mlkem_ek`] beside it. Those
    /// freeze because a changed value means an attacker redirecting a seal
    /// target. This describes the *currently running* process, so each cold boot
    /// must overwrite the last: freezing it would pin a report from an image
    /// that is no longer deployed, which is precisely the stale-diagnostic
    /// failure the `reported_at` stamp exists to prevent. There is nothing to
    /// protect by freezing — a bridge that could lie on the second write could
    /// equally have lied on the first, which is why this is not an attestation.
    ///
    /// Revoked rows are skipped (they are excluded from every projection
    /// anyway). A missing row is a benign no-op: the handler's prior x25519
    /// upsert has already established one on the production path.
    pub async fn record_bridge_confinement(
        &self,
        ed25519_pubkey: &[u8; 32],
        c: &BridgeConfinementRow,
    ) -> Result<()> {
        let pk = *ed25519_pubkey;
        let (sealed_store, landlock, seccomp) = (
            c.sealed_store.clone(),
            c.landlock.clone(),
            c.seccomp.clone(),
        );
        let (uid, reported_at) = (c.uid, c.reported_at);
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE bridge_service_users
                SET confinement_uid = ?1,
                    confinement_sealed_store = ?2,
                    confinement_landlock = ?3,
                    confinement_seccomp = ?4,
                    confinement_reported_at = ?5
              WHERE ed25519_pubkey = ?6
                AND status != 'revoked'",
            rusqlite::params![uid, sealed_store, landlock, seccomp, reported_at, &pk[..]],
        )
        .context("record bridge confinement")?;
        Ok(())
    }

    /// Read a bridge service-user's published ML-KEM-768 encapsulation key
    /// (PQ-CAP-2), or `None` if the row is absent or the bridge is classical-only
    /// (no ek published). The client mint reads this (via the `fetch_bridge_pubkey`
    /// projection) to seal capability grants X-Wing to the holder; a `None`
    /// degrades the mint to the classical wrap.
    pub async fn bridge_mlkem_ek(&self, ed25519_pubkey: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let pk = *ed25519_pubkey;
        let conn = self.conn.lock().await;
        let ek = conn
            .query_row(
                "SELECT mlkem_ek FROM bridge_service_users WHERE ed25519_pubkey = ?1",
                rusqlite::params![&pk[..]],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()
            .context("read bridge mlkem_ek")?
            .flatten();
        Ok(ek)
    }

    /// Admin HTTP path: flip pending → approved, optionally recording
    /// approver's actor id. `None` is allowed for early test/scaffold
    /// paths; production callers (after T15) always pass `Some`.
    ///
    /// On a successful approve, an `INSERT OR IGNORE INTO users` row is
    /// added (in the same transaction) so the bridge's challenge-response
    /// auth (`fauna.auth.verify` → `auth_core::verify_core` → `db.get_user`)
    /// succeeds. Without
    /// this side-effect the bridge could never authenticate to nest
    /// post-approval (chicken-and-egg). The users row is audit-only for
    /// bridges — bridges are not persons — and uses the 'free' tier as a
    /// neutral placeholder; tier policy doesn't gate bridge RPC (the
    /// `bridge_service_users` row + `bridge_method_allowlist` does).
    pub async fn approve_bridge_service_user(
        &self,
        ed25519_pubkey: &[u8; 32],
        approved_by_actor_id: Option<&[u8; 32]>,
    ) -> Result<bool> {
        let pk = *ed25519_pubkey;
        let approver = approved_by_actor_id.copied();
        let now_ms = now_epoch_millis();
        let now_s = now_epoch_secs();
        let n = {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction().context("begin approve tx")?;
            let n = tx
                .execute(
                    "UPDATE bridge_service_users
                        SET status = 'approved',
                            approved_at = ?1,
                            approved_by_actor_id = ?2
                      WHERE ed25519_pubkey = ?3
                        AND status = 'pending'",
                    rusqlite::params![now_ms, approver.as_ref().map(|a| &a[..]), &pk[..]],
                )
                .context("approve bridge service user")?;
            if n > 0 {
                // Audit-only users row so challenge-response auth resolves.
                // INSERT OR IGNORE leaves any pre-existing row (with its
                // admin-set tier/label/handle) untouched.
                tx.execute(
                    "INSERT OR IGNORE INTO users (actor_id, tier, label, created_at)
                     VALUES (?1, 'free', '', ?2)",
                    rusqlite::params![&pk[..], now_s],
                )
                .context("insert users row for approved bridge")?;
            }
            tx.commit().context("commit approve tx")?;
            n
        };
        if n > 0 {
            tracing::info!(
                target: "bridge_service_users",
                actor_prefix = hex::encode(&pk[..4]),
                approver_prefix = ?approver.as_ref().map(|a| hex::encode(&a[..4])),
                "bridge enrollment: pending → approved"
            );
        }
        Ok(n > 0)
    }

    /// Admin HTTP path: revoke at any state (pending / approved →
    /// revoked). Returns whether the row existed and changed.
    pub async fn revoke_bridge_service_user(&self, ed25519_pubkey: &[u8; 32]) -> Result<bool> {
        let pk = *ed25519_pubkey;
        let now = now_epoch_millis();
        let n = {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE bridge_service_users
                    SET status = 'revoked', revoked_at = ?1
                  WHERE ed25519_pubkey = ?2
                    AND status != 'revoked'",
                rusqlite::params![now, &pk[..]],
            )
            .context("revoke bridge service user")?
        };
        if n > 0 {
            tracing::info!(
                target: "bridge_service_users",
                actor_prefix = hex::encode(&pk[..4]),
                "bridge enrollment: → revoked"
            );
        }
        Ok(n > 0)
    }

    pub async fn lookup_bridge_service_user(
        &self,
        ed25519_pubkey: &[u8; 32],
    ) -> Result<Option<BridgeServiceUser>> {
        let pk = *ed25519_pubkey;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                &format!(
                    "SELECT {BRIDGE_ROW_COLUMNS}
                   FROM bridge_service_users WHERE ed25519_pubkey = ?1"
                ),
                rusqlite::params![&pk[..]],
                row_to_tuple,
            )
            .optional()
            .context("lookup bridge service user")?;
        row.map(tuple_to_row).transpose()
    }

    pub async fn list_pending_bridge_service_users(&self) -> Result<Vec<BridgeServiceUser>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {BRIDGE_ROW_COLUMNS}
                   FROM bridge_service_users
                  WHERE status = 'pending'
                  ORDER BY created_at ASC"
            ))
            .context("prepare list pending bridge service users")?;
        let rows = stmt
            .query_map([], row_to_tuple)
            .context("query list pending bridge service users")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list pending bridge service users")?;
        rows.into_iter().map(tuple_to_row).collect()
    }

    /// All bridge service users with `status='approved'`. Used by
    /// `SealedStorage::store_acme_material` to fan-out wrapped TLS-cert
    /// blobs to every registered bridge that has an x25519 pubkey.
    pub async fn list_approved_bridge_service_users(
        &self,
    ) -> anyhow::Result<Vec<BridgeServiceUser>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {BRIDGE_ROW_COLUMNS}
                   FROM bridge_service_users
                  WHERE status = 'approved'
                  ORDER BY created_at ASC"
            ))
            .context("prepare list approved bridge service users")?;
        let rows = stmt
            .query_map([], row_to_tuple)
            .context("query list approved bridge service users")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list approved bridge service users")?;
        rows.into_iter().map(tuple_to_row).collect()
    }

    /// Enumerate bridge service-users, optionally filtered by role and/or
    /// status. The table is small (a handful of bridges), so filtering in
    /// Rust keeps the query simple. Public metadata only — the caller-facing
    /// projection (`fauna.bridges.list_service_users`) never exposes secrets.
    pub async fn list_bridge_service_users(
        &self,
        role: Option<BridgeRole>,
        status: Option<BridgeStatus>,
    ) -> Result<Vec<BridgeServiceUser>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {BRIDGE_ROW_COLUMNS}
                   FROM bridge_service_users
                  ORDER BY created_at ASC"
            ))
            .context("prepare list bridge service users")?;
        let rows = stmt
            .query_map([], row_to_tuple)
            .context("query list bridge service users")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list bridge service users")?;
        let all: Result<Vec<BridgeServiceUser>> = rows.into_iter().map(tuple_to_row).collect();
        Ok(all?
            .into_iter()
            .filter(|u| role.as_ref().is_none_or(|r| &u.role == r))
            .filter(|u| status.as_ref().is_none_or(|s| &u.status == s))
            .collect())
    }

    /// Mark an enrollment row as an **in-process** holder (see
    /// [`BridgeServiceUser::in_process`]). Called by the in-process holder's
    /// own boot-time self-enrollment (`web_content::holder::WebServeHolder::init`),
    /// keyed on its actual pubkey — never on `bridge_id` — and on every boot,
    /// so the holder's freshly created enrollment row is marked (this is the
    /// only writer of `in_process` for it). Idempotent; a revoked row is marked too (harmless — revoked
    /// rows are excluded from every resolution already).
    pub async fn mark_bridge_service_user_in_process(
        &self,
        ed25519_pubkey: &[u8; 32],
    ) -> Result<()> {
        let pk = *ed25519_pubkey;
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE bridge_service_users SET in_process = 1 WHERE ed25519_pubkey = ?1",
            rusqlite::params![&pk[..]],
        )
        .context("mark bridge service user in-process")?;
        Ok(())
    }

    pub async fn find_approved_bridge_by_role_and_id(
        &self,
        role: BridgeRole,
        bridge_id: &str,
    ) -> Result<Option<BridgeServiceUser>> {
        let bridge_id = bridge_id.to_string();
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                &format!(
                    "SELECT {BRIDGE_ROW_COLUMNS}
                   FROM bridge_service_users
                  WHERE role = ?1 AND bridge_id = ?2 AND status = 'approved'
                  LIMIT 1"
                ),
                rusqlite::params![role.as_str(), bridge_id],
                row_to_tuple,
            )
            .optional()
            .context("find approved bridge by role and id")?;
        row.map(tuple_to_row).transpose()
    }
}

// ── Row decoding helpers ─────────────────────────────────────

/// The column list every `bridge_service_users` row read selects. Single source
/// so a new column is added in ONE place; the decoder below reads **by name**,
/// so neither the order here nor the count can silently desynchronize a reader
/// (the previous positional 10-tuple made every addition a five-site edit where
/// one missed site compiled fine and returned the wrong column).
const BRIDGE_ROW_COLUMNS: &str = "ed25519_pubkey, x25519_pubkey, role, bridge_id, in_process, \
     status, created_at, approved_at, revoked_at, approved_by_actor_id, \
     confinement_uid, confinement_sealed_store, confinement_landlock, \
     confinement_seccomp, confinement_reported_at";

/// Owned column values for one row, decoded by name.
struct RowValues {
    ed25519_pubkey: Vec<u8>,
    x25519_pubkey: Option<Vec<u8>>,
    role: String,
    bridge_id: String,
    in_process: bool,
    status: String,
    created_at: i64,
    approved_at: Option<i64>,
    revoked_at: Option<i64>,
    approved_by_actor_id: Option<Vec<u8>>,
    confinement_uid: Option<i64>,
    confinement_sealed_store: Option<String>,
    confinement_landlock: Option<String>,
    confinement_seccomp: Option<String>,
    confinement_reported_at: Option<i64>,
}

fn row_to_tuple(row: &rusqlite::Row<'_>) -> rusqlite::Result<RowValues> {
    Ok(RowValues {
        ed25519_pubkey: row.get("ed25519_pubkey")?,
        x25519_pubkey: row.get("x25519_pubkey")?,
        role: row.get("role")?,
        bridge_id: row.get("bridge_id")?,
        in_process: row.get("in_process")?,
        status: row.get("status")?,
        created_at: row.get("created_at")?,
        approved_at: row.get("approved_at")?,
        revoked_at: row.get("revoked_at")?,
        approved_by_actor_id: row.get("approved_by_actor_id")?,
        confinement_uid: row.get("confinement_uid")?,
        confinement_sealed_store: row.get("confinement_sealed_store")?,
        confinement_landlock: row.get("confinement_landlock")?,
        confinement_seccomp: row.get("confinement_seccomp")?,
        confinement_reported_at: row.get("confinement_reported_at")?,
    })
}

fn tuple_to_row(t: RowValues) -> Result<BridgeServiceUser> {
    let RowValues {
        ed25519_pubkey: pk_v,
        x25519_pubkey: xpk_v,
        role: role_s,
        bridge_id,
        in_process,
        status: status_s,
        created_at,
        approved_at,
        revoked_at,
        approved_by_actor_id: approver_v,
        confinement_uid,
        confinement_sealed_store,
        confinement_landlock,
        confinement_seccomp,
        confinement_reported_at,
    } = t;
    // A confinement report is present only once a bridge running a probing
    // build has connected. Any one of the columns being set is enough to
    // reconstruct it — the missing halves degrade to `unknown` rather than
    // suppressing the whole report, so a partially-written row still tells the
    // admin what it does know.
    let confinement = (confinement_reported_at.is_some()
        || confinement_sealed_store.is_some()
        || confinement_landlock.is_some())
    .then(|| BridgeConfinementRow {
        uid: confinement_uid.unwrap_or(-1),
        sealed_store: confinement_sealed_store.unwrap_or_else(|| "unknown".into()),
        landlock: confinement_landlock.unwrap_or_else(|| "unknown".into()),
        seccomp: confinement_seccomp.unwrap_or_else(|| "unknown".into()),
        reported_at: confinement_reported_at.unwrap_or(0),
    });
    let pk_arr: [u8; 32] = blob_to_array(pk_v.as_slice(), "ed25519_pubkey")?;
    let xpk_arr = xpk_v
        .map(|v| blob_to_array(v.as_slice(), "x25519_pubkey"))
        .transpose()?;
    let approver_arr = approver_v
        .map(|v| blob_to_array(v.as_slice(), "approved_by_actor_id"))
        .transpose()?;
    Ok(BridgeServiceUser {
        ed25519_pubkey: pk_arr,
        x25519_pubkey: xpk_arr,
        role: BridgeRole::parse(&role_s).ok_or_else(|| anyhow!("invalid role: {role_s}"))?,
        bridge_id,
        in_process,
        status: BridgeStatus::parse(&status_s)
            .ok_or_else(|| anyhow!("invalid status: {status_s}"))?,
        created_at,
        approved_at,
        revoked_at,
        approved_by_actor_id: approver_arr,
        confinement,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every enrollment path creates rows with `in_process = 0`; only the
    /// explicit self-mark flips it, idempotently, keyed on the pubkey.
    #[tokio::test]
    async fn in_process_defaults_false_and_marks_idempotently() {
        let db = CacheDb::open_in_memory().unwrap();
        let holder = [8u8; 32];
        let external = [9u8; 32];
        db.create_pending_bridge_service_user(&holder, BridgeRole::ContentProcessor, "web-serve")
            .await
            .unwrap();
        db.create_pending_bridge_service_user(&external, BridgeRole::ContentProcessor, "cp-1")
            .await
            .unwrap();
        for pk in [&holder, &external] {
            assert!(
                !db.lookup_bridge_service_user(pk)
                    .await
                    .unwrap()
                    .unwrap()
                    .in_process,
                "enrollment never sets in_process — only the self-mark does"
            );
        }

        db.mark_bridge_service_user_in_process(&holder)
            .await
            .unwrap();
        db.mark_bridge_service_user_in_process(&holder)
            .await
            .unwrap(); // idempotent
        assert!(
            db.lookup_bridge_service_user(&holder)
                .await
                .unwrap()
                .unwrap()
                .in_process
        );
        assert!(
            !db.lookup_bridge_service_user(&external)
                .await
                .unwrap()
                .unwrap()
                .in_process,
            "the mark is pubkey-keyed — the sibling row is untouched"
        );
    }

    #[tokio::test]
    async fn pending_then_approve_then_revoke_round_trip() {
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [7u8; 32];

        // Set up an admin actor_id (FK target).
        let admin_actor = [1u8; 32];
        let admin_for_db = admin_actor;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO admin_actor_ids (actor_id, added_at) VALUES (?1, 0)",
                rusqlite::params![&admin_for_db[..]],
            )
            .unwrap();
        }

        db.create_pending_bridge_service_user(&pk, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();

        let row = db.lookup_bridge_service_user(&pk).await.unwrap().unwrap();
        assert_eq!(row.status, BridgeStatus::Pending);
        assert_eq!(row.role, BridgeRole::Mta);
        assert_eq!(row.bridge_id, "mta-1");

        let pending = db.list_pending_bridge_service_users().await.unwrap();
        assert_eq!(pending.len(), 1);

        assert!(
            db.approve_bridge_service_user(&pk, Some(&admin_actor))
                .await
                .unwrap()
        );
        let row = db.lookup_bridge_service_user(&pk).await.unwrap().unwrap();
        assert_eq!(row.status, BridgeStatus::Approved);
        assert!(row.approved_at.is_some());
        assert_eq!(row.approved_by_actor_id, Some(admin_actor));

        // Idempotency: approving an already-approved row is a no-op.
        assert!(
            !db.approve_bridge_service_user(&pk, Some(&admin_actor))
                .await
                .unwrap()
        );

        assert!(db.revoke_bridge_service_user(&pk).await.unwrap());
        let row = db.lookup_bridge_service_user(&pk).await.unwrap().unwrap();
        assert_eq!(row.status, BridgeStatus::Revoked);
        assert!(row.revoked_at.is_some());
    }

    #[tokio::test]
    async fn duplicate_pubkey_rejected() {
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [9u8; 32];
        db.create_pending_bridge_service_user(&pk, BridgeRole::Mda, "mda-1")
            .await
            .unwrap();
        let err = db
            .create_pending_bridge_service_user(&pk, BridgeRole::Mta, "other")
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn upsert_x25519_records_pubkey_and_returns_status() {
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [3u8; 32];
        let xpk = [4u8; 32];
        db.create_pending_bridge_service_user(&pk, BridgeRole::Mta, "b1")
            .await
            .unwrap();
        let status = db.upsert_bridge_x25519(&pk, &xpk).await.unwrap();
        assert_eq!(status, Some(BridgeStatus::Pending));
        let row = db.lookup_bridge_service_user(&pk).await.unwrap().unwrap();
        assert_eq!(row.x25519_pubkey, Some(xpk));
    }

    #[tokio::test]
    async fn upsert_x25519_unknown_returns_none() {
        let db = CacheDb::open_in_memory().unwrap();
        let unknown = [99u8; 32];
        let status = db.upsert_bridge_x25519(&unknown, &[1u8; 32]).await.unwrap();
        assert_eq!(status, None);
    }

    #[tokio::test]
    async fn upsert_x25519_is_set_once_freeze() {
        // The x25519 binding is set-once. The first attestation binds it;
        // re-attesting the *same* key is idempotent; attesting a *different*
        // key is rejected — a compromised/co-resident bridge cannot swap the
        // sealing target after enrollment — and the bound key is unchanged.
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [7u8; 32];
        db.create_pending_bridge_service_user(&pk, BridgeRole::Mta, "b1")
            .await
            .unwrap();

        // First attestation: NULL -> [4; 32] binds and reports status.
        let first = db.upsert_bridge_x25519(&pk, &[4u8; 32]).await.unwrap();
        assert_eq!(first, Some(BridgeStatus::Pending));

        // Idempotent re-attestation of the same key succeeds.
        let again = db.upsert_bridge_x25519(&pk, &[4u8; 32]).await.unwrap();
        assert_eq!(again, Some(BridgeStatus::Pending));

        // A *different* key is frozen out.
        let res = db.upsert_bridge_x25519(&pk, &[5u8; 32]).await;
        assert!(
            res.is_err(),
            "rebinding a different x25519 to an attested bridge must be rejected"
        );
        assert!(
            res.unwrap_err()
                .downcast_ref::<BridgeX25519Frozen>()
                .is_some(),
            "the rebind rejection must be the BridgeX25519Frozen marker error"
        );

        // The originally-bound key is unchanged.
        let row = db.lookup_bridge_service_user(&pk).await.unwrap().unwrap();
        assert_eq!(row.x25519_pubkey, Some([4u8; 32]));
    }

    #[tokio::test]
    async fn upsert_mlkem_ek_records_and_is_set_once_freeze() {
        // PQ-CAP-2: the ML-KEM ek publish binds when unset, is idempotent for the
        // same (seed-derived, deterministic) value, and freezes a *changed* value
        // — the post-quantum sibling of the x25519 set-once freeze.
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [7u8; 32];
        db.create_pending_bridge_service_user(&pk, BridgeRole::Mda, "mda-1")
            .await
            .unwrap();
        // Precondition mirroring production: x25519 attested first.
        db.upsert_bridge_x25519(&pk, &[4u8; 32]).await.unwrap();

        let ek_a = vec![0xAAu8; 1184];
        let ek_b = vec![0xBBu8; 1184];

        // First publish binds; idempotent re-publish of the same ek succeeds.
        db.upsert_bridge_mlkem_ek(&pk, &ek_a).await.unwrap();
        db.upsert_bridge_mlkem_ek(&pk, &ek_a).await.unwrap();

        // A *different* ek is frozen out with the typed marker.
        let res = db.upsert_bridge_mlkem_ek(&pk, &ek_b).await;
        assert!(
            res.is_err(),
            "rebinding a different mlkem_ek must be rejected"
        );
        assert!(
            res.unwrap_err()
                .downcast_ref::<BridgeMlkemEkFrozen>()
                .is_some(),
            "the rebind rejection must be the BridgeMlkemEkFrozen marker"
        );

        // The originally-bound ek is unchanged.
        let stored: Option<Vec<u8>> = {
            let conn = db.conn.lock().await;
            conn.query_row(
                "SELECT mlkem_ek FROM bridge_service_users WHERE ed25519_pubkey = ?1",
                rusqlite::params![&pk[..]],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(stored, Some(ek_a));
    }

    #[tokio::test]
    async fn upsert_mlkem_ek_rejected_on_revoked_bridge() {
        // A revoked bridge cannot publish an ML-KEM ek (status gate) — surfaced
        // as the frozen marker, same as the x25519 path treats a locked row.
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [8u8; 32];
        db.create_pending_bridge_service_user(&pk, BridgeRole::Mda, "mda-revoked")
            .await
            .unwrap();
        db.upsert_bridge_x25519(&pk, &[4u8; 32]).await.unwrap();
        db.revoke_bridge_service_user(&pk).await.unwrap();
        // No non-revoked row exists → no-op, not an error (the handler never
        // reaches this because a revoked bridge fails the x25519 upsert first).
        db.upsert_bridge_mlkem_ek(&pk, &vec![0xAAu8; 1184])
            .await
            .unwrap();
        // The revoked row's ek stays NULL (nothing was written).
        let stored: Option<Vec<u8>> = {
            let conn = db.conn.lock().await;
            conn.query_row(
                "SELECT mlkem_ek FROM bridge_service_users WHERE ed25519_pubkey = ?1",
                rusqlite::params![&pk[..]],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(stored, None);
    }

    #[tokio::test]
    async fn revoke_pending_bridge_service_user_works() {
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [55u8; 32];
        db.create_pending_bridge_service_user(&pk, BridgeRole::Mta, "mta-pending")
            .await
            .unwrap();
        assert!(db.revoke_bridge_service_user(&pk).await.unwrap());
        let row = db.lookup_bridge_service_user(&pk).await.unwrap().unwrap();
        assert_eq!(row.status, BridgeStatus::Revoked);
        assert!(row.revoked_at.is_some());
        assert!(row.approved_at.is_none());
    }

    #[tokio::test]
    async fn approve_revoked_bridge_service_user_is_noop() {
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [66u8; 32];
        db.create_pending_bridge_service_user(&pk, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();
        assert!(db.revoke_bridge_service_user(&pk).await.unwrap());
        // Approve on revoked → no-op (returns false), status stays revoked.
        assert!(!db.approve_bridge_service_user(&pk, None).await.unwrap());
        let row = db.lookup_bridge_service_user(&pk).await.unwrap().unwrap();
        assert_eq!(row.status, BridgeStatus::Revoked);
    }

    #[tokio::test]
    async fn find_approved_by_role_and_id_returns_only_approved() {
        let db = CacheDb::open_in_memory().unwrap();
        let pk_pending = [10u8; 32];
        let pk_approved = [20u8; 32];
        db.create_pending_bridge_service_user(&pk_pending, BridgeRole::Mta, "mta-pending")
            .await
            .unwrap();
        db.create_pending_bridge_service_user(&pk_approved, BridgeRole::Mta, "mta-approved")
            .await
            .unwrap();
        db.upsert_bridge_x25519(&pk_approved, &[1u8; 32])
            .await
            .unwrap();
        db.approve_bridge_service_user(&pk_approved, None)
            .await
            .unwrap();

        // Pending isn't returned.
        let none = db
            .find_approved_bridge_by_role_and_id(BridgeRole::Mta, "mta-pending")
            .await
            .unwrap();
        assert!(none.is_none());

        // Approved is returned.
        let some = db
            .find_approved_bridge_by_role_and_id(BridgeRole::Mta, "mta-approved")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(some.ed25519_pubkey, pk_approved);
        assert_eq!(some.bridge_id, "mta-approved");

        // Wrong role doesn't match.
        let none2 = db
            .find_approved_bridge_by_role_and_id(BridgeRole::Mda, "mta-approved")
            .await
            .unwrap();
        assert!(none2.is_none());
    }

    #[tokio::test]
    async fn list_approved_returns_only_approved_with_correct_pubkeys() {
        let db = CacheDb::open_in_memory().unwrap();

        // Three bridges: two approved with x25519 keys, one pending (must be excluded).
        let mta_pk = [0x10u8; 32];
        let mda_pk = [0x20u8; 32];
        let pending_pk = [0x30u8; 32];

        let mta_x25519 = [0xAAu8; 32];
        let mda_x25519 = [0xBBu8; 32];

        db.create_pending_bridge_service_user(&mta_pk, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();
        db.create_pending_bridge_service_user(&mda_pk, BridgeRole::Mda, "mda-1")
            .await
            .unwrap();
        db.create_pending_bridge_service_user(&pending_pk, BridgeRole::Mta, "mta-pending")
            .await
            .unwrap();

        db.upsert_bridge_x25519(&mta_pk, &mta_x25519).await.unwrap();
        db.upsert_bridge_x25519(&mda_pk, &mda_x25519).await.unwrap();

        db.approve_bridge_service_user(&mta_pk, None).await.unwrap();
        db.approve_bridge_service_user(&mda_pk, None).await.unwrap();
        // pending_pk stays pending

        let approved = db.list_approved_bridge_service_users().await.unwrap();
        assert_eq!(approved.len(), 2, "only approved bridges returned");

        // Both should have their x25519 pubkeys.
        let mta_row = approved
            .iter()
            .find(|r| r.bridge_id == "mta-1")
            .expect("mta-1 must be in result");
        assert_eq!(mta_row.role, BridgeRole::Mta);
        assert_eq!(mta_row.x25519_pubkey, Some(mta_x25519));
        assert_eq!(mta_row.status, BridgeStatus::Approved);

        let mda_row = approved
            .iter()
            .find(|r| r.bridge_id == "mda-1")
            .expect("mda-1 must be in result");
        assert_eq!(mda_row.role, BridgeRole::Mda);
        assert_eq!(mda_row.x25519_pubkey, Some(mda_x25519));

        // The pending bridge must not appear.
        assert!(
            !approved.iter().any(|r| r.bridge_id == "mta-pending"),
            "pending bridge must not appear in list_approved"
        );
    }

    #[tokio::test]
    async fn approve_inserts_users_row_for_challenge_auth() {
        // After admin approve, the bridge's challenge-response auth
        // (fauna.auth.verify → auth_core::verify_core → db.get_user) must
        // succeed. Without
        // the users-row side-effect of approve, the bridge cannot
        // authenticate to nest post-approval — chicken-and-egg break.
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [0x7Fu8; 32];

        db.create_pending_bridge_service_user(&pk, BridgeRole::Mta, "mta-auth-test")
            .await
            .unwrap();

        // Pre-condition: no users row yet.
        assert!(
            db.get_user(&pk).await.unwrap().is_none(),
            "users row must not exist before approve"
        );

        assert!(
            db.approve_bridge_service_user(&pk, None).await.unwrap(),
            "approve should report change"
        );

        // Post-condition: get_user returns Some so challenge-auth succeeds.
        let user = db
            .get_user(&pk)
            .await
            .unwrap()
            .expect("approve must insert users row");
        assert_eq!(&user.actor_id[..], &pk[..]);
        assert_eq!(user.tier, "free");
        assert!(!user.suspended);
    }

    #[tokio::test]
    async fn approve_users_row_insert_is_idempotent() {
        // If a users row already exists for the bridge pubkey (e.g.,
        // admin pre-registered the actor), the INSERT OR IGNORE side
        // of approve must not error or clobber existing tier/label.
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [0x5Bu8; 32];

        db.create_user(&pk, "personal", "pre-existing")
            .await
            .unwrap();

        db.create_pending_bridge_service_user(&pk, BridgeRole::Mda, "mda-idem")
            .await
            .unwrap();

        assert!(db.approve_bridge_service_user(&pk, None).await.unwrap());

        // Existing row preserved (tier/label not overwritten by INSERT OR IGNORE).
        let user = db.get_user(&pk).await.unwrap().expect("users row");
        assert_eq!(user.tier, "personal");
        assert_eq!(user.label, "pre-existing");
    }

    #[tokio::test]
    async fn approve_noop_does_not_insert_users_row() {
        // Approving a non-pending bridge (e.g., revoked) is a no-op
        // and must NOT insert a users row — only successful approves
        // promote the bridge to authenticatable status.
        let db = CacheDb::open_in_memory().unwrap();
        let pk = [0x4Eu8; 32];

        db.create_pending_bridge_service_user(&pk, BridgeRole::Mta, "mta-revoked")
            .await
            .unwrap();
        db.revoke_bridge_service_user(&pk).await.unwrap();

        // Approve on revoked → false, no users row created.
        assert!(!db.approve_bridge_service_user(&pk, None).await.unwrap());
        assert!(
            db.get_user(&pk).await.unwrap().is_none(),
            "no-op approve must not insert users row"
        );
    }

    #[tokio::test]
    async fn list_pending_filters_and_orders() {
        let db = CacheDb::open_in_memory().unwrap();
        let pending_a = [0xAAu8; 32];
        let pending_b = [0xBBu8; 32];
        let approved = [0xCCu8; 32];
        let revoked = [0xDDu8; 32];
        let admin = [1u8; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO admin_actor_ids (actor_id, added_at) VALUES (?1, 0)",
                rusqlite::params![&admin[..]],
            )
            .unwrap();
        }
        // Insert in deliberate order; created_at advances strictly.
        db.create_pending_bridge_service_user(&pending_a, BridgeRole::Mta, "a")
            .await
            .unwrap();
        // tiny sleep to let now_epoch_millis advance
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        db.create_pending_bridge_service_user(&approved, BridgeRole::Mta, "approved")
            .await
            .unwrap();
        db.approve_bridge_service_user(&approved, Some(&admin))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        db.create_pending_bridge_service_user(&revoked, BridgeRole::Mda, "revoked")
            .await
            .unwrap();
        db.revoke_bridge_service_user(&revoked).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        db.create_pending_bridge_service_user(&pending_b, BridgeRole::Mda, "b")
            .await
            .unwrap();

        let pending = db.list_pending_bridge_service_users().await.unwrap();
        assert_eq!(pending.len(), 2);
        // ASC by created_at: pending_a came first.
        assert_eq!(pending[0].ed25519_pubkey, pending_a);
        assert_eq!(pending[1].ed25519_pubkey, pending_b);
    }
}
