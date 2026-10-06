//! The Settings → General sub-page (`ui/settings.md` § Navigation model; nav
//! id `general`) — tui's scoped-down terminal-native equivalent of the other
//! apps' appearance/startup/updates page.
//!
//! Every other app's General page bundles a theme picker, OS-level
//! autostart, tray behavior, a notification-sound toggle, and an in-app
//! updater — none of which apply here: tui inherits the terminal emulator's
//! own color scheme (there is nothing to pick), has no window to autostart
//! or a tray to minimize into, and ships through whatever channel installed
//! the binary, not an in-app updater. Forcing any of those as a no-op
//! control would be worse than omitting it. What's genuinely tui-applicable:
//! a short note explaining why there's no theme picker, and the app version.
//!
//! Purely static — no state, no hydrate (`route_subpage` returns `None` for
//! `SubPage::General`, the `P2p` shape but with no `on_open` side effect
//! either since there's nothing to reset).

use fauna_i18n::strings::settings;
use fauna_i18n::strings::settings::general_page as gp;
use fauna_ui_ids as ids;

use super::{Action, SettingsState};
use crate::element::{Element, Gesture};

pub(super) fn general_elements(_state: &SettingsState) -> Vec<Element> {
    vec![
        Element::label(ids::PAGE_HEADING, settings::GENERAL),
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
        Element::chrome(gp::APPEARANCE_NOTE),
        Element::chrome(format!("{}: {}", gp::VERSION, env!("CARGO_PKG_VERSION"))),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> crate::app::App {
        let mut app = crate::app::tests::authed_app();
        app.page = crate::pages::Page::Settings;
        app.settings.sub = crate::settings::SubPage::General;
        app
    }

    #[test]
    fn paints_a_real_heading_and_nav_back() {
        let app = state();
        let els = general_elements(&app.settings);
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids[0], "page-heading");
        assert_eq!(
            els[0].text,
            fauna_i18n::strings::settings::GENERAL,
            "the heading must be the real localized title, not a placeholder"
        );
        assert!(ids.contains(&"settings-nav-back"));
    }

    #[test]
    fn explains_the_terminal_owns_the_color_scheme_rather_than_painting_a_picker() {
        let app = state();
        let painted = crate::ui::painted_line_texts(&general_elements(&app.settings));
        assert!(
            painted
                .iter()
                .any(|l| l.contains("terminal") && l.contains("color scheme")),
            "General must explain why there is no theme picker, not silently omit \
             appearance content; painted: {painted:?}"
        );
        assert!(
            !painted.iter().any(|l| l.to_lowercase().contains("theme:")),
            "and must NOT paint a fake theme picker — there is nothing to pick"
        );
    }

    #[test]
    fn shows_the_running_binarys_own_version() {
        let app = state();
        let painted = crate::ui::painted_line_texts(&general_elements(&app.settings));
        assert!(
            painted
                .iter()
                .any(|l| l.contains(env!("CARGO_PKG_VERSION"))),
            "painted: {painted:?}"
        );
    }
}
