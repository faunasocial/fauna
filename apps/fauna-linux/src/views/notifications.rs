use crate::i18n::strings::common;
use adw::prelude::*;
use fauna_client_notifications::NotificationDestination;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

/// The shared decision's answer for each painted row, index-for-index with the
/// list box's rows — read at activation time so a click never re-derives it.
pub type Destinations = Rc<RefCell<Vec<Option<NotificationDestination>>>>;

pub struct NotificationHandles {
    pub count_badge: gtk::Label,
    pub list_box: gtk::ListBox,
    pub destinations: Destinations,
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
pub fn update_notifications_list(
    list_box: &gtk::ListBox,
    count_badge: &gtk::Label,
    destinations: &Destinations,
    family_gated_in: bool,
    items: &[fauna_client_notifications::notifications::NotifItem],
) -> u32 {
    crate::views::layout::clear_list_box_rows(list_box);
    let mut painted = Vec::with_capacity(items.len());

    let mut unread = 0u32;
    for item in items {
        if !item.is_read {
            unread += 1;
        }
        let row = build_notification_row(
            &item.notif_type,
            &crate::i18n::notification_text(item),
            &crate::client::format_epoch_us(item.created_at),
        );
        // Where the row leads is shared Rust's call, never a local type table.
        let dest = fauna_client_notifications::notification_destination(item);
        row.set_activatable(navigable(&dest, family_gated_in));
        painted.push(dest);
        list_box.append(&row);
    }
    *destinations.borrow_mut() = painted;

    count_badge.set_text(&common::unread_count(&unread.to_string()));
    unread
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

    vbox.append(&top_line);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&vbox));
    crate::testid::set_test_id(&row, ids::NOTIFICATION_ITEM);
    row
}
