//! Page 7 of handle-first onboarding: post-VPS DNS-deferred instructions.
//!
//! Shown only when the user picked "Set up DNS later" on dns_config and
//! provisioning has completed. Renders the markdown record list the machine
//! generated (`m.dns_post_instructions()`) so the user can copy-paste into
//! their DNS provider's panel manually, then clicks Continue. Continue calls
//! `continue_from_dns_post_instructions()` which sets `wizard_outcome()` to
//! `AwaitingManualDns` and returns `OnboardingStep::Done`; the app's
//! exit-routing logic (see `docs/goal/behavior/onboarding.md` "Wizard exit handling")
//! then navigates to the "Almost ready" surface.
//!
//! ui.yaml IDs: `page-heading`, `dns-post-instructions-text`,
//! `dns-post-instructions-copy-button`, `dns-post-instructions-continue-button`.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Button, Label, Orientation, ScrolledWindow, TextView, gdk};

use crate::i18n::resolve_key as resolve;
use crate::testid::set_test_id;
use fauna_onboarding_machine::OnboardingMachine;

pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .build();
    root.set_margin_top(32);
    root.set_margin_bottom(32);
    root.set_margin_start(48);
    root.set_margin_end(48);

    let title = Label::new(Some(&resolve("onboarding.dns_post_instructions.title")));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    set_test_id(&title, ids::PAGE_HEADING);
    root.append(&title);

    let desc = Label::new(Some(&resolve(
        "onboarding.dns_post_instructions.description",
    )));
    desc.set_wrap(true);
    desc.set_halign(gtk::Align::Start);
    desc.add_css_class("fauna-muted");
    root.append(&desc);

    let scroll = ScrolledWindow::builder().min_content_height(300).build();
    let text_view = TextView::new();
    text_view.set_editable(false);
    text_view.set_monospace(true);
    text_view.set_wrap_mode(gtk::WrapMode::WordChar);
    set_test_id(&text_view, ids::DNS_POST_INSTRUCTIONS_TEXT);
    scroll.set_child(Some(&text_view));
    root.append(&scroll);

    // Track the latest text the refresh closure wrote so the copy button
    // doesn't capture a stale snapshot.
    let current_text: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

    let copy_btn = Button::with_label(&resolve("onboarding.dns_post_instructions.copy_button"));
    set_test_id(&copy_btn, ids::DNS_POST_INSTRUCTIONS_COPY_BUTTON);
    {
        let current_text = current_text.clone();
        copy_btn.connect_clicked(move |_| {
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(&current_text.borrow());
            }
        });
    }
    root.append(&copy_btn);

    let continue_btn = Button::with_label(&resolve("common.continue"));
    continue_btn.add_css_class("suggested-action");
    set_test_id(&continue_btn, ids::DNS_POST_INSTRUCTIONS_CONTINUE_BUTTON);
    {
        let m = m.clone();
        continue_btn.connect_clicked(move |_| {
            // Returned `OnboardingStep` is `Done`; the orchestrator's
            // observer-driven refresh handles the wizard exit by reading
            // `wizard_outcome()` (the AwaitingManualDns variant) — see
            // docs/goal/behavior/onboarding.md "Wizard exit handling".
            m.continue_from_dns_post_instructions();
        });
    }
    root.append(&continue_btn);

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let text_view = text_view.clone();
        let current_text = current_text.clone();
        move || {
            let text = m.dns_post_instructions().unwrap_or_default();
            text_view.buffer().set_text(&text);
            *current_text.borrow_mut() = text;
        }
    });

    (root, refresh)
}
