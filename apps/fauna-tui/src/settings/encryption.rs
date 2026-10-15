//! The Settings → Encryption sub-page (`ui/settings.md` § Navigation model;
//! nav id `encryption`) — the MLS key-package pool: the available count, a
//! low-key warning, and a manual Refresh. Mirrors linux's
//! `settings/encryption.rs`; the i18n copy set (`encryption_page`) already
//! existed for it, unrendered on tui until now.
//!
//! **The three content ids** — `encryption-keypackage-count`,
//! `encryption-keypackage-low-warning` (present only while low) and
//! `encryption-keypackage-refresh-button` — were user-approved 2026-09-25
//! under rule A; tui is the first app to carry them. The low test is the
//! shared [`fauna_conversations::keypackages_low`] (fewer than five), never a
//! per-app literal (`direct-messages.md` § Key Package Management).
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
        && fauna_conversations::keypackages_low(count)
    {
        els.push(
            Element::label(
                ids::ENCRYPTION_KEYPACKAGE_LOW_WARNING,
                enc::low_key_warning_body(&count.to_string()),
            )
            .labelled(enc::LOW_KEY_WARNING_TITLE),
        );
    }

    els.push(Element::chrome(enc::MLS_KEY_PACKAGES));
    els.push(Element::chrome(enc::MLS_DESCRIPTION));

    let count_text = match state.encryption_key_packages {
        Some(count) => count.to_string(),
        None => common::LOADING.to_string(),
    };
    els.push(
        Element::label(ids::ENCRYPTION_KEYPACKAGE_COUNT, count_text)
            .labelled(enc::AVAILABLE_KEY_PACKAGES),
    );

    els.push(Element::chrome(enc::REFRESH_KEYS_DESCRIPTION));
    els.push(Element::gesture_button(
        ids::ENCRYPTION_KEYPACKAGE_REFRESH_BUTTON,
        enc::REFRESH_KEYS,
        true,
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
    fn a_count_below_five_paints_the_low_key_warning_naming_the_exact_count() {
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
    fn a_count_between_five_and_ten_paints_no_warning() {
        // The pre-unification tui threshold was ten; seven is healthy now.
        let app = state_with(Some(7));
        let painted = crate::ui::painted_line_texts(&encryption_elements(&app.settings));
        assert!(
            !painted
                .iter()
                .any(|l| l.contains(enc::LOW_KEY_WARNING_TITLE))
        );
    }

    #[test]
    fn exactly_at_threshold_paints_no_warning() {
        // The shared threshold is five (`direct-messages.md` § Key Package
        // Management: "fewer than five") — five itself is healthy, so a
        // mutation flipping `<` to `<=` must fail this.
        let app = state_with(Some(fauna_conversations::KEYPACKAGE_LOW_THRESHOLD));
        let painted = crate::ui::painted_line_texts(&encryption_elements(&app.settings));
        assert!(
            !painted
                .iter()
                .any(|l| l.contains(enc::LOW_KEY_WARNING_TITLE))
        );
    }

    #[test]
    fn the_approved_ids_carry_the_count_the_warning_and_refresh() {
        let app = state_with(Some(3));
        let els = encryption_elements(&app.settings);
        let by_id = |id: &str| els.iter().find(|e| e.id == id);
        assert_eq!(
            by_id(ids::ENCRYPTION_KEYPACKAGE_COUNT).map(|e| e.text.as_str()),
            Some("3"),
            "the count id's text is the bare count every driver reads"
        );
        assert!(by_id(ids::ENCRYPTION_KEYPACKAGE_LOW_WARNING).is_some());
        let refresh = by_id(ids::ENCRYPTION_KEYPACKAGE_REFRESH_BUTTON).expect("refresh");
        assert_eq!(refresh.text, enc::REFRESH_KEYS);
        assert!(refresh.enabled);
        let healthy = state_with(Some(5));
        assert!(
            !encryption_elements(&healthy.settings)
                .iter()
                .any(|e| e.id == ids::ENCRYPTION_KEYPACKAGE_LOW_WARNING),
            "the warning id is absent while the count is not low"
        );
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
