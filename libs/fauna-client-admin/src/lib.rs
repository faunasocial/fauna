//! Typed-call wrapper for the Layer-5 **Admin** WS-RPC kinds — the admin
//! surface (`fauna.admin.*`) clients hit from the admin shell. Admin is a
//! Fauna app (product invariant: nest configuration is set from clients,
//! not CLI/HTTP), so the admin tools speak the same WS-RPC transport as every
//! other client, and this is the shared home for that call composition —
//! replacing the per-app `/admin/api/*` HTTP twins (no-http-ws-rpc-
//! everywhere directive).
//!
//! Pattern: the same shape as [`fauna_client_bridges::BridgesClient`] /
//! [`fauna_client_bridges::MailAdminClient`] — a thin
//! `pub struct AdminClient<R: RpcRequester>`, one async method per kind, no
//! state machine. The kind-composition logic is written once here and shared
//! across native + wasm (priority #2): native call sites pass `Arc<NestClient>`
//! (which `impl`s `RpcRequester`), the wasm SPA its `WsRpcClient`. Errors
//! propagate as the transport's `R::Error`.
//!
//! All kinds are Admin-gated nest-side (`bridge_method_allowlist::is_permitted`
//! checks the `Admin` caller class); a non-admin caller gets a permission-
//! denied `RpcError`. End-to-end round-trip conformance against the real router
//! lives in `bins/fauna-nest/tests/` (the `conformance_admin*` suites); this
//! crate stays transport-free so it builds on every target.
//!
//! **Surface covered today:** the user-administration cluster (C1 users +
//! evictions), the C2 tiers / invite-codes / invite-requests groups, and the
//! services toggle — i.e. everything the `admin-users`, `admin-settings`
//! (tiers), and `admin-services` pages need (`admin.md`). The remaining admin
//! clusters — C3 stats / audit / ops, C5 wireguard / folders, C4 pairings,
//! and `admins.*` — are not yet wrapped here; add them when those pages migrate
//! off their HTTP twins (the protocol payloads already exist in
//! `fauna_protocol::admin`).

use fauna_core::localized::LocalizedText;
use fauna_protocol::ByteBuf;
use fauna_protocol::RpcRequester;
use fauna_protocol::admin::{
    AdminAdminAddRequest, AdminAdminRemoveRequest, AdminAdminsListReply, AdminAdminsListRequest,
    AdminEvictionsListReply, AdminEvictionsListRequest, AdminInviteCodeCreateReply,
    AdminInviteCodeCreateRequest, AdminInviteCodeDeleteRequest, AdminInviteCodesListReply,
    AdminInviteCodesListRequest, AdminInviteRequestApproveReply, AdminInviteRequestApproveRequest,
    AdminInviteRequestDenyRequest, AdminInviteRequestsListReply, AdminInviteRequestsListRequest,
    AdminLogsReply, AdminLogsRequest, AdminMembershipTierClearRequest,
    AdminMembershipTierSetRequest, AdminMembershipTiersListReply, AdminMembershipTiersListRequest,
    AdminOkReply, AdminPendingActionReply, AdminServiceUpdateRequest, AdminServicesListReply,
    AdminServicesListRequest, AdminStatsReply, AdminStatsRequest, AdminStatusReply,
    AdminStatusRequest, AdminTierCreateRequest, AdminTierUpdateRequest, AdminTiersListReply,
    AdminTiersListRequest, AdminUserCancelEvictionRequest, AdminUserClearHandleRequest,
    AdminUserCreateRequest, AdminUserDeleteRequest, AdminUserEvictRequest, AdminUserGetReply,
    AdminUserGetRequest, AdminUserSuspendRequest, AdminUserUpdateRequest, AdminUsersListReply,
    AdminUsersListRequest,
};
// Node-policy singletons (the admin's deployment-wide choices) live in a sibling
// protocol module; the kinds are still `fauna.admin.set_*` (Admin-class).
use fauna_protocol::age::AgeBand;
use fauna_protocol::node_policy::{
    SetAgeVerificationRequiredReply, SetAgeVerificationRequiredRequest, SetRegistrationModeReply,
    SetRegistrationModeRequest, SetServingPortReply, SetServingPortRequest,
};
// The declared region rides its own protocol module (it serves two planes —
// `region-blocking.md` owns the shared plumbing), but the kinds are
// `fauna.admin.region.{get,set}`, Admin-class, so the call surface belongs here.
use fauna_protocol::region::{
    AdminRegionSetReply, AdminRegionSetRequest, AdminRegionStatusReply, AdminRegionStatusRequest,
};
// The web-app origin's types and derivation live in their own protocol module
// (the nest's `/app` router serves from the same fold), but the kinds are
// `fauna.admin.web_app_origin.{get,set}`, Admin-class, so the call surface and
// the one render fold belong here.
use fauna_protocol::web_app_origin::AdminWebAppOriginGetReply;
pub use fauna_protocol::web_app_origin::{
    AdminWebAppOriginGetReply as WebAppOriginStatus, WebAppOrigin,
};
use fauna_protocol::web_app_origin::{
    AdminWebAppOriginGetRequest, AdminWebAppOriginSetRequest, CENTRAL_APP_ORIGIN,
};
// The nest-held OAuth issuer key set is deployment crypto material whose
// controls `authorization-server.md` § The issuer places "beside
// `rotate_srs_secret` and `force_rotate_dkim` in the admin shell" — the
// `fauna.oauth.*` kinds are Admin-class, so their call surface is this crate's
// too, not a bridge's.
use fauna_protocol::oauth_issuer::{
    ForceRotateIssuerKeyReply, ForceRotateIssuerKeyRequest, ForceRotateSessionSecretReply,
    ForceRotateSessionSecretRequest, IssuerKeyStatusReply, IssuerKeyStatusRequest,
    RotateIssuerKeyReply, RotateIssuerKeyRequest,
};

// Re-exported so a client reads/builds the admin wire shapes through this
// wrapper crate's typed surface without depending on `fauna-protocol` directly
// (mirrors `fauna-client-bridges`).
pub use fauna_protocol::admin;
pub use fauna_protocol::admin::{
    AdminEviction, AdminInviteCode, AdminInviteRequest, AdminLogEntry, AdminLogLevel,
    AdminServiceFlags, AdminServiceUpdateReply, AdminTier, AdminUser,
};
// The registration posture is an enum, not a string, everywhere above the wire:
// re-exported so a client picks a variant rather than spelling `"invite_required"`
// itself (the wire form is derived once, in `set_registration_mode` below).
pub use fauna_protocol::node_policy::RegistrationMode;
// Same reason for the region: a client names a `RegionCode` (parsed, never
// case-folded) rather than spelling a bare string onto the wire.
pub use fauna_core::region_authority::RegionCode;
pub use fauna_protocol::region::{AdminRegionStatusReply as RegionStatus, RegionDocumentRef};
// The issuer key set's wire shapes, re-exported for the same reason as `admin`
// above: an app reads the status reply and the two rotation replies through
// this crate's typed surface.
pub use fauna_protocol::oauth_issuer::{
    ForceRotateIssuerKeyReply as IssuerKeyForcedRotation,
    ForceRotateSessionSecretReply as SessionSecretForcedRotation, IssuerKeyEntry,
    IssuerKeyStatusReply as IssuerKeyStatus, RotateIssuerKeyReply as IssuerKeyRotation,
};

/// Typed `fauna.admin.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`). One async method per nest kind; success-only `{ ok }`
/// replies are discarded (failures surface as the namespaced `RpcError`), the
/// richer replies are returned.
pub struct AdminClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> AdminClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    // ── C1 — user management ─────────────────────────────────────────────────

    /// `fauna.admin.users.list` — a page of users plus the unpaginated total.
    /// `limit` `None` ⇒ the nest default (50, clamped `1..=500`); `offset` `>=0`.
    pub async fn users_list(
        &self,
        limit: Option<i64>,
        offset: i64,
    ) -> Result<AdminUsersListReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.users.list",
                AdminUsersListRequest {
                    limit,
                    offset,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.admin.users.get` — one user by raw 32-byte actor id. A missing
    /// user is `fauna.admin.not_found`.
    pub async fn users_get(&self, actor_id: Vec<u8>) -> Result<AdminUserGetReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.users.get",
                AdminUserGetRequest {
                    actor_id: ByteBuf::from(actor_id),
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.admin.users.create` — admit a known actor id directly, the third
    /// of the three account-creation paths (`public-mode.md` § Registration &
    /// Identity). This is the `admin-users` Admit section's one call.
    ///
    /// `handle` is the handle the actor is admitted under — the admit moment
    /// is where an admin names it, because there is no set-later (admins can
    /// only *clear* a handle). `None` admits a handle-less account: a
    /// deliberate state that cannot send deployment-domain mail
    /// (`public-mode.md` § A handle-less account); the UI passes it for a
    /// deliberately blank field, never as a fallback. The `{ ok: true }`
    /// reply is discarded; a duplicate actor or taken handle is
    /// `fauna.admin.conflict`, a malformed/reserved handle
    /// `fauna.admin.invalid_params`.
    pub async fn users_create(
        &self,
        actor_id: Vec<u8>,
        tier: impl Into<String>,
        label: impl Into<String>,
        handle: Option<String>,
    ) -> Result<(), R::Error> {
        let _: AdminOkReply = self
            .nest
            .request(
                "fauna.admin.users.create",
                AdminUserCreateRequest {
                    actor_id: ByteBuf::from(actor_id),
                    tier: tier.into(),
                    label: label.into(),
                    handle,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.admin.users.update` — set a user's `tier` (= the quota) + `label`.
    /// This is the change-tier control on the `admin-users` Users section. The
    /// `{ ok: true }` reply is discarded; a missing user is `fauna.admin.not_found`.
    pub async fn users_update(
        &self,
        actor_id: Vec<u8>,
        tier: impl Into<String>,
        label: impl Into<String>,
    ) -> Result<(), R::Error> {
        let _: AdminOkReply = self
            .nest
            .request(
                "fauna.admin.users.update",
                AdminUserUpdateRequest {
                    actor_id: ByteBuf::from(actor_id),
                    tier: tier.into(),
                    label: label.into(),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.admin.users.evict` — start the eviction timeline (warn → suspend →
    /// delete). `category` ∈ {terms,capacity,legal,abuse,other}, `reason`
    /// non-empty (else `fauna.admin.invalid_params`); a missing or already-
    /// evicting user is `fauna.admin.conflict`. The `{ ok: true }` reply is
    /// discarded.
    pub async fn users_evict(
        &self,
        actor_id: Vec<u8>,
        reason: impl Into<String>,
        category: impl Into<String>,
    ) -> Result<(), R::Error> {
        let _: AdminOkReply = self
            .nest
            .request(
                "fauna.admin.users.evict",
                AdminUserEvictRequest {
                    actor_id: ByteBuf::from(actor_id),
                    reason: reason.into(),
                    category: category.into(),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.admin.users.cancel_eviction` — cancel an in-flight eviction. No
    /// active eviction is `fauna.admin.not_found`. The `{ ok: true }` reply is
    /// discarded.
    pub async fn users_cancel_eviction(&self, actor_id: Vec<u8>) -> Result<(), R::Error> {
        let _: AdminOkReply = self
            .nest
            .request(
                "fauna.admin.users.cancel_eviction",
                AdminUserCancelEvictionRequest {
                    actor_id: ByteBuf::from(actor_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.admin.users.suspend` — suspend a user **immediately**: cut them
    /// off now, with no delete timeline. Reverse it with
    /// [`Self::users_cancel_eviction`] (suspension is the eviction machine's
    /// `suspended` state, so the restore path is the one the admin UI already
    /// has — `admin.md` § 2 Users → Suspension).
    ///
    /// A missing user is `fauna.admin.not_found`; an **admin** target is
    /// `fauna.admin.conflict` — remove the admin role first, else a suspended
    /// sole admin could never be restored. Empty `reason` / `category` default
    /// to `"suspended by admin"` / `"other"`.
    pub async fn users_suspend(
        &self,
        actor_id: Vec<u8>,
        reason: String,
        category: String,
    ) -> Result<AdminOkReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.users.suspend",
                AdminUserSuspendRequest {
                    actor_id: ByteBuf::from(actor_id),
                    reason,
                    category,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.admin.users.delete` — schedule a user-deletion pending action;
    /// returns the queued-action summary. A missing user is
    /// `fauna.admin.not_found`.
    pub async fn users_delete(
        &self,
        actor_id: Vec<u8>,
    ) -> Result<AdminPendingActionReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.users.delete",
                AdminUserDeleteRequest {
                    actor_id: ByteBuf::from(actor_id),
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.admin.users.clear_handle` — force-release a user's handle (clear
    /// only; admins cannot reassign — a handle change needs user consent).
    /// Pushes `AccountUpdated`. The `{ ok: true }` reply is discarded.
    pub async fn users_clear_handle(&self, actor_id: Vec<u8>) -> Result<(), R::Error> {
        let _: AdminOkReply = self
            .nest
            .request(
                "fauna.admin.users.clear_handle",
                AdminUserClearHandleRequest {
                    actor_id: ByteBuf::from(actor_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.admin.evictions.list` — every user with an in-flight eviction
    /// (each entry's `eviction` is always `Some`). Replay-safe pure read.
    pub async fn evictions_list(&self) -> Result<AdminEvictionsListReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.evictions.list",
                AdminEvictionsListRequest::default(),
            )
            .await
    }

    // ── C2 — tiers ───────────────────────────────────────────────────────────

    /// `fauna.admin.tiers.list` — every defined tier (the quota caps each tier
    /// grants). The `admin-users` tier pickers read this for their options;
    /// `admin-settings` renders the definitions. Replay-safe pure read.
    pub async fn tiers_list(&self) -> Result<AdminTiersListReply, R::Error> {
        self.nest
            .request("fauna.admin.tiers.list", AdminTiersListRequest::default())
            .await
    }

    /// `fauna.admin.tiers.create` — define a new tier. An empty `name` is
    /// `fauna.admin.invalid_params`; a duplicate is `fauna.admin.conflict`. The
    /// `{ ok: true }` reply is discarded.
    pub async fn tiers_create(&self, tier: AdminTierCreateRequest) -> Result<(), R::Error> {
        let _: AdminOkReply = self.nest.request("fauna.admin.tiers.create", tier).await?;
        Ok(())
    }

    /// `fauna.admin.tiers.update` — overwrite a tier's caps. A missing tier is
    /// `fauna.admin.not_found`. The `{ ok: true }` reply is discarded.
    pub async fn tiers_update(&self, tier: AdminTierUpdateRequest) -> Result<(), R::Error> {
        let _: AdminOkReply = self.nest.request("fauna.admin.tiers.update", tier).await?;
        Ok(())
    }

    // ── C2 — membership tiers (monetization.md § Pillar 4) ───────────────────

    /// `fauna.admin.membership_tiers.list` — every membership designation the
    /// calling admin owns: which of *their own* subscription tiers mean paid
    /// access to this nest, and the quota tiers an admitted / lapsed member runs
    /// under. An empty list is the out-of-the-box state (nothing is monetized).
    /// Replay-safe pure read.
    pub async fn membership_tiers_list(&self) -> Result<AdminMembershipTiersListReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.membership_tiers.list",
                AdminMembershipTiersListRequest::default(),
            )
            .await
    }

    /// `fauna.admin.membership_tiers.set` — designate one of the caller's own
    /// subscription tiers as a membership tier, or re-point an existing
    /// designation (an upsert, so this is idempotent). A `tier_name` the caller
    /// does not own is `fauna.admin.not_found`; an `admin_tier` / `lapse_tier`
    /// naming no known quota tier is `fauna.admin.invalid_params`. Omit
    /// `lapse_tier` for [`fauna_protocol::admin::DEFAULT_LAPSE_TIER`]. The
    /// `{ ok: true }` reply is discarded.
    pub async fn membership_tiers_set(
        &self,
        designation: AdminMembershipTierSetRequest,
    ) -> Result<(), R::Error> {
        let _: AdminOkReply = self
            .nest
            .request("fauna.admin.membership_tiers.set", designation)
            .await?;
        Ok(())
    }

    /// `fauna.admin.membership_tiers.clear` — drop a designation, leaving the
    /// subscription tier itself untouched (it reverts to an ordinary content
    /// tier). A tier carrying no designation is `fauna.admin.not_found`. The
    /// `{ ok: true }` reply is discarded.
    pub async fn membership_tiers_clear(&self, tier_name: String) -> Result<(), R::Error> {
        let _: AdminOkReply = self
            .nest
            .request(
                "fauna.admin.membership_tiers.clear",
                AdminMembershipTierClearRequest {
                    tier_name,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    // ── C2 — invite codes ──────────────────────────────────────────────────────

    /// `fauna.admin.invite_codes.list` — every closed-registration invite code.
    /// Replay-safe pure read.
    pub async fn invite_codes_list(&self) -> Result<AdminInviteCodesListReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.invite_codes.list",
                AdminInviteCodesListRequest::default(),
            )
            .await
    }

    /// `fauna.admin.invite_codes.create` — mint (or register) a code at `tier`
    /// and `uses`. **Pass an empty `code` to have the nest mint a random token**
    /// (the admit-a-user flow on the `admin-users` Invite section): the reply
    /// carries the resulting `code` for the UI to surface/copy. A supplied
    /// non-empty `code` is used verbatim (vanity/migration); a duplicate is
    /// `fauna.admin.conflict`. `guardian_actor` links the redeemed account to a
    /// guardian for supervised admission (`family-safety.md` § Wire & data
    /// shape); `None` mints an ordinary code. `age_band` is the admitting
    /// guardian's band dial for that supervised admission
    /// (`admin-users-invite-age-band-select`; `family-safety.md` § The account
    /// age band, D2) — typed here so no client spells a wire token; the nest
    /// refuses a band without a guardian, and the UI gates the same way.
    pub async fn invite_codes_create(
        &self,
        code: impl Into<String>,
        tier: impl Into<String>,
        uses: i64,
        guardian_actor: Option<Vec<u8>>,
        age_band: Option<AgeBand>,
    ) -> Result<AdminInviteCodeCreateReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.invite_codes.create",
                AdminInviteCodeCreateRequest {
                    code: code.into(),
                    tier: tier.into(),
                    uses,
                    guardian_actor: guardian_actor.map(ByteBuf::from),
                    age_band: age_band.map(|b| b.as_str().to_string()),
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.admin.invite_codes.delete` — remove a code. A missing code is
    /// `fauna.admin.not_found`. The `{ ok: true }` reply is discarded.
    pub async fn invite_codes_delete(&self, code: impl Into<String>) -> Result<(), R::Error> {
        let _: AdminOkReply = self
            .nest
            .request(
                "fauna.admin.invite_codes.delete",
                AdminInviteCodeDeleteRequest {
                    code: code.into(),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    // ── C2 — invite requests ───────────────────────────────────────────────────

    /// `fauna.admin.invite_requests.list` — every invite request (pending +
    /// decided), the `admin-users` Pending requests section. Replay-safe read.
    pub async fn invite_requests_list(&self) -> Result<AdminInviteRequestsListReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.invite_requests.list",
                AdminInviteRequestsListRequest::default(),
            )
            .await
    }

    /// `fauna.admin.invite_requests.approve` — admit the requester, creating the
    /// account at `tier` (`None` ⇒ the nest default `"free"`) with an optional
    /// `label`, and deleting the request. Returns the resolved
    /// `{ actor_id, handle, tier }`. A missing request is `fauna.admin.not_found`;
    /// a non-pending request / re-taken handle / duplicate actor is
    /// `fauna.admin.conflict`. The requester's onboarding poll then advances
    /// (cross-page contract, `admin.md` § Architectural rules 5).
    /// `guardian_actor` links the admitted account to a guardian for supervised
    /// admission (`family-safety.md` § Wire & data shape); `None` admits an
    /// ordinary account. `age_band` is the band the admitting guardian admits
    /// at (`invite-request-row-age-band-select` — defaulting to the
    /// applicant's claim, the adult's judgment overriding it silently;
    /// `family-safety.md` § The account age band, D5); the nest refuses a band
    /// without a guardian.
    pub async fn invite_requests_approve(
        &self,
        id: i64,
        tier: Option<String>,
        label: Option<String>,
        guardian_actor: Option<Vec<u8>>,
        age_band: Option<AgeBand>,
    ) -> Result<AdminInviteRequestApproveReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.invite_requests.approve",
                AdminInviteRequestApproveRequest {
                    id,
                    tier,
                    label,
                    guardian_actor: guardian_actor.map(ByteBuf::from),
                    age_band: age_band.map(|b| b.as_str().to_string()),
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.admin.invite_requests.deny` — mark a request denied with an
    /// optional `reason` (surfaced on the requester's `invite_request_pending`
    /// screen). A missing request is `fauna.admin.not_found`; a non-pending one
    /// is `fauna.admin.conflict`. The `{ ok: true }` reply is discarded.
    pub async fn invite_requests_deny(
        &self,
        id: i64,
        reason: Option<String>,
    ) -> Result<(), R::Error> {
        let _: AdminOkReply = self
            .nest
            .request(
                "fauna.admin.invite_requests.deny",
                AdminInviteRequestDenyRequest {
                    id,
                    reason,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    // ── C5 — services toggle ─────────────────────────────────────────────────

    /// `fauna.admin.services.list` — the sidecar-service enable flags + intent-
    /// file version (the `admin-services` toggles). Replay-safe pure read.
    pub async fn services_list(&self) -> Result<AdminServicesListReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.services.list",
                AdminServicesListRequest::default(),
            )
            .await
    }

    /// `fauna.admin.services.update` — flip one service flag. `name` ∈
    /// {bridge,pairing} (else `fauna.admin.invalid_params` nest-side). Returns
    /// the applied `{ ok, service, enabled }` echo.
    pub async fn services_update(
        &self,
        name: impl Into<String>,
        enabled: bool,
    ) -> Result<AdminServiceUpdateReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.services.update",
                AdminServiceUpdateRequest {
                    name: name.into(),
                    enabled,
                    ..Default::default()
                },
            )
            .await
    }

    // ── C3 — nest stats / runtime status ──────────────────────────────────────

    /// `fauna.admin.stats` — nest-wide counters for the admin dashboard
    /// (`admin.md` § Dashboard): total/suspended users, per-tier breakdown,
    /// aggregate inbox + storage bytes, and the live WebSocket connection count.
    /// Parameterless; replay-safe pure read. The typed twin of the deleted
    /// `GET /admin/api/stats`.
    pub async fn stats(&self) -> Result<AdminStatsReply, R::Error> {
        self.nest
            .request("fauna.admin.stats", AdminStatsRequest::default())
            .await
    }

    /// `fauna.admin.status` — the running nest version + any pending
    /// self-update advisory (`update_available` is `None` when up to date).
    /// Parameterless; replay-safe pure read. The typed twin of the deleted
    /// `GET /admin/api/status`. Live worker/cluster runtime detail lives on the
    /// sibling `fauna.admin.{cluster,worker}.status` kinds, not here.
    pub async fn status(&self) -> Result<AdminStatusReply, R::Error> {
        self.nest
            .request("fauna.admin.status", AdminStatusRequest::default())
            .await
    }

    // ── Node-policy singletons (deployment-wide admin choices) ────────────────

    /// `fauna.admin.set_serving_port` — set the deployment-wide client-facing API
    /// serving port (the nest's own HTTPS listener: the WS-RPC transport + the
    /// served SPA; `nest/common.md` § Serving ports). One of two written members
    /// of the node-policy `set_*` family (with `set_registration_mode` below);
    /// the rest (cors / max-storage) are read back on `fauna.setup.status` but
    /// not yet written from any client. Admin-class; applies on the next nest
    /// (re)start (the nest cannot hot-rebind its own listener), but the chosen
    /// value is persisted to the `serving_port` singleton and read back on
    /// `fauna.setup.status` (`SetupStatusReply.serving_port`) immediately. The
    /// `{ ok: true }` reply is discarded (failures surface as the namespaced
    /// `RpcError`). The symmetric twin of `fauna_client_bridges`'s
    /// `set_caldav_port`, kept here because this is a `fauna.admin.*` kind.
    pub async fn set_serving_port(&self, port: u16) -> Result<(), R::Error> {
        let _: SetServingPortReply = self
            .nest
            .request(
                "fauna.admin.set_serving_port",
                SetServingPortRequest {
                    port,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.admin.set_registration_mode` — set the deployment's registration
    /// posture and, orthogonally, the free-tier ceiling (the `admin-users`
    /// registration section's single Save; `admin.md` § 2 Users → *Section 2 —
    /// Registration*). Admin-class; the nest upserts the `nest_registration_mode`
    /// singleton and swaps the live value with **no restart**, and the admin reads
    /// it back on `fauna.setup.status`
    /// (`SetupStatusReply.{registration_mode,max_free_users}`).
    ///
    /// `mode` is the [`RegistrationMode`] enum, not a string: the wire form is
    /// derived here via `as_wire_str`, so no client spells the vocabulary itself
    /// and no client can send a mode the nest would reject as
    /// `fauna.node_policy.registration_mode_invalid`.
    ///
    /// `max_free_users` is **orthogonal to the mode** — it caps free-tier accounts
    /// regardless of posture (`public-mode.md` § Registration Modes) — and `None`
    /// *clears* the cap rather than leaving it unchanged: the two values are one
    /// admin decision, saved together, so a caller always sends the state it wants
    /// both to have. The cap counts **every** free-tier account including the
    /// admin's own (an admin is a user with an extra role, seeded at `free`), so
    /// "room for one more" is a cap of 2, not 1 — pinned by
    /// `registration_mode_api::the_free_tier_ceiling_is_orthogonal_to_the_mode`.
    ///
    /// The `{ ok: true }` reply is discarded (failures surface as the namespaced
    /// `RpcError`).
    pub async fn set_registration_mode(
        &self,
        mode: RegistrationMode,
        max_free_users: Option<u64>,
    ) -> Result<(), R::Error> {
        let _: SetRegistrationModeReply = self
            .nest
            .request(
                "fauna.admin.set_registration_mode",
                SetRegistrationModeRequest {
                    mode: mode.as_wire_str().to_string(),
                    max_free_users,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.admin.set_age_verification_required` — flip the **"accept only
    /// signups carrying app age verification"** gate (`family-safety.md` § The
    /// account age band, D5+D6; gating scope `public-mode.md` § Registration
    /// Modes → *Age at registration*). Default **off**; read back on
    /// `fauna.setup.status` (`SetupStatusReply.age_verification_required`).
    /// The `admin-users` Registration section's save dispatches this beside
    /// [`Self::set_registration_mode`] **only when the toggle's value changed**
    /// (`admin-users-registration-age-verification-toggle`, one gesture for the
    /// whole section). The `{ ok: true }` reply is discarded.
    pub async fn set_age_verification_required(&self, required: bool) -> Result<(), R::Error> {
        let _: SetAgeVerificationRequiredReply = self
            .nest
            .request(
                "fauna.admin.set_age_verification_required",
                SetAgeVerificationRequiredRequest {
                    required,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.admin.request_host_restart` — the admin's "restart now" affordance
    /// for the host Ubuntu box of an onboarded VPS (`installers/vps.md` § Host OS
    /// Maintenance § 4). Writes a flag the host `fauna-reboot-coordinator` picks
    /// up on its next run and reboots (gracefully, regardless of idle/ceiling).
    /// Admin-class; the `{ ok: true }` reply is discarded (failures surface as the
    /// namespaced `RpcError`, e.g. `fauna.host_maintenance.no_host` on a nest with
    /// no maintenance mount — dev / desktop / bare-metal).
    pub async fn request_host_restart(&self) -> Result<(), R::Error> {
        let _: admin::RequestHostRestartReply = self
            .nest
            .request(
                "fauna.admin.request_host_restart",
                admin::RequestHostRestartRequest::default(),
            )
            .await?;
        Ok(())
    }

    // ── The OAuth issuer key set — deployment crypto, two rotation arms ────────

    /// `fauna.oauth.issuer_key_status` — the key set the issuer serves at
    /// `/oauth/jwks`: every `kid`, which one signs, when a retired one stops
    /// being served (`authorization-server.md` § The issuer). Pure read,
    /// Admin-class; fold it through [`issuer_key_view`] for the rendering
    /// decisions, derived once here rather than seven times.
    pub async fn issuer_key_status(&self) -> Result<IssuerKeyStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.oauth.issuer_key_status",
                IssuerKeyStatusRequest::default(),
            )
            .await
    }

    /// `fauna.oauth.rotate_issuer_key` — the **ordinary** arm: mint a new
    /// signer and retire the outgoing key, which stays served for the
    /// retirement horizon so every token minted before the call still
    /// verifies. Nothing breaks. Admin-class, replay-forbidden nest-side
    /// (each call mints a key).
    pub async fn rotate_issuer_key(&self) -> Result<RotateIssuerKeyReply, R::Error> {
        self.nest
            .request(
                "fauna.oauth.rotate_issuer_key",
                RotateIssuerKeyRequest::default(),
            )
            .await
    }

    /// `fauna.oauth.force_rotate_issuer_key` — the **forced** arm, the
    /// compromise response (`authorization-server.md` § The issuer → *Two
    /// rotation arms*): mint a new signer and drop every other key at once,
    /// horizon skipped, so a leaked `kid` leaves the JWKS on the next read.
    /// **Every honest token signed by a dropped key stops verifying
    /// immediately** — the app control states that cost before dispatch and
    /// arms an inline confirm, the `admin-nest-seed-rotate-*` shape. The
    /// reply names every `kid` dropped. Admin-class, replay-forbidden.
    pub async fn force_rotate_issuer_key(&self) -> Result<ForceRotateIssuerKeyReply, R::Error> {
        self.nest
            .request(
                "fauna.oauth.force_rotate_issuer_key",
                ForceRotateIssuerKeyRequest::default(),
            )
            .await
    }

    /// `fauna.oauth.force_rotate_session_secret` — the forced arm's sibling
    /// for the issuer's **second signer**: re-mint the secret this nest's
    /// OAuth refresh tokens are MACed under, so every outstanding refresh
    /// token dies at once and clients re-authorize. Both signers rest in the
    /// same store, so a compromise response that rotates only the issuer key
    /// is half a response — a forged refresh token would redeem for an access
    /// token signed by the new key. The app control offers it beside the
    /// forced issuer rotation, states the cost (every connected app must
    /// re-consent) before dispatch, and arms the same inline confirm. The
    /// reply dates the generation replaced. Admin-class, replay-forbidden.
    pub async fn force_rotate_session_secret(
        &self,
    ) -> Result<ForceRotateSessionSecretReply, R::Error> {
        self.nest
            .request(
                "fauna.oauth.force_rotate_session_secret",
                ForceRotateSessionSecretRequest::default(),
            )
            .await
    }

    // The three doors `admin-nest-oauth-*` actually drives — read-and-fold, and
    // dispatch-and-word — shared by every app: tui and linux call them
    // in-process, the other five through the UniFFI and wasm faces. The raw
    // calls above stay for callers that need the reply itself.

    /// The key set, read and folded through [`issuer_key_view`] — the one read
    /// `admin-nest-oauth-section` paints from. A failure stays an `Err` for
    /// the caller to put on the section's reason line, never on the page:
    /// the kind is newer than every other admin-nest read, and a newer app on
    /// an older nest must still paint the rest.
    pub async fn issuer_key_status_view(&self) -> Result<IssuerKeyView, R::Error> {
        Ok(issuer_key_view(&self.issuer_key_status().await?))
    }

    /// The ordinary rotation, dispatched and worded: the one sentence
    /// `admin-nest-oauth-status` shows after it. A failure is a verdict too,
    /// never an `Err` — the kind mints on the nest, so a reply lost to a
    /// timeout can follow a committed rotation, and only the shared fold may
    /// say so ([`issuer_key_rotation_verdict`]).
    pub async fn rotate_issuer_key_verdict(&self) -> LocalizedText {
        let reply = self.rotate_issuer_key().await.map_err(|e| e.to_string());
        issuer_key_rotation_verdict(reply.as_ref().map_err(String::as_str))
    }

    /// One forced arm, dispatched and worded: exactly `arm`'s kind — the
    /// armed confirm names the arm it was painted for, so it can never fire
    /// its sibling — and its verdict, failure included, as
    /// [`Self::rotate_issuer_key_verdict`].
    ///
    /// `format_instant` is the caller's clock face for the session-secret
    /// verdict's instant ([`session_secret_forced_rotation_verdict`]): this
    /// crate is wasm-clean and the OS timezone is not, so a native caller
    /// passes `fauna_core::format::format_unix_local` and the web SPA its own.
    pub async fn force_rotate_verdict(
        &self,
        arm: IssuerForcedArm,
        format_instant: impl Fn(i64) -> String,
    ) -> LocalizedText {
        match arm {
            IssuerForcedArm::IssuerKey => {
                let reply = self
                    .force_rotate_issuer_key()
                    .await
                    .map_err(|e| e.to_string());
                issuer_key_forced_rotation_verdict(reply.as_ref().map_err(String::as_str))
            }
            IssuerForcedArm::SessionSecret => {
                let reply = self
                    .force_rotate_session_secret()
                    .await
                    .map_err(|e| e.to_string());
                session_secret_forced_rotation_verdict(
                    reply.as_ref().map_err(String::as_str),
                    format_instant,
                )
            }
        }
    }

    // ── The declared region — the region tier's one human choice ──────────────

    /// `fauna.admin.region.get` — what region this deployment declares as its
    /// legal situs, plus the state of the authority channel that region's policy
    /// arrives on (`region-blocking.md` § The region/authority plumbing →
    /// Region determination; `dynamic-features.md` § The region tier →
    /// Attachment).
    ///
    /// Pure read, Admin-class. Everything a screen needs is on the one reply —
    /// see [`admin_region_view`] for the rendering decisions, which are derived
    /// once here rather than seven times.
    pub async fn region_status(&self) -> Result<AdminRegionStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.region.get",
                AdminRegionStatusRequest::default(),
            )
            .await
    }

    /// `fauna.admin.region.set` — declare, re-declare, or **withdraw** this
    /// deployment's region.
    ///
    /// `None` withdraws: the absent case, not a sentinel, and a whole-value
    /// replace with no third "leave it unchanged" state. Withdrawing (and
    /// re-declaring) **retires the previous region's feature-policy document**
    /// nest-side — a document is one authority's statement about deployments in
    /// its own region and must not survive a change of situs — so a caller
    /// surfacing this as a mere toggle would understate it.
    ///
    /// A region with no enrolled authority is **accepted and inert**, never
    /// refused: a legal situs is a fact about the deployment, and most of the
    /// world has no Fauna-enrolled authority. Admin-class; the `{ ok: true }`
    /// reply is discarded (failures surface as the namespaced `RpcError`).
    pub async fn set_region(&self, region: Option<RegionCode>) -> Result<(), R::Error> {
        let _: AdminRegionSetReply = self
            .nest
            .request(
                "fauna.admin.region.set",
                AdminRegionSetRequest {
                    region,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    // ── The web-app origin — what this nest's own `/app/` answers ─────────────

    /// `fauna.admin.web_app_origin.get` — the chosen mode and what `/app/`
    /// answers because of it (`web-content-hosting.md` § Same-origin security
    /// model → *The nest-served `/app/` and the central origin*).
    ///
    /// `Ok(None)` is a nest that **predates the choice**: it answers the kind
    /// with `fauna.protocol.unknown_kind`, the additive-evolution signal, and
    /// [`admin_web_app_origin_view`] words that as "cannot be set here" rather
    /// than as a failure. Every other refusal stays an error. (Not derived from
    /// `fauna.setup.status`: its `web_app_origin` field is required, so an older
    /// nest's reply carries no "absent" to read.)
    pub async fn web_app_origin(&self) -> Result<Option<AdminWebAppOriginGetReply>, R::Error>
    where
        R::Error: fauna_protocol::RpcErrorClass,
    {
        match self
            .nest
            .request(
                "fauna.admin.web_app_origin.get",
                AdminWebAppOriginGetRequest::default(),
            )
            .await
        {
            Ok(reply) => Ok(Some(reply)),
            Err(e) if is_unknown_kind(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `fauna.admin.web_app_origin.set` — a whole-value replace of the choice.
    /// The reply is the same projection the read answers, so a save renders its
    /// status with no second read. Admin-class, `OnlineOnly` nest-side.
    pub async fn set_web_app_origin(
        &self,
        mode: WebAppOrigin,
    ) -> Result<AdminWebAppOriginGetReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.web_app_origin.set",
                AdminWebAppOriginSetRequest {
                    mode,
                    extra: Default::default(),
                },
            )
            .await
    }

    // ── Admins (the roster) ──────────────────────────────────────────────────

    /// `fauna.admin.admins.list` — every actor holding the admin role
    /// (`admin.md` § Admin continuity and succession → *the roster surface*).
    ///
    /// **Authoritative and unpaginated**, which is why the roster is read here
    /// rather than filtered out of a page of [`Self::users_list`] on
    /// `AdminUser.is_admin`: that flag answers *"is this row an admin"* for the
    /// rows a page happens to contain, so a roster member past the page window
    /// is silently missing. Under-listing is the failure that matters wherever
    /// this read is load-bearing — the deployment-seed rotation confirm names
    /// "the set that will inherit" (`box-recovery.md` § Deployment-seed
    /// rotation), and an admin who rotates believing two people inherit when
    /// three do has been told the opposite of the truth.
    ///
    /// The entries carry no handle (`AdminAdminEntry` is `{actor_id, added_at}`);
    /// a surface that wants names joins them itself and folds the result through
    /// [`seed_rotation_confirm_view`] — or, for the deployment-seed rotation
    /// confirm specifically, calls [`Self::seed_rotate_roster_view`] directly.
    /// Replay-safe pure read.
    pub async fn admins_list(&self) -> Result<AdminAdminsListReply, R::Error> {
        self.nest
            .request("fauna.admin.admins.list", AdminAdminsListRequest::default())
            .await
    }

    /// `fauna.admin.admins.list` + a per-admin `fauna.admin.users.get` label
    /// join, folded through [`seed_rotation_confirm_view`] — the deployment-
    /// seed rotation confirm's roster (`box-recovery.md` § Deployment-seed
    /// rotation). Lifted here (priority #2) after the list→loop→fold
    /// orchestration was found hand-rolled identically on the fauna-ffi and
    /// fauna-wasm faces (a two-face-test finding, 2026-08-25).
    ///
    /// A per-admin `users_get` failure is swallowed — best-effort label
    /// resolution, per [`seed_rotation_confirm_view`]'s own contract: an
    /// admin whose label can't be read still inherits and still appears in
    /// the confirm view, degraded to their short id rather than dropped.
    pub async fn seed_rotate_roster_view(&self) -> Result<SeedRotationConfirmView, R::Error> {
        let roster = self.admins_list().await?;
        let mut labels = Vec::with_capacity(roster.admins.len());
        for entry in &roster.admins {
            let actor_id = entry.actor_id.to_vec();
            if let Ok(reply) = self.users_get(actor_id.clone()).await {
                labels.push((actor_id, reply.user.label));
            }
        }
        Ok(seed_rotation_confirm_view(&roster, &labels))
    }

    /// `fauna.admin.admins.add` — grant the admin role, the roster surface's
    /// `admin-users-make-admin-button` (`admin.md` § Admin continuity and
    /// succession, instrument 1). Schedules an `AdminAdd` pending action (the
    /// grant applies after the delay window, matching the destructive lifecycle
    /// controls beside it); returns the queued-action summary. A wrong-length
    /// actor id is `fauna.admin.invalid_params`.
    pub async fn admins_add(&self, actor_id: Vec<u8>) -> Result<AdminPendingActionReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.admins.add",
                AdminAdminAddRequest {
                    actor_id: ByteBuf::from(actor_id),
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.admin.admins.remove` — revoke the admin role, the roster surface's
    /// `admin-users-remove-admin-button` (`admin.md` § Admin continuity and
    /// succession, instrument 1). Schedules an `AdminRemove` pending action;
    /// refuses (`fauna.admin.conflict`) whenever it would leave zero superadmins
    /// — a suspended *sole* admin could never be restored by anyone, so the role
    /// must be removed from someone else first.
    pub async fn admins_remove(
        &self,
        actor_id: Vec<u8>,
    ) -> Result<AdminPendingActionReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.admins.remove",
                AdminAdminRemoveRequest {
                    actor_id: ByteBuf::from(actor_id),
                    ..Default::default()
                },
            )
            .await
    }

    // ── Pending admin actions (admin.md § Pending admin actions) ──────────────

    /// `fauna.admin.pending_actions.list` — every account's pending actions
    /// (all statuses; the console filters to `pending`), the read behind the
    /// admin Users hub's `admin-users-pending-section`. Replay-safe pure read.
    pub async fn pending_actions_list(
        &self,
    ) -> Result<fauna_protocol::admin::AdminPendingActionsListReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.pending_actions.list",
                fauna_protocol::admin::AdminPendingActionsListRequest::default(),
            )
            .await
    }

    /// `fauna.pending_actions.approve` — add this admin's approval to a
    /// quorum-gated action (`admin-pending-action-approve-button`). The nest
    /// refuses self-approval and a row no longer pending
    /// (`fauna.pending_actions.permission_denied`); a repeat approval is
    /// idempotent.
    pub async fn pending_action_approve(&self, id: i64) -> Result<(), R::Error> {
        let _: fauna_protocol::pending_actions::PendingActionApproveReply = self
            .nest
            .request(
                "fauna.pending_actions.approve",
                fauna_protocol::pending_actions::PendingActionApproveRequest {
                    id,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.pending_actions.cancel` from the admin console
    /// (`admin-pending-action-cancel-button`) — the same kind
    /// `fauna_client_account::AccountClient::pending_action_cancel` drives
    /// from Settings; here the caller is an admin calling off an admin action
    /// the cancel matrix admits any admin to.
    pub async fn pending_action_cancel(&self, id: i64) -> Result<(), R::Error> {
        let _: fauna_protocol::pending_actions::PendingActionCancelReply = self
            .nest
            .request(
                "fauna.pending_actions.cancel",
                fauna_protocol::pending_actions::PendingActionCancelRequest {
                    id,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    // ── C6 — observability / logs ─────────────────────────────────────────────

    /// `fauna.admin.logs` — the nest's in-memory `fauna-log` ring snapshot for
    /// the admin Logs view (`observability.md` § Surfaces), oldest-first. The
    /// admin surface renders these with the **same widget** as the client's own
    /// Settings → Logs page and filters by severity client-side. Replay-safe
    /// pure read.
    pub async fn logs(&self) -> Result<AdminLogsReply, R::Error> {
        self.nest
            .request("fauna.admin.logs", AdminLogsRequest::default())
            .await
    }

    // ── Factory reset ────────────────────────────────────────────────────────

    /// `fauna.admin.factory_reset` — return the nest to fresh / unclaimed via a
    /// restart-wipe and learn the post-reset claim code. The handler stages a
    /// marker, replies with [`FactoryResetReply`], then exits so the supervisor
    /// restarts into a pre-claim wipe (`behavior/mail-bridge-lifecycle.md` §
    /// Factory reset). `new_claim_code` optionally pins the post-reset code;
    /// `None` ⇒ the nest generates a random one, returned in the reply.
    ///
    /// The reply lands before the process exits (the handler stages the code
    /// first), so the caller never reads `/data` off the box — it drives the
    /// re-onboard with the returned code. The WS connection drops as the nest
    /// restarts; the caller resumes onboarding once it's back unclaimed.
    pub async fn factory_reset(
        &self,
        new_claim_code: Option<String>,
    ) -> Result<admin::FactoryResetReply, R::Error> {
        self.nest
            .request(
                "fauna.admin.factory_reset",
                admin::FactoryResetRequest {
                    new_claim_code,
                    ..Default::default()
                },
            )
            .await
    }

    // ── Abuse reports (the `admin-nest` reports section) ─────────────────────

    /// `fauna.moderation.abuse_report.queue` — the open user reports on this
    /// nest, local and forwarded (`moderation.md` § User-initiated reporting →
    /// *Where it lands*). A forwarded row carries its origin nest and never a
    /// reporter identity. Admin-class.
    pub async fn abuse_report_queue(&self) -> Result<moderation::AbuseReportQueueReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.abuse_report.queue",
                moderation::AbuseReportQueueRequest::default(),
            )
            .await
    }

    /// `fauna.moderation.abuse_report.resolve` — record an open report's
    /// outcome. A **record, not an action**: nothing happens to the content or
    /// the author; the reporter is told the outcome only (or, for a forwarded
    /// report, the origin nest is). Admin-class.
    pub async fn abuse_report_resolve(
        &self,
        report_id: impl Into<String>,
        outcome: moderation::AbuseReportOutcome,
    ) -> Result<moderation::AbuseReportResolveReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.abuse_report.resolve",
                moderation::AbuseReportResolveRequest {
                    report_id: report_id.into(),
                    outcome,
                    extra: Default::default(),
                },
            )
            .await
    }
}

// The abuse-report queue's wire shapes, re-exported so an admin surface reads
// them through this crate without depending on `fauna-protocol` directly.
pub use fauna_protocol::moderation;

/// The eviction-machine state an `admin-users` row is in, as the *client* needs
/// to read it. The wire carries this as `AdminUser.eviction.status`, a `String`
/// (forward-compat: a nest may add states); anything unrecognized is treated as
/// [`Self::EvictionPending`] — some cut-off is in flight, so the row offers the
/// restore control and withholds the two entry controls. That is the safe
/// direction: it never *invents* an entry point a newer nest might refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminUserLifecycle {
    /// No cut-off in flight: `eviction` is absent.
    Active,
    /// The timed eviction ladder is running but has not yet cut the user off
    /// (`status = "warning"`). The user still has full access — this is the
    /// export window.
    EvictionWarning,
    /// The user is cut off (`status = "suspended"`), whether they got there by
    /// the timed ladder or by an immediate suspend.
    Suspended,
    /// The timed ladder has reached deletion (`status = "deleting"`): the
    /// account is being reclaimed and the nest refuses restore. A row stays
    /// here only while the finalize is held — the account still holds the
    /// admin role, or still guards supervised accounts — and it completes
    /// once that is resolved.
    Deleting,
    /// A cut-off is in flight in a state this client does not know by name.
    EvictionPending,
}

/// Which of the three per-row lifecycle controls an `admin-users` row offers.
///
/// One source of truth for all seven apps (priority #1/#3): the rule is not
/// obvious — it is three eviction states crossed with the admin-role guard — and
/// getting it wrong either strands a user (no restore control) or renders a
/// control the nest always refuses.
///
/// `Serialize` is what the web wasm face (`adminUserRowControls`) hands across
/// the boundary; the natives go through the `uniffi::Record` mirror in
/// `fauna-ffi`, which can't derive off a foreign type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct AdminUserRowControls {
    /// `admin-users-suspend-button` — cut the user off *now*, no delete timeline.
    pub suspend: bool,
    /// `admin-users-evict-button` — start the timed warn → suspend → delete ladder.
    pub evict: bool,
    /// `admin-users-cancel-eviction-button` — restore, from *either* entry point.
    pub restore: bool,
    /// `admin-users-make-admin-button` — grant the admin role (`admin.md`
    /// § Admin continuity and succession, instrument 1). Offered on any plain
    /// (non-admin) row, regardless of lifecycle state — granting the role to an
    /// already-suspended user is deliberate (their own restore control still
    /// stands, so this can't strand them).
    pub make_admin: bool,
    /// `admin-users-remove-admin-button` — revoke the admin role. Offered on any
    /// `is_admin` row; the nest, not the client, refuses the last-superadmin
    /// case (`fauna.admin.conflict`, surfaced on `admin-users-action-error`).
    pub remove_admin: bool,
}

/// Read a row's lifecycle state off the wire projection.
pub fn admin_user_lifecycle(user: &AdminUser) -> AdminUserLifecycle {
    match user.eviction.as_ref() {
        None => AdminUserLifecycle::Active,
        Some(e) => match e.status.as_str() {
            "warning" => AdminUserLifecycle::EvictionWarning,
            "suspended" => AdminUserLifecycle::Suspended,
            "deleting" => AdminUserLifecycle::Deleting,
            _ => AdminUserLifecycle::EvictionPending,
        },
    }
}

/// Which lifecycle controls an `admin-users` row offers
/// (`admin.md` § 2 Users → *Cutting a user off — eviction and suspension*).
///
/// The rule, straight from the ratified doc:
///
/// - **An admin can be neither suspended nor evicted** — the nest answers
///   `fauna.admin.conflict`, because a suspended *sole* admin could never be
///   restored by anyone. The admin role is removed first
///   (`fauna.admin.admins.remove`), then the now-plain user is suspended. So an
///   admin row offers **no** entry control. It may still offer *restore*: the
///   role can be granted to someone already suspended, and stranding them
///   would violate `common.md` § Client-state recoverability.
/// - **Suspend is reachable from `warning`, not only from `Active`** — suspending
///   a mid-eviction user promotes them at once *and clears the pending delete*
///   (`eviction_delete_at = NULL`), erring away from deletion per the iron-clad
///   *no user-data loss* invariant. Withholding it there would force the admin to
///   fully *restore* an abuse-in-progress user before they could stop them.
/// - **Suspend is not offered on an already-suspended row** — the nest's
///   `suspend_user_now` matches `eviction_status IN ('', 'warning')`, so it would
///   be a no-op. Restore is that row's only move.
/// - **Evict is offered only from `Active`** — it *starts* the ladder; re-running
///   it on a row already on the ladder is not an entry point.
/// - **Make-admin / remove-admin split purely on `is_admin`**, with no lifecycle
///   crossing: a plain row (any lifecycle state, including suspended) offers
///   make-admin, an admin row offers remove-admin. Unlike the cut-off trio, the
///   nest carries the one guard that matters (refusing to strand the roster at
///   zero superadmins) — the client renders the split, not the refusal.
/// - **A `deleting` row offers neither restore nor make-admin.** The ladder has
///   reached deletion, the nest refuses restore from there, and the role would
///   only hold the deletion (the finalize refuses an admin). Remove-admin stays:
///   it is what lets a held deletion complete.
pub fn admin_user_row_controls(user: &AdminUser) -> AdminUserRowControls {
    let lifecycle = admin_user_lifecycle(user);
    let cut_off_allowed = !user.is_admin;
    AdminUserRowControls {
        suspend: cut_off_allowed
            && matches!(
                lifecycle,
                AdminUserLifecycle::Active | AdminUserLifecycle::EvictionWarning
            ),
        evict: cut_off_allowed && lifecycle == AdminUserLifecycle::Active,
        restore: !matches!(
            lifecycle,
            AdminUserLifecycle::Active | AdminUserLifecycle::Deleting
        ),
        make_admin: !user.is_admin && lifecycle != AdminUserLifecycle::Deleting,
        remove_admin: user.is_admin,
    }
}

/// The option text an admin picker offers for `user` (`admin.md` § 2 → *What
/// identifies a user in an admin picker*): the **handle** — unique on the
/// nest by construction, where the display `label` is freely editable and
/// non-unique — falling back to the full actor hex for a handle-less
/// account. The one shared decision behind tui's/linux's guardian and
/// invite pickers and (over FFI/wasm) android's/web's — no client re-derives
/// the handle-or-hex fallback (priority #1/#2).
pub fn admin_picker_option(user: &AdminUser) -> String {
    match user.handle.as_deref().filter(|h| !h.is_empty()) {
        Some(handle) => handle.to_string(),
        None => fauna_core::format::hex_full(user.actor_id.as_ref()),
    }
}

/// The `(actor_id, option)` pairs an actor picker offers, built from
/// [`users_list_all`]'s accounts — the DNS catch-all/role-address family and
/// the web apex family share this one mapping (`admin.md` § 2 → *What
/// identifies a user in an admin picker*) so a future edit at either build
/// site can't reintroduce the raw, non-unique `label` independently of the
/// other — the one
/// implementation fleet-wide, consumed directly by tui and linux; android/web
/// build the same mapping per-item, each call reaching [`admin_picker_option`]
/// over FFI/wasm (no list-shaped export crosses that boundary today).
pub fn actor_picker_options(users: &[AdminUser]) -> Vec<(Vec<u8>, String)> {
    users
        .iter()
        .map(|u| (u.actor_id.to_vec(), admin_picker_option(u)))
        .collect()
}

/// Every account on the nest, newest first — the account list each admin
/// actor picker offers (`admin.md` § 2 → *Which accounts a picker offers*).
///
/// `fauna.admin.users.list` answers one page, so a picker reading a single
/// reply loses the oldest accounts — the box claimer first — once the nest
/// holds more than a page. This pages at the nest's ceiling
/// ([`admin::USERS_LIST_MAX_LIMIT`]: one round trip for up to 500 accounts)
/// until the reply's `total`, skips an actor id an earlier page already
/// returned, and stops early on an empty page or on a page adding no new
/// account, so a nest that ignores `offset` cannot loop it.
///
/// A free function over [`AdminClient`] rather than a method, so the client
/// keeps one method per kind (`admin.md` § Where logic lives). Native apps
/// reach it as `FfiAdminClient::users_list_all`, the web SPA as
/// `adminUsersListAll`.
pub async fn users_list_all<R: RpcRequester>(
    client: &AdminClient<R>,
) -> Result<Vec<AdminUser>, R::Error> {
    let mut users: Vec<AdminUser> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut offset = 0;
    loop {
        let page = client
            .users_list(Some(admin::USERS_LIST_MAX_LIMIT), offset)
            .await?;
        let returned = page.users.len();
        let before = users.len();
        for user in page.users {
            if seen.insert(user.actor_id.to_vec()) {
                users.push(user);
            }
        }
        offset += returned as i64;
        if returned == 0 || users.len() == before || offset >= page.total {
            return Ok(users);
        }
    }
}

/// The registration-mode select's display order — the shipped order on every
/// app (`admin.md` § 2 Users → *Section 2 — Registration*;
/// `public-mode.md` § Registration Modes). Not derived from the enum's
/// declaration order via a discriminant: explicit, so the picker order can
/// never silently drift if a variant is ever reordered/inserted.
const REGISTRATION_MODE_ORDER: [RegistrationMode; 3] = [
    RegistrationMode::Open,
    RegistrationMode::InviteRequired,
    RegistrationMode::Closed,
];

/// One `admin-users-registration-mode-select` option: the canonical wire value
/// ([`RegistrationMode::as_wire_str`]) plus the i18n label key the client
/// resolves. Mirrors `ConflictPolicyOption` /
/// `RuleTypeOption`: shared Rust owns the option *set*, the client owns the
/// widget. `Serialize` is what the web wasm face (`registrationModeOptions`)
/// hands across the boundary; the natives go through the `uniffi::Record`
/// mirror in `fauna-ffi`, which can't derive off a foreign type (mirrors
/// [`AdminUserRowControls`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegistrationModeOption {
    pub value: String,
    pub label: fauna_core::localized::LocalizedText,
}

/// i18n label key for a [`RegistrationMode`] value (`admin.users_page.*`).
fn registration_mode_label_key(mode: RegistrationMode) -> &'static str {
    match mode {
        RegistrationMode::Open => "admin.users_page.registration_mode_open",
        RegistrationMode::InviteRequired => "admin.users_page.registration_mode_invite_required",
        RegistrationMode::Closed => "admin.users_page.registration_mode_closed",
    }
}

/// The canonical `admin-users-registration-mode-select` picker options, in the
/// ratified order. Was hand-rolled identically on linux/android/web (each
/// re-listing the same 3 wire values in the same order, each own doc comment
/// claiming to mirror the others) — this is the one source of the vocabulary +
/// order, so a client never spells `"open"`/`"invite_required"`/`"closed"`
/// itself. windows/apple have not built the section yet; they adopt this on
/// arrival instead of hand-rolling a fourth copy.
pub fn registration_mode_options() -> Vec<RegistrationModeOption> {
    REGISTRATION_MODE_ORDER
        .into_iter()
        .map(|m| RegistrationModeOption {
            value: m.as_wire_str().to_string(),
            label: fauna_core::localized::LocalizedText::key(registration_mode_label_key(m)),
        })
        .collect()
}

// ── The age-band pickers: one option catalog for both admission surfaces ─────

/// The option value the two age-band selects carry for "no band chosen" —
/// the picker's first row (`admin-users-invite-age-band-select` /
/// `invite-request-row-age-band-select`; `family-safety.md` § App surface →
/// *Age-band surfaces*). A non-empty token that [`AgeBand::from_wire`] can
/// never name (so it can never reach the wire as a band), rather than `""`:
/// a select whose current value paints as an empty string reads as broken on
/// a terminal, and the cross-app `select(id, value)` e2e contract drives every
/// option — this one included — by value.
pub const AGE_BAND_NOT_SET_VALUE: &str = "not-set";

/// One age-band select option: the canonical wire token
/// ([`AgeBand::as_str`], or [`AGE_BAND_NOT_SET_VALUE`]) plus the i18n label
/// the client resolves (`family.age_band.*`). The [`RegistrationModeOption`]
/// shape — shared Rust owns the option *set* and its order, the client owns
/// the widget.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgeBandOption {
    pub value: String,
    pub label: LocalizedText,
}

/// The canonical age-band picker options, in the ratified order: *not set*
/// first, then the four bands oldest-last ([`AgeBand::ORDER`]). One source for
/// both admission surfaces on all 7 apps, so no client spells
/// `u13`/`13-15`/`16-17`/`18+` or its order itself.
pub fn age_band_options() -> Vec<AgeBandOption> {
    std::iter::once(AgeBandOption {
        value: AGE_BAND_NOT_SET_VALUE.to_string(),
        label: LocalizedText::key("family.age_band.not_set"),
    })
    .chain(AgeBand::ORDER.into_iter().map(|b| AgeBandOption {
        value: b.as_str().to_string(),
        label: LocalizedText::key(b.label_key()),
    }))
    .collect()
}

/// The typed band a select's committed option value names — `None` for
/// [`AGE_BAND_NOT_SET_VALUE`] **and** for anything the vocabulary cannot name,
/// so a stale or hand-typed value can never reach the wire as a band.
pub fn age_band_from_option_value(value: &str) -> Option<AgeBand> {
    AgeBand::from_wire(value)
}

/// The `invite-request-row-age-band-select` seed for a pending request: the
/// applicant's claimed band (`AdminInviteRequest.age_band`) as an option
/// value when this client can name it, else [`AGE_BAND_NOT_SET_VALUE`] — the
/// claim corroborates, the admitting adult decides (`family-safety.md` § App
/// surface → *Age-band surfaces*, D5). One rule for all 7 apps.
pub fn claimed_age_band_option(claimed: Option<&str>) -> String {
    claimed
        .and_then(AgeBand::from_wire)
        .map_or(AGE_BAND_NOT_SET_VALUE, AgeBand::as_str)
        .to_string()
}

// ── The declared region: the wire→render decisions, made once ────────────────

/// Everything the `admin-nest-region-*` section paints, derived from one
/// [`AdminRegionStatusReply`] (`admin.md` § N Nest → *Declared region*).
///
/// Shared rather than per-app for the reason boundary 4 of `dynamic-features.md`
/// § What this is NOT gives: the region tier can *bind* accounts on this
/// deployment, so what the admin is told about it must not differ per app
/// (priority #1). Three of the four decisions below are ones an app folding the
/// reply itself gets wrong in the same way — see each field.
///
/// Pure and clock-free: `stale` is the nest's own judgement (it owns the
/// refresh cadence), never re-derived here from `last_checked_at`, which would
/// make seven apps disagree with the nest and with each other about when a
/// channel has gone quiet.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AdminRegionView {
    /// The declared code, for the input's re-seed. `None` is the ratified
    /// fresh-install state.
    pub declared: Option<String>,
    /// `admin-nest-region-status` — the declared region, or that none is
    /// declared. **A normal state, never an error**: a deployment that has
    /// never declared is conforming, and rendering this through an error
    /// surface would tell an admin something is broken when nothing is.
    ///
    /// The one exception is `declaration_unreadable` — corruption, not absence
    /// — whose wording carries its own recovery (re-declare or withdraw), since
    /// this screen's own controls are the in-app fix.
    pub status: fauna_core::localized::LocalizedText,
    /// `admin-nest-region-authority` — whether the declared region has an
    /// enrolled authority and which policy document is in force. `None` only
    /// while nothing is declared **and** no document still binds (an unreadable
    /// declaration keeps naming the document whose bounds stay in force —
    /// `dynamic-features.md` § Fail posture, the unreadable-declaration
    /// clause).
    ///
    /// The honest everywhere-today answer is *"no authority is enrolled"* — the
    /// curated registry is empty, so no deployment on earth has one. Saying so
    /// plainly is what stops an admin concluding the feature is broken.
    pub authority: Option<fauna_core::localized::LocalizedText>,
    /// `admin-nest-region-staleness` — present **only** when the nest reports
    /// the channel unreached for longer than its cadence allows.
    ///
    /// Worded as a warning and nothing stronger, because § Fail posture is
    /// explicit that a stale nest keeps enforcing the last-known-good document:
    /// enforcement never relaxes on no information, so this is an "act when you
    /// can", not an outage.
    pub staleness: Option<fauna_core::localized::LocalizedText>,
    /// Whether `admin-nest-region-withdraw-button` paints at all — there is
    /// nothing to withdraw before a declaration exists.
    pub can_withdraw: bool,
}

/// The undeclared state — **not** a derived `Default`, which would give an empty
/// [`fauna_core::localized::LocalizedText`] and paint a blank status line.
///
/// A blank line reads as "nothing to say here"; the truth is the ratified
/// fresh-install state, and it has words. This is what a defaulted snapshot
/// (an app whose read has not landed yet) therefore renders.
impl Default for AdminRegionView {
    fn default() -> Self {
        admin_region_view(&AdminRegionStatusReply::default())
    }
}

/// Fold [`AdminClient::region_status`]'s reply into what a screen renders.
pub fn admin_region_view(reply: &AdminRegionStatusReply) -> AdminRegionView {
    let Some(declared) = reply.declared.as_ref() else {
        // Two undeclared-looking states, deliberately distinguished: an
        // unreadable declaration is corruption, not absence. The row exists, so
        // withdrawing stays offered (it clears the corrupt row — a recovery
        // affordance, alongside re-declaring), and a document whose folded
        // bounds still bind keeps being named — reporting "no region" while a
        // region's rules are in force is the silent gate boundary 4 forbids.
        if reply.declaration_unreadable {
            return AdminRegionView {
                declared: None,
                status: LocalizedText::key("admin.nest_page.region_unreadable"),
                authority: reply.feature_policy.as_ref().map(|doc| {
                    LocalizedText::key_args(
                        "admin.nest_page.region_document",
                        [
                            ("authority", doc.authority_name.clone()),
                            ("sequence", doc.sequence.to_string()),
                        ],
                    )
                }),
                staleness: None,
                can_withdraw: true,
            };
        }
        return AdminRegionView {
            declared: None,
            status: LocalizedText::key("admin.nest_page.region_none"),
            authority: None,
            staleness: None,
            can_withdraw: false,
        };
    };
    let code = declared.as_str().to_string();

    // The document's own record of who signed it, NOT a fresh registry lookup:
    // the authority name is stored at acceptance nest-side precisely so a
    // de-listing cannot turn a document still in force into "no region" — a
    // bound nobody can see (`dynamic-features.md` § Implementation status, the
    // region tier's ruling (iv)).
    let authority = match (&reply.feature_policy, reply.enrolled) {
        (Some(doc), _) => LocalizedText::key_args(
            "admin.nest_page.region_document",
            [
                ("authority", doc.authority_name.clone()),
                ("sequence", doc.sequence.to_string()),
            ],
        ),
        (None, true) => LocalizedText::key("admin.nest_page.region_enrolled_no_document"),
        (None, false) => LocalizedText::key("admin.nest_page.region_not_enrolled"),
    };

    AdminRegionView {
        declared: Some(code.clone()),
        status: LocalizedText::key_arg("admin.nest_page.region_declared", "region", code),
        authority: Some(authority),
        staleness: reply
            .stale
            .then(|| LocalizedText::key("admin.nest_page.region_stale")),
        can_withdraw: true,
    }
}

/// Validate a typed region code before it reaches the wire, returning the i18n
/// key of the refusal rather than a raw parse error.
///
/// Shared for the same reason the picker vocabularies above are: the accepted
/// shape is `RegionCode`'s (2–8 uppercase ASCII alphanumerics, **never**
/// case-folded — a normalising client would make two spellings of one region
/// both storable and the nest's key ambiguous), and seven apps re-deriving that
/// from the type's doc comment is seven chances to lower-case it helpfully.
///
/// Deliberately no "detect my region" companion, here or anywhere: determination
/// is user-ratified as *declared, never detected* (`region-blocking.md` §
/// Region determination), so there is no IP lookup and no locale guess to
/// prefill from.
pub fn parse_region_code(raw: &str) -> Result<RegionCode, &'static str> {
    RegionCode::parse(raw.trim()).map_err(|_| "admin.nest_page.region_invalid")
}

// ── The web-app origin: the wire→render decisions, made once ─────────────────

/// `true` when a transport error is the nest's typed "I don't know this kind"
/// — the additive-evolution answer an older nest gives a newer app.
fn is_unknown_kind<E: fauna_protocol::RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|r| r.code == "fauna.protocol.unknown_kind")
}

/// Everything the `admin-nest-web-app-origin-*` section paints, derived from
/// one [`AdminClient::web_app_origin`] read or [`AdminClient::set_web_app_origin`]
/// reply (`admin.md` § N Nest → *Web-app origin*).
///
/// Shared rather than per-app (priority #1): the target address is the nest's
/// own projection, never re-derived here, and the scope sentence is the one
/// place the trust claim could be overstated (`front-door.md` § Which origin a
/// user loads the app from) — seven apps wording it is seven chances to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AdminWebAppOriginView {
    /// The mode the radios pre-select (`-bundled-radio` / `-central-radio`).
    /// `None` when the nest predates the choice or names a mode this build
    /// cannot — marking a radio there would state a choice the nest does not
    /// hold.
    pub selected: Option<WebAppOrigin>,
    /// `admin-nest-web-app-origin-status` — what this nest's address answers
    /// now: the exact redirect target in central mode, the domainless reason,
    /// or why the choice cannot be set here.
    pub status: LocalizedText,
    /// The sentence that the choice changes only what this nest's own address
    /// answers — never where a user who opens the central origin loads from.
    pub scope: LocalizedText,
    /// Whether the radios and `-save-button` are live. `false` on a nest that
    /// predates the choice (the kind does not exist there) and on a mode this
    /// build cannot name (it renders read-only rather than overwrite a choice
    /// it does not understand).
    pub can_set: bool,
    /// `admin-nest-web-app-origin-central-radio`'s label — it names the
    /// central origin from the one constant, so no app spells the address.
    pub central_label: LocalizedText,
}

/// The never-set state — bundled, the works-out-of-the-box default — **not** a
/// derived `Default`, which would paint a blank status line.
impl Default for AdminWebAppOriginView {
    fn default() -> Self {
        admin_web_app_origin_view(Some(&AdminWebAppOriginGetReply::project(
            WebAppOrigin::Bundled,
            None,
        )))
    }
}

/// Fold the read into what a screen renders. `None` is a nest that predates
/// the choice ([`AdminClient::web_app_origin`]'s `Ok(None)`).
pub fn admin_web_app_origin_view(
    reply: Option<&AdminWebAppOriginGetReply>,
) -> AdminWebAppOriginView {
    let scope = LocalizedText::key_arg(
        "admin.nest_page.web_app_origin_scope",
        "origin",
        CENTRAL_APP_ORIGIN,
    );
    let central_label = LocalizedText::key_arg(
        "admin.nest_page.web_app_origin_central",
        "origin",
        CENTRAL_APP_ORIGIN,
    );
    let Some(reply) = reply else {
        return AdminWebAppOriginView {
            selected: None,
            status: LocalizedText::key("admin.nest_page.web_app_origin_status_predates"),
            scope,
            can_set: false,
            central_label,
        };
    };
    let Some(mode) = WebAppOrigin::parse(&reply.mode) else {
        return AdminWebAppOriginView {
            selected: None,
            status: LocalizedText::key_arg(
                "admin.nest_page.web_app_origin_status_unknown_mode",
                "mode",
                reply.mode.clone(),
            ),
            scope,
            can_set: false,
            central_label,
        };
    };
    // The target is the nest's projection, read verbatim: the nest owns the
    // derivation (`fauna_protocol::web_app_origin`), and a central reply with
    // neither a target nor the domainless flag is a nest that serves bundled —
    // say what it answers, never invent an address.
    let status = match (mode, reply.redirect_target.as_deref(), reply.domainless) {
        (WebAppOrigin::Central, Some(target), _) => LocalizedText::key_arg(
            "admin.nest_page.web_app_origin_status_central",
            "target",
            target,
        ),
        (WebAppOrigin::Central, None, true) => LocalizedText::key_arg(
            "admin.nest_page.web_app_origin_status_domainless",
            "origin",
            CENTRAL_APP_ORIGIN,
        ),
        _ => LocalizedText::key("admin.nest_page.web_app_origin_status_bundled"),
    };
    AdminWebAppOriginView {
        selected: Some(mode),
        status,
        scope,
        can_set: true,
        central_label,
    }
}

// ── Deployment-seed rotation: the confirm surface's roster fold ──────────────

/// One row of the deployment-seed rotation confirm surface — an admin who
/// **inherits** the successor seed through the self-healing capture
/// (`box-recovery.md` § Custody after rotation).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SeedRotationInheritor {
    /// Raw 32-byte actor id — the stable identity behind the label.
    pub actor_id: Vec<u8>,
    /// What a screen paints: the member's `AdminUser.label` when one resolved,
    /// else the canonical short id
    /// ([`fauna_core::format::account_display_label`], so a roster row and an
    /// account-switcher row elide the same way).
    pub label: String,
}

/// What the rotation confirm surface renders, derived once here rather than
/// seven times (priority #2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SeedRotationConfirmView {
    /// The current roster, in the nest's own order — the set that will inherit.
    pub inheritors: Vec<SeedRotationInheritor>,
    /// Whether the destructive confirm may be offered at all.
    pub can_confirm: bool,
    /// Why not, when it may not — the page states the reason instead of
    /// painting a control that cannot honour the ceremony's own contract.
    pub blocked_reason: Option<fauna_core::localized::LocalizedText>,
}

/// Fold the roster read into the rotation confirm surface
/// (`box-recovery.md` § Deployment-seed rotation → *Ordering rule — rotate only
/// into a clean roster*: "the app's confirm surface lists the current roster —
/// the set that will inherit — before dispatch").
///
/// `labels` is the caller's best-effort `(actor_id, AdminUser.label)` join — the
/// roster wire shape carries no name at all, so a surface that wants names
/// resolves them (today: one [`AdminClient::users_get`] per member; the roster
/// is 1–3 rows). An unresolved **or blank** label degrades the row to the short
/// id; it never drops the row, because an inheritor we cannot name still
/// inherits and omitting them would under-state who keeps recovery custody.
///
/// An **empty** roster withholds the confirm. It cannot satisfy the doc's
/// "lists the set that will inherit", and it is also self-refuting — the caller
/// is themselves an admin, so a nest reporting nobody is answering wrongly.
/// Rotating on that reading would flip the box's identity while telling the
/// admin nothing true about who keeps recovery custody.
pub fn seed_rotation_confirm_view(
    roster: &AdminAdminsListReply,
    labels: &[(Vec<u8>, String)],
) -> SeedRotationConfirmView {
    let inheritors: Vec<SeedRotationInheritor> = roster
        .admins
        .iter()
        .map(|entry| {
            let actor_id = entry.actor_id.to_vec();
            let resolved = labels
                .iter()
                .find(|(id, _)| id == &actor_id)
                .map(|(_, l)| l.as_str());
            SeedRotationInheritor {
                label: fauna_core::format::account_display_label(resolved, &hex::encode(&actor_id)),
                actor_id,
            }
        })
        .collect();

    let can_confirm = !inheritors.is_empty();
    SeedRotationConfirmView {
        inheritors,
        can_confirm,
        blocked_reason: (!can_confirm).then(|| {
            fauna_core::localized::LocalizedText::key("admin.nest_page.rotate_seed_roster_empty")
        }),
    }
}

// ── The issuer key set view — what the admin-shell control renders ───────────

/// One key in the issuer's served set, as the admin shell lists it.
///
/// `Deserialize` as well as `Serialize`, like [`IssuerKeyView`] and
/// [`IssuerForcedArm`]: the web SPA holds the folded view as plain JSON and
/// hands it back to the pure folds below through the wasm face.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssuerKeyRow {
    /// The RFC 7638 thumbprint `/oauth/jwks` publishes for this key.
    pub kid: String,
    /// Whether this is the key currently signing. Exactly one row is, on a
    /// nest that holds a deployment signing key.
    pub signing: bool,
    /// When a rotation retired this key (epoch seconds) — `None` for the
    /// signer.
    pub retired_at: Option<i64>,
    /// When the JWKS stops carrying this key (epoch seconds): `retired_at +
    /// retirement_horizon_secs`, computed here so no app re-derives the sum
    /// — the number the nest reports is the one it will act on. `None` for
    /// the signer. This is what turns "rotated 2 minutes ago" into "the old
    /// key stops being served in 18 minutes".
    pub served_until: Option<i64>,
}

/// What the admin-shell issuer-key control renders, derived once here rather
/// than seven times (priority #2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssuerKeyView {
    /// The `kid` of the key currently signing — empty only on a nest holding
    /// no deployment signing key, the same condition that makes `/oauth/jwks`
    /// unable to answer.
    pub active_kid: String,
    /// Every served key, signer first, in the nest's own order.
    pub keys: Vec<IssuerKeyRow>,
    /// How long after `retired_at` a key stops being served, in seconds.
    pub retirement_horizon_secs: u64,
    /// Whether any retired key is still being served — a rotation inside its
    /// horizon. The control uses it to say "the outgoing key is still served"
    /// beside the ordinary arm, and to make the forced arm's cost concrete
    /// ("drops N keys", not just the signer).
    pub rotation_in_flight: bool,
}

/// Fold the `fauna.oauth.issuer_key_status` reply into the control's view.
///
/// `served_until` saturates rather than wrapping: the horizon is a `u64`
/// policy constant and `retired_at` an `i64` epoch, and a nest reporting
/// nonsense must render as "far future", never as a negative instant that
/// reads as already gone.
pub fn issuer_key_view(reply: &IssuerKeyStatusReply) -> IssuerKeyView {
    let horizon = i64::try_from(reply.retirement_horizon_secs).unwrap_or(i64::MAX);
    let keys: Vec<IssuerKeyRow> = reply
        .keys
        .iter()
        .map(|k| IssuerKeyRow {
            kid: k.kid.clone(),
            signing: k.retired_at.is_none(),
            retired_at: k.retired_at,
            served_until: k.retired_at.map(|t| t.saturating_add(horizon)),
        })
        .collect();
    IssuerKeyView {
        active_kid: reply.active_kid.clone(),
        rotation_in_flight: keys.iter().any(|k| !k.signing),
        keys,
        retirement_horizon_secs: reply.retirement_horizon_secs,
    }
}

// ── The issuer key controls' words — worded once, rendered by all 7 apps ─────
//
// Every sentence the admin reads about the issuer's two signers is decided
// here (priority #2): the per-key line, what each of the three controls costs,
// and what each reply means. An app picks the element and resolves the text;
// it never chooses which sentence a compromise response gets.

/// Whole minutes, rounded UP, for a positive count of seconds — "0 min" beside
/// a key the JWKS still serves would read as already gone.
fn whole_minutes_up(secs: i64) -> String {
    // Spelled out rather than `div_ceil` (unstable for signed ints), and in
    // this order so a saturated `i64::MAX` horizon cannot overflow the add.
    let secs = secs.max(1);
    (secs / 60 + i64::from(secs % 60 != 0)).to_string()
}

/// One served key's line, e.g. `"<kid> — signing"` or `"<kid> — replaced; still
/// accepted for 18 min"`.
///
/// `now_secs` is the app's clock at paint, because the countdown is the whole
/// point of the line (`authorization-server.md` § The issuer's status kind: "an
/// admin reads 'the old key stops being served in N minutes' rather than
/// deriving it"); `served_until` is the nest's own `retired_at + horizon`, so
/// the only arithmetic left is the subtraction. A key past its instant is
/// reported as no longer accepted — the nest drops it on its next key-set read.
pub fn issuer_key_row_label(row: &IssuerKeyRow, now_secs: i64) -> LocalizedText {
    if row.signing {
        return LocalizedText::key_arg("admin.nest_page.oauth_key_signing", "kid", row.kid.clone());
    }
    match row.served_until {
        Some(until) if until > now_secs => LocalizedText::key_args(
            "admin.nest_page.oauth_key_retiring",
            [
                ("kid", row.kid.clone()),
                ("minutes", whole_minutes_up(until - now_secs)),
            ],
        ),
        _ => LocalizedText::key_arg("admin.nest_page.oauth_key_retired", "kid", row.kid.clone()),
    }
}

/// The ordinary rotation's cost, stated beside its button: precautionary, the
/// outgoing key stays accepted for the retirement horizon, nobody is signed
/// out. It needs no confirm (§ The issuer → *Two rotation arms*: "nothing
/// breaks") — only the two forced arms do.
pub fn issuer_key_rotate_cost(view: &IssuerKeyView) -> LocalizedText {
    let horizon = i64::try_from(view.retirement_horizon_secs).unwrap_or(i64::MAX);
    LocalizedText::key_arg(
        "admin.nest_page.oauth_rotate_desc",
        "minutes",
        whole_minutes_up(horizon),
    )
}

/// The two forced arms — the compromise response proper
/// (`authorization-server.md` § The issuer → *Two rotation arms*). Each states
/// its cost in an armed inline confirm before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IssuerForcedArm {
    /// `fauna.oauth.force_rotate_issuer_key` — mint a signer and drop every
    /// other served key at once, so a leaked `kid` leaves the JWKS on the next
    /// read. Every honest token signed by a dropped key stops verifying now.
    IssuerKey,
    /// `fauna.oauth.force_rotate_session_secret` — re-mint the second signer, so
    /// every outstanding OAuth refresh token dies and every connected app must
    /// re-consent. The half of the response the key arm cannot reach.
    SessionSecret,
}

impl IssuerForcedArm {
    /// The wire kind this arm dispatches — one place, so a confirm can never
    /// fire the other arm's kind.
    pub fn kind(self) -> &'static str {
        match self {
            IssuerForcedArm::IssuerKey => "fauna.oauth.force_rotate_issuer_key",
            IssuerForcedArm::SessionSecret => "fauna.oauth.force_rotate_session_secret",
        }
    }
}

/// What an armed forced confirm paints: its cost, stated before dispatch, and
/// the confirm button's words.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IssuerForcedConfirmView {
    pub summary: LocalizedText,
    pub confirm_label: LocalizedText,
}

/// Fold an arm into its confirm. The key arm names **how many** served keys
/// stop verifying — a rotation inside its horizon means two, not just the
/// signer, and the admin should read that number before choosing it.
pub fn issuer_forced_confirm_view(
    arm: IssuerForcedArm,
    view: &IssuerKeyView,
) -> IssuerForcedConfirmView {
    match arm {
        IssuerForcedArm::IssuerKey => IssuerForcedConfirmView {
            summary: if view.keys.len() > 1 {
                LocalizedText::key_arg(
                    "admin.nest_page.oauth_force_rotate_confirm_many",
                    "count",
                    view.keys.len().to_string(),
                )
            } else {
                LocalizedText::key("admin.nest_page.oauth_force_rotate_confirm_one")
            },
            confirm_label: LocalizedText::key("admin.nest_page.oauth_force_rotate_confirm_button"),
        },
        IssuerForcedArm::SessionSecret => IssuerForcedConfirmView {
            summary: LocalizedText::key("admin.nest_page.oauth_secret_force_rotate_confirm"),
            confirm_label: LocalizedText::key(
                "admin.nest_page.oauth_secret_force_rotate_confirm_button",
            ),
        },
    }
}

/// A failed call's verdict, shared by all three controls. It says the nest did
/// not *confirm* — never that nothing changed: every one of these kinds is
/// replay-forbidden and mints on the nest, so a reply lost to a timeout can
/// follow a rotation that committed. The re-read key list is the truth.
fn issuer_control_failed(cause: &str) -> LocalizedText {
    LocalizedText::key_arg("admin.nest_page.oauth_rotate_failed", "cause", cause)
}

/// The ordinary rotation's verdict: the new signer's `kid`, which the admin
/// matches against the key list below it.
pub fn issuer_key_rotation_verdict(outcome: Result<&IssuerKeyRotation, &str>) -> LocalizedText {
    match outcome {
        Ok(reply) => LocalizedText::key_arg(
            "admin.nest_page.oauth_rotate_done",
            "kid",
            reply.kid.clone(),
        ),
        Err(cause) => issuer_control_failed(cause),
    }
}

/// The forced key rotation's verdict: the new signer and every `kid` that
/// stopped verifying, as the nest reported them — the admin sees exactly which
/// keys died rather than inferring it from a list that no longer shows them.
pub fn issuer_key_forced_rotation_verdict(
    outcome: Result<&IssuerKeyForcedRotation, &str>,
) -> LocalizedText {
    match outcome {
        Ok(reply) if reply.dropped_kids.is_empty() => LocalizedText::key_arg(
            "admin.nest_page.oauth_force_rotate_done_none",
            "kid",
            reply.kid.clone(),
        ),
        Ok(reply) => LocalizedText::key_args(
            "admin.nest_page.oauth_force_rotate_done",
            [
                ("kid", reply.kid.clone()),
                ("dropped", reply.dropped_kids.join(", ")),
            ],
        ),
        Err(cause) => issuer_control_failed(cause),
    }
}

/// The forced session-secret rotation's verdict: the instant the replaced
/// generation was minted, so the admin can date which sign-ins died, **and how
/// many outside apps were actually signed out** — or, on a nest that had never
/// minted a secret, that there was nothing to end.
///
/// The count is the blast radius as a fact rather than a sentence. The instants
/// alone say only *when* the secret changed; `grants_ended` says what that did,
/// which is the difference between an admin who knows their compromise response
/// signed out four connections and one who has to go and look.
///
/// Three arms on the count, picked here so no sentence has to read "1 apps" —
/// the shape `fauna_core::format::tip_count` uses for the same reason. The
/// zero arm is its own sentence rather than the plural with a `0`: "no outside
/// apps were connected here" is the useful reading, where "0 outside apps were
/// signed out" invites the admin to wonder what went wrong.
///
/// Worded here for all 7 apps, not per app: this is the one owner every app
/// reaches (tui and linux in-process, the other five through the UniFFI and
/// wasm faces), so the count needs no per-app work and no trickle-down row.
///
/// `format_instant` is the app's own local-time rendering of an epoch-seconds
/// instant: the wording is decided here, the clock face stays with the app
/// (this crate is wasm-clean and the OS timezone is not).
pub fn session_secret_forced_rotation_verdict(
    outcome: Result<&SessionSecretForcedRotation, &str>,
    format_instant: impl Fn(i64) -> String,
) -> LocalizedText {
    match outcome {
        Ok(reply) => match reply.replaced_minted_at {
            Some(minted) => {
                let minted = format_instant(minted);
                match reply.grants_ended {
                    0 => LocalizedText::key_arg(
                        "admin.nest_page.oauth_secret_force_rotate_done_none",
                        "minted",
                        minted,
                    ),
                    1 => LocalizedText::key_arg(
                        "admin.nest_page.oauth_secret_force_rotate_done_one",
                        "minted",
                        minted,
                    ),
                    apps => LocalizedText::key_args(
                        "admin.nest_page.oauth_secret_force_rotate_done",
                        [("minted", minted), ("apps", apps.to_string())],
                    ),
                }
            }
            // No secret to replace means no token was ever MACed under one, so
            // no grant this rotation could end — the count is necessarily 0 and
            // needs no arm of its own here.
            None => LocalizedText::key("admin.nest_page.oauth_secret_force_rotate_first"),
        },
        Err(cause) => issuer_control_failed(cause),
    }
}

// ── C6 — observability / logs: the wire→render conversion ────────────────────

/// One `fauna.admin.logs` wire row → the owned [`fauna_log::LogEntry`] every
/// admin Logs surface renders (`observability.md` § Surfaces — "the client's
/// admin surface renders it with the same widget as the client Logs page", so
/// the wire type must become the *same* type the local ring yields). The
/// inverse of the nest's `log_entry_to_wire`.
///
/// Shared here rather than per-app (priority #2): linux carried a private
/// `wire_to_log_entry` from 2026-06, and tui would have been the second copy.
/// A free function, not a `From` impl — both types are foreign to this crate.
///
/// A negative wire timestamp (never produced by the nest, but representable in
/// the `i64` the admin wire convention uses for millis) floors to 0 rather than
/// wrapping into the far future through the `u64` cast.
pub fn log_entry_from_wire(entry: &admin::AdminLogEntry) -> fauna_log::LogEntry {
    fauna_log::LogEntry {
        timestamp_ms: entry.timestamp_ms.max(0) as u64,
        level: match entry.level {
            admin::AdminLogLevel::Error => fauna_log::LogLevel::Error,
            admin::AdminLogLevel::Warn => fauna_log::LogLevel::Warn,
            admin::AdminLogLevel::Info => fauna_log::LogLevel::Info,
            admin::AdminLogLevel::Debug => fauna_log::LogLevel::Debug,
            admin::AdminLogLevel::Trace => fauna_log::LogLevel::Trace,
            // A level a newer nest added: the least severe, so the row shows
            // only in the most inclusive view and never raises an alarm.
            admin::AdminLogLevel::Unknown => fauna_log::LogLevel::Trace,
        },
        target: entry.target.clone(),
        message: entry.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{FailingRequester, MockRequester, RecordingRequester, block_on};
    use std::sync::Arc;

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = AdminClient::new(MockRequester);
    }

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.admin.region.set" => fauna_protocol::encode_canonical(&AdminRegionSetReply {
                ok: true,
                ..Default::default()
            }),
            "fauna.admin.region.get" => {
                fauna_protocol::encode_canonical(&AdminRegionStatusReply::default())
            }
            "fauna.admin.web_app_origin.set" => fauna_protocol::encode_canonical(
                &AdminWebAppOriginGetReply::project(WebAppOrigin::Central, Some("example.org")),
            ),
            "fauna.admin.invite_codes.create" => {
                fauna_protocol::encode_canonical(&AdminInviteCodeCreateReply {
                    code: "MINTED1234".into(),
                    ok: true,
                    ..Default::default()
                })
            }
            "fauna.admin.invite_requests.approve" => {
                fauna_protocol::encode_canonical(&AdminInviteRequestApproveReply {
                    actor_id: ByteBuf::from(vec![7u8; 32]),
                    handle: "bob@fauna.test".into(),
                    tier: "personal".into(),
                    ..Default::default()
                })
            }
            "fauna.admin.set_age_verification_required" => {
                fauna_protocol::encode_canonical(&SetAgeVerificationRequiredReply {
                    ok: true,
                    extra: Default::default(),
                })
            }
            "fauna.admin.admins.list" => fauna_protocol::encode_canonical(&AdminAdminsListReply {
                admins: vec![admin::AdminAdminEntry {
                    actor_id: ByteBuf::from(vec![1u8; 32]),
                    added_at: 5,
                    ..Default::default()
                }],
                ..Default::default()
            }),
            "fauna.admin.users.get" => fauna_protocol::encode_canonical(&AdminUserGetReply {
                user: admin::AdminUser {
                    actor_id: ByteBuf::from(vec![1u8; 32]),
                    label: "root@fauna.test".into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            "fauna.admin.users.list" => fauna_protocol::encode_canonical(&AdminUsersListReply {
                users: vec![],
                total: 0,
                ..Default::default()
            }),
            "fauna.admin.logs" => fauna_protocol::encode_canonical(&AdminLogsReply {
                entries: vec![admin::AdminLogEntry {
                    timestamp_ms: 42,
                    level: admin::AdminLogLevel::Warn,
                    target: "fauna_nest".into(),
                    message: "hi".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            "fauna.admin.factory_reset" => {
                fauna_protocol::encode_canonical(&admin::FactoryResetReply {
                    claim_code: "ABC123".into(),
                    ..Default::default()
                })
            }
            "fauna.admin.admins.add" | "fauna.admin.admins.remove" => {
                fauna_protocol::encode_canonical(&AdminPendingActionReply {
                    pending_action_id: 7,
                    execute_after: 1_700_000_000,
                    status: "pending".into(),
                    ..Default::default()
                })
            }
            "fauna.admin.stats" => fauna_protocol::encode_canonical(&AdminStatsReply {
                total_users: 7,
                users_by_tier: vec![("free".into(), 5), ("personal".into(), 2)],
                suspended_users: 1,
                total_inbox_bytes: 1024,
                total_storage_bytes: 4096,
                ws_connections: 3,
                ..Default::default()
            }),
            "fauna.admin.status" => fauna_protocol::encode_canonical(&AdminStatusReply {
                version: "9.9.9".into(),
                update_available: None,
                ..Default::default()
            }),
            "fauna.oauth.issuer_key_status" => {
                fauna_protocol::encode_canonical(&IssuerKeyStatusReply {
                    active_kid: "new-kid".into(),
                    keys: vec![
                        IssuerKeyEntry {
                            kid: "new-kid".into(),
                            retired_at: None,
                            ..Default::default()
                        },
                        IssuerKeyEntry {
                            kid: "old-kid".into(),
                            retired_at: Some(1_700_000_000),
                            ..Default::default()
                        },
                    ],
                    retirement_horizon_secs: 1200,
                    ..Default::default()
                })
            }
            "fauna.oauth.rotate_issuer_key" => {
                fauna_protocol::encode_canonical(&RotateIssuerKeyReply {
                    kid: "newer-kid".into(),
                    rotated_at: 1_700_000_100,
                    ..Default::default()
                })
            }
            "fauna.oauth.force_rotate_issuer_key" => {
                fauna_protocol::encode_canonical(&ForceRotateIssuerKeyReply {
                    kid: "newest-kid".into(),
                    rotated_at: 1_700_000_200,
                    dropped_kids: vec!["new-kid".into(), "old-kid".into()],
                    ..Default::default()
                })
            }
            "fauna.oauth.force_rotate_session_secret" => {
                fauna_protocol::encode_canonical(&ForceRotateSessionSecretReply {
                    rotated_at: 1_700_000_300,
                    replaced_minted_at: Some(1_690_000_000),
                    // Non-zero on purpose: the count has to survive the wire
                    // decode and reach the sentence, which a 0 here could not
                    // distinguish from a zeroed default.
                    grants_ended: 3,
                    ..Default::default()
                })
            }
            // Every other kind wrapped here decodes an `AdminOkReply`
            // (update / evict / cancel_eviction / clear_handle / tiers
            // create+update / invite_codes.delete / invite_requests.deny).
            _ => fauna_protocol::encode_canonical(&AdminOkReply {
                ok: true,
                ..Default::default()
            }),
        }
        .expect("encode reply")
        .to_vec()
    }

    fn entry(actor: u8, added_at: i64) -> admin::AdminAdminEntry {
        admin::AdminAdminEntry {
            actor_id: ByteBuf::from(vec![actor; 32]),
            added_at,
            ..Default::default()
        }
    }

    #[test]
    fn admins_list_composes_the_roster_kind() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let roster = block_on(client.admins_list()).expect("infallible");
        assert_eq!(roster.admins.len(), 1);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.admins.list");
        let _req: AdminAdminsListRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
    }

    /// The list→per-admin-join→fold orchestration lifted off the fauna-ffi and
    /// fauna-wasm faces (a two-face-test finding, 2026-08-25): the roster read
    /// comes first, then one `users.get` per admin to resolve a label, in that
    /// order — a caller relying on the sequence (e.g. a recording double that
    /// only knows how to answer `users.get` once the roster names an actor)
    /// would break if the calls were reordered or batched differently.
    #[test]
    fn seed_rotate_roster_view_composes_the_list_then_join_then_fold() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let view = block_on(client.seed_rotate_roster_view()).expect("infallible");

        assert_eq!(
            rec.kinds(),
            vec!["fauna.admin.admins.list", "fauna.admin.users.get"]
        );
        assert!(view.can_confirm);
        assert_eq!(view.blocked_reason, None);
        assert_eq!(view.inheritors.len(), 1);
        assert_eq!(view.inheritors[0].actor_id, vec![1u8; 32]);
        assert_eq!(view.inheritors[0].label, "root@fauna.test");
    }

    /// The three issuer-key kinds compose exactly their wire names with empty
    /// requests — the shape an older nest tolerates on the two it knows and
    /// refuses loudly on the one it does not (the forced arm is a kind, not a
    /// flag, for exactly that reason — `authorization-server.md` § The issuer
    /// → *Two rotation arms*).
    #[test]
    fn issuer_key_kinds_compose_their_wire_names_with_empty_requests() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let status = block_on(client.issuer_key_status()).expect("infallible");
        assert_eq!(status.active_kid, "new-kid");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.oauth.issuer_key_status");
        let _req: IssuerKeyStatusRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");

        let rotated = block_on(client.rotate_issuer_key()).expect("infallible");
        assert_eq!(rotated.kid, "newer-kid");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.oauth.rotate_issuer_key");
        let _req: RotateIssuerKeyRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");

        let forced = block_on(client.force_rotate_issuer_key()).expect("infallible");
        assert_eq!(forced.kid, "newest-kid");
        assert_eq!(forced.dropped_kids, vec!["new-kid", "old-kid"]);
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.oauth.force_rotate_issuer_key");
        let _req: ForceRotateIssuerKeyRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");

        let session = block_on(client.force_rotate_session_secret()).expect("infallible");
        assert_eq!(session.rotated_at, 1_700_000_300);
        assert_eq!(session.replaced_minted_at, Some(1_690_000_000));
        assert_eq!(
            session.grants_ended, 3,
            "the count of connected apps the rotation ended rides the reply — \
             the admin's response has no audit shape without it"
        );
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.oauth.force_rotate_session_secret");
        let _req: ForceRotateSessionSecretRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
    }

    #[test]
    fn issuer_key_view_dates_the_outgoing_keys_exit_from_the_nests_own_horizon() {
        // The one derivation the control needs: `retired_at + horizon` is when
        // the old key stops being served, computed from the number the nest
        // reported — never a client-side copy of the constant.
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec);
        let status = block_on(client.issuer_key_status()).expect("infallible");

        let view = issuer_key_view(&status);
        assert_eq!(view.active_kid, "new-kid");
        assert_eq!(view.retirement_horizon_secs, 1200);
        assert!(view.rotation_in_flight);
        assert_eq!(view.keys.len(), 2);
        assert!(view.keys[0].signing, "the signer leads");
        assert_eq!(view.keys[0].served_until, None);
        assert!(!view.keys[1].signing);
        assert_eq!(view.keys[1].retired_at, Some(1_700_000_000));
        assert_eq!(view.keys[1].served_until, Some(1_700_001_200));
    }

    #[test]
    fn issuer_key_view_with_only_a_signer_reports_no_rotation_in_flight() {
        let status = IssuerKeyStatusReply {
            active_kid: "k".into(),
            keys: vec![IssuerKeyEntry {
                kid: "k".into(),
                retired_at: None,
                ..Default::default()
            }],
            retirement_horizon_secs: 1200,
            ..Default::default()
        };
        let view = issuer_key_view(&status);
        assert!(!view.rotation_in_flight);
        assert_eq!(view.keys.len(), 1);
        assert!(view.keys[0].signing);
    }

    #[test]
    fn issuer_key_view_saturates_an_absurd_horizon_rather_than_wrapping() {
        // A nest reporting a horizon past i64 must render "far future", not a
        // negative instant that reads as already gone.
        let status = IssuerKeyStatusReply {
            active_kid: "k".into(),
            keys: vec![IssuerKeyEntry {
                kid: "old".into(),
                retired_at: Some(5),
                ..Default::default()
            }],
            retirement_horizon_secs: u64::MAX,
            ..Default::default()
        };
        let view = issuer_key_view(&status);
        assert_eq!(view.keys[0].served_until, Some(i64::MAX));
    }

    fn key_row(kid: &str, served_until: Option<i64>) -> IssuerKeyRow {
        IssuerKeyRow {
            kid: kid.into(),
            signing: served_until.is_none(),
            retired_at: served_until.map(|t| t - 1200),
            served_until,
        }
    }

    fn view_of(keys: Vec<IssuerKeyRow>, horizon: u64) -> IssuerKeyView {
        IssuerKeyView {
            active_kid: keys
                .iter()
                .find(|k| k.signing)
                .map(|k| k.kid.clone())
                .unwrap_or_default(),
            rotation_in_flight: keys.iter().any(|k| !k.signing),
            keys,
            retirement_horizon_secs: horizon,
        }
    }

    /// The signer's line names its `kid` — the fact an admin matches against
    /// the rotate verdict's "now signs" to see which key is live.
    #[test]
    fn the_signing_row_names_its_kid() {
        let label = issuer_key_row_label(&key_row("kid-a", None), 1_000);
        assert_eq!(label.key, "admin.nest_page.oauth_key_signing");
        assert_eq!(label.args["kid"], "kid-a");
    }

    /// A retired key counts down in whole minutes, rounded UP: "0 min" beside a
    /// key the JWKS still carries would read as already gone, which is the one
    /// wrong thing a compromise surface must not say about a live key.
    #[test]
    fn a_retiring_key_counts_down_whole_minutes_rounding_up() {
        let now = 1_700_000_000;
        for (left, minutes) in [(1, "1"), (60, "1"), (61, "2"), (1_200, "20")] {
            let label = issuer_key_row_label(&key_row("old", Some(now + left)), now);
            assert_eq!(
                label.key, "admin.nest_page.oauth_key_retiring",
                "{left}s left"
            );
            assert_eq!(label.args["kid"], "old");
            assert_eq!(label.args["minutes"], minutes, "{left}s left");
        }
    }

    /// Past its horizon a key is no longer accepted — the nest drops it on its
    /// next key-set read, so the line says so rather than counting below zero.
    #[test]
    fn a_key_past_its_horizon_reads_as_no_longer_accepted() {
        let now = 1_700_000_000;
        for until in [now, now - 30] {
            let label = issuer_key_row_label(&key_row("gone", Some(until)), now);
            assert_eq!(label.key, "admin.nest_page.oauth_key_retired");
            assert_eq!(label.args["kid"], "gone");
        }
    }

    /// The ordinary arm states its cost beside its button, from the horizon the
    /// nest reported — never a client-side copy of the constant.
    #[test]
    fn the_ordinary_rotation_cost_quotes_the_nests_horizon_in_minutes() {
        // The last pair: a nest reporting an absurd horizon saturates rather
        // than overflowing the round-up (`issuer_key_view`'s posture).
        for (horizon, minutes) in [
            (1_200, "20"),
            (1_201, "21"),
            (30, "1"),
            (u64::MAX, "153722867280912931"),
        ] {
            let cost = issuer_key_rotate_cost(&view_of(vec![key_row("k", None)], horizon));
            assert_eq!(cost.key, "admin.nest_page.oauth_rotate_desc");
            assert_eq!(cost.args["minutes"], minutes, "horizon {horizon}s");
        }
    }

    /// The forced key arm's confirm makes its cost concrete: it names how many
    /// served keys stop verifying — a rotation in flight means two, not just the
    /// signer (`IssuerKeyView::rotation_in_flight`'s stated purpose).
    #[test]
    fn the_forced_key_confirm_names_how_many_keys_stop_verifying() {
        let lone = issuer_forced_confirm_view(
            IssuerForcedArm::IssuerKey,
            &view_of(vec![key_row("k", None)], 1_200),
        );
        assert_eq!(
            lone.summary.key,
            "admin.nest_page.oauth_force_rotate_confirm_one"
        );

        let two = issuer_forced_confirm_view(
            IssuerForcedArm::IssuerKey,
            &view_of(vec![key_row("k", None), key_row("old", Some(9))], 1_200),
        );
        assert_eq!(
            two.summary.key,
            "admin.nest_page.oauth_force_rotate_confirm_many"
        );
        assert_eq!(two.summary.args["count"], "2");
        assert_eq!(
            two.confirm_label.key,
            "admin.nest_page.oauth_force_rotate_confirm_button"
        );
    }

    /// The session-secret arm states the re-consent cost, and its confirm button
    /// reads differently from the key arm's — two forced acts with different
    /// blast radii must never share a button label.
    #[test]
    fn the_secret_confirm_states_the_reconsent_cost() {
        let view = view_of(vec![key_row("k", None)], 1_200);
        let secret = issuer_forced_confirm_view(IssuerForcedArm::SessionSecret, &view);
        let key = issuer_forced_confirm_view(IssuerForcedArm::IssuerKey, &view);
        assert_eq!(
            secret.summary.key,
            "admin.nest_page.oauth_secret_force_rotate_confirm"
        );
        assert_eq!(
            secret.confirm_label.key,
            "admin.nest_page.oauth_secret_force_rotate_confirm_button"
        );
        assert_ne!(secret.confirm_label, key.confirm_label);
    }

    /// Each forced arm names exactly its own kind — the pair a swap would make
    /// dangerous (ending sign-ins when the admin meant to drop a key).
    #[test]
    fn forced_arms_name_their_own_wire_kinds() {
        assert_eq!(
            IssuerForcedArm::IssuerKey.kind(),
            "fauna.oauth.force_rotate_issuer_key"
        );
        assert_eq!(
            IssuerForcedArm::SessionSecret.kind(),
            "fauna.oauth.force_rotate_session_secret"
        );
    }

    /// The three verdicts surface what each reply carries — the new `kid`, the
    /// dropped `kid`s, the replaced generation's instant — and every failure
    /// says the nest did not confirm, never that nothing changed: a timeout can
    /// land after the nest committed.
    #[test]
    fn the_three_verdicts_surface_their_replies() {
        let rotated = issuer_key_rotation_verdict(Ok(&IssuerKeyRotation {
            kid: "kid-b".into(),
            rotated_at: 1,
            ..Default::default()
        }));
        assert_eq!(rotated.key, "admin.nest_page.oauth_rotate_done");
        assert_eq!(rotated.args["kid"], "kid-b");

        let forced = issuer_key_forced_rotation_verdict(Ok(&IssuerKeyForcedRotation {
            kid: "kid-c".into(),
            rotated_at: 2,
            dropped_kids: vec!["kid-b".into(), "kid-a".into()],
            ..Default::default()
        }));
        assert_eq!(forced.key, "admin.nest_page.oauth_force_rotate_done");
        assert_eq!(forced.args["kid"], "kid-c");
        assert_eq!(forced.args["dropped"], "kid-b, kid-a");

        // Nothing ended: the zero arm, which is its own sentence rather than
        // the plural carrying a `0`.
        let replaced = session_secret_forced_rotation_verdict(
            Ok(&SessionSecretForcedRotation {
                rotated_at: 3,
                replaced_minted_at: Some(1_690_000_000),
                grants_ended: 0,
                ..Default::default()
            }),
            |secs| format!("at {secs}"),
        );
        assert_eq!(
            replaced.key,
            "admin.nest_page.oauth_secret_force_rotate_done_none"
        );
        assert_eq!(replaced.args["minted"], "at 1690000000");

        // One, then several — the plural is picked here so no sentence reads
        // "1 apps", and only the many-arm carries the count as an argument.
        let one = session_secret_forced_rotation_verdict(
            Ok(&SessionSecretForcedRotation {
                rotated_at: 3,
                replaced_minted_at: Some(1_690_000_000),
                grants_ended: 1,
                ..Default::default()
            }),
            |secs| format!("at {secs}"),
        );
        assert_eq!(
            one.key,
            "admin.nest_page.oauth_secret_force_rotate_done_one"
        );
        assert_eq!(one.args["minted"], "at 1690000000");
        assert!(!one.args.contains_key("apps"), "the one-arm names no count");

        let many = session_secret_forced_rotation_verdict(
            Ok(&SessionSecretForcedRotation {
                rotated_at: 3,
                replaced_minted_at: Some(1_690_000_000),
                grants_ended: 4,
                ..Default::default()
            }),
            |secs| format!("at {secs}"),
        );
        assert_eq!(many.key, "admin.nest_page.oauth_secret_force_rotate_done");
        assert_eq!(many.args["apps"], "4");
        assert_eq!(many.args["minted"], "at 1690000000");

        let first = session_secret_forced_rotation_verdict(
            Ok(&SessionSecretForcedRotation {
                rotated_at: 3,
                replaced_minted_at: None,
                ..Default::default()
            }),
            |_| unreachable!("no generation to date"),
        );
        assert_eq!(first.key, "admin.nest_page.oauth_secret_force_rotate_first");

        for failed in [
            issuer_key_rotation_verdict(Err("timed out")),
            issuer_key_forced_rotation_verdict(Err("timed out")),
            session_secret_forced_rotation_verdict(Err("timed out"), |_| String::new()),
        ] {
            assert_eq!(failed.key, "admin.nest_page.oauth_rotate_failed");
            assert_eq!(failed.args["cause"], "timed out");
        }
    }

    /// A forced rotation that dropped nothing (a nest that had no key to drop)
    /// must not render an empty "stopped working:" list.
    #[test]
    fn a_forced_rotation_that_dropped_nothing_says_so() {
        let forced = issuer_key_forced_rotation_verdict(Ok(&IssuerKeyForcedRotation {
            kid: "kid-c".into(),
            rotated_at: 2,
            dropped_kids: vec![],
            ..Default::default()
        }));
        assert_eq!(forced.key, "admin.nest_page.oauth_force_rotate_done_none");
        assert_eq!(forced.args["kid"], "kid-c");
    }

    /// The driven, worded doors every app shares — tui and linux in-process,
    /// the rest through the UniFFI and wasm faces: the read folds through
    /// [`issuer_key_view`], and each control dispatches exactly its own kind
    /// and hands back the one sentence `admin-nest-oauth-status` shows.
    #[test]
    fn the_driven_issuer_doors_dispatch_their_own_kind_and_word_the_outcome() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let view = block_on(client.issuer_key_status_view()).expect("infallible");
        assert_eq!(view.active_kid, "new-kid");
        assert_eq!(view.keys.len(), 2);
        assert_eq!(view.keys[1].served_until, Some(1_700_000_000 + 1200));

        let rotated = block_on(client.rotate_issuer_key_verdict());
        assert_eq!(rotated.key, "admin.nest_page.oauth_rotate_done");
        assert_eq!(rotated.args["kid"], "newer-kid");

        let forced = block_on(
            client.force_rotate_verdict(IssuerForcedArm::IssuerKey, |_| {
                unreachable!("the key arm dates nothing")
            }),
        );
        assert_eq!(forced.key, "admin.nest_page.oauth_force_rotate_done");
        assert_eq!(forced.args["kid"], "newest-kid");
        assert_eq!(forced.args["dropped"], "new-kid, old-kid");

        // The clock face is the caller's: the wording is decided here, the
        // rendering of the instant is not.
        let ended = block_on(
            client
                .force_rotate_verdict(IssuerForcedArm::SessionSecret, |secs| format!("at {secs}")),
        );
        assert_eq!(ended.key, "admin.nest_page.oauth_secret_force_rotate_done");
        assert_eq!(ended.args["minted"], "at 1690000000");
        assert_eq!(
            ended.args["apps"], "3",
            "the blast radius reaches the admin as a count, end to end through \
             the driven door rather than only through the fold"
        );

        assert_eq!(
            rec.kinds(),
            vec![
                "fauna.oauth.issuer_key_status",
                "fauna.oauth.rotate_issuer_key",
                "fauna.oauth.force_rotate_issuer_key",
                "fauna.oauth.force_rotate_session_secret",
            ]
        );
    }

    /// A call that never got its reply is a VERDICT, not an `Err`: the shared
    /// fold says the nest did not confirm (never that nothing changed), and a
    /// door that returned the transport error instead would leave every app to
    /// word it — or to put it on `error-message`, which the section forbids.
    /// Each door still tried exactly its own kind, once.
    #[test]
    fn a_failed_issuer_call_is_a_worded_verdict_never_an_error() {
        for (arm, kind) in [
            (None, "fauna.oauth.rotate_issuer_key"),
            (
                Some(IssuerForcedArm::IssuerKey),
                "fauna.oauth.force_rotate_issuer_key",
            ),
            (
                Some(IssuerForcedArm::SessionSecret),
                "fauna.oauth.force_rotate_session_secret",
            ),
        ] {
            let failing = Arc::new(FailingRequester::new("socket closed"));
            let client = AdminClient::new(failing.clone());
            let verdict = match arm {
                None => block_on(client.rotate_issuer_key_verdict()),
                Some(arm) => block_on(
                    client.force_rotate_verdict(arm, |_| unreachable!("a failure dates nothing")),
                ),
            };
            assert_eq!(verdict.key, "admin.nest_page.oauth_rotate_failed", "{kind}");
            assert_eq!(verdict.args["cause"], "socket closed", "{kind}");
            assert_eq!(failing.kinds(), vec![kind]);
        }
    }

    #[test]
    fn admins_add_composes_the_grant_kind() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let queued = block_on(client.admins_add(vec![0x11; 32])).expect("infallible");
        assert_eq!(queued.pending_action_id, 7);
        assert_eq!(queued.status, "pending");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.admins.add");
        let req: AdminAdminAddRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.actor_id.as_ref(), [0x11; 32]);
    }

    #[test]
    fn admins_remove_composes_the_revoke_kind() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let queued = block_on(client.admins_remove(vec![0x22; 32])).expect("infallible");
        assert_eq!(queued.pending_action_id, 7);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.admins.remove");
        let req: AdminAdminRemoveRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.actor_id.as_ref(), [0x22; 32]);
    }

    /// The confirm surface must name *people*, so a resolved handle wins; an
    /// unresolvable one falls back to the canonical short id rather than
    /// dropping the row — an inheritor we cannot name still inherits, and
    /// omitting them would under-state who keeps recovery custody.
    #[test]
    fn the_confirm_view_labels_by_handle_and_falls_back_to_the_short_id() {
        let view = seed_rotation_confirm_view(
            &AdminAdminsListReply {
                admins: vec![entry(0xaa, 10), entry(0xbb, 20)],
                ..Default::default()
            },
            &[(vec![0xaa; 32], "root@fauna.test".to_string())],
        );

        assert!(view.can_confirm);
        assert_eq!(view.blocked_reason, None);
        assert_eq!(
            view.inheritors
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>(),
            vec!["root@fauna.test", "bbbbbbbbbbbb…"],
        );
        assert_eq!(view.inheritors[1].actor_id, vec![0xbb; 32]);
    }

    /// An empty handle is absence, not a name (`account_display_label`'s rule) —
    /// a nest that answers with a blank handle must not paint a blank row.
    #[test]
    fn a_blank_handle_falls_back_to_the_short_id() {
        let view = seed_rotation_confirm_view(
            &AdminAdminsListReply {
                admins: vec![entry(0xcc, 1)],
                ..Default::default()
            },
            &[(vec![0xcc; 32], String::new())],
        );
        assert_eq!(view.inheritors[0].label, "cccccccccccc…");
    }

    /// The ceremony's confirm surface owes the admin the set that will inherit
    /// (`box-recovery.md` § Deployment-seed rotation → *Ordering rule*). A roster
    /// that lists nobody cannot satisfy that — and it is also a lie, since the
    /// caller is themselves an admin — so the confirm is withheld with the reason
    /// on the page rather than dispatching a ceremony we cannot describe.
    #[test]
    fn an_empty_roster_withholds_the_confirm_and_says_why() {
        let view = seed_rotation_confirm_view(&AdminAdminsListReply::default(), &[]);
        assert!(view.inheritors.is_empty());
        assert!(!view.can_confirm);
        assert_eq!(
            view.blocked_reason,
            Some(fauna_core::localized::LocalizedText::key(
                "admin.nest_page.rotate_seed_roster_empty"
            )),
        );
    }

    #[test]
    fn invite_codes_create_composes_kind_and_mint_on_empty_payload() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        // Empty code = mint-on-empty; the reply carries the minted token.
        let reply = block_on(client.invite_codes_create("", "personal", 3, None, None))
            .expect("infallible");
        assert_eq!(reply.code, "MINTED1234");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.invite_codes.create");
        let req: AdminInviteCodeCreateRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.code, "");
        assert_eq!(req.tier, "personal");
        assert_eq!(req.uses, 3);
        assert_eq!(req.guardian_actor, None);
    }

    #[test]
    fn invite_codes_create_composes_guardian_actor_for_supervised_admission() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        block_on(client.invite_codes_create("", "personal", 1, Some(vec![5u8; 32]), None))
            .expect("infallible");

        let (_, payload) = rec.recorded();
        let req: AdminInviteCodeCreateRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.guardian_actor, Some(ByteBuf::from(vec![5u8; 32])));
        assert_eq!(
            req.age_band, None,
            "no band chosen rides as absence, not a token"
        );
    }

    /// The band dial rides as its wire token beside the guardian — typed at
    /// this boundary, so no client ever spells `13-15` itself.
    #[test]
    fn invite_codes_create_composes_the_age_band_token_beside_the_guardian() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        block_on(client.invite_codes_create(
            "",
            "personal",
            1,
            Some(vec![5u8; 32]),
            Some(AgeBand::Teen13To15),
        ))
        .expect("infallible");

        let (_, payload) = rec.recorded();
        let req: AdminInviteCodeCreateRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.guardian_actor, Some(ByteBuf::from(vec![5u8; 32])));
        assert_eq!(req.age_band.as_deref(), Some("13-15"));
    }

    #[test]
    fn invite_requests_approve_composes_the_age_band_token() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        block_on(client.invite_requests_approve(
            42,
            Some("personal".into()),
            None,
            Some(vec![6u8; 32]),
            Some(AgeBand::U13),
        ))
        .expect("infallible");

        let (_, payload) = rec.recorded();
        let req: AdminInviteRequestApproveRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.age_band.as_deref(), Some("u13"));
    }

    #[test]
    fn set_age_verification_required_composes_the_flag() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        block_on(client.set_age_verification_required(true)).expect("infallible");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.set_age_verification_required");
        let req: SetAgeVerificationRequiredRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert!(req.required);
    }

    /// The picker catalog: *not set* first, then the four bands oldest-last,
    /// every value round-tripping through the typed parse — and nothing else.
    #[test]
    fn age_band_options_are_not_set_then_the_ratified_order() {
        let options = age_band_options();
        let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(
            values,
            [AGE_BAND_NOT_SET_VALUE, "u13", "13-15", "16-17", "18+"]
        );
        assert_eq!(
            options[0].label,
            LocalizedText::key("family.age_band.not_set")
        );
        assert_eq!(
            options[2].label,
            LocalizedText::key("family.age_band.teen_13_15")
        );
        assert_eq!(age_band_from_option_value(AGE_BAND_NOT_SET_VALUE), None);
        assert_eq!(
            age_band_from_option_value("16-17"),
            Some(AgeBand::Teen16To17)
        );
        assert_eq!(age_band_from_option_value("Not set"), None);
    }

    /// The request-row select's seed: the applicant's claimed band when this
    /// client can name it, else *not set* — never an unnameable token.
    #[test]
    fn claimed_age_band_option_seeds_a_nameable_claim_else_not_set() {
        assert_eq!(claimed_age_band_option(Some("13-15")), "13-15");
        assert_eq!(claimed_age_band_option(Some("18+")), "18+");
        assert_eq!(claimed_age_band_option(None), AGE_BAND_NOT_SET_VALUE);
        // A newer nest's token this build predates — not nameable.
        assert_eq!(
            claimed_age_band_option(Some("teen")),
            AGE_BAND_NOT_SET_VALUE
        );
        assert_eq!(claimed_age_band_option(Some("")), AGE_BAND_NOT_SET_VALUE);
    }

    #[test]
    fn users_update_composes_change_tier_payload() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        block_on(client.users_update(vec![9u8; 32], "enterprise", "renamed")).expect("infallible");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.users.update");
        let req: AdminUserUpdateRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.actor_id.as_ref(), &[9u8; 32][..]);
        assert_eq!(req.tier, "enterprise");
        assert_eq!(req.label, "renamed");
    }

    #[test]
    fn invite_requests_approve_composes_tier_payload_and_returns_resolved() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let reply =
            block_on(client.invite_requests_approve(42, Some("personal".into()), None, None, None))
                .expect("infallible");
        assert_eq!(reply.handle, "bob@fauna.test");
        assert_eq!(reply.tier, "personal");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.invite_requests.approve");
        let req: AdminInviteRequestApproveRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.id, 42);
        assert_eq!(req.tier.as_deref(), Some("personal"));
        assert_eq!(req.label, None);
        assert_eq!(req.guardian_actor, None);
    }

    #[test]
    fn invite_requests_approve_composes_guardian_actor_for_supervised_admission() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        block_on(client.invite_requests_approve(
            42,
            Some("personal".into()),
            None,
            Some(vec![6u8; 32]),
            None,
        ))
        .expect("infallible");

        let (_, payload) = rec.recorded();
        let req: AdminInviteRequestApproveRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.guardian_actor, Some(ByteBuf::from(vec![6u8; 32])));
    }

    #[test]
    fn logs_composes_kind_and_returns_entries() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let reply = block_on(client.logs()).expect("infallible");
        assert_eq!(reply.entries.len(), 1);
        assert_eq!(reply.entries[0].message, "hi");
        assert_eq!(reply.entries[0].level, admin::AdminLogLevel::Warn);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.logs");
        // The request is parameterless (the client filters severity in its own
        // widget); it must still decode as the request type.
        let _req: AdminLogsRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
    }

    #[test]
    fn stats_composes_kind_and_returns_counters() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let reply = block_on(client.stats()).expect("infallible");
        assert_eq!(reply.total_users, 7);
        assert_eq!(
            reply.users_by_tier,
            vec![("free".into(), 5), ("personal".into(), 2)]
        );
        assert_eq!(reply.suspended_users, 1);
        assert_eq!(reply.ws_connections, 3);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.stats");
        // Parameterless, but must still decode as the request type.
        let _req: AdminStatsRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
    }

    #[test]
    fn status_composes_kind_and_returns_version() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        let reply = block_on(client.status()).expect("infallible");
        assert_eq!(reply.version, "9.9.9");
        assert!(reply.update_available.is_none());

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.status");
        let _req: AdminStatusRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
    }

    #[test]
    fn factory_reset_composes_kind_and_returns_claim_code() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        // `None` ⇒ nest generates the post-reset code; the reply carries it.
        let reply = block_on(client.factory_reset(None)).expect("infallible");
        assert_eq!(reply.claim_code, "ABC123");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.factory_reset");
        let req: admin::FactoryResetRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.new_claim_code, None);
    }

    // ── Row lifecycle controls (admin.md § 2 → Cutting a user off) ───────────

    /// A row in the given eviction state. `None` ⇒ no eviction in flight.
    fn row(eviction_status: Option<&str>, is_admin: bool) -> AdminUser {
        AdminUser {
            eviction: eviction_status.map(|s| AdminEviction {
                status: s.to_string(),
                ..Default::default()
            }),
            is_admin,
            ..Default::default()
        }
    }

    #[test]
    fn active_user_offers_both_entry_points_and_no_restore() {
        let c = admin_user_row_controls(&row(None, false));
        assert_eq!(
            c,
            AdminUserRowControls {
                suspend: true,
                evict: true,
                restore: false,
                make_admin: true,
                remove_admin: false,
            }
        );
    }

    #[test]
    fn suspended_user_offers_only_restore() {
        // `suspend_user_now` matches `eviction_status IN ('', 'warning')`, so a
        // second suspend is a no-op; evict is not an entry point from here.
        // Make-admin still stands — granting the role to a suspended user is
        // deliberate (their own restore control isn't withdrawn by it).
        let c = admin_user_row_controls(&row(Some("suspended"), false));
        assert_eq!(
            c,
            AdminUserRowControls {
                suspend: false,
                evict: false,
                restore: true,
                make_admin: true,
                remove_admin: false,
            }
        );
    }

    /// The transition the doc ratifies and a naive `eviction.is_some() ⇒ restore
    /// only` row would make unreachable: suspending a user who is mid-eviction
    /// promotes them at once *and clears the pending delete*. Without this the
    /// admin would have to fully restore an abuse-in-progress user first.
    #[test]
    fn mid_eviction_warning_user_can_still_be_suspended_immediately() {
        let c = admin_user_row_controls(&row(Some("warning"), false));
        assert_eq!(
            c,
            AdminUserRowControls {
                suspend: true,
                evict: false,
                restore: true,
                make_admin: true,
                remove_admin: false,
            }
        );
    }

    /// The ladder's last state: the nest refuses restore from `deleting`
    /// (`CacheDb::cancel_eviction`), so the row offers no dead button, and no
    /// make-admin either — the role would only hold the deletion. An admin
    /// row that got there keeps remove-admin, which is what completes it.
    #[test]
    fn deleting_row_offers_no_restore_and_no_make_admin() {
        let c = admin_user_row_controls(&row(Some("deleting"), false));
        assert_eq!(c, AdminUserRowControls::default());
        let c = admin_user_row_controls(&row(Some("deleting"), true));
        assert_eq!(
            c,
            AdminUserRowControls {
                remove_admin: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn admin_row_offers_no_cut_off_control() {
        // The nest answers `fauna.admin.conflict` on both, so offering either
        // would be a dead button (admin.md § 2: remove the role first). It does
        // offer remove-admin, never make-admin (already admin).
        let c = admin_user_row_controls(&row(None, true));
        assert_eq!(
            c,
            AdminUserRowControls {
                suspend: false,
                evict: false,
                restore: false,
                make_admin: false,
                remove_admin: true,
            }
        );
    }

    /// Granting the admin role to an already-suspended user must not strand
    /// them: restore stays reachable (`common.md` § Client-state recoverability).
    #[test]
    fn suspended_admin_can_still_be_restored() {
        let c = admin_user_row_controls(&row(Some("suspended"), true));
        assert!(c.restore, "restore must survive the admin guard");
        assert!(!c.suspend);
        assert!(!c.evict);
        assert!(
            c.remove_admin,
            "remove-admin ignores lifecycle, same as restore"
        );
        assert!(!c.make_admin);
    }

    /// Forward-compat: a nest that adds an eviction state this client doesn't
    /// know must not make the row *offer* an entry point it might refuse.
    #[test]
    fn unknown_eviction_state_is_treated_as_a_cut_off_in_flight() {
        assert_eq!(
            admin_user_lifecycle(&row(Some("some_future_state"), false)),
            AdminUserLifecycle::EvictionPending
        );
        let c = admin_user_row_controls(&row(Some("some_future_state"), false));
        assert_eq!(
            c,
            AdminUserRowControls {
                suspend: false,
                evict: false,
                restore: true,
                make_admin: true,
                remove_admin: false,
            }
        );
    }

    /// An absent `is_admin` (a current nest's non-admin row) decodes to `false`. Here we pin
    /// what that *means* for the row: it offers the controls, and the nest's own
    /// `fauna.admin.conflict` surfaces in `admin-users-action-error`. Degraded
    /// UX, never a wrong authorization decision — the nest is the sole enforcer.
    #[test]
    fn default_is_admin_false_degrades_to_offering_the_controls() {
        let c = admin_user_row_controls(&AdminUser::default());
        assert!(c.suspend && c.evict);
    }

    // ── Picker option text (admin.md § 2 → What identifies a user in an
    // admin picker) — the one shared owner behind tui/linux (Rust-native)
    // and android/web (over FFI/wasm). ────────────────────────────────────

    #[test]
    fn admin_picker_option_prefers_the_handle_over_the_label() {
        let u = AdminUser {
            actor_id: ByteBuf::from(vec![0x11; 32]),
            label: "Alex".to_string(),
            handle: Some("alex99".to_string()),
            ..Default::default()
        };
        assert_eq!(admin_picker_option(&u), "alex99");
    }

    #[test]
    fn admin_picker_option_falls_back_to_full_hex_when_handle_absent() {
        let u = AdminUser {
            actor_id: ByteBuf::from(vec![0x22; 32]),
            label: "Bob".to_string(),
            handle: None,
            ..Default::default()
        };
        assert_eq!(
            admin_picker_option(&u),
            fauna_core::format::hex_full(&[0x22; 32])
        );
    }

    #[test]
    fn admin_picker_option_falls_back_to_full_hex_when_handle_is_empty() {
        // A handle-less account's stored empty string folds to
        // `None` before it ever reaches a client — but the fallback holds
        // either way, so a stray empty string can't slip an empty option in.
        let u = AdminUser {
            actor_id: ByteBuf::from(vec![0x33; 32]),
            handle: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(
            admin_picker_option(&u),
            fauna_core::format::hex_full(&[0x33; 32])
        );
    }

    /// The bug this fix addresses: two users
    /// sharing a display LABEL but distinct handles must produce two DISTINCT
    /// option strings —
    /// the DNS catch-all/role-address and web apex build sites had drifted
    /// onto raw `u.label.clone()`, which would collapse both users to the
    /// same "Alex" option, indistinguishable to the human picking between
    /// them even though an index-aligned app's binding stays correct either
    /// way (`admin.md` § 2's two-halves rule).
    #[test]
    fn actor_picker_options_stays_injective_when_labels_collide() {
        let alex = |byte: u8, handle: &str| AdminUser {
            actor_id: ByteBuf::from(vec![byte; 32]),
            label: "Alex".to_string(),
            handle: Some(handle.to_string()),
            ..Default::default()
        };
        let users = [alex(1, "alex"), alex(2, "alex2")];
        assert_eq!(
            actor_picker_options(&users),
            vec![
                (vec![1u8; 32], "alex".to_string()),
                (vec![2u8; 32], "alex2".to_string()),
            ],
        );
    }

    #[test]
    fn registration_mode_options_are_wire_values_in_the_ratified_order() {
        let opts = registration_mode_options();
        let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, ["open", "invite_required", "closed"]);
    }

    #[test]
    fn registration_mode_options_round_trip_through_from_wire_str() {
        for opt in registration_mode_options() {
            let mode = RegistrationMode::from_wire_str(&opt.value)
                .expect("every option's value must be a valid wire string");
            assert_eq!(mode.as_wire_str(), opt.value);
        }
    }

    #[test]
    fn registration_mode_options_label_keys_are_distinct() {
        let opts = registration_mode_options();
        let keys: std::collections::BTreeSet<&str> =
            opts.iter().map(|o| o.label.key.as_str()).collect();
        assert_eq!(keys.len(), opts.len());
    }

    // ── log_entry_from_wire ──────────────────────────────────────────────────

    fn wire_entry(level: admin::AdminLogLevel, timestamp_ms: i64) -> admin::AdminLogEntry {
        admin::AdminLogEntry {
            timestamp_ms,
            level,
            target: "fauna_nest::serve".to_string(),
            message: "listening".to_string(),
            extra: Default::default(),
        }
    }

    /// Every wire level maps to its own render level — a collapsed arm would
    /// silently mis-colour a row and break the severity filter's narrowing.
    #[test]
    fn log_entry_from_wire_maps_every_level() {
        let cases = [
            (admin::AdminLogLevel::Error, fauna_log::LogLevel::Error),
            (admin::AdminLogLevel::Warn, fauna_log::LogLevel::Warn),
            (admin::AdminLogLevel::Info, fauna_log::LogLevel::Info),
            (admin::AdminLogLevel::Debug, fauna_log::LogLevel::Debug),
            (admin::AdminLogLevel::Trace, fauna_log::LogLevel::Trace),
        ];
        for (wire, expected) in cases {
            assert_eq!(
                log_entry_from_wire(&wire_entry(wire, 1_700_000_000_000)).level,
                expected
            );
        }
    }

    #[test]
    fn log_entry_from_wire_carries_target_message_and_timestamp() {
        let got = log_entry_from_wire(&wire_entry(admin::AdminLogLevel::Info, 1_700_000_000_000));
        assert_eq!(got.timestamp_ms, 1_700_000_000_000);
        assert_eq!(got.target, "fauna_nest::serve");
        assert_eq!(got.message, "listening");
    }

    /// A negative wire timestamp floors to 0 instead of wrapping through the
    /// `u64` cast into the year ~584 billion, which would sort the row to the
    /// top of a newest-first view forever.
    #[test]
    fn log_entry_from_wire_floors_negative_timestamp() {
        let got = log_entry_from_wire(&wire_entry(admin::AdminLogLevel::Warn, -1));
        assert_eq!(got.timestamp_ms, 0);
    }

    // ── The web-app origin ────────────────────────────────────────────────────

    fn origin_reply(mode: WebAppOrigin, domain: Option<&str>) -> AdminWebAppOriginGetReply {
        AdminWebAppOriginGetReply::project(mode, domain)
    }

    #[test]
    fn bundled_marks_the_bundled_radio_and_says_what_the_address_serves() {
        let view = admin_web_app_origin_view(Some(&origin_reply(WebAppOrigin::Bundled, None)));
        assert_eq!(view.selected, Some(WebAppOrigin::Bundled));
        assert!(view.can_set);
        assert_eq!(
            view.status.key,
            "admin.nest_page.web_app_origin_status_bundled"
        );
        // The never-set default is exactly this state, never a blank line.
        assert_eq!(AdminWebAppOriginView::default(), view);
    }

    #[test]
    fn central_names_the_nests_exact_target_verbatim() {
        let reply = origin_reply(WebAppOrigin::Central, Some("example.org"));
        let view = admin_web_app_origin_view(Some(&reply));
        assert_eq!(view.selected, Some(WebAppOrigin::Central));
        assert_eq!(
            view.status.key,
            "admin.nest_page.web_app_origin_status_central"
        );
        assert_eq!(
            view.status.args.get("target").map(String::as_str),
            Some("https://app.fauna.social/app/?nest=example.org"),
        );
    }

    #[test]
    fn a_domainless_central_box_says_it_keeps_serving_bundled() {
        let view = admin_web_app_origin_view(Some(&origin_reply(WebAppOrigin::Central, None)));
        // The choice is still central — the radio shows what the admin chose.
        assert_eq!(view.selected, Some(WebAppOrigin::Central));
        assert!(view.can_set);
        assert_eq!(
            view.status.key,
            "admin.nest_page.web_app_origin_status_domainless"
        );
    }

    #[test]
    fn a_nest_predating_the_choice_cannot_set_it_and_marks_no_radio() {
        let view = admin_web_app_origin_view(None);
        assert_eq!(view.selected, None);
        assert!(!view.can_set);
        assert_eq!(
            view.status.key,
            "admin.nest_page.web_app_origin_status_predates"
        );
    }

    #[test]
    fn a_newer_nests_mode_renders_read_only() {
        let reply = AdminWebAppOriginGetReply {
            mode: "elsewhere".into(),
            ..Default::default()
        };
        let view = admin_web_app_origin_view(Some(&reply));
        assert_eq!(view.selected, None);
        assert!(!view.can_set);
        assert_eq!(
            view.status.args.get("mode").map(String::as_str),
            Some("elsewhere")
        );
    }

    #[test]
    fn every_state_carries_the_scope_sentence_naming_the_central_origin() {
        for reply in [
            None,
            Some(origin_reply(WebAppOrigin::Bundled, None)),
            Some(origin_reply(WebAppOrigin::Central, Some("example.org"))),
        ] {
            let view = admin_web_app_origin_view(reply.as_ref());
            assert_eq!(view.scope.key, "admin.nest_page.web_app_origin_scope");
            assert_eq!(
                view.scope.args.get("origin").map(String::as_str),
                Some(CENTRAL_APP_ORIGIN)
            );
        }
    }

    #[test]
    fn the_read_maps_unknown_kind_to_predates_and_keeps_other_refusals() {
        use fauna_client_testkit::RejectingRequester;
        use fauna_protocol::RpcError;
        let old = RejectingRequester::new().reject(
            "fauna.admin.web_app_origin.get",
            RpcError::new("fauna.protocol.unknown_kind", "error.protocol.unknown_kind"),
        );
        assert_eq!(block_on(AdminClient::new(old).web_app_origin()), Ok(None));

        let refused = RejectingRequester::new().reject(
            "fauna.admin.web_app_origin.get",
            RpcError::new("fauna.auth.forbidden", "error.auth.forbidden"),
        );
        assert!(block_on(AdminClient::new(refused).web_app_origin()).is_err());

        let current = RejectingRequester::new().reply(
            "fauna.admin.web_app_origin.get",
            &origin_reply(WebAppOrigin::Bundled, None),
        );
        assert_eq!(
            block_on(AdminClient::new(current).web_app_origin()),
            Ok(Some(origin_reply(WebAppOrigin::Bundled, None)))
        );
    }

    #[test]
    fn the_set_sends_the_typed_mode_and_returns_the_projection() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());
        let got = block_on(client.set_web_app_origin(WebAppOrigin::Central)).expect("infallible");
        assert_eq!(got.mode, "central");
        let (kind, bytes) = rec.recorded();
        assert_eq!(kind, "fauna.admin.web_app_origin.set");
        let sent: AdminWebAppOriginSetRequest = fauna_protocol::decode_strict(&bytes).unwrap();
        assert_eq!(sent.mode, WebAppOrigin::Central);
    }

    // ── The declared region ───────────────────────────────────────────────────

    fn region(code: &str) -> RegionCode {
        RegionCode::parse(code).expect("test code is well-formed")
    }

    fn document(authority: &str, sequence: u64) -> RegionDocumentRef {
        RegionDocumentRef {
            region: region("NO"),
            authority_name: authority.into(),
            key_id: "k1".into(),
            sequence,
            issued_at: 1_700_000_000,
            extra: Default::default(),
        }
    }

    /// The fresh-install state is a NORMAL state: a status line, no authority
    /// line (there is no channel to describe), no warning, and nothing to
    /// withdraw. If this ever renders as an error, an admin is being told a
    /// conforming deployment is broken.
    #[test]
    fn undeclared_region_is_a_normal_state_not_an_error() {
        let view = admin_region_view(&AdminRegionStatusReply::default());
        assert_eq!(view.declared, None);
        assert_eq!(view.status.key, "admin.nest_page.region_none");
        assert_eq!(view.authority, None);
        assert_eq!(view.staleness, None);
        assert!(!view.can_withdraw);
    }

    /// The everywhere-today answer: declared, no enrolled authority. Accepted
    /// and inert — the view says so plainly rather than refusing or warning.
    #[test]
    fn declared_but_unenrolled_region_says_so_plainly() {
        let view = admin_region_view(&AdminRegionStatusReply {
            declared: Some(region("NO")),
            enrolled: false,
            ..Default::default()
        });
        assert_eq!(view.declared.as_deref(), Some("NO"));
        assert_eq!(view.status.key, "admin.nest_page.region_declared");
        assert_eq!(
            view.status.args.get("region").map(String::as_str),
            Some("NO")
        );
        assert_eq!(
            view.authority.as_ref().map(|t| t.key.as_str()),
            Some("admin.nest_page.region_not_enrolled")
        );
        assert!(
            view.staleness.is_none(),
            "an unfetched channel that does not exist is not stale"
        );
        assert!(view.can_withdraw);
    }

    /// Enrolled but nothing published yet is its OWN line — collapsing it into
    /// "no authority" would tell an admin no authority exists when one does,
    /// and collapsing it into the document line would name a version that has
    /// never been issued.
    #[test]
    fn enrolled_without_a_document_is_its_own_line() {
        let view = admin_region_view(&AdminRegionStatusReply {
            declared: Some(region("EU")),
            enrolled: true,
            ..Default::default()
        });
        assert_eq!(
            view.authority.as_ref().map(|t| t.key.as_str()),
            Some("admin.nest_page.region_enrolled_no_document")
        );
    }

    /// The document in force names its authority and version from the stored
    /// ref, so a de-listing cannot turn a binding document into "no region".
    #[test]
    fn a_document_in_force_names_its_authority_and_version() {
        let view = admin_region_view(&AdminRegionStatusReply {
            declared: Some(region("NO")),
            enrolled: true,
            feature_policy: Some(document("Datatilsynet", 7)),
            ..Default::default()
        });
        let authority = view
            .authority
            .expect("declared regions carry an authority line");
        assert_eq!(authority.key, "admin.nest_page.region_document");
        assert_eq!(
            authority.args.get("authority").map(String::as_str),
            Some("Datatilsynet")
        );
        assert_eq!(
            authority.args.get("sequence").map(String::as_str),
            Some("7")
        );
    }

    /// A document survives its authority's de-listing (`enrolled: false` while
    /// `feature_policy` is `Some`) — the ruling that keeps a bound visible. The
    /// document line must win over the not-enrolled line, or the admin is shown
    /// "no rules apply" while rules are being enforced.
    #[test]
    fn a_document_outranks_a_lost_enrolment() {
        let view = admin_region_view(&AdminRegionStatusReply {
            declared: Some(region("NO")),
            enrolled: false,
            feature_policy: Some(document("Datatilsynet", 2)),
            ..Default::default()
        });
        assert_eq!(
            view.authority.as_ref().map(|t| t.key.as_str()),
            Some("admin.nest_page.region_document")
        );
    }

    /// An unreadable declaration is corruption, not absence: its own status
    /// line (worded with the recovery — this screen's controls are the in-app
    /// fix), the still-binding document still named, and withdraw offered
    /// because the corrupt row exists to withdraw.
    #[test]
    fn an_unreadable_declaration_is_distinguished_from_absence() {
        let view = admin_region_view(&AdminRegionStatusReply {
            declaration_unreadable: true,
            feature_policy: Some(document("Datatilsynet", 7)),
            ..Default::default()
        });
        assert_eq!(view.declared, None, "garbage is not offered for re-seed");
        assert_eq!(view.status.key, "admin.nest_page.region_unreadable");
        let authority = view
            .authority
            .expect("a binding document must stay visible — identity included");
        assert_eq!(authority.key, "admin.nest_page.region_document");
        assert_eq!(
            authority.args.get("sequence").map(String::as_str),
            Some("7")
        );
        assert!(
            view.can_withdraw,
            "withdrawing the corrupt row is a recovery"
        );
    }

    /// …and with no document ever accepted, the corruption is reported and
    /// nothing else — no authority line is invented.
    #[test]
    fn an_unreadable_declaration_without_a_document_reports_only_the_corruption() {
        let view = admin_region_view(&AdminRegionStatusReply {
            declaration_unreadable: true,
            ..Default::default()
        });
        assert_eq!(view.status.key, "admin.nest_page.region_unreadable");
        assert_eq!(view.authority, None);
        assert!(view.can_withdraw);
    }

    /// Staleness is the nest's own judgement, rendered only when it says so —
    /// never re-derived from `last_checked_at`, which would make seven apps
    /// disagree with the nest about when a channel has gone quiet.
    #[test]
    fn staleness_renders_only_when_the_nest_reports_it() {
        let mut reply = AdminRegionStatusReply {
            declared: Some(region("NO")),
            enrolled: true,
            feature_policy: Some(document("Datatilsynet", 1)),
            last_checked_at: Some(1),
            ..Default::default()
        };
        assert!(
            admin_region_view(&reply).staleness.is_none(),
            "an ancient last_checked_at is not staleness — the nest decides"
        );
        reply.stale = true;
        assert_eq!(
            admin_region_view(&reply).staleness.map(|t| t.key),
            Some("admin.nest_page.region_stale".to_string())
        );
    }

    /// Case is NOT folded: a helpfully-upcasing client would make two spellings
    /// of one region both storable and the nest's key ambiguous.
    #[test]
    fn region_input_is_validated_and_never_case_folded() {
        assert_eq!(
            parse_region_code("NO").map(|c| c.as_str().to_string()),
            Ok("NO".into())
        );
        assert_eq!(
            parse_region_code("  EU  ").map(|c| c.as_str().to_string()),
            Ok("EU".into())
        );
        for bad in ["no", "No", "N", "TOOLONGCODE", "N-O", ""] {
            assert_eq!(
                parse_region_code(bad),
                Err("admin.nest_page.region_invalid"),
                "{bad:?} must be refused client-side, not sent to the nest"
            );
        }
    }

    /// The withdraw is the ABSENT case on the wire, not a sentinel string — and
    /// the request must decode as the real type, so a client cannot invent a
    /// "none" region code the nest would reject as malformed.
    #[test]
    fn withdraw_sends_an_absent_region_not_a_sentinel() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = AdminClient::new(rec.clone());

        block_on(client.set_region(None)).expect("infallible");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.admin.region.set");
        let req: AdminRegionSetRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.region, None);

        block_on(client.set_region(Some(region("NO")))).expect("infallible");
        let (_, payload) = rec.recorded();
        let req: AdminRegionSetRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.region, Some(region("NO")));
    }

    // ── users_list_all ───────────────────────────────────────────────────────

    /// One encoded `fauna.admin.users.list` reply holding an account per `n` in
    /// `accounts` (actor id = `n` big-endian, repeated to 32 bytes).
    fn users_page(accounts: std::ops::Range<u16>, total: i64) -> Vec<u8> {
        let users = accounts
            .map(|n| AdminUser {
                actor_id: ByteBuf::from(n.to_be_bytes().repeat(16)),
                ..Default::default()
            })
            .collect();
        fauna_protocol::encode_canonical(&AdminUsersListReply {
            users,
            total,
            ..Default::default()
        })
        .expect("encode users page")
        .to_vec()
    }

    fn account_number(user: &AdminUser) -> u16 {
        let id = user.actor_id.to_vec();
        u16::from_be_bytes([id[0], id[1]])
    }

    /// A nest past one page: the read asks for the largest page the nest answers,
    /// resumes where each page ended, and stops exactly at `total` — so the
    /// oldest accounts, which a single page drops, all come back (`admin.md` § 2
    /// → *Which accounts a picker offers*).
    #[test]
    fn users_list_all_pages_at_the_nest_ceiling_until_total() {
        let rec = Arc::new(fauna_client_testkit::ScriptedRequester::new([
            users_page(0..500, 620),
            users_page(500..620, 620),
        ]));
        let users = block_on(users_list_all(&AdminClient::new(rec.clone()))).expect("infallible");

        assert_eq!(
            users.iter().map(account_number).collect::<Vec<_>>(),
            (0..620).collect::<Vec<_>>(),
            "every account, the oldest included, in the nest's order"
        );
        assert_eq!(rec.remaining(), 0, "no request past total");
        let asked: Vec<(Option<i64>, i64)> = rec
            .payloads()
            .iter()
            .map(|payload| {
                let req: AdminUsersListRequest =
                    fauna_protocol::decode_strict(payload).expect("a users.list request");
                (req.limit, req.offset)
            })
            .collect();
        assert_eq!(
            asked,
            vec![
                (Some(admin::USERS_LIST_MAX_LIMIT), 0),
                (Some(admin::USERS_LIST_MAX_LIMIT), 500),
            ]
        );
        assert_eq!(rec.kinds(), vec!["fauna.admin.users.list"; 2]);
    }

    /// A page repeating an account an earlier page returned (ties shuffling on a
    /// nest without the insertion-order tiebreak) yields it once, and a page that
    /// adds no new account ends the read — a nest ignoring `offset` cannot spin it.
    #[test]
    fn users_list_all_skips_a_repeated_account_and_stops_when_a_page_adds_nothing() {
        let rec = Arc::new(fauna_client_testkit::ScriptedRequester::new([
            users_page(0..3, 10),
            users_page(2..5, 10),
            users_page(2..5, 10),
        ]));
        let users = block_on(users_list_all(&AdminClient::new(rec.clone()))).expect("infallible");

        assert_eq!(
            users.iter().map(account_number).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(
            rec.remaining(),
            0,
            "the page adding nothing new ended the read"
        );
    }

    /// An empty page ends the read even while `total` still counts more (an
    /// account deleted between requests).
    #[test]
    fn users_list_all_stops_on_an_empty_page() {
        let rec = Arc::new(fauna_client_testkit::ScriptedRequester::new([
            users_page(0..2, 5),
            users_page(0..0, 5),
        ]));
        let users = block_on(users_list_all(&AdminClient::new(rec.clone()))).expect("infallible");

        assert_eq!(users.len(), 2);
        assert_eq!(rec.remaining(), 0);
    }

    /// A failed read is an error — never an empty account list a picker would
    /// render as nobody to pick.
    #[test]
    fn users_list_all_surfaces_a_failed_read() {
        let client = AdminClient::new(FailingRequester::new("nest unreachable"));
        let err = block_on(users_list_all(&client)).expect_err("a failed read is an error");
        assert!(err.to_string().contains("nest unreachable"), "{err}");
    }
}
