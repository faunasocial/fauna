//! The nest-admin shell — `docs/goal/behavior/admin.md` § Navigation model;
//! ui.yaml `navigation.gated_tabs` (`admin-tab`) + `navigation.admin_pages`.
//!
//! The tui twin of linux's admin surface (`apps/fauna-linux/src/views/admin.rs`,
//! the reference). Admin is ONE gated top-level tab ([`Page::Admin`], the
//! `admin-tab` sidebar row) that opens a shell mirroring tui's Settings shell
//! (`crate::settings`) exactly (priority #3 — same concepts everywhere): a
//! top-level page reached via its own tab, holding a **sub-page set** switched by
//! an [`AdminPage`] sub-nav. The `admin-tab` row is gated on the shared
//! `fauna.account.am_i_admin` check, which is computed **once in shared Rust and
//! only rendered here** (`admin.md` § Where logic lives / § Don't do these — "Don't
//! compute admin gating per-app"); it is fail-closed, so a non-admin
//! or any RPC error never reveals the shell.
//!
//! **The sub-nav** ([`AdminPage`] + [`route_subpage`]) grew exactly as
//! `settings::SubPage` grew: the two-element nav `{"view":"admin","id":"<page>"}`
//! (the "admin-style two-element nav" `automation::apply_nav` names) routes the
//! page, and a rail of the built sub-pages ([`admin_nav_rail`]) is painted atop
//! every admin page so a human can switch between them — the tui adaptation of the
//! GUI's vertical sidebar-swap switcher (`admin.md` § Navigation model). The
//! separate `admin-nav-back` on every page **leaves the shell** to the primary view.
//!
//! **Scope built so far.** The shell + Dashboard (`admin-dashboard`) + the three
//! deployment-wide DAV-enable sub-pages: Calendar (`admin-calendar`, CalDAV enable
//! and port), Contacts (`admin-contacts`, CardDAV enable), and Files
//! (`admin-files`, WebDAV enable) — `admin.md` §§ 8/Contacts/Files. Each DAV page is a dumb
//! renderer of a shared focused machine in `fauna-client-mail-settings`
//! (`CaldavPolicyMachine` / `CarddavPolicyMachine` / `WebdavPolicyMachine`); no
//! nest or shared-Rust work — the machines + write kinds already existed. The
//! remaining sub-pages (Users, Tiers, Nest, Mail, Aliases — `admin.md`
//! § navigation.admin_pages) grow the same way: one [`AdminPage`] variant + its
//! `elements`/load arm each.

mod aliases;
mod bridges;
mod calendar;
mod contacts;
mod custody_hosting;
mod dashboard;
mod dns;
/// The local-domain fixtures, for the `nest_retire` page's admin-entry tests.
#[cfg(test)]
pub(crate) use dns::tests::{domain_row, domains_snapshot};
mod files;
mod logs;
mod mail;
mod nest;
mod settings;
mod users;
mod web;

use std::collections::BTreeMap;
use std::sync::Arc;

use fauna_client::{NestClient, SetupStatusReply, SetupStatusRequest};
use fauna_client_account::AccountClient;
use fauna_client_accounts::RegistryLaunchPersistence;
use fauna_client_admin::{
    AGE_BAND_NOT_SET_VALUE, AdminClient, AdminInviteCode, AdminInviteRequest, AdminUser,
    IssuerForcedArm, IssuerForcedConfirmView, IssuerKeyView, RegistrationMode,
    SeedRotationConfirmView, age_band_from_option_value, claimed_age_band_option,
};
use fauna_client_capabilities::custody_hosting::{
    AdminHostingSnapshot, load_admin_hosting_snapshot,
};
use fauna_client_dns::{DnsAction, DnsManagementMachine, DnsSnapshot};
use fauna_client_mail_settings::admin_policy::{
    MailPolicyAction, MailPolicyMachine, MailPolicySnapshot,
};
use fauna_client_mail_settings::bridge_approval::{
    BridgeApprovalAction, BridgeApprovalMachine, BridgeApprovalSnapshot,
};
use fauna_client_mail_settings::caldav_policy::{
    CaldavPolicyAction, CaldavPolicyMachine, CaldavPolicySnapshot,
};
use fauna_client_mail_settings::carddav_policy::{
    CarddavPolicyAction, CarddavPolicyMachine, CarddavPolicySnapshot,
};
use fauna_client_mail_settings::forwarders::{
    ForwarderAction, ForwarderMachine, ForwardersSnapshot,
};
use fauna_client_mail_settings::local_domains::{
    DEFAULT_CERT_MODE, LocalDomainAction, LocalDomainMachine, LocalDomainsSnapshot, RoleAddressKind,
};
use fauna_client_mail_settings::webdav_policy::{
    WebdavPolicyAction, WebdavPolicyMachine, WebdavPolicySnapshot,
};
use fauna_client_moderation::{
    ModerationClient, TakedownContentType, TakedownForm, TakedownFormView, takedown_form_view,
    takedown_verdict,
};
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_client_web::WebClient;
use fauna_i18n::strings::admin as t;
use fauna_launch_machine::mint_and_persist_pending_factory_reset;
use fauna_onboarding_machine::{AdminNatModeMachine, NatModeSnapshot, NodeMode};
use fauna_protocol::RpcRequester;
use fauna_protocol::admin::{
    AdminMembershipTier, AdminMembershipTierSetRequest, AdminTier, AdminTierCreateRequest,
    AdminTierUpdateRequest, DEFAULT_LAPSE_TIER,
};
use fauna_protocol::age::AgeBand;
use fauna_sync_engine::account_runtime::AccountStoreHandle;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, PageOutcome, UiMessage};
use crate::element::{Element, Gesture};
use crate::pages::Page;

pub use dashboard::DashboardSnapshot;

/// Which admin sub-page the shell currently shows. Mirrors `settings::SubPage`
/// (`admin.md` § Navigation model — the internal switcher). Grows one variant per
/// sub-page as they land; [`Self::BUILT`] is the nav order the rail paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdminPage {
    /// The landing (`admin-dashboard`): stat cards, re-read on entry.
    #[default]
    Dashboard,
    /// `admin-users` — the user-administration hub: the user list (tier change +
    /// eviction/suspension/restore + serving audit + pagination), the registration
    /// posture, invite-code minting, and pending invite-request approval, all framed
    /// around assigning a *tier* (`admin.md` § 2 Users).
    Users,
    /// `admin-settings` — the Tiers page: tier *definitions* (the `AdminTier`
    /// caps), edited in place (`admin.md` § 3 Settings).
    Settings,
    /// `admin-calendar` — the deployment-wide CalDAV-enable toggle + CalDAV port.
    Calendar,
    /// `admin-contacts` — the deployment-wide CardDAV-enable toggle.
    Contacts,
    /// `admin-files` — the deployment-wide WebDAV-enable toggle.
    Files,
    /// `admin-nest` — nest-wide settings + danger zone: the admin pairing
    /// toggle, the client-facing serving port, the NAT-mode radios, the
    /// host-OS-maintenance indicator, and the Factory Reset (`admin.md` § N Nest).
    Nest,
    /// `admin-mail` — the flat mail-policy form: the mail-enable + auto-enable
    /// toggles and the six full-PUT policy groups (spam / auth / submission /
    /// imap / outbound / alias), a dumb renderer of the shared `MailPolicyMachine`
    /// (`admin.md` § 6 Mail).
    Mail,
    /// `admin-aliases` — the admin external-forwarders page: an address on a
    /// hosted local domain forwarding to an external destination, no local
    /// mailbox (`admin.md` § 4 Aliases / `mail-aliases.md` § Kind 7).
    Aliases,
    /// `admin-web` — the deployment apex-actor designation: whose `web` content
    /// serves at `https://<domain>/` (`admin.md` § 7 Web).
    ///
    /// A **contextual detail page**: ui.yaml's `navigation.admin_pages` does not
    /// list it (it lists the nine flat pages), but linux — the reference — still
    /// carries it as a nav-rail row, so a human can reach it. [`Self::BUILT`]
    /// follows linux.
    Web,
    /// `admin-bridges-pending` — pending-bridge approval + the approved-bridge
    /// roster with service-user key rotation (`admin.md` §§ Bridge display
    /// naming / Approved-bridges roster). A **contextual detail page** like
    /// [`Self::Web`]: absent from ui.yaml's `navigation.admin_pages`, present on
    /// linux's nav rail, so it gets a rail row here too.
    Bridges,
    /// `admin-dns` — the unified DNS page: the per-domain record matrix with live
    /// public-DNS red/green verdicts, domain add/remove/restore, the client-held
    /// DNS-provider credential store, the per-domain managed/manual mode, the
    /// per-domain catch-all + role-address pickers, and the primary-domain rename
    /// wizard (`dns-management.md` § App surface; `admin.md` § 4 for the
    /// catch-all's home). A **contextual detail page** like [`Self::Web`] /
    /// [`Self::Bridges`] / [`Self::Logs`]: absent from ui.yaml's
    /// `navigation.admin_pages`, present on linux's nav rail, so it gets a rail row.
    Dns,
    /// `admin-custody-hosting` — the nest-wide custody-hosting registry: every
    /// row an account holder here armed, with the host that armed it, the
    /// address the pump dials, the budget, the metered hold, and a remove. A
    /// **contextual detail page** like [`Self::Web`] / [`Self::Bridges`] /
    /// [`Self::Dns`] / [`Self::Logs`]: absent from ui.yaml's
    /// `navigation.admin_pages`, so it earns its reachability from the nav rail.
    ///
    /// It is the recoverability half of *no client-causable unrecoverable nest
    /// state*: the per-caller `fauna.custody.hosting.list` is host-scoped, so
    /// without this page an admin cannot see — let alone drop — a row someone
    /// planted, and the only remedy would be `sqlite3` plus `rm -rf`.
    CustodyHosting,
    /// `admin-logs` — the nest's `fauna-log` ring over `fauna.admin.logs`,
    /// rendered with the same widget as the client's own Settings → Logs page
    /// (`observability.md` § 3 Surfaces). A **contextual detail page** like
    /// [`Self::Web`] / [`Self::Bridges`]: absent from ui.yaml's
    /// `navigation.admin_pages`, present on linux's nav rail (its last row), so
    /// it gets a rail row here too.
    Logs,
}

impl AdminPage {
    /// The admin sub-pages this client has built, in ui.yaml `navigation.admin_pages`
    /// order, with the **contextual detail pages** slotted where linux's own rail
    /// puts them (`views/admin.rs::admin_nav` — after `admin-files`, before
    /// `admin-aliases`). The rail paints one row per entry; a new sub-page joins by
    /// adding its variant here, and that is what makes it reachable by a human — a
    /// page routable only by the e2e's nav patch ships unreachable (the
    /// `mail-aliases` miss).
    pub(crate) const BUILT: &'static [AdminPage] = &[
        AdminPage::Dashboard,
        AdminPage::Users,
        AdminPage::Settings,
        AdminPage::Nest,
        AdminPage::Mail,
        AdminPage::Calendar,
        AdminPage::Contacts,
        AdminPage::Files,
        AdminPage::Web,
        AdminPage::Bridges,
        // linux's own rail order puts `admin-dns` between the pending-bridge page
        // and Aliases (`views/admin.rs::admin_nav`), so tui slots it there too.
        AdminPage::Dns,
        AdminPage::Aliases,
        // No linux rail to follow here — tui is the lead app for this page — so
        // it slots beside the other nest-wide registries, before Logs.
        AdminPage::CustodyHosting,
        AdminPage::Logs,
    ];

    /// The rail row's label — each sub-page's own title.
    fn rail_label(self) -> &'static str {
        match self {
            AdminPage::Dashboard => t::dashboard::TITLE,
            AdminPage::Users => t::users_page::TITLE,
            AdminPage::Calendar => t::calendar_page::TITLE,
            AdminPage::Contacts => t::contacts_page::TITLE,
            AdminPage::Files => t::files_page::TITLE,
            AdminPage::Nest => t::nest_page::TITLE,
            AdminPage::Mail => t::mail_page::TITLE,
            AdminPage::Aliases => t::aliases_page::TITLE,
            AdminPage::Settings => t::settings_page::TITLE,
            AdminPage::Web => t::web_page::TITLE,
            AdminPage::Bridges => t::bridges_pending::TITLE,
            AdminPage::Dns => t::dns::TITLE,
            AdminPage::CustodyHosting => t::custody_hosting::TITLE,
            AdminPage::Logs => t::logs_page::TITLE,
        }
    }

    /// The ui.yaml page key this sub-page renders — the key of its rail row's
    /// `admin-nav-row[<key>]` (ui.yaml `navigation.sub_page_nav_rows`).
    pub(crate) fn page_key(self) -> &'static str {
        match self {
            AdminPage::Dashboard => "admin-dashboard",
            AdminPage::Users => "admin-users",
            AdminPage::Settings => "admin-settings",
            AdminPage::Calendar => "admin-calendar",
            AdminPage::Contacts => "admin-contacts",
            AdminPage::Files => "admin-files",
            AdminPage::Nest => "admin-nest",
            AdminPage::Mail => "admin-mail",
            AdminPage::Aliases => "admin-aliases",
            AdminPage::Web => "admin-web",
            AdminPage::Dns => "admin-dns",
            AdminPage::CustodyHosting => "admin-custody-hosting",
            AdminPage::Logs => "admin-logs",
            AdminPage::Bridges => "admin-bridges-pending",
        }
    }
}

/// An editable field on an admin sub-page. Nests under [`crate::element::Field`]
/// exhaustively, the same as every other page (`tui.md` § The page-module contract
/// — field ownership carried on the type itself).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AdminField {
    /// `admin-calendar-caldav-port-input` — the CalDAV listener port draft.
    CaldavPort,
    /// `admin-nest-serving-port-input` — the client-facing API serving-port draft.
    ServingPort,
    /// `admin-nest-region-input` — the declared-region draft (`admin.md` § N Nest
    /// → Declared region).
    Region,
    /// `admin-nest-takedown-content-id-input` — the legal-takedown console's
    /// content-id draft (`moderation.md` § Legal takedown → Invocation surface).
    TakedownContentId,
    /// `admin-nest-takedown-reference-input` — the legal-reference draft
    /// (required for a takedown; the optional overturn note on restore).
    TakedownReference,
    /// `admin-aliases-forwarder-add-pattern-input` — the new forwarder's local-part.
    ForwarderPattern,
    /// `admin-aliases-forwarder-add-target-input` — the new forwarder's external target.
    ForwarderTarget,
    /// `admin-settings-tier-cap-*` — one editable raw-i64 cap on the `row`-th tier
    /// definition (scoped under `admin-settings-tier-item[row]`).
    TierCap { row: usize, cap: TierCap },
    /// `admin-settings-tier-add-name-input` — the new tier's name draft.
    TierAddName,
    /// `admin-settings-tier-cap-*` scoped under `admin-settings-tier-add-section` —
    /// one raw-i64 cap draft of the tier being defined.
    TierAddCap(TierCap),
    /// One `admin-mail-*` numeric or list text input (`admin.md` § 6). The draft
    /// String lives in [`MailDrafts`]; [`MailField`] names which one.
    Mail(MailField),
    /// `admin-users-max-free-users-input` — the free-tier ceiling draft (blank = no
    /// cap; `admin.md` § 2 Registration).
    MaxFreeUsers,
    /// `admin-settings-max-uses-input` — the minted invite code's max-uses draft
    /// (`admin.md` § 2 Invite).
    InviteMaxUses,
    /// `admin-users-admit-actor-input` — the actor id (64 hex chars) the Admit
    /// section admits directly (`public-mode.md` § Registration & Identity).
    AdmitActor,
    /// `admin-users-admit-handle-input` — the handle the admitted actor gets;
    /// blank admits handle-less (`public-mode.md` § A handle-less account).
    AdmitHandle,
    /// `invite-request-row-deny-reason-field[row]` — the optional deny reason for the
    /// `row`-th pending request (`admin.md` § 2 Pending requests).
    RequestDenyReason { row: usize },
    /// `admin-dns-add-domain-input` — the new local mail domain's name draft
    /// (`dns-management.md` § App surface — this page is the single
    /// domain-management surface).
    DnsAddDomain,
    /// One field of the write-only add-credential form, keyed by the **raw**
    /// `providers.yaml` field id (Cloudflare `api-token`, Porkbun `api-key` /
    /// `secret-api-key`) — which is also the element id every app tags it with
    /// (linux `views/admin.rs::rebuild_credential_fields`), so the driver types
    /// into `api-token` directly. Never a prefixed id.
    DnsCredential(String),
    /// `admin-dns-rename-grace-days-input` — the rename wizard's optional
    /// grace-window override in days (blank ⇒ the nest default of 7).
    DnsRenameGraceDays,
    /// `admin-dns-rename-extend-days-input` — how many days the in-flight rename's
    /// grace window is extended by.
    DnsRenameExtendDays,
    /// `feature-policy-editor-cell-input[cell]` on the admin host — one slot of
    /// the open editor's draft, owned by the shared `PolicyEditor`.
    FeatureLimitCell { cell: usize },
}

/// Which cap of an `AdminTier` an `admin-settings-tier-cap-*` input edits — the
/// five raw-i64 caps `fauna.admin.tiers.update` overwrites (`admin.md` § 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TierCap {
    /// `admin-settings-tier-cap-inbox` — `max_inbox_bytes` (raw bytes).
    Inbox,
    /// `admin-settings-tier-cap-storage` — `max_storage_bytes` (raw bytes).
    Storage,
    /// `admin-settings-tier-cap-devices` — `max_devices` (count).
    Devices,
    /// `admin-settings-tier-cap-blob-size` — `max_blob_size` (raw bytes).
    BlobSize,
    /// `admin-settings-tier-cap-feeds` — `max_feeds` (count).
    Feeds,
}

impl TierCap {
    /// All five caps, in the order the rows and the add form paint them.
    pub const ALL: [TierCap; 5] = [
        TierCap::Inbox,
        TierCap::Storage,
        TierCap::Devices,
        TierCap::BlobSize,
        TierCap::Feeds,
    ];
}

/// The editable cap drafts for one `admin-settings-tier-item` row — raw-i64
/// strings, re-seeded from the persisted `AdminTier` on every `tiers.list` (the
/// field mirrors nest state, the DAV-port re-seed shape), so a row always shows
/// the current caps until the human types over one.
#[derive(Debug, Clone, Default)]
pub struct TierCapDrafts {
    pub inbox: String,
    pub storage: String,
    pub devices: String,
    pub blob_size: String,
    pub feeds: String,
}

impl TierCapDrafts {
    /// Seed the five drafts from a persisted tier's caps.
    fn from_tier(tier: &AdminTier) -> Self {
        Self {
            inbox: tier.max_inbox_bytes.to_string(),
            storage: tier.max_storage_bytes.to_string(),
            devices: tier.max_devices.to_string(),
            blob_size: tier.max_blob_size.to_string(),
            feeds: tier.max_feeds.to_string(),
        }
    }

    fn get(&self, cap: TierCap) -> &str {
        match cap {
            TierCap::Inbox => &self.inbox,
            TierCap::Storage => &self.storage,
            TierCap::Devices => &self.devices,
            TierCap::BlobSize => &self.blob_size,
            TierCap::Feeds => &self.feeds,
        }
    }

    fn set(&mut self, cap: TierCap, value: String) {
        match cap {
            TierCap::Inbox => self.inbox = value,
            TierCap::Storage => self.storage = value,
            TierCap::Devices => self.devices = value,
            TierCap::BlobSize => self.blob_size = value,
            TierCap::Feeds => self.feeds = value,
        }
    }
}

/// One `admin-settings-membership-item` row's editable drafts (monetization.md
/// § Pillar 4) — the subscription-tier-name select (display + fix-up only, "Save
/// reads the current selection directly" — linux's `build_membership_row`
/// comment) + the two quota-tier selects, re-seeded from the persisted
/// designation (or the documented defaults — `""` for admit, `DEFAULT_LAPSE_TIER`
/// for lapse) on every membership fold, parallel to
/// [`AdminState::own_membership_tier_names`].
#[derive(Debug, Clone, Default)]
pub struct MembershipRowDraft {
    pub tier_name: String,
    pub admin_tier: String,
    pub lapse_tier: String,
}

impl MembershipRowDraft {
    /// Seed a row's drafts: `own_tier_name` is this row's subscription tier
    /// (the row set, never edited away from its position); `existing` is the
    /// persisted designation for that tier, if any.
    fn seed(own_tier_name: &str, existing: Option<&AdminMembershipTier>) -> Self {
        Self {
            tier_name: own_tier_name.to_string(),
            admin_tier: existing.map(|m| m.admin_tier.clone()).unwrap_or_default(),
            lapse_tier: existing
                .map(|m| m.lapse_tier.clone())
                .unwrap_or_else(|| DEFAULT_LAPSE_TIER.to_string()),
        }
    }
}

/// One editable `admin-mail-*` **text input** — a numeric field or a
/// one-value-per-line list (`admin.md` § 6). Carried on [`AdminField::Mail`] so
/// field ownership stays on the type (the page-module contract). Each maps to a
/// `String` draft on [`MailDrafts`]; the in-group toggles are [`MailToggle`] and
/// the two dropdowns their own `Action` variants, so this enum is text-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailField {
    // Spam / inbound perimeter (`put_spam_policy`).
    SpamJunk,
    SpamReject,
    SpamDnsbl,
    SpamGreylistDelay,
    SpamMaxConnPerMin,
    SpamMaxMessageBytes,
    SpamBayesianWeight,
    SpamBayesianMinSamples,
    SpamBayesianFullConfidence,
    SpamTrainingRetention,
    SpamUnlistedPenalty,
    // Inbound authentication enforcement (`put_auth_policy`).
    AuthMaxFailures,
    AuthMaxConnPerIp,
    // Submission quotas (`put_submission_policy`).
    SubmissionMaxPerDay,
    SubmissionMaxRecipients,
    // IMAP server policy (`put_imap_policy`).
    ImapIdleTimeout,
    ImapTombstoneRetention,
    ImapBodystructureCache,
    ImapStorageBytes,
    ImapMessageCount,
    // Outbound delivery (`put_outbound_policy`).
    OutboundRetrySchedule,
    OutboundPermfailTimeout,
    OutboundDelayWarning,
    OutboundNdrRateLimit,
    OutboundTreat5xx,
    // Aliases (`put_alias_policy`).
    AliasExactMax,
    AliasReservedLocalParts,
}

/// One editable `admin-mail-*` **in-group toggle** — a `bool` gathered into its
/// group's full-PUT on Save (unlike the deployment-wide mail-enable / auto-enable
/// toggles, which dispatch immediately). Carried on [`Action::ToggleMail`]; the
/// read-only `postmaster_cc_bounces` switch is deliberately absent (project policy
/// never CC, so it is never editable — `admin.md` § 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailToggle {
    SpamRejectNoRdns,
    SpamGreylistEnabled,
    SpamHeloIdentityRequired,
    SpamRejectFcrdnsFail,
    AuthEnforceDmarc,
    AuthEnforceDmarcQuarantine,
    AuthEnforceSpfHardfail,
    AuthEnforceDkim,
    AuthLogOnly,
    OutboundSuppressNdrSpf,
    OutboundSuppressNdrDmarc,
    OutboundTlsrptSend,
    OutboundIpv6,
    AliasSubaddressing,
    AliasWildcardPrefix,
}

/// The six mail-policy save groups' editable drafts (`admin-mail`, `admin.md`
/// § 6). Each numeric/list input is a `String` draft (a mid-edit value can be
/// empty or partial, which a typed `u32` can't hold) and each in-group toggle a
/// `bool`; the two dropdowns keep their wire value. Re-seeded from the persisted
/// [`MailPolicySnapshot`] on every fold (the DAV-port re-seed shape) so a group
/// shows persisted state until the human edits it, and each group's Save gathers
/// its drafts into the full-PUT `*View` (unparsed numerics fall back to the
/// persisted value, mirroring linux's `gather_*`). The paint + the seed/text/
/// toggle/gather impl live in [`mail`]; these are the field-typed data the shell
/// holds.
#[derive(Debug, Clone, Default)]
pub struct MailDrafts {
    pub spam: SpamDraft,
    pub auth: AuthDraft,
    pub submission: SubmissionDraft,
    pub imap: ImapDraft,
    pub outbound: OutboundDraft,
    pub alias: AliasDraft,
}

/// The Spam / inbound-perimeter group's drafts (`SpamPolicyView`, `put_spam_policy`).
#[derive(Debug, Clone, Default)]
pub struct SpamDraft {
    pub junk: String,
    pub reject: String,
    /// One blocklist host per line.
    pub dnsbl: String,
    pub reject_no_rdns: bool,
    pub greylist_enabled: bool,
    pub greylist_delay: String,
    pub max_conn_per_min: String,
    /// Wire value: `off` / `score_signal` / `enforce`.
    pub fcrdns_mode: String,
    pub helo_identity_required: bool,
    pub reject_fcrdns_fail: bool,
    pub max_message_bytes: String,
    pub bayesian_weight: String,
    pub bayesian_min_samples: String,
    pub bayesian_full_confidence: String,
    pub training_retention: String,
    pub unlisted_penalty: String,
}

/// The inbound authentication-enforcement group's drafts (`AuthPolicyView`).
#[derive(Debug, Clone, Default)]
pub struct AuthDraft {
    pub enforce_dmarc: bool,
    pub enforce_dmarc_quarantine: bool,
    pub enforce_spf_hardfail: bool,
    pub enforce_dkim: bool,
    pub log_only: bool,
    pub max_failures: String,
    pub max_conn_per_ip: String,
}

/// The submission-quota group's drafts (`SubmissionPolicyView`).
#[derive(Debug, Clone, Default)]
pub struct SubmissionDraft {
    pub max_per_day: String,
    pub max_recipients: String,
}

/// The IMAP-server-policy group's drafts (`ImapPolicyView`).
#[derive(Debug, Clone, Default)]
pub struct ImapDraft {
    pub idle_timeout: String,
    pub tombstone_retention: String,
    /// Wire value: `forbidden` / `allowed`.
    pub delete_nonempty: String,
    pub bodystructure_cache: String,
    pub storage_bytes: String,
    pub message_count: String,
}

/// The outbound-delivery group's drafts (`OutboundPolicyView`). `postmaster_cc`
/// is not modelled — it is a read-only switch (project policy never CC).
#[derive(Debug, Clone, Default)]
pub struct OutboundDraft {
    /// One delay-seconds value per line.
    pub retry_schedule: String,
    pub permfail_timeout: String,
    pub delay_warning: String,
    pub ndr_rate_limit: String,
    pub suppress_ndr_spf: bool,
    pub suppress_ndr_dmarc: bool,
    pub tlsrpt_send: bool,
    pub ipv6: bool,
    /// One enhanced-status code per line.
    pub treat_5xx: String,
}

/// The alias-policy group's drafts (`AliasPolicyView`, `put_alias_policy` — the
/// dual-read group, hydrated from `get_alias_policy`).
#[derive(Debug, Clone, Default)]
pub struct AliasDraft {
    pub exact_max: String,
    /// One reserved local-part per line; empty clears the reservation.
    pub reserved_local_parts: String,
    pub subaddressing: bool,
    pub wildcard_prefix: bool,
}

/// The `admin-nest` page's reflective read — the pairing flag (from
/// `fauna.admin.services.list`) plus the serving-port + host-OS-maintenance
/// fields (from `fauna.setup.status`). Re-read on entry and after any nest-page
/// mutation; the NAT-mode leg lives on the shared machine, not here.
#[derive(Debug, Clone, Default)]
pub struct NestPageSnapshot {
    /// `admin-service-pairing-toggle`/`-status` — whether user-initiated nest
    /// pairing is enabled (`AdminServiceFlags.pairing`).
    pub pairing_enabled: bool,
    /// `admin-nest-serving-port-input` — the admin-set client-facing API port
    /// (`SetupStatusReply.serving_port`).
    pub serving_port: u16,
    /// Whether the nest is fronted by the SNI router — the serving-port control
    /// is inert on a router-fronted (domain) deploy (`SetupStatusReply.fronted_by_router`).
    pub fronted_by_router: bool,
    /// `nest-os-updates-count` — pending host security updates (0 on a nest with
    /// no host maintenance channel).
    pub os_security_updates_pending: u32,
    /// `nest-os-restart-now-button` gate — a host reboot is pending.
    pub os_reboot_pending: bool,
    /// `admin-nest-region-*` — the declared region folded for render by the
    /// shared `admin_region_view` (`admin.md` § N Nest → Declared region).
    ///
    /// Rides this snapshot rather than its own so it re-reads with every nest-page
    /// mutation, like the serving port: declaring a region is a write whose
    /// consequences (which document is in force, whether one was retired) are
    /// only visible in the *next* read.
    pub region: fauna_client_admin::AdminRegionView,
    /// `admin-nest-web-app-origin-*` — what this nest's `/app/` answers, folded
    /// by the shared `admin_web_app_origin_view` (`admin.md` § N Nest →
    /// *Web-app origin*). On this snapshot for the region's reason: it re-reads
    /// with every nest-page mutation. A nest that predates the choice answers
    /// the read with unknown-kind, which the fold words — never a failed read.
    pub web_app_origin: fauna_client_admin::AdminWebAppOriginView,
    /// `admin-nest-oauth-*` — the OAuth issuer's served key set, folded by the
    /// shared `issuer_key_view` (`authorization-server.md` § The issuer).
    ///
    /// Carried as its own three-state read rather than failing the whole
    /// reflect: the kind is newer than every other read here, and a failure
    /// reading it must still paint pairing, the port and the rest.
    pub oauth: OauthKeysRead,
    /// `admin-nest-feature-limits-*` — the admin tier's AUTHORED feature
    /// documents (`fauna.features.policy.get`), folded by the shared
    /// `fauna_client_features::authored_surface` (`admin.md` § N Nest → Feature
    /// limits). Its own three-state read for the `oauth` reason: the kind is
    /// newer than the rest of this page, and a failed read must not blank it.
    pub feature_limits: FeatureLimitsRead,
}

/// The admin tier's authored-document read, as the Feature limits section
/// paints it. Never collapsed into an empty list: "not read" and "couldn't
/// read" must not look like "this nest gates nothing".
#[derive(Debug, Clone, Default, PartialEq)]
pub enum FeatureLimitsRead {
    #[default]
    Unread,
    Ready(fauna_client_features::AuthoredSurface),
    /// The read failed — already worded.
    Failed(String),
}

/// The issuer key set's read, as the `admin-nest-oauth-*` section paints it.
///
/// Three honest states, never collapsed into an empty list: "we haven't asked"
/// and "we couldn't find out" must not read like "no keys", and the forced
/// arm's confirm can only name what it drops once the set has answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OauthKeysRead {
    /// Not read yet (the default — the reflect always sets one of the other two).
    #[default]
    Unread,
    /// The set answered; the shared fold decides every line.
    Ready(IssuerKeyView),
    /// The read failed — already worded, the section's reason line.
    Failed(String),
}

/// The `admin-users` hub's read data — the whole page in one snapshot
/// (`admin.md` § 2 Users: the four sections ride one snapshot). Re-read on entry
/// and after every mutation (offset-preserving); the drafts that mirror nest state
/// re-seed from it on each fold, the field-mirrors-nest-state discipline. Drives
/// **directly** off the shared `AdminClient` (no state machine — `fauna-client-admin`
/// is a thin one-method-per-kind client), so the shell (`super`) owns the reads.
#[derive(Debug, Clone, Default)]
pub struct UsersSnapshot {
    /// The current page of users (`users_list { limit: 50, offset }`).
    pub users: Vec<AdminUser>,
    /// Every account on the nest (`fauna_client_admin::users_list_all`) — what
    /// both guardian pickers offer and resolve against, never the page above
    /// (`admin.md` § 2 → *Which accounts a picker offers*).
    pub picker_users: Vec<AdminUser>,
    /// The unpaginated total (`AdminUsersListReply.total`) — drives `user-count-text`
    /// and the prev/next-page enable math.
    pub total: i64,
    /// The tier *names* (`tiers_list`) — the option set every tier picker offers
    /// (user-row change-tier, invite mint, request approve-at).
    pub tiers: Vec<String>,
    /// The existing invite codes (`invite_codes_list`) — the Invite section list.
    pub invite_codes: Vec<AdminInviteCode>,
    /// The pending (and decided) invite requests (`invite_requests_list`) — the
    /// Pending-requests section rows; `is_pending()` gates each row's controls.
    pub invite_requests: Vec<AdminInviteRequest>,
    /// The nest's registration posture, raw wire string (`fauna.setup.status`
    /// → `registration_mode`). `None`/an unrecognized value renders the read-only
    /// "unknown posture" label instead of the picker (never coerced — `admin.md`
    /// § 2 Registration).
    pub registration_mode: Option<String>,
    /// The free-tier ceiling (`SetupStatusReply.max_free_users`), `None` = no cap.
    pub max_free_users: Option<u64>,
    /// The age require-knob (`SetupStatusReply.age_verification_required` —
    /// "accept only signups carrying app age verification", default off;
    /// `family-safety.md` § The account age band D5+D6). Rendered as
    /// `admin-users-registration-age-verification-toggle`, saved by the
    /// section's save button only when its draft differs from this.
    pub age_verification_required: bool,
    /// Every account's pending actions (`fauna.admin.pending_actions.list`, all
    /// statuses; the section paints the still-`pending` ones) — the
    /// pending-admin-actions section's rows (`admin.md` § Pending admin actions).
    pub pending_actions: Vec<fauna_protocol::admin::AdminPendingActionSummary>,
}

/// The `admin-users` hub's editable/form state, held beside the read
/// [`UsersSnapshot`]. The drafts that mirror nest state (registration mode +
/// ceiling, the per-request approve-at drafts) re-seed from the snapshot on every
/// fold; the invite-form drafts are transient form state (not re-seeded).
#[derive(Debug, Clone, Default)]
pub struct UsersState {
    /// The last read snapshot, `None` until the nav-edge fetch lands (the stat-card
    /// `None`-while-empty shape — the rows don't register until real data arrives).
    pub snapshot: Option<UsersSnapshot>,
    /// The current pagination offset (`users_list { offset }`), preserved across
    /// mutations so an evict/tier-change stays on the same page.
    pub offset: i64,
    /// The page-scoped `admin-users-action-error` (all three sections' errors route
    /// here, NOT the app-wide `error-message` — `admin.md` § Errors). Painted only
    /// when set (the empty-doesn't-register discipline).
    pub action_error: Option<String>,
    /// `admin-users-registration-mode-select` — the picked mode wire value, re-seeded
    /// from the persisted posture on every fold (the field-mirrors-nest-state re-seed).
    pub registration_mode_draft: String,
    /// `admin-users-max-free-users-input` — the free-tier ceiling draft (blank = no
    /// cap), re-seeded from the persisted value on every fold.
    pub max_free_users_input: String,
    /// `admin-users-registration-age-verification-toggle` — the require-knob
    /// draft, re-seeded from [`UsersSnapshot::age_verification_required`] on
    /// every fold; the section save dispatches
    /// `set_age_verification_required` only when it differs from the snapshot.
    pub age_verification_draft: bool,
    /// Whether the invite create form (`admin-settings-invite-create-form`) is shown
    /// — toggled by `create-invite-code-btn` / `admin-settings-invite-cancel-button`.
    pub invite_form_open: bool,
    /// `admin-settings-tier-select` — the tier the minted code admits at (draft).
    pub invite_tier_draft: String,
    /// `admin-settings-max-uses-input` — the minted code's max-uses draft (default "1").
    pub invite_uses_input: String,
    /// `admin-users-invite-guardian-select` — the picked guardian's display LABEL
    /// for a supervised admission ([`GUARDIAN_NONE_VALUE`] / `""` = none;
    /// family-safety.md; sourced from non-suspended users). Resolved to the wire
    /// actor id at mint time by [`resolve_guardian`].
    pub invite_guardian_draft: String,
    /// `admin-users-invite-age-band-select` — the minted code's band draft, an
    /// option VALUE ([`AGE_BAND_NOT_SET_VALUE`] or a wire token;
    /// `family-safety.md` § App surface → *Age-band surfaces*). Transient form
    /// state like the guardian draft; reset to not-set whenever the guardian is
    /// cleared (a band presupposes a guardian — the nest refuses one without).
    pub invite_age_band_draft: String,
    /// The freshly minted invite-code token (`AdminInviteCodeCreateReply.code`),
    /// revealed copyable via `admin-users-invite-code-copy-btn` — `None` until a mint.
    pub minted_code: Option<String>,
    /// `admin-users-admit-actor-input` — the actor id (64 hex chars) the Admit
    /// section admits directly. Transient form state like the invite drafts —
    /// a re-read must not wipe a half-filled form, so `reseed` leaves it alone.
    pub admit_actor_input: String,
    /// `admin-users-admit-handle-input` — the handle the admitted actor gets;
    /// blank admits handle-less on purpose (`public-mode.md` § A handle-less
    /// account). Transient form state, untouched by `reseed`.
    pub admit_handle_input: String,
    /// `admin-users-admit-tier-select` — the admitted account's tier draft
    /// (seeded to "free" like the invite tier when empty).
    pub admit_tier_draft: String,
    /// Per-pending-request approve-at tier drafts (`invite-request-row-tier-select[i]`),
    /// parallel to `snapshot.invite_requests`, re-seeded on every fold.
    pub request_tier_drafts: Vec<String>,
    /// Per-pending-request guardian drafts (`invite-request-row-guardian-select[i]`)
    /// — display LABELs like [`Self::invite_guardian_draft`]
    /// ([`GUARDIAN_NONE_VALUE`] / `""` = none), parallel to `snapshot.invite_requests`.
    pub request_guardian_drafts: Vec<String>,
    /// Per-pending-request band drafts (`invite-request-row-age-band-select[i]`)
    /// — option VALUES, parallel to `snapshot.invite_requests`, **seeded from
    /// the applicant's claimed band** when the request carries a nameable one
    /// (D5: the claim corroborates, the admitting adult decides — the
    /// override is silent to the applicant), else [`AGE_BAND_NOT_SET_VALUE`].
    pub request_age_band_drafts: Vec<String>,
    /// Per-pending-request deny-reason drafts (`invite-request-row-deny-reason-field[i]`),
    /// parallel to `snapshot.invite_requests`.
    pub request_deny_reasons: Vec<String>,
}

impl UsersState {
    /// Re-seed the drafts that mirror nest state from a fresh snapshot (called on
    /// every `UsersLoaded` fold — the field-mirrors-nest-state discipline the whole
    /// admin module follows):
    /// - the registration mode/ceiling drafts track the persisted posture (a mode
    ///   the client doesn't recognize seeds an empty draft — the section renders
    ///   read-only rather than offering a picker that could overwrite it);
    /// - the per-request approve-at drafts are re-aligned to the request list (a
    ///   default tier + no guardian + no reason), so `invite-request-row-*[i]`
    ///   always has a value at each row index;
    /// - the invite-form drafts are seeded only when empty (they are transient form
    ///   state, not a mirror — a re-read triggered by an unrelated mutation must not
    ///   wipe a half-filled create form).
    fn reseed(&mut self, snapshot: &UsersSnapshot) {
        // Registration posture (§ 2) — draft = the persisted wire value iff known.
        self.registration_mode_draft = snapshot
            .registration_mode
            .as_deref()
            .filter(|m| RegistrationMode::from_wire_str(m).is_some())
            .unwrap_or_default()
            .to_string();
        self.max_free_users_input = snapshot
            .max_free_users
            .map(|n| n.to_string())
            .unwrap_or_default();
        self.age_verification_draft = snapshot.age_verification_required;

        // Pending-request drafts (§ 1) — re-align to the request list.
        let default_tier = snapshot
            .tiers
            .iter()
            .find(|t| *t == "free")
            .or_else(|| snapshot.tiers.first())
            .cloned()
            .unwrap_or_else(|| "free".to_string());
        let n = snapshot.invite_requests.len();
        self.request_tier_drafts = vec![default_tier.clone(); n];
        self.request_guardian_drafts = vec![GUARDIAN_NONE_VALUE.to_string(); n];
        self.request_age_band_drafts = snapshot
            .invite_requests
            .iter()
            .map(|r| claimed_age_band_option(r.age_band.as_deref()))
            .collect();
        self.request_deny_reasons = vec![String::new(); n];

        // Admit-form tier draft — seeded like the invite tier; the actor/handle
        // inputs are transient form state and stay untouched (a re-read
        // triggered by an unrelated mutation must not wipe a half-filled form).
        if self.admit_tier_draft.is_empty() {
            self.admit_tier_draft = default_tier.clone();
        }

        // Invite-form drafts (§ 3) — seed sensible defaults only when untouched.
        if self.invite_tier_draft.is_empty() {
            self.invite_tier_draft = default_tier;
        }
        if self.invite_uses_input.is_empty() {
            self.invite_uses_input = "1".to_string();
        }
        if self.invite_guardian_draft.is_empty() {
            self.invite_guardian_draft = GUARDIAN_NONE_VALUE.to_string();
        }
        if self.invite_age_band_draft.is_empty() {
            self.invite_age_band_draft = AGE_BAND_NOT_SET_VALUE.to_string();
        }
    }
}

/// Whether a guardian-picker draft names a guardian at all — the gate the two
/// age-band selects follow (enabled only while a guardian is selected; the
/// nest refuses a band without a guardian designation, and the UI gates the
/// same way, `family-safety.md` § App surface → *Age-band surfaces*).
fn guardian_draft_names_someone(draft: &str) -> bool {
    !draft.is_empty() && draft != GUARDIAN_NONE_VALUE
}

/// The admin shell's state: the live client, the current sub-page, and each
/// sub-page's shared machine + last-fetched snapshot.
///
/// The admin surface holds no client-side cache (`admin.md` § Persistence: "every
/// page re-reads on entry/refresh"), so this is the transport handle, the shared
/// DAV machines (built once at [`init`]), and the last snapshots the UI paints.
#[derive(Default)]
pub struct AdminState {
    /// The live client, `None` pre-login. Built at the post-auth hook ([`init`])
    /// and dropped on sign-out, the notifications/contacts shape.
    nest: Option<Arc<NestClient>>,
    /// Which sub-page is showing. Reset to [`AdminPage::Dashboard`] on the nav
    /// edge into the shell (`App::apply`), the settings-shell-Root precedent.
    pub sub: AdminPage,
    /// The dashboard snapshot, `None` until the nav-edge fetch lands. The stat
    /// cards do not register while `None`, so the e2e's `wait_for` waits for real
    /// data rather than a blank-but-present card (the settings `quota-section`
    /// -while-`None` shape).
    pub dash: Option<DashboardSnapshot>,
    /// The `admin-calendar` machine (`fauna.bridges.set_caldav_enabled` / `_port`),
    /// built at [`init`]; drives the CalDAV-enable toggle + port.
    caldav: Option<Arc<CaldavPolicyMachine>>,
    /// Last `admin-calendar` snapshot the page painted.
    pub caldav_snapshot: Option<CaldavPolicySnapshot>,
    /// The `admin-contacts` machine (`fauna.bridges.set_carddav_enabled`).
    carddav: Option<Arc<CarddavPolicyMachine>>,
    /// Last `admin-contacts` snapshot.
    pub carddav_snapshot: Option<CarddavPolicySnapshot>,
    /// The `admin-files` machine (`fauna.bridges.set_webdav_enabled`).
    webdav: Option<Arc<WebdavPolicyMachine>>,
    /// Last `admin-files` snapshot.
    pub webdav_snapshot: Option<WebdavPolicySnapshot>,
    /// The `admin-mail` machine (the shared `MailPolicyMachine` — set_mail_enabled,
    /// set_auto_enable_mail_for_new_users, and the six `put_*_policy` groups), built
    /// at [`init`]; drives the whole flat mail-policy form.
    mail: Option<Arc<MailPolicyMachine>>,
    /// Last `admin-mail` snapshot the page painted (the persisted state the drafts
    /// re-seed from, the mail-enable/auto-enable toggles read, and the baseline
    /// publish result + error surface from).
    pub mail_snapshot: Option<MailPolicySnapshot>,
    /// The six mail-policy groups' editable drafts — re-seeded from the persisted
    /// `mail_snapshot` on every fold, gathered into each group's full-PUT on Save.
    pub mail_drafts: MailDrafts,
    /// Whether `admin-mail-health-warmup-reset-button` is armed — the two-click
    /// inline confirm (ui.yaml scopes no confirm id to it, so the arm lives on the
    /// button, which relabels to the confirm sentence). Disarmed by the confirm
    /// and by every mail fold, so it never survives a re-read.
    pub mail_warmup_reset_armed: bool,
    /// The `admin-calendar-caldav-port-input` editable draft — re-seeded from the
    /// persisted `caldav_port` on every snapshot (the field mirrors nest state),
    /// so it always shows the current port until the human types over it.
    pub caldav_port_input: String,
    /// The nest url — captured at [`init`] for the factory-reset record's
    /// `nest_url`. (The NAT machine takes url + secret at construction, but the
    /// secret is NOT retained here: the re-onboard reads it fresh from the
    /// credential store, `launch::route_wizard_entry`.)
    nest_url: String,
    /// The shared NAT-mode machine (`fauna-onboarding-machine`), built at [`init`]
    /// from `(nest_url, secret_hex)`. Its snapshot drives the `admin-nest-nat-mode-*`
    /// radios/save/status; `select` mutates it in place and `submit` signs the commit.
    nat: Option<Arc<AdminNatModeMachine>>,
    /// Last NAT-mode snapshot the page painted — re-read after `select`/`submit`
    /// and on the load edge (after `hydrate`). Held (rather than read live) so the
    /// fold owns the repaint, like the other snapshots.
    pub nat_snapshot: Option<NatModeSnapshot>,
    /// The `admin-nest` reflective snapshot (pairing + serving-port + os-maintenance),
    /// `None` until the nav-edge fetch lands (the stat-card `None`-while-empty shape).
    pub nest_snapshot: Option<NestPageSnapshot>,
    /// The `admin-nest-serving-port-input` editable draft — re-seeded from the
    /// persisted `serving_port` on every reflective snapshot.
    pub serving_port_input: String,
    /// The `admin-nest-region-input` editable draft — re-seeded from the declared
    /// region on every reflective snapshot, so it shows the current declaration
    /// until the admin types over it (the field-mirrors-nest-state discipline).
    /// Empty is the honest draft for a deployment that has declared nothing.
    pub region_input: String,
    /// The `admin-nest-web-app-origin-*-radio` selection — local until the save
    /// commits it, re-seeded from the nest's choice on every reflective snapshot
    /// (the field-mirrors-nest-state discipline). `None` marks neither radio.
    pub web_app_origin_draft: Option<fauna_client_admin::WebAppOrigin>,
    /// The shared `feature-policy-editor`, open over one member at the ADMIN
    /// tier (`None` while closed) — seeded from the authored document in the
    /// reflective snapshot, re-seeded after every write.
    pub feature_editor: Option<fauna_client_features::PolicyEditor>,
    /// `feature-policy-editor-status` — the last admin-tier write's verdict.
    pub feature_editor_status: Option<String>,
    /// Whether the inline Factory-Reset confirm (`admin-factory-reset-confirm-button`)
    /// is armed — set by `admin-factory-reset-button`, the sign-out-confirm shape.
    pub factory_reset_confirming: bool,
    /// The armed deployment-identity rotation confirm
    /// (`admin-nest-seed-rotate-confirm-button` + the roster rows), `None` while
    /// un-armed.
    ///
    /// Three states, deliberately distinguished — the ceremony's confirm surface
    /// owes the admin *the set that will inherit* (`box-recovery.md` §
    /// Deployment-seed rotation → *Ordering rule*), so "we don't know yet" and
    /// "we couldn't find out" must not both render as an empty list beside a
    /// live confirm button: [`SeedRotateConfirm::Loading`] while the roster read
    /// is in flight, [`SeedRotateConfirm::Failed`] when it could not answer, and
    /// [`SeedRotateConfirm::Ready`] carrying the shared fold.
    ///
    /// Captured at arm time and never re-derived while armed — the bridges
    /// rotate-confirm discipline ([`RotateConfirm`]): a roster that shifted
    /// between arming and confirming must not silently change what the admin
    /// already read.
    pub seed_rotate_confirm: Option<SeedRotateConfirm>,
    /// The rotation ceremony's own outcome line (`admin-nest-seed-rotate-status`),
    /// `None` until one has been attempted. Held rather than pushed onto
    /// `error-message` because the interesting outcomes are *successes with a
    /// caveat* (an unmarked predecessor), which an error line would misreport.
    pub seed_rotate_status: Option<String>,
    /// The legal-takedown console's drafts (`admin-nest-takedown-*` —
    /// `moderation.md` § Legal takedown → *Invocation surface*): the content-id
    /// and legal-reference inputs, the content-kind radio pair (`false` = post,
    /// the default), and the overturn/restore checkbox.
    pub takedown_content_id: String,
    pub takedown_reference: String,
    pub takedown_conversation: bool,
    pub takedown_restore: bool,
    /// The armed takedown confirm (`admin-nest-takedown-confirm-*`), `None`
    /// while un-armed. Captured at arm time — form AND folded view — and never
    /// re-derived while armed (the seed-rotate discipline): the confirm names
    /// exactly what will be dispatched, so an edit after arming must change
    /// neither the summary the admin already read nor the dispatch itself.
    pub takedown_confirm: Option<ArmedTakedown>,
    /// The console's outcome line (`admin-nest-takedown-status`), `None` until a
    /// dispatch has been attempted. Its own element, not `error-message` — the
    /// common outcome is a success whose consequences deserve words.
    pub takedown_status: Option<String>,
    /// The reports queue (`admin-nest-reports-section` — `moderation.md`
    /// § Where it lands): the open abuse reports, oldest first, as
    /// `fauna.moderation.abuse_report.queue` returns them. The takedown
    /// console's inbox — each row is a door to it, never a lever of its own.
    pub reports: Vec<fauna_protocol::moderation::AbuseReportQueueEntry>,
    /// Whether the queue read has resolved — the empty state paints only off
    /// this bit (`ui/README.md` § *List pages: loading is not empty*).
    pub reports_loaded: bool,
    /// The last resolve's outcome line, worded by the shared `resolve_verdict`.
    pub reports_status: Option<String>,
    /// The armed forced-rotation confirm (`admin-nest-oauth-confirm-*`), `None`
    /// while un-armed. Captured at arm time — the arm AND its folded cost — and
    /// never re-derived while armed (the seed-rotate discipline): a key-set
    /// re-read that landed between arming and confirming must not change the
    /// number of keys the admin already read would stop verifying.
    pub oauth_confirm: Option<ArmedOauthForced>,
    /// The issuer controls' outcome line (`admin-nest-oauth-status`), `None`
    /// until one has been used. Its own element, not `error-message`: every
    /// success here has consequences worth words (which key signs, which died).
    pub oauth_status: Option<String>,
    /// Whether an issuer control's call is in flight. Every one of the three
    /// kinds mints on the nest and is replay-forbidden, so while one runs the
    /// controls desensitize — a second press would chain a second rotation
    /// onto the first (the ordinary arm has no confirm to disarm).
    pub oauth_in_flight: bool,
    /// The `admin-aliases` machine (`fauna.bridges.{create,list,delete}_forwarder`),
    /// built at [`init`]; drives the external-forwarder list + add form.
    forwarders: Option<Arc<ForwarderMachine>>,
    /// Last `admin-aliases` snapshot the page painted (forwarder rows +
    /// hosted-domain picker options + the page-scoped action error).
    pub forwarders_snapshot: Option<ForwardersSnapshot>,
    /// `admin-aliases-forwarder-add-domain-select` — the picked hosted domain for
    /// the new forwarder. Re-seeded to the first hosted domain when the snapshot
    /// lands and the current pick is empty or no longer hosted (the picker always
    /// shows a real, submittable domain), and overridden by the human's select.
    pub forwarder_add_domain: String,
    /// `admin-aliases-forwarder-add-pattern-input` — the new forwarder's local-part draft.
    pub forwarder_add_pattern: String,
    /// `admin-aliases-forwarder-add-target-input` — the new forwarder's external-target draft.
    pub forwarder_add_target: String,
    /// The `admin-settings` tier *definitions* (`fauna.admin.tiers.list`), `None`
    /// until the nav-edge fetch lands (the stat-card `None`-while-empty shape). One
    /// `admin-settings-tier-item` row per tier.
    pub tiers: Option<Vec<AdminTier>>,
    /// The per-row editable cap drafts, parallel to [`Self::tiers`] — re-seeded
    /// from the persisted caps on every `tiers.list` (`admin.md` § 3 — raw i64).
    pub tier_cap_drafts: Vec<TierCapDrafts>,
    /// `admin-settings-tier-add-name-input` — the name of the tier being defined.
    pub tier_add_name: String,
    /// The add form's five raw-i64 cap drafts (`admin-settings-tier-cap-*` scoped
    /// under `admin-settings-tier-add-section`). Empty until typed — unlike a
    /// row's drafts there is no persisted tier to mirror.
    pub tier_add_caps: TierCapDrafts,
    /// The membership-designation section's row set: the admin's own
    /// subscription tier names (`fauna.subscriptions.tiers.list`), `None` until
    /// the nav-edge fetch lands. One `admin-settings-membership-item` row per
    /// entry (monetization.md § Pillar 4).
    pub own_membership_tier_names: Option<Vec<String>>,
    /// The last `fauna.admin.membership_tiers.list` reply — which of
    /// [`Self::own_membership_tier_names`] already carry a designation, and at
    /// what quota tiers.
    pub membership_tiers: Option<Vec<AdminMembershipTier>>,
    /// The per-row editable drafts, parallel to [`Self::own_membership_tier_names`]
    /// — re-seeded from [`Self::membership_tiers`] on every fold.
    pub membership_drafts: Vec<MembershipRowDraft>,
    /// The `admin-users` hub's read snapshot + editable/form state (`admin.md` § 2).
    pub users: UsersState,
    /// The `admin-web` apex designation + the pickable actor list, `None` until
    /// the sub-page's load lands (the stat-card `None`-while-empty shape).
    pub web_snapshot: Option<AdminWebSnapshot>,
    /// The `admin-custody-hosting` registry read, `None` until the sub-page's
    /// load lands. `None` is deliberately NOT an empty list: "nobody asked this
    /// nest to hold anything" is the one reassuring thing this page must never
    /// say before the nest has answered.
    pub custody_hosting: Option<AdminHostingSnapshot>,
    /// Which registry row's remove confirm is armed, as the `(host, grant)` pair
    /// the remove door is keyed by — carried whole rather than as a painted
    /// index, because a re-read re-orders rows (heaviest first) and an index
    /// would then name a different row than the admin armed.
    pub custody_hosting_confirm: Option<(String, Vec<u8>)>,
    /// The `admin-bridges-pending` machine (`fauna.bridges.{list_pending_bridges,
    /// approve,reject,list_service_users,revoke_service_user}`), built at
    /// [`init`]; drives both the pending cards and the approved roster.
    bridges: Option<Arc<BridgeApprovalMachine>>,
    /// Last `admin-bridges-pending` snapshot the page painted.
    pub bridges_snapshot: Option<BridgeApprovalSnapshot>,
    /// Which approved bridge's rotate confirm is armed, if any — the inline
    /// reveal `admin-bridges-rotate-confirm` (the factory-reset confirm shape).
    /// `None` = no dialog painted at all.
    pub rotate_confirm: Option<RotateConfirm>,
    /// The **nest's** log ring as `fauna.admin.logs` last delivered it,
    /// oldest-first, already converted to the render type by the shared
    /// `fauna_client_admin::log_entry_from_wire`. Distinct from this device's own
    /// ring (`fauna_log::snapshot`, what `settings::logs` paints) — the admin page
    /// never reads the local one. Empty until the sub-page's load lands, which
    /// paints the placeholder rather than a row.
    pub logs: Vec<fauna_log::LogEntry>,
    /// `log-level-filter` on `admin-logs` — the severity index into
    /// `settings::logs::log_filter_labels` (`0` = All). Its own field, not shared
    /// with the Settings page's `log_filter`: the two pages filter different
    /// sources, so narrowing one must not narrow the other.
    pub log_filter: u32,
    /// A failed `fauna.admin.logs` read, surfaced on the page's `error-message`.
    pub logs_error: Option<String>,
    /// The **persistent** `admin-dns` DNS-management machine, built once at
    /// [`init`] with the credential store (`build_dns_management_machine_with_
    /// credentials`). Persistent — not rebuilt per dispatch — because the live
    /// ACME order behind a suspended manual cert issuance lives inside it, so a
    /// fresh machine per action would drop `pending_cert` between
    /// `BeginManualIssueCert` and `CompleteManualIssueCert` (linux's
    /// `client.rs::dns_machine` learned this).
    dns: Option<Arc<DnsManagementMachine>>,
    /// Last `admin-dns` DNS snapshot the page painted — the per-domain record
    /// matrix + verdicts, the held credentials, and (Phase 2) the cert lifecycle.
    pub dns_snapshot: Option<DnsSnapshot>,
    /// The `admin-dns` local-domain machine — the *other* machine on this page
    /// (`admin.md:176`): domain add/remove/restore, the per-domain catch-all +
    /// role-address designations, and the primary-domain rename lifecycle.
    local_domains: Option<Arc<LocalDomainMachine>>,
    /// Last `admin-dns` local-domain snapshot the page painted. The domain **rows**
    /// come from here (`active` / `soft_deleted`); the DNS matrix is looked up per
    /// row by name from [`Self::dns_snapshot`], linux's `render_admin_dns` shape.
    pub local_domains_snapshot: Option<LocalDomainsSnapshot>,
    /// `(actor_id, option)` for the actors the per-domain catch-all + role-address
    /// pickers offer (`fauna.admin.users.list`), in list order — `option` is
    /// [`picker_option`]'s injective handle/hex string, never the raw editable
    /// label (`admin.md` § 2). Empty until the page load lands — a picker painted
    /// before its options would offer nothing.
    pub dns_actors: Vec<(Vec<u8>, String)>,
    /// Whether the inline `admin-dns-add-domain-*` form is revealed (armed by
    /// `admin-dns-add-domain-button`). tui has no modals, so every confirm/form on
    /// this page is an **inline reveal** painted into the page's own element list.
    pub dns_add_domain_open: bool,
    /// `admin-dns-add-domain-input` — the new domain's name draft.
    pub dns_add_domain_input: String,
    /// Whether the inline write-only `admin-dns-add-credential-*` form is revealed.
    pub dns_add_credential_open: bool,
    /// Which DNS provider the add-credential form is collecting fields for
    /// (`admin-dns-add-credential-provider-row[<pid>]`). `None` ⇒ no field entries
    /// paint yet, mirroring onboarding's `dns_config` and linux's rebuild-on-select.
    pub dns_credential_provider: Option<String>,
    /// The add-credential form's per-field drafts, keyed by the raw
    /// `providers.yaml` field id. Cleared when the provider pick changes (a token
    /// typed for one provider means nothing to another).
    pub dns_credential_fields: BTreeMap<String, String>,
    /// Whether `admin-dns-rename-sheet` is revealed (opened by the primary row's
    /// `admin-dns-domain-rename-button` or a non-primary row's `-promote-button`).
    pub dns_rename_sheet_open: bool,
    /// `admin-dns-rename-new-primary-select` — the picked promotion target's domain
    /// **name** (the label round-trip; resolved back to a `domain_id` at submit
    /// against the snapshot that painted it, so no positional index crosses the
    /// gesture door). Empty ⇒ default to the first offered target.
    pub dns_rename_target: String,
    /// `admin-dns-rename-grace-days-input` — the optional grace-window override.
    pub dns_rename_grace_days: String,
    /// `admin-dns-rename-extend-days-input` — the extend-by-N-days draft.
    pub dns_rename_extend_days: String,
    /// Whether the rename banner's **complete** confirm is armed — the inline
    /// reveal-then-confirm pair (`admin-dns-rename-complete-button` →
    /// `-complete-confirm-button`), the rotate-confirm shape.
    pub dns_rename_completing: bool,
    /// Whether the rename banner's **abort** confirm is armed
    /// (`admin-dns-rename-abort-button` → `-abort-confirm-button`).
    pub dns_rename_aborting: bool,
    /// Which domain's inline `admin-dns-cert-delegate-*` form is revealed, if any
    /// (`admin-dns-cert-delegate-button` arms it, per row). One at a time — the
    /// driver scopes its reads to `admin-dns-domain[i]`, and a second open form
    /// would be a second element with the same id in a different row's scope.
    pub dns_delegate_open: Option<String>,
    /// `admin-dns-cert-delegate-zone-select` — the picked controlled zone to
    /// re-home `_acme-challenge` into. Re-seeded to the first held-credential zone
    /// when the form opens (the picker always offers a submittable value).
    pub dns_delegate_zone: String,
    /// A DNS-page action that failed before reaching either machine, so neither
    /// snapshot carries the reason ([`Outcome::DnsActionFailed`]). Cleared by the
    /// next successful DNS fold, so it cannot outlive the state it describes.
    pub dns_action_error: Option<String>,
}

/// The armed deployment-identity rotation confirm (`admin-nest-seed-rotate-*`).
///
/// The roster read is a network round trip, so arming cannot paint the listing
/// synchronously the way the factory-reset confirm paints its warning. These are
/// the three honest renderings of that gap — an empty list is never one of them,
/// because "nobody inherits" and "we haven't asked yet" would look identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedRotateConfirm {
    /// The roster read is in flight; the confirm button is present but disabled.
    Loading,
    /// The roster read failed — the message is the page's reason line. The
    /// confirm stays disabled: dispatching would flip the box's identity while
    /// telling the admin nothing true about who keeps recovery custody.
    Failed(String),
    /// The roster answered; the shared fold decides what is painted and whether
    /// the confirm may fire at all.
    Ready(Box<SeedRotationConfirmView>),
}

/// The armed legal-takedown confirm (`admin-nest-takedown-confirm-*`): the form
/// **and** its folded view, both captured at arm time (see the
/// [`AdminState::takedown_confirm`] doc for why neither is re-derived while
/// armed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedTakedown {
    pub form: TakedownForm,
    pub view: TakedownFormView,
}

/// The armed forced-rotation confirm (`admin-nest-oauth-confirm-*`): which arm,
/// and its cost as folded when it was armed (see [`AdminState::oauth_confirm`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedOauthForced {
    pub arm: IssuerForcedArm,
    pub view: IssuerForcedConfirmView,
}

/// The issuer key set, once it has answered — the one precondition all three
/// issuer controls share: the page paints them disabled otherwise, and the
/// gestures refuse on the same test, so a driver-forced press cannot act on a
/// set nobody has read.
/// What the `nest_retire` page's admin entry re-reads live from the nest: the
/// local-domain machine (the nest's active domains). `None` pre-login.
pub(crate) fn retire_local_domains(state: &AdminState) -> Option<Arc<LocalDomainMachine>> {
    state.local_domains.clone()
}

pub(crate) fn oauth_keys(state: &AdminState) -> Option<&IssuerKeyView> {
    match state.nest_snapshot.as_ref().map(|s| &s.oauth) {
        Some(OauthKeysRead::Ready(view)) => Some(view),
        _ => None,
    }
}

/// The armed rotate confirm (`admin-bridges-rotate-confirm`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotateConfirm {
    /// The approved card's `admin-bridges-approved-pubkey-hex` — which **is**
    /// the `bridge_actor_id` the shared `Rotate` action decodes.
    pub pubkey_hex: String,
}

/// One `admin-web` render's data: the current apex designation, the actors the
/// picker offers (`fauna.admin.users.list`), and a page-scoped read/write error.
/// The linux `admin_web.rs::Snapshot` twin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminWebSnapshot {
    /// The designated apex actor's id, or `None` (the built-in info page).
    pub current: Option<Vec<u8>>,
    /// `(actor_id, option)` for every pickable actor, in `users.list` order —
    /// `option` is [`picker_option`]'s injective handle/hex string, never the
    /// raw editable label (`admin.md` § 2).
    pub actors: Vec<(Vec<u8>, String)>,
    /// The last read/write failure, bridged onto the page's `error-message`.
    pub error: Option<String>,
}

impl AdminState {
    /// The `https://<domain>/` the apex serves at, for `admin-web-apex-info`.
    ///
    /// The domain is the nest's own host (the fallback linux uses when its
    /// account cache has no domain) — `admin-web` is nest-wide, so the
    /// deployment's host IS the apex domain. Both halves are shared Rust: the
    /// host parse is `fauna_core::format::url_host` (the one shape linux/windows/
    /// apple's per-app parses were unified onto) and the formatting is
    /// `fauna_core::web::apex_url`, so the hint can't drift from the nest's
    /// routing.
    pub fn apex_url(&self) -> String {
        fauna_core::web::apex_url(&fauna_core::format::url_host(&self.nest_url))
    }
}

/// The apex picker's `(options, selected_option)`.
///
/// Option 0 is always the localized "None" (clear); then one option per actor,
/// by [`picker_option`]'s injective handle/hex string (`admin.md` § 2 — NEVER
/// the raw editable label). A designation the actor list doesn't carry gets a
/// **trailing fallback option** so it stays visible and selected — linux's
/// `actor_id_fallback_label` behaviour, and the reason a stale designation
/// can't read as an accidental clear. The option-string round-trip is the
/// picker contract: `get_text` returns the selected option's display text and
/// `select` takes that same text (`SelectTarget::ApexActor`), so this one
/// helper serves both paint and resolve.
pub(super) fn apex_option_labels(snap: &AdminWebSnapshot) -> (Vec<String>, String) {
    let mut options = Vec::with_capacity(snap.actors.len() + 2);
    options.push(t::web_page::APEX_NONE.to_string());
    options.extend(snap.actors.iter().map(|(_, label)| label.clone()));

    let selected = match &snap.current {
        None => t::web_page::APEX_NONE.to_string(),
        Some(current) => match snap.actors.iter().find(|(id, _)| id == current) {
            Some((_, label)) => label.clone(),
            None => {
                let fallback = t::actor_id_fallback_label(&fauna_core::format::hex_full(current));
                options.push(fallback.clone());
                fallback
            }
        },
    };
    (options, selected)
}

/// Resolve a picked apex **option string** ([`picker_option`] — handle, falling
/// back to actor hex; NEVER the raw editable label, `admin.md` § 2) back to its
/// actor id: `None` for the "None" option (a clear) and for the fallback option
/// (which already designates the current actor — re-picking it is a no-op the
/// caller drops).
///
/// Returns `None` for an unknown option too; the caller treats "no such option"
/// as nothing to do rather than silently clearing the designation.
fn resolve_apex_label(snap: &AdminWebSnapshot, label: &str) -> Option<ApexPick> {
    if label == t::web_page::APEX_NONE {
        return Some(ApexPick::Clear);
    }
    snap.actors
        .iter()
        .find(|(_, l)| l == label)
        .map(|(id, _)| ApexPick::Designate(id.clone()))
}

/// What an apex-picker selection resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ApexPick {
    /// Clear the designation — the apex reverts to the built-in info page.
    Clear,
    /// Designate this actor.
    Designate(Vec<u8>),
}

/// Build the admin state at the post-auth hook: wire the shared DAV machines
/// (cheap — each wraps `MailAdminClient::new(nest)`, no network) and fire the gate
/// check, whose result reveals (or, fail-closed, hides) the `admin-tab` sidebar
/// row. No sub-page is prefetched here — entering the shell is the trigger
/// ([`nav_enter_op`], awaited on the nav edge), so a login never pays for a page
/// the user may not open (the media/contacts nav-edge convention).
pub fn init(
    nest: Arc<NestClient>,
    nest_url: &str,
    secret_hex: &str,
    account: fauna_client_dns::AccountHandleSource,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) -> AdminState {
    use fauna_client_mail_settings::rpc_glue;
    let state = AdminState {
        caldav: Some(Arc::new(rpc_glue::build_caldav_policy_machine(
            nest.clone(),
        ))),
        carddav: Some(Arc::new(rpc_glue::build_carddav_policy_machine(
            nest.clone(),
        ))),
        webdav: Some(Arc::new(rpc_glue::build_webdav_policy_machine(
            nest.clone(),
        ))),
        forwarders: Some(Arc::new(rpc_glue::build_forwarders_machine(nest.clone()))),
        mail: Some(Arc::new(rpc_glue::build_mail_policy_machine(nest.clone()))),
        bridges: Some(Arc::new(rpc_glue::build_bridge_approval_machine(
            nest.clone(),
        ))),
        local_domains: Some(Arc::new(rpc_glue::build_local_domains_machine(
            nest.clone(),
        ))),
        // The credentialed DNS machine needs the admin's own keypair (the
        // issuance seal signs with it) and the account runtime (the DNS record
        // is the account's `fauna.state.dns` row). `secret_hex` already passed launch
        // validation, so the derive is practically infallible; a failure leaves
        // the machine `None` and the page paints its empty state rather than
        // panicking a whole session over it.
        dns: match fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex) {
            Ok(keypair) => Some(Arc::new(
                fauna_client_dns::build_dns_management_machine_with_credentials(
                    nest.clone(),
                    keypair,
                    account,
                ),
            )),
            Err(e) => {
                tracing::error!("[admin] admin-dns machine: derive keypair: {e:#}");
                None
            }
        },
        // The NAT-mode machine builds its own pre-identity `WsNestApi` from the
        // url + secret (payload-signature auth, no bearer) — the wizard's seam.
        nat: Some(AdminNatModeMachine::new(
            nest_url.to_string(),
            // `SecretString`, not a bare `String`, since row 331 hardened the
            // whole-session identity-seed carriers to the zeroizing newtype. That commit moved the shared crate and linux but
            // left this call site behind, so every tui build was red.
            fauna_core::secret::SecretString::new(secret_hex.to_string()),
        )),
        nest_url: nest_url.to_string(),
        nest: Some(nest),
        ..Default::default()
    };
    spawn_gate_check(&state, tx, session_generation);
    spawn_auto_renew_cadence(&state, tx, session_generation);
    state
}

/// Fire-and-forget the `fauna.account.am_i_admin` gate check — the **post-auth**
/// path (no driver ack to honour; the result lands through the channel and folds
/// via [`apply_outcome`]). Mirrors linux's `check_admin_status()` at AuthSuccess.
fn spawn_gate_check(state: &AdminState, tx: &UnboundedSender<UiMessage>, session_generation: u64) {
    let Some(nest) = state.nest.clone() else {
        return;
    };
    let tx = tx.clone();
    tokio::spawn(async move {
        let outcome = Op::CheckGate { nest }.run().await;
        let _ = tx.send(UiMessage::Data(DataMessage::Page(
            session_generation,
            PageOutcome::Admin(outcome),
        )));
    });
}

/// Fire-and-forget the nest's public host-address report
/// (`fauna.dns.set_host_address`), the tui twin of linux's
/// `client.rs::report_host_address` and the native FFI / web `reportHostAddress`
/// bindings. tui was the last client without a call site
/// (`dns-management.md` § Implementation status → *Remaining* item 2).
///
/// Per priority #2 there is **no** logic here: the shared
/// `fauna_client_dns::host_address::report_host_address` classifies the
/// dial-address, enforces the safety invariant (**never** reports a private/LAN
/// address), reports the public IP when one is determinable, and logs its own
/// outcome. Idempotent last-writer-wins nest-side, so repeats are harmless and a
/// failure just retries on the next session.
fn spawn_host_address_report(state: &AdminState) {
    let Some(nest) = state.nest.clone() else {
        return;
    };
    tokio::spawn(async move {
        // The dial-address the client used to reach the nest, read before the
        // Arc moves into the typed DNS caller (linux's ordering).
        let dial_url = nest.nest_url();
        let dns = fauna_client_dns::DnsAdminClient::new(nest);
        let probe = fauna_client_dns::host_address::NativeHostAddressProbe;
        let _ = fauna_client_dns::host_address::report_host_address(&dns, &dial_url, &probe).await;
    });
}

/// The 32-byte node id the connection is bound to — the cert-delivery target
/// an issuance seals to (`tls-certificates.md` § B.2). The shared
/// `fauna-client-pair::resolve_this_nest_id`; it rides the *action* rather
/// than living inside `DnsManagementMachine` so the machine stays decoupled
/// from linked-nests while all 7 apps resolve it the same way (D7).
use fauna_client_pair::resolve_this_nest_id;

/// Spawn the hands-off certificate-renewal cadence (`tls-certificates.md` § C.3:
/// *"Any synced admin device renews … with no admin tap"*). tui is a **native**
/// app, so like linux/apple/android it owes this loop — web is the only app that
/// legitimately has none (a browser SPA runs no background timer, so it issues on
/// page-open instead).
///
/// **Nothing about the tick is decided here.** The cadence, the refresh-then-ask
/// order, the skip-if-empty rule, the per-domain non-fatal rule and the trailing
/// health re-read all live in `fauna_client_dns` (`auto_renew_poll_secs`,
/// `auto_renew_scan`, `auto_renew_issue`), so every native app runs the identical
/// tick. This loop owns only what is genuinely tui's: the timer, the
/// `target_nest_id` resolution (linked-nests state, D7 — deliberately outside the
/// DNS machine), the log sink, and shipping the refreshed snapshot to an open
/// `admin-dns` page.
fn spawn_auto_renew_cadence(
    state: &AdminState,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) {
    let (Some(machine), Some(nest)) = (state.dns.clone(), state.nest.clone()) else {
        return;
    };
    let tx = tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(
                fauna_client_dns::auto_renew_poll_secs(),
            ))
            .await;
            let due = machine.auto_renew_scan().await;
            if due.is_empty() {
                // Nothing due ⇒ ship nothing, so an open admin-dns page is not
                // re-rendered for a no-op, and no nest id is resolved.
                continue;
            }
            let target_nest_id = match resolve_this_nest_id(&nest).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!("[admin] auto-renew cadence: resolve target nest id: {e}");
                    continue;
                }
            };
            let pass = machine.auto_renew_issue(due, target_nest_id).await;
            for failure in &pass.failed {
                tracing::warn!(
                    "[admin] auto-renew cadence: IssueCert {}: {}",
                    failure.domain,
                    failure.error
                );
            }
            let _ = tx.send(UiMessage::Data(DataMessage::Page(
                session_generation,
                PageOutcome::Admin(Outcome::DnsSnapshotOnly(Box::new(machine.snapshot()))),
            )));
        }
    });
}

/// Entering the admin shell loads its **current** sub-page (the Dashboard on a
/// fresh nav edge, after `App::apply` reset `sub`) — the page's leg of the one
/// nav-edge hook ([`crate::app::on_nav_enter`]). Returns the op; the agent's nav
/// **awaits** it (single-shot reads), the keyboard's spawns it.
pub fn nav_enter_op(state: &AdminState) -> Option<Op> {
    subpage_load_op(state, state.sub)
}

/// Route the two-element nav `{"view":"admin","id":"<page>"}` to a sub-page and
/// return its self-hydrate op — the admin twin of `settings::route_subpage`, run
/// by `automation::apply_nav` **after** the one nav door already fired
/// [`nav_enter_op`]. The `id` is the ui.yaml **page id** (`admin-calendar`, …),
/// the contract the e2e driver sends (`actions/admin.py`).
pub fn route_subpage(state: &mut AdminState, id: Option<&str>) -> Option<Op> {
    let page = match id {
        None | Some("dashboard") | Some("admin-dashboard") => AdminPage::Dashboard,
        Some("admin-calendar") => AdminPage::Calendar,
        Some("admin-contacts") => AdminPage::Contacts,
        Some("admin-files") => AdminPage::Files,
        Some("admin-nest") => AdminPage::Nest,
        Some("admin-mail") => AdminPage::Mail,
        Some("admin-aliases") => AdminPage::Aliases,
        // `actions/admin.py::navigate_web` sends the full ui.yaml page id.
        Some("admin-web") => AdminPage::Web,
        // Likewise `actions/admin.py::navigate_bridges_pending`.
        Some("admin-bridges-pending") => AdminPage::Bridges,
        // Likewise `actions/admin.py::navigate_dns` (`:108` sends the full page id).
        Some("admin-dns") => AdminPage::Dns,
        // Likewise `actions/admin.py::navigate_logs`.
        Some("admin-logs") => AdminPage::Logs,
        // Likewise `actions/admin.py::navigate_custody_hosting`.
        Some("admin-custody-hosting") => AdminPage::CustodyHosting,
        // The Users hub's driver nav sends the SHORT id `users` (not the full
        // `admin-users`) — `actions/admin.py::navigate_users`/`navigate_invite_codes`;
        // accept both, the settings-short-id precedent.
        Some("users") | Some("admin-users") => AdminPage::Users,
        // The Tiers page's driver nav sends the SHORT id `settings` (not the full
        // `admin-settings`) — `actions/admin.py::navigate_settings`; accept both.
        Some("settings") | Some("admin-settings") => AdminPage::Settings,
        Some(other) => {
            tracing::warn!("[admin] two-element nav to unknown sub-page {other:?}");
            AdminPage::Dashboard
        }
    };
    state.sub = page;
    // The Dashboard's landing fetch is [`nav_enter_op`]'s job (fired by the one nav
    // door just before this runs), so returning `None` here avoids a double
    // `fauna.admin.stats` read — the settings `route_subpage(Root) -> None` shape.
    // The DAV pages self-hydrate.
    match page {
        AdminPage::Dashboard => None,
        _ => subpage_load_op(state, page),
    }
}

/// The fetch a sub-page needs on entry: the Dashboard re-reads its stats; a DAV
/// page hydrates its machine (`dispatch(Refresh)`, not `hydrate()`, so a read
/// failure lands on the snapshot's `error` and reaches `error-message` — see
/// [`apply_outcome`]). Owns only `Arc`s, so the returned [`Op`] crosses a
/// `tokio::spawn` (keyboard) and an `await` (agent).
fn subpage_load_op(state: &AdminState, page: AdminPage) -> Option<Op> {
    match page {
        AdminPage::Dashboard => state.nest.clone().map(|nest| Op::LoadDashboard { nest }),
        AdminPage::Calendar => state.caldav.clone().map(|machine| Op::Caldav {
            machine,
            action: CaldavPolicyAction::Refresh,
        }),
        AdminPage::Contacts => state.carddav.clone().map(|machine| Op::Carddav {
            machine,
            action: CarddavPolicyAction::Refresh,
        }),
        AdminPage::Files => state.webdav.clone().map(|machine| Op::Webdav {
            machine,
            action: WebdavPolicyAction::Refresh,
        }),
        // The Nest page reads three surfaces at once (pairing + setup.status +
        // the NAT machine's hydrate) — one Op, one fold, the "one snapshot"
        // discipline. Needs both the client and the NAT machine.
        AdminPage::Nest => match (state.nest.clone(), state.nat.clone()) {
            (Some(nest), Some(nat)) => Some(Op::LoadNest { nest, nat }),
            _ => None,
        },
        AdminPage::Aliases => state.forwarders.clone().map(|machine| Op::Forwarders {
            machine,
            action: ForwarderAction::Refresh,
        }),
        // The mail page hydrates its machine (three reads: get_mail_config +
        // get_alias_policy + get_auto_enable_mail_for_new_users) via `dispatch(Refresh)`
        // — not `hydrate()` — so a read failure lands on the snapshot's `error` and
        // reaches `error-message`, the DAV-page discipline.
        AdminPage::Mail => state.mail.clone().map(|machine| Op::Mail {
            machine,
            action: MailPolicyAction::Refresh,
        }),
        AdminPage::Settings => state.nest.clone().map(|nest| Op::LoadTiers { nest }),
        // The Users hub reads the whole page at once (users list + tier names for
        // the pickers + invite codes + pending requests + the registration posture)
        // — one Op, one `UsersLoaded` fold, offset-preserving (the "one snapshot"
        // discipline).
        AdminPage::Users => state.nest.clone().map(|nest| Op::LoadUsers {
            nest,
            offset: state.users.offset,
        }),
        // The Web page reads both halves at once — the apex designation
        // (`fauna.web.get_apex_actor`) and the actors the picker offers
        // (`fauna.admin.users.list`) — as one Op, one fold: a picker painted
        // before its option list lands would offer nothing to select.
        AdminPage::Web => state.nest.clone().map(|nest| Op::LoadWeb { nest }),
        // The registry is one nest-wide read; the projection (and therefore
        // the row order) is the shared fold's, not this page's.
        AdminPage::CustodyHosting => state
            .nest
            .clone()
            .map(|nest| Op::LoadCustodyHosting { nest }),
        // The bridges page hydrates its machine via `dispatch(Refresh)` — not
        // `hydrate()` — so a read failure lands on the snapshot's `error` and
        // reaches `error-message`, the DAV-page discipline.
        AdminPage::Bridges => state.bridges.clone().map(|machine| Op::Bridges {
            machine,
            action: BridgeApprovalAction::Refresh,
        }),
        // The DNS page reads THREE surfaces at once — the local-domain rows (which
        // domains exist, their catch-all / role-address designations, the rename
        // state), the DNS record matrix + live verdicts, and the actor list the
        // per-row pickers offer — as one Op, one fold. Split loads would paint a
        // row before its records, or a picker before its options
        // (`dns-management.md` § App surface: one page, two machines).
        AdminPage::Dns => match (
            state.nest.clone(),
            state.dns.clone(),
            state.local_domains.clone(),
        ) {
            (Some(nest), Some(dns), Some(domains)) => Some(Op::LoadDns { nest, dns, domains }),
            _ => None,
        },
        // The Logs page is a pure read of the nest ring — one fetch on entry, then
        // every filter change narrows the held source client-side (no refetch),
        // which is what makes `All` restore exactly the pre-filter count.
        AdminPage::Logs => state.nest.clone().map(|nest| Op::LoadLogs { nest }),
    }
}

/// An admin-shell gesture, dispatched through the one gesture door as
/// `Gesture::Admin(Action)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A rail row → switch to an admin sub-page (and hydrate it).
    Open(AdminPage),
    /// `admin-calendar-enabled-toggle` — flip the deployment-wide CalDAV enable.
    ToggleCaldavEnabled,
    /// `admin-calendar-caldav-port-save-button` — parse the port draft and save.
    SaveCaldavPort,
    /// `admin-contacts-carddav-enabled-toggle` — flip the CardDAV enable.
    ToggleCarddavEnabled,
    /// `admin-files-webdav-enabled-toggle` — flip the WebDAV enable.
    ToggleWebdavEnabled,
    /// `admin-service-pairing-toggle` — flip the user-pairing policy.
    TogglePairing,
    /// `admin-nest-serving-port-save-button` — parse the serving-port draft and save.
    SaveServingPort,
    /// `admin-nest-region-save-button` — validate the region draft and declare it.
    SaveRegion,
    /// `admin-nest-region-withdraw-button` — withdraw the declaration (the absent
    /// case on the wire), which also retires the previous region's policy document.
    WithdrawRegion,
    /// `admin-nest-web-app-origin-{bundled,central}-radio` — pick the mode
    /// (local; no network until the save).
    SelectWebAppOrigin(fauna_client_admin::WebAppOrigin),
    /// `admin-nest-web-app-origin-save-button` — commit the selected mode.
    SaveWebAppOrigin,
    /// `admin-nest-feature-limits-edit-button[i]` — open the shared editor at
    /// the ADMIN tier for the member with this stable key (local; seeded from
    /// the reflective snapshot's authored read).
    OpenFeatureLimitEditor(String),
    /// `feature-policy-editor-on-radio` (`true`) / `-off-radio` (`false`) on
    /// the admin host (local).
    FeatureLimitOn(bool),
    /// `feature-policy-editor-cancel-button` on the admin host (local).
    CancelFeatureLimitEditor,
    /// `feature-policy-editor-save-button` on the admin host — parse through the
    /// shared seam (a refusal dispatches nothing), write, re-read the page.
    SaveFeatureLimit,
    /// `feature-policy-editor-remove-button` on the admin host — the absent
    /// policy, then the re-read.
    RemoveFeatureLimit,
    /// `admin-nest-nat-mode-{public,private}-radio` — pick the NAT mode (local;
    /// `AdminNatModeMachine::select`, no network).
    SelectNatMode(NodeMode),
    /// `admin-nest-nat-mode-save-button` — sign + commit the selected NAT mode.
    SaveNatMode,
    /// `nest-os-restart-now-button` — request a host restart.
    RestartHost,
    /// `admin-factory-reset-button` — arm the inline confirm.
    OpenFactoryResetConfirm,
    /// `admin-factory-reset-confirm-button` — mint+persist the claim code, dispatch
    /// `fauna.admin.factory_reset`, then re-onboard at the pre-filled claim code.
    ConfirmFactoryReset,
    /// `admin-nest-seed-rotate-button` — arm the rotation confirm and start the
    /// roster read that fills its listing.
    OpenSeedRotateConfirm,
    /// `admin-nest-seed-rotate-cancel-button` — disarm, touching nothing.
    CancelSeedRotate,
    /// `admin-nest-seed-rotate-confirm-button` — drive the ceremony (mint →
    /// custody → dispatch → mark, in shared Rust); the plane carries the successor's row, no fan-out.
    ConfirmSeedRotate,
    /// `admin-custody-hosting-remove-button-<i>` — arm the remove confirm for
    /// one registry row. Carries the `(host, grant)` pair the remove door is
    /// keyed by rather than the painted index: the fold re-orders rows
    /// heaviest-hold-first, so an index would name a different row after a
    /// re-read.
    OpenCustodyHostingRemoveConfirm {
        host_actor_id: String,
        grant_id: Vec<u8>,
    },
    /// `admin-custody-hosting-remove-cancel-button` — disarm, touching nothing.
    CancelCustodyHostingRemove,
    /// `admin-custody-hosting-remove-confirm-button` — drop the armed row
    /// (`fauna.admin.custody_hosting.remove`) and re-read the registry. The
    /// custodied store beneath it falls only when the row was its
    /// `(host, owner)` pair's last.
    ConfirmCustodyHostingRemove,
    /// `admin-nest-takedown-type-post-radio` / `-conversation-radio` — pick the
    /// content kind for the legal-takedown console (local; the draft only).
    SetTakedownConversation(bool),
    /// `admin-nest-takedown-restore-checkbox` — flip the console between take
    /// down and overturn/restore (local).
    ToggleTakedownRestore,
    /// `admin-nest-takedown-button` — arm the inline confirm, capturing the
    /// form + its shared fold. A press while the fold refuses (`can_submit`
    /// false — the button renders disabled) stays a no-op.
    OpenTakedownConfirm,
    /// `admin-nest-takedown-cancel-button` — disarm, touching nothing.
    CancelTakedown,
    /// `admin-nest-takedown-confirm-button` — dispatch the CAPTURED form over
    /// `fauna.moderation.legal_takedown` (disarm-first, the seed-rotate shape).
    ConfirmTakedown,
    /// `admin-nest-report-open-takedown-button` — pre-fill the takedown
    /// console with the report's subject (the shared `takedown_prefill`).
    /// Local: the console's own guards still stand between this and a
    /// dispatch.
    OpenReportTakedown {
        report_id: String,
    },
    /// `admin-nest-report-acted-button` / `-dismiss-button` — record the
    /// outcome (`fauna.moderation.abuse_report.resolve`). A record, not an
    /// action: nothing happens to the content or the author.
    ResolveReport {
        report_id: String,
        outcome: fauna_protocol::moderation::AbuseReportOutcome,
    },
    /// `admin-nest-oauth-rotate-button` — the ordinary issuer-key rotation
    /// (`fauna.oauth.rotate_issuer_key`). No confirm: the outgoing key stays
    /// accepted for the horizon, so nothing breaks, and its cost is stated
    /// beside the button (`authorization-server.md` § The issuer → *Two
    /// rotation arms*).
    RotateIssuerKey,
    /// `admin-nest-oauth-force-rotate-button` / `-secret-force-rotate-button` —
    /// arm the inline confirm for one forced arm, capturing its folded cost.
    /// Local: nothing dispatches until the confirm.
    OpenOauthForcedConfirm(IssuerForcedArm),
    /// `admin-nest-oauth-cancel-button` — disarm, touching nothing.
    CancelOauthForced,
    /// `admin-nest-oauth-confirm-button` — dispatch the armed forced arm
    /// (disarm-first). Carries the arm it was painted for, so the offline gate
    /// names the exact kind, and a confirm painted for one arm can never fire
    /// the other after a re-arm raced it.
    ConfirmOauthForced(IssuerForcedArm),
    /// `admin-aliases-forwarder-add-domain-select` — pick the hosted domain for the
    /// new forwarder (local; sets the draft, no network).
    SelectForwarderDomain(String),
    /// `admin-web-apex-actor-select` — designate (an actor label) or clear (the
    /// localized "None") the deployment apex actor. Unlike the forwarder-domain
    /// picker this one is NOT a local draft: picking commits immediately, exactly
    /// as linux's `connect_selected_notify` → `set_apex` does.
    SelectApexActor(String),
    /// `log-level-filter` on `admin-logs` — narrow the **already-fetched** nest
    /// ring to a severity. Purely local (no refetch), which is what makes going
    /// back to "All" restore exactly the pre-filter row count.
    SetLogLevel(u32),
    /// `log-copy-button` on `admin-logs` — copy the currently-filtered nest lines
    /// to the terminal clipboard over OSC 52, the same path
    /// `settings::Action::CopyLogs` uses. Local and fire-and-forget.
    CopyLogs,
    /// `admin-bridges-pending-approve-button[row]` — approve the pending bridge,
    /// carrying its own pubkey + enrolled role (never a positional guess: the
    /// feed re-reads between paint and click).
    ApproveBridge {
        pubkey_hex: String,
        role: String,
    },
    /// `admin-bridges-pending-reject-button[row]` — reject the pending bridge
    /// (`→ revoked`; the row is kept so a re-connect rejects immediately).
    RejectBridge {
        pubkey_hex: String,
    },
    /// `admin-bridges-approved-rotate-button[row]` — arm the inline rotate
    /// confirm. Local: no network until the confirm.
    OpenRotateConfirm {
        pubkey_hex: String,
    },
    /// `admin-bridges-rotate-confirm-button` — dispatch the shared `Rotate`
    /// action (`revoke_service_user`) for the armed bridge.
    ConfirmRotate,
    /// `admin-bridges-rotate-cancel-button` — drop the armed confirm, no write.
    CancelRotate,
    /// `admin-aliases-forwarder-add-submit-button` — create an external forwarder
    /// from the add-form drafts (domain + local-part + target).
    CreateForwarder,
    /// `admin-aliases-forwarder-row-delete-button` — delete the forwarder with this
    /// hex alias id (from the rendered `ForwarderView`).
    DeleteForwarder {
        alias_id_hex: String,
    },
    /// `admin-settings-tier-save-button` — parse the `row`-th tier's five cap
    /// drafts and persist them (`fauna.admin.tiers.update`).
    SaveTier {
        row: usize,
    },
    /// `admin-settings-tier-add-button` — parse the add form's name + five cap
    /// drafts and define the tier (`fauna.admin.tiers.create`).
    AddTier,
    /// `admin-settings-membership-tier-select[row]` — re-point the `row`-th
    /// membership row to a different owned subscription tier (LOCAL draft;
    /// display + fix-up only — Save reads the current selection directly).
    SetMembershipTierName {
        row: usize,
        tier_name: String,
    },
    /// `admin-settings-membership-admin-tier-select[row]` — set the `row`-th
    /// membership row's admitted quota-tier draft (LOCAL).
    SetMembershipAdminTier {
        row: usize,
        tier: String,
    },
    /// `admin-settings-membership-lapse-tier-select[row]` — set the `row`-th
    /// membership row's lapsed quota-tier draft (LOCAL).
    SetMembershipLapseTier {
        row: usize,
        tier: String,
    },
    /// `admin-settings-membership-save-button[row]` — designate/re-point the
    /// `row`-th membership row from its drafts (`fauna.admin.membership_tiers.set`,
    /// an upsert).
    SaveMembership {
        row: usize,
    },
    /// `admin-settings-membership-clear-button[row]` — drop the `row`-th
    /// membership row's designation (`fauna.admin.membership_tiers.clear`); the
    /// subscription tier itself survives, reverting to undesignated.
    ClearMembership {
        row: usize,
    },
    /// `admin-mail-enabled-toggle` — flip the deployment-wide mail-enable
    /// (non-optimistic, dispatched immediately — not gathered into a group PUT).
    ToggleMailEnabled,
    /// `admin-mail-auto-enable-new-users-toggle` — flip the deployment-wide
    /// auto-enable-mail-for-new-users policy (immediate; read back from setup.status).
    ToggleMailAutoEnable,
    /// One `admin-mail-*` in-group toggle — flip its draft `bool` (local; gathered
    /// into the group's full-PUT on Save).
    ToggleMail(MailToggle),
    /// `admin-mail-fcrdns-mode-select` — set the FCrDNS-mode draft (local wire value).
    SetFcrdnsMode(String),
    /// `admin-mail-imap-delete-nonempty-select` — set the delete-non-empty draft.
    SetImapDelete(String),
    /// `admin-mail-spam-save-button` — full-PUT the Spam group (`put_spam_policy`).
    SaveMailSpam,
    /// `admin-mail-auth-save-button` — full-PUT the Auth group (`put_auth_policy`).
    SaveMailAuth,
    /// `admin-mail-submission-save-button` — full-PUT Submission (`put_submission_policy`).
    SaveMailSubmission,
    /// `admin-mail-imap-save-button` — full-PUT the IMAP group (`put_imap_policy`).
    SaveMailImap,
    /// `admin-mail-outbound-save-button` — full-PUT Outbound (`put_outbound_policy`).
    SaveMailOutbound,
    /// `admin-mail-alias-save-button` — full-PUT the Alias group (`put_alias_policy`).
    SaveMailAlias,
    /// `admin-mail-publish-spam-baseline-button` — aggregate opted-in users' models
    /// (`publish_spam_baseline`); the reply stashes on the snapshot, no re-read.
    PublishSpamBaseline,
    /// `admin-mail-spam-baseline-standing-toggle` — flip standing baseline publish
    /// (immediate, non-optimistic, like the two deployment-wide toggles: the shared
    /// machine full-PUTs the persisted Spam group with the one field flipped, then
    /// re-reads; off withdraws the served baseline).
    ToggleMailBaselineStanding,
    /// `admin-mail-health-recheck-button` — the shared
    /// `MailPolicyAction::RecheckHealth` (a blocklist self-check + a diagnostics
    /// run, then a re-read of the health readout).
    RecheckMailHealth,
    /// `admin-mail-health-warmup-reset-button` — the two-click inline confirm:
    /// the first press arms (local), the second dispatches the shared
    /// `MailPolicyAction::ResetWarmup` (`outbound_warmup_reset`, then a re-read).
    ResetMailWarmup,
    /// `admin-mail-health-delist-link` — a terminal cannot open a browser, so
    /// the link shows the de-listing URL and copies it (OSC 52). Local.
    CopyMailDelistUrl {
        url: String,
    },
    // ── admin-users: the user-list section (`admin.md` § 2 Users → Section 4) ──
    /// `admin-users-tier-select[row]` — change the `row`-th user's tier (= quota).
    /// LIVE + re-read (the select applies `users_update` and the row re-renders from
    /// persisted state, so `get_text` reads back the target — the e2e's contract).
    SetUserTier {
        row: usize,
        tier: String,
    },
    /// `admin-users-evict-button[row]` — start the timed eviction ladder for the
    /// `row`-th user (`users_evict`).
    EvictUser {
        row: usize,
    },
    /// `admin-users-suspend-button[row]` — suspend the `row`-th user immediately
    /// (`users_suspend`; no delete timeline).
    SuspendUser {
        row: usize,
    },
    /// `admin-users-cancel-eviction-button[row]` — restore the `row`-th user from
    /// eviction or suspension (`users_cancel_eviction`).
    CancelUserEviction {
        row: usize,
    },
    /// `admin-users-make-admin-button[row]` — grant the `row`-th user the admin
    /// role (`admins_add`; a scheduled pending action, `admin.md` § Admin
    /// continuity and succession).
    MakeAdmin {
        row: usize,
    },
    /// `admin-users-remove-admin-button[row]` — revoke the `row`-th user's admin
    /// role (`admins_remove`; the nest refuses at the last superadmin).
    RemoveAdmin {
        row: usize,
    },
    /// `admin-pending-action-approve-button[i]` — add this admin's approval to
    /// pending action `id` (`pending_action_approve`; the nest refuses
    /// self-approval, surfaced on the page-scoped error — `admin.md` § Pending
    /// admin actions).
    ApprovePending {
        id: i64,
    },
    /// `admin-pending-action-cancel-button[i]` — call pending action `id` off
    /// (`pending_action_cancel`; one click, no confirm).
    CancelPending {
        id: i64,
    },
    /// `admin-users-next-page` — advance the user list one page (offset += 50),
    /// guarded at the last page.
    UsersNextPage,
    /// `admin-users-prev-page` — go back one page (offset -= 50), guarded at 0.
    UsersPrevPage,
    // ── admin-users: the registration section (`admin.md` § 2 → Section 2) ──
    /// `admin-users-registration-mode-select` — set the registration-mode draft
    /// (LOCAL wire value; committed by the save button).
    SetRegistrationMode(String),
    /// `admin-users-registration-age-verification-toggle` — set the age
    /// require-knob draft (LOCAL; committed by the section save, which sends
    /// `set_age_verification_required` only when the value changed).
    SetAgeVerificationRequired(bool),
    /// `admin-users-registration-save-button` — commit the mode + free-tier ceiling
    /// together (`set_registration_mode`), plus the age require-knob when changed.
    SaveRegistration,
    // ── admin-users: the Admit section (public-mode.md § Registration &
    //    Identity — direct admission, the third account-creation path) ──
    /// `admin-users-admit-tier-select` — set the admitted account's tier draft
    /// (LOCAL; committed by the admit button).
    SetAdmitTier(String),
    /// `admin-users-admit-button` — admit the drafted actor id under the
    /// drafted handle + tier (one `fauna.admin.users.create` call).
    AdmitUser,
    // ── admin-users: the invite-code section (`admin.md` § 2 → Section 3) ──
    /// `admin-settings-tier-select` — set the minted code's tier draft (LOCAL).
    SetInviteTier(String),
    /// `admin-users-invite-guardian-select` — set the minted code's guardian draft
    /// (LOCAL; `""` = none). Clearing the guardian resets the band draft.
    SetInviteGuardian(String),
    /// `admin-users-invite-age-band-select` — set the minted code's age-band draft
    /// (LOCAL; an option value — `not-set` or a wire token).
    SetInviteAgeBand(String),
    /// `create-invite-code-btn` — reveal the create form.
    OpenInviteForm,
    /// `admin-settings-invite-cancel-button` — hide the create form.
    CancelInviteForm,
    /// `create-invite-confirm-btn` — mint an invite code from the drafts (empty code
    /// ⇒ the nest mints); the reply's token is revealed copyable.
    ConfirmInvite,
    /// `admin-users-invite-code-copy-btn` — copy the freshly minted token to the
    /// clipboard (client glue).
    CopyMintedCode,
    /// `admin-settings-invite-delete-button[i]` — delete the invite code with this token.
    DeleteInvite {
        code: String,
    },
    // ── admin-users: the pending-requests section (`admin.md` § 2 → Section 1) ──
    /// `invite-request-row-tier-select[row]` — set the `row`-th request's approve-at
    /// tier draft (LOCAL).
    SetRequestTier {
        row: usize,
        tier: String,
    },
    /// `invite-request-row-guardian-select[row]` — set the `row`-th request's guardian
    /// draft (LOCAL; `""` = none). Clearing the guardian resets the band draft.
    SetRequestGuardian {
        row: usize,
        guardian: String,
    },
    /// `invite-request-row-age-band-select[row]` — set the `row`-th request's
    /// age-band draft (LOCAL; an option value).
    SetRequestAgeBand {
        row: usize,
        band: String,
    },
    /// `invite-request-row-approve-button[row]` — approve the `row`-th request at its
    /// drafted tier (+ guardian), admitting the requester.
    ApproveRequest {
        row: usize,
    },
    /// `invite-request-row-deny-button[row]` — deny the `row`-th request with its
    /// (optional) drafted reason.
    DenyRequest {
        row: usize,
    },
    // ── admin-dns (`dns-management.md` § App surface) ────────────────────────
    /// `admin-dns-refresh-button` — re-read the record matrix, the held
    /// credentials, and the domain rows (the whole page, one Op).
    RefreshDns,
    /// `admin-dns-add-domain-button` / `-cancel-button` — reveal/hide the inline
    /// add-domain form (local; the draft survives a cancel, the settings shape).
    OpenDnsAddDomain,
    CancelDnsAddDomain,
    /// `admin-dns-add-domain-submit-button` — claim the drafted domain
    /// (`LocalDomainAction::AddDomain` at the shared default MTA-STS knobs; the
    /// nest auto-sets `is_primary` on the first one and is idempotent).
    SubmitDnsAddDomain,
    /// `admin-dns-domain-remove-button[row]` — soft-delete the domain, carrying its
    /// **name** rather than the row index: the rows re-read between paint and click
    /// (the bridges-approve lesson). The primary's button paints disabled — the
    /// nest refuses it anyway (`cannot_remove_primary_domain`).
    RemoveDnsDomain {
        domain: String,
    },
    /// `admin-dns-removed-domain-restore-button[row]` — un-soft-delete within the
    /// 30-day window, likewise by name.
    RestoreDnsDomain {
        domain: String,
    },
    /// `admin-dns-domain-mode[row]` — flip one domain's Fauna-managed opt-in
    /// (`DnsAction::SetMode`). Opting in without a covering held credential is
    /// rejected `InvalidState` and surfaces on `error-message`; the current value
    /// rides the gesture so a re-read between paint and click can't invert it.
    ToggleDnsDomainMode {
        domain: String,
        managed: bool,
    },
    /// `admin-dns-manage-all-toggle` — the deployment master switch: set **every**
    /// active domain's mode at once (stored per-domain — `dns-management.md`
    /// § The two modes). `managed` is the target state, read from the shared
    /// `DnsSnapshot::all_domains_managed` projection at paint time.
    ToggleDnsManageAll {
        managed: bool,
    },
    /// `admin-dns-domain-catch-all-select[row]` — designate (an actor label) or
    /// clear (the localized "None") this domain's catch-all actor. Commits
    /// immediately, like the apex picker.
    SelectDnsCatchAll {
        row: usize,
        label: String,
    },
    /// `admin-dns-domain-role-address-<role>-select[row]` — designate or clear
    /// ("Admin (default)") one role address's override actor. Commits immediately.
    SelectDnsRoleAddress {
        row: usize,
        role: RoleAddressKind,
        label: String,
    },
    /// `admin-dns-add-credential-button` / `-cancel-button` — reveal/hide the inline
    /// write-only add-credential form. Cancel also drops the provider pick + every
    /// typed field (a secret must not linger behind a closed form).
    OpenDnsAddCredential,
    CancelDnsAddCredential,
    /// `admin-dns-add-credential-provider-row[<pid>]` — pick the provider, which
    /// rebuilds the dynamic field set from `providers.yaml`. Clears the prior
    /// provider's drafts (a token for one provider means nothing to another).
    SelectDnsCredentialProvider(String),
    /// `admin-dns-add-credential-submit-button` — verify the typed credential
    /// against the provider API and, on success, seal it into `fauna.state.dns`
    /// (`DnsAction::PutCredentials`). A failed `verify()` stores nothing and
    /// surfaces on `error-message`.
    SubmitDnsCredential,
    /// `admin-dns-record-copy-button[i]` — copy that record's exact expected value
    /// to the terminal clipboard over OSC 52, the `CopyMintedCode` path. The value
    /// rides the gesture (it is already painted), so the copy can't race a re-read.
    CopyDnsRecord {
        value: String,
    },
    /// `admin-dns-credential-item-clear-button[i]` — drop the held credential at
    /// this `DnsSnapshot.credentials` index; domains that lose coverage re-render
    /// manual.
    ClearDnsCredential {
        index: u32,
    },
    /// `admin-dns-domain-rename-button` (primary row) / `-promote-button[row]`
    /// (a non-primary row, pre-targeting itself) — reveal the rename sheet.
    OpenDnsRenameSheet {
        target: Option<String>,
    },
    CancelDnsRenameSheet,
    /// `admin-dns-rename-new-primary-select` — pick the promotion target by domain
    /// name (local draft; resolved to a `domain_id` at submit).
    SelectDnsRenameTarget(String),
    /// `admin-dns-rename-submit-button` — start the rename
    /// (`LocalDomainAction::StartPrimaryRename`). The nest validates every
    /// precondition; a refusal surfaces on `error-message`.
    SubmitDnsRename,
    /// `admin-dns-rename-complete-button` → `-complete-confirm-button` — arm, then
    /// complete the in-flight rename (forcing when only `can_force_complete`).
    OpenDnsRenameCompleteConfirm,
    ConfirmDnsRenameComplete,
    /// `admin-dns-rename-extend-button` — extend the grace window by the drafted
    /// number of days.
    ExtendDnsRenameGrace,
    /// `admin-dns-rename-abort-button` → `-abort-confirm-button` — arm, then abort
    /// the in-flight rename (destructive; the confirm names the re-flip cost).
    OpenDnsRenameAbortConfirm,
    ConfirmDnsRenameAbort,
    // ── admin-dns: the per-domain TLS-cert lifecycle (`tls-certificates.md`) ────
    /// `admin-dns-cert-issue-button[row]` — get/renew a publicly-trusted cert.
    /// `single_issue` (read from the snapshot at paint time: the domain is
    /// effectively managed **or** delegated, i.e. a client *can* auto-publish
    /// `_acme-challenge`) picks the path: a single `IssueCert`, else the two-phase
    /// `BeginManualIssueCert` that surfaces the TXT to paste (§ B tier 2/3).
    IssueDnsCert {
        domain: String,
        single_issue: bool,
    },
    /// `admin-dns-cert-complete-button` — finalize the suspended manual order now
    /// that the admin pasted the TXT. Carries no domain: the machine holds at most
    /// one pending issuance (`snapshot.pending_cert`).
    CompleteDnsManualIssue,
    /// `admin-dns-cert-cancel-button` — abandon the suspended manual order; the
    /// nest stays gracefully on the self-signed floor.
    CancelDnsManualIssue,
    /// `admin-dns-cert-delegate-button[row]` / `-delegate-cancel-button` — reveal /
    /// hide this domain's one-time CNAME renewal-delegation form.
    OpenDnsDelegate {
        domain: String,
    },
    CancelDnsDelegate,
    /// `admin-dns-cert-delegate-zone-select` — pick the controlled target zone
    /// (local draft; the submit reads it).
    SelectDnsDelegateZone(String),
    /// `admin-dns-cert-delegate-submit-button` — persist the delegation and surface
    /// the one-time CNAME for the admin to paste once.
    SubmitDnsDelegate,
    /// `admin-dns-cert-remove-delegation-button[row]` — drop the delegation; the
    /// domain reverts to manual paste-per-renewal.
    RemoveDnsDelegation {
        domain: String,
    },
    /// `admin-dns-domain-auto-renew[row]` — turn hands-off certificate renewal
    /// on/off. Default is **on** for every managed/delegated domain, so the admin
    /// only ever touches this to disable it (`tls-certificates.md` § C.3).
    ToggleDnsAutoRenew {
        domain: String,
        enabled: bool,
    },
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Exhaustive with no fallback arm,
    /// so a new admin gesture must answer the offline question.
    ///
    /// **This is where `OnlineOnly` concentrates**, as the charter's class-3
    /// sentence predicts ("auth ceremonies, admin/provisioning"): every
    /// mutation of *deployment* state — the DAV/mail enables, the policy
    /// full-PUTs, user admission and eviction, invite minting, the nest knobs,
    /// bridge approval, the domain plane, the seed rotation, the factory reset
    /// — desensitizes together the moment there is no nest.
    ///
    /// **The one class that does NOT, and the reason it is worth stating:** a
    /// slice of `admin-dns` writes the *admin's own* `fauna.state.dns` document
    /// rather than deployment state — held DNS-provider credentials, the
    /// per-domain managed opt-in, the auto-renew opt-out, the CNAME renewal
    /// delegation, and the manual-issuance breadcrumb. Those all end in
    /// `fauna.account.state.put`, which the shared table classifies **`OfflineSafe`**
    /// (a CAS-retry-mergeable write of a document this admin owns), so they
    /// stay live. Reading the whole admin plane as online-only would grey them
    /// on a guess, which is the failure the rulings exist to prevent — the gate
    /// must never over-claim.
    ///
    /// **Trace `Op::run`, not the doc comment.** Several arms below are decided
    /// by a call two layers down: the DAV/mail/bridge/forwarder/DNS gestures go
    /// through a shared state machine whose *action* picks the kind, and the
    /// cert gestures split on a flag the gesture carries. The read-mutate-
    /// rewrite shape the events leg met recurs here — a gesture that opens with
    /// a read is decided by the **write** it ends in.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // ── admin-calendar / -contacts / -files: the three DAV enables ───
            Action::ToggleCaldavEnabled => Some("fauna.bridges.set_caldav_enabled"),
            Action::SaveCaldavPort => Some("fauna.bridges.set_caldav_port"),
            Action::ToggleCarddavEnabled => Some("fauna.bridges.set_carddav_enabled"),
            Action::ToggleWebdavEnabled => Some("fauna.bridges.set_webdav_enabled"),

            // ── admin-nest ──────────────────────────────────────────────────
            // The pairing policy is one `services.update` row (name `pairing`).
            Action::TogglePairing => Some("fauna.admin.services.update"),
            Action::SaveServingPort => Some("fauna.admin.set_serving_port"),
            // Declare and withdraw are the same kind — withdrawal is the
            // absent case on the wire (`region: None`), not a second call.
            Action::SaveRegion | Action::WithdrawRegion => Some("fauna.admin.region.set"),
            // Picking a radio is local; the save is the one kind.
            Action::SelectWebAppOrigin(_) => None,
            Action::SaveWebAppOrigin => Some("fauna.admin.web_app_origin.set"),
            // The admin tier's feature document: save and remove are one
            // `OnlineOnly` kind (removal is the absent policy). Opening,
            // On/Off and cancelling are local.
            Action::SaveFeatureLimit | Action::RemoveFeatureLimit => {
                Some("fauna.features.policy.update")
            }
            Action::OpenFeatureLimitEditor(_)
            | Action::FeatureLimitOn(_)
            | Action::CancelFeatureLimitEditor => None,
            // `AdminNatModeMachine::submit` signs and commits the selection.
            Action::SaveNatMode => Some("fauna.setup.nat_mode"),
            Action::RestartHost => Some("fauna.admin.request_host_restart"),
            // The pre-dispatch `fauna.account.get` and the claim-code mint are
            // a read and a LOCAL store write; the reset itself is the kind.
            Action::ConfirmFactoryReset => Some("fauna.admin.factory_reset"),
            // Arming the rotate confirm reads the admin roster to fill its
            // listing — a `Read`, which the gate declines to decide on, so this
            // stays live. Declared anyway (the search-page precedent): it
            // records which kind runs here, so a later reclassification reaches
            // this page for free. The best-effort `users.get` per member that
            // decorates the roster with handles is not the op's kind.
            Action::OpenSeedRotateConfirm => Some("fauna.admin.admins.list"),

            // ── admin-custody-hosting ───────────────────────────────────────
            // Arming is local. The remove is the kind; the re-read that follows
            // it is a `Read` the gate declines to decide on, so the write is
            // what this declares.
            Action::ConfirmCustodyHostingRemove => Some("fauna.admin.custody_hosting.remove"),
            Action::ConfirmSeedRotate => Some("fauna.admin.deployment_seed.rotate"),
            Action::ConfirmTakedown => Some("fauna.moderation.legal_takedown"),
            Action::ResolveReport { .. } => Some("fauna.moderation.abuse_report.resolve"),
            // The issuer's controls: the ordinary arm dispatches on the press;
            // each forced arm on its confirm, whose gesture carries the arm so
            // the declared kind is the one that runs. Spelled as LITERALS, not
            // `arm.kind()`: this body is the cross-app offline-gate
            // oracle, and a
            // computed kind would hide both forced arms from the check that
            // holds the other six apps to gating them.
            // `admin_deployment_mutations_are_online_only` pins each arm to the
            // same string `IssuerForcedArm::kind` answers.
            Action::RotateIssuerKey => Some("fauna.oauth.rotate_issuer_key"),
            Action::ConfirmOauthForced(IssuerForcedArm::IssuerKey) => {
                Some("fauna.oauth.force_rotate_issuer_key")
            }
            Action::ConfirmOauthForced(IssuerForcedArm::SessionSecret) => {
                Some("fauna.oauth.force_rotate_session_secret")
            }

            // ── admin-bridges ───────────────────────────────────────────────
            Action::ApproveBridge { .. } => Some("fauna.bridges.approve_pending_bridge"),
            Action::RejectBridge { .. } => Some("fauna.bridges.reject_pending_bridge"),
            // Rotating a service-user key IS a revoke — the bridge exits, the
            // supervisor restarts it, and the fresh key re-enrolls.
            Action::ConfirmRotate => Some("fauna.bridges.revoke_service_user"),

            // ── admin-aliases ───────────────────────────────────────────────
            Action::CreateForwarder => Some("fauna.bridges.create_forwarder"),
            Action::DeleteForwarder { .. } => Some("fauna.bridges.delete_forwarder"),

            // ── admin-settings (Tiers + membership designation) ─────────────
            Action::SaveTier { .. } => Some("fauna.admin.tiers.update"),
            Action::AddTier => Some("fauna.admin.tiers.create"),
            Action::SaveMembership { .. } => Some("fauna.admin.membership_tiers.set"),
            Action::ClearMembership { .. } => Some("fauna.admin.membership_tiers.clear"),

            // ── admin-mail ──────────────────────────────────────────────────
            // The two deployment-wide toggles dispatch immediately; the six
            // group saves each full-PUT their own policy sub-struct.
            Action::ToggleMailEnabled => Some("fauna.bridges.set_mail_enabled"),
            Action::ToggleMailAutoEnable => {
                Some("fauna.bridges.set_auto_enable_mail_for_new_users")
            }
            Action::SaveMailSpam => Some("fauna.bridges.put_spam_policy"),
            Action::SaveMailAuth => Some("fauna.bridges.put_auth_policy"),
            Action::SaveMailSubmission => Some("fauna.bridges.put_submission_policy"),
            Action::SaveMailImap => Some("fauna.bridges.put_imap_policy"),
            Action::SaveMailOutbound => Some("fauna.bridges.put_outbound_policy"),
            Action::SaveMailAlias => Some("fauna.bridges.put_alias_policy"),
            Action::PublishSpamBaseline => Some("fauna.bridges.publish_spam_baseline"),
            Action::ToggleMailBaselineStanding => Some("fauna.bridges.put_spam_policy"),
            // The recheck runs two kinds and re-reads; the self-check is its
            // first (and the one an offline nest would refuse first).
            Action::RecheckMailHealth => Some("fauna.bridges.blocklist_self_check_run"),
            // Named for the confirm; the arming press is local (it dispatches nothing).
            Action::ResetMailWarmup => Some("fauna.bridges.outbound_warmup_reset"),

            // ── admin-users ─────────────────────────────────────────────────
            // The tier IS the quota, so a tier change is `users_update`.
            Action::SetUserTier { .. } => Some("fauna.admin.users.update"),
            Action::EvictUser { .. } => Some("fauna.admin.users.evict"),
            Action::SuspendUser { .. } => Some("fauna.admin.users.suspend"),
            Action::CancelUserEviction { .. } => Some("fauna.admin.users.cancel_eviction"),
            Action::MakeAdmin { .. } => Some("fauna.admin.admins.add"),
            Action::RemoveAdmin { .. } => Some("fauna.admin.admins.remove"),
            Action::ApprovePending { .. } => Some("fauna.pending_actions.approve"),
            Action::CancelPending { .. } => Some("fauna.pending_actions.cancel"),
            Action::SaveRegistration => Some("fauna.admin.set_registration_mode"),
            Action::AdmitUser => Some("fauna.admin.users.create"),
            Action::ConfirmInvite => Some("fauna.admin.invite_codes.create"),
            Action::DeleteInvite { .. } => Some("fauna.admin.invite_codes.delete"),
            Action::ApproveRequest { .. } => Some("fauna.admin.invite_requests.approve"),
            Action::DenyRequest { .. } => Some("fauna.admin.invite_requests.deny"),

            // ── admin-web ───────────────────────────────────────────────────
            // The picker commits immediately (no local draft), and designate
            // and clear are the same kind — `actor: None` is the clear.
            Action::SelectApexActor(_) => Some("fauna.web.set_apex_actor"),

            // ── admin-dns: the nest-side domain plane ───────────────────────
            Action::SubmitDnsAddDomain => Some("fauna.bridges.add_local_domain"),
            Action::RemoveDnsDomain { .. } => Some("fauna.bridges.remove_local_domain"),
            Action::RestoreDnsDomain { .. } => Some("fauna.bridges.restore_local_domain"),
            Action::SelectDnsCatchAll { .. } => Some("fauna.bridges.set_catch_all_actor"),
            Action::SelectDnsRoleAddress { .. } => Some("fauna.bridges.set_role_address"),
            Action::SubmitDnsRename => Some("fauna.bridges.start_primary_domain_rename"),
            Action::ConfirmDnsRenameComplete => {
                Some("fauna.bridges.complete_primary_domain_rename")
            }
            Action::ExtendDnsRenameGrace => {
                Some("fauna.bridges.extend_primary_domain_rename_grace")
            }
            Action::ConfirmDnsRenameAbort => Some("fauna.bridges.abort_primary_domain_rename"),

            // ── admin-dns: delivering an issued certificate to the nest ─────
            // The managed/delegated path runs the whole DNS-01 order and ends
            // by delivering the cert (`deliver_issued_cert`); the manual path's
            // *phase 2* does the same once the admin has pasted the TXT. The
            // ACME round-trips are with the CA, not the nest — the kind is the
            // delivery, which is what actually needs this nest.
            Action::IssueDnsCert {
                single_issue: true, ..
            }
            | Action::CompleteDnsManualIssue => Some("fauna.tls.publish_cert"),

            // ── admin-dns: the admin's OWN config document ──────────────────
            // All of these load-mutate-save `fauna.state.dns` and nothing else:
            // the held provider credentials, the per-domain managed opt-in (and
            // its whole-deployment sweep, which is N of the same write), the
            // auto-renew opt-out, the CNAME renewal delegation, and the
            // manual-issuance breadcrumb. `fauna.account.state.put` is `OfflineSafe`,
            // so they stay live — see the method doc.
            //
            // `IssueDnsCert { single_issue: false }` belongs here rather than
            // above because manual *phase 1* only opens the CA order and stashes
            // the breadcrumb; nothing reaches the nest but reads.
            Action::ToggleDnsDomainMode { .. }
            | Action::ToggleDnsManageAll { .. }
            | Action::SubmitDnsCredential
            | Action::ClearDnsCredential { .. }
            | Action::ToggleDnsAutoRenew { .. }
            | Action::SubmitDnsDelegate
            | Action::RemoveDnsDelegation { .. }
            | Action::CancelDnsManualIssue
            | Action::IssueDnsCert {
                single_issue: false,
                ..
            } => Some("fauna.account.state.put"),

            // ── Local by construction ───────────────────────────────────────
            // Navigation and the page loads it triggers. `Open` and `RefreshDns`
            // each fire a whole page's read set (three or five kinds, all
            // `Read`), so there is no single kind to name — and gating them
            // would strand an admin on whatever sub-page they were on when the
            // connection dropped, with a page they can still read. The two
            // pagination steps are the same read set at another offset.
            Action::Open(_)
            | Action::RefreshDns
            | Action::UsersNextPage
            | Action::UsersPrevPage => None,

            // Arming and disarming an inline confirm, and revealing or hiding
            // an inline form. Nothing dispatches until the confirm/submit,
            // which is declared above.
            Action::OpenFactoryResetConfirm
            | Action::OpenCustodyHostingRemoveConfirm { .. }
            | Action::CancelCustodyHostingRemove
            | Action::CancelSeedRotate
            | Action::SetTakedownConversation(_)
            | Action::ToggleTakedownRestore
            | Action::OpenTakedownConfirm
            | Action::CancelTakedown
            | Action::OpenReportTakedown { .. }
            | Action::OpenOauthForcedConfirm(_)
            | Action::CancelOauthForced
            | Action::OpenRotateConfirm { .. }
            | Action::CancelRotate
            | Action::OpenInviteForm
            | Action::CancelInviteForm
            | Action::OpenDnsAddDomain
            | Action::CancelDnsAddDomain
            | Action::OpenDnsAddCredential
            | Action::CancelDnsAddCredential
            | Action::OpenDnsRenameSheet { .. }
            | Action::CancelDnsRenameSheet
            | Action::OpenDnsRenameCompleteConfirm
            | Action::OpenDnsRenameAbortConfirm
            | Action::OpenDnsDelegate { .. }
            | Action::CancelDnsDelegate => None,

            // Form drafts: a picker or toggle whose value is read by the Save
            // beside it, never dispatched on change. The three membership
            // selects, the in-group mail toggles and dropdowns, the
            // registration mode, the invite and per-request tier/guardian
            // picks, the credential provider, and the two rename/delegate
            // targets all live here.
            Action::SelectNatMode(_)
            | Action::SelectForwarderDomain(_)
            | Action::SetMembershipTierName { .. }
            | Action::SetMembershipAdminTier { .. }
            | Action::SetMembershipLapseTier { .. }
            | Action::ToggleMail(_)
            | Action::SetFcrdnsMode(_)
            | Action::SetImapDelete(_)
            | Action::SetRegistrationMode(_)
            | Action::SetAgeVerificationRequired(_)
            | Action::SetAdmitTier(_)
            | Action::SetInviteTier(_)
            | Action::SetInviteGuardian(_)
            | Action::SetInviteAgeBand(_)
            | Action::SetRequestTier { .. }
            | Action::SetRequestGuardian { .. }
            | Action::SetRequestAgeBand { .. }
            | Action::SelectDnsCredentialProvider(_)
            | Action::SelectDnsRenameTarget(_)
            | Action::SelectDnsDelegateZone(_) => None,

            // Client-side only: the log-severity filter narrows the ALREADY
            // fetched ring (no refetch), and the four copy buttons write to
            // the terminal clipboard over OSC 52.
            Action::SetLogLevel(_)
            | Action::CopyLogs
            | Action::CopyMintedCode
            | Action::CopyDnsRecord { .. }
            | Action::CopyMailDelistUrl { .. } => None,
        }
    }
}

/// The synchronous local half of an admin gesture; the returned [`Op`] is the
/// network half (`tui.md` § The page-module contract). Toggles are
/// **non-optimistic** (the current value is read from the snapshot and the write
/// is awaited, so the `state` attr flips only once the nest confirms), mirroring
/// the mail serve-here toggle.
pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    match action {
        Action::Open(page) => {
            app.admin.sub = page;
            subpage_load_op(&app.admin, page)
        }
        Action::ToggleCaldavEnabled => {
            let enabled = !app
                .admin
                .caldav_snapshot
                .as_ref()
                .map(|s| s.caldav_enabled)
                .unwrap_or(false);
            let machine = app.admin.caldav.clone()?;
            Some(Op::Caldav {
                machine,
                action: CaldavPolicyAction::SetCaldavEnabled { enabled },
            })
        }
        Action::SaveCaldavPort => {
            match fauna_core::format::parse_port(&app.admin.caldav_port_input) {
                // `admin.md` § 8 Calendar — validation, mirroring linux's client-side
                // check that routes the message to `error-message`.
                Some(port) => {
                    let machine = app.admin.caldav.clone()?;
                    Some(Op::Caldav {
                        machine,
                        action: CaldavPolicyAction::SetCaldavPort { port },
                    })
                }
                None => {
                    app.errors.insert(
                        Page::Admin,
                        t::calendar_page::CALDAV_PORT_INVALID.to_string(),
                    );
                    None
                }
            }
        }
        Action::ToggleCarddavEnabled => {
            let enabled = !app
                .admin
                .carddav_snapshot
                .as_ref()
                .map(|s| s.carddav_enabled)
                .unwrap_or(false);
            let machine = app.admin.carddav.clone()?;
            Some(Op::Carddav {
                machine,
                action: CarddavPolicyAction::SetCarddavEnabled { enabled },
            })
        }
        Action::ToggleWebdavEnabled => {
            let enabled = !app
                .admin
                .webdav_snapshot
                .as_ref()
                .map(|s| s.webdav_enabled)
                .unwrap_or(false);
            let machine = app.admin.webdav.clone()?;
            Some(Op::Webdav {
                machine,
                action: WebdavPolicyAction::SetWebdavEnabled { enabled },
            })
        }
        Action::TogglePairing => {
            let enabled = !app
                .admin
                .nest_snapshot
                .as_ref()
                .map(|s| s.pairing_enabled)
                .unwrap_or(false);
            let nest = app.admin.nest.clone()?;
            Some(Op::TogglePairing { nest, enabled })
        }
        Action::SaveServingPort => {
            match fauna_core::format::parse_port(&app.admin.serving_port_input) {
                // Mirrors linux's client-side check (`admin.rs::save`) that routes
                // the message to `error-message`.
                Some(port) => {
                    let nest = app.admin.nest.clone()?;
                    Some(Op::SaveServingPort { nest, port })
                }
                None => {
                    app.errors
                        .insert(Page::Admin, t::nest_page::SERVING_PORT_INVALID.to_string());
                    None
                }
            }
        }
        Action::SaveRegion => {
            // Client-side validation routes the refusal to `error-message` with no
            // dispatch — the serving-port shape. The nest would refuse a malformed
            // code too; asking first is what keeps the admin's typo from reading
            // like a nest failure.
            match fauna_client_admin::parse_region_code(&app.admin.region_input) {
                Ok(region) => {
                    let nest = app.admin.nest.clone()?;
                    Some(Op::SaveRegion {
                        nest,
                        region: Some(region),
                    })
                }
                // The shared validator owns WHICH refusal this is (it returns the
                // i18n key `admin.nest_page.region_invalid`); this renders that
                // same key through the generated constant, the serving-port shape.
                Err(_) => {
                    app.errors
                        .insert(Page::Admin, t::nest_page::REGION_INVALID.to_string());
                    None
                }
            }
        }
        Action::WithdrawRegion => {
            let nest = app.admin.nest.clone()?;
            Some(Op::SaveRegion { nest, region: None })
        }
        Action::SelectWebAppOrigin(mode) => {
            // Only where the shared view says the choice can be set — a nest
            // predating it (or naming a mode this build cannot) keeps both
            // radios unmarked, and a local select must not pretend otherwise.
            let settable = app
                .admin
                .nest_snapshot
                .as_ref()
                .is_some_and(|s| s.web_app_origin.can_set);
            if settable {
                app.admin.web_app_origin_draft = Some(mode);
            }
            None
        }
        Action::SaveWebAppOrigin => {
            let settable = app
                .admin
                .nest_snapshot
                .as_ref()
                .is_some_and(|s| s.web_app_origin.can_set);
            let mode = app.admin.web_app_origin_draft.filter(|_| settable)?;
            let nest = app.admin.nest.clone()?;
            Some(Op::SaveWebAppOrigin { nest, mode })
        }
        Action::OpenFeatureLimitEditor(feature) => {
            app.admin.feature_editor =
                match app.admin.nest_snapshot.as_ref().map(|s| &s.feature_limits) {
                    Some(FeatureLimitsRead::Ready(surface)) => {
                        surface.editor(fauna_client_features::AuthoringTier::Admin, &feature)
                    }
                    _ => None,
                };
            app.admin.feature_editor_status = None;
            None
        }
        Action::FeatureLimitOn(on) => {
            if let Some(editor) = app.admin.feature_editor.as_mut() {
                editor.set_on(on);
            }
            None
        }
        Action::CancelFeatureLimitEditor => {
            app.admin.feature_editor = None;
            app.admin.feature_editor_status = None;
            None
        }
        // The draft is parsed HERE, synchronously, through the shared seam: a
        // cell it cannot read goes to `error-message` and nothing is dispatched
        // (the serving-port shape; § Authoring surfaces, *Refusals and failure*).
        Action::SaveFeatureLimit | Action::RemoveFeatureLimit => {
            let remove = matches!(action, Action::RemoveFeatureLimit);
            let editor = app.admin.feature_editor.clone()?;
            if !remove && let Err(reason) = editor.draft() {
                app.errors
                    .insert(Page::Admin, reason.resolve(fauna_i18n::strings::lookup));
                return None;
            }
            let nest = app.admin.nest.clone()?;
            Some(Op::WriteFeatureLimit {
                nest,
                editor: Box::new(editor),
                remove,
            })
        }
        Action::SelectNatMode(mode) => {
            // Local-only (no network): mutate the shared machine and re-snapshot
            // so the radios repaint — the wizard's `SelectNatMode` shape.
            if let Some(nat) = app.admin.nat.as_ref() {
                nat.select(mode);
                app.admin.nat_snapshot = Some(nat.snapshot());
            }
            None
        }
        Action::SaveNatMode => {
            let nat = app.admin.nat.clone()?;
            Some(Op::NatSubmit { nat })
        }
        Action::RestartHost => {
            let nest = app.admin.nest.clone()?;
            Some(Op::RestartHost { nest })
        }
        Action::OpenFactoryResetConfirm => {
            app.admin.factory_reset_confirming = true;
            None
        }
        Action::ConfirmFactoryReset => {
            let nest = app.admin.nest.clone()?;
            let nest_url = app.admin.nest_url.clone();
            // The mint-and-persist seam writes the claim code to the long-term
            // store BEFORE the reset dispatches (CR-1 recoverability), so build
            // the persistence handle here, where `app` is in hand.
            let persistence = crate::session::launch_persistence(app);
            // Read the cached handle here too, while `app` is in hand — it is the
            // fallback when the pre-reset `fauna.account.get` cannot answer (see
            // the field's doc comment; linux reads its own cache at the same
            // point, before the spawn).
            let cached_handle = crate::session::stored_account(app)
                .map(|(_, _, handle)| handle)
                .unwrap_or_default();
            Some(Op::FactoryReset {
                nest,
                nest_url,
                persistence,
                cached_handle,
            })
        }
        Action::OpenSeedRotateConfirm => {
            let nest = app.admin.nest.clone()?;
            // Arm into `Loading` *first*, so the confirm exists (disabled) from
            // the same frame the button was pressed — tui has no modal, and a
            // confirm that appeared only once the network answered would read as
            // a dead button under load.
            app.admin.seed_rotate_confirm = Some(SeedRotateConfirm::Loading);
            app.admin.seed_rotate_status = None;
            Some(Op::LoadSeedRotateRoster { nest })
        }
        Action::CancelSeedRotate => {
            app.admin.seed_rotate_confirm = None;
            None
        }
        Action::OpenCustodyHostingRemoveConfirm {
            host_actor_id,
            grant_id,
        } => {
            // The `(host, grant)` pair is carried whole rather than a painted
            // index: the fold re-orders rows heaviest-first, so an index armed
            // against one surface would name a different row on the next read.
            app.admin.custody_hosting_confirm = Some((host_actor_id, grant_id));
            if let Some(snapshot) = app.admin.custody_hosting.as_mut() {
                snapshot.status = None;
            }
            None
        }
        Action::CancelCustodyHostingRemove => {
            app.admin.custody_hosting_confirm = None;
            None
        }
        Action::ConfirmCustodyHostingRemove => {
            // Disarm FIRST (the seed-rotate / bridges-rotate discipline): a
            // double press must not dispatch a second remove, which — after the
            // first dropped the row — would land on whatever row the re-read
            // put in its place.
            let (host_actor_id, grant_id) = app.admin.custody_hosting_confirm.take()?;
            let nest = app.admin.nest.clone()?;
            Some(Op::RemoveCustodyHosting {
                nest,
                host_actor_id,
                grant_id,
            })
        }
        Action::ConfirmSeedRotate => {
            // Disarm FIRST (the bridges-rotate discipline): a double click must
            // not dispatch a second ceremony, which would chain a *second*
            // rotation onto the first and strand a successor nobody marked.
            let armed = app.admin.seed_rotate_confirm.take()?;
            let SeedRotateConfirm::Ready(view) = armed else {
                // Re-arm: the roster never answered, so there is nothing to
                // confirm against and the admin keeps the surface they had.
                app.admin.seed_rotate_confirm = Some(armed);
                return None;
            };
            if !view.can_confirm {
                app.admin.seed_rotate_confirm = Some(SeedRotateConfirm::Ready(view));
                return None;
            }
            let nest = app.admin.nest.clone()?;
            // The account-store handle, read here while `app` is in hand: the
            // ceremony writes the successor's custody row through it before it
            // dispatches.
            let store = app.settings.account_store.clone();
            app.admin.seed_rotate_status = Some(t::nest_page::ROTATE_SEED_WORKING.to_string());
            Some(Op::RotateDeploymentSeed { nest, store })
        }
        Action::SetTakedownConversation(conversation) => {
            app.admin.takedown_conversation = conversation;
            None
        }
        Action::ToggleTakedownRestore => {
            app.admin.takedown_restore = !app.admin.takedown_restore;
            None
        }
        Action::OpenTakedownConfirm => {
            let form = TakedownForm {
                content_id: app.admin.takedown_content_id.clone(),
                content_type: if app.admin.takedown_conversation {
                    TakedownContentType::Conversation
                } else {
                    TakedownContentType::Post
                },
                legal_reference: app.admin.takedown_reference.clone(),
                restore: app.admin.takedown_restore,
            };
            let view = takedown_form_view(&form);
            if !view.can_submit {
                // The button renders disabled with the fold's stated reason; a
                // driver-forced press must not arm what the guard refuses.
                return None;
            }
            app.admin.takedown_status = None;
            app.admin.takedown_confirm = Some(ArmedTakedown { form, view });
            None
        }
        Action::CancelTakedown => {
            app.admin.takedown_confirm = None;
            None
        }
        Action::ConfirmTakedown => {
            // Disarm FIRST (the seed-rotate discipline): a double click must not
            // dispatch a second compulsory act.
            let armed = app.admin.takedown_confirm.take()?;
            let nest = app.admin.nest.clone()?;
            app.admin.takedown_status = Some(t::nest_page::TAKEDOWN_WORKING.to_string());
            Some(Op::SubmitTakedown {
                nest,
                form: armed.form,
            })
        }
        Action::OpenReportTakedown { report_id } => {
            let entry = app
                .admin
                .reports
                .iter()
                .find(|r| r.report_id == report_id)?;
            let form = fauna_client_moderation::report::takedown_prefill(&entry.subject)?;
            // A fresh draft: the id and kind the report names, no citation
            // (the arm stays disabled until the admin types one), take-down
            // not restore, and any armed confirm disarmed — it named a
            // different content.
            app.admin.takedown_content_id = form.content_id;
            app.admin.takedown_conversation =
                form.content_type == TakedownContentType::Conversation;
            app.admin.takedown_reference = String::new();
            app.admin.takedown_restore = false;
            app.admin.takedown_confirm = None;
            app.admin.takedown_status = None;
            None
        }
        Action::ResolveReport { report_id, outcome } => {
            app.admin.reports_status = None;
            Some(Op::ResolveReport {
                nest: app.admin.nest.clone()?,
                report_id,
                outcome,
            })
        }
        Action::RotateIssuerKey => {
            // The button renders disabled until the key set has answered and
            // while a call is in flight; a driver-forced press must honour the
            // same two guards rather than chain a second rotation.
            if app.admin.oauth_in_flight || oauth_keys(&app.admin).is_none() {
                return None;
            }
            // A forced confirm armed beside it named a key count this rotation
            // is about to change — disarm it rather than let it state a stale
            // cost (on the press itself, like every other disarm here).
            app.admin.oauth_confirm = None;
            let nest = app.admin.nest.clone()?;
            app.admin.oauth_in_flight = true;
            app.admin.oauth_status = Some(t::nest_page::OAUTH_WORKING.to_string());
            Some(Op::RotateIssuerKey { nest })
        }
        Action::OpenOauthForcedConfirm(arm) => {
            // Same two guards as the ordinary arm: the confirm names what it
            // drops, so it cannot be armed before the set has answered.
            if app.admin.oauth_in_flight {
                return None;
            }
            let view = oauth_keys(&app.admin)?;
            let confirm = fauna_client_admin::issuer_forced_confirm_view(arm, view);
            app.admin.oauth_status = None;
            app.admin.oauth_confirm = Some(ArmedOauthForced { arm, view: confirm });
            None
        }
        Action::CancelOauthForced => {
            app.admin.oauth_confirm = None;
            None
        }
        Action::ConfirmOauthForced(arm) => {
            // Disarm FIRST (the seed-rotate discipline): a double press must not
            // dispatch a second forced rotation — the second would drop the very
            // key the first minted.
            let armed = app.admin.oauth_confirm.take()?;
            if armed.arm != arm || app.admin.oauth_in_flight {
                // A confirm painted for one arm, pressed after the other was
                // armed: dispatch nothing, and keep what the admin can see.
                app.admin.oauth_confirm = Some(armed);
                return None;
            }
            let nest = app.admin.nest.clone()?;
            app.admin.oauth_in_flight = true;
            app.admin.oauth_status = Some(t::nest_page::OAUTH_WORKING.to_string());
            Some(Op::OauthForced { nest, arm })
        }
        Action::SelectForwarderDomain(domain) => {
            // Local-only: record the picked hosted domain; the create reads it.
            app.admin.forwarder_add_domain = domain;
            None
        }
        Action::ApproveBridge { pubkey_hex, role } => {
            let machine = app.admin.bridges.clone()?;
            Some(Op::Bridges {
                machine,
                action: BridgeApprovalAction::Approve { pubkey_hex, role },
            })
        }
        Action::RejectBridge { pubkey_hex } => {
            let machine = app.admin.bridges.clone()?;
            Some(Op::Bridges {
                machine,
                action: BridgeApprovalAction::Reject { pubkey_hex },
            })
        }
        Action::OpenRotateConfirm { pubkey_hex } => {
            app.admin.rotate_confirm = Some(RotateConfirm { pubkey_hex });
            None
        }
        Action::ConfirmRotate => {
            // Disarm first: the confirm is consumed by the dispatch, so a second
            // click can't re-fire a revoke against a pubkey already rotated.
            let confirm = app.admin.rotate_confirm.take()?;
            let machine = app.admin.bridges.clone()?;
            Some(Op::Bridges {
                machine,
                action: BridgeApprovalAction::Rotate {
                    pubkey_hex: confirm.pubkey_hex,
                },
            })
        }
        Action::CancelRotate => {
            app.admin.rotate_confirm = None;
            None
        }
        Action::SetLogLevel(index) => {
            app.admin.log_filter = index;
            None
        }
        Action::CopyLogs => {
            // `filtered_nest_entries` is already newest-first, and `rendered_text`
            // reverses what it is given — so hand it the oldest-first order it
            // expects, matching the Settings page's payload exactly.
            let mut entries = logs::filtered_nest_entries(&app.admin);
            entries.reverse();
            let text = fauna_log::format::rendered_text(
                &entries,
                crate::settings::logs::local_offset_secs(),
            );
            crate::wizard::copy_to_clipboard(&text);
            None
        }
        Action::SelectApexActor(label) => {
            // Resolve the picked LABEL against the snapshot that painted it, so
            // the gesture never carries a positional index. An unknown label (or
            // the fallback option, which already names the current designation)
            // resolves to nothing to do — never a silent clear.
            let snapshot = app.admin.web_snapshot.as_ref()?;
            let actor = match resolve_apex_label(snapshot, &label)? {
                ApexPick::Clear => None,
                ApexPick::Designate(id) => Some(id),
            };
            if actor == snapshot.current {
                return None;
            }
            let nest = app.admin.nest.clone()?;
            Some(Op::SetApexActor { nest, actor })
        }
        Action::CreateForwarder => {
            let machine = app.admin.forwarders.clone()?;
            Some(Op::Forwarders {
                machine,
                action: ForwarderAction::Create {
                    local_domain: app.admin.forwarder_add_domain.trim().to_string(),
                    pattern: app.admin.forwarder_add_pattern.trim().to_string(),
                    forward_target: app.admin.forwarder_add_target.trim().to_string(),
                },
            })
        }
        Action::DeleteForwarder { alias_id_hex } => {
            let machine = app.admin.forwarders.clone()?;
            Some(Op::Forwarders {
                machine,
                action: ForwarderAction::Delete { alias_id_hex },
            })
        }
        Action::SaveTier { row } => match tier_update_req(&app.admin, row) {
            // A parsed request → dispatch the update (needs the live client).
            Some(req) => app
                .admin
                .nest
                .clone()
                .map(|nest| Op::UpdateTier { nest, req }),
            // Any unparseable/empty cap draft surfaces the shared "failed to save
            // tier" message on the globally-registered `error-message` and
            // dispatches nothing (the port-input validation precedent — the nest
            // never sees a malformed request).
            None => {
                app.errors.insert(
                    Page::Admin,
                    t::settings_page::SAVE_TIER_ERROR_INVALID_CAP.to_string(),
                );
                None
            }
        },
        Action::AddTier => match tier_create_req(&app.admin) {
            Ok(req) => app
                .admin
                .nest
                .clone()
                .map(|nest| Op::CreateTier { nest, req }),
            // The two LOCAL refusals name the rule and the control (Copy
            // comprehensibility); nothing is dispatched, the nest never sees a
            // malformed request. A taken name is the NEST's refusal and arrives
            // as `Outcome::Failed` below.
            Err(refusal) => {
                app.errors.insert(Page::Admin, refusal.to_string());
                None
            }
        },
        // The three membership-row selects are LOCAL drafts — Save reads them
        // directly, nothing dispatches on change (the tier-cap-input precedent).
        Action::SetMembershipTierName { row, tier_name } => {
            if let Some(draft) = app.admin.membership_drafts.get_mut(row) {
                draft.tier_name = tier_name;
            }
            None
        }
        Action::SetMembershipAdminTier { row, tier } => {
            if let Some(draft) = app.admin.membership_drafts.get_mut(row) {
                draft.admin_tier = tier;
            }
            None
        }
        Action::SetMembershipLapseTier { row, tier } => {
            if let Some(draft) = app.admin.membership_drafts.get_mut(row) {
                draft.lapse_tier = tier;
            }
            None
        }
        Action::SaveMembership { row } => match membership_save_req(&app.admin, row) {
            Some(req) => app
                .admin
                .nest
                .clone()
                .map(|nest| Op::SaveMembership { nest, req }),
            // An empty admitted-tier draft (no quota tiers exist yet, or the row
            // somehow lost its selection) — same shared-error shape as `SaveTier`.
            None => {
                app.errors.insert(
                    Page::Admin,
                    t::settings_page::SAVE_MEMBERSHIP_TIER_ERROR_NO_TIER.to_string(),
                );
                None
            }
        },
        Action::ClearMembership { row } => {
            let tier_name = app.admin.membership_drafts.get(row)?.tier_name.clone();
            app.admin
                .nest
                .clone()
                .map(|nest| Op::ClearMembership { nest, tier_name })
        }
        // The two deployment-wide toggles dispatch immediately (non-optimistic:
        // the current value is read from the persisted snapshot, and `state` flips
        // only after the write + re-read) — they are NOT gathered into a group PUT.
        Action::ToggleMailEnabled => {
            let machine = app.admin.mail.clone()?;
            let enabled = !mail_snapshot_bool(app, |s| s.mail_enabled);
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::SetMailEnabled { enabled },
            })
        }
        Action::ToggleMailAutoEnable => {
            let machine = app.admin.mail.clone()?;
            let enabled = !mail_snapshot_bool(app, |s| s.auto_enable_mail_for_new_users);
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::SetAutoEnableMailForNewUsers { enabled },
            })
        }
        // In-group edits are local drafts, gathered on Save (no network here).
        Action::ToggleMail(toggle) => {
            app.admin.mail_drafts.toggle(toggle);
            None
        }
        Action::SetFcrdnsMode(mode) => {
            app.admin.mail_drafts.spam.fcrdns_mode = mode;
            None
        }
        Action::SetImapDelete(mode) => {
            app.admin.mail_drafts.imap.delete_nonempty = mode;
            None
        }
        // Each Save gathers its group's drafts into the full `*View` (seeded from
        // the persisted snapshot, so unedited fields ride through unchanged — the
        // no-clobber full-PUT) and dispatches the group's put. Needs both the live
        // machine and a persisted snapshot to seed the gather from.
        Action::SaveMailSpam => {
            let machine = app.admin.mail.clone()?;
            let snapshot = app.admin.mail_snapshot.as_ref()?;
            let policy = app.admin.mail_drafts.gather_spam(&snapshot.spam);
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::SaveSpam { policy },
            })
        }
        Action::SaveMailAuth => {
            let machine = app.admin.mail.clone()?;
            let snapshot = app.admin.mail_snapshot.as_ref()?;
            let policy = app.admin.mail_drafts.gather_auth(&snapshot.auth);
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::SaveAuth { policy },
            })
        }
        Action::SaveMailSubmission => {
            let machine = app.admin.mail.clone()?;
            let snapshot = app.admin.mail_snapshot.as_ref()?;
            let policy = app
                .admin
                .mail_drafts
                .gather_submission(&snapshot.submission);
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::SaveSubmission { policy },
            })
        }
        Action::SaveMailImap => {
            let machine = app.admin.mail.clone()?;
            let snapshot = app.admin.mail_snapshot.as_ref()?;
            let policy = app.admin.mail_drafts.gather_imap(&snapshot.imap);
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::SaveImap { policy },
            })
        }
        Action::SaveMailOutbound => {
            let machine = app.admin.mail.clone()?;
            let snapshot = app.admin.mail_snapshot.as_ref()?;
            let policy = app.admin.mail_drafts.gather_outbound(&snapshot.outbound);
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::SaveOutbound { policy },
            })
        }
        Action::SaveMailAlias => {
            let machine = app.admin.mail.clone()?;
            let snapshot = app.admin.mail_snapshot.as_ref()?;
            let policy = app.admin.mail_drafts.gather_alias(&snapshot.alias);
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::SaveAlias { policy },
            })
        }
        Action::PublishSpamBaseline => {
            let machine = app.admin.mail.clone()?;
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::PublishSpamBaseline,
            })
        }
        Action::ToggleMailBaselineStanding => {
            let machine = app.admin.mail.clone()?;
            let enabled = !mail_snapshot_bool(app, |s| s.spam.baseline_standing_publish);
            Some(Op::Mail {
                machine,
                action: MailPolicyAction::SetBaselineStandingPublish { enabled },
            })
        }
        Action::RecheckMailHealth => Some(Op::Mail {
            machine: app.admin.mail.clone()?,
            action: MailPolicyAction::RecheckHealth,
        }),
        Action::ResetMailWarmup => {
            // The two-click inline confirm (the `MailSpamReset` shape): arming is
            // local, and only the confirm reaches the machine — disarming first,
            // so a third press cannot chain a second reset.
            if !app.admin.mail_warmup_reset_armed {
                app.admin.mail_warmup_reset_armed = true;
                return None;
            }
            app.admin.mail_warmup_reset_armed = false;
            Some(Op::Mail {
                machine: app.admin.mail.clone()?,
                action: MailPolicyAction::ResetWarmup,
            })
        }
        Action::CopyMailDelistUrl { url } => {
            crate::wizard::copy_to_clipboard(&url);
            None
        }
        // The tier IS the quota (`admin.md` § 2) — a tier change is `users_update`
        // carrying the (unchanged) label. LIVE + re-read so the picker's `get_text`
        // reads back the target once persisted (the e2e's `set_user_tier` contract).
        Action::SetUserTier { row, tier } => {
            mutate_users_op(app, user_change_tier_mutation(&app.admin.users, row, tier)?)
        }
        Action::EvictUser { row } => mutate_users_op(
            app,
            UsersMutation::Evict {
                actor: user_actor_at(&app.admin.users, row)?,
            },
        ),
        Action::SuspendUser { row } => mutate_users_op(
            app,
            UsersMutation::Suspend {
                actor: user_actor_at(&app.admin.users, row)?,
            },
        ),
        Action::CancelUserEviction { row } => mutate_users_op(
            app,
            UsersMutation::CancelEviction {
                actor: user_actor_at(&app.admin.users, row)?,
            },
        ),
        Action::MakeAdmin { row } => mutate_users_op(
            app,
            UsersMutation::AddAdmin {
                actor: user_actor_at(&app.admin.users, row)?,
            },
        ),
        Action::RemoveAdmin { row } => mutate_users_op(
            app,
            UsersMutation::RemoveAdmin {
                actor: user_actor_at(&app.admin.users, row)?,
            },
        ),
        Action::ApprovePending { id } => mutate_users_op(app, UsersMutation::ApproveAction { id }),
        Action::CancelPending { id } => mutate_users_op(app, UsersMutation::CancelAction { id }),
        Action::UsersNextPage => users_page_op(app, next_users_offset(&app.admin.users)),
        Action::UsersPrevPage => users_page_op(app, prev_users_offset(&app.admin.users)),
        // ── Registration (§ 2) — LOCAL drafts + one combined save ──
        Action::SetRegistrationMode(mode) => {
            app.admin.users.registration_mode_draft = mode;
            None
        }
        Action::SetAgeVerificationRequired(required) => {
            app.admin.users.age_verification_draft = required;
            None
        }
        Action::SaveRegistration => match registration_mutation(&app.admin.users) {
            Ok(mutation) => mutate_users_op(app, mutation),
            Err(hint) => {
                // A non-numeric ceiling is a user error — surface it on the page-scoped
                // action error and dispatch nothing (linux's `MAX_FREE_USERS_HINT`).
                app.admin.users.action_error = Some(hint);
                None
            }
        },
        // ── Admit (public-mode.md § Registration & Identity) — LOCAL drafts +
        //    one admit ──
        Action::SetAdmitTier(tier) => {
            app.admin.users.admit_tier_draft = tier;
            None
        }
        Action::AdmitUser => match admit_mutation(&app.admin.users) {
            Ok(mutation) => mutate_users_op(app, mutation),
            Err(hint) => {
                // A malformed actor id is a user error — page-scoped, nothing
                // dispatched (the nest would refuse it anyway; failing local
                // keeps the message actionable).
                app.admin.users.action_error = Some(hint);
                None
            }
        },
        // ── Invite (§ 3) — LOCAL form drafts + mint/copy/delete ──
        Action::SetInviteTier(tier) => {
            app.admin.users.invite_tier_draft = tier;
            None
        }
        Action::SetInviteGuardian(guardian) => {
            // A band presupposes a guardian: clearing one clears the other, so
            // a stale band can never ride a now-unsupervised mint.
            if !guardian_draft_names_someone(&guardian) {
                app.admin.users.invite_age_band_draft = AGE_BAND_NOT_SET_VALUE.to_string();
            }
            app.admin.users.invite_guardian_draft = guardian;
            None
        }
        Action::SetInviteAgeBand(band) => {
            app.admin.users.invite_age_band_draft = band;
            None
        }
        Action::OpenInviteForm => {
            app.admin.users.invite_form_open = true;
            None
        }
        Action::CancelInviteForm => {
            app.admin.users.invite_form_open = false;
            app.admin.users.minted_code = None;
            None
        }
        Action::ConfirmInvite => mutate_users_op(app, create_invite_mutation(&app.admin.users)),
        Action::CopyMintedCode => {
            if let Some(code) = app.admin.users.minted_code.as_deref() {
                crate::wizard::copy_to_clipboard(code);
            }
            None
        }
        Action::DeleteInvite { code } => mutate_users_op(app, UsersMutation::DeleteInvite { code }),
        // ── Pending requests (§ 1) — LOCAL per-row drafts + approve/deny ──
        Action::SetRequestTier { row, tier } => {
            if let Some(draft) = app.admin.users.request_tier_drafts.get_mut(row) {
                *draft = tier;
            }
            None
        }
        Action::SetRequestGuardian { row, guardian } => {
            // A band presupposes a guardian (the invite form's rule, per row).
            if let Some(band) = app
                .admin
                .users
                .request_age_band_drafts
                .get_mut(row)
                .filter(|_| !guardian_draft_names_someone(&guardian))
            {
                *band = AGE_BAND_NOT_SET_VALUE.to_string();
            }
            if let Some(draft) = app.admin.users.request_guardian_drafts.get_mut(row) {
                *draft = guardian;
            }
            None
        }
        Action::SetRequestAgeBand { row, band } => {
            if let Some(draft) = app.admin.users.request_age_band_drafts.get_mut(row) {
                *draft = band;
            }
            None
        }
        Action::ApproveRequest { row } => {
            mutate_users_op(app, approve_request_mutation(&app.admin.users, row)?)
        }
        Action::DenyRequest { row } => {
            mutate_users_op(app, deny_request_mutation(&app.admin.users, row)?)
        }
        // ── admin-dns ──────────────────────────────────────────────────────────
        // Refresh is the page's own load op, so the button and the nav edge run
        // exactly the same three reads.
        Action::RefreshDns => subpage_load_op(&app.admin, AdminPage::Dns),
        Action::OpenDnsAddDomain => {
            app.admin.dns_add_domain_open = true;
            None
        }
        Action::CancelDnsAddDomain => {
            app.admin.dns_add_domain_open = false;
            None
        }
        Action::SubmitDnsAddDomain => {
            let domain = app.admin.dns_add_domain_input.trim().to_string();
            if domain.is_empty() {
                return None;
            }
            // Close the form and clear the draft optimistically — the row either
            // appears or the nest's refusal lands on `error-message`; either way
            // the admin is not left staring at a stale half-typed form.
            app.admin.dns_add_domain_open = false;
            app.admin.dns_add_domain_input.clear();
            local_domains_op(
                &app.admin,
                LocalDomainAction::AddDomain {
                    domain,
                    // The shared default, the same one linux passes — the
                    // add-domain form carries no MTA-STS knobs, and the policy
                    // mode is the nest's own to set and advance.
                    mta_sts_cert_mode: DEFAULT_CERT_MODE.to_string(),
                },
            )
        }
        Action::RemoveDnsDomain { domain } => {
            local_domains_op(&app.admin, LocalDomainAction::RemoveDomain { domain })
        }
        Action::RestoreDnsDomain { domain } => {
            local_domains_op(&app.admin, LocalDomainAction::RestoreDomain { domain })
        }
        Action::ToggleDnsDomainMode { domain, managed } => {
            dns_op(&app.admin, DnsAction::SetMode { domain, managed })
        }
        Action::ToggleDnsManageAll { managed } => {
            // The master switch is "set every active domain's mode at once"; the
            // stored state stays per-domain (`dns-management.md` § The two modes).
            // One dispatch per domain would need N ops, so the whole sweep is one
            // op that walks the rows (see `Op::DnsManageAll`).
            let domains: Vec<String> = app
                .admin
                .local_domains_snapshot
                .as_ref()
                .map(|s| s.active.iter().map(|d| d.domain.clone()).collect())
                .unwrap_or_default();
            app.admin.dns.clone().map(|machine| Op::DnsManageAll {
                machine,
                domains,
                managed,
            })
        }
        Action::SelectDnsCatchAll { row, label } => {
            let domain = dns_row_domain(&app.admin, row)?;
            let actor_id = resolve_dns_actor(&app.admin, &label, t::dns::CATCH_ALL_NONE);
            local_domains_op(
                &app.admin,
                LocalDomainAction::SetCatchAllActor { domain, actor_id },
            )
        }
        Action::SelectDnsRoleAddress { row, role, label } => {
            let domain = dns_row_domain(&app.admin, row)?;
            let actor_id =
                resolve_dns_actor(&app.admin, &label, t::dns::ROLE_ADDRESS_ADMIN_DEFAULT);
            local_domains_op(
                &app.admin,
                LocalDomainAction::SetRoleAddress {
                    domain,
                    role,
                    actor_id,
                },
            )
        }
        Action::OpenDnsAddCredential => {
            app.admin.dns_add_credential_open = true;
            None
        }
        Action::CancelDnsAddCredential => {
            // Drop the provider pick AND every typed field: a secret must not
            // linger behind a closed write-only form.
            app.admin.dns_add_credential_open = false;
            app.admin.dns_credential_provider = None;
            app.admin.dns_credential_fields.clear();
            None
        }
        Action::SelectDnsCredentialProvider(provider_id) => {
            // A token typed for one provider means nothing to another, so the
            // drafts clear with the pick (linux's `rebuild_credential_fields`).
            app.admin.dns_credential_fields.clear();
            app.admin.dns_credential_provider = Some(provider_id);
            None
        }
        Action::SubmitDnsCredential => {
            let provider_id = app.admin.dns_credential_provider.clone()?;
            let fields: Vec<fauna_client_dns::DnsCredentialField> =
                dns::credential_field_ids(&provider_id)
                    .into_iter()
                    .map(|id| fauna_client_dns::DnsCredentialField {
                        value: fauna_client_dns::SecretString::new(
                            app.admin
                                .dns_credential_fields
                                .get(&id)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                        id,
                    })
                    .collect();
            // Any field left blank ⇒ skip the guaranteed-failing provider
            // round-trip and keep the form open so the admin can finish (linux's
            // submit guard).
            if fields.is_empty() || fields.iter().any(|f| f.value.as_str().trim().is_empty()) {
                return None;
            }
            let op = dns_op(
                &app.admin,
                DnsAction::PutCredentials {
                    label: provider_id.clone(),
                    provider_id,
                    fields,
                },
            );
            app.admin.dns_add_credential_open = false;
            app.admin.dns_credential_provider = None;
            app.admin.dns_credential_fields.clear();
            op
        }
        Action::CopyDnsRecord { value } => {
            crate::wizard::copy_to_clipboard(&value);
            None
        }
        Action::ClearDnsCredential { index } => {
            dns_op(&app.admin, DnsAction::ClearCredentials { index })
        }
        Action::OpenDnsRenameSheet { target } => {
            app.admin.dns_rename_sheet_open = true;
            // A promote-button pre-targets its own row; the primary row's rename
            // button leaves the re-seeded default in place.
            if let Some(target) = target {
                app.admin.dns_rename_target = target;
            }
            None
        }
        Action::CancelDnsRenameSheet => {
            app.admin.dns_rename_sheet_open = false;
            None
        }
        Action::SelectDnsRenameTarget(domain) => {
            app.admin.dns_rename_target = domain;
            None
        }
        Action::SubmitDnsRename => {
            // Resolve the picked domain NAME back to its 16-byte `domain_id`
            // against the snapshot that painted the picker, so no positional index
            // crosses the gesture door.
            let snapshot = app.admin.local_domains_snapshot.as_ref()?;
            let new_primary_domain_id = snapshot
                .active
                .iter()
                .find(|d| !d.is_primary && d.domain == app.admin.dns_rename_target)
                .map(|d| d.domain_id.clone())?;
            // Blank ⇒ the nest's default 7 days; a non-numeric draft likewise
            // (the nest validates the [1, 30] range it does get).
            let grace_days = app.admin.dns_rename_grace_days.trim().parse::<i64>().ok();
            app.admin.dns_rename_sheet_open = false;
            local_domains_op(
                &app.admin,
                LocalDomainAction::StartPrimaryRename {
                    new_primary_domain_id,
                    grace_days,
                },
            )
        }
        Action::OpenDnsRenameCompleteConfirm => {
            app.admin.dns_rename_completing = true;
            None
        }
        Action::ConfirmDnsRenameComplete => {
            let rename = active_rename(&app.admin)?;
            // Force only when the grace window has NOT elapsed — the shared view's
            // own `can_force_complete` flag decides, never a re-derived clock read.
            let force = rename.can_force_complete;
            let rename_id = rename.rename_id.clone();
            app.admin.dns_rename_completing = false;
            local_domains_op(
                &app.admin,
                LocalDomainAction::CompletePrimaryRename { rename_id, force },
            )
        }
        Action::ExtendDnsRenameGrace => {
            let rename_id = active_rename(&app.admin)?.rename_id.clone();
            let additional_days = app
                .admin
                .dns_rename_extend_days
                .trim()
                .parse::<i64>()
                .unwrap_or(DEFAULT_RENAME_EXTEND_DAYS);
            local_domains_op(
                &app.admin,
                LocalDomainAction::ExtendPrimaryRenameGrace {
                    rename_id,
                    additional_days,
                },
            )
        }
        Action::OpenDnsRenameAbortConfirm => {
            app.admin.dns_rename_aborting = true;
            None
        }
        Action::ConfirmDnsRenameAbort => {
            let rename_id = active_rename(&app.admin)?.rename_id.clone();
            app.admin.dns_rename_aborting = false;
            local_domains_op(
                &app.admin,
                LocalDomainAction::AbortPrimaryRename {
                    rename_id,
                    reason: None,
                },
            )
        }
        // ── The per-domain TLS-cert lifecycle ─────────────────────────────────
        Action::IssueDnsCert {
            domain,
            single_issue,
        } => match (app.admin.dns.clone(), app.admin.nest.clone()) {
            (Some(machine), Some(nest)) => Some(Op::DnsIssueCert {
                machine,
                nest,
                domain,
                single_issue,
            }),
            _ => None,
        },
        Action::CompleteDnsManualIssue => dns_op(&app.admin, DnsAction::CompleteManualIssueCert),
        Action::CancelDnsManualIssue => dns_op(&app.admin, DnsAction::CancelManualIssueCert),
        Action::OpenDnsDelegate { domain } => {
            // Seed the zone pick to the first zone a held credential covers — the
            // only zones `DelegateRenewal` accepts, so the picker always offers a
            // submittable value.
            app.admin.dns_delegate_zone = credential_zones(&app.admin)
                .first()
                .cloned()
                .unwrap_or_default();
            app.admin.dns_delegate_open = Some(domain);
            None
        }
        Action::CancelDnsDelegate => {
            app.admin.dns_delegate_open = None;
            None
        }
        Action::SelectDnsDelegateZone(zone) => {
            app.admin.dns_delegate_zone = zone;
            None
        }
        Action::SubmitDnsDelegate => {
            let domain = app.admin.dns_delegate_open.take()?;
            let target_zone = app.admin.dns_delegate_zone.clone();
            if target_zone.is_empty() {
                return None;
            }
            dns_op(
                &app.admin,
                DnsAction::DelegateRenewal {
                    domain,
                    target_zone,
                },
            )
        }
        Action::RemoveDnsDelegation { domain } => {
            dns_op(&app.admin, DnsAction::RemoveDelegation { domain })
        }
        Action::ToggleDnsAutoRenew { domain, enabled } => {
            dns_op(&app.admin, DnsAction::SetAutoRenew { domain, enabled })
        }
    }
}

/// Every zone the admin's held credentials cover, sorted + deduped — the
/// `admin-dns-cert-delegate-zone-select` option set (`DelegateRenewal` validates
/// that a held credential covers the chosen zone, so offering anything else would
/// be offering a guaranteed rejection). Empty ⇒ the delegate affordance is
/// disabled.
pub(super) fn credential_zones(state: &AdminState) -> Vec<String> {
    let mut zones: Vec<String> = state
        .dns_snapshot
        .as_ref()
        .map(|s| s.credentials.iter().flat_map(|c| c.zones.clone()).collect())
        .unwrap_or_default();
    zones.sort();
    zones.dedup();
    zones
}

/// How many days `admin-dns-rename-extend-button` extends the grace window by
/// when the input is blank — the nest's own default window, so the common case
/// needs no typing.
const DEFAULT_RENAME_EXTEND_DAYS: i64 = 7;

/// Wrap one `LocalDomainMachine` dispatch as an [`Op`], or `None` pre-login.
fn local_domains_op(state: &AdminState, action: LocalDomainAction) -> Option<Op> {
    state
        .local_domains
        .clone()
        .map(|machine| Op::LocalDomains { machine, action })
}

/// Wrap one `DnsManagementMachine` dispatch as an [`Op`], or `None` when the
/// machine could not be built (an impossible keypair derive — see [`init`]).
fn dns_op(state: &AdminState, action: DnsAction) -> Option<Op> {
    state.dns.clone().map(|machine| Op::Dns { machine, action })
}

/// The active-domain row `row` names, from the snapshot that painted it.
fn dns_row_domain(state: &AdminState, row: usize) -> Option<String> {
    state
        .local_domains_snapshot
        .as_ref()?
        .active
        .get(row)
        .map(|d| d.domain.clone())
}

/// The in-flight primary-domain rename, if any.
fn active_rename(
    state: &AdminState,
) -> Option<&fauna_client_mail_settings::primary_domain_rename::PrimaryDomainRenameView> {
    state
        .local_domains_snapshot
        .as_ref()?
        .active_rename
        .as_ref()
}

/// Resolve an actor-picker **option string** ([`picker_option`] — handle, falling
/// back to actor hex; NEVER the raw editable label, `admin.md` § 2) back to the
/// wire actor id, against the same actor list the picker's options came from.
/// The picker's own "clear" sentinel (`clear_label` — the localized "None" /
/// "Admin (default)"), an empty string, and an option no listed actor carries (a
/// stale pick after a re-read dropped that user) all mean *clear* — never a
/// half-resolved guess. The nest re-validates the actor it does get.
fn resolve_dns_actor(state: &AdminState, label: &str, clear_label: &str) -> Option<Vec<u8>> {
    if label.is_empty() || label == clear_label {
        return None;
    }
    state
        .dns_actors
        .iter()
        .find(|(_, l)| l == label)
        .map(|(id, _)| id.clone())
}

/// The sentinel draft value for "no guardian" in a guardian picker (family-safety)
/// — the localized "None" option both admission surfaces default to, exactly the
/// label linux's `GuardianSelect` puts at index 0. A LABEL picker round-trips it as
/// the option value, and [`resolve_guardian`] maps it (and an empty draft) to `None`.
pub(super) const GUARDIAN_NONE_VALUE: &str = t::users_page::GUARDIAN_NONE;

/// The option string ANY admin pick-a-user control offers for `user` — the
/// **handle** (`admin.md` § 2 → *What identifies a user in an admin picker*:
/// unique on the nest by construction, where the display label is freely
/// editable and non-unique — two accounts labelled `Alex` would resolve
/// first-match), falling back to the full actor hex for a handle-less account
/// (the admit form's blank handle). The option string IS
/// every string-round-tripping picker's resolve value (guardian, DNS
/// catch-all/role-address, web apex), so it must stay injective (handles are
/// unique; the hex is unique and never empty) — one *local* name so the
/// three resolvers can never drift apart again, delegating to the one
/// cross-app owner, `fauna_client_admin::admin_picker_option` (linux calls it
/// directly, android/web reach it over FFI/wasm).
pub(super) fn picker_option(user: &AdminUser) -> String {
    fauna_client_admin::admin_picker_option(user)
}

/// Resolve a guardian-picker draft — a **handle**, or a handle-less account's
/// actor hex ([`picker_option`]; `admin.md` § 2 → *What identifies a
/// user in an admin picker*) — to the wire `guardian_actor` (raw bytes), by
/// looking it up in the same non-suspended-account list the picker's options
/// came from: every account on the nest ([`UsersSnapshot::picker_users`]), never
/// the Users page on screen. Injective by construction: handles are unique on
/// the nest, the hex is unique, and a display label (editable, non-unique) is
/// never matched.
///
/// The `none` sentinel, an empty draft, and an option no listed user carries (a
/// stale draft after a re-read dropped/suspended that user) all mean no guardian
/// — never a half-resolved guess. The nest re-validates the actor it does get
/// (exists / not suspended / not itself supervised / ≠ the admitted actor).
fn resolve_guardian(snapshot: &UsersSnapshot, draft: &str) -> Option<Vec<u8>> {
    if draft.is_empty() || draft == GUARDIAN_NONE_VALUE {
        return None;
    }
    snapshot
        .picker_users
        .iter()
        .filter(|u| !u.suspended)
        .find(|u| picker_option(u) == draft)
        .map(|u| u.actor_id.to_vec())
}

/// Build the registration-save mutation from the drafts: the picked mode (which the
/// picker only offers as a known wire value) + the parsed free-tier ceiling (blank ⇒
/// no cap). A non-numeric ceiling is a user error (`Err(hint)`), never dispatched.
fn registration_mutation(users: &UsersState) -> Result<UsersMutation, String> {
    let mode = RegistrationMode::from_wire_str(&users.registration_mode_draft)
        .ok_or_else(|| t::users_page::REGISTRATION_MODE_LABEL.to_string())?;
    let raw = users.max_free_users_input.trim();
    let max_free = if raw.is_empty() {
        None
    } else {
        Some(
            raw.parse::<u64>()
                .map_err(|_| t::users_page::MAX_FREE_USERS_HINT.to_string())?,
        )
    };
    // The age require-knob rides the same save, but only when it changed — an
    // unchanged knob sends nothing (`family-safety.md` § App surface →
    // *Age-band surfaces*: one gesture, no half-saved section).
    let persisted = users
        .snapshot
        .as_ref()
        .map(|s| s.age_verification_required)
        .unwrap_or(false);
    let age_verification =
        (users.age_verification_draft != persisted).then_some(users.age_verification_draft);
    Ok(UsersMutation::SaveRegistration {
        mode,
        max_free,
        age_verification,
    })
}

/// Build the direct-admit mutation from the Admit-section drafts: the actor id
/// (must be exactly 64 hex chars — a malformed id is a user error, never
/// dispatched), the drafted tier, and the trimmed handle (blank ⇒ `None`, the
/// deliberate handle-less admission — `public-mode.md` § A handle-less
/// account; the nest vets a present handle with the same rigour as the other
/// two account-creation paths).
fn admit_mutation(users: &UsersState) -> Result<UsersMutation, String> {
    let raw = users.admit_actor_input.trim();
    let actor = (raw.len() == 64)
        .then(|| {
            (0..raw.len())
                .step_by(2)
                // `.get` (not a slice) — a multi-byte char both fails the
                // parse AND cannot panic on a non-boundary index.
                .map(|i| u8::from_str_radix(raw.get(i..i + 2)?, 16).ok())
                .collect::<Option<Vec<u8>>>()
        })
        .flatten()
        .ok_or_else(|| t::users_page::ADMIT_ACTOR_HINT.to_string())?;
    let handle = Some(users.admit_handle_input.trim().to_string()).filter(|h| !h.is_empty());
    Ok(UsersMutation::AdmitUser {
        actor,
        tier: users.admit_tier_draft.clone(),
        handle,
    })
}

/// Build the mint mutation from the invite-form drafts: an empty code (⇒ the nest
/// mints), the drafted tier + guardian, and the max-uses (defaulting to 1, the
/// minimum, on a blank/unparseable entry — linux's `parse.max(1)`).
fn create_invite_mutation(users: &UsersState) -> UsersMutation {
    let uses = users
        .invite_uses_input
        .trim()
        .parse::<i64>()
        .unwrap_or(1)
        .max(1);
    let guardian = users
        .snapshot
        .as_ref()
        .and_then(|s| resolve_guardian(s, &users.invite_guardian_draft));
    // The band rides only beside a guardian (the nest refuses it otherwise;
    // the select is disabled without one, and this is the belt to that brace).
    let age_band = guardian
        .as_ref()
        .and_then(|_| age_band_from_option_value(&users.invite_age_band_draft));
    UsersMutation::CreateInvite {
        tier: users.invite_tier_draft.clone(),
        uses,
        guardian,
        age_band,
    }
}

/// Build the approve mutation for the `row`-th pending request: its id + the drafted
/// approve-at tier + guardian. `None` if the row is stale (a re-read shrank the list).
fn approve_request_mutation(users: &UsersState, row: usize) -> Option<UsersMutation> {
    let snapshot = users.snapshot.as_ref()?;
    let id = snapshot.invite_requests.get(row)?.id;
    let tier = users.request_tier_drafts.get(row).cloned();
    let guardian = users
        .request_guardian_drafts
        .get(row)
        .and_then(|d| resolve_guardian(snapshot, d));
    let age_band = guardian.as_ref().and_then(|_| {
        users
            .request_age_band_drafts
            .get(row)
            .and_then(|d| age_band_from_option_value(d))
    });
    Some(UsersMutation::ApproveRequest {
        id,
        tier,
        guardian,
        age_band,
    })
}

/// Build the deny mutation for the `row`-th pending request: its id + the (optional)
/// drafted reason (a blank reason is `None`). `None` if the row is stale.
fn deny_request_mutation(users: &UsersState, row: usize) -> Option<UsersMutation> {
    let id = users.snapshot.as_ref()?.invite_requests.get(row)?.id;
    let reason = users
        .request_deny_reasons
        .get(row)
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .map(str::to_string);
    Some(UsersMutation::DenyRequest { id, reason })
}

/// The `users_list` page size (`admin.md` § 2 — pagination is `limit`/`offset`).
/// Matches the nest's own default so `total` / offset math lines up with linux.
pub(super) const USERS_PAGE_SIZE: i64 = 50;

/// The raw actor id of the `row`-th user in the current page, or `None` if the
/// snapshot/row is absent (a stale click after a re-read shrank the page).
fn user_actor_at(users: &UsersState, row: usize) -> Option<Vec<u8>> {
    Some(
        users
            .snapshot
            .as_ref()?
            .users
            .get(row)?
            .actor_id
            .as_ref()
            .to_vec(),
    )
}

/// Build the change-tier mutation for the `row`-th user: `users_update` carrying the
/// (unchanged) label, since the tier IS the quota. Pure — the network wrapping is
/// [`mutate_users_op`]'s job — so the resolution is unit-testable without a client.
fn user_change_tier_mutation(
    users: &UsersState,
    row: usize,
    tier: String,
) -> Option<UsersMutation> {
    let user = users.snapshot.as_ref()?.users.get(row)?;
    Some(UsersMutation::ChangeTier {
        actor: user.actor_id.as_ref().to_vec(),
        tier,
        label: user.label.clone(),
    })
}

/// The offset one page forward, or `None` at the last page. Delegates to the
/// shared `fauna_core::format::next_page_offset` stepper (the harvested
/// sibling of `total_pages`/`current_page`, `admin/users.rs`).
/// Pure guard logic — the click side-effect is [`users_page_op`]'s.
fn next_users_offset(users: &UsersState) -> Option<i64> {
    let total = users.snapshot.as_ref().map(|s| s.total).unwrap_or(0);
    fauna_core::format::next_page_offset(users.offset, total, USERS_PAGE_SIZE)
}

/// The offset one page back, or `None` at page 1. Delegates to the shared
/// `fauna_core::format::prev_page_offset` stepper.
fn prev_users_offset(users: &UsersState) -> Option<i64> {
    fauna_core::format::prev_page_offset(users.offset, USERS_PAGE_SIZE)
}

/// Build a users-hub mutation op carrying the live client + the current page offset
/// (preserved so the post-mutation re-read stays on the same page). `None` pre-login.
fn mutate_users_op(app: &App, mutation: UsersMutation) -> Option<Op> {
    let nest = app.admin.nest.clone()?;
    Some(Op::MutateUsers {
        nest,
        offset: app.admin.users.offset,
        mutation,
    })
}

/// Move to a new page: store the offset (so a repaint before the re-read lands shows
/// the target page's indicator) and dispatch the re-read. A `None` target (a guarded
/// boundary click) is a no-op. `None` client pre-login.
fn users_page_op(app: &mut App, new_offset: Option<i64>) -> Option<Op> {
    let new_offset = new_offset?;
    app.admin.users.offset = new_offset;
    let nest = app.admin.nest.clone()?;
    Some(Op::LoadUsers {
        nest,
        offset: new_offset,
    })
}

/// Read a bool off the persisted mail snapshot (the non-optimistic toggle reads
/// its current value from persisted state, defaulting off pre-hydrate).
fn mail_snapshot_bool(app: &App, pick: impl Fn(&MailPolicySnapshot) -> bool) -> bool {
    app.admin.mail_snapshot.as_ref().map(pick).unwrap_or(false)
}

/// Build the `tiers.update` request for the `row`-th tier from its cap drafts, or
/// `None` if the row is gone or any of its five raw-i64 caps fails to parse. Pure
/// (no `app`, no network) so the parse/validation is unit-testable without a live
/// client. The `name` identifies the row and is not editable; the tier's flattened
/// `extra` is carried through so an update never drops an unknown additive field.
fn tier_update_req(state: &AdminState, row: usize) -> Option<AdminTierUpdateRequest> {
    let tier = state.tiers.as_ref()?.get(row)?;
    let drafts = state.tier_cap_drafts.get(row)?;
    // The shared tier-cap validator (`value-formatting.md` § Tier cap
    // validation), not a bare `parse::<i64>()`: it trims, accepts a leading `+`,
    // rejects fractional/out-of-range — and clamps a **negative** to `0`, which
    // a bare parse accepts, so this page used to send a negative allowance to
    // the nest on a row that looked valid.
    //
    // ⚠ Deliberate divergence from the section's `parse_cap(text).unwrap_or(prev)`
    // consume shape, and this app's is the richer half: `?` here abandons the
    // whole save and `apply_local` paints `SAVE_TIER_ERROR_INVALID_CAP` on
    // `error-message`, where `unwrap_or(prev)` silently reverts the bad edit with
    // no feedback at all. Both honour the section's actual invariant — never
    // silently zero a cap — but only this one tells the admin their edit was
    // rejected. A negative still clamps rather than refusing, because `0` is a
    // meaningful cap here (an explicit "no allowance"), so it is a real edit.
    let parse = |s: &str| fauna_core::format::parse_cap(s);
    Some(AdminTierUpdateRequest {
        name: tier.name.clone(),
        max_inbox_bytes: parse(&drafts.inbox)?,
        max_storage_bytes: parse(&drafts.storage)?,
        max_devices: parse(&drafts.devices)?,
        max_blob_size: parse(&drafts.blob_size)?,
        max_feeds: parse(&drafts.feeds)?,
        extra: tier.extra.clone(),
    })
}

/// Why the add form cannot be sent — both refusals are local, worded by the
/// shared strings so every app can say the same sentence.
#[derive(Debug, PartialEq, Eq)]
enum TierAddRefusal {
    EmptyName,
    InvalidCap,
}

impl std::fmt::Display for TierAddRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EmptyName => t::settings_page::ADD_TIER_ERROR_EMPTY_NAME,
            Self::InvalidCap => t::settings_page::ADD_TIER_ERROR_INVALID_CAP,
        })
    }
}

/// Build the `tiers.create` request from the add form's drafts. Pure — the
/// [`tier_update_req`] shape, same shared cap validator (`parse_cap`: trims,
/// clamps a negative to `0`, refuses fractional/empty), plus the one rule a new
/// tier adds: a name. A name already taken is the nest's call (`fauna.admin.conflict`).
fn tier_create_req(state: &AdminState) -> Result<AdminTierCreateRequest, TierAddRefusal> {
    let name = state.tier_add_name.trim();
    if name.is_empty() {
        return Err(TierAddRefusal::EmptyName);
    }
    let caps = &state.tier_add_caps;
    let parse = |s: &str| fauna_core::format::parse_cap(s).ok_or(TierAddRefusal::InvalidCap);
    Ok(AdminTierCreateRequest {
        name: name.to_string(),
        max_inbox_bytes: parse(&caps.inbox)?,
        max_storage_bytes: parse(&caps.storage)?,
        max_devices: parse(&caps.devices)?,
        max_blob_size: parse(&caps.blob_size)?,
        max_feeds: parse(&caps.feeds)?,
        extra: Default::default(),
    })
}

/// Build the `membership_tiers.set` request for the `row`-th membership row from
/// its drafts, or `None` if the row is gone or its admitted-quota-tier draft is
/// empty (nothing to designate at). Pure, unit-testable without a live client —
/// the [`tier_update_req`] shape. The lapse-tier draft is always a definite
/// selection (seeded to [`DEFAULT_LAPSE_TIER`] when undesignated), so it always
/// rides as an explicit `Some`, never relying on the wire's omit-means-default
/// (linux's `build_membership_row` comment).
fn membership_save_req(state: &AdminState, row: usize) -> Option<AdminMembershipTierSetRequest> {
    let draft = state.membership_drafts.get(row)?;
    if draft.admin_tier.is_empty() {
        return None;
    }
    Some(AdminMembershipTierSetRequest {
        tier_name: draft.tier_name.clone(),
        admin_tier: draft.admin_tier.clone(),
        lapse_tier: Some(draft.lapse_tier.clone()),
        extra: Default::default(),
    })
}

/// Read an editable admin field's current value — the automation agent's
/// `/element/type` / `get` and the keyboard's own read. Exhaustive per the `Field`
/// contract (`tui.md` § The page-module contract).
pub fn field(state: &AdminState, field: &AdminField) -> String {
    match field {
        AdminField::CaldavPort => state.caldav_port_input.clone(),
        AdminField::ServingPort => state.serving_port_input.clone(),
        AdminField::Region => state.region_input.clone(),
        AdminField::TakedownContentId => state.takedown_content_id.clone(),
        AdminField::TakedownReference => state.takedown_reference.clone(),
        AdminField::ForwarderPattern => state.forwarder_add_pattern.clone(),
        AdminField::ForwarderTarget => state.forwarder_add_target.clone(),
        AdminField::TierCap { row, cap } => state
            .tier_cap_drafts
            .get(*row)
            .map(|d| d.get(*cap).to_string())
            .unwrap_or_default(),
        AdminField::TierAddName => state.tier_add_name.clone(),
        AdminField::TierAddCap(cap) => state.tier_add_caps.get(*cap).to_string(),
        AdminField::Mail(mail_field) => state.mail_drafts.text(*mail_field),
        AdminField::MaxFreeUsers => state.users.max_free_users_input.clone(),
        AdminField::InviteMaxUses => state.users.invite_uses_input.clone(),
        AdminField::AdmitActor => state.users.admit_actor_input.clone(),
        AdminField::AdmitHandle => state.users.admit_handle_input.clone(),
        AdminField::RequestDenyReason { row } => state
            .users
            .request_deny_reasons
            .get(*row)
            .cloned()
            .unwrap_or_default(),
        AdminField::DnsAddDomain => state.dns_add_domain_input.clone(),
        AdminField::DnsCredential(id) => state
            .dns_credential_fields
            .get(id)
            .cloned()
            .unwrap_or_default(),
        AdminField::DnsRenameGraceDays => state.dns_rename_grace_days.clone(),
        AdminField::DnsRenameExtendDays => state.dns_rename_extend_days.clone(),
        AdminField::FeatureLimitCell { cell } => state
            .feature_editor
            .as_ref()
            .map(|editor| editor.cell_text(*cell).to_string())
            .unwrap_or_default(),
    }
}

/// Write an editable admin field — a keystroke into a focused input, or the
/// agent's `/element/type`. No network work implied (unlike a feed search), so it
/// returns nothing.
pub fn set_field(state: &mut AdminState, field: AdminField, value: String) {
    match field {
        AdminField::FeatureLimitCell { cell } => {
            if let Some(editor) = state.feature_editor.as_mut() {
                editor.set_cell(cell, value);
            }
        }
        AdminField::CaldavPort => state.caldav_port_input = value,
        AdminField::ServingPort => state.serving_port_input = value,
        AdminField::Region => state.region_input = value,
        AdminField::TakedownContentId => state.takedown_content_id = value,
        AdminField::TakedownReference => state.takedown_reference = value,
        AdminField::ForwarderPattern => state.forwarder_add_pattern = value,
        AdminField::ForwarderTarget => state.forwarder_add_target = value,
        AdminField::TierCap { row, cap } => {
            if let Some(drafts) = state.tier_cap_drafts.get_mut(row) {
                drafts.set(cap, value);
            }
        }
        AdminField::TierAddName => state.tier_add_name = value,
        AdminField::TierAddCap(cap) => state.tier_add_caps.set(cap, value),
        AdminField::Mail(mail_field) => state.mail_drafts.set_text(mail_field, value),
        AdminField::MaxFreeUsers => state.users.max_free_users_input = value,
        AdminField::InviteMaxUses => state.users.invite_uses_input = value,
        AdminField::AdmitActor => state.users.admit_actor_input = value,
        AdminField::AdmitHandle => state.users.admit_handle_input = value,
        AdminField::RequestDenyReason { row } => {
            if let Some(reason) = state.users.request_deny_reasons.get_mut(row) {
                *reason = value;
            }
        }
        AdminField::DnsAddDomain => state.dns_add_domain_input = value,
        // Accept a keystroke for any provider field id — the entry only paints
        // while its provider is selected, and the drafts are cleared on a
        // provider change, so an id from another provider cannot leak into a
        // submit.
        AdminField::DnsCredential(id) => {
            state.dns_credential_fields.insert(id, value);
        }
        AdminField::DnsRenameGraceDays => state.dns_rename_grace_days = value,
        AdminField::DnsRenameExtendDays => state.dns_rename_extend_days = value,
    }
}

/// The network half of the admin shell's reads. Each variant owns only `Arc`s (and
/// a plain action / bool), so it crosses a `tokio::spawn` (keyboard) and an `await`
/// (agent).
pub enum Op {
    /// The shared `fauna.account.am_i_admin` gate (`admin.md` § Shell). Its bool
    /// reveals/hides the gated `admin-tab` sidebar row — nothing else.
    CheckGate { nest: Arc<NestClient> },
    /// The dashboard's `fauna.admin.stats` + `.status` reads (`admin.md` § 1
    /// Dashboard → Data sources), through the shared `AdminClient` (no HTTP twin).
    LoadDashboard { nest: Arc<NestClient> },
    /// One `admin-calendar` machine dispatch (`Refresh` / `SetCaldavEnabled` /
    /// `SetCaldavPort`), then re-snapshot — one Op variant per machine so the whole
    /// page has one fold site (the mail-page "one `MailSnapshot`" discipline).
    Caldav {
        machine: Arc<CaldavPolicyMachine>,
        action: CaldavPolicyAction,
    },
    /// One `admin-bridges-pending` machine dispatch (`Refresh` / `Approve` /
    /// `Reject` / `Rotate`), then re-snapshot — one Op variant, one fold site
    /// (the DAV "one snapshot" discipline). The machine re-reads both feeds
    /// after a mutation, so approve/reject/rotate need no separate reload.
    Bridges {
        machine: Arc<BridgeApprovalMachine>,
        action: BridgeApprovalAction,
    },
    /// One `admin-contacts` machine dispatch, then re-snapshot.
    Carddav {
        machine: Arc<CarddavPolicyMachine>,
        action: CarddavPolicyAction,
    },
    /// One `admin-files` machine dispatch, then re-snapshot.
    Webdav {
        machine: Arc<WebdavPolicyMachine>,
        action: WebdavPolicyAction,
    },
    /// One `admin-aliases` forwarder-machine dispatch (`Refresh` / `Create` /
    /// `Delete`), then re-snapshot — one Op variant, one fold site (the DAV
    /// "one snapshot" discipline; a failed create rides `snapshot.error`).
    Forwarders {
        machine: Arc<ForwarderMachine>,
        action: ForwarderAction,
    },
    /// One `admin-mail` machine dispatch (`Refresh` / a group `Save*` / a `Set*`
    /// toggle / `PublishSpamBaseline`), then re-snapshot — one Op variant, one fold
    /// site (the DAV "one snapshot" discipline). A save/read failure rides
    /// `snapshot.error`, which `apply_outcome` bridges to `error-message`.
    Mail {
        machine: Arc<MailPolicyMachine>,
        action: MailPolicyAction,
    },
    /// The `admin-settings` (Tiers) page's whole-page read: tier definitions
    /// (`fauna.admin.tiers.list`) + the membership section's row set
    /// (`fauna.subscriptions.tiers.list`) + designations
    /// (`fauna.admin.membership_tiers.list`) — one Op, one `TiersLoaded` fold
    /// (the `LoadUsers`/`LoadNest` "one snapshot" discipline; monetization.md
    /// § Pillar 4).
    LoadTiers { nest: Arc<NestClient> },
    /// Overwrite a tier's caps (`fauna.admin.tiers.update`), then re-read the
    /// whole Tiers page so the row re-renders from persisted state (non-optimistic).
    UpdateTier {
        nest: Arc<NestClient>,
        req: AdminTierUpdateRequest,
    },
    /// Define a new tier (`fauna.admin.tiers.create`), then re-read the whole
    /// Tiers page so it appears as an ordinary row (non-optimistic).
    CreateTier {
        nest: Arc<NestClient>,
        req: AdminTierCreateRequest,
    },
    /// Designate/re-point a membership row (`fauna.admin.membership_tiers.set`,
    /// an upsert), then re-read the whole Tiers page.
    SaveMembership {
        nest: Arc<NestClient>,
        req: AdminMembershipTierSetRequest,
    },
    /// Drop a membership row's designation (`fauna.admin.membership_tiers.clear`),
    /// then re-read the whole Tiers page. The subscription tier itself is
    /// untouched — it just reverts to undesignated.
    ClearMembership {
        nest: Arc<NestClient>,
        tier_name: String,
    },
    /// The `admin-nest` full load: `services.list` (pairing) + `fauna.setup.status`
    /// (serving-port + os-maintenance) + the NAT machine's `hydrate` — one Op, one
    /// `NestLoaded` fold.
    LoadNest {
        nest: Arc<NestClient>,
        nat: Arc<AdminNatModeMachine>,
    },
    /// Flip the pairing policy (`services.update` name `pairing`), then re-read the
    /// reflective snapshot (non-optimistic — the toggle's status flips only once the
    /// nest confirms).
    TogglePairing {
        nest: Arc<NestClient>,
        enabled: bool,
    },
    /// Commit the client-facing serving port (`fauna.admin.set_serving_port`), then
    /// re-read the reflective snapshot.
    SaveServingPort { nest: Arc<NestClient>, port: u16 },
    /// Declare (`Some`) or withdraw (`None`) the deployment's region, then re-read.
    SaveRegion {
        nest: Arc<NestClient>,
        region: Option<fauna_client_admin::RegionCode>,
    },
    /// Commit the web-app origin choice, then re-read.
    SaveWebAppOrigin {
        nest: Arc<NestClient>,
        mode: fauna_client_admin::WebAppOrigin,
    },
    /// Save the admin-tier editor's draft or remove the document
    /// (`fauna.features.policy.update`), then re-read the whole reflective
    /// snapshot — the section's rows and the editor's re-seed both come from it.
    WriteFeatureLimit {
        nest: Arc<NestClient>,
        editor: Box<fauna_client_features::PolicyEditor>,
        remove: bool,
    },
    /// Sign + commit the selected NAT mode (`AdminNatModeMachine::submit` →
    /// `fauna.setup.nat_mode`), then re-snapshot the machine.
    NatSubmit { nat: Arc<AdminNatModeMachine> },
    /// Request a host restart (`fauna.admin.request_host_restart`), then re-read the
    /// reflective snapshot.
    RestartHost { nest: Arc<NestClient> },
    /// The danger-zone reset: fetch the authoritative handle, mint+persist the
    /// post-reset claim code (CR-1), dispatch `fauna.admin.factory_reset`, then
    /// signal the re-onboard transition.
    FactoryReset {
        nest: Arc<NestClient>,
        nest_url: String,
        persistence: RegistryLaunchPersistence,
        /// The registry's cached handle, read at click time as the **fallback**
        /// for the authoritative `fauna.account.get` lookup below.
        ///
        /// Load-bearing, not belt-and-braces: a handle is REQUIRED to claim a
        /// nest (a handle-less admin is rejected — `common.md` § Client-state
        /// recoverability, the claim type-promotion), and the CR-1 mint persists
        /// whatever handle it is given without validating it
        /// (`mint_and_persist_pending_factory_reset` verifies only that the
        /// claim code round-tripped). So sourcing the handle from the live
        /// session ALONE strands the client whenever that lookup cannot answer:
        /// against an unreachable nest the slot is persisted with an empty
        /// handle, the launch machine honors it, pre-fills the claim page — and
        /// the re-claim then fails with "a handle is required to claim the
        /// nest", with no in-app exit. That is the trap the invariant forbids.
        ///
        /// linux has had this fallback all along
        /// (`apps/fauna-linux/src/client.rs::factory_reset`, `cached_handle`),
        /// and it is the same fix apple needed (`common.md` § *How apple got
        /// there* — ask the live session first, keep the cache as the fallback);
        /// tui had adopted only the first half.
        cached_handle: String,
    },
    /// Read the current admin roster for the armed rotation confirm
    /// (`fauna.admin.admins.list`, plus a best-effort `users.get` per member for
    /// the handle labels). Read-only: nothing is rotated by arming.
    LoadSeedRotateRoster { nest: Arc<NestClient> },
    /// Dispatch the legal-takedown console's captured form
    /// (`fauna.moderation.legal_takedown` — take down or overturn; the shared
    /// `takedown_verdict` words the outcome).
    SubmitTakedown {
        nest: Arc<NestClient>,
        form: TakedownForm,
    },
    /// Record one report's outcome, then re-read the queue (a resolved row
    /// leaves it — the observable effect).
    ResolveReport {
        nest: Arc<NestClient>,
        report_id: String,
        outcome: fauna_protocol::moderation::AbuseReportOutcome,
    },
    /// The ordinary issuer-key rotation (`fauna.oauth.rotate_issuer_key`),
    /// then a re-read of the key set so the outgoing key's countdown paints.
    RotateIssuerKey { nest: Arc<NestClient> },
    /// One forced arm (`fauna.oauth.force_rotate_issuer_key` /
    /// `force_rotate_session_secret`), then a re-read of the key set.
    OauthForced {
        nest: Arc<NestClient>,
        arm: IssuerForcedArm,
    },
    /// Drive the deployment-seed rotation ceremony on the account plane
    /// (`fauna_client_account_runtime::deployment_seeds::rotate_deployment_seed`
    /// — custody merged and published before dispatch; no fan-out, the plane
    /// carries the successor's row).
    ///
    /// This op **outlives its click** ([`crate::app::PageOp::outlives_click`]):
    /// the committed rotation tears down the box's serving generation, so the
    /// connection this runs on is dropped by WS 1001 right after the reply
    /// flushes and the drive's own marking step reconnects by design
    /// (`box-recovery.md` § Adoption by the running process). Bounding it by an
    /// agent reply budget would abandon the ceremony mid-flight, exactly where a
    /// caller most needs its verdict.
    RotateDeploymentSeed {
        nest: Arc<NestClient>,
        /// The account-store handle — the plane door the ceremony writes the
        /// successor's custody row through *before* it dispatches (the CR-1
        /// ordering). `None` before the store is ready; the shared drive
        /// decides what that permits.
        store: Option<AccountStoreHandle>,
    },
    /// The `admin-users` whole-page read (`users_list` + `tiers_list` +
    /// `invite_codes_list` + `invite_requests_list` + `fauna.setup.status`) at the
    /// given page offset — one Op, one `UsersLoaded` fold (the "one snapshot"
    /// discipline). The nav-edge load and every pagination step run this.
    LoadUsers { nest: Arc<NestClient>, offset: i64 },
    /// One `admin-users` mutation (evict / suspend / restore / tier change / …),
    /// then a re-read of the whole page at `offset` (non-optimistic — the row
    /// re-renders from persisted state). A mutation failure lands on the page-scoped
    /// `admin-users-action-error` (`Outcome::UsersActionFailed`), not `error-message`.
    MutateUsers {
        nest: Arc<NestClient>,
        offset: i64,
        mutation: UsersMutation,
    },
    /// The `admin-web` whole-page read: the apex designation
    /// (`fauna.web.get_apex_actor`) + the pickable actors
    /// (`fauna.admin.users.list`) — one Op, one `WebLoaded` fold.
    LoadWeb { nest: Arc<NestClient> },
    /// `fauna.admin.custody_hosting.list` — the nest-wide hosting registry,
    /// folded through the shared projection.
    LoadCustodyHosting { nest: Arc<NestClient> },
    /// `fauna.admin.custody_hosting.remove` for one `(host, grant)` pair, then
    /// a re-read: the removed row must leave the surface, and the remaining
    /// rows re-order (the fold sorts heaviest-hold first).
    RemoveCustodyHosting {
        nest: Arc<NestClient>,
        host_actor_id: String,
        grant_id: Vec<u8>,
    },
    /// Fetch the nest's `fauna-log` ring for `admin-logs` (`fauna.admin.logs`,
    /// Admin-class). Read-only and replay-safe, so the page re-fetches freely on
    /// every entry.
    LoadLogs { nest: Arc<NestClient> },
    /// The `admin-dns` whole-page read: the local-domain rows, the DNS record
    /// matrix + live public-DNS verdicts, and the actor list the per-row pickers
    /// offer — one Op, one `DnsLoaded` fold (a row painted before its records, or
    /// a picker before its options, is the split-load bug this avoids).
    LoadDns {
        nest: Arc<NestClient>,
        dns: Arc<DnsManagementMachine>,
        domains: Arc<LocalDomainMachine>,
    },
    /// One `admin-dns` DNS-machine dispatch (`SetMode` / `PutCredentials` /
    /// `ClearCredentials` / …), then re-snapshot — one Op variant, one fold site
    /// (the DAV "one snapshot" discipline; the machine records its own failures on
    /// `DnsSnapshot.error`, which `apply_outcome` bridges to `error-message`).
    Dns {
        machine: Arc<DnsManagementMachine>,
        action: DnsAction,
    },
    /// One `admin-dns` local-domain-machine dispatch (domain CRUD / catch-all /
    /// role address / the rename lifecycle), then re-snapshot. The machine
    /// re-reads the rows itself after every mutation, so no extra `Refresh` rides
    /// along (linux's per-action `Refresh` exists only because it rebuilds the
    /// machine each time; ours is persistent and already hydrated).
    LocalDomains {
        machine: Arc<LocalDomainMachine>,
        action: LocalDomainAction,
    },
    /// `admin-dns-cert-issue-button` — run a client-driven DNS-01 order for
    /// `domain` and deliver the issued cert to the nest that serves it
    /// (`tls-certificates.md` § B tier 2/3). Needs the `NestClient` too, because
    /// `target_nest_id` is resolved from the pairing surface at dispatch time
    /// (`LinkedNestsMachine::bound_nest_id()` — carried on the action rather than
    /// resolved inside the machine, which keeps the machine decoupled from
    /// linked-nests and uniform across all 7 apps).
    DnsIssueCert {
        machine: Arc<DnsManagementMachine>,
        nest: Arc<NestClient>,
        domain: String,
        /// The domain can auto-publish `_acme-challenge` (managed or delegated), so
        /// one dispatch finishes the order; otherwise the two-phase manual paste
        /// flow opens instead.
        single_issue: bool,
    },
    /// `admin-dns-manage-all-toggle` — sweep every active domain to `managed`
    /// (`dns-management.md` § The two modes: a deployment-level convenience over
    /// per-domain stored state, not a deployment-wide flag). One Op for the whole
    /// sweep so the page re-renders once, on the final projection.
    DnsManageAll {
        machine: Arc<DnsManagementMachine>,
        domains: Vec<String>,
        managed: bool,
    },
    /// Designate (`Some`) or clear (`None`) the apex actor
    /// (`fauna.web.set_apex_actor`), then re-read the whole page so the picker
    /// re-renders from persisted state (non-optimistic — the selection reflects
    /// the nest's echo, which is what makes reading it back a round-trip proof).
    SetApexActor {
        nest: Arc<NestClient>,
        actor: Option<Vec<u8>>,
    },
}

/// One `admin-users` write, the network half of a hub gesture. Each resolves to
/// exactly one `AdminClient` call; on success the whole page re-reads (`admin.md`
/// § Persistence — no client-side cache), on failure the page-scoped action error
/// is set. The actor ids are raw bytes (`AdminUser.actor_id`).
#[derive(Debug, Clone)]
pub enum UsersMutation {
    /// `users_evict` — start the timed eviction ladder.
    Evict { actor: Vec<u8> },
    /// `users_suspend` — suspend immediately, no delete timeline.
    Suspend { actor: Vec<u8> },
    /// `users_cancel_eviction` — restore from eviction or suspension.
    CancelEviction { actor: Vec<u8> },
    /// `admins_add` — grant the admin role (a scheduled pending action).
    AddAdmin { actor: Vec<u8> },
    /// `admins_remove` — revoke the admin role (a scheduled pending action; the
    /// nest refuses at the last superadmin).
    RemoveAdmin { actor: Vec<u8> },
    /// `pending_action_approve` — add this admin's approval to a pending action.
    ApproveAction { id: i64 },
    /// `pending_action_cancel` — call a pending admin action off.
    CancelAction { id: i64 },
    /// `users_update` — change the user's tier (= quota), carrying the unchanged label.
    ChangeTier {
        actor: Vec<u8>,
        tier: String,
        label: String,
    },
    /// `set_registration_mode` — commit the registration posture + free-tier ceiling,
    /// then `set_age_verification_required` when the knob changed (`Some`).
    SaveRegistration {
        mode: RegistrationMode,
        max_free: Option<u64>,
        age_verification: Option<bool>,
    },
    /// `users_create` — admit a known actor id directly, under a handle + tier
    /// (`public-mode.md` § Registration & Identity, the third account-creation
    /// path; `handle: None` is the deliberate handle-less admission).
    AdmitUser {
        actor: Vec<u8>,
        tier: String,
        handle: Option<String>,
    },
    /// `invite_codes_create` — mint an invite code (empty code ⇒ the nest mints); the
    /// reply's token is threaded to `admin-users-invite-code-copy-btn` via
    /// [`Outcome::InviteMinted`].
    CreateInvite {
        tier: String,
        uses: i64,
        guardian: Option<Vec<u8>>,
        /// The guardian's band dial for a supervised mint; `None` without a
        /// guardian (`family-safety.md` § The account age band, D2).
        age_band: Option<AgeBand>,
    },
    /// `invite_codes_delete` — delete an existing invite code by its token.
    DeleteInvite { code: String },
    /// `invite_requests_approve` — approve a pending request at a tier (+ guardian),
    /// admitting the requester (the cross-page invariant advances their onboarding).
    ApproveRequest {
        id: i64,
        tier: Option<String>,
        guardian: Option<Vec<u8>>,
        /// The band the request is admitted at (seeded from the claim); `None`
        /// without a guardian.
        age_band: Option<AgeBand>,
    },
    /// `invite_requests_deny` — deny a pending request with an optional reason.
    DenyRequest { id: i64, reason: Option<String> },
}

/// What an [`Op`] resolved to; folded back by [`apply_outcome`]. Derives `Debug`
/// through the protocol/snapshot types' own, so [`PageOutcome`] stays `Debug` with
/// no manual impl.
#[derive(Debug)]
pub enum Outcome {
    /// The gate result — folded onto [`App::am_i_admin`] (an app-level field, not
    /// page state: it drives the sidebar even off the admin page).
    GateLoaded { is_admin: bool },
    /// A fresh dashboard snapshot (stats + version).
    DashboardLoaded(DashboardSnapshot),
    /// A failed dashboard read — lands on the admin page's `error-message`.
    Failed(String),
    /// A tier was defined: the inner [`Outcome::TiersLoaded`] re-read, folded
    /// after the add form's drafts are cleared.
    TierAdded(Box<Outcome>),
    /// The armed rotation confirm's roster listing resolved (or failed to).
    SeedRotateRoster(Box<SeedRotateConfirm>),
    /// The rotation ceremony's verdict, already worded — the op resolves the
    /// outcome→sentence mapping where the `SeedRotation` variants are in hand,
    /// so the fold stays a one-line assignment.
    SeedRotated(String),
    /// The legal-takedown dispatch's verdict, already worded by the shared
    /// `takedown_verdict` (same one-line-fold shape as `SeedRotated`).
    TakedownDone(String),
    /// A resolve dispatched — its verdict line (the shared `resolve_verdict`)
    /// and the re-read queue.
    ReportResolved(String, ReportsRead),
    /// An issuer control's verdict (already worded by the shared fold) and the
    /// key set as re-read after it — only the section's own leg, so a rotation
    /// does not re-seed the port or region drafts the admin may be typing in.
    OauthDone { status: String, keys: OauthKeysRead },
    /// A fresh `admin-calendar` snapshot — the single fold for every CalDAV op
    /// (hydrate / toggle / port). The machine reports both success and failure on
    /// the snapshot, so there is one variant, one fold, one error-bridge site.
    CaldavSnapshot(CaldavPolicySnapshot),
    /// A fresh `admin-contacts` snapshot.
    CarddavSnapshot(CarddavPolicySnapshot),
    /// A fresh `admin-files` snapshot.
    WebdavSnapshot(WebdavPolicySnapshot),
    /// A fresh `admin-aliases` snapshot — the single fold for every forwarder op
    /// (hydrate / create / delete). The machine reports success and failure on the
    /// snapshot's `error`, surfaced on the page-scoped `admin-aliases-action-error`.
    ForwardersSnapshot(ForwardersSnapshot),
    /// A fresh `admin-mail` snapshot — the single fold for every mail op (hydrate /
    /// group save / toggle / publish-baseline). The machine reports success and
    /// failure on the snapshot's `error`, bridged to the global `error-message`.
    /// Boxed: `MailPolicySnapshot` inlines the six policy `*View`s, so an unboxed
    /// variant would balloon `Outcome` (and the `PageOutcome`/`DataMessage` chain it
    /// rides) past the `large_enum_variant` threshold.
    MailSnapshot(Box<MailPolicySnapshot>),
    /// A fresh `admin-settings` (Tiers) page snapshot — tier *definitions* plus
    /// the membership-designation section's row set + designations
    /// (monetization.md § Pillar 4). The single fold for the nav-edge load and
    /// every post-save/-clear re-read (both sections' mutations reload the whole
    /// page, the `UsersLoaded` discipline); re-seeds both sections' per-row
    /// drafts from persisted state.
    TiersLoaded {
        tiers: Vec<AdminTier>,
        own_membership_tier_names: Vec<String>,
        membership_tiers: Vec<AdminMembershipTier>,
    },
    /// The `admin-nest` full load — the reflective snapshot plus the freshly
    /// hydrated NAT snapshot. The snapshot is boxed for the `MailSnapshot`
    /// reason: it inlines the region view AND the issuer key set's read, and
    /// unboxed it tips `Outcome` (and the `PageOutcome` chain it rides) past the
    /// `large_enum_variant` threshold.
    NestLoaded {
        reflective: Box<NestPageSnapshot>,
        nat: NatModeSnapshot,
        /// The reports queue, read in the same load — its own `Result`, so a
        /// refused queue read never blanks the rest of the page.
        reports: ReportsRead,
    },
    /// A re-read of the `admin-nest` reflective snapshot only (after a pairing /
    /// serving-port / restart mutation) — the NAT machine is untouched. Boxed
    /// for the same reason as [`Outcome::NestLoaded`].
    NestReflect(Box<NestPageSnapshot>),
    /// An admin-tier feature-limit write landed: its verdict, the editor
    /// re-seeded from the fresh authored read, and the page's re-read.
    FeatureLimitWritten {
        status: String,
        editor: Box<Option<fauna_client_features::PolicyEditor>>,
        reflective: Box<NestPageSnapshot>,
    },
    /// A fresh NAT snapshot after `submit` — its own status element carries any
    /// error (never `error-message`).
    NatSnapshot(NatModeSnapshot),
    /// The factory reset dispatched and its claim code is durably persisted — tear
    /// down the session (keeping credentials) and re-onboard at the pre-filled code.
    FactoryResetComplete,
    /// A fresh `admin-users` whole-page snapshot — the single fold for the nav-edge
    /// load, every pagination step, and every post-mutation re-read; re-seeds the
    /// drafts that mirror nest state and clears the page-scoped action error. Boxed
    /// because `UsersSnapshot` inlines several `Vec`s + the wire records, so an
    /// unboxed variant would balloon `Outcome` (and the `PageOutcome`/`DataMessage`
    /// chain) past the `large_enum_variant` threshold (the `MailSnapshot` shape).
    UsersLoaded(Box<UsersSnapshot>),
    /// A failed `admin-users` mutation — lands on the page-scoped
    /// `admin-users-action-error` (NOT the app-wide `error-message`; `admin.md`
    /// § Errors), leaving the last snapshot painted.
    UsersActionFailed(String),
    /// An invite code was minted — carries the freshly returned token (revealed
    /// copyable via `admin-users-invite-code-copy-btn`) plus the re-read snapshot (so
    /// the new code appears in the list). The create form stays open showing the token.
    InviteMinted {
        code: String,
        snapshot: Box<UsersSnapshot>,
    },
    /// The `admin-web` page's read landed (or failed onto the snapshot's `error`).
    WebLoaded(AdminWebSnapshot),
    /// The `admin-custody-hosting` registry read — one fold for the nav-edge
    /// load, the post-remove re-read, and a failed remove alike.
    CustodyHostingLoaded(AdminHostingSnapshot),
    /// The nest log ring (`Ok`) or the read failure (`Err`) — one fold for both,
    /// so a failure clears nothing the admin was already reading.
    LogsLoaded(Result<Vec<fauna_log::LogEntry>, String>),
    /// An `admin-bridges-pending` refresh or mutation resolved. `Box`ed for the
    /// `clippy::large_enum_variant` reason the mail snapshot is.
    BridgesSnapshot(Box<BridgeApprovalSnapshot>),
    /// The whole `admin-dns` page landed — both machines' snapshots plus the
    /// picker actor list, in one fold. `Box`ed for the `large_enum_variant` reason.
    DnsLoaded {
        dns: Box<DnsSnapshot>,
        domains: Box<LocalDomainsSnapshot>,
        actors: Vec<(Vec<u8>, String)>,
    },
    /// One `admin-dns` DNS-machine dispatch resolved (success or an error already
    /// recorded on the snapshot).
    DnsSnapshotOnly(Box<DnsSnapshot>),
    /// One `admin-dns` local-domain-machine dispatch resolved.
    LocalDomainsSnapshotOnly(Box<LocalDomainsSnapshot>),
    /// An `admin-dns` action failed **before** any machine dispatch, so no
    /// snapshot carries the reason (today: resolving the cert-delivery target nest
    /// for an issuance). Lands on [`AdminState::dns_action_error`], which the page
    /// paints as `error-message` alongside the two snapshots' own errors — so this
    /// never needs the app-wide `app.errors` slot the shell also registers that id
    /// from, and the page keeps exactly one `error-message`.
    DnsActionFailed(String),
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::CheckGate { nest } => {
                // Fail-closed (`admin.md` § Where logic lives): a non-admin or
                // any RPC error all resolve to
                // "not admin", so the shell is never shown to a non-admin
                // (§ Don't do these). Linux's `check_admin_status` `unwrap_or(false)`.
                let is_admin = AccountClient::new(nest)
                    .am_i_admin()
                    .await
                    .map(|r| r.admin)
                    .unwrap_or(false);
                Outcome::GateLoaded { is_admin }
            }
            Op::LoadDashboard { nest } => {
                // The two shared reads the linux dashboard makes, on one client:
                // `fauna.admin.stats` (the four stat cards) and `fauna.admin.status`
                // (the running version). Loaded atomically — a partial dashboard
                // (stats-but-no-version) would fail the e2e's "Version" wait for no
                // benefit, so either both land or the page shows the error.
                let client = AdminClient::new(nest.clone());
                let stats = match client.stats().await {
                    Ok(stats) => stats,
                    Err(e) => return Outcome::Failed(format!("load admin stats: {e}")),
                };
                let version = match client.status().await {
                    Ok(status) => status.version,
                    Err(e) => return Outcome::Failed(format!("load admin status: {e}")),
                };
                // The Mail card's state (`fauna.bridges.mail_health`). Tolerant,
                // unlike the two reads above: a failed read (or
                // any other error) drops the one card, never the dashboard.
                let mail_state = fauna_client_bridges::MailAdminClient::new(nest)
                    .mail_health()
                    .await
                    .ok()
                    .map(|reply| reply.state);
                Outcome::DashboardLoaded(DashboardSnapshot {
                    stats,
                    version,
                    mail_state,
                })
            }
            // The three DAV dispatches share one shape: run the action, drop its
            // `Result` (a failure rides `snapshot.error`, which `apply_outcome`
            // bridges — the deliberate drop the mail dispatches make), re-snapshot.
            Op::Caldav { machine, action } => {
                let _ = machine.dispatch(action).await;
                Outcome::CaldavSnapshot(machine.snapshot())
            }
            Op::Carddav { machine, action } => {
                let _ = machine.dispatch(action).await;
                Outcome::CarddavSnapshot(machine.snapshot())
            }
            // Same shape: a failed approve/reject/rotate rides `snapshot.error`,
            // which the fold bridges onto this page's `error-message`.
            Op::Bridges { machine, action } => {
                let _ = machine.dispatch(action).await;
                Outcome::BridgesSnapshot(Box::new(machine.snapshot()))
            }
            Op::Webdav { machine, action } => {
                let _ = machine.dispatch(action).await;
                Outcome::WebdavSnapshot(machine.snapshot())
            }
            // Same shape as the DAV dispatches: run the forwarder action, drop its
            // `Result` (a create/delete failure rides `snapshot.error`, which
            // `apply_outcome` surfaces on `admin-aliases-action-error`), re-snapshot.
            Op::Forwarders { machine, action } => {
                let _ = machine.dispatch(action).await;
                Outcome::ForwardersSnapshot(machine.snapshot())
            }
            // Same shape as the DAV/forwarder dispatches: run the mail action, drop
            // its `Result` (a failure — e.g. the nest's out-of-order spam-threshold
            // rejection — rides `snapshot.error`, which `apply_outcome` bridges to
            // the global `error-message`), re-snapshot. `PublishSpamBaseline` stashes
            // its reply on the snapshot without a re-read, so this one path serves it too.
            Op::Mail { machine, action } => {
                let _ = machine.dispatch(action).await;
                Outcome::MailSnapshot(Box::new(machine.snapshot()))
            }
            Op::LoadTiers { nest } => match load_settings_snapshot(&nest).await {
                Ok(outcome) => outcome,
                Err(e) => Outcome::Failed(format!("load admin tiers: {e}")),
            },
            Op::UpdateTier { nest, req } => {
                if let Err(e) = AdminClient::new(nest.clone()).tiers_update(req).await {
                    return Outcome::Failed(format!("update tier: {e}"));
                }
                // Re-read the whole page so the row re-renders from persisted state
                // (the e2e polls the cap back after save+refetch).
                match load_settings_snapshot(&nest).await {
                    Ok(outcome) => outcome,
                    Err(e) => Outcome::Failed(format!("reload admin tiers: {e}")),
                }
            }
            Op::CreateTier { nest, req } => {
                if let Err(e) = AdminClient::new(nest.clone()).tiers_create(req).await {
                    return Outcome::Failed(t::settings_page::add_tier_error(&e.to_string()));
                }
                // The add form clears only now: a refused name keeps what was
                // typed so the admin fixes the one field.
                match load_settings_snapshot(&nest).await {
                    Ok(outcome) => Outcome::TierAdded(Box::new(outcome)),
                    Err(e) => Outcome::Failed(format!("reload admin tiers: {e}")),
                }
            }
            Op::SaveMembership { nest, req } => {
                if let Err(e) = AdminClient::new(nest.clone())
                    .membership_tiers_set(req)
                    .await
                {
                    return Outcome::Failed(format!("save membership designation: {e}"));
                }
                match load_settings_snapshot(&nest).await {
                    Ok(outcome) => outcome,
                    Err(e) => Outcome::Failed(format!("reload membership designations: {e}")),
                }
            }
            Op::ClearMembership { nest, tier_name } => {
                if let Err(e) = AdminClient::new(nest.clone())
                    .membership_tiers_clear(tier_name)
                    .await
                {
                    return Outcome::Failed(format!("clear membership designation: {e}"));
                }
                match load_settings_snapshot(&nest).await {
                    Ok(outcome) => outcome,
                    Err(e) => Outcome::Failed(format!("reload membership designations: {e}")),
                }
            }
            Op::LoadNest { nest, nat } => match reflect_nest(&nest).await {
                Ok(reflective) => {
                    // `hydrate` pre-selects the radios from the nest's `node_mode`.
                    nat.hydrate().await;
                    Outcome::NestLoaded {
                        reflective: Box::new(reflective),
                        nat: nat.snapshot(),
                        reports: read_reports(&nest).await,
                    }
                }
                Err(e) => Outcome::Failed(e),
            },
            Op::TogglePairing { nest, enabled } => {
                if let Err(e) = AdminClient::new(nest.clone())
                    .services_update("pairing", enabled)
                    .await
                {
                    return Outcome::Failed(format!("update pairing: {e}"));
                }
                reflect_outcome(&nest).await
            }
            Op::SaveServingPort { nest, port } => {
                if let Err(e) = AdminClient::new(nest.clone()).set_serving_port(port).await {
                    return Outcome::Failed(format!("set serving port: {e}"));
                }
                reflect_outcome(&nest).await
            }
            Op::SaveRegion { nest, region } => {
                if let Err(e) = AdminClient::new(nest.clone()).set_region(region).await {
                    return Outcome::Failed(format!("set declared region: {e}"));
                }
                reflect_outcome(&nest).await
            }
            Op::SaveWebAppOrigin { nest, mode } => {
                if let Err(e) = AdminClient::new(nest.clone())
                    .set_web_app_origin(mode)
                    .await
                {
                    return Outcome::Failed(format!("set web-app origin: {e}"));
                }
                reflect_outcome(&nest).await
            }
            Op::WriteFeatureLimit {
                nest,
                editor,
                remove,
            } => {
                let client = fauna_client_features::FeaturesClient::new(nest.clone());
                let written = if remove {
                    client.remove(&editor).await
                } else {
                    client.save(&editor).await
                };
                let saved = match written {
                    Ok(saved) => saved,
                    Err(e) => {
                        return Outcome::Failed(e.text().resolve(fauna_i18n::strings::lookup));
                    }
                };
                match reflect_nest(&nest).await {
                    Ok(reflective) => Outcome::FeatureLimitWritten {
                        status: saved.status.resolve(fauna_i18n::strings::lookup),
                        editor: Box::new(editor.reseeded(&saved.reply)),
                        reflective: Box::new(reflective),
                    },
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::NatSubmit { nat } => {
                // The machine records success/failure on its own snapshot (the
                // `-nat-mode-status` element), never `error-message`.
                nat.submit().await;
                Outcome::NatSnapshot(nat.snapshot())
            }
            Op::RestartHost { nest } => {
                if let Err(e) = AdminClient::new(nest.clone()).request_host_restart().await {
                    return Outcome::Failed(format!("request host restart: {e}"));
                }
                reflect_outcome(&nest).await
            }
            Op::FactoryReset {
                nest,
                nest_url,
                persistence,
                cached_handle,
            } => {
                // Authoritative handle from the still-live session, before the wipe
                // (linux `client.rs:4298`); qualify a bare local-part with the nest
                // host so the post-reset re-claim re-registers the primary mail domain.
                //
                // Fall back to the registry's cached handle when that lookup cannot
                // answer — an unreachable nest, a session already torn down. Without
                // the fallback the slot persists an EMPTY handle and the post-reset
                // re-claim is refused ("a handle is required to claim the nest"),
                // trapping the client; see the variant's `cached_handle` doc.
                let handle = AccountClient::new(nest.clone())
                    .get()
                    .await
                    .ok()
                    .and_then(|r| r.handle)
                    .filter(|h| !h.is_empty())
                    .or_else(|| Some(cached_handle).filter(|h| !h.is_empty()))
                    .map(|h| qualify_handle(h, &nest_url))
                    .unwrap_or_default();
                // CR-1: mint + persist the post-reset claim code BEFORE dispatching.
                // The helper returns the code only once the row is durable, so the
                // crash-unsafe ordering is unrepresentable; a store that silently
                // dropped the row (a full disk) must NOT dispatch — a reset whose
                // code we failed to persist is the un-claimable box CR-1 describes.
                let Some(code) =
                    mint_and_persist_pending_factory_reset(&persistence, nest_url, handle)
                else {
                    return Outcome::Failed(
                        t::settings_page::FACTORY_RESET_PERSIST_FAILED.to_string(),
                    );
                };
                match AdminClient::new(nest).factory_reset(Some(code)).await {
                    Ok(_) => Outcome::FactoryResetComplete,
                    Err(e) => Outcome::Failed(format!("factory reset: {e}")),
                }
            }
            Op::LoadSeedRotateRoster { nest } => {
                Outcome::SeedRotateRoster(Box::new(load_seed_rotate_roster(&nest).await))
            }
            Op::SubmitTakedown { nest, form } => {
                Outcome::TakedownDone(submit_takedown(nest, form).await)
            }
            Op::ResolveReport {
                nest,
                report_id,
                outcome,
            } => {
                let error = AdminClient::new(Arc::clone(&nest))
                    .abuse_report_resolve(report_id, outcome)
                    .await
                    .err()
                    .map(|e| e.to_string());
                let verdict = crate::wizard::localized(
                    &fauna_client_moderation::report::resolve_verdict(outcome, error),
                );
                Outcome::ReportResolved(verdict, read_reports(&nest).await)
            }
            Op::RotateIssuerKey { nest } => {
                let verdict = AdminClient::new(nest.clone())
                    .rotate_issuer_key_verdict()
                    .await;
                Outcome::OauthDone {
                    status: crate::wizard::localized(&verdict),
                    keys: read_oauth_keys(&nest).await,
                }
            }
            Op::OauthForced { nest, arm } => {
                let verdict = AdminClient::new(nest.clone())
                    .force_rotate_verdict(arm, fauna_core::format::format_unix_local)
                    .await;
                Outcome::OauthDone {
                    status: crate::wizard::localized(&verdict),
                    keys: read_oauth_keys(&nest).await,
                }
            }
            Op::RotateDeploymentSeed { nest, store } => {
                Outcome::SeedRotated(rotate_deployment_seed(nest, store).await)
            }
            Op::LoadUsers { nest, offset } => match load_users_snapshot(&nest, offset).await {
                Ok(snapshot) => Outcome::UsersLoaded(Box::new(snapshot)),
                Err(e) => Outcome::Failed(e),
            },
            Op::LoadWeb { nest } => Outcome::WebLoaded(load_web_snapshot(&nest).await),
            Op::LoadCustodyHosting { nest } => {
                Outcome::CustodyHostingLoaded(load_custody_hosting_snapshot(&nest, None).await)
            }
            Op::RemoveCustodyHosting {
                nest,
                host_actor_id,
                grant_id,
            } => {
                let status =
                    match fauna_client_capabilities::custody_hosting::AdminHostingClient::new(
                        nest.clone(),
                    )
                    .remove(&host_actor_id, &grant_id)
                    .await
                    {
                        // `removed: false` is an honest no-op, not a failure: the
                        // row was already gone (a sibling admin, or the host's own
                        // reclaim). Saying so beats an error line that would claim
                        // the opposite of what happened.
                        Ok(reply) if !reply.removed => {
                            Some(t::custody_hosting::REMOVE_MISSING.to_string())
                        }
                        Ok(reply) if reply.store_dropped => {
                            Some(t::custody_hosting::REMOVED_WITH_STORE.to_string())
                        }
                        Ok(_) => Some(t::custody_hosting::REMOVED.to_string()),
                        Err(e) => {
                            // A failed remove re-reads anyway: the registry is the
                            // authority on what is still there, and a stale surface
                            // after a failure is how an admin comes to believe a row
                            // is gone when it is not.
                            return Outcome::CustodyHostingLoaded(
                                load_custody_hosting_snapshot(
                                    &nest,
                                    Some(format!("remove held custody: {e}")),
                                )
                                .await,
                            );
                        }
                    };
                Outcome::CustodyHostingLoaded(
                    load_custody_hosting_snapshot(&nest, None)
                        .await
                        .with_status(status),
                )
            }
            // Three reads, one fold. Ordered so the cheap structural half lands
            // first: the domain rows, then the record matrix (`list_records`),
            // then the live public-DNS verdicts overlaid onto it
            // (`verify_records`) — linux's `fetch_dns_records` ordering, minus the
            // cert-status read (Phase 2's badge). Each dispatch records its own
            // failure on its snapshot's `error`, so a verify timeout still leaves
            // the matrix rendered with `checking` rows.
            Op::LoadDns { nest, dns, domains } => {
                let _ = domains.dispatch(LocalDomainAction::Refresh).await;
                let _ = dns.dispatch(DnsAction::Refresh).await;
                // Then the per-domain served-cert health badge
                // (`tls-certificates.md` § C.4) — a pure Admin read projected onto
                // `cert_statuses`, one row per domain the `Refresh` just loaded.
                // Independent of verify, and it also fires the cert-coupled
                // floor-MX TLSA auto-withdraw when the primary flips
                // floor→trusted (a best-effort side effect of the read).
                let _ = dns.dispatch(DnsAction::RefreshCertStatus).await;
                let _ = dns
                    .dispatch(DnsAction::VerifyRecords { domain: None })
                    .await;
                let actors = users_actor_picker_options(&nest).await;
                Outcome::DnsLoaded {
                    dns: Box::new(dns.snapshot()),
                    domains: Box::new(domains.snapshot()),
                    actors,
                }
            }
            // Same shape as the DAV/forwarder dispatches: run the action, drop its
            // `Result` (the machine already recorded any failure on
            // `DnsSnapshot.error`), re-snapshot.
            Op::Dns { machine, action } => {
                let _ = machine.dispatch(action).await;
                Outcome::DnsSnapshotOnly(Box::new(machine.snapshot()))
            }
            Op::LocalDomains { machine, action } => {
                let _ = machine.dispatch(action).await;
                Outcome::LocalDomainsSnapshotOnly(Box::new(machine.snapshot()))
            }
            Op::DnsIssueCert {
                machine,
                nest,
                domain,
                single_issue,
            } => {
                match resolve_this_nest_id(&nest).await {
                    Ok(target_nest_id) => {
                        let action = if single_issue {
                            DnsAction::IssueCert {
                                domain,
                                target_nest_id,
                            }
                        } else {
                            DnsAction::BeginManualIssueCert {
                                domain,
                                target_nest_id,
                            }
                        };
                        let _ = machine.dispatch(action).await;
                        // Re-read the served-cert health so the badge reflects the
                        // new cert (a managed order installs it before returning);
                        // a manual `Begin` leaves the badge alone and surfaces the
                        // paste card instead.
                        let _ = machine.dispatch(DnsAction::RefreshCertStatus).await;
                    }
                    // The order cannot start without a delivery target. The
                    // machine never saw the action, so it has no error to report —
                    // carry the message ourselves so it still reaches the page's
                    // `error-message` instead of failing silently.
                    Err(e) => {
                        return Outcome::DnsActionFailed(format!(
                            "resolve target nest for {domain}: {e}"
                        ));
                    }
                }
                Outcome::DnsSnapshotOnly(Box::new(machine.snapshot()))
            }
            Op::DnsManageAll {
                machine,
                domains,
                managed,
            } => {
                // Sequential on purpose: `SetMode` is a read-modify-write of the
                // one `fauna.state.dns` document, so concurrent dispatches would
                // race and lose opt-ins. The first refusal (no covering
                // credential) is left on the snapshot and the sweep stops — a
                // partial "some domains managed" state is exactly what the
                // per-domain store is meant to represent, and pressing on would
                // overwrite that error with N-1 copies of itself.
                for domain in domains {
                    if machine
                        .dispatch(DnsAction::SetMode { domain, managed })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Outcome::DnsSnapshotOnly(Box::new(machine.snapshot()))
            }
            Op::LoadLogs { nest } => Outcome::LogsLoaded(
                AdminClient::new(nest)
                    .logs()
                    .await
                    .map(|reply| {
                        reply
                            .entries
                            .iter()
                            .map(fauna_client_admin::log_entry_from_wire)
                            .collect()
                    })
                    .map_err(|e| format!("load nest logs: {e}")),
            ),
            Op::SetApexActor { nest, actor } => {
                // A write failure paints on the page's own `error-message` and
                // leaves the last-read designation showing — never a picker that
                // claims a designation the nest rejected.
                if let Err(e) = WebClient::new(nest.clone()).set_apex_actor(actor).await {
                    let mut snapshot = load_web_snapshot(&nest).await;
                    snapshot.error = Some(format!("set apex actor: {e}"));
                    return Outcome::WebLoaded(snapshot);
                }
                // Re-read the whole page so the picker re-renders from persisted
                // state (the e2e polls `fauna.web.get_apex_actor` for the same
                // designation).
                Outcome::WebLoaded(load_web_snapshot(&nest).await)
            }
            Op::MutateUsers {
                nest,
                offset,
                mutation,
            } => {
                let client = AdminClient::new(nest.clone());
                // A mint carries the returned token forward (the form stays open
                // showing it) — its own outcome; the rest share the re-read tail.
                if let UsersMutation::CreateInvite {
                    tier,
                    uses,
                    guardian,
                    age_band,
                } = mutation
                {
                    return match client
                        .invite_codes_create("", tier, uses, guardian, age_band)
                        .await
                    {
                        Ok(reply) => match load_users_snapshot(&nest, offset).await {
                            Ok(snapshot) => Outcome::InviteMinted {
                                code: reply.code,
                                snapshot: Box::new(snapshot),
                            },
                            Err(e) => Outcome::UsersActionFailed(e),
                        },
                        Err(e) => Outcome::UsersActionFailed(format!("mint invite code: {e}")),
                    };
                }
                // Each remaining mutation is one AdminClient call; a failure lands on
                // the page-scoped action error, not `error-message` (`admin.md` § Errors).
                let result = match mutation {
                    UsersMutation::Evict { actor } => client
                        .users_evict(actor, t::users_page::EVICT_DEFAULT_REASON, "other")
                        .await
                        .map_err(|e| format!("evict user: {e}")),
                    UsersMutation::Suspend { actor } => client
                        .users_suspend(
                            actor,
                            t::users_page::SUSPEND_DEFAULT_REASON.to_string(),
                            "other".to_string(),
                        )
                        .await
                        .map(|_| ())
                        .map_err(|e| format!("suspend user: {e}")),
                    UsersMutation::CancelEviction { actor } => client
                        .users_cancel_eviction(actor)
                        .await
                        .map_err(|e| format!("restore user: {e}")),
                    UsersMutation::AddAdmin { actor } => client
                        .admins_add(actor)
                        .await
                        .map(|_| ())
                        .map_err(|e| format!("grant admin: {e}")),
                    UsersMutation::RemoveAdmin { actor } => client
                        .admins_remove(actor)
                        .await
                        .map(|_| ())
                        .map_err(|e| format!("revoke admin: {e}")),
                    UsersMutation::ApproveAction { id } => client
                        .pending_action_approve(id)
                        .await
                        .map_err(|e| format!("approve pending action: {e}")),
                    UsersMutation::CancelAction { id } => client
                        .pending_action_cancel(id)
                        .await
                        .map_err(|e| format!("cancel pending action: {e}")),
                    UsersMutation::ChangeTier { actor, tier, label } => client
                        .users_update(actor, tier, label)
                        .await
                        .map_err(|e| format!("change user tier: {e}")),
                    UsersMutation::SaveRegistration {
                        mode,
                        max_free,
                        age_verification,
                    } => match client
                        .set_registration_mode(mode, max_free)
                        .await
                        .map_err(|e| format!("save registration: {e}"))
                    {
                        // The require-knob is a second kind on the same gesture,
                        // sent only when it changed (`admin.md` § 2 → Registration).
                        Ok(()) => match age_verification {
                            Some(required) => client
                                .set_age_verification_required(required)
                                .await
                                .map_err(|e| format!("save age verification: {e}")),
                            None => Ok(()),
                        },
                        Err(e) => Err(e),
                    },
                    UsersMutation::AdmitUser {
                        actor,
                        tier,
                        handle,
                    } => client
                        .users_create(actor, tier, String::new(), handle)
                        .await
                        .map_err(|e| format!("admit user: {e}")),
                    UsersMutation::DeleteInvite { code } => client
                        .invite_codes_delete(code)
                        .await
                        .map_err(|e| format!("delete invite code: {e}")),
                    UsersMutation::ApproveRequest {
                        id,
                        tier,
                        guardian,
                        age_band,
                    } => client
                        .invite_requests_approve(id, tier, None, guardian, age_band)
                        .await
                        .map(|_| ())
                        .map_err(|e| format!("approve request: {e}")),
                    UsersMutation::DenyRequest { id, reason } => client
                        .invite_requests_deny(id, reason)
                        .await
                        .map_err(|e| format!("deny request: {e}")),
                    // Handled above (returns early with its own outcome).
                    UsersMutation::CreateInvite { .. } => {
                        unreachable!("CreateInvite handled above")
                    }
                };
                match result {
                    // Re-read the whole page from persisted state (non-optimistic —
                    // the row re-renders from the truth, the tiers/evict e2e contract).
                    Ok(()) => match load_users_snapshot(&nest, offset).await {
                        Ok(snapshot) => Outcome::UsersLoaded(Box::new(snapshot)),
                        Err(e) => Outcome::UsersActionFailed(e),
                    },
                    Err(e) => Outcome::UsersActionFailed(e),
                }
            }
        }
    }
}

/// Read the whole `admin-settings` (Tiers) page in one shot: the tier
/// definitions (`tiers_list`), the membership section's row set — the admin's
/// own subscription tier names (`fauna.subscriptions.tiers.list`) — and which of
/// those already carry a designation (`membership_tiers_list`). One helper for
/// the nav-edge load and every post-save/-clear re-read (`admin.md` § Persistence
/// — every page re-reads; monetization.md § Pillar 4). Own tier names degrade to
/// empty on a failed read — a payee with no subscription tiers yet has nothing
/// to designate, not an error (linux's `fetch_own_membership_tier_names`); a
/// failed tiers/membership read is real and propagates.
async fn load_settings_snapshot(nest: &Arc<NestClient>) -> Result<Outcome, String> {
    let admin = AdminClient::new(nest.clone());
    let tiers = admin
        .tiers_list()
        .await
        .map_err(|e| format!("load tiers: {e}"))?
        .tiers;
    let own_membership_tier_names = SubscriptionsClient::new(nest.clone())
        .tiers_list()
        .await
        .map(|list| list.into_iter().map(|t| t.name).collect())
        .unwrap_or_default();
    let membership_tiers = admin
        .membership_tiers_list()
        .await
        .map_err(|e| format!("load membership tiers: {e}"))?
        .membership_tiers;
    Ok(Outcome::TiersLoaded {
        tiers,
        own_membership_tier_names,
        membership_tiers,
    })
}

/// Read the whole `admin-web` page in one shot: the apex designation
/// (`fauna.web.get_apex_actor`) + the actors the picker offers
/// (`fauna.admin.users.list`). One helper for the nav-edge load and the
/// post-write re-read (`admin.md` § Persistence — every page re-reads).
///
/// The two reads are NOT equally fatal, matching linux's `admin_web.rs::hydrate`:
/// the apex read failing means the page has nothing true to show, so it paints
/// the error; the actor-list failing degrades to an empty option list (the
/// picker still offers "None" and still shows the designation via the fallback
/// entry), because a designation the admin can *see* is worth more than a blank
/// page.
async fn load_web_snapshot<R: RpcRequester>(nest: &R) -> AdminWebSnapshot {
    let current = match WebClient::new(nest).get_apex_actor().await {
        Ok(current) => current,
        Err(e) => {
            return AdminWebSnapshot {
                current: None,
                actors: Vec::new(),
                error: Some(format!("load apex actor: {e}")),
            };
        }
    };
    let actors = users_actor_picker_options(nest).await;
    AdminWebSnapshot {
        current,
        actors,
        error: None,
    }
}

/// The every-account read (`fauna_client_admin::users_list_all` — never one
/// `fauna.admin.users.list` page, `admin.md` § 2 → *Which accounts a picker
/// offers*) + injective actor-picker mapping shared, verbatim, by
/// `Op::LoadDns`'s DNS/catch-all actor list and [`load_web_snapshot`]'s apex
/// picker — one read, one fold, so a call site can only drift back onto the
/// raw, non-injective `u.label.clone()` (`admin.md` § 2's two-halves rule — the
/// drift that already happened once) or onto a single
/// page by ceasing to be a bare call to this fn. Generic over the transport so
/// a fake `RpcRequester` can pin both real build sites directly, rather than a
/// test re-deriving state by calling `actor_picker_options`
/// itself.
async fn users_actor_picker_options<R: RpcRequester>(nest: &R) -> Vec<(Vec<u8>, String)> {
    fauna_client_admin::users_list_all(&AdminClient::new(nest))
        .await
        .map(|users| fauna_client_admin::actor_picker_options(&users))
        .unwrap_or_default()
}

/// The `admin-custody-hosting` read wrapper: tui's call sites pass `&Arc<NestClient>`
/// (borrowed, reused across the remove-then-reread sequence below), while the
/// shared [`load_admin_hosting_snapshot`] takes its transport by value.
async fn load_custody_hosting_snapshot(
    nest: &Arc<NestClient>,
    error: Option<String>,
) -> AdminHostingSnapshot {
    load_admin_hosting_snapshot(nest.clone(), error).await
}

/// The armed rotation confirm's listing: the **authoritative** roster
/// (`fauna.admin.admins.list` — never a page of `users.list` filtered on
/// `is_admin`, which cannot see a member past the page window), plus a
/// best-effort per-member label join, folded by shared Rust.
///
/// The join is one round trip per member and the roster is 1–3 rows; a member
/// whose lookup fails keeps its row and degrades to the short id
/// ([`seed_rotation_confirm_view`]'s rule), because an inheritor we cannot name
/// still inherits.
async fn load_seed_rotate_roster(nest: &Arc<NestClient>) -> SeedRotateConfirm {
    let client = AdminClient::new(nest.clone());
    match client.seed_rotate_roster_view().await {
        Ok(view) => SeedRotateConfirm::Ready(Box::new(view)),
        Err(e) => SeedRotateConfirm::Failed(crate::wizard::localized(
            &fauna_core::localized::LocalizedText::key_arg(
                "admin.nest_page.rotate_seed_roster_error",
                "cause",
                e.to_string(),
            ),
        )),
    }
}

/// One read of the reports queue, as the page's folds take it.
pub type ReportsRead = Result<Vec<fauna_protocol::moderation::AbuseReportQueueEntry>, String>;

/// The reports queue read (`fauna.moderation.abuse_report.queue`, Admin).
async fn read_reports(nest: &Arc<NestClient>) -> ReportsRead {
    AdminClient::new(Arc::clone(nest))
        .abuse_report_queue()
        .await
        .map(|reply| reply.reports)
        .map_err(|e| format!("load reports: {e}"))
}

/// Fold a queue read. A failure lands on `error-message` and leaves
/// `reports_loaded` alone, so a refused read never paints "No open reports".
fn apply_reports(app: &mut App, reports: ReportsRead) {
    match reports {
        Ok(reports) => {
            app.admin.reports = reports;
            app.admin.reports_loaded = true;
        }
        Err(e) => {
            app.errors.insert(Page::Admin, e);
        }
    }
}

/// Dispatch the captured takedown form and word the outcome — the id and
/// reference are trimmed here, exactly as the shared form fold judged them
/// (`takedown_form_view` gates on the trimmed values), so what was confirmed is
/// what is sent.
async fn submit_takedown(nest: Arc<NestClient>, form: TakedownForm) -> String {
    let error = ModerationClient::new(nest)
        .legal_takedown(
            form.content_id.trim(),
            form.content_type.wire(),
            form.legal_reference.trim(),
            form.restore,
        )
        .await
        .err()
        .map(|e| e.to_string());
    crate::wizard::localized(&takedown_verdict(form.restore, error))
}

/// Drive the deployment-seed rotation ceremony and report it in one sentence
/// (`box-recovery.md` § Deployment-seed rotation → § The ceremony; § The
/// plane-era recovery floor, (c) The writes).
///
/// The whole drive — resolve the bound identity, custody the successor's row
/// on the plane before dispatch, dispatch, mark — and every outcome's one
/// verdict live in the shared
/// `fauna_client_account_runtime::deployment_seeds::rotate_deployment_seed`;
/// this function only renders it. There is no fan-out step: the plane carries
/// the successor's row to every box the admin binds.
async fn rotate_deployment_seed(
    nest: Arc<NestClient>,
    store: Option<AccountStoreHandle>,
) -> String {
    let verdict = fauna_client_account_runtime::deployment_seeds::rotate_deployment_seed(
        &nest,
        store.as_ref(),
    )
    .await;
    crate::wizard::localized(&verdict)
}

/// Read the whole `admin-users` page in one shot: the current page of users +
/// unpaginated total (`users_list`), every account for the guardian pickers
/// (`users_list_all`), the tier names for the pickers (`tiers_list`), the invite
/// codes (`invite_codes_list`), the pending requests (`invite_requests_list`),
/// and the registration posture (`fauna.setup.status`). One helper for the
/// nav-edge load, every pagination step, and every post-mutation re-read
/// (`admin.md` § Persistence — every page re-reads).
async fn load_users_snapshot(nest: &Arc<NestClient>, offset: i64) -> Result<UsersSnapshot, String> {
    let client = AdminClient::new(nest.clone());
    let users_reply = client
        .users_list(Some(USERS_PAGE_SIZE), offset)
        .await
        .map_err(|e| format!("load users: {e}"))?;
    // The guardian pickers' option source: every account, never the page above
    // (`admin.md` § 2 → *Which accounts a picker offers*).
    let picker_users = fauna_client_admin::users_list_all(&client)
        .await
        .map_err(|e| format!("load users for the guardian pickers: {e}"))?;
    let tiers = client
        .tiers_list()
        .await
        .map_err(|e| format!("load tiers: {e}"))?
        .tiers
        .into_iter()
        .map(|t| t.name)
        .collect();
    let invite_codes = client
        .invite_codes_list()
        .await
        .map_err(|e| format!("load invite codes: {e}"))?
        .invite_codes;
    let invite_requests = client
        .invite_requests_list()
        .await
        .map_err(|e| format!("load invite requests: {e}"))?
        .invite_requests;
    let status: SetupStatusReply = nest
        .request("fauna.setup.status", SetupStatusRequest::default())
        .await
        .map_err(|e| format!("load setup status: {e}"))?;
    let pending_actions = client
        .pending_actions_list()
        .await
        .map_err(|e| format!("load pending admin actions: {e}"))?
        .actions;
    Ok(UsersSnapshot {
        users: users_reply.users,
        picker_users,
        total: users_reply.total,
        tiers,
        invite_codes,
        invite_requests,
        registration_mode: status.registration_mode,
        max_free_users: status.max_free_users,
        age_verification_required: status.age_verification_required,
        pending_actions,
    })
}

/// Read the `admin-nest` reflective surface: the pairing flag (`services.list`)
/// plus the serving-port + host-OS-maintenance fields (`fauna.setup.status`).
/// The NAT-mode leg is the shared machine's own concern.
async fn reflect_nest(nest: &Arc<NestClient>) -> Result<NestPageSnapshot, String> {
    let pairing_enabled = AdminClient::new(nest.clone())
        .services_list()
        .await
        .map_err(|e| format!("load admin services: {e}"))?
        .services
        .pairing;
    let status: SetupStatusReply = nest
        .request("fauna.setup.status", SetupStatusRequest::default())
        .await
        .map_err(|e| format!("load setup status: {e}"))?;
    let region = AdminClient::new(nest.clone())
        .region_status()
        .await
        .map_err(|e| format!("load declared region: {e}"))?;
    // `Ok(None)` is a nest predating the choice (unknown-kind) — the fold's
    // own state, not a failed read.
    let web_app_origin = AdminClient::new(nest.clone())
        .web_app_origin()
        .await
        .map_err(|e| format!("load web-app origin: {e}"))?;
    Ok(NestPageSnapshot {
        pairing_enabled,
        serving_port: status.serving_port,
        fronted_by_router: status.fronted_by_router,
        os_security_updates_pending: status.os_security_updates_pending,
        os_reboot_pending: status.os_reboot_pending,
        region: fauna_client_admin::admin_region_view(&region),
        web_app_origin: fauna_client_admin::admin_web_app_origin_view(web_app_origin.as_ref()),
        oauth: read_oauth_keys(nest).await,
        feature_limits: read_feature_limits(nest).await,
    })
}

/// The admin tier's authored feature documents (`fauna.features.policy.get`
/// joined with the capability set), folded — non-fatal by construction, the
/// `read_oauth_keys` shape: a failed read gets a
/// worded reason line, and the rest of the page still paints.
async fn read_feature_limits(nest: &Arc<NestClient>) -> FeatureLimitsRead {
    match fauna_client_features::FeaturesClient::new(nest.clone())
        .authored_surface(fauna_client_features::AuthoringTier::Admin)
        .await
    {
        Ok(surface) => FeatureLimitsRead::Ready(surface),
        Err(e) => FeatureLimitsRead::Failed(fauna_i18n::strings::features::editor_load_failed(
            &e.to_string(),
        )),
    }
}

/// The issuer key set's read (`fauna.oauth.issuer_key_status`), folded — and
/// non-fatal by construction: a failure (any read error, e.g. a
/// transient transport fault) becomes the section's own worded reason line,
/// never the reflect's error, so the rest of the page still paints.
async fn read_oauth_keys(nest: &Arc<NestClient>) -> OauthKeysRead {
    match AdminClient::new(nest.clone())
        .issuer_key_status_view()
        .await
    {
        Ok(view) => OauthKeysRead::Ready(view),
        Err(e) => OauthKeysRead::Failed(t::nest_page::oauth_keys_error(&e.to_string())),
    }
}

/// [`reflect_nest`] wrapped as an [`Outcome`] — the re-read every nest-page
/// mutation ends with (`NestReflect` on success, `Failed` on a read error).
async fn reflect_outcome(nest: &Arc<NestClient>) -> Outcome {
    match reflect_nest(nest).await {
        Ok(reflective) => Outcome::NestReflect(Box::new(reflective)),
        Err(e) => Outcome::Failed(e),
    }
}

/// Qualify a bare local-part handle with the nest host (linux's `nest_host`
/// fallback) so the post-reset re-claim re-registers the primary mail domain
/// (claim auto-registers the handle's `@domain`). An already-qualified handle
/// (contains `@`) is returned unchanged.
fn qualify_handle(handle: String, nest_url: &str) -> String {
    if handle.contains('@') {
        return handle;
    }
    match nest_host(nest_url) {
        Some(host) => format!("{handle}@{host}"),
        None => handle,
    }
}

/// The host portion of a nest url (`wss://host:port/path` → `host`), or
/// `None` when no host can be isolated (empty/scheme-only input) — a bare
/// `fauna_core::format::url_host` swap would be wrong here: that fn's
/// echo-the-input fallback can't be told apart from a genuine bare-host
/// success by output inspection alone (`url_host("")` == `""`, which doesn't
/// look like a failure).
fn nest_host(nest_url: &str) -> Option<String> {
    fauna_core::format::url_host_opt(nest_url)
}

/// Fold an [`Outcome`] back into the app.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::GateLoaded { is_admin } => {
            app.am_i_admin = is_admin;
            if is_admin {
                // The admin auto-default (`long-term-store.md` § Per-account
                // re-auth): this session's account is an admin identity, so flip
                // its re-auth-on-activate flag ON — idempotently, and only if the
                // user has not already set it (an explicit OFF sticks forever).
                // The registry never learns admin-ness; the *client* decides it
                // here, at the `am_i_admin` observation, exactly as linux hooks it
                // at the Admin-sidebar nav gate.
                crate::session::auto_enable_admin_confirm(app);
                // Report the nest's PUBLIC host-address so it gates ACME HTTP-01
                // on the strong resolve-check and can assemble the apex/`mail.`
                // rows (`domains-and-tls-bootstrap.md` § Host-address
                // acquisition). Fired here — once per session, at the same
                // `am_i_admin` observation linux hooks
                // (`app.rs` AdminStatusLoaded → `report_host_address()`) — because
                // `set_host_address` is Admin-only, so gating on the caller side
                // avoids a pointless failing RPC for a non-admin.
                spawn_host_address_report(&app.admin);
            }
        }
        Outcome::DashboardLoaded(snapshot) => {
            app.admin.dash = Some(snapshot);
            app.errors.remove(&Page::Admin);
        }
        Outcome::Failed(message) => {
            app.errors.insert(Page::Admin, message);
        }
        Outcome::SeedRotateRoster(confirm) => {
            // Only while still armed: a cancel that raced the read must not
            // re-open the confirm under the admin.
            if app.admin.seed_rotate_confirm.is_some() {
                app.admin.seed_rotate_confirm = Some(*confirm);
            }
        }
        Outcome::SeedRotated(status) => {
            app.admin.seed_rotate_status = Some(status);
        }
        Outcome::TakedownDone(status) => {
            app.admin.takedown_status = Some(status);
        }
        Outcome::ReportResolved(status, reports) => {
            app.admin.reports_status = Some(status);
            apply_reports(app, reports);
        }
        Outcome::OauthDone { status, keys } => {
            app.admin.oauth_in_flight = false;
            app.admin.oauth_status = Some(status);
            // Only the section's own leg is replaced; a snapshot that has not
            // landed yet (the admin left and came back mid-call) is left for
            // the nav-edge load, which reads the set afresh anyway.
            if let Some(snapshot) = app.admin.nest_snapshot.as_mut() {
                snapshot.oauth = keys;
            }
        }
        Outcome::CaldavSnapshot(snapshot) => {
            bridge_admin_error(app, &snapshot.error);
            // Re-seed the port draft from the persisted value — the field mirrors
            // nest state (linux re-seeds the entry from `caldav_port` on every
            // render).
            app.admin.caldav_port_input = snapshot.caldav_port.to_string();
            app.admin.caldav_snapshot = Some(snapshot);
        }
        Outcome::CarddavSnapshot(snapshot) => {
            bridge_admin_error(app, &snapshot.error);
            app.admin.carddav_snapshot = Some(snapshot);
        }
        Outcome::WebdavSnapshot(snapshot) => {
            bridge_admin_error(app, &snapshot.error);
            app.admin.webdav_snapshot = Some(snapshot);
        }
        Outcome::ForwardersSnapshot(snapshot) => {
            // Unlike the DAV pages, a forwarder error surfaces on the page's OWN
            // `admin-aliases-action-error` element (`admin.md` § 4 — errors route
            // there, NOT the app-wide `error-message`); `aliases::aliases_elements`
            // paints it from `snapshot.error`, so this fold does not touch
            // `App::errors`. Re-seed the domain picker to a real hosted domain when
            // the current pick is empty or no longer hosted (the picker always
            // offers a submittable value; a human's select still wins).
            let hosted = &snapshot.local_domains;
            if app.admin.forwarder_add_domain.is_empty()
                || !hosted.contains(&app.admin.forwarder_add_domain)
            {
                app.admin.forwarder_add_domain = hosted.first().cloned().unwrap_or_default();
            }
            app.admin.forwarders_snapshot = Some(snapshot);
        }
        Outcome::MailSnapshot(snapshot) => {
            // Bridge a machine dispatch/read error onto the global `error-message`
            // (admin.md § 6 — mail errors surface there, unlike the page-scoped
            // aliases error), then re-seed the six groups' drafts from the persisted
            // snapshot (the field-mirrors-nest-state re-seed) so a group always shows
            // persisted values until the human edits again. Publish stashes its result
            // on the same snapshot, so this one fold serves it too.
            let snapshot = *snapshot;
            bridge_admin_error(app, &snapshot.error);
            app.admin.mail_drafts.seed(&snapshot);
            // A confirm never stays armed across a re-read (page entry, any act):
            // a second press must mean "yes, now", never land on a stale arm.
            app.admin.mail_warmup_reset_armed = false;
            app.admin.mail_snapshot = Some(snapshot);
        }
        Outcome::TierAdded(reloaded) => {
            app.admin.tier_add_name.clear();
            app.admin.tier_add_caps = TierCapDrafts::default();
            apply_outcome(app, *reloaded);
        }
        Outcome::TiersLoaded {
            tiers,
            own_membership_tier_names,
            membership_tiers,
        } => {
            // Re-seed each row's cap drafts from the persisted caps (the field
            // mirrors nest state, like the DAV port), then store — one fold for
            // both the load edge and the post-save re-read. A clean read clears a
            // prior error.
            app.admin.tier_cap_drafts = tiers.iter().map(TierCapDrafts::from_tier).collect();
            app.admin.tiers = Some(tiers);
            // Re-seed the membership row drafts: one per owned subscription tier,
            // looked up against the persisted designations (the linux
            // `update_membership_tiers` merge).
            app.admin.membership_drafts = own_membership_tier_names
                .iter()
                .map(|name| {
                    let existing = membership_tiers.iter().find(|m| &m.tier_name == name);
                    MembershipRowDraft::seed(name, existing)
                })
                .collect();
            app.admin.own_membership_tier_names = Some(own_membership_tier_names);
            app.admin.membership_tiers = Some(membership_tiers);
            app.errors.remove(&Page::Admin);
        }
        Outcome::NestLoaded {
            reflective,
            nat,
            reports,
        } => {
            // Re-seed the port draft from the persisted value (the field mirrors
            // nest state, like the DAV port).
            app.admin.serving_port_input = reflective.serving_port.to_string();
            // Same discipline for the region: the draft mirrors the declaration, so
            // a withdrawal empties the field rather than leaving the withdrawn code
            // sitting in it looking declared.
            app.admin.region_input = reflective.region.declared.clone().unwrap_or_default();
            // The radios mirror the nest's choice, like the drafts above.
            app.admin.web_app_origin_draft = reflective.web_app_origin.selected;
            app.admin.nest_snapshot = Some(*reflective);
            app.admin.nat_snapshot = Some(nat);
            app.errors.remove(&Page::Admin);
            apply_reports(app, reports);
        }
        Outcome::NestReflect(reflective) => {
            app.admin.serving_port_input = reflective.serving_port.to_string();
            // Same discipline for the region: the draft mirrors the declaration, so
            // a withdrawal empties the field rather than leaving the withdrawn code
            // sitting in it looking declared.
            app.admin.region_input = reflective.region.declared.clone().unwrap_or_default();
            // The radios mirror the nest's choice, like the drafts above.
            app.admin.web_app_origin_draft = reflective.web_app_origin.selected;
            app.admin.nest_snapshot = Some(*reflective);
            app.errors.remove(&Page::Admin);
        }
        Outcome::NatSnapshot(nat) => {
            app.admin.nat_snapshot = Some(nat);
        }
        // Deliberately does NOT re-seed the port / region drafts: this write
        // touched neither, and the admin may be typing in them.
        Outcome::FeatureLimitWritten {
            status,
            editor,
            reflective,
        } => {
            app.admin.feature_editor = *editor;
            app.admin.feature_editor_status = Some(status);
            app.admin.nest_snapshot = Some(*reflective);
            app.errors.remove(&Page::Admin);
        }
        Outcome::FactoryResetComplete => {
            // The claim code is durably persisted; tear the session down (keeping
            // credentials) and re-onboard at the pre-filled claim code — linux's
            // `FactoryResetComplete` handler (`app.rs:2852`).
            crate::launch::enter_factory_reset_reonboard(app);
        }
        Outcome::UsersLoaded(snapshot) => {
            // One fold for the nav-edge load, pagination, and every post-mutation
            // re-read. Re-seed the drafts that mirror nest state, store the snapshot,
            // and clear the page-scoped action error (a clean read clears it — the
            // TiersLoaded shape).
            let snapshot = *snapshot;
            app.admin.users.reseed(&snapshot);
            app.admin.users.snapshot = Some(snapshot);
            app.admin.users.action_error = None;
        }
        Outcome::BridgesSnapshot(snapshot) => {
            // The page paints its own `error-message` off the snapshot, like the
            // Web page — nothing to bridge onto `app.errors`.
            app.admin.bridges_snapshot = Some(*snapshot);
        }
        Outcome::WebLoaded(snapshot) => {
            // One fold for the nav-edge load and every post-write re-read. The
            // page paints its own `error-message` off the snapshot, so unlike the
            // shared-machine pages there is nothing to bridge onto `app.errors`.
            app.admin.web_snapshot = Some(snapshot);
        }
        Outcome::CustodyHostingLoaded(snapshot) => {
            // A landed read always disarms: the rows have re-ordered, so a
            // confirm armed against the old surface must not survive into the
            // new one.
            app.admin.custody_hosting_confirm = None;
            bridge_admin_error(app, &snapshot.error);
            app.admin.custody_hosting = Some(snapshot);
        }
        Outcome::DnsLoaded {
            dns,
            domains,
            actors,
        } => {
            // One fold for the whole page. Like the Web/Bridges pages, `admin-dns`
            // paints its own `error-message` off the held snapshots (in linux's
            // precedence: domain-CRUD feedback first, then list/verify), so there
            // is nothing to bridge onto `app.errors`.
            app.admin.dns_action_error = None;
            reseed_dns_rename_target(&mut app.admin, &domains);
            app.admin.dns_snapshot = Some(*dns);
            app.admin.local_domains_snapshot = Some(*domains);
            app.admin.dns_actors = actors;
        }
        Outcome::DnsSnapshotOnly(snapshot) => {
            app.admin.dns_action_error = None;
            app.admin.dns_snapshot = Some(*snapshot);
        }
        Outcome::LocalDomainsSnapshotOnly(snapshot) => {
            app.admin.dns_action_error = None;
            reseed_dns_rename_target(&mut app.admin, &snapshot);
            app.admin.local_domains_snapshot = Some(*snapshot);
        }
        Outcome::DnsActionFailed(message) => {
            app.admin.dns_action_error = Some(message);
        }
        Outcome::LogsLoaded(result) => match result {
            Ok(entries) => {
                app.admin.logs = entries;
                app.admin.logs_error = None;
            }
            // Keep the last-painted rows: a transient read failure should say so
            // on `error-message`, not blank a view the admin was reading.
            Err(message) => app.admin.logs_error = Some(message),
        },
        Outcome::UsersActionFailed(message) => {
            // A hub mutation failed — surface on the page-scoped `admin-users-action-error`
            // (NOT `error-message`; `admin.md` § Errors), leaving the last snapshot painted.
            app.admin.users.action_error = Some(message);
        }
        Outcome::InviteMinted { code, snapshot } => {
            // Store the re-read snapshot (the new code now lists) and reveal the token
            // copyable — `minted_code` is form state, so it survives the reseed and the
            // create form stays open showing it (the copy-button-visible e2e contract).
            let snapshot = *snapshot;
            app.admin.users.reseed(&snapshot);
            app.admin.users.snapshot = Some(snapshot);
            app.admin.users.action_error = None;
            app.admin.users.minted_code = Some(code);
        }
    }
}

/// Bridge a shared-machine snapshot error onto the admin page's globally-registered
/// `error-message`, clearing it on a clean snapshot — the page-module-contract
/// discipline (`tui.md` § The page-module contract): a gesture failing inside
/// shared Rust reports on the machine's snapshot, a different place from
/// `App::errors`, so a dispatch that fails would otherwise paint nothing and do
/// nothing (the dropped-command shape). The DAV machines store their dispatch
/// error on the snapshot, so this is the one place it reaches the banner.
/// Keep `admin-dns-rename-new-primary-select` pointing at a real promotion
/// target: re-seed the pick to the first active non-primary domain whenever it is
/// blank or names a domain that is no longer offerable (removed, or promoted to
/// primary itself). The forwarder-domain picker's re-seed shape — the picker
/// always shows a submittable value, and a human's explicit pick still wins.
fn reseed_dns_rename_target(state: &mut AdminState, domains: &LocalDomainsSnapshot) {
    let offered: Vec<&str> = rename_target_names(domains);
    if state.dns_rename_target.is_empty() || !offered.contains(&state.dns_rename_target.as_str()) {
        state.dns_rename_target = offered.first().map(|d| d.to_string()).unwrap_or_default();
    }
}

/// The promotion targets the rename wizard offers: every **active non-primary**
/// domain (the two-step rule — the wizard never adds a domain,
/// `mail-primary-domain-rename.md` § UX surface).
pub(super) fn rename_target_names(domains: &LocalDomainsSnapshot) -> Vec<&str> {
    domains
        .active
        .iter()
        .filter(|d| !d.is_primary)
        .map(|d| d.domain.as_str())
        .collect()
}

/// The active admin sub-page's own error, for the sub-pages that derive it from
/// a held snapshot instead of folding it onto `App::errors`.
///
/// Dispatching on `state.sub` is what makes the four snapshot-driven sub-pages
/// safe to share one `Page::Admin` slot: only the sub-page on screen can answer,
/// so a stale snapshot on a sub-page the user navigated away from cannot surface
/// under the one the user is looking at. `App::page_snapshot_error` carries the
/// contract this serves.
pub(crate) fn page_error(state: &AdminState) -> Option<String> {
    match state.sub {
        AdminPage::Web => web::page_error(state),
        AdminPage::Bridges => bridges::page_error(state),
        AdminPage::Dns => dns::page_error(state),
        AdminPage::Logs => logs::page_error(state),
        AdminPage::Nest => nest::page_error(state),
        AdminPage::CustodyHosting => custody_hosting::page_error(state),
        _ => None,
    }
}

fn bridge_admin_error(app: &mut App, error: &Option<String>) {
    match error {
        Some(message) => {
            app.errors.insert(Page::Admin, message.clone());
        }
        None => {
            app.errors.remove(&Page::Admin);
        }
    }
}

/// The ordered ui.yaml element list for the admin shell's current surface: the
/// sub-page nav rail (every page) followed by the active sub-page's own elements.
pub fn elements(app: &App) -> Vec<Element> {
    let state = &app.admin;
    let mut els = admin_nav_rail();
    els.extend(match state.sub {
        AdminPage::Dashboard => dashboard::dashboard_elements(state),
        AdminPage::Users => users::users_elements(state),
        AdminPage::Calendar => calendar::calendar_elements(state),
        AdminPage::Contacts => contacts::contacts_elements(state),
        AdminPage::Files => files::files_elements(state),
        AdminPage::Nest => nest::nest_elements(state),
        AdminPage::Mail => mail::mail_elements(state),
        AdminPage::Aliases => aliases::aliases_elements(state),
        AdminPage::Settings => settings::settings_elements(state),
        AdminPage::Web => web::web_elements(state),
        AdminPage::Dns => dns::dns_elements(state),
        AdminPage::CustodyHosting => custody_hosting::custody_hosting_elements(state),
        AdminPage::Logs => logs::logs_elements(state),
        AdminPage::Bridges => bridges::bridges_elements(state),
    });
    els
}

/// The admin sub-page switcher — the tui adaptation of the GUI's vertical
/// sidebar-swap (`admin.md` § Navigation model): a rail of the built admin
/// sub-pages, painted atop every admin page so a human can move between them (the
/// separate `admin-nav-back` is the "leave admin" affordance). Each row is
/// `admin-nav-row[<page key>]` (ui.yaml `navigation.sub_page_nav_rows`), so a
/// test bound to gestures reaches any sub-page by clicks; the two-element `nav`
/// patch stays the automation path for everything else.
fn admin_nav_rail() -> Vec<Element> {
    AdminPage::BUILT
        .iter()
        .map(|&page| {
            Element::gesture_button(
                format!("{}[{}]", fauna_ui_ids::ADMIN_NAV_ROW, page.page_key()),
                page.rail_label(),
                true,
                Gesture::Admin(Action::Open(page)),
            )
            // Every row here is a destination — `apps/tui.md` § Rendering →
            // *Control vocabulary* rule 4: brackets promise "acts now".
            .nav()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_admin::seed_rotation_confirm_view;
    use fauna_client_mail_settings::caldav_policy::CaldavPolicyStatus;

    /// Every admin rail row is `admin-nav-row[<page key>]` (ui.yaml
    /// `navigation.sub_page_nav_rows`), one distinct key per built sub-page, and
    /// that key routes back to the same sub-page through the two-element nav —
    /// so the click path and the nav-patch path can never name different pages.
    #[test]
    fn every_admin_rail_row_is_keyed_by_the_page_it_opens() {
        let rail = admin_nav_rail();
        assert_eq!(rail.len(), AdminPage::BUILT.len());
        let mut seen = std::collections::BTreeSet::new();
        for (row, &page) in rail.iter().zip(AdminPage::BUILT) {
            assert_eq!(row.id, format!("admin-nav-row[{}]", page.page_key()));
            assert!(seen.insert(row.id.clone()), "duplicate rail row {}", row.id);
            let mut state = AdminState::default();
            route_subpage(&mut state, Some(page.page_key()));
            assert_eq!(state.sub, page, "{} routes elsewhere", page.page_key());
        }
        assert!(seen.contains("admin-nav-row[admin-dns]"));
        assert!(seen.contains("admin-nav-row[admin-bridges-pending]"));
    }

    #[test]
    fn nest_host_extracts_host_from_scheme_and_port() {
        assert_eq!(
            nest_host("wss://nest.example.com:8443/path").as_deref(),
            Some("nest.example.com")
        );
        assert_eq!(nest_host("bare-host").as_deref(), Some("bare-host"));
    }

    /// The cases a naive `url_host`-based adapter gets wrong:
    /// echoing the original input on extraction failure happens not to
    /// contain "://" for these two inputs, so an adapter built as
    /// `let host = url_host(u); (!host.contains("://")).then_some(host)`
    /// would return `Some("")`/`Some(":")` here instead of `None`.
    #[test]
    fn nest_host_is_none_on_empty_or_bare_colon() {
        assert_eq!(nest_host(""), None);
        assert_eq!(nest_host(":"), None);
    }

    #[test]
    fn qualify_handle_leaves_an_already_qualified_handle_unchanged() {
        assert_eq!(
            qualify_handle("alice@other.example".to_string(), "wss://nest.example.com"),
            "alice@other.example"
        );
    }

    #[test]
    fn qualify_handle_appends_the_nest_host() {
        assert_eq!(
            qualify_handle("alice".to_string(), "wss://nest.example.com:8443"),
            "alice@nest.example.com"
        );
    }

    /// When the nest URL yields no host, the handle stays unqualified rather
    /// than gaining a garbage `@`/`@:` suffix — the regression a prior adapter
    /// analysis flagged as the real, if rare, failure mode to guard against.
    #[test]
    fn qualify_handle_stays_unqualified_when_nest_host_is_unavailable() {
        assert_eq!(qualify_handle("alice".to_string(), ""), "alice");
        assert_eq!(qualify_handle("alice".to_string(), ":"), "alice");
    }

    /// The gate fold sets the app-level `am_i_admin` flag (which drives the
    /// sidebar), not page state — proven from both arms, fail-closed.
    #[test]
    fn gate_outcome_drives_the_app_flag() {
        let mut app = crate::app::tests::test_app();
        assert!(!app.am_i_admin, "fail-closed default");
        apply_outcome(&mut app, Outcome::GateLoaded { is_admin: true });
        assert!(app.am_i_admin);
        apply_outcome(&mut app, Outcome::GateLoaded { is_admin: false });
        assert!(!app.am_i_admin);
    }

    fn caldav_snapshot(enabled: bool, port: u16, error: Option<&str>) -> CaldavPolicySnapshot {
        CaldavPolicySnapshot {
            caldav_enabled: enabled,
            caldav_port: port,
            status: CaldavPolicyStatus::Idle,
            error: error.map(str::to_string),
        }
    }

    /// A CalDAV snapshot fold stores the snapshot, re-seeds the port draft from the
    /// persisted value, and clears a prior error; a snapshot carrying an error
    /// bridges it onto `error-message` (the shared-manager error-bridge discipline).
    #[test]
    fn caldav_snapshot_seeds_port_and_bridges_error() {
        let mut app = crate::app::tests::test_app();
        // A failing snapshot bridges its error onto the admin page.
        apply_outcome(
            &mut app,
            Outcome::CaldavSnapshot(caldav_snapshot(false, 8443, Some("boom"))),
        );
        assert_eq!(
            app.errors.get(&Page::Admin).map(String::as_str),
            Some("boom")
        );
        assert_eq!(app.admin.caldav_port_input, "8443");
        // A clean snapshot clears the error and re-seeds the port draft.
        apply_outcome(
            &mut app,
            Outcome::CaldavSnapshot(caldav_snapshot(true, 9443, None)),
        );
        assert!(!app.errors.contains_key(&Page::Admin));
        assert_eq!(app.admin.caldav_port_input, "9443");
        assert!(app.admin.caldav_snapshot.as_ref().unwrap().caldav_enabled);
    }

    /// `SaveCaldavPort` validation: a valid `[1, 65535]` draft surfaces no error
    /// (the dispatch itself needs a wired machine, tested by the e2e); an
    /// out-of-range or unparseable draft surfaces the shared invalid message on
    /// `error-message` and dispatches no op.
    #[test]
    fn save_caldav_port_validates_client_side() {
        let mut app = crate::app::tests::test_app();
        for good in ["8443", "1", "65535"] {
            app.errors.remove(&Page::Admin);
            app.admin.caldav_port_input = good.to_string();
            apply_local(&mut app, Action::SaveCaldavPort);
            assert!(
                !app.errors.contains_key(&Page::Admin),
                "{good:?} is a valid port (no invalid message)"
            );
        }
        for bad in ["0", "70000", "", "abc", "-1"] {
            app.errors.remove(&Page::Admin);
            app.admin.caldav_port_input = bad.to_string();
            let op = apply_local(&mut app, Action::SaveCaldavPort);
            assert!(op.is_none(), "{bad:?} dispatches no op");
            assert_eq!(
                app.errors.get(&Page::Admin).map(String::as_str),
                Some(t::calendar_page::CALDAV_PORT_INVALID),
                "{bad:?} surfaces the invalid message",
            );
        }
    }

    /// The takedown console's arm honours the shared fold: a citation-less
    /// takedown press is a no-op even if forced (the button renders disabled),
    /// the SAME empty reference in restore mode arms (the wire guard's
    /// asymmetry), and arming CAPTURES the form — a later edit changes neither
    /// the summary the admin read nor what a confirm dispatches.
    #[test]
    fn arming_the_takedown_confirm_honours_the_shared_guard_and_captures() {
        let mut app = crate::app::tests::test_app();
        app.admin.takedown_content_id = "ab".repeat(32);

        assert!(apply_local(&mut app, Action::OpenTakedownConfirm).is_none());
        assert_eq!(
            app.admin.takedown_confirm, None,
            "a citation-less takedown must not arm"
        );

        apply_local(&mut app, Action::ToggleTakedownRestore);
        assert!(app.admin.takedown_restore);
        apply_local(&mut app, Action::OpenTakedownConfirm);
        let armed = app
            .admin
            .takedown_confirm
            .clone()
            .expect("a note-less restore arms");
        assert!(armed.form.restore);

        app.admin.takedown_content_id = "cd".repeat(32);
        assert_eq!(
            app.admin.takedown_confirm.as_ref().unwrap().form.content_id,
            "ab".repeat(32),
            "the armed form is captured, not re-derived from the drafts"
        );
    }

    /// Confirm consumes the armed surface synchronously (disarm-before-dispatch
    /// — a double click must not dispatch a second compulsory act), arming
    /// clears a stale verdict, and cancel disarms touching nothing else.
    #[test]
    fn the_takedown_confirm_disarms_first_and_a_second_click_is_inert() {
        let mut app = crate::app::tests::test_app();
        app.admin.takedown_content_id = "ab".repeat(32);
        app.admin.takedown_reference = "Court order 42/2026".into();
        app.admin.takedown_status = Some("stale verdict".into());

        apply_local(&mut app, Action::OpenTakedownConfirm);
        assert!(app.admin.takedown_confirm.is_some());
        assert_eq!(
            app.admin.takedown_status, None,
            "arming clears the previous attempt's verdict"
        );

        // The offline fixture wires no nest, so no op comes back — the disarm
        // must happen regardless.
        assert!(apply_local(&mut app, Action::ConfirmTakedown).is_none());
        assert_eq!(
            app.admin.takedown_confirm, None,
            "the first confirm consumes the armed surface"
        );
        assert!(
            apply_local(&mut app, Action::ConfirmTakedown).is_none(),
            "a second confirm has nothing left to dispatch"
        );

        apply_local(&mut app, Action::OpenTakedownConfirm);
        assert!(apply_local(&mut app, Action::CancelTakedown).is_none());
        assert_eq!(app.admin.takedown_confirm, None);
    }

    /// A served key set of `n` keys (the signer first, then retired ones), as
    /// the issuer section's snapshot leg carries it.
    fn oauth_ready(n: usize) -> OauthKeysRead {
        let keys = (0..n)
            .map(|i| fauna_client_admin::IssuerKeyRow {
                kid: format!("kid-{i}"),
                signing: i == 0,
                retired_at: (i > 0).then_some(1_700_000_000),
                served_until: (i > 0).then_some(1_700_001_200),
            })
            .collect::<Vec<_>>();
        OauthKeysRead::Ready(IssuerKeyView {
            active_kid: "kid-0".into(),
            rotation_in_flight: n > 1,
            keys,
            retirement_horizon_secs: 1_200,
        })
    }

    fn with_oauth(app: &mut App, keys: OauthKeysRead) {
        app.admin.nest_snapshot = Some(NestPageSnapshot {
            oauth: keys,
            ..Default::default()
        });
    }

    /// No control acts on a key set nobody has read: unread, failed, or no
    /// snapshot at all — the forced confirm could not name what it drops, and
    /// the ordinary arm would rotate blind. The page paints them disabled on
    /// the same test (`oauth_keys`).
    #[test]
    fn issuer_controls_refuse_until_the_key_set_has_answered() {
        let mut app = crate::app::tests::test_app();
        for keys in [
            None,
            Some(OauthKeysRead::Unread),
            Some(OauthKeysRead::Failed("unknown kind".into())),
        ] {
            app.admin.nest_snapshot = keys.map(|oauth| NestPageSnapshot {
                oauth,
                ..Default::default()
            });
            for arm in [IssuerForcedArm::IssuerKey, IssuerForcedArm::SessionSecret] {
                assert!(apply_local(&mut app, Action::OpenOauthForcedConfirm(arm)).is_none());
                assert_eq!(app.admin.oauth_confirm, None, "{arm:?} must not arm");
            }
            assert!(apply_local(&mut app, Action::RotateIssuerKey).is_none());
            assert!(!app.admin.oauth_in_flight);
            assert_eq!(
                app.admin.oauth_status, None,
                "nothing ran, nothing to report"
            );
        }
    }

    /// Arming captures the fold — the arm and the number of keys it drops — and
    /// a key-set re-read landing while armed does not change what the admin
    /// already read (the seed-rotate discipline).
    #[test]
    fn arming_a_forced_arm_captures_its_cost() {
        let mut app = crate::app::tests::test_app();
        with_oauth(&mut app, oauth_ready(2));
        app.admin.oauth_status = Some("stale verdict".into());

        assert!(
            apply_local(
                &mut app,
                Action::OpenOauthForcedConfirm(IssuerForcedArm::IssuerKey)
            )
            .is_none(),
            "arming is local — nothing dispatches"
        );
        let armed = app.admin.oauth_confirm.clone().expect("armed");
        assert_eq!(armed.arm, IssuerForcedArm::IssuerKey);
        assert_eq!(
            armed.view.summary.key,
            "admin.nest_page.oauth_force_rotate_confirm_many"
        );
        assert_eq!(armed.view.summary.args["count"], "2");
        assert_eq!(
            app.admin.oauth_status, None,
            "arming clears the last verdict"
        );

        with_oauth(&mut app, oauth_ready(1));
        assert_eq!(
            app.admin.oauth_confirm,
            Some(armed),
            "a re-read must not rewrite an armed confirm"
        );
    }

    /// Disarm-first, and a confirm only ever fires the arm it was painted for:
    /// a confirm for the secret pressed while the KEY arm is armed dispatches
    /// nothing and leaves the key confirm standing.
    #[test]
    fn the_forced_confirm_disarms_first_and_never_fires_the_other_arm() {
        let mut app = crate::app::tests::test_app();
        with_oauth(&mut app, oauth_ready(1));
        apply_local(
            &mut app,
            Action::OpenOauthForcedConfirm(IssuerForcedArm::IssuerKey),
        );

        assert!(
            apply_local(
                &mut app,
                Action::ConfirmOauthForced(IssuerForcedArm::SessionSecret)
            )
            .is_none()
        );
        assert_eq!(
            app.admin.oauth_confirm.as_ref().map(|c| c.arm),
            Some(IssuerForcedArm::IssuerKey),
            "the mismatched press must leave the armed confirm as it was"
        );

        // The offline fixture wires no nest, so no op comes back — the disarm
        // must happen regardless, and a second press has nothing to fire.
        assert!(
            apply_local(
                &mut app,
                Action::ConfirmOauthForced(IssuerForcedArm::IssuerKey)
            )
            .is_none()
        );
        assert_eq!(
            app.admin.oauth_confirm, None,
            "the first confirm consumes it"
        );
        assert!(
            apply_local(
                &mut app,
                Action::ConfirmOauthForced(IssuerForcedArm::IssuerKey)
            )
            .is_none()
        );

        apply_local(
            &mut app,
            Action::OpenOauthForcedConfirm(IssuerForcedArm::SessionSecret),
        );
        assert!(apply_local(&mut app, Action::CancelOauthForced).is_none());
        assert_eq!(app.admin.oauth_confirm, None);
    }

    /// The forced arms' kinds are spelled as literals for the offline-gate
    /// oracle — and must stay the very kinds the shared arm dispatches, so a
    /// confirm's gate and its dispatch can never name two different kinds.
    #[test]
    fn the_forced_confirm_declares_the_kind_its_arm_dispatches() {
        for arm in [IssuerForcedArm::IssuerKey, IssuerForcedArm::SessionSecret] {
            assert_eq!(
                Action::ConfirmOauthForced(arm).wire_kind(),
                Some(arm.kind()),
                "{arm:?}"
            );
        }
    }

    /// While a call is in flight every control desensitizes: each kind mints on
    /// the nest, and the ordinary arm has no confirm to disarm, so a second
    /// press would chain a second rotation onto the first.
    #[test]
    fn an_in_flight_issuer_call_desensitizes_every_control() {
        let mut app = crate::app::tests::test_app();
        with_oauth(&mut app, oauth_ready(1));
        app.admin.oauth_in_flight = true;

        assert!(
            apply_local(
                &mut app,
                Action::OpenOauthForcedConfirm(IssuerForcedArm::SessionSecret)
            )
            .is_none()
        );
        assert_eq!(app.admin.oauth_confirm, None);
        assert!(apply_local(&mut app, Action::RotateIssuerKey).is_none());
    }

    /// The ordinary rotation disarms a forced confirm armed beside it — that
    /// confirm named a key count the rotation is about to change.
    #[test]
    fn the_ordinary_rotation_disarms_a_stale_forced_confirm() {
        let mut app = crate::app::tests::test_app();
        with_oauth(&mut app, oauth_ready(1));
        apply_local(
            &mut app,
            Action::OpenOauthForcedConfirm(IssuerForcedArm::IssuerKey),
        );
        assert!(app.admin.oauth_confirm.is_some());

        let _ = apply_local(&mut app, Action::RotateIssuerKey);
        assert_eq!(app.admin.oauth_confirm, None);
    }

    /// The outcome replaces only the section's own leg: the verdict lands, the
    /// in-flight guard lifts, the key set is the re-read one — and the drafts
    /// the admin may be typing in (port, region) are untouched.
    #[test]
    fn the_issuer_outcome_replaces_only_its_own_leg() {
        let mut app = crate::app::tests::test_app();
        with_oauth(&mut app, oauth_ready(1));
        app.admin.serving_port_input = "84".into(); // mid-edit
        app.admin.region_input = "N".into(); // mid-edit
        app.admin.oauth_in_flight = true;

        apply_outcome(
            &mut app,
            Outcome::OauthDone {
                status: "Replaced.".into(),
                keys: oauth_ready(2),
            },
        );
        assert!(!app.admin.oauth_in_flight);
        assert_eq!(app.admin.oauth_status.as_deref(), Some("Replaced."));
        assert_eq!(
            app.admin.nest_snapshot.as_ref().map(|s| s.oauth.clone()),
            Some(oauth_ready(2))
        );
        assert_eq!(app.admin.serving_port_input, "84");
        assert_eq!(app.admin.region_input, "N");
    }

    /// The pin: two non-suspended users share a
    /// display LABEL, and the picker must still let the admin designate the
    /// second — its identity is the HANDLE (`admin.md` § 2 → *What identifies a
    /// user in an admin picker*), unique on the nest by construction; the label
    /// is freely editable and non-unique, so resolving on it bound first-match.
    #[test]
    fn guardian_picker_designates_by_handle_so_shared_labels_cannot_misbind() {
        let alex = |byte: u8, handle: &str| fauna_client_admin::admin::AdminUser {
            actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            label: "Alex".into(),
            handle: Some(handle.into()),
            ..Default::default()
        };
        let snapshot = UsersSnapshot {
            picker_users: vec![alex(1, "alex"), alex(2, "alex2")],
            ..Default::default()
        };
        assert_eq!(picker_option(&snapshot.picker_users[0]), "alex");
        assert_eq!(picker_option(&snapshot.picker_users[1]), "alex2");
        assert_eq!(
            resolve_guardian(&snapshot, "alex2"),
            Some(vec![2u8; 32]),
            "picking the second Alex's handle binds the second Alex's actor"
        );
    }

    /// The handle-less account (the admit form's blank handle)
    /// stays offerable by its full actor hex — unique and
    /// never empty, so the option string stays injective.
    #[test]
    fn guardian_picker_falls_back_to_actor_hex_for_a_handle_less_account() {
        let user = fauna_client_admin::admin::AdminUser {
            actor_id: fauna_protocol::ByteBuf::from(vec![3u8; 32]),
            label: "Handle-less".into(),
            ..Default::default()
        };
        let snapshot = UsersSnapshot {
            picker_users: vec![user],
            ..Default::default()
        };
        let hex = fauna_core::format::hex_full(&[3u8; 32]);
        assert_eq!(picker_option(&snapshot.picker_users[0]), hex);
        assert_eq!(resolve_guardian(&snapshot, &hex), Some(vec![3u8; 32]));
    }

    /// The read `Op::LoadDns` and `load_web_snapshot` both bare-call: a fake
    /// `RpcRequester` answers `fauna.admin.users.list` for two non-suspended
    /// users sharing a display LABEL, and the real read must still fold them
    /// to two DISTINCT options — identity is the HANDLE (`admin.md` § 2), never
    /// the freely-editable label. Exercises the actual `AdminClient::users_list`
    /// round trip, not a hand-built fixture.
    #[tokio::test]
    async fn users_actor_picker_options_reads_users_list_and_stays_injective_when_labels_collide() {
        let alex = |byte: u8, handle: &str| fauna_client_admin::admin::AdminUser {
            actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            label: "Alex".into(),
            handle: Some(handle.into()),
            ..Default::default()
        };
        let fake = fauna_client_testkit::RejectingRequester::new().reply(
            "fauna.admin.users.list",
            &fauna_client_admin::admin::AdminUsersListReply {
                users: vec![alex(1, "alex"), alex(2, "alex2")],
                total: 2,
                ..Default::default()
            },
        );
        assert_eq!(
            users_actor_picker_options(&fake).await,
            vec![
                (vec![1u8; 32], "alex".to_string()),
                (vec![2u8; 32], "alex2".to_string()),
            ],
        );
    }

    /// The read `Op::LoadDns` and `load_web_snapshot` both bare-call offers every
    /// account on the nest, not one `users.list` page (`admin.md` § 2 → *Which
    /// accounts a picker offers*): the box claimer, on the second page of a nest
    /// past one page, is still offered — and the read stops at `total`.
    #[tokio::test]
    async fn users_actor_picker_options_offers_an_account_beyond_the_first_page() {
        let account = |byte: u8, handle: &str| fauna_client_admin::admin::AdminUser {
            actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            handle: Some(handle.into()),
            ..Default::default()
        };
        let page = |users: Vec<fauna_client_admin::admin::AdminUser>, total: i64| {
            fauna_protocol::encode_canonical(&fauna_client_admin::admin::AdminUsersListReply {
                users,
                total,
                ..Default::default()
            })
            .expect("encode users page")
            .to_vec()
        };
        let fake = fauna_client_testkit::ScriptedRequester::new([
            page(vec![account(1, "newest")], 2),
            page(vec![account(2, "claimer")], 2),
        ]);
        assert_eq!(
            users_actor_picker_options(&fake).await,
            vec![
                (vec![1u8; 32], "newest".to_string()),
                (vec![2u8; 32], "claimer".to_string()),
            ],
        );
        assert_eq!(fake.remaining(), 0, "no request past total");
    }

    /// DNS catch-all/role-address leg: two
    /// non-suspended users share a display LABEL, and `resolve_dns_actor` must
    /// still designate the second — its identity is the HANDLE (`admin.md` § 2),
    /// unique on the nest by construction. Before the fix `dns_actors` carried
    /// the raw editable `label` and this bound first-match (actor #1). Builds
    /// `dns_actors` through [`users_actor_picker_options`] — the same fn
    /// `Op::LoadDns`'s real build site bare-calls — rather than re-deriving state
    /// via `actor_picker_options` directly.
    #[tokio::test]
    async fn resolve_dns_actor_designates_by_handle_so_shared_labels_cannot_misbind() {
        let alex = |byte: u8, handle: &str| fauna_client_admin::admin::AdminUser {
            actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            label: "Alex".into(),
            handle: Some(handle.into()),
            ..Default::default()
        };
        let fake = fauna_client_testkit::RejectingRequester::new().reply(
            "fauna.admin.users.list",
            &fauna_client_admin::admin::AdminUsersListReply {
                users: vec![alex(1, "alex"), alex(2, "alex2")],
                total: 2,
                ..Default::default()
            },
        );
        let state = AdminState {
            dns_actors: users_actor_picker_options(&fake).await,
            ..Default::default()
        };
        assert_eq!(
            resolve_dns_actor(&state, "alex2", t::dns::CATCH_ALL_NONE),
            Some(vec![2u8; 32]),
            "picking the second Alex's handle binds the second Alex's actor"
        );
    }

    /// Web apex leg: two non-suspended users
    /// share a display LABEL, and `resolve_apex_label` must still designate the
    /// second — its identity is the HANDLE (`admin.md` § 2), unique on the nest
    /// by construction. Before the fix `AdminWebSnapshot.actors` carried the raw
    /// editable `label` and this bound first-match (actor #1). Builds the
    /// snapshot by calling [`load_web_snapshot`] itself — the real
    /// `Op::LoadWeb` build site — against a fake `RpcRequester`, rather than
    /// hand-constructing `AdminWebSnapshot`.
    #[tokio::test]
    async fn resolve_apex_label_designates_by_handle_so_shared_labels_cannot_misbind() {
        let alex = |byte: u8, handle: &str| fauna_client_admin::admin::AdminUser {
            actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            label: "Alex".into(),
            handle: Some(handle.into()),
            ..Default::default()
        };
        let fake = fauna_client_testkit::RejectingRequester::new()
            .reply(
                "fauna.web.get_apex_actor",
                &fauna_protocol::web::WebGetApexActorReply::default(),
            )
            .reply(
                "fauna.admin.users.list",
                &fauna_client_admin::admin::AdminUsersListReply {
                    users: vec![alex(1, "alex"), alex(2, "alex2")],
                    total: 2,
                    ..Default::default()
                },
            );
        let snap = load_web_snapshot(&fake).await;
        assert_eq!(
            resolve_apex_label(&snap, "alex2"),
            Some(ApexPick::Designate(vec![2u8; 32])),
            "picking the second Alex's handle binds the second Alex's actor"
        );
    }

    /// The bug this whole row fixes, isolated at its actual source: [`picker_option`]
    /// itself is injective, but the two build sites had drifted onto raw
    /// `u.label.clone()`. Two users sharing a label but distinct handles must
    /// produce two DISTINCT option strings, not a collision an admin can't
    /// disambiguate in the picker.
    #[test]
    fn actor_picker_options_stays_injective_when_labels_collide() {
        let alex = |byte: u8, handle: &str| fauna_client_admin::admin::AdminUser {
            actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            label: "Alex".into(),
            handle: Some(handle.into()),
            ..Default::default()
        };
        let users = [alex(1, "alex"), alex(2, "alex2")];
        let options = fauna_client_admin::actor_picker_options(&users);
        assert_eq!(
            options,
            vec![
                (vec![1u8; 32], "alex".to_string()),
                (vec![2u8; 32], "alex2".to_string()),
            ],
        );
    }

    fn confirmable_roster() -> SeedRotateConfirm {
        SeedRotateConfirm::Ready(Box::new(seed_rotation_confirm_view(
            &fauna_protocol::admin::AdminAdminsListReply {
                admins: vec![fauna_protocol::admin::AdminAdminEntry {
                    actor_id: fauna_protocol::ByteBuf::from(vec![3u8; 32]),
                    added_at: 1,
                    ..Default::default()
                }],
                ..Default::default()
            },
            &[],
        )))
    }

    /// Arming enters `Loading` **synchronously** — tui paints no modal, so a
    /// confirm that only appeared once the roster read answered would read as a
    /// dead button under load. Cancel disarms and touches nothing else.
    #[test]
    fn arming_the_rotation_confirm_is_synchronous_and_cancel_disarms() {
        let mut app = crate::app::tests::test_app();
        app.admin.seed_rotate_status = Some("stale verdict".into());

        // No wired nest in the offline fixture, so no op is dispatched — but the
        // arming itself must not depend on that.
        apply_local(&mut app, Action::OpenSeedRotateConfirm);
        assert!(matches!(
            app.admin.seed_rotate_confirm,
            None | Some(SeedRotateConfirm::Loading)
        ));

        app.admin.seed_rotate_confirm = Some(SeedRotateConfirm::Loading);
        assert!(apply_local(&mut app, Action::CancelSeedRotate).is_none());
        assert_eq!(app.admin.seed_rotate_confirm, None);
    }

    /// Confirming against an **unresolved** roster dispatches nothing and leaves
    /// the surface exactly as it was: the ceremony's precondition is naming the
    /// set that inherits (`box-recovery.md` § Deployment-seed rotation →
    /// *Ordering rule*), so a confirm that slipped through while the read was in
    /// flight — or after it failed — must not flip the box's identity.
    #[test]
    fn confirming_an_unresolved_roster_dispatches_nothing_and_stays_armed() {
        let mut app = crate::app::tests::test_app();
        for armed in [
            SeedRotateConfirm::Loading,
            SeedRotateConfirm::Failed("nest unreachable".into()),
            // Resolved, but the fold refused it (an empty roster is self-refuting).
            SeedRotateConfirm::Ready(Box::new(seed_rotation_confirm_view(
                &Default::default(),
                &[],
            ))),
        ] {
            app.admin.seed_rotate_confirm = Some(armed.clone());
            let op = apply_local(&mut app, Action::ConfirmSeedRotate);
            assert!(op.is_none(), "{armed:?} must dispatch no ceremony");
            assert_eq!(
                app.admin.seed_rotate_confirm,
                Some(armed.clone()),
                "{armed:?} must stay armed — the admin keeps the surface they had"
            );
        }
    }

    /// The ceremony must be **spawned**, never awaited under the agent's reply
    /// budget. Its own committed rotation tears the box's serving generation
    /// down (`box-recovery.md` § Adoption by the running process), so the drive
    /// reconnects mid-flight by design; a budget-bounded await would abandon it
    /// between the dispatch that flipped the identity and the mark that retires
    /// the predecessor — the one window where the verdict matters most.
    #[test]
    fn the_rotation_op_outlives_the_click_that_starts_it() {
        let op = Op::RotateDeploymentSeed {
            nest: NestClient::new(
                "http://127.0.0.1:1".to_string(),
                fauna_core::identity::ActorKeypair::generate(),
            ),
            store: None,
        };
        assert!(
            crate::app::PageOp::Admin(op).outlives_click(),
            "the agent must spawn the rotation, not await it"
        );
    }

    /// The double-fire guard: confirming **disarms first**, so a second click on
    /// a still-painted button cannot dispatch a second ceremony. That is not
    /// cosmetic — a second rotation chains onto the first and strands a
    /// successor seed nobody marked as the predecessor's replacement.
    #[test]
    fn confirming_disarms_before_dispatch_so_a_double_click_cannot_rotate_twice() {
        let mut app = crate::app::tests::test_app();
        app.admin.seed_rotate_confirm = Some(confirmable_roster());

        // The offline fixture has no wired nest, so the first confirm returns
        // `None` at the `nest.clone()?` — but the disarm must already have
        // happened by then, which is exactly what makes the second click inert.
        let _ = apply_local(&mut app, Action::ConfirmSeedRotate);
        assert_eq!(
            app.admin.seed_rotate_confirm, None,
            "the confirm must be consumed by the first click, not by the reply"
        );
        assert!(
            apply_local(&mut app, Action::ConfirmSeedRotate).is_none(),
            "a second click has nothing left to confirm"
        );
    }

    /// The nav rail lists every built sub-page as an `admin-nav-row[…]` `Open`
    /// button, and it prefixes every admin surface — so `elements()` for the
    /// Dashboard starts with the rail, then the Dashboard heading.
    #[test]
    fn nav_rail_lists_built_subpages_and_prefixes_every_page() {
        let app = crate::app::tests::test_app();
        let rail = admin_nav_rail();
        assert_eq!(rail.len(), AdminPage::BUILT.len());
        assert!(
            rail.iter().all(|e| e.id.starts_with("admin-nav-row[")),
            "every rail row is an admin-nav-row[<page key>]"
        );
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        let first_page_id = ids
            .iter()
            .find(|id| !id.is_empty() && !id.starts_with("admin-nav-row["));
        assert_eq!(
            first_page_id.map(String::as_str),
            Some("admin-dashboard-heading")
        );
    }

    /// `route_subpage` maps the ui.yaml page ids the e2e driver sends onto the
    /// sub-pages; the Dashboard returns no op (its landing fetch is `nav_enter_op`'s
    /// job) and an unknown id falls back to the Dashboard rather than a wrong page.
    #[test]
    fn route_subpage_maps_page_ids() {
        let mut state = AdminState::default();
        assert!(route_subpage(&mut state, None).is_none());
        assert_eq!(state.sub, AdminPage::Dashboard);

        // No machine wired (pre-login) → the DAV routes set the sub-page but yield
        // no op; the mapping itself is what this asserts.
        route_subpage(&mut state, Some("admin-calendar"));
        assert_eq!(state.sub, AdminPage::Calendar);
        route_subpage(&mut state, Some("admin-contacts"));
        assert_eq!(state.sub, AdminPage::Contacts);
        route_subpage(&mut state, Some("admin-files"));
        assert_eq!(state.sub, AdminPage::Files);
        route_subpage(&mut state, Some("admin-nest"));
        assert_eq!(state.sub, AdminPage::Nest);
        route_subpage(&mut state, Some("admin-aliases"));
        assert_eq!(state.sub, AdminPage::Aliases);
        // The Tiers page's driver nav sends the SHORT id `settings`; both it and
        // the full `admin-settings` route to Settings.
        route_subpage(&mut state, Some("settings"));
        assert_eq!(state.sub, AdminPage::Settings);
        state.sub = AdminPage::Dashboard;
        route_subpage(&mut state, Some("admin-settings"));
        assert_eq!(state.sub, AdminPage::Settings);
        // The Users hub's driver nav sends the SHORT id `users`; both it and the
        // full `admin-users` route to Users.
        route_subpage(&mut state, Some("users"));
        assert_eq!(state.sub, AdminPage::Users);
        state.sub = AdminPage::Dashboard;
        route_subpage(&mut state, Some("admin-users"));
        assert_eq!(state.sub, AdminPage::Users);
        // An unknown sub-page still falls back to the Dashboard, not a wrong page.
        route_subpage(&mut state, Some("admin-nonexistent"));
        assert_eq!(state.sub, AdminPage::Dashboard);
    }

    // ── The offline gate's admin declarations (W4 (account-data-plane.md § Workstreams) phase 4, row 43) ──────────

    /// One instance of **every** [`Action`] variant.
    ///
    /// Hand-built on purpose, and the reason is the coverage gap it closes.
    /// Walk invariants I6/I7 (`crate::walk`) check only what a page actually
    /// *paints*, and most of this plane paints nothing offline — the controls
    /// are rendered from snapshots that never load without a nest. So the walks
    /// see a fraction of these variants, and a typo in any of the rest would
    /// read as `Available` forever (`affordance`'s ruling 2). Enumerating here
    /// is what puts every declaration in front of the registry check below.
    ///
    /// The `ACTION_COUNT` assertion is the ratchet that keeps it honest: adding
    /// a variant without adding it here fails, the same way the exhaustive
    /// `match` in [`Action::wire_kind`] makes the *declaration* unskippable.
    fn every_action() -> Vec<Action> {
        let s = || "x".to_string();
        vec![
            Action::Open(AdminPage::Dashboard),
            Action::ToggleCaldavEnabled,
            Action::SaveCaldavPort,
            Action::ToggleCarddavEnabled,
            Action::ToggleWebdavEnabled,
            Action::TogglePairing,
            Action::SaveServingPort,
            Action::SaveRegion,
            Action::WithdrawRegion,
            Action::SelectWebAppOrigin(fauna_client_admin::WebAppOrigin::Central),
            Action::SaveWebAppOrigin,
            Action::SelectNatMode(NodeMode::Public),
            Action::SaveNatMode,
            Action::RestartHost,
            Action::OpenFactoryResetConfirm,
            Action::ConfirmFactoryReset,
            Action::OpenSeedRotateConfirm,
            Action::CancelSeedRotate,
            Action::ConfirmSeedRotate,
            Action::OpenCustodyHostingRemoveConfirm {
                host_actor_id: s(),
                grant_id: vec![0],
            },
            Action::CancelCustodyHostingRemove,
            Action::ConfirmCustodyHostingRemove,
            Action::SetTakedownConversation(true),
            Action::ToggleTakedownRestore,
            Action::OpenTakedownConfirm,
            Action::CancelTakedown,
            Action::ConfirmTakedown,
            Action::RotateIssuerKey,
            Action::OpenOauthForcedConfirm(IssuerForcedArm::IssuerKey),
            Action::CancelOauthForced,
            Action::ConfirmOauthForced(IssuerForcedArm::IssuerKey),
            Action::ConfirmOauthForced(IssuerForcedArm::SessionSecret),
            Action::SelectForwarderDomain(s()),
            Action::SelectApexActor(s()),
            Action::SetLogLevel(0),
            Action::CopyLogs,
            Action::ApproveBridge {
                pubkey_hex: s(),
                role: s(),
            },
            Action::RejectBridge { pubkey_hex: s() },
            Action::OpenRotateConfirm { pubkey_hex: s() },
            Action::ConfirmRotate,
            Action::CancelRotate,
            Action::CreateForwarder,
            Action::DeleteForwarder { alias_id_hex: s() },
            Action::SaveTier { row: 0 },
            Action::AddTier,
            Action::SetMembershipTierName {
                row: 0,
                tier_name: s(),
            },
            Action::SetMembershipAdminTier { row: 0, tier: s() },
            Action::SetMembershipLapseTier { row: 0, tier: s() },
            Action::SaveMembership { row: 0 },
            Action::ClearMembership { row: 0 },
            Action::ToggleMailEnabled,
            Action::ToggleMailAutoEnable,
            Action::ToggleMail(MailToggle::SpamRejectNoRdns),
            Action::SetFcrdnsMode(s()),
            Action::SetImapDelete(s()),
            Action::SaveMailSpam,
            Action::SaveMailAuth,
            Action::SaveMailSubmission,
            Action::SaveMailImap,
            Action::SaveMailOutbound,
            Action::SaveMailAlias,
            Action::PublishSpamBaseline,
            Action::ToggleMailBaselineStanding,
            Action::RecheckMailHealth,
            Action::ResetMailWarmup,
            Action::CopyMailDelistUrl { url: s() },
            // The feature-limit editor and the report resolution, missing from
            // this corpus since they landed (their declarations went unchecked).
            Action::OpenFeatureLimitEditor(s()),
            Action::FeatureLimitOn(true),
            Action::CancelFeatureLimitEditor,
            Action::SaveFeatureLimit,
            Action::RemoveFeatureLimit,
            Action::OpenReportTakedown { report_id: s() },
            Action::ResolveReport {
                report_id: s(),
                outcome: fauna_protocol::moderation::AbuseReportOutcome::Acted,
            },
            Action::SetUserTier { row: 0, tier: s() },
            Action::EvictUser { row: 0 },
            Action::SuspendUser { row: 0 },
            Action::CancelUserEviction { row: 0 },
            Action::MakeAdmin { row: 0 },
            Action::RemoveAdmin { row: 0 },
            Action::ApprovePending { id: 1 },
            Action::CancelPending { id: 1 },
            Action::UsersNextPage,
            Action::UsersPrevPage,
            Action::SetRegistrationMode(s()),
            Action::SetAgeVerificationRequired(true),
            Action::SaveRegistration,
            Action::SetAdmitTier(s()),
            Action::AdmitUser,
            Action::SetInviteTier(s()),
            Action::SetInviteGuardian(s()),
            Action::SetInviteAgeBand(s()),
            Action::OpenInviteForm,
            Action::CancelInviteForm,
            Action::ConfirmInvite,
            Action::CopyMintedCode,
            Action::DeleteInvite { code: s() },
            Action::SetRequestTier { row: 0, tier: s() },
            Action::SetRequestGuardian {
                row: 0,
                guardian: s(),
            },
            Action::SetRequestAgeBand { row: 0, band: s() },
            Action::ApproveRequest { row: 0 },
            Action::DenyRequest { row: 0 },
            Action::RefreshDns,
            Action::OpenDnsAddDomain,
            Action::CancelDnsAddDomain,
            Action::SubmitDnsAddDomain,
            Action::RemoveDnsDomain { domain: s() },
            Action::RestoreDnsDomain { domain: s() },
            Action::ToggleDnsDomainMode {
                domain: s(),
                managed: true,
            },
            Action::ToggleDnsManageAll { managed: true },
            Action::SelectDnsCatchAll { row: 0, label: s() },
            Action::SelectDnsRoleAddress {
                row: 0,
                role: RoleAddressKind::Postmaster,
                label: s(),
            },
            Action::OpenDnsAddCredential,
            Action::CancelDnsAddCredential,
            Action::SelectDnsCredentialProvider(s()),
            Action::SubmitDnsCredential,
            Action::CopyDnsRecord { value: s() },
            Action::ClearDnsCredential { index: 0 },
            Action::OpenDnsRenameSheet { target: None },
            Action::CancelDnsRenameSheet,
            Action::SelectDnsRenameTarget(s()),
            Action::SubmitDnsRename,
            Action::OpenDnsRenameCompleteConfirm,
            Action::ConfirmDnsRenameComplete,
            Action::ExtendDnsRenameGrace,
            Action::OpenDnsRenameAbortConfirm,
            Action::ConfirmDnsRenameAbort,
            Action::IssueDnsCert {
                domain: s(),
                single_issue: true,
            },
            Action::IssueDnsCert {
                domain: s(),
                single_issue: false,
            },
            Action::CompleteDnsManualIssue,
            Action::CancelDnsManualIssue,
            Action::OpenDnsDelegate { domain: s() },
            Action::CancelDnsDelegate,
            Action::SelectDnsDelegateZone(s()),
            Action::SubmitDnsDelegate,
            Action::RemoveDnsDelegation { domain: s() },
            Action::ToggleDnsAutoRenew {
                domain: s(),
                enabled: true,
            },
        ]
    }

    /// `Action` has this many variants. [`every_action`] carries at least one
    /// instance of each — a second `IssueDnsCert` and `ConfirmOauthForced`,
    /// because each one's two arms answer different kinds — so the check counts
    /// distinct variants, not instances.
    ///
    /// ⚠ The count is hand-kept: it held at 104 while ten variants (the
    /// takedown console, the custody-hosting remove, make/remove admin) were
    /// added to the enum and never to the corpus, so their declarations went
    /// unchecked until the issuer controls' pass re-derived it. Re-count from
    /// the enum when adding one, never by incrementing.
    const ACTION_COUNT: usize = 134;

    #[test]
    fn every_admin_action_is_in_the_corpus() {
        let distinct: std::collections::HashSet<_> =
            every_action().iter().map(std::mem::discriminant).collect();
        assert_eq!(
            distinct.len(),
            ACTION_COUNT,
            "a new `Action` variant must be added to `every_action` — otherwise \
             its wire-kind declaration is never checked against the registry"
        );
    }

    /// I7 at the type level: every kind this page declares must be one the
    /// shared table knows. `affordance` reads an unregistered kind as
    /// `Available` by design (ruling 2 — forward compatibility with a future
    /// nest), so a misspelled kind here silently *ungates* that affordance and
    /// nothing reports it. The walk's I7 catches this only for elements it
    /// paints; this catches it for all 102.
    #[test]
    fn every_declared_admin_kind_is_registered() {
        crate::test_support::assert_every_wire_kind_is_registered(every_action(), |a| {
            a.wire_kind()
        });
    }

    /// The deployment-mutating half desensitizes offline. One representative
    /// per sub-page, so a regression that drops a whole page's declarations is
    /// caught by name rather than by an aggregate count.
    ///
    /// The **exact kind** is asserted, not just its class, and that is the
    /// point: with a dozen sibling kinds sharing one class, a class-only
    /// assertion passes when two arms are swapped. Today that swap is invisible
    /// (both grey out), but the declaration's whole value is that a later
    /// reclassification reaches this page for free — which a wrong kind
    /// silently defeats.
    #[test]
    fn admin_deployment_mutations_are_online_only() {
        use fauna_protocol::offline_class::{OfflineClass, offline_class};
        for (action, expected) in [
            (
                Action::ToggleCaldavEnabled,
                "fauna.bridges.set_caldav_enabled",
            ),
            (Action::TogglePairing, "fauna.admin.services.update"),
            (Action::ConfirmFactoryReset, "fauna.admin.factory_reset"),
            (
                Action::ConfirmSeedRotate,
                "fauna.admin.deployment_seed.rotate",
            ),
            (Action::RotateIssuerKey, "fauna.oauth.rotate_issuer_key"),
            (
                Action::ConfirmOauthForced(IssuerForcedArm::IssuerKey),
                "fauna.oauth.force_rotate_issuer_key",
            ),
            (
                Action::ConfirmOauthForced(IssuerForcedArm::SessionSecret),
                "fauna.oauth.force_rotate_session_secret",
            ),
            (
                Action::ApproveBridge {
                    pubkey_hex: String::new(),
                    role: String::new(),
                },
                "fauna.bridges.approve_pending_bridge",
            ),
            (Action::CreateForwarder, "fauna.bridges.create_forwarder"),
            (Action::SaveTier { row: 0 }, "fauna.admin.tiers.update"),
            (Action::AddTier, "fauna.admin.tiers.create"),
            (Action::SaveMailSpam, "fauna.bridges.put_spam_policy"),
            (Action::SaveMailAuth, "fauna.bridges.put_auth_policy"),
            (Action::EvictUser { row: 0 }, "fauna.admin.users.evict"),
            (Action::MakeAdmin { row: 0 }, "fauna.admin.admins.add"),
            (Action::RemoveAdmin { row: 0 }, "fauna.admin.admins.remove"),
            // `ApprovePending` / `CancelPending` ride the user-class
            // `fauna.pending_actions.{approve,cancel}` kinds, which the protocol
            // table classes OfflineQueued (the settings page's cancel queues
            // too) — they are corpus-checked above, not online-only.
            (Action::ConfirmInvite, "fauna.admin.invite_codes.create"),
            (
                Action::SelectApexActor(String::new()),
                "fauna.web.set_apex_actor",
            ),
            (Action::SubmitDnsAddDomain, "fauna.bridges.add_local_domain"),
            (
                Action::SubmitDnsRename,
                "fauna.bridges.start_primary_domain_rename",
            ),
            (Action::CompleteDnsManualIssue, "fauna.tls.publish_cert"),
        ] {
            assert_eq!(
                action.wire_kind(),
                Some(expected),
                "{action:?} must declare exactly {expected}"
            );
            assert_eq!(
                offline_class(expected),
                Some(OfflineClass::OnlineOnly),
                "{action:?} → {expected} must be OnlineOnly so the control \
                 greys out with no nest"
            );
        }
    }

    /// The counterweight, and the finding this leg turned up: a slice of
    /// `admin-dns` writes the admin's OWN `fauna.state.dns` document, not
    /// deployment state, so it is `OfflineSafe` and must STAY LIVE. Reading
    /// "the admin plane is online-only" as a blanket rule would grey these on a
    /// guess — the over-claim the three rulings exist to prevent.
    #[test]
    fn the_admin_config_plane_slice_stays_available_offline() {
        use fauna_protocol::offline_class::{OfflineClass, affordance, offline_class};
        for action in [
            Action::ToggleDnsDomainMode {
                domain: String::new(),
                managed: true,
            },
            Action::ToggleDnsManageAll { managed: true },
            Action::SubmitDnsCredential,
            Action::ClearDnsCredential { index: 0 },
            Action::ToggleDnsAutoRenew {
                domain: String::new(),
                enabled: true,
            },
            Action::SubmitDnsDelegate,
            Action::RemoveDnsDelegation {
                domain: String::new(),
            },
            Action::CancelDnsManualIssue,
        ] {
            let kind = action
                .wire_kind()
                .unwrap_or_else(|| panic!("{action:?} must declare a wire kind"));
            assert_eq!(
                offline_class(kind),
                Some(OfflineClass::OfflineSafe),
                "{action:?} → {kind} writes the admin's own config document"
            );
            assert!(
                affordance(kind, "disconnected").is_available(),
                "{action:?} must stay actuable with no nest — greying a control \
                 that works offline is the worse failure"
            );
        }
    }

    /// `IssueDnsCert` is the one admin gesture whose class depends on state —
    /// and the discriminant is carried ON the action, which is exactly what
    /// makes it gateable (the fix caution 1 of row 43 describes). The
    /// managed/delegated path delivers the cert to the nest; manual phase 1
    /// only opens the CA order and stashes a breadcrumb in the admin's config.
    /// The two must not collapse to one answer.
    #[test]
    fn issue_cert_splits_on_the_discriminant_it_carries() {
        let managed = Action::IssueDnsCert {
            domain: "example.test".to_string(),
            single_issue: true,
        };
        let manual = Action::IssueDnsCert {
            domain: "example.test".to_string(),
            single_issue: false,
        };
        assert_eq!(managed.wire_kind(), Some("fauna.tls.publish_cert"));
        assert_eq!(manual.wire_kind(), Some("fauna.account.state.put"));
        assert_ne!(
            managed.wire_kind(),
            manual.wire_kind(),
            "the two issuance paths differ in class; collapsing them would \
             either grey a working manual order or ungate a real delivery"
        );
    }

    /// Navigation stays live with no nest. Greying the rail or the pagination
    /// would strand an admin on whatever sub-page they were on when the
    /// connection dropped, with a page they can still read (caution 2).
    #[test]
    fn admin_navigation_is_never_gated() {
        for action in [
            Action::Open(AdminPage::Users),
            Action::RefreshDns,
            Action::UsersNextPage,
            Action::UsersPrevPage,
        ] {
            assert_eq!(
                action.wire_kind(),
                None,
                "{action:?} fires a whole page's read set — there is no single \
                 kind to name, and gating navigation strands the admin"
            );
        }
    }

    /// A draft picker and an inline confirm dispatch nothing until the Save or
    /// Confirm beside them, which IS declared. Gating the draft would disable
    /// the admin's ability to *compose* a change they could then submit on
    /// reconnect.
    #[test]
    fn admin_drafts_and_confirm_arming_declare_nothing() {
        for action in [
            Action::SetMembershipAdminTier {
                row: 0,
                tier: String::new(),
            },
            Action::ToggleMail(MailToggle::AuthEnforceDkim),
            Action::SetRegistrationMode(String::new()),
            Action::OpenFactoryResetConfirm,
            Action::OpenRotateConfirm {
                pubkey_hex: String::new(),
            },
            Action::CopyDnsRecord {
                value: String::new(),
            },
        ] {
            assert_eq!(action.wire_kind(), None, "{action:?} issues nothing");
        }
    }

    /// The pin — `docs/goal/behavior/admin.md` § 2 *What identifies
    /// a user in an admin picker* requires a string-round-tripping picker's
    /// option texts to be injective by construction. Row 99 made the user
    /// half injective (`picker_option` = handle, else full actor hex); this
    /// covers the other half — every non-user sentinel sharing the same
    /// `Vec<String>` namespace must be structurally unable to collide with a
    /// handle. Today that holds only by accident of English spelling (a
    /// capital letter, a space, punctuation); a lowercased i18n edit or a
    /// second locale could make one a valid handle, and every resolver here
    /// tests the sentinel FIRST — a user holding it would silently resolve
    /// to "clear" before the user list is ever consulted.
    #[test]
    fn admin_picker_sentinels_can_never_be_valid_handles() {
        for sentinel in [
            GUARDIAN_NONE_VALUE,
            t::dns::CATCH_ALL_NONE,
            t::dns::ROLE_ADDRESS_ADMIN_DEFAULT,
            t::web_page::APEX_NONE,
            t::actor_id_fallback_label(&"00112233".repeat(8)).as_str(),
        ] {
            assert!(
                fauna_protocol::handle::validate_handle(sentinel).is_err(),
                "picker sentinel {sentinel:?} is a VALID HANDLE — a user \
                 holding it would silently resolve to \"clear\" on that picker"
            );
        }
    }
}
