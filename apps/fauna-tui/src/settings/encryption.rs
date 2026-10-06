//! The Settings → Encryption sub-page (`ui/settings.md` § Navigation model;
//! nav id `encryption`) — the MLS key-package pool: the available count, a
//! low-key warning, and a manual Refresh. Mirrors linux's
//! `settings/encryption.rs`; the i18n copy set (`encryption_page`) already
//! existed for it, unrendered on tui until now.
//!
//! **No ui.yaml element ids exist for this content on ANY of the 4 apps that
//! render it** (linux/web/windows/macos all built ahead of any e2e id
//! allocation for this page). tui follows the same shape rather than
//! inventing a new id family unilaterally (§ UI Consistency rule A): every
//! element here besides the shared `page-heading`/`settings-nav-back` is
//! `Element::chrome`/`chrome_button` — real, human-usable content and (for
//! the Refresh action) a real focus-ring/keyboard target, just not
//! ui.yaml-registered or e2e-drivable, exactly linux's own gap.
//!
//! **The count read and the refresh mint are deliberately two different
//! client seams.** The count (`Op::HydrateEncryption`) is a pure
//! `fauna.conversations.keypackage.count` RPC over a one-off
//! `ConversationsClient` — no MLS engine involved. The refresh
//! (`Op::RefreshKeyPackages`) must instead ride the durable
//! `ConversationsManager::ensure_keypackages`, which mints on the session's
//! `MlsEngine` and notifies the replica autosave observer: a raw RPC upload
//! of freshly-minted packages would skip that notify, and a later state
//! restore (a provider swap, or a relaunch) would wipe the fresh private
//! init keys, stranding every peer that already fetched the package
//! (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync —
//! linux's `conv_backend.rs::replenish_key_packages` doc comment states the
//! same contract; this page's refresh is the same call, tui's idiom).

use fauna_i18n::strings::common;
use fauna_i18n::strings::settings::encryption_page as enc;
use fauna_ui_ids as ids;

use super::{Action, SettingsState};
use crate::element::{Element, Gesture};

/// Below this available count, the low-key warning paints — linux's
/// `LOW_KEY_THRESHOLD`.
const LOW_KEY_THRESHOLD: u64 = 10;

pub(super) fn encryption_elements(state: &SettingsState) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, enc::TITLE),
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    ];

    // The warning reads the SAME count the row below states — never a
    // separately-fetched value — so the two can't disagree. Painted only
    // once hydrated AND low: an un-hydrated page has no basis to claim
    // either "fine" or "low" (the `quota`/`features` un-hydrated-paint
    // discipline).
    if let Some(count) = state.encryption_key_packages
        && count < LOW_KEY_THRESHOLD
    {
        els.push(Element::chrome(enc::LOW_KEY_WARNING_TITLE));
        els.push(Element::chrome(enc::low_key_warning_body(
            &count.to_string(),
        )));
    }

    els.push(Element::chrome(enc::MLS_KEY_PACKAGES));
    els.push(Element::chrome(enc::MLS_DESCRIPTION));

    let count_text = match state.encryption_key_packages {
        Some(count) => count.to_string(),
        None => common::LOADING.to_string(),
    };
    els.push(Element::chrome(format!(
        "{}: {count_text}",
        enc::AVAILABLE_KEY_PACKAGES
    )));

    els.push(Element::chrome(enc::REFRESH_KEYS_DESCRIPTION));
    els.push(Element::chrome_button(
        enc::REFRESH_KEYS,
        Gesture::Settings(Action::RefreshKeyPackages),
    ));

    els
}

/// This sub-page's error — read by `App::page_snapshot_error`, the `web`
/// shape (see that module's `page_error` doc comment for why this must NOT
/// be a page-pushed `error-message` element).
pub(super) fn page_error(state: &SettingsState) -> Option<String> {
    state
        .encryption_error
        .as_deref()
        .filter(|e| !e.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;

    fn state_with(count: Option<u64>) -> crate::app::App {
        let mut app = crate::app::tests::authed_app();
        app.page = crate::pages::Page::Settings;
        app.settings.sub = SubPage::Encryption;
        app.settings.encryption_key_packages = count;
        app
    }

    #[test]
    fn paints_a_real_heading_and_nav_back() {
        let app = state_with(Some(20));
        let els = encryption_elements(&app.settings);
        assert_eq!(els[0].id, "page-heading");
        assert_eq!(els[0].text, enc::TITLE);
        assert!(els.iter().any(|e| e.id == "settings-nav-back"));
    }

    #[test]
    fn pre_hydrate_shows_loading_not_a_stale_or_zero_count() {
        let app = state_with(None);
        let painted = crate::ui::painted_line_texts(&encryption_elements(&app.settings));
        assert!(
            painted
                .iter()
                .any(|l| l.contains(enc::AVAILABLE_KEY_PACKAGES) && l.contains("Loading")),
            "painted: {painted:?}"
        );
        assert!(
            !painted
                .iter()
                .any(|l| l.contains(enc::LOW_KEY_WARNING_TITLE)),
            "an un-hydrated page has no basis to claim the count is low"
        );
    }

    #[test]
    fn a_healthy_count_paints_no_warning() {
        let app = state_with(Some(20));
        let painted = crate::ui::painted_line_texts(&encryption_elements(&app.settings));
        assert!(painted.iter().any(|l| l.contains("20")));
        assert!(
            !painted
                .iter()
                .any(|l| l.contains(enc::LOW_KEY_WARNING_TITLE))
        );
    }

    #[test]
    fn a_count_below_ten_paints_the_low_key_warning_naming_the_exact_count() {
        let app = state_with(Some(3));
        let painted = crate::ui::painted_line_texts(&encryption_elements(&app.settings));
        assert!(
            painted
                .iter()
                .any(|l| l.contains(enc::LOW_KEY_WARNING_TITLE))
        );
        assert!(
            painted.iter().any(|l| l.contains('3')),
            "the warning body must name the exact remaining count; painted: {painted:?}"
        );
    }

    #[test]
    fn exactly_at_threshold_paints_no_warning() {
        // LOW_KEY_THRESHOLD is 10 — the boundary is "< 10", not "<= 10", so 10
        // itself is healthy. A mutation flipping `<` to `<=` must fail this.
        let app = state_with(Some(LOW_KEY_THRESHOLD));
        let painted = crate::ui::painted_line_texts(&encryption_elements(&app.settings));
        assert!(
            !painted
                .iter()
                .any(|l| l.contains(enc::LOW_KEY_WARNING_TITLE))
        );
    }

    #[test]
    fn refresh_is_a_real_focusable_control_with_no_ui_yaml_id() {
        let app = state_with(Some(20));
        let els = encryption_elements(&app.settings);
        let refresh = els
            .iter()
            .find(|e| e.text == enc::REFRESH_KEYS)
            .expect("Refresh Keys control painted");
        assert_eq!(
            refresh.id, "",
            "no ui.yaml id exists for this content on ANY app — chrome_button, not a minted id"
        );
        assert!(refresh.enabled);
    }

    #[test]
    fn a_hydrate_failure_surfaces_on_page_error_not_a_painted_element() {
        let mut app = state_with(None);
        app.settings.encryption_error = Some("keypackage count: refused".to_string());
        assert_eq!(
            page_error(&app.settings).as_deref(),
            Some("keypackage count: refused")
        );
        assert!(
            !encryption_elements(&app.settings)
                .iter()
                .any(|e| e.id == "error-message"),
            "must not be a second, page-pushed copy of the id"
        );
    }

    #[test]
    fn a_successful_read_clears_a_prior_error() {
        let mut app = state_with(None);
        app.settings.encryption_error = Some("stale".to_string());
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::EncryptionKeyPackageCount(Ok(20)),
        );
        assert_eq!(app.settings.encryption_key_packages, Some(20));
        assert_eq!(page_error(&app.settings), None);
    }

    #[test]
    fn a_failed_read_surfaces_the_message_and_leaves_the_prior_count() {
        let mut app = state_with(Some(20));
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::EncryptionKeyPackageCount(Err("refused".to_string())),
        );
        assert_eq!(
            app.settings.encryption_key_packages,
            Some(20),
            "a failed re-read must not clobber the last known-good count"
        );
        assert_eq!(page_error(&app.settings).as_deref(), Some("refused"));
    }
}
