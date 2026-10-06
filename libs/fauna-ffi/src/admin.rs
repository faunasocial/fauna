//! UniFFI façade for the Layer-5 **Admin** WS-RPC kinds (`fauna.admin.*`) —
//! the admin surface the four native apps (windows / macOS / iOS /
//! Android) drive from the consolidated `admin-users` hub (Pending requests /
//! Invite / Users).
//!
//! [`FfiAdminClient`] wraps `fauna_client_admin::AdminClient`; the mirror
//! records below are the FFI-visible shape of `fauna_protocol::admin::*`. The
//! Rust-native Linux app (`apps/fauna-linux/src/views/admin.rs`) calls the
//! same `AdminClient` directly (`AdminClient<Arc<NestClient>>`, no FFI); the
//! web SPA reaches it through the wasm twin (`libs/fauna-wasm/src/rpc.rs`).
//! One shared client, exposed once at each boundary (priority #2) — not five
//! hand-rolled per-app wrappers.
//!
//! Protocol → FFI conversions are exhaustive `From` impls, so a *new* field on
//! an `admin` wire type that the UI should see is a compile error here (mirror-
//! drift guard, priority #1/#4). The wire types' `extra: BTreeMap<String,
//! Value>` forward-compat catch-all is intentionally dropped — it is a
//! decode escape hatch, never a value the page renders.
//!
//! **Surface covered today**: the consolidated-page minimum
//! (`users_list` / `users_create` / `users_update`, `tiers_list`,
//! `invite_codes_{list,create,delete}`,
//! `invite_requests_{list,approve,deny}`) plus the eviction + suspension +
//! tier-definition controls (`users_evict` / `users_suspend` /
//! `users_cancel_eviction` / `evictions_list`, `tiers_create` /
//! `tiers_update`; pagination already rides `users_list`'s
//! `limit`/`offset`/`total`; [`admin_user_row_controls`] is the shared decision
//! for which of the three a row offers), plus
//! `factory_reset` (the admin-settings Danger-zone affordance —
//! `mail-bridge-lifecycle.md` § Factory reset). The
//! `users_{delete,clear_handle}` methods on `AdminClient`
//! back still-deferred page features — add them here when those surfaces migrate
//! (mirror linux's Stage-2 deferral).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_admin::AdminClient;
use fauna_client_admin::admin::{
    AdminEviction, AdminInviteCode, AdminInviteRequest, AdminInviteRequestApproveReply,
    AdminMembershipTier, AdminMembershipTierSetRequest, AdminServiceFlags, AdminStatsReply,
    AdminStatusReply, AdminTier, AdminTierCreateRequest, AdminTierUpdateRequest,
    AdminUpdateAvailable, AdminUser, AdminUsersListReply,
};

use fauna_client_admin::RegistrationMode;
#[cfg(feature = "value-format")]
use fauna_core::localized::LocalizedText;
use fauna_protocol::age::AgeBand;

use crate::{FfiError, stringify};

// ── Record mirrors ─────────────────────────────────────────────────────

/// The deployment's registration posture — the three modes of `admin-users`'
/// `admin-users-registration-mode-select`. Mirrors
/// [`fauna_protocol::node_policy::RegistrationMode`] across the FFI boundary.
///
/// An enum, not a string, so no client spells the wire vocabulary itself and the
/// select's options are exhaustive at compile time. Owner of the semantics:
/// `public-mode.md` § Registration Modes.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum FfiRegistrationMode {
    /// Anyone may register.
    Open,
    /// Registration requires a valid invite code.
    InviteRequired,
    /// Registration is closed; no new accounts. The admin can still admit a user
    /// directly or approve an invite request — those are admin actions, not
    /// self-service registration. The default posture of a fresh nest.
    Closed,
}

impl From<FfiRegistrationMode> for RegistrationMode {
    fn from(m: FfiRegistrationMode) -> Self {
        match m {
            FfiRegistrationMode::Open => RegistrationMode::Open,
            FfiRegistrationMode::InviteRequired => RegistrationMode::InviteRequired,
            FfiRegistrationMode::Closed => RegistrationMode::Closed,
        }
    }
}

impl From<RegistrationMode> for FfiRegistrationMode {
    fn from(m: RegistrationMode) -> Self {
        match m {
            RegistrationMode::Open => FfiRegistrationMode::Open,
            RegistrationMode::InviteRequired => FfiRegistrationMode::InviteRequired,
            RegistrationMode::Closed => FfiRegistrationMode::Closed,
        }
    }
}

/// Parse the posture reported by `fauna.setup.status`
/// ([`crate::nest_client::FfiSetupStatus::registration_mode`]) into the enum the
/// mode select renders.
///
/// **`None` is not "closed" — it means this client cannot name the posture**, and
/// the two ways that happens are both real under the bidirectional-compat
/// invariant (`version-compatibility.md`): the raw value is `None` or a mode string a
/// *newer* nest added that this client predates. A caller must **not** coerce either to a variant and offer a Save:
/// that would write this client's guess over the nest's real posture. Render the
/// section read-only instead, and leave the posture alone.
///
/// The nest itself always reports a concrete mode (`discovery_core` resolves the
/// live `AppState` value), so `None` never means "the admin has not chosen yet".
#[uniffi::export]
pub fn registration_mode_from_wire(mode: &str) -> Option<FfiRegistrationMode> {
    RegistrationMode::from_wire_str(mode).map(Into::into)
}

/// FFI mirror of [`fauna_protocol::admin::AdminEviction`] — the in-flight
/// eviction timeline on a user row (`Some` only while evicting).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminEviction {
    pub status: String,
    pub reason: String,
    pub category: String,
    pub warned_at: Option<i64>,
    pub suspend_at: Option<i64>,
    pub delete_at: Option<i64>,
}

impl From<AdminEviction> for FfiAdminEviction {
    fn from(e: AdminEviction) -> Self {
        FfiAdminEviction {
            status: e.status,
            reason: e.reason,
            category: e.category,
            warned_at: e.warned_at,
            suspend_at: e.suspend_at,
            delete_at: e.delete_at,
        }
    }
}

/// FFI mirror of [`fauna_protocol::admin::AdminUser`] — one row of the Users
/// section. `actor_id` is the raw 32-byte actor id.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminUser {
    pub actor_id: Vec<u8>,
    pub tier: String,
    pub label: String,
    /// The account's handle — unique on the nest, and what identifies a user
    /// in an admin picker (`admin.md` § 2; the editable, non-unique `label`
    /// never is). `None` for a handle-less account — pickers fall back to the full actor hex. Additive 2026-08-30.
    pub handle: Option<String>,
    pub suspended: bool,
    pub created_at: i64,
    pub inbox_bytes_used: i64,
    pub storage_bytes_used: i64,
    pub eviction: Option<FfiAdminEviction>,
    /// Read-only IMAP/CalDAV-serving audit indicator (default on). The user sets
    /// it from their own mail-settings; the admin only sees it. Resolved by the
    /// nest projection (`actor_mail_serving`, absent ⇒ on).
    pub mail_serving_enabled: bool,
    /// Whether this actor holds the admin role. Read-only — the role moves via
    /// `fauna.admin.admins.{add,remove}`, never from a Users row. Clients use it
    /// to withhold the lifecycle controls the nest refuses on an admin
    /// (`fauna.admin.conflict`); the shared decision is
    /// `fauna_client_admin::admin_user_row_controls`, so no client re-derives it.
    pub is_admin: bool,
}

impl From<AdminUser> for FfiAdminUser {
    fn from(u: AdminUser) -> Self {
        FfiAdminUser {
            actor_id: u.actor_id.to_vec(),
            tier: u.tier,
            label: u.label,
            handle: u.handle,
            suspended: u.suspended,
            created_at: u.created_at,
            inbox_bytes_used: u.inbox_bytes_used,
            storage_bytes_used: u.storage_bytes_used,
            eviction: u.eviction.map(Into::into),
            mail_serving_enabled: u.mail_serving_enabled,
            is_admin: u.is_admin,
        }
    }
}

/// Which of the three per-row lifecycle controls an `admin-users` row offers
/// (`admin.md` § 2 Users → *Cutting a user off — eviction and suspension*).
/// FFI mirror of [`fauna_client_admin::AdminUserRowControls`] — the shared
/// decision every app renders from, so no client re-derives the eviction-
/// state × admin-role-guard cross (priority #1/#2/#4). Linux (Rust-native) and
/// the web wasm face call `fauna_client_admin::admin_user_row_controls`
/// directly; native apps go through [`admin_user_row_controls`] below.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfiAdminUserRowControls {
    /// `admin-users-suspend-button` — cut the user off *now*, no delete timeline.
    pub suspend: bool,
    /// `admin-users-evict-button` — start the timed warn → suspend → delete ladder.
    pub evict: bool,
    /// `admin-users-cancel-eviction-button` — restore, from *either* entry point.
    pub restore: bool,
    /// `admin-users-make-admin-button` — grant the admin role.
    pub make_admin: bool,
    /// `admin-users-remove-admin-button` — revoke the admin role.
    pub remove_admin: bool,
}

impl From<fauna_client_admin::AdminUserRowControls> for FfiAdminUserRowControls {
    fn from(c: fauna_client_admin::AdminUserRowControls) -> Self {
        FfiAdminUserRowControls {
            suspend: c.suspend,
            evict: c.evict,
            restore: c.restore,
            make_admin: c.make_admin,
            remove_admin: c.remove_admin,
        }
    }
}

/// `fauna_client_admin::admin_user_row_controls` over the FFI boundary — the
/// single source of truth for which of the three Users-row lifecycle controls
/// to render. Do NOT re-derive this from `FfiAdminUser.eviction`/`is_admin`
/// client-side; call this instead (see the doc comment on
/// [`FfiAdminUserRowControls`]).
#[uniffi::export]
pub fn admin_user_row_controls(user: FfiAdminUser) -> FfiAdminUserRowControls {
    let wire = AdminUser {
        eviction: user.eviction.map(|e| AdminEviction {
            status: e.status,
            reason: e.reason,
            category: e.category,
            warned_at: e.warned_at,
            suspend_at: e.suspend_at,
            delete_at: e.delete_at,
            ..Default::default()
        }),
        is_admin: user.is_admin,
        ..Default::default()
    };
    fauna_client_admin::admin_user_row_controls(&wire).into()
}

/// `fauna_client_admin::admin_picker_option` over the FFI boundary — the
/// option text an admin picker (guardian, invite-request) offers for `user`:
/// the **handle**, falling back to the full actor hex for a handle-less
/// account (`admin.md` § 2 → *What identifies a user in an admin picker*).
/// Linux (Rust-native) and the web wasm face call
/// `fauna_client_admin::admin_picker_option` directly; native apps go through
/// this function.
#[uniffi::export]
pub fn admin_picker_option(user: FfiAdminUser) -> String {
    let wire = AdminUser {
        actor_id: fauna_protocol::ByteBuf::from(user.actor_id),
        handle: user.handle,
        ..Default::default()
    };
    fauna_client_admin::admin_picker_option(&wire)
}

/// FFI mirror of [`fauna_protocol::admin::AdminUsersListReply`] — a page of
/// users plus the unpaginated `total` (for the Users section's pager).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminUsersListReply {
    pub users: Vec<FfiAdminUser>,
    pub total: i64,
}

impl From<AdminUsersListReply> for FfiAdminUsersListReply {
    fn from(r: AdminUsersListReply) -> Self {
        FfiAdminUsersListReply {
            users: r.users.into_iter().map(Into::into).collect(),
            total: r.total,
        }
    }
}

/// FFI mirror of [`fauna_protocol::admin::AdminTier`] — a quota tier (the tier
/// pickers' options on the Users / Invite sections).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminTier {
    pub name: String,
    pub max_inbox_bytes: i64,
    pub max_storage_bytes: i64,
    pub max_devices: i64,
    pub max_blob_size: i64,
    pub max_feeds: i64,
}

impl From<AdminTier> for FfiAdminTier {
    fn from(t: AdminTier) -> Self {
        FfiAdminTier {
            name: t.name,
            max_inbox_bytes: t.max_inbox_bytes,
            max_storage_bytes: t.max_storage_bytes,
            max_devices: t.max_devices,
            max_blob_size: t.max_blob_size,
            max_feeds: t.max_feeds,
        }
    }
}

/// FFI mirror of [`fauna_protocol::admin::AdminMembershipTier`] — one
/// membership designation (monetization.md § Pillar 4): which of the admin's own
/// subscription tiers means paid access to this nest, and the quota tiers an
/// admitted / lapsed member runs under. `tier_name` addresses
/// `subscription_tiers`; `admin_tier` / `lapse_tier` address the quota tiers
/// [`FfiAdminTier`] lists — the two systems are joined here, never merged.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminMembershipTier {
    pub tier_name: String,
    pub admin_tier: String,
    pub lapse_tier: String,
    pub created_at: i64,
}

impl From<AdminMembershipTier> for FfiAdminMembershipTier {
    fn from(m: AdminMembershipTier) -> Self {
        FfiAdminMembershipTier {
            tier_name: m.tier_name,
            admin_tier: m.admin_tier,
            lapse_tier: m.lapse_tier,
            created_at: m.created_at,
        }
    }
}

/// One `(tier, user-count)` row of the admin dashboard's per-tier breakdown.
/// UniFFI has no tuple type, so the wire `Vec<(String, i64)>` becomes a
/// `Vec<FfiAdminTierCount>` (order-preserving, unlike a map).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminTierCount {
    pub tier: String,
    pub count: i64,
}

/// FFI mirror of [`fauna_protocol::admin::AdminStatsReply`] — the nest-wide
/// counters the admin dashboard renders (`fauna.admin.stats`). The wire
/// `extra` forward-compat catch-all is dropped (mirror-drift guard).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminStats {
    pub total_users: i64,
    pub users_by_tier: Vec<FfiAdminTierCount>,
    pub suspended_users: i64,
    pub total_inbox_bytes: i64,
    pub total_storage_bytes: i64,
    pub ws_connections: i64,
}

impl From<AdminStatsReply> for FfiAdminStats {
    fn from(s: AdminStatsReply) -> Self {
        FfiAdminStats {
            total_users: s.total_users,
            users_by_tier: s
                .users_by_tier
                .into_iter()
                .map(|(tier, count)| FfiAdminTierCount { tier, count })
                .collect(),
            suspended_users: s.suspended_users,
            total_inbox_bytes: s.total_inbox_bytes,
            total_storage_bytes: s.total_storage_bytes,
            ws_connections: s.ws_connections,
        }
    }
}

/// FFI mirror of [`fauna_protocol::admin::AdminStatusReply`] — the running nest
/// version + any pending self-update advisory (`fauna.admin.status`). The admin
/// dashboard's Version card reads `version` here, replacing the deleted
/// `GET /api/v1/node-info` HTTP twin; `update_available` is `None` when the nest
/// is up to date. The wire `extra` forward-compat catch-all is dropped.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminStatus {
    pub version: String,
    pub update_available: Option<FfiAdminUpdateAvailable>,
}

/// FFI mirror of [`fauna_protocol::admin::AdminUpdateAvailable`] — a pending
/// self-update advisory: the newer version + its release/download URL.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminUpdateAvailable {
    pub version: String,
    pub url: String,
}

impl From<AdminUpdateAvailable> for FfiAdminUpdateAvailable {
    fn from(u: AdminUpdateAvailable) -> Self {
        FfiAdminUpdateAvailable {
            version: u.version,
            url: u.url,
        }
    }
}

impl From<AdminStatusReply> for FfiAdminStatus {
    fn from(s: AdminStatusReply) -> Self {
        FfiAdminStatus {
            version: s.version,
            update_available: s.update_available.map(Into::into),
        }
    }
}

/// FFI mirror of [`fauna_protocol::admin::AdminInviteCode`] — one closed-
/// registration invite code on the Invite section.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminInviteCode {
    pub code: String,
    pub tier: String,
    pub uses_left: i64,
    pub created_at: i64,
    /// The band the code admits under (`family-safety.md` § The account age
    /// band — wire token, `None` = no band chosen / an ordinary code); the
    /// `invite-code-item` row echoes it through `age_band_label`.
    pub age_band: Option<String>,
}

impl From<AdminInviteCode> for FfiAdminInviteCode {
    fn from(c: AdminInviteCode) -> Self {
        FfiAdminInviteCode {
            code: c.code,
            tier: c.tier,
            uses_left: c.uses_left,
            created_at: c.created_at,
            age_band: c.age_band,
        }
    }
}

/// FFI mirror of [`fauna_protocol::admin::AdminInviteRequest`] — one in-band
/// request awaiting (or post) an admin decision on the Pending requests
/// section. `actor_id` / `decided_by` are raw 32-byte ids; `decided_*` /
/// `denial_reason` are `Some` only once decided.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminInviteRequest {
    pub id: i64,
    pub actor_id: Vec<u8>,
    pub handle: String,
    pub message: String,
    pub status: String,
    /// Derived from `status` (`== "pending"`) in shared Rust so clients gate
    /// the approve/deny/tier controls without re-deriving the wire string.
    pub is_pending: bool,
    pub created_at: i64,
    pub decided_at: Option<i64>,
    pub decided_by: Option<Vec<u8>>,
    pub denial_reason: Option<String>,
    /// The applicant's age-claim band (wire token) when the submit carried one
    /// — absence-as-signal on `invite-request-row-age-claim`
    /// (`family-safety.md` § The account age band, D6); render through
    /// `age_claim_label`, and seed `invite-request-row-age-band-select` from it.
    pub age_band: Option<String>,
    /// How that claim was established (`attested-ios` / `attested-android` /
    /// `none` = declared-only), when present.
    pub age_band_provenance: Option<String>,
}

impl From<AdminInviteRequest> for FfiAdminInviteRequest {
    fn from(r: AdminInviteRequest) -> Self {
        let is_pending = r.is_pending();
        FfiAdminInviteRequest {
            id: r.id,
            actor_id: r.actor_id.to_vec(),
            handle: r.handle,
            message: r.message,
            status: r.status,
            is_pending,
            created_at: r.created_at,
            decided_at: r.decided_at,
            decided_by: r.decided_by.map(|b| b.to_vec()),
            denial_reason: r.denial_reason,
            age_band: r.age_band,
            age_band_provenance: r.age_band_provenance,
        }
    }
}

/// FFI mirror of [`fauna_protocol::admin::AdminInviteRequestApproveReply`] —
/// the resolved `{ actor_id, handle, tier }` after admitting a requester.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminInviteRequestApproveReply {
    pub actor_id: Vec<u8>,
    pub handle: String,
    pub tier: String,
}

impl From<AdminInviteRequestApproveReply> for FfiAdminInviteRequestApproveReply {
    fn from(r: AdminInviteRequestApproveReply) -> Self {
        FfiAdminInviteRequestApproveReply {
            actor_id: r.actor_id.to_vec(),
            handle: r.handle,
            tier: r.tier,
        }
    }
}

/// FFI mirror of [`fauna_protocol::admin::AdminServiceFlags`] — the sidecar-
/// service enable flags backing the `admin-services` toggles. `bridge` /
/// `pairing` are the two nest service flags
/// (`fauna.admin.services.{list,update}`); the deployment "Fauna controls DNS"
/// master switch (`admin-service-dns-toggle`) is **not** a flag here — it rides
/// the shared `DnsManagementMachine` all-managed projection (admin.md § 5). The
/// `version` + `extra` catch-all the wire reply carries are dropped — the UI
/// renders only the two flags.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminServiceFlags {
    pub bridge: bool,
    pub pairing: bool,
}

impl From<AdminServiceFlags> for FfiAdminServiceFlags {
    fn from(f: AdminServiceFlags) -> Self {
        FfiAdminServiceFlags {
            bridge: f.bridge,
            pairing: f.pairing,
        }
    }
}

/// FFI mirror of [`fauna_client_capabilities::view_model::ReceiptState`] — the
/// three receipt-freshness states a custody-hosting row renders. Never
/// collapsed to a string here: an enum keeps the three words exhaustive at
/// compile time, the same reasoning [`FfiRegistrationMode`] documents.
#[cfg(feature = "custody")]
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum FfiReceiptState {
    Fresh,
    Stale,
    NoReceiptYet,
}

#[cfg(feature = "custody")]
impl From<fauna_client_capabilities::view_model::ReceiptState> for FfiReceiptState {
    fn from(s: fauna_client_capabilities::view_model::ReceiptState) -> Self {
        use fauna_client_capabilities::view_model::ReceiptState as Rs;
        match s {
            Rs::Fresh => Self::Fresh,
            Rs::Stale => Self::Stale,
            Rs::NoReceiptYet => Self::NoReceiptYet,
        }
    }
}

/// FFI mirror of [`fauna_client_capabilities::view_model::AdminHostingRowView`]
/// — one row of the nest-wide `admin-custody-hosting` registry.
/// `(host_actor_id, grant_id)` is the remove door's key, carried here so a leg
/// removes what the list rendered rather than a painted index (a re-read can
/// reorder rows — see
/// [`fauna_client_capabilities::view_model::admin_hosting_rows`]'s ordering
/// note).
#[cfg(feature = "custody")]
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminHostingRow {
    /// Hex actor id of the DEPOSITING host.
    pub host_actor_id: String,
    /// Hex actor id of the custodied owner.
    pub owner_actor_id: String,
    /// The pull leg's dial anchor, rendered verbatim.
    pub owner_nest_url: String,
    pub grant_id: Vec<u8>,
    /// `0` = the row carries no cap, and the pump substitutes the hard-coded
    /// default — the leg must render *Default*, never `0 B`. `i64`, not
    /// `u64` — the FFI byte-count convention this file's own `FfiAdminUser`/
    /// `FfiAdminTier`/`FfiAdminStats` already follow, to avoid `ULong`/`UInt64`
    /// friction on the Kotlin/Swift side. A byte count never approaches
    /// `i64::MAX` (8 exbibytes), so the source `u64` fits losslessly.
    pub retained_bytes_cap: i64,
    pub held_bytes: i64,
    /// A stopped row still holds its bytes — remove exists precisely because
    /// stop alone does not free them.
    pub stopped: bool,
    pub receipt_state: FfiReceiptState,
}

#[cfg(feature = "custody")]
impl From<fauna_client_capabilities::view_model::AdminHostingRowView> for FfiAdminHostingRow {
    fn from(r: fauna_client_capabilities::view_model::AdminHostingRowView) -> Self {
        FfiAdminHostingRow {
            host_actor_id: r.host_actor_id,
            owner_actor_id: r.owner_actor_id,
            owner_nest_url: r.owner_nest_url,
            grant_id: r.grant_id,
            retained_bytes_cap: r.retained_bytes_cap as i64,
            held_bytes: r.held_bytes as i64,
            stopped: r.stopped,
            receipt_state: r.receipt_state.into(),
        }
    }
}

/// FFI mirror of [`fauna_protocol::custody::AdminHostingRemoveReply`].
#[cfg(feature = "custody")]
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfiAdminHostingRemoveReply {
    /// The row existed and was dropped.
    pub removed: bool,
    /// The `(host, owner)` custodied store was dropped too — only when the
    /// removed row was the pair's last.
    pub store_dropped: bool,
}

#[cfg(feature = "custody")]
impl From<fauna_protocol::custody::AdminHostingRemoveReply> for FfiAdminHostingRemoveReply {
    fn from(r: fauna_protocol::custody::AdminHostingRemoveReply) -> Self {
        FfiAdminHostingRemoveReply {
            removed: r.removed,
            store_dropped: r.store_dropped,
        }
    }
}

// ── FfiAdminClient ─────────────────────────────────────────────────────

/// UniFFI handle for the `fauna.admin.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::admin`]; methods are exposed to Swift
/// as `async throws` and Kotlin as `suspend fun`. All kinds are Admin-gated
/// nest-side — a non-admin caller gets a permission-denied error.
#[derive(uniffi::Object)]
pub struct FfiAdminClient {
    nest: Arc<NestClient>,
}

impl FfiAdminClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    pub(crate) fn client(&self) -> AdminClient<Arc<NestClient>> {
        AdminClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiAdminClient {
    /// `fauna.admin.stats` — nest-wide counters for the admin dashboard
    /// (total/suspended users, per-tier breakdown, inbox/storage bytes, live
    /// WS connections). Replaces the deleted `GET /admin/api/stats` twin.
    pub async fn stats(&self) -> Result<FfiAdminStats, FfiError> {
        let reply = self.client().stats().await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.admin.status` — the running nest version + any pending self-update
    /// advisory. The admin dashboard's Version card reads `version` here,
    /// replacing the deleted `GET /api/v1/node-info` HTTP twin.
    pub async fn status(&self) -> Result<FfiAdminStatus, FfiError> {
        let reply = self.client().status().await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.admin.set_serving_port` — set the deployment-wide client-facing API
    /// serving port (the nest's own HTTPS listener + the served SPA; `nest/
    /// common.md` § Serving ports). Admin-class; applies on the next nest restart
    /// (the nest cannot hot-rebind its own listener), but the chosen value is
    /// persisted and read back on `fauna.setup.status`
    /// ([`crate::nest_client::FfiSetupStatus::serving_port`]) immediately. The
    /// shared admin-shell surface (`admin-nest-serving-port-*`) writes through
    /// here; the symmetric twin of the CalDAV-port field's bridges call.
    pub async fn set_serving_port(&self, port: u16) -> Result<(), FfiError> {
        self.client()
            .set_serving_port(port)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.set_registration_mode` — set the deployment's registration
    /// posture and, orthogonally, the free-tier ceiling. The `admin-users`
    /// registration section's single Save writes through here
    /// (`admin.md` § 2 Users → *Section 2 — Registration*): one call carries both
    /// values, and the nest swaps the live posture with **no restart**. Read back
    /// on `fauna.setup.status`
    /// ([`crate::nest_client::FfiSetupStatus::registration_mode`] +
    /// [`crate::nest_client::FfiSetupStatus::max_free_users`]).
    ///
    /// `max_free_users` `None` **clears** the cap (blank input = no cap) rather
    /// than leaving it unchanged — mode + ceiling are one decision, saved
    /// together. The cap counts every free-tier account *including the admin's
    /// own*, so "room for one more" is a cap of 2 (see
    /// [`fauna_client_admin::AdminClient::set_registration_mode`]).
    pub async fn set_registration_mode(
        &self,
        mode: FfiRegistrationMode,
        max_free_users: Option<u64>,
    ) -> Result<(), FfiError> {
        self.client()
            .set_registration_mode(mode.into(), max_free_users)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.set_age_verification_required` — the Registration
    /// section's "accept only signups carrying app age verification" knob
    /// (`admin-users-registration-age-verification-toggle`; default off; read
    /// back on `fauna.setup.status`
    /// [`crate::nest_client::FfiSetupStatus::age_verification_required`]).
    /// The section's save dispatches it beside [`Self::set_registration_mode`]
    /// only when the toggle's value changed (see
    /// [`fauna_client_admin::AdminClient::set_age_verification_required`]).
    pub async fn set_age_verification_required(&self, required: bool) -> Result<(), FfiError> {
        self.client()
            .set_age_verification_required(required)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.request_host_restart` — the admin's "restart now" affordance
    /// for the host Ubuntu box of an onboarded VPS (`installers/vps.md` § Host OS
    /// Maintenance § 4). Writes a flag the host reboot-coordinator picks up and
    /// reboots gracefully. Admin-class; rejected on a nest with no maintenance
    /// mount (dev / desktop / bare-metal). The admin-shell `nest-os-restart-now-
    /// button` calls through here.
    pub async fn request_host_restart(&self) -> Result<(), FfiError> {
        self.client()
            .request_host_restart()
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.users.list` — a page of users plus the unpaginated total.
    /// `limit` `None` ⇒ the nest default (50, clamped `1..=500`); `offset` `>=0`.
    pub async fn users_list(
        &self,
        limit: Option<i64>,
        offset: i64,
    ) -> Result<FfiAdminUsersListReply, FfiError> {
        let reply = self
            .client()
            .users_list(limit, offset)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// Every account on the nest, newest first —
    /// `fauna_client_admin::users_list_all`, the one read each admin actor
    /// picker offers from (`admin.md` § 2 → *Which accounts a picker offers*).
    /// It pages `fauna.admin.users.list` to its total, so a native app never
    /// pages it itself for a picker.
    pub async fn users_list_all(&self) -> Result<Vec<FfiAdminUser>, FfiError> {
        let users = fauna_client_admin::users_list_all(&self.client())
            .await
            .map_err(stringify)?;
        Ok(users.into_iter().map(Into::into).collect())
    }

    /// `fauna.admin.users.create` — admit a known actor id directly, the
    /// third of the three account-creation paths (`public-mode.md` §
    /// Registration & Identity; the `admin-users` Admit section's one call).
    /// `handle` is the handle the actor is admitted under — there is no
    /// set-later (admins can only *clear* a handle) — `None` admits the
    /// deliberate handle-less state (§ A handle-less account), which cannot
    /// send deployment-domain mail. `label` is always empty, matching
    /// tui/linux/web — there is no free-text label on direct admission, only
    /// a tier. A duplicate actor or taken handle is `fauna.admin.conflict`, a
    /// malformed/reserved handle `fauna.admin.invalid_params`.
    pub async fn users_create(
        &self,
        actor_id: Vec<u8>,
        tier: String,
        handle: Option<String>,
    ) -> Result<(), FfiError> {
        self.client()
            .users_create(actor_id, tier, String::new(), handle)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.users.update` — set a user's `tier` (= the quota) + `label`
    /// (the change-tier control on the Users section). A missing user is
    /// `fauna.admin.not_found`.
    pub async fn users_update(
        &self,
        actor_id: Vec<u8>,
        tier: String,
        label: String,
    ) -> Result<(), FfiError> {
        self.client()
            .users_update(actor_id, tier, label)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.users.evict` — schedule an eviction (warn → suspend → delete
    /// timeline) for a user with a `reason` + `category`; the row's `eviction`
    /// then renders the in-flight timeline (the `admin-users-evict-button`
    /// control). A missing user is `fauna.admin.not_found`.
    pub async fn users_evict(
        &self,
        actor_id: Vec<u8>,
        reason: String,
        category: String,
    ) -> Result<(), FfiError> {
        self.client()
            .users_evict(actor_id, reason, category)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.users.cancel_eviction` — cancel an in-flight eviction (the
    /// `admin-users-cancel-eviction-button` control). No active eviction is
    /// `fauna.admin.not_found`.
    pub async fn users_cancel_eviction(&self, actor_id: Vec<u8>) -> Result<(), FfiError> {
        self.client()
            .users_cancel_eviction(actor_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.users.suspend` — cut a user off *now*, no delete timeline
    /// (the `admin-users-suspend-button` control; `admin.md` § 2 Users →
    /// *Cutting a user off*). Reachable from `Active` or `EvictionWarning`
    /// (clears the pending delete); restore is the same
    /// [`Self::users_cancel_eviction`] control the eviction ladder already
    /// uses. A missing user is `fauna.admin.not_found`; an admin target is
    /// `fauna.admin.conflict`. Empty `reason`/`category` default nest-side to
    /// `"suspended by admin"`/`"other"`. The `{ ok: true }` reply is discarded.
    pub async fn users_suspend(
        &self,
        actor_id: Vec<u8>,
        reason: String,
        category: String,
    ) -> Result<(), FfiError> {
        self.client()
            .users_suspend(actor_id, reason, category)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.admin.admins.add` — grant the admin role, the roster surface's
    /// `admin-users-make-admin-button` (`admin.md` § Admin continuity and
    /// succession, instrument 1). Schedules an `AdminAdd` pending action (24h
    /// delay) — the row does not flip to an admin row right away; a scheduled
    /// reply (no error) is success. The queued-action summary is discarded, same
    /// as [`Self::users_suspend`] — the caller re-reads the page.
    pub async fn admins_add(&self, actor_id: Vec<u8>) -> Result<(), FfiError> {
        self.client()
            .admins_add(actor_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.admin.admins.remove` — revoke the admin role, the roster surface's
    /// `admin-users-remove-admin-button`. Schedules an `AdminRemove` pending
    /// action; refuses (`fauna.admin.conflict`) when it would leave zero
    /// superadmins — the nest, not the client, makes that call.
    pub async fn admins_remove(&self, actor_id: Vec<u8>) -> Result<(), FfiError> {
        self.client()
            .admins_remove(actor_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.admin.evictions.list` — every user with an in-flight eviction (each
    /// row's `eviction` is always `Some`). Backs the Users section's eviction
    /// view; reuses the [`FfiAdminUser`] row shape.
    pub async fn evictions_list(&self) -> Result<Vec<FfiAdminUser>, FfiError> {
        let reply = self.client().evictions_list().await.map_err(stringify)?;
        Ok(reply.evictions.into_iter().map(Into::into).collect())
    }

    /// `fauna.admin.tiers.list` — every defined tier (the tier pickers' options).
    pub async fn tiers_list(&self) -> Result<Vec<FfiAdminTier>, FfiError> {
        let reply = self.client().tiers_list().await.map_err(stringify)?;
        Ok(reply.tiers.into_iter().map(Into::into).collect())
    }

    /// `fauna.admin.tiers.create` — define a new tier (the `admin-settings`
    /// tier-definition UI). An empty `name` is `fauna.admin.invalid_params`; a
    /// duplicate name is `fauna.admin.conflict`.
    pub async fn tiers_create(
        &self,
        name: String,
        max_inbox_bytes: i64,
        max_storage_bytes: i64,
        max_devices: i64,
        max_blob_size: i64,
        max_feeds: i64,
    ) -> Result<(), FfiError> {
        self.client()
            .tiers_create(AdminTierCreateRequest {
                name,
                max_inbox_bytes,
                max_storage_bytes,
                max_devices,
                max_blob_size,
                max_feeds,
                ..Default::default()
            })
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.membership_tiers.list` — every membership designation the
    /// admin owns (monetization.md § Pillar 4). An empty list is the
    /// out-of-the-box state: nothing about this nest is monetized.
    pub async fn membership_tiers_list(&self) -> Result<Vec<FfiAdminMembershipTier>, FfiError> {
        let reply = self
            .client()
            .membership_tiers_list()
            .await
            .map_err(stringify)?;
        Ok(reply.membership_tiers.into_iter().map(Into::into).collect())
    }

    /// `fauna.admin.membership_tiers.set` — designate one of the admin's own
    /// subscription tiers as a membership tier, or re-point an existing
    /// designation (an upsert — safe to repeat). Pass an empty `lapse_tier` for
    /// the documented default (`free`). A `tier_name` the admin does not own is
    /// `fauna.admin.not_found`; a quota tier that does not exist is
    /// `fauna.admin.invalid_params`.
    pub async fn membership_tiers_set(
        &self,
        tier_name: String,
        admin_tier: String,
        lapse_tier: String,
    ) -> Result<(), FfiError> {
        self.client()
            .membership_tiers_set(AdminMembershipTierSetRequest {
                tier_name,
                admin_tier,
                // Empty ⇒ omitted on the wire ⇒ the nest applies the default.
                lapse_tier: (!lapse_tier.is_empty()).then_some(lapse_tier),
                ..Default::default()
            })
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.membership_tiers.clear` — drop a designation, leaving the
    /// subscription tier itself untouched. A tier carrying no designation is
    /// `fauna.admin.not_found`.
    pub async fn membership_tiers_clear(&self, tier_name: String) -> Result<(), FfiError> {
        self.client()
            .membership_tiers_clear(tier_name)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.tiers.update` — overwrite a tier's caps. A missing tier is
    /// `fauna.admin.not_found`.
    pub async fn tiers_update(
        &self,
        name: String,
        max_inbox_bytes: i64,
        max_storage_bytes: i64,
        max_devices: i64,
        max_blob_size: i64,
        max_feeds: i64,
    ) -> Result<(), FfiError> {
        self.client()
            .tiers_update(AdminTierUpdateRequest {
                name,
                max_inbox_bytes,
                max_storage_bytes,
                max_devices,
                max_blob_size,
                max_feeds,
                ..Default::default()
            })
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.invite_codes.list` — every closed-registration invite code.
    pub async fn invite_codes_list(&self) -> Result<Vec<FfiAdminInviteCode>, FfiError> {
        let reply = self.client().invite_codes_list().await.map_err(stringify)?;
        Ok(reply.invite_codes.into_iter().map(Into::into).collect())
    }

    /// `fauna.admin.invite_codes.create` — mint (or register) a code at `tier`
    /// + `uses`. **Pass an empty `code` to have the nest mint a random token**
    /// (the admit-a-user flow on the Invite section); the returned `String` is
    /// the resulting code for the UI to surface/copy. A supplied non-empty
    /// `code` is used verbatim; a duplicate is `fauna.admin.conflict`.
    /// `guardian_actor` links the redeemed account to a guardian for supervised
    /// admission (`family-safety.md` § Wire & data shape); `None` mints an
    /// ordinary code. `age_band` is the admitting guardian's band for that
    /// supervised admission — the `admin-users-invite-age-band-select` option
    /// value (a wire token from `age_band_options`, or `None`/the not-set
    /// value); anything the vocabulary cannot name is refused **here**, never
    /// sent (`family-safety.md` § The account age band, D2).
    pub async fn invite_codes_create(
        &self,
        code: String,
        tier: String,
        uses: i64,
        guardian_actor: Option<Vec<u8>>,
        age_band: Option<String>,
    ) -> Result<String, FfiError> {
        let age_band = parse_age_band(age_band)?;
        let reply = self
            .client()
            .invite_codes_create(code, tier, uses, guardian_actor, age_band)
            .await
            .map_err(stringify)?;
        Ok(reply.code)
    }

    /// `fauna.admin.invite_codes.delete` — remove a code. A missing code is
    /// `fauna.admin.not_found`.
    pub async fn invite_codes_delete(&self, code: String) -> Result<(), FfiError> {
        self.client()
            .invite_codes_delete(code)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.invite_requests.list` — every invite request (pending +
    /// decided), the Pending requests section.
    pub async fn invite_requests_list(&self) -> Result<Vec<FfiAdminInviteRequest>, FfiError> {
        let reply = self
            .client()
            .invite_requests_list()
            .await
            .map_err(stringify)?;
        Ok(reply.invite_requests.into_iter().map(Into::into).collect())
    }

    /// `fauna.admin.invite_requests.approve` — admit the requester, creating the
    /// account at `tier` (`None` ⇒ the nest default `"free"`) with an optional
    /// `label`. Returns the resolved `{ actor_id, handle, tier }`. A missing
    /// request is `fauna.admin.not_found`; a non-pending request / re-taken
    /// handle / duplicate actor is `fauna.admin.conflict`. `guardian_actor`
    /// links the admitted account to a guardian for supervised admission
    /// (`family-safety.md` § Wire & data shape); `None` admits an ordinary
    /// account. `age_band` is the `invite-request-row-age-band-select` option
    /// value (a wire token or `None`/the not-set value), refused here when the
    /// vocabulary cannot name it.
    pub async fn invite_requests_approve(
        &self,
        id: i64,
        tier: Option<String>,
        label: Option<String>,
        guardian_actor: Option<Vec<u8>>,
        age_band: Option<String>,
    ) -> Result<FfiAdminInviteRequestApproveReply, FfiError> {
        let age_band = parse_age_band(age_band)?;
        let reply = self
            .client()
            .invite_requests_approve(id, tier, label, guardian_actor, age_band)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.admin.invite_requests.deny` — mark a request denied with an
    /// optional `reason` (surfaced on the requester's `invite_request_pending`
    /// screen). A missing request is `fauna.admin.not_found`; a non-pending one
    /// is `fauna.admin.conflict`.
    pub async fn invite_requests_deny(
        &self,
        id: i64,
        reason: Option<String>,
    ) -> Result<(), FfiError> {
        self.client()
            .invite_requests_deny(id, reason)
            .await
            .map_err(stringify)
    }

    /// `fauna.admin.services.list` — the sidecar-service enable flags backing the
    /// `admin-services` toggles (`bridge` / `pairing`). Replay-safe
    /// pure read. The dns toggle is synced separately from the
    /// `DnsManagementMachine` (no `dns` service flag — admin.md § 5).
    pub async fn services_list(&self) -> Result<FfiAdminServiceFlags, FfiError> {
        let reply = self.client().services_list().await.map_err(stringify)?;
        Ok(reply.services.into())
    }

    /// `fauna.admin.services.update` — flip one service flag. `name` ∈
    /// {`bridge`, `pairing`} (anything else is
    /// `fauna.admin.invalid_params` nest-side). The applied echo is discarded —
    /// the UI re-reads `services_list` to reflect the toggle.
    pub async fn services_update(&self, name: String, enabled: bool) -> Result<(), FfiError> {
        self.client()
            .services_update(name, enabled)
            .await
            .map(|_| ())
            .map_err(stringify)
    }

    /// `fauna.admin.factory_reset` — return the nest to fresh / unclaimed via a
    /// restart-wipe (the admin "Factory reset this nest" Danger-zone control,
    /// `admin-factory-reset-button`). `new_claim_code` `None` ⇒ the nest mints a
    /// random post-reset code; the returned `String` is the code the client
    /// re-seeds onboarding with — the **human never sees it** (the handler stages
    /// the code, replies, then exits + restarts; the box wipes deployment state
    /// but preserves the nest identity, ACME cert, DKIM + bridge keypairs). The
    /// caller tears down the authed session keeping local creds and re-claims at
    /// the claim-code step with this code pre-filled. Per
    /// `docs/goal/behavior/mail-bridge-lifecycle.md` § Factory reset.
    pub async fn factory_reset(&self, new_claim_code: Option<String>) -> Result<String, FfiError> {
        let reply = self
            .client()
            .factory_reset(new_claim_code)
            .await
            .map_err(stringify)?;
        Ok(reply.claim_code)
    }
}

// `fauna.admin.logs` lives in its own gated `#[uniffi::export]` impl block (not
// folded into the block above) because `#[uniffi::export]` re-emits the method
// scaffolding unconditionally — a `#[cfg]` on a single method *inside* an export
// block doesn't gate the generated FFI wrapper, so the whole block must carry
// the cfg.
#[cfg(feature = "logs")]
#[fauna_uniffi_async::export]
impl FfiAdminClient {
    /// `fauna.admin.logs` — the nest's in-memory `fauna-log` ring snapshot,
    /// mapped to the *same* `fauna_log::LogEntry` the client's own Settings →
    /// Logs page renders, so the admin Logs page reuses one widget (the Linux
    /// `logs_view` shape). The nest ring has no Clear (no RPC wipes it). Gated
    /// on `logs` like the rest of the `fauna-log` exposure — dropped from the Go
    /// mail-bridge `--no-default-features` FFI build, which has no admin surface.
    /// See `docs/goal/architecture/apps/observability.md` § Surfaces.
    pub async fn logs(&self) -> Result<Vec<fauna_log::LogEntry>, FfiError> {
        let reply = self.client().logs().await.map_err(stringify)?;
        Ok(reply
            .entries
            .iter()
            .map(fauna_client_admin::log_entry_from_wire)
            .collect())
    }
}

// ── Registration-mode picker catalog (client-consumption) ──────────────
//
// Thin FFI mirror of `fauna_client_admin::registration_mode_options` — the
// single source of the `admin-users-registration-mode-select` wire vocabulary
// + display order + i18n label. Mirrors `family.rs`'s
// `unknown_sender_options`/`ReachPolicyOption` pattern; `fauna_client_admin`
// carries no `uniffi` feature (like `fauna_client_feed`), so the type crosses
// through an `Ffi*` mirror declared here rather than a derive on the shared
// crate (mirrors `feed_rules.rs::FfiRuleTypeOption`).
//
// Gated behind `value-format` (default-on, dropped from the Go mail-bridge
// `--no-default-features` build): a `#[uniffi::export]` returning a
// `fauna_core` `LocalizedText` makes uniffi-bindgen-go emit an uncompilable
// bare `import "fauna_core"` — the value-format footgun (see `family.rs`'s
// note). The mail-bridge has no use for client-picker label strings.

/// FFI mirror of [`fauna_client_admin::RegistrationModeOption`] — one
/// `admin-users-registration-mode-select` option.
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiRegistrationModeOption {
    /// The canonical wire value ([`RegistrationMode::as_wire_str`]). What the
    /// select writes to `fauna.admin.set_registration_mode` and what the
    /// cross-app `select(id, value)` e2e contract drives. Never localized.
    pub value: String,
    /// The picker label (`admin.users_page.registration_mode_*`), for the
    /// client to resolve.
    pub label: LocalizedText,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_admin::RegistrationModeOption> for FfiRegistrationModeOption {
    fn from(o: fauna_client_admin::RegistrationModeOption) -> Self {
        FfiRegistrationModeOption {
            value: o.value,
            label: o.label,
        }
    }
}

/// UniFFI façade for [`fauna_client_admin::registration_mode_options`] — the
/// 3-option `admin-users-registration-mode-select` catalog (wire value +
/// localized label, in display order). Lets the Apple / Windows / Android
/// forms drop their hand-rolled registration-mode maps.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn registration_mode_options() -> Vec<FfiRegistrationModeOption> {
    fauna_client_admin::registration_mode_options()
        .into_iter()
        .map(FfiRegistrationModeOption::from)
        .collect()
}

/// The two admission age-band selects' option value → the typed band the
/// shared writer takes. `None` and the not-set value are "no band"; a token
/// the vocabulary cannot name is an error at this boundary, so a stale or
/// hand-typed select value never reaches the wire as a band.
fn parse_age_band(value: Option<String>) -> Result<Option<AgeBand>, FfiError> {
    match value.as_deref() {
        None | Some(fauna_client_admin::AGE_BAND_NOT_SET_VALUE) => Ok(None),
        Some(token) => AgeBand::from_wire(token)
            .map(Some)
            .ok_or_else(|| FfiError::General {
                msg: format!(
                    "unknown age band {token:?}; expected one of u13 / 13-15 / 16-17 / 18+"
                ),
            }),
    }
}

/// FFI mirror of [`fauna_client_admin::AgeBandOption`] — one option of the two
/// admission age-band selects (`admin-users-invite-age-band-select` /
/// `invite-request-row-age-band-select`).
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiAgeBandOption {
    /// The canonical wire token, or the empty not-set value
    /// ([`fauna_client_admin::AGE_BAND_NOT_SET_VALUE`]). What the select passes
    /// to `invite_codes_create` / `invite_requests_approve`. Never localized.
    pub value: String,
    /// The picker label (`family.age_band.*`), for the client to resolve.
    pub label: LocalizedText,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_admin::AgeBandOption> for FfiAgeBandOption {
    fn from(o: fauna_client_admin::AgeBandOption) -> Self {
        FfiAgeBandOption {
            value: o.value,
            label: o.label,
        }
    }
}

/// UniFFI façade for [`fauna_client_admin::age_band_options`] — the 5-option
/// age-band catalog (*not set* + the four bands, in the ratified order), so no
/// native app spells the vocabulary or its order itself.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn age_band_options() -> Vec<FfiAgeBandOption> {
    fauna_client_admin::age_band_options()
        .into_iter()
        .map(FfiAgeBandOption::from)
        .collect()
}

/// The *not set* option value of the two admission age-band selects
/// ([`fauna_client_admin::AGE_BAND_NOT_SET_VALUE`]) — the default a fresh
/// select holds and the value it resets to when its guardian is cleared, so
/// no native app spells the sentinel itself.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn age_band_not_set_value() -> String {
    fauna_client_admin::AGE_BAND_NOT_SET_VALUE.to_string()
}

/// The `invite-request-row-age-band-select` seed for a pending request — the
/// applicant's claimed band when this client can name it, else *not set*
/// ([`fauna_client_admin::claimed_age_band_option`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn claimed_age_band_option(claimed: Option<String>) -> String {
    fauna_client_admin::claimed_age_band_option(claimed.as_deref())
}

/// The applicant's claim on a pending request row
/// (`invite-request-row-age-claim`) — the shared, total text: "No app age
/// verification" when the request carries no nameable claim
/// ([`fauna_protocol::age::age_claim_label`]). Args are nested keys: resolve
/// with `resolve_nested`.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn age_claim_label(band: Option<String>, provenance: Option<String>) -> LocalizedText {
    fauna_protocol::age::age_claim_label(band.as_deref(), provenance.as_deref())
}

/// A band's display label for its wire token, or `None` for a token this
/// client cannot name (render nothing — [`fauna_protocol::age::age_band_label`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn age_band_label(band: String) -> Option<LocalizedText> {
    fauna_protocol::age::age_band_label(&band)
}

/// The two family-page readouts' line for a band + provenance — the
/// guardian's per-ward row (`own = false`) or the ward's own summary (`own =
/// true`); `None` when the band is unnamed (absent, never placeholdered —
/// [`fauna_protocol::age::age_band_line`]). Args are nested keys: resolve with
/// `resolve_nested`.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn age_band_line(band: String, provenance: String, own: bool) -> Option<LocalizedText> {
    fauna_protocol::age::age_band_line(&band, &provenance, own)
}

// ── Deployment-seed rotation confirm surface (`admin-nest-seed-rotate-*`) ────
//
// The native (windows/apple/android) leg — the reference is tui
// (`apps/fauna-tui/src/admin/mod.rs::load_seed_rotate_roster`) and linux
// (`apps/fauna-linux/src/client.rs`), both calling `AdminClient` directly;
// this delegates to `AdminClient::seed_rotate_roster_view` over the FFI
// boundary. A free function (not an `FfiAdminClient` method) for the same
// reason `registration_mode_options` is one: keeping every `LocalizedText`-
// touching export cleanly cfg-gated out of the Go mail-bridge build, never
// mixed into the always-on impl block above.

/// One admin who inherits the deployment-seed rotation's successor — mirrors
/// [`fauna_client_admin::SeedRotationInheritor`].
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSeedRotationInheritor {
    /// Raw 32-byte actor id.
    pub actor_id: Vec<u8>,
    /// The member's resolved `AdminUser.label`, or the canonical short id when
    /// unresolved/blank — never dropped, because an inheritor we cannot name
    /// still inherits.
    pub label: String,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_admin::SeedRotationInheritor> for FfiSeedRotationInheritor {
    fn from(i: fauna_client_admin::SeedRotationInheritor) -> Self {
        FfiSeedRotationInheritor {
            actor_id: i.actor_id,
            label: i.label,
        }
    }
}

/// What the rotation confirm surface renders — mirrors
/// [`fauna_client_admin::SeedRotationConfirmView`]. `blocked_reason` is `Some`
/// exactly when `can_confirm` is `false` (an empty roster withholds the
/// destructive confirm — `box-recovery.md` § Deployment-seed rotation →
/// *Ordering rule*).
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSeedRotationConfirmView {
    /// The current roster, in the nest's own order — the set that will inherit.
    pub inheritors: Vec<FfiSeedRotationInheritor>,
    /// Whether the destructive confirm may be offered at all.
    pub can_confirm: bool,
    /// Why not, when it may not.
    pub blocked_reason: Option<LocalizedText>,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_admin::SeedRotationConfirmView> for FfiSeedRotationConfirmView {
    fn from(v: fauna_client_admin::SeedRotationConfirmView) -> Self {
        FfiSeedRotationConfirmView {
            inheritors: v.inheritors.into_iter().map(Into::into).collect(),
            can_confirm: v.can_confirm,
            blocked_reason: v.blocked_reason,
        }
    }
}

/// Read the current admin roster and fold it into the rotation confirm
/// surface (`fauna.admin.admins.list`, plus a best-effort `users.get` per
/// member for the handle label — the roster wire shape carries no name at
/// all). Read-only: nothing is rotated by arming.
///
/// # Errors
///
/// - The `FfiError` from a failed `fauna.admin.admins.list` — the caller
///   renders it as the confirm's `Failed` state (`box-recovery.md` §
///   Deployment-seed rotation: the confirm must say *why* it does not know,
///   never paint an empty roster as if it were the answer).
#[cfg(feature = "value-format")]
#[fauna_uniffi_async::export]
pub async fn seed_rotate_roster(
    nest: Arc<crate::nest_client::FfiNestClient>,
) -> Result<FfiSeedRotationConfirmView, FfiError> {
    let client = AdminClient::new(nest.nest_arc());
    Ok(client
        .seed_rotate_roster_view()
        .await
        .map_err(stringify)?
        .into())
}

// ── Declared region (`admin-nest-region-*`) ──────────────────────────────
//
// The native (windows/apple/android) leg — the reference is
// tui and linux/web (call `AdminClient::
// region_status`/`set_region` directly, no FFI); this is the same read + fold
// + write, once, over the FFI boundary. Free functions (not `FfiAdminClient`
// methods) for the same reason `seed_rotate_roster` above is one: keeping
// every `LocalizedText`-touching export cleanly cfg-gated out of the Go
// mail-bridge build, never mixed into the always-on impl block.

/// FFI mirror of [`fauna_client_admin::AdminRegionView`] — every rendering
/// decision `admin-nest-region-*` needs, already made (`docs/goal/behavior/
/// region-blocking.md` § The region/authority plumbing). Do NOT re-derive any
/// of these from `declared`/`stale` in app code — see the shared type's own
/// field docs for why each one is a decision, not a passthrough.
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiAdminRegionView {
    /// The declared code, for `admin-nest-region-input`'s re-seed. `None` is
    /// the ratified fresh-install state.
    pub declared: Option<String>,
    /// `admin-nest-region-status`.
    pub status: LocalizedText,
    /// `admin-nest-region-authority` (`optional_elements` — absent exactly
    /// when nothing is declared and no document still binds).
    pub authority: Option<LocalizedText>,
    /// `admin-nest-region-staleness` (`optional_elements` — present only when
    /// stale).
    pub staleness: Option<LocalizedText>,
    /// Whether `admin-nest-region-withdraw-button` paints at all.
    pub can_withdraw: bool,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_admin::AdminRegionView> for FfiAdminRegionView {
    fn from(v: fauna_client_admin::AdminRegionView) -> Self {
        FfiAdminRegionView {
            declared: v.declared,
            status: v.status,
            authority: v.authority,
            staleness: v.staleness,
            can_withdraw: v.can_withdraw,
        }
    }
}

/// `fauna.admin.region.get` folded through [`fauna_client_admin::admin_region_view`]
/// — the one call `admin-nest-region-section` needs to paint all five fields.
#[cfg(feature = "value-format")]
#[fauna_uniffi_async::export]
pub async fn admin_region_status(
    nest: Arc<crate::nest_client::FfiNestClient>,
) -> Result<FfiAdminRegionView, FfiError> {
    let client = AdminClient::new(nest.nest_arc());
    let reply = client.region_status().await.map_err(stringify)?;
    Ok(fauna_client_admin::admin_region_view(&reply).into())
}

/// Validate a typed region code before it reaches the wire —
/// `admin-nest-region-input`'s client-side refusal path
/// (`test_malformed_region_is_refused_client_side`). `Ok` carries the code
/// unchanged (the parser never case-folds — two spellings of one region must
/// not both be storable); `Err` carries the shared engine's own i18n key for
/// the caller to resolve and show on `error-message`, same convention as
/// [`crate::family::parse_time_of_day`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn admin_parse_region_code(raw: String) -> Result<String, FfiError> {
    fauna_client_admin::parse_region_code(&raw)
        .map(|code| code.as_str().to_string())
        .map_err(|msg| FfiError::General {
            msg: msg.to_string(),
        })
}

/// `fauna.admin.region.set` — `admin-nest-region-save-button` (`region:
/// Some(code)`, already validated by [`admin_parse_region_code`]) and
/// `admin-nest-region-withdraw-button` (`region: None`) both call through
/// here; re-validates regardless of whether the caller already did, since
/// this is the only path that can reach the wire.
#[cfg(feature = "value-format")]
#[fauna_uniffi_async::export]
pub async fn admin_set_region(
    nest: Arc<crate::nest_client::FfiNestClient>,
    region: Option<String>,
) -> Result<(), FfiError> {
    let client = AdminClient::new(nest.nest_arc());
    let region = region
        .map(|r| fauna_client_admin::parse_region_code(&r))
        .transpose()
        .map_err(|msg| FfiError::General {
            msg: msg.to_string(),
        })?;
    client.set_region(region).await.map_err(stringify)
}

// ── Web-app origin (`admin-nest-web-app-origin-*`) ────────────────────────
//
// The native (windows/apple/android) face of the shared read + fold + write
// (`admin.md` § N Nest → *Web-app origin*); tui and linux call
// `AdminClient::web_app_origin`/`set_web_app_origin` directly. Free functions,
// `value-format` gated, for the region block's reason.

/// FFI mirror of [`fauna_client_admin::WebAppOrigin`] — the two modes an app
/// can choose.
#[cfg(feature = "value-format")]
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiWebAppOrigin {
    Bundled,
    Central,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_admin::WebAppOrigin> for FfiWebAppOrigin {
    fn from(m: fauna_client_admin::WebAppOrigin) -> Self {
        match m {
            fauna_client_admin::WebAppOrigin::Bundled => FfiWebAppOrigin::Bundled,
            fauna_client_admin::WebAppOrigin::Central => FfiWebAppOrigin::Central,
        }
    }
}

#[cfg(feature = "value-format")]
impl From<FfiWebAppOrigin> for fauna_client_admin::WebAppOrigin {
    fn from(m: FfiWebAppOrigin) -> Self {
        match m {
            FfiWebAppOrigin::Bundled => fauna_client_admin::WebAppOrigin::Bundled,
            FfiWebAppOrigin::Central => fauna_client_admin::WebAppOrigin::Central,
        }
    }
}

/// FFI mirror of [`fauna_client_admin::AdminWebAppOriginView`] — every
/// rendering decision the section needs, already made. Do NOT re-derive the
/// status from a mode or a `setup.status` field in app code.
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiAdminWebAppOriginView {
    /// The radio to pre-select; `None` marks neither (a nest predating the
    /// choice, or a mode this build cannot name).
    pub selected: Option<FfiWebAppOrigin>,
    /// `admin-nest-web-app-origin-status`.
    pub status: LocalizedText,
    /// The scope sentence painted under the radios.
    pub scope: LocalizedText,
    /// Whether the radios and `admin-nest-web-app-origin-save-button` are live.
    pub can_set: bool,
    /// `admin-nest-web-app-origin-central-radio`'s label (it names the central
    /// origin from the one constant).
    pub central_label: LocalizedText,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_admin::AdminWebAppOriginView> for FfiAdminWebAppOriginView {
    fn from(v: fauna_client_admin::AdminWebAppOriginView) -> Self {
        FfiAdminWebAppOriginView {
            selected: v.selected.map(Into::into),
            status: v.status,
            scope: v.scope,
            can_set: v.can_set,
            central_label: v.central_label,
        }
    }
}

/// `fauna.admin.web_app_origin.get` folded through
/// [`fauna_client_admin::admin_web_app_origin_view`] — an older nest's
/// unknown-kind answer is the view's "predates the choice" state, not an error.
#[cfg(feature = "value-format")]
#[fauna_uniffi_async::export]
pub async fn admin_web_app_origin_status(
    nest: Arc<crate::nest_client::FfiNestClient>,
) -> Result<FfiAdminWebAppOriginView, FfiError> {
    let client = AdminClient::new(nest.nest_arc());
    let reply = client.web_app_origin().await.map_err(stringify)?;
    Ok(fauna_client_admin::admin_web_app_origin_view(reply.as_ref()).into())
}

/// `fauna.admin.web_app_origin.set` — `admin-nest-web-app-origin-save-button`.
/// Answers the fold of the nest's reply, so the save repaints the status with
/// no second read.
#[cfg(feature = "value-format")]
#[fauna_uniffi_async::export]
pub async fn admin_set_web_app_origin(
    nest: Arc<crate::nest_client::FfiNestClient>,
    mode: FfiWebAppOrigin,
) -> Result<FfiAdminWebAppOriginView, FfiError> {
    let client = AdminClient::new(nest.nest_arc());
    let reply = client
        .set_web_app_origin(mode.into())
        .await
        .map_err(stringify)?;
    Ok(fauna_client_admin::admin_web_app_origin_view(Some(&reply)).into())
}

// ── Outside-app sign-in keys (`admin-nest-oauth-*`) ──────────────────────
//
// The native (windows/apple/android) face of the issuer key controls
// (`authorization-server.md` § The issuer → *Two rotation arms*). The reference
// is tui (`apps/fauna-tui/src/admin/nest.rs::oauth_elements`); tui and linux
// call `AdminClient`'s three issuer doors directly, and this is the same read,
// the same folds and the same worded dispatches, once, over the FFI boundary —
// no app words a verdict or picks a sentence. Free functions, `value-format`
// gated, for the region block's reason: every one touches `LocalizedText`,
// which the Go mail-bridge build cannot carry.
//
// The two records cross the boundary in BOTH directions: the app holds the
// folded view it painted and hands it back to [`issuer_key_rotate_cost`] and
// [`issuer_forced_confirm_view`] (the latter at arm time, so the confirm names
// the set the admin was looking at), and each row to [`issuer_key_row_label`]
// at paint, because the countdown is counted against the paint-time clock.

/// FFI mirror of [`fauna_client_admin::IssuerKeyRow`] — one served key.
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiIssuerKeyRow {
    /// The RFC 7638 thumbprint `/oauth/jwks` publishes for this key.
    pub kid: String,
    /// Whether this is the key currently signing (the signer comes first).
    pub signing: bool,
    /// When a rotation retired this key (epoch seconds); `None` for the signer.
    pub retired_at: Option<i64>,
    /// When the JWKS stops carrying this key (epoch seconds) — the nest's own
    /// `retired_at + horizon`, already summed; `None` for the signer.
    pub served_until: Option<i64>,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_admin::IssuerKeyRow> for FfiIssuerKeyRow {
    fn from(r: fauna_client_admin::IssuerKeyRow) -> Self {
        FfiIssuerKeyRow {
            kid: r.kid,
            signing: r.signing,
            retired_at: r.retired_at,
            served_until: r.served_until,
        }
    }
}

#[cfg(feature = "value-format")]
impl From<FfiIssuerKeyRow> for fauna_client_admin::IssuerKeyRow {
    fn from(r: FfiIssuerKeyRow) -> Self {
        fauna_client_admin::IssuerKeyRow {
            kid: r.kid,
            signing: r.signing,
            retired_at: r.retired_at,
            served_until: r.served_until,
        }
    }
}

/// FFI mirror of [`fauna_client_admin::IssuerKeyView`] — what
/// `admin-nest-oauth-section` paints from. Paint `admin-nest-oauth-key-item-{n}`
/// ONLY from a view [`admin_issuer_key_status`] returned: a failed read is the
/// `admin-nest-oauth-key-reason` line, never an empty list.
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiIssuerKeyView {
    /// The `kid` currently signing.
    pub active_kid: String,
    /// Every served key, signer first, in the nest's own order.
    pub keys: Vec<FfiIssuerKeyRow>,
    /// How long after `retired_at` a key stops being served, in seconds.
    pub retirement_horizon_secs: u64,
    /// Whether a retired key is still being served.
    pub rotation_in_flight: bool,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_admin::IssuerKeyView> for FfiIssuerKeyView {
    fn from(v: fauna_client_admin::IssuerKeyView) -> Self {
        FfiIssuerKeyView {
            active_kid: v.active_kid,
            keys: v.keys.into_iter().map(Into::into).collect(),
            retirement_horizon_secs: v.retirement_horizon_secs,
            rotation_in_flight: v.rotation_in_flight,
        }
    }
}

#[cfg(feature = "value-format")]
impl From<FfiIssuerKeyView> for fauna_client_admin::IssuerKeyView {
    fn from(v: FfiIssuerKeyView) -> Self {
        fauna_client_admin::IssuerKeyView {
            active_kid: v.active_kid,
            keys: v.keys.into_iter().map(Into::into).collect(),
            retirement_horizon_secs: v.retirement_horizon_secs,
            rotation_in_flight: v.rotation_in_flight,
        }
    }
}

/// FFI mirror of [`fauna_client_admin::IssuerForcedArm`] — the two forced arms
/// behind the one inline confirm. The armed confirm carries the arm it was
/// painted for into [`admin_force_rotate_issuer`], so it can never fire its
/// sibling.
#[cfg(feature = "value-format")]
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum FfiIssuerForcedArm {
    /// `admin-nest-oauth-force-rotate-button` — drop every served key at once.
    IssuerKey,
    /// `admin-nest-oauth-secret-force-rotate-button` — end every saved sign-in.
    SessionSecret,
}

#[cfg(feature = "value-format")]
impl From<FfiIssuerForcedArm> for fauna_client_admin::IssuerForcedArm {
    fn from(a: FfiIssuerForcedArm) -> Self {
        match a {
            FfiIssuerForcedArm::IssuerKey => fauna_client_admin::IssuerForcedArm::IssuerKey,
            FfiIssuerForcedArm::SessionSecret => fauna_client_admin::IssuerForcedArm::SessionSecret,
        }
    }
}

/// FFI mirror of [`fauna_client_admin::IssuerForcedConfirmView`] — the armed
/// confirm's `admin-nest-oauth-confirm-summary` and its button's label.
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiIssuerForcedConfirmView {
    pub summary: LocalizedText,
    pub confirm_label: LocalizedText,
}

/// `fauna.oauth.issuer_key_status` folded through the shared
/// `issuer_key_view` — the section's one read. Non-fatal to the page: render
/// the `Err` (resolved through `admin.nest_page.oauth_keys_error`) on
/// `admin-nest-oauth-key-reason` and keep painting everything else, since any failure
/// (a transport error included) must not blank the rest of the page.
#[cfg(feature = "value-format")]
#[fauna_uniffi_async::export]
pub async fn admin_issuer_key_status(
    nest: Arc<crate::nest_client::FfiNestClient>,
) -> Result<FfiIssuerKeyView, FfiError> {
    let client = AdminClient::new(nest.nest_arc());
    Ok(client
        .issuer_key_status_view()
        .await
        .map_err(stringify)?
        .into())
}

/// One `admin-nest-oauth-key-item-{n}` line — `now_secs` is the app's clock
/// at paint (epoch seconds), because a retired key's countdown is the point
/// of its line.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn issuer_key_row_label(row: FfiIssuerKeyRow, now_secs: i64) -> LocalizedText {
    fauna_client_admin::issuer_key_row_label(&row.into(), now_secs)
}

/// The ordinary arm's cost, painted beside `admin-nest-oauth-rotate-button`
/// (it has no confirm — nothing breaks).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn issuer_key_rotate_cost(view: FfiIssuerKeyView) -> LocalizedText {
    fauna_client_admin::issuer_key_rotate_cost(&view.into())
}

/// Fold `arm` into its confirm, at ARM time, over the view the admin is
/// looking at — the confirm then renders what this returned and is never
/// re-folded while armed.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn issuer_forced_confirm_view(
    arm: FfiIssuerForcedArm,
    view: FfiIssuerKeyView,
) -> FfiIssuerForcedConfirmView {
    let confirm = fauna_client_admin::issuer_forced_confirm_view(arm.into(), &view.into());
    FfiIssuerForcedConfirmView {
        summary: confirm.summary,
        confirm_label: confirm.confirm_label,
    }
}

/// `admin-nest-oauth-rotate-button` — the ordinary rotation, dispatched and
/// worded: the returned sentence IS `admin-nest-oauth-status`, success or
/// failure (never an error — a reply lost to a timeout can follow a committed
/// rotation, and only the shared fold may say so). Re-read the key set after.
#[cfg(feature = "value-format")]
#[fauna_uniffi_async::export]
pub async fn admin_rotate_issuer_key(
    nest: Arc<crate::nest_client::FfiNestClient>,
) -> LocalizedText {
    AdminClient::new(nest.nest_arc())
        .rotate_issuer_key_verdict()
        .await
}

/// `admin-nest-oauth-confirm-button` — exactly the armed `arm`'s kind,
/// dispatched and worded like [`admin_rotate_issuer_key`]. The session-secret
/// verdict dates the ended generation with the shared native clock face
/// (`fauna_core::format::format_unix_local`, the one `value_format.rs`
/// exports) — the same rendering tui and linux give it. Disarm before calling.
#[cfg(feature = "value-format")]
#[fauna_uniffi_async::export]
pub async fn admin_force_rotate_issuer(
    nest: Arc<crate::nest_client::FfiNestClient>,
    arm: FfiIssuerForcedArm,
) -> LocalizedText {
    AdminClient::new(nest.nest_arc())
        .force_rotate_verdict(arm.into(), fauna_core::format::format_unix_local)
        .await
}

// ── Custody-hosting registry (`admin-custody-hosting`) ──────────────────
//
// The native (windows/apple/android) leg — the reference is tui
// (`apps/fauna-tui/src/admin/custody_hosting.rs`) and linux
// (`apps/fauna-linux/src/client.rs`'s `fetch_custody_hosting`/
// `remove_custody_hosting`), both calling `fauna_client_capabilities::
// custody_hosting::AdminHostingClient` directly (Rust-native, no FFI); this is
// the same read + fold, once, over the FFI boundary. A dedicated gated impl
// block (not folded into the always-on block above) for the same reason
// `logs` is one: `#[uniffi::export]` re-emits the method scaffolding
// unconditionally, so a `#[cfg]` on one method inside a shared block would not
// gate the generated wrapper. Gated on `custody` — the feature that pulls in
// `fauna-client-capabilities` at all (`custody.rs`'s module doc explains why
// the dep is optional: it enables `fauna-sync-engine/account-runtime`, which
// the Go mail-bridge build does not pay for).

#[cfg(feature = "custody")]
impl FfiAdminClient {
    fn hosting_client(
        &self,
    ) -> fauna_client_capabilities::custody_hosting::AdminHostingClient<Arc<NestClient>> {
        fauna_client_capabilities::custody_hosting::AdminHostingClient::new(Arc::clone(&self.nest))
    }
}

#[cfg(feature = "custody")]
#[fauna_uniffi_async::export]
impl FfiAdminClient {
    /// `fauna.admin.custody_hosting.list` — every hosting row on this nest,
    /// folded by the shared `admin_hosting_rows` projection (heaviest hold
    /// first, tie-broken on `(host, owner, grant)`) so every lift app renders
    /// the same rows in the same order. Do NOT re-sort or re-derive receipt
    /// freshness client-side.
    pub async fn custody_hosting_list(&self) -> Result<Vec<FfiAdminHostingRow>, FfiError> {
        let reply = self.hosting_client().list().await.map_err(stringify)?;
        let now = fauna_core::data::Timestamp::now().0;
        Ok(
            fauna_client_capabilities::view_model::admin_hosting_rows(&reply, now)
                .into_iter()
                .map(FfiAdminHostingRow::from)
                .collect(),
        )
    }

    /// `fauna.admin.custody_hosting.remove` — drop one row, keyed by the
    /// `(host, grant)` pair a [`custody_hosting_list`] row carries (never a
    /// painted index — a re-read can reorder rows). `removed: false` is an
    /// honest no-op (the row was already gone), not a failure.
    pub async fn custody_hosting_remove(
        &self,
        host_actor_id: String,
        grant_id: Vec<u8>,
    ) -> Result<FfiAdminHostingRemoveReply, FfiError> {
        let reply = self
            .hosting_client()
            .remove(&host_actor_id, &grant_id)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_admin::admin::AdminUser as ProtoUser;
    use fauna_protocol::ByteBuf;
    use fauna_protocol::admin::AdminAdminsListReply;

    #[test]
    fn admin_user_with_eviction_maps() {
        let proto = ProtoUser {
            actor_id: ByteBuf::from(vec![3u8; 32]),
            tier: "personal".into(),
            label: "Alice".into(),
            handle: Some("alice99".into()),
            suspended: false,
            created_at: 1_700_000_000,
            inbox_bytes_used: 1024,
            storage_bytes_used: 4096,
            eviction: Some(AdminEviction {
                status: "warned".into(),
                reason: "capacity".into(),
                category: "capacity".into(),
                warned_at: Some(1_700_000_100),
                suspend_at: None,
                delete_at: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        let ffi = FfiAdminUser::from(proto);
        assert_eq!(ffi.actor_id, vec![3u8; 32]);
        assert_eq!(ffi.tier, "personal");
        assert_eq!(ffi.eviction.as_ref().unwrap().status, "warned");
        assert_eq!(ffi.handle.as_deref(), Some("alice99"));
    }

    #[test]
    fn admin_user_handleless_folds_to_none() {
        let proto = ProtoUser {
            actor_id: ByteBuf::from(vec![4u8; 32]),
            tier: "personal".into(),
            label: "Bob".into(),
            handle: None,
            ..Default::default()
        };
        let ffi = FfiAdminUser::from(proto);
        assert_eq!(ffi.handle, None);
    }

    #[test]
    fn invite_request_decided_fields_map() {
        let proto = AdminInviteRequest {
            id: 7,
            actor_id: ByteBuf::from(vec![1u8; 32]),
            handle: "bob@fauna.test".into(),
            message: "let me in".into(),
            status: "denied".into(),
            created_at: 1_700_000_000,
            decided_at: Some(1_700_000_500),
            decided_by: Some(ByteBuf::from(vec![9u8; 32])),
            denial_reason: Some("nope".into()),
            ..Default::default()
        };
        let ffi = FfiAdminInviteRequest::from(proto);
        assert_eq!(ffi.id, 7);
        assert_eq!(ffi.decided_by, Some(vec![9u8; 32]));
        assert_eq!(ffi.denial_reason.as_deref(), Some("nope"));
    }

    #[test]
    fn service_flags_map_both_toggles() {
        let proto = AdminServiceFlags {
            bridge: true,
            pairing: true,
            ..Default::default()
        };
        let ffi = FfiAdminServiceFlags::from(proto);
        assert!(ffi.bridge);
        assert!(ffi.pairing);
    }

    #[test]
    fn admin_stats_maps_counters_and_tier_breakdown() {
        let proto = AdminStatsReply {
            total_users: 12,
            users_by_tier: vec![("free".into(), 9), ("pro".into(), 3)],
            suspended_users: 1,
            total_inbox_bytes: 4096,
            total_storage_bytes: 8192,
            ws_connections: 5,
            ..Default::default()
        };
        let ffi = FfiAdminStats::from(proto);
        assert_eq!(ffi.total_users, 12);
        assert_eq!(ffi.suspended_users, 1);
        assert_eq!(ffi.total_inbox_bytes, 4096);
        assert_eq!(ffi.total_storage_bytes, 8192);
        assert_eq!(ffi.ws_connections, 5);
        // Order-preserving (a map would not be).
        assert_eq!(ffi.users_by_tier.len(), 2);
        assert_eq!(ffi.users_by_tier[0].tier, "free");
        assert_eq!(ffi.users_by_tier[0].count, 9);
        assert_eq!(ffi.users_by_tier[1].tier, "pro");
        assert_eq!(ffi.users_by_tier[1].count, 3);
    }

    /// The issuer view crosses the boundary BOTH ways — the app hands the
    /// painted view back to the folds — so a lossy mirror would let the armed
    /// confirm name a different set from the one on screen. And each FFI arm
    /// maps to exactly its own wire kind.
    #[cfg(feature = "value-format")]
    #[test]
    fn issuer_key_view_round_trips_the_boundary_and_arms_keep_their_kinds() {
        use fauna_client_admin::{IssuerForcedArm, IssuerKeyRow, IssuerKeyView};
        let view = IssuerKeyView {
            active_kid: "kid-new".into(),
            keys: vec![
                IssuerKeyRow {
                    kid: "kid-new".into(),
                    signing: true,
                    retired_at: None,
                    served_until: None,
                },
                IssuerKeyRow {
                    kid: "kid-old".into(),
                    signing: false,
                    retired_at: Some(100),
                    served_until: Some(1_300),
                },
            ],
            retirement_horizon_secs: 1_200,
            rotation_in_flight: true,
        };
        let back: IssuerKeyView = FfiIssuerKeyView::from(view.clone()).into();
        assert_eq!(back, view);

        assert_eq!(
            IssuerForcedArm::from(FfiIssuerForcedArm::IssuerKey).kind(),
            "fauna.oauth.force_rotate_issuer_key"
        );
        assert_eq!(
            IssuerForcedArm::from(FfiIssuerForcedArm::SessionSecret).kind(),
            "fauna.oauth.force_rotate_session_secret"
        );

        let confirm =
            issuer_forced_confirm_view(FfiIssuerForcedArm::IssuerKey, view.clone().into());
        assert_eq!(
            confirm.summary,
            fauna_client_admin::issuer_forced_confirm_view(IssuerForcedArm::IssuerKey, &view)
                .summary
        );
        assert_eq!(
            issuer_key_row_label(view.keys[1].clone().into(), 100),
            fauna_client_admin::issuer_key_row_label(&view.keys[1], 100)
        );
    }

    #[cfg(feature = "value-format")]
    #[test]
    fn registration_mode_options_maps_value_and_order() {
        let opts = registration_mode_options();
        let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, ["open", "invite_required", "closed"]);
        assert!(opts.iter().all(|o| !o.label.key.is_empty()));
    }

    /// The FFI `From` mirror carries the biconditional the shared fold
    /// establishes (`seed_rotation_confirm_view`): a non-empty roster means
    /// `can_confirm` and no `blocked_reason`, and every inheritor's raw
    /// actor id survives the crossing. Catches a field silently dropped in
    /// the mirror the way the wire-type exhaustiveness guard catches an
    /// added field.
    #[cfg(feature = "value-format")]
    #[test]
    fn seed_rotation_confirm_view_maps_a_non_empty_roster() {
        let roster = AdminAdminsListReply {
            admins: vec![
                fauna_client_admin::admin::AdminAdminEntry {
                    actor_id: ByteBuf::from(vec![1u8; 32]),
                    added_at: 5,
                    ..Default::default()
                },
                fauna_client_admin::admin::AdminAdminEntry {
                    actor_id: ByteBuf::from(vec![2u8; 32]),
                    added_at: 6,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let labels = [(vec![1u8; 32], "Alice".to_string())];
        let view: FfiSeedRotationConfirmView =
            fauna_client_admin::seed_rotation_confirm_view(&roster, &labels).into();

        assert!(view.can_confirm);
        assert!(view.blocked_reason.is_none());
        assert_eq!(view.inheritors.len(), 2);
        assert_eq!(view.inheritors[0].actor_id, vec![1u8; 32]);
        assert_eq!(view.inheritors[0].label, "Alice");
        // Unresolved label degrades to the short id — never drops the row.
        assert!(!view.inheritors[1].label.is_empty());
    }

    /// The empty-roster arm — `can_confirm: false` with a reason, never a
    /// live confirm beside zero rows (the ordering rule this surface exists
    /// to uphold).
    #[cfg(feature = "value-format")]
    #[test]
    fn seed_rotation_confirm_view_withholds_confirm_on_an_empty_roster() {
        let roster = AdminAdminsListReply::default();
        let view: FfiSeedRotationConfirmView =
            fauna_client_admin::seed_rotation_confirm_view(&roster, &[]).into();

        assert!(!view.can_confirm);
        assert!(view.blocked_reason.is_some());
        assert!(view.inheritors.is_empty());
    }

    /// A capless row (`retained_bytes_cap: 0`) must cross the FFI boundary
    /// unchanged — the leg, not this mirror, decides to render *Default*.
    /// Pins the field-for-field mapping so a new `AdminHostingRowView` field
    /// is a compile error here (mirror-drift guard).
    #[cfg(feature = "custody")]
    #[test]
    fn admin_hosting_row_mirrors_every_field() {
        use fauna_client_capabilities::view_model::{AdminHostingRowView, ReceiptState};

        let view = AdminHostingRowView {
            host_actor_id: "aa".repeat(32),
            owner_actor_id: "bb".repeat(32),
            owner_nest_url: "https://owner.example".to_string(),
            grant_id: b"g1".to_vec(),
            retained_bytes_cap: 0,
            held_bytes: 4096,
            stopped: true,
            receipt_state: ReceiptState::Stale,
        };
        let mirrored: FfiAdminHostingRow = view.clone().into();

        assert_eq!(mirrored.host_actor_id, view.host_actor_id);
        assert_eq!(mirrored.owner_actor_id, view.owner_actor_id);
        assert_eq!(mirrored.owner_nest_url, view.owner_nest_url);
        assert_eq!(mirrored.grant_id, view.grant_id);
        assert_eq!(mirrored.retained_bytes_cap, 0);
        assert_eq!(mirrored.held_bytes, view.held_bytes as i64);
        assert!(mirrored.stopped);
        assert_eq!(mirrored.receipt_state, FfiReceiptState::Stale);
    }

    /// The three receipt states map one-to-one — no collapsing, matching the
    /// three-word honesty rule every custody-facing surface holds to.
    #[cfg(feature = "custody")]
    #[test]
    fn every_receipt_state_maps_to_its_own_variant() {
        use fauna_client_capabilities::view_model::ReceiptState;

        assert_eq!(
            FfiReceiptState::from(ReceiptState::Fresh),
            FfiReceiptState::Fresh
        );
        assert_eq!(
            FfiReceiptState::from(ReceiptState::Stale),
            FfiReceiptState::Stale
        );
        assert_eq!(
            FfiReceiptState::from(ReceiptState::NoReceiptYet),
            FfiReceiptState::NoReceiptYet
        );
    }

    /// `store_dropped` must survive the mirror — it is the only signal
    /// distinguishing "this row was reclaimed" from "and it was the last one
    /// for this owner".
    #[cfg(feature = "custody")]
    #[test]
    fn admin_hosting_remove_reply_mirrors_store_dropped() {
        let reply = fauna_protocol::custody::AdminHostingRemoveReply {
            removed: true,
            store_dropped: true,
            extra: Default::default(),
        };
        let mirrored: FfiAdminHostingRemoveReply = reply.into();
        assert!(mirrored.removed);
        assert!(mirrored.store_dropped);
    }

    /// `can_withdraw` must survive the mirror unchanged — it is the field the
    /// undeclared-vs-declared distinction hinges on, and a mistranslation here
    /// would either hide `admin-nest-region-withdraw-button` on a real
    /// declaration or show it with nothing to withdraw.
    #[cfg(feature = "value-format")]
    #[test]
    fn admin_region_view_mirrors_can_withdraw() {
        let view = fauna_client_admin::admin_region_view(&Default::default());
        assert!(
            !view.can_withdraw,
            "the undeclared state offers no withdraw"
        );
        let mirrored: FfiAdminRegionView = view.into();
        assert!(!mirrored.can_withdraw);
        assert!(mirrored.declared.is_none());
    }

    /// The parser's own case-sensitivity contract, exercised through the FFI
    /// face: two spellings of one region must not both validate, or the
    /// nest's declared-region key becomes ambiguous (the reason the shared
    /// parser exists at all — see its own doc comment).
    #[cfg(feature = "value-format")]
    #[test]
    fn admin_parse_region_code_rejects_lower_case() {
        assert!(admin_parse_region_code("no".to_string()).is_err());
        assert_eq!(admin_parse_region_code("NO".to_string()).unwrap(), "NO");
    }
}
