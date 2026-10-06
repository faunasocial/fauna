//! WS-RPC handlers for the nest-OWN transport/abuse policy —
//! `fauna.transport.{get,put}_policy` (both **Admin**-class).
//!
//! This is nest's own client-facing TLS-listener abuse config (today: the
//! per-source-IP concurrent-connection cap `serve_tls` enforces), NOT a mail
//! policy and NOT projected to the bridge — contrast the
//! `fauna.bridges.put_<substruct>_policy` kinds in
//! [`crate::bridge_routing_handlers`], which nest serves to the Go mail
//! bridge over `fetch_config`. By the product invariant an admin-tunable
//! abuse knob (same class as spam thresholds) is client-set nest config, so
//! the cap lives in the `transport_policy` store
//! ([`crate::db::transport_policy`]) and in no environment variable. The cap is
//! **hot-reloaded**: `put_policy` calls `set_max` on the `AppState`-shared
//! `PerIpConnLimit` the live `serve_tls` accept loop holds, so a change binds
//! the listener without a nest restart. Spec:
//! `docs/goal/architecture/transport-connection.md` § Abuse posture item (2).

use std::time::Duration;

use crate::bridge_routing_handlers::{encode_reply, internal, malformed, require_class};
use crate::db::transport_policy::TransportPolicyOverrides;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};
use fauna_protocol::decode_strict as decode;
use fauna_protocol::transport::{
    GetTransportPolicyRequest, KIND_GET_TRANSPORT_POLICY, KIND_PUT_TRANSPORT_POLICY,
    PutTransportPolicyReply, PutTransportPolicyRequest, TransportPolicyView,
};

// ── fauna.transport.get_policy (Admin) ──────────────────────────────

fn transport_policy_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, KIND_GET_TRANSPORT_POLICY).await?;
            let _req: GetTransportPolicyRequest = decode(&payload).map_err(malformed)?;
            let overrides = state.db.get_transport_policy().await.map_err(internal)?;
            // Report the **live** effective cap (DB override → env →
            // catalog default) via the one shared resolver, so an admin
            // reads back exactly what the TLS accept loop enforces.
            let max_conns_per_ip = crate::resolve_tls_per_ip_cap(overrides.max_conns_per_ip) as u32;
            encode_reply(&TransportPolicyView {
                max_conns_per_ip,
                ..Default::default()
            })
        })
    })
}

// ── fauna.transport.put_policy (Admin) ──────────────────────────────

fn transport_policy_put_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, KIND_PUT_TRANSPORT_POLICY).await?;
            let req: PutTransportPolicyRequest = decode(&payload).map_err(malformed)?;
            // Full PUT (the admin form submits the complete policy); a
            // missing field decodes as `None` ⇒ catalog default. Idempotent
            // overwrite of the single `id = 1` row.
            state
                .db
                .put_transport_policy(TransportPolicyOverrides {
                    max_conns_per_ip: req.max_conns_per_ip,
                })
                .await
                .map_err(internal)?;
            // Hot-reload the live `serve_tls` per-IP cap so the change binds the
            // accept loop without a nest restart. `set_max` on the AppState-
            // shared limiter (the same Arc `serve_tls` holds) takes effect on
            // its next `try_acquire`. The effective value goes through the same
            // `resolve_tls_per_ip_cap` precedence the boot read + `get_policy`
            // view use, so the listener and the read-back stay in lock-step.
            // (A plain-HTTP nest has no TLS accept loop; the set_max is a
            // harmless no-op on an unenforced limiter.)
            state
                .per_ip_conn_limit
                .set_max(crate::resolve_tls_per_ip_cap(req.max_conns_per_ip));
            encode_reply(&PutTransportPolicyReply {
                ok: true,
                ..Default::default()
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────

pub fn register_transport_policy_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_GET_TRANSPORT_POLICY,
        RpcKindMeta {
            // Pure read of the deployment-wide transport policy.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: transport_policy_get_handler(),
        },
    );
    b.add(
        KIND_PUT_TRANSPORT_POLICY,
        RpcKindMeta {
            // Idempotent overwrite of the single-row policy — same payload
            // twice → same state, so a replay is harmless.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: transport_policy_put_handler(),
        },
    );
}
