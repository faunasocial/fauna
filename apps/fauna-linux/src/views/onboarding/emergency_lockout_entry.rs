//! The signed-out lock door (`ui/sessions.md` § Layout & flow → *The signed-out
//! door*; behavior `devices.md` § The signed-out door) — linux's leg of the page
//! tui led (`apps/fauna-tui/src/wizard/emergency_lockout_entry.rs`).
//!
//! ui.yaml `onboarding.emergency_lockout_entry` elements: `paste-secret-field`,
//! `recovery-entry-account-field`, `sessions-lockout-warning`,
//! `sessions-lockout-confirm-field`, `sessions-lockout-button`,
//! `emergency-lockout-back-button`, and `error-message` (the wizard's banner).
//!
//! Reached from `identity_choice`'s `emergency-lockout-entry-button`. Submit
//! runs the shared `OnboardingMachine::submit_emergency_lockout` — the
//! seed-signed `fauna.account.lockout` over one anonymous connection — and
//! **persists nothing**: the three fields are cleared on every visit and the
//! moment a lock lands, and a lock returns to `identity_choice` with the
//! confirmation line on its `error-message`.
//!
//! The lock warning and the `LOCK` prompt are the shared `sessions.*` strings,
//! the same text the signed-in Sessions page paints — one copy of the REQUIRED
//! warning (`devices.md` § The two panic buttons).
use fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD;
use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_onboarding_machine::{EmergencyLockoutOutcome, OnboardingMachine};

use crate::async_helper;
use crate::i18n::strings::common;
use crate::i18n::strings::onboarding::emergency_lockout as strings;
use crate::i18n::strings::sessions as s;

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

    for text in [strings::DESC, strings::ACCOUNT_HINT] {
        let label = gtk::Label::new(Some(text));
        label.set_wrap(true);
        label.add_css_class("fauna-muted");
        label.set_halign(gtk::Align::Start);
        content.append(&label);
    }

    let secret_label = gtk::Label::new(Some(strings::SECRET_LABEL));
    secret_label.set_halign(gtk::Align::Start);
    secret_label.set_margin_top(8);
    content.append(&secret_label);
    let secret_entry = gtk::PasswordEntry::builder().hexpand(true).build();
    secret_entry.set_show_peek_icon(true);
    crate::testid::set_test_id(&secret_entry, ids::PASTE_SECRET_FIELD);
    content.append(&secret_entry);

    let account_label = gtk::Label::new(Some(common::HANDLE));
    account_label.set_halign(gtk::Align::Start);
    content.append(&account_label);
    let account_entry = gtk::Entry::builder().hexpand(true).build();
    crate::testid::set_test_id(&account_entry, ids::RECOVERY_ENTRY_ACCOUNT_FIELD);
    content.append(&account_entry);

    // REQUIRED floor copy, always visible — never behind the confirm.
    let warning = gtk::Label::new(Some(s::LOCKOUT_WARNING));
    warning.set_wrap(true);
    warning.set_halign(gtk::Align::Start);
    warning.set_margin_top(8);
    warning.add_css_class("error-banner");
    crate::testid::set_test_id(&warning, ids::SESSIONS_LOCKOUT_WARNING);
    content.append(&warning);

    let confirm_entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text(s::LOCKOUT_CONFIRM_PLACEHOLDER)
        .build();
    crate::testid::set_test_id(&confirm_entry, ids::SESSIONS_LOCKOUT_CONFIRM_FIELD);
    content.append(&confirm_entry);

    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    action_row.set_halign(gtk::Align::End);
    action_row.set_margin_top(12);

    let back_btn = gtk::Button::with_label(common::BACK);
    crate::testid::set_test_id(&back_btn, ids::EMERGENCY_LOCKOUT_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| m.back());
    }
    action_row.append(&back_btn);

    // The word gates the button here AND in the machine — a wrong word is
    // refused identically on all seven apps (`submit_emergency_lockout`).
    let lock_btn = gtk::Button::with_label(s::LOCKOUT_BUTTON);
    lock_btn.add_css_class("destructive-action");
    lock_btn.set_sensitive(false);
    crate::testid::set_test_id(&lock_btn, ids::SESSIONS_LOCKOUT_BUTTON);
    {
        let lock_btn = lock_btn.clone();
        confirm_entry.connect_changed(move |e| {
            lock_btn.set_sensitive(e.text() == LOCKOUT_CONFIRM_WORD);
        });
    }
    {
        let m = m.clone();
        let secret_entry = secret_entry.clone();
        let account_entry = account_entry.clone();
        let confirm_entry = confirm_entry.clone();
        lock_btn.connect_clicked(move |btn| {
            // The account rides on the machine's one handle slot — forwarded
            // as shown, empty included, so the pasted `handle=` or the typed
            // field locates the nest, never a stale earlier handle.
            m.set_current_handle(account_entry.text().trim().to_string());
            let secret = secret_entry.text().to_string();
            let word = confirm_entry.text().to_string();
            btn.set_sensitive(false);
            let m_async = m.clone();
            let m_done = m.clone();
            let btn = btn.clone();
            let fields = [
                secret_entry.clone().upcast::<gtk::Editable>(),
                account_entry.clone().upcast::<gtk::Editable>(),
                confirm_entry.clone().upcast::<gtk::Editable>(),
            ];
            async_helper::run_on_tokio(
                async move { m_async.submit_emergency_lockout(secret, word).await },
                move |outcome| {
                    if matches!(outcome, EmergencyLockoutOutcome::Locked { .. }) {
                        // Nothing persists: drop what was typed the moment the
                        // lock lands (the `map` handler covers a later visit).
                        for f in &fields {
                            f.set_text("");
                        }
                    } else {
                        btn.set_sensitive(fields[2].text() == LOCKOUT_CONFIRM_WORD);
                    }
                    settle(&m_done, &outcome);
                },
            );
        });
    }
    action_row.append(&lock_btn);
    content.append(&action_row);

    page.append(&content);

    // A fresh page per VISIT: the stack keeps this widget for the life of the
    // wizard, so a secret typed on an earlier visit would otherwise still sit
    // in the field — the door keeps nothing.
    page.connect_map({
        let secret_entry = secret_entry.clone();
        let account_entry = account_entry.clone();
        let confirm_entry = confirm_entry.clone();
        move |_| {
            secret_entry.set_text("");
            account_entry.set_text("");
            confirm_entry.set_text("");
        }
    });

    // `error-message` is the wizard's banner (`handle_change`), which renders
    // the machine's line on every tick — nothing machine-derived paints here.
    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        let _ = &m;
    });
    (page, refresh)
}

/// Render the outcome's one shared line onto the machine's `error-message`
/// channel: the confirmation on `identity_choice` after a landed lock, the
/// refusal on this step otherwise (`EmergencyLockoutOutcome::message`).
fn settle(m: &OnboardingMachine, outcome: &EmergencyLockoutOutcome) {
    let unlock_time = match outcome {
        EmergencyLockoutOutcome::Locked { locked_until } => {
            fauna_core::format::format_unix_local(*locked_until)
        }
        _ => String::new(),
    };
    m.set_error_message(
        outcome
            .message(&unlock_time)
            .resolve(crate::i18n::strings::lookup),
    );
}
