use adw::prelude::*;
use fauna_ui_ids as ids;
use gtk::glib;
use std::cell::RefCell;
use std::sync::Arc;

use crate::i18n::strings::{common, errors, settings::p2p_page};

// ---------------------------------------------------------------------------
// Thread-local widget references updated by the polling timer
// ---------------------------------------------------------------------------

thread_local! {
    static TUNNEL_STATUS_ROW: RefCell<Option<adw::ActionRow>> = const { RefCell::new(None) };
    static NODE_ID_ROW: RefCell<Option<adw::ActionRow>> = const { RefCell::new(None) };
    static TOGGLE_BTN: RefCell<Option<gtk::Button>> = const { RefCell::new(None) };
    static NODE_ID_COPY_BTN: RefCell<Option<gtk::Button>> = const { RefCell::new(None) };
    static CONTACTS_GROUP: RefCell<Option<adw::PreferencesGroup>> = const { RefCell::new(None) };
    static CONTACTS_ROWS: RefCell<Vec<gtk::Widget>> = const { RefCell::new(Vec::new()) };
}

/// Build the "P2P" preferences page with tunnel controls and the contact list.
pub fn build_p2p_page() -> gtk::Box {
    let page = adw::PreferencesPage::builder()
        .title(p2p_page::TITLE)
        .icon_name("network-workgroup-symbolic")
        .build();
    crate::testid::set_test_id(&page, ids::P2P_TAB);

    // -----------------------------------------------------------------------
    // Group 1: P2P node (iroh)
    // -----------------------------------------------------------------------
    let tunnel_group = adw::PreferencesGroup::builder()
        .title(p2p_page::TUNNEL_GROUP_TITLE)
        .description(p2p_page::TUNNEL_GROUP_DESCRIPTION)
        .build();

    // error-message — page-level error label (convention 2), hidden until a
    // failed start sets it. Built hidden + cleared on success per convention
    // 2's 2026-08-04 rider: `is_showing` (automation/find.rs) prunes a
    // `set_visible(false)` widget, so the negative "no error" assertion can
    // actually fail.
    let error_label = gtk::Label::builder().visible(false).build();
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    tunnel_group.add(&error_row);

    let status_row = adw::ActionRow::builder()
        .title(common::STATUS)
        .subtitle(common::INACTIVE)
        .build();
    tunnel_group.add(&status_row);

    let toggle_btn = gtk::Button::builder()
        .label(p2p_page::START)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    crate::testid::set_test_id(&toggle_btn, ids::P2P_TUNNEL_TOGGLE);
    // NO offline-gate declaration, deliberately: this toggle builds a LOCAL
    // iroh endpoint and hosts a `PeerNode` on it (`p2p.rs::start_tunnel`) —
    // nest-free by construction, so gating it would grey the tunnel in exactly
    // the situation it exists for (`p2p.md` § Offline share initiation, the
    // same rule that keeps the offline-share cluster undeclared). Re-verify
    // `start_tunnel` before ever adding one. (It was renamed off the WG-era
    // `wg-register-button` id 2026-08-23; there is no nest registration.)

    let toggle_row = adw::ActionRow::builder()
        .title(p2p_page::TUNNEL_ROW_TITLE)
        .subtitle(p2p_page::TUNNEL_ROW_SUBTITLE)
        .activatable(true)
        .build();
    toggle_row.add_suffix(&toggle_btn);
    tunnel_group.add(&toggle_row);

    // This device's iroh node id (shown when active). The WG-era `Tunnel IP`
    // label went with the stack 2026-08-23 — `tunnel_info()` returns the node
    // identity, which is what the renamed `p2p-node-id-copy-btn` copies.
    let node_id_row = adw::ActionRow::builder()
        .title(p2p_page::NODE_ID)
        .subtitle("—")
        .subtitle_selectable(true)
        .build();
    {
        let row_ref = node_id_row.clone();
        let ip_copy = gtk::Button::with_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
        ip_copy.add_css_class("flat");
        crate::testid::set_test_id(&ip_copy, ids::P2P_NODE_ID_COPY_BTN);
        ip_copy.connect_clicked(move |btn| {
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
        node_id_row.add_suffix(&ip_copy);
        NODE_ID_COPY_BTN.with(|c| *c.borrow_mut() = Some(ip_copy));
    }
    tunnel_group.add(&node_id_row);

    page.add(&tunnel_group);

    // Wire the toggle button
    {
        let status_row_ref = status_row.clone();
        let id_row_ref = node_id_row.clone();
        let btn_ref = toggle_btn.clone();
        let error_label_ref = error_label.clone();
        toggle_btn.connect_clicked(move |_| {
            let Some(p2p) = super::get_p2p() else {
                tracing::error!("[settings/p2p] toggle: no P2pService available");
                return;
            };

            // A stale error from a previous failed attempt must not survive
            // into a new one (convention 2's rider — the read must see the
            // real, current state, not a leftover).
            error_label_ref.set_text("");
            error_label_ref.set_visible(false);

            if p2p.is_tunnel_active() {
                // Stop
                p2p.stop_tunnel();
                status_row_ref.set_subtitle(common::INACTIVE);
                id_row_ref.set_subtitle("—");
                btn_ref.set_label(p2p_page::START);
                btn_ref.remove_css_class("destructive-action");
                btn_ref.add_css_class("suggested-action");
                // A prior "Copied!" click's 2s revert timer (below) may still
                // be pending — force the label back now rather than leaving a
                // stale timer to fire during whatever comes next (found live
                // 2026-08-25: it raced test_tunnel_start_stop_round_trip's
                // immediately-following no-op check). The timer firing later
                // just re-sets the same value, so this needs no cancellation.
                NODE_ID_COPY_BTN.with(|c| {
                    if let Some(btn) = c.borrow().as_ref() {
                        btn.set_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
                    }
                });
            } else {
                // Start (async)
                let sr = status_row_ref.clone();
                let ir = id_row_ref.clone();
                let br = btn_ref.clone();
                let er = error_label_ref.clone();
                sr.set_subtitle(common::STARTING);
                br.set_sensitive(false);

                let p2p_clone = Arc::clone(&p2p);
                glib::MainContext::default().spawn_local(async move {
                    // `start_tunnel` is synchronous — it builds the iroh transport
                    // + node on the app's long-lived runtime (the node's accept
                    // loop must outlive this call), so there is no per-call
                    // throwaway runtime here.
                    let result = p2p_clone.start_tunnel();

                    match result {
                        Ok(()) => {
                            sr.set_subtitle(common::ACTIVE);
                            br.set_label(p2p_page::STOP);
                            br.remove_css_class("suggested-action");
                            br.add_css_class("destructive-action");
                            if let Some(node_id) = p2p_clone.tunnel_info() {
                                ir.set_subtitle(&node_id);
                            }
                        }
                        Err(e) => {
                            tracing::error!("[settings/p2p] start_tunnel failed: {e}");
                            sr.set_subtitle(common::INACTIVE);
                            er.set_text(&errors::http_error(&e.to_string()));
                            er.set_visible(true);
                        }
                    }
                    br.set_sensitive(true);
                });
            }
        });
    }

    // Stash widget refs for periodic updates
    TUNNEL_STATUS_ROW.with(|c| *c.borrow_mut() = Some(status_row.clone()));
    NODE_ID_ROW.with(|c| *c.borrow_mut() = Some(node_id_row.clone()));
    TOGGLE_BTN.with(|c| *c.borrow_mut() = Some(toggle_btn.clone()));

    // Poll tunnel status every 3 seconds to keep the UI in sync
    glib::timeout_add_local(std::time::Duration::from_secs(3), || {
        refresh_tunnel_status();
        glib::ControlFlow::Continue
    });

    // -----------------------------------------------------------------------
    // Group 2b: Contacts (the removal affordance — apps row 128, p2p.md
    // § Implementation status today "Peer-contact removal is written three
    // times and called zero times"). `PeerDb::delete_contact` was written,
    // tested, and exported over FFI, but no app rendered a list to hang a
    // removal button on — the since-retired Accept-Invite dialog could ADD a
    // contact with no way to see or remove it afterward. Local-only removal
    // (`P2pService::remove_contact` → the survivor `delete_contact`) — the
    // superseded cross-device `revoke_contact` rail is gone (deleted
    // 2026-08-25, p2p.md § Implementation status today).
    // -----------------------------------------------------------------------
    let contacts_group = adw::PreferencesGroup::builder()
        .title(p2p_page::CONTACTS_GROUP_TITLE)
        .description(p2p_page::CONTACTS_GROUP_DESCRIPTION)
        .build();
    page.add(&contacts_group);
    CONTACTS_GROUP.with(|c| *c.borrow_mut() = Some(contacts_group.clone()));
    refresh_contacts_list();

    // -----------------------------------------------------------------------
    // Group 3: Connection
    // -----------------------------------------------------------------------
    let connection_group = adw::PreferencesGroup::builder()
        .title(p2p_page::CONNECTION_GROUP_TITLE)
        .description(p2p_page::CONNECTION_GROUP_DESCRIPTION)
        .build();

    // No STUN row: the nest's STUN server was deleted 2026-08-23 with the
    // WireGuard stack, and iroh does its own hole-punching. LAN addresses are
    // the one thing left worth surfacing here.
    let lan_row = adw::ActionRow::builder()
        .title(p2p_page::LAN_ADDRESSES)
        .subtitle(get_lan_addresses())
        .subtitle_selectable(true)
        .build();
    {
        let row_ref = lan_row.clone();
        let lan_copy = gtk::Button::with_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
        lan_copy.add_css_class("flat");
        crate::testid::set_test_id(&lan_copy, ids::P2P_LAN_COPY_BTN);
        lan_copy.connect_clicked(move |btn| {
            let subtitle = row_ref
                .subtitle()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if subtitle != p2p_page::LAN_NONE {
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
        lan_row.add_suffix(&lan_copy);
    }
    connection_group.add(&lan_row);

    page.add(&connection_group);

    // Set initial state if tunnel is already active
    if let Some(p2p) = super::get_p2p()
        && p2p.is_tunnel_active()
    {
        status_row.set_subtitle(common::ACTIVE);
        toggle_btn.set_label(p2p_page::STOP);
        toggle_btn.remove_css_class("suggested-action");
        toggle_btn.add_css_class("destructive-action");
        if let Some(node_id) = p2p.tunnel_info() {
            node_id_row.set_subtitle(&node_id);
        }
    }

    crate::testid::wrap_page_with_heading(p2p_page::TITLE, ids::PAGE_HEADING, &page)
}

/// Refresh tunnel status rows from the current P2pService state.
fn refresh_tunnel_status() {
    let Some(p2p) = super::get_p2p() else {
        return;
    };

    let active = p2p.is_tunnel_active();

    TUNNEL_STATUS_ROW.with(|c| {
        if let Some(row) = c.borrow().as_ref() {
            row.set_subtitle(if active {
                common::ACTIVE
            } else {
                common::INACTIVE
            });
        }
    });

    TOGGLE_BTN.with(|c| {
        if let Some(btn) = c.borrow().as_ref() {
            if active {
                btn.set_label(p2p_page::STOP);
                btn.remove_css_class("suggested-action");
                btn.add_css_class("destructive-action");
            } else {
                btn.set_label(p2p_page::START);
                btn.remove_css_class("destructive-action");
                btn.add_css_class("suggested-action");
            }
        }
    });

    if active {
        if let Some(node_id) = p2p.tunnel_info() {
            NODE_ID_ROW.with(|c| {
                if let Some(row) = c.borrow().as_ref() {
                    row.set_subtitle(&node_id);
                }
            });
        }
    } else {
        NODE_ID_ROW.with(|c| {
            if let Some(row) = c.borrow().as_ref() {
                row.set_subtitle("—");
            }
        });
    }
}

/// Rebuild the Contacts group's rows from `P2pService::list_contacts()`.
/// Called on initial page build, after a removal (below), and on every re-nav
/// to the `p2p` sub-page (`main.rs`'s nav match, mirroring the `"contacts"`
/// page's fetch-on-nav) — cheap: a local SQLite read, no network. No-op if
/// the page has not been built yet (no `P2pService`, or the group ref was
/// never stashed).
pub(crate) fn refresh_contacts_list() {
    let Some(group) = CONTACTS_GROUP.with(|c| c.borrow().clone()) else {
        return;
    };
    let Some(p2p) = super::get_p2p() else {
        return;
    };
    let contacts = p2p.list_contacts().unwrap_or_default();

    CONTACTS_ROWS.with(|rows| {
        for row in rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
    });

    if contacts.is_empty() {
        let empty_row = adw::ActionRow::builder()
            .title(crate::i18n::strings::p2p::NO_CONTACTS)
            .build();
        group.add(&empty_row);
        CONTACTS_ROWS.with(|rows| rows.borrow_mut().push(empty_row.upcast()));
        return;
    }

    for contact in contacts {
        let row = build_contact_row(&contact);
        group.add(&row);
        CONTACTS_ROWS.with(|rows| rows.borrow_mut().push(row.upcast()));
    }
}

/// One P2P contact row: `p2p-contact-row` (container) → `p2p-contact-name`
/// (display name) + `p2p-contact-remove-button` (local-only removal via
/// `P2pService::remove_contact` → the survivor `delete_contact`; the
/// superseded `revoke_contact` rail is gone — p2p.md § Implementation
/// status today).
fn build_contact_row(contact: &fauna_peer::contact::PeerContact) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&contact.display_name)
        .build();
    crate::testid::set_test_id(&row, ids::P2P_CONTACT_ROW);

    // 1px marker carrying the row's display name as separately-addressable
    // `p2p-contact-name` text — the row's real `.title()` above paints the
    // same string, but ActionRow's internal title label isn't its own
    // widget (the 1px-marker idiom this file uses for text-only rows).
    let name_marker = gtk::Label::new(Some(&contact.display_name));
    name_marker.set_height_request(1);
    name_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&name_marker, ids::P2P_CONTACT_NAME);
    row.add_prefix(&name_marker);

    let remove_btn = gtk::Button::with_label(crate::i18n::strings::p2p::DELETE_CONTACT);
    remove_btn.add_css_class("flat");
    remove_btn.add_css_class("destructive-action");
    remove_btn.set_valign(gtk::Align::Center);
    crate::testid::set_test_id(&remove_btn, ids::P2P_CONTACT_REMOVE_BUTTON);

    let actor_id_hex = fauna_core::hex32::encode(&contact.actor_id);
    remove_btn.connect_clicked(move |_| {
        let Some(p2p) = super::get_p2p() else {
            tracing::error!("[settings/p2p] remove contact: no P2pService available");
            return;
        };
        match p2p.remove_contact(&actor_id_hex) {
            Ok(()) => refresh_contacts_list(),
            Err(e) => tracing::error!("[settings/p2p] remove_contact failed: {e}"),
        }
    });
    row.add_suffix(&remove_btn);

    row
}
/// Discover this device's LAN IPv4 addresses — real dialable RFC-1918
/// addresses, via the shared `fauna_peer_sync::lan` arithmetic (the same
/// discovery the peer leg uses to compose `EndpointFacts.lan_addrs`), not
/// interface *names*. Reusing it here (priority #2) means the row's label
/// ("LAN Addresses") and its content finally agree.
fn get_lan_addresses() -> String {
    let addrs: Vec<String> = fauna_peer_sync::lan::discover_lan_candidates()
        .iter()
        .map(std::net::Ipv4Addr::to_string)
        .collect();

    if addrs.is_empty() {
        p2p_page::LAN_NONE.to_string()
    } else {
        addrs.join(", ")
    }
}

/// Called externally to update the P2P tunnel status row in the status view.
/// Returns `(is_active, node_id)`.
pub fn get_tunnel_status_summary() -> (bool, Option<String>) {
    let Some(p2p) = super::get_p2p() else {
        return (false, None);
    };

    if p2p.is_tunnel_active() {
        (true, p2p.tunnel_info())
    } else {
        (false, None)
    }
}
