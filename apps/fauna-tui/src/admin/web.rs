//! The admin Web sub-page (`admin-web`) — the deployment apex-actor designation
//! (`admin.md` § 7 Web; feature authority `web-content-hosting.md` § Admin apex
//! hosting).
//!
//! One control: the **apex-actor picker** (`admin-web-apex-actor-select`), an
//! Admin-class designation of which actor's `web` content serves at
//! `https://<domain>/`, with "none" clearing it to the built-in info page. The
//! option list comes from `fauna.admin.users.list`; the current designation and
//! the set/clear ride the shared `fauna-client-web` `WebClient` (priority #2 —
//! the same crate linux/web/windows/apple/android render off). Per-user
//! subdomain hosting is the *user* `web-settings` page (`crate::settings::web`),
//! not here.
//!
//! The shell (`super`) owns the client, op and fold; this file is paint only.

use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::{AdminState, apex_option_labels};
use crate::element::{Element, Gesture, SelectTarget};
use crate::pages::Page;

pub(super) fn web_elements(state: &AdminState) -> Vec<Element> {
    let snap = state.web_snapshot.as_ref();
    // The picker's option list and the currently-selected label are derived by the
    // same helper the select gesture resolves against, so a label the human can
    // pick always maps back to an actor id (no positional guess).
    let (options, selected) = match snap {
        Some(s) => apex_option_labels(s),
        None => (vec![t::web_page::APEX_NONE.to_string()], String::new()),
    };

    let els = vec![
        Element::label(ids::ADMIN_WEB_HEADING, t::web_page::TITLE),
        Element::chrome(t::web_page::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
        Element::select(
            ids::ADMIN_WEB_APEX_ACTOR_SELECT,
            selected,
            SelectTarget::ApexActor,
            options,
        )
        .labelled(t::web_page::APEX_SELECT_LABEL),
        Element::chrome(t::web_page::APEX_SELECT_SUBTITLE),
        Element::label(
            ids::ADMIN_WEB_APEX_INFO,
            t::web_page::apex_info(&state.apex_url()),
        ),
    ];

    els
}

/// This page's read/write failure — read by `App::screen_error_text`, not
/// painted here (`crate::admin::page_error` carries the why).
pub(super) fn page_error(state: &AdminState) -> Option<String> {
    state
        .web_snapshot
        .as_ref()
        .and_then(|s| s.error.as_deref())
        .filter(|e| !e.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{AdminPage, AdminWebSnapshot};

    fn snapshot(current: Option<Vec<u8>>) -> AdminWebSnapshot {
        AdminWebSnapshot {
            current,
            actors: vec![
                (vec![1u8; 32], "alice".to_string()),
                (vec![2u8; 32], "bob".to_string()),
            ],
            error: None,
        }
    }

    #[test]
    fn web_page_paints_picker_info_and_back() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Web;
        app.admin.web_snapshot = Some(snapshot(None));

        let els = web_elements(&app.admin);
        let tagged: Vec<&str> = els
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        assert_eq!(
            tagged,
            vec![
                "admin-web-heading",
                "admin-nav-back",
                "admin-web-apex-actor-select",
                "admin-web-apex-info",
            ],
            "a clean page paints no error-message"
        );
    }

    /// The picker offers "None" first and then one option per actor, and an
    /// undesignated apex selects "None" — the clear-state contract the e2e's
    /// `set_apex_actor(S.admin.web_page.apex_none)` leg drives.
    #[test]
    fn picker_offers_none_plus_every_actor() {
        let mut app = crate::app::tests::test_app();
        app.admin.web_snapshot = Some(snapshot(None));
        let els = web_elements(&app.admin);
        let picker = els
            .iter()
            .find(|e| e.id == "admin-web-apex-actor-select")
            .expect("picker painted");
        match &picker.role {
            crate::element::Role::Select { options, .. } => assert_eq!(
                options,
                &vec![
                    t::web_page::APEX_NONE.to_string(),
                    "alice".to_string(),
                    "bob".to_string(),
                ]
            ),
            other => panic!("expected a Select role, got {other:?}"),
        }
        assert_eq!(
            picker.text,
            t::web_page::APEX_NONE,
            "an undesignated apex reads back as the None option"
        );
    }

    /// A designation selects that actor's label — the assertion the e2e makes
    /// after `set_apex_actor(label)` (via `apex_actor_selected()`).
    #[test]
    fn designated_apex_selects_that_actors_label() {
        let mut app = crate::app::tests::test_app();
        app.admin.web_snapshot = Some(snapshot(Some(vec![2u8; 32])));
        let els = web_elements(&app.admin);
        let picker = els
            .iter()
            .find(|e| e.id == "admin-web-apex-actor-select")
            .expect("picker painted");
        assert_eq!(picker.text, "bob");
    }

    /// A designation the actor list doesn't carry still shows + stays selected
    /// (linux's `actor_id_fallback_label` trailing entry) rather than silently
    /// reading as cleared — which would look identical to a real clear. The
    /// fallback carries the FULL actor hex (widened from a
    /// 4-byte-truncated prefix to match apple/linux's `hex_full`).
    #[test]
    fn unknown_designation_gets_a_visible_fallback_option() {
        let mut app = crate::app::tests::test_app();
        app.admin.web_snapshot = Some(snapshot(Some(vec![0xabu8; 32])));
        let els = web_elements(&app.admin);
        let picker = els
            .iter()
            .find(|e| e.id == "admin-web-apex-actor-select")
            .expect("picker painted");
        let expected = t::actor_id_fallback_label(&"ab".repeat(32));
        assert_eq!(picker.text, expected);
        match &picker.role {
            crate::element::Role::Select { options, .. } => {
                assert!(options.contains(&expected), "fallback option offered")
            }
            other => panic!("expected a Select role, got {other:?}"),
        }
    }

    /// A read/write failure must NOT be a page-pushed `error-message` element —
    /// see the sibling test for where it goes instead.
    #[test]
    fn the_page_pushes_no_error_message_element_of_its_own() {
        let mut app = crate::app::tests::test_app();
        app.admin.web_snapshot = Some(AdminWebSnapshot {
            current: None,
            actors: Vec::new(),
            error: Some("apex read failed".to_string()),
        });
        assert!(
            !web_elements(&app.admin)
                .iter()
                .any(|e| e.id == "error-message"),
            "the id is registered by the ONE global funnel, never by the page"
        );
    }

    /// …and the SAME failure must reach `error_line_text()`, which is what the
    /// state protocol answers `messages.error` with.
    ///
    /// A page-painted `error-message` element alone is **not** readable by the
    /// cross-app `error_text()` (`tests/e2e-unified/actions/__init__.py`): it
    /// reads `messages.error` first and only falls back to the element when the
    /// key is ABSENT — and tui always emits the key (`automation.rs`'s state
    /// payload), so a null there resolves to `""` and the fallback is
    /// unreachable. Bridging onto `App::errors` is what `tui.md` § The
    /// page-module contract already requires; this pins the half that was
    /// missing.
    #[test]
    fn snapshot_error_also_reaches_the_state_protocols_error_line() {
        let mut app = crate::app::tests::authed_app();
        app.page = Page::Admin;
        app.admin.sub = AdminPage::Web;
        crate::admin::apply_outcome(
            &mut app,
            crate::admin::Outcome::WebLoaded(AdminWebSnapshot {
                current: None,
                actors: Vec::new(),
                error: Some("apex read failed".to_string()),
            }),
        );
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("apex read failed"),
            "the snapshot error must reach `messages.error`, not only the painted element"
        );
    }
}
