//! The phrase-only identity restore (`onboarding.md` § 1 Identity) — linux's
//! leg of the page tui led (`apps/fauna-tui/src/wizard/recovery_entry.rs`).
//!
//! Reached from `identity_choice`'s `restore-from-recovery-kit-button`. Submit
//! runs the shared `OnboardingMachine::submit_recovery_entry` (the pre-identity
//! escrow restore) and, on success, lands on `handle_entry` holding the
//! recovered seed — exactly where an import lands, which is why the seed is
//! committed here the way `identity_import` commits (moment 1,
//! [`super::commit_confirmed_identity`]).
//!
//! What every refusal SAYS is the shared table
//! (`RecoveryEntryOutcome::message`), resolved here and rendered on
//! `error-message`; `Superseded` alone routes instead of speaking. The account
//! field asks for a handle (`user@domain`, or the nest's own address after the
//! `@` when the domain is gone) — the ceremony has no session to ask where the
//! account lives. `qr-camera-view` is scoped to apps with a camera; a desktop
//! paste is the linux path.
use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_onboarding_machine::{OnboardingMachine, RecoveryEntryOutcome};

use crate::async_helper;
use crate::i18n::strings::common;
use crate::i18n::strings::onboarding::recovery_entry as strings;

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

    for text in [strings::DESC, strings::ACCOUNT_HINT] {
        let label = gtk::Label::new(Some(text));
        label.set_wrap(true);
        label.add_css_class("fauna-muted");
        label.set_halign(gtk::Align::Start);
        content.append(&label);
    }

    let phrase_label = gtk::Label::new(Some(strings::PHRASE_LABEL));
    phrase_label.set_halign(gtk::Align::Start);
    phrase_label.set_margin_top(8);
    content.append(&phrase_label);
    let phrase_entry = gtk::Entry::builder().hexpand(true).build();
    crate::testid::set_test_id(&phrase_entry, ids::RECOVERY_ENTRY_PHRASE_FIELD);
    content.append(&phrase_entry);

    // The shared `common::HANDLE` label: the same field concept as
    // `handle_entry`'s (priority #3), which a successful restore pre-fills.
    let account_label = gtk::Label::new(Some(common::HANDLE));
    account_label.set_halign(gtk::Align::Start);
    content.append(&account_label);
    let account_entry = gtk::Entry::builder().hexpand(true).build();
    crate::testid::set_test_id(&account_entry, ids::RECOVERY_ENTRY_ACCOUNT_FIELD);
    content.append(&account_entry);

    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    action_row.set_halign(gtk::Align::End);
    action_row.set_margin_top(12);

    let back_btn = gtk::Button::with_label(common::BACK);
    crate::testid::set_test_id(&back_btn, ids::RECOVERY_ENTRY_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| m.back());
    }
    action_row.append(&back_btn);

    // Enabled unconditionally: every way the input can be wrong is an answer
    // the ceremony gives on `error-message` (an unparseable phrase and a
    // missing account are refused locally, before anything is sent) — a
    // disabled submit would make "why can't I click this?" the user's problem.
    let submit_btn = gtk::Button::with_label(strings::SUBMIT);
    submit_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&submit_btn, ids::RECOVERY_ENTRY_SUBMIT_BUTTON);
    {
        let m = m.clone();
        let phrase_entry = phrase_entry.clone();
        let account_entry = account_entry.clone();
        submit_btn.connect_clicked(move |btn| {
            // The typed account rides on the machine's one account field — the
            // one `handle_entry` asks for next. ALWAYS forwarded, empty
            // included: what the field shows is what is sent. Forwarding only a
            // non-empty value let a handle left on the machine by an earlier
            // flow ride along invisibly, so an empty field targeted an account
            // the user never typed (and the payload's own `handle=` lost to it).
            m.set_current_handle(account_entry.text().trim().to_string());
            let phrase = phrase_entry.text().to_string();
            btn.set_sensitive(false);
            let m_async = m.clone();
            let m_done = m.clone();
            let btn = btn.clone();
            async_helper::run_on_tokio(
                async move { m_async.submit_recovery_entry(phrase).await },
                move |outcome| {
                    btn.set_sensitive(true);
                    settle(&m_done, &outcome, append);
                },
            );
        });
    }
    action_row.append(&submit_btn);
    content.append(&action_row);

    let error_label = gtk::Label::new(None);
    error_label.set_visible(false);
    error_label.set_wrap(true);
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    content.append(&error_label);

    page.append(&content);

    // A fresh page per VISIT: the stack keeps this widget for the life of the
    // wizard, so text typed on an earlier visit would otherwise still sit in
    // the fields — and the account field is forwarded as shown, so a stale
    // account there silently targets it. `map` fires when the stack makes this
    // page visible, never on the same-page ticks a refusal produces, so a
    // user correcting a refused entry keeps what they typed.
    page.connect_map({
        let phrase_entry = phrase_entry.clone();
        let account_entry = account_entry.clone();
        move |_| {
            phrase_entry.set_text("");
            account_entry.set_text("");
        }
    });

    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        let msg = m.error_message().filter(|msg| !msg.is_empty());
        crate::settings::render_error_label(&error_label, msg.as_deref());
    });
    (page, refresh)
}

/// Fold a restore's outcome back into the wizard, on the GTK thread.
fn settle(m: &OnboardingMachine, outcome: &RecoveryEntryOutcome, append: bool) {
    match outcome {
        // Uniform with the launch flow's superseded refusal: the import screen,
        // carrying why.
        RecoveryEntryOutcome::Superseded => {
            m.begin_import_identity_with_reason(strings::SUPERSEDED.to_string());
            return;
        }
        // The seed is back: commit it exactly as an import does, so a crash
        // before complete-login resumes at `handle_entry`.
        RecoveryEntryOutcome::Restored | RecoveryEntryOutcome::RestoredPredecessorsLost { .. } => {
            if let Some(secret) = m.effective_secret() {
                super::commit_confirmed_identity(&secret, append);
            }
        }
        _ => {}
    }
    if let Some(message) = outcome.message() {
        m.set_error_message(message.resolve(crate::i18n::strings::lookup));
    }
}
