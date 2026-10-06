pub mod address_book;
pub mod detail;
pub mod find;
pub mod guardian_ask;
pub mod list;

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

use crate::client::FaunaClient;
use crate::i18n::strings::contacts;
use crate::testid::set_test_id;

/// Build the full contacts view: a `Contacts | Address Book` segment switcher
/// over a stack of two panes — the social contacts split (list + detail, left)
/// and the CardDAV Address Book master-detail (slice 4b, a SEPARATE store —
/// contacts.md § Layout & flow). Selecting the Address Book segment lazily loads
/// it (web parity: `showAddressBook` fetches on first entry).
///
/// Returns `(outer_box, knocks_list_box, contacts_list_box, find_results_list_box,
/// contacts_filter_index, address_book_handles, switch_to_addressbook_segment)`
/// so the caller can update all lists when data arrives (the filter index is
/// threaded into `list::update_contacts_list`; the address-book handles into
/// the `AddressbooksLoaded` / `CardsLoaded` handlers), and can switch to the
/// Address Book segment programmatically (a `SearchNav::Contact` deep link)
/// WITHOUT the segment toggle's own `fetch_addressbooks` side effect — see
/// `switch_to_addressbook_segment`'s own doc comment.
#[allow(clippy::type_complexity)]
pub fn build_contacts_view(
    fauna_client: Rc<FaunaClient>,
) -> (
    gtk::Box,
    gtk::ListBox,
    gtk::ListBox,
    gtk::ListBox,
    list::ContactFilterIndex,
    address_book::AddressBookHandles,
    Rc<dyn Fn()>,
) {
    let (
        list_widget,
        knocks_list_box,
        contacts_list_box,
        find_results_list_box,
        contacts_filter_index,
    ) = list::build_contact_list_with_boxes(&fauna_client);

    let empty_detail = detail::build_empty_detail();

    let list_page = adw::NavigationPage::builder()
        .title(contacts::TITLE)
        .child(&list_widget)
        .build();

    let detail_page = adw::NavigationPage::builder()
        .title(contacts::DETAIL_TITLE)
        .child(&empty_detail)
        .build();

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&list_page));
    split.set_content(Some(&detail_page));

    // The Address Book segment (CardDAV vCards — slice 4b).
    let (address_book_view, address_book_handles) = address_book::build_address_book_view();

    // Segment switcher: Contacts | Address Book — a linked ToggleButton group,
    // mirroring the Events view-mode toggle (`events-view-toggle`).
    let segment = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    segment.add_css_class("linked");
    set_test_id(&segment, ids::CONTACTS_VIEW_SEGMENT);

    let people_btn = gtk::ToggleButton::with_label(contacts::TITLE);
    let addressbook_btn = gtk::ToggleButton::with_label(contacts::address_book::TITLE);
    addressbook_btn.set_group(Some(&people_btn));
    people_btn.set_active(true);
    set_test_id(&people_btn, ids::CONTACTS_SEGMENT_PEOPLE);
    set_test_id(&addressbook_btn, ids::CONTACTS_SEGMENT_ADDRESSBOOK);
    segment.append(&people_btn);
    segment.append(&addressbook_btn);

    let segment_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    segment_row.set_halign(gtk::Align::Center);
    segment_row.set_margin_top(8);
    segment_row.set_margin_bottom(4);
    segment_row.append(&segment);

    // Inner stack: the existing people split | the address book view.
    let inner = gtk::Stack::new();
    inner.set_vexpand(true);
    inner.set_hexpand(true);
    inner.add_named(&split, Some("people"));
    inner.add_named(&address_book_view, Some("addressbook"));

    // Suppresses the addressbook toggle's own `fetch_addressbooks` side effect
    // for a PROGRAMMATIC segment switch (`switch_to_addressbook_segment`
    // below): a `SearchNav::Contact` deep link already fetches the specific
    // book + card the locate resolved to in ONE round trip
    // (`FaunaClient::locate_card_by_uid`), so a second generic
    // `fetch_addressbooks` here would race that reply and could repaint the
    // Address Book with the wrong book's cards a moment after the locate
    // opened the right one. A real user click always wants the fetch (the
    // suppress flag defaults off), mirroring `views/feed/mod.rs`'s
    // Trending/Feeds `suppress` idiom for the same "don't re-trigger my own
    // side effect" shape.
    let suppress_addressbook_fetch = Rc::new(Cell::new(false));
    {
        let inner = inner.clone();
        people_btn.connect_toggled(move |b| {
            if b.is_active() {
                inner.set_visible_child_name("people");
            }
        });
    }
    {
        let inner = inner.clone();
        let client = Rc::clone(&fauna_client);
        let suppress = Rc::clone(&suppress_addressbook_fetch);
        addressbook_btn.connect_toggled(move |b| {
            if b.is_active() {
                inner.set_visible_child_name("addressbook");
                if !suppress.get() {
                    client.fetch_addressbooks();
                }
            }
        });
    }

    // Switch to the Address Book segment without the toggle's own fetch — the
    // `SearchNav::Contact` deep-link door (`ui/search.md` § Where logic lives
    // → Result navigation). A no-op when the segment is already active (GTK's
    // `set_active` only fires `connect_toggled` on an actual false→true
    // transition), which is fine: the caller still issues its own locate.
    let switch_to_addressbook_segment: Rc<dyn Fn()> = {
        let addressbook_btn = addressbook_btn.clone();
        let suppress = Rc::clone(&suppress_addressbook_fetch);
        Rc::new(move || {
            suppress.set(true);
            addressbook_btn.set_active(true);
            suppress.set(false);
        })
    };

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_vexpand(true);
    outer.set_hexpand(true);
    outer.append(&segment_row);
    outer.append(&inner);

    (
        outer,
        knocks_list_box,
        contacts_list_box,
        find_results_list_box,
        contacts_filter_index,
        address_book_handles,
        switch_to_addressbook_segment,
    )
}
