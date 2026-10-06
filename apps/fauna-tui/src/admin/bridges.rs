//! The admin Bridges sub-page (`admin-bridges-pending`) — pending-bridge
//! approval **and** the approved-bridge roster with service-user key rotation
//! (`admin.md` §§ Bridge display naming / Approved-bridges roster + rotate
//! service-user key; rotation flow owner `mail-bridge-lifecycle.md`
//! § Service-user re-keying).
//!
//! Two sections on one page — deliberately: there is no separate bridges-detail
//! page, the approval page carries the running-phase roster too (the flat-page
//! decision, `admin.md` § Approved-bridges roster).
//!
//! - **Pending approval** — one `admin-bridges-pending-card` per bridge in
//!   `list_pending_bridges`, showing the friendly per-role name above the
//!   technical role (so the admin reads "what is this" before "which protocol
//!   role"), the pubkey the admin verifies against the admin's fingerprint,
//!   and approve / reject.
//! - **Approved bridges** — one `admin-bridges-approved-card` per row of
//!   `list_service_users(status="approved")`, each with a **Rotate
//!   service-user key** button.
//!
//! The rotate confirm is an **inline reveal in this page's own element list**,
//! never a modal or a route. That is not cosmetic: the shared e2e asserts
//! `is_visible` on `admin-bridges-rotate-warning-text` *synchronously* right
//! after the rotate click, and an async-opening dialog loses that race — which
//! is why all six other apps converged on an inline reveal too (`admin.md`
//! § Approved-bridges roster).
//!
//! A dumb renderer of the shared `BridgeApprovalMachine`
//! (`fauna-client-mail-settings::bridge_approval`), including the per-role name
//! (the shared `bridge_display_name`, never a local map). The shell (`super`) owns the machine, op and fold; this file is
//! paint only.

use fauna_client_mail_settings::bridge_approval::bridge_display_name;
use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::{Action, AdminState};
use crate::element::{Element, Gesture};
use crate::pages::Page;

/// An empty [`Element`] never registers with the frame, so a per-card field that
/// can render empty would silently drop that card's row from its own id's index
/// sequence — leaving the driver's positional `index=i` addressing a *different*
/// bridge than the pubkey-hex it matched. Every optional card field goes through
/// here so each card contributes exactly one element per id.
fn or_placeholder(text: String) -> String {
    if text.is_empty() {
        t::bridges_pending::SOURCE_IP_UNKNOWN.to_string()
    } else {
        text
    }
}

pub(super) fn bridges_elements(state: &AdminState) -> Vec<Element> {
    let snap = state.bridges_snapshot.as_ref();
    let pending = snap.map(|s| s.pending.as_slice()).unwrap_or_default();
    let approved = snap.map(|s| s.approved.as_slice()).unwrap_or_default();

    let mut els = vec![
        // The page's ui.yaml landmark is the shared `page-heading`, not an
        // `admin-bridges-*-heading` (this page block names `page-heading`).
        Element::label(ids::PAGE_HEADING, t::bridges_pending::TITLE),
        Element::chrome(t::bridges_pending::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
    ];

    // ── Pending approval ────────────────────────────────────────────────────
    els.push(Element::label(
        ids::ADMIN_BRIDGES_PENDING_SECTION,
        t::bridges_pending::PENDING_SECTION,
    ));
    if pending.is_empty() {
        els.push(Element::chrome(t::bridges_pending::EMPTY));
    }
    for bridge in pending {
        // The card marker itself is the indexed component anchor; the fields
        // below it paint in ui.yaml order, so `index=i` lines up across every id.
        els.push(Element::label(
            ids::ADMIN_BRIDGES_PENDING_CARD,
            crate::wizard::localized(&bridge_display_name(&bridge.requested_role)),
        ));
        els.push(Element::label(
            ids::ADMIN_BRIDGES_PENDING_CARD_NAME,
            crate::wizard::localized(&bridge_display_name(&bridge.requested_role)),
        ));
        els.push(
            Element::label(
                ids::ADMIN_BRIDGES_PENDING_PUBKEY_HEX,
                bridge.pubkey_hex.clone(),
            )
            .labelled(t::bridges_pending::PUBKEY),
        );
        // The technical role still renders below the friendly name — it drives
        // the per-role allowlist applied on approve, so nothing is lost
        // (`admin.md` § Bridge display naming).
        els.push(
            Element::label(
                ids::ADMIN_BRIDGES_PENDING_REQUESTED_ROLE,
                bridge.requested_role.clone(),
            )
            .labelled(t::bridges_pending::ROLE),
        );
        els.push(
            Element::label(
                ids::ADMIN_BRIDGES_PENDING_SOURCE_IP,
                bridge
                    .source_ip
                    .clone()
                    .unwrap_or_else(|| t::bridges_pending::SOURCE_IP_UNKNOWN.to_string()),
            )
            .labelled(t::bridges_pending::SOURCE_IP),
        );
        els.push(
            Element::label(
                ids::ADMIN_BRIDGES_PENDING_FIRST_SEEN_AT,
                // `first_seen_at` is epoch-**millis** on the wire; the shared
                // relative-time bucketing takes micros. An unset timestamp
                // formats empty, and an empty element never registers — which
                // would silently shift this id's row indexes out of step with
                // the pubkey-hex ones the driver addresses cards by. The
                // placeholder keeps one element per card, always.
                or_placeholder(crate::format::format_epoch_us(
                    bridge.first_seen_at as i64 * 1_000,
                )),
            )
            .labelled(t::bridges_pending::FIRST_SEEN),
        );
        els.push(Element::gesture_button(
            ids::ADMIN_BRIDGES_PENDING_APPROVE_BUTTON,
            t::bridges_pending::APPROVE,
            true,
            Gesture::Admin(Action::ApproveBridge {
                pubkey_hex: bridge.pubkey_hex.clone(),
                role: bridge.requested_role.clone(),
            }),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_BRIDGES_PENDING_REJECT_BUTTON,
            t::bridges_pending::REJECT,
            true,
            Gesture::Admin(Action::RejectBridge {
                pubkey_hex: bridge.pubkey_hex.clone(),
            }),
        ));
    }

    // ── Approved bridges ────────────────────────────────────────────────────
    els.push(Element::label(
        ids::ADMIN_BRIDGES_APPROVED_SECTION,
        t::bridges_pending::APPROVED_SECTION,
    ));
    if approved.is_empty() {
        els.push(Element::chrome(t::bridges_pending::APPROVED_EMPTY));
    }
    for bridge in approved {
        els.push(Element::label(
            ids::ADMIN_BRIDGES_APPROVED_CARD,
            crate::wizard::localized(&bridge_display_name(&bridge.role)),
        ));
        els.push(Element::label(
            ids::ADMIN_BRIDGES_APPROVED_CARD_NAME,
            crate::wizard::localized(&bridge_display_name(&bridge.role)),
        ));
        els.push(
            Element::label(ids::ADMIN_BRIDGES_APPROVED_ROLE, bridge.role.clone())
                .labelled(t::bridges_pending::ROLE),
        );
        // This hex **is** the `bridge_actor_id` the Rotate action decodes.
        els.push(
            Element::label(
                ids::ADMIN_BRIDGES_APPROVED_PUBKEY_HEX,
                bridge.pubkey_hex.clone(),
            )
            .labelled(t::bridges_pending::PUBKEY),
        );
        els.push(
            Element::label(
                ids::ADMIN_BRIDGES_APPROVED_APPROVED_AT,
                // `None` on a non-conforming reply (only approve stamps it) — placeheld
                // for the same index-alignment reason as `-first-seen-at` above.
                or_placeholder(
                    bridge
                        .approved_at
                        .map(|ms| crate::format::format_epoch_us(ms as i64 * 1_000))
                        .unwrap_or_default(),
                ),
            )
            .labelled(t::bridges_pending::APPROVED_AT),
        );
        els.push(Element::gesture_button(
            ids::ADMIN_BRIDGES_APPROVED_ROTATE_BUTTON,
            t::bridges_pending::ROTATE,
            true,
            Gesture::Admin(Action::OpenRotateConfirm {
                pubkey_hex: bridge.pubkey_hex.clone(),
            }),
        ));
    }

    // ── The inline rotate confirm ───────────────────────────────────────────
    //
    // Painted into THIS page's element list (not a modal), so it registers in
    // the same frame as the click that armed it — see the module docs.
    if state.rotate_confirm.is_some() {
        els.push(Element::label(
            ids::ADMIN_BRIDGES_ROTATE_CONFIRM,
            t::bridges_rotate::TITLE,
        ));
        els.push(Element::label(
            ids::ADMIN_BRIDGES_ROTATE_WARNING_TEXT,
            t::bridges_rotate::WARNING,
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_BRIDGES_ROTATE_CONFIRM_BUTTON,
            t::bridges_rotate::CONFIRM,
            true,
            Gesture::Admin(Action::ConfirmRotate),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_BRIDGES_ROTATE_CANCEL_BUTTON,
            t::bridges_rotate::CANCEL,
            true,
            Gesture::Admin(Action::CancelRotate),
        ));
    }

    els
}

/// This page's error — read by `App::screen_error_text`, not painted here
/// (`crate::admin::page_error` carries the why). `BridgeApprovalSnapshot.error`
/// is a plain String (unlike Devices' `LocalizedText`) — the machine already
/// resolved it.
pub(super) fn page_error(state: &AdminState) -> Option<String> {
    state
        .bridges_snapshot
        .as_ref()
        .and_then(|s| s.error.as_deref())
        .filter(|e| !e.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{AdminPage, RotateConfirm};
    use fauna_client_mail_settings::bridge_approval::{
        ApprovedBridgeView, BridgeApprovalSnapshot, BridgeApprovalStatus, PendingBridgeView,
    };

    fn pending(role: &str, pubkey: &str) -> PendingBridgeView {
        PendingBridgeView {
            pubkey_hex: pubkey.to_string(),
            requested_role: role.to_string(),
            source_ip: None,
            first_seen_at: 0,
        }
    }

    fn approved(role: &str, pubkey: &str) -> ApprovedBridgeView {
        ApprovedBridgeView {
            pubkey_hex: pubkey.to_string(),
            role: role.to_string(),
            approved_at: None,
        }
    }

    fn snapshot(
        pending_rows: Vec<PendingBridgeView>,
        approved_rows: Vec<ApprovedBridgeView>,
    ) -> BridgeApprovalSnapshot {
        BridgeApprovalSnapshot {
            pending: pending_rows,
            approved: approved_rows,
            mail_enabled: None,
            caldav_enabled: None,
            carddav_enabled: None,
            webdav_enabled: None,
            status: BridgeApprovalStatus::Idle,
            error: None,
        }
    }

    fn app_with(snap: BridgeApprovalSnapshot) -> crate::app::App {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Bridges;
        app.admin.bridges_snapshot = Some(snap);
        app
    }

    fn texts<'a>(els: &'a [Element], id: &str) -> Vec<&'a str> {
        els.iter()
            .filter(|e| e.id == id)
            .map(|e| e.text.as_str())
            .collect()
    }

    /// An mta card names "Mail bridge" (SMTP only — no calendar) and still shows
    /// the technical role, which is what drives the approve-time allowlist.
    #[test]
    fn mta_pending_card_names_mail_and_keeps_the_role() {
        let app = app_with(snapshot(vec![pending("mta", "aa11")], Vec::new()));
        let els = bridges_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-bridges-pending-card-name"),
            vec![t::bridges_pending::NAME_MAIL]
        );
        assert_eq!(
            texts(&els, "admin-bridges-pending-requested-role"),
            vec!["mta"]
        );
        assert_eq!(
            texts(&els, "admin-bridges-pending-pubkey-hex"),
            vec!["aa11"]
        );
    }

    /// An mda serves IMAP **and** CalDAV, so its card names calendar — the other
    /// half of the shared per-role map (never a tui-local table).
    #[test]
    fn mda_pending_card_names_mail_and_calendar() {
        let app = app_with(snapshot(vec![pending("mda", "bb22")], Vec::new()));
        let els = bridges_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-bridges-pending-card-name"),
            vec![t::bridges_pending::NAME_MAIL_CALENDAR]
        );
    }

    /// Card fields paint in a stable per-row order, so the driver's positional
    /// `index=i` addresses the SAME bridge across every id on the card — the
    /// contract `_find_card` relies on when it matches a pubkey at index `i` and
    /// then reads `-requested-role` at that same index.
    #[test]
    fn per_row_indexes_line_up_across_every_card_id() {
        let app = app_with(snapshot(
            vec![pending("mta", "aa11"), pending("mda", "bb22")],
            Vec::new(),
        ));
        let els = bridges_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-bridges-pending-pubkey-hex"),
            vec!["aa11", "bb22"]
        );
        assert_eq!(
            texts(&els, "admin-bridges-pending-requested-role"),
            vec!["mta", "mda"]
        );
        assert_eq!(
            texts(&els, "admin-bridges-pending-card-name"),
            vec![
                t::bridges_pending::NAME_MAIL,
                t::bridges_pending::NAME_MAIL_CALENDAR
            ]
        );
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "admin-bridges-pending-approve-button")
                .count(),
            2
        );
    }

    /// Every card contributes exactly one element per id **even when its
    /// optional fields are unset** — an empty label would not register, dropping
    /// that card from the id's index sequence and silently mis-addressing every
    /// later row. Both rows here have `first_seen_at: 0` / `approved_at: None`,
    /// the exact case that formats empty.
    #[test]
    fn optional_card_fields_still_contribute_one_element_per_row() {
        let app = app_with(snapshot(
            vec![pending("mta", "aa11"), pending("mda", "bb22")],
            vec![approved("mta", "cc33"), approved("mda", "dd44")],
        ));
        let els = bridges_elements(&app.admin);
        for id in [
            "admin-bridges-pending-pubkey-hex",
            "admin-bridges-pending-first-seen-at",
            "admin-bridges-pending-source-ip",
        ] {
            assert_eq!(
                texts(&els, id).len(),
                2,
                "{id} must paint once per pending card"
            );
        }
        for id in [
            "admin-bridges-approved-pubkey-hex",
            "admin-bridges-approved-approved-at",
        ] {
            assert_eq!(
                texts(&els, id).len(),
                2,
                "{id} must paint once per approved card"
            );
        }
    }

    /// The approved roster is a separate id family from the pending cards, so a
    /// bridge that moved from one to the other is unambiguous to the driver.
    #[test]
    fn approved_roster_renders_its_own_id_family() {
        let app = app_with(snapshot(Vec::new(), vec![approved("mta", "cc33")]));
        let els = bridges_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-bridges-approved-pubkey-hex"),
            vec!["cc33"]
        );
        assert_eq!(texts(&els, "admin-bridges-approved-role"), vec!["mta"]);
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "admin-bridges-approved-rotate-button")
                .count(),
            1
        );
        assert!(
            texts(&els, "admin-bridges-pending-pubkey-hex").is_empty(),
            "an approved bridge must not also paint as pending"
        );
    }

    /// No confirm armed ⇒ no dialog elements at all, so `is_visible` on the
    /// warning is false before the rotate click.
    #[test]
    fn rotate_confirm_is_absent_until_armed() {
        let app = app_with(snapshot(Vec::new(), vec![approved("mta", "cc33")]));
        let els = bridges_elements(&app.admin);
        assert!(
            !els.iter()
                .any(|e| e.id == "admin-bridges-rotate-warning-text")
        );
        assert!(
            !els.iter()
                .any(|e| e.id == "admin-bridges-rotate-confirm-button")
        );
    }

    /// An armed rotate shows the base warning and NO DKIM warning for any role:
    /// the nest holds every DKIM key, so a re-key touches none.
    #[test]
    fn armed_rotate_shows_base_warning_and_no_dkim_warning() {
        for (role, pubkey) in [("mta", "cc33"), ("mda", "dd44")] {
            let mut app = app_with(snapshot(Vec::new(), vec![approved(role, pubkey)]));
            app.admin.rotate_confirm = Some(RotateConfirm {
                pubkey_hex: pubkey.to_string(),
            });
            let els = bridges_elements(&app.admin);
            assert_eq!(
                texts(&els, "admin-bridges-rotate-warning-text"),
                vec![t::bridges_rotate::WARNING]
            );
            assert!(
                !els.iter()
                    .any(|e| e.id == "admin-bridges-rotate-dkim-warning-text"),
                "a {role} rotation must not warn about DKIM"
            );
            assert!(
                els.iter()
                    .any(|e| e.id == "admin-bridges-rotate-confirm-button")
            );
            assert!(
                els.iter()
                    .any(|e| e.id == "admin-bridges-rotate-cancel-button")
            );
        }
    }

    /// A machine error bridges onto the page's `error-message` (rule 2).
    #[test]
    fn snapshot_error_reaches_the_screens_error_line() {
        let mut snap = snapshot(Vec::new(), Vec::new());
        snap.error = Some("list failed".to_string());
        let mut app = app_with(snap);
        app.session = Some(crate::app::tests::test_session());
        app.page = Page::Admin;
        app.admin.sub = AdminPage::Bridges;
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("list failed"),
            "the page error must reach the ONE funnel the paint, the registry and \
             `messages.error` all read"
        );
        assert!(
            !bridges_elements(&app.admin)
                .iter()
                .any(|e| e.id == "error-message"),
            "and must NOT be a second, page-pushed copy of the id"
        );
    }
}
