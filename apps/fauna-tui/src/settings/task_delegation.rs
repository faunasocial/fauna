//! The Settings → Task delegation sub-page — the cross-participant surface that
//! shows, per heavy background task kind, which participant currently runs it
//! and lets the user pin it (or leave it automatic).
//!
//! Authority: `docs/goal/behavior/participants.md` § Task delegation (Q-B/Q-C,
//! ratified 2026-07-06; its own rail sub-page ratified 2026-07-08 — the rail slot
//! is `docs/goal/ui/settings.md` § Navigation model, after Nests). ui.yaml page
//! `task-delegation`, reached `{"view":"settings","id":"task-delegation"}` (no
//! `-tab` — settings sub-pages don't use one). linux is the reference leg
//! (`apps/fauna-linux/src/settings/task_delegation.rs`); tui is the seventh and
//! last client to lift it (priority #1).
//!
//! **No policy here.** Per priority #2 this module is a dumb renderer of the
//! shared `fauna_client_delegation::TaskDelegationView`, which composes
//! `fauna.state.delegation` pins with the live `fauna.delegation.observe` lease
//! into per-kind `TaskDelegationRow`s. In particular the picker's option list is
//! a **correctness surface** the shared layer owns (`fauna_core::delegation::
//! PinOption`: a pin to a participant that can never run the kind would strand it
//! forever), so this page renders `row.pin_options` **verbatim** — it never
//! constructs, filters or reorders the option list.
//!
//! **What tui declares it runs is `index`, and nothing else** (per-kind since
//! 2026-08-03 — participants.md § The assignment picker). tui was blanket
//! `ViewerOnly` while it ran nothing at all, and the note here said "if tui ever
//! grows a runner loop, flipping this one constant is the whole change" — that
//! happened: tui resumes the content-index builder at login
//! (`conversations/conv_backend.rs`), making it a truthful `index` pin target
//! alongside linux. It still ships no `LeaseCoordinator` and no segment
//! upload driver (its sync story is the out-of-process agent —
//! `crate::sync_agent`), so it must never be offered for `backup-upload`:
//! that would let a user strand their own backups on a client that never runs
//! them, the precise failure `PinOption`'s doc exists to prevent.
//!
//! Participant **display names** come from the shared `DevicesMachine` roster
//! already built for the Devices sub-page (`device_id` hex → label): device names
//! are client-side state the shared view-model deliberately does not bake in
//! (`RunnerStatus::Other` carries a ref, not a name).

use fauna_ui_ids as ids;
use std::collections::HashMap;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_delegation::{
    HeavyTaskCapability, PinOption, TaskDelegationRow, TaskDelegationView,
};
use fauna_core::data::ParticipantRef;
use fauna_i18n::strings::task_delegation as t;

use super::{Action, SettingsState};
use crate::element::{Element, Gesture, SelectTarget};

/// The Task delegation sub-page's state — the composed rows plus the display-name
/// map they are rendered through.
#[derive(Debug, Clone, Default)]
pub(crate) struct TaskDelegationState {
    /// One row per `LIVE_TASK_KINDS` entry, as composed by the shared view-model.
    /// Empty pre-auth and until the first load resolves.
    pub(crate) rows: Vec<TaskDelegationRow>,
    /// `device_id` hex → the roster's label for it, for naming a runner or a
    /// pinned participant. Empty is fine: the shared `participant_name` falls
    /// back to the ref's own rendering.
    pub(crate) labels: HashMap<String, String>,
    /// Set when the visit could not even *dispatch* a load because this device
    /// cannot resolve its own id — rendered on `error-message` through
    /// [`super::page_error`], the `web.error` shape.
    ///
    /// **Why the error lives here and not on `App::errors`.** The load has two
    /// entry points and only one of them has an `App`: `Action::OpenTaskDelegation`
    /// does, but `route_subpage` takes `&mut SettingsState` alone — and *that* is
    /// the path the shared `TaskDelegationActions.navigate()` sends every app,
    /// so it is the one that cannot be skipped. A sub-page whose load can fail
    /// before an `Op` exists therefore records the failure in its own state and
    /// renders it through `page_error`, which is exactly the contract
    /// `App::page_snapshot_error` already carries. Self-healing by construction:
    /// every visit re-runs the load op, which sets or clears this, and the
    /// paint only reads it while `sub == TaskDelegation`.
    pub(crate) load_error: Option<String>,
}

/// The Task delegation sub-page's ordered element list.
///
/// The page's `error-message` is registered globally by
/// [`crate::ui::register_frame`] (the privacy/logs/muted-words precedent).
///
/// **Row scoping.** The shared action reads a row's leaves with a single-step
/// `scope="task-delegation-kind-item[i]"`, which the registry resolves wherever
/// that container sits in a leaf's ancestor path. Hence
/// `.within(ids::TASK_DELEGATION_KIND_ITEM, i)` on the three leaves and a FLAT
/// `task-delegation-list` / `task-delegation-kind-item` (the muted-words and
/// restore-history-item finding — ⚠ now historical: before the 2026-08-14
/// descendant ruling, nesting the rows under the list container would have
/// put the list first in every leaf's path and left every scoped read empty
/// while the page painted perfectly).
pub(super) fn task_delegation_elements(state: &SettingsState) -> Vec<Element> {
    let td = &state.task_delegation;
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        // The page landmark the driver waits on.
        Element::label(ids::TASK_DELEGATION, t::DESCRIPTION),
        // The row container. Flat — see the doc comment.
        Element::label(ids::TASK_DELEGATION_LIST, String::new()),
    ];
    for (i, row) in td.rows.iter().enumerate() {
        els.push(Element::label(
            ids::TASK_DELEGATION_KIND_ITEM,
            String::new(),
        ));
        els.push(
            Element::label(
                ids::TASK_DELEGATION_KIND_NAME,
                crate::wizard::localized(&row.name),
            )
            .within(ids::TASK_DELEGATION_KIND_ITEM, i),
        );
        els.push(
            Element::label(
                ids::TASK_DELEGATION_KIND_RUNNER,
                crate::wizard::localized(&fauna_core::delegation::runner_label(
                    &row.runner,
                    &td.labels,
                )),
            )
            .within(ids::TASK_DELEGATION_KIND_ITEM, i),
        );
        // A real `Role::Select`, never a cycle button: tui's agent answers "not
        // selectable" for a select on a non-select element, and the shared
        // action drives this by its option LABEL (the `folder-conflict-policy-select`
        // shape) — a cycle button would silently fail (testing.md point 11).
        // The options are `row.pin_options` rendered verbatim; the selected value
        // is the label of `row.assignment`, which is always one of them.
        let options: Vec<String> = row
            .pin_options
            .iter()
            .map(|o| option_text(o, &td.labels))
            .collect();
        els.push(
            Element::select(
                ids::TASK_DELEGATION_ASSIGNMENT_PICKER,
                option_text(&row.assignment, &td.labels),
                SelectTarget::TaskDelegationAssignment { row: i },
                options,
            )
            .within(ids::TASK_DELEGATION_KIND_ITEM, i),
        );
    }
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}

/// One picker option's display string — the shared label, localized.
pub(super) fn option_text(option: &PinOption, labels: &HashMap<String, String>) -> String {
    crate::wizard::localized(&fauna_core::delegation::option_label(option, labels))
}

/// Resolve a picker selection (which arrives as the option's **label**, the one
/// thing a `Role::Select` round-trips) back to the `PinOption` it names.
///
/// `None` when the label matches nothing in the row's option list — a select
/// against a stale render. Returning `None` is what lets the caller refuse
/// loudly instead of writing a pin the user never picked.
pub(super) fn option_for_label(
    state: &SettingsState,
    row: usize,
    label: &str,
) -> Option<(String, PinOption)> {
    let r = state.task_delegation.rows.get(row)?;
    let option = r
        .pin_options
        .iter()
        .find(|o| option_text(o, &state.task_delegation.labels) == label)?;
    Some((r.task_kind.clone(), option.clone()))
}

// ── Shared-call sequencing (the only logic here; everything else is shared) ──
//
// The async side takes only `Send` inputs (the `Arc<NestClient>` WS handle and
// this device's ref), the `muted_words.rs` shape.

/// Build the shared view-model. **tui declares `index` and nothing else** — see
/// the module docs.
fn view(nest: Arc<NestClient>, self_ref: ParticipantRef) -> TaskDelegationView<Arc<NestClient>> {
    TaskDelegationView::for_nest(
        nest,
        self_ref,
        HeavyTaskCapability::runner_for([fauna_core::delegation::KIND_INDEX]),
    )
}

/// This device's participant ref for account `actor_id_hex` — the same
/// `Device { device_id: hex }` encoding the lease loop heartbeats with. `None`
/// when the device-id store is unreadable (or no account is attached), which
/// degrades the page to "no self row", never a panic.
pub(super) fn self_participant_ref(actor_id_hex: &str) -> Option<ParticipantRef> {
    Some(ParticipantRef::Device {
        device_id: crate::media::device_id_hex(actor_id_hex)?,
    })
}

/// Read the surface: the composed rows.
///
/// The **pins** come off the replica's own account store (waited for when the
/// page is opened before the runtime is up); the live per-kind leases are a
/// nest read, and the row composition is the shared view's.
pub(super) async fn load_rows(
    store: &fauna_sync_engine::account_runtime::SeatAccountStore,
    nest: Arc<NestClient>,
    self_ref: ParticipantRef,
) -> Result<Vec<TaskDelegationRow>, String> {
    let view = view(nest, self_ref);
    fauna_sync_engine::preference_surfaces::load_task_delegation_rows(store, &view)
        .await
        .map_err(fauna_sync_engine::preference_surfaces::delegation_failure)
}

/// Persist one kind's assignment.
///
/// The plane's read-modify-write of the pins record, since the pins are
/// cross-device state a sibling device may be editing concurrently and a blind
/// save would drop its change. An unrunnable self-pin is refused by the shared
/// rule before anything is written.
pub(super) async fn write_pin(
    store: &fauna_sync_engine::account_runtime::SeatAccountStore,
    nest: Arc<NestClient>,
    self_ref: ParticipantRef,
    task_kind: String,
    option: PinOption,
) -> Result<(), String> {
    let view = view(nest, self_ref);
    fauna_sync_engine::preference_surfaces::set_task_assignment(store, &view, &task_kind, &option)
        .await
        .map_err(fauna_sync_engine::preference_surfaces::delegation_failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;
    use fauna_core::delegation::{RunnerStatus, delegation_rows};

    fn state_with_rows() -> SettingsState {
        let mut state = SettingsState {
            sub: SubPage::TaskDelegation,
            ..Default::default()
        };
        // The rows come from the SHARED composer, not hand-built — so the test
        // exercises whatever option set the shared correctness surface emits for
        // tui's real declared capability, rather than a fixture that could drift
        // from it.
        state.task_delegation.rows = delegation_rows(
            &ParticipantRef::Device {
                device_id: "aa".repeat(32),
            },
            &HeavyTaskCapability::runner_for([fauna_core::delegation::KIND_INDEX]),
            &Default::default(),
            &[],
            fauna_client_delegation::LEASE_STALE_MS,
        );
        state
    }

    fn ids(els: &[Element]) -> Vec<String> {
        els.iter().map(|e| e.id.clone()).collect()
    }

    /// Every static ui.yaml id paints, and one row per live task kind.
    #[test]
    fn the_page_paints_every_static_ui_yaml_id_and_one_row_per_kind() {
        let state = state_with_rows();
        let els = task_delegation_elements(&state);
        let ids = ids(&els);
        for id in [
            "page-heading",
            "task-delegation",
            "task-delegation-list",
            "settings-nav-back",
        ] {
            assert!(
                ids.contains(&id.to_string()),
                "missing {id:?}; have {ids:?}"
            );
        }
        let rows = state.task_delegation.rows.len();
        assert!(rows > 0, "the shared composer emits at least one live kind");
        for id in [
            "task-delegation-kind-item",
            "task-delegation-kind-name",
            "task-delegation-kind-runner",
            "task-delegation-assignment-picker",
        ] {
            assert_eq!(
                ids.iter().filter(|i| *i == id).count(),
                rows,
                "one {id} per kind"
            );
        }
    }

    /// An authenticated app parked on this page, carrying the shared composer's
    /// rows and a nest to dispatch against. Authenticated on purpose:
    /// `App::error_line_text` reports the *launch* surface's error while signed
    /// out, so a signed-out fixture cannot see a page error at all (the
    /// `settings::web` fixture's finding).
    fn authed_on_this_page() -> crate::app::App {
        let mut app = crate::app::tests::authed_app();
        app.page = crate::pages::Page::Settings;
        app.settings.sub = SubPage::TaskDelegation;
        app.settings.task_delegation = state_with_rows().task_delegation;
        // Both ops read the nest handle and the identity secret through `?`
        // before they reach the device-id guard, so a fixture missing either
        // would return `None` for the wrong reason and pass the refusal tests
        // vacuously. `secret_hex` lives on `SettingsState`, not on the session
        // `authed_app()` sets — any 64-hex secret decodes fine.
        app.settings.nest = Some(crate::settings::tests::nest_for_test());
        app.settings.secret_hex = "11".repeat(32).into();
        app
    }

    /// This device's ref, as the real `self_participant_ref` would build it.
    fn a_self_ref() -> ParticipantRef {
        ParticipantRef::Device {
            device_id: "bb".repeat(32),
        }
    }

    /// A label the CURRENT render really offers for row 0 — resolved through the
    /// same `option_text` the picker paints with, so the fixture cannot drift
    /// from the shared option set.
    fn a_live_label(state: &SettingsState) -> String {
        option_text(
            &state.task_delegation.rows[0].pin_options[0],
            &state.task_delegation.labels,
        )
    }

    /// **The load half of the device-id refusal.** An unreadable device-id store
    /// used to render the page with no rows AND no message — a state the screen
    /// stated nothing about (`ui/README.md` § Copy comprehensibility rule 6).
    ///
    /// Asserted through `App::error_line_text`, the one funnel the paint, the
    /// registry and `messages.error` all read — not the raw slot, which a driver
    /// cannot see.
    #[test]
    fn a_load_that_cannot_resolve_this_device_says_so_on_error_message() {
        let mut app = authed_on_this_page();
        let op = crate::settings::task_delegation_load_op_with(&mut app.settings, None);
        assert!(op.is_none(), "no load is dispatched without a self ref");
        assert_eq!(
            app.error_line_text().as_deref(),
            Some(t::ERROR_DEVICE_ID),
            "the refusal reaches the screen's one error funnel"
        );
    }

    /// The other direction: a device that CAN resolve its id dispatches the load
    /// and leaves the screen clean — so the assertion above pins the refusal, not
    /// merely a message that is always present.
    #[test]
    fn a_load_that_resolves_this_device_dispatches_and_says_nothing() {
        let mut app = authed_on_this_page();
        let op =
            crate::settings::task_delegation_load_op_with(&mut app.settings, Some(a_self_ref()));
        assert!(
            matches!(op, Some(crate::settings::Op::LoadTaskDelegation { .. })),
            "a resolvable device dispatches the load"
        );
        assert_eq!(app.error_line_text(), None, "and paints no error");
    }

    /// A refusal never outlives the condition that produced it: the next visit
    /// that CAN load clears it. `page_error` is read every frame, so a sticky
    /// slot would keep reporting a device-id failure over a working page.
    #[test]
    fn a_later_visit_that_can_load_clears_the_refusal() {
        let mut app = authed_on_this_page();
        crate::settings::task_delegation_load_op_with(&mut app.settings, None);
        assert!(app.error_line_text().is_some(), "armed");
        crate::settings::task_delegation_load_op_with(&mut app.settings, Some(a_self_ref()));
        assert_eq!(
            app.error_line_text(),
            None,
            "the working visit clears the previous refusal"
        );
    }

    /// **The mutation half**, same condition and same message: a pin change made
    /// while the device id is unreadable is refused loudly rather than eaten
    /// (`e2e-conventions.md` point 11 — a dropped command reads downstream as a
    /// product bug).
    #[test]
    fn a_pin_change_that_cannot_resolve_this_device_says_so_on_error_message() {
        let mut app = authed_on_this_page();
        let label = a_live_label(&app.settings);
        let op = crate::settings::task_delegation_set_op(&mut app, 0, &label, None);
        assert!(op.is_none(), "no write is dispatched without a self ref");
        assert_eq!(
            app.error_line_text().as_deref(),
            Some(t::ERROR_DEVICE_ID),
            "the refusal reaches the screen's one error funnel"
        );
    }

    /// The other direction for the mutation half: the same gesture with a
    /// resolvable device id writes the pin and reports nothing.
    #[test]
    fn a_pin_change_that_resolves_this_device_writes_and_says_nothing() {
        let mut app = authed_on_this_page();
        let label = a_live_label(&app.settings);
        let op = crate::settings::task_delegation_set_op(&mut app, 0, &label, Some(a_self_ref()));
        assert!(
            matches!(op, Some(crate::settings::Op::SetTaskAssignment { .. })),
            "a resolvable device writes the pin"
        );
        assert_eq!(app.error_line_text(), None, "and paints no error");
    }

    /// The load-bearing scoping contract: each row leaf's path **starts** with
    /// `task-delegation-kind-item[i]`, which is what makes the shared action's
    /// single-step scoped read resolve.
    #[test]
    fn row_leaves_are_scoped_within_their_own_indexed_row() {
        let els = task_delegation_elements(&state_with_rows());
        for id in [
            "task-delegation-kind-name",
            "task-delegation-kind-runner",
            "task-delegation-assignment-picker",
        ] {
            let leaf = els.iter().find(|e| e.id == id).unwrap();
            assert_eq!(
                leaf.path,
                vec![("task-delegation-kind-item".to_string(), 0)],
                "{id} must scope to its own row"
            );
        }
        assert!(
            els.iter()
                .filter(|e| e.id == "task-delegation-kind-item")
                .all(|e| e.path.is_empty()),
            "rows paint flat, not under task-delegation-list"
        );
    }

    /// **tui offers "This device" for `index` and for nothing else** — the
    /// per-kind capability, pinned at the only place a user could act on it: the
    /// rendered picker. tui resumes the content-index builder at login, so an
    /// `index` self-pin is honest; it ships no lease loop and no upload driver,
    /// so a `backup-upload` or `content-rescore` self-pin would strand the kind
    /// forever — the exact failure the shared option set exists to prevent.
    ///
    /// Asserting both directions is the point. "Never offers a self-pin" was
    /// true while tui ran nothing, and would still pass today if the picker
    /// silently dropped the option for every kind — a test that cannot tell
    /// "correctly withheld" from "wrongly withheld" is no pin on a per-kind rule.
    #[test]
    fn the_picker_offers_this_device_for_index_only() {
        let state = state_with_rows();
        for row in &state.task_delegation.rows {
            let offered = row.pin_options.contains(&PinOption::ThisDevice);
            assert_eq!(
                offered,
                row.task_kind == fauna_core::delegation::KIND_INDEX,
                "kind {:?} self-pin offered={offered}, but tui runs only the \
                 content-index builder",
                row.task_kind
            );
        }

        // ...and the rendered picker agrees with the row it was built from, so
        // the paint layer cannot add or drop the option behind the model's back.
        let els = task_delegation_elements(&state);
        let this_device = option_text(&PinOption::ThisDevice, &HashMap::new());
        let pickers: Vec<_> = els
            .iter()
            .filter(|e| e.id == "task-delegation-assignment-picker")
            .collect();
        assert_eq!(
            pickers.len(),
            state.task_delegation.rows.len(),
            "one picker per kind row"
        );
        for (picker, row) in pickers.iter().zip(&state.task_delegation.rows) {
            if let crate::element::Role::Select { options, .. } = &picker.role {
                assert_eq!(
                    options.contains(&this_device),
                    row.pin_options.contains(&PinOption::ThisDevice),
                    "picker for {:?} disagrees with its row about the self-pin",
                    row.task_kind
                );
            }
        }
    }

    /// The picker's painted value is the CURRENT assignment's label and is always
    /// one of its own options — so a driver reading the selection back always
    /// finds it, and the select never opens on a value it cannot round-trip.
    #[test]
    fn the_picker_value_is_always_one_of_its_options() {
        let els = task_delegation_elements(&state_with_rows());
        for el in els
            .iter()
            .filter(|e| e.id == "task-delegation-assignment-picker")
        {
            if let crate::element::Role::Select { options, .. } = &el.role {
                assert!(
                    options.contains(&el.text),
                    "selected {:?} is not among {options:?}",
                    el.text
                );
            }
        }
    }

    /// A selection resolves back to the `PinOption` its label names; an unknown
    /// label resolves to nothing, so a select against a stale render refuses
    /// rather than writing a pin the user never picked.
    #[test]
    fn a_selection_resolves_by_label_and_an_unknown_one_refuses() {
        let state = state_with_rows();
        let automatic = option_text(&PinOption::Automatic, &HashMap::new());
        let (kind, option) =
            option_for_label(&state, 0, &automatic).expect("Automatic is always offered");
        assert_eq!(kind, state.task_delegation.rows[0].task_kind);
        assert_eq!(option, PinOption::Automatic);

        assert!(option_for_label(&state, 0, "not an option").is_none());
        assert!(option_for_label(&state, 999, &automatic).is_none());
    }

    /// A runner label reads through the roster map, so another device shows its
    /// NAME rather than a raw id — the reason the page joins the roster at all.
    #[test]
    fn a_runner_on_another_device_is_named_from_the_roster() {
        let mut state = state_with_rows();
        let other = "bb".repeat(32);
        state.task_delegation.labels =
            HashMap::from([(other.clone(), "Anna's laptop".to_string())]);
        state.task_delegation.rows[0].runner = RunnerStatus::Other {
            who: ParticipantRef::Device {
                device_id: other.clone(),
            },
        };
        let els = task_delegation_elements(&state);
        let runner = els
            .iter()
            .find(|e| e.id == "task-delegation-kind-runner")
            .unwrap();
        assert!(
            runner.text.contains("Anna's laptop"),
            "runner label should name the device, got {:?}",
            runner.text
        );
        assert!(
            !runner.text.contains(&other),
            "the raw device id must not leak into the label: {:?}",
            runner.text
        );
    }
}
