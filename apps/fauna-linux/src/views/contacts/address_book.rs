//! The CardDAV "Address Book" master-detail, shown as the `Address Book` segment
//! of the Contacts page (carddav-server.md § Independent enablement, contacts.md
//! § Layout & flow). Read-only in slice 4b — a book picker + a card list + a
//! detail pane, mirroring the Events page's calendar/agenda/detail structure.
//!
//! Data flows in from `client.rs::{fetch_addressbooks, fetch_cards}` via
//! `DataMessage::{AddressbooksLoaded, CardsLoaded}`; the handler calls
//! [`update_book_list`] / [`update_card_list`]. A card click rebuilds the detail
//! pane locally ([`build_card_detail`]) — the vCard is already decoded, no
//! round-trip. All seal/parse/display-row logic lives in the shared
//! `fauna-client-carddav` crate (priority #2); this module is just GTK glue over
//! its `AddressbookRow`/`VCardRow` rows.

use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use fauna_client_carddav::{AddressbookRow, VCardRow};

use crate::client::FaunaClient;
use crate::i18n::strings::contacts::address_book as ab_strings;
use crate::testid::set_test_id;

/// Widget handles the message handler needs to repopulate the Address Book on
/// `AddressbooksLoaded` / `CardsLoaded` (mirror `EventViewHandles`).
#[allow(dead_code)]
pub struct AddressBookHandles {
    /// The book picker list — rebuilt on `AddressbooksLoaded`.
    pub book_list_box: gtk::ListBox,
    /// The card list for the selected book — rebuilt on `CardsLoaded`.
    pub card_list_box: gtk::ListBox,
    /// The detail pane container — rebuilt in-place on a card click, and reset to
    /// the "select a card" placeholder whenever the book changes.
    pub detail_container: gtk::Box,
    /// The whole Address Book view — mapped only while the Contacts page shows
    /// the Address Book segment, which is the page gate [`Self::is_showing`] reads.
    pub view: gtk::Box,
    /// The hex id of the book whose cards the list shows (or was last asked to
    /// show). A `CardsLoaded` for any other book is a late reply for a book the
    /// user has left and is dropped ([`update_card_list`]); a re-list keeps this
    /// book open rather than jumping back to the first ([`update_book_list`]).
    pub open_book: Rc<RefCell<Option<String>>>,
}

impl AddressBookHandles {
    /// Whether the Address Book half is on screen — the page gate for the
    /// `StaleSurfaces::address_book` re-read (`app.rs::apply_stale`). GTK's own
    /// map state, so it is false both off the Contacts page and on its People
    /// half.
    pub fn is_showing(&self) -> bool {
        self.view.is_mapped()
    }
}

/// Fetch one book's cards and record it as the open book, so the reply is
/// applied and a later re-list keeps it open.
fn open_book(open: &Rc<RefCell<Option<String>>>, client: &FaunaClient, book_id: &str) {
    *open.borrow_mut() = Some(book_id.to_string());
    client.fetch_cards(book_id);
}

/// Build the Address Book master-detail view (book picker | card list | detail).
///
/// Returns `(outer, handles)`; the caller inserts `outer` as the "addressbook"
/// child of the Contacts segment stack and keeps `handles` for updates.
pub fn build_address_book_view() -> (gtk::Box, AddressBookHandles) {
    // --- Book picker (left) ---
    let book_list_box = gtk::ListBox::new();
    book_list_box.set_selection_mode(gtk::SelectionMode::None);
    book_list_box.add_css_class("boxed-list");
    let book_placeholder = gtk::Label::new(Some(ab_strings::NO_ADDRESSBOOKS));
    book_placeholder.add_css_class("dim-label");
    book_placeholder.set_margin_top(8);
    book_placeholder.set_margin_bottom(8);
    book_list_box.set_placeholder(Some(&book_placeholder));

    let book_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&book_list_box)
        .build();

    let book_pane = gtk::Box::new(gtk::Orientation::Vertical, 6);
    book_pane.set_width_request(180);
    let book_header = gtk::Label::new(Some(ab_strings::TITLE));
    book_header.add_css_class("dim-label");
    book_header.set_halign(gtk::Align::Start);
    book_pane.append(&book_header);
    book_pane.append(&book_scroll);

    // --- Card list (middle) ---
    let card_list_box = gtk::ListBox::new();
    card_list_box.set_selection_mode(gtk::SelectionMode::None);
    card_list_box.add_css_class("boxed-list");
    let card_placeholder = gtk::Label::new(Some(ab_strings::NO_CARDS));
    card_placeholder.add_css_class("dim-label");
    card_placeholder.set_margin_top(8);
    card_placeholder.set_margin_bottom(8);
    card_list_box.set_placeholder(Some(&card_placeholder));

    let card_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&card_list_box)
        .build();
    let card_pane = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card_pane.set_width_request(220);
    card_pane.append(&card_scroll);

    // --- Detail pane (right) ---
    let detail_container = gtk::Box::new(gtk::Orientation::Vertical, 8);
    detail_container.set_margin_top(8);
    detail_container.set_margin_start(8);
    detail_container.set_margin_end(8);
    set_detail_placeholder(&detail_container);

    let detail_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&detail_container)
        .build();

    // --- Assemble ---
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    outer.set_hexpand(true);
    outer.set_vexpand(true);
    outer.set_margin_top(8);
    outer.set_margin_bottom(8);
    outer.set_margin_start(12);
    outer.set_margin_end(12);
    outer.append(&book_pane);
    outer.append(&card_pane);
    outer.append(&detail_scroll);

    let handles = AddressBookHandles {
        book_list_box,
        card_list_box,
        detail_container,
        view: outer.clone(),
        open_book: Rc::new(RefCell::new(None)),
    };
    (outer, handles)
}

/// Repopulate the book picker from a fresh `list_addressbooks` read, and
/// re-read the open book's cards — the book already open when it is still
/// listed (a `fauna.addressbook.changed` re-read must not move the user off the
/// book they are reading), else the first book, so the card list isn't empty
/// on entry (mirrors the web reference's `loadAddressbooks`). Each
/// `addressbook-item` click fetches that book's cards.
pub fn update_book_list(
    handles: &AddressBookHandles,
    books: &[AddressbookRow],
    client: &Rc<FaunaClient>,
) {
    let lb = &handles.book_list_box;
    while let Some(child) = lb.first_child() {
        lb.remove(&child);
    }
    for book in books {
        lb.append(&build_book_row(book, &handles.open_book, client));
    }
    let still_open = handles
        .open_book
        .borrow()
        .clone()
        .filter(|id| books.iter().any(|b| &b.id == id));
    match still_open.or_else(|| books.first().map(|b| b.id.clone())) {
        Some(id) => open_book(&handles.open_book, client, &id),
        None => *handles.open_book.borrow_mut() = None,
    }
}

/// Repopulate the Address Book from a `locate_card_by_uid_hash` resolve — the
/// `SearchNav::Contact` deep-link door (`ui/search.md` § Where logic lives →
/// Result navigation (deep link); `FaunaClient::locate_card_by_uid`).
///
/// Deliberately NOT built on [`update_book_list`] / [`update_card_list`]:
/// `update_book_list` auto-fetches the FIRST book's cards on repopulate (web
/// parity, "the card list isn't empty on entry"), which here would race a
/// SECOND `CardsLoaded` reply against the one this function is already
/// painting from — the locate round-trip already returned the specific book
/// AND card list holding the target card (or found nothing), in one read.
///
/// `open` is `None` when no book holds the `uid_hash` any more (deleted since
/// the search index picked it up): the book-picker rows still land, so the
/// user lands on a real picker rather than a blank pane, same as tui's
/// `Outcome::CardLocated` degrade.
pub fn open_located_card(
    handles: &AddressBookHandles,
    books: &[AddressbookRow],
    open: Option<(String, Vec<VCardRow>, String)>,
    client: &Rc<FaunaClient>,
) {
    let lb = &handles.book_list_box;
    while let Some(child) = lb.first_child() {
        lb.remove(&child);
    }
    for book in books {
        lb.append(&build_book_row(book, &handles.open_book, client));
    }

    set_detail_placeholder(&handles.detail_container);
    let cl = &handles.card_list_box;
    while let Some(child) = cl.first_child() {
        cl.remove(&child);
    }
    // The holding book is the open one now — or none, when no book holds it.
    *handles.open_book.borrow_mut() = open.as_ref().map(|(book_id, _, _)| book_id.clone());
    let Some((_, cards, card_id)) = open else {
        return;
    };
    for card in &cards {
        cl.append(&build_card_row(card, &handles.detail_container));
    }
    if let Some(card) = cards.iter().find(|c| c.id == card_id) {
        build_card_detail(&handles.detail_container, card);
    }
}

/// Repopulate the card list for the selected book, and reset the detail pane to
/// its placeholder (the previous book's card may no longer be in view). Cards
/// for any book but the open one are a late reply and are dropped.
pub fn update_card_list(handles: &AddressBookHandles, addressbook_id: &str, cards: &[VCardRow]) {
    if handles.open_book.borrow().as_deref() != Some(addressbook_id) {
        tracing::debug!("CardsLoaded for book {addressbook_id}, which is no longer open: dropped");
        return;
    }
    let lb = &handles.card_list_box;
    while let Some(child) = lb.first_child() {
        lb.remove(&child);
    }
    set_detail_placeholder(&handles.detail_container);
    for card in cards {
        lb.append(&build_card_row(card, &handles.detail_container));
    }
}

/// One book row: a clickable `addressbook-item` button (a Button, not a Label,
/// so AT-SPI `do_action(0)` fires the click) carrying name + card count. The
/// row's `widget_name` is the hex `addressbook_id`.
fn build_book_row(
    book: &AddressbookRow,
    open: &Rc<RefCell<Option<String>>>,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let name = if book.name.is_empty() {
        ab_strings::TITLE
    } else {
        book.name.as_str()
    };

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let name_label = gtk::Label::new(Some(name));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_hexpand(true);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let count_label = gtk::Label::new(Some(&book.card_count.to_string()));
    count_label.add_css_class("dim-label");
    count_label.add_css_class("caption");
    hbox.append(&name_label);
    hbox.append(&count_label);

    let btn = gtk::Button::new();
    btn.add_css_class("flat");
    btn.set_child(Some(&hbox));
    set_test_id(&btn, ids::ADDRESSBOOK_ITEM);
    {
        let cl = Rc::clone(client);
        let open = Rc::clone(open);
        let id = book.id.clone();
        btn.connect_clicked(move |_| open_book(&open, &cl, &id));
    }

    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&book.id);
    row.set_child(Some(&btn));
    row.set_activatable(false);
    row
}

/// One card row: a clickable `vcard-card` button (opens the detail pane) plus a
/// height-1 marker `Label` carrying `vcard-card-fn` for `get_text` — exactly the
/// `event-card` / `event-card-summary` pattern. The two share the card's index,
/// so `open_card_by_name` (click `vcard-card` at the matching `vcard-card-fn`
/// index) resolves correctly.
fn build_card_row(card: &VCardRow, detail: &gtk::Box) -> gtk::ListBoxRow {
    let fn_btn = gtk::Button::with_label(&card.formatted_name);
    fn_btn.set_halign(gtk::Align::Start);
    fn_btn.set_hexpand(true);
    fn_btn.add_css_class("flat");
    set_test_id(&fn_btn, ids::VCARD_CARD);
    {
        let d = detail.clone();
        let c = card.clone();
        fn_btn.connect_clicked(move |_| build_card_detail(&d, &c));
    }

    // Hidden marker carrying the canonical `vcard-card-fn` ID for get_text().
    // Height >= 1px so AT-SPI reports SHOWING (mirror `event-card-summary`).
    let fn_marker = gtk::Label::new(Some(&card.formatted_name));
    fn_marker.set_height_request(1);
    fn_marker.set_overflow(gtk::Overflow::Hidden);
    set_test_id(&fn_marker, ids::VCARD_CARD_FN);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(4);
    hbox.set_margin_bottom(4);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);
    hbox.append(&fn_btn);
    hbox.append(&fn_marker);

    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&card.id);
    row.set_child(Some(&hbox));
    row
}

/// Rebuild the detail pane for a selected card: FN header + ORG + one row per
/// EMAIL / TEL / ADR + NOTE (each value tagged with its `vcard-detail-*` ID,
/// indexed where repeatable).
pub fn build_card_detail(detail: &gtk::Box, card: &VCardRow) {
    while let Some(child) = detail.first_child() {
        detail.remove(&child);
    }

    let fn_label = gtk::Label::new(Some(&card.formatted_name));
    fn_label.set_halign(gtk::Align::Start);
    fn_label.add_css_class("title-2");
    fn_label.set_wrap(true);
    fn_label.set_xalign(0.0);
    set_test_id(&fn_label, ids::VCARD_DETAIL_FN);
    detail.append(&fn_label);

    if !card.title.is_empty() {
        let title = gtk::Label::new(Some(&card.title));
        title.add_css_class("dim-label");
        title.set_halign(gtk::Align::Start);
        detail.append(&title);
    }
    if !card.org.is_empty() {
        let org = gtk::Label::new(Some(&card.org));
        org.add_css_class("dim-label");
        org.set_halign(gtk::Align::Start);
        set_test_id(&org, ids::VCARD_DETAIL_ORG);
        detail.append(&org);
    }
    for email in &card.emails {
        detail.append(&detail_row(
            ab_strings::EMAIL,
            email,
            ids::VCARD_DETAIL_EMAIL,
        ));
    }
    for tel in &card.tels {
        detail.append(&detail_row(ab_strings::PHONE, tel, ids::VCARD_DETAIL_TEL));
    }
    for adr in &card.addresses {
        detail.append(&detail_row(ab_strings::ADDRESS, adr, ids::VCARD_DETAIL_ADR));
    }
    if !card.note.is_empty() {
        detail.append(&detail_row(
            ab_strings::NOTE,
            &card.note,
            ids::VCARD_DETAIL_NOTE,
        ));
    }
}

/// One labeled detail row: a dim field label + the value (tagged `value_id` for
/// `get_text`).
fn detail_row(label: &str, value: &str, value_id: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_top(2);
    row.set_margin_bottom(2);

    let label_widget = gtk::Label::new(Some(label));
    label_widget.add_css_class("dim-label");
    label_widget.add_css_class("caption");
    label_widget.set_halign(gtk::Align::Start);
    label_widget.set_width_request(90);
    label_widget.set_xalign(0.0);

    let value_widget = gtk::Label::new(Some(value));
    value_widget.set_halign(gtk::Align::Start);
    value_widget.set_hexpand(true);
    value_widget.set_wrap(true);
    value_widget.set_xalign(0.0);
    value_widget.set_selectable(true);
    set_test_id(&value_widget, value_id);

    row.append(&label_widget);
    row.append(&value_widget);
    row
}

/// Reset the detail pane to the "select a card" placeholder.
fn set_detail_placeholder(detail: &gtk::Box) {
    while let Some(child) = detail.first_child() {
        detail.remove(&child);
    }
    let placeholder = gtk::Label::new(Some(ab_strings::SELECT_CARD));
    placeholder.add_css_class("dim-label");
    placeholder.set_halign(gtk::Align::Start);
    detail.append(&placeholder);
}
