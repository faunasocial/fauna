//! Stage 3b-bis: the NAT-mode choice — the terminal admin-path setup step
//! (`onboarding.md` § 3b-bis).
//!
//! ui.yaml `onboarding.nat_mode_choice` elements: `public-nat-mode-radio`,
//! `private-nat-mode-radio`, `nat-mode-confirm-button`, `nat-mode-defer-button`,
//! `nat-mode-status`.
//!
//! **No Back button** — like the storage-mode step, the admin is
//! server-committed by the time this page shows.
//!
//! `selected_mode` is pre-seeded in shared Rust from the nest's resolved
//! `node_mode` (refined private-ward for a private-network handle target), so
//! the common case is confirm-only: one click on the already-selected option.
//! The client renders the snapshot and never re-derives the default.
//!
//! Defer keeps the seeded mode — a working default — and the admin can change
//! it later from the admin-nest page, so neither exit can strand the nest.

use fauna_core::nat_mode::NodeMode;
use fauna_i18n::strings::onboarding::nat_mode as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard, localized};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::DESCRIPTION.to_string()]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let snap = w.machine.nat_mode_snapshot();
    let public = snap.selected_mode == NodeMode::Public;

    vec![
        // One-of-N, so `Radio`, not a checkbox stack (`apps/tui.md` § Rendering
        // → *Control vocabulary*, rule 1) — the pre-seeded selection is
        // deliberate (machine-derived from the install) and explained by the
        // page description, so exactly one option is always marked here.
        Element::radio_gesture(
            ids::PUBLIC_NAT_MODE_RADIO,
            t::PUBLIC_LABEL,
            public,
            crate::element::Gesture::Wizard(Action::SelectNatMode(NodeMode::Public)),
        )
        .attr("state", if public { "on" } else { "off" }),
        Element::radio_gesture(
            ids::PRIVATE_NAT_MODE_RADIO,
            t::PRIVATE_LABEL,
            !public,
            crate::element::Gesture::Wizard(Action::SelectNatMode(NodeMode::Private)),
        )
        .attr("state", if public { "off" } else { "on" }),
        Element::button(
            ids::NAT_MODE_CONFIRM_BUTTON,
            t::CONFIRM_BUTTON,
            snap.submit_enabled,
            Action::SubmitNatModeChoice,
        ),
        Element::button(
            ids::NAT_MODE_DEFER_BUTTON,
            t::DEFER_BUTTON,
            true,
            Action::DeferNatModeChoice,
        ),
        // The machine owns the wording of every state (`Choosing` /
        // `Submitting` / `Done` / `Error`, plus the private-network hint), handed
        // over as a `LocalizedText` — never re-derived here.
        Element::label(ids::NAT_MODE_STATUS, localized(&snap.message)),
    ]
}
