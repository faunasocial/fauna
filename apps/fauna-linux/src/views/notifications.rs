use crate::i18n::strings::common;
use adw::prelude::*;
use fauna_ui_ids as ids;

pub struct NotificationHandles {
    pub count_badge: gtk::Label,
    pub list_box: gtk::ListBox,
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
    items: &[fauna_client_notifications::notifications::NotifItem],
) -> u32 {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

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
        list_box.append(&row);
    }

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
