//! The "Task delegation" Settings sub-page — the per-user surface that shows,
//! for each heavy background task kind, which device currently runs it and lets
//! the user pin it to a device (or leave it automatic).
//!
//! Authority: `docs/goal/behavior/participants.md` § Task delegation (Q-B/Q-C,
//! ratified 2026-07-06; placement RATIFIED 2026-07-08 as its own Settings rail
//! sub-page, `docs/goal/ui/settings.md` § Navigation model — after Nests). ui.yaml
//! page `task-delegation`, reached `{"view":"settings","id":"task-delegation"}`
//! (no `-tab` — like muted-words / nests / personalization). linux is the
//! reference leg; the other five apps lift this shape (priority #1).
//!
//! Per priority #2 this layer holds **no** delegation policy — it is dumb
//! rendering of the shared `fauna_client_delegation::TaskDelegationView`
//! view-model (which composes `fauna.state.delegation` pins + the live
//! `fauna.delegation.observe` lease into per-kind
//! [`TaskDelegationRow`]s) + dispatch of a pin write (`set_assignment`). The
//! option list a picker offers is a **correctness surface** the shared layer
//! guarantees (`fauna_core::delegation::PinOption` — a pin to a target that can
//! never run the kind would strand it forever), so this page renders
//! `row.pin_options` **verbatim** and never constructs or filters the option
//! list itself.
//!
//! **What linux declares it runs is `index`, and nothing else** (per-kind since
//! 2026-08-03 — participants.md § The assignment picker). It resumes the
//! content-index builder at login (`conversations/conv_backend.rs`), so it is a
//! truthful `index` pin target. It is **not** a `backup-upload` one: its in-app
//! upload driver was deleted 2026-07-29 and it hosts no `LeaseCoordinator` at
//! all. Until the capability became per-kind, linux passed a blanket `Runner`
//! and the picker offered a `backup-upload` self-pin that could only ever
//! wait — the exact stranding `PinOption`'s doc exists to prevent. Add a kind
//! here when linux grows a runner for it, never before.
//!
//! Participant **display names** (for a runner / pin that is another device) are
//! resolved from the shared `DevicesMachine` roster (`device_id` hex → label),
//! since device names are inherently client-side state the shared view-model
//! deliberately does not bake in (`fauna_core::delegation::RunnerStatus::Other`).
//! Like `settings/muted_words.rs`, the async side takes only `Send` inputs (the
//! `Arc<NestClient>` WS handle + this device's ref), so
//! the page's `Rc<FaunaClient>` never crosses the spawn boundary.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client::NestClient;
use fauna_client_delegation::{
    HeavyTaskCapability, PinOption, TaskDelegationRow, TaskDelegationView,
};
use fauna_core::data::ParticipantRef;
use fauna_devices_machine::{DevicesObserver, build_devices_machine};

use crate::async_helper::{hydrate_with_retry, spawn_with_snapshot};
use crate::i18n::strings::{self, task_delegation as S};
use crate::testid::set_test_id;

type Nest = Arc<NestClient>;
type Store = fauna_sync_engine::account_runtime::SeatAccountStore;

/// The rail label + `add_titled` title (settings_shell.rs). Kept on the shared
/// generated i18n const so the rail and page share one source of truth.
pub const TITLE: &str = S::TITLE;

/// One async round-trip's result: the composed per-kind rows (or an error string
/// surfaced in the page `error-message`) together with the `device_id`→label map
/// used to name a runner / pinned participant.
struct Loaded {
    rows: Result<Vec<TaskDelegationRow>, String>,
    labels: HashMap<String, String>,
}

/// No-op `DevicesMachine` observer: this page reads the roster once per reload
/// (for display names) rather than reacting to it, so it needs no reactivity.
struct NoopObserver;
impl DevicesObserver for NoopObserver {
    fn on_changed(&self) {}
}

/// Widget handles the render + event closures need.
struct Widgets {
    error_label: gtk::Label,
    /// `task-delegation-list` — container the indexed `task-delegation-kind-item`
    /// rows are rebuilt into on every load.
    list: gtk::Box,
    rows: RefCell<Vec<gtk::Box>>,
    /// What the list currently shows. A reload that answers the same rows
    /// paints nothing — the store-change notice is a level, so a re-read must
    /// not rebuild a row out from under an open picker.
    painted: RefCell<Option<PaintedList>>,
}

/// The rows and the per-kind picker state one paint shows, kept to compare the next load against.
type PaintedList = (Vec<TaskDelegationRow>, HashMap<String, String>);

/// Everything the handlers + render need. `Rc`-shared into closures. The
/// building blocks are all `Send` so the async side can reconstruct the shared
/// view-model without moving an `Rc`/`FaunaClient` across the spawn boundary.
struct Ctx {
    nest: Nest,
    /// This device's participant ref (`Device { device_id: hex(device_id) }` —
    /// the same encoding the lease loop heartbeats with, `backup.rs`).
    self_ref: ParticipantRef,
    rt: tokio::runtime::Handle,
    w: Widgets,
}

/// Build the "Task delegation" preferences page. Constructs every static ui.yaml
/// ID even when no client is registered (the clientless unit test); only the
/// async wiring is gated on a live client.
pub fn build_task_delegation_page() -> (adw::PreferencesPage, Rc<dyn Fn()>) {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("preferences-system-time-symbolic")
        .build();
    // task-delegation — the page landmark view (mirrors muted-words / nests).
    set_test_id(&page, ids::TASK_DELEGATION);

    // --- Top group: heading + description + page-level error ---
    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();
    // page-heading — a marker so AT-SPI resolves the global heading element.
    top_group.set_header_suffix(Some(&super::marker("page-heading")));

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    top_group.add(&error_label);
    page.add(&top_group);

    // --- List group: the container of the per-kind rows ---
    let list_group = adw::PreferencesGroup::builder().build();
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&list, ids::TASK_DELEGATION_LIST);
    list_group.add(&list);
    page.add(&list_group);

    let widgets = Widgets {
        error_label,
        list,
        rows: RefCell::new(Vec::new()),
        painted: RefCell::new(None),
    };
    // Re-load every time the page is shown. Two reasons, both load-bearing:
    // (1) the runner column is **live** advisory-lease state that changes outside
    // this app (a peer claims the lease, a laptop unplugs and yields), so a
    // build-time hydrate would show app-start data forever; and (2) the settings
    // shell builds every sub-page once at app init, where the post-login WS
    // socket may still be coming up — `hydrate_with_retry`'s ~5 s budget loses
    // that race on a loaded machine and the page would then stay **permanently
    // empty** until an app restart. Mirrors `settings/mail_spam.rs`'s
    // refresh-on-visible (the same shell gap; tracked internally).
    // The same reload is what the store-change notice re-drives while the page
    // is open (`crate::store_surfaces`); a page left static re-drives nothing.
    let refresh: Rc<dyn Fn()> = match wire(widgets) {
        Some(refresh) => Rc::new(refresh),
        None => Rc::new(|| {}),
    };
    {
        let refresh = Rc::clone(&refresh);
        page.connect_map(move |_| refresh());
    }
    (page, refresh)
}

/// Wire the page to the shared view-model, hydrate once, and return a closure
/// that re-loads it. `None` (page stays at its empty list) when no client is
/// registered — e.g. the unit test — or when this device's id can't be resolved
/// (near-impossible post-login; the page surfaces `error_device_id` on
/// `error-message` rather than rendering blank — `docs/goal/ui/README.md`
/// § Copy comprehensibility rule 6).
fn wire(widgets: Widgets) -> Option<impl Fn()> {
    let client = crate::settings::get_client()?;
    let device_id = match crate::sync::device_id() {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                target: "fauna_linux",
                "task-delegation: device id unavailable ({e}); leaving page static"
            );
            super::render_error_label(&widgets.error_label, Some(S::ERROR_DEVICE_ID));
            return None;
        }
    };

    let ctx = Rc::new(Ctx {
        nest: client.nest_rpc().clone(),
        self_ref: ParticipantRef::Device {
            device_id: fauna_core::hex32::encode(&device_id),
        },
        rt: client.runtime_handle(),
        w: widgets,
    });

    reload(&ctx);
    Some(move || reload(&ctx))
}

/// Load the surface (retrying while the WS socket comes up post-login — the
/// embedded page can mount before it's ready), then render on the GTK thread.
fn reload(ctx: &Rc<Ctx>) {
    let store = crate::account_runtime::handle_source();
    let nest = ctx.nest.clone();
    let self_ref = ctx.self_ref.clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { load_all(store, nest, self_ref).await },
        move |loaded| render_page(&ctx_render, loaded),
    );
}

/// Persist a pin change for `task_kind`, then reload so the runner column + the
/// picker reflect authoritative state. On failure the error shows and the page
/// is left as-is (the write is the plane's read-modify-write — a failure
/// changed nothing).
fn write_pin(ctx: &Rc<Ctx>, task_kind: String, option: PinOption) {
    let store = crate::account_runtime::handle_source();
    let nest = ctx.nest.clone();
    let self_ref = ctx.self_ref.clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { do_set_pin(store, nest, self_ref, task_kind, option).await },
        move |result: Result<(), String>| match result {
            Ok(()) => reload(&ctx_render),
            Err(msg) => super::render_error_label(&ctx_render.w.error_label, Some(&msg)),
        },
    );
}

/// Render a loaded surface (GTK main thread). On success the rows re-render from
/// the composed view-model; on error the message shows and the existing rows are
/// left untouched (mirrors `settings/muted_words.rs`).
fn render_page(ctx: &Rc<Ctx>, loaded: Loaded) {
    let w = &ctx.w;
    match &loaded.rows {
        Ok(rows) => {
            super::render_error_label(&w.error_label, None);

            let answer = Some((rows.clone(), loaded.labels.clone()));
            if *w.painted.borrow() == answer {
                return;
            }
            *w.painted.borrow_mut() = answer;

            let mut existing = w.rows.borrow_mut();
            for row in existing.drain(..) {
                w.list.remove(&row);
            }
            for row in rows {
                let item = build_kind_row(ctx, row, &loaded.labels);
                w.list.append(&item);
                existing.push(item);
            }
        }
        Err(msg) => super::render_error_label(&w.error_label, Some(msg)),
    }
}

/// Build one `task-delegation-kind-item` row: the kind's display name, the
/// current runner/status, and the assignment picker. Every row carries the
/// **bare** id `task-delegation-kind-item` (rows are addressed positionally in
/// `LIVE_TASK_KINDS` order); a plain Box defaults to the AT-SPI role Generic,
/// which the Linux bridge can omit from the tree, so the row is given an explicit
/// Group role (same idiom as `nests-item` / `device-card` / `folder-member-item`).
fn build_kind_row(
    ctx: &Rc<Ctx>,
    row: &TaskDelegationRow,
    labels: &HashMap<String, String>,
) -> gtk::Box {
    let item = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&item, ids::TASK_DELEGATION_KIND_ITEM);
    item.add_css_class("card");
    item.set_margin_top(4);
    item.set_margin_bottom(4);

    // task-delegation-kind-name — the kind's localized display name.
    let name = gtk::Label::new(Some(&row.name.resolve(strings::lookup)));
    name.set_halign(gtk::Align::Start);
    name.set_xalign(0.0);
    name.add_css_class("heading");
    set_test_id(&name, ids::TASK_DELEGATION_KIND_NAME);
    item.append(&name);

    // task-delegation-kind-runner — who currently runs the kind (this device /
    // another participant / waiting).
    let runner = gtk::Label::new(Some(
        &fauna_core::delegation::runner_label(&row.runner, labels).resolve(strings::lookup),
    ));
    runner.set_halign(gtk::Align::Start);
    runner.set_xalign(0.0);
    runner.add_css_class("caption");
    runner.add_css_class("dim-label");
    set_test_id(&runner, ids::TASK_DELEGATION_KIND_RUNNER);
    item.append(&runner);

    // task-delegation-assignment-picker — Automatic / pin to a participant. The
    // model strings are `row.pin_options` rendered verbatim (the shared layer
    // guarantees the option set is legal); the selected index is the position of
    // `row.assignment` (always an element of `pin_options`). Mirrors the
    // `folder-conflict-policy-select` dropdown: e2e reads back the selected option's
    // label string (`automation/find.rs`).
    let option_labels: Vec<String> = row
        .pin_options
        .iter()
        .map(|o| fauna_core::delegation::option_label(o, labels).resolve(strings::lookup))
        .collect();
    let option_refs: Vec<&str> = option_labels.iter().map(String::as_str).collect();
    let picker = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&option_refs))
        .build();
    set_test_id(&picker, ids::TASK_DELEGATION_ASSIGNMENT_PICKER);
    crate::offline_gate::declare_wire_kind(&picker, "fauna.account.state.put");
    // Pre-select the current assignment *before* connecting the handler, so the
    // initial programmatic selection (and every re-render) does not re-fire the
    // change → no spurious pin write / reload loop.
    let selected = row
        .pin_options
        .iter()
        .position(|o| o == &row.assignment)
        .unwrap_or(0);
    picker.set_selected(selected as u32);
    {
        let ctx = Rc::clone(ctx);
        let task_kind = row.task_kind.clone();
        let options = row.pin_options.clone();
        picker.connect_selected_notify(move |dd| {
            let Some(option) = options.get(dd.selected() as usize) else {
                return;
            };
            write_pin(&ctx, task_kind.clone(), option.clone());
        });
    }
    item.append(&picker);

    item
}

// ── Shared-call sequencing (the only logic here; everything else is shared) ──
//
// The tokio-runtime side takes only `Send` inputs (the `Arc<NestClient>` WS
// handle and this device's ref), so the page's
// `Rc<FaunaClient>` never crosses the spawn boundary (mirrors
// `settings/muted_words.rs`).

/// Build the shared view-model from `Send` inputs. linux declares the kinds in
/// the module header: `index` only.
fn view(nest: Nest, self_ref: ParticipantRef) -> TaskDelegationView<Nest> {
    TaskDelegationView::for_nest(
        nest,
        self_ref,
        HeavyTaskCapability::runner_for([fauna_core::delegation::KIND_INDEX]),
    )
}

/// Read the surface (rows + device-name map) for a full render.
///
/// The pins come off the replica's own account store
/// (`crate::account_runtime::handle_source()`, waited for when the page opens
/// before the assembly lands); the live per-kind leases are a nest read, and
/// the row composition is the shared view's — all of it the shared
/// `preference_surfaces::load_task_delegation_rows`, not a hand-copy of tui's
/// `settings/task_delegation.rs`.
async fn load_all(store: Store, nest: Nest, self_ref: ParticipantRef) -> Loaded {
    let vm = view(nest.clone(), self_ref);
    // Kept: composing the pins (an account-store read) with a
    // `fauna.delegation.observe` RPC for the live lease — not a single
    // NestClient RPC (transport.md § Request lifecycle step 3's note).
    let rows = hydrate_with_retry(|| async {
        fauna_sync_engine::preference_surfaces::load_task_delegation_rows(&store, &vm)
            .await
            .map_err(fauna_sync_engine::preference_surfaces::delegation_failure)
    })
    .await;
    Loaded {
        rows,
        labels: device_labels(nest).await,
    }
}

/// Persist a single pin change: the plane's read-modify-write of the pins
/// record, since the pins are cross-device state a sibling device may be
/// editing concurrently and a blind save would drop its change. An unrunnable
/// self-pin is refused by the shared rule before anything is written.
async fn do_set_pin(
    store: Store,
    nest: Nest,
    self_ref: ParticipantRef,
    task_kind: String,
    option: PinOption,
) -> Result<(), String> {
    let vm = view(nest, self_ref);
    fauna_sync_engine::preference_surfaces::set_task_assignment(&store, &vm, &task_kind, &option)
        .await
        .map_err(fauna_sync_engine::preference_surfaces::delegation_failure)
}

/// The `device_id` hex → label map from the shared `DevicesMachine` roster, for
/// naming a runner / pinned device. Any read failure yields an empty map — the
/// caller then falls back to a short-hex abbreviation rather than showing
/// nothing.
async fn device_labels(nest: Nest) -> HashMap<String, String> {
    let devices = build_devices_machine(
        nest,
        Some(crate::account_runtime::folder_key_store()),
        Arc::new(NoopObserver),
    );
    devices.refresh().await;
    devices
        .snapshot()
        .devices
        .into_iter()
        .map(|d| (d.device_id, d.label))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// The page exposes every static (page-level) ui.yaml ID with no registered
    /// client. The per-row IDs (`task-delegation-kind-item` + children) render
    /// only after an async load, so they're covered by the cross-app e2e, not
    /// here (mirrors `settings/muted_words.rs`'s static-ID test).
    #[test]
    fn task_delegation_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            // No registered client → the page stays at its empty list, but it must
            // still build with every static ui.yaml ID present.
            let (page, _refresh) = build_task_delegation_page();
            let names = widget_names(&page);
            for id in [
                "task-delegation",
                "page-heading",
                "error-message",
                "task-delegation-list",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }
}
