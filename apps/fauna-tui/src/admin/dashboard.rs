//! The admin Dashboard sub-page (`admin-dashboard`) — the shell's landing surface
//! (`admin.md` § 1 Dashboard). Split out of the shell (`super`) when the sub-nav
//! landed, mirroring `settings/`'s per-sub-page files.

use fauna_i18n::strings::admin as t;
use fauna_protocol::admin::AdminStatsReply;
use fauna_ui_ids as ids;

use super::AdminState;
use crate::element::{Element, Gesture};
use crate::pages::Page;

/// The Dashboard's read model — the two shared reads the linux dashboard makes,
/// loaded together so the cards render as one snapshot: the nest stats
/// (`fauna.admin.stats`) and the running server version (`fauna.admin.status`),
/// plus the mail health state (`fauna.bridges.mail_health`) for the Mail card.
#[derive(Debug, Clone)]
pub struct DashboardSnapshot {
    pub stats: AdminStatsReply,
    /// The running nest version (`AdminStatusReply.version`) — the Version stat
    /// card. A separate read from the stats, but one snapshot to the UI.
    pub version: String,
    /// The mail health readout's categorical `state` (`fauna.bridges.mail_health`)
    /// — the Mail stat card (`mail-deliverability.md` § The mail health readout),
    /// so a broken state shows on the shell's landing page. `None` when the read
    /// did not answer: the card is then omitted, never the dashboard.
    pub mail_state: Option<String>,
}

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
        for (label, value) in dashboard_cards(snapshot) {
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
            out.push(Element::label(ids::ADMIN_STAT_CARD_LABEL, label));
            out.push(Element::label(ids::ADMIN_STAT_CARD_VALUE, value));
        }
    }
    out
}

/// The dashboard stat cards, lifted from linux
/// (`views/admin.rs::build_dashboard_page` and its stats/status refresh,
/// priority #1/#4) with the same shared labels: Registered Users, Total Storage
/// Used, Total Inbox Messages, Active Sessions (`admin::view::*`, from
/// `fauna.admin.stats`), then Version (`admin::dashboard::VERSION`, from
/// `fauna.admin.status`). The shared `navigate_dashboard` e2e helper gates on the
/// Users **and** Version cards being present, so both reads feed this list.
///
/// The **inbox card stays "—"** on purpose: the nest exposes inbox *bytes*
/// (`AdminStatsReply.total_inbox_bytes`), not a message *count*, so rendering
/// bytes under a "Total Inbox Messages" label would be a label/data mismatch —
/// linux leaves this card blank for exactly this reason (`admin.rs`, the
/// `total_inbox_messages` fallback that matches no reply key), and tui matches
/// that choice rather than inventing a divergent label.
///
/// Then **Mail**, when the health read answered: its value is the same shared
/// label the `admin-mail` status line opens with
/// (`fauna_core::format::mail_health_state_label` — one more instance of the
/// existing card, no new id).
fn dashboard_cards(snapshot: &DashboardSnapshot) -> Vec<(&'static str, String)> {
    let stats = &snapshot.stats;
    let mut cards = vec![
        (t::view::REGISTERED_USERS, stats.total_users.to_string()),
        (
            t::view::TOTAL_STORAGE_USED,
            crate::format::byte_size(stats.total_storage_bytes),
        ),
        (t::view::TOTAL_INBOX_MESSAGES, "—".to_string()),
        (t::view::ACTIVE_SESSIONS, stats.ws_connections.to_string()),
        (t::dashboard::VERSION, snapshot.version.clone()),
    ];
    if let Some(state) = &snapshot.mail_state {
        cards.push((
            t::dashboard::MAIL,
            fauna_core::format::mail_health_state_label(state).resolve(fauna_i18n::strings::lookup),
        ));
    }
    cards
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::Page;

    fn snapshot() -> DashboardSnapshot {
        DashboardSnapshot {
            stats: AdminStatsReply {
                total_users: 3,
                users_by_tier: Vec::new(),
                suspended_users: 0,
                total_inbox_bytes: 4096,
                total_storage_bytes: 2048,
                ws_connections: 5,
                extra: Default::default(),
            },
            version: "1.2.3".to_string(),
            mail_state: None,
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

    /// Once the snapshot loads, the page paints the five cards as flat
    /// `-label`/`-value` leaf pairs in linux's order, with linux's exact value
    /// bindings.
    /// The label set must include both "Users" and "Version" — the two the
    /// shared `navigate_dashboard` e2e helper gates on.
    #[test]
    fn loaded_dashboard_paints_the_stat_cards() {
        let mut app = crate::app::tests::test_app();
        app.admin.dash = Some(snapshot());
        let els = dashboard_elements(&app.admin);
        let ids: Vec<String> = els.iter().map(|e| e.id.clone()).collect();
        let mut expected = vec!["admin-dashboard-heading", "admin-nav-back"];
        for _ in 0..5 {
            expected.push("admin-stat-card");
            expected.push("admin-stat-card-label");
            expected.push("admin-stat-card-value");
        }
        assert_eq!(ids, expected);

        let labels: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "admin-stat-card-label")
            .map(|e| e.text.as_str())
            .collect();
        let values: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "admin-stat-card-value")
            .map(|e| e.text.as_str())
            .collect();
        assert!(
            labels.iter().any(|l| l.contains("Users")),
            "a card label contains 'Users' (navigate_dashboard gates on it): {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Version")),
            "a card label contains 'Version' (navigate_dashboard gates on it): {labels:?}"
        );
        assert_eq!(values[0], "3", "Registered Users = total_users");
        assert!(
            values[1].chars().any(|c| c.is_ascii_digit()),
            "Total Storage Used = byte_size(total_storage_bytes): {}",
            values[1]
        );
        assert_eq!(
            values[2], "—",
            "Total Inbox Messages stays blank (bytes ≠ count)"
        );
        assert_eq!(values[3], "5", "Active Sessions = ws_connections");
        assert_eq!(values[4], "1.2.3", "Version = AdminStatusReply.version");
    }

    /// With the health read answered, a sixth card, "Mail", carries the shared
    /// label of the state; without it the five cards stand alone.
    #[test]
    fn mail_card_carries_the_shared_state_label() {
        let mut app = crate::app::tests::test_app();
        app.admin.dash = Some(DashboardSnapshot {
            mail_state: Some("warming_up".into()),
            ..snapshot()
        });
        let els = dashboard_elements(&app.admin);
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
        assert_eq!(pairs.len(), 6);
        assert_eq!(pairs[5], ("Mail", "Mail: warming up"));

        // An unknown state from a newer nest reads "needs attention".
        app.admin.dash = Some(DashboardSnapshot {
            mail_state: Some("on_fire".into()),
            ..snapshot()
        });
        let value = dashboard_elements(&app.admin)
            .into_iter()
            .rfind(|e| e.id == "admin-stat-card-value")
            .map(|e| e.text);
        assert_eq!(value.as_deref(), Some("Mail: needs attention"));
    }

    /// A failed dashboard read lands on the admin page's `error-message`; a
    /// subsequent successful load clears it (fresh truth). Exercises the shell's
    /// `apply_outcome` for the Dashboard variants.
    #[test]
    fn dashboard_failure_then_success_bridges_and_clears_the_error() {
        let mut app = crate::app::tests::test_app();
        super::super::apply_outcome(&mut app, super::super::Outcome::Failed("boom".into()));
        assert_eq!(
            app.errors.get(&Page::Admin).map(String::as_str),
            Some("boom")
        );
        super::super::apply_outcome(&mut app, super::super::Outcome::DashboardLoaded(snapshot()));
        assert!(!app.errors.contains_key(&Page::Admin));
        assert!(app.admin.dash.is_some());
    }
}
