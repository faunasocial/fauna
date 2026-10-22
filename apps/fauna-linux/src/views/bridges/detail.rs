use adw::prelude::*;
use fauna_client::Value;
use fauna_client_bridges::bridges_ui::{
    BridgeFollow, BridgeIdentity, BridgeLinkMode, BridgeSetting,
};
use fauna_client_bridges::{SettingControl, follow_display, setting_control};
use fauna_ui_ids as ids;
use gtk::glib;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::client::FaunaClient;
use crate::i18n::strings::{bridges as bridges_strings, common};

// ── The ward's feed-source asks (family-safety.md § Feed-source approvals) ──
//
// The `bridge-source-request-button` / `bridge-source-request-state` pair
// inside `bridge-card`, lifted from tui's `bridges::source_ask_rows`. Two
// inputs, both supervised-only by construction (so no "is supervised" test is
// written — rule (h)): the durable `status.feed_requests` and this session's
// TYPED refusals, both held in `crate::ward_asks`. Rows are keyed on the ask
// data itself, never on the add-follow row's buffers (rule (d)) — the row clears
// its entries on send, so they are gone by the time a refusal comes back.

/// One open card's ask container (weak) and its repaint hook.
type SourceAskPane = (glib::WeakRef<gtk::Box>, Rc<dyn Fn()>);

thread_local! {
    /// Every live card's repaint hook, so a `FeedSourceRefused` or a status
    /// read can repaint whichever card(s) are open (the Bridges page's detail
    /// pane, the AT Protocol page's embedded panel). Weak on both the container and
    /// the client, so a torn-down card or a signed-out client is pruned rather
    /// than kept alive.
    static SOURCE_ASK_PANES: RefCell<Vec<SourceAskPane>> =
        const { RefCell::new(Vec::new()) };
}

/// Paint `container` now and register it for [`repaint_source_asks`].
fn register_source_asks(container: &gtk::Box, bridge_id: &str, client: &Rc<FaunaClient>) {
    let weak_box = container.downgrade();
    let weak_client = Rc::downgrade(client);
    let bridge_id = bridge_id.to_string();
    let repaint: Rc<dyn Fn()> = Rc::new(move || {
        let (Some(container), Some(client)) = (weak_box.upgrade(), weak_client.upgrade()) else {
            return;
        };
        let weak_client = Rc::downgrade(&client);
        fill_source_asks(&container, &bridge_id, move |bridge, operation, target| {
            let Some(client) = weak_client.upgrade() else {
                return;
            };
            ask_for_source(&client, &bridge, &operation, &target);
        });
    });
    repaint();
    SOURCE_ASK_PANES.with_borrow_mut(|panes| {
        panes.retain(|(w, _)| w.upgrade().is_some());
        panes.push((container.downgrade(), repaint));
    });
}

/// Repaint every open card's ask rows from `crate::ward_asks` — called when a
/// refusal lands, when an ask lands, and on a status read.
pub fn repaint_source_asks() {
    let hooks: Vec<Rc<dyn Fn()>> = SOURCE_ASK_PANES.with_borrow_mut(|panes| {
        panes.retain(|(w, _)| w.upgrade().is_some());
        panes.iter().map(|(_, f)| Rc::clone(f)).collect()
    });
    for hook in hooks {
        hook();
    }
}

/// `fauna.family.feed_source.request` for one refused triple. On success the
/// nest's re-read lands in the durable store (the button swaps for its state)
/// and the refusal comes off `error-message`; on failure the ask's own typed
/// refusal is the ward's to read verbatim.
fn ask_for_source(client: &Rc<FaunaClient>, bridge_id: &str, operation: &str, target: &str) {
    let tx = client.tx();
    // Display-only: the add-follow petname is gone by now (the row cleared on send),
    // so the bridge id is the one label still available — nothing is invented.
    client.request_feed_source(bridge_id, operation, target, bridge_id, move |result| {
        match result {
            Ok(requests) => {
                crate::ward_asks::replace_feed_requests(requests);
                tx.send(crate::app::UiMessage::Action(
                    crate::app::ActionResult::Success {
                        context: "feed_source_requested".into(),
                    },
                ));
            }
            Err(e) => {
                tx.send(crate::app::UiMessage::Action(
                    crate::app::ActionResult::FailedLocalized { message: e },
                ));
            }
        }
        repaint_source_asks();
    });
}

/// Replace `container`'s rows for `bridge_id`: first one
/// `bridge-source-request-state` per durable ask (pending, or APPROVED → the
/// "try again" PROMPT — never an auto-retry, since the grant is single-use and
/// the ward redeems it by repeating the original Link / Add Follow, which stays
/// on the card; rule (e)), then one `bridge-source-request-button` per refused
/// triple that has no durable ask yet. `on_ask(bridge, operation, target)` is
/// the button's action. Hidden when there is nothing to show.
fn fill_source_asks(
    container: &gtk::Box,
    bridge_id: &str,
    on_ask: impl Fn(String, String, String) + Clone + 'static,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let mut any = false;
    for ask in crate::ward_asks::feed_requests_for(bridge_id) {
        let text = match fauna_client_family::FeedRequestState::of(&ask) {
            fauna_client_family::FeedRequestState::Approved => {
                bridges_strings::SOURCE_REQUEST_APPROVED
            }
            fauna_client_family::FeedRequestState::Pending => {
                bridges_strings::SOURCE_REQUEST_PENDING
            }
        };
        let state = gtk::Label::new(Some(text));
        state.set_halign(gtk::Align::Start);
        state.add_css_class("dim-label");
        crate::testid::set_test_id(&state, ids::BRIDGE_SOURCE_REQUEST_STATE);
        container.append(&state);
        any = true;
    }
    for (operation, target) in crate::ward_asks::feed_refusals_for(bridge_id) {
        // An answered ask shows its verdict (above) rather than the button again.
        if crate::ward_asks::feed_request_state(bridge_id, &operation, &target).is_some() {
            continue;
        }
        let btn = gtk::Button::with_label(bridges_strings::SOURCE_REQUEST_BUTTON);
        btn.set_halign(gtk::Align::Start);
        crate::testid::set_test_id(&btn, ids::BRIDGE_SOURCE_REQUEST_BUTTON);
        crate::offline_gate::declare_wire_kind(&btn, "fauna.family.feed_source.request");
        {
            let on_ask = on_ask.clone();
            let bridge = bridge_id.to_string();
            btn.connect_clicked(move |b| {
                b.set_sensitive(false);
                on_ask(bridge.clone(), operation.clone(), target.clone());
            });
        }
        container.append(&btn);
        any = true;
    }
    container.set_visible(any);
}

// ── Small Value builder for the link form ──────────────────────────
//
// The generic link form (`build_link_dialog`) builds a tiny key→value map from
// the provider-declared `BridgeLinkMode.fields[]`, which rides as the
// `params: Value` argument to `fauna.bridges.link`.

/// Only the modes available on this platform — the shared rule
/// (`fauna_client_bridges::applicable_modes`, lifted 2026-08-15 from the seven
/// per-app copies of this exact filter; the shared module owns the vocabulary).
/// This wrapper pins linux's canonical name; one call site shared by the detail
/// pane's Link trigger and the dialog's submit, so the two can never disagree
/// about whether this bridge is linkable.
fn platform_applicable(link_modes: &[BridgeLinkMode]) -> Vec<BridgeLinkMode> {
    fauna_client_bridges::applicable_modes(link_modes, "linux")
}

fn cbor_text_entry(key: &str, value: impl Into<String>) -> (String, Value) {
    (key.to_string(), Value::String(value.into()))
}

/// Handles a caller needs back from a built detail pane — the `follows_list`
/// widget, so a caller can repaint it when a `fauna.bridges.list_follows` reply
/// lands later (the pane used to render nothing to hand back, so the
/// follows section always showed its empty placeholder no matter what the nest
/// returned), and the follow-requests section, repainted the same way.
/// Mirrors `list::BridgeListHandles`'s shape.
#[derive(Clone)]
pub struct BridgeDetailHandles {
    pub follows_list: gtk::ListBox,
    pub follow_requests: FollowRequestsHandles,
}

/// The *Follow requests* section of a bridge card (`bridges.md` § Follow
/// requests). Built hidden: only a bridge that declares
/// `supports_follow_requests` is ever asked for its requests, and the section
/// appears with the first reply — empty, it keeps its heading at a zero count.
#[derive(Clone)]
pub struct FollowRequestsHandles {
    pub section: gtk::Box,
    pub heading: gtk::Label,
    pub list: gtk::ListBox,
}

/// Build the bridge detail pane for a given bridge.
///
/// Displays:
/// - Link / Unlink controls
/// - Identity info (when linked)
/// - Settings (when linked, as editable rows)
/// - Follows list with add/remove (when linked + supports_follows)
#[allow(clippy::too_many_arguments)]
pub fn build_bridge_detail(
    bridge_id: &str,
    bridge_name: &str,
    link_modes: Vec<BridgeLinkMode>,
    linked: bool,
    error: Option<String>,
    identity: Option<BridgeIdentity>,
    settings: Vec<BridgeSetting>,
    client: Rc<FaunaClient>,
) -> (gtk::Box, BridgeDetailHandles) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(bridge_name))));
    outer.append(&header);

    let (scroll_content, handles) = build_bridge_detail_content(
        bridge_id, link_modes, linked, error, identity, settings, client,
    );

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&scroll_content)
        .build();

    outer.append(&scrolled);

    (outer, handles)
}

/// The inner content of a bridge detail pane — link/identity, the link button
/// row, settings, and follows — as a scrollable vertical `gtk::Box`, WITHOUT the
/// HeaderBar/ScrolledWindow chrome that `build_bridge_detail` wraps around it for
/// the standalone Bridges detail page. Extracted so the **Bluesky** settings page
/// can embed the very same content as its Linked-account panel (ui/atproto.md
/// § Layout & flow — the Linked panel reuses the shared bridge-link-form/
/// bridge-card components; zero new IDs).
pub fn build_bridge_detail_content(
    bridge_id: &str,
    link_modes: Vec<BridgeLinkMode>,
    linked: bool,
    error: Option<String>,
    identity: Option<BridgeIdentity>,
    settings: Vec<BridgeSetting>,
    client: Rc<FaunaClient>,
) -> (gtk::Box, BridgeDetailHandles) {
    let scroll_content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    scroll_content.set_margin_top(16);
    scroll_content.set_margin_bottom(16);
    scroll_content.set_margin_start(16);
    scroll_content.set_margin_end(16);
    crate::testid::set_test_id(&scroll_content, ids::BRIDGE_CARD);

    // -----------------------------------------------------------------------
    // Link / Identity section
    // -----------------------------------------------------------------------
    let link_group = adw::PreferencesGroup::new();
    link_group.set_title(bridges_strings::LINK_STATUS);

    let identity_row = adw::ActionRow::new();
    identity_row.set_title(common::IDENTITY);
    identity_row.set_subtitle(common::NOT_LINKED);
    // A linked card shows the identity the nest reports for the account
    // (`BridgeStatus.identity`): `bridge-identity` reads `<label>: <display>`,
    // the shared line every app paints (`bridges.md` § User actions). An
    // unlinked card has none, so the placeholder row carries no id.
    if let (true, Some(identity)) = (linked, identity.as_ref()) {
        identity_row.set_title(&identity.label);
        identity_row.set_subtitle(&identity.display);
        crate::testid::set_test_id(&identity_row, ids::BRIDGE_IDENTITY);
        crate::testid::set_test_text(
            &identity_row,
            &format!("{}: {}", identity.label, identity.display),
        );
    }
    link_group.add(&identity_row);

    let mode_row = adw::ActionRow::new();
    mode_row.set_title(common::MODE);
    mode_row.set_subtitle("—");
    link_group.add(&mode_row);

    scroll_content.append(&link_group);

    // -----------------------------------------------------------------------
    // Link button row
    // -----------------------------------------------------------------------
    let link_btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    link_btn_row.set_halign(gtk::Align::Start);

    // Exactly one of Link/Unlink is showing at a time, matching every other
    // app's single dual-purpose `bridge-action-button` (bridges.md §
    // Element IDs / § User actions) — both used to render unconditionally
    // (a pre-existing bug: clicking "Link" while already linked opened an
    // empty, disabled dialog; "Unlink" had no test id at all). Sharing the
    // id is then collision-free: `is_showing()` prunes the hidden one.
    let link_btn = gtk::Button::with_label(bridges_strings::LINK_BRIDGE);
    link_btn.add_css_class("suggested-action");
    link_btn.set_visible(!linked);
    crate::testid::set_test_id(&link_btn, ids::BRIDGE_ACTION_BUTTON);

    let unlink_btn = gtk::Button::with_label(bridges_strings::UNLINK_BRIDGE);
    unlink_btn.add_css_class("destructive-action");
    unlink_btn.set_visible(linked);
    crate::testid::set_test_id(&unlink_btn, ids::BRIDGE_ACTION_BUTTON);
    // Direct call, no intermediate dialog (unlike the Link trigger below) —
    // this button IS the fauna.bridges.unlink ceremony.
    crate::offline_gate::declare_wire_kind(&unlink_btn, "fauna.bridges.unlink");

    link_btn_row.append(&link_btn);
    link_btn_row.append(&unlink_btn);
    scroll_content.append(&link_btn_row);

    // The ward's feed-source ask rows for THIS card (`family-safety.md`
    // § Feed-source approvals) — empty and hidden in the common case; painted
    // only where the guardian gate actually bit. Registered so a refusal or a
    // status read that lands while the card is open repaints it.
    let source_asks = gtk::Box::new(gtk::Orientation::Vertical, 4);
    register_source_asks(&source_asks, bridge_id, &client);
    scroll_content.append(&source_asks);

    // Nothing to link with? The trigger goes insensitive HERE, on the pane, and
    // the reason renders beside it — so the user learns why without opening a
    // dialog whose submit button was dead on arrival (`bridges.md` § Errors &
    // edge cases: disabled, not absent). The dialog's own submit keeps its
    // matching gate below as a second line of defence.
    let platform_modes = platform_applicable(&link_modes);
    if let Some(block) =
        fauna_client_bridges::link_block_of(linked, error.as_deref(), platform_modes.len())
    {
        link_btn.set_sensitive(false);
        let reason = gtk::Label::new(Some(&block.text()));
        reason.set_halign(gtk::Align::Start);
        reason.set_wrap(true);
        reason.add_css_class("dim-label");
        crate::testid::set_test_id(&reason, ids::BRIDGE_LINK_BLOCKED_REASON);
        scroll_content.append(&reason);
    }

    // Wire "Link" button to open the metadata-driven link dialog.
    {
        let bid = bridge_id.to_string();
        let modes = link_modes.clone();
        let c = Rc::clone(&client);
        link_btn.connect_clicked(move |btn| {
            let dialog = build_link_dialog(&bid, &modes, Rc::clone(&c));
            if let Some(root) = btn.root()
                && let Some(win) = root.downcast_ref::<gtk::Window>()
            {
                dialog.set_transient_for(Some(win));
            }
            dialog.present();
        });
    }

    // Wire "Unlink" button.
    {
        let bid = bridge_id.to_string();
        let c = Rc::clone(&client);
        unlink_btn.connect_clicked(move |_| {
            c.unlink_bridge(&bid);
        });
    }

    // -----------------------------------------------------------------------
    // Settings section
    // -----------------------------------------------------------------------
    let settings_group = build_settings_group(&settings, {
        let bridge_id = bridge_id.to_string();
        let client = Rc::clone(&client);
        Rc::new(move |key, value| commit_setting(&client, &bridge_id, key, value))
    });
    scroll_content.append(&settings_group);

    // -----------------------------------------------------------------------
    // Follows section
    // -----------------------------------------------------------------------
    let follows_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    follows_header.set_margin_top(8);

    let follows_title = gtk::Label::new(Some(bridges_strings::FOLLOWS));
    follows_title.add_css_class("heading");
    follows_title.set_hexpand(true);
    follows_title.set_halign(gtk::Align::Start);

    follows_header.append(&follows_title);
    scroll_content.append(&follows_header);

    let follows_list = gtk::ListBox::new();
    follows_list.set_selection_mode(gtk::SelectionMode::None);
    follows_list.add_css_class("boxed-list");

    let follows_placeholder = gtk::Label::new(Some(bridges_strings::NO_FOLLOWS_CONFIGURED));
    follows_placeholder.add_css_class("dim-label");
    follows_placeholder.set_margin_top(8);
    follows_placeholder.set_margin_bottom(8);
    follows_list.set_placeholder(Some(&follows_placeholder));

    scroll_content.append(&follows_list);

    // The add row — inline on the card like every sibling app (web's
    // `BridgeCard.svelte`, tui's `follows_elements`): the id and the optional
    // petname, then the button that sends both.
    scroll_content.append(&build_add_follow_row(bridge_id, Rc::clone(&client)));

    // -----------------------------------------------------------------------
    // Follow requests section (hidden until a `supports_follow_requests` bridge
    // answers — `populate_follow_requests`)
    // -----------------------------------------------------------------------
    let requests_section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    requests_section.set_visible(false);
    let requests_heading = gtk::Label::new(None);
    requests_heading.add_css_class("heading");
    requests_heading.set_halign(gtk::Align::Start);
    requests_heading.set_margin_top(8);
    crate::testid::set_test_id(&requests_heading, ids::BRIDGE_FOLLOW_REQUESTS_LIST);
    let requests_list = gtk::ListBox::new();
    requests_list.set_selection_mode(gtk::SelectionMode::None);
    requests_list.add_css_class("boxed-list");
    requests_section.append(&requests_heading);
    requests_section.append(&requests_list);
    scroll_content.append(&requests_section);

    // (The bridge-feed-subscription elements — bridge-feed-subscriptions /
    // bridge-feed-item / bridge-subscribe-feed-button — were retired here per
    // the Feed-only decision: subscription is the Feed page's `bridge-form-*`
    // surface, not the bridge detail. See bridges.md § Layout & flow.)

    // Note: Protocol-specific timeline/DM/feed sections are intentionally absent.
    // The nest abstracts bridge content into unified feeds and conversations.
    // Only the link dialog (below) needs per-protocol field customisation.
    let bid_lower = bridge_id.to_lowercase();
    let _ = &bid_lower;

    (
        scroll_content,
        BridgeDetailHandles {
            follows_list,
            follow_requests: FollowRequestsHandles {
                section: requests_section,
                heading: requests_heading,
                list: requests_list,
            },
        },
    )
}

/// Render a bridge's `settings[]` as editable rows, one `adw::PreferencesGroup`
/// row per metadata-driven `BridgeSetting` — the linux leg of the six-app
/// bridge-settings renderer (`bridges.md` § Bridge settings; mirrors tui's
/// `setting_elements` / web's `BridgeCard.svelte` / android's `SettingRow`
/// arm-for-arm, priority #1). Each row commits its OWN key via
/// `fauna.bridges.set_settings` the moment it changes — fire-and-forget,
/// mirroring `settings/nostr_tab.rs`'s `save_settings` idiom (the widget's own
/// state already reflects the user's intent; a transient error just logs).
///
/// `commit` writes one changed `(key, value)`: a bridge's card sends it over
/// `fauna.bridges.set_settings`, a hosted plugin's settings card over
/// `fauna.plugins.set_settings` (`views::bridges::plugins`) — the one
/// renderer for both, as tui's `setting_elements` is.
#[allow(clippy::type_complexity)]
pub(super) fn build_settings_group(
    settings: &[BridgeSetting],
    commit: Rc<dyn Fn(&str, Value)>,
) -> adw::PreferencesGroup {
    let settings_group = adw::PreferencesGroup::new();
    settings_group.set_title(common::SETTINGS);
    settings_group.set_description(Some(bridges_strings::BRIDGE_SETTINGS_DESC));

    if settings.is_empty() {
        let placeholder = gtk::Label::new(Some(bridges_strings::NO_SETTINGS));
        placeholder.add_css_class("dim-label");
        placeholder.set_margin_top(8);
        placeholder.set_margin_bottom(8);
        settings_group.add(&placeholder);
        return settings_group;
    }

    for setting in settings {
        match setting_control(&setting.setting_type) {
            SettingControl::Toggle => {
                let row = adw::SwitchRow::builder()
                    .title(setting.label.as_str())
                    .build();
                crate::testid::set_test_id(&row, &setting_id(&setting.key));
                row.set_active(matches!(setting.value, Value::Bool(true)));
                let key = setting.key.clone();
                let commit = Rc::clone(&commit);
                row.connect_active_notify(move |sw| {
                    commit(&key, Value::Bool(sw.is_active()));
                });
                settings_group.add(&row);
            }
            SettingControl::Select => {
                let options = setting.options.clone().unwrap_or_default();
                let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
                let combo = adw::ComboRow::builder()
                    .title(setting.label.as_str())
                    .model(&gtk::StringList::new(&labels))
                    .build();
                crate::testid::set_test_id(&combo, &setting_id(&setting.key));
                if let Some(idx) = options.iter().position(|o| o.value == setting.value) {
                    combo.set_selected(idx as u32);
                }
                let key = setting.key.clone();
                let commit = Rc::clone(&commit);
                combo.connect_selected_notify(move |cb| {
                    if let Some(opt) = options.get(cb.selected() as usize) {
                        commit(&key, opt.value.clone());
                    }
                });
                settings_group.add(&combo);
            }
            SettingControl::Number => {
                let row = adw::ActionRow::builder()
                    .title(setting.label.as_str())
                    .build();
                let entry = gtk::Entry::builder().width_chars(10).build();
                crate::testid::set_test_id(&entry, &setting_id(&setting.key));
                if let Value::Integer(n) = setting.value {
                    entry.set_text(&n.to_string());
                }
                row.add_suffix(&entry);
                let key = setting.key.clone();
                let commit = Rc::clone(&commit);
                // Commit-on-Enter, never per-keystroke — the
                // `folder-member-cap-input` idiom (`views/devices_folders/
                // folders.rs`), this page's own analogue of a debounced
                // auto-save. Nest-side clamping (bridges.md § Bridge
                // settings) means the value shown here can lag the server's
                // clamped truth until this pane is next rebuilt — the same
                // limitation every other setting on this page already has,
                // since none of them repaint from a live snapshot today.
                entry.connect_activate(move |e| {
                    if let Some(n) = fauna_core::format::parse_count_i64(&e.text()) {
                        commit(&key, Value::Integer(n as i128));
                    }
                });
                settings_group.add(&row);
            }
            SettingControl::Text => {
                let row = adw::ActionRow::builder()
                    .title(setting.label.as_str())
                    .build();
                let entry = gtk::Entry::new();
                crate::testid::set_test_id(&entry, &setting_id(&setting.key));
                if let Value::String(s) = &setting.value {
                    entry.set_text(s);
                }
                row.add_suffix(&entry);
                let key = setting.key.clone();
                let commit = Rc::clone(&commit);
                entry.connect_activate(move |e| {
                    commit(&key, Value::String(e.text().to_string()));
                });
                settings_group.add(&row);
            }
            SettingControl::Unknown => {
                // Forward-compat fallback: a setting_type this build predates
                // (a newer nest) — a read-only row, never a dropped setting.
                // Mirrors tui's `Element::chrome` fallback / web's read-only
                // label fallback (`BridgeCard.svelte`).
                let row = adw::ActionRow::builder()
                    .title(setting.label.as_str())
                    .subtitle(setting_display(&setting.value).as_str())
                    .build();
                crate::testid::set_test_id(&row, &setting_id(&setting.key));
                crate::testid::set_test_text(
                    &row,
                    &format!("{}: {}", setting.label, setting_display(&setting.value)),
                );
                settings_group.add(&row);
            }
        }
    }

    settings_group
}

/// A setting row's id, `bridge-setting[<key>]` — keyed by the setting's own
/// key (the `settings-nav-row[<key>]` idiom), so a test addresses a setting by
/// what it is, never by its position in the provider's list.
fn setting_id(key: &str) -> String {
    format!("{}[{key}]", ids::BRIDGE_SETTING)
}

/// Plain-text rendering of a setting's scalar value, for the forward-compat
/// fallback row. `""` for a shape this function doesn't know (list/map/etc —
/// no provider emits one today).
fn setting_display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Integer(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Float(f) => f.to_string(),
        _ => String::new(),
    }
}

/// Commit a single bridge setting via `fauna.bridges.set_settings` — sends
/// only the one changed key. Thin wrapper over the shared
/// `settings::commit_bridge_settings` (which also backs
/// `settings/nostr_tab.rs`'s relay-list save and multi-key save).
fn commit_setting(client: &Rc<FaunaClient>, bridge_id: &str, key: &str, value: Value) {
    let settings = Value::Map(BTreeMap::from_iter([(key.to_string(), value)]));
    crate::settings::commit_bridge_settings(client, bridge_id, settings);
}

/// Populate the follows list inside a bridge detail pane with `follows`,
/// replacing any previously-rendered rows.
///
/// `follows` is the typed list carried by the `fauna.bridges.list_follows`
/// WS-RPC reply (delivered to the UI via `DataMessage::BridgeFollowsLoaded`).
/// `on_remove` is called with a follow's id when its row's remove button is
/// clicked — the caller supplies the `fauna.bridges.remove_follow` wiring
/// (bridge id + client), so this function needs neither and is directly
/// GTK-thread-testable with no `FaunaClient` (the `build_contact_row`
/// callback idiom, `views/contacts/list.rs`).
///
/// Two callers drive this: `app.rs`'s `DataMessage::
/// BridgeFollowsLoaded` handler, via the currently-open Bridges-page detail's
/// tracked [`BridgeDetailHandles::follows_list`]; and the ATProto settings
/// page's embedded panel, via `crate::settings::notify_bridge_follows_loaded`.
/// Before this wiring existed, `mod.rs`'s row activation fetched follows
/// (`client.fetch_bridge_follows`) but the reply handler only logged it and
/// never called this — so the follows list always showed its empty
/// placeholder (`NO_FOLLOWS_CONFIGURED`), never real data; same gap in
/// `settings/atproto.rs`, which never even called `fetch_bridge_follows` for
/// its embedded copy of this pane.
pub fn populate_follows_list(
    follows_list: &gtk::ListBox,
    follows: &[BridgeFollow],
    on_remove: impl Fn(&str) + Clone + 'static,
) {
    crate::views::layout::clear_list_box_rows(follows_list);

    for follow in follows {
        let display = follow_display(follow);
        let on_remove = on_remove.clone();
        let follow_id = follow.id.clone();
        let row = build_follow_row(&follow.id, &display, move || on_remove(&follow_id));
        follows_list.append(&row);
    }
}

thread_local! {
    /// The last follow-request list each bridge answered with. Opening a
    /// bridge's detail REBUILDS the pane (row activation), whose section starts
    /// hidden; painting this cache at once keeps the list on screen across the
    /// rebuild while the re-read is in flight, instead of blanking it.
    static FOLLOW_REQUESTS_CACHE: RefCell<HashMap<String, Vec<fauna_client_bridges::FollowRequestRow>>> =
        RefCell::new(HashMap::new());
}

/// Remember `rows` as `bridge_id`'s latest follow-request list.
pub fn cache_follow_requests(bridge_id: &str, rows: &[fauna_client_bridges::FollowRequestRow]) {
    FOLLOW_REQUESTS_CACHE.with(|c| {
        c.borrow_mut().insert(bridge_id.to_string(), rows.to_vec());
    });
}

/// Paint `bridge_id`'s cached follow requests into a freshly built pane, if the
/// bridge has answered before this session (otherwise the section stays hidden
/// until its first reply).
pub fn paint_cached_follow_requests(
    handles: &FollowRequestsHandles,
    bridge_id: &str,
    on_resolve: impl Fn(&str, bool) + Clone + 'static,
) {
    if let Some(rows) = FOLLOW_REQUESTS_CACHE.with(|c| c.borrow().get(bridge_id).cloned()) {
        populate_follow_requests(handles, &rows, on_resolve);
    }
}

/// Paint a bridge's waiting follow requests (`BridgeDetailHandles::
/// follow_requests`), replacing the rows: the section shows, its heading
/// carries the count (kept at zero), and each row is the shared
/// `FollowRequestRow::display` line with **Approve** and **Refuse**.
/// `on_resolve` is called with the request's own `id` and `approve`; the caller
/// supplies the resolve-then-re-read wiring (no confirm, non-optimistic).
pub fn populate_follow_requests(
    handles: &FollowRequestsHandles,
    rows: &[fauna_client_bridges::FollowRequestRow],
    on_resolve: impl Fn(&str, bool) + Clone + 'static,
) {
    handles.section.set_visible(true);
    handles
        .heading
        .set_label(&bridges_strings::follow_requests(&rows.len().to_string()));
    crate::views::layout::clear_list_box_rows(&handles.list);
    for request in rows {
        handles
            .list
            .append(&build_follow_request_row(request, on_resolve.clone()));
    }
}

fn build_follow_request_row(
    request: &fauna_client_bridges::FollowRequestRow,
    on_resolve: impl Fn(&str, bool) + Clone + 'static,
) -> gtk::ListBoxRow {
    let label = gtk::Label::new(Some(&request.display()));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);

    let approve_btn = gtk::Button::with_label(bridges_strings::APPROVE_FOLLOW_REQUEST);
    approve_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&approve_btn, ids::BRIDGE_FOLLOW_REQUEST_APPROVE);
    crate::offline_gate::declare_wire_kind(&approve_btn, "fauna.bridges.resolve_follow_request");
    let refuse_btn = gtk::Button::with_label(bridges_strings::REFUSE_FOLLOW_REQUEST);
    refuse_btn.add_css_class("destructive-action");
    crate::testid::set_test_id(&refuse_btn, ids::BRIDGE_FOLLOW_REQUEST_REFUSE);
    crate::offline_gate::declare_wire_kind(&refuse_btn, "fauna.bridges.resolve_follow_request");

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    // The item id sits on the display label itself, so a row's read-back text is
    // the shared display line (the follow row's marker is empty; this one is
    // what the witness test reads the address and name from).
    crate::testid::set_test_id(&label, ids::BRIDGE_FOLLOW_REQUEST_ITEM);
    hbox.append(&label);
    hbox.append(&approve_btn);
    hbox.append(&refuse_btn);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row.set_widget_name(&request.id);

    {
        let id = request.id.clone();
        let on_resolve = on_resolve.clone();
        approve_btn.connect_clicked(move |_| on_resolve(&id, true));
    }
    {
        let id = request.id.clone();
        refuse_btn.connect_clicked(move |_| on_resolve(&id, false));
    }

    row
}

fn build_follow_row(
    follow_id: &str,
    display: &str,
    on_remove: impl Fn() + 'static,
) -> gtk::ListBoxRow {
    let label = gtk::Label::new(Some(display));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);

    let remove_btn = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .tooltip_text(bridges_strings::REMOVE_FOLLOW)
        .build();
    remove_btn.add_css_class("flat");
    crate::testid::set_test_id(&remove_btn, ids::BRIDGE_FOLLOW_REMOVE);
    crate::offline_gate::declare_wire_kind(&remove_btn, "fauna.bridges.remove_follow");

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    // The item id sits on the display label itself, so a row's read-back text
    // is the shared display line (the nickname, else the id).
    crate::testid::set_test_id(&label, ids::BRIDGE_FOLLOW_ITEM);
    hbox.append(&label);
    hbox.append(&remove_btn);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row.set_widget_name(follow_id);

    remove_btn.connect_clicked(move |_| on_remove());

    row
}

/// Build the metadata-driven link dialog for a bridge.
///
/// Renders entirely from the provider-declared `BridgeLinkMode` metadata
/// (`fauna.bridges.list` → `BridgeStatus.link_modes`): a mode selector when the
/// provider offers more than one mode, and one `bridge-link-field-{key}` input
/// per `BridgeLinkField` the selected mode declares (a `PasswordEntry` for a
/// `secret` field, a plain `Entry` otherwise). Submitting dispatches
/// `fauna.bridges.link` with the chosen `mode` + a `{key: value}` params map;
/// for an `oauth_redirect`-style mode the reply's `redirect_url` is opened in
/// the browser by `FaunaClient::link_bridge`. No per-bridge render logic — the
/// old hard-coded per-provider dialogs are dropped (bridges.md § Layout & flow).
fn build_link_dialog(
    bridge_id: &str,
    link_modes: &[BridgeLinkMode],
    client: Rc<FaunaClient>,
) -> adw::Window {
    let dialog = adw::Window::builder()
        .title(bridges_strings::LINK_BRIDGE)
        .modal(true)
        .default_width(380)
        .build();

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    // ui.yaml's `bridge-link-form` component (bridges.md § Element IDs) — a
    // scope anchor so a test can disambiguate this dialog's own
    // `bridge-action-button` (the submit action) from the bridge-card's
    // trigger button of the same id, which stays in the tree (behind the
    // modal) while this dialog is open.
    crate::testid::set_test_id(&vbox, ids::BRIDGE_LINK_FORM);

    let modes: Vec<BridgeLinkMode> = platform_applicable(link_modes);

    // Active field inputs for the currently-selected mode, shared between the
    // mode-switch rebuild and the submit handler. Index into `modes`.
    let field_inputs: Rc<RefCell<Vec<(String, gtk::Editable)>>> = Rc::new(RefCell::new(Vec::new()));
    let active_mode: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));

    // Container the per-mode fields are (re)rendered into.
    let fields_box = gtk::Box::new(gtk::Orientation::Vertical, 8);

    // Mode selector — only shown when the provider offers more than one mode.
    if modes.len() > 1 {
        let mode_label = gtk::Label::new(Some(common::MODE));
        mode_label.set_halign(gtk::Align::Start);
        let labels: Vec<&str> = modes.iter().map(|m| m.label.as_str()).collect();
        let mode_dropdown = gtk::DropDown::from_strings(&labels);
        mode_dropdown.set_selected(0);
        {
            let fb = fields_box.clone();
            let fi = Rc::clone(&field_inputs);
            let am = Rc::clone(&active_mode);
            let modes = modes.clone();
            mode_dropdown.connect_selected_notify(move |dd| {
                let idx = dd.selected() as usize;
                *am.borrow_mut() = idx;
                render_mode_fields(&fb, modes.get(idx), &fi);
            });
        }
        vbox.append(&mode_label);
        vbox.append(&mode_dropdown);
    }

    vbox.append(&fields_box);

    // Initial fields for the first (or only) mode.
    render_mode_fields(&fields_box, modes.first(), &field_inputs);

    let link_btn = gtk::Button::with_label(bridges_strings::LINK_ACTION);
    link_btn.add_css_class("suggested-action");
    link_btn.set_sensitive(!modes.is_empty());
    // Same id as the bridge-card's trigger button (bridges.md § User actions:
    // "bridge-action-button (unlinked) — Link via the selected mode") — this
    // IS that action, just realized as this dialog's submit step. Scope by
    // `bridge-link-form` to reach this instance specifically.
    crate::testid::set_test_id(&link_btn, ids::BRIDGE_ACTION_BUTTON);
    // This IS the fauna.bridges.link ceremony's submit step — the card-level
    // trigger that opened this dialog only navigates here (module docs on
    // that button; the `file-version-restore-button` precedent in
    // media/detail.rs), so it stays undeclared and this one carries the kind.
    crate::offline_gate::declare_wire_kind(&link_btn, "fauna.bridges.link");
    vbox.append(&link_btn);

    dialog.set_content(Some(&vbox));

    {
        let bid = bridge_id.to_string();
        let d = dialog.clone();
        let fi = Rc::clone(&field_inputs);
        let am = Rc::clone(&active_mode);
        let modes = modes.clone();
        let c = Rc::clone(&client);
        link_btn.connect_clicked(move |_| {
            let idx = *am.borrow();
            let mode = match modes.get(idx) {
                Some(m) => m.mode.clone(),
                None => return,
            };
            let mut entries: Vec<(String, Value)> = Vec::new();
            for (key, editable) in fi.borrow().iter() {
                let val = editable.text().to_string();
                if !val.is_empty() {
                    entries.push(cbor_text_entry(key, val));
                }
            }
            c.link_bridge(&bid, &mode, Value::Map(BTreeMap::from_iter(entries)));
            d.close();
        });
    }

    dialog
}

/// (Re)render the fields for one link mode into `container`, replacing any
/// previously-rendered fields and refreshing `field_inputs` to the new set.
/// Each field becomes a labelled `bridge-link-field-{key}` input — a
/// `PasswordEntry` for a `secret` field, a plain `Entry` otherwise.
fn render_mode_fields(
    container: &gtk::Box,
    mode: Option<&BridgeLinkMode>,
    field_inputs: &Rc<RefCell<Vec<(String, gtk::Editable)>>>,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    field_inputs.borrow_mut().clear();

    let mode = match mode {
        Some(m) => m,
        None => return,
    };

    for field in &mode.fields {
        let label = gtk::Label::new(Some(&field.label));
        label.set_halign(gtk::Align::Start);
        container.append(&label);

        let testid = format!("bridge-link-field-{}", field.key);
        let editable: gtk::Editable = if field.field_type == "secret" {
            let entry = gtk::PasswordEntry::new();
            entry.set_show_peek_icon(true);
            if let Some(ph) = &field.placeholder {
                entry.set_placeholder_text(Some(ph));
            }
            crate::testid::set_test_id(&entry, &testid);
            container.append(&entry);
            entry.upcast()
        } else {
            let entry = gtk::Entry::new();
            if let Some(ph) = &field.placeholder {
                entry.set_placeholder_text(Some(ph));
            }
            crate::testid::set_test_id(&entry, &testid);
            container.append(&entry);
            entry.upcast()
        };
        field_inputs
            .borrow_mut()
            .push((field.key.clone(), editable));
    }
}

/// The follow add row: `bridge-follow-id-input`, `bridge-follow-petname-input`
/// and `bridge-add-follow-button`. The button sends `fauna.bridges.add_follow`
/// with the typed id (and the nickname when typed), then clears both fields; an
/// empty id sends nothing.
fn build_add_follow_row(bridge_id: &str, client: Rc<FaunaClient>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_top(8);

    let id_entry = gtk::Entry::new();
    id_entry.set_hexpand(true);
    id_entry.set_placeholder_text(Some(bridges_strings::ID_TO_FOLLOW_PLACEHOLDER));
    crate::testid::set_test_id(&id_entry, ids::BRIDGE_FOLLOW_ID_INPUT);

    let petname_entry = gtk::Entry::new();
    petname_entry.set_placeholder_text(Some(bridges_strings::FRIENDLY_NAME_PLACEHOLDER));
    crate::testid::set_test_id(&petname_entry, ids::BRIDGE_FOLLOW_PETNAME_INPUT);

    let add_btn = gtk::Button::with_label(bridges_strings::ADD_FOLLOW);
    add_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&add_btn, ids::BRIDGE_ADD_FOLLOW_BUTTON);
    crate::offline_gate::declare_wire_kind(&add_btn, "fauna.bridges.add_follow");

    row.append(&id_entry);
    row.append(&petname_entry);
    row.append(&add_btn);

    let bid = bridge_id.to_string();
    add_btn.connect_clicked(move |_| {
        let id = id_entry.text().to_string();
        if id.is_empty() {
            return;
        }
        let petname_text = petname_entry.text().to_string();
        let petname = (!petname_text.is_empty()).then_some(petname_text.as_str());
        client.add_bridge_follow(&bid, &id, petname);
        id_entry.set_text("");
        petname_entry.set_text("");
    });

    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::find_by_test_id;
    use fauna_client_bridges::bridges_ui::{BridgeLinkField, BridgeLinkMode};

    fn field(key: &str, ftype: &str) -> BridgeLinkField {
        BridgeLinkField {
            key: key.to_string(),
            label: format!("{key} label"),
            field_type: ftype.to_string(),
            placeholder: Some(format!("{key}…")),
            extra: BTreeMap::new(),
        }
    }

    fn mode(mode: &str, fields: Vec<BridgeLinkField>) -> BridgeLinkMode {
        BridgeLinkMode {
            mode: mode.to_string(),
            label: mode.to_string(),
            client_action: None,
            platform: None,
            fields,
            extra: BTreeMap::new(),
        }
    }

    /// Each declared field renders as a `bridge-link-field-{key}` input — a plain
    /// Entry for a `text` field, a PasswordEntry for a `secret` field — and is
    /// recorded in `field_inputs` keyed by `BridgeLinkField.key`
    /// (bridges.md § Element IDs: the metadata-driven link form).
    #[test]
    fn renders_bridge_link_field_per_metadata_field() {
        crate::testid::run_on_gtk_thread(|| {
            let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let inputs: Rc<RefCell<Vec<(String, gtk::Editable)>>> =
                Rc::new(RefCell::new(Vec::new()));
            let m = mode(
                "oauth",
                vec![field("handle", "text"), field("app_password", "secret")],
            );

            render_mode_fields(&container, Some(&m), &inputs);

            let handle = find_by_test_id(&container, "bridge-link-field-handle")
                .expect("bridge-link-field-handle present");
            assert!(
                handle.downcast_ref::<gtk::Entry>().is_some(),
                "text field renders as a plain Entry",
            );
            let secret = find_by_test_id(&container, "bridge-link-field-app_password")
                .expect("bridge-link-field-app_password present");
            assert!(
                secret.downcast_ref::<gtk::PasswordEntry>().is_some(),
                "secret field renders as a PasswordEntry",
            );

            let keys: Vec<String> = inputs.borrow().iter().map(|(k, _)| k.clone()).collect();
            assert_eq!(keys, vec!["handle".to_string(), "app_password".to_string()]);
        });
    }

    /// A fieldless mode (e.g. ActivityPub `enable`) renders no input widgets and
    /// records no field inputs — the form is just the submit action.
    #[test]
    fn fieldless_mode_renders_no_inputs() {
        crate::testid::run_on_gtk_thread(|| {
            let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let inputs: Rc<RefCell<Vec<(String, gtk::Editable)>>> =
                Rc::new(RefCell::new(Vec::new()));
            let m = mode("enable", vec![]);
            render_mode_fields(&container, Some(&m), &inputs);
            assert!(
                container.first_child().is_none(),
                "no field widgets for a fieldless mode",
            );
            assert!(inputs.borrow().is_empty());
        });
    }

    /// The forward-compat fallback row's display text — the one piece of
    /// `build_settings_group` with no GTK/client dependency, so it is unit-
    /// tested directly rather than through a widget tree (no test in this
    /// crate constructs a real `FaunaClient`, which every other row-kind
    /// arm needs for its commit closure).
    #[test]
    fn setting_display_renders_every_scalar_shape() {
        assert_eq!(setting_display(&Value::String("x".to_string())), "x");
        assert_eq!(setting_display(&Value::Integer(1000)), "1000");
        assert_eq!(setting_display(&Value::Bool(true)), "true");
        assert_eq!(setting_display(&Value::List(vec![])), "");
    }

    fn follow(id: &str, petname: Option<&str>) -> BridgeFollow {
        BridgeFollow {
            id: id.to_string(),
            petname: petname.map(str::to_string),
            created_at: None,
            extra: None,
            unknown_keys: Default::default(),
        }
    }

    /// The first hbox child inside a follow row's content is the display
    /// `Label` (`label`, then `remove_btn` — the order `build_follow_row`
    /// appends them in).
    fn row_display_label(row: &gtk::Widget) -> String {
        let hbox = row.first_child().expect("row has content");
        let label = hbox.first_child().expect("hbox has the display label");
        label
            .downcast::<gtk::Label>()
            .expect("first hbox child is the display Label")
            .text()
            .to_string()
    }

    /// `populate_follows_list` renders exactly one row per follow, in the
    /// order given, each carrying its follow id as `widget_name` and its
    /// resolved display text — the core render fix. Before it,
    /// `app.rs`'s `BridgeFollowsLoaded` handler never called this at all, so
    /// a bridge with real follows always showed the empty
    /// `NO_FOLLOWS_CONFIGURED` placeholder no matter what the nest returned.
    #[test]
    fn populate_follows_list_renders_one_row_per_follow_in_order() {
        crate::testid::run_on_gtk_thread(|| {
            let list = gtk::ListBox::new();
            let follows = vec![
                follow("alice", Some("Alice")),
                follow("bob", None),
                follow("carol", Some("  ")),
            ];
            populate_follows_list(&list, &follows, |_| {});

            let mut rows = Vec::new();
            let mut child = list.first_child();
            while let Some(row) = child {
                rows.push((row.widget_name().to_string(), row_display_label(&row)));
                child = row.next_sibling();
            }
            assert_eq!(
                rows,
                vec![
                    ("alice".to_string(), "Alice".to_string()),
                    ("bob".to_string(), "bob".to_string()),
                    ("carol".to_string(), "carol".to_string()),
                ]
            );
        });
    }

    /// A re-population (a second `fauna.bridges.list_follows` reply, e.g.
    /// after an add/remove) fully replaces the previous rows rather than
    /// appending to them.
    #[test]
    fn populate_follows_list_clears_previous_rows_on_repopulate() {
        crate::testid::run_on_gtk_thread(|| {
            let list = gtk::ListBox::new();
            populate_follows_list(&list, &[follow("alice", None)], |_| {});
            populate_follows_list(&list, &[follow("bob", None), follow("carol", None)], |_| {});

            let mut names = Vec::new();
            let mut child = list.first_child();
            while let Some(row) = child {
                names.push(row.widget_name().to_string());
                child = row.next_sibling();
            }
            assert_eq!(names, vec!["bob".to_string(), "carol".to_string()]);
        });
    }

    /// Each row's remove button is wired to `on_remove` with THAT row's own
    /// follow id — not the last one populated, and not empty (the shape a
    /// half-wired per-iteration closure capture would silently produce).
    #[test]
    fn populate_follows_list_wires_each_remove_button_to_its_own_follow_id() {
        crate::testid::run_on_gtk_thread(|| {
            let list = gtk::ListBox::new();
            let removed: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
            let r = Rc::clone(&removed);
            populate_follows_list(
                &list,
                &[follow("alice", None), follow("bob", None)],
                move |id| r.borrow_mut().push(id.to_string()),
            );

            let mut child = list.first_child();
            while let Some(row) = child {
                let remove_btn = find_by_test_id(&row, ids::BRIDGE_FOLLOW_REMOVE)
                    .and_then(|w| w.downcast::<gtk::Button>().ok())
                    .expect("each follow row exposes bridge-follow-remove");
                remove_btn.emit_clicked();
                child = row.next_sibling();
            }

            assert_eq!(
                *removed.borrow(),
                vec!["alice".to_string(), "bob".to_string()]
            );
        });
    }

    fn request(id: &str, name: Option<&str>) -> fauna_client_bridges::FollowRequestRow {
        fauna_client_bridges::FollowRequestRow {
            id: id.to_string(),
            address: format!("@{id}@remote.example"),
            name: name.map(str::to_string),
        }
    }

    fn requests_handles() -> FollowRequestsHandles {
        FollowRequestsHandles {
            section: gtk::Box::new(gtk::Orientation::Vertical, 0),
            heading: gtk::Label::new(None),
            list: gtk::ListBox::new(),
        }
    }

    /// The section is hidden until a reply paints it; an empty reply shows the
    /// heading at a zero count with no rows (`bridges.md` § Follow requests), and
    /// a populated one paints a counted heading, one row per request carrying the
    /// shared display line, and Approve / Refuse wired to THAT row's own id.
    #[test]
    fn populate_follow_requests_paints_a_counted_heading_and_wires_both_buttons() {
        crate::testid::run_on_gtk_thread(|| {
            let h = requests_handles();
            h.section.set_visible(false);

            populate_follow_requests(&h, &[], |_, _| {});
            assert!(h.section.is_visible());
            assert_eq!(h.heading.label(), "Follow requests (0)");
            assert!(h.list.first_child().is_none());

            let answered: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
            let a = Rc::clone(&answered);
            populate_follow_requests(
                &h,
                &[request("bob", Some("Bob")), request("cy", None)],
                move |id, approve| a.borrow_mut().push((id.to_string(), approve)),
            );
            assert_eq!(h.heading.label(), "Follow requests (2)");

            let mut lines = Vec::new();
            let mut child = h.list.first_child();
            while let Some(row) = child {
                lines.push(
                    find_by_test_id(&row, ids::BRIDGE_FOLLOW_REQUEST_ITEM)
                        .and_then(|w| w.downcast::<gtk::Label>().ok())
                        .expect("each request row exposes its item label")
                        .text()
                        .to_string(),
                );
                for (test_id, _) in [
                    (ids::BRIDGE_FOLLOW_REQUEST_APPROVE, true),
                    (ids::BRIDGE_FOLLOW_REQUEST_REFUSE, false),
                ] {
                    find_by_test_id(&row, test_id)
                        .and_then(|w| w.downcast::<gtk::Button>().ok())
                        .expect("each request row exposes its button")
                        .emit_clicked();
                }
                child = row.next_sibling();
            }
            assert_eq!(
                lines,
                vec![
                    "Bob (@bob@remote.example)".to_string(),
                    "@cy@remote.example".to_string()
                ]
            );
            assert_eq!(
                *answered.borrow(),
                vec![
                    ("bob".to_string(), true),
                    ("bob".to_string(), false),
                    ("cy".to_string(), true),
                    ("cy".to_string(), false),
                ]
            );
        });
    }

    // ── Ward-side feed-source asks (family-safety.md § Feed-source approvals) ──
    //
    // Mirrors tui's `bridges.rs` arms. A test that only asserted "a row
    // appeared" would pass against a surface that painted the button
    // unconditionally — the failure the gate exists to prevent — so each arm
    // pins the text or the count.

    fn feed_ask(
        op: &str,
        target: &str,
        approved: bool,
    ) -> fauna_client_family::FamilyFeedRequestInfo {
        fauna_client_family::FamilyFeedRequestInfo {
            bridge_id: "activitypub".to_string(),
            operation: op.to_string(),
            target: target.to_string(),
            label: String::new(),
            created_at: 1,
            approved_at: approved.then_some(2),
            extra: Default::default(),
        }
    }

    /// `(state texts, button count)` showing in a filled container.
    fn source_rows(container: &gtk::Box) -> (Vec<String>, usize) {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(container);
        let r: &gtk::Widget = root.upcast_ref();
        let mut states = Vec::new();
        crate::automation::find::collect_in(r, ids::BRIDGE_SOURCE_REQUEST_STATE, &mut states);
        let texts = states
            .into_iter()
            .map(|w| {
                w.downcast::<gtk::Label>()
                    .expect("a label")
                    .text()
                    .to_string()
            })
            .collect();
        let buttons = crate::automation::find::count_in(r, ids::BRIDGE_SOURCE_REQUEST_BUTTON);
        root.remove(container);
        (texts, buttons)
    }

    /// An account that never hit the gate shows NEITHER element — which is
    /// also the unsupervised case, since both inputs are supervised-only.
    #[test]
    fn no_refusal_and_no_ask_renders_no_source_elements() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            let c = gtk::Box::new(gtk::Orientation::Vertical, 0);
            fill_source_asks(&c, "activitypub", |_, _, _| {});
            assert_eq!(source_rows(&c), (Vec::new(), 0));
            assert!(!c.is_visible(), "nothing to show hides the section");
        });
    }

    /// A typed refusal offers the ask for the refused triple only, and the
    /// button carries THAT triple.
    #[test]
    fn a_guardian_refusal_offers_the_ask_button_for_its_triple() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            crate::ward_asks::note_feed_refusal("activitypub", "follow", "npub1abc");
            crate::ward_asks::note_feed_refusal("other", "link", "");
            let asked: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
            let c = gtk::Box::new(gtk::Orientation::Vertical, 0);
            {
                let asked = Rc::clone(&asked);
                fill_source_asks(&c, "activitypub", move |b, o, t| {
                    asked.borrow_mut().push((b, o, t))
                });
            }
            assert_eq!(
                source_rows(&c),
                (Vec::new(), 1),
                "one refused follow, one ask"
            );
            find_by_test_id(&c, ids::BRIDGE_SOURCE_REQUEST_BUTTON)
                .and_then(|w| w.downcast::<gtk::Button>().ok())
                .expect("the ask button")
                .emit_clicked();
            assert_eq!(
                *asked.borrow(),
                vec![("activitypub".into(), "follow".into(), "npub1abc".into())]
            );
            crate::ward_asks::clear_for_identity_change();
        });
    }

    /// The durable list wins over the session refusal: a landed ask shows its
    /// verdict instead of offering the button again.
    #[test]
    fn a_landed_ask_replaces_the_button_with_its_state() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            crate::ward_asks::note_feed_refusal("activitypub", "follow", "npub1abc");
            crate::ward_asks::set_from_status(
                true,
                None,
                Vec::new(),
                vec![feed_ask("follow", "npub1abc", false)],
            );
            let c = gtk::Box::new(gtk::Orientation::Vertical, 0);
            fill_source_asks(&c, "activitypub", |_, _, _| {});
            assert_eq!(
                source_rows(&c),
                (vec![bridges_strings::SOURCE_REQUEST_PENDING.to_string()], 0)
            );
            crate::ward_asks::clear_for_identity_change();
        });
    }

    /// ⚠ An APPROVED grant prompts the retry and is a LABEL, never a button:
    /// rendering it as an affordance is the first step to redeeming a
    /// single-use grant on paint (rule (e)).
    #[test]
    fn an_approved_ask_prompts_the_retry_and_never_auto_retries() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            crate::ward_asks::set_from_status(
                true,
                None,
                Vec::new(),
                vec![feed_ask("follow", "npub1abc", true)],
            );
            let c = gtk::Box::new(gtk::Orientation::Vertical, 0);
            fill_source_asks(&c, "activitypub", |_, _, _| {
                panic!("an approved grant must never issue anything on paint")
            });
            assert_eq!(
                source_rows(&c),
                (
                    vec![bridges_strings::SOURCE_REQUEST_APPROVED.to_string()],
                    0
                )
            );
            crate::ward_asks::clear_for_identity_change();
        });
    }
}
