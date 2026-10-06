//! Client-set `[nest]`-policy singletons — the DB half of the deployment-wide
//! auth/discovery toggles an admin chooses from a client (a product
//! invariant: nest config a user/admin picks comes from clients, not CLI/env/
//! hand-edited files; the CLI/env value is the pre-claim *seed* only).
//!
//! Three settable-both-ways single-row singletons, mirroring [`super::mail_enable`]
//! (which hosts the `mail_enabled` + `mail_auto_enable_new_users` toggles in one
//! file) and the mutable [`super::nest_nat_mode`] axis:
//!
//! - `nest_subhandles` — whether the nest advertises the `handle@domain` /
//!   `@handle.domain` subhandle address forms (`account_core`, `discovery_core`
//!   readers). Absent ⇒ the `config.nest.subhandles` seed (default `false`) wins.
//! - `nest_cors_origins` — the deployment-wide CORS allow-list of trusted browser
//!   origins for nest's own HTTP API (the `lib.rs` `serve` reader). The list
//!   member: the whole `Vec<String>` is one JSON blob, so an absent row ⇒ the
//!   `config.nest.cors_origins` seed wins, a present row (even `[]`) ⇒ the
//!   client-set list wins. Row-presence carries the unset/set-empty distinction.
//! - `nest_max_storage_bytes` — the node-wide storage/capacity cap (`router_status`
//!   reader). The int member: `max_bytes` is nullable, so the row carries a
//!   three-state — absent (⇒ the `config.nest.max_storage_bytes` seed wins),
//!   present-`NULL` (⇒ an explicitly-cleared cap, no limit), present-value (⇒ the
//!   cap). Outer/inner `Option` on `get_max_storage_bytes` mirrors this.
//!
//! All three flip freely for the life of the deployment
//! (`INSERT … ON CONFLICT(id) DO UPDATE`); the single upsert is the atomic
//! decision point that keeps the toggle crash-safe
//! (common.md § Client-state recoverability). The admin RPC
//! (`fauna.admin.set_{registration_mode,subhandles}`) upserts; `start_server`
//! boot-resolves the row into the live `AppState` RwLock field; `setup_status`
//! reads it back for the admin client.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

impl crate::db::CacheDb {
    // NOTE: `get/set_require_registration` are **gone**. The boolean they backed
    // conflated "may an unknown actor self-provision?" (now permanently no — the
    // auto-provision branch is deleted) with "does this actor's tier cap apply?"
    // (now `AppState::enforce_tier_quotas`, artifact-set IPC with no DB row). Its
    // table left at schema 99 (`migrations::retire_dead_tables`), and
    // `fauna.admin.set_require_registration` left the wire 2026-09-24 with the
    // compat-remnant sweep (it never had a caller).

    /// Read the client-set registration posture — the mode plus the orthogonal
    /// free-tier ceiling — or `None` if the admin has never set one (the caller
    /// falls back to the `config.nest.registration_mode` seed).
    ///
    /// An **unparseable** stored mode (a row written by a newer binary that knows
    /// a mode this one does not) resolves to `Closed`, never to an open posture:
    /// an unknown registration mode must fail *shut*. Logged loudly — it means a
    /// downgrade, and the admin's real choice is preserved in the row for when the
    /// newer binary comes back.
    pub async fn get_registration_mode(
        &self,
    ) -> Result<Option<(fauna_protocol::node_policy::RegistrationMode, Option<u64>)>> {
        let conn = self.conn.lock().await;
        let raw: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT mode, max_free_users FROM nest_registration_mode WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("get nest_registration_mode")?;
        Ok(raw.map(|(mode, cap)| {
            let parsed = fauna_protocol::node_policy::RegistrationMode::from_wire_str(&mode)
                .unwrap_or_else(|| {
                    tracing::error!(
                        "nest_registration_mode holds unknown mode {mode:?} (written by a newer \
                         binary?); failing SHUT to `closed` — the row is left intact"
                    );
                    fauna_protocol::node_policy::RegistrationMode::Closed
                });
            (parsed, cap.map(|c| c as u64))
        }))
    }

    /// Upsert the deployment-wide registration posture (settable freely; the
    /// latest write wins). The mode and the free-tier ceiling are written
    /// together — they are one admin decision (one Save on `admin-users`).
    pub async fn set_registration_mode(
        &self,
        mode: fauna_protocol::node_policy::RegistrationMode,
        max_free_users: Option<u64>,
    ) -> Result<()> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO nest_registration_mode (id, mode, max_free_users, set_at)
             VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET mode = ?1, max_free_users = ?2, set_at = ?3",
            rusqlite::params![mode.as_wire_str(), max_free_users.map(|c| c as i64), now],
        )
        .context("upsert nest_registration_mode")?;
        Ok(())
    }

    /// Read the client-set `subhandles` policy, or `None` if the admin has never
    /// set it (the caller falls back to the `config.nest.subhandles` seed).
    pub async fn get_subhandles(&self) -> Result<Option<bool>> {
        let raw: Option<i64> = self
            .get_singleton_column("nest_subhandles", "enabled")
            .await?;
        Ok(raw.map(|v| v != 0))
    }

    /// Upsert the deployment-wide `subhandles` toggle (settable both ways; the
    /// latest write wins).
    pub async fn set_subhandles(&self, enabled: bool) -> Result<()> {
        self.set_singleton_column("nest_subhandles", "enabled", enabled as i64)
            .await
    }

    /// Read the client-set "accept only signups carrying app age verification"
    /// gate (`family-safety.md` § The account age band D5+D6), or `None` if the
    /// admin has never set it. Unlike `subhandles` there is deliberately **no
    /// config seed to fall back to** — the hard-coded default is off, so the
    /// caller folds `None` to `false` (`node_policy_core::resolve_age_verification_required`).
    pub async fn get_age_verification_required(&self) -> Result<Option<bool>> {
        let raw: Option<i64> = self
            .get_singleton_column("nest_age_verification_required", "enabled")
            .await?;
        Ok(raw.map(|v| v != 0))
    }

    /// Upsert the deployment-wide age-verification gate (settable both ways;
    /// the latest write wins).
    pub async fn set_age_verification_required(&self, enabled: bool) -> Result<()> {
        self.set_singleton_column("nest_age_verification_required", "enabled", enabled as i64)
            .await
    }

    /// Read the client-set node-wide storage cap. The **outer** `Option` is row
    /// presence (`None` ⇒ the admin never set it ⇒ the caller falls back to the
    /// `config.nest.max_storage_bytes` seed); the **inner** `Option` is the value
    /// (`None` ⇒ an admin who explicitly cleared the cap ⇒ no limit, which still
    /// wins over a non-`None` seed). The nullable `max_bytes` column carries the
    /// inner option directly.
    pub async fn get_max_storage_bytes(&self) -> Result<Option<Option<u64>>> {
        let raw: Option<Option<i64>> = self
            .get_singleton_column("nest_max_storage_bytes", "max_bytes")
            .await?;
        Ok(raw.map(|inner| inner.map(|v| v as u64)))
    }

    /// Upsert the deployment-wide storage cap (settable both ways; the latest
    /// write wins). `Some(v)` caps at `v` bytes; `None` writes a present row with
    /// `NULL` — an explicitly-cleared cap, distinct from an absent row.
    pub async fn set_max_storage_bytes(&self, max_bytes: Option<u64>) -> Result<()> {
        self.set_singleton_column(
            "nest_max_storage_bytes",
            "max_bytes",
            max_bytes.map(|v| v as i64),
        )
        .await
    }

    /// Read the client-set CORS allow-list. The `Option` is **row presence**
    /// (`None` ⇒ the admin never set it ⇒ the caller falls back to the
    /// `config.nest.cors_origins` seed); a present row decodes its JSON blob into
    /// the list (which may be empty — an admin who explicitly cleared the list,
    /// distinct from an absent row, and still wins over a non-empty seed). The
    /// list member of the `[nest]`-policy singletons.
    pub async fn get_cors_origins(&self) -> Result<Option<Vec<String>>> {
        let json: Option<String> = self
            .get_singleton_column("nest_cors_origins", "origins_json")
            .await?;
        match json {
            Some(s) => Ok(Some(
                serde_json::from_str(&s).context("decode nest_cors_origins JSON")?,
            )),
            None => Ok(None),
        }
    }

    /// Upsert the deployment-wide CORS allow-list (settable both ways; the latest
    /// write wins). Replaces the whole list as one JSON blob — the admin form
    /// submits the complete list, so this is a PUT not a merge. Writing `[]` is an
    /// explicitly-cleared list (present row), distinct from an absent row.
    pub async fn set_cors_origins(&self, origins: Vec<String>) -> Result<()> {
        let json = serde_json::to_string(&origins).context("encode nest_cors_origins")?;
        self.set_singleton_column("nest_cors_origins", "origins_json", json)
            .await
    }

    /// Read the admin's web-app origin choice. `None` ⇒ never set (the caller
    /// serves bundled); a present row whose text is not a known mode is an
    /// error, not a silent bundled — the boot resolve logs it and serves bundled,
    /// and a fresh `set` from the admin screen overwrites it.
    pub async fn get_web_app_origin(
        &self,
    ) -> Result<Option<fauna_protocol::web_app_origin::WebAppOrigin>> {
        let raw: Option<String> = self
            .get_singleton_column("nest_web_app_origin", "mode")
            .await?;
        raw.map(|s| {
            fauna_protocol::web_app_origin::WebAppOrigin::parse(&s)
                .with_context(|| format!("nest_web_app_origin holds an unknown mode {s:?}"))
        })
        .transpose()
    }

    /// Upsert the admin's web-app origin choice (a whole-value replace).
    pub async fn set_web_app_origin(
        &self,
        mode: fauna_protocol::web_app_origin::WebAppOrigin,
    ) -> Result<()> {
        self.set_singleton_column("nest_web_app_origin", "mode", mode.as_str())
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn web_app_origin_unset_reads_none_then_round_trips() {
        use fauna_protocol::web_app_origin::WebAppOrigin;
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_web_app_origin().await.unwrap(), None);
        db.set_web_app_origin(WebAppOrigin::Central).await.unwrap();
        assert_eq!(
            db.get_web_app_origin().await.unwrap(),
            Some(WebAppOrigin::Central)
        );
        db.set_web_app_origin(WebAppOrigin::Bundled).await.unwrap();
        assert_eq!(
            db.get_web_app_origin().await.unwrap(),
            Some(WebAppOrigin::Bundled)
        );
    }

    #[tokio::test]
    async fn subhandles_unset_reads_none_then_round_trips() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_subhandles().await.unwrap(), None);

        db.set_subhandles(true).await.unwrap();
        assert_eq!(db.get_subhandles().await.unwrap(), Some(true));

        db.set_subhandles(false).await.unwrap();
        assert_eq!(db.get_subhandles().await.unwrap(), Some(false));
    }

    #[tokio::test]
    async fn max_storage_bytes_distinguishes_unset_cleared_and_capped() {
        let db = CacheDb::open_in_memory().unwrap();
        // Unset on a fresh DB — outer None ⇒ the caller falls back to the seed.
        assert_eq!(db.get_max_storage_bytes().await.unwrap(), None);

        // A cap: present row, inner Some.
        db.set_max_storage_bytes(Some(8_000_000_000)).await.unwrap();
        assert_eq!(
            db.get_max_storage_bytes().await.unwrap(),
            Some(Some(8_000_000_000))
        );

        // An explicitly-cleared cap: present row, inner None (distinct from the
        // unset outer None above).
        db.set_max_storage_bytes(None).await.unwrap();
        assert_eq!(db.get_max_storage_bytes().await.unwrap(), Some(None));

        // Mutable: a later set overwrites.
        db.set_max_storage_bytes(Some(1)).await.unwrap();
        assert_eq!(db.get_max_storage_bytes().await.unwrap(), Some(Some(1)));
    }

    #[tokio::test]
    async fn cors_origins_distinguishes_unset_cleared_and_listed() {
        let db = CacheDb::open_in_memory().unwrap();
        // Unset on a fresh DB — None ⇒ the caller falls back to the seed.
        assert_eq!(db.get_cors_origins().await.unwrap(), None);

        // A list: present row.
        let list = vec![
            "https://app.example.com".to_string(),
            "https://admin.example.com".to_string(),
        ];
        db.set_cors_origins(list.clone()).await.unwrap();
        assert_eq!(db.get_cors_origins().await.unwrap(), Some(list));

        // An explicitly-cleared list: present row holding `[]` (distinct from the
        // unset None above).
        db.set_cors_origins(vec![]).await.unwrap();
        assert_eq!(db.get_cors_origins().await.unwrap(), Some(vec![]));

        // Mutable: a later set overwrites.
        let single = vec!["https://only.example.com".to_string()];
        db.set_cors_origins(single.clone()).await.unwrap();
        assert_eq!(db.get_cors_origins().await.unwrap(), Some(single));
    }

    #[tokio::test]
    async fn registration_mode_round_trips_and_latest_write_wins() {
        use fauna_protocol::node_policy::RegistrationMode;
        let db = CacheDb::open_in_memory().unwrap();
        // Never set ⇒ no row ⇒ the caller falls back to its seed.
        assert_eq!(db.get_registration_mode().await.unwrap(), None);
        // Mode + the orthogonal free-tier ceiling round-trip together.
        db.set_registration_mode(RegistrationMode::InviteRequired, Some(25))
            .await
            .unwrap();
        assert_eq!(
            db.get_registration_mode().await.unwrap(),
            Some((RegistrationMode::InviteRequired, Some(25)))
        );
        // Latest write wins; the cap is independently clearable.
        db.set_registration_mode(RegistrationMode::Closed, None)
            .await
            .unwrap();
        assert_eq!(
            db.get_registration_mode().await.unwrap(),
            Some((RegistrationMode::Closed, None))
        );
    }

    /// A row written by a NEWER binary that knows a registration mode this one
    /// does not must resolve **`Closed`**, never fall open — an unknown posture
    /// fails shut. The row is left intact, so the admin's real choice survives the
    /// downgrade and comes back when the newer binary does.
    #[tokio::test]
    async fn unknown_stored_registration_mode_fails_shut() {
        use fauna_protocol::node_policy::RegistrationMode;
        let db = CacheDb::open_in_memory().unwrap();
        db.set_registration_mode(RegistrationMode::Open, None)
            .await
            .unwrap();
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE nest_registration_mode SET mode = 'some_future_mode' WHERE id = 1",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            db.get_registration_mode().await.unwrap(),
            Some((RegistrationMode::Closed, None))
        );
    }
}
