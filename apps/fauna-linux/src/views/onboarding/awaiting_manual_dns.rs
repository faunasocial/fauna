//! The "Almost ready" surface — post-provisioning, DNS still pending.
//!
//! Reached only via the deferred-DNS path: the user provisioned their own nest
//! but chose "Set up later" for DNS, so the wizard exits to `Done` with
//! `wizard_outcome() == AwaitingManualDns`.
//!
//! **This is not an `OnboardingStep`.** It is rendered whenever
//! `wizard_outcome()` is `AwaitingManualDns` — which is true on *both* paths
//! that reach it: the same-session exit from `dns_post_instructions`, and the
//! relaunch hydration (the long-term store's awaiting-manual-dns slot ->
//! `LaunchMachine`'s `WizardAt{AwaitingManualDns}` row -> `seed_awaiting_manual_dns`).
//! Keying on the outcome instead of a step is what makes those two paths one
//! code path, and it avoids growing the `OnboardingStep` enum that all six
//! apps match on exhaustively. See `docs/goal/behavior/onboarding.md`
//! § "Almost ready" surface.
//!
//! The orchestrator (`mod.rs`) owns the poll cadence; this module owns the
//! render. The records text itself comes from the shared machine
//! (`awaiting_dns_records_text()`), so the label, the copy button, and every
//! other app all show the user the same instruction.
//!
//! ui.yaml IDs: `page-heading`, `awaiting-dns-records`, `awaiting-dns-status`,
//! `awaiting-dns-recheck-button`, `awaiting-dns-copy-button`. (`error-message`
//! is the wizard's window-level label, shared by every page.)

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{Button, Label, Orientation, ScrolledWindow, TextView, gdk};

use crate::i18n::resolve_key as resolve;
use crate::testid::set_test_id;
use fauna_onboarding_machine::{AwaitingDnsState, OnboardingMachine};

/// How often the surface re-probes the nest while it is shown. The machine's
/// `recheck_manual_dns()` is single-shot by contract — the client owns the
/// cadence, so native and wasm behave identically.
///
/// Reads the shared constant rather than restating `10` (`onboarding.md` § The
/// pending-invite surface — "never seven hand-copied numbers"; this was one of
/// the per-app copies that bullet names).
pub const POLL_INTERVAL: Duration =
    Duration::from_millis(fauna_onboarding_machine::AWAITING_DNS_POLL_MS);

pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .build();
    root.set_margin_top(32);
    root.set_margin_bottom(32);
    root.set_margin_start(48);
    root.set_margin_end(48);

    let title = Label::new(Some(&resolve("onboarding.awaiting_dns.title")));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    set_test_id(&title, ids::PAGE_HEADING);
    root.append(&title);

    // The machine owns the status wording (including the terminal
    // `Error { cause }`), handed over as a `LocalizedText` — never re-derived
    // here, so every app says the same thing in the same state.
    let status = Label::new(None);
    status.set_wrap(true);
    status.set_halign(gtk::Align::Start);
    status.add_css_class("fauna-muted");
    set_test_id(&status, ids::AWAITING_DNS_STATUS);
    root.append(&status);

    let scroll = ScrolledWindow::builder().min_content_height(300).build();
    let records_view = TextView::new();
    records_view.set_editable(false);
    records_view.set_monospace(true);
    records_view.set_wrap_mode(gtk::WrapMode::WordChar);
    set_test_id(&records_view, ids::AWAITING_DNS_RECORDS);
    scroll.set_child(Some(&records_view));
    root.append(&scroll);

    // Track the latest text the refresh closure wrote, so the copy button can't
    // capture a stale snapshot (same idiom as `dns_post_instructions`).
    let current_text: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

    let copy_btn = Button::with_label(&resolve("onboarding.awaiting_dns.copy_button"));
    set_test_id(&copy_btn, ids::AWAITING_DNS_COPY_BUTTON);
    {
        let current_text = current_text.clone();
        copy_btn.connect_clicked(move |_| {
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(&current_text.borrow());
            }
        });
    }
    root.append(&copy_btn);

    let recheck_btn = Button::with_label(&resolve("onboarding.awaiting_dns.recheck_button"));
    recheck_btn.add_css_class("suggested-action");
    set_test_id(&recheck_btn, ids::AWAITING_DNS_RECHECK_BUTTON);
    {
        let m = m.clone();
        recheck_btn.connect_clicked(move |_| {
            // The explicit "check now" probe. Single-shot, exactly like the
            // orchestrator's timer tick — this button just skips the wait.
            super::spawn_recheck_manual_dns(m.clone());
        });
    }
    root.append(&recheck_btn);

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let records_view = records_view.clone();
        let status = status.clone();
        let recheck_btn = recheck_btn.clone();
        let copy_btn = copy_btn.clone();
        let current_text = current_text.clone();
        move || {
            let snap = m.awaiting_manual_dns_snapshot();

            let text = m.awaiting_dns_records_text();
            records_view.buffer().set_text(&text);
            *current_text.borrow_mut() = text;

            status.set_text(&snap.message.resolve(crate::i18n::strings::lookup));

            // A probe or claim is already in flight — a second one would race
            // the first for no benefit.
            recheck_btn.set_sensitive(!matches!(
                snap.state,
                AwaitingDnsState::Checking | AwaitingDnsState::Claiming
            ));

            // Records-less resumed runs have nothing to copy; disabled, never
            // hidden — ui.yaml scopes this ID to the page's required elements.
            copy_btn.set_sensitive(m.awaiting_dns_copy_enabled());
        }
    });

    (root, refresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_onboarding_machine::{OnboardingMachine, OnboardingObserver};

    struct NopObs;
    impl OnboardingObserver for NopObs {
        fn on_changed(&self) {}
    }

    use crate::testid::widget_names;

    #[test]
    fn page_exposes_all_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let m = OnboardingMachine::new(Arc::new(NopObs));
            let (root, refresh) = build(m);
            refresh();
            let names = widget_names(&root);
            // `error-message` is deliberately absent: it is the wizard's
            // window-level banner (mod.rs), shared by every page, not a per-page
            // label — adding one here would duplicate the ID in the widget tree.
            for id in [
                "page-heading",
                "awaiting-dns-records",
                "awaiting-dns-status",
                "awaiting-dns-recheck-button",
                "awaiting-dns-copy-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }

    /// The records-less mode's second rule (the first being the status copy):
    /// "Copy all" is INERT, because there is nothing to copy. A button that
    /// answers a click by silently copying the empty string reads as a broken
    /// page rather than an empty one — and it is disabled rather than removed,
    /// since ui.yaml scopes the ID to this page's required elements.
    #[test]
    fn copy_all_is_disabled_in_the_records_less_mode_and_live_with_records() {
        crate::testid::run_on_gtk_thread(|| {
            let copy_button_sensitive = |m: Arc<OnboardingMachine>| {
                let (root, refresh) = build(m);
                refresh();
                crate::testid::find_by_test_id(&root, ids::AWAITING_DNS_COPY_BUTTON)
                    .expect("the copy button is one of the page's required elements")
                    .is_sensitive()
            };

            // A resumed standard-path run has a slot with a claim code and no
            // DNS records at all — the standard path's records were ours to
            // write, not the registrar's.
            let records_less = OnboardingMachine::new(Arc::new(NopObs));
            records_less.seed_awaiting_manual_dns(
                "https://nest.example".into(),
                "alice".into(),
                Vec::new(),
                "claim-abc".into(),
            );
            assert!(
                !copy_button_sensitive(records_less),
                "a resumed standard-path run has no records, so Copy all must be inert"
            );

            let with_records = OnboardingMachine::new(Arc::new(NopObs));
            with_records.seed_awaiting_manual_dns(
                "https://nest.example".into(),
                "alice".into(),
                vec![fauna_onboarding_machine::DnsRecordPlain {
                    record_type: "A".into(),
                    name: "@".into(),
                    value: "203.0.113.7".into(),
                    ttl: 300,
                    priority: None,
                }],
                "claim-abc".into(),
            );
            assert!(
                copy_button_sensitive(with_records),
                "with records to add at a registrar, Copy all is exactly the \
                 affordance the mode exists for"
            );
        });
    }
}
