use crate::i18n::strings::{backups, common, conversations, events, family, media, profile};
use adw::prelude::*;

// NB: the top-level "Peers" sidebar item was removed in the 2026-06-28
// sync/folder UI unification — the device roster + folder control plane now
// live as the Settings → Devices / Settings → Folders sub-pages
// (`views/devices_folders/`). The legacy `{"view":"devices"}` nav still lands on
// the roster via the settings shell (test_agent.rs nav seam).

// ---------------------------------------------------------------------------
// Sidebar items — one per top-level section
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidebarItem {
    Conversations,
    Contacts,
    Profile,
    Events,
    Feed,
    Bridges,
    Media,
    Backups,
    Notifications,
    Moderation,
    Settings,
    /// GATED (ui.yaml `navigation.gated_tabs`): revealed only for a nest admin
    /// (`fauna.account.am_i_admin`) — see [`show_admin_sidebar_row`].
    Admin,
    /// GATED (ui.yaml `navigation.gated_tabs`): revealed only when
    /// `fauna.family.status` returns a relationship — guardian **or**
    /// supervised — or an incoming transfer proposal
    /// (`docs/goal/behavior/family-safety.md` § App surface + § Graduation
    /// & transfer). See [`show_family_sidebar_row`].
    Family,
}

impl SidebarItem {
    /// Standard sidebar items (always visible). The two GATED items (Admin,
    /// Family) are appended after these, hidden, and revealed by their
    /// respective `show_*_sidebar_row` — so they must NOT appear here.
    pub const ALL: [SidebarItem; 11] = [
        SidebarItem::Conversations,
        SidebarItem::Contacts,
        SidebarItem::Profile,
        SidebarItem::Events,
        SidebarItem::Feed,
        SidebarItem::Bridges,
        SidebarItem::Media,
        SidebarItem::Backups,
        SidebarItem::Notifications,
        SidebarItem::Moderation,
        SidebarItem::Settings,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            SidebarItem::Conversations => conversations::list::TITLE,
            SidebarItem::Contacts => common::CONTACTS,
            SidebarItem::Profile => profile::TITLE,
            SidebarItem::Events => events::TITLE,
            SidebarItem::Feed => common::FEED,
            SidebarItem::Bridges => common::BRIDGES,
            SidebarItem::Media => media::TITLE,
            SidebarItem::Backups => backups::TITLE,
            SidebarItem::Notifications => common::NOTIFICATIONS,
            SidebarItem::Moderation => common::MODERATION,
            SidebarItem::Settings => common::SETTINGS,
            SidebarItem::Admin => common::ADMIN,
            SidebarItem::Family => family::TITLE,
        }
    }

    pub fn icon_name(&self) -> &'static str {
        match self {
            SidebarItem::Conversations => "mail-unread-symbolic",
            SidebarItem::Contacts => "avatar-default-symbolic",
            SidebarItem::Profile => "contact-new-symbolic",
            SidebarItem::Events => "x-office-calendar-symbolic",
            SidebarItem::Feed => "network-wireless-symbolic",
            SidebarItem::Bridges => "network-workgroup-symbolic",
            SidebarItem::Media => "folder-documents-symbolic",
            SidebarItem::Backups => "drive-harddisk-symbolic",
            SidebarItem::Notifications => "bell-symbolic",
            SidebarItem::Moderation => "security-high-symbolic",
            SidebarItem::Settings => "emblem-system-symbolic",
            SidebarItem::Admin => "preferences-system-symbolic",
            SidebarItem::Family => "system-users-symbolic",
        }
    }

    /// Stack page name used as the child-name in `gtk::Stack`.
    pub fn stack_name(&self) -> &'static str {
        match self {
            SidebarItem::Conversations => "conversations",
            SidebarItem::Contacts => "contacts",
            SidebarItem::Profile => "profile",
            SidebarItem::Events => "events",
            SidebarItem::Feed => "feed",
            SidebarItem::Bridges => "bridges",
            SidebarItem::Media => "media",
            SidebarItem::Backups => "backups",
            SidebarItem::Notifications => "notifications",
            SidebarItem::Moderation => "moderation",
            SidebarItem::Settings => "settings",
            SidebarItem::Admin => "admin",
            SidebarItem::Family => "family",
        }
    }

    /// Widget name used on the sidebar `ListBoxRow` for E2E test targeting.
    pub fn tab_widget_name(&self) -> &'static str {
        match self {
            SidebarItem::Conversations => "conversations-tab",
            SidebarItem::Contacts => "contacts-tab",
            SidebarItem::Profile => "profile-tab",
            SidebarItem::Events => "events-tab",
            SidebarItem::Feed => "feed-tab",
            SidebarItem::Bridges => "bridges-tab",
            SidebarItem::Media => "media-tab",
            SidebarItem::Backups => "backups-tab",
            SidebarItem::Notifications => "notifications-tab",
            SidebarItem::Moderation => "moderation-tab",
            SidebarItem::Settings => "settings-tab",
            SidebarItem::Admin => "admin-tab",
            SidebarItem::Family => "family-tab",
        }
    }

    /// Look up a `SidebarItem` by its zero-based index in the sidebar. Indices
    /// `0..ALL.len()` map to `ALL`; the two GATED rows follow in build order —
    /// `ALL.len()` is `Admin`, `ALL.len() + 1` is `Family`. (A `ListBoxRow`'s
    /// `index()` counts every row, hidden ones included, so the mapping is
    /// stable whether or not either gate has fired.)
    pub fn from_index(index: usize) -> Option<SidebarItem> {
        if index < SidebarItem::ALL.len() {
            SidebarItem::ALL.get(index).copied()
        } else if index == SidebarItem::ALL.len() {
            Some(SidebarItem::Admin)
        } else if index == SidebarItem::ALL.len() + 1 {
            Some(SidebarItem::Family)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Build the sidebar ListBox
// ---------------------------------------------------------------------------

/// Build the sidebar `gtk::ListBox`. Calls `on_select` whenever the user
/// picks a different row.
///
/// The two GATED rows (ui.yaml `navigation.gated_tabs`) are appended at the end,
/// in this order, both initially hidden:
/// - `admin-tab` — call `show_admin_sidebar_row()` when admin status is confirmed.
/// - `family-tab` — call `show_family_sidebar_row()` when `fauna.family.status`
///   returns any relationship (guardian or supervised) or an incoming
///   transfer proposal.
pub fn build_sidebar(on_select: impl Fn(SidebarItem) + 'static) -> gtk::ListBox {
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Single);
    list_box.add_css_class("sidebar-list");
    list_box.add_css_class("navigation-sidebar");

    for item in SidebarItem::ALL {
        let row = build_sidebar_row(item);
        list_box.append(&row);
    }

    // Admin row — hidden by default, shown when admin status is confirmed.
    let admin_row = build_sidebar_row(SidebarItem::Admin);
    admin_row.set_visible(false);
    list_box.append(&admin_row);

    // Family row — hidden by default, shown when `fauna.family.status` returns a
    // guardianship (either role). Fails closed: on a status error it stays hidden.
    let family_row = build_sidebar_row(SidebarItem::Family);
    family_row.set_visible(false);
    list_box.append(&family_row);

    list_box.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            let index = row.index() as usize;
            if let Some(item) = SidebarItem::from_index(index) {
                on_select(item);
            }
        }
    });

    // Select the first row by default.
    if let Some(first) = list_box.row_at_index(0) {
        list_box.select_row(Some(&first));
    }

    list_box
}

/// Show the Admin sidebar row. Call this when admin status is confirmed.
pub fn show_admin_sidebar_row(list_box: &gtk::ListBox) {
    let admin_index = SidebarItem::ALL.len() as i32; // index 11 (after the 11 standard items)
    if let Some(row) = list_box.row_at_index(admin_index) {
        row.set_visible(true);
    }
}

/// Show the Family sidebar row (`family-tab`). Call this when
/// `fauna.family.status` returns any relationship — guardian **or** supervised
/// — or an incoming transfer proposal (family-safety.md § App surface →
/// Navigation; § Graduation & transfer widened the gate so a proposed
/// guardian with no other family relationship can reach the prompt).
pub fn show_family_sidebar_row(list_box: &gtk::ListBox) {
    let family_index = SidebarItem::ALL.len() as i32 + 1; // index 12 (after Admin)
    if let Some(row) = list_box.row_at_index(family_index) {
        row.set_visible(true);
    }
}

/// Build a single sidebar row: icon + label in a horizontal box.
fn build_sidebar_row(item: SidebarItem) -> gtk::ListBoxRow {
    let icon = gtk::Image::from_icon_name(item.icon_name());
    icon.set_margin_end(12);

    let label = gtk::Label::new(Some(item.label()));
    label.set_halign(gtk::Align::Start);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&icon);
    hbox.append(&label);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    // Use the tab widget name for E2E test targeting.
    crate::testid::set_test_id(&row, item.tab_widget_name());

    row
}
