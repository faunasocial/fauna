//! The STANDING pending-actions section (`docs/goal/ui/settings.md` §
//! Pending actions, ui.yaml's five `pending-action*` ids) — the Account
//! page's cancellable-window listing for the three delayed verbs (handle
//! change, account delete, snapshot delete). tui shipped this first
//! (`apps/fauna-tui/src/settings/account.rs::pending_actions_elements`);
//! this module ports the same shape: **always present** (never a
//! conditional render — that would hide the affordance exactly when a
//! mis-clicker goes looking for it), a title that answers honestly across
//! three states (bare title un-hydrated / empty-state line / counted
//! title), and one row per still-`pending` action with its description,
//! execute-after time, and a **one-click** cancel (no confirm — cancelling
//! is the safe direction).
//!
//! **Never feed a delayed verb's echoed new value into any local cache** —
//! the change has not applied (the section's own iron-clad,
//! `settings.md` § Pending actions): `target` exists only to be
//! *described*, never stored as state.

use adw::prelude::*;
use fauna_protocol::pending_actions::PendingActionSummary;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use crate::i18n::strings::settings::pending_actions as pa;

struct Widgets {
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
}

#[derive(Default)]
struct State {
    /// `None` = not yet hydrated (bare title); `Some(vec![])` = hydrated and
    /// empty; `Some(rows)` = hydrated with rows. Mirrors tui's
    /// `SettingsState::pending_actions`.
    rows: Option<Vec<PendingActionSummary>>,
}

/// Build the "Pending actions" preferences group. Returns the group plus a
/// refresh closure the caller should invoke whenever the page becomes
/// visible (the same shape `recovery_kit::build_recovery_kit_group` uses) —
/// the list is read fresh every time, never trusted stale.
pub fn build_pending_actions_group() -> (adw::PreferencesGroup, Rc<dyn Fn()>) {
    let group = adw::PreferencesGroup::builder().title(pa::TITLE).build();
    crate::testid::set_test_id(&group, ids::PENDING_ACTIONS_SECTION);

    let widgets = Rc::new(Widgets {
        group: group.clone(),
        rows: RefCell::new(Vec::new()),
    });

    let state = Rc::new(RefCell::new(State::default()));

    crate::settings::set_pending_actions_handler(Rc::new({
        let state = state.clone();
        let widgets = widgets.clone();
        move |result| {
            match result {
                Ok(rows) => state.borrow_mut().rows = Some(rows),
                Err(msg) => {
                    tracing::error!("[settings/pending_actions] list failed: {msg}");
                }
            }
            repaint(&state.borrow(), &widgets);
        }
    }));

    // Initial read, so the section does not sit on the bare title until the
    // user navigates away and back.
    if let Some(client) = crate::settings::get_client() {
        client.fetch_pending_actions();
    }

    let refresh: Rc<dyn Fn()> = Rc::new(|| {
        if let Some(client) = crate::settings::get_client() {
            client.fetch_pending_actions();
        }
    });

    (group, refresh)
}

/// Repaint the section's title (three-state, never a settled "nothing
/// scheduled" claim with no basis) and its row list from `state`, never a
/// local match on anything but the fetched projection (priority #2).
fn repaint(state: &State, widgets: &Widgets) {
    match &state.rows {
        None => widgets.group.set_title(pa::TITLE),
        Some(rows) if rows.is_empty() => widgets.group.set_title(pa::NONE_SCHEDULED),
        Some(rows) => widgets
            .group
            .set_title(&pa::title_count(&rows.len().to_string())),
    }

    let mut existing = widgets.rows.borrow_mut();
    for row in existing.drain(..) {
        widgets.group.remove(&row);
    }
    if let Some(rows) = &state.rows {
        for row in rows {
            let action_row = build_pending_action_row(row);
            widgets.group.add(&action_row);
            existing.push(action_row);
        }
    }
}

fn build_pending_action_row(row: &PendingActionSummary) -> adw::ActionRow {
    let description = row.description();
    let applies_at = pa::applies(&fauna_core::format::format_unix_local(row.execute_after));
    let action_row = adw::ActionRow::builder()
        .title(&description)
        .subtitle(&applies_at)
        .build();
    action_row.add_prefix(&super::marker(ids::PENDING_ACTION_ITEM));
    action_row.add_suffix(&value_marker(ids::PENDING_ACTION_DESCRIPTION, &description));
    action_row.add_suffix(&value_marker(
        ids::PENDING_ACTION_EXECUTE_AFTER,
        &applies_at,
    ));

    let cancel_btn = gtk::Button::builder()
        .label(pa::CANCEL)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    crate::testid::set_test_id(&cancel_btn, ids::PENDING_ACTION_CANCEL_BUTTON);
    crate::offline_gate::declare_wire_kind(&cancel_btn, "fauna.pending_actions.cancel");
    let id = row.id;
    cancel_btn.connect_clicked(move |_| {
        if let Some(client) = crate::settings::get_client() {
            client.cancel_pending_action(id);
        }
    });
    action_row.add_suffix(&cancel_btn);

    action_row
}

fn value_marker(id: &str, value: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(value));
    label.set_use_markup(false);
    label.set_height_request(1);
    label.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&label, id);
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::widget_names;

    fn row(action_type: &str, target: Option<&str>, execute_after: i64) -> PendingActionSummary {
        PendingActionSummary {
            id: 1,
            action_type: action_type.to_string(),
            target: target.map(str::to_string),
            status: "pending".to_string(),
            created_at: 0,
            execute_after,
            requires_quorum: 0,
            approvals: Vec::new(),
            extra: Default::default(),
        }
    }

    /// `pending_description`'s full verb/skew coverage is now the shared
    /// `fauna_protocol::pending_actions` test
    /// (`describe_pending_action_names_each_verb_and_survives_skew`) — this
    /// just proves `build_pending_action_row` actually calls through to it.
    #[test]
    fn row_description_delegates_to_the_shared_renderer() {
        assert_eq!(
            row("handle.change", Some("bob"), 1).description(),
            pa::change_handle_to("bob")
        );
    }

    /// The section answers honestly across its three states (never a settled
    /// "nothing scheduled" claim before the first list read lands), and a
    /// hydrated row exposes every `pending-action-*` id ui.yaml lists,
    /// indexed by repetition (one full id-set per row).
    #[test]
    fn repaint_answers_honestly_across_the_three_states() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let group = adw::PreferencesGroup::builder().title(pa::TITLE).build();
            crate::testid::set_test_id(&group, ids::PENDING_ACTIONS_SECTION);
            let widgets = Widgets {
                group: group.clone(),
                rows: RefCell::new(Vec::new()),
            };

            // Un-hydrated — bare title, no basis for any claim yet, no rows.
            repaint(&State { rows: None }, &widgets);
            assert_eq!(group.title().as_str(), pa::TITLE);
            assert!(
                !widget_names(&group)
                    .iter()
                    .any(|n| n.as_str() == ids::PENDING_ACTION_ITEM)
            );

            // Hydrated and empty — the honest "nothing scheduled" line, still no rows.
            repaint(
                &State {
                    rows: Some(Vec::new()),
                },
                &widgets,
            );
            assert_eq!(group.title().as_str(), pa::NONE_SCHEDULED);
            assert!(
                !widget_names(&group)
                    .iter()
                    .any(|n| n.as_str() == ids::PENDING_ACTION_ITEM)
            );

            // Hydrated with two rows — counted title, both id-sets present,
            // and the prior (empty) repaint left no stale rows behind.
            repaint(
                &State {
                    rows: Some(vec![
                        row("handle.change", Some("carol"), 111),
                        row("account.delete", None, 222),
                    ]),
                },
                &widgets,
            );
            assert_eq!(group.title().to_string(), pa::title_count("2"));
            let names = widget_names(&group);
            assert_eq!(
                names
                    .iter()
                    .filter(|n| n.as_str() == ids::PENDING_ACTION_ITEM)
                    .count(),
                2
            );
            assert_eq!(
                names
                    .iter()
                    .filter(|n| n.as_str() == ids::PENDING_ACTION_DESCRIPTION)
                    .count(),
                2
            );
            assert_eq!(
                names
                    .iter()
                    .filter(|n| n.as_str() == ids::PENDING_ACTION_EXECUTE_AFTER)
                    .count(),
                2
            );
            assert_eq!(
                names
                    .iter()
                    .filter(|n| n.as_str() == ids::PENDING_ACTION_CANCEL_BUTTON)
                    .count(),
                2
            );
        });
    }
}
