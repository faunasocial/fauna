//! Stage 3a of handle-first onboarding: one-time claim-code entry for an
//! unclaimed nest.
//!
//! Reached when handle-check returns `UnregisteredUnclaimedNest` — the nest
//! is reachable but the `fauna.setup.status` WS-RPC kind reports `claimed=false`. There is
//! no admin yet, so requesting an invite is impossible. The user pastes the
//! one-time claim code printed by the nest server's bootstrap process and
//! atomically becomes the admin via `POST /api/v1/claim-admin`.
//!
//! ui.yaml IDs: `page-heading`, `claim-code-input`, `claim-code-submit-button`,
//! `claim-code-status`, `claim-code-back-button`, `error-message`.
//!
//! Per `docs/goal/behavior/onboarding.md` §3a (Claim code).

use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Button, Entry, Label, Orientation};

use crate::testid::set_test_id;
use fauna_onboarding_machine::OnboardingMachine;

use crate::async_helper;

pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .build();
    root.set_margin_top(32);
    root.set_margin_bottom(32);
    root.set_margin_start(48);
    root.set_margin_end(48);

    // -- Title --
    let title = Label::new(Some(crate::i18n::strings::onboarding::claim_code::TITLE));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    set_test_id(&title, ids::PAGE_HEADING);
    root.append(&title);

    // -- Description --
    let description = Label::new(Some(
        crate::i18n::strings::onboarding::claim_code::DESCRIPTION,
    ));
    description.set_wrap(true);
    description.set_halign(gtk::Align::Start);
    description.set_xalign(0.0);
    description.add_css_class("fauna-muted");
    root.append(&description);

    // ── Input row: claim-code input + Submit button ────────────────────
    let input_row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .build();
    input_row.set_margin_top(8);

    let code_input = Entry::builder()
        .placeholder_text(crate::i18n::strings::onboarding::claim_code::PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(&code_input, ids::CLAIM_CODE_INPUT);
    input_row.append(&code_input);

    let submit_btn =
        Button::with_label(crate::i18n::strings::onboarding::claim_code::SUBMIT_BUTTON);
    submit_btn.add_css_class("suggested-action");
    submit_btn.set_sensitive(false); // gated on input non-empty + snapshot.submit_enabled
    set_test_id(&submit_btn, ids::CLAIM_CODE_SUBMIT_BUTTON);
    {
        let m = m.clone();
        let code_input = code_input.clone();
        submit_btn.connect_clicked(move |_| {
            let code = code_input.text().to_string();
            let m_async = m.clone();
            // A 2xx lands the machine on `Done` (LoggedIn, admin): the
            // observer tick routes the outcome (`mod.rs::handle_change`). For
            // ClaimCode (stay) the snapshot already carries the error; the
            // refresh closure renders it via claim-code-status.
            async_helper::run_on_tokio(
                async move { m_async.wizard_submit_claim_code(code).await },
                |_| {},
            );
        });
    }
    input_row.append(&submit_btn);
    root.append(&input_row);

    // -- Status label (renders snapshot.message — Idle / Submitting /
    // Claimed / Invalid{reason} / Error{cause}). --
    let status_label = Label::new(None);
    status_label.set_wrap(true);
    status_label.set_halign(gtk::Align::Start);
    status_label.set_xalign(0.0);
    set_test_id(&status_label, ids::CLAIM_CODE_STATUS);
    root.append(&status_label);

    // -- Error label (page-level error-message; per ui.yaml the page
    // exposes both claim-code-status (per-field) and error-message
    // (page-level). Mirrors invite_request.rs:134-140.) --
    let error_label = Label::new(None);
    error_label.add_css_class("error");
    error_label.set_wrap(true);
    error_label.set_halign(gtk::Align::Start);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    error_label.set_visible(false);
    root.append(&error_label);

    // -- Back button row --
    let btn_row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    btn_row.set_margin_top(12);

    let back_btn = Button::with_label(crate::i18n::strings::common::BACK);
    set_test_id(&back_btn, ids::CLAIM_CODE_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| {
            // Standard back navigation — machine routes ClaimCode → HandleEntry
            // (machine.rs:1003).
            m.back();
        });
    }
    btn_row.append(&back_btn);
    root.append(&btn_row);

    // -- Refresh closure --
    // Per `docs/goal/behavior/onboarding.md` §3a, `submit_enabled` is the canonical
    // gate (encodes both "machine is not inflight" and "input is non-empty"
    // — the input-changed handler below pushes keystrokes through the
    // machine via `set_claim_code_input` / equivalent). We trust the
    // snapshot here directly, matching the cross-app contract that the
    // Python e2e fixtures rely on.
    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let submit_btn = submit_btn.clone();
        let status_label = status_label.clone();
        let error_label = error_label.clone();
        let code_input = code_input.clone();
        move || {
            let snap = m.claim_code_snapshot();

            // Pre-fill the code when the machine carries one (the factory-reset
            // re-onboard path — the human never sees the returned code, so
            // without this they'd be stranded). Only fill an empty input so we
            // never fight a user edit on a later refresh tick.
            if code_input.text().is_empty()
                && let Some(code) = m.claim_code_prefill()
                && !code.is_empty()
            {
                code_input.set_text(&code);
            }

            // Status label: localized message from the snapshot (per
            // target-doc rule: don't recompute message text on the client).
            status_label.set_text(&snap.message.resolve(crate::i18n::strings::lookup));

            // Submit button: snapshot is the source of truth.
            submit_btn.set_sensitive(snap.submit_enabled);

            // Error banner.
            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });

    (root, refresh)
}
