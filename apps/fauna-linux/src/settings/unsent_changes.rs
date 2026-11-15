//! The STANDING unsent-changes section (`account-offline-mutation.md` § The
//! offline-mutation contract → *The unsent-changes surface*, point 1;
//! ui.yaml's seven `unsent-change*` ids) — every change this device's outbox
//! holds that has not reached the nest. tui shipped it first
//! (`apps/fauna-tui/src/settings/account.rs::unsent_changes_elements`); this
//! module ports the same shape beside its sibling, the pending-actions section
//! (`super::pending_actions`): **always present**, a title answering in all
//! three states (bare title before the first read / empty line once read and
//! empty / counted title with rows), one row per intent in the shared model's
//! order with the shared describer's words, and — on a NOT-SENT row only —
//! retry and a confirmed discard.
//!
//! Read from the device outbox only (`crate::offline_outbox::queue`, the
//! `OfflineIntentQueue` doors `unsent_changes` / `retry_intent` /
//! `discard_intent`), never from the nest, so the list answers with the nest
//! down — which is when it matters. The runtime's generic drain tells the app
//! nothing, so the section re-reads itself every [`REFRESH_INTERVAL`] while it
//! is mapped (tui's `session::spawn_unsent_refresh` tick).

use adw::prelude::*;
use fauna_core::offline_intent::{UnsentChange, UnsentState};
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use crate::i18n::strings::settings::unsent_changes as uc;

/// How often a mapped section re-reads the outbox (tui's
/// `UNSENT_REFRESH_INTERVAL`).
pub const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

struct Widgets {
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
}

#[derive(Default)]
struct State {
    /// `None` = not read yet (bare title, never a settled "nothing" claim);
    /// `Some(vec![])` = read and empty; `Some(rows)` = read with rows.
    /// Mirrors tui's `SettingsState::unsent_changes`.
    rows: Option<Vec<UnsentChange>>,
    /// The intent id whose discard is armed (`unsent-change-discard-button`
    /// pressed, `unsent-change-discard-confirm-button` showing on that row).
    discard_armed: Option<String>,
}

/// What a click asks of the outbox.
#[derive(Clone, Copy)]
enum Settle {
    Retry,
    Discard,
}

struct Ctx {
    widgets: Widgets,
    state: RefCell<State>,
}

/// Build the "Unsent changes" preferences group. Returns the group plus a
/// refresh closure the caller invokes whenever the page becomes visible (the
/// `pending_actions::build_pending_actions_group` shape). While the group is
/// mapped it also re-reads itself every [`REFRESH_INTERVAL`].
pub fn build_unsent_changes_group() -> (adw::PreferencesGroup, Rc<dyn Fn()>) {
    let group = adw::PreferencesGroup::builder().title(uc::TITLE).build();
    crate::testid::set_test_id(&group, ids::UNSENT_CHANGES_SECTION);

    let ctx = Rc::new(Ctx {
        widgets: Widgets {
            group: group.clone(),
            rows: RefCell::new(Vec::new()),
        },
        state: RefCell::new(State::default()),
    });

    // Initial read, so the section does not sit on the bare title until the
    // first tick.
    read(&ctx);

    // The re-read edge: while the Account page shows, re-read every
    // `REFRESH_INTERVAL`; stop once the group is gone.
    {
        let weak = Rc::downgrade(&ctx);
        glib::timeout_add_local(REFRESH_INTERVAL, move || {
            let Some(ctx) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ctx.widgets.group.is_mapped() {
                read(&ctx);
            }
            glib::ControlFlow::Continue
        });
    }

    let refresh: Rc<dyn Fn()> = {
        let ctx = ctx.clone();
        Rc::new(move || read(&ctx))
    };

    (group, refresh)
}

/// Re-read the outbox's unsent changes on the client's runtime, then repaint.
/// A failed read keeps the last rows (a background read — logged, never put on
/// `error-message`).
fn read(ctx: &Rc<Ctx>) {
    let Some(client) = crate::settings::get_client() else {
        return;
    };
    let queue = crate::offline_outbox::queue();
    let ctx = ctx.clone();
    crate::async_helper::spawn_with_snapshot(
        &client.runtime_handle(),
        move || async move { queue.unsent_changes().await },
        move |read| apply_read(&ctx, read),
    );
}

/// Retry or discard one not-sent intent through the outbox's door, then
/// re-read the list in the same task — the row's new state (or its absence)
/// is the receipt.
fn settle(ctx: &Rc<Ctx>, intent_id: String, settle: Settle) {
    let Some(client) = crate::settings::get_client() else {
        return;
    };
    let Some(id) = fauna_core::offline_intent::parse_intent_id(&intent_id) else {
        tracing::error!("[settings/unsent_changes] not an intent id: {intent_id}");
        return;
    };
    let queue = crate::offline_outbox::queue();
    let ctx = ctx.clone();
    crate::async_helper::spawn_with_snapshot(
        &client.runtime_handle(),
        move || async move {
            let settled = match settle {
                Settle::Retry => queue.retry_intent(id).await,
                Settle::Discard => queue.discard_intent(id).await,
            };
            match settled {
                Ok(_) => queue.unsent_changes().await,
                Err(why) => Err(why),
            }
        },
        move |read| apply_read(&ctx, read),
    );
}

fn apply_read(ctx: &Rc<Ctx>, read: Result<Vec<UnsentChange>, String>) {
    match read {
        Ok(rows) => {
            let mut state = ctx.state.borrow_mut();
            if state.rows.as_ref() == Some(&rows) {
                return;
            }
            // A discard armed on a row that has left the list has nothing
            // left to confirm.
            if let Some(armed) = state.discard_armed.as_deref()
                && !rows.iter().any(|r| r.intent_id == armed)
            {
                state.discard_armed = None;
            }
            state.rows = Some(rows);
        }
        Err(why) => {
            tracing::debug!("[settings/unsent_changes] not re-read: {why}");
            return;
        }
    }
    repaint(ctx);
}

/// Repaint the title (three-state) and the rows from the state.
fn repaint(ctx: &Rc<Ctx>) {
    let state = ctx.state.borrow();
    let widgets = &ctx.widgets;
    widgets.group.set_title(&section_title(&state));

    let mut existing = widgets.rows.borrow_mut();
    for row in existing.drain(..) {
        widgets.group.remove(&row);
    }
    if let Some(rows) = &state.rows {
        for row in rows {
            let armed = state.discard_armed.as_deref() == Some(row.intent_id.as_str());
            let action_row = build_unsent_change_row(ctx, row, armed);
            widgets.group.add(&action_row);
            existing.push(action_row);
        }
    }
}

fn section_title(state: &State) -> String {
    match &state.rows {
        None => uc::TITLE.to_string(),
        Some(rows) if rows.is_empty() => uc::NONE.to_string(),
        Some(rows) => uc::title_count(&rows.len().to_string()),
    }
}

/// An unsent change's kind and target in words — the shared describer's
/// label, resolved here and composed nowhere else.
fn unsent_description(row: &UnsentChange) -> String {
    row.description.label.resolve(crate::i18n::strings::lookup)
}

/// An unsent change's state line: the shared state text, and on a not-sent
/// row why — the shared reason text (tui's `unsent_status` twin).
fn unsent_status(row: &UnsentChange) -> String {
    use fauna_protocol::offline_describe::{change_reason_text, change_state_text};
    let state = change_state_text(row).resolve(crate::i18n::strings::lookup);
    match change_reason_text(row) {
        Some(reason) => {
            uc::status_with_reason(&state, &reason.resolve(crate::i18n::strings::lookup))
        }
        None => state,
    }
}

/// One `unsent-change-item` row. The row is the item's automation scope
/// (`crate::testid::set_test_scope`), so a scoped read like
/// `unsent-change-item[i]/unsent-change-status` reaches the marker labels and
/// buttons beside the item's own marker.
fn build_unsent_change_row(ctx: &Rc<Ctx>, row: &UnsentChange, armed: bool) -> adw::ActionRow {
    let description = unsent_description(row);
    let status = unsent_status(row);
    let action_row = adw::ActionRow::builder()
        .title(&description)
        .subtitle(&status)
        .build();
    crate::testid::set_test_scope(&action_row, ids::UNSENT_CHANGE_ITEM);
    action_row.add_prefix(&super::marker(ids::UNSENT_CHANGE_ITEM));
    action_row.add_suffix(&super::value_marker(
        ids::UNSENT_CHANGE_DESCRIPTION,
        &description,
    ));
    action_row.add_suffix(&super::value_marker(ids::UNSENT_CHANGE_STATUS, &status));

    if !matches!(row.state, UnsentState::Unsent { .. }) {
        return action_row;
    }

    // The doors are this device's outbox, never the nest: no wire kind is
    // declared, so the offline gate keeps them live with the nest down.
    let retry_btn = gtk::Button::builder()
        .label(uc::RETRY)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    crate::testid::set_test_id(&retry_btn, ids::UNSENT_CHANGE_RETRY_BUTTON);
    {
        let ctx = Rc::downgrade(ctx);
        let intent_id = row.intent_id.clone();
        retry_btn.connect_clicked(move |_| {
            let Some(ctx) = ctx.upgrade() else { return };
            ctx.state.borrow_mut().discard_armed = None;
            settle(&ctx, intent_id.clone(), Settle::Retry);
        });
    }
    action_row.add_suffix(&retry_btn);

    let discard_btn = gtk::Button::builder()
        .label(uc::DISCARD)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "destructive-action"])
        .build();
    crate::testid::set_test_id(&discard_btn, ids::UNSENT_CHANGE_DISCARD_BUTTON);
    {
        let ctx = Rc::downgrade(ctx);
        let intent_id = row.intent_id.clone();
        discard_btn.connect_clicked(move |_| {
            let Some(ctx) = ctx.upgrade() else { return };
            ctx.state.borrow_mut().discard_armed = Some(intent_id.clone());
            // Repaint after the handler returns: the repaint removes this
            // very row (and its button) from the group.
            let ctx = ctx.clone();
            glib::idle_add_local_once(move || repaint(&ctx));
        });
    }
    action_row.add_suffix(&discard_btn);

    if armed {
        let confirm_btn = gtk::Button::builder()
            .label(uc::DISCARD_CONFIRM)
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        crate::testid::set_test_id(&confirm_btn, ids::UNSENT_CHANGE_DISCARD_CONFIRM_BUTTON);
        let ctx = Rc::downgrade(ctx);
        let intent_id = row.intent_id.clone();
        confirm_btn.connect_clicked(move |_| {
            let Some(ctx) = ctx.upgrade() else { return };
            ctx.state.borrow_mut().discard_armed = None;
            settle(&ctx, intent_id.clone(), Settle::Discard);
        });
        action_row.add_suffix(&confirm_btn);
    }

    action_row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::widget_names;
    use fauna_core::localized::LocalizedText;
    use fauna_core::offline_intent::IntentDescription;

    fn change(id: &str, state: UnsentState) -> UnsentChange {
        UnsentChange {
            intent_id: id.to_string(),
            kind: "fauna.posts.interact".to_string(),
            scope: "posts.interact:p".to_string(),
            state,
            created_at: 0,
            attempts: 0,
            description: IntentDescription {
                label: LocalizedText::key("settings.unsent_changes.kind_like"),
                target: None,
            },
            hold: None,
        }
    }

    fn count(names: &[String], id: &str) -> usize {
        names.iter().filter(|n| n.as_str() == id).count()
    }

    /// The section is standing in all three states, one row per change with
    /// the shared describer's words, retry and discard on a not-sent row only,
    /// and the confirm only once discard is armed — tui's
    /// `the_unsent_changes_section_is_standing_and_offers_retry_on_a_not_sent_row_only`
    /// twin.
    #[test]
    fn unsent_changes_section_is_standing_and_offers_retry_on_a_not_sent_row_only() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let group = adw::PreferencesGroup::builder().title(uc::TITLE).build();
            crate::testid::set_test_id(&group, ids::UNSENT_CHANGES_SECTION);
            let ctx = Rc::new(Ctx {
                widgets: Widgets {
                    group: group.clone(),
                    rows: RefCell::new(Vec::new()),
                },
                state: RefCell::new(State::default()),
            });

            // Not read yet — the bare title, no rows.
            repaint(&ctx);
            assert_eq!(group.title().as_str(), uc::TITLE);
            assert_eq!(count(&widget_names(&group), ids::UNSENT_CHANGE_ITEM), 0);

            // Read and empty — the empty line.
            apply_read(&ctx, Ok(Vec::new()));
            assert_eq!(group.title().as_str(), uc::NONE);
            assert_eq!(count(&widget_names(&group), ids::UNSENT_CHANGE_ITEM), 0);

            // Two rows: one not sent, one held.
            apply_read(
                &ctx,
                Ok(vec![
                    change(
                        "aa",
                        UnsentState::Unsent {
                            reason_code: "not_found".to_string(),
                        },
                    ),
                    change("bb", UnsentState::Held),
                ]),
            );
            assert_eq!(group.title().to_string(), uc::title_count("2"));
            let names = widget_names(&group);
            assert_eq!(count(&names, ids::UNSENT_CHANGE_ITEM), 2);
            assert_eq!(count(&names, ids::UNSENT_CHANGE_DESCRIPTION), 2);
            assert_eq!(count(&names, ids::UNSENT_CHANGE_STATUS), 2);
            assert_eq!(
                count(&names, ids::UNSENT_CHANGE_RETRY_BUTTON),
                1,
                "the not-sent row only"
            );
            assert_eq!(
                count(&names, ids::UNSENT_CHANGE_DISCARD_BUTTON),
                1,
                "the not-sent row only"
            );
            assert_eq!(count(&names, ids::UNSENT_CHANGE_DISCARD_CONFIRM_BUTTON), 0);

            let rows = ctx.state.borrow().rows.clone().unwrap();
            assert_eq!(
                unsent_status(&rows[0]),
                uc::status_with_reason(uc::STATE_UNSENT, &uc::reason_fallback("not_found"))
            );
            assert_eq!(unsent_status(&rows[1]), uc::STATE_HELD);
            assert!(rows.iter().all(|r| unsent_description(r) == uc::KIND_LIKE));

            // Arming the discard reveals the confirm on that row.
            ctx.state.borrow_mut().discard_armed = Some("aa".to_string());
            repaint(&ctx);
            assert_eq!(
                count(
                    &widget_names(&group),
                    ids::UNSENT_CHANGE_DISCARD_CONFIRM_BUTTON
                ),
                1
            );

            // The armed row leaves the list — nothing left to confirm.
            apply_read(&ctx, Ok(vec![change("bb", UnsentState::Held)]));
            assert!(ctx.state.borrow().discard_armed.is_none());
            assert_eq!(
                count(
                    &widget_names(&group),
                    ids::UNSENT_CHANGE_DISCARD_CONFIRM_BUTTON
                ),
                0
            );
        });
    }
}
