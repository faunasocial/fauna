//! Boot-resolve + live-apply for the client-set `[nest]`-policy knobs
//! (`registration_mode`, `subhandles`, `max_storage_bytes`, `cors_origins`).
//! The DB half is `db/node_policy.rs`;
//! the WS-RPC handlers live in `node_policy_handlers`. Mirrors `nat_mode_core`
//! (resolve-from-DB-else-seed + the single apply write path) but **simpler**:
//! these are post-claim Admin-class toggles, so there is no signed pre-identity
//! commit ceremony — the handler authenticates the admin from the connection
//! (`require_class`) — and the apply drives no supervisor/bridge reconcile (no
//! bridge reads these), just an upsert + a live `RwLock` swap.
//!
//! Each toggle resolves DB-row-wins-else-seed: a present `nest_registration_mode`
//! / `nest_subhandles` row (client-set) wins; absent, the boot seed
//! (`config.nest.registration_mode` / `config.nest.subhandles`) is used. The config
//! value is a fallback seed, so DB-present-and-different-from-config is the
//! normal post-onboarding state, not an error.
//!
//! **Not in this family:** `AppState.enforce_tier_quotas`. It is artifact-set IPC
//! (server `true` / desktop `false`), not an admin choice, so it has no DB row, no
//! seed, and no admin kind. The former `fauna.admin.set_require_registration`
//! (retired 2026-07-12, off the wire 2026-09-24) conflated the two — see
//! `AppState::enforce_tier_quotas`.

use std::sync::Arc;

use fauna_protocol::node_policy::RegistrationMode;

use crate::routes::AppState;

/// Resolve the deployment's registration posture at boot: the client-set
/// `nest_registration_mode` row wins (mode + the orthogonal free-tier ceiling);
/// absent, the `config.nest.registration_mode` `seed`.
///
/// A read error degrades to [`fauna_protocol::node_policy::DEFAULT_REGISTRATION_MODE`]
/// (`Closed`), **not** to the seed: every other resolve in this file falls back to
/// the seed because any posture is as safe as any other, but a registration
/// posture is not symmetric — if the DB is unreadable we must not risk booting a
/// box that admits strangers. Fail shut, and log loudly.
pub async fn resolve_registration_mode(
    db: &crate::db::CacheDb,
    seed: (RegistrationMode, Option<u64>),
) -> (RegistrationMode, Option<u64>) {
    match db.get_registration_mode().await {
        Ok(Some(v)) => v,
        Ok(None) => seed,
        Err(e) => {
            tracing::error!(
                "get_registration_mode at boot failed: {e:#}; failing SHUT to \
                 `closed` (a registration posture must never fail open)"
            );
            (fauna_protocol::node_policy::DEFAULT_REGISTRATION_MODE, None)
        }
    }
}

/// The single registration-posture write path: upsert the `nest_registration_mode`
/// row (the atomic decision point), then swap the live `AppState.registration_mode`
/// so the next `fauna.account.register` follows immediately — no reboot.
pub async fn apply_registration_mode_change(
    state: &Arc<AppState>,
    mode: RegistrationMode,
    max_free_users: Option<u64>,
) -> anyhow::Result<()> {
    state.db.set_registration_mode(mode, max_free_users).await?;
    *state.registration_mode.write().await = (mode, max_free_users);
    Ok(())
}

/// Resolve the deployment's `subhandles` policy at boot: the client-set
/// `nest_subhandles` row wins; absent, the `config.nest.subhandles` `seed`
/// (default `false`). A read error degrades to the seed.
pub async fn resolve_subhandles(db: &crate::db::CacheDb, seed: bool) -> bool {
    match db.get_subhandles().await {
        Ok(Some(v)) => v,
        Ok(None) => seed,
        Err(e) => {
            tracing::error!("get_subhandles at boot failed: {e:#}; falling back to config seed");
            seed
        }
    }
}

/// The single `subhandles` write path: upsert the `nest_subhandles` row, then
/// swap the live `AppState.subhandles`. The next `nest.info` / address-resolution
/// read advertises (or stops advertising) the subhandle forms accordingly.
pub async fn apply_subhandles_change(state: &Arc<AppState>, enabled: bool) -> anyhow::Result<()> {
    state.db.set_subhandles(enabled).await?;
    *state.subhandles.write().await = enabled;
    Ok(())
}

/// Resolve the "accept only signups carrying app age verification" gate at
/// boot: the client-set `nest_age_verification_required` row wins; absent, the
/// **hard-coded default off** (`family-safety.md` § The account age band D5 —
/// works out of the box: web/desktop signups have no attestation path).
/// Deliberately no config seed (`public-mode.md` § Age at registration); a
/// read error degrades to off, which admits exactly what an un-set nest
/// admits.
pub async fn resolve_age_verification_required(db: &crate::db::CacheDb) -> bool {
    match db.get_age_verification_required().await {
        Ok(Some(v)) => v,
        Ok(None) => false,
        Err(e) => {
            tracing::error!(
                "get_age_verification_required at boot failed: {e:#}; falling back to off"
            );
            false
        }
    }
}

/// The single age-verification-gate write path: upsert the
/// `nest_age_verification_required` row, then swap the live
/// `AppState.age_verification_required`. The next `fauna.account.register`
/// reads the new posture live — no restart.
pub async fn apply_age_verification_required_change(
    state: &Arc<AppState>,
    enabled: bool,
) -> anyhow::Result<()> {
    state.db.set_age_verification_required(enabled).await?;
    *state.age_verification_required.write().await = enabled;
    Ok(())
}

/// Resolve the deployment's node-wide storage cap at boot: a present
/// `nest_max_storage_bytes` row (client-set) wins — including a present row that
/// cleared the cap (`Some(None)` ⇒ `None`); absent (`None`), the
/// `config.nest.max_storage_bytes` `seed`. A read error degrades to the seed.
pub async fn resolve_max_storage_bytes(db: &crate::db::CacheDb, seed: Option<u64>) -> Option<u64> {
    match db.get_max_storage_bytes().await {
        Ok(Some(v)) => v,
        Ok(None) => seed,
        Err(e) => {
            tracing::error!(
                "get_max_storage_bytes at boot failed: {e:#}; falling back to config seed"
            );
            seed
        }
    }
}

/// The single `max_storage_bytes` write path: upsert the `nest_max_storage_bytes`
/// row (the atomic decision point), then swap the live `AppState.max_storage_bytes`
/// so the next `/internal/router-status` capacity read follows immediately. No
/// bridge/supervisor reconcile (nothing off-box reads this cap).
pub async fn apply_max_storage_bytes_change(
    state: &Arc<AppState>,
    max_bytes: Option<u64>,
) -> anyhow::Result<()> {
    state.db.set_max_storage_bytes(max_bytes).await?;
    *state.max_storage_bytes.write().await = max_bytes;
    Ok(())
}

/// The built-in CORS origin a fresh / never-configured nest trusts — the official
/// hosted web app. An empty client-set (or seed) list resolves to "trust only
/// this origin" (see [`origin_allowed`]); the single source of truth shared by the
/// live `AllowOrigin::predicate` in `lib.rs` and the unit-tested helper. It IS
/// the shared central-origin constant the web-app origin redirect also targets,
/// so no second literal names the origin.
pub const DEFAULT_CORS_ORIGIN: &str = fauna_protocol::web_app_origin::CENTRAL_APP_ORIGIN;

/// Resolve the deployment's CORS allow-list at boot: a present `nest_cors_origins`
/// row (client-set) wins — including a present row holding `[]` (an explicitly
/// cleared list ⇒ the default-origin-only posture, which still wins over a
/// non-empty seed); absent (`None`), the `config.nest.cors_origins` `seed`. A read
/// error degrades to the seed (the box must boot serving something).
pub async fn resolve_cors_origins(db: &crate::db::CacheDb, seed: Vec<String>) -> Vec<String> {
    match db.get_cors_origins().await {
        Ok(Some(v)) => v,
        Ok(None) => seed,
        Err(e) => {
            tracing::error!("get_cors_origins at boot failed: {e:#}; falling back to config seed");
            seed
        }
    }
}

/// Resolve the deployment's client-facing serving **port** at boot on a
/// direct-listener deployment: the client-set `serving_port` singleton wins;
/// absent, the `--bind`/`listen` `seed_port`. A read error degrades to the seed
/// (the box must bind something). Unlike the other resolves this returns the
/// **port only** — the caller keeps the seed's interface/host (only the port is
/// the admin's choice; the interface/host stays artifact-wiring). **Apply-on-
/// restart:** the nest cannot hot-rebind its own `TcpListener`, so this is read
/// once at startup; a later `set_serving_port` takes effect on the next restart
/// (the desktop supervisor drives the restart via the `/data/serving-port` flag).
/// **Inert behind the SNI router:** the caller skips this entirely when
/// `FAUNA_FRONTED_BY_ROUTER` is set (the external port is then the router +
/// compose port-map). See `lib.rs` + `nest/common.md` § Serving ports.
pub async fn resolve_serving_port(db: &crate::db::CacheDb, seed_port: u16) -> u16 {
    match db.get_serving_port().await {
        Ok(Some(p)) => p,
        Ok(None) => seed_port,
        Err(e) => {
            tracing::error!(
                "get_serving_port at boot failed: {e:#}; falling back to bind seed port"
            );
            seed_port
        }
    }
}

/// The single `cors_origins` write path: upsert the `nest_cors_origins` JSON-blob
/// row (the atomic decision point), then atomically swap the live
/// `AppState.cors_origins` ArcSwap so the next cross-origin request's
/// `AllowOrigin::predicate` follows immediately — no reboot, no listener rebuild.
/// No bridge/supervisor reconcile (nothing off-box reads this list).
pub async fn apply_cors_origins_change(
    state: &Arc<AppState>,
    origins: Vec<String>,
) -> anyhow::Result<()> {
    state.db.set_cors_origins(origins.clone()).await?;
    state.cors_origins.store(Arc::new(origins));
    Ok(())
}

/// Whether a request `origin` is allowed by the resolved CORS allow-list. An empty
/// list collapses to "trust only [`DEFAULT_CORS_ORIGIN`]" — preserving the
/// fresh-nest default; a non-empty list is an exact membership check. Pure +
/// allocation-free so the per-request `AllowOrigin::predicate` can call it
/// directly and it stays unit-testable apart from the tower middleware.
pub fn origin_allowed(origins: &[String], origin: &str) -> bool {
    if origins.is_empty() {
        origin == DEFAULT_CORS_ORIGIN
    } else {
        origins.iter().any(|o| o == origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    #[tokio::test]
    async fn resolve_registration_mode_db_row_wins_over_seed() {
        let db = CacheDb::open_in_memory().unwrap();
        // No row → the seed passes through (mode + cap together).
        assert_eq!(
            resolve_registration_mode(&db, (RegistrationMode::Open, Some(10))).await,
            (RegistrationMode::Open, Some(10))
        );
        assert_eq!(
            resolve_registration_mode(&db, (RegistrationMode::Closed, None)).await,
            (RegistrationMode::Closed, None)
        );
        // A client-set row wins over the seed.
        db.set_registration_mode(RegistrationMode::InviteRequired, Some(5))
            .await
            .unwrap();
        assert_eq!(
            resolve_registration_mode(&db, (RegistrationMode::Open, None)).await,
            (RegistrationMode::InviteRequired, Some(5))
        );
        // A client-set row that CLOSES registration wins over an open seed — the
        // posture the old `require_registration` boolean could silently override.
        db.set_registration_mode(RegistrationMode::Closed, None)
            .await
            .unwrap();
        assert_eq!(
            resolve_registration_mode(&db, (RegistrationMode::Open, Some(99))).await,
            (RegistrationMode::Closed, None)
        );
    }

    #[test]
    fn registration_mode_wire_strings_round_trip() {
        for m in [
            RegistrationMode::Open,
            RegistrationMode::InviteRequired,
            RegistrationMode::Closed,
        ] {
            assert_eq!(RegistrationMode::from_wire_str(m.as_wire_str()), Some(m));
        }
        assert_eq!(RegistrationMode::from_wire_str("nonsense"), None);
    }

    #[tokio::test]
    async fn resolve_subhandles_db_row_wins_over_seed() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(!resolve_subhandles(&db, false).await);
        assert!(resolve_subhandles(&db, true).await);
        db.set_subhandles(true).await.unwrap();
        assert!(resolve_subhandles(&db, false).await);
        db.set_subhandles(false).await.unwrap();
        assert!(!resolve_subhandles(&db, true).await);
    }

    #[tokio::test]
    async fn resolve_max_storage_bytes_db_row_wins_over_seed() {
        let db = CacheDb::open_in_memory().unwrap();
        // No row → the seed passes through (a cap or None).
        assert_eq!(resolve_max_storage_bytes(&db, Some(9)).await, Some(9));
        assert_eq!(resolve_max_storage_bytes(&db, None).await, None);
        // A client-set cap wins.
        db.set_max_storage_bytes(Some(5)).await.unwrap();
        assert_eq!(resolve_max_storage_bytes(&db, None).await, Some(5));
        // An explicitly-cleared cap (present row, inner None) wins over a
        // non-None seed.
        db.set_max_storage_bytes(None).await.unwrap();
        assert_eq!(resolve_max_storage_bytes(&db, Some(9)).await, None);
    }

    #[tokio::test]
    async fn resolve_cors_origins_db_row_wins_over_seed() {
        let db = CacheDb::open_in_memory().unwrap();
        let seed = vec!["https://seed.example.com".to_string()];
        // No row → the seed passes through (a list or empty).
        assert_eq!(resolve_cors_origins(&db, seed.clone()).await, seed);
        assert!(resolve_cors_origins(&db, vec![]).await.is_empty());
        // A client-set list wins.
        let set = vec!["https://set.example.com".to_string()];
        db.set_cors_origins(set.clone()).await.unwrap();
        assert_eq!(resolve_cors_origins(&db, vec![]).await, set);
        // An explicitly-cleared list (present row, empty) wins over a non-empty
        // seed.
        db.set_cors_origins(vec![]).await.unwrap();
        assert!(resolve_cors_origins(&db, seed).await.is_empty());
    }

    #[tokio::test]
    async fn resolve_serving_port_db_row_wins_over_seed() {
        let db = CacheDb::open_in_memory().unwrap();
        // No row → the bind seed port passes through unchanged.
        assert_eq!(resolve_serving_port(&db, 443).await, 443);
        assert_eq!(resolve_serving_port(&db, 3000).await, 3000);
        // A client-set port wins over the seed.
        db.set_serving_port(8443).await.unwrap();
        assert_eq!(resolve_serving_port(&db, 443).await, 8443);
        assert_eq!(resolve_serving_port(&db, 3000).await, 8443);
        // Latest write wins (settable both ways).
        db.set_serving_port(443).await.unwrap();
        assert_eq!(resolve_serving_port(&db, 3000).await, 443);
    }

    #[test]
    fn origin_allowed_empty_means_default_origin_only() {
        // Empty list ⇒ only the built-in default origin is trusted.
        assert!(origin_allowed(&[], DEFAULT_CORS_ORIGIN));
        assert!(!origin_allowed(&[], "https://evil.example.com"));
        // Non-empty list ⇒ exact membership; the default is NOT implicitly added.
        let origins = vec!["https://app.example.com".to_string()];
        assert!(origin_allowed(&origins, "https://app.example.com"));
        assert!(!origin_allowed(&origins, DEFAULT_CORS_ORIGIN));
        assert!(!origin_allowed(&origins, "https://other.example.com"));
    }
}
