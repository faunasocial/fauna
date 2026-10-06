//! The admin Contacts sub-page (`admin-contacts`) — the deployment-wide CardDAV
//! enable toggle (`admin.md` § Contacts). No port: CardDAV rides the shared DAV
//! listener the Calendar page's port already governs.
//!
//! A dumb renderer of the shared `CarddavPolicyMachine`
//! (`fauna-client-mail-settings::carddav_policy`): the toggle flips
//! `fauna.bridges.set_carddav_enabled`, re-read from `get_mail_config`. The shell
//! (`super`) owns the machine, op and fold; this file is paint only.

use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::{Action, AdminState};
use crate::element::{Element, Gesture};
use crate::pages::Page;

pub(super) fn contacts_elements(state: &AdminState) -> Vec<Element> {
    let enabled = state
        .carddav_snapshot
        .as_ref()
        .map(|s| s.carddav_enabled)
        .unwrap_or(false);
    vec![
        Element::label(ids::ADMIN_CONTACTS_HEADING, t::contacts_page::TITLE),
        Element::chrome(t::contacts_page::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
        Element::checkbox_gesture(
            ids::ADMIN_CONTACTS_CARDDAV_ENABLED_TOGGLE,
            t::contacts_page::ENABLED_LABEL,
            enabled,
            Gesture::Admin(Action::ToggleCarddavEnabled),
        )
        .attr("state", if enabled { "on" } else { "off" }),
        Element::chrome(t::contacts_page::ENABLED_SUBTITLE),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_mail_settings::carddav_policy::{CarddavPolicySnapshot, CarddavPolicyStatus};

    #[test]
    fn contacts_page_paints_toggle_and_back() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = super::super::AdminPage::Contacts;
        app.admin.carddav_snapshot = Some(CarddavPolicySnapshot {
            carddav_enabled: false,
            status: CarddavPolicyStatus::Idle,
            error: None,
        });

        let els = contacts_elements(&app.admin);
        let tagged: Vec<&str> = els
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        assert_eq!(
            tagged,
            vec![
                "admin-contacts-heading",
                "admin-nav-back",
                "admin-contacts-carddav-enabled-toggle",
            ]
        );
        let toggle = els
            .iter()
            .find(|e| e.id == "admin-contacts-carddav-enabled-toggle")
            .expect("toggle painted");
        assert_eq!(
            toggle
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("off"),
            "the toggle state reflects carddav_enabled = false"
        );
    }
}
