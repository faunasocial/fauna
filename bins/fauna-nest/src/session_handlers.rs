//! Session-management WS-RPC handlers (bearer connection) — part of the
//! WS-RPC-everywhere migration (tracked internally). A behavior-preserving
//! transport migration of the bearer-authed session routes
//! (`session_routes::{list_sessions, revoke_session, revoke_all_sessions}`)
//! plus an authed variant of the emergency lockout.
//!
//! Four kinds:
//!
//! - `fauna.sessions.list` — `TokenStore::list_sessions`.
//! - `fauna.sessions.revoke` — ownership check + `revoke_session_authority`.
//! - `fauna.sessions.revoke_all` — `revoke_other_sessions_authority` (the
//!   client names its own current session by the `token_id` it learned at mint;
//!   the WS connection has no raw bearer to infer "current" from — see
//!   `sessions.rs`).
//!
//! Both revoke arms do **two halves**: the token row and the sockets that
//! bearer already opened. Until 2026-09-20 they did the first alone, which —
//! since the bearer is validated once, at the upgrade — governed only the
//! revoked session's *next* connection and left the one it held dispatching as
//! `User` (`transport-connection.md` § Connection lifecycle → *Revocation
//! teardown*; `devices.md` § What a session is, and what revoking one does).
//! - `fauna.sessions.lockout` — authed emergency lockout: `revoke_actor` +
//!   `set_locked_until`, no signature (the connection authenticates the actor),
//!   then every socket of the actor closed through the caller-sparing form, so
//!   the app that pressed the lock still hears that it landed.
//!   The **no-token** Ed25519 recovery channel is the pre-identity kind
//!   `fauna.account.lockout` (`account_handlers`; its HTTP twin is deleted —
//!   `api-layers.md` § Sessions). Both kinds lock for the shared hard-coded
//!   `account_core::EMERGENCY_LOCKOUT_SECS` window.
//!
//! Gate `User | Admin` (an admin manages their own sessions too) — enforced in
//! `bridge_method_allowlist::is_permitted`.

use std::time::Duration;

use fauna_core::identity::ActorId;
use fauna_protocol::sessions::{
    LockoutReply, LockoutRequest, RevokeAllReply, RevokeAllRequest, RevokeReply, RevokeRequest,
    SessionInfo, SessionsListReply, SessionsListRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for every `fauna.sessions.*` code.
const NS: &str = "sessions";

// ── Helpers (mirroring `push_handlers`, scoped to `sessions`) ────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

/// The twin's "session not found" (`revoke_session` returned 404 when the
/// `token_id` was not one of the caller's).
fn not_found() -> RpcError {
    crate::rpc_errors::not_found_ns(NS, "session not found")
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

// ── fauna.sessions.list (≡ GET /api/v1/account/sessions) ─────────────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sessions.list").await?;
            let _req: SessionsListRequest = decode(&payload).map_err(malformed)?;

            let sessions = state
                .auth
                .token_store
                .list_sessions(&ActorId(actor_id))
                .await
                .into_iter()
                .map(|s| SessionInfo {
                    token_id: s.token_id,
                    created_at: s.created_at,
                    expires_at: s.expires_at,
                    ip_address: s.ip_address,
                    last_used_at: s.last_used_at,
                    minted_by_device: s.minted_by_device.map(|k| fauna_core::hex32::encode(&k)),
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&SessionsListReply {
                sessions,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sessions.revoke (≡ DELETE /api/v1/account/sessions/{token_id}) ─────

fn revoke_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sessions.revoke").await?;
            let req: RevokeRequest = decode(&payload).map_err(malformed)?;

            // Verify the token_id belongs to this actor before revoking
            // (mirrors `session_routes::revoke_session`).
            let owned = state
                .auth
                .token_store
                .list_sessions(&ActorId(actor_id))
                .await
                .iter()
                .any(|s| s.token_id == req.token_id);
            if !owned {
                return Err(not_found());
            }
            // Both halves — the token row AND the sockets that bearer already
            // opened. The bearer is validated once, at the upgrade, so the
            // revoke alone would govern only the session's *next* connection
            // and leave the one it holds dispatching as `User`
            // (`transport-connection.md` § Connection lifecycle → *Revocation
            // teardown*). The caller's own Reply survives its own teardown, for
            // the case this kind meets constantly: the session being ended is
            // the connection asking.
            state
                .revoke_session_authority(&actor_id, &req.token_id)
                .await;
            encode_reply(&RevokeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sessions.revoke_all (≡ POST /api/v1/account/sessions/revoke-all) ──

fn revoke_all_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sessions.revoke_all").await?;
            let req: RevokeAllRequest = decode(&payload).map_err(malformed)?;

            // Both halves, as in the `revoke` arm. A `keep_token_id` matching
            // no session of this actor's ends them all — the caller's
            // included; that is the renewal race `devices.md` § The client's
            // own session accepts, and the grace is what keeps the `revoked`
            // count reaching the app through it.
            let revoked = state
                .revoke_other_sessions_authority(&actor_id, &req.keep_token_id)
                .await;
            encode_reply(&RevokeAllReply {
                ok: true,
                revoked: revoked as u64,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sessions.lockout (authed variant of POST /api/v1/account/lockout) ──

fn lockout_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sessions.lockout").await?;
            // The request carries no parameters; decoding still refuses a
            // malformed payload. The window is the hard-coded shared constant,
            // the same ruling as the pre-identity twin
            // (`account_core::EMERGENCY_LOCKOUT_SECS`; one constant on both
            // kinds keeps the twins from diverging).
            let _req: LockoutRequest = decode(&payload).map_err(malformed)?;
            let locked_until = (now_secs() + crate::account_core::EMERGENCY_LOCKOUT_SECS) as i64;

            let actor = ActorId(actor_id);
            state.auth.token_store.revoke_actor(&actor).await;
            state
                .db
                .set_locked_until(&actor_id, Some(locked_until))
                .await
                .map_err(internal)?;
            // Revoking tokens stops the *next* connection; this closes the ones
            // the actor already holds. Without it the emergency lockout leaves a
            // compromised session fully functional, which is the one thing it
            // exists to prevent (`transport-connection.md` § Revocation
            // teardown).
            //
            // Through the SPARING form: this kind is self-directed by
            // construction, so the connection this request arrived on is one of
            // the ones being closed, and its Reply is the app's only evidence
            // the lock committed (`ui/sessions.md` § User actions: on success
            // the app leaves its shell for the locked surface). That one
            // connection closes 4401 the instant its Reply is on the wire and
            // dispatches nothing further meanwhile; every other socket of the
            // actor — where a thief's is — closes at once, with no drain.
            state.revoke_actor_authority_sparing_caller(&actor_id).await;
            encode_reply(&LockoutReply {
                ok: true,
                locked_until,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ─────────────────────────────────────────────────

/// Register the session-management surface on the **bearer** router. Per-kind
/// replay semantics + rationale: see `KindRegistry::register_sessions_kinds`.
/// All four are `forbid_replay = false` @5 s (a read + idempotent mutations).
pub fn register_sessions_handlers(b: &mut RpcRouterBuilder) {
    for (kind, handler) in [
        ("fauna.sessions.list", list_handler()),
        ("fauna.sessions.revoke", revoke_handler()),
        ("fauna.sessions.revoke_all", revoke_all_handler()),
        ("fauna.sessions.lockout", lockout_handler()),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(5),
                handler,
            },
        );
    }
}
