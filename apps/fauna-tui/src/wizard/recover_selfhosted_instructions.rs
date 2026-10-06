//! Box-recovery step 4 — self-hosted seed install (`box-recovery.md` § Recovery
//! UI (step 4)).
//!
//! ui.yaml `onboarding.recover_selfhosted_instructions`:
//! `recover-selfhosted-command`, `recover-selfhosted-copy-button`,
//! `recover-selfhosted-continue-button`, `recover-restore-cta`.
//!
//! The admin runs their installer on a fresh box with the rendered
//! `FAUNA_DEPLOYMENT_SEED=<64-hex>` line, so the rebuilt box adopts the *saved*
//! deployment identity and re-presents the same `nest_actor_id` — every
//! TOFU-pinned client then reconnects without a trust break.
//!
//! **The command is the real one, resolved from the account plane's custody
//! map.** `app.rs` fires the read on page entry
//! (`crate::recovery::load_selfhosted_command` — this device's own account store
//! joined with a cold read from the nest, when one is known; `box-recovery.md`
//! § The plane-era recovery floor), which resolves the selected box's custodied
//! seed in Rust and renders it through the one shared
//! `fauna_client_config::selfhosted_recovery_command_in`, so every app emits a
//! byte-identical line. Until that read lands — and when no source custodies
//! the box — the page shows the pending placeholder. It must never show a command carrying the *wrong* box's seed:
//! that would rebuild the box under a different identity, which is precisely the
//! trust break recovery exists to avoid.
//!
//! The seed IS surfaced here, by design — it is the installer input the admin
//! pastes (`box-recovery.md` § Trust & audience). That is the one deliberate
//! exception to "the seed never leaves Rust".

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::recovery as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard};

pub fn title() -> String {
    t::SELFHOSTED_TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::SELFHOSTED_DESC.to_string()]
}

/// What `recover-selfhosted-command` renders: the resolved installer line, or
/// the pending placeholder while the reachable-nest read is in flight (or when
/// no surviving nest can serve it).
pub fn command_text(w: &Wizard) -> String {
    w.selfhosted_command
        .clone()
        .unwrap_or_else(|| t::SELFHOSTED_COMMAND_PENDING.to_string())
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let command = command_text(w);
    // Nothing to copy while the placeholder is up — copying "the command will
    // appear here" into the admin's clipboard is worse than a disabled button.
    let resolved = w.selfhosted_command.is_some();
    vec![
        Element::label(ids::RECOVER_SELFHOSTED_COMMAND, command),
        Element::button(
            ids::RECOVER_SELFHOSTED_COPY_BUTTON,
            common::COPY,
            resolved,
            Action::CopySelfhostedCommand,
        ),
        // Both exits leave the recovery flow: the rebuilt box reconnects through
        // the normal launch flow once the admin has run the installer and it is
        // reachable. The restore CTA deep-links to the Backups page on web; on
        // tui the Backups page lives in the authenticated shell (M8), which the
        // pre-auth wizard cannot reach — and the box being recovered is not up
        // yet anyway, so there is nothing to restore *into* from here.
        Element::button(
            ids::RECOVER_RESTORE_CTA,
            t::RESTORE_CTA,
            true,
            Action::ExitRecovery,
        ),
        Element::button(
            ids::RECOVER_SELFHOSTED_CONTINUE_BUTTON,
            t::SELFHOSTED_CONTINUE,
            true,
            Action::ExitRecovery,
        ),
    ]
}
