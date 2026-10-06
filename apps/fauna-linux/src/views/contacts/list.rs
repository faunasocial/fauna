use adw::prelude::*;
use fauna_conversations::contacts::ContactsCache;
use fauna_core::format::PeerLabel;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::client::FaunaClient;
use crate::conversations::overlays;
use crate::i18n::strings::{common, contacts as contacts_strings};

/// Side-index mapping a contact's `actor_id` (the row's `widget_name`) to its
/// `(handle, domain)`. Populated by [`update_contacts_list`] and read by the
/// roster-filter closure so it can match through the shared
/// `fauna_core::format::contact_matches_filter` predicate without walking the
/// widget tree. Threaded from [`build_contact_list_with_boxes`] up to `app.rs`
/// so the filter closure (set once at build) and the row data (arriving later)
/// share one source.
pub type ContactFilterIndex = Rc<RefCell<HashMap<String, (Option<String>, Option<String>)>>>;

/// Populate the knocks `ListBox` from a slice of `KnockRow`.
/// Wires Accept / Block / Dismiss buttons to the `FaunaClient`.
pub fn update_knocks_list(
    list_box: &gtk::ListBox,
    knocks: &[crate::rows::KnockRow],
    client: &Rc<FaunaClient>,
) {
    fill_knocks_list(list_box, knocks, &overlays::projection(), |pid| {
        (
            {
                let (c, p) = (Rc::clone(client), pid.to_string());
                move || c.accept_knock(&p)
            },
            {
                let (c, p) = (Rc::clone(client), pid.to_string());
                move || c.dismiss_knock(&p)
            },
            {
                let (c, p) = (Rc::clone(client), pid.to_string());
                move || c.block_knock(&p)
            },
        )
    });
}

/// Replace the knocks list's rows with one [`build_wired_knock_row`] per knock,
/// `actions(peer_actor_id)` supplying its accept / dismiss / block legs — the
/// client-free half of [`update_knocks_list`], so a GTK unit test renders the
/// same rows the page does.
fn fill_knocks_list<A, D, B>(
    list_box: &gtk::ListBox,
    knocks: &[crate::rows::KnockRow],
    overlays: &ContactsCache,
    actions: impl Fn(&str) -> (A, D, B),
) where
    A: Fn() + 'static,
    D: Fn() + 'static,
    B: Fn() + 'static,
{
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    for knock in knocks {
        let (on_accept, on_dismiss, on_block) = actions(&knock.peer_actor_id);
        let row = build_wired_knock_row(knock, overlays, on_accept, on_dismiss, on_block);
        // No `set_widget_name(peer_actor_id)`, unlike the contacts roster: this
        // row's widget name IS its `knock-card` test id, and nothing reads a
        // knock row's name back — the actor id rides the three action closures.
        list_box.append(&row);
    }
}

/// Build a knock row with Accept / Dismiss / Block wired to the three actions.
///
/// The actions are callbacks rather than a `&Rc<FaunaClient>` so the row stays
/// a pure widget builder a GTK unit test can drive — the seam
/// [`build_contact_row`] uses for its confirm leg. They are required
/// parameters, so every button this packs is connected.
fn build_wired_knock_row(
    knock: &crate::rows::KnockRow,
    overlays: &ContactsCache,
    on_accept: impl Fn() + 'static,
    on_dismiss: impl Fn() + 'static,
    on_block: impl Fn() + 'static,
) -> gtk::ListBoxRow {
    let vbox_info = gtk::Box::new(gtk::Orientation::Vertical, 2);
    vbox_info.set_hexpand(true);

    // `knock-sender` through the one resolver with no handle: the viewer's
    // nickname for this sender when they gave one, else the shared short id,
    // as on every app (`contacts.md` § Where logic lives → Knock sender
    // display, § The private overlay). It used to be the summary, so the
    // "sender" label showed whatever the knock said about itself; the summary
    // now has the caption line below.
    let sender = gtk::Label::new(Some(
        &overlays
            .peer_label(None, None, &knock.peer_actor_id)
            .primary,
    ));
    sender.set_halign(gtk::Align::Start);
    sender.add_css_class("heading");
    crate::testid::set_test_id(&sender, ids::KNOCK_SENDER);
    vbox_info.append(&sender);

    // A knock with no summary falls back to the fixed line, never a blank one.
    let summary = knock
        .summary
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(contacts_strings::WANTS_TO_CONNECT);
    let subtitle = gtk::Label::new(Some(summary));
    subtitle.set_halign(gtk::Align::Start);
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");
    vbox_info.append(&subtitle);

    let accept_btn = gtk::Button::with_label(common::ACCEPT);
    accept_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&accept_btn, ids::CONTACTS_ACCEPT_BUTTON);

    let dismiss_btn = gtk::Button::with_label(common::DISMISS);
    crate::testid::set_test_id(&dismiss_btn, ids::KNOCK_DISMISS);

    let block_btn = gtk::Button::with_label(common::BLOCK);
    block_btn.add_css_class("destructive-action");
    crate::testid::set_test_id(&block_btn, ids::CONTACTS_BLOCK_BUTTON);

    accept_btn.connect_clicked(move |_| on_accept());
    dismiss_btn.connect_clicked(move |_| on_dismiss());
    block_btn.connect_clicked(move |_| on_block());

    let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    btn_box.append(&accept_btn);
    btn_box.append(&dismiss_btn);
    btn_box.append(&block_btn);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&vbox_info);
    hbox.append(&btn_box);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    crate::testid::set_test_id(&row, ids::KNOCK_CARD);
    row
}

/// Populate the contacts `ListBox` from a slice of `ContactRow`, refreshing the
/// `filter_index` (actor_id → handle/domain) the roster-filter closure reads.
pub fn update_contacts_list(
    list_box: &gtk::ListBox,
    filter_index: &ContactFilterIndex,
    contacts: &[crate::rows::ContactRow],
    member_reviews: &[fauna_core::data::MemberReview],
    client: &Rc<FaunaClient>,
) {
    // Refresh the filter side-index in its own scope so the mutable borrow is
    // dropped before `invalidate_filter` below re-runs the (immutably
    // borrowing) filter closure — otherwise the `RefCell` would double-borrow.
    {
        let mut index = filter_index.borrow_mut();
        index.clear();
        for contact in contacts {
            index.insert(
                contact.peer_actor_id.clone(),
                (contact.peer_handle.clone(), contact.peer_domain.clone()),
            );
        }
    }
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    // The row's names come finished from the overlay projection — the one
    // resolver over the viewer's nickname, the enriched handle, then the
    // canonical short id (`contacts.md` § The private overlay → *Where the
    // nickname paints*); this view resolves no name itself.
    let overlays = overlays::projection();
    for contact in contacts {
        let name =
            overlays.peer_label(None, contact.peer_handle.as_deref(), &contact.peer_actor_id);
        let labels = overlays.labels_line(&contact.peer_actor_id);
        let reviewed =
            fauna_core::data::is_under_review_hex(member_reviews, &contact.peer_actor_id);
        let row = build_contact_row(&name, labels.as_deref(), &contact.status, reviewed, {
            let c = Rc::clone(client);
            let pid = contact.peer_actor_id.clone();
            move || c.confirm_contact(&pid)
        });
        row.set_widget_name(&contact.peer_actor_id);
        list_box.append(&row);
    }
    // Re-apply the active query to the freshly-rebuilt rows.
    list_box.invalidate_filter();
}

/// Build the contact list pane: search bar, find results, knocks section, contacts list.
/// Returns `(outer_box, knocks_list_box, contacts_list_box, find_results_list_box,
/// contacts_filter_index)` — the last is the roster-filter side-index the caller
/// threads into [`update_contacts_list`].
pub fn build_contact_list_with_boxes(
    client: &Rc<FaunaClient>,
) -> (
    gtk::Box,
    gtk::ListBox,
    gtk::ListBox,
    gtk::ListBox,
    ContactFilterIndex,
) {
    // `accessible_role(Group)` at construction: a plain `gtk::Box` defaults to
    // role `Generic`, which Linux AT-SPI prunes from the tree, so `contacts-view`
    // (the page container) never resolves for `is_visible` even though its child
    // page-heading does. `Group` keeps the container discoverable (same pattern
    // as the device cards / onboarding rows; tracked internally).
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    crate::testid::set_test_id(&outer, ids::CONTACTS_VIEW);

    // The overlay projection moved (a Save here, a sibling device's edit, a
    // succession fold): the roster rows, the knock senders and the filter all
    // read it, so `app.rs` repaints both lists from the data it holds.
    {
        let tx = client.ui_sender();
        overlays::watch(&outer, move || {
            tx.send(crate::app::UiMessage::Data(
                crate::app::DataMessage::ContactOverlaysChanged,
            ));
        });
    }

    // Header bar.
    let header = adw::HeaderBar::new();
    let title_label = gtk::Label::new(Some(common::CONTACTS));
    crate::testid::set_test_id(&title_label, ids::PAGE_HEADING);
    header.set_title_widget(Some(&title_label));
    outer.append(&header);

    // Contacts search/filter field — filters existing contacts by name/handle.
    let contacts_search = gtk::SearchEntry::new();
    contacts_search.set_placeholder_text(Some(contacts_strings::FILTER_PLACEHOLDER));
    contacts_search.set_margin_start(8);
    contacts_search.set_margin_end(8);
    contacts_search.set_margin_top(4);
    contacts_search.set_margin_bottom(4);
    crate::testid::set_test_id(&contacts_search, ids::CONTACTS_SEARCH_FIELD);
    outer.append(&contacts_search);

    // Find User search bar with lookup button.
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    search_box.set_margin_start(8);
    search_box.set_margin_end(8);
    search_box.set_margin_top(4);
    search_box.set_margin_bottom(4);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(contacts_strings::HANDLE_OR_ACTOR_ID));
    search.set_hexpand(true);
    crate::testid::set_test_id(&search, ids::CONTACT_ACTOR_ID_FIELD);
    search_box.append(&search);

    let lookup_btn = gtk::Button::with_label(contacts_strings::find_user::FIND);
    lookup_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&lookup_btn, ids::CONTACT_ACTOR_ID_LOOKUP);
    {
        let entry = search.clone();
        lookup_btn.connect_clicked(move |_| {
            entry.emit_activate();
        });
    }
    search_box.append(&lookup_btn);
    outer.append(&search_box);

    // --- Find results section (shown after a search) ---
    let find_results_label = gtk::Label::new(Some(contacts_strings::SEARCH_RESULTS));
    find_results_label.set_halign(gtk::Align::Start);
    find_results_label.add_css_class("heading");
    find_results_label.set_margin_start(12);
    find_results_label.set_margin_top(8);
    find_results_label.set_margin_bottom(4);
    find_results_label.set_visible(false);
    outer.append(&find_results_label);

    let find_results_list = super::find::build_find_results();
    find_results_list.set_visible(false);
    outer.append(&find_results_list);

    let find_error_label = gtk::Label::new(None);
    find_error_label.set_halign(gtk::Align::Start);
    find_error_label.set_wrap(true);
    find_error_label.set_margin_start(12);
    find_error_label.set_margin_end(12);
    find_error_label.add_css_class("error");
    find_error_label.set_visible(false);
    crate::testid::set_test_id(&find_error_label, ids::CONTACT_FIND_ERROR);
    outer.append(&find_error_label);

    // Wire search entry activate: detect actor ID vs handle.
    {
        let c = Rc::clone(client);
        let results_list = find_results_list.clone();
        let results_label = find_results_label.clone();
        search.connect_activate(move |entry| {
            let input = entry.text().to_string().trim().to_lowercase();
            if input.is_empty() {
                return;
            }

            // Clear previous find results.
            while let Some(child) = results_list.first_child() {
                results_list.remove(&child);
            }

            // Classify via the shared recipient parser (priority #2/#4) instead of
            // re-deriving the actor-id check + handle split here. The 3-way routing
            // (actor-id row / remote handle resolve / bare-handle local resolve) is
            // linux-specific I/O layered on the shared `RecipientInput`.
            match fauna_core::resolve::classify_recipient(&input) {
                fauna_core::resolve::RecipientInput::ActorId(id) => {
                    // Actor ID: show result row directly with a Knock button. The
                    // label carries the FULL id (not `short_id`) — the row's own
                    // `set_ellipsize(Middle)` handles on-screen truncation, so the
                    // backing text (what `get_text()`/AT-SPI read) stays complete.
                    // Android's equivalent result label follows the same shape
                    // (`resolved.actorId` + `TextOverflow.Ellipsis`, ContactsScreen.kt)
                    // — a bare `short_id` here was this row's own outlier, silently
                    // breaking the raw-actor-id-lookup echo a caller resolving
                    // offline depends on to confirm the right person before
                    // knocking (test_contacts_knock_send_web.py's cross-app id-echo
                    // check).
                    let row = super::find::build_find_result_row(&id, &id, &c);
                    results_list.append(&row);
                    results_list.set_visible(true);
                    results_label.set_visible(true);
                }
                fauna_core::resolve::RecipientInput::Handle { user, domain } => {
                    // user@domain: resolve the domain to its nest URL, then look up
                    // the handle there (the `NestResolved` handler auto-chains).
                    c.resolve_nest(&domain, &user);
                    results_list.set_visible(true);
                    results_label.set_visible(true);
                }
                fauna_core::resolve::RecipientInput::Invalid => {
                    // Bare handle (no `@`) or otherwise unsplit input: resolve on the
                    // local nest. Covers "find a user on my own nest by handle".
                    c.resolve_handle(&input);
                    results_list.set_visible(true);
                    results_label.set_visible(true);
                }
            }
        });
    }

    // Scrollable area for knocks + contacts.
    let scroll_content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // --- Knocks section ---
    let knocks_label = gtk::Label::new(Some(contacts_strings::KNOCKS));
    knocks_label.set_halign(gtk::Align::Start);
    knocks_label.add_css_class("heading");
    knocks_label.set_margin_start(12);
    knocks_label.set_margin_top(12);
    knocks_label.set_margin_bottom(4);
    scroll_content.append(&knocks_label);

    let knocks_list = gtk::ListBox::new();
    knocks_list.set_selection_mode(gtk::SelectionMode::None);
    knocks_list.add_css_class("boxed-list");
    knocks_list.set_margin_start(8);
    knocks_list.set_margin_end(8);

    let knocks_placeholder = gtk::Label::new(Some(contacts_strings::NO_PENDING_KNOCKS));
    knocks_placeholder.add_css_class("dim-label");
    knocks_placeholder.set_margin_top(8);
    knocks_placeholder.set_margin_bottom(8);
    knocks_list.set_placeholder(Some(&knocks_placeholder));

    scroll_content.append(&knocks_list);

    // --- Contacts section ---
    let contacts_label = gtk::Label::new(Some(common::CONTACTS));
    contacts_label.set_halign(gtk::Align::Start);
    contacts_label.add_css_class("heading");
    contacts_label.set_margin_start(12);
    contacts_label.set_margin_top(16);
    contacts_label.set_margin_bottom(4);
    scroll_content.append(&contacts_label);

    let contacts_list = gtk::ListBox::new();
    contacts_list.set_selection_mode(gtk::SelectionMode::Single);
    contacts_list.add_css_class("boxed-list");
    contacts_list.set_margin_start(8);
    contacts_list.set_margin_end(8);
    contacts_list.set_margin_bottom(8);

    let contacts_placeholder = adw::StatusPage::builder()
        .title(contacts_strings::NO_CONTACTS)
        .icon_name("avatar-default-symbolic")
        .build();
    contacts_list.set_placeholder(Some(&contacts_placeholder));

    // `contacts-no-matches` (`ui.yaml:382`) — a NON-EMPTY roster narrowed to
    // zero rows must say so distinguishably from the true-empty roster
    // (`contacts.md` § Errors & edge cases case (c)).
    //
    // ⚠ It is a plain sibling of the list, NOT a child of the `contacts_list`
    // placeholder above, and that is load-bearing — MEASURED, not reasoned.
    // The placeholder reads as the natural home (it is the list's own
    // empty-state surface), and it was written that way first; at runtime the
    // agent then read `count=0` in exactly the state the id exists to report,
    // and `test_roster_filter_narrows_by_handle[linux]` stayed red. Moving the
    // label to a sibling turned that test green. The likely mechanism is that a
    // `GtkListBox` placeholder is not shown while the list still holds rows that
    // are merely filtered out, so `find::is_showing` (`w.is_visible() &&
    // w.is_child_visible()`) prunes the whole subtree — but only the placement
    // is proven; the mechanism is inference, so don't build on it.
    //
    // ⚠ A GTK unit test CANNOT catch this: nothing in a test tree is realized or
    // mapped, so the placeholder keeps its default `visible = true` and the walk
    // reaches a label parented inside it either way. An empty-list unit test of
    // the placeholder version passes and means nothing. The running app is the
    // only witness — see the note in `no_matches_tests`.
    //
    // ⚠ `contacts.md:141` recorded linux's case (c) as FIXED because
    // `contacts_placeholder_title` flips the placeholder *text* — but the id the
    // shared assertion reads was never painted, so
    // `test_roster_filter_narrows_by_handle[linux]` had been RED. Same live red
    // tui was found in on 2026-07-29 (`contacts.md:144`), and this takes tui's
    // richer condition rather than linux's old query-non-empty-alone key.
    let no_matches_label = gtk::Label::new(Some(contacts_strings::NO_MATCHING_CONTACTS));
    crate::testid::set_test_id(&no_matches_label, ids::CONTACTS_NO_MATCHES);
    no_matches_label.set_visible(false);
    no_matches_label.set_margin_top(8);
    no_matches_label.add_css_class("dim-label");

    // Wire the contacts search field to the roster filter. The match routes
    // through the shared `fauna_core::format::contact_matches_filter` predicate
    // (priority #2/#4) so all seven apps narrow the roster identically over
    // `handle@domain@actor-id` plus the viewer's own nickname and labels for
    // the person (the overlay projection). Each row's `(handle, domain)` comes from
    // `filter_index` (keyed by the row's `widget_name` = actor_id, populated in
    // `update_contacts_list`); the query is read live from the entry, so one
    // filter func + `invalidate_filter` serves both keystrokes and list reloads.
    let filter_index: ContactFilterIndex = Rc::new(RefCell::new(HashMap::new()));
    {
        let index = filter_index.clone();
        let entry = contacts_search.clone();
        contacts_list.set_filter_func(move |row| {
            let query = entry.text().to_string();
            let actor_id = row.widget_name().to_string();
            let index = index.borrow();
            let (handle, domain) = index
                .get(&actor_id)
                .map(|(h, d)| (h.as_deref(), d.as_deref()))
                .unwrap_or((None, None));
            overlays::projection().matches_filter(&query, handle, domain, &actor_id)
        });
    }
    {
        let list = contacts_list.clone();
        let placeholder = contacts_placeholder.clone();
        let entry = contacts_search.clone();
        let index = filter_index.clone();
        let no_matches = no_matches_label.clone();
        contacts_search.connect_search_changed(move |_| {
            list.invalidate_filter();
            let query = entry.text().to_string();
            placeholder.set_title(contacts_placeholder_title(&query));
            no_matches.set_visible(roster_narrowed_to_zero(
                &index.borrow(),
                &query,
                &overlays::projection(),
            ));
        });
    }

    scroll_content.append(&contacts_list);
    scroll_content.append(&no_matches_label);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&scroll_content)
        .build();

    outer.append(&scrolled);
    (
        outer,
        knocks_list,
        contacts_list,
        find_results_list,
        filter_index,
    )
}

/// Build the contact list pane without list box handles (legacy, unwired).
#[allow(dead_code)]
pub fn build_contact_list() -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Header bar.
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(common::CONTACTS))));
    outer.append(&header);

    // Find User search bar.
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Find user..."));
    search.set_margin_start(8);
    search.set_margin_end(8);
    search.set_margin_top(4);
    search.set_margin_bottom(4);
    outer.append(&search);

    // Scrollable area for knocks + contacts.
    let scroll_content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // --- Knocks section ---
    let knocks_label = gtk::Label::new(Some(contacts_strings::KNOCKS));
    knocks_label.set_halign(gtk::Align::Start);
    knocks_label.add_css_class("heading");
    knocks_label.set_margin_start(12);
    knocks_label.set_margin_top(12);
    knocks_label.set_margin_bottom(4);
    scroll_content.append(&knocks_label);

    let knocks_list = gtk::ListBox::new();
    knocks_list.set_selection_mode(gtk::SelectionMode::None);
    knocks_list.add_css_class("boxed-list");
    knocks_list.set_margin_start(8);
    knocks_list.set_margin_end(8);

    let knocks_placeholder = gtk::Label::new(Some(contacts_strings::NO_PENDING_KNOCKS));
    knocks_placeholder.add_css_class("dim-label");
    knocks_placeholder.set_margin_top(8);
    knocks_placeholder.set_margin_bottom(8);
    knocks_list.set_placeholder(Some(&knocks_placeholder));

    scroll_content.append(&knocks_list);

    // --- Contacts section ---
    let contacts_label = gtk::Label::new(Some(common::CONTACTS));
    contacts_label.set_halign(gtk::Align::Start);
    contacts_label.add_css_class("heading");
    contacts_label.set_margin_start(12);
    contacts_label.set_margin_top(16);
    contacts_label.set_margin_bottom(4);
    scroll_content.append(&contacts_label);

    let contacts_list = gtk::ListBox::new();
    contacts_list.set_selection_mode(gtk::SelectionMode::Single);
    contacts_list.add_css_class("boxed-list");
    contacts_list.set_margin_start(8);
    contacts_list.set_margin_end(8);
    contacts_list.set_margin_bottom(8);

    let contacts_placeholder = adw::StatusPage::builder()
        .title(contacts_strings::NO_CONTACTS)
        .icon_name("avatar-default-symbolic")
        .build();
    contacts_list.set_placeholder(Some(&contacts_placeholder));

    scroll_content.append(&contacts_list);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&scroll_content)
        .build();

    outer.append(&scrolled);
    outer
}

// NOTE: an unwired `build_knock_row` (same widgets as `build_wired_knock_row`
// but with no `connect_clicked`) lived here with zero callers. Deleted rather
// than left as a trap: it is the exact shape OPEN TRACK 17 turned out to be — a
// row whose buttons are built, id-tagged and packed, but connected to nothing,
// so the agent answers `{"ok": true}` while the user's click does nothing.
// `build_wired_knock_row` is the only knock-row builder.

/// Build a contact row: its names + status label + the `contact-confirm` button.
///
/// `name` is the shared resolver's answer for this person: `primary` heads the
/// row (`contact-name`), and when the viewer's nickname supplied it, `public`
/// — the name it replaced — renders beneath as `contact-public-name`, so a
/// private name never hides the public identity. `labels` is the viewer's
/// labels on one line (`contact-labels`), absent when there are none. Both
/// secondary lines are scoped within `contact-row[i]`, since each renders on
/// some rows only (`contacts.md` § The private overlay).
///
/// The confirm button renders on `accepted` rows only (user ruling IN-PERSON 2026-08-15;
/// contacts.md § Layout & flow region 2 — confirm promotes an accepted edge),
/// matching tui and the four apps that always gated it. `contact-confirm[i]`
/// indexes over the rows that carry it, not over `contact-row[i]` — same as
/// `contact-unattested-mark`, the page's other conditional per-row leaf.
///
/// `on_confirm` runs the `fauna.contacts.confirm` leg. It is a callback rather
/// than a `&Rc<FaunaClient>` (the knock rows' idiom) so the row stays a pure
/// widget builder a GTK unit test can drive — the same seam
/// `RecipientPicker::new` uses for its accept path.
pub fn build_contact_row(
    name: &PeerLabel,
    labels: Option<&str>,
    status: &str,
    reviewed: bool,
    on_confirm: impl Fn() + 'static,
) -> gtk::ListBoxRow {
    let names = gtk::Box::new(gtk::Orientation::Vertical, 2);
    names.set_hexpand(true);
    names.set_valign(gtk::Align::Center);

    let handle_label = gtk::Label::new(Some(&name.primary));
    handle_label.set_halign(gtk::Align::Start);
    handle_label.add_css_class("heading");
    crate::testid::set_test_id(&handle_label, ids::CONTACT_NAME);
    names.append(&handle_label);

    let caption = |text: &str, id: &str| {
        let label = gtk::Label::new(Some(text));
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        crate::testid::set_test_id(&label, id);
        names.append(&label);
    };
    if let Some(public) = &name.public {
        caption(public, ids::CONTACT_PUBLIC_NAME);
    }
    if let Some(labels) = labels {
        caption(labels, ids::CONTACT_LABELS);
    }

    // Status icon + label based on confirmation state.
    let (icon_name, css_class) = match status {
        "confirmed" | "accepted" => ("emblem-ok-symbolic", "success"),
        "pending" | "sent" => ("appointment-soon-symbolic", "warning"),
        "blocked" => ("action-unavailable-symbolic", "error"),
        _ => ("dialog-question-symbolic", "dim-label"),
    };

    let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(14);
    icon.add_css_class(css_class);
    status_box.append(&icon);

    // Shared status→label map (`fauna_core::format::contact_status_label`),
    // resolved through linux's i18n; the icon + css color above stay an idiomatic
    // per-app render. See contacts.md § Where logic lives → Status badge text.
    let status_text =
        fauna_core::format::contact_status_label(status).resolve(crate::i18n::strings::lookup);
    let status_label = gtk::Label::new(Some(&status_text));
    status_label.set_halign(gtk::Align::End);
    status_label.add_css_class("caption");
    status_label.add_css_class(css_class);
    crate::testid::set_test_id(&status_label, ids::CONTACT_STATUS);
    status_box.append(&status_label);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&names);
    hbox.append(&status_box);

    // 1px marker for E2E test identification of each contact row.
    let marker = gtk::Label::new(None);
    marker.set_height_request(1);
    marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&marker, ids::CONTACT_ROW);
    hbox.append(&marker);

    // The post-succession review badge — the second of § Propagation's three
    // renderings of the one flag (the member chip pair on the conversations
    // page is the load-bearing one; the permanent Settings view is the
    // third). Badge only, deliberately with no Keep/Remove pair: the
    // decision belongs where removal already lives (the group member chip),
    // and this row has no eviction affordance to join
    // (`identity-succession.md` § Propagation → *MLS groups*).
    if reviewed {
        let mark = gtk::Label::new(Some(crate::i18n::strings::contacts::UNATTESTED_MARK));
        mark.add_css_class("caption");
        mark.add_css_class("warning");
        crate::testid::set_test_id(&mark, ids::CONTACT_UNATTESTED_MARK);
        hbox.append(&mark);
    }

    // `contact-confirm` — a real button (ui.yaml declares one; linux once
    // painted a 1px inert `gtk::Label`, so the id existed but nothing could be
    // confirmed), rendered only on the row the transition applies to.
    if fauna_core::data::ContactStatus::from_wire(status)
        == Some(fauna_core::data::ContactStatus::Accepted)
    {
        let confirm_btn = gtk::Button::with_label(common::CONFIRM);
        confirm_btn.add_css_class("flat");
        confirm_btn.set_valign(gtk::Align::Center);
        crate::testid::set_test_id(&confirm_btn, ids::CONTACT_CONFIRM);
        confirm_btn.connect_clicked(move |_| on_confirm());
        hbox.append(&confirm_btn);
    }

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    // `contact-row` is the 1px marker above; the row is its scope, so
    // `scope="contact-row[i]"` reaches the row's conditional lines.
    crate::testid::set_test_scope(&row, ids::CONTACT_ROW);
    row
}

/// Picks the contacts-list empty-state title: a non-empty roster-filter query
/// distinguishes "search matched nothing" from "there is nothing to show" —
/// showing "No contacts yet." while the user is mid-search would misreport a
/// non-empty roster as empty. Mirrors web's `feed.list` no_matching_posts
/// ternary (`+page.svelte:1038`, `searchInput ? no_matching_posts : no_posts`).
/// Whether a **non-empty** roster has been narrowed to zero rows by a
/// **non-empty** query — the `contacts-no-matches` condition
/// (`contacts.md` § Errors & edge cases case (c)).
///
/// Computed from the same side-index + shared predicate the `set_filter_func`
/// uses, not from widget state, so it cannot disagree with what the list
/// actually shows. Web's/tui's richer condition: keying on "query non-empty"
/// alone (linux's older placeholder-title rule) cannot tell an empty account
/// from a narrowed roster.
fn roster_narrowed_to_zero(
    index: &HashMap<String, (Option<String>, Option<String>)>,
    query: &str,
    overlays: &ContactsCache,
) -> bool {
    if query.trim().is_empty() || index.is_empty() {
        return false;
    }
    !index.iter().any(|(actor_id, (handle, domain))| {
        overlays.matches_filter(query, handle.as_deref(), domain.as_deref(), actor_id)
    })
}

fn contacts_placeholder_title(filter_query: &str) -> &'static str {
    if filter_query.is_empty() {
        contacts_strings::NO_CONTACTS
    } else {
        contacts_strings::NO_MATCHING_CONTACTS
    }
}

/// A row's names with no overlay on the person — the public name alone.
#[cfg(test)]
fn plain(name: &str) -> PeerLabel {
    PeerLabel {
        primary: name.to_string(),
        public: None,
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use crate::automation::find;
    use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
    use std::collections::BTreeMap;

    fn reg(value: &str) -> Register {
        Register {
            stamp: Stamp::new(1, [1; 32]),
            value: Some(value.to_string()),
        }
    }

    /// Every widget under `root` carrying `id`, in document order.
    fn all(root: &gtk::Widget, id: &str) -> Vec<gtk::Widget> {
        let mut out = Vec::new();
        find::collect_in(root, id, &mut out);
        out
    }

    /// A projection holding one person the viewer nicknamed and labelled.
    fn projection(actor: &str) -> ContactsCache {
        let cache = ContactsCache::default();
        cache.replace(BTreeMap::from([(
            actor.to_string(),
            ContactOverlay {
                nickname: reg("Mum"),
                labels: BTreeMap::from([("book club".to_string(), reg("Book club"))]),
                ..Default::default()
            },
        )]));
        cache
    }

    /// The row a nickname heads shows the public name it replaced and the
    /// labels, each reachable as `contact-row[i]`'s own line; a row with no
    /// overlay paints neither element (`contacts.md` § The private overlay).
    #[test]
    fn a_nicknamed_row_keeps_its_public_name_and_labels_within_the_row() {
        crate::testid::run_on_gtk_thread(|| {
            let actor = "ab".repeat(32);
            let cache = projection(&actor);
            let list = gtk::ListBox::new();
            list.append(&build_contact_row(
                &plain("bob"),
                None,
                "accepted",
                false,
                || {},
            ));
            list.append(&build_contact_row(
                &cache.peer_label(None, Some("alice"), &actor),
                cache.labels_line(&actor).as_deref(),
                "accepted",
                false,
                || {},
            ));
            let root: gtk::Widget = list.upcast();
            let texts =
                |id: &str| -> Vec<String> { all(&root, id).iter().map(find::text_of).collect() };
            assert_eq!(texts(ids::CONTACT_NAME), ["bob", "Mum"]);
            assert_eq!(texts(ids::CONTACT_PUBLIC_NAME), ["alice"]);
            assert_eq!(texts(ids::CONTACT_LABELS), ["Book club"]);

            // The secondary lines answer under the row that carries them, and
            // under no other.
            let marker = |i: usize| all(&root, ids::CONTACT_ROW)[i].clone();
            let scope = |i: usize| find::scope_container(marker(i), ids::CONTACT_ROW);
            assert!(find::find_in(&scope(0), ids::CONTACT_PUBLIC_NAME).is_none());
            assert!(find::find_in(&scope(0), ids::CONTACT_LABELS).is_none());
            assert_eq!(
                find::find_in(&scope(1), ids::CONTACT_PUBLIC_NAME)
                    .as_ref()
                    .map(find::text_of)
                    .as_deref(),
                Some("alice")
            );
        });
    }

    /// The knock sender is the viewer's nickname for that sender when they
    /// gave one — and still the shared short id when they did not.
    #[test]
    fn a_nicknamed_knock_sender_reads_as_the_nickname() {
        crate::testid::run_on_gtk_thread(|| {
            let actor = "ab".repeat(32);
            let knock = crate::rows::KnockRow {
                peer_actor_id: actor.clone(),
                summary: None,
                timestamp: "0".to_string(),
            };
            let sender = |cache: &ContactsCache| {
                let row = build_wired_knock_row(&knock, cache, || {}, || {}, || {});
                let root: gtk::Widget = row.upcast();
                find::text_of(&find::find_in(&root, ids::KNOCK_SENDER).expect("knock-sender"))
            };
            assert_eq!(sender(&projection(&actor)), "Mum");
            assert_eq!(
                sender(&ContactsCache::default()),
                fauna_core::format::short_id(&actor)
            );
        });
    }

    /// A label narrows the roster to the people carrying it, so a query that
    /// matches only a label is not "no matches".
    #[test]
    fn a_label_query_matches_through_the_projection() {
        let actor = "ab".repeat(32);
        let index = HashMap::from([(actor.clone(), (Some("alice".to_string()), None))]);
        assert!(roster_narrowed_to_zero(
            &index,
            "book",
            &ContactsCache::default()
        ));
        assert!(!roster_narrowed_to_zero(
            &index,
            "book",
            &projection(&actor)
        ));
        assert!(!roster_narrowed_to_zero(&index, "mum", &projection(&actor)));
    }
}

#[cfg(test)]
mod placeholder_tests {
    use super::*;

    #[test]
    fn distinguishes_empty_roster_from_filtered_to_zero() {
        assert_eq!(
            contacts_placeholder_title(""),
            contacts_strings::NO_CONTACTS
        );
        assert_eq!(
            contacts_placeholder_title("zzqqxxnomatch"),
            contacts_strings::NO_MATCHING_CONTACTS
        );
    }
}

#[cfg(test)]
mod confirm_button_tests {
    use super::*;

    /// `contact-confirm` is declared a **button** in ui.yaml (`:6811`, in the
    /// contacts page's shared `elements`), and every other app paints one.
    /// linux painted a 1px inert `gtk::Label` marker, so the id resolved but the
    /// click went nowhere — `Client::confirm_contact` (the `fauna.contacts.confirm`
    /// leg) existed with zero callers. This pins the shape the agent needs: an
    /// activatable widget the real click path can deliver to.
    #[test]
    fn the_contact_confirm_id_rides_an_activatable_button() {
        crate::testid::run_on_gtk_thread(|| {
            let confirmed = Rc::new(std::cell::Cell::new(0u32));
            let row = build_contact_row(&plain("alice@example.test"), None, "accepted", false, {
                let confirmed = Rc::clone(&confirmed);
                move || confirmed.set(confirmed.get() + 1)
            });
            let root: gtk::Widget = row.upcast();
            let found = crate::automation::find::find_in(&root, "contact-confirm")
                .expect("the contacts row exposes contact-confirm");
            assert!(
                found.downcast_ref::<gtk::Button>().is_some(),
                "ui.yaml declares contact-confirm as a button; got {}",
                found.type_().name()
            );
            // Two distinct properties, one per failure mode this row has had:
            //
            // (1) the agent's real click path must *deliver*, not refuse — the
            //     inert 1px marker this replaced failed exactly here
            //     (`testing.md` point 11, and the flip in `agent.rs` now says so
            //     out loud instead of answering `ok`);
            let reply = crate::automation::agent::actuate_click(&found);
            assert_eq!(
                reply.get("ok").and_then(|v| v.as_bool()),
                Some(true),
                "clicking contact-confirm must be delivered: {reply:?}"
            );
            // (2) …and something must actually be *connected* to it — the half
            //     OPEN TRACK 17's agenda-RSVP bug was missing (buttons built,
            //     id-tagged, packed, and never `connect_clicked`). The agent's
            //     click emits `clicked` on a button synchronously, so the
            //     handler has run by the time (1) replied.
            assert_eq!(
                confirmed.get(),
                1,
                "contact-confirm must be wired to the fauna.contacts.confirm leg"
            );
        });
    }

    /// `contact-confirm` renders on `accepted` rows only (user ruling IN-PERSON
    /// 2026-08-15; contacts.md § Layout & flow region 2 — the confirm action is
    /// for accepted edges). A confirmed row carries NO confirm affordance: the
    /// nest guard makes a stray click safe, but a dead button is still the
    /// wrong surface, and four of seven apps already gated it this way.
    #[test]
    fn a_confirmed_row_carries_no_confirm_affordance() {
        crate::testid::run_on_gtk_thread(|| {
            for hidden_status in ["confirmed", "blocked"] {
                let row = build_contact_row(
                    &plain("ada@example.test"),
                    None,
                    hidden_status,
                    false,
                    || {},
                );
                let root: gtk::Widget = row.upcast();
                assert!(
                    crate::automation::find::find_in(&root, "contact-confirm").is_none(),
                    "a {hidden_status} row must not offer contact-confirm"
                );
                // The gate hides the affordance, never the row.
                assert!(
                    crate::automation::find::find_in(&root, "contact-row").is_some(),
                    "the {hidden_status} row itself still renders"
                );
            }
        });
    }
}

#[cfg(test)]
mod knock_row_tests {
    use super::*;
    use crate::rows::KnockRow;
    use std::cell::Cell;

    fn a_knock(summary: Option<&str>) -> KnockRow {
        KnockRow {
            peer_actor_id: "ab".repeat(32),
            summary: summary.map(str::to_string),
            timestamp: "0".to_string(),
        }
    }

    /// Every `gtk::Label` text under `w`, depth-first.
    fn label_texts(w: &gtk::Widget, out: &mut Vec<String>) {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            out.push(l.text().to_string());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            label_texts(&c, out);
            child = c.next_sibling();
        }
    }

    fn sender_text(root: &gtk::Widget) -> String {
        let sender = crate::automation::find::find_in(root, ids::KNOCK_SENDER)
            .expect("the knock row exposes knock-sender");
        crate::automation::find::text_of(&sender)
    }

    /// `knock-sender` is the shared `short_id` of the sender — the text all 7
    /// apps render (`contacts.md` § Where logic lives → Knock sender display).
    /// linux used to put the knock's summary here, so the "sender" label showed
    /// whatever the knock said about itself; the summary now has its own line.
    #[test]
    fn knock_sender_is_the_shared_short_id_and_the_summary_keeps_its_own_line() {
        crate::testid::run_on_gtk_thread(|| {
            let knock = a_knock(Some("hello from ab"));
            let row = build_wired_knock_row(&knock, &ContactsCache::default(), || {}, || {}, || {});
            let root: gtk::Widget = row.upcast();
            assert_eq!(
                sender_text(&root),
                fauna_core::format::short_id(&knock.peer_actor_id),
                "knock-sender must be the shared short id — not the summary, not the full hex"
            );
            let mut labels = Vec::new();
            label_texts(&root, &mut labels);
            assert!(
                labels.iter().any(|t| t == "hello from ab"),
                "the summary keeps a line of its own: {labels:?}"
            );
        });
    }

    /// A knock with an empty summary still names its sender, and its second
    /// line falls back to the fixed "wants to connect" rather than going blank.
    #[test]
    fn a_knock_without_a_summary_says_it_wants_to_connect() {
        crate::testid::run_on_gtk_thread(|| {
            let knock = a_knock(Some(""));
            let row = build_wired_knock_row(&knock, &ContactsCache::default(), || {}, || {}, || {});
            let root: gtk::Widget = row.upcast();
            assert_eq!(
                sender_text(&root),
                fauna_core::format::short_id(&knock.peer_actor_id)
            );
            let mut labels = Vec::new();
            label_texts(&root, &mut labels);
            assert!(
                labels
                    .iter()
                    .any(|t| t == contacts_strings::WANTS_TO_CONNECT),
                "an empty summary falls back to wants-to-connect: {labels:?}"
            );
        });
    }

    /// Each button runs its own action, exactly once — the callback seam must
    /// not swap or drop a leg (the unwired-row trap the NOTE above records).
    #[test]
    fn each_knock_button_runs_its_own_action() {
        crate::testid::run_on_gtk_thread(|| {
            let hits = Rc::new([Cell::new(0u32), Cell::new(0u32), Cell::new(0u32)]);
            let leg = |i: usize| {
                let hits = Rc::clone(&hits);
                move || hits[i].set(hits[i].get() + 1)
            };
            let row = build_wired_knock_row(
                &a_knock(None),
                &ContactsCache::default(),
                leg(0),
                leg(1),
                leg(2),
            );
            let root: gtk::Widget = row.upcast();
            let buttons = [
                ids::CONTACTS_ACCEPT_BUTTON,
                ids::KNOCK_DISMISS,
                ids::CONTACTS_BLOCK_BUTTON,
            ];
            for (i, id) in buttons.into_iter().enumerate() {
                crate::automation::find::find_in(&root, id)
                    .and_then(|w| w.downcast::<gtk::Button>().ok())
                    .unwrap_or_else(|| panic!("the knock row exposes {id} as a button"))
                    .emit_clicked();
                let counts: Vec<u32> = hits.iter().map(Cell::get).collect();
                let want: Vec<u32> = (0..3).map(|j| u32::from(j <= i)).collect();
                assert_eq!(
                    counts, want,
                    "clicking {id} must run exactly its own action"
                );
            }
        });
    }

    /// Every row the knocks list renders answers as `knock-card` — the indexed
    /// container ui.yaml declares and `test_knock_live_refresh.py` counts. The id
    /// sits on the row itself, and linux's test id IS the widget name
    /// (`testid::set_test_id`), so the list must not rename the row afterwards.
    /// It used to stamp each row with its sender's actor id, which erased
    /// `knock-card` from every row while `knock-sender` and the three buttons
    /// inside kept theirs: a page that visibly listed the knock and answered
    /// `count("knock-card") == 0`, read for weeks as a knock pump that never
    /// fired. The builder-level tests above cannot see it — the rename happened
    /// in the list, not the builder.
    #[test]
    fn every_rendered_knock_row_answers_as_a_knock_card() {
        crate::testid::run_on_gtk_thread(|| {
            let list = gtk::ListBox::new();
            let knocks = [
                KnockRow {
                    peer_actor_id: "cd".repeat(32),
                    ..a_knock(Some("hi"))
                },
                a_knock(None),
            ];
            fill_knocks_list(&list, &knocks, &ContactsCache::default(), |_| {
                (|| {}, || {}, || {})
            });
            let root: gtk::Widget = list.upcast();
            let senders = crate::automation::find::count_in(&root, ids::KNOCK_SENDER);
            assert_eq!(senders, knocks.len(), "each knock renders its knock-sender");
            assert_eq!(
                crate::automation::find::count_in(&root, ids::KNOCK_CARD),
                senders,
                "every rendered knock row must answer as knock-card"
            );
        });
    }
}

#[cfg(test)]
mod review_badge_tests {
    use super::*;

    /// The second of § Propagation's three renderings
    /// (`identity-succession.md` § Propagation → *MLS groups*, item 3a):
    /// a contact under post-succession review carries the badge.
    #[test]
    fn a_flagged_contact_carries_the_review_badge() {
        crate::testid::run_on_gtk_thread(|| {
            let person = fauna_core::identity::ActorId([5u8; 32]);
            let reviews = vec![fauna_core::data::MemberReview {
                person,
                reasons: vec![fauna_core::data::MemberUnattestedReason::CompromiseWindow],
            }];
            assert!(fauna_core::data::is_under_review_hex(
                &reviews,
                &person.to_hex()
            ));

            let row =
                build_contact_row(&plain("alice@example.test"), None, "accepted", true, || {});
            let root: gtk::Widget = row.upcast();
            assert!(
                crate::automation::find::find_in(&root, ids::CONTACT_UNATTESTED_MARK).is_some(),
                "a flagged contact must carry contact-unattested-mark"
            );
        });
    }

    #[test]
    fn an_unflagged_contact_carries_no_badge() {
        crate::testid::run_on_gtk_thread(|| {
            let row =
                build_contact_row(&plain("alice@example.test"), None, "accepted", false, || {});
            let root: gtk::Widget = row.upcast();
            assert!(
                crate::automation::find::find_in(&root, ids::CONTACT_UNATTESTED_MARK).is_none(),
                "an unflagged contact must not carry contact-unattested-mark"
            );
        });
    }
}

#[cfg(test)]
mod no_matches_tests {
    use super::*;

    /// The three zero-row states must stay distinguishable
    /// (`contacts.md` § Errors & edge cases case (c)).
    #[test]
    fn only_a_narrowed_non_empty_roster_counts_as_no_matches() {
        let none = ContactsCache::default();
        let mut index = HashMap::new();
        // True-empty roster: no query, and with a query — neither is "no matches".
        assert!(!roster_narrowed_to_zero(&index, "", &none));
        assert!(!roster_narrowed_to_zero(&index, "rosterbob", &none));

        index.insert(
            "aa".repeat(32),
            (
                Some("rosterbob".to_string()),
                Some("example.test".to_string()),
            ),
        );
        // Non-empty roster, no query → the list is simply unfiltered.
        assert!(!roster_narrowed_to_zero(&index, "", &none));
        // …a whitespace-only query is not a query.
        assert!(!roster_narrowed_to_zero(&index, "   ", &none));
        // …a query that MATCHES leaves rows showing.
        assert!(!roster_narrowed_to_zero(&index, "rosterbob", &none));
        // …and only a non-matching query over a non-empty roster is case (c).
        assert!(roster_narrowed_to_zero(&index, "zzqqxxnomatch", &none));
    }

    /// The agent must reach the label **in the state that matters**: a list that
    /// still HAS a row, with that row filtered out. That distinction is the
    /// whole bug. Parenting the label inside the `GtkListBox` placeholder passes
    /// an empty-list version of this test and still fails for real, because GTK
    /// shows a placeholder only when the list has no rows at all — not when its
    /// rows are merely filtered — so `find::is_showing` prunes the label exactly
    /// when a narrowed roster needs to report itself. This test therefore builds
    /// the filtered-not-empty case rather than the convenient empty one.
    #[test]
    fn the_no_matches_label_is_reachable_while_rows_exist_but_are_filtered_out() {
        crate::testid::run_on_gtk_thread(|| {
            let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let list = gtk::ListBox::new();
            // A real row that the filter rejects — the list is NOT empty.
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&gtk::Label::new(Some("rosterbob"))));
            list.append(&row);
            list.set_filter_func(|_| false);
            list.invalidate_filter();

            let label = gtk::Label::new(Some(contacts_strings::NO_MATCHING_CONTACTS));
            crate::testid::set_test_id(&label, ids::CONTACTS_NO_MATCHES);
            label.set_visible(true);
            container.append(&list);
            container.append(&label);

            let root: gtk::Widget = container.upcast();
            let found = crate::automation::find::find_in(&root, "contacts-no-matches").expect(
                "the agent's walk must reach contacts-no-matches while rows are filtered out",
            );
            assert_eq!(
                crate::automation::find::text_of(&found),
                contacts_strings::NO_MATCHING_CONTACTS
            );

            // And it must disappear from the walk when hidden — otherwise the
            // true-empty-roster state would report "no matches" too.
            label.set_visible(false);
            assert!(
                crate::automation::find::find_in(&root, "contacts-no-matches").is_none(),
                "a hidden no-matches label must not be findable"
            );

            // ⚠ Do NOT try to pin the placeholder trap here. A unit test cannot see
            // it: nothing in this tree is realized or mapped, so a `GtkListBox`
            // placeholder keeps its default `visible = true` and `find_in` reaches a
            // label parented inside it — which is precisely the false green that let
            // the placeholder version ship. The only witness that can tell these
            // apart is a running app, i.e. `test_roster_filter_narrows_by_handle`.
            // What this test *can* pin is the sibling contract above, and it does.
        });
    }
}
