use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_onboarding_machine::OnboardingMachine;

use crate::i18n::strings::onboarding::identity_choice as strings;

/// Build the identity choice step: Create New, Import, restore from a
/// recovery kit, or recover a lost box.
///
/// Conforms to the universal handle-first signature
/// `build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>)`.
/// The refresh closure is a no-op — this page is purely button-driven and
/// doesn't render any machine state.
pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.set_vexpand(true);
    page.set_valign(gtk::Align::Center);
    page.set_halign(gtk::Align::Center);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(32);
    content.set_margin_bottom(32);
    content.set_margin_start(48);
    content.set_margin_end(48);
    content.set_halign(gtk::Align::Center);

    let title = gtk::Label::new(Some(strings::TITLE));
    title.add_css_class("title-1");
    title.add_css_class("fauna-accent");
    content.append(&title);

    let subtitle = gtk::Label::new(Some(strings::SUBTITLE));
    subtitle.add_css_class("fauna-muted");
    subtitle.set_wrap(true);
    subtitle.set_margin_bottom(24);
    content.append(&subtitle);

    let create_btn = gtk::Button::with_label(strings::CREATE_NEW);
    create_btn.add_css_class("suggested-action");
    create_btn.add_css_class("pill");
    create_btn.set_hexpand(true);
    crate::testid::set_test_id(&create_btn, ids::CREATE_IDENTITY_BUTTON);
    {
        let m = m.clone();
        create_btn.connect_clicked(move |_| {
            m.begin_create_identity();
        });
    }
    content.append(&create_btn);

    let import_btn = gtk::Button::with_label(strings::IMPORT_EXISTING);
    import_btn.add_css_class("pill");
    import_btn.set_hexpand(true);
    crate::testid::set_test_id(&import_btn, ids::IMPORT_IDENTITY_BUTTON);
    {
        let m = m.clone();
        import_btn.connect_clicked(move |_| {
            m.begin_import_identity();
        });
    }
    content.append(&import_btn);

    // The phrase-only IDENTITY restore (`onboarding.md` § 1 Identity) —
    // distinct from the lost-box NEST recovery below it.
    let restore_btn = gtk::Button::with_label(strings::RESTORE_FROM_RECOVERY_KIT);
    restore_btn.add_css_class("pill");
    restore_btn.set_hexpand(true);
    crate::testid::set_test_id(&restore_btn, ids::RESTORE_FROM_RECOVERY_KIT_BUTTON);
    {
        let m = m.clone();
        restore_btn.connect_clicked(move |_| {
            m.begin_recovery_entry();
        });
    }
    content.append(&restore_btn);

    // Fresh-client recovery entry (box-recovery.md § Recovery UI (step 4)).
    // Routes through identity_import with recovery intent (the identity must be
    // loaded to open the account's deployment-seed custody map), then lands on
    // nest_recovery.
    let recover_btn = gtk::Button::with_label(strings::RECOVER_LOST_BOX);
    recover_btn.add_css_class("flat");
    recover_btn.set_hexpand(true);
    crate::testid::set_test_id(&recover_btn, ids::RECOVER_LOST_BOX_BUTTON);
    {
        let m = m.clone();
        recover_btn.connect_clicked(move |_| {
            m.begin_recover_lost_box();
        });
    }
    content.append(&recover_btn);

    // Error label (hidden by default).
    let error_label = gtk::Label::new(None);
    error_label.set_visible(false);
    error_label.set_wrap(true);
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    content.append(&error_label);

    let paint_residue = build_residue_view(&content);

    page.append(&content);

    // No machine-derived UI on this page; the residue view is the app's own
    // state, re-read on every tick so a sign-out, a launch re-check and a
    // Remove Again all reach it the same way.
    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        let _ = &m;
        paint_residue();
    });
    (page, refresh)
}

/// The optional `sign-out-residue` view — what the last sign-out's erase could
/// not remove, with Remove Again beside it (`account-scoping.md` § Erasure
/// follows scope → *the residue surface*). Hidden while nothing owes work.
/// Returns the painter; the button repaints itself after its retry rather than
/// waiting for the next machine tick, which a retry does not cause.
fn build_residue_view(content: &gtk::Box) -> Rc<dyn Fn()> {
    let residue = gtk::Box::new(gtk::Orientation::Vertical, 8);
    residue.set_margin_top(16);
    residue.set_visible(false);
    crate::testid::set_test_id(&residue, ids::SIGN_OUT_RESIDUE);

    let message = gtk::Label::new(None);
    message.set_wrap(true);
    message.add_css_class("error-banner");
    crate::testid::set_test_id(&message, ids::SIGN_OUT_RESIDUE_MESSAGE);
    residue.append(&message);

    let retry = gtk::Button::with_label(fauna_i18n::strings::settings::SIGN_OUT_RESIDUE_RETRY);
    retry.add_css_class("pill");
    retry.set_halign(gtk::Align::Center);
    crate::testid::set_test_id(&retry, ids::SIGN_OUT_RESIDUE_RETRY_BUTTON);
    residue.append(&retry);

    content.append(&residue);

    let paint: Rc<dyn Fn()> = Rc::new(move || match crate::account_scope::sign_out_residue() {
        Some(surface) => {
            message.set_text(&surface.line);
            residue.set_visible(true);
        }
        None => residue.set_visible(false),
    });
    {
        let paint = Rc::clone(&paint);
        retry.connect_clicked(move |_| {
            crate::account_scope::retry_residue();
            paint();
        });
    }
    paint();
    paint
}
