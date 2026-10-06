//! The admin Calendar sub-page (`admin-calendar`) — the deployment-wide CalDAV
//! enable toggle plus the admin-set CalDAV port (`admin.md` § 8 Calendar).
//!
//! A dumb renderer of the shared `CaldavPolicyMachine`
//! (`fauna-client-mail-settings::caldav_policy`): the toggle flips
//! `fauna.bridges.set_caldav_enabled`, the port input + save button write
//! `fauna.bridges.set_caldav_port`, both re-read from `get_mail_config` — the
//! shell (`super`) owns the machine, ops and folds; this file is paint only.

use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::{Action, AdminField, AdminState};
use crate::element::{Element, Field, Gesture};
use crate::pages::Page;

pub(super) fn calendar_elements(state: &AdminState) -> Vec<Element> {
    let snap = state.caldav_snapshot.as_ref();
    let enabled = snap.map(|s| s.caldav_enabled).unwrap_or(false);
    vec![
        Element::label(ids::ADMIN_CALENDAR_HEADING, t::calendar_page::TITLE),
        Element::chrome(t::calendar_page::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
        // The deployment-wide CalDAV-enable toggle. `state` carries the toggle's
        // OWN on/off answer (what the e2e reads via `get_attr(id, "state")`) — the
        // uniform cross-app toggle idiom, non-optimistic (the shell awaits the
        // nest write before the snapshot flips it).
        Element::checkbox_gesture(
            ids::ADMIN_CALENDAR_ENABLED_TOGGLE,
            t::calendar_page::ENABLED_LABEL,
            enabled,
            Gesture::Admin(Action::ToggleCaldavEnabled),
        )
        .attr("state", if enabled { "on" } else { "off" }),
        Element::chrome(t::calendar_page::ENABLED_SUBTITLE),
        // The admin-set CalDAV listener port (router-less direct listener only —
        // inert on a domain nest). The input paints the draft (re-seeded from the
        // persisted `caldav_port` on every snapshot); `[1, 65535]` is validated
        // client-side at save (`super::apply_local`), the invalid message on
        // `error-message`.
        Element::input(
            ids::ADMIN_CALENDAR_CALDAV_PORT_INPUT,
            state.caldav_port_input.clone(),
            Field::Admin(AdminField::CaldavPort),
        )
        .labelled(t::calendar_page::CALDAV_PORT_LABEL),
        Element::chrome(t::calendar_page::CALDAV_PORT_DESC),
        Element::gesture_button(
            ids::ADMIN_CALENDAR_CALDAV_PORT_SAVE_BUTTON,
            t::calendar_page::CALDAV_PORT_SAVE,
            true,
            Gesture::Admin(Action::SaveCaldavPort),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_mail_settings::caldav_policy::{CaldavPolicySnapshot, CaldavPolicyStatus};

    /// The page paints its ui.yaml element set in order, and the toggle's `state`
    /// attr reflects the snapshot's `caldav_enabled` (what the e2e reads).
    #[test]
    fn calendar_page_paints_toggle_port_and_back() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = super::super::AdminPage::Calendar;
        app.admin.caldav_snapshot = Some(CaldavPolicySnapshot {
            caldav_enabled: true,
            caldav_port: 9443,
            status: CaldavPolicyStatus::Idle,
            error: None,
        });
        app.admin.caldav_port_input = "9443".to_string();

        let els = calendar_elements(&app.admin);
        let tagged: Vec<&str> = els
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        assert_eq!(
            tagged,
            vec![
                "admin-calendar-heading",
                "admin-nav-back",
                "admin-calendar-enabled-toggle",
                "admin-calendar-caldav-port-input",
                "admin-calendar-caldav-port-save-button",
            ]
        );

        let toggle = els
            .iter()
            .find(|e| e.id == "admin-calendar-enabled-toggle")
            .expect("toggle painted");
        assert_eq!(
            toggle
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("on"),
            "the toggle state reflects caldav_enabled = true"
        );

        let port = els
            .iter()
            .find(|e| e.id == "admin-calendar-caldav-port-input")
            .expect("port input painted");
        assert_eq!(port.text, "9443", "the port input paints the draft value");
    }
}
