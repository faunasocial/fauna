//! The admin Files sub-page (`admin-files`) — the deployment-wide WebDAV enable
//! toggle (`admin.md` § Files). No port: WebDAV rides the shared DAV listener the
//! Calendar page's port already governs. Enabling it deployment-wide exposes
//! nothing on its own — a user must still flag a folder for WebDAV.
//!
//! A dumb renderer of the shared `WebdavPolicyMachine`
//! (`fauna-client-mail-settings::webdav_policy`): the toggle flips
//! `fauna.bridges.set_webdav_enabled`, re-read from `get_mail_config`. The shell
//! (`super`) owns the machine, op and fold; this file is paint only.

use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::{Action, AdminState};
use crate::element::{Element, Gesture};
use crate::pages::Page;

pub(super) fn files_elements(state: &AdminState) -> Vec<Element> {
    let enabled = state
        .webdav_snapshot
        .as_ref()
        .map(|s| s.webdav_enabled)
        .unwrap_or(false);
    vec![
        Element::label(ids::ADMIN_FILES_HEADING, t::files_page::TITLE),
        Element::chrome(t::files_page::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
        Element::checkbox_gesture(
            ids::ADMIN_FILES_WEBDAV_ENABLED_TOGGLE,
            t::files_page::ENABLED_LABEL,
            enabled,
            Gesture::Admin(Action::ToggleWebdavEnabled),
        )
        .attr("state", if enabled { "on" } else { "off" }),
        Element::chrome(t::files_page::ENABLED_SUBTITLE),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_mail_settings::webdav_policy::{WebdavPolicySnapshot, WebdavPolicyStatus};

    #[test]
    fn files_page_paints_toggle_and_back() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = super::super::AdminPage::Files;
        app.admin.webdav_snapshot = Some(WebdavPolicySnapshot {
            webdav_enabled: true,
            status: WebdavPolicyStatus::Idle,
            error: None,
        });

        let els = files_elements(&app.admin);
        let tagged: Vec<&str> = els
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        assert_eq!(
            tagged,
            vec![
                "admin-files-heading",
                "admin-nav-back",
                "admin-files-webdav-enabled-toggle",
            ]
        );
        let toggle = els
            .iter()
            .find(|e| e.id == "admin-files-webdav-enabled-toggle")
            .expect("toggle painted");
        assert_eq!(
            toggle
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("on"),
            "the toggle state reflects webdav_enabled = true"
        );
    }
}
