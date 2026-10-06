//! The `tui-settings` external-media sub-page (split out of `settings/mod.rs`).

use fauna_i18n::strings::{common, tui_settings};
use fauna_ui_ids as ids;

use super::{Action, ExternalMediaMode, SettingsState};
use crate::element::{Element, Gesture, SelectTarget};

/// The `tui-settings` page — ui.yaml `page-heading`, `tui-settings-external-media`
/// (the select), `settings-nav-back` (optional; the "leave" affordance). The
/// page's `error-message` is registered globally by [`crate::ui::register_frame`]
/// when the page carries an error, so it is not painted here.
pub(super) fn tui_settings_elements(state: &SettingsState) -> Vec<Element> {
    let mode = &state.prefs.external_media;
    vec![
        Element::label(ids::PAGE_HEADING, tui_settings::TITLE),
        // Untagged explainer chrome for the select below — ui.yaml scopes no
        // label element on this page, only the select and the heading.
        Element::chrome(tui_settings::EXTERNAL_MEDIA_LABEL),
        Element::chrome(tui_settings::EXTERNAL_MEDIA_SUBTITLE),
        // `text` is the raw token (`ask`/`always`/`never`) that round-trips
        // through `/element/select`; the human reads the localized label. Options
        // are the three tokens, so the keyboard cycle steps ask→always→never.
        Element::select(
            ids::TUI_SETTINGS_EXTERNAL_MEDIA,
            mode.token(),
            SelectTarget::ExternalMedia,
            ExternalMediaMode::ALL
                .iter()
                .map(|m| m.token().to_string())
                .collect(),
        )
        .display_value(mode.label()),
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    ]
}

// ─────────────────────────── External-media handoff seam ───────────────────────────

/// What the current mode does with an external-media open request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffOutcome {
    /// `Always` — the OS handler was (or should be) launched.
    Launched,
    /// `Never` — nothing launches; metadata stays on screen.
    Suppressed,
    /// `Ask` — the caller shows an inline confirm before launching. (The media-
    /// open UI that renders this prompt lands with M6 — `tui.md` § Rendering.)
    Prompt,
}

/// Decide what to do with an external-media open under `mode`. Pure (no I/O), so
/// the gate is unit-tested directly; only `Launched` implies an actual spawn (via
/// [`crate::os_open::open`] — the gate is what is media-specific, the spawn is
/// the shared OS-handler resolution every open-external call site uses).
///
/// A mode a newer tui wrote that this build cannot name launches nothing — the
/// most restrictive reading (`transport.md` § Rule 3 in full).
pub fn external_media_outcome(mode: &ExternalMediaMode) -> HandoffOutcome {
    match mode {
        ExternalMediaMode::Never | ExternalMediaMode::Other(_) => HandoffOutcome::Suppressed,
        ExternalMediaMode::Ask => HandoffOutcome::Prompt,
        ExternalMediaMode::Always => HandoffOutcome::Launched,
    }
}
