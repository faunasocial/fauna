use adw::prelude::*;
use fauna_ui_ids as ids;

use crate::i18n::strings::common;
use crate::i18n::strings::settings::encryption_page as enc;

/// Build the "Encryption" preferences page.
pub fn build_encryption_page() -> gtk::Box {
    let page = adw::PreferencesPage::builder()
        .title(enc::TITLE)
        .icon_name("dialog-password-symbolic")
        .build();

    // --- Low-key warning row (hidden until count is fetched) ---
    let warning_group = adw::PreferencesGroup::new();
    // error-message — the page-level error (`settings.md` § Errors): a failed
    // count read says why here, on this page alone, never on the app-wide
    // banner (tui's `encryption_error`). Hidden until a read fails.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    warning_group.add(&error_label);
    let warning_row = adw::ActionRow::builder()
        .title(enc::LOW_KEY_WARNING_TITLE)
        .subtitle(enc::LOW_KEY_WARNING_SUBTITLE)
        .build();
    warning_row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
    // The warning names the count it read (`update_key_count` sets its
    // subtitle), so `get_text` on it reads the same number the count row states.
    crate::testid::set_test_id(&warning_row, ids::ENCRYPTION_KEYPACKAGE_LOW_WARNING);
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
    // A caption/value row: `get_text` reads the subtitle — the bare count, or
    // the loading copy before the first read lands.
    crate::testid::set_test_id(&key_count_row, ids::ENCRYPTION_KEYPACKAGE_COUNT);
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
    // is a Read that gates nothing (tui's `Action::RefreshKeyPackages`).
    crate::testid::set_test_id(&refresh_btn, ids::ENCRYPTION_KEYPACKAGE_REFRESH_BUTTON);
    crate::offline_gate::declare_wire_kind(&refresh_btn, "fauna.conversations.keypackage.upload");

    let key_count_row_ref = key_count_row.clone();
    let warning_row_ref = warning_row.clone();

    refresh_btn.connect_clicked(move |_| {
        if crate::settings::get_client().is_none() {
            tracing::error!("[settings/encryption] refresh: no client available");
            return;
        }
        // Replenish the one-time pool through the durable manager surface
        // (mint on the session engine + notify→autosave), the SAME path login
        // and web drive — never a raw engine mint whose fresh private init
        // keys a later provider swap would wipe (`devices.md` § Cross-device
        // MLS group-state sync).
        let replenished = crate::conversations::conv_backend::replenish_key_packages();
        key_count_row_ref.set_subtitle(common::REFRESHING);
        warning_row_ref.set_visible(false);
        // Re-read the count only once the publish has landed — the result
        // comes back via `UiMessage` to `update_key_count`.
        gtk::glib::spawn_future_local(async move {
            if let Some(replenished) = replenished {
                let _ = replenished.recv().await;
            }
            if let Some(client) = crate::settings::get_client() {
                client.fetch_key_package_count();
            }
        });
    });

    refresh_row.add_suffix(&refresh_btn);
    keys_group.add(&refresh_row);

    page.add(&keys_group);

    // Fetch the current key package count on page construction so the row
    // is populated as soon as the window opens; `refresh_on_visit` re-reads it
    // on every later visit.
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
    ERROR_LABEL.with(|cell| {
        *cell.borrow_mut() = Some(error_label.clone());
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
    static ERROR_LABEL: RefCell<Option<gtk::Label>> = const { RefCell::new(None) };
}

/// Paint (or, with `None`, clear) the page's own `error-message`.
fn render_page_error(error: Option<&str>) {
    ERROR_LABEL.with(|cell| {
        if let Some(label) = cell.borrow().as_ref() {
            super::render_error_label(label, error);
        }
    });
}

/// Called by `app.rs` when `DataMessage::KeyPackageCountFailed` arrives: the
/// page says why, and the count row keeps the last count it read (tui's
/// `a_failed_read_surfaces_the_message_and_leaves_the_prior_count`).
pub fn show_count_error(error: &str) {
    render_page_error(Some(error));
}

/// Called by `app.rs` when `DataMessage::KeyPackageCountLoaded` arrives.
pub fn update_key_count(count: u32) {
    render_page_error(None);
    KEY_COUNT_ROW.with(|cell| {
        if let Some(row) = cell.borrow().as_ref() {
            row.set_subtitle(&count.to_string());
        }
    });
    WARNING_ROW.with(|cell| {
        if let Some(row) = cell.borrow().as_ref() {
            row.set_subtitle(&enc::low_key_warning_body(&count.to_string()));
            row.set_visible(fauna_conversations::keypackages_low(u64::from(count)));
        }
    });
}

/// The settings shell's nav edge onto this page (`settings_shell.rs`): every
/// visit reads the nest's count afresh — a read only, never a mint
/// (`direct-messages.md` § Key Package Management). The row drops back to the
/// loading copy and the warning hides until the read lands, so neither can
/// state the previous visit's number as this one's (tui hydrates the page on
/// the same edge).
pub fn refresh_on_visit() {
    KEY_COUNT_ROW.with(|cell| {
        if let Some(row) = cell.borrow().as_ref() {
            row.set_subtitle(common::LOADING);
        }
    });
    WARNING_ROW.with(|cell| {
        if let Some(row) = cell.borrow().as_ref() {
            row.set_visible(false);
        }
    });
    if let Some(client) = crate::settings::get_client() {
        client.fetch_key_package_count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failed count read paints the page's own `error-message` and a later
    /// good read clears it (tui's `encryption_error` pair), so the failure
    /// never needs the app-wide banner — which a read fired at the settings
    /// shell's mount would leave standing over every other page.
    #[test]
    fn a_failed_count_read_says_so_on_this_page_and_a_good_read_clears_it() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let page = build_encryption_page();
            let label = crate::testid::find_by_test_id(&page, "error-message")
                .expect("the Encryption page carries its own error-message")
                .downcast::<gtk::Label>()
                .expect("error-message is a label");
            assert!(!label.is_visible(), "no error before a read fails");

            show_count_error("connection lost");
            assert!(label.is_visible());
            assert_eq!(label.text(), "connection lost");

            update_key_count(20);
            assert!(!label.is_visible(), "a good read clears the page's error");
        });
    }
}
