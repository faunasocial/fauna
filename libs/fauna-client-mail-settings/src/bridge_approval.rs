//! Shared orchestration for the admin `admin-bridges-pending` page — the
//! pending-bridge approval feed (list / approve / reject) plus the
//! deployment-wide mail-enable toggle.
//!
//! Authority for behavior: `docs/goal/behavior/mail-bridge-lifecycle.md`
//! § Pending approval (the approval cards: `:81`–`:91`) and § Default-off on
//! first claim (the `mail.enabled` toggle: `:97`, `:118`, `:127`). Authority
//! for UX/IDs: `tests/e2e-unified/ui.yaml` `admin-bridges-pending` (the
//! `admin-bridges-pending-card` component IDs ratified `:1890`).
//!
//! Per priority #2, the snapshot projection + action sequencing (hex-decoding
//! the card's pubkey, re-reading the feed after a mutation) live here, not in
//! any per-app shell: the UI renders [`BridgeApprovalSnapshot`] and
//! dispatches [`BridgeApprovalAction`]; the per-app glue implements one
//! WS-RPC seam ([`BridgeApprovalNest`]) over `MailAdminClient`
//! (`libs/fauna-client-bridges`: `list_pending_bridges`,
//! `approve_pending_bridge`, `reject_pending_bridge`, `set_mail_enabled`).
//! Mirrors `local_domains.rs`.
//!
//! The enable toggle folds in here (rather than a standalone machine) because
//! § Default-off interleaves it with approval — toggle ON launches the bridge,
//! whose approval card then appears on this same page — and it has no readable
//! state to back its own snapshot (see the second caveat below).
//!
//! Two scope caveats vs. the goal doc, both **nest-side gaps the wire doesn't
//! yet expose** (code-behind-goal, not contradictions —
//! `mail-bridge-lifecycle.md` § Implementation status today `:29`):
//!   * The ui.yaml card has an `admin-bridges-pending-source-ip` element, but
//!     the landed `ServiceUserInfo` wire carries no source IP, so
//!     [`PendingBridgeView::source_ip`] is `None` until nest adds the column.
//!   * `set_mail_enabled` is write-only — there is no nest read-path for the
//!     toggle state (the Phase-E DB-toggle + `fetch_config` rewire are not yet
//!     landed; today the flag-file presence is the enable-state).
//!     [`BridgeApprovalSnapshot::mail_enabled`] therefore carries only the value
//!     this session last set, not an authoritative read.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_core::localized::LocalizedText;
use fauna_protocol::MaybeSendSync;
use fauna_protocol::wrapped_blob::ServiceUserInfo;
use serde::{Deserialize, Serialize};

use crate::error::{DispatchError, NestError};

/// One pending bridge as the `admin-bridges-pending-card` renders it. Projected
/// from the wire [`ServiceUserInfo`] (public metadata only — no secret material).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PendingBridgeView {
    /// Lowercase hex of the bridge's Ed25519 pubkey — the admin verifies this
    /// against the admin's expected fingerprint
    /// (`admin-bridges-pending-pubkey-hex`). Carried verbatim into the
    /// approve/reject actions (the machine hex-decodes it for the wire).
    pub pubkey_hex: String,
    /// Role the bridge enrolled as (`"mta"` / `"mda"`); drives the per-role
    /// allowlist applied on approve (`admin-bridges-pending-requested-role`).
    pub requested_role: String,
    /// IP the bridge first connected from (`admin-bridges-pending-source-ip`;
    /// advisory only — the anti-impersonation gate is the pubkey). `None` until
    /// nest adds a source-IP column to `ServiceUserInfo` (the wire doesn't carry
    /// it today — see the module scope note).
    pub source_ip: Option<String>,
    /// Epoch-millis of first contact (`admin-bridges-pending-first-seen-at`;
    /// per-app localized in the UI).
    pub first_seen_at: u64,
}

impl From<ServiceUserInfo> for PendingBridgeView {
    fn from(u: ServiceUserInfo) -> Self {
        Self {
            pubkey_hex: hex::encode(&u.ed25519_pubkey),
            requested_role: u.role,
            source_ip: None,
            first_seen_at: u.created_at,
        }
    }
}

/// One approved (running-phase) bridge as the `admin-bridges-approved-card`
/// renders it, below the pending cards. Projected from the wire
/// [`ServiceUserInfo`] rows of `list_service_users(status="approved")`. Carries
/// the rotate-service-user-key affordance (`mail-bridge-lifecycle.md`
/// § Service-user re-keying).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ApprovedBridgeView {
    /// Lowercase hex of the bridge's Ed25519 pubkey
    /// (`admin-bridges-approved-pubkey-hex`) — this **is** the `bridge_actor_id`
    /// the [`BridgeApprovalAction::Rotate`] action hex-decodes for
    /// `revoke_service_user`.
    pub pubkey_hex: String,
    /// Enrolled role (`"mta"` / `"mda"`) — rendered at
    /// `admin-bridges-approved-role`; resolve the friendly name via
    /// [`bridge_display_name`].
    pub role: String,
    /// Epoch-millis of approval (`admin-bridges-approved-approved-at`;
    /// per-app localized). `None` for a pre-`approved_at`-column row.
    pub approved_at: Option<u64>,
}

impl From<ServiceUserInfo> for ApprovedBridgeView {
    fn from(u: ServiceUserInfo) -> Self {
        Self {
            pubkey_hex: hex::encode(&u.ed25519_pubkey),
            role: u.role,
            approved_at: u.approved_at,
        }
    }
}

/// Friendly display name for a bridge's enrolled role, returned as
/// [`LocalizedText`] so each app resolves it through its own i18n runtime
/// (mirrors [`crate::member_status_label`]). The MDA serves IMAP + CalDAV
/// ("Mail & calendar bridge"), the MTA serves SMTP only ("Mail bridge"); an
/// unknown future role falls back to the generic "Bridge". The role string is
/// the short form (`"mta"` / `"mda"`); the `mail.`-prefixed forms are matched
/// defensively. Lifts the identical map linux/windows/android each hard-coded —
/// one source of truth for the `admin-bridges-pending-card` name (priority
/// #1/#2/#3). Canonical role→name table: `docs/goal/architecture/apps/bridges.md`
/// § Active bridges.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn bridge_display_name(role: &str) -> LocalizedText {
    match role {
        "mda" | "mail.mda" => LocalizedText::key("admin.bridges_pending.name_mail_calendar"),
        "mta" | "mail.mta" => LocalizedText::key("admin.bridges_pending.name_mail"),
        // Closes the gap `bridges.md` § User-facing display names named on
        // itself: an approved Bluesky bridge's card is no longer a bare
        // "Bridge".
        "atproto.pds" | "atproto_pds" => LocalizedText::key("admin.bridges_pending.name_bluesky"),
        _ => LocalizedText::key("admin.bridges_pending.name_bridge"),
    }
}

/// Coarse machine status for spinner / disabled-control rendering. Mirrors
/// `LocalDomainStatus` / `DnsStatus`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum BridgeApprovalStatus {
    Idle,
    Loading,
    Working,
}

/// Read-only snapshot the per-app UI renders for `admin-bridges-pending`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BridgeApprovalSnapshot {
    /// Bridges awaiting approval (`status == pending`), each rendering one
    /// `admin-bridges-pending-card`.
    pub pending: Vec<PendingBridgeView>,
    /// Approved (running-phase) bridges, each rendering one
    /// `admin-bridges-approved-card` below the pending cards — the roster the
    /// rotate-service-user-key affordance acts on. Fed by
    /// `list_service_users(status="approved")`.
    pub approved: Vec<ApprovedBridgeView>,
    /// The deployment-wide `mail.enabled` toggle as last set **this session**
    /// (`None` = not yet set/known). Write-only today — there is no nest
    /// read-path (Phase-E gap, see module scope note); the UI's toggle reflects
    /// optimistic local intent until that lands.
    pub mail_enabled: Option<bool>,
    /// The deployment-wide `caldav_enabled` toggle as last set **this session**
    /// (`None` = not yet set/known). Sibling of [`Self::mail_enabled`] — same
    /// optimistic-write-only semantics. Set by the onboarding launch glue today
    /// (`set_caldav_enabled`); a future admin / mail-settings CalDAV toggle
    /// renders it the way the mail toggle renders `mail_enabled`. Per
    /// `caldav-server.md` § Independent enablement.
    pub caldav_enabled: Option<bool>,
    /// The deployment-wide `carddav_enabled` toggle as last set **this session**
    /// (`None` = not yet set/known). The contacts sibling of
    /// [`Self::caldav_enabled`] — same optimistic-write-only semantics. Set by
    /// the onboarding launch glue (`set_carddav_enabled`); the `admin-contacts`
    /// page's live toggle is `CarddavPolicyMachine` (which has the read twin).
    /// Per `carddav-server.md` § Independent enablement.
    pub carddav_enabled: Option<bool>,
    /// The deployment-wide `webdav_enabled` toggle as last set **this session**
    /// (`None` = not yet set/known). The files sibling of
    /// [`Self::carddav_enabled`] — same optimistic-write-only semantics. Set by
    /// the onboarding launch glue (`set_webdav_enabled`); the `admin-files`
    /// page's live toggle is `WebdavPolicyMachine` (which has the read twin).
    /// Per `webdav-server.md` § Independent enablement.
    pub webdav_enabled: Option<bool>,
    pub status: BridgeApprovalStatus,
    /// Last action's error, surfaced via the `error-message` element.
    pub error: Option<String>,
    /// The act an open confirm dialog is asking about (`None` = no dialog):
    /// the renderer paints `admin-bridges-reject-confirm` or
    /// `admin-bridges-rotate-confirm` for it, and
    /// [`BridgeApprovalAction::Confirm`] runs it. Set by
    /// [`BridgeApprovalMachine::arm`]; every other action and every re-read
    /// disarms, so a confirm never lands on a card the feed has since moved.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub armed: Option<BridgeConfirm>,
}

/// A bridge act the page confirms in a dialog before it reaches the nest —
/// one shape for both of the page's confirms (`mail-bridge-lifecycle.md`
/// § Pending approval → rejection flow; § Service-user re-keying). The arm
/// lives in [`BridgeApprovalMachine`], not per app.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum BridgeConfirm {
    /// `admin-bridges-pending-reject-button` → `admin-bridges-reject-confirm`
    /// → `reject_pending_bridge`. `pubkey_hex` is the pending card's.
    Reject { pubkey_hex: String },
    /// `admin-bridges-approved-rotate-button` → `admin-bridges-rotate-confirm`
    /// → `revoke_service_user`. `pubkey_hex` is the approved card's.
    Rotate { pubkey_hex: String },
}

impl BridgeApprovalSnapshot {
    fn empty() -> Self {
        Self {
            pending: Vec::new(),
            approved: Vec::new(),
            mail_enabled: None,
            caldav_enabled: None,
            carddav_enabled: None,
            webdav_enabled: None,
            status: BridgeApprovalStatus::Idle,
            error: None,
            armed: None,
        }
    }
}

/// Actions the per-app UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum BridgeApprovalAction {
    /// Re-read the pending-bridge feed (page load / pull-to-refresh / after a
    /// `BridgePendingApproval` push event).
    Refresh,
    /// Approve a pending bridge (`pending → approved`), recording the calling
    /// admin as approver. `role` confirms the enrolled role (`"mta"` / `"mda"`);
    /// the nest validates it against the pending row. Idempotent on an already-
    /// approved bridge; a revoked bridge is refused (must re-enroll).
    Approve { pubkey_hex: String, role: String },
    /// Reject a pending bridge (`→ revoked`; the row is kept so a re-connect
    /// from the same pubkey rejects immediately rather than re-pending).
    /// Idempotent, with NO confirm: a renderer arms
    /// [`BridgeConfirm::Reject`] and dispatches [`Self::Confirm`] instead. This
    /// direct form stays only for the apps that still hold the rejection
    /// dialog in their own renderer, and goes when they move to the arm.
    Reject { pubkey_hex: String },
    /// Rotate an **approved** bridge's service-user key (`mail-bridge-lifecycle.md`
    /// § Service-user re-keying, step 3). Dispatches `revoke_service_user(pubkey)`:
    /// the running bridge detects the revoke and exits, the supervisor restarts
    /// it, and on a mail-enabled box the fresh key auto-approves — with NO
    /// confirm: a renderer arms [`BridgeConfirm::Rotate`] (the
    /// `admin-bridges-rotate-confirm` dialog; no DKIM warning for any role: the
    /// nest holds every DKIM key) and dispatches [`Self::Confirm`]. This direct
    /// form stays only for the apps that still hold the dialog in their own
    /// renderer. `pubkey_hex` is the approved card's `admin-bridges-approved-pubkey-hex`.
    Rotate { pubkey_hex: String },
    /// The open dialog's confirm button: disarm and run the armed act
    /// ([`BridgeApprovalSnapshot::armed`]). Nothing armed is an error — the
    /// dialog was already dismissed — and reaches no nest.
    Confirm,
    /// Flip the deployment-wide `mail.enabled` toggle (§ Default-off on first
    /// claim). `true` materializes the flag file + brings the bridge services
    /// up; `false` brings them down.
    SetMailEnabled { enabled: bool },
    /// Flip the deployment-wide `caldav_enabled` toggle, independently of mail
    /// (`caldav-server.md` § Independent enablement). `true` materializes the
    /// `/data/caldav-enabled` flag + binds the MDA's `:443` CalDAV listener;
    /// `false` unbinds it. Sibling of [`Self::SetMailEnabled`].
    SetCalDavEnabled { enabled: bool },
    /// Flip the deployment-wide `carddav_enabled` toggle, independently of mail
    /// and CalDAV (`carddav-server.md` § Independent enablement). CardDAV rides
    /// the shared DAV listener, so `true` materializes the
    /// `/data/carddav-enabled` flag + adds the `/carddav` path handler; `false`
    /// removes it. Sibling of [`Self::SetCalDavEnabled`].
    SetCardDavEnabled { enabled: bool },
    /// Flip the deployment-wide `webdav_enabled` toggle, independently of mail,
    /// CalDAV, and CardDAV (`webdav-server.md` § Independent enablement). WebDAV
    /// rides the shared DAV listener, so `true` materializes the
    /// `/data/webdav-enabled` flag + adds the `/webdav` path handler; `false`
    /// removes it. Sibling of [`Self::SetCardDavEnabled`].
    SetWebDavEnabled { enabled: bool },
}

/// WS-RPC seam to nest. Per-app glue implements this over `MailAdminClient`
/// (`libs/fauna-client-bridges`) — each method is a 1:1 forward; the pubkey is
/// already the 32-byte wire form (the machine hex-decodes the card's string).
// Dual `async_trait` arm + `MaybeSendSync` supertrait so the one seam serves
// native + wasm (see `fauna_protocol::MaybeSendSync`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait BridgeApprovalNest: MaybeSendSync {
    /// `fauna.bridges.list_pending_bridges` — every bridge with `status ==
    /// pending`, public metadata only.
    async fn list_pending_bridges(&self) -> Result<Vec<ServiceUserInfo>, NestError>;
    /// `fauna.bridges.list_service_users` — the full bridge roster, filtered by
    /// `role` / `status` (the approved roster passes `status="approved"`).
    async fn list_service_users(
        &self,
        role: Option<String>,
        status: Option<String>,
    ) -> Result<Vec<ServiceUserInfo>, NestError>;
    /// `fauna.bridges.approve_pending_bridge`.
    async fn approve_pending_bridge(
        &self,
        ed25519_pubkey: Vec<u8>,
        role: String,
    ) -> Result<(), NestError>;
    /// `fauna.bridges.reject_pending_bridge`.
    async fn reject_pending_bridge(&self, ed25519_pubkey: Vec<u8>) -> Result<(), NestError>;
    /// `fauna.bridges.revoke_service_user` — revoke an already-approved bridge so
    /// its service-user key can be rotated (`mail-bridge-lifecycle.md`
    /// § Service-user re-keying). The running-phase counterpart to
    /// `reject_pending_bridge`.
    async fn revoke_service_user(&self, ed25519_pubkey: Vec<u8>) -> Result<(), NestError>;
    /// `fauna.bridges.set_mail_enabled`.
    async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError>;
    /// `fauna.bridges.set_caldav_enabled`.
    async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), NestError>;
    /// `fauna.bridges.set_carddav_enabled`.
    async fn set_carddav_enabled(&self, enabled: bool) -> Result<(), NestError>;
    /// `fauna.bridges.set_webdav_enabled`.
    async fn set_webdav_enabled(&self, enabled: bool) -> Result<(), NestError>;
}

/// Decode the card's hex pubkey to the 32-byte wire form. The hex comes from a
/// rendered [`PendingBridgeView`], but a corrupted snapshot shouldn't crash the
/// dispatch — a malformed / wrong-length string surfaces as a user-visible
/// error instead of panicking or sending garbage to nest.
fn decode_pubkey(pubkey_hex: &str) -> Result<Vec<u8>, DispatchError> {
    fauna_core::hex32::decode(pubkey_hex)
        .map(|b| b.to_vec())
        .map_err(|e| match e {
            fauna_core::hex32::Hex32Error::NotHex(inner) => {
                DispatchError::Wrap(format!("bridge pubkey hex: {inner}"))
            }
            fauna_core::hex32::Hex32Error::WrongLength(n) => {
                DispatchError::InvalidState(format!("bridge pubkey must be 32 bytes, got {n}"))
            }
        })
}

/// One instance per admin client. Holds the rendered snapshot; drives the seam.
/// Mirrors `LocalDomainMachine` (snapshot + dispatch).
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct BridgeApprovalMachine {
    nest: Arc<dyn BridgeApprovalNest>,
    inner: Mutex<BridgeApprovalSnapshot>,
}

// `new` takes `Arc<dyn BridgeApprovalNest>` (not an FFI type), so it stays in a
// plain (non-exported) impl alongside the private helpers. The FFI surface —
// `snapshot` (sync) + `hydrate`/`dispatch` (async) — lives in the exported impl
// blocks below (mirrors the onboarding-machine export/private-helper split).
impl BridgeApprovalMachine {
    pub fn new(nest: Arc<dyn BridgeApprovalNest>) -> Self {
        Self {
            nest,
            inner: Mutex::new(BridgeApprovalSnapshot::empty()),
        }
    }

    fn set_status(&self, status: BridgeApprovalStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(BridgeApprovalStatus::Loading);
        let pending = self.nest.list_pending_bridges().await?;
        // The approved roster (running-phase bridges) — the rotate affordance's
        // source. Nest filters by status, mirroring the pending feed's server-side
        // filter (§ nest list_service_users_handler honors `status`).
        let approved = self
            .nest
            .list_service_users(None, Some("approved".into()))
            .await?;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.pending = pending.into_iter().map(PendingBridgeView::from).collect();
        snap.approved = approved.into_iter().map(ApprovedBridgeView::from).collect();
        snap.status = BridgeApprovalStatus::Idle;
        Ok(())
    }

    async fn approve(&self, pubkey_hex: String, role: String) -> Result<(), DispatchError> {
        self.set_status(BridgeApprovalStatus::Working);
        let pubkey = decode_pubkey(&pubkey_hex)?;
        self.nest.approve_pending_bridge(pubkey, role).await?;
        // Re-read so the approved bridge drops out of the pending feed.
        self.refresh().await
    }

    async fn reject(&self, pubkey_hex: String) -> Result<(), DispatchError> {
        self.set_status(BridgeApprovalStatus::Working);
        let pubkey = decode_pubkey(&pubkey_hex)?;
        self.nest.reject_pending_bridge(pubkey).await?;
        // Re-read so the revoked bridge drops out of the pending feed.
        self.refresh().await
    }

    async fn rotate(&self, pubkey_hex: String) -> Result<(), DispatchError> {
        self.set_status(BridgeApprovalStatus::Working);
        let pubkey = decode_pubkey(&pubkey_hex)?;
        self.nest.revoke_service_user(pubkey).await?;
        // Re-read: the rotated bridge is now `revoked` (drops from the approved
        // roster). On a mail-enabled box it re-enrolls + auto-approves with a
        // fresh key and reappears on a later refresh; otherwise it surfaces as a
        // fresh pending card for the admin to approve.
        self.refresh().await
    }

    async fn set_mail_enabled(&self, enabled: bool) -> Result<(), DispatchError> {
        self.set_status(BridgeApprovalStatus::Working);
        self.nest.set_mail_enabled(enabled).await?;
        // No refresh: the toggle has no nest read-path (Phase-E gap). Record the
        // optimistic intent so the UI's toggle reflects the admin's click.
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.mail_enabled = Some(enabled);
        snap.status = BridgeApprovalStatus::Idle;
        Ok(())
    }

    async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), DispatchError> {
        self.set_status(BridgeApprovalStatus::Working);
        self.nest.set_caldav_enabled(enabled).await?;
        // Sibling of set_mail_enabled: no nest read-path, record optimistic intent.
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.caldav_enabled = Some(enabled);
        snap.status = BridgeApprovalStatus::Idle;
        Ok(())
    }

    async fn set_carddav_enabled(&self, enabled: bool) -> Result<(), DispatchError> {
        self.set_status(BridgeApprovalStatus::Working);
        self.nest.set_carddav_enabled(enabled).await?;
        // Sibling of set_caldav_enabled: no nest read-path, record optimistic intent.
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.carddav_enabled = Some(enabled);
        snap.status = BridgeApprovalStatus::Idle;
        Ok(())
    }

    async fn set_webdav_enabled(&self, enabled: bool) -> Result<(), DispatchError> {
        self.set_status(BridgeApprovalStatus::Working);
        self.nest.set_webdav_enabled(enabled).await?;
        // Sibling of set_carddav_enabled: no nest read-path, record optimistic intent.
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.webdav_enabled = Some(enabled);
        snap.status = BridgeApprovalStatus::Idle;
        Ok(())
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl BridgeApprovalMachine {
    pub fn snapshot(&self) -> BridgeApprovalSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }

    /// Open the confirm dialog for `confirm` (replacing any open one). Arming
    /// reaches no nest, so it is a sync call: a renderer paints the dialog in
    /// the same frame as the click that opened it — the inline-reveal
    /// contract the e2e reads synchronously (`admin.md` § Approved-bridges
    /// roster).
    pub fn arm(&self, confirm: BridgeConfirm) {
        self.inner.lock().expect("snapshot mutex").armed = Some(confirm);
    }

    /// The open dialog's cancel button: close it, no write.
    pub fn disarm(&self) {
        self.inner.lock().expect("snapshot mutex").armed = None;
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl BridgeApprovalMachine {
    /// Initial page load. Routes through `dispatch(Refresh)` (not a bare
    /// `refresh()`) so a rejected list — e.g. a non-admin's Admin-gated
    /// `list_pending_bridges` fetch — is recorded into `snapshot.error`, exactly
    /// like every other action. That is the `error-message` contract the page
    /// renders (`test_admin_error_surfacing`); a bare `refresh()` returned the
    /// `Err` but left `snapshot.error` empty, so the page had nothing to surface.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.dispatch(BridgeApprovalAction::Refresh).await
    }

    pub async fn dispatch(&self, action: BridgeApprovalAction) -> Result<(), DispatchError> {
        // Clear any prior error before the new action runs; and disarm — a
        // `Confirm` takes the arm with it, any other action closes the dialog.
        let armed = {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.error = None;
            snap.armed.take()
        };
        crate::dispatch_capturing_error!(
            self,
            BridgeApprovalStatus,
            match action {
                BridgeApprovalAction::Refresh => self.refresh().await,
                BridgeApprovalAction::Approve { pubkey_hex, role } => {
                    self.approve(pubkey_hex, role).await
                }
                BridgeApprovalAction::Reject { pubkey_hex } => self.reject(pubkey_hex).await,
                BridgeApprovalAction::Rotate { pubkey_hex } => self.rotate(pubkey_hex).await,
                BridgeApprovalAction::Confirm => match armed {
                    Some(BridgeConfirm::Reject { pubkey_hex }) => self.reject(pubkey_hex).await,
                    Some(BridgeConfirm::Rotate { pubkey_hex }) => self.rotate(pubkey_hex).await,
                    None => Err(DispatchError::InvalidState(
                        "no bridge confirm is open".into(),
                    )),
                },
                BridgeApprovalAction::SetMailEnabled { enabled } => {
                    self.set_mail_enabled(enabled).await
                }
                BridgeApprovalAction::SetCalDavEnabled { enabled } => {
                    self.set_caldav_enabled(enabled).await
                }
                BridgeApprovalAction::SetCardDavEnabled { enabled } => {
                    self.set_carddav_enabled(enabled).await
                }
                BridgeApprovalAction::SetWebDavEnabled { enabled } => {
                    self.set_webdav_enabled(enabled).await
                }
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn bridge_display_name_maps_role_to_i18n_key() {
        // MDA serves IMAP + CalDAV; MTA serves SMTP only; short + mail.-prefixed.
        assert_eq!(
            bridge_display_name("mda").key,
            "admin.bridges_pending.name_mail_calendar"
        );
        assert_eq!(
            bridge_display_name("mail.mda").key,
            "admin.bridges_pending.name_mail_calendar"
        );
        assert_eq!(
            bridge_display_name("mta").key,
            "admin.bridges_pending.name_mail"
        );
        assert_eq!(
            bridge_display_name("mail.mta").key,
            "admin.bridges_pending.name_mail"
        );
        assert_eq!(
            bridge_display_name("atproto.pds").key,
            "admin.bridges_pending.name_bluesky"
        );
        // Unknown future role → generic fallback.
        assert_eq!(
            bridge_display_name("future").key,
            "admin.bridges_pending.name_bridge"
        );
    }

    /// In-memory nest modelling the enrollment-row lifecycle: `list` projects
    /// the `pending` rows, `approve` flips `pending → approved` (validating role,
    /// refusing revoked/unknown), `reject` flips `→ revoked`, `set_mail_enabled`
    /// records the toggle.
    #[derive(Default)]
    struct FakeNest {
        bridges: StdMutex<Vec<ServiceUserInfo>>,
        mail_enabled: StdMutex<Option<bool>>,
        caldav_enabled: StdMutex<Option<bool>>,
        carddav_enabled: StdMutex<Option<bool>>,
        webdav_enabled: StdMutex<Option<bool>>,
        /// When set, `list_pending_bridges` rejects — models the nest's
        /// `require_admin` rejection a non-admin hits (`test_admin_error_surfacing`).
        reject_list: StdMutex<bool>,
    }

    fn bridge(pubkey: u8, role: &str, status: &str) -> ServiceUserInfo {
        ServiceUserInfo {
            bridge_id: format!("bridge-{pubkey}"),
            role: role.into(),
            status: status.into(),
            ed25519_pubkey: vec![pubkey; 32],
            has_x25519: false,
            created_at: 1_700_000_000_000,
            approved_at: None,
            ..Default::default()
        }
    }

    /// An `approved` bridge carrying an `approved_at` timestamp — the shape the
    /// approved roster projects.
    fn approved_bridge(pubkey: u8, role: &str) -> ServiceUserInfo {
        ServiceUserInfo {
            approved_at: Some(1_700_000_100_000),
            ..bridge(pubkey, role, "approved")
        }
    }

    #[async_trait]
    impl BridgeApprovalNest for FakeNest {
        async fn list_pending_bridges(&self) -> Result<Vec<ServiceUserInfo>, NestError> {
            if *self.reject_list.lock().unwrap() {
                return Err(NestError::Rejected("require_admin".into()));
            }
            Ok(self
                .bridges
                .lock()
                .unwrap()
                .iter()
                .filter(|b| b.status == "pending")
                .cloned()
                .collect())
        }

        async fn list_service_users(
            &self,
            role: Option<String>,
            status: Option<String>,
        ) -> Result<Vec<ServiceUserInfo>, NestError> {
            // Models the nest handler's server-side role+status filter.
            Ok(self
                .bridges
                .lock()
                .unwrap()
                .iter()
                .filter(|b| match &role {
                    Some(r) => &b.role == r,
                    None => true,
                })
                .filter(|b| match &status {
                    Some(s) => &b.status == s,
                    None => true,
                })
                .cloned()
                .collect())
        }

        async fn approve_pending_bridge(
            &self,
            ed25519_pubkey: Vec<u8>,
            role: String,
        ) -> Result<(), NestError> {
            let mut bridges = self.bridges.lock().unwrap();
            let b = bridges
                .iter_mut()
                .find(|b| b.ed25519_pubkey == ed25519_pubkey)
                .ok_or_else(|| NestError::Rejected("fauna.bridges.not_found".into()))?;
            if b.status == "revoked" {
                // Mirrors nest: a revoked bridge must re-enroll.
                return Err(NestError::Rejected("fauna.protocol.malformed".into()));
            }
            if b.role != role {
                return Err(NestError::Rejected("role_mismatch".into()));
            }
            b.status = "approved".into();
            b.approved_at = Some(1_700_000_100_000);
            Ok(())
        }

        async fn reject_pending_bridge(&self, ed25519_pubkey: Vec<u8>) -> Result<(), NestError> {
            let mut bridges = self.bridges.lock().unwrap();
            let b = bridges
                .iter_mut()
                .find(|b| b.ed25519_pubkey == ed25519_pubkey)
                .ok_or_else(|| NestError::Rejected("fauna.bridges.not_found".into()))?;
            b.status = "revoked".into();
            Ok(())
        }

        async fn revoke_service_user(&self, ed25519_pubkey: Vec<u8>) -> Result<(), NestError> {
            // Running-phase counterpart of reject: an approved bridge → revoked.
            let mut bridges = self.bridges.lock().unwrap();
            let b = bridges
                .iter_mut()
                .find(|b| b.ed25519_pubkey == ed25519_pubkey)
                .ok_or_else(|| NestError::Rejected("fauna.bridges.not_found".into()))?;
            b.status = "revoked".into();
            Ok(())
        }

        async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError> {
            *self.mail_enabled.lock().unwrap() = Some(enabled);
            Ok(())
        }

        async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            *self.caldav_enabled.lock().unwrap() = Some(enabled);
            Ok(())
        }

        async fn set_carddav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            *self.carddav_enabled.lock().unwrap() = Some(enabled);
            Ok(())
        }

        async fn set_webdav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            *self.webdav_enabled.lock().unwrap() = Some(enabled);
            Ok(())
        }
    }

    fn machine_with(bridges: Vec<ServiceUserInfo>) -> BridgeApprovalMachine {
        BridgeApprovalMachine::new(Arc::new(FakeNest {
            bridges: StdMutex::new(bridges),
            ..Default::default()
        }))
    }

    #[tokio::test]
    async fn refresh_projects_pending_only() {
        let m = machine_with(vec![
            bridge(0xAB, "mta", "pending"),
            bridge(0xCD, "mda", "approved"),
        ]);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.pending.len(), 1, "approved bridge excluded");
        let card = &snap.pending[0];
        assert_eq!(card.pubkey_hex, hex::encode([0xABu8; 32]));
        assert_eq!(card.requested_role, "mta");
        assert_eq!(card.first_seen_at, 1_700_000_000_000);
        // Source IP is a nest-side wire gap — projected as None today.
        assert_eq!(card.source_ip, None);
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn approve_flips_to_approved_and_drops_from_feed() {
        let m = machine_with(vec![bridge(0x11, "mta", "pending")]);
        m.hydrate().await.unwrap();
        let pubkey_hex = m.snapshot().pending[0].pubkey_hex.clone();

        m.dispatch(BridgeApprovalAction::Approve {
            pubkey_hex,
            role: "mta".into(),
        })
        .await
        .unwrap();

        let snap = m.snapshot();
        assert!(snap.pending.is_empty(), "approved bridge left the feed");
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn reject_revokes_and_drops_from_feed() {
        let m = machine_with(vec![bridge(0x22, "mda", "pending")]);
        m.hydrate().await.unwrap();
        let pubkey_hex = m.snapshot().pending[0].pubkey_hex.clone();

        m.dispatch(BridgeApprovalAction::Reject { pubkey_hex })
            .await
            .unwrap();

        assert!(
            m.snapshot().pending.is_empty(),
            "revoked bridge left the feed"
        );
    }

    /// Both confirms are one shape held by the machine: arming opens the
    /// dialog and reaches no nest; `Confirm` disarms and runs the armed act.
    #[tokio::test]
    async fn an_armed_confirm_runs_its_act_only_on_confirm() {
        let m = machine_with(vec![
            bridge(0x22, "mda", "pending"),
            approved_bridge(0x33, "mta"),
        ]);
        m.hydrate().await.unwrap();
        let pending_hex = m.snapshot().pending[0].pubkey_hex.clone();
        let approved_hex = m.snapshot().approved[0].pubkey_hex.clone();

        let reject = BridgeConfirm::Reject {
            pubkey_hex: pending_hex,
        };
        m.arm(reject.clone());
        assert_eq!(m.snapshot().armed, Some(reject), "arming opens the dialog");
        m.dispatch(BridgeApprovalAction::Refresh).await.unwrap();
        assert_eq!(m.snapshot().pending.len(), 1, "arming rejected nothing");

        m.arm(BridgeConfirm::Reject {
            pubkey_hex: m.snapshot().pending[0].pubkey_hex.clone(),
        });
        m.dispatch(BridgeApprovalAction::Confirm).await.unwrap();
        let snap = m.snapshot();
        assert!(snap.pending.is_empty(), "the confirm rejected the bridge");
        assert_eq!(snap.armed, None, "the confirm closes the dialog");

        m.arm(BridgeConfirm::Rotate {
            pubkey_hex: approved_hex,
        });
        assert_eq!(m.snapshot().approved.len(), 1, "arming rotated nothing");
        m.dispatch(BridgeApprovalAction::Confirm).await.unwrap();
        let snap = m.snapshot();
        assert!(snap.approved.is_empty(), "the confirm rotated the key");
        assert_eq!(snap.armed, None);
    }

    /// Cancel closes the dialog with no write; any other action or re-read
    /// closes it too, so a confirm never lands on a stale arm — and a confirm
    /// with nothing armed surfaces an error instead of acting.
    #[tokio::test]
    async fn cancel_or_any_other_action_disarms_and_a_bare_confirm_errors() {
        let m = machine_with(vec![bridge(0x22, "mda", "pending")]);
        m.hydrate().await.unwrap();
        let hex = m.snapshot().pending[0].pubkey_hex.clone();
        let reject = BridgeConfirm::Reject { pubkey_hex: hex };

        m.arm(reject.clone());
        m.disarm();
        assert_eq!(m.snapshot().armed, None, "cancel closes the dialog");

        m.arm(reject.clone());
        m.dispatch(BridgeApprovalAction::SetMailEnabled { enabled: true })
            .await
            .unwrap();
        assert_eq!(m.snapshot().armed, None, "another action disarms");

        m.arm(reject);
        m.dispatch(BridgeApprovalAction::Refresh).await.unwrap();
        assert_eq!(m.snapshot().armed, None, "a re-read disarms");

        assert!(m.dispatch(BridgeApprovalAction::Confirm).await.is_err());
        let snap = m.snapshot();
        assert!(snap.error.is_some(), "a bare confirm surfaces an error");
        assert_eq!(snap.pending.len(), 1, "a stale confirm must never reject");
    }

    #[tokio::test]
    async fn refresh_projects_approved_roster() {
        // Pending + approved + revoked all present; the refresh must project the
        // pending feed AND the approved roster, each excluding the other statuses.
        let m = machine_with(vec![
            bridge(0xAA, "mta", "pending"),
            approved_bridge(0xBB, "mta"),
            approved_bridge(0xCC, "mda"),
            bridge(0xDD, "mta", "revoked"),
        ]);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();

        assert_eq!(snap.pending.len(), 1, "only the pending bridge");
        assert_eq!(snap.pending[0].pubkey_hex, hex::encode([0xAAu8; 32]));

        assert_eq!(snap.approved.len(), 2, "both approved, not revoked/pending");
        let mta = &snap.approved[0];
        assert_eq!(mta.pubkey_hex, hex::encode([0xBBu8; 32]));
        assert_eq!(mta.role, "mta");
        assert_eq!(mta.approved_at, Some(1_700_000_100_000));
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn rotate_revokes_approved_bridge_and_drops_from_roster() {
        let m = machine_with(vec![approved_bridge(0x77, "mta")]);
        m.hydrate().await.unwrap();
        let pubkey_hex = m.snapshot().approved[0].pubkey_hex.clone();

        m.dispatch(BridgeApprovalAction::Rotate { pubkey_hex })
            .await
            .unwrap();

        // The bridge is now `revoked`, so it leaves the approved roster (and the
        // pending feed — a re-enroll would resurface it fresh on a mail-enabled box).
        let snap = m.snapshot();
        assert!(snap.approved.is_empty(), "rotated bridge left the roster");
        assert!(snap.pending.is_empty());
        assert!(snap.error.is_none());
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
    }

    #[tokio::test]
    async fn rotate_unknown_pubkey_surfaces_nest_error() {
        let m = machine_with(vec![approved_bridge(0x88, "mda")]);
        let unknown = hex::encode([0x99u8; 32]);
        let err = m
            .dispatch(BridgeApprovalAction::Rotate {
                pubkey_hex: unknown,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)));
        let snap = m.snapshot();
        assert!(
            snap.error.as_deref().unwrap().contains("not_found"),
            "error: {:?}",
            snap.error
        );
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
    }

    #[tokio::test]
    async fn hydrate_records_snapshot_error_on_rejected_list() {
        // A non-admin's Admin-gated `list_pending_bridges` is rejected. `hydrate`
        // must record that rejection into `snapshot.error` (the `error-message`
        // contract — `test_admin_error_surfacing`), not just return the `Err`.
        let m = BridgeApprovalMachine::new(Arc::new(FakeNest {
            reject_list: StdMutex::new(true),
            ..Default::default()
        }));
        let err = m.hydrate().await.unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)));
        let snap = m.snapshot();
        assert!(
            snap.error
                .as_deref()
                .unwrap_or("")
                .contains("require_admin"),
            "hydrate must record the rejected list into snapshot.error; got {:?}",
            snap.error
        );
        // Status returns to Idle (not stuck on Loading) so the page isn't wedged busy.
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
    }

    #[tokio::test]
    async fn approve_unknown_pubkey_surfaces_nest_error() {
        let m = machine_with(vec![bridge(0x33, "mta", "pending")]);
        let unknown = hex::encode([0x99u8; 32]);
        let err = m
            .dispatch(BridgeApprovalAction::Approve {
                pubkey_hex: unknown,
                role: "mta".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)));
        let snap = m.snapshot();
        assert!(
            snap.error.as_deref().unwrap().contains("not_found"),
            "error: {:?}",
            snap.error
        );
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
    }

    #[tokio::test]
    async fn approve_role_mismatch_surfaces_nest_error() {
        let m = machine_with(vec![bridge(0x44, "mta", "pending")]);
        m.hydrate().await.unwrap();
        let pubkey_hex = m.snapshot().pending[0].pubkey_hex.clone();
        let err = m
            .dispatch(BridgeApprovalAction::Approve {
                pubkey_hex,
                role: "mda".into(), // wrong role
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)));
        // The pending bridge is untouched — still in the feed (no refresh on the
        // error path, but the row was never approved regardless).
        assert!(m.snapshot().error.is_some());
    }

    #[tokio::test]
    async fn approve_malformed_hex_surfaces_wrap_error_without_calling_nest() {
        let m = machine_with(vec![bridge(0x55, "mta", "pending")]);
        let err = m
            .dispatch(BridgeApprovalAction::Approve {
                pubkey_hex: "zz-not-hex".into(),
                role: "mta".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Wrap(_)), "got {err:?}");
        assert!(m.snapshot().error.is_some());
    }

    #[tokio::test]
    async fn approve_wrong_length_hex_surfaces_invalid_state() {
        let m = machine_with(vec![]);
        // Valid hex, but only 16 bytes — not a 32-byte Ed25519 pubkey.
        let err = m
            .dispatch(BridgeApprovalAction::Approve {
                pubkey_hex: hex::encode([0x66u8; 16]),
                role: "mta".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn set_mail_enabled_records_optimistic_toggle() {
        let m = machine_with(vec![]);
        assert_eq!(m.snapshot().mail_enabled, None, "unknown until set");

        m.dispatch(BridgeApprovalAction::SetMailEnabled { enabled: true })
            .await
            .unwrap();
        assert_eq!(m.snapshot().mail_enabled, Some(true));

        m.dispatch(BridgeApprovalAction::SetMailEnabled { enabled: false })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.mail_enabled, Some(false));
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn set_caldav_enabled_records_optimistic_toggle() {
        let m = machine_with(vec![]);
        assert_eq!(m.snapshot().caldav_enabled, None, "unknown until set");

        m.dispatch(BridgeApprovalAction::SetCalDavEnabled { enabled: true })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.caldav_enabled, Some(true));
        // Independent of mail: the mail toggle is untouched.
        assert_eq!(snap.mail_enabled, None, "caldav toggle leaves mail unset");

        m.dispatch(BridgeApprovalAction::SetCalDavEnabled { enabled: false })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.caldav_enabled, Some(false));
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn set_carddav_enabled_records_optimistic_toggle() {
        let m = machine_with(vec![]);
        assert_eq!(m.snapshot().carddav_enabled, None, "unknown until set");

        m.dispatch(BridgeApprovalAction::SetCardDavEnabled { enabled: true })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.carddav_enabled, Some(true));
        // Independent of mail and CalDAV: both sibling toggles untouched.
        assert_eq!(snap.mail_enabled, None, "carddav toggle leaves mail unset");
        assert_eq!(
            snap.caldav_enabled, None,
            "carddav toggle leaves caldav unset"
        );

        m.dispatch(BridgeApprovalAction::SetCardDavEnabled { enabled: false })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.carddav_enabled, Some(false));
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn set_webdav_enabled_records_optimistic_toggle() {
        let m = machine_with(vec![]);
        assert_eq!(m.snapshot().webdav_enabled, None, "unknown until set");

        m.dispatch(BridgeApprovalAction::SetWebDavEnabled { enabled: true })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.webdav_enabled, Some(true));
        // Independent of mail, CalDAV, and CardDAV: all sibling toggles untouched.
        assert_eq!(snap.mail_enabled, None, "webdav toggle leaves mail unset");
        assert_eq!(
            snap.caldav_enabled, None,
            "webdav toggle leaves caldav unset"
        );
        assert_eq!(
            snap.carddav_enabled, None,
            "webdav toggle leaves carddav unset"
        );

        m.dispatch(BridgeApprovalAction::SetWebDavEnabled { enabled: false })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.webdav_enabled, Some(false));
        assert_eq!(snap.status, BridgeApprovalStatus::Idle);
        assert!(snap.error.is_none());
    }
}
