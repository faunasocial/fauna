//! Stage 3 of handle-first onboarding: invite-request + OOB code entry.
//!
//! ui.yaml IDs: `page-heading`, `invite-request-submit-button`,
//! `invite-request-status`, `invite-code-input`, `invite-code-check-button`,
//! `invite-code-status`, `invite-request-continue-button`,
//! `invite-request-back-button`, `error-message`.
//! Optional: `invite-request-recheck-button` (visible iff
//! `snapshot.recheck_visible`); `invite-code-supervised-notice` (visible iff the
//! checked OOB code carries a guardian — `family-safety.md` § Wire & data shape).
//!
//! Layout — two rows on one page:
//!   Top row:  submit + status + recheck (PendingReview only)
//!   Bottom row: code input + check + status
//!
//! Replaces the old single-row form that called `submit_invite_request(message)`.

use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Button, Entry, Label, Orientation};

use crate::testid::set_test_id;
use fauna_onboarding_machine::{OnboardingMachine, snapshots::invite_request::OobCodeState};

use crate::async_helper;
use crate::i18n::resolve_key as resolve;

pub fn build(m: Arc<OnboardingMachine>, append: bool) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .build();
    root.set_margin_top(32);
    root.set_margin_bottom(32);
    root.set_margin_start(48);
    root.set_margin_end(48);

    // -- Title --
    let title = Label::new(Some(&resolve("onboarding.invite_request.title")));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    set_test_id(&title, ids::PAGE_HEADING);
    root.append(&title);

    // ── Top row: Submit + Status + Recheck ──────────────────────────────
    let top_row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .build();
    top_row.set_margin_top(8);

    let submit_btn = Button::with_label(&resolve("onboarding.invite.requestButton"));
    set_test_id(&submit_btn, ids::INVITE_REQUEST_SUBMIT_BUTTON);
    {
        let m = m.clone();
        submit_btn.connect_clicked(move |_| {
            let m = m.clone();
            let m_done = m.clone();
            async_helper::run_on_tokio(
                async move { m.wizard_submit_invite_request().await },
                move |_step| {
                    // Snapshot state changes drive the UI — discard returned step.
                    //
                    // This return is "the only write moment" for the resume slot
                    // (`onboarding.md` § 3 Persistence callouts). It used to ride
                    // the retired `InviteSubmitted` exit; the journey no longer
                    // exits, so the write happens here.
                    //
                    // The poll is NOT armed here — it is armed once per entry to
                    // this page (see `start_pending_invite_poll`'s doc), so that
                    // the same-session submit and the relaunch hydration share
                    // one timer instead of racing two.
                    super::persist_pending_invite_slot(&m_done, append);
                },
            );
        });
    }
    top_row.append(&submit_btn);

    let invite_status = Label::new(None);
    invite_status.set_halign(gtk::Align::Start);
    invite_status.set_hexpand(true);
    set_test_id(&invite_status, ids::INVITE_REQUEST_STATUS);
    top_row.append(&invite_status);

    let recheck_btn = Button::with_label(&resolve("onboarding.invite.recheckButton"));
    set_test_id(&recheck_btn, ids::INVITE_REQUEST_RECHECK_BUTTON);
    recheck_btn.set_visible(false); // shown only during PendingReview (driven by snapshot)
    {
        let m = m.clone();
        recheck_btn.connect_clicked(move |_| {
            let m = m.clone();
            async_helper::run_on_tokio(async move { m.recheck_invite_status().await }, |_step| {
                // Discard returned step; snapshot state changes drive the UI.
            });
        });
    }
    top_row.append(&recheck_btn);
    root.append(&top_row);

    // ── Bottom row: Code input + Check + Status ──────────────────────────
    let bottom_row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .build();
    bottom_row.set_margin_top(8);

    let code_input = Entry::builder()
        .placeholder_text(resolve("onboarding.oob_code.placeholder"))
        .hexpand(true)
        .build();
    set_test_id(&code_input, ids::INVITE_CODE_INPUT);
    bottom_row.append(&code_input);

    let code_check_btn = Button::with_label(crate::i18n::strings::common::CHECK);
    set_test_id(&code_check_btn, ids::INVITE_CODE_CHECK_BUTTON);
    {
        let m = m.clone();
        let code_input = code_input.clone();
        code_check_btn.connect_clicked(move |_| {
            let code = code_input.text().to_string();
            let m = m.clone();
            async_helper::run_on_tokio(async move { m.verify_oob_invite_code(code).await }, |_| {});
        });
    }
    bottom_row.append(&code_check_btn);

    let code_status = Label::new(None);
    code_status.set_halign(gtk::Align::Start);
    set_test_id(&code_status, ids::INVITE_CODE_STATUS);
    bottom_row.append(&code_status);
    root.append(&bottom_row);

    // `invite-code-supervised-notice` — "this account will be supervised by X",
    // rendered BEFORE redemption when the checked out-of-band code carries a
    // guardian designation (`family-safety.md` § Wire & data shape: the additive
    // `supervised_by` on the `fauna.account.invite_code.verify` reply, surfaced by
    // the shared machine as `OobCodeState::Valid { supervised_by }`). Transparency
    // at creation — the supervised user knows before they redeem.
    let supervised_notice = Label::new(None);
    supervised_notice.add_css_class("heading");
    supervised_notice.set_halign(gtk::Align::Start);
    supervised_notice.set_wrap(true);
    supervised_notice.set_visible(false);
    set_test_id(&supervised_notice, ids::INVITE_CODE_SUPERVISED_NOTICE);
    root.append(&supervised_notice);

    // -- Error label --
    let error_label = Label::new(None);
    error_label.add_css_class("error");
    error_label.set_wrap(true);
    error_label.set_halign(gtk::Align::Start);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    error_label.set_visible(false);
    root.append(&error_label);

    // -- Back / Continue row --
    let btn_row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    btn_row.set_margin_top(12);

    let back_btn = Button::with_label(crate::i18n::strings::common::BACK);
    set_test_id(&back_btn, ids::INVITE_REQUEST_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| {
            // Cancel any in-flight invite op, then standard Back.
            // The store entry is NOT deleted by Back (per target-state doc).
            m.cancel_invite_op();
            m.back();
        });
    }
    btn_row.append(&back_btn);

    let continue_btn = Button::with_label(crate::i18n::strings::common::CONTINUE);
    continue_btn.add_css_class("suggested-action");
    continue_btn.set_sensitive(false); // driven by snapshot.continue_enabled
    set_test_id(&continue_btn, ids::INVITE_REQUEST_CONTINUE_BUTTON);
    {
        let m = m.clone();
        continue_btn.connect_clicked(move |_| {
            let m_async = m.clone();
            // Continue is the out-of-band code's redeem and nothing else
            // (`onboarding.md` § 3 — the button's row). The former PendingReview
            // branch retired 2026-08-12 with the continue-exit: that journey
            // advances by polling, and `continue_enabled` is false throughout it,
            // so this handler is unreachable there.
            // On `Done` the observer tick routes the outcome
            // (`mod.rs::handle_change`).
            async_helper::run_on_tokio(async move { m_async.redeem_invite().await }, |_| {});
        });
    }
    btn_row.append(&continue_btn);
    root.append(&btn_row);

    // -- Refresh closure --
    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let invite_status = invite_status.clone();
        let recheck_btn = recheck_btn.clone();
        let code_status = code_status.clone();
        let supervised_notice = supervised_notice.clone();
        let continue_btn = continue_btn.clone();
        let error_label = error_label.clone();
        move || {
            let snap = m.invite_request_snapshot();

            // Top row: localized message from the snapshot (per target-doc
            // rule 4: don't recompute message text on the client) + recheck
            // button visibility.
            invite_status.set_text(&snap.message.resolve(crate::i18n::strings::lookup));
            recheck_btn.set_visible(snap.recheck_visible);

            // Bottom row: OOB code status.
            code_status.set_text(&snap.oob_message.resolve(crate::i18n::strings::lookup));

            // Supervised-admission notice: only a *valid* code that carries a
            // guardian shows it (an invalid/idle/verifying code never does).
            match &snap.out_of_band_code_state {
                OobCodeState::Valid {
                    supervised_by: Some(guardian),
                    ..
                } => {
                    supervised_notice.set_text(
                        &crate::i18n::strings::family::supervised_notice_onboarding(guardian),
                    );
                    supervised_notice.set_visible(true);
                }
                _ => {
                    supervised_notice.set_text("");
                    supervised_notice.set_visible(false);
                }
            }

            // Continue button sensitivity.
            continue_btn.set_sensitive(snap.continue_enabled);

            // Error banner.
            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });

    (root, refresh)
}
