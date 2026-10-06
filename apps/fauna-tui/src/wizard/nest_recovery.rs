//! Box-recovery step 4 — the box-selection hub (`box-recovery.md` § Recovery UI
//! (step 4)).
//!
//! ui.yaml `onboarding.nest_recovery` — required: `recover-box-list`,
//! `recover-box-item` (indexed), `recover-method-cloud-button`,
//! `recover-method-selfhosted-button`, `recover-back-button`. Optional:
//! `recover-box-empty-message`.
//!
//! One selectable row per box the admin custodies — `m.recovery_boxes()`, whose
//! production list is pushed in by `app.rs`'s page-entry fetch (a reachable-nest
//! deployment-seed plane read; `crate::recovery`). Picking a row (`select_recovery_box`)
//! enables the two re-provision methods, exactly as the machine's own
//! `require_selected_recovery_box` guard demands; an empty list shows
//! `recover-box-empty-message` instead of the rows.
//!
//! **Only public `nest_actor_id`s are on this page.** The custodied seed stays
//! in the `fauna.state.deployment-seeds` plane and is resolved back in Rust by the re-provision drive — the one
//! place it is deliberately surfaced is the self-hosted installer command
//! (`super::recover_selfhosted_instructions`).
//!
//! A row is a **checkbox**, not a plain button: it is a selection, and the
//! `[x] …` paint is how tui already renders the sibling `dns-provider-row[…]` /
//! `vps-provider-row[…]` selectable rows (one selected-row idiom, not two).

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::recovery as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::SUBTITLE.to_string()]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let boxes = w.machine.recovery_boxes();
    let selected = w.machine.recovery_selected_nest_id();
    let mut out: Vec<Element> = Vec::new();

    if boxes.is_empty() {
        // Nothing custodied here yet: neither this device's own account store
        // nor a reachable nest holds a custody row (`box-recovery.md` § The
        // plane-era recovery floor, (b) The reads).
        out.push(Element::label(
            ids::RECOVER_BOX_EMPTY_MESSAGE,
            t::EMPTY_MESSAGE,
        ));
    } else {
        out.push(Element::label(ids::RECOVER_BOX_LIST, t::BOX_LIST_LABEL));
        for (i, id) in boxes.iter().enumerate() {
            out.push(
                Element::checkbox(
                    format!("recover-box-item-{i}"),
                    // The short id is the row's label; the row's *value* to a
                    // driver is that same text. `BOX_ITEM_HINT` is chrome, so it
                    // rides the paint-only label rather than the registry text.
                    fauna_core::format::short_nest_id(id),
                    selected.as_deref() == Some(id.as_str()),
                    Action::SelectRecoveryBox(id.clone()),
                )
                .labelled(t::BOX_ITEM_HINT)
                .within(ids::RECOVER_BOX_LIST, 0),
            );
        }
    }

    // Both methods are gated on a selection — the client mirrors the machine's
    // `require_selected_recovery_box` guard rather than letting the user click
    // into an error.
    let has_selection = selected.is_some();
    out.push(Element::button(
        ids::RECOVER_METHOD_CLOUD_BUTTON,
        t::METHOD_CLOUD,
        has_selection,
        Action::RecoverViaCloud,
    ));
    out.push(Element::button(
        ids::RECOVER_METHOD_SELFHOSTED_BUTTON,
        t::METHOD_SELFHOSTED,
        has_selection,
        Action::RecoverViaSelfhosted,
    ));
    // `back()` returns to handle_entry on the came-from-identity entry (Q2-A);
    // the came-from-launch entry stays put by design — the launch glue owns that
    // exit.
    out.push(Element::button(
        ids::RECOVER_BACK_BUTTON,
        common::BACK,
        true,
        Action::Back,
    ));
    out
}
