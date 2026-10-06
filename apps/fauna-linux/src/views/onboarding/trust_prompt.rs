//! Stage 3b-ter: the one-tap "trust this box" offer (`onboarding.md`
//! § 3b-ter) — the one ratified survivor of the retired claim-time trust
//! question (`storage-modes.md` § What replaced each piece of the axis).
//!
//! ui.yaml `onboarding.trust_prompt` elements: `trust-box-summary`,
//! `trust-box-grant-button`, `trust-box-skip-button` (+ `error-message`).
//! Deliberately **no `page-heading`** — the approved set is exactly those
//! three, and the summary is the readable text a driver asserts on (tui, the
//! lead app, declares the same four).
//!
//! **No Back button** — like the NAT page before it, the admin is already
//! server-committed by the time this shows; grant and skip are its only exits
//! and both conclude the wizard with the same `LoggedIn` outcome.
//!
//! The screen **asks only.** Minting the default set needs an authenticated
//! session and the nest's content-processor roster, neither of which the
//! wizard holds, so the answer is latched (`grant_default_trust` →
//! `take_trust_prompt_granted`) and the mint runs at the signed-in handoff in
//! [`super::launch_main_app_after_signin`] — the same deferral the captured
//! deployment seed and the DNS credential use. linux reaches this page only
//! because `machine_glue::make_machine` declares
//! `set_renders_trust_prompt(true)`; before that it exited `nat_mode_choice`
//! straight to `Done`.
//!
//! ⚠ The summary copy promises **no renewal**: the blessed-box auto-renew loop
//! is unbuilt (`ui/nests.md` § Expiry / renewal), so the shared string says
//! nothing about it and this page must not add its own wording.

use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Button, Label, Orientation};

use fauna_onboarding_machine::OnboardingMachine;

use crate::i18n::strings::onboarding::trust_prompt as strings;
use crate::testid::set_test_id;

pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .build();
    root.set_margin_top(32);
    root.set_margin_bottom(32);
    root.set_margin_start(48);
    root.set_margin_end(48);

    // -- Title -- untagged on purpose (see the module doc: the page's approved
    // ID set has no `page-heading`, and an extra ID would be a ui.yaml
    // deviation needing fresh user approval).
    let title = Label::new(Some(strings::TITLE));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    root.append(&title);

    // -- What the grant covers -- the page's readable text.
    let summary = Label::new(Some(strings::SUMMARY));
    summary.set_wrap(true);
    summary.set_halign(gtk::Align::Start);
    summary.set_xalign(0.0);
    summary.add_css_class("fauna-muted");
    set_test_id(&summary, ids::TRUST_BOX_SUMMARY);
    root.append(&summary);

    // -- The two exits --
    let buttons = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .build();
    buttons.set_margin_top(12);
    buttons.set_halign(gtk::Align::Start);

    let grant_btn = Button::with_label(strings::GRANT_BUTTON);
    grant_btn.add_css_class("suggested-action");
    set_test_id(&grant_btn, ids::TRUST_BOX_GRANT_BUTTON);
    buttons.append(&grant_btn);

    let skip_btn = Button::with_label(strings::SKIP_BUTTON);
    set_test_id(&skip_btn, ids::TRUST_BOX_SKIP_BUTTON);
    buttons.append(&skip_btn);

    root.append(&buttons);

    // -- Error banner -- present for the page contract (e2e convention 2) even
    // though neither exit can fail here: both are pure local latches.
    let error_label = Label::new(None);
    error_label.set_wrap(true);
    error_label.set_halign(gtk::Align::Start);
    error_label.set_xalign(0.0);
    error_label.add_css_class("error");
    error_label.set_visible(false);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    root.append(&error_label);

    // Both handlers do exactly ONE thing: latch the answer and return. See
    // `conclude` below for why nothing else may happen inside the click.
    {
        let m = m.clone();
        grant_btn.connect_clicked(move |_| {
            m.grant_default_trust();
        });
    }
    {
        let m = m.clone();
        skip_btn.connect_clicked(move |_| {
            m.skip_trust_prompt();
        });
    }

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let error_label = error_label.clone();
        move || {
            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });

    (root, refresh)
}

// ⚠ **The click handlers must NOT drive the wizard's conclusion inline** —
// measured, not theorised (`test_trust_prompt.py --app linux`, 2026-08-14).
//
// Both answers are synchronous local latches that land the machine on `Done`,
// so calling `super::handle_wizard_done` straight from the click looks
// harmless. It is not: the observer-driven `handle_change` in `mod.rs` owns the
// `Done` arm, and that arm *launches the authenticated app* —
// `launch_main_app_after_signin`, which stores credentials, builds the main
// window and wires the whole authenticated session. Doing any of that inside
// the click callback means the callback — and therefore the test agent's
// `click` command — does not return until the launch finishes, which reliably
// blew the agent's 25 s reply budget: `Bridge error (504): agent timeout — the
// UI thread did not reply within 25s` on both the grant and the skip journeys,
// while the "is the offer shown" test passed. The NAT page never hit this
// because its submit is async and its completion is marshalled back separately.
//
// ⚠ Do not read the 2026-08-14 measurement as "the silent challenge is the
// problem". It was *a* problem — `launch_main_app_after_signin` then ran the
// challenge as a `block_on` on a runtime built and dropped on the GTK thread,
// and that alone froze the main loop long enough to 504 every claim→app
// journey, whether or not this page was in the flow (A/B-proved with the
// capability flag off). That is fixed at the source: the challenge now runs off
// the main thread and the launch is its continuation, like the cold-launch path
// in `main.rs`. The rule below survives the fix on its own merits — the
// continuation is still real main-thread work, and a click handler is still the
// wrong place to run it from.
//
// So: latch, return, and let the observer's next main-context turn do the rest
// — the same path every other step transition in this wizard takes. That also
// keeps e2e convention 11 honest (a dropped command must fail loudly, never
// hang) rather than papering over a blocked UI thread with a bigger timeout.

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_onboarding_machine::{OnboardingMachine, OnboardingObserver};

    struct NopObs;
    impl OnboardingObserver for NopObs {
        fn on_changed(&self) {}
    }

    use crate::testid::widget_names;

    /// The page exposes every ui.yaml ID for `trust_prompt` — and no more: the
    /// approved set is exactly three plus the error element.
    #[test]
    fn page_exposes_all_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let m = OnboardingMachine::new(Arc::new(NopObs));
            let (root, refresh) = build(m);
            refresh();
            let names = widget_names(&root);
            for id in [
                "trust-box-summary",
                "trust-box-grant-button",
                "trust-box-skip-button",
                "error-message",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }

    /// Granting latches the answer for the signed-in handoff to consume, and
    /// skipping does not — the whole contract this page carries into the launch
    /// glue (`onboarding.md` § 3b-ter).
    #[test]
    fn grant_latches_the_answer_and_skip_does_not() {
        let granted = OnboardingMachine::new(Arc::new(NopObs));
        granted.grant_default_trust();
        assert!(
            granted.take_trust_prompt_granted(),
            "grant must latch for the handoff to mint"
        );
        assert!(
            !granted.take_trust_prompt_granted(),
            "the latch is consume-once, so a handoff that runs twice mints once"
        );

        let skipped = OnboardingMachine::new(Arc::new(NopObs));
        skipped.skip_trust_prompt();
        assert!(
            !skipped.take_trust_prompt_granted(),
            "skipping must leave nothing to mint"
        );
    }
}
