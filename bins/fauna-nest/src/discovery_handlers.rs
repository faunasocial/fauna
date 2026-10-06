//! Pre-identity public-discovery WS-RPC handlers — `fauna.nest.info`,
//! `fauna.handle.available`, `fauna.nest.resolve`, `fauna.actor.by_handle`,
//! `fauna.setup.status`. A behavior-preserving transport migration of the
//! public GET routes `/api/v1/{node-info,handle-available/{h},resolve-node/{d},
//! actor/by-handle/{h},setup-status}`; the read logic lives in the shared
//! `discovery_core` (the HTTP twins call the same fns). These kinds run **only**
//! on the anonymous WS connection (`GET /api/v1/ws`, no bearer) — enforced by
//! `pre_identity_allowlist` + the dispatcher gate in `routes::dispatch_request`.
//! Part of the WS-RPC-everywhere migration (tracked internally). Mirrors
//! `auth_handlers`.
//!
//! All five are anonymous reads — the dispatcher's `actor_id` argument is
//! ignored (there is no bearer-actor on the anonymous connection, and these
//! reads never depended on one).
//!
//! Error codes: `fauna.handle.invalid` (bad handle format),
//! `fauna.nest.invalid_domain` (rejected resolve target), `fauna.actor.not_found`
//! (unknown handle), `fauna.actor.domain_not_local` (a `by_handle` `domain`
//! qualifier this nest does not serve); malformed payloads and server faults
//! reuse the `fauna.protocol.*` infra codes.

use std::time::Duration;

use fauna_protocol::discovery::{
    ActorByHandleReply, ActorByHandleRequest, HandleAvailableReply, HandleAvailableRequest,
    ModerationInfo, NestInfoReply, NestInfoRequest, NestResolveReply, NestResolveRequest,
    RegistrationInfo, SetupStatusReply, SetupStatusRequest,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode};

use crate::discovery_core::{self, DiscoveryError};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

/// Map the transport-agnostic `DiscoveryError` to an `RpcError` — the WS-RPC
/// counterpart of the HTTP status mapping in the deprecated registration twins.
fn discovery_error_to_rpc(e: DiscoveryError) -> RpcError {
    match e {
        DiscoveryError::InvalidHandle(reason) => {
            let mut r = RpcError::new("fauna.handle.invalid", "error.handle.invalid");
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        DiscoveryError::InvalidDomain(reason) => {
            let mut r = RpcError::new("fauna.nest.invalid_domain", "error.node.invalid_domain");
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        DiscoveryError::HandleNotFound => {
            RpcError::new("fauna.actor.not_found", "error.actor.not_found")
        }
        DiscoveryError::DomainNotLocal => RpcError::new(
            "fauna.actor.domain_not_local",
            "error.actor.domain_not_local",
        ),
        DiscoveryError::Internal(msg) => {
            let mut r = RpcError::new("fauna.protocol.internal", "error.protocol.internal");
            r.details = Some(Box::new(Value::String(msg)));
            r
        }
    }
}

// ── fauna.nest.info (≡ GET /api/v1/node-info) ───────────────────────────────

fn nest_info_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            // Empty request (mirrors fauna.spam.get_preferences) — decode to
            // reject a malformed (non-map) payload, then ignore.
            let _req: NestInfoRequest = decode(&payload).map_err(malformed)?;
            let info = discovery_core::nest_info_core(&state).await;
            encode_reply(&NestInfoReply {
                domain: info.domain,
                nest_id: hex::encode(info.nest_id),
                version: info.version.to_string(),
                software: info.software.to_string(),
                protocols: info.protocols,
                capabilities: info.capabilities,
                iroh_relay_url: info.iroh_relay_url,
                subhandles: info.subhandles,
                registration: info.registration.map(|r| RegistrationInfo {
                    tiers: r.tiers,
                    handle_domain: r.handle_domain,
                    extra: Default::default(),
                }),
                moderation: ModerationInfo {
                    extra: Default::default(),
                },
                web_serving_domain: Some(info.web_serving_domain),
                room_read_pubkey: info.room_read_pubkey.map(hex::encode),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.handle.available (≡ GET /api/v1/handle-available/{handle}) ─────────

fn handle_available_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: HandleAvailableRequest = decode(&payload).map_err(malformed)?;
            let avail = discovery_core::handle_available_core(&state, &req.handle)
                .await
                .map_err(discovery_error_to_rpc)?;
            encode_reply(&HandleAvailableReply {
                available: avail.available,
                handle: avail.handle,
                domain: avail.domain,
                cooldown: avail.cooldown,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.nest.resolve (≡ GET /api/v1/resolve-node/{domain}) ─────────────────

fn nest_resolve_handler() -> RpcHandler {
    Box::new(|_state, _actor, payload| {
        Box::pin(async move {
            let req: NestResolveRequest = decode(&payload).map_err(malformed)?;
            let url = discovery_core::resolve_nest_core(&req.domain)
                .await
                .map_err(discovery_error_to_rpc)?;
            encode_reply(&NestResolveReply {
                url,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.actor.by_handle (≡ GET /api/v1/actor/by-handle/{handle}) ───────────

fn actor_by_handle_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: ActorByHandleRequest = decode(&payload).map_err(malformed)?;
            let res =
                discovery_core::resolve_handle_core(&state, &req.handle, req.domain.as_deref())
                    .await
                    .map_err(discovery_error_to_rpc)?;
            encode_reply(&ActorByHandleReply {
                actor_id: hex::encode(res.actor_id),
                handle: res.handle,
                domain: res.domain,
                addresses: res.addresses,
                addressable: res.addressable,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.setup.status (≡ GET /api/v1/setup-status) ─────────────────────────

fn setup_status_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            let _req: SetupStatusRequest = decode(&payload).map_err(malformed)?;
            // OS-LEAK (2026-06-28 second-pass review): the host-OS `os_*` patch-state
            // fields are admin-only (see `setup_status_core`). `setup.status` is a
            // pre-identity kind reachable anonymously, so gate them on the
            // bearer-bound connection actor being an admin; the all-zero anonymous
            // actor and any non-admin user resolve to `false` (→ "nothing pending").
            let caller_is_admin = state.db.is_admin(&actor[..]).await.unwrap_or(false);
            let s = discovery_core::setup_status_core(&state, caller_is_admin).await;
            encode_reply(&SetupStatusReply {
                domain: s.domain,
                dns_configured: s.dns_configured,
                tls_active: s.tls_active,
                email_enabled: s.email_enabled,
                admin_exists: s.admin_exists,
                claimed: s.claimed,
                node_mode: s.node_mode.as_str().to_string(),
                version: s.version.to_string(),
                mail_subsystem_ok: s.mail_subsystem_ok,
                auto_enable_mail_for_new_users: s.auto_enable_mail_for_new_users,
                registration_mode: s.registration_mode,
                max_free_users: s.max_free_users,
                subhandles: s.subhandles,
                age_verification_required: s.age_verification_required,
                max_storage_bytes: s.max_storage_bytes,
                cors_origins: s.cors_origins,
                serving_port: s.serving_port,
                fronted_by_router: s.fronted_by_router,
                dkim_records: s.dkim_records,
                os_security_updates_pending: s.os_security_updates_pending,
                os_reboot_pending: s.os_reboot_pending,
                os_reboot_deferred_since: s.os_reboot_deferred_since,
                os_last_patched_at: s.os_last_patched_at,
                web_app_origin: s.web_app_origin.mode,
                web_app_origin_target: s.web_app_origin.redirect_target,
                web_app_origin_domainless: s.web_app_origin.domainless,
                extra: Default::default(),
            })
        })
    })
}

/// Register the pre-identity public-discovery kinds. All five are replay-safe
/// pure reads at 5 s (no side effects). See `KindRegistry::register_discovery_kinds`
/// for the client-side metadata twin.
pub fn register_discovery_handlers(b: &mut RpcRouterBuilder) {
    let read = |handler| RpcKindMeta {
        forbid_replay: false,
        default_deadline: Duration::from_secs(5),
        handler,
    };
    b.add("fauna.nest.info", read(nest_info_handler()));
    b.add("fauna.handle.available", read(handle_available_handler()));
    b.add("fauna.nest.resolve", read(nest_resolve_handler()));
    b.add("fauna.actor.by_handle", read(actor_by_handle_handler()));
    b.add("fauna.setup.status", read(setup_status_handler()));
}
