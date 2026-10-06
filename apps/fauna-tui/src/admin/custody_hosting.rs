//! The admin Held-Custody sub-page (`admin-custody-hosting`) — the nest-wide
//! custody-hosting registry.
//!
//! **Why this page exists.** `fauna.custody.hosting.register` is a User-class
//! door: any account holder here can arm a standing outbound pull that makes
//! this nest dial an address they chose and keep what it serves. The nest-side
//! bounds (a per-row byte ceiling, a per-host row cap, the tier charge) landed
//! first, but a bound is only half of *no client-causable unrecoverable nest
//! state* — the other half is that an admin can SEE and DROP what was planted.
//! `fauna.custody.hosting.list` is host-scoped, so before these doors the only
//! remedy was `sqlite3` on `nest.db` plus `rm -rf`, which the invariant
//! forbids outright.
//!
//! **Read** is `fauna.admin.custody_hosting.list` through the shared
//! [`AdminHostingClient`], projected by the shared
//! [`admin_hosting_rows`] fold (priority #2 — the six lift apps render the
//! same rows in the same order, rather than each re-deriving them).
//! **Write** is `fauna.admin.custody_hosting.remove`, behind an inline
//! arm/confirm because removing frees bytes that stop deliberately does not.
//!
//! The shell (`super`) owns the client, the ops and the fold; this file is
//! paint only — the `admin/web.rs` shape.
//!
//! [`AdminHostingClient`]: fauna_client_capabilities::custody_hosting::AdminHostingClient
//! [`admin_hosting_rows`]: fauna_client_capabilities::view_model::admin_hosting_rows

#[cfg(test)]
use fauna_client_capabilities::view_model::ReceiptState;
use fauna_client_capabilities::view_model::{AdminHostingRowView, custody_hosting_budget_text};
use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::{Action, AdminState};
use crate::element::{Element, Gesture};
use crate::pages::Page;

/// An actor id is 64 hex chars; a registry row is scanned, not read. Abbreviate
/// to the same 8-char prefix every other tui actor rendering uses, so the
/// column stays comparable across rows.
fn short_actor(hex: &str) -> String {
    match hex.char_indices().nth(8) {
        Some((idx, _)) => format!("{}…", &hex[..idx]),
        None => hex.to_string(),
    }
}

/// The budget in force. `0` is not "no bytes allowed" — it means the row
/// carries no cap and the pump substitutes the hard-coded default, so saying
/// *Default* is the honest rendering and a printed `0 B` would be a lie.
fn budget_text(cap: u64) -> String {
    custody_hosting_budget_text(cap, fauna_i18n::strings::lookup)
}

/// One registry row's elements. The row container repeats its one ID per row
/// (`indexed: true`) and every child is scoped `within` it by position — the
/// convention-1 shape a driver's `scope="admin-custody-hosting-row[i]"` or
/// `index=i` resolves; never an index baked into the ID.
fn row_elements(i: usize, row: &AdminHostingRowView, armed: bool) -> Vec<Element> {
    let children = vec![
        Element::label(
            ids::ADMIN_CUSTODY_HOSTING_HOST,
            short_actor(&row.host_actor_id),
        )
        .labelled(t::custody_hosting::HOST),
        Element::label(
            ids::ADMIN_CUSTODY_HOSTING_OWNER,
            short_actor(&row.owner_actor_id),
        )
        .labelled(t::custody_hosting::OWNER),
        // Verbatim, never prettified: an admin reading this page is looking for
        // exactly the address the pump dials.
        Element::label(ids::ADMIN_CUSTODY_HOSTING_URL, row.owner_nest_url.clone())
            .labelled(t::custody_hosting::URL),
        Element::label(
            ids::ADMIN_CUSTODY_HOSTING_BUDGET,
            budget_text(row.retained_bytes_cap),
        )
        .labelled(t::custody_hosting::BUDGET),
        Element::label(
            ids::ADMIN_CUSTODY_HOSTING_HELD,
            fauna_core::format::byte_size(row.held_bytes).resolve(fauna_i18n::strings::lookup),
        )
        .labelled(t::custody_hosting::HELD),
        // Paused is a real, distinct state from Active — a stopped row still
        // holds its bytes, which is the whole reason remove exists beside stop.
        Element::label(
            ids::ADMIN_CUSTODY_HOSTING_STOPPED,
            if row.stopped {
                t::custody_hosting::STOPPED
            } else {
                t::custody_hosting::ACTIVE
            },
        ),
        Element::label(
            ids::ADMIN_CUSTODY_HOSTING_RECEIPT,
            fauna_client_capabilities::view_model::receipt_text(row.receipt_state),
        ),
        Element::gesture_button(
            ids::ADMIN_CUSTODY_HOSTING_REMOVE_BUTTON,
            t::custody_hosting::REMOVE,
            // A row whose confirm is already armed cannot re-arm: the confirm
            // names ONE row, and a second arm would silently retarget it.
            !armed,
            Gesture::Admin(Action::OpenCustodyHostingRemoveConfirm {
                host_actor_id: row.host_actor_id.clone(),
                grant_id: row.grant_id.clone(),
            }),
        ),
    ];
    let mut els = vec![Element::label(
        ids::ADMIN_CUSTODY_HOSTING_ROW,
        String::new(),
    )];
    els.extend(
        children
            .into_iter()
            .map(|e| e.within(ids::ADMIN_CUSTODY_HOSTING_ROW, i)),
    );
    if armed {
        els.push(Element::chrome(t::custody_hosting::REMOVE_CONFIRM_TITLE));
        els.push(Element::chrome(t::custody_hosting::REMOVE_CONFIRM_BODY));
        els.push(Element::gesture_button(
            ids::ADMIN_CUSTODY_HOSTING_REMOVE_CONFIRM_BUTTON,
            t::custody_hosting::REMOVE_CONFIRM,
            true,
            Gesture::Admin(Action::ConfirmCustodyHostingRemove),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_CUSTODY_HOSTING_REMOVE_CANCEL_BUTTON,
            t::custody_hosting::REMOVE_CANCEL,
            true,
            Gesture::Admin(Action::CancelCustodyHostingRemove),
        ));
    }
    els
}

pub(super) fn custody_hosting_elements(state: &AdminState) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::custody_hosting::TITLE),
        Element::chrome(t::custody_hosting::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
    ];

    let Some(snap) = state.custody_hosting.as_ref() else {
        // Pre-hydrate: no count, no empty state. "Nobody has asked this nest to
        // hold anything" and "the list has not answered yet" are different
        // facts, and this page must never render the reassuring one for the
        // unknown one.
        return els;
    };

    els.push(Element::label(
        ids::ADMIN_CUSTODY_HOSTING_COUNT,
        t::custody_hosting::count(&snap.rows.len().to_string()),
    ));

    if snap.rows.is_empty() {
        els.push(Element::label(
            ids::ADMIN_CUSTODY_HOSTING_EMPTY,
            t::custody_hosting::EMPTY,
        ));
    }

    for (i, row) in snap.rows.iter().enumerate() {
        let armed = state
            .custody_hosting_confirm
            .as_ref()
            .is_some_and(|(host, grant)| host == &row.host_actor_id && grant == &row.grant_id);
        els.extend(row_elements(i, row, armed));
    }

    // The remove's own verdict, present only once one has been attempted. Not
    // `error-message`: `removed: false` on a row someone else already dropped
    // is an honest no-op, not a failure, and an error line would report the
    // opposite of what happened.
    if let Some(status) = snap.status.as_ref() {
        els.push(Element::chrome(status.clone()));
    }

    els
}

/// This page's read/write failure — read by `App::screen_error_text`, not
/// painted here (`crate::admin::page_error` carries the why).
pub(super) fn page_error(state: &AdminState) -> Option<String> {
    state
        .custody_hosting
        .as_ref()
        .and_then(|s| s.error.as_deref())
        .filter(|e| !e.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{AdminHostingSnapshot, AdminPage};

    fn row(host: &str, grant: &[u8], held: u64) -> AdminHostingRowView {
        AdminHostingRowView {
            host_actor_id: host.to_string(),
            owner_actor_id: "0".repeat(64),
            owner_nest_url: "https://owner.example".to_string(),
            grant_id: grant.to_vec(),
            retained_bytes_cap: 8192,
            held_bytes: held,
            stopped: false,
            receipt_state: ReceiptState::Fresh,
        }
    }

    fn snap(rows: Vec<AdminHostingRowView>) -> AdminHostingSnapshot {
        AdminHostingSnapshot {
            rows,
            status: None,
            error: None,
        }
    }

    fn app_on_page(snapshot: Option<AdminHostingSnapshot>) -> crate::app::App {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::CustodyHosting;
        app.admin.custody_hosting = snapshot;
        app
    }

    #[test]
    fn a_populated_registry_paints_every_column_and_the_remove_button() {
        let app = app_on_page(Some(snap(vec![row(&"ab".repeat(32), b"g1", 4096)])));
        let els = custody_hosting_elements(&app.admin);
        let tagged: Vec<&str> = els
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        assert_eq!(
            tagged,
            vec![
                "page-heading",
                "admin-nav-back",
                "admin-custody-hosting-count",
                "admin-custody-hosting-row",
                "admin-custody-hosting-host",
                "admin-custody-hosting-owner",
                "admin-custody-hosting-url",
                "admin-custody-hosting-budget",
                "admin-custody-hosting-held",
                "admin-custody-hosting-stopped",
                "admin-custody-hosting-receipt",
                "admin-custody-hosting-remove-button",
            ],
            "a clean page paints no error-message and no armed confirm"
        );
    }

    /// Convention 1's `indexed: true` shape: the row container repeats ONE id
    /// and each child is scoped `within` its row by position. An index baked
    /// into the id (`admin-custody-hosting-row-0`) answers no driver query for
    /// `admin-custody-hosting-row` — the page's first e2e run found exactly that.
    #[test]
    fn rows_repeat_one_id_and_scope_their_children_by_position() {
        let app = app_on_page(Some(snap(vec![
            row(&"aa".repeat(32), b"g0", 900),
            row(&"bb".repeat(32), b"g1", 100),
        ])));
        let els = custody_hosting_elements(&app.admin);
        let rows = els
            .iter()
            .filter(|e| e.id == ids::ADMIN_CUSTODY_HOSTING_ROW)
            .count();
        assert_eq!(rows, 2, "one row container per registry row, same id");
        let hosts: Vec<&Vec<(String, usize)>> = els
            .iter()
            .filter(|e| e.id == ids::ADMIN_CUSTODY_HOSTING_HOST)
            .map(|e| &e.path)
            .collect();
        assert_eq!(
            hosts,
            vec![
                &vec![(ids::ADMIN_CUSTODY_HOSTING_ROW.to_string(), 0)],
                &vec![(ids::ADMIN_CUSTODY_HOSTING_ROW.to_string(), 1)],
            ],
            "each child sits within its own row"
        );
    }

    /// The unknown and the reassuring answer must not look alike: before the
    /// list answers there is no count and no empty state, and only an answered
    /// empty list says "nobody asked".
    #[test]
    fn an_unhydrated_page_is_not_an_empty_one() {
        let pre = custody_hosting_elements(&app_on_page(None).admin);
        assert!(
            !pre.iter().any(|e| e.id == ids::ADMIN_CUSTODY_HOSTING_EMPTY),
            "pre-hydrate must not claim the registry is empty"
        );
        assert!(!pre.iter().any(|e| e.id == ids::ADMIN_CUSTODY_HOSTING_COUNT));

        let answered = custody_hosting_elements(&app_on_page(Some(snap(Vec::new()))).admin);
        assert!(
            answered
                .iter()
                .any(|e| e.id == ids::ADMIN_CUSTODY_HOSTING_EMPTY),
            "an answered empty list says so"
        );
    }

    /// The confirm names ONE row. Arming row 1 must leave row 0's confirm
    /// unpainted, or an admin could confirm against a row they never armed.
    #[test]
    fn the_armed_confirm_belongs_to_exactly_one_row() {
        let mut app = app_on_page(Some(snap(vec![
            row(&"aa".repeat(32), b"g0", 900),
            row(&"bb".repeat(32), b"g1", 100),
        ])));
        app.admin.custody_hosting_confirm = Some(("bb".repeat(32), b"g1".to_vec()));

        let els = custody_hosting_elements(&app.admin);
        let ids_painted: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids_painted
                .iter()
                .filter(|id| **id == ids::ADMIN_CUSTODY_HOSTING_REMOVE_CONFIRM_BUTTON)
                .count(),
            1,
            "exactly one confirm is painted"
        );

        // …and it is row 1's: row 1's own remove button is the disabled one.
        let remove_enabled_by_row: Vec<(usize, bool)> = els
            .iter()
            .filter(|e| e.id == ids::ADMIN_CUSTODY_HOSTING_REMOVE_BUTTON)
            .map(|e| (e.path[0].1, e.enabled))
            .collect();
        assert_eq!(
            remove_enabled_by_row,
            vec![(0, true), (1, false)],
            "the armed row's re-arm is closed; an unarmed row stays actionable"
        );
    }

    /// A zero cap means "no cap on the row — the pump substitutes the default",
    /// which is the opposite of "zero bytes allowed". Printing `0 B` would
    /// state the opposite of the truth.
    #[test]
    fn a_capless_row_reads_as_default_never_as_zero_bytes() {
        let mut r = row(&"cc".repeat(32), b"g", 0);
        r.retained_bytes_cap = 0;
        let app = app_on_page(Some(snap(vec![r])));
        let els = custody_hosting_elements(&app.admin);
        let budget = els
            .iter()
            .find(|e| e.id == ids::ADMIN_CUSTODY_HOSTING_BUDGET)
            .expect("budget painted");
        assert_eq!(budget.text, t::custody_hosting::BUDGET_DEFAULT);
    }

    /// The three-state receipt word is never collapsed and never empty — the
    /// rule both custody sides already hold to.
    #[test]
    fn every_receipt_state_gets_its_own_word() {
        let words: Vec<&str> = [
            ReceiptState::Fresh,
            ReceiptState::Stale,
            ReceiptState::NoReceiptYet,
        ]
        .into_iter()
        .map(fauna_client_capabilities::view_model::receipt_text)
        .collect();
        assert!(words.iter().all(|w| !w.is_empty()));
        let unique: std::collections::BTreeSet<&&str> = words.iter().collect();
        assert_eq!(unique.len(), 3, "three states, three distinct words");
    }

    /// A read failure must NOT be a page-pushed `error-message` element — it
    /// rides `App::errors` so the cross-app `error_text()` can read it off
    /// `messages.error`.
    #[test]
    fn the_page_pushes_no_error_message_element_of_its_own() {
        let app = app_on_page(Some(AdminHostingSnapshot {
            rows: Vec::new(),
            status: None,
            error: Some("hosting list failed".to_string()),
        }));
        assert!(
            !custody_hosting_elements(&app.admin)
                .iter()
                .any(|e| e.id == "error-message"),
            "the id is registered by the ONE global funnel, never by the page"
        );
        assert_eq!(
            page_error(&app.admin).as_deref(),
            Some("hosting list failed")
        );
    }
}
