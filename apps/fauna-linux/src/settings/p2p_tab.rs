use adw::prelude::*;
use fauna_ui_ids as ids;
use gtk::glib;
use std::cell::RefCell;
use std::sync::Arc;

use crate::i18n::strings::{common, errors, settings::p2p_page};
use fauna_core::localized::LocalizedText;
use fauna_peer_sync::p2p_page as possible_here;

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
    static ERROR_LABEL: RefCell<Option<gtk::Label>> = const { RefCell::new(None) };
}

// ---------------------------------------------------------------------------
// Only what can work (p2p.md § Element IDs → *Only what can work*): every
// control is painted from its `fauna_peer_sync::p2p_page` predicate through
// the possibility seam, each click asks the same predicate first, and every
// failure lands on the tab's `error-message` — never a log line alone.
// ---------------------------------------------------------------------------

/// The node id there is to copy: the service's report while the node runs.
fn current_node_id() -> Option<String> {
    let p2p = super::get_p2p()?;
    if p2p.is_tunnel_active() {
        p2p.tunnel_info()
    } else {
        None
    }
}

/// Write `text` on the tab's `error-message` (shown).
fn show_error(text: &str) {
    ERROR_LABEL.with(|c| {
        if let Some(label) = c.borrow().as_ref() {
            label.set_text(text);
            label.set_visible(true);
        }
    });
}

/// Show a shared-Rust reason on the tab's `error-message`.
fn show_reason(reason: &LocalizedText) {
    show_error(&reason.resolve(crate::i18n::strings::lookup));
}

/// Clear the tab's `error-message` (hidden, per convention 2's rider).
fn clear_error() {
    ERROR_LABEL.with(|c| {
        if let Some(label) = c.borrow().as_ref() {
            label.set_text("");
            label.set_visible(false);
        }
    });
}

/// Paint the toggle and the node-id copy button from their predicates. Run at
/// build, after every start/stop, and on the status poll.
fn repaint_tunnel_controls() {
    let service = super::get_p2p().is_some();
    TOGGLE_BTN.with(|c| {
        if let Some(btn) = c.borrow().as_ref() {
            crate::possible::paint(btn, None, possible_here::tunnel_toggle_possibility(service));
        }
    });
    let node_id = current_node_id();
    NODE_ID_COPY_BTN.with(|c| {
        if let Some(btn) = c.borrow().as_ref() {
            crate::possible::paint(
                btn,
                None,
                possible_here::node_id_copy_possibility(node_id.as_deref()),
            );
        }
    });
}

/// Report a removal's outcome: success refreshes the list, a refusal is
/// written on the tab's `error-message`.
fn report_remove_result(name: &str, result: Result<(), String>) {
    match result {
        Ok(()) => {
            clear_error();
            refresh_contacts_list();
        }
        Err(e) => {
            tracing::warn!("[settings/p2p] remove_contact failed: {e}");
            show_reason(&possible_here::contact_remove_failed(name, &e));
        }
    }
}

/// `p2p-contact-remove-button`'s click: ask the predicate, then remove.
fn remove_contact_clicked(actor_id_hex: &str, name: &str) {
    let p2p = super::get_p2p();
    if let Some(reason) = possible_here::contact_remove_possibility(p2p.is_some()).reason() {
        show_reason(reason);
        return;
    }
    let Some(p2p) = p2p else { return };
    report_remove_result(
        name,
        p2p.remove_contact(actor_id_hex).map_err(|e| e.to_string()),
    );
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
        let ip_copy = gtk::Button::with_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
        ip_copy.add_css_class("flat");
        crate::testid::set_test_id(&ip_copy, ids::P2P_NODE_ID_COPY_BTN);
        ip_copy.connect_clicked(move |btn| {
            let node_id = current_node_id();
            let answer = possible_here::node_id_copy_possibility(node_id.as_deref());
            if let Some(reason) = answer.reason() {
                show_reason(reason);
                return;
            }
            if let Some(node_id) = node_id {
                crate::clipboard::copy_text(&node_id);
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
            let p2p = super::get_p2p();
            if let Some(reason) = possible_here::tunnel_toggle_possibility(p2p.is_some()).reason() {
                show_reason(reason);
                return;
            }
            let Some(p2p) = p2p else { return };

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
                repaint_tunnel_controls();
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
                    // The seam repaints the toggle live again (it was held
                    // insensitive while starting) and the node-id copy from
                    // the node's new state.
                    repaint_tunnel_controls();
                });
            }
        });
    }

    // Stash widget refs for periodic updates
    TUNNEL_STATUS_ROW.with(|c| *c.borrow_mut() = Some(status_row.clone()));
    NODE_ID_ROW.with(|c| *c.borrow_mut() = Some(node_id_row.clone()));
    TOGGLE_BTN.with(|c| *c.borrow_mut() = Some(toggle_btn.clone()));
    ERROR_LABEL.with(|c| *c.borrow_mut() = Some(error_label.clone()));

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
    let lan_addrs = fauna_peer_sync::lan::discover_lan_candidates();
    let lan_row = adw::ActionRow::builder()
        .title(p2p_page::LAN_ADDRESSES)
        .subtitle(lan_addresses_text(&lan_addrs))
        .subtitle_selectable(true)
        .build();
    {
        let lan_copy = gtk::Button::with_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
        lan_copy.add_css_class("flat");
        crate::testid::set_test_id(&lan_copy, ids::P2P_LAN_COPY_BTN);
        crate::possible::paint(
            &lan_copy,
            None,
            possible_here::lan_copy_possibility(&lan_addrs),
        );
        let addrs = lan_addrs.clone();
        lan_copy.connect_clicked(move |btn| {
            if let Some(reason) = possible_here::lan_copy_possibility(&addrs).reason() {
                show_reason(reason);
                return;
            }
            {
                crate::clipboard::copy_text(&lan_addresses_text(&addrs));
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

    repaint_tunnel_controls();

    crate::testid::wrap_page_with_heading(p2p_page::TITLE, ids::PAGE_HEADING, &page)
}

/// Refresh tunnel status rows from the current P2pService state.
fn refresh_tunnel_status() {
    repaint_tunnel_controls();
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

    crate::possible::paint(
        &remove_btn,
        None,
        possible_here::contact_remove_possibility(super::get_p2p().is_some()),
    );
    let actor_id_hex = fauna_core::hex32::encode(&contact.actor_id);
    let name = contact.display_name.clone();
    remove_btn.connect_clicked(move |_| remove_contact_clicked(&actor_id_hex, &name));
    row.add_suffix(&remove_btn);

    row
}
/// The LAN row's text for this device's LAN IPv4 addresses — real dialable RFC-1918
/// addresses, via the shared `fauna_peer_sync::lan` arithmetic (the same
/// discovery the peer leg uses to compose `EndpointFacts.lan_addrs`), not
/// interface *names*. Reusing it here (priority #2) means the row's label
/// ("LAN Addresses") and its content finally agree.
fn lan_addresses_text(addrs: &[std::net::Ipv4Addr]) -> String {
    if addrs.is_empty() {
        p2p_page::LAN_NONE.to_string()
    } else {
        addrs
            .iter()
            .map(std::net::Ipv4Addr::to_string)
            .collect::<Vec<_>>()
            .join(", ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;

    fn resolved(reason: &LocalizedText) -> String {
        reason.resolve(crate::i18n::strings::lookup)
    }

    fn error_text() -> Option<String> {
        ERROR_LABEL.with(|c| {
            c.borrow()
                .as_ref()
                .filter(|l| l.is_visible())
                .map(|l| l.text().to_string())
        })
    }

    /// With no peer service (its start failed at sign-in) the toggle and the
    /// node-id copy are painted disabled with their reasons, and a click the
    /// seam would never let through still says why on `error-message`.
    #[test]
    fn without_a_service_the_tunnel_controls_are_blocked_and_say_why() {
        run_on_gtk_thread(|| {
            assert!(
                super::super::get_p2p().is_none(),
                "test thread has no service"
            );
            let _page = build_p2p_page();
            let toggle = TOGGLE_BTN
                .with(|c| c.borrow().clone())
                .expect("toggle built");
            let unavailable = resolved(&possible_here::service_unavailable_reason());
            assert!(!toggle.is_sensitive());
            assert_eq!(toggle.tooltip_text().as_deref(), Some(unavailable.as_str()));
            let copy = NODE_ID_COPY_BTN
                .with(|c| c.borrow().clone())
                .expect("copy built");
            assert!(!copy.is_sensitive());
            assert!(
                copy.tooltip_text().is_some(),
                "the copy's reason is painted"
            );

            toggle.emit_clicked();
            assert_eq!(error_text().as_deref(), Some(unavailable.as_str()));
        });
    }

    /// A contact removal with no service is refused on `error-message`.
    #[test]
    fn a_removal_without_a_service_says_why() {
        run_on_gtk_thread(|| {
            let _page = build_p2p_page();
            clear_error();
            remove_contact_clicked(&"00".repeat(32), "Ana");
            assert_eq!(
                error_text(),
                Some(resolved(&possible_here::service_unavailable_reason()))
            );
        });
    }

    /// A removal the store refuses is written on `error-message`, naming the
    /// contact and the error — never a log line alone.
    #[test]
    fn a_failed_removal_sets_the_error_message() {
        run_on_gtk_thread(|| {
            let _page = build_p2p_page();
            clear_error();
            report_remove_result("Ana", Err("database is locked".to_string()));
            let text = error_text().expect("error-message shown");
            assert!(
                text.contains("Ana") && text.contains("database is locked"),
                "{text}"
            );
        });
    }
}
