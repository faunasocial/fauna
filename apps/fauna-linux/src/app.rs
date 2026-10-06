use adw::prelude::*;
use fauna_protocol::StaleSurfaces;
use fauna_ui_ids as ids;
use gtk::gio;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::client::FaunaClient;
use crate::i18n::strings::common;
use crate::i18n::strings::errors;
use crate::i18n::strings::{contacts, events, family, navigation};
use crate::tray;
use crate::views;
use crate::views::conversations;
use crate::views::sidebar::{SidebarItem, build_sidebar};

/// How often the Events page re-lists the encrypted CalDAV store while it is the
/// visible page, so calendars/events written by an *external* CalDAV client
/// (e.g. macOS Calendar talking to the mail-bridge MDA) surface *while the user
/// stays on the page* — not only on navigate-away-and-back or re-login. This is
/// the poll half of events.md § Where logic lives ("`sync_calendar_since`
/// (RFC 6578) poll + a calendar-change push"); the delta-optimised
/// `sync_calendar_since` form and the server push are the follow-on. 10s is
/// perceptibly "quick" without hammering nest for an open, rarely-changing page.
const CALENDAR_POLL_INTERVAL_SECS: u64 = 10;

/// Order-insensitive equality of two row slices keyed by a stable id, used to
/// decide whether a `CalendarsLoaded` / `EventsLoaded` refresh actually changed
/// anything. The Events-page poll re-fetches on a fixed cadence; without this a
/// steady-state poll would destroy and rebuild `calendar-item` / `event-card`
/// widgets (and re-run reminder checks) every interval — flickering the page and
/// racing an in-progress click. Comparing sorted-by-id makes a stable nest reply
/// order irrelevant. Returns true when the slices differ.
fn rows_differ_by_id<T: PartialEq>(a: &[T], b: &[T], id: impl Fn(&T) -> &str) -> bool {
    if a.len() != b.len() {
        return true;
    }
    let mut a: Vec<&T> = a.iter().collect();
    let mut b: Vec<&T> = b.iter().collect();
    a.sort_by(|x, y| id(x).cmp(id(y)));
    b.sort_by(|x, y| id(x).cmp(id(y)));
    a != b
}

// ---------------------------------------------------------------------------
// UiMessage — from background tasks to GTK main thread
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum UiMessage {
    Data(DataMessage),
    Action(ActionResult),
    Realtime(WsEvent),
    /// No-op message — used when a callback handles UI inline on the GTK
    /// thread and does not need to route through the normal message handler.
    #[allow(dead_code)]
    Noop,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum DataMessage {
    AuthSuccess {
        actor_id: String,
        handle: String,
    },
    AuthFailed {
        error: String,
    },
    /// The nest's pinned deployment identity changed **mid-session**
    /// (`security.md` § Post-auth surfacing, ratified 2026-07-23). Carries no
    /// payload on purpose: the handler re-enters the real launch flow, whose
    /// own `LaunchMachine` re-derives the verdict and — crucially — is the
    /// machine `trust_nest_identity()` needs to act on. A payload here would
    /// invite rendering the surface from a synthesized phase, whose re-trust
    /// button would have no machine to drive and would silently do nothing.
    NestIdentityChanged,
    /// A background silent refresh learned this identity was **succeeded**
    /// (`identity-succession.md` § Propagation → *Own device fleet*). Escalates
    /// to the launch flow, which lands on the identity-import screen. Carries no
    /// payload for the same reason [`Self::NestIdentityChanged`] doesn't: the
    /// handler re-enters the real launch flow, whose own machine re-derives the
    /// verdict and owns the import affordance. The claimed successor is logged
    /// at the point of classification, never rendered from here.
    IdentitySuperseded,
    /// The nest stopped signing this signed-in identity in
    /// (`fauna.auth.not_registered`) — suspended or removed mid-session, met by
    /// the supervisor's post-4401 re-mint or a background silent refresh.
    /// Escalates to the launch flow like [`Self::IdentitySuperseded`]; its
    /// re-run challenge earns the same refusal and lands the previously-
    /// signed-in row's surface (`onboarding.md` § App-launch routing).
    SignInRefused,
    MessagesLoaded {
        conversation_id: String,
        messages: Vec<crate::rows::MessageRow>,
    },
    ContactsLoaded {
        contacts: Vec<crate::rows::ContactRow>,
    },
    MemberReviewsLoaded {
        reviews: Vec<fauna_core::data::MemberReview>,
    },
    KnocksLoaded {
        knocks: Vec<crate::rows::KnockRow>,
    },
    /// The private contact overlay projection moved (`contacts.md` § The
    /// private overlay): the roster rows and knock senders name people through
    /// it, so both lists repaint from the data already held.
    ContactOverlaysChanged,
    CalendarsLoaded {
        calendars: Vec<crate::rows::CalendarRow>,
    },
    EventsLoaded {
        /// The calendar these events belong to. The handler replaces only this
        /// calendar's slice of `events_cache` (events are fetched per-calendar),
        /// so the cache accumulates the union across all calendars rather than
        /// being overwritten by whichever per-calendar fetch completed last.
        /// Empty string = a cross-calendar query (e.g. invited events).
        calendar_id: String,
        events: Vec<crate::rows::EventRow>,
    },
    EventAttendeesLoaded {
        event_id: String,
        attendees: Vec<crate::views::events::caldav_backend::CalDavAttendee>,
    },
    /// The actor's CardDAV address books (slice 4b), from `list_addressbooks` +
    /// client-side metadata unseal. Repopulates the Address Book segment's book
    /// picker; the first book is auto-opened.
    AddressbooksLoaded {
        addressbooks: Vec<fauna_client_carddav::AddressbookRow>,
    },
    /// The decoded vCards of one address book, from `query_cards` + client-side
    /// unseal/parse. Repopulates the Address Book segment's card list.
    CardsLoaded {
        /// Hex `addressbook_id` these cards belong to (informational — the read
        /// is whole-book, so the handler replaces the list wholesale).
        addressbook_id: String,
        cards: Vec<fauna_client_carddav::VCardRow>,
    },
    /// A `SearchNav::Contact` deep link's `locate_card_by_uid_hash` resolve
    /// (`FaunaClient::locate_card_by_uid`) — carries BOTH the book-picker rows
    /// and, when found, the holding book's cards + the target `card_id` in one
    /// round trip (`ui/search.md` § Where logic lives → Result navigation).
    /// `open` is `None` when no book holds the `uid_hash` any more.
    CardLocated {
        addressbooks: Vec<fauna_client_carddav::AddressbookRow>,
        /// The holding book's hex id, its cards, and the target card's hex id.
        open: Option<(String, Vec<fauna_client_carddav::VCardRow>, String)>,
    },
    /// The reminder offset for an event, loaded from (or just written to) the
    /// encrypted store's VEVENT `VALARM`. `event_id` is the hex `uid_hash`;
    /// `offset` is the ISO-8601 preset (e.g. `PT1H`) or `None` when no reminder
    /// is set. Drives the two-state reminder control on the detail panel.
    EventReminderLoaded {
        event_id: String,
        offset: Option<String>,
    },
    /// An event card was clicked (or the selection was cleared, e.g. after
    /// delete). Drives the persistent event-detail panel: the handler stores it
    /// in `CalendarViewState::selected_event` and re-renders the panel. This is a
    /// local UI message (no nest round-trip) emitted via `FaunaClient::select_event`.
    EventSelected {
        event: Option<crate::rows::EventRow>,
    },
    SyncFilesLoaded {
        folder: String,
        files: Vec<crate::rows::SyncFileRow>,
    },
    // The Backups page's snapshot half — the folder list, the snapshot list,
    // the detail file list and the check verdict — no longer travels through
    // this channel at all. It is `BackupsMachine` state, painted by the page's
    // own observer render loop (`views/backups/mod.rs`), which is what retired
    // `SnapshotsLoaded` / `SnapshotDetailLoaded` / `SnapshotCheckResult` and
    // the `BackupStatusLoaded` selector-population side effect.
    /// Message-kind snapshots for the `restore-snapshot-select` picker
    /// (`fauna.filesync.snapshot.list`).
    MessageKindSnapshotsLoaded {
        snapshots: Vec<fauna_client_snapshots::filesync::SnapshotSummaryRow>,
    },
    /// Restore-history rows (`fauna.filesync.snapshot.list_restore_history`).
    /// The message loop fires one `fetch_restore_divergence` per row to fill
    /// the per-row banners.
    RestoreHistoryLoaded {
        rows: Vec<fauna_client_snapshots::filesync::RestoreHistoryRow>,
    },
    /// A local restore (`fauna.filesync.snapshot.restore_message_kind`)
    /// completed. `config_present == false` is the reply's one-shot advisory —
    /// the restore proceeded, but the bridge cannot sign in after restart until
    /// the account's configuration is restored too — painted as
    /// `restore-warning` (`docs/goal/ui/backups.md` § Restore from backup
    /// destination). The reply is its only carrier.
    MessageKindRestored {
        config_present: bool,
    },
    /// Divergence rows for one snapshot's restore
    /// (`fauna.filesync.snapshot.list_restore_divergence`) — updates that
    /// row's banner + stores the rows for the forensic modal.
    RestoreDivergenceLoaded {
        snapshot_id: i64,
        rows: Vec<fauna_client_snapshots::filesync::RestoreDivergenceRow>,
    },
    QuotaLoaded {
        quota: serde_json::Value,
    },
    /// The gated-feature plane's transparency read (`feature-limits-section`
    /// — `dynamic-features.md` § Transparency & auditability), already folded
    /// by the shared `FeaturesClient::rows()`.
    FeaturesLoaded {
        rows: Vec<fauna_client_features::FeatureRow>,
    },
    /// The region relay's answers for the declared chain
    /// (`FaunaClient::fetch_region`), folded on the GTK main thread by
    /// `crate::region::apply_replies`. A failed ask rides as its `Err` and is
    /// dropped there (§ Fail posture: a failed fetch writes nothing).
    RegionReplies {
        replies: Vec<(
            fauna_core::region_authority::RegionCode,
            Result<fauna_protocol::region::RegionArtifactGetReply, String>,
        )>,
    },
    AccountLoaded {
        handle: Option<String>,
    },
    /// Result of a successful silent sign-in (challenge + verify against
    /// `/api/v1/auth/verify`). Carries all three server-data cache
    /// fields so the launch path can refresh handle/domain/tier in one
    /// shot. Triggered from `FaunaClient::silent_sign_in` on startup;
    /// non-success cases (network, 404 unregistered) log and skip
    /// rather than emitting this variant.
    IdentityRefreshed {
        handle: String,
        domain: String,
        tier: String,
    },
    NestInfoLoaded {
        info: serde_json::Value,
    },
    InboxModeLoaded {
        mode: String,
    },
    KeyPackageCountLoaded {
        count: u32,
    },
    HandleChanged {
        new_handle: String,
    },
    /// A fresh read of `recovery-kit-status` off the registration chain
    /// (`settings.md` § Recovery kit — never a local flag, so a kit created on
    /// another device is reflected here). Fired on Settings/Account page build
    /// and on every page-visible refresh.
    RecoveryStatusLoaded {
        result: Result<fauna_client_recovery::RecoveryKitStatus, String>,
    },
    /// A fresh read of the actor's still-`pending` scheduled actions
    /// (`settings.md` § Pending actions). Fired on Settings/Account page
    /// build, on every page-visible refresh, and after either delayed verb
    /// this page hosts (change handle / delete account) completes.
    PendingActionsLoaded {
        result: Result<Vec<fauna_protocol::pending_actions::PendingActionSummary>, String>,
    },
    /// A create/replace/lost ceremony landed. Carries `(secret_hex, status)`:
    /// the minted secret and a no-second-hop status re-read taken inside the
    /// same background task (mirrors tui's `with_status`) — the section
    /// repaints from ONE message, no follow-up fetch, no wall-clock wait
    /// (convention 14). The kit always comes back even when the escrow half
    /// failed — the registration has already landed, and the returned secret
    /// is the only copy in existence. The `fauna://recovery` display URI is
    /// built on the GTK thread (tui's shape too — it needs the handle/actor-id
    /// the async task does not carry), not here.
    RecoveryKitMinted {
        result: Result<(String, fauna_client_recovery::RecoveryKitStatus), String>,
    },
    /// A kit-in-hand repair landed or refused — the pending-window veto or the
    /// no-escrow re-seal. Nothing was minted, so it carries only the status
    /// re-read inside the same task (convention 14), or the failure already
    /// localized by the client method that ran it.
    RecoveryKitRepaired {
        result: Result<fauna_client_recovery::RecoveryKitStatus, String>,
    },
    /// The succession ceremony landed (or refused). `Ok` carries the successor
    /// seed, the identity the account now belongs to, and the pre-switch group
    /// sweep's outcome — the propagation half rides here as a report rather than
    /// as an `Err`, because it can fail without unmaking the succession.
    ///
    /// **Boxed**: `LandedSuccession` is much the largest payload this enum
    /// carries, and this variant is rare. `Debug` on the inner type is
    /// hand-written and redacts the seed — at the instant this message exists,
    /// that seed IS the account and it exists nowhere else in the world.
    RecoverySucceeded {
        outcome: Box<fauna_client_recovery::ceremony::StolenOutcome>,
    },
    /// One report from the post-succession aftermath (`crate::succession_aftermath`),
    /// folded into the Recovery kit section's session cell and repainted.
    AftermathProgress(crate::settings::recovery_kit::AftermathUpdate),
    /// A press of `recovery-kit-sweep-retry-button` came back. `Ok` replaces the
    /// parked sweep with the fresher report; `Err` carries a sentence the shared
    /// projection already chose (three of the retry's four answers ARE the whole
    /// gesture — the press must answer in words).
    SweepRetried {
        result: Result<Box<fauna_client_recovery::ceremony::SweepStatus>, String>,
    },
    SpamPreferencesLoaded {
        prefs: fauna_client_spam::spam::SpamPreferences,
    },
    BridgesLoaded {
        bridges: Vec<fauna_client_bridges::bridges_ui::BridgeStatus>,
    },
    /// The connection actor's moderation queue — the union of the server
    /// `fauna.moderation.actions` obligation rows and the client's post-decrypt
    /// local detections, merged + deduped via `fauna_client_moderation::merge_queue`.
    ModerationActionsLoaded {
        rows: Vec<fauna_client_moderation::QueueRow>,
    },
    BridgeFollowsLoaded {
        bridge_id: String,
        follows: Vec<fauna_client_bridges::bridges_ui::BridgeFollow>,
    },
    /// The guardian gate refused a bridge link / follow with the TYPED
    /// `guardian_approval_required` error (`family-safety.md` § Feed-source
    /// approvals). Carries the `(bridge, operation, target)` triple the grant is
    /// scoped to, so `bridge-source-request-button` is offered for exactly the
    /// refused operation — and still lands on `error-message` (rule (b)).
    FeedSourceRefused {
        bridge_id: String,
        operation: String,
        target: String,
    },
    NotificationsLoaded {
        notifications: Vec<fauna_client_notifications::notifications::NotifItem>,
    },
    BlueskyThreadLoaded {
        post_id: String,
        /// Flat thread list from `bluesky.feed.thread` (ancestors oldest-first,
        /// the focal post, then its direct replies).
        posts: Vec<fauna_client_bluesky::bluesky::BlueskyPost>,
        /// Index into `posts` of the focal post, named by the nest.
        focal_index: usize,
    },
    /// The Status page's Sync-section aggregate ("Files Synced" / "Last Sync"),
    /// from `fetch_sync_status_summary` — a count + max `updated_at` (epoch
    /// seconds) across every device-sync folder binding's `fauna.sync.files`.
    SyncStatusSummaryLoaded {
        files_synced: u64,
        last_sync_at: Option<i64>,
    },
    HandleResolved {
        result: serde_json::Value,
    },
    NestResolved {
        domain: String,
        handle: String,
        result: serde_json::Value,
    },
    UpdateAvailable {
        version: String,
        url: String,
    },
    /// Open an external URL in the user's default browser (main-thread launch).
    /// Used by the bridge-link OAuth flow to surface a provider's authorize URL
    /// (`fauna.bridges.link` reply `redirect_url`), mirroring web's
    /// `window.location.href = redirect_url`.
    OpenUrl {
        url: String,
    },
    AdminStatusLoaded {
        is_admin: bool,
    },
    /// `fauna.family.status`, read once post-auth — drives the two GATED
    /// family surfaces (`family-safety.md` § App surface): the `family-tab`
    /// sidebar row (any relationship — guardian **or** supervised) and the
    /// global `supervised-indicator` (supervised only). Fails closed: a status
    /// error resolves to `None` / `0` and both surfaces stay hidden
    /// (`FaunaClient::check_family_status`).
    FamilyStatusLoaded {
        /// The guardian's handle, when this account is supervised.
        supervised_by: Option<String>,
        /// How many accounts this account guards.
        ward_count: usize,
        /// Transfer proposals awaiting this account's consent as proposed
        /// guardian — widens the `family-tab` gate (§ Graduation & transfer:
        /// a target with no other family relationship must reach the prompt).
        incoming_transfer_count: usize,
        /// The three client-enforced pillars this read carries, as the SHARED
        /// at-rest record (`family-safety.md` § Content policy, the
        /// unfetched-policy ruling): the guardian content floor, the Guardian
        /// Notify knob, and the screen-time policy. Drives
        /// `content_policy::set_ward_content_policy` /
        /// `set_ward_content_notify` and the `screen-time-lock` overlay, and is
        /// persisted verbatim so the next launch enforces it before its own
        /// read lands.
        ///
        /// ⚠ These three used to ride as separate `Option`s read straight off
        /// `status.policy`, which meant they were gated on the policy DOCUMENT
        /// being present rather than on a guardianship existing — linux relied
        /// on the nest never sending a policy to an unsupervised caller.
        /// `SupervisionSnapshot::from_status` carries that gate itself, so a
        /// reply naming no guardian now yields nothing enforceable here, the
        /// same rule tui applies at its render seam.
        snapshot: fauna_client_family::SupervisionSnapshot,
        /// The supervised viewer's OWN cross-device usage total for their
        /// current local day (family-safety.md § Screen time), from
        /// `status.usage_today_minutes`. Seeds the heartbeat so the first paint
        /// can already evaluate the budget, and is the number the ward's own
        /// read-only summary shows — the same one their guardian sees.
        /// `None` when unsupervised or no daily budget is set.
        usage_today_minutes: Option<u32>,
        /// The supervised caller's OWN pending contact asks
        /// (`status.contact_requests`) and feed-source asks
        /// (`status.feed_requests`) — folded into `crate::ward_asks`, gated on
        /// `supervised_by`, for the refused-send surfaces that read them back
        /// (`family-safety.md` § Child-initiated contact requests → *Ward
        /// transparency*, § Feed-source approvals).
        contact_requests: Vec<fauna_client_family::family::FamilyContactRequestInfo>,
        feed_requests: Vec<fauna_client_family::family::FamilyFeedRequestInfo>,
    },
    AdminStatsLoaded {
        stats: serde_json::Value,
    },
    /// Typed user page (`fauna.admin.users.list`, via the shared `AdminClient`)
    /// — backs the consolidated `admin-users` Users section + the dashboard
    /// recent-users group. Replaces the `/admin/api/users` JSON twin. Rides with
    /// every account on the nest (`fauna_client_admin::users_list_all`), the
    /// admin actor pickers' option source, so the page and its pickers land in
    /// one message; `picker_users` is `None` when that read failed, and the
    /// pickers keep the list they had.
    AdminUsersLoaded {
        reply: fauna_client_admin::admin::AdminUsersListReply,
        picker_users: Option<Vec<fauna_client_admin::AdminUser>>,
    },
    AdminServerStatusLoaded {
        status: serde_json::Value,
    },
    /// The defined tiers (`fauna.admin.tiers.list`) — the `admin-users` tier
    /// pickers cycle through these names (the tier is the quota).
    AdminTiersLoaded {
        reply: fauna_client_admin::admin::AdminTiersListReply,
    },
    /// A user's tier was changed / eviction started or cancelled
    /// (`fauna.admin.users.{update,evict,cancel_eviction}` ok) — triggers a users
    /// refetch (at the current page) so the row reflects the new state.
    AdminUserUpdated,
    /// A tier definition's caps were saved (`fauna.admin.tiers.update` ok) —
    /// triggers a tiers refetch so the `admin-settings` rows re-render from the
    /// persisted state (admin.md § 3 — in-place tier-cap editing).
    AdminTierUpdated,
    /// The calling admin's membership designations (`fauna.admin.membership_tiers.list`
    /// — monetization.md § Pillar 4). One of two sources the membership section
    /// renders from; races `OwnMembershipTierNamesLoaded` (both handlers
    /// re-render, mirroring `refresh_guardian_pickers`).
    AdminMembershipTiersLoaded {
        reply: fauna_client_admin::admin::AdminMembershipTiersListReply,
    },
    /// The calling admin's own subscription tier names
    /// (`fauna.subscriptions.tiers.list`) — the membership section's row set
    /// (one row per owned tier).
    OwnMembershipTierNamesLoaded {
        names: Vec<String>,
    },
    /// A membership designation was set or cleared
    /// (`fauna.admin.membership_tiers.{set,clear}` ok) — triggers a
    /// designations refetch so the row re-renders from persisted state.
    AdminMembershipTierUpdated,
    /// Deployment registration posture (`fauna.setup.status` reply
    /// `registration_mode` raw wire string + `max_free_users`) — seeds the
    /// `admin-users-registration-section` (admin.md § 2 Users → Section 2 —
    /// Registration). `mode` is deliberately the raw wire string, never
    /// pre-parsed: `None` (field absent, a non-conforming nest) and an unrecognized
    /// string (a newer nest) both mean "this client cannot name the posture" and
    /// render the section fully read-only
    /// (`public-mode.md` § Registration Modes + Implementation status today).
    RegistrationModeLoaded {
        mode: Option<String>,
        max_free_users: Option<u64>,
        /// `SetupStatusReply.age_verification_required` — seeds the age
        /// require-knob (`admin-users-registration-age-verification-toggle`).
        age_verification_required: bool,
    },
    /// `fauna.admin.set_registration_mode` succeeded — refetch `setup.status` so
    /// the section re-seeds from the persisted posture (the re-read IS the
    /// confirmation; admin.md § 2 specifies no separate confirmation element).
    RegistrationModeSaved,
    /// Deployment client-facing API serving port (`fauna.setup.status` reply
    /// `serving_port`, default 443) — seeds the `admin-nest-serving-port-input`.
    /// `fronted` (`fauna.setup.status` reply `fronted_by_router`) renders the
    /// field read-only when the nest sits behind the cloud :443 SNI router (the
    /// chosen port is inert there) — nest/common.md § Serving ports.
    NestServingPortLoaded {
        port: u16,
        fronted: bool,
    },
    /// `fauna.admin.set_serving_port` succeeded — refetch `setup.status` so the
    /// `admin-nest-serving-port-input` re-seeds from the persisted port.
    NestServingPortSaved,
    /// The declared region (`fauna.admin.region.get`, folded through
    /// `fauna_client_admin::admin_region_view`) — fills the
    /// `admin-nest-region-*` section (region-blocking.md § Region
    /// determination). Every rendering decision lives in the fold; this app
    /// paints exactly what it hands back.
    NestRegionLoaded {
        view: fauna_client_admin::AdminRegionView,
    },
    /// `fauna.admin.region.set` succeeded (declare or withdraw) — refetch
    /// `fauna.admin.region.get` so the section re-seeds from the persisted
    /// declaration.
    NestRegionSaved,
    /// Host-OS-maintenance state (`fauna.setup.status` `os_*`) — fills the
    /// `nest-os-maintenance-status` line + the `nest-os-updates-count` badge +
    /// gates the `nest-os-restart-now-button` on admin-nest (installers/vps.md
    /// § Host OS Maintenance § 4).
    NestOsMaintenanceLoaded {
        security_updates_pending: u32,
        reboot_pending: bool,
    },
    /// `fauna.admin.request_host_restart` succeeded — refetch `setup.status` so
    /// the host-OS-maintenance indicator reflects the requested restart.
    NestHostRestartRequested,
    /// The sidecar-service enable flags (`fauna.admin.services.list`) — the
    /// `admin-service-pairing-toggle` + its status badge on
    /// `admin-nest` (admin.md § N Nest).
    AdminServicesLoaded {
        reply: fauna_client_admin::admin::AdminServicesListReply,
    },
    /// A service flag was flipped (`fauna.admin.services.update` ok) — triggers a
    /// services refetch so the toggle + status reflect the applied state.
    AdminServiceUpdated,
    /// The nest's `fauna-log` ring (`fauna.admin.logs`) — the `admin-logs` page
    /// (observability.md § Surfaces), rendered with the client Logs widget.
    AdminLogsLoaded {
        reply: fauna_client_admin::admin::AdminLogsReply,
    },
    /// Typed invite codes (`fauna.admin.invite_codes.list`) — the `admin-users`
    /// Invite section list. Replaces the `/admin/api/invite-codes` JSON twin.
    AdminInviteCodesLoaded {
        reply: fauna_client_admin::admin::AdminInviteCodesListReply,
    },
    /// A freshly minted invite code (`fauna.admin.invite_codes.create`) — its
    /// `code` is the token the Invite section surfaces copyable (mint-on-empty).
    AdminInviteCodeCreated {
        reply: fauna_client_admin::admin::AdminInviteCodeCreateReply,
    },
    /// An invite code was deleted (`fauna.admin.invite_codes.delete` ok) —
    /// triggers an invite-codes refetch.
    AdminInviteCodeDeleted,
    /// Typed invite requests (`fauna.admin.invite_requests.list`) — the
    /// `admin-users` Pending requests section. Replaces the JSON twin.
    AdminInviteRequestsLoaded {
        reply: fauna_client_admin::admin::AdminInviteRequestsListReply,
    },
    AdminInviteRequestDecided,
    /// The deployment's local mail domains (`fauna.bridges.list_local_domains`
    /// over WS-RPC, via the shared `LocalDomainMachine`). Backs the admin
    /// Settings page's email-domains section.
    AdminLocalDomainsLoaded {
        snapshot: fauna_client_mail_settings::local_domains::LocalDomainsSnapshot,
    },
    /// The pending-bridge approval feed (`fauna.bridges.list_pending_bridges`
    /// over WS-RPC, via the shared `BridgeApprovalMachine`). Backs the admin
    /// `admin-bridges-pending` approval page.
    AdminPendingBridgesLoaded {
        snapshot: fauna_client_mail_settings::bridge_approval::BridgeApprovalSnapshot,
    },
    /// The nest-wide custody-hosting registry (`fauna.admin.custody_hosting.list`,
    /// via the shared `AdminHostingClient`). Backs the admin
    /// `admin-custody-hosting` page.
    AdminCustodyHostingLoaded {
        snapshot: views::admin::AdminHostingSnapshot,
    },
    /// The deployment's external mail forwarders + hosted-domain list
    /// (`fauna.bridges.{list_forwarders,list_local_domains}` over WS-RPC, via the
    /// shared `ForwarderMachine`). Backs the admin `admin-aliases` page
    /// (forwarder half — admin.md § 4).
    AdminForwardersLoaded {
        snapshot: fauna_client_mail_settings::forwarders::ForwardersSnapshot,
    },
    /// The unified-DNS per-domain record matrix + live verdicts
    /// (`fauna.dns.{list_records,verify_records}` over WS-RPC, via the shared
    /// `DnsManagementMachine`). Backs the admin `admin-dns` page.
    AdminDnsRecordsLoaded {
        snapshot: fauna_client_dns::DnsSnapshot,
    },
    /// The enrolled-device roster for one expanded folder row
    /// (`fauna.folders.members.list`), lazy-loaded when the row expands. The
    /// page-level device / folder / conflict lists render off the shared
    /// `DevicesMachine` snapshot directly — only this row-detail read still flows
    /// through a `DataMessage`.
    FolderMembersLoaded {
        name: String,
        members: Vec<fauna_client_folders::folders::FolderMember>,
    },
    /// The cross-user **actor** roster (the owner-side "Shared with" list,
    /// `fauna.folders.members.list_actors`) for one folder — lazy on expand,
    /// eager for already-shared rows, and re-read after a share/remove write.
    /// `channel_id` is the set's derived `ChannelId` (hex) when shared, threaded
    /// through so the row's `folder-member-remove-button`s can address the set;
    /// `None` for an owner-only set (empty roster). See `folders.md` § Sharing.
    FolderActorsLoaded {
        name: String,
        members: Vec<fauna_client_folders::folders::FolderActorMember>,
        channel_id: Option<String>,
    },
    /// The per-set **device-activity** roster (`fauna.folders.devices`) — the
    /// ordinary sync change signal (`label` + `change_count` per device),
    /// distinct from the enrolled-member roster (`FolderMembersLoaded`) and
    /// the cross-user actor roster (`FolderActorsLoaded` above). Lazy on first
    /// expand AND re-fetched on every `fauna.sync.changed` push while the row
    /// stays expanded (`PushEvent::SyncChanged` below) — that live-update path
    /// is the entire point of the feature: it is what makes web's
    /// remote-change nudge e2e-pinnable
    /// (`docs/goal/behavior/file-sync.md` § Implementation status today).
    FolderDevicesLoaded {
        name: String,
        devices: Vec<fauna_client_folders::folders::FolderDevice>,
    },
    /// The per-set **destination places** — this owner's enrolled backup
    /// destinations, each marked attached-or-not for the row's `folder_id`
    /// (`fauna.backup.destination.list`, `docs/goal/behavior/backup-destinations.md`
    /// § Ordinary-folder coverage). Lazy on first expand, and re-sent after
    /// every attach/detach so the section repaints from the nest's own
    /// answer — never an optimistic flip.
    FolderDestinationsLoaded {
        name: String,
        folder_id: i64,
        places: Vec<fauna_client_config::FolderDestinationPlace>,
    },
    /// The recipient-side **pending folder shares** — the staged ("knocked")
    /// cross-user shares a stranger sent, awaiting accept/decline (the page-level
    /// "Shared with you" section, `folders.md` § Sharing — Recipient side).
    /// A page-level read (not `DevicesMachine` state); fetched on auth +
    /// folders-page-visible + re-fetched after an accept/decline write.
    FolderPendingSharesLoaded {
        shares: Vec<crate::client::PendingShareView>,
    },
    /// The co-present ceremony's group-share surface — pending consent-card
    /// invitations AND the shared sets this device can actually read
    /// (`p2p.md` § Offline share initiation, row 334). A page-level read;
    /// fetched on auth + folders-page-visible, same cadence as
    /// [`Self::FolderPendingSharesLoaded`].
    #[cfg(feature = "p2p-share")]
    GroupSharesLoaded {
        views: crate::offline_share::GroupShareViews,
    },
    /// The ceremony seat bound (or already-bound and handed straight back)
    /// after `offline-share-button` / `offline-receive-button` opened a
    /// panel. Payload-free: the seat sits in the panel state's `SessionSeat`
    /// the bind went through.
    #[cfg(feature = "p2p-share")]
    OfflineShareSeatBound,
    /// The share plane's driver bound — or was handed — this session's seat
    /// (`crate::share_glue`, row 338). Payload-free for the same reason: both
    /// doors go through the one `offline_share::SessionSeat`, so the seat is
    /// the panel's by construction and there is nothing to fold.
    #[cfg(feature = "p2p-share")]
    SharePlaneSeatBound,
    /// A share pump pass changed the transfer surface's state cell
    /// (`fauna_sync_engine::share_glue::SharePlaneState`). Payload-free: the
    /// handler repaints from the fresh cell, and it is posted only on actual
    /// change.
    #[cfg(feature = "p2p-share")]
    SharePlaneChanged,
    /// This side's ceremony progressed — begin / consent / decline all
    /// report through this one variant, mirroring `CeremonyStatus`'s own
    /// single-state shape.
    #[cfg(feature = "p2p-share")]
    OfflineShareProgressed {
        status: fauna_client_capabilities::group_ceremony_view::CeremonyStatus,
    },
    /// A ceremony step failed — the brake refused, the dial failed, or a
    /// consent/decline round-trip errored. The reason rides the Folders
    /// sub-page's own `error-message` (convention 2).
    #[cfg(feature = "p2p-share")]
    OfflineShareFailed {
        message: String,
    },
    /// The account store may have changed — this runtime's own pump changed an
    /// entry a read can answer, or another connection committed (a sibling
    /// same-account instance, the sync agent). Payload-free, posted by
    /// `crate::store_surfaces::watch`; the handler re-drives the OPEN
    /// store-backed surface's own load (`account-runtime.md` § Multi-instance
    /// concurrency → *A runtime's own pump is a source of the notice too*).
    AccountStoreChanged,
    /// `fauna.admin.factory_reset` returned the post-reset claim code. The nest
    /// is now restarting into the wipe; the handler tears down the authenticated
    /// session (keeping local credentials — the identity is still valid for the
    /// re-claim) and re-seeds onboarding at the claim-code step with the code
    /// pre-filled (the human never sees it). Carries the re-onboard seed the
    /// client already holds so the handler doesn't re-read libsecret.
    /// See `docs/goal/behavior/mail-bridge-lifecycle.md` § Factory reset.
    FactoryResetComplete {
        claim_code: String,
        nest_url: String,
        secret_hex: String,
        handle: String,
    },
    /// `fauna.admin.factory_reset` failed before the nest exited (the call
    /// errored, so the box is untouched). Surfaced on the admin-settings page.
    FactoryResetFailed {
        error: String,
    },
    /// The deployment-identity rotation confirm's roster read resolved (arm
    /// click → `fauna.admin.admins.list` + a per-admin `fauna.admin.users.get`
    /// join, folded by the shared `seed_rotation_confirm_view`) —
    /// `views::admin::render_seed_rotate` paints it (`admin-nest-seed-rotate-*`,
    /// `box-recovery.md` § Deployment-seed rotation).
    SeedRotateRosterLoaded {
        state: views::admin::SeedRotateConfirmState,
    },
    /// The deployment-seed rotation ceremony's own verdict sentence
    /// (`fauna_client_config::seed_rotation_verdict`) — `predecessor_marked:
    /// false` renders as a *success with a caveat*, never an error, so this is
    /// carried as plain status text rather than routed through
    /// `ActionResult::Failed`.
    SeedRotated {
        status: String,
    },
    /// The outside-app sign-in key set's read (`fauna.oauth.issuer_key_status`,
    /// folded by the shared `AdminClient::issuer_key_status_view`) — the
    /// `admin-nest-oauth-*` section's key rows, or its worded reason line when
    /// the read failed; never a page error (`authorization-server.md` § The
    /// issuer). `views::admin::set_oauth_keys` paints it.
    OauthKeysLoaded {
        keys: views::admin::OauthKeysRead,
    },
    /// An `admin-nest-oauth-*` control's verdict (the shared verdict folds — a
    /// failure is a verdict too, so never `ActionResult::Failed`) and the key
    /// set re-read after it, carried together so the list and the sentence land
    /// in one paint (tui's `Outcome::OauthDone`).
    OauthDone {
        status: String,
        keys: views::admin::OauthKeysRead,
    },
    /// The legal-takedown console's dispatch verdict
    /// (`fauna_client_moderation::takedown_verdict`) — success is the common
    /// case, so this is plain status text, not `error-message` material.
    TakedownSubmitted {
        status: String,
    },
}

#[derive(Debug, Clone)]
pub enum ActionResult {
    Success {
        context: String,
    },
    /// Non-HTTP failure: network error, body read error, token issuance failure.
    /// Rendered as `"{context}: {error}"` — the `context` is a bare prefix (often a
    /// raw RPC method name).
    Failed {
        context: String,
        error: String,
    },
    /// A failure whose message is **already complete and localized** — the caller
    /// resolved a `{message}`-carrying i18n key (`devices::error_share_set(&detail)`)
    /// and there is nothing left to append. Rendered verbatim.
    ///
    /// Use this, not `Failed`, whenever the i18n key carries its own `{message}`:
    /// pairing such a key with `Failed`'s prefix-append would print the detail twice,
    /// and pairing it with a hand-built `format!("{key}: {detail}")` is precisely the
    /// per-app concatenation the copy standard removed (`docs/goal/ui/README.md`
    /// § Copy comprehensibility).
    FailedLocalized {
        message: String,
    },
    /// HTTP 4xx/5xx response from the nest, with its structured `{"error": ...}`
    /// body already deserialized into `message`.
    ApiFailed {
        endpoint: String,
        status: u16,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub enum WsEvent {
    Connected,
    Connecting,
    Disconnected,
    /// Connecting has failed enough times running that the gap is no longer
    /// transient — the indicator says "Cannot connect" instead of an indefinite
    /// "Connecting…". `fauna_ws_substrate::supervisor::ConnectionState::Unreachable`.
    Unreachable,
    /// Fired on every reconnect (a `Connected` after the first connect), distinct
    /// from `Connected` (which also fires on the initial connect). Drives the
    /// snapshot re-hydrate — the feed has no poll backstop. transport.md § Push
    /// events: observers re-pull through their snapshot-refresh path on reconnect.
    Reconnected,
    Push(Box<fauna_client::PushEvent>),
}

// ---------------------------------------------------------------------------
// Application state — shared via Rc<RefCell<...>> on the GTK main thread
// ---------------------------------------------------------------------------

/// Mutable application state that is updated from the polling loop and read
/// by view-update helpers.
#[allow(dead_code)]
#[derive(Default)]
pub struct AppState {
    pub actor_id: String,
    pub handle: String,
    pub node_url: String,
    pub connected: bool,
    /// Contacts from the most recent ContactsLoaded.
    pub contacts: Vec<crate::rows::ContactRow>,
    /// This window's post-succession review roster — the one copy the contacts
    /// badge and the conversations page's member-chip pair both read (the
    /// conversations view is handed this same `Rc` at build), written only by
    /// the `MemberReviewsLoaded` arm (`settings::member_review::Roster`).
    pub member_reviews: Rc<crate::settings::member_review::Roster>,
    /// Incoming knocks from the most recent KnocksLoaded.
    pub knocks: Vec<crate::rows::KnockRow>,
    /// Events from the most recent EventsLoaded.
    pub events: Vec<crate::rows::EventRow>,
    /// Unified notification unread count.
    pub notifications_unread_count: u32,
    /// Sync files from the most recent SyncFilesLoaded.
    pub sync_files: Vec<crate::rows::SyncFileRow>,
    /// MLS encryption manager — created on auth success, `None` before that.
    pub mls: Option<std::sync::Arc<crate::mls::MlsManager>>,
    /// P2P service — created on auth success, `None` before that.
    pub p2p: Option<std::sync::Arc<crate::p2p::P2pService>>,
    /// The recipient-side pending folder shares from the most recent
    /// `FolderPendingSharesLoaded` — cached (not just re-painted) because the
    /// co-present ceremony's group invitations continue the SAME indexed
    /// `folder-pending-share` list (the consent card
    /// "reuses the knock trio"), and the two sources are two independent
    /// async round-trips that must never clobber each other's already-painted
    /// rows. Either handler writes its own cache field, then both call the
    /// one combined [`views::devices_folders::folders::populate_pending_shares`].
    pub folder_pending_shares: Vec<crate::client::PendingShareView>,
    /// The co-present ceremony's group-share surface from the most recent
    /// `GroupSharesLoaded` — see [`Self::folder_pending_shares`] for why this
    /// is cached rather than painted inline.
    #[cfg(feature = "p2p-share")]
    pub group_shares: crate::offline_share::GroupShareViews,
}

// ---------------------------------------------------------------------------
// Widget handles — references to widgets that need dynamic updates
// ---------------------------------------------------------------------------

/// Holds references to widgets that the polling loop must update when new
/// data arrives.
#[allow(dead_code)]
pub struct WidgetHandles {
    // Global connection-status indicator at the top of the sidebar
    // (`connection-status`): live state of the nest WS-RPC connection, driven by
    // the `ConnectionState` watch. Distinct from the per-detail status view's
    // `status_connection_row`.
    pub connection_status_icon: gtk::Image,
    pub connection_status_label: gtk::Label,
    // Global local sync-agent process health indicator (`sync-agent-status`),
    // just below connection-status. Distinct concept: connection-status is the
    // nest WS-RPC link, this is the LOCAL fauna-sync-agent process
    // (sync-agent.md § Local agent health). Driven by a periodic
    // `GetServiceStatus` poll, not a push event (the agent has no health-push
    // channel — see `sync_agent.rs::start_status_poll`).
    pub sync_agent_status_icon: gtk::Image,
    pub sync_agent_status_label: gtk::Label,
    pub sync_agent_status_version_label: gtk::Label,
    pub sync_agent_status_uptime_label: gtk::Label,
    // Status view rows
    pub status_actor_id_row: adw::ActionRow,
    pub status_handle_row: adw::ActionRow,
    pub status_node_url_row: adw::ActionRow,
    pub status_connection_row: adw::ActionRow,
    pub status_quota_tier_row: adw::ActionRow,
    pub status_quota_inbox_row: adw::ActionRow,
    pub status_quota_storage_row: adw::ActionRow,
    pub status_quota_devices_row: adw::ActionRow,
    pub status_quota_section_marker: gtk::Label,
    pub status_quota_inbox_marker: gtk::Label,
    pub status_quota_storage_marker: gtk::Label,
    pub status_quota_devices_marker: gtk::Label,
    pub status_feature_limits_group: adw::PreferencesGroup,
    pub status_feature_limits_rows_box: gtk::Box,
    pub status_feature_limits_section_marker: gtk::Label,
    pub status_region_rows_box: gtk::Box,
    pub status_node_domain_row: adw::ActionRow,
    pub status_node_version_row: adw::ActionRow,
    pub status_files_row: adw::ActionRow,
    pub status_last_sync_row: adw::ActionRow,

    // Notifications list — rebuilt on every `NotificationsLoaded` (login fetch,
    // `fauna.notification` push, `ResyncRequired`, `Reconnected`), so the page is
    // live while mounted rather than a permanently-empty shell.
    pub notifications_list_box: gtk::ListBox,
    pub notifications_count_badge: gtk::Label,

    // Contacts / knocks lists
    pub knocks_list_box: gtk::ListBox,
    pub contacts_list_box: gtk::ListBox,
    pub find_results_list_box: gtk::ListBox,
    // Roster-filter side-index (actor_id → handle/domain) read by the contacts
    // search filter; refreshed on every `update_contacts_list`.
    pub contacts_filter_index: crate::views::contacts::list::ContactFilterIndex,

    // Address Book segment (CardDAV vCards — slice 4b): the book-picker + card
    // list + detail-pane handles, repopulated on AddressbooksLoaded / CardsLoaded.
    pub address_book: crate::views::contacts::address_book::AddressBookHandles,

    // Events — calendar view handles
    pub events_handles: crate::views::events::EventViewHandles,

    // Feed — only the post-list content stack is reached from `app.rs` (to show
    // the Bluesky thread view); the rest of the Feed page is self-managed by its
    // `FeedManager` observer loop.
    pub feed_content_stack: gtk::Stack,

    // Page-level Media state machine (the cross-set explorer, content plane) —
    // refreshed on auth, on page-visible, and on the remote-change nudge while
    // the page is on screen. The page renders off its own observer loop; app.rs
    // kicks the initial refresh and the push-driven one.
    pub media_machine: std::sync::Arc<fauna_media_machine::MediaMachine>,
    // The Media page root, so the `PushEvent::SyncChanged` arm can ask GTK
    // whether Media is on screen before re-reading the cross-set aggregate
    // (`views::media::media_page_is_visible`).
    pub media_page: gtk::Box,

    // Bridges list + split view
    pub bridges_list_box: gtk::ListBox,
    pub bridges_split: adw::NavigationSplitView,
    // Most-recent `fauna.bridges.list` snapshot (id → BridgeStatus), retained so
    // the detail pane renders the metadata-driven link form from `link_modes`.
    pub bridges_snapshot: views::bridges::BridgesSnapshot,
    // `(bridge_id, follows_list)` of the currently-open Bridges-page detail
    // pane, if any — consulted by the `BridgeFollowsLoaded` handler so a
    // `fauna.bridges.list_follows` reply repaints the right widget.
    pub bridges_open_detail: Rc<RefCell<Option<(String, gtk::ListBox)>>>,

    // Moderation queue list (`moderation-queue`) — repainted on
    // `ModerationActionsLoaded`.
    pub moderation_queue_list_box: gtk::ListBox,

    // Folder list (peers page) — kept so `FolderMembersLoaded` populates the
    // lazy-loaded member roster into the right expander row.
    pub folder_list_box: gtk::ListBox,

    // Recipient-side "Shared with you" pending-share section (group + inner list),
    // repainted on `FolderPendingSharesLoaded` (a page-level read).
    pub folder_pending_shares_group: adw::PreferencesGroup,
    pub folder_pending_shares_box: gtk::ListBox,

    // The co-present offline-share ceremony's panel widgets + state
    // (`p2p.md` § Offline share initiation, row 334), repainted on
    // `OfflineShareSeatBound` / `OfflineShareProgressed` / `OfflineShareFailed`
    // and read by `FolderPendingSharesLoaded` / `GroupSharesLoaded`'s combined
    // `populate_pending_shares` + `populate_group_invitations` call.
    #[cfg(feature = "p2p-share")]
    pub offline_share: views::devices_folders::folders::OfflineShareHandles,
    #[cfg(feature = "p2p-share")]
    pub offline_share_state: Rc<RefCell<crate::offline_share::OfflineShareState>>,
    // Repaints the ceremony's shared-set `folder-row`s from the scopes half of
    // `GroupSharesLoaded` (`DevicesFoldersHandles::repaint_group_scopes`).
    #[cfg(feature = "p2p-share")]
    pub repaint_group_scopes: views::devices_folders::RepaintGroupScopes,
    // The peer-transfer surface (`p2p.md` § Cross-user shared-set transfer,
    // row 338), repainted on `SharePlaneChanged` from the plane's own state
    // cell — render-only, no gestures.
    #[cfg(feature = "p2p-share")]
    pub share_transfer: views::devices_folders::folders::ShareTransferHandles,

    // Page-level Devices state machine — refreshed on auth + page-visible.
    pub devices_machine: std::sync::Arc<fauna_devices_machine::DevicesMachine>,

    // Page-level community-labeler-catalog state machine (Settings →
    // Personalization + Settings → Community labelers) — refreshed on auth +
    // page-visible.
    pub labeler_catalog_machine:
        std::sync::Arc<fauna_labeler_catalog_machine::LabelerCatalogMachine>,

    // Backups. The snapshot half is `BackupsMachine`-driven and self-painting;
    // app.rs holds only the machine (to refresh it on auth) and the restore
    // half's handles, which keep their own `fauna-client-snapshots` seams.
    pub backups_machine: std::sync::Arc<fauna_backups_machine::BackupsMachine>,
    pub snapshot_file_list_box: gtk::ListBox,
    pub backup_detail_header: adw::HeaderBar,
    pub backups_restore: views::backups::restore::RestoreHandles,

    // Search entry widget (for Ctrl+F focus) — everything else on the Search
    // page is self-managed by `views::search::build_search_view`'s own
    // observer refresh loop (the `SearchManager` paint-shell shape).
    pub search_entry: gtk::SearchEntry,

    // Toast overlay for error/info messages
    pub toast_overlay: adw::ToastOverlay,

    // Persistent banner labels (hidden by default; shown via set_error_message or test agent)
    pub error_label: gtk::Label,
    pub warning_label: gtk::Label,
    pub info_label: gtk::Label,

    // The window itself (for compose dialog)
    pub window: adw::ApplicationWindow,

    // Admin dashboard handles
    pub admin_handles: crate::views::admin::AdminHandles,

    // Sidebar list box — needed to show/hide the gated admin + family rows
    pub sidebar_list_box: gtk::ListBox,

    /// The global `supervised-indicator` header button (family-safety.md
    /// § App surface). Hidden until `FamilyStatusLoaded` reports a guardian.
    pub supervised_indicator: gtk::Button,

    /// The global `screen-time-lock` overlay (family-safety.md § Screen time).
    /// Repainted by `FamilyStatusLoaded`, by every navigation, and by the
    /// one-minute tick; hidden whenever the ward is not locked (and always on
    /// the family page, which stays reachable read-only).
    pub screen_lock_overlay: gtk::Box,

    /// The main content stack — needed here (not just on `MainWindowResult`)
    /// because the lock verdict is page-dependent, so a status read must be
    /// able to ask which page is showing.
    pub content_stack: gtk::Stack,
    /// The Settings sub-stack and its store-backed sub-pages' re-drives — what
    /// the `AccountStoreChanged` handler needs to re-run the open one's load
    /// (`crate::store_surfaces`).
    pub settings_sub_stack: gtk::Stack,
    pub settings_store_resync: Rc<dyn Fn(crate::store_surfaces::StoreSurface)>,
}

impl WidgetHandles {
    /// Rebuild the Status page's `StatusHandles` from the flat widget-handle
    /// fields — the seam every `Data*Loaded` arm that updates a Status row
    /// goes through (`QuotaLoaded`, `NestInfoLoaded`, `SyncStatusSummaryLoaded`),
    /// so a new `StatusHandles` field only needs updating here, not at every
    /// call site (a struct-literal duplicated across arms is exactly how a
    /// field-add silently misses one — `wire-struct-field-breaks-explicit-
    /// construction-sites`).
    fn status_handles(&self) -> views::status::StatusHandles {
        views::status::StatusHandles {
            actor_id_row: self.status_actor_id_row.clone(),
            handle_row: self.status_handle_row.clone(),
            node_url_row: self.status_node_url_row.clone(),
            connection_row: self.status_connection_row.clone(),
            quota_tier_row: self.status_quota_tier_row.clone(),
            quota_inbox_row: self.status_quota_inbox_row.clone(),
            quota_storage_row: self.status_quota_storage_row.clone(),
            quota_devices_row: self.status_quota_devices_row.clone(),
            quota_section_marker: self.status_quota_section_marker.clone(),
            quota_inbox_marker: self.status_quota_inbox_marker.clone(),
            quota_storage_marker: self.status_quota_storage_marker.clone(),
            quota_devices_marker: self.status_quota_devices_marker.clone(),
            feature_limits_group: self.status_feature_limits_group.clone(),
            feature_limits_rows_box: self.status_feature_limits_rows_box.clone(),
            region_rows_box: self.status_region_rows_box.clone(),
            feature_limits_section_marker: self.status_feature_limits_section_marker.clone(),
            node_domain_row: self.status_node_domain_row.clone(),
            node_version_row: self.status_node_version_row.clone(),
            files_row: self.status_files_row.clone(),
            last_sync_row: self.status_last_sync_row.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Build result
// ---------------------------------------------------------------------------

pub struct MainWindowResult {
    pub window: adw::ApplicationWindow,
    pub state: Rc<RefCell<AppState>>,
    pub widgets: Rc<WidgetHandles>,
    /// The content stack — `visible_child_name()` gives the active sidebar section.
    pub stack: gtk::Stack,
    /// Rebuild the Profile page for a target and show it: `None` = the viewer's
    /// own profile, `Some(hex)` = another actor's. This is *the* way to reach a
    /// specific actor's profile — the page is built per target, so switching the
    /// stack alone always lands on SELF. Handed out so the launch/relaunch paths
    /// can give it to the test-command drain, whose `{"view":"profile",
    /// "actor_id":…}` nav needs the same rebuild the sidebar and the contacts
    /// tap-through get.
    pub open_profile: Rc<dyn Fn(Option<String>)>,
}

// ---------------------------------------------------------------------------
// Build the main window
// ---------------------------------------------------------------------------

/// Hard-minimum window size. Also the floor used when clamping a restored
/// geometry to the current monitor, so the two never disagree.
const MIN_WINDOW_WIDTH: i32 = 800;
const MIN_WINDOW_HEIGHT: i32 = 500;

/// Clamp a restored window size to the current monitor geometry.
///
/// A persisted geometry can be larger than the screen the app now runs on — the
/// state was saved on a bigger display, the user moved monitors, or a Wayland
/// scale-factor quirk inflated the saved `window.width()`. GTK restores
/// `default_width`/`default_height` verbatim with **no** bound to the screen
/// (unlike macOS/Windows, whose window managers re-constrain restored frames),
/// so without this the window opens wider/taller than the display and can't be
/// dragged back to a usable size. Cap each axis at the largest available
/// monitor — we don't yet know which one the window maps onto — leaving a small
/// margin for a panel / CSD shadow, and never below the minimum request. When
/// no monitor is known (headless/CI) the saved size is trusted unchanged.
fn clamp_to_monitor(width: i32, height: i32) -> (i32, i32) {
    let Some(display) = gtk::gdk::Display::default() else {
        return (width, height);
    };
    let monitors = display.monitors();
    let (mut max_w, mut max_h) = (0, 0);
    for i in 0..monitors.n_items() {
        let Some(monitor) = monitors
            .item(i)
            .and_then(|o| o.downcast::<gtk::gdk::Monitor>().ok())
        else {
            continue;
        };
        let geo = monitor.geometry();
        max_w = max_w.max(geo.width());
        max_h = max_h.max(geo.height());
    }
    clamp_dims(width, height, max_w, max_h)
}

/// Pure clamp arithmetic, split out from [`clamp_to_monitor`] so it is
/// unit-testable without a live display. Caps `(width, height)` at 96% of the
/// monitor bounds `(max_w, max_h)` — a margin for a panel / window shadow — and
/// never below the minimum request. `max_w`/`max_h` of 0 (no monitor known)
/// returns the saved size unchanged.
fn clamp_dims(width: i32, height: i32, max_w: i32, max_h: i32) -> (i32, i32) {
    if max_w <= 0 || max_h <= 0 {
        return (width, height); // no usable monitor info — trust the saved size
    }
    let cap_w = ((f64::from(max_w) * 0.96) as i32).max(MIN_WINDOW_WIDTH);
    let cap_h = ((f64::from(max_h) * 0.96) as i32).max(MIN_WINDOW_HEIGHT);
    (
        width.clamp(MIN_WINDOW_WIDTH, cap_w),
        height.clamp(MIN_WINDOW_HEIGHT, cap_h),
    )
}

#[cfg(test)]
mod window_clamp_tests {
    //! Regression for the "window far too wide for the screen" bug: a saved
    //! geometry larger than the current monitor was restored verbatim.
    use super::{MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH, clamp_dims};

    #[test]
    fn oversized_saved_geometry_is_capped_to_the_screen() {
        // The exact reported case: 3959×1150 saved (from a larger display),
        // restored on a 1710×1073 screen — must not exceed the screen.
        let (w, h) = clamp_dims(3959, 1150, 1710, 1073);
        assert!(w <= 1710, "width {w} still exceeds the 1710px screen");
        assert!(h <= 1073, "height {h} still exceeds the 1073px screen");
        assert_eq!(w, 1641); // 0.96 * 1710
        assert_eq!(h, 1030); // 0.96 * 1073, below the saved 1150
    }

    #[test]
    fn geometry_within_screen_is_left_unchanged() {
        assert_eq!(clamp_dims(1100, 700, 1710, 1073), (1100, 700));
    }

    #[test]
    fn clamp_never_drops_below_the_minimum_request() {
        // Even a pathologically small monitor yields at least the min size.
        assert_eq!(
            clamp_dims(1100, 700, 400, 300),
            (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT)
        );
    }

    #[test]
    fn no_monitor_info_trusts_the_saved_size() {
        assert_eq!(clamp_dims(3959, 1150, 0, 0), (3959, 1150));
    }
}

/// Build and return the main application window plus shared state and widget
/// handles. The caller should call `.present()` on `result.window`.
pub fn build_main_window(
    app: &adw::Application,
    fauna_client: &Rc<FaunaClient>,
) -> MainWindowResult {
    let state = Rc::new(RefCell::new(AppState::default()));

    // Load persisted window state (falls back to defaults if file absent).
    let saved_state = crate::window_state::load_window_state().unwrap_or_default();

    // -----------------------------------------------------------------------
    // Content stack — one child per sidebar section
    // -----------------------------------------------------------------------
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(150);
    // Size the content area to the VISIBLE page only. A GtkStack is homogeneous
    // by default, so it requests the max width/height over ALL pages — meaning a
    // single wide page (e.g. the calendar, or a page with an over-wide element)
    // forces the whole window wide even while a narrow page is shown. The pages
    // here are deliberately heterogeneous, and AdwClamp already governs the
    // visible content width, so per-page sizing is correct.
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);

    // Conversations — observer-driven, owns its own state via the
    // shared ConversationsManager singleton (linux is in-process Rust).
    // The tokio runtime is passed so the compose bar's send buttons can
    // submit outbound mail through the manager (the SMTP rail is
    // session-registered at AuthSuccess and reads the live self-address
    // cell at send time). The member-review roster is the one exception to
    // "owns its own state": it is this window's, shared with the contacts badge.
    let conversations_view = conversations::build_conversations_view(
        crate::conversations::manager(),
        Rc::clone(fauna_client),
        fauna_client.runtime_handle(),
        Rc::clone(&state.borrow().member_reviews),
    );
    stack.add_named(&conversations_view, Some("conversations"));

    let (
        contacts_view,
        knocks_list_box,
        contacts_list_box,
        find_results_list_box,
        contacts_filter_index,
        address_book,
        switch_to_addressbook_segment,
    ) = views::contacts::build_contacts_view(Rc::clone(fauna_client));
    stack.add_named(&contacts_view, Some("contacts"));

    // Profile — the canonical per-user detail surface (profile.md). Rebuilt per
    // target by `open_profile`: None = the viewer's own profile (SELF, Slice-A
    // author management on the Tiers tab), Some(hex) = another actor's profile by
    // tap-through (Slice-B subscriber-browse offers). The current page's
    // Tiers/offers refresh closure lives in a slot so the on-visible hook + each
    // rebuild drive the live page.
    // `profile-start-dm-button` (OTHER profile) seeds the Conversations
    // new-thread composer (in the profile view) and then switches the content
    // stack to the Conversations page. Stack-only nav — mirrors `open_profile`
    // below, which likewise switches the stack child without syncing the sidebar
    // selection (the sidebar_list isn't built yet at this point anyway).
    let on_open_conversations: Rc<dyn Fn()> = {
        let stack = stack.clone();
        Rc::new(move || stack.set_visible_child_name("conversations"))
    };
    let profile_refresh_slot: Rc<RefCell<Rc<dyn Fn()>>> = {
        let (profile_view, profile_handles) = views::profile::build_profile_view(
            fauna_client,
            None,
            Rc::clone(&on_open_conversations),
        );
        stack.add_named(&profile_view, Some("profile"));
        Rc::new(RefCell::new(profile_handles.refresh_tiers))
    };
    let open_profile: Rc<dyn Fn(Option<String>)> = {
        let stack = stack.clone();
        let client = Rc::clone(fauna_client);
        let slot = Rc::clone(&profile_refresh_slot);
        let on_open_conversations = Rc::clone(&on_open_conversations);
        Rc::new(move |target: Option<String>| {
            if let Some(existing) = stack.child_by_name("profile") {
                stack.remove(&existing);
            }
            let (view, handles) = views::profile::build_profile_view(
                &client,
                target,
                Rc::clone(&on_open_conversations),
            );
            stack.add_named(&view, Some("profile"));
            *slot.borrow_mut() = handles.refresh_tiers;
            stack.set_visible_child_name("profile");
        })
    };

    // Re-read the visible profile page when it becomes visible (mirrors the Peers
    // refresh above). The page is observer-free, so a pending subscribe request
    // that arrived while the author was elsewhere surfaces on return. "profile"
    // is not the stack's initial visible child, so this never fires during
    // construction — only on a real nav to the profile page.
    {
        let slot = Rc::clone(&profile_refresh_slot);
        stack.connect_visible_child_name_notify(move |s| {
            if s.visible_child_name().as_deref() == Some("profile") {
                let refresh = slot.borrow().clone();
                refresh();
            }
        });
    }

    // Tap-through: clicking a contact row opens that actor's profile (the
    // canonical list -> detail nav, profile.md § Relationship to Contacts). The
    // row's widget_name carries the peer actor_id (contacts/list.rs).
    {
        let open_profile = Rc::clone(&open_profile);
        contacts_list_box.connect_row_activated(move |_lb, row| {
            let actor_id = row.widget_name().to_string();
            if !actor_id.is_empty() {
                open_profile(Some(actor_id));
            }
        });
    }

    // Devices + Folders now live as Settings sub-pages (built by the settings
    // shell below, off one shared `DevicesMachine`); the former top-level "Peers"
    // stack child + its per-nav refresh are gone. The shell's pages self-refresh
    // the machine on `connect_map` (becoming visible); the initial on-auth load is
    // wired via `widgets.devices_machine` in the AuthSucceeded handler.

    let (events_view, events_handles) = views::events::build_events_view(Rc::clone(fauna_client));
    stack.add_named(&events_view, Some("events"));

    // Build (or rebuild, on re-auth) the process-wide feed manager over the
    // authed nest + the local actor's secret before the Feed view consumes it.
    crate::feed::host::init(fauna_client.nest_rpc().clone(), fauna_client.secret_bytes());
    let (feed_view, feed_handles) = views::feed::build_feed_view(fauna_client);
    stack.add_named(&feed_view, Some("feed"));

    // Build (or rebuild, on re-auth) the process-wide search manager before the
    // Search view consumes it — the same one-manager-per-login shape as the
    // feed manager above.
    crate::search::host::init(fauna_client.nest_rpc().clone());

    let bridges_snapshot: views::bridges::BridgesSnapshot = Default::default();
    // Share the very same snapshot with the settings subsystem: the Bluesky
    // page's Linked-account panel renders the "bluesky" provider's link
    // surface off it (ui/atproto.md § Layout & flow) rather than issuing a
    // second `fauna.bridges.list`.
    crate::settings::set_bridges_snapshot(&bridges_snapshot);
    let (bridges_view, bridges_handles) =
        views::bridges::build_bridges_view(fauna_client, Rc::clone(&bridges_snapshot));
    stack.add_named(&bridges_view, Some("bridges"));

    let (media_view, media_handles) = views::media::build_media_view(fauna_client);
    stack.add_named(&media_view, Some("media"));

    let (moderation_view, moderation_handles) = views::moderation::build_moderation_view();
    stack.add_named(&moderation_view, Some("moderation"));

    let (backups_view, backups_handles) = views::backups::build_backups_view(fauna_client);
    stack.add_named(&backups_view, Some("backups"));

    let (notifications_view, notifications_handles) =
        views::notifications::build_notifications_view();
    stack.add_named(&notifications_view, Some("notifications"));

    // The Settings shell is a sidebar-swap (settings.md § Navigation model, the
    // desktop shape mirroring admin): `settings_view` is the content (the
    // "settings" stack child — a sub-stack of all settings pages); the rail takes
    // over the split-view sidebar slot while settings shows (wired below via the
    // content-stack visible-child notify). The former standalone status view +
    // the `adw::PreferencesWindow` modal are both gone — status is the shell's
    // first sub-page. `settings_handles.status` carries the live `StatusHandles`
    // the message handlers update (identity / quota / node), threaded into
    // `widgets` exactly as the old standalone status view's were.
    let on_navigate_to_feed: Rc<dyn Fn()> = {
        let stack = stack.clone();
        Rc::new(move || stack.set_visible_child_name("feed"))
    };
    let (settings_view, settings_sidebar, settings_handles) =
        views::settings_shell::build_settings_shell(fauna_client, on_navigate_to_feed);
    stack.add_named(&settings_view, Some("settings"));
    let views::settings_shell::SettingsShellHandles {
        nav_back: settings_nav_back,
        sub_stack: settings_sub_stack,
        status: status_handles,
        devices_folders: devices_folders_handles,
        personalization: personalization_handles,
        store_resync: settings_store_resync,
    } = settings_handles;

    // The admin shell is a sidebar-swap: `admin_view` is the content (the
    // "admin" stack child); `admin_sidebar` is the vertical admin rail that
    // takes over the split-view sidebar slot while admin shows (wired below via
    // the content-stack visible-child notify). admin.md § Navigation model.
    let (admin_view, admin_sidebar, admin_handles) = views::admin::build_admin_view(fauna_client);
    stack.add_named(&admin_view, Some("admin"));
    let admin_sub_stack = admin_handles.sub_stack.clone();

    // The `family` page (family-safety.md § App surface) — an ordinary
    // top-level content-stack child (no sidebar swap), reached from the GATED
    // `family-tab` sidebar row and from the global `supervised-indicator`. The
    // e2e nav protocol passes an unknown canonical view name straight through to
    // the GTK stack child name (`test_agent::canonical_to_gtk`), so the child
    // literally named "family" needs no mapping entry.
    let family_view = views::family::build_family_view(fauna_client);
    stack.add_named(&family_view, Some("family"));

    // The cross-page navigation `search-result-item` activation dispatches
    // into, one per `SearchNav` destination (`ui/search.md` § Where logic
    // lives → Result navigation (deep link)) — see `views::search::SearchNavHandles`.
    let search_nav = views::search::SearchNavHandles {
        stack: stack.clone(),
        open_post_detail: feed_handles.open_post_detail.clone(),
        switch_to_addressbook_segment: Rc::clone(&switch_to_addressbook_segment),
        media_machine: std::sync::Arc::clone(&media_handles.media_machine),
    };
    let (search_view, search_handles) = views::search::build_search_view(fauna_client, search_nav);
    // Search is accessible via Ctrl+F or can be added to sidebar in future.
    // For now, add as a named stack page.
    stack.add_named(&search_view, Some("search"));

    // Start on the last-used section, but never launch *into* a shell you "step
    // into" (Settings / Admin) — see `window_state::initial_sidebar_item`.
    let initial_view = crate::window_state::initial_sidebar_item(&saved_state.sidebar_item);
    stack.set_visible_child_name(&initial_view);

    // Refresh data when navigating to data-driven views. This ensures the
    // list is up-to-date even when changes happened outside the app (e.g.
    // a group created via the API while the app was on another page).
    {
        let client_for_nav = Rc::clone(fauna_client);
        // A fresh arrival on the Backups page must not still be reporting the
        // LAST restore (see the "backups" arm below). tui's plain page refresh
        // already lands `restore-progress` back on its idle prompt because the
        // state is a field its load rewrites; linux's is a label that keeps
        // whatever was last written to it, so the reset has to be explicit.
        let restore_progress_for_nav = backups_handles.restore.progress.clone();
        let restore_warning_for_nav = backups_handles.restore.warning.clone();
        stack.connect_visible_child_name_notify(move |s| {
            if let Some(name) = s.visible_child_name() {
                match name.as_str() {
                    // The Feed page self-refreshes via its `FeedManager` observer
                    // loop + the split's `connect_map` (re-list feeds + reload on
                    // becoming visible) — no nav-handler fetch needed.
                    "feed" => {}
                    // Refresh the moderation queue (a flag may have landed since
                    // login / last visit — the actions are nest-authoritative).
                    "moderation" => client_for_nav.fetch_moderation_actions(),
                    // Roster, knocks and the review badge — the same door the
                    // test agent's navigate takes (`main.rs`).
                    "contacts" => client_for_nav.refresh_contacts_page(),
                    // Re-list calendars (which cascades into per-calendar event
                    // fetches) so calendars/events created *outside* the app —
                    // e.g. via the macOS Calendar app talking CalDAV to the MDA
                    // — appear on return to the Events page, not only after a
                    // re-login. The test-agent nav path (`main.rs`) already does
                    // this; the real UI handler had silently omitted it, so e2e
                    // green hid the gap.
                    "events" => client_for_nav.fetch_calendars(),
                    "backups" => {
                        // The snapshot half re-reads itself: the page's
                        // `connect_map` refreshes `BackupsMachine`, which
                        // reloads the folder selector and the selected set's
                        // snapshots. What still needs firing here are the
                        // restore surfaces: history (+ per-row divergence,
                        // fired from the RestoreHistoryLoaded arm) and the
                        // local-snapshot picker.
                        client_for_nav.fetch_restore_history();
                        client_for_nav.fetch_message_kind_snapshots();
                        // Arriving on the page is not the end of a restore: a
                        // stale "Done — restart the bridge." would tell an owner
                        // who has just walked in that something finished while
                        // they were away.
                        restore_progress_for_nav
                            .set_text(crate::i18n::strings::backups::RESTORE_PROGRESS_IDLE);
                        restore_warning_for_nav.set_visible(false);
                    }
                    // The test-agent nav path (`main.rs`) already refreshes
                    // the admin shell on nav; the real sidebar-click path had
                    // silently omitted it, same class of gap as "events"
                    // above — see `FaunaClient::refresh_admin_shell`'s doc
                    // comment for the full fetch list and rationale.
                    "admin" => client_for_nav.refresh_admin_shell(),
                    _ => {}
                }
            }
        });
    }

    // Poll the encrypted CalDAV store while the Events page is the visible child,
    // so a calendar/event created by an *external* CalDAV client (macOS Calendar
    // → mail-bridge MDA → `bridge_caldav_*`) appears *while the user sits on the
    // page*, not only on navigate-away-and-back (the visible-child-notify above)
    // or re-login — the manual-test gap, 2026-06-13. This is the poll half of the
    // goal-doc mechanism (events.md § Where logic lives); it reuses the existing
    // full `fetch_calendars()` re-list, which preserves per-calendar visibility
    // and the selected date/view/open-detail across a refresh
    // (`calendar_sidebar::update_calendar_sidebar` only adds *new* calendars and
    // never clears the selection). The gate on the visible child keeps the poll
    // idle on every other page. The delta-optimised `sync_calendar_since` form +
    // a calendar-change push are the follow-on (events.md § Implementation status
    // today).
    {
        let client_for_poll = Rc::clone(fauna_client);
        let stack_for_poll = stack.clone();
        glib::timeout_add_local(
            std::time::Duration::from_secs(CALENDAR_POLL_INTERVAL_SECS),
            move || {
                if stack_for_poll
                    .visible_child_name()
                    .is_some_and(|n| n.as_str() == "events")
                {
                    client_for_poll.fetch_calendars();
                }
                glib::ControlFlow::Continue
            },
        );
    }

    // -----------------------------------------------------------------------
    // Sidebar
    // -----------------------------------------------------------------------
    let stack_for_sidebar = stack.clone();
    let open_profile_for_sidebar = Rc::clone(&open_profile);
    let sidebar_list = build_sidebar(move |item| {
        // The profile row always opens the viewer's OWN profile — rebuild as SELF
        // so a prior tap-through to another actor's profile is reset.
        if item.stack_name() == "profile" {
            open_profile_for_sidebar(None);
        } else {
            stack_for_sidebar.set_visible_child_name(item.stack_name());
        }
    });

    // Sync sidebar selection to the restored stack page (`initial_view` already
    // mapped shells → the primary view, so this never selects a shell row).
    {
        let initial_index = SidebarItem::ALL
            .iter()
            .position(|i| i.stack_name() == initial_view.as_str())
            .unwrap_or(0);
        if let Some(row) = sidebar_list.row_at_index(initial_index as i32) {
            sidebar_list.select_row(Some(&row));
        }
    }

    // `admin-nav-back` (admin.md § Navigation model): the rail-top button leaves
    // the admin shell back to the non-admin app. Admin is a top-level nav peer
    // (reached via the `admin_pages` nav entry, not nested under Settings), so it
    // lands on the primary view (Conversations) — mirroring `settings-nav-back`
    // below, the parallel "leave shell" affordance. (Was: landed on the Settings
    // shell; corrected so exiting admin returns to the main view, not Settings.)
    {
        let stack_for_back = stack.clone();
        let sidebar_for_back = sidebar_list.clone();
        let conv_index = SidebarItem::ALL
            .iter()
            .position(|i| i.stack_name() == "conversations")
            .unwrap_or(0);
        admin_handles.nav_back.connect_clicked(move |_| {
            stack_for_back.set_visible_child_name("conversations");
            if let Some(row) = sidebar_for_back.row_at_index(conv_index as i32) {
                sidebar_for_back.select_row(Some(&row));
            }
        });
    }

    // `settings-nav-back` (settings.md § Navigation model): the rail-top button
    // leaves the Settings shell back to the non-settings app. Lands on the
    // primary view (Conversations) and syncs the sidebar selection.
    {
        let stack_for_back = stack.clone();
        let sidebar_for_back = sidebar_list.clone();
        let conv_index = SidebarItem::ALL
            .iter()
            .position(|i| i.stack_name() == "conversations")
            .unwrap_or(0);
        settings_nav_back.connect_clicked(move |_| {
            stack_for_back.set_visible_child_name("conversations");
            if let Some(row) = sidebar_for_back.row_at_index(conv_index as i32) {
                sidebar_for_back.select_row(Some(&row));
            }
        });
    }

    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .width_request(200)
        .vexpand(true)
        .child(&sidebar_list)
        .build();

    // Global connection-status indicator pinned to the top of the sidebar —
    // always-visible live state of the nest WS-RPC connection (driven by the
    // `ConnectionState` watch via the WsEvent pump). Starts Disconnected; the
    // first state event updates it.
    let connection_status_icon = gtk::Image::from_icon_name("network-offline-symbolic");
    let connection_status_label = gtk::Label::new(Some(common::DISCONNECTED));
    connection_status_label.set_xalign(0.0);
    crate::testid::set_test_id(&connection_status_label, ids::CONNECTION_STATUS);
    let connection_status_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    connection_status_box.set_margin_top(8);
    connection_status_box.set_margin_bottom(8);
    connection_status_box.set_margin_start(12);
    connection_status_box.set_margin_end(12);
    connection_status_box.append(&connection_status_icon);
    connection_status_box.append(&connection_status_label);

    // Global sync-agent-status indicator, just below connection-status —
    // health of the LOCAL fauna-sync-agent process (sync-agent.md § Local
    // agent health). Starts "Not running"; the first status poll updates it
    // (`sync_agent.rs::start_status_poll`). A dim version/uptime subtitle line
    // sits underneath, populated on the same poll.
    let sync_agent_status_icon = gtk::Image::from_icon_name("emblem-synchronizing-symbolic");
    let sync_agent_status_label =
        gtk::Label::new(Some(crate::i18n::strings::status::sync_agent::NOT_RUNNING));
    sync_agent_status_label.set_xalign(0.0);
    crate::testid::set_test_id(&sync_agent_status_label, ids::SYNC_AGENT_STATUS);
    let sync_agent_status_primary_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    sync_agent_status_primary_box.append(&sync_agent_status_icon);
    sync_agent_status_primary_box.append(&sync_agent_status_label);

    let sync_agent_status_version_label = gtk::Label::new(None);
    sync_agent_status_version_label.set_xalign(0.0);
    sync_agent_status_version_label.add_css_class("dim-label");
    sync_agent_status_version_label.add_css_class("caption");
    crate::testid::set_test_id(
        &sync_agent_status_version_label,
        ids::SYNC_AGENT_STATUS_VERSION,
    );
    let sync_agent_status_uptime_label = gtk::Label::new(None);
    sync_agent_status_uptime_label.set_xalign(0.0);
    sync_agent_status_uptime_label.add_css_class("dim-label");
    sync_agent_status_uptime_label.add_css_class("caption");
    crate::testid::set_test_id(
        &sync_agent_status_uptime_label,
        ids::SYNC_AGENT_STATUS_UPTIME,
    );
    let sync_agent_status_detail_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    sync_agent_status_detail_box.append(&sync_agent_status_version_label);
    sync_agent_status_detail_box.append(&sync_agent_status_uptime_label);

    let sync_agent_status_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sync_agent_status_box.set_margin_bottom(8);
    sync_agent_status_box.set_margin_start(12);
    sync_agent_status_box.set_margin_end(12);
    sync_agent_status_box.append(&sync_agent_status_primary_box);
    sync_agent_status_box.append(&sync_agent_status_detail_box);

    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_box.append(&connection_status_box);
    sidebar_box.append(&sync_agent_status_box);
    sidebar_box.append(&sidebar_scroll);

    // Sidebar-swap container: the split-view's sidebar slot holds a Stack with
    // the normal app nav ("main") and the vertical admin rail ("admin"). The
    // admin shell is a sidebar-swap — entering admin replaces the main nav with
    // the admin rail in place (no horizontal sub-tabs, no second rail), which is
    // what removes the admin page's horizontal width pressure (admin.md
    // § Navigation model). The swap is driven below off the content stack's
    // visible-child so it fires for the sidebar Admin row, the test agent's
    // name-based nav, and Ctrl+<n> alike — always keeping `admin-nav-back` shown.
    // Both the admin shell and the settings shell are sidebar-swaps, so the slot
    // holds three rails: "main" (normal nav), "settings", "admin". Entering
    // either shell replaces the main nav with that shell's rail in place.
    let sidebar_stack = gtk::Stack::new();
    sidebar_stack.add_named(&sidebar_box, Some("main"));
    sidebar_stack.add_named(&settings_sidebar, Some("settings"));
    sidebar_stack.add_named(&admin_sidebar, Some("admin"));
    sidebar_stack.set_visible_child_name(match initial_view.as_str() {
        "admin" => "admin",
        "settings" => "settings",
        _ => "main",
    });

    // -----------------------------------------------------------------------
    // OverlaySplitView — sidebar on left, content on right
    // -----------------------------------------------------------------------
    let split_view = adw::OverlaySplitView::new();
    split_view.set_sidebar(Some(&sidebar_stack));
    // The every-page critical-alerts banner sits ABOVE the page stack, so it
    // is visible whatever page shows (critical-alerts.md; ui.yaml `global:`).
    let content_with_alerts = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_with_alerts.append(&crate::critical_alerts::build_banner());
    // The ward's `screen-time-lock` covers the page stack — deliberately NOT
    // the header bar, so the `supervised-indicator` button stays clickable and
    // the Family page stays reachable read-only while locked (family-safety.md
    // § Screen time). It hides itself on the family page; see `screen_lock`.
    let screen_lock_overlay = crate::screen_lock::build_lock_overlay();
    let stack_overlay = gtk::Overlay::new();
    stack_overlay.set_child(Some(&stack));
    stack_overlay.add_overlay(&screen_lock_overlay);
    stack_overlay.set_vexpand(true);
    content_with_alerts.append(&stack_overlay);
    stack.set_vexpand(true);
    split_view.set_content(Some(&content_with_alerts));
    split_view.set_collapsed(false);

    // Swap the sidebar rail whenever the content stack enters/leaves a shell
    // ("admin" → admin rail, "settings" → settings rail, otherwise the main nav).
    {
        let sidebar_stack = sidebar_stack.clone();
        let lock_for_nav = screen_lock_overlay.clone();
        let settings_sub_stack = settings_sub_stack.clone();
        let admin_sub_stack = admin_sub_stack.clone();
        stack.connect_visible_child_name_notify(move |s| {
            let rail = match s.visible_child_name().as_deref() {
                Some("admin") => "admin",
                Some("settings") => "settings",
                _ => "main",
            };
            sidebar_stack.set_visible_child_name(rail);
            // Entering a shell lands on its canonical entry — Settings → Status,
            // Admin → Dashboard (`docs/goal/ui/README.md` § Navigation model,
            // ratified 2026-08-13): a shell's sub-page is shell state, not session
            // state, so it never survives leaving the shell.
            //
            // This notify IS the nav edge the rule asks for — it fires only when the
            // content stack's visible child actually CHANGES — which gets the rule's
            // two qualifications for free: switching sub-pages within a visit drives
            // the shell's own sub-stack and never reaches here, and the two-element
            // deep link (`{"view":"settings","id":"<page>"}`) seats its sub-page in
            // `main.rs`'s nav patch *after* `set_visible_child_name` fires this, so
            // the deep link still wins.
            //
            // It also puts the reset on the path a HUMAN takes. `main.rs`'s nav patch
            // already resolved a bare `{"view":"admin"}` to the Dashboard, matching
            // macOS + Windows — but only for the test agent, so the sidebar-clicking
            // user kept landing on the stale sub-page and no e2e test could see it
            // (the agent's own nav had already reset what it was about to assert).
            // Sweep 4 in `walk.rs` drives the real sidebar for exactly that reason.
            match rail {
                "settings" => settings_sub_stack
                    .set_visible_child_name(views::settings_shell::CANONICAL_ENTRY),
                "admin" => admin_sub_stack.set_visible_child_name(views::admin::CANONICAL_ENTRY),
                _ => {}
            }
            // The lock is page-dependent (the family page is exempt), so it
            // re-evaluates on every navigation, not just on a status read.
            crate::screen_lock::refresh(&lock_for_nav, s.visible_child_name().as_deref());
        });
    }

    // Re-evaluate the lock once a minute so a ward already in the app crosses
    // into (or out of) their usage window without needing to navigate.
    crate::screen_lock::start_lock_tick(screen_lock_overlay.clone(), stack.clone());
    // Register the mounted surface so the heartbeat (and anything else that
    // moves the verdict) can repaint without threading both widgets through.
    crate::screen_lock::register_surface(&screen_lock_overlay, &stack);

    // -----------------------------------------------------------------------
    // Header bar with title and sidebar toggle
    // -----------------------------------------------------------------------
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some("Fauna"))));

    let toggle_btn = gtk::ToggleButton::new();
    toggle_btn.set_icon_name("sidebar-show-symbolic");
    toggle_btn.set_active(true);
    toggle_btn.set_tooltip_text(Some(common::TOGGLE_SIDEBAR));

    // Bind toggle button to split view.
    let sv = split_view.clone();
    toggle_btn.connect_toggled(move |btn| {
        sv.set_show_sidebar(btn.is_active());
    });

    header.pack_start(&toggle_btn);

    // `supervised-indicator` (ui.yaml `global.elements`) — the permanent,
    // non-dismissable "This account is supervised by X" chrome, present on EVERY
    // authenticated page and navigating to the `family` page
    // (family-safety.md § App surface; § The trust shape invariant 4 —
    // supervision is never silent). Hidden until `fauna.family.status` reports a
    // guardian (`DataMessage::FamilyStatusLoaded`).
    //
    // Placement is the **header bar**, deliberately not the sidebar: the sidebar
    // slot is a `sidebar_stack` that the Settings and Admin shells swap out
    // (below), so a sidebar-only indicator would vanish on exactly the pages a
    // supervised user is most likely to go looking at. The header is outside the
    // split view and shows on every stack child, shells included — the linux peer
    // of the page-level overlay windows used for the same reason.
    let supervised_indicator = gtk::Button::new();
    supervised_indicator.add_css_class("suggested-action");
    supervised_indicator.set_visible(false);
    crate::testid::set_test_id(&supervised_indicator, ids::SUPERVISED_INDICATOR);
    {
        let stack_for_indicator = stack.clone();
        let sidebar_for_indicator = sidebar_list.clone();
        supervised_indicator.connect_clicked(move |_| {
            stack_for_indicator.set_visible_child_name("family");
            // Keep the sidebar selection in step with the page (the gated Family
            // row is the last one — `SidebarItem::from_index`).
            let family_index = SidebarItem::ALL.len() as i32 + 1;
            if let Some(row) = sidebar_for_indicator.row_at_index(family_index) {
                sidebar_for_indicator.select_row(Some(&row));
            }
        });
    }
    header.pack_end(&supervised_indicator);

    // The header cogwheel ("Preferences" → `adw::PreferencesWindow` modal) has
    // been REMOVED. Settings is reached from the left-menu "Settings" item, which
    // swaps in the inline Settings sidebar-swap shell (settings.md § Navigation
    // model); `Ctrl+,` is kept as a shortcut to it (wired below). No modal.

    // -----------------------------------------------------------------------
    // Message banner labels — hidden by default; shown via set_error_message()
    // or the test agent's messages patch.
    // -----------------------------------------------------------------------
    let error_label = gtk::Label::new(None);
    error_label.set_halign(gtk::Align::Fill);
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    error_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    error_label.set_selectable(true);
    error_label.add_css_class("error-banner");
    error_label.set_visible(false);
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);

    let warning_label = gtk::Label::new(None);
    warning_label.set_halign(gtk::Align::Fill);
    warning_label.set_xalign(0.0);
    warning_label.set_wrap(true);
    warning_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    warning_label.set_selectable(true);
    warning_label.add_css_class("warning-banner");
    warning_label.set_visible(false);
    crate::testid::set_test_id(&warning_label, ids::WARNING_MESSAGE);

    let info_label = gtk::Label::new(None);
    info_label.set_halign(gtk::Align::Fill);
    info_label.set_xalign(0.0);
    info_label.set_wrap(true);
    info_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    info_label.set_selectable(true);
    info_label.add_css_class("info-banner");
    info_label.set_visible(false);
    crate::testid::set_test_id(&info_label, ids::INFO_MESSAGE);

    // Wrap split_view and banner labels in a vertical box so banners
    // appear at the top of the content area, below the header bar.
    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.append(&error_label);
    content_box.append(&warning_label);
    content_box.append(&info_label);
    content_box.append(&split_view);

    // -----------------------------------------------------------------------
    // Toast overlay — wraps the main content so toasts appear
    // -----------------------------------------------------------------------
    let toast_overlay = adw::ToastOverlay::new();

    // -----------------------------------------------------------------------
    // Toolbar view wrapping header + content box
    // -----------------------------------------------------------------------
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content_box));

    toast_overlay.set_child(Some(&toolbar));

    // -----------------------------------------------------------------------
    // Window
    // -----------------------------------------------------------------------
    // Restore the saved size, but never larger than the current screen — a
    // stale geometry from a bigger display would otherwise open off-screen-wide.
    let (win_width, win_height) = clamp_to_monitor(saved_state.width, saved_state.height);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Fauna")
        .default_width(win_width)
        .default_height(win_height)
        .width_request(MIN_WINDOW_WIDTH)
        .height_request(MIN_WINDOW_HEIGHT)
        .content(&toast_overlay)
        .build();
    // Sign-out targets this widget_name when it iterates `app.windows()` to
    // tear the authenticated UI down.
    window.set_widget_name("fauna-main-window");

    // -----------------------------------------------------------------------
    // Keyboard shortcuts: Ctrl+1 through Ctrl+8 switch sidebar sections
    // -----------------------------------------------------------------------
    let shortcut_controller = gtk::ShortcutController::new();
    shortcut_controller.set_scope(gtk::ShortcutScope::Managed);

    for (i, item) in SidebarItem::ALL.iter().enumerate() {
        if i >= 9 {
            break; // GTK only supports single-digit shortcuts (<Control>1 through <Control>9)
        }
        let trigger_str = format!("<Control>{}", i + 1);
        let trigger =
            gtk::ShortcutTrigger::parse_string(&trigger_str).expect("valid shortcut trigger");

        let stack_ref = stack.clone();
        let sidebar_ref = sidebar_list.clone();
        let stack_name = item.stack_name().to_string();

        let action = gtk::CallbackAction::new(move |_widget, _args| {
            stack_ref.set_visible_child_name(&stack_name);
            // Also update sidebar selection to match.
            if let Some(row) = sidebar_ref.row_at_index(i as i32) {
                sidebar_ref.select_row(Some(&row));
            }
            glib::Propagation::Stop
        });

        let shortcut = gtk::Shortcut::new(Some(trigger), Some(action));
        shortcut_controller.add_shortcut(shortcut);
    }

    // Ctrl+N — start new conversation in the in-pane recipient picker.
    {
        let trigger =
            gtk::ShortcutTrigger::parse_string("<Control>n").expect("valid shortcut trigger");
        let action = gtk::CallbackAction::new(move |_widget, _args| {
            crate::conversations::manager().start_new_conversation();
            glib::Propagation::Stop
        });
        let shortcut = gtk::Shortcut::new(Some(trigger), Some(action));
        shortcut_controller.add_shortcut(shortcut);
    }

    // Ctrl+, — open Settings (navigate the content stack to the inline Settings
    // shell; the sidebar rail swaps via the content-stack notify). Replaces the
    // removed `adw::PreferencesWindow` modal.
    {
        let trigger =
            gtk::ShortcutTrigger::parse_string("<Control>comma").expect("valid shortcut trigger");
        let stack_ref = stack.clone();
        let sidebar_ref = sidebar_list.clone();
        let settings_index = SidebarItem::ALL
            .iter()
            .position(|i| i.stack_name() == "settings")
            .unwrap_or(0);
        let action = gtk::CallbackAction::new(move |_widget, _args| {
            stack_ref.set_visible_child_name("settings");
            if let Some(row) = sidebar_ref.row_at_index(settings_index as i32) {
                sidebar_ref.select_row(Some(&row));
            }
            glib::Propagation::Stop
        });
        let shortcut = gtk::Shortcut::new(Some(trigger), Some(action));
        shortcut_controller.add_shortcut(shortcut);
    }

    // Ctrl+W — close window (hide-to-tray when enabled, otherwise quit).
    {
        let trigger =
            gtk::ShortcutTrigger::parse_string("<Control>w").expect("valid shortcut trigger");
        let win_ref = window.clone();
        let action = gtk::CallbackAction::new(move |_widget, _args| {
            if tray::should_hide_to_tray() {
                win_ref.set_visible(false);
            } else {
                win_ref.close();
            }
            glib::Propagation::Stop
        });
        let shortcut = gtk::Shortcut::new(Some(trigger), Some(action));
        shortcut_controller.add_shortcut(shortcut);
    }

    // Ctrl+K — open Quick Switcher dialog.
    {
        let trigger =
            gtk::ShortcutTrigger::parse_string("<Control>k").expect("valid shortcut trigger");
        let win_ref = window.clone();
        let action = gtk::CallbackAction::new(move |_widget, _args| {
            let qs = build_quick_switcher(&win_ref);
            qs.present();
            glib::Propagation::Stop
        });
        let shortcut = gtk::Shortcut::new(Some(trigger), Some(action));
        shortcut_controller.add_shortcut(shortcut);
    }

    // Ctrl+F — switch to Search view and focus the search entry.
    // We can't capture search_entry here (built after shortcuts), so we
    // store a clone in a Cell that is filled in after build.
    let search_entry_cell: Rc<RefCell<Option<gtk::SearchEntry>>> = Rc::new(RefCell::new(None));
    {
        let trigger =
            gtk::ShortcutTrigger::parse_string("<Control>f").expect("valid shortcut trigger");
        let stack_ref = stack.clone();
        let sidebar_ref = sidebar_list.clone();
        let entry_cell = Rc::clone(&search_entry_cell);
        let action = gtk::CallbackAction::new(move |_widget, _args| {
            stack_ref.set_visible_child_name("search");
            // Deselect all sidebar rows since search isn't a sidebar item.
            sidebar_ref.unselect_all();
            // Grab focus on the search entry if available.
            if let Some(entry) = entry_cell.borrow().as_ref() {
                entry.grab_focus();
            }
            glib::Propagation::Stop
        });
        let shortcut = gtk::Shortcut::new(Some(trigger), Some(action));
        shortcut_controller.add_shortcut(shortcut);
    }

    // Ctrl+Shift+H — hide window to system tray. Only honoured when a tray host
    // is present to restore from; otherwise hiding would strand the window.
    {
        let trigger = gtk::ShortcutTrigger::parse_string("<Control><Shift>h")
            .expect("valid shortcut trigger");
        let win_ref = window.clone();
        let action = gtk::CallbackAction::new(move |_widget, _args| {
            if tray::tray_host_available() {
                win_ref.set_visible(false);
            }
            glib::Propagation::Stop
        });
        let shortcut = gtk::Shortcut::new(Some(trigger), Some(action));
        shortcut_controller.add_shortcut(shortcut);
    }

    // Escape — minimize to tray when close-to-tray is enabled.
    {
        let trigger = gtk::ShortcutTrigger::parse_string("Escape").expect("valid shortcut trigger");
        let win_ref = window.clone();
        let action = gtk::CallbackAction::new(move |_widget, _args| {
            if tray::should_hide_to_tray() {
                win_ref.set_visible(false);
                glib::Propagation::Stop
            } else {
                // Close-to-tray off, or no tray host to restore from — Escape
                // does nothing (never hide with no way back).
                glib::Propagation::Proceed
            }
        });
        let shortcut = gtk::Shortcut::new(Some(trigger), Some(action));
        shortcut_controller.add_shortcut(shortcut);
    }

    window.add_controller(shortcut_controller);

    // -----------------------------------------------------------------------
    // Assemble widget handles
    // -----------------------------------------------------------------------
    // Fill in the search entry cell now that we have the handle.
    *search_entry_cell.borrow_mut() = Some(search_handles.search_entry.clone());

    let widgets = Rc::new(WidgetHandles {
        connection_status_icon,
        connection_status_label,
        sync_agent_status_icon,
        sync_agent_status_label,
        sync_agent_status_version_label,
        sync_agent_status_uptime_label,
        status_actor_id_row: status_handles.actor_id_row,
        status_handle_row: status_handles.handle_row,
        status_node_url_row: status_handles.node_url_row,
        status_connection_row: status_handles.connection_row,
        status_quota_tier_row: status_handles.quota_tier_row,
        status_quota_inbox_row: status_handles.quota_inbox_row,
        status_quota_storage_row: status_handles.quota_storage_row,
        status_quota_devices_row: status_handles.quota_devices_row,
        status_quota_section_marker: status_handles.quota_section_marker,
        status_quota_inbox_marker: status_handles.quota_inbox_marker,
        status_quota_storage_marker: status_handles.quota_storage_marker,
        status_quota_devices_marker: status_handles.quota_devices_marker,
        status_feature_limits_group: status_handles.feature_limits_group,
        status_feature_limits_rows_box: status_handles.feature_limits_rows_box,
        status_feature_limits_section_marker: status_handles.feature_limits_section_marker,
        status_region_rows_box: status_handles.region_rows_box,
        status_node_domain_row: status_handles.node_domain_row,
        status_node_version_row: status_handles.node_version_row,
        status_files_row: status_handles.files_row,
        status_last_sync_row: status_handles.last_sync_row,
        notifications_list_box: notifications_handles.list_box,
        notifications_count_badge: notifications_handles.count_badge,
        knocks_list_box,
        contacts_list_box,
        find_results_list_box,
        contacts_filter_index,
        address_book,
        events_handles,
        feed_content_stack: feed_handles.content_stack,
        bridges_list_box: bridges_handles.list_handles.list_box,
        bridges_split: bridges_handles.split,
        bridges_snapshot,
        bridges_open_detail: bridges_handles.open_detail,
        moderation_queue_list_box: moderation_handles.queue_list_box,
        folder_list_box: devices_folders_handles.folder_list_box,
        folder_pending_shares_group: devices_folders_handles.pending_shares_group,
        folder_pending_shares_box: devices_folders_handles.pending_shares_box,
        #[cfg(feature = "p2p-share")]
        offline_share: devices_folders_handles.offline_share,
        #[cfg(feature = "p2p-share")]
        offline_share_state: devices_folders_handles.offline_share_state,
        #[cfg(feature = "p2p-share")]
        repaint_group_scopes: devices_folders_handles.repaint_group_scopes,
        #[cfg(feature = "p2p-share")]
        share_transfer: devices_folders_handles.share_transfer,
        devices_machine: devices_folders_handles.devices_machine,
        labeler_catalog_machine: personalization_handles.labeler_catalog_machine,
        media_machine: media_handles.media_machine,
        media_page: media_handles.media_page,
        backups_machine: backups_handles.machine,
        snapshot_file_list_box: backups_handles.snapshot_file_list_box,
        backup_detail_header: backups_handles.detail_header,
        backups_restore: backups_handles.restore,
        search_entry: search_handles.search_entry,
        toast_overlay,
        error_label,
        warning_label,
        info_label,
        window: window.clone(),
        admin_handles,
        sidebar_list_box: sidebar_list.clone(),
        supervised_indicator,
        screen_lock_overlay,
        content_stack: stack.clone(),
        settings_sub_stack: settings_sub_stack.clone(),
        settings_store_resync,
    });

    // `snapshot-check-button` is wired inside the Backups page itself now: it
    // dispatches `BackupsMachine::check`, which knows the selected set and
    // whose reply drives both the verdict surface and each row's derived
    // integrity. Reaching in from here to read a dropdown was the shape that
    // made the check a page-external gesture with its own busy story.

    MainWindowResult {
        window,
        state,
        widgets,
        stack,
        open_profile,
    }
}

// ---------------------------------------------------------------------------
// Handle incoming UiMessages — called from the polling loop in main.rs
// ---------------------------------------------------------------------------

/// Process a single `UiMessage`, updating `AppState` and the live widgets.
pub fn handle_ui_message(
    msg: &UiMessage,
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<WidgetHandles>,
    fauna_client: &Rc<FaunaClient>,
) {
    match msg {
        UiMessage::Data(data_msg) => match data_msg {
            DataMessage::AuthSuccess { actor_id, handle } => {
                // Breadcrumbs: this arm is the longest stretch of GTK-main-thread
                // work in the app, and until 2026-08-17 only its MLS and P2P steps
                // said anything, leaving a large silent gap (the account-cache read
                // and the sync-agent install). See the note in
                // `views::onboarding::launch_main_app_after_signin`.
                tracing::info!("[launch] AuthSuccess: acquiring the session instance");
                // The (OS login, account) single-instance guard — become this
                // account's one instance BEFORE opening any of its scoped
                // state below (MLS db, P2P stores, sync agent), reusing the
                // held lock on a same-account rebuild and swapping on an
                // in-process switch (account-scoping.md § Concurrent
                // instances). Refusal is terminal for primary and bound alike:
                // GApplication's D-Bus uniqueness only guards PLAIN launches
                // (and not at all under e2e's NON_UNIQUE), so this is the
                // guard that holds for bound launches, bare-binary races, and
                // bus-less sessions — apple's `refuseLaunch` contract. stderr
                // as well as tracing: `exit` skips the log appender's flush.
                if let Err(refusal) = crate::account_scope::become_session_instance(actor_id) {
                    refusal.exit(actor_id);
                }

                // Update state.
                {
                    let mut s = state.borrow_mut();
                    s.actor_id = actor_id.clone();
                    s.handle = handle.clone();
                    s.connected = true;
                }

                // Update tray connection state.
                *crate::tray::tray_state().connected.lock().unwrap() = true;

                // Update status view.
                widgets.status_actor_id_row.set_subtitle(actor_id);
                widgets.status_handle_row.set_subtitle(handle);
                widgets
                    .status_connection_row
                    .set_subtitle(common::CONNECTED);

                // Start WS-RPC: connects the shared NestClient and spawns
                // the connection-state + push pumps. Replaces the legacy
                // raw `?token=` push socket (WS-RPC adoption, tracked internally).
                tracing::info!("[launch] AuthSuccess: starting WS-RPC");
                fauna_client.start_ws_rpc();

                // The post-succession aftermath — FIRST among the post-auth hooks,
                // and the ordering is deliberate: the pass is spawned ahead of the
                // account-state readers below (the muted-keyword preload) — tui's
                // `establish` order. A head start, not a barrier: each of those
                // readers self-heals on its next read. An ordinary identity pays
                // one in-memory registry walk and starts nothing.
                fauna_client.run_succession_aftermath();

                // The deployment-seed custody leg's post-auth edge
                // (box-recovery.md § The plane-era recovery floor → (c) The
                // writes): runs now only if the account store is already up;
                // otherwise `account_runtime::install`'s store-ready edge below,
                // landing second, runs it. A run ending with custody unconfirmed
                // for an admin surfaces as the custody-warning toast.
                fauna_client.run_deployment_seed_custody_leg();

                // Opportunistically refresh the published mail content-sealing
                // epoch schedule at the same universal post-auth hook, so it
                // keeps sliding forward on every returning-user relaunch, not
                // just at enable-mail/rotate time (encryption-at-rest.md
                // § Capability tiering → Content-sealing epochs). Best-effort
                // / log-only; a no-op when mail is disabled.
                fauna_client.refresh_mail_epoch_schedule();

                // Run the feeders that have no page of their own, at the same
                // universal post-auth hook (`critical-alerts.md` § Goal — a
                // set-and-forget deployment must still raise its banner).
                // tui's `session::establish` call is the reference; this is
                // linux's leg.
                fauna_client.run_critical_alert_sweep();

                // Preload the sealed muted-keyword list into the conversation
                // collapse cache (moderation.md § Muted keywords) at the same
                // post-auth hook, so a matching DM collapses on a fresh launch
                // without first visiting the "Muted words" Settings page.
                fauna_client.load_muted_keywords();

                // The in-process file-sync driver starts further down, *after*
                // the MLS engine is initialized — it shares the one
                // `Arc<MlsEngine>` (device-sync channel + key-package state).

                // Initialize MLS engine. Per-account state dir
                // (account-scoping.md § The scoping taxonomy), resolved once
                // here so the P2P init below reuses the SAME directory.
                let account_dir = crate::account_scope::account_state_dir(&state.borrow().actor_id);
                let mls_db_path = account_dir.join("mls_state.db");

                let secret_hex = fauna_client.secret_hex().to_string();
                if let Ok(identity) =
                    fauna_core::identity::ActorKeypair::from_secret_hex(&secret_hex)
                {
                    // Release-before-build: the hand-over `account-data-plane.md`
                    // § Multi-instance concurrency ruled in-process, which linux
                    // reaches through its own factory rather than the shared FFI
                    // one. Without it, a re-login for THIS account is refused by
                    // its own predecessor's still-registered rail and the arm
                    // below arms the standing refusal over a live engine.
                    match crate::conversations::conv_backend::build_session_engine(
                        &crate::conversations::host::manager(),
                        identity,
                        &mls_db_path,
                    ) {
                        Ok(mgr) => {
                            tracing::info!("MLS engine initialized at {}", mls_db_path.display());
                            state.borrow_mut().mls = Some(mgr);
                            crate::conversations::host::manager()
                                .set_engine_served_elsewhere(false);
                        }
                        // The conversations-engine role is held by another instance —
                        // the honest standing refusal, not a failure: the state is
                        // intact and the holder is serving it (`account-data-plane.md`
                        // § Multi-instance concurrency). Surfaced on `error-message` via
                        // the manager's top-precedence flag; everything non-conversations
                        // proceeds normally with `state.mls` staying `None`.
                        Err(fauna_mls::error::MlsError::ServedElsewhere) => {
                            tracing::info!(
                                "MLS engine: this account's conversations are served in \
                                 another instance — refusing the engine role honestly"
                            );
                            crate::conversations::host::manager().set_engine_served_elsewhere(true);
                        }
                        Err(e) => {
                            tracing::error!("MLS engine init failed: {e}");
                            crate::conversations::host::manager()
                                .set_engine_served_elsewhere(false);
                        }
                    }
                }

                // (Login-time key-package replenish is session-owned: the shared
                // `start_receive_loop` — reached via `start_conversations_session`
                // below — runs `ensure_keypackages` AFTER the replica restore,
                // through the durable notify→autosave surface. A pre-restore mint
                // here would lose its private init keys to the restore's provider
                // swap — the very hazard `devices.md` § Cross-device MLS
                // group-state sync forbids — so the former "legacy DM path"
                // auto-publish is deleted.)

                // Install the external sync-agent surface (A3 cutover,
                // `sync-agent.md` § Control plane split): ensure the per-user
                // agent runs (systemd user unit; e2e → direct child spawn),
                // start the shared provisioning convergence loop (RenewBearer
                // grant + capability push), and start the folder-binding model,
                // which the agent's reconcile populates. Resident sync engines live in
                // the agent from here on — file sync keeps running app-dead.
                crate::sync_agent::install(fauna_client);
                start_sync_agent_status_poll(widgets);

                // No in-app backup upload driver is started here: the slice-5
                // flip (2026-07-29) made the owner's **source nest** the sole
                // segment-backup writer, so this app no longer uploads segments
                // at all. The nest's own `NestBackupWorker` sweeps every enrolled
                // owner with each app asleep; this page only *reads* that
                // work via `fauna.backup.status`, and audits it independently
                // (`crate::backup_audit`).
                // `message-segment-store.md` § Cross-location backup protocol.

                // Build the shared conversations session (FaunaMls + SMTP + inbound
                // mail) over the singleton manager and start its unified receive
                // loop — the SEND / membership / receive path all native apps
                // drive (the `fauna-ffi` factory twin), replacing linux's former
                // bespoke FaunaMls loop + inbound-mail poll loop (conversations.md §
                // Architectural rules #2). `self_address = <handle>@<domain>` from
                // the account cache seeds the session's live cell (empty until
                // populated — the `IdentityRefreshed` handler pushes the resolved
                // address when the background silent sign-in lands it).
                // Resolved ONCE and shared with every plane below (the `__mls`
                // replica and all three drafts rails) — never a second registry
                // walk, mirroring tui's single `succession_predecessor_backup_keys`
                // session-hook resolve (`FaunaClient::predecessor_backup_keys`'s
                // own doc comment).
                let succession_predecessors = fauna_client.predecessor_backup_keys();
                if let Some(ref mls) = state.borrow().mls {
                    let (handle, domain, _tier) = crate::client::load_account_cache();
                    let self_address = match (handle, domain) {
                        (Some(h), Some(d)) if !h.is_empty() && !d.is_empty() => {
                            format!("{h}@{d}")
                        }
                        _ => String::new(),
                    };
                    crate::conversations::conv_backend::start_conversations_session(
                        crate::conversations::host::manager(),
                        mls.engine(),
                        std::sync::Arc::clone(fauna_client.nest_rpc()),
                        self_address,
                        fauna_client.secret_hex(),
                        &fauna_client.runtime_handle(),
                        succession_predecessors.clone(),
                        fauna_client.ui_sender(),
                    );
                }

                // The W3 (account-data-plane.md § Workstreams) account-store runtime, hosted by this app
                // (`crate::account_runtime`; `account-data-plane.md` § The
                // account store → The client-side lifecycle). Installed AFTER
                // the conversations session above so the very first pump pass
                // can already answer the membership question — the member half
                // of the content-scope set is read off that session's live MLS
                // engine, which the bearer-only agent structurally cannot link,
                // so this process is the only one that makes joined `__conv`
                // channels get walked at all. Best-effort and fully spawned:
                // a failed assembly leaves every store-backed surface failing.
                //
                // The co-present ceremony's per-sign-in state is minted FIRST:
                // the share plane binds through its `SessionSeat`, the slot the
                // panel's own bind goes through (`p2p.md` § Offline share
                // initiation → *One seat per session*). Painted further down,
                // with the rest of this hook's offline-share half.
                #[cfg(feature = "p2p-share")]
                {
                    *widgets.offline_share_state.borrow_mut() =
                        crate::offline_share::init(fauna_client.secret_hex());
                    let share_seat = widgets.offline_share_state.borrow().session_seat.clone();
                    crate::account_runtime::install(fauna_client, share_seat);
                }
                #[cfg(not(feature = "p2p-share"))]
                crate::account_runtime::install(fauna_client);

                // Draft-persistence v2 (file-sync.md § Drafts Sync): restore the
                // user's conversation compose drafts from the `__drafts` reserved
                // folder on launch, and autosave (debounced) after each compose
                // edit so they survive restart and reach the user's other devices.
                // Independent of MLS — drafts seal under the owner's BackupKey.
                crate::conversations::drafts::start(
                    crate::conversations::host::manager(),
                    std::sync::Arc::clone(fauna_client.nest_rpc()),
                    fauna_client.secret_hex(),
                    &fauna_client.runtime_handle(),
                    &succession_predecessors,
                );

                // Draft-persistence v2, the `posts` rail (feed.md § Persistence):
                // same mechanism as the conversations leg above, one rail over.
                // `feed::host::init` already built the singleton during
                // `build_main_window`, so this is normally `Some`; a missing
                // manager is logged and skipped rather than failing login.
                if let Some(feed_manager) = crate::feed::host::manager() {
                    crate::feed::drafts::start(
                        feed_manager,
                        std::sync::Arc::clone(fauna_client.nest_rpc()),
                        fauna_client.secret_hex(),
                        &fauna_client.runtime_handle(),
                        &succession_predecessors,
                    );
                } else {
                    tracing::warn!("feed drafts: no feed manager yet; persistence disabled");
                }

                // Draft-persistence v2, the `events` rail (events.md
                // § Persistence): the third and last constant of
                // `fauna_protocol::drafts::DRAFT_RAILS`. Unlike the two above it
                // takes no manager — the Events compose lives in a transient
                // dialog, so the rail itself holds the draft (see that module's
                // header) — but the wiring point and the outside-every-MLS-gate
                // rule are identical.
                crate::views::events::drafts::start(
                    std::sync::Arc::clone(fauna_client.nest_rpc()),
                    fauna_client.secret_hex(),
                    &fauna_client.runtime_handle(),
                    &succession_predecessors,
                );

                // Author-side subscription reconcile loop (monetization.md § The
                // unifying model, grant path 2 + § Pillar 1): on connect (+ a poll
                // backstop) auto-approve queued `auto_approve` follows and re-drive
                // any crash-staged subscriber removal. Makes an encrypted-mode
                // FOLLOW frictionless — the nest can't mint the KeyBlob, so the
                // follow enqueues (`Queued`) until this loop mints + grants it.
                // Independent of MLS (subscriptions custody seals under the owner's
                // BackupKey), so it sits outside the conversations `mls` gate above.
                crate::subscriptions_author::start(
                    std::sync::Arc::clone(fauna_client.nest_rpc()),
                    fauna_client.secret_bytes(),
                    &fauna_client.runtime_handle(),
                );

                // Initialize P2P service. Reuses `account_dir`, already
                // resolved + adopted above alongside the MLS engine — both are
                // loose files under the same actor-scoped `fauna/` base.
                {
                    let actor_hex = state.borrow().actor_id.clone();
                    let actor_secret_hex = fauna_client.secret_hex();
                    match crate::p2p::P2pService::new(
                        &actor_hex,
                        actor_secret_hex,
                        &account_dir,
                        fauna_client.runtime_handle(),
                    ) {
                        Ok(p2p) => {
                            tracing::info!("P2P service initialized");
                            crate::settings::set_p2p(p2p.clone());
                            state.borrow_mut().p2p = Some(p2p.clone());
                            // P2P cell removed — peers now live in settings.
                        }
                        Err(e) => tracing::error!("P2P service init failed: {e}"),
                    }
                }

                // Trigger contacts, calendars, feeds, quota, node-info fetch.
                fauna_client.fetch_knocks();
                fauna_client.fetch_contacts();
                // (The review roster is read at the account store's ready edge,
                // by the post-store-ready pass's re-read — the succession ledger
                // it lives on does not exist before then;
                // `crate::succession_aftermath::run_ledger`.)
                fauna_client.fetch_calendars();
                // Feed + bridge feeds load via the Feed view's observer loop +
                // `connect_map` when the page is shown (FeedManager-driven).
                fauna_client.fetch_bridges();
                fauna_client.fetch_quota();
                fauna_client.fetch_account();
                fauna_client.fetch_nest_info();
                fauna_client.fetch_features();
                // The region relay: the first ask at login, then the shared
                // cadence (`REFRESH_INTERVAL_SECS`) on a minute tick.
                fauna_client.fetch_region(true);
                start_region_refresh_poll(fauna_client);
                // Initial Backups-page load — the snapshot half renders off the
                // shared `BackupsMachine` snapshot; nav-to-Backups refreshes it
                // again. This replaces the old pair of reads, one of which
                // (`fetch_snapshots("default")`) named a folder that need not
                // exist: selection is the machine's, and its default is the
                // first row of the real list.
                {
                    let machine = std::sync::Arc::clone(&widgets.backups_machine);
                    fauna_client
                        .runtime_handle()
                        .spawn(async move { machine.refresh().await });
                }
                // After `sync_agent::install` above has seeded the folder-binding
                // model `current_locations()` reads (status.md footnote 4 — the
                // Sync section used to never leave its "0"/"Never" placeholder).
                fauna_client.fetch_sync_status_summary();
                fauna_client.check_for_updates();
                // Initial Devices-page load — the page renders off the shared
                // `DevicesMachine` snapshot; nav-to-Devices refreshes it again.
                {
                    let machine = std::sync::Arc::clone(&widgets.devices_machine);
                    fauna_client
                        .runtime_handle()
                        .spawn(async move { machine.refresh().await });
                }
                // Initial Personalization / Community-labelers load — both
                // sub-pages render off the shared `LabelerCatalogMachine`;
                // nav-to-either refreshes it again (`connect_map`).
                {
                    let machine = std::sync::Arc::clone(&widgets.labeler_catalog_machine);
                    fauna_client
                        .runtime_handle()
                        .spawn(async move { machine.refresh().await });
                }
                // Recipient-side pending folder shares — the page-level "Shared
                // with you" section (`folders.md` § Sharing, Recipient side).
                // Re-fetched when the Folders sub-page becomes visible.
                fauna_client.fetch_folder_pending_shares();
                // The co-present offline-share ceremony's identity half
                // (`p2p.md` § Offline share initiation, row 334). Same
                // no-work-here posture as tui's own `session.rs` post-auth
                // hook, one step further: it does not even prepare a fetch —
                // this only resolves the actor id whose hex IS the compare
                // code, which is what makes the two entry buttons render; the
                // ceremony LISTENER binds only when the user opens a panel.
                // Then a repaint (the entry buttons become visible for the
                // first time) and the group-share surface's own fetch,
                // mirroring `fetch_folder_pending_shares` exactly.
                // (The state itself was minted above, before the account
                // runtime's install took its seat slot.)
                #[cfg(feature = "p2p-share")]
                {
                    views::devices_folders::folders::render_offline_share(
                        &widgets.offline_share,
                        &widgets.offline_share_state.borrow().view(),
                    );
                    fauna_client.fetch_group_shares();
                }
                // Initial Media-page load — the explorer renders off the shared
                // `MediaMachine` (cross-set `fauna.media.list`); nav-to-Media
                // refreshes it again (`connect_map`).
                {
                    let machine = std::sync::Arc::clone(&widgets.media_machine);
                    let backup_key =
                        fauna_core::crypto::BackupKey::derive(&fauna_client.secret_bytes())
                            .to_bytes()
                            .to_vec();
                    fauna_client
                        .runtime_handle()
                        .spawn(async move { machine.refresh(Some(backup_key)).await });
                }
                fauna_client.check_admin_status();
                // Clause 2 of the unfetched-policy ruling (family-safety.md
                // § Content policy): restore the last-known supervision
                // snapshot BEFORE the read below. "Read failed" and "read says
                // unsupervised" are different facts, and rendering the second
                // while the network is down is what let a ward walk past a
                // bedtime lock by going offline (§ Screen time is enforced by
                // pure client-local clock). Restores nothing when this device
                // has never completed one successful read — clause 3's declared
                // residual — so it costs an unsupervised user nothing.
                //
                // `own_policy`'s linux analogue (the ward's read-only policy
                // summary) is deliberately NOT restored: the snapshot carries
                // only the three client-enforced pillars, and synthesizing a
                // ReachPolicy from it would show default REACH knobs the
                // guardian never set. Those arrive with the read.
                if let Some(snapshot) = crate::supervision_snapshot::load() {
                    let guardian = snapshot.supervised_by.as_ref().map(|g| g.handle.clone());
                    // `usage_today_minutes` is None: the day's cross-device
                    // total is nest-accounted and unknowable offline. The
                    // window half still binds; the budget half resumes at the
                    // first heartbeat reply.
                    crate::screen_lock::set_ward_screen_time(
                        snapshot.screen_time,
                        guardian.clone(),
                        None,
                    );
                    crate::content_policy::set_ward_content_policy(snapshot.content_policy);
                    crate::content_policy::set_ward_content_notify(snapshot.content_notify);
                    if let Some(guardian) = guardian {
                        // § Screen time requires the Family page stay reachable
                        // read-only while a lock is up, so a restored lock must
                        // bring its explanation surface with it.
                        views::sidebar::show_family_sidebar_row(&widgets.sidebar_list_box);
                        widgets
                            .supervised_indicator
                            .set_label(&family::supervised_indicator(&guardian));
                        widgets.supervised_indicator.set_visible(true);
                    }
                }
                // The two GATED family surfaces are driven by one post-auth
                // `fauna.family.status` read (family-safety.md § App surface:
                // "Driven by fauna.family.status read at login"): the `family-tab`
                // sidebar row and the global `supervised-indicator`.
                fauna_client.check_family_status();
                // Hydrate the viewer's own spam/phishing thresholds for content-policy
                // render enforcement (family-safety.md § Content policy — the every-user
                // own-threshold collapse). The reply (`SpamPreferencesLoaded`) caches
                // into `content_policy`, so a feed/thread rendered later this session
                // composes the viewer's own thresholds without opening the privacy page.
                fauna_client.fetch_spam_preferences();

                // ── The succession ceremony's CLOSING ACT
                //    (`identity-succession.md` § The RecoveryKey → *At
                //    succession*): the successor's own first authenticated
                //    session mints a fresh RecoveryKey and SHOWS it.
                //
                //    Not a nicety, and not a background chore. The succession
                //    transaction deletes the old `recovery_escrow` row and
                //    retires the old kit, so from the moment the statement lands
                //    until this mint the account has **no kit and no escrow at
                //    all** — for a user who has just proven they are a theft
                //    target. A silent mint cannot close that window either:
                //    custody forbids persisting the secret, so an unshown mint
                //    registers a kit **nobody holds**, which the § names as
                //    strictly worse than never-created. Mint *and show*, or not
                //    at all.
                //
                //    **Ordering, both halves load-bearing.** The navigation is
                //    synchronous and comes FIRST: entering Account clears any kit
                //    on screen (the shown-once custody rule), so navigating after
                //    the mint landed would wipe the very thing this exists to
                //    show. And the spawn comes last in this arm — the minted-kit
                //    fold reads the session's actor id and handle to build the
                //    `fauna://recovery` payload (`recovery_kit::recovery_kit_uri`),
                //    so minting before the session is attached would produce a kit
                //    whose URI names no account.
                //
                //    Costs an ordinary sign-in one mutex read. `claim_succession_kit`
                //    answers false for every session that is not the successor a
                //    ceremony just named, and takes the obligation exactly once.
                {
                    let successor = state.borrow().actor_id.clone();
                    if crate::settings::recovery_kit::claim_succession_kit(&successor) {
                        tracing::info!(
                            "[succession] discharging the owed successor kit for {successor}"
                        );
                        // The settings shell, then its inner walk to Account —
                        // `navigate_shell_subpage` is the same door the e2e
                        // agent's nav arm takes (`main.rs`), never a second copy.
                        widgets.content_stack.set_visible_child_name("settings");
                        navigate_shell_subpage(&widgets.content_stack, "settings", "account");
                        // `create_recovery_kit` resolves the escrow blob's
                        // predecessor section from the registry's own succession
                        // link (`client.rs`'s `escrow_predecessor_seeds`, written
                        // by `record_succession` in the ceremony), so the
                        // device-loss backstop rides along with no extra
                        // plumbing here (§ Seed escrow).
                        fauna_client.create_recovery_kit();
                    }
                }
            }

            DataMessage::AuthFailed { error } => {
                {
                    let mut s = state.borrow_mut();
                    s.connected = false;
                }

                // Update tray connection state.
                *crate::tray::tray_state().connected.lock().unwrap() = false;

                widgets
                    .status_connection_row
                    .set_subtitle(errors::AUTH_FAILED);
                let auth_err_msg = errors::auth_error(error);
                show_toast(widgets, &auth_err_msg);
                set_error_message(widgets, &auth_err_msg);
            }

            DataMessage::NestIdentityChanged => {
                // `security.md` § Post-auth surfacing: route the verdict to the
                // SAME blocking launch surface the launch path renders — no
                // banner, no badge, no toast. Deliberately NOT the error/toast
                // treatment two arms above: those are for faults the session
                // can survive, and this session cannot (its connections can no
                // longer graduate), so a soft surface over it would be a
                // generic-error mystery instead of the honest verdict.
                //
                // The handler (registered in `main.rs`, holding the
                // `adw::Application` the way sign-out and account-switch do)
                // tears the session down and re-enters the launch flow.
                crate::settings::escalate_to_launch("nest-identity-changed");
            }

            DataMessage::IdentitySuperseded => {
                // The second terminal verdict, and the SAME escalation — one
                // handler, because the teardown is verdict-agnostic and the
                // launch flow re-derives which surface to render. Credentials
                // are kept here too: the old secret is still the user's, and it
                // is what a successor ceremony and any later re-import reason
                // about. The account moved, not the person.
                crate::settings::escalate_to_launch("identity-superseded");
            }

            DataMessage::SignInRefused => {
                // The third terminal verdict, the same escalation: suspended
                // or removed while signed in. Credentials kept — the identity
                // is still the user's, and the admin's restore plus the
                // surface's Retry is the way back in.
                crate::settings::escalate_to_launch("sign-in-refused");
            }

            DataMessage::KnocksLoaded { knocks } => {
                state.borrow_mut().knocks = knocks.clone();
                views::contacts::list::update_knocks_list(
                    &widgets.knocks_list_box,
                    knocks,
                    fauna_client,
                );
            }

            DataMessage::ContactsLoaded { contacts } => {
                state.borrow_mut().contacts = contacts.clone();
                views::contacts::list::update_contacts_list(
                    &widgets.contacts_list_box,
                    &widgets.contacts_filter_index,
                    contacts,
                    &state.borrow().member_reviews.reviews(),
                    fauna_client,
                );
            }

            DataMessage::MemberReviewsLoaded { reviews } => {
                // The roster's one writer (`settings::member_review::Roster`).
                // Setting it repaints the conversations page's member-chip pair,
                // which subscribed at build; the contacts badge is repainted here
                // off the same roster (`identity-succession.md` § Propagation →
                // *MLS groups*, item 3a). The `Rc` is cloned out first so no
                // `AppState` borrow is held across the repaint.
                let roster = Rc::clone(&state.borrow().member_reviews);
                roster.set(reviews.clone());
                let contacts = state.borrow().contacts.clone();
                views::contacts::list::update_contacts_list(
                    &widgets.contacts_list_box,
                    &widgets.contacts_filter_index,
                    &contacts,
                    reviews,
                    fauna_client,
                );
            }

            DataMessage::ContactOverlaysChanged => {
                let (contacts, knocks, roster) = {
                    let st = state.borrow();
                    (
                        st.contacts.clone(),
                        st.knocks.clone(),
                        Rc::clone(&st.member_reviews),
                    )
                };
                views::contacts::list::update_knocks_list(
                    &widgets.knocks_list_box,
                    &knocks,
                    fauna_client,
                );
                views::contacts::list::update_contacts_list(
                    &widgets.contacts_list_box,
                    &widgets.contacts_filter_index,
                    &contacts,
                    &roster.reviews(),
                    fauna_client,
                );
            }

            DataMessage::CalendarsLoaded { calendars } => {
                tracing::debug!("CalendarsLoaded: {} calendars", calendars.len());
                // Fetch events for each calendar so grids can display them. The
                // EventsLoaded handler skips the grid rebuild when a calendar's
                // events are unchanged, so the Events-page poll stays a UI no-op
                // in steady state.
                for cal in calendars.iter() {
                    fauna_client.fetch_events(&cal.id);
                }
                let cal_handles = &widgets.events_handles.calendar;
                let state = &cal_handles.state;
                // Only rebuild the sidebar when the calendar set actually changed.
                // The poll re-lists on a fixed cadence; rebuilding an unchanged
                // list would destroy/recreate `calendar-item` widgets every
                // interval (flicker + a race with a list click in progress). On a
                // real change (a new external calendar, a rename) the set differs
                // and we rebuild — so external creates still "appear quickly".
                let calendars_changed =
                    rows_differ_by_id(&state.borrow().displayed_calendars, calendars, |c| {
                        c.id.as_str()
                    });
                if calendars_changed {
                    state.borrow_mut().displayed_calendars = calendars.clone();
                    let mgc = &cal_handles.month_grid_container;
                    // Refreshing every surface — the three grids AND the agenda
                    // — is what a scope change (visibility toggle or calendar
                    // selection) means. The agenda used to be omitted here while
                    // `calendar_view.rs`'s own build-time callback included it,
                    // so once a `CalendarsLoaded` rebuilt the rows the agenda
                    // stopped tracking the checkboxes.
                    let refresh_all_views = {
                        let mgc2 = mgc.clone();
                        let wgc2 = cal_handles.week_grid_container.clone();
                        let dgc2 = cal_handles.day_grid_container.clone();
                        let agc2 = cal_handles.agenda_container.clone();
                        let st2 = std::rc::Rc::clone(state);
                        let cl2 = std::rc::Rc::clone(fauna_client);
                        let hooks2 = cal_handles.month_hooks.clone();
                        move || {
                            crate::views::events::month_grid::refresh_month_grid(
                                &mgc2, &st2, &cl2, &hooks2,
                            );
                            crate::views::events::week_grid::refresh_week_grid(&wgc2, &st2, &cl2);
                            crate::views::events::day_grid::refresh_day_grid(&dgc2, &st2, &cl2);
                            crate::views::events::event_list::refresh_agenda_view(
                                &agc2, &st2, &cl2,
                            );
                        }
                    };
                    let on_select = {
                        // `calendar-item` click → select that calendar
                        // (goal/ui/events.md § User actions). Repaint the
                        // selection marker in place rather than rebuilding the
                        // rows we were just clicked through.
                        let st3 = std::rc::Rc::clone(state);
                        let list_box = cal_handles.calendar_list_box.clone();
                        let refresh = refresh_all_views.clone();
                        move |calendar_id: String| {
                            {
                                let mut s = st3.borrow_mut();
                                s.selected_calendar = Some(calendar_id.clone());
                                s.selected_event = None;
                            }
                            crate::views::events::calendar_sidebar::mark_selected_calendar(
                                &list_box,
                                Some(&calendar_id),
                            );
                            refresh();
                        }
                    };
                    crate::views::events::calendar_sidebar::update_calendar_sidebar(
                        &cal_handles.calendar_list_box,
                        calendars,
                        state,
                        refresh_all_views,
                        on_select,
                    );
                    // Refresh active grid with updated calendar colors.
                    let view_mode = state.borrow().view_mode;
                    match view_mode {
                        crate::views::events::calendar_view::ViewMode::Month => {
                            crate::views::events::month_grid::refresh_month_grid(
                                mgc,
                                state,
                                fauna_client,
                                &cal_handles.month_hooks,
                            );
                        }
                        crate::views::events::calendar_view::ViewMode::Week => {
                            crate::views::events::week_grid::refresh_week_grid(
                                &cal_handles.week_grid_container,
                                state,
                                fauna_client,
                            );
                        }
                        crate::views::events::calendar_view::ViewMode::Day => {
                            crate::views::events::day_grid::refresh_day_grid(
                                &cal_handles.day_grid_container,
                                state,
                                fauna_client,
                            );
                        }
                        crate::views::events::calendar_view::ViewMode::Agenda => {}
                    }
                }
            }

            DataMessage::EventsLoaded {
                calendar_id,
                events,
            } => {
                state.borrow_mut().events = events.clone();
                let cal_handles = &widgets.events_handles.calendar;
                let view_mode = cal_handles.state.borrow().view_mode;
                // Events are fetched per-calendar; replace only this calendar's
                // slice of the cache so the agenda/grids see the union across all
                // visible calendars (not just the last-fetched one). An empty
                // calendar_id (cross-calendar query) replaces no slice. Detect
                // whether the slice actually changed: the Events-page poll
                // re-fetches on a fixed cadence, and re-running the reminder
                // check + rebuilding the event-card grid on identical data would
                // spam reminder notifications every interval and race a card
                // click in progress. On a real change (an external write) the
                // slice differs and we refresh — so external events still
                // "appear quickly".
                let events_changed = {
                    let mut s = cal_handles.state.borrow_mut();
                    let prev: Vec<crate::rows::EventRow> = if calendar_id.is_empty() {
                        Vec::new()
                    } else {
                        s.events_cache
                            .iter()
                            .filter(|e| e.calendar_id.as_str() == calendar_id.as_str())
                            .cloned()
                            .collect()
                    };
                    let changed = rows_differ_by_id(&prev, events, |e| e.id.as_str());
                    if !calendar_id.is_empty() {
                        s.events_cache
                            .retain(|e| e.calendar_id.as_str() != calendar_id.as_str());
                    }
                    s.events_cache.extend(events.clone());
                    changed
                };
                if !events_changed {
                    return;
                }
                check_event_reminders(events);
                // Refresh the active grid with new events.
                match view_mode {
                    crate::views::events::calendar_view::ViewMode::Month => {
                        crate::views::events::month_grid::refresh_month_grid(
                            &cal_handles.month_grid_container,
                            &cal_handles.state,
                            fauna_client,
                            &cal_handles.month_hooks,
                        );
                    }
                    crate::views::events::calendar_view::ViewMode::Week => {
                        crate::views::events::week_grid::refresh_week_grid(
                            &cal_handles.week_grid_container,
                            &cal_handles.state,
                            fauna_client,
                        );
                    }
                    crate::views::events::calendar_view::ViewMode::Day => {
                        crate::views::events::day_grid::refresh_day_grid(
                            &cal_handles.day_grid_container,
                            &cal_handles.state,
                            fauna_client,
                        );
                    }
                    crate::views::events::calendar_view::ViewMode::Agenda => {
                        crate::views::events::event_list::refresh_agenda_view(
                            &cal_handles.agenda_container,
                            &cal_handles.state,
                            fauna_client,
                        );
                    }
                }
            }

            DataMessage::AddressbooksLoaded { addressbooks } => {
                tracing::debug!("AddressbooksLoaded: {} books", addressbooks.len());
                crate::views::contacts::address_book::update_book_list(
                    &widgets.address_book,
                    addressbooks,
                    fauna_client,
                );
            }

            DataMessage::CardsLoaded {
                addressbook_id,
                cards,
            } => {
                tracing::debug!("CardsLoaded: book={addressbook_id}, {} cards", cards.len());
                crate::views::contacts::address_book::update_card_list(
                    &widgets.address_book,
                    addressbook_id,
                    cards,
                );
            }

            DataMessage::CardLocated { addressbooks, open } => {
                tracing::debug!(
                    "CardLocated: {} books, found={}",
                    addressbooks.len(),
                    open.is_some()
                );
                // The stack + Address Book segment are already switched
                // synchronously by the search-result click that fired this
                // locate (`views::search::open_search_result`) — this handler
                // only paints the resolve's result.
                crate::views::contacts::address_book::open_located_card(
                    &widgets.address_book,
                    addressbooks,
                    open.clone(),
                    fauna_client,
                );
                // No book holds that `uid_hash` any more: the card was deleted
                // between being indexed and being clicked. Say so on
                // `error-message` — the same DROPPED outcome a query-time
                // resolve gives, except this one has a user waiting on it, and
                // a silently-repainted picker would read as a dead row
                // (`ui/search.md` § Where logic lives → Result navigation
                // (deep link), the Contact bullet's DROPPED clause; matches
                // tui's `Outcome::CardLocated` handling).
                if open.is_none() {
                    set_error_message(widgets, contacts::address_book::CARD_NOT_FOUND);
                }
            }

            DataMessage::EventAttendeesLoaded {
                event_id,
                attendees,
            } => {
                tracing::debug!(
                    "EventAttendeesLoaded: event_id={}, {} attendees",
                    event_id,
                    attendees.len()
                );
                // Cache attendees, then re-render the detail panel if this is the
                // currently-selected event (observer-driven, per goal/ui/events.md
                // arch rule #1) — so a freshly-RSVP'd attendee appears without
                // re-opening anything.
                let cal_handles = &widgets.events_handles.calendar;
                let is_selected = {
                    let mut s = cal_handles.state.borrow_mut();
                    s.attendees_cache
                        .insert(event_id.clone(), attendees.clone());
                    s.selected_event.as_ref().map(|e| e.id.as_str()) == Some(event_id.as_str())
                };
                if is_selected {
                    crate::views::events::event_detail::refresh_event_detail_panel(
                        &cal_handles.detail_panel,
                        &cal_handles.state,
                        fauna_client,
                    );
                }
            }

            DataMessage::EventReminderLoaded { event_id, offset } => {
                // Cache the reminder offset and re-render the detail panel when
                // this is the selected event (same observer-driven pattern as
                // EventAttendeesLoaded), so the reminder control flips between
                // its set/unset states without re-opening.
                let cal_handles = &widgets.events_handles.calendar;
                let is_selected = {
                    let mut s = cal_handles.state.borrow_mut();
                    s.reminders_cache.insert(event_id.clone(), offset.clone());
                    s.selected_event.as_ref().map(|e| e.id.as_str()) == Some(event_id.as_str())
                };
                if is_selected {
                    crate::views::events::event_detail::refresh_event_detail_panel(
                        &cal_handles.detail_panel,
                        &cal_handles.state,
                        fauna_client,
                    );
                }
            }

            DataMessage::EventSelected { event } => {
                // An event card was clicked (or selection cleared). Store it and
                // re-render the persistent detail panel.
                let cal_handles = &widgets.events_handles.calendar;
                cal_handles.state.borrow_mut().selected_event = event.clone();
                crate::views::events::event_detail::refresh_event_detail_panel(
                    &cal_handles.detail_panel,
                    &cal_handles.state,
                    fauna_client,
                );
                // Load the reminder for the freshly-selected event so the detail
                // panel's reminder control shows the right state (mirrors web's
                // openDetail → getReminder). Cleared selection (None) needs no
                // fetch.
                if let Some(ev) = event {
                    fauna_client.get_reminder(&ev.calendar_id, &ev.id);
                }
            }

            DataMessage::BridgesLoaded { bridges } => {
                // Retain the snapshot (id → BridgeStatus) for EVERY provider —
                // including the dedicated-page ones the Bridges list itself
                // filters out — so the detail pane can render the
                // metadata-driven link form from `link_modes` on row
                // activation, and the AT Protocol settings page can render the same
                // components as its Linked-account panel.
                {
                    let mut snap = widgets.bridges_snapshot.borrow_mut();
                    snap.clear();
                    for b in bridges {
                        snap.insert(b.id.clone(), b.clone());
                    }
                }
                // Repaint any settings page embedding a bridge surface (the
                // Bluesky Linked panel) — a link/unlink completed on the
                // Bridges page must not leave it stale.
                crate::settings::notify_bridges_changed();
                // `update_bridge_list` applies the unified-page filter itself.
                views::bridges::update_bridge_list(&widgets.bridges_list_box, bridges);

                // If any bridge is Bluesky and linked, kick off notification
                // polling.  The first call fires immediately; subsequent calls
                // happen every 60 s via a glib timer set up here.
                // NOTE: this reads the UNFILTERED list on purpose — Bluesky is
                // excluded from the unified Bridges page, so filtering upstream
                // would silently disable this trigger for every linked user.
                let has_linked_bluesky = bridges
                    .iter()
                    .any(|b| (b.id.contains("bluesky") || b.id.contains("atproto")) && b.linked);
                if has_linked_bluesky {
                    // The endpoint is unified, but the trigger condition is
                    // still Bluesky-link-only. Fauna-native polling triggers
                    // are a follow-up .
                    start_notification_poll(fauna_client);
                }
            }

            DataMessage::ModerationActionsLoaded { rows } => {
                views::moderation::update_moderation_queue(
                    &widgets.moderation_queue_list_box,
                    rows,
                    fauna_client,
                );
            }

            DataMessage::BridgeFollowsLoaded { bridge_id, follows } => {
                tracing::debug!(
                    "BridgeFollowsLoaded: bridge_id={}, {} follows",
                    bridge_id,
                    follows.len()
                );
                // Repaint the Bridges page's own detail pane, but only when it
                // is currently open ON this bridge — a reply for a bridge the
                // user has since navigated away from must not repaint a
                // follows_list that belongs to a different (or torn-down)
                // detail.
                if let Some((open_id, follows_list)) = widgets.bridges_open_detail.borrow().as_ref()
                    && open_id == bridge_id
                {
                    let c = Rc::clone(fauna_client);
                    let bid = bridge_id.clone();
                    views::bridges::detail::populate_follows_list(
                        follows_list,
                        follows,
                        move |follow_id| c.remove_bridge_follow(&bid, follow_id),
                    );
                }
                // Repaint the AT Protocol settings page's embedded panel, if it
                // registered a hook — a no-op when that page has never been
                // built this session.
                crate::settings::notify_bridge_follows_loaded(bridge_id, follows);
            }

            DataMessage::FeedSourceRefused {
                bridge_id,
                operation,
                target,
            } => {
                // Rule (b): the refusal STAYS on `error-message` — the operation
                // did not happen — it just stops being a dead end: the open
                // bridge card now offers the ask for exactly this triple.
                crate::ward_asks::note_feed_refusal(bridge_id, operation, target);
                set_error_message(widgets, crate::i18n::strings::bridges::SOURCE_BLOCKED);
                views::bridges::detail::repaint_source_asks();
            }

            DataMessage::NotificationsLoaded { notifications } => {
                // Desktop toast for each unread row, saying what the row says —
                // the OS-level notification is localized exactly like the row
                // it announces (`behavior/notifications.md` § Localized body).
                for item in notifications.iter().filter(|n| !n.is_read) {
                    crate::notifications::notify_unified(
                        &item.notif_type,
                        &crate::i18n::notification_text(item),
                    );
                }
                // Render them. Before 2026-07-12 this handler only counted +
                // toasted, so the notifications *page* stayed empty on every
                // path — the rows had nowhere to land. Rebuilding here (the
                // `KnocksLoaded` pattern) makes the page live while mounted:
                // this arm runs on the `fauna.notification` push, so a
                // notification arriving under the user's eyes now appears.
                let unread = views::notifications::update_notifications_list(
                    &widgets.notifications_list_box,
                    &widgets.notifications_count_badge,
                    notifications,
                );
                state.borrow_mut().notifications_unread_count = unread;
                tracing::debug!(
                    "NotificationsLoaded: {} items ({} unread)",
                    notifications.len(),
                    unread
                );
            }

            DataMessage::BlueskyThreadLoaded {
                post_id,
                posts,
                focal_index,
            } => {
                tracing::debug!(
                    "BlueskyThreadLoaded: post_id={} posts={}",
                    post_id,
                    posts.len()
                );
                // Show thread in the feed content stack as a "thread" page.
                let stack = &widgets.feed_content_stack;
                if let Some(old) = stack.child_by_name("thread") {
                    stack.remove(&old);
                }
                let s = stack.clone();
                let thread_view = views::feed::post_detail::build_thread_view(
                    posts,
                    *focal_index,
                    fauna_client,
                    move || {
                        // Back: return to whichever page was showing before.
                        s.set_visible_child_name("list");
                        if let Some(t) = s.child_by_name("thread") {
                            s.remove(&t);
                        }
                    },
                );
                stack.add_named(&thread_view, Some("thread"));
                stack.set_visible_child_name("thread");
            }

            DataMessage::SyncFilesLoaded { folder: _, files } => {
                // Per-set sync-file state feeds the Backups state report
                // (`data.sync.files`); the Media page no longer consumes it (the
                // explorer reads the cross-set `fauna.media.list` via MediaMachine).
                state.borrow_mut().sync_files = files.clone();
            }

            DataMessage::FolderMembersLoaded { name, members } => {
                // The device roster is a lazy row-detail read (not owned by the
                // `DevicesMachine`); populate it into the expander row by name.
                // It paints the place EDITOR, so it needs the machine (the
                // `set_folder_place` write) and the client (the roster re-read
                // that repaints from nest truth afterwards).
                views::devices_folders::folders::populate_folder_members(
                    &widgets.folder_list_box,
                    name,
                    members,
                    &widgets.devices_machine,
                    fauna_client,
                );
            }

            DataMessage::FolderActorsLoaded {
                name,
                members,
                channel_id,
            } => {
                // The cross-user actor roster ("Shared with") is a lazy/eager
                // row-detail read; populate the section + `folder-shared-badge`
                // by name and wire each `folder-member-remove-button` (needs the
                // client + the set's derived channel id).
                //
                // The published-folder writer warning is state-based: each
                // member row asks the shared `writer_grant_reach` with the set's
                // audience as the machine's snapshot has it right now.
                let reach = views::devices_folders::folders::WriterReach::for_set(
                    &widgets.devices_machine,
                    name,
                );
                views::devices_folders::folders::populate_folder_actors(
                    &widgets.folder_list_box,
                    name,
                    members,
                    channel_id.as_deref(),
                    fauna_client,
                    &reach,
                );
            }

            DataMessage::FolderDevicesLoaded { name, devices } => {
                // The device-activity roster is a lazy/push-refreshed row-detail
                // read (not owned by the `DevicesMachine`); populate it into the
                // expander row by name — same shape as the member roster above,
                // just re-runnable in place since `PushEvent::SyncChanged` (below)
                // sends this again on every recorded change while the row stays
                // expanded.
                views::devices_folders::folders::populate_folder_devices(
                    &widgets.folder_list_box,
                    name,
                    devices,
                );
            }

            DataMessage::FolderDestinationsLoaded {
                name,
                folder_id,
                places,
            } => {
                // The destination-places section is a lazy/mutation-refreshed
                // row-detail read; populate it into the expander row by name
                // (needs `folder_id` + the client to wire the attach/detach
                // handlers).
                views::devices_folders::folders::populate_folder_destinations(
                    &widgets.folder_list_box,
                    name,
                    *folder_id,
                    places,
                    fauna_client,
                );
            }

            DataMessage::FolderPendingSharesLoaded { shares } => {
                // The recipient-side "Shared with you" staged shares — a
                // page-level read (not `DevicesMachine` state); cached (not
                // just re-painted) because the co-present ceremony's own
                // invitations continue the SAME indexed `folder-pending-share`
                // list, and the two are independent async round-trips that
                // must never clobber each other's already-painted rows.
                let mut s = state.borrow_mut();
                s.folder_pending_shares = shares.clone();
                views::devices_folders::folders::populate_pending_shares(
                    &widgets.folder_pending_shares_box,
                    &s.folder_pending_shares,
                    fauna_client,
                );
                #[cfg(feature = "p2p-share")]
                {
                    views::devices_folders::folders::populate_group_invitations(
                        &widgets.folder_pending_shares_box,
                        &s.group_shares.invitations,
                        fauna_client,
                        &widgets.offline_share_state,
                    );
                    widgets.folder_pending_shares_group.set_visible(
                        !s.folder_pending_shares.is_empty()
                            || !s.group_shares.invitations.is_empty(),
                    );
                }
                #[cfg(not(feature = "p2p-share"))]
                widgets
                    .folder_pending_shares_group
                    .set_visible(!s.folder_pending_shares.is_empty());
            }

            // The co-present ceremony's group-share surface — pending
            // consent-card invitations AND the shared sets this device can
            // actually read (`p2p.md` § Offline share initiation, row 334).
            // Cached for the same reason `FolderPendingSharesLoaded` caches
            // its half — see that arm's comment.
            #[cfg(feature = "p2p-share")]
            DataMessage::GroupSharesLoaded { views: group_views } => {
                let mut s = state.borrow_mut();
                s.group_shares = group_views.clone();
                views::devices_folders::folders::populate_pending_shares(
                    &widgets.folder_pending_shares_box,
                    &s.folder_pending_shares,
                    fauna_client,
                );
                views::devices_folders::folders::populate_group_invitations(
                    &widgets.folder_pending_shares_box,
                    &s.group_shares.invitations,
                    fauna_client,
                    &widgets.offline_share_state,
                );
                widgets.folder_pending_shares_group.set_visible(
                    !s.folder_pending_shares.is_empty() || !s.group_shares.invitations.is_empty(),
                );
                drop(s);
                // The same read's other half: the shared sets this device can
                // read list as `folder-row`s, painted from here and nowhere
                // else, so a card and its landed set never disagree.
                (widgets.repaint_group_scopes)(&group_views.scopes);
            }

            // A bind door came back with this session's seat — the panel's,
            // after `offline-share-button` / `offline-receive-button` opened
            // it, or the share plane's driver. Nothing to fold: both went
            // through the panel state's `SessionSeat`, so the seat is already
            // the panel's in either order. Repaint.
            #[cfg(feature = "p2p-share")]
            DataMessage::OfflineShareSeatBound | DataMessage::SharePlaneSeatBound => {
                views::devices_folders::folders::render_offline_share(
                    &widgets.offline_share,
                    &widgets.offline_share_state.borrow().view(),
                );
            }

            // A pump pass moved the transfer surface's state cell: repaint the
            // six ids from it. Payload-free — the cell is the truth, and the
            // driver posts this only on actual change.
            #[cfg(feature = "p2p-share")]
            DataMessage::SharePlaneChanged => {
                let state = crate::share_glue::state();
                views::devices_folders::folders::render_share_transfers(
                    &widgets.share_transfer,
                    state.as_ref(),
                );
            }

            // The account store may have changed under an open page: re-drive
            // that page's own load (reload semantics — the same refetch a
            // same-page nav fires). Any other page reads fresh on its next
            // visit, so nothing runs for it.
            DataMessage::AccountStoreChanged => {
                use crate::store_surfaces::StoreSurface;
                let open = crate::store_surfaces::open_surface(
                    widgets.content_stack.visible_child_name().as_deref(),
                    widgets.settings_sub_stack.visible_child_name().as_deref(),
                );
                match open {
                    // The sealed scorers load only inside the manager's
                    // reload; Trending is preserved (not a `selected_feed`).
                    Some(StoreSurface::Feed) => {
                        if let Some(m) = crate::feed::host::manager() {
                            fauna_client
                                .runtime_handle()
                                .spawn(async move { m.refresh_current_feed().await });
                        }
                    }
                    Some(surface) => (widgets.settings_store_resync)(surface),
                    None => {}
                }
            }

            // This side's ceremony progressed — begin / consent / decline all
            // report through this one variant.
            #[cfg(feature = "p2p-share")]
            DataMessage::OfflineShareProgressed { status } => {
                widgets.offline_share_state.borrow_mut().status = *status;
                views::devices_folders::folders::render_offline_share(
                    &widgets.offline_share,
                    &widgets.offline_share_state.borrow().view(),
                );
                // A ceremony that landed a scope lists it on the page the user
                // is looking at: re-read the group listing now, not at the
                // next nav edge.
                if status.lands_a_scope() {
                    fauna_client.fetch_group_shares();
                }
            }

            // A ceremony step failed — the brake refused, the dial failed, or
            // a consent/decline round-trip errored. A bind that never
            // produced a seat closes the panel again: the open was
            // optimistic, and a panel over a listener that never came up
            // would invite the user to type a code that can never work. A
            // ceremony that failed on a LIVE seat keeps its panel — there the
            // user can read the error and try again without re-navigating
            // (mirrors tui's `Outcome::OfflineShareFailed`).
            #[cfg(feature = "p2p-share")]
            DataMessage::OfflineShareFailed { message } => {
                let mut s = widgets.offline_share_state.borrow_mut();
                if s.seat().is_none() {
                    s.panel =
                        fauna_client_capabilities::group_ceremony_view::OfflineSharePanel::Closed;
                    s.peer_code_input.clear();
                }
                // The terminal status flip tui's own `Outcome::OfflineShareFailed`
                // makes (settings/mod.rs) — missing here left a failed ceremony
                // showing only the transient global banner while the panel's own
                // status readout stayed on whatever it was before the failure,
                // never reaching `Failed` at all (measured:
                // `test_offline_share_two_seat.py`'s three ceremony-outcome tests).
                s.status = fauna_client_capabilities::group_ceremony_view::CeremonyStatus::Failed;
                drop(s);
                views::devices_folders::folders::render_offline_share(
                    &widgets.offline_share,
                    &widgets.offline_share_state.borrow().view(),
                );
                // The global banner — the same `ActionResult::FailedLocalized`
                // path `accept_folder_share`/`decline_folder_share` already use
                // for a folder-sharing failure.
                set_error_message(widgets, message);
            }

            DataMessage::MessageKindSnapshotsLoaded { snapshots } => {
                views::backups::restore::populate_snapshot_select(
                    &widgets.backups_restore,
                    snapshots,
                );
            }

            DataMessage::MessageKindRestored { config_present } => {
                show_toast(widgets, &common::success_detail("message_kind_restored"));
                // The restore wrote a restore_history row; refresh the history
                // section (which re-fires per-row divergence).
                fauna_client.fetch_restore_history();
                // `restore-progress` reaches its TERMINAL state here and nowhere
                // else — the click arms `Running` — and `restore-warning` is
                // decided in the same pass, so a reader that sees DONE sees the
                // advisory's final verdict (`../../docs/goal/ui/backups.md`
                // § Restore from backup destination).
                widgets
                    .backups_restore
                    .progress
                    .set_text(crate::i18n::strings::backups::RESTORE_PROGRESS_DONE);
                widgets.backups_restore.warning.set_visible(!config_present);
            }

            DataMessage::RestoreHistoryLoaded { rows } => {
                views::backups::restore::populate_restore_history(
                    &widgets.backups_restore,
                    rows,
                    fauna_client,
                );
                // Fire one divergence fetch per row to fill the banners.
                for row in rows {
                    fauna_client.fetch_restore_divergence(row.snapshot_id);
                }
            }

            DataMessage::RestoreDivergenceLoaded { snapshot_id, rows } => {
                views::backups::restore::populate_divergence(
                    &widgets.backups_restore,
                    *snapshot_id,
                    rows,
                );
            }

            DataMessage::AccountLoaded { handle } => {
                // Persist the handle so the status bar and account settings
                // page don't show "(not loaded)" after a fresh sign-in. The
                // auth-token response doesn't carry the handle, so we fetch
                // /api/v1/account separately after auth success.
                crate::settings::set_handle(handle.clone());
                if let Some(h) = handle.as_deref() {
                    state.borrow_mut().handle = h.to_string();
                    widgets.status_handle_row.set_subtitle(h);

                    // Also refresh the libsecret cache so the next cold
                    // launch can show the handle instantly without
                    // waiting for /api/v1/account. Best-effort + log:
                    // a libsecret failure here just means we'll re-write
                    // on the next account fetch (idempotent).
                    if let Err(e) = crate::client::store_account_cache(Some(h), None, None) {
                        tracing::error!("[libsecret] cache handle write failed: {e:#}");
                    }
                }
            }

            DataMessage::IdentityRefreshed {
                handle,
                domain,
                tier,
            } => {
                // Authoritative refresh from /api/v1/auth/verify on launch:
                // overwrite both the in-memory state and the libsecret
                // cache (already written by FaunaClient::silent_sign_in
                // before this message; this branch updates the UI and the
                // conversations session's live self-address cell).
                crate::settings::set_handle(Some(handle.clone()));
                state.borrow_mut().handle = handle.clone();
                widgets.status_handle_row.set_subtitle(handle);
                // The ONE self-heal call (`conversations.md` § State & data
                // shape → *Self-address: live, never baked*): a session built
                // before this refresh landed (empty cache) starts sending, and
                // a server-side handle rename reaches the SMTP `From:`, the MLS
                // same-nest routing domain, and the reply-all self-drop — no
                // backend rebuilt, no compose-path re-register. Only a full
                // `<handle>@<domain>` is pushed; while unresolved the empty
                // cell keeps the honest local-refusal floor.
                if !handle.is_empty()
                    && !domain.is_empty()
                    && let Some(session) = crate::conversations::conv_backend::active_session()
                {
                    session.set_self_address(format!("{handle}@{domain}"));
                }
                let _ = tier; // settings UI doesn't surface tier yet
            }

            DataMessage::QuotaLoaded { quota } => {
                views::status::update_quota(&widgets.status_handles(), quota);
            }

            DataMessage::NestInfoLoaded { info } => {
                views::status::update_nest_info(&widgets.status_handles(), info);
            }

            DataMessage::FeaturesLoaded { rows } => {
                views::status::update_features(&widgets.status_handles(), rows);
            }

            DataMessage::RegionReplies { replies } => {
                // Verified and folded in shared Rust; the device record is
                // persisted and the render engine re-armed inside. The feed and
                // thread surfaces compose the new rule sets on their next build,
                // exactly as a guardian-floor change does.
                crate::region::apply_replies(replies.clone());
                crate::region::paint_settings(&widgets.status_region_rows_box);
            }

            DataMessage::SyncStatusSummaryLoaded {
                files_synced,
                last_sync_at,
            } => {
                views::status::update_sync_status(
                    &widgets.status_handles(),
                    *files_synced,
                    *last_sync_at,
                );
            }

            DataMessage::HandleResolved { result } => {
                // Populate the contact-find results list with the resolved actor.
                let list = &widgets.find_results_list_box;
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                if let Some(actor_id) = result.get("actor_id").and_then(|v| v.as_str()) {
                    let handle = result
                        .get("handle")
                        .and_then(|v| v.as_str())
                        .unwrap_or(actor_id);
                    // The nest echoes the resolved domain (the typed `@domain` for a
                    // multi-domain handle, else its canonical/identity domain), so the
                    // row shows `bob@domain2` — parity with the web find-result span
                    // (contacts/+page.svelte `{handle}@{domain}`). Guard the rare
                    // empty-domain case (a search result with no domain) back to the bare `@handle`.
                    let domain = result.get("domain").and_then(|v| v.as_str()).unwrap_or("");
                    let display = if handle == actor_id {
                        fauna_core::format::short_id(actor_id)
                    } else if domain.is_empty() {
                        format!("@{} ({})", handle, fauna_core::format::short_id(actor_id))
                    } else {
                        format!(
                            "{}@{} ({})",
                            handle,
                            domain,
                            fauna_core::format::short_id(actor_id)
                        )
                    };
                    let row = views::contacts::find::build_find_result_row(
                        &display,
                        actor_id,
                        fauna_client,
                    );
                    list.append(&row);
                    list.set_visible(true);
                } else {
                    // No result — show a "not found" label.
                    let label =
                        gtk::Label::new(Some(crate::i18n::strings::contacts::HANDLE_NOT_FOUND));
                    label.add_css_class("dim-label");
                    label.set_margin_top(8);
                    label.set_margin_bottom(8);
                    let row = gtk::ListBoxRow::new();
                    row.set_child(Some(&label));
                    row.set_selectable(false);
                    list.append(&row);
                    list.set_visible(true);
                }
            }

            DataMessage::NestResolved {
                domain,
                handle,
                result,
            } => {
                // Show the resolved nest URL with a "Look up" button.
                let list = &widgets.find_results_list_box;
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                if let Some(url) = result.get("url").and_then(|v| v.as_str()) {
                    if url.is_empty() {
                        let label =
                            gtk::Label::new(Some(&errors::could_not_resolve_domain(domain)));
                        label.add_css_class("dim-label");
                        label.set_margin_top(8);
                        label.set_margin_bottom(8);
                        let row = gtk::ListBoxRow::new();
                        row.set_child(Some(&label));
                        row.set_selectable(false);
                        list.append(&row);
                    } else {
                        // Auto-chain the remote handle lookup rather than render a
                        // SECOND "Look up" button (the reported confusing double
                        // lookup — which also duplicated the contact-actor-id-lookup
                        // id). The HandleResolved reply, or the clean "no Fauna user
                        // / not a Fauna server" error, lands on the normal channel
                        // and replaces this transient status row.
                        fauna_client.resolve_handle_on_remote(url, handle, Some(domain.as_str()));
                        let label =
                            gtk::Label::new(Some(&contacts::looking_up_handle(handle, domain)));
                        label.add_css_class("dim-label");
                        label.set_margin_top(8);
                        label.set_margin_bottom(8);
                        let row = gtk::ListBoxRow::new();
                        row.set_child(Some(&label));
                        row.set_selectable(false);
                        list.append(&row);
                    }
                    list.set_visible(true);
                } else {
                    let label = gtk::Label::new(Some(&errors::could_not_resolve_domain(domain)));
                    label.add_css_class("dim-label");
                    label.set_margin_top(8);
                    label.set_margin_bottom(8);
                    let row = gtk::ListBoxRow::new();
                    row.set_child(Some(&label));
                    row.set_selectable(false);
                    list.append(&row);
                    list.set_visible(true);
                }
            }

            DataMessage::KeyPackageCountLoaded { count } => {
                // Paint only — tui's shape (its count read never mints). This
                // count is fetched when the build-once settings shell mounts at
                // login, i.e. BEFORE the session's `__mls` replica restore, and a
                // mint then loses its private init keys to the restore's
                // provider swap while its packages stay published: after a
                // succession the replica holds the predecessor's provider, so
                // every invitation to the recovered account then found a
                // package no device could open. Login replenish is the
                // session's own, after the restore (`start_receive_loop`); the
                // Encryption page's refresh button is the manual one.
                crate::settings::encryption::update_key_count(*count);
            }

            DataMessage::MessagesLoaded {
                conversation_id,
                messages,
            } => {
                // Individual message loading is used by the conversation detail view.
                // The detail pane manages its own display; we just log receipt here.
                tracing::debug!(
                    "MessagesLoaded: conversation_id={}, {} messages",
                    conversation_id,
                    messages.len()
                );
            }

            DataMessage::InboxModeLoaded { mode } => {
                // Record the account's REAL mode and repaint the Privacy page's
                // radio group. Until 2026-08-05 this arm logged and dropped the
                // reply while the group painted a hard-coded "open", so Settings
                // → Privacy told every account its inbox was open to everyone. Enforcement is nest-side, so what
                // broke was the user's ability to see and trust the setting —
                // which on a privacy control is not cosmetic.
                crate::settings::apply_loaded_inbox_mode(mode);
                tracing::debug!("InboxModeLoaded: mode={mode}");
            }

            DataMessage::HandleChanged { new_handle } => {
                // User changed their handle via the settings account tab.
                // Update status view, state, and the settings cache so a
                // re-opened Settings → Account shows the new value.
                {
                    let mut s = state.borrow_mut();
                    s.handle = new_handle.clone();
                }
                crate::settings::set_handle(Some(new_handle.clone()));
                widgets.status_handle_row.set_subtitle(new_handle);
            }

            DataMessage::RecoveryStatusLoaded { result } => {
                crate::settings::apply_recovery_status(result.clone());
            }

            DataMessage::PendingActionsLoaded { result } => {
                crate::settings::apply_pending_actions(result.clone());
            }

            DataMessage::RecoveryKitMinted { result } => {
                crate::settings::apply_recovery_kit_minted(result.clone());
            }

            DataMessage::RecoveryKitRepaired { result } => {
                crate::settings::apply_recovery_repaired(result.clone());
            }

            DataMessage::RecoverySucceeded { outcome } => {
                crate::settings::apply_recovery_succeeded(outcome);
            }

            DataMessage::SweepRetried { result } => {
                crate::settings::apply_sweep_retried(result);
            }

            DataMessage::AftermathProgress(update) => {
                crate::settings::apply_aftermath_progress(update.clone());
            }

            DataMessage::SpamPreferencesLoaded { prefs } => {
                // Cache the viewer's own spam/phishing thresholds for content-policy
                // render enforcement (family-safety.md § Content policy — the every-user
                // own-threshold collapse that un-darks moderation.md § Categories &
                // enforcement item 1). `content_policy::verdict_for` composes these into
                // both the feed and conversations render, strictest-wins with any
                // guardian floor. Fetched at login (below, beside check_family_status)
                // so it is live before the first feed/thread render, and re-set on every
                // privacy-page save.
                crate::content_policy::set_spam_preferences(Some(prefs.clone()));
                // (The settings privacy-page widgets are built locally in
                // settings/privacy.rs and aren't reachable from here, so the slider
                // load-back stays a parity follow-up — tracked internally.)
                tracing::debug!(
                    "SpamPreferencesLoaded: spam={} phishing={}",
                    prefs.spam_threshold,
                    prefs.phishing_threshold
                );
            }

            DataMessage::UpdateAvailable { version, url } => {
                // The sign-in look paints the same Settings → General notice the
                // asked check does; the toast is its attention-getter.
                crate::settings::general::show_update_notice(version, url);
                let toast = adw::Toast::new(&format!(
                    "Fauna {}",
                    crate::i18n::strings::settings::general_page::update_available(version)
                ));
                toast.set_button_label(Some(common::DOWNLOAD));
                toast.set_timeout(0); // persistent
                let url_clone = url.clone();
                toast.connect_button_clicked(move |_| {
                    gtk::UriLauncher::new(&url_clone).launch(
                        gtk::Window::NONE,
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                });
                widgets.toast_overlay.add_toast(toast);
            }

            DataMessage::OpenUrl { url } => {
                gtk::UriLauncher::new(url).launch(
                    gtk::Window::NONE,
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }

            DataMessage::AdminStatusLoaded { is_admin } => {
                tracing::debug!("AdminStatusLoaded: is_admin={}", is_admin);
                if *is_admin {
                    // Show the Admin sidebar row.
                    views::sidebar::show_admin_sidebar_row(&widgets.sidebar_list_box);
                    // The admin auto-default (long-term-store.md § Multi-account
                    // evolution: "Default off; a client turns it on for its admin
                    // identity"). This nav gate — the one that reveals the Admin row
                    // — is linux's `am-i-admin = true` observation, and a switch
                    // rebuilds the session, so it re-fires on every switch. The
                    // registry never learns admin-ness itself; the client decides.
                    // Idempotent, and it never overrides an explicit user choice
                    // (`require_confirm_user_set` pins an OFF forever).
                    auto_enable_require_confirm_for_active_admin();
                    // Report the nest's public host-address (admin-gated, once per
                    // session) so ACME HTTP-01 gates on the strong resolve-check
                    // (domains-and-tls-bootstrap.md § Host-address acquisition). The
                    // linux twin of the native FFI / web reportHostAddress; the
                    // shared fn never publishes a private/LAN address. Fire-and-forget.
                    fauna_client.report_host_address();
                    // Fetch admin data.
                    fauna_client.fetch_admin_stats();
                    fauna_client.fetch_admin_users();
                    fauna_client.fetch_admin_server_status();
                    fauna_client.fetch_admin_tiers();
                    fauna_client.fetch_admin_membership_tiers();
                    fauna_client.fetch_own_membership_tier_names();
                    fauna_client.fetch_registration_mode();
                    fauna_client.fetch_nest_serving_port();
                    fauna_client.fetch_nest_region();
                    fauna_client.fetch_oauth_issuer_keys();
                    fauna_client.fetch_nest_os_maintenance();
                    fauna_client.fetch_admin_invite_codes();
                    fauna_client.fetch_admin_invite_requests();
                    fauna_client.fetch_local_domains();
                    fauna_client.fetch_pending_bridges();
                    fauna_client.fetch_custody_hosting();
                    fauna_client.fetch_dns_records();
                    fauna_client.fetch_admin_services();
                    fauna_client.fetch_forwarders();
                    fauna_client.fetch_admin_logs();
                }
            }

            DataMessage::FamilyStatusLoaded {
                supervised_by,
                ward_count,
                incoming_transfer_count,
                snapshot,
                usage_today_minutes,
                contact_requests,
                feed_requests,
            } => {
                // Clause 2 of the unfetched-policy ruling (family-safety.md
                // § Content policy): this read succeeded, so remember it. Only
                // a successful read reaches here — `family_status_loaded` maps
                // Err to no message at all — so clause 1 holds by construction.
                crate::supervision_snapshot::persist(snapshot);
                // The ward's own asks, for the refused-send surfaces — gated on
                // `supervised_by` for the same reason the content floor is: a
                // graduated account has no guardian to be waiting on.
                crate::ward_asks::set_from_status(
                    supervised_by.is_some(),
                    contact_requests.clone(),
                    feed_requests.clone(),
                );
                views::bridges::detail::repaint_source_asks();
                // Screen time (family-safety.md § Screen time): the supervised
                // viewer's own policy + guardian drive the global
                // `screen-time-lock`. Recorded on EVERY status read, so a
                // guardian's edit takes effect on the ward's next read; the
                // repaint below is what makes it visible without a navigation.
                crate::screen_lock::set_ward_screen_time(
                    snapshot.screen_time,
                    supervised_by.clone(),
                    *usage_today_minutes,
                );
                crate::screen_lock::refresh(
                    &widgets.screen_lock_overlay,
                    widgets.content_stack.visible_child_name().as_deref(),
                );
                // The budget half needs a heartbeat (§ Screen time: daily-budget
                // enforcement needs cross-device accounting). Armed on any status
                // read that finds a budget set, idempotently — the same shape as
                // the Guardian Notify flush below.
                if snapshot.screen_time.and_then(|p| p.daily_minutes).is_some() {
                    start_usage_heartbeat_poll(fauna_client);
                }
                // Render enforcement on both social surfaces (family-safety.md
                // § Content policy): the supervised viewer's own guardian content
                // floor drives whether the feed AND conversations collapse/block a
                // flagged item. Set before the next render (status is read
                // post-auth, ahead of feed/thread population); a rebuild after
                // login/nav/inject enforces it via `content_policy::verdict_for`.
                crate::content_policy::set_ward_content_policy(snapshot.content_policy);
                // Guardian Notify (family-safety.md § Guardian Notify): the ward's
                // client counts its guardian-floor enforcement events only while the
                // knob is on, and the flush tick (started here, idempotently) reports
                // the batch ≤ hourly. Started on any status read that finds the knob
                // on, so enabling it on reconnect also arms reporting.
                let notify_on = snapshot.content_notify;
                crate::content_policy::set_ward_content_notify(notify_on);
                if notify_on {
                    start_notify_flush_poll(fauna_client);
                }
                // `family-tab` appears when `fauna.family.status` returns ANY
                // relationship — guardian or supervised — OR a pending/incoming
                // transfer (family-safety.md § Graduation & transfer →
                // Visibility: a proposed guardian with no other family
                // relationship must still reach the prompt; an outgoing pending
                // proposal implies wards > 0, already covered).
                if *ward_count > 0 || supervised_by.is_some() || *incoming_transfer_count > 0 {
                    views::sidebar::show_family_sidebar_row(&widgets.sidebar_list_box);
                }
                // The global `supervised-indicator` is for the SUPERVISED side
                // only ("This account is supervised by X"), so a guardian who is
                // not themself supervised never sees it. Its label must carry the
                // guardian's handle.
                match supervised_by {
                    Some(guardian) => {
                        widgets
                            .supervised_indicator
                            .set_label(&family::supervised_indicator(guardian));
                        widgets.supervised_indicator.set_visible(true);
                    }
                    None => widgets.supervised_indicator.set_visible(false),
                }
            }

            DataMessage::AdminStatsLoaded { stats } => {
                views::admin::update_admin_stats(&widgets.admin_handles, stats);
            }

            DataMessage::AdminUsersLoaded {
                reply,
                picker_users,
            } => {
                views::admin::update_dashboard_users(&widgets.admin_handles, reply);
                views::admin::update_users_page(&widgets.admin_handles, reply, fauna_client);
                // Every admin actor picker draws its options from every account on
                // the nest, never this page (admin.md § 2 → *Which accounts a
                // picker offers*): cache the list, then re-render admin-dns and
                // refill both guardian pickers. A failed every-account read leaves
                // the pickers on the list they had.
                if let Some(users) = picker_users {
                    views::admin::set_admin_picker_users(
                        &widgets.admin_handles,
                        users.as_slice(),
                        fauna_client,
                    );
                }
            }

            DataMessage::AdminServerStatusLoaded { status } => {
                views::admin::update_admin_status(&widgets.admin_handles, status);
            }

            DataMessage::AdminTiersLoaded { reply } => {
                views::admin::update_tiers(&widgets.admin_handles, reply, fauna_client);
            }

            DataMessage::AdminUserUpdated => {
                // Refresh the list (at the current page) so the changed row
                // reflects its new tier / eviction state.
                fauna_client.fetch_admin_users_page(widgets.admin_handles.users_offset.get());
            }

            DataMessage::AdminTierUpdated => {
                // Refetch so the edited row re-renders from the persisted caps
                // (the page stays authoritative on the nest's stored definition).
                fauna_client.fetch_admin_tiers();
            }

            DataMessage::AdminMembershipTiersLoaded { reply } => {
                *widgets.admin_handles.last_membership_tiers.borrow_mut() = Some(reply.clone());
                views::admin::update_membership_tiers(&widgets.admin_handles, fauna_client);
            }

            DataMessage::OwnMembershipTierNamesLoaded { names } => {
                *widgets.admin_handles.own_membership_tier_names.borrow_mut() = names.clone();
                views::admin::update_membership_tiers(&widgets.admin_handles, fauna_client);
            }

            DataMessage::AdminMembershipTierUpdated => {
                // Refetch so the edited row re-renders from the persisted state
                // (the page stays authoritative on the nest's stored designation).
                fauna_client.fetch_admin_membership_tiers();
            }

            DataMessage::RegistrationModeLoaded {
                mode,
                max_free_users,
                age_verification_required,
            } => {
                views::admin::set_registration_mode(
                    &widgets.admin_handles,
                    mode.as_deref(),
                    *max_free_users,
                    *age_verification_required,
                );
            }

            DataMessage::RegistrationModeSaved => {
                // Re-seed the section from the persisted posture (the write
                // succeeded) — the re-read IS the confirmation.
                fauna_client.fetch_registration_mode();
            }

            DataMessage::NestServingPortLoaded { port, fronted } => {
                views::admin::set_nest_serving_port(&widgets.admin_handles, *port, *fronted);
            }

            DataMessage::NestServingPortSaved => {
                // Re-seed the entry from the persisted port (the write succeeded).
                fauna_client.fetch_nest_serving_port();
            }

            DataMessage::NestRegionLoaded { view } => {
                views::admin::set_nest_region(&widgets.admin_handles, view);
            }

            DataMessage::NestRegionSaved => {
                // Re-seed the section from the persisted declaration (the write
                // succeeded) — the re-read IS the confirmation.
                fauna_client.fetch_nest_region();
            }

            DataMessage::NestOsMaintenanceLoaded {
                security_updates_pending,
                reboot_pending,
            } => {
                views::admin::set_nest_os_maintenance(
                    &widgets.admin_handles,
                    *security_updates_pending,
                    *reboot_pending,
                );
            }

            DataMessage::NestHostRestartRequested => {
                // The flag is written; refetch so the indicator reflects it.
                fauna_client.fetch_nest_os_maintenance();
            }

            DataMessage::AdminServicesLoaded { reply } => {
                views::admin::update_services(&widgets.admin_handles, reply);
            }

            DataMessage::AdminLogsLoaded { reply } => {
                views::admin::update_admin_logs(&widgets.admin_handles, reply);
            }

            DataMessage::AdminServiceUpdated => {
                // Refetch so the toggle active state + status badge reflect the
                // applied flag (the nest echoes the write, but a refetch keeps the
                // page authoritative on the persisted intent).
                fauna_client.fetch_admin_services();
            }

            DataMessage::AdminInviteCodesLoaded { reply } => {
                views::admin::update_invite_codes(&widgets.admin_handles, reply, fauna_client);
            }

            DataMessage::AdminInviteCodeDeleted => {
                fauna_client.fetch_admin_invite_codes();
            }

            DataMessage::AdminLocalDomainsLoaded { snapshot } => {
                views::admin::set_local_domains_snapshot(
                    &widgets.admin_handles,
                    snapshot,
                    fauna_client,
                );
            }

            DataMessage::AdminPendingBridgesLoaded { snapshot } => {
                views::admin::update_pending_bridges(
                    &widgets.admin_handles,
                    snapshot,
                    fauna_client,
                );
            }

            DataMessage::AdminCustodyHostingLoaded { snapshot } => {
                views::admin::update_custody_hosting(
                    &widgets.admin_handles,
                    snapshot.clone(),
                    fauna_client,
                );
            }

            DataMessage::AdminForwardersLoaded { snapshot } => {
                views::admin::set_forwarders_snapshot(
                    &widgets.admin_handles,
                    snapshot,
                    fauna_client,
                );
            }

            DataMessage::AdminDnsRecordsLoaded { snapshot } => {
                views::admin::set_dns_snapshot(&widgets.admin_handles, snapshot, fauna_client);
            }

            DataMessage::AdminInviteCodeCreated { reply } => {
                // Surface the minted token copyable, then refetch the list so
                // the new code appears as a row.
                views::admin::show_minted_invite_code(&widgets.admin_handles, reply);
                fauna_client.fetch_admin_invite_codes();
            }

            DataMessage::AdminInviteRequestsLoaded { reply } => {
                views::admin::update_invite_requests(&widgets.admin_handles, reply, fauna_client);
            }

            DataMessage::AdminInviteRequestDecided => {
                // Refresh the list so the row is removed (approved) or the
                // status updated (denied).
                fauna_client.fetch_admin_invite_requests();
                // An *approve* ADMITS a user, so the Users section is stale too —
                // and with it both guardian pickers, whose options are exactly the
                // non-suspended users (`refresh_guardian_pickers`, fired from the
                // `AdminUsersLoaded` arm). Without this, admitting a guardian and
                // then admitting a ward *under* them in the same session would
                // find the fresh guardian missing from the picker.
                fauna_client.fetch_admin_users_page(widgets.admin_handles.users_offset.get());
            }

            DataMessage::FactoryResetComplete {
                claim_code,
                nest_url,
                secret_hex,
                handle,
            } => {
                // The nest replied with the post-reset claim code and is now
                // restarting into the wipe. Tear down the authenticated session
                // (keeping credentials) and re-seed onboarding at claim-code with
                // the code pre-filled. The handler is registered in main.rs.
                tracing::info!("FactoryResetComplete: re-seeding onboarding at claim-code");
                crate::settings::trigger_factory_reset(
                    claim_code.clone(),
                    nest_url.clone(),
                    secret_hex.clone(),
                    handle.clone(),
                );
            }
            DataMessage::FactoryResetFailed { error } => {
                tracing::error!("FactoryResetFailed: {error}");
                show_toast(
                    widgets,
                    crate::i18n::strings::admin::settings_page::FACTORY_RESET_FAILED,
                );
            }
            DataMessage::SeedRotateRosterLoaded { state } => {
                views::admin::set_seed_rotate_roster(&widgets.admin_handles, state.clone());
            }
            DataMessage::SeedRotated { status } => {
                views::admin::set_seed_rotate_status(&widgets.admin_handles, status.clone());
            }
            DataMessage::OauthKeysLoaded { keys } => {
                views::admin::set_oauth_keys(&widgets.admin_handles, keys.clone());
            }
            DataMessage::OauthDone { status, keys } => {
                views::admin::set_oauth_done(&widgets.admin_handles, status.clone(), keys.clone());
            }
            DataMessage::TakedownSubmitted { status } => {
                views::admin::set_takedown_status(&widgets.admin_handles, status.clone());
            }
        },

        UiMessage::Action(action_msg) => match action_msg {
            ActionResult::Success { context } => {
                // ICS import/export show custom toasts below; skip the generic one.
                if !context.starts_with("ics_exported:")
                    && !context.starts_with("ics_imported:")
                    && !context.starts_with("data_exported:")
                {
                    show_toast(widgets, &common::success_detail(context));
                }
                // Both delayed verbs this page hosts feed the pending-actions
                // section (`settings.md` § Pending actions) rather than
                // applying immediately — re-list so the standing section
                // shows the freshly scheduled row. Deliberately does NOT feed
                // the new handle into any cache: the change has not applied.
                if context == "handle_changed" || context == "account_deleted" {
                    fauna_client.fetch_pending_actions();
                }
                // "message_sent" no longer triggers a refresh here: conversation
                // threads render off the shared `ConversationsManager` snapshot,
                // whose observer refreshes the list (and the tray badge) on send.
                // Group-page success contexts ("group_created" /
                // "group_invited" / "group_reaction" / "group_message_sent")
                // disappeared with the standalone Groups page. Group threads
                // now live on the unified conversations page; their refresh
                // path is the snapshot observer.
                // A confirm changes the row's status the same way an accept
                // makes the row: without its re-read the roster kept reading
                // "Accepted" until a push or a nav edge happened by (caught by
                // `test_contacts_edge_actions.py`'s confirm witness, 2026-09-22).
                if context == "knock_accepted"
                    || context == "knock_blocked"
                    || context == "knock_dismissed"
                    || context == "contact_confirmed"
                {
                    fauna_client.fetch_knocks();
                    fauna_client.fetch_contacts();
                }
                if context == "calendar_created" {
                    fauna_client.fetch_calendars();
                }
                // A landed guardian ask (contacts Find User, bridges) retires the
                // refusal the page put on `error-message` — the send is no longer
                // a dead end, and the pair now reads pending (`family-safety.md`
                // § Child-initiated contact requests, § Feed-source approvals).
                if context == "contact_requested" || context == "feed_source_requested" {
                    clear_error_message(widgets);
                }
                if context == "event_created" || context == "event_deleted" {
                    // Re-fetch calendars (which triggers event loading).
                    fauna_client.fetch_calendars();
                }
                if context == "event_created" {
                    // The event exists now, so it is no longer a draft: empty
                    // the `"events"` rail and tick it, or relaunching would
                    // restore the text of an event already on the calendar
                    // (`events.md` § Persistence). Deliberately here on the
                    // SUCCESS result rather than at the submit click — a create
                    // that failed leaves the user's text where they can still
                    // see and retry it.
                    crate::views::events::drafts::clear();
                }
                // `rsvp_event` pushes the updated roster straight to the detail
                // panel via `EventAttendeesLoaded` (it computes the new roster
                // during its read-mutate-rewrite), so no post-RSVP attendee
                // re-fetch is needed here.
                if let Some(path) = context.strip_prefix("ics_exported:") {
                    show_toast(widgets, &events::calendar_exported(path));
                }
                if let Some(summary) = context.strip_prefix("ics_imported:") {
                    show_toast(widgets, summary);
                    // Re-fetch calendars to show newly imported events.
                    fauna_client.fetch_calendars();
                }
                if let Some(path) = context.strip_prefix("data_exported:") {
                    show_toast(
                        widgets,
                        &crate::i18n::strings::status::data_export::exported_to(path),
                    );
                }
                // The snapshot mutations no longer report through this channel:
                // every one of them is a `BackupsMachine` gesture that ends in
                // the machine's own re-read, so there is nothing to re-fetch by
                // hand and no dropdown to interrogate for the set to re-fetch
                // *for*. The friction-bar modal likewise closes from the render
                // pass, when the row it targets actually leaves the list.
                // Feed CRUD + bridge-feed subscribe/unsubscribe now go through
                // the shared `FeedManager`, which refreshes the relevant list +
                // notifies its observer itself — no app.rs refresh needed.
                if context == "bridge_linked"
                    || context == "bridge_unlinked"
                    || context == "bridge_settings_updated"
                    || context == "bridge_follow_added"
                    || context == "bridge_follow_removed"
                {
                    fauna_client.fetch_bridges();
                }
                if context == "bluesky_linked" {
                    // Start periodic notification polling now that a bridge
                    // is linked. The endpoint is unified; the trigger is
                    // still bluesky-link-only as a follow-up.
                    start_notification_poll(fauna_client);
                }
                // `post_interaction` needs no refresh here: `interact_with_post`
                // now goes through `FeedManager::interact`, which folds the
                // nest's post-act counters into the loaded window and notifies —
                // so the tapped count moves without the full feed re-query this
                // arm used to do (which re-ranked the window under the user's
                // finger, since a like moves `content_meta.score`).
            }
            ActionResult::Failed { context, error } => {
                let fail_msg = format!("{}: {}", context, error);
                show_toast(widgets, &fail_msg);
                // A restore that failed is not still running: hand
                // `restore-progress` back to its idle prompt so the banner is
                // the only thing reporting the failure. Leaving "Restoring…"
                // standing would claim work that stopped.
                if context == "message_kind_restored" {
                    widgets
                        .backups_restore
                        .progress
                        .set_text(crate::i18n::strings::backups::RESTORE_PROGRESS_IDLE);
                }
                // The consolidated `admin-users` hub owns a dedicated error
                // surface (`admin-users-action-error`); its three sections' action
                // failures route there, not the app-wide `error-message` banner
                // (admin.md § Errors — "Users (all three sections):
                // admin-users-action-error"). Everything else still uses the
                // global banner.
                if views::admin::is_users_hub_context(context) {
                    views::admin::set_users_action_error(&widgets.admin_handles, Some(&fail_msg));
                } else {
                    set_error_message(widgets, &fail_msg);
                }
            }
            ActionResult::FailedLocalized { message } => {
                // Already complete — no prefix to append. (The admin-users hub
                // routing above keys on a raw RPC `context`, which a resolved
                // sentence never is, so these always take the global banner.)
                show_toast(widgets, message);
                set_error_message(widgets, message);
            }
            ActionResult::ApiFailed {
                endpoint,
                status,
                message,
            } => {
                tracing::error!("API error {} {}: {}", status, endpoint, message);
                let msg = errors::api_error(&status.to_string(), message);
                show_toast(widgets, &msg);
                set_error_message(widgets, &msg);
            }
        },

        UiMessage::Realtime(ws_event) => {
            handle_ws_event(ws_event, state, widgets, fauna_client);
        }

        UiMessage::Noop => {}
    }
}

// ---------------------------------------------------------------------------
// Notification polling
// ---------------------------------------------------------------------------

/// The admin auto-default (`long-term-store.md` § Multi-account evolution:
/// *"Default off; a client turns it on for its admin identity"*): called at every
/// `am-i-admin = true` observation — linux's is the nav gate that reveals the Admin
/// sidebar row — to flip the ACTIVE account's `require_confirm_to_activate` on,
/// unless the user has ever touched that account's toggle (an explicit OFF sticks;
/// the registry enforces that via `require_confirm_user_set`, this is just the
/// client-side observation hook, since the registry never learns admin-ness).
///
/// Best-effort: a failed auto-default must never break the admin gate.
/// The apple twin is `FaunaAccounts.autoEnableRequireConfirmForActiveAdmin()`.
fn auto_enable_require_confirm_for_active_admin() {
    let registry = crate::account_registry();
    let Some(actor_id) = registry.active() else {
        return;
    };
    match registry.auto_enable_require_confirm(&actor_id) {
        Ok(true) => tracing::info!(
            "[admin-auto-default] require_confirm_to_activate auto-enabled for {actor_id}"
        ),
        Ok(false) => {}
        Err(e) => tracing::warn!(
            "[admin-auto-default] auto_enable_require_confirm({actor_id}) failed: {e:#}"
        ),
    }
}

/// Arm-slot for this actor's notification poll (`crate::actor_scope`).
static NOTIF_POLL_ARMED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Start a periodic 60-second timer that fetches unified notifications.
///
/// The first fetch fires immediately. Subsequent fetches run every 60 seconds.
/// Idempotent **within one actor** — calling it more than once per sign-in is a
/// no-op, while an actor change re-arms it against the incoming actor's client
/// (`crate::actor_scope`).
fn start_notification_poll(fauna_client: &Rc<FaunaClient>) {
    let Some(generation) = crate::actor_scope::claim_poll_slot(&NOTIF_POLL_ARMED) else {
        return; // this actor is already polling
    };

    tracing::info!("Starting notification poll (60 s interval)");

    // Fire the first fetch immediately.
    fauna_client.fetch_notifications();

    // Set up the periodic timer.
    let client = Rc::clone(fauna_client);
    glib::timeout_add_local(std::time::Duration::from_secs(60), move || {
        if !crate::actor_scope::poll_still_current(generation) {
            return glib::ControlFlow::Break;
        }
        client.fetch_notifications();
        glib::ControlFlow::Continue
    });
}

/// Arm-slot for this actor's Guardian Notify flush timer (`crate::actor_scope`).
static NOTIFY_FLUSH_ARMED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Start the Guardian Notify flush tick (`family-safety.md` § Guardian Notify): a
/// short periodic check that reports the ward's batched per-category enforcement
/// counts. The actual `fauna.family.notify_report` fires at most hourly (the
/// accumulator's own gate); this 5-second cadence only bounds the *latency* of the
/// first report after the ward flags something, so the check is cheap and the send
/// is rare. Started post-auth only once a supervised ward's `content_notify` knob is
/// on; idempotent within one actor.
///
/// The generation check is load-bearing beyond tidiness here: a Notify report
/// carries no identity of its own, so a tick left over from the outgoing ward
/// would drain *their* counts through whichever client is signed in now
/// (`crate::actor_scope`; the accumulator itself is dropped by
/// `content_policy::clear_for_identity_change`).
///
/// Under e2e (`crate::e2e_mode_enabled()`) the real tick is never armed at
/// all — the same "hasAgent" idiom web's `familyNotify.ts::ensureTimer()`
/// uses. A live 5s tick racing a multi-step test (WS-RPC round trips, page
/// loads) could flush for real, under the wrong moment, before the test
/// reaches its own switch step — exactly the wall-clock race testing.md
/// convention 14 forbids. Tests drive checks explicitly via the
/// `family_notify_check_now` e2e command instead.
fn start_notify_flush_poll(fauna_client: &Rc<FaunaClient>) {
    if crate::e2e_mode_enabled() {
        return;
    }
    let Some(generation) = crate::actor_scope::claim_poll_slot(&NOTIFY_FLUSH_ARMED) else {
        return; // this actor is already flushing
    };
    let client = Rc::clone(fauna_client);
    glib::timeout_add_local(std::time::Duration::from_secs(5), move || {
        if !crate::actor_scope::poll_still_current(generation) {
            return glib::ControlFlow::Break;
        }
        client.flush_notify_report();
        glib::ControlFlow::Continue
    });
}

/// Arm-slot for this actor's region relay refresh (`crate::actor_scope`).
static REGION_REFRESH_ARMED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The region relay's cadence tick (`region-blocking.md` § How an app obtains
/// its region's policy): every minute, ask again when the shared
/// `REFRESH_INTERVAL_SECS` is due (`crate::region::chain_to_refresh` decides;
/// this timer only bounds the resolution). Idempotent per actor, like the
/// Notify flush poll above.
fn start_region_refresh_poll(fauna_client: &Rc<FaunaClient>) {
    let Some(generation) = crate::actor_scope::claim_poll_slot(&REGION_REFRESH_ARMED) else {
        return;
    };
    let client = Rc::clone(fauna_client);
    glib::timeout_add_local(std::time::Duration::from_secs(60), move || {
        if !crate::actor_scope::poll_still_current(generation) {
            return glib::ControlFlow::Break;
        }
        client.fetch_region(false);
        glib::ControlFlow::Continue
    });
}

/// Arm-slot for this actor's screen-time heartbeat (`crate::actor_scope`).
static USAGE_HEARTBEAT_ARMED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Start the screen-time usage heartbeat (`family-safety.md` § Screen time —
/// *"Daily-budget enforcement needs cross-device accounting"*). Armed post-auth
/// only once a supervised ward's guardian has set a daily budget; idempotent.
///
/// The tick runs every minute, which is what
/// [`fauna_core::screen_time::MAX_ACCRUAL_STEP_SECS`] requires of a caller — the
/// engine credits at most one step per call, so a slower tick would under-count
/// real use. *Whether* a report actually goes out is the shared engine's own
/// cadence decision ([`fauna_core::screen_time::USAGE_REPORT_INTERVAL_SECS`]),
/// so this timer only bounds the resolution, exactly as the Notify flush poll
/// above bounds that batch's latency.
///
/// Foreground is the window's own focus (`is_active`) — the honest reading of
/// "the child is looking at this". The lock is repainted after every reply, so
/// crossing the budget shows up without waiting for the separate lock tick.
fn start_usage_heartbeat_poll(fauna_client: &Rc<FaunaClient>) {
    // Idempotent within one actor; an actor change re-arms it, so the incoming
    // ward's own screen-time budget is accounted rather than the outgoing
    // ward's continuing to accrue (`crate::actor_scope`).
    let Some(generation) = crate::actor_scope::claim_poll_slot(&USAGE_HEARTBEAT_ARMED) else {
        return; // this actor's heartbeat is already running
    };
    // Fire once immediately: the engine's first report is eager, and it is a
    // zero-minute READ that fetches the day's cross-device total. Waiting a
    // full tick for it would leave a ward who is already over budget on another
    // device unlocked for that minute.
    fauna_client.flush_usage_report(crate::screen_lock::window_focused());
    let client = Rc::clone(fauna_client);
    glib::timeout_add_local(std::time::Duration::from_secs(60), move || {
        if !crate::actor_scope::poll_still_current(generation) {
            return glib::ControlFlow::Break;
        }
        client.flush_usage_report(crate::screen_lock::window_focused());
        glib::ControlFlow::Continue
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Show an adw::Toast in the main window.
///
/// A toast is "displayed to the user" (observability.md § What must be logged,
/// category 1), so the funnel logs it as it shows — at `info`, since toasts here
/// carry status/success ("Saved", "Backup complete"). One log call covers every
/// `show_toast` caller.
fn show_toast(widgets: &WidgetHandles, message: &str) {
    tracing::info!("{message}");
    let toast = adw::Toast::new(message);
    toast.set_timeout(5);
    widgets.toast_overlay.add_toast(toast);
}

/// Set the persistent error banner to the given message and make it visible.
/// Call `clear_error_message` to hide it again.
///
/// The banner is "displayed to the user" (observability.md § category 1); the
/// funnel logs it at `error` as it shows, so it also lands in Settings → Logs.
pub fn set_error_message(widgets: &WidgetHandles, message: &str) {
    tracing::error!("{message}");
    crate::settings::render_error_label(&widgets.error_label, Some(message));
}

/// Hide the persistent error banner and clear its text.
#[allow(dead_code)]
pub fn clear_error_message(widgets: &WidgetHandles) {
    crate::settings::render_error_label(&widgets.error_label, None);
}

/// Update the global top-of-sidebar connection-status indicator
/// (`connection-status`) from a WS event. Text is the user-facing signal;
/// the icon mirrors it. `Push` events don't change connection state. The
/// state → label decision is the shared `fauna_core::format::connection_state_label`
/// (transport.md § Connection-status indicator) — only the icon choice is
/// linux's own (presentation, not part of the shared decision).
fn update_connection_indicator(widgets: &Rc<WidgetHandles>, ws_event: &WsEvent) {
    let (state_word, icon) = match ws_event {
        WsEvent::Connected => ("connected", "network-idle-symbolic"),
        WsEvent::Connecting => ("connecting", "network-transmit-receive-symbolic"),
        WsEvent::Disconnected => ("disconnected", "network-offline-symbolic"),
        // A settled failure keeps the offline icon but says so in words — the
        // whole point is that the user can tell it from a passing blip.
        WsEvent::Unreachable => ("unreachable", "network-error-symbolic"),
        // Reconnected rides alongside Connected (which already set the indicator);
        // it only triggers the re-hydrate, not an indicator change.
        WsEvent::Reconnected => return,
        WsEvent::Push(_) => return,
    };
    // Every report is counted, repeats included — the offline gate below
    // early-returns on a repeated word, which is exactly the report the
    // stickiness proof needs (`fauna_e2e_agent::CONNECTION_REPORTS_KEY`).
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    crate::automation::observables::observe_connection_report(state_word);
    widgets
        .connection_status_label
        .set_text(&crate::i18n::connection_state_label(state_word));
    widgets.connection_status_icon.set_icon_name(Some(icon));
    // The offline affordance gate reads the SAME word, from here, for the same
    // reason the label does — so what the indicator says and what the gate
    // greys can never disagree (`account-data-plane.md` § The offline-mutation
    // contract → *How a surface asks*; tui states the identical guarantee in
    // `App::connection_state_word`).
    crate::offline_gate::set_connection_state(state_word);
}

/// Update the global `sync-agent-status` sidebar indicator (health of the
/// LOCAL fauna-sync-agent process — distinct from `connection-status`, the
/// nest WS-RPC link) from a `GetServiceStatus` poll attempt plus the
/// provisioner's own content-key retry (`keys_pending`).
/// `sync-agent.md` § Local agent health.
fn update_sync_agent_status_indicator(
    widgets: &Rc<WidgetHandles>,
    result: Result<fauna_ipc::sync::ServiceStatusInfo, fauna_client_sync::agent::AgentControlError>,
) {
    use crate::i18n::strings::status::sync_agent as strings;
    use fauna_client_sync::agent::AgentHealthState;

    let state = fauna_client_sync::agent::agent_health_state(&result, env!("CARGO_PKG_VERSION"));
    let (text, icon) = match state {
        AgentHealthState::Running => (strings::RUNNING, "emblem-synchronizing-symbolic"),
        AgentHealthState::RestartPending => {
            (strings::RESTART_PENDING, "software-update-urgent-symbolic")
        }
        AgentHealthState::KeysPending => (strings::KEYS_PENDING, "dialog-password-symbolic"),
        AgentHealthState::NotEnrolled => (strings::NOT_ENROLLED, "system-users-symbolic"),
        AgentHealthState::NotRunning => (strings::NOT_RUNNING, "dialog-warning-symbolic"),
    };
    widgets.sync_agent_status_label.set_text(text);
    widgets.sync_agent_status_icon.set_icon_name(Some(icon));

    match &result {
        Ok(s) => {
            widgets.sync_agent_status_version_label.set_text(&s.version);
            widgets
                .sync_agent_status_uptime_label
                .set_text(&crate::i18n::duration_secs(s.uptime_secs));
        }
        Err(_) => {
            widgets.sync_agent_status_version_label.set_text("");
            widgets.sync_agent_status_uptime_label.set_text("");
        }
    }
}

/// Arm-slot for this actor's sync-agent-status poll (`crate::actor_scope`).
static SYNC_AGENT_STATUS_POLL_ARMED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Start a periodic poll of the local sync-agent's `GetServiceStatus`
/// (`sync-agent.md` § Local agent health). The agent has no health-push
/// channel (unlike per-file sync completion, which does — `spawn_event_listener`),
/// so polling is how `sync-agent-status` stays current. The first poll fires
/// immediately; idempotent within one actor — a second call is a no-op.
///
/// The tick captures **this window's** `WidgetHandles`, and an actor change
/// destroys the window it painted, so without the generation check the outgoing
/// actor's tick would keep writing status into a torn-down widget tree while the
/// incoming actor's window never got a poll of its own (`crate::actor_scope`).
fn start_sync_agent_status_poll(widgets: &Rc<WidgetHandles>) {
    let Some(generation) = crate::actor_scope::claim_poll_slot(&SYNC_AGENT_STATUS_POLL_ARMED)
    else {
        return; // this actor's window is already polling
    };

    let poll = {
        let widgets = Rc::clone(widgets);
        move || {
            let Some((provisioner, rt)) = crate::sync_agent::provisioner_and_runtime() else {
                return;
            };
            let widgets = Rc::clone(&widgets);
            crate::async_helper::spawn_with_snapshot(
                &rt,
                move || async move { provisioner.get_service_status().await },
                move |result| {
                    // The same reply says whether this host can serve an
                    // on-demand binding — what every bound row's
                    // `folder-location-mode-toggle` reads.
                    if let Ok(status) = &result {
                        crate::sync_agent::fold_service_status(status);
                    }
                    update_sync_agent_status_indicator(&widgets, result)
                },
            );
            // The mass-delete floor's per-set hold rides the same tick: it is
            // derived inside the agent on its own rescan cadence, so it needs a
            // watch rather than a reconcile hook (`sync_agent::refresh_engine_holds`).
            crate::sync_agent::refresh_engine_holds();
        }
    };

    poll();
    glib::timeout_add_local(std::time::Duration::from_secs(10), move || {
        if !crate::actor_scope::poll_still_current(generation) {
            return glib::ControlFlow::Break;
        }
        poll();
        glib::ControlFlow::Continue
    });
}

/// Re-read whatever a push (or a reconnect) has made stale.
///
/// The *decision* — which surfaces those are — is not made here: it comes from
/// `fauna_protocol`'s [`StaleSurfaces`], the one exhaustive match all seven apps
/// share. This function only says how linux serves each surface. Before the seam
/// existed both halves lived inline in [`handle_ws_event`], with tui's twin
/// carrying a doc comment asking a human to keep the two in step — and linux's
/// two recovery paths had silently drifted out of it, sweeping neither Media nor
/// the ATProto consent list.
///
/// Side effects stay at the call site. A desktop toast, the sync engine's
/// `pull_set_now` nudge and the payload-parameterized per-folder device fetch are
/// per-app or per-payload; what is *stale* follows from the wire kind alone.
fn apply_stale(stale: &StaleSurfaces, widgets: &Rc<WidgetHandles>, fauna_client: &Rc<FaunaClient>) {
    if stale.feed {
        // The feed has NO poll backstop — without this a post that arrived
        // during a socket gap stays invisible until a manual refresh — so
        // re-list feeds + bridge feeds and reload the current selection through
        // the `FeedManager`.
        if let Some(m) = crate::feed::host::manager() {
            let rt = fauna_client.runtime_handle();
            rt.spawn(async move {
                m.refresh_feeds().await;
                m.refresh_bridge_feeds().await;
                m.refresh_available_bridges().await;
                // Preserving Trending, which is not a `selected_feed` id
                // (`trending.md` § The Trending feed) — re-selecting
                // `selected_feed` here would drop a Trending viewer into Local
                // on every reconnect.
                m.refresh_current_feed().await;
            });
        }
    }
    if stale.notifications {
        fauna_client.fetch_notifications();
    }
    if stale.knocks {
        fauna_client.fetch_knocks();
    }
    if stale.contacts {
        fauna_client.fetch_contacts();
        fauna_client.fetch_member_reviews();
    }
    if stale.account {
        // Quota, tier, handle.
        fauna_client.fetch_account();
    }
    if stale.atproto {
        // Nudge the AT Protocol settings page's own machine to re-list its pending
        // consent cards — a no-op if that page was never built this session
        // (`notify_atproto_rehydrate` is a no-op with no handler registered).
        crate::settings::notify_atproto_rehydrate();
    }
    if stale.events {
        // Re-list calendars — the Events page's fetch path, which cascades into
        // per-calendar event fetches; the visible-page poll remains the
        // lossy-push backstop.
        fauna_client.fetch_calendars();
    }
    // Page-gated, unlike every surface above — see [`StaleSurfaces::media`]. The
    // gate is GTK's own map state, i.e. literally the fact `connect_map` fires
    // on, so the push path and the become-visible path cannot drift apart.
    if stale.media && views::media::media_page_is_visible(&widgets.media_page) {
        let machine = std::sync::Arc::clone(&widgets.media_machine);
        let backup_key = fauna_core::crypto::BackupKey::derive(&fauna_client.secret_bytes())
            .to_bytes()
            .to_vec();
        fauna_client
            .runtime_handle()
            .spawn(async move { machine.refresh(Some(backup_key)).await });
    }
    // Page-gated like media — see [`StaleSurfaces::address_book`]: a contacts
    // app's first sync is one push per card, so only a showing Address Book
    // re-reads. The re-list keeps the open book open and re-reads its cards
    // (`address_book::update_book_list`); a late reply for a book the user has
    // since left is dropped (`update_card_list`).
    if stale.address_book && widgets.address_book.is_showing() {
        fauna_client.fetch_addressbooks();
    }
    if stale.family {
        // The ward's supervision read — the same `fauna.family.status` the
        // post-auth arm fires, so a guardian's edit binds at WS reconnect and
        // not only at the ward's next login (family-client-enforcement.md
        // § Content policy, clause 1: "refresh fires at cold launch and on WS
        // reconnect"). Safe to re-fire because `client.rs::family_status_loaded`
        // sends NO message on a failed read — the sweep can only move
        // enforcement state on a successful reply. Until 2026-09-13 this arm did
        // not exist and linux re-read only at login.
        fauna_client.check_family_status();
    }
}

/// Handle a WebSocket event — update connection state, show notifications,
/// and trigger re-fetches as appropriate.
fn handle_ws_event(
    ws_event: &WsEvent,
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<WidgetHandles>,
    fauna_client: &Rc<FaunaClient>,
) {
    // Always reflect the live state in the global sidebar indicator.
    update_connection_indicator(widgets, ws_event);

    match ws_event {
        WsEvent::Connected => {
            {
                let mut s = state.borrow_mut();
                s.connected = true;
            }
            *crate::tray::tray_state().connected.lock().unwrap() = true;
            widgets
                .status_connection_row
                .set_subtitle(common::CONNECTED_REALTIME);
        }
        WsEvent::Connecting => {
            {
                let mut s = state.borrow_mut();
                s.connected = false;
            }
            *crate::tray::tray_state().connected.lock().unwrap() = false;
            widgets
                .status_connection_row
                .set_subtitle(common::CONNECTING);
        }
        WsEvent::Disconnected => {
            {
                let mut s = state.borrow_mut();
                s.connected = false;
            }
            *crate::tray::tray_state().connected.lock().unwrap() = false;
            widgets
                .status_connection_row
                .set_subtitle(common::DISCONNECTED);
        }
        WsEvent::Unreachable => {
            {
                let mut s = state.borrow_mut();
                s.connected = false;
            }
            *crate::tray::tray_state().connected.lock().unwrap() = false;
            widgets
                .status_connection_row
                .set_subtitle(common::CANNOT_CONNECT);
        }
        WsEvent::Reconnected => {
            // Re-pull the visible snapshot surfaces after a reconnect: the
            // transport.md § Push events contract that observers re-pull through
            // their snapshot-refresh path (push `seq` reset to 0; the gap may
            // have dropped any push — so *everything* a push feeds is stale,
            // plus the feed, which nothing else recovers).
            //
            // Which surfaces those are is the shared seam's answer, not this
            // arm's. Hand-listing them here is exactly how this arm came to be
            // missing `fetch_calendars` once (patched 2026-08-22 after tui's
            // twin was written), and Media and Bluesky until this seam landed.
            apply_stale(&StaleSurfaces::on_reconnect(), widgets, fauna_client);
        }
        WsEvent::Push(push_event) => {
            use fauna_client::PushEvent;
            // What this kind made stale, straight from the shared seam — the
            // same answer tui and every other app gets. The match below is now
            // only for what is genuinely linux's or genuinely this payload's:
            // desktop toasts, the sync engine nudge, the per-folder roster.
            apply_stale(&push_event.invalidates(), widgets, fauna_client);
            match &**push_event {
                PushEvent::Knock(p) => {
                    crate::notifications::notify_knock(&crate::i18n::knock_push_text(p));
                }
                PushEvent::AccountUpdated(_) => {
                    // Nothing beyond the account re-read above.
                }
                PushEvent::Notification(p) => {
                    crate::notifications::notify_unified(
                        &p.notif_type,
                        &crate::i18n::notification_push_text(p),
                    );
                }
                PushEvent::PeerWake(_) => {
                    // P2P wake — no UI re-fetch. The hook lands when the p2p
                    // service grows a WS push consumer.
                }
                PushEvent::CalendarChanged(_) => {
                    // Nothing beyond the calendar re-list above.
                }
                PushEvent::AddressBookChanged(_) => {
                    // The carddav twin of CalendarChanged: a durable card or
                    // book write landed in one of this actor's address books.
                    // Nothing beyond `apply_stale`'s page-gated Address Book
                    // re-read off `StaleSurfaces::address_book` (transport.md
                    // § Push events). The index-side consumer (the contacts
                    // reconcile walk) is owned by the shared receive loop, not
                    // this dispatch.
                }
                PushEvent::ChannelMessage(_) => {
                    // MLS channel data — the conversations snapshot observer
                    // drives in-pane refresh. No standalone fetch from here.
                }
                PushEvent::Welcome(_) => {
                    // MLS Welcome handling lives in the conversations inbound task
                    // (`conversations/conv_backend.rs`), which holds its own
                    // `subscribe_kind("fauna.conversations.welcome.received")` and
                    // calls `ingest_welcome` (join the shared engine + materialize
                    // and bind the conversations thread). Joining here too would
                    // double-spend the init key on the one shared engine, so this
                    // central dispatch is a no-op (mirrors ChannelMessage /
                    // MailReceived, whose subscriptions also live in that task).
                }
                PushEvent::InboxItem(_) => {
                    // Best-effort prompt that the durable fauna-native inbox
                    // delivery queue has a new item. linux no longer drains
                    // that queue (the legacy `fetch_inbox` HTTP path was
                    // removed); the durable-inbox *delivery* feature — applying
                    // queued contact-requests / Welcomes / security-notices —
                    // is dormant fleet-wide and its cross-app wiring is owned
                    // by the WS-RPC-everywhere lead (`api-layers.md` § inbox).
                    // No-op until that lands.
                }
                PushEvent::ResyncRequired(p) => {
                    // The sweep itself is the seam's answer above: every
                    // surface a *dropped* push could have staled, which is
                    // every one but the feed. Only the log is linux's.
                    tracing::warn!(
                        "ws-rpc: nest dropped {} push event(s); resyncing",
                        p.dropped_count
                    );
                }
                PushEvent::SegmentsChanged(_) => {
                    // Segment-store-level event (Plan 5 mail-segment backup).
                    // Consumed by the backup-driver slice when it lands; the
                    // UI client surfaces a "backup health" indicator from
                    // the same data. No-op here until that slice arrives.
                }
                PushEvent::MailReceived(_) => {
                    // Per-record mail arrival — the shared receive loop holds
                    // its own `subscribe_kind("fauna.mail.received")` and
                    // fetches on it (`ConversationsSession::start_receive_loop`).
                    // No standalone fetch from this central dispatch (mirrors
                    // ChannelMessage).
                }
                PushEvent::PushNotification(_) => {
                    // A `ws-device` push row's banner (`apps/common.md` § Push
                    // Notifications → *Transports*). The sync agent posts it
                    // while no app is attached; an open app owns the machine's
                    // banners under its own focus rule and ignores it.
                }
                PushEvent::SyncChunkWanted(_) => {
                    // Relay serving's ask (`file-sync.md` § Relay serving) is
                    // for the connection that announced the folder — the
                    // serving engine's, never this UI's, which announces
                    // nothing and so is never asked.
                }
                PushEvent::MailFlagsChanged(_) => {
                    // Mail read-state wake (`mail-app-surface.md` § Read state).
                    // The consumer is the shared conversations mail rail, which
                    // will hold its own subscription and answer with
                    // `fauna.email.inbox.flag_changes` — the app half is not
                    // built yet. No-op here, like MailReceived.
                }
                PushEvent::SyncChanged(p) => {
                    // A sync record landed in a shared/synced set we participate
                    // in (file-sync.md § Remote-change nudge). Nudge the sync
                    // agent's resident engine for that set to pull now, off its
                    // rescan cadence — so a collaborator's / second device's save
                    // materializes in seconds instead of a whole rescan interval.
                    // Best-effort: an absent engine or a pending pull is a no-op
                    // agent-side, and the tick is the backstop.
                    crate::sync_agent::pull_set_now(
                        p.folder.clone(),
                        p.folder_hash.as_ref().map(|h| h.to_vec()),
                    );
                    // No account-plane arm here: the store runtime reads the
                    // session's push stream itself (`with_session_wakes` in
                    // `account_runtime.rs`; `account-data-plane.md` § The
                    // client-side lifecycle, the pump bullet's wake source
                    // (1)), so a scope-tagged nudge wakes its pump with no app glue.
                    // Per-set device-activity LIVE UPDATE (file-sync.md §
                    // Implementation status today) — the whole point of the
                    // feature: the expanded row's `folder-device-activity-item`/
                    // -count picks up a collaborator's change with no manual
                    // reload, mirroring web's `if (expandedFs) void
                    // loadDeviceActivity(...)`. Gated on the row currently being
                    // EXPANDED: the roster only paints inside the expander body
                    // (unlike the "Shared" badge above, nothing about it is
                    // visible while collapsed), so fetching for a collapsed row
                    // would just write into a hidden widget nobody's looking at
                    // — see `expanded_folder_row_where` for the fuller version of
                    // this judgment call.
                    // Matched by the push's hash address (`names_set`): a
                    // sealed set's nudge names no plaintext, so the row's own
                    // rendered title is what the fetch addresses.
                    if let Some(name) = views::devices_folders::folders::expanded_folder_row_where(
                        &widgets.folder_list_box,
                        |title| p.names_set(title),
                    ) {
                        fauna_client.fetch_folder_devices(&name);
                    }
                    // (The Media page's live re-read is the seam's answer
                    // above — `ui/media.md` § Implementation status today.)
                }
                PushEvent::BridgeMailboxState(_) => {
                    // IMAP/CalDAV mailbox-state push (mail-bridge I5 Phase F).
                    // Consumed by the mailbox-state slice when it lands on
                    // linux; no-op until then.
                }
                PushEvent::BridgeConfigChanged(_) => {
                    // Bridge config hot-reload nudge — pushed only to approved
                    // mail-bridge service-user connections, never a user
                    // client's. Harmless if ever seen here; no-op.
                }
                PushEvent::BridgeAtprotoSessionsChanged(_) => {
                    // `fauna.bridges.atproto.sessions_changed` — an account's
                    // external-app session/credential/kill-switch state changed.
                    // Routed only to approved atproto.pds bridge connections
                    // (they drop cached sessions and re-fetch), never a user
                    // client's. Harmless if ever seen here; no-op (mirrors
                    // BridgeConfigChanged / BridgeRescoreReady above).
                }
                PushEvent::BridgeAtprotoProjectionReady(_) => {
                    // `fauna.bridges.atproto.projection_ready` — nudge to the
                    // approved atproto.pds bridge to pull fetch_public_posts /
                    // fetch_profile from its stored cursor (the outbound_ready
                    // nudge+poll pattern; atproto-pds-bridge.md § Where logic
                    // lives). Routed only to that bridge connection, never a
                    // user client's. Harmless if ever seen here; no-op (mirrors
                    // BridgeOutboundReady / BridgeRescoreReady above).
                }
                PushEvent::BridgeAtprotoIssuerKeyRotated(_) => {
                    // `fauna.bridges.atproto.issuer_key_rotated` — nudge to the
                    // approved atproto.pds bridge that the admin rotated the
                    // NEST's OAuth issuer key set (it re-fetches
                    // fetch_issuer_jwks and replaces the set it verifies
                    // nest-minted access tokens against). Routed only to that
                    // bridge connection, never a user client's. Harmless if
                    // ever seen here; no-op.
                }
                PushEvent::BridgeAtprotoPermissionSetRequested(_) => {
                    // `fauna.bridges.atproto.permission_set_requested` — the
                    // nest's /oauth/par asking the approved atproto.pds bridge
                    // to resolve an `include:` set (it answers with
                    // deliver_permission_set). Routed only to that bridge
                    // connection, never a user client's. Harmless if ever
                    // seen here; no-op.
                }
                PushEvent::AtprotoConsentRequested(_) => {
                    // `fauna.atproto.consent_requested` — an external ATProto
                    // app is asking to act as this account (F4 rung 2). This
                    // one IS addressed to the user's own clients: it carries
                    // the approval card's contents, including the binding code
                    // the user matches against their browser.
                    //
                    // linux built the card 2026-08-02 (batched trickle-down
                    // off tui's F4 slice 6c lead). The re-list is the seam's
                    // answer above.
                }
                PushEvent::BridgeOutboundReady(_) => {
                    // Outbound-queue drain nudge — pushed only to the approved
                    // MTA bridge's connection, never a user client's. Harmless
                    // if ever seen here; no-op.
                }
                PushEvent::BridgeRescoreReady(_) => {
                    // Re-score-drain nudge — pushed only to an approved
                    // MDA / content-processor bridge's connection, never a user
                    // client's. Harmless if ever seen here; no-op.
                }
                PushEvent::BridgeSpamBaselinePublish(_) => {
                    // Spam-baseline publish nudge — pushed only to an approved
                    // MDA / content-processor bridge's connection (it pulls the
                    // grant-gated worklist), never a user client's. Harmless if
                    // ever seen here; no-op (mirrors BridgeRescoreReady).
                }
                PushEvent::BridgeSpamModelUpdated(_) | PushEvent::BridgeSpamModelReset(_) => {
                    // `fauna.bridges.push.spam_model_{updated,reset}` — this
                    // actor's per-user spam model changed (a training event /
                    // undo) or was reset on another client. The signal is a
                    // "refresh the `mail-spam` training-history list" nudge; the
                    // page also reloads on its own `MailSpamMachine::refresh`, so
                    // this is a no-op until the linux mail-spam view subscribes
                    // to the push (mirrors BridgeMailboxState
                    // above — server-side wire kinds landed ahead of the linux
                    // consume leg). mail-spam.md §§ Reset, Undo.
                }
                PushEvent::LeaseChanged(_) => {
                    // `fauna.delegation.lease_changed` — a task-delegation lease
                    // changed hands; a "re-observe this kind's lease promptly"
                    // nudge (participants.md § Coordination primitive). No-op
                    // until the delegation lease loop is wired into linux
                    // (slice 3+ — the wire + client crate landed ahead of the
                    // consume leg, mirroring BridgeSpamModel*
                    // above); the observe poll is the backstop meanwhile.
                }
                PushEvent::BridgeConversationChanged(_) => {
                    // `fauna.bridges.push.conversation_changed` — a bridged
                    // room changed. No-op until linux registers the shared
                    // `BridgedBackend`, whose poll owns the nudge (the
                    // render slices; rust-first ordering).
                }
                PushEvent::BridgeExportProgress(_)
                | PushEvent::BridgeExportError(_)
                | PushEvent::BridgeExportComplete(_) => {
                    // `fauna.bridges.push.export_{progress,error,complete}`
                    // (mail-export.md § Session row model) — a mailbox-export
                    // session's progress/outcome nudge. linux paints the
                    // five-step wizard but drives it through
                    // `MailExportMachine`, whose seam is still the
                    // unimplemented stub: no-op until the shared client drive
                    // loop lands (rust-first ordering — the nest-side wire
                    // surface lands ahead of the client consume leg, exactly
                    // as the import block below did).
                }
                PushEvent::BridgeImportProgress(_)
                | PushEvent::BridgeImportError(_)
                | PushEvent::BridgeImportComplete(_) => {
                    // `fauna.bridges.push.import_{progress,error,complete}`
                    // (mailbox-migration.md) — a mailbox-import session's
                    // progress/outcome nudge. No linux import UI exists yet
                    // (the nest-side wire + import surface landed ahead of the
                    // client consume leg, mirroring
                    // BridgeMailboxState / LeaseChanged above); no-op until
                    // that view lands.
                }
                PushEvent::Unknown(u) => {
                    // Forward-compat: nest emitted a kind this client doesn't
                    // know. Per spec § 2.3 rule 4 — log and ignore.
                    tracing::warn!("ws-rpc: unknown push kind: {}", u.kind);
                }
            }
        }
    }
}

/// Check whether any of the supplied events are starting within the next
/// 15 minutes and show a reminder notification for each one.
///
/// Called every time `EventsLoaded` is received so that reminders fire
/// when the app first loads events and again if events are re-fetched.
fn check_event_reminders(events: &[crate::rows::EventRow]) {
    let now = fauna_core::data::Timestamp::now_secs_or_zero() as u64;

    for event in events {
        if let Ok(start) = event.start_time.parse::<u64>() {
            let diff = start.saturating_sub(now);
            if diff > 0 && diff <= 900 {
                // Within the next 15 minutes.
                crate::notifications::notify_event_reminder(&event.summary, diff / 60);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Quick Switcher dialog (Ctrl+K)
// ---------------------------------------------------------------------------

/// A single item shown in the Quick Switcher results list.
#[derive(Debug, Clone)]
struct QuickSwitcherItem {
    /// Display name shown in the list.
    display: String,
    /// The `gtk::Stack` child name to navigate to.
    section: &'static str,
}

/// Build and return the Quick Switcher modal dialog.
fn build_quick_switcher(parent: &adw::ApplicationWindow) -> adw::Window {
    let dialog = adw::Window::builder()
        .title(navigation::QUICK_SWITCHER)
        .default_width(400)
        .default_height(500)
        .modal(true)
        .transient_for(parent)
        .build();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Header bar.
    let header = adw::HeaderBar::new();
    outer.append(&header);

    // Search entry.
    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some(navigation::QUICK_SWITCHER_PLACEHOLDER));
    search_entry.set_margin_top(8);
    search_entry.set_margin_bottom(8);
    search_entry.set_margin_start(12);
    search_entry.set_margin_end(12);
    outer.append(&search_entry);

    // Results list.
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Browse);
    list_box.add_css_class("boxed-list");

    let placeholder = adw::StatusPage::builder()
        .title(navigation::NO_MATCHES)
        .icon_name("edit-find-symbolic")
        .vexpand(true)
        .build();
    list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();
    outer.append(&scrolled);

    dialog.set_content(Some(&outer));

    // Collect all items. Conversations come from the shared
    // `ConversationsManager` snapshot — the unified source. (The legacy
    // inbox-drain `state.conversations` was removed with the fauna-native
    // inbox HTTP rip-out; `ThreadSummary.label` is the per-thread display
    // name the conversations list already shows.)
    let all_items: Vec<QuickSwitcherItem> = {
        let mut items = Vec::new();

        for t in &crate::conversations::manager().snapshot().threads {
            items.push(QuickSwitcherItem {
                display: t.label.clone(),
                section: "conversations",
            });
        }

        items
    };

    // Populate helper — rebuilds the list based on the current query.
    let list_ref = list_box.clone();
    let items_ref = Rc::new(all_items);

    let populate = {
        let list_ref = list_ref.clone();
        let items_ref = Rc::clone(&items_ref);
        move |query: &str| {
            // Remove existing rows.
            while let Some(child) = list_ref.first_child() {
                list_ref.remove(&child);
            }

            let q = query.to_lowercase();
            for item in items_ref.iter() {
                if q.is_empty() || item.display.to_lowercase().contains(&q) {
                    let label = gtk::Label::new(Some(&item.display));
                    label.set_halign(gtk::Align::Start);
                    label.set_margin_top(8);
                    label.set_margin_bottom(8);
                    label.set_margin_start(12);
                    label.set_margin_end(12);
                    let row = gtk::ListBoxRow::new();
                    row.set_child(Some(&label));
                    // Store section in widget name for activation lookup.
                    row.set_widget_name(item.section);
                    list_ref.append(&row);
                }
            }
            // Select the first match automatically.
            if let Some(first) = list_ref.row_at_index(0) {
                list_ref.select_row(Some(&first));
            }
        }
    };

    // Initial populate (show all items).
    populate("");

    // Wire search entry changes.
    let populate_rc = Rc::new(populate);
    {
        let pop = Rc::clone(&populate_rc);
        search_entry.connect_search_changed(move |entry| {
            let text = entry.text().to_string();
            pop(&text);
        });
    }

    // Arrow key navigation in the search entry moves selection in list_box.
    {
        let list_nav = list_box.clone();
        let key_ctrl = gtk::EventControllerKey::new();
        let dialog_esc = dialog.clone();
        key_ctrl.connect_key_pressed(move |_, keyval, _, _| match keyval {
            gtk::gdk::Key::Down => {
                if let Some(row) = list_nav.selected_row() {
                    let next = row.index() + 1;
                    if let Some(next_row) = list_nav.row_at_index(next) {
                        list_nav.select_row(Some(&next_row));
                    }
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Up => {
                if let Some(row) = list_nav.selected_row() {
                    let prev = row.index() - 1;
                    if prev >= 0
                        && let Some(prev_row) = list_nav.row_at_index(prev)
                    {
                        list_nav.select_row(Some(&prev_row));
                    }
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Escape => {
                dialog_esc.close();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        search_entry.add_controller(key_ctrl);
    }

    // Enter in search entry activates the selected row.
    {
        let list_activate = list_box.clone();
        let dialog_close = dialog.clone();
        let parent_ref = parent.clone();
        search_entry.connect_activate(move |_| {
            if let Some(row) = list_activate.selected_row() {
                let section = row.widget_name().to_string();
                activate_quick_switcher_item(&section, &parent_ref);
                dialog_close.close();
            }
        });
    }

    // Row activated by click or Enter on the list_box itself.
    {
        let dialog_close2 = dialog.clone();
        let parent_ref2 = parent.clone();
        list_box.connect_row_activated(move |_, row| {
            let section = row.widget_name().to_string();
            activate_quick_switcher_item(&section, &parent_ref2);
            dialog_close2.close();
        });
    }

    dialog
}

/// Navigate the main window's content stack to `section`, resolving the
/// toplevel window from any widget already in the tree.
///
/// The affordance a *settings sub-page* needs to deep-link OUT of the settings
/// shell — `settings/mail_import.rs`'s two Done-step buttons ("View imported"
/// / "Review skipped" both land on Conversations, where mail lives), the linux
/// peer of tui's `Gesture::Nav(Page::Conversations)`. Deliberately reuses
/// [`activate_quick_switcher_item`]'s content-stack walk rather than a second
/// one, so the sidebar rail follows through the stack's own
/// `visible-child-name` notify exactly as it does for a quick-switcher jump —
/// leaving the settings shell by this door is the same event as leaving it by
/// any other.
pub fn navigate_to_section(widget: &impl IsA<gtk::Widget>, section: &str) {
    if let Some(window) = widget
        .root()
        .and_then(|r| r.downcast::<adw::ApplicationWindow>().ok())
    {
        activate_quick_switcher_item(section, &window);
    }
}

/// Walk a sidebar-swap shell's inner sub-stack to `sub_id` — the second half of
/// reaching a settings (or admin) SUB-page, after the content stack is already
/// showing the shell itself.
///
/// The shell's content child is a `gtk::Box` holding the lone inner
/// `gtk::Stack`, so this scans that Box's direct children for it rather than
/// threading a handle through every caller. Silent when the shell or its inner
/// stack cannot be found: both callers are navigation gestures over a window
/// they just built, and neither has a surface to fail onto.
///
/// ⚠ **One door, two callers, deliberately.** The e2e agent's nav arm
/// (`main.rs`) and the post-succession closing act
/// (`settings::recovery_kit::claim_succession_kit`'s discharge in the
/// `AuthSuccess` handler) both need "land on Settings § Account", and a second
/// hand-written copy of this walk is exactly how a production path and its test
/// path drift into proving different things (`e2e-conventions.md` convention 8 —
/// the agent drives the door a user drives).
pub fn navigate_shell_subpage(content_stack: &gtk::Stack, shell: &str, sub_id: &str) {
    let Some(shell_child) = content_stack.child_by_name(shell) else {
        return;
    };
    let mut child = shell_child.first_child();
    while let Some(w) = child {
        if let Ok(inner) = w.clone().downcast::<gtk::Stack>() {
            crate::views::nav_rail::set_visible_child_forced(&inner, sub_id);
            return;
        }
        child = w.next_sibling();
    }
}

/// Navigate the main window to the given stack section when a quick-switcher
/// item is activated.
fn activate_quick_switcher_item(section: &str, parent: &adw::ApplicationWindow) {
    // Search under the split view's CONTENT, never the window. The sidebar is
    // itself a `gtk::Stack` (`sidebar_stack`, the rail that swaps for the admin
    // and settings shells), so "the first Stack" is an ambiguous target and the
    // wrong answer navigates the rail — a failure quieter than the one this
    // function just came back from, since the rail visibly moves.
    //
    // ⚠ Measured 2026-08-16, because the first version of this comment asserted
    // the opposite: `OverlaySplitView` yields its *content* before its sidebar,
    // so an unscoped search happens to find the right stack today. Scoping is
    // still right — it names the target by construction rather than leaning on
    // an undocumented child order libadwaita may reorder — but do not repeat the
    // claim that the rail comes first; it does not.
    if let Some(stack) = split_view_of_window(parent)
        .and_then(|split| split.content())
        .and_then(|content| first_descendant::<gtk::Stack>(&content))
    {
        stack.set_visible_child_name(section);
    }
}

/// The first descendant of `root` (inclusive) of type `T`, in document order.
///
/// Type-based rather than a hand-written chain of `.child()`/`.content()`
/// downcasts, and that is the whole point — see [`split_view_of_window`].
fn first_descendant<T: glib::object::IsA<gtk::Widget>>(root: &gtk::Widget) -> Option<T> {
    if let Ok(hit) = root.clone().downcast::<T>() {
        return Some(hit);
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        if let Some(hit) = first_descendant::<T>(&c) {
            return Some(hit);
        }
        child = c.next_sibling();
    }
    None
}

/// The authenticated window's `OverlaySplitView` — the one place the chrome
/// nesting is resolved.
///
/// ⚠ **This was a hand-written downcast chain until 2026-08-16, and the chain
/// was WRONG — in two places.** Its comment claimed the window content is
/// `ToastOverlay → ToolbarView → OverlaySplitView → Stack`; the tree actually
/// built by `build_authenticated_window` is `ToastOverlay → ToolbarView →
/// content_box (a `gtk::Box` holding the error/warning/info labels *and* the
/// split view) → OverlaySplitView → content_with_alerts (another `gtk::Box`) →
/// `gtk::Overlay` → Stack`. Both omitted `Box` hops made the chain resolve to
/// `None`, so `activate_quick_switcher_item` — its only caller — had been a
/// **silent no-op**: activating a quick-switcher row navigated nowhere, with no
/// error and no log, and no test anywhere covered it.
///
/// The lesson is the shape, not the two missing hops: a hand-written widget path
/// keeps compiling when someone inserts a wrapper, and fails by returning
/// `None`, which every caller reads as "this app has no such widget". Searching
/// by TYPE cannot drift that way. It was found by convention 17's layer-(c) walk
/// on its first real run against linux, as a loud `switch_pane` refusal — which
/// is precisely the class of unseen state that layer existing is meant to catch.
pub fn split_view_of_window(window: &adw::ApplicationWindow) -> Option<adw::OverlaySplitView> {
    let content = window.content()?;
    first_descendant::<adw::OverlaySplitView>(&content)
}

// populate_conversation_list deleted: the unified conversations page
// renders its own list off the shared ConversationsManager snapshot via
// `views::conversations::list::ConversationList::render`. The legacy
// DB-backed inbox-drain → populate flow (`fetch_inbox` / `InboxLoaded`) was
// removed entirely in the WS-RPC-everywhere rip-out.

#[cfg(test)]
mod chrome_accessor_tests {
    //! Regression for a widget path that drifted and failed SILENTLY.
    //!
    //! [`split_view_of_window`] and `activate_quick_switcher_item` both used to
    //! walk a hand-written chain of `.child()`/`.content()` downcasts asserting
    //! `ToastOverlay → ToolbarView → OverlaySplitView → Stack`. The tree
    //! `build_authenticated_window` actually builds has a `gtk::Box` at BOTH of
    //! those hops — `content_box` (the error/warning/info labels beside the
    //! split view) and `content_with_alerts` — so the chain resolved to `None`
    //! and the quick switcher navigated nowhere, with no error and no log.
    //!
    //! ⚠ **The bug class, which is what this test is really for:** a hand-written
    //! widget path keeps compiling when someone inserts a wrapper, and fails by
    //! returning `None` — which every caller reads as "this app has no such
    //! widget" rather than as a broken accessor. So the assertions below are
    //! deliberately about *wrapper tolerance* and *which* stack, not about
    //! today's exact nesting: pinning the exact chain here would just recreate
    //! the brittleness in a test. Found by convention 17's layer-(c) walk on its
    //! first real run against linux, surfacing as a loud `switch_pane` refusal.
    use super::{first_descendant, split_view_of_window};
    use adw::prelude::*;

    /// Build the real chrome shape — including BOTH `Box` wrappers — and require
    /// the accessor to resolve through them.
    #[test]
    fn the_split_view_accessor_sees_through_the_box_wrappers() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let split = adw::OverlaySplitView::new();
            split.set_sidebar(Some(&gtk::Label::new(Some("rail"))));
            let stack = gtk::Stack::new();
            stack.add_named(&gtk::Label::new(Some("feed page")), Some("feed"));
            // `content_with_alerts` — wrapper #2, the one that hid the Stack.
            let content_with_alerts = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content_with_alerts.append(&stack);
            split.set_content(Some(&content_with_alerts));

            // `content_box` — wrapper #1, the one that hid the split view.
            let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content_box.append(&gtk::Label::new(Some("error-message")));
            content_box.append(&split);

            let toolbar = adw::ToolbarView::new();
            toolbar.set_content(Some(&content_box));
            let toast_overlay = adw::ToastOverlay::new();
            toast_overlay.set_child(Some(&toolbar));

            let window = adw::ApplicationWindow::builder()
                .content(&toast_overlay)
                .build();

            let found = split_view_of_window(&window);
            assert!(
                found.is_some(),
                "the split-view accessor must see through the Box wrappers — a \
                 `None` here is the silent failure this test exists to forbid, \
                 and it is what shipped until 2026-08-16"
            );

            // And the quick switcher's half: the Stack must be found under the
            // split's CONTENT, never window-wide.
            let content = found.unwrap().content().expect("the split has content");
            let stack_found = first_descendant::<gtk::Stack>(&content)
                .expect("the content stack is reachable under the split's content");
            stack_found.set_visible_child_name("feed");
            assert_eq!(
                stack_found.visible_child_name().map(|s| s.to_string()),
                Some("feed".to_owned()),
                "the accessor must return the CONTENT stack, the one the quick \
                 switcher navigates"
            );
        });
    }

    /// The quick switcher actually NAVIGATES — the property that was broken.
    ///
    /// This is deliberately a pin on `activate_quick_switcher_item` itself
    /// rather than on GTK's child order. The first draft of this test asserted
    /// that a split-wide `gtk::Stack` search reaches the SIDEBAR rail first, as
    /// the reason for scoping the search to `split.content()`. **That was
    /// measured and refuted** (2026-08-16): `OverlaySplitView` yields its
    /// *content* before its sidebar, so the naive search happens to land on the
    /// right stack today. Scoping to `split.content()` is still correct — it is
    /// precise by construction instead of leaning on an undocumented child
    /// order that a libadwaita release could reorder without telling anyone —
    /// but the honest justification is *"do not depend on incidental order"*,
    /// not *"the rail comes first"*.
    ///
    /// So the assertion below is about the app's own behaviour, which is stable:
    /// activating a row moves the CONTENT stack and leaves the rail alone.
    #[test]
    fn activating_a_quick_switcher_row_moves_the_content_stack_not_the_rail() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let split = adw::OverlaySplitView::new();
            let rail = gtk::Stack::new();
            rail.add_named(&gtk::Label::new(Some("rail-main")), Some("main"));
            rail.add_named(&gtk::Label::new(Some("rail-events")), Some("events"));
            rail.set_visible_child_name("main");
            split.set_sidebar(Some(&rail));

            let content_stack = gtk::Stack::new();
            content_stack.add_named(&gtk::Label::new(Some("feed")), Some("feed"));
            content_stack.add_named(&gtk::Label::new(Some("events")), Some("events"));
            content_stack.set_visible_child_name("feed");
            // Both `Box` wrappers the real tree has, so this exercises the same
            // path the shipped window does.
            let content_with_alerts = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content_with_alerts.append(&content_stack);
            split.set_content(Some(&content_with_alerts));

            let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content_box.append(&split);
            let toolbar = adw::ToolbarView::new();
            toolbar.set_content(Some(&content_box));
            let toast_overlay = adw::ToastOverlay::new();
            toast_overlay.set_child(Some(&toolbar));
            let window = adw::ApplicationWindow::builder()
                .content(&toast_overlay)
                .build();

            super::activate_quick_switcher_item("events", &window);

            assert_eq!(
                content_stack.visible_child_name().map(|s| s.to_string()),
                Some("events".to_owned()),
                "activating a quick-switcher row must navigate the content stack. \
                 Until 2026-08-16 this was a SILENT no-op: the hand-written widget \
                 chain missed two `Box` wrappers, resolved to `None`, and the \
                 function returned having done nothing at all"
            );
            assert_eq!(
                rail.visible_child_name().map(|s| s.to_string()),
                Some("main".to_owned()),
                "and it must not drive the sidebar rail, which is also a \
                 `gtk::Stack` and also has an `events` child"
            );
        });
    }
}
