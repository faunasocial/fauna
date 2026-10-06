//! Add-participant modal overlay — builds a reusable `adw::MessageDialog`
//! wrapping a `RecipientPicker` plus an "Add" confirm button (id
//! `add-participant-confirm`, matching the e2e contract and the Windows
//! analog). The dialog has no behavior of its own: `ConversationsDetail`
//! owns the returned dialog + picker, drives the picker off
//! `snapshot.add_participant`, and connects the responses to
//! `manager.confirm_add_participant()` / `manager.cancel_add_participant()`.
//!
//! Still `adw::MessageDialog`, not `adw::AlertDialog` — libadwaita's `v1_5`
//! feature has been enabled since 2026-05-31, so this is a
//! deliberate scope cut, not a version gate. This site sits outside the seam
//! `crate::confirm_dialog` migrated — it isn't a
//! destructive confirm — and wants its own pass; `ConversationDetail`'s
//! `add_participant_dialog` field (`views/conversations/detail.rs`) carries
//! this same type.

use adw::prelude::*;
use fauna_ui_ids as ids;
use gtk::glib;

use super::recipient_picker::RecipientPicker;

/// Build the add-participant dialog and its picker. `on_input` /
/// `on_accept` are forwarded to the `RecipientPicker`; the caller wires
/// them to the manager (`set_add_participant_recipient_input` /
/// `accept_current_recipient_chip`). `on_add` / `on_cancel` fire on the
/// dialog's `add` / non-`add` responses respectively. The dialog is built
/// hidden; the caller `present()`s / `close()`s it from `render`.
#[allow(deprecated)]
pub fn build(
    parent: &impl IsA<gtk::Widget>,
    on_input: impl Fn(String) + 'static,
    on_accept: impl Fn() + 'static,
    on_add: impl Fn() + 'static,
    on_cancel: impl Fn() + 'static,
) -> (adw::MessageDialog, RecipientPicker) {
    let toplevel = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());

    let picker = RecipientPicker::new(on_input, on_accept);

    let dialog = adw::MessageDialog::new(
        toplevel.as_ref(),
        Some(crate::i18n::strings::conversations::unified::THREAD_ADD_PARTICIPANT),
        None,
    );
    dialog.set_extra_child(Some(&picker.root));
    dialog.add_response("cancel", crate::i18n::strings::common::CANCEL);
    dialog.add_response("add", crate::i18n::strings::common::ADD);
    dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("add"));
    dialog.set_close_response("cancel");

    // Tag the Add button with the e2e test id once the dialog's button
    // tree is materialised (tag_response_button walks the dialog tree, so
    // it has to run after the first `present()`/`map` — see dialog_helpers).
    // (No offline-gate declaration here: there is no persistent widget
    // reference to this button. The gate for this ceremony declares on the
    // ENTRY that opens it — `thread_header.rs`'s `add_participant_btn` —
    // re-decided every render off the currently-viewed thread's rail/flavor,
    // since unlike rename this ceremony's kind is not constant.)
    let dialog_for_id = dialog.clone();
    dialog.connect_map(move |_| {
        crate::testid::tag_response_button(
            dialog_for_id.upcast_ref::<gtk::Widget>(),
            crate::i18n::strings::common::ADD,
            ids::ADD_PARTICIPANT_CONFIRM,
        );
    });

    let on_add = std::rc::Rc::new(on_add);
    let on_cancel = std::rc::Rc::new(on_cancel);
    dialog.connect_response(None, move |_dlg, response| {
        if response == "add" {
            on_add();
        } else {
            on_cancel();
        }
        // The dialog is hidden by `ConversationsDetail::render` once the
        // manager clears `snapshot.add_participant`; don't close here too.
    });

    let _ = glib::user_data_dir(); // keep glib import live for future hooks
    (dialog, picker)
}
