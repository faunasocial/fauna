//! Stage 3: request an invite to a claimed nest (`onboarding.md` § 3).
//!
//! ui.yaml `onboarding.invite_request` elements: `invite-request-submit-button`,
//! `invite-request-status`, `invite-code-input`, `invite-code-check-button`,
//! `invite-code-status`, `invite-request-continue-button`,
//! `invite-request-back-button`; optional `invite-request-recheck-button`
//! (visible only during `PendingReview`) and `invite-code-supervised-notice`
//! (only when the checked code carries a guardian designation).
//!
//! Two independent rows: the admin-flow request on top, the out-of-band code
//! below. Both status labels render the snapshot's `LocalizedText` — the OOB
//! row's text is `oob_message`, derived in shared Rust so no client recomputes
//! it from the state variant (`onboarding.md` Architectural rule 4).

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::invite_request as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Field, Wizard, WizardField, localized};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::SUBTITLE.to_string(), t::CODE_SECTION_TITLE.to_string()]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let snap = w.machine.invite_request_snapshot();
    let code = w.field(WizardField::InviteCode);

    let mut out = vec![
        Element::button(
            ids::INVITE_REQUEST_SUBMIT_BUTTON,
            t::REQUEST_BUTTON,
            !w.machine.is_loading(),
            Action::SubmitInviteRequest,
        ),
        Element::label(ids::INVITE_REQUEST_STATUS, localized(&snap.message)),
    ];

    if snap.recheck_visible {
        out.push(Element::button(
            ids::INVITE_REQUEST_RECHECK_BUTTON,
            t::RECHECK_BUTTON,
            true,
            Action::RecheckInviteStatus,
        ));
    }

    // Labelled: an unlabelled input prompts with its raw element id
    // (`apps/tui.md` § Rendering → *Control vocabulary*).
    out.push(
        Element::input(
            ids::INVITE_CODE_INPUT,
            code.clone(),
            Field::Wizard(WizardField::InviteCode),
        )
        .labelled(t::CODE_LABEL),
    );
    out.push(Element::button(
        ids::INVITE_CODE_CHECK_BUTTON,
        common::CHECK,
        !code.is_empty(),
        Action::VerifyOobInviteCode,
    ));
    out.push(Element::label(
        ids::INVITE_CODE_STATUS,
        localized(&snap.oob_message),
    ));

    // `invite-code-supervised-notice` — "This account will be supervised by X",
    // rendered BEFORE redemption when the checked out-of-band code carries a
    // guardian designation (`family-safety.md` § Wire & data shape: the additive
    // `supervised_by` on the `fauna.account.invite_code.verify` reply, surfaced by
    // the shared machine as `OobCodeState::Valid { supervised_by }`). Transparency
    // at creation — the supervised user knows before they redeem.
    //
    // Registered ONLY for a *valid* code that names a guardian (an idle/verifying/
    // invalid code, or a valid ordinary one, registers nothing), so the driver's
    // `is_visible` is an honest witness — the linux label's `set_visible(false)`
    // arm, expressed the terminal way.
    if let fauna_onboarding_machine::OobCodeState::Valid {
        supervised_by: Some(guardian),
        ..
    } = &snap.out_of_band_code_state
    {
        out.push(Element::label(
            ids::INVITE_CODE_SUPERVISED_NOTICE,
            fauna_i18n::strings::family::supervised_notice_onboarding(guardian),
        ));
    }

    out.push(Element::button(
        ids::INVITE_REQUEST_CONTINUE_BUTTON,
        common::CONTINUE,
        snap.continue_enabled,
        Action::InviteContinue,
    ));
    out.push(Element::button(
        ids::INVITE_REQUEST_BACK_BUTTON,
        common::BACK,
        true,
        Action::CancelInviteOpAndBack,
    ));
    out
}

#[cfg(test)]
mod tests {
    use fauna_onboarding_machine::{InviteRequestSnapshot, OnboardingStep, OobCodeState};

    /// Paint the invite-request step with the OOB row in `oob`, returning every
    /// painted id plus the supervised notice's text (`None` when it does not
    /// register at all — what the driver's `is_visible` reads).
    fn paint(oob: OobCodeState) -> (Vec<String>, Option<String>) {
        let app = crate::app::tests::test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::InviteRequest);
        app.wizard
            .machine
            .set_invite_request_snapshot_for_test(InviteRequestSnapshot {
                out_of_band_code_state: oob,
                ..InviteRequestSnapshot::idle()
            });
        // Through the step router, so this proves the wired page, not a helper.
        let els = app.wizard.elements();
        let ids = els.iter().map(|e| e.id.clone()).collect();
        let notice = els
            .iter()
            .find(|e| e.id == "invite-code-supervised-notice")
            .map(|e| e.text.clone());
        (ids, notice)
    }

    /// A checked code carrying a guardian designation surfaces
    /// `invite-code-supervised-notice`, naming the guardian, BEFORE redemption
    /// (`family-safety.md` § Wire & data shape). Every other OOB state — idle,
    /// verifying, invalid, and a *valid ordinary* code — registers no notice at
    /// all, so `is_visible` is an honest witness rather than a blank label that
    /// always reads visible.
    #[test]
    fn supervised_notice_only_for_a_supervised_code() {
        let (ids, notice) = paint(OobCodeState::Valid {
            invite_id: "inv-1".to_string(),
            supervised_by: Some("family-guardian3".to_string()),
        });
        assert_eq!(
            notice.as_deref(),
            Some("This account will be supervised by family-guardian3"),
            "the notice names the guardian the code designates"
        );
        // It sits with the OOB row, after its status and before Continue.
        let at = |id: &str| ids.iter().position(|e| e == id).expect(id);
        assert!(at("invite-code-status") < at("invite-code-supervised-notice"));
        assert!(at("invite-code-supervised-notice") < at("invite-request-continue-button"));

        for state in [
            OobCodeState::Idle,
            OobCodeState::Verifying,
            OobCodeState::Invalid {
                reason: "expired".to_string(),
            },
            OobCodeState::Error {
                cause: "boom".to_string(),
            },
            OobCodeState::Valid {
                invite_id: "inv-2".to_string(),
                supervised_by: None,
            },
        ] {
            let (ids, notice) = paint(state.clone());
            assert_eq!(notice, None, "{state:?} must register no notice");
            assert!(
                ids.iter().any(|id| id == "invite-code-status"),
                "the rest of the OOB row still paints for {state:?}"
            );
        }
    }
}
