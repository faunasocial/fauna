use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub use fauna_client_capabilities::custody_hosting::AdminHostingSnapshot;
use fauna_client_dns::{DnsSnapshot, RecordVerdict, VerifyStatus};
use fauna_client_mail_settings::bridge_approval::{
    ApprovedBridgeView, BridgeApprovalSnapshot, PendingBridgeView,
};
use fauna_client_mail_settings::local_domains::LocalDomainsSnapshot;

use fauna_log::LogEntry;

use crate::client::FaunaClient;
use crate::i18n::resolve_key as resolve_i18n;
use crate::i18n::strings::{admin, common};
use crate::logs_view;

/// The custody-hosting registry's row key: `(host_actor_id, grant_id)`. A
/// remove-confirm names exactly this pair, never a painted index.
type CustodyHostingRowKey = (String, Vec<u8>);

/// Widget handles for admin sub-pages that need dynamic updates.
pub struct AdminHandles {
    /// `admin-nav-back` — the shell-header "leave admin" button (admin.md
    /// § Navigation model). Click handler is wired in `app.rs` (it owns the
    /// main stack + sidebar needed to switch back to the non-admin app).
    pub nav_back: gtk::Button,
    /// The sub-stack the rail switches. `app.rs` seats it on [`CANONICAL_ENTRY`]
    /// whenever the content stack *enters* this shell — the canonical-entry rule
    /// (`docs/goal/ui/README.md` § Navigation model): sub-page position is shell
    /// state, not session state, so it never survives leaving the shell.
    pub sub_stack: gtk::Stack,
    // --- Dashboard sub-page (existing) ---
    pub stats_users_row: adw::ActionRow,
    pub stats_storage_row: adw::ActionRow,
    pub stats_inbox_row: adw::ActionRow,
    pub stats_sessions_row: adw::ActionRow,
    // Hidden per-card E2E value markers (`admin-stat-card-value`).
    pub stats_users_value: gtk::Label,
    pub stats_storage_value: gtk::Label,
    pub stats_inbox_value: gtk::Label,
    pub stats_sessions_value: gtk::Label,
    pub dashboard_users_group: adw::PreferencesGroup,
    pub server_version_row: adw::ActionRow,
    pub server_version_value: gtk::Label,
    pub server_uptime_row: adw::ActionRow,
    pub server_workers_row: adw::ActionRow,
    // --- Users hub (`admin-users`, five sections — admin.md § Users) ---
    // Section "Users": the user list + per-row tier picker.
    pub users_list_box: gtk::ListBox,
    pub users_page_count: gtk::Label,
    /// Current `users.list` offset (admin.md § Users — pagination). The prev/next
    /// buttons mutate this and refetch; refetch-after-action (evict / tier change)
    /// re-reads it so the user stays on the same page.
    pub users_offset: Rc<std::cell::Cell<i64>>,
    /// Last-seen unpaginated user total (`AdminUsersListReply.total`); bounds the
    /// next-page button (disabled when `offset + page >= total`).
    pub users_total: Rc<std::cell::Cell<i64>>,
    /// Prev/next page buttons — kept so `update_users_page` can reflect the
    /// at-bounds state via `set_sensitive` (the click handlers also guard).
    pub users_prev_page: gtk::Button,
    pub users_next_page: gtk::Button,
    /// Page-position indicator (`admin-users-pagination`) — a `gtk::Label` (a
    /// bare `gtk::Box` doesn't surface its id to AT-SPI, so the container id
    /// rides this label, the same idiom as the section anchors). Updated to the
    /// "Page X of Y" position in `update_users_page`.
    pub users_pagination: gtk::Label,
    // Section "Invite": mint-a-code form + codes list + copy button.
    pub invite_codes_list: gtk::ListBox,
    pub create_invite_btn: gtk::Button,
    /// Mint-form guardian picker (`admin-users-invite-guardian-select`): the
    /// guardian the redeemed account is supervised by, default "None"
    /// (`family-safety.md` § App surface → Admission surfaces). Repopulated on
    /// every `AdminUsersLoaded` (`refresh_guardian_pickers`).
    pub invite_guardian_select: GuardianSelect,
    /// Copy the freshly minted token (`admin-users-invite-code-copy-btn`).
    pub invite_copy_btn: gtk::Button,
    /// The last minted token (what the copy button copies; also surfaced as the
    /// copy button's tooltip/label so a headless e2e can read it).
    pub minted_code: Rc<RefCell<String>>,
    // Section "Pending requests": folded-in invite-request rows.
    pub invite_requests_list: gtk::ListBox,
    /// The last `fauna.admin.invite_requests.list` reply. Cached so the pending
    /// rows can be re-rendered when the **users** list lands out of order — each
    /// row's `invite-request-row-guardian-select` draws its options from
    /// `picker_users`, and the two fetches race (`refresh_guardian_pickers`).
    pub last_invite_requests:
        Rc<RefCell<Option<fauna_client_admin::admin::AdminInviteRequestsListReply>>>,
    /// Defined tier names (`fauna.admin.tiers.list`); every tier picker on the
    /// page cycles through these. Seeded with the nest defaults so the pickers
    /// work before `AdminTiersLoaded` lands; overwritten on arrival.
    pub tier_names: Rc<RefCell<Vec<String>>>,
    /// The hub's dedicated `admin-users-action-error` label (admin.md § Errors —
    /// Users-section action failures route here, NOT the app-wide `error-message`
    /// banner). Set by `app.rs`'s `ActionResult::Failed` routing via
    /// `set_users_action_error` for every `is_users_hub_context`; cleared on the
    /// hub's success re-renders (`update_users_page` / `update_invite_codes` /
    /// `update_invite_requests`).
    pub users_action_error: gtk::Label,
    // Section "Registration": the nest's registration posture + the orthogonal
    // free-tier ceiling (admin.md § 2 Users → Section 2 — Registration). A
    // `None`/unparseable `fauna.setup.status` mode renders the section read-only
    // (`registration_readonly_label` shown, `registration_editable_box` hidden) —
    // public-mode.md § Registration Modes + Implementation status today.
    pub registration_readonly_label: gtk::Label,
    pub registration_editable_box: gtk::Box,
    /// The mode `DropDown` — model = the three wire values
    /// (`REGISTRATION_MODE_WIRE_VALUES`), rendered with a custom
    /// `SignalListItemFactory` showing the localized label
    /// (`build_registration_mode_dropdown`).
    pub registration_mode_select: gtk::DropDown,
    pub registration_max_free_users_input: gtk::Entry,
    /// `admin-users-registration-age-verification-toggle` — the DRAFT of the
    /// "accept only signups carrying app age verification" require-knob
    /// (`family-safety.md` § App surface → *Age-band surfaces*), seeded from
    /// `fauna.setup.status` and saved by the section's one save button.
    pub registration_age_verification_toggle: gtk::Switch,
    /// The knob's persisted value as last read from `fauna.setup.status` — the
    /// save sends `fauna.admin.set_age_verification_required` only when the
    /// draft differs from it.
    pub registration_age_verification_persisted: Rc<std::cell::Cell<bool>>,
    // --- Settings sub-page (`admin-settings`, nav label "Tiers") — tier
    // *definitions* only (admin.md § 3 / § Admin IA redesign). Storage-mode +
    // Factory Reset moved to the Nest page below.
    /// Tier-definition rows (`admin-settings-tier-item`, indexed), rendered from
    /// `fauna.admin.tiers.list` (admin.md § 3 — policy, distinct from admission).
    pub settings_tiers_list: gtk::ListBox,
    /// Membership designation rows (`admin-settings-membership-item`, indexed;
    /// monetization.md § Pillar 4) — a link editor between the admin's own
    /// subscription tiers and this page's quota tiers, one row per subscription
    /// tier the admin owns. Rendered from BOTH `own_membership_tier_names`
    /// (the row set) and `last_membership_tiers` (which rows are designated) —
    /// the two fetches race, so both handlers re-render (mirrors
    /// `refresh_guardian_pickers`'s race tolerance).
    pub membership_tiers_list: gtk::ListBox,
    /// The admin's own subscription tier names (`fauna.subscriptions.tiers.list`)
    /// — the row set for the membership section. Empty is the normal
    /// out-of-the-box state (no subscription tiers minted yet).
    pub own_membership_tier_names: Rc<RefCell<Vec<String>>>,
    /// The last `fauna.admin.membership_tiers.list` reply — which of the rows
    /// above already carry a designation, and what it links to. `None` until
    /// the first fetch lands (renders every row as undesignated in the
    /// meantime, corrected on arrival — never a stale false-empty claim held
    /// past the first real read).
    pub last_membership_tiers:
        Rc<RefCell<Option<fauna_client_admin::admin::AdminMembershipTiersListReply>>>,
    // --- Nest sub-page (`admin-nest`) — nest-wide settings (admin.md § N Nest):
    // the admin pairing toggle + the Factory Reset danger zone. The
    // per-page-services redesign (admin.md § Admin IA redesign, 2026-06-04)
    // removed the standalone Services page: the vestigial `bridge` + residue
    // `algorithm` toggles were dropped, mail on/off is the Mail page's
    // `admin-mail-enabled-toggle`, and the dns master switch already lives on
    // `admin-dns` (`admin-dns-manage-all-toggle`). `pairing` is the one live
    // homeless flag and lands here. The read-only storage-mode indicator
    // (`nest-mode-indicator`) is retired with the storage-mode question itself
    // (storage-modes.md § no-modes — every nest is sealed from first boot).
    /// `admin-service-pairing-toggle` + its `-status` badge — the admin
    /// nest-level pairing-policy knob (`fauna.admin.services.update` name
    /// "pairing", default-on; gates `fauna.pair.add`). Reflective: `update_services`
    /// syncs it from `AdminServicesListReply.services.pairing`, so it carries a
    /// guard against the programmatic `set_active`.
    pub svc_pairing_toggle: gtk::Switch,
    pub svc_pairing_status: gtk::Label,
    pub svc_pairing_guard: Rc<std::cell::Cell<bool>>,
    /// `admin-nest-serving-port-input` — the admin-set client-facing API serving
    /// port entry (seeded from `fauna.setup.status` reply `serving_port` by
    /// `set_nest_serving_port`; the save button drives `fauna.admin.set_serving_port`).
    pub serving_port_entry: gtk::Entry,
    /// `admin-nest-serving-port-save-button` + the read-only hint label —
    /// `set_nest_serving_port` desensitizes the entry+button and reveals the hint
    /// when `fauna.setup.status` reports `fronted_by_router` (router-fronted nest).
    pub serving_port_save: gtk::Button,
    pub serving_port_hint: gtk::Label,
    /// Declared region (`admin-nest-region-*`, `fauna.admin.region.{get,set}`)
    /// — the deployment's legal situs, the region tier's one human choice
    /// (region-blocking.md § Region determination; dynamic-features.md § The
    /// region tier). Every rendering decision is the shared
    /// `fauna_client_admin::admin_region_view` fold (tui's `admin/nest.rs`,
    /// the reference leg) — `set_nest_region` paints exactly what it hands
    /// back and this file decides nothing about the plane. ⚠ DECLARED, NEVER
    /// DETECTED (ratified 2026-08-11): no detect/prefill affordance here.
    pub region_status: gtk::Label,
    /// `admin-nest-region-authority` — present only while a region is
    /// declared (`set_nest_region` toggles visibility from the fold).
    pub region_authority: gtk::Label,
    /// `admin-nest-region-staleness` — the "rules already received stay in
    /// force" warning, present only when the nest reports the channel
    /// unreached.
    pub region_staleness: gtk::Label,
    /// `admin-nest-region-input` — the draft region code; mirrors the
    /// declaration (`set_nest_region`), so a withdrawal empties it rather
    /// than leaving the withdrawn code looking declared.
    pub region_entry: gtk::Entry,
    /// `admin-nest-region-withdraw-button` — shown only while `can_withdraw`
    /// (a region is declared); sends `fauna.admin.region.set` with the region
    /// absent, which also retires the previous region's feature-policy
    /// document nest-side.
    pub region_withdraw: gtk::Button,
    /// Host-OS-maintenance widgets (`installers/vps.md` § Host OS Maintenance § 4),
    /// hydrated by `set_nest_os_maintenance` from `fauna.setup.status` `os_*`:
    /// the always-present localized status line (`nest-os-maintenance-status`,
    /// shared `os_maintenance_status_label`), the pending-updates count badge
    /// (`nest-os-updates-count`, shown only when `os_security_updates_pending > 0`),
    /// and the "restart now" button (`nest-os-restart-now-button`, shown only when
    /// `os_reboot_pending`; drives `fauna.admin.request_host_restart`).
    pub os_maintenance_status: gtk::Label,
    pub os_updates_count: gtk::Label,
    pub os_restart_now_button: gtk::Button,
    // --- Bridges-pending sub-page ---
    /// Pending-bridge approval cards (`admin-bridges-pending-card`, indexed).
    pub pending_bridges_list: gtk::ListBox,
    /// Approved-bridge roster cards (`admin-bridges-approved-card`, indexed) —
    /// the rotate-service-user-key surface below the pending cards.
    pub approved_bridges_list: gtk::ListBox,
    /// The bridges-pending page's `error-message` label — shows
    /// `BridgeApprovalSnapshot.error` when a fetch/approve/reject/rotate fails.
    pub pending_bridges_error: gtk::Label,
    // --- DNS sub-page (`admin-dns`) — the unified domain-management surface ---
    /// Per-domain DNS sections (`admin-dns-domain`, indexed) + the soft-deleted
    /// `admin-dns-removed-domain` rows. Rebuilt on every snapshot update.
    pub dns_records_list: gtk::ListBox,
    /// The DNS page's `error-message` label — shows `LocalDomainsSnapshot.error`
    /// (CRUD feedback) or `DnsSnapshot.error` (list/verify) when a fetch fails.
    pub dns_records_error: gtk::Label,
    /// Held DNS-provider credentials (`admin-dns-credentials-list`, holding
    /// indexed `admin-dns-credential-item` rows). Rebuilt from
    /// `DnsSnapshot.credentials` on every snapshot update (the managed-mode
    /// "Fauna controls DNS" credential store; client-held, never on the nest).
    pub dns_credentials_list: gtk::Box,
    /// Deployment "Fauna controls all domains" master switch
    /// (`admin-dns-manage-all-toggle`). Its active state is reflective —
    /// `render_admin_dns` syncs it to "every active domain is effectively
    /// managed" — so we sync it through `dns_manage_all_guard`.
    pub dns_manage_all_toggle: gtk::ToggleButton,
    /// Guards the reflective `set_active` on `dns_manage_all_toggle`: set true
    /// around the programmatic sync so its `toggled` handler skips dispatching
    /// `dns_set_all_managed` (only genuine user clicks dispatch).
    pub dns_manage_all_guard: Rc<std::cell::Cell<bool>>,
    /// Primary-domain-rename UX on `admin-dns` (mail-primary-domain-rename.md
    /// § UX surface): the deployment-wide in-flight banner + the start-a-rename
    /// wizard sheet, both page-level singletons built once. `render_admin_dns`
    /// repopulates the sheet's target picker + rebuilds the banner each render.
    pub dns_rename: RenameUi,
    /// The add-domain form's irreversibility warning (untagged chrome, no
    /// ui.yaml id) — visible only while the form is open AND
    /// `dns_adding_first_domain` says so (`deployment-home-with-public-relay.md`
    /// § MUA reach). `render_admin_dns` keeps the cell current; the reveal click
    /// in `build_add_domain_form` reads it to decide visibility at open time.
    pub dns_add_domain_warning: gtk::Label,
    pub dns_add_domain_form: gtk::Box,
    pub dns_adding_first_domain: Rc<std::cell::Cell<bool>>,
    /// Latest local-domains snapshot (active + soft-deleted + `is_primary`),
    /// from `fauna.bridges.list_local_domains`. The admin-dns page renders the
    /// domain list + CRUD from this; records are overlaid from `last_dns` by
    /// domain name. Cached so either snapshot's arrival re-renders the merge.
    pub last_local_domains: Rc<RefCell<Option<LocalDomainsSnapshot>>>,
    /// Latest DNS record matrix (`fauna.dns.list_records` + verify verdicts),
    /// overlaid onto the active domains by name.
    pub last_dns: Rc<RefCell<Option<DnsSnapshot>>>,
    /// Every account on the nest (`fauna_client_admin::users_list_all`) — the
    /// option source of every admin actor picker on these pages: the `admin-dns`
    /// per-domain catch-all + role-address pickers and both guardian pickers
    /// (`admin.md` § 2 → *Which accounts a picker offers*), never the Users page
    /// on screen. Arrives with `AdminUsersLoaded`, whose handler re-renders
    /// admin-dns and refills the guardian pickers so they populate regardless of
    /// order. (A designated actor missing from it is still shown selected — see
    /// `build_domain_section`.)
    pub picker_users: Rc<RefCell<Option<Vec<fauna_client_admin::AdminUser>>>>,
    // --- Aliases (`admin-aliases`) — external forwarders (admin.md § 4) ---
    /// The forwarder rows list (`admin-aliases-forwarder-list`). Rebuilt on every
    /// `AdminForwardersLoaded` from `ForwardersSnapshot.forwarders`.
    pub forwarders_list: gtk::ListBox,
    /// The add-form domain picker (`admin-aliases-forwarder-add-domain-select`),
    /// repopulated from `ForwardersSnapshot.local_domains` so a forwarder can
    /// only target a hosted domain.
    pub forwarder_add_domain: gtk::DropDown,
    /// The page's dedicated `admin-aliases-action-error` label (admin.md
    /// § Errors). Carries `ForwardersSnapshot.error`.
    pub forwarders_action_error: gtk::Label,
    // --- Logs (`admin-logs`) — the nest's fauna-log ring over WS-RPC
    // (observability.md § Surfaces). ---
    /// Render state for the admin Logs page; `update_admin_logs` refreshes its
    /// source (the fetched nest ring) + re-renders at the active filter.
    pub admin_logs_ctx: Rc<AdminLogsCtx>,
    /// The Logs page's `error-message` label — shows a `fauna.admin.logs` fetch
    /// failure.
    pub admin_logs_error: gtk::Label,
    // --- Deployment identity (`admin-nest-seed-rotate-*`) — the deployment-seed
    // rotation ceremony (`box-recovery.md` § Deployment-seed rotation) ---
    /// The inline confirm surface's container, revealed only while armed
    /// (`seed_rotate_confirm` is `Some`) — holds the roster rows, the withhold
    /// reason, and the confirm/cancel buttons. Rebuilt on every render rather
    /// than diffed: the roster is 1-3 rows, tops.
    pub seed_rotate_confirm_box: gtk::Box,
    /// Roster-row container (`admin-nest-seed-rotate-roster-item-{n}`, indexed)
    /// — cleared and rebuilt from `SeedRotateConfirmState::Ready`'s inheritors
    /// on every render; empty (no rows) in `Loading`/`Failed`, per the ordering
    /// rule (`box-recovery.md`) — an empty list beside a live confirm would
    /// read as "nobody inherits".
    pub seed_rotate_roster_box: gtk::Box,
    /// `admin-nest-seed-rotate-roster-reason` — why the confirm is withheld
    /// (loading / read failed / empty roster); hidden when there is none.
    pub seed_rotate_reason: gtk::Label,
    /// `admin-nest-seed-rotate-confirm-button` — destructive; disabled until
    /// the roster resolves with `can_confirm`.
    pub seed_rotate_confirm_button: gtk::Button,
    /// `admin-nest-seed-rotate-status` — the ceremony's own verdict, present
    /// only once one has been attempted. Not `error-message`: the outcome that
    /// matters most (`predecessor_marked: false`) is a *success with a
    /// caveat*, which an error line would misreport.
    pub seed_rotate_status: gtk::Label,
    /// The armed confirm's current state — `None` while un-armed. Mirrors
    /// tui's `AdminState::seed_rotate_confirm`: captured at arm time and never
    /// re-derived while armed (the bridges rotate-confirm discipline — a
    /// roster that shifted between arming and confirming must not silently
    /// change what the admin already read). Consumed (`take()`) by the confirm
    /// click before dispatch — disarm-before-dispatch, so a double click
    /// cannot chain a second rotation onto the first.
    pub seed_rotate_confirm: Rc<RefCell<Option<SeedRotateConfirmState>>>,
    // --- Outside-app sign-in keys (`admin-nest-oauth-*`) — the nest-held
    // OAuth issuer key set and its refresh-token secret
    // (`authorization-server.md` § The issuer → *Two rotation arms*). Every
    // sentence is a shared `fauna_client_admin` fold; this page paints and
    // wires. Mirrors tui's `admin/nest.rs` `oauth_elements` (the reference
    // leg). ---
    /// The section's state + widgets — `app.rs` hands it the entry/refresh
    /// read (`set_oauth_keys`) and each control's verdict + re-read
    /// (`set_oauth_done`).
    pub oauth: Rc<OauthSectionCtx>,
    // --- Legal takedown (`admin-nest-takedown-*`) — the legal-compulsion
    // console (`moderation.md` § Legal takedown → Invocation surface, ruled
    // 2026-08-16). Every gating/wording decision is the
    // shared `fauna_client_moderation::takedown` fold; this page paints and
    // wires. Mirrors tui's `admin/nest.rs` (the reference leg). ---
    /// `admin-nest-takedown-status` — the dispatch's own verdict, present only
    /// once an attempt has been made.
    pub takedown_status: gtk::Label,
    // --- Custody-hosting registry (`admin-custody-hosting`) — the nest-wide
    // held-for-others registry (account-data-plane.md § Two-sided bounds, reference: `apps/fauna-tui/src/admin/custody_hosting.rs`). ---
    /// `admin-custody-hosting-row` cards (indexed), rebuilt on every
    /// `AdminCustodyHostingLoaded` from `AdminHostingSnapshot.rows`
    /// (already ordered by the shared `admin_hosting_rows` fold — heaviest
    /// hold first — never re-sorted per app).
    pub custody_hosting_list: gtk::ListBox,
    /// `admin-custody-hosting-count` / `admin-custody-hosting-empty` — the
    /// pre-hydrate/populated/empty header, painted by
    /// [`update_custody_hosting`] alongside the list.
    pub custody_hosting_header: gtk::Label,
    /// The page's `error-message` label — carries
    /// `AdminHostingSnapshot.error` (a failed list or remove).
    pub custody_hosting_error: gtk::Label,
    /// The last remove's outcome sentence (not an error — `removed: false` on
    /// a row someone else already dropped is an honest no-op).
    pub custody_hosting_status: gtk::Label,
    /// The armed remove-confirm's `(host_actor_id, grant_id)` pair — `None`
    /// while un-armed. Mirrors tui's `AdminState::custody_hosting_confirm`:
    /// the confirm names ONE row, so arming a second row must retarget it
    /// rather than stack, and a landed read always disarms (the rows may have
    /// re-ordered).
    pub custody_hosting_confirm: Rc<RefCell<Option<CustodyHostingRowKey>>>,
    /// The last landed read, cached so the confirm arm/disarm/remove buttons
    /// can repaint the list without a network round trip.
    pub custody_hosting_last: Rc<RefCell<Option<AdminHostingSnapshot>>>,
}

/// The deployment-identity rotation confirm's roster-read state — mirrors
/// tui's `admin::SeedRotateConfirm` (`apps/fauna-tui/src/admin/mod.rs`).
/// Three states, deliberately distinguished: `Loading` and `Failed` paint NO
/// roster rows — an empty list beside a live confirm button would read as
/// "nobody inherits", the one thing this surface must never say
/// (`box-recovery.md` § Deployment-seed rotation → Ordering rule).
#[derive(Debug, Clone)]
pub enum SeedRotateConfirmState {
    Loading,
    Failed(String),
    Ready(Box<fauna_client_admin::SeedRotationConfirmView>),
}

/// The outside-app sign-in key set's read, as the `admin-nest-oauth-*` section
/// paints it — mirrors tui's `admin::OauthKeysRead`
/// (`apps/fauna-tui/src/admin/mod.rs`).
///
/// Three honest states, never collapsed into an empty list: "we haven't asked"
/// and "we couldn't find out" must not read like "no keys", and the forced
/// arm's confirm can only name what it drops once the set has answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OauthKeysRead {
    /// Not read yet — the section's state until the first read lands.
    #[default]
    Unread,
    /// The set answered (`AdminClient::issuer_key_status_view`); the shared
    /// folds decide every line painted from it.
    Ready(fauna_client_admin::IssuerKeyView),
    /// The read failed — already worded (`oauth_keys_error`), the section's
    /// reason line. Any read error (e.g. a transport fault) lands here.
    Failed(String),
}

/// The armed forced-rotation confirm (`admin-nest-oauth-confirm-*`): which arm,
/// and its cost as folded when it was armed — mirrors tui's `ArmedOauthForced`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedOauthForced {
    pub arm: fauna_client_admin::IssuerForcedArm,
    pub view: fauna_client_admin::IssuerForcedConfirmView,
}

/// The whole `admin-nest-oauth-*` section's state — tui's
/// `AdminState::{oauth_confirm, oauth_status, oauth_in_flight}` plus the nest
/// snapshot's `oauth` leg, held in one GTK-free value so the four gestures'
/// guards are unit-testable without a display. The click handlers
/// (`wire_oauth_section`) drive it and [`render_oauth`] paints it; nothing
/// here words a sentence — the shared `fauna_client_admin` folds do.
#[derive(Debug, Default)]
struct OauthSectionState {
    /// The key set's read (`admin-nest-oauth-key-item-{n}` / `-key-reason`).
    /// Its own result, never the page's: a failed read must not blank pairing,
    /// the serving port, the region or the seed rotation beside it.
    keys: OauthKeysRead,
    /// The armed forced confirm, `None` while un-armed. Captured at arm time
    /// and never re-folded while armed (the seed-rotate discipline): a key-set
    /// re-read landing between arming and confirming must not change the
    /// number of keys the admin already read would stop verifying.
    confirm: Option<ArmedOauthForced>,
    /// The verdict line (`admin-nest-oauth-status`), `None` until a control
    /// was used. Its own element, never `error-message`: every success here has
    /// consequences worth words (which key signs, which died).
    status: Option<String>,
    /// Whether a control's call is in flight. All three kinds mint on the nest
    /// and are replay-forbidden, so the controls desensitize while one runs — a
    /// second press would chain a second rotation onto the first (the ordinary
    /// arm has no confirm to disarm).
    in_flight: bool,
}

impl OauthSectionState {
    /// The key set, once it has answered — the one precondition all three
    /// controls share (tui's `oauth_keys`): [`render_oauth`] paints them
    /// disabled otherwise, and the gestures below refuse on the same test, so a
    /// driver-forced press cannot act on a set nobody has read.
    fn answered(&self) -> Option<&fauna_client_admin::IssuerKeyView> {
        match &self.keys {
            OauthKeysRead::Ready(view) => Some(view),
            _ => None,
        }
    }

    /// Whether the three controls are live: the set has answered and no call
    /// is in flight. Disabled, never hidden, otherwise.
    fn controls_live(&self) -> bool {
        self.answered().is_some() && !self.in_flight
    }

    /// `admin-nest-oauth-rotate-button` (tui's `Action::RotateIssuerKey`) —
    /// `true` when the ordinary rotation should dispatch.
    fn press_rotate(&mut self) -> bool {
        // The button renders disabled until the set has answered and while a
        // call is in flight; a driver-forced press honours the same two guards
        // rather than chain a second rotation.
        if !self.controls_live() {
            return false;
        }
        // A forced confirm armed beside it named a key count this rotation is
        // about to change — disarm it rather than let it state a stale cost.
        self.confirm = None;
        self.in_flight = true;
        self.status = Some(admin::nest_page::OAUTH_WORKING.to_string());
        true
    }

    /// `admin-nest-oauth-force-rotate-button` / `-secret-force-rotate-button`
    /// (tui's `Action::OpenOauthForcedConfirm`) — arm `arm`'s confirm,
    /// dispatching nothing. Its cost is folded NOW from the set the admin can
    /// see, and arming the other arm replaces it: one is armed at a time.
    fn arm(&mut self, arm: fauna_client_admin::IssuerForcedArm) {
        // Same two guards as the ordinary arm: the confirm names what it
        // drops, so it cannot be armed before the set has answered.
        if self.in_flight {
            return;
        }
        let Some(view) = self.answered() else {
            return;
        };
        let confirm = fauna_client_admin::issuer_forced_confirm_view(arm, view);
        self.status = None;
        self.confirm = Some(ArmedOauthForced { arm, view: confirm });
    }

    /// `admin-nest-oauth-cancel-button` (tui's `Action::CancelOauthForced`) —
    /// disarm, touching nothing.
    fn cancel(&mut self) {
        self.confirm = None;
    }

    /// `admin-nest-oauth-confirm-button` (tui's `Action::ConfirmOauthForced`)
    /// — the arm to dispatch, if any. Disarms FIRST: a double press must not
    /// dispatch a second forced rotation, which would drop the very key the
    /// first minted.
    fn press_confirm(&mut self) -> Option<fauna_client_admin::IssuerForcedArm> {
        let armed = self.confirm.take()?;
        if self.in_flight {
            // Dispatch nothing, and keep what the admin can see.
            self.confirm = Some(armed);
            return None;
        }
        self.in_flight = true;
        self.status = Some(admin::nest_page::OAUTH_WORKING.to_string());
        Some(armed.arm)
    }

    /// A key-set read landed on its own (the page's entry/refresh read). It
    /// replaces only the read: an armed confirm keeps the cost it captured, and
    /// an in-flight call keeps its guard.
    fn keys_loaded(&mut self, keys: OauthKeysRead) {
        self.keys = keys;
    }

    /// A control's call finished: its verdict and the key set re-read after
    /// it, landed together so the list and the sentence arrive in one paint
    /// (tui's `Outcome::OauthDone`).
    fn done(&mut self, status: String, keys: OauthKeysRead) {
        self.in_flight = false;
        self.status = Some(status);
        self.keys = keys;
    }
}

/// The `admin-nest-oauth-*` section's widgets, built once by
/// [`build_oauth_section`] and repainted whole by [`render_oauth`].
struct OauthWidgets {
    /// Key-row container (`admin-nest-oauth-key-item-{n}`, indexed) — cleared
    /// and rebuilt on every paint, one label per served key from an answered
    /// read (signer first, the nest's own order); empty otherwise.
    keys_box: gtk::Box,
    /// `admin-nest-oauth-key-reason` — why no rows are painted (the read is in
    /// flight, or failed); hidden once the set has answered.
    key_reason: gtk::Label,
    /// The ordinary arm's cost, stated beside its button because it has no
    /// confirm (untagged — the shared `issuer_key_rotate_cost`); shown only
    /// once the set has answered.
    rotate_cost: gtk::Label,
    /// `admin-nest-oauth-rotate-button` — dispatches `fauna.oauth.rotate_issuer_key`.
    rotate_button: gtk::Button,
    /// `admin-nest-oauth-force-rotate-button` — arms the forced key arm.
    force_rotate_button: gtk::Button,
    /// `admin-nest-oauth-secret-force-rotate-button` — arms the secret arm.
    secret_force_rotate_button: gtk::Button,
    /// The armed confirm's container, visible only while a forced arm is armed.
    confirm_box: gtk::Box,
    /// `admin-nest-oauth-confirm-summary` — the CAPTURED fold's cost.
    confirm_summary: gtk::Label,
    /// `admin-nest-oauth-confirm-button` — its label names the arm; declared
    /// with the armed arm's kind (`wire_oauth_section` re-declares on arm).
    confirm_button: gtk::Button,
    /// `admin-nest-oauth-cancel-button` — disarms, touching nothing.
    cancel_button: gtk::Button,
    /// `admin-nest-oauth-status` — visible only once a control was used.
    status: gtk::Label,
}

/// The `admin-nest-oauth-*` section's state and widgets in one context, so the
/// click handlers, the `DataMessage::{OauthKeysLoaded, OauthDone}` handlers and
/// the countdown tick all repaint through [`render_oauth`].
pub struct OauthSectionCtx {
    state: RefCell<OauthSectionState>,
    widgets: OauthWidgets,
}

/// Build a hidden per-page `error-message` label (cross-app rule #2: every
/// page carries one). Each admin mail sub-page owns its own instance so it
/// reflects only *its* snapshot's error, independent of sibling fetches — the
/// same per-page convention as the shipped `settings/mail.rs` page. The driver
/// filters by visibility, so a hidden label on a hidden stack page never leaks
/// into a sibling page's counts.
fn build_error_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_visible(false);
    label.set_wrap(true);
    label.add_css_class("error");
    crate::testid::set_test_id(&label, ids::ERROR_MESSAGE);
    label
}

/// Show `label` with `error`'s text, or hide it when there is no (non-empty)
/// error. The uniform error-surfacing primitive for the admin mail sub-pages
/// (mirrors `settings/mail.rs`): the page renders its snapshot's `error` onto
/// its `error-message` element on every update.
fn render_page_error(label: &gtk::Label, error: Option<&str>) {
    match error {
        Some(msg) if !msg.is_empty() => {
            label.set_text(msg);
            label.set_visible(true);
        }
        _ => {
            label.set_text("");
            label.set_visible(false);
        }
    }
}

/// True for the WS-RPC `context`s of every action on the consolidated
/// `admin-users` hub — the five sections (Pending requests / Registration /
/// Admit / Invite / Users) all route their failures through the dedicated
/// `admin-users-action-error` element rather than the app-wide `error-message`
/// banner (admin.md § Errors & edge cases — "Users (all sections):
/// `admin-users-action-error`"). The
/// `fauna.admin.{users,invite_codes,invite_requests,admins}.*` families cover
/// tier change / evict / cancel-evict, invite create+delete+list, request
/// approve+deny+list, and the roster's grant+revoke;
/// `fauna.admin.set_registration_mode` and
/// `fauna.admin.set_age_verification_required` cover the Registration
/// section's single save (flat kind names, not `fauna.admin.*.` namespace
/// families, hence the exact-match arms). Pure so `app.rs`'s
/// `ActionResult::Failed` routing is unit-testable without a nest.
pub fn is_users_hub_context(context: &str) -> bool {
    context.starts_with("fauna.admin.users.")
        || context.starts_with("fauna.admin.invite_codes.")
        || context.starts_with("fauna.admin.invite_requests.")
        || context.starts_with("fauna.admin.admins.")
        || context == "fauna.admin.set_registration_mode"
        || context == "fauna.admin.set_age_verification_required"
}

/// Render an `error` onto the Users hub's `admin-users-action-error` label
/// (`None`/empty hides it). The `app.rs` routing calls this for every
/// `is_users_hub_context` failure; the hub's success re-renders clear it.
pub fn set_users_action_error(handles: &AdminHandles, error: Option<&str>) {
    render_page_error(&handles.users_action_error, error);
}

/// Carry a switch's state on the `on`/`off` marker the cross-app toggle read
/// asserts (`get_attr(id, "state")`; an unmarked `Switch` reads `true`/`false`).
/// The marker wins over the live state, so it is re-marked on every flip.
fn mark_switch_state(sw: &gtk::Switch) {
    crate::testid::set_test_attr(sw, "state", if sw.is_active() { "on" } else { "off" });
}

/// Build the admin shell as a vertical **sidebar-swap**: returns
/// `(content, admin_sidebar, handles)`. `content` is the admin sub-stack (the
/// page area, the main content-stack's "admin" child); `admin_sidebar` is a
/// vertical rail (`admin-nav-back` on top + a `gtk::StackSidebar` driving that
/// sub-stack) that `app.rs` swaps into the main split-view sidebar slot while
/// the admin view is showing (admin.md § Navigation model). Replaces the former
/// centered horizontal `StackSwitcher`, whose summed-tab width forced the whole
/// window wide. The sub-stack stays a direct child of `content` so the test
/// agent's admin sub-page walker (`main.rs`) still finds it.
/// The sub-page this shell is entered on — the first rail entry, the Dashboard.
/// `app.rs` seats the sub-stack here on every nav edge into the shell, per the
/// canonical-entry rule (`docs/goal/ui/README.md` § Navigation model). Named
/// rather than inlined so the rule's two implementation sites (here and the
/// Settings twin) are greppable from the doc.
pub const CANONICAL_ENTRY: &str = "dashboard";

pub fn build_admin_view(client: &Rc<FaunaClient>) -> (gtk::Box, gtk::Box, AdminHandles) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    // `admin-nav-back` — the uniform "leave admin" affordance (admin.md
    // § Navigation model). Sits at the top of the admin sidebar rail so it is
    // present on every admin sub-page; clicking it exits the shell back to the
    // non-admin app. The exit target is wired in `app.rs` (it sets the content
    // stack to "status"; the sidebar swaps back off that stack's visible-child
    // notify). Rendered icon + label so it reads as a row in the vertical rail.
    let nav_back_content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    nav_back_content.append(&gtk::Image::from_icon_name("go-previous-symbolic"));
    nav_back_content.append(&gtk::Label::new(Some(admin::EXIT)));
    let nav_back = gtk::Button::builder().child(&nav_back_content).build();
    nav_back.set_widget_name("admin-nav-back");
    nav_back.set_tooltip_text(Some(admin::EXIT));
    nav_back.add_css_class("flat");
    nav_back.set_halign(gtk::Align::Fill);
    nav_back.set_margin_top(8);
    nav_back.set_margin_start(8);
    nav_back.set_margin_end(8);
    nav_back.set_margin_bottom(8);

    // --- Dashboard sub-page ---
    let (dashboard_page, dashboard_handles) = build_dashboard_page();
    stack.add_titled(&dashboard_page, Some("dashboard"), admin::dashboard::TITLE);

    // --- Users hub sub-page (`admin-users`): the consolidated five-section
    // user-administration surface — Pending requests / Registration / Admit /
    // Invite / Users (admin.md § Users). Folds in the former standalone invite-requests
    // page and the settings invite-code section. ---
    let hub = build_users_hub(client);
    stack.add_titled(&hub.page, Some("users"), admin::users_page::TITLE);

    // --- Settings sub-page (nav label "Tiers"): tier *definitions* only. The
    // invite-code section moved to the Users hub's Invite section (2026-05-29);
    // storage-mode + Factory Reset moved to the Nest page (admin.md § Admin IA
    // redesign). Child name stays `settings` (historical `admin-settings-` IDs). ---
    let (settings_page, settings_tiers_list, membership_tiers_list) = build_settings_page();
    stack.add_titled(
        &settings_page,
        Some("settings"),
        admin::settings_page::TITLE,
    );

    // --- Nest sub-page (`admin-nest`: nest-wide settings — admin.md § N Nest):
    // the admin pairing toggle (moved off the removed Services page) + the
    // Factory Reset danger zone (moved off Settings). Replaces the old
    // standalone Services page (the vestigial bridge/algorithm toggles were
    // dropped; dns lives on `admin-dns`). Child name `admin-nest` is the
    // nav-by-id target. ---
    let nest = build_nest_page(client);
    stack.add_titled(&nest.page, Some("admin-nest"), admin::nest_page::TITLE);

    // --- Mail sub-page (`admin-mail`: the flat admin-tier mail-*policy* form —
    // mail-enable + spam/inbound-perimeter + auth-enforcement knobs, admin.md
    // § 6 Mail). A dumb renderer of the shared `MailPolicyMachine`
    // (`fauna_client_mail_settings::admin_policy`); it self-wires from the
    // registered client (like the user mail-settings pages). Child name
    // `admin-mail` is the nav-by-id target. ---
    let admin_mail_page = crate::settings::admin_mail::build_admin_mail_page();
    stack.add_titled(
        &admin_mail_page,
        Some("admin-mail"),
        admin::mail_page::TITLE,
    );

    // --- Calendar sub-page (`admin-calendar`: the deployment-wide CalDAV-enable
    // toggle, admin.md § 8 Calendar — the sibling of `admin-mail`'s mail-enable
    // toggle). A dumb renderer of the shared `CaldavPolicyMachine`
    // (`fauna_client_mail_settings::caldav_policy`); self-wires from the
    // registered client like the mail page. Child name `admin-calendar` is the
    // nav-by-id target. ---
    let admin_calendar_page = crate::settings::admin_calendar::build_admin_calendar_page();
    stack.add_titled(
        &admin_calendar_page,
        Some("admin-calendar"),
        admin::calendar_page::TITLE,
    );

    // --- Contacts sub-page (`admin-contacts`: the deployment-wide CardDAV-enable
    // toggle, admin.md § Contacts — the contacts sibling of `admin-calendar`).
    // A dumb renderer of the shared `CarddavPolicyMachine`
    // (`fauna_client_mail_settings::carddav_policy`); self-wires from the
    // registered client like the calendar page. Child name `admin-contacts` is
    // the nav-by-id target. ---
    let admin_contacts_page = crate::settings::admin_contacts::build_admin_contacts_page();
    stack.add_titled(
        &admin_contacts_page,
        Some("admin-contacts"),
        admin::contacts_page::TITLE,
    );

    // --- Files sub-page (`admin-files`: the deployment-wide WebDAV-enable
    // toggle, admin.md § Files — the files sibling of `admin-contacts`).
    // A dumb renderer of the shared `WebdavPolicyMachine`
    // (`fauna_client_mail_settings::webdav_policy`); self-wires from the
    // registered client like the contacts page. Child name `admin-files` is
    // the nav-by-id target. ---
    let admin_files_page = crate::settings::admin_files::build_admin_files_page();
    stack.add_titled(
        &admin_files_page,
        Some("admin-files"),
        admin::files_page::TITLE,
    );

    // --- Web sub-page (`admin-web`: the nest-wide apex-actor designation,
    // web-content-hosting.md § Admin apex hosting). Self-wires from the
    // registered client like the mail pages. Child name `admin-web` is the
    // nav-by-id target. ---
    let admin_web_page = crate::settings::admin_web::build_admin_web_page();
    stack.add_titled(&admin_web_page, Some("admin-web"), admin::web_page::TITLE);

    // --- Bridges-pending sub-page (I3 Phase H: admin approves enrolled mail
    // bridges; child name `admin-bridges-pending` is the nav-by-id target). ---
    let (bridges_pending_page, pending_bridges_list, approved_bridges_list, pending_bridges_error) =
        build_bridges_pending_page();
    stack.add_titled(
        &bridges_pending_page,
        Some("admin-bridges-pending"),
        admin::bridges_pending::TITLE,
    );

    // --- DNS sub-page (unified DNS-management + domain-management page; child
    // name `admin-dns` is the nav-by-id target). The per-domain record matrix +
    // live red/green verdicts (dns-management.md § Manual + live verification),
    // PLUS domain add/remove/restore + the primary badge (decided 2026-05-25:
    // one domains surface; the settings "Email domains" section is removed).
    // Managed-mode controls are client-held: the manage-all master switch + the
    // held DNS-provider credential block drive the shared DnsManagementMachine
    // (`client-holds-provider-keys-not-nest`). ---
    let (
        dns_page,
        dns_records_list,
        dns_records_error,
        dns_credentials_list,
        dns_manage_all_toggle,
        dns_manage_all_guard,
        dns_rename,
        dns_add_domain_warning,
        dns_add_domain_form,
        dns_adding_first_domain,
    ) = build_dns_page(client);
    stack.add_titled(&dns_page, Some("admin-dns"), admin::dns::TITLE);

    // --- Aliases sub-page (`admin-aliases`): the admin-tier alias surface —
    // external forwarders (Kind 7) + (deferred) catch-all designation
    // (admin.md § 4). Child name `admin-aliases` is the nav-by-id target. ---
    let aliases = build_admin_aliases_page(client);
    stack.add_titled(
        &aliases.page,
        Some("admin-aliases"),
        // Nav label = `admin.aliases` ("Aliases"), the uniform cross-app nav
        // name (web +layout, apple AdminPage); the in-page heading keeps the
        // descriptive `aliases_page::TITLE` ("External Forwarders").
        admin::ALIASES,
    );

    // --- Custody-hosting sub-page (`admin-custody-hosting`): the nest-wide
    // held-for-others registry (account-data-plane.md § Two-sided bounds). No linux rail slot to follow — tui is the lead
    // app for this page — so it slots beside the other nest-wide registries,
    // before Logs, exactly where tui put it. Child name `admin-custody-hosting`
    // is the nav-by-id target; deliberately absent from ui.yaml's
    // `navigation.admin_pages` (reached the same way `admin-bridges-pending`
    // is, not through the canonical admin nav set). ---
    let (
        custody_hosting_page,
        custody_hosting_list,
        custody_hosting_header,
        custody_hosting_error,
        custody_hosting_status,
    ) = build_custody_hosting_page();
    stack.add_titled(
        &custody_hosting_page,
        Some("admin-custody-hosting"),
        admin::custody_hosting::TITLE,
    );

    // --- Logs sub-page (`admin-logs`): the nest's `fauna-log` ring fetched over
    // `fauna.admin.logs`, rendered with the same widget as the client's own
    // Settings → Logs page (observability.md § Surfaces). Child name `admin-logs`
    // is the nav-by-id target. ---
    let admin_logs = build_admin_logs_page();
    stack.add_titled(
        &admin_logs.page,
        Some("admin-logs"),
        admin::logs_page::TITLE,
    );

    content.append(&stack);
    stack.set_vexpand(true);

    // Left-align every page's content column (libadwaita centers it by default,
    // which leaves wide empty gutters on a maximized window). Mirrors the
    // settings shell.
    crate::views::layout::left_align_clamped_pages(&content);

    // The admin sidebar rail — `app.rs` swaps this into the split-view sidebar
    // slot while the admin view shows. `admin-nav-back` on top, then a vertical
    // icon+label nav list driving the same sub-stack: the vertical replacement
    // for the old horizontal StackSwitcher, so the admin nav no longer adds
    // horizontal width. Built via the shared `build_nav_rail` helper — the same
    // rail shape the settings shell uses (admin.md / settings.md § Navigation
    // model). (sub-stack child name, label, symbolic icon) per admin page, in
    // rail order; names + labels mirror the `add_titled` calls above.
    // (child name, label, icon, indent). The admin rail is flat — no nesting —
    // so every entry passes `false`.
    let admin_nav: [(&'static str, &'static str, &'static str, bool); 14] = [
        (
            "dashboard",
            admin::dashboard::TITLE,
            "view-grid-symbolic",
            false,
        ),
        (
            "users",
            admin::users_page::TITLE,
            "system-users-symbolic",
            false,
        ),
        (
            "settings",
            admin::settings_page::TITLE,
            "emblem-system-symbolic",
            false,
        ),
        (
            "admin-nest",
            admin::nest_page::TITLE,
            "applications-system-symbolic",
            false,
        ),
        (
            "admin-mail",
            admin::mail_page::TITLE,
            "mail-unread-symbolic",
            false,
        ),
        (
            "admin-calendar",
            admin::calendar_page::TITLE,
            "x-office-calendar-symbolic",
            false,
        ),
        (
            "admin-contacts",
            admin::contacts_page::TITLE,
            "x-office-address-book-symbolic",
            false,
        ),
        (
            "admin-files",
            admin::files_page::TITLE,
            "folder-symbolic",
            false,
        ),
        (
            "admin-web",
            admin::web_page::TITLE,
            "network-server-symbolic",
            false,
        ),
        (
            "admin-bridges-pending",
            admin::bridges_pending::TITLE,
            "network-workgroup-symbolic",
            false,
        ),
        (
            "admin-dns",
            admin::dns::TITLE,
            "network-server-symbolic",
            false,
        ),
        (
            "admin-aliases",
            admin::ALIASES,
            "mail-forward-symbolic",
            false,
        ),
        (
            "admin-custody-hosting",
            admin::custody_hosting::TITLE,
            "network-server-symbolic",
            false,
        ),
        (
            "admin-logs",
            admin::logs_page::TITLE,
            "utilities-system-monitor-symbolic",
            false,
        ),
    ];
    let admin_sidebar = crate::views::nav_rail::build_nav_rail(&nav_back, &admin_nav, &stack);

    let handles = AdminHandles {
        nav_back,
        sub_stack: stack.clone(),
        stats_users_row: dashboard_handles.stats_users_row,
        stats_storage_row: dashboard_handles.stats_storage_row,
        stats_inbox_row: dashboard_handles.stats_inbox_row,
        stats_sessions_row: dashboard_handles.stats_sessions_row,
        stats_users_value: dashboard_handles.stats_users_value,
        stats_storage_value: dashboard_handles.stats_storage_value,
        stats_inbox_value: dashboard_handles.stats_inbox_value,
        stats_sessions_value: dashboard_handles.stats_sessions_value,
        dashboard_users_group: dashboard_handles.users_group,
        server_version_row: dashboard_handles.server_version_row,
        server_version_value: dashboard_handles.server_version_value,
        server_uptime_row: dashboard_handles.server_uptime_row,
        server_workers_row: dashboard_handles.server_workers_row,
        users_list_box: hub.users_list_box,
        users_page_count: hub.users_page_count,
        users_offset: hub.users_offset,
        users_total: hub.users_total,
        users_prev_page: hub.users_prev_page,
        users_next_page: hub.users_next_page,
        users_pagination: hub.users_pagination,
        invite_codes_list: hub.invite_codes_list,
        create_invite_btn: hub.create_invite_btn,
        invite_guardian_select: hub.invite_guardian_select,
        invite_copy_btn: hub.invite_copy_btn,
        minted_code: hub.minted_code,
        invite_requests_list: hub.invite_requests_list,
        last_invite_requests: Rc::new(RefCell::new(None)),
        tier_names: hub.tier_names,
        users_action_error: hub.users_action_error,
        registration_readonly_label: hub.registration_readonly_label,
        registration_editable_box: hub.registration_editable_box,
        registration_mode_select: hub.registration_mode_select,
        registration_max_free_users_input: hub.registration_max_free_users_input,
        registration_age_verification_toggle: hub.registration_age_verification_toggle,
        registration_age_verification_persisted: hub.registration_age_verification_persisted,
        settings_tiers_list,
        membership_tiers_list,
        own_membership_tier_names: Rc::new(RefCell::new(Vec::new())),
        last_membership_tiers: Rc::new(RefCell::new(None)),
        svc_pairing_toggle: nest.pairing_toggle,
        svc_pairing_status: nest.pairing_status,
        svc_pairing_guard: nest.pairing_guard,
        serving_port_entry: nest.serving_port_entry,
        serving_port_save: nest.serving_port_save,
        serving_port_hint: nest.serving_port_hint,
        region_status: nest.region_status,
        region_authority: nest.region_authority,
        region_staleness: nest.region_staleness,
        region_entry: nest.region_entry,
        region_withdraw: nest.region_withdraw,
        os_maintenance_status: nest.os_maintenance_status,
        os_updates_count: nest.os_updates_count,
        os_restart_now_button: nest.os_restart_now_button,
        seed_rotate_confirm_box: nest.seed_rotate_confirm_box,
        seed_rotate_roster_box: nest.seed_rotate_roster_box,
        seed_rotate_reason: nest.seed_rotate_reason,
        seed_rotate_confirm_button: nest.seed_rotate_confirm_button,
        seed_rotate_status: nest.seed_rotate_status,
        seed_rotate_confirm: nest.seed_rotate_confirm,
        oauth: nest.oauth,
        takedown_status: nest.takedown_status,
        custody_hosting_list,
        custody_hosting_header,
        custody_hosting_error,
        custody_hosting_status,
        custody_hosting_confirm: Rc::new(RefCell::new(None)),
        custody_hosting_last: Rc::new(RefCell::new(None)),
        pending_bridges_list,
        approved_bridges_list,
        pending_bridges_error,
        dns_records_list,
        dns_records_error,
        dns_credentials_list,
        dns_manage_all_toggle,
        dns_manage_all_guard,
        dns_rename,
        dns_add_domain_warning,
        dns_add_domain_form,
        dns_adding_first_domain,
        last_local_domains: Rc::new(RefCell::new(None)),
        last_dns: Rc::new(RefCell::new(None)),
        picker_users: Rc::new(RefCell::new(None)),
        forwarders_list: aliases.forwarders_list,
        forwarder_add_domain: aliases.forwarder_add_domain,
        forwarders_action_error: aliases.forwarders_action_error,
        admin_logs_ctx: admin_logs.ctx,
        admin_logs_error: admin_logs.error_label,
    };

    (content, admin_sidebar, handles)
}

// ---------------------------------------------------------------------------
// Users hub (`admin-users`) — five sections: Pending requests / Registration /
// Admit / Invite / Users (admin.md § Users). Built once in `build_users_hub`; the section bodies
// (lists, mint form) are updated from typed `Admin*` replies via the
// `update_*` functions further down.
// ---------------------------------------------------------------------------

/// Nest-seeded default tiers (`db/migrations.rs` SEED_TIERS). The tier pickers
/// cycle through these until the `fauna.admin.tiers.list` reply arrives
/// (`AdminTiersLoaded` then overwrites `tier_names`); the tier is the
/// quota (admin.md § Users — there is no separate per-user quota control).
const DEFAULT_TIERS: &[&str] = &["free", "personal", "community"];

/// Users-section page size for `fauna.admin.users.list` (admin.md § Users —
/// pagination math is `limit`/`offset`). Matches the nest default (50,
/// `admin_ws_handlers::list_handler`), so the no-limit initial fetch and an
/// explicit-limit paged fetch return the same first page.
pub const USERS_PAGE_SIZE: i64 = 50;

/// Handles for the consolidated `admin-users` hub, threaded into `AdminHandles`.
struct UsersHubHandles {
    page: gtk::Box,
    users_list_box: gtk::ListBox,
    users_page_count: gtk::Label,
    users_offset: Rc<std::cell::Cell<i64>>,
    users_total: Rc<std::cell::Cell<i64>>,
    users_prev_page: gtk::Button,
    users_next_page: gtk::Button,
    users_pagination: gtk::Label,
    invite_codes_list: gtk::ListBox,
    create_invite_btn: gtk::Button,
    invite_guardian_select: GuardianSelect,
    invite_copy_btn: gtk::Button,
    minted_code: Rc<RefCell<String>>,
    invite_requests_list: gtk::ListBox,
    tier_names: Rc<RefCell<Vec<String>>>,
    users_action_error: gtk::Label,
    registration_readonly_label: gtk::Label,
    registration_editable_box: gtk::Box,
    registration_mode_select: gtk::DropDown,
    registration_max_free_users_input: gtk::Entry,
    registration_age_verification_toggle: gtk::Switch,
    registration_age_verification_persisted: Rc<std::cell::Cell<bool>>,
}

/// A **guardian picker** — the two admission surfaces of the family-safety
/// client surface (`family-safety.md` § App surface → Admission surfaces):
/// `admin-users-invite-guardian-select` (mint form) and
/// `invite-request-row-guardian-select` (per pending-request row). Both default
/// to "None" (an ordinary, unsupervised admission).
///
/// A `gtk::DropDown` over a `gtk::StringList` whose **display labels** are what
/// `driver.select` matches (the e2e passes a user's label/handle), plus an
/// index-aligned actor-id table read at submit time. Options: "None" at index 0,
/// then one entry per **non-suspended** user — the filter is UX only; the nest
/// re-validates the guardian (exists / not suspended / not itself supervised /
/// ≠ the admitted actor) inside the admission transaction.
#[derive(Clone)]
pub struct GuardianSelect {
    dd: gtk::DropDown,
    /// Index-aligned with the DropDown model: `None` at 0 ("None"), then one
    /// actor id per listed user.
    actors: Rc<RefCell<Vec<Option<Vec<u8>>>>>,
    /// Set while [`Self::populate`] swaps the model — the swap passes through
    /// "None" before the selection is restored, which must not read as the
    /// admin clearing the guardian ([`Self::connect_changed`]).
    populating: Rc<std::cell::Cell<bool>>,
}

impl GuardianSelect {
    /// Build the picker with only its "None" option — [`Self::populate`] fills in
    /// the users when `fauna.admin.users.list` lands (the two callers' build
    /// order vs. the users fetch is not guaranteed).
    fn new(testid: &str) -> Self {
        let dd = gtk::DropDown::from_strings(&[admin::users_page::GUARDIAN_NONE]);
        dd.add_css_class("flat");
        dd.set_selected(0);
        crate::testid::set_test_id(&dd, testid);
        Self {
            dd,
            actors: Rc::new(RefCell::new(vec![None])),
            populating: Rc::new(std::cell::Cell::new(false)),
        }
    }

    /// Rebuild the option list from every account on the nest
    /// ([`AdminHandles::picker_users`] — never the Users page on screen),
    /// preserving the current selection by actor id when it is still listed.
    fn populate(&self, users: Option<&[fauna_client_admin::AdminUser]>) {
        let previous = self.selected();

        let mut labels: Vec<String> = vec![admin::users_page::GUARDIAN_NONE.to_string()];
        let mut actors: Vec<Option<Vec<u8>>> = vec![None];
        for u in users.unwrap_or_default() {
            if u.suspended {
                continue;
            }
            labels.push(fauna_client_admin::admin_picker_option(u));
            actors.push(Some(u.actor_id.to_vec()));
        }

        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.populating.set(true);
        self.dd.set_model(Some(&gtk::StringList::new(&refs)));
        let restored = previous
            .and_then(|id| actors.iter().position(|a| a.as_deref() == Some(&id[..])))
            .unwrap_or(0);
        *self.actors.borrow_mut() = actors;
        self.dd.set_selected(restored as u32);
        self.populating.set(false);
        // One settled notification for the restored selection — a guardian
        // that is no longer listed reads as cleared, exactly once.
        self.dd.notify("selected");
    }

    /// Call `on_change(guardian_selected)` whenever the selection settles on a
    /// new option (never mid-[`Self::populate`]). Captures the shared actor
    /// table, not `self`, so the DropDown holds no reference cycle to itself.
    fn connect_changed(&self, on_change: impl Fn(bool) + 'static) {
        let actors = Rc::clone(&self.actors);
        let populating = Rc::clone(&self.populating);
        self.dd.connect_selected_notify(move |d| {
            if populating.get() {
                return;
            }
            let selected = actors
                .borrow()
                .get(d.selected() as usize)
                .is_some_and(Option::is_some);
            on_change(selected);
        });
    }

    /// The selected guardian's actor id — `None` for the "None" option (an
    /// ordinary admission), which is what the shared `AdminClient`'s
    /// `guardian_actor: Option<Vec<u8>>` arg then carries.
    fn selected(&self) -> Option<Vec<u8>> {
        self.actors
            .borrow()
            .get(self.dd.selected() as usize)
            .cloned()
            .flatten()
    }

    fn widget(&self) -> &gtk::DropDown {
        &self.dd
    }
}

/// An **age-band picker** — the two age-band admission surfaces
/// (`family-safety.md` § App surface → *Age-band surfaces*):
/// `admin-users-invite-age-band-select` (mint form) and the indexed
/// `invite-request-row-age-band-select` (per pending-request row).
///
/// The model holds the shared catalog's option VALUES
/// (`fauna_client_admin::age_band_options` — `not-set` + the four wire
/// tokens, in `AgeBand::ORDER`), so `select()`/`text_of()` speak the value
/// contract `actions/admin.py` drives; a display `ClosureExpression` paints
/// the localized label (the `build_registration_mode_dropdown` split, in its
/// lighter shape). No band vocabulary is spelled here. **Enabled only while a
/// guardian is selected, reset to `not-set` when the guardian is cleared**
/// ([`Self::follow_guardian`]) — the nest refuses a band without a guardian,
/// and that refusal stays the authority; this gate is UX.
#[derive(Clone)]
pub struct AgeBandSelect {
    dd: gtk::DropDown,
}

impl AgeBandSelect {
    /// Build the picker preselected at `initial` (an option value — `not-set`,
    /// or the applicant's claim via `claimed_age_band_option`).
    fn new(testid: &str, initial: &str) -> Self {
        let values = age_band_option_values();
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let dd = gtk::DropDown::builder()
            .model(&gtk::StringList::new(&refs))
            .build();
        dd.add_css_class("flat");
        let label_expr = gtk::ClosureExpression::new::<String>(
            &[] as &[gtk::Expression],
            gtk::glib::closure!(|item: gtk::StringObject| {
                age_band_option_label(item.string().as_str())
            }),
        );
        dd.set_expression(Some(&label_expr));
        crate::testid::set_test_id(&dd, testid);
        let select = Self { dd };
        select.set_value(initial);
        select
    }

    fn set_value(&self, value: &str) {
        if let Some(i) = age_band_option_values().iter().position(|v| v == value) {
            self.dd.set_selected(i as u32);
        }
    }

    /// The typed band the picker names — `None` for `not-set` (and anything
    /// the shared vocabulary cannot name).
    fn selected(&self) -> Option<fauna_protocol::age::AgeBand> {
        dropdown_wire_value(&self.dd)
            .as_deref()
            .and_then(fauna_client_admin::age_band_from_option_value)
    }

    /// Gate this picker on `guardian`: sensitive only while a guardian is
    /// selected (and `enabled` — a decided request row stays inert), reset to
    /// `not-set` whenever the guardian is cleared, so a stale band can never
    /// ride a now-unsupervised admission.
    fn follow_guardian(&self, guardian: &GuardianSelect, enabled: bool) {
        self.dd
            .set_sensitive(enabled && guardian.selected().is_some());
        let band = self.clone();
        guardian.connect_changed(move |selected| {
            if !selected {
                band.set_value(fauna_client_admin::AGE_BAND_NOT_SET_VALUE);
            }
            band.dd.set_sensitive(enabled && selected);
        });
    }

    fn widget(&self) -> &gtk::DropDown {
        &self.dd
    }
}

/// The age-band pickers' option values, in the shared catalog's order.
fn age_band_option_values() -> Vec<String> {
    fauna_client_admin::age_band_options()
        .into_iter()
        .map(|o| o.value)
        .collect()
}

/// The localized label the shared catalog pairs with an option value.
fn age_band_option_label(value: &str) -> String {
    fauna_client_admin::age_band_options()
        .into_iter()
        .find(|o| o.value == value)
        .map(|o| o.label.resolve(crate::i18n::strings::lookup))
        .unwrap_or_else(|| value.to_string())
}

/// The tier a `gtk::DropDown` tier picker currently shows, i.e. its selected
/// `StringObject`'s string. `None` when nothing is selected (empty model).
fn dropdown_tier(dd: &gtk::DropDown) -> Option<String> {
    dd.selected_item()
        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
        .map(|s| s.string().to_string())
}

/// Build a tier picker as a native `gtk::DropDown` over `tier_names`, with
/// `initial` preselected. Changing the selection invokes `on_select(new_tier)`.
/// The in-process automation agent actuates a `DropDown` by display label
/// (`driver.select`), so a real dropdown is now drivable headless — replacing
/// the old cycle-button work-around. `on_select` is wired *after* the initial
/// selection so building a row at its current tier doesn't spuriously fire it
/// (which, for the live Users picker, would re-apply the same tier each render).
fn build_tier_select_dropdown(
    testid: &str,
    initial: &str,
    tier_names: &Rc<RefCell<Vec<String>>>,
    on_select: impl Fn(&str) + 'static,
) -> gtk::DropDown {
    let names = tier_names.borrow();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let dd = gtk::DropDown::from_strings(&refs);
    dd.add_css_class("flat");
    if let Some(i) = names.iter().position(|t| t == initial) {
        dd.set_selected(i as u32);
    }
    drop(names);
    crate::testid::set_test_id(&dd, testid);
    let tier_names = Rc::clone(tier_names);
    dd.connect_selected_notify(move |d| {
        if let Some(tier) = tier_names.borrow().get(d.selected() as usize) {
            on_select(tier);
        }
    });
    dd
}

/// The registration-mode select's option catalog (wire value + localized
/// label, in display order) — the shared `fauna_client_admin::registration_mode_options`
/// (`public-mode.md` § Registration Modes; `admin.md` § Where logic lives).
/// Resolved once at dropdown-build time. This list's wire values literally
/// drive the `DropDown`'s model (`build_registration_mode_dropdown`), so
/// `select()`/`text_of()` (`automation/{agent,find}.rs`) — which read/write the
/// model's `StringObject` text, never the custom factory's rendered label — speak
/// the wire-value contract the e2e driver requires
/// (`tests/e2e-unified/actions/admin.py::registration_mode`) with zero
/// automation-layer changes.
fn registration_mode_options() -> Vec<(String, String)> {
    fauna_client_admin::registration_mode_options()
        .into_iter()
        .map(|o| (o.value, o.label.resolve(crate::i18n::strings::lookup)))
        .collect()
}

/// The wire value the registration-mode `DropDown`'s model currently shows — its
/// selected `StringObject`'s string (`"open"` / `"invite_required"` /
/// `"closed"`), NEVER the localized label the custom factory renders. `None`
/// when nothing is selected (empty model). The registration-section twin of
/// `dropdown_tier`, kept separate because the two pickers' display-vs-value
/// relationship differs (tier: display == value; registration mode: they
/// deliberately diverge — see `build_registration_mode_dropdown`).
fn dropdown_wire_value(dd: &gtk::DropDown) -> Option<String> {
    dd.selected_item()
        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
        .map(|s| s.string().to_string())
}

/// Build the registration-mode `DropDown`: the model holds the WIRE VALUES
/// (`registration_mode_options`) — so `select()`/`text_of()` naturally speak
/// the wire-value contract the e2e driver requires, as `dropdown_wire_value`'s
/// doc explains — while a custom `gtk::SignalListItemFactory` renders the
/// localized label (resolved once, into the closure) for the human-visible
/// dropdown (both the closed-state button and the popup rows: `set_factory`
/// covers both unless a separate `list_factory` is set, which this doesn't do).
/// This is the first select on this page where the display string differs from
/// the dispatch value — every other `DropDown` here (`build_tier_select_dropdown`,
/// `GuardianSelect`) has display == value. It was also the first on linux when
/// written; the split is now the app-wide idiom for any picker whose wire value
/// and label diverge (`views/devices_folders/folders.rs`, `views/media/mod.rs`,
/// `views/feed/feed_list.rs::build_rule_type_dropdown`, …), most of them via a
/// display `ClosureExpression` rather than a full factory — reach for that
/// lighter shape unless, as here, the rows need real widgets.
///
/// `connect_setup`/`connect_bind` take `&glib::Object` (not `&gtk::ListItem`)
/// under the `v4_8` GTK feature this crate's `v4_12` pin implies — downcast to
/// `gtk::ListItem` inside each callback, matching the vendored gtk4-rs 0.9.7
/// signature (verified against `Cargo.lock`'s pinned version before writing this).
fn build_registration_mode_dropdown() -> gtk::DropDown {
    let options = registration_mode_options();
    let wire_values: Vec<&str> = options.iter().map(|(v, _)| v.as_str()).collect();
    let model = gtk::StringList::new(&wire_values);
    let dd = gtk::DropDown::new(Some(model), gtk::Expression::NONE);
    dd.add_css_class("flat");

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_halign(gtk::Align::Start);
        list_item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let wire = list_item
            .item()
            .and_then(|o| o.downcast::<gtk::StringObject>().ok())
            .map(|s| s.string().to_string())
            .unwrap_or_default();
        if let Some(label) = list_item
            .child()
            .and_then(|w| w.downcast::<gtk::Label>().ok())
        {
            let text = options
                .iter()
                .find(|(v, _)| *v == wire)
                .map(|(_, l)| l.as_str())
                .unwrap_or(&wire);
            label.set_text(text);
        }
    });
    dd.set_factory(Some(&factory));

    crate::testid::set_test_id(&dd, ids::ADMIN_USERS_REGISTRATION_MODE_SELECT);
    dd
}

/// A section container with a heading label that carries `section_testid` (one
/// of the `admin-users-*-section` view IDs). The id rides the heading **label**,
/// not the `gtk::Box`: a bare container's accessible name/description is not
/// exposed to AT-SPI (only a `gtk::Label`/`gtk::Button`/`gtk::ListBoxRow` is),
/// so the e2e driver can only `wait_for`/`is_visible` the section when its anchor
/// is a label — the same anchor-on-a-label pattern as `quota-section`
/// (`views/status.rs`). The visible heading IS the section's anchor.
fn build_section(section_testid: &str, heading_text: &str) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    section.set_margin_top(8);

    let heading = gtk::Label::new(Some(heading_text));
    heading.add_css_class("title-4");
    heading.set_halign(gtk::Align::Start);
    heading.set_margin_start(12);
    heading.set_margin_top(8);
    heading.set_margin_bottom(4);
    crate::testid::set_test_id(&heading, section_testid);
    section.append(&heading);

    section
}

fn build_users_hub(client: &Rc<FaunaClient>) -> UsersHubHandles {
    let tier_names = Rc::new(RefCell::new(
        DEFAULT_TIERS
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>(),
    ));
    // Pagination state (admin.md § Users — `limit`/`offset` on `users.list`).
    let users_offset = Rc::new(std::cell::Cell::new(0i64));
    let users_total = Rc::new(std::cell::Cell::new(0i64));

    // The hub's dedicated error surface is `admin-users-action-error` (admin.md
    // § Users / § Errors), NOT the generic `error-message` — ui.yaml scopes only
    // `admin-users-action-error` on this page. Hidden until a Users-section action
    // fails (the `app.rs` `ActionResult::Failed` routing populates it, plus the
    // Registration section's own client-side cap-validation guard below). Built
    // early (not where it's appended to `outer`) so the Section 2 save handler can
    // capture it.
    let users_action_error = gtk::Label::new(None);
    users_action_error.set_visible(false);
    users_action_error.set_wrap(true);
    users_action_error.set_halign(gtk::Align::Start);
    users_action_error.set_margin_start(12);
    users_action_error.set_margin_end(12);
    users_action_error.set_margin_bottom(8);
    users_action_error.add_css_class("error");
    crate::testid::set_test_id(&users_action_error, ids::ADMIN_USERS_ACTION_ERROR);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Visible page heading.
    let heading = gtk::Label::new(Some(admin::users_page::TITLE));
    heading.add_css_class("title-2");
    heading.set_margin_top(16);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&heading, ids::ADMIN_USERS_HEADING);
    outer.append(&heading);

    let scroll_body = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // ── Section 1: Pending requests ──────────────────────────────────────────
    let requests_section = build_section(
        "admin-users-requests-section",
        admin::users_page::SECTION_REQUESTS,
    );
    let invite_requests_list = gtk::ListBox::new();
    invite_requests_list.set_selection_mode(gtk::SelectionMode::None);
    invite_requests_list.add_css_class("boxed-list");
    invite_requests_list.set_margin_start(8);
    invite_requests_list.set_margin_end(8);
    crate::testid::set_test_id(&invite_requests_list, ids::ADMIN_INVITE_REQUESTS_LIST);
    let requests_placeholder = adw::StatusPage::builder()
        .title(admin::invite_requests_page::EMPTY)
        .icon_name("mail-unread-symbolic")
        .build();
    invite_requests_list.set_placeholder(Some(&requests_placeholder));
    requests_section.append(&invite_requests_list);
    scroll_body.append(&requests_section);

    // ── Section 2: Registration ──────────────────────────────────────────────
    // The nest's registration posture (open / invite_required / closed) plus the
    // orthogonal free-tier ceiling, saved together by ONE
    // `fauna.admin.set_registration_mode` call (admin.md § 2 Users → Section 2 —
    // Registration). A `None`/unparseable mode from `fauna.setup.status` is NEVER
    // coerced to a guessed variant — the whole editable block disappears and a
    // read-only i18n message takes its place instead (public-mode.md §
    // Registration Modes + Implementation status today), matching web's
    // `registrationMode === null` branch. Both widgets below start hidden; the
    // real state is set by `set_registration_mode` (views/admin.rs) once
    // `RegistrationModeLoaded` lands (app.rs wires the fetch on admin-status).
    let registration_section = build_section(
        "admin-users-registration-section",
        admin::users_page::SECTION_REGISTRATION,
    );

    let registration_readonly_label = gtk::Label::new(None);
    registration_readonly_label.set_wrap(true);
    registration_readonly_label.set_halign(gtk::Align::Start);
    registration_readonly_label.set_margin_start(12);
    registration_readonly_label.set_margin_bottom(8);
    registration_readonly_label.add_css_class("dim-label");
    registration_readonly_label.set_visible(false);
    registration_section.append(&registration_readonly_label);

    let registration_editable_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    registration_editable_box.set_margin_start(12);
    registration_editable_box.set_margin_end(12);
    registration_editable_box.set_margin_bottom(8);
    registration_editable_box.set_visible(false);

    let registration_mode_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let registration_mode_label = gtk::Label::new(Some(admin::users_page::REGISTRATION_MODE_LABEL));
    registration_mode_label.set_halign(gtk::Align::Start);
    registration_mode_row.append(&registration_mode_label);
    let registration_mode_select = build_registration_mode_dropdown();
    registration_mode_row.append(&registration_mode_select);
    registration_editable_box.append(&registration_mode_row);

    let registration_cap_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let registration_cap_label = gtk::Label::new(Some(admin::users_page::MAX_FREE_USERS_LABEL));
    registration_cap_label.set_halign(gtk::Align::Start);
    registration_cap_row.append(&registration_cap_label);
    let registration_max_free_users_input = gtk::Entry::new();
    registration_max_free_users_input.set_width_chars(8);
    crate::testid::set_test_id(
        &registration_max_free_users_input,
        ids::ADMIN_USERS_MAX_FREE_USERS_INPUT,
    );
    registration_cap_row.append(&registration_max_free_users_input);
    registration_editable_box.append(&registration_cap_row);

    let registration_cap_hint = gtk::Label::new(Some(admin::users_page::MAX_FREE_USERS_HINT));
    registration_cap_hint.add_css_class("dim-label");
    registration_cap_hint.set_halign(gtk::Align::Start);
    registration_editable_box.append(&registration_cap_hint);

    // The age require-knob (`family-safety.md` § App surface → *Age-band
    // surfaces*; `admin.md` § 2 Users → Registration): a DRAFT seeded from
    // `fauna.setup.status` (`set_registration_mode`), committed only by the
    // section's save below. The driver reads it as `on`/`off` via the `state`
    // marker, re-marked in the same turn the switch flips.
    let registration_age_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let registration_age_label =
        gtk::Label::new(Some(admin::users_page::AGE_VERIFICATION_REQUIRED_LABEL));
    registration_age_label.set_halign(gtk::Align::Start);
    registration_age_label.set_hexpand(true);
    registration_age_label.set_wrap(true);
    registration_age_row.append(&registration_age_label);
    let registration_age_verification_toggle =
        gtk::Switch::builder().valign(gtk::Align::Center).build();
    crate::testid::set_test_id(
        &registration_age_verification_toggle,
        ids::ADMIN_USERS_REGISTRATION_AGE_VERIFICATION_TOGGLE,
    );
    mark_switch_state(&registration_age_verification_toggle);
    registration_age_verification_toggle.connect_active_notify(mark_switch_state);
    registration_age_row.append(&registration_age_verification_toggle);
    registration_editable_box.append(&registration_age_row);
    let registration_age_verification_persisted = Rc::new(std::cell::Cell::new(false));

    let registration_save = gtk::Button::with_label(admin::users_page::REGISTRATION_SAVE);
    registration_save.add_css_class("suggested-action");
    registration_save.set_halign(gtk::Align::End);
    crate::testid::set_test_id(
        &registration_save,
        ids::ADMIN_USERS_REGISTRATION_SAVE_BUTTON,
    );
    // tui's `admin::Action::SaveRegistration`.
    crate::offline_gate::declare_wire_kind(&registration_save, "fauna.admin.set_registration_mode");
    registration_editable_box.append(&registration_save);

    registration_section.append(&registration_editable_box);
    scroll_body.append(&registration_section);

    // Save → validate the cap client-side (blank or digits-only, mirroring web's
    // `capIsValid`/`saveRegistration`), then dispatch mode + cap TOGETHER — the
    // nest re-read after success (`RegistrationModeSaved` →
    // `fetch_registration_mode`) is the only confirmation (admin.md § 2 specifies
    // no separate confirmation element).
    {
        let client_ref = Rc::clone(client);
        let mode_dd = registration_mode_select.clone();
        let cap_entry = registration_max_free_users_input.clone();
        let age_toggle = registration_age_verification_toggle.clone();
        let age_persisted = Rc::clone(&registration_age_verification_persisted);
        let error_label = users_action_error.clone();
        registration_save.connect_clicked(move |_| {
            // The DropDown's model only ever holds the three wire values (built by
            // `build_registration_mode_dropdown`), and the save button is only
            // reachable while the section is in its editable state — so this
            // always resolves; the `None` arm is unreachable defensiveness, not a
            // real path (never send a guessed mode — public-mode.md's rule).
            let Some(wire_mode) = dropdown_wire_value(&mode_dd) else {
                return;
            };
            let raw_cap = cap_entry.text();
            let trimmed = raw_cap.trim();
            // `u64::from_str` alone enforces "blank or digits-only": it rejects a
            // sign, a decimal point, or any non-digit character, matching web's
            // `/^\d+$/` regex without a separate character-class check.
            let cap: Option<u64> = if trimmed.is_empty() {
                None
            } else {
                match trimmed.parse::<u64>() {
                    Ok(n) => Some(n),
                    Err(_) => {
                        render_page_error(
                            &error_label,
                            Some(admin::users_page::MAX_FREE_USERS_HINT),
                        );
                        return;
                    }
                }
            };
            render_page_error(&error_label, None);
            // The knob rides this save only when it changed — an unchanged
            // knob sends nothing (one gesture, no half-saved section).
            let age_draft = age_toggle.is_active();
            let age_verification = (age_draft != age_persisted.get()).then_some(age_draft);
            client_ref.set_registration_mode(wire_mode, cap, age_verification);
        });
    }

    // ── Section 3: Admit (direct admission — `fauna.admin.users.create`, the
    // third account-creation path, `public-mode.md` § Registration & Identity;
    // user-approved 2026-08-15; tui's `admin/users.rs::admit_section` is the
    // reference). The admin types a known actor id, names the handle the actor
    // is admitted under (a blank handle admits the deliberate handle-less
    // state — there is no set-later, only `fauna.admin.users.clear_handle`),
    // picks a tier ("admission is always choosing a tier"), and admits — one
    // `AdminClient::users_create` call. The form stays populated on both
    // success and failure (tui's own idiom: nothing here clears the drafts);
    // success feedback is the new row appearing in the Users-section refetch.
    let admit_section = build_section(
        "admin-users-admit-section",
        admin::users_page::SECTION_ADMIT,
    );

    let admit_actor_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let admit_actor_label = gtk::Label::new(Some(admin::users_page::ADMIT_ACTOR_LABEL));
    admit_actor_label.set_halign(gtk::Align::Start);
    admit_actor_row.append(&admit_actor_label);
    let admit_actor_input = gtk::Entry::new();
    admit_actor_input.set_hexpand(true);
    crate::testid::set_test_id(&admit_actor_input, ids::ADMIN_USERS_ADMIT_ACTOR_INPUT);
    admit_actor_row.append(&admit_actor_input);
    admit_section.append(&admit_actor_row);

    let admit_handle_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let admit_handle_label = gtk::Label::new(Some(admin::users_page::ADMIT_HANDLE_LABEL));
    admit_handle_label.set_halign(gtk::Align::Start);
    admit_handle_row.append(&admit_handle_label);
    let admit_handle_input = gtk::Entry::new();
    admit_handle_input.set_hexpand(true);
    crate::testid::set_test_id(&admit_handle_input, ids::ADMIN_USERS_ADMIT_HANDLE_INPUT);
    admit_handle_row.append(&admit_handle_input);
    admit_section.append(&admit_handle_row);

    let admit_tier_select = build_tier_select_dropdown(
        ids::ADMIN_USERS_ADMIT_TIER_SELECT,
        DEFAULT_TIERS[0],
        &tier_names,
        |_| {},
    );
    admit_section.append(&admit_tier_select);

    let admit_button = gtk::Button::with_label(admin::users_page::ADMIT_BUTTON);
    admit_button.add_css_class("suggested-action");
    admit_button.set_halign(gtk::Align::End);
    crate::testid::set_test_id(&admit_button, ids::ADMIN_USERS_ADMIT_BUTTON);
    // tui's `admin::Action::AdmitUser`.
    crate::offline_gate::declare_wire_kind(&admit_button, "fauna.admin.users.create");
    admit_section.append(&admit_button);

    scroll_body.append(&admit_section);

    // Admit → client-side 64-hex actor validation (mirrors tui's
    // `admin_mutation`: a malformed id is a user error, never dispatched — the
    // nest would refuse it anyway, and failing local keeps the message
    // actionable), a blank handle ⇒ `None` (the handle-less admission), then
    // one `fauna.admin.users.create` call. Success re-render is the Users
    // section's own `AdminUserUpdated` refetch (shared with tier-change/evict).
    {
        let client_ref = Rc::clone(client);
        let actor_entry = admit_actor_input.clone();
        let handle_entry = admit_handle_input.clone();
        let tier_dd = admit_tier_select.clone();
        let error_label = users_action_error.clone();
        admit_button.connect_clicked(move |_| {
            let raw = actor_entry.text();
            let trimmed = raw.trim();
            let actor: Option<Vec<u8>> = (trimmed.len() == 64)
                .then(|| {
                    (0..trimmed.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(trimmed.get(i..i + 2)?, 16).ok())
                        .collect::<Option<Vec<u8>>>()
                })
                .flatten();
            let Some(actor) = actor else {
                render_page_error(&error_label, Some(admin::users_page::ADMIT_ACTOR_HINT));
                return;
            };
            let handle = {
                let h = handle_entry.text();
                let h = h.trim();
                (!h.is_empty()).then(|| h.to_string())
            };
            let tier = dropdown_tier(&tier_dd).unwrap_or_else(|| DEFAULT_TIERS[0].to_string());
            render_page_error(&error_label, None);
            client_ref.admit_user(actor, tier, handle);
        });
    }

    // ── Section 4: Invite (mint a code) ──────────────────────────────────────
    let invite_section = build_section(
        "admin-users-invite-section",
        admin::users_page::SECTION_INVITE,
    );

    let invite_codes_list = gtk::ListBox::new();
    invite_codes_list.set_selection_mode(gtk::SelectionMode::None);
    invite_codes_list.add_css_class("boxed-list");
    invite_codes_list.set_margin_start(8);
    invite_codes_list.set_margin_end(8);
    let codes_placeholder = adw::StatusPage::builder()
        .title(admin::settings_page::NO_CODES)
        .icon_name("emblem-documents-symbolic")
        .build();
    invite_codes_list.set_placeholder(Some(&codes_placeholder));
    invite_section.append(&invite_codes_list);

    // Create form (tier picker + max-uses); revealed on Create, gated by Confirm.
    let create_form = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    crate::testid::set_test_id(&create_form, ids::ADMIN_SETTINGS_INVITE_CREATE_FORM);
    create_form.set_margin_start(8);
    create_form.set_margin_end(8);
    create_form.set_margin_top(8);
    create_form.set_visible(false);

    let invite_tier_select = build_tier_select_dropdown(
        "admin-settings-tier-select",
        DEFAULT_TIERS[0],
        &tier_names,
        |_| {},
    );
    create_form.append(&invite_tier_select);

    let invite_max_uses = gtk::Entry::builder()
        .placeholder_text(admin::settings_page::MAX_USES)
        .text("1")
        .build();
    invite_max_uses.set_width_chars(6);
    crate::testid::set_test_id(&invite_max_uses, ids::ADMIN_SETTINGS_MAX_USES_INPUT);
    create_form.append(&invite_max_uses);

    // Guardian for a supervised admission (`family-safety.md` § App surface →
    // Admission surfaces). Defaults to "None"; populated from the users list when
    // `AdminUsersLoaded` lands (`refresh_guardian_pickers`).
    let guardian_caption = gtk::Label::new(Some(admin::users_page::GUARDIAN_LABEL));
    guardian_caption.add_css_class("dim-label");
    guardian_caption.set_valign(gtk::Align::Center);
    create_form.append(&guardian_caption);

    let invite_guardian_select = GuardianSelect::new("admin-users-invite-guardian-select");
    create_form.append(invite_guardian_select.widget());

    // The band the redeemed account is admitted under — gated on the guardian
    // (`family-safety.md` § App surface → *Age-band surfaces*).
    let invite_age_band_select = AgeBandSelect::new(
        ids::ADMIN_USERS_INVITE_AGE_BAND_SELECT,
        fauna_client_admin::AGE_BAND_NOT_SET_VALUE,
    );
    invite_age_band_select.follow_guardian(&invite_guardian_select, true);
    create_form.append(invite_age_band_select.widget());
    invite_section.append(&create_form);

    // Button row.
    let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_box.set_margin_start(8);
    btn_box.set_margin_end(8);
    btn_box.set_margin_top(8);
    btn_box.set_margin_bottom(8);
    btn_box.set_halign(gtk::Align::End);

    let create_btn = gtk::Button::with_label(admin::settings_page::CREATE_CODE);
    create_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&create_btn, ids::CREATE_INVITE_CODE_BTN);

    let cancel_btn = gtk::Button::with_label(common::CANCEL);
    cancel_btn.set_visible(false);
    crate::testid::set_test_id(&cancel_btn, ids::ADMIN_SETTINGS_INVITE_CANCEL_BUTTON);

    let confirm_btn = gtk::Button::with_label(admin::settings_page::CREATE_CODE);
    confirm_btn.add_css_class("destructive-action");
    confirm_btn.set_visible(false);
    crate::testid::set_test_id(&confirm_btn, ids::CREATE_INVITE_CONFIRM_BTN);
    // `create_btn` only arms this form; the confirm is the mint (tui's
    // `admin::Action::ConfirmInvite`).
    crate::offline_gate::declare_wire_kind(&confirm_btn, "fauna.admin.invite_codes.create");

    // Copy the freshly minted token. Hidden until a code is minted.
    let copy_btn = gtk::Button::with_label(admin::users_page::COPY_CODE);
    copy_btn.set_visible(false);
    crate::testid::set_test_id(&copy_btn, ids::ADMIN_USERS_INVITE_CODE_COPY_BTN);

    btn_box.append(&copy_btn);
    btn_box.append(&create_btn);
    btn_box.append(&cancel_btn);
    btn_box.append(&confirm_btn);
    invite_section.append(&btn_box);
    scroll_body.append(&invite_section);

    let minted_code = Rc::new(RefCell::new(String::new()));

    // Create → reveal form + confirm/cancel.
    {
        let form = create_form.clone();
        let confirm_ref = confirm_btn.clone();
        let cancel_ref = cancel_btn.clone();
        create_btn.connect_clicked(move |btn| {
            btn.set_visible(false);
            form.set_visible(true);
            confirm_ref.set_visible(true);
            cancel_ref.set_visible(true);
        });
    }
    // Cancel → hide form.
    {
        let form = create_form.clone();
        let confirm_ref = confirm_btn.clone();
        let create_ref = create_btn.clone();
        cancel_btn.connect_clicked(move |btn| {
            btn.set_visible(false);
            form.set_visible(false);
            confirm_ref.set_visible(false);
            create_ref.set_visible(true);
        });
    }
    // Confirm → mint at the picked tier + max-uses (empty code ⇒ nest mints).
    {
        let client_ref = Rc::clone(client);
        let form = create_form.clone();
        let cancel_ref = cancel_btn.clone();
        let create_ref = create_btn.clone();
        let tier_dd = invite_tier_select.clone();
        let uses_entry = invite_max_uses.clone();
        let guardian_dd = invite_guardian_select.clone();
        let band_dd = invite_age_band_select.clone();
        confirm_btn.connect_clicked(move |btn| {
            let tier = dropdown_tier(&tier_dd).unwrap_or_else(|| DEFAULT_TIERS[0].to_string());
            let uses = uses_entry.text().trim().parse::<i64>().unwrap_or(1).max(1);
            let guardian = guardian_dd.selected();
            // The band rides only beside a guardian (the select is disabled
            // without one; this is the belt to that brace).
            let age_band = guardian.as_ref().and_then(|_| band_dd.selected());
            client_ref.create_admin_invite_code(tier, uses, guardian, age_band);
            btn.set_visible(false);
            form.set_visible(false);
            cancel_ref.set_visible(false);
            create_ref.set_visible(true);
            create_ref.set_sensitive(false);
        });
    }
    // Copy → write the last minted token to the clipboard.
    {
        let minted = Rc::clone(&minted_code);
        copy_btn.connect_clicked(move |_| {
            let code = minted.borrow().clone();
            if !code.is_empty() {
                crate::clipboard::copy_text(&code);
            }
        });
    }

    // ── Section 5: Users ─────────────────────────────────────────────────────
    let users_section = build_section("admin-users-list-section", admin::users_page::SECTION_USERS);

    let count_label = gtk::Label::new(Some(admin::users_page::LOADING));
    count_label.add_css_class("dim-label");
    count_label.set_halign(gtk::Align::Start);
    count_label.set_margin_start(12);
    count_label.set_margin_bottom(8);
    crate::testid::set_test_id(&count_label, ids::USER_COUNT_TEXT);
    users_section.append(&count_label);

    let users_list_box = gtk::ListBox::new();
    users_list_box.set_selection_mode(gtk::SelectionMode::None);
    users_list_box.add_css_class("boxed-list");
    users_list_box.set_margin_start(8);
    users_list_box.set_margin_end(8);
    users_list_box.set_margin_bottom(8);
    let users_placeholder = adw::StatusPage::builder()
        .title(admin::users_page::LOADING)
        .icon_name("avatar-default-symbolic")
        .build();
    users_list_box.set_placeholder(Some(&users_placeholder));
    users_section.append(&users_list_box);

    // Pagination controls (admin.md § Users — prev/next over `users.list`
    // `limit`/`offset`). `update_users_page` reflects the bounds with
    // `set_sensitive` (offset > 0 for prev; offset + page < total for next) —
    // that is the guard the USER meets. The click handlers additionally check
    // the same bounds, which is now pure defence in depth rather than the
    // primary guard: the old comment here justified keeping the buttons
    // sensitive on the grounds that "a reliable disabled-state read isn't
    // available on the Linux AT-SPI bridge", and that is no longer true —
    // `find::is_enabled` reads GTK's effective `is_sensitive()` and the
    // convention-11 actuation gate is built on it. Keep both: the handler guard
    // costs nothing and makes the bound safe even if a future refactor forgets
    // a `set_sensitive` call. The `admin-users-pagination` container id rides
    // the page-indicator `gtk::Label` (a bare `gtk::Box` doesn't surface its id
    // to AT-SPI — the section-anchor idiom).
    let pagination_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    pagination_box.set_halign(gtk::Align::Center);
    pagination_box.set_margin_top(8);
    pagination_box.set_margin_bottom(8);

    let users_prev_page = gtk::Button::with_label(admin::users_page::PREV_PAGE);
    crate::testid::set_test_id(&users_prev_page, ids::ADMIN_USERS_PREV_PAGE);
    {
        let client = Rc::clone(client);
        let offset = Rc::clone(&users_offset);
        users_prev_page.connect_clicked(move |_| {
            let cur = offset.get();
            if let Some(next) = fauna_core::format::prev_page_offset(cur, USERS_PAGE_SIZE) {
                offset.set(next);
                client.fetch_admin_users_page(next);
            }
        });
    }
    pagination_box.append(&users_prev_page);

    let users_pagination = gtk::Label::new(Some(&admin::users_page::page_indicator("1", "1")));
    users_pagination.add_css_class("dim-label");
    users_pagination.set_valign(gtk::Align::Center);
    crate::testid::set_test_id(&users_pagination, ids::ADMIN_USERS_PAGINATION);
    pagination_box.append(&users_pagination);

    let users_next_page = gtk::Button::with_label(admin::users_page::NEXT_PAGE);
    crate::testid::set_test_id(&users_next_page, ids::ADMIN_USERS_NEXT_PAGE);
    {
        let client = Rc::clone(client);
        let offset = Rc::clone(&users_offset);
        let total = Rc::clone(&users_total);
        users_next_page.connect_clicked(move |_| {
            let cur = offset.get();
            if let Some(next) =
                fauna_core::format::next_page_offset(cur, total.get(), USERS_PAGE_SIZE)
            {
                offset.set(next);
                client.fetch_admin_users_page(next);
            }
        });
    }
    pagination_box.append(&users_next_page);
    users_section.append(&pagination_box);

    scroll_body.append(&users_section);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&scroll_body)
        .build();
    outer.append(&scrolled);

    // `users_action_error` was built earlier (right after `users_total`) so the
    // Section 2 (Registration) save handler could capture it; append it here,
    // in its original visual position (after the scrolled body).
    outer.append(&users_action_error);

    UsersHubHandles {
        page: outer,
        users_list_box,
        users_page_count: count_label,
        users_offset,
        users_total,
        users_prev_page,
        users_next_page,
        users_pagination,
        invite_codes_list,
        create_invite_btn: create_btn,
        invite_guardian_select,
        invite_copy_btn: copy_btn,
        minted_code,
        invite_requests_list,
        tier_names,
        users_action_error,
        registration_readonly_label,
        registration_editable_box,
        registration_mode_select,
        registration_max_free_users_input,
        registration_age_verification_toggle,
        registration_age_verification_persisted,
    }
}

/// Short hex of a raw 32-byte actor id for display.
fn short_actor_id(actor_id: &[u8]) -> String {
    // Canonical short-id display, shared with web + native apps
    // (`fauna_core::format::short_id`): first 12 hex chars + `…`. Behaviour is
    // identical to the prior inline form this delegates from. Priority #1/#4.
    fauna_core::format::short_id(&fauna_core::format::hex_full(actor_id))
}

/// Build one row for a pending/decided invite request from a typed
/// [`AdminInviteRequest`]. Carries the `admin-invite-requests-list` component
/// IDs incl. the per-row `invite-request-row-tier-select` (the tier to admit at)
/// and `invite-request-row-guardian-select` (the guardian to admit *under*, for
/// a supervised admission — `family-safety.md` § App surface); approve/deny
/// call the shared `AdminClient` via `client`.
fn build_invite_request_row(
    request: &fauna_client_admin::admin::AdminInviteRequest,
    tier_names: &Rc<RefCell<Vec<String>>>,
    users: Option<&[fauna_client_admin::AdminUser]>,
    client: &Rc<FaunaClient>,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    let id = request.id;
    row.set_widget_name(&id.to_string());

    let actor = short_actor_id(request.actor_id.as_ref());
    row.set_title(&request.handle);
    row.set_subtitle(&request.message);

    let handle_label = gtk::Label::new(Some(&request.handle));
    handle_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&handle_label, ids::INVITE_REQUEST_ROW_HANDLE);
    row.add_prefix(&handle_label);

    let actor_label = gtk::Label::new(Some(&actor));
    actor_label.add_css_class("monospace");
    actor_label.add_css_class("fauna-muted");
    actor_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&actor_label, ids::INVITE_REQUEST_ROW_ACTOR);
    row.add_prefix(&actor_label);

    let message_label = gtk::Label::new(Some(&request.message));
    message_label.set_wrap(true);
    message_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&message_label, ids::INVITE_REQUEST_ROW_MESSAGE);
    row.add_suffix(&message_label);

    // Tier to admit at (DropDown; read at approve time).
    let tier_select = build_tier_select_dropdown(
        "invite-request-row-tier-select",
        DEFAULT_TIERS[0],
        tier_names,
        |_| {},
    );
    row.add_suffix(&tier_select);

    // Guardian to admit UNDER (DropDown; read at approve time). "None" = an
    // ordinary admission; picking a user admits the account supervised by them,
    // in one admission transaction (`family-safety.md` § Wire & data shape).
    let guardian_select = GuardianSelect::new("invite-request-row-guardian-select");
    guardian_select.populate(users);
    row.add_suffix(guardian_select.widget());

    // The applicant's recorded claim — total: absence IS the signal the
    // admitting admin reads (`family-safety.md` § The account age band, D6).
    let age_claim_label = gtk::Label::new(Some(
        &fauna_protocol::age::age_claim_label(
            request.age_band.as_deref(),
            request.age_band_provenance.as_deref(),
        )
        .resolve_nested(crate::i18n::strings::lookup),
    ));
    age_claim_label.add_css_class("dim-label");
    age_claim_label.set_wrap(true);
    crate::testid::set_test_id(&age_claim_label, ids::INVITE_REQUEST_ROW_AGE_CLAIM);
    row.add_suffix(&age_claim_label);

    // The band to admit at — seeded from the applicant's claim (D5: the claim
    // corroborates, the admitting adult decides), gated on the guardian.
    let pending = request.is_pending();
    let age_band_select = AgeBandSelect::new(
        ids::INVITE_REQUEST_ROW_AGE_BAND_SELECT,
        &fauna_client_admin::claimed_age_band_option(request.age_band.as_deref()),
    );
    age_band_select.follow_guardian(&guardian_select, pending);
    row.add_suffix(age_band_select.widget());

    let deny_reason = gtk::Entry::builder()
        .placeholder_text(admin::invite_requests_page::DENY_REASON_PLACEHOLDER)
        .build();
    crate::testid::set_test_id(&deny_reason, ids::INVITE_REQUEST_ROW_DENY_REASON_FIELD);
    row.add_suffix(&deny_reason);

    let deny_btn = gtk::Button::with_label(admin::invite_requests_page::DENY);
    deny_btn.add_css_class("destructive-action");
    crate::testid::set_test_id(&deny_btn, ids::INVITE_REQUEST_ROW_DENY_BUTTON);
    // tui's `admin::Action::DenyRequest`.
    crate::offline_gate::declare_wire_kind(&deny_btn, "fauna.admin.invite_requests.deny");
    row.add_suffix(&deny_btn);

    let approve_btn = gtk::Button::with_label(admin::invite_requests_page::APPROVE);
    approve_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&approve_btn, ids::INVITE_REQUEST_ROW_APPROVE_BUTTON);
    // tui's `admin::Action::ApproveRequest`.
    crate::offline_gate::declare_wire_kind(&approve_btn, "fauna.admin.invite_requests.approve");
    row.add_suffix(&approve_btn);

    // Already-decided rows display for context but the actions don't fire.
    approve_btn.set_sensitive(pending);
    deny_btn.set_sensitive(pending);
    tier_select.set_sensitive(pending);
    guardian_select.widget().set_sensitive(pending);

    {
        let client = Rc::clone(client);
        let tier_dd = tier_select.clone();
        let guardian_dd = guardian_select.clone();
        let band_dd = age_band_select.clone();
        approve_btn.connect_clicked(move |_| {
            let guardian = guardian_dd.selected();
            let age_band = guardian.as_ref().and_then(|_| band_dd.selected());
            client.approve_invite_request(id, dropdown_tier(&tier_dd), guardian, age_band);
        });
    }
    {
        let client = Rc::clone(client);
        deny_btn.connect_clicked(move |_| {
            let reason = deny_reason.text().to_string().trim().to_string();
            let reason = if reason.is_empty() {
                None
            } else {
                Some(reason)
            };
            client.deny_invite_request(id, reason);
        });
    }

    row
}

/// Populate the Pending requests section from a typed
/// [`AdminInviteRequestsListReply`]. Caches the reply so a later
/// `AdminUsersLoaded` can re-render the rows with a populated guardian picker
/// (see [`refresh_guardian_pickers`]).
pub fn update_invite_requests(
    handles: &AdminHandles,
    reply: &fauna_client_admin::admin::AdminInviteRequestsListReply,
    client: &Rc<FaunaClient>,
) {
    *handles.last_invite_requests.borrow_mut() = Some(reply.clone());
    render_invite_requests(handles, reply, client);
}

fn render_invite_requests(
    handles: &AdminHandles,
    reply: &fauna_client_admin::admin::AdminInviteRequestsListReply,
    client: &Rc<FaunaClient>,
) {
    // A successful requests render clears any prior Users-section action error.
    render_page_error(&handles.users_action_error, None);
    clear_list_box(&handles.invite_requests_list);
    let users = handles.picker_users.borrow();
    for req in &reply.invite_requests {
        let row = build_invite_request_row(req, &handles.tier_names, users.as_deref(), client);
        handles.invite_requests_list.append(&row);
    }
}

/// Refill both guardian pickers (`family-safety.md` § App surface → Admission
/// surfaces) from every account on the nest ([`AdminHandles::picker_users`]).
/// Called whenever that list lands ([`set_admin_picker_users`]): the users fetch
/// and the invite-requests fetch are fired together at admin confirmation and
/// race, so a row built before the accounts landed would offer only "None". The
/// mint-form picker is repopulated in place (preserving its selection); the
/// pending rows are re-rendered from the cached requests reply.
pub fn refresh_guardian_pickers(handles: &AdminHandles, client: &Rc<FaunaClient>) {
    handles
        .invite_guardian_select
        .populate(handles.picker_users.borrow().as_deref());
    let cached = handles.last_invite_requests.borrow().clone();
    if let Some(reply) = cached {
        render_invite_requests(handles, &reply, client);
    }
}

/// Store the defined tier names so the page's tier pickers cycle through the
/// real set (`fauna.admin.tiers.list`), AND render the tier *definitions* on the
/// Settings page (`admin-settings-tier-item`). Empty list keeps the seeded
/// picker defaults and leaves the definitions list empty.
pub fn update_tiers(
    handles: &AdminHandles,
    reply: &fauna_client_admin::admin::AdminTiersListReply,
    client: &Rc<FaunaClient>,
) {
    if reply.tiers.is_empty() {
        // Reply arrived with no tier definitions: swap the loading placeholder
        // for the empty-state (`admin.settings_page.no_tiers`), matching web's
        // loading_tiers → no_tiers distinction (the placeholder is built as
        // LOADING_TIERS, so before any reply it correctly reads as loading).
        // Clear any prior rows so the placeholder shows.
        clear_list_box(&handles.settings_tiers_list);
        let placeholder = adw::StatusPage::builder()
            .title(admin::settings_page::NO_TIERS)
            .icon_name("emblem-system-symbolic")
            .build();
        handles
            .settings_tiers_list
            .set_placeholder(Some(&placeholder));
        return;
    }
    *handles.tier_names.borrow_mut() = reply.tiers.iter().map(|t| t.name.clone()).collect();

    // Settings page: one `admin-settings-tier-item` row per definition, with the
    // tier name + an editable raw-i64 input per cap + a save button (the tier
    // *is* the quota — admin.md § 3, in-place tier-cap editing).
    clear_list_box(&handles.settings_tiers_list);
    for tier in &reply.tiers {
        handles
            .settings_tiers_list
            .append(&build_tier_definition_row(tier, client));
    }
}

/// An editable tier-*definition* row (`admin-settings-tier-item`): the tier name
/// (read-only — it identifies the row) + one raw-i64 `gtk::Entry` per `AdminTier`
/// cap (`admin-settings-tier-cap-*`, pre-filled with the persisted value) + an
/// `admin-settings-tier-save-button` firing `fauna.admin.tiers.update`. Raw
/// integers (bytes for byte caps, counts otherwise); a unit-aware editor is a
/// future shared-Rust refinement (admin.md § 3). A `gtk::ListBoxRow` (not
/// `adw::ActionRow`) and ids that ride a `Label`/`Entry`/`Button` so they surface
/// to AT-SPI — same constraint as `build_user_row` / `update_invite_codes`.
fn build_tier_definition_row(
    tier: &fauna_client_admin::admin::AdminTier,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    crate::testid::set_test_id(&row, ids::ADMIN_SETTINGS_TIER_ITEM);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let name = gtk::Label::new(Some(&tier.name));
    name.add_css_class("heading");
    name.set_halign(gtk::Align::Start);
    vbox.append(&name);

    // One labeled raw-i64 entry per cap, pre-filled with the persisted value.
    let inbox = build_cap_entry(
        &vbox,
        admin::settings_page::CAP_INBOX_BYTES,
        "admin-settings-tier-cap-inbox",
        tier.max_inbox_bytes,
    );
    let storage = build_cap_entry(
        &vbox,
        admin::settings_page::CAP_STORAGE_BYTES,
        "admin-settings-tier-cap-storage",
        tier.max_storage_bytes,
    );
    let devices = build_cap_entry(
        &vbox,
        admin::settings_page::CAP_DEVICES,
        "admin-settings-tier-cap-devices",
        tier.max_devices,
    );
    let blob_size = build_cap_entry(
        &vbox,
        admin::settings_page::CAP_BLOB_SIZE,
        "admin-settings-tier-cap-blob-size",
        tier.max_blob_size,
    );
    let feeds = build_cap_entry(
        &vbox,
        admin::settings_page::CAP_FEEDS,
        "admin-settings-tier-cap-feeds",
        tier.max_feeds,
    );

    let save = gtk::Button::with_label(admin::settings_page::SAVE_TIER);
    save.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&save, ids::ADMIN_SETTINGS_TIER_SAVE_BUTTON);
    // tui's `admin::Action::SaveTier`.
    crate::offline_gate::declare_wire_kind(&save, "fauna.admin.tiers.update");
    {
        let client = Rc::clone(client);
        // The save replaces *all* caps for the named tier; an unparseable field
        // falls back to the value the row was rendered with (no silent zeroing).
        let name = tier.name.clone();
        let prev = tier.clone();
        save.connect_clicked(move |_| {
            let req = fauna_client_admin::admin::AdminTierUpdateRequest {
                name: name.clone(),
                max_inbox_bytes: parse_cap(&inbox, prev.max_inbox_bytes),
                max_storage_bytes: parse_cap(&storage, prev.max_storage_bytes),
                max_devices: parse_cap(&devices, prev.max_devices),
                max_blob_size: parse_cap(&blob_size, prev.max_blob_size),
                max_feeds: parse_cap(&feeds, prev.max_feeds),
                extra: Default::default(),
            };
            client.update_admin_tier(req);
        });
    }
    vbox.append(&save);

    row.set_child(Some(&vbox));
    row
}

/// Build a labeled raw-i64 cap `gtk::Entry` (id `test_id`), pre-filled with
/// `value`, appended to `parent`. Returns the entry so the save handler can read
/// it back.
fn build_cap_entry(parent: &gtk::Box, label: &str, test_id: &str, value: i64) -> gtk::Entry {
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let lbl = gtk::Label::new(Some(label));
    lbl.add_css_class("dim-label");
    lbl.set_halign(gtk::Align::Start);
    lbl.set_width_chars(16);
    lbl.set_xalign(0.0);
    hbox.append(&lbl);

    let entry = gtk::Entry::builder().hexpand(true).build();
    entry.set_text(&value.to_string());
    crate::testid::set_test_id(&entry, test_id);
    hbox.append(&entry);

    parent.append(&hbox);
    entry
}

/// Parse a cap entry's text as a non-negative `i64`; fall back to `prev` on an
/// empty/unparseable value so a stray edit never silently zeroes a cap. The
/// trim/parse/clamp lives in the shared validator `fauna_core::format::parse_cap`
/// (one source of truth across all seven apps — see
/// `docs/goal/behavior/value-formatting.md` § Tier cap validation); this thin
/// wrapper just adapts the `gtk::Entry` to its `&str` input. Byte-identical to the
/// old local parse: a persisted `prev` is always ≥ 0, so the shared fn's
/// clamp-on-parse matches the old clamp-on-result.
fn parse_cap(entry: &gtk::Entry, prev: i64) -> i64 {
    fauna_core::format::parse_cap(&entry.text()).unwrap_or(prev)
}

/// Re-render the membership section (`admin-settings-membership-item`,
/// monetization.md § Pillar 4) from BOTH cached sources —
/// `own_membership_tier_names` (the row set: every subscription tier the
/// admin owns) and `last_membership_tiers` (which of those rows already carry
/// a designation). Called from both fetches' arrival handlers, mirroring
/// `refresh_guardian_pickers`'s race tolerance — whichever lands second
/// produces the final correct render.
///
/// An empty row set is the normal out-of-the-box state (no subscription
/// tiers minted yet) — rendered as an empty-state pointer at the admin's own
/// Tiers tab (admin.md § Users → Paid nest access: "designating does not
/// create either kind of tier"), never an error.
pub fn update_membership_tiers(handles: &AdminHandles, client: &Rc<FaunaClient>) {
    let own_names = handles.own_membership_tier_names.borrow().clone();
    clear_list_box(&handles.membership_tiers_list);
    if own_names.is_empty() {
        let placeholder = adw::StatusPage::builder()
            .title(admin::settings_page::NO_MEMBERSHIP_TIERS)
            .icon_name("emblem-system-symbolic")
            .build();
        handles
            .membership_tiers_list
            .set_placeholder(Some(&placeholder));
        return;
    }
    let designations = handles.last_membership_tiers.borrow();
    for tier_name in &own_names {
        let existing = designations.as_ref().and_then(|reply| {
            reply
                .membership_tiers
                .iter()
                .find(|m| &m.tier_name == tier_name)
        });
        handles.membership_tiers_list.append(&build_membership_row(
            tier_name,
            existing,
            &handles.own_membership_tier_names,
            &handles.tier_names,
            client,
        ));
    }
}

/// One `admin-settings-membership-item` row: the subscription tier name (via
/// `admin-settings-membership-tier-select`, pre-selected to this row's own
/// tier — day-to-day it is a display, but a real select per the approved
/// shape, and it lets an admin fix a mis-set row without deleting it), the
/// admitted/lapsed quota-tier selects (pre-filled from `existing` when
/// designated, else `admin.md`'s documented default — `free` for lapse, the
/// first quota tier for admit), and Save/Clear. Save always sends an explicit
/// `Some(lapse_tier)` (never relies on the wire's omit-means-default, since
/// the row always shows a definite selection). Clear is desensitized when
/// this row carries no designation yet — nothing to clear.
fn build_membership_row(
    tier_name: &str,
    existing: Option<&fauna_client_admin::admin::AdminMembershipTier>,
    own_tier_names: &Rc<RefCell<Vec<String>>>,
    quota_tier_names: &Rc<RefCell<Vec<String>>>,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    crate::testid::set_test_id(&row, ids::ADMIN_SETTINGS_MEMBERSHIP_ITEM);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let tier_select = build_tier_select_dropdown(
        "admin-settings-membership-tier-select",
        tier_name,
        own_tier_names,
        |_| {}, // Display + fix-up only — Save reads the current selection directly.
    );
    vbox.append(&tier_select);

    let admin_tier_initial = existing.map(|m| m.admin_tier.as_str()).unwrap_or("");
    let admin_tier_hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let admin_tier_label = gtk::Label::new(Some(admin::settings_page::MEMBERSHIP_ADMITS_AT));
    admin_tier_label.add_css_class("dim-label");
    admin_tier_label.set_halign(gtk::Align::Start);
    admin_tier_label.set_width_chars(12);
    admin_tier_hbox.append(&admin_tier_label);
    let admin_tier_select = build_tier_select_dropdown(
        "admin-settings-membership-admin-tier-select",
        admin_tier_initial,
        quota_tier_names,
        |_| {},
    );
    admin_tier_hbox.append(&admin_tier_select);
    vbox.append(&admin_tier_hbox);

    let lapse_tier_initial = existing
        .map(|m| m.lapse_tier.as_str())
        .unwrap_or(fauna_protocol::admin::DEFAULT_LAPSE_TIER);
    let lapse_tier_hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let lapse_tier_label = gtk::Label::new(Some(admin::settings_page::MEMBERSHIP_LAPSES_TO));
    lapse_tier_label.add_css_class("dim-label");
    lapse_tier_label.set_halign(gtk::Align::Start);
    lapse_tier_label.set_width_chars(12);
    lapse_tier_hbox.append(&lapse_tier_label);
    let lapse_tier_select = build_tier_select_dropdown(
        "admin-settings-membership-lapse-tier-select",
        lapse_tier_initial,
        quota_tier_names,
        |_| {},
    );
    lapse_tier_hbox.append(&lapse_tier_select);
    vbox.append(&lapse_tier_hbox);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let save = gtk::Button::with_label(admin::settings_page::MEMBERSHIP_SAVE);
    crate::testid::set_test_id(&save, ids::ADMIN_SETTINGS_MEMBERSHIP_SAVE_BUTTON);
    // tui's `admin::Action::SaveMembership`.
    crate::offline_gate::declare_wire_kind(&save, "fauna.admin.membership_tiers.set");
    {
        let client = Rc::clone(client);
        let tier_select = tier_select.clone();
        let admin_tier_select = admin_tier_select.clone();
        let lapse_tier_select = lapse_tier_select.clone();
        save.connect_clicked(move |_| {
            let Some(tier_name) = dropdown_tier(&tier_select) else {
                return;
            };
            let Some(admin_tier) = dropdown_tier(&admin_tier_select) else {
                return;
            };
            let lapse_tier = dropdown_tier(&lapse_tier_select);
            client.save_membership_tier(fauna_client_admin::admin::AdminMembershipTierSetRequest {
                tier_name,
                admin_tier,
                lapse_tier,
                extra: Default::default(),
            });
        });
    }
    buttons.append(&save);

    let clear = gtk::Button::with_label(admin::settings_page::MEMBERSHIP_CLEAR);
    crate::testid::set_test_id(&clear, ids::ADMIN_SETTINGS_MEMBERSHIP_CLEAR_BUTTON);
    clear.set_sensitive(existing.is_some());
    // tui's `admin::Action::ClearMembership`.
    crate::offline_gate::declare_wire_kind(&clear, "fauna.admin.membership_tiers.clear");
    {
        let client = Rc::clone(client);
        let tier_name = tier_name.to_string();
        clear.connect_clicked(move |_| {
            client.clear_membership_tier(tier_name.clone());
        });
    }
    buttons.append(&clear);

    vbox.append(&buttons);
    row.set_child(Some(&vbox));
    row
}

/// Seed the `admin-nest-serving-port-input` from `fauna.setup.status`'s
/// `serving_port` (default 443 when the admin never set it). A plain text set
/// (the entry only dispatches on the save button, never on change), guarded so a
/// re-seed doesn't clobber an in-progress edit.
///
/// `fronted` (`fauna.setup.status` `fronted_by_router`) renders the field
/// read-only on a router-fronted nest: the entry + save button are desensitized
/// and the "served on 443 by this deployment" hint is revealed (the chosen port
/// is inert behind the :443 SNI router, and the nest rejects a write). On a
/// direct listener (the default) the field stays editable. nest/common.md
/// § Serving ports.
pub fn set_nest_serving_port(handles: &AdminHandles, port: u16, fronted: bool) {
    let text = port.to_string();
    if handles.serving_port_entry.text() != text {
        handles.serving_port_entry.set_text(&text);
    }
    handles.serving_port_entry.set_sensitive(!fronted);
    handles.serving_port_save.set_sensitive(!fronted);
    handles.serving_port_hint.set_visible(fronted);
}

/// Seed the declared-region section from `fauna.admin.region.get`'s reply,
/// via the shared `fauna_client_admin::AdminRegionView` fold (tui's
/// `admin/nest.rs`, the reference leg — this file decides nothing about the
/// plane). The draft entry mirrors the declaration, guarded so a re-seed
/// doesn't clobber an in-progress edit — and so a withdrawal empties the
/// field rather than leaving the withdrawn code sitting in it looking
/// declared. The authority/staleness lines paint only when the fold says so,
/// and the withdraw button shows only while `can_withdraw`.
pub fn set_nest_region(handles: &AdminHandles, view: &fauna_client_admin::AdminRegionView) {
    let declared = view.declared.clone().unwrap_or_default();
    if handles.region_entry.text() != declared {
        handles.region_entry.set_text(&declared);
    }
    handles
        .region_status
        .set_text(&view.status.resolve(crate::i18n::strings::lookup));
    match view.authority.as_ref() {
        Some(authority) => {
            handles
                .region_authority
                .set_text(&authority.resolve(crate::i18n::strings::lookup));
            handles.region_authority.set_visible(true);
        }
        None => {
            handles.region_authority.set_text("");
            handles.region_authority.set_visible(false);
        }
    }
    match view.staleness.as_ref() {
        Some(stale) => {
            handles
                .region_staleness
                .set_text(&stale.resolve(crate::i18n::strings::lookup));
            handles.region_staleness.set_visible(true);
        }
        None => {
            handles.region_staleness.set_text("");
            handles.region_staleness.set_visible(false);
        }
    }
    handles.region_withdraw.set_visible(view.can_withdraw);
}

/// Render the host-OS-maintenance indicator from `fauna.setup.status`'s `os_*`
/// fields (installers/vps.md § Host OS Maintenance § 4). The status line maps
/// through the shared `fauna_core::format::os_maintenance_status_label` (the same
/// state→key decision every app uses); the count badge shows the raw pending
/// count only when `> 0`; the restart-now button shows only when a reboot is
/// pending. A nest with no host channel reports the serde-default zeros → the
/// line reads "OS up to date" and neither the badge nor the button render.
pub fn set_nest_os_maintenance(
    handles: &AdminHandles,
    security_updates_pending: u32,
    reboot_pending: bool,
) {
    handles.os_maintenance_status.set_text(
        &fauna_core::format::os_maintenance_status_label(security_updates_pending, reboot_pending)
            .resolve(crate::i18n::strings::lookup),
    );
    if security_updates_pending > 0 {
        handles
            .os_updates_count
            .set_text(&security_updates_pending.to_string());
        handles.os_updates_count.set_visible(true);
    } else {
        handles.os_updates_count.set_visible(false);
    }
    handles.os_restart_now_button.set_visible(reboot_pending);
}

/// Surface a freshly minted invite-code token copyable: stash it for the copy
/// button and reveal the button (its tooltip carries the token so a headless
/// e2e can read it via `get_attr`).
pub fn show_minted_invite_code(
    handles: &AdminHandles,
    reply: &fauna_client_admin::admin::AdminInviteCodeCreateReply,
) {
    *handles.minted_code.borrow_mut() = reply.code.clone();
    handles
        .invite_copy_btn
        .set_tooltip_text(Some(&admin::users_page::minted_code(&reply.code)));
    handles.invite_copy_btn.set_visible(true);
}

// ---------------------------------------------------------------------------
// Dashboard sub-page (existing logic, extracted)
// ---------------------------------------------------------------------------

struct DashboardHandles {
    stats_users_row: adw::ActionRow,
    stats_storage_row: adw::ActionRow,
    stats_inbox_row: adw::ActionRow,
    stats_sessions_row: adw::ActionRow,
    // Hidden per-card E2E value markers (`admin-stat-card-value`), updated on
    // data load — see `attach_stat_markers`.
    stats_users_value: gtk::Label,
    stats_storage_value: gtk::Label,
    stats_inbox_value: gtk::Label,
    stats_sessions_value: gtk::Label,
    users_group: adw::PreferencesGroup,
    server_version_row: adw::ActionRow,
    server_version_value: gtk::Label,
    server_uptime_row: adw::ActionRow,
    server_workers_row: adw::ActionRow,
}

/// Attach hidden per-card E2E markers to a dashboard stat-card row:
/// `admin-stat-card-label` (the card's title text) + `admin-stat-card-value`
/// (empty until the data-load path fills it). Returns the value marker so the
/// loader can update it. One label+value pair PER card lets the shared
/// `dashboard_card_value(title)` e2e accessor match a card by its title and read
/// that card's value — the same contract every other app implements (e.g.
/// web's per-card `admin-stat-card-label`/`-value` divs in
/// apps/fauna-web/src/routes/admin/+page.svelte). The value marker starts empty
/// (not "—") so a test that polls for a non-empty value waits for the async
/// fetch rather than parsing the placeholder.
fn attach_stat_markers(row: &adw::ActionRow, title: &str) -> gtk::Label {
    let label_marker = gtk::Label::new(Some(title));
    label_marker.set_height_request(1);
    label_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&label_marker, ids::ADMIN_STAT_CARD_LABEL);
    row.add_suffix(&label_marker);
    let value_marker = gtk::Label::new(None);
    value_marker.set_height_request(1);
    value_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&value_marker, ids::ADMIN_STAT_CARD_VALUE);
    row.add_suffix(&value_marker);
    value_marker
}

fn build_dashboard_page() -> (adw::PreferencesPage, DashboardHandles) {
    let page = adw::PreferencesPage::new();

    // --- Nest Statistics group ---
    let stats_group = adw::PreferencesGroup::new();
    stats_group.set_title(admin::view::NEST_STATISTICS);
    let dashboard_heading = gtk::Label::new(Some(admin::view::NEST_STATISTICS));
    dashboard_heading.set_height_request(1);
    dashboard_heading.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&dashboard_heading, ids::ADMIN_DASHBOARD_HEADING);
    stats_group.set_header_suffix(Some(&dashboard_heading));

    let stats_users_row = adw::ActionRow::builder()
        .title(admin::view::REGISTERED_USERS)
        .subtitle("—")
        .build();
    crate::testid::set_test_id(&stats_users_row, ids::ADMIN_STAT_CARD);
    stats_group.add(&stats_users_row);

    let stats_storage_row = adw::ActionRow::builder()
        .title(admin::view::TOTAL_STORAGE_USED)
        .subtitle("—")
        .build();
    crate::testid::set_test_id(&stats_storage_row, ids::ADMIN_STAT_CARD);
    stats_group.add(&stats_storage_row);

    let stats_inbox_row = adw::ActionRow::builder()
        .title(admin::view::TOTAL_INBOX_MESSAGES)
        .subtitle("—")
        .build();
    crate::testid::set_test_id(&stats_inbox_row, ids::ADMIN_STAT_CARD);
    stats_group.add(&stats_inbox_row);

    let stats_sessions_row = adw::ActionRow::builder()
        .title(admin::view::ACTIVE_SESSIONS)
        .subtitle("—")
        .build();
    crate::testid::set_test_id(&stats_sessions_row, ids::ADMIN_STAT_CARD);
    stats_group.add(&stats_sessions_row);

    page.add(&stats_group);

    // Per-card hidden E2E markers (label = title, value = data-on-load). One
    // pair per card so `dashboard_card_value(title)` can match each card.
    let stats_users_value = attach_stat_markers(&stats_users_row, admin::view::REGISTERED_USERS);
    let stats_storage_value =
        attach_stat_markers(&stats_storage_row, admin::view::TOTAL_STORAGE_USED);
    let stats_inbox_value =
        attach_stat_markers(&stats_inbox_row, admin::view::TOTAL_INBOX_MESSAGES);
    let stats_sessions_value =
        attach_stat_markers(&stats_sessions_row, admin::view::ACTIVE_SESSIONS);

    // --- Recent Users group ---
    let users_group = adw::PreferencesGroup::new();
    users_group.set_title(admin::view::RECENT_USERS);
    users_group.set_description(Some(admin::view::RECENT_USERS_DESC));

    let placeholder_row = adw::ActionRow::builder()
        .title(admin::view::NO_USERS_LOADED)
        .build();
    users_group.add(&placeholder_row);

    page.add(&users_group);

    // --- Server Status group ---
    let status_group = adw::PreferencesGroup::new();
    status_group.set_title(admin::view::SERVER_STATUS);

    let server_version_row = adw::ActionRow::builder()
        .title(admin::dashboard::VERSION)
        .subtitle("—")
        .build();
    let server_version_value = attach_stat_markers(&server_version_row, admin::dashboard::VERSION);
    status_group.add(&server_version_row);

    let server_uptime_row = adw::ActionRow::builder()
        .title(admin::view::UPTIME)
        .subtitle("—")
        .build();
    status_group.add(&server_uptime_row);

    let server_workers_row = adw::ActionRow::builder()
        .title(admin::view::WORKERS)
        .subtitle("—")
        .build();
    status_group.add(&server_workers_row);

    page.add(&status_group);

    let handles = DashboardHandles {
        stats_users_row,
        stats_storage_row,
        stats_inbox_row,
        stats_sessions_row,
        stats_users_value,
        stats_storage_value,
        stats_inbox_value,
        stats_sessions_value,
        users_group,
        server_version_row,
        server_version_value,
        server_uptime_row,
        server_workers_row,
    };

    (page, handles)
}

// ---------------------------------------------------------------------------
// Settings sub-page (nav label "Tiers") — tier *definitions* only (policy;
// admission lives on the Users hub). The invite-code section moved to the Users
// hub's Invite section (2026-05-29, admin.md § 3); domain management moved to
// `admin-dns` (2026-05-25); the read-only storage-mode indicator + the Factory
// Reset danger zone moved to the Nest page (admin.md § Admin IA redesign,
// 2026-06-04). Editing a tier's caps in place is not yet surfaced — ui.yaml has
// no edit-field IDs for it (admin.md § 3 "view / edit", edit half pending
// approval-gated IDs), so this builds the display only. Returns the tier-item
// list box for the update path.
// ---------------------------------------------------------------------------

fn build_settings_page() -> (gtk::Box, gtk::ListBox, gtk::ListBox) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let heading = gtk::Label::new(Some(admin::settings_page::TITLE));
    heading.add_css_class("title-2");
    heading.set_margin_top(16);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&heading, ids::ADMIN_SETTINGS_HEADING);
    outer.append(&heading);

    // ── Tier definitions (`admin-settings-tiers-section` + items) ────────────
    // The section anchor rides a `gtk::Label` (a bare Box/ActionRow doesn't
    // surface its id as a findable AT-SPI name — see `build_section`).
    let tiers_section = build_section("admin-settings-tiers-section", admin::settings_page::TIERS);

    let settings_tiers_list = gtk::ListBox::new();
    settings_tiers_list.set_selection_mode(gtk::SelectionMode::None);
    settings_tiers_list.add_css_class("boxed-list");
    settings_tiers_list.set_margin_start(8);
    settings_tiers_list.set_margin_end(8);
    settings_tiers_list.set_margin_bottom(8);
    let tiers_placeholder = adw::StatusPage::builder()
        .title(admin::settings_page::LOADING_TIERS)
        .icon_name("emblem-system-symbolic")
        .build();
    settings_tiers_list.set_placeholder(Some(&tiers_placeholder));
    tiers_section.append(&settings_tiers_list);
    outer.append(&tiers_section);

    // ── Membership designations (`admin-settings-membership-section` + items,
    // monetization.md § Pillar 4) — a link editor over the admin's own
    // subscription tiers, never a third tier list. ─────────────────────────
    let membership_section = build_section(
        "admin-settings-membership-section",
        admin::settings_page::MEMBERSHIP_SECTION,
    );

    let membership_tiers_list = gtk::ListBox::new();
    membership_tiers_list.set_selection_mode(gtk::SelectionMode::None);
    membership_tiers_list.add_css_class("boxed-list");
    membership_tiers_list.set_margin_start(8);
    membership_tiers_list.set_margin_end(8);
    membership_tiers_list.set_margin_bottom(8);
    let membership_placeholder = adw::StatusPage::builder()
        .title(admin::settings_page::LOADING_MEMBERSHIP)
        .icon_name("emblem-system-symbolic")
        .build();
    membership_tiers_list.set_placeholder(Some(&membership_placeholder));
    membership_section.append(&membership_tiers_list);
    outer.append(&membership_section);

    // Per-page `error-message` (cross-app rule #2: every page carries one).
    outer.append(&build_error_label());

    (outer, settings_tiers_list, membership_tiers_list)
}

/// The admin "Danger zone" — a destructive "Factory reset this nest" button
/// (`admin-factory-reset-button`) behind an `adw::AlertDialog` confirm. On
/// confirm it calls `FaunaClient::factory_reset`, which hits the Admin-gated
/// `fauna.admin.factory_reset`; the reply's claim code drives the re-onboard
/// (the message-pump `FactoryResetComplete` handler tears down the session and
/// re-seeds onboarding at claim-code with the code pre-filled). Admin-gated
/// **nest-side** — `fauna.admin.factory_reset` is `require_admin`, and that is
/// the whole boundary: the admin view itself is built regardless of `is_admin`
/// (only the sidebar *row* is gated, `app.rs`'s `show_admin_sidebar_row`), the
/// entry-level-never-content-level shape all 7 apps share
/// (`docs/goal/behavior/admin.md` § Entry-level, never content-level). v1 of the
/// kind has no extra cooldown gate (admin.rs § Factory reset). Per
/// `docs/goal/behavior/mail-bridge-lifecycle.md` § Factory reset.
fn build_factory_reset_section() -> gtk::Box {
    let section = build_section(
        "admin-factory-reset-section",
        admin::settings_page::FACTORY_RESET_SECTION,
    );

    let row = adw::ActionRow::builder()
        .title(admin::settings_page::FACTORY_RESET_TITLE)
        .subtitle(admin::settings_page::FACTORY_RESET_DESC)
        .build();

    let reset_btn = gtk::Button::builder()
        .label(admin::settings_page::FACTORY_RESET_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    crate::testid::set_test_id(&reset_btn, ids::ADMIN_FACTORY_RESET_BUTTON);
    // The click only opens the confirm dialog, but the ceremony behind it can
    // end in exactly one kind, so the ENTRY is what declares — offering a
    // ceremony that must dead-end at its confirm is worse than withholding it.
    // (tui declares on `ConfirmFactoryReset` because there the confirm IS the
    // element in the list; linux's confirm lives in a transient dialog.)
    crate::offline_gate::declare_wire_kind(&reset_btn, "fauna.admin.factory_reset");

    reset_btn.connect_clicked(|btn| {
        // No wire kind here — it is declared on the ENTRY button above, per the
        // comment there.
        crate::confirm_dialog::present_confirm(
            btn,
            crate::confirm_dialog::ConfirmSpec::new(
                admin::settings_page::FACTORY_RESET_CONFIRM_TITLE,
                crate::confirm_dialog::ConfirmBody::Text(
                    admin::settings_page::FACTORY_RESET_CONFIRM_BODY,
                ),
                "factory-reset",
                admin::settings_page::FACTORY_RESET_CONFIRM_BUTTON,
                admin::settings_page::FACTORY_RESET_CANCEL,
            )
            .with_confirm_id(ids::ADMIN_FACTORY_RESET_CONFIRM_BUTTON)
            .with_cancel_id(ids::ADMIN_FACTORY_RESET_CANCEL_BUTTON),
            || {
                if let Some(client) = crate::settings::get_client() {
                    client.factory_reset();
                } else {
                    tracing::error!("factory_reset: no client available");
                }
            },
        );
    });

    row.add_suffix(&reset_btn);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    list.set_margin_start(8);
    list.set_margin_end(8);
    list.set_margin_bottom(8);
    list.append(&row);
    section.append(&list);

    section
}

// ---------------------------------------------------------------------------
// Update functions
// ---------------------------------------------------------------------------

/// Store the latest [`LocalDomainsSnapshot`] (active + soft-deleted +
/// `is_primary`) and re-render the unified admin-dns page. Called on every
/// `AdminLocalDomainsLoaded` (page load + after add/remove/restore).
pub fn set_local_domains_snapshot(
    handles: &AdminHandles,
    snapshot: &LocalDomainsSnapshot,
    client: &Rc<FaunaClient>,
) {
    *handles.last_local_domains.borrow_mut() = Some(snapshot.clone());
    render_admin_dns(handles, client);
}

/// Cache every account on the nest ([`AdminHandles::picker_users`]) — the option
/// source of the admin-dns catch-all + role-address pickers and both guardian
/// pickers — then re-render admin-dns and refill the guardian pickers, so each
/// populates whichever of its fetches landed first. Called from the
/// `AdminUsersLoaded` handler alongside the users-page + dashboard renders.
pub fn set_admin_picker_users(
    handles: &AdminHandles,
    users: &[fauna_client_admin::AdminUser],
    client: &Rc<FaunaClient>,
) {
    *handles.picker_users.borrow_mut() = Some(users.to_vec());
    render_admin_dns(handles, client);
    refresh_guardian_pickers(handles, client);
}

/// Store the latest [`DnsSnapshot`] (per-domain record matrix + verdicts) and
/// re-render the unified admin-dns page. Called on every `AdminDnsRecordsLoaded`.
pub fn set_dns_snapshot(handles: &AdminHandles, snapshot: &DnsSnapshot, client: &Rc<FaunaClient>) {
    *handles.last_dns.borrow_mut() = Some(snapshot.clone());
    render_admin_dns(handles, client);
}

// ---------------------------------------------------------------------------
// Bridges-pending sub-page (I3 Phase H — admin approves enrolled mail bridges)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Nest sub-page (`admin-nest`) — nest-wide settings (admin.md § N Nest). Holds
// the admin pairing toggle + the Factory Reset danger zone. Replaces the
// removed standalone Services page (the per-page-services redesign, admin.md
// § Admin IA redesign, 2026-06-04: the vestigial `bridge` + residue
// `algorithm` toggles were dropped, mail on/off is the Mail page's
// `admin-mail-enabled-toggle`, and the dns master switch lives on
// `admin-dns` as `admin-dns-manage-all-toggle`). The read-only storage-mode
// indicator is retired with the storage-mode question itself
// (storage-modes.md § no-modes).
// ---------------------------------------------------------------------------

/// Widget handles produced by [`build_nest_page`], folded into [`AdminHandles`].
struct NestPage {
    page: gtk::Box,
    /// `admin-service-pairing-toggle` + `-pairing-status` — reflective admin
    /// pairing knob (synced by `update_services`), guarded against the
    /// programmatic `set_active`.
    pairing_toggle: gtk::Switch,
    pairing_status: gtk::Label,
    pairing_guard: Rc<std::cell::Cell<bool>>,
    /// `admin-nest-serving-port-input` — the admin-set client-facing API serving
    /// port entry (seeded by `set_nest_serving_port` once `fauna.setup.status`
    /// lands; the save button drives `fauna.admin.set_serving_port`).
    serving_port_entry: gtk::Entry,
    /// `admin-nest-serving-port-save-button` — desensitized alongside the entry
    /// when the deployment is router-fronted (`set_nest_serving_port`).
    serving_port_save: gtk::Button,
    /// Read-only hint shown only on a router-fronted nest (`set_nest_serving_port`
    /// flips its visibility from `fronted_by_router`).
    serving_port_hint: gtk::Label,
    /// Declared region (`admin-nest-region-*`) — see [`AdminHandles`]'s
    /// fields of the same name for what each holds.
    region_status: gtk::Label,
    region_authority: gtk::Label,
    region_staleness: gtk::Label,
    region_entry: gtk::Entry,
    region_withdraw: gtk::Button,
    /// Host-OS-maintenance status line / count badge / restart-now button
    /// (set by `set_nest_os_maintenance` from `fauna.setup.status` `os_*`).
    os_maintenance_status: gtk::Label,
    os_updates_count: gtk::Label,
    os_restart_now_button: gtk::Button,
    /// Deployment identity (`admin-nest-seed-rotate-*`) — see [`AdminHandles`]'s
    /// fields of the same name for what each holds.
    seed_rotate_confirm_box: gtk::Box,
    seed_rotate_roster_box: gtk::Box,
    seed_rotate_reason: gtk::Label,
    seed_rotate_confirm_button: gtk::Button,
    seed_rotate_status: gtk::Label,
    seed_rotate_confirm: Rc<RefCell<Option<SeedRotateConfirmState>>>,
    /// Outside-app sign-in keys (`admin-nest-oauth-*`) — see [`AdminHandles::oauth`].
    oauth: Rc<OauthSectionCtx>,
    /// Legal takedown (`admin-nest-takedown-*`) — see [`AdminHandles::takedown_status`].
    takedown_status: gtk::Label,
}

/// Build one service row: a title label, a status badge `gtk::Label` carrying
/// `status_testid`, a native `gtk::Switch` carrying `toggle_testid` (the agent
/// actuates it via `set_active`), and a dim description below. The switch's
/// active state is *reflective* (synced from the fetched flag), so the caller
/// wires the `active-notify` handler behind a guard. Returns the row plus the
/// switch + status handles.
fn build_service_row(
    title: &str,
    desc: &str,
    toggle_testid: &str,
    status_testid: &str,
) -> (gtk::Box, gtk::Switch, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.set_margin_top(8);
    row.set_margin_bottom(8);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    // Title rides its own label; the switch sits at the far right edge with the
    // status text between, mirroring the Adwaita layout.
    let title_label = gtk::Label::new(Some(title));
    title_label.set_halign(gtk::Align::Start);
    header.append(&title_label);

    let status = gtk::Label::new(None);
    status.set_halign(gtk::Align::End);
    status.set_hexpand(true);
    status.set_xalign(1.0);
    status.add_css_class("dim-label");
    crate::testid::set_test_id(&status, status_testid);
    header.append(&status);

    let toggle = gtk::Switch::builder()
        .active(false)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::End)
        .build();
    crate::testid::set_test_id(&toggle, toggle_testid);
    header.append(&toggle);

    row.append(&header);

    let desc_label = gtk::Label::new(Some(desc));
    desc_label.set_halign(gtk::Align::Start);
    desc_label.set_xalign(0.0);
    desc_label.set_wrap(true);
    desc_label.add_css_class("dim-label");
    row.append(&desc_label);

    (row, toggle, status)
}

/// Build the `admin-nest` sub-page: a heading + the admin pairing toggle
/// row + the Factory Reset danger zone + a per-page `error-message`. The
/// pairing toggle flips `fauna.admin.services.{list,update}` name "pairing"
/// via the shared `AdminClient` (`client.set_service`); it is reflective
/// (`update_services` syncs it from `AdminServicesListReply.services.pairing`),
/// so its `active-notify` handler is guarded against the programmatic
/// `set_active`. The pairing + Factory Reset strings reuse the
/// `settings_page` / `services_page` i18n namespaces (kept for the 5
/// not-yet-migrated clients); they consolidate under `nest_page` once all six
/// apps lift (en.yaml).
fn build_nest_page(client: &Rc<FaunaClient>) -> NestPage {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let heading = gtk::Label::new(Some(admin::nest_page::TITLE));
    heading.add_css_class("title-2");
    heading.set_margin_top(16);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&heading, ids::ADMIN_NEST_HEADING);
    outer.append(&heading);

    // Page subtitle (`admin.nest_page.description`) — the other 5 clients render
    // a one-line description under the heading; lift it for parity (decorative,
    // no test id).
    let description = gtk::Label::new(Some(admin::nest_page::DESCRIPTION));
    description.add_css_class("dim-label");
    description.set_halign(gtk::Align::Start);
    description.set_wrap(true);
    description.set_xalign(0.0);
    description.set_margin_start(12);
    description.set_margin_bottom(8);
    outer.append(&description);

    // ── Host-OS maintenance (installers/vps.md § Host OS Maintenance § 4) ─────
    // A passive status line read from the `fauna.setup.status` `os_*` fields:
    // "OS up to date" (default — a nest with no host channel reports the
    // serde-default zeros) / "Security updates pending" / "Restart pending — …".
    // The state→line decision is shared (`fauna_core::format::os_maintenance_status_label`,
    // single-sourced across all 7 apps), the pending-update count renders as a
    // focused integer badge (`nest-os-updates-count`, shown only when
    // `os_security_updates_pending > 0`), and a "restart now" button
    // (`nest-os-restart-now-button`, shown only when `os_reboot_pending`) drives
    // `fauna.admin.request_host_restart` to expedite the otherwise idle-gated
    // reboot. Hydrated by `set_nest_os_maintenance` on every page show (the
    // `connect_map` refetch below — os_* can change at runtime, unlike the
    // claim-time storage mode).
    let os_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    os_row.set_margin_start(12);
    os_row.set_margin_end(12);
    os_row.set_margin_bottom(8);

    let os_maintenance_status = gtk::Label::new(Some(
        &fauna_core::format::os_maintenance_status_label(0, false)
            .resolve(crate::i18n::strings::lookup),
    ));
    os_maintenance_status.add_css_class("dim-label");
    os_maintenance_status.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&os_maintenance_status, ids::NEST_OS_MAINTENANCE_STATUS);
    os_row.append(&os_maintenance_status);

    // Focused pending-updates count, split out of the categorical status line
    // (the line never carries the number — ratified in installers/vps.md § 4).
    let os_updates_count = gtk::Label::new(None);
    os_updates_count.add_css_class("dim-label");
    os_updates_count.set_halign(gtk::Align::Start);
    os_updates_count.set_visible(false);
    crate::testid::set_test_id(&os_updates_count, ids::NEST_OS_UPDATES_COUNT);
    os_row.append(&os_updates_count);

    let os_restart_now_button = gtk::Button::builder()
        .label(admin::nest_page::OS_RESTART_NOW)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::End)
        .hexpand(true)
        .build();
    os_restart_now_button.set_visible(false);
    crate::testid::set_test_id(&os_restart_now_button, ids::NEST_OS_RESTART_NOW_BUTTON);
    crate::offline_gate::declare_wire_kind(
        &os_restart_now_button,
        "fauna.admin.request_host_restart",
    );
    os_row.append(&os_restart_now_button);

    outer.append(&os_row);

    // Refetch `setup.status` whenever admin-nest is shown so the os_* indicator
    // reflects the host's current state (mirrors the `connect_map` refresh-on-show
    // in settings/{mail,logs}.rs). The login-time fetch in `AdminStatusLoaded`
    // seeds it; this keeps it live. The outside-app sign-in key set re-reads on
    // the same edge (tui's nav-edge `reflect_nest` reads it with the rest of the
    // page) — as its own result, so a failed read words its own section and
    // never blanks this one.
    {
        let client = Rc::clone(client);
        outer.connect_map(move |_| {
            client.fetch_nest_os_maintenance();
            client.fetch_oauth_issuer_keys();
        });
    }

    // Restart-now → `fauna.admin.request_host_restart` via the shared `AdminClient`
    // (no per-feature wrapper — a raw `fauna.admin.*` call, the serving-port twin).
    // The nest writes a `restart-requested` flag the host coordinator consumes; a
    // nest with no maintenance mount rejects `no_host`, surfaced on the page error.
    {
        let client = Rc::clone(client);
        os_restart_now_button.connect_clicked(move |_| client.request_host_restart());
    }

    // ── Pairing (fauna.admin.services.update name="pairing") ─────────────────
    // The admin nest-level master switch for user-initiated nest pairing
    // (per-user multi-homing). Default on; off → the nest rejects
    // `fauna.pair.add` (pair_handlers.rs). The user-facing link/unlink surface
    // is settings/linked_nests.rs (linked-nests.md); this is the admin's only
    // pairing control. (Moved off the removed Services page.)
    let (pairing_row, pairing_toggle, pairing_status) = build_service_row(
        admin::services_page::PAIRING,
        admin::services_page::PAIRING_DESC,
        "admin-service-pairing-toggle",
        "admin-service-pairing-status",
    );
    // One `services.update` row, so one kind (tui's `admin::Action::TogglePairing`).
    crate::offline_gate::declare_wire_kind(&pairing_toggle, "fauna.admin.services.update");
    let pairing_guard = Rc::new(std::cell::Cell::new(false));
    {
        let client = Rc::clone(client);
        let guard = Rc::clone(&pairing_guard);
        pairing_toggle.connect_active_notify(move |sw| {
            if guard.get() {
                return; // reflective sync from update_services, not a user click
            }
            client.set_service("pairing".into(), sw.is_active());
        });
    }
    outer.append(&pairing_row);

    // ── Serving port (fauna.admin.set_serving_port) ──────────────────────────
    // The admin-set client-facing API serving port (nest/common.md § Serving
    // ports), the symmetric twin of the CalDAV port on admin-calendar. A
    // text_input + save button: the entry holds the in-progress edit, the save
    // button validates a u16 in [1, 65535] (invalid → error-message, no dispatch)
    // then drives the raw `fauna.admin.set_serving_port` kind via the shared
    // `AdminClient` (no policy machine — a `fauna.admin.*` call). Seeded from
    // `setup.status` (`serving_port`) by `set_nest_serving_port`. Governs only the
    // router-less direct listener; inert behind the :443 SNI router.
    let serving_port_row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    serving_port_row.set_margin_start(12);
    serving_port_row.set_margin_end(12);
    serving_port_row.set_margin_top(8);
    serving_port_row.set_margin_bottom(8);

    let sp_title = gtk::Label::new(Some(admin::nest_page::SERVING_PORT_LABEL));
    sp_title.set_halign(gtk::Align::Start);
    serving_port_row.append(&sp_title);

    let sp_input_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let serving_port_entry = gtk::Entry::builder()
        .valign(gtk::Align::Center)
        .width_chars(12)
        .hexpand(true)
        .build();
    crate::testid::set_test_id(&serving_port_entry, ids::ADMIN_NEST_SERVING_PORT_INPUT);
    sp_input_row.append(&serving_port_entry);

    let serving_port_save = gtk::Button::builder()
        .label(admin::nest_page::SERVING_PORT_SAVE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    crate::testid::set_test_id(&serving_port_save, ids::ADMIN_NEST_SERVING_PORT_SAVE_BUTTON);
    // The entry beside it issues nothing; only the save does — the same split
    // tui declares (`admin::Action::SaveServingPort`), so typing stays possible
    // offline and only the commit is withheld.
    crate::offline_gate::declare_wire_kind(&serving_port_save, "fauna.admin.set_serving_port");
    sp_input_row.append(&serving_port_save);
    serving_port_row.append(&sp_input_row);

    let sp_desc = gtk::Label::new(Some(admin::nest_page::SERVING_PORT_DESC));
    sp_desc.set_halign(gtk::Align::Start);
    sp_desc.set_xalign(0.0);
    sp_desc.set_wrap(true);
    sp_desc.add_css_class("dim-label");
    serving_port_row.append(&sp_desc);

    // Read-only hint shown only when the nest sits behind the cloud :443 SNI
    // router (`fauna.setup.status` `fronted_by_router`): the entry + save button
    // are desensitized and this replaces the editable affordance with "served on
    // 443 by this deployment". Hidden on a direct listener (the default), where
    // the port is a genuine admin choice. nest/common.md § Serving ports.
    let serving_port_hint = gtk::Label::new(Some(admin::nest_page::SERVING_PORT_FRONTED_HINT));
    serving_port_hint.set_halign(gtk::Align::Start);
    serving_port_hint.set_xalign(0.0);
    serving_port_hint.set_wrap(true);
    serving_port_hint.add_css_class("dim-label");
    serving_port_hint.set_visible(false);
    serving_port_row.append(&serving_port_hint);

    outer.append(&serving_port_row);

    // ── NAT mode (fauna.setup.nat_mode via the shared AdminNatModeMachine) ───
    // The post-onboarding change surface for the axis the wizard's
    // nat_mode_choice confirmed once at claim (admin.md § Nest → NAT-mode
    // control). Radios + save + status off the shared machine
    // (`fauna_onboarding_machine::admin_nat_mode` — the same seam and commit
    // ceremony as the wizard page); labels reuse the onboarding.nat_mode
    // strings so both surfaces read identically. No defer button — navigating
    // away is the defer. Save stays enabled after success (mutable upsert; an
    // immediate re-flip is allowed).
    let nat_row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    nat_row.set_margin_start(12);
    nat_row.set_margin_end(12);
    nat_row.set_margin_top(8);
    nat_row.set_margin_bottom(8);

    let nat_title = gtk::Label::new(Some(admin::nest_page::NAT_MODE_LABEL));
    nat_title.set_halign(gtk::Align::Start);
    nat_row.append(&nat_title);

    use crate::i18n::strings::onboarding::nat_mode as nat_strings;
    let nat_public_radio = gtk::CheckButton::with_label(nat_strings::PUBLIC_LABEL);
    nat_public_radio.set_margin_top(4);
    crate::testid::set_test_id(&nat_public_radio, ids::ADMIN_NEST_NAT_MODE_PUBLIC_RADIO);
    nat_row.append(&nat_public_radio);

    let nat_public_desc = gtk::Label::new(Some(nat_strings::PUBLIC_DESC));
    nat_public_desc.set_halign(gtk::Align::Start);
    nat_public_desc.set_xalign(0.0);
    nat_public_desc.set_wrap(true);
    nat_public_desc.add_css_class("dim-label");
    nat_row.append(&nat_public_desc);

    let nat_private_radio = gtk::CheckButton::with_label(nat_strings::PRIVATE_LABEL);
    nat_private_radio.set_group(Some(&nat_public_radio));
    nat_private_radio.set_margin_top(4);
    crate::testid::set_test_id(&nat_private_radio, ids::ADMIN_NEST_NAT_MODE_PRIVATE_RADIO);
    nat_row.append(&nat_private_radio);

    let nat_private_desc = gtk::Label::new(Some(nat_strings::PRIVATE_DESC));
    nat_private_desc.set_halign(gtk::Align::Start);
    nat_private_desc.set_xalign(0.0);
    nat_private_desc.set_wrap(true);
    nat_private_desc.add_css_class("dim-label");
    nat_row.append(&nat_private_desc);

    let nat_action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    nat_action_row.set_margin_top(4);

    // Status line: renders the machine snapshot's message — the idle/saved
    // texts carry the live-vs-restart-applied caveat.
    let nat_status = gtk::Label::new(None);
    nat_status.set_halign(gtk::Align::Start);
    nat_status.set_xalign(0.0);
    nat_status.set_wrap(true);
    nat_status.set_hexpand(true);
    nat_status.add_css_class("dim-label");
    crate::testid::set_test_id(&nat_status, ids::ADMIN_NEST_NAT_MODE_STATUS);
    nat_action_row.append(&nat_status);

    let nat_save = gtk::Button::builder()
        .label(admin::nest_page::NAT_MODE_SAVE)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::End)
        .css_classes(["suggested-action"])
        .build();
    crate::testid::set_test_id(&nat_save, ids::ADMIN_NEST_NAT_MODE_SAVE_BUTTON);
    // The two radios are a local selection; the save is what signs and commits
    // it (tui's `admin::Action::SaveNatMode`).
    crate::offline_gate::declare_wire_kind(&nat_save, "fauna.setup.nat_mode");
    nat_action_row.append(&nat_save);
    nat_row.append(&nat_action_row);

    outer.append(&nat_row);

    // Wiring: dispatch-style — await an action on the worker runtime, then
    // re-render from the snapshot (the nat_mode_choice.rs idiom).
    {
        let machine = client.admin_nat_mode_machine();
        let nat_updating = Rc::new(std::cell::Cell::new(false));

        let nat_refresh: Rc<dyn Fn()> = Rc::new({
            let machine = machine.clone();
            let public_radio = nat_public_radio.clone();
            let private_radio = nat_private_radio.clone();
            let save = nat_save.clone();
            let status = nat_status.clone();
            let updating = Rc::clone(&nat_updating);
            move || {
                use fauna_onboarding_machine::{NatModeState, NodeMode};
                let snap = machine.snapshot();
                updating.set(true);
                match snap.selected_mode {
                    NodeMode::Public if !public_radio.is_active() => {
                        public_radio.set_active(true);
                    }
                    NodeMode::Private if !private_radio.is_active() => {
                        private_radio.set_active(true);
                    }
                    _ => {}
                }
                updating.set(false);
                save.set_sensitive(snap.submit_enabled);
                let inflight = snap.state == NatModeState::Submitting;
                public_radio.set_sensitive(!inflight);
                private_radio.set_sensitive(!inflight);
                status.set_text(&snap.message.resolve(crate::i18n::strings::lookup));
            }
        });

        // Radio toggles: guarded against the programmatic set_active above.
        {
            let machine = machine.clone();
            let updating = Rc::clone(&nat_updating);
            let refresh = Rc::clone(&nat_refresh);
            nat_public_radio.connect_toggled(move |r| {
                if updating.get() || !r.is_active() {
                    return;
                }
                machine.select(fauna_onboarding_machine::NodeMode::Public);
                refresh();
            });
        }
        {
            let machine = machine.clone();
            let updating = Rc::clone(&nat_updating);
            let refresh = Rc::clone(&nat_refresh);
            nat_private_radio.connect_toggled(move |r| {
                if updating.get() || !r.is_active() {
                    return;
                }
                machine.select(fauna_onboarding_machine::NodeMode::Private);
                refresh();
            });
        }

        // Save → sign + commit the mutable fauna.setup.nat_mode.
        {
            let machine = machine.clone();
            let refresh = Rc::clone(&nat_refresh);
            nat_save.connect_clicked(move |_| {
                let m = machine.clone();
                let refresh = Rc::clone(&refresh);
                crate::async_helper::run_on_tokio(async move { m.submit().await }, move |()| {
                    refresh()
                });
            });
        }

        // Hydrate on every page show: pre-select the current node_mode from
        // fauna.setup.status (the mode can change from another client).
        {
            let refresh = Rc::clone(&nat_refresh);
            outer.connect_map(move |_| {
                let m = machine.clone();
                let refresh = Rc::clone(&refresh);
                crate::async_helper::run_on_tokio(async move { m.hydrate().await }, move |()| {
                    refresh()
                });
            });
        }

        nat_refresh();
    }

    // ── Declared region (fauna.admin.region.{get,set}) ───────────────────────
    // The deployment's legal situs — the region tier's one human choice
    // (region-blocking.md § Region determination; dynamic-features.md § The
    // region tier). Every rendering decision below is the shared
    // `fauna_client_admin::admin_region_view` fold (tui's `admin/nest.rs`, the
    // reference leg) — this file paints exactly what it hands back and wires
    // two buttons; it decides nothing about the plane (priority #2 — six apps
    // lift the same fold, and what an admin is told about a tier that can BIND
    // accounts here must not differ per app).
    // ⚠ DECLARED, NEVER DETECTED (ratified 2026-08-11): no detect/prefill
    // affordance may be added here — `parse_region_code` deliberately does not
    // even case-fold.
    let region_section = build_section("admin-nest-region-section", admin::nest_page::REGION_LABEL);

    let region_desc = gtk::Label::new(Some(admin::nest_page::REGION_DESC));
    region_desc.set_halign(gtk::Align::Start);
    region_desc.set_xalign(0.0);
    region_desc.set_wrap(true);
    region_desc.add_css_class("dim-label");
    region_desc.set_margin_start(12);
    region_desc.set_margin_end(12);
    region_section.append(&region_desc);

    // `admin-nest-region-status` — the declared region, or that none is
    // declared. A NORMAL state, never an error: a deployment that has never
    // declared is conforming (seeded by `set_nest_region` from the fold).
    let region_status = gtk::Label::new(Some(admin::nest_page::REGION_NONE));
    region_status.set_halign(gtk::Align::Start);
    region_status.set_xalign(0.0);
    region_status.set_wrap(true);
    region_status.set_margin_start(12);
    region_status.set_margin_top(4);
    crate::testid::set_test_id(&region_status, ids::ADMIN_NEST_REGION_STATUS);
    region_section.append(&region_status);

    // `admin-nest-region-authority` — present only while a region is
    // declared: before that there is no authority channel to describe, and
    // inventing a line about one would be a claim.
    let region_authority = gtk::Label::new(None);
    region_authority.set_halign(gtk::Align::Start);
    region_authority.set_xalign(0.0);
    region_authority.set_wrap(true);
    region_authority.add_css_class("dim-label");
    region_authority.set_margin_start(12);
    region_authority.set_visible(false);
    crate::testid::set_test_id(&region_authority, ids::ADMIN_NEST_REGION_AUTHORITY);
    region_section.append(&region_authority);

    // `admin-nest-region-staleness` — the nest-reported "act when you can"
    // warning, only when the channel is unreached. The rules already
    // received stay in force, so this is a caveat, never an outage.
    let region_staleness = gtk::Label::new(None);
    region_staleness.set_halign(gtk::Align::Start);
    region_staleness.set_xalign(0.0);
    region_staleness.set_wrap(true);
    region_staleness.add_css_class("dim-label");
    region_staleness.set_margin_start(12);
    region_staleness.set_visible(false);
    crate::testid::set_test_id(&region_staleness, ids::ADMIN_NEST_REGION_STALENESS);
    region_section.append(&region_staleness);

    let region_input_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    region_input_row.set_margin_start(12);
    region_input_row.set_margin_end(12);
    region_input_row.set_margin_top(8);

    let region_entry = gtk::Entry::builder()
        .valign(gtk::Align::Center)
        .width_chars(12)
        .hexpand(true)
        .build();
    region_entry.set_placeholder_text(Some(admin::nest_page::REGION_PLACEHOLDER));
    crate::testid::set_test_id(&region_entry, ids::ADMIN_NEST_REGION_INPUT);
    region_input_row.append(&region_entry);

    let region_save = gtk::Button::builder()
        .label(admin::nest_page::REGION_SAVE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    crate::testid::set_test_id(&region_save, ids::ADMIN_NEST_REGION_SAVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&region_save, "fauna.admin.region.set");
    region_input_row.append(&region_save);
    region_section.append(&region_input_row);

    // `admin-nest-region-withdraw-button` — shown only while `can_withdraw`
    // (a region is declared). Withdrawing also retires the previous region's
    // feature-policy document nest-side (a document is one authority's
    // statement about deployments in its own region and must not survive a
    // change of situs).
    let region_withdraw = gtk::Button::builder()
        .label(admin::nest_page::REGION_WITHDRAW)
        .halign(gtk::Align::Start)
        .margin_start(12)
        .margin_top(4)
        .build();
    region_withdraw.set_visible(false);
    crate::testid::set_test_id(&region_withdraw, ids::ADMIN_NEST_REGION_WITHDRAW_BUTTON);
    crate::offline_gate::declare_wire_kind(&region_withdraw, "fauna.admin.region.set");
    region_section.append(&region_withdraw);

    outer.append(&region_section);

    // ── Deployment identity — the rotation ceremony (`box-recovery.md` §
    // Deployment-seed rotation), driven by the shared
    // plane drive (`deployment_seeds::rotate_deployment_seed`). Same inline-confirm shape as the
    // Factory reset below, with one thing the reset does not have: the
    // confirm's whole job is to name **the set that will inherit** before
    // dispatch (the doc's ordering rule), so the roster listing is not
    // decoration — it is the decision, and the confirm stays disabled until it
    // can be shown. Mirrors tui's `admin/nest.rs` (the reference leg).
    let seed_rotate_section = build_section(
        "admin-nest-seed-rotate-section",
        admin::nest_page::ROTATE_SEED_LABEL,
    );
    let seed_rotate_desc = gtk::Label::new(Some(admin::nest_page::ROTATE_SEED_DESC));
    seed_rotate_desc.set_halign(gtk::Align::Start);
    seed_rotate_desc.set_xalign(0.0);
    seed_rotate_desc.set_wrap(true);
    seed_rotate_desc.add_css_class("dim-label");
    seed_rotate_desc.set_margin_start(12);
    seed_rotate_desc.set_margin_end(12);
    seed_rotate_section.append(&seed_rotate_desc);

    let seed_rotate_button = gtk::Button::builder()
        .label(admin::nest_page::ROTATE_SEED_BUTTON)
        .halign(gtk::Align::Start)
        .margin_start(12)
        .margin_top(4)
        .build();
    crate::testid::set_test_id(&seed_rotate_button, ids::ADMIN_NEST_SEED_ROTATE_BUTTON);
    // The click only arms the confirm, but the ceremony behind it can end in
    // exactly one kind, so the ENTRY declares (the same reasoning as the
    // Factory reset button below).
    crate::offline_gate::declare_wire_kind(&seed_rotate_button, "fauna.admin.admins.list");
    seed_rotate_section.append(&seed_rotate_button);

    let seed_rotate_confirm_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    seed_rotate_confirm_box.set_margin_start(12);
    seed_rotate_confirm_box.set_margin_end(12);
    seed_rotate_confirm_box.set_margin_top(4);
    seed_rotate_confirm_box.set_visible(false);

    let seed_rotate_confirm_body =
        gtk::Label::new(Some(admin::nest_page::ROTATE_SEED_CONFIRM_BODY));
    seed_rotate_confirm_body.set_halign(gtk::Align::Start);
    seed_rotate_confirm_body.set_xalign(0.0);
    seed_rotate_confirm_body.set_wrap(true);
    seed_rotate_confirm_box.append(&seed_rotate_confirm_body);

    // Roster-row container — one label per inheritor, painted only from a
    // resolved roster. `Loading` and `Failed` deliberately paint NO rows: an
    // empty list beside a live confirm would read as "nobody inherits", the
    // one wrong thing this surface must never say.
    let seed_rotate_roster_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    seed_rotate_confirm_box.append(&seed_rotate_roster_box);

    let seed_rotate_reason = gtk::Label::new(None);
    seed_rotate_reason.set_halign(gtk::Align::Start);
    seed_rotate_reason.set_xalign(0.0);
    seed_rotate_reason.set_wrap(true);
    seed_rotate_reason.add_css_class("dim-label");
    seed_rotate_reason.set_visible(false);
    crate::testid::set_test_id(
        &seed_rotate_reason,
        ids::ADMIN_NEST_SEED_ROTATE_ROSTER_REASON,
    );
    seed_rotate_confirm_box.append(&seed_rotate_reason);

    let seed_rotate_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let seed_rotate_confirm_button = gtk::Button::builder()
        .label(admin::nest_page::ROTATE_SEED_CONFIRM_BUTTON)
        .css_classes(["destructive-action"])
        .sensitive(false)
        .build();
    crate::testid::set_test_id(
        &seed_rotate_confirm_button,
        ids::ADMIN_NEST_SEED_ROTATE_CONFIRM_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(
        &seed_rotate_confirm_button,
        "fauna.admin.deployment_seed.rotate",
    );
    seed_rotate_buttons.append(&seed_rotate_confirm_button);

    let seed_rotate_cancel_button = gtk::Button::builder()
        .label(admin::nest_page::ROTATE_SEED_CANCEL_BUTTON)
        .build();
    crate::testid::set_test_id(
        &seed_rotate_cancel_button,
        ids::ADMIN_NEST_SEED_ROTATE_CANCEL_BUTTON,
    );
    seed_rotate_buttons.append(&seed_rotate_cancel_button);
    seed_rotate_confirm_box.append(&seed_rotate_buttons);

    seed_rotate_section.append(&seed_rotate_confirm_box);

    let seed_rotate_status = gtk::Label::new(None);
    seed_rotate_status.set_halign(gtk::Align::Start);
    seed_rotate_status.set_xalign(0.0);
    seed_rotate_status.set_wrap(true);
    seed_rotate_status.set_margin_start(12);
    seed_rotate_status.set_visible(false);
    crate::testid::set_test_id(&seed_rotate_status, ids::ADMIN_NEST_SEED_ROTATE_STATUS);
    seed_rotate_section.append(&seed_rotate_status);

    outer.append(&seed_rotate_section);

    // The armed confirm's state — `None` while un-armed. See `AdminHandles`'s
    // field of the same name for the disarm-before-dispatch discipline.
    let seed_rotate_confirm: Rc<RefCell<Option<SeedRotateConfirmState>>> =
        Rc::new(RefCell::new(None));

    {
        let client = Rc::clone(client);
        let state = Rc::clone(&seed_rotate_confirm);
        let confirm_box = seed_rotate_confirm_box.clone();
        let roster_box = seed_rotate_roster_box.clone();
        let reason = seed_rotate_reason.clone();
        let confirm_button = seed_rotate_confirm_button.clone();
        let status = seed_rotate_status.clone();
        seed_rotate_button.connect_clicked(move |_| {
            // Arm into `Loading` FIRST, so the confirm exists (disabled) from
            // the same frame the button was pressed — a confirm that
            // appeared only once the network answered would read as a dead
            // button under load.
            *state.borrow_mut() = Some(SeedRotateConfirmState::Loading);
            status.set_visible(false);
            render_seed_rotate(
                &state.borrow(),
                &confirm_box,
                &roster_box,
                &reason,
                &confirm_button,
            );
            client.load_seed_rotate_roster();
        });
    }
    {
        let state = Rc::clone(&seed_rotate_confirm);
        let confirm_box = seed_rotate_confirm_box.clone();
        let roster_box = seed_rotate_roster_box.clone();
        let reason = seed_rotate_reason.clone();
        let confirm_button = seed_rotate_confirm_button.clone();
        seed_rotate_cancel_button.connect_clicked(move |_| {
            *state.borrow_mut() = None;
            render_seed_rotate(
                &state.borrow(),
                &confirm_box,
                &roster_box,
                &reason,
                &confirm_button,
            );
        });
    }
    {
        let client = Rc::clone(client);
        let state = Rc::clone(&seed_rotate_confirm);
        let confirm_box = seed_rotate_confirm_box.clone();
        let roster_box = seed_rotate_roster_box.clone();
        let reason = seed_rotate_reason.clone();
        let confirm_button = seed_rotate_confirm_button.clone();
        let status = seed_rotate_status.clone();
        seed_rotate_confirm_button.connect_clicked(move |_| {
            // Disarm FIRST (the bridges-rotate discipline): a double click
            // must not dispatch a second ceremony, which would chain a
            // *second* rotation onto the first and strand a successor nobody
            // marked.
            let armed = state.borrow_mut().take();
            let Some(SeedRotateConfirmState::Ready(view)) = armed else {
                // Re-arm: the roster never answered (or this fired while
                // Loading/Failed — the button is disabled then, but a
                // just-landed disable can race a queued click), so there is
                // nothing to confirm against and the admin keeps the surface
                // they had.
                *state.borrow_mut() = armed;
                return;
            };
            if !view.can_confirm {
                *state.borrow_mut() = Some(SeedRotateConfirmState::Ready(view));
                return;
            }
            render_seed_rotate(
                &state.borrow(),
                &confirm_box,
                &roster_box,
                &reason,
                &confirm_button,
            );
            status.set_text(admin::nest_page::ROTATE_SEED_WORKING);
            status.set_visible(true);
            client.rotate_deployment_seed();
        });
    }

    // ── Outside-app sign-in keys (`fauna.oauth.*`; authorization-server.md §
    // The issuer → *Two rotation arms*) — the nest-held OAuth issuer key set
    // and its second signer, the refresh-token secret. Directly after the
    // deployment identity: the same class of deployment crypto, no nav entry
    // (admin.md § N Nest → Outside-app sign-in keys). Paint only — every
    // sentence is a shared `fauna_client_admin` fold, and the four gestures'
    // guards live in the GTK-free `OauthSectionState`. Mirrors tui's
    // `admin/nest.rs` `oauth_elements` (the reference leg).
    let (oauth_section, oauth_widgets) = build_oauth_section();
    outer.append(&oauth_section);
    let oauth = Rc::new(OauthSectionCtx {
        state: RefCell::new(OauthSectionState::default()),
        widgets: oauth_widgets,
    });
    render_oauth(&oauth);
    wire_oauth_section(&oauth, client);

    // ── Legal takedown (`fauna.moderation.legal_takedown`; moderation.md §
    // Legal takedown → Invocation surface, ruled 2026-08-16). Every gating/wording decision is the shared
    // `fauna_client_moderation::takedown` fold (`takedown_form_view` /
    // `takedown_verdict`) — this section paints and wires; nothing here
    // decides. Same inline arm→confirm→dispatch shape as the deployment-
    // identity rotation above, but simpler: the confirm has no async
    // intermediate — `takedown_form_view` is pure, so the arm control's
    // sensitivity + reason recompute synchronously on every keystroke/toggle,
    // never behind a "Loading" state. Mirrors tui's `admin/nest.rs` (the
    // reference leg).
    let takedown_section = build_section(
        "admin-nest-takedown-section",
        admin::nest_page::TAKEDOWN_LABEL,
    );
    let takedown_desc = gtk::Label::new(Some(admin::nest_page::TAKEDOWN_DESC));
    takedown_desc.set_halign(gtk::Align::Start);
    takedown_desc.set_xalign(0.0);
    takedown_desc.set_wrap(true);
    takedown_desc.add_css_class("dim-label");
    takedown_desc.set_margin_start(12);
    takedown_desc.set_margin_end(12);
    takedown_section.append(&takedown_desc);

    let takedown_content_id_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    takedown_content_id_row.set_margin_start(12);
    takedown_content_id_row.set_margin_end(12);
    takedown_content_id_row.set_margin_top(8);
    let takedown_content_id_label =
        gtk::Label::new(Some(admin::nest_page::TAKEDOWN_CONTENT_ID_LABEL));
    takedown_content_id_row.append(&takedown_content_id_label);
    let takedown_content_id_entry = gtk::Entry::builder()
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    crate::testid::set_test_id(
        &takedown_content_id_entry,
        ids::ADMIN_NEST_TAKEDOWN_CONTENT_ID_INPUT,
    );
    takedown_content_id_row.append(&takedown_content_id_entry);
    takedown_section.append(&takedown_content_id_row);

    let takedown_type_post_radio =
        gtk::CheckButton::with_label(admin::nest_page::TAKEDOWN_TYPE_POST);
    takedown_type_post_radio.set_active(true);
    takedown_type_post_radio.set_margin_start(12);
    takedown_type_post_radio.set_margin_top(4);
    crate::testid::set_test_id(
        &takedown_type_post_radio,
        ids::ADMIN_NEST_TAKEDOWN_TYPE_POST_RADIO,
    );
    takedown_section.append(&takedown_type_post_radio);

    let takedown_type_conversation_radio =
        gtk::CheckButton::with_label(admin::nest_page::TAKEDOWN_TYPE_CONVERSATION);
    takedown_type_conversation_radio.set_group(Some(&takedown_type_post_radio));
    takedown_type_conversation_radio.set_margin_start(12);
    crate::testid::set_test_id(
        &takedown_type_conversation_radio,
        ids::ADMIN_NEST_TAKEDOWN_TYPE_CONVERSATION_RADIO,
    );
    takedown_section.append(&takedown_type_conversation_radio);

    let takedown_reference_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    takedown_reference_row.set_margin_start(12);
    takedown_reference_row.set_margin_end(12);
    takedown_reference_row.set_margin_top(8);
    let takedown_reference_label =
        gtk::Label::new(Some(admin::nest_page::TAKEDOWN_REFERENCE_LABEL));
    takedown_reference_row.append(&takedown_reference_label);
    let takedown_reference_entry = gtk::Entry::builder()
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    crate::testid::set_test_id(
        &takedown_reference_entry,
        ids::ADMIN_NEST_TAKEDOWN_REFERENCE_INPUT,
    );
    takedown_reference_row.append(&takedown_reference_entry);
    takedown_section.append(&takedown_reference_row);

    let takedown_restore_checkbox =
        gtk::CheckButton::with_label(admin::nest_page::TAKEDOWN_RESTORE_LABEL);
    takedown_restore_checkbox.set_margin_start(12);
    takedown_restore_checkbox.set_margin_top(4);
    crate::testid::set_test_id(
        &takedown_restore_checkbox,
        ids::ADMIN_NEST_TAKEDOWN_RESTORE_CHECKBOX,
    );
    takedown_section.append(&takedown_restore_checkbox);

    // The disabled arm control owes its reason (walk I5's spirit) — decorative,
    // no test id (ui.yaml declares none for it; tui's twin is unregistered
    // `Element::chrome`).
    let takedown_reason = gtk::Label::new(None);
    takedown_reason.set_halign(gtk::Align::Start);
    takedown_reason.set_xalign(0.0);
    takedown_reason.set_wrap(true);
    takedown_reason.add_css_class("dim-label");
    takedown_reason.set_margin_start(12);
    takedown_reason.set_margin_top(4);
    takedown_reason.set_visible(false);
    takedown_section.append(&takedown_reason);

    let takedown_button = gtk::Button::builder()
        .label(admin::nest_page::TAKEDOWN_ARM_TAKEDOWN)
        .halign(gtk::Align::Start)
        .margin_start(12)
        .margin_top(8)
        .css_classes(["destructive-action"])
        .sensitive(false)
        .build();
    crate::testid::set_test_id(&takedown_button, ids::ADMIN_NEST_TAKEDOWN_BUTTON);
    crate::offline_gate::declare_wire_kind(&takedown_button, "fauna.moderation.legal_takedown");
    takedown_section.append(&takedown_button);

    let takedown_confirm_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    takedown_confirm_box.set_margin_start(12);
    takedown_confirm_box.set_margin_end(12);
    takedown_confirm_box.set_margin_top(4);
    takedown_confirm_box.set_visible(false);

    let takedown_confirm_summary = gtk::Label::new(None);
    takedown_confirm_summary.set_halign(gtk::Align::Start);
    takedown_confirm_summary.set_xalign(0.0);
    takedown_confirm_summary.set_wrap(true);
    crate::testid::set_test_id(
        &takedown_confirm_summary,
        ids::ADMIN_NEST_TAKEDOWN_CONFIRM_SUMMARY,
    );
    takedown_confirm_box.append(&takedown_confirm_summary);

    let takedown_confirm_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let takedown_confirm_button = gtk::Button::builder()
        .label(admin::nest_page::TAKEDOWN_CONFIRM_BUTTON_TAKEDOWN)
        .css_classes(["destructive-action"])
        .build();
    crate::testid::set_test_id(
        &takedown_confirm_button,
        ids::ADMIN_NEST_TAKEDOWN_CONFIRM_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(
        &takedown_confirm_button,
        "fauna.moderation.legal_takedown",
    );
    takedown_confirm_buttons.append(&takedown_confirm_button);

    let takedown_cancel_button = gtk::Button::builder()
        .label(admin::nest_page::TAKEDOWN_CANCEL_BUTTON)
        .build();
    crate::testid::set_test_id(
        &takedown_cancel_button,
        ids::ADMIN_NEST_TAKEDOWN_CANCEL_BUTTON,
    );
    takedown_confirm_buttons.append(&takedown_cancel_button);
    takedown_confirm_box.append(&takedown_confirm_buttons);

    takedown_section.append(&takedown_confirm_box);

    let takedown_status = gtk::Label::new(None);
    takedown_status.set_halign(gtk::Align::Start);
    takedown_status.set_xalign(0.0);
    takedown_status.set_wrap(true);
    takedown_status.set_margin_start(12);
    takedown_status.set_visible(false);
    crate::testid::set_test_id(&takedown_status, ids::ADMIN_NEST_TAKEDOWN_STATUS);
    takedown_section.append(&takedown_status);

    outer.append(&takedown_section);

    // The armed confirm's captured form — `None` while un-armed. Mirrors
    // tui's `AdminState::takedown_confirm`: captured at arm time and never
    // re-derived while armed, so a field the admin keeps typing after arming
    // cannot silently change what the confirm named. Consumed (`take()`) by
    // the confirm click before dispatch — disarm-before-dispatch, so a double
    // click cannot dispatch a second compulsory act.
    let takedown_armed: Rc<RefCell<Option<fauna_client_moderation::TakedownForm>>> =
        Rc::new(RefCell::new(None));

    // Recompute the arm control's label/sensitivity + reason from the shared
    // fold — called on every field change, never a per-app guess (decisions
    // 1/2 in `takedown.rs`'s module doc: a citation-less takedown is never
    // armable, but a note-less restore is).
    refresh_takedown_arm(
        &takedown_content_id_entry,
        &takedown_type_conversation_radio,
        &takedown_reference_entry,
        &takedown_restore_checkbox,
        &takedown_button,
        &takedown_reason,
    ); // the initial empty-form state (button disabled, no reason shown yet)
    for widget in [&takedown_content_id_entry, &takedown_reference_entry] {
        let content_id = takedown_content_id_entry.clone();
        let conversation_radio = takedown_type_conversation_radio.clone();
        let reference = takedown_reference_entry.clone();
        let restore = takedown_restore_checkbox.clone();
        let button = takedown_button.clone();
        let reason = takedown_reason.clone();
        widget.connect_changed(move |_| {
            refresh_takedown_arm(
                &content_id,
                &conversation_radio,
                &reference,
                &restore,
                &button,
                &reason,
            );
        });
    }
    for widget in [
        &takedown_type_post_radio,
        &takedown_type_conversation_radio,
        &takedown_restore_checkbox,
    ] {
        let content_id = takedown_content_id_entry.clone();
        let conversation_radio = takedown_type_conversation_radio.clone();
        let reference = takedown_reference_entry.clone();
        let restore = takedown_restore_checkbox.clone();
        let button = takedown_button.clone();
        let reason = takedown_reason.clone();
        widget.connect_toggled(move |_| {
            refresh_takedown_arm(
                &content_id,
                &conversation_radio,
                &reference,
                &restore,
                &button,
                &reason,
            );
        });
    }

    // Arm — capture the form + fold now, so the confirm names exactly what a
    // later dispatch will send even if the admin keeps typing.
    {
        let content_id = takedown_content_id_entry.clone();
        let conversation_radio = takedown_type_conversation_radio.clone();
        let reference = takedown_reference_entry.clone();
        let restore = takedown_restore_checkbox.clone();
        let armed = Rc::clone(&takedown_armed);
        let confirm_box = takedown_confirm_box.clone();
        let confirm_summary = takedown_confirm_summary.clone();
        let confirm_button = takedown_confirm_button.clone();
        let status = takedown_status.clone();
        takedown_button.connect_clicked(move |_| {
            let form =
                current_takedown_form(&content_id, &conversation_radio, &reference, &restore);
            let view = fauna_client_moderation::takedown_form_view(&form);
            if !view.can_submit {
                // The button renders disabled with the fold's stated reason; a
                // driver-forced press must not arm what the guard refuses.
                return;
            }
            status.set_visible(false);
            confirm_summary.set_text(&view.confirm_summary.resolve(crate::i18n::strings::lookup));
            confirm_button.set_label(&view.confirm_label.resolve(crate::i18n::strings::lookup));
            *armed.borrow_mut() = Some(form);
            confirm_box.set_visible(true);
        });
    }

    // Cancel — disarms, touching nothing.
    {
        let armed = Rc::clone(&takedown_armed);
        let confirm_box = takedown_confirm_box.clone();
        takedown_cancel_button.connect_clicked(move |_| {
            *armed.borrow_mut() = None;
            confirm_box.set_visible(false);
        });
    }

    // Confirm — disarm FIRST (the seed-rotate discipline): a double click
    // must not dispatch a second compulsory act.
    {
        let client = Rc::clone(client);
        let armed = Rc::clone(&takedown_armed);
        let confirm_box = takedown_confirm_box.clone();
        let status = takedown_status.clone();
        takedown_confirm_button.connect_clicked(move |_| {
            let Some(form) = armed.borrow_mut().take() else {
                return;
            };
            confirm_box.set_visible(false);
            status.set_text(admin::nest_page::TAKEDOWN_WORKING);
            status.set_visible(true);
            client.submit_takedown(form);
        });
    }

    // ── Danger zone — Factory reset (`fauna.admin.factory_reset`) ────────────
    // Moved off Settings (admin.md § Admin IA redesign): a nest-wide concern.
    outer.append(&build_factory_reset_section());

    // Per-page `error-message` (rule #2): the page-level error surface for a
    // failed `services.{list,update}` on the pairing toggle path, the
    // client-side invalid-serving-port message, and the client-side
    // invalid-region message. Built here so the serving-port + region save
    // handlers can target it; appended last to sit at the page tail.
    let error_label = build_error_label();

    // Serving-port save → validate via fauna_core::format::parse_port (a u16 in
    // [1, 65535], invalid → error-message, no dispatch), else `set_serving_port`
    // (mirrors the admin_calendar CalDAV-port save).
    {
        let client = Rc::clone(client);
        let entry = serving_port_entry.clone();
        let err = error_label.clone();
        serving_port_save.connect_clicked(move |_| {
            match fauna_core::format::parse_port(&entry.text()) {
                Some(port) => {
                    err.set_visible(false);
                    client.set_serving_port(port);
                }
                None => {
                    err.set_text(admin::nest_page::SERVING_PORT_INVALID);
                    err.set_visible(true);
                }
            }
        });
    }

    // Region save → validate via the shared `fauna_client_admin::parse_region_code`
    // (declared, never detected — no case-fold; region-blocking.md), invalid →
    // error-message, no dispatch — the serving-port shape. Mirrors tui's
    // `Action::SaveRegion`.
    {
        let client = Rc::clone(client);
        let entry = region_entry.clone();
        let err = error_label.clone();
        region_save.connect_clicked(move |_| {
            match fauna_client_admin::parse_region_code(&entry.text()) {
                Ok(region) => {
                    err.set_visible(false);
                    client.set_region(Some(region));
                }
                Err(_) => {
                    err.set_text(admin::nest_page::REGION_INVALID);
                    err.set_visible(true);
                }
            }
        });
    }

    // Region withdraw → `fauna.admin.region.set` with the region absent;
    // nothing to validate. Mirrors tui's `Action::WithdrawRegion`.
    {
        let client = Rc::clone(client);
        region_withdraw.connect_clicked(move |_| {
            client.set_region(None);
        });
    }

    outer.append(&error_label);

    NestPage {
        page: outer,
        pairing_toggle,
        pairing_status,
        pairing_guard,
        serving_port_entry,
        serving_port_save,
        serving_port_hint,
        region_status,
        region_authority,
        region_staleness,
        region_entry,
        region_withdraw,
        os_maintenance_status,
        os_updates_count,
        os_restart_now_button,
        seed_rotate_confirm_box,
        seed_rotate_roster_box,
        seed_rotate_reason,
        seed_rotate_confirm_button,
        seed_rotate_status,
        seed_rotate_confirm,
        oauth,
        takedown_status,
    }
}

/// Re-render the deployment-identity rotation confirm surface from `state`
/// (mirrors tui's `nest_elements` seed-rotate block, `apps/fauna-tui/src/admin/nest.rs`
/// — the reference leg). Called both synchronously from the arm/cancel/confirm
/// click handlers (`build_nest_page`, same-frame paint) and from the
/// `DataMessage::SeedRotateRosterLoaded` pump handler once the roster read
/// resolves.
fn render_seed_rotate(
    state: &Option<SeedRotateConfirmState>,
    confirm_box: &gtk::Box,
    roster_box: &gtk::Box,
    reason: &gtk::Label,
    confirm_button: &gtk::Button,
) {
    clear_box(roster_box);
    let Some(armed) = state else {
        confirm_box.set_visible(false);
        return;
    };
    confirm_box.set_visible(true);
    let (rows, reason_text, enabled): (
        &[fauna_client_admin::SeedRotationInheritor],
        Option<String>,
        bool,
    ) = match armed {
        SeedRotateConfirmState::Loading => (
            &[],
            Some(admin::nest_page::ROTATE_SEED_ROSTER_LOADING.to_string()),
            false,
        ),
        SeedRotateConfirmState::Failed(message) => (&[], Some(message.clone()), false),
        SeedRotateConfirmState::Ready(view) => (
            view.inheritors.as_slice(),
            view.blocked_reason
                .as_ref()
                .map(|r| r.clone().resolve(crate::i18n::strings::lookup)),
            view.can_confirm,
        ),
    };
    for (i, inheritor) in rows.iter().enumerate() {
        let label = gtk::Label::new(Some(&inheritor.label));
        label.set_halign(gtk::Align::Start);
        crate::testid::set_test_id(&label, &format!("admin-nest-seed-rotate-roster-item-{i}"));
        roster_box.append(&label);
    }
    match reason_text {
        Some(text) => {
            reason.set_text(&text);
            reason.set_visible(true);
        }
        None => {
            reason.set_text("");
            reason.set_visible(false);
        }
    }
    confirm_button.set_sensitive(enabled);
}

/// `DataMessage::SeedRotateRosterLoaded` handler — the roster read resolved
/// (or failed); repaint from the new state. Guarded on still being armed: a
/// cancel click before the read lands sets `seed_rotate_confirm` back to
/// `None`, and a late-arriving roster must not silently re-arm the confirm
/// the admin already dismissed (mirrors tui's `Outcome::SeedRotateRoster`
/// handler, `admin/mod.rs`).
pub fn set_seed_rotate_roster(handles: &AdminHandles, state: SeedRotateConfirmState) {
    if handles.seed_rotate_confirm.borrow().is_none() {
        return;
    }
    *handles.seed_rotate_confirm.borrow_mut() = Some(state);
    render_seed_rotate(
        &handles.seed_rotate_confirm.borrow(),
        &handles.seed_rotate_confirm_box,
        &handles.seed_rotate_roster_box,
        &handles.seed_rotate_reason,
        &handles.seed_rotate_confirm_button,
    );
}

/// `DataMessage::SeedRotated` handler — the ceremony's own verdict
/// (`fauna_client_config::seed_rotation_verdict`). Not `error-message`: the
/// outcome that matters most (`predecessor_marked: false`) is a *success with
/// a caveat*, which an error line would misreport.
pub fn set_seed_rotate_status(handles: &AdminHandles, status: String) {
    handles.seed_rotate_status.set_text(&status);
    handles.seed_rotate_status.set_visible(true);
}

/// How often a replaced key's countdown is re-counted while the page shows.
/// Its line says whole minutes rounded up, so a tick well under a minute keeps
/// it true to the minute; the tick repaints nothing unless a replaced key is
/// still counting down.
const OAUTH_COUNTDOWN_TICK_SECS: u32 = 15;

/// Build the `admin-nest-oauth-*` section's widgets, in tui's element order —
/// client-free, so a test can paint it without a nest; [`wire_oauth_section`]
/// attaches the gestures. Every control exists from construction: the three
/// buttons are disabled, never hidden, until the set has answered, and the
/// confirm trio lives in a box [`render_oauth`] reveals only while armed.
fn build_oauth_section() -> (gtk::Box, OauthWidgets) {
    let section = build_section(ids::ADMIN_NEST_OAUTH_SECTION, admin::nest_page::OAUTH_LABEL);
    let desc = gtk::Label::new(Some(admin::nest_page::OAUTH_DESC));
    desc.set_halign(gtk::Align::Start);
    desc.set_xalign(0.0);
    desc.set_wrap(true);
    desc.add_css_class("dim-label");
    desc.set_margin_start(12);
    desc.set_margin_end(12);
    section.append(&desc);

    // Key-row container — one label per served key, painted only from an
    // answered read (the seed-rotate roster's rule).
    let keys_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    keys_box.set_margin_start(12);
    keys_box.set_margin_end(12);
    keys_box.set_margin_top(4);
    section.append(&keys_box);

    let key_reason = gtk::Label::new(Some(admin::nest_page::OAUTH_KEYS_LOADING));
    key_reason.set_halign(gtk::Align::Start);
    key_reason.set_xalign(0.0);
    key_reason.set_wrap(true);
    key_reason.add_css_class("dim-label");
    key_reason.set_margin_start(12);
    key_reason.set_margin_end(12);
    key_reason.set_margin_top(4);
    crate::testid::set_test_id(&key_reason, ids::ADMIN_NEST_OAUTH_KEY_REASON);
    section.append(&key_reason);

    // The ordinary arm and its cost, side by side: it dispatches on the press
    // (nothing breaks — the outgoing key stays accepted for the horizon), so
    // its cost is stated beside it rather than in a confirm.
    let rotate_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    rotate_row.set_margin_start(12);
    rotate_row.set_margin_end(12);
    rotate_row.set_margin_top(8);
    let rotate_button = gtk::Button::builder()
        .label(admin::nest_page::OAUTH_ROTATE_BUTTON)
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    crate::testid::set_test_id(&rotate_button, ids::ADMIN_NEST_OAUTH_ROTATE_BUTTON);
    crate::offline_gate::declare_wire_kind(&rotate_button, "fauna.oauth.rotate_issuer_key");
    rotate_row.append(&rotate_button);
    let rotate_cost = gtk::Label::new(None);
    rotate_cost.set_halign(gtk::Align::Start);
    rotate_cost.set_xalign(0.0);
    rotate_cost.set_wrap(true);
    rotate_cost.set_hexpand(true);
    rotate_cost.add_css_class("dim-label");
    rotate_cost.set_visible(false);
    rotate_row.append(&rotate_cost);
    section.append(&rotate_row);

    // The two forced arms. Arming dispatches nothing, so neither declares a
    // wire kind (tui's `wire_kind` answers `None` for both) — the confirm
    // below carries the armed arm's.
    let arms_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    arms_row.set_margin_start(12);
    arms_row.set_margin_end(12);
    arms_row.set_margin_top(4);
    let force_rotate_button = gtk::Button::builder()
        .label(admin::nest_page::OAUTH_FORCE_ROTATE_BUTTON)
        .sensitive(false)
        .build();
    crate::testid::set_test_id(
        &force_rotate_button,
        ids::ADMIN_NEST_OAUTH_FORCE_ROTATE_BUTTON,
    );
    arms_row.append(&force_rotate_button);
    let secret_force_rotate_button = gtk::Button::builder()
        .label(admin::nest_page::OAUTH_SECRET_FORCE_ROTATE_BUTTON)
        .sensitive(false)
        .build();
    crate::testid::set_test_id(
        &secret_force_rotate_button,
        ids::ADMIN_NEST_OAUTH_SECRET_FORCE_ROTATE_BUTTON,
    );
    arms_row.append(&secret_force_rotate_button);
    section.append(&arms_row);

    // The one shared forced confirm — revealed only while an arm is armed.
    let confirm_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    confirm_box.set_margin_start(12);
    confirm_box.set_margin_end(12);
    confirm_box.set_margin_top(4);
    confirm_box.set_visible(false);

    let confirm_summary = gtk::Label::new(None);
    confirm_summary.set_halign(gtk::Align::Start);
    confirm_summary.set_xalign(0.0);
    confirm_summary.set_wrap(true);
    crate::testid::set_test_id(&confirm_summary, ids::ADMIN_NEST_OAUTH_CONFIRM_SUMMARY);
    confirm_box.append(&confirm_summary);

    let confirm_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let confirm_button = gtk::Button::builder()
        .css_classes(["destructive-action"])
        .build();
    crate::testid::set_test_id(&confirm_button, ids::ADMIN_NEST_OAUTH_CONFIRM_BUTTON);
    // Gated from construction on the key arm's kind; it is hidden until an arm
    // is armed, and every arm re-declares the kind it will actually dispatch
    // (`wire_oauth_section`).
    crate::offline_gate::declare_wire_kind(
        &confirm_button,
        fauna_client_admin::IssuerForcedArm::IssuerKey.kind(),
    );
    confirm_buttons.append(&confirm_button);
    let cancel_button = gtk::Button::builder()
        .label(admin::nest_page::OAUTH_CANCEL_BUTTON)
        .build();
    crate::testid::set_test_id(&cancel_button, ids::ADMIN_NEST_OAUTH_CANCEL_BUTTON);
    confirm_buttons.append(&cancel_button);
    confirm_box.append(&confirm_buttons);
    section.append(&confirm_box);

    let status = gtk::Label::new(None);
    status.set_halign(gtk::Align::Start);
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.set_margin_start(12);
    status.set_margin_end(12);
    status.set_margin_top(4);
    status.set_visible(false);
    crate::testid::set_test_id(&status, ids::ADMIN_NEST_OAUTH_STATUS);
    section.append(&status);

    (
        section,
        OauthWidgets {
            keys_box,
            key_reason,
            rotate_cost,
            rotate_button,
            force_rotate_button,
            secret_force_rotate_button,
            confirm_box,
            confirm_summary,
            confirm_button,
            cancel_button,
            status,
        },
    )
}

/// Attach the `admin-nest-oauth-*` section's four gestures and its countdown
/// tick. Each gesture runs its [`OauthSectionState`] transition and repaints
/// synchronously BEFORE it dispatches, so a disarmed confirm is gone from the
/// tree the moment the click returns. The closures hold the context WEAKLY:
/// [`AdminHandles::oauth`] owns it, and a strong capture would tie it into a
/// cycle with its own buttons that kept the tick alive past the page.
fn wire_oauth_section(ctx: &Rc<OauthSectionCtx>, client: &Rc<FaunaClient>) {
    let w = &ctx.widgets;
    {
        let client = Rc::clone(client);
        let ctx = Rc::downgrade(ctx);
        w.rotate_button.connect_clicked(move |_| {
            let Some(ctx) = ctx.upgrade() else {
                return;
            };
            let dispatch = ctx.state.borrow_mut().press_rotate();
            render_oauth(&ctx);
            if dispatch {
                client.rotate_oauth_issuer_key();
            }
        });
    }
    for (button, arm) in [
        (
            &w.force_rotate_button,
            fauna_client_admin::IssuerForcedArm::IssuerKey,
        ),
        (
            &w.secret_force_rotate_button,
            fauna_client_admin::IssuerForcedArm::SessionSecret,
        ),
    ] {
        let ctx = Rc::downgrade(ctx);
        button.connect_clicked(move |_| {
            let Some(ctx) = ctx.upgrade() else {
                return;
            };
            ctx.state.borrow_mut().arm(arm);
            // The one confirm button dispatches whichever arm is armed, so the
            // offline gate must decide on THAT arm's kind: re-declare once the
            // arm has decided (`offline_gate::declare_wire_kind`'s
            // persistent-tree form of tui's `Action::wire_kind`).
            let armed_kind = ctx.state.borrow().confirm.as_ref().map(|a| a.arm.kind());
            if let Some(kind) = armed_kind {
                crate::offline_gate::declare_wire_kind(&ctx.widgets.confirm_button, kind);
            }
            render_oauth(&ctx);
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        w.cancel_button.connect_clicked(move |_| {
            let Some(ctx) = ctx.upgrade() else {
                return;
            };
            ctx.state.borrow_mut().cancel();
            render_oauth(&ctx);
        });
    }
    {
        let client = Rc::clone(client);
        let ctx = Rc::downgrade(ctx);
        w.confirm_button.connect_clicked(move |_| {
            let Some(ctx) = ctx.upgrade() else {
                return;
            };
            // Disarm first (inside `press_confirm`), then repaint, THEN
            // dispatch: a double click finds no confirm left to press, so it
            // cannot drop the very key the first press minted.
            let arm = ctx.state.borrow_mut().press_confirm();
            render_oauth(&ctx);
            if let Some(arm) = arm {
                client.force_rotate_oauth_issuer(arm);
            }
        });
    }
    // A replaced key's line counts down against the wall clock at paint (tui
    // repaints every frame); a GTK label is painted once, so re-count it on a
    // tick — only while the page shows and a replaced key is still counting.
    // Breaks itself once the section is gone.
    {
        let ctx = Rc::downgrade(ctx);
        glib::timeout_add_seconds_local(OAUTH_COUNTDOWN_TICK_SECS, move || {
            let Some(ctx) = ctx.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let counting = ctx.widgets.keys_box.is_mapped()
                && ctx
                    .state
                    .borrow()
                    .answered()
                    .is_some_and(|view| view.keys.iter().any(|k| !k.signing));
            if counting {
                render_oauth(&ctx);
            }
            glib::ControlFlow::Continue
        });
    }
}

/// Repaint the whole `admin-nest-oauth-*` section from its state (mirrors
/// tui's `oauth_elements`, `apps/fauna-tui/src/admin/nest.rs` — the reference
/// leg). Paint only: every sentence is a shared `fauna_client_admin` fold
/// resolved here, and the enabled flags read the same `controls_live` test
/// the gestures refuse on.
fn render_oauth(ctx: &OauthSectionCtx) {
    let state = ctx.state.borrow();
    let w = &ctx.widgets;
    let lookup = crate::i18n::strings::lookup;

    // The key rows, painted ONLY from an answered read (the seed-rotate
    // roster's rule): "not asked yet" and "couldn't find out" get the reason
    // line instead, never an empty list that would read as "no keys".
    clear_box(&w.keys_box);
    let reason = match &state.keys {
        OauthKeysRead::Ready(view) => {
            // The countdown is the point of a replaced key's line, so it is
            // counted against the wall clock at paint; the instant it counts
            // to is the nest's own.
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            for (i, row) in view.keys.iter().enumerate() {
                let label = gtk::Label::new(Some(
                    &fauna_client_admin::issuer_key_row_label(row, now).resolve(lookup),
                ));
                label.set_halign(gtk::Align::Start);
                label.set_xalign(0.0);
                label.set_wrap(true);
                crate::testid::set_test_id(
                    &label,
                    &format!("{}-{i}", ids::ADMIN_NEST_OAUTH_KEY_ITEM),
                );
                w.keys_box.append(&label);
            }
            None
        }
        OauthKeysRead::Failed(reason) => Some(reason.clone()),
        OauthKeysRead::Unread => Some(admin::nest_page::OAUTH_KEYS_LOADING.to_string()),
    };
    match reason {
        Some(text) => {
            w.key_reason.set_text(&text);
            w.key_reason.set_visible(true);
        }
        None => {
            w.key_reason.set_text("");
            w.key_reason.set_visible(false);
        }
    }

    // The ordinary arm states its cost beside itself (it has no confirm) —
    // once there is a set for the cost to be about.
    match state.answered() {
        Some(view) => {
            w.rotate_cost
                .set_text(&fauna_client_admin::issuer_key_rotate_cost(view).resolve(lookup));
            w.rotate_cost.set_visible(true);
        }
        None => {
            w.rotate_cost.set_text("");
            w.rotate_cost.set_visible(false);
        }
    }

    // All three controls are live exactly when the set has answered and no
    // call is in flight — disabled, never hidden, beside the reason otherwise.
    let live = state.controls_live();
    w.rotate_button.set_sensitive(live);
    w.force_rotate_button.set_sensitive(live);
    w.secret_force_rotate_button.set_sensitive(live);

    // The armed confirm renders the CAPTURED fold — never a re-fold of a set
    // that landed after arming.
    match &state.confirm {
        Some(armed) => {
            w.confirm_summary
                .set_text(&armed.view.summary.resolve(lookup));
            w.confirm_button
                .set_label(&armed.view.confirm_label.resolve(lookup));
            w.confirm_box.set_visible(true);
        }
        None => w.confirm_box.set_visible(false),
    }

    // The verdict, its own element — not `error-message`: every success here
    // has consequences worth words, and a failure must not claim nothing
    // changed.
    match &state.status {
        Some(status) => {
            w.status.set_text(status);
            w.status.set_visible(true);
        }
        None => {
            w.status.set_text("");
            w.status.set_visible(false);
        }
    }
}

/// `DataMessage::OauthKeysLoaded` handler — the page's entry/refresh read of
/// the issuer key set landed (rows, or the worded reason line).
pub fn set_oauth_keys(handles: &AdminHandles, keys: OauthKeysRead) {
    handles.oauth.state.borrow_mut().keys_loaded(keys);
    render_oauth(&handles.oauth);
}

/// `DataMessage::OauthDone` handler — a sign-in key control's verdict and the
/// key set re-read after it, painted together (tui's `Outcome::OauthDone`).
pub fn set_oauth_done(handles: &AdminHandles, status: String, keys: OauthKeysRead) {
    handles.oauth.state.borrow_mut().done(status, keys);
    render_oauth(&handles.oauth);
}

/// Read the legal-takedown console's current widget values into the shared
/// draft type (`moderation.md` § Legal takedown → Invocation surface) — the
/// single read point both the live-refresh handlers and the arm click share,
/// so they cannot disagree about what "the form" currently holds.
fn current_takedown_form(
    content_id: &gtk::Entry,
    conversation_radio: &gtk::CheckButton,
    reference: &gtk::Entry,
    restore: &gtk::CheckButton,
) -> fauna_client_moderation::TakedownForm {
    fauna_client_moderation::TakedownForm {
        content_id: content_id.text().to_string(),
        content_type: if conversation_radio.is_active() {
            fauna_client_moderation::TakedownContentType::Conversation
        } else {
            fauna_client_moderation::TakedownContentType::Post
        },
        legal_reference: reference.text().to_string(),
        restore: restore.is_active(),
    }
}

/// Re-fold the takedown console's arm control from the current widget values
/// (mirrors tui's `nest_elements` takedown block, `apps/fauna-tui/src/admin/nest.rs`
/// — the reference leg). Called from every field-change handler in
/// `build_nest_page`; the shared `takedown_form_view` is pure, so there is no
/// async intermediate to gate on (unlike the seed-rotation roster above).
fn refresh_takedown_arm(
    content_id: &gtk::Entry,
    conversation_radio: &gtk::CheckButton,
    reference: &gtk::Entry,
    restore: &gtk::CheckButton,
    button: &gtk::Button,
    reason: &gtk::Label,
) {
    let form = current_takedown_form(content_id, conversation_radio, reference, restore);
    let view = fauna_client_moderation::takedown_form_view(&form);
    button.set_label(&view.arm_label.resolve(crate::i18n::strings::lookup));
    button.set_sensitive(view.can_submit);
    match view.blocked_reason {
        Some(text) => {
            reason.set_text(&text.resolve(crate::i18n::strings::lookup));
            reason.set_visible(true);
        }
        None => {
            reason.set_text("");
            reason.set_visible(false);
        }
    }
}

/// `DataMessage::TakedownSubmitted` handler — the dispatch's own verdict
/// (`fauna_client_moderation::takedown_verdict`). Not `error-message`: success
/// is the common case and the two verbs earn different sentences.
pub fn set_takedown_status(handles: &AdminHandles, status: String) {
    handles.takedown_status.set_text(&status);
    handles.takedown_status.set_visible(true);
}

/// Reflectively set a service switch's active state + status badge from a
/// fetched flag, guarding the programmatic `set_active` so its `active-notify`
/// handler doesn't re-dispatch a `services.update`.
fn sync_service_toggle(
    toggle: &gtk::Switch,
    status: &gtk::Label,
    guard: &Rc<std::cell::Cell<bool>>,
    enabled: bool,
) {
    guard.set(true);
    toggle.set_active(enabled);
    guard.set(false);
    status.set_text(if enabled {
        admin::services_page::ENABLED
    } else {
        admin::services_page::DISABLED
    });
}

/// Sync the admin `admin-service-pairing-toggle` + its status badge from a
/// `fauna.admin.services.list` reply (admin.md § N Nest). Post the per-page-
/// services redesign (2026-06-04) this is the **only** service toggle linux
/// renders: the `bridge` toggle was dropped (vestigial) and the `algorithm` flag
/// left the wire with its sidecar (2026-10-01),
/// and the dns master switch is synced separately from the `DnsSnapshot`
/// (`render_admin_dns` → `admin-dns-manage-all-toggle`). The reply still carries
/// `services.bridge` — it is simply no longer surfaced.
pub fn update_services(
    handles: &AdminHandles,
    reply: &fauna_client_admin::admin::AdminServicesListReply,
) {
    sync_service_toggle(
        &handles.svc_pairing_toggle,
        &handles.svc_pairing_status,
        &handles.svc_pairing_guard,
        reply.services.pairing,
    );
}

// ── admin Logs page (`admin-logs`) — the nest's fauna-log ring over WS-RPC ────
// observability.md § Surfaces: an admin sees the NEST's log ring, rendered with
// the SAME widget as the client's own Settings → Logs page. The shared render
// lives in `crate::logs_view`, so this page is a thin fetch + client-side-filter
// shell over it — the only difference from the Settings page is the source (a
// fetched `Vec` here vs. the local process ring there) and no Clear (there is no
// admin RPC to wipe the nest ring). Redaction binds the nest call sites.

/// Render state for the admin Logs page, `Rc`-shared into its filter/copy
/// closures and held in `AdminHandles` so `update_admin_logs` can refresh it.
/// Public name, opaque fields — app.rs only ever passes it back to the render.
pub struct AdminLogsCtx {
    filter: gtk::DropDown,
    list_group: adw::PreferencesGroup,
    placeholder_row: adw::ActionRow,
    rows: RefCell<Vec<adw::ActionRow>>,
    /// The last-fetched nest ring (oldest-first). The filter narrows THIS in
    /// memory — the admin view has no local ring to re-query — exactly mirroring
    /// the Settings page's `snapshot_at_least` over the client's own ring.
    source: RefCell<Vec<LogEntry>>,
}

struct AdminLogsPage {
    page: adw::PreferencesPage,
    ctx: Rc<AdminLogsCtx>,
    error_label: gtk::Label,
}

/// Re-render the list from the held source at the active filter (client-side).
fn render_admin_logs(ctx: &Rc<AdminLogsCtx>) {
    let min = fauna_log::format::level_for_index(ctx.filter.selected());
    let entries = fauna_log::format::filter_entries(&ctx.source.borrow(), min);
    logs_view::populate(&ctx.list_group, &ctx.placeholder_row, &ctx.rows, &entries);
}

fn build_admin_logs_page() -> AdminLogsPage {
    let page = adw::PreferencesPage::builder()
        .title(admin::logs_page::TITLE)
        .icon_name("utilities-system-monitor-symbolic")
        .build();

    let top_group = adw::PreferencesGroup::builder()
        .title(admin::logs_page::TITLE)
        .description(admin::logs_page::DESCRIPTION)
        .build();
    // admin-logs-heading — the page-visible marker the e2e waits on (mirrors
    // admin-dashboard-heading); the stack child name "admin-logs" is the nav id.
    let heading = gtk::Label::new(Some(admin::logs_page::TITLE));
    heading.set_height_request(1);
    heading.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&heading, ids::ADMIN_LOGS_HEADING);
    top_group.set_header_suffix(Some(&heading));

    // error-message (cross-app Rule 2) — hidden until a fetch fails.
    let error_label = build_error_label();
    top_group.add(&error_label);

    // Severity filter (client-side over the fetched source) — reuses the shared
    // Logs filter labels so admin + Settings offer the identical severity set.
    let filter = gtk::DropDown::from_strings(&logs_view::filter_labels());
    filter.set_valign(gtk::Align::Center);
    crate::testid::set_test_id(&filter, ids::LOG_LEVEL_FILTER);
    let filter_row = adw::ActionRow::builder()
        .title(crate::i18n::strings::logs::FILTER_LABEL)
        .activatable(false)
        .build();
    filter_row.add_suffix(&filter);
    top_group.add(&filter_row);

    // Copy the currently-visible lines (no Clear — there is no RPC to wipe the
    // nest ring).
    let copy_button = gtk::Button::builder()
        .label(crate::i18n::strings::logs::COPY_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    crate::testid::set_test_id(&copy_button, ids::LOG_COPY_BUTTON);
    let copy_row = adw::ActionRow::builder().activatable(false).build();
    copy_row.add_suffix(&copy_button);
    top_group.add(&copy_row);

    page.add(&top_group);

    let list_group = adw::PreferencesGroup::new();
    let placeholder_row = adw::ActionRow::builder()
        .title(admin::logs_page::EMPTY)
        .build();
    list_group.add(&placeholder_row);
    page.add(&list_group);

    let ctx = Rc::new(AdminLogsCtx {
        filter: filter.clone(),
        list_group,
        placeholder_row,
        rows: RefCell::new(Vec::new()),
        source: RefCell::new(Vec::new()),
    });

    render_admin_logs(&ctx);

    {
        let ctx = Rc::clone(&ctx);
        filter.connect_selected_notify(move |_| render_admin_logs(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        copy_button.connect_clicked(move |btn| {
            let min = fauna_log::format::level_for_index(ctx.filter.selected());
            let entries = fauna_log::format::filter_entries(&ctx.source.borrow(), min);
            btn.clipboard().set_text(&fauna_log::format::rendered_text(
                &entries,
                logs_view::local_offset_secs(),
            ));
        });
    }

    AdminLogsPage {
        page,
        ctx,
        error_label,
    }
}

/// Populate the admin Logs page from a `fauna.admin.logs` reply: store the
/// nest's ring as the page source + re-render at the active filter.
pub fn update_admin_logs(
    handles: &AdminHandles,
    reply: &fauna_client_admin::admin::AdminLogsReply,
) {
    render_page_error(&handles.admin_logs_error, None);
    *handles.admin_logs_ctx.source.borrow_mut() = reply
        .entries
        .iter()
        .map(fauna_client_admin::log_entry_from_wire)
        .collect();
    render_admin_logs(&handles.admin_logs_ctx);
}

/// Build the `admin-bridges-pending` sub-page: a heading + a boxed list of
/// approval cards. The cards are rebuilt on every `AdminPendingBridgesLoaded`
/// (interactive — unlike the display-only email-domains list — so the buttons
/// dispatch through the shared `BridgeApprovalMachine`). Mirrors the
/// email-domains section's structure.
/// One section subheading inside the bridges page ("Pending approval" /
/// "Approved bridges") separating the two rosters.
fn bridges_section_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("heading");
    label.set_halign(gtk::Align::Start);
    label.set_margin_start(12);
    label.set_margin_top(8);
    label.set_margin_bottom(4);
    label
}

/// A boxed-list [`gtk::ListBox`] with a `StatusPage` placeholder for one bridge
/// roster (pending or approved).
fn bridges_roster_list(placeholder: &adw::StatusPage) -> gtk::ListBox {
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    list_box.set_margin_start(8);
    list_box.set_margin_end(8);
    list_box.set_margin_bottom(8);
    list_box.set_placeholder(Some(placeholder));
    list_box
}

fn build_bridges_pending_page() -> (gtk::Box, gtk::ListBox, gtk::ListBox, gtk::Label) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let heading = gtk::Label::new(Some(admin::bridges_pending::TITLE));
    heading.add_css_class("title-2");
    heading.set_margin_top(16);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&heading, ids::PAGE_HEADING);
    outer.append(&heading);

    let desc = gtk::Label::new(Some(admin::bridges_pending::DESCRIPTION));
    desc.add_css_class("dim-label");
    desc.set_halign(gtk::Align::Start);
    desc.set_margin_start(12);
    desc.set_margin_bottom(8);
    outer.append(&desc);

    // Both sections (pending cards + the approved-bridge roster) stack inside one
    // scroll (admin.md § Approved-bridges roster).
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    content.append(&bridges_section_label(
        admin::bridges_pending::PENDING_SECTION,
    ));
    let pending_placeholder = adw::StatusPage::builder()
        .title(admin::bridges_pending::EMPTY)
        .description(admin::bridges_pending::EMPTY_DESC)
        .icon_name("network-server-symbolic")
        .build();
    let pending_list = bridges_roster_list(&pending_placeholder);
    content.append(&pending_list);

    content.append(&bridges_section_label(
        admin::bridges_pending::APPROVED_SECTION,
    ));
    let approved_placeholder = adw::StatusPage::builder()
        .title(admin::bridges_pending::APPROVED_EMPTY)
        .icon_name("network-server-symbolic")
        .build();
    let approved_list = bridges_roster_list(&approved_placeholder);
    content.append(&approved_list);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&content)
        .build();
    outer.append(&scrolled);

    // Per-page `error-message` (rule #2). Shows `BridgeApprovalSnapshot.error`
    // (a failed list/approve/reject/rotate) via `update_pending_bridges`.
    let error_label = build_error_label();
    outer.append(&error_label);

    (outer, pending_list, approved_list, error_label)
}

/// One labelled field row inside an approval card. The testid sits on the value
/// label so the e2e bridge reads the value text directly (the same convention as
/// `admin-settings-email-domain-item`); the caption carries no testid.
fn pending_field_row(caption: &str, value: &str, value_testid: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.set_margin_top(2);
    row.set_margin_bottom(2);

    let cap = gtk::Label::new(Some(caption));
    cap.add_css_class("dim-label");
    cap.set_halign(gtk::Align::Start);
    cap.set_width_chars(12);
    cap.set_xalign(0.0);
    row.append(&cap);

    let val = gtk::Label::new(Some(value));
    val.set_halign(gtk::Align::Start);
    val.set_hexpand(true);
    val.set_xalign(0.0);
    val.set_selectable(true);
    val.set_wrap(true);
    val.add_css_class("monospace");
    crate::testid::set_test_id(&val, value_testid);
    row.append(&val);

    row
}

/// Format an epoch-millis timestamp for the `first-seen-at` field: the shared
/// minute-precision local render ([`fauna_core::format::format_unix_local`]),
/// adapted once here from this wire field's milliseconds.
fn format_first_seen(epoch_millis: u64) -> String {
    fauna_core::format::format_unix_local((epoch_millis / 1000) as i64)
}

/// Build one `admin-bridges-pending-card` from a [`PendingBridgeView`]. The
/// approve/reject buttons dispatch through the client (→ shared
/// `BridgeApprovalMachine`), capturing the card's hex pubkey + role.
fn build_pending_bridge_card(
    view: &PendingBridgeView,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    crate::testid::set_test_id(&row, ids::ADMIN_BRIDGES_PENDING_CARD);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.set_margin_top(8);
    card.set_margin_bottom(8);

    // Friendly per-role display name atop the card (admin.md § Bridge display
    // naming; bridges.md § Active bridges). The MDA serves IMAP + CalDAV, the MTA
    // serves SMTP only — so only the MDA names calendar. The technical role still
    // renders below (it drives the per-role allowlist applied on approve).
    let name = fauna_client_mail_settings::bridge_display_name(&view.requested_role)
        .resolve(crate::i18n::strings::lookup);
    let name_label = gtk::Label::new(Some(&name));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_xalign(0.0);
    name_label.set_margin_start(12);
    name_label.set_margin_end(12);
    name_label.set_margin_bottom(4);
    name_label.add_css_class("heading");
    crate::testid::set_test_id(&name_label, ids::ADMIN_BRIDGES_PENDING_CARD_NAME);
    card.append(&name_label);

    card.append(&pending_field_row(
        admin::bridges_pending::ROLE,
        &view.requested_role,
        "admin-bridges-pending-requested-role",
    ));
    card.append(&pending_field_row(
        admin::bridges_pending::PUBKEY,
        &view.pubkey_hex,
        "admin-bridges-pending-pubkey-hex",
    ));
    // `source_ip` is a nest-side wire gap today (`ServiceUserInfo` carries none)
    // → render a placeholder dash; the ID stays so ui.yaml conforms.
    card.append(&pending_field_row(
        admin::bridges_pending::SOURCE_IP,
        view.source_ip
            .as_deref()
            .unwrap_or(admin::bridges_pending::SOURCE_IP_UNKNOWN),
        "admin-bridges-pending-source-ip",
    ));
    card.append(&pending_field_row(
        admin::bridges_pending::FIRST_SEEN,
        &format_first_seen(view.first_seen_at),
        "admin-bridges-pending-first-seen-at",
    ));

    // Action buttons.
    let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_box.set_margin_start(12);
    btn_box.set_margin_end(12);
    btn_box.set_margin_top(8);
    btn_box.set_halign(gtk::Align::End);

    let reject_btn = gtk::Button::with_label(admin::bridges_pending::REJECT);
    reject_btn.add_css_class("destructive-action");
    crate::testid::set_test_id(&reject_btn, ids::ADMIN_BRIDGES_PENDING_REJECT_BUTTON);
    // tui's `admin::Action::RejectBridge`.
    crate::offline_gate::declare_wire_kind(&reject_btn, "fauna.bridges.reject_pending_bridge");
    {
        let client_ref = Rc::clone(client);
        let pubkey_hex = view.pubkey_hex.clone();
        reject_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            client_ref.reject_pending_bridge(pubkey_hex.clone());
        });
    }

    let approve_btn = gtk::Button::with_label(admin::bridges_pending::APPROVE);
    approve_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&approve_btn, ids::ADMIN_BRIDGES_PENDING_APPROVE_BUTTON);
    // tui's `admin::Action::ApproveBridge`.
    crate::offline_gate::declare_wire_kind(&approve_btn, "fauna.bridges.approve_pending_bridge");
    {
        let client_ref = Rc::clone(client);
        let pubkey_hex = view.pubkey_hex.clone();
        let role = view.requested_role.clone();
        approve_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            client_ref.approve_pending_bridge(pubkey_hex.clone(), role.clone());
        });
    }

    btn_box.append(&reject_btn);
    btn_box.append(&approve_btn);
    card.append(&btn_box);

    row.set_child(Some(&card));
    row
}

/// Build one `admin-bridges-approved-card` from an [`ApprovedBridgeView`]. The
/// rotate button opens the `admin-bridges-rotate-confirm` dialog, which on
/// confirm dispatches the rotation through the client (→ shared
/// `BridgeApprovalMachine` `Rotate` → `revoke_service_user`).
fn build_approved_bridge_card(
    view: &ApprovedBridgeView,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    crate::testid::set_test_id(&row, ids::ADMIN_BRIDGES_APPROVED_CARD);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.set_margin_top(8);
    card.set_margin_bottom(8);

    // Same friendly per-role display name as the pending card (shared
    // `bridge_display_name`).
    let name = fauna_client_mail_settings::bridge_display_name(&view.role)
        .resolve(crate::i18n::strings::lookup);
    let name_label = gtk::Label::new(Some(&name));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_xalign(0.0);
    name_label.set_margin_start(12);
    name_label.set_margin_end(12);
    name_label.set_margin_bottom(4);
    name_label.add_css_class("heading");
    crate::testid::set_test_id(&name_label, ids::ADMIN_BRIDGES_APPROVED_CARD_NAME);
    card.append(&name_label);

    card.append(&pending_field_row(
        admin::bridges_pending::ROLE,
        &view.role,
        "admin-bridges-approved-role",
    ));
    card.append(&pending_field_row(
        admin::bridges_pending::PUBKEY,
        &view.pubkey_hex,
        "admin-bridges-approved-pubkey-hex",
    ));
    // `approved_at` is optional (None on a non-conforming reply; pending rows carry none) → dash placeholder.
    card.append(&pending_field_row(
        admin::bridges_pending::APPROVED_AT,
        &view
            .approved_at
            .map(format_first_seen)
            .unwrap_or_else(|| admin::bridges_pending::SOURCE_IP_UNKNOWN.to_string()),
        "admin-bridges-approved-approved-at",
    ));

    let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_box.set_margin_start(12);
    btn_box.set_margin_end(12);
    btn_box.set_margin_top(8);
    btn_box.set_halign(gtk::Align::End);

    let rotate_btn = gtk::Button::with_label(admin::bridges_pending::ROTATE);
    rotate_btn.add_css_class("destructive-action");
    crate::testid::set_test_id(&rotate_btn, ids::ADMIN_BRIDGES_APPROVED_ROTATE_BUTTON);
    // The click only opens the confirm dialog, but the ceremony behind it ends
    // in exactly one kind (`open_rotate_confirm` → `rotate_service_user`), so
    // the ENTRY declares — same reasoning as the factory-reset button (the
    // dialog's own confirm response button is materialized post-present and
    // cannot be `declare_wire_kind`d directly). tui's `admin::Action::ConfirmRotate`.
    crate::offline_gate::declare_wire_kind(&rotate_btn, "fauna.bridges.revoke_service_user");
    {
        let client_ref = Rc::clone(client);
        let pubkey_hex = view.pubkey_hex.clone();
        rotate_btn.connect_clicked(move |btn| {
            open_rotate_confirm(btn, &client_ref, pubkey_hex.clone());
        });
    }
    btn_box.append(&rotate_btn);
    card.append(&btn_box);

    row.set_child(Some(&card));
    row
}

/// The `admin-bridges-rotate-confirm` dialog. Shows the rotation warning for
/// every role (no DKIM warning: the nest holds every DKIM key —
/// `mail-bridge-lifecycle.md` § Service-user re-keying). On confirm, dispatches
/// the rotation through the client.
fn open_rotate_confirm(anchor: &gtk::Button, client: &Rc<FaunaClient>, pubkey_hex: String) {
    // The warning is a `Child` body (not a plain string) so it can carry its
    // own test id.
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let warning = gtk::Label::new(Some(admin::bridges_rotate::WARNING));
    warning.set_wrap(true);
    warning.set_xalign(0.0);
    crate::testid::set_test_id(&warning, ids::ADMIN_BRIDGES_ROTATE_WARNING_TEXT);
    body.append(&warning);

    let client_ref = Rc::clone(client);
    crate::confirm_dialog::present_confirm(
        anchor,
        crate::confirm_dialog::ConfirmSpec::new(
            admin::bridges_rotate::TITLE,
            crate::confirm_dialog::ConfirmBody::Child(body.upcast_ref::<gtk::Widget>()),
            "rotate",
            admin::bridges_rotate::CONFIRM,
            admin::bridges_rotate::CANCEL,
        )
        .with_dialog_id(ids::ADMIN_BRIDGES_ROTATE_CONFIRM)
        .with_confirm_id(ids::ADMIN_BRIDGES_ROTATE_CONFIRM_BUTTON)
        .with_cancel_id(ids::ADMIN_BRIDGES_ROTATE_CANCEL_BUTTON),
        move || client_ref.rotate_service_user(pubkey_hex.clone()),
    );
}

/// Render the bridge rosters from a [`BridgeApprovalSnapshot`]: the pending
/// approval cards (`admin-bridges-pending-card`) above and the approved roster
/// (`admin-bridges-approved-card`, each with a rotate button) below. Rebuilt on
/// every load (approve/reject/rotate re-read the feed, so both lists reflect the
/// post-mutation state).
pub fn update_pending_bridges(
    handles: &AdminHandles,
    snapshot: &BridgeApprovalSnapshot,
    client: &Rc<FaunaClient>,
) {
    render_page_error(&handles.pending_bridges_error, snapshot.error.as_deref());
    clear_list_box(&handles.pending_bridges_list);
    for view in &snapshot.pending {
        let row = build_pending_bridge_card(view, client);
        handles.pending_bridges_list.append(&row);
    }
    clear_list_box(&handles.approved_bridges_list);
    for view in &snapshot.approved {
        let row = build_approved_bridge_card(view, client);
        handles.approved_bridges_list.append(&row);
    }
}

// ---------------------------------------------------------------------------
// Custody-hosting sub-page (`admin-custody-hosting`) — the nest-wide
// held-for-others registry (account-data-plane.md § Two-sided bounds). Reference: `apps/fauna-tui/src/admin/
// custody_hosting.rs`.
// ---------------------------------------------------------------------------

/// The budget in force. `0` is not "no bytes allowed" — it means the row
/// carries no cap and the pump substitutes the hard-coded default, so
/// *Default* is the honest rendering and a printed `0 B` would be a lie.
fn custody_hosting_budget_text(cap: u64) -> String {
    fauna_client_capabilities::view_model::custody_hosting_budget_text(
        cap,
        crate::i18n::strings::lookup,
    )
}

/// Widget handles + shared state for the `admin-custody-hosting` page,
/// bundled so row click closures can re-render the list from the cached
/// snapshot without a network round trip. `Clone` is cheap: GTK widgets are
/// themselves reference-counted, and the other two fields are `Rc`.
#[derive(Clone)]
struct CustodyHostingHandles {
    list: gtk::ListBox,
    header: gtk::Label,
    error: gtk::Label,
    status: gtk::Label,
    /// The armed remove-confirm's `(host_actor_id, grant_id)` pair — mirrors
    /// [`AdminHandles::custody_hosting_confirm`] (the SAME `Rc`, threaded
    /// through by [`update_custody_hosting`]).
    confirm_state: Rc<RefCell<Option<CustodyHostingRowKey>>>,
    /// The last landed read, cached so arm/disarm/remove can repaint without
    /// re-fetching — mirrors [`AdminHandles::custody_hosting_last`] (the SAME
    /// `Rc`).
    last_snapshot: Rc<RefCell<Option<AdminHostingSnapshot>>>,
}

/// Build one `admin-custody-hosting-row` card — every card repeats the same
/// ids (`indexed: true`, convention 1: a driver addresses row `n` by index or
/// scope, never by an index baked into the id). `armed` is whether THIS row's
/// `(host, grant)` pair is the one `h.confirm_state` currently names — the
/// confirm names ONE row, so a second arm must retarget it rather than stack.
fn build_custody_hosting_row(
    row: &fauna_client_capabilities::view_model::AdminHostingRowView,
    armed: bool,
    client: &Rc<FaunaClient>,
    h: &CustodyHostingHandles,
) -> gtk::ListBoxRow {
    let list_row = gtk::ListBoxRow::new();
    list_row.set_selectable(false);
    crate::testid::set_test_id(&list_row, ids::ADMIN_CUSTODY_HOSTING_ROW);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.set_margin_top(8);
    card.set_margin_bottom(8);

    let host_label = pending_field_row(
        admin::custody_hosting::HOST,
        &fauna_core::format::short_id(&row.host_actor_id),
        ids::ADMIN_CUSTODY_HOSTING_HOST,
    );
    card.append(&host_label);
    card.append(&pending_field_row(
        admin::custody_hosting::OWNER,
        &fauna_core::format::short_id(&row.owner_actor_id),
        ids::ADMIN_CUSTODY_HOSTING_OWNER,
    ));
    // Verbatim, never prettified: an admin reading this page is looking for
    // exactly the address the pump dials.
    card.append(&pending_field_row(
        admin::custody_hosting::URL,
        &row.owner_nest_url,
        ids::ADMIN_CUSTODY_HOSTING_URL,
    ));
    card.append(&pending_field_row(
        admin::custody_hosting::BUDGET,
        &custody_hosting_budget_text(row.retained_bytes_cap),
        ids::ADMIN_CUSTODY_HOSTING_BUDGET,
    ));
    card.append(&pending_field_row(
        admin::custody_hosting::HELD,
        &crate::i18n::byte_size(row.held_bytes),
        ids::ADMIN_CUSTODY_HOSTING_HELD,
    ));
    // Paused is a real, distinct state from Active — a stopped row still
    // holds its bytes, which is the whole reason remove exists beside stop.
    card.append(&pending_field_row(
        "",
        if row.stopped {
            admin::custody_hosting::STOPPED
        } else {
            admin::custody_hosting::ACTIVE
        },
        ids::ADMIN_CUSTODY_HOSTING_STOPPED,
    ));
    card.append(&pending_field_row(
        "",
        fauna_client_capabilities::view_model::receipt_text(row.receipt_state),
        ids::ADMIN_CUSTODY_HOSTING_RECEIPT,
    ));

    let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_box.set_margin_start(12);
    btn_box.set_margin_end(12);
    btn_box.set_margin_top(8);
    btn_box.set_halign(gtk::Align::End);

    let remove_btn = gtk::Button::with_label(admin::custody_hosting::REMOVE);
    remove_btn.add_css_class("destructive-action");
    remove_btn.set_sensitive(!armed);
    crate::testid::set_test_id(&remove_btn, ids::ADMIN_CUSTODY_HOSTING_REMOVE_BUTTON);
    {
        let h = h.clone();
        let client = Rc::clone(client);
        let host = row.host_actor_id.clone();
        let grant = row.grant_id.clone();
        remove_btn.connect_clicked(move |_| {
            *h.confirm_state.borrow_mut() = Some((host.clone(), grant.clone()));
            rerender_custody_hosting(&h, &client);
        });
    }
    btn_box.append(&remove_btn);
    card.append(&btn_box);

    if armed {
        let confirm_title = gtk::Label::new(Some(admin::custody_hosting::REMOVE_CONFIRM_TITLE));
        confirm_title.set_halign(gtk::Align::Start);
        confirm_title.set_margin_start(12);
        confirm_title.add_css_class("heading");
        card.append(&confirm_title);

        let confirm_body = gtk::Label::new(Some(admin::custody_hosting::REMOVE_CONFIRM_BODY));
        confirm_body.set_halign(gtk::Align::Start);
        confirm_body.set_margin_start(12);
        confirm_body.set_margin_end(12);
        confirm_body.set_wrap(true);
        confirm_body.add_css_class("dim-label");
        card.append(&confirm_body);

        let confirm_btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        confirm_btn_box.set_margin_start(12);
        confirm_btn_box.set_margin_end(12);
        confirm_btn_box.set_margin_top(8);
        confirm_btn_box.set_halign(gtk::Align::End);

        let confirm_btn = gtk::Button::with_label(admin::custody_hosting::REMOVE_CONFIRM);
        confirm_btn.add_css_class("destructive-action");
        crate::testid::set_test_id(
            &confirm_btn,
            ids::ADMIN_CUSTODY_HOSTING_REMOVE_CONFIRM_BUTTON,
        );
        // The row rebuilds fresh on arm/disarm (`rerender_custody_hosting`), so
        // this confirm button is a fresh widget per armed render — declare
        // directly on it, not the arming `remove_btn` above (which is local).
        // tui's `admin::Action::ConfirmCustodyHostingRemove`.
        crate::offline_gate::declare_wire_kind(&confirm_btn, "fauna.admin.custody_hosting.remove");
        {
            let h = h.clone();
            let client = Rc::clone(client);
            confirm_btn.connect_clicked(move |_| {
                // Disarm-before-dispatch: a double click cannot chain a second
                // remove onto the first.
                if let Some((host, grant)) = h.confirm_state.borrow_mut().take() {
                    client.remove_custody_hosting(host, grant);
                }
                rerender_custody_hosting(&h, &client);
            });
        }
        confirm_btn_box.append(&confirm_btn);

        let cancel_btn = gtk::Button::with_label(admin::custody_hosting::REMOVE_CANCEL);
        crate::testid::set_test_id(&cancel_btn, ids::ADMIN_CUSTODY_HOSTING_REMOVE_CANCEL_BUTTON);
        {
            let h = h.clone();
            let client = Rc::clone(client);
            cancel_btn.connect_clicked(move |_| {
                *h.confirm_state.borrow_mut() = None;
                rerender_custody_hosting(&h, &client);
            });
        }
        confirm_btn_box.append(&cancel_btn);
        card.append(&confirm_btn_box);
    }

    list_row.set_child(Some(&card));
    list_row
}

/// Re-render the list from `h`'s cached snapshot — the arm/disarm/remove
/// click path, which must not cost a network round trip just to toggle a
/// confirm.
fn rerender_custody_hosting(h: &CustodyHostingHandles, client: &Rc<FaunaClient>) {
    let Some(snapshot) = h.last_snapshot.borrow().clone() else {
        return;
    };
    render_custody_hosting_list(h, &snapshot, client);
}

/// The list-painting half of [`update_custody_hosting`] — shared with the
/// arm/disarm re-render, which repaints from the cache rather than a fresh
/// network read.
fn render_custody_hosting_list(
    h: &CustodyHostingHandles,
    snapshot: &AdminHostingSnapshot,
    client: &Rc<FaunaClient>,
) {
    render_page_error(&h.error, snapshot.error.as_deref());
    match &snapshot.status {
        Some(s) if !s.is_empty() => {
            h.status.set_text(s);
            h.status.set_visible(true);
        }
        _ => {
            h.status.set_text("");
            h.status.set_visible(false);
        }
    }

    h.header.set_visible(true);
    if snapshot.rows.is_empty() {
        h.header.set_text(admin::custody_hosting::EMPTY);
        crate::testid::set_test_id(&h.header, ids::ADMIN_CUSTODY_HOSTING_EMPTY);
    } else {
        h.header.set_text(&admin::custody_hosting::count(
            &snapshot.rows.len().to_string(),
        ));
        crate::testid::set_test_id(&h.header, ids::ADMIN_CUSTODY_HOSTING_COUNT);
    }

    clear_list_box(&h.list);
    let armed_pair = h.confirm_state.borrow().clone();
    for row in &snapshot.rows {
        let armed = armed_pair
            .as_ref()
            .is_some_and(|(host, grant)| host == &row.host_actor_id && grant == &row.grant_id);
        let card = build_custody_hosting_row(row, armed, client, h);
        h.list.append(&card);
    }
}

/// Build the `admin-custody-hosting` sub-page: a heading + description + a
/// boxed list of hosting-row cards (mirrors `build_bridges_pending_page`'s
/// shape). Pre-hydrate the header stays hidden — "nobody asked this nest to
/// hold anything" and "the read has not answered yet" are different facts,
/// and this page must never render the reassuring one for the unknown one.
fn build_custody_hosting_page() -> (gtk::Box, gtk::ListBox, gtk::Label, gtk::Label, gtk::Label) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let heading = gtk::Label::new(Some(admin::custody_hosting::TITLE));
    heading.add_css_class("title-2");
    heading.set_margin_top(16);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&heading, ids::PAGE_HEADING);
    outer.append(&heading);

    let desc = gtk::Label::new(Some(admin::custody_hosting::DESCRIPTION));
    desc.add_css_class("dim-label");
    desc.set_halign(gtk::Align::Start);
    desc.set_margin_start(12);
    desc.set_margin_bottom(8);
    desc.set_wrap(true);
    outer.append(&desc);

    // Pre-hydrate: no count, no empty state. Hidden until the first read
    // lands — see the module doc above.
    let header = gtk::Label::new(None);
    header.set_halign(gtk::Align::Start);
    header.set_margin_start(12);
    header.set_margin_bottom(4);
    header.set_visible(false);
    outer.append(&header);

    let placeholder = adw::StatusPage::builder()
        .title(admin::custody_hosting::EMPTY)
        .icon_name("network-server-symbolic")
        .build();
    let list = bridges_roster_list(&placeholder);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();
    outer.append(&scrolled);

    // The remove's own verdict, present only once one has been attempted.
    // Not `error-message`: `removed: false` on a row someone else already
    // dropped is an honest no-op, not a failure.
    let status = gtk::Label::new(None);
    status.set_visible(false);
    status.set_halign(gtk::Align::Start);
    status.set_margin_start(12);
    status.set_margin_bottom(4);
    outer.append(&status);

    let error_label = build_error_label();
    outer.append(&error_label);

    (outer, list, header, error_label, status)
}

/// Repaint the `admin-custody-hosting` page from a fresh
/// `AdminCustodyHostingLoaded` read — caches the snapshot so the confirm
/// arm/disarm/remove buttons can re-render without a network round trip, and
/// always disarms first: the rows may have re-ordered, so a confirm armed
/// against the old surface must not survive into the new one.
pub fn update_custody_hosting(
    handles: &AdminHandles,
    snapshot: AdminHostingSnapshot,
    client: &Rc<FaunaClient>,
) {
    *handles.custody_hosting_confirm.borrow_mut() = None;
    let h = CustodyHostingHandles {
        list: handles.custody_hosting_list.clone(),
        header: handles.custody_hosting_header.clone(),
        error: handles.custody_hosting_error.clone(),
        status: handles.custody_hosting_status.clone(),
        confirm_state: Rc::clone(&handles.custody_hosting_confirm),
        last_snapshot: Rc::clone(&handles.custody_hosting_last),
    };
    render_custody_hosting_list(&h, &snapshot, client);
    *handles.custody_hosting_last.borrow_mut() = Some(snapshot);
}

// ---------------------------------------------------------------------------
// Aliases sub-page (`admin-aliases`) — admin external forwarders (admin.md § 4)
// ---------------------------------------------------------------------------

/// Handles for the `admin-aliases` page, threaded into [`AdminHandles`].
struct AdminAliasesPage {
    page: gtk::Box,
    forwarders_list: gtk::ListBox,
    forwarder_add_domain: gtk::DropDown,
    forwarders_action_error: gtk::Label,
}

/// The selected display string of a plain `gtk::DropDown` (its `StringObject`),
/// or `None` on an empty model. Used for the forwarder domain picker (the
/// hosted-domain options are populated async from `ForwardersSnapshot`).
fn dropdown_selected_string(dd: &gtk::DropDown) -> Option<String> {
    dd.selected_item()
        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
        .map(|s| s.string().to_string())
}

/// Build the `admin-aliases` sub-page (admin.md § 4): the external-forwarders
/// section — an add form (hosted-domain picker + local-part + external target)
/// above the indexed forwarder list. The catch-all-designation section is a
/// deferred slice (needs a `set_catch_all_actor` nest RPC). The add form is
/// built once; the list + the domain picker's options are repopulated on every
/// `AdminForwardersLoaded` (`set_forwarders_snapshot`).
fn build_admin_aliases_page(client: &Rc<FaunaClient>) -> AdminAliasesPage {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let heading = gtk::Label::new(Some(admin::aliases_page::TITLE));
    heading.add_css_class("title-2");
    heading.set_margin_top(16);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&heading, ids::ADMIN_ALIASES_HEADING);
    outer.append(&heading);

    // ── Forwarders section ──
    // `accessible_role(Group)` so the container Box reliably surfaces its id to
    // AT-SPI (a plain Box defaults to role Generic, which the bridge may omit —
    // same idiom as the DNS page's credential list).
    let forwarders_section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    forwarders_section.set_margin_start(12);
    forwarders_section.set_margin_end(12);
    forwarders_section.set_margin_top(8);
    crate::testid::set_test_id(&forwarders_section, ids::ADMIN_ALIASES_FORWARDERS_SECTION);

    let section_title = gtk::Label::new(Some(admin::aliases_page::FORWARDERS_TITLE));
    section_title.add_css_class("title-4");
    section_title.set_halign(gtk::Align::Start);
    forwarders_section.append(&section_title);

    let section_desc = gtk::Label::new(Some(admin::aliases_page::FORWARDERS_DESC));
    section_desc.add_css_class("dim-label");
    section_desc.set_wrap(true);
    section_desc.set_halign(gtk::Align::Start);
    section_desc.set_margin_bottom(4);
    forwarders_section.append(&section_desc);

    // Add form: domain picker + local-part + target + submit. A single row.
    let add_form = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    add_form.set_margin_bottom(8);

    // Hosted-domain picker — options populated from the snapshot's
    // `local_domains` (a forwarder must live on a hosted domain). Empty until the
    // first `AdminForwardersLoaded`.
    let domain_select = gtk::DropDown::from_strings(&[]);
    domain_select.set_tooltip_text(Some(admin::aliases_page::FORWARDER_DOMAIN));
    crate::testid::set_test_id(
        &domain_select,
        ids::ADMIN_ALIASES_FORWARDER_ADD_DOMAIN_SELECT,
    );
    add_form.append(&domain_select);

    let pattern_entry = gtk::Entry::builder()
        .placeholder_text(admin::aliases_page::FORWARDER_LOCAL_PART_PLACEHOLDER)
        .build();
    pattern_entry.set_tooltip_text(Some(admin::aliases_page::FORWARDER_LOCAL_PART));
    crate::testid::set_test_id(
        &pattern_entry,
        ids::ADMIN_ALIASES_FORWARDER_ADD_PATTERN_INPUT,
    );
    add_form.append(&pattern_entry);

    let target_entry = gtk::Entry::builder()
        .placeholder_text(admin::aliases_page::FORWARDER_TARGET_PLACEHOLDER)
        .hexpand(true)
        .build();
    target_entry.set_tooltip_text(Some(admin::aliases_page::FORWARDER_TARGET));
    crate::testid::set_test_id(&target_entry, ids::ADMIN_ALIASES_FORWARDER_ADD_TARGET_INPUT);
    add_form.append(&target_entry);

    let submit_btn = gtk::Button::with_label(admin::aliases_page::CREATE_FORWARDER);
    submit_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&submit_btn, ids::ADMIN_ALIASES_FORWARDER_ADD_SUBMIT_BUTTON);
    // tui's `admin::Action::CreateForwarder`.
    crate::offline_gate::declare_wire_kind(&submit_btn, "fauna.bridges.create_forwarder");
    add_form.append(&submit_btn);

    {
        let client = Rc::clone(client);
        let domain_dd = domain_select.clone();
        let pattern = pattern_entry.clone();
        let target = target_entry.clone();
        submit_btn.connect_clicked(move |_| {
            let local_domain = match dropdown_selected_string(&domain_dd) {
                Some(d) => d,
                None => return, // no hosted domain to forward from yet
            };
            let pat = pattern.text().to_string().trim().to_string();
            let tgt = target.text().to_string().trim().to_string();
            if pat.is_empty() || tgt.is_empty() {
                return;
            }
            client.create_forwarder(local_domain, pat, tgt);
            // Clear the inputs; the refetched snapshot re-renders the list with
            // the new forwarder (or surfaces an error to the action-error label).
            pattern.set_text("");
            target.set_text("");
        });
    }

    forwarders_section.append(&add_form);
    outer.append(&forwarders_section);

    // Dedicated action-error label (admin.md § Errors — forwarder action
    // failures route here, e.g. `conflicts_with_existing_alias`). Placed above
    // the vexpanding list so it gets a natural height and shows reliably (the
    // same allocation gotcha the DNS page documents).
    let action_error = gtk::Label::new(None);
    action_error.set_wrap(true);
    action_error.set_halign(gtk::Align::Start);
    action_error.set_margin_start(12);
    action_error.set_margin_end(12);
    action_error.add_css_class("error");
    action_error.set_visible(false);
    crate::testid::set_test_id(&action_error, ids::ADMIN_ALIASES_ACTION_ERROR);
    outer.append(&action_error);

    // Per-page generic `error-message` (cross-app rule #2). Present but only
    // the dedicated action-error carries forwarder failures.
    let error_label = build_error_label();
    outer.append(&error_label);

    let forwarders_list = gtk::ListBox::new();
    forwarders_list.set_selection_mode(gtk::SelectionMode::None);
    forwarders_list.add_css_class("boxed-list");
    forwarders_list.set_margin_start(8);
    forwarders_list.set_margin_end(8);
    forwarders_list.set_margin_bottom(8);
    crate::testid::set_test_id(&forwarders_list, ids::ADMIN_ALIASES_FORWARDER_LIST);

    let placeholder = adw::StatusPage::builder()
        .title(admin::aliases_page::NO_FORWARDERS)
        .icon_name("mail-forward-symbolic")
        .build();
    forwarders_list.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&forwarders_list)
        .build();
    outer.append(&scrolled);

    AdminAliasesPage {
        page: outer,
        forwarders_list,
        forwarder_add_domain: domain_select,
        forwarders_action_error: action_error,
    }
}

/// One forwarder row (`admin-aliases-forwarder-list[i]`): the source address +
/// the external target + a delete button. The indexed `admin-aliases-forwarder-
/// row-*` ids ride the labels/button (a bare row Box wouldn't surface them).
fn build_forwarder_row(
    view: &fauna_client_mail_settings::forwarders::ForwarderView,
    client: &Rc<FaunaClient>,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&view.address);
    row.set_subtitle(&view.forward_target);

    let address_label = gtk::Label::new(Some(&view.address));
    address_label.add_css_class("monospace");
    address_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&address_label, ids::ADMIN_ALIASES_FORWARDER_ROW_ADDRESS);
    row.add_prefix(&address_label);

    let target_label = gtk::Label::new(Some(&view.forward_target));
    target_label.add_css_class("monospace");
    target_label.add_css_class("fauna-muted");
    target_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&target_label, ids::ADMIN_ALIASES_FORWARDER_ROW_TARGET);
    row.add_suffix(&target_label);

    let delete_btn = gtk::Button::with_label(admin::aliases_page::DELETE_FORWARDER);
    delete_btn.add_css_class("destructive-action");
    delete_btn.set_valign(gtk::Align::Center);
    crate::testid::set_test_id(&delete_btn, ids::ADMIN_ALIASES_FORWARDER_ROW_DELETE_BUTTON);
    // tui's `admin::Action::DeleteForwarder`.
    crate::offline_gate::declare_wire_kind(&delete_btn, "fauna.bridges.delete_forwarder");
    row.add_suffix(&delete_btn);

    {
        let client = Rc::clone(client);
        let alias_id_hex = view.alias_id_hex.clone();
        delete_btn.connect_clicked(move |_| {
            client.delete_forwarder(alias_id_hex.clone());
        });
    }

    row
}

/// Re-render the `admin-aliases` forwarder section from a [`ForwardersSnapshot`]
/// (`AdminForwardersLoaded`): rebuild the indexed forwarder list, repopulate the
/// add-form's hosted-domain picker, and route any action error to
/// `admin-aliases-action-error`.
pub fn set_forwarders_snapshot(
    handles: &AdminHandles,
    snapshot: &fauna_client_mail_settings::forwarders::ForwardersSnapshot,
    client: &Rc<FaunaClient>,
) {
    // Rebuild the forwarder list.
    clear_list_box(&handles.forwarders_list);
    for view in &snapshot.forwarders {
        handles
            .forwarders_list
            .append(&build_forwarder_row(view, client));
    }

    // Repopulate the hosted-domain picker, preserving the prior selection if it
    // still exists (so a refetch mid-edit doesn't reset the admin's choice).
    let prev = dropdown_selected_string(&handles.forwarder_add_domain);
    let refs: Vec<&str> = snapshot.local_domains.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&refs);
    handles.forwarder_add_domain.set_model(Some(&model));
    if let Some(prev) = prev
        && let Some(i) = snapshot.local_domains.iter().position(|d| *d == prev)
    {
        handles.forwarder_add_domain.set_selected(i as u32);
    }

    render_page_error(&handles.forwarders_action_error, snapshot.error.as_deref());
}

// ---------------------------------------------------------------------------
// DNS sub-page (`admin-dns` — unified DNS-management page, manual-mode read+verify)
// ---------------------------------------------------------------------------

/// Handles for the primary-domain-rename UX on `admin-dns`
/// (mail-primary-domain-rename.md § UX surface). Built once by [`build_rename_ui`];
/// the per-row rename/promote buttons (rebuilt each render in
/// [`build_domain_section`]) reveal the `sheet`, and [`render_admin_dns`]
/// repopulates the target `select` (+ its parallel `targets` name→id map) and
/// rebuilds the deployment-wide `banner` from the snapshot's `active_rename`.
/// Every field is a cheap ref-clone (GTK object / `Rc`), so it's passed by value
/// into `build_domain_section`.
/// Parallel `(display name, 16-byte domain_id)` picker targets, shared by
/// [`RenameUi::targets`] and its build site.
type RenameTargets = Rc<RefCell<Vec<(String, Vec<u8>)>>>;

#[derive(Clone)]
pub struct RenameUi {
    /// Deployment-wide in-flight banner container (page-level; cleared +
    /// repopulated each render — empty ⟺ no active rename, so
    /// `count(admin-dns-rename-banner) == 0`).
    pub banner: gtk::Box,
    /// The start-a-rename wizard sheet (built once; visibility-toggled by the
    /// per-row rename/promote buttons + its own Cancel — the add-domain-form
    /// idiom, so it survives the list's per-render rebuilds).
    pub sheet: gtk::Box,
    /// The sheet's new-primary target picker; its string model + `targets` are
    /// repopulated from the active non-primary domains each render.
    pub select: gtk::DropDown,
    /// The sheet's optional grace-days override entry (blank → nest default 7).
    pub grace_input: gtk::Entry,
    /// Parallel `(display name, 16-byte domain_id)` for the picker options, so
    /// submit/promote map the selected name → its id for `StartPrimaryRename`.
    /// Repopulated alongside the picker each render.
    pub targets: RenameTargets,
}

/// Build the `admin-dns` sub-page: a heading + the add-domain form + a boxed
/// list of per-domain DNS sections (and the soft-deleted group). The list is
/// rebuilt on every snapshot update (`render_admin_dns`), which merges the
/// `LocalDomainsSnapshot` (domain set + `is_primary` + soft-deleted) with the
/// `DnsSnapshot` record matrix by domain name. Mirrors the bridges-pending
/// sub-page's structure; the add-form is built once (not rebuilt per render).
///
/// This is the unified domain-management surface (decided 2026-05-25): domain
/// add/remove/restore live here, not in a separate settings section. The
/// managed-mode controls are all client-held (`client-holds-provider-keys-not-nest`):
/// the `admin-dns-manage-all-toggle` deployment master switch + the held
/// DNS-provider credential block (`admin-dns-credentials-list` + the write-only
/// add-credential form) drive the shared `DnsManagementMachine`, which seals the
/// credential into `fauna.state.dns` and publishes via `fauna-provisioning` —
/// the nest never sees the key. Returns the page root, the records `ListBox`, the
/// page `error-message` label, the held-credential `Box`, the manage-all
/// `ToggleButton`, and the guard that suppresses spurious dispatches when
/// `render_admin_dns` syncs the toggle's reflective state.
#[allow(clippy::type_complexity)]
fn build_dns_page(
    client: &Rc<FaunaClient>,
) -> (
    gtk::Box,
    gtk::ListBox,
    gtk::Label,
    gtk::Box,
    gtk::ToggleButton,
    Rc<std::cell::Cell<bool>>,
    RenameUi,
    gtk::Label,
    gtk::Box,
    Rc<std::cell::Cell<bool>>,
) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let heading = gtk::Label::new(Some(admin::dns::TITLE));
    heading.add_css_class("title-2");
    heading.set_margin_top(16);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&heading, ids::PAGE_HEADING);
    outer.append(&heading);

    let desc = gtk::Label::new(Some(admin::dns::DESCRIPTION));
    desc.add_css_class("dim-label");
    desc.set_wrap(true);
    desc.set_halign(gtk::Align::Start);
    desc.set_margin_start(12);
    desc.set_margin_end(12);
    desc.set_margin_bottom(8);
    outer.append(&desc);

    // Primary-domain-rename UX (mail-primary-domain-rename.md § UX surface): the
    // deployment-wide in-flight banner + the start-a-rename wizard sheet. Both are
    // page-level singletons built once here (near the top, mirroring web); the
    // banner starts empty + the sheet starts hidden — `render_admin_dns` fills
    // them from the snapshot's `active_rename`.
    let rename_ui = build_rename_ui(client);
    outer.append(&rename_ui.banner);
    outer.append(&rename_ui.sheet);

    let (add_domain_form, dns_add_domain_warning, dns_add_domain_form, dns_adding_first_domain) =
        build_add_domain_form(client);
    outer.append(&add_domain_form);

    // Deployment "Fauna controls all domains" master switch: opts every active
    // domain into Fauna-managed DNS at once (dns-management.md § The two modes;
    // the stored state stays per-domain). A labelled `ToggleButton` (not a bare
    // Switch) to match web's labelled `admin-dns-manage-all-toggle` button — the
    // uniform cross-app shape. Its active state is *reflective* (active ⟺ ≥1
    // domain and all are effectively managed) and re-synced by `render_admin_dns`;
    // the shared `manage_all_guard` Cell makes that programmatic `set_active` skip
    // the toggled handler so it doesn't re-dispatch `dns_set_all_managed`.
    let manage_all = gtk::ToggleButton::with_label(admin::dns::MANAGE_ALL);
    manage_all.set_halign(gtk::Align::Start);
    manage_all.set_margin_start(12);
    manage_all.set_margin_end(12);
    manage_all.set_margin_bottom(8);
    crate::testid::set_test_id(&manage_all, ids::ADMIN_DNS_MANAGE_ALL_TOGGLE);
    let manage_all_guard = Rc::new(std::cell::Cell::new(false));
    {
        let client = Rc::clone(client);
        let guard = Rc::clone(&manage_all_guard);
        manage_all.connect_toggled(move |btn| {
            if guard.get() {
                return; // reflective state sync from render_admin_dns, not a user click
            }
            client.dns_set_all_managed(btn.is_active());
        });
    }
    outer.append(&manage_all);

    // --- Managed-mode DNS-provider credentials section + page refresh.
    // The "Fauna controls DNS" credential store is client-held (`fauna.state.dns`,
    // never on the nest — `client-holds-provider-keys-not-nest`); the held list is
    // rebuilt from `DnsSnapshot.credentials` in `render_admin_dns`. The refresh
    // button re-runs the full fetch (records + verify + credential reload). ---
    let creds_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    creds_section.set_margin_start(12);
    creds_section.set_margin_end(12);
    creds_section.set_margin_bottom(8);

    let creds_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let creds_title = gtk::Label::new(Some(admin::dns::CREDENTIALS_TITLE));
    creds_title.add_css_class("title-4");
    creds_title.set_halign(gtk::Align::Start);
    creds_title.set_hexpand(true);
    creds_title.set_xalign(0.0);
    creds_header.append(&creds_title);

    let refresh_btn = gtk::Button::with_label(admin::dns::REFRESH);
    refresh_btn.set_halign(gtk::Align::End);
    crate::testid::set_test_id(&refresh_btn, ids::ADMIN_DNS_REFRESH_BUTTON);
    {
        let client = Rc::clone(client);
        refresh_btn.connect_clicked(move |_| client.fetch_dns_records());
    }
    creds_header.append(&refresh_btn);
    creds_section.append(&creds_header);

    // Held-credential list (indexed `admin-dns-credential-item` rows). Always
    // present (container carries the test id); empty until a credential is held.
    // `accessible_role(Group)` so the container Box is reliably discoverable by
    // ID in the AT-SPI tree (a plain Box defaults to role Generic, which the
    // Linux AT-SPI bridge may omit — same pattern as onboarding's
    // `dns-provider-row`).
    let credentials_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    crate::testid::set_test_id(&credentials_list, ids::ADMIN_DNS_CREDENTIALS_LIST);
    creds_section.append(&credentials_list);

    // Write-only add-credential form (reveal → provider buttons → field entries
    // → verify+store). Built once so it keeps state across list refreshes.
    creds_section.append(&build_add_credential_form(client));

    outer.append(&creds_section);

    // Per-page `error-message` (rule #2). Shows `LocalDomainsSnapshot.error`
    // (add/remove/restore feedback, e.g. `cannot_remove_primary_domain`) or
    // `DnsSnapshot.error` (list_records / verify_records / PutCredentials verify)
    // via `render_admin_dns`. **Placed ABOVE the vexpand records list:** as the
    // last child after a `vexpand(true)` ScrolledWindow the label is allocated
    // zero height (the scrolled window eats all vertical space), so although it
    // is mapped with text set, AT-SPI never marks it SHOWING and the e2e
    // `is_visible("error-message")` reads false. Above the expanding region it
    // gets its natural height and shows reliably.
    let error_label = build_error_label();
    outer.append(&error_label);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    list_box.set_margin_start(8);
    list_box.set_margin_end(8);
    list_box.set_margin_bottom(8);

    let placeholder = adw::StatusPage::builder()
        .title(admin::dns::EMPTY)
        .description(admin::dns::EMPTY_DESC)
        .icon_name("network-server-symbolic")
        .build();
    list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();
    outer.append(&scrolled);

    (
        outer,
        list_box,
        error_label,
        credentials_list,
        manage_all,
        manage_all_guard,
        rename_ui,
        dns_add_domain_warning,
        dns_add_domain_form,
        dns_adding_first_domain,
    )
}

/// Build the page-level primary-domain-rename widgets (built once; see
/// [`RenameUi`]). The `banner` container starts empty (`render_admin_dns` fills
/// it from `active_rename`); the `sheet` starts hidden (the per-row rename/promote
/// buttons reveal it — the add-domain-form idiom, so it keeps state across the
/// list's per-render rebuilds). Submit maps the picker's selection → its parallel
/// `domain_id` in `targets` + the optional grace override, dispatches
/// `StartPrimaryRename`, and hides the sheet; Cancel just hides it. The nest
/// validates every precondition (single-active-rename / new-primary-is-additional
/// / cert-mode / TLS-posture / SAN-cap) — a refusal surfaces via the page
/// error-message (the "guard is the nest's" pattern).
fn build_rename_ui(client: &Rc<FaunaClient>) -> RenameUi {
    // Deployment-wide in-flight banner container: page-level, cleared +
    // repopulated each render (empty ⟺ no active rename). `accessible_role(Group)`
    // so the container is reliably discoverable in the AT-SPI tree even while
    // empty (same reason as `admin-dns-credentials-list`).
    let banner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    banner.set_margin_start(12);
    banner.set_margin_end(12);

    // The start-a-rename wizard sheet — built once, hidden until a row's
    // rename/promote button reveals it.
    let sheet = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sheet.set_margin_start(12);
    sheet.set_margin_end(12);
    sheet.set_margin_bottom(8);
    sheet.set_visible(false);
    crate::testid::set_test_id(&sheet, ids::ADMIN_DNS_RENAME_SHEET);

    let title = gtk::Label::new(Some(admin::dns::rename::SHEET_TITLE));
    title.add_css_class("title-4");
    title.set_halign(gtk::Align::Start);
    sheet.append(&title);

    let new_primary_label = gtk::Label::new(Some(admin::dns::rename::NEW_PRIMARY_LABEL));
    new_primary_label.add_css_class("dim-label");
    new_primary_label.set_halign(gtk::Align::Start);
    sheet.append(&new_primary_label);

    // Target picker: an empty DropDown; `render_admin_dns` fills its string model
    // + the parallel `targets` map from the active non-primary domains (the
    // two-step rule — the wizard never adds a domain).
    let select = gtk::DropDown::from_strings(&[]);
    select.add_css_class("flat");
    select.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&select, ids::ADMIN_DNS_RENAME_NEW_PRIMARY_SELECT);
    sheet.append(&select);

    let grace_label = gtk::Label::new(Some(admin::dns::rename::GRACE_DAYS_LABEL));
    grace_label.add_css_class("dim-label");
    grace_label.set_halign(gtk::Align::Start);
    sheet.append(&grace_label);

    let grace_input = gtk::Entry::new();
    grace_input.set_input_purpose(gtk::InputPurpose::Digits);
    grace_input.set_halign(gtk::Align::Start);
    grace_input.set_max_width_chars(4);
    crate::testid::set_test_id(&grace_input, ids::ADMIN_DNS_RENAME_GRACE_DAYS_INPUT);
    sheet.append(&grace_input);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let submit = gtk::Button::with_label(admin::dns::rename::SUBMIT);
    submit.add_css_class("suggested-action");
    crate::testid::set_test_id(&submit, ids::ADMIN_DNS_RENAME_SUBMIT_BUTTON);
    // tui's `admin::Action::SubmitDnsRename`.
    crate::offline_gate::declare_wire_kind(&submit, "fauna.bridges.start_primary_domain_rename");
    actions.append(&submit);

    let cancel = gtk::Button::with_label(admin::dns::rename::CANCEL);
    crate::testid::set_test_id(&cancel, ids::ADMIN_DNS_RENAME_CANCEL_BUTTON);
    actions.append(&cancel);

    sheet.append(&actions);

    let targets: RenameTargets = Rc::new(RefCell::new(Vec::new()));

    // Submit: map the picker's selection → its parallel domain_id, read the
    // optional grace override (blank/invalid → `None` = nest default 7), dispatch
    // StartPrimaryRename, hide the sheet. No selectable target → no-op (an empty
    // DropDown's `selected()` is GTK_INVALID_LIST_POSITION, so `get` misses).
    {
        let client = Rc::clone(client);
        let sheet = sheet.clone();
        let select = select.clone();
        let grace_input = grace_input.clone();
        let targets = Rc::clone(&targets);
        submit.connect_clicked(move |_| {
            let idx = select.selected() as usize;
            let Some((_, domain_id)) = targets.borrow().get(idx).cloned() else {
                return;
            };
            let grace_days = grace_input.text().trim().parse::<i64>().ok();
            client.start_primary_rename(domain_id, grace_days);
            sheet.set_visible(false);
        });
    }

    // Cancel: hide without starting a rename.
    {
        let sheet = sheet.clone();
        cancel.connect_clicked(move |_| sheet.set_visible(false));
    }

    RenameUi {
        banner,
        sheet,
        select,
        grace_input,
        targets,
    }
}

/// Build the deployment-wide in-flight primary-domain-rename banner
/// (`admin-dns-rename-banner`; mail-primary-domain-rename.md § UX surface) — a
/// dumb render of the projected [`PrimaryDomainRenameView`]. Complete/Abort are
/// reveal-then-confirm (the confirm copy names the cache-flush / inverse-re-flip
/// cost); Extend pushes the grace window out by N days. Every action is gated on
/// the view's `can_*` flags — the nest owns state advancement + validation, so a
/// refusal surfaces via the page error-message. Rebuilt each render, so the
/// transient reveal state resets after the resulting snapshot re-render.
///
/// [`PrimaryDomainRenameView`]: fauna_client_mail_settings::PrimaryDomainRenameView
fn build_rename_banner(
    view: &fauna_client_mail_settings::PrimaryDomainRenameView,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    let banner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    banner.add_css_class("card");
    banner.set_margin_top(8);
    banner.set_margin_bottom(8);
    crate::testid::set_test_id(&banner, ids::ADMIN_DNS_RENAME_BANNER);

    // Head: title + old → new.
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::new(Some(admin::dns::rename::BANNER_TITLE));
    title.add_css_class("title-4");
    title.set_halign(gtk::Align::Start);
    head.append(&title);
    let arrow = gtk::Label::new(Some(&format!(
        "{} → {}",
        view.old_primary_domain, view.new_primary_domain
    )));
    arrow.add_css_class("monospace");
    head.append(&arrow);
    banner.append(&head);

    // Meta: state + (post-flip) grace countdown.
    let meta = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let state = gtk::Label::new(Some(&format!(
        "{} {}",
        admin::dns::rename::STATE_LABEL,
        view.state
    )));
    state.add_css_class("dim-label");
    meta.append(&state);
    if view.is_post_flip_active
        && let Some(ends_at) = view.grace_ends_at
    {
        let grace = gtk::Label::new(Some(&format!(
            "{} {}",
            admin::dns::rename::GRACE_ENDS,
            grace_remaining(ends_at)
        )));
        grace.add_css_class("dim-label");
        meta.append(&grace);
    }
    banner.append(&meta);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    // Complete (reveal → confirm). Shown when can_complete || can_force_complete;
    // `force` iff can_force_complete (still in `grace`, before the window elapses).
    if view.can_complete || view.can_force_complete {
        let complete_btn = gtk::Button::with_label(admin::dns::rename::COMPLETE);
        crate::testid::set_test_id(&complete_btn, ids::ADMIN_DNS_RENAME_COMPLETE_BUTTON);
        let confirm_btn = gtk::Button::with_label(admin::dns::rename::COMPLETE_CONFIRM);
        confirm_btn.add_css_class("suggested-action");
        confirm_btn.set_visible(false);
        crate::testid::set_test_id(&confirm_btn, ids::ADMIN_DNS_RENAME_COMPLETE_CONFIRM_BUTTON);
        // The reveal-only `complete_btn` arms this; the confirm is the dispatch
        // (tui's `admin::Action::ConfirmDnsRenameComplete`).
        crate::offline_gate::declare_wire_kind(
            &confirm_btn,
            "fauna.bridges.complete_primary_domain_rename",
        );
        // Early-complete warning, revealed alongside the confirm (force only).
        let warn = gtk::Label::new(Some(admin::dns::rename::COMPLETE_FORCE_WARNING));
        warn.add_css_class("warning");
        warn.set_wrap(true);
        warn.set_visible(false);
        {
            let confirm_btn = confirm_btn.clone();
            let warn = warn.clone();
            let force = view.can_force_complete;
            complete_btn.connect_clicked(move |btn| {
                btn.set_visible(false);
                confirm_btn.set_visible(true);
                if force {
                    warn.set_visible(true);
                }
            });
        }
        {
            let client = Rc::clone(client);
            let rename_id = view.rename_id.clone();
            let force = view.can_force_complete;
            confirm_btn.connect_clicked(move |_| {
                client.complete_primary_rename(rename_id.clone(), force);
            });
        }
        actions.append(&complete_btn);
        actions.append(&confirm_btn);
        actions.append(&warn);
    }

    // Extend (input + button) — push grace_ends_at out by N days.
    if view.can_extend {
        let extend_input = gtk::Entry::new();
        extend_input.set_input_purpose(gtk::InputPurpose::Digits);
        extend_input.set_text("7");
        extend_input.set_max_width_chars(3);
        crate::testid::set_test_id(&extend_input, ids::ADMIN_DNS_RENAME_EXTEND_DAYS_INPUT);
        let extend_btn = gtk::Button::with_label(admin::dns::rename::EXTEND);
        crate::testid::set_test_id(&extend_btn, ids::ADMIN_DNS_RENAME_EXTEND_BUTTON);
        // tui's `admin::Action::ExtendDnsRenameGrace`.
        crate::offline_gate::declare_wire_kind(
            &extend_btn,
            "fauna.bridges.extend_primary_domain_rename_grace",
        );
        {
            let client = Rc::clone(client);
            let rename_id = view.rename_id.clone();
            let extend_input = extend_input.clone();
            extend_btn.connect_clicked(move |_| {
                let n = extend_input.text().trim().parse::<i64>().unwrap_or(0);
                if n >= 1 {
                    client.extend_primary_rename_grace(rename_id.clone(), n);
                }
            });
        }
        actions.append(&extend_input);
        actions.append(&extend_btn);
    }

    // Abort (reveal → confirm). The post-flip confirm names the inverse-re-flip
    // cost; pre-flip abort is cheap (nothing to unwind).
    if view.can_abort {
        let abort_btn = gtk::Button::with_label(admin::dns::rename::ABORT);
        abort_btn.add_css_class("destructive-action");
        crate::testid::set_test_id(&abort_btn, ids::ADMIN_DNS_RENAME_ABORT_BUTTON);
        let confirm_btn = gtk::Button::with_label(admin::dns::rename::ABORT_CONFIRM);
        confirm_btn.add_css_class("destructive-action");
        confirm_btn.set_visible(false);
        crate::testid::set_test_id(&confirm_btn, ids::ADMIN_DNS_RENAME_ABORT_CONFIRM_BUTTON);
        // The reveal-only `abort_btn` arms this; the confirm is the dispatch
        // (tui's `admin::Action::ConfirmDnsRenameAbort`).
        crate::offline_gate::declare_wire_kind(
            &confirm_btn,
            "fauna.bridges.abort_primary_domain_rename",
        );
        let warn = gtk::Label::new(Some(admin::dns::rename::ABORT_POSTFLIP_WARNING));
        warn.add_css_class("warning");
        warn.set_wrap(true);
        warn.set_visible(false);
        {
            let confirm_btn = confirm_btn.clone();
            let warn = warn.clone();
            let post_flip = !view.is_pre_flip;
            abort_btn.connect_clicked(move |btn| {
                btn.set_visible(false);
                confirm_btn.set_visible(true);
                if post_flip {
                    warn.set_visible(true);
                }
            });
        }
        {
            let client = Rc::clone(client);
            let rename_id = view.rename_id.clone();
            confirm_btn.connect_clicked(move |_| {
                client.abort_primary_rename(rename_id.clone(), None);
            });
        }
        actions.append(&abort_btn);
        actions.append(&confirm_btn);
        actions.append(&warn);
    }

    banner.append(&actions);
    banner
}

/// Client-rendered countdown to a rename's `grace_ends_at` (epoch-millis). The
/// rename *state* is nest-authoritative; only this *display* is client-side.
/// Routes through the shared `fauna_core::format::grace_countdown`
/// (value-formatting.md § Grace countdown); `None` means past the deadline,
/// render the elapsed label.
fn grace_remaining(ends_at_ms: i64) -> String {
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    crate::i18n::grace_countdown(ends_at_ms, now_ms)
        .unwrap_or_else(|| admin::dns::rename::GRACE_ELAPSED.to_string())
}

/// Build the always-present add-domain form: a reveal button
/// (`admin-dns-add-domain-button`) that toggles an inline row with the
/// domain-name entry (`admin-dns-add-domain-input`) + submit/cancel buttons.
/// Submit dispatches `add_local_domain` (the shared `LocalDomainMachine`), then
/// clears + hides the form; the resulting `AdminLocalDomainsLoaded` re-renders
/// the list with the new domain. Built once (not rebuilt per render), so it
/// keeps focus/state across list refreshes.
fn build_add_domain_form(
    client: &Rc<FaunaClient>,
) -> (gtk::Box, gtk::Label, gtk::Box, Rc<std::cell::Cell<bool>>) {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 4);
    container.set_margin_start(12);
    container.set_margin_end(12);
    container.set_margin_bottom(8);

    let add_btn = gtk::Button::with_label(admin::dns::ADD_DOMAIN);
    add_btn.add_css_class("suggested-action");
    add_btn.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&add_btn, ids::ADMIN_DNS_ADD_DOMAIN_BUTTON);
    container.append(&add_btn);

    // A domainless nest's first add is a one-way door — say so before the
    // submit that makes it irreversible (`deployment-home-with-public-relay.md`
    // § MUA reach; `mail-multidomain.md` § Removing a local domain). Untagged
    // chrome (no ui.yaml id, mirroring `desc` above); `adding_first_domain` is
    // cached in `warning_shown_for` at each `render_admin_dns` so the reveal
    // click (below) can decide visibility without re-reading the snapshot.
    let warning = gtk::Label::new(Some(admin::dns::ADD_DOMAIN_PRIMARY_WARNING));
    warning.set_wrap(true);
    warning.set_halign(gtk::Align::Start);
    warning.set_visible(false);
    container.append(&warning);
    let warning_shown_for = Rc::new(std::cell::Cell::new(false));

    // Inline form, hidden until the add button is clicked.
    let form = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    form.set_visible(false);

    let input = gtk::Entry::new();
    input.set_hexpand(true);
    input.set_placeholder_text(Some(admin::dns::ADD_DOMAIN_PLACEHOLDER));
    crate::testid::set_test_id(&input, ids::ADMIN_DNS_ADD_DOMAIN_INPUT);
    form.append(&input);

    let submit = gtk::Button::with_label(admin::dns::ADD_DOMAIN_SUBMIT);
    submit.add_css_class("suggested-action");
    crate::testid::set_test_id(&submit, ids::ADMIN_DNS_ADD_DOMAIN_SUBMIT_BUTTON);
    // tui's `admin::Action::SubmitDnsAddDomain`.
    crate::offline_gate::declare_wire_kind(&submit, "fauna.bridges.add_local_domain");
    form.append(&submit);

    let cancel = gtk::Button::with_label(common::CANCEL);
    crate::testid::set_test_id(&cancel, ids::ADMIN_DNS_ADD_DOMAIN_CANCEL_BUTTON);
    form.append(&cancel);

    container.append(&form);

    // Add button reveals the form (and hides itself). The warning follows the
    // form's open state, shown only when the cached snapshot says this add
    // would be the first (`render_admin_dns` keeps `warning_shown_for` current).
    {
        let form_ref = form.clone();
        let input_ref = input.clone();
        let warning_ref = warning.clone();
        let shown_for = Rc::clone(&warning_shown_for);
        add_btn.connect_clicked(move |btn| {
            btn.set_visible(false);
            form_ref.set_visible(true);
            warning_ref.set_visible(shown_for.get());
            input_ref.grab_focus();
        });
    }

    // Cancel clears + hides the form, restoring the add button.
    {
        let form_ref = form.clone();
        let input_ref = input.clone();
        let add_ref = add_btn.clone();
        let warning_ref = warning.clone();
        cancel.connect_clicked(move |_| {
            input_ref.set_text("");
            form_ref.set_visible(false);
            warning_ref.set_visible(false);
            add_ref.set_visible(true);
        });
    }

    // Submit dispatches add_local_domain with the typed (trimmed, lowercased)
    // domain, then resets the form. Empty input is a no-op. `Rc<dyn Fn()>` so
    // both the submit click and the entry's `activate` (Enter) share one body.
    let do_submit: Rc<dyn Fn()> = {
        let client = Rc::clone(client);
        let form_ref = form.clone();
        let input_ref = input.clone();
        let add_ref = add_btn.clone();
        let warning_ref = warning.clone();
        Rc::new(move || {
            let domain = input_ref.text().trim().to_lowercase();
            if domain.is_empty() {
                return;
            }
            client.add_local_domain(domain);
            input_ref.set_text("");
            form_ref.set_visible(false);
            warning_ref.set_visible(false);
            add_ref.set_visible(true);
        })
    };
    {
        let do_submit = Rc::clone(&do_submit);
        submit.connect_clicked(move |_| do_submit());
    }
    // Enter in the entry submits too.
    {
        let do_submit = Rc::clone(&do_submit);
        input.connect_activate(move |_| do_submit());
    }

    (container, warning, form, warning_shown_for)
}

/// Build the always-present write-only add-credential form: a reveal button
/// (`admin-dns-add-credential-button`) toggling a form with per-provider buttons
/// (`admin-dns-add-credential-provider-row[<pid>]`, one per DNS-capable provider),
/// a dynamic field-entry container (`admin-dns-add-credential-form`, rebuilt on
/// provider-select from the provider's `Capability::Dns` fields), and
/// submit/cancel buttons. Submit collects the typed fields into a
/// `Vec<DnsCredentialField>` and dispatches `client.dns_put_credentials` (the
/// shared `DnsManagementMachine` verifies the credential then seals it into
/// `fauna.state.dns`). On a failed verify the page `error-message` shows the
/// rejection and nothing is stored.
///
/// Mirrors onboarding's `dns_config` provider-row + per-field-entry pattern
/// (priority #3 — same concepts) but is self-contained (no `OnboardingMachine`).
/// Like `build_add_domain_form` it is built once (not rebuilt per render), so it
/// keeps half-typed state across credential-list refreshes. `accessible_role(Group)`
/// on the container Boxes so AT-SPI exposes them headless (plain Box → role
/// Generic, which the Linux bridge may omit); the provider buttons + field
/// entries have definite roles regardless.
fn build_add_credential_form(client: &Rc<FaunaClient>) -> gtk::Box {
    use fauna_provisioning::{Capability, PROVIDERS};

    let container = gtk::Box::new(gtk::Orientation::Vertical, 4);
    container.set_margin_top(4);

    let add_btn = gtk::Button::with_label(admin::dns::ADD_CREDENTIAL);
    add_btn.add_css_class("suggested-action");
    add_btn.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&add_btn, ids::ADMIN_DNS_ADD_CREDENTIAL_BUTTON);
    container.append(&add_btn);

    // Inline form, hidden until the reveal button is clicked. GTK4 AT-SPI only
    // exposes *visible* widgets, so the provider buttons / field entries enter
    // the tree on reveal — the e2e flow clicks the reveal button first.
    let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
    form.set_visible(false);

    // Per-provider buttons (one per DNS-capable provider), keyed [<pid>] exactly
    // like onboarding's `dns-provider-row`.
    let provider_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    crate::testid::set_test_id(&provider_row, ids::ADMIN_DNS_ADD_CREDENTIAL_PROVIDER_ROW);
    form.append(&provider_row);

    // Field-entry container, rebuilt on provider-select (mirrors onboarding's
    // `dns-credentials-form`). Empty until a provider is chosen.
    let fields_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    crate::testid::set_test_id(&fields_box, ids::ADMIN_DNS_ADD_CREDENTIAL_FORM);
    form.append(&fields_box);

    // Selected provider id + the live (field_id, Entry) pairs the submit reads.
    let selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let entries: Rc<RefCell<Vec<(String, gtk::Entry)>>> = Rc::new(RefCell::new(Vec::new()));

    for p in PROVIDERS
        .iter()
        .filter(|p| p.capabilities.contains(&Capability::Dns))
    {
        let btn = gtk::Button::with_label(&resolve_i18n(p.display_name_key));
        let pid = p.id.as_str().to_string();
        crate::testid::set_test_id(
            &btn,
            &format!("admin-dns-add-credential-provider-row[{pid}]"),
        );
        {
            let fields_box = fields_box.clone();
            let entries = Rc::clone(&entries);
            let selected = Rc::clone(&selected);
            let pid_for_click = pid.clone();
            btn.connect_clicked(move |_| {
                *selected.borrow_mut() = Some(pid_for_click.clone());
                rebuild_credential_fields(&fields_box, &entries, &pid_for_click);
            });
        }
        provider_row.append(&btn);
    }

    // Submit + cancel row.
    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let submit = gtk::Button::with_label(admin::dns::ADD_CREDENTIAL_SUBMIT);
    submit.add_css_class("suggested-action");
    crate::testid::set_test_id(&submit, ids::ADMIN_DNS_ADD_CREDENTIAL_SUBMIT_BUTTON);
    btn_row.append(&submit);
    let cancel = gtk::Button::with_label(common::CANCEL);
    crate::testid::set_test_id(&cancel, ids::ADMIN_DNS_ADD_CREDENTIAL_CANCEL_BUTTON);
    btn_row.append(&cancel);
    form.append(&btn_row);

    container.append(&form);

    // Reveal: hide the add button, show the form.
    {
        let form_ref = form.clone();
        add_btn.connect_clicked(move |btn| {
            btn.set_visible(false);
            form_ref.set_visible(true);
        });
    }

    // Reset: clear selection + fields, hide the form, restore the add button.
    let reset: Rc<dyn Fn()> = {
        let form_ref = form.clone();
        let add_ref = add_btn.clone();
        let fields_box = fields_box.clone();
        let selected = Rc::clone(&selected);
        let entries = Rc::clone(&entries);
        Rc::new(move || {
            *selected.borrow_mut() = None;
            entries.borrow_mut().clear();
            clear_box(&fields_box);
            form_ref.set_visible(false);
            add_ref.set_visible(true);
        })
    };

    {
        let reset = Rc::clone(&reset);
        cancel.connect_clicked(move |_| reset());
    }

    // Submit: collect (field_id, value), dispatch PutCredentials, reset the form.
    // No provider selected, or any field left blank → no-op (keep the form open
    // so the admin can finish; the provider verify would reject a blank token
    // anyway, so we skip the guaranteed-failing round-trip). The resulting
    // snapshot re-renders the held list (success) or the error-message (verify
    // failure) via `render_admin_dns`.
    {
        let client = Rc::clone(client);
        let selected = Rc::clone(&selected);
        let entries = Rc::clone(&entries);
        let reset = Rc::clone(&reset);
        submit.connect_clicked(move |_| {
            let Some(pid) = selected.borrow().clone() else {
                return;
            };
            let fields: Vec<fauna_client_dns::DnsCredentialField> = entries
                .borrow()
                .iter()
                .map(|(id, e)| fauna_client_dns::DnsCredentialField {
                    id: id.clone(),
                    value: e.text().to_string().into(),
                })
                .collect();
            if fields.is_empty() || fields.iter().any(|f| f.value.trim().is_empty()) {
                return;
            }
            // No label input on the add-form, so the provider id is the label.
            client.dns_put_credentials(pid.clone(), fields, pid);
            reset();
        });
    }

    container
}

/// Rebuild the add-credential form's field entries for the selected provider:
/// one labeled `gtk::Entry` per `Capability::Dns` field (Secret fields masked),
/// the entry's test id being the raw `providers.yaml` field id (e.g. Cloudflare
/// `api-token`) — the same convention onboarding's `generic_provider_form` uses,
/// so the e2e bridge types into `api-token` directly. Records each `(field_id,
/// Entry)` into `entries` for the submit handler to read at dispatch time.
fn rebuild_credential_fields(
    fields_box: &gtk::Box,
    entries: &Rc<RefCell<Vec<(String, gtk::Entry)>>>,
    provider_id: &str,
) {
    use fauna_provisioning::{Capability, FieldType, PROVIDERS};

    clear_box(fields_box);
    entries.borrow_mut().clear();

    let Some(provider) = PROVIDERS.iter().find(|p| p.id.as_str() == provider_id) else {
        return;
    };
    for field in provider
        .fields
        .iter()
        .filter(|f| f.kinds.contains(&Capability::Dns))
    {
        // role(Group) on the row Box so AT-SPI walks into it (plain Box → Generic).
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();

        let label = gtk::Label::new(Some(&resolve_i18n(field.label_key)));
        label.add_css_class("dim-label");
        label.set_halign(gtk::Align::Start);
        label.set_xalign(0.0);
        row.append(&label);

        let entry = gtk::Entry::builder().hexpand(true).build();
        if matches!(field.field_type, FieldType::Secret | FieldType::HostedAuth) {
            entry.set_visibility(false);
        }
        crate::testid::set_test_id(&entry, field.id);
        row.append(&entry);

        fields_box.append(&row);
        entries.borrow_mut().push((field.id.to_string(), entry));
    }
}

/// Map a record's optional verify verdict to its display text + CSS class. A
/// missing verdict (verify not yet returned, or didn't cover this row) reads as
/// "checking" — the neutral no-verdict state, never a false green/red. The
/// verdict → label **key** decision is single-sourced in
/// `fauna_core::format::dns_verdict_label` (passing this record's `VerifyStatus`
/// serde variant name plus what public DNS actually served, resolved through
/// linux's i18n — so a `Mismatch` reads "found 1.2.3.4" rather than dead-ending);
/// the red/green CSS class stays an idiomatic per-app render.
/// value-formatting.md § DNS verdict label.
fn verdict_text_and_class(verdict: Option<&RecordVerdict>) -> (String, &'static str) {
    let (variant, css) = match verdict.map(|v| v.status) {
        Some(VerifyStatus::Ok) => ("Ok", "success"),
        Some(VerifyStatus::Missing) => ("Missing", "error"),
        Some(VerifyStatus::Mismatch) => ("Mismatch", "error"),
        Some(VerifyStatus::Checking) | None => ("Checking", "dim-label"),
    };
    let observed = verdict.map(|v| v.observed.as_slice()).unwrap_or(&[]);
    let text = fauna_core::format::dns_verdict_label(variant, observed)
        .resolve(crate::i18n::strings::lookup);
    (text, css)
}

/// One labelled field row inside a DNS record card. The testid sits on the value
/// label so the e2e bridge reads the text directly (same convention as the
/// bridges-pending card fields).
fn dns_field_row(caption: &str, value: &str, value_testid: &str, monospace: bool) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.set_margin_top(2);
    row.set_margin_bottom(2);

    let cap = gtk::Label::new(Some(caption));
    cap.add_css_class("dim-label");
    cap.set_halign(gtk::Align::Start);
    cap.set_width_chars(8);
    cap.set_xalign(0.0);
    row.append(&cap);

    let val = gtk::Label::new(Some(value));
    val.set_halign(gtk::Align::Start);
    val.set_hexpand(true);
    val.set_xalign(0.0);
    val.set_selectable(true);
    val.set_wrap(true);
    if monospace {
        val.add_css_class("monospace");
    }
    crate::testid::set_test_id(&val, value_testid);
    row.append(&val);

    row
}

/// Render the unified admin-dns page, merging the cached [`LocalDomainsSnapshot`]
/// (the domain set + `is_primary` + soft-deleted, authoritative for the CRUD
/// list) with the [`DnsSnapshot`] record matrix (overlaid per domain by name).
/// One indexed `admin-dns-domain` section per active domain (name + mode +
/// primary-badge / remove-button + its `admin-dns-record` rows), then the
/// soft-deleted `admin-dns-removed-domain` group. Rebuilt on every snapshot
/// update; the add-form is separate (built once in `build_dns_page`).
///
/// Until the local-domains snapshot arrives (both fetches fire on nav), falls
/// back to rendering the DNS snapshot's domains read-only so records appear ASAP.
pub fn render_admin_dns(handles: &AdminHandles, client: &Rc<FaunaClient>) {
    let ld = handles.last_local_domains.borrow();
    let dns = handles.last_dns.borrow();

    // Error: prefer CRUD feedback (local_domains, e.g. cannot_remove_primary),
    // then list/verify (dns).
    let error = ld
        .as_ref()
        .and_then(|s| s.error.as_deref())
        .or_else(|| dns.as_ref().and_then(|s| s.error.as_deref()));
    render_page_error(&handles.dns_records_error, error);

    // First-add irreversibility warning (`deployment-home-with-public-relay.md`
    // § MUA reach): cache the current hint for the reveal click, and correct the
    // warning's own visibility now if the add-domain form happens to already be
    // open (rare — the form always closes on submit, so a re-render mid-open is
    // only a concurrent fetch, e.g. the admin-users list arriving).
    let adding_first_domain = ld.as_ref().map(|s| s.adding_first_domain).unwrap_or(false);
    handles.dns_adding_first_domain.set(adding_first_domain);
    if handles.dns_add_domain_form.is_visible() {
        handles
            .dns_add_domain_warning
            .set_visible(adding_first_domain);
    }

    clear_list_box(&handles.dns_records_list);

    // Record matrix lookup by domain name (from the DNS snapshot).
    let dns_by_name: HashMap<&str, &fauna_client_dns::DomainView> = dns
        .as_ref()
        .map(|s| s.domains.iter().map(|d| (d.domain.as_str(), d)).collect())
        .unwrap_or_default();

    // Per-domain served-cert health lookup (tls-certificates.md § C.4), populated
    // by `RefreshCertStatus` (`fauna.tls.cert_status`). Empty until that read
    // returns; a domain with no entry renders a "checking" badge.
    let cert_by_name: HashMap<&str, &fauna_client_dns::CertStatusRow> = dns
        .as_ref()
        .map(|s| {
            s.cert_statuses
                .iter()
                .map(|c| (c.domain.as_str(), c))
                .collect()
        })
        .unwrap_or_default();

    // Per-domain `_acme-challenge` CNAME delegation lookup (tls-certificates.md
    // § B tier 3, S6b), projected from `DnsConfig.delegations` on `Refresh`.
    let delegation_by_name: HashMap<&str, &fauna_client_dns::DelegationView> = dns
        .as_ref()
        .map(|s| {
            s.delegations
                .iter()
                .map(|d| (d.domain.as_str(), d))
                .collect()
        })
        .unwrap_or_default();

    // The single in-flight manual-paste issuance, if any (tls-certificates.md
    // § B tier 3, S6a): `snapshot.pending_cert` holds at most one suspended order
    // (the machine rejects a second `BeginManualIssueCert`), so its `domain`
    // selects which section renders the paste surface + complete/cancel.
    let pending_cert = dns.as_ref().and_then(|s| s.pending_cert.as_ref());

    // Zones the admin's held credentials cover — the delegate-zone-select
    // options (DelegateRenewal validates a held credential covers the chosen
    // zone). Sorted + deduped; empty disables the delegate affordance.
    let cred_zones: Vec<String> = dns
        .as_ref()
        .map(|s| {
            let mut z: Vec<String> = s
                .credentials
                .iter()
                .flat_map(|c| c.zones.iter().cloned())
                .collect();
            z.sort();
            z.dedup();
            z
        })
        .unwrap_or_default();

    // Actor options for the per-domain catch-all + role-address pickers
    // (`admin-dns-domain-catch-all-select` / `-role-address-<role>-select`):
    // every account on the nest (admin.md § 2 → *Which accounts a picker
    // offers*), mapped by the shared `fauna_client_admin::actor_picker_options`
    // (the two-halves rule's display half). Empty until `AdminUsersLoaded`
    // arrives (its handler re-renders us).
    let actors: Vec<(Vec<u8>, String)> = handles
        .picker_users
        .borrow()
        .as_deref()
        .map(fauna_client_admin::actor_picker_options)
        .unwrap_or_default();

    // Primary-domain-rename state (mail-primary-domain-rename.md § UX surface),
    // threaded into each row's affordances (`build_domain_section`) + the
    // deployment-wide banner (rebuilt below). `rename_available` gates the primary
    // row's "Rename primary" button (the two-step rule); `active_rename` drives
    // the per-row in-flight state + the banner.
    let (rename_available, active_rename) = ld
        .as_ref()
        .map(|s| (s.rename_available, s.active_rename.clone()))
        .unwrap_or((false, None));

    match ld.as_ref() {
        Some(ld) => {
            for d in &ld.active {
                let view = dns_by_name.get(d.domain.as_str()).copied();
                let cert = cert_by_name.get(d.domain.as_str()).copied();
                let delegation = delegation_by_name.get(d.domain.as_str()).copied();
                let pending = pending_cert.filter(|p| p.domain == d.domain);
                handles.dns_records_list.append(&build_domain_section(
                    &d.domain,
                    d.is_primary,
                    rename_available,
                    active_rename.as_ref(),
                    &handles.dns_rename,
                    d.catch_all_actor_id.as_deref(),
                    d.catch_all_cleared_by_succession_at,
                    &d.role_address_overrides,
                    &actors,
                    view,
                    cert,
                    delegation,
                    pending,
                    &cred_zones,
                    client,
                ));
            }
            if !ld.soft_deleted.is_empty() {
                handles
                    .dns_records_list
                    .append(&build_removed_section_header());
            }
            for d in &ld.soft_deleted {
                handles
                    .dns_records_list
                    .append(&build_removed_domain_row(&d.domain, client));
            }
        }
        None => {
            // Local-domains snapshot not in yet — render the DNS domains
            // read-only (no is_primary / catch-all known; the remove-button is
            // enabled but the nest still refuses the primary).
            if let Some(dns) = dns.as_ref() {
                for view in &dns.domains {
                    let cert = cert_by_name.get(view.domain.as_str()).copied();
                    let delegation = delegation_by_name.get(view.domain.as_str()).copied();
                    let pending = pending_cert.filter(|p| p.domain == view.domain);
                    handles.dns_records_list.append(&build_domain_section(
                        &view.domain,
                        false,
                        false,
                        None,
                        &handles.dns_rename,
                        None,
                        None,
                        &[],
                        &actors,
                        Some(view),
                        cert,
                        delegation,
                        pending,
                        &cred_zones,
                        client,
                    ));
                }
            }
        }
    }

    // Repopulate the rename sheet's target picker + its parallel name→id map from
    // the active non-primary domains (the two-step rule — only existing additional
    // domains are promotion targets). Submit/promote resolve the picked name to
    // its 16-byte domain_id via `targets`. Model set before selection so an empty
    // list leaves the picker inert (submit no-ops on GTK_INVALID_LIST_POSITION).
    {
        let targets: Vec<(String, Vec<u8>)> = ld
            .as_ref()
            .map(|s| {
                s.active
                    .iter()
                    .filter(|d| !d.is_primary)
                    .map(|d| (d.domain.clone(), d.domain_id.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let names: Vec<&str> = targets.iter().map(|(n, _)| n.as_str()).collect();
        handles
            .dns_rename
            .select
            .set_model(Some(&gtk::StringList::new(&names)));
        if !names.is_empty() {
            handles.dns_rename.select.set_selected(0);
        }
        *handles.dns_rename.targets.borrow_mut() = targets;
    }

    // Rebuild the deployment-wide in-flight rename banner (empty ⟺ no active
    // rename, so `count(admin-dns-rename-banner) == 0`). A dumb render of
    // `active_rename`; the action buttons are gated on the projected `can_*` flags.
    clear_box(&handles.dns_rename.banner);
    if let Some(r) = active_rename.as_ref() {
        handles
            .dns_rename
            .banner
            .append(&build_rename_banner(r, client));
    }

    // Held DNS-provider credentials (managed-mode "Fauna controls DNS" store).
    // Rebuilt from the DNS snapshot's `credentials` (provider + zones + label,
    // no secrets — client-held, never on the nest). Empty when none is held or
    // the snapshot hasn't arrived (the section container stays present).
    clear_box(&handles.dns_credentials_list);
    let creds = dns
        .as_ref()
        .map(|s| s.credentials.as_slice())
        .unwrap_or_default();
    if creds.is_empty() {
        let empty = gtk::Label::new(Some(admin::dns::CREDENTIALS_EMPTY));
        empty.add_css_class("dim-label");
        empty.set_halign(gtk::Align::Start);
        empty.set_xalign(0.0);
        empty.set_wrap(true);
        handles.dns_credentials_list.append(&empty);
    } else {
        for (i, cred) in creds.iter().enumerate() {
            handles
                .dns_credentials_list
                .append(&build_credential_item(i as u32, cred, client));
        }
    }

    // Reflect the deployment master switch (`admin-dns-manage-all-toggle`):
    // active ⟺ there is ≥1 active domain and every one is effectively managed.
    // Prefer the local-domains active set (authoritative on which domains are
    // active); fall back to the DNS snapshot's own domains when it hasn't arrived.
    // The projection itself is the shared `DnsSnapshot::all_domains_managed`
    // (one source of truth across clients); here we just thread the active set in.
    // Guarded so this programmatic `set_active` doesn't re-enter `dns_set_all_managed`.
    let active_names: Option<Vec<String>> = ld
        .as_ref()
        .map(|ld| ld.active.iter().map(|d| d.domain.clone()).collect());
    let all_managed = dns
        .as_ref()
        .is_some_and(|s| s.all_domains_managed(active_names.as_deref()));
    handles.dns_manage_all_guard.set(true);
    handles.dns_manage_all_toggle.set_active(all_managed);
    handles.dns_manage_all_guard.set(false);
    // (The former `admin-services` DNS toggle — a second reflective view of this
    // same master-switch state — was removed with the Services page, 2026-06-04;
    // `admin-dns-manage-all-toggle` above is now the sole control.)
}

/// The trailing "not loaded" picker option for a designation that isn't among
/// the actors fetched so far (e.g. paginated out) — full un-truncated hex, not
/// a short prefix (admin.md § 2's two-halves rule; row 603: widened from a
/// 4-byte-truncated prefix to match apple's `hexFull`).
fn actor_not_loaded_fallback_label(id: &[u8]) -> String {
    admin::actor_id_fallback_label(&fauna_core::format::hex_full(id))
}

/// Build one active-domain `admin-dns-domain` section: header (name + mode +
/// primary-badge / remove-button), the per-domain catch-all picker, the
/// served-TLS-cert health badge (`admin-dns-cert-status`, tls-certificates.md
/// § C.4), then its `admin-dns-record` rows. `is_primary` rows show
/// `admin-dns-domain-primary-badge` and a disabled remove-button (the primary
/// cannot be removed); non-primary rows get an enabled remove-button. The
/// remove-button is present on every row so its index aligns with
/// `admin-dns-domain-name` for scoped e2e clicks. `cert` is this domain's
/// served-cert status from `RefreshCertStatus` (`None` → a "checking" badge);
/// `delegation` is this domain's `_acme-challenge` CNAME renewal-delegation if
/// one is set (`Some` → render the one-time CNAME + a remove affordance; `None`
/// → offer the reveal-on-demand delegate form); `cred_zones` are the held
/// credentials' covered zones (the delegate-zone-select options).
#[allow(clippy::too_many_arguments)]
fn build_domain_section(
    domain: &str,
    is_primary: bool,
    rename_available: bool,
    active_rename: Option<&fauna_client_mail_settings::PrimaryDomainRenameView>,
    rename_ui: &RenameUi,
    catch_all_actor_id: Option<&[u8]>,
    catch_all_cleared_by_succession_at: Option<i64>,
    role_overrides: &[fauna_client_mail_settings::local_domains::RoleAddressOverrideView],
    actors: &[(Vec<u8>, String)],
    view: Option<&fauna_client_dns::DomainView>,
    cert: Option<&fauna_client_dns::CertStatusRow>,
    delegation: Option<&fauna_client_dns::DelegationView>,
    pending: Option<&fauna_client_dns::PendingCertIssue>,
    cred_zones: &[String],
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    crate::testid::set_test_id(&row, ids::ADMIN_DNS_DOMAIN);

    let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    section.set_margin_top(8);
    section.set_margin_bottom(8);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_margin_start(12);
    header.set_margin_end(12);
    header.set_margin_bottom(4);

    let name = gtk::Label::new(Some(domain));
    name.add_css_class("title-4");
    name.set_halign(gtk::Align::Start);
    name.set_hexpand(true);
    name.set_xalign(0.0);
    crate::testid::set_test_id(&name, ids::ADMIN_DNS_DOMAIN_NAME);
    header.append(&name);

    // Per-domain Fauna-managed / manual toggle (dns-management.md § The two
    // modes). Active ⟺ the shared machine's effective-mode projection is
    // "managed" (opted-in ∧ a held credential covers the domain). Toggling on
    // dispatches SetMode (then an initial Publish) via `dns_set_mode`; opting in
    // without a covering credential is rejected (InvalidState → error-message)
    // and the next render snaps the toggle back to inactive. `set_active` runs
    // before `connect_toggled` so the initial state never spuriously dispatches.
    // A labelled `ToggleButton` whose label *is* the mode text ("Managed"/
    // "Manual") — not a bare Switch — to match web's labelled
    // `admin-dns-domain-mode` button (the uniform cross-app shape) and the
    // `get_text(admin-dns-domain-mode)` → mode-label e2e contract.
    let is_managed = view.is_some_and(fauna_client_dns::DomainView::is_managed);
    let mode = gtk::ToggleButton::with_label(if is_managed {
        admin::dns::MODE_MANAGED
    } else {
        admin::dns::MODE_MANUAL
    });
    mode.set_active(is_managed);
    mode.set_halign(gtk::Align::End);
    crate::testid::set_test_id(&mode, ids::ADMIN_DNS_DOMAIN_MODE);
    {
        let client = Rc::clone(client);
        let domain = domain.to_string();
        mode.connect_toggled(move |btn| client.dns_set_mode(domain.clone(), btn.is_active()));
    }
    header.append(&mode);

    // Per-domain auto-renew checkbox (`admin-dns-domain-auto-renew`,
    // tls-certificates.md § C.3). Shown ONLY for managed/delegated rows — the only
    // kind a synced client can auto-issue; a manual-non-delegated domain can never
    // auto-renew, so the control is absent there (its `auto_renew` is always
    // false). Default-on: a fresh managed/delegated domain renders checked with no
    // admin action. Toggling dispatches `SetAutoRenew` (config-only). `set_active`
    // runs before `connect_toggled` so the initial state never spuriously
    // dispatches. A `state` test-attr mirrors the checked state for the e2e (the
    // label is the constant "Auto-renew", so it can't carry the state).
    if is_managed || delegation.is_some() {
        let on = view.is_some_and(|v| v.auto_renew);
        let auto_renew = gtk::CheckButton::with_label(admin::dns::cert::AUTO_RENEW);
        auto_renew.set_active(on);
        auto_renew.set_halign(gtk::Align::End);
        crate::testid::set_test_id(&auto_renew, ids::ADMIN_DNS_DOMAIN_AUTO_RENEW);
        crate::testid::set_test_attr(&auto_renew, "state", if on { "on" } else { "off" });
        {
            let client = Rc::clone(client);
            let domain = domain.to_string();
            auto_renew.connect_toggled(move |btn| {
                client.dns_set_auto_renew(domain.clone(), btn.is_active());
            });
        }
        header.append(&auto_renew);
    }

    if is_primary {
        let badge = gtk::Label::new(Some(admin::dns::PRIMARY_BADGE));
        badge.add_css_class("dim-label");
        badge.add_css_class("caption");
        crate::testid::set_test_id(&badge, ids::ADMIN_DNS_DOMAIN_PRIMARY_BADGE);
        header.append(&badge);
    }

    let remove_btn = gtk::Button::with_label(admin::dns::REMOVE);
    remove_btn.add_css_class("destructive-action");
    // Disabled on the primary (which cannot be removed); present on every row
    // so the e2e index aligns with admin-dns-domain-name.
    remove_btn.set_sensitive(!is_primary);
    crate::testid::set_test_id(&remove_btn, ids::ADMIN_DNS_DOMAIN_REMOVE_BUTTON);
    // tui's `admin::Action::RemoveDnsDomain`.
    crate::offline_gate::declare_wire_kind(&remove_btn, "fauna.bridges.remove_local_domain");
    {
        let client = Rc::clone(client);
        let domain = domain.to_string();
        remove_btn.connect_clicked(move |_| client.remove_local_domain(domain.clone()));
    }
    header.append(&remove_btn);

    // Primary-domain-rename affordances (mail-primary-domain-rename.md § UX
    // surface). On the PRIMARY row: "Rename primary domain" (insensitive until a
    // non-primary domain exists — the two-step rule — or while a rename already
    // runs) + a read-only in-flight state label during a rename. On each
    // NON-PRIMARY active row: "Promote to primary" (a shortcut pre-targeting this
    // domain; hidden while a rename is already in flight). Both open the shared
    // sheet (built once; `render_admin_dns` keeps its target picker current).
    if is_primary {
        let rename_btn = gtk::Button::with_label(admin::dns::rename::BUTTON);
        rename_btn.set_sensitive(rename_available && active_rename.is_none());
        crate::testid::set_test_id(&rename_btn, ids::ADMIN_DNS_DOMAIN_RENAME_BUTTON);
        {
            let rename_ui = rename_ui.clone();
            rename_btn.connect_clicked(move |_| {
                // Opened from the primary row — no pre-target; the picker keeps
                // its first option (`render_admin_dns` selects index 0). Clear any
                // stale grace override so it defaults to the nest's grace (web
                // resets `renameGraceDays` on open).
                rename_ui.grace_input.set_text("");
                rename_ui.sheet.set_visible(true);
            });
        }
        header.append(&rename_btn);

        if let Some(r) = active_rename {
            let state = gtk::Label::new(Some(&format!(
                "{} {} ({})",
                admin::dns::rename::RENAMING_TO,
                r.new_primary_domain,
                r.state
            )));
            state.add_css_class("dim-label");
            state.add_css_class("caption");
            crate::testid::set_test_id(&state, ids::ADMIN_DNS_DOMAIN_RENAME_STATE);
            header.append(&state);
        }
    } else if active_rename.is_none() {
        let promote_btn = gtk::Button::with_label(admin::dns::rename::PROMOTE);
        crate::testid::set_test_id(&promote_btn, ids::ADMIN_DNS_DOMAIN_PROMOTE_BUTTON);
        {
            let rename_ui = rename_ui.clone();
            let domain = domain.to_string();
            promote_btn.connect_clicked(move |_| {
                // Pre-target this row's domain: find it in the picker's parallel
                // targets and select that index, then reveal the sheet.
                if let Some(i) = rename_ui
                    .targets
                    .borrow()
                    .iter()
                    .position(|(name, _)| *name == domain)
                {
                    rename_ui.select.set_selected(i as u32);
                }
                rename_ui.grace_input.set_text("");
                rename_ui.sheet.set_visible(true);
            });
        }
        header.append(&promote_btn);
    }

    section.append(&header);

    // Per-domain catch-all actor picker (`admin-dns-domain-catch-all-select`;
    // mail-aliases.md § Kind 4, mail-multidomain.md § Per-domain catch-all). A
    // 1-per-domain setting: "None" clears, any actor designates. Rebuilt fresh
    // each render with the initial selection set BEFORE the handler is connected,
    // so the initial `set_selected` never spuriously dispatches.
    let catch_all_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    catch_all_row.set_margin_start(12);
    catch_all_row.set_margin_end(12);
    catch_all_row.set_margin_bottom(4);

    let ca_label = gtk::Label::new(Some(admin::dns::CATCH_ALL_LABEL));
    ca_label.add_css_class("dim-label");
    ca_label.set_halign(gtk::Align::Start);
    catch_all_row.append(&ca_label);

    // Model index 0 = "None" (clear); index i>0 = `actors[i-1]`. `ids` is the
    // parallel actor-id map (`None` at 0). If the current designation isn't among
    // the loaded actors (e.g. paginated out), a trailing entry keeps it visible +
    // selected rather than silently clearing it.
    let mut labels: Vec<String> = Vec::with_capacity(actors.len() + 1);
    let mut ids: Vec<Option<Vec<u8>>> = Vec::with_capacity(actors.len() + 1);
    labels.push(admin::dns::CATCH_ALL_NONE.to_string());
    ids.push(None);
    for (id, label) in actors {
        labels.push(label.clone());
        ids.push(Some(id.clone()));
    }
    let mut selected: u32 = 0;
    if let Some(current) = catch_all_actor_id {
        match actors.iter().position(|(id, _)| id.as_slice() == current) {
            Some(i) => selected = (i + 1) as u32,
            None => {
                labels.push(actor_not_loaded_fallback_label(current));
                ids.push(Some(current.to_vec()));
                selected = (labels.len() - 1) as u32;
            }
        }
    }

    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let picker = gtk::DropDown::from_strings(&label_refs);
    picker.add_css_class("flat");
    picker.set_selected(selected);
    crate::testid::set_test_id(&picker, ids::ADMIN_DNS_DOMAIN_CATCH_ALL_SELECT);
    // tui's `admin::Action::SelectDnsCatchAll`.
    crate::offline_gate::declare_wire_kind(&picker, "fauna.bridges.set_catch_all_actor");
    {
        let client = Rc::clone(client);
        let domain = domain.to_string();
        picker.connect_selected_notify(move |d| {
            let idx = d.selected() as usize;
            if let Some(actor_id) = ids.get(idx).cloned() {
                client.set_catch_all_actor(domain.clone(), actor_id);
            }
        });
    }
    catch_all_row.append(&picker);
    section.append(&catch_all_row);

    // Present only when a SUCCESSION (not an admin) last cleared this domain's
    // catch-all — tells the admin why the picker above reads "none"
    // and that unmatched mail is now bouncing; re-designating via that same
    // picker is the fix. Same read-only-explainer idiom as the rename-state
    // label (`ADMIN_DNS_DOMAIN_RENAME_STATE` above).
    if catch_all_cleared_by_succession_at.is_some() {
        let cleared_state = gtk::Label::new(Some(admin::dns::CATCH_ALL_CLEARED_BY_SUCCESSION));
        cleared_state.add_css_class("dim-label");
        cleared_state.add_css_class("caption");
        cleared_state.set_halign(gtk::Align::Start);
        cleared_state.set_margin_start(12);
        cleared_state.set_margin_end(12);
        cleared_state.set_margin_bottom(4);
        cleared_state.set_wrap(true);
        crate::testid::set_test_id(
            &cleared_state,
            ids::ADMIN_DNS_DOMAIN_CATCH_ALL_CLEARED_STATE,
        );
        section.append(&cleared_state);
    }

    // Per-domain role-address override pickers
    // (`admin-dns-domain-role-address-<role>-select`; mail-multidomain.md
    // § Per-domain role-address routing). One actor dropdown per overridable
    // RFC 2142 / RFC 5321 §4.5.1 role (postmaster/abuse/noc/security): index 0 =
    // "Admin (default)" clears the override (the role falls back to the deployment
    // admin); any actor designates. The same per-row dropdown shape as catch-all,
    // ×4 — the nest atomic-merges so each role is independent. Rebuilt fresh each
    // render with the initial selection set BEFORE the handler connects, so the
    // initial `set_selected` never spuriously dispatches.
    use fauna_client_mail_settings::local_domains::RoleAddressKind;
    let role_header = gtk::Label::new(Some(admin::dns::ROLE_ADDRESS_LABEL));
    role_header.add_css_class("dim-label");
    role_header.set_halign(gtk::Align::Start);
    role_header.set_margin_start(12);
    role_header.set_margin_end(12);
    role_header.set_margin_bottom(2);
    section.append(&role_header);

    for role in RoleAddressKind::ALL {
        let role_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        role_row.set_margin_start(24);
        role_row.set_margin_end(12);
        role_row.set_margin_bottom(4);

        let ra_label = gtk::Label::new(Some(&format!("{}@", role.as_storage_key())));
        ra_label.add_css_class("dim-label");
        ra_label.set_halign(gtk::Align::Start);
        role_row.append(&ra_label);

        let current = role_overrides
            .iter()
            .find(|o| o.role == role)
            .map(|o| o.actor_id.as_slice());

        // Model index 0 = "Admin (default)" (clear → admin); index i>0 =
        // `actors[i-1]`. A current designation not among the loaded actors (e.g.
        // paginated out) gets a trailing entry so it stays visible + selected.
        let mut labels: Vec<String> = Vec::with_capacity(actors.len() + 1);
        let mut ids: Vec<Option<Vec<u8>>> = Vec::with_capacity(actors.len() + 1);
        labels.push(admin::dns::ROLE_ADDRESS_ADMIN_DEFAULT.to_string());
        ids.push(None);
        for (id, label) in actors {
            labels.push(label.clone());
            ids.push(Some(id.clone()));
        }
        let mut selected: u32 = 0;
        if let Some(cur) = current {
            match actors.iter().position(|(id, _)| id.as_slice() == cur) {
                Some(i) => selected = (i + 1) as u32,
                None => {
                    labels.push(actor_not_loaded_fallback_label(cur));
                    ids.push(Some(cur.to_vec()));
                    selected = (labels.len() - 1) as u32;
                }
            }
        }

        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let picker = gtk::DropDown::from_strings(&label_refs);
        picker.add_css_class("flat");
        picker.set_selected(selected);
        crate::testid::set_test_id(
            &picker,
            &format!(
                "admin-dns-domain-role-address-{}-select",
                role.as_storage_key()
            ),
        );
        // tui's `admin::Action::SelectDnsRoleAddress`.
        crate::offline_gate::declare_wire_kind(&picker, "fauna.bridges.set_role_address");
        {
            let client = Rc::clone(client);
            let domain = domain.to_string();
            picker.connect_selected_notify(move |d| {
                let idx = d.selected() as usize;
                if let Some(actor_id) = ids.get(idx).cloned() {
                    client.set_role_address(domain.clone(), role, actor_id);
                }
            });
        }
        role_row.append(&picker);
        section.append(&role_row);
    }

    // Per-domain served-TLS-cert health badge (`admin-dns-cert-status`,
    // tls-certificates.md § C.4): the nest-computed state of the cert the
    // listener actually serves for this domain, sitting just above the
    // per-record red/green checks. A pure read — the same badge renders on
    // every app (web included). `cert` is `None` until `RefreshCertStatus`
    // returns, rendering a "checking" placeholder.
    let cert_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    cert_row.set_margin_start(12);
    cert_row.set_margin_end(12);
    cert_row.set_margin_bottom(4);
    let cert_badge = gtk::Label::new(Some(&cert_status_text(cert)));
    cert_badge.add_css_class("caption");
    cert_badge.add_css_class(cert_status_css(cert));
    cert_badge.set_halign(gtk::Align::Start);
    cert_badge.set_xalign(0.0);
    cert_badge.set_wrap(true);
    crate::testid::set_test_id(&cert_badge, ids::ADMIN_DNS_CERT_STATUS);
    cert_row.append(&cert_badge);
    section.append(&cert_row);

    // Per-domain cert *issuance* (tls-certificates.md § B tier 2/3): the
    // get/renew button (single `IssueCert` for managed/delegated, two-phase
    // `BeginManualIssueCert` for a manual domain) plus, while a manual order is
    // pending, the `_acme-challenge` paste surface + complete/cancel. Native-only.
    let single_issue =
        view.is_some_and(fauna_client_dns::DomainView::is_managed) || delegation.is_some();
    build_cert_issuance(&section, domain, single_issue, pending, client);

    // `_acme-challenge` CNAME renewal-delegation (tls-certificates.md § B tier 3,
    // S6b). When a delegation exists, show the one-time CNAME (the same
    // `admin-dns-record` shape every record uses) + a "renewals automated" label
    // + a remove affordance. Otherwise offer a reveal-on-demand delegate form:
    // pick a controlled zone (from the held credentials' zones) the domain's
    // `_acme-challenge` renewals re-home to. Config-only — renders on web too.
    build_cert_delegation(&section, domain, delegation, cred_zones, client);

    if let Some(view) = view {
        for record in &view.records {
            section.append(&build_dns_record_card(record));
        }
    }

    row.set_child(Some(&section));
    row
}

/// The per-domain served-cert health badge text (`admin-dns-cert-status`,
/// tls-certificates.md § C.4). Owned by `fauna_client_dns::cert_status_text`
/// — see its doc comment for the shared decision + text-assembly split.
use fauna_client_dns::cert_status_text;

/// The libadwaita CSS class colouring the cert-status badge: success (trusted),
/// warning (expiring / on-floor — non-fatal, native apps keep working), or a
/// dim placeholder while the status is still loading.
fn cert_status_css(cert: Option<&fauna_client_dns::CertStatusRow>) -> &'static str {
    use fauna_client_dns::CertHealthState;
    match cert.map(|c| c.state) {
        Some(CertHealthState::ValidTrusted) => "success",
        Some(CertHealthState::Expiring) | Some(CertHealthState::OnFloorRenewNeeded) => "warning",
        None => "dim-label",
    }
}

/// Render the per-domain TLS-cert **issuance** affordances (tls-certificates.md
/// § B tier 2/3). Always shows `admin-dns-cert-issue-button` (get/renew):
/// - `single_issue` (managed or CNAME-delegated) → `dns_issue_cert` runs a single
///   client-driven DNS-01 order (auto-publish + finalize + seal to the nest).
/// - otherwise (manual, no covering credential, undelegated) → `dns_begin_manual_issue`
///   opens the order and surfaces the `_acme-challenge` TXT(s) to paste.
///
/// When a manual order is pending **for this domain** (`pending = Some`), the
/// issue-button is disabled (one order at a time) and the paste surface renders:
/// the challenge TXT row(s) via the shared `admin-dns-record` card, then
/// `admin-dns-cert-complete-button` (`CompleteManualIssueCert`, once the admin has
/// pasted + the record is live) and `admin-dns-cert-cancel-button`
/// (`CancelManualIssueCert`). Native-only — on web these buttons render disabled
/// (the order core is not WASM-safe); linux is native, so they are enabled here.
fn build_cert_issuance(
    section: &gtk::Box,
    domain: &str,
    single_issue: bool,
    pending: Option<&fauna_client_dns::PendingCertIssue>,
    client: &Rc<FaunaClient>,
) {
    let pending_here = pending.is_some_and(|p| p.domain == domain);

    let issue_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    issue_row.set_margin_start(12);
    issue_row.set_margin_end(12);
    issue_row.set_margin_bottom(4);

    let issue_btn = gtk::Button::with_label(admin::dns::cert::ISSUE);
    issue_btn.add_css_class("flat");
    issue_btn.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&issue_btn, ids::ADMIN_DNS_CERT_ISSUE_BUTTON);
    // One order at a time: while a manual order is pending, the get/renew button
    // is inert (the admin completes or cancels the open order first).
    issue_btn.set_sensitive(!pending_here);
    // A composite control (tui's `admin::Action::IssueDnsCert { single_issue }`):
    // the managed/delegated leg runs the whole DNS-01 order and delivers the
    // cert (`fauna.tls.publish_cert`, OnlineOnly); the manual leg only opens
    // the CA order and stashes a breadcrumb — no nest call but the config
    // read/write that follows (`fauna.account.state.put`, OfflineSafe — left
    // undeclared, per rule: declare the leg that BINDS, and only when it is
    // genuinely OnlineOnly). `single_issue` is fixed per this card's render.
    if single_issue {
        crate::offline_gate::declare_wire_kind(&issue_btn, "fauna.tls.publish_cert");
    }
    {
        let client = Rc::clone(client);
        let domain = domain.to_string();
        issue_btn.connect_clicked(move |_| {
            if single_issue {
                client.dns_issue_cert(domain.clone());
            } else {
                client.dns_begin_manual_issue(domain.clone());
            }
        });
    }
    issue_row.append(&issue_btn);
    section.append(&issue_row);

    // Manual paste surface — only while an order for this domain awaits the admin.
    let Some(pc) = pending.filter(|p| p.domain == domain) else {
        return;
    };

    let instr = gtk::Label::new(Some(admin::dns::cert::PASTE_INSTRUCTIONS));
    instr.add_css_class("caption");
    instr.add_css_class("dim-label");
    instr.set_halign(gtk::Align::Start);
    instr.set_xalign(0.0);
    instr.set_wrap(true);
    instr.set_margin_start(12);
    instr.set_margin_end(12);
    section.append(&instr);

    // The transient `_acme-challenge` TXT(s) to paste — the same `admin-dns-record`
    // card every record uses (one record type, one path).
    for challenge in &pc.challenges {
        section.append(&build_dns_record_card(challenge));
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_margin_start(12);
    actions.set_margin_end(12);
    actions.set_margin_bottom(4);

    let complete = gtk::Button::with_label(admin::dns::cert::ISSUE_COMPLETE);
    complete.add_css_class("suggested-action");
    crate::testid::set_test_id(&complete, ids::ADMIN_DNS_CERT_COMPLETE_BUTTON);
    // The manual path's phase 2 — tui's `admin::Action::CompleteDnsManualIssue`
    // — delivers the cert exactly as the managed/delegated leg does.
    crate::offline_gate::declare_wire_kind(&complete, "fauna.tls.publish_cert");
    {
        let client = Rc::clone(client);
        complete.connect_clicked(move |_| client.dns_complete_manual_issue());
    }
    actions.append(&complete);

    let cancel = gtk::Button::with_label(admin::dns::cert::ISSUE_CANCEL);
    cancel.add_css_class("flat");
    crate::testid::set_test_id(&cancel, ids::ADMIN_DNS_CERT_CANCEL_BUTTON);
    {
        let client = Rc::clone(client);
        cancel.connect_clicked(move |_| client.dns_cancel_manual_issue());
    }
    actions.append(&cancel);

    section.append(&actions);
}

/// Render the `_acme-challenge` CNAME renewal-delegation affordance for one domain
/// section (tls-certificates.md § B tier 3, S6b). Two states:
/// - **Delegated** (`delegation = Some`): a "renewals automated" label + the
///   one-time CNAME (the same `admin-dns-record` shape every record uses) +
///   `admin-dns-cert-remove-delegation-button` (→ `RemoveDelegation`).
/// - **Not delegated** (`None`): `admin-dns-cert-delegate-button` revealing an
///   inline form — `admin-dns-cert-delegate-zone-select` (the held credentials'
///   zones) + `-submit-button` (→ `DelegateRenewal{domain, zone}`) +
///   `-cancel-button`. Disabled when no held credential covers any zone (nothing
///   to delegate to). Config-only — renders on every app incl web.
fn build_cert_delegation(
    section: &gtk::Box,
    domain: &str,
    delegation: Option<&fauna_client_dns::DelegationView>,
    cred_zones: &[String],
    client: &Rc<FaunaClient>,
) {
    if let Some(d) = delegation {
        let lbl_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        lbl_row.set_margin_start(12);
        lbl_row.set_margin_end(12);
        let lbl = gtk::Label::new(Some(admin::dns::cert::RENEWALS_AUTOMATED));
        lbl.add_css_class("caption");
        lbl.add_css_class("success");
        lbl.set_halign(gtk::Align::Start);
        lbl.set_hexpand(true);
        lbl.set_xalign(0.0);
        lbl_row.append(&lbl);

        let remove = gtk::Button::with_label(admin::dns::cert::REMOVE_DELEGATION);
        remove.add_css_class("flat");
        crate::testid::set_test_id(&remove, ids::ADMIN_DNS_CERT_REMOVE_DELEGATION_BUTTON);
        {
            let client = Rc::clone(client);
            let domain = domain.to_string();
            remove.connect_clicked(move |_| client.dns_remove_delegation(domain.clone()));
        }
        lbl_row.append(&remove);
        section.append(&lbl_row);

        // The one-time CNAME the admin sets once at their registrar.
        section.append(&build_dns_record_card(&d.cname));
        return;
    }

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_start(12);
    row.set_margin_end(12);

    let delegate_btn = gtk::Button::with_label(admin::dns::cert::DELEGATE);
    delegate_btn.add_css_class("flat");
    crate::testid::set_test_id(&delegate_btn, ids::ADMIN_DNS_CERT_DELEGATE_BUTTON);
    row.append(&delegate_btn);

    // Inline form, hidden until the delegate button is clicked.
    let form = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    form.set_visible(false);

    let zone_label = gtk::Label::new(Some(admin::dns::cert::DELEGATE_ZONE_LABEL));
    zone_label.add_css_class("dim-label");
    form.append(&zone_label);

    let zone_refs: Vec<&str> = cred_zones.iter().map(String::as_str).collect();
    let zone_select = gtk::DropDown::from_strings(&zone_refs);
    crate::testid::set_test_id(&zone_select, ids::ADMIN_DNS_CERT_DELEGATE_ZONE_SELECT);
    form.append(&zone_select);

    let submit = gtk::Button::with_label(admin::dns::cert::DELEGATE_SUBMIT);
    submit.add_css_class("suggested-action");
    crate::testid::set_test_id(&submit, ids::ADMIN_DNS_CERT_DELEGATE_SUBMIT_BUTTON);
    form.append(&submit);

    let cancel = gtk::Button::with_label(admin::dns::cert::DELEGATE_CANCEL);
    crate::testid::set_test_id(&cancel, ids::ADMIN_DNS_CERT_DELEGATE_CANCEL_BUTTON);
    form.append(&cancel);

    row.append(&form);

    if cred_zones.is_empty() {
        // No held credential → no controlled zone to delegate to.
        delegate_btn.set_sensitive(false);
        delegate_btn.set_tooltip_text(Some(admin::dns::cert::DELEGATE_NO_ZONES));
    } else {
        {
            let form = form.clone();
            delegate_btn.connect_clicked(move |b| {
                b.set_visible(false);
                form.set_visible(true);
            });
        }
        {
            let form = form.clone();
            let delegate_btn = delegate_btn.clone();
            cancel.connect_clicked(move |_| {
                form.set_visible(false);
                delegate_btn.set_visible(true);
            });
        }
        {
            let client = Rc::clone(client);
            let domain = domain.to_string();
            let cred_zones = cred_zones.to_vec();
            submit.connect_clicked(move |_| {
                let idx = zone_select.selected() as usize;
                if let Some(zone) = cred_zones.get(idx) {
                    client.dns_delegate_renewal(domain.clone(), zone.clone());
                }
            });
        }
    }

    section.append(&row);
}

/// Build one soft-deleted `admin-dns-removed-domain` row: the domain name + a
/// restore button (re-add within 30 days). No DNS records are shown for removed
/// domains.
fn build_removed_domain_row(domain: &str, client: &Rc<FaunaClient>) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    crate::testid::set_test_id(&row, ids::ADMIN_DNS_REMOVED_DOMAIN);

    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    inner.set_margin_top(8);
    inner.set_margin_bottom(8);

    let name = gtk::Label::new(Some(domain));
    name.add_css_class("dim-label");
    name.set_halign(gtk::Align::Start);
    name.set_hexpand(true);
    name.set_xalign(0.0);
    crate::testid::set_test_id(&name, ids::ADMIN_DNS_REMOVED_DOMAIN_NAME);
    inner.append(&name);

    let restore_btn = gtk::Button::with_label(admin::dns::RESTORE);
    crate::testid::set_test_id(&restore_btn, ids::ADMIN_DNS_REMOVED_DOMAIN_RESTORE_BUTTON);
    // tui's `admin::Action::RestoreDnsDomain`.
    crate::offline_gate::declare_wire_kind(&restore_btn, "fauna.bridges.restore_local_domain");
    {
        let client = Rc::clone(client);
        let domain = domain.to_string();
        restore_btn.connect_clicked(move |_| client.restore_local_domain(domain.clone()));
    }
    inner.append(&restore_btn);

    row.set_child(Some(&inner));
    row
}

/// A non-selectable header row introducing the soft-deleted
/// `admin-dns-removed-domain` group: `admin.dns.removed_title` heading +
/// `admin.dns.removed_desc` subtitle. The other 5 clients render this section
/// heading + description above the restorable rows (web admin/dns/+page.svelte
/// `{#if removed.length > 0}`); appended only when at least one domain is
/// soft-deleted. Decorative — no test id.
fn build_removed_section_header() -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);

    let inner = gtk::Box::new(gtk::Orientation::Vertical, 2);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    inner.set_margin_top(12);
    inner.set_margin_bottom(4);

    let title = gtk::Label::new(Some(admin::dns::REMOVED_TITLE));
    title.add_css_class("title-4");
    title.set_halign(gtk::Align::Start);
    title.set_xalign(0.0);
    inner.append(&title);

    let desc = gtk::Label::new(Some(admin::dns::REMOVED_DESC));
    desc.add_css_class("dim-label");
    desc.set_halign(gtk::Align::Start);
    desc.set_xalign(0.0);
    inner.append(&desc);

    row.set_child(Some(&inner));
    row
}

/// Build one `admin-dns-record` card from a record row: name / type / value +
/// a red/green verify status + a copy button.
fn build_dns_record_card(record: &fauna_client_dns::DnsRecordRow) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.set_margin_top(4);
    card.set_margin_bottom(4);
    crate::testid::set_test_id(&card, ids::ADMIN_DNS_RECORD);

    card.append(&dns_field_row(
        admin::dns::FIELD_NAME,
        &record.name,
        "admin-dns-record-name",
        true,
    ));
    card.append(&dns_field_row(
        admin::dns::FIELD_TYPE,
        &record.record_type,
        "admin-dns-record-type",
        false,
    ));
    card.append(&dns_field_row(
        admin::dns::FIELD_VALUE,
        &record.expected,
        "admin-dns-record-value",
        true,
    ));

    // Status + copy button on one row.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_margin_start(12);
    actions.set_margin_end(12);
    actions.set_margin_top(2);
    actions.set_margin_bottom(2);

    let (status_text, status_class) = verdict_text_and_class(record.verdict.as_ref());
    let status = gtk::Label::new(Some(status_text.as_str()));
    status.add_css_class(status_class);
    status.set_halign(gtk::Align::Start);
    status.set_hexpand(true);
    status.set_xalign(0.0);
    crate::testid::set_test_id(&status, ids::ADMIN_DNS_RECORD_STATUS);
    actions.append(&status);

    let copy_btn = gtk::Button::with_label(admin::dns::COPY);
    crate::testid::set_test_id(&copy_btn, ids::ADMIN_DNS_RECORD_COPY_BUTTON);
    {
        let value = record.expected.clone();
        copy_btn.connect_clicked(move |btn| {
            btn.clipboard().set_text(&value);
        });
    }
    actions.append(&copy_btn);

    card.append(&actions);

    // PTR can never be zone-published — reverse DNS is set at the IP owner
    // (dns-management.md § Records covered) — so only this row gets the
    // advisory instead of the normal paste-and-verify treatment.
    if record.record_type == "PTR" {
        let note = gtk::Label::new(Some(admin::dns::PTR_PROVIDER_NOTE));
        note.add_css_class("dim-label");
        note.set_halign(gtk::Align::Start);
        note.set_xalign(0.0);
        note.set_wrap(true);
        note.set_margin_start(12);
        note.set_margin_end(12);
        note.set_margin_top(2);
        note.set_margin_bottom(2);
        crate::testid::set_test_id(&note, ids::ADMIN_DNS_RECORD_PROVIDER_NOTE);
        card.append(&note);
    }

    card
}

/// Build one held-credential row (`admin-dns-credential-item`, indexed): the
/// provider id + covered zone names + a clear button. The credential's secret
/// fields are never rendered (the add-form is write-only); the row shows only
/// the secret-free `CredentialSummary`. The clear-button dispatches
/// `ClearCredentials { index }` keyed by the snapshot position.
fn build_credential_item(
    index: u32,
    cred: &fauna_client_dns::CredentialSummary,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    // `accessible_role(Group)` so the row Box is countable/discoverable by ID
    // (plain Box → role Generic, which AT-SPI on Linux may omit from the tree).
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    row.set_margin_top(4);
    row.set_margin_bottom(4);
    crate::testid::set_test_id(&row, ids::ADMIN_DNS_CREDENTIAL_ITEM);

    let provider = gtk::Label::new(Some(&cred.provider_id));
    provider.add_css_class("title-4");
    provider.set_halign(gtk::Align::Start);
    provider.set_xalign(0.0);
    crate::testid::set_test_id(&provider, ids::ADMIN_DNS_CREDENTIAL_ITEM_PROVIDER);
    row.append(&provider);

    let zones_text = if cred.zones.is_empty() {
        admin::dns::CREDENTIAL_ZONES.to_string()
    } else {
        format!(
            "{}: {}",
            admin::dns::CREDENTIAL_ZONES,
            cred.zones.join(", ")
        )
    };
    let zones = gtk::Label::new(Some(&zones_text));
    zones.add_css_class("dim-label");
    zones.set_halign(gtk::Align::Start);
    zones.set_hexpand(true);
    zones.set_xalign(0.0);
    zones.set_wrap(true);
    crate::testid::set_test_id(&zones, ids::ADMIN_DNS_CREDENTIAL_ITEM_ZONES);
    row.append(&zones);

    let clear_btn = gtk::Button::with_label(admin::dns::REMOVE);
    clear_btn.add_css_class("destructive-action");
    crate::testid::set_test_id(&clear_btn, ids::ADMIN_DNS_CREDENTIAL_ITEM_CLEAR_BUTTON);
    {
        let client = Rc::clone(client);
        clear_btn.connect_clicked(move |_| client.dns_clear_credentials(index));
    }
    row.append(&clear_btn);

    row
}

/// Update the nest statistics rows from a JSON stats response.
pub fn update_admin_stats(handles: &AdminHandles, stats: &serde_json::Value) {
    // Each card's value is written to BOTH the visible row subtitle and the
    // hidden `admin-stat-card-value` E2E marker, so the test reads the same
    // value the user sees (and only once the data has actually loaded).
    let set = |row: &adw::ActionRow, marker: &gtk::Label, value: &str| {
        row.set_subtitle(value);
        marker.set_text(value);
    };

    // Keys are the nest's `/admin/api/stats` response shape (see
    // bins/fauna-nest/src/admin.rs::stats and web's admin +page.svelte, the
    // canonical consumer): `total_users`, `total_storage_bytes`,
    // `total_inbox_bytes`, `ws_connections` — read by exactly those names.
    let u64_field = |keys: &[&str]| -> Option<u64> {
        keys.iter()
            .find_map(|k| stats.get(*k).and_then(|v| v.as_u64()))
    };

    if let Some(users) = u64_field(&["total_users"]) {
        set(
            &handles.stats_users_row,
            &handles.stats_users_value,
            &users.to_string(),
        );
    }

    if let Some(storage) = u64_field(&["total_storage_bytes"]) {
        set(
            &handles.stats_storage_row,
            &handles.stats_storage_value,
            &crate::i18n::byte_size(storage),
        );
    }

    // The nest exposes inbox SIZE (`total_inbox_bytes`), not a message count;
    // this card's title is "Total Inbox Messages", so reading bytes here would
    // be a label/data mismatch. Leave it reading a message-count key (stays
    // blank until the nest exposes one) rather than show bytes under a
    // "messages" label. Not E2E-gated; see NEXT for the label-vs-data follow-up.
    if let Some(inbox) = u64_field(&["total_inbox_messages", "inbox_count"]) {
        set(
            &handles.stats_inbox_row,
            &handles.stats_inbox_value,
            &inbox.to_string(),
        );
    }

    if let Some(sessions) = u64_field(&["ws_connections"]) {
        set(
            &handles.stats_sessions_row,
            &handles.stats_sessions_value,
            &sessions.to_string(),
        );
    }
}

/// Update the dashboard recent-users group from a typed users page (read-only;
/// no tier picker — that lives on the Users hub).
pub fn update_dashboard_users(
    handles: &AdminHandles,
    reply: &fauna_client_admin::admin::AdminUsersListReply,
) {
    super::layout::clear_preferences_group_rows(&handles.dashboard_users_group);
    if reply.users.is_empty() {
        let empty_row = adw::ActionRow::builder()
            .title(admin::view::NO_REGISTERED_USERS)
            .build();
        handles.dashboard_users_group.add(&empty_row);
        return;
    }
    for user in reply.users.iter().take(10) {
        handles
            .dashboard_users_group
            .add(&build_dashboard_user_row(user));
    }
}

/// Update the Users section from a typed users page, wiring each row's
/// `admin-users-tier-select` change-tier picker.
pub fn update_users_page(
    handles: &AdminHandles,
    reply: &fauna_client_admin::admin::AdminUsersListReply,
    client: &Rc<FaunaClient>,
) {
    // A successful list render clears any prior Users-section action error.
    render_page_error(&handles.users_action_error, None);
    let total = if reply.total > 0 {
        reply.total as u64
    } else {
        reply.users.len() as u64
    };
    handles
        .users_page_count
        .set_text(&admin::users_page::total(&total.to_string()));

    // Record the bound + reflect it on the prev/next buttons (the handlers also
    // guard, so this is visual feedback only — admin.md § Users pagination) and
    // the page-position indicator.
    handles.users_total.set(total as i64);
    let offset = handles.users_offset.get();
    handles.users_prev_page.set_sensitive(offset > 0);
    handles
        .users_next_page
        .set_sensitive(offset + USERS_PAGE_SIZE < total as i64);
    let pages = fauna_core::format::total_pages(total as i64, USERS_PAGE_SIZE);
    let current = fauna_core::format::current_page(offset, USERS_PAGE_SIZE);
    handles
        .users_pagination
        .set_text(&admin::users_page::page_indicator(
            &current.to_string(),
            &pages.to_string(),
        ));

    clear_list_box(&handles.users_list_box);
    for user in &reply.users {
        handles
            .users_list_box
            .append(&build_user_row(user, &handles.tier_names, client));
    }
}

/// Render the `admin-users-registration-section` from the last `fauna.setup.status`
/// (raw `registration_mode` wire string + `max_free_users`) — admin.md § 2 Users
/// → Section 2 — Registration; public-mode.md § Registration Modes +
/// Implementation status today: a `None`/unparseable mode is NEVER coerced to a
/// guessed enum variant. `raw_mode` is `None` for an absent field (a non-conforming nest reply),
/// `Some(s)` for a present wire string (possibly one this
/// binary predates); both unparseable cases render the section fully read-only —
/// the editable block (select, input, save) disappears entirely rather than being
/// merely disabled — matching web's `registrationMode === null` branch exactly
/// (never offer a Save that could overwrite the nest's real posture with a guess).
///
/// `age_verification_required` re-seeds the require-knob's draft and the
/// persisted baseline the save compares it against.
pub fn set_registration_mode(
    handles: &AdminHandles,
    raw_mode: Option<&str>,
    max_free_users: Option<u64>,
    age_verification_required: bool,
) {
    handles
        .registration_age_verification_persisted
        .set(age_verification_required);
    handles
        .registration_age_verification_toggle
        .set_active(age_verification_required);
    match raw_mode.and_then(fauna_client_admin::RegistrationMode::from_wire_str) {
        Some(mode) => {
            handles.registration_readonly_label.set_visible(false);
            handles.registration_editable_box.set_visible(true);
            let wire = mode.as_wire_str();
            if let Some(i) = registration_mode_options()
                .iter()
                .position(|(v, _)| v == wire)
            {
                handles.registration_mode_select.set_selected(i as u32);
            }
            let text = max_free_users.map(|n| n.to_string()).unwrap_or_default();
            if handles.registration_max_free_users_input.text() != text {
                handles.registration_max_free_users_input.set_text(&text);
            }
        }
        None => {
            handles.registration_editable_box.set_visible(false);
            handles.registration_readonly_label.set_text(
                &admin::users_page::registration_mode_unknown(raw_mode.unwrap_or("—")),
            );
            handles.registration_readonly_label.set_visible(true);
        }
    }
}

/// Update the Invite section's code list from a typed reply, wiring each row's
/// `admin-settings-invite-delete-button`.
pub fn update_invite_codes(
    handles: &AdminHandles,
    reply: &fauna_client_admin::admin::AdminInviteCodesListReply,
    client: &Rc<FaunaClient>,
) {
    // A successful codes render clears any prior Users-section action error.
    render_page_error(&handles.users_action_error, None);
    clear_list_box(&handles.invite_codes_list);

    for item in &reply.invite_codes {
        // The minted band echoes on the same row, richer text, no new id
        // (`family-safety.md` § App surface → *Age-band surfaces*).
        let band = item
            .age_band
            .as_deref()
            .and_then(fauna_protocol::age::age_band_label)
            .map(|t| format!(" · {}", t.resolve(crate::i18n::strings::lookup)))
            .unwrap_or_default();
        let subtitle = format!(
            "{} · {}{band}",
            item.tier,
            admin::settings_page::uses_left_n(&item.uses_left.to_string()),
        );

        // A `gtk::ListBoxRow` (not `adw::ActionRow`) so the `invite-code-item`
        // id surfaces to AT-SPI — see `build_user_row` for the same constraint.
        let row = gtk::ListBoxRow::new();
        row.set_selectable(false);
        crate::testid::set_test_id(&row, ids::INVITE_CODE_ITEM);

        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hbox.set_margin_top(8);
        hbox.set_margin_bottom(8);
        hbox.set_margin_start(12);
        hbox.set_margin_end(12);

        let info = gtk::Box::new(gtk::Orientation::Vertical, 2);
        info.set_hexpand(true);
        let code_label = gtk::Label::new(Some(&item.code));
        code_label.add_css_class("monospace");
        code_label.set_halign(gtk::Align::Start);
        code_label.set_selectable(true);
        crate::testid::set_test_id(&code_label, ids::INVITE_CODE_VALUE);
        info.append(&code_label);
        let subtitle_label = gtk::Label::new(Some(&subtitle));
        subtitle_label.add_css_class("dim-label");
        subtitle_label.set_halign(gtk::Align::Start);
        info.append(&subtitle_label);
        hbox.append(&info);

        let delete_btn = gtk::Button::from_icon_name("user-trash-symbolic");
        delete_btn.add_css_class("flat");
        delete_btn.set_valign(gtk::Align::Center);
        crate::testid::set_test_id(&delete_btn, ids::ADMIN_SETTINGS_INVITE_DELETE_BUTTON);
        {
            let client = Rc::clone(client);
            let code = item.code.clone();
            delete_btn.connect_clicked(move |_| client.delete_admin_invite_code(code.clone()));
        }
        hbox.append(&delete_btn);

        row.set_child(Some(&hbox));
        handles.invite_codes_list.append(&row);
    }

    handles.create_invite_btn.set_sensitive(true);
}

/// Update the server status rows from a JSON status response.
pub fn update_admin_status(handles: &AdminHandles, status: &serde_json::Value) {
    if let Some(version) = status.get("version").and_then(|v| v.as_str()) {
        handles.server_version_row.set_subtitle(version);
        handles.server_version_value.set_text(version);
    }

    if let Some(uptime) = status.get("uptime").and_then(|v| v.as_u64()) {
        handles
            .server_uptime_row
            .set_subtitle(&crate::i18n::duration_secs(uptime));
    } else if let Some(uptime) = status.get("uptime").and_then(|v| v.as_str()) {
        handles.server_uptime_row.set_subtitle(uptime);
    }

    if let Some(workers) = status.get("workers").and_then(|v| v.as_str()) {
        handles.server_workers_row.set_subtitle(workers);
    } else if let Some(workers) = status.get("worker_count").and_then(|v| v.as_u64()) {
        handles
            .server_workers_row
            .set_subtitle(&format!("{} active", workers));
    } else if let Some(status_str) = status.get("worker_status").and_then(|v| v.as_str()) {
        handles.server_workers_row.set_subtitle(status_str);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn clear_list_box(list_box: &gtk::ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
}

fn clear_box(b: &gtk::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

/// Title for a user row: the label if set, else the short actor id, else the
/// "no handle" placeholder. `AdminUser` has no handle (the read-side admin view
/// keys on actor id; a handle change needs user consent — admin.md).
/// The prominent row title: the user's handle. The handle is what identifies a
/// user to the admin (admin.md § 2 / "handle is more important than the actor
/// id"), so a handle-less user reads `NO_HANDLE` here rather than borrowing the
/// actor id as a stand-in title — the actor id is shown once, dim, as the
/// secondary line.
fn user_row_title(user: &fauna_client_admin::admin::AdminUser) -> String {
    if user.label.is_empty() {
        admin::users_page::NO_HANDLE.to_string()
    } else {
        user.label.clone()
    }
}

/// A read-only dashboard recent-user row (no tier picker).
///
/// Deliberately carries NO `user-row` / `user-actor-id` test id: ui.yaml scopes
/// those to the `admin-users` page only (the Users hub), not `admin-dashboard`,
/// so the dashboard's recent-users group is not an automation target. That
/// scoping is the whole reason; it stands on its own.
///
/// ⚠ The second reason this comment used to give — "GTK keeps every
/// `adw::ViewStack` page mounted, and the agent walks the whole window tree
/// (unlike AT-SPI / the other apps, which only expose the visible view), so
/// tagging these rows would double-count `user-row`" — is **retired, and was
/// wrong in both halves.** This shell is a `gtk::Stack`, not an
/// `adw::ViewStack` (the app constructs none), and the agent does not walk the
/// whole tree: `automation::find::is_showing` prunes every page a stack is not
/// currently on, mid-transition included. A tagged dashboard row would be pruned along with the dashboard. Do
/// not reason from "the agent sees everything" anywhere in this app.
fn build_dashboard_user_row(user: &fauna_client_admin::admin::AdminUser) -> adw::ActionRow {
    // Handle is the prominent title; the actor id appears once, short, in the
    // subtitle next to the tier. The former full-hex suffix label rendered the
    // 64-char id a third time (title/subtitle/suffix) and dominated the row — it
    // carried no test id (dashboard rows are not an automation target), so it is
    // pure clutter and is dropped. "Handle is more important than the actor id."
    adw::ActionRow::builder()
        .title(user_row_title(user))
        .subtitle(format!(
            "{} · {}",
            user.tier,
            short_actor_id(user.actor_id.as_ref())
        ))
        .subtitle_selectable(true)
        .build()
}

/// A Users-section user row from a typed [`AdminUser`], carrying `user-row` /
/// `user-actor-id` + the per-row `admin-users-tier-select` change-tier picker.
fn build_user_row(
    user: &fauna_client_admin::admin::AdminUser,
    tier_names: &Rc<RefCell<Vec<String>>>,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    // A `gtk::ListBoxRow` (not `adw::ActionRow`): a bare ListBoxRow surfaces its
    // `set_test_id` widget-name to the automation agent, matching the
    // `admin-bridges-pending-card` / `admin-dns-domain` row prior art.
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    crate::testid::set_test_id(&row, ids::USER_ROW);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);

    let info = gtk::Box::new(gtk::Orientation::Vertical, 2);
    info.set_hexpand(true);
    let title = gtk::Label::new(Some(&user_row_title(user)));
    title.set_halign(gtk::Align::Start);
    info.append(&title);
    // The actor id, shown exactly once: short form (first 12 hex + …), dim, as
    // the secondary line under the handle — no longer repeated as the title (a
    // handle-less row now reads "No handle") nor as a third full-hex column.
    // Tagged `user-actor-id`; the e2e prefix-matches the displayed id against
    // the full hex (`test_admin_serving_indicator`), so the short form passes
    // and converges linux onto web's `<first-12>…` display (priority #4).
    let actor_id_label = gtk::Label::new(Some(&short_actor_id(user.actor_id.as_ref())));
    actor_id_label.add_css_class("dim-label");
    actor_id_label.add_css_class("monospace");
    actor_id_label.set_halign(gtk::Align::Start);
    actor_id_label.set_selectable(true);
    crate::testid::set_test_id(&actor_id_label, ids::USER_ACTOR_ID);
    info.append(&actor_id_label);
    hbox.append(&info);

    // Read-only IMAP/CalDAV-serving audit indicator (admin.md § Users;
    // deployment-home-with-public-relay.md § MUA reach). The admin only *sees*
    // this — the user sets it from their own mail-settings serve-here toggle, so
    // there is no control here (admin.md § Don't do these: no per-user controls).
    // Shared bool→key decision (`fauna_core::format::mail_serving_status_label`),
    // so the `serving_here` / `serving_disabled` choice can't drift per-app.
    let serving_text = fauna_core::format::mail_serving_status_label(user.mail_serving_enabled)
        .resolve(crate::i18n::strings::lookup);
    let serving_label = gtk::Label::new(Some(&serving_text));
    serving_label.add_css_class("dim-label");
    serving_label.set_valign(gtk::Align::Center);
    crate::testid::set_test_id(&serving_label, ids::ADMIN_USERS_MAIL_SERVING_STATUS);
    hbox.append(&serving_label);

    // Clone for the eviction control before the tier closure moves its own clone.
    let evict_actor = user.actor_id.as_ref().to_vec();
    let evict_client = Rc::clone(client);

    // Clone for the roster controls (make/remove admin), same reasoning.
    let actor_for_role = user.actor_id.as_ref().to_vec();
    let role_client = Rc::clone(client);

    // Per-row tier picker: selecting a tier applies it via
    // `fauna.admin.users.update` (the tier is the quota — admin.md § Users).
    // Seeded at the user's current tier; the on-select handler is wired after
    // that seed, so building the row doesn't re-apply the same tier.
    let actor_id = user.actor_id.as_ref().to_vec();
    let label = user.label.clone();
    let client = Rc::clone(client);
    let tier_select = build_tier_select_dropdown(
        "admin-users-tier-select",
        &user.tier,
        tier_names,
        move |new_tier| {
            client.update_admin_user_tier(actor_id.clone(), new_tier.to_string(), label.clone());
        },
    );
    tier_select.set_valign(gtk::Align::Center);
    // tui's `admin::Action::SetUserTier`.
    crate::offline_gate::declare_wire_kind(&tier_select, "fauna.admin.users.update");
    hbox.append(&tier_select);

    // Lifecycle controls (admin.md § 2 Users → *Cutting a user off*). Which of
    // the three a row offers is the SHARED decision — `admin_user_row_controls`
    // in `fauna-client-admin`, unit-tested there — so all seven apps agree on a
    // rule that is genuinely easy to get wrong (three eviction states crossed
    // with the admin-role guard). Notably: Suspend stays available on a
    // mid-eviction `warning` row (it promotes the user and *clears* the pending
    // delete), and an admin row offers no cut-off control at all because the nest
    // answers `fauna.admin.conflict`. All are descendants of the `user-row`, so
    // the e2e queries them scoped to the row (`user-row[i]`). One click acts
    // immediately — no confirm dialog, since both cut-off paths are reversible by
    // the Cancel-eviction control beside them.
    let controls = fauna_client_admin::admin_user_row_controls(user);

    if controls.evict {
        let actor = evict_actor.clone();
        let client = Rc::clone(&evict_client);
        let evict_btn = gtk::Button::with_label(admin::users_page::EVICT);
        evict_btn.add_css_class("flat");
        evict_btn.add_css_class("destructive-action");
        evict_btn.set_valign(gtk::Align::Center);
        crate::testid::set_test_id(&evict_btn, ids::ADMIN_USERS_EVICT_BUTTON);
        // tui's `admin::Action::EvictUser`.
        crate::offline_gate::declare_wire_kind(&evict_btn, "fauna.admin.users.evict");
        evict_btn.connect_clicked(move |_| {
            client.evict_user(actor.clone());
        });
        hbox.append(&evict_btn);
    }

    if controls.suspend {
        let actor = evict_actor.clone();
        let client = Rc::clone(&evict_client);
        let suspend_btn = gtk::Button::with_label(admin::users_page::SUSPEND);
        suspend_btn.add_css_class("flat");
        suspend_btn.add_css_class("destructive-action");
        suspend_btn.set_valign(gtk::Align::Center);
        crate::testid::set_test_id(&suspend_btn, ids::ADMIN_USERS_SUSPEND_BUTTON);
        // tui's `admin::Action::SuspendUser`.
        crate::offline_gate::declare_wire_kind(&suspend_btn, "fauna.admin.users.suspend");
        suspend_btn.connect_clicked(move |_| {
            client.suspend_user(actor.clone());
        });
        hbox.append(&suspend_btn);
    }

    if controls.restore {
        let cancel_btn = gtk::Button::with_label(admin::users_page::CANCEL_EVICTION);
        cancel_btn.add_css_class("flat");
        cancel_btn.set_valign(gtk::Align::Center);
        crate::testid::set_test_id(&cancel_btn, ids::ADMIN_USERS_CANCEL_EVICTION_BUTTON);
        // tui's `admin::Action::CancelUserEviction`.
        crate::offline_gate::declare_wire_kind(&cancel_btn, "fauna.admin.users.cancel_eviction");
        cancel_btn.connect_clicked(move |_| {
            evict_client.cancel_user_eviction(evict_actor.clone());
        });
        hbox.append(&cancel_btn);
    }

    if controls.make_admin {
        let actor = actor_for_role.clone();
        let client = Rc::clone(&role_client);
        let make_admin_btn = gtk::Button::with_label(admin::users_page::MAKE_ADMIN);
        make_admin_btn.add_css_class("flat");
        make_admin_btn.set_valign(gtk::Align::Center);
        crate::testid::set_test_id(&make_admin_btn, ids::ADMIN_USERS_MAKE_ADMIN_BUTTON);
        // tui's `admin::Action::MakeAdmin`.
        crate::offline_gate::declare_wire_kind(&make_admin_btn, "fauna.admin.admins.add");
        make_admin_btn.connect_clicked(move |_| {
            client.make_admin(actor.clone());
        });
        hbox.append(&make_admin_btn);
    }

    if controls.remove_admin {
        let remove_admin_btn = gtk::Button::with_label(admin::users_page::REMOVE_ADMIN);
        remove_admin_btn.add_css_class("flat");
        remove_admin_btn.add_css_class("destructive-action");
        remove_admin_btn.set_valign(gtk::Align::Center);
        crate::testid::set_test_id(&remove_admin_btn, ids::ADMIN_USERS_REMOVE_ADMIN_BUTTON);
        // tui's `admin::Action::RemoveAdmin`.
        crate::offline_gate::declare_wire_kind(&remove_admin_btn, "fauna.admin.admins.remove");
        remove_admin_btn.connect_clicked(move |_| {
            role_client.remove_admin(actor_for_role.clone());
        });
        hbox.append(&remove_admin_btn);
    }

    row.set_child(Some(&hbox));
    row
}

#[cfg(test)]
mod tests {
    use super::{
        GuardianSelect, OauthKeysRead, OauthSectionState, actor_not_loaded_fallback_label, admin,
        is_users_hub_context,
    };
    use adw::prelude::*;
    use fauna_client_admin::IssuerForcedArm;

    /// The DNS catch-all/role-address pickers' not-loaded fallback shows the
    /// **full** actor hex, not a 4-byte-truncated prefix (admin.md § 2's
    /// two-halves rule; row 603, mirroring apple's
    /// `actorLabelFallsBackToFullHexWhenActorNotLoaded`). A 32-byte id renders
    /// 64 hex characters through the same `"actor {short}…"` template every
    /// app shares — the trailing "…" stays even though `{short}` no longer is.
    #[test]
    fn dns_picker_fallback_label_shows_full_hex_not_truncated_prefix() {
        let id = [0xabu8; 32];
        let label = actor_not_loaded_fallback_label(&id);
        assert_eq!(label, format!("actor {}…", "ab".repeat(32)));
        assert_ne!(
            label,
            format!("actor {}…", "ab".repeat(4)),
            "must not regress to the old 4-byte-truncated prefix"
        );
    }

    // `guardian_option_label`'s handle/hex-fallback logic moved to the shared
    // `fauna_client_admin::admin_picker_option` (the cross-app owner tui/
    // android/web also call) — its own injectivity is pinned there. The test
    // below pins the one thing that pin can't see: whether THIS app's build
    // site still calls it per-item, rather than having drifted back onto the
    // raw `label`.

    /// The call-site injectivity pin, mirroring
    /// apple's `actorLabelStaysInjectiveWhenLabelsCollide`
    /// (`AdminDnsVMTests.swift`): `GuardianSelect::populate` is linux's one
    /// remaining bare per-item call
    /// to `fauna_client_admin::admin_picker_option` — the DNS/apex build
    /// sites already route through the list-shaped, already-pinned
    /// `fauna_client_admin::actor_picker_options` (`:6084`,
    /// `settings/admin_web.rs:143`), so only the guardian picker can still
    /// drift back onto the raw, non-unique `label` without any test noticing.
    /// Two non-suspended users sharing a display label but holding distinct
    /// handles must still populate two DISTINCT dropdown options.
    #[test]
    fn guardian_select_options_stay_injective_when_labels_collide() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let alex = fauna_client_admin::AdminUser {
                actor_id: fauna_protocol::ByteBuf::from(vec![0x11u8; 32]),
                label: "e2e-test".into(),
                handle: Some("alex99".into()),
                ..Default::default()
            };
            let bao = fauna_client_admin::AdminUser {
                actor_id: fauna_protocol::ByteBuf::from(vec![0x22u8; 32]),
                label: "e2e-test".into(),
                handle: Some("bao77".into()),
                ..Default::default()
            };
            let users = vec![alex, bao];

            let select = GuardianSelect::new("test-guardian-select");
            select.populate(Some(users.as_slice()));

            let options: Vec<String> = select
                .dd
                .model()
                .map(|model| {
                    (0..model.n_items())
                        .filter_map(|i| {
                            model.item(i).and_then(|o| {
                                o.downcast::<gtk::StringObject>()
                                    .ok()
                                    .map(|s| s.string().to_string())
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            assert_eq!(
                options,
                vec![
                    admin::users_page::GUARDIAN_NONE.to_string(),
                    "alex99".to_string(),
                    "bao77".to_string(),
                ],
                "two same-labelled users must still populate two distinct options"
            );
        });
    }

    #[test]
    fn users_hub_contexts_route_to_dedicated_error() {
        // Every action on the three Users-hub sections (admin.md § Users):
        for ctx in [
            "fauna.admin.users.update",
            "fauna.admin.users.evict",
            // Suspending an *admin* is refused with `fauna.admin.conflict`. The
            // row withholds the button (`admin_user_row_controls`), but a row
            // loaded before the user was made admin still offers it — so the
            // refusal has to land somewhere the admin can read it.
            "fauna.admin.users.suspend",
            "fauna.admin.users.cancel_eviction",
            "fauna.admin.admins.add",
            "fauna.admin.admins.remove",
            "fauna.admin.invite_codes.list",
            "fauna.admin.invite_codes.create",
            "fauna.admin.invite_codes.delete",
            "fauna.admin.invite_requests.list",
            "fauna.admin.invite_requests.approve",
            "fauna.admin.invite_requests.deny",
            // The Registration section's one save — mode + ceiling, then the
            // age require-knob when it changed.
            "fauna.admin.set_registration_mode",
            "fauna.admin.set_age_verification_required",
        ] {
            assert!(
                is_users_hub_context(ctx),
                "{ctx} should route to admin-users-action-error"
            );
        }
    }

    #[test]
    fn non_users_hub_contexts_use_global_banner() {
        // Other admin sub-pages keep their own per-page error surface / the
        // global banner — they must NOT be captured by the Users-hub prefix.
        for ctx in [
            "fauna.admin.services.update",
            "fauna.admin.tiers.list",
            "fauna.admin.stats",
            "fauna.dns.list_records",
            "fauna.bridges.list_pending",
            "message_sent",
            "",
        ] {
            assert!(
                !is_users_hub_context(ctx),
                "{ctx} must not route to admin-users-action-error"
            );
        }
    }

    /// A zero cap means "no cap on the row — the pump substitutes the
    /// default", which is the opposite of "zero bytes allowed". Printing
    /// `0 B` would state the opposite of the truth (mirrors tui's own
    /// `a_capless_row_reads_as_default_never_as_zero_bytes`).
    #[test]
    fn a_capless_custody_hosting_row_reads_as_default_never_as_zero_bytes() {
        assert_eq!(
            super::custody_hosting_budget_text(0),
            super::admin::custody_hosting::BUDGET_DEFAULT
        );
    }

    #[test]
    fn a_real_custody_hosting_cap_renders_its_byte_size() {
        assert_ne!(
            super::custody_hosting_budget_text(8192),
            super::admin::custody_hosting::BUDGET_DEFAULT
        );
    }

    /// The three-state receipt word is never collapsed and never empty — the
    /// rule both custody sides already hold to (mirrors tui's own
    /// `every_receipt_state_gets_its_own_word`).
    #[test]
    fn every_custody_hosting_receipt_state_gets_its_own_word() {
        use fauna_client_capabilities::view_model::ReceiptState;
        let words: Vec<&str> = [
            ReceiptState::Fresh,
            ReceiptState::Stale,
            ReceiptState::NoReceiptYet,
        ]
        .into_iter()
        .map(fauna_client_capabilities::view_model::receipt_text)
        .collect();
        assert!(words.iter().all(|w| !w.is_empty()));
        let unique: std::collections::BTreeSet<&&str> = words.iter().collect();
        assert_eq!(unique.len(), 3, "three states, three distinct words");
    }

    // ── Outside-app sign-in keys (`admin-nest-oauth-*`) — the four gestures'
    // guards on the GTK-free `OauthSectionState` (mirrors tui's own oauth
    // tests in `apps/fauna-tui/src/admin/mod.rs`), then one paint test. ──

    /// An answered set of `n` keys, the signer first and the rest retired
    /// inside their horizon — the shape a rotation leaves behind.
    fn oauth_ready(n: usize) -> OauthKeysRead {
        OauthKeysRead::Ready(fauna_client_admin::IssuerKeyView {
            active_kid: "kid-0".into(),
            keys: (0..n)
                .map(|i| fauna_client_admin::IssuerKeyRow {
                    kid: format!("kid-{i}"),
                    signing: i == 0,
                    retired_at: (i > 0).then_some(1_000),
                    served_until: (i > 0).then_some(2_200),
                })
                .collect(),
            retirement_horizon_secs: 1_200,
            rotation_in_flight: n > 1,
        })
    }

    fn oauth_state(keys: OauthKeysRead) -> OauthSectionState {
        OauthSectionState {
            keys,
            ..Default::default()
        }
    }

    /// Every control refuses until the set has answered — "not asked yet" and
    /// "couldn't find out" alike: the forced confirm cannot name what it drops
    /// before then, and a driver-forced press on a disabled button must not
    /// dispatch either.
    #[test]
    fn oauth_controls_refuse_until_the_key_set_has_answered() {
        for keys in [
            OauthKeysRead::Unread,
            OauthKeysRead::Failed("unknown kind".into()),
        ] {
            let mut s = oauth_state(keys.clone());
            assert!(
                !s.controls_live(),
                "{keys:?} must paint the controls disabled"
            );
            for arm in [IssuerForcedArm::IssuerKey, IssuerForcedArm::SessionSecret] {
                s.arm(arm);
                assert_eq!(s.confirm, None, "{arm:?} must not arm on {keys:?}");
            }
            assert!(
                !s.press_rotate(),
                "the ordinary arm must not dispatch on {keys:?}"
            );
            assert!(!s.in_flight);
            assert_eq!(
                s.status, None,
                "a refused press must not claim to be working"
            );
        }
    }

    /// Arming captures the shared fold NOW and dispatches nothing; a re-read
    /// landing while armed must not re-fold the cost the admin already read;
    /// one arm at a time.
    #[test]
    fn oauth_arming_captures_the_fold_and_dispatches_nothing() {
        let mut s = oauth_state(oauth_ready(2));
        s.status = Some("a previous verdict".into());
        s.arm(IssuerForcedArm::IssuerKey);
        let armed = s
            .confirm
            .clone()
            .expect("the key arm arms on an answered set");
        assert_eq!(armed.arm, IssuerForcedArm::IssuerKey);
        assert_eq!(
            armed.view,
            fauna_client_admin::issuer_forced_confirm_view(
                IssuerForcedArm::IssuerKey,
                s.answered().expect("answered"),
            ),
            "the confirm paints the shared fold, not a per-app sentence"
        );
        assert_eq!(
            armed.view.summary.key, "admin.nest_page.oauth_force_rotate_confirm_many",
            "two served keys: the confirm names how many stop verifying"
        );
        assert!(!s.in_flight, "arming dispatches nothing");
        assert_eq!(s.status, None, "arming clears the previous verdict");

        s.keys_loaded(oauth_ready(1));
        assert_eq!(
            s.confirm,
            Some(armed),
            "a re-read landing while armed must not change the captured cost"
        );

        s.arm(IssuerForcedArm::SessionSecret);
        assert_eq!(
            s.confirm.as_ref().map(|c| c.arm),
            Some(IssuerForcedArm::SessionSecret),
            "arming the sibling replaces the armed arm — one at a time"
        );
    }

    /// The confirm disarms FIRST and fires exactly the armed arm; a double
    /// press finds nothing armed; nothing re-arms or re-dispatches while the
    /// call is in flight; the verdict and the re-read land together.
    #[test]
    fn oauth_confirm_disarms_first_and_fires_only_the_armed_arm() {
        for arm in [IssuerForcedArm::IssuerKey, IssuerForcedArm::SessionSecret] {
            let mut s = oauth_state(oauth_ready(2));
            s.arm(arm);
            assert_eq!(
                s.press_confirm(),
                Some(arm),
                "the confirm fires the armed arm"
            );
            assert_eq!(
                s.confirm, None,
                "the surface is gone once the press returns"
            );
            assert!(s.in_flight);
            assert_eq!(
                s.status.as_deref(),
                Some(super::admin::nest_page::OAUTH_WORKING)
            );
            assert_eq!(s.press_confirm(), None, "a double press dispatches nothing");

            s.arm(arm);
            assert_eq!(s.confirm, None, "nothing arms while a call is in flight");
            assert!(!s.press_rotate(), "nothing chains onto an in-flight call");

            s.done("the verdict".into(), oauth_ready(1));
            assert!(!s.in_flight);
            assert_eq!(s.status.as_deref(), Some("the verdict"));
            assert_eq!(s.keys, oauth_ready(1), "the re-read replaces the set");
            assert!(s.controls_live(), "the controls come back once it lands");
        }
    }

    /// A confirm somehow still armed while a call is in flight keeps what the
    /// admin can see and dispatches nothing.
    #[test]
    fn oauth_confirm_while_in_flight_keeps_the_armed_surface() {
        let mut s = oauth_state(oauth_ready(1));
        s.arm(IssuerForcedArm::IssuerKey);
        let armed = s.confirm.clone();
        s.in_flight = true;
        assert_eq!(s.press_confirm(), None);
        assert_eq!(s.confirm, armed, "the armed surface stays as painted");
    }

    /// The ordinary arm dispatches on the press (no confirm), disarms a forced
    /// confirm whose count it is about to change, and cannot chain.
    #[test]
    fn oauth_rotate_disarms_a_stale_confirm_and_cannot_chain() {
        let mut s = oauth_state(oauth_ready(1));
        s.arm(IssuerForcedArm::IssuerKey);
        assert!(s.press_rotate(), "the ordinary arm dispatches on the press");
        assert_eq!(
            s.confirm, None,
            "the forced confirm named a count about to change"
        );
        assert!(s.in_flight);
        assert_eq!(
            s.status.as_deref(),
            Some(super::admin::nest_page::OAUTH_WORKING)
        );
        assert!(
            !s.press_rotate(),
            "a second press must not chain a second rotation"
        );
    }

    /// Cancel disarms and dispatches nothing.
    #[test]
    fn oauth_cancel_disarms_and_dispatches_nothing() {
        let mut s = oauth_state(oauth_ready(2));
        s.arm(IssuerForcedArm::SessionSecret);
        s.cancel();
        assert_eq!(s.confirm, None);
        assert!(!s.in_flight);
        assert_eq!(s.status, None);
    }

    /// The paint rules on the real widgets: rows only from an answered read,
    /// signer first and in the nest's order; the reason line otherwise; the
    /// three buttons disabled, never hidden, until answered; the confirm trio
    /// only while armed, painting the captured fold; the verdict only once a
    /// control was used.
    #[test]
    fn oauth_section_paints_rows_only_from_an_answered_read() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            // The page's intent is what this test reads, so pin the gate open
            // (`offline_gate::declare_wire_kind`'s documented test trap).
            crate::offline_gate::reset_for_test("connected");
            let (_section, widgets) = super::build_oauth_section();
            let ctx = super::OauthSectionCtx {
                state: std::cell::RefCell::new(OauthSectionState::default()),
                widgets,
            };
            let w = &ctx.widgets;
            let rows = |ctx: &super::OauthSectionCtx| -> Vec<(String, String)> {
                let mut out = Vec::new();
                let mut child = ctx.widgets.keys_box.first_child();
                while let Some(c) = child {
                    let label = c
                        .clone()
                        .downcast::<gtk::Label>()
                        .expect("a row is a label");
                    out.push((label.widget_name().to_string(), label.text().to_string()));
                    child = c.next_sibling();
                }
                out
            };

            // Unread: no rows, the loading reason, every control disabled.
            super::render_oauth(&ctx);
            assert!(rows(&ctx).is_empty());
            assert!(w.key_reason.is_visible());
            assert_eq!(
                w.key_reason.text().as_str(),
                super::admin::nest_page::OAUTH_KEYS_LOADING
            );
            for button in [
                &w.rotate_button,
                &w.force_rotate_button,
                &w.secret_force_rotate_button,
            ] {
                assert!(button.is_visible() && !button.is_sensitive());
            }
            assert!(!w.confirm_box.is_visible());
            assert!(!w.status.is_visible());

            // Failed: still no rows — the worded reason instead.
            ctx.state
                .borrow_mut()
                .keys_loaded(OauthKeysRead::Failed("no such kind".into()));
            super::render_oauth(&ctx);
            assert!(rows(&ctx).is_empty());
            assert_eq!(w.key_reason.text().as_str(), "no such kind");

            // Answered: one row per key, signer first, literal indexed ids.
            ctx.state.borrow_mut().keys_loaded(oauth_ready(2));
            super::render_oauth(&ctx);
            let painted = rows(&ctx);
            assert_eq!(
                painted
                    .iter()
                    .map(|(id, _)| id.as_str())
                    .collect::<Vec<_>>(),
                ["admin-nest-oauth-key-item-0", "admin-nest-oauth-key-item-1"]
            );
            assert!(
                painted[0].1.starts_with("kid-0"),
                "the signer is first: {painted:?}"
            );
            assert!(
                !w.key_reason.is_visible(),
                "an answered set owes no reason line"
            );
            assert!(w.rotate_cost.is_visible());
            assert!(w.rotate_button.is_sensitive());
            assert!(w.force_rotate_button.is_sensitive());
            assert!(w.secret_force_rotate_button.is_sensitive());

            // Armed: the confirm trio paints the captured fold.
            ctx.state.borrow_mut().arm(IssuerForcedArm::IssuerKey);
            super::render_oauth(&ctx);
            assert!(w.confirm_box.is_visible());
            let armed = ctx.state.borrow().confirm.clone().expect("armed");
            assert_eq!(
                w.confirm_summary.text().to_string(),
                armed.view.summary.resolve(crate::i18n::strings::lookup)
            );
            assert_eq!(
                w.confirm_button.label().map(|l| l.to_string()),
                Some(
                    armed
                        .view
                        .confirm_label
                        .resolve(crate::i18n::strings::lookup)
                )
            );

            // Confirmed: disarmed, working, controls disabled; then the verdict.
            assert_eq!(
                ctx.state.borrow_mut().press_confirm(),
                Some(IssuerForcedArm::IssuerKey)
            );
            super::render_oauth(&ctx);
            assert!(!w.confirm_box.is_visible());
            assert!(!w.rotate_button.is_sensitive());
            assert_eq!(
                w.status.text().as_str(),
                super::admin::nest_page::OAUTH_WORKING
            );
            ctx.state
                .borrow_mut()
                .done("the verdict".into(), oauth_ready(1));
            super::render_oauth(&ctx);
            assert_eq!(rows(&ctx).len(), 1);
            assert!(w.status.is_visible());
            assert_eq!(w.status.text().as_str(), "the verdict");
            assert!(w.rotate_button.is_sensitive());

            crate::offline_gate::reset_for_test("disconnected");
        });
    }
}
