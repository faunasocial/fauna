//! The admin Dashboard sub-page (`admin-dashboard`) — the shell's landing surface
//! (`admin.md` § 1 Dashboard). Split out of the shell (`super`) when the sub-nav
//! landed, mirroring `settings/`'s per-sub-page files.

use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::AdminState;
use crate::element::{Element, Gesture};
use crate::pages::Page;

/// The Dashboard's read model: the shared card set
/// (`fauna_client_admin::AdminClient::dashboard_snapshot` — the five reads and
/// the one fold, `admin.md` § 1. Dashboard → *The card set*). tui renders its
/// cards and chooses none of its own.
pub use fauna_client_admin::AdminDashboardSnapshot as DashboardSnapshot;

/// The `admin-dashboard` page: heading + `admin-nav-back` + the stat cards
/// (once loaded). The shell's [`super::elements`] prepends the sub-page nav rail,
/// so this returns only the page's own elements.
pub(super) fn dashboard_elements(state: &AdminState) -> Vec<Element> {
    let mut out = vec![
        Element::label(ids::ADMIN_DASHBOARD_HEADING, t::dashboard::TITLE),
        // The uniform "leave admin" affordance — present on EVERY admin page
        // (admin.md § Navigation model), the single testable shell exit. Leaving
        // the shell returns to the **primary view**, which every desktop app
        // lands on Conversations (admin.md § Navigation model — the same landing
        // as `settings-nav-back`, corrected 2026-06-07; the shared
        // `test_admin_nav_back_lands_on_primary_view` asserts it). tui matches
        // that landing rather than inventing its own, and reuses the one nav door
        // (`Gesture::Nav`), so no admin-specific nav-back gesture exists.
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
    ];
    if let Some(snapshot) = &state.dash {
        for card in &snapshot.stats {
            // FLAT-indexed leaf ids, read by `get_text(id, index=i)` in
            // `tests/e2e-unified/actions/admin.py` — the notifications/
            // conversations-bubble flat-sibling convention (NOT a `.within`
            // scope). Unlike `notification-row` (realized via its leaves with
            // no wrapper id at all), `admin-stat-card`'s own `elements:` list
            // in ui.yaml requires the wrapper id itself — so it gets a bare
            // container marker (empty text, the `mail-rotate-keys-exclude-list`
            // idiom) immediately before each card's leaves, indexed the same
            // way every other app tags a real card container.
            out.push(Element::label(ids::ADMIN_STAT_CARD, String::new()));
            out.push(Element::label(
                ids::ADMIN_STAT_CARD_LABEL,
                card.label.resolve(fauna_i18n::strings::lookup),
            ));
            out.push(Element::label(
                ids::ADMIN_STAT_CARD_VALUE,
                card.value.resolve(fauna_i18n::strings::lookup),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::Page;
    use fauna_client_admin::{AdminDashboardReads, admin_dashboard_cards};
    use fauna_protocol::admin::{AdminStatsReply, AdminStatusReply};

    fn snapshot() -> DashboardSnapshot {
        DashboardSnapshot {
            stats: admin_dashboard_cards(&AdminDashboardReads {
                stats: Some(AdminStatsReply {
                    total_users: 3,
                    users_by_tier: vec![("free".into(), 3)],
                    suspended_users: 0,
                    total_inbox_bytes: 4096,
                    total_storage_bytes: 2048,
                    ws_connections: 5,
                    extra: Default::default(),
                }),
                status: Some(AdminStatusReply {
                    version: "1.2.3".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            error: None,
        }
    }

    /// The empty (pre-load) dashboard paints exactly the heading + the exit button
    /// — no stat cards, so the e2e's `wait_for` waits for real data. (The shell's
    /// nav rail is prepended by `super::elements`, not here.)
    #[test]
    fn empty_dashboard_paints_heading_and_exit_only() {
        let app = crate::app::tests::test_app();
        let ids: Vec<String> = dashboard_elements(&app.admin)
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(ids, vec!["admin-dashboard-heading", "admin-nav-back"]);
    }

    /// Once the snapshot loads, the page paints the shared fold's cards, in its
    /// order, as flat `-label`/`-value` leaf pairs resolved against tui's table.
    /// The labels include "Users" and "Version" — the two the shared
    /// `navigate_dashboard` e2e helper gates on.
    #[test]
    fn loaded_dashboard_paints_the_shared_cards() {
        let mut app = crate::app::tests::test_app();
        app.admin.dash = Some(snapshot());
        let els = dashboard_elements(&app.admin);
        let n = app.admin.dash.as_ref().unwrap().stats.len();
        let ids: Vec<String> = els.iter().map(|e| e.id.clone()).collect();
        let mut expected = vec!["admin-dashboard-heading", "admin-nav-back"];
        for _ in 0..n {
            expected.push("admin-stat-card");
            expected.push("admin-stat-card-label");
            expected.push("admin-stat-card-value");
        }
        assert_eq!(ids, expected);

        let pairs: Vec<(&str, &str)> = els
            .iter()
            .filter(|e| e.id == "admin-stat-card-label")
            .map(|e| e.text.as_str())
            .zip(
                els.iter()
                    .filter(|e| e.id == "admin-stat-card-value")
                    .map(|e| e.text.as_str()),
            )
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("Registered Users", "3"),
                ("Suspended", "0"),
                ("free", "3"),
                ("Total Storage", "6 KB"),
                ("Inbox Storage", "4 KB"),
                ("File Storage", "2 KB"),
                ("Connections", "5"),
                ("Version", "1.2.3"),
            ]
        );
    }

    /// A failed dashboard read lands on the admin page's `error-message`; a
    /// subsequent successful load clears it (fresh truth). Exercises the shell's
    /// `apply_outcome` for the Dashboard variants.
    #[test]
    fn dashboard_failure_then_success_bridges_and_clears_the_error() {
        let mut app = crate::app::tests::test_app();
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::DashboardLoaded(DashboardSnapshot {
                error: Some("fauna.admin.stats: boom".into()),
                ..snapshot()
            }),
        );
        let shown = app.errors.get(&Page::Admin).cloned().unwrap_or_default();
        assert!(shown.contains("boom"), "the failed read's detail: {shown}");
        assert!(
            app.admin.dash.is_some(),
            "the other reads' cards still render"
        );
        super::super::apply_outcome(&mut app, super::super::Outcome::DashboardLoaded(snapshot()));
        assert!(!app.errors.contains_key(&Page::Admin));
    }
}
