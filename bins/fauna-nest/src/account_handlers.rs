//! Account WS-RPC handlers. Two registration entry points:
//!
//! - [`register_account_handlers`] — the single **pre-identity** kind
//!   `fauna.account.register`, the **sole** registration transport (the
//!   `POST /api/v1/register` HTTP twin was retired in S4f); the ceremony
//!   lives in the shared `account_core`. It runs
//!   **only** on the anonymous WS connection (`GET /api/v1/ws`, no bearer) —
//!   enforced by `pre_identity_allowlist` + the dispatcher gate in
//!   `routes::dispatch_request`. Track A3 of
//!   the WS-RPC-everywhere migration (tracked internally). Mirrors `auth_handlers` /
//!   `discovery_handlers`.
//! - [`register_account_user_handlers`] — the **authenticated** account surface
//!   on the bearer router: `fauna.account.{get,delete,upgrade,am_i_admin}`,
//!   `fauna.quota.get`, `fauna.profile.handle.change`. A behavior-preserving
//!   migration of the bearer-authed HTTP twins. Track B1 of
//!   the WS-RPC-everywhere migration (tracked internally). See the section below.
//!
//! **`register` (pre-identity):** the connection's bearer-actor is irrelevant
//! (there is none) — the handler authenticates the registering actor from the
//! **request payload** via the signed message, exactly as the HTTP twin did, so
//! the dispatcher's `actor_id` argument is ignored. Error codes
//! (`fauna.account.*`): `registration_closed`, `invalid_request` (detail = the
//! field hint), `signature_failed`, `invite_required`, `free_limit_reached`,
//! `actor_exists`, `handle_taken`, `handle_cooldown` — plus the auth
//! ceremonies' `fauna.auth.superseded` for a retired key
//! (`auth_core::successor_of`).
//!
//! **Authenticated surface (bearer):** the calling actor IS the connection
//! `actor_id`. Error codes are scoped per kind namespace
//! (`fauna.{account,quota,profile}.{not_found,invalid_request,permission_denied,internal}`,
//! plus `fauna.profile.{handle_taken,handle_cooldown}`).
//!
//! Both surfaces map malformed payloads + server faults to the `fauna.protocol.*`
//! infra codes.

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::account::{
    AccountDeleteReply, AccountDeleteRequest, AccountDeviceLimit, AccountEviction, AccountGetQuota,
    AccountGetReply, AccountGetRequest, AccountLockoutReply, AccountLockoutRequest,
    AccountNodePolicy, AmIAdminReply, AmIAdminRequest, ChangeHandleReply, ChangeHandleRequest,
    QuotaDeviceUsage, QuotaFeatures, QuotaGetReply, QuotaGetRequest, RegisterReply,
    RegisterRequest, UpgradeReply, UpgradeRequest, UsageBytes,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode};

use crate::account_core::{self, LockoutError, RegisterError};
use crate::pending_actions::ActionType;
use crate::registration::validate_handle;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

/// Map the transport-agnostic `RegisterError` to an `RpcError` — the WS-RPC
/// counterpart of the HTTP status mapping the retired `registration::post_register`
/// twin once did.
fn register_error_to_rpc(e: RegisterError) -> RpcError {
    match e {
        RegisterError::RegistrationClosed => RpcError::new(
            "fauna.account.registration_closed",
            "error.account.registration_closed",
        ),
        RegisterError::InvalidRequest(reason) => {
            let mut r = RpcError::new(
                "fauna.account.invalid_request",
                "error.account.invalid_request",
            );
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        RegisterError::SignatureFailed => RpcError::new(
            "fauna.account.signature_failed",
            "error.account.signature_failed",
        ),
        RegisterError::InviteRequired => RpcError::new(
            "fauna.account.invite_required",
            "error.account.invite_required",
        ),
        RegisterError::FreeLimitReached => RpcError::new(
            "fauna.account.free_limit_reached",
            "error.account.free_limit_reached",
        ),
        RegisterError::ActorAlreadyRegistered => {
            RpcError::new("fauna.account.actor_exists", "error.account.actor_exists")
        }
        RegisterError::Superseded { new_actor_id } => RpcError::superseded(&new_actor_id),
        RegisterError::HandleTaken => {
            RpcError::new("fauna.account.handle_taken", "error.account.handle_taken")
        }
        RegisterError::HandleCooldown => RpcError::new(
            "fauna.account.handle_cooldown",
            "error.account.handle_cooldown",
        ),
        RegisterError::AgeVerificationRequired => RpcError::new(
            "fauna.account.age_verification_required",
            "error.account.age_verification_required",
        ),
        RegisterError::GuardianAdmissionRequired => RpcError::new(
            "fauna.account.guardian_admission_required",
            "error.account.guardian_admission_required",
        ),
        RegisterError::AgeAttestationInvalid(reason) => {
            let mut r = RpcError::new(
                "fauna.account.age_attestation_invalid",
                "error.account.age_attestation_invalid",
            );
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        RegisterError::Internal(msg) => {
            let mut r = RpcError::new("fauna.protocol.internal", "error.protocol.internal");
            r.details = Some(Box::new(Value::String(msg)));
            r
        }
    }
}

// ── fauna.account.register (≡ POST /api/v1/register) ────────────────────────

fn register_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: RegisterRequest = decode(&payload).map_err(malformed)?;
            let outcome = account_core::register_core(
                &state,
                &req.actor_id,
                &req.handle,
                req.timestamp,
                &req.signature,
                req.invite_code.as_deref(),
                req.age_claim.as_ref(),
            )
            .await
            .map_err(register_error_to_rpc)?;
            encode_reply(&RegisterReply {
                actor_id: hex::encode(outcome.actor_id),
                handle: outcome.handle,
                domain: outcome.domain,
                tier: outcome.tier,
                node_url: outcome.node_url,
                addresses: outcome.addresses,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.account.age_nonce ──────────────────────────────────────────────────

/// Mint the single-use nonce an attested age claim commits to
/// (`family-safety.md` § The account age band; contract:
/// `fauna_protocol::age::age_claim_signed_message`). Anonymous pre-identity
/// kind — the registering actor has no account yet; the actor binding lives
/// inside the platform-signed payload, not here. Throttled like every
/// anonymous kind (`pre_identity_allowlist::is_throttled_anonymous_kind`).
/// The reply lists the platforms this build can verify
/// (`age_attest::attestation_platforms`), so the app attaches nothing the
/// nest would ignore.
fn age_nonce_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let _req: fauna_protocol::age::AgeNonceRequest = decode(&payload).map_err(malformed)?;
            let (nonce, expires_in_secs) = state.auth.age_nonce_store.issue().await;
            encode_reply(&fauna_protocol::age::AgeNonceReply {
                nonce: hex::encode(nonce),
                expires_in_secs,
                attestation_platforms: crate::age_attest::attestation_platforms(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.account.lockout (≡ POST /api/v1/account/lockout) ──────────────────

/// Map the transport-agnostic `LockoutError` to an `RpcError` — the WS-RPC
/// counterpart of the HTTP status mapping the retired `session_routes::lockout`
/// twin once did. Reuses the `fauna.account.*` codes (`invalid_request` /
/// `signature_failed`) the `register` mapper already defines.
fn lockout_error_to_rpc(e: LockoutError) -> RpcError {
    match e {
        LockoutError::InvalidRequest(reason) => {
            let mut r = RpcError::new(
                "fauna.account.invalid_request",
                "error.account.invalid_request",
            );
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        LockoutError::SignatureFailed => RpcError::new(
            "fauna.account.signature_failed",
            "error.account.signature_failed",
        ),
        LockoutError::Internal(msg) => {
            let mut r = RpcError::new("fauna.protocol.internal", "error.protocol.internal");
            r.details = Some(Box::new(Value::String(msg)));
            r
        }
        // The same shared refusal the auth ceremonies emit — one code and one
        // shape across every seed-signature surface, so a client renders the
        // "import the successor" affordance from one match arm.
        LockoutError::Superseded { new_actor_id } => RpcError::superseded(&new_actor_id),
    }
}

/// Emergency no-token lockout. Like `register`, the anonymous connection binds
/// no actor — the handler authenticates the actor from the **signed payload**
/// (Ed25519 over `actor_id ‖ timestamp_be`), so the dispatcher's `actor_id`
/// argument is ignored.
fn lockout_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: AccountLockoutRequest = decode(&payload).map_err(malformed)?;
            // The window is the hard-coded `EMERGENCY_LOCKOUT_SECS`; the
            // request carries no duration
            // (`account_core::EMERGENCY_LOCKOUT_SECS`'s rationale).
            let locked_until =
                account_core::lockout_core(&state, &req.actor_id, req.timestamp, &req.signature)
                    .await
                    .map_err(lockout_error_to_rpc)?;
            encode_reply(&AccountLockoutReply {
                ok: true,
                locked_until,
                extra: Default::default(),
            })
        })
    })
}

/// Register the pre-identity account kinds — `fauna.account.register` and
/// `fauna.account.lockout`, both on the **anonymous** connection
/// (`pre_identity_allowlist`). `register` is `forbid_replay = false` @30 s (a
/// write with a DB transaction + spawned DNS, matching `fauna.posts.create`);
/// `lockout` is `forbid_replay = false` @5 s (a quick revoke + `set_locked_until`
/// write). Both are replay-safe **for what `forbid_replay` governs — a client's
/// blind auto-retry after a reconnect, which re-sends the identical bytes**:
/// register's conflict checks refuse the second run, a re-lock re-applies the
/// same protective window, and the per-connection idempotency cache replays the
/// first reply on a recovered connection. On-path *attacker* replay is a
/// different question this flag never answered: the lockout signature covers
/// `actor_id ‖ timestamp_be` only, which is exactly why the lock window is the
/// hard-coded `account_core::EMERGENCY_LOCKOUT_SECS` and never a wire value
/// (before that ruling a captured 1-hour lockout
/// replayed inside the ±300 s window escalated to 24 hours via the unsigned
/// `duration_secs`, which has since left the wire). See `KindRegistry::register_account_{register,lockout}_kind` for
/// the metadata twins.
pub fn register_account_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.account.register",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: register_handler(),
        },
    );
    b.add(
        "fauna.account.age_nonce",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: age_nonce_handler(),
        },
    );
    b.add(
        "fauna.account.lockout",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: lockout_handler(),
        },
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Authenticated account surface (bearer connection) — Track B1 of
// the WS-RPC-everywhere migration (tracked internally). A behavior-preserving transport
// migration of the bearer-authed HTTP routes (`account_routes::get_account`,
// `quota_routes::get_quota`, `routes::am_i_admin`, and the `registration.rs`
// trio `put_handle` / `post_upgrade` / `delete_account`). Each handler reuses
// the same `CacheDb` methods the twin called; the connection `actor_id`
// replaces the HTTP `{actor}` path-param + bearer-match. Caller class is
// `User | Admin` (an admin manages their own account too) — see
// `bridge_method_allowlist::is_permitted`.
//
// `/api/v1/export` is **not** migrated here — a zstd-tar byte download, HTTP
// residue per `api-layers.md` § HTTP residue.
// ─────────────────────────────────────────────────────────────────────────────

/// Per-namespace `*.internal` error carrying the underlying cause as detail.
fn internal(ns: &str, err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(ns, err)
}

/// Per-namespace `*.not_found` (the twin's HTTP 404 "user not found").
fn not_found(ns: &str) -> RpcError {
    crate::rpc_errors::not_found_ns(ns, "user not found")
}

/// Per-namespace `*.invalid_request` (the twin's HTTP 400) carrying the reason.
fn invalid_request(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_request_ns(ns, reason)
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm. `ns` scopes the `permission_denied` code to the kind's namespace.
async fn require_permission(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    kind: &str,
    ns: &str,
) -> Result<(), RpcError> {
    crate::bridge_method_allowlist::require_permission(&state.db, actor_id, kind, |e| {
        internal(ns, e)
    })
    .await?;
    Ok(())
}

// ── fauna.account.get (≡ GET /api/v1/account) ───────────────────────────────

fn account_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.account.get", "account").await?;
            let _req: AccountGetRequest = decode(&payload).map_err(malformed)?;

            let user = state
                .db
                .get_user(&actor_id)
                .await
                .map_err(|e| internal("account", e))?
                .ok_or_else(|| not_found("account"))?;

            // Best-effort like the twin (`unwrap_or(None)`): a missing handle /
            // tier row degrades the reply rather than failing the read.
            let handle = state.db.get_handle(&actor_id).await.unwrap_or(None);
            let tier_limits = state.db.get_tier(&user.tier).await.ok().flatten();

            let eviction = if !user.eviction_status.is_empty() {
                let export_token = state
                    .db
                    .get_eviction_token_for_actor(&actor_id)
                    .await
                    .unwrap_or(None);
                Some(AccountEviction {
                    status: user.eviction_status,
                    reason: user.eviction_reason,
                    category: user.eviction_category,
                    warned_at: user.eviction_warned_at,
                    suspend_at: user.eviction_suspend_at,
                    delete_at: user.eviction_delete_at,
                    export_token,
                    extra: Default::default(),
                })
            } else {
                None
            };

            let (max_inbox, max_storage, max_devices) = tier_limits
                .map(|t| (t.max_inbox_bytes, t.max_storage_bytes, t.max_devices))
                .unwrap_or((0, 0, 0));

            encode_reply(&AccountGetReply {
                actor_id: hex::encode(&user.actor_id),
                handle,
                tier: user.tier,
                created_at: user.created_at,
                eviction,
                quota: AccountGetQuota {
                    inbox: UsageBytes {
                        used_bytes: user.inbox_bytes_used,
                        max_bytes: max_inbox,
                        extra: Default::default(),
                    },
                    storage: UsageBytes {
                        used_bytes: user.storage_bytes_used,
                        max_bytes: max_storage,
                        extra: Default::default(),
                    },
                    devices: AccountDeviceLimit {
                        max: max_devices,
                        extra: Default::default(),
                    },
                    extra: Default::default(),
                },
                node_policy: AccountNodePolicy {
                    eviction_warning_days: fauna_protocol::node_policy::EVICTION_WARNING_DAYS,
                    eviction_suspension_days: fauna_protocol::node_policy::EVICTION_SUSPENSION_DAYS,
                    extra: Default::default(),
                },
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.quota.get (≡ GET /api/v1/quota) ────────────────────────────────────

fn quota_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.quota.get", "quota").await?;
            let _req: QuotaGetRequest = decode(&payload).map_err(malformed)?;

            let user = state
                .db
                .get_user(&actor_id)
                .await
                .map_err(|e| internal("quota", e))?
                .ok_or_else(|| not_found("quota"))?;
            let tier = state
                .db
                .get_tier(&user.tier)
                .await
                .map_err(|e| internal("quota", e))?
                .ok_or_else(|| internal("quota", "tier not found"))?;
            let device_count = state.db.count_sync_devices(&actor_id).await.unwrap_or(0);

            // The twin's feature flags are derived from the tier name; compute
            // before moving `user.tier` into the reply.
            let is_paid = user.tier != "free";

            encode_reply(&QuotaGetReply {
                tier: user.tier,
                inbox: UsageBytes {
                    used_bytes: user.inbox_bytes_used,
                    max_bytes: tier.max_inbox_bytes,
                    extra: Default::default(),
                },
                storage: UsageBytes {
                    used_bytes: user.storage_bytes_used,
                    max_bytes: tier.max_storage_bytes,
                    extra: Default::default(),
                },
                devices: QuotaDeviceUsage {
                    used: device_count,
                    max: tier.max_devices,
                    extra: Default::default(),
                },
                features: QuotaFeatures {
                    versioned_backup: is_paid,
                    bridges: is_paid,
                    max_feeds: tier.max_feeds,
                    extra: Default::default(),
                },
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.account.am_i_admin (≡ GET /api/v1/am-i-admin) ──────────────────────

fn am_i_admin_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.account.am_i_admin", "account").await?;
            let _req: AmIAdminRequest = decode(&payload).map_err(malformed)?;
            // Mirror the twin's `unwrap_or(false)` — a db hiccup reports
            // non-admin rather than failing the gate read.
            let admin = state.db.is_admin(&actor_id).await.unwrap_or(false);
            encode_reply(&AmIAdminReply {
                admin,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.profile.handle.change (≡ PUT /api/v1/profile/handle) ───────────────

fn change_handle_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.profile.handle.change", "profile").await?;
            let req: ChangeHandleRequest = decode(&payload).map_err(malformed)?;

            validate_handle(&req.handle).map_err(|m| invalid_request("profile", m))?;
            if state
                .auth
                .registration
                .reserved_handles
                .iter()
                .any(|r| r == &req.handle)
            {
                return Err(invalid_request("profile", "handle is reserved"));
            }

            // Cooldown — another actor may still own this handle.
            match state.db.check_handle_cooldown(&req.handle, &actor_id).await {
                Ok(false) => {
                    let mut e = RpcError::new(
                        "fauna.profile.handle_cooldown",
                        "error.profile.handle_cooldown",
                    );
                    e.details = Some(Box::new(Value::String("handle is in cooldown".into())));
                    return Err(e);
                }
                Err(e) => return Err(internal("profile", e)),
                _ => {}
            }

            // Not already taken by another actor.
            match state.db.resolve_handle(&req.handle).await {
                Ok(Some(existing)) if existing != actor_id => {
                    let mut e =
                        RpcError::new("fauna.profile.handle_taken", "error.profile.handle_taken");
                    e.details = Some(Box::new(Value::String("handle already taken".into())));
                    return Err(e);
                }
                Err(e) => return Err(internal("profile", e)),
                _ => {}
            }

            // `target` = the requested handle: the list SUMMARY is what the
            // apps' standing pending-actions section describes rows from
            // (`ui/settings.md` § Pending actions — "verb + target, e.g. the
            // new handle"), and the summary deliberately omits `payload`, so
            // a None target left every app describing this row as a bare
            // "handle.change". The snapshot-delete creator already passes its
            // target; the executor reads only `payload` either way.
            let payload_json = serde_json::json!({ "new_handle": req.handle }).to_string();
            let row = crate::pending_actions::schedule(
                &state,
                &ActionType::HandleChange,
                &actor_id,
                Some(&req.handle),
                Some(&payload_json),
            )
            .await
            .map_err(|e| internal("profile", e))?;
            let action_id = row.id;

            tracing::info!(
                actor = hex::encode(actor_id),
                new_handle = %req.handle,
                "handle change scheduled (WS-RPC)"
            );

            encode_reply(&ChangeHandleReply {
                pending_action_id: action_id,
                execute_after: row.execute_after,
                status: "pending".into(),
                new_handle: req.handle,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.account.upgrade (≡ POST /api/v1/upgrade) ───────────────────────────

fn account_upgrade_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.account.upgrade", "account").await?;
            let req: UpgradeRequest = decode(&payload).map_err(malformed)?;

            let user = state
                .db
                .get_user(&actor_id)
                .await
                .map_err(|e| internal("account", e))?
                .ok_or_else(|| not_found("account"))?;

            // Simple tier ordering: free < personal < community.
            let tier_order = |t: &str| -> u8 {
                match t {
                    "free" => 0,
                    "personal" => 1,
                    "community" => 2,
                    _ => 3,
                }
            };
            // Check tier ordering BEFORE consuming the invite code.
            if tier_order(&req.tier) <= tier_order(&user.tier) {
                return Err(invalid_request(
                    "account",
                    "can only upgrade to a higher tier",
                ));
            }

            let code_tier = match state.db.validate_invite_code(&req.invite_code).await {
                // A supervised (guardian-carrying) code admits NEW accounts
                // only — consuming it as a tier-upgrade token would silently
                // drop the guardianship designation (family-safety.md § Wire
                // & data shape). The band rides only beside a guardian
                // (mint-validated), so this arm covers banded codes too.
                Ok(Some(grant)) if grant.guardian_actor.is_some() => {
                    return Err(invalid_request(
                        "account",
                        "supervised invite codes admit new accounts only",
                    ));
                }
                Ok(Some(grant)) => grant.tier,
                Ok(None) => {
                    return Err(invalid_request("account", "invalid or expired invite code"));
                }
                Err(e) => return Err(internal("account", e)),
            };
            if code_tier != req.tier {
                return Err(invalid_request("account", "invite code tier mismatch"));
            }

            state
                .db
                .update_user(&actor_id, &req.tier, &user.label)
                .await
                .map_err(|e| internal("account", e))?;

            tracing::info!(actor = hex::encode(actor_id), tier = %req.tier, "user upgraded (WS-RPC)");

            encode_reply(&UpgradeReply {
                ok: true,
                tier: req.tier,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.account.delete (≡ DELETE /api/v1/account) ──────────────────────────

fn account_delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.account.delete", "account").await?;
            let _req: AccountDeleteRequest = decode(&payload).map_err(malformed)?;

            // Admin lifecycle gate (common.md § Client-state recoverability).
            // `delete_user` drops the `users` row but not the actor's
            // `admin_actor_ids` row, and the claim gate keys on
            // `admin_count > 0` — so an admin who self-deletes leaves a box that
            // reports *claimed* while nobody can authenticate as its admin
            // (`check_actor_active` refuses an actor with no `users` row once the
            // bearer expires). Off-box brick; make it unrepresentable rather than
            // detect-and-repair, exactly as `require_not_admin` does for
            // suspend/evict.
            //
            // A *sole* superadmin cannot demote themselves either
            // (the superadmin floor — `can_remove_admin` — refuses the last
            // superadmin), so their exit is
            // `fauna.admin.factory_reset` — the universal recovery floor, which
            // returns the box to fresh/unclaimed and is itself client-driven.
            if state
                .db
                .is_admin(&actor_id[..])
                .await
                .map_err(|e| internal("account", e))?
                // A retired identity of this account holding an admin row is
                // the same gate: the deletion takes the local chain with it,
                // and that row would leave without `admin.remove`'s floor.
                || !state
                    .db
                    .local_predecessors_holding_admin(&actor_id)
                    .await
                    .map_err(|e| internal("account", e))?
                    .is_empty()
            {
                let mut e = RpcError::new(
                    "fauna.account.admin_role_held",
                    "error.account.admin_role_held",
                );
                e.details = Some(Box::new(Value::String(
                    "this account holds the admin role — remove it (fauna.admin.admins.remove) \
                     before deleting. A sole superadmin cannot be demoted; use \
                     fauna.admin.factory_reset to return the nest to fresh/unclaimed instead."
                        .into(),
                )));
                return Err(e);
            }

            // Room-ownership gate (conversation-rooms.md § Roles and
            // authorization, the owner rule). Deleting the owner of a
            // ceremony-born room purges the owner seat and keeps the room, so
            // appoint, demote and transfer would be gone for the life of the
            // room while a transfer to a member who could take over was one
            // act away. Refused only where that transfer exists: an owner with
            // nobody to hand the room to is let go, since a refusal nothing
            // can lift is its own unrecoverable state. The executor re-checks
            // (`pending_actions::finalize_self_deletion`).
            let held = state
                .db
                .rooms_awaiting_an_ownership_transfer(&actor_id)
                .await
                .map_err(|e| internal("account", e))?;
            if !held.is_empty() {
                let mut e = RpcError::new(
                    "fauna.account.room_ownership_held",
                    "error.account.room_ownership_held",
                );
                e.details = Some(Box::new(Value::String(format!(
                    "this account owns {} room(s) another member could take over — transfer \
                     ownership (fauna.conversations.room.transfer_ownership) before deleting",
                    held.len()
                ))));
                return Err(e);
            }

            // Family-safety lifecycle gates (family-safety.md § Lifecycle
            // gates). A SUPERVISED account cannot self-delete — that would
            // unilaterally sever the guardianship link (the one bounded
            // exception to user-controls-their-data); the guardian graduates
            // first. A GUARDIAN cannot self-delete while links reference it —
            // resolve each via transfer or graduation first.
            if state
                .db
                .get_guardian_of(&actor_id)
                .await
                .map_err(|e| internal("account", e))?
                .is_some()
            {
                return Err(crate::rpc_errors::guardian_approval_required_ns(
                    "account",
                    "this account is supervised — your guardian must graduate it before it can be deleted",
                ));
            }
            let wards = state
                .db
                .list_wards(&actor_id)
                .await
                .map_err(|e| internal("account", e))?;
            if !wards.is_empty() {
                let mut e = RpcError::new(
                    "fauna.account.guardianships_unresolved",
                    "error.account.guardianships_unresolved",
                );
                e.details = Some(Box::new(Value::String(format!(
                    "this account guards {} supervised account(s) — transfer or graduate each (fauna.family.transfer / fauna.family.graduate) first",
                    wards.len()
                ))));
                return Err(e);
            }

            let row = crate::pending_actions::schedule(
                &state,
                &ActionType::AccountDelete,
                &actor_id,
                None,
                None,
            )
            .await
            .map_err(|e| internal("account", e))?;
            let action_id = row.id;

            tracing::info!(
                actor = hex::encode(actor_id),
                "account deletion scheduled (WS-RPC)"
            );

            encode_reply(&AccountDeleteReply {
                pending_action_id: action_id,
                execute_after: row.execute_after,
                status: "pending".into(),
                message: "account deletion scheduled".into(),
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ───────────────────────────────────

/// Register the authenticated account surface on the **bearer** router. Per-kind
/// replay semantics + rationale: see `KindRegistry::register_account_kinds`. The
/// three reads + the two pending-action creators are `forbid_replay = false`
/// @5 s; `account.upgrade` is `forbid_replay = false` @30 s (invite-consuming
/// write, protected by the per-connection idempotency cache).
pub fn register_account_user_handlers(b: &mut RpcRouterBuilder) {
    let read = |handler| RpcKindMeta {
        forbid_replay: false,
        default_deadline: Duration::from_secs(5),
        handler,
    };
    b.add("fauna.account.get", read(account_get_handler()));
    b.add("fauna.quota.get", read(quota_get_handler()));
    b.add("fauna.account.am_i_admin", read(am_i_admin_handler()));
    b.add("fauna.profile.handle.change", read(change_handle_handler()));
    b.add("fauna.account.delete", read(account_delete_handler()));
    b.add(
        "fauna.account.upgrade",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: account_upgrade_handler(),
        },
    );
}
