//! Per-folder local-folder binding (desktop), **nested under each folder**.
//!
//! Lifted from the former `settings/sync_tab.rs` as part of the 2026-06-28
//! sync/folder UI unification (spec § 3, O-1). A folder is created **once**
//! via the wizard; binding a local folder on *this device* is an action *within*
//! an existing set's row, so the set is **contextual** — there is **no free-text
//! folder-name field** (`folder-location-fileset-input` removed) and **no per-row
//! set-name display** (`folder-location-fileset` removed). The binding lives under
//! each `folder-row` expander on the **Settings → Folders** sub-page.
//!
//! The folder↔folder map persisted here is device-local config (the in-process
//! engine's `watch_dir` equivalent — `apps/linux.md` § File Sync), not a
//! control-plane concern; place flags / mode / roster are the nest-authoritative
//! per-set controls that live elsewhere on the same row. The bound-folder list
//! carries the cross-app `folder-location-*` test IDs (ui.yaml § folders
//! `platform_elements: linux`), including the per-binding
//! `folder-location-mode-toggle`: the switch between always-resident and the
//! agent's FUSE on-demand root (`on-demand-files.md` § Linux FUSE binding).
//!
//! The add flow is the cross-app typed-path shape: `folder-location-path-input` is
//! the drivable source of truth, and the native `gtk::FileDialog` folder picker is
//! an optional `folder-location-browse-button` that merely *fills* it (the picker
//! itself the drivers can't drive). The list-render / remove e2e path seeds folders
//! via the `sync_inject_locations` test-agent command (`inject_locations_for_test`).

use adw::prelude::*;
use fauna_ui_ids as ids;
use gtk::gio;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::app::{ActionResult, UiMessage};
use crate::client::FaunaClient;
use crate::i18n::strings;
use crate::i18n::strings::settings::sync_page as sp;
use crate::sync::LocationBinding;
use crate::testid::set_test_id;

/// The live [`RERENDER`] callback: replaces the device-local folder map and
/// repaints the nested binding rows.
type RerenderCb = Rc<dyn Fn(Vec<LocationBinding>)>;

thread_local! {
    /// Folders injected by the `sync_inject_locations` e2e command *before* the
    /// Folders sub-page is built (the lazy-build fallback). The page builder
    /// seeds the device-local map from here when present, else from the persisted
    /// map. `None` in production. The live path is [`RERENDER`].
    static INJECTED_LOCATIONS: RefCell<Option<Vec<LocationBinding>>> = const { RefCell::new(None) };

    /// Re-render callback registered by the live Folders sub-page: replaces the
    /// device-local folder map with an injected vec and re-renders the folder
    /// list (so each set's nested binding rows repaint). Invoked by
    /// `inject_locations_for_test` so the cross-app `test_sync_folders.py`
    /// drives the render/dispatch surface without the native folder picker. `None`
    /// until the page is built.
    static RERENDER: RefCell<Option<RerenderCb>> = const { RefCell::new(None) };
}

/// Test-only entry point for the `sync_inject_locations` e2e command: replace the
/// device-local folder map with `mappings` and re-render the folder list so the
/// nested binding rows repaint. No-op on the live list if the page isn't built
/// yet — the mappings are stashed in [`INJECTED_LOCATIONS`] so the next page build
/// seeds from them. Compiled only where its one caller is — `main.rs`'s
/// `handle_sync_inject_locations`, under the same gate; a release build keeps
/// `INJECTED_LOCATIONS` at `None` for good.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn inject_locations_for_test(mappings: Vec<LocationBinding>) {
    INJECTED_LOCATIONS.with(|s| *s.borrow_mut() = Some(mappings.clone()));
    RERENDER.with(|s| {
        if let Some(cb) = s.borrow().as_ref() {
            cb(mappings);
        }
    });
}

/// Register the live re-render callback (see `inject_locations_for_test`). The
/// Folders page builder calls this once with a closure that swaps the shared
/// map's contents and re-renders the folder list. Latest registration wins, so
/// a shell rebuild (re-login) re-points it at the fresh widgets.
pub fn register_rerender(cb: Rc<dyn Fn(Vec<LocationBinding>)>) {
    RERENDER.with(|s| *s.borrow_mut() = Some(cb));
}

/// Seed the device-local folder map for a fresh page build: the e2e-injected
/// vec if one was stashed before the page existed, else the live agent-backed
/// binding model ([`crate::sync_agent::current_locations`], empty pre-install).
pub fn initial_location_map() -> Vec<LocationBinding> {
    INJECTED_LOCATIONS
        .with(|s| s.borrow().clone())
        .unwrap_or_else(crate::sync_agent::current_locations)
}

/// Repaint the nested binding rows from the agent-backed model (the reconcile
/// path — [`crate::sync_agent::reconcile`]). No-op while an e2e injection is
/// active: an injected render fixture must not be overwritten by a background
/// reconcile mid-test, and no-op before the page registers its callback.
pub fn rerender_bindings(mappings: Vec<LocationBinding>) {
    let injected = INJECTED_LOCATIONS.with(|s| s.borrow().is_some());
    if injected {
        return;
    }
    RERENDER.with(|s| {
        if let Some(cb) = s.borrow().as_ref() {
            cb(mappings);
        }
    });
}

/// A set hosted on **another** nest, as the bind gesture needs to see it: where
/// the set lives and which channel addresses it there.
/// Present only for a cross-nest row — `None` is an own-nest set.
#[derive(Clone)]
pub struct ForeignBind {
    /// The set's home-nest base URL, from the member's own `fauna.state.folder-keys` record.
    pub home_nest_url: String,
    /// The set's derived `ChannelId` (hex) — how the federated kinds address it
    /// (its *name* only resolves on its home nest).
    pub channel_id_hex: String,
}

/// Build the nested folder-binding sub-section for folder `set_name`: the
/// device-local folders bound to *this* set (each `folder-location-row` → its
/// `folder-location-path` + a remove button), followed by the typed-path add form
/// (`folder-location-path-input` + optional `folder-location-browse-button` +
/// `folder-location-add-button`). The add binds the typed path to `set_name` (the
/// enclosing set — no free-text name). `location_map` is the shared device-local
/// render map; mutations route to the external sync agent
/// (`crate::sync_agent` — optimistic push + union reconcile).
///
/// **`foreign` flips the add from optimistic to verified** (D3). An own-nest
/// bind stays optimistic: the `access` the row rendered from was projected by
/// the very nest that will judge the write, so it is fresh by construction. A
/// **cross-nest** grant is not — it reaches the client as advisory data
/// refreshed on a poll — so the add first asks the set's home nest for a write
/// token and binds only if that succeeds. Nothing is rendered or pushed to the
/// agent until it does: a stale-writer row is survivable, a stale-writer
/// *binding* is a folder the user believes is syncing whose every edit is
/// refused.
pub fn build_location_binding(
    set_name: &str,
    folder_id: Option<String>,
    location_map: &Rc<RefCell<Vec<LocationBinding>>>,
    fauna_client: &Rc<FaunaClient>,
    foreign: Option<ForeignBind>,
) -> gtk::Box {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();

    let caption = gtk::Label::builder()
        .label(sp::SYNCED_LOCATIONS)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    container.append(&caption);

    let location_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    set_test_id(&location_list, ids::FOLDER_LOCATION_LIST);
    let placeholder = adw::StatusPage::builder()
        .title(sp::NO_LOCATIONS_SYNCED)
        .icon_name("folder-symbolic")
        .build();
    location_list.set_placeholder(Some(&placeholder));

    for mapping in location_map.borrow().iter() {
        if mapping.folder == set_name {
            location_list.append(&build_location_row(mapping, location_map, &location_list));
        }
    }
    container.append(&location_list);

    // --- Add-folder form (typed path + optional browse) — binds to THIS set. ---
    let path_row = adw::EntryRow::builder().title(sp::LOCATION_PATH).build();
    set_test_id(&path_row, ids::FOLDER_LOCATION_PATH_INPUT);
    let browse_btn = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&browse_btn, ids::FOLDER_LOCATION_BROWSE_BUTTON);
    path_row.add_suffix(&browse_btn);

    let add_row = adw::ActionRow::builder()
        .title(sp::ADD_LOCATION)
        .subtitle(sp::ADD_LOCATION_SUBTITLE)
        .activatable(true)
        .build();
    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&add_btn, ids::FOLDER_LOCATION_ADD_BUTTON);
    // A CROSS-NEST bind is the one leg of this form that leaves the box: it asks
    // the set's home nest for a write token first and binds nothing on refusal
    // (`FaunaClient::verify_foreign_write_access`), so offline it can only fail
    // closed. Own-nest binds are pure `sync_agent` puts and declare nothing —
    // hence the condition rather than a blanket declaration. `foreign` is fixed
    // for this build, so one declaration settles it (no rule-4 re-paint).
    // ⚠ `fauna.folders.write_token.get` is classed `Read` today, so the gate
    // leaves the button live; declared anyway on the reveal-read precedent, so a
    // later reclassification reaches this control for free. Whoever revisits the
    // class: the *kind* really is a read — it is this bind CEREMONY that is
    // fail-closed, which is an argument about the ceremony, not the kind.
    if foreign.is_some() {
        crate::offline_gate::declare_wire_kind(&add_btn, "fauna.folders.write_token.get");
    }
    add_row.add_suffix(&add_btn);

    let add_group = adw::PreferencesGroup::new();
    add_group.add(&path_row);
    add_group.add(&add_row);
    container.append(&add_group);

    // Browse → fill the typed path entry (optional convenience; the picker is not
    // e2e-driveable). Disable while open so a second click can't spawn a second picker.
    {
        let path_row = path_row.clone();
        browse_btn.connect_clicked(move |btn| {
            let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let dialog = gtk::FileDialog::builder()
                .title(sp::SELECT_DIRECTORY_DIALOG)
                .build();
            let path_row = path_row.clone();
            let browse_btn = btn.clone();
            browse_btn.set_sensitive(false);
            dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |result| {
                browse_btn.set_sensitive(true);
                if let Ok(file) = result
                    && let Some(path) = file.path()
                {
                    path_row.set_text(&path.display().to_string());
                }
            });
        });
    }

    // Add → commit the typed path bound to THIS set (no free-text name). No native
    // picker, so the flow is e2e-driveable (`test_location_add_via_typed_path`).
    {
        let location_map = Rc::clone(location_map);
        let location_list = location_list.clone();
        let path_row = path_row.clone();
        let set_name = set_name.to_string();
        let folder_id = folder_id.clone();
        let fauna_client = Rc::clone(fauna_client);
        let foreign = foreign.clone();
        add_btn.connect_clicked(move |_| {
            let path_text = path_row.text().trim().to_string();
            if path_text.is_empty() {
                path_row.add_css_class("error");
                return;
            }
            path_row.remove_css_class("error");

            // Fail closed: a binding is keyed by the set's ref alone. A row
            // `folder_ref_for_row` yields no ref for is refused on the page's
            // `error-message`, never bound by a name two sets can share.
            let Some(folder_ref) = folder_id.clone() else {
                path_row.add_css_class("error");
                fauna_client
                    .tx()
                    .send(UiMessage::Action(ActionResult::FailedLocalized {
                        message: strings::devices::error_bind_location(
                            "the folder's identity could not be resolved",
                        ),
                    }));
                return;
            };
            let mapping = LocationBinding {
                path: PathBuf::from(&path_text),
                folder: set_name.clone(),
                folder_id: folder_ref,
            };

            let Some(foreign) = foreign.clone() else {
                commit_binding(mapping, &location_map, &location_list, &path_row);
                return;
            };

            // Cross-nest (D3): ask the set's HOME nest before binding anything.
            // Nothing is rendered or pushed until it agrees, so a refusal leaves
            // the UI exactly as it was — no folder appears to be syncing that is
            // not. The button is held insensitive for the round trip so a second
            // click cannot queue a duplicate bind.
            add_btn_set_busy(&path_row, true);
            let location_map = Rc::clone(&location_map);
            let location_list = location_list.clone();
            let path_row = path_row.clone();
            let tx = fauna_client.tx();
            fauna_client.verify_foreign_write_access(
                foreign.home_nest_url,
                foreign.channel_id_hex,
                move |result| {
                    add_btn_set_busy(&path_row, false);
                    match result {
                        Ok(()) => commit_binding(mapping, &location_map, &location_list, &path_row),
                        Err(error) => {
                            // The set's own nest refused — the rendered grant was
                            // stale, or was never a writer grant. Surface it and
                            // bind nothing.
                            path_row.add_css_class("error");
                            tx.send(UiMessage::Action(ActionResult::FailedLocalized {
                                message: strings::devices::error_bind_location(&error),
                            }));
                        }
                    }
                },
            );
        });
    }

    container
}

/// Render the new mapping and push it to the external sync agent. The optimistic
/// half of the add: a failed push keeps the row rendered and re-pushes on the
/// next reconcile (union semantics, `sync_agent.rs`).
fn commit_binding(
    mapping: LocationBinding,
    location_map: &Rc<RefCell<Vec<LocationBinding>>>,
    location_list: &gtk::ListBox,
    path_row: &adw::EntryRow,
) {
    // The model first: the row reads its mode switch off it
    // (`sync_agent::mode_toggle_for`).
    crate::sync_agent::add_binding(mapping.clone());
    location_list.append(&build_location_row(&mapping, location_map, location_list));
    location_map.borrow_mut().push(mapping);
    path_row.set_text("");
    path_row.remove_css_class("error");
}

/// Hold the add form inert while a cross-nest bind verify is in flight, so a
/// second click cannot queue a duplicate bind behind the first.
fn add_btn_set_busy(path_row: &adw::EntryRow, busy: bool) {
    path_row.set_sensitive(!busy);
}

/// One folder-binding row (`<path>` + remove) carrying the cross-app
/// `folder-location-*` IDs. No `folder-location-fileset` display — the enclosing
/// `folder-row` expander already names the set (O-1, nested). Clicking the path
/// opens the folder in the system file manager. A named `gtk::Box` (role `Group`,
/// like `device-card`) so the path is a separately addressable label
/// (`folder-location-path`).
fn build_location_row(
    mapping: &LocationBinding,
    location_map: &Rc<RefCell<Vec<LocationBinding>>>,
    list: &gtk::ListBox,
) -> gtk::Box {
    // The row is a VERTICAL box: the path + remove line, and — only while the
    // mass-delete floor is holding — the hold line and its apply verb beneath.
    // The test ids the shared suite scopes by (`folder-location-path`,
    // `folder-location-remove-button`, …) resolve as descendants, so nesting the
    // first line inside costs nothing.
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::FOLDER_LOCATION_ROW);

    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();

    let path_label = gtk::Label::builder()
        .label(mapping.path.display().to_string())
        .halign(gtk::Align::Start)
        .hexpand(true)
        .css_classes(["heading"])
        .build();
    set_test_id(&path_label, ids::FOLDER_LOCATION_PATH);

    // Clicking the path opens the folder in the system file manager
    // (`gtk::FileLauncher`). The trash button is a separate child of the row, so
    // removing a mapping never triggers an open.
    let open_path = mapping.path.clone();
    let open_gesture = gtk::GestureClick::new();
    open_gesture.connect_released(move |_, _, _, _| {
        let file = gio::File::for_path(&open_path);
        gtk::FileLauncher::new(Some(&file)).launch(
            gtk::Window::NONE,
            gio::Cancellable::NONE,
            |_| {},
        );
    });
    path_label.add_controller(open_gesture);
    path_label.set_cursor_from_name(Some("pointer"));
    path_label.set_tooltip_text(Some(sp::OPEN_DIRECTORY));

    let remove_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&remove_btn, ids::FOLDER_LOCATION_REMOVE_BUTTON);

    top.append(&path_label);
    top.append(&remove_btn);
    row.append(&top);

    // The per-binding on-demand switch (`on-demand-files.md` § On-Demand Files →
    // *The choice is the user's*; § Linux FUSE binding). What it shows is the
    // shared rule's answer (`LocationBindingsModel::mode_toggle`): the agent's
    // mode for this binding, insensitive with the host's reason where the agent
    // cannot mount (no `fuse3`), and this location's own line when its mount
    // was refused. The uniform `state` attr carries `always|on-demand`.
    if let Some(toggle) = crate::sync_agent::mode_toggle_for(&mapping.path) {
        let mode_line = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        let label = strings::devices::sync_locations::ON_DEMAND_LABEL;
        mode_line.append(
            &gtk::Label::builder()
                .label(label)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .build(),
        );
        let switch = gtk::Switch::builder()
            .active(toggle.on_demand)
            .sensitive(toggle.enabled)
            .valign(gtk::Align::Center)
            .build();
        switch.update_property(&[gtk::accessible::Property::Label(label)]);
        set_test_id(&switch, ids::FOLDER_LOCATION_MODE_TOGGLE);
        crate::testid::set_test_attr(
            &switch,
            "state",
            if toggle.on_demand {
                "on-demand"
            } else {
                "always"
            },
        );
        mode_line.append(&switch);
        row.append(&mode_line);

        if let Some(line) = toggle
            .notice
            .and_then(|notice| strings::lookup(notice.i18n_key()))
        {
            switch.set_tooltip_text(Some(line));
            row.append(
                &gtk::Label::builder()
                    .css_classes(["dim-label", "caption"])
                    .label(line)
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
        }

        // Not optimistic: the flip goes to the agent and the row is rebuilt
        // from its answer, so the switch never claims a mode the agent refused.
        let mode_path = mapping.path.display().to_string();
        switch.connect_active_notify(move |switch| {
            crate::sync_agent::set_location_mode(mode_path.clone(), switch.is_active());
        });
    }

    // The mass-delete floor's confirm affordance (`delete-propagation.md` § A
    // wholesale-vanished folder is infrastructure failure). Every tracked file
    // in this folder vanished at once — an unmounted volume, a folder moved
    // away — so the engine recorded NOTHING and the nest still holds the set.
    // The line says that before the button offers to change it, and reconnecting
    // the folder (remove + re-add above) is the non-destructive answer.
    //
    // Rendered only while the hold stands: `0` is the reading that retracts it,
    // and a zeroed line would leave an offer to destroy files standing over a
    // folder that is perfectly healthy.
    let held = crate::sync_agent::deletes_held_for(&mapping.folder);
    if held > 0 {
        let count = held.to_string();
        let held_label = gtk::Label::builder()
            .css_classes(["warning", "caption"])
            .label(strings::folders::deletes_held(&count))
            .wrap(true)
            .xalign(0.0)
            .build();
        set_test_id(&held_label, ids::FOLDER_LOCATION_DELETES_HELD);
        row.append(&held_label);

        let apply_btn = gtk::Button::builder()
            .label(strings::folders::apply_deletes(&count))
            .halign(gtk::Align::Start)
            .css_classes(["destructive-action"])
            .build();
        set_test_id(&apply_btn, ids::FOLDER_LOCATION_APPLY_DELETES_BUTTON);
        let apply_folder = mapping.folder.clone();
        apply_btn.connect_clicked(move |_| {
            // The SET, never the count above: the agent re-derives what is
            // actually missing at click time, so a confirm racing a remount
            // deletes nothing.
            crate::sync_agent::apply_held_deletes(apply_folder.clone());
        });
        row.append(&apply_btn);
    }

    let path: PathBuf = mapping.path.clone();
    let folder = mapping.folder.clone();
    let location_map = Rc::clone(location_map);
    let list = list.clone();
    remove_btn.connect_clicked(move |btn| {
        // The card is a `gtk::Box`, so `ListBox::append` wrapped it in an
        // auto-created `GtkListBoxRow`. Remove that wrapper (a direct child of the
        // list) — removing the inner box does NOT drop the row here.
        if let Some(row) = btn.ancestor(gtk::ListBoxRow::static_type()) {
            list.remove(&row);
        }
        location_map.borrow_mut().retain(|m| m.path != path);
        // Apply live — optimistic unbind pushed to the external sync agent
        // (re-pushed on reconcile until the agent drops it; `sync_agent.rs`).
        crate::sync_agent::remove_binding(&folder);
    });

    row
}
