//! The headless credential-store unlock/create surface (`tui.md` § Credential
//! storage; ui.yaml page `tui-unlock`, user-approved 2026-07-14).
//!
//! Shown **before** launch routing whenever the credential store resolved to
//! the sealed passphrase backend and is still locked — `launch::start` reads
//! the long-term store, so nothing may run until the store can serve it. Two
//! modes on one page, keyed off whether a sealed file exists:
//!
//! - **Unlock** (file exists): one passphrase input; a wrong passphrase paints
//!   the honest wrong-or-corrupt message in `error-message` and stays.
//! - **Create** (first headless run): passphrase + confirm inputs. Created
//!   *before* onboarding on purpose — mid-wizard exits (a pending invite, a
//!   deferred-DNS claim code) persist durable secrets, so the store must be
//!   writable from the wizard's first step.
//!
//! The submit runs **synchronously** on both actuation paths: Argon2id at the
//! interactive cost is ~100ms, well under the driver's single-shot-read
//! budget, and a spawned derive would let the driver read a pre-unlock frame.
//!
//! The passphrase never enters the automation registry: the inputs register
//! with a bullet-masked `text`, and the agent's `type` path appends through
//! [`crate::app::App::field`] (the real buffer), so masking costs it nothing.

use fauna_i18n::strings::tui_unlock as t;
use fauna_ui_ids as ids;

use crate::app::{App, UiMessage};
use crate::element::{Element, Field};

/// Which affordance the surface shows (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Unlock,
    Create,
}

/// The surface's local state: the two input buffers and the last error.
/// Buffers are cleared on success (and best-effort on failure of the confirm
/// check) so a later frame can never paint a stale passphrase length.
#[derive(Default)]
pub struct UnlockState {
    pub passphrase: String,
    pub confirm: String,
    pub error: Option<String>,
}

/// The one gesture this surface dispatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockAction {
    Submit,
}

/// An unlock-surface editable field (`crate::unlock`).
///
/// `App::field` serves the RAW buffer for both — the passphrase never enters
/// the automation registry (module docs): only the painted/registered element
/// `text` is masked ([`elements`]), never the value [`field`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnlockField {
    /// `tui-unlock-passphrase-input` — a local buffer on `App::unlock`. The
    /// element's registered `text` is a same-length **mask**, never the value
    /// (the buffer itself is what `App::field` serves the type path). Enter
    /// submits (the `ReplyRecipientAdd`-style special case in the keymap).
    Passphrase,
    /// `tui-unlock-confirm-input` — the create-mode second entry; same
    /// masking + submit-on-Enter contract as [`Self::Passphrase`].
    Confirm,
}

/// Read the RAW buffer for a field — never the masked/registered text (see
/// [`UnlockField`] docs).
pub fn field(state: &UnlockState, field: &UnlockField) -> String {
    match field {
        UnlockField::Passphrase => state.passphrase.clone(),
        UnlockField::Confirm => state.confirm.clone(),
    }
}

/// Write a field's RAW buffer.
pub fn set_field(state: &mut UnlockState, field: UnlockField, value: String) {
    match field {
        UnlockField::Passphrase => state.passphrase = value,
        UnlockField::Confirm => state.confirm = value,
    }
}

/// Mask an input buffer for paint + the automation registry: same length,
/// no content. `get_text` on a passphrase input answers this, never the value.
/// `pub(crate)` — the settings credential-store re-key modal's three inputs
/// carry the same contract (`settings.md` § Credential store), one mask fn.
pub(crate) fn mask(value: &str) -> String {
    "•".repeat(value.chars().count())
}

/// The ordered ui.yaml element list for the surface — the one source paint,
/// the automation registry, and the focus ring all read (ui.yaml `tui-unlock`).
pub fn elements(app: &App, mode: Mode) -> Vec<Element> {
    let mut out = vec![
        Element::input(
            ids::TUI_UNLOCK_PASSPHRASE_INPUT,
            mask(&app.unlock.passphrase),
            Field::Unlock(UnlockField::Passphrase),
        )
        .labelled(t::PASSPHRASE_LABEL),
    ];
    if mode == Mode::Create {
        out.push(
            Element::input(
                ids::TUI_UNLOCK_CONFIRM_INPUT,
                mask(&app.unlock.confirm),
                Field::Unlock(UnlockField::Confirm),
            )
            .labelled(t::CONFIRM_LABEL),
        );
    }
    out.push(Element::gesture_button(
        ids::TUI_UNLOCK_SUBMIT_BUTTON,
        match mode {
            Mode::Unlock => t::UNLOCK_BUTTON,
            Mode::Create => t::CREATE_BUTTON,
        },
        true,
        crate::element::Gesture::Unlock(UnlockAction::Submit),
    ));
    out
}

/// Pane title (chrome, not an automatable element).
pub fn title(mode: Mode) -> String {
    match mode {
        Mode::Unlock => t::UNLOCK_TITLE.to_string(),
        Mode::Create => t::CREATE_TITLE.to_string(),
    }
}

/// Help text painted above the elements (chrome).
pub fn description(mode: Mode) -> Vec<String> {
    match mode {
        Mode::Unlock => vec![t::UNLOCK_PROMPT.to_string()],
        Mode::Create => vec![t::CREATE_PROMPT.to_string()],
    }
}

/// Run the submit: validate, drive the sealed backend, and on success hand
/// the screen to launch routing (the deferred `launch::start`).
///
/// Synchronous by design (module docs); both the keyboard path and the
/// agent's click path call straight through here.
pub fn submit(app: &mut App, tx: &tokio::sync::mpsc::UnboundedSender<UiMessage>) {
    use crate::launch::LaunchSurface;

    let mode = match app.launch {
        LaunchSurface::Unlock => Mode::Unlock,
        LaunchSurface::CreatePassphrase => Mode::Create,
        // Unreachable: the button paints only on the two surfaces above.
        _ => return,
    };
    if app.unlock.passphrase.is_empty() {
        app.unlock.error = Some(t::ERROR_EMPTY.to_string());
        return;
    }
    let Some(sealed) = app.credentials.sealed_backend() else {
        // Unreachable: the surface is only entered when the store resolved to
        // the sealed backend (`App::route_locked_store`).
        tracing::error!("[unlock] submit with no sealed backend");
        return;
    };
    let outcome = match mode {
        Mode::Unlock => sealed.unlock(&app.unlock.passphrase),
        Mode::Create => {
            if app.unlock.passphrase != app.unlock.confirm {
                app.unlock.error = Some(t::ERROR_MISMATCH.to_string());
                return;
            }
            sealed.create(&app.unlock.passphrase)
        }
    };
    match outcome {
        Ok(()) => {
            app.unlock = UnlockState::default();
            app.focus = 0;
            // The store now serves reads — run the launch routing this surface
            // was deferring (`onboarding.md` § App-launch routing), including
            // the collision check (`account-scoping.md` § Concurrent
            // instances) now that the store-active account is finally
            // readable.
            crate::launch::start_or_offer_chooser(app, tx);
        }
        Err(fauna_credential_store::sealed::SealedStoreError::WrongPassphraseOrCorrupt) => {
            app.unlock.error = Some(t::ERROR_WRONG.to_string());
        }
        Err(e) => {
            app.unlock.error = Some(t::error_failed(&e.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(elements: &[Element]) -> Vec<String> {
        elements.iter().map(|e| e.id.clone()).collect()
    }

    /// ui.yaml `tui-unlock`: unlock mode paints the passphrase input + submit;
    /// create mode adds the confirm input between them.
    #[test]
    fn the_two_modes_paint_exactly_the_ui_yaml_elements() {
        let app = crate::app::tests::test_app();
        assert_eq!(
            ids(&elements(&app, Mode::Unlock)),
            vec!["tui-unlock-passphrase-input", "tui-unlock-submit-button"],
        );
        assert_eq!(
            ids(&elements(&app, Mode::Create)),
            vec![
                "tui-unlock-passphrase-input",
                "tui-unlock-confirm-input",
                "tui-unlock-submit-button",
            ],
        );
    }

    /// The registry (and so `get_text` / `/app/state`) must never see the
    /// passphrase — inputs answer a same-length mask.
    #[test]
    fn the_registry_text_is_masked() {
        let mut app = crate::app::tests::test_app();
        app.unlock.passphrase = "hunter2".into();
        app.unlock.confirm = "hu".into();
        let els = elements(&app, Mode::Create);
        assert_eq!(els[0].text, "•••••••");
        assert_eq!(els[1].text, "••");
        assert!(!els.iter().any(|e| e.text.contains("hunter2")));
    }
}
