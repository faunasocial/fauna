//! Typed-call wrapper for the Layer-3 Bridge Management WS-RPC kinds —
//! the user-facing `fauna.bridges.*` surface clients hit from the
//! bridges page (list / link / unlink / settings / follows / feeds).
//!
//! Surface grows as per-endpoint slices land (tracked internally). T1b
//! shipped `list`; T2 adds `set_settings` + `list_follows`; T5 adds the
//! cross-bridge `feeds.*` CRUD.
//!
//! Pattern: same shape as the per-feature client wrappers documented
//! at `fauna_client::client::NestClient::with_registry` — a thin
//! `pub struct BridgesClient { nest: Arc<NestClient> }`, one async
//! method per kind, no state machine.

use fauna_protocol::ByteBuf;
use fauna_protocol::RpcErrorClass;
use fauna_protocol::RpcRequester;
use fauna_protocol::Value;
use fauna_protocol::bridge_routing::{
    AbortPrimaryDomainRenameReply, AbortPrimaryDomainRenameRequest, AddListMemberReply,
    AddListMemberRequest, AddLocalDomainReply, AddLocalDomainRequest,
    BatchImportListMembersRequest, CompletePrimaryDomainRenameReply,
    CompletePrimaryDomainRenameRequest, CreateAccountAliasReply, CreateAccountAliasRequest,
    CreateAccountListReply, CreateAccountListRequest, CreateForwarderReply, CreateForwarderRequest,
    DeleteAccountAliasReply, DeleteAccountAliasRequest, DeleteAccountListReply,
    DeleteAccountListRequest, DeleteForwarderReply, DeleteForwarderRequest, DmarcMode,
    EnableAccountAliasReply, EnableAccountAliasRequest, ExtendPrimaryDomainRenameGraceReply,
    ExtendPrimaryDomainRenameGraceRequest, ForceRotateDkimReply, ForceRotateDkimRequest,
    GenerateDisposableAliasRequest, GetForwardAllToReply, GetForwardAllToRequest,
    GetForwardPerHourReply, GetForwardPerHourRequest, GetPrimaryDomainRenameStatusReply,
    GetPrimaryDomainRenameStatusRequest, GetSpamBaselineStateReply, GetSpamBaselineStateRequest,
    GetSpamThresholdOverrideReply, GetSpamThresholdOverrideRequest, ImportAccountAliasesReply,
    ImportAccountAliasesRequest, ImportAliasOutcome, ListAccountAliasHitsReply,
    ListAccountAliasHitsRequest, ListAccountAliasesReply, ListAccountAliasesRequest,
    ListAccountListsReply, ListAccountListsRequest, ListForwardersReply, ListForwardersRequest,
    ListListMembersRequest, ListLocalDomainsReply, ListLocalDomainsRequest,
    ListPrimaryDomainRenamesReply, ListPrimaryDomainRenamesRequest, ListSpamTrainingHistoryReply,
    ListSpamTrainingHistoryRequest, ProvisionSelfSignedCertReply, ProvisionSelfSignedCertRequest,
    PublishSpamBaselineReply, PublishSpamBaselineRequest, PutPolicyReply, RemoveLocalDomainReply,
    RemoveLocalDomainRequest, ResetSpamModelReply, ResetSpamModelRequest, RestoreLocalDomainReply,
    RestoreLocalDomainRequest, RestoreRealTlsCertReply, RestoreRealTlsCertRequest,
    ResubscribeListMemberReply, ResubscribeListMemberRequest, RevokeAccountAliasReply,
    RevokeAccountAliasRequest, RoleAddressKind, SetBaselineContributionReply,
    SetBaselineContributionRequest, SetCatchAllActorReply, SetCatchAllActorRequest,
    SetForwardAllToReply, SetForwardAllToRequest, SetForwardPerHourReply, SetForwardPerHourRequest,
    SetRoleAddressReply, SetRoleAddressRequest, SetSpamThresholdOverrideReply,
    SetSpamThresholdOverrideRequest, SpamHistoryOp, StartPrimaryDomainRenameReply,
    StartPrimaryDomainRenameRequest, UnsubscribeListMemberReply, UnsubscribeListMemberRequest,
    UpdateAccountAliasReply, UpdateAccountAliasRequest, UpdateAccountListReply,
    UpdateAccountListRequest, UpdateLocalDomainConfigReply, UpdateLocalDomainConfigRequest,
};
use fauna_protocol::bridge_routing::{
    BlocklistSelfCheckRunReply, BlocklistSelfCheckRunRequest, MailHealthReply, MailHealthRequest,
    OutboundWarmupResetRequest, OutboundWarmupStatusReply, RunDeliverabilityDiagnosticsReply,
    RunDeliverabilityDiagnosticsRequest,
};
use fauna_protocol::bridge_routing::{
    ListListSendHistoryReply, ListListSendHistoryRequest, ListSendHistoryRow, SendListMessageReply,
    SendListMessageRequest,
};
// `AliasControls` + `AliasRow` come in via the `pub use` re-export below
// (used internally here *and* re-exported as the crate's typed surface).
use fauna_protocol::bridges_ui::{
    AddFollowReply, AddFollowRequest, CreateFeedReply, CreateFeedRequest, DeleteFeedReply,
    DeleteFeedRequest, LinkChallengeReply, LinkChallengeRequest, LinkReply, LinkRequest,
    ListBridgesReply, ListBridgesRequest, ListFeedsReply, ListFeedsRequest,
    ListFollowRequestsReply, ListFollowRequestsRequest, ListFollowsReply, ListFollowsRequest,
    RemoveFollowReply, RemoveFollowRequest, ResolveFollowRequestReply, ResolveFollowRequestRequest,
    SetSettingsReply, SetSettingsRequest, UnlinkReply, UnlinkRequest,
};
use fauna_protocol::wrapped_blob::{
    ApprovePendingBridgeReply, ApprovePendingBridgeRequest, FetchBridgePubkeyReply,
    FetchBridgePubkeyRequest, FetchSpamModelReply, FetchSpamModelRequest,
    GetSpamScoringPolicyRequest, ListDkimSelectorsReply, ListDkimSelectorsRequest,
    ListPendingBridgesReply, ListPendingBridgesRequest, ListServiceUsersReply,
    ListServiceUsersRequest, ProvisionReply, ProvisionTlsCertBlobRequest, PutSpamModelOutcome,
    PutSpamModelReply, PutSpamModelRequest, RejectPendingBridgeReply, RejectPendingBridgeRequest,
    RevokeDkimBlobRequest, RevokeReply, RevokeServiceUserReply, RevokeServiceUserRequest,
    SetAutoEnableMailForNewUsersReply, SetAutoEnableMailForNewUsersRequest, SetCalDavEnabledReply,
    SetCalDavEnabledRequest, SetCaldavPortReply, SetCaldavPortRequest, SetCardDavEnabledReply,
    SetCardDavEnabledRequest, SetMailEnabledReply, SetMailEnabledRequest, SetWebDavEnabledReply,
    SetWebDavEnabledRequest, SpamModelHolderCopy,
};
// The auto-enable-mail-for-new-users knob is a *client-read* deployment default
// surfaced on `fauna.setup.status` (not in `FetchConfigReply`); the admin read
// twin for the `admin-mail` toggle reads it from there.
use fauna_protocol::discovery::{SetupStatusReply, SetupStatusRequest};

// Re-exported so a client reads the DKIM deliverability surface
// (`DkimSelectorInfo`, the bridge-pubkey reply, the service-user list, the
// pending-approval feed) through this wrapper crate's typed surface without
// depending on `fauna-protocol`.
pub use fauna_protocol::wrapped_blob::{
    DkimSelectorInfo, FetchBridgePubkeyReply as BridgePubkey, GetSpamScoringPolicyReply,
    ListPendingBridgesReply as PendingBridges, ServiceUserInfo,
};

pub use fauna_protocol::bridges_ui;
// Re-exported so a client builds `AliasControls` / reads `AliasRow` /
// `GenerateDisposableAliasReply` through this wrapper crate (its typed
// surface) without depending on `fauna-protocol` directly.
pub use fauna_protocol::bridge_routing::{
    AliasControls, AliasHitRow, AliasRow, GenerateDisposableAliasReply,
};
// The `mail-lists` / `mail-list-members` wire rows, re-exported so the shared
// `fauna-client-mail-settings` seams project them into their view types without
// depending on `fauna-protocol` directly (same posture as `AliasRow` above).
pub use fauna_protocol::bridge_routing::{
    BatchImportListMembersReply, ListListMembersReply, MailListMemberRow, MailListRow,
};
// Re-exported so a client builds the per-sub-struct policy override payloads
// (`PutSpamPolicyRequest` etc.) through this wrapper crate's typed surface
// without depending on `fauna-protocol` directly (A3 Bucket B).
pub use fauna_protocol::bridge_routing::{
    PutAuthPolicyRequest, PutImapPolicyRequest, PutOutboundPolicyRequest, PutSpamPolicyRequest,
    PutSubmissionPolicyRequest,
};
// The nest-side alias-policy override payload + its admin read twin
// (`get_alias_policy` → the effective `AliasPolicy`; tracked internally).
pub use fauna_protocol::bridge_routing::{
    AliasPolicy, GetAliasPolicyRequest, PutAliasPolicyRequest,
};
// The admin read twin of `fetch_config`: `get_mail_config` returns the overlaid
// effective `FetchConfigReply` so the `admin-mail` policy form hydrates before
// edit (see `MailAdminClient::get_mail_config`).
pub use fauna_protocol::bridge_routing::{FetchConfigReply, GetMailConfigRequest};

pub mod atproto;
pub mod atproto_credential;
pub mod atproto_delegation;
pub mod conversations;
pub mod credential_id;
pub mod follow_requests;
pub mod labels;
pub mod link_block;
pub mod nostr_settings;
pub mod settings;
pub use atproto::AtprotoSettingsClient;
pub use credential_id::derive_credential_id;
pub use follow_requests::{FOLLOW_REQUEST_HANDLE_KEY, FollowRequestRow, follow_request_row};
pub use labels::{
    NOSTR_LINK_MODE_GENERATE, NOSTR_LINK_MODE_IMPORT, NOSTR_LINK_MODE_REMOTE, NOSTR_LINK_MODES,
    follow_display, nostr_key_source_label, nostr_link_mode_label,
};
pub use link_block::{LinkBlock, applicable_modes, link_block, link_block_of, mode_applies};
pub use nostr_settings::{BridgeToggleOption, nostr_content_toggle_options};
pub use settings::{RELAY_LIST_KEY, bool_setting, relay_list_setting};

// ── Content-processor holder discovery ──────────────────────────────
//
// Lifted here from `fauna-client-pair` (2026-07-19) so the ONE discovery
// implementation is shared by every consumer of this crate's `MailAdminClient`
// (priority #2): pair's linked-nests mint AND the mail-settings rotation-heal
// driver. `fauna-client-pair` re-exports these; it already depends on this
// crate, so the lift adds no dependency cycle (this crate never points back at
// pair). `HolderInfo` needs to be nameable by the mail-settings `NestClient`
// trait, which lives outside that crate's optional `rpc-glue` feature — hence
// the home in this always-on, wasm-clean crate rather than the optional pair dep.

/// A discovered content-processor holder on a nest — the seal target a grant is
/// minted to. `pubkey` is the X25519 key (`fetch_bridge_pubkey`); `role` /
/// `bridge_id` are the service-user metadata (`list_service_users`). Not an FFI
/// type — internal to a machine's discovery step (`nests.md:106`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderInfo {
    /// The 32-byte X25519 pubkey the grant is HPKE-sealed to.
    pub pubkey: [u8; 32],
    /// The holder's published ML-KEM-768 encapsulation key (1184 B) from
    /// `fetch_bridge_pubkey`, or `None` for a classical-only holder. Present ⇒ the
    /// mint wraps the grant X-Wing to `from_parts(mlkem_ek, pubkey)` (PQ-CAP-3);
    /// absent ⇒ the classical wrap (`post-quantum.md` § Capability-grant holders).
    pub mlkem_ek: Option<Vec<u8>>,
    /// `"mda"` / content-processor role.
    pub role: String,
    pub bridge_id: String,
}

/// Error from [`discover_holders`]: a transport failure classified
/// transient-vs-rejection (the classifier every app wraps a nest call with), or
/// a nest that returned a malformed holder pubkey — which is a *rejection*,
/// since the nest reached a decision and answered with something unusable.
///
/// The shared [`fauna_protocol::NestSeamError`], under this crate's own name.
/// Until 2026-08-23 it was a hand-rolled twin (`Display`/`Error` written out to
/// keep this deliberately-light crate off `thiserror`) whose two spellings had
/// to agree with three sibling copies by hand; the alias costs no dependency —
/// `fauna-protocol` is already this crate's, and it owns the `thiserror` derive.
/// Consumers that used to map this into their own nest-error type (pair, mail
/// settings) now share the very same type, so those `From` impls are gone.
pub use fauna_protocol::NestSeamError as DiscoverHoldersError;

/// Classify a transport error into [`DiscoverHoldersError`].
fn discover_holders_error<E: RpcErrorClass + core::fmt::Display>(e: E) -> DiscoverHoldersError {
    fauna_protocol::nest_seam_error(e)
}

/// The service-user roles that *read* content and are therefore grant holders
/// (`BridgeRole::ContentProcessor` — the web-serve paywall holder and any future
/// scorer/indexer all share this one role; `nests.md` § Mint). The MTA is
/// **excluded**: it holds an X25519 key for DKIM/TLS seals but does not *read*
/// content, so `has_x25519` alone would wrongly include it (`nests.md:106`).
/// Because `"content-processor"` covers multiple distinct services, a mint
/// targets exactly ONE holder by `bridge_id` — role alone can't discriminate.
const CONTENT_PROCESSOR_ROLES: &[&str] = &[MDA_HOLDER_ROLE, CONTENT_PROCESSOR_ROLE];

/// The service-user role of the mail delivery agent — the holder every mail
/// and calendar grant is sealed to (`nests.md` § Mint: "mail/calendar → the
/// `mda` holder"), and the one a `wasm` mail-labeler subscription's per-labeler
/// grant targets. One spelling for every derivation that picks it
/// (`fauna_client_capabilities::view_model::mint_options`, the labeler
/// catalog's subscribe mint).
pub const MDA_HOLDER_ROLE: &str = "mda";

/// The generic content-processor role — the web-serve paywall holder and any
/// future scorer/indexer (`monetization.md` § Pillar 2); a post-tier grant's
/// target.
pub const CONTENT_PROCESSOR_ROLE: &str = "content-processor";

/// A service-user is a content-processor holder iff it *reads* content (a
/// [`CONTENT_PROCESSOR_ROLES`] role) **and** has attested an X25519 seal target.
/// Split out so the load-bearing MTA-exclusion is unit-testable without a mock
/// transport.
fn is_content_processor_holder(role: &str, has_x25519: bool) -> bool {
    has_x25519 && CONTENT_PROCESSOR_ROLES.contains(&role)
}

/// Shared holder discovery over any transport (native `Arc<NestClient>` / wasm
/// `WsRpcClient`) — one implementation for both seams (priority #2). Enumerates
/// a nest's approved content-processor service-users, then resolves each one's
/// X25519 seal target (+ optional ML-KEM ek). A malformed pubkey is a
/// [`DiscoverHoldersError::Rejected`].
pub async fn discover_holders<R>(
    admin: &MailAdminClient<R>,
) -> Result<Vec<HolderInfo>, DiscoverHoldersError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let roster = admin
        .list_service_users(None, Some("approved".to_string()))
        .await
        .map_err(discover_holders_error)?;
    let mut holders = Vec::new();
    for su in roster.service_users {
        if !is_content_processor_holder(&su.role, su.has_x25519) {
            continue;
        }
        let keys = admin
            .fetch_bridge_pubkey(su.role.clone(), su.bridge_id.clone())
            .await
            .map_err(discover_holders_error)?;
        let pubkey: [u8; 32] = keys.x25519_pubkey.as_slice().try_into().map_err(|_| {
            DiscoverHoldersError::Rejected(format!(
                "content processor {} returned a malformed X25519 pubkey ({} bytes, want 32)",
                su.bridge_id,
                keys.x25519_pubkey.len()
            ))
        })?;
        holders.push(HolderInfo {
            pubkey,
            // The holder's published ML-KEM ek (PQ-CAP-3): drives the mint's
            // X-Wing wrap selection. `None` for a classical-only holder → the
            // classical wrap. Length is validated at the seal (the selector's
            // 1184-B filter), not here — a wrong-length ek just falls back to
            // classical, never a hard error.
            mlkem_ek: keys.mlkem_ek.as_ref().map(|ek| ek.to_vec()),
            role: su.role,
            bridge_id: su.bridge_id,
        });
    }
    Ok(holders)
}

/// Typed `fauna.bridges.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm SPA
/// passes its `WsRpcClient`. The kind-composition logic is written once here
/// and shared across native + wasm (priority #2). Errors propagate as the
/// transport's `R::Error` (native `NestClientError`, wasm rpc-wasm error).
pub struct BridgesClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> BridgesClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.bridges.list` — snapshot of every configured bridge for
    /// the calling actor (linked status + identity + settings + link
    /// modes). Replay-safe; the 5 s default deadline is the
    /// `KindRegistry` value (see
    /// `fauna_protocol::KindRegistry::register_bridges_ui_kinds`).
    pub async fn list(&self) -> Result<ListBridgesReply, R::Error> {
        self.nest
            .request("fauna.bridges.list", ListBridgesRequest {})
            .await
    }

    /// `fauna.bridges.set_settings` — overwrite the per-bridge settings
    /// blob for the calling actor. Replay-safe (idempotent PUT). The
    /// reply carries `{ ok: true }` and is discarded; errors surface as
    /// the namespaced `RpcError` (`fauna.bridges.not_found`,
    /// `fauna.bridges.provider_error`, …) the handler emits.
    pub async fn set_settings(
        &self,
        bridge_id: impl Into<String>,
        settings: Value,
    ) -> Result<(), R::Error> {
        let _: SetSettingsReply = self
            .nest
            .request(
                "fauna.bridges.set_settings",
                SetSettingsRequest {
                    bridge_id: bridge_id.into(),
                    settings,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.link` — start the OAuth or credential flow for
    /// the named bridge in the given mode. Replay is forbidden
    /// (`forbid_replay=true`); the auto-retry path won't re-issue this
    /// kind, so the caller must explicitly re-call on disconnect. The
    /// reply carries the new linked state plus optional `redirect_url`
    /// (set when the provider returned an OAuth flow URL to surface to
    /// the user agent).
    pub async fn link(
        &self,
        bridge_id: impl Into<String>,
        mode: impl Into<String>,
        params: Value,
    ) -> Result<LinkReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.link",
                LinkRequest {
                    bridge_id: bridge_id.into(),
                    mode: mode.into(),
                    params,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.link_challenge` — the proof-of-possession challenge an
    /// external signer must sign before [`Self::link`] in `mode` is accepted
    /// (Nostr `nip07`: hand `payload` to `window.nostr.signEvent`, then pass
    /// the signed event back as the link's `proof_json` param). Replay-safe:
    /// a re-issue supersedes the last challenge.
    pub async fn link_challenge(
        &self,
        bridge_id: impl Into<String>,
        mode: impl Into<String>,
    ) -> Result<LinkChallengeReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.link_challenge",
                LinkChallengeRequest {
                    bridge_id: bridge_id.into(),
                    mode: mode.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.unlink` — disconnect the named bridge for the
    /// calling actor. Idempotent (already-unlinked is a no-op) and
    /// replay-safe; the typed `{ ok: true }` reply is discarded by the
    /// wrapper.
    pub async fn unlink(&self, bridge_id: impl Into<String>) -> Result<(), R::Error> {
        let _: UnlinkReply = self
            .nest
            .request(
                "fauna.bridges.unlink",
                UnlinkRequest {
                    bridge_id: bridge_id.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.list_follows` — list the calling actor's follow
    /// entries on the named bridge. Replay-safe pure read.
    pub async fn list_follows(
        &self,
        bridge_id: impl Into<String>,
    ) -> Result<ListFollowsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_follows",
                ListFollowsRequest {
                    bridge_id: bridge_id.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.add_follow` — add an external account to the
    /// calling actor's per-bridge follow list. `forbid_replay=true` —
    /// the auto-retry path won't re-issue this kind because the
    /// duplicate-follow constraint is server-enforced and a replay
    /// would surface as a spurious conflict. The typed `{ ok: true }`
    /// reply is discarded; errors surface as the namespaced `RpcError`.
    pub async fn add_follow(
        &self,
        bridge_id: impl Into<String>,
        id: impl Into<String>,
        petname: Option<String>,
        extra: Option<Value>,
    ) -> Result<(), R::Error> {
        let _: AddFollowReply = self
            .nest
            .request(
                "fauna.bridges.add_follow",
                AddFollowRequest {
                    bridge_id: bridge_id.into(),
                    id: id.into(),
                    petname,
                    extra,
                    unknown_keys: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.remove_follow` — remove an entry from the calling
    /// actor's per-bridge follow list. Idempotent (already-removed is a
    /// no-op); replay-safe. The typed `{ ok: true }` reply is discarded.
    pub async fn remove_follow(
        &self,
        bridge_id: impl Into<String>,
        follow_id: impl Into<String>,
    ) -> Result<(), R::Error> {
        let _: RemoveFollowReply = self
            .nest
            .request(
                "fauna.bridges.remove_follow",
                RemoveFollowRequest {
                    bridge_id: bridge_id.into(),
                    follow_id: follow_id.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.list_follow_requests` — the follow requests waiting on
    /// the calling actor's account on the named bridge. Only a bridge whose
    /// `BridgeStatus.supports_follow_requests` is true answers it, so a
    /// card that reads the flag first never asks one that would refuse the
    /// kind. Replay-safe pure read.
    pub async fn list_follow_requests(
        &self,
        bridge_id: impl Into<String>,
    ) -> Result<ListFollowRequestsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_follow_requests",
                ListFollowRequestsRequest {
                    bridge_id: bridge_id.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// Approve the waiting follow request `id` names
    /// (`fauna.bridges.resolve_follow_request`, `approve: true`). Idempotent —
    /// a request already gone succeeds. Non-optimistic: re-read
    /// [`Self::list_follow_requests`] before painting.
    pub async fn approve_follow_request(
        &self,
        bridge_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Result<(), R::Error> {
        self.resolve_follow_request(bridge_id.into(), id.into(), true)
            .await
    }

    /// Refuse the waiting follow request `id` names
    /// (`fauna.bridges.resolve_follow_request`, `approve: false`). Refusing is
    /// not blocking: the requester may ask again. Idempotent and
    /// non-optimistic, as [`Self::approve_follow_request`].
    pub async fn refuse_follow_request(
        &self,
        bridge_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Result<(), R::Error> {
        self.resolve_follow_request(bridge_id.into(), id.into(), false)
            .await
    }

    async fn resolve_follow_request(
        &self,
        bridge_id: String,
        id: String,
        approve: bool,
    ) -> Result<(), R::Error> {
        let _: ResolveFollowRequestReply = self
            .nest
            .request(
                "fauna.bridges.resolve_follow_request",
                ResolveFollowRequestRequest {
                    bridge_id,
                    id,
                    approve,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.feeds.list` — snapshot of every cross-bridge feed
    /// subscription for the calling actor. Replay-safe pure read.
    pub async fn feeds_list(&self) -> Result<ListFeedsReply, R::Error> {
        self.nest
            .request("fauna.bridges.feeds.list", ListFeedsRequest {})
            .await
    }

    /// `fauna.bridges.feeds.create` — subscribe the calling actor to a
    /// named feed on the given bridge (e.g. a Bluesky custom feed).
    /// Replay-safe — the underlying `INSERT OR IGNORE` against
    /// `UNIQUE(actor_id, bridge, feed_uri)` makes duplicate-create
    /// return the same server-assigned `id` rather than a constraint
    /// failure. Returns the id of the (new or existing) row.
    pub async fn feeds_create(
        &self,
        bridge: impl Into<String>,
        feed_uri: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<i64, R::Error> {
        let reply: CreateFeedReply = self
            .nest
            .request(
                "fauna.bridges.feeds.create",
                CreateFeedRequest {
                    bridge: bridge.into(),
                    feed_uri: feed_uri.into(),
                    name: name.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.id)
    }

    /// `fauna.bridges.feeds.delete` — remove a feed subscription by
    /// row id. Replay-safe; the typed `{ ok: true }` reply is discarded.
    /// Errors surface as `fauna.bridges.not_found` when no row matches
    /// (id, calling actor).
    pub async fn feeds_delete(&self, id: i64) -> Result<(), R::Error> {
        let _: DeleteFeedReply = self
            .nest
            .request(
                "fauna.bridges.feeds.delete",
                DeleteFeedRequest {
                    id,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }
}

/// Typed **admin-class** `fauna.bridges.*` call surface for configuring
/// the mail bridge from a Fauna app — the nest-side control plane
/// that lets an admin set up the deployment entirely from their client,
/// no HTTP (tracked internally). Generic over the same
/// `R: RpcRequester` transport as [`BridgesClient`]; the kind-composition
/// logic is written once and shared across all 7 apps (priority #2).
///
/// All kinds here are Admin-gated by `bridge_method_allowlist.rs`; a
/// non-admin caller gets `fauna.bridges.permission_denied`.
///
/// Slice 1 (tracked internally) covers the local-domain
/// surface (`mail-multidomain.md` § Wire shapes); § A6 adds the DKIM /
/// TLS sealed-blob provisioning wrappers (`mail-bridge-lifecycle.md`
/// § DKIM provisioning UX / § TLS provisioning); aliases / policy /
/// deliverability admin land here as later tracks ship their nest kinds.
pub struct MailAdminClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> MailAdminClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.bridges.add_local_domain` — claim a mail-hosting domain.
    /// The nest sets `is_primary` itself (the first domain claimed is the
    /// deployment's primary). Idempotent on the domain name: re-adding an
    /// already-active domain returns the existing row with
    /// `reply.skipped == true`. `catch_all_actor` is an optional 32-byte
    /// actor id; `dkim_selector_override` overrides the default selector.
    pub async fn add_local_domain(
        &self,
        domain: impl Into<String>,
        mta_sts_cert_mode: impl Into<String>,
        catch_all_actor: Option<Vec<u8>>,
        dkim_selector_override: Option<String>,
    ) -> Result<AddLocalDomainReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.add_local_domain",
                AddLocalDomainRequest {
                    domain: domain.into(),
                    mta_sts_cert_mode: mta_sts_cert_mode.into(),
                    catch_all_actor: catch_all_actor.map(ByteBuf::from),
                    dkim_selector_override,
                },
            )
            .await
    }

    /// `fauna.bridges.remove_local_domain` — soft-delete a domain (30-day
    /// recovery window). Refuses the primary
    /// (`fauna.protocol.malformed` / `cannot_remove_primary_domain`).
    pub async fn remove_local_domain(
        &self,
        domain: impl Into<String>,
    ) -> Result<RemoveLocalDomainReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.remove_local_domain",
                RemoveLocalDomainRequest {
                    domain: domain.into(),
                },
            )
            .await
    }

    /// `fauna.bridges.restore_local_domain` — un-soft-delete within the
    /// 30-day window.
    pub async fn restore_local_domain(
        &self,
        domain: impl Into<String>,
    ) -> Result<RestoreLocalDomainReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.restore_local_domain",
                RestoreLocalDomainRequest {
                    domain: domain.into(),
                },
            )
            .await
    }

    /// `fauna.bridges.list_local_domains` — active + soft-deleted-within-30d
    /// domains. Replay-safe pure read.
    pub async fn list_local_domains(&self) -> Result<ListLocalDomainsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_local_domains",
                ListLocalDomainsRequest {},
            )
            .await
    }

    /// `fauna.bridges.update_local_domain_config` — partial edit of the
    /// per-domain knobs (each `None` arg leaves it untouched).
    /// `dmarc_policy_mode` is the domain's published DMARC policy:
    /// `Some(Reject)`, the default, clears the domain's override
    /// (`UpdateLocalDomainConfigRequest::dmarc_policy_mode`). The knobs that
    /// need a genuine clear have their own setters below.
    pub async fn update_local_domain_config(
        &self,
        domain: impl Into<String>,
        mta_sts_max_age_seconds: Option<i64>,
        mta_sts_cert_mode: Option<String>,
        spf_record: Option<String>,
        dmarc_policy_mode: Option<DmarcMode>,
    ) -> Result<UpdateLocalDomainConfigReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.update_local_domain_config",
                UpdateLocalDomainConfigRequest {
                    domain: domain.into(),
                    mta_sts_max_age_seconds,
                    mta_sts_cert_mode,
                    spf_record,
                    dmarc_policy_mode,
                },
            )
            .await
    }

    /// `fauna.bridges.set_catch_all_actor` — designate (`Some(32-byte actor
    /// id)`) or clear (`None`) the domain's per-domain catch-all actor
    /// (`mail_domains.catch_all_actor_id`; `mail-aliases.md` § Kind 4). A
    /// dedicated setter rather than a field on `update_local_domain_config`
    /// because the catch-all column is clearable and `Option<Option<_>>` is not
    /// DAG-CBOR-round-trippable — here the single `Option` is unambiguous.
    pub async fn set_catch_all_actor(
        &self,
        domain: impl Into<String>,
        actor_id: Option<Vec<u8>>,
    ) -> Result<SetCatchAllActorReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.set_catch_all_actor",
                SetCatchAllActorRequest {
                    domain: domain.into(),
                    actor_id: actor_id.map(ByteBuf::from),
                },
            )
            .await
    }

    /// `fauna.bridges.set_role_address` — designate (`Some(32-byte actor id)`) or
    /// clear (`None`) the per-domain override actor for one role address
    /// (`postmaster`/`abuse`/`noc`/`security`; `mail-multidomain.md` § Per-domain
    /// role-address routing). A dedicated setter mirroring [`Self::set_catch_all_actor`]:
    /// the per-key set/clear is a tri-state not DAG-CBOR-round-trippable, so it
    /// can't ride `update_local_domain_config`. The nest atomic-merges the override
    /// map, so setting one role preserves the others.
    pub async fn set_role_address(
        &self,
        domain: impl Into<String>,
        role: RoleAddressKind,
        actor_id: Option<Vec<u8>>,
    ) -> Result<SetRoleAddressReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.set_role_address",
                SetRoleAddressRequest {
                    domain: domain.into(),
                    role,
                    actor_id: actor_id.map(ByteBuf::from),
                },
            )
            .await
    }

    /// `fauna.bridges.start_primary_domain_rename` — begin renaming the
    /// deployment's primary domain (`mail-primary-domain-rename.md` § Wire shapes).
    /// `new_primary_domain_id` is the 16-byte `mail_domains.domain_id` of the
    /// promotion target, which must **already** be an active additional (the
    /// two-step rule — `add_local_domain` first; the nest refuses
    /// `new_primary_must_be_additional` otherwise). `grace_days` (`[1, 30]`,
    /// default 7) sizes the peer-MTA cache-flush window. The nest validates all
    /// preconditions (single-active-rename, cert-mode, TLS-posture monotonicity,
    /// SAN cap) and surfaces a 409-class error string on refusal.
    pub async fn start_primary_domain_rename(
        &self,
        new_primary_domain_id: Vec<u8>,
        grace_days: Option<i64>,
    ) -> Result<StartPrimaryDomainRenameReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.start_primary_domain_rename",
                StartPrimaryDomainRenameRequest {
                    new_primary_domain_id: ByteBuf::from(new_primary_domain_id),
                    grace_days,
                },
            )
            .await
    }

    /// `fauna.bridges.get_primary_domain_rename_status` — the in-flight rename, or
    /// `reply.rename == None` when none is active. Cheap active-only read the
    /// `admin-dns` page polls for the in-flight banner + per-row state.
    pub async fn get_primary_domain_rename_status(
        &self,
    ) -> Result<GetPrimaryDomainRenameStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.get_primary_domain_rename_status",
                GetPrimaryDomainRenameStatusRequest {},
            )
            .await
    }

    /// `fauna.bridges.list_primary_domain_renames` — every rename (in-flight +
    /// terminal), newest-first, for the audit surface. Replay-safe pure read.
    pub async fn list_primary_domain_renames(
        &self,
    ) -> Result<ListPrimaryDomainRenamesReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_primary_domain_renames",
                ListPrimaryDomainRenamesRequest {},
            )
            .await
    }

    /// `fauna.bridges.complete_primary_domain_rename` — finalize a rename in
    /// `ready_to_complete` (or in `grace` with `force = true`, accepting the early
    /// peer-cache-flush risk). `rename_id` scopes the call so a stale rename can't
    /// be completed across an abort-then-restart race (the client holds it from
    /// `start` / `get_status`). Without `force`, a `grace` row refuses
    /// `grace_period_not_expired`.
    pub async fn complete_primary_domain_rename(
        &self,
        rename_id: Vec<u8>,
        force: bool,
    ) -> Result<CompletePrimaryDomainRenameReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.complete_primary_domain_rename",
                CompletePrimaryDomainRenameRequest {
                    rename_id: ByteBuf::from(rename_id),
                    // `None` and `Some(false)` are equivalent nest-side; send the
                    // explicit bool the admin picked.
                    force: Some(force),
                },
            )
            .await
    }

    /// `fauna.bridges.abort_primary_domain_rename` — unwind a rename from any
    /// non-terminal state (pre-flip: a cheap state change; post-flip: the
    /// expensive inverse re-flip the confirm dialog must name). `rename_id` scopes
    /// it; `abort_reason` is an optional audit string.
    pub async fn abort_primary_domain_rename(
        &self,
        rename_id: Vec<u8>,
        abort_reason: Option<String>,
    ) -> Result<AbortPrimaryDomainRenameReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.abort_primary_domain_rename",
                AbortPrimaryDomainRenameRequest {
                    rename_id: ByteBuf::from(rename_id),
                    abort_reason,
                },
            )
            .await
    }

    /// `fauna.bridges.extend_primary_domain_rename_grace` — push `grace_ends_at`
    /// out by `additional_days × 1 day` (`[1, 30]` per call). Valid from `grace`
    /// or `ready_to_complete` (a `ready_to_complete` row reverts to `grace`).
    pub async fn extend_primary_domain_rename_grace(
        &self,
        rename_id: Vec<u8>,
        additional_days: i64,
    ) -> Result<ExtendPrimaryDomainRenameGraceReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.extend_primary_domain_rename_grace",
                ExtendPrimaryDomainRenameGraceRequest {
                    rename_id: ByteBuf::from(rename_id),
                    additional_days,
                },
            )
            .await
    }

    /// `fauna.bridges.force_rotate_dkim` — emergency DKIM rotation for one local
    /// domain (`mail-multidomain.md` § Rotation, "Emergency rotation"). Flips the
    /// domain's active selector (`mail_domains.dkim_selector`) to its newest
    /// selector immediately, skipping the scheduled 24 h peer-cache wait, so
    /// the nest signs with the new selector at the next outbound hand-out.
    /// Precondition: the rotation mint has already seated a NEWER selector
    /// (keygen is nest-side; no client holds the private key) — else the call
    /// errors `fauna.bridges.no_dkim_selector_to_rotate`.
    pub async fn force_rotate_dkim(
        &self,
        domain: impl Into<String>,
    ) -> Result<ForceRotateDkimReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.force_rotate_dkim",
                ForceRotateDkimRequest {
                    domain: domain.into(),
                },
            )
            .await
    }

    /// `fauna.bridges.provision_self_signed_cert` — ask nest to synthesize a
    /// self-signed TLS cert for an active local mail domain (CN + first SAN =
    /// `domain`, plus any `additional_dns_sans`) and seal+fan it out to every
    /// approved bridge with an x25519 pubkey (the bridge fetches it via
    /// `fetch_tls_cert_blob`). The reply names which bridges were sealed-to and
    /// which were skipped (no x25519 attested yet — re-call after the bridge
    /// attests). The cert has a 90-day window and is **not** auto-renewed:
    /// re-call to refresh. Per `mail-bridge-lifecycle.md` § TLS provisioning,
    /// "Admin-synthesized (self-signed)" — the dev / internal / air-gapped path
    /// where neither ACME nor an externally-issued cert fits.
    pub async fn provision_self_signed_cert(
        &self,
        domain: impl Into<String>,
        additional_dns_sans: Vec<String>,
    ) -> Result<ProvisionSelfSignedCertReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.provision_self_signed_cert",
                ProvisionSelfSignedCertRequest {
                    domain: domain.into(),
                    additional_dns_sans,
                },
            )
            .await
    }

    /// `fauna.bridges.restore_real_tls_cert` — undo a self-signed override of
    /// the nest's own TLS listener and switch it back to a real (CA-issued)
    /// cert. If `provision_self_signed_cert` previously clobbered a real cert,
    /// the real cert was preserved and is restored immediately (the reply has
    /// `restored_immediately: true`, `method: "backup"`). With no preserved cert
    /// it is a no-op (`method: "self_heal"`) — the nest's ACME lifecycle already
    /// self-heals a self-signed cert to a real one automatically, so this RPC is
    /// purely an optimization that skips the ~5-minute self-heal wait, never a
    /// requirement. Non-destructive and parameterless. Per
    /// `mail-bridge-lifecycle.md` § TLS provisioning.
    pub async fn restore_real_tls_cert(&self) -> Result<RestoreRealTlsCertReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.restore_real_tls_cert",
                RestoreRealTlsCertRequest {},
            )
            .await
    }

    /// `fauna.bridges.fetch_bridge_pubkey` — the approved bridge's public
    /// keys (Ed25519 + X25519). The X25519 pubkey is the HPKE *seal target*
    /// for a blob sealed to that bridge (a TLS cert, a spam-model copy). This
    /// returns public keys only — the bridge's X25519 *secret* never leaves
    /// the bridge. Errors (`fauna.bridges.not_found`) when no approved bridge
    /// with that `(role, id)` exists, or it hasn't attested an X25519 key yet.
    pub async fn fetch_bridge_pubkey(
        &self,
        bridge_role: impl Into<String>,
        bridge_id: impl Into<String>,
    ) -> Result<FetchBridgePubkeyReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.fetch_bridge_pubkey",
                FetchBridgePubkeyRequest {
                    bridge_role: bridge_role.into(),
                    bridge_id: bridge_id.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.list_dkim_selectors` — enumerate provisioned selectors
    /// as *unsealed* public metadata (DNS TXT record + created-at). `domain`
    /// restricts to one domain; `None` lists all. This is the deliverability
    /// page's source of truth for re-displaying the DNS record after the
    /// one-time provision view. Replay-safe pure read.
    pub async fn list_dkim_selectors(
        &self,
        domain: Option<String>,
    ) -> Result<ListDkimSelectorsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_dkim_selectors",
                ListDkimSelectorsRequest {
                    domain,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.revoke_dkim_blob` — retire a selector (deletes the
    /// nest-held key and its public record). Used after rotating to a new
    /// selector and letting DNS propagate. Idempotent: retiring an absent
    /// selector still replies `{ ok: true }`.
    pub async fn revoke_dkim_blob(
        &self,
        domain: impl Into<String>,
        selector: impl Into<String>,
    ) -> Result<(), R::Error> {
        let _: RevokeReply = self
            .nest
            .request(
                "fauna.bridges.revoke_dkim_blob",
                RevokeDkimBlobRequest {
                    domain: domain.into(),
                    selector: selector.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.list_service_users` — enumerate mail service-user
    /// bridges (public metadata; no secrets). The deliverability flow uses
    /// this to find the approved MTA `bridge_id` to seal DKIM/TLS blobs to
    /// (and to gate provisioning on `has_x25519`). `role`/`status` filter
    /// (`"mta"`/`"mda"`, `"pending"`/`"approved"`/`"revoked"`); `None` = all.
    /// Replay-safe pure read.
    pub async fn list_service_users(
        &self,
        role: Option<String>,
        status: Option<String>,
    ) -> Result<ListServiceUsersReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_service_users",
                ListServiceUsersRequest {
                    role,
                    status,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.list_pending_bridges` — the approval-card feed: every
    /// enrolled bridge awaiting approval (`status == pending`), public metadata
    /// only. Distinct from [`list_service_users`](Self::list_service_users)
    /// (the full roster) so the card view has a no-arg call
    /// (`mail-bridge-lifecycle.md` § Pending approval). Replay-safe pure read.
    pub async fn list_pending_bridges(&self) -> Result<ListPendingBridgesReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_pending_bridges",
                ListPendingBridgesRequest {},
            )
            .await
    }

    /// `fauna.bridges.approve_pending_bridge` — enroll a pending bridge:
    /// `pending → approved`, recording the calling admin as approver. `role`
    /// (`"mta"`/`"mda"`) is validated against the bridge's enrolled role (the
    /// nest fixes role at enrollment, so this confirms rather than re-assigns).
    /// Idempotent on an already-approved bridge; a revoked bridge is refused
    /// (`fauna.protocol.malformed` — it must re-enroll). The `{ ok: true }`
    /// reply is discarded; errors surface as the namespaced `RpcError`
    /// (`fauna.bridges.not_found` for an unknown pubkey).
    pub async fn approve_pending_bridge(
        &self,
        ed25519_pubkey: Vec<u8>,
        role: impl Into<String>,
    ) -> Result<(), R::Error> {
        let _: ApprovePendingBridgeReply = self
            .nest
            .request(
                "fauna.bridges.approve_pending_bridge",
                ApprovePendingBridgeRequest {
                    ed25519_pubkey,
                    role: role.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.reject_pending_bridge` — revoke an enrolled bridge
    /// (`→ revoked`; the row is kept so a re-connect from the same pubkey
    /// rejects immediately rather than re-pending). Idempotent on an already-
    /// revoked bridge; `fauna.bridges.not_found` for an unknown pubkey. The
    /// typed `{ ok: true }` reply is discarded (`mail-bridge-lifecycle.md`
    /// § Pending approval → rejection flow).
    pub async fn reject_pending_bridge(&self, ed25519_pubkey: Vec<u8>) -> Result<(), R::Error> {
        let _: RejectPendingBridgeReply = self
            .nest
            .request(
                "fauna.bridges.reject_pending_bridge",
                RejectPendingBridgeRequest {
                    ed25519_pubkey,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.set_mail_enabled` — flip the deployment-wide mail-enable
    /// toggle. Nest materializes the `/data/imap-enabled` flag file (the signal
    /// the supervisor run-script gates on) and best-effort signals the
    /// supervisor sidekick socket so the bridge services come up / down without
    /// a restart (`mail-bridge-lifecycle.md` § Default-off on first claim). The
    /// `{ ok: true }` reply is discarded.
    pub async fn set_mail_enabled(&self, enabled: bool) -> Result<(), R::Error> {
        let _: SetMailEnabledReply = self
            .nest
            .request(
                "fauna.bridges.set_mail_enabled",
                SetMailEnabledRequest {
                    enabled,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.set_caldav_enabled` — flip the deployment-wide CalDAV-enable
    /// toggle, **independently** of mail (`caldav-server.md` § Independent
    /// enablement: CalDAV needs only the HTTPS surface, not MX/DKIM/SPF/port-25,
    /// so a calendar-only deployment is valid). Nest persists the `caldav_enabled`
    /// DB toggle + materializes the `/data/caldav-enabled` flag (the signal the
    /// MDA run-script gates the `:443` CalDAV listener on) and reconciles the
    /// supervisor so the MDA binds/unbinds CalDAV without a full restart. Sibling
    /// of [`Self::set_mail_enabled`]; the `{ ok: true }` reply is discarded.
    pub async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), R::Error> {
        let _: SetCalDavEnabledReply = self
            .nest
            .request(
                "fauna.bridges.set_caldav_enabled",
                SetCalDavEnabledRequest {
                    enabled,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.set_carddav_enabled` — flip the deployment-wide
    /// CardDAV-enable toggle, **independently** of mail and CalDAV
    /// (`carddav-server.md` § Independent enablement: CardDAV needs only the
    /// HTTPS surface and rides the **same** DAV listener as CalDAV — no separate
    /// port, so a contacts-only deployment is valid). Nest persists the
    /// `carddav_enabled` DB toggle + materializes the `/data/carddav-enabled`
    /// flag (alongside `imap-enabled` + `caldav-enabled`) and reconciles the
    /// supervisor so the MDA adds/removes the `/carddav` handler without a full
    /// restart. Sibling of [`Self::set_caldav_enabled`]; the `{ ok: true }`
    /// reply is discarded.
    pub async fn set_carddav_enabled(&self, enabled: bool) -> Result<(), R::Error> {
        let _: SetCardDavEnabledReply = self
            .nest
            .request(
                "fauna.bridges.set_carddav_enabled",
                SetCardDavEnabledRequest {
                    enabled,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.set_webdav_enabled` — flip the deployment-wide
    /// WebDAV-enable toggle, **independently** of mail, CalDAV, and CardDAV
    /// (`webdav-server.md` § Independent enablement: WebDAV needs only the HTTPS
    /// surface and rides the **same** DAV listener as CalDAV/CardDAV — no
    /// separate port, so a files-only deployment is valid). Nest persists the
    /// `webdav_enabled` DB toggle + materializes the `/data/webdav-enabled` flag
    /// (alongside `imap-enabled` + `caldav-enabled` + `carddav-enabled`) and
    /// reconciles the supervisor so the MDA adds/removes the `/webdav` handler
    /// without a full restart. Harmless-on: nothing is served until a set is
    /// individually flagged. Sibling of [`Self::set_carddav_enabled`]; the
    /// `{ ok: true }` reply is discarded.
    pub async fn set_webdav_enabled(&self, enabled: bool) -> Result<(), R::Error> {
        let _: SetWebDavEnabledReply = self
            .nest
            .request(
                "fauna.bridges.set_webdav_enabled",
                SetWebDavEnabledRequest {
                    enabled,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.set_caldav_port` — set the deployment-wide CalDAV listener
    /// port, an **admin choice** (`caldav-server.md` § Network exposure —
    /// admin-settable CalDAV port; a product invariant — a port a human
    /// picks is client UI + nest state, never a config file/env). Default
    /// `bridge_routing::DEFAULT_CALDAV_PORT` (8443). Nest persists the `caldav_port`
    /// singleton + pushes a `CALDAV_PORT` `config_changed`; the MDA re-fetches
    /// `fetch_config` and, when no operator-hatch pins the listener (the bare-IP /
    /// desktop case), exits cleanly so the supervisor rebinds it to the new port.
    /// Sibling of [`Self::set_caldav_enabled`]; the `{ ok: true }` reply is
    /// discarded.
    pub async fn set_caldav_port(&self, port: u16) -> Result<(), R::Error> {
        let _: SetCaldavPortReply = self
            .nest
            .request(
                "fauna.bridges.set_caldav_port",
                SetCaldavPortRequest {
                    port,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.set_auto_enable_mail_for_new_users` — flip the
    /// deployment-wide policy deciding whether a freshly-registered user's client
    /// auto-provisions its own mailbox on first setup (default-**on**, the
    /// works-out-of-box invariant extended to every user). Unlike
    /// [`Self::set_mail_enabled`] this drives **no** bridge state (the bridge
    /// never reads it) — it is a client-read deployment default surfaced on
    /// `fauna.setup.status` (read back via
    /// [`Self::get_auto_enable_mail_for_new_users`]); the nest cannot mint the
    /// mailbox itself (the MSEK is client-held). Admin-only; a deployment default,
    /// never a per-user control (`admin.md` § Don't do these). The `{ ok: true }`
    /// reply is discarded. `mail-policy-config.md` § Tier-2 new-user mail defaults.
    pub async fn set_auto_enable_mail_for_new_users(&self, enabled: bool) -> Result<(), R::Error> {
        let _: SetAutoEnableMailForNewUsersReply = self
            .nest
            .request(
                "fauna.bridges.set_auto_enable_mail_for_new_users",
                SetAutoEnableMailForNewUsersRequest {
                    enabled,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// The **admin read twin** of [`Self::set_auto_enable_mail_for_new_users`].
    /// This knob is **not** in `FetchConfigReply` (the bridge never reads it), so
    /// — exactly like the alias policy needing its own read twin — the
    /// `admin-mail` toggle hydrates it from `fauna.setup.status`
    /// (`SetupStatusReply.auto_enable_mail_for_new_users`, default-**on** when the
    /// reply omits it). Read-only; works on any authed connection.
    pub async fn get_auto_enable_mail_for_new_users(&self) -> Result<bool, R::Error> {
        let reply: SetupStatusReply = self
            .nest
            .request("fauna.setup.status", SetupStatusRequest::default())
            .await?;
        Ok(reply.auto_enable_mail_for_new_users)
    }

    /// `fauna.bridges.get_mail_config` — the **admin read twin** of the
    /// bridge's `fetch_config`. Returns the overlaid effective config (catalog
    /// defaults with every `put_<substruct>_policy` override applied) so the
    /// `admin-mail` policy form hydrates from real persisted state before edit
    /// — without it the form could only blind-write. Admin-class, read-only.
    /// The nest-side **alias** policy (`put_alias_policy`) is *not* in
    /// `FetchConfigReply`; it has its own read twin. See
    /// `docs/goal/behavior/mail-policy-config.md` § Implementation status today.
    pub async fn get_mail_config(&self) -> Result<FetchConfigReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.get_mail_config",
                GetMailConfigRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.revoke_service_user` — revoke an already-approved bridge
    /// so the admin can rotate its service-user key (the running-phase "Rotate
    /// bridge service-user key" affordance). The bridge's next `whoami` returns
    /// `revoked`, it shuts down gracefully, the supervisor restarts it, and it
    /// regenerates a fresh keypair → a new pending approval. Keyed by
    /// `bridge_actor_id` (a bridge's actor_id is its ed25519 pubkey). Idempotent
    /// on an already-revoked bridge; `fauna.bridges.not_found` for an unknown
    /// actor id. The `{ ok: true }` reply is discarded (`mail-bridge-lifecycle.md`
    /// § Service-user re-keying).
    pub async fn revoke_service_user(&self, bridge_actor_id: Vec<u8>) -> Result<(), R::Error> {
        let _: RevokeServiceUserReply = self
            .nest
            .request(
                "fauna.bridges.revoke_service_user",
                RevokeServiceUserRequest {
                    bridge_actor_id,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.provision_tls_cert_blob` — upload a TLS cert chain +
    /// private key, **already HPKE-sealed to the bridge's X25519 pubkey by
    /// the caller** (via `seal_tls_cert_blob`). `sealed_bytes` is the
    /// canonical-CBOR `TlsCertBlob` ciphertext; its
    /// `(bridge_role, bridge_id, domain)` index lives inside the blob header.
    /// This is the "Admin-uploaded" TLS path (`mail-bridge-lifecycle.md`
    /// § TLS provisioning) — the admin's client wraps the pasted/uploaded
    /// chain + key and calls this directly with the pre-sealed bytes. The
    /// `{ ok: true }` reply is discarded; errors surface as the namespaced
    /// `RpcError`.
    pub async fn provision_tls_cert_blob(&self, sealed_bytes: Vec<u8>) -> Result<(), R::Error> {
        let _: ProvisionReply = self
            .nest
            .request(
                "fauna.bridges.provision_tls_cert_blob",
                ProvisionTlsCertBlobRequest {
                    blob: ByteBuf::from(sealed_bytes),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    // ── A3 Bucket B — per-sub-struct mail-policy write path ──────────
    //
    // Five uniform admin kinds, one per `FetchConfigReply` sub-struct.
    // Each carries one `Option<T>` per field (`None` ⇒ keep the catalog
    // default; the admin form submits the whole sub-struct, so this is a
    // PUT not a merge). The `{ ok: true }` reply carries no information the
    // `Result` doesn't (a validation failure — e.g. spam thresholds out of
    // order — surfaces as `fauna.protocol.malformed`), so all five discard
    // it. The bridge picks the change up on its next `fetch_config`
    // (subscribe-and-hot-reload still deferred).

    /// `fauna.bridges.put_spam_policy` — overlay the `SpamPolicyThresholds`
    /// sub-struct. Nest rejects an override whose effective thresholds
    /// violate `spam_folder < reject`.
    pub async fn put_spam_policy(&self, policy: PutSpamPolicyRequest) -> Result<(), R::Error> {
        let _: PutPolicyReply = self
            .nest
            .request("fauna.bridges.put_spam_policy", policy)
            .await?;
        Ok(())
    }

    /// `fauna.bridges.publish_spam_baseline` — aggregate the trained models of
    /// every opt-in user into the single-row deployment baseline (admin-only).
    /// The reply carries only **aggregate** counts (contributors merged + total
    /// samples), never a contributor identity (`mail-spam.md` § Cold start
    /// Path 2). The nest's k-anonymity floor withholds — publishing an
    /// empty/withdrawn baseline with `published = false` — below
    /// `BASELINE_MIN_CONTRIBUTORS` opt-in contributors.
    pub async fn publish_spam_baseline(&self) -> Result<PublishSpamBaselineReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.publish_spam_baseline",
                PublishSpamBaselineRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.get_spam_baseline_state` — the deployment baseline's
    /// current state (admin-only): whether a baseline is served, over how many
    /// contributors and since when, whether the last run was deferred, and
    /// whether standing publish is on. Aggregate-only and history-free — no
    /// withdrawal time or reason exists on the wire (`mail-spam.md` § Cold
    /// start Path 2 → *Standing publish*).
    pub async fn get_spam_baseline_state(&self) -> Result<GetSpamBaselineStateReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.get_spam_baseline_state",
                GetSpamBaselineStateRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.mail_health` — the mail health readout (admin-only): the
    /// categorical state, the seven check rows, the two heartbeat stamps and the
    /// de-listing URL (`mail-deliverability.md` § The mail health readout).
    pub async fn mail_health(&self) -> Result<MailHealthReply, R::Error> {
        self.nest
            .request("fauna.bridges.mail_health", MailHealthRequest::default())
            .await
    }

    /// `fauna.bridges.blocklist_self_check_run` — force-refresh the whole DNSBL
    /// set (admin-only; each DNSBL is rate-limited nest-side to once a minute).
    pub async fn blocklist_self_check_run(&self) -> Result<BlocklistSelfCheckRunReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.blocklist_self_check_run",
                BlocklistSelfCheckRunRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.run_deliverability_diagnostics` — run the DNS/auth + TLS
    /// checklist now (admin-only); the nest persists the run.
    pub async fn run_deliverability_diagnostics(
        &self,
    ) -> Result<RunDeliverabilityDiagnosticsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.run_deliverability_diagnostics",
                RunDeliverabilityDiagnosticsRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.outbound_warmup_reset` — restart the fresh-IP warm-up ramp
    /// at day 1 after an outbound IP change (admin-only); returns the post-reset
    /// state.
    pub async fn outbound_warmup_reset(&self) -> Result<OutboundWarmupStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.outbound_warmup_reset",
                OutboundWarmupResetRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.put_auth_policy` — overlay the `AuthPolicy` sub-struct
    /// (DMARC/SPF/DKIM enforcement gates, AUTH-failure lockout).
    pub async fn put_auth_policy(&self, policy: PutAuthPolicyRequest) -> Result<(), R::Error> {
        let _: PutPolicyReply = self
            .nest
            .request("fauna.bridges.put_auth_policy", policy)
            .await?;
        Ok(())
    }

    /// `fauna.bridges.put_submission_policy` — overlay the
    /// `SubmissionPolicyThresholds` sub-struct.
    pub async fn put_submission_policy(
        &self,
        policy: PutSubmissionPolicyRequest,
    ) -> Result<(), R::Error> {
        let _: PutPolicyReply = self
            .nest
            .request("fauna.bridges.put_submission_policy", policy)
            .await?;
        Ok(())
    }

    /// `fauna.bridges.put_imap_policy` — overlay the `ImapPolicy` sub-struct.
    pub async fn put_imap_policy(&self, policy: PutImapPolicyRequest) -> Result<(), R::Error> {
        let _: PutPolicyReply = self
            .nest
            .request("fauna.bridges.put_imap_policy", policy)
            .await?;
        Ok(())
    }

    /// `fauna.bridges.put_outbound_policy` — overlay the `OutboundPolicy`
    /// sub-struct.
    pub async fn put_outbound_policy(
        &self,
        policy: PutOutboundPolicyRequest,
    ) -> Result<(), R::Error> {
        let _: PutPolicyReply = self
            .nest
            .request("fauna.bridges.put_outbound_policy", policy)
            .await?;
        Ok(())
    }

    /// `fauna.bridges.put_alias_policy` — set the four nest-side
    /// alias-policy knobs (`exact_aliases_max`, `reserved_local_parts`,
    /// `subaddressing_enabled`, `wildcard_prefix_enabled`). Unlike the five
    /// `put_<substruct>_policy` calls above these are read nest-side by the
    /// alias resolver + alias CRUD, not projected to the bridge
    /// (tracked internally). `None` ⇒ keep the catalog default.
    pub async fn put_alias_policy(&self, policy: PutAliasPolicyRequest) -> Result<(), R::Error> {
        let _: PutPolicyReply = self
            .nest
            .request("fauna.bridges.put_alias_policy", policy)
            .await?;
        Ok(())
    }

    /// `fauna.bridges.get_alias_policy` — the **admin read twin** of
    /// `put_alias_policy`. Returns the effective (override-or-default) nest-side
    /// alias policy so the `admin-mail` form hydrates before edit. The four
    /// alias knobs are *not* in `FetchConfigReply` (consumed nest-side by the
    /// alias resolver + CRUD), so they read through this dedicated kind rather
    /// than `get_mail_config` — the read half of the write-path split. Admin-
    /// class, read-only (tracked internally).
    pub async fn get_alias_policy(&self) -> Result<AliasPolicy, R::Error> {
        self.nest
            .request(
                "fauna.bridges.get_alias_policy",
                GetAliasPolicyRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.create_forwarder` — create an admin external forwarder
    /// (`mail-aliases.md` § Kind 7): an address with no local mailbox that
    /// forwards inbound to `forward_target`, attributed to the calling admin.
    /// The nest validates the target (RFC-5321 + must-not-be-a-hosted-domain),
    /// the pattern (reserved-local-part via the `put_alias_policy` tunable), and
    /// the exact↔forwarder collision. Returns the new forwarder's 16-byte id.
    pub async fn create_forwarder(
        &self,
        local_domain: impl Into<String>,
        pattern: impl Into<String>,
        forward_target: impl Into<String>,
    ) -> Result<CreateForwarderReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.create_forwarder",
                CreateForwarderRequest {
                    local_domain: local_domain.into(),
                    pattern: pattern.into(),
                    forward_target: forward_target.into(),
                },
            )
            .await
    }

    /// `fauna.bridges.list_forwarders` — enumerate the deployment's external
    /// forwarders (the `AliasRow`s carry `forward_target`). Distinct from the
    /// owner-scoped `list_account_aliases`, which excludes forwarders.
    pub async fn list_forwarders(&self) -> Result<ListForwardersReply, R::Error> {
        self.nest
            .request("fauna.bridges.list_forwarders", ListForwardersRequest {})
            .await
    }

    /// `fauna.bridges.delete_forwarder` — destructively remove a forwarder by
    /// id. Any admin may delete any forwarder (deployment config).
    pub async fn delete_forwarder(
        &self,
        alias_id: Vec<u8>,
    ) -> Result<DeleteForwarderReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.delete_forwarder",
                DeleteForwarderRequest {
                    alias_id: ByteBuf::from(alias_id),
                },
            )
            .await
    }
}

/// Typed **user-class** `fauna.bridges.*` call surface for a person
/// managing **their own** per-account mail addresses — the account-detail
/// Mail-tier "Aliases" surface (`docs/goal/behavior/mail-aliases.md`).
/// Distinct from [`MailAdminClient`]: those kinds are Admin-gated and
/// configure the deployment; these are `User`-gated and the nest derives
/// the owning actor from the authenticated caller, so there is no target-
/// actor parameter — a user can only touch their own aliases.
///
/// Generic over the same `R: RpcRequester` transport; the kind-composition
/// logic is written once and shared across all 7 apps (priority #2).
/// This is also the home for the sibling per-account `mail-forwarding.md`
/// surface when it lands (same account-detail Mail-tier UX shape).
///
/// Slices 1-3 (tracked internally) cover the
/// **exact** + **wildcard_prefix** CRUD and the **disposable** mint
/// (`generate_disposable_alias`); `+suffix` + the fixed-order resolver are
/// nest-side (`resolve_recipient` is bridge→nest, not a client kind); the
/// alias-hit audit lands in A2.4.
pub struct MailAccountClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> MailAccountClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.bridges.list_account_aliases` — the calling actor's own alias
    /// rows across all local domains. Replay-safe pure read.
    pub async fn list_account_aliases(&self) -> Result<Vec<AliasRow>, R::Error> {
        let reply: ListAccountAliasesReply = self
            .nest
            .request(
                "fauna.bridges.list_account_aliases",
                ListAccountAliasesRequest {},
            )
            .await?;
        Ok(reply.aliases)
    }

    /// `fauna.bridges.create_account_alias` — create one alias owned by the
    /// calling actor; returns its new 16-byte UUID. `kind` is `"exact"` or
    /// `"wildcard_prefix"` (the nest rejects `disposable`/`catchall` here —
    /// disposable mints via `generate_disposable_alias`, A2.3; catch-all is
    /// admin policy). The nest validates `pattern` (per-kind: char-class /
    /// length / reserved for exact; min-`<2>-` prefix + reserved-glob for
    /// wildcard), enforces the per-account exact cap / one-wildcard-per-actor,
    /// and surfaces a duplicate (or a wildcard shadowing an exact) as
    /// `fauna.bridges.conflicts_with_existing_alias`. The wildcard `pattern`
    /// is the literal prefix incl. its trailing `-` (e.g. `bob-`).
    /// `forbid_replay` is set server-side, so the caller re-issues on disconnect.
    pub async fn create_account_alias(
        &self,
        kind: impl Into<String>,
        local_domain: impl Into<String>,
        pattern: impl Into<String>,
        controls: AliasControls,
    ) -> Result<Vec<u8>, R::Error> {
        let reply: CreateAccountAliasReply = self
            .nest
            .request(
                "fauna.bridges.create_account_alias",
                CreateAccountAliasRequest {
                    kind: kind.into(),
                    local_domain: local_domain.into(),
                    pattern: pattern.into(),
                    controls,
                },
            )
            .await?;
        Ok(reply.alias_id.into_vec())
    }

    /// `fauna.bridges.import_account_aliases` (User, self-scoped) — bulk-create
    /// exact aliases from a pasted address list; best-effort per line, idempotent.
    /// Returns the per-line outcomes (`mail-aliases.md` § Bulk import).
    pub async fn import_account_aliases(
        &self,
        lines: Vec<String>,
    ) -> Result<Vec<ImportAliasOutcome>, R::Error> {
        let reply: ImportAccountAliasesReply = self
            .nest
            .request(
                "fauna.bridges.import_account_aliases",
                ImportAccountAliasesRequest { lines },
            )
            .await?;
        Ok(reply.results)
    }

    /// `fauna.bridges.update_account_alias` — full-overwrite of an owned
    /// alias's `pattern` + controls (`kind` is immutable). The client
    /// submits the complete control set from the row it edits. The typed
    /// `{ ok: true }` reply is discarded; a non-owned/absent `alias_id`
    /// surfaces as `fauna.bridges.not_found`.
    pub async fn update_account_alias(
        &self,
        alias_id: Vec<u8>,
        pattern: impl Into<String>,
        controls: AliasControls,
    ) -> Result<(), R::Error> {
        let _: UpdateAccountAliasReply = self
            .nest
            .request(
                "fauna.bridges.update_account_alias",
                UpdateAccountAliasRequest {
                    alias_id: ByteBuf::from(alias_id),
                    pattern: pattern.into(),
                    controls,
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.revoke_account_alias` — flip `disabled = true` on an
    /// owned alias (soft-off, preserves the row). Idempotent; the typed
    /// `{ ok: true }` reply is discarded.
    pub async fn revoke_account_alias(&self, alias_id: Vec<u8>) -> Result<(), R::Error> {
        let _: RevokeAccountAliasReply = self
            .nest
            .request(
                "fauna.bridges.revoke_account_alias",
                RevokeAccountAliasRequest {
                    alias_id: ByteBuf::from(alias_id),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.enable_account_alias` — flip `disabled = false` on an
    /// owned alias: the reverse of `revoke_account_alias`, so a soft-off alias
    /// can be brought back (`mail-aliases.md:156`). Idempotent; the typed
    /// `{ ok: true }` reply is discarded.
    pub async fn enable_account_alias(&self, alias_id: Vec<u8>) -> Result<(), R::Error> {
        let _: EnableAccountAliasReply = self
            .nest
            .request(
                "fauna.bridges.enable_account_alias",
                EnableAccountAliasRequest {
                    alias_id: ByteBuf::from(alias_id),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.delete_account_alias` — destructively remove an owned
    /// alias (cascades its audit rows). The typed `{ ok: true }` reply is
    /// discarded; a non-owned/absent `alias_id` is `fauna.bridges.not_found`.
    pub async fn delete_account_alias(&self, alias_id: Vec<u8>) -> Result<(), R::Error> {
        let _: DeleteAccountAliasReply = self
            .nest
            .request(
                "fauna.bridges.delete_account_alias",
                DeleteAccountAliasRequest {
                    alias_id: ByteBuf::from(alias_id),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.generate_disposable_alias` — mint one disposable alias
    /// for the calling actor (`mail-aliases.md` § Kind 5). The nest derives
    /// the `<handle>` + hosting `<domain>` from the actor's canonical (oldest)
    /// exact alias, so the actor must already have one (else
    /// `fauna.bridges.no_canonical_address`). `ttl_days` / `uses` are `None` =
    /// the per-user default (30 days / 1 use); `uses = Some(0)` = unlimited.
    /// Returns `{alias_id, full_address, token}` — the client copies
    /// `full_address` to the clipboard. Server-set `forbid_replay`, so the
    /// caller re-issues on disconnect (a replay would mint a second token).
    pub async fn generate_disposable_alias(
        &self,
        ttl_days: Option<u32>,
        uses: Option<u32>,
        label: impl Into<String>,
    ) -> Result<GenerateDisposableAliasReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.generate_disposable_alias",
                GenerateDisposableAliasRequest {
                    ttl_days,
                    uses,
                    label: label.into(),
                },
            )
            .await
    }

    /// `fauna.bridges.list_account_alias_hits` — the per-alias audit list
    /// (`mail-aliases.md` § Per-alias-hit audit list). Newest-first; the caller
    /// must own `alias_id` (else `fauna.bridges.not_found`). `before_hit_id` is
    /// the keyset cursor: pass the **last** returned `hit_id` to fetch the next
    /// (older) page; `None` = newest page. Returns the page of `AliasHitRow`s.
    pub async fn list_account_alias_hits(
        &self,
        alias_id: Vec<u8>,
        limit: u32,
        before_hit_id: Option<Vec<u8>>,
    ) -> Result<Vec<AliasHitRow>, R::Error> {
        let reply: ListAccountAliasHitsReply = self
            .nest
            .request(
                "fauna.bridges.list_account_alias_hits",
                ListAccountAliasHitsRequest {
                    alias_id: ByteBuf::from(alias_id),
                    limit,
                    before_hit_id: before_hit_id.map(ByteBuf::from),
                },
            )
            .await?;
        Ok(reply.hits)
    }

    /// `fauna.bridges.get_forward_all_to` — the calling actor's per-account
    /// "forward all incoming mail to" address, or `None` when disabled
    /// (`mail-forwarding.md` § Per-account "forward all"). Replay-safe pure
    /// read. Stored at the plaintext routing-metadata floor in both storage
    /// modes (the separate-bridge-sealed form is the deferred N1b upgrade).
    pub async fn get_forward_all_to(&self) -> Result<Option<String>, R::Error> {
        let reply: GetForwardAllToReply = self
            .nest
            .request(
                "fauna.bridges.get_forward_all_to",
                GetForwardAllToRequest {},
            )
            .await?;
        Ok(reply.forward_all_to)
    }

    /// `fauna.bridges.set_forward_all_to` — set a non-empty address to enable
    /// forward-all, or `None`/blank to disable. The nest RFC-5321-validates the
    /// address and rejects one pointing at a hosted `local_domains` address
    /// (`fauna.protocol.malformed` — use an alias instead). Stored at the
    /// plaintext routing-metadata floor in both storage modes.
    pub async fn set_forward_all_to(&self, forward_all_to: Option<String>) -> Result<(), R::Error> {
        let _: SetForwardAllToReply = self
            .nest
            .request(
                "fauna.bridges.set_forward_all_to",
                SetForwardAllToRequest { forward_all_to },
            )
            .await?;
        Ok(())
    }

    /// `set_forward_all_to` then a confirming re-read, so the caller reflects
    /// the **persisted** value (the nest trims, and a blank clears), never the
    /// local edit — the [`Self::set_spam_threshold_override_and_reload`] shape.
    pub async fn set_forward_all_to_and_reload(
        &self,
        forward_all_to: Option<String>,
    ) -> Result<Option<String>, R::Error> {
        self.set_forward_all_to(forward_all_to).await?;
        self.get_forward_all_to().await
    }

    /// `fauna.bridges.get_forward_per_hour` — the calling actor's hourly
    /// forward cap (`mail.account.forward_per_hour`, default 100) and the admin
    /// ceiling it may not exceed (`mail-forwarding.md` § Per-account forward
    /// rate-limit) — the ceiling bounds the app's field. Replay-safe pure read.
    pub async fn get_forward_per_hour(&self) -> Result<GetForwardPerHourReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.get_forward_per_hour",
                GetForwardPerHourRequest {},
            )
            .await
    }

    /// `fauna.bridges.set_forward_per_hour` — overwrite the hourly forward cap.
    /// The nest refuses a value outside `1..=ceiling` with
    /// `fauna.protocol.malformed`; `fauna_mail::validate_forward_per_hour` is
    /// the same check for an app to explain the refusal before the round trip.
    pub async fn set_forward_per_hour(&self, forward_per_hour: u32) -> Result<(), R::Error> {
        let _: SetForwardPerHourReply = self
            .nest
            .request(
                "fauna.bridges.set_forward_per_hour",
                SetForwardPerHourRequest {
                    forward_per_hour,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `set_forward_per_hour` then a confirming re-read (value + ceiling), the
    /// [`Self::set_forward_all_to_and_reload`] shape.
    pub async fn set_forward_per_hour_and_reload(
        &self,
        forward_per_hour: u32,
    ) -> Result<GetForwardPerHourReply, R::Error> {
        self.set_forward_per_hour(forward_per_hour).await?;
        self.get_forward_per_hour().await
    }

    /// `fauna.bridges.get_spam_threshold_override` — the calling actor's
    /// per-account spam-folder threshold in whole points, or `None` when the
    /// account follows the admin default (`mail-aliases.md` § Spam-threshold
    /// override — the middle of `per-alias > per-account > admin`;
    /// `mail-policy-config.md` § Tier 3). Replay-safe pure read.
    pub async fn get_spam_threshold_override(&self) -> Result<Option<u32>, R::Error> {
        let reply: GetSpamThresholdOverrideReply = self
            .nest
            .request(
                "fauna.bridges.get_spam_threshold_override",
                GetSpamThresholdOverrideRequest {},
            )
            .await?;
        Ok(reply.spam_threshold_override)
    }

    /// `fauna.bridges.set_spam_threshold_override` — set the per-account
    /// threshold, or `None` to follow the admin default again. `Some(0)` is a
    /// setting, not a clear: it turns auto-Junk filing off for this account.
    /// The resolved value is frozen onto each message at delivery, so a change
    /// applies to newly delivered mail, never to mail already filed.
    pub async fn set_spam_threshold_override(
        &self,
        spam_threshold_override: Option<u32>,
    ) -> Result<(), R::Error> {
        let _: SetSpamThresholdOverrideReply = self
            .nest
            .request(
                "fauna.bridges.set_spam_threshold_override",
                SetSpamThresholdOverrideRequest {
                    spam_threshold_override,
                },
            )
            .await?;
        Ok(())
    }

    /// `set_spam_threshold_override` then a confirming re-read, so the caller
    /// reflects the **persisted** value, never the local edit — the shape
    /// every one of the four apps hand-copied around this pair.
    pub async fn set_spam_threshold_override_and_reload(
        &self,
        spam_threshold_override: Option<u32>,
    ) -> Result<Option<u32>, R::Error> {
        self.set_spam_threshold_override(spam_threshold_override)
            .await?;
        self.get_spam_threshold_override().await
    }

    // ── User-tier spam-classifier management (the `mail-spam` page) ──────────
    //
    // The four caller-scoped RPCs the user manages their own per-account Bayesian
    // classifier with (`mail-spam.md` §§ Reset, Training-sample retention, Undo,
    // Cold start Path 2). Like the alias kinds above these are `User`-gated and
    // the nest derives the subject from the authenticated caller — there is no
    // target-actor parameter, a user touches only their own model + history.

    /// `fauna.bridges.list_spam_training_history` — the caller's recent
    /// training-history rows (newest-first) plus the `contribute_baseline`
    /// read-back the `mail-spam-contribute-baseline-toggle` renders. Replay-safe
    /// pure read; the request is left at its `Default` (newest page, the nest's
    /// own default limit + cap). Returns the whole reply — `rpc_glue` projects it
    /// into the `mail-spam` page's view shape.
    pub async fn list_spam_training_history(
        &self,
    ) -> Result<ListSpamTrainingHistoryReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_spam_training_history",
                ListSpamTrainingHistoryRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.fetch_spam_model` — the caller's per-user spam model,
    /// **sealed to their MSEK-derived key** (the same `fauna_mls::wrapped_blob`
    /// shape as a mail body), for the on-device post-decrypt scorer — the
    /// User/Fauna-app leg of the "one shared scorer, every position" shape
    /// (`mail-spam.md` § Scoring placement; the Rust twin of the Go MDA's
    /// `wsrpc.FetchSpamModel`). The caller unwraps the returned bytes on-device
    /// (the same `open_inbound_record` decrypt it runs on a mail body), then
    /// scores with `weighted_bayesian_milli_for_model` — the model never crosses
    /// to nest in the clear. `None` ⇒ no trained model yet ⇒ cold start (the
    /// scorer weight is 0 below `bayesian_min_samples`).
    ///
    /// Caller-scoped: the handler enforces `target == caller` (no one reads another user's model, even an admin), so `actor_id` must be
    /// the caller's own. It is carried explicitly because the request schema is
    /// shared with the `BridgeMda` leg (which serves a named local-mail actor);
    /// the on-device caller passes its own id. Replay-safe pure read.
    ///
    /// Returns the full decoded reply: alongside `blob`, the `stored_sealed`
    /// dispatch signal and — for a client-sealed stored model — the additive
    /// `baseline` field carrying the published deployment aggregate the agent
    /// folds locally (`SpamModel::fold_baseline_faded` — the no-double-fold
    /// rule, `mail-spam.md` § Encrypted-mode interaction). Callers that need
    /// only the sealed bytes read `.blob`.
    pub async fn fetch_spam_model(
        &self,
        actor_id: Vec<u8>,
    ) -> Result<FetchSpamModelReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.fetch_spam_model",
                FetchSpamModelRequest {
                    actor_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.put_spam_model` — write the caller's whole **re-sealed**
    /// per-user spam model back opaque (the sealed-write-back twin of
    /// [`Self::fetch_spam_model`]). When the model is sealed at rest the nest can
    /// no longer read-mutate-write it, so the client runs the whole
    /// unwrap → mutate → re-seal loop (shared `fauna_mail::spam::model_write
    /// ::apply_and_reseal[_hybrid]`) and hands the resulting opaque blob here; the
    /// nest stores it verbatim into `spam_models.model_json` (no decode, no merge —
    /// `mail-spam.md:355`). Caller-scoped: this client caller leaves `actor_id`
    /// unset, so the connection's authenticated actor *is* the subject and a
    /// caller can only write their own row (even an admin). Nest-side, a
    /// `User`/`Admin` naming any *other* actor is rejected (the
    /// mirror); only the `BridgeMda` leg-2 agent-side
    /// write-back names a target (trusted-naming).
    ///
    /// `sample_count` is **advisory** (the post-mutation training-document count,
    /// for the settings display only) — the nest never trusts it against the
    /// opaque blob. Returns the reply's [`PutSpamModelOutcome`]: `Written`, or
    /// `DuplicateSignal` when the one-lesson rule (`mail-spam.md` § 3) rejected
    /// a `history_op: Insert` that repeats the actor's newest recorded lesson for
    /// that message — then NOTHING was written and the caller must not treat its
    /// mutated model as the stored one. This is the *only* model-write path: the
    /// model rests sealed, so the nest has no server-side train/undo of its own.
    ///
    /// `history_op` rides the same kind so the model re-seal and its audit-row
    /// mutation commit **atomically** (build-item 3 write side, option (a)):
    /// [`SpamHistoryOp::Insert`] for a client-path train (`{model-write +
    /// history-INSERT}`, carrying the client-sealed subject + delta),
    /// [`SpamHistoryOp::Delete`] for a client-side undo (`{model-write +
    /// history-DELETE}`), or `None` for a model-only write (a moderation-queue
    /// *social* train, or the initial holder-copy re-seal).
    pub async fn put_spam_model(
        &self,
        sealed_model: Vec<u8>,
        sample_count: u32,
        history_op: Option<SpamHistoryOp>,
        holder_copy: Option<SpamModelHolderCopy>,
    ) -> Result<PutSpamModelOutcome, R::Error> {
        let reply: PutSpamModelReply = self
            .nest
            .request(
                "fauna.bridges.put_spam_model",
                PutSpamModelRequest {
                    sealed_model,
                    sample_count,
                    history_op,
                    // The deployment-baseline holder copy the writer sealed to the
                    // aggregation holder while opted in (piece (b), `mail-spam.md`
                    // § Encrypted-mode interaction) — stored atomically with the
                    // model; `None` ⇒ opted out / no holder / server-path write, and
                    // the nest leaves the stored copy untouched.
                    holder_copy,
                    // `actor_id` left unset (Default): this is the client's own-key
                    // path — the connection's actor is the subject (naming any other
                    // actor is rejected for a `User`/`Admin`; only the `BridgeMda`
                    // leg-2 write-back names a target). Covers `extra` too.
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.outcome)
    }

    /// `fauna.bridges.get_spam_scoring_policy` — the admin-effective spam-scoring
    /// policy (the `spam_folder` threshold + the three Bayesian knobs) the
    /// on-device Fauna-app scorer must consume so its INBOX→Junk line is
    /// byte-identical to the MDA/nest at every scoring position (`mail-spam.md`
    /// § Scoring placement, § Combined-score formula). The User-reachable read of
    /// the same effective policy the MDA reads Go-side via `fetch_config` (the
    /// Admin-only `get_mail_config` twin is unreachable to a client). The values
    /// are deployment-wide (no per-user override today); the whole reply is
    /// returned for the caller's scorer to project. Replay-safe pure read.
    pub async fn get_spam_scoring_policy(&self) -> Result<GetSpamScoringPolicyReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.get_spam_scoring_policy",
                GetSpamScoringPolicyRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.reset_spam_model` — delete the caller's per-user model +
    /// all their training history (`mail-spam.md` § Reset). Irreversible but
    /// idempotent; the typed reply carries no payload.
    pub async fn reset_spam_model(&self) -> Result<(), R::Error> {
        let _: ResetSpamModelReply = self
            .nest
            .request(
                "fauna.bridges.reset_spam_model",
                ResetSpamModelRequest::default(),
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.set_baseline_contribution` — opt the caller's training
    /// in/out of the deployment baseline (`mail-spam-contribute-baseline-toggle`,
    /// `mail-spam.md` § Cold start Path 2). The echoed flag is discarded; the page
    /// re-reads it via `list_spam_training_history`.
    pub async fn set_baseline_contribution(&self, contribute: bool) -> Result<(), R::Error> {
        let _: SetBaselineContributionReply = self
            .nest
            .request(
                "fauna.bridges.set_baseline_contribution",
                SetBaselineContributionRequest {
                    contribute,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    // ── mail-lists / mail-list-members (mail-mass-mailing.md § Wire shapes) ──
    //
    // The nine user-tier list RPCs. All are caller-scoped — the nest derives the
    // owning actor from the authenticated connection and refuses a `list_id` the
    // caller does not own — so they need no actor argument, exactly like the
    // alias + spam methods above. Being generic over `R: RpcRequester`, these
    // serve the native (`Arc<NestClient>`) and wasm (`WsRpcClient`) seams from
    // one implementation (priority #2).

    /// `fauna.bridges.list_account_lists` — the calling actor's own mailing
    /// lists. Replay-safe pure read.
    pub async fn list_account_lists(&self) -> Result<Vec<MailListRow>, R::Error> {
        let reply: ListAccountListsReply = self
            .nest
            .request(
                "fauna.bridges.list_account_lists",
                ListAccountListsRequest {},
            )
            .await?;
        Ok(reply.lists)
    }

    /// [`Self::list_account_lists`] with the per-account meter the reply also
    /// carries (`account_recipients_today` / `account_recipients_per_day`) — the
    /// compose form's "Today's quota: N / M" (`mail-mass-mailing.md`
    /// § Composing a list message).
    pub async fn list_account_lists_with_meter(&self) -> Result<ListAccountListsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_account_lists",
                ListAccountListsRequest {},
            )
            .await
    }

    /// `fauna.bridges.create_account_list` — create one list owned by the caller;
    /// returns its new 16-byte UUID. The nest validates `local_part` (strict
    /// ASCII, ≤64, not a creation-reserved local-part per
    /// `mail-mass-mailing.md` § Reserved local-part) and surfaces a collision as
    /// `fauna.bridges.conflicts_with_existing_alias`; a `recipients_per_send`
    /// above the admin ceiling is rejected `malformed`.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_account_list(
        &self,
        local_part: impl Into<String>,
        local_domain: impl Into<String>,
        friendly_name: Option<String>,
        description: Option<String>,
        list_help_url: Option<String>,
        list_archive_url: Option<String>,
        recipients_per_send: Option<i64>,
    ) -> Result<Vec<u8>, R::Error> {
        let reply: CreateAccountListReply = self
            .nest
            .request(
                "fauna.bridges.create_account_list",
                CreateAccountListRequest {
                    local_part: local_part.into(),
                    local_domain: local_domain.into(),
                    friendly_name,
                    description,
                    list_help_url,
                    list_archive_url,
                    recipients_per_send,
                },
            )
            .await?;
        Ok(reply.list_id.into_vec())
    }

    /// `fauna.bridges.update_account_list` — full-overwrite of a caller-owned
    /// list's editable metadata (the posting address is immutable, like an alias
    /// `kind`). The typed `{ ok }` reply is discarded; a non-owned `list_id`
    /// surfaces as `fauna.bridges.not_found`.
    pub async fn update_account_list(
        &self,
        list_id: Vec<u8>,
        friendly_name: Option<String>,
        description: Option<String>,
        list_help_url: Option<String>,
        list_archive_url: Option<String>,
        recipients_per_send: Option<i64>,
    ) -> Result<(), R::Error> {
        let _: UpdateAccountListReply = self
            .nest
            .request(
                "fauna.bridges.update_account_list",
                UpdateAccountListRequest {
                    list_id: ByteBuf::from(list_id),
                    friendly_name,
                    description,
                    list_help_url,
                    list_archive_url,
                    recipients_per_send,
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.delete_account_list` — destructive; cascades the alias row
    /// → list row → all member rows in one transaction.
    pub async fn delete_account_list(&self, list_id: Vec<u8>) -> Result<(), R::Error> {
        let _: DeleteAccountListReply = self
            .nest
            .request(
                "fauna.bridges.delete_account_list",
                DeleteAccountListRequest {
                    list_id: ByteBuf::from(list_id),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.list_list_members` — a caller-owned list's members plus the
    /// subscribed / unsubscribed summary counts. `include_unsubscribed` widens
    /// the rows; the counts are returned either way.
    pub async fn list_list_members(
        &self,
        list_id: Vec<u8>,
        include_unsubscribed: bool,
    ) -> Result<ListListMembersReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_list_members",
                ListListMembersRequest {
                    list_id: ByteBuf::from(list_id),
                    include_unsubscribed,
                },
            )
            .await
    }

    /// `fauna.bridges.add_list_member` — subscribe one address. Idempotent: a
    /// duplicate leaves the existing membership untouched (subscription is
    /// sticky), so the `added` flag is discarded here.
    pub async fn add_list_member(
        &self,
        list_id: Vec<u8>,
        recipient_address: impl Into<String>,
    ) -> Result<(), R::Error> {
        let _: AddListMemberReply = self
            .nest
            .request(
                "fauna.bridges.add_list_member",
                AddListMemberRequest {
                    list_id: ByteBuf::from(list_id),
                    recipient_address: recipient_address.into(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.batch_import_list_members` — bulk-subscribe; returns the
    /// tally the import sheet renders. Invalid + local-domain addresses are
    /// skipped and counted, never fatal.
    pub async fn batch_import_list_members(
        &self,
        list_id: Vec<u8>,
        addresses: Vec<String>,
    ) -> Result<BatchImportListMembersReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.batch_import_list_members",
                BatchImportListMembersRequest {
                    list_id: ByteBuf::from(list_id),
                    addresses,
                },
            )
            .await
    }

    /// `fauna.bridges.unsubscribe_list_member` — the **manual** owner-driven
    /// unsubscribe by address. The one-click token form is internal to the
    /// HTTPS/mailto handlers and never reaches this RPC. Idempotent.
    pub async fn unsubscribe_list_member(
        &self,
        list_id: Vec<u8>,
        recipient_address: impl Into<String>,
    ) -> Result<(), R::Error> {
        let _: UnsubscribeListMemberReply = self
            .nest
            .request(
                "fauna.bridges.unsubscribe_list_member",
                UnsubscribeListMemberRequest {
                    list_id: ByteBuf::from(list_id),
                    recipient_address: recipient_address.into(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.resubscribe_list_member` — the only way back from a sticky
    /// unsubscribe (`mail-mass-mailing.md` § Architectural rules).
    pub async fn resubscribe_list_member(
        &self,
        list_id: Vec<u8>,
        recipient_address: impl Into<String>,
    ) -> Result<(), R::Error> {
        let _: ResubscribeListMemberReply = self
            .nest
            .request(
                "fauna.bridges.resubscribe_list_member",
                ResubscribeListMemberRequest {
                    list_id: ByteBuf::from(list_id),
                    recipient_address: recipient_address.into(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.bridges.send_list_message` — the ONLY list-send path
    /// (`mail-mass-mailing.md` § Composing a list message): the nest validates
    /// ownership, reserves the per-send / per-account / per-deployment caps
    /// atomically, stamps each member's `List-*` headers and enqueues one
    /// outbound per subscribed member. `message` is the composed RFC 5322
    /// bytes. Over a cap the nest refuses with the 552/452-class error the
    /// compose form explains (§ Per-list rate accounting). Online-only: a
    /// send never queues offline.
    pub async fn send_list_message(
        &self,
        list_id: Vec<u8>,
        message: Vec<u8>,
    ) -> Result<SendListMessageReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.send_list_message",
                SendListMessageRequest {
                    list_id: ByteBuf::from(list_id),
                    message: ByteBuf::from(message),
                },
            )
            .await
    }

    /// `fauna.bridges.list_list_send_history` — a caller-owned list's send
    /// audit, newest first (`limit` 0 ⇒ the nest's default page). The compose
    /// form renders the newest row as the send's one whole-send progress.
    pub async fn list_list_send_history(
        &self,
        list_id: Vec<u8>,
        limit: u32,
    ) -> Result<Vec<ListSendHistoryRow>, R::Error> {
        let reply: ListListSendHistoryReply = self
            .nest
            .request(
                "fauna.bridges.list_list_send_history",
                ListListSendHistoryRequest {
                    list_id: ByteBuf::from(list_id),
                    limit,
                },
            )
            .await?;
        Ok(reply.sends)
    }
}

/// Whether a `fauna.bridges.list` row belongs on the **unified** Bridges page
/// (`bridges.md` § Scope). Nostr and Bluesky each have their own dedicated page
/// (like mail) — deep integrations important enough to warrant it — so the
/// unified page must exclude `bridge_id:"nostr"` and `"bluesky"` even though they
/// ride the same shared wire (Bluesky's Linked-account card migrated onto the
/// dedicated `atproto` page — `ui/atproto.md` § Migration). `list()` itself stays
/// unfiltered (each dedicated page's own fetch needs its row), so this is a
/// client-side filter, not a server-side one.
///
/// Was hand-rolled identically (`.filter(|b| b.id != "nostr")` /
/// `filter { it.id != "nostr" }` / `.filter((b) => b.id !== 'nostr')`) on
/// every app that has built the unified page — one source of the
/// exclusion rule so a future second dedicated-page bridge type doesn't need
/// each app to remember to add its id here too. (Until every app has the
/// AT Protocol page, clients that consume this predicate but lack the page simply
/// show no Bluesky card — a no-op today: the `BlueskyProvider` is registered only
/// under the nest's `bluesky` feature, absent from production/default builds, so
/// `fauna.bridges.list` returns no `bluesky` row to filter — `ui/atproto.md`
/// § Migration point 2.)
pub fn is_unified_bridges_page_bridge(id: &str) -> bool {
    id != "nostr" && id != "bluesky"
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = BridgesClient::new(MockRequester);
    }

    #[test]
    fn mail_admin_constructor_builds_over_generic_requester() {
        let _c = MailAdminClient::new(MockRequester);
    }

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        // Answer with a reply the requested `Reply` type decodes. The
        // `provision_*` kinds + the `{ ok }` alias replies
        // (update/revoke/delete) share the `ProvisionReply` shape; the
        // create/list alias kinds have distinct shapes.
        match kind {
            "fauna.bridges.create_account_alias" => {
                fauna_protocol::encode_canonical(&CreateAccountAliasReply {
                    alias_id: ByteBuf::from(vec![0x11u8; 16]),
                })
            }
            "fauna.bridges.list_account_aliases" => {
                fauna_protocol::encode_canonical(&ListAccountAliasesReply { aliases: vec![] })
            }
            "fauna.bridges.generate_disposable_alias" => {
                fauna_protocol::encode_canonical(&GenerateDisposableAliasReply {
                    alias_id: ByteBuf::from(vec![0x22u8; 16]),
                    full_address: "bob-temp-a2b3c4@example.com".into(),
                    token: "a2b3c4".into(),
                })
            }
            "fauna.bridges.list_account_alias_hits" => {
                fauna_protocol::encode_canonical(&ListAccountAliasHitsReply {
                    hits: vec![AliasHitRow {
                        hit_id: ByteBuf::from(vec![0x33u8; 16]),
                        matched_address: "bob-amazon@example.com".into(),
                        sender_domain: "amazon.com".into(),
                        received_at: 1_700_000_000,
                    }],
                })
            }
            "fauna.bridges.create_forwarder" => {
                fauna_protocol::encode_canonical(&CreateForwarderReply {
                    alias_id: ByteBuf::from(vec![0x44u8; 16]),
                })
            }
            "fauna.bridges.list_forwarders" => {
                fauna_protocol::encode_canonical(&ListForwardersReply { forwarders: vec![] })
            }
            "fauna.bridges.get_forward_all_to" => {
                fauna_protocol::encode_canonical(&GetForwardAllToReply {
                    forward_all_to: Some("alice@example.net".into()),
                })
            }
            "fauna.bridges.set_forward_all_to" => {
                fauna_protocol::encode_canonical(&SetForwardAllToReply {})
            }
            "fauna.bridges.get_forward_per_hour" => {
                fauna_protocol::encode_canonical(&GetForwardPerHourReply {
                    forward_per_hour: 40,
                    forward_per_hour_ceiling: 500,
                    ..Default::default()
                })
            }
            "fauna.bridges.set_forward_per_hour" => {
                fauna_protocol::encode_canonical(&SetForwardPerHourReply {})
            }
            "fauna.bridges.get_spam_threshold_override" => {
                fauna_protocol::encode_canonical(&GetSpamThresholdOverrideReply {
                    spam_threshold_override: Some(3),
                })
            }
            "fauna.bridges.set_spam_threshold_override" => {
                fauna_protocol::encode_canonical(&SetSpamThresholdOverrideReply {})
            }
            "fauna.bridges.list_spam_training_history" => {
                use fauna_protocol::bridge_routing::{
                    SpamLabel, SpamTrainingHistoryRow, TrainingSource,
                };
                fauna_protocol::encode_canonical(&ListSpamTrainingHistoryReply {
                    events: vec![SpamTrainingHistoryRow {
                        history_id: vec![0x55u8; 16],
                        message: "Cheap pills · INBOX".into(),
                        sealed_subject: Default::default(),
                        mailbox: "INBOX".into(),
                        label: SpamLabel::Spam,
                        source: TrainingSource::ImapJunkFlag,
                        created_at_ms: 1_700_000_000_000,
                        model_delta_applied: br#"["cheap","pills"]"#.to_vec(),
                        extra: Default::default(),
                    }],
                    contribute_baseline: true,
                    extra: Default::default(),
                })
            }
            "fauna.bridges.set_baseline_contribution" => {
                fauna_protocol::encode_canonical(&SetBaselineContributionReply {
                    contribute: true,
                    extra: Default::default(),
                })
            }
            "fauna.bridges.list_account_lists" => {
                fauna_protocol::encode_canonical(&ListAccountListsReply {
                    lists: vec![MailListRow {
                        list_id: ByteBuf::from(vec![0xa1u8; 16]),
                        alias_id: ByteBuf::from(vec![0xa2u8; 16]),
                        owner_actor_id: ByteBuf::from(vec![0xa3u8; 32]),
                        local_domain: "example.com".into(),
                        pattern: "bob-weekly".into(),
                        friendly_name: Some("Bob's Weekly".into()),
                        description: Some("A newsletter".into()),
                        list_help_url: None,
                        list_archive_url: None,
                        recipients_per_send: Some(2500),
                        created_at: 1_700_000_000_000,
                        last_send_at: Some(1_700_000_500_000),
                        member_count: 3,
                        sends_today: 1,
                        recipients_today: 3,
                    }],
                    account_recipients_today: 3,
                    account_recipients_per_day: 20_000,
                })
            }
            "fauna.bridges.create_account_list" => {
                fauna_protocol::encode_canonical(&CreateAccountListReply {
                    list_id: ByteBuf::from(vec![0xa1u8; 16]),
                })
            }
            "fauna.bridges.update_account_list" => {
                fauna_protocol::encode_canonical(&UpdateAccountListReply { ok: true })
            }
            "fauna.bridges.delete_account_list" => {
                fauna_protocol::encode_canonical(&DeleteAccountListReply { ok: true })
            }
            "fauna.bridges.list_list_members" => {
                fauna_protocol::encode_canonical(&ListListMembersReply {
                    members: vec![
                        MailListMemberRow {
                            member_id: ByteBuf::from(vec![0xb1u8; 16]),
                            recipient_address: "reader@example.net".into(),
                            subscribed_at: 1_700_000_000_000,
                            unsubscribed_at: None,
                        },
                        MailListMemberRow {
                            member_id: ByteBuf::from(vec![0xb2u8; 16]),
                            recipient_address: "gone@example.net".into(),
                            subscribed_at: 1_700_000_100_000,
                            unsubscribed_at: Some(1_700_000_900_000),
                        },
                    ],
                    subscribed_count: 1,
                    unsubscribed_count: 1,
                })
            }
            "fauna.bridges.add_list_member" => {
                fauna_protocol::encode_canonical(&AddListMemberReply {
                    member_id: ByteBuf::from(vec![0xb1u8; 16]),
                    added: true,
                })
            }
            "fauna.bridges.batch_import_list_members" => {
                fauna_protocol::encode_canonical(&BatchImportListMembersReply {
                    added: 2,
                    skipped_invalid: 1,
                    skipped_duplicate: 3,
                })
            }
            "fauna.bridges.unsubscribe_list_member" => {
                fauna_protocol::encode_canonical(&UnsubscribeListMemberReply { ok: true })
            }
            "fauna.bridges.resubscribe_list_member" => {
                fauna_protocol::encode_canonical(&ResubscribeListMemberReply { ok: true })
            }
            "fauna.bridges.send_list_message" => {
                fauna_protocol::encode_canonical(&SendListMessageReply {
                    queued_count: 3,
                    estimated_quota_remaining: 19_994,
                })
            }
            "fauna.bridges.list_list_send_history" => {
                fauna_protocol::encode_canonical(&ListListSendHistoryReply {
                    sends: vec![ListSendHistoryRow {
                        sent_at: 1_700_000_500_000,
                        recipient_count: 3,
                        delivered_count: 3,
                        unsubscribed_during_send: 0,
                    }],
                })
            }
            "fauna.bridges.fetch_bridge_pubkey" => {
                fauna_protocol::encode_canonical(&FetchBridgePubkeyReply {
                    extra: Default::default(),
                    ed25519_pubkey: vec![0x11u8; 32],
                    x25519_pubkey: vec![0x22u8; 32],
                    mlkem_ek: None,
                })
            }
            "fauna.bridges.list_dkim_selectors" => {
                fauna_protocol::encode_canonical(&ListDkimSelectorsReply {
                    extra: Default::default(),
                    selectors: vec![DkimSelectorInfo {
                        extra: Default::default(),
                        domain: "example.com".into(),
                        selector: "2026a".into(),
                        created_at: 1_700_000_000_000,
                        public_dns_value: "v=DKIM1; k=ed25519; p=AAA".into(),
                    }],
                })
            }
            "fauna.bridges.list_service_users" => {
                fauna_protocol::encode_canonical(&ListServiceUsersReply {
                    extra: Default::default(),
                    enrollment_strict: None,
                    service_users: vec![ServiceUserInfo {
                        extra: Default::default(),
                        bridge_id: "mta-1".into(),
                        role: "mta".into(),
                        status: "approved".into(),
                        ed25519_pubkey: vec![0x11u8; 32],
                        has_x25519: true,
                        created_at: 1_700_000_000_000,
                        approved_at: Some(1_700_000_100_000),
                        ..Default::default()
                    }],
                })
            }
            "fauna.bridges.list_pending_bridges" => {
                fauna_protocol::encode_canonical(&ListPendingBridgesReply {
                    extra: Default::default(),
                    bridges: vec![ServiceUserInfo {
                        extra: Default::default(),
                        bridge_id: "mta-1".into(),
                        role: "mta".into(),
                        status: "pending".into(),
                        ed25519_pubkey: vec![0x11u8; 32],
                        has_x25519: false,
                        created_at: 1_700_000_000_000,
                        approved_at: None,
                        ..Default::default()
                    }],
                })
            }
            "fauna.bridges.provision_self_signed_cert" => {
                use fauna_protocol::bridge_routing::SealedBridgeInfo;
                fauna_protocol::encode_canonical(&ProvisionSelfSignedCertReply {
                    bridges_sealed_to: vec![SealedBridgeInfo {
                        role: "mta".into(),
                        bridge_id: "mta-1".into(),
                    }],
                    bridges_skipped_no_x25519: vec![],
                    expires_at_unix: 1_900_000_000,
                })
            }
            "fauna.bridges.get_mail_config" => {
                fauna_protocol::encode_canonical(&FetchConfigReply::default())
            }
            "fauna.bridges.get_alias_policy" => {
                fauna_protocol::encode_canonical(&AliasPolicy::default())
            }
            "fauna.bridges.fetch_spam_model" => {
                fauna_protocol::encode_canonical(&FetchSpamModelReply {
                    blob: Some(ByteBuf::from(vec![0x99u8; 8])),
                    ..Default::default()
                })
            }
            "fauna.bridges.get_spam_scoring_policy" => {
                // Canned to the catalog defaults so the wrapper test proves
                // the four fields project through unchanged.
                fauna_protocol::encode_canonical(&GetSpamScoringPolicyReply {
                    spam_folder_threshold: 5,
                    bayesian_weight_milli: 700,
                    bayesian_min_samples: 50,
                    bayesian_full_confidence_samples: 200,
                    extra: Default::default(),
                })
            }
            "fauna.bridges.publish_spam_baseline" => {
                fauna_protocol::encode_canonical(&PublishSpamBaselineReply {
                    contributors: 4,
                    sample_count: 128,
                    published: true,
                    skipped_contributors: 0,
                    deferred: false,
                    extra: Default::default(),
                })
            }
            "fauna.bridges.mail_health" => fauna_protocol::encode_canonical(&MailHealthReply {
                state: "warming_up".into(),
                last_inbound_accepted_at: Some(1_700_000_000),
                ..Default::default()
            }),
            "fauna.bridges.get_spam_baseline_state" => {
                fauna_protocol::encode_canonical(&GetSpamBaselineStateReply {
                    published: true,
                    contributors: 4,
                    sample_count: 128,
                    published_at: Some(1_700_000_000_000),
                    skipped_contributors: 0,
                    deferred: true,
                    standing: true,
                    extra: Default::default(),
                })
            }
            "fauna.setup.status" => {
                // The read twin of `set_auto_enable_mail_for_new_users` reads
                // the flag off setup.status; answer with it OFF so the test
                // proves projection (default is ON).
                fauna_protocol::encode_canonical(&SetupStatusReply {
                    domain: "example.com".into(),
                    dns_configured: true,
                    tls_active: true,
                    email_enabled: true,
                    admin_exists: true,
                    claimed: true,
                    version: "test".into(),
                    mail_subsystem_ok: true,
                    auto_enable_mail_for_new_users: false,
                    registration_mode: None,
                    max_free_users: None,
                    subhandles: false,
                    age_verification_required: false,
                    max_storage_bytes: None,
                    cors_origins: Vec::new(),
                    serving_port: 443,
                    fronted_by_router: false,
                    dkim_records: Vec::new(),
                    os_security_updates_pending: 0,
                    os_reboot_pending: false,
                    os_reboot_deferred_since: None,
                    os_last_patched_at: None,
                    web_app_origin: "bundled".into(),
                    web_app_origin_target: None,
                    web_app_origin_domainless: false,
                    node_mode: "public".into(),
                    extra: Default::default(),
                })
            }
            _ => fauna_protocol::encode_canonical(&ProvisionReply {
                extra: Default::default(),
                ok: true,
            }),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn provision_self_signed_cert_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let reply = block_on(
            client.provision_self_signed_cert("mail.example.com", vec!["example.com".into()]),
        )
        .expect("infallible mock");
        assert_eq!(reply.bridges_sealed_to.len(), 1);
        assert_eq!(reply.bridges_sealed_to[0].role, "mta");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.provision_self_signed_cert");
        let req: ProvisionSelfSignedCertRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.domain, "mail.example.com");
        assert_eq!(req.additional_dns_sans, vec!["example.com".to_string()]);
    }

    #[test]
    fn fetch_bridge_pubkey_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let reply = block_on(client.fetch_bridge_pubkey("mta", "mta-1")).expect("infallible mock");
        assert_eq!(reply.x25519_pubkey.len(), 32);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.fetch_bridge_pubkey");
        let req: FetchBridgePubkeyRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.bridge_role, "mta");
        assert_eq!(req.bridge_id, "mta-1");
    }

    #[test]
    fn list_dkim_selectors_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let reply = block_on(client.list_dkim_selectors(Some("example.com".into())))
            .expect("infallible mock");
        assert_eq!(reply.selectors.len(), 1);
        assert_eq!(
            reply.selectors[0].public_dns_value,
            "v=DKIM1; k=ed25519; p=AAA"
        );

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.list_dkim_selectors");
        let req: ListDkimSelectorsRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.domain.as_deref(), Some("example.com"));
    }

    #[test]
    fn list_service_users_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let reply =
            block_on(client.list_service_users(Some("mta".into()), Some("approved".into())))
                .expect("infallible mock");
        assert_eq!(reply.service_users.len(), 1);
        assert_eq!(reply.service_users[0].bridge_id, "mta-1");
        assert!(reply.service_users[0].has_x25519);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.list_service_users");
        let req: ListServiceUsersRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.role.as_deref(), Some("mta"));
        assert_eq!(req.status.as_deref(), Some("approved"));
    }

    #[test]
    fn publish_spam_baseline_composes_kind_and_returns_aggregate() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let reply = block_on(client.publish_spam_baseline()).expect("infallible mock");
        assert!(reply.published);
        assert_eq!(reply.contributors, 4);
        assert_eq!(reply.sample_count, 128);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.publish_spam_baseline");
        let _req: PublishSpamBaselineRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn mail_health_composes_kind_and_returns_the_readout() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let reply = block_on(client.mail_health()).expect("infallible mock");
        assert_eq!(reply.state, "warming_up");
        assert_eq!(reply.last_inbound_accepted_at, Some(1_700_000_000));

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.mail_health");
        let _req: MailHealthRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn get_spam_baseline_state_composes_kind_and_returns_state() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let reply = block_on(client.get_spam_baseline_state()).expect("infallible mock");
        assert!(reply.published && reply.deferred && reply.standing);
        assert_eq!(reply.contributors, 4);
        assert_eq!(reply.published_at, Some(1_700_000_000_000));

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.get_spam_baseline_state");
        let _req: GetSpamBaselineStateRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn list_pending_bridges_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let reply = block_on(client.list_pending_bridges()).expect("infallible mock");
        assert_eq!(reply.bridges.len(), 1);
        assert_eq!(reply.bridges[0].status, "pending");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.list_pending_bridges");
        let _req: ListPendingBridgesRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn approve_pending_bridge_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.approve_pending_bridge(vec![0x11u8; 32], "mta")).expect("infallible mock");

        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.approve_pending_bridge");
        let req: ApprovePendingBridgeRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.ed25519_pubkey.as_slice(), &[0x11u8; 32][..]);
        assert_eq!(req.role, "mta");
    }

    #[test]
    fn reject_pending_bridge_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.reject_pending_bridge(vec![0x22u8; 32])).expect("infallible mock");

        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.reject_pending_bridge");
        let req: RejectPendingBridgeRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.ed25519_pubkey.as_slice(), &[0x22u8; 32][..]);
    }

    #[test]
    fn set_mail_enabled_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.set_mail_enabled(true)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_mail_enabled");
        let req: SetMailEnabledRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.enabled);

        block_on(client.set_mail_enabled(false)).expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: SetMailEnabledRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(!req.enabled);
    }

    #[test]
    fn set_caldav_enabled_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.set_caldav_enabled(true)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_caldav_enabled");
        let req: SetCalDavEnabledRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.enabled);

        block_on(client.set_caldav_enabled(false)).expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: SetCalDavEnabledRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(!req.enabled);
    }

    #[test]
    fn set_carddav_enabled_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.set_carddav_enabled(true)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_carddav_enabled");
        let req: SetCardDavEnabledRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.enabled);

        block_on(client.set_carddav_enabled(false)).expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: SetCardDavEnabledRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(!req.enabled);
    }

    #[test]
    fn set_webdav_enabled_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.set_webdav_enabled(true)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_webdav_enabled");
        let req: SetWebDavEnabledRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.enabled);

        block_on(client.set_webdav_enabled(false)).expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: SetWebDavEnabledRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(!req.enabled);
    }

    #[test]
    fn set_caldav_port_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.set_caldav_port(8443)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_caldav_port");
        let req: SetCaldavPortRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.port, 8443);

        block_on(client.set_caldav_port(9443)).expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: SetCaldavPortRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.port, 9443);
    }

    #[test]
    fn set_auto_enable_mail_for_new_users_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.set_auto_enable_mail_for_new_users(true)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_auto_enable_mail_for_new_users");
        let req: SetAutoEnableMailForNewUsersRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.enabled);

        block_on(client.set_auto_enable_mail_for_new_users(false)).expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: SetAutoEnableMailForNewUsersRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(!req.enabled);
    }

    #[test]
    fn get_auto_enable_mail_for_new_users_reads_setup_status() {
        // The read twin issues `fauna.setup.status` and projects the policy flag
        // (not in `FetchConfigReply` — the bridge never reads it). The mock's
        // setup.status arm answers with the flag OFF.
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let enabled =
            block_on(client.get_auto_enable_mail_for_new_users()).expect("infallible mock");
        let (kind, _) = last_call(&rec);
        assert_eq!(kind, "fauna.setup.status");
        assert!(
            !enabled,
            "projects SetupStatusReply.auto_enable_mail_for_new_users"
        );
    }

    #[test]
    fn get_mail_config_composes_kind_and_decodes_reply() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let cfg = block_on(client.get_mail_config()).expect("infallible mock");
        let (kind, _payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.get_mail_config");
        // The mock answers the catalog-default config; the typed reply decodes.
        assert_eq!(cfg, FetchConfigReply::default());
    }

    #[test]
    fn get_alias_policy_composes_kind_and_decodes_reply() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        let policy = block_on(client.get_alias_policy()).expect("infallible mock");
        let (kind, _payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.get_alias_policy");
        // The mock answers the catalog-default alias policy; the typed reply
        // decodes (the read half of the nest-side write-path split).
        assert_eq!(policy, AliasPolicy::default());
    }

    #[test]
    fn revoke_service_user_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.revoke_service_user(vec![0x33u8; 32])).expect("infallible mock");

        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.revoke_service_user");
        let req: RevokeServiceUserRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.bridge_actor_id.as_slice(), &[0x33u8; 32][..]);
    }

    #[test]
    fn revoke_dkim_blob_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());

        block_on(client.revoke_dkim_blob("example.com", "2026a")).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.revoke_dkim_blob");
        let req: RevokeDkimBlobRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.domain, "example.com");
        assert_eq!(req.selector, "2026a");
    }

    #[test]
    fn provision_tls_cert_blob_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());
        let sealed = vec![0xCDu8; 128];

        block_on(client.provision_tls_cert_blob(sealed.clone())).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.provision_tls_cert_blob");
        let req: ProvisionTlsCertBlobRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as the request type");
        assert_eq!(req.blob.as_ref(), sealed.as_slice());
    }

    // ── A3 Bucket B — put_<substruct>_policy wrappers ───────────────
    //
    // The RecordingRequester's default branch answers `{ ok: true }`,
    // which `PutPolicyReply` decodes; each test asserts the wrapper picks
    // the right kind string and that its payload round-trips to the
    // request it was handed (the realistic forwarder failure: kind typo /
    // wrong type).

    #[test]
    fn put_spam_policy_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());
        let policy = PutSpamPolicyRequest {
            dnsbl_servers: Some(vec![]),
            max_score_before_reject: Some(20),
            fcrdns_mode: Some("off".into()),
            ..Default::default()
        };
        block_on(client.put_spam_policy(policy.clone())).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.put_spam_policy");
        assert_eq!(
            policy,
            fauna_protocol::decode_strict::<PutSpamPolicyRequest>(&payload).unwrap()
        );
    }

    #[test]
    fn put_auth_policy_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());
        let policy = PutAuthPolicyRequest {
            enforce_dkim: Some(true),
            log_only: Some(true),
            ..Default::default()
        };
        block_on(client.put_auth_policy(policy.clone())).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.put_auth_policy");
        assert_eq!(
            policy,
            fauna_protocol::decode_strict::<PutAuthPolicyRequest>(&payload).unwrap()
        );
    }

    #[test]
    fn put_submission_policy_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());
        let policy = PutSubmissionPolicyRequest {
            max_per_day: Some(500),
            max_recipients_per_message: Some(50),
            ..Default::default()
        };
        block_on(client.put_submission_policy(policy.clone())).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.put_submission_policy");
        assert_eq!(
            policy,
            fauna_protocol::decode_strict::<PutSubmissionPolicyRequest>(&payload).unwrap()
        );
    }

    #[test]
    fn put_imap_policy_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());
        let policy = PutImapPolicyRequest {
            delete_nonempty: Some("allowed".into()),
            storage_bytes_default: Some(2 << 30),
            ..Default::default()
        };
        block_on(client.put_imap_policy(policy.clone())).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.put_imap_policy");
        assert_eq!(
            policy,
            fauna_protocol::decode_strict::<PutImapPolicyRequest>(&payload).unwrap()
        );
    }

    #[test]
    fn put_outbound_policy_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());
        let policy = PutOutboundPolicyRequest {
            retry_schedule_seconds: Some(vec![0, 600, 3600]),
            ipv6_enabled: Some(false),
            ..Default::default()
        };
        block_on(client.put_outbound_policy(policy.clone())).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.put_outbound_policy");
        assert_eq!(
            policy,
            fauna_protocol::decode_strict::<PutOutboundPolicyRequest>(&payload).unwrap()
        );
    }

    #[test]
    fn put_alias_policy_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());
        let policy = PutAliasPolicyRequest {
            exact_aliases_max: Some(5),
            reserved_local_parts: Some(vec!["postmaster".into(), "sales".into()]),
            subaddressing_enabled: Some(false),
            ..Default::default()
        };
        block_on(client.put_alias_policy(policy.clone())).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.put_alias_policy");
        assert_eq!(
            policy,
            fauna_protocol::decode_strict::<PutAliasPolicyRequest>(&payload).unwrap()
        );
    }

    #[test]
    fn create_forwarder_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());
        block_on(client.create_forwarder("fauna.example", "info", "real@example.net"))
            .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.create_forwarder");
        let req = fauna_protocol::decode_strict::<CreateForwarderRequest>(&payload).unwrap();
        assert_eq!(req.local_domain, "fauna.example");
        assert_eq!(req.pattern, "info");
        assert_eq!(req.forward_target, "real@example.net");
    }

    #[test]
    fn delete_forwarder_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAdminClient::new(rec.clone());
        block_on(client.delete_forwarder(vec![9u8; 16])).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.delete_forwarder");
        let req = fauna_protocol::decode_strict::<DeleteForwarderRequest>(&payload).unwrap();
        assert_eq!(req.alias_id.as_ref(), &[9u8; 16][..]);
    }

    // ── MailAccountClient (user-tier alias surface, A2.1) ────────────

    #[test]
    fn mail_account_constructor_builds_over_generic_requester() {
        let _c = MailAccountClient::new(MockRequester);
    }

    fn last_call(rec: &std::sync::Arc<RecordingRequester>) -> (&'static str, Vec<u8>) {
        rec.recorded()
    }

    #[test]
    fn create_account_alias_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let controls = AliasControls {
            label: "work".into(),
            spam_threshold_override: Some(8),
            rate_limit_per_hour: Some(100),
            rate_limit_per_day: None,
        };
        let alias_id = block_on(client.create_account_alias(
            "exact",
            "example.com",
            "bob.smith",
            controls.clone(),
        ))
        .expect("infallible mock");
        // The mock's canned CreateAccountAliasReply alias_id is surfaced.
        assert_eq!(alias_id, vec![0x11u8; 16]);

        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.create_account_alias");
        let req: CreateAccountAliasRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.kind, "exact");
        assert_eq!(req.local_domain, "example.com");
        assert_eq!(req.pattern, "bob.smith");
        assert_eq!(req.controls, controls);
    }

    #[test]
    fn list_account_aliases_composes_kind() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let aliases = block_on(client.list_account_aliases()).expect("infallible mock");
        assert!(aliases.is_empty());
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.list_account_aliases");
        let _req: ListAccountAliasesRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
    }

    #[test]
    fn get_forward_all_to_composes_kind() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let got = block_on(client.get_forward_all_to()).expect("infallible mock");
        assert_eq!(got.as_deref(), Some("alice@example.net"));
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.get_forward_all_to");
        let _req: GetForwardAllToRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
    }

    #[test]
    fn set_forward_all_to_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        // Set.
        block_on(client.set_forward_all_to(Some("alice@example.net".into())))
            .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_forward_all_to");
        let req: SetForwardAllToRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.forward_all_to.as_deref(), Some("alice@example.net"));
        // Clear.
        block_on(client.set_forward_all_to(None)).expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: SetForwardAllToRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.forward_all_to, None);
    }

    /// The set→get pair every app's Forwarding field commits through: the
    /// field repaints from what the nest persisted, never from the keystrokes.
    #[test]
    fn set_forward_all_to_and_reload_sets_then_rereads() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let got = block_on(client.set_forward_all_to_and_reload(Some("alice@example.net".into())))
            .expect("infallible mock");
        assert_eq!(got.as_deref(), Some("alice@example.net"));
        assert_eq!(
            rec.kinds(),
            vec![
                "fauna.bridges.set_forward_all_to",
                "fauna.bridges.get_forward_all_to"
            ]
        );
    }

    #[test]
    fn set_forward_per_hour_and_reload_sets_then_rereads_with_the_ceiling() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let got = block_on(client.set_forward_per_hour_and_reload(40)).expect("infallible mock");
        assert_eq!(
            (got.forward_per_hour, got.forward_per_hour_ceiling),
            (40, 500)
        );
        assert_eq!(
            rec.kinds(),
            vec![
                "fauna.bridges.set_forward_per_hour",
                "fauna.bridges.get_forward_per_hour"
            ]
        );
    }

    #[test]
    fn get_forward_per_hour_composes_kind_and_returns_the_ceiling() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let got = block_on(client.get_forward_per_hour()).expect("infallible mock");
        assert_eq!(got.forward_per_hour, 40);
        assert_eq!(got.forward_per_hour_ceiling, 500);
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.get_forward_per_hour");
        let _req: GetForwardPerHourRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
    }

    #[test]
    fn set_forward_per_hour_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        block_on(client.set_forward_per_hour(25)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_forward_per_hour");
        let req: SetForwardPerHourRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.forward_per_hour, 25);
    }

    #[test]
    fn get_spam_threshold_override_composes_kind() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let got = block_on(client.get_spam_threshold_override()).expect("infallible mock");
        assert_eq!(got, Some(3));
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.get_spam_threshold_override");
        let _req: GetSpamThresholdOverrideRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
    }

    #[test]
    fn set_spam_threshold_override_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        block_on(client.set_spam_threshold_override(Some(6))).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_spam_threshold_override");
        let req: SetSpamThresholdOverrideRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.spam_threshold_override, Some(6));
        // Zero must survive the wire as a SETTING (auto-Junk off), distinct
        // from the clear below — the whole reason the field is `Option<u32>`.
        block_on(client.set_spam_threshold_override(Some(0))).expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: SetSpamThresholdOverrideRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.spam_threshold_override, Some(0));
        // Clear.
        block_on(client.set_spam_threshold_override(None)).expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: SetSpamThresholdOverrideRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.spam_threshold_override, None);
    }

    #[test]
    fn update_account_alias_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let id = vec![7u8; 16];
        let controls = AliasControls {
            label: "renamed".into(),
            ..Default::default()
        };
        block_on(client.update_account_alias(id.clone(), "bob.s", controls.clone()))
            .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.update_account_alias");
        let req: UpdateAccountAliasRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.alias_id.as_ref(), id.as_slice());
        assert_eq!(req.pattern, "bob.s");
        assert_eq!(req.controls, controls);
    }

    #[test]
    fn revoke_and_delete_account_alias_compose_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let id = vec![3u8; 16];

        block_on(client.revoke_account_alias(id.clone())).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.revoke_account_alias");
        let req: RevokeAccountAliasRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.alias_id.as_ref(), id.as_slice());

        block_on(client.delete_account_alias(id.clone())).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.delete_account_alias");
        let req: DeleteAccountAliasRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.alias_id.as_ref(), id.as_slice());
    }

    #[test]
    fn generate_disposable_alias_composes_kind_payload_and_returns_reply() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());

        let reply = block_on(client.generate_disposable_alias(Some(7), Some(0), "amazon"))
            .expect("infallible mock");
        // The wrapper returns the full reply (clients copy `full_address`).
        assert_eq!(reply.full_address, "bob-temp-a2b3c4@example.com");
        assert_eq!(reply.token, "a2b3c4");

        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.generate_disposable_alias");
        let req: GenerateDisposableAliasRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.ttl_days, Some(7));
        assert_eq!(req.uses, Some(0));
        assert_eq!(req.label, "amazon");
    }

    #[test]
    fn list_account_alias_hits_composes_kind_payload_and_returns_hits() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let alias_id = vec![0x44u8; 16];
        let cursor = vec![0x55u8; 16];

        let hits =
            block_on(client.list_account_alias_hits(alias_id.clone(), 100, Some(cursor.clone())))
                .expect("infallible mock");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sender_domain, "amazon.com");

        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.list_account_alias_hits");
        let req: ListAccountAliasHitsRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.alias_id.as_ref(), alias_id.as_slice());
        assert_eq!(req.limit, 100);
        assert_eq!(
            req.before_hit_id.as_ref().map(|b| b.as_ref()),
            Some(cursor.as_slice())
        );
    }

    // ── user-tier spam-classifier management (mail-spam page) ───────────────

    #[test]
    fn list_spam_training_history_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());

        let reply = block_on(client.list_spam_training_history()).expect("infallible mock");
        assert_eq!(reply.events.len(), 1);
        assert!(reply.contribute_baseline);

        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.list_spam_training_history");
        // Default request (newest page, no cursor/limit) — decodes cleanly.
        let req: ListSpamTrainingHistoryRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert!(req.limit.is_none());
        assert!(req.before_history_id.is_none());
    }

    #[test]
    fn reset_spam_model_composes_kind() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        block_on(client.reset_spam_model()).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.reset_spam_model");
        let _req: ResetSpamModelRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
    }

    #[test]
    fn set_baseline_contribution_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        block_on(client.set_baseline_contribution(true)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.set_baseline_contribution");
        let req: SetBaselineContributionRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert!(req.contribute);
    }

    // ── mail-lists / mail-list-members (mail-mass-mailing.md § Wire shapes) ──

    #[test]
    fn send_list_message_composes_kind_and_payload_and_surfaces_the_tally() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let msg = b"From: a@example.com\r\nSubject: Issue 1\r\n\r\nHi\r\n".to_vec();
        let out = block_on(client.send_list_message(vec![0xa1u8; 16], msg.clone()))
            .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.send_list_message");
        let req: SendListMessageRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.list_id, vec![0xa1u8; 16]);
        assert_eq!(req.message.as_slice(), msg.as_slice());
        assert_eq!(out.queued_count, 3);
        assert_eq!(out.estimated_quota_remaining, 19_994);
    }

    #[test]
    fn list_list_send_history_composes_kind_and_payload_and_surfaces_rows() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let rows =
            block_on(client.list_list_send_history(vec![0xa1u8; 16], 1)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.list_list_send_history");
        let req: ListListSendHistoryRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.list_id, vec![0xa1u8; 16]);
        assert_eq!(req.limit, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].recipient_count, 3);
        assert_eq!(rows[0].delivered_count, 3);
    }

    #[test]
    fn list_account_lists_composes_kind_and_surfaces_rows() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let rows = block_on(client.list_account_lists()).expect("infallible mock");
        let (kind, _) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.list_account_lists");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pattern, "bob-weekly");
        assert_eq!(rows[0].member_count, 3);
    }

    #[test]
    fn create_account_list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let id = block_on(client.create_account_list(
            "bob-weekly",
            "example.com",
            Some("Bob's Weekly".into()),
            Some("A newsletter".into()),
            Some("https://example.com/help".into()),
            Some("https://example.com/archive".into()),
            Some(2500),
        ))
        .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.create_account_list");
        let req: CreateAccountListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.local_part, "bob-weekly");
        assert_eq!(req.local_domain, "example.com");
        assert_eq!(req.friendly_name.as_deref(), Some("Bob's Weekly"));
        assert_eq!(req.recipients_per_send, Some(2500));
        assert_eq!(id, vec![0xa1u8; 16]);
    }

    #[test]
    fn update_account_list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        block_on(client.update_account_list(
            vec![0xa1u8; 16],
            Some("Renamed".into()),
            None,
            None,
            None,
            None,
        ))
        .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.update_account_list");
        let req: UpdateAccountListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.list_id, vec![0xa1u8; 16]);
        assert_eq!(req.friendly_name.as_deref(), Some("Renamed"));
    }

    #[test]
    fn delete_account_list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        block_on(client.delete_account_list(vec![0xa1u8; 16])).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.delete_account_list");
        let req: DeleteAccountListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.list_id, vec![0xa1u8; 16]);
    }

    #[test]
    fn list_list_members_composes_kind_and_surfaces_counts() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let reply =
            block_on(client.list_list_members(vec![0xa1u8; 16], true)).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.list_list_members");
        let req: ListListMembersRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.list_id, vec![0xa1u8; 16]);
        assert!(
            req.include_unsubscribed,
            "the members page asks for both halves so it can render the summary"
        );
        assert_eq!(reply.members.len(), 2);
        assert_eq!(reply.subscribed_count, 1);
        assert_eq!(reply.unsubscribed_count, 1);
    }

    #[test]
    fn add_list_member_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        block_on(client.add_list_member(vec![0xa1u8; 16], "reader@example.net"))
            .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.add_list_member");
        let req: AddListMemberRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.list_id, vec![0xa1u8; 16]);
        assert_eq!(req.recipient_address, "reader@example.net");
    }

    #[test]
    fn batch_import_list_members_composes_kind_and_surfaces_tally() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let tally = block_on(client.batch_import_list_members(
            vec![0xa1u8; 16],
            vec!["a@example.net".into(), "b@example.net".into()],
        ))
        .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.batch_import_list_members");
        let req: BatchImportListMembersRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.addresses.len(), 2);
        assert_eq!(tally.added, 2);
        assert_eq!(tally.skipped_invalid, 1);
        assert_eq!(tally.skipped_duplicate, 3);
    }

    #[test]
    fn unsubscribe_list_member_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        block_on(client.unsubscribe_list_member(vec![0xa1u8; 16], "reader@example.net"))
            .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.unsubscribe_list_member");
        let req: UnsubscribeListMemberRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.recipient_address, "reader@example.net");
    }

    #[test]
    fn resubscribe_list_member_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        block_on(client.resubscribe_list_member(vec![0xa1u8; 16], "reader@example.net"))
            .expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.resubscribe_list_member");
        let req: ResubscribeListMemberRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.recipient_address, "reader@example.net");
    }

    #[test]
    fn fetch_spam_model_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let blob = block_on(client.fetch_spam_model(vec![0x77u8; 32])).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.fetch_spam_model");
        let req: FetchSpamModelRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.actor_id, vec![0x77u8; 32]);
        // The mock's canned sealed blob is surfaced on the decoded reply
        // (unwrapped on-device before scoring); no baseline is canned.
        assert_eq!(blob.blob.map(|b| b.into_vec()), Some(vec![0x99u8; 8]));
        assert_eq!(blob.baseline, None);
    }

    #[test]
    fn put_spam_model_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        // The client hands the nest an already-re-sealed opaque blob + the advisory
        // sample count; the reply is a bare ack (the default mock arm). A model-only
        // write (e.g. a social train that keeps the server's no-history parity)
        // carries `history_op: None`.
        let outcome = block_on(client.put_spam_model(vec![0xAAu8; 96], 42, None, None))
            .expect("infallible mock");
        assert_eq!(
            outcome,
            PutSpamModelOutcome::Written,
            "a bare ack (a nest from before the outcome field) reads as written"
        );
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.put_spam_model");
        let req: PutSpamModelRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.sealed_model, vec![0xAAu8; 96]);
        assert_eq!(req.sample_count, 42);
        assert_eq!(req.history_op, None);
        assert_eq!(req.holder_copy, None);
    }

    #[test]
    fn put_spam_model_composes_a_holder_copy() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        // While opted into the deployment baseline, the write carries the copy the
        // orchestrator sealed to the aggregation holder (piece (b)).
        let copy = SpamModelHolderCopy {
            holder_pubkey: vec![0x11u8; 32],
            sealed_copy: vec![0x22u8; 64],
            ..Default::default()
        };
        block_on(client.put_spam_model(vec![0xAAu8; 96], 5, None, Some(copy.clone())))
            .expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: PutSpamModelRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(req.holder_copy, Some(copy));
    }

    #[test]
    fn put_spam_model_carries_a_delete_history_op() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        // A client-side undo rides the atomic `Delete` (build-item 3 write side).
        block_on(client.put_spam_model(
            vec![0xBBu8; 96],
            7,
            Some(SpamHistoryOp::Delete {
                history_id: vec![0x01u8; 16],
            }),
            None,
        ))
        .expect("infallible mock");
        let (_, payload) = last_call(&rec);
        let req: PutSpamModelRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        assert_eq!(
            req.history_op,
            Some(SpamHistoryOp::Delete {
                history_id: vec![0x01u8; 16],
            })
        );
    }

    #[test]
    fn get_spam_scoring_policy_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MailAccountClient::new(rec.clone());
        let reply = block_on(client.get_spam_scoring_policy()).expect("infallible mock");
        let (kind, payload) = last_call(&rec);
        assert_eq!(kind, "fauna.bridges.get_spam_scoring_policy");
        // The request carries only the forward-compat catch-all; it decodes.
        let _req: GetSpamScoringPolicyRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes as request");
        // The mock's canned catalog-default policy projects through unchanged.
        assert_eq!(reply.spam_folder_threshold, 5);
        assert_eq!(reply.bayesian_weight_milli, 700);
        assert_eq!(reply.bayesian_min_samples, 50);
        assert_eq!(reply.bayesian_full_confidence_samples, 200);
    }

    #[test]
    fn is_unified_bridges_page_bridge_excludes_nostr_and_bluesky() {
        assert!(!is_unified_bridges_page_bridge("nostr"));
        assert!(!is_unified_bridges_page_bridge("bluesky"));
        assert!(is_unified_bridges_page_bridge("activitypub"));
        assert!(is_unified_bridges_page_bridge("mail"));
    }

    #[test]
    fn is_content_processor_holder_excludes_mta_and_keyless() {
        // Lifted from `fauna-client-pair` with `discover_holders` (2026-07-19):
        // the MTA-exclusion is the load-bearing rule keeping DKIM/TLS seal keys
        // from being mistaken for content-read grant holders (`nests.md:106`).
        assert!(is_content_processor_holder("mda", true));
        assert!(
            !is_content_processor_holder("mta", true),
            "MTA seals DKIM, does not read content"
        );
        assert!(
            !is_content_processor_holder("mda", false),
            "no x25519 ⇒ not a seal target"
        );
    }
}
