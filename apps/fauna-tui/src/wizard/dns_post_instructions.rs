//! Stage 7: manual DNS setup (`onboarding.md` § 7).
//!
//! ui.yaml `onboarding.dns_post_instructions`: `dns-post-instructions-text`,
//! `dns-post-instructions-copy-button`, `dns-post-instructions-continue-button`.
//!
//! Reached only on the deferred-DNS path ("Set up later" on § 4). It renders the
//! records the user must add at their registrar — apex/`mail` `A`, `MX`, `SPF`,
//! `_dmarc`. **No DKIM record**: the nest mints its own signing key on first
//! read post-boot, so a client-published DKIM TXT would advertise a key the
//! bridge never signs with.
//!
//! The record text is the machine's pre-rendered `dns_post_instructions()`
//! markdown — the same string linux paints and the copy button copies. A second,
//! client-local formatter here could drift from what the page shows, and it is
//! precisely the copy that has to be paste-accurate.
//!
//! Continue is terminal: `continue_from_dns_post_instructions()` returns `Done`
//! and sets `wizard_outcome()` to `AwaitingManualDns`, which is what routes the
//! app onto the "Almost ready" surface (`wizard/awaiting_manual_dns.rs`) and
//! persists the awaiting-DNS launch slot.

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::dns_post_instructions as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::DESCRIPTION.to_string()]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    vec![
        Element::label(
            ids::DNS_POST_INSTRUCTIONS_TEXT,
            w.machine.dns_post_instructions().unwrap_or_default(),
        ),
        Element::button(
            ids::DNS_POST_INSTRUCTIONS_COPY_BUTTON,
            t::COPY_BUTTON,
            true,
            Action::CopyDnsPostInstructions,
        ),
        Element::button(
            ids::DNS_POST_INSTRUCTIONS_CONTINUE_BUTTON,
            common::CONTINUE,
            true,
            Action::ContinueFromDnsPostInstructions,
        ),
    ]
}
