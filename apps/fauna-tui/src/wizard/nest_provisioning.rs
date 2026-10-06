//! Stage 6: nest provisioning (`onboarding.md` § 6).
//!
//! ui.yaml `onboarding.nest_provisioning`: the `provisioning-progress`
//! **component** (whose five child IDs — `provisioning-step-row`,
//! `provisioning-step-checkbox`, `provisioning-step-label`,
//! `provisioning-substep`, `provisioning-step-error` — repeat once per step),
//! plus `provisioning-start-button`, `provisioning-cancel-button`,
//! `provisioning-retry-button`, `provisioning-elapsed`,
//! `provisioning-continue-button`, `provisioning-back-button`, and the
//! top-region price summary — `provisioning-price-bom` (container, always
//! present), `provisioning-bom-domain-line` (optional, only when buying a new
//! domain), `provisioning-bom-vps-line` (always present) — sourced from
//! `OnboardingMachine::bom_domain_line()` / `bom_vps_line()`.
//!
//! **This page is the first on tui to nest elements under an indexed
//! container.** The four step rows are `provisioning-step-row[0..3]` (Domain,
//! Server, Dns, Online, in fixed order), and each row's children carry the scope
//! path `[(provisioning-step-row, i)]` — so a test scopes
//! `get_text("provisioning-substep", scope="provisioning-step-row[2]")` rather
//! than counting globally and slicing (the "scoped queries, not global counts"
//! rule). The child IDs are only addressable *through* that scope, which is
//! exactly how ui.yaml models them.
//!
//! **Nothing on this page re-derives a display rule** (`onboarding.md:252,263,265`):
//!
//! | Question | Shared source |
//! |---|---|
//! | Show the substep row? | `StepSnapshot::shows_substep` |
//! | Show the error row? | `StepSnapshot::shows_error` |
//! | Append "(attempt n of m)"? | `StepSnapshot::shows_attempt_suffix` |
//! | Which buttons are live? | `OverallStatus::{is_idle,is_running,can_retry,can_continue}` |
//! | Step glyph / label / substep text | `status_glyph` / `step_label` / `substep_label` |
//! | Elapsed time | `progress::elapsed_display` |
//!
//! The error row's rule in particular is `Failed && !empty`, *not* "when
//! `last_error` is populated" — a `Running` row mid-retry still carries the
//! previous attempt's error, which must not surface. That rule lives in
//! `StepSnapshot::recompute_display`; reading the boolean is how we inherit it.

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::nest_provisioning as t;
use fauna_i18n::strings::onboarding::provision;
use fauna_provisioning::progress::{self, StepSnapshot};
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard, localized};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description(w: &Wizard) -> Vec<String> {
    let snap = w.machine.provisioning_snapshot();
    match snap.final_error {
        Some(e) if !e.is_empty() => vec![e],
        _ => Vec::new(),
    }
}

/// The text of one step's substep line: the typed `SubstepKey` through the i18n
/// table (never English), plus the attempt suffix when the shared snapshot says
/// to show one.
fn substep_text(step: &StepSnapshot) -> String {
    let mut text = step
        .substep
        .map(|k| localized(&progress::substep_label(k, step.last_error.clone())))
        .unwrap_or_default();
    if step.shows_attempt_suffix {
        // The template already carries its own leading space.
        text.push_str(&provision::step_attempt_template(
            &step.attempt.to_string(),
            &step.max_attempts.to_string(),
        ));
    }
    text
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let m = &w.machine;
    // `provisioning_snapshot()` returns a clone with the `shows_*` display
    // fields already enriched — never reach into the machine's mutex directly,
    // or the flags read stale.
    let snap = m.provisioning_snapshot();
    let overall = snap.overall;

    // The progress region itself. ui.yaml scopes `provisioning-progress` in the
    // page's `elements` (it is both the component *and* a page element, and
    // linux registers the container widget under that id), so the page must
    // paint it — `test_idle_renders_four_pending_rows` asserts it is visible
    // before it reads a single row.
    //
    // The step rows are deliberately **not** nested under it in the scope path —
    // it is a *page* element, not a container. Nesting them would no longer
    // break the shared `scope="provisioning-step-row[i]"` reads: since the
    // 2026-08-14 descendant ruling (e2e-conventions.md § convention 1) a scope
    // resolves through unnamed ancestors, exactly as AT-SPI always did for
    // linux. Before that ruling it would have pushed the rows out of prefix
    // position and silently resolved every scoped row read to nothing — the
    // divergence the ruling closed.
    let mut out = vec![Element::label(ids::PROVISIONING_PROGRESS, "")];

    // Top-region price summary ("Bill of Materials") — a pre-commit recap of
    // up to two priced line items from `bill_of_materials()`: the domain's
    // one-time registration price (only when buying a new domain) and the
    // selected VPS's monthly price (always present once `vps_config`'s
    // Continue has run). Pure computation over already-in-state DNS/VPS
    // data — no IO, so this just re-derives on every observer tick like the
    // rest of the page. `onboarding.md` §6.
    out.push(Element::label(ids::PROVISIONING_PRICE_BOM, ""));
    // Pre-derived by the shared machine (`resolve_nested`, not `localized`/
    // `resolve`: `{label}` carries the step's own i18n key). Mirrors linux's
    // `onboarding::nest_provisioning`.
    if let Some(text) = m.bom_domain_line() {
        out.push(Element::label(
            ids::PROVISIONING_BOM_DOMAIN_LINE,
            text.resolve_nested(fauna_i18n::strings::lookup),
        ));
    }
    if let Some(text) = m.bom_vps_line() {
        out.push(Element::label(
            ids::PROVISIONING_BOM_VPS_LINE,
            text.resolve_nested(fauna_i18n::strings::lookup),
        ));
    }

    for (i, step) in snap.steps.iter().enumerate() {
        let row = "provisioning-step-row";
        // The row itself is the indexed container; its children hang off it.
        out.push(Element::label(row, "").within(row, i));
        out.push(
            Element::label(
                ids::PROVISIONING_STEP_CHECKBOX,
                progress::status_glyph(step.status),
            )
            .within(row, i),
        );
        out.push(
            Element::label(
                ids::PROVISIONING_STEP_LABEL,
                localized(&progress::step_label(step.kind)),
            )
            .within(row, i),
        );
        if step.shows_substep {
            out.push(Element::label(ids::PROVISIONING_SUBSTEP, substep_text(step)).within(row, i));
        }
        if step.shows_error {
            out.push(
                Element::label(
                    ids::PROVISIONING_STEP_ERROR,
                    step.last_error.clone().unwrap_or_default(),
                )
                .within(row, i),
            );
        }
    }

    if let Some(elapsed) =
        progress::elapsed_display(snap.started_at_ms, snap.finished_at_ms, progress::now_ms())
    {
        out.push(Element::label(
            ids::PROVISIONING_ELAPSED,
            localized(&elapsed),
        ));
    }

    // Affordances are the typed predicates', not ours — `can_retry()` is the one
    // carrying the Failed-**or-Cancelled** rule (a soft-cancel must leave retry
    // reachable, else the user is stranded with only Back).
    if overall.is_idle() {
        out.push(Element::button(
            ids::PROVISIONING_START_BUTTON,
            t::START_BUTTON,
            true,
            Action::StartProvisioning,
        ));
    }
    if overall.is_running() {
        out.push(Element::button(
            ids::PROVISIONING_CANCEL_BUTTON,
            t::CANCEL_BUTTON,
            true,
            Action::CancelProvisioning,
        ));
    }
    if overall.can_retry() {
        out.push(Element::button(
            ids::PROVISIONING_RETRY_BUTTON,
            t::RETRY_BUTTON,
            true,
            Action::RetryProvisioning,
        ));
    }

    out.push(Element::button(
        ids::PROVISIONING_BACK_BUTTON,
        common::BACK,
        true,
        Action::Back,
    ));
    out.push(Element::button(
        ids::PROVISIONING_CONTINUE_BUTTON,
        common::CONTINUE,
        overall.can_continue(),
        Action::ContinueFromProvisioning,
    ));
    // Rule 5's reason for the Continue directly above, from the same value
    // that disabled it. ID-less chrome: it is prose, not an affordance, and
    // rule 5's own exemplar is un-id'd on tui (the `dns-status-text` fix's
    // shape) — so no ui.yaml ID is minted for it.
    if let Some(reason) = overall.continue_blocked_reason() {
        out.push(Element::chrome(localized(&reason)));
    }
    out
}

#[cfg(test)]
mod tests {
    use fauna_onboarding_machine::OnboardingStep;

    use super::*;

    /// At entry this screen paints a dead Continue under four `○` glyphs and
    /// said nothing about either — a symbol is not a reason (`ui/README.md`
    /// § Copy comprehensibility rule 5). The line comes from
    /// `OverallStatus::continue_blocked_reason()`, the sibling of the very
    /// predicate that disabled the button, so this is one line of tui evidence
    /// for a seven-app fix.
    #[test]
    fn the_idle_screen_explains_its_disabled_continue() {
        let app = crate::app::tests::test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::NestProvisioning);

        let els = app.wizard.elements();
        let cont = els
            .iter()
            .find(|e| e.id == "provisioning-continue-button")
            .expect("provisioning-continue-button");
        assert!(!cont.enabled, "precondition: Continue is dead at entry");

        let reason = localized(
            &fauna_provisioning::progress::OverallStatus::Idle
                .continue_blocked_reason()
                .expect("Idle blocks Continue, so it owes a reason"),
        );
        assert!(!reason.is_empty(), "a blank line explains nothing");
        assert!(
            els.iter().any(|e| e.text == reason),
            "the screen must paint the reason its own predicate produced; got {:?}",
            els.iter().map(|e| &e.text).collect::<Vec<_>>()
        );
    }
}
