//! Launch screen — pre-wizard transient view.
//!
//! Shown while the silent challenge runs against a saved nest_url, and
//! during the transient-error retry CTA per
//! `docs/goal/behavior/onboarding.md` "App-launch routing" lines 246-253.
//!
//! Two visual sub-states:
//!
//! - Launching: a spinner + status label "Signing you in…". No test ID
//!   exposed — the launching phase is transient and other apps
//!   (Windows / Android) don't expose one either; ui.yaml's
//!   `onboarding.launch_retry` page only enumerates the retry-phase IDs.
//!
//! - TransientRetry: title, error description (`launch-transient-error`),
//!   Retry (`launch-retry-button`), "Use a different nest"
//!   (`launch-fallthrough-button`), and (conditionally) "Recover a lost box"
//!   (`launch-recover-button`). Shown when the silent challenge hits
//!   a retryable network failure; the user can retry, fall through to
//!   the wizard at handle_entry to pick a different domain, or — when the
//!   saved nest is reachable and custodies ≥1 deployment seed — enter box
//!   recovery. The recover button is hidden until the launch-time
//!   reachable-nest box-list read (`set_recover_boxes`) surfaces a box
//!   (box-recovery.md § Recovery UI (step 4); the surviving-device entry,
//!   mirroring the web `recoverableBoxes` gate).
//!
//! - NeedsUpdate: the NON-retry terminal surface shown when the nest
//!   authoritatively reports it is outdated (the launch machine's
//!   `Offline { transient: false }` carrying the localized `fauna.nest.outdated`
//!   message). The message renders in the canonical `error-message` element
//!   (matching web), the Retry button is omitted (retrying the same outdated
//!   nest is futile), and only `launch-fallthrough-button` remains. This is the
//!   "update prompt vs. retry" distinction of `version-compatibility.md` Dim 4 /
//!   `onboarding.md` § App-launch routing (version-mismatch row).
//!
//! - SignInRefused: a nest this app signed in to before now refuses the identity
//!   (the snapshot's `sign_in_refused` side channel on `Offline { transient:
//!   false }`). Paints `launch-sign-in-refused-notice`, WITH `launch-retry-button`
//!   (the admin's restore happens off this device) and `launch-fallthrough-button`.
//!   Mirrors `apps/fauna-tui/src/launch.rs`'s `LaunchSurface::SignInRefused`.
//!
//! - AccountIndexUnreadable: the saved account index is present and this
//!   build cannot use it (`version-compatibility.md` § 5 item 9). Checked
//!   BEFORE every other row — the machine projects it onto
//!   `Offline { transient: false }`, and the verdict rides the additive
//!   `LaunchSnapshot::account_index_refusal` side channel (the same pattern
//!   as `superseded_successor`). Paints `account-index-refusal-warning`,
//!   never a retry and never `launch-fallthrough-button` — the nest is not
//!   the problem. Only the malformed verdict adds
//!   `account-index-reset-button`, which reveals
//!   `account-index-reset-confirm-button` on a first press (a purely local
//!   reveal, no machine round trip) — the documented floor
//!   (`long-term-store.md` § Cleanup contract). Mirrors
//!   `apps/fauna-tui/src/launch.rs`'s `LaunchSurface::AccountIndexUnreadable`.
//!
//! The 404-unregistered, decommissioned, and authenticated outcomes do
//! NOT use this view; they navigate immediately to the wizard at
//! invite_request, the wizard at handle_entry, or the authenticated main
//! window respectively (per the target doc's failure-mode table).
//!
//! ui.yaml: `onboarding.launch_retry` page. Cross-app peer
//! implementations:
//! - Web: `apps/fauna-web/src/routes/onboarding/+page.svelte` (`launchState`).
//! - macOS: `apps/fauna-apple/Fauna-macOS/Views/MainWindow/ContentView.swift`
//!   (`LaunchProgressView` + `LaunchRetryView`).
//! - Windows: `apps/fauna-windows/FaunaApp/FaunaApp/Views/LaunchRetryPage.xaml`.
//! - Android: `apps/fauna-android/app/src/main/java/com/fauna/app/ui/screen/LaunchRetryScreen.kt`.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{Button, Label, Orientation};

use crate::i18n::strings as i18n;
use crate::testid::set_test_id;

/// Visual phase of the launch screen.
#[derive(Clone)]
pub enum LaunchPhase {
    /// Silent challenge in flight. Show spinner + `launch-status` text.
    Launching,
    /// Transient network failure. Show title + `launch-transient-error`
    /// (with the given message) + `launch-retry-button` +
    /// `launch-fallthrough-button`.
    TransientRetry { error: String },
    /// Terminal version mismatch: the nest reported it is outdated
    /// (`fauna.nest.outdated`). Show the localized `error` in `error-message`
    /// and `launch-fallthrough-button` only — NO `launch-retry-button` (retrying
    /// the same outdated nest is futile). version-compatibility.md Dim 4.
    NeedsUpdate { error: String },
    /// A nest this app signed in to before now refuses the identity (the
    /// machine's `Offline { transient: false }` told apart by the snapshot's
    /// `sign_in_refused` side channel; `onboarding.md` § App-launch routing →
    /// the previously-signed-in row). Show the sentence in
    /// `launch-sign-in-refused-notice`, **with** `launch-retry-button` — the
    /// remedy (the admin's restore) happens off this device, and the machine
    /// honours the retry from this state — plus `launch-fallthrough-button`.
    /// Twin of tui's `LaunchSurface::SignInRefused`.
    SignInRefused { error: String },
    /// The nest's pinned deployment identity changed, or it can no longer prove
    /// the identity we pinned (security.md § Transport trust — the
    /// SSH `known_hosts` model). Show the localized warning in `error-message`
    /// and `launch-fallthrough-button` only — NO `launch-retry-button`: a retry
    /// cannot change the verdict, and must never silently re-pin.
    ///
    /// The full uniform surface: the warning + the "trust this nest" re-trust
    /// button (`nest-identity-changed-trust-button` →
    /// `LaunchMachine::trust_nest_identity()`) + the fallthrough. `ui.yaml`'s
    /// `launch_identity_changed` was widened out of `platforms: [web]` to all
    /// apps (user-approved, rule A, 2026-07-13), so this is the same surface
    /// web renders. Linux warns and offers both ways out, rather than — as it
    /// did before this arm existed — falling into the catch-all and rendering a
    /// *dead* Retry button (`retry_silent_challenge()` is a no-op outside
    /// `Offline{transient:true}`) over a raw Rust debug dump.
    IdentityChanged { error: String },
    /// The saved account index is present and this build cannot use it
    /// (`version-compatibility.md` § 5 item 9). Show
    /// `account-index-refusal-warning` with the verdict's message; the
    /// `NewerBuild` verdict offers no action, the `Malformed` verdict adds
    /// `account-index-reset-button` (reveals the confirm) or, once revealed,
    /// `account-index-reset-confirm-button` (runs the documented floor).
    /// Never `launch-retry-button` or `launch-fallthrough-button` — the nest
    /// was never contacted.
    AccountIndexUnreadable {
        refusal: fauna_launch_machine::AccountIndexRefusal,
        /// The malformed verdict's start-over has been pressed and the
        /// confirm — which states the residual — is showing. Always `false`
        /// for the `NewerBuild` verdict, which offers no action at all.
        confirming: bool,
    },
}

/// The `retry_box`'s title text for the three phases that show it — chrome,
/// not an automatable element (no test id; mirrors tui's `LaunchSurface::
/// title()`). NeedsUpdate/IdentityChanged re-label rather than reuse
/// `RETRY_TITLE`: the nest WAS reached in both, so "Couldn't reach your nest"
/// would misstate what happened (copy-audit, 2026-08-04).
fn retry_title_text(phase: &LaunchPhase) -> &'static str {
    match phase {
        LaunchPhase::Launching | LaunchPhase::TransientRetry { .. } => i18n::launch::RETRY_TITLE,
        LaunchPhase::NeedsUpdate { .. } => i18n::launch::NEEDS_UPDATE_TITLE,
        LaunchPhase::SignInRefused { .. } => i18n::onboarding::launch::SIGN_IN_REFUSED_TITLE,
        LaunchPhase::IdentityChanged { .. } => i18n::launch::IDENTITY_CHANGED_TITLE,
        LaunchPhase::AccountIndexUnreadable { refusal, .. } => match refusal {
            fauna_launch_machine::AccountIndexRefusal::NewerBuild { .. } => {
                i18n::onboarding::launch::INDEX_NEWER_BUILD_TITLE
            }
            fauna_launch_machine::AccountIndexRefusal::Malformed => {
                i18n::onboarding::launch::INDEX_MALFORMED_TITLE
            }
        },
    }
}

/// Handles the launch view exposes to its caller. Caller wires up
/// `on_retry`, `on_trust`, `on_fallthrough`, and `on_recover` to the
/// silent-challenge controller.
pub struct LaunchView {
    pub window: adw::ApplicationWindow,
    /// Switch the launch screen between Launching and TransientRetry.
    pub set_phase: Rc<dyn Fn(LaunchPhase)>,
    /// Feed the launch-time box list (the hex `nest_actor_id`s from
    /// `crate::client::load_recoverable_boxes` — this device's store joined with
    /// the saved nest). A non-empty list reveals
    /// the surviving-device `launch-recover-button`; empty hides it. Called
    /// best-effort after the transient-retry surface paints, so the button
    /// appears only once the read resolves (box-recovery.md § Recovery UI
    /// (step 4)).
    pub set_recover_boxes: Rc<dyn Fn(Vec<String>)>,
    /// Per `OnboardingResult` convention: exposed so `start_test_agent_if_enabled`
    /// can mirror it into the test-agent JSON payload.
    pub error_label: Label,
}

/// Build the launch window. Initial phase is `Launching`.
///
/// Caller is responsible for:
/// - Calling `set_phase(TransientRetry { error })` when the silent
///   challenge returns a transient failure.
/// - Tearing down this window (calling `window.destroy()`, NOT
///   `window.close()`) and presenting the next window (main authenticated
///   window or onboarding wizard) when the silent challenge resolves.
///   `close()` fires `connect_close_request`, which calls `app.quit()`
///   for the user-clicked-X case; programmatic teardown must bypass that.
pub fn build_launch_window<F, G, H, T, I>(
    app: &adw::Application,
    on_retry: F,
    on_fallthrough: G,
    on_recover: H,
    // `nest-identity-changed-trust-button`: the explicit re-trust
    // (`LaunchMachine::trust_nest_identity()` — forget the pin, re-TOFU,
    // re-challenge). The ONLY path that forgets a pin.
    on_trust: T,
    // `account-index-reset-confirm-button`: the documented floor
    // (`long-term-store.md` § Cleanup contract) — erase every account +
    // wipe the credential namespace + land on onboarding. Only reachable
    // from the `Malformed` verdict's confirm; a no-op call site would be a
    // bug (`views::launch` guards it, not the caller).
    on_reset: I,
) -> LaunchView
where
    F: Fn() + 'static,
    G: Fn() + 'static,
    // Surviving-device recovery entry: receives the launch-time box list
    // (`recoverableBoxes` on web) so the wizard can seed identity + push the
    // boxes into the recovery machine.
    H: Fn(Vec<String>) + 'static,
    T: Fn() + 'static,
    I: Fn() + 'static,
{
    let outer = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .build();

    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(true);
    header.set_show_start_title_buttons(true);
    outer.append(&header);

    // Banner-level error label for orchestrator-style errors. Always
    // present so the test agent can mirror its content per
    // OnboardingResult convention; visibility is toggled by callers.
    let error_label = Label::new(None);
    error_label.set_halign(gtk::Align::Fill);
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    error_label.add_css_class("error-banner");
    error_label.set_visible(false);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    outer.append(&error_label);

    let center = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(16)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .hexpand(true)
        .build();
    center.set_margin_top(48);
    center.set_margin_bottom(48);
    center.set_margin_start(48);
    center.set_margin_end(48);
    outer.append(&center);

    // ── Launching sub-state ───────────────────────────────────────────
    let launching_box = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();

    let spinner = gtk::Spinner::new();
    spinner.set_size_request(48, 48);
    spinner.start();
    launching_box.append(&spinner);

    // No test ID on the launching status text — ui.yaml's
    // onboarding.launch_retry page only defines the retry-phase IDs, and
    // peer clients (Windows, Android) don't expose one either.
    let status = Label::new(Some(i18n::launch::SIGNING_IN));
    status.add_css_class("dim-label");
    launching_box.append(&status);

    center.append(&launching_box);

    // ── TransientRetry sub-state ──────────────────────────────────────
    let retry_box = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    retry_box.set_visible(false);

    let retry_title = Label::new(Some(i18n::launch::RETRY_TITLE));
    retry_title.add_css_class("title-3");
    retry_box.append(&retry_title);

    let transient_error = Label::new(None);
    transient_error.set_wrap(true);
    transient_error.add_css_class("dim-label");
    set_test_id(&transient_error, ids::LAUNCH_TRANSIENT_ERROR);
    retry_box.append(&transient_error);

    // `launch_identity_changed` (ui.yaml): the SSH known_hosts warning. Its own
    // element, not the generic `error-message` — the page names it explicitly,
    // and an e2e asserting "the user was warned about a changed nest identity"
    // must not be satisfiable by any old error text.
    let identity_warning = Label::new(None);
    identity_warning.set_wrap(true);
    identity_warning.add_css_class("error");
    identity_warning.set_visible(false);
    set_test_id(&identity_warning, ids::NEST_IDENTITY_CHANGED_WARNING);
    retry_box.append(&identity_warning);

    // `launch_sign_in_refused` (ui.yaml): the nest no longer signs this identity
    // in. Its own element, like the identity warning, so an e2e asserting "the
    // user was told this nest no longer signs them in" cannot be satisfied by
    // any old error text.
    let sign_in_refused_notice = Label::new(None);
    sign_in_refused_notice.set_wrap(true);
    sign_in_refused_notice.add_css_class("error");
    sign_in_refused_notice.set_visible(false);
    set_test_id(&sign_in_refused_notice, ids::LAUNCH_SIGN_IN_REFUSED_NOTICE);
    retry_box.append(&sign_in_refused_notice);

    // `launch_account_index_unreadable` (ui.yaml): the saved account index is
    // present and this build cannot use it. Its own element — the two
    // verdicts differ in the action they offer, so a generic `error-message`
    // couldn't distinguish "update helps" from "update cannot help".
    let account_index_warning = Label::new(None);
    account_index_warning.set_wrap(true);
    account_index_warning.add_css_class("error");
    account_index_warning.set_visible(false);
    set_test_id(&account_index_warning, ids::ACCOUNT_INDEX_REFUSAL_WARNING);
    retry_box.append(&account_index_warning);

    let buttons = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    buttons.set_margin_top(12);

    let retry_btn = Button::with_label(i18n::launch::RETRY_BUTTON);
    retry_btn.add_css_class("suggested-action");
    set_test_id(&retry_btn, ids::LAUNCH_RETRY_BUTTON);
    {
        let on_retry = Rc::new(on_retry);
        retry_btn.connect_clicked(move |_| on_retry());
    }
    buttons.append(&retry_btn);

    // "Trust this nest and continue" — the explicit, user-approved re-trust
    // (`LaunchMachine::trust_nest_identity()`: forget the pin, re-TOFU, re-run
    // the silent challenge). Visible ONLY on the IdentityChanged surface; the pin
    // is never forgotten by any other path (no silent re-pin, ever).
    let trust_btn = Button::with_label(i18n::onboarding::launch::IDENTITY_CHANGED_TRUST);
    trust_btn.add_css_class("suggested-action");
    trust_btn.set_visible(false);
    set_test_id(&trust_btn, ids::NEST_IDENTITY_CHANGED_TRUST_BUTTON);
    {
        let on_trust = Rc::new(on_trust);
        trust_btn.connect_clicked(move |_| on_trust());
    }
    buttons.append(&trust_btn);

    // "Start over on this device" — shown ONLY for the malformed verdict,
    // never the version one (`account-index-reset-button`). Reveals
    // `account-index-reset-confirm-button`; performs nothing itself — a
    // purely local reveal, no machine round trip (mirrors tui's
    // `LaunchAction::StartOver`/`reveal_start_over`).
    let account_index_reset_btn =
        Button::with_label(i18n::onboarding::launch::INDEX_MALFORMED_RESET);
    account_index_reset_btn.add_css_class("destructive-action");
    account_index_reset_btn.set_visible(false);
    set_test_id(&account_index_reset_btn, ids::ACCOUNT_INDEX_RESET_BUTTON);
    buttons.append(&account_index_reset_btn);

    // Inline destructive confirm for starting over (mirrors
    // `sign-out-confirm-button`). Its own surface already states the
    // residual (the warning label's text switches to
    // `INDEX_MALFORMED_RESET_RESIDUAL` when this button is shown) before it
    // runs `on_reset` — the documented floor (`clear_all` + re-onboard).
    let account_index_confirm_btn =
        Button::with_label(i18n::onboarding::launch::INDEX_MALFORMED_RESET_CONFIRM);
    account_index_confirm_btn.add_css_class("destructive-action");
    account_index_confirm_btn.set_visible(false);
    set_test_id(
        &account_index_confirm_btn,
        ids::ACCOUNT_INDEX_RESET_CONFIRM_BUTTON,
    );
    buttons.append(&account_index_confirm_btn);

    let fallthrough_btn = Button::with_label(i18n::launch::USE_DIFFERENT_NEST);
    set_test_id(&fallthrough_btn, ids::LAUNCH_FALLTHROUGH_BUTTON);
    {
        let on_fallthrough = Rc::new(on_fallthrough);
        fallthrough_btn.connect_clicked(move |_| on_fallthrough());
    }
    buttons.append(&fallthrough_btn);

    // Surviving-device recovery entry (box-recovery.md § Recovery UI (step 4)).
    // Hidden until the launch-time reachable-nest read (`set_recover_boxes`)
    // surfaces ≥1 custodied box — mirrors the web `{#if recoverableBoxes.length}`
    // gate. On click it hands the read box list to `on_recover`, which seeds the
    // identity for recovery and drops the wizard into `nest_recovery`.
    let recoverable: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let recover_btn = Button::with_label(i18n::onboarding::launch::RECOVER_LOST_BOX);
    recover_btn.add_css_class("flat");
    recover_btn.set_visible(false);
    set_test_id(&recover_btn, ids::LAUNCH_RECOVER_BUTTON);
    {
        let on_recover = Rc::new(on_recover);
        let recoverable = recoverable.clone();
        recover_btn.connect_clicked(move |_| on_recover(recoverable.borrow().clone()));
    }
    buttons.append(&recover_btn);

    retry_box.append(&buttons);
    center.append(&retry_box);

    // ── Window ────────────────────────────────────────────────────────
    let window = adw::ApplicationWindow::builder()
        .default_width(600)
        .default_height(420)
        .resizable(false)
        .content(&outer)
        .build();
    window.set_application(Some(app));

    // Same close-handling as the onboarding window: app.hold() in main.rs
    // keeps the process alive for the authenticated session, so on a
    // pre-onboarding launch screen we have to quit on close or we leak
    // a headless zombie.
    {
        let app_for_quit = app.clone();
        window.connect_close_request(move |_| {
            app_for_quit.quit();
            gtk::glib::Propagation::Proceed
        });
    }

    // ── set_phase closure ─────────────────────────────────────────────
    let phase_state: Rc<RefCell<LaunchPhase>> = Rc::new(RefCell::new(LaunchPhase::Launching));
    let set_phase: Rc<dyn Fn(LaunchPhase)> = {
        let launching_box = launching_box.clone();
        let retry_box = retry_box.clone();
        let retry_title = retry_title.clone();
        let transient_error = transient_error.clone();
        let retry_btn = retry_btn.clone();
        let recover_btn = recover_btn.clone();
        let error_label = error_label.clone();
        let identity_warning = identity_warning.clone();
        let sign_in_refused_notice = sign_in_refused_notice.clone();
        let trust_btn = trust_btn.clone();
        let fallthrough_btn = fallthrough_btn.clone();
        let account_index_warning = account_index_warning.clone();
        let account_index_reset_btn = account_index_reset_btn.clone();
        let account_index_confirm_btn = account_index_confirm_btn.clone();
        let phase_state = phase_state.clone();
        Rc::new(move |phase: LaunchPhase| {
            // Each arm sets every property it cares about — set_phase may be
            // called repeatedly (Launching → … → terminal), so a stale property
            // from a prior phase must never leak through.
            match &phase {
                LaunchPhase::Launching => {
                    launching_box.set_visible(true);
                    retry_box.set_visible(false);
                    error_label.set_visible(false);
                    identity_warning.set_visible(false);
                    sign_in_refused_notice.set_visible(false);
                    trust_btn.set_visible(false);
                    fallthrough_btn.set_visible(true);
                    account_index_warning.set_visible(false);
                    account_index_reset_btn.set_visible(false);
                    account_index_confirm_btn.set_visible(false);
                    // Reset the recovery entry — the launch-time read re-runs on
                    // the next transient failure (set_recover_boxes re-shows it).
                    recover_btn.set_visible(false);
                }
                LaunchPhase::TransientRetry { error } => {
                    launching_box.set_visible(false);
                    error_label.set_visible(false);
                    identity_warning.set_visible(false);
                    sign_in_refused_notice.set_visible(false);
                    trust_btn.set_visible(false);
                    fallthrough_btn.set_visible(true);
                    account_index_warning.set_visible(false);
                    account_index_reset_btn.set_visible(false);
                    account_index_confirm_btn.set_visible(false);
                    retry_title.set_text(retry_title_text(&phase));
                    retry_title.set_visible(true);
                    transient_error.set_text(
                        &fauna_launch_machine::render_text::transient_error_text(error),
                    );
                    transient_error.set_visible(true);
                    retry_btn.set_visible(true);
                    retry_box.set_visible(true);
                    // `launch-recover-button` visibility is owned by
                    // `set_recover_boxes` (the async box-list read), fired after
                    // this surface paints — leave it as-is here.
                }
                LaunchPhase::NeedsUpdate { error } => {
                    // Non-retry "update required": reuse the failure box but render
                    // the localized message in `error-message` (matching web), drop
                    // the transient label, and hide the Retry button so only
                    // "Use a different nest" remains. version-compatibility.md Dim 4.
                    launching_box.set_visible(false);
                    retry_title.set_text(retry_title_text(&phase));
                    retry_title.set_visible(true);
                    transient_error.set_visible(false);
                    retry_btn.set_visible(false);
                    identity_warning.set_visible(false);
                    sign_in_refused_notice.set_visible(false);
                    trust_btn.set_visible(false);
                    fallthrough_btn.set_visible(true);
                    account_index_warning.set_visible(false);
                    account_index_reset_btn.set_visible(false);
                    account_index_confirm_btn.set_visible(false);
                    // A version-mismatch nest is reachable but outdated — recovery
                    // against it is not the offered action, so hide the entry.
                    recover_btn.set_visible(false);
                    // An empty message never registers as visible (apple's
                    // lesson, mirrored from tui's `LaunchSurface::error_text`):
                    // a blank label is worse than none, and the retry-box
                    // chrome above already carries the surface's title.
                    crate::settings::render_error_label(
                        &error_label,
                        (!error.is_empty()).then_some(error.as_str()),
                    );
                    retry_box.set_visible(true);
                }
                LaunchPhase::SignInRefused { error } => {
                    // The one terminal surface WITH a retry: the admin's restore
                    // happens off this device, and the machine honours
                    // `retry_silent_challenge()` from this state. The sentence
                    // goes in its OWN `launch-sign-in-refused-notice` element.
                    launching_box.set_visible(false);
                    retry_title.set_text(retry_title_text(&phase));
                    retry_title.set_visible(true);
                    transient_error.set_visible(false);
                    error_label.set_visible(false);
                    identity_warning.set_visible(false);
                    trust_btn.set_visible(false);
                    account_index_warning.set_visible(false);
                    account_index_reset_btn.set_visible(false);
                    account_index_confirm_btn.set_visible(false);
                    // Recovering a box is not the offered action against a nest
                    // that refused this identity.
                    recover_btn.set_visible(false);
                    sign_in_refused_notice.set_text(error);
                    sign_in_refused_notice.set_visible(true);
                    retry_btn.set_visible(true);
                    fallthrough_btn.set_visible(true);
                    retry_box.set_visible(true);
                }
                LaunchPhase::IdentityChanged { error } => {
                    // Same non-retry shape as NeedsUpdate, and for a stronger
                    // reason: the pin verdict is a possible-MITM signal, so a Retry
                    // CTA would both be futile and risk re-pinning by habit
                    // (never a retry loop, never a silent re-pin).
                    launching_box.set_visible(false);
                    retry_title.set_text(retry_title_text(&phase));
                    retry_title.set_visible(true);
                    transient_error.set_visible(false);
                    retry_btn.set_visible(false);
                    error_label.set_visible(false);
                    sign_in_refused_notice.set_visible(false);
                    fallthrough_btn.set_visible(true);
                    account_index_warning.set_visible(false);
                    account_index_reset_btn.set_visible(false);
                    account_index_confirm_btn.set_visible(false);
                    // Never offer to recover a box through a nest that just failed
                    // to prove it is that box.
                    recover_btn.set_visible(false);
                    // The warning goes in its OWN `nest-identity-changed-warning`
                    // element (ui.yaml `launch_identity_changed`), not the generic
                    // `error-message`, and the two explicit ways out are the trust
                    // button + the fallthrough.
                    identity_warning.set_text(error);
                    identity_warning.set_visible(true);
                    trust_btn.set_visible(true);
                    retry_box.set_visible(true);
                }
                LaunchPhase::AccountIndexUnreadable {
                    refusal,
                    confirming,
                } => {
                    // Never a retry or fallthrough — the nest was never
                    // contacted, so neither CTA means anything here.
                    launching_box.set_visible(false);
                    retry_title.set_text(retry_title_text(&phase));
                    retry_title.set_visible(true);
                    transient_error.set_visible(false);
                    retry_btn.set_visible(false);
                    error_label.set_visible(false);
                    identity_warning.set_visible(false);
                    sign_in_refused_notice.set_visible(false);
                    trust_btn.set_visible(false);
                    fallthrough_btn.set_visible(false);
                    recover_btn.set_visible(false);
                    use fauna_launch_machine::AccountIndexRefusal;
                    match (refusal, confirming) {
                        (AccountIndexRefusal::NewerBuild { .. }, _) => {
                            account_index_warning
                                .set_text(i18n::onboarding::launch::INDEX_NEWER_BUILD);
                            account_index_warning.set_visible(true);
                            account_index_reset_btn.set_visible(false);
                            account_index_confirm_btn.set_visible(false);
                        }
                        (AccountIndexRefusal::Malformed, false) => {
                            account_index_warning
                                .set_text(i18n::onboarding::launch::INDEX_MALFORMED);
                            account_index_warning.set_visible(true);
                            account_index_reset_btn.set_visible(true);
                            account_index_confirm_btn.set_visible(false);
                        }
                        (AccountIndexRefusal::Malformed, true) => {
                            account_index_warning
                                .set_text(i18n::onboarding::launch::INDEX_MALFORMED_RESET_RESIDUAL);
                            account_index_warning.set_visible(true);
                            account_index_reset_btn.set_visible(false);
                            account_index_confirm_btn.set_visible(true);
                        }
                    }
                    retry_box.set_visible(true);
                }
            }
            *phase_state.borrow_mut() = phase;
        })
    };

    // Two purely local account-index actions — no machine round trip, exactly
    // like tui's `LaunchAction::StartOver`/`ConfirmStartOver`
    // (`reveal_start_over`/`start_over`): reveal the confirm by re-entering
    // `set_phase` with `confirming: true`, or — once revealed — run the
    // documented floor. Both are guarded on the current phase so a stray
    // click (e.g. a delayed event after the surface changed) is inert.
    {
        let phase_state = phase_state.clone();
        let set_phase = set_phase.clone();
        account_index_reset_btn.connect_clicked(move |_| {
            let refusal = match &*phase_state.borrow() {
                LaunchPhase::AccountIndexUnreadable {
                    refusal: refusal @ fauna_launch_machine::AccountIndexRefusal::Malformed,
                    confirming: false,
                } => Some(*refusal),
                _ => None,
            };
            if let Some(refusal) = refusal {
                set_phase(LaunchPhase::AccountIndexUnreadable {
                    refusal,
                    confirming: true,
                });
            }
        });
    }
    {
        let phase_state = phase_state.clone();
        let on_reset = Rc::new(on_reset);
        account_index_confirm_btn.connect_clicked(move |_| {
            let armed = matches!(
                &*phase_state.borrow(),
                LaunchPhase::AccountIndexUnreadable {
                    refusal: fauna_launch_machine::AccountIndexRefusal::Malformed,
                    confirming: true,
                }
            );
            if armed {
                on_reset();
            }
        });
    }

    // ── set_recover_boxes closure ─────────────────────────────────────
    // Store the launch-time reachable-nest box list + reveal the recovery entry
    // when it's non-empty (the surviving-device path). Called best-effort from
    // the transient-retry handler once the async read resolves.
    let set_recover_boxes: Rc<dyn Fn(Vec<String>)> = {
        let recover_btn = recover_btn.clone();
        let recoverable = recoverable.clone();
        Rc::new(move |boxes: Vec<String>| {
            let has_boxes = !boxes.is_empty();
            *recoverable.borrow_mut() = boxes;
            recover_btn.set_visible(has_boxes);
        })
    };

    LaunchView {
        window,
        set_phase,
        set_recover_boxes,
        error_label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins each arm's retry-box title (2026-08-05 copy audit — NeedsUpdate and
    /// IdentityChanged re-label rather than reuse RETRY_TITLE, since both mean
    /// the nest WAS reached).
    #[test]
    fn retry_title_text_matches_the_reached_or_not_distinction() {
        assert_eq!(
            retry_title_text(&LaunchPhase::TransientRetry {
                error: "x".to_string()
            }),
            i18n::launch::RETRY_TITLE
        );
        assert_eq!(
            retry_title_text(&LaunchPhase::NeedsUpdate {
                error: "x".to_string()
            }),
            i18n::launch::NEEDS_UPDATE_TITLE
        );
        assert_eq!(
            retry_title_text(&LaunchPhase::SignInRefused {
                error: "x".to_string()
            }),
            i18n::onboarding::launch::SIGN_IN_REFUSED_TITLE
        );
        assert_eq!(
            retry_title_text(&LaunchPhase::IdentityChanged {
                error: "x".to_string()
            }),
            i18n::launch::IDENTITY_CHANGED_TITLE
        );
    }

    /// The two account-index verdicts title distinctly — the
    /// version case says "update needed", the malformed case says the saved
    /// accounts can't be read; neither reuses `RETRY_TITLE` since the nest
    /// was never contacted.
    #[test]
    fn account_index_unreadable_titles_by_verdict() {
        use fauna_launch_machine::AccountIndexRefusal;
        assert_eq!(
            retry_title_text(&LaunchPhase::AccountIndexUnreadable {
                refusal: AccountIndexRefusal::NewerBuild {
                    index_v: 2,
                    index_min: 2,
                    bin_v: 1,
                },
                confirming: false,
            }),
            i18n::onboarding::launch::INDEX_NEWER_BUILD_TITLE
        );
        assert_eq!(
            retry_title_text(&LaunchPhase::AccountIndexUnreadable {
                refusal: AccountIndexRefusal::Malformed,
                confirming: true,
            }),
            i18n::onboarding::launch::INDEX_MALFORMED_TITLE
        );
    }
}
