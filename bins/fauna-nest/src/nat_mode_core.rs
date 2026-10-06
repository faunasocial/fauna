//! Transport-agnostic NAT-mode commit core + the single `apply_node_mode_change`
//! write path. Makes the nest's NAT axis (`public` / `private`) client-set
//! through the signed pre-identity commit ceremony (`mode_commit`) —
//! **mutable**: the NAT mode flips at runtime without touching at-rest data, so
//! there is no write-once `mode_conflict`, just an upsert.
//!
//! Two entry points share one write path:
//! - [`commit_nat_mode_core`] — the signed, admin-authenticated commit driven by
//!   the `fauna.setup.nat_mode` WS handler (onboarding wizard + admin toggle).
//! - [`apply_node_mode_change`] — the side-effecting write: upsert the row, swap
//!   the live `AppState.node_mode`, reconcile the MTA supervisor + notify
//!   bridges, audit. ACME/STUN are **restart-applied** (the boot reconcile in
//!   [`resolve_node_mode`] enforces them; a runtime flip does not tear down a
//!   live ACME client / STUN listener — design § 6, sanctioned).
//!
//! The module is deliberately free of any `fauna_protocol` dependency; the
//! core → wire mapping happens in `nat_mode_handlers` (design tracked
//! internally).

use std::sync::Arc;

use crate::config::NodeMode;
use crate::mode_commit::ModeCommitError;
use crate::routes::AppState;

/// Resolve the deployment's NAT axis at boot: the client-set `nest_nat_mode`
/// row wins; absent, fall back to the `config.nest.mode` seed (`FAUNA_MODE`).
/// Called once by `start_server` to populate `AppState.node_mode`, and once by
/// `main` (the same tiny indexed singleton read) for the pre-`AppState`
/// ACME/STUN/HTTP-01 boot decisions — both consult the same row, so they agree.
/// There is **no** "mismatch is fatal" reconcile (unlike storage mode): the
/// config value is a fallback seed, so DB-present-and-different-from-config is
/// the normal post-onboarding state, not an error.
pub async fn resolve_node_mode(
    db: &crate::db::CacheDb,
    config: &crate::config::NestConfig,
) -> NodeMode {
    match db.get_nat_mode().await {
        Ok(Some(mode)) => mode,
        Ok(None) => config.nest.mode,
        Err(e) => {
            // A read error is not fatal — degrade to the config seed (the box
            // must boot in some posture). Logged loudly; the next read retries.
            tracing::error!("get_nat_mode at boot failed: {e:#}; falling back to config seed");
            config.nest.mode
        }
    }
}

/// The set mode — adapters map this to the `NatModeReply` wire type.
#[derive(Debug)]
pub struct NatModeOutcome {
    pub mode: NodeMode,
}

/// `fauna.setup.nat_mode` — the admin's NAT-axis set. Verifies the mode,
/// actor_id, timestamp (±300 s, ms-aware), and the Ed25519 signature over
/// `mode_wire_str ‖ "\n" ‖ actor_id_hex ‖ "\n" ‖ timestamp_decimal`; requires
/// the nest claimed + the signer to be the committed admin; then applies the
/// change via [`apply_node_mode_change`] (mutable upsert + live re-eval). Any
/// valid set succeeds — re-setting the same mode is idempotent, a different mode
/// flips.
///
/// **`nest_id_hex` binds the commit to this nest** (`transport-connection.md`,
/// the nest-bound mode commit): it must equal this nest's own identity — a
/// blob bound to another nest is refused with a clear `invalid_request` (the
/// value is public requester-supplied cleartext, so naming the mismatch is
/// self-diagnosis, not an oracle) — and the signature is verified over the
/// nest-bound bytes under `SETUP_NAT_MODE_V2`, built from this nest's OWN
/// identity, never the request's spelling of it. The unbound V1 arm that
/// verified alongside through a transition window was removed 2026-09-24 by
/// the compat-remnant sweep (`version-compatibility.md` § Dimension 2).
pub async fn commit_nat_mode_core(
    state: &Arc<AppState>,
    mode_str: &str,
    actor_id_hex: &str,
    timestamp_ms: i64,
    signature_hex: &str,
    nest_id_hex: &str,
) -> Result<NatModeOutcome, ModeCommitError> {
    // 1. Parse + validate the mode.
    let mode = NodeMode::from_wire_str(mode_str).ok_or(ModeCommitError::InvalidRequest(
        "mode must be 'public' or 'private'",
    ))?;

    // 1b. The bound nest must be THIS nest, before any signature work — a
    //    mismatch is a verdict about the blob's target, not about its bytes.
    let own_hex = hex::encode(state.bound_identity());
    if nest_id_hex != own_hex {
        return Err(ModeCommitError::InvalidRequest(
            "nest_id does not name this nest",
        ));
    }

    // 2-5. Actor_id parse, timestamp freshness, Ed25519 signature (over the
    //    nat-mode signed message, keyed to the canonical actor hex), claimed +
    //    is_admin (`mode_commit::validate_mode_commit`). A pass here MUTATES
    //    (step 6 applies the mode and re-evaluates the live network posture),
    //    so the `is_admin` gate is this ceremony's only remaining belt.
    let actor_bytes = crate::mode_commit::validate_mode_commit(
        state,
        actor_id_hex,
        timestamp_ms,
        signature_hex,
        |actor_hex| {
            fauna_protocol::nat_mode::nat_mode_signed_message(
                mode.as_str(),
                actor_hex,
                timestamp_ms,
                &own_hex,
            )
        },
        "nat-mode commit",
    )
    .await?;

    // 6. Apply (mutable upsert + live re-eval). Audit names the admin.
    apply_node_mode_change(state, mode)
        .await
        .map_err(|_| ModeCommitError::Internal("failed to persist nat mode"))?;
    let _ = state
        .db
        .audit(
            Some(actor_bytes.as_slice()),
            "nest.nat_mode_set",
            Some(mode.as_str()),
            None,
        )
        .await;
    tracing::info!(
        "nat mode set: {} (by admin {})",
        mode.as_str(),
        hex::encode(actor_bytes)
    );
    Ok(NatModeOutcome { mode })
}

/// The single NAT-mode write path, shared by the onboarding commit and the
/// admin toggle (and any future Admin-class twin). Steps:
///
/// 1. **Upsert** the `nest_nat_mode` row (the atomic decision point).
/// 2. **Swap** the live `AppState.node_mode` so every runtime reader (whoami →
///    MDA bind, the supervisor gate, the private-only LAN-cert sync) follows.
/// 3. **Reconcile the MTA supervisor + notify bridges** — exactly the path
///    `set_mail_enabled` drives: a public→private flip commands the MTA service
///    down (private never runs the perimeter parser), private→public brings it
///    up when mail is enabled; the MDA bind follows the next per-request
///    `whoami`. The notify makes a running bridge re-read promptly.
///
/// **ACME + STUN are restart-applied, not torn down live** (design § 6,
/// sanctioned): a runtime flip to private leaves any already-running ACME
/// client / STUN listener until the next restart, where [`resolve_node_mode`]
/// re-gates them. The security-critical re-evals (MTA down, MDA LAN bind) are
/// live; ACME/STUN on a now-private box are harmless until restart (a failing
/// renewal / an idle STUN registration). Declared in the deployment-home
/// implementation-status.
pub async fn apply_node_mode_change(
    state: &Arc<AppState>,
    new_mode: NodeMode,
) -> anyhow::Result<()> {
    // 1. Persist (mutable upsert).
    state.db.set_nat_mode(new_mode).await?;
    // 2. Swap the live value.
    *state.node_mode.write().await = new_mode;
    // 3. Reconcile the MTA supervisor against the new axis + the independent
    //    service toggles, then notify a running bridge. The toggles come from
    //    the one reader (`effective_service_toggles`): unset mail reads OFF
    //    (Stage-5 default-off, owned by `effective_mail_enabled`) and unset
    //    caldav/carddav/webdav follow mail. Best-effort on a read error —
    //    `ServiceToggles::default()` is all-off, which is the safe direction
    //    here: it can only fail to start a service, never start the perimeter
    //    SMTP parser on a box whose state we could not read.
    let toggles = crate::mail_enable::effective_service_toggles(&state.db)
        .await
        .unwrap_or_default();
    crate::mail_enable::reconcile_supervisor(
        new_mode,
        toggles.mail,
        toggles.caldav,
        toggles.carddav,
        toggles.webdav,
    )
    .await;
    crate::bridge_routing_handlers::notify_bridges_config_changed(
        state,
        fauna_protocol::bridge_routing::config_change_reason::NODE_MODE,
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use std::sync::Arc;

    /// `resolve_node_mode`: the DB row wins; absent, the config seed is used.
    #[tokio::test]
    async fn resolve_prefers_db_row_over_config_seed() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let mut config = test_config(NodeMode::Public);
        // No row → follows the seed.
        assert_eq!(resolve_node_mode(&db, &config).await, NodeMode::Public);
        config.nest.mode = NodeMode::Private;
        assert_eq!(resolve_node_mode(&db, &config).await, NodeMode::Private);
        // A client-set row wins over the seed, in either direction.
        db.set_nat_mode(NodeMode::Public).await.unwrap();
        assert_eq!(resolve_node_mode(&db, &config).await, NodeMode::Public);
        db.set_nat_mode(NodeMode::Private).await.unwrap();
        config.nest.mode = NodeMode::Public;
        assert_eq!(resolve_node_mode(&db, &config).await, NodeMode::Private);
    }

    fn test_config(mode: NodeMode) -> crate::config::NestConfig {
        crate::config::NestConfig {
            nest: crate::config::NestSection {
                mode,
                listen: "127.0.0.1:0".into(),
                ..Default::default()
            },
            bridges: None,
            submission: None,
            acme: None,
            email: None,
            update: Default::default(),
        }
    }
}
