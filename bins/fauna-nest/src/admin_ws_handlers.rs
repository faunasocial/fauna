//! Admin (Layer-5) WS-RPC handlers — the admin surface migrating
//! `/admin/api/*` onto the per-actor **bearer** WS-RPC connection (admin is a
//! Fauna app; product invariant: nest configuration is set from clients).
//! Track C of the WS-RPC-everywhere migration (tracked internally).
//!
//! This file opens the **user-management cluster (C1)** with the read surface:
//!
//! - `fauna.admin.users.list` ≡ GET `/admin/api/users?limit&offset` —
//!   `CacheDb::list_users_paginated` (paginated rows + unpaginated total).
//! - `fauna.admin.users.get` ≡ GET `/admin/api/users/{actor_id}` —
//!   `CacheDb::get_user` (one row, or `fauna.admin.not_found`).
//!
//! Behavior-preserving: the handlers reshape the same `UserRow`s the HTTP twins
//! (`admin::{list_users_paginated, get_user}`) emit — no shared core, one DB
//! call plus reply shaping (the `stats_handlers` pattern).
//!
//! **Admin-only.** The twins gate `AdminBearerAuth`; the kinds gate `Admin` in
//! `bridge_method_allowlist::is_permitted` (the `fauna.pending_actions.approve`
//! B20 admin precedent). The connection `actor_id` is used only for that gate,
//! never for data selection — the queried user is the request's `actor_id`.

use std::collections::BTreeMap;
use std::time::Duration;

use fauna_protocol::admin::{
    AdminAdminAddRequest,
    AdminAdminEntry,
    AdminAdminRemoveRequest,
    AdminAdminsListReply,
    AdminAdminsListRequest,
    AdminAuditEntry,
    AdminAuditIntegrityReply,
    AdminAuditIntegrityRequest,
    AdminAuditListReply,
    AdminAuditListRequest,
    AdminClusterStatusReply,
    AdminClusterStatusRequest,
    AdminDeploymentSeedGetReply,
    AdminDeploymentSeedGetRequest,
    AdminDeploymentSeedRotateReply,
    AdminDeploymentSeedRotateRequest,
    AdminEviction,
    AdminEvictionsListReply,
    AdminEvictionsListRequest,
    // C5 — folders / services.
    AdminFolderAddMemberRequest,
    AdminFolderCreateReply,
    AdminFolderCreateRequest,
    AdminFolderGetReply,
    AdminFolderGetRequest,
    AdminFolderMember,
    AdminGcReply,
    AdminGcRequest,
    AdminInviteCode,
    AdminInviteCodeCreateReply,
    AdminInviteCodeCreateRequest,
    AdminInviteCodeDeleteRequest,
    AdminInviteCodesListReply,
    AdminInviteCodesListRequest,
    AdminInviteRequest,
    AdminInviteRequestApproveReply,
    AdminInviteRequestApproveRequest,
    AdminInviteRequestDenyRequest,
    AdminInviteRequestsListReply,
    AdminInviteRequestsListRequest,
    AdminLogEntry,
    AdminLogLevel,
    AdminLogsReply,
    AdminLogsRequest,
    AdminMembershipTier,
    AdminMembershipTierClearRequest,
    AdminMembershipTierSetRequest,
    AdminMembershipTiersListReply,
    AdminMembershipTiersListRequest,
    AdminOkReply,
    AdminPendingActionReply,
    AdminPendingActionSummary,
    AdminPendingActionsListReply,
    AdminPendingActionsListRequest,
    AdminServiceFlags,
    AdminServiceUpdateReply,
    AdminServiceUpdateRequest,
    AdminServicesListReply,
    AdminServicesListRequest,
    AdminStatsReply,
    AdminStatsRequest,
    AdminStatusReply,
    AdminStatusRequest,
    AdminTier,
    AdminTierCreateRequest,
    AdminTierUpdateRequest,
    AdminTiersListReply,
    AdminTiersListRequest,
    AdminUpdateAvailable,
    AdminUser,
    AdminUserCancelEvictionRequest,
    AdminUserClearHandleRequest,
    AdminUserCreateRequest,
    AdminUserDeleteRequest,
    AdminUserEvictRequest,
    AdminUserGetReply,
    AdminUserGetRequest,
    AdminUserSuspendRequest,
    AdminUserUpdateRequest,
    AdminUsersListReply,
    AdminUsersListRequest,
    AdminWorkerInfo,
    AdminWorkerStatusReply,
    AdminWorkerStatusRequest,
    DEFAULT_LAPSE_TIER,
    FactoryResetReply,
    FactoryResetRequest,
};
use fauna_protocol::push_events::AccountUpdatedPayload;
use fauna_protocol::{ByteBuf, PushEvent, RpcError, Value, decode_strict as decode};

use fauna_core::secret::SecretString;
use zeroize::Zeroizing;

use crate::db::nest_rotation::RotationRefusal;
use crate::db::{InviteCodeRow, InviteRequestRow, TierRow, UserRow};
use crate::pending_actions::ActionType;
use crate::registration::validate_handle;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for every `fauna.admin.*` code.
const NS: &str = "admin";

use crate::rpc_errors::{encode_reply, malformed};

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

fn invalid_params(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_params_ns(NS, reason)
}

fn not_found(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::not_found_ns(NS, reason)
}

fn conflict(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::conflict_ns(NS, reason)
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `AdminBearerAuth` extractor gate.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// `UserRow` → the wire `AdminUser` (the twin's per-user JSON object). The
/// eviction sub-object is `Some` only when `eviction_status` is non-empty.
/// `mail_serving_enabled` is the read-only audit indicator the caller resolves
/// from `actor_mail_serving` (absent ⇒ on) — see [`list_handler`] (batch read)
/// and [`get_handler`] (single read). `is_admin` is resolved from
/// `admin_actor_ids` so the client can decline to offer the lifecycle controls
/// the nest refuses on an admin (`admin.md` § 2 → *Cutting a user off*).
fn user_to_wire(u: UserRow, mail_serving_enabled: bool, is_admin: bool) -> AdminUser {
    let eviction = (!u.eviction_status.is_empty()).then(|| AdminEviction {
        status: u.eviction_status,
        reason: u.eviction_reason,
        category: u.eviction_category,
        warned_at: u.eviction_warned_at,
        suspend_at: u.eviction_suspend_at,
        delete_at: u.eviction_delete_at,
        extra: Default::default(),
    });
    AdminUser {
        actor_id: ByteBuf::from(u.actor_id),
        tier: u.tier,
        label: u.label,
        // The DB stores the empty string for a handle-less admission (see the
        // conformance note on `create_without_a_handle_…`); the wire reports
        // absent, so no picker ever offers an empty option string.
        handle: u.handle.filter(|h| !h.is_empty()),
        suspended: u.suspended,
        created_at: u.created_at,
        inbox_bytes_used: u.inbox_bytes_used,
        storage_bytes_used: u.storage_bytes_used,
        eviction,
        mail_serving_enabled,
        is_admin,
        extra: Default::default(),
    }
}

/// Resolve a `UserRow`'s read-only serving + admin-role flags from the batch
/// reads the list-style handlers do once per page (no N-query fan-out).
fn user_to_wire_with_overrides(
    u: UserRow,
    overrides: &std::collections::HashMap<[u8; 32], bool>,
    admins: &std::collections::HashSet<Vec<u8>>,
) -> AdminUser {
    let serving = <[u8; 32]>::try_from(u.actor_id.as_slice())
        .ok()
        .and_then(|id| overrides.get(&id).copied())
        .unwrap_or(true);
    let is_admin = admins.contains(&u.actor_id);
    user_to_wire(u, serving, is_admin)
}

/// The set of actors holding the admin role, as one batch read — the companion
/// of `list_mail_serving_overrides` for the per-row `is_admin` flag.
async fn admin_actor_set(state: &AppState) -> Result<std::collections::HashSet<Vec<u8>>, RpcError> {
    Ok(state
        .db
        .list_admin_actors()
        .await
        .map_err(internal)?
        .into_iter()
        .map(|(actor_id, _added_at)| actor_id)
        .collect())
}

/// Wire `ByteBuf` actor id → `[u8; 32]` (the twin's `parse_actor_id` 400).
fn actor_id_from_wire(bytes: &ByteBuf) -> Result<[u8; 32], RpcError> {
    crate::rpc_errors::require_bytes32("actor_id", bytes.as_ref()).map_err(invalid_params)
}

/// Wire `ByteBuf` → `[u8; 32]` for a named field (the C5 folders twins'
/// `parse_hash` 400 — the HTTP twin took hex; the kind takes raw 32 bytes).
fn bytes32_from_wire(bytes: &ByteBuf, field: &str) -> Result<[u8; 32], RpcError> {
    crate::rpc_errors::require_bytes32(field, bytes.as_ref()).map_err(invalid_params)
}

/// Wall-clock seconds (the twins' `AccountUpdated` push timestamp + the
/// eviction-token `delete_at` fallback).
fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// Push `AccountUpdated{[change]}` to the affected user (the twins fire this on
/// handle clear + eviction start/cancel so the user's clients refresh).
fn push_account_updated(state: &AppState, target: &[u8; 32], change: &str) {
    state.ws.notify_push(
        target,
        PushEvent::AccountUpdated(AccountUpdatedPayload {
            changes: vec![change.to_string()],
            timestamp: now_secs(),
            extra: BTreeMap::new(),
        }),
    );
}

/// Map a write DB error: a SQLite `UNIQUE` violation → `fauna.admin.conflict`
/// (the twin's `409`), anything else → internal. The **full** error chain
/// (`{e:#}`) is matched because the `CacheDb` writers wrap the rusqlite error in
/// `.context(...)`, which hides the `UNIQUE` marker from `to_string()` — the C1
/// `create_user` / B14 folders latent-twin-bug precedent. (Several HTTP twins
/// inspected only `to_string()` and so silently `500`ed on a duplicate; the
/// kinds return the intended conflict.)
fn unique_conflict(e: anyhow::Error, conflict_msg: &'static str) -> RpcError {
    if format!("{e:#}").contains("UNIQUE") {
        conflict(conflict_msg)
    } else {
        internal(e)
    }
}

fn create_user_error(e: anyhow::Error) -> RpcError {
    unique_conflict(e, "user already exists")
}

/// `TierRow` → wire `AdminTier` (the twin's per-tier JSON object).
fn tier_to_wire(t: TierRow) -> AdminTier {
    AdminTier {
        name: t.name,
        max_inbox_bytes: t.max_inbox_bytes,
        max_storage_bytes: t.max_storage_bytes,
        max_devices: t.max_devices,
        max_blob_size: t.max_blob_size,
        max_feeds: t.max_feeds,
        extra: Default::default(),
    }
}

/// `InviteCodeRow` → wire `AdminInviteCode`.
fn invite_code_to_wire(c: InviteCodeRow) -> AdminInviteCode {
    AdminInviteCode {
        code: c.code,
        tier: c.tier,
        uses_left: c.uses_left,
        created_at: c.created_at,
        guardian_actor: c.guardian_actor.map(ByteBuf::from),
        age_band: c.age_band,
        extra: Default::default(),
    }
}

/// Refuse an admin as the target of suspension or eviction.
///
/// Both transitions reach the eviction machine's `suspended` state, and a
/// suspended actor loses its caller class entirely (`caller_class_for_actor`).
/// A suspended *sole* admin could therefore never be restored by anyone — an
/// off-box brick, which `docs/goal/architecture/nest/common.md`
/// § Client-state recoverability forbids. Rather than detect-and-repair, make
/// the state unrepresentable: demote first via `fauna.admin.admins.remove`
/// (itself guarded by the superadmin floor, `can_remove_admin` at the door
/// and the writer's own refusal at execution), then suspend or evict the
/// now-plain user. Self-targeting is the same brick, caught by the same check,
/// since `require_permission` has already established the caller is an admin.
///
/// **Deletion is guarded for a stricter reason.** Suspension is reversible
/// (`cancel_eviction`); deletion is not. Dropping an admin's `users` row leaves
/// its `admin_actor_ids` row orphaned — `delete_user` does not touch that table
/// — so the claim gate keeps reading `admin_count > 0` and answering
/// `already_claimed` while the ex-admin can no longer authenticate at all. The
/// executor carries the matching fail-safe for already-queued actions
/// (`pending_actions::finalize_user_deletion`), because a persisted pending
/// action can outlive the upgrade that added this guard.
async fn require_not_admin(state: &AppState, target: &[u8; 32]) -> Result<(), RpcError> {
    if state.db.is_admin(&target[..]).await.map_err(internal)? {
        return Err(conflict(
            "cannot suspend, evict, or delete an admin — remove the admin role first",
        ));
    }
    Ok(())
}

/// Deletion's chain-wide admin gate: a deletion takes the target's local
/// predecessors with it (their `admin_actor_ids` rows purged, their `users`
/// rows deleted), so a predecessor's admin row would leave without
/// `admin.remove`'s quorum or the superadmin floor. Refused at the door so the
/// admin hears it now; `finalize_user_deletion` carries the matching fail-safe
/// for already-queued actions.
async fn require_no_predecessor_admin(state: &AppState, target: &[u8; 32]) -> Result<(), RpcError> {
    if !state
        .db
        .local_predecessors_holding_admin(target)
        .await
        .map_err(internal)?
        .is_empty()
    {
        return Err(conflict(
            "a retired identity of this account still holds the admin role — remove it first",
        ));
    }
    Ok(())
}

/// `Admin ⊇ User` at the *add* door (`../architecture/api-layers.md` § `Admin ⊇ User`):
/// an admin must first be a registered user.
///
/// `add_admin_actor` touches only `admin_actor_ids`, so promoting an actor with no
/// `users` row minted an admin that `caller_class_for_actor` resolves (admins are
/// resolved before the row lookup) but that owns no account — invisible to every
/// suspension, eviction, and deletion guard, which all reason over `users`. That is
/// the orphaned-admin brick approached from the other side: the *delete* door was
/// closed in `fix(nest): an admin cannot be deleted…`; this is its twin at the
/// *add* door. The executor carries the matching fail-safe, because a pending
/// action can outlive the upgrade that added this guard.
async fn require_registered_user(state: &AppState, target: &[u8; 32]) -> Result<(), RpcError> {
    if state.db.get_user(target).await.map_err(internal)?.is_none() {
        return Err(not_found(
            "cannot grant the admin role to an actor that is not a registered user",
        ));
    }
    // A retired (succeeded) identity keeps a handle-less `users` row until its
    // successor is deleted, so the check above answers "registered" for a key
    // that can never log in again. Granting it would count dead weight in
    // `admin_count` and the removal quorum, and the row would later leave with
    // the successor's deletion chain rather than through `admin.remove`'s floor
    // (`admin.md` § Admin continuity and succession). The successor is who
    // holds the account now.
    if crate::auth_core::successor_of(state, target)
        .await
        .map_err(|e| internal(format!("supersession consult failed: {e:?}")))?
        .is_some()
    {
        return Err(not_found(
            "cannot grant the admin role to a retired identity — grant it to its successor",
        ));
    }
    Ok(())
}

/// Family-safety lifecycle gate (family-safety.md § Lifecycle gates): an
/// account that guards supervised accounts cannot be evicted or deleted while
/// the links exist — a stranded ward would be unrecoverable oversight. The
/// admin resolves each link (fauna.family.transfer / fauna.family.graduate)
/// first.
async fn require_no_guardianships(state: &AppState, target: &[u8; 32]) -> Result<(), RpcError> {
    let wards = state.db.list_wards(target).await.map_err(internal)?;
    if !wards.is_empty() {
        let listed: Vec<String> = wards
            .iter()
            .map(|w| hex::encode(&w.supervised_actor_id))
            .collect();
        let mut e = RpcError::new(
            "fauna.admin.guardianships_unresolved",
            "error.admin.guardianships_unresolved",
        );
        e.details = Some(Box::new(Value::String(format!(
            "account guards supervised account(s) [{}] — transfer or graduate each first",
            listed.join(", ")
        ))));
        return Err(e);
    }
    Ok(())
}

/// The five tier caps are **non-negative** by ratified rule
/// (`value-formatting.md` § Tier cap validation — every app's `parse_cap`
/// clamps a negative to `0`, and `0` is a valid admin-chosen "no allowance").
///
/// The nest checks it too because the nest is the trust boundary and the app
/// clamps are not reachable from here: a non-conforming client can still put an
/// unclamped value on the wire. A negative that reaches `tiers` is silently destructive rather
/// than loud — the storage gate compares `used + delta > max_storage_bytes`, so
/// a negative cap refuses **every** write by the tier's users while the admin
/// sees a successful save.
///
/// Shared by `create` and `update` so the two doors cannot drift (priority #1).
fn validate_tier_caps(t: &TierRow) -> Result<(), RpcError> {
    for (field, value) in [
        ("max_inbox_bytes", t.max_inbox_bytes),
        ("max_storage_bytes", t.max_storage_bytes),
        ("max_devices", t.max_devices),
        ("max_blob_size", t.max_blob_size),
        ("max_feeds", t.max_feeds),
    ] {
        if value < 0 {
            return Err(invalid_params(format!(
                "{field} must be non-negative (got {value})"
            )));
        }
    }
    if t.max_devices > MAX_TIER_MAX_DEVICES {
        return Err(invalid_params(format!(
            "max_devices must be at most {MAX_TIER_MAX_DEVICES} (got {})",
            t.max_devices
        )));
    }
    Ok(())
}

/// The largest `max_devices` an admin may set on any tier.
///
/// **Derived from [`fauna_sync_engine::MAX_FRONTIER_WRITERS`] on purpose, and
/// that derivation is the point of the constant.** The page loop's writer
/// ceiling sizes itself against the honest writer set whose first term is
/// `AdminTier::max_devices` ("single digits in every shipped tier"), but the
/// two numbers lived in different crates with no relationship expressed
/// anywhere: `validate_tier_caps` accepted any non-negative `i64`, so an admin
/// could set a tier whose *honest* fleet would overrun a ceiling whose whole
/// justification was that no honest walk reaches it. An overgrown frontier is
/// refused above the checkpoint — correct against a hostile counterpart, whose
/// rows stop when it does, but the writers here would be the account's own, so
/// every pass would re-grow past the ceiling and refuse again.
///
/// The 1:64 ratio leaves room for the ceiling's other terms — the retired
/// succession identities, the nest, and the same again per member on a
/// shared-set scope — while staying far above any honest fleet (the shipped
/// tiers seed 3 / 5 / 10, `db/migrations.rs` `SEED_TIERS`). It does **not**
/// make the ceiling's sizing sentence unconditionally true: the succession
/// term is unbounded by lifetime count, which `account-sync-plane.md` § Feeds
/// and cursors now names rather than assumes.
const MAX_TIER_MAX_DEVICES: i64 = (fauna_sync_engine::MAX_FRONTIER_WRITERS / 64) as i64;

/// Validate a supervised-admission guardian designation
/// (`family-safety.md` § The guardianship link): 32 raw bytes naming an
/// existing, non-suspended user that is not itself supervised (no chains).
/// Returns the validated 32-byte id.
async fn validate_guardian(state: &AppState, guardian: &[u8]) -> Result<[u8; 32], RpcError> {
    let id: [u8; 32] =
        crate::rpc_errors::require_bytes32("guardian_actor", guardian).map_err(invalid_params)?;
    match state
        .db
        .check_guardian_admissible(&id)
        .await
        .map_err(internal)?
    {
        Ok(()) => Ok(id),
        Err("not_found") => Err(not_found("guardian actor not found")),
        Err(reason) => Err(invalid_params(format!("guardian actor is {reason}"))),
    }
}

/// Validate an admission's age-band dial (`family-safety.md` § The account
/// age band): the token must be one [`fauna_protocol::age::AgeBand`] names,
/// and a band **requires a guardian designation** — the band is set exactly
/// where the supervised designation is set, so a band on an unsupervised
/// mint/approve is refused rather than stored inert. Returns the validated
/// token (borrowed from the request).
fn validate_age_band(age_band: Option<&str>, has_guardian: bool) -> Result<Option<&str>, RpcError> {
    match age_band {
        None => Ok(None),
        Some(band) => {
            if fauna_protocol::age::AgeBand::from_wire(band).is_none() {
                return Err(invalid_params(format!("unknown age band: {band}")));
            }
            if !has_guardian {
                return Err(invalid_params(
                    "an age band requires a guardian designation (family-safety.md § The account age band)",
                ));
            }
            Ok(Some(band))
        }
    }
}

/// `InviteRequestRow` → wire `AdminInviteRequest` (the twin's `status_json`).
fn invite_request_to_wire(r: InviteRequestRow) -> AdminInviteRequest {
    AdminInviteRequest {
        id: r.id,
        actor_id: ByteBuf::from(r.actor_id),
        handle: r.handle,
        message: r.message,
        status: r.status,
        created_at: r.created_at,
        decided_at: r.decided_at,
        decided_by: r.decided_by.map(ByteBuf::from),
        denial_reason: r.denial_reason,
        age_band: r.age_band,
        age_band_provenance: r.age_provenance,
        extra: Default::default(),
    }
}

// ── fauna.admin.users.list (≡ GET /admin/api/users?limit&offset) ─────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.users.list").await?;
            let req: AdminUsersListRequest = decode(&payload).map_err(malformed)?;

            // The twin's defaults: absent limit → the default page; clamp to
            // 1..=the largest page; offset >= 0.
            let limit = req
                .limit
                .unwrap_or(fauna_protocol::admin::USERS_LIST_DEFAULT_LIMIT)
                .clamp(1, fauna_protocol::admin::USERS_LIST_MAX_LIMIT);
            let offset = req.offset.max(0);

            let (users, total) = state
                .db
                .list_users_paginated(limit, offset)
                .await
                .map_err(internal)?;
            // Read-only IMAP/CalDAV-serving audit indicator (admin.md § Users;
            // mail-settings.md § Local IMAP/CalDAV-serving toggle): one batch read
            // (the override table holds only actors who opted out) resolves the
            // per-row flag — no N-query fan-out.
            let serving_overrides = state
                .db
                .list_mail_serving_overrides()
                .await
                .map_err(internal)?;
            let admins = admin_actor_set(&state).await?;
            encode_reply(&AdminUsersListReply {
                users: users
                    .into_iter()
                    .map(|u| user_to_wire_with_overrides(u, &serving_overrides, &admins))
                    .collect(),
                total,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.users.get (≡ GET /admin/api/users/{actor_id}) ────────────────

fn get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.users.get").await?;
            let req: AdminUserGetRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;

            let user = state
                .db
                .get_user(&target)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("user not found"))?;
            let serving = state
                .db
                .get_actor_mail_serving_enabled(&target)
                .await
                .map_err(internal)?
                .unwrap_or(true);
            let is_admin = state.db.is_admin(&target).await.map_err(internal)?;
            encode_reply(&AdminUserGetReply {
                user: user_to_wire(user, serving, is_admin),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.users.create (≡ POST /admin/api/users) ───────────────────────

fn create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.users.create").await?;
            let req: AdminUserCreateRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;

            // A handle, when supplied, is vetted exactly as the invite-request
            // approval path vets one (format → reserved → taken) so the three
            // account-creation paths cannot admit handles of differing rigour.
            // Absent ⇒ the handle-less admission (the admit form's blank
            // handle); see `AdminUserCreateRequest::handle`.
            let handle = req
                .handle
                .as_deref()
                .map(str::trim)
                .filter(|h| !h.is_empty());

            // Supervised admission (`family-safety.md` § The guardianship
            // link — the direct-admission arm): validated exactly as the
            // approve path validates its designation, including guardian ≠
            // admitted actor (supervised-by-self is unrepresentable). The
            // handle-less shape cannot be supervised: both other
            // supervision-carrying paths always carry a handle.
            let guardian = match req.guardian_actor.as_ref() {
                Some(g) => {
                    if handle.is_none() {
                        return Err(invalid_params(
                            "a supervised admission carries a handle (family-safety.md § The guardianship link)",
                        ));
                    }
                    if g.as_slice() == target.as_slice() {
                        return Err(invalid_params(
                            "the admitted actor cannot be their own guardian",
                        ));
                    }
                    Some(validate_guardian(&state, g).await?)
                }
                None => None,
            };
            // The age-band dial rides the same admission (`family-safety.md`
            // § The account age band) — guardian-asserted provenance, same
            // validation as mint/approve.
            let age_band = validate_age_band(req.age_band.as_deref(), guardian.is_some())?;

            if let Some(handle) = handle {
                validate_handle(handle).map_err(invalid_params)?;
                if state
                    .auth
                    .registration
                    .reserved_handles
                    .iter()
                    .any(|r| r == handle)
                {
                    return Err(invalid_params("handle is reserved"));
                }
                if state
                    .db
                    .resolve_handle(handle)
                    .await
                    .map_err(internal)?
                    .is_some_and(|owner| owner != target)
                {
                    return Err(conflict("handle already taken"));
                }
                state
                    .db
                    .create_user_with_handle_and_age(
                        &target,
                        &req.tier,
                        handle,
                        guardian.as_ref().map(|g| g.as_slice()),
                        age_band.map(|band| {
                            (
                                band,
                                fauna_protocol::age::AgeBandProvenance::GuardianAsserted.as_str(),
                            )
                        }),
                    )
                    .await
                    .map_err(|e| unique_conflict(e, "actor or handle already taken"))?;
                // `create_user_with_handle` takes no label, so apply it the way
                // the invite-approval path's tail does.
                if !req.label.is_empty() {
                    let _ = state.db.update_user(&target, &req.tier, &req.label).await;
                }
            } else {
                state
                    .db
                    .create_user(&target, &req.tier, &req.label)
                    .await
                    .map_err(create_user_error)?;
            }

            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "user.create",
                    Some(&hex::encode(target)),
                    Some(&{
                        let mut detail = match handle {
                            Some(h) => format!("tier={}, handle={h}", req.tier),
                            None => format!("tier={}", req.tier),
                        };
                        if let Some(g) = guardian {
                            detail.push_str(&format!(", guardian={}", hex::encode(g)));
                        }
                        if let Some(band) = age_band {
                            detail.push_str(&format!(", age_band={band}"));
                        }
                        detail
                    }),
                )
                .await;
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.users.update (≡ PUT /admin/api/users/{actor_id}) ─────────────

fn update_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.users.update").await?;
            let req: AdminUserUpdateRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;

            let updated = state
                .db
                .update_user(&target, &req.tier, &req.label)
                .await
                .map_err(internal)?;
            if !updated {
                return Err(not_found("user not found"));
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "user.update",
                    Some(&hex::encode(target)),
                    Some(&format!("tier={}, label={}", req.tier, req.label)),
                )
                .await;
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.users.delete (≡ DELETE /admin/api/users/{actor_id}) ──────────

fn delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.users.delete").await?;
            let req: AdminUserDeleteRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;
            require_not_admin(&state, &target).await?;
            require_no_predecessor_admin(&state, &target).await?;
            require_no_guardianships(&state, &target).await?;

            // Verify the user exists before scheduling deletion (the twin's 404).
            if state
                .db
                .get_user(&target)
                .await
                .map_err(internal)?
                .is_none()
            {
                return Err(not_found("user not found"));
            }
            encode_reply(
                &schedule_pending_action(
                    &state,
                    &actor_id,
                    &target,
                    ActionType::AdminDeleteUser,
                    "user.delete.scheduled",
                )
                .await?,
            )
        })
    })
}

// ── fauna.admin.users.clear_handle (≡ DELETE /admin/api/users/{id}/handle) ───

fn clear_handle_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.users.clear_handle").await?;
            let req: AdminUserClearHandleRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;

            state
                .db
                .set_handle(&target, "")
                .await
                .map_err(|e| internal(format!("failed to clear handle: {e}")))?;
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "user.handle.clear",
                    Some(&hex::encode(target)),
                    None,
                )
                .await;
            push_account_updated(&state, &target, "handle");
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.users.evict (≡ POST /admin/api/users/{id}/evict) ─────────────

const EVICTION_CATEGORIES: [&str; 5] = ["terms", "capacity", "legal", "abuse", "other"];

fn evict_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.users.evict").await?;
            let req: AdminUserEvictRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;

            if !EVICTION_CATEGORIES.contains(&req.category.as_str()) {
                return Err(invalid_params("invalid category"));
            }
            if req.reason.is_empty() {
                return Err(invalid_params("reason is required"));
            }
            require_not_admin(&state, &target).await?;
            require_no_guardianships(&state, &target).await?;

            let started = state
                .db
                .start_eviction(
                    &target,
                    &req.reason,
                    &req.category,
                    fauna_protocol::node_policy::EVICTION_WARNING_DAYS,
                    fauna_protocol::node_policy::EVICTION_SUSPENSION_DAYS,
                )
                .await
                .map_err(internal)?;
            if !started {
                // The twin's 409: user not found or already being evicted.
                return Err(conflict("user not found or already being evicted"));
            }

            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "eviction.start",
                    Some(&hex::encode(target)),
                    Some(&format!("category={}, reason={}", req.category, req.reason)),
                )
                .await;

            // Issue an eviction export token (survives regular token revocation).
            let export_token = fauna_core::identity::random_hex(32);
            let delete_at = state
                .db
                .get_user(&target)
                .await
                .ok()
                .flatten()
                .and_then(|u| u.eviction_delete_at)
                .unwrap_or_else(|| {
                    now_secs() as i64
                        + (fauna_protocol::node_policy::EVICTION_WARNING_DAYS
                            + fauna_protocol::node_policy::EVICTION_SUSPENSION_DAYS)
                            * 86400
                });
            let _ = state
                .db
                .create_eviction_token(&export_token, &target, delete_at)
                .await;
            push_account_updated(&state, &target, "eviction");
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.users.cancel_eviction (≡ POST .../cancel-eviction) ───────────

fn cancel_eviction_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.users.cancel_eviction").await?;
            let req: AdminUserCancelEvictionRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;

            let cancelled = state.db.cancel_eviction(&target).await.map_err(internal)?;
            if !cancelled {
                return Err(not_found("no active eviction for this user"));
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "eviction.cancel",
                    Some(&hex::encode(target)),
                    None,
                )
                .await;
            let _ = state.db.delete_eviction_tokens(&target).await;
            push_account_updated(&state, &target, "eviction");
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.users.suspend (≡ POST /admin/api/users/{id}/suspend) ─────────

fn suspend_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.users.suspend").await?;
            let req: AdminUserSuspendRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;

            // Verify the user exists before suspending (the twin's 404).
            if state
                .db
                .get_user(&target)
                .await
                .map_err(internal)?
                .is_none()
            {
                return Err(not_found("user not found"));
            }
            require_not_admin(&state, &target).await?;

            let category = if req.category.is_empty() {
                "other"
            } else {
                &req.category
            };
            if !EVICTION_CATEGORIES.contains(&category) {
                return Err(invalid_params("invalid category"));
            }
            let reason = if req.reason.is_empty() {
                "suspended by admin"
            } else {
                &req.reason
            };

            // Immediate, and reversible by construction: the user enters the
            // eviction machine's `suspended` state with no delete timeline, so
            // `fauna.admin.users.cancel_eviction` restores them.
            let suspended = state
                .db
                .suspend_user_now(&target, reason, category)
                .await
                .map_err(internal)?;
            if !suspended {
                return Err(conflict("user not found or already suspended"));
            }

            // Kill the tokens, then close the connections already open. The
            // suspension gate in `caller_class_for_actor` (re-read per RPC)
            // already denies a live connection every *kind*, but the socket
            // itself stayed up and kept receiving Push events — a suspended user
            // watched their feed update in real time. Closing it makes the
            // cut-off total (`transport.md` § Revocation teardown). What keeps a
            // NEW connection out is not this revoke alone — a bearer it never
            // saw would otherwise still open a socket — but the two standing
            // checks behind it: the mints refuse a suspended actor
            // (`auth_core`'s `check_actor_active`, verify included), and every
            // bearer door asks the actor's standing at use
            // (`auth::check_bearer_session`).
            state
                .auth
                .token_store
                .revoke_actor(&fauna_core::identity::ActorId(target))
                .await;
            let _ = state.db.delete_eviction_tokens(&target).await;
            // This push is now best-effort: `disconnect_actor` closes the socket
            // without draining, so a queued Push frame is usually dropped. That
            // is fine — the 4401 close is the authoritative signal, and the
            // client learns the reason when its re-auth is refused. The push
            // still matters when the actor has no live connection at all.
            push_account_updated(&state, &target, "suspension");
            state.close_actor_sockets(&target);

            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "user.suspended",
                    Some(&hex::encode(target)),
                    Some(&format!("category={category}, reason={reason}")),
                )
                .await;

            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// Shared body for the admin pending-action creators (`users.delete` /
/// `admins.add` / `admins.remove`): the caller has already verified the target.
/// Schedules the action attributed to the admin (`caller`) — which rings its
/// creation notice (`pending_actions::schedule`) — audits it, and shapes the
/// `202`-style reply. Mirrors `admin::{delete_user, suspend_user}`.
async fn schedule_pending_action(
    state: &std::sync::Arc<AppState>,
    caller: &[u8; 32],
    target: &[u8; 32],
    action_type: ActionType,
    audit_action: &str,
) -> Result<AdminPendingActionReply, RpcError> {
    let target_hex = hex::encode(target);
    let row =
        crate::pending_actions::schedule(state, &action_type, &caller[..], Some(&target_hex), None)
            .await
            .map_err(internal)?;
    let action_id = row.id;
    let _ = state
        .db
        .audit(
            Some(&caller[..]),
            audit_action,
            Some(&target_hex),
            Some(&format!("pending_action_id={action_id}")),
        )
        .await;
    Ok(AdminPendingActionReply {
        pending_action_id: action_id,
        execute_after: row.execute_after,
        status: "pending".to_string(),
        extra: Default::default(),
    })
}

// ── fauna.admin.evictions.list (≡ GET /admin/api/evictions) ──────────────────

fn evictions_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.evictions.list").await?;
            let _req: AdminEvictionsListRequest = decode(&payload).map_err(malformed)?;

            let users = state.db.list_evictions().await.map_err(internal)?;
            let serving_overrides = state
                .db
                .list_mail_serving_overrides()
                .await
                .map_err(internal)?;
            let admins = admin_actor_set(&state).await?;
            encode_reply(&AdminEvictionsListReply {
                evictions: users
                    .into_iter()
                    .map(|u| user_to_wire_with_overrides(u, &serving_overrides, &admins))
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

// ═════════════════════════════════════════════════════════════════════════════
// C2 — admin-management cluster (tiers / invite codes / invite requests / admins)
// ═════════════════════════════════════════════════════════════════════════════

// ── Tiers (≡ /admin/api/tiers) ───────────────────────────────────────────────

fn tiers_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.tiers.list").await?;
            let _req: AdminTiersListRequest = decode(&payload).map_err(malformed)?;
            let tiers = state.db.list_tiers().await.map_err(internal)?;
            encode_reply(&AdminTiersListReply {
                tiers: tiers.into_iter().map(tier_to_wire).collect(),
                extra: Default::default(),
            })
        })
    })
}

fn tiers_create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.tiers.create").await?;
            let req: AdminTierCreateRequest = decode(&payload).map_err(malformed)?;
            if req.name.is_empty() {
                return Err(invalid_params("name is required"));
            }
            let tier = TierRow {
                name: req.name.clone(),
                max_inbox_bytes: req.max_inbox_bytes,
                max_storage_bytes: req.max_storage_bytes,
                max_devices: req.max_devices,
                max_blob_size: req.max_blob_size,
                max_feeds: req.max_feeds,
            };
            validate_tier_caps(&tier)?;
            state
                .db
                .create_tier(&tier)
                .await
                .map_err(|e| unique_conflict(e, "tier already exists"))?;
            let _ = state
                .db
                .audit(Some(&actor_id[..]), "tier.create", Some(&req.name), None)
                .await;
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Membership tiers (monetization.md § Pillar 4 — paid nest access) ─────────
//
// The designation linking one of the admin's OWN subscription tiers (the
// entitlement object) to the quota tiers (`tiers`) an admitted / lapsed member
// runs under. Distinct kind family from `tiers.*` above precisely because the
// two tier systems are never merged.

/// Shared validation for `set`: the named subscription tier must exist **and be
/// owned by the caller** (an admin is a payee like any other and may only
/// designate their own tiers), and both quota tiers must name real `tiers` rows.
///
/// The ownership + existence check lives here rather than in a foreign key on
/// purpose (see `MIGRATIONS_MEMBERSHIP_TIERS`): an FK to `subscription_tiers`
/// would block the payee from ever deleting the tier.
async fn validate_membership_designation(
    state: &AppState,
    admin_id: &[u8; 32],
    tier_name: &str,
    admin_tier: &str,
    lapse_tier: &str,
) -> Result<(), RpcError> {
    if tier_name.is_empty() {
        return Err(invalid_params("tier_name is required"));
    }
    if state
        .db
        .get_subscription_tier(admin_id, tier_name)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err(not_found("subscription tier not found"));
    }
    // Both quota tiers must exist. Checked before the write so a refusal leaves
    // no row behind, and so the failure names the offending field rather than
    // surfacing as a bare foreign-key violation.
    for (label, quota_tier) in [("admin_tier", admin_tier), ("lapse_tier", lapse_tier)] {
        if quota_tier.is_empty() {
            return Err(invalid_params(format!("{label} is required")));
        }
        if state
            .db
            .get_tier(quota_tier)
            .await
            .map_err(internal)?
            .is_none()
        {
            return Err(invalid_params(format!("{label} names no known tier")));
        }
    }
    Ok(())
}

fn membership_tiers_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.membership_tiers.list").await?;
            let _req: AdminMembershipTiersListRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_membership_tiers(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&AdminMembershipTiersListReply {
                membership_tiers: rows
                    .into_iter()
                    .map(|r| AdminMembershipTier {
                        tier_name: r.tier_name,
                        admin_tier: r.admin_tier,
                        lapse_tier: r.lapse_tier,
                        created_at: r.created_at,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

fn membership_tiers_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.membership_tiers.set").await?;
            let req: AdminMembershipTierSetRequest = decode(&payload).map_err(malformed)?;
            // monetization.md § Pillar 4: "`lapse_tier` defaults to `free`".
            let lapse_tier = req.lapse_tier.as_deref().unwrap_or(DEFAULT_LAPSE_TIER);
            validate_membership_designation(
                &state,
                &actor_id,
                &req.tier_name,
                &req.admin_tier,
                lapse_tier,
            )
            .await?;
            state
                .db
                .upsert_membership_tier(&actor_id, &req.tier_name, &req.admin_tier, lapse_tier)
                .await
                .map_err(internal)?;
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "membership_tier.set",
                    Some(&req.tier_name),
                    Some(&format!(
                        "admin_tier={} lapse_tier={}",
                        req.admin_tier, lapse_tier
                    )),
                )
                .await;
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn membership_tiers_clear_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.membership_tiers.clear").await?;
            let req: AdminMembershipTierClearRequest = decode(&payload).map_err(malformed)?;
            let cleared = state
                .db
                .delete_membership_tier(&actor_id, &req.tier_name)
                .await
                .map_err(internal)?;
            if !cleared {
                return Err(not_found("membership designation not found"));
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "membership_tier.clear",
                    Some(&req.tier_name),
                    None,
                )
                .await;
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn tiers_update_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.tiers.update").await?;
            let req: AdminTierUpdateRequest = decode(&payload).map_err(malformed)?;
            let tier = TierRow {
                name: req.name.clone(),
                max_inbox_bytes: req.max_inbox_bytes,
                max_storage_bytes: req.max_storage_bytes,
                max_devices: req.max_devices,
                max_blob_size: req.max_blob_size,
                max_feeds: req.max_feeds,
            };
            validate_tier_caps(&tier)?;
            let updated = state.db.update_tier(&tier).await.map_err(internal)?;
            if !updated {
                return Err(not_found("tier not found"));
            }
            let _ = state
                .db
                .audit(Some(&actor_id[..]), "tier.update", Some(&req.name), None)
                .await;
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Invite codes (≡ /admin/api/invite-codes) ─────────────────────────────────

fn invite_codes_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.invite_codes.list").await?;
            let _req: AdminInviteCodesListRequest = decode(&payload).map_err(malformed)?;
            let codes = state.db.list_invite_codes().await.map_err(internal)?;
            encode_reply(&AdminInviteCodesListReply {
                invite_codes: codes.into_iter().map(invite_code_to_wire).collect(),
                extra: Default::default(),
            })
        })
    })
}

fn invite_codes_create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.invite_codes.create").await?;
            let req: AdminInviteCodeCreateRequest = decode(&payload).map_err(malformed)?;
            // A mint at `uses <= 0` is a **born-dead code**: redemption requires
            // `uses_left > 0` (`db::admin::validate_invite_code`), so the admin
            // would hand out a token every invitee is told is invalid. All 7
            // apps clamp to `>= 1`, but a non-conforming client can still send
            // less and the door is the trust boundary. `1` is the floor, not `2` — single-use invites are
            // the common case.
            if req.uses < 1 {
                return Err(invalid_params(format!(
                    "uses must be at least 1 (got {}) — a lower value mints a code that can never be redeemed",
                    req.uses
                )));
            }
            // Empty code → mint one. The admin's input is tier + uses; the token
            // is just what the invitee types during onboarding.
            let code = if req.code.is_empty() {
                crate::admin::generate_invite_code()
            } else {
                req.code.clone()
            };
            // Supervised admission: validate the guardian designation at mint
            // (family-safety.md § Wire & data shape).
            let guardian = match req.guardian_actor.as_ref() {
                Some(g) => Some(validate_guardian(&state, g).await?),
                None => None,
            };
            // The age band rides only beside a guardian (`family-safety.md`
            // § The account age band — the band is set exactly where the
            // supervised designation is), and its token must be one the nest
            // can name (validate at write — the established knob rule).
            let age_band = validate_age_band(req.age_band.as_deref(), guardian.is_some())?;
            state
                .db
                .create_invite_code_with_guardian(
                    &code,
                    &req.tier,
                    req.uses,
                    guardian.as_ref().map(|g| g.as_slice()),
                    age_band,
                )
                .await
                .map_err(|e| unique_conflict(e, "invite code already exists"))?;
            // The code is a bearer credential and an audit row never stores
            // one — the row rides this admin's own export (the `audit_log`
            // verdict), so it gets the fingerprint, never the code.
            let fingerprint = crate::admin::invite_code_audit_fingerprint(
                &state.nest_identity.signing_key.to_bytes(),
                &code,
            );
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "invite.create",
                    Some(&fingerprint),
                    Some(&match guardian {
                        Some(g) => format!(
                            "tier={}, uses={}, guardian={}{}",
                            req.tier,
                            req.uses,
                            hex::encode(g),
                            match age_band {
                                Some(band) => format!(", age_band={band}"),
                                None => String::new(),
                            }
                        ),
                        None => format!("tier={}, uses={}", req.tier, req.uses),
                    }),
                )
                .await;
            encode_reply(&AdminInviteCodeCreateReply {
                code,
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn invite_codes_delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.invite_codes.delete").await?;
            let req: AdminInviteCodeDeleteRequest = decode(&payload).map_err(malformed)?;
            let deleted = state
                .db
                .delete_invite_code(&req.code)
                .await
                .map_err(internal)?;
            if !deleted {
                return Err(not_found("invite code not found"));
            }
            // The same fingerprint `invite.create` stored, so the two match.
            let fingerprint = crate::admin::invite_code_audit_fingerprint(
                &state.nest_identity.signing_key.to_bytes(),
                &req.code,
            );
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "invite.delete",
                    Some(&fingerprint),
                    None,
                )
                .await;
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Invite requests (admin side, ≡ /admin/api/invite-requests) ───────────────

fn invite_requests_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.invite_requests.list").await?;
            let _req: AdminInviteRequestsListRequest = decode(&payload).map_err(malformed)?;
            let rows = state.db.list_invite_requests().await.map_err(internal)?;
            encode_reply(&AdminInviteRequestsListReply {
                invite_requests: rows.into_iter().map(invite_request_to_wire).collect(),
                extra: Default::default(),
            })
        })
    })
}

fn invite_requests_approve_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.invite_requests.approve").await?;
            let req: AdminInviteRequestApproveRequest = decode(&payload).map_err(malformed)?;

            let row = state
                .db
                .get_invite_request(req.id)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("invite request not found"))?;
            if row.status != "pending" {
                return Err(conflict("invite request is not pending"));
            }
            let target: [u8; 32] = row
                .actor_id
                .as_slice()
                .try_into()
                .map_err(|_| internal("corrupt actor_id"))?;
            let tier = req.tier.as_deref().unwrap_or("free").to_string();

            // Re-validate the stored handle at approval time, not just at
            // submission — the reserved list can change while a row is pending,
            // so a pending row could carry a handle the current list would
            // refuse. Mint-time is the boundary.
            validate_handle(&row.handle).map_err(invalid_params)?;
            if state
                .auth
                .registration
                .reserved_handles
                .iter()
                .any(|r| r == &row.handle)
            {
                return Err(invalid_params("handle is reserved"));
            }

            // Supervised admission: validate the guardian designation at
            // approval (family-safety.md § Wire & data shape) — including the
            // guardian ≠ requester rule (supervised-by-self is unrepresentable).
            let guardian = match req.guardian_actor.as_ref() {
                Some(g) => {
                    // guardian ≠ requester first (supervised-by-self is
                    // unrepresentable) — the clearer error than the
                    // admissibility check's not_found (a pending requester is
                    // not a user yet, so it would fail that too).
                    if g.as_slice() == target.as_slice() {
                        return Err(invalid_params("the requester cannot be their own guardian"));
                    }
                    Some(validate_guardian(&state, g).await?)
                }
                None => None,
            };

            // The admin/guardian's age-band dial (`family-safety.md` § The
            // account age band) — picked at approval exactly like the tier;
            // guardian-asserted provenance. Same validation as the mint.
            let age_band = validate_age_band(req.age_band.as_deref(), guardian.is_some())?;

            // Re-check the handle at approval time (it may have been taken since
            // submission). The twin's 409.
            if state
                .db
                .resolve_handle(&row.handle)
                .await
                .map_err(internal)?
                .is_some()
            {
                return Err(conflict("handle already taken"));
            }
            state
                .db
                .create_user_with_handle_and_age(
                    &target,
                    &tier,
                    &row.handle,
                    guardian.as_ref().map(|g| g.as_slice()),
                    age_band.map(|band| {
                        (
                            band,
                            fauna_protocol::age::AgeBandProvenance::GuardianAsserted.as_str(),
                        )
                    }),
                )
                .await
                .map_err(|e| unique_conflict(e, "actor or handle already taken"))?;

            // Optional label update; best-effort FTS indexing; delete the
            // (now-terminal) request — exactly the twin's tail.
            if let Some(label) = req.label.as_deref()
                && !label.is_empty()
            {
                let _ = state.db.update_user(&target, &tier, label).await;
            }
            let _ = state.db.delete_invite_request(req.id).await;

            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "invite_request.approve",
                    Some(&hex::encode(target)),
                    Some(&match guardian {
                        Some(g) => format!(
                            "handle={} tier={} guardian={}{}",
                            row.handle,
                            tier,
                            hex::encode(g),
                            match age_band {
                                Some(band) => format!(" age_band={band}"),
                                None => String::new(),
                            }
                        ),
                        None => format!("handle={} tier={}", row.handle, tier),
                    }),
                )
                .await;
            encode_reply(&AdminInviteRequestApproveReply {
                actor_id: ByteBuf::from(target.to_vec()),
                handle: row.handle,
                tier,
                extra: Default::default(),
            })
        })
    })
}

fn invite_requests_deny_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.invite_requests.deny").await?;
            let req: AdminInviteRequestDenyRequest = decode(&payload).map_err(malformed)?;

            let row = state
                .db
                .get_invite_request(req.id)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("invite request not found"))?;
            if row.status != "pending" {
                return Err(conflict("invite request is not pending"));
            }
            let reason = req.reason.as_deref();
            let denied = state
                .db
                .deny_invite_request(req.id, &actor_id, reason)
                .await
                .map_err(internal)?;
            if !denied {
                // Raced to non-pending between the read and the UPDATE.
                return Err(conflict("invite request is not pending"));
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "invite_request.deny",
                    Some(&hex::encode(&row.actor_id)),
                    reason,
                )
                .await;
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Admins (≡ /admin/api/admins) ─────────────────────────────────────────────

fn admins_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.admins.list").await?;
            let _req: AdminAdminsListRequest = decode(&payload).map_err(malformed)?;
            let rows = state.db.list_admin_actors().await.map_err(internal)?;
            encode_reply(&AdminAdminsListReply {
                admins: rows
                    .into_iter()
                    .map(|(actor_id, added_at)| AdminAdminEntry {
                        actor_id: ByteBuf::from(actor_id),
                        added_at,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

fn admins_add_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.admins.add").await?;
            let req: AdminAdminAddRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;
            require_registered_user(&state, &target).await?;
            // Schedule the grant (the executor applies it after the delay) —
            // `schedule_pending_action` hex-encodes `target` as the action's
            // target id, matching the twin's `Some(&actor_id_hex)`.
            encode_reply(
                &schedule_pending_action(
                    &state,
                    &actor_id,
                    &target,
                    ActionType::AdminAdd,
                    "admin.add.scheduled",
                )
                .await?,
            )
        })
    })
}

fn admins_remove_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.admins.remove").await?;
            let req: AdminAdminRemoveRequest = decode(&payload).map_err(malformed)?;
            let target = actor_id_from_wire(&req.actor_id)?;
            // The door's synchronous half of the superadmin floor,
            // target-aware: removing the last superadmin refuses immediately;
            // a non-superadmin admin's removal never trips it. The WRITER
            // re-refuses at execution — this 409 alone cannot hold a
            // 24 h-delayed quorum action (admin.md § 2 → *Cutting a user off*).
            if !state.db.can_remove_admin(&target).await.map_err(internal)? {
                return Err(conflict("cannot remove the last superadmin"));
            }
            encode_reply(
                &schedule_pending_action(
                    &state,
                    &actor_id,
                    &target,
                    ActionType::AdminRemove,
                    "admin.remove.scheduled",
                )
                .await?,
            )
        })
    })
}

/// `fauna.admin.deployment_seed.get` — the co-admin seed hand-off
/// (`nest/box-recovery.md` § Mechanism, the co-admin bullet). The claim
/// hand-off (`claim_handlers.rs`) generalized from *the claiming admin* to
/// *any current admin*: gated to the roster (`require_permission` below), it
/// hands the same deployment signing seed to a later-added co-admin's client
/// so the self-healing capture trigger can custody it off-box — without this,
/// a co-admin holds the role but not the recovery custody the roster's
/// continuity promise depends on.
fn deployment_seed_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.deployment_seed.get").await?;
            let _req: AdminDeploymentSeedGetRequest = decode(&payload).map_err(malformed)?;
            // Same hex + SecretString sourcing as the claim hand-off
            // (`claim_handlers.rs`) — `None` only if this nest holds no
            // signing key at all (should not happen post-boot; the client
            // treats it as a benign no-op, never an error).
            let deployment_seed = state
                .nest_signing_key
                .as_ref()
                .map(|sk| SecretString::from(hex::encode(sk.to_bytes())));
            encode_reply(&AdminDeploymentSeedGetReply {
                deployment_seed,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.admin.deployment_seed.rotate` — the deployment-seed rotation ceremony
/// (`nest/box-recovery.md` § Deployment-seed rotation).
///
/// The handler's whole job is the transaction: everything before it (mint the
/// successor seed, custody it off-box, fan it out) already happened on the
/// caller's client, and everything after it (rewrite the durable key file,
/// re-publish DNS `self=`) is the box's own follow-through. It refuses rather
/// than schedules: rotation is **immediate, no pending window**, because a delay
/// serves the attacker in the compromise-response case and costs legitimate
/// roster members nothing (their custody self-heals).
fn deployment_seed_rotate_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.deployment_seed.rotate").await?;
            let req: AdminDeploymentSeedRotateRequest = decode(&payload).map_err(malformed)?;

            let new_seed = decode_seed_hex(req.new_seed.as_str())
                .ok_or_else(|| invalid_params("new_seed must be 64 hex chars of a 32-byte seed"))?;
            let old_seed = state
                .nest_signing_key
                .as_ref()
                .map(|sk| Zeroizing::new(sk.to_bytes()))
                .ok_or_else(|| internal("this nest holds no deployment signing key"))?;

            match state
                .db
                .rotate_deployment_seed(&old_seed, &new_seed)
                .await
                .map_err(internal)?
            {
                Ok(outcome) => {
                    let new_id = outcome.statement.statement.new_nest_actor_id;
                    // Bearer eviction rides the rotation decision
                    // (`box-recovery.md` § Client acceptance → *Live-session
                    // convergence*, the ruling): every bearer was
                    // minted under the predecessor's authority, and the token
                    // store deliberately survives the serving-generation
                    // restart — so clear it here, making the teardown's forced
                    // reconnect take 401 → re-mint → graduation → the rotation
                    // bridge (the same path total box loss already forces).
                    // In-flight connections are unaffected (auth is bound at
                    // the upgrade), so the reply below still flushes; the
                    // teardown disconnects them moments later, and an upgrade
                    // racing this clear dies with that same teardown. Any
                    // process tear that loses this line loses the in-memory
                    // store with it. Deliberately NOT run on the
                    // `AlreadyRotated` ack: a late idempotent retry must not
                    // evict sessions minted after the rotation it acks.
                    let evicted = state.auth.token_store.clear().await;
                    tracing::info!(
                        evicted,
                        "deployment-seed rotation: every bearer session evicted"
                    );
                    // Post-commit: heal the durable file forward. A crash (or a
                    // failure) before this lands is recoverable — the boot
                    // reconcile recognises the superseded on-disk key and
                    // rewrites it — which is exactly why this sits *outside* the
                    // transaction and only logs on failure.
                    match crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
                        Some(dir) => {
                            if let Err(e) = std::fs::write(
                                crate::deployment_key::deployment_key_path(&dir),
                                new_seed.as_slice(),
                            ) {
                                tracing::error!(
                                    "deployment-seed rotation committed but the durable key file \
                                     was not rewritten ({e}); the next boot will heal it forward"
                                );
                            }
                        }
                        None => tracing::error!(
                            "deployment-seed rotation committed but the data dir could not be \
                             resolved from the db path; the next boot will heal the key file"
                        ),
                    }
                    tracing::warn!(
                        seq = outcome.statement.statement.seq,
                        satellites_rekeyed = outcome.satellites_rekeyed,
                        "deployment seed ROTATED to nest_actor_id {}",
                        hex::encode(new_id)
                    );
                    let _ = state
                        .db
                        .audit(
                            Some(&actor_id),
                            "deployment_seed.rotate",
                            Some(&hex::encode(new_id)),
                            Some(&format!("seq={}", outcome.statement.statement.seq)),
                        )
                        .await;
                    // Adoption by the running process (`box-recovery.md`
                    // § Deployment-seed rotation): after this reply flushes,
                    // tear down the serving generation and re-enter
                    // `start_server`, so the WHOLE graph — `nest_identity`,
                    // `nest_signing_key`, the workers holding cloned key
                    // material — rebuilds from the rotated DB. Same 750 ms
                    // reply-flush delay as factory reset's exit; unlike
                    // factory reset the process never exits, so no supervisor
                    // is needed. Deliberately a bare `tokio::spawn`, not
                    // `spawn_scoped`: this is the cross-generation messenger
                    // that CAUSES the teardown, and must not race the
                    // cancellation it triggers.
                    let restart = state.serve_restart.clone();
                    // spawn-ok(cross-generation-messenger): causes the teardown, must not die with the generation it tears down
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                        restart.notify_one();
                    });
                    encode_reply(&AdminDeploymentSeedRotateReply {
                        nest_actor_id: ByteBuf::from(new_id.to_vec()),
                        seq: outcome.statement.statement.seq,
                        already_rotated: false,
                        extra: Default::default(),
                    })
                }
                // The retry the ceremony is designed for: the client custodied
                // its seed before dispatch, so a lost reply is re-sent with the
                // seed the box already adopted. Answering "success, nothing to
                // do" is what makes that retry safe.
                Err(RotationRefusal::AlreadyRotated) => {
                    let id = ed25519_dalek::SigningKey::from_bytes(&new_seed)
                        .verifying_key()
                        .to_bytes();
                    encode_reply(&AdminDeploymentSeedRotateReply {
                        nest_actor_id: ByteBuf::from(id.to_vec()),
                        seq: 0,
                        already_rotated: true,
                        extra: Default::default(),
                    })
                }
                Err(refusal) => Err(conflict(refusal)),
            }
        })
    })
}

/// 64 hex chars → the raw 32-byte seed, held zeroizing. Same convention as the
/// claim reply's `deployment_seed` and `FAUNA_DEPLOYMENT_SEED`.
fn decode_seed_hex(value: &str) -> Option<Zeroizing<[u8; 32]>> {
    let bytes = Zeroizing::new(hex::decode(value.trim()).ok()?);
    Some(Zeroizing::new(<[u8; 32]>::try_from(bytes.as_slice()).ok()?))
}

// ── C3 — stats / audit / ops ─────────────────────────────────────────────────

// ── fauna.admin.stats (≡ GET /admin/api/stats) ───────────────────────────────

fn stats_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.stats").await?;
            let _req: AdminStatsRequest = decode(&payload).map_err(malformed)?;

            let s = state.db.get_stats().await.map_err(internal)?;
            encode_reply(&AdminStatsReply {
                total_users: s.total_users,
                users_by_tier: s.users_by_tier,
                suspended_users: s.suspended_users,
                total_inbox_bytes: s.total_inbox_bytes,
                total_storage_bytes: s.total_storage_bytes,
                ws_connections: state.ws.connection_count() as i64,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.status (≡ GET /admin/api/status) ─────────────────────────────

fn status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.status").await?;
            let _req: AdminStatusRequest = decode(&payload).map_err(malformed)?;

            let update_available = match state.update_status.borrow().clone() {
                Some(fauna_update::UpdateStatus::Available {
                    version,
                    release_url,
                }) => Some(AdminUpdateAvailable {
                    version,
                    url: release_url,
                    extra: Default::default(),
                }),
                _ => None,
            };
            encode_reply(&AdminStatusReply {
                version: env!("CARGO_PKG_VERSION").to_string(),
                update_available,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.audit.list (≡ GET /admin/api/audit?limit&before_id) ──────────

fn audit_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.audit.list").await?;
            let req: AdminAuditListRequest = decode(&payload).map_err(malformed)?;

            // The twin's defaults: absent limit → 100; clamp 1..=1000.
            let limit = req.limit.unwrap_or(100).clamp(1, 1000);
            let rows = state
                .db
                .list_audit(limit, req.before_id)
                .await
                .map_err(internal)?;
            let entries = rows
                .into_iter()
                .map(|r| AdminAuditEntry {
                    id: r.id,
                    ts: r.ts,
                    actor_id: r.actor_id.map(ByteBuf::from),
                    action: r.action,
                    target: r.target,
                    detail: r.detail,
                    prev_hash: r.prev_hash,
                    entry_hash: r.entry_hash,
                    entry_hash_version: r.entry_hash_version,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&AdminAuditListReply {
                entries,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.audit.integrity (≡ GET /admin/api/audit/integrity) ───────────

fn audit_integrity_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.audit.integrity").await?;
            let _req: AdminAuditIntegrityRequest = decode(&payload).map_err(malformed)?;

            let (chain_length, head_id, head_hash, first_entry_at, last_entry_at) =
                state.db.audit_integrity().await.map_err(internal)?;
            let census = state
                .db
                .audit_entry_version_census()
                .await
                .map_err(internal)?;
            encode_reply(&AdminAuditIntegrityReply {
                head_id,
                head_hash,
                chain_length,
                first_entry_at,
                last_entry_at,
                entries_v2: census.v2,
                entries_unverifiable: census.unverifiable,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.cluster.status (≡ GET /admin/api/cluster/status) ─────────────

fn cluster_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.cluster.status").await?;
            let _req: AdminClusterStatusRequest = decode(&payload).map_err(malformed)?;

            let stats = state.db.blob_storage_stats().await.map_err(internal)?;
            encode_reply(&AdminClusterStatusReply {
                total_blobs: stats.total_blobs,
                total_bytes: stats.total_bytes,
                local_blobs: stats.local_blobs,
                s3_blobs: stats.s3_blobs,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.gc (≡ POST /admin/api/gc) ────────────────────────────────────

fn gc_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.gc").await?;
            let req: AdminGcRequest = decode(&payload).map_err(malformed)?;

            // The twin's default grace period is 1800 s (30 min).
            let grace_period = req.grace_period_secs.unwrap_or(1800);
            // A NEGATIVE grace period inverts the window instead of shrinking
            // it, and this door deletes user data. `gc.rs` keeps a blob iff
            // `created_at > now - grace_period_secs`, so a negative puts the
            // cutoff in the FUTURE, which no blob's `created_at` can exceed —
            // every unreferenced blob becomes deletable and the fresh-blob grace
            // is silently disabled outright, which is exactly the in-flight
            // writer race the window exists to prevent (the superseded-sync pin
            // at `gc.rs`'s `superseded_cutoff_millis` unpins the same way).
            // Refused here, before the backup-service lookup, so an input error
            // is answered as one rather than resolving resources first. `0` is
            // allowed — "no grace, collect everything unreferenced now" is a
            // coherent admin choice, the same way `0` is a valid tier cap.
            if grace_period < 0 {
                return Err(invalid_params(format!(
                    "grace_period_secs must be non-negative (got {grace_period}) — a negative value would invert the grace window and delete blobs it exists to protect"
                )));
            }
            let backup_svc = state
                .backup_service
                .as_ref()
                .ok_or_else(|| invalid_params("backup not configured"))?;
            let blob_store = backup_svc.local_blob_store();

            let result = crate::backup::gc::garbage_collect(
                &state.db,
                &blob_store,
                crate::backup::gc::PostBodySource {
                    segments: &state.post_segments,
                },
                grace_period,
                backup_svc.encryption_key(),
                req.dry_run,
            )
            .await
            .map_err(internal)?;

            if !result.dry_run {
                let _ = state
                    .db
                    .audit(
                        Some(&actor_id[..]),
                        "gc.trigger",
                        None,
                        Some(&format!(
                            "deleted_blobs={}, deleted_bytes={}, skipped_grace={}, \
                             manifest_decode_failures={}",
                            result.deleted_blobs,
                            result.deleted_bytes,
                            result.skipped_grace_period,
                            result.manifest_decode_failures
                        )),
                    )
                    .await;
            }
            encode_reply(&AdminGcReply {
                dry_run: result.dry_run,
                deleted_blobs: result.deleted_blobs as i64,
                deleted_bytes: result.deleted_bytes as i64,
                live_snapshots: result.live_snapshots as i64,
                referenced_manifests: result.referenced_manifests as i64,
                skipped_grace_period: result.skipped_grace_period as i64,
                record_blob_refs: result.record_blob_refs as i64,
                conv_attachment_refs: result.conv_attachment_refs as i64,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.worker.status (≡ GET /admin/api/worker/status) ───────────────

fn worker_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.worker.status").await?;
            let _req: AdminWorkerStatusRequest = decode(&payload).map_err(malformed)?;

            let connected = state.bridge.worker.is_connected().await;
            let authorized_key = state.bridge.worker.authorized_key_hex();
            let replication_count = state.db.replication_count().await.unwrap_or(0);
            let worker = state
                .bridge
                .worker
                .get_handle()
                .await
                .map(|handle| AdminWorkerInfo {
                    max_storage_bytes: handle.info.max_storage_bytes as i64,
                    current_usage_bytes: handle.info.current_usage_bytes as i64,
                    payload_count: handle.info.payload_count as i64,
                    connected_at: handle.connected_at as i64,
                    extra: Default::default(),
                });
            encode_reply(&AdminWorkerStatusReply {
                authorized_key,
                connected,
                replication_count,
                worker,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.pending_actions.list (≡ GET /admin/api/pending-actions) ──────

/// Parse the DB's JSON-array string of approver-actor hexes; a malformed blob
/// degrades to an empty list (the B20 / DB `unwrap_or_default()` precedent).
fn parse_approvals(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn pending_actions_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.pending_actions.list").await?;
            let _req: AdminPendingActionsListRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .list_all_pending_actions()
                .await
                .map_err(internal)?;
            let actions = rows
                .into_iter()
                .map(|r| AdminPendingActionSummary {
                    id: r.id,
                    actor_id: ByteBuf::from(r.actor_id),
                    action_type: r.action_type,
                    target: r.target,
                    status: r.status,
                    created_at: r.created_at,
                    execute_after: r.execute_after,
                    requires_quorum: r.requires_quorum,
                    approvals: parse_approvals(&r.approvals),
                    ip_address: r.ip_address,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&AdminPendingActionsListReply {
                actions,
                extra: Default::default(),
            })
        })
    })
}

// ── C5 — folders / services ────────────────────────────────────────────────

// (The `fauna.admin.wireguard.{status,peers,keygen}` handlers that shared this
//  section died with the WireGuard stack, 2026-08-23.)

// ── fauna.admin.folders.create (≡ POST /admin/api/file-sets) ───────────────

fn folder_create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.folders.create").await?;
            let req: AdminFolderCreateRequest = decode(&payload).map_err(malformed)?;

            // The reserved (`__`) namespace belongs to the nest's own rails,
            // minted at the DB layer (`get_or_create_reserved_folder` — an
            // `INSERT OR IGNORE`, so an admin-pre-created row would be silently
            // ADOPTED as the rail, in a namespace the nest routes on by literal
            // name). Unconditional here, unlike the user-class create's
            // custody carve-out: this kind takes no mode, and the
            // coordinator provisions custody sets over `fauna.folders.create`.
            if crate::db::snapshots::is_reserved_folder_name(&req.name) {
                return Err(invalid_params(
                    "the \"__\" folder namespace is reserved for the nest's internal rails",
                ));
            }

            let actor = bytes32_from_wire(&req.actor_id, "actor_id")?;
            let id = match req.node_cache {
                Some(node_cache) => {
                    state
                        .db
                        .create_folder_with_node_cache(&req.name, &actor, node_cache)
                        .await
                }
                None => state.db.create_folder(&req.name, &actor).await,
            }
            // The twin mapped every error to a 500 "storage error" — but
            // `folders.name` is `UNIQUE` and both create paths wrap the
            // rusqlite error in `.context(...)`, so a duplicate name silently
            // `500`ed (the C1 `create_user` / B14 folders latent-bug class). The
            // kind returns the intended `fauna.admin.conflict` (matched via the
            // full chain `{e:#}`).
            .map_err(|e| unique_conflict(e, "folder already exists"))?;
            encode_reply(&AdminFolderCreateReply {
                id,
                name: req.name,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.folders.get (≡ GET /admin/api/file-sets/{name}) ────────────

/// Resolve the admin folder reference — by `name_hash` when the caller sent
/// one, else by plaintext `name`.
///
/// `folders` is unique on `(name, actor_id)` **and** on `(name_hash,
/// actor_id)`, so on a multi-user nest two actors may own same-named sets and a
/// bare reference of *either* kind is ambiguous. `actor_id` present → that
/// owner's row; absent → the single row matching, erroring honestly on a
/// cross-actor collision instead of returning whichever row sorts first (the
/// name-only `get_folder` failure mode this replaced).
///
/// **Why the hash arm exists (S5, `path-sealing.md`).** An
/// admin is not a set's key audience, so once the flip scrubs
/// `folders.name` they cannot read a name to send one. The hash they were
/// handed in a listing is the reference that survives, and it addresses the row
/// exactly. The two arms deliberately share **one** disposition — 0 / 1 / many —
/// so the ambiguity contract cannot drift between them. The wire parse itself
/// routes through `routes::parse_name_hash` — the single home for every
/// `name_hash` call site.
async fn resolve_folder_by_name(
    state: &AppState,
    name: &str,
    name_hash: &Option<ByteBuf>,
    actor_id: &Option<ByteBuf>,
) -> Result<crate::db::FolderRow, RpcError> {
    let by_hash = crate::routes::parse_name_hash(name_hash, |msg| invalid_params(msg))?;

    if let Some(a) = actor_id {
        let actor = bytes32_from_wire(a, "actor_id")?;
        let row = match by_hash {
            Some(h) => state
                .db
                .get_folder_for_actor_by_name_hash(&h, &actor)
                .await
                .map_err(internal)?,
            None => state
                .db
                .get_folder_for_actor(name, &actor)
                .await
                .map_err(internal)?,
        };
        return row.ok_or_else(|| not_found("folder not found"));
    }

    let mut rows = match by_hash {
        Some(h) => state
            .db
            .get_folders_by_name_hash(&h)
            .await
            .map_err(internal)?,
        None => state.db.get_folders_by_name(name).await.map_err(internal)?,
    };
    match rows.len() {
        0 => Err(not_found("folder not found")),
        1 => Ok(rows.remove(0)),
        _ => Err(invalid_params(
            "ambiguous folder reference: multiple actors own a set with this name; pass actor_id",
        )),
    }
}

fn folder_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.folders.get").await?;
            let req: AdminFolderGetRequest = decode(&payload).map_err(malformed)?;

            let fs =
                resolve_folder_by_name(&state, &req.name, &req.name_hash, &req.actor_id).await?;
            let members = state
                .db
                .get_folder_members(fs.id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|m| AdminFolderMember {
                    device_id: ByteBuf::from(m.device_id),
                    flags: m.flags,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&AdminFolderGetReply {
                id: fs.id,
                name: fs.name,
                name_hash: fs.name_hash.map(ByteBuf::from),
                actor_id: ByteBuf::from(fs.actor_id),
                node_cache: fs.node_cache,
                members,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.folders.add_member (≡ POST .../{name}/members) ─────────────

fn folder_add_member_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.folders.add_member").await?;
            let req: AdminFolderAddMemberRequest = decode(&payload).map_err(malformed)?;

            // Twin order: device-id parse, then lookup. Every flag point is a
            // valid place (folders re-model § Places).
            let device_id = bytes32_from_wire(&req.device_id, "device_id")?;
            let fs =
                resolve_folder_by_name(&state, &req.name, &req.name_hash, &req.actor_id).await?;
            state
                .db
                .add_folder_member(fs.id, &device_id, &req.flags)
                .await
                .map_err(internal)?;
            encode_reply(&AdminOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.services.list (≡ GET /admin/api/services) ────────────────────

fn services_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.services.list").await?;
            let _req: AdminServicesListRequest = decode(&payload).map_err(malformed)?;

            let intent = crate::services::ServiceIntent::read_from(&state.services_json_path)
                .map_err(internal)?;
            encode_reply(&AdminServicesListReply {
                version: intent.version as i64,
                services: AdminServiceFlags {
                    bridge: intent.services.bridge,
                    pairing: intent.services.pairing,
                    extra: Default::default(),
                },
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.services.update (≡ PUT /admin/api/services/{name}) ───────────

fn services_update_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.services.update").await?;
            let req: AdminServiceUpdateRequest = decode(&payload).map_err(malformed)?;

            let mut intent = crate::services::ServiceIntent::read_from(&state.services_json_path)
                .map_err(internal)?;
            match req.name.as_str() {
                "bridge" => intent.services.bridge = req.enabled,
                "pairing" => intent.services.pairing = req.enabled,
                other => {
                    return Err(invalid_params(format!(
                        "unknown service: {other}. Valid: bridge, pairing"
                    )));
                }
            }
            intent
                .write_to(&state.services_json_path)
                .map_err(internal)?;
            encode_reply(&AdminServiceUpdateReply {
                ok: true,
                service: req.name,
                enabled: req.enabled,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.admin.logs (the nest's in-memory `fauna-log` ring snapshot) ─────────

/// `fauna_log::LogEntry` → the wire `AdminLogEntry`. The millis timestamp rides
/// as the admin wire convention's `i64` (the ring's `u64` never approaches the
/// `i64` ceiling — it is wall-clock millis). Levels map 1:1.
fn log_entry_to_wire(e: fauna_log::LogEntry) -> AdminLogEntry {
    let level = match e.level {
        fauna_log::LogLevel::Error => AdminLogLevel::Error,
        fauna_log::LogLevel::Warn => AdminLogLevel::Warn,
        fauna_log::LogLevel::Info => AdminLogLevel::Info,
        fauna_log::LogLevel::Debug => AdminLogLevel::Debug,
        fauna_log::LogLevel::Trace => AdminLogLevel::Trace,
    };
    AdminLogEntry {
        timestamp_ms: e.timestamp_ms as i64,
        level,
        target: e.target,
        message: e.message,
        extra: Default::default(),
    }
}

/// `fauna.admin.logs` — the nest's in-memory `fauna-log` ring snapshot for the
/// admin Logs view (`observability.md` § Surfaces). Reads the process-global
/// ring `main()` installs (`fauna_log::RingLayer`); no DB. Admin-only (the ring
/// can carry deployment-operational targets/metadata) and replay-safe.
fn logs_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.logs").await?;
            let _req: AdminLogsRequest = decode(&payload).map_err(malformed)?;
            // Merged: the nest's own ring PLUS the sidecar log plane's remote
            // ring, timestamp-ordered into the one existing reply shape
            // (`observability.md` § The sidecar log plane → *The remote ring*).
            // The split ring is a security property — a chatty or hostile
            // sidecar can never evict the nest's own history — and the merge is
            // what makes plane entries appear inline on `admin-logs` with no
            // client change.
            let entries = fauna_log::snapshot_merged()
                .into_iter()
                .map(log_entry_to_wire)
                .collect();
            encode_reply(&AdminLogsReply {
                entries,
                extra: Default::default(),
            })
        })
    })
}

// C4 pairings RETIRED — pairing is authorized/revoked by the user via
// `fauna.pair.{add,revoke}` (`crate::pair_handlers`); the admin's only
// control is the nest-level `pairing` service knob (`fauna.admin.services.*`
// above). Per the 2026-05-25 per-user-pairing design.

// ── fauna.admin.factory_reset ────────────────────────────────────────────────

/// Return the nest to fresh / unclaimed via a restart-wipe (`factory_reset.rs`).
/// Stages a marker carrying the next claim code, replies with that code, then
/// exits so the s6 supervisor restarts the process into a pre-claim wipe. The
/// reply must flush before the exit, so the exit is scheduled on a short delay
/// after this handler returns.
fn factory_reset_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.admin.factory_reset").await?;
            let req: FactoryResetRequest = decode(&payload).map_err(malformed)?;

            // Post-reset claim code: caller-pinned (trimmed, non-empty) or a
            // fresh random one. The same value is staged + returned, so the
            // caller can drive the re-claim without reading /data off the box.
            let claim_code = match req.new_claim_code {
                Some(c) if !c.trim().is_empty() => c.trim().to_string(),
                _ => crate::claim::generate_claim_code(),
            };

            // Stage the marker (carries the claim code through the wipe).
            let data_dir = crate::factory_reset::data_dir_for_db(&state.config.nest.db_path);
            crate::factory_reset::stage_factory_reset(&data_dir, &claim_code).map_err(internal)?;

            tracing::warn!(
                actor = %hex::encode(actor_id),
                "factory_reset staged — nest will restart into a wipe"
            );

            // Schedule the exit AFTER this reply flushes to the caller. The s6
            // `longrun` supervisor restarts the process into the wipe
            // (`maybe_run_factory_reset` at startup). 750ms is far more than the
            // WS frame needs to flush.
            let db = state.db.clone();
            // The resolved (client-set) NAT axis, captured before the spawn — the
            // MTA-down reconcile must match what the running supervisor was gated
            // on, not the boot seed.
            let node_mode = *state.node_mode.read().await;
            // spawn-ok(cross-generation-messenger): schedules the factory-reset process exit — supervision boundary, not a generation task
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(750)).await;
                // Bring the running mail-bridge services DOWN before the wipe.
                // The wipe deletes the bridge enrollment rows, but the long-lived
                // `fauna-mail-bridge-{mta,mda}` s6 services keep their old keypair
                // and would otherwise loop forever on `/auth/verify 404 actor not
                // registered` (they only request enrollment at cold boot). Signal
                // the supervisor (same path as `set_mail_enabled(false)`) so s6
                // SIGTERM-drains them and holds them `s6-svc -d` across the
                // restart; the freshly-claimed nest starts mail-off, and the next
                // `set_mail_enabled(true)` `s6-svc -u`'s them into a clean cold
                // boot → fresh `request_enrollment`. Best-effort (socket absent on
                // a dev box / e2e harness is a no-op). All protocols off → the
                // MDA (IMAP + CalDAV + CardDAV + WebDAV) goes down too. (All off
                // ⇒ the node_mode gate is moot here, but the signature carries
                // it.)
                crate::mail_enable::reconcile_supervisor(node_mode, false, false, false, false)
                    .await;
                if let Err(e) = db.flush().await {
                    tracing::error!("factory_reset: db flush before exit failed: {e}");
                }
                tracing::warn!("factory_reset: exiting 0 for supervisor restart");
                std::process::exit(0);
            });

            encode_reply(&FactoryResetReply {
                claim_code,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ─────────────────────────────────────────────────

/// Register the admin cluster on the **bearer** router:
///
/// - **C1** user-management — reads (`users.{list,get}`, `evictions.list`),
///   mutations (`users.{create,update,delete,clear_handle}`), lifecycle actions
///   (`users.{evict,cancel_eviction,suspend}`).
/// - **C2** admin-management — `tiers.{list,create,update}`,
///   `invite_codes.{list,create,delete}`, `invite_requests.{list,approve,deny}`,
///   `admins.{list,add,remove}`.
/// - **C3** stats / audit / ops — `stats`, `status`, `audit.{list,integrity}`,
///   `cluster.status`, `gc`, `worker.status`, `pending_actions.list`.
/// - **C5** folders / services — `folders.{create,get,add_member}`,
///   `services.{list,update}`. (The `wireguard.*` arms died with the
///   WireGuard stack, 2026-08-23.)
/// - **C4** pairings — RETIRED (per-user-pairing design): pairing is
///   authorized/revoked by the user via `fauna.pair.{add,revoke}`; the
///   admin's only control is the nest-level `pairing` service knob.
///
/// All `forbid_replay = false` @5 s — except `deployment_seed.rotate` @60 s (its
/// transaction re-keys the KEK satellites). Replay semantics + rationale: see
/// `KindRegistry::register_admin_kinds`.
pub fn register_admin_handlers(b: &mut RpcRouterBuilder) {
    fn meta(handler: RpcHandler) -> RpcKindMeta {
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler,
        }
    }
    // C1 — user management.
    b.add("fauna.admin.users.list", meta(list_handler()));
    b.add("fauna.admin.users.get", meta(get_handler()));
    b.add("fauna.admin.users.create", meta(create_handler()));
    b.add("fauna.admin.users.update", meta(update_handler()));
    b.add("fauna.admin.users.delete", meta(delete_handler()));
    b.add(
        "fauna.admin.users.clear_handle",
        meta(clear_handle_handler()),
    );
    b.add("fauna.admin.users.evict", meta(evict_handler()));
    b.add(
        "fauna.admin.users.cancel_eviction",
        meta(cancel_eviction_handler()),
    );
    b.add("fauna.admin.users.suspend", meta(suspend_handler()));
    b.add("fauna.admin.evictions.list", meta(evictions_list_handler()));

    // C2 — admin management.
    b.add("fauna.admin.tiers.list", meta(tiers_list_handler()));
    b.add("fauna.admin.tiers.create", meta(tiers_create_handler()));
    b.add("fauna.admin.tiers.update", meta(tiers_update_handler()));
    b.add(
        "fauna.admin.membership_tiers.list",
        meta(membership_tiers_list_handler()),
    );
    b.add(
        "fauna.admin.membership_tiers.set",
        meta(membership_tiers_set_handler()),
    );
    b.add(
        "fauna.admin.membership_tiers.clear",
        meta(membership_tiers_clear_handler()),
    );
    b.add(
        "fauna.admin.invite_codes.list",
        meta(invite_codes_list_handler()),
    );
    b.add(
        "fauna.admin.invite_codes.create",
        RpcKindMeta {
            // NOT idempotent on its minting branch: an empty `req.code` makes
            // the handler allocate a fresh `admin::generate_invite_code()` and
            // plain-`INSERT INTO invite_codes` keyed on it, so a replay lands a
            // SECOND independently redeemable admission credential (with its
            // own `uses_left`, and its own guardian designation) against one
            // admin action. The admin-supplied-code branch *is* idempotent (the
            // UNIQUE on `code` rejects the repeat), but `forbid_replay` is
            // per-kind, so the non-idempotent branch decides it — same shape
            // and same reasoning as `fauna.payments.claims.mint`. Standing
            // evidence:
            // `db::tests::creating_twice_yields_two_independently_redeemable_invite_codes`.
            // Mirror any change in `KindRegistry::register_admin_kinds`.
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: invite_codes_create_handler(),
        },
    );
    b.add(
        "fauna.admin.invite_codes.delete",
        meta(invite_codes_delete_handler()),
    );
    b.add(
        "fauna.admin.invite_requests.list",
        meta(invite_requests_list_handler()),
    );
    b.add(
        "fauna.admin.invite_requests.approve",
        meta(invite_requests_approve_handler()),
    );
    b.add(
        "fauna.admin.invite_requests.deny",
        meta(invite_requests_deny_handler()),
    );
    b.add("fauna.admin.admins.list", meta(admins_list_handler()));
    b.add("fauna.admin.admins.add", meta(admins_add_handler()));
    b.add("fauna.admin.admins.remove", meta(admins_remove_handler()));
    // The co-admin seed hand-off (`nest/box-recovery.md` § Mechanism) — the
    // claim hand-off generalized to any current roster admin.
    b.add(
        "fauna.admin.deployment_seed.get",
        meta(deployment_seed_get_handler()),
    );
    // The one admin kind that is not @5 s: its transaction re-keys every
    // nest-internal KEK satellite (`nest_kek::reencrypt_satellites`), which is
    // AEAD work per row. Must match `KindRegistry::register_admin_kinds` — the
    // bijection test `router_and_kind_registry_agree_on_every_kind` pins it.
    b.add(
        "fauna.admin.deployment_seed.rotate",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: deployment_seed_rotate_handler(),
        },
    );

    // C3 — stats / audit / ops.
    b.add("fauna.admin.stats", meta(stats_handler()));
    b.add("fauna.admin.status", meta(status_handler()));
    b.add("fauna.admin.audit.list", meta(audit_list_handler()));
    b.add(
        "fauna.admin.audit.integrity",
        meta(audit_integrity_handler()),
    );
    b.add("fauna.admin.cluster.status", meta(cluster_status_handler()));
    b.add("fauna.admin.gc", meta(gc_handler()));
    b.add("fauna.admin.worker.status", meta(worker_status_handler()));
    b.add(
        "fauna.admin.pending_actions.list",
        meta(pending_actions_list_handler()),
    );

    // C5 — folders / services.
    b.add("fauna.admin.folders.create", meta(folder_create_handler()));
    b.add("fauna.admin.folders.get", meta(folder_get_handler()));
    b.add(
        "fauna.admin.folders.add_member",
        meta(folder_add_member_handler()),
    );
    b.add("fauna.admin.services.list", meta(services_list_handler()));
    b.add(
        "fauna.admin.services.update",
        meta(services_update_handler()),
    );

    // C6 — observability: the admin Logs view's read of the nest log ring.
    b.add("fauna.admin.logs", meta(logs_handler()));

    // Factory reset — destructive return-to-fresh via restart-wipe.
    b.add("fauna.admin.factory_reset", meta(factory_reset_handler()));

    // C4 — pairings RETIRED (per-user-pairing design): no admin kinds.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(max_devices: i64) -> TierRow {
        TierRow {
            name: "custom".into(),
            max_inbox_bytes: 1,
            max_storage_bytes: 1,
            max_devices,
            max_blob_size: 1,
            max_feeds: 1,
        }
    }

    /// The bound exists so an admin cannot set a tier whose **honest** fleet
    /// would overrun `fauna_sync_engine::MAX_FRONTIER_WRITERS` — a ceiling
    /// whose own sizing comment names `max_devices` as its first term. Before
    /// it, any non-negative `i64` was accepted, so the ceiling's justification
    /// rested on a number nothing bounded.
    #[test]
    fn max_devices_is_bounded_against_the_frontier_writer_ceiling() {
        assert!(validate_tier_caps(&tier(MAX_TIER_MAX_DEVICES)).is_ok());
        let err = validate_tier_caps(&tier(MAX_TIER_MAX_DEVICES + 1))
            .expect_err("one past the bound is refused");
        assert_eq!(err.code, "fauna.admin.invalid_params", "got {err:?}");

        // The relationship, not just the number: a future edit that raises the
        // ceiling raises this with it, and one that raises this alone fails.
        assert!(
            (MAX_TIER_MAX_DEVICES as usize) < fauna_sync_engine::MAX_FRONTIER_WRITERS,
            "the tier bound must leave the frontier ceiling headroom for its \
             other terms (successions, the nest, per-member on a shared set)"
        );
    }

    /// The shipped tiers must remain settable through the admin door they are
    /// edited by — a bound that refuses `db/migrations.rs`'s own seeds would
    /// wedge the tier editor on a fresh nest.
    #[test]
    fn the_shipped_seeds_all_validate() {
        for seeded in [2, 5, 10] {
            assert!(
                validate_tier_caps(&tier(seeded)).is_ok(),
                "the shipped seed {seeded} must validate"
            );
        }
    }

    #[test]
    fn a_negative_cap_is_still_refused() {
        assert!(validate_tier_caps(&tier(-1)).is_err());
    }
}
