use crate::client::FaunaClient;
use crate::i18n::strings::common;
use adw::prelude::*;
use fauna_client_notifications::NotificationDestination;
use fauna_ui_ids as ids;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The shared decision's answer for each painted row, index-for-index with the
/// list box's rows — read at activation time so a click never re-derives it.
pub type Destinations = Rc<RefCell<Vec<Option<NotificationDestination>>>>;

thread_local! {
    /// Session-local set of notification ids whose muted-word collapse the user
    /// revealed (`notifications.md` § Muted words rule 3) — render state, the
    /// twin of the feed's `REVEALED_MUTED_POSTS`. The mute stays.
    static REVEALED_MUTED_NOTIFICATIONS: RefCell<std::collections::HashSet<i64>> =
        RefCell::new(std::collections::HashSet::new());
}

pub struct NotificationHandles {
    pub count_badge: gtk::Label,
    pub list_box: gtk::ListBox,
    pub destinations: Destinations,
    /// `notification-clear` — live only while a row is shown.
    pub clear_button: gtk::Button,
    /// The newest `created_at` the page has painted — the bound a clear-all
    /// sends, so a row that arrived after the user looked survives
    /// (`notifications.md` § Retention rule 2). `None` on an empty page.
    pub clear_up_to: Rc<Cell<Option<i64>>>,
}

/// Whether a row with this destination is offered as a control. The shared
/// decision's answer minus its one app-local subtraction: `Family` only while
/// the family tab is gated in (tui's `navigable()` shape) — a control into a
/// page the sidebar hides would be a dead end.
fn navigable(dest: &Option<NotificationDestination>, family_gated_in: bool) -> bool {
    match dest {
        None => false,
        Some(NotificationDestination::Family) => family_gated_in,
        Some(_) => true,
    }
}

/// Re-derive every row's `activatable` flag from its stored destination and the
/// current family gate. A row with no destination is plain text, never a
/// control that silently does nothing; rerun when the gate moves, because
/// `fauna.family.status` can land after the first notifications paint.
pub fn sync_activatable(
    list_box: &gtk::ListBox,
    destinations: &Destinations,
    family_gated_in: bool,
) {
    for (i, dest) in destinations.borrow().iter().enumerate() {
        if let Some(row) = list_box.row_at_index(i as i32) {
            row.set_activatable(navigable(dest, family_gated_in));
        }
    }
}

/// The cross-page navigation a `notification-item` activation dispatches into,
/// one per [`NotificationDestination`] arm — each routed into the destination
/// page's own gesture, as a direct click there would (`notifications.md`
/// § Deep-link destinations; mirrors tui's `open_notification`).
pub struct NotificationNav {
    /// The main content stack, switched to the destination page.
    pub stack: gtk::Stack,
    /// Opens the Feed post-detail pane for a post named only by id — the same
    /// door a `search-result-item` uses; it also resolves a post the timeline
    /// never loaded.
    pub open_post_detail: Rc<dyn Fn(String)>,
    /// Switches Contacts to its People segment, which lists the pending knocks.
    pub switch_to_people_segment: Rc<dyn Fn()>,
    /// The sidebar, for the Family gate and its selection.
    pub sidebar_list_box: gtk::ListBox,
}

/// Wire row activation on the notifications list.
pub fn connect_row_activation(
    list_box: &gtk::ListBox,
    destinations: &Destinations,
    nav: NotificationNav,
) {
    let destinations = Rc::clone(destinations);
    list_box.connect_row_activated(move |_, row| {
        let idx = row.index().max(0) as usize;
        let dest = destinations.borrow().get(idx).cloned().flatten();
        let gated = crate::views::sidebar::family_sidebar_row_shown(&nav.sidebar_list_box);
        if !navigable(&dest, gated) {
            return;
        }
        match dest {
            Some(NotificationDestination::Post { post_id }) => {
                nav.stack.set_visible_child_name("feed");
                (nav.open_post_detail)(post_id);
            }
            Some(NotificationDestination::Knock { .. }) => {
                nav.stack.set_visible_child_name("contacts");
                (nav.switch_to_people_segment)();
            }
            Some(NotificationDestination::Family) => {
                nav.stack.set_visible_child_name("family");
                let family_index = crate::views::sidebar::SidebarItem::ALL.len() as i32 + 1;
                if let Some(row) = nav.sidebar_list_box.row_at_index(family_index) {
                    nav.sidebar_list_box.select_row(Some(&row));
                }
            }
            Some(NotificationDestination::External { url }) => {
                crate::url_opener::open(&url);
            }
            None => {}
        }
    });
}

pub fn build_notifications_view() -> (gtk::Box, NotificationHandles) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    let title_label = gtk::Label::new(Some(common::NOTIFICATIONS));
    crate::testid::set_test_id(&title_label, ids::PAGE_HEADING);
    header.set_title_widget(Some(&title_label));

    let mark_read_btn = gtk::Button::with_label(common::MARK_ALL_READ);
    mark_read_btn.add_css_class("flat");
    crate::testid::set_test_id(&mark_read_btn, ids::NOTIFICATION_MARK_READ);
    header.pack_end(&mark_read_btn);

    let clear_btn = gtk::Button::with_label(crate::i18n::strings::notifications::CLEAR_ALL);
    clear_btn.add_css_class("flat");
    clear_btn.set_sensitive(false);
    crate::testid::set_test_id(&clear_btn, ids::NOTIFICATION_CLEAR);
    header.pack_end(&clear_btn);

    let count_badge = gtk::Label::new(Some(&common::unread_count("0")));
    count_badge.add_css_class("badge");
    crate::testid::set_test_id(&count_badge, ids::NOTIFICATION_COUNT_BADGE);
    header.pack_start(&count_badge);

    // Wire the mark-read button to reset the badge text.
    {
        let badge = count_badge.clone();
        mark_read_btn.connect_clicked(move |_| {
            badge.set_text(&common::unread_count("0"));
        });
    }

    outer.append(&header);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let placeholder = adw::StatusPage::builder()
        .title(common::NO_NOTIFICATIONS)
        .icon_name("bell-symbolic")
        .build();
    list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list_box)
        .build();
    outer.append(&scrolled);

    let handles = NotificationHandles {
        count_badge,
        list_box,
        destinations: Rc::new(RefCell::new(Vec::new())),
        clear_button: clear_btn,
        clear_up_to: Rc::new(Cell::new(None)),
    };
    (outer, handles)
}

/// Rebuild the notifications list from the rows a `fauna.notifications.list` fetch
/// returned — the notifications twin of `contacts::list::update_knocks_list`.
///
/// Until 2026-07-12 nothing called [`build_notification_row`] at all: the page was a
/// permanently-empty shell (the placeholder + a hard-coded "0 unread" badge), and
/// `NotificationsLoaded` only counted unread rows to fire desktop toasts. So a
/// notification never appeared on the page on *any* path — push, navigation, or
/// reconnect. This is the missing half.
///
/// Called from every `NotificationsLoaded` (login fetch, the `fauna.notification`
/// push arm, `ResyncRequired`, `Reconnected`), so a notification arriving while the
/// user sits on the page shows up with no navigation — the same liveness the web SPA
/// gets from its push arm (`transport.md` § Push events).
///
/// Each row says what the shared decision picks — the localized body, the
/// English `summary`, or the default ([`crate::i18n::notification_text`]).
/// `created_at` is epoch-**microseconds** on the unified-notification wire shape;
/// [`crate::client::format_epoch_us`] resolves the relative-time bucket through
/// shared Rust (`fauna_core::format::relative_time`), never hand-rolled here.
/// Returns the unread count so the caller can keep `AppState` in step.
#[allow(clippy::too_many_arguments)]
pub fn update_notifications_list(
    list_box: &gtk::ListBox,
    count_badge: &gtk::Label,
    destinations: &Destinations,
    family_gated_in: bool,
    items: &[fauna_client_notifications::notifications::NotifItem],
    clear_button: &gtk::Button,
    clear_up_to: &Cell<Option<i64>>,
    client: &Rc<FaunaClient>,
) -> u32 {
    crate::views::layout::clear_list_box_rows(list_box);
    let mut painted = Vec::with_capacity(items.len());

    // The muted-word collapse (§ Muted words): the shared decision over each
    // row's relayed text, against the one in-memory list the conversation
    // collapse reads (preloaded at login, refreshed by the Settings page).
    let muted = crate::conversations::muted_keywords_cache();
    let mut unread = 0u32;
    for (index, item) in items.iter().enumerate() {
        // The unread badge counts every unread row, muted or not (rule 4).
        if !item.is_read {
            unread += 1;
        }
        let revealed = REVEALED_MUTED_NOTIFICATIONS.with(|s| s.borrow().contains(&item.id));
        if !revealed && fauna_client_notifications::notification_muted(&muted, item) {
            // A collapsed row is no tap target: it holds no destination.
            painted.push(None);
            list_box.append(&build_muted_row(
                list_box,
                destinations,
                family_gated_in,
                index,
                item,
                client,
            ));
            continue;
        }
        let (row, dest) = build_full_row(item, family_gated_in, client);
        painted.push(dest);
        list_box.append(&row);
    }
    *destinations.borrow_mut() = painted;

    // Every loaded row counts toward the clear bound, a collapsed one too
    // (§ Muted words rule 4).
    clear_up_to.set(items.iter().map(|i| i.created_at).max());
    clear_button.set_sensitive(!items.is_empty());

    count_badge.set_text(&common::unread_count(&unread.to_string()));
    unread
}

/// One open row — its shared text, and the destination it is activatable for.
fn build_full_row(
    item: &fauna_client_notifications::notifications::NotifItem,
    family_gated_in: bool,
    client: &Rc<FaunaClient>,
) -> (gtk::ListBoxRow, Option<NotificationDestination>) {
    let row = build_notification_row(
        &item.notif_type,
        &crate::i18n::notification_text(item),
        &crate::client::format_epoch_us(item.created_at),
        &dismiss_button(item, client),
    );
    // Where the row leads is shared Rust's call, never a local type table.
    let dest = fauna_client_notifications::notification_destination(item);
    row.set_activatable(navigable(&dest, family_gated_in));
    (row, dest)
}

/// `notification-dismiss` for one row — greyed on a security notice inside its
/// retention window by the shared predicate (`notifications.md` § Retention
/// rule 2; the nest's refusal stays the authority). Every row carries one,
/// collapsed or open (§ Muted words rule 4).
fn dismiss_button(
    item: &fauna_client_notifications::notifications::NotifItem,
    client: &Rc<FaunaClient>,
) -> gtk::Button {
    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX));
    let button = gtk::Button::with_label(crate::i18n::strings::notifications::DISMISS);
    button.add_css_class("flat");
    button.set_sensitive(!fauna_client_notifications::notification_retained(
        item, now_us,
    ));
    crate::testid::set_test_id(&button, ids::NOTIFICATION_DISMISS);
    let (client, id) = (Rc::clone(client), item.id);
    button.connect_clicked(move |_| client.dismiss_notification(id));
    button
}

/// A muted-collapsed row (§ Muted words rule 3): the type icon, the placeholder
/// in place of the sentence, the timestamp, the dismiss and the one other
/// gesture — reveal, which swaps the full row in at the same index for this
/// session. Not activatable and not a `notification-item`, so a muted sender's
/// name never paints as a control.
fn build_muted_row(
    list_box: &gtk::ListBox,
    destinations: &Destinations,
    family_gated_in: bool,
    index: usize,
    item: &fauna_client_notifications::notifications::NotifItem,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top_line.set_margin_top(8);
    top_line.set_margin_bottom(8);
    top_line.set_margin_start(12);
    top_line.set_margin_end(12);

    let type_icon = gtk::Label::new(Some(notification_type_glyph(&item.notif_type)));
    crate::testid::set_test_id(&type_icon, ids::NOTIFICATION_TYPE_ICON);
    top_line.append(&type_icon);

    let placeholder = gtk::Label::new(Some(crate::i18n::strings::muted_words::COLLAPSED));
    placeholder.add_css_class("dim-label");
    placeholder.set_halign(gtk::Align::Start);
    placeholder.set_hexpand(true);
    crate::testid::set_test_id(&placeholder, ids::NOTIFICATION_MUTED);
    top_line.append(&placeholder);

    let reveal = gtk::Button::with_label(crate::i18n::strings::muted_words::REVEAL);
    reveal.add_css_class("flat");
    crate::testid::set_test_id(&reveal, ids::NOTIFICATION_MUTED_REVEAL_BUTTON);
    top_line.append(&reveal);

    let time_label = gtk::Label::new(Some(&crate::client::format_epoch_us(item.created_at)));
    time_label.add_css_class("dim-label");
    time_label.add_css_class("caption");
    top_line.append(&time_label);
    top_line.append(&dismiss_button(item, client));

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&top_line));
    row.set_activatable(false);
    {
        let item = item.clone();
        let client = Rc::clone(client);
        let list_box = list_box.clone();
        let destinations = Rc::clone(destinations);
        // Weak: the row owns this button through its child.
        let row_weak = glib::object::ObjectExt::downgrade(&row);
        reveal.connect_clicked(move |_| {
            REVEALED_MUTED_NOTIFICATIONS.with(|s| s.borrow_mut().insert(item.id));
            let Some(row) = row_weak.upgrade() else {
                return;
            };
            let (full, dest) = build_full_row(&item, family_gated_in, &client);
            list_box.remove(&row);
            list_box.insert(&full, index as i32);
            if let Some(slot) = destinations.borrow_mut().get_mut(index) {
                *slot = dest;
            }
        });
    }
    row
}

/// The glyph linux paints for a notification's type — delegates to the shared
/// `fauna_core::notification_glyph` classification web also uses via wasm, so
/// the two apps read the same at a glance (priority #1).
fn notification_type_glyph(notif_type: &fauna_protocol::notifications::NotifType) -> &'static str {
    fauna_core::notification_glyph::NotificationGlyph::of(notif_type).emoji()
}

/// Build a single notification row for the list box.
pub fn build_notification_row(
    notif_type: &fauna_protocol::notifications::NotifType,
    body: &str,
    timestamp: &str,
    dismiss: &gtk::Button,
) -> gtk::ListBoxRow {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    // A `gtk::Label`, not `gtk::Image` (the prior widget): ui.yaml registers
    // `notification-type-icon` as `type: text` and the e2e driver reads it via
    // `get_text` — an icon-only widget with no accessible text can never satisfy
    // that contract (verified 2026-07-17: it always resolved empty, regardless of
    // notification content — a real bug, not a test-writing gap).
    let type_icon = gtk::Label::new(Some(notification_type_glyph(notif_type)));
    crate::testid::set_test_id(&type_icon, ids::NOTIFICATION_TYPE_ICON);
    top_line.append(&type_icon);

    let body_label = gtk::Label::new(Some(body));
    body_label.set_halign(gtk::Align::Start);
    body_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    body_label.set_max_width_chars(80);
    body_label.set_hexpand(true);
    top_line.append(&body_label);

    let time_label = gtk::Label::new(Some(timestamp));
    time_label.set_halign(gtk::Align::End);
    time_label.add_css_class("dim-label");
    time_label.add_css_class("caption");
    top_line.append(&time_label);

    top_line.append(dismiss);

    vbox.append(&top_line);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&vbox));
    crate::testid::set_test_id(&row, ids::NOTIFICATION_ITEM);
    row
}
