use crate::i18n::strings::{common, contacts as contacts_strings};
use adw::prelude::*;

// `build_contact_detail` — a full in-page detail pane (Actor ID/Status
// sections + hardcoded-English Confirm/Block/Remove buttons) — used to live
// here, `#[allow(dead_code)]` with zero callers. Deleted rather than wired up
// : a contact-row's tap-through
// already opens that actor's real Profile page (`app.rs`'s
// `contacts_list_box.connect_row_activated` → `open_profile`, tested via
// `open_contact_profile` in test_profile.py/test_subscriptions.py) — the
// ratified cross-app contract (`contacts.md` § Relationship to Profile: "the
// detail half is the per-user Profile"; web/android/windows/ios/tui all
// navigate to Profile on a contact-row tap; only macOS shows an embedded
// pane, the one outlier). Building this pane out would have created a SECOND,
// contradictory detail surface duplicating logic Profile already owns
// correctly (confirm on the row itself, block on `profile-block-button`) and
// inventing a "Remove" action the wire protocol doesn't have (contacts.md §
// Persistence: `knocks.unblock` is the lone edge-deleting transition).

/// Build the empty-state detail pane shown when no contact is selected.
pub fn build_empty_detail() -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(common::CONTACTS))));
    outer.append(&header);

    let status = adw::StatusPage::builder()
        .title(contacts_strings::NO_CONTACT_SELECTED)
        .icon_name("avatar-default-symbolic")
        .vexpand(true)
        .build();

    outer.append(&status);
    outer
}
