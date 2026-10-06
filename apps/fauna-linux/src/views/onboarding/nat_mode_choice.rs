//! Stage 3b-bis of handle-first onboarding: the NAT-mode choice for a
//! freshly claimed nest — the single, terminal admin-path setup step.
//!
//! Resolves the nest's NAT mode (`public` / `private`, the
//! network-reachability axis). `selected_mode` pre-selects the nest's seeded
//! `node_mode` (with the private-ward refinement for private-network
//! targets), so the common case is one click on `nat-mode-confirm-button`.
//! An always-visible "Decide later" button (`nat-mode-defer-button`) exits
//! the wizard with the seed still in effect — a working default the admin
//! can change later on the admin-nest page. Unlike the retired storage-mode
//! step there is no deferral resume loop: there is no unresolved state.
//!
//! ui.yaml IDs: `page-heading`, `public-nat-mode-radio`,
//! `private-nat-mode-radio`, `nat-mode-confirm-button`,
//! `nat-mode-defer-button`, `nat-mode-status`, `error-message`.
//!
//! Per `docs/goal/behavior/onboarding.md` § 3b-bis (NAT mode choice) and
//! `docs/goal/architecture/nest/deployment-home-with-public-relay.md`.

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Button, CheckButton, Label, Orientation};

use crate::i18n::strings::onboarding::nat_mode as strings;
use crate::testid::set_test_id;
use fauna_onboarding_machine::{NatModeState, NodeMode, OnboardingMachine};

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
    let title = Label::new(Some(strings::TITLE));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    set_test_id(&title, ids::PAGE_HEADING);
    root.append(&title);

    // -- Description --
    let description = Label::new(Some(strings::DESCRIPTION));
    description.set_wrap(true);
    description.set_halign(gtk::Align::Start);
    description.set_xalign(0.0);
    description.add_css_class("fauna-muted");
    root.append(&description);

    // ── Radio group: Public vs Private ─────────────────────────────────
    // Native grouped `gtk::CheckButton`s (radios), matching the
    // encryption-mode page's shape. Pre-selection comes from the snapshot
    // (the seeded node_mode, refined private-ward for private-network
    // targets) via the refresh closure.
    let public_radio = CheckButton::with_label(strings::PUBLIC_LABEL);
    public_radio.set_margin_top(8);
    public_radio.set_hexpand(true);
    set_test_id(&public_radio, ids::PUBLIC_NAT_MODE_RADIO);
    root.append(&public_radio);

    let public_desc = Label::new(Some(strings::PUBLIC_DESC));
    public_desc.set_wrap(true);
    public_desc.set_halign(gtk::Align::Start);
    public_desc.set_xalign(0.0);
    public_desc.add_css_class("fauna-muted");
    root.append(&public_desc);

    let private_radio = CheckButton::with_label(strings::PRIVATE_LABEL);
    private_radio.set_group(Some(&public_radio));
    private_radio.set_margin_top(4);
    private_radio.set_hexpand(true);
    set_test_id(&private_radio, ids::PRIVATE_NAT_MODE_RADIO);
    root.append(&private_radio);

    let private_desc = Label::new(Some(strings::PRIVATE_DESC));
    private_desc.set_wrap(true);
    private_desc.set_halign(gtk::Align::Start);
    private_desc.set_xalign(0.0);
    private_desc.add_css_class("fauna-muted");
    root.append(&private_desc);

    // -- Status label (renders snapshot.message: Choosing / private-ward
    // hint / Submitting / Done / Error{cause}). --
    let status_label = Label::new(None);
    status_label.set_wrap(true);
    status_label.set_halign(gtk::Align::Start);
    status_label.set_xalign(0.0);
    status_label.set_margin_top(8);
    set_test_id(&status_label, ids::NAT_MODE_STATUS);
    root.append(&status_label);

    // -- Error label (page-level error-message; mirrors claim_code.rs). --
    let error_label = Label::new(None);
    error_label.add_css_class("error");
    error_label.set_wrap(true);
    error_label.set_halign(gtk::Align::Start);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    error_label.set_visible(false);
    root.append(&error_label);

    // ── Button row: [Decide later]  [Confirm] ──────────────────────────
    let btn_row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    btn_row.set_margin_top(12);

    let defer_btn = Button::with_label(strings::DEFER_BUTTON);
    set_test_id(&defer_btn, ids::NAT_MODE_DEFER_BUTTON);
    {
        let m = m.clone();
        defer_btn.connect_clicked(move |_| {
            // Sync: sets wizard_outcome() == LoggedIn and returns
            // OnboardingStep::Done — the seeded mode stays in effect (a
            // working default). The observer tick drives the wizard
            // teardown via mod.rs::handle_change → handle_wizard_done.
            m.defer_nat_mode_choice();
        });
    }
    btn_row.append(&defer_btn);

    let confirm_btn = Button::with_label(strings::CONFIRM_BUTTON);
    confirm_btn.add_css_class("suggested-action");
    set_test_id(&confirm_btn, ids::NAT_MODE_CONFIRM_BUTTON);
    {
        let m = m.clone();
        confirm_btn.connect_clicked(move |_| submit_nat_mode(m.clone()));
    }
    btn_row.append(&confirm_btn);

    root.append(&btn_row);

    // ── Radio toggled handlers ──────────────────────────────────────────
    // `updating` suppresses re-entrancy: the refresh closure programmatically
    // syncs the radios from the snapshot, which would otherwise fire these
    // handlers and clobber an in-flight `Submitting` state via a redundant
    // `select_nat_mode` call.
    let updating = Rc::new(Cell::new(false));
    {
        let m = m.clone();
        let updating = updating.clone();
        public_radio.connect_toggled(move |r| {
            if updating.get() || !r.is_active() {
                return;
            }
            m.select_nat_mode(NodeMode::Public);
        });
    }
    {
        let m = m.clone();
        let updating = updating.clone();
        private_radio.connect_toggled(move |r| {
            if updating.get() || !r.is_active() {
                return;
            }
            m.select_nat_mode(NodeMode::Private);
        });
    }

    // ── Refresh closure ─────────────────────────────────────────────────
    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let public_radio = public_radio.clone();
        let private_radio = private_radio.clone();
        let confirm_btn = confirm_btn.clone();
        let status_label = status_label.clone();
        let error_label = error_label.clone();
        let updating = updating.clone();
        move || {
            let snap = m.nat_mode_snapshot();

            // Sync radio selection from the snapshot (guarded — see above).
            // The pre-selection is the seeded node_mode with the private-ward
            // refinement, so the first refresh reflects it without a click.
            updating.set(true);
            match snap.selected_mode {
                NodeMode::Public if !public_radio.is_active() => {
                    public_radio.set_active(true);
                }
                NodeMode::Private if !private_radio.is_active() => {
                    private_radio.set_active(true);
                }
                _ => {}
            }
            updating.set(false);

            confirm_btn.set_sensitive(snap.submit_enabled);

            // Radios disabled while a submit is in flight.
            let inflight = snap.state == NatModeState::Submitting;
            public_radio.set_sensitive(!inflight);
            private_radio.set_sensitive(!inflight);

            // Status label: localized message from the snapshot.
            status_label.set_text(&snap.message.resolve(crate::i18n::strings::lookup));

            // Error banner.
            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });

    (root, refresh)
}

/// Fire `submit_nat_mode_choice()` on a worker tokio runtime. On success the
/// machine transitions to `Done` with `wizard_outcome() == LoggedIn` — the
/// terminal admin-path action; the observer-driven `handle_change` in
/// `mod.rs` then persists credentials and launches the authenticated UI.
/// On failure the snapshot moves to `Error { transient, ... }` and the
/// refresh closure renders it via `nat-mode-status`.
fn submit_nat_mode(m: Arc<OnboardingMachine>) {
    let m_async = m.clone();
    async_helper::run_on_tokio(
        async move { m_async.submit_nat_mode_choice().await },
        |_| {},
    );
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

    /// The page exposes every ui.yaml ID for `nat_mode_choice`.
    #[test]
    fn page_exposes_all_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let m = OnboardingMachine::new(Arc::new(NopObs));
            let (root, refresh) = build(m);
            refresh();
            let names = widget_names(&root);
            for id in [
                "page-heading",
                "public-nat-mode-radio",
                "private-nat-mode-radio",
                "nat-mode-confirm-button",
                "nat-mode-defer-button",
                "nat-mode-status",
                "error-message",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }
}
