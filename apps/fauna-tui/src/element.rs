//! The painted-element model — the crate's single source for paint, the
//! automation registry, and the keyboard focus ring.
//!
//! Ratatui is immediate-mode, so unlike linux (a persistent GTK widget tree with
//! a per-page refresh closure) each page declares its ui.yaml elements once, as
//! an ordered [`Element`] list rebuilt from the current snapshot. Paint, the
//! registry, and the focus ring all consume that **one** list, so a painted
//! element is automatable and focusable by construction — the "no invisible shim
//! elements" rule holds without a second table to keep in sync.
//!
//! **The invariant, stated once for every page that will ever exist** (it
//! originated in the wizard, and the authenticated shell now obeys it too):
//!
//! > The element list **is** the registry. The viewport clips *paint* only.
//!
//! A page lists every element its snapshot implies — all N posts of a feed, not
//! the handful that fit the terminal. `count("post-card")` answers from the
//! registry, so clipping the *list* to the visible rows would silently cap every
//! cross-app count assertion at terminal height (the e2e pty is 40×120, and a
//! human's terminal is smaller). Scrolling is a paint concern; see
//! [`crate::ui`].
//!
//! These types started life in `wizard/` and moved here when the feed page
//! landed: `use crate::wizard::Element` from an authenticated page is nonsense
//! to a cold reader. `wizard/` re-exports them, so its 14 page modules import
//! through `use super::{…}` and did not change.

use fauna_core::render::RenderDocument;
use fauna_e2e_agent::ScopeStep;

use crate::pages::Page;

/// A plain RGB colour — ratatui-free (module docs), matching
/// [`crate::thumbnail::HalfBlockCell`]'s `[u8; 3]` pixel shape. [`crate::ui`]
/// maps it to ratatui's `Color::Rgb` at paint time.
pub type Rgb = [u8; 3];

/// An editable field's identity — what `/element/type` and `/element/clear`
/// write, and what a keystroke into a focused [`Role::Input`] appends to.
///
/// Nests one enum per page, mirroring [`Gesture`]: field ownership is carried
/// on the type itself, exhaustively, so an editable field that names no
/// owning page fails to compile rather than silently routing to a fallback
/// owner (`apps/tui.md` § Target state — "field ownership is carried on
/// the `Field` type itself, exhaustively"). Every match on `Field` (and on a
/// nested per-page field enum) is exhaustive with no fallback arm; the
/// compiler enumerates every owner.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Field {
    Wizard(crate::wizard::WizardField),
    Feed(crate::feed::FeedField),
    Conversations(crate::conversations::ConversationsField),
    Contacts(crate::contacts::ContactsField),
    Unlock(crate::unlock::UnlockField),
    /// The locked launch surface's `identity_stolen_entry` step
    /// (`crate::locked`) — buffers on `App::locked`, like the unlock surface's.
    Locked(crate::locked::LockedField),
    Profile(crate::profile::ProfileField),
    Events(crate::events::EventsField),
    Settings(crate::settings::SettingsField),
    Search(crate::search::SearchField),
    Media(crate::media::MediaField),
    Admin(crate::admin::AdminField),
    Moderation(crate::moderation::ModerationField),
    /// The shared report sheet's note (`crate::report`) — owned by the sheet,
    /// not by whichever page it opened over.
    Report(crate::report::ReportField),
    Nostr(crate::nostr::NostrField),
    Bridges(crate::bridges::BridgesField),
    Family(crate::family::FamilyField),
    Backups(crate::backups::BackupsField),
}

/// A dispatchable gesture behind a painted control.
///
/// Wizard gestures map 1:1 onto `OnboardingMachine` mutators; launch gestures
/// drive the `LaunchMachine` / the launch surface, which lives outside
/// `machine.step()`; nav gestures select a sidebar page. All are painted by the
/// same [`Element`] list, so every screen — authenticated or not — has one focus
/// ring and one automation registry regardless of which surface owns it.
#[derive(Debug, Clone)]
pub enum Gesture {
    Wizard(crate::wizard::Action),
    Launch(crate::launch::LaunchAction),
    /// The `nest_retire` page and its two entries (`crate::wizard::nest_retire`)
    /// — driven by the shared `NestRetireMachine`, not the onboarding machine.
    Retire(crate::wizard::nest_retire::RetireAction),
    /// The headless-store unlock/create submit (`crate::unlock`). Synchronous
    /// on both actuation paths — see that module's docs.
    Unlock(crate::unlock::UnlockAction),
    /// `sign-out-residue-retry-button` — re-sweep what the last sign-out left
    /// (`crate::account_scope::retry_residue`). Not a wizard [`Self::Wizard`]
    /// action: it drives no onboarding-machine mutator, and it needs the app's
    /// credential store and serving bases the machine never holds. Synchronous
    /// on both actuation paths — a disk sweep and, only when credentials
    /// survived, the same credential erase a sign-out runs inline.
    RetrySignOutResidue,
    /// A sidebar `{page}-tab` row. Selection *is* navigation (the linux
    /// `gtk::ListBox` sidebar convention), so this carries no confirm step.
    Nav(Page),
    Feed(crate::feed::Action),
    Conversations(crate::conversations::Action),
    Contacts(crate::contacts::Action),
    Notifications(crate::notifications::Action),
    Moderation(crate::moderation::Action),
    /// The shared report sheet (`crate::report`) — opened by the feed, message
    /// and profile entry verbs, one component on every page it serves.
    Report(crate::report::Action),
    Profile(crate::profile::Action),
    /// Open a profile **detail** view (`profile.md` § Relationship to Contacts).
    /// `None` = the viewer's own (the `profile-tab` route, via `apply`);
    /// `Some(hex)` = another actor (a contact-row tap, the linux
    /// `open_profile(Some(hex))` twin). Carried as a distinct gesture because it
    /// sets the viewed actor, not just the page.
    OpenProfile(Option<String>),
    Events(crate::events::Action),
    /// A Settings-shell gesture (`crate::settings`) — sub-page nav
    /// (`OpenTuiSettings` / `NavBack`) or the external-media select. All local,
    /// so both actuation paths dispatch it synchronously (no network op).
    Settings(crate::settings::Action),
    /// A search-page gesture (`crate::search`) — submit / type-filter select /
    /// clear / load-more.
    Search(crate::search::Action),
    /// `search-result-item[i]` activation — carries the row's typed
    /// [`fauna_client_search::SearchNav`] target directly, like
    /// [`Self::OpenProfile`]: the target isn't known until paint time, and
    /// its destination crosses page boundaries (feed/conversations today), so
    /// it sits outside `crate::search::Action` rather than inside it —
    /// dispatching it mutates a page `search` doesn't own
    /// (`crate::search::open_result`, `search.md` § User actions).
    OpenSearchResult(fauna_client_search::SearchNav),
    /// `notification-item[i]` activation — carries the row's typed
    /// [`fauna_client_notifications::NotificationDestination`] directly, the
    /// [`Self::OpenSearchResult`] twin and for the same reason: the target
    /// isn't known until paint time and its destination crosses page
    /// boundaries (feed/contacts/family), so it sits outside
    /// `crate::notifications::Action` — dispatching it mutates a page
    /// `notifications` doesn't own (`crate::notifications::open_notification`,
    /// `behavior/notifications.md` § Deep-link destinations).
    OpenNotification(fauna_client_notifications::NotificationDestination),
    /// A media-page gesture (`crate::media`) — the explorer's view/sort/filter
    /// chrome, the upload submit, and the `media-item-detail` surface.
    Media(crate::media::Action),
    /// An admin-shell gesture (`crate::admin`) — sub-page nav (`Open`) or a DAV
    /// sub-page's enable toggle / CalDAV-port save. All local (they return a
    /// network `Op` from `apply_local`), so both actuation paths dispatch them
    /// through the one gesture door.
    Admin(crate::admin::Action),
    /// `calendar-item` click — carries the selected calendar's id (a
    /// `select_calendar`/`apply_local` split like [`Self::OpenProfile`]'s,
    /// since the target isn't known until paint time).
    SelectCalendar(String),
    /// `calendar-visibility` click — the per-calendar show/hide **display**
    /// filter beside each `calendar-item` (`ui/events.md` § Where logic lives →
    /// *Which calendars display*). Carries its own target like
    /// [`Self::SelectCalendar`], and is purely local: the toggle never refetches,
    /// because the union arm already holds every owned calendar's events.
    ToggleCalendarVisibility(String),
    /// `event-card` click — opens the `event_detail` sub-page for this
    /// event's `uid_hash` hex id. Purely local (the row is already cached),
    /// but carries its own target like [`Self::SelectCalendar`].
    OpenEventDetail(String),
    /// `event-rsvp-going|interested|decline` — the **card-level** inline RSVP
    /// quick action on an agenda `event-card`, submitting for the event whose
    /// `uid_hash` hex id it carries without opening `event_detail`
    /// (`ui/events.md` § components: `rsvp-button-group` is a child of both
    /// `event-card` and `event_detail`).
    ///
    /// Separate from the detail trio's [`crate::events::Action::Rsvp`] only in
    /// how the target is reached — both submit through the one
    /// `events::rsvp_op` producer, so the two surfaces cannot drift apart.
    RsvpEvent {
        event_id: String,
        /// The answer fed to `fauna_client_caldav::apply_rsvp`. Typed, so the
        /// button *label* ("decline") can no longer be mistaken for the value
        /// ([`RsvpResponse::Declined`]) — the near-miss windows' `NormalizeRsvp`
        /// papers over with a string compare.
        response: fauna_core::rsvp::RsvpResponse,
    },
    /// A Nostr-page gesture (`crate::nostr`) — account link/unlink, a content
    /// flag, or a relay/follow list mutation. All ride the unified
    /// `fauna.bridges.*` wire keyed `bridge_id:"nostr"` (`ui/nostr.md`
    /// § WS-RPC migration contract).
    Nostr(crate::nostr::Action),
    /// A unified Bridges-page gesture (`crate::bridges`) — a bridge link/unlink,
    /// a metadata-driven setting toggle/select, or a follow-list mutation, over
    /// the same `fauna.bridges.*` wire keyed by each bridge's own id
    /// (`behavior/bridges.md`).
    Bridges(crate::bridges::Action),
    /// A Family-page gesture (`crate::family`) — a ward selection, a local
    /// policy-editor edit, or one `fauna.family.*` mutation (policy update,
    /// approval decide, contact pre-approval, graduate, transfer handshake).
    /// The page is GATED, so this gesture is only ever painted for an account
    /// whose `fauna.family.status` reported a relationship.
    Family(crate::family::Action),
    /// A Backups-page gesture (`crate::backups`) — a destination dialog
    /// transition, or one shared enroll / edit / deregister sequence
    /// (`ui/backups.md` § Manage backup destinations).
    Backups(crate::backups::Action),
}

impl Gesture {
    /// The wire kind this gesture issues, when it issues exactly one — the
    /// input the shared offline gate
    /// ([`fauna_protocol::offline_class::affordance`]) classifies.
    ///
    /// `None` means "this gate cannot decide on it": a purely local gesture
    /// (nav, a buffer edit, a focus move), a gesture whose kind depends on
    /// state only [`apply_local`](crate::settings::apply_local) knows, or a
    /// page whose sweep has not landed yet. All three read as *available*,
    /// which is what ruling 2 of [`fauna_protocol::offline_class::affordance`]
    /// asks for: never grey a control on a guess.
    ///
    /// The match is **exhaustive with no fallback arm**, exactly like
    /// [`Field`]'s ownership match: a new page cannot be added without
    /// answering the offline question for it. The per-page `Action::wire_kind`
    /// delegates carry the same property one level down for the pages that
    /// have them.
    ///
    /// **Sweep status: COMPLETE as of 2026-08-13.** All sixteen gesture
    /// families declare per-action; the only `None`s left are the two
    /// documented classes below — genuinely local gestures, and the
    /// state-dependent ones whose discriminant is deliberately not on the
    /// action (each named in its own page's `wire_kind` doc). A *new* page must
    /// add its arm here, and the exhaustive match is what forces that.
    /// `docs/goal/architecture/account-data-plane.md` § Implementation status →
    /// *Built — W4 (account-data-plane.md § Workstreams) phase 4* owns the findings.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // ── Swept ────────────────────────────────────────────────────
            Gesture::Bridges(action) => action.wire_kind(),
            Gesture::Contacts(action) => action.wire_kind(),
            Gesture::Moderation(action) => action.wire_kind(),
            Gesture::Report(action) => action.wire_kind(),
            Gesture::Notifications(action) => action.wire_kind(),
            Gesture::Nostr(action) => action.wire_kind(),
            Gesture::Feed(action) => action.wire_kind(),
            Gesture::Profile(action) => action.wire_kind(),
            Gesture::Events(action) => action.wire_kind(),
            Gesture::Search(action) => action.wire_kind(),
            Gesture::Admin(action) => action.wire_kind(),
            Gesture::Media(action) => action.wire_kind(),
            Gesture::Settings(action) => action.wire_kind(),
            Gesture::Family(action) => action.wire_kind(),
            Gesture::Backups(action) => action.wire_kind(),
            Gesture::Conversations(action) => action.wire_kind(),
            // The card-level RSVP quick action. It reaches its target
            // differently from the detail trio but submits through the same
            // `events::rsvp_op`, so it answers what
            // `crate::events::Action::Rsvp` answers — stated here rather than
            // delegated because this variant carries no `Action` to ask.
            Gesture::RsvpEvent { .. } => Some("fauna.bridges.put_event_ciphertext"),

            // ── Local by construction ────────────────────────────────────
            // Navigation and view-local state: no nest call is issued, on any
            // path, so there is nothing for the gate to classify.
            Gesture::Nav(_)
            | Gesture::OpenProfile(_)
            | Gesture::OpenSearchResult(_)
            | Gesture::OpenNotification(_)
            | Gesture::SelectCalendar(_)
            | Gesture::ToggleCalendarVisibility(_)
            | Gesture::OpenEventDetail(_) => None,
            // The pre-authentication surfaces. A launch/wizard/unlock screen is
            // reached precisely when there is no session yet, so gating it on
            // "no live nest" would desensitize the controls whose whole job is
            // to establish one.
            Gesture::Wizard(_)
            | Gesture::Launch(_)
            | Gesture::Unlock(_)
            | Gesture::RetrySignOutResidue => None,
            // Retiring talks to the cloud provider, never to a nest — it is
            // built to work exactly when no nest is reachable
            // (`nest-retirement.md` § Goal), so the offline gate must never
            // desensitize it.
            Gesture::Retire(_) => None,
        }
    }
}

/// A `<select>`-style picker's target. `/element/select` hands the driver's
/// chosen **value** here, and the target maps it back to whatever the mutator
/// actually wants.
///
/// What that value *is* differs by picker, and both conventions are load-bearing:
/// `vps-location-picker` selects by the location's display **name** (the
/// cross-app contract apple had to fix — `get_text` returns the name, and
/// `select` takes that same name), while `feed-rule-type-select` selects by the
/// `FilterRule` variant **key** (`BodyContains`), because that is what
/// `create_feed` encodes and what the shared suites drive it with. A picker's
/// `text` is therefore always the value that round-trips back through `select`,
/// and its human-readable label rides on [`Element::label`] as paint-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectTarget {
    /// `compose-gate-tier-select` — the composer's "who can read this?" picker:
    /// the localized `Public`, one of the author's own tier names, or the
    /// localized `Sell this post…`. A **label** round-trip like
    /// [`Self::VpsLocation`], matching linux's DropDown
    /// (`views/feed/post_list.rs:619`) rather than a token split, so the
    /// cross-app `select(id, label)` contract is one shape.
    ComposeGateTier,
    VpsLocation,
    RuleType,
    Combination,
    Factor,
    /// `bridge-form-bridge-select` — the stable bridge **id** (what
    /// `subscribe_bridge` encodes) is both what `get_text` returns and what
    /// `select` takes; the human-readable name is the paint-only
    /// `display_value`, the same stable-key-vs-display split
    /// [`Self::RuleType`]/[`Self::Factor`] use.
    BridgeKind,
    /// `event-detail-reminder-select` — the raw ISO-8601 offset value
    /// (`PT15M`/`PT1H`/`P1D`) is both what `get_text` returns and what
    /// `select` takes (events.md § Reminders — the cross-app
    /// `select(id, "PT1H")` contract), so unlike `VpsLocation` there is no
    /// display-name/id split.
    ReminderOffset,
    /// `task-delegation-assignment-picker[row]` — the row's index is baked in at
    /// paint time (this enum is `Copy`, and the select op carries only the chosen
    /// **label**), so the settings layer can resolve the label back to the row's
    /// own `PinOption`. Never a bare kind string: the option list is per-row and
    /// two rows can legally offer different targets.
    TaskDelegationAssignment {
        row: usize,
    },
    /// `tui-settings-external-media` — the mode **token** (`ask`/`always`/`never`)
    /// is what `get_text` returns and `select` takes (like [`Self::ReminderOffset`],
    /// a raw-value picker); the human reads the localized label off `Element::label`.
    ExternalMedia,
    /// `room-join-rule-select` — the room policy editor's who-can-invite rule
    /// (`invite`/`member-invite`); a raw-token picker like
    /// [`Self::ReminderOffset`]: the token is what `get_text` returns and
    /// `select` takes, the localized label rides `display_value`.
    RoomJoinRule,
    /// `room-history-policy-select` — the editor's history-for-joiners rule
    /// (`none`/`full`); same token contract as [`Self::RoomJoinRule`].
    RoomHistoryPolicy,
    /// `report-reason-select` — the report sheet's reason. A raw-token picker
    /// like [`Self::RoomJoinRule`]: the shared `AbuseReportReason` token
    /// (`spam`/`harassment`/…) is what `get_text` returns and `select` takes;
    /// the localized label rides `display_value`.
    ReportReason,
    /// `log-level-filter` — the Settings → Logs severity picker. `get_text`/
    /// `select` round-trip the display **label** (`All`/`Error`/…), the DropDown
    /// contract (like [`Self::VpsLocation`]'s name round-trip, not a token split).
    LogLevel,
    /// `log-level-filter` on **`admin-logs`** — the same id, the same label
    /// round-trip as [`Self::LogLevel`], but a distinct target because the two
    /// pages filter different sources: Settings narrows this device's local ring,
    /// the admin page narrows the nest ring it fetched. Sharing one target would
    /// make filtering one page silently narrow the other.
    AdminLogLevel,
    /// `filter-rule-type` — the add-filter-form rule-kind tag (`SenderIs`/…),
    /// the PascalCase set `fauna_protocol::email::encode_filter_rule` accepts.
    /// A raw-value picker like [`Self::ReminderOffset`] — the tag round-trips
    /// as both `get_text` and `select`, no separate display label (matching
    /// linux's dropdown, which shows the tag itself).
    /// `mail-lists-add-sheet-domain-picker` — which of the user's own mail
    /// domains a new list sends from. A raw-value picker: the domain name is
    /// both what `get_text` returns and what `select` takes (the options come
    /// from the shared user-tier `derive_list_domains`, never an admin RPC).
    MailListDomain,
    /// `mail-export-format-picker` — the export wizard's step-1 format choice.
    /// A **label** round-trip like [`Self::VpsLocation`]: the option strings are
    /// the shared `export_format_label` map resolved through i18n, which is what
    /// linux's DropDown is built from and what the cross-app
    /// `select(id, label)` contract sends (`actions/mail_export.py::select_format`).
    /// A raw-token picker here would make one app's `select` call miss another's
    /// picker.
    MailExportFormat,
    /// `mail-import-source-picker` — the import wizard's step-1 provider choice.
    /// A **label** round-trip, the [`Self::MailExportFormat`] shape exactly: the
    /// option strings are the shared `import_source_kind_label` map resolved
    /// through i18n.
    MailImportSourceKind,
    /// `mail-import-source-tls-mode` — Generic/Outlook-fallback TLS choice. A
    /// **label** round-trip like [`Self::MailImportSourceKind`] (the shared
    /// `import_tls_mode_label` map), not a raw token — there is no bare-token
    /// convention for this pair the way [`Self::ReminderOffset`] has one.
    MailImportTlsMode,
    /// `archive-import-source-picker` — the archive wizard's step-1 platform
    /// choice. A **label** round-trip, the [`Self::MailImportSourceKind`] shape:
    /// the option strings are the shared `ArchiveSourceKind::label` set (brand
    /// names, deliberately untranslated), which is what every app's picker is
    /// built from and what the cross-app `select(id, label)` contract sends.
    ArchiveImportSourceKind,
    /// `archive-import-scope-audience-mode` — keep each record's platform
    /// audience or override everything to owner-only. A **label** round-trip
    /// like [`Self::ArchiveImportSourceKind`], through the localized
    /// `archive_import::audience_mode_label` map.
    ArchiveImportAudienceMode,
    /// `subscription-subscribers-tier-select` — which of the author's own tiers
    /// the §3 roster shows. A raw-value picker: the tier **name** is both what
    /// `get_text` returns and what `select` takes (the name is the server-side
    /// key, so there is no display/id split to invent).
    SubscriberRosterTier,
    /// `subscription-claim-tier-select` — which tier a freshly minted §5 claim
    /// code entitles. Same tier-name round-trip as
    /// [`Self::SubscriberRosterTier`].
    #[cfg(feature = "payments")]
    ClaimTier,
    /// `subscription-provider-form-kind` — the §4 provider kind, one of the
    /// shared `fauna_client_payments::known_kinds()` registry values. A
    /// raw-value picker (the registry token is the label).
    #[cfg(feature = "payments")]
    ProviderFormKind,
    /// `subscription-provider-form-tier-map` — which tier the §4 provider's
    /// verified payments entitle. Same tier-name round-trip as
    /// [`Self::SubscriberRosterTier`].
    #[cfg(feature = "payments")]
    ProviderFormTier,
    EmailFilterRuleType,
    /// `filter-action-select` — the add-filter-form action tag (`Allow`/
    /// `Discard`/`Reject`), same raw-value shape as
    /// [`Self::EmailFilterRuleType`].
    EmailFilterAction,
    /// `search-type-filter` — one of `crate::search`'s raw content-type
    /// tokens (`"all"`/`"post"`/`"imap"`/`"profile"`), re-firing the search
    /// SERVER-SIDE on change (linux/windows' pattern — `crate::search` module
    /// docs). A raw-value picker like [`Self::ReminderOffset`].
    SearchType,
    /// `media-sort-select` — the raw sort key (`"name"`/`"size"`/`"date"`), the
    /// stable option value every app's suite drives it with. A raw-value
    /// picker like [`Self::ReminderOffset`]; the localized label is paint-only.
    MediaSort,
    /// `media-sort-direction` — the raw direction value (`"ascending"` /
    /// `"descending"`) applied to the active [`Self::MediaSort`] key. A raw-value
    /// picker like [`Self::MediaSort`]; the ordering itself runs in shared Rust
    /// (`MediaSnapshot::view`'s `descending`), never per client.
    MediaSortDirection,
    /// `media-folder-filter` — a folder **name**, or `crate::media`'s
    /// `__all__` sentinel for the all-media default view. A raw-value picker:
    /// the set name is both what `get_text` returns and what `select` takes
    /// (the linux/web `FILTER_ALL_VALUE` contract).
    MediaFolderFilter,
    /// `share-link-expiry-select` — the raw expiry value (`"1d"` / `"7d"` /
    /// `"30d"` / `"1y"`, `fauna_client_share::EXPIRY_OPTIONS`); the localized
    /// label is paint-only.
    ShareLinkExpiry,
    /// `restore-source-select` — which configured backup destination holds the
    /// chunks (`ui/backups.md:59`). A raw-value picker over the SHARED
    /// destination label (`fauna_core::format::backup_destination_label`), so the
    /// string the user reads is the same one the destination rows above show.
    RestoreSource,
    /// `backup-folder-selector` — which folder's snapshots the list below
    /// shows (`ui/backups.md` § Snapshot-list shape, *Selection* ruling). A
    /// raw-value picker: the set NAME is both what `get_text` returns and what
    /// `select` takes, which is the cross-app contract
    /// `BackupsActions.select_folder` drives on every native app.
    BackupFolder,
    /// `backup-destination-kind-select` — which destination kind the add dialog
    /// is composing ("Another nest" / "This device"; `ui/backups.md` § Third
    /// destination kind). A raw-value picker over the SHARED kind badge label
    /// (`fauna_core::format::backup_destination_kind_label`), so the option a
    /// user picks is the same string the status row's badge shows back.
    BackupDestinationKind,
    /// `restore-snapshot-select` — which local snapshot to restore
    /// (`ui/backups.md:60`). The option string is linux's display form
    /// (`"mail (#7)"`, `views/backups/restore.rs:257-263`) rather than a raw id,
    /// because the kind is what distinguishes two same-day snapshots; the id the
    /// friction bar matches against is read off the selected row's state, never
    /// off this label.
    RestoreSnapshot,
    /// `admin-aliases-forwarder-add-domain-select` — the hosted local domain the
    /// new external forwarder lives on (`admin.md` § 4 Aliases). A raw-value
    /// picker like [`Self::ReminderOffset`]: the domain name is both what
    /// `get_text` returns and what `select` takes (the web `<option value={d}>`
    /// contract), chosen from the snapshot's `local_domains`.
    ForwarderDomain,
    /// `admin-mail-fcrdns-mode-select` — the FCrDNS-mode wire value
    /// (`off` / `score_signal` / `enforce`, `admin.md` § 6 Mail). A raw-value picker:
    /// the wire token is both what `get_text` returns and what `select` takes,
    /// stored on the Spam draft until Save (`SpamPolicyView.fcrdns_mode`).
    MailFcrdnsMode,
    /// `admin-mail-imap-delete-nonempty-select` — the delete-non-empty wire value
    /// (`forbidden` / `allowed`, `admin.md` § 6 Mail). A raw-value picker like
    /// [`Self::MailFcrdnsMode`], stored on the IMAP draft (`ImapPolicyView.delete_nonempty`).
    MailImapDelete,
    /// `admin-users-tier-select[row]` — the `row`-th user's tier (= quota, `admin.md`
    /// § 2 Users). A raw-value picker: the tier *name* is both what `get_text`
    /// returns and what `select` takes (the tier-list contract). The row is baked at
    /// paint time (the select op passes only the value, not the occurrence index),
    /// so a flat per-row picker still resolves its own user. LIVE — the select
    /// dispatches `users_update` and the row re-renders from persisted state.
    UsersTier {
        row: usize,
    },
    /// `admin-users-registration-mode-select` — the nest's registration posture
    /// (`admin.md` § 2 Registration). A raw-value picker over the wire values
    /// (`open`/`invite_required`/`closed`, the shared `registration_mode_options`);
    /// LOCAL — the draft is committed by `admin-users-registration-save-button`.
    RegistrationMode,
    /// `admin-settings-tier-select` (on the `admin-users` hub) — the tier a minted
    /// invite code admits at (`admin.md` § 2 Invite). LOCAL draft, committed by
    /// `create-invite-confirm-btn`.
    InviteTier,
    /// `admin-users-admit-tier-select` — the tier a directly-admitted account
    /// gets (`public-mode.md` § Registration & Identity: admission is always
    /// choosing a tier). A raw-value picker like [`Self::InviteTier`]; LOCAL
    /// draft, committed by `admin-users-admit-button`.
    AdmitTier,
    /// `admin-users-invite-guardian-select` — the guardian for a supervised
    /// admission via a minted code (family-safety). **A LABEL picker** like
    /// [`Self::FamilyUnknownSender`]: the option value is the user's handle (the
    /// `actions/admin.py` contract, shared with linux/android), mapped back to the
    /// wire actor id by `admin::resolve_guardian` at mint time. LOCAL draft.
    InviteGuardian,
    /// `admin-users-invite-age-band-select` — the minted code's age band for a
    /// supervised admission (`family-safety.md` § App surface → *Age-band
    /// surfaces*). LOCAL draft; option VALUES (`not-set` + the four wire tokens).
    InviteAgeBand,
    /// `invite-request-row-tier-select[row]` — the tier the `row`-th pending request
    /// is approved at (`admin.md` § 2 Pending requests). LOCAL draft (no nest call
    /// until approve); the row is baked at paint time (the select passes only the value).
    RequestTier {
        row: usize,
    },
    /// `invite-request-row-guardian-select[row]` — the guardian for approving the
    /// `row`-th request as a supervised admission. Same LABEL contract as
    /// [`Self::InviteGuardian`]. LOCAL draft.
    RequestGuardian {
        row: usize,
    },
    /// `invite-request-row-age-band-select[row]` — the band the `row`-th pending
    /// request is admitted at (defaults to the applicant's claim). LOCAL draft.
    RequestAgeBand {
        row: usize,
    },
    /// `admin-settings-membership-tier-select[row]` — the `row`-th membership
    /// row's subscription-tier name (monetization.md § Pillar 4). A raw-value
    /// picker: the tier name is both `get_text` and `select`. LOCAL, display +
    /// fix-up only — `admin-settings-membership-save-button` reads the current
    /// selection directly (linux's `build_membership_row` comment).
    MembershipTierName {
        row: usize,
    },
    /// `admin-settings-membership-admin-tier-select[row]` — the `row`-th
    /// membership row's admitted quota-tier draft. LOCAL, committed by
    /// `admin-settings-membership-save-button`.
    MembershipAdminTier {
        row: usize,
    },
    /// `admin-settings-membership-lapse-tier-select[row]` — the `row`-th
    /// membership row's lapsed quota-tier draft. LOCAL, same commit as
    /// [`Self::MembershipAdminTier`].
    MembershipLapseTier {
        row: usize,
    },
    /// `nostr-link-mode` — the link *request* mode (`generate`/`import`/
    /// `remote`, `crate::nostr::LINK_MODES`). A raw-value picker like
    /// [`Self::ExternalMedia`]: the mode token is both what `get_text` returns
    /// and what `select` takes. LOCAL — it only swaps which credential field
    /// renders; `nostr-link-button` commits. `nip07` is deliberately absent —
    /// a browser-extension boundary call has no meaning in a terminal
    /// (`ui/nostr.md` § Architectural rules 4, the native-client set).
    NostrLinkMode,
    /// `folder-conflict-policy-select` — the per-set conflict-policy edit
    /// (every owner row), scoped to its `folder-row[row]`
    /// (`ui/folders.md` § Conflicts). A raw-value picker like
    /// [`Self::ReminderOffset`]: the wire value round-trips both ways.
    FolderConflictPolicy {
        row: usize,
    },
    /// `folder-audience-select` — the per-folder audience control on the expanded
    /// row. A raw-value picker like [`Self::FolderConflictPolicy`]: the option
    /// values ARE the wire values (`private` / `shared` / `public`), so the
    /// selection round-trips without a display-name lookup.
    ///
    /// The option SET is built per-row from `audience_options(bound, current)`,
    /// so it already excludes what the nest would refuse — and offers `shared`
    /// as the flip-back exit while a bound folder is `public`.
    /// Picking `public` does not commit — it arms the declassify confirm
    /// (`Action::SetFolderAudience` -> `FoldersUiState::audience_public_pending`).
    FolderAudience {
        row: usize,
    },
    /// `folder-nest-residency-select` — the nest place's content residency on
    /// the expanded owner row (phase 5). A raw-value picker over the wire values
    /// `full` / `metadata_only` (`fauna_folders_machine::residency_options`).
    /// Picking `metadata_only` does not commit — it arms the residency confirm
    /// (`Action::SetFolderResidency` -> `FoldersUiState::residency_pending`),
    /// exactly as `public` arms the declassify confirm on [`Self::FolderAudience`].
    FolderNestResidency {
        row: usize,
    },
    /// `folder-paywall-tier-select` — the per-set "paywall to tier" control on
    /// **web-type** rows, the structural sibling of `folder-webdav-toggle` on
    /// owner rows. A raw-value picker: the option values are the creator's own
    /// tier NAMES (the wire value `web_paywall_tier` stores), so the selected
    /// value round-trips both ways. Empty string = the "Not paywalled"
    /// placeholder, offered only while the set is still public (v1 is set-only).
    FolderPaywallTier {
        row: usize,
    },
    /// `folder-member-role-select[member]` inside `folder-row[row]` — the
    /// per-member access edit (Reader | Writer), owner-editable in place
    /// (`ui/folders.md` § Sharing, multi-writer Phase 1). A raw-value picker
    /// like [`Self::FolderConflictPolicy`]: the option values ARE the wire
    /// values (`member_access_options()`), so `select(id, "writer")` and
    /// `get_text(id) == "writer"` both round-trip.
    ///
    /// Carried by INDICES, not the member's actor id, because `SelectTarget`
    /// must stay `Copy` — `crate::settings` resolves them back against the live
    /// roster, the [`Self::BridgeSetting`] convention.
    FolderMemberAccess {
        row: usize,
        member: usize,
    },
    /// `folder-share-role-select` — the access granted to the recipient being
    /// picked in the open share form, default Reader. Same raw-value contract as
    /// [`Self::FolderMemberAccess`]; carries no index (one open form at a time).
    FolderShareAccess,
    /// `sync-default-conflict-policy-select` — the PAGE-LEVEL default conflict
    /// policy stamped onto newly created sets (`ui/folders.md` § Element IDs).
    /// Carries no `row`: unlike [`Self::FolderConflictPolicy`] there is exactly
    /// one per page, and it edits `fauna.state.sync-prefs`, not a set row.
    FolderDefaultConflictPolicy,
    /// `folder-nest-snapshots-select` — the expanded row's nest-place
    /// "keeps snapshots" knob, indexed by folder row. A raw-value picker over
    /// the three stable values `default` / `on` / `off`
    /// (`folders::NEST_SNAPSHOTS_*`); `default` is UNSET, not off.
    FolderNestSnapshots {
        row: usize,
    },
    /// `folder-destination-attach-select` — which enrolled backup destination
    /// to give the expanded folder a destination place on
    /// (`backup-destinations.md` § Ordinary-folder coverage). A raw-value
    /// picker: the stable `destination_id` round-trips as both `get_text` and
    /// `select` (the [`Self::BridgeKind`] id-vs-display split); the display
    /// name is paint-only.
    FolderDestinationAttach {
        row: usize,
    },
    /// A `select`-type bridge setting on the unified Bridges page
    /// (`behavior/bridges.md` § Bridge settings). A raw-value picker: the
    /// `BridgeSettingOption.value` round-trips as both `get_text` and `select`.
    /// Carried by **indices** — `bridge` into `App::bridges::bridges`, `setting`
    /// into that bridge's `settings` — because [`SelectTarget`] must stay `Copy`
    /// and cannot hold the `String` bridge id / setting key; `crate::bridges`
    /// resolves them back against the live snapshot (the [`Self::UsersTier`]
    /// row-scoped-select convention). The setting rows carry no ui.yaml id, so
    /// this picker is untagged (keyboard-focusable, not automatable).
    BridgeSetting {
        bridge: usize,
        setting: usize,
    },
    /// `family-policy-unknown-sender-select` — the guardian's cold-inbound-mail
    /// knob (`family-safety.md` § Guardian policy pillar 1).
    ///
    /// **A LABEL picker, not a raw-value one** (unlike most of the list above):
    /// `actions/family.py` compares `get_text` against its `UNKNOWN_SENDER_LABELS`
    /// map and calls `select(id, LABEL)`, so both directions carry the LOCALIZED
    /// label and `crate::family` maps it back to the wire value against the
    /// shared `fauna_core::format::unknown_sender_options` catalog — the same
    /// contract linux's `gtk::StringList` model states. LOCAL: the draft is
    /// committed by `family-policy-save-button`.
    FamilyUnknownSender,
    /// `family-policy-feed-sources-select` — same label contract, this knob's
    /// two-option catalog.
    FamilyFeedSources,
    /// `family-policy-unknown-peer-dm-select` — the bridge-DM gate's knob
    /// (`family-safety.md` § The bridge-DM gate). Same label contract, this
    /// knob's two-option catalog.
    FamilyUnknownPeerDm,
    /// `family-policy-content-{nsfw,spam,phishing,commercial}-select` — the
    /// guardian's per-category content floor (§ Content policy). Same label
    /// contract; `category` is the index into
    /// `fauna_core::obligation::GUARDIAN_FLOOR_CATEGORIES`, carried as a `usize`
    /// so [`SelectTarget`] stays `Copy` (the [`Self::FolderConflictPolicy`]
    /// row-scoped-select convention).
    FamilyContentFloor {
        category: usize,
    },
    /// `nest-trust-mint-scope-select` — the Nests page's scope-first mint
    /// picker (`nests.md` § Mint). A **LABEL** picker like
    /// [`Self::FamilyUnknownSender`]: `actions/nest_trust.py` calls
    /// `select(id, S.nests.mint_option_*)` and every other app's option text
    /// IS that localized label, so `crate::settings::nests` maps it back to a
    /// `LinkedNestRow.mint_options` index against the shared catalog. Carries no
    /// row index — v1 populates the catalog on the home row only, and the open
    /// form remembers which row it belongs to.
    TrustMintScope,
    /// `nest-trust-mint-duration-select` — the mint form's duration pick,
    /// round-tripping the rendered label like [`Self::TrustMintScope`].
    TrustMintDuration,
    /// `custody-mint-host-select` — the custody offer-initiation host pick
    /// (Devices page, T16). Option text is the conversation's rendered
    /// label; the settings state maps it back to a candidate index.
    CustodyMintHost,
    /// `custody-offer-target-select[i]` — the consent card's host-side
    /// choice ("This device" / "My nest", the nest-custodian identity
    /// fact). Carries the painted offer index; the settings state maps the
    /// LABEL to a per-grant flag the accept reads.
    CustodyOfferTarget(u32),
    /// `nest-trust-mint-holder-select` — the CONDITIONAL holder pick, rendered
    /// only when the chosen scope option lists more than one candidate (never
    /// today; every option derives exactly one holder). A raw-value picker: the
    /// candidate's stable `bridge_id` is both what `get_text` returns and what
    /// `select` takes, matching linux's candidate `StringList`.
    TrustMintHolder,
    /// `admin-web-apex-actor-select` — whose `web` content serves the deployment
    /// apex, or the localized "None" (clear). A **label** round-trip like
    /// [`Self::VpsLocation`], matching linux's `DropDown` of actor labels: the
    /// admin layer resolves the label back to an actor id against the snapshot
    /// that painted it, so no positional index crosses the gesture door.
    ApexActor,
    /// `admin-dns-domain-catch-all-select[row]` — which actor receives the
    /// `row`-th domain's unmatched mail, or the localized "None" (clear). A
    /// **label** round-trip like [`Self::ApexActor`]; the row index is baked in at
    /// paint time (this enum is `Copy` and the select op carries only the chosen
    /// label), so the admin layer resolves the label against that row's own domain.
    DnsCatchAll {
        row: usize,
    },
    /// `admin-dns-domain-role-address-<role>-select[row]` — which actor receives
    /// the `row`-th domain's `<role>@` mail, or "Admin (default)" (clear). Same
    /// label round-trip as [`Self::DnsCatchAll`]; the role rides the target because
    /// four independent pickers share one row (the nest atomic-merges them).
    DnsRoleAddress {
        row: usize,
        role: fauna_client_mail_settings::local_domains::RoleAddressKind,
    },
    /// `admin-dns-rename-new-primary-select` — which active non-primary domain the
    /// primary-domain rename promotes. A raw-value picker: the domain **name** is
    /// both what `get_text` returns and what `select` takes, resolved to the row's
    /// 16-byte `domain_id` at submit.
    DnsRenameTarget,
    /// `personalization-trained-factor-publish-kind-select` — which artifact
    /// kind the open publish sheet reviews. A RAW-VALUE picker: `get_text` and
    /// `select` round-trip the `artifact_kind` discriminator (`list` /
    /// `text-model`), and the human label rides `display_value` — so a driver
    /// names the same two strings on all 7 apps and no rewording of the copy
    /// can make the select undriveable (`BackupDestinationKind`'s rule).
    TrainedFactorPublishKind,
    /// `admin-dns-cert-delegate-zone-select[row]` — which controlled zone the
    /// `row`-th domain's `_acme-challenge` renewals are re-homed into. A raw-value
    /// picker (the zone name round-trips), offered only from the zones a held
    /// credential covers. The row index is carried for symmetry with the other
    /// per-row pickers even though at most one delegate form is open at a time.
    DnsDelegateZone {
        row: usize,
    },
}

impl SelectTarget {
    /// The gesture this picker dispatches for `value`. Returns a [`Gesture`], not
    /// a wizard action, so the pickers on an authenticated page dispatch through
    /// the same one door as everything else.
    pub fn gesture(self, value: String) -> Gesture {
        match self {
            SelectTarget::VpsLocation => {
                Gesture::Wizard(crate::wizard::Action::SelectVpsLocationByName(value))
            }
            SelectTarget::RuleType => Gesture::Feed(crate::feed::Action::SetRuleType(value)),
            SelectTarget::Combination => Gesture::Feed(crate::feed::Action::SetCombination(value)),
            SelectTarget::Factor => Gesture::Feed(crate::feed::Action::SetFactor(value)),
            SelectTarget::BridgeKind => Gesture::Feed(crate::feed::Action::SetBridgeKind(value)),
            SelectTarget::ReminderOffset => {
                Gesture::Events(crate::events::Action::SetReminderOffset(value))
            }
            SelectTarget::RoomJoinRule => {
                Gesture::Conversations(crate::conversations::Action::SetRoomJoinRule(value))
            }
            SelectTarget::RoomHistoryPolicy => {
                Gesture::Conversations(crate::conversations::Action::SetRoomHistoryPolicy(value))
            }
            SelectTarget::ReportReason => Gesture::Report(crate::report::Action::SetReason(value)),
            SelectTarget::TaskDelegationAssignment { row } => {
                Gesture::Settings(crate::settings::Action::SetTaskAssignment { row, label: value })
            }
            SelectTarget::TrainedFactorPublishKind => {
                Gesture::Settings(crate::settings::Action::SetTrainedFactorPublishKind(
                    // The picker only ever emits one of its two option tokens;
                    // an unrecognized value keeps the List default rather than
                    // silently upgrading to the stronger disclosure.
                    crate::settings::trained_topics::PublishKind::from_wire(&value),
                ))
            }
            SelectTarget::ExternalMedia => {
                Gesture::Settings(crate::settings::Action::SetExternalMedia(
                    // The picker only ever emits one of its three option tokens;
                    // an unrecognized value keeps the default rather than panicking.
                    crate::settings::ExternalMediaMode::from_token(&value).unwrap_or_default(),
                ))
            }
            SelectTarget::LogLevel => Gesture::Settings(crate::settings::Action::SetLogLevel(
                // The label maps back to a filter index; an unknown label → All.
                crate::settings::log_filter_index_for_label(&value),
            )),
            SelectTarget::AdminLogLevel => Gesture::Admin(crate::admin::Action::SetLogLevel(
                // Same label→index resolution as the Settings picker; the label
                // set is shared, only the filtered source differs.
                crate::settings::log_filter_index_for_label(&value),
            )),
            SelectTarget::ComposeGateTier => Gesture::Feed(crate::feed::Action::SetGateTier(value)),
            SelectTarget::MailListDomain => {
                Gesture::Settings(crate::settings::Action::MailListsSetDomain(value))
            }
            SelectTarget::MailExportFormat => Gesture::Settings(
                // The label maps back to its `ExportFormat`; an unknown label keeps
                // the ratified default (mbox) rather than panicking.
                crate::settings::Action::MailExportSelectFormat(
                    crate::settings::mail_export::format_for_label(&value),
                ),
            ),
            SelectTarget::MailImportSourceKind => Gesture::Settings(
                // The label maps back to its `ImportSourceKind`; an unknown label
                // keeps the ratified default (Gmail) rather than panicking.
                crate::settings::Action::MailImportSelectSourceKind(
                    crate::settings::mail_import::source_kind_for_label(&value),
                ),
            ),
            SelectTarget::MailImportTlsMode => {
                Gesture::Settings(crate::settings::Action::MailImportSetTlsMode(
                    crate::settings::mail_import::tls_mode_for_label(&value),
                ))
            }
            SelectTarget::ArchiveImportSourceKind => Gesture::Settings(
                // The label maps back to its `ArchiveSourceKind`; an unknown
                // label keeps the default (Facebook) rather than panicking.
                crate::settings::Action::ArchiveImportSelectSource(
                    crate::settings::archive_import::source_kind_for_label(&value),
                ),
            ),
            SelectTarget::ArchiveImportAudienceMode => {
                Gesture::Settings(crate::settings::Action::ArchiveImportSetAudienceMode(
                    crate::settings::archive_import::audience_mode_for_label(&value),
                ))
            }
            SelectTarget::SubscriberRosterTier => {
                Gesture::Profile(crate::profile::Action::SelectRosterTier(value))
            }
            #[cfg(feature = "payments")]
            SelectTarget::ClaimTier => {
                Gesture::Profile(crate::profile::Action::SelectClaimTier(value))
            }
            #[cfg(feature = "payments")]
            SelectTarget::ProviderFormKind => {
                Gesture::Profile(crate::profile::Action::SetProviderFormKind(value))
            }
            #[cfg(feature = "payments")]
            SelectTarget::ProviderFormTier => {
                Gesture::Profile(crate::profile::Action::SetProviderFormTier(value))
            }
            SelectTarget::EmailFilterRuleType => {
                Gesture::Settings(crate::settings::Action::SetFilterRuleType(value))
            }
            SelectTarget::EmailFilterAction => {
                Gesture::Settings(crate::settings::Action::SetFilterActionSelect(value))
            }
            SelectTarget::SearchType => {
                Gesture::Search(crate::search::Action::SetTypeFilter(value))
            }
            SelectTarget::MediaSort => Gesture::Media(crate::media::Action::SetSort(value)),
            SelectTarget::MediaSortDirection => {
                Gesture::Media(crate::media::Action::SetSortDirection(value))
            }
            SelectTarget::MediaFolderFilter => {
                Gesture::Media(crate::media::Action::SetFilter(value))
            }
            SelectTarget::ShareLinkExpiry => {
                Gesture::Media(crate::media::Action::SetShareExpiry(value))
            }
            SelectTarget::BackupFolder => {
                Gesture::Backups(crate::backups::Action::SelectFolder(value))
            }
            SelectTarget::RestoreSource => {
                Gesture::Backups(crate::backups::Action::SelectRestoreSource(value))
            }
            SelectTarget::BackupDestinationKind => {
                Gesture::Backups(crate::backups::Action::SelectDestinationKind(value))
            }
            SelectTarget::RestoreSnapshot => {
                Gesture::Backups(crate::backups::Action::SelectSnapshot(value))
            }
            SelectTarget::ForwarderDomain => {
                Gesture::Admin(crate::admin::Action::SelectForwarderDomain(value))
            }
            SelectTarget::ApexActor => Gesture::Admin(crate::admin::Action::SelectApexActor(value)),
            SelectTarget::DnsCatchAll { row } => {
                Gesture::Admin(crate::admin::Action::SelectDnsCatchAll { row, label: value })
            }
            SelectTarget::DnsRoleAddress { row, role } => {
                Gesture::Admin(crate::admin::Action::SelectDnsRoleAddress {
                    row,
                    role,
                    label: value,
                })
            }
            SelectTarget::DnsRenameTarget => {
                Gesture::Admin(crate::admin::Action::SelectDnsRenameTarget(value))
            }
            SelectTarget::DnsDelegateZone { row: _ } => {
                Gesture::Admin(crate::admin::Action::SelectDnsDelegateZone(value))
            }
            SelectTarget::MailFcrdnsMode => {
                Gesture::Admin(crate::admin::Action::SetFcrdnsMode(value))
            }
            SelectTarget::MailImapDelete => {
                Gesture::Admin(crate::admin::Action::SetImapDelete(value))
            }
            SelectTarget::UsersTier { row } => {
                Gesture::Admin(crate::admin::Action::SetUserTier { row, tier: value })
            }
            SelectTarget::RegistrationMode => {
                Gesture::Admin(crate::admin::Action::SetRegistrationMode(value))
            }
            SelectTarget::InviteTier => Gesture::Admin(crate::admin::Action::SetInviteTier(value)),
            SelectTarget::AdmitTier => Gesture::Admin(crate::admin::Action::SetAdmitTier(value)),
            SelectTarget::InviteGuardian => {
                Gesture::Admin(crate::admin::Action::SetInviteGuardian(value))
            }
            SelectTarget::InviteAgeBand => {
                Gesture::Admin(crate::admin::Action::SetInviteAgeBand(value))
            }
            SelectTarget::RequestTier { row } => {
                Gesture::Admin(crate::admin::Action::SetRequestTier { row, tier: value })
            }
            SelectTarget::RequestGuardian { row } => {
                Gesture::Admin(crate::admin::Action::SetRequestGuardian {
                    row,
                    guardian: value,
                })
            }
            SelectTarget::RequestAgeBand { row } => {
                Gesture::Admin(crate::admin::Action::SetRequestAgeBand { row, band: value })
            }
            SelectTarget::MembershipTierName { row } => {
                Gesture::Admin(crate::admin::Action::SetMembershipTierName {
                    row,
                    tier_name: value,
                })
            }
            SelectTarget::MembershipAdminTier { row } => {
                Gesture::Admin(crate::admin::Action::SetMembershipAdminTier { row, tier: value })
            }
            SelectTarget::MembershipLapseTier { row } => {
                Gesture::Admin(crate::admin::Action::SetMembershipLapseTier { row, tier: value })
            }
            SelectTarget::NostrLinkMode => Gesture::Nostr(crate::nostr::Action::SetLinkMode(value)),
            SelectTarget::BridgeSetting { bridge, setting } => {
                Gesture::Bridges(crate::bridges::Action::SetSelectSetting {
                    bridge,
                    setting,
                    value,
                })
            }
            SelectTarget::FolderConflictPolicy { row } => {
                Gesture::Settings(crate::settings::Action::SetFolderConflictPolicy {
                    row,
                    policy: value,
                })
            }
            SelectTarget::FolderNestSnapshots { row } => {
                Gesture::Settings(crate::settings::Action::SetFolderNestSnapshots { row, value })
            }
            SelectTarget::FolderDestinationAttach { row } => {
                Gesture::Settings(crate::settings::Action::SetFolderDestinationAttach {
                    row,
                    destination_id: value,
                })
            }
            SelectTarget::FolderDefaultConflictPolicy => Gesture::Settings(
                crate::settings::Action::SetFolderDefaultConflictPolicy(value),
            ),
            SelectTarget::FolderAudience { row } => {
                Gesture::Settings(crate::settings::Action::SetFolderAudience {
                    row,
                    audience: value,
                })
            }
            SelectTarget::FolderNestResidency { row } => {
                Gesture::Settings(crate::settings::Action::SetFolderResidency {
                    row,
                    residency: value,
                })
            }
            SelectTarget::FolderPaywallTier { row } => {
                Gesture::Settings(crate::settings::Action::SetFolderPaywallTier {
                    row,
                    tier: value,
                })
            }
            SelectTarget::FolderMemberAccess { row, member } => {
                Gesture::Settings(crate::settings::Action::SetFolderMemberAccess {
                    row,
                    member,
                    access: Some(value),
                })
            }
            SelectTarget::FolderShareAccess => {
                Gesture::Settings(crate::settings::Action::SetFolderShareAccess(value))
            }
            // The four family pickers hand the LOCALIZED label straight to the
            // page, which resolves it against the shared catalog (and fails
            // closed on a label outside it) — the mapping lives there, once.
            SelectTarget::FamilyUnknownSender => {
                Gesture::Family(crate::family::Action::SetUnknownSender(value))
            }
            SelectTarget::FamilyFeedSources => {
                Gesture::Family(crate::family::Action::SetFeedSources(value))
            }
            SelectTarget::FamilyUnknownPeerDm => {
                Gesture::Family(crate::family::Action::SetUnknownPeerDm(value))
            }
            SelectTarget::FamilyContentFloor { category } => {
                Gesture::Family(crate::family::Action::SetContentFloor {
                    category,
                    label: value,
                })
            }
            SelectTarget::TrustMintScope => {
                Gesture::Settings(crate::settings::Action::NestsSetMintScope(value))
            }
            SelectTarget::TrustMintDuration => {
                Gesture::Settings(crate::settings::Action::NestsSetMintDuration(value))
            }
            SelectTarget::CustodyMintHost => {
                Gesture::Settings(crate::settings::Action::CustodyMintSelectHost(value))
            }
            SelectTarget::CustodyOfferTarget(index) => {
                Gesture::Settings(crate::settings::Action::CustodyOfferSelectTarget {
                    index,
                    label: value,
                })
            }
            SelectTarget::TrustMintHolder => {
                Gesture::Settings(crate::settings::Action::NestsSetMintHolder(value))
            }
        }
    }
}

/// What a painted element *is* — determines how it renders, whether the
/// automation agent can actuate it, and whether the focus ring stops on it.
#[derive(Debug, Clone)]
pub enum Role {
    /// Read-only text (headings, status lines, message areas).
    Label,
    Button(Gesture),
    Input(Field),
    /// An input that ALSO commits on activation — a GTK `gtk::Entry` with a
    /// `connect_activate` handler, i.e. "type, then press Enter".
    ///
    /// Plain [`Self::Input`] registers a `field` and NO action, so a click on it
    /// answers `not actuable`; but the cross-app action layer's commit idiom for
    /// this widget class is *type then click* (`actions/backups.py::set_member_cap`
    /// — "the agent's `click` on an editable emits exactly that activation,
    /// mirroring the SpinButton commit idiom"). Without this variant a tui input
    /// can only be committed by some OTHER element, which is why
    /// `folder-member-cap-input` needed it: nothing else on the member row
    /// writes the cap. Same shape of gap as `Element::enabled(bool)` before the
    /// 2026-07-30 webdav slice — the vocabulary, not the widget, was missing.
    InputCommit {
        field: Field,
        gesture: Gesture,
    },
    Checkbox {
        gesture: Gesture,
        checked: bool,
    },
    /// One option of a mutually-exclusive group (an inbox mode, a wizard
    /// folder mode, a calendar view mode) — paints `(*) label` / `( ) label`,
    /// the terminal radio idiom, so the CURRENT choice is visible on the
    /// group itself. Distinct from [`Role::Checkbox`] (`[x]`, independent
    /// on/off) and from a plain [`Role::Button`] (`[ label ]`, a one-shot
    /// action): a live user reading a group of plain buttons has no on-screen
    /// answer to "which one is currently chosen" (the 2026-08-03 comprehensibility audit's second
    /// seed finding; the user-reported control-kind-legibility class).
    /// Selection paint is a marker, never `REVERSED` — reverse video is the
    /// focus affordance and must stay unique to it (`render_page`'s rule).
    Radio {
        gesture: Gesture,
        selected: bool,
    },
    /// A picker (the linux `gtk::DropDown`, web `<select>`). `text` is the
    /// **selected option's display value** — the cross-app
    /// `vps-location-picker` contract is that `get_text` returns the location's
    /// display *name* and `select` takes that same name (apple had to fix
    /// exactly this: it was returning the id). `options` drives the keyboard
    /// path, which cycles through them.
    ///
    /// `display` is the **paint-only human form of the current value**, for
    /// the pages whose `text` must stay a raw wire key (`"none"`, a variant
    /// name). With it, [`Element::label`] means *prompt* on every role
    /// uniformly — the pre-2026-08-03 paint treated a select's label as its
    /// display value instead, and every page that passed a field NAME there
    /// painted the name and silently hid the current VALUE (`< Share model
    /// with community >` with no way to see "none" — the audit's
    /// dropdown-state-unknowable class).
    Select {
        target: SelectTarget,
        options: Vec<String>,
        display: Option<String>,
    },
}

/// One ui.yaml element painted by the current page.
///
/// `id` is a `String`, not a `&'static str`: a provider's credential inputs are
/// `dns-credentials-form-{field.id}`, and the field set is per-provider data
/// from the generated providers table — so the id genuinely isn't known at
/// compile time. (The registry copied every id into a `String` anyway, so this
/// moves the allocation rather than adding one.)
#[derive(Debug, Clone)]
pub struct Element {
    pub id: String,
    pub text: String,
    pub enabled: bool,
    pub role: Role,
    /// Paint-only prompt for an input, e.g. "API token" for
    /// `dns-credentials-form-api-token`. **Not** registered with the automation
    /// agent — an element's registry `text` stays its *value*, which is what a
    /// driver's `get_text` must return. Without this a TUI input can only prompt
    /// with its own element id, which is meaningless to the human typing into a
    /// per-provider credential field or a 9-field WHOIS form.
    pub label: Option<String>,
    /// Ancestor scope path — `[(container-id, occurrence-index)]`. Empty for a
    /// top-level element, so a scoped query correctly misses it.
    ///
    /// This is what lets a test say `count("provisioning-substep",
    /// scope="provisioning-step-row[2]")` instead of counting globally and
    /// slicing (the "use scoped queries, not global counts" rule). ui.yaml
    /// models the four provisioning rows as the `provisioning-progress`
    /// component, whose five child IDs repeat once per row — so those IDs are
    /// only addressable *through* a scope.
    pub path: Vec<ScopeStep>,
    /// A rich body to paint by **walking** it ([`crate::document::render_document`]),
    /// rather than as the single flat line `text` would give.
    ///
    /// This is what keeps ONE element list driving all three consumers when a
    /// page has rich content: paint walks the document, while the registry and
    /// every `get_text` still read the flat [`Element::text`], which
    /// [`Element::document`] pins to `doc.to_plaintext()` — the same shared read
    /// every app's `feed-post-text` answers with. A second, parallel "rich
    /// content" list would be exactly the drift this model exists to prevent.
    pub doc: Option<RenderDocument>,
    /// A raster body to paint as **per-cell coloured** half-block art
    /// ([`crate::thumbnail`]) — today the `media-thumbnail` of a Media item.
    ///
    /// A third arm beside [`Element::doc`] rather than a `text` encoding,
    /// because colour is the one thing `text` cannot carry: [`crate::ui`] paints
    /// a row as a single styled `Line`, and half-block art needs *fg = top
    /// pixel, bg = bottom pixel* on every individual cell. It stays a plain
    /// pixel grid (no ratatui types) for the reason this whole module is
    /// toolkit-free: the registry and the focus ring consume the same list and
    /// read no paint types.
    ///
    /// As with `doc`, [`Element::text`] stays the plaintext of what is painted,
    /// so `get_text` keeps answering for this element.
    pub art: Option<crate::thumbnail::HalfBlockArt>,
    /// The same picture as [`Element::art`], at protocol resolution — the pixels
    /// [`crate::graphics::Painter`] emits when the terminal speaks kitty, iTerm2
    /// or sixel (`apps/tui.md` § Rendering).
    ///
    /// Carried **beside** `art`, never instead of it: the art is the fallback
    /// arm, the eraser for the compositing protocols, and the e2e observable all
    /// at once — [`crate::graphics`] module docs own that claim. So both fields
    /// are populated together and `Element::text` stays the art's plaintext
    /// whatever the terminal turns out to support.
    ///
    /// Like `art`, a plain pixel grid: no ratatui types, no escape sequences.
    /// This module describes *what* is on screen; `graphics` decides how it gets
    /// there.
    pub pixels: Option<crate::thumbnail::Pixels>,
    /// An explicit (foreground, background) colour pair that overrides
    /// terminal-theme inheritance for this element's WHOLE body — set via
    /// [`Element::colors`]. `None` (the default) means "paint with whatever
    /// `Style` the enabled/disabled state computes", i.e. inherit the
    /// terminal theme, same as every element today.
    ///
    /// This is deliberately **not** [`Element::art`]: `art` carries a colour
    /// pair PER CELL (an image, where every pixel differs), while this field
    /// is ONE pair for the entire element — the shape a monochrome QR code
    /// needs. Its first (and so far only) consumer is the identity-export
    /// QR: a QR's module/quiet-zone contrast is part of the spec (dark
    /// modules on a light background) and must not depend on the user's
    /// theme, or an inverted rendering fails to scan (`ui/settings.md` §
    /// Identity export — apple hit exactly this: "dark-on-light is painted
    /// explicitly rather than from theme colors... a theme-inverted QR does
    /// not scan").
    pub colors: Option<(Rgb, Rgb)>,
    /// Automation attributes an element carries beyond its `text` — read by
    /// `get_attr(id, key)` (the agent's [`ElementKind::Attr`](fauna_e2e_agent::ElementKind)
    /// arm). The GUI apps carry the same values on an AT-SPI Description /
    /// UIA HelpText / a `test-attr-*` CSS class (linux
    /// `recipient_picker.rs:191`); the tui carries them inline on the element,
    /// which the per-frame registry copies. The one live consumer today is
    /// `recipient-resolve-status`'s `state` (idle/resolving/resolved/not-found/
    /// error), the value the recipient-picker suite polls to know the resolve
    /// reached a terminal state. Empty for the vast majority of elements.
    pub attrs: Vec<(String, String)>,
    /// Paint this element on the SAME screen line as the inline run it belongs
    /// to, rather than on a line of its own — set via [`Element::inline`].
    ///
    /// Every element painted a full-width line until the month grid needed
    /// seven individually-addressable day cells side by side
    /// (`events-day-cell-{YYYY-MM-DD}`, `ui/events.md` § Layout & flow). Making
    /// each cell its own element is what gives it an id, a gesture and a place
    /// in the focus ring; this flag is what lets seven of them still *look*
    /// like one week row.
    ///
    /// [`crate::ui::element_lines`] groups a run of consecutive `inline`
    /// elements into one `Line` of several spans and records each one's column
    /// band, so `hit_test` resolves a click to the cell under the cursor rather
    /// than to the whole row. [`crate::ui::RowHit`] always carried the column
    /// band — until this, only the page projection threw it away.
    pub inline: bool,
    /// This inline cell **begins a new painted row** rather than continuing the
    /// open run — set via [`Element::starts_row`]. Read only when
    /// [`inline`](Self::inline) is true, which [`Element::starts_row`]
    /// guarantees by setting both.
    ///
    /// Without it an inline run can only be ended by a *non-inline* element,
    /// which takes a full line of its own — so a grid of N rows × M cells had
    /// no way to express its row boundary, and every cell of every row joined
    /// ONE line. That is not hypothetical: the month grid pushed all 42 day
    /// cells as one unbroken run, so weeks 1–6 painted end to end on a single
    /// line and everything past the terminal's width was simply invisible. The
    /// e2e suite could not see it either — the cells were all *registered*, and
    /// the registry is what `is_visible`/`click` read (`crate::element` — "the
    /// element list IS the registry; the viewport clips paint only"), so a
    /// month grid that painted as one clipped strip passed every id assertion.
    pub starts_row: bool,
    /// This [`Role::Button`] **goes somewhere** rather than doing something —
    /// set via [`Element::nav`]. Paint-only: a nav row is a button in every
    /// other respect (focusable, actuable, one gesture), so nothing in the
    /// registry, the automation surface or the focus ring reads this.
    ///
    /// **Why a flag and not a `Role`.** The vocabulary table
    /// (`apps/tui.md` § Rendering → *Control vocabulary*) is one idiom per
    /// control KIND, and nav is not a distinct kind — a rail row and a Copy
    /// button behave identically. What differs is the *promise the paint makes*:
    /// `[ Copy ]` says "this acts now", `Account ▸` says "this takes you
    /// elsewhere". A live user reading the Settings rail had no way to tell the
    /// two apart, because both painted `[ text ]` (the audit's
    /// nav-vs-action-affordance finding, the user's own "navigational?"
    /// question). Making it a `Role` would fork every match arm that today
    /// treats a button as a button, for a difference that is purely visual.
    pub nav: bool,
    /// This nav button goes **back out** rather than onward — set via
    /// [`Element::nav_back`], which guarantees [`nav`](Self::nav) is set too
    /// (a way out is still a destination), exactly as
    /// [`starts_row`](Self::starts_row) guarantees [`inline`](Self::inline).
    ///
    /// **Why the direction needs its own flag.** Rule 4 of the vocabulary table
    /// says a destination must not wear brackets, and a Back button plainly is
    /// one — but `Back ▸` promises *onward*, so applying the rule with the
    /// forward glyph would leave the screen saying the opposite of what the
    /// control does. The arrow carries two promises, "another surface" and
    /// "which way", and only the first is shared with [`nav`](Self::nav).
    ///
    /// It paints leading — `◂ Back` — because the glyph points at where you are
    /// going and you are going back; that is also the idiom every other app
    /// already ships for the same control (priority #3).
    pub nav_back: bool,
    /// This button is a fixed-width **canvas cell**, not a control — set via
    /// [`Element::cell`]. It paints its `text` raw, dropping the `[ ... ]` the
    /// vocabulary otherwise gives a button.
    ///
    /// The month grid's day cells and the week/day time grids' slot cells are
    /// the consumers: each is a padded `MONTH_CELL_WIDTH`-wide box that a click
    /// acts on, so [`Element::clickable`] makes it a `Role::Button` — but its
    /// body is *canvas*, and brackets would both break the column alignment its
    /// paint tests pin and collide with `[15]`, which is how the month grid
    /// marks today.
    ///
    /// **The flag marks the exception, deliberately.** Painting a control as
    /// bare text is a silent failure — it reads as a label, and nothing catches
    /// it (the Events sidebar's `calendar-item` and the agenda's RSVP row sat
    /// that way). Painting a canvas cell as a control is a loud one: the column
    /// alignment moves and the grid's own paint assertion reds, which the
    /// layout rule already requires every such surface to have (`apps/tui.md`
    /// § Rendering — *A page whose LAYOUT is load-bearing asserts the paint*).
    /// So the default is "a button looks like a button" and the grids opt out.
    pub cell: bool,
    /// The element's **second** gesture: what a double-press actuates, when
    /// that is a different action from the single press — set via
    /// [`Element::double_clickable`].
    ///
    /// The month grid is the shape that needs it: `ui/events.md` § Layout &
    /// flow gives one day cell two meanings — single-click drills into Day
    /// view, double-click opens the new-event compose prefilled with that date
    /// (Outlook's model, and Outlook is the calendar UX reference).
    ///
    /// The automation agent's `DoubleClick` arm runs THIS gesture directly, it
    /// never sends two timed clicks: that keeps an e2e latency-independent
    /// (`testing.md` convention 14) while still being honest that the
    /// double-click arm ran rather than aliasing it onto `Click`
    /// (`testing.md` point 11). The wall-clock double-press threshold lives
    /// only on the human mouse path in `main.rs`, where it belongs.
    pub dbl: Option<Gesture>,
    /// The account-data-plane identity of the record whose **body** this
    /// element paints — set via [`Element::observes`], read by
    /// [`crate::observation`] once the shell knows the element actually landed
    /// on screen (`account-data-plane.md` § The replica boundary → T1).
    ///
    /// **In-process only**, like [`nav`](Self::nav): nothing in the registry,
    /// the automation surface or the focus ring reads it, and it deliberately
    /// gets no `attrs` entry — a record CID is not a UI observable, and putting
    /// one there would publish it to every driver that reads attributes.
    ///
    /// **Why it hangs on the element and not on the page's message list.** T1's
    /// trigger is the body being *materialized for display*, and this element
    /// list is the only structure that knows which arm of the bubble ran: a
    /// muted, content-collapsed, blocked, deleted or legally-withheld message
    /// registers no `dm-message-text` at all, so it carries no observation by
    /// construction rather than by a second set of conditions someone has to
    /// keep in step. The remaining half — *did this element land inside the
    /// viewport* — is the shell's, and only the shell has it.
    pub observation: Option<crate::observation::PlaneRecord>,
    /// The post whose dwell this element heads, set via [`Element::cue`] on a
    /// feed card's own `post-card` row and read by the engagement-cue capture
    /// shell ([`crate::feed::cues`]) once it has measured where the card landed.
    ///
    /// **In-process only**, for [`observation`](Self::observation)'s reasons: no
    /// registry, driver or focus-ring reader, and no `attrs` entry.
    ///
    /// **Why the identity rides the element.** The capture measures the card from
    /// the element list the frame actually painted; reading the post id back out
    /// of a second manager snapshot by card index would attribute one post's
    /// dwell to another whenever the window changed between the two reads — the
    /// mid-rebuild misattribution `fauna_feed::CueRow::post_id` forbids.
    pub cue: Option<crate::feed::cues::CueSubject>,
}

impl Element {
    /// Declare that this element paints the body of `record` — the T1
    /// observation the shell reports once the element is really on screen.
    ///
    /// Take care to call this on the element that carries the **body**, never
    /// on a sibling that merely accompanies it: a timestamp row scrolled into
    /// view under an off-screen body is not a body handed to a visible view.
    pub fn observes(mut self, record: crate::observation::PlaneRecord) -> Self {
        self.observation = Some(record);
        self
    }

    /// Declare that this element heads the feed card of `subject` — the row the
    /// engagement-cue capture measures ([`crate::feed::cues`]).
    ///
    /// Call it on the card's own `post-card` row, and only on a card that shows
    /// the post: a muted, blocked or content-collapsed placeholder hides what
    /// the post says, so lingering on it is not an exposure to the post.
    pub fn cue(mut self, subject: crate::feed::cues::CueSubject) -> Self {
        self.cue = Some(subject);
        self
    }
    /// Nest this element (and, transitively, nothing else) under one occurrence
    /// of an indexed container: `Element::label(...).within(ids::POST_CARD, 2)`.
    ///
    /// This is a real positional **scope** — the driver sends
    /// `scope: [{"id":"post-card","index":2}]` and the registry matches on the
    /// path. It is **not** either of the two id-string idioms it is easy to
    /// confuse it with: `dns-provider-row[<slug>]` puts a literal bracket *in
    /// the id*, and `recover-box-item-3` puts a literal dash-index in it. The
    /// three are not interchangeable; registering the wrong one leaves every
    /// shared-test query resolving to nothing.
    pub fn within(mut self, container: &str, index: usize) -> Self {
        self.path.insert(0, (container.to_string(), index));
        self
    }

    /// Give an input its prompt, or a read-only value its name — both paint as
    /// "prompt: value" (a contact field's "Postal code", a MUA row's "IMAP
    /// host"). Paint-only: [`Element::text`] stays the bare value, which is what
    /// the registry hands every driver's `get_text`.
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Force an explicit (fg, bg) colour pair, overriding terminal-theme
    /// inheritance (see [`Element::colors`]). The identity-export QR's
    /// `.colors([0, 0, 0], [255, 255, 255])` (dark-on-light) is the first
    /// consumer.
    pub fn colors(mut self, fg: Rgb, bg: Rgb) -> Self {
        self.colors = Some((fg, bg));
        self
    }

    /// Gate interactivity on a capability the page resolved (a checkbox or
    /// select, which — unlike [`Element::gesture_button`] — take no `enabled`
    /// parameter). A disabled element still PAINTS and still answers `get_text` /
    /// `is_enabled`, but [`crate::automation`] refuses to fire its gesture, so
    /// "the control is visible but not actionable" is a real, testable state
    /// rather than a hint the user can click straight past.
    ///
    /// First consumer: `folder-webdav-toggle` for an actor holding no MSEK,
    /// where a click would commit the nest's `webdav_enabled` flag and only then
    /// fail — the capability makes that state unreachable
    /// (`ui/folders.md` § Element IDs → WebDAV serving).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// The gesture this element actuates, across every actuable [`Role`], or
    /// `None` for an inert one.
    ///
    /// The single door [`crate::app::App::page_elements`]' offline gate reads,
    /// so a control is desensitized because of what it *does*, never because
    /// some list names its id.
    ///
    /// The [`Role::Select`] arm rebuilds the gesture the picker would dispatch
    /// for its **current** value ([`SelectTarget::gesture`] is pure and total).
    /// That is sound for the gate's question specifically: a picker's chosen
    /// value decides the *argument*, never which wire kind it calls — so any
    /// value answers "which kind is this?" identically.
    pub fn gesture(&self) -> Option<Gesture> {
        match &self.role {
            Role::Button(gesture) => Some(gesture.clone()),
            Role::InputCommit { gesture, .. } => Some(gesture.clone()),
            Role::Checkbox { gesture, .. } => Some(gesture.clone()),
            Role::Radio { gesture, .. } => Some(gesture.clone()),
            Role::Select { target, .. } => Some(target.gesture(self.text.clone())),
            Role::Label | Role::Input(_) => None,
        }
    }

    /// Make a **body** element actuable — a picture, a document or a label that
    /// a click acts on, rather than a button whose whole body is its label.
    ///
    /// [`Element::gesture_button`] cannot express this: it builds its own
    /// `text`-only element, so an element whose body is [`art`](Element::art)
    /// (or [`doc`](Element::doc)) had no way to carry a gesture and was
    /// necessarily inert. That is exactly the shape a feed `post-image` needs —
    /// on every other app it IS a button (linux's `build_post_image` returns a
    /// `gtk::Button`, web's `C2paImage` takes an `onclick`), and its click is
    /// what opens `image-lightbox`.
    ///
    /// The same one-builder-away gap [`Element::enabled`] closed for capability
    /// gating: check for it before concluding a "picture you can click" needs a
    /// bespoke element shape.
    pub fn clickable(mut self, gesture: Gesture) -> Self {
        self.role = Role::Button(gesture);
        self
    }

    /// Give this element a **second** gesture, actuated by a double-press (see
    /// [`Element::dbl`]). Additive: an element without one behaves exactly as
    /// before, and the agent refuses `double_click` on it explicitly rather
    /// than silently running the single-click arm.
    ///
    /// Pair it with [`Element::clickable`] when the two presses mean different
    /// things — the month day cell drills into Day view on one press and opens
    /// the prefilled new-event compose on two (`ui/events.md` § Layout & flow).
    pub fn double_clickable(mut self, gesture: Gesture) -> Self {
        self.dbl = Some(gesture);
        self
    }

    /// Paint this element on the same line as the inline run it belongs to
    /// (see [`Element::inline`]) — a horizontal row of individually-addressable
    /// cells, rather than one element per full-width line.
    ///
    /// The element keeps everything that makes it an element: its id, its
    /// gestures, its registry entry and its place in the focus ring. Only the
    /// paint geometry changes, and with it the hit-test band, so a click lands
    /// on the cell under the cursor instead of on the whole row.
    pub fn inline(mut self) -> Self {
        self.inline = true;
        self
    }

    /// Mark this inline cell as the **first of a new painted row** (see
    /// [`Element::starts_row`]) — implies [`inline`](Element::inline), because
    /// a row-starting cell is by definition part of a row.
    ///
    /// Put it on the leading cell of every row of a grid: the week/day time
    /// grids' hour gutter, the month grid's Sunday-or-Monday cell. Everything
    /// after it on that row is a plain [`inline`](Element::inline) cell.
    pub fn starts_row(mut self) -> Self {
        self.inline = true;
        self.starts_row = true;
        self
    }

    /// Mark this button as **navigational** — it paints `text ▸` instead of
    /// `[ text ]` (see [`Element::nav`](Self::nav) the field).
    ///
    /// Use it on anything whose activation lands the user on another surface: a
    /// settings/admin rail row, an in-page link to a sibling page. Do NOT use it
    /// on a button that *does* something on the current page, even if the page
    /// then changes — the distinction the paint promises is "goes somewhere" vs
    /// "acts now", not "the screen updates".
    ///
    /// Button-only: a debug build asserts on misuse, the `display_value` shape.
    pub fn nav(mut self) -> Self {
        debug_assert!(
            matches!(self.role, Role::Button(_)),
            "Element::nav is for Role::Button — {} is not one",
            self.id
        );
        self.nav = true;
        self
    }

    /// Mark this button as the surface's **way out** — it paints `◂ text`
    /// instead of `text ▸` or `[ text ]` (see [`Element::nav_back`](Self::nav_back)
    /// the field), and implies [`nav`](Element::nav).
    ///
    /// Use it on the affordance that returns the user to where they came from:
    /// a sub-page's Back to its rail, an area's Exit to the app. Do NOT use it
    /// on Cancel — abandoning a wizard step is an action with an effect on the
    /// *current* surface, and it keeps its brackets.
    ///
    /// Button-only: a debug build asserts on misuse, the `display_value` shape.
    pub fn nav_back(mut self) -> Self {
        debug_assert!(
            matches!(self.role, Role::Button(_)),
            "Element::nav_back is for Role::Button — {} is not one",
            self.id
        );
        self.nav = true;
        self.nav_back = true;
        self
    }

    /// Mark this button as a fixed-width **canvas cell** — it paints its `text`
    /// raw instead of `[ text ]` (see [`Element::cell`](Self::cell) the field).
    ///
    /// Only a grid whose cells are a painted matrix wants this; a button that
    /// happens to sit in an inline row is still a control and keeps its
    /// brackets. Pair it with [`Element::inline`]/[`Element::starts_row`].
    ///
    /// Button-only: a debug build asserts on misuse, the `display_value` shape.
    pub fn cell(mut self) -> Self {
        debug_assert!(
            matches!(self.role, Role::Button(_)),
            "Element::cell is for Role::Button — {} is not one",
            self.id
        );
        self.cell = true;
        self
    }

    /// The paint-only human form of a [`Role::Select`]'s current value (see
    /// the role's `display` doc) — for pages whose `text` must stay a raw
    /// wire key. A select whose `text` is already human-readable doesn't need
    /// it. Select-only; a debug build asserts on misuse.
    pub fn display_value(mut self, display: impl Into<String>) -> Self {
        if let Role::Select { display: d, .. } = &mut self.role {
            *d = Some(display.into());
        } else {
            debug_assert!(false, "display_value is a Select-only builder");
        }
        self
    }

    /// Attach an automation attribute the agent reads via `get_attr(id, key)`
    /// (see [`Element::attrs`]) — additive, so an element with no attr behaves
    /// exactly as before. `recipient-resolve-status` uses it to carry its
    /// `state` (the resolve terminal-state name the picker suite polls),
    /// matching the AT-SPI/UIA attribute the GUI apps set for the same query.
    pub fn attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attrs.push((key.into(), value.into()));
        self
    }

    pub fn label(id: impl Into<String>, text: impl Into<String>) -> Self {
        Element {
            id: id.into(),
            text: text.into(),
            enabled: true,
            role: Role::Label,
            label: None,
            path: Vec::new(),
            doc: None,
            art: None,
            pixels: None,
            colors: None,
            attrs: Vec::new(),
            inline: false,
            starts_row: false,
            nav: false,
            nav_back: false,
            cell: false,
            dbl: None,
            observation: None,
            cue: None,
        }
    }

    /// Painted text that ui.yaml gives **no element ID** — a feed's "No posts
    /// yet.", a form's help line. It gets pixels and never registers (its empty
    /// id is the signal [`crate::ui::register_frame`] skips on).
    ///
    /// This is the honest way to paint real UI that isn't automatable, and it is
    /// what every other app already does with an untagged label. The
    /// *opposite* — minting a app-specific id like `feed-empty` so a page can
    /// carry its own chrome — is the invented-ID anti-pattern the rules forbid.
    pub fn chrome(text: impl Into<String>) -> Self {
        Element::label(String::new(), text)
    }

    /// [`Self::chrome`]'s **interactive** twin: a real control ui.yaml gives no
    /// element ID. It reaches the focus ring like any other button
    /// ([`Self::focusable`] keys on the role) but never registers
    /// ([`crate::ui::register_frame`] skips the empty id), so a human can use it
    /// and a driver cannot see it.
    ///
    /// **Why this exists rather than a minted id.** In a GTK or DOM client an
    /// untagged button is still mouse-clickable, so "presentation only, no test
    /// id" costs nothing but e2e reach — which is why linux's
    /// `build_content_collapse` and web's `PostCard.svelte` both take it for the
    /// content-policy `collapse` reveal. A TUI has no mouse: the same element
    /// list drives paint, the registry AND the focus ring, so an unregistered
    /// control would be unreachable by the *user*, not merely by a test. This
    /// constructor keeps the sibling clients' ID decision intact without turning
    /// their presentation choice into a dead affordance here.
    ///
    /// Use it only where ui.yaml deliberately scopes no id. Minting a
    /// app-specific one instead is the invented-ID anti-pattern (§ UI
    /// Consistency A).
    pub fn chrome_button(text: impl Into<String>, gesture: Gesture) -> Self {
        Element::gesture_button(String::new(), text, true, gesture)
    }

    /// A read-only element whose body is a [`RenderDocument`] — a post's
    /// `feed-post-text`, a message bubble.
    ///
    /// `text` is the document's plaintext, so the registry (and every driver
    /// `get_text`) reads exactly what the other six apps report, while paint
    /// walks the block tree for real emphasis, lists and code.
    pub fn document(id: impl Into<String>, doc: RenderDocument) -> Self {
        Element {
            id: id.into(),
            text: doc.to_plaintext(),
            enabled: true,
            role: Role::Label,
            label: None,
            path: Vec::new(),
            doc: Some(doc),
            art: None,
            pixels: None,
            colors: None,
            attrs: Vec::new(),
            inline: false,
            starts_row: false,
            nav: false,
            nav_back: false,
            cell: false,
            dbl: None,
            observation: None,
            cue: None,
        }
    }

    /// A read-only element whose body is half-block art — a Media item's
    /// `media-thumbnail`.
    ///
    /// `text` is the art's plaintext (the `▀` glyphs the terminal literally
    /// paints), mirroring [`Element::document`]: the registry and every driver
    /// `get_text` keep reading a real string, while paint styles each cell with
    /// its own two pixels.
    ///
    /// The **absence** of art is not this ctor's business — an item with no
    /// thumbnail, a failed fetch, or one still loading paints
    /// [`crate::thumbnail::PLACEHOLDER`] through the plain
    /// [`Element::label`] ctor, so the element is always registered and one
    /// unreadable thumbnail can never blank the row (`ui/media.md` § Thumbnails).
    ///
    /// Takes the whole [`crate::thumbnail::Thumbnail`] rather than the art alone,
    /// so the two representations of one picture cannot be set apart: `text` is
    /// *always* the art's plaintext and [`Element::pixels`] is *always* the same
    /// picture. A ctor that let a caller pass one without the other is precisely
    /// how a live protocol arm would come to empty the registry text and make the
    /// e2e paint assertion vacuous ([`crate::graphics`] module docs).
    pub fn thumbnail(id: impl Into<String>, thumbnail: crate::thumbnail::Thumbnail) -> Self {
        Element {
            id: id.into(),
            text: thumbnail.art.to_plaintext(),
            enabled: true,
            role: Role::Label,
            label: None,
            path: Vec::new(),
            doc: None,
            art: Some(thumbnail.art),
            pixels: Some(thumbnail.pixels),
            colors: None,
            attrs: Vec::new(),
            inline: false,
            starts_row: false,
            nav: false,
            nav_back: false,
            cell: false,
            dbl: None,
            observation: None,
            cue: None,
        }
    }

    /// A wizard button: `action` is an `OnboardingMachine` mutator.
    pub fn button(
        id: impl Into<String>,
        text: impl Into<String>,
        enabled: bool,
        action: crate::wizard::Action,
    ) -> Self {
        Element::gesture_button(id, text, enabled, Gesture::Wizard(action))
    }

    /// A launch-surface button (`launch_retry`). Always enabled — the surface
    /// only paints a CTA the user may actually take.
    pub fn launch_button(
        id: impl Into<String>,
        text: impl Into<String>,
        action: crate::launch::LaunchAction,
    ) -> Self {
        Element::gesture_button(id, text, true, Gesture::Launch(action))
    }

    /// A sidebar `{page}-tab` row. Enabled always; the focus ring landing on it
    /// *is* the selection, so there is no separate "selected tab" state to drift
    /// from [`crate::app::App::page`].
    pub fn tab(page: Page) -> Self {
        Element::gesture_button(page.tab_id(), page.label(), true, Gesture::Nav(page))
    }

    /// A button behind any [`Gesture`] — the shared constructor the typed ones
    /// above funnel through.
    pub fn gesture_button(
        id: impl Into<String>,
        text: impl Into<String>,
        enabled: bool,
        gesture: Gesture,
    ) -> Self {
        Element {
            id: id.into(),
            text: text.into(),
            enabled,
            role: Role::Button(gesture),
            label: None,
            path: Vec::new(),
            doc: None,
            art: None,
            pixels: None,
            colors: None,
            attrs: Vec::new(),
            inline: false,
            starts_row: false,
            nav: false,
            nav_back: false,
            cell: false,
            dbl: None,
            observation: None,
            cue: None,
        }
    }

    pub fn input(id: impl Into<String>, value: impl Into<String>, field: Field) -> Self {
        Element {
            id: id.into(),
            text: value.into(),
            enabled: true,
            role: Role::Input(field),
            label: None,
            path: Vec::new(),
            doc: None,
            art: None,
            pixels: None,
            colors: None,
            attrs: Vec::new(),
            inline: false,
            starts_row: false,
            nav: false,
            nav_back: false,
            cell: false,
            dbl: None,
            observation: None,
            cue: None,
        }
    }

    /// An [`Role::InputCommit`] — an input whose click/Enter commits `gesture`.
    /// Use when the value has no other commit affordance on its row; a form with
    /// its own save button should stay a plain [`Self::input`].
    pub fn input_commit(
        id: impl Into<String>,
        value: impl Into<String>,
        field: Field,
        gesture: Gesture,
    ) -> Self {
        Element {
            id: id.into(),
            text: value.into(),
            enabled: true,
            role: Role::InputCommit { field, gesture },
            label: None,
            path: Vec::new(),
            doc: None,
            art: None,
            pixels: None,
            colors: None,
            attrs: Vec::new(),
            inline: false,
            starts_row: false,
            nav: false,
            nav_back: false,
            cell: false,
            dbl: None,
            observation: None,
            cue: None,
        }
    }

    pub fn checkbox(
        id: impl Into<String>,
        text: impl Into<String>,
        checked: bool,
        action: crate::wizard::Action,
    ) -> Self {
        Element::checkbox_gesture(id, text, checked, Gesture::Wizard(action))
    }

    /// One option of a mutually-exclusive group ([`Role::Radio`]) behind any
    /// [`Gesture`]. Pair it with the toggle convention's automation mirror at
    /// the call site — `.attr("state", "on"|"off")` (or the page's own
    /// established vocabulary, e.g. the Bluesky rungs' `active`/`inactive`) —
    /// so a driver can read which option is current.
    pub fn radio_gesture(
        id: impl Into<String>,
        text: impl Into<String>,
        selected: bool,
        gesture: Gesture,
    ) -> Self {
        Element {
            id: id.into(),
            text: text.into(),
            enabled: true,
            role: Role::Radio { gesture, selected },
            label: None,
            path: Vec::new(),
            doc: None,
            art: None,
            pixels: None,
            colors: None,
            attrs: Vec::new(),
            inline: false,
            starts_row: false,
            nav: false,
            nav_back: false,
            cell: false,
            dbl: None,
            observation: None,
            cue: None,
        }
    }

    /// A checkbox behind any [`Gesture`] (the feed's rule/factor toggles).
    pub fn checkbox_gesture(
        id: impl Into<String>,
        text: impl Into<String>,
        checked: bool,
        gesture: Gesture,
    ) -> Self {
        Element {
            id: id.into(),
            text: text.into(),
            enabled: true,
            role: Role::Checkbox { gesture, checked },
            label: None,
            path: Vec::new(),
            doc: None,
            art: None,
            pixels: None,
            colors: None,
            attrs: Vec::new(),
            inline: false,
            starts_row: false,
            nav: false,
            nav_back: false,
            cell: false,
            dbl: None,
            observation: None,
            cue: None,
        }
    }

    /// A picker painted as `< value >` (plus a `label` prompt where given —
    /// `prompt: < value >`). `selected` is what `get_text`/`select` round-trip;
    /// pages whose wire value isn't human-readable add
    /// [`display_value`](Self::display_value).
    pub fn select(
        id: impl Into<String>,
        selected: impl Into<String>,
        target: SelectTarget,
        options: Vec<String>,
    ) -> Self {
        Element {
            id: id.into(),
            text: selected.into(),
            enabled: true,
            role: Role::Select {
                target,
                options,
                display: None,
            },
            label: None,
            path: Vec::new(),
            doc: None,
            art: None,
            pixels: None,
            colors: None,
            attrs: Vec::new(),
            inline: false,
            starts_row: false,
            nav: false,
            nav_back: false,
            cell: false,
            dbl: None,
            observation: None,
            cue: None,
        }
    }

    /// Whether the keyboard focus ring stops here (labels are skipped).
    pub fn focusable(&self) -> bool {
        !matches!(self.role, Role::Label)
    }
}
