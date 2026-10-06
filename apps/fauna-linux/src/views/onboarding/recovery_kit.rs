//! The recovery-kit offer (`onboarding.md` § 1 Identity), right after the
//! identity secret is confirmed — linux's leg of the screen tui led
//! (`apps/fauna-tui/src/wizard/recovery_kit.rs`).
//!
//! The page **mints and displays only**: no nest exists at this position, so
//! registration + escrow run at the wizard's signed-in handoff
//! (`FaunaClient::register_deferred_recovery_kit`, over the shared
//! `fauna_client_recovery::ceremony::register_deferred_kit`). That is why
//! `recovery-kit-escrow-status` renders exactly one state here — the deferred
//! line, never "protected". The machine routes here only because
//! `machine_glue::make_machine` declares `set_renders_recovery_kit(true)`; the
//! two land together.
//!
//! The display is the bare 64-hex (what a user copies onto paper); the QR and
//! the copy button both carry the machine's one `fauna://recovery` URI
//! (`OnboardingMachine::recovery_kit_uri`, `identity-succession.md` § The
//! RecoveryKey — *Which encoding each affordance carries*).
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_core::qr_matrix::QrMatrix;
use fauna_onboarding_machine::OnboardingMachine;

use crate::i18n::strings::common;
use crate::i18n::strings::onboarding::recovery_kit as strings;

/// Side of the drawn QR, in px — the Settings kit screen's box.
const QR_SIZE_PX: i32 = 220;

pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
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
    crate::testid::set_test_id(&desc, ids::RECOVERY_KIT_DESCRIPTION);
    content.append(&desc);

    let secret_label = gtk::Label::new(Some(strings::NOT_MINTED));
    secret_label.set_selectable(true);
    secret_label.set_wrap(true);
    secret_label.set_wrap_mode(gtk::pango::WrapMode::Char);
    secret_label.add_css_class("monospace");
    secret_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&secret_label, ids::RECOVERY_KIT_SECRET_DISPLAY);
    content.append(&secret_label);

    let copy_btn = gtk::Button::with_label(common::COPY);
    copy_btn.add_css_class("flat");
    copy_btn.set_halign(gtk::Align::Start);
    copy_btn.set_sensitive(false);
    crate::testid::set_test_id(&copy_btn, ids::RECOVERY_KIT_SECRET_COPY_BTN);
    {
        let m = m.clone();
        copy_btn.connect_clicked(move |_| {
            // Read at click time, never cached: the URI lives exactly as long
            // as the machine holds the pending root.
            if let Some(uri) = m.recovery_kit_uri() {
                crate::clipboard::copy_text(&uri);
            }
        });
    }
    content.append(&copy_btn);

    let matrix: Rc<RefCell<Option<QrMatrix>>> = Rc::new(RefCell::new(None));
    let qr_area = gtk::DrawingArea::builder()
        .content_width(QR_SIZE_PX)
        .content_height(QR_SIZE_PX)
        .build();
    qr_area.set_halign(gtk::Align::Start);
    qr_area.set_visible(false);
    crate::testid::set_test_id(&qr_area, ids::RECOVERY_KIT_QR);
    qr_area.set_draw_func({
        let matrix = matrix.clone();
        move |_area, cr, width, height| {
            if let Some(mx) = matrix.borrow().as_ref() {
                crate::qr_widget::draw_matrix(cr, mx, width, height);
            }
        }
    });
    content.append(&qr_area);

    let escrow_status = gtk::Label::new(Some(strings::ESCROW_DEFERRED));
    escrow_status.set_wrap(true);
    escrow_status.add_css_class("fauna-muted");
    escrow_status.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&escrow_status, ids::RECOVERY_KIT_ESCROW_STATUS);
    content.append(&escrow_status);

    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    action_row.set_halign(gtk::Align::End);
    action_row.set_margin_top(12);

    // Skip is one click and never blocks onboarding; the minted root is dropped
    // so nothing registers at handoff, and Settings' never-created warning
    // tells the truth.
    let skip_btn = gtk::Button::with_label(strings::SKIP);
    crate::testid::set_test_id(&skip_btn, ids::RECOVERY_KIT_SKIP_BUTTON);
    {
        let m = m.clone();
        skip_btn.connect_clicked(move |_| m.skip_recovery_kit());
    }
    action_row.append(&skip_btn);

    let confirm_btn = gtk::Button::with_label(strings::CONFIRM);
    confirm_btn.add_css_class("suggested-action");
    confirm_btn.set_sensitive(false);
    crate::testid::set_test_id(&confirm_btn, ids::RECOVERY_KIT_CONFIRM_BUTTON);
    {
        let m = m.clone();
        confirm_btn.connect_clicked(move |_| m.confirm_recovery_kit());
    }
    action_row.append(&confirm_btn);
    content.append(&action_row);

    let error_label = gtk::Label::new(None);
    error_label.set_visible(false);
    error_label.set_wrap(true);
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    content.append(&error_label);

    page.append(&content);

    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        let secret = m.recovery_kit_secret_hex();
        let minted = secret.is_some();
        secret_label.set_text(secret.as_deref().unwrap_or(strings::NOT_MINTED));
        let mx = m
            .recovery_kit_uri()
            .and_then(|uri| fauna_core::qr_matrix::qr_matrix(&uri).ok());
        qr_area.set_visible(mx.is_some());
        matrix.replace(mx);
        qr_area.queue_draw();
        copy_btn.set_sensitive(minted);
        confirm_btn.set_sensitive(minted);
        let msg = m.error_message().filter(|msg| !msg.is_empty());
        crate::settings::render_error_label(&error_label, msg.as_deref());
    });
    (page, refresh)
}
