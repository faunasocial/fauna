//! Stage 2: handle input + live handle-check (`onboarding.md` § 2).
//!
//! ui.yaml `onboarding.handle_entry` elements: `handle-input`,
//! `handle-check-button`, `handle-message-area`,
//! `handle-entry-continue-button`, `handle-entry-back-button`; optional
//! `handle-control-checkbox` (visible only on the `RegisteredNoNest` outcome)
//! and the terms notice `onboarding-terms-notice` + `onboarding-terms-link`
//! (only while `terms_notice_visible` — a Continue that signs in), with the
//! nest's rules `onboarding-nest-rules` + `onboarding-nest-rules-item` directly
//! above it on the same condition.
//!
//! Everything is rendered from `handle_check_snapshot()`. The client recomputes
//! nothing: `continue_enabled`, the checkbox's visibility/checked state and the
//! `LocalizedText` message all come from the snapshot. The one client-side rule
//! is the doc's own — "Check stays disabled while the input is empty"
//! (`onboarding.md:49`) — plus not re-arming Check mid-probe.

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::handle as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Field, Wizard, WizardField, localized};

pub fn title() -> String {
    t::PROMPT.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::EXAMPLES_HELP.to_string(), t::LOCALHOST_HINT.to_string()]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let snap = w.machine.handle_check_snapshot();
    let handle = w.field(WizardField::Handle);

    let mut out = vec![
        // Every input MUST carry a label (`apps/tui.md` § Rendering → *Control
        // vocabulary*): without one the paint falls back to the element id, so
        // the very first field of onboarding prompted "handle-input: _" — a
        // machine string shown to a human. Walk invariant I3 exists for exactly
        // this class but reads `page_elements()`, which the wizard never
        // populates, so it never saw this surface.
        Element::input(
            ids::HANDLE_INPUT,
            handle.clone(),
            Field::Wizard(WizardField::Handle),
        )
        .labelled(common::HANDLE),
        Element::button(
            ids::HANDLE_CHECK_BUTTON,
            common::CHECK,
            !handle.is_empty() && !w.machine.is_loading(),
            Action::StartHandleCheck,
        ),
        Element::label(ids::HANDLE_MESSAGE_AREA, localized(&snap.message)),
    ];

    if snap.control_checkbox_visible {
        out.push(Element::checkbox(
            ids::HANDLE_CONTROL_CHECKBOX,
            t::CONTROL_CHECKBOX,
            snap.control_checkbox_checked,
            Action::SetControlCheckbox(!snap.control_checkbox_checked),
        ));
    }

    // Above the bottom row, only while Continue signs in — the snapshot's
    // field, never an outcome comparison here.
    // The nest's rules sit directly above it, on the same condition — the
    // machine's getter applies it (`nest-rules.md` § Acceptance at joining).
    if snap.terms_notice_visible {
        if let Some(rules) = w.machine.joining_nest_rules(crate::nest_rules::UI_LANG) {
            out.extend(crate::nest_rules::joining_elements(&rules));
        }
        out.extend(super::terms_notice());
    }

    out.push(Element::button(
        ids::HANDLE_ENTRY_CONTINUE_BUTTON,
        common::CONTINUE,
        snap.continue_enabled,
        Action::SubmitHandleCheckContinue,
    ));
    out.push(Element::button(
        ids::HANDLE_ENTRY_BACK_BUTTON,
        common::BACK,
        true,
        Action::Back,
    ));
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fauna_onboarding_machine::OnboardingStep;
    use fauna_ui_ids as ids;

    use crate::wizard::WizardField;

    /// `test@localhost` with no nest running — a default install, which sets
    /// up the app but no nest — must not ask whether the user controls DNS for
    /// `localhost`. Nobody controls DNS for a local target (`onboarding.md`
    /// § 2, Local / self-hosted targets), so the DNS-control checkbox and its
    /// DNS-config route are a dead end there: the page says no nest is running
    /// at that address and offers nothing to continue to.
    ///
    /// Drives the REAL handle check: the probe's `nest` override points at a
    /// port with nothing listening, the same seam the desktop app uses to aim
    /// a bare-loopback handle at its own same-box nest.
    #[tokio::test]
    async fn localhost_with_no_nest_never_asks_about_dns() {
        let mut app = crate::app::tests::test_app();
        let machine = app.wizard.machine.clone();
        machine.begin_create_identity();
        machine
            .confirm_generated_identity()
            .expect("a generated secret exists after begin_create_identity");
        // Past the recovery-kit offer, which the fresh identity lands on.
        machine.set_step_for_test(OnboardingStep::HandleEntry);

        // Bind then drop: the port is free, so the probe is refused.
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("bind an ephemeral port")
            .port();
        machine.set_provider_base_urls(HashMap::from([(
            "nest".to_string(),
            format!("http://127.0.0.1:{closed}"),
        )]));

        app.wizard
            .set_field(WizardField::Handle, "test@localhost".to_string());
        // What `Action::StartHandleCheck` runs.
        machine.start_handle_check(machine.current_handle()).await;

        let els = app.wizard.elements();
        assert!(
            !els.iter().any(|e| e.id == ids::HANDLE_CONTROL_CHECKBOX),
            "a local target has no DNS to control — no control checkbox"
        );
        let message = els
            .iter()
            .find(|e| e.id == ids::HANDLE_MESSAGE_AREA)
            .map(|e| e.text.clone())
            .expect("the message area paints");
        assert!(
            !message.contains("DNS"),
            "the message must not ask about DNS for localhost: {message:?}"
        );
        assert!(
            message.contains("localhost"),
            "the message names the address that has no nest: {message:?}"
        );
        let continue_enabled = els
            .iter()
            .find(|e| e.id == ids::HANDLE_ENTRY_CONTINUE_BUTTON)
            .map(|e| e.enabled)
            .expect("Continue paints");
        assert!(!continue_enabled, "nothing to continue to without a nest");

        // Even a forced checkbox tick must not route a local target to DNS.
        machine.set_control_checkbox(true);
        assert_ne!(
            machine.submit_handle_check_continue().await,
            OnboardingStep::DnsConfig,
            "a local target never routes to DNS configuration"
        );
    }
}
