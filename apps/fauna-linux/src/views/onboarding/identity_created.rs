use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_onboarding_machine::OnboardingMachine;
use gtk::glib;

use crate::i18n::strings::onboarding::identity_created as strings;

/// Build the identity-created step: displays the freshly-generated secret key
/// (read from `m.generated_secret()`), a Continue button advancing the
/// machine, and a Back button.
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

    let desc = gtk::Label::new(Some(strings::DESC));
    desc.set_wrap(true);
    desc.add_css_class("fauna-muted");
    desc.set_halign(gtk::Align::Start);
    content.append(&desc);

    // Secret key label.
    let key_header = gtk::Label::new(Some(strings::SECRET_KEY_LABEL));
    key_header.set_halign(gtk::Align::Start);
    key_header.set_margin_top(8);
    content.append(&key_header);

    let secret_label = gtk::Label::new(None);
    secret_label.set_selectable(true);
    secret_label.set_wrap(true);
    secret_label.set_wrap_mode(gtk::pango::WrapMode::Char);
    secret_label.add_css_class("monospace");
    secret_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&secret_label, ids::SECRET_KEY_DISPLAY);
    content.append(&secret_label);

    {
        let label_ref = secret_label.clone();
        let copy_btn = gtk::Button::with_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
        copy_btn.add_css_class("flat");
        copy_btn.set_halign(gtk::Align::Start);
        crate::testid::set_test_id(&copy_btn, ids::SECRET_KEY_COPY_BTN);
        copy_btn.connect_clicked(move |btn| {
            let text = label_ref.text().to_string();
            if !text.is_empty() {
                crate::clipboard::copy_text(&text);
                btn.set_label(crate::i18n::strings::settings::account_page::COPIED_CLIPBOARD);
                let btn_weak = btn.downgrade();
                glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                    if let Some(btn) = btn_weak.upgrade() {
                        btn.set_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
                    }
                });
            }
        });
        content.append(&copy_btn);
    }

    // Warning text.
    let warning = gtk::Label::new(Some(strings::WARNING));
    warning.set_wrap(true);
    warning.add_css_class("fauna-muted");
    warning.set_halign(gtk::Align::Start);
    warning.set_margin_top(8);
    content.append(&warning);

    // Action row: Back + Continue.
    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    action_row.set_halign(gtk::Align::End);
    action_row.set_margin_top(12);

    let back_btn = gtk::Button::with_label(crate::i18n::strings::common::BACK);
    crate::testid::set_test_id(&back_btn, ids::IDENTITY_CREATED_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| {
            m.back();
        });
    }
    action_row.append(&back_btn);

    let continue_btn = gtk::Button::with_label(strings::CONTINUE);
    continue_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&continue_btn, ids::IDENTITY_CONTINUE_BUTTON);
    {
        let m = m.clone();
        continue_btn.connect_clicked(move |_| {
            // The machine validates `generated_secret` is set; errors surface
            // through the orchestrator's error banner via `m.error_message()`.
            // On success, commit the secret immediately so a crash before
            // complete-login can resume at HandleEntry — moment 1, owned by
            // `super::commit_confirmed_identity` (which owns the append split
            // and the read-back this generated secret most needs: it exists
            // nowhere else).
            match m.confirm_generated_identity() {
                Ok(secret) => super::commit_confirmed_identity(&secret, append),
                Err(_) => {
                    // Error already surfaced via m.error_message().
                }
            }
        });
    }
    action_row.append(&continue_btn);

    content.append(&action_row);

    page.append(&content);

    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        let secret = m.generated_secret().unwrap_or_default();
        secret_label.set_text(&secret);
    });
    (page, refresh)
}
