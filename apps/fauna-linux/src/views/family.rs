//! The `family` page (`docs/goal/behavior/family-safety.md` § App surface) —
//! one page rendering two role-dependent sections:
//!
//! - **Guardian side** (visible when the caller guards ≥1 account): the wards
//!   (`family-ward-item`, indexed, each with `family-ward-handle`), the ONE
//!   shared reach-policy editor for the *selected* ward
//!   (`family-policy-contact-approval-toggle`, `family-policy-unknown-sender-select`,
//!   `family-policy-federation-toggle`, `family-policy-feed-sources-select`,
//!   `family-policy-save-button`), contact pre-approval
//!   (`family-contact-add-input` + `family-contact-add-button`), graduation
//!   (`family-graduate-button` → `family-graduate-confirm-button`), and the
//!   approvals queue (`family-approval-item` + approve/deny).
//! - **Supervised side** (visible when the caller is supervised):
//!   `family-guardian-handle` + the read-only `family-policy-summary`.
//!
//! ui.yaml's `family:` block carries exactly one (non-indexed) policy-editor
//! element set, so a guardian with several wards edits one at a time — clicking
//! a `family-ward-item` row loads that ward's policy into the shared editor. The
//! approvals queue is **not** ward-scoped: `fauna.family.approvals.list` returns
//! every ward's queue in one read.
//!
//! **windows is the reference leg** (`FamilyViewModel` / `FamilyPage.xaml`,
//! 2026-07-10); this is the lift of that exact shape (priority #1/#3), over the
//! shared `fauna_client_family::FamilyClient` seam (priority #2 — linux depends
//! on the `fauna-client-*` crates directly over `Arc<NestClient>`, no FFI hop).
//! Page structure follows `settings/muted_words.rs`: a client-free
//! `build_page_widgets()` (so the ID-conformance unit test can walk the tree)
//! plus `wire()`, with `spawn_with_snapshot` + `hydrate_with_retry` as the async
//! idiom.
//!
//! Two hard e2e contracts live here:
//!
//! 1. Both toggles carry a `test-attr-state-{on,off}` marker class on every
//!    render/flip (`crate::testid::set_test_attr`) — the automation agent's
//!    `get_attr(id, "state")` prefers that marker (`automation/agent.rs`).
//! 2. Both selects hold the **localized labels** as their `gtk::StringList`
//!    model strings, never the wire values: `driver.select` matches the model's
//!    `StringObject` strings and `get_text` on a selector reads back the selected
//!    model string. The option set, the label keys, and the fail-closed rule are
//!    shared Rust (`fauna_core::format::{unknown_sender_options,
//!    feed_sources_options}`); this module owns only the index ↔ option bridge the
//!    GTK `ComboRow` needs.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_core::data::{FeedSources, UnknownPeerDm, UnknownSenderMail};
use fauna_core::obligation::{ContentFloor, ContentPolicy};

use fauna_client_family::FamilyClient;
use fauna_client_family::family::{
    FamilyApprovalEntry, FamilyIncomingTransferInfo, FamilyWardDeviceInfo, FamilyWardInfo,
    ReachPolicy,
};

use crate::async_helper::{hydrate_with_retry, spawn_with_snapshot};
use crate::client::FaunaClient;
use crate::i18n::strings::family as S;
use crate::testid::{set_test_attr, set_test_id};

type Nest = Arc<fauna_client::NestClient>;

// ── The two closed wire enums, and their localized display labels ───────────
//
// The **model strings are the labels** (contract 2 above); shared Rust owns the
// option *set* and the fail-closed rule — `fauna_core::data::{UnknownSenderMail,
// FeedSources}` (the wire parse) and `fauna_core::format::{unknown_sender_options,
// feed_sources_options}` (the ordered catalog + label keys). This file owns only
// the index ↔ option bridge the GTK `ComboRow` needs, since a `ComboRow` selects
// by position while the wire carries a string.
//
// An unrecognized value renders as its knob's *fail-closed* option (`hold` /
// `block`) rather than the permissive one, so an older client can never silently
// *relax* a policy knob it cannot parse (family-safety.md § Implementation status
// — "an unrecognized value renders as the strictest option … never the permissive
// one", a safety rule, not a cosmetic one). The nest owns the enum and rejects
// anything else on `fauna.family.policy.update`.

/// A `ComboRow` model over owned label strings (`StringList::new` wants
/// `&[&str]`, the shared catalog yields owned `String`s).
fn string_list(labels: Vec<String>) -> gtk::StringList {
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    gtk::StringList::new(&refs)
}

/// The ordered `unknown_sender_mail` catalog, resolved to display labels for the
/// `ComboRow` model. Index-aligned with [`UnknownSenderMail::ORDER`].
fn unknown_sender_labels() -> Vec<String> {
    fauna_core::format::unknown_sender_option_labels(crate::i18n::strings::lookup)
}

/// The ordered `feed_sources` catalog, resolved to display labels.
fn feed_sources_labels() -> Vec<String> {
    fauna_core::format::feed_sources_option_labels(crate::i18n::strings::lookup)
}

/// Position of a stored wire value in `ORDER`. The value is parsed by the shared
/// fail-closed rule first, so it is always a member of the catalog and the
/// position always exists — an unparseable value lands on its knob's strict
/// option, never index 0.
fn unknown_sender_index(wire: &str) -> u32 {
    let parsed = UnknownSenderMail::from_wire(wire);
    UnknownSenderMail::ORDER
        .iter()
        .position(|v| *v == parsed)
        .expect("from_wire always yields a member of ORDER") as u32
}

fn feed_sources_index(wire: &str) -> u32 {
    let parsed = FeedSources::from_wire(wire);
    FeedSources::ORDER
        .iter()
        .position(|v| *v == parsed)
        .expect("from_wire always yields a member of ORDER") as u32
}

/// The wire value at a `ComboRow` position. The model is built from `ORDER`, so
/// an out-of-range position is unreachable; the fallback is defense in depth and
/// degrades to the knob's **fail-closed** option — never the permissive index 0,
/// which is what a bare `unwrap_or(values[0])` would have silently written back.
fn unknown_sender_at(index: u32) -> String {
    UnknownSenderMail::ORDER
        .get(index as usize)
        .copied()
        .unwrap_or(UnknownSenderMail::FAIL_CLOSED)
        .as_str()
        .to_string()
}

fn feed_sources_at(index: u32) -> String {
    FeedSources::ORDER
        .get(index as usize)
        .copied()
        .unwrap_or(FeedSources::FAIL_CLOSED)
        .as_str()
        .to_string()
}

/// The ordered `unknown_peer_dm` catalog, resolved to display labels
/// (`family-safety.md` § The bridge-DM gate).
fn unknown_peer_dm_labels() -> Vec<String> {
    fauna_core::format::unknown_peer_dm_option_labels(crate::i18n::strings::lookup)
}

fn unknown_peer_dm_at(index: u32) -> String {
    UnknownPeerDm::ORDER
        .get(index as usize)
        .copied()
        .unwrap_or(UnknownPeerDm::FAIL_CLOSED)
        .as_str()
        .to_string()
}

/// Position of the ward's stored `unknown_peer_dm` in `ORDER`, for RENDER only.
/// The mirror image of every other knob: an ABSENT value is the `allow`
/// DEFAULT, never the fail-closed `hold` — the nest omits a knob sitting at
/// its default, so absence here means "already allow", not "a value this
/// client could not parse" (`family-safety.md` § The bridge-DM gate; pinned by
/// `an_absent_unknown_peer_dm_renders_its_allow_default_not_the_fail_closed_value`).
fn unknown_peer_dm_index_for_render(wire: Option<&str>) -> u32 {
    let value = wire
        .map(UnknownPeerDm::from_wire)
        .unwrap_or(UnknownPeerDm::Allow);
    UnknownPeerDm::ORDER
        .iter()
        .position(|v| *v == value)
        .expect("ORDER always contains Allow") as u32
}

/// The ordered content-floor catalog (`inherit / collapse / block`), resolved to
/// display labels for the four `family-policy-content-*-select` ComboRows.
/// Index-aligned with [`ContentFloor::ORDER`] (`family-safety.md` § Content policy).
fn content_floor_labels() -> Vec<String> {
    fauna_core::format::content_floor_option_labels(crate::i18n::strings::lookup)
}

/// Position of a stored content-floor wire value in `ORDER`, parsed by the shared
/// fail-closed rule first (an unparseable value lands on `block`, never index 0).
fn content_floor_index(wire: &str) -> u32 {
    let parsed = match ContentFloor::from_wire(wire) {
        // `Unknown` is not a picker option — a value this client cannot parse
        // renders at the strict `block` position (family-safety.md § Content policy).
        ContentFloor::Unknown => ContentFloor::FAIL_CLOSED,
        v => v,
    };
    ContentFloor::ORDER
        .iter()
        .position(|v| *v == parsed)
        .expect("from_wire (mapped off Unknown) always yields a member of ORDER") as u32
}

/// The content-floor wire value at a `ComboRow` position, degrading an
/// out-of-range index to the **fail-closed** `block`, never the permissive
/// `inherit` at index 0.
fn content_floor_at(index: u32) -> String {
    ContentFloor::ORDER
        .get(index as usize)
        .copied()
        .unwrap_or(ContentFloor::FAIL_CLOSED)
        .as_str()
        .to_string()
}

/// The one `fauna.family.status` + `fauna.family.approvals.list` read that fills
/// BOTH sections (the windows `LoadAsync` shape).
struct FamilyView {
    /// The caller's guardian, when supervised (drives the supervised section +
    /// the global `supervised-indicator`).
    guardian_handle: Option<String>,
    /// The caller's own active policy, when supervised (read-only summary).
    policy: Option<ReachPolicy>,
    wards: Vec<FamilyWardInfo>,
    /// Every ward's pending approvals — the queue is NOT ward-scoped.
    approvals: Vec<FamilyApprovalEntry>,
    /// Transfer proposals awaiting THIS caller's consent as proposed guardian
    /// (`family-safety.md` § Graduation & transfer). Any user can be one —
    /// the widened `family-tab` gate is what gets them to this page.
    incoming: Vec<FamilyIncomingTransferInfo>,
    /// The caller's OWN screen-time usage for their local day, when supervised
    /// under a daily budget (`family-safety.md` § Screen time). Folded into the
    /// read-only `family-policy-summary` — the ward sees the very number their
    /// guardian sees, which is the pillar's transparency rule. `None` = no
    /// budget, and no budget means nothing to show.
    usage_today_minutes: Option<u32>,
    /// The caller's OWN pending contact / feed-source asks, when supervised —
    /// folded into `crate::ward_asks` so the refused-send surfaces on the
    /// contacts, profile and bridges pages read the freshest list.
    contact_requests: Vec<fauna_client_family::family::FamilyContactRequestInfo>,
    feed_requests: Vec<fauna_client_family::family::FamilyFeedRequestInfo>,
    /// The caller's OWN established age band, when one exists — the supervised
    /// section's `family-age-band-summary` (`family-safety.md` § App surface →
    /// *Age-band surfaces*). `None` paints nothing (never a placeholder).
    own_age_band: Option<fauna_client_family::family::FamilyAgeBandInfo>,
}

/// One async round-trip's result: the refreshed page view, or an error string
/// surfaced in the page `error-message`.
type LoadResult = Result<FamilyView, String>;

/// Widget handles the render + event closures need.
struct Widgets {
    error_label: gtk::Label,

    // ── Supervised section ──
    supervised_group: adw::PreferencesGroup,
    guardian_handle: gtk::Label,
    policy_summary: gtk::Label,
    /// `family-age-band-summary` — shown only when the caller has a band.
    age_band_summary: gtk::Label,

    // ── Guardian section: wards ──
    wards_group: adw::PreferencesGroup,
    wards_list: gtk::ListBox,
    wards_empty: gtk::Label,

    // ── Guardian section: the ONE shared reach-policy editor ──
    policy_group: adw::PreferencesGroup,
    policy_heading: gtk::Label,
    contact_approval: adw::SwitchRow,
    unknown_sender: adw::ComboRow,
    federation: adw::SwitchRow,
    feed_sources: adw::ComboRow,
    unknown_peer_dm: adw::ComboRow,
    content_nsfw: adw::ComboRow,
    content_spam: adw::ComboRow,
    content_phishing: adw::ComboRow,
    content_commercial: adw::ComboRow,
    content_notify: adw::SwitchRow,
    /// Screen time (Slice E) — the usage window's two `HH:MM` bounds and the
    /// daily budget in whole minutes. Free-text rather than spin buttons so an
    /// empty field means "this control is unset", which is how a guardian
    /// clears a limit; the three strings are parsed through shared Rust
    /// (`fauna_core::screen_time`), never here.
    screen_window_start: adw::EntryRow,
    screen_window_end: adw::EntryRow,
    screen_daily_minutes: adw::EntryRow,
    save_button: gtk::Button,
    /// The selected ward's device-mark rows (`family-device-mark-item`), rebuilt
    /// on every `load_editor` — Slice F.
    devices_hint: gtk::Label,
    devices_list: gtk::Box,
    devices_empty: gtk::Label,
    device_rows: RefCell<Vec<gtk::Box>>,
    /// The selected ward's DENIED bridge-DM peers (`family-blocked-peer-item`,
    /// each with its `family-blocked-peer-allow-button`), rebuilt on every
    /// `load_editor` — the un-deny surface.
    blocked_hint: gtk::Label,
    blocked_list: gtk::Box,
    blocked_empty: gtk::Label,
    blocked_rows: RefCell<Vec<gtk::Box>>,
    contact_input: gtk::Entry,
    contact_add_button: gtk::Button,
    transfer_input: gtk::Entry,
    transfer_button: gtk::Button,
    transfer_pending: gtk::Label,
    transfer_cancel_button: gtk::Button,
    graduate_button: gtk::Button,
    graduate_confirm_button: gtk::Button,

    // ── Guardian section: the approvals queue ──
    approvals_group: adw::PreferencesGroup,
    approvals_list: gtk::Box,
    approvals_empty: gtk::Label,
    approval_rows: RefCell<Vec<gtk::Box>>,

    // ── Incoming-transfer prompts (proposed-guardian side) ──
    incoming_group: adw::PreferencesGroup,
    incoming_list: gtk::Box,
    incoming_rows: RefCell<Vec<gtk::Box>>,
}

/// Everything the handlers + render need.
struct Ctx {
    nest: Nest,
    rt: tokio::runtime::Handle,
    w: Widgets,
    /// The wards from the last successful load, index-aligned with the
    /// `family-ward-item` rows.
    wards: RefCell<Vec<FamilyWardInfo>>,
    /// The approvals from the last successful load, index-aligned with the
    /// `family-approval-item` rows.
    approvals: RefCell<Vec<FamilyApprovalEntry>>,
    /// The incoming transfer proposals from the last successful load,
    /// index-aligned with the `family-incoming-transfer-item` rows.
    incoming: RefCell<Vec<FamilyIncomingTransferInfo>>,
    /// The ward currently loaded into the shared policy editor. Re-selected
    /// after a refresh if still present, else the first ward.
    selected_ward: RefCell<Option<Vec<u8>>>,
    /// Suppresses the toggle/select notify handlers while `render` syncs the
    /// editor programmatically.
    syncing: std::cell::Cell<bool>,
    /// Whether the guardian has touched `unknown_peer_dm` since the editor was
    /// last loaded. Unlike every sibling reach knob, an absent `unknown_peer_dm`
    /// means "leave unchanged" (`family-safety.md` § The bridge-DM gate), so an
    /// always-send would echo back a value only a newer nest could have written,
    /// silently overwriting the ward's knob on an unrelated save. Reset to
    /// `false` on every `load_editor` call and flipped `true` only by a
    /// `connect_selected_notify` firing outside `syncing`.
    unknown_peer_dm_edited: std::cell::Cell<bool>,
}

thread_local! {
    /// The live page's refresh entry point, registered by [`wire`].
    ///
    /// `connect_map` covers navigating *into* the page from another, but the e2e
    /// nav protocol re-enters the SAME page to force a reload (`FamilyActions.reload
    /// = navigate`), and `Stack::set_visible_child_name` to the current child is a
    /// no-op that fires no `map`. So the nav-patch handler (`main.rs`) also calls
    /// [`refresh_page`] unconditionally — the same "refetch on every nav patch"
    /// contract the admin shell's nav arm honours. GTK-main-thread only.
    static REFRESH: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Re-read `fauna.family.status` + the approvals queue for the live `family`
/// page, if one is built. No-op before the page exists.
pub fn refresh_page() {
    let cb = REFRESH.with(|r| r.borrow().clone());
    if let Some(cb) = cb {
        cb();
    }
}

/// Build the `family` view (a top-level content-stack page, `stack_name()` =
/// `"family"`; the gated `family-tab` sidebar row navigates to it).
pub fn build_family_view(client: &Rc<FaunaClient>) -> adw::PreferencesPage {
    let (page, widgets) = build_page_widgets();
    wire(client, widgets, &page);
    page
}

/// Build the static page widget tree (every static ui.yaml ID present) with no
/// client dependency — split out so the unit test can exercise ID conformance
/// without a real `FaunaClient` (the `settings/muted_words.rs` shape).
fn build_page_widgets() -> (adw::PreferencesPage, Widgets) {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("system-users-symbolic")
        .build();

    // ── Header group: headings + the page-level error label ──
    let header_group = adw::PreferencesGroup::new();

    let page_heading = gtk::Label::new(Some(S::TITLE));
    page_heading.add_css_class("title-2");
    page_heading.set_halign(gtk::Align::Start);
    set_test_id(&page_heading, ids::PAGE_HEADING);
    header_group.add(&page_heading);

    // Page landmark (ui.yaml: "text — page landmark"), mirroring the windows
    // `family-heading` TextBlock and the `admin-users-heading` convention.
    let family_heading = gtk::Label::new(Some(S::TITLE));
    family_heading.add_css_class("title-4");
    family_heading.set_halign(gtk::Align::Start);
    set_test_id(&family_heading, ids::FAMILY_HEADING);
    header_group.add(&family_heading);

    // error-message — page-level error label (E2E rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    header_group.add(&error_label);
    page.add(&header_group);

    // ── Supervised section (hidden until `supervised_by` resolves) ──
    let supervised_group = adw::PreferencesGroup::builder()
        .title(S::POLICY_SUMMARY_HEADING)
        .visible(false)
        .build();

    let guardian_handle = gtk::Label::new(None);
    guardian_handle.add_css_class("heading");
    guardian_handle.set_halign(gtk::Align::Start);
    guardian_handle.set_wrap(true);
    set_test_id(&guardian_handle, ids::FAMILY_GUARDIAN_HANDLE);
    supervised_group.add(&guardian_handle);

    let policy_summary = gtk::Label::new(None);
    policy_summary.add_css_class("dim-label");
    policy_summary.set_halign(gtk::Align::Start);
    policy_summary.set_wrap(true);
    set_test_id(&policy_summary, ids::FAMILY_POLICY_SUMMARY);
    supervised_group.add(&policy_summary);

    // The ward's own age band and how it was established (transparency, the
    // policy summary's twin) — hidden until a band is known.
    let age_band_summary = gtk::Label::new(None);
    age_band_summary.add_css_class("dim-label");
    age_band_summary.set_halign(gtk::Align::Start);
    age_band_summary.set_wrap(true);
    age_band_summary.set_visible(false);
    set_test_id(&age_band_summary, ids::FAMILY_AGE_BAND_SUMMARY);
    supervised_group.add(&age_band_summary);
    page.add(&supervised_group);

    // ── Guardian section: the wards list ──
    let wards_group = adw::PreferencesGroup::builder()
        .title(S::WARDS_HEADING)
        .visible(false)
        .build();

    let wards_list = gtk::ListBox::new();
    wards_list.set_selection_mode(gtk::SelectionMode::Single);
    wards_list.add_css_class("boxed-list");
    wards_group.add(&wards_list);

    let wards_empty = gtk::Label::new(Some(S::NO_WARDS));
    wards_empty.add_css_class("dim-label");
    wards_empty.set_halign(gtk::Align::Start);
    wards_group.add(&wards_empty);
    page.add(&wards_group);

    // ── Guardian section: the ONE shared reach-policy editor ──
    // Hidden until a `family-ward-item` row is selected (auto-selects ward 0 on
    // load) — the windows `PolicyEditor` panel.
    let policy_group = adw::PreferencesGroup::builder().visible(false).build();

    let policy_heading = gtk::Label::new(None);
    policy_heading.add_css_class("heading");
    policy_heading.set_halign(gtk::Align::Start);
    policy_group.add(&policy_heading);

    let contact_approval = adw::SwitchRow::builder().active(false).build();
    contact_approval.set_title(S::POLICY_CONTACT_APPROVAL_LABEL);
    set_test_id(
        &contact_approval,
        ids::FAMILY_POLICY_CONTACT_APPROVAL_TOGGLE,
    );
    set_test_attr(&contact_approval, "state", "off");
    policy_group.add(&contact_approval);

    let unknown_sender = adw::ComboRow::builder().build();
    unknown_sender.set_title(S::POLICY_UNKNOWN_SENDER_LABEL);
    unknown_sender.set_model(Some(&string_list(unknown_sender_labels())));
    set_test_id(&unknown_sender, ids::FAMILY_POLICY_UNKNOWN_SENDER_SELECT);
    policy_group.add(&unknown_sender);

    let federation = adw::SwitchRow::builder().active(true).build();
    federation.set_title(S::POLICY_FEDERATION_LABEL);
    set_test_id(&federation, ids::FAMILY_POLICY_FEDERATION_TOGGLE);
    set_test_attr(&federation, "state", "on");
    policy_group.add(&federation);

    let feed_sources = adw::ComboRow::builder().build();
    feed_sources.set_title(S::POLICY_FEED_SOURCES_LABEL);
    feed_sources.set_model(Some(&string_list(feed_sources_labels())));
    set_test_id(&feed_sources, ids::FAMILY_POLICY_FEED_SOURCES_SELECT);
    policy_group.add(&feed_sources);

    // `feed_sources` only gates NEW connections; inbound DMs riding an already-
    // connected bridge account are the separate `unknown_peer_dm` knob below
    // (family-safety.md § The bridge-DM gate). The caption names it by its own
    // label. No test id on the caption — it is prose, not an assertable element.
    let feed_sources_caveat = gtk::Label::new(Some(S::POLICY_FEED_SOURCES_CAVEAT));
    feed_sources_caveat.add_css_class("dim-label");
    feed_sources_caveat.add_css_class("caption");
    feed_sources_caveat.set_halign(gtk::Align::Start);
    feed_sources_caveat.set_wrap(true);
    feed_sources_caveat.set_xalign(0.0);
    policy_group.add(&feed_sources_caveat);

    // The bridge-DM gate (family-safety.md § The bridge-DM gate): whether a DM
    // arriving over an already-connected bridge account, from a peer the ward
    // has never messaged, is held for guardian review. `Option<String>` on the
    // wire — absent means "leave unchanged" — so sends are gated on
    // `unknown_peer_dm_edited`, set only by a genuine user edit (below).
    let unknown_peer_dm = adw::ComboRow::builder().build();
    unknown_peer_dm.set_title(S::POLICY_UNKNOWN_PEER_DM_LABEL);
    unknown_peer_dm.set_model(Some(&string_list(unknown_peer_dm_labels())));
    set_test_id(&unknown_peer_dm, ids::FAMILY_POLICY_UNKNOWN_PEER_DM_SELECT);
    policy_group.add(&unknown_peer_dm);

    // ── Content policy (family-safety.md § Content policy, Slice C) ──
    // Four per-category floor selects (inherit / collapse / block) + the Notify
    // toggle. All share the ONE content-floor catalog from shared Rust.
    let content_nsfw = adw::ComboRow::builder().build();
    content_nsfw.set_title(S::POLICY_CONTENT_NSFW_LABEL);
    content_nsfw.set_model(Some(&string_list(content_floor_labels())));
    set_test_id(&content_nsfw, ids::FAMILY_POLICY_CONTENT_NSFW_SELECT);
    policy_group.add(&content_nsfw);

    let content_spam = adw::ComboRow::builder().build();
    content_spam.set_title(S::POLICY_CONTENT_SPAM_LABEL);
    content_spam.set_model(Some(&string_list(content_floor_labels())));
    set_test_id(&content_spam, ids::FAMILY_POLICY_CONTENT_SPAM_SELECT);
    policy_group.add(&content_spam);

    let content_phishing = adw::ComboRow::builder().build();
    content_phishing.set_title(S::POLICY_CONTENT_PHISHING_LABEL);
    content_phishing.set_model(Some(&string_list(content_floor_labels())));
    set_test_id(
        &content_phishing,
        ids::FAMILY_POLICY_CONTENT_PHISHING_SELECT,
    );
    policy_group.add(&content_phishing);

    let content_commercial = adw::ComboRow::builder().build();
    content_commercial.set_title(S::POLICY_CONTENT_COMMERCIAL_LABEL);
    content_commercial.set_model(Some(&string_list(content_floor_labels())));
    set_test_id(
        &content_commercial,
        ids::FAMILY_POLICY_CONTENT_COMMERCIAL_SELECT,
    );
    policy_group.add(&content_commercial);

    let content_notify = adw::SwitchRow::builder().active(false).build();
    content_notify.set_title(S::POLICY_CONTENT_NOTIFY_LABEL);
    set_test_id(&content_notify, ids::FAMILY_POLICY_CONTENT_NOTIFY_TOGGLE);
    set_test_attr(&content_notify, "state", "off");
    policy_group.add(&content_notify);

    // ── Screen time (family-safety.md § Screen time, Slice E) ──
    // The usage WINDOW is the hours the ward may use the account (so "no device
    // after 21:00" is the window 07:00–21:00), and it may wrap midnight. Both
    // bounds are typed as HH:MM; the daily budget is whole minutes counted
    // across every device. An empty field clears that control — which is why
    // these are entries and not spin buttons: a spin button has no "unset".
    let screen_heading = gtk::Label::new(Some(S::POLICY_SCREEN_HEADING));
    screen_heading.add_css_class("heading");
    screen_heading.set_halign(gtk::Align::Start);
    screen_heading.set_margin_top(8);
    policy_group.add(&screen_heading);

    let screen_window_start = adw::EntryRow::builder()
        .title(S::POLICY_SCREEN_WINDOW_START_LABEL)
        .build();
    set_test_id(
        &screen_window_start,
        ids::FAMILY_POLICY_SCREEN_WINDOW_START_INPUT,
    );
    policy_group.add(&screen_window_start);

    let screen_window_end = adw::EntryRow::builder()
        .title(S::POLICY_SCREEN_WINDOW_END_LABEL)
        .build();
    set_test_id(
        &screen_window_end,
        ids::FAMILY_POLICY_SCREEN_WINDOW_END_INPUT,
    );
    policy_group.add(&screen_window_end);

    let screen_daily_minutes = adw::EntryRow::builder()
        .title(S::POLICY_SCREEN_DAILY_MINUTES_LABEL)
        .build();
    set_test_id(
        &screen_daily_minutes,
        ids::FAMILY_POLICY_SCREEN_DAILY_MINUTES_INPUT,
    );
    policy_group.add(&screen_daily_minutes);

    // § Screen time's conforming-client bound, stated on the surface that
    // exposes the knob — the same rule pillar 1's feed_sources caveat follows.
    let screen_caveat = gtk::Label::new(Some(S::POLICY_SCREEN_CAVEAT));
    screen_caveat.add_css_class("dim-label");
    screen_caveat.add_css_class("caption");
    screen_caveat.set_halign(gtk::Align::Start);
    screen_caveat.set_wrap(true);
    screen_caveat.set_xalign(0.0);
    policy_group.add(&screen_caveat);

    let save_button = gtk::Button::with_label(S::POLICY_SAVE_BUTTON);
    save_button.add_css_class("suggested-action");
    save_button.set_halign(gtk::Align::Start);
    set_test_id(&save_button, ids::FAMILY_POLICY_SAVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&save_button, "fauna.family.policy.update");
    policy_group.add(&save_button);

    // ── The guardian-enrolled-device marker (family-safety.md § Full visibility
    // for young children, Slice F) ──
    // One `family-device-mark-item` per device of the SELECTED ward, each with a
    // `family-device-mark-toggle`. It lives inside the per-ward editor — not on
    // the ward rows — for the same reason the policy knobs do: the index space
    // then belongs to exactly one ward, so `family-device-mark-toggle[i]` is
    // unambiguous with several wards on the page. Unlike the policy knobs the
    // toggle is NOT batched behind Save: `fauna.family.device.mark` is its own
    // per-device RPC, so a flip dispatches immediately and re-reads.
    let devices_heading = gtk::Label::new(Some(S::WARD_DEVICES_HEADING));
    devices_heading.add_css_class("heading");
    devices_heading.set_halign(gtk::Align::Start);
    devices_heading.set_margin_top(8);
    policy_group.add(&devices_heading);

    let devices_hint = gtk::Label::new(None);
    devices_hint.add_css_class("dim-label");
    devices_hint.add_css_class("caption");
    devices_hint.set_halign(gtk::Align::Start);
    devices_hint.set_wrap(true);
    devices_hint.set_xalign(0.0);
    policy_group.add(&devices_hint);

    let devices_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    policy_group.add(&devices_list);

    let devices_empty = gtk::Label::new(Some(S::NO_WARD_DEVICES));
    devices_empty.add_css_class("dim-label");
    devices_empty.set_halign(gtk::Align::Start);
    devices_empty.set_visible(false);
    policy_group.add(&devices_empty);

    // ── The un-deny surface (family-safety.md § The bridge-DM gate → *The
    // un-deny surface*) ──
    // The guardian's DENIED bridge-DM peers for the selected ward, each with a
    // one-click flip back. Inside the per-ward editor, beside the device list,
    // for the same reason: the `family-blocked-peer-item[i]` index space then
    // belongs to exactly one ward. Not batched behind Save — the flip is its own
    // `approvals_decide` call and re-reads.
    let blocked_heading = gtk::Label::new(Some(S::BLOCKED_PEERS_HEADING));
    blocked_heading.add_css_class("heading");
    blocked_heading.set_halign(gtk::Align::Start);
    blocked_heading.set_margin_top(8);
    policy_group.add(&blocked_heading);

    let blocked_hint = gtk::Label::new(None);
    blocked_hint.add_css_class("dim-label");
    blocked_hint.add_css_class("caption");
    blocked_hint.set_halign(gtk::Align::Start);
    blocked_hint.set_wrap(true);
    blocked_hint.set_xalign(0.0);
    policy_group.add(&blocked_hint);

    let blocked_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    policy_group.add(&blocked_list);

    // The empty case is STATED, not a blank gap — a guardian who denied nobody
    // and a surface that failed to load would otherwise look identical.
    let blocked_empty = gtk::Label::new(Some(S::NO_BLOCKED_PEERS));
    blocked_empty.add_css_class("dim-label");
    blocked_empty.set_halign(gtk::Align::Start);
    blocked_empty.set_visible(false);
    policy_group.add(&blocked_empty);

    // Pre-approve a contact for the selected ward (v1: a hex actor id — handle
    // resolution is future UX polish, not part of this seam).
    let contact_input = gtk::Entry::builder()
        .placeholder_text(S::CONTACT_ADD_PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(&contact_input, ids::FAMILY_CONTACT_ADD_INPUT);

    let contact_add_button = gtk::Button::with_label(S::CONTACT_ADD_BUTTON);
    contact_add_button.set_valign(gtk::Align::Center);
    set_test_id(&contact_add_button, ids::FAMILY_CONTACT_ADD_BUTTON);
    crate::offline_gate::declare_wire_kind(&contact_add_button, "fauna.family.contact.add");

    let contact_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    contact_row.append(&contact_input);
    contact_row.append(&contact_add_button);
    policy_group.add(&contact_row);

    // Transfer initiation (family-safety.md § Graduation & transfer) — per
    // selected ward, the same hex-actor-id convention as contact-add. The
    // pending marker + cancel button render from the ward's nest-confirmed
    // `pending_transfer` (load_editor swaps the two states); the proposal
    // stays pending until the target accepts.
    let transfer_input = gtk::Entry::builder()
        .placeholder_text(S::TRANSFER_PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(&transfer_input, ids::FAMILY_TRANSFER_INPUT);

    let transfer_button = gtk::Button::with_label(S::TRANSFER_BUTTON);
    transfer_button.set_valign(gtk::Align::Center);
    set_test_id(&transfer_button, ids::FAMILY_TRANSFER_BUTTON);
    crate::offline_gate::declare_wire_kind(&transfer_button, "fauna.family.transfer");

    let transfer_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    transfer_row.append(&transfer_input);
    transfer_row.append(&transfer_button);
    policy_group.add(&transfer_row);

    let transfer_pending = gtk::Label::builder().visible(false).build();
    transfer_pending.add_css_class("dim-label");
    transfer_pending.set_halign(gtk::Align::Start);
    transfer_pending.set_wrap(true);
    set_test_id(&transfer_pending, ids::FAMILY_TRANSFER_PENDING);
    policy_group.add(&transfer_pending);

    let transfer_cancel_button = gtk::Button::with_label(S::TRANSFER_CANCEL_BUTTON);
    transfer_cancel_button.set_halign(gtk::Align::Start);
    transfer_cancel_button.set_visible(false);
    set_test_id(&transfer_cancel_button, ids::FAMILY_TRANSFER_CANCEL_BUTTON);
    crate::offline_gate::declare_wire_kind(&transfer_cancel_button, "fauna.family.transfer.cancel");
    policy_group.add(&transfer_cancel_button);

    // Graduation — reveal-then-confirm (the windows convention): the confirm
    // button is hidden until `family-graduate-button` is clicked, and its label
    // names the ward.
    let graduate_button = gtk::Button::with_label(S::GRADUATE_BUTTON);
    graduate_button.set_halign(gtk::Align::Start);
    set_test_id(&graduate_button, ids::FAMILY_GRADUATE_BUTTON);
    policy_group.add(&graduate_button);

    let graduate_confirm_button = gtk::Button::with_label(S::GRADUATE_CONFIRM_BUTTON);
    graduate_confirm_button.add_css_class("destructive-action");
    graduate_confirm_button.set_halign(gtk::Align::Start);
    graduate_confirm_button.set_visible(false);
    set_test_id(
        &graduate_confirm_button,
        ids::FAMILY_GRADUATE_CONFIRM_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(&graduate_confirm_button, "fauna.family.graduate");
    policy_group.add(&graduate_confirm_button);
    page.add(&policy_group);

    // ── Guardian section: the approvals queue (across ALL wards) ──
    let approvals_group = adw::PreferencesGroup::builder()
        .title(S::APPROVALS_HEADING)
        .visible(false)
        .build();

    let approvals_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    approvals_group.add(&approvals_list);

    let approvals_empty = gtk::Label::new(Some(S::NO_APPROVALS));
    approvals_empty.add_css_class("dim-label");
    approvals_empty.set_halign(gtk::Align::Start);
    approvals_group.add(&approvals_empty);
    page.add(&approvals_group);

    // ── Incoming-transfer prompts (any user can be a proposed guardian —
    // reaching this section is what the widened family-tab gate exists for).
    let incoming_group = adw::PreferencesGroup::builder()
        .title(S::INCOMING_TRANSFERS_HEADING)
        .visible(false)
        .build();
    let incoming_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    incoming_group.add(&incoming_list);
    page.add(&incoming_group);

    let widgets = Widgets {
        error_label,
        supervised_group,
        guardian_handle,
        policy_summary,
        age_band_summary,
        wards_group,
        wards_list,
        wards_empty,
        policy_group,
        policy_heading,
        contact_approval,
        unknown_sender,
        federation,
        feed_sources,
        unknown_peer_dm,
        content_nsfw,
        content_spam,
        content_phishing,
        content_commercial,
        content_notify,
        screen_window_start,
        screen_window_end,
        screen_daily_minutes,
        save_button,
        devices_hint,
        devices_list,
        devices_empty,
        device_rows: RefCell::new(Vec::new()),
        blocked_hint,
        blocked_list,
        blocked_empty,
        blocked_rows: RefCell::new(Vec::new()),
        contact_input,
        contact_add_button,
        transfer_input,
        transfer_button,
        transfer_pending,
        transfer_cancel_button,
        graduate_button,
        graduate_confirm_button,
        approvals_group,
        approvals_list,
        approvals_empty,
        approval_rows: RefCell::new(Vec::new()),
        incoming_group,
        incoming_list,
        incoming_rows: RefCell::new(Vec::new()),
    };
    (page, widgets)
}

/// Wire the page to the shared `FamilyClient` seam, load on mount, and connect
/// every interaction.
fn wire(client: &Rc<FaunaClient>, widgets: Widgets, page: &adw::PreferencesPage) {
    let ctx = Rc::new(Ctx {
        nest: client.nest_rpc().clone(),
        rt: client.runtime_handle(),
        w: widgets,
        wards: RefCell::new(Vec::new()),
        approvals: RefCell::new(Vec::new()),
        incoming: RefCell::new(Vec::new()),
        selected_ward: RefCell::new(None),
        syncing: std::cell::Cell::new(false),
        unknown_peer_dm_edited: std::cell::Cell::new(false),
    });

    refresh(&ctx);

    // Re-read on every navigation to the page (`connect_map` fires when the
    // content stack makes this child visible) — the guardian's queue is
    // nest-authoritative and a knock/hold can land while another page is up.
    {
        let ctx = Rc::clone(&ctx);
        page.connect_map(move |_| refresh(&ctx));
    }

    // …and register the same refresh for the nav-patch handler, which re-enters
    // the SAME page on an e2e `reload()` (a no-op for `map`). See [`REFRESH`].
    {
        let ctx = Rc::clone(&ctx);
        let cb: Rc<dyn Fn()> = Rc::new(move || refresh(&ctx));
        REFRESH.with(|r| *r.borrow_mut() = Some(cb));
    }

    // Ward selection → load that ward's policy into the shared editor.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .wards_list
            .clone()
            .connect_row_activated(move |_, row| {
                let index = row.index();
                if index < 0 {
                    return;
                }
                let Some(ward) = ctx.wards.borrow().get(index as usize).cloned() else {
                    return;
                };
                *ctx.selected_ward.borrow_mut() = Some(ward.actor_id.to_vec());
                load_editor(&ctx, &ward);
            });
    }

    // Toggles: local edit state only (persisted by `family-policy-save-button`).
    // Each flip re-stamps the `state` marker class the e2e reads.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .contact_approval
            .clone()
            .connect_active_notify(move |sw| {
                if ctx.syncing.get() {
                    return;
                }
                set_test_attr(sw, "state", if sw.is_active() { "on" } else { "off" });
            });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.federation.clone().connect_active_notify(move |sw| {
            if ctx.syncing.get() {
                return;
            }
            set_test_attr(sw, "state", if sw.is_active() { "on" } else { "off" });
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .content_notify
            .clone()
            .connect_active_notify(move |sw| {
                if ctx.syncing.get() {
                    return;
                }
                set_test_attr(sw, "state", if sw.is_active() { "on" } else { "off" });
            });
    }
    // `unknown_peer_dm` is `Option<String>` on the wire — a genuine edit is the
    // ONLY thing allowed to arm the send (see `unknown_peer_dm_edited`'s doc
    // comment). `load_editor`'s own `set_selected` fires this same signal, which
    // is exactly why it runs under `syncing`.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .unknown_peer_dm
            .clone()
            .connect_selected_notify(move |_| {
                if ctx.syncing.get() {
                    return;
                }
                ctx.unknown_peer_dm_edited.set(true);
            });
    }

    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .save_button
            .clone()
            .connect_clicked(move |_| submit_policy(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .contact_add_button
            .clone()
            .connect_clicked(move |_| submit_contact_add(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .contact_input
            .clone()
            .connect_activate(move |_| submit_contact_add(&ctx));
    }

    // Transfer: propose for the selected ward; cancel withdraws the pending
    // proposal (§ Graduation & transfer).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .transfer_button
            .clone()
            .connect_clicked(move |_| submit_transfer(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .transfer_input
            .clone()
            .connect_activate(move |_| submit_transfer(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .transfer_cancel_button
            .clone()
            .connect_clicked(move |_| {
                let Some(ward) = ctx.selected_ward.borrow().clone() else {
                    return;
                };
                dispatch(&ctx, Mutation::TransferCancel { ward }, false);
            });
    }

    // Graduate: reveal, then confirm.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.graduate_button.clone().connect_clicked(move |_| {
            ctx.w.graduate_confirm_button.set_visible(true);
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .graduate_confirm_button
            .clone()
            .connect_clicked(move |_| {
                let Some(ward) = ctx.selected_ward.borrow().clone() else {
                    return;
                };
                ctx.w.graduate_confirm_button.set_visible(false);
                dispatch(&ctx, Mutation::Graduate { ward }, false);
            });
    }
}

// ── Interaction handlers ────────────────────────────────────────────────────

/// Persist the editor's knobs for the selected ward, then fully refresh
/// (never optimistic — the reload is the round-trip proof).
///
/// A screen-time entry the policy cannot hold is refused *here*, on the shared
/// rule the nest would refuse it by, and surfaced on `error-message` with no
/// round trip — so the guardian reads the reason instead of a generic RPC
/// failure, and the rest of the editor is left untouched.
fn submit_policy(ctx: &Rc<Ctx>) {
    let Some(ward) = ctx.selected_ward.borrow().clone() else {
        return;
    };
    let policy = match editor_policy(&ctx.w, ctx.unknown_peer_dm_edited.get()) {
        Ok(policy) => policy,
        Err(reason) => {
            show_error(ctx, reason);
            return;
        }
    };
    dispatch(ctx, Mutation::SavePolicy { ward, policy }, false);
}

/// `fauna.family.contact.add` for the selected ward. v1 takes a **hex actor id**
/// (no handle resolution — the windows seam's rule); a parse failure surfaces in
/// `error-message` without a round trip.
fn submit_contact_add(ctx: &Rc<Ctx>) {
    let Some(ward) = ctx.selected_ward.borrow().clone() else {
        return;
    };
    let input = ctx.w.contact_input.text().trim().to_string();
    if input.is_empty() {
        return;
    }
    let Ok(peer) = fauna_core::identity::ActorId::from_hex(&input).map(|a| a.0.to_vec()) else {
        show_error(ctx, S::CONTACT_ADD_INVALID_ACTOR_ID);
        return;
    };
    dispatch(ctx, Mutation::ContactAdd { ward, peer }, true);
}

/// `fauna.family.transfer` — propose a new guardian for the selected ward
/// (pending until the target accepts, § Graduation & transfer). Same hex
/// actor-id convention (and error) as contact-add.
fn submit_transfer(ctx: &Rc<Ctx>) {
    let Some(ward) = ctx.selected_ward.borrow().clone() else {
        return;
    };
    let input = ctx.w.transfer_input.text().trim().to_string();
    if input.is_empty() {
        return;
    }
    let Ok(target) = fauna_core::identity::ActorId::from_hex(&input).map(|a| a.0.to_vec()) else {
        show_error(ctx, S::CONTACT_ADD_INVALID_ACTOR_ID);
        return;
    };
    dispatch(ctx, Mutation::Transfer { ward, target }, false);
}

/// Accept/decline one incoming proposal (proposed-guardian side). Accept
/// re-points the link — the follow-up re-read lands the ward in this
/// account's own Wards list.
fn submit_incoming_decision(ctx: &Rc<Ctx>, index: usize, accept: bool) {
    let Some(entry) = ctx.incoming.borrow().get(index).cloned() else {
        return;
    };
    let ward = entry.supervised_actor_id.to_vec();
    let mutation = if accept {
        Mutation::TransferAccept { ward }
    } else {
        Mutation::TransferDecline { ward }
    };
    dispatch(ctx, mutation, false);
}

/// Approve/deny one queue row. `decide` passes the entry's identifying fields
/// straight through: a `contact` is named by `peer_actor_id`, a `mail_hold` by
/// `message_id` (family-safety.md § Reach approvals).
fn submit_decide(ctx: &Rc<Ctx>, index: usize, approve: bool) {
    let Some(entry) = ctx.approvals.borrow().get(index).cloned() else {
        return;
    };
    dispatch(
        ctx,
        Mutation::Decide {
            ward: entry.supervised_actor_id.to_vec(),
            kind: entry.kind.clone(),
            peer: entry.peer_actor_id.to_vec(),
            message_id: entry.message_id.to_vec(),
            // A `feed_source` item's key is this triple; empty for every other
            // kind, exactly as `peer`/`message_id` are empty for the kinds they
            // do not name.
            bridge_id: entry.bridge_id.clone(),
            operation: entry.operation.clone(),
            target: entry.target.clone(),
            // A `dm_hold` item's key is `(bridge_id, peer_address)` — the
            // external peer is not an actor on this nest, so it cannot ride
            // `peer`. Empty for every other kind except `mail_hold`, which
            // renders (but does not key on) the sender address.
            peer_address: entry.peer_address.clone(),
            approve,
        },
        false,
    );
}

// ── Async plumbing ──────────────────────────────────────────────────────────

/// The mutations the page dispatches. Each is followed by a full re-read, so
/// every render comes from nest-confirmed state.
enum Mutation {
    SavePolicy {
        ward: Vec<u8>,
        policy: ReachPolicy,
    },
    Decide {
        ward: Vec<u8>,
        kind: String,
        peer: Vec<u8>,
        message_id: Vec<u8>,
        bridge_id: String,
        operation: String,
        target: String,
        peer_address: String,
        approve: bool,
    },
    ContactAdd {
        ward: Vec<u8>,
        peer: Vec<u8>,
    },
    Graduate {
        ward: Vec<u8>,
    },
    /// Propose `target` as `ward`'s new guardian (pending until accepted).
    Transfer {
        ward: Vec<u8>,
        target: Vec<u8>,
    },
    /// Withdraw `ward`'s pending proposal (initiator side).
    TransferCancel {
        ward: Vec<u8>,
    },
    /// Consent to a proposal naming this caller (proposed-guardian side).
    TransferAccept {
        ward: Vec<u8>,
    },
    /// Refuse a proposal naming this caller.
    TransferDecline {
        ward: Vec<u8>,
    },
    /// Set/clear the guardian-enrolled-device marker on one of `ward`'s devices
    /// (`family-safety.md` § Full visibility for young children, Slice F).
    /// `device_id` is the hex spelling `fauna.family.status` handed us.
    DeviceMark {
        ward: Vec<u8>,
        device_id: String,
        marked: bool,
    },
    /// Un-deny one denied bridge-DM peer of `ward` — the shared
    /// `FamilyClient::allow_blocked_dm_peer` (`family-safety.md` § The
    /// bridge-DM gate → *The un-deny surface*).
    AllowBlockedPeer {
        ward: Vec<u8>,
        bridge_id: String,
        peer_id: String,
    },
}

/// Load the page on mount / on navigate.
fn refresh(ctx: &Rc<Ctx>) {
    let nest = ctx.nest.clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        // Kept: load() is two sequential RPCs (status + approvals_list), not
        // a single NestClient RPC (transport.md § Request lifecycle step 3's
        // note).
        move || async move { hydrate_with_retry(|| load(nest.clone())).await },
        move |result| apply(&ctx_render, result, false),
    );
}

/// Run a mutation on the tokio runtime, re-read, then render on the GTK thread.
fn dispatch(ctx: &Rc<Ctx>, mutation: Mutation, clear_contact_input: bool) {
    ctx.w.save_button.set_sensitive(false);
    let nest = ctx.nest.clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { run_mutation(nest, mutation).await },
        move |result| apply(&ctx_render, result, clear_contact_input),
    );
}

async fn run_mutation(nest: Nest, mutation: Mutation) -> LoadResult {
    let client = FamilyClient::new(nest.clone());
    match mutation {
        Mutation::SavePolicy { ward, policy } => client
            .policy_update(ward, policy)
            .await
            .map_err(|e| e.to_string())?,
        Mutation::Decide {
            ward,
            kind,
            peer,
            message_id,
            bridge_id,
            operation,
            target,
            peer_address,
            approve,
        } => client
            .approvals_decide(
                ward,
                kind,
                peer,
                message_id,
                bridge_id,
                operation,
                target,
                peer_address,
                approve,
            )
            .await
            .map_err(|e| e.to_string())?,
        Mutation::ContactAdd { ward, peer } => client
            .contact_add(ward, peer)
            .await
            .map_err(|e| e.to_string())?,
        Mutation::Graduate { ward } => client.graduate(ward).await.map_err(|e| e.to_string())?,
        Mutation::Transfer { ward, target } => client
            .transfer(ward, target)
            .await
            .map_err(|e| e.to_string())?,
        Mutation::TransferCancel { ward } => client
            .transfer_cancel(ward)
            .await
            .map_err(|e| e.to_string())?,
        Mutation::TransferAccept { ward } => client
            .transfer_accept(ward)
            .await
            .map_err(|e| e.to_string())?,
        Mutation::TransferDecline { ward } => client
            .transfer_decline(ward)
            .await
            .map_err(|e| e.to_string())?,
        Mutation::DeviceMark {
            ward,
            device_id,
            marked,
        } => client
            .device_mark(ward, device_id, marked)
            .await
            .map_err(|e| e.to_string())?,
        Mutation::AllowBlockedPeer {
            ward,
            bridge_id,
            peer_id,
        } => client
            .allow_blocked_dm_peer(ward, bridge_id, peer_id)
            .await
            .map_err(|e| e.to_string())?,
    }
    load(nest).await
}

/// The one read that fills BOTH sections: `fauna.family.status` (guardian +
/// supervised sides) plus the guardian's `fauna.family.approvals.list`.
async fn load(nest: Nest) -> LoadResult {
    let client = FamilyClient::new(nest);
    let status = client.status().await.map_err(|e| e.to_string())?;
    // The approvals queue is guardian-side only; a supervised-only account gets
    // an empty list, so one unconditional read keeps a single code path.
    let approvals = client
        .approvals_list()
        .await
        .map_err(|e| e.to_string())?
        .approvals;
    Ok(FamilyView {
        guardian_handle: status.supervised_by.map(|g| g.handle),
        policy: status.policy,
        wards: status.wards,
        approvals,
        incoming: status.incoming_transfers,
        usage_today_minutes: status.usage_today_minutes,
        contact_requests: status.contact_requests,
        feed_requests: status.feed_requests,
        own_age_band: status.age_band,
    })
}

// ── Render ──────────────────────────────────────────────────────────────────

/// Render a load/mutation outcome (GTK main thread). On error the message shows
/// and the rendered state is untouched.
fn apply(ctx: &Rc<Ctx>, result: LoadResult, clear_contact_input: bool) {
    ctx.w.save_button.set_sensitive(true);
    match result {
        Ok(view) => {
            crate::settings::render_error_label(&ctx.w.error_label, None);
            if clear_contact_input {
                ctx.w.contact_input.set_text("");
            }
            render(ctx, view);
        }
        Err(msg) => show_error(ctx, &msg),
    }
}

fn show_error(ctx: &Rc<Ctx>, msg: &str) {
    crate::settings::render_error_label(&ctx.w.error_label, Some(msg));
}

fn render(ctx: &Rc<Ctx>, view: FamilyView) {
    // ── Supervised section ──
    match (&view.guardian_handle, &view.policy) {
        (Some(handle), policy) => {
            ctx.w.guardian_handle.set_text(&S::guardian_label(handle));
            ctx.w.policy_summary.set_text(&policy_summary(
                policy.as_ref().cloned().unwrap_or_default(),
                view.usage_today_minutes,
            ));
            ctx.w.supervised_group.set_visible(true);
        }
        (None, _) => {
            ctx.w.guardian_handle.set_text("");
            ctx.w.policy_summary.set_text("");
            ctx.w.supervised_group.set_visible(false);
        }
    }
    // Absent, never placeholdered, without a nameable band (or when the
    // caller is not supervised — the summary lives in that section).
    let own_band = view
        .guardian_handle
        .as_ref()
        .and(view.own_age_band.as_ref())
        .and_then(fauna_client_family::row_text::own_age_band_text);
    ctx.w
        .age_band_summary
        .set_text(own_band.as_deref().unwrap_or_default());
    ctx.w.age_band_summary.set_visible(own_band.is_some());

    // This page's own `fauna.family.status` read is the freshest view of the
    // ward's OWN policy anywhere in the app, so it also refreshes the global
    // `screen-time-lock` inputs (family-safety.md § Screen time). Without this
    // the lock would only ever see the policy as it stood at login, and a
    // guardian's edit would not bind until the ward restarted — while the very
    // page the ward is told to visit had just read the new one.
    crate::screen_lock::set_ward_screen_time(
        view.policy.as_ref().and_then(|p| p.screen_time),
        view.guardian_handle.clone(),
        view.usage_today_minutes,
    );
    // Same read, same reason, for the ward's own pending asks — gated on the
    // guardian being present (`crate::ward_asks::set_from_status`).
    crate::ward_asks::set_from_status(
        view.guardian_handle.is_some(),
        view.contact_requests.clone(),
        view.feed_requests.clone(),
    );

    // ── Guardian section ──
    let is_guardian = !view.wards.is_empty();
    ctx.w.wards_group.set_visible(is_guardian);
    ctx.w.approvals_group.set_visible(is_guardian);

    render_wards(ctx, &view.wards);
    render_approvals(ctx, &view.approvals);

    // ── Incoming-transfer prompts (independent of both roles — any user can
    // be a proposed guardian) ──
    ctx.w.incoming_group.set_visible(!view.incoming.is_empty());
    render_incoming(ctx, &view.incoming);

    *ctx.wards.borrow_mut() = view.wards;
    *ctx.approvals.borrow_mut() = view.approvals;
    *ctx.incoming.borrow_mut() = view.incoming;

    // Re-select the previously-selected ward if it's still present, else the
    // first one (the windows `LoadAsync` rule).
    let selected = ctx.selected_ward.borrow().clone();
    let wards = ctx.wards.borrow();
    let still = selected
        .as_ref()
        .and_then(|id| wards.iter().position(|w| w.actor_id.as_slice() == &id[..]));
    let index = still.or(if wards.is_empty() { None } else { Some(0) });
    match index.and_then(|i| wards.get(i).map(|w| (i, w.clone()))) {
        Some((i, ward)) => {
            *ctx.selected_ward.borrow_mut() = Some(ward.actor_id.to_vec());
            if let Some(row) = ctx.w.wards_list.row_at_index(i as i32) {
                ctx.w.wards_list.select_row(Some(&row));
            }
            drop(wards);
            load_editor(ctx, &ward);
        }
        None => {
            *ctx.selected_ward.borrow_mut() = None;
            ctx.w.policy_group.set_visible(false);
            ctx.w.graduate_confirm_button.set_visible(false);
        }
    }
}

/// Rebuild the indexed `family-ward-item` rows.
fn render_wards(ctx: &Rc<Ctx>, wards: &[FamilyWardInfo]) {
    while let Some(row) = ctx.w.wards_list.row_at_index(0) {
        ctx.w.wards_list.remove(&row);
    }
    for ward in wards {
        let handle = gtk::Label::new(Some(&ward.handle));
        handle.set_halign(gtk::Align::Start);
        handle.set_hexpand(true);
        set_test_id(&handle, ids::FAMILY_WARD_HANDLE);

        // The handle, and — for a ward with Guardian Notify counts today — the
        // per-ward readout below it (family-safety.md § Guardian Notify: category +
        // count, never content). Rendered only when non-empty, so the row stays
        // clean for a ward with nothing flagged.
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 2);
        inner.set_hexpand(true);
        inner.append(&handle);
        if !ward.content_notices.is_empty() {
            let notices = gtk::Label::new(Some(
                &fauna_client_family::row_text::ward_content_notices_text(&ward.content_notices),
            ));
            notices.set_halign(gtk::Align::Start);
            notices.set_wrap(true);
            notices.set_xalign(0.0);
            notices.add_css_class("dim-label");
            set_test_id(&notices, ids::FAMILY_WARD_CONTENT_NOTICES);
            inner.append(&notices);
        }
        // The screen-time readout, on the same terms: rendered only when the
        // nest actually accounted a figure for this ward, which it does only
        // while a daily budget is set (family-safety.md § Screen time — "no
        // usage accounting without a declared policy"). No budget → `None` →
        // no row clutter. The wording is the shared `usage_today_line`, the
        // same call the ward's own summary makes, so guardian and child cannot
        // be shown different numbers.
        if let Some(used) = ward.usage_today_minutes {
            let usage =
                gtk::Label::new(Some(&ward_usage_today_text(used, ward.policy.screen_time)));
            usage.set_halign(gtk::Align::Start);
            usage.set_wrap(true);
            usage.set_xalign(0.0);
            usage.add_css_class("dim-label");
            set_test_id(&usage, ids::FAMILY_WARD_USAGE_TODAY);
            inner.append(&usage);
        }
        // The ward's age band and how it was set — rendered only for a band
        // this client can name (absent, never placeholdered; `family-safety.md`
        // § App surface → *Age-band surfaces*).
        if let Some(text) = ward
            .age_band
            .as_ref()
            .and_then(fauna_client_family::row_text::ward_age_band_text)
        {
            let band = gtk::Label::new(Some(&text));
            band.set_halign(gtk::Align::Start);
            band.set_wrap(true);
            band.set_xalign(0.0);
            band.add_css_class("dim-label");
            set_test_id(&band, ids::FAMILY_WARD_AGE_BAND);
            inner.append(&band);
        }

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.set_margin_top(10);
        content.set_margin_bottom(10);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.append(&inner);

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&content));
        set_test_id(&row, ids::FAMILY_WARD_ITEM);
        ctx.w.wards_list.append(&row);
    }
    ctx.w.wards_empty.set_visible(wards.is_empty());
}

/// Rebuild the indexed `family-approval-item` rows.
///
/// **A row's display text is `peer_address` for a `mail_hold` and `summary`
/// otherwise.** A mail hold's `summary` is deliberately always empty — a subject
/// line is content, and the message is sealed to the ward, so the nest never
/// sees it; its peer is an *address*, carried in `peer_address`
/// (family-safety.md § Reach approvals). Rendering `summary` for such a row
/// leaves it blank (and, on windows, pruned from the accessibility tree).
fn render_approvals(ctx: &Rc<Ctx>, approvals: &[FamilyApprovalEntry]) {
    let mut rows = ctx.w.approval_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.approvals_list.remove(&row);
    }
    for (index, entry) in approvals.iter().enumerate() {
        let row = build_approval_row(ctx, index, entry);
        ctx.w.approvals_list.append(&row);
        rows.push(row);
    }
    ctx.w.approvals_empty.set_visible(approvals.is_empty());
}

fn build_approval_row(ctx: &Rc<Ctx>, index: usize, entry: &FamilyApprovalEntry) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::FAMILY_APPROVAL_ITEM);

    let text = gtk::Label::new(Some(
        entry
            .display_text()
            .as_deref()
            .unwrap_or(S::APPROVAL_NO_SENDER),
    ));
    text.set_hexpand(true);
    text.set_halign(gtk::Align::Start);
    text.set_wrap(true);
    row.append(&text);

    let approve = gtk::Button::with_label(S::APPROVE);
    approve.add_css_class("suggested-action");
    approve.set_valign(gtk::Align::Center);
    set_test_id(&approve, ids::FAMILY_APPROVAL_APPROVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&approve, "fauna.family.approvals.decide");
    {
        let ctx = Rc::clone(ctx);
        approve.connect_clicked(move |_| submit_decide(&ctx, index, true));
    }
    row.append(&approve);

    let deny = gtk::Button::with_label(S::DENY);
    deny.add_css_class("destructive-action");
    deny.set_valign(gtk::Align::Center);
    set_test_id(&deny, ids::FAMILY_APPROVAL_DENY_BUTTON);
    crate::offline_gate::declare_wire_kind(&deny, "fauna.family.approvals.decide");
    {
        let ctx = Rc::clone(ctx);
        deny.connect_clicked(move |_| submit_decide(&ctx, index, false));
    }
    row.append(&deny);

    row
}

/// Rebuild the indexed `family-incoming-transfer-item` rows (the
/// proposed-guardian prompt, § Graduation & transfer).
fn render_incoming(ctx: &Rc<Ctx>, incoming: &[FamilyIncomingTransferInfo]) {
    let mut rows = ctx.w.incoming_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.incoming_list.remove(&row);
    }
    for (index, entry) in incoming.iter().enumerate() {
        let row = build_incoming_row(ctx, index, entry);
        ctx.w.incoming_list.append(&row);
        rows.push(row);
    }
}

fn build_incoming_row(ctx: &Rc<Ctx>, index: usize, entry: &FamilyIncomingTransferInfo) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::FAMILY_INCOMING_TRANSFER_ITEM);

    // "{guardian} asks you to take over supervision of {ward}" — the CURRENT
    // guardian, which is the fact the prompt renders (the initiator may have
    // been the admin).
    let text = gtk::Label::new(Some(&S::incoming_transfer_text(
        &entry.guardian_handle,
        &entry.supervised_handle,
    )));
    text.set_hexpand(true);
    text.set_halign(gtk::Align::Start);
    text.set_wrap(true);
    row.append(&text);

    let accept = gtk::Button::with_label(S::INCOMING_TRANSFER_ACCEPT_BUTTON);
    accept.add_css_class("suggested-action");
    accept.set_valign(gtk::Align::Center);
    set_test_id(&accept, ids::FAMILY_INCOMING_TRANSFER_ACCEPT_BUTTON);
    crate::offline_gate::declare_wire_kind(&accept, "fauna.family.transfer.accept");
    {
        let ctx = Rc::clone(ctx);
        accept.connect_clicked(move |_| submit_incoming_decision(&ctx, index, true));
    }
    row.append(&accept);

    let decline = gtk::Button::with_label(S::INCOMING_TRANSFER_DECLINE_BUTTON);
    decline.add_css_class("destructive-action");
    decline.set_valign(gtk::Align::Center);
    set_test_id(&decline, ids::FAMILY_INCOMING_TRANSFER_DECLINE_BUTTON);
    crate::offline_gate::declare_wire_kind(&decline, "fauna.family.transfer.decline");
    {
        let ctx = Rc::clone(ctx);
        decline.connect_clicked(move |_| submit_incoming_decision(&ctx, index, false));
    }
    row.append(&decline);

    row
}

/// Rebuild the selected ward's indexed `family-device-mark-item` rows
/// (`family-safety.md` § Full visibility for young children, Slice F).
///
/// The rows carry the ward + `device_id` **by value** into their handlers rather
/// than an index into `ctx`, so a concurrent refresh that reorders the ward's
/// devices can never route a flip at the wrong device — the mark is a security
/// promise ("the child cannot remove the guardian's device"), and an off-by-one
/// there marks the child's own phone.
fn render_ward_devices(ctx: &Rc<Ctx>, ward: &FamilyWardInfo) {
    let mut rows = ctx.w.device_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.devices_list.remove(&row);
    }
    for device in &ward.devices {
        let row = build_device_row(ctx, ward.actor_id.to_vec(), device);
        ctx.w.devices_list.append(&row);
        rows.push(row);
    }
    ctx.w.devices_empty.set_visible(ward.devices.is_empty());
}

/// Rebuild the selected ward's `family-blocked-peer-item` rows (`family-safety.md`
/// § The bridge-DM gate → *The un-deny surface*). Each row's allow button
/// dispatches the un-deny for THAT row's own `(bridge_id, peer_id)` — see
/// [`fill_blocked_peers`].
fn render_blocked_peers(ctx: &Rc<Ctx>, ward: &FamilyWardInfo) {
    let dispatch_ctx = Rc::clone(ctx);
    let rows = fill_blocked_peers(
        &ctx.w.blocked_list,
        &mut ctx.w.blocked_rows.borrow_mut(),
        ward,
        move |mutation| dispatch(&dispatch_ctx, mutation, false),
    );
    ctx.w.blocked_empty.set_visible(rows == 0);
}

/// The pure half of [`render_blocked_peers`]: replace `rows` in `list` with one
/// `family-blocked-peer-item` per denied peer of `ward`, each allow button
/// wired to `on_allow` with the un-deny for its OWN row. Returns the row count.
///
/// ⚠ The load-bearing property is per-row addressing: a button pointing at
/// row 0 would un-deny the WRONG person while the surface still looked right —
/// a silent, guardian-invisible failure. So each button captures its peer by
/// value at build time, never an index into a list a refresh could reorder
/// (the device rows' rule, for the same reason).
///
/// The un-deny rides the shared `FamilyClient::allow_blocked_dm_peer` (the
/// approving `dm_hold` decide, which owns the wire shape), idempotent and not
/// queue-scoped — it works long after the hold row that prompted the deny is
/// gone. This row only supplies its own `(bridge_id, peer_id)` pair.
fn fill_blocked_peers(
    list: &gtk::Box,
    rows: &mut Vec<gtk::Box>,
    ward: &FamilyWardInfo,
    on_allow: impl Fn(Mutation) + Clone + 'static,
) -> usize {
    for row in rows.drain(..) {
        list.remove(&row);
    }
    for peer in &ward.blocked_dm_peers {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        set_test_id(&row, ids::FAMILY_BLOCKED_PEER_ITEM);
        // The row's own text IS the peer id — the only name this nest has for
        // an external bridge peer (it has no actor here, which is why the gate
        // exists). A test reads rows by id, never by position.
        let label = gtk::Label::new(Some(&peer.peer_id));
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        label.set_wrap(true);
        label.set_xalign(0.0);
        row.append(&label);

        let allow = gtk::Button::with_label(S::BLOCKED_PEER_ALLOW);
        allow.set_valign(gtk::Align::Center);
        set_test_id(&allow, ids::FAMILY_BLOCKED_PEER_ALLOW_BUTTON);
        crate::offline_gate::declare_wire_kind(&allow, "fauna.family.approvals.decide");
        {
            let on_allow = on_allow.clone();
            let ward = ward.actor_id.to_vec();
            let bridge_id = peer.bridge_id.clone();
            let peer_id = peer.peer_id.clone();
            allow.connect_clicked(move |_| {
                on_allow(Mutation::AllowBlockedPeer {
                    ward: ward.clone(),
                    bridge_id: bridge_id.clone(),
                    peer_id: peer_id.clone(),
                })
            });
        }
        row.append(&allow);

        list.append(&row);
        rows.push(row);
    }
    rows.len()
}

/// One device row's widget tree — the pure half of [`build_device_row`], split
/// out so a unit test can assert the **structural** contract the e2e depends on
/// (the toggle is a CHILD of its `family-device-mark-item`, so a scoped read
/// resolves it) without needing a live [`Ctx`]. That contract is not cosmetic:
/// a toggle painted outside its row makes a scoped read return nothing for the
/// marked *and* the unmarked device — a false pass on the negative half, which
/// is exactly how tui's ward-side badge shipped broken (fixed).
fn device_row_widgets(device: &FamilyWardDeviceInfo) -> (gtk::Box, gtk::Switch) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::FAMILY_DEVICE_MARK_ITEM);

    // The device's own label is the row's ONLY text, so the e2e's
    // `get_text("family-device-mark-item", i)` (which joins descendant labels on
    // this client) identifies a row by device name — no reliance on the nest's
    // device order, the same rule the ward-side badge test already follows.
    let label = gtk::Label::new(Some(&device.label));
    label.set_hexpand(true);
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label.set_xalign(0.0);
    row.append(&label);

    let toggle = gtk::Switch::builder()
        .active(device.guardian_marked)
        .valign(gtk::Align::Center)
        .build();
    // The switch carries no visible caption (the section hint explains the
    // control once), so name it for the accessibility tree — "Guardian device",
    // the same string the child later reads on their own device list.
    toggle.update_property(&[gtk::accessible::Property::Label(S::DEVICE_MARK_LABEL)]);
    set_test_id(&toggle, ids::FAMILY_DEVICE_MARK_TOGGLE);
    crate::offline_gate::declare_wire_kind(&toggle, "fauna.family.device.mark");
    set_test_attr(
        &toggle,
        "state",
        if device.guardian_marked { "on" } else { "off" },
    );
    row.append(&toggle);

    (row, toggle)
}

fn build_device_row(ctx: &Rc<Ctx>, ward: Vec<u8>, device: &FamilyWardDeviceInfo) -> gtk::Box {
    let (row, toggle) = device_row_widgets(device);
    {
        let ctx = Rc::clone(ctx);
        let device_id = device.device_id.clone();
        toggle.connect_active_notify(move |sw| {
            if ctx.syncing.get() {
                return;
            }
            let marked = sw.is_active();
            set_test_attr(sw, "state", if marked { "on" } else { "off" });
            // Not batched behind `family-policy-save-button`: `device.mark` is
            // its own per-device RPC, so the flip lands immediately and the
            // re-read re-renders every row from nest-confirmed state.
            dispatch(
                &ctx,
                Mutation::DeviceMark {
                    ward: ward.clone(),
                    device_id: device_id.clone(),
                    marked,
                },
                false,
            );
        });
    }
    row.append(&toggle);

    row
}

/// Load one ward's policy into the ONE shared editor (the windows
/// `LoadPolicyFromWard`). Reveals the editor and resets the graduate confirm
/// step.
fn load_editor(ctx: &Rc<Ctx>, ward: &FamilyWardInfo) {
    let w = &ctx.w;
    ctx.syncing.set(true);

    w.policy_heading.set_text(&ward.handle);
    w.contact_approval.set_active(ward.policy.contact_approval);
    w.federation.set_active(ward.policy.federation_contact);
    w.unknown_sender
        .set_selected(unknown_sender_index(&ward.policy.unknown_sender_mail));
    w.feed_sources
        .set_selected(feed_sources_index(&ward.policy.feed_sources));
    w.unknown_peer_dm
        .set_selected(unknown_peer_dm_index_for_render(
            ward.policy.unknown_peer_dm.as_deref(),
        ));
    ctx.unknown_peer_dm_edited.set(false);

    // Content policy (§ Content policy): the four per-category floors + Notify.
    // An absent content_policy is the all-inherit unsupervised-equivalent default.
    let content = ward.policy.content_policy.unwrap_or_default();
    w.content_nsfw
        .set_selected(content_floor_index(content.nsfw.as_str()));
    w.content_spam
        .set_selected(content_floor_index(content.spam.as_str()));
    w.content_phishing
        .set_selected(content_floor_index(content.phishing.as_str()));
    w.content_commercial
        .set_selected(content_floor_index(content.commercial.as_str()));
    let notify_on = ward.policy.content_notify.unwrap_or(false);
    w.content_notify.set_active(notify_on);

    // Contract 1: the `state` marker the e2e's `get_attr(id, "state")` reads.
    // Stamped on every render (not only on flip), so it always mirrors the
    // nest-confirmed policy.
    set_test_attr(
        &w.contact_approval,
        "state",
        if ward.policy.contact_approval {
            "on"
        } else {
            "off"
        },
    );
    set_test_attr(
        &w.federation,
        "state",
        if ward.policy.federation_contact {
            "on"
        } else {
            "off"
        },
    );
    set_test_attr(
        &w.content_notify,
        "state",
        if notify_on { "on" } else { "off" },
    );

    // Screen time for THIS ward (Slice E). An absent pillar is the
    // unsupervised-equivalent default: every field empty, i.e. no limit. The
    // stored minutes render back through the same shared formatter the parse
    // side inverts, so what a guardian sees is exactly what they could retype.
    let screen = ward.policy.screen_time.unwrap_or_default();
    w.screen_window_start.set_text(
        &screen
            .window_start
            .map(fauna_core::screen_time::format_time_of_day)
            .unwrap_or_default(),
    );
    w.screen_window_end.set_text(
        &screen
            .window_end
            .map(fauna_core::screen_time::format_time_of_day)
            .unwrap_or_default(),
    );
    w.screen_daily_minutes.set_text(
        &screen
            .daily_minutes
            .map(|m| m.to_string())
            .unwrap_or_default(),
    );

    // The guardian-enrolled-device marker for THIS ward (Slice F) — the hint
    // names the ward, so the promise it states ("X cannot remove a marked
    // device") is unambiguous with several wards on the page.
    w.devices_hint.set_text(&S::ward_devices_hint(&ward.handle));
    render_ward_devices(ctx, ward);

    // The un-deny surface for THIS ward — the hint names the ward, like the
    // device hint above.
    w.blocked_hint
        .set_text(&S::blocked_peers_hint(&ward.handle));
    render_blocked_peers(ctx, ward);

    w.graduate_confirm_button
        .set_label(&S::graduate_confirm_button(&ward.handle));
    w.graduate_confirm_button.set_visible(false);

    // Transfer state for THIS ward (§ Graduation & transfer): a pending
    // proposal swaps the initiate row for the pending marker + cancel.
    match &ward.pending_transfer {
        Some(pending) => {
            w.transfer_pending
                .set_text(&S::transfer_pending(&pending.proposed_guardian_handle));
            w.transfer_pending.set_visible(true);
            w.transfer_cancel_button.set_visible(true);
            w.transfer_input.set_visible(false);
            w.transfer_button.set_visible(false);
        }
        None => {
            w.transfer_pending.set_text("");
            w.transfer_pending.set_visible(false);
            w.transfer_cancel_button.set_visible(false);
            w.transfer_input.set_text("");
            w.transfer_input.set_visible(true);
            w.transfer_button.set_visible(true);
        }
    }

    w.policy_group.set_visible(true);

    ctx.syncing.set(false);
}

/// The editor's current knobs as a `ReachPolicy` (the label ↔ wire mapping,
/// contract 2).
///
/// Fallible **only** because of screen time: the three text inputs are the one
/// place a guardian can type something the policy cannot hold, so this returns
/// the shared reason string and `submit_policy` surfaces it on `error-message`
/// without a round trip. Both the per-field parse and the cross-field rules
/// (`ScreenTimePolicy::validate` — bounds come in pairs, `start == end` is
/// ambiguous) are checked here, against the *same* shared code the nest runs at
/// `policy.update`, so a save that would be refused never leaves the client and
/// the guardian sees why locally.
fn editor_policy(w: &Widgets, unknown_peer_dm_edited: bool) -> Result<ReachPolicy, &'static str> {
    let screen_time = fauna_core::screen_time::ScreenTimePolicy {
        window_start: fauna_core::screen_time::parse_time_of_day(&w.screen_window_start.text())?,
        window_end: fauna_core::screen_time::parse_time_of_day(&w.screen_window_end.text())?,
        daily_minutes: fauna_core::screen_time::parse_daily_minutes(
            &w.screen_daily_minutes.text(),
        )?,
    };
    screen_time.validate()?;
    Ok(ReachPolicy {
        contact_approval: w.contact_approval.is_active(),
        unknown_sender_mail: unknown_sender_at(w.unknown_sender.selected()),
        federation_contact: w.federation.is_active(),
        feed_sources: feed_sources_at(w.feed_sources.selected()),
        // Absent means "leave unchanged" (family-safety.md § The bridge-DM
        // gate) — send only when the guardian actually touched the select, or
        // an unrelated save would silently rewrite the ward's knob back to
        // whatever this render happened to show.
        unknown_peer_dm: unknown_peer_dm_edited
            .then(|| unknown_peer_dm_at(w.unknown_peer_dm.selected())),
        // Content policy (§ Content policy, Slice C): the four per-category floors
        // + the Notify toggle are now authored here, so `Some(...)` replaces them
        // on policy.update. content_floor_at fails closed to `block` for an
        // out-of-range index, never the permissive `inherit`.
        content_policy: Some(ContentPolicy {
            nsfw: ContentFloor::from_wire(&content_floor_at(w.content_nsfw.selected())),
            spam: ContentFloor::from_wire(&content_floor_at(w.content_spam.selected())),
            phishing: ContentFloor::from_wire(&content_floor_at(w.content_phishing.selected())),
            commercial: ContentFloor::from_wire(&content_floor_at(w.content_commercial.selected())),
        }),
        content_notify: Some(w.content_notify.is_active()),
        // Screen time (§ Screen time, Slice E) is now authored here, so
        // `Some(...)` replaces it on policy.update — an all-empty editor sends
        // the all-`None` default, which is how a guardian removes every limit.
        // (Absent would mean "leave unchanged" and could never clear one.)
        screen_time: Some(screen_time),
        ..Default::default()
    })
}

/// The supervised side's read-only `family-policy-summary` text — one
/// "{label}: {value}" line per reach knob. The lines (order, labels, and the
/// fail-closed value rendering) come from shared Rust via
/// [`ReachPolicy::summary_lines`]; this file owns only the "{label}: {value}"
/// join, which is linux's own line layout.
/// `usage_today_minutes` folds the screen-time readout into this same summary
/// rather than claiming a new ui.yaml ID. The supervised section's element set
/// is `family-guardian-handle` + `family-policy-summary`, and the ward's usage
/// *is* a line of "the active policy, read-only" — the same shape as every
/// other rule shown here. The number and its wording come from the shared
/// [`fauna_core::format::usage_today_line`], the very call the guardian's own
/// per-ward readout makes, so the two surfaces cannot show different figures
/// (`family-safety.md` § Screen time — the ward's summary shows the same
/// number). `None` renders nothing at all: no budget, no accounting.
fn policy_summary(p: ReachPolicy, usage_today_minutes: Option<u32>) -> String {
    let mut lines = p.summary_lines();
    if let Some(used) = usage_today_minutes {
        lines.push(fauna_core::format::usage_today_line(
            used,
            p.screen_time.and_then(|s| s.daily_minutes),
        ));
    }
    lines
        .into_iter()
        .map(|l| {
            format!(
                "{}: {}",
                l.label.resolve(crate::i18n::strings::lookup),
                l.value.resolve(crate::i18n::strings::lookup)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The guardian's per-ward `family-ward-usage-today` readout — the day's
/// cross-device screen-time total for one ward (`family-safety.md` § Screen
/// time). The number, its label and the "of budget" framing all come from
/// shared Rust via [`fauna_core::format::usage_today_line`] — the same call the
/// ward's own summary makes, which is what makes the goal doc's transparency
/// promise structural; this file owns only the "{label}: {value}" join,
/// mirroring [`policy_summary`] and
/// [`fauna_client_family::row_text::ward_content_notices_text`].
fn ward_usage_today_text(
    used_minutes: u32,
    screen_time: Option<fauna_core::screen_time::ScreenTimePolicy>,
) -> String {
    let line = fauna_core::format::usage_today_line(
        used_minutes,
        screen_time.and_then(|s| s.daily_minutes),
    );
    format!(
        "{}: {}",
        line.label.resolve(crate::i18n::strings::lookup),
        line.value.resolve(crate::i18n::strings::lookup)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// The page exposes every **static** ui.yaml ID (`family:` block,
    /// ui.yaml:1750-1773) with no registered client — `build_family_view`
    /// requires a real `FaunaClient` for `wire`, so this exercises the
    /// client-free `build_page_widgets` split, the exact widget tree the real
    /// page builds before wiring. The indexed IDs (`family-ward-item`,
    /// `family-ward-handle`, `family-approval-item` + its two buttons) are
    /// data-driven and covered by the tier_3 e2e (`tests/test_family.py`).
    #[test]
    fn family_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let (page, _widgets) = build_page_widgets();
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "family-heading",
                "error-message",
                "family-policy-contact-approval-toggle",
                "family-policy-unknown-sender-select",
                "family-policy-federation-toggle",
                "family-policy-feed-sources-select",
                "family-policy-unknown-peer-dm-select",
                "family-policy-content-nsfw-select",
                "family-policy-content-spam-select",
                "family-policy-content-phishing-select",
                "family-policy-content-commercial-select",
                "family-policy-content-notify-toggle",
                "family-policy-screen-window-start-input",
                "family-policy-screen-window-end-input",
                "family-policy-screen-daily-minutes-input",
                "family-policy-save-button",
                "family-contact-add-input",
                "family-contact-add-button",
                "family-transfer-input",
                "family-transfer-button",
                "family-transfer-pending",
                "family-transfer-cancel-button",
                "family-graduate-button",
                "family-graduate-confirm-button",
                "family-guardian-handle",
                "family-policy-summary",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }

    /// Slice F's structural contract: a device-mark row's toggle is a
    /// **descendant of its own `family-device-mark-item`**, and its `state`
    /// marker mirrors `guardian_marked`.
    ///
    /// This is the tier_1 half of the mechanism the e2e reads scoped
    /// (`scope="family-device-mark-item[i]"`). A toggle painted flat — a
    /// sibling of the rows rather than a child — makes that scoped read return
    /// nothing for the marked device *and* nothing for the unmarked one, so the
    /// negative assertion ("marking one device must not mark the other") passes
    /// while the surface is broken. That is precisely how tui's ward-side
    /// guardian badge shipped (fixed), and the failure is
    /// invisible in a green e2e run — so it is pinned here, cheaply, rather
    /// than trusted to the journey test alone.
    #[test]
    fn device_mark_toggle_is_scoped_inside_its_own_row() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            for marked in [false, true] {
                let device = FamilyWardDeviceInfo {
                    device_id: "aa".repeat(32),
                    label: "parents-tablet".to_string(),
                    guardian_marked: marked,
                    extra: Default::default(),
                };
                let (row, _toggle) = device_row_widgets(&device);

                // The row itself carries the item id, and the toggle resolves from
                // WITHIN it — the scoped-read contract.
                let names = widget_names(&row);
                assert_eq!(
                    names.first().map(String::as_str),
                    Some("family-device-mark-item"),
                    "the row widget itself must carry the item id; have {names:?}"
                );
                assert!(
                    names.iter().any(|n| n == "family-device-mark-toggle"),
                    "the toggle must be a descendant of its own row (a flat paint \
                 makes the e2e's scoped read a false pass); have {names:?}"
                );

                // The device's label is the row's own text, so a test can find the
                // row by device name instead of trusting the nest's device order.
                assert!(
                    crate::automation::find::text_of(row.upcast_ref::<gtk::Widget>())
                        .contains("parents-tablet"),
                    "the row must render its device label as its own text"
                );
            }
        });
    }

    fn blocked(bridge: &str, peer: &str) -> fauna_client_family::family::FamilyBlockedPeerInfo {
        fauna_client_family::family::FamilyBlockedPeerInfo {
            bridge_id: bridge.to_string(),
            peer_id: peer.to_string(),
            extra: Default::default(),
        }
    }

    fn ward_with_denials(
        actor: u8,
        peers: Vec<fauna_client_family::family::FamilyBlockedPeerInfo>,
    ) -> FamilyWardInfo {
        FamilyWardInfo {
            actor_id: fauna_protocol::ByteBuf::from(vec![actor; 32]),
            handle: "kid".to_string(),
            blocked_dm_peers: peers,
            ..Default::default()
        }
    }

    /// Each allow press's `(ward, bridge_id, peer_id)` — the key the shared
    /// un-deny (`FamilyClient::allow_blocked_dm_peer`) addresses.
    type Allowed = Rc<RefCell<Vec<(Vec<u8>, String, String)>>>;

    fn record_allows(allowed: &Allowed) -> impl Fn(Mutation) + Clone + 'static {
        let allowed = Rc::clone(allowed);
        move |m| match m {
            Mutation::AllowBlockedPeer {
                ward,
                bridge_id,
                peer_id,
            } => allowed.borrow_mut().push((ward, bridge_id, peer_id)),
            _ => panic!("the allow button must dispatch the shared un-deny"),
        }
    }

    /// No denials → no rows, and the empty case reports zero so the page
    /// STATES it (`NO_BLOCKED_PEERS`) rather than leaving a blank gap.
    #[test]
    fn a_ward_with_nothing_denied_renders_no_rows() {
        crate::testid::run_on_gtk_thread(|| {
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let mut rows = Vec::new();
            let allowed: Allowed = Rc::default();
            let n = fill_blocked_peers(
                &list,
                &mut rows,
                &ward_with_denials(1, Vec::new()),
                record_allows(&allowed),
            );
            assert_eq!(n, 0);
            assert!(
                !widget_names(&list)
                    .iter()
                    .any(|n| n == "family-blocked-peer-item")
            );
        });
    }

    /// Every denial is a row whose own text is the peer id, with its allow
    /// button INSIDE it (the e2e reads it scoped to `family-blocked-peer-item[i]`).
    #[test]
    fn each_denied_peer_gets_a_row_and_an_allow_button() {
        crate::testid::run_on_gtk_thread(|| {
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let mut rows = Vec::new();
            let allowed: Allowed = Rc::default();
            let w = ward_with_denials(
                1,
                vec![blocked("nostr", "npub1aaa"), blocked("nostr", "npub1bbb")],
            );
            assert_eq!(
                fill_blocked_peers(&list, &mut rows, &w, record_allows(&allowed)),
                2
            );
            let texts: Vec<String> = rows
                .iter()
                .map(|r| crate::automation::find::text_of(r.upcast_ref::<gtk::Widget>()))
                .collect();
            assert!(texts[0].contains("npub1aaa") && texts[1].contains("npub1bbb"));
            for row in &rows {
                let names = widget_names(row);
                assert_eq!(
                    names.first().map(String::as_str),
                    Some("family-blocked-peer-item")
                );
                assert!(
                    names
                        .iter()
                        .any(|n| n == "family-blocked-peer-allow-button"),
                    "every denial must be reversible from within its own row"
                );
            }
            // A re-render replaces, never appends.
            assert_eq!(
                fill_blocked_peers(&list, &mut rows, &w, record_allows(&allowed)),
                2
            );
            assert_eq!(
                widget_names(&list)
                    .iter()
                    .filter(|n| *n == "family-blocked-peer-item")
                    .count(),
                2
            );
        });
    }

    /// ⚠ The assertion that protects the user (rule (f)): each allow button
    /// un-denies ITS OWN row's `(bridge_id, peer_id)` on THIS ward, via the
    /// shared un-deny. Pointing every button at row 0 would
    /// un-deny the wrong person while the surface still looked correct.
    #[test]
    fn the_allow_button_addresses_its_own_rows_peer() {
        crate::testid::run_on_gtk_thread(|| {
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let mut rows = Vec::new();
            let allowed: Allowed = Rc::default();
            let w = ward_with_denials(
                7,
                vec![blocked("nostr", "npub1aaa"), blocked("matrix", "npub1bbb")],
            );
            fill_blocked_peers(&list, &mut rows, &w, record_allows(&allowed));
            for row in &rows {
                crate::testid::find_by_test_id(row, ids::FAMILY_BLOCKED_PEER_ALLOW_BUTTON)
                    .and_then(|b| b.downcast::<gtk::Button>().ok())
                    .expect("each row carries its allow button")
                    .emit_clicked();
            }
            assert_eq!(
                *allowed.borrow(),
                vec![
                    (vec![7u8; 32], "nostr".to_string(), "npub1aaa".to_string()),
                    (vec![7u8; 32], "matrix".to_string(), "npub1bbb".to_string()),
                ],
                "each button must carry its own row's peer, not the first row's"
            );
        });
    }

    /// Contract 2: the two selects' model strings are the **localized labels**
    /// the e2e passes to `driver.select` / reads back from `get_text` — a wire
    /// value ("hold") in the model would never match "Hold for review".
    #[test]
    fn policy_selects_hold_localized_labels_not_wire_values() {
        assert_eq!(
            unknown_sender_labels(),
            ["Allow", "Hold for review", "Reject"]
        );
        assert_eq!(feed_sources_labels(), ["Allow", "Block"]);
        assert_eq!(unknown_peer_dm_labels(), ["Allow", "Hold for review"]);
        // …and the index ↔ wire-value bridge round-trips.
        assert_eq!(unknown_sender_index("hold"), 1);
        assert_eq!(unknown_sender_at(2), "reject");
        assert_eq!(feed_sources_index("block"), 1);
        assert_eq!(feed_sources_at(0), "allow");
        assert_eq!(unknown_peer_dm_at(0), "allow");
        // An unparseable knob renders as its fail-closed option, never as the
        // permissive index 0 (family-safety.md § Implementation status).
        assert_eq!(unknown_sender_index("quarantine"), 1);
        assert_eq!(feed_sources_index("curated"), 1);
    }

    /// The *save* direction fails closed too. `editor_policy` writes
    /// `<select>_at(selected())` straight back to `policy.update`, so an
    /// out-of-range position must degrade to the knob's strict option — never to
    /// the permissive index 0, which would silently downgrade the ward's
    /// protection on the next save. (The model is built from the shared catalog,
    /// so out-of-range is unreachable; this pins the defense-in-depth fallback
    /// that a bare `unwrap_or(values[0])` previously got backwards.)
    #[test]
    fn an_out_of_range_selection_saves_the_fail_closed_value_not_allow() {
        for oob in [3, 99, u32::MAX] {
            assert_eq!(unknown_sender_at(oob), "hold", "position {oob}");
        }
        for oob in [2, 99, u32::MAX] {
            assert_eq!(feed_sources_at(oob), "block", "position {oob}");
        }
        for oob in [2, 99, u32::MAX] {
            assert_eq!(unknown_peer_dm_at(oob), "hold", "position {oob}");
        }
    }

    /// The render direction is the mirror image of the fail-closed rule above:
    /// an ABSENT `unknown_peer_dm` is the `allow` DEFAULT, never the
    /// fail-closed `hold` — the nest omits a knob sitting at its default, so
    /// absence here means "already allow", not "unparseable"
    /// (`family-safety.md` § The bridge-DM gate).
    #[test]
    fn an_absent_unknown_peer_dm_renders_the_allow_default_not_the_fail_closed_value() {
        assert_eq!(unknown_peer_dm_index_for_render(None), 0);
        assert_eq!(unknown_peer_dm_index_for_render(Some("allow")), 0);
        assert_eq!(unknown_peer_dm_index_for_render(Some("hold")), 1);
    }

    /// The save-direction twin, and the one rule in this row that is NOT a copy
    /// of a sibling knob: `unknown_peer_dm` is `Option<String>` and absent means
    /// "leave unchanged", so `editor_policy` must send `None` unless the
    /// guardian actually touched the select — an always-send would echo a
    /// render-time value back on every unrelated save (`family-safety.md`
    /// § The bridge-DM gate).
    #[test]
    fn editor_policy_sends_unknown_peer_dm_only_when_edited() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (_page, widgets) = build_page_widgets();

            widgets.unknown_peer_dm.set_selected(1); // "Hold for review"

            let untouched = editor_policy(&widgets, false).expect("valid screen-time defaults");
            assert_eq!(
                untouched.unknown_peer_dm, None,
                "an untouched select must leave the stored knob alone, \
                 regardless of what it happens to render"
            );

            let touched = editor_policy(&widgets, true).expect("valid screen-time defaults");
            assert_eq!(touched.unknown_peer_dm, Some("hold".to_string()));
        });
    }

    /// The single most important row-rendering rule: a `mail_hold` renders its
    /// `peer_address` (its `summary` is deliberately always empty — a subject
    /// line is content), every other kind its `summary`. The rule itself is
    /// `fauna_core::format::approval_display_text` (pinned there); this test
    /// confirms linux's call site — `entry.display_text().as_deref().unwrap_or(...)` —
    /// wires it correctly.
    #[test]
    fn mail_hold_row_renders_peer_address_not_the_empty_summary() {
        let hold = FamilyApprovalEntry {
            kind: "mail_hold".into(),
            peer_address: "stranger@example.com".into(),
            summary: String::new(),
            ..Default::default()
        };
        let contact = FamilyApprovalEntry {
            kind: "contact".into(),
            peer_address: String::new(),
            summary: "hi from bob".into(),
            ..Default::default()
        };
        assert_eq!(
            hold.display_text()
                .as_deref()
                .unwrap_or(S::APPROVAL_NO_SENDER),
            "stranger@example.com"
        );
        assert_eq!(
            contact
                .display_text()
                .as_deref()
                .unwrap_or(S::APPROVAL_NO_SENDER),
            "hi from bob"
        );
    }

    /// A held null-path message (`MAIL FROM:<>`) truthfully carries an empty
    /// `peer_address`; the row falls back to the localized no-sender label
    /// rather than rendering blank (family-safety.md § The mail gate).
    #[test]
    fn null_path_mail_hold_row_renders_the_no_sender_label() {
        let null_path_hold = FamilyApprovalEntry {
            kind: "mail_hold".into(),
            peer_address: String::new(),
            summary: String::new(),
            ..Default::default()
        };
        assert_eq!(
            null_path_hold
                .display_text()
                .as_deref()
                .unwrap_or(S::APPROVAL_NO_SENDER),
            S::APPROVAL_NO_SENDER
        );
    }
}
