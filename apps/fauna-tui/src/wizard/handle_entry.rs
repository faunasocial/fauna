//! Stage 2: handle input + live handle-check (`onboarding.md` § 2).
//!
//! ui.yaml `onboarding.handle_entry` elements: `handle-input`,
//! `handle-check-button`, `handle-message-area`,
//! `handle-entry-continue-button`, `handle-entry-back-button`; optional
//! `handle-control-checkbox` (visible only on the `RegisteredNoNest` outcome).
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
