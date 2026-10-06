use adw::prelude::*;
use fauna_ui_ids as ids;

use crate::i18n::strings::common;
use crate::i18n::strings::settings::encryption_page as enc;

const LOW_KEY_THRESHOLD: u32 = 10;

/// Build the "Encryption" preferences page.
pub fn build_encryption_page() -> gtk::Box {
    let page = adw::PreferencesPage::builder()
        .title(enc::TITLE)
        .icon_name("dialog-password-symbolic")
        .build();

    // --- Low-key warning row (hidden until count is fetched) ---
    let warning_group = adw::PreferencesGroup::new();
    let warning_row = adw::ActionRow::builder()
        .title(enc::LOW_KEY_WARNING_TITLE)
        .subtitle(enc::LOW_KEY_WARNING_SUBTITLE)
        .build();
    warning_row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
    warning_row.set_visible(false);
    warning_group.add(&warning_row);
    page.add(&warning_group);

    // --- Key packages group ---
    let keys_group = adw::PreferencesGroup::builder()
        .title(enc::MLS_KEY_PACKAGES)
        .description(enc::MLS_DESCRIPTION)
        .build();

    let key_count_row = adw::ActionRow::builder()
        .title(enc::AVAILABLE_KEY_PACKAGES)
        .subtitle(common::LOADING)
        .build();
    keys_group.add(&key_count_row);

    let refresh_row = adw::ActionRow::builder()
        .title(enc::REFRESH_KEYS)
        .subtitle(enc::REFRESH_KEYS_DESCRIPTION)
        .activatable(true)
        .build();

    let refresh_btn = gtk::Button::builder()
        .label(common::REFRESH)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    // Replenishing the one-time pool MINTS then PUBLISHES key packages — the
    // upload is the leg that needs the nest, and the count re-fetch behind it
    // is a Read that gates nothing (tui's `Action::RefreshKeyPackages`). No
    // `set_test_id` beside it: the Encryption page's refresh control carries no
    // ui.yaml id on any app, and the gate does not need one.
    crate::offline_gate::declare_wire_kind(&refresh_btn, "fauna.conversations.keypackage.upload");

    let key_count_row_ref = key_count_row.clone();
    let warning_row_ref = warning_row.clone();

    refresh_btn.connect_clicked(move |_| {
        if let Some(client) = crate::settings::get_client() {
            // Replenish the one-time pool through the durable manager surface
            // (mint on the session engine + notify→autosave), the SAME path login
            // and web drive — never a raw engine mint whose fresh private init
            // keys a later provider swap would wipe (`devices.md` § Cross-device
            // MLS group-state sync).
            crate::conversations::conv_backend::replenish_key_packages();

            // Re-fetch count after publishing so the row updates.
            let count_row = key_count_row_ref.clone();
            let warn_row = warning_row_ref.clone();
            // Trigger a re-fetch — the result will come back via UiMessage.
            // We optimistically update the subtitle while we wait.
            count_row.set_subtitle(common::REFRESHING);
            warn_row.set_visible(false);
            client.fetch_key_package_count();
        } else {
            tracing::error!("[settings/encryption] refresh: no client available");
        }
    });

    refresh_row.add_suffix(&refresh_btn);
    keys_group.add(&refresh_row);

    page.add(&keys_group);

    // Fetch the current key package count on page construction so the row
    // is populated as soon as the window opens.
    if let Some(client) = crate::settings::get_client() {
        client.fetch_key_package_count();
    }

    // Wire the key_count_row subtitle to be updated when
    // DataMessage::KeyPackageCountLoaded arrives. Because we can't hook
    // into the UiMessage channel from inside a preferences page (that
    // channel is owned by main.rs), we store the row in a glib idle
    // callback that polls from the static state updated by handle_ui_message.
    //
    // The simpler path: expose a standalone function that app.rs can call
    // when it receives KeyPackageCountLoaded — see update_key_count() below.
    // app.rs matches on that variant and calls it.
    // We stash the row reference in module-level storage for that purpose.
    KEY_COUNT_ROW.with(|cell| {
        *cell.borrow_mut() = Some(key_count_row.clone());
    });
    WARNING_ROW.with(|cell| {
        *cell.borrow_mut() = Some(warning_row.clone());
    });

    crate::testid::wrap_page_with_heading(enc::TITLE, ids::PAGE_HEADING, &page)
}

// ---------------------------------------------------------------------------
// Thread-local widget references updated by app.rs dispatch
// ---------------------------------------------------------------------------

use std::cell::RefCell;

thread_local! {
    static KEY_COUNT_ROW: RefCell<Option<adw::ActionRow>> = const { RefCell::new(None) };
    static WARNING_ROW: RefCell<Option<adw::ActionRow>> = const { RefCell::new(None) };
}

/// Called by `app.rs` when `DataMessage::KeyPackageCountLoaded` arrives.
pub fn update_key_count(count: u32) {
    KEY_COUNT_ROW.with(|cell| {
        if let Some(row) = cell.borrow().as_ref() {
            row.set_subtitle(&count.to_string());
        }
    });
    WARNING_ROW.with(|cell| {
        if let Some(row) = cell.borrow().as_ref() {
            row.set_visible(count < LOW_KEY_THRESHOLD);
        }
    });
}
