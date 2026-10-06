//! WS-RPC handlers for the client-set `[nest]`-policy knobs —
//! `fauna.admin.set_subhandles` (bool),
//! `fauna.admin.set_max_storage_bytes` (nullable int), and
//! `fauna.admin.set_cors_origins` (string list). Thin adapters over
//! `node_policy_core`'s apply path: authenticate the admin from the connection
//! (`require_class`, Admin-class via `bridge_method_allowlist`), decode the value
//! (`{ enabled }` / `{ max_bytes }` / `{ origins }`), upsert the DB singleton +
//! swap the live `AppState` cell (RwLock for the bool/int; ArcSwap for the
//! list, read by the sync CORS predicate), audit, reply `{ ok }`. Mirrors
//! `set_mail_enabled_handler` — the authed Admin-class shape, *not* `nat_mode`'s
//! pre-identity signed ceremony (these are post-claim knobs with sensible fresh
//! defaults).
//!
//! The admin reads the current values back on `fauna.setup.status`
//! (`SetupStatusReply.{require_registration,subhandles,max_storage_bytes,cors_origins}`);
//! `subhandles` is also reflected on `fauna.nest.info`. Kind registry twin:
//! `kind.rs::register_admin_kinds`.

use std::time::Duration;

use fauna_protocol::node_policy::{
    RegistrationMode, SetAgeVerificationRequiredReply, SetAgeVerificationRequiredRequest,
    SetCorsOriginsReply, SetCorsOriginsRequest, SetMaxStorageBytesReply, SetMaxStorageBytesRequest,
    SetRegistrationModeReply, SetRegistrationModeRequest, SetServingPortReply,
    SetServingPortRequest, SetSubhandlesReply, SetSubhandlesRequest,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode};

use crate::bridge_routing_handlers::require_class;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ──────────────────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

// ── fauna.admin.set_registration_mode ────────────────────────────────────────

fn set_registration_mode_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.admin.set_registration_mode").await?;
            let req: SetRegistrationModeRequest = decode(&payload).map_err(malformed)?;
            let mode = RegistrationMode::from_wire_str(&req.mode).ok_or_else(|| {
                let mut e = RpcError::new(
                    "fauna.node_policy.registration_mode_invalid",
                    "error.node_policy.registration_mode_invalid",
                );
                e.details = Some(Box::new(Value::String(format!(
                    "unknown registration mode {:?}; expected open / invite_required / closed",
                    req.mode
                ))));
                e
            })?;
            crate::node_policy_core::apply_registration_mode_change(
                &state,
                mode,
                req.max_free_users,
            )
            .await
            .map_err(|e| internal(format!("persist registration mode: {e:#}")))?;
            let _ = state
                .db
                .audit(
                    Some(actor_id.as_slice()),
                    "nest.registration_mode_set",
                    Some(mode.as_wire_str()),
                    None,
                )
                .await;
            encode_reply(&SetRegistrationModeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.set_subhandles ───────────────────────────────────────────────

fn set_subhandles_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.admin.set_subhandles").await?;
            let req: SetSubhandlesRequest = decode(&payload).map_err(malformed)?;
            crate::node_policy_core::apply_subhandles_change(&state, req.enabled)
                .await
                .map_err(|e| internal(format!("persist subhandles toggle: {e:#}")))?;
            let _ = state
                .db
                .audit(
                    Some(actor_id.as_slice()),
                    "nest.subhandles_set",
                    Some(if req.enabled { "true" } else { "false" }),
                    None,
                )
                .await;
            encode_reply(&SetSubhandlesReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.set_age_verification_required ────────────────────────────────

fn set_age_verification_required_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.admin.set_age_verification_required",
            )
            .await?;
            let req: SetAgeVerificationRequiredRequest = decode(&payload).map_err(malformed)?;
            crate::node_policy_core::apply_age_verification_required_change(&state, req.required)
                .await
                .map_err(|e| {
                    internal(format!("persist age_verification_required toggle: {e:#}"))
                })?;
            let _ = state
                .db
                .audit(
                    Some(actor_id.as_slice()),
                    "nest.age_verification_required_set",
                    Some(if req.required { "true" } else { "false" }),
                    None,
                )
                .await;
            encode_reply(&SetAgeVerificationRequiredReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.set_max_storage_bytes ────────────────────────────────────────

fn set_max_storage_bytes_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.admin.set_max_storage_bytes").await?;
            let req: SetMaxStorageBytesRequest = decode(&payload).map_err(malformed)?;
            crate::node_policy_core::apply_max_storage_bytes_change(&state, req.max_bytes)
                .await
                .map_err(|e| internal(format!("persist max_storage_bytes: {e:#}")))?;
            let detail = match req.max_bytes {
                Some(v) => v.to_string(),
                None => "none".to_string(),
            };
            let _ = state
                .db
                .audit(
                    Some(actor_id.as_slice()),
                    "nest.max_storage_bytes_set",
                    Some(detail.as_str()),
                    None,
                )
                .await;
            encode_reply(&SetMaxStorageBytesReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.set_cors_origins ─────────────────────────────────────────────

fn set_cors_origins_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.admin.set_cors_origins").await?;
            let req: SetCorsOriginsRequest = decode(&payload).map_err(malformed)?;
            let count = req.origins.len();
            crate::node_policy_core::apply_cors_origins_change(&state, req.origins)
                .await
                .map_err(|e| internal(format!("persist cors_origins: {e:#}")))?;
            let _ = state
                .db
                .audit(
                    Some(actor_id.as_slice()),
                    "nest.cors_origins_set",
                    Some(count.to_string().as_str()),
                    None,
                )
                .await;
            encode_reply(&SetCorsOriginsReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.set_serving_port ─────────────────────────────────────────────

/// Set the deployment-wide client-facing API serving port (the nest's own HTTPS
/// listener: the WS-RPC transport + the served SPA). The **port** member of the
/// `[nest]`-policy family, but it does **not** swap a live `AppState` cell like
/// the bool/int/list knobs above — the nest cannot hot-rebind its own
/// `TcpListener`, so the change is **apply-on-restart** (boot-resolved into the
/// direct-listener bind; see `node_policy_core::resolve_serving_port` +
/// `lib.rs`). It mirrors `set_caldav_port_handler` instead: persist the DB
/// singleton, then materialize the `/data/serving-port` value flag the desktop
/// supervisor reads to restart the nest on the new port. No bridge
/// `config_changed` notify (the *nest's own* listener changes, not the MDA's).
/// **Rejected on a router-fronted (Docker) nest** (`is_fronted_by_router()` ⇒
/// `fauna.node_policy.serving_port_fronted`): the external port there is the
/// `fauna-sni-router` + compose port-map, so the singleton can never apply —
/// the serving port is a direct-listener (desktop / self-hosted / bare-IP)
/// choice only (decision D, 2026-06-23), and accepting an inert value would be
/// config theatre. Admin-class (`bridge_method_allowlist.rs`). Spec:
/// `nest/common.md` § Serving ports.
fn set_serving_port_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.admin.set_serving_port").await?;
            // (Decision D, 2026-06-23.) A router-fronted deployment (Docker)
            // realizes its external client-facing port via the `fauna-sni-router`
            // + compose port-map, NOT nest's own bind, so a chosen `serving_port`
            // can never take effect here. The serving port is a direct-listener
            // (desktop / self-hosted / bare-IP) admin choice only — persisting an
            // inert value would be config theatre (a silent no-op). Reject up
            // front with a clear, surfaceable error. See `nest/common.md`
            // § Serving ports.
            if crate::is_fronted_by_router() {
                return Err(RpcError::new(
                    "fauna.node_policy.serving_port_fronted",
                    "error.node_policy.serving_port_fronted",
                )
                .with_details_text(
                    "this deployment serves its client-facing API on a fixed port \
                     behind the built-in router; the serving port is configurable \
                     only on a self-hosted or desktop nest",
                ));
            }
            let req: SetServingPortRequest = decode(&payload).map_err(malformed)?;
            // Port 0 is not a bindable listener port — reject it up front for a
            // clean error rather than tripping the DB CHECK constraint. (u16
            // already caps the upper bound at 65535.)
            if req.port == 0 {
                return Err(malformed("serving_port must be in 1..=65535 (got 0)"));
            }
            // (1) Persist the authoritative DB singleton.
            state
                .db
                .set_serving_port(req.port)
                .await
                .map_err(|e| internal(format!("persist serving_port: {e:#}")))?;
            // (2) Materialize the desktop-supervisor's value flag (skipped on the
            // in-memory/test path, which has no data dir). The Docker entrypoint
            // ignores the flag — the external port there is the SNI router +
            // compose port-map.
            match crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
                Some(dir) => {
                    crate::mail_enable::set_serving_port_flag(&dir, req.port).map_err(|e| {
                        internal(format!("write serving-port flag in {}: {e}", dir.display()))
                    })?;
                }
                None => {
                    tracing::debug!(
                        target: "serving_port",
                        port = req.port,
                        "no data dir configured (in-memory / test) — serving-port flag-file write skipped"
                    );
                }
            }
            let _ = state
                .db
                .audit(
                    Some(actor_id.as_slice()),
                    "nest.serving_port_set",
                    Some(req.port.to_string().as_str()),
                    None,
                )
                .await;
            encode_reply(&SetServingPortReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// Register the client-set `[nest]`-policy kinds (Admin-class, `@5 s`,
/// `forbid_replay = false` — re-setting the same value is idempotent and the
/// per-connection idempotency cache replays the first reply on recovery).
pub fn register_node_policy_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.admin.set_registration_mode",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_registration_mode_handler(),
        },
    );
    b.add(
        "fauna.admin.set_subhandles",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_subhandles_handler(),
        },
    );
    b.add(
        "fauna.admin.set_age_verification_required",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_age_verification_required_handler(),
        },
    );
    b.add(
        "fauna.admin.set_max_storage_bytes",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_max_storage_bytes_handler(),
        },
    );
    b.add(
        "fauna.admin.set_cors_origins",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_cors_origins_handler(),
        },
    );
    b.add(
        "fauna.admin.set_serving_port",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_serving_port_handler(),
        },
    );
}
