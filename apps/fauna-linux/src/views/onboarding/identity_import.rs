use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_onboarding_machine::OnboardingMachine;

use crate::i18n::strings::errors;
use crate::i18n::strings::onboarding::identity_import as strings;

/// Build the identity-import step: paste a 64-char hex secret (or a
/// `fauna://identity?secret=&handle=` URI). The shared `fauna_core::identity_qr`
/// parser validates the format first (surfacing the localized `invalid_secret`
/// without touching the machine); a recognized payload's optional handle pre-fills
/// the next step before the machine validates + persists the secret. Machine-level
/// errors still surface through `m.error_message()`, which the page-local error
/// label re-renders on every refresh tick.
///
/// `append` selects the moment-1 store view — see
/// [`super::commit_confirmed_identity`], which owns the split.
pub fn build(m: Arc<OnboardingMachine>, append: bool) -> (gtk::Box, Rc<dyn Fn()>) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.set_vexpand(true);
    page.set_valign(gtk::Align::Center);
    page.set_halign(gtk::Align::Center);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(32);
    content.set_margin_bottom(32);
    content.set_margin_start(48);
    content.set_margin_end(48);
    content.set_width_request(400);

    let title = gtk::Label::new(Some(strings::TITLE));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    content.append(&title);

    let desc = gtk::Label::new(Some(strings::PASTE_SUBTITLE));
    desc.set_wrap(true);
    desc.add_css_class("fauna-muted");
    desc.set_halign(gtk::Align::Start);
    content.append(&desc);

    // Paste field.
    let paste_label = gtk::Label::new(Some(strings::PASTE_LABEL));
    paste_label.set_halign(gtk::Align::Start);
    paste_label.set_margin_top(8);
    content.append(&paste_label);

    let paste_entry = gtk::Entry::builder()
        .placeholder_text(strings::PASTE_PLACEHOLDER)
        .hexpand(true)
        .visibility(false)
        .build();
    crate::testid::set_test_id(&paste_entry, ids::PASTE_SECRET_FIELD);
    content.append(&paste_entry);

    // Action row: Back + Import.
    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    action_row.set_halign(gtk::Align::End);
    action_row.set_margin_top(12);

    let back_btn = gtk::Button::with_label(crate::i18n::strings::common::BACK);
    crate::testid::set_test_id(&back_btn, ids::IDENTITY_IMPORT_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| {
            m.back();
        });
    }
    action_row.append(&back_btn);

    let import_btn = gtk::Button::with_label(strings::IMPORT);
    import_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&import_btn, ids::IMPORT_SUBMIT_BUTTON);
    action_row.append(&import_btn);

    content.append(&action_row);

    // The submit is painted from the shared draft answer as the secret is
    // pasted (`ui/README.md` § Offer only what is possible, rule 4's form
    // case): blank or malformed → disabled, its reason on the tooltip.
    let paint_import = {
        let import_btn = import_btn.clone();
        move |raw: &str| {
            crate::possible::paint(&import_btn, None, import_answer(raw));
        }
    };
    paint_import("");
    {
        let paint_import = paint_import.clone();
        paste_entry.connect_changed(move |e| paint_import(&e.text()));
    }

    // Error label (hidden by default, populated from `m.error_message()` in refresh).
    let error_label = gtk::Label::new(None);
    error_label.set_visible(false);
    error_label.set_wrap(true);
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    content.append(&error_label);

    {
        let m = m.clone();
        let paste_entry = paste_entry.clone();
        let error_label = error_label.clone();
        import_btn.connect_clicked(move |_| {
            let raw = paste_entry.text().to_string();
            // The press asks the answer it was painted from again (rule 6);
            // a blocked draft's reason goes on `error-message`, nothing sent.
            if crate::possible::refused_at_press(&import_answer(&raw), &error_label) {
                return;
            }
            // Route the pasted field through the shared union parser
            // (`fauna_core::identity_qr`) — the same grammar web/android/iOS/macOS
            // adopted (bare 64-hex secret · `fauna://identity?secret=&handle=`
            // query form · colon form) instead of hand-rolling it here
            // (priority #2/#4). The draft answer above already refused a paste
            // the parser rejects, with its `invalid_secret` line.
            let Some(parsed) = fauna_core::identity_qr::parse_import_input(raw.trim()) else {
                return;
            };
            // Pre-fill the handle step when the payload carried one (QR payload is
            // `(identity, handle)`; onboarding.md §1 Identity → identity_import).
            if let Some(handle) = parsed.handle.as_ref().filter(|h| !h.is_empty()) {
                m.set_current_handle(handle.clone());
            }
            match m.confirm_imported_identity(parsed.secret) {
                Ok(secret) => {
                    error_label.set_visible(false);
                    // Commit the secret immediately so a crash before
                    // complete-login can resume at HandleEntry — moment 1,
                    // owned by `super::commit_confirmed_identity`.
                    super::commit_confirmed_identity(&m, &secret, append);
                }
                Err(_) => {
                    // The machine sets error_message on failure; we still set
                    // a sensible immediate label in case error_message hasn't
                    // been populated by this transition path.
                    let msg = m
                        .error_message()
                        .unwrap_or_else(|| errors::SECRET_KEY_INVALID.to_string());
                    crate::settings::render_error_label(&error_label, Some(&msg));
                }
            }
        });
    }

    page.append(&content);

    // Entry ticks only fire when the machine mutates while this page is the
    // visible step — and on this page, the only mutations (confirm_imported /
    // back) immediately navigate away. So a refresh tick on identity_import
    // means we just landed here, and any leftover text in `paste_entry` is
    // stale state from a previous visit (e.g. across a `m.reset()` between
    // E2E tests). Clear it so `type_text` from the next test doesn't
    // accumulate onto a 64-hex prefix.
    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        if !paste_entry.text().is_empty() && m.error_message().is_none() {
            paste_entry.set_text("");
        }
        paint_import(&paste_entry.text());
        let msg = m.error_message().filter(|msg| !msg.is_empty());
        crate::settings::render_error_label(&error_label, msg.as_deref());
    });
    (page, refresh)
}

/// `import-submit-button`'s answer: the shared draft predicate over the paste
/// as the press reads it.
fn import_answer(raw: &str) -> fauna_core::possibility::Possibility {
    fauna_onboarding_machine::identity_import_possibility(raw.trim().to_string())
}
