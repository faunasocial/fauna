//! Shared orchestration for the domain-management section of the admin
//! `admin-dns` page (list / add / remove / restore / update-config of
//! mail-hosting domains).
//!
//! Authority for behavior: `docs/goal/behavior/mail-multidomain.md`
//! (§ The `mail_domains` model, § Adding / Removing a new local domain).
//! Authority for UX/IDs: `tests/e2e-unified/ui.yaml` `admin-dns` (the
//! `admin-dns-domain` component + the `admin-dns-add-domain-*` form). Domain
//! management was folded onto the unified DNS surface per
//! `docs/goal/behavior/dns-management.md` (one domain-management surface); there
//! is no separate `admin-mail-domains` page.
//!
//! Scope (per `mail-multidomain.md` § Implementation status today `:14`): this
//! wraps the **landed** CRUD surface — the `{add,remove,restore,list,update}_
//! local_domain` WS-RPC kinds, which insert / soft-delete / restore / edit the
//! `mail_domains` row. DKIM provisioning (keygen + seal) is fully **nest-side**
//! (`mail-bridge-lifecycle.md` § DKIM provisioning — the nest mints the key
//! when the domain is added); DNS-record publish + cert provisioning (§ Adding a new local
//! domain steps 7.3–7.4) are separate deferred tracks; this orchestration is
//! the row-management half the linux `admin-dns` page renders today.
//!
//! Per priority #2, the snapshot projection + action sequencing live here, not
//! in any per-app shell: the UI renders [`LocalDomainsSnapshot`] and
//! dispatches [`LocalDomainAction`]; the per-app glue implements one WS-RPC
//! seam ([`LocalDomainNest`]) over `MailAdminClient`. Mirrors `bridge_approval.rs`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_protocol::MaybeSendSync;
use fauna_protocol::bridge_routing::{DmarcMode, MailDomainRenameRow, MailDomainRow};
use serde::{Deserialize, Serialize};

use crate::error::{DispatchError, NestError};
use crate::primary_domain_rename::{PrimaryDomainRenameView, rename_available};

/// MTA-STS cert-mode default: `expand_primary` (the shared-DNS-provider case) —
/// `mail-multidomain.md` § Adding a new local domain step 3 `:334`.
pub const DEFAULT_CERT_MODE: &str = "expand_primary";

/// One overridable role address whose per-domain delivery target the admin may
/// redirect — the four RFC 2142 / RFC 5321 §4.5.1 operations roles
/// (`mail-multidomain.md` § Per-domain role-address routing). `tlsrpt` /
/// `dmarc-report` are intentionally absent: they always route to the
/// deployment-wide report processor and are never admin-overridable.
///
/// The local UniFFI/wasm mirror of the wire
/// `fauna_protocol::bridge_routing::RoleAddressKind` (fauna-protocol carries no
/// UniFFI scaffolding, so it can't appear in an exported `LocalDomainAction`);
/// the `From` impl below converts between the two — the same projection pattern
/// [`LocalDomainView`] uses for [`MailDomainRow`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RoleAddressKind {
    Postmaster,
    Abuse,
    Noc,
    Security,
}

/// Convert the UniFFI/wasm-facing role enum to the wire enum. Lives here
/// (not behind the `rpc-glue` feature that gates its main consumer,
/// `rpc_glue::set_role_address`'s `.into()` calls) because `as_storage_key`
/// below needs it unconditionally — the `rpc-glue`-less `mail-bridge-ffi`
/// release build doesn't compile `rpc_glue` at all, and this impl is the only
/// thing standing between the wire's storage keys and every caller of
/// `as_storage_key`, not just the RPC seam.
impl From<RoleAddressKind> for fauna_protocol::bridge_routing::RoleAddressKind {
    fn from(r: RoleAddressKind) -> Self {
        match r {
            RoleAddressKind::Postmaster => Self::Postmaster,
            RoleAddressKind::Abuse => Self::Abuse,
            RoleAddressKind::Noc => Self::Noc,
            RoleAddressKind::Security => Self::Security,
        }
    }
}

impl RoleAddressKind {
    /// The `role_address_overrides` JSON storage key + the reserved local-part
    /// this role routes (`postmaster` / `abuse` / `noc` / `security`).
    ///
    /// Delegates through the wire enum rather than repeating its match. The
    /// mirror above exists only because fauna-protocol carries no UniFFI
    /// scaffolding — the *keys* are the wire's, and a second copy of them here
    /// could drift from the storage the nest actually reads. This doc used to
    /// say the copy "matches the wire enum's `as_storage_key`", which is the
    /// hazard written down rather than removed; the `From` seam that already
    /// exists for every other crossing makes it structural instead.
    pub fn as_storage_key(self) -> &'static str {
        fauna_protocol::bridge_routing::RoleAddressKind::from(self).as_storage_key()
    }

    /// The four overridable roles in render order — the per-domain picker draws
    /// one `admin-dns-domain-role-address-<role>-select` dropdown per entry.
    pub const ALL: [RoleAddressKind; 4] = [
        RoleAddressKind::Postmaster,
        RoleAddressKind::Abuse,
        RoleAddressKind::Noc,
        RoleAddressKind::Security,
    ];
}

/// One overridable role address as an app's picker needs it: the role itself,
/// and the storage key that is simultaneously the `role_address_overrides` JSON
/// key, the reserved local-part, the `<key>@` caption and the
/// `admin-dns-domain-role-address-<key>-select` element-id suffix.
///
/// Exists because an app that is not written in Rust cannot call
/// [`RoleAddressKind::as_storage_key`], so before this every non-Rust app wrote
/// the four-arm map itself — web, apple, android and windows all did, each under
/// a comment naming the shared function it was mirroring. Handing the whole
/// table across the boundary is the shape `fauna_core::format::
/// content_floor_options` / `unknown_sender_options` already use for exactly
/// this problem (priority #2/#4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RoleAddressOption {
    /// The `role_address_overrides` storage key / reserved local-part —
    /// [`RoleAddressKind::as_storage_key`], never re-derived by the caller.
    pub key: String,
    /// The role this option designates. Serializes as the same tag the
    /// `SetRoleAddress` action carries, so a caller reading this table has both
    /// halves it needs and hand-writes neither.
    pub kind: RoleAddressKind,
}

/// The overridable RFC 2142 role addresses, in the render order
/// [`RoleAddressKind::ALL`] fixes — the one table every app's per-domain picker
/// draws from. Exported over UniFFI so android/apple/windows read this table
/// instead of hand-writing the four-arm map (`mail-multidomain.md` § Per-domain
/// role-address routing → *One owner for the role vocabulary*).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn role_address_options() -> Vec<RoleAddressOption> {
    RoleAddressKind::ALL
        .iter()
        .map(|&kind| RoleAddressOption {
            key: kind.as_storage_key().to_string(),
            kind,
        })
        .collect()
}

/// One per-domain role-address override as the picker renders it: the role and
/// the 32-byte actor designated as its delivery target. A role *absent* from
/// [`LocalDomainView::role_address_overrides`] has no override — it falls back to
/// the deployment admin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RoleAddressOverrideView {
    pub role: RoleAddressKind,
    pub actor_id: Vec<u8>,
}

/// A domain's published DMARC policy — the `p=` and `sp=` it asks receivers to
/// apply (`dmarc-reporting.md` § Multi-domain deployments). The per-domain
/// `admin-dns-domain-dmarc-policy-select` renders [`LocalDomainView::dmarc_policy`]
/// and dispatches [`LocalDomainAction::UpdateConfig`] with one of these.
///
/// The local UniFFI/wasm mirror of the wire `fauna_protocol::bridge_routing::DmarcMode`
/// (the same projection pattern as [`RoleAddressKind`]). `Monitor` is the RFC 7489
/// `p=none` — reports only, no enforcement — named so it cannot collide with
/// Swift's `Optional.none` in the generated bindings.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DomainDmarcPolicy {
    /// The deployment default (`p=reject; sp=reject`). Choosing it clears the
    /// domain's override rather than storing it.
    #[default]
    Reject,
    Quarantine,
    Monitor,
}

impl From<DomainDmarcPolicy> for DmarcMode {
    fn from(p: DomainDmarcPolicy) -> Self {
        match p {
            DomainDmarcPolicy::Reject => DmarcMode::Reject,
            DomainDmarcPolicy::Quarantine => DmarcMode::Quarantine,
            DomainDmarcPolicy::Monitor => DmarcMode::None,
        }
    }
}

impl From<DmarcMode> for DomainDmarcPolicy {
    fn from(m: DmarcMode) -> Self {
        match m {
            DmarcMode::Reject => DomainDmarcPolicy::Reject,
            DmarcMode::Quarantine => DomainDmarcPolicy::Quarantine,
            DmarcMode::None => DomainDmarcPolicy::Monitor,
        }
    }
}

/// One domain row as the page renders it. Projected from the wire
/// [`MailDomainRow`]; drops the not-yet-wired per-domain DKIM/override columns
/// (their consumer tracks surface them — see the module scope note). Carries
/// `catch_all_actor_id` — the catch-all *designation* surface (`admin-dns`
/// per-domain row, `admin.md` § 4); the RCPT-time resolver consumption is a
/// separate track (`mail-multidomain.md` § Implementation status today).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LocalDomainView {
    /// 16-byte `mail_domains.domain_id` (opaque). The primary-domain-rename
    /// picker sends this as the promotion target's id
    /// (`start_primary_domain_rename` is id-keyed, not name-keyed), and the
    /// rename-status projection resolves a rename row's `old`/`new` ids back to
    /// these names. Rendered nowhere directly.
    pub domain_id: Vec<u8>,
    pub domain: String,
    /// Exactly one active domain is primary; the primary cannot be removed
    /// (`mail-multidomain.md` § Additional domains `:88`). The UI hides /
    /// disables the remove affordance when true.
    pub is_primary: bool,
    pub mta_sts_mode: String,
    pub mta_sts_cert_mode: String,
    pub mta_sts_max_age_seconds: i64,
    pub spf_record: String,
    /// `None` until the per-domain DKIM/ACME track populates it.
    pub dkim_selector: Option<String>,
    /// Nest-computed signal that the active DKIM selector is due for rotation
    /// (`now - dkim_selector_activated_at ≥ effective rotation_days`; the
    /// deployment-wide default lives nest-side, so the client can't derive it).
    /// DKIM provisioning + rotation are fully **nest-side** now
    /// (`mail-bridge-lifecycle.md` § DKIM provisioning — the nest mints a key when
    /// the domain is added and the scheduled auto-flip activates a rotated one; no
    /// client producer reads this today). Kept as a projected status signal. `false` until
    /// the per-domain DKIM track populates a selector.
    pub dkim_rotation_due: bool,
    /// Epoch-ms the active `dkim_selector` last became active (stamped on every
    /// rotation flip); `None` until the first flip (the due window then runs from
    /// `added_at`). Drives a "last rotated" display + the producer's
    /// "already-provisioned-this-cycle" reasoning.
    pub dkim_selector_activated_at: Option<i64>,
    /// 32-byte actor id designated as this domain's catch-all, or `None` for no
    /// catch-all. The `admin-dns-domain-catch-all-select` picker renders this and
    /// dispatches [`LocalDomainAction::SetCatchAllActor`] to change it.
    pub catch_all_actor_id: Option<Vec<u8>>,
    /// Epoch-ms a succession ceremony or the boot reconcile last cleared
    /// `catch_all_actor_id` because it named a retired identity; `None` when
    /// the catch-all was never set, or was last set/cleared by an admin (a
    /// fresh admin decision supersedes it). Drives the admin-dns surface's
    /// "a succession cleared this catch-all" state (
    /// `succession-aftermath.md` § Re-key scope).
    pub catch_all_cleared_by_succession_at: Option<i64>,
    /// Per-domain role-address overrides (`mail-multidomain.md` § Per-domain
    /// role-address routing). One entry per overridable role that has an override;
    /// a role *not* listed falls back to the deployment admin. The four
    /// `admin-dns-domain-role-address-<role>-select` pickers render these and
    /// dispatch [`LocalDomainAction::SetRoleAddress`]. Projected from the wire
    /// typed `role_address_overrides` (`RoleAddressOverrides::resolve` — the
    /// same resolution the nest writer + RCPT-time resolver use, priority #2).
    pub role_address_overrides: Vec<RoleAddressOverrideView>,
    /// The domain's published DMARC policy — its stored override's
    /// `policy_mode`, else the deployment default `Reject`. The
    /// `admin-dns-domain-dmarc-policy-select` renders this.
    pub dmarc_policy: DomainDmarcPolicy,
    /// Epoch-millis the domain was claimed (per-app localized in UI).
    pub added_at: i64,
    /// Epoch-millis the domain was soft-deleted; `None` for active rows.
    pub removed_at: Option<i64>,
}

impl From<MailDomainRow> for LocalDomainView {
    fn from(r: MailDomainRow) -> Self {
        // The wire carries the override map typed (the nest projected the stored
        // column, degrading a malformed one to "no overrides"); list only the
        // roles that resolve to a valid actor.
        let overrides = &r.role_address_overrides;
        let role_address_overrides = RoleAddressKind::ALL
            .into_iter()
            .filter_map(|role| {
                overrides
                    .resolve(role.as_storage_key())
                    .map(|actor| RoleAddressOverrideView {
                        role,
                        actor_id: actor.to_vec(),
                    })
            })
            .collect();
        Self {
            domain_id: r.domain_id.into_vec(),
            domain: r.domain_name,
            is_primary: r.is_primary,
            mta_sts_mode: r.mta_sts_mode,
            mta_sts_cert_mode: r.mta_sts_cert_mode,
            mta_sts_max_age_seconds: r.mta_sts_max_age_seconds,
            spf_record: r.spf_record,
            dkim_selector: r.dkim_selector,
            dkim_rotation_due: r.dkim_rotation_due,
            dkim_selector_activated_at: r.dkim_selector_activated_at,
            catch_all_actor_id: r.catch_all_actor_id.map(|b| b.into_vec()),
            catch_all_cleared_by_succession_at: r.catch_all_cleared_by_succession_at,
            role_address_overrides,
            dmarc_policy: r
                .dmarc_overrides
                .policy_mode
                .map(DomainDmarcPolicy::from)
                .unwrap_or_default(),
            added_at: r.added_at,
            removed_at: r.removed_at,
        }
    }
}

impl LocalDomainView {
    /// The 32-byte actor overriding `role`'s delivery on this domain, or `None`
    /// (the role falls back to the deployment admin). The native picker reads this
    /// to set each role dropdown's initial selection. A plain (non-exported)
    /// helper — Rust consumers (linux) call it; FFI/wasm consumers iterate
    /// [`Self::role_address_overrides`] themselves.
    pub fn role_override(&self, role: RoleAddressKind) -> Option<&[u8]> {
        self.role_address_overrides
            .iter()
            .find(|o| o.role == role)
            .map(|o| o.actor_id.as_slice())
    }
}

/// Coarse machine status for spinner / disabled-control rendering. Mirrors
/// `BridgeApprovalStatus`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum LocalDomainStatus {
    Idle,
    Loading,
    Working,
}

/// Read-only snapshot the per-app UI renders for `admin-dns`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LocalDomainsSnapshot {
    /// Domains currently accepting mail (`removed_at IS NULL`).
    pub active: Vec<LocalDomainView>,
    /// Soft-deleted within the 30-day recovery window — each renders a
    /// "Restore" affordance (`mail-multidomain.md` § Re-add within 30 days).
    pub soft_deleted: Vec<LocalDomainView>,
    pub status: LocalDomainStatus,
    /// Last action's error, surfaced via the `error-message` element.
    pub error: Option<String>,
    /// `true` when the last `AddDomain` was an idempotent no-op re-add of an
    /// already-active domain (`add_local_domain` reply `skipped`). The UI may
    /// surface "domain already added" instead of a generic success toast.
    pub last_add_skipped: bool,
    /// The single in-flight primary-domain rename, or `None` when none is active
    /// (`mail-primary-domain-rename.md` § Lifecycle). Fetched best-effort on every
    /// refresh (a nest lacking the rename kinds degrades to `None` — see
    /// [`LocalDomainMachine::refresh`]); drives the `admin-dns-rename-banner` +
    /// the per-row `admin-dns-domain-rename-state`.
    pub active_rename: Option<PrimaryDomainRenameView>,
    /// `true` when the "Rename primary domain" affordance is offerable — there is
    /// a primary **and** at least one active non-primary domain to promote (the
    /// two-step rule surfaced as an enable/disable hint;
    /// [`crate::primary_domain_rename::rename_available`]). A UX hint only — the
    /// nest re-validates.
    pub rename_available: bool,
    /// `true` when `active` is empty, i.e. the *next* `AddDomain` would be the
    /// deployment's first — mirroring the nest's own `is_primary` derivation
    /// (`list_active_mail_domains().is_empty()`,
    /// `bridge_routing_handlers.rs::add_local_domain`) bit-for-bit, off the same
    /// already-fetched `active` list. The first domain added to a domainless
    /// nest becomes the **primary** and can never be removed from any app — a
    /// one-way door (`deployment-home-with-public-relay.md` § MUA reach;
    /// `mail-multidomain.md` § Removing a local domain). A UX hint only, driving
    /// the add-domain form's irreversibility warning; the nest is authority on
    /// whether an add is actually first.
    pub adding_first_domain: bool,
}

impl LocalDomainsSnapshot {
    fn empty() -> Self {
        Self {
            active: Vec::new(),
            soft_deleted: Vec::new(),
            status: LocalDomainStatus::Idle,
            error: None,
            last_add_skipped: false,
            active_rename: None,
            rename_available: false,
            adding_first_domain: false,
        }
    }
}

/// Actions the per-app UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum LocalDomainAction {
    /// Re-read the active + soft-deleted lists (page load / pull-to-refresh).
    Refresh,
    /// Claim a new mail-hosting domain. `mta_sts_cert_mode` carries the wizard
    /// pick (default [`DEFAULT_CERT_MODE`]). The MTA-STS policy *mode* is not an
    /// input: the nest stores `testing` and advances it to `enforce` by itself
    /// (`mail-multidomain.md` § The advance). The nest auto-sets `is_primary`
    /// (first domain = primary) and is idempotent on the domain name (re-adding
    /// an active domain is a no-op that sets
    /// [`LocalDomainsSnapshot::last_add_skipped`]).
    AddDomain {
        domain: String,
        mta_sts_cert_mode: String,
    },
    /// Soft-delete a domain (30-day recovery). The nest refuses the primary
    /// (`cannot_remove_primary_domain`); the resulting error surfaces.
    RemoveDomain { domain: String },
    /// Un-soft-delete a domain within the 30-day window.
    RestoreDomain { domain: String },
    /// Partial edit of the per-domain knobs (each `None` leaves it untouched;
    /// `update_local_domain_config`). `dmarc_policy` is the domain's published
    /// DMARC policy: `Some(Reject)`, the default, clears the override. The knobs
    /// that need a genuine clear are their own actions below.
    UpdateConfig {
        domain: String,
        mta_sts_max_age_seconds: Option<i64>,
        mta_sts_cert_mode: Option<String>,
        spf_record: Option<String>,
        dmarc_policy: Option<DomainDmarcPolicy>,
    },
    /// Designate (`Some(32-byte actor id)`) or clear (`None`) a domain's
    /// catch-all actor — the `admin-dns-domain-catch-all-select` picker
    /// (`mail-aliases.md` § Kind 4, `mail-multidomain.md` § Per-domain catch-all).
    /// Unlike [`UpdateConfig`], the whole action sets the catch-all, so a single
    /// `Option` is unambiguous (there is no "leave alone" — see
    /// `fauna.bridges.set_catch_all_actor`).
    SetCatchAllActor {
        domain: String,
        actor_id: Option<Vec<u8>>,
    },
    /// Designate (`Some(32-byte actor id)`) or clear (`None`) the per-domain
    /// override actor for one role address — the
    /// `admin-dns-domain-role-address-<role>-select` picker
    /// (`mail-multidomain.md` § Per-domain role-address routing). Like
    /// [`SetCatchAllActor`], the whole action sets one (domain, role), so a single
    /// `Option` is unambiguous: clear ⇒ the role falls back to the deployment
    /// admin. The nest atomic-merges, so setting one role preserves the others
    /// (`fauna.bridges.set_role_address`).
    SetRoleAddress {
        domain: String,
        role: RoleAddressKind,
        actor_id: Option<Vec<u8>>,
    },
    /// Begin renaming the deployment's primary domain to an existing active
    /// additional (`mail-primary-domain-rename.md` § Wire shapes). `new_primary_
    /// domain_id` is the 16-byte `domain_id` of the promotion target (from the
    /// picked [`LocalDomainView::domain_id`]); `grace_days` is the admin's
    /// grace-window pick (`None` → nest default 7; range `[1, 30]`). The nest
    /// validates all preconditions and surfaces a 409-class error on refusal —
    /// the client does **not** duplicate the rules (the "Rename primary"
    /// affordance's enable state is a hint, not a guard).
    StartPrimaryRename {
        new_primary_domain_id: Vec<u8>,
        grace_days: Option<i64>,
    },
    /// Finalize the in-flight rename. `rename_id` scopes it (from
    /// [`LocalDomainsSnapshot::active_rename`]); `force` completes early from
    /// `grace` (accepting the cache-flush risk). Offered per the
    /// `can_complete` / `can_force_complete` flags on the rename view.
    CompletePrimaryRename { rename_id: Vec<u8>, force: bool },
    /// Push the grace window out by `additional_days × 1 day` (`[1, 30]` per
    /// call). Valid from `grace` / `ready_to_complete` (`can_extend`).
    ExtendPrimaryRenameGrace {
        rename_id: Vec<u8>,
        additional_days: i64,
    },
    /// Unwind the in-flight rename (`can_abort`). Cheap pre-flip; the expensive
    /// inverse re-flip post-flip — the confirm dialog names the cost when
    /// `!is_pre_flip`. `reason` is an optional audit string.
    AbortPrimaryRename {
        rename_id: Vec<u8>,
        reason: Option<String>,
    },
}

/// WS-RPC seam to nest. Per-app glue implements this over `MailAdminClient`
/// (`libs/fauna-client-bridges`): `list_local_domains`, `add_local_domain`,
/// `remove_local_domain`, `restore_local_domain`, `update_local_domain_config`.
// Dual `async_trait` arm + `MaybeSendSync` supertrait so the one seam serves
// native (`Send` boxed futures, `Arc<NestClient>`) and wasm (`?Send`, the
// `Rc`-based `WsRpcClient`). See `fauna_protocol::MaybeSendSync`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait LocalDomainNest: MaybeSendSync {
    /// `(active, soft_deleted_within_30d)`.
    async fn list_local_domains(
        &self,
    ) -> Result<(Vec<MailDomainRow>, Vec<MailDomainRow>), NestError>;
    /// Returns `(row, skipped)` — `skipped == true` on an idempotent re-add of
    /// an already-active domain.
    async fn add_local_domain(
        &self,
        domain: String,
        mta_sts_cert_mode: String,
    ) -> Result<(MailDomainRow, bool), NestError>;
    async fn remove_local_domain(&self, domain: String) -> Result<MailDomainRow, NestError>;
    async fn restore_local_domain(&self, domain: String) -> Result<MailDomainRow, NestError>;
    async fn update_local_domain_config(
        &self,
        domain: String,
        mta_sts_max_age_seconds: Option<i64>,
        mta_sts_cert_mode: Option<String>,
        spf_record: Option<String>,
        dmarc_policy: Option<DomainDmarcPolicy>,
    ) -> Result<MailDomainRow, NestError>;
    /// Designate (`Some`) or clear (`None`) the domain's catch-all actor.
    async fn set_catch_all_actor(
        &self,
        domain: String,
        actor_id: Option<Vec<u8>>,
    ) -> Result<MailDomainRow, NestError>;
    /// Designate (`Some`) or clear (`None`) the per-domain override actor for one
    /// role address. The nest atomic-merges the override map; returns the updated row.
    async fn set_role_address(
        &self,
        domain: String,
        role: RoleAddressKind,
        actor_id: Option<Vec<u8>>,
    ) -> Result<MailDomainRow, NestError>;

    // --- Primary-domain rename (`mail-primary-domain-rename.md` § Wire shapes) ---
    // The `admin-dns` page's rename banner + per-row affordances ride the same
    // machine as the domain CRUD (priority #2 — one machine per page), so the six
    // rename RPCs are seam methods here. `list_primary_domain_renames` (audit) is
    // deferred with the audit-pane UI track.

    /// The in-flight rename, or `None` when none is active. **Best-effort** in
    /// `refresh` — a transient read failure degrades to
    /// `None` rather than failing the whole domain-list refresh.
    async fn get_primary_domain_rename_status(
        &self,
    ) -> Result<Option<MailDomainRenameRow>, NestError>;
    /// Begin a rename toward the 16-byte `new_primary_domain_id`; `grace_days`
    /// (`None` → nest default). The nest validates every precondition.
    async fn start_primary_domain_rename(
        &self,
        new_primary_domain_id: Vec<u8>,
        grace_days: Option<i64>,
    ) -> Result<MailDomainRenameRow, NestError>;
    /// Finalize `rename_id`; `force` completes early from `grace`.
    async fn complete_primary_domain_rename(
        &self,
        rename_id: Vec<u8>,
        force: bool,
    ) -> Result<MailDomainRenameRow, NestError>;
    /// Push `rename_id`'s grace window out by `additional_days`.
    async fn extend_primary_domain_rename_grace(
        &self,
        rename_id: Vec<u8>,
        additional_days: i64,
    ) -> Result<MailDomainRenameRow, NestError>;
    /// Unwind `rename_id`; `reason` is an optional audit string.
    async fn abort_primary_domain_rename(
        &self,
        rename_id: Vec<u8>,
        reason: Option<String>,
    ) -> Result<MailDomainRenameRow, NestError>;
}

/// One instance per admin client. Holds the rendered snapshot; drives the seam.
/// Mirrors `BridgeApprovalMachine`'s shape (snapshot + dispatch).
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct LocalDomainMachine {
    nest: Arc<dyn LocalDomainNest>,
    inner: Mutex<LocalDomainsSnapshot>,
}

// `new` takes `Arc<dyn LocalDomainNest>` (not an FFI type), so it stays in a
// plain (non-exported) impl alongside the private helpers. The FFI surface —
// `snapshot` (sync) + `hydrate`/`dispatch` (async) — lives in the exported impl
// blocks below (mirrors the onboarding-machine export/private-helper split).
impl LocalDomainMachine {
    pub fn new(nest: Arc<dyn LocalDomainNest>) -> Self {
        Self {
            nest,
            inner: Mutex::new(LocalDomainsSnapshot::empty()),
        }
    }

    fn set_status(&self, status: LocalDomainStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Loading);
        let (active, soft_deleted) = self.nest.list_local_domains().await?;
        // Best-effort in-flight-rename fetch. The domain list is the hard
        // requirement; the rename status is additive. A failed status read
        // (a transient failure) degrades to "no
        // rename" and still renders the domain list rather than breaking the whole
        // `admin-dns` page; it self-heals on the next
        // refresh (nest state is authoritative; the banner is informational).
        let rename_row = match self.nest.get_primary_domain_rename_status().await {
            Ok(row) => row,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "primary-domain rename status unavailable; rendering domains without the rename banner"
                );
                None
            }
        };
        let active_views: Vec<LocalDomainView> =
            active.into_iter().map(LocalDomainView::from).collect();
        let soft_views: Vec<LocalDomainView> = soft_deleted
            .into_iter()
            .map(LocalDomainView::from)
            .collect();
        let rename_offerable = rename_available(&active_views);
        let active_rename =
            rename_row.map(|r| PrimaryDomainRenameView::project(&r, &active_views, &soft_views));
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.adding_first_domain = active_views.is_empty();
        snap.active = active_views;
        snap.soft_deleted = soft_views;
        snap.rename_available = rename_offerable;
        snap.active_rename = active_rename;
        snap.status = LocalDomainStatus::Idle;
        Ok(())
    }

    async fn add_domain(
        &self,
        domain: String,
        mta_sts_cert_mode: String,
    ) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        // Normalize per the row-shape contract (lowercased; `mail-multidomain.md`
        // § Row shape `:56`). The nest re-validates RFC-1035 syntax + uniqueness.
        let domain = domain.trim().to_lowercase();
        let (_row, skipped) = self
            .nest
            .add_local_domain(domain, mta_sts_cert_mode)
            .await?;
        self.inner.lock().expect("snapshot mutex").last_add_skipped = skipped;
        // Re-read so the new (or already-present) row + the primary flag appear.
        self.refresh().await
    }

    async fn remove_domain(&self, domain: String) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        // The primary-domain guard is the nest's (it refuses with
        // `cannot_remove_primary_domain`); we don't duplicate the rule from a
        // possibly-stale snapshot. The UI hides Remove on `is_primary` rows.
        self.nest.remove_local_domain(domain).await?;
        self.refresh().await
    }

    async fn restore_domain(&self, domain: String) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        self.nest.restore_local_domain(domain).await?;
        self.refresh().await
    }

    async fn update_config(
        &self,
        domain: String,
        mta_sts_max_age_seconds: Option<i64>,
        mta_sts_cert_mode: Option<String>,
        spf_record: Option<String>,
        dmarc_policy: Option<DomainDmarcPolicy>,
    ) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        self.nest
            .update_local_domain_config(
                domain,
                mta_sts_max_age_seconds,
                mta_sts_cert_mode,
                spf_record,
                dmarc_policy,
            )
            .await?;
        self.refresh().await
    }

    async fn set_catch_all_actor(
        &self,
        domain: String,
        actor_id: Option<Vec<u8>>,
    ) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        self.nest.set_catch_all_actor(domain, actor_id).await?;
        // Re-read so the row's `catch_all_actor_id` reflects the change.
        self.refresh().await
    }

    async fn set_role_address(
        &self,
        domain: String,
        role: RoleAddressKind,
        actor_id: Option<Vec<u8>>,
    ) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        self.nest.set_role_address(domain, role, actor_id).await?;
        // Re-read so the row's `role_address_overrides` reflect the change.
        self.refresh().await
    }

    // Each rename action mutates nest state then re-reads via `refresh` — so the
    // snapshot's `active_rename` reflects the new lifecycle state (and the
    // per-domain `is_primary` flip, once the rename reaches grace). Preconditions
    // are the nest's (single-active-rename, cert-mode, TLS-posture, SAN-cap,
    // grace-not-expired); a refusal surfaces as the dispatched error, exactly
    // like the remove-primary guard.

    async fn start_primary_rename(
        &self,
        new_primary_domain_id: Vec<u8>,
        grace_days: Option<i64>,
    ) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        self.nest
            .start_primary_domain_rename(new_primary_domain_id, grace_days)
            .await?;
        self.refresh().await
    }

    async fn complete_primary_rename(
        &self,
        rename_id: Vec<u8>,
        force: bool,
    ) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        self.nest
            .complete_primary_domain_rename(rename_id, force)
            .await?;
        self.refresh().await
    }

    async fn extend_primary_rename_grace(
        &self,
        rename_id: Vec<u8>,
        additional_days: i64,
    ) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        self.nest
            .extend_primary_domain_rename_grace(rename_id, additional_days)
            .await?;
        self.refresh().await
    }

    async fn abort_primary_rename(
        &self,
        rename_id: Vec<u8>,
        reason: Option<String>,
    ) -> Result<(), DispatchError> {
        self.set_status(LocalDomainStatus::Working);
        self.nest
            .abort_primary_domain_rename(rename_id, reason)
            .await?;
        self.refresh().await
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl LocalDomainMachine {
    pub fn snapshot(&self) -> LocalDomainsSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl LocalDomainMachine {
    /// Initial page load.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    pub async fn dispatch(&self, action: LocalDomainAction) -> Result<(), DispatchError> {
        // Clear the prior action's transients before the new action runs.
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.error = None;
            snap.last_add_skipped = false;
        }
        let result = match action {
            LocalDomainAction::Refresh => self.refresh().await,
            LocalDomainAction::AddDomain {
                domain,
                mta_sts_cert_mode,
            } => self.add_domain(domain, mta_sts_cert_mode).await,
            LocalDomainAction::RemoveDomain { domain } => self.remove_domain(domain).await,
            LocalDomainAction::RestoreDomain { domain } => self.restore_domain(domain).await,
            LocalDomainAction::UpdateConfig {
                domain,
                mta_sts_max_age_seconds,
                mta_sts_cert_mode,
                spf_record,
                dmarc_policy,
            } => {
                self.update_config(
                    domain,
                    mta_sts_max_age_seconds,
                    mta_sts_cert_mode,
                    spf_record,
                    dmarc_policy,
                )
                .await
            }
            LocalDomainAction::SetCatchAllActor { domain, actor_id } => {
                self.set_catch_all_actor(domain, actor_id).await
            }
            LocalDomainAction::SetRoleAddress {
                domain,
                role,
                actor_id,
            } => self.set_role_address(domain, role, actor_id).await,
            LocalDomainAction::StartPrimaryRename {
                new_primary_domain_id,
                grace_days,
            } => {
                self.start_primary_rename(new_primary_domain_id, grace_days)
                    .await
            }
            LocalDomainAction::CompletePrimaryRename { rename_id, force } => {
                self.complete_primary_rename(rename_id, force).await
            }
            LocalDomainAction::ExtendPrimaryRenameGrace {
                rename_id,
                additional_days,
            } => {
                self.extend_primary_rename_grace(rename_id, additional_days)
                    .await
            }
            LocalDomainAction::AbortPrimaryRename { rename_id, reason } => {
                self.abort_primary_rename(rename_id, reason).await
            }
        };
        if let Err(ref e) = result {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            crate::state::set_snapshot_error(&mut snap.error, e.to_string());
            snap.status = LocalDomainStatus::Idle;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// The four storage keys are the nest's, not this crate's — they name
    /// `role_address_overrides` entries the nest reads back. Pinned literally
    /// (a derivation could rename them in step with itself and still be
    /// wrong), and pinned as *equal to the wire enum's* so a future edit that
    /// re-inlines the match here fails instead of drifting silently.
    #[test]
    fn role_storage_keys_are_the_wire_enum_s() {
        let expected = [
            (RoleAddressKind::Postmaster, "postmaster"),
            (RoleAddressKind::Abuse, "abuse"),
            (RoleAddressKind::Noc, "noc"),
            (RoleAddressKind::Security, "security"),
        ];
        for (role, key) in expected {
            assert_eq!(role.as_storage_key(), key);
            assert_eq!(
                role.as_storage_key(),
                fauna_protocol::bridge_routing::RoleAddressKind::from(role).as_storage_key(),
                "{role:?} disagrees with the wire enum it mirrors"
            );
        }
        // Every overridable role is covered, and no two share a key — either
        // would silently redirect one role's mail to another's override.
        assert_eq!(RoleAddressKind::ALL.len(), expected.len());
        let keys: std::collections::BTreeSet<&str> = RoleAddressKind::ALL
            .iter()
            .map(|r| r.as_storage_key())
            .collect();
        assert_eq!(keys.len(), RoleAddressKind::ALL.len());
    }

    /// The table an app's picker draws from carries BOTH halves it would
    /// otherwise hand-write: the storage key, and the serialized `kind` tag.
    ///
    /// The tag is pinned literally and not merely round-tripped, because it is
    /// the half a Rust-side test is least likely to notice breaking: web sends
    /// it verbatim as the `SetRoleAddress` action's `role` and matches it
    /// against `role_address_overrides[].role`. A `#[serde(rename_all)]` added
    /// to [`RoleAddressKind`] would keep every Rust caller compiling and
    /// passing while silently making the web picker address a role the nest
    /// does not recognise — the override would then designate nothing, with no
    /// error anywhere.
    #[test]
    fn role_address_options_carry_the_key_and_the_wire_tag() {
        let opts = role_address_options();
        assert_eq!(opts.len(), RoleAddressKind::ALL.len());
        let expected = [
            ("postmaster", "\"Postmaster\""),
            ("abuse", "\"Abuse\""),
            ("noc", "\"Noc\""),
            ("security", "\"Security\""),
        ];
        for (opt, (key, tag)) in opts.iter().zip(expected) {
            assert_eq!(opt.key, key, "storage key drifted from the wire enum's");
            assert_eq!(
                serde_json::to_string(&opt.kind).expect("role kind serializes"),
                tag,
                "the `SetRoleAddress` tag every app hands back drifted",
            );
            assert_eq!(opt.key, opt.kind.as_storage_key());
        }
        // Render order is the picker's row order on all seven apps, so it is
        // part of the contract, not an implementation detail.
        let order: Vec<&str> = opts.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(order, ["postmaster", "abuse", "noc", "security"]);
    }

    /// In-memory nest modelling the `mail_domains` row lifecycle: add appends
    /// to `active` (idempotent), remove moves active→soft (refusing primary),
    /// restore moves soft→active, update mutates the active row in place.
    #[derive(Default)]
    struct FakeNest {
        active: StdMutex<Vec<MailDomainRow>>,
        soft: StdMutex<Vec<MailDomainRow>>,
        /// The single in-flight rename (preset by a test, or created by
        /// `start_primary_domain_rename`).
        rename: StdMutex<Option<MailDomainRenameRow>>,
        /// When true, `get_primary_domain_rename_status` errors — models a failed
        /// status read (best-effort-degradation test).
        rename_status_err: StdMutex<bool>,
    }

    fn row(domain: &str, is_primary: bool) -> MailDomainRow {
        MailDomainRow {
            domain_name: domain.to_string(),
            is_primary,
            added_at: 1_700_000_000_000,
            mta_sts_mode: "testing".to_string(),
            mta_sts_cert_mode: "expand_primary".to_string(),
            mta_sts_max_age_seconds: 86_400,
            spf_record: "v=spf1 mx ~all".to_string(),
            ..Default::default()
        }
    }

    /// A row with an explicit 16-byte `domain_id` (`[id; 16]`) so rename tests can
    /// address a promotion target by id + resolve the rename row's ids to names.
    fn row_id(domain: &str, is_primary: bool, id: u8) -> MailDomainRow {
        let mut r = row(domain, is_primary);
        r.domain_id = serde_bytes::ByteBuf::from(vec![id; 16]);
        r
    }

    /// A preset in-flight rename row in `state`, from `old_id` to `new_id`.
    fn rename_row(state: &str, old_id: u8, new_id: u8) -> MailDomainRenameRow {
        MailDomainRenameRow {
            rename_id: serde_bytes::ByteBuf::from(vec![0xAB; 16]),
            old_primary_domain_id: serde_bytes::ByteBuf::from(vec![old_id; 16]),
            new_primary_domain_id: serde_bytes::ByteBuf::from(vec![new_id; 16]),
            state: state.to_string(),
            started_at: 1_700_000_100_000,
            grace_days: 7,
            initiated_by_actor_id: serde_bytes::ByteBuf::from(vec![0x1; 32]),
            ..Default::default()
        }
    }

    #[async_trait]
    impl LocalDomainNest for FakeNest {
        async fn list_local_domains(
            &self,
        ) -> Result<(Vec<MailDomainRow>, Vec<MailDomainRow>), NestError> {
            Ok((
                self.active.lock().unwrap().clone(),
                self.soft.lock().unwrap().clone(),
            ))
        }

        async fn add_local_domain(
            &self,
            domain: String,
            mta_sts_cert_mode: String,
        ) -> Result<(MailDomainRow, bool), NestError> {
            let mut active = self.active.lock().unwrap();
            if let Some(existing) = active.iter().find(|r| r.domain_name == domain) {
                // Idempotent re-add of an already-active domain.
                return Ok((existing.clone(), true));
            }
            // First domain claimed is the primary (nest auto-sets `is_primary`).
            let is_primary = active.is_empty();
            let mut r = row(&domain, is_primary);
            r.mta_sts_cert_mode = mta_sts_cert_mode;
            active.push(r.clone());
            Ok((r, false))
        }

        async fn remove_local_domain(&self, domain: String) -> Result<MailDomainRow, NestError> {
            let mut active = self.active.lock().unwrap();
            let idx = active
                .iter()
                .position(|r| r.domain_name == domain)
                .ok_or_else(|| NestError::Rejected("unknown_domain".to_string()))?;
            if active[idx].is_primary {
                return Err(NestError::Rejected(
                    "cannot_remove_primary_domain".to_string(),
                ));
            }
            let mut r = active.remove(idx);
            r.removed_at = Some(1_700_000_500_000);
            self.soft.lock().unwrap().push(r.clone());
            Ok(r)
        }

        async fn restore_local_domain(&self, domain: String) -> Result<MailDomainRow, NestError> {
            let mut soft = self.soft.lock().unwrap();
            let idx = soft
                .iter()
                .position(|r| r.domain_name == domain)
                .ok_or_else(|| NestError::Rejected("unknown_domain".to_string()))?;
            let mut r = soft.remove(idx);
            r.removed_at = None;
            self.active.lock().unwrap().push(r.clone());
            Ok(r)
        }

        async fn update_local_domain_config(
            &self,
            domain: String,
            mta_sts_max_age_seconds: Option<i64>,
            mta_sts_cert_mode: Option<String>,
            spf_record: Option<String>,
            dmarc_policy: Option<DomainDmarcPolicy>,
        ) -> Result<MailDomainRow, NestError> {
            let mut active = self.active.lock().unwrap();
            let r = active
                .iter_mut()
                .find(|r| r.domain_name == domain)
                .ok_or_else(|| NestError::Rejected("unknown_domain".to_string()))?;
            if let Some(v) = mta_sts_max_age_seconds {
                r.mta_sts_max_age_seconds = v;
            }
            if let Some(v) = mta_sts_cert_mode {
                r.mta_sts_cert_mode = v;
            }
            if let Some(v) = spf_record {
                r.spf_record = v;
            }
            // The nest's rule: the default clears both keys, a softer policy
            // sets both (`fauna_mail::dmarc_publish::set_policy_mode_json`).
            if let Some(p) = dmarc_policy {
                let mode = match p {
                    DomainDmarcPolicy::Reject => None,
                    soft => Some(DmarcMode::from(soft)),
                };
                r.dmarc_overrides.policy_mode = mode;
                r.dmarc_overrides.subdomain_policy_mode = mode;
            }
            Ok(r.clone())
        }

        async fn set_catch_all_actor(
            &self,
            domain: String,
            actor_id: Option<Vec<u8>>,
        ) -> Result<MailDomainRow, NestError> {
            let mut active = self.active.lock().unwrap();
            let r = active
                .iter_mut()
                .find(|r| r.domain_name == domain)
                .ok_or_else(|| NestError::Rejected("unknown_domain".to_string()))?;
            r.catch_all_actor_id = actor_id.map(serde_bytes::ByteBuf::from);
            Ok(r.clone())
        }

        async fn set_role_address(
            &self,
            domain: String,
            role: RoleAddressKind,
            actor_id: Option<Vec<u8>>,
        ) -> Result<MailDomainRow, NestError> {
            let mut active = self.active.lock().unwrap();
            let r = active
                .iter_mut()
                .find(|r| r.domain_name == domain)
                .ok_or_else(|| NestError::Rejected("unknown_domain".to_string()))?;
            // Atomic read-merge-write of the override map (mirrors the nest writer).
            r.role_address_overrides
                .set(role.as_storage_key(), actor_id.map(hex::encode));
            Ok(r.clone())
        }

        async fn get_primary_domain_rename_status(
            &self,
        ) -> Result<Option<MailDomainRenameRow>, NestError> {
            if *self.rename_status_err.lock().unwrap() {
                return Err(NestError::Transient("rename_kind_unavailable".to_string()));
            }
            Ok(self.rename.lock().unwrap().clone())
        }

        async fn start_primary_domain_rename(
            &self,
            new_primary_domain_id: Vec<u8>,
            _grace_days: Option<i64>,
        ) -> Result<MailDomainRenameRow, NestError> {
            // Single-active-rename guard (the nest's `rename_already_in_progress`).
            if self.rename.lock().unwrap().is_some() {
                return Err(NestError::Rejected(
                    "rename_already_in_progress".to_string(),
                ));
            }
            let active = self.active.lock().unwrap();
            let old = active
                .iter()
                .find(|r| r.is_primary)
                .ok_or_else(|| NestError::Rejected("no_primary_domain".to_string()))?;
            // Two-step rule: the target must be an existing active non-primary.
            let new = active
                .iter()
                .find(|r| r.domain_id.as_ref() == new_primary_domain_id.as_slice())
                .ok_or_else(|| NestError::Rejected("new_primary_must_be_additional".to_string()))?;
            if new.is_primary {
                return Err(NestError::Rejected("same_domain_for_rename".to_string()));
            }
            let row = MailDomainRenameRow {
                rename_id: serde_bytes::ByteBuf::from(vec![0xCD; 16]),
                old_primary_domain_id: old.domain_id.clone(),
                new_primary_domain_id: serde_bytes::ByteBuf::from(new_primary_domain_id),
                state: "requested".to_string(),
                started_at: 1_700_000_100_000,
                grace_days: 7,
                initiated_by_actor_id: serde_bytes::ByteBuf::from(vec![0x1; 32]),
                ..Default::default()
            };
            *self.rename.lock().unwrap() = Some(row.clone());
            Ok(row)
        }

        async fn complete_primary_domain_rename(
            &self,
            _rename_id: Vec<u8>,
            force: bool,
        ) -> Result<MailDomainRenameRow, NestError> {
            let mut guard = self.rename.lock().unwrap();
            let mut row = guard
                .clone()
                .ok_or_else(|| NestError::Rejected("no_active_rename".to_string()))?;
            let ready = row.state == "ready_to_complete";
            let grace = row.state == "grace";
            if !ready && !(grace && force) {
                return Err(NestError::Rejected("grace_period_not_expired".to_string()));
            }
            row.state = "completed".to_string();
            row.completed_at = Some(1_700_001_000_000);
            // Terminal → no longer the *active* rename (`get_status` returns None).
            *guard = None;
            Ok(row)
        }

        async fn extend_primary_domain_rename_grace(
            &self,
            _rename_id: Vec<u8>,
            additional_days: i64,
        ) -> Result<MailDomainRenameRow, NestError> {
            let mut guard = self.rename.lock().unwrap();
            let row = guard
                .as_mut()
                .ok_or_else(|| NestError::Rejected("no_active_rename".to_string()))?;
            if row.state != "grace" && row.state != "ready_to_complete" {
                return Err(NestError::Rejected("rename_not_in_grace".to_string()));
            }
            // A ready_to_complete row reverts to grace (deadline is future again).
            row.state = "grace".to_string();
            row.grace_ends_at = Some(row.grace_ends_at.unwrap_or(0) + additional_days * 86_400_000);
            Ok(row.clone())
        }

        async fn abort_primary_domain_rename(
            &self,
            _rename_id: Vec<u8>,
            reason: Option<String>,
        ) -> Result<MailDomainRenameRow, NestError> {
            let mut guard = self.rename.lock().unwrap();
            let mut row = guard
                .clone()
                .ok_or_else(|| NestError::Rejected("no_active_rename".to_string()))?;
            if row.state == "completed" || row.state == "aborted" {
                return Err(NestError::Rejected("rename_terminal".to_string()));
            }
            row.state = "aborted".to_string();
            row.aborted_at = Some(1_700_001_000_000);
            row.abort_reason = reason;
            *guard = None;
            Ok(row)
        }
    }

    fn machine_with(active: Vec<MailDomainRow>, soft: Vec<MailDomainRow>) -> LocalDomainMachine {
        LocalDomainMachine::new(Arc::new(FakeNest {
            active: StdMutex::new(active),
            soft: StdMutex::new(soft),
            ..Default::default()
        }))
    }

    /// Build a machine whose fake nest also carries a preset in-flight rename.
    fn machine_with_rename(
        active: Vec<MailDomainRow>,
        rename: Option<MailDomainRenameRow>,
    ) -> LocalDomainMachine {
        LocalDomainMachine::new(Arc::new(FakeNest {
            active: StdMutex::new(active),
            rename: StdMutex::new(rename),
            ..Default::default()
        }))
    }

    #[test]
    fn projection_carries_dkim_rotation_signal() {
        // The automatic rotation producer reads these off the projection; the
        // wire `MailDomainRow` carries them, the client `LocalDomainView` must
        // surface them (regression: the projection used to drop them).
        let mut r = row("primary.example", true);
        r.dkim_selector = Some("202603".to_string());
        r.dkim_selector_activated_at = Some(1_700_000_000_000);
        r.dkim_rotation_due = true;
        let v = LocalDomainView::from(r);
        assert_eq!(v.dkim_selector.as_deref(), Some("202603"));
        assert!(v.dkim_rotation_due);
        assert_eq!(v.dkim_selector_activated_at, Some(1_700_000_000_000));

        // Default row (no DKIM provisioned yet) projects "not due, never flipped".
        let v0 = LocalDomainView::from(row("two.example", false));
        assert!(!v0.dkim_rotation_due);
        assert_eq!(v0.dkim_selector_activated_at, None);
    }

    #[tokio::test]
    async fn refresh_splits_active_and_soft_deleted() {
        let m = machine_with(
            vec![row("primary.example", true), row("two.example", false)],
            vec![row("gone.example", false)],
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.active.len(), 2);
        assert_eq!(snap.soft_deleted.len(), 1);
        assert_eq!(snap.soft_deleted[0].domain, "gone.example");
        assert!(
            snap.active
                .iter()
                .any(|d| d.domain == "primary.example" && d.is_primary)
        );
        assert!(
            snap.active
                .iter()
                .any(|d| d.domain == "two.example" && !d.is_primary)
        );
        assert_eq!(snap.status, LocalDomainStatus::Idle);
    }

    #[tokio::test]
    async fn set_catch_all_actor_designates_and_clears_in_snapshot() {
        let m = machine_with(vec![row("primary.example", true)], vec![]);
        m.hydrate().await.unwrap();
        assert!(m.snapshot().active[0].catch_all_actor_id.is_none());

        // Designate.
        let actor = vec![9u8; 32];
        m.dispatch(LocalDomainAction::SetCatchAllActor {
            domain: "primary.example".to_string(),
            actor_id: Some(actor.clone()),
        })
        .await
        .unwrap();
        assert_eq!(
            m.snapshot().active[0].catch_all_actor_id.as_deref(),
            Some(actor.as_slice())
        );

        // Clear.
        m.dispatch(LocalDomainAction::SetCatchAllActor {
            domain: "primary.example".to_string(),
            actor_id: None,
        })
        .await
        .unwrap();
        assert!(m.snapshot().active[0].catch_all_actor_id.is_none());
    }

    #[tokio::test]
    async fn set_role_address_designates_and_clears_in_snapshot() {
        let m = machine_with(vec![row("primary.example", true)], vec![]);
        m.hydrate().await.unwrap();
        assert!(
            m.snapshot().active[0].role_address_overrides.is_empty(),
            "no overrides initially"
        );
        assert!(
            m.snapshot().active[0]
                .role_override(RoleAddressKind::Abuse)
                .is_none()
        );

        // Delegate abuse@ to a moderator actor.
        let moderator = vec![0x7u8; 32];
        m.dispatch(LocalDomainAction::SetRoleAddress {
            domain: "primary.example".to_string(),
            role: RoleAddressKind::Abuse,
            actor_id: Some(moderator.clone()),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(
            snap.active[0].role_override(RoleAddressKind::Abuse),
            Some(moderator.as_slice())
        );
        // Setting one role leaves the others on the admin default (unset).
        assert!(
            snap.active[0]
                .role_override(RoleAddressKind::Postmaster)
                .is_none()
        );
        assert_eq!(snap.active[0].role_address_overrides.len(), 1);

        // Clear it → back to admin default (the override is gone).
        m.dispatch(LocalDomainAction::SetRoleAddress {
            domain: "primary.example".to_string(),
            role: RoleAddressKind::Abuse,
            actor_id: None,
        })
        .await
        .unwrap();
        assert!(
            m.snapshot().active[0].role_address_overrides.is_empty(),
            "clear removes the override"
        );
    }

    #[tokio::test]
    async fn set_role_address_atomic_merge_preserves_other_roles() {
        let m = machine_with(vec![row("primary.example", true)], vec![]);
        m.hydrate().await.unwrap();
        let pm = vec![0x1u8; 32];
        let sec = vec![0x2u8; 32];
        for (role, actor) in [
            (RoleAddressKind::Postmaster, pm.clone()),
            (RoleAddressKind::Security, sec.clone()),
        ] {
            m.dispatch(LocalDomainAction::SetRoleAddress {
                domain: "primary.example".to_string(),
                role,
                actor_id: Some(actor),
            })
            .await
            .unwrap();
        }
        let snap = m.snapshot();
        // The second set preserved the first (atomic read-merge-write).
        assert_eq!(
            snap.active[0].role_override(RoleAddressKind::Postmaster),
            Some(pm.as_slice())
        );
        assert_eq!(
            snap.active[0].role_override(RoleAddressKind::Security),
            Some(sec.as_slice())
        );
        assert_eq!(snap.active[0].role_address_overrides.len(), 2);
    }

    #[tokio::test]
    async fn add_first_domain_becomes_primary_and_normalizes_case() {
        let m = machine_with(vec![], vec![]);
        m.dispatch(LocalDomainAction::AddDomain {
            domain: "  Example.COM  ".to_string(),
            mta_sts_cert_mode: DEFAULT_CERT_MODE.to_string(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.active.len(), 1);
        // Trimmed + lowercased per the row-shape contract.
        assert_eq!(snap.active[0].domain, "example.com");
        assert!(snap.active[0].is_primary, "first domain claimed is primary");
        assert_eq!(snap.active[0].mta_sts_mode, "testing");
        assert!(!snap.last_add_skipped);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn add_existing_domain_is_idempotent_and_sets_skipped() {
        let m = machine_with(vec![row("example.com", true)], vec![]);
        m.dispatch(LocalDomainAction::AddDomain {
            domain: "example.com".to_string(),
            mta_sts_cert_mode: DEFAULT_CERT_MODE.to_string(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.active.len(), 1, "no duplicate row");
        assert!(snap.last_add_skipped, "idempotent re-add sets skipped");
    }

    #[tokio::test]
    async fn remove_non_primary_moves_to_soft_deleted() {
        let m = machine_with(
            vec![row("primary.example", true), row("two.example", false)],
            vec![],
        );
        m.dispatch(LocalDomainAction::RemoveDomain {
            domain: "two.example".to_string(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.active.len(), 1);
        assert!(!snap.active.iter().any(|d| d.domain == "two.example"));
        assert_eq!(snap.soft_deleted.len(), 1);
        assert_eq!(snap.soft_deleted[0].domain, "two.example");
        assert!(snap.soft_deleted[0].removed_at.is_some());
    }

    #[tokio::test]
    async fn remove_primary_is_refused_and_surfaces_error() {
        let m = machine_with(vec![row("primary.example", true)], vec![]);
        m.hydrate().await.unwrap();
        let err = m
            .dispatch(LocalDomainAction::RemoveDomain {
                domain: "primary.example".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)));
        let snap = m.snapshot();
        // The error path does not refresh, so the hydrated snapshot still shows
        // the primary — it was never removed.
        assert_eq!(snap.active.len(), 1, "primary not removed");
        assert!(
            snap.error
                .as_deref()
                .unwrap()
                .contains("cannot_remove_primary_domain"),
            "error: {:?}",
            snap.error
        );
        assert_eq!(snap.status, LocalDomainStatus::Idle);
    }

    #[tokio::test]
    async fn restore_moves_soft_deleted_back_to_active() {
        let m = machine_with(vec![], vec![row("gone.example", false)]);
        m.dispatch(LocalDomainAction::RestoreDomain {
            domain: "gone.example".to_string(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.active.len(), 1);
        assert_eq!(snap.active[0].domain, "gone.example");
        assert!(snap.active[0].removed_at.is_none());
        assert!(snap.soft_deleted.is_empty());
    }

    #[tokio::test]
    async fn update_config_applies_and_refreshes() {
        let m = machine_with(vec![row("example.com", true)], vec![]);
        m.dispatch(LocalDomainAction::UpdateConfig {
            domain: "example.com".to_string(),
            mta_sts_max_age_seconds: Some(604_800),
            mta_sts_cert_mode: None,
            spf_record: Some("v=spf1 mx -all".to_string()),
            dmarc_policy: None,
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.active[0].mta_sts_max_age_seconds, 604_800);
        assert_eq!(snap.active[0].spf_record, "v=spf1 mx -all");
        // Untouched knob unchanged.
        assert_eq!(snap.active[0].mta_sts_cert_mode, "expand_primary");
        assert_eq!(snap.active[0].dmarc_policy, DomainDmarcPolicy::Reject);
    }

    #[tokio::test]
    async fn update_config_softens_and_restores_one_domains_dmarc_policy() {
        let m = machine_with(
            vec![row("example.com", true), row("other.example", false)],
            vec![],
        );
        let set = |p: DomainDmarcPolicy| LocalDomainAction::UpdateConfig {
            domain: "other.example".to_string(),
            mta_sts_max_age_seconds: None,
            mta_sts_cert_mode: None,
            spf_record: None,
            dmarc_policy: Some(p),
        };
        m.dispatch(set(DomainDmarcPolicy::Monitor)).await.unwrap();
        let snap = m.snapshot();
        let policy = |d: &str| {
            snap.active
                .iter()
                .find(|v| v.domain == d)
                .map(|v| v.dmarc_policy)
                .unwrap()
        };
        assert_eq!(policy("other.example"), DomainDmarcPolicy::Monitor);
        assert_eq!(policy("example.com"), DomainDmarcPolicy::Reject);

        m.dispatch(set(DomainDmarcPolicy::Reject)).await.unwrap();
        let snap = m.snapshot();
        let other = snap
            .active
            .iter()
            .find(|v| v.domain == "other.example")
            .unwrap();
        assert_eq!(other.dmarc_policy, DomainDmarcPolicy::Reject);
    }

    #[test]
    fn domain_dmarc_policy_maps_to_the_wire_modes() {
        for (p, m) in [
            (DomainDmarcPolicy::Reject, DmarcMode::Reject),
            (DomainDmarcPolicy::Quarantine, DmarcMode::Quarantine),
            (DomainDmarcPolicy::Monitor, DmarcMode::None),
        ] {
            assert_eq!(DmarcMode::from(p), m);
            assert_eq!(DomainDmarcPolicy::from(m), p);
        }
    }

    // --- Primary-domain rename (folded into the same machine) ---

    #[tokio::test]
    async fn refresh_surfaces_active_rename_and_availability() {
        // A primary + a non-primary additional, with a preset `requested` rename
        // from the primary (id 1) to the additional (id 2).
        let m = machine_with_rename(
            vec![
                row_id("old.example", true, 1),
                row_id("new.example", false, 2),
            ],
            Some(rename_row("requested", 1, 2)),
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert!(snap.rename_available, "primary + additional → offerable");
        let r = snap.active_rename.expect("active rename surfaced");
        assert_eq!(r.state, "requested");
        assert_eq!(r.old_primary_domain, "old.example");
        assert_eq!(r.new_primary_domain, "new.example");
        assert!(r.is_pre_flip);
        assert!(r.can_abort);
        assert!(!r.can_complete);
    }

    #[tokio::test]
    async fn rename_unavailable_with_only_a_primary() {
        let m = machine_with_rename(vec![row_id("solo.example", true, 1)], None);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert!(!snap.rename_available, "no non-primary to promote");
        assert!(snap.active_rename.is_none());
    }

    #[tokio::test]
    async fn adding_first_domain_is_true_only_on_a_domainless_nest() {
        let domainless = machine_with(vec![], vec![]);
        domainless.hydrate().await.unwrap();
        assert!(
            domainless.snapshot().adding_first_domain,
            "no active domain yet → the next add would be primary"
        );

        let already_has_one = machine_with(vec![row_id("existing.example", true, 1)], vec![]);
        already_has_one.hydrate().await.unwrap();
        assert!(
            !already_has_one.snapshot().adding_first_domain,
            "a primary already exists → the next add is ordinary"
        );
    }

    #[tokio::test]
    async fn start_primary_rename_dispatches_and_shows_requested() {
        let m = machine_with_rename(
            vec![
                row_id("old.example", true, 1),
                row_id("new.example", false, 2),
            ],
            None,
        );
        m.hydrate().await.unwrap();
        assert!(m.snapshot().active_rename.is_none());

        m.dispatch(LocalDomainAction::StartPrimaryRename {
            new_primary_domain_id: vec![2u8; 16],
            grace_days: None,
        })
        .await
        .unwrap();

        let r = m.snapshot().active_rename.expect("rename now in flight");
        assert_eq!(r.state, "requested");
        assert_eq!(r.new_primary_domain, "new.example");
        assert_eq!(r.old_primary_domain, "old.example");
    }

    #[tokio::test]
    async fn complete_ready_rename_clears_active() {
        let m = machine_with_rename(
            vec![
                row_id("old.example", true, 1),
                row_id("new.example", false, 2),
            ],
            Some(rename_row("ready_to_complete", 1, 2)),
        );
        m.hydrate().await.unwrap();
        let r = m.snapshot().active_rename.unwrap();
        assert!(r.can_complete, "ready → complete offered");

        m.dispatch(LocalDomainAction::CompletePrimaryRename {
            rename_id: r.rename_id.clone(),
            force: false,
        })
        .await
        .unwrap();
        // Terminal → no longer the active rename.
        assert!(m.snapshot().active_rename.is_none());
    }

    #[tokio::test]
    async fn complete_grace_without_force_errors_and_keeps_rename() {
        let m = machine_with_rename(
            vec![
                row_id("old.example", true, 1),
                row_id("new.example", false, 2),
            ],
            Some(rename_row("grace", 1, 2)),
        );
        m.hydrate().await.unwrap();
        let r = m.snapshot().active_rename.unwrap();
        assert!(!r.can_complete, "grace: not without force");
        assert!(r.can_force_complete);

        let err = m
            .dispatch(LocalDomainAction::CompletePrimaryRename {
                rename_id: r.rename_id.clone(),
                force: false,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)));
        let snap = m.snapshot();
        assert!(
            snap.error
                .as_deref()
                .unwrap()
                .contains("grace_period_not_expired"),
            "error: {:?}",
            snap.error
        );
        // The error path does not refresh, so the rename is still shown.
        assert!(snap.active_rename.is_some(), "rename not completed");
        assert_eq!(snap.status, LocalDomainStatus::Idle);
    }

    #[tokio::test]
    async fn abort_clears_active_rename() {
        let m = machine_with_rename(
            vec![
                row_id("old.example", true, 1),
                row_id("new.example", false, 2),
            ],
            Some(rename_row("cert_issuance", 1, 2)),
        );
        m.hydrate().await.unwrap();
        let r = m.snapshot().active_rename.unwrap();
        m.dispatch(LocalDomainAction::AbortPrimaryRename {
            rename_id: r.rename_id.clone(),
            reason: Some("changed my mind".to_string()),
        })
        .await
        .unwrap();
        assert!(m.snapshot().active_rename.is_none());
    }

    #[tokio::test]
    async fn rename_status_error_degrades_to_none_without_failing_refresh() {
        // A failed status fetch (a transient error); the
        // domain list must still render.
        let nest = FakeNest {
            active: StdMutex::new(vec![row_id("old.example", true, 1)]),
            rename_status_err: StdMutex::new(true),
            ..Default::default()
        };
        let m = LocalDomainMachine::new(Arc::new(nest));
        m.hydrate().await.unwrap(); // must NOT error
        let snap = m.snapshot();
        assert_eq!(snap.active.len(), 1, "domains still listed");
        assert!(snap.active_rename.is_none(), "rename degraded to none");
        assert_eq!(snap.status, LocalDomainStatus::Idle);
        assert!(snap.error.is_none(), "degradation is silent, not an error");
    }
}
