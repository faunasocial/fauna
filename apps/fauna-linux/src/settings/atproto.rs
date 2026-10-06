//! Settings → **Bluesky** — the ATProto integration page
//! (`docs/goal/ui/atproto.md`). One page answers one question: *how deep is
//! this user's Bluesky integration?* The spine is the four-rung
//! **integration-depth selector**; every other control reveals below it as a
//! sub-setting of the level that makes it meaningful:
//!
//! - **selector** (`atproto-depth-*`) — one ordered choice; selecting a
//!   *different* level stages the transition card, never mutates the level
//!   directly (the one exception, Off → Linked, is effect-free and applies on
//!   select). Hosted rungs grey-with-reason on a non-public domain.
//! - **transition card** (`atproto-depth-confirm-card`) — the composed effect
//!   lines, rendered *verbatim* from the machine's `pending_transition.lines`
//!   (the single `TransitionPlan` the nest also executes), plus the
//!   history-backfill opt-in on a minting move and confirm/cancel.
//! - **Linked-account panel** — the consume-side link surface at level =
//!   `linked`: the shared `bridge-link-form` / `bridge-card` components
//!   embedded *verbatim* from the Bridges page via
//!   `views::bridges::detail::build_bridge_detail_content` (zero new element
//!   IDs — ui/atproto.md § Element IDs). It replaces the Bluesky provider row
//!   the unified Bridges page used to carry (§ Migration step 2), so the two
//!   always land together; its behavior stays owned by behavior/bridges.md.
//! - **hosted panel** — pre-mint: the DID-method radio + the either-way handle
//!   line; post-mint: the identity summary (`atproto-hosted-handle`).
//! - **full-PDS panel** — the F1 login-plane surface (app credentials, the
//!   connected-apps list, the external-apps kill-switch), gated on level =
//!   `hosted_full`.
//! - **delete presence** (`atproto-delete-presence`) — visible whenever a
//!   hosted identity exists; the destructive flow itself is S5-scoped.
//!
//! Backed by the shared `AtprotoSettingsMachine`, which mirrors
//! `LabelerCatalogMachine`'s observer-driven shape (`views/personalization/`
//! is the wiring template): a registered observer notifies over a channel, and
//! a render loop repaints the whole page off `snapshot()`. Per-gesture dispatch
//! uses `spawn_with_snapshot` — the `settings/mail.rs` idiom — since a
//! gesture's *local* result (a minted/revealed secret) never rides the snapshot
//! (D3: the secret is never in the passively-rendered state). The senior
//! rotation key a hosted mint needs is generated inside the machine's
//! `confirm_transition` (via `fauna-client-atproto::mint_rotation_key`);
//! this leg never touches key material.
//!
//! F1 collects no label/dm_allowed input at mint time — the client auto-labels
//! ("App credential N") and defaults `dm_allowed` to `false` (least privilege).

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use fauna_atproto_settings_machine::{
    AppCredentialRow, AtprotoSettingsMachine, AtprotoSettingsObserver, DelegationRow,
    depth_level_options,
};
use fauna_client_bridges::bridges_ui::BridgeStatus;
use fauna_core::localized::LocalizedText;

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::atproto_settings as BS;
use crate::testid::{set_test_attr, set_test_id};

fn resolve(text: &LocalizedText) -> String {
    text.clone().resolve(crate::i18n::strings::lookup)
}

/// Bridges `AtprotoSettingsMachine` notifications to the GTK main loop.
/// Mirrors `GtkLabelerCatalogObserver` (`views/personalization/mod.rs`).
struct GtkAtprotoSettingsObserver {
    tx: async_channel::Sender<()>,
}

impl AtprotoSettingsObserver for GtkAtprotoSettingsObserver {
    fn on_changed(&self) {
        let _ = self.tx.try_send(());
    }
}

/// The page's static widgets (built once; list rows + card lines are rebuilt
/// every render, everything else toggles visibility/state).
struct AtprotoPageWidgets {
    error_label: gtk::Label,

    // ── Recovery-fork contest (`atproto-contest-*`, IDs user-approved
    //    2026-08-02) — LEADS the page, above the depth selector: an identity
    //    under active attack outranks every settings row below it. Rendered
    //    off CLIENT-SIDE evidence only (behavior/atproto-identity-custody.md
    //    § The 72 h recovery-fork contest).
    contest_group: adw::PreferencesGroup,
    /// `atproto-contest-card`; carries the `state` attr.
    contest_card: gtk::Box,
    contest_detail: gtk::Label,
    contest_deadline: gtk::Label,
    contest_btn: gtk::Button,
    /// `atproto-contest-confirm-card`; visible only while
    /// `snapshot.contest_confirm` is `Some`.
    contest_confirm_card: gtk::Box,
    contest_confirm_lines: gtk::Box,
    contest_confirm_line_labels: RefCell<Vec<gtk::Label>>,
    contest_confirm_btn: gtk::Button,
    contest_cancel_btn: gtk::Button,

    // ── Depth selector ──────────────────────────────────────────────────
    /// `atproto-depth-selector`; carries the current level in its `state` attr.
    depth_selector: gtk::Box,
    /// The four rung radios, in `DEPTH_LEVELS` order.
    depth_rungs: [gtk::CheckButton; 4],
    /// The visible one-line gate reason under the hosted rungs.
    depth_reason: gtk::Label,

    // ── Transition card ─────────────────────────────────────────────────
    card_group: adw::PreferencesGroup,
    /// `atproto-depth-confirm-card`; the composed effect lines are rebuilt into
    /// it each render.
    card_lines: gtk::Box,
    card_line_labels: RefCell<Vec<gtk::Label>>,
    history_backfill: gtk::CheckButton,
    confirm_btn: gtk::Button,
    cancel_btn: gtk::Button,

    // ── Linked-account panel ────────────────────────────────────────────
    /// Wrapper hosting the embedded Bridges detail content (that content is a
    /// `gtk::Box` of its own PreferencesGroups, and `PreferencesPage::add`
    /// only accepts a group).
    linked_group: adw::PreferencesGroup,
    /// The currently-embedded content, so it can be torn down on rebuild.
    linked_content: RefCell<Option<gtk::Box>>,
    /// The `BridgeStatus` the embedded content was built from. Rebuilding is
    /// gated on this actually changing: the content owns live widget state
    /// (an open link dialog, typed-in fields) that a per-render rebuild would
    /// destroy, and `render` runs on every machine notification.
    linked_built_from: RefCell<Option<BridgeStatus>>,
    /// The embedded content's `follows_list`, tracked so the
    /// `notify_bridge_follows_loaded` hook can repaint it when a
    /// `fauna.bridges.list_follows` reply for "bluesky" lands.
    /// `None` until the first (re)build, and torn down/rebuilt in lockstep
    /// with `linked_content`.
    linked_follows_list: RefCell<Option<gtk::ListBox>>,

    // ── Hosted panel ────────────────────────────────────────────────────
    hosted_group: adw::PreferencesGroup,
    /// `atproto-did-method`; the pre-mint DID-method radio group.
    did_method_box: gtk::Box,
    did_plc: gtk::CheckButton,
    did_web: gtk::CheckButton,
    handle_preview: gtk::Label,
    /// Own group for the identity summary — gated on the identity existing,
    /// never on `show_hosted` (see the render-time comment on why).
    identity_group: adw::PreferencesGroup,
    /// `atproto-hosted-handle`; the identity summary line.
    hosted_handle: gtk::Label,

    // ── Delete presence ─────────────────────────────────────────────────
    delete_group: adw::PreferencesGroup,
    delete_btn: gtk::Button,
    /// `atproto-delete-confirm-card`; visible only while
    /// `snapshot.delete_confirm` is `Some`.
    delete_confirm_card: gtk::Box,
    delete_confirm_lines: gtk::Box,
    delete_confirm_line_labels: RefCell<Vec<gtk::Label>>,
    delete_confirm_btn: gtk::Button,
    delete_cancel_btn: gtk::Button,

    // ── Full-PDS panel (gated on level = hosted_full) ───────────────────
    fullpds_group: adw::PreferencesGroup,
    external_apps_toggle: adw::SwitchRow,
    credentials_group: adw::PreferencesGroup,
    credential_rows: RefCell<Vec<adw::ActionRow>>,

    // ── D10 authoring-delegation row ────────────────────────────────────
    /// Hosts the whole `atproto-delegation-*` surface. Torn down and rebuilt
    /// each render (the credentials/sessions idiom) because the row has two
    /// shapes — provisioned and not — rather than one shape with hidden leaves.
    delegation_group: adw::PreferencesGroup,
    delegation_rows: RefCell<Vec<gtk::Widget>>,
}

/// Per-page context threaded through gesture closures.
struct AtprotoPageCtx {
    machine: Arc<AtprotoSettingsMachine>,
    rt: tokio::runtime::Handle,
    /// Reentrancy guard: suppresses the `toggled`/`active-notify` re-fire from
    /// `render`'s own programmatic `set_active` on the radios and switches (the
    /// `settings/mail.rs` idiom). One guard spans the whole render.
    guard: Rc<Cell<bool>>,
    /// Secrets revealed (by mint or explicit reveal) this session, keyed by
    /// `credential_id`. Never persisted, never part of the snapshot (D3).
    revealed: Rc<RefCell<HashMap<String, String>>>,
    w: AtprotoPageWidgets,
}

/// Build the **Bluesky** settings sub-page. Static skeleton only — data +
/// gestures are wired by `wire_machine` (no-op when no client is registered).
pub fn build_atproto_settings_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(BS::TITLE)
        .icon_name("network-transmit-receive-symbolic")
        .build();

    // --- Top group: heading + page landmark + page-level error ---
    let top_group = adw::PreferencesGroup::builder()
        .title(glib::markup_escape_text(BS::TITLE).as_str())
        .build();
    top_group.set_header_suffix(Some(&super::marker("page-heading")));
    top_group.add(&super::marker("atproto-page")); // page landmark

    let error_label = gtk::Label::builder().visible(false).wrap(true).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);
    page.add(&top_group);

    // --- Recovery-fork contest ceremony: LEADS the page, above the depth
    //     selector (behavior/atproto-identity-custody.md § The 72 h
    //     recovery-fork contest). Hidden until a standing custody violation
    //     names an op this client's ring protects. This function derives
    //     nothing: every string below is machine-composed and rendered
    //     verbatim — see `apps/fauna-tui/src/settings/atproto.rs::contest_elements`,
    //     the reference shape every shell copies. ---
    let contest_group = adw::PreferencesGroup::builder().visible(false).build();
    let contest_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    set_test_id(&contest_card, ids::ATPROTO_CONTEST_CARD);
    let contest_heading = gtk::Label::builder()
        .label(BS::CONTEST_CARD_HEADING)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    contest_card.append(&contest_heading);
    let contest_detail = gtk::Label::builder().wrap(true).xalign(0.0).build();
    set_test_id(&contest_detail, ids::ATPROTO_CONTEST_DETAIL);
    contest_card.append(&contest_detail);
    let contest_deadline = gtk::Label::builder()
        .visible(false)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    set_test_id(&contest_deadline, ids::ATPROTO_CONTEST_DEADLINE);
    contest_card.append(&contest_deadline);
    // Decision 2 (no dead button on a hopeless state): the button exists
    // only where pressing it can work — `show_contest`, never inferred from
    // `state`. No `declare_wire_kind`: decision 9, this is wholly
    // client-side (the client's own HTTPS to the public PLC directory), no
    // nest call at all.
    let contest_btn = gtk::Button::builder()
        .label(BS::CONTEST_BUTTON)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Start)
        .visible(false)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&contest_btn, ids::ATPROTO_CONTEST);
    contest_card.append(&contest_btn);
    contest_group.add(&contest_card);

    let contest_confirm_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    set_test_id(&contest_confirm_card, ids::ATPROTO_CONTEST_CONFIRM_CARD);
    contest_confirm_card.set_visible(false);
    let contest_confirm_lines = gtk::Box::new(gtk::Orientation::Vertical, 4);
    contest_confirm_card.append(&contest_confirm_lines);
    let contest_confirm_btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    contest_confirm_btn_row.set_halign(gtk::Align::End);
    contest_confirm_btn_row.set_margin_top(8);
    let contest_cancel_btn = gtk::Button::with_label(BS::CONTEST_CANCEL_BUTTON);
    set_test_id(&contest_cancel_btn, ids::ATPROTO_CONTEST_CANCEL);
    let contest_confirm_btn = gtk::Button::builder()
        .label(BS::CONTEST_CONFIRM_BUTTON)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&contest_confirm_btn, ids::ATPROTO_CONTEST_CONFIRM);
    contest_confirm_btn_row.append(&contest_cancel_btn);
    contest_confirm_btn_row.append(&contest_confirm_btn);
    contest_confirm_card.append(&contest_confirm_btn_row);
    contest_group.add(&contest_confirm_card);
    page.add(&contest_group);

    // --- Depth selector ---
    let selector_group = adw::PreferencesGroup::builder()
        .title(BS::DEPTH_HEADING)
        .build();
    let depth_selector = gtk::Box::new(gtk::Orientation::Vertical, 8);
    set_test_id(&depth_selector, ids::ATPROTO_DEPTH_SELECTOR);
    // Rung ids, titles and descriptions come from the shared catalog
    // (`atproto.md` § Where logic lives) — linux previously carried its own
    // `DEPTH_LEVELS` const plus this parallel `rung_titles` array, two tables
    // held in sync by index alone.
    let mut rungs: Vec<gtk::CheckButton> = Vec::with_capacity(4);
    for rung in depth_level_options() {
        let title = resolve(&rung.title);
        let desc = resolve(&rung.description);
        let leader = rungs.first().cloned();
        let (rung_box, btn) = build_radio_rung(&rung.ui_id, &title, &desc, leader.as_ref());
        // A rung's gesture is `select_level`, and BOTH paths that reach the
        // nest at all — the effect-free Off→Linked move applied on select, and
        // the card's confirm — end in the same `set_integration_level` write
        // (`machine.rs::perform_staged_transition`). Declared here rather than
        // inside `build_radio_rung`: the DID-method radios share that helper
        // and are pure-local (`set_did_method` touches no wire).
        crate::offline_gate::declare_wire_kind(&btn, "fauna.bridges.atproto.set_integration_level");
        depth_selector.append(&rung_box);
        rungs.push(btn);
    }
    let depth_reason = gtk::Label::builder()
        .visible(false)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    depth_selector.append(&depth_reason);
    selector_group.add(&depth_selector);
    page.add(&selector_group);
    let depth_rungs: [gtk::CheckButton; 4] = rungs
        .try_into()
        .expect("depth_level_options() has exactly four entries");

    // --- Transition card ---
    let card_group = adw::PreferencesGroup::builder()
        .title(BS::DEPTH_CARD_HEADING)
        .visible(false)
        .build();
    let card_lines = gtk::Box::new(gtk::Orientation::Vertical, 6);
    set_test_id(&card_lines, ids::ATPROTO_DEPTH_CONFIRM_CARD);
    card_group.add(&card_lines);
    let history_backfill = gtk::CheckButton::with_label(BS::HISTORY_BACKFILL_LABEL);
    history_backfill.set_visible(false);
    set_test_id(&history_backfill, ids::ATPROTO_HISTORY_BACKFILL);
    card_group.add(&history_backfill);
    let card_btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    card_btn_row.set_halign(gtk::Align::End);
    card_btn_row.set_margin_top(8);
    let cancel_btn = gtk::Button::with_label(BS::DEPTH_CANCEL_BUTTON);
    set_test_id(&cancel_btn, ids::ATPROTO_DEPTH_CANCEL);
    let confirm_btn = gtk::Button::builder()
        .label(BS::DEPTH_CONFIRM_BUTTON)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&confirm_btn, ids::ATPROTO_DEPTH_CONFIRM);
    // The card's confirm is the ONE nest call a staged level change makes; the
    // cancel beside it and the history-backfill opt-in above are pure-local
    // machine mutations and declare nothing.
    crate::offline_gate::declare_wire_kind(
        &confirm_btn,
        "fauna.bridges.atproto.set_integration_level",
    );
    card_btn_row.append(&cancel_btn);
    card_btn_row.append(&confirm_btn);
    card_group.add(&card_btn_row);
    page.add(&card_group);

    // --- Linked-account panel (level = linked): an empty wrapper the shared
    //     Bridges detail content is embedded into on first render. Untitled on
    //     purpose — the embedded content brings its own group headings, and a
    //     wrapper title would double them. ---
    let linked_group = adw::PreferencesGroup::builder().visible(false).build();
    page.add(&linked_group);

    // --- Hosted panel ---
    let hosted_group = adw::PreferencesGroup::builder().visible(false).build();
    let did_method_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    set_test_id(&did_method_box, ids::ATPROTO_DID_METHOD);
    let did_heading = gtk::Label::builder()
        .label(BS::DID_METHOD_HEADING)
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    did_method_box.append(&did_heading);
    let (plc_box, did_plc) = build_radio_rung(
        "atproto-did-method-plc",
        BS::DID_METHOD_PLC_TITLE,
        BS::DID_METHOD_PLC_DESC,
        None,
    );
    let (web_box, did_web) = build_radio_rung(
        "atproto-did-method-web",
        BS::DID_METHOD_WEB_TITLE,
        BS::DID_METHOD_WEB_DESC,
        Some(&did_plc),
    );
    did_method_box.append(&plc_box);
    did_method_box.append(&web_box);
    let handle_preview = gtk::Label::builder()
        .visible(false)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    did_method_box.append(&handle_preview);
    hosted_group.add(&did_method_box);
    page.add(&hosted_group);

    // --- Identity summary: its OWN group, gated on the identity, not the
    //     level (see the render-time comment on this field for why it must
    //     not sit inside `hosted_group` — that group's visibility is
    //     `show_hosted`, which is false exactly at Off/Linked, the two
    //     states `ui/atproto.md` § Errors & edge cases requires this to
    //     survive). ---
    let identity_group = adw::PreferencesGroup::builder().visible(false).build();
    // Visibility lives on `identity_group` alone; this label stays visible so
    // the group's own toggle is the single source of truth.
    let hosted_handle = gtk::Label::builder().xalign(0.0).build();
    set_test_id(&hosted_handle, ids::ATPROTO_HOSTED_HANDLE);
    identity_group.add(&hosted_handle);
    page.add(&identity_group);

    // --- Delete presence: the button opens its own confirm card, never the
    //     depth selector's — the ceremony lives in the shared machine
    //     (`open_delete_confirm`/`cancel_delete`/`confirm_delete`), so this
    //     page derives nothing; the card's lines are the machine's, rendered
    //     verbatim (`apps/fauna-tui/src/settings/atproto.rs`, the reference
    //     shape every shell copies — same pattern as the contest ceremony
    //     above). ---
    let delete_group = adw::PreferencesGroup::builder().visible(false).build();
    let delete_btn = gtk::Button::builder()
        .label(BS::DELETE_PRESENCE_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&delete_btn, ids::ATPROTO_DELETE_PRESENCE);
    let delete_row = adw::ActionRow::builder().activatable(false).build();
    delete_row.add_suffix(&delete_btn);
    delete_group.add(&delete_row);

    let delete_confirm_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    set_test_id(&delete_confirm_card, ids::ATPROTO_DELETE_CONFIRM_CARD);
    delete_confirm_card.set_visible(false);
    let delete_confirm_lines = gtk::Box::new(gtk::Orientation::Vertical, 4);
    delete_confirm_card.append(&delete_confirm_lines);
    let delete_confirm_btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    delete_confirm_btn_row.set_halign(gtk::Align::End);
    delete_confirm_btn_row.set_margin_top(8);
    let delete_cancel_btn = gtk::Button::with_label(BS::DELETE_CANCEL_BUTTON);
    set_test_id(&delete_cancel_btn, ids::ATPROTO_DELETE_CANCEL);
    let delete_confirm_btn = gtk::Button::builder()
        .label(BS::DELETE_CONFIRM_BUTTON)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&delete_confirm_btn, ids::ATPROTO_DELETE_CONFIRM);
    delete_confirm_btn_row.append(&delete_cancel_btn);
    delete_confirm_btn_row.append(&delete_confirm_btn);
    delete_confirm_card.append(&delete_confirm_btn_row);
    delete_group.add(&delete_confirm_card);
    page.add(&delete_group);

    // --- Full-PDS panel: kill-switch + mint (gated on level = hosted_full) ---
    let fullpds_group = adw::PreferencesGroup::builder().visible(false).build();
    let external_apps_toggle = adw::SwitchRow::builder()
        .title(BS::EXTERNAL_APPS_TOGGLE)
        .active(true)
        .build();
    set_test_id(&external_apps_toggle, ids::ATPROTO_EXTERNAL_APPS_ENABLE);
    crate::offline_gate::declare_wire_kind(
        &external_apps_toggle,
        "fauna.bridges.atproto.set_external_apps_enabled",
    );
    fullpds_group.add(&external_apps_toggle);
    let mint_btn = gtk::Button::builder()
        .label(BS::MINT_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&mint_btn, ids::ATPROTO_APP_CREDENTIAL_MINT);
    // Mint's one nest call is `provision_app_credential` — the verifier upload
    // (`machine.rs::mint` → `fauna_client_bridges::atproto`); the secret itself
    // never crosses the wire (D3), and the local `fauna.state.atproto` write that follows
    // is OfflineSafe, so the provisioning write is what BINDS the gesture.
    crate::offline_gate::declare_wire_kind(
        &mint_btn,
        "fauna.bridges.atproto.provision_app_credential",
    );
    let mint_row = adw::ActionRow::builder().activatable(false).build();
    mint_row.add_suffix(&mint_btn);
    fullpds_group.add(&mint_row);
    page.add(&fullpds_group);

    let credentials_group = adw::PreferencesGroup::builder()
        .title(BS::APP_CREDENTIALS_HEADING)
        .visible(false)
        .build();
    page.add(&credentials_group);

    // --- D10 authoring-delegation row: what authorizes an external app to
    //     *post* as this account, as opposed to merely signing in (everything
    //     above governs that). Last on the page, matching tui's order. ---
    let delegation_group = adw::PreferencesGroup::builder()
        .title(BS::DELEGATION_HEADING)
        .visible(false)
        .build();
    page.add(&delegation_group);

    let widgets = AtprotoPageWidgets {
        error_label,
        contest_group,
        contest_card,
        contest_detail,
        contest_deadline,
        contest_btn,
        contest_confirm_card,
        contest_confirm_lines,
        contest_confirm_line_labels: RefCell::new(Vec::new()),
        contest_confirm_btn,
        contest_cancel_btn,
        depth_selector,
        depth_rungs,
        depth_reason,
        card_group,
        card_lines,
        card_line_labels: RefCell::new(Vec::new()),
        history_backfill,
        confirm_btn,
        cancel_btn,
        linked_group,
        linked_content: RefCell::new(None),
        linked_built_from: RefCell::new(None),
        linked_follows_list: RefCell::new(None),
        hosted_group,
        did_method_box,
        did_plc,
        did_web,
        handle_preview,
        identity_group,
        hosted_handle,
        delete_group,
        delete_btn,
        delete_confirm_card,
        delete_confirm_lines,
        delete_confirm_line_labels: RefCell::new(Vec::new()),
        delete_confirm_btn,
        delete_cancel_btn,
        fullpds_group,
        external_apps_toggle,
        credentials_group,
        credential_rows: RefCell::new(Vec::new()),
        delegation_group,
        delegation_rows: RefCell::new(Vec::new()),
    };

    wire_machine(&page, mint_btn, widgets);

    page
}

/// One radio rung: a titled `gtk::CheckButton` carrying `id`, with a dim
/// description line under it, in a vertical box. Grouped under `leader` (the
/// first rung) so exactly one is active. The CheckButton is the addressable +
/// clickable + sensitivity-bearing element; the e2e reads its `state` attr and
/// `is_enabled`.
fn build_radio_rung(
    id: &str,
    title: &str,
    desc: &str,
    leader: Option<&gtk::CheckButton>,
) -> (gtk::Box, gtk::CheckButton) {
    let rung = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let btn = gtk::CheckButton::with_label(title);
    if let Some(leader) = leader {
        btn.set_group(Some(leader));
    }
    set_test_id(&btn, id);
    rung.append(&btn);
    let desc_label = gtk::Label::builder()
        .label(desc)
        .wrap(true)
        .xalign(0.0)
        .margin_start(28)
        .css_classes(["dim-label"])
        .build();
    rung.append(&desc_label);
    (rung, btn)
}

/// Connect the page to the shared `AtprotoSettingsMachine`, hydrate on mount,
/// and wire every gesture. No-op (page stays at static placeholders) when no
/// client is registered.
fn wire_machine(page: &adw::PreferencesPage, mint_btn: gtk::Button, widgets: AtprotoPageWidgets) {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        None => return,
    };

    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let observer: Arc<dyn AtprotoSettingsObserver> = Arc::new(GtkAtprotoSettingsObserver { tx });
    let machine = match crate::mail_glue::build_atproto_settings_machine(&client, observer) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("atproto page: build machine failed: {e}");
            return;
        }
    };

    let ctx = Rc::new(AtprotoPageCtx {
        machine: Arc::clone(&machine),
        rt: client.runtime_handle(),
        guard: Rc::new(Cell::new(false)),
        revealed: Rc::new(RefCell::new(HashMap::new())),
        w: widgets,
    });

    // Initial hydrate.
    {
        let m = Arc::clone(&machine);
        client
            .runtime_handle()
            .spawn(async move { m.refresh().await });
    }

    // Push-observer render loop: repaints on every machine notification,
    // including the ones a gesture's own internal `refresh()` fires.
    {
        let ctx = Rc::clone(&ctx);
        crate::async_helper::spawn_wake_loop(rx, move || {
            render(&ctx);
            glib::ControlFlow::Continue
        });
    }

    // Re-hydrate whenever the page is shown (a sibling device may have changed
    // level / minted / revoked since this page was built). The bridges re-fetch
    // rides along so the Linked panel reflects a link/unlink performed
    // elsewhere; its reply lands in the shared snapshot and calls us back
    // through `notify_bridges_changed`.
    {
        let m = Arc::clone(&machine);
        let rt = client.runtime_handle();
        page.connect_map(move |_| {
            let m = Arc::clone(&m);
            rt.spawn(async move { m.refresh().await });
            // Re-resolve the client per invocation rather than capturing it:
            // the settings subsystem holds a `Weak` on purpose, so a strong
            // capture in a long-lived widget closure would keep the client
            // alive past sign-out.
            if let Some(c) = crate::settings::get_client() {
                c.fetch_bridges();
            }
        });
    }

    // Repaint the Linked panel whenever a `fauna.bridges.list` reply lands.
    // Weak on purpose: the page's own widget closures own `ctx`, so this hook
    // must not be the thing that keeps a torn-down page (and its machine)
    // alive — it just goes inert once the page is gone.
    {
        let ctx = Rc::downgrade(&ctx);
        crate::settings::set_bridges_changed_handler(Rc::new(move || {
            if let Some(ctx) = ctx.upgrade() {
                render(&ctx);
            }
        }));
    }

    // Repaint the Linked panel's follows list whenever a
    // `fauna.bridges.list_follows` reply for "bluesky" lands — the
    // panel never got one before this hook existed, so its follows section
    // always showed its empty placeholder. Weak, same reason as the
    // bridges-changed hook above; re-resolves the client per firing rather
    // than capturing it, same reason `page.connect_map`'s re-hydrate does.
    {
        let ctx = Rc::downgrade(&ctx);
        crate::settings::set_bridge_follows_loaded_handler(Rc::new(move |bridge_id, follows| {
            if bridge_id != "bluesky" {
                return;
            }
            let Some(ctx) = ctx.upgrade() else {
                return;
            };
            let Some(follows_list) = ctx.w.linked_follows_list.borrow().clone() else {
                return;
            };
            let Some(client) = crate::settings::get_client() else {
                return;
            };
            crate::views::bridges::detail::populate_follows_list(
                &follows_list,
                follows,
                move |follow_id| client.remove_bridge_follow("bluesky", follow_id),
            );
        }));
    }

    // Re-fetch on a `fauna.atproto.consent_requested` push (F4 rung 2): an
    // external app is asking to sign in and a browser is blocked waiting for
    // the answer, so this must not wait for the next re-navigation. Re-lists
    // via the machine's own `refresh()` rather than folding the push payload
    // in — `list_pending_consents` stays the one source of the card set, and
    // the observer's own render loop repaints once it lands (mirrors
    // `page.connect_map`'s re-hydrate above). Weak, same reason as the
    // bridges hook: must not keep a torn-down machine alive.
    {
        let machine = Arc::downgrade(&machine);
        let rt = client.runtime_handle();
        crate::settings::set_atproto_rehydrate_handler(Rc::new(move || {
            if let Some(m) = machine.upgrade() {
                let rt = rt.clone();
                rt.spawn(async move { m.refresh().await });
            }
        }));
    }

    // --- Recovery-fork contest: open / cancel are pure-local machine
    //     mutations (they call `observer.on_changed()` internally, same as
    //     `cancel_transition` below — fires the render loop, no manual
    //     render call needed here). Confirm is the one gesture that submits
    //     to the network (the client's own connection to the PLC directory,
    //     never the nest), so it takes `spawn_with_snapshot` like
    //     `confirm_transition`. ---
    {
        let contest_btn = ctx.w.contest_btn.clone();
        let ctx = Rc::clone(&ctx);
        contest_btn.connect_clicked(move |_| {
            ctx.machine.open_contest_confirm(); // fires on_changed → render loop
        });
    }
    {
        let contest_cancel_btn = ctx.w.contest_cancel_btn.clone();
        let ctx = Rc::clone(&ctx);
        contest_cancel_btn.connect_clicked(move |_| {
            ctx.machine.cancel_contest(); // fires on_changed → render loop
        });
    }
    {
        let contest_confirm_btn = ctx.w.contest_confirm_btn.clone();
        let ctx = Rc::clone(&ctx);
        contest_confirm_btn.connect_clicked(move |_| {
            let machine = Arc::clone(&ctx.machine);
            let rt = ctx.rt.clone();
            let ctx = Rc::clone(&ctx);
            spawn_with_snapshot(
                &rt,
                move || async move { machine.request_contest().await },
                move |()| render(&ctx),
            );
        });
    }

    // --- Delete presence: open / cancel are pure-local machine mutations
    //     (like the contest ceremony above); confirm is the one gesture that
    //     submits to the network, so it takes `spawn_with_snapshot`. ---
    {
        let delete_btn = ctx.w.delete_btn.clone();
        let ctx = Rc::clone(&ctx);
        delete_btn.connect_clicked(move |_| {
            ctx.machine.open_delete_confirm(); // fires on_changed → render loop
        });
    }
    {
        let delete_cancel_btn = ctx.w.delete_cancel_btn.clone();
        let ctx = Rc::clone(&ctx);
        delete_cancel_btn.connect_clicked(move |_| {
            ctx.machine.cancel_delete(); // fires on_changed → render loop
        });
    }
    {
        let delete_confirm_btn = ctx.w.delete_confirm_btn.clone();
        let ctx = Rc::clone(&ctx);
        delete_confirm_btn.connect_clicked(move |_| {
            let machine = Arc::clone(&ctx.machine);
            let rt = ctx.rt.clone();
            let ctx = Rc::clone(&ctx);
            spawn_with_snapshot(
                &rt,
                move || async move { machine.confirm_delete().await },
                move |()| render(&ctx),
            );
        });
    }

    // --- Depth selector: select a level (never mutates directly — the machine
    //     stages the card, or applies Off→Linked immediately). ---
    for (rung, opt) in ctx.w.depth_rungs.iter().zip(depth_level_options()) {
        let ctx = Rc::clone(&ctx);
        let level = opt.level;
        rung.connect_toggled(move |b| {
            if ctx.guard.get() || !b.is_active() {
                return;
            }
            let machine = Arc::clone(&ctx.machine);
            let rt = ctx.rt.clone();
            let ctx = Rc::clone(&ctx);
            let level = level.clone();
            spawn_with_snapshot(
                &rt,
                move || async move { machine.select_level(level).await },
                move |()| render(&ctx),
            );
        });
    }

    // --- Transition card: confirm / cancel / history-backfill ---
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.confirm_btn.connect_clicked({
            let ctx = Rc::clone(&ctx);
            move |_| {
                let machine = Arc::clone(&ctx.machine);
                let rt = ctx.rt.clone();
                let ctx = Rc::clone(&ctx);
                spawn_with_snapshot(
                    &rt,
                    move || async move { machine.confirm_transition().await },
                    move |()| render(&ctx),
                );
            }
        });
    }
    {
        let cancel_btn = ctx.w.cancel_btn.clone();
        let ctx = Rc::clone(&ctx);
        cancel_btn.connect_clicked(move |_| {
            ctx.machine.cancel_transition(); // fires on_changed → render loop
        });
    }
    {
        let history_backfill = ctx.w.history_backfill.clone();
        let ctx = Rc::clone(&ctx);
        history_backfill.connect_toggled(move |b| {
            if ctx.guard.get() {
                return;
            }
            ctx.machine.set_history_backfill(b.is_active());
        });
    }

    // --- Hosted panel: DID-method radio (pre-mint) ---
    for (btn, method) in [(&ctx.w.did_plc, "plc"), (&ctx.w.did_web, "web")] {
        let ctx = Rc::clone(&ctx);
        let method = method.to_string();
        btn.connect_toggled(move |b| {
            if ctx.guard.get() || !b.is_active() {
                return;
            }
            ctx.machine.set_did_method(method.clone());
        });
    }

    // --- Full-PDS panel: kill-switch + mint ---
    {
        let external_apps_toggle = ctx.w.external_apps_toggle.clone();
        let ctx = Rc::clone(&ctx);
        external_apps_toggle.connect_active_notify(move |sw| {
            if ctx.guard.get() {
                return;
            }
            let enabled = sw.is_active();
            let machine = Arc::clone(&ctx.machine);
            let rt = ctx.rt.clone();
            let ctx = Rc::clone(&ctx);
            spawn_with_snapshot(
                &rt,
                move || async move {
                    machine.set_external_apps_enabled(enabled).await;
                },
                move |()| render(&ctx),
            );
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        mint_btn.connect_clicked(move |_| {
            let machine = Arc::clone(&ctx.machine);
            let rt = ctx.rt.clone();
            let ctx = Rc::clone(&ctx);
            spawn_with_snapshot(
                &rt,
                move || {
                    let machine = Arc::clone(&machine);
                    async move {
                        let before: HashSet<String> = machine
                            .snapshot()
                            .credentials
                            .into_iter()
                            .map(|c| c.credential_id)
                            .collect();
                        let label = BS::default_credential_label(&(before.len() + 1).to_string());
                        let result = machine.mint(label, false).await;
                        result.ok().map(|secret| {
                            let new_id = machine
                                .snapshot()
                                .credentials
                                .into_iter()
                                .map(|c| c.credential_id)
                                .find(|id| !before.contains(id))
                                .unwrap_or_default();
                            (new_id, secret.as_str().to_string())
                        })
                    }
                },
                move |new_secret: Option<(String, String)>| {
                    if let Some((id, secret)) = new_secret
                        && !id.is_empty()
                    {
                        ctx.revealed.borrow_mut().insert(id, secret);
                    }
                    render(&ctx);
                },
            );
        });
    }

    // First paint (the observer channel only fires on a *change*; the initial
    // `refresh()` spawn races it, so paint once now from whatever state exists).
    render(&ctx);
}

/// Re-render the whole page off a fresh `AtprotoSettingsSnapshot`.
fn render(ctx: &Rc<AtprotoPageCtx>) {
    let snap = ctx.machine.snapshot();
    let w = &ctx.w;

    let resolved_error = snap.error.as_ref().map(resolve);
    super::render_error_label(&w.error_label, resolved_error.as_deref());

    // ── Recovery-fork contest: LEADS the page, above the selector ──────
    match &snap.contest {
        Some(card) => {
            w.contest_group.set_visible(true);
            set_test_attr(&w.contest_card, "state", &card.state);
            w.contest_detail.set_text(&resolve(&card.detail));
            match &card.deadline {
                Some(deadline) => {
                    w.contest_deadline.set_text(&resolve(deadline));
                    w.contest_deadline.set_visible(true);
                }
                None => w.contest_deadline.set_visible(false),
            }
            // Decision 2: the button exists only where pressing it can work.
            w.contest_btn.set_visible(card.show_contest);
        }
        None => {
            w.contest_group.set_visible(false);
        }
    }
    match &snap.contest_confirm {
        Some(confirm) => {
            w.contest_confirm_card.set_visible(true);
            let mut labels = w.contest_confirm_line_labels.borrow_mut();
            for label in labels.drain(..) {
                w.contest_confirm_lines.remove(&label);
            }
            for line in &confirm.lines {
                let label = gtk::Label::builder()
                    .label(resolve(line))
                    .wrap(true)
                    .xalign(0.0)
                    .build();
                w.contest_confirm_lines.append(&label);
                labels.push(label);
            }
            w.contest_confirm_btn.set_sensitive(!confirm.in_progress);
            w.contest_cancel_btn.set_sensitive(!confirm.in_progress);
        }
        None => {
            w.contest_confirm_card.set_visible(false);
        }
    }

    ctx.guard.set(true);

    // ── Depth selector ──────────────────────────────────────────────────
    set_test_attr(&w.depth_selector, "state", &snap.level);
    let reason_text = snap
        .hosted_gate_reason
        .as_ref()
        .map(resolve)
        .unwrap_or_default();
    for (rung, opt) in w.depth_rungs.iter().zip(depth_level_options()) {
        let active = snap.level == opt.level;
        rung.set_active(active);
        set_test_attr(rung, "state", if active { "active" } else { "inactive" });
        // `hosted` is the catalog's fact, not a `starts_with("hosted")` sniff
        // this page re-runs on a level string it did not define.
        let gated = opt.hosted && !snap.hosted_allowed;
        // A hosted rung the user is *already at* stays sensitive so a step-down
        // is reachable even if the domain later stops being public; the gate
        // only ever blocks *entering* a hosted level from a lower one.
        rung.set_sensitive(!gated || active);
        set_test_attr(rung, "reason", if gated { "gated" } else { "ok" });
    }
    if snap.hosted_allowed || reason_text.is_empty() {
        w.depth_reason.set_visible(false);
    } else {
        w.depth_reason.set_text(&reason_text);
        w.depth_reason.set_visible(true);
    }

    // ── Transition card ─────────────────────────────────────────────────
    match &snap.pending_transition {
        Some(card) => {
            w.card_group.set_visible(true);
            let mut labels = w.card_line_labels.borrow_mut();
            for label in labels.drain(..) {
                w.card_lines.remove(&label);
            }
            for line in &card.lines {
                let label = gtk::Label::builder()
                    .label(resolve(line))
                    .wrap(true)
                    .xalign(0.0)
                    .build();
                w.card_lines.append(&label);
                labels.push(label);
            }
            w.history_backfill.set_visible(card.show_history_backfill);
            w.history_backfill.set_active(snap.history_backfill);
            w.confirm_btn.set_sensitive(!card.in_progress);
            w.cancel_btn.set_sensitive(!card.in_progress);
        }
        None => {
            w.card_group.set_visible(false);
        }
    }

    // ── Linked-account panel: the shared bridge surface at level = linked ──
    let at_linked = snap.level == "linked";
    w.linked_group.set_visible(at_linked);
    if at_linked {
        sync_linked_panel(ctx);
    }

    // ── Hosted panel: at (or entering) a hosted level ───────────────────
    let targeting_hosted = snap
        .pending_transition
        .as_ref()
        .is_some_and(|p| p.target_level.starts_with("hosted"));
    let at_hosted = snap.level.starts_with("hosted");
    let show_hosted = at_hosted || targeting_hosted;
    w.hosted_group.set_visible(show_hosted);
    if show_hosted {
        // Pre-mint: DID-method radio + the either-way handle line.
        w.did_method_box.set_visible(snap.show_did_method_radio);
        if snap.show_did_method_radio {
            w.did_plc.set_active(snap.did_method == "plc");
            w.did_web.set_active(snap.did_method == "web");
            if snap.handle_preview.is_empty() {
                w.handle_preview.set_visible(false);
            } else {
                w.handle_preview
                    .set_text(&BS::handle_either_way(&snap.handle_preview));
                w.handle_preview.set_visible(true);
            }
        }
    }

    // ── The identity summary: gated on the IDENTITY, not on the level ───
    //
    // `ui/atproto.md` § Errors & edge cases: "A deactivated identity at level
    // Off/Linked: the identity summary renders (marked deactivated) so the user
    // can see what re-enabling restores." This used to sit inside `show_hosted`
    // above, the one place the rule can never hold — the states it names are
    // exactly the two that gate closes on. `apps/fauna-tui/src/settings/
    // bluesky.rs` leads the fix; this is the trickle-down leg. Its own group
    // (never `hosted_group`, whose visibility IS `show_hosted`).
    match &snap.identity {
        Some(id) => {
            let status = resolve(&fauna_atproto_settings_machine::identity_status_label(
                &id.status,
            ));
            w.hosted_handle.set_text(&format!(
                "{} · {} · {}",
                BS::hosted_handle_prefix(&id.handle),
                BS::hosted_method_prefix(&id.method),
                status
            ));
            w.identity_group.set_visible(true);
        }
        None => w.identity_group.set_visible(false),
    }

    // ── Delete presence: whenever a hosted identity exists ──────────────
    w.delete_group.set_visible(snap.show_delete_presence);
    // Its own confirm card — never the depth selector's. The copy is the
    // machine's, rendered verbatim.
    match &snap.delete_confirm {
        Some(confirm) => {
            w.delete_confirm_card.set_visible(true);
            let mut labels = w.delete_confirm_line_labels.borrow_mut();
            for label in labels.drain(..) {
                w.delete_confirm_lines.remove(&label);
            }
            for line in &confirm.lines {
                let label = gtk::Label::builder()
                    .label(resolve(line))
                    .wrap(true)
                    .xalign(0.0)
                    .build();
                w.delete_confirm_lines.append(&label);
                labels.push(label);
            }
            w.delete_confirm_btn.set_sensitive(!confirm.in_progress);
            w.delete_cancel_btn.set_sensitive(!confirm.in_progress);
        }
        None => {
            w.delete_confirm_card.set_visible(false);
        }
    }

    // ── Full-PDS panel: gated on level = hosted_full ────────────────────
    let at_full = snap.level == "hosted_full";
    w.fullpds_group.set_visible(at_full);
    w.credentials_group.set_visible(at_full);
    w.external_apps_toggle
        .set_active(snap.external_apps_enabled);
    set_test_attr(
        &w.external_apps_toggle,
        "state",
        if snap.external_apps_enabled {
            "on"
        } else {
            "off"
        },
    );

    ctx.guard.set(false);

    // Credentials list: tear down + rebuild from the snapshot.
    {
        let mut rows = w.credential_rows.borrow_mut();
        for row in rows.drain(..) {
            w.credentials_group.remove(&row);
        }
        if snap.credentials.is_empty() {
            let placeholder = adw::ActionRow::builder()
                .title(BS::APP_CREDENTIALS_EMPTY)
                .build();
            w.credentials_group.add(&placeholder);
            rows.push(placeholder);
        } else {
            for cred in &snap.credentials {
                let row = build_credential_row(ctx, cred);
                w.credentials_group.add(&row);
                rows.push(row);
            }
        }
    }

    // The D10 authoring-delegation row: same tear-down/rebuild shape.
    w.delegation_group.set_visible(at_full);
    sync_delegation_row(ctx, snap.delegation.as_ref());
}

/// Rebuild the D10 authoring-delegation surface (`atproto-delegation-*`) — what
/// authorizes an external ATProto app to *post* as this account, as opposed to
/// merely signing in (the credentials and connected-apps groups above govern
/// that). `atproto-pds-full.md` § App surface; the six IDs were user-approved
/// 2026-07-29 and `-last-used` 2026-07-31.
///
/// Two states, one always-present control — the shape tui set as lead app:
///
/// - **`None`** — no delegation, or one whose stored cert failed the
///   client-side verify under the account's own identity key. The row and its
///   leaves are **withheld**, never rendered as a grant the user cannot be
///   shown to have made (the mismatch surfaces on `error-message`, which the
///   machine has already set). Only `atproto-delegation-authorize` renders.
/// - **`Some`** — the leaves render: scope, lasts-until, liveness, last-used.
///   `-authorize` STAYS rendered, because re-authorizing is the renewal
///   gesture — provisioning overwrites the cert, so a lapsed grant recovers in
///   one gesture with no revoke first.
fn sync_delegation_row(ctx: &Rc<AtprotoPageCtx>, row: Option<&DelegationRow>) {
    let w = &ctx.w;
    let mut widgets = w.delegation_rows.borrow_mut();
    for widget in widgets.drain(..) {
        w.delegation_group.remove(&widget);
    }

    let Some(row) = row else {
        let empty = adw::ActionRow::builder()
            .title(BS::DELEGATION_EMPTY)
            .title_lines(0)
            .build();
        let authorize = delegation_button(
            ctx,
            "atproto-delegation-authorize",
            BS::DELEGATION_AUTHORIZE_BUTTON,
            true,
        );
        empty.add_suffix(&authorize);
        w.delegation_group.add(&empty);
        widgets.push(empty.upcast());
        return;
    };

    // The row landmark: the addressable container the e2e's
    // `is_delegation_row_visible` reads, with the leaves painted inside it.
    // A `gtk::Box` rather than an `adw::ActionRow` — the leaves are a stacked
    // block, not a title/subtitle pair, and this is the same shape
    // a plain card on this page.
    let landmark = gtk::Box::new(gtk::Orientation::Vertical, 2);
    landmark.add_css_class("card");
    set_test_id(&landmark, ids::ATPROTO_DELEGATION_ROW);
    let leaves = gtk::Box::new(gtk::Orientation::Vertical, 2);
    leaves.set_margin_top(8);
    leaves.set_margin_bottom(8);
    leaves.set_margin_start(12);
    leaves.set_margin_end(12);

    let scope_labels: Vec<String> = row.capability_labels().iter().map(resolve).collect();
    leaves.append(&delegation_leaf(
        "atproto-delegation-scope",
        &BS::delegation_scope_prefix(&scope_labels.join(", ")),
    ));

    // MICROseconds on this row — these come from the signed cert, not the wire,
    // unlike the credential/session rows above which carry milliseconds.
    let authorized =
        fauna_core::format::format_unix_local((row.authorized_at_micros / 1_000_000) as i64);
    leaves.append(&delegation_leaf(
        "atproto-delegation-lasts-until",
        &match row.expires_at_micros {
            Some(micros) => BS::delegation_lasts_until(
                &authorized,
                &fauna_core::format::format_unix_local((micros / 1_000_000) as i64),
            ),
            None => BS::delegation_lasts_until_no_expiry(&authorized),
        },
    ));

    // The liveness wire spelling rides the `state` attr so the e2e asserts the
    // STATE, not its prose — an unrecognized spelling from a newer nest still
    // renders (degrade, never fail to decode).
    let status = delegation_leaf("atproto-delegation-status", &resolve(&row.status_label()));
    set_test_attr(&status, "state", &row.liveness);
    leaves.append(&status);

    // The ADVISORY last-use hint (D10 § Audit; ID user-approved 2026-07-31).
    // Every leaf above is derived from the SIGNED cert, verified client-side
    // under the account's own identity key. This one is not: the nest simply
    // asserts it, with nothing signing it. So the wording hedges deliberately
    // ("Last reported use", "No use reported yet") and the leaf carries
    // `advisory=true` — an absent stamp means nothing was REPORTED, never that
    // nothing happened, because a nest that under-reports is exactly what this
    // value cannot detect. The audit surface that IS trustworthy is the feed's
    // `delegated-origin-badge`, read from signed bytes; the hint line below
    // points a user there rather than leaving them to trust this number.
    let last_used = delegation_leaf(
        "atproto-delegation-last-used",
        &match row.last_used_at_millis {
            Some(millis) => {
                BS::delegation_last_used(&fauna_core::format::format_unix_local(millis / 1000))
            }
            None => BS::DELEGATION_LAST_USED_NEVER.to_string(),
        },
    );
    set_test_attr(&last_used, "advisory", "true");
    leaves.append(&last_used);

    let hint = gtk::Label::builder()
        .label(BS::DELEGATION_LAST_USED_HINT)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    leaves.append(&hint);

    landmark.append(&leaves);
    w.delegation_group.add(&landmark);
    widgets.push(landmark.upcast());

    // Renewal is the same control, relabeled — never a revoke-then-re-mint.
    let actions = adw::ActionRow::builder().activatable(false).build();
    actions.add_suffix(&delegation_button(
        ctx,
        "atproto-delegation-authorize",
        BS::DELEGATION_REAUTHORIZE_BUTTON,
        true,
    ));
    actions.add_suffix(&delegation_button(
        ctx,
        "atproto-delegation-revoke",
        BS::DELEGATION_REVOKE_BUTTON,
        false,
    ));
    w.delegation_group.add(&actions);
    widgets.push(actions.upcast());
}

/// One delegation leaf: a wrapping, start-aligned label carrying `id`.
fn delegation_leaf(id: &str, text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build();
    set_test_id(&label, id);
    label
}

/// One delegation control. `authorize` picks the gesture: `true` provisions (or
/// re-provisions — the same call, which is why renewal needs no revoke first),
/// `false` revokes, destroying the signing sub-key.
fn delegation_button(
    ctx: &Rc<AtprotoPageCtx>,
    id: &str,
    label: &str,
    authorize: bool,
) -> gtk::Button {
    let btn = gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .css_classes(if authorize {
            ["suggested-action"]
        } else {
            ["destructive-action"]
        })
        .build();
    set_test_id(&btn, id);
    // The authorize leg is a three-call ceremony (fetch `K_pub`, sign, upload);
    // the upload is the kind that BINDS it — the fetch is a Read that gates
    // nothing and completing without the upload authorizes nobody.
    crate::offline_gate::declare_wire_kind(
        &btn,
        if authorize {
            "fauna.bridges.atproto.provision_authoring_delegation"
        } else {
            "fauna.bridges.atproto.revoke_authoring_delegation"
        },
    );
    let ctx = Rc::clone(ctx);
    btn.connect_clicked(move |_| {
        let machine = Arc::clone(&ctx.machine);
        let rt = ctx.rt.clone();
        let ctx = Rc::clone(&ctx);
        spawn_with_snapshot(
            &rt,
            move || async move {
                if authorize {
                    machine.authorize_external_apps().await;
                } else {
                    machine.deauthorize_external_apps().await;
                }
            },
            move |()| render(&ctx),
        );
    });
    btn
}

/// Embed (or re-embed) the shared Bridges detail content as the Linked-account
/// panel, reading the "bluesky" provider's `BridgeStatus` from the app-wide
/// bridges snapshot. Reusing the components verbatim is the spec
/// (ui/atproto.md § Layout & flow / § Element IDs — zero new IDs), and reading
/// the shared snapshot rather than issuing a second `fauna.bridges.list` is
/// what keeps this panel and the Bridges page from disagreeing: the snapshot is
/// re-fetched on every link/unlink, and `notify_bridges_changed` repaints us.
///
/// A `None` status — no reply yet, or a nest built without the `bluesky`
/// provider feature — renders the unlinked surface, which is the honest state:
/// nothing is linked.
fn sync_linked_panel(ctx: &Rc<AtprotoPageCtx>) {
    let w = &ctx.w;
    let status = crate::settings::get_bridge_status("bluesky");

    // Rebuild only on a real change — `render` runs on every machine tick.
    if w.linked_content.borrow().is_some() && *w.linked_built_from.borrow() == status {
        return;
    }

    let client = match crate::settings::get_client() {
        Some(c) => c,
        None => return,
    };

    if let Some(old) = w.linked_content.borrow_mut().take() {
        w.linked_group.remove(&old);
    }

    let (link_modes, linked, error, settings) = status
        .as_ref()
        .map(|b| {
            (
                b.link_modes.clone().unwrap_or_default(),
                b.linked,
                b.error.clone(),
                b.settings.clone(),
            )
        })
        .unwrap_or_default();
    let (content, handles) = crate::views::bridges::detail::build_bridge_detail_content(
        "bluesky",
        link_modes,
        linked,
        error,
        settings,
        Rc::clone(&client),
    );
    w.linked_group.add(&content);
    *w.linked_content.borrow_mut() = Some(content);
    *w.linked_built_from.borrow_mut() = status;
    *w.linked_follows_list.borrow_mut() = Some(handles.follows_list);

    // A rebuild IS "the detail just (re)appeared" — the same moment the
    // Bridges page's own row activation fetches follows. Only when
    // actually linked: an unlinked panel has no follows to show.
    if linked {
        client.fetch_bridge_follows("bluesky");
    }
}

/// One `atproto-app-credential-item` row. Reveal is gated on `revealable`;
/// shown even when not gated if this session already revealed it.
fn build_credential_row(ctx: &Rc<AtprotoPageCtx>, cred: &AppCredentialRow) -> adw::ActionRow {
    let created = fauna_core::format::format_unix_local(cred.created_at_millis / 1000);
    let subtitle = match cred.last_used_at_millis {
        Some(millis) => format!(
            "{} · {}",
            BS::credential_created_prefix(&created),
            BS::credential_last_used_prefix(&fauna_core::format::format_unix_local(millis / 1000))
        ),
        None => format!(
            "{} · {}",
            BS::credential_created_prefix(&created),
            BS::CREDENTIAL_NEVER_USED
        ),
    };

    let row = adw::ActionRow::builder()
        .title(&cred.label)
        .subtitle(&subtitle)
        .build();
    set_test_id(&row, ids::ATPROTO_APP_CREDENTIAL_ITEM);
    // The row's identity is the credential label; the subtitle is
    // created/last-used detail. Declare it — see `testid::set_test_text`.
    crate::testid::set_test_text(&row, &cred.label);

    let already_revealed = ctx.revealed.borrow().get(&cred.credential_id).cloned();
    if cred.revealable || already_revealed.is_some() {
        let reveal_btn = gtk::Button::builder()
            .label(BS::REVEAL_BUTTON)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        set_test_id(&reveal_btn, ids::ATPROTO_APP_CREDENTIAL_REVEAL);
        // A pure read of this client's OWN `fauna.state.atproto` store (`machine.rs::
        // reveal_secret` — the nest structurally cannot answer, D3). A `Read`
        // gates nothing; declared anyway so a later reclassification reaches
        // this control for free, the `mail-settings` reveal's precedent.
        if let Some(secret) = already_revealed {
            reveal_btn.set_label(&secret);
            reveal_btn.set_sensitive(false);
        } else {
            let ctx = Rc::clone(ctx);
            let credential_id = cred.credential_id.clone();
            reveal_btn.connect_clicked(move |_| {
                let machine = Arc::clone(&ctx.machine);
                let rt = ctx.rt.clone();
                let ctx = Rc::clone(&ctx);
                let credential_id = credential_id.clone();
                let credential_id_render = credential_id.clone();
                spawn_with_snapshot(
                    &rt,
                    move || async move { machine.reveal_secret(credential_id).await },
                    move |result| {
                        if let Ok(secret) = result {
                            ctx.revealed
                                .borrow_mut()
                                .insert(credential_id_render, secret.as_str().to_string());
                        }
                        render(&ctx);
                    },
                );
            });
        }
        row.add_suffix(&reveal_btn);
    }

    let revoke_btn = gtk::Button::builder()
        .label(BS::REVOKE_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&revoke_btn, ids::ATPROTO_APP_CREDENTIAL_REVOKE);
    // Nest first, then the local secret drop: the nest-side revoke is the leg
    // that cannot run without a link, and the plane write behind it is
    // OfflineSafe.
    crate::offline_gate::declare_wire_kind(
        &revoke_btn,
        "fauna.bridges.atproto.revoke_app_credential",
    );
    {
        let ctx = Rc::clone(ctx);
        let credential_id = cred.credential_id.clone();
        revoke_btn.connect_clicked(move |_| {
            let machine = Arc::clone(&ctx.machine);
            let rt = ctx.rt.clone();
            let ctx = Rc::clone(&ctx);
            let credential_id = credential_id.clone();
            spawn_with_snapshot(
                &rt,
                move || async move { machine.revoke(credential_id).await },
                move |()| render(&ctx),
            );
        });
    }
    row.add_suffix(&revoke_btn);

    row
}
