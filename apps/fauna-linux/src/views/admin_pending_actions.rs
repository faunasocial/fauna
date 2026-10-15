//! The Users hub's STANDING pending-admin-actions section
//! (`docs/goal/behavior/admin.md` § Pending admin actions, ui.yaml's
//! `admin-users-pending-section` + `admin-pending-action-*` ids) — the
//! co-admin approve / veto surface over every account's still-`pending`
//! delayed admin action. tui shipped this first
//! (`apps/fauna-tui/src/admin/users.rs::pending_section`); this ports the same
//! shape: always present, a heading that answers in the settings page's states
//! (bare title un-hydrated / empty line / counted title), one row per
//! `pending` action with the shared description plus who scheduled it, when it
//! runs and its approvals readout, and two one-click controls (approve, cancel
//! — no confirm). Failures route to the hub's `admin-users-action-error`.

use adw::prelude::*;
use fauna_client_admin::admin::AdminPendingActionSummary;
use fauna_ui_ids as ids;
use std::rc::Rc;

use crate::client::FaunaClient;
use crate::i18n::strings::{admin, settings::pending_actions as pa};
use crate::settings::{marker, value_marker};

/// The section's widgets: the heading (carries `admin-users-pending-section`)
/// and the row list.
#[derive(Clone)]
pub struct PendingActionsSection {
    pub section: gtk::Box,
    pub heading: gtk::Label,
    pub list: gtk::ListBox,
}

/// Build the (initially un-hydrated) section; [`render`] fills it.
pub fn build() -> PendingActionsSection {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    section.set_margin_top(8);

    let heading = gtk::Label::new(Some(admin::users_page::SECTION_PENDING));
    heading.add_css_class("title-4");
    heading.set_halign(gtk::Align::Start);
    heading.set_margin_start(12);
    heading.set_margin_top(8);
    heading.set_margin_bottom(4);
    crate::testid::set_test_id(&heading, ids::ADMIN_USERS_PENDING_SECTION);
    section.append(&heading);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    list.set_margin_start(8);
    list.set_margin_end(8);
    list.set_margin_bottom(8);
    section.append(&list);

    PendingActionsSection {
        section,
        heading,
        list,
    }
}

/// Repaint from a fresh `fauna.admin.pending_actions.list` read: narrow to the
/// still-`pending` rows (the console offers approve / cancel only on a live
/// action), set the heading, rebuild the rows.
pub fn render(
    section: &PendingActionsSection,
    actions: &[AdminPendingActionSummary],
    client: &Rc<FaunaClient>,
) {
    let rows: Vec<&AdminPendingActionSummary> =
        actions.iter().filter(|a| a.status == "pending").collect();
    section.heading.set_label(&if rows.is_empty() {
        admin::users_page::PENDING_NONE.to_string()
    } else {
        admin::users_page::pending_count(&rows.len().to_string())
    });
    while let Some(child) = section.list.first_child() {
        section.list.remove(&child);
    }
    for action in rows {
        section.list.append(&build_row(action, client));
    }
}

fn build_row(action: &AdminPendingActionSummary, client: &Rc<FaunaClient>) -> adw::ActionRow {
    let description = format!(
        "{} {}",
        fauna_protocol::pending_actions::describe_pending_action(
            &action.action_type,
            action.target.as_deref(),
        ),
        admin::users_page::pending_by(&fauna_core::format::short_id(
            &fauna_core::format::hex_full(action.actor_id.as_ref())
        )),
    );
    let applies_at = pa::applies(&fauna_core::format::format_unix_local(action.execute_after));
    let approvals = admin::users_page::pending_approvals(
        &action.approvals.len().to_string(),
        &action.requires_quorum.to_string(),
    );

    let row = adw::ActionRow::builder()
        .title(&description)
        .subtitle(&applies_at)
        .build();
    row.add_prefix(&marker(ids::ADMIN_PENDING_ACTION_ITEM));
    row.add_suffix(&value_marker(
        ids::ADMIN_PENDING_ACTION_DESCRIPTION,
        &description,
    ));
    row.add_suffix(&value_marker(
        ids::ADMIN_PENDING_ACTION_EXECUTE_AFTER,
        &applies_at,
    ));
    let approvals_label = gtk::Label::new(Some(&approvals));
    approvals_label.add_css_class("dim-label");
    crate::testid::set_test_id(&approvals_label, ids::ADMIN_PENDING_ACTION_APPROVALS);
    row.add_suffix(&approvals_label);

    let id = action.id;
    let approve_btn = gtk::Button::builder()
        .label(admin::users_page::PENDING_APPROVE)
        .valign(gtk::Align::Center)
        .build();
    crate::testid::set_test_id(&approve_btn, ids::ADMIN_PENDING_ACTION_APPROVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&approve_btn, "fauna.pending_actions.approve");
    {
        let client = Rc::clone(client);
        approve_btn.connect_clicked(move |_| client.approve_admin_pending_action(id));
    }
    row.add_suffix(&approve_btn);

    let cancel_btn = gtk::Button::builder()
        .label(pa::CANCEL)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    crate::testid::set_test_id(&cancel_btn, ids::ADMIN_PENDING_ACTION_CANCEL_BUTTON);
    crate::offline_gate::declare_wire_kind(&cancel_btn, "fauna.pending_actions.cancel");
    {
        let client = Rc::clone(client);
        cancel_btn.connect_clicked(move |_| client.cancel_admin_pending_action(id));
    }
    row.add_suffix(&cancel_btn);

    row
}
