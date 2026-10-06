use adw::prelude::*;
use fauna_client::Value;
use fauna_client_bridges::bridges_ui::BridgeFollow;
use fauna_client_bridges::{
    NOSTR_LINK_MODE_GENERATE as MODE_GENERATE, NOSTR_LINK_MODE_IMPORT as MODE_IMPORT,
    NOSTR_LINK_MODE_REMOTE as MODE_REMOTE, NOSTR_LINK_MODES as LINK_MODES, bool_setting,
    nostr_content_toggle_options, relay_list_setting,
};
use fauna_client_config::npub_confirmation_owed_for;
#[cfg(feature = "zaps")]
use fauna_client_nostr::nostr::ZapSignerEntry;
use fauna_core::localized::LocalizedText;
use fauna_core::qr_matrix::QrMatrix;
use fauna_ui_ids as ids;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::client::FaunaClient;
use crate::i18n::strings::{common, nostr};
use crate::testid::{set_test_attr, set_test_id};

/// Resolve a shared-Rust [`LocalizedText`] through linux's own i18n runtime —
/// the `settings/atproto.rs::resolve` idiom, needed here since the content
/// toggles' titles/subtitles now arrive as keys from the shared catalog rather
/// than as `fauna_i18n` constants named at the call site.
fn resolve_text(text: &LocalizedText) -> String {
    text.clone().resolve(crate::i18n::strings::lookup)
}

/// Side of the drawn bunker-invite QR, in px — matches
/// `settings/identity_export.rs`'s `QR_SIZE_PX` (same visual weight for the
/// same "scan me" affordance).
const BUNKER_QR_SIZE_PX: i32 = 220;

// ---------------------------------------------------------------------------
// Thread-local widget references updated after async operations
// ---------------------------------------------------------------------------

thread_local! {
    static STATUS_LABEL: RefCell<Option<adw::ActionRow>> = const { RefCell::new(None) };
    static PUBKEY_ROW: RefCell<Option<adw::ActionRow>> = const { RefCell::new(None) };
    static MODE_ROW: RefCell<Option<adw::ActionRow>> = const { RefCell::new(None) };
    static LINK_BTN: RefCell<Option<gtk::Button>> = const { RefCell::new(None) };
}

// Native link modes (`MODE_GENERATE`/`MODE_IMPORT`/`MODE_REMOTE`/`LINK_MODES`)
// are `fauna_client_bridges::NOSTR_LINK_MODE_*`/`NOSTR_LINK_MODES`, imported
// above under their local names — this tab hand-copied them until this lift.

/// The caller's own clock, in epoch seconds — the unit the
/// `fauna.state.nostr-confirmation` stamp is stored in.
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

/// The rows/groups that swap between the unlinked account-link form and the
/// linked surface (nostr.md § Layout & flow: "unlinked → only the
/// account-link form; linked → pubkey-copy + unlink-button, the 5 content
/// toggles, relays, follows"). Mirrors apple `NostrSettingsView`'s `if
/// status.linked` split — GTK visibility standing in for SwiftUI's `if`.
/// Before this gate existed every row rendered unconditionally, so
/// `nostr-pubkey-copy-btn` (the e2e `is_linked()` signal) never actually
/// reflected link state.
#[derive(Clone)]
struct NostrLinkGate {
    pubkey_row: adw::ActionRow,
    mode_row: adw::ActionRow,
    unlink_row: adw::ActionRow,
    settings_group: adw::PreferencesGroup,
    relays_group: adw::PreferencesGroup,
    follows_group: adw::PreferencesGroup,
    link_mode_combo: adw::ComboRow,
    link_row: adw::ActionRow,
    nsec_row: adw::PasswordEntryRow,
    bunker_row: adw::EntryRow,
    /// Connected apps (NIP-46 bunker) — visible only for a linked account in
    /// a **custodial** mode (nostr.md § Layout & flow item 6: "Rendered only
    /// for a linked account in a custodial mode (generated/imported)"). A
    /// `remote` (external-bunker) account has no key on this box to sign
    /// with, so it can never itself be a bunker.
    connected_apps_group: adw::PreferencesGroup,
    /// Zap signers (the NIP-57 trust root — `monetization.md` § Zap receipts
    /// — the trust model). Unlike [`Self::connected_apps_group`], gated on
    /// **linked** alone, NOT custodial: the bunker role needs a key on this
    /// box to sign *with*, but designating who may speak for your money is
    /// orthogonal to where your key lives, and any linked account has a
    /// pubkey a receipt's `p` tag can name (`nostr.md` § Layout & flow item 7).
    #[cfg(feature = "zaps")]
    zap_signers_group: adw::PreferencesGroup,
}

impl NostrLinkGate {
    /// `import_mode`/`remote_mode` only matter while unlinked — the mode
    /// picker itself is hidden once linked, so its selection is moot.
    /// `custodial` is the post-link signal (the account's actual signing
    /// mode once linked, not the pre-link mode-picker selection) — see
    /// [`Self::connected_apps_group`].
    fn apply(&self, linked: bool, import_mode: bool, remote_mode: bool, custodial: bool) {
        self.pubkey_row.set_visible(linked);
        self.mode_row.set_visible(linked);
        self.unlink_row.set_visible(linked);
        self.settings_group.set_visible(linked);
        self.relays_group.set_visible(linked);
        self.follows_group.set_visible(linked);
        self.link_mode_combo.set_visible(!linked);
        self.link_row.set_visible(!linked);
        self.nsec_row.set_visible(!linked && import_mode);
        self.bunker_row.set_visible(!linked && remote_mode);
        self.connected_apps_group.set_visible(linked && custodial);
        #[cfg(feature = "zaps")]
        self.zap_signers_group.set_visible(linked);
    }
}

fn selected_link_mode(combo: &adw::ComboRow) -> &'static str {
    LINK_MODES
        .get(combo.selected() as usize)
        .copied()
        .unwrap_or(MODE_GENERATE)
}

/// Fire-and-forget `relay_list` persist (mirrors the content-toggle
/// `save_settings` posture in this file — errors are logged, not surfaced as
/// a blocking failure, since the local list already reflects the user's
/// intent and a transient blip re-converges next save). Thin wrapper over the
/// shared `super::commit_bridge_settings`.
fn save_relay_list(client: &Rc<FaunaClient>, relays: &[String]) {
    let json = serde_json::to_string(relays).unwrap_or_else(|_| "[]".to_string());
    let settings = Value::Map(BTreeMap::from_iter([(
        fauna_client_bridges::RELAY_LIST_KEY.to_string(),
        Value::String(json),
    )]));
    super::commit_bridge_settings(client, "nostr", settings);
}

/// Rebuild every `nostr-relay-item` row from `relays` (tear-down + re-append,
/// the `update_contacts_list`/`update_media_list` idiom). Each row's remove
/// button optimistically updates `relays` (the local mirror the Add/Remove
/// flows share) then persists.
fn rebuild_relay_rows(
    container: &gtk::Box,
    relays: &Rc<RefCell<Vec<String>>>,
    client: &Rc<FaunaClient>,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let list = relays.borrow().clone();
    for url in &list {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        set_test_id(&row, ids::NOSTR_RELAY_ITEM);
        // Plain Boxes default to Generic and drop out of the AT-SPI tree —
        // Group keeps this indexed row discoverable (same idiom as
        // `settings/linked_nests.rs`'s `nests-item`).
        row.set_accessible_role(gtk::AccessibleRole::Group);

        let label = gtk::Label::new(Some(url));
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        row.append(&label);

        let remove_btn = gtk::Button::from_icon_name("edit-delete-symbolic");
        remove_btn.add_css_class("flat");
        set_test_id(&remove_btn, ids::NOSTR_REMOVE_RELAY);
        crate::offline_gate::declare_wire_kind(&remove_btn, "fauna.bridges.set_settings");
        row.append(&remove_btn);

        {
            let relays = Rc::clone(relays);
            let client = Rc::clone(client);
            let container = container.clone();
            let url = url.clone();
            remove_btn.connect_clicked(move |_| {
                relays.borrow_mut().retain(|r| r != &url);
                rebuild_relay_rows(&container, &relays, &client);
                save_relay_list(&client, &relays.borrow());
            });
        }
        container.append(&row);
    }
}

/// Rebuild every `nostr-follow-item` row from a fresh `list_follows` reply.
/// Unlike relays (a settings blob this client owns locally), follows are a
/// server-side list with no local mirror — remove re-fetches rather than
/// optimistically splicing.
fn rebuild_follow_rows(container: &gtk::Box, follows: &[BridgeFollow], client: &Rc<FaunaClient>) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    for follow in follows {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        set_test_id(&row, ids::NOSTR_FOLLOW_ITEM);
        row.set_accessible_role(gtk::AccessibleRole::Group);

        let info = gtk::Box::new(gtk::Orientation::Vertical, 2);
        info.set_hexpand(true);
        let id_label = gtk::Label::new(Some(&follow.id));
        id_label.set_halign(gtk::Align::Start);
        id_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        info.append(&id_label);
        if let Some(petname) = follow.petname.as_ref().filter(|p| !p.is_empty()) {
            let petname_label = gtk::Label::new(Some(petname));
            petname_label.set_halign(gtk::Align::Start);
            petname_label.add_css_class("dim-label");
            petname_label.add_css_class("caption");
            info.append(&petname_label);
        }
        row.append(&info);

        let remove_btn = gtk::Button::from_icon_name("edit-delete-symbolic");
        remove_btn.add_css_class("flat");
        set_test_id(&remove_btn, ids::NOSTR_REMOVE_FOLLOW);
        crate::offline_gate::declare_wire_kind(&remove_btn, "fauna.bridges.remove_follow");
        row.append(&remove_btn);

        {
            let client = Rc::clone(client);
            let container = container.clone();
            let follow_id = follow.id.clone();
            remove_btn.connect_clicked(move |_| {
                let client = Rc::clone(&client);
                let container = container.clone();
                let fid = follow_id.clone();
                glib::MainContext::default().spawn_local(async move {
                    let bridges =
                        fauna_client_bridges::BridgesClient::new(client.nest_rpc().clone());
                    let handle = client.runtime_handle();
                    let result = handle
                        .spawn(async move { bridges.remove_follow("nostr", fid).await })
                        .await;
                    match result {
                        Ok(Err(e)) => {
                            tracing::error!("[settings/nostr] remove_follow error: {e}")
                        }
                        Err(e) => {
                            tracing::error!("[settings/nostr] remove_follow join error: {e}")
                        }
                        Ok(Ok(())) => {}
                    }
                    refresh_follows(&client, &container);
                });
            });
        }
        container.append(&row);
    }
}

/// Re-fetch this actor's Nostr follows and rebuild the list — called after
/// add/remove and once at initial load (when linked).
fn refresh_follows(client: &Rc<FaunaClient>, container: &gtk::Box) {
    let bridges = fauna_client_bridges::BridgesClient::new(client.nest_rpc().clone());
    let handle = client.runtime_handle();
    let client = Rc::clone(client);
    let container = container.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = handle
            .spawn(async move { bridges.list_follows("nostr").await })
            .await;
        match result {
            Ok(Ok(reply)) => rebuild_follow_rows(&container, &reply.follows, &client),
            Ok(Err(e)) => tracing::error!("[settings/nostr] list_follows error: {e}"),
            Err(e) => tracing::error!("[settings/nostr] list_follows join error: {e}"),
        }
    });
}

// ---------------------------------------------------------------------------
// Zap signers (the NIP-57 trust root — monetization.md § Zap receipts — the
// trust model; nostr.md § Layout & flow item 7). Mirrors the follows section
// above for the roster shape (a server-side list, no local mirror) and tui's
// `apps/fauna-tui/src/nostr.rs` (the reference leg) for the three behaviors
// that are load-bearing rather than cosmetic.
// ---------------------------------------------------------------------------

/// A roster row's text: the user's label (or a placeholder) and the signer's
/// short pubkey. Owned by `fauna_client_nostr` — see its doc comment for the
/// STORED-vs-typed-pubkey rationale; `refresh_zap_signers` re-lists after
/// `add` instead of pushing the input locally for that reason.
#[cfg(feature = "zaps")]
use fauna_client_nostr::zap_signer_row_text;

/// Rebuild every `nostr-zap-signer-item` row from a fresh `list` reply, and
/// toggle `nostr-zap-signer-empty` — mirrors `rebuild_follow_rows`: a
/// server-side roster with no local mirror, so remove re-fetches rather than
/// optimistically splicing.
///
/// **An empty roster is a STATED state, not a blank list** — designating
/// nobody means believing nobody, the ratified out-of-the-box default; "you
/// have not designated any signer" reads honestly where a blank list would
/// read as reassurance.
#[cfg(feature = "zaps")]
fn rebuild_zap_signer_rows(
    container: &gtk::Box,
    empty_label: &gtk::Label,
    signers: &[ZapSignerEntry],
    client: &Rc<FaunaClient>,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    empty_label.set_visible(signers.is_empty());
    for signer in signers {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        set_test_id(&row, ids::NOSTR_ZAP_SIGNER_ITEM);
        row.set_accessible_role(gtk::AccessibleRole::Group);

        let label = gtk::Label::new(Some(&zap_signer_row_text(signer)));
        label.set_halign(gtk::Align::Start);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        row.append(&label);

        let remove_btn = gtk::Button::from_icon_name("edit-delete-symbolic");
        remove_btn.add_css_class("flat");
        set_test_id(&remove_btn, ids::NOSTR_ZAP_SIGNER_REMOVE);
        // Always live: removal is de-escalation, which is never gated
        // (`dynamic-features.md`, ruling (i)) — a tier that can only tighten
        // must not be able to trap a user in a roster they cannot undo.
        crate::offline_gate::declare_wire_kind(&remove_btn, "fauna.nostr.zap_signers.remove");
        row.append(&remove_btn);

        {
            let client = Rc::clone(client);
            let container = container.clone();
            let empty_label = empty_label.clone();
            // Keyed by the row's own STORED pubkey, like the bunker roster is
            // keyed by its connection id: a roster that shifted under us must
            // still remove the signer the user actually saw.
            let pubkey = signer.signer_pubkey.clone();
            remove_btn.connect_clicked(move |_| {
                let client = Rc::clone(&client);
                let container = container.clone();
                let empty_label = empty_label.clone();
                let pubkey = pubkey.clone();
                glib::MainContext::default().spawn_local(async move {
                    let zap_signers =
                        fauna_client_nostr::NostrZapSignerClient::new(client.nest_rpc().clone());
                    let handle = client.runtime_handle();
                    let result = handle
                        .spawn(async move { zap_signers.remove(pubkey).await })
                        .await;
                    match result {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::error!("[settings/nostr] zap_signers remove error: {e}")
                        }
                        Err(e) => {
                            tracing::error!("[settings/nostr] zap_signers remove join error: {e}")
                        }
                    }
                    refresh_zap_signers(&client, &container, &empty_label);
                });
            });
        }
        container.append(&row);
    }
}

/// Re-fetch this actor's designated zap signers and rebuild the list —
/// called after add/remove and once at initial load (when linked). Mirrors
/// `refresh_follows`.
#[cfg(feature = "zaps")]
fn refresh_zap_signers(client: &Rc<FaunaClient>, container: &gtk::Box, empty_label: &gtk::Label) {
    let zap_signers = fauna_client_nostr::NostrZapSignerClient::new(client.nest_rpc().clone());
    let handle = client.runtime_handle();
    let client = Rc::clone(client);
    let container = container.clone();
    let empty_label = empty_label.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = handle.spawn(async move { zap_signers.list().await }).await;
        match result {
            Ok(Ok(signers)) => rebuild_zap_signer_rows(&container, &empty_label, &signers, &client),
            Ok(Err(e)) => tracing::error!("[settings/nostr] zap_signers list error: {e}"),
            Err(e) => tracing::error!("[settings/nostr] zap_signers list join error: {e}"),
        }
    });
}

/// Why `nostr-zap-signer-add-btn` is dead, or `None` when it works. Mirrors
/// tui's `designate_gate` (`apps/fauna-tui/src/nostr.rs`, the reference leg)
/// exactly — both now call the shared `fauna_client_features::gate_reason`,
/// which owns the three load-bearing shapes: the decision is READ off the
/// shared `FeaturesClient::rows()` affordance, never re-composed (it already
/// accounts for the `payments` subset edge); a `hidden` affordance still
/// disables rather than vanishing (the *excision* story is the orthogonal
/// compile-time `zaps` feature, not this render-time courtesy); and an
/// un-hydrated read leaves the button LIVE — the nest, not the app, is the
/// enforcement floor.
#[cfg(feature = "zaps")]
fn zap_signer_add_gate_reason(rows: &[fauna_client_features::FeatureRow]) -> Option<String> {
    fauna_client_features::gate_reason(rows, "zaps", crate::i18n::strings::lookup)
}

/// Re-fetch the gated-feature plane and apply the `zaps` verdict to the add
/// button + its why-line — called once at initial load (when linked). Never
/// disables eagerly before the fetch resolves, which is what makes an
/// un-hydrated read leave the button live (see
/// [`zap_signer_add_gate_reason`]).
#[cfg(feature = "zaps")]
fn refresh_zap_signer_gate(
    client: &Rc<FaunaClient>,
    add_btn: &gtk::Button,
    reason_label: &gtk::Label,
) {
    let features = fauna_client_features::FeaturesClient::new(client.nest_rpc().clone());
    let handle = client.runtime_handle();
    let add_btn = add_btn.clone();
    let reason_label = reason_label.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = handle.spawn(async move { features.rows().await }).await;
        match result {
            Ok(Ok(rows)) => {
                let reason = zap_signer_add_gate_reason(&rows);
                add_btn.set_sensitive(reason.is_none());
                match &reason {
                    Some(text) => {
                        reason_label.set_text(text);
                        reason_label.set_visible(true);
                    }
                    None => reason_label.set_visible(false),
                }
            }
            Ok(Err(e)) => tracing::error!("[settings/nostr] features fetch error: {e}"),
            Err(e) => tracing::error!("[settings/nostr] features fetch join error: {e}"),
        }
    });
}

/// Build the "Nostr" preferences page with account linking and status.
pub fn build_nostr_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(nostr::TITLE)
        .icon_name("network-transmit-symbolic")
        .build();

    // -----------------------------------------------------------------------
    // Group 1: Account Status
    // -----------------------------------------------------------------------
    let account_group = adw::PreferencesGroup::builder()
        .title(nostr::account::TITLE)
        .description(nostr::account::DESCRIPTION)
        .build();
    account_group.set_header_suffix(Some(&super::marker("page-heading")));

    // error-message — page-level error label (ui rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    account_group.add(&error_row);

    let status_row = adw::ActionRow::builder()
        .title(common::STATUS)
        .subtitle(common::CHECKING)
        .build();
    account_group.add(&status_row);

    let pubkey_row = adw::ActionRow::builder()
        .title(nostr::account::PUBLIC_KEY)
        .subtitle("—")
        .subtitle_selectable(true)
        .build();
    account_group.add(&pubkey_row);
    {
        let row_ref = pubkey_row.clone();
        let nostr_copy = gtk::Button::with_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
        nostr_copy.add_css_class("flat");
        crate::testid::set_test_id(&nostr_copy, ids::NOSTR_PUBKEY_COPY_BTN);
        nostr_copy.connect_clicked(move |btn| {
            let subtitle = row_ref
                .subtitle()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if subtitle != "—" {
                crate::clipboard::copy_text(&subtitle);
                btn.set_label(crate::i18n::strings::settings::account_page::COPIED_CLIPBOARD);
                let btn_weak = btn.downgrade();
                glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                    if let Some(btn) = btn_weak.upgrade() {
                        btn.set_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
                    }
                });
            }
        });
        pubkey_row.add_suffix(&nostr_copy);
    }

    let mode_row = adw::ActionRow::builder()
        .title(nostr::account::SIGNING_MODE)
        .subtitle("—")
        .build();
    account_group.add(&mode_row);

    page.add(&account_group);

    // -----------------------------------------------------------------------
    // Succession-aftermath npub confirm banner (leg 3 — `nostr.md` § Key
    // succession and rotation; tui reference `apps/fauna-tui/src/nostr.rs`).
    // Dismissible, never a blocking modal — the successor may reach this page
    // long after the ceremony (nostr.md's own Gotcha), so visibility is gated
    // purely on the predicate, checked once on nav-enter, never a one-shot
    // local flag. Same group/banner-row shape as `settings/mail.rs`'s pending-
    // rotation banner.
    // -----------------------------------------------------------------------
    let npub_confirm_group = adw::PreferencesGroup::new();
    npub_confirm_group.set_visible(false);
    let npub_confirm_label = gtk::Label::builder().wrap(true).xalign(0.0).build();
    set_test_id(&npub_confirm_label, ids::NOSTR_NPUB_CONFIRM_BANNER);
    let npub_confirm_row = adw::ActionRow::builder().build();
    npub_confirm_row.add_prefix(&npub_confirm_label);
    let npub_confirm_yes_btn = gtk::Button::builder()
        .label(nostr::npub_confirm::YES_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&npub_confirm_yes_btn, ids::NOSTR_NPUB_CONFIRM_YES_BUTTON);
    crate::offline_gate::declare_wire_kind(&npub_confirm_yes_btn, "fauna.account.state.put");
    npub_confirm_row.add_suffix(&npub_confirm_yes_btn);
    let npub_confirm_no_btn = gtk::Button::builder()
        .label(nostr::npub_confirm::NO_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&npub_confirm_no_btn, ids::NOSTR_NPUB_CONFIRM_NO_BUTTON);
    // "No / nothing is linked" IS an unlink — it delegates straight to
    // `unlink_btn`'s own handler below (nostr.md:75, tui's
    // `Action::DismissNpubToNewKey`), so it binds the same kind.
    crate::offline_gate::declare_wire_kind(&npub_confirm_no_btn, "fauna.bridges.unlink");
    npub_confirm_row.add_suffix(&npub_confirm_no_btn);
    npub_confirm_group.add(&npub_confirm_row);
    page.add(&npub_confirm_group);

    // -----------------------------------------------------------------------
    // Group 2: Actions — link mode + nsec/bunker fields + link/unlink
    // -----------------------------------------------------------------------
    let actions_group = adw::PreferencesGroup::builder()
        .title(common::ACTIONS)
        .build();

    // nostr-link-mode: generate / import / remote (NIP-46 bunker). Labels are
    // the model strings (the `driver.select`/`get_text` contract — same
    // pattern as `views/family.rs`'s selects).
    let mode_labels: Vec<String> = LINK_MODES
        .iter()
        .map(|m| crate::i18n::nostr_link_mode_label(m))
        .collect();
    let mode_label_refs: Vec<&str> = mode_labels.iter().map(String::as_str).collect();
    let link_mode_combo = adw::ComboRow::builder()
        .title(nostr::link_account::MODE_LABEL)
        .model(&gtk::StringList::new(&mode_label_refs))
        .build();
    set_test_id(&link_mode_combo, ids::NOSTR_LINK_MODE);
    actions_group.add(&link_mode_combo);

    let nsec_row = adw::PasswordEntryRow::builder()
        .title(nostr::link_account::NSEC_LABEL)
        .visible(false)
        .build();
    set_test_id(&nsec_row, ids::NOSTR_NSEC_INPUT);
    actions_group.add(&nsec_row);

    // NIP-46 bunker URL — no ui.yaml id (native-only field, mirrors apple
    // `NostrSettingsView`/android `NostrScreen`).
    let bunker_row = adw::EntryRow::builder()
        .title("bunker://pubkey?relay=wss://...")
        .visible(false)
        .build();
    actions_group.add(&bunker_row);

    // Wired below (after `link_gate`/`linked_state` exist) — see "Wire: link
    // mode select" further down.

    // Generate / Link button
    let link_btn = gtk::Button::builder()
        .label(nostr::account::GENERATE_KEY_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&link_btn, ids::NOSTR_LINK_BUTTON);
    crate::offline_gate::declare_wire_kind(&link_btn, "fauna.bridges.link");

    let link_row = adw::ActionRow::builder()
        .title(nostr::link_account::LINK_BUTTON)
        .subtitle(nostr::account::LINK_SUBTITLE)
        .activatable(true)
        .build();
    link_row.add_suffix(&link_btn);
    actions_group.add(&link_row);

    // Unlink button
    let unlink_btn = gtk::Button::builder()
        .label(nostr::account::UNLINK_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&unlink_btn, ids::NOSTR_UNLINK_BUTTON);
    crate::offline_gate::declare_wire_kind(&unlink_btn, "fauna.bridges.unlink");

    let unlink_row = adw::ActionRow::builder()
        .title(nostr::account::UNLINK)
        .subtitle(nostr::account::UNLINK_SUBTITLE)
        .activatable(true)
        .build();
    unlink_row.add_suffix(&unlink_btn);
    actions_group.add(&unlink_row);

    // `nostr-npub-confirm-no-button` — "no / nothing is linked" reuses the
    // EXISTING unlink gesture rather than a bespoke flow (nostr.md:75: "the
    // remedy is the existing page machinery"). Unlinking is that machinery's
    // own entry point: it drops the wrong (or thief-relinked) key and reveals
    // the link form, where the owner generates or imports a fresh one exactly
    // as any first-time link would.
    {
        let unlink_btn = unlink_btn.clone();
        npub_confirm_no_btn.connect_clicked(move |_| {
            unlink_btn.emit_clicked();
        });
    }

    // `nostr-npub-confirm-yes-button` — "yes, that's my npub": writes
    // the confirmation stamp at `now()` to the account plane
    // (`fauna.state.nostr-confirmation`), then re-checks (non-optimistic, like every other mutation on this page
    // — the banner's disappearance is a fresh read, not an assumed outcome).
    {
        let npub_confirm_group_ref = npub_confirm_group.clone();
        let btn_ref = npub_confirm_yes_btn.clone();
        npub_confirm_yes_btn.connect_clicked(move |_| {
            let Some(client) = super::get_client() else {
                tracing::error!("[settings/nostr] npub confirm: no client available");
                return;
            };
            let nest = client.nest_rpc().clone();
            let handle = client.runtime_handle();
            let group = npub_confirm_group_ref.clone();
            let br = btn_ref.clone();

            br.set_sensitive(false);
            glib::MainContext::default().spawn_local(async move {
                let Some(account) = crate::account_runtime::handle() else {
                    tracing::error!("[settings/nostr] npub confirm: no account runtime yet");
                    br.set_sensitive(true);
                    return;
                };
                let confirm_result = handle
                    .spawn({
                        let account = account.clone();
                        async move { account.confirm_nostr_npub(now_secs()).await }
                    })
                    .await;
                match confirm_result {
                    Ok(Ok(_)) => {
                        let owed = handle
                            .spawn(async move {
                                npub_confirmation_owed_for(
                                    nest.as_ref(),
                                    Some(account.npub_confirmed_at()),
                                )
                                .await
                            })
                            .await
                            .unwrap_or(false);
                        group.set_visible(owed);
                    }
                    Ok(Err(e)) => {
                        tracing::error!("[settings/nostr] npub confirm error: {e}");
                    }
                    Err(e) => {
                        tracing::error!("[settings/nostr] npub confirm join error: {e}");
                    }
                }
                br.set_sensitive(true);
            });
        });
    }

    page.add(&actions_group);

    // -----------------------------------------------------------------------
    // Group 3: Settings (shown when linked) — the 5 content-publishing flags
    // -----------------------------------------------------------------------
    let settings_group = adw::PreferencesGroup::builder()
        .title(common::SETTINGS)
        .description(nostr::settings::DESCRIPTION)
        .build();

    // Element id, wire key, default, title AND subtitle all come from the
    // shared catalog (`nostr.md` § Where logic lives) — linux previously spelled
    // the five rows out three times over (here, the save path, the hydrate
    // path), each spelling free to drift from the others.
    //
    // Kept as `(wire key, row)` pairs in catalog order: every later site
    // iterates this instead of naming a switch, so a sixth flag added to the
    // catalog appears in all three places at once.
    let content_switches: Vec<(String, adw::SwitchRow)> = nostr_content_toggle_options()
        .into_iter()
        .map(|opt| {
            let mut builder = adw::SwitchRow::builder()
                .title(resolve_text(&opt.label))
                .active(opt.default_on);
            if let Some(subtitle) = &opt.subtitle {
                builder = builder.subtitle(resolve_text(subtitle));
            }
            let row = builder.build();
            set_test_id(&row, &opt.ui_id);
            crate::offline_gate::declare_wire_kind(&row, "fauna.bridges.set_settings");
            // The e2e's `driver.get_attr(id, "state")` contract: an unmarked
            // `SwitchRow` falls back to "true"/"false", not "on"/"off".
            set_test_attr(&row, "state", if opt.default_on { "on" } else { "off" });
            settings_group.add(&row);
            (opt.key, row)
        })
        .collect();

    page.add(&settings_group);

    // -----------------------------------------------------------------------
    // Group 4: Relays — read-modify-write the `relay_list` JSON setting
    // -----------------------------------------------------------------------
    let relays_group = adw::PreferencesGroup::builder()
        .title(nostr::relays::TITLE)
        .build();

    let relay_list_container = gtk::Box::new(gtk::Orientation::Vertical, 4);
    relays_group.add(&relay_list_container);

    let relay_input_row = adw::EntryRow::builder()
        .title(nostr::relays::PLACEHOLDER)
        .build();
    set_test_id(&relay_input_row, ids::NOSTR_RELAY_INPUT);
    let add_relay_btn = gtk::Button::from_icon_name("list-add-symbolic");
    add_relay_btn.add_css_class("flat");
    add_relay_btn.set_valign(gtk::Align::Center);
    set_test_id(&add_relay_btn, ids::NOSTR_ADD_RELAY);
    crate::offline_gate::declare_wire_kind(&add_relay_btn, "fauna.bridges.set_settings");
    relay_input_row.add_suffix(&add_relay_btn);
    relays_group.add(&relay_input_row);

    page.add(&relays_group);

    // -----------------------------------------------------------------------
    // Group 5: Follows
    // -----------------------------------------------------------------------
    let follows_group = adw::PreferencesGroup::builder()
        .title(nostr::follows::TITLE)
        .build();

    let follow_list_container = gtk::Box::new(gtk::Orientation::Vertical, 4);
    follows_group.add(&follow_list_container);

    let follow_pubkey_row = adw::EntryRow::builder()
        .title(nostr::follows::PUBKEY_PLACEHOLDER)
        .build();
    set_test_id(&follow_pubkey_row, ids::NOSTR_FOLLOW_PUBKEY_INPUT);
    let follow_petname_row = adw::EntryRow::builder()
        .title(nostr::follows::PETNAME_PLACEHOLDER)
        .build();
    set_test_id(&follow_petname_row, ids::NOSTR_FOLLOW_PETNAME_INPUT);
    let add_follow_btn = gtk::Button::from_icon_name("list-add-symbolic");
    add_follow_btn.add_css_class("flat");
    add_follow_btn.set_valign(gtk::Align::Center);
    set_test_id(&add_follow_btn, ids::NOSTR_ADD_FOLLOW);
    crate::offline_gate::declare_wire_kind(&add_follow_btn, "fauna.bridges.add_follow");
    follow_pubkey_row.add_suffix(&add_follow_btn);
    follows_group.add(&follow_pubkey_row);
    follows_group.add(&follow_petname_row);

    page.add(&follows_group);

    // -----------------------------------------------------------------------
    // Group 6: Connected apps (Nostr Connect — NIP-46 bunker; nostr.md § The
    // nest as the user's NIP-46 signer). Visible only for a linked custodial
    // account (`NostrLinkGate.connected_apps_group` — a `remote` account has
    // no local key to sign with, so it can never itself be a bunker).
    // -----------------------------------------------------------------------
    let connected_apps_group = adw::PreferencesGroup::builder()
        .title(nostr::connected_apps::TITLE)
        .description(nostr::connected_apps::DESCRIPTION)
        .build();

    let bunker_connect_btn = gtk::Button::builder()
        .label(nostr::connected_apps::CONNECT_BUTTON)
        .halign(gtk::Align::Start)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&bunker_connect_btn, ids::NOSTR_BUNKER_CONNECT_BTN);
    crate::offline_gate::declare_wire_kind(&bunker_connect_btn, "fauna.nostr.bunker.create_invite");
    connected_apps_group.add(&bunker_connect_btn);

    // Reveal area — the one-time connect string + QR (mail-credentials
    // one-time-reveal precedent, § Connection model). Hidden until a fresh
    // invite is minted.
    let bunker_reveal_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    bunker_reveal_box.set_visible(false);
    bunker_reveal_box.set_margin_top(8);

    let bunker_reveal_title = gtk::Label::builder()
        .label(nostr::connected_apps::REVEAL_TITLE)
        .halign(gtk::Align::Start)
        .build();
    bunker_reveal_title.add_css_class("heading");
    bunker_reveal_box.append(&bunker_reveal_title);

    // The matrix the draw func paints — `crate::qr_widget` is the one
    // Linux-specific rendering routine, shared with `identity_export.rs`.
    let bunker_qr_matrix: Rc<RefCell<Option<QrMatrix>>> = Rc::new(RefCell::new(None));
    let bunker_qr_area = gtk::DrawingArea::builder()
        .content_width(BUNKER_QR_SIZE_PX)
        .content_height(BUNKER_QR_SIZE_PX)
        .visible(false)
        .build();
    bunker_qr_area.set_halign(gtk::Align::Start);
    bunker_qr_area.set_tooltip_text(Some(nostr::connected_apps::QR_ALT));
    set_test_id(&bunker_qr_area, ids::NOSTR_BUNKER_CONNECT_QR);
    bunker_qr_area.set_draw_func({
        let matrix = bunker_qr_matrix.clone();
        move |_area, cr, width, height| {
            let borrowed = matrix.borrow();
            let Some(m) = borrowed.as_ref() else { return };
            crate::qr_widget::draw_matrix(cr, m, width, height);
        }
    });
    bunker_reveal_box.append(&bunker_qr_area);

    let bunker_connect_string_label = gtk::Label::builder()
        .selectable(true)
        .wrap(true)
        .halign(gtk::Align::Start)
        .build();
    bunker_connect_string_label.add_css_class("monospace");
    set_test_id(
        &bunker_connect_string_label,
        ids::NOSTR_BUNKER_CONNECT_STRING,
    );
    bunker_reveal_box.append(&bunker_connect_string_label);

    let bunker_copy_btn = gtk::Button::builder()
        .label(common::COPY)
        .halign(gtk::Align::Start)
        .build();
    set_test_id(&bunker_copy_btn, ids::NOSTR_BUNKER_CONNECT_COPY_BTN);
    bunker_reveal_box.append(&bunker_copy_btn);

    let bunker_reveal_hint = gtk::Label::builder()
        .label(nostr::connected_apps::REVEAL_HINT)
        .halign(gtk::Align::Start)
        .build();
    bunker_reveal_hint.add_css_class("dim-label");
    bunker_reveal_hint.add_css_class("caption");
    bunker_reveal_box.append(&bunker_reveal_hint);

    connected_apps_group.add(&bunker_reveal_box);

    page.add(&connected_apps_group);

    // -----------------------------------------------------------------------
    // Group 7: Zap signers (the NIP-57 trust root — monetization.md § Zap
    // receipts — the trust model; nostr.md § Layout & flow item 7). Visible
    // for any LINKED account — not custodial-gated like Connected apps above
    // (`NostrLinkGate.zap_signers_group`).
    // -----------------------------------------------------------------------
    #[cfg(feature = "zaps")]
    let (
        zap_signers_group,
        zap_signers_container,
        zap_signers_empty_label,
        zap_signer_pubkey_row,
        zap_signer_label_row,
        zap_signer_add_btn,
        zap_signer_gate_reason_label,
    ) = {
        let zap_signers_group = adw::PreferencesGroup::builder()
            .title(nostr::zap_signers::TITLE)
            .description(nostr::zap_signers::DESCRIPTION)
            .build();

        let zap_signers_container = gtk::Box::new(gtk::Orientation::Vertical, 4);
        zap_signers_group.add(&zap_signers_container);

        let zap_signers_empty_label = gtk::Label::new(Some(nostr::zap_signers::NONE));
        zap_signers_empty_label.set_halign(gtk::Align::Start);
        zap_signers_empty_label.set_wrap(true);
        zap_signers_empty_label.add_css_class("dim-label");
        zap_signers_empty_label.add_css_class("caption");
        set_test_id(&zap_signers_empty_label, ids::NOSTR_ZAP_SIGNER_EMPTY);
        zap_signers_group.add(&zap_signers_empty_label);

        let zap_signer_pubkey_row = adw::EntryRow::builder()
            .title(nostr::zap_signers::PUBKEY_PLACEHOLDER)
            .build();
        set_test_id(&zap_signer_pubkey_row, ids::NOSTR_ZAP_SIGNER_PUBKEY_INPUT);
        let zap_signer_label_row = adw::EntryRow::builder()
            .title(nostr::zap_signers::LABEL_PLACEHOLDER)
            .build();
        set_test_id(&zap_signer_label_row, ids::NOSTR_ZAP_SIGNER_LABEL_INPUT);
        let zap_signer_add_btn = gtk::Button::from_icon_name("list-add-symbolic");
        zap_signer_add_btn.add_css_class("flat");
        zap_signer_add_btn.set_valign(gtk::Align::Center);
        set_test_id(&zap_signer_add_btn, ids::NOSTR_ZAP_SIGNER_ADD_BTN);
        crate::offline_gate::declare_wire_kind(&zap_signer_add_btn, "fauna.nostr.zap_signers.add");
        zap_signer_pubkey_row.add_suffix(&zap_signer_add_btn);
        zap_signers_group.add(&zap_signer_pubkey_row);
        zap_signers_group.add(&zap_signer_label_row);

        // The Dim-3 courtesy why-line (`zap_signer_add_gate_reason`) — hidden
        // until a restricted verdict actually resolves.
        let zap_signer_gate_reason_label = gtk::Label::builder().visible(false).build();
        zap_signer_gate_reason_label.set_halign(gtk::Align::Start);
        zap_signer_gate_reason_label.set_wrap(true);
        zap_signer_gate_reason_label.add_css_class("dim-label");
        zap_signer_gate_reason_label.add_css_class("caption");
        zap_signers_group.add(&zap_signer_gate_reason_label);

        page.add(&zap_signers_group);

        (
            zap_signers_group,
            zap_signers_container,
            zap_signers_empty_label,
            zap_signer_pubkey_row,
            zap_signer_label_row,
            zap_signer_add_btn,
            zap_signer_gate_reason_label,
        )
    };

    // Stash widget refs for updates
    STATUS_LABEL.with(|c| *c.borrow_mut() = Some(status_row.clone()));
    PUBKEY_ROW.with(|c| *c.borrow_mut() = Some(pubkey_row.clone()));
    MODE_ROW.with(|c| *c.borrow_mut() = Some(mode_row.clone()));
    LINK_BTN.with(|c| *c.borrow_mut() = Some(link_btn.clone()));

    // Local mirror of the `relay_list` setting — the Add/Remove flows share
    // this so a rebuild never needs a round trip just to redraw.
    let relays_state: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    // Linked/unlinked visibility gate (see `NostrLinkGate`) + the state it
    // reads. Starts unlinked — correct for a fresh box, and for the
    // `!available` case (nostr.md: the link form must render regardless of
    // `available` so it can bootstrap the first deposit); the initial status
    // fetch below flips it to `true` if the account is already linked.
    let link_gate = NostrLinkGate {
        pubkey_row: pubkey_row.clone(),
        mode_row: mode_row.clone(),
        unlink_row: unlink_row.clone(),
        settings_group: settings_group.clone(),
        relays_group: relays_group.clone(),
        follows_group: follows_group.clone(),
        link_mode_combo: link_mode_combo.clone(),
        link_row: link_row.clone(),
        nsec_row: nsec_row.clone(),
        bunker_row: bunker_row.clone(),
        connected_apps_group: connected_apps_group.clone(),
        #[cfg(feature = "zaps")]
        zap_signers_group: zap_signers_group.clone(),
    };
    let linked_state: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    // Whether the linked account is custodial (`generated`/`imported`, holds
    // its key on this box) — distinct from `linked_state`: gates
    // `connected_apps_group` independently of the pre-link mode-picker
    // selection (see `NostrLinkGate::connected_apps_group`).
    let custodial_state: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    link_gate.apply(false, false, false, false);

    // -----------------------------------------------------------------------
    // Wire: link mode select (generate / import / remote) — nsec/bunker
    // fields show only while unlinked, and only for their own mode.
    // -----------------------------------------------------------------------
    {
        let link_gate = link_gate.clone();
        let linked_state = Rc::clone(&linked_state);
        let custodial_state = Rc::clone(&custodial_state);
        link_mode_combo.connect_selected_notify(move |combo| {
            let mode = selected_link_mode(combo);
            link_gate.apply(
                linked_state.get(),
                mode == MODE_IMPORT,
                mode == MODE_REMOTE,
                custodial_state.get(),
            );
        });
    }

    // -----------------------------------------------------------------------
    // Wire: Generate/Import/Remote link button
    // -----------------------------------------------------------------------
    {
        let status_row_ref = status_row.clone();
        let pubkey_row_ref = pubkey_row.clone();
        let mode_row_ref = mode_row.clone();
        let btn_ref = link_btn.clone();
        let link_mode_combo_ref = link_mode_combo.clone();
        let nsec_row_ref = nsec_row.clone();
        let bunker_row_ref = bunker_row.clone();
        let error_label_ref = error_label.clone();
        let link_gate = link_gate.clone();
        let linked_state = Rc::clone(&linked_state);
        let custodial_state = Rc::clone(&custodial_state);
        let npub_confirm_group_ref = npub_confirm_group.clone();
        link_btn.connect_clicked(move |_| {
            let Some(client) = super::get_client() else {
                tracing::error!("[settings/nostr] link: no client available");
                return;
            };
            let mode = LINK_MODES
                .get(link_mode_combo_ref.selected() as usize)
                .copied()
                .unwrap_or(MODE_GENERATE);

            let mut fields = BTreeMap::new();
            if mode == MODE_IMPORT {
                let nsec = nsec_row_ref.text().trim().to_string();
                if nsec.is_empty() {
                    super::render_error_label(
                        &error_label_ref,
                        Some(nostr::link_account::ENTER_NSEC),
                    );
                    return;
                }
                fields.insert("nsec".to_string(), Value::String(nsec));
            } else if mode == MODE_REMOTE {
                let bunker = bunker_row_ref.text().trim().to_string();
                fields.insert("bunker_url".to_string(), Value::String(bunker));
            }
            error_label_ref.set_visible(false);

            let bridges = fauna_client_bridges::BridgesClient::new(client.nest_rpc().clone());
            let handle = client.runtime_handle();

            let sr = status_row_ref.clone();
            let pr = pubkey_row_ref.clone();
            let mr = mode_row_ref.clone();
            let br = btn_ref.clone();
            let nsec_clear = nsec_row_ref.clone();
            let link_gate = link_gate.clone();
            let linked_state = Rc::clone(&linked_state);
            let custodial_state = Rc::clone(&custodial_state);
            let npub_confirm_group_ref = npub_confirm_group_ref.clone();

            sr.set_subtitle(common::LINKING);
            br.set_sensitive(false);

            glib::MainContext::default().spawn_local(async move {
                // `fauna.bridges.link` (bridge_id "nostr") replaces the deleted
                // `POST /api/v1/nostr/link` control-plane route (nostr.md §
                // WS-RPC migration contract).
                let result = handle
                    .spawn(async move { bridges.link("nostr", mode, Value::Map(fields)).await })
                    .await;

                match result {
                    Ok(Ok(reply)) => {
                        // npub rides `identity.value` on the generic bridge wire;
                        // `LinkReply` carries no `mode`, so show the requested one.
                        let npub = reply
                            .identity
                            .as_ref()
                            .map(|i| i.value.as_str())
                            .unwrap_or("(unknown)");
                        sr.set_subtitle(common::LINKED);
                        pr.set_subtitle(npub);
                        mr.set_subtitle(&crate::i18n::nostr_signing_mode_label(match mode {
                            MODE_IMPORT => "imported",
                            MODE_REMOTE => "remote",
                            _ => "generated",
                        }));
                        nsec_clear.set_text("");
                        linked_state.set(true);
                        custodial_state.set(mode != MODE_REMOTE);
                        link_gate.apply(true, false, false, custodial_state.get());

                        // A fresh (re-)link is itself the new-npub remedy
                        // (nostr.md:75): the owner just chose this key, so
                        // best-effort record the confirmation — never blocking
                        // the link on it (mirrors tui's `Op::Link`). Harmless
                        // on an account with no succession history: the
                        // predicate short-circuits before ever reading it.
                        if let Some(account) = crate::account_runtime::handle() {
                            let _ = handle
                                .spawn(async move { account.confirm_nostr_npub(now_secs()).await })
                                .await;
                        }
                        npub_confirm_group_ref.set_visible(false);
                    }
                    Ok(Err(e)) => {
                        tracing::error!("[settings/nostr] link error: {e}");
                        sr.set_subtitle(common::ERROR);
                    }
                    Err(e) => {
                        tracing::error!("[settings/nostr] join error: {e}");
                        sr.set_subtitle(common::ERROR);
                    }
                }
                br.set_sensitive(true);
            });
        });
    }

    // -----------------------------------------------------------------------
    // Wire: Unlink button
    // -----------------------------------------------------------------------
    {
        let status_row_ref = status_row.clone();
        let pubkey_row_ref = pubkey_row.clone();
        let mode_row_ref = mode_row.clone();
        let relays_state = Rc::clone(&relays_state);
        let relay_list_container = relay_list_container.clone();
        let follow_list_container = follow_list_container.clone();
        let bunker_reveal_box = bunker_reveal_box.clone();
        let bunker_qr_matrix = bunker_qr_matrix.clone();
        let link_gate = link_gate.clone();
        let linked_state = Rc::clone(&linked_state);
        let custodial_state = Rc::clone(&custodial_state);
        let link_mode_combo_ref = link_mode_combo.clone();
        let npub_confirm_group_ref = npub_confirm_group.clone();
        unlink_btn.connect_clicked(move |btn| {
            let Some(client) = super::get_client() else {
                tracing::error!("[settings/nostr] unlink: no client available");
                return;
            };
            let bridges = fauna_client_bridges::BridgesClient::new(client.nest_rpc().clone());
            let handle = client.runtime_handle();

            let sr = status_row_ref.clone();
            let pr = pubkey_row_ref.clone();
            let mr = mode_row_ref.clone();
            let br = btn.clone();
            let relays_state = Rc::clone(&relays_state);
            let relay_list_container = relay_list_container.clone();
            let follow_list_container = follow_list_container.clone();
            let bunker_reveal_box = bunker_reveal_box.clone();
            let bunker_qr_matrix = bunker_qr_matrix.clone();
            let client_for_clear = Rc::clone(&client);
            let link_gate = link_gate.clone();
            let linked_state = Rc::clone(&linked_state);
            let custodial_state = Rc::clone(&custodial_state);
            let mode = selected_link_mode(&link_mode_combo_ref);
            let npub_confirm_group_ref = npub_confirm_group_ref.clone();

            sr.set_subtitle(common::UNLINKING);
            br.set_sensitive(false);

            glib::MainContext::default().spawn_local(async move {
                // `fauna.bridges.unlink` replaces the deleted `DELETE
                // /api/v1/nostr/link` control-plane route.
                let result = handle
                    .spawn(async move { bridges.unlink("nostr").await })
                    .await;

                match result {
                    Ok(Ok(())) => {
                        sr.set_subtitle(common::NOT_LINKED);
                        pr.set_subtitle("—");
                        mr.set_subtitle("—");
                        relays_state.borrow_mut().clear();
                        rebuild_relay_rows(&relay_list_container, &relays_state, &client_for_clear);
                        while let Some(child) = follow_list_container.first_child() {
                            follow_list_container.remove(&child);
                        }
                        bunker_reveal_box.set_visible(false);
                        bunker_qr_matrix.replace(None);
                        linked_state.set(false);
                        custodial_state.set(false);
                        link_gate.apply(false, mode == MODE_IMPORT, mode == MODE_REMOTE, false);
                        // No linked npub left to confirm — covers both a
                        // direct Unlink click and the banner's own "no /
                        // nothing is linked" button (which delegates here).
                        npub_confirm_group_ref.set_visible(false);
                    }
                    Ok(Err(e)) => {
                        tracing::error!("[settings/nostr] unlink error: {e}");
                        sr.set_subtitle(common::ERROR);
                    }
                    Err(e) => {
                        tracing::error!("[settings/nostr] join error: {e}");
                        sr.set_subtitle(common::ERROR);
                    }
                }
                br.set_sensitive(true);
            });
        });
    }

    // -----------------------------------------------------------------------
    // Wire: Settings switches — save on toggle
    // -----------------------------------------------------------------------
    {
        let switches = content_switches.clone();

        let save_settings = move || {
            let Some(client) = super::get_client() else {
                return;
            };

            // `fauna.bridges.set_settings` (bridge_id "nostr") replaces the
            // deleted `PUT /api/v1/nostr/settings`; the settings blob keeps the
            // same field names (nostr.md § WS-RPC migration contract). The nest
            // merges by key (`nostr/db.rs::update_settings` — each field is a
            // separate `UPDATE ... WHERE` gated on `Some`), so sending only
            // these 5 keys never clobbers `relay_list`.
            let settings = Value::Map(
                switches
                    .iter()
                    .map(|(key, sw)| (key.clone(), Value::Bool(sw.is_active())))
                    .collect::<BTreeMap<_, _>>(),
            );

            super::commit_bridge_settings(&client, "nostr", settings);
        };

        // Each flip re-stamps the `state` marker class the e2e reads (the
        // `views/family.rs` idiom — a `SwitchRow` with no explicit marker
        // falls back to a "true"/"false" `state`, not the "on"/"off" contract
        // `driver.get_attr(id, "state")` expects).
        for (_, row) in &content_switches {
            let save = save_settings.clone();
            row.connect_active_notify(move |sw| {
                set_test_attr(sw, "state", if sw.is_active() { "on" } else { "off" });
                save();
            });
        }
    }

    // -----------------------------------------------------------------------
    // Wire: Add relay / Add follow
    // -----------------------------------------------------------------------
    {
        let relays_state = Rc::clone(&relays_state);
        let container = relay_list_container.clone();
        let input = relay_input_row.clone();
        let error_label = error_label.clone();
        add_relay_btn.connect_clicked(move |_| {
            let Some(client) = super::get_client() else {
                return;
            };
            let Some(url) = fauna_protocol::nostr_relay::trimmed_relay_input(&input.text()) else {
                return;
            };
            // The shared predicate and its message: a malformed URL or, per
            // F7, a private-network relay (`nostr_relay::relay_url_error`).
            if let Some(err) = fauna_protocol::nostr_relay::relay_url_error(&url) {
                super::render_error_label(&error_label, Some(&resolve_text(&err)));
                return;
            }
            error_label.set_visible(false);
            {
                let existing = relays_state.borrow().clone();
                if let Some(next) =
                    fauna_protocol::nostr_relay::relay_list_appending(&existing, &url)
                {
                    *relays_state.borrow_mut() = next;
                }
            }
            rebuild_relay_rows(&container, &relays_state, &client);
            save_relay_list(&client, &relays_state.borrow());
            input.set_text("");
        });
    }
    {
        let pubkey_input = follow_pubkey_row.clone();
        let petname_input = follow_petname_row.clone();
        let container = follow_list_container.clone();
        add_follow_btn.connect_clicked(move |_| {
            let Some(client) = super::get_client() else {
                return;
            };
            let pubkey = pubkey_input.text().trim().to_string();
            if pubkey.is_empty() {
                return;
            }
            let petname = petname_input.text().trim().to_string();
            let petname = if petname.is_empty() {
                None
            } else {
                Some(petname)
            };
            let bridges = fauna_client_bridges::BridgesClient::new(client.nest_rpc().clone());
            let handle = client.runtime_handle();
            let pubkey_input = pubkey_input.clone();
            let petname_input = petname_input.clone();
            let container = container.clone();
            let client_for_refresh = Rc::clone(&client);
            glib::MainContext::default().spawn_local(async move {
                let result = handle
                    .spawn(async move { bridges.add_follow("nostr", pubkey, petname, None).await })
                    .await;
                match result {
                    Ok(Ok(())) => {
                        pubkey_input.set_text("");
                        petname_input.set_text("");
                        refresh_follows(&client_for_refresh, &container);
                    }
                    Ok(Err(e)) => tracing::error!("[settings/nostr] add_follow error: {e}"),
                    Err(e) => tracing::error!("[settings/nostr] add_follow join error: {e}"),
                }
            });
        });
    }

    // -----------------------------------------------------------------------
    // Wire: Designate a zap signer
    // -----------------------------------------------------------------------
    #[cfg(feature = "zaps")]
    {
        let pubkey_input = zap_signer_pubkey_row.clone();
        let label_input = zap_signer_label_row.clone();
        let container = zap_signers_container.clone();
        let empty_label = zap_signers_empty_label.clone();
        let error_label = error_label.clone();
        zap_signer_add_btn.connect_clicked(move |_| {
            let Some(client) = super::get_client() else {
                return;
            };
            let pubkey = pubkey_input.text().trim().to_string();
            if pubkey.is_empty() {
                return;
            }
            // Client-glue validation, written straight to `error-message` like
            // the relay check above: the nest refuses a non-64-hex key anyway,
            // so this only spares a guaranteed round trip and names the rule.
            if pubkey.len() != 64 || !pubkey.chars().all(|c| c.is_ascii_hexdigit()) {
                super::render_error_label(&error_label, Some(nostr::zap_signers::INVALID_PUBKEY));
                return;
            }
            error_label.set_visible(false);
            let label = label_input.text().trim().to_string();
            let zap_signers =
                fauna_client_nostr::NostrZapSignerClient::new(client.nest_rpc().clone());
            let handle = client.runtime_handle();
            let pubkey_input = pubkey_input.clone();
            let label_input = label_input.clone();
            let container = container.clone();
            let empty_label = empty_label.clone();
            let client_for_refresh = Rc::clone(&client);
            glib::MainContext::default().spawn_local(async move {
                let result = handle
                    .spawn(async move { zap_signers.add(pubkey, label).await })
                    .await;
                match result {
                    Ok(Ok(_)) => {
                        pubkey_input.set_text("");
                        label_input.set_text("");
                        refresh_zap_signers(&client_for_refresh, &container, &empty_label);
                    }
                    Ok(Err(e)) => tracing::error!("[settings/nostr] zap_signers add error: {e}"),
                    Err(e) => tracing::error!("[settings/nostr] zap_signers add join error: {e}"),
                }
            });
        });
    }

    // -----------------------------------------------------------------------
    // Wire: Connect an app (mint invite → one-time reveal) / Copy connect
    // string. The connections themselves are rows of Settings → Connected apps.
    // -----------------------------------------------------------------------
    {
        let reveal_box = bunker_reveal_box.clone();
        let qr_area = bunker_qr_area.clone();
        let qr_matrix = bunker_qr_matrix.clone();
        let connect_string_label = bunker_connect_string_label.clone();
        let error_label = error_label.clone();
        bunker_connect_btn.connect_clicked(move |_| {
            let Some(client) = super::get_client() else {
                tracing::error!("[settings/nostr] bunker connect: no client available");
                return;
            };
            let bunker = fauna_client_nostr::NostrBunkerClient::new(client.nest_rpc().clone());
            let handle = client.runtime_handle();

            let reveal_box = reveal_box.clone();
            let qr_area = qr_area.clone();
            let qr_matrix = qr_matrix.clone();
            let connect_string_label = connect_string_label.clone();
            let error_label = error_label.clone();

            glib::MainContext::default().spawn_local(async move {
                let result = handle
                    .spawn(async move { bunker.create_invite().await })
                    .await;
                match result {
                    Ok(Ok(reply)) => {
                        error_label.set_visible(false);
                        connect_string_label.set_text(&reply.connect_string);
                        match fauna_core::qr_matrix::qr_matrix(&reply.connect_string) {
                            Ok(m) => {
                                qr_matrix.replace(Some(m));
                                qr_area.queue_draw();
                                qr_area.set_visible(true);
                            }
                            Err(e) => {
                                tracing::error!("[settings/nostr] bunker QR encode error: {e}");
                                qr_matrix.replace(None);
                                qr_area.set_visible(false);
                            }
                        }
                        reveal_box.set_visible(true);
                    }
                    Ok(Err(e)) => {
                        tracing::error!("[settings/nostr] bunker create_invite error: {e}");
                        super::render_error_label(&error_label, Some(common::ERROR));
                    }
                    Err(e) => {
                        tracing::error!("[settings/nostr] bunker create_invite join error: {e}");
                        super::render_error_label(&error_label, Some(common::ERROR));
                    }
                }
            });
        });
    }
    {
        let connect_string_label = bunker_connect_string_label.clone();
        bunker_copy_btn.connect_clicked(move |btn| {
            let text = connect_string_label.text();
            if !text.is_empty() {
                crate::clipboard::copy_text(&text);
                btn.set_label(common::COPIED);
                let btn_weak = btn.downgrade();
                glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                    if let Some(btn) = btn_weak.upgrade() {
                        btn.set_label(common::COPY);
                    }
                });
            }
        });
    }

    // -----------------------------------------------------------------------
    // Initial fetch of nostr status
    // -----------------------------------------------------------------------
    {
        let sr = status_row;
        let pr = pubkey_row;
        let mr = mode_row;
        let switches = content_switches;
        let relays_state = Rc::clone(&relays_state);
        let relay_list_container = relay_list_container.clone();
        let follow_list_container = follow_list_container.clone();
        #[cfg(feature = "zaps")]
        let zap_signers_container = zap_signers_container.clone();
        #[cfg(feature = "zaps")]
        let zap_signers_empty_label = zap_signers_empty_label.clone();
        #[cfg(feature = "zaps")]
        let zap_signer_add_btn = zap_signer_add_btn.clone();
        #[cfg(feature = "zaps")]
        let zap_signer_gate_reason_label = zap_signer_gate_reason_label.clone();
        let link_gate = link_gate.clone();
        let linked_state = Rc::clone(&linked_state);
        let custodial_state = Rc::clone(&custodial_state);
        let link_mode_combo_ref = link_mode_combo.clone();
        let npub_confirm_group_ref = npub_confirm_group.clone();
        let npub_confirm_label_ref = npub_confirm_label.clone();

        glib::idle_add_local_once(move || {
            let Some(client) = super::get_client() else {
                sr.set_subtitle(nostr::account::STATUS_NO_CLIENT);
                return;
            };
            let bridges = fauna_client_bridges::BridgesClient::new(client.nest_rpc().clone());
            let handle = client.runtime_handle();
            let client_for_follows = Rc::clone(&client);
            let client_for_relays = Rc::clone(&client);
            #[cfg(feature = "zaps")]
            let client_for_zap_signers = Rc::clone(&client);
            #[cfg(feature = "zaps")]
            let client_for_zap_signer_gate = Rc::clone(&client);
            let nest_for_confirm = client.nest_rpc().clone();

            glib::MainContext::default().spawn_local(async move {
                // `fauna.bridges.list` (filtered to bridge_id "nostr") replaces
                // the deleted `GET /api/v1/nostr/status` (nostr.md § WS-RPC
                // migration contract). On the generic bridge wire: npub rides
                // `identity.value`; the publish flags + `relay_list` ride
                // `settings[]` keyed by `key`; `available == false` means the
                // nest is encrypted / unconfigured (Phase-1 storage-mode gate).
                let result = handle.spawn(async move { bridges.list().await }).await;

                let mut linked = false;
                match result {
                    Ok(Ok(reply)) => match reply.bridges.iter().find(|b| b.id == "nostr") {
                        Some(b) if !b.available => {
                            sr.set_subtitle(nostr::account::STATUS_UNAVAILABLE);
                        }
                        Some(b) if b.linked => {
                            linked = true;
                            sr.set_subtitle(common::LINKED);
                            let npub = b
                                .identity
                                .as_ref()
                                .map(|i| i.value.as_str())
                                .unwrap_or("(unknown)");
                            pr.set_subtitle(npub);
                            mr.set_subtitle(&crate::i18n::nostr_signing_mode_label(
                                b.mode.as_deref().unwrap_or("—"),
                            ));
                            // Connected apps (NIP-46 bunker) needs a deposited
                            // key on this box — `remote` accounts sign
                            // elsewhere (nostr.md § Layout & flow item 6).
                            // Mirrors web's `mode === 'generated' || mode ===
                            // 'imported'`: an absent/unknown mode is treated
                            // as non-custodial (hide), not custodial.
                            custodial_state.set(matches!(
                                b.mode.as_deref(),
                                Some("generated") | Some("imported")
                            ));
                            // Each row's default comes from the same catalog
                            // that built it, so a never-configured account
                            // paints what the nest would actually do.
                            for (opt, (_, sw)) in
                                nostr_content_toggle_options().iter().zip(switches.iter())
                            {
                                sw.set_active(bool_setting(&b.settings, &opt.key, opt.default_on));
                                set_test_attr(
                                    sw,
                                    "state",
                                    if sw.is_active() { "on" } else { "off" },
                                );
                            }
                            *relays_state.borrow_mut() = relay_list_setting(&b.settings);
                            rebuild_relay_rows(
                                &relay_list_container,
                                &relays_state,
                                &client_for_relays,
                            );

                            // Succession-aftermath npub confirm (leg 3 —
                            // nostr.md § Key succession and rotation),
                            // checked once on this nav-enter read — the same
                            // "extra leg" tui's `Op::Refresh` documents.
                            let npub_owned = npub.to_string();
                            let account = crate::account_runtime::handle();
                            let owed = handle
                                .spawn(async move {
                                    npub_confirmation_owed_for(
                                        nest_for_confirm.as_ref(),
                                        account.as_ref().map(|a| a.npub_confirmed_at()),
                                    )
                                    .await
                                })
                                .await
                                .unwrap_or(false);
                            if owed {
                                npub_confirm_label_ref
                                    .set_text(&nostr::npub_confirm::banner(&npub_owned));
                            }
                            npub_confirm_group_ref.set_visible(owed);
                        }
                        // Not linked, or no Nostr bridge registered on this nest.
                        _ => {
                            sr.set_subtitle(common::NOT_LINKED);
                            npub_confirm_group_ref.set_visible(false);
                        }
                    },
                    Ok(Err(e)) => {
                        tracing::error!("[settings/nostr] status error: {e}");
                        sr.set_subtitle(common::ERROR);
                    }
                    Err(e) => {
                        tracing::error!("[settings/nostr] join error: {e}");
                        sr.set_subtitle(common::ERROR);
                    }
                }

                linked_state.set(linked);
                let mode = selected_link_mode(&link_mode_combo_ref);
                link_gate.apply(
                    linked,
                    mode == MODE_IMPORT,
                    mode == MODE_REMOTE,
                    custodial_state.get(),
                );

                if linked {
                    refresh_follows(&client_for_follows, &follow_list_container);
                    #[cfg(feature = "zaps")]
                    {
                        refresh_zap_signers(
                            &client_for_zap_signers,
                            &zap_signers_container,
                            &zap_signers_empty_label,
                        );
                        refresh_zap_signer_gate(
                            &client_for_zap_signer_gate,
                            &zap_signer_add_btn,
                            &zap_signer_gate_reason_label,
                        );
                    }
                }
            });
        });
    }

    page
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_link_gate() -> NostrLinkGate {
        NostrLinkGate {
            pubkey_row: adw::ActionRow::builder().build(),
            mode_row: adw::ActionRow::builder().build(),
            unlink_row: adw::ActionRow::builder().build(),
            settings_group: adw::PreferencesGroup::builder().build(),
            relays_group: adw::PreferencesGroup::builder().build(),
            follows_group: adw::PreferencesGroup::builder().build(),
            link_mode_combo: adw::ComboRow::builder().build(),
            link_row: adw::ActionRow::builder().build(),
            nsec_row: adw::PasswordEntryRow::builder().build(),
            bunker_row: adw::EntryRow::builder().build(),
            connected_apps_group: adw::PreferencesGroup::builder().build(),
            #[cfg(feature = "zaps")]
            zap_signers_group: adw::PreferencesGroup::builder().build(),
        }
    }

    /// The regression this test pins: before `NostrLinkGate` existed, every
    /// row/group rendered unconditionally, so `nostr-pubkey-copy-btn` (the
    /// e2e `is_linked()` signal) was always visible regardless of real link
    /// state — which broke unlink detection AND made `ensure_linked()` skip
    /// a real link call in a later test.
    #[test]
    fn link_gate_swaps_unlinked_form_for_linked_surface() {
        crate::testid::run_on_gtk_thread(|| {
            let gate = bare_link_gate();

            gate.apply(false, false, false, false);
            assert!(!gate.pubkey_row.is_visible());
            assert!(!gate.mode_row.is_visible());
            assert!(!gate.unlink_row.is_visible());
            assert!(!gate.settings_group.is_visible());
            assert!(!gate.relays_group.is_visible());
            assert!(!gate.follows_group.is_visible());
            assert!(!gate.connected_apps_group.is_visible());
            #[cfg(feature = "zaps")]
            assert!(!gate.zap_signers_group.is_visible());
            assert!(gate.link_mode_combo.is_visible());
            assert!(gate.link_row.is_visible());

            gate.apply(true, false, false, true);
            assert!(gate.pubkey_row.is_visible());
            assert!(gate.mode_row.is_visible());
            assert!(gate.unlink_row.is_visible());
            assert!(gate.settings_group.is_visible());
            assert!(gate.relays_group.is_visible());
            assert!(gate.follows_group.is_visible());
            assert!(gate.connected_apps_group.is_visible());
            #[cfg(feature = "zaps")]
            assert!(gate.zap_signers_group.is_visible());
            assert!(!gate.link_mode_combo.is_visible());
            assert!(!gate.link_row.is_visible());
        });
    }

    /// Unlike `connected_apps_group`, `zap_signers_group` is gated on
    /// `linked` ALONE — nostr.md § Layout & flow item 7: designating who may
    /// speak for your money is orthogonal to where your key lives, so a
    /// `remote` (non-custodial) linked account still gets the section.
    #[cfg(feature = "zaps")]
    #[test]
    fn zap_signers_group_requires_only_linked() {
        crate::testid::run_on_gtk_thread(|| {
            let gate = bare_link_gate();

            gate.apply(true, false, false, false); // linked, remote (non-custodial)
            assert!(gate.zap_signers_group.is_visible());

            gate.apply(false, false, false, false); // not linked
            assert!(!gate.zap_signers_group.is_visible());
        });
    }

    /// `connected_apps_group` (Connected apps / NIP-46 bunker) is gated on
    /// BOTH `linked` and `custodial` — nostr.md § Layout & flow item 6:
    /// "Rendered only for a linked account in a custodial mode". A `remote`
    /// account has no key on this box, so it must never show the section
    /// even while linked.
    #[test]
    fn connected_apps_group_requires_linked_and_custodial() {
        crate::testid::run_on_gtk_thread(|| {
            let gate = bare_link_gate();

            gate.apply(true, false, false, false); // linked, remote (non-custodial)
            assert!(!gate.connected_apps_group.is_visible());

            gate.apply(false, false, false, true); // custodial but not linked
            assert!(!gate.connected_apps_group.is_visible());

            gate.apply(true, false, false, true); // linked and custodial
            assert!(gate.connected_apps_group.is_visible());
        });
    }

    #[test]
    fn link_gate_shows_nsec_or_bunker_only_while_unlinked_and_mode_matches() {
        crate::testid::run_on_gtk_thread(|| {
            let gate = bare_link_gate();

            gate.apply(false, true, false, false); // unlinked, import mode
            assert!(gate.nsec_row.is_visible());
            assert!(!gate.bunker_row.is_visible());

            gate.apply(false, false, true, false); // unlinked, remote mode
            assert!(!gate.nsec_row.is_visible());
            assert!(gate.bunker_row.is_visible());

            // Linked hides both regardless of the stale mode-flag values — the
            // mode picker itself is gone, so its selection can't matter anymore.
            gate.apply(true, true, true, true);
            assert!(!gate.nsec_row.is_visible());
            assert!(!gate.bunker_row.is_visible());
        });
    }

    #[test]
    fn link_mode_label_covers_all_three_native_modes() {
        // Delegates to the shared `fauna_client_bridges::nostr_link_mode_label`
        // (via `crate::i18n`'s wrapper) rather than a local match — this pins
        // the resolved English text stays exactly what it was before the lift.
        assert_eq!(
            crate::i18n::nostr_link_mode_label(MODE_GENERATE),
            nostr::link_account::GENERATE
        );
        assert_eq!(
            crate::i18n::nostr_link_mode_label(MODE_IMPORT),
            nostr::link_account::IMPORT_NSEC
        );
        assert_eq!(
            crate::i18n::nostr_link_mode_label(MODE_REMOTE),
            nostr::account::MODE_REMOTE
        );
        // Unlike the retired local match, the shared fn has a real key for
        // `nip07` (the web-only fourth mode) rather than folding it into
        // "generate" — see `fauna_client_bridges::nostr_link_mode_label`.
        assert_eq!(
            crate::i18n::nostr_link_mode_label("nip07"),
            nostr::link_account::NIP07
        );
    }

    #[cfg(feature = "zaps")]
    fn zap_entry(pubkey: &str, label: &str) -> ZapSignerEntry {
        ZapSignerEntry {
            id: 1,
            signer_pubkey: pubkey.to_string(),
            label: label.to_string(),
            created_at: 0,
            extra: Default::default(),
        }
    }

    #[cfg(feature = "zaps")]
    #[test]
    fn zap_signer_row_text_prefers_label_then_falls_back_to_unnamed() {
        let labeled = zap_signer_row_text(&zap_entry(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd",
            "Alby",
        ));
        assert!(labeled.starts_with("Alby — "), "got {labeled}");

        let unlabeled = zap_signer_row_text(&zap_entry(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd",
            "",
        ));
        assert!(
            unlabeled.starts_with(nostr::zap_signers::UNNAMED),
            "got {unlabeled}"
        );
    }

    // `zaps_row` lives in `fauna_client_features::test_fixtures` — one
    // canonical body shared with fauna-tui's identical fixture (`nostr.rs`).
    #[cfg(feature = "zaps")]
    use fauna_client_features::test_fixtures::zaps_row;

    #[cfg(feature = "zaps")]
    #[test]
    fn zap_signer_add_gate_reason_is_none_when_available() {
        let rows = vec![zaps_row(&[])];
        assert_eq!(zap_signer_add_gate_reason(&rows), None);
    }

    #[cfg(feature = "zaps")]
    #[test]
    fn zap_signer_add_gate_reason_names_the_restriction_when_denied() {
        let denied = fauna_core::feature_gate::FeaturePolicy {
            availability: fauna_core::feature_gate::Availability::Deny,
            ..Default::default()
        };
        let rows = vec![zaps_row(&[(
            fauna_client_features::RuleTier::Admin,
            denied,
        )])];
        assert!(
            zap_signer_add_gate_reason(&rows).is_some(),
            "a denied row must disable the button with a reason"
        );
    }

    #[cfg(feature = "zaps")]
    #[test]
    fn zap_signer_add_gate_reason_is_none_when_the_row_is_absent() {
        // An un-hydrated read (no rows fetched yet, or `zaps` missing from a
        // stale nest) must leave the button LIVE — the nest, not the app, is
        // the enforcement floor.
        assert_eq!(zap_signer_add_gate_reason(&[]), None);
    }
}
