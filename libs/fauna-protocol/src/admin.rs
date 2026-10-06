//! Admin (Layer-5) WS-RPC payload types — the admin surface migrating
//! `/admin/api/*` onto the per-actor **bearer** WS-RPC connection. Admin is a
//! Fauna app (product invariant: nest configuration is set from clients,
//! not CLI/HTTP), so the admin tools speak the same WS-RPC transport as every
//! other client. Track C of the WS-RPC-everywhere migration (tracked internally).
//!
//! This module holds the full **user-management cluster (C1)** — the reads
//! `fauna.admin.users.{list,get}`, the mutations `{create,update,delete,
//! clear_handle}`, the lifecycle actions `{evict,cancel_eviction,suspend}`, and
//! the cross-user `fauna.admin.evictions.list` — a behavior-preserving
//! transport migration of the bearer-authed `admin::*` HTTP handlers
//! (`bins/fauna-nest/src/admin.rs`). The handlers reshape the same `CacheDb`
//! rows / side effects the twins produce (no shared core — one DB call plus
//! reply shaping, like `stats_handlers`), preserving the
//! twins' audit-log writes, pending-action scheduling (delete / suspend), and
//! `AccountUpdated` pushes (clear_handle / evict / cancel_eviction).
//!
//! **Admin-only.** The HTTP twins gate `AdminBearerAuth`; the kinds gate
//! `Admin` in `bridge_method_allowlist::is_permitted` — the
//! `fauna.pending_actions.approve` (B20) admin precedent.
//!
//! Wire convention (matching `stats.rs`): actor ids ride as raw
//! 32-byte `ByteBuf` (the dag-cbor-native id shape used by the events /
//! folders clusters — the twin's hex string is a JSON artifact); every count
//! and timestamp is `i64`; booleans are `bool`; no floats (the dag-cbor wire
//! forbids them — none arise here); optionals are plain `Option`. Every struct
//! carries a `#[serde(flatten, default)] extra` forward-compat map.
//!
//! Kind registry: `kind.rs::register_admin_kinds`.

use fauna_core::secret::SecretString;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;

/// In-flight eviction sub-state for a user. Present (`Some`) only when an
/// eviction is underway — the twin omits the `eviction` key when
/// `eviction_status` is empty (`UserRow`, `bins/fauna-nest/src/db/mod.rs`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminEviction {
    pub status: String,
    pub reason: String,
    pub category: String,
    pub warned_at: Option<i64>,
    pub suspend_at: Option<i64>,
    pub delete_at: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One row of the admin user-management surface (the twin's per-user JSON
/// object from `admin::{get_user, list_users_paginated}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUser {
    /// Raw 32-byte actor id (the twin emitted `hex::encode(actor_id)`).
    pub actor_id: ByteBuf,
    pub tier: String,
    pub label: String,
    /// The account's handle — unique on the nest by construction, and what
    /// identifies a user in an admin picker (`admin.md` § 2; the display
    /// `label` is freely editable and non-unique, so it is never a picker
    /// identity). `None` for a blank-handle admission (the nest folds
    /// its stored empty string to absent), which is why pickers keep a
    /// full-actor-hex fallback. Additive
    /// 2026-08-30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    pub suspended: bool,
    pub created_at: i64,
    pub inbox_bytes_used: i64,
    pub storage_bytes_used: i64,
    /// `Some` only while an eviction is in flight.
    #[serde(default)]
    pub eviction: Option<AdminEviction>,
    /// Read-only audit indicator (the admin **sees** it, never sets it):
    /// whether *this* nest's MDA serves this actor's mail/calendar over
    /// IMAP/CalDAV. Default **on**; the user alone controls it from their own
    /// `mail-settings` serve-here toggle (`fauna.bridges.set_mail_serving_enabled`,
    /// User-class, caller-scoped). The nest projection resolves the per-actor
    /// `actor_mail_serving` flag (absent ⇒ on) before sending, so a plain `bool`
    /// reaches the client. See `deployment-home-with-public-relay.md` § MUA reach
    /// and `mail-settings.md` § Local IMAP/CalDAV-serving toggle. A missing key on
    /// the wire decodes to **on** (`default_mail_serving_enabled`), faithful to the
    /// nest's `actor_mail_serving` "absent ⇒ serve" invariant.
    #[serde(default = "default_mail_serving_enabled")]
    pub mail_serving_enabled: bool,
    /// Whether this actor holds the admin role (`admin_actor_ids`). Read-only:
    /// the role is granted/revoked through `fauna.admin.admins.{add,remove}`,
    /// never from a Users row. It is on this projection so a client can decline
    /// to *offer* the lifecycle controls the nest would refuse anyway — an admin
    /// cannot be suspended, evicted, or deleted (`fauna.admin.conflict`;
    /// `admin.md` § 2 Users → *Cutting a user off*), and rendering a control that
    /// always errors is the dead-button the doc's own gotcha warns against. A
    /// missing key on the wire decodes to `false`, which is the safe direction:
    /// a row without it *offers* the controls
    /// and surfaces the nest's `fauna.admin.conflict` in `admin-users-action-error`
    /// — degraded UX, never a wrong authorization decision (the nest is the only
    /// enforcer; this flag is presentation).
    #[serde(default)]
    pub is_admin: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.users.list (≡ GET /admin/api/users?limit&offset) ─────────────

/// The page `fauna.admin.users.list` answers when the request names no `limit`
/// (the twin's default).
pub const USERS_LIST_DEFAULT_LIMIT: i64 = 50;

/// The largest page `fauna.admin.users.list` answers; a larger `limit` is
/// clamped to it. A caller reading every account pages at this size
/// (`fauna_client_admin::users_list_all`).
pub const USERS_LIST_MAX_LIMIT: i64 = 500;

/// List users, paginated, newest account first. `limit` absent →
/// [`USERS_LIST_DEFAULT_LIMIT`]; the handler clamps it to
/// `1..=`[`USERS_LIST_MAX_LIMIT`] and `offset` to `>= 0` (the twin's `clamp`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUsersListRequest {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A page of users plus the unpaginated `total` (the twin's
/// `{"users": [...], "total": n}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUsersListReply {
    pub users: Vec<AdminUser>,
    pub total: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.users.get (≡ GET /admin/api/users/{actor_id}) ────────────────

/// Fetch one user by actor id. A missing user is a `fauna.admin.not_found`
/// `RpcError` (the twin's `404`), so the reply always carries a `user`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUserGetRequest {
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The requested user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUserGetReply {
    pub user: AdminUser,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Shared replies ───────────────────────────────────────────────────────────

/// Success-only reply for the mutations/actions whose HTTP twin returned
/// `{"ok": true}` (create / update / clearHandle / evict / cancelEviction).
/// `ok` is always `true` — failures surface as `RpcError`s, never `ok: false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminOkReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The queued destructive-operation reply (the HTTP twins' `202 Accepted`
/// body) shared by `delete` + `suspend`. Both schedule a `pending_actions`
/// row (api-layers.md § "Destructive operations are delayed"); the admin polls
/// `fauna.pending_actions.*` for status. Mirrors `account::AccountDeleteReply`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminPendingActionReply {
    pub pending_action_id: i64,
    /// Earliest execution timestamp (epoch secs — the twin's `execute_after`).
    pub execute_after: i64,
    /// Always `"pending"` on creation.
    pub status: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.users.create (≡ POST /admin/api/users) ───────────────────────

/// Create a user. `actor_id` rides as raw 32 bytes (the twin parsed 64 hex
/// chars). A duplicate actor id is `fauna.admin.conflict` (the twin's `409`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUserCreateRequest {
    pub actor_id: ByteBuf,
    pub tier: String,
    #[serde(default)]
    pub label: String,
    /// The handle to admit this actor under.
    ///
    /// `public-mode.md` § Registration & Identity holds that an account comes
    /// into being through exactly three paths and that *registering is choosing
    /// a handle* — an admin admitting a user directly is one of those three, so
    /// it carries a handle like the other two. Omitting it (or sending it blank)
    /// admits a handle-less actor — the admit form's deliberately-blank-handle
    /// choice (`admin.md` § 2 Users → Section 3 — Admit), a product state, not
    /// an older-client arm. Such an actor cannot send email — the nest's sender-handle verification has no
    /// handle to match (`mail-app-surface.md` § First-party client send).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Supervised admission: link the admitted account to this guardian actor
    /// (32 raw bytes) — the direct-admission arm of `family-safety.md` § The
    /// guardianship link (all three account-creation paths carry the
    /// designation; same validation as the invite-code mint and the
    /// request-approval, plus the guardian must not be the admitted actor).
    /// Requires `handle`: a handle-less admission cannot be supervised.
    /// Additive 2026-08-30.
    #[serde(default)]
    pub guardian_actor: Option<ByteBuf>,
    /// The age band the admitted account starts under (`family-safety.md`
    /// § The account age band — the admin/guardian picks it at admission,
    /// exactly like the tier; provenance `guardian-asserted`). Same
    /// validation as mint/approve: a nameable token, and requires
    /// `guardian_actor`. Additive 2026-08-30.
    #[serde(default)]
    pub age_band: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.users.update (≡ PUT /admin/api/users/{actor_id}) ─────────────

/// Update a user's tier + label. A missing user is `fauna.admin.not_found`
/// (the twin's `404`). Suspension is not settable here — use the eviction flow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUserUpdateRequest {
    pub actor_id: ByteBuf,
    pub tier: String,
    #[serde(default)]
    pub label: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.users.delete (≡ DELETE /admin/api/users/{actor_id}) ──────────

/// Schedule a user-deletion pending action. A missing user is
/// `fauna.admin.not_found`; success returns `AdminPendingActionReply`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUserDeleteRequest {
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.users.clear_handle (≡ DELETE /admin/api/users/{id}/handle) ───

/// Force-release a user's handle (clear only — admins cannot reassign it; a
/// handle change requires user consent). Pushes `AccountUpdated{["handle"]}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUserClearHandleRequest {
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.users.evict (≡ POST /admin/api/users/{id}/evict) ─────────────

/// Start the eviction timeline (warn → suspend → delete). `category` must be
/// one of `terms`/`capacity`/`legal`/`abuse`/`other` and `reason` non-empty
/// (else `fauna.admin.invalid_params`); a missing or already-evicting user is
/// `fauna.admin.conflict` (the twin's `409`). Pushes `AccountUpdated`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUserEvictRequest {
    pub actor_id: ByteBuf,
    pub reason: String,
    pub category: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.users.cancel_eviction (≡ POST .../cancel-eviction) ───────────

/// Cancel an in-flight eviction. No active eviction is `fauna.admin.not_found`
/// (the twin's `404`). Deletes the eviction export tokens; pushes `AccountUpdated`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUserCancelEvictionRequest {
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.users.suspend (≡ POST /admin/api/users/{id}/suspend) ─────────

/// Suspend a user **immediately** — cut them off now, with no delete timeline.
///
/// Suspension is the eviction machine's `suspended` state entered directly, so
/// `fauna.admin.users.cancel_eviction` is the restore path (`admin.md` § 2
/// Users → Suspension). A missing user is `fauna.admin.not_found`; an **admin**
/// target is `fauna.admin.conflict` (demote via `fauna.admin.admins.remove`
/// first — a suspended sole admin would be unrecoverable). Success returns
/// `AdminOkReply`.
///
/// `reason` / `category` are optional and additive; empty means
/// `"suspended by admin"` / `"other"`. `category` shares `AdminUserEvictRequest`'s
/// vocabulary (terms / capacity / legal / abuse / other). (The HTTP twin additionally
/// gated on a role-tier extractor since removed as dead — the roster is single-role
/// today, so the `Admin` caller-class gate was always behavior-preserving.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUserSuspendRequest {
    pub actor_id: ByteBuf,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub category: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.evictions.list (≡ GET /admin/api/evictions) ──────────────────

/// List every user with an in-flight eviction. Takes no parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminEvictionsListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The users currently under eviction. Each entry is a full `AdminUser` (its
/// `eviction` is always `Some` here) — the richer shape of the twin's
/// `{actor_id, tier, label, eviction}` subset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminEvictionsListReply {
    pub evictions: Vec<AdminUser>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ═════════════════════════════════════════════════════════════════════════════
// C2 — admin-management cluster (tiers / invite codes / invite requests / admins)
// ═════════════════════════════════════════════════════════════════════════════
//
// The four `/admin/api/{tiers,invite-codes,invite-requests,admins}` route
// groups, named `fauna.admin.{tiers,invite_codes,invite_requests,admins}.*`
// (the `admin.` prefix keeps them distinct from the pre-identity public invite
// flow `fauna.account.invite_{request,code}.*` — Track A5). Same shape rules as
// C1: every count/timestamp `i64`, no floats, raw `ByteBuf` actor ids, a
// `#[serde(flatten, default)] extra` on every struct. The `add`/`remove`-admin
// kinds reuse the shared `AdminPendingActionReply`; the success-only mutations
// reuse `AdminOkReply`.

// ── Tiers (≡ /admin/api/tiers) ───────────────────────────────────────────────

/// A storage/feature tier (the twin's `TierRow` JSON) — byte/count caps only,
/// no floats.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminTier {
    pub name: String,
    pub max_inbox_bytes: i64,
    pub max_storage_bytes: i64,
    pub max_devices: i64,
    pub max_blob_size: i64,
    pub max_feeds: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.tiers.list (≡ GET /admin/api/tiers) — no parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminTiersListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Every defined tier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminTiersListReply {
    pub tiers: Vec<AdminTier>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.tiers.create (≡ POST /admin/api/tiers). An empty `name` is
/// `fauna.admin.invalid_params` (the twin's 400); a duplicate name is
/// `fauna.admin.conflict` (the twin's 409 on UNIQUE).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminTierCreateRequest {
    pub name: String,
    pub max_inbox_bytes: i64,
    pub max_storage_bytes: i64,
    pub max_devices: i64,
    pub max_blob_size: i64,
    pub max_feeds: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.tiers.update (≡ PUT /admin/api/tiers/{name}). A missing tier is
/// `fauna.admin.not_found` (the twin's 404).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminTierUpdateRequest {
    pub name: String,
    pub max_inbox_bytes: i64,
    pub max_storage_bytes: i64,
    pub max_devices: i64,
    pub max_blob_size: i64,
    pub max_feeds: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Membership tiers (monetization.md § Pillar 4) ────────────────────────────
//
// The membership *designation*: the link that makes one of the admin's own
// subscription tiers (the entitlement object) mean "paid access to this nest",
// by naming the quota tier (`tiers`) an admitted member is assigned and the one
// a lapsed member degrades to. The two tier systems stay distinct concepts
// joined by this link, never merged — which is why this is its own kind family
// beside `fauna.admin.tiers.*` (quota policy) rather than a field on either.
//
// No twin: this family is WS-RPC-native (there was never an `/admin/api`
// endpoint for it). Additive nest state + additive wire, `version-compatibility.md`
// I4 — a nest with no designation behaves exactly as before.

/// The quota tier a lapsed member degrades to when a designation does not name
/// one (monetization.md § Pillar 4: "`lapse_tier` defaults to `free`"). Shared
/// so a client can render the default it will get without hard-coding it.
pub const DEFAULT_LAPSE_TIER: &str = "free";

/// One membership designation: `(admin actor, subscription tier) →
/// { admin_tier, lapse_tier }`. `tier_name` names a `subscription_tiers` row
/// owned by the calling admin; `admin_tier` / `lapse_tier` name `tiers` rows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminMembershipTier {
    /// The admin's own subscription tier this designation applies to.
    pub tier_name: String,
    /// Quota tier an admitted member is assigned (`users.tier`).
    pub admin_tier: String,
    /// Quota tier a lapsed member degrades to. Never a suspension — lapse is a
    /// reversible quota downgrade (monetization.md § Pillar 4, Rail C step 3).
    pub lapse_tier: String,
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.membership_tiers.list — no parameters. Scoped to the calling
/// admin's own designations (an admin is a payee like any other).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminMembershipTiersListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Every membership designation the calling admin owns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminMembershipTiersListReply {
    pub membership_tiers: Vec<AdminMembershipTier>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.membership_tiers.set — designate one of the caller's own
/// subscription tiers as a membership tier. An **upsert**: re-designating the
/// same tier re-points the link rather than conflicting.
///
/// A `tier_name` the caller does not own (or that does not exist) is
/// `fauna.admin.not_found`; an `admin_tier` / `lapse_tier` naming no `tiers`
/// row is `fauna.admin.invalid_params`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminMembershipTierSetRequest {
    pub tier_name: String,
    pub admin_tier: String,
    /// Omitted ⇒ `free` (monetization.md § Pillar 4: "`lapse_tier` defaults to
    /// `free`"). `Option` rather than a bare `String` so an older client that
    /// never learned the field still lands on the documented default.
    #[serde(default)]
    pub lapse_tier: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.membership_tiers.clear — drop the designation, leaving the
/// underlying subscription tier untouched (it reverts to an ordinary
/// content tier). A tier carrying no designation is `fauna.admin.not_found`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminMembershipTierClearRequest {
    pub tier_name: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Invite codes (≡ /admin/api/invite-codes) ─────────────────────────────────

/// One closed-registration invite code (the twin's `InviteCodeRow` JSON).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteCode {
    pub code: String,
    pub tier: String,
    pub uses_left: i64,
    pub created_at: i64,
    /// Supervised admission: the guardian actor the redeemed account will be
    /// linked to (`family-safety.md` § Wire & data shape). `None` = an
    /// ordinary (unsupervised) code. Additive 2026-07-09.
    #[serde(default)]
    pub guardian_actor: Option<ByteBuf>,
    /// The age band the redeemed account is admitted under
    /// ([`crate::age::AgeBand`] wire token; `family-safety.md` § The account
    /// age band — set beside the guardian, provenance `guardian-asserted`).
    /// `None` = no band chosen (an ordinary code).
    /// Additive 2026-08-24.
    #[serde(default)]
    pub age_band: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.invite_codes.list (≡ GET /admin/api/invite-codes) — no parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteCodesListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Every invite code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteCodesListReply {
    pub invite_codes: Vec<AdminInviteCode>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.invite_codes.create (≡ POST /admin/api/invite-codes). `tier`
/// defaults to `"free"` and `uses` to `1` (so absent fields decode to those).
/// An **empty `code` means "mint one"** — the nest generates a random token and
/// returns it in [`AdminInviteCodeCreateReply::code`]. The admin's real input is
/// tier + uses; the token is just something the invitee types during onboarding,
/// so the create UI carries no code field. A supplied non-empty `code` is used
/// verbatim (vanity / migration); a duplicate is `fauna.admin.conflict`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdminInviteCodeCreateRequest {
    #[serde(default)]
    pub code: String,
    #[serde(default = "default_invite_tier")]
    pub tier: String,
    #[serde(default = "default_invite_uses")]
    pub uses: i64,
    /// Supervised admission: link the redeemed account to this guardian actor
    /// (32 raw bytes). Validated at mint — the guardian must be an existing,
    /// non-suspended, non-supervised user (`family-safety.md` § Wire & data
    /// shape). Additive 2026-07-09.
    #[serde(default)]
    pub guardian_actor: Option<ByteBuf>,
    /// The age band the admitted account starts under (`family-safety.md`
    /// § The account age band — the guardian's dial, provenance
    /// `guardian-asserted`). Validated at mint: must be a token
    /// [`crate::age::AgeBand::from_wire`] names, and **requires
    /// `guardian_actor`** — the band is set exactly where the supervised
    /// designation is set, so a band on an unsupervised code is refused.
    /// Additive 2026-08-24.
    #[serde(default)]
    pub age_band: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The quota tier a newly-admitted member starts at when an invite/direct-
/// admit request doesn't name one explicitly. A DIFFERENT concept from
/// [`DEFAULT_LAPSE_TIER`] (a lapsed member's degrade-to tier, not a new
/// member's start tier) — both currently resolve to `"free"`, but they are
/// independently configurable business decisions (onboarding vs. offboarding),
/// so this stays its own default rather than reusing that one. `pub` so
/// clients can render the shared default instead of hand-copying the literal
/// (FFI export: `libs/fauna-ffi/src/value_format.rs`).
pub fn default_invite_tier() -> String {
    "free".to_string()
}
/// `AdminUser::mail_serving_enabled` defaults to **on** — a missing wire key
/// mirrors the nest's `actor_mail_serving` "absent ⇒ serve" invariant.
fn default_mail_serving_enabled() -> bool {
    true
}
fn default_invite_uses() -> i64 {
    1
}

impl Default for AdminInviteCodeCreateRequest {
    /// Matches the twin's `tier`/`uses` defaults so struct-update fixtures
    /// (`..Default::default()`) carry the production-correct values.
    fn default() -> Self {
        Self {
            code: String::new(),
            tier: default_invite_tier(),
            uses: default_invite_uses(),
            guardian_actor: None,
            age_band: None,
            extra: BTreeMap::new(),
        }
    }
}

/// Reply to `fauna.admin.invite_codes.create` — carries the `code` that now
/// exists, whether the admin supplied it or the nest minted it. Lets the create
/// UI show/copy the token immediately without re-listing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteCodeCreateReply {
    pub code: String,
    #[serde(default)]
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.invite_codes.delete (≡ DELETE /admin/api/invite-codes/{code}). A
/// missing code is `fauna.admin.not_found` (the twin's 404).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteCodeDeleteRequest {
    pub code: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Invite requests (admin side, ≡ /admin/api/invite-requests) ───────────────

/// One in-band invite request awaiting an admin decision (the twin's
/// `status_json`). `actor_id` + `decided_by` ride as raw 32-byte `ByteBuf`
/// (the twin emitted `hex::encode`); `decided_by` / `decided_at` /
/// `denial_reason` are `Some` only once decided.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteRequest {
    pub id: i64,
    pub actor_id: ByteBuf,
    pub handle: String,
    pub message: String,
    pub status: String,
    pub created_at: i64,
    #[serde(default)]
    pub decided_at: Option<i64>,
    #[serde(default)]
    pub decided_by: Option<ByteBuf>,
    #[serde(default)]
    pub denial_reason: Option<String>,
    /// The applicant's age-claim band, when the submit carried one
    /// (`public-mode.md` § Age at registration — **absence-as-signal**: the
    /// admin sees on the request row whether the applicant's app made a claim,
    /// and decides with their own judgment; the require-knob never gates this
    /// path). [`crate::age::AgeBand`] wire token. Additive 2026-08-24.
    #[serde(default)]
    pub age_band: Option<String>,
    /// How the claim above was established, when present — `attested-ios` /
    /// `attested-android` for a claim the nest verified at submit, `none` for
    /// a declared-only claim ([`crate::age::AgeBandProvenance`]). Additive
    /// 2026-08-24.
    #[serde(default)]
    pub age_band_provenance: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl AdminInviteRequest {
    /// The wire `status` value for a request still awaiting an admin decision.
    pub const STATUS_PENDING: &'static str = "pending";

    /// `true` while this request is still awaiting a decision — the single
    /// typed read of the stringly-typed wire `status`. Approve/deny/tier
    /// controls fire only for pending rows; decided rows display for context.
    pub fn is_pending(&self) -> bool {
        self.status == Self::STATUS_PENDING
    }
}

/// fauna.admin.invite_requests.list (≡ GET /admin/api/invite-requests) — no parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteRequestsListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Every invite request (pending and decided).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteRequestsListReply {
    pub invite_requests: Vec<AdminInviteRequest>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.invite_requests.approve (≡ POST .../{id}/approve). Creates the
/// user (actor + handle) and deletes the request. `tier` defaults to `"free"`
/// when absent; a non-empty `label` is applied. A missing request is
/// `fauna.admin.not_found`; a non-pending request, a re-taken handle, or a
/// duplicate actor is `fauna.admin.conflict`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteRequestApproveRequest {
    pub id: i64,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// Supervised admission: link the approved account to this guardian actor
    /// (32 raw bytes) — same validation as the invite-code mint, plus the
    /// guardian must not be the requester (`family-safety.md` § Wire & data
    /// shape). Additive 2026-07-09.
    #[serde(default)]
    pub guardian_actor: Option<ByteBuf>,
    /// The age band the approved account starts under (`family-safety.md`
    /// § The account age band — the admin/guardian picks it at approval time,
    /// exactly like the tier; provenance `guardian-asserted`). Same
    /// validation as the mint: a nameable token, and requires
    /// `guardian_actor`. Additive 2026-08-24.
    #[serde(default)]
    pub age_band: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The approved user (the twin's `{actor_id, handle, tier}` — richer than a
/// bare ok so the client can render the resolved tier).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteRequestApproveReply {
    pub actor_id: ByteBuf,
    pub handle: String,
    pub tier: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.invite_requests.deny (≡ POST .../{id}/deny). Marks the request
/// denied with an optional reason; success returns `AdminOkReply`. A missing
/// request is `fauna.admin.not_found`; a non-pending request is
/// `fauna.admin.conflict`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminInviteRequestDenyRequest {
    pub id: i64,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Admins (≡ /admin/api/admins) ─────────────────────────────────────────────

/// One admin grant (the twin's `{actor_id, added_at}`). `actor_id` rides as raw
/// 32-byte `ByteBuf` (the twin emitted `hex::encode`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAdminEntry {
    pub actor_id: ByteBuf,
    pub added_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.admins.list (≡ GET /admin/api/admins) — no parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAdminsListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Every admin actor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAdminsListReply {
    pub admins: Vec<AdminAdminEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.admins.add (≡ POST /admin/api/admins). Schedules an `AdminAdd`
/// pending action (the grant applies after the delay); returns
/// `AdminPendingActionReply`. A wrong-length actor id is
/// `fauna.admin.invalid_params`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAdminAddRequest {
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.admins.remove (≡ DELETE /admin/api/admins/{actor_id}). Refuses
/// to schedule removing the last superadmin (`fauna.admin.conflict`, the twin's
/// 409); otherwise schedules an `AdminRemove` pending action and returns
/// `AdminPendingActionReply`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAdminRemoveRequest {
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.admin.deployment_seed.rotate` — the deployment-seed rotation ceremony
/// (`nest/box-recovery.md` § Deployment-seed rotation).
///
/// The **client mints and custodies the successor seed before dispatching this**
/// (the CR-1 discipline): a caller cannot flip a box to an identity nobody has
/// persisted, and a crash before dispatch leaves at worst an orphan custody
/// entry. The seed travels the authenticated Admin channel — the claim
/// hand-off's exposure in reverse, to a principal who already holds full
/// nest-admin authority, so it crosses no new trust boundary.
///
/// Same hex + [`SecretString`] convention as [`crate::claim::ClaimAdminReply`]'s
/// `deployment_seed`: 64 hex chars of the raw 32-byte Ed25519 seed, zeroized on
/// drop, redacted in `Debug`, and byte-identical to a `String` on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminDeploymentSeedRotateRequest {
    /// The successor deployment seed, 64-char hex. **Already custodied** by the
    /// caller when this is sent.
    pub new_seed: SecretString,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.admin.deployment_seed.get` — the co-admin seed hand-off
/// (`nest/box-recovery.md` § Mechanism, the co-admin bullet). The claim
/// hand-off generalized from *the claiming admin* to *any current admin*: a
/// later-added co-admin never saw the claim-reply hand-off, so this serves
/// the same deployment signing seed to any current `admin_actor_ids` roster
/// holder over the authenticated Admin channel — identical trust argument to
/// the rotation kind's reverse direction (a roster holder already has full
/// nest-admin authority, so this crosses no new boundary).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminDeploymentSeedGetRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.admin.deployment_seed.get`.
///
/// Same hex + [`SecretString`] convention as [`crate::claim::ClaimAdminReply`]'s
/// `deployment_seed` and [`AdminDeploymentSeedRotateRequest::new_seed`]: 64 hex
/// chars of the raw 32-byte Ed25519 seed, zeroized on drop, redacted in
/// `Debug`, byte-identical to a `String` on the wire. `None` only if this nest
/// holds no deployment signing key at all — should not happen post-boot; the
/// caller treats it as a benign no-op, never an error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminDeploymentSeedGetReply {
    pub deployment_seed: Option<SecretString>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.admin.deployment_seed.rotate`.
///
/// `already_rotated` is the **idempotent ack**: the client minted its seed before
/// dispatch, so a retry after a lost reply re-sends the committed seed. That is a
/// success, not a failure — the box is on the identity the caller asked for. The
/// refusals that are genuinely errors (a superseded ancestor, a pending admin
/// removal) come back as errors instead, because each needs its own admin-facing
/// remedy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminDeploymentSeedRotateReply {
    /// The box's `nest_actor_id` after the ceremony — the successor's public
    /// half, or the unchanged current identity on an idempotent ack.
    pub nest_actor_id: ByteBuf,
    /// The rotation log position this ceremony wrote; `0` on an idempotent ack
    /// (nothing was appended).
    pub seq: u64,
    /// True when the box was already serving this identity and nothing was
    /// written.
    pub already_rotated: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── C3 — stats / audit / ops ─────────────────────────────────────────────────
// (≡ /admin/api/{stats,status,audit,audit/integrity,cluster/status,gc,
//  worker/status,pending-actions}). Behavior-preserving reshapes of the
// `AdminBearerAuth` twins in `bins/fauna-nest/src/admin.rs` (+ the cross-actor
// `/admin/api/pending-actions` twin, now deleted). Every numeric field is an
// `i64`/`bool` — the twins' reply JSON is already float-free (`db::Stats`,
// `db::AuditRow`, `db::BlobStorageStats`, `backup::gc::GcResult`, the nest-link
// `WorkerInfo` are all integer counts / byte totals / wall-clock seconds).

/// fauna.admin.stats (≡ GET /admin/api/stats) — no parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminStatsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Nest-wide counters (the twin's `db::Stats` plus the live `ws_connections`).
/// `users_by_tier` is `(tier_name, count)` pairs (the `db::Stats` shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminStatsReply {
    pub total_users: i64,
    pub users_by_tier: Vec<(String, i64)>,
    pub suspended_users: i64,
    pub total_inbox_bytes: i64,
    pub total_storage_bytes: i64,
    /// Live WebSocket connection count (`WsRegistry::connection_count`).
    pub ws_connections: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.status (≡ GET /admin/api/status) — no parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The available-update advisory (the twin's nested `{version, url}`); the
/// reply's `update_available` is `None` when the nest is up to date.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminUpdateAvailable {
    pub version: String,
    pub url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Runtime status — the running nest version + any pending self-update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminStatusReply {
    pub version: String,
    #[serde(default)]
    pub update_available: Option<AdminUpdateAvailable>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.audit.list (≡ GET /admin/api/audit?limit&before_id). `limit`
/// defaults to 100 and is clamped to `1..=1000` (the twin's clamp); `before_id`
/// pages backward from a known id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAuditListRequest {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub before_id: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One audit-log row (the twin's `db::AuditRow`). `actor_id` is the raw 32-byte
/// admin actor (the twin emitted `Option<Vec<u8>>`); `None` for system entries.
/// `prev_hash`/`entry_hash` are the hash-chain links (the integrity surface).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAuditEntry {
    pub id: i64,
    pub ts: i64,
    #[serde(default)]
    pub actor_id: Option<ByteBuf>,
    pub action: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    pub prev_hash: String,
    pub entry_hash: String,
    /// Which preimage `entry_hash` was computed under — today always `2`, the
    /// length-framed one (`0` and `1` are retired numbers).
    ///
    /// An admin verifies this chain by recomputing each row's hash from the
    /// columns they were served, so the format has to be served too: without
    /// it a verifier must keep an arm for every format live for every row,
    /// which is the interchangeability the version record exists to remove
    /// (`succession-aftermath.md` § Re-key scope, the `audit_log` ruling).
    /// **Select the preimage from this value and refuse a value you do not
    /// know** — never try formats until one matches. Always served: an entry
    /// without it is refused rather than defaulted.
    pub entry_hash_version: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The audit page (newest first, the twin's bare array — wrapped here per the
/// list-reply convention).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAuditListReply {
    pub entries: Vec<AdminAuditEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.audit.integrity (≡ GET /admin/api/audit/integrity) — no params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAuditIntegrityRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Audit-chain integrity summary (the twin's
/// `{head_id, head_hash, chain_length, first_entry_at, last_entry_at}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminAuditIntegrityReply {
    pub head_id: i64,
    pub head_hash: String,
    pub chain_length: i64,
    pub first_entry_at: i64,
    pub last_entry_at: i64,
    /// Rows recording the length-framed preimage.
    #[serde(default)]
    pub entries_v2: i64,
    /// Rows the nest will not verify: the preimage version is one this nest
    /// does not implement. The refuse rule, counted rather than absorbed by
    /// trying formats until one fits.
    #[serde(default)]
    pub entries_unverifiable: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.cluster.status (≡ GET /admin/api/cluster/status) — no params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminClusterStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Blob-storage breakdown (the twin's `db::BlobStorageStats`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminClusterStatusReply {
    pub total_blobs: i64,
    pub total_bytes: i64,
    pub local_blobs: i64,
    pub s3_blobs: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.gc (≡ POST /admin/api/gc). `grace_period_secs` defaults to 1800
/// (30 min) when absent; `dry_run` reports the would-be deletions without
/// removing anything.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminGcRequest {
    #[serde(default)]
    pub grace_period_secs: Option<i64>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Garbage-collection result (the twin's `backup::gc::GcResult`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminGcReply {
    pub dry_run: bool,
    pub deleted_blobs: i64,
    pub deleted_bytes: i64,
    pub live_snapshots: i64,
    pub referenced_manifests: i64,
    pub skipped_grace_period: i64,
    /// Distinct blobs the walk pinned through live signed records — posts
    /// and profiles (`backup-restore.md` § 9 step 2f). Additive (2026-09-09):
    /// an omitted key reads as 0.
    #[serde(default)]
    pub record_blob_refs: i64,
    /// Distinct blobs the walk pinned through live conversation records'
    /// plaintext `attachment_refs` (§ 9 step 2g — the conversation kind's
    /// blob-reachability floor). The observable that a real send's refs
    /// reached the sweep as live; additive like the field above.
    #[serde(default)]
    pub conv_attachment_refs: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The connected nest-link worker's storage figures (the twin's nested `worker`
/// object); the reply's `worker` is `None` when no worker is connected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminWorkerInfo {
    pub max_storage_bytes: i64,
    pub current_usage_bytes: i64,
    pub payload_count: i64,
    pub connected_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.worker.status (≡ GET /admin/api/worker/status) — no params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminWorkerStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Nest-link proxy worker status: whether a worker is connected, its authorized
/// key (hex; `None` when none is configured), the replication count, and the
/// connected worker's storage figures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminWorkerStatusReply {
    #[serde(default)]
    pub authorized_key: Option<String>,
    pub connected: bool,
    pub replication_count: i64,
    #[serde(default)]
    pub worker: Option<AdminWorkerInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One cross-actor pending action (the admin twin's
/// `GET /admin/api/pending-actions` projection). Mirrors the user-scoped
/// `pending_actions::PendingActionSummary` but adds `actor_id` (the owning
/// actor — this is the all-actors list) and `ip_address`, and (like the
/// summary) omits `payload`/`executed_at`. `approvals` is the parsed approver
/// list (the DB stores a JSON-array string; the B20 precedent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminPendingActionSummary {
    pub id: i64,
    pub actor_id: ByteBuf,
    pub action_type: String,
    #[serde(default)]
    pub target: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub execute_after: i64,
    /// Minimum approvals required before execution (`0` = no quorum).
    pub requires_quorum: i64,
    pub approvals: Vec<String>,
    #[serde(default)]
    pub ip_address: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.pending_actions.list (≡ GET /admin/api/pending-actions) — no
/// params. The cross-actor admin list, **distinct** from B20's user-scoped
/// `fauna.pending_actions.list` (which is keyed on the connection actor).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminPendingActionsListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Every actor's pending destructive operations, for admin review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminPendingActionsListReply {
    pub actions: Vec<AdminPendingActionSummary>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── C5 — folders / services ────────────────────────────────────────────────
// (≡ /admin/api/{folders*,services*}). Behavior-preserving reshapes of the
// `AdminBearerAuth` twins in `bins/fauna-nest/src/{folder_routes.rs,
// services.rs}`. The **admin** folders bucket is distinct from the user
// `fauna.folders.*` (Track B14) cluster. Every numeric field is `i64`/`bool`:
// the twins' reply JSON is float-free (blob counts are sizes, service
// `version` is `u32` — no timestamps or transfer/health ratios appear in any
// of these replies). The `AdminWireguard*` types that shared this bucket were
// removed 2026-08-23 with the WireGuard stack.

/// fauna.admin.folders.create (≡ POST /admin/api/file-sets). `actor_id` rides
/// as a raw 32-byte `ByteBuf` (the twin parsed hex); `node_cache` defaults to
/// `false`. The `source_device_id` this once carried retired with the
/// single-source concept (the role contraction) — a stray key rides `extra`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminFolderCreateRequest {
    pub name: String,
    pub actor_id: ByteBuf,
    #[serde(default)]
    pub node_cache: Option<bool>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The created folder's row id + echoed name (the twin's `201` body).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminFolderCreateReply {
    pub id: i64,
    pub name: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.folders.get (≡ GET /admin/api/file-sets/{name}).
///
/// `actor_id` (additive, 2026-07-08): `folders` is unique on
/// `(name, actor_id)` — on a multi-user nest two actors may own same-named
/// sets, so a bare name is ambiguous. `Some` scopes the lookup to that owner;
/// `None` is the name-only lookup, which errors honestly
/// (`fauna.admin.invalid_params`) when the name matches more than one actor's
/// set, instead of silently returning an arbitrary one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminFolderGetRequest {
    pub name: String,
    #[serde(default)]
    pub actor_id: Option<ByteBuf>,
    /// The set's `name_hash` (`fauna_core::path_crypto::set_name_hash`) — the
    /// post-flip way to address a set whose plaintext `name` no longer rests.
    ///
    /// An admin cannot open a set's `name_sealed` (they are not its audience), so
    /// after the flip they address a row by the hash the listing handed them
    /// rather than by a name they cannot read. `(name_hash, actor_id)` is exact by
    /// the `UNIQUE(name_hash, actor_id)` index; a hash alone is ambiguous on a
    /// multi-user nest in exactly the way a bare `name` already is, and is refused
    /// the same way. When present it takes precedence over [`Self::name`].
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One folder member device — its place (`device_id` rode as `hex::encode` on
/// the twin; raw 32-byte `ByteBuf` here).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminFolderMember {
    pub device_id: ByteBuf,
    pub flags: crate::folders::PlaceFlags,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The folder + its members (the twin's nested `GET` body).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminFolderGetReply {
    pub id: i64,
    pub name: String,
    /// The set's `name_hash` (`fauna_core::path_crypto::set_name_hash`) — the
    /// address that survives once the plaintext [`Self::name`] scrubs,
    /// and what an admin, who cannot open the seal, addresses the set by
    /// (`path-sealing.md` § the set-name plane). `None` when the set row carries no
    /// `name_hash`. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    pub actor_id: ByteBuf,
    pub node_cache: bool,
    pub members: Vec<AdminFolderMember>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.folders.add_member (≡ POST /admin/api/file-sets/{name}/members).
/// Writes the device's place with `flags` — any of the eight points. The
/// reply is `AdminOkReply`. `actor_id` disambiguates a cross-actor name
/// collision exactly as on [`AdminFolderGetRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminFolderAddMemberRequest {
    pub name: String,
    #[serde(default)]
    pub actor_id: Option<ByteBuf>,
    /// The set's `name_hash` (`fauna_core::path_crypto::set_name_hash`) — the
    /// post-flip way to address a set whose plaintext `name` no longer rests.
    ///
    /// An admin cannot open a set's `name_sealed` (they are not its audience), so
    /// after the flip they address a row by the hash the listing handed them
    /// rather than by a name they cannot read. `(name_hash, actor_id)` is exact by
    /// the `UNIQUE(name_hash, actor_id)` index; a hash alone is ambiguous on a
    /// multi-user nest in exactly the way a bare `name` already is, and is refused
    /// the same way. When present it takes precedence over [`Self::name`].
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    pub device_id: ByteBuf,
    pub flags: crate::folders::PlaceFlags,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.services.list (≡ GET /admin/api/services) — no params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminServicesListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The sidecar-service enable flags (the twin's `services` object). The
/// vestigial `dns` flag was dropped when nest-side DNS writes were retired
/// (see `services.rs`); a stray `dns` key flows into `extra` via
/// `#[serde(flatten)]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminServiceFlags {
    pub bridge: bool,
    /// Admin nest-level pairing-policy knob (per-user multi-homing) —
    /// default **on** at the nest; gates `fauna.pair.add`. See
    /// `docs/goal/architecture/nest/public-mode.md` § Nest Pairing Policy.
    #[serde(default)]
    pub pairing: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The service intent file contents (the twin's `{version, services}`; the
/// on-disk `version` is `u32`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminServicesListReply {
    pub version: i64,
    pub services: AdminServiceFlags,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.admin.services.update (≡ PUT /admin/api/services/{name}). `name` ∈
/// {bridge,pairing} (`invalid_params` on anything else).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminServiceUpdateRequest {
    pub name: String,
    pub enabled: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The applied flag echo (the twin's `{ok, service, enabled}` body).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminServiceUpdateReply {
    pub ok: bool,
    pub service: String,
    pub enabled: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── C6 — observability / logs (fauna.admin.logs) ─────────────────────────────
// The admin Logs view (`docs/goal/architecture/apps/observability.md`
// § Surfaces): an admin-scoped read of the nest's in-memory `fauna-log` ring,
// rendered by the client's admin surface with the *same widget* as the client's
// own Settings → Logs page. The wire types mirror `fauna_log::{LogEntry,
// LogLevel}` as **standalone** serde structs — the same standalone-mirror
// convention as `AdminUser`/`AdminTier`/… — so the L3 protocol crate stays free
// of the observability crate (which pulls `tracing-subscriber`/`-appender`); the
// nest and the linux app, which depend on both crates, convert at the edge.
//
// ⚠ Redaction (observability.md § Persistence & privacy): these entries land in
// a user-visible (admin) page, so nest call sites must never log message
// plaintext or secret material — levels, targets, operation names, and error
// metadata only.

/// Severity of one captured nest log line — a standalone wire mirror of
/// `fauna_log::LogLevel`. Discriminants run most-severe → least-severe
/// (`Error` first), the ordering the client's level filter relies on (an
/// `Error`-only view is a subset of `All`). Serialized as the variant name
/// (`"Error"`, `"Warn"`, …) — a deterministic dag-cbor text string.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum AdminLogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
    /// A level a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// collapsing). It reads as the least severe level. Never serialized: a
    /// path that would re-emit it fails instead of replacing the newer value.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// One captured nest log line — a standalone wire mirror of `fauna_log::LogEntry`
/// (the millis timestamp rides as the admin wire convention's `i64`, never a
/// float). The client renders these with the same row widget as its own Logs
/// page (priority #2/#3 — shared render, not a per-surface re-implementation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminLogEntry {
    /// Milliseconds since the Unix epoch when the event was recorded.
    pub timestamp_ms: i64,
    pub level: AdminLogLevel,
    /// The `tracing` target (module path), e.g. `fauna_nest::serve`.
    pub target: String,
    /// The event's `message` plus any structured fields (already redacted at the
    /// call site — see the module note above).
    pub message: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.logs (the nest's in-memory `fauna-log` ring snapshot) ─────────

/// Read the nest's current log ring. Parameterless (the client filters by
/// severity in its own widget, mirroring the Settings → Logs page's client-side
/// filter over the local ring); the forward-compat `extra` leaves room to add a
/// server-side `min_level` later without a wire break. Replay-safe pure read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminLogsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The nest's `fauna-log` ring snapshot, oldest-first (the client renders
/// newest-first). Bounded by `fauna_log::RING_CAPACITY`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminLogsReply {
    pub entries: Vec<AdminLogEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── C4 — pairings: RETIRED ───────────────────────────────────────────────────
// The admin pairing kinds (`fauna.admin.pairings.{list,approve}`) and their
// types were removed by the per-user-pairing reshape (2026-05-25 design): a
// pairing is authorized/revoked by the **user** from their own client via the
// bearer `fauna.pair.{add,revoke}` kinds (`crate::pair`), and the admin's
// only pairing control is the nest-level `pairing` service knob (above);
// design tracked internally.

// ── fauna.admin.factory_reset ────────────────────────────────────────────────

/// Return the nest to **fresh / unclaimed**. Destructive: wipes all deployment
/// state (actors, handles, admin grants, mail domains, account_aliases, bridge
/// enrollments + blobs, mail records, storage-mode + claim markers) and
/// regenerates the claim code, while **preserving** the nest host identity
/// keypair and the on-disk ACME cert so the box stays reachable for the
/// re-claim. Mechanism is restart-wipe: the handler stages a marker carrying
/// the next claim code, replies, then exits so the supervisor restarts the
/// process into a pre-claim wipe (see `behavior/mail-bridge-lifecycle.md` §
/// Factory reset and `bins/fauna-nest/src/factory_reset.rs`).
///
/// `new_claim_code` optionally pins the post-reset claim code (so the caller
/// can drive the re-claim with a code it chose); absent → a fresh random one is
/// generated and returned in the reply (26 chars / 130 bits from the
/// ambiguity-free 32-symbol alphabet — `fauna_core::claim_code`).
///
/// Clients **always** pin it: the code is minted and durably persisted before
/// dispatch, because a client killed before rendering the reply would otherwise
/// lose it and leave the box un-claimable (gap CR-1,
/// `docs/goal/architecture/nest/common.md` § Client-state recoverability). The
/// nest honors a pinned code verbatim (trimmed).
///
/// ⚠ v1 is `Admin`-gated only — the cooldown / client-compromise gating is a
/// deferred follow-up (acceptable on the disposable dogfood VPS). See the goal
/// doc § Factory reset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FactoryResetRequest {
    /// Optional explicit post-reset claim code. `None` → generate a random one.
    #[serde(default)]
    pub new_claim_code: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The claim code the freshly-reset nest will accept — the re-onboarding link
/// the very next claim step needs. Returned synchronously (the handler stages
/// it before exiting), so the caller never reads `/data` off the box.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FactoryResetReply {
    pub claim_code: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.admin.request_host_restart ─────────────────────────────────────────

/// `fauna.admin.request_host_restart` — the admin's "restart now" affordance for
/// the **host Ubuntu box** of an onboarded VPS (`installers/vps.md` § Host OS
/// Maintenance § 4). The nest reboots the host on idle (or a 24 h ceiling) for a
/// pending security update; this kind lets the admin trigger the (still-graceful)
/// reboot immediately. The handler writes a `restart-requested` flag into the
/// `/data/maintenance` bind mount; the host `fauna-reboot-coordinator` picks it
/// up on its next run, reboots regardless of idle/ceiling, and consumes the flag
/// so it fires exactly once (crash-safe + idempotent). Admin-class. No
/// parameters — it is a pure "do it now" request. **Rejected on a nest without
/// the maintenance mount** (dev / desktop / bare-metal): there is no host box to
/// reboot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RequestHostRestartRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Acknowledges that the restart request was recorded (the flag is written; the
/// coordinator reboots on its next ~15 min run). `ok: true` on success; failures
/// surface as the namespaced `RpcError` (e.g. no maintenance mount).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RequestHostRestartReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn sample_user(eviction: Option<AdminEviction>) -> AdminUser {
        AdminUser {
            actor_id: ByteBuf::from(vec![9u8; 32]),
            tier: "default".into(),
            label: "alice".into(),
            suspended: false,
            created_at: 1_700_000_000,
            inbox_bytes_used: 4096,
            storage_bytes_used: 1_048_576,
            eviction,
            mail_serving_enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn user_round_trips_with_and_without_eviction() {
        assert_round_trips(&sample_user(None));
        assert_round_trips(&sample_user(Some(AdminEviction {
            status: "warned".into(),
            reason: "over quota".into(),
            category: "storage".into(),
            warned_at: Some(1_700_000_100),
            suspend_at: Some(1_700_500_000),
            delete_at: None,
            extra: BTreeMap::new(),
        })));
    }

    #[test]
    fn list_request_and_reply_round_trip() {
        assert_round_trips(&AdminUsersListRequest {
            limit: None,
            offset: 0,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminUsersListRequest {
            limit: Some(100),
            offset: 50,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminUsersListReply {
            users: vec![sample_user(None), sample_user(None)],
            total: 2,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn get_request_and_reply_round_trip() {
        assert_round_trips(&AdminUserGetRequest {
            actor_id: ByteBuf::from(vec![1u8; 32]),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminUserGetReply {
            user: sample_user(None),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn create_and_update_requests_round_trip() {
        // Struct-update fixtures: this type grows over time, and hand-listing
        // every field makes two branches that each add one collide on the
        // grown axis. Prefer `..Default::default()` for any wire type that is
        // still gaining fields.
        assert_round_trips(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(vec![3u8; 32]),
            tier: "free".into(),
            label: "carol".into(),
            ..Default::default()
        });
        // label defaults to "" and handle to None when absent.
        assert_round_trips(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(vec![4u8; 32]),
            tier: "personal".into(),
            ..Default::default()
        });
        // Admission WITH a handle — the shape `public-mode.md` § Registration
        // & Identity describes for all three account-creation paths.
        assert_round_trips(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(vec![7u8; 32]),
            tier: "free".into(),
            handle: Some("carol".into()),
            ..Default::default()
        });
        // Supervised direct admission — guardian + band ride the same request
        // (`family-safety.md` § The guardianship link, the direct-admission
        // arm, 2026-08-30).
        assert_round_trips(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(vec![8u8; 32]),
            tier: "free".into(),
            handle: Some("kiddo".into()),
            guardian_actor: Some(ByteBuf::from(vec![9u8; 32])),
            age_band: Some("u13".into()),
            ..Default::default()
        });
        assert_round_trips(&AdminUserUpdateRequest {
            actor_id: ByteBuf::from(vec![5u8; 32]),
            tier: "personal".into(),
            label: "renamed".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn actor_ref_requests_round_trip() {
        assert_round_trips(&AdminUserDeleteRequest {
            actor_id: ByteBuf::from(vec![6u8; 32]),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminUserClearHandleRequest {
            actor_id: ByteBuf::from(vec![7u8; 32]),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminUserCancelEvictionRequest {
            actor_id: ByteBuf::from(vec![8u8; 32]),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminUserSuspendRequest {
            actor_id: ByteBuf::from(vec![9u8; 32]),
            reason: "abuse in progress".into(),
            category: "abuse".into(),
            extra: BTreeMap::new(),
        });
        // The optional halves default cleanly (the nest fills them in).
        assert_round_trips(&AdminUserSuspendRequest {
            actor_id: ByteBuf::from(vec![9u8; 32]),
            ..Default::default()
        });
    }

    #[test]
    fn evict_request_round_trips() {
        assert_round_trips(&AdminUserEvictRequest {
            actor_id: ByteBuf::from(vec![10u8; 32]),
            reason: "spam".into(),
            category: "abuse".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn shared_replies_round_trip() {
        assert_round_trips(&AdminOkReply {
            ok: true,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminPendingActionReply {
            pending_action_id: 42,
            execute_after: 1_700_604_800,
            status: "pending".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn evictions_list_round_trips() {
        assert_round_trips(&AdminEvictionsListRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminEvictionsListReply {
            evictions: vec![sample_user(Some(AdminEviction {
                status: "suspended".into(),
                reason: "tos".into(),
                category: "terms".into(),
                warned_at: Some(1_700_000_100),
                suspend_at: Some(1_700_500_000),
                delete_at: Some(1_701_000_000),
                extra: BTreeMap::new(),
            }))],
            extra: BTreeMap::new(),
        });
    }

    // ── C2: tiers / invite codes / invite requests / admins ──────────────────

    fn sample_tier() -> AdminTier {
        AdminTier {
            name: "personal".into(),
            max_inbox_bytes: 1_073_741_824,
            max_storage_bytes: 10_737_418_240,
            max_devices: 5,
            max_blob_size: 104_857_600,
            max_feeds: 20,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn tiers_round_trip() {
        assert_round_trips(&AdminTiersListRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminTiersListReply {
            tiers: vec![sample_tier()],
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminTierCreateRequest {
            name: "enterprise".into(),
            max_inbox_bytes: 1,
            max_storage_bytes: 2,
            max_devices: 3,
            max_blob_size: 4,
            max_feeds: 5,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminTierUpdateRequest {
            name: "personal".into(),
            max_inbox_bytes: 9,
            max_storage_bytes: 8,
            max_devices: 7,
            max_blob_size: 6,
            max_feeds: 5,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn invite_codes_round_trip() {
        assert_round_trips(&AdminInviteCodesListRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminInviteCodesListReply {
            invite_codes: vec![
                AdminInviteCode {
                    code: "WELCOME".into(),
                    tier: "free".into(),
                    uses_left: 10,
                    created_at: 1_700_000_000,
                    ..Default::default()
                },
                AdminInviteCode {
                    code: "SEEDED".into(),
                    tier: "personal".into(),
                    uses_left: 1,
                    created_at: 1_700_000_100,
                    // Supervised code (additive 2026-07-09).
                    guardian_actor: Some(ByteBuf::from(vec![4u8; 32])),
                    ..Default::default()
                },
            ],
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminInviteCodeCreateRequest {
            code: "NEWCODE".into(),
            tier: "personal".into(),
            uses: 5,
            ..Default::default()
        });
        // Supervised mint carries the guardian actor (additive 2026-07-09).
        assert_round_trips(&AdminInviteCodeCreateRequest {
            guardian_actor: Some(ByteBuf::from(vec![5u8; 32])),
            ..Default::default()
        });
        // The Default carries the twin's tier="free", uses=1.
        let d = AdminInviteCodeCreateRequest::default();
        assert_eq!(d.tier, "free");
        assert_eq!(d.uses, 1);
        assert_round_trips(&d);
        assert_round_trips(&AdminInviteCodeDeleteRequest {
            code: "OLDCODE".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn invite_code_create_request_applies_serde_defaults() {
        // A client omitting tier/uses gets the twin's defaults.
        let bytes = encode_canonical(&{
            let mut m = BTreeMap::new();
            m.insert("code".to_string(), Value::String("X".into()));
            m
        })
        .unwrap();
        let req: AdminInviteCodeCreateRequest = decode(&bytes).unwrap();
        assert_eq!(req.code, "X");
        assert_eq!(req.tier, "free");
        assert_eq!(req.uses, 1);
        assert!(req.extra.is_empty());
    }

    #[test]
    fn invite_requests_round_trip() {
        assert_round_trips(&AdminInviteRequestsListRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminInviteRequestsListReply {
            invite_requests: vec![
                AdminInviteRequest {
                    id: 1,
                    actor_id: ByteBuf::from(vec![1u8; 32]),
                    handle: "newbie".into(),
                    message: "please let me in".into(),
                    status: "pending".into(),
                    created_at: 1_700_000_000,
                    // Absence-as-signal: the applicant's app made a claim
                    // (declared-only here) — the admin sees it on the row.
                    age_band: Some("13-15".into()),
                    age_band_provenance: Some("none".into()),
                    ..Default::default()
                },
                AdminInviteRequest {
                    id: 2,
                    actor_id: ByteBuf::from(vec![2u8; 32]),
                    handle: "denied".into(),
                    message: String::new(),
                    status: "denied".into(),
                    created_at: 1_700_000_050,
                    decided_at: Some(1_700_000_200),
                    decided_by: Some(ByteBuf::from(vec![9u8; 32])),
                    denial_reason: Some("spam".into()),
                    ..Default::default()
                },
            ],
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminInviteRequestApproveRequest {
            id: 1,
            tier: Some("personal".into()),
            label: Some("Alice".into()),
            ..Default::default()
        });
        assert_round_trips(&AdminInviteRequestApproveRequest {
            id: 2,
            ..Default::default()
        });
        // Supervised approval carries the guardian actor (additive 2026-07-09).
        assert_round_trips(&AdminInviteRequestApproveRequest {
            id: 3,
            guardian_actor: Some(ByteBuf::from(vec![6u8; 32])),
            ..Default::default()
        });
        // …and, since 2026-08-24, the guardian's age-band dial beside it.
        assert_round_trips(&AdminInviteRequestApproveRequest {
            id: 4,
            guardian_actor: Some(ByteBuf::from(vec![6u8; 32])),
            age_band: Some("u13".into()),
            ..Default::default()
        });
        assert_round_trips(&AdminInviteRequestApproveReply {
            actor_id: ByteBuf::from(vec![1u8; 32]),
            handle: "newbie".into(),
            tier: "free".into(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminInviteRequestDenyRequest {
            id: 3,
            reason: Some("not eligible".into()),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminInviteRequestDenyRequest {
            id: 4,
            reason: None,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn admins_round_trip() {
        assert_round_trips(&AdminAdminsListRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminAdminsListReply {
            admins: vec![AdminAdminEntry {
                actor_id: ByteBuf::from(vec![7u8; 32]),
                added_at: 1_700_000_000,
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminAdminAddRequest {
            actor_id: ByteBuf::from(vec![5u8; 32]),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminAdminRemoveRequest {
            actor_id: ByteBuf::from(vec![6u8; 32]),
            extra: BTreeMap::new(),
        });
    }

    // ── C3: stats / audit / ops ──────────────────────────────────────────────

    #[test]
    fn stats_round_trip() {
        assert_round_trips(&AdminStatsRequest {
            extra: BTreeMap::new(),
        });
        // `users_by_tier` as `(String, i64)` pairs must survive canonical CBOR.
        assert_round_trips(&AdminStatsReply {
            total_users: 12,
            users_by_tier: vec![("free".into(), 9), ("personal".into(), 3)],
            suspended_users: 1,
            total_inbox_bytes: 4_096,
            total_storage_bytes: 1_048_576,
            ws_connections: 7,
            extra: BTreeMap::new(),
        });
        // The empty-tier case must round-trip too.
        assert_round_trips(&AdminStatsReply::default());
    }

    #[test]
    fn status_round_trip() {
        assert_round_trips(&AdminStatusRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminStatusReply {
            version: "1.2.3".into(),
            update_available: None,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminStatusReply {
            version: "1.2.3".into(),
            update_available: Some(AdminUpdateAvailable {
                version: "1.3.0".into(),
                url: "https://example/releases/1.3.0".into(),
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn audit_round_trip() {
        assert_round_trips(&AdminAuditListRequest {
            limit: Some(50),
            before_id: Some(1000),
            extra: BTreeMap::new(),
        });
        // Defaults (limit/before_id absent) round-trip.
        assert_round_trips(&AdminAuditListRequest::default());
        assert_round_trips(&AdminAuditListReply {
            entries: vec![
                AdminAuditEntry {
                    id: 2,
                    ts: 1_700_000_100,
                    actor_id: Some(ByteBuf::from(vec![7u8; 32])),
                    action: "user.create".into(),
                    target: Some("abcd".into()),
                    detail: Some("tier=free".into()),
                    prev_hash: "00".into(),
                    entry_hash: "ff".into(),
                    entry_hash_version: 2,
                    extra: BTreeMap::new(),
                },
                // A system entry: no actor, no target/detail.
                AdminAuditEntry {
                    id: 1,
                    ts: 1_700_000_000,
                    actor_id: None,
                    action: "nest.boot".into(),
                    target: None,
                    detail: None,
                    prev_hash: String::new(),
                    entry_hash: "aa".into(),
                    entry_hash_version: 2,
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        });
    }

    /// The preimage version always rides the wire, and an entry without it is
    /// refused rather than defaulted to a number a verifier would then have to
    /// guess the meaning of — no nest omits it.
    #[test]
    fn an_audit_entry_without_its_preimage_version_is_refused() {
        let entry = AdminAuditEntry {
            id: 1,
            ts: 1_700_000_000,
            action: "nest.boot".into(),
            prev_hash: String::new(),
            entry_hash: "aa".into(),
            entry_hash_version: 2,
            ..Default::default()
        };
        let bytes = encode_canonical(&entry).unwrap();
        let mut map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(map.contains_key("entry_hash_version"));
        assert_eq!(entry, decode::<AdminAuditEntry>(&bytes).unwrap());
        map.remove("entry_hash_version");
        let without = encode_canonical(&map).unwrap();
        assert!(decode::<AdminAuditEntry>(&without).is_err());
    }

    #[test]
    fn audit_integrity_round_trip() {
        assert_round_trips(&AdminAuditIntegrityRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminAuditIntegrityReply {
            head_id: 42,
            head_hash: "deadbeef".into(),
            chain_length: 42,
            first_entry_at: 1_700_000_000,
            last_entry_at: 1_700_900_000,
            // Three rows recording a version this nest does not implement.
            entries_v2: 9,
            entries_unverifiable: 3,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn cluster_status_round_trip() {
        assert_round_trips(&AdminClusterStatusRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminClusterStatusReply {
            total_blobs: 100,
            total_bytes: 5_000_000,
            local_blobs: 80,
            s3_blobs: 20,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn gc_round_trip() {
        assert_round_trips(&AdminGcRequest {
            grace_period_secs: Some(3600),
            dry_run: true,
            extra: BTreeMap::new(),
        });
        // Defaults (grace absent → handler's 1800; dry_run false) round-trip.
        assert_round_trips(&AdminGcRequest::default());
        assert_round_trips(&AdminGcReply {
            dry_run: false,
            deleted_blobs: 3,
            deleted_bytes: 9_000,
            live_snapshots: 12,
            referenced_manifests: 40,
            skipped_grace_period: 1,
            record_blob_refs: 3,
            conv_attachment_refs: 1,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn worker_status_round_trip() {
        assert_round_trips(&AdminWorkerStatusRequest {
            extra: BTreeMap::new(),
        });
        // No worker connected.
        assert_round_trips(&AdminWorkerStatusReply {
            authorized_key: None,
            connected: false,
            replication_count: 0,
            worker: None,
            extra: BTreeMap::new(),
        });
        // Connected worker with storage figures.
        assert_round_trips(&AdminWorkerStatusReply {
            authorized_key: Some("abcd1234".into()),
            connected: true,
            replication_count: 2,
            worker: Some(AdminWorkerInfo {
                max_storage_bytes: 10_737_418_240,
                current_usage_bytes: 1_048_576,
                payload_count: 17,
                connected_at: 1_700_000_000,
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn pending_actions_list_round_trip() {
        assert_round_trips(&AdminPendingActionsListRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminPendingActionsListReply {
            actions: vec![
                AdminPendingActionSummary {
                    id: 5,
                    actor_id: ByteBuf::from(vec![3u8; 32]),
                    action_type: "AdminDeleteUser".into(),
                    target: Some("victim".into()),
                    status: "pending".into(),
                    created_at: 1_700_000_000,
                    execute_after: 1_700_604_800,
                    requires_quorum: 2,
                    approvals: vec!["aa".into(), "bb".into()],
                    ip_address: Some("203.0.113.7".into()),
                    extra: BTreeMap::new(),
                },
                // No target / ip / approvals.
                AdminPendingActionSummary {
                    id: 6,
                    actor_id: ByteBuf::from(vec![4u8; 32]),
                    action_type: "AccountDelete".into(),
                    target: None,
                    status: "pending".into(),
                    created_at: 1_700_000_100,
                    execute_after: 1_701_209_600,
                    requires_quorum: 0,
                    approvals: vec![],
                    ip_address: None,
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        });
    }

    // ── C5: folders / services ─────────────────────────────────────────

    #[test]
    fn folder_create_round_trip() {
        assert_round_trips(&AdminFolderCreateRequest {
            name: "photos".into(),
            actor_id: ByteBuf::from(vec![1u8; 32]),
            node_cache: None,
            ..Default::default()
        });
        assert_round_trips(&AdminFolderCreateRequest {
            name: "docs".into(),
            actor_id: ByteBuf::from(vec![2u8; 32]),
            node_cache: Some(true),
            ..Default::default()
        });
        assert_round_trips(&AdminFolderCreateReply {
            id: 7,
            name: "photos".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn folder_get_round_trip() {
        assert_round_trips(&AdminFolderGetRequest {
            name: "photos".into(),
            actor_id: Some(ByteBuf::from(vec![1u8; 32])),
            // The post-flip addressing arm: an admin holds the hash, not a name.
            name_hash: Some(ByteBuf::from(vec![7u8; 32])),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminFolderGetRequest {
            name: "photos".into(),
            actor_id: None,
            name_hash: None,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminFolderGetReply {
            id: 7,
            name: "photos".into(),
            name_hash: Some(ByteBuf::from(vec![9u8; 32])),
            actor_id: ByteBuf::from(vec![1u8; 32]),
            node_cache: true,
            members: vec![AdminFolderMember {
                device_id: ByteBuf::from(vec![4u8; 32]),
                flags: crate::folders::PlaceFlags::new(true, false, false),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        });
        // Empty folder (no members).
        assert_round_trips(&AdminFolderGetReply {
            id: 8,
            name: "empty".into(),
            actor_id: ByteBuf::from(vec![1u8; 32]),
            node_cache: false,
            members: vec![],
            ..Default::default()
        });
    }

    #[test]
    fn folder_member_requests_round_trip() {
        assert_round_trips(&AdminFolderAddMemberRequest {
            name: "photos".into(),
            actor_id: Some(ByteBuf::from(vec![1u8; 32])),
            name_hash: Some(ByteBuf::from(vec![7u8; 32])),
            device_id: ByteBuf::from(vec![4u8; 32]),
            flags: crate::folders::PlaceFlags::default_place(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn services_round_trip() {
        assert_round_trips(&AdminServicesListRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminServicesListReply {
            version: 1,
            services: AdminServiceFlags {
                bridge: true,
                pairing: true,
                extra: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminServiceUpdateRequest {
            name: "bridge".into(),
            enabled: true,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminServiceUpdateReply {
            ok: true,
            service: "bridge".into(),
            enabled: true,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn logs_round_trip() {
        assert_round_trips(&AdminLogsRequest {
            extra: BTreeMap::new(),
        });
        // An empty ring + a populated one spanning every level, so the enum
        // discriminants and the i64 timestamp all round-trip canonically.
        assert_round_trips(&AdminLogsReply {
            entries: Vec::new(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AdminLogsReply {
            entries: vec![
                AdminLogEntry {
                    timestamp_ms: 1_717_500_000_000,
                    level: AdminLogLevel::Error,
                    target: "fauna_nest::serve".into(),
                    message: "bind failed".into(),
                    extra: BTreeMap::new(),
                },
                AdminLogEntry {
                    timestamp_ms: 1_717_500_000_500,
                    level: AdminLogLevel::Info,
                    target: "fauna_nest".into(),
                    message: "listening".into(),
                    extra: BTreeMap::new(),
                },
                AdminLogEntry {
                    timestamp_ms: 1_717_500_001_000,
                    level: AdminLogLevel::Trace,
                    target: "fauna_nest::ws".into(),
                    message: "frame".into(),
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        });
    }

    // C4 pairings retired — see `crate::pair` for the user-side kinds.

    #[test]
    fn factory_reset_round_trip() {
        assert_round_trips(&FactoryResetRequest {
            new_claim_code: None,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&FactoryResetRequest {
            new_claim_code: Some("ABCDEF".into()),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&FactoryResetReply {
            claim_code: "ABCDEF".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn request_host_restart_round_trip() {
        assert_round_trips(&RequestHostRestartRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&RequestHostRestartReply {
            ok: true,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn invite_request_is_pending() {
        let pending = AdminInviteRequest {
            status: "pending".into(),
            ..Default::default()
        };
        assert!(pending.is_pending());
        for decided in ["approved", "denied", ""] {
            let req = AdminInviteRequest {
                status: decided.into(),
                ..Default::default()
            };
            assert!(!req.is_pending(), "status {decided:?} must not be pending");
        }
    }
}
