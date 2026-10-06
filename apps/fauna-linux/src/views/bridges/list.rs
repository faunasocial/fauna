use adw::prelude::*;
use fauna_ui_ids as ids;
use std::rc::Rc;

use crate::client::FaunaClient;
use crate::i18n::strings::{bridges as bridges_strings, common};

/// Handles returned from `build_bridge_list` for runtime population.
pub struct BridgeListHandles {
    pub list_box: gtk::ListBox,
}

/// Build the bridge list pane with live handles.
///
/// Returns `(outer_box, handles)`.
pub fn build_bridge_list(client: &Rc<FaunaClient>) -> (gtk::Box, BridgeListHandles) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    let title_label = gtk::Label::new(Some(common::BRIDGES));
    crate::testid::set_test_id(&title_label, ids::PAGE_HEADING);
    header.set_title_widget(Some(&title_label));

    let refresh_btn = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text(bridges_strings::REFRESH_LIST)
        .build();

    {
        let c = Rc::clone(client);
        refresh_btn.connect_clicked(move |_| {
            c.fetch_bridges();
        });
    }

    header.pack_end(&refresh_btn);
    outer.append(&header);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Single);
    list_box.add_css_class("boxed-list");
    list_box.set_margin_start(8);
    list_box.set_margin_end(8);
    list_box.set_margin_top(8);
    list_box.set_margin_bottom(8);

    let placeholder = adw::StatusPage::builder()
        .title(bridges_strings::NO_BRIDGES)
        .description(bridges_strings::NO_BRIDGES_DESC)
        .icon_name("network-transmit-symbolic")
        .build();
    list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();

    outer.append(&scrolled);

    let handles = BridgeListHandles { list_box };
    (outer, handles)
}

/// Populate the bridge list from a typed `fauna.bridges.list` reply.
///
/// Rows carry only the bridge id (as `widget_name`) + display fields; the
/// detail pane is opened by the list's single `row-activated` handler
/// (`mod::build_bridges_view`), which reads the selected bridge's `link_modes`
/// from the retained snapshot.
pub fn update_bridge_list(
    list_box: &gtk::ListBox,
    bridges: &[fauna_client_bridges::bridges_ui::BridgeStatus],
) {
    // Remove existing rows.
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    // The unified Bridges page shows only the providers that have no dedicated
    // settings page of their own — Nostr and Bluesky each own one, so the
    // shared `is_unified_bridges_page_bridge` predicate (one rule, every
    // app) drops them here. This is the page-level filter web applies in
    // `routes/bridges/+page.svelte`; `fetch_bridges` deliberately hands us the
    // unfiltered list because its other consumers need every provider.
    // bridges.md § Scope; ui/atproto.md § Migration.
    let bridges = bridges
        .iter()
        .filter(|b| fauna_client_bridges::is_unified_bridges_page_bridge(&b.id));

    for bridge in bridges {
        let row = build_bridge_list_row(&bridge.id, &bridge.name, bridge.linked, bridge.available);
        list_box.append(&row);
    }
}

/// Localized caption for a bridge list row's link status.
fn bridge_status_text(available: bool, linked: bool) -> &'static str {
    if !available {
        bridges_strings::STATUS_UNAVAILABLE
    } else if linked {
        common::LINKED
    } else {
        common::NOT_LINKED
    }
}

fn build_bridge_list_row(
    bridge_id: &str,
    name: &str,
    linked: bool,
    available: bool,
) -> gtk::ListBoxRow {
    let name_label = gtk::Label::new(Some(name));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_hexpand(true);

    let status_label = gtk::Label::new(Some(bridge_status_text(available, linked)));
    status_label.add_css_class("dim-label");
    status_label.add_css_class("caption");

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
    vbox.set_hexpand(true);
    vbox.append(&name_label);
    vbox.append(&status_label);

    let linked_icon = gtk::Image::from_icon_name(if linked {
        "emblem-ok-symbolic"
    } else {
        "emblem-important-symbolic"
    });
    linked_icon.set_margin_start(8);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hbox.set_margin_top(10);
    hbox.set_margin_bottom(10);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&vbox);
    hbox.append(&linked_icon);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row.set_widget_name(bridge_id);
    // Activation (→ detail pane) is handled once by the list's `row-activated`
    // handler in `mod::build_bridges_view`, which has the snapshot for the link
    // form — no per-row handler here (it would double-build the detail pane).

    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_wins_over_linked() {
        assert_eq!(
            bridge_status_text(false, true),
            bridges_strings::STATUS_UNAVAILABLE
        );
    }

    #[test]
    fn available_and_linked_reads_linked() {
        assert_eq!(bridge_status_text(true, true), common::LINKED);
    }

    #[test]
    fn available_and_unlinked_reads_not_linked() {
        assert_eq!(bridge_status_text(true, false), common::NOT_LINKED);
    }
}
