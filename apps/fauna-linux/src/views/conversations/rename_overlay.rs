//! Rename-thread modal overlay.
//!
//! Mirror of the Windows `RenameOverlay` border block in `ConversationsPage.xaml`
//! (lines 209+). On Linux we use `adw::MessageDialog` — not yet migrated to
//! `adw::AlertDialog`: the crate's `v1_5` feature has been enabled since
//! 2026-05-31, so this is a deliberate scope cut, not a version
//! gate. This site sits outside the seam `crate::confirm_dialog` migrated — it isn't a destructive confirm — and wants its
//! own pass.
//!
//! IDs: `thread-rename-field` on the entry, `thread-rename-confirm` on the
//! Save button. The cancel button has the default MessageDialog id; the
//! e2e tests don't address it.

use adw::prelude::*;
use fauna_ui_ids as ids;

/// Show the rename overlay over the toplevel window of `parent`. Calls
/// `on_save(new_label)` on Save click iff the field is non-empty.
#[allow(deprecated)]
pub fn show<F: Fn(String) + 'static>(
    parent: &impl IsA<gtk::Widget>,
    current_label: &str,
    on_save: F,
) {
    let toplevel = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());

    let dialog = adw::MessageDialog::new(
        toplevel.as_ref(),
        Some(crate::i18n::strings::conversations::unified::THREAD_RENAME),
        None,
    );

    let entry = gtk::Entry::new();
    entry.set_text(current_label);
    entry.set_placeholder_text(Some(
        crate::i18n::strings::conversations::unified::THREAD_RENAME_PLACEHOLDER,
    ));
    crate::testid::set_test_id(&entry, ids::THREAD_RENAME_FIELD);
    entry.select_region(0, -1);

    dialog.set_extra_child(Some(&entry));

    dialog.add_response("cancel", crate::i18n::strings::common::CANCEL);
    dialog.add_response("save", crate::i18n::strings::common::SAVE);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");

    // MessageDialog response buttons get materialized inside the dialog
    // at present-time; tag the Save one with the e2e id once it appears.
    // (No offline-gate declaration here: there is no persistent widget
    // reference to this button. The gate for this ceremony declares on the
    // ENTRY that opens it — `thread_header.rs`'s `rename_btn`.)
    let dialog_for_id = dialog.clone();
    glib::idle_add_local_once(move || {
        crate::testid::tag_response_button(
            dialog_for_id.upcast_ref::<gtk::Widget>(),
            crate::i18n::strings::common::SAVE,
            ids::THREAD_RENAME_CONFIRM,
        );
    });

    let entry_for_save = entry.clone();
    dialog.connect_response(None, move |dlg, response| {
        if response == "save" {
            let text = entry_for_save.text().to_string();
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                on_save(trimmed.to_string());
            }
        }
        dlg.close();
    });

    dialog.present();
}

use gtk::glib;
