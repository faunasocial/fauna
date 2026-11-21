//! Stage 2 of handle-first onboarding: handle input + live handle-check.
//!
//! ui.yaml IDs: `page-heading`, `handle-input`, `handle-check-button`,
//! `handle-message-area`, `handle-entry-continue-button`,
//! `handle-entry-back-button`, `error-message`.
//!
//! Replaces the old `submit_handle()` flow with the new snapshot-driven
//! start_handle_check / handle_check_snapshot API.

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_onboarding_machine::OnboardingMachine;

use crate::testid::set_test_id;

use crate::async_helper;

pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    root.set_margin_top(32);
    root.set_margin_bottom(32);
    root.set_margin_start(48);
    root.set_margin_end(48);

    // -- Title --
    let title = gtk::Label::new(Some(crate::i18n::strings::onboarding::handle::PROMPT));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    set_test_id(&title, ids::PAGE_HEADING);
    root.append(&title);

    // Examples / format help under the prompt (`onboarding.handle.examples_help`),
    // matching all 5 other apps (web subtitle, apple/windows/android help line).
    let examples_help = gtk::Label::new(Some(
        crate::i18n::strings::onboarding::handle::EXAMPLES_HELP,
    ));
    examples_help.add_css_class("dim-label");
    examples_help.set_halign(gtk::Align::Start);
    examples_help.set_wrap(true);
    examples_help.set_xalign(0.0);
    root.append(&examples_help);

    // -- Handle input + Check button row --
    let input_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();

    let entry = gtk::Entry::builder()
        .placeholder_text("alice@example.com")
        .hexpand(true)
        .build();
    set_test_id(&entry, ids::HANDLE_INPUT);
    // Push every keystroke into the machine so `m.current_handle()` reflects it.
    {
        let m = m.clone();
        entry.connect_changed(move |e| m.set_current_handle(e.text().to_string()));
    }
    input_row.append(&entry);

    let check_btn = gtk::Button::with_label(crate::i18n::strings::common::CHECK);
    check_btn.add_css_class("suggested-action");
    set_test_id(&check_btn, ids::HANDLE_CHECK_BUTTON);
    // Painted from the machine's answer (`ui/README.md` § Offer only what is
    // possible, rule 4's form case): a handle typed and no check in flight.
    crate::possible::paint(&check_btn, None, m.handle_check_possibility(String::new()));
    {
        let m = m.clone();
        let check_btn = check_btn.clone();
        entry.connect_changed(move |e| {
            crate::possible::paint(
                &check_btn,
                None,
                m.handle_check_possibility(e.text().to_string()),
            );
        });
    }
    // -- Error label --
    let error_label = gtk::Label::new(None);
    error_label.add_css_class("error");
    error_label.set_wrap(true);
    error_label.set_halign(gtk::Align::Start);
    error_label.set_selectable(true);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    error_label.set_visible(false);
    {
        let m = m.clone();
        let entry = entry.clone();
        let error_label = error_label.clone();
        check_btn.connect_clicked(move |_| {
            let handle = entry.text().to_string();
            // The press asks the answer it was painted from again (rule 6).
            let answer = m.handle_check_possibility(handle.clone());
            if crate::possible::refused_at_press(&answer, &error_label) {
                return;
            }
            let m = m.clone();
            async_helper::run_on_tokio(async move { m.start_handle_check(handle).await }, |_| {});
        });
    }
    input_row.append(&check_btn);
    root.append(&input_row);

    // -- Handle message area --
    let message_label = gtk::Label::new(None);
    message_label.set_wrap(true);
    message_label.set_halign(gtk::Align::Start);
    message_label.add_css_class("fauna-muted");
    // Selectable, so a message can be copied out and searched for.
    message_label.set_selectable(true);
    set_test_id(&message_label, ids::HANDLE_MESSAGE_AREA);
    root.append(&message_label);

    root.append(&error_label);

    // -- By-continuing terms notice: only while Continue signs in, read from
    // the shared snapshot's `terms_notice_visible` (never re-derived here) --
    let terms_notice = super::widgets::terms_notice();
    terms_notice.set_visible(false);
    // -- The nest's rules directly above it, on the same condition — the
    // machine's getter applies it (`nest-rules.md` § Acceptance at joining) --
    let nest_rules = gtk::Box::new(gtk::Orientation::Vertical, 4);
    nest_rules.set_visible(false);
    root.append(&nest_rules);
    root.append(&terms_notice);

    // -- Back / Continue button row --
    let btn_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    btn_row.set_margin_top(12);

    let back_btn = gtk::Button::with_label(crate::i18n::strings::common::BACK);
    set_test_id(&back_btn, ids::HANDLE_ENTRY_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| m.back());
    }
    btn_row.append(&back_btn);

    let continue_btn = gtk::Button::with_label(crate::i18n::strings::common::CONTINUE);
    continue_btn.add_css_class("suggested-action");
    continue_btn.set_sensitive(false); // painted from the machine's answer on refresh
    set_test_id(&continue_btn, ids::HANDLE_ENTRY_CONTINUE_BUTTON);
    {
        let m = m.clone();
        let window_ref: Rc<std::cell::RefCell<Option<adw::Window>>> =
            Rc::new(std::cell::RefCell::new(None));
        // We'll retrieve the parent window at click time via widget ancestry.
        let _ = window_ref; // unused for now; we use app.quit() path via the orchestrator
        continue_btn.connect_clicked(move |_| {
            let m_async = m.clone();
            // The machine updates m.step() and the observer fires: the
            // orchestrator swaps the page, or on `Done` routes the outcome
            // (`mod.rs::handle_change`).
            async_helper::run_on_tokio(
                async move { m_async.submit_handle_check_continue().await },
                |_| {},
            );
        });
    }
    btn_row.append(&continue_btn);
    root.append(&btn_row);

    // -- Refresh closure --
    // Tracks last handle pushed into the entry to avoid fighting keystrokes.
    let last_handle: Rc<Cell<String>> = Rc::new(Cell::new(String::new()));
    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let entry = entry.clone();
        let check_btn = check_btn.clone();
        let message_label = message_label.clone();
        let continue_btn = continue_btn.clone();
        let error_label = error_label.clone();
        let terms_notice = terms_notice.clone();
        let nest_rules = nest_rules.clone();
        let last_handle = last_handle.clone();
        move || {
            // Mirror handle into entry (without clobbering in-progress typing).
            let h = m.current_handle();
            if last_handle.replace(h.clone()) != h && entry.text() != h.as_str() {
                entry.set_text(&h);
            }

            crate::possible::paint(
                &check_btn,
                None,
                m.handle_check_possibility(entry.text().to_string()),
            );

            // snapshot-driven fields
            let snap = m.handle_check_snapshot();
            // Only on a change: a repaint would drop a selection being made.
            let message = snap.message.resolve(crate::i18n::strings::lookup);
            if message_label.text() != message.as_str() {
                message_label.set_text(&message);
            }

            crate::possible::paint(&continue_btn, None, m.handle_continue_possibility());
            terms_notice.set_visible(snap.terms_notice_visible);
            crate::nest_rules::paint_joining(
                &nest_rules,
                m.joining_nest_rules(crate::nest_rules::UI_LANG).as_ref(),
            );

            // error-message
            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });

    (root, refresh)
}
