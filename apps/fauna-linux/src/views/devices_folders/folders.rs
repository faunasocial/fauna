use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use fauna_client_folders::engine_binding::folder_ref_for_row;
use fauna_client_folders::folders::{
    self as folders_wire, FolderActorMember, FolderMember, PlaceRow,
};
use fauna_devices_machine::{DevicesMachine, FolderSummary};

use super::location_binding;
use crate::async_helper;
use crate::client::FaunaClient;
use crate::i18n::strings;
use crate::sync::LocationBinding;
use crate::testid::set_test_id;
use crate::views::conversations::recipient_picker::RecipientPicker;

/// Internal widget name (NOT a public ui.yaml test ID — same `set_widget_name`
/// convention as the onboarding `vps-server-types-container`) tagging the
/// lazily-filled members box inside each expander, so `populate_folder_members`
/// can find it again after the async `members.list` round-trip returns.
const MEMBERS_BOX: &str = "folder-members-box";

/// Internal widget name tagging the cross-user "Shared with" actor-roster box
/// inside each expander, so `populate_folder_actors` can find it again after the
/// async `members.list_actors` round-trip returns (mirrors [`MEMBERS_BOX`]).
const SHARED_WITH_BOX: &str = "folder-shared-with-box";

/// The `folder-shared-badge` test id — doubles as the widget name (via
/// `set_test_id`), so `populate_folder_actors` finds the badge suffix to update
/// its "Shared · N" text after the actor read.
const SHARED_BADGE_ID: &str = ids::FOLDER_SHARED_BADGE;

/// Internal widget name tagging the per-set **device-activity** roster box
/// (`fauna.folders.devices` — the ordinary sync change signal, distinct
/// from `FolderSummary::cached_snapshot_count`/`cached_total_bytes`, the nest
/// place's snapshot tally), so `populate_folder_devices` can find it again after the
/// async read returns (mirrors [`MEMBERS_BOX`]). Unlike the members/actors
/// rosters this box is also repainted on every `fauna.sync.changed` push while
/// the row stays expanded — see `PushEvent::SyncChanged` in `app.rs`.
const DEVICE_ACTIVITY_BOX: &str = "folder-device-activity-box";

/// Internal widget name tagging the per-set **destination-places** section
/// (`backup-destinations.md` § Ordinary-folder coverage) inside each
/// expander, so `populate_folder_destinations` can find it again after the
/// async `fauna.backup.destination.list` round-trip returns (mirrors
/// `MEMBERS_BOX`). Unlike `DEVICE_ACTIVITY_BOX`'s static heading, THIS tag
/// covers the whole section including its heading: an affordance that
/// cannot work (no destination enrolled at all) must not paint, so the two
/// toggle visibility as one unit.
const DESTINATION_PLACES_BOX: &str = "folder-destination-places-box";

/// Build the folders section: a PreferencesGroup with an Add button suffix and a ListBox.
///
/// Returns the group, the list box, and the add button — the latter is wired in
/// app.rs (`folder-add-button` → open the folder wizard) since opening the
/// wizard needs the authenticated `FaunaClient` available there.
pub fn build_folders_section() -> (adw::PreferencesGroup, gtk::ListBox, gtk::Button) {
    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();

    let placeholder = adw::StatusPage::builder()
        .title(strings::common::NO_FOLDERS_CONFIGURED)
        .icon_name("folder-symbolic")
        .build();
    list_box.set_placeholder(Some(&placeholder));

    let group = adw::PreferencesGroup::builder()
        .title(strings::devices::FOLDERS)
        .build();

    // Add button as header suffix.
    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .label(strings::devices::ADD_FOLDER)
        .css_classes(["flat"])
        .build();
    set_test_id(&add_btn, ids::FOLDER_ADD_BUTTON);
    group.set_header_suffix(Some(&add_btn));

    // The click handler is wired in app.rs (needs the FaunaClient + device list).

    group.add(&list_box);
    (group, list_box, add_btn)
}

/// Rebuild the folder list from `snapshot.folders`. Called from the Folders
/// sub-page render loop on every `DevicesMachine` observer tick. Each set is an
/// expandable `adw::ExpanderRow` (`folder-row`, indexed) whose revealed body
/// carries the enrolled-member roster (lazy-loaded on first expand via
/// `fauna.folders.members.list` — a row-detail read the machine doesn't own),
/// the selective-sync path editors (`folder-include-paths` /
/// `folder-exclude-paths` / `folder-save-paths` → `DevicesMachine::set_folder_paths`),
/// and the delete affordance (`folder-delete-button` → `DevicesMachine::delete_folder`).
#[allow(clippy::too_many_arguments)]
pub fn update_folder_list(
    list_box: &gtk::ListBox,
    folders: &[FolderSummary],
    machine: &Arc<DevicesMachine>,
    fauna_client: &Rc<FaunaClient>,
    location_map: &Rc<RefCell<Vec<LocationBinding>>>,
    can_serve_webdav: bool,
    own_tiers: &[String],
    website_address_enabled: Option<bool>,
    #[cfg(feature = "p2p-share")] group_scopes: &[crate::offline_share::GroupScopeView],
) {
    // Which rows the user had OPEN, so the rebuild below can put them back.
    //
    // This fn rebuilds the whole list from scratch on every snapshot change,
    // and a freshly built `AdwExpanderRow` starts collapsed — so without this,
    // any write that refreshes the machine (a place-flag checkbox, an audience
    // pick, a path save) snapped the row the user was working in shut, taking
    // every body control with it. Two features already depended on the row
    // staying open across a refresh and were silently losing to it: the
    // device-activity roster's live update (wired in `app.rs` behind
    // [`expanded_folder_row_where`], which a collapsed row answers `false`) and
    // the place editor's repaint-from-nest-truth below. Restoring expansion
    // also re-fires `connect_expanded_notify`, so a restored row re-reads its
    // rosters — which is exactly the re-read those repaints want.
    let expanded_titles = expanded_folder_titles(list_box);

    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    for fs in folders {
        // B3 member-list-visibility: a set shared *with* this client (a
        // `role == "member"` row) renders read-only. The load-bearing "only if this
        // client has actually MLS-joined the group" join-filter now lives ONCE in the
        // shared `DevicesMachine` (it delegates to this client's `LinuxMlsQuery` seam,
        // wired in `mod.rs` via `set_mls_query`), so every `role == "member"` row
        // reaching this snapshot is already joined — a stranger's un-accepted knock
        // was dropped upstream and stays only a `folder-pending-share`. Owner rows (`role == "owner"`, or absent, i.e. not a
        // member row) render with the full management UI.
        if fs.role.as_deref() == Some("member") {
            // A **writer** member (multi-writer Phase 1) binds local folders and
            // syncs read-write — an `ExpanderRow` carrying the `folder-location-*` UI.
            // A **reader** stays read-only (a plain `ActionRow`, no binding — a
            // bound folder whose edits could not upload would breach
            // `file-sync.md`'s iron rule). Fail-safe: absent access ⇒ reader.
            // A member whose binding the agent PARKED keeps the binding row
            // and its `folder-access-revoked-warning` even once their access
            // reads `reader` — which is exactly what a demotion does — so the
            // one shared decision is asked, not the access alone.
            let section = fauna_folders_machine::binding_section(
                fs.role.as_deref(),
                fs.access.as_deref(),
                crate::sync_agent::is_access_revoked(&fs.name),
            );
            if section.shown {
                let row = build_writer_member_folder_row(fs, machine, fauna_client, location_map);
                list_box.append(&row);
                restore_expansion(&row, &expanded_titles);
            } else {
                list_box.append(&build_member_folder_row(fs, machine, fauna_client));
            }
            continue;
        }
        let row = build_folder_row(
            fs,
            machine,
            fauna_client,
            location_map,
            can_serve_webdav,
            own_tiers,
            website_address_enabled,
        );
        list_box.append(&row);
        restore_expansion(&row, &expanded_titles);
    }

    // Group scopes are sets like any other, so they are `folder-row`s like
    // any other — appended after the M2 sets, continuing the list. They carry
    // no per-row body in v1: a group scope has no local seat config to edit
    // and no name to rename, so what a row owes is exactly what it can
    // honestly show — the set, and who it is shared with (mirrors tui's
    // `group_scope_row_elements`).
    #[cfg(feature = "p2p-share")]
    for scope in group_scopes {
        list_box.append(&build_group_scope_row(scope));
    }
}

/// One thin, read-only `folder-row` for a shared set this device holds the
/// machinery for (the co-present ceremony's own listing — `p2p.md` §
/// Offline share initiation). Not an `ExpanderRow`: toggling would expand a
/// body this row does not have (no local seat config, no rename, no leave —
/// severance is the authority's mint, not a self-scoped roster drop,
/// `account-data-plane.md` § The recipient-set scheme). The set's identity is
/// its short scope id (nameless in v1), and its `folder-shared-badge` reads
/// "Shared by ‹them›" on someone else's scope, "Shared · N" on your own —
/// the same two readings the M2 rows use.
#[cfg(feature = "p2p-share")]
fn build_group_scope_row(scope: &crate::offline_share::GroupScopeView) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(strings::folders::offline_share_set(&scope.short_id))
        .build();
    set_test_id(&row, ids::FOLDER_ROW);
    crate::testid::set_test_text(&row, &strings::folders::offline_share_set(&scope.short_id));

    let badge = gtk::Label::builder()
        .css_classes(["accent", "caption"])
        .label(match &scope.shared_by {
            Some(who) => strings::devices::shared_by(who),
            None => strings::devices::shared_badge(&scope.member_count.to_string()),
        })
        .build();
    set_test_id(&badge, SHARED_BADGE_ID);
    row.add_suffix(&badge);

    row
}

/// One read-only `folder-row` for a set shared *with* this client (B3
/// member-list-visibility, `docs/goal/ui/folders.md` § Sharing — Recipient
/// side): the set name, a `folder-shared-badge` ("Shared by ‹who›")
/// over the precomputed `owner_display`, and a `folder-leave-button` to remove
/// yourself. Read-only otherwise — NONE of the owner affordances (share / member
/// roster / remove / delete / path editors): a member receives
/// content, they neither manage the share nor sync the owner's folders (and the
/// nest withholds the owner's local paths anyway, `member_summary`). A plain
/// `adw::ActionRow` (not the owner's `ExpanderRow`) — there is nothing to expand.
///
/// The `folder-leave-button` runs the self-scoped member-leave primitive
/// (`FaunaClient::leave_folder_share`): the nest self-drop `fauna.folders.leave`
/// (drop the caller's own roster row, no `ownerSecret`, no content-key rotation)
/// and the local `MlsEngine::forget_group`, addressed by the raw `mls_group_id` this
/// row carries. On success the `DevicesMachine` refreshes and the row drops out of
/// the list (off the roster, `list_owned_and_shared` no longer unions the set).
///
/// Unlike the owner badge (filled async by [`populate_folder_actors`] after the
/// actor read), the recipient badge is known synchronously from the B3 snapshot,
/// so it is set at build time and always visible.
fn build_member_folder_row(
    fs: &FolderSummary,
    machine: &Arc<DevicesMachine>,
    fauna_client: &Rc<FaunaClient>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(&fs.name).build();
    set_test_id(&row, ids::FOLDER_ROW);
    // A `folder-row` reads back as the SET NAME on every app (tui's
    // `Element::gesture_button("folder-row", fs.name, …)`, apple's
    // `.automationValue("folder-row", text: { folder.name })`). THIS row is
    // the one shape that cannot say so by inference: a plain `adw::ActionRow`,
    // whose `find::text_of` reading was its *subtitle* (the retired folder
    // mode) — so the name was invisible to every test (the owner and writer rows are `ExpanderRow`s
    // and kept their title through the descendant-label join, by accident of
    // widget kind). Declare it; see `testid::set_test_text` for the full story.
    crate::testid::set_test_text(&row, &fs.name);

    // "Shared by ‹who›" — the shared pre-computed label (handle, else canonical
    // short id; `value-formatting.md` § Account display label). Empty only for a
    // row this function should never see (a member row always has an owner).
    let who = if fs.owner_display.is_empty() {
        strings::common::UNKNOWN.to_string()
    } else {
        fs.owner_display.clone()
    };
    let shared_badge = gtk::Label::builder()
        .css_classes(["accent", "caption"])
        .label(strings::devices::shared_by(&who))
        .build();
    set_test_id(&shared_badge, SHARED_BADGE_ID);
    row.add_suffix(&shared_badge);

    // `folder-leave-button` — remove yourself from a set shared with you. The
    // shared `DevicesMachine` join-filter (via `LinuxMlsQuery`) only lets a
    // MLS-joined member row reach this snapshot, so `mls_group_id` is present and
    // decodable here; guard defensively anyway (an inert button on the
    // theoretically-impossible None keeps the id present).
    let leave_btn = gtk::Button::builder()
        .label(strings::common::LEAVE)
        .css_classes(["flat", "destructive-action"])
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&leave_btn, ids::FOLDER_LEAVE_BUTTON);
    // The nest self-drop is the leg that matters — `MlsEngine::forget_group`
    // beside it is device-local.
    crate::offline_gate::declare_wire_kind(&leave_btn, "fauna.folders.leave");
    match fs.mls_group_id.clone() {
        Some(group_id) => {
            let machine = Arc::clone(machine);
            let client = Rc::clone(fauna_client);
            leave_btn.connect_clicked(move |_| {
                client.leave_folder_share(Arc::clone(&machine), group_id.clone());
            });
        }
        None => leave_btn.set_sensitive(false),
    }
    row.add_suffix(&leave_btn);

    row
}

/// One `adw::ExpanderRow` for a set shared *with* this client where the caller
/// holds a **writer** grant (`access == "writer"`, multi-writer Phase 1,
/// `docs/goal/ui/folders.md` § Sharing — Recipient side). Like the reader row
/// [`build_member_folder_row`] it shows the set name, a
/// `folder-shared-badge` ("Shared by ‹who›"), and a `folder-leave-button`; a
/// writer *additionally* **binds local folders and syncs read-write**, so it is an
/// `ExpanderRow` carrying the `folder-location-*` binding UI — the same
/// [`location_binding::build_location_binding`] the owner row uses. None of the OTHER
/// owner affordances (share / member roster / remove / delete / path
/// editors / webdav / paywall) appear: those stay owner-only — a writer
/// contributes content, they do not manage the set. The nest admits their records
/// at the `writable_folder` gate; the engine seals under the set's content key
/// from the writer's own custody (`decide_engine_content_binding`).
fn build_writer_member_folder_row(
    fs: &FolderSummary,
    machine: &Arc<DevicesMachine>,
    fauna_client: &Rc<FaunaClient>,
    location_map: &Rc<RefCell<Vec<LocationBinding>>>,
) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder().title(&fs.name).build();
    set_test_id(&row, ids::FOLDER_ROW);
    // Name-only read, same as the reader row above — an `ExpanderRow` would
    // otherwise read back as the join of every descendant label, which happens
    // to contain the name but only by accident of widget kind.
    crate::testid::set_test_text(&row, &fs.name);

    // "Shared by ‹who›" — the shared pre-computed label (handle, else canonical
    // short id; `value-formatting.md` § Account display label). Known
    // synchronously from the B3 snapshot, exactly like the reader row.
    let who = if fs.owner_display.is_empty() {
        strings::common::UNKNOWN.to_string()
    } else {
        fs.owner_display.clone()
    };
    let shared_badge = gtk::Label::builder()
        .css_classes(["accent", "caption"])
        .label(strings::devices::shared_by(&who))
        .build();
    set_test_id(&shared_badge, SHARED_BADGE_ID);
    row.add_suffix(&shared_badge);

    // `folder-leave-button` — remove yourself from the share (the same
    // self-scoped member-leave primitive the reader row uses).
    let leave_btn = gtk::Button::builder()
        .label(strings::common::LEAVE)
        .css_classes(["flat", "destructive-action"])
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&leave_btn, ids::FOLDER_LEAVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&leave_btn, "fauna.folders.leave");
    match fs.mls_group_id.clone() {
        Some(group_id) => {
            let machine = Arc::clone(machine);
            let client = Rc::clone(fauna_client);
            leave_btn.connect_clicked(move |_| {
                client.leave_folder_share(Arc::clone(&machine), group_id.clone());
            });
        }
        None => leave_btn.set_sensitive(false),
    }
    row.add_suffix(&leave_btn);

    // The writer's local-folder binding — the ONLY management affordance a member
    // gets (the writer half of the share). Reuses the owner row's binding widget;
    // an edit under a bound folder uploads under the `writable_folder` gate.
    //
    // A **cross-nest** set (`home_nest_url` is `Some` exactly for a foreign row —
    // `DevicesMachine::foreign_rows`) binds through the same widget, with two
    // differences it carries in `ForeignBind`: the add gesture is verified
    // against the set's home nest before anything binds (D3), and the agent
    // builds a foreign-routed engine for it (D5, via the pushed key blob's
    // routing pair). Until both existed this binding was withheld for foreign
    // rows — the engine would have run unbound and sealed the user's edits under
    // their own `BackupKey`, which is the silent wrong-key class, not a
    // fail-closed one.
    //
    // `folder-access-revoked-warning` (D4) — the owner withdrew this actor's
    // write grant mid-life, the authoritative nest refused the next mint/record,
    // and the agent parked every folder bound to the set. Rendered ABOVE the
    // binding widget, and worded to say both halves (sync stopped; local files
    // untouched), because the failure this exists to prevent is the user
    // discovering months later that a folder they believed was syncing simply
    // went quiet. The binding rows stay visible and removable: a park is not a
    // deletion, and the user still needs to see what was bound. Recovery is a
    // re-bind, which re-runs the eager `write_token.get` verify (D3) and clears
    // the park agent-side, so the same gesture works whether the grant came back
    // or is still gone — it just fails loudly in the latter case.
    if crate::sync_agent::is_access_revoked(&fs.name) {
        let revoked = gtk::Label::builder()
            .css_classes(["warning", "caption"])
            .label(strings::devices::ACCESS_REVOKED_WARNING)
            .wrap(true)
            .xalign(0.0)
            .build();
        set_test_id(&revoked, ids::FOLDER_ACCESS_REVOKED_WARNING);
        row.add_row(&revoked);
    }

    let foreign = foreign_bind_for(fs);
    row.add_row(&location_binding::build_location_binding(
        &fs.name,
        // A foreign set has no row on this nest, so its identity is the derived
        // channel; an own-nest row's is the nest's `folders.id` PK.
        folder_ref_for_row(
            fs.id,
            fs.mls_group_id.as_deref(),
            fs.home_nest_url.as_deref(),
        )
        .map(|r| r.to_wire()),
        location_map,
        fauna_client,
        foreign,
    ));

    row
}

/// The cross-nest bind context for `fs`, or `None` when this row is an own-nest
/// set and binds the ordinary optimistic way.
///
/// `home_nest_url` is `Some` exactly for a foreign row (`DevicesMachine::
/// foreign_rows`), and the set is addressed on its home nest by its derived
/// `ChannelId` — its *name* resolves only there. The derivation is the crate's
/// single [`channel_id_from_group_id_hex`](crate::client::channel_id_from_group_id_hex);
/// deriving it a second way here would address a channel the home nest has never
/// heard of, refusing every bind on a legitimately-granted set.
///
/// **Fail-closed on anything unresolvable** — a foreign row with no group id, or
/// a malformed one, returns `None`, which the caller renders as the withheld
/// binding a reader gets. That is right rather than merely cautious: a set whose
/// channel cannot be derived cannot be addressed by any federated kind, so a
/// bind on it could never verify and could never record.
fn foreign_bind_for(fs: &FolderSummary) -> Option<location_binding::ForeignBind> {
    let home_nest_url = fs.home_nest_url.clone()?;
    let channel_id =
        crate::client::channel_id_from_group_id_hex(fs.mls_group_id.as_deref()?).ok()?;
    Some(location_binding::ForeignBind {
        home_nest_url,
        channel_id_hex: fauna_core::hex32::encode(&channel_id.0),
    })
}

/// One `adw::ExpanderRow` for a folder, with its revealed body. `machine`
/// backs the save-paths / delete gestures; `fauna_client` backs the lazy
/// member-roster read (a row-detail WS-RPC call outside the machine's surface).
/// `can_serve_webdav` is the per-actor MSEK capability the page cached (it is not
/// a snapshot field — the `DevicesMachine` holds no key material); it gates the
/// `folder-webdav-toggle`.
fn build_folder_row(
    fs: &FolderSummary,
    machine: &Arc<DevicesMachine>,
    fauna_client: &Rc<FaunaClient>,
    location_map: &Rc<RefCell<Vec<LocationBinding>>>,
    can_serve_webdav: bool,
    own_tiers: &[String],
    website_address_enabled: Option<bool>,
) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder().title(&fs.name).build();
    set_test_id(&row, ids::FOLDER_ROW);
    // Expanding an owner row is not navigation: it loads that set's rosters +
    // device activity (the `connect_expanded_notify` below). The actor roster is
    // the read the expansion exists for — the same one tui's `ToggleFolderRow`
    // declares — so it, not the members/devices reads that ride along, is the
    // declared kind.
    crate::offline_gate::declare_wire_kind(&row, "fauna.folders.members.list_actors");
    // Name-only read, same as the member rows — see `build_member_folder_row`.
    crate::testid::set_test_text(&row, &fs.name);

    // A `folder-shared-badge` marks a shared set ("Shared · N"); it lives in the
    // header (visible even collapsed) and is filled by `populate_folder_actors`
    // once the actor read returns. Hidden until we know the set is shared.
    let shared_badge = gtk::Label::builder()
        .css_classes(["accent", "caption"])
        .build();
    set_test_id(&shared_badge, SHARED_BADGE_ID);
    shared_badge.set_visible(false);
    row.add_suffix(&shared_badge);

    // Rosters. The enrolled-*device* roster loads lazily on first expand. The
    // cross-user *actor* roster (the "Shared with" list + the badge) loads EAGERLY
    // for an already-shared set (`mls_group_id` known synchronously from the
    // snapshot) so the badge renders on the collapsed row, else lazily on first
    // expand (an owner-only set's read just returns `not_shared`). Both trigger the
    // same actor fetch; one guard keeps it to once per row instance.
    {
        let actors_loaded = Rc::new(Cell::new(false));
        if fs.mls_group_id.is_some() {
            actors_loaded.set(true);
            fauna_client.fetch_folder_actors(&fs.name, fs.mls_group_id.clone());
        }
        let members_loaded = Cell::new(false);
        // Device activity is lazy-only, like the member roster (never eager):
        // unlike the actor badge above it has no collapsed-row representation
        // to keep fresh, so there is nothing to gain from fetching before the
        // row is actually opened. The live-update half (re-fetch on every
        // `fauna.sync.changed` push) is wired separately in `app.rs`, gated on
        // [`expanded_folder_row_where`] rather than this guard — a push can
        // legitimately arrive after this closure already flipped it once.
        let devices_loaded = Cell::new(false);
        // Destination places (`backup-destinations.md` § Ordinary-folder
        // coverage): lazy on first expand like the rosters above, never
        // eager — unlike the actor badge, an unattached-anywhere folder has
        // no collapsed-row representation to keep fresh.
        let destinations_loaded = Cell::new(false);
        let actors_guard = Rc::clone(&actors_loaded);
        let name = fs.name.clone();
        let mls = fs.mls_group_id.clone();
        let folder_id = fs.id;
        let client = Rc::clone(fauna_client);
        row.connect_expanded_notify(move |r| {
            if !r.is_expanded() {
                return;
            }
            if !members_loaded.replace(true) {
                client.fetch_folder_members(&name);
            }
            if !actors_guard.replace(true) {
                client.fetch_folder_actors(&name, mls.clone());
            }
            if !devices_loaded.replace(true) {
                client.fetch_folder_devices(&name);
            }
            if !destinations_loaded.replace(true) {
                client.fetch_folder_destinations(&name, folder_id);
            }
        });
    }

    // Members roster — filled in by `populate_folder_members` once the async
    // `members.list` returns. Tagged with an internal widget name so it can be
    // found again.
    let members_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();
    members_box.set_widget_name(MEMBERS_BOX);
    let members_loading = gtk::Label::builder()
        .label(strings::devices::LOADING_MEMBERS)
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    members_box.append(&members_loading);
    row.add_row(&members_box);

    // Per-set device-activity roster (`fauna.folders.devices`) — the ordinary
    // sync-type change signal (`docs/goal/behavior/file-sync.md` § Implementation
    // status today). Lazy-loaded on first expand (above) and LIVE-UPDATED on every
    // `fauna.sync.changed` push while the row stays expanded (`app.rs`'s
    // `PushEvent::SyncChanged` arm) — that live refresh is the entire point: it is
    // what makes the remote-change nudge e2e-pinnable, mirroring web's
    // `loadDeviceActivity`. Filled in by `populate_folder_devices`.
    row.add_row(&build_device_activity_section());

    // Cross-user "Shared with" section (owner side) — the actor roster + share
    // affordance (`docs/goal/ui/folders.md` § Sharing). Filled by
    // `populate_folder_actors` after the async `members.list_actors` read.
    row.add_row(&build_shared_with_section(&fs.name, machine, fauna_client));

    // Selective-sync path editors. The labels themselves carry the
    // "(comma-separated)" hint, so they double as the entry placeholders.
    let include_entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text(strings::devices::INCLUDE_PATHS)
        .text(fauna_folders_machine::join_paths_field(
            fs.include_paths.as_deref(),
        ))
        .build();
    set_test_id(&include_entry, ids::FOLDER_INCLUDE_PATHS);
    row.add_row(&entry_row(strings::devices::INCLUDE_PATHS, &include_entry));

    let exclude_entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text(strings::devices::EXCLUDE_PATHS)
        .text(fauna_folders_machine::join_paths_field(
            fs.exclude_paths.as_deref(),
        ))
        .build();
    set_test_id(&exclude_entry, ids::FOLDER_EXCLUDE_PATHS);
    row.add_row(&entry_row(strings::devices::EXCLUDE_PATHS, &exclude_entry));

    // (The per-row scan-frequency editor that used to sit here retired with
    // phase 5 of the folders re-model: the cadence is a constant, not a choice
    // — `file-sync.md` § Config, the phase-5 block.)

    // Per-set conflict policy (`folder-conflict-policy-select`, indexed per
    // row) — file-sync.md § Conflicts, policy. A value/label DropDown split
    // (mirroring web's `<option value>`/label): the MODEL strings are the
    // stable wire values ("auto" / "latest_wins_always") the cross-app
    // `select(id, value)` / `get_text` e2e contract drives, and a display
    // expression maps each to its localized label — both from the shared
    // `conflict_policy_options()` catalog (same source as every app's
    // picker). Every owner row — a folder has no type (`ui/folders.md`
    // § Conflicts); the resolving device reads the policy off the authoritative
    // nest row.
    let policy_opts = fauna_folders_machine::conflict_policy_options();
    let policy_values: Vec<String> = policy_opts.iter().map(|o| o.value.clone()).collect();
    let policy_value_refs: Vec<&str> = policy_values.iter().map(String::as_str).collect();
    let policy_dropdown = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&policy_value_refs))
        .build();
    let policy_label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        gtk::glib::closure!(|item: gtk::StringObject| {
            fauna_folders_machine::conflict_policy_label(item.string().as_str())
                .resolve(strings::lookup)
        }),
    );
    policy_dropdown.set_expression(Some(&policy_label_expr));
    set_test_id(&policy_dropdown, ids::FOLDER_CONFLICT_POLICY_SELECT);
    // The per-set policy is a column on the folder row — `folders.update`,
    // not the account-wide `config.put` its page-level sibling
    // (`sync-default-conflict-policy-select`) writes.
    crate::offline_gate::declare_wire_kind(&policy_dropdown, "fauna.folders.update");
    // Pre-select the row's current policy BEFORE connecting the handler
    // so the initial programmatic selection (and every re-render after a
    // refresh) does not re-fire the change — no spurious write. Absent
    // (a member row) renders as the column default, auto.
    let current_policy = fs.conflict_policy.as_deref().unwrap_or("auto");
    if let Some(idx) = policy_values.iter().position(|v| v == current_policy) {
        policy_dropdown.set_selected(idx as u32);
    }
    {
        let machine = Arc::clone(machine);
        let name = fs.name.clone();
        policy_dropdown.connect_selected_notify(move |dd| {
            let Some(value) = policy_values.get(dd.selected() as usize) else {
                return;
            };
            let machine = Arc::clone(&machine);
            let name = name.clone();
            let value = value.to_string();
            async_helper::run_on_tokio(
                async move { machine.set_folder_conflict_policy(name, value).await },
                |_| {},
            );
        });
    }
    row.add_row(&entry_row(
        strings::devices::CONFLICT_POLICY,
        &policy_dropdown,
    ));

    // ── Audience + website serving (folders re-model phase 4 slice 4d) ──────
    //
    // Both render on EVERY owner row. Audience is the folder's
    // IDENTITY rather than a serving option (`behavior/folders.md` § Target
    // re-model), and the website toggle is the ONLY door to a website folder
    // (a folder has no type, `ui/folders.md` § Modes).
    // UX contract: `ui/folders.md` § Audience and website serving.
    let bound = fs.mls_group_id.is_some();
    // NORMALIZED, never the raw column: the select's value has to be one of the
    // options it offers, and an absent audience arrives as
    // the empty string (`FolderSummary`'s default). Fail-closed in
    // the one direction that matters — nothing unparseable ever paints as
    // Public, because a binary that cannot read the column must not tell the
    // user their folder is world-readable.
    let audience = fauna_folders_machine::normalize_audience(&fs.audience, bound);
    // The option set rides (bound, current), so it offers only what the nest
    // would accept: an unbound folder gets private/public, a bound one gets
    // `shared` (its own state) plus `public`, and a bound-and-public one gets
    // the `shared` flip-back — the one legal exit from its public window.
    // A value/label DropDown split exactly like the conflict-policy picker
    // above: the MODEL strings are the stable wire values the cross-app
    // `select(id, value)` / `get_text` contract drives, and a display expression
    // maps each to its localized label.
    let audience_opts = fauna_folders_machine::audience_options(bound, &audience);
    let audience_values: Vec<String> = audience_opts.iter().map(|o| o.value.clone()).collect();
    let audience_value_refs: Vec<&str> = audience_values.iter().map(String::as_str).collect();
    let audience_dropdown = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&audience_value_refs))
        .build();
    let audience_label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        gtk::glib::closure!(|item: gtk::StringObject| {
            fauna_folders_machine::audience_label(item.string().as_str()).resolve(strings::lookup)
        }),
    );
    audience_dropdown.set_expression(Some(&audience_label_expr));
    set_test_id(&audience_dropdown, ids::FOLDER_AUDIENCE_SELECT);
    // Keyless for every direction the picker offers, the bound `→shared`
    // flip-back included — a plain `folders.update`, never an orchestration.
    crate::offline_gate::declare_wire_kind(&audience_dropdown, "fauna.folders.update");
    // Pre-select BEFORE connecting the handler, as the conflict-policy picker
    // does: the initial programmatic selection (and every re-render after a
    // refresh) must not re-fire the change into a spurious write.
    if let Some(idx) = audience_values.iter().position(|v| *v == audience) {
        audience_dropdown.set_selected(idx as u32);
    }
    row.add_row(&entry_row(
        strings::devices::FOLDER_AUDIENCE,
        &audience_dropdown,
    ));
    // The hint rides the SAME (bound, current) inputs as the option set, so the
    // copy and the picker cannot disagree: a bound folder explains that sharing
    // is edited below, and a bound folder currently public explains the one exit
    // its picker does offer.
    row.add_row(&hint_label(
        &fauna_folders_machine::audience_hint(bound, &audience).resolve(strings::lookup),
    ));
    {
        let fauna_client = Rc::clone(fauna_client);
        let machine = Arc::clone(machine);
        let name = fs.name.clone();
        let current_audience = audience.clone();
        let dropdown_for_handler = audience_dropdown.clone();
        audience_dropdown.connect_selected_notify(move |dd| {
            let Some(value) = audience_values.get(dd.selected() as usize) else {
                return;
            };
            // Re-selecting the folder's current audience is a no-op, never a
            // write. This is also what makes a bound folder's `shared` option
            // non-selectable in the one state where the contract says it is
            // (bound and NOT public, where `shared` IS the current value), while
            // leaving it live as the flip-back when the folder is public — the
            // `selectable` flag on `AudienceOption` expressed as behaviour. And
            // it is what terminates the snap-back below: setting the selection
            // re-enters this handler, which then sees current == picked.
            if *value == current_audience {
                return;
            }
            if value == fauna_folders_machine::AUDIENCE_PUBLIC {
                // Picking Public ARMS; it does not publish. A public folder
                // rests UNSEALED — content and names/paths alike, since the
                // names become the address of each file (`principles.md` § The
                // user always controls their data owns that single exception to
                // sealed-at-rest), so the flip happens only when the confirm is
                // answered.
                //
                // ⚠ Snap the select back FIRST. GTK commits a `DropDown` pick
                // immediately, unlike a framework that re-renders off state, so
                // without this the row would paint `Public` while the confirm is
                // still unanswered and the nest has not moved — reporting an
                // audience the folder does not have. The contract is explicit
                // that the select keeps painting the CURRENT audience for as
                // long as the confirm is armed, which is exactly the assertion
                // no other test makes and the one an app is most likely to get
                // wrong. Answering the confirm repaints it off the snapshot; a
                // cancel then needs no reset at all, because nothing moved.
                if let Some(idx) = audience_values.iter().position(|v| *v == current_audience) {
                    dd.set_selected(idx as u32);
                }
                confirm_public_audience(&dropdown_for_handler, &name, &machine, &fauna_client);
                return;
            }
            fauna_client.set_folder_audience(Arc::clone(&machine), &name, value);
        });
    }

    // "Serve this folder as your website" — the structural sibling of the WebDAV
    // toggle below, and the door phase 2 slice e closed. It stays ENABLED on a
    // folder that is neither public nor paywalled, and is merely inert there:
    // the flag publishes the folder's HEAD, the audience decides who may READ
    // it. Disabling it would imply the setting is unavailable and would strand
    // the user with no way to prepare a site before publishing it.
    //
    // The wording is the shared TRI-state on the live serving picture
    // (`website_serve_hint` owns the states and the degrade direction):
    // publishing a site takes switches in two places — this toggle plus the
    // actor's own web-address opt-in — and a user who flipped only this half was
    // told nothing while the nest served its info page in their site's place.
    // `website_address_enabled` is the best-effort second half; UNKNOWN must
    // never claim the site is live, which is why the hedge is its own state.
    let website_hint = fauna_folders_machine::website_serve_hint(
        &audience,
        fs.web_paywall_tier.is_some(),
        website_address_enabled,
    )
    .resolve(strings::lookup);
    let website_switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(fs.website_enabled)
        .tooltip_text(&website_hint)
        .build();
    set_test_id(&website_switch, ids::FOLDER_WEBSITE_TOGGLE);
    // Keyless like the audience beside it — `set_website_enabled` is a plain
    // `folders.update`, NOT the `serve_set` orchestration the WebDAV toggle runs.
    crate::offline_gate::declare_wire_kind(&website_switch, "fauna.folders.update");
    {
        let fauna_client = Rc::clone(fauna_client);
        let machine = Arc::clone(machine);
        let name = fs.name.clone();
        website_switch.connect_active_notify(move |sw| {
            fauna_client.set_folder_website(Arc::clone(&machine), &name, sw.is_active());
        });
    }
    row.add_row(&entry_row(strings::devices::SERVE_WEBSITE, &website_switch));
    // A switch's tooltip is not discoverable on a control the user never
    // hovers, so the hint ALSO paints inline — the same placement intent as
    // tui's adjacent chrome, web's hint span, and the WebDAV needs-mail label.
    row.add_row(&hint_label(&website_hint));

    // Per-set "serve over WebDAV" opt-in (`folder-webdav-toggle`, every owner row
    // — a folder has no type) — webdav-server.md § Independent enablement point 2. NOT a
    // `DevicesMachine` config write: it runs the full serve orchestration
    // (content-key genesis/rotation + the nest flag + the MSEK-sealed
    // `WebdavKeysBlob` re-provision) via the shared `FoldersAuthor::serve_set`,
    // crate-direct mirror of the web reference (`FoldersSection.svelte::changeWebdav`
    // → `foldersServeSet`).
    //
    // Serving seals the blob under the actor's MSEK, so an actor who has not set
    // up mail cannot serve at all: the switch is INSENSITIVE with a "set up mail
    // first" tooltip (`can_serve_webdav`, cached by the page off the shared
    // `owner_can_serve_webdav`). That is not cosmetic — `serve_set` flips the nest
    // flag before it re-provisions, so a doomed enable would commit the flag and
    // only then fail `NoMsek`, leaving the set served-but-blobless until the next
    // reconcile heals it. Any *other* failure still surfaces on the shared
    // `error-message` banner.
    let webdav_switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(fs.webdav_enabled)
        .sensitive(can_serve_webdav)
        .tooltip_text(if can_serve_webdav {
            strings::devices::SERVE_WEBDAV_HINT
        } else {
            strings::devices::SERVE_WEBDAV_NEEDS_MAIL
        })
        .build();
    set_test_id(&webdav_switch, ids::FOLDER_WEBDAV_TOGGLE);
    // The composite's BINDING leg, not the cheap flag: `serve_set` flips the
    // nest flag and then re-provisions the MSEK-sealed `WebdavKeysBlob`, and
    // the provision is the half that genuinely needs a nest (an unfinished
    // flip is what `serve_set`'s own doc warns about). Declared AFTER the
    // builder's `.sensitive(can_serve_webdav)`, so the "set up mail first"
    // intent is what a reconnect restores — never a blanket enable.
    crate::offline_gate::declare_wire_kind(
        &webdav_switch,
        "fauna.bridges.provision_webdav_keys_blob",
    );
    {
        let machine = Arc::clone(machine);
        let fauna_client = Rc::clone(fauna_client);
        let name = fs.name.clone();
        let mls_group_id = fs.mls_group_id.clone();
        webdav_switch.connect_active_notify(move |sw| {
            fauna_client.serve_set_folder(
                Arc::clone(&machine),
                &name,
                mls_group_id.clone(),
                sw.is_active(),
            );
        });
    }
    let webdav_row = entry_row(strings::devices::SERVE_WEBDAV, &webdav_switch);
    if !can_serve_webdav {
        // Say *why* it is insensitive, inline — the tooltip alone is not
        // discoverable on a control the user cannot focus. Same string, same
        // placement intent as the web hint span / android hint Text.
        webdav_row.append(
            &gtk::Label::builder()
                .label(strings::devices::SERVE_WEBDAV_NEEDS_MAIL)
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    row.add_row(&webdav_row);

    // Per-set "paywall to tier" control (`folder-paywall-tier-select`,
    // website-enabled rows only) — folders.md § Web paywall / monetization.md
    // § Pillar 2. The structural sibling of the webdav toggle above: NOT a
    // `DevicesMachine` config write, it runs the full paywall orchestration
    // (content-key genesis/re-seal + the nest `web_paywall_tier` flag + the
    // web-serve-holder `content.read{folder:set}` grant mint) via the shared
    // `FoldersAuthor::paywall_set`, crate-direct mirror of the FFI/wasm faces.
    //
    // Same value/label DropDown split as the conflict select: the MODEL
    // strings are the stable wire values the cross-app `select(id, value)` /
    // `get_text` e2e contract drives — each own-tier's NAME, plus an empty-string
    // sentinel for the "Not paywalled (public)" placeholder — and a display
    // expression maps each to its label. v1 is SET-ONLY (ratified 2026-07-13): the
    // placeholder is offered only while the set is still public; once paywalled it
    // is gone (no clear affordance — the nest-side revoke/rotation leg is not
    // shipped). No tiers ⇒ nothing to paywall to: the select is insensitive with a
    // "create a tier first" hint, mirroring the webdav "set up mail first" gate.
    // Website-enabled rows only — keyed on the toggle, never on the retired
    // `mode = "web"` spelling.
    if fs.website_enabled {
        let current = fs.web_paywall_tier.clone();
        let has_tiers = !own_tiers.is_empty();

        // Model = wire values. The empty sentinel (placeholder) rides only while
        // public. A tier the set is already paywalled to is always present even if
        // it was since removed from the tier list, so the row still shows its state.
        let mut values: Vec<String> = Vec::new();
        if current.is_none() {
            values.push(String::new());
        }
        for t in own_tiers {
            values.push(t.clone());
        }
        if let Some(cur) = &current
            && !own_tiers.iter().any(|t| t == cur)
        {
            values.push(cur.clone());
        }
        let value_refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let paywall_dropdown = gtk::DropDown::builder()
            .model(&gtk::StringList::new(&value_refs))
            .build();
        let label_expr = gtk::ClosureExpression::new::<String>(
            &[] as &[gtk::Expression],
            gtk::glib::closure!(|item: gtk::StringObject| {
                let v = item.string();
                if v.is_empty() {
                    strings::devices::PAYWALL_TIER_NONE.to_string()
                } else {
                    v.to_string()
                }
            }),
        );
        paywall_dropdown.set_expression(Some(&label_expr));
        set_test_id(&paywall_dropdown, ids::FOLDER_PAYWALL_TIER_SELECT);

        // Pre-select the row's current tier BEFORE connecting the handler (same
        // no-spurious-write rule as the conflict select). Public ⇒ the
        // placeholder at index 0.
        let selected_idx = match &current {
            Some(cur) => values.iter().position(|v| v == cur).unwrap_or(0),
            None => 0,
        };
        paywall_dropdown.set_selected(selected_idx as u32);

        paywall_dropdown.set_sensitive(has_tiers);
        paywall_dropdown.set_tooltip_text(Some(if has_tiers {
            strings::devices::PAYWALL_TIER_HINT
        } else {
            strings::devices::PAYWALL_TIER_NEEDS_TIER
        }));
        // Same composite rule as the webdav toggle: `paywall_set` keys the set,
        // flips `folders.set_web_paywall`, then mints the web-serve holder's
        // `content.read{folder:set}` grant — the mint is what makes an entitled
        // visitor able to open the seal, so it is the declared leg. Declared
        // AFTER `set_sensitive(has_tiers)` so the "create a tier first" intent
        // survives an offline→online cycle.
        crate::offline_gate::declare_wire_kind(&paywall_dropdown, "fauna.capabilities.mint");
        {
            let machine = Arc::clone(machine);
            let fauna_client = Rc::clone(fauna_client);
            let name = fs.name.clone();
            let mls_group_id = fs.mls_group_id.clone();
            let values = values.clone();
            paywall_dropdown.connect_selected_notify(move |dd| {
                let Some(value) = values.get(dd.selected() as usize) else {
                    return;
                };
                // The empty placeholder is not a write — v1 is set-only, there is
                // no "clear the paywall" path yet. Only a real tier dispatches.
                if value.is_empty() {
                    return;
                }
                fauna_client.paywall_set_folder(
                    Arc::clone(&machine),
                    &name,
                    mls_group_id.clone(),
                    value,
                );
            });
        }
        let paywall_row = entry_row(strings::devices::PAYWALL_TIER, &paywall_dropdown);
        if !has_tiers {
            // Say *why* it is insensitive, inline — the tooltip alone is not
            // discoverable on a control the user cannot focus (same intent as the
            // webdav "set up mail first" hint).
            paywall_row.append(
                &gtk::Label::builder()
                    .label(strings::devices::PAYWALL_TIER_NEEDS_TIER)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["dim-label", "caption"])
                    .build(),
            );
        }
        row.add_row(&paywall_row);
    }

    // Local-folder binding (desktop) — nested under THIS folder (spec § 3, O-1):
    // the device-local folders bound to this set + the typed-path add control
    // (no free-text set name — the set is contextual). `apps/linux.md` § File Sync.
    // An owner's own set is always same-nest (a foreign set has no nest row here
    // at all), so its bind stays optimistic — `None` foreign context.
    row.add_row(&location_binding::build_location_binding(
        &fs.name,
        folder_ref_for_row(
            fs.id,
            fs.mls_group_id.as_deref(),
            fs.home_nest_url.as_deref(),
        )
        .map(|r| r.to_wire()),
        location_map,
        fauna_client,
        None,
    ));

    // Destination places (`backup-destinations.md` § Ordinary-folder
    // coverage) — filled in by `populate_folder_destinations` once
    // `fauna.backup.destination.list` returns for this row's `folder_id`.
    // Hidden until then: an affordance that cannot work (no destination
    // enrolled at all) must not paint.
    row.add_row(&build_destination_places_section());

    // ── The nest place's snapshot policy (backup-restore.md § 8b) ────────
    //
    // On EVERY folder: "what the nest keeps" is a property of the one place
    // every folder has (a folder has no type, `ui/folders.md` § Modes).
    //
    // Three knobs, each three-state. The select spells its third state out; for
    // the two retention boxes and the quiet period, a BLANK box IS that third
    // state — so all four are staged and committed together by
    // `folder-nest-save-button`, never applied on change like the conflict
    // select above. An apply-on-change knob here would have to send its three
    // siblings with it, committing half-typed values the user had not saved.
    row.add_row(&section_heading(strings::devices::NEST_PLACE_SECTION));

    // Same value/label DropDown split as the conflict select: the
    // MODEL strings are the wire values ("default" / "on" / "off") the cross-app
    // `select(id, value)` contract drives, and the display expression maps each
    // to its localized label — both from the shared catalog.
    let nest_snapshot_opts = fauna_folders_machine::nest_snapshots_options();
    let nest_snapshot_values: Vec<String> =
        nest_snapshot_opts.iter().map(|o| o.value.clone()).collect();
    let nest_snapshot_refs: Vec<&str> = nest_snapshot_values.iter().map(String::as_str).collect();
    let nest_snapshots_dropdown = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&nest_snapshot_refs))
        .build();
    let nest_snapshots_label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        gtk::glib::closure!(|item: gtk::StringObject| {
            fauna_folders_machine::nest_snapshots_label(item.string().as_str())
                .resolve(strings::lookup)
        }),
    );
    nest_snapshots_dropdown.set_expression(Some(&nest_snapshots_label_expr));
    set_test_id(&nest_snapshots_dropdown, ids::FOLDER_NEST_SNAPSHOTS_SELECT);

    // Seed all four controls from the row through the shared prefill, which owns
    // the rules an app must not re-derive: an unset knob shows BLANK, and so
    // does a ZERO retention bound — zero is the nest's own spelling of unset, so
    // rendering it would turn "nothing chosen" into a bound the owner appears to
    // have picked. Seeded BEFORE any handler is connected, like the two selects
    // above, so the initial programmatic selection never re-fires as a write.
    let seed = fauna_folders_machine::nest_place_edit_from_row(
        fs.nest_snapshots,
        fs.nest_snapshot_quiet_secs,
        fs.retention_policy.clone(),
    );
    if let Some(idx) = nest_snapshot_values
        .iter()
        .position(|v| *v == seed.snapshots)
    {
        nest_snapshots_dropdown.set_selected(idx as u32);
    }
    row.add_row(&entry_row(
        strings::devices::NEST_SNAPSHOTS,
        &nest_snapshots_dropdown,
    ));

    let nest_quiet_entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text(strings::devices::NEST_QUIET)
        .text(&seed.quiet_secs)
        .build();
    set_test_id(&nest_quiet_entry, ids::FOLDER_NEST_QUIET_INPUT);
    row.add_row(&entry_row(strings::devices::NEST_QUIET, &nest_quiet_entry));

    let nest_retention_snapshots_entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text(strings::devices::NEST_RETENTION_SNAPSHOTS)
        .text(&seed.retention_snapshots)
        .build();
    set_test_id(
        &nest_retention_snapshots_entry,
        ids::FOLDER_NEST_RETENTION_SNAPSHOTS,
    );
    row.add_row(&entry_row(
        strings::devices::NEST_RETENTION_SNAPSHOTS,
        &nest_retention_snapshots_entry,
    ));

    let nest_retention_days_entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text(strings::devices::NEST_RETENTION_DAYS)
        .text(&seed.retention_days)
        .build();
    set_test_id(&nest_retention_days_entry, ids::FOLDER_NEST_RETENTION_DAYS);
    row.add_row(&entry_row(
        strings::devices::NEST_RETENTION_DAYS,
        &nest_retention_days_entry,
    ));

    // The version-retention SIBLING pair (file-versions.md § Retention ruling
    // 1): bounds file-version history, never snapshots — its own
    // `folders.version_retention` column, riding the same save.
    let version_retention_seed = fauna_folders_machine::version_retention_edit_from_bounds(
        fs.version_retention_max_versions,
        fs.version_retention_max_age_days,
    );
    let version_retention_count_entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text(strings::devices::VERSION_RETENTION_COUNT)
        .text(&version_retention_seed.count)
        .build();
    set_test_id(
        &version_retention_count_entry,
        ids::FOLDER_VERSION_RETENTION_COUNT,
    );
    row.add_row(&entry_row(
        strings::devices::VERSION_RETENTION_COUNT,
        &version_retention_count_entry,
    ));

    let version_retention_days_entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text(strings::devices::VERSION_RETENTION_DAYS)
        .text(&version_retention_seed.days)
        .build();
    set_test_id(
        &version_retention_days_entry,
        ids::FOLDER_VERSION_RETENTION_DAYS,
    );
    row.add_row(&entry_row(
        strings::devices::VERSION_RETENTION_DAYS,
        &version_retention_days_entry,
    ));

    // Not decoration: the only on-screen statement that emptying a box is a real
    // choice rather than a no-op.
    row.add_row(&hint_label(strings::devices::NEST_PLACE_BLANK_HINT));

    let nest_save_btn = gtk::Button::builder()
        .label(strings::devices::NEST_SAVE)
        .css_classes(["flat"])
        .halign(gtk::Align::End)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();
    set_test_id(&nest_save_btn, ids::FOLDER_NEST_SAVE_BUTTON);
    // The four nest-place knobs above are STAGED — this button is the one
    // gesture that reaches the nest (`set_folder_nest_place` → the whole-policy
    // `folders.update`), so it carries the declaration and they carry none.
    crate::offline_gate::declare_wire_kind(&nest_save_btn, "fauna.folders.update");
    {
        let name = fs.name.clone();
        let machine = Arc::clone(machine);
        let snapshots_dd = nest_snapshots_dropdown.clone();
        let quiet_entry = nest_quiet_entry.clone();
        let retention_snapshots_entry = nest_retention_snapshots_entry.clone();
        let retention_days_entry = nest_retention_days_entry.clone();
        let version_retention_count_entry = version_retention_count_entry.clone();
        let version_retention_days_entry = version_retention_days_entry.clone();
        nest_save_btn.connect_clicked(move |_| {
            // The four controls' raw values go through the shared
            // `nest_place_write`, which owns both traps: every knob rides on
            // every save (the nest applies the policy whole, so an emptied box
            // must arrive as unset), and a cleared retention rides as the
            // canonical binds-nothing policy — never `None`, which the wire
            // reads as "leave unchanged" and would let a user set retention and
            // never take it back.
            let write =
                fauna_folders_machine::nest_place_write(&fauna_folders_machine::NestPlaceEdit {
                    snapshots: snapshots_dd
                        .selected_item()
                        .and_then(|i| i.downcast::<gtk::StringObject>().ok())
                        .map(|s| s.string().to_string())
                        .unwrap_or_default(),
                    quiet_secs: quiet_entry.text().to_string(),
                    retention_snapshots: retention_snapshots_entry.text().to_string(),
                    retention_days: retention_days_entry.text().to_string(),
                });
            // The version-retention sibling rides the same save, its own
            // family sent whole: both boxes blank ⇒ the binds-nothing write
            // that clears the policy (never the wire's leave-unchanged None —
            // the knobs are on screen, so what they say is what the user said).
            let version_retention = fauna_folders_machine::version_retention_write(
                &fauna_folders_machine::VersionRetentionEdit {
                    count: version_retention_count_entry.text().to_string(),
                    days: version_retention_days_entry.text().to_string(),
                },
            );
            let machine = Arc::clone(&machine);
            let name = name.clone();
            async_helper::run_on_tokio(
                async move {
                    machine
                        .set_folder_nest_place(
                            name,
                            write.snapshots,
                            write.quiet_secs,
                            write.retention,
                            Some(version_retention),
                        )
                        .await
                },
                |_| {},
            );
        });
    }
    row.add_row(&nest_save_btn);

    // ── The nest place's content residency (folders re-model phase 5 —
    // file-sync.md § Content residency) ──────────────────────────────────
    //
    // Applies ON CHANGE, unlike the four staged knobs above — its own
    // `folders.update` field, deliberately OUTSIDE the batched save, so an
    // older writer's policy edit can never silently clear it. The flip to
    // Metadata-only is consent-gated: it deletes the nest's copy of the
    // folder's content, so picking it opens `confirm_residency` (the
    // `folder-delete-confirm` `adw::AlertDialog` pattern) rather than
    // committing — the same UX shape as `folder-audience-public-confirm`.
    let residency_opts = fauna_folders_machine::residency_options();
    let residency_values: Vec<String> = residency_opts.iter().map(|o| o.value.clone()).collect();
    let residency_value_refs: Vec<&str> = residency_values.iter().map(String::as_str).collect();
    let residency_dropdown = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&residency_value_refs))
        .build();
    let residency_label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        gtk::glib::closure!(|item: gtk::StringObject| {
            fauna_folders_machine::residency_label(item.string().as_str()).resolve(strings::lookup)
        }),
    );
    residency_dropdown.set_expression(Some(&residency_label_expr));
    set_test_id(&residency_dropdown, ids::FOLDER_NEST_RESIDENCY_SELECT);
    crate::offline_gate::declare_wire_kind(&residency_dropdown, "fauna.folders.update");

    // Pre-select BEFORE connecting the handler, like every select above, so the
    // initial programmatic selection never re-fires as a write. Fail-closed:
    // `normalize_residency` reads anything but the exact `metadata_only` value
    // as Full — the same reading the hint below and the write's own refusal
    // path share.
    let current_residency = fauna_folders_machine::normalize_residency(&fs.residency);
    if let Some(idx) = residency_values
        .iter()
        .position(|v| *v == current_residency)
    {
        residency_dropdown.set_selected(idx as u32);
    }
    row.add_row(&entry_row(
        strings::devices::FOLDER_RESIDENCY,
        &residency_dropdown,
    ));
    row.add_row(&hint_label(
        &fauna_folders_machine::residency_hint(&current_residency).resolve(strings::lookup),
    ));
    {
        let machine = Arc::clone(machine);
        let name = fs.name.clone();
        let dropdown_for_handler = residency_dropdown.clone();
        let current_residency_for_handler = current_residency.clone();
        residency_dropdown.connect_selected_notify(move |dd| {
            let Some(value) = residency_values.get(dd.selected() as usize) else {
                return;
            };
            // Re-selecting the folder's current residency is a no-op, never a
            // write — and it is what terminates the snap-back below, which
            // re-enters this handler with current == picked.
            if *value == current_residency_for_handler {
                return;
            }
            if value == fauna_folders_machine::RESIDENCY_METADATA_ONLY {
                // ⚠ Snap the select back FIRST, then arm. GTK commits a
                // `DropDown` pick immediately, so without this the row paints
                // Metadata-only while the confirm is still unanswered and the
                // nest has not moved — reporting a residency the folder does
                // not have. `file-sync.md` § Content residency is explicit that
                // this is "the audience-confirm pattern: the select keeps
                // painting the current value until the confirm is answered",
                // and before slice 4d's `folder-audience-public-confirm` landed
                // beside it this arm reset only on CANCEL, which left exactly
                // that window mispainted.
                if let Some(idx) = residency_values
                    .iter()
                    .position(|v| *v == current_residency_for_handler)
                {
                    dd.set_selected(idx as u32);
                }
                confirm_residency(&dropdown_for_handler, &name, &machine);
                return;
            }
            let machine = Arc::clone(&machine);
            let name = name.clone();
            let value = value.to_string();
            async_helper::run_on_tokio(
                async move { machine.set_folder_residency(name, value).await },
                |_| {},
            );
        });
    }

    // Action buttons (save paths + delete) on one row.
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();

    let save_btn = gtk::Button::builder()
        .label(strings::devices::SAVE_PATHS)
        .css_classes(["flat"])
        .build();
    set_test_id(&save_btn, ids::FOLDER_SAVE_PATHS);
    // The include/exclude entries stage; this button sends them
    // (`set_folder_paths` → the path-fields-only `folders.update`). Its
    // safety-net click of `folder-location-add-button` below is device-local
    // (the sync agent), so it adds no kind of its own.
    crate::offline_gate::declare_wire_kind(&save_btn, "fauna.folders.update");
    {
        let name = fs.name.clone();
        let machine = Arc::clone(machine);
        let include_entry = include_entry.clone();
        let exclude_entry = exclude_entry.clone();
        let row_weak = row.downgrade();
        save_btn.connect_clicked(move |_| {
            // Safety net for a real user-reported mistake (2026-07-20 manual-test
            // loop): this button only owns the include/exclude fields, but a user
            // who just typed/picked a device-local folder path
            // (`folder-location-path-input`, a few rows up) naturally reaches for this
            // "Save…"-labeled button too. Without this, that pending path is
            // silently dropped — no error, no effect (regression test:
            // `test_folder_save_paths_does_not_drop_pending_location_path`). So
            // before saving the patterns, commit any pending folder path exactly
            // as the dedicated Add button would.
            if let Some(row) = row_weak.upgrade()
                && let Some(path_input) = crate::testid::find_by_test_id(
                    row.upcast_ref::<gtk::Widget>(),
                    "folder-location-path-input",
                )
                && let Some(entry) = path_input.downcast_ref::<adw::EntryRow>()
                && !entry.text().trim().is_empty()
                && let Some(add_button) = crate::testid::find_by_test_id(
                    row.upcast_ref::<gtk::Widget>(),
                    "folder-location-add-button",
                )
                && let Some(button) = add_button.downcast_ref::<gtk::Button>()
            {
                button.emit_clicked();
            }

            let machine = Arc::clone(&machine);
            let name = name.clone();
            let include = fauna_folders_machine::parse_paths_field(&include_entry.text());
            let exclude = fauna_folders_machine::parse_paths_field(&exclude_entry.text());
            // `set_folder_paths` sends only the path fields then self-refreshes
            // (observer tick) so the row re-renders from authoritative state.
            async_helper::run_on_tokio(
                async move {
                    machine
                        .set_folder_paths(name, Some(include), Some(exclude))
                        .await
                },
                |_| {},
            );
        });
    }
    actions.append(&save_btn);

    let delete_btn = gtk::Button::builder()
        .label(strings::devices::DELETE_FOLDER)
        .css_classes(["flat", "destructive-action"])
        .build();
    set_test_id(&delete_btn, ids::FOLDER_DELETE_BUTTON);
    {
        let name = fs.name.clone();
        let machine = Arc::clone(machine);
        delete_btn.connect_clicked(move |btn| {
            confirm_delete(btn, &name, &machine);
        });
    }
    actions.append(&delete_btn);

    row.add_row(&actions);

    row
}

/// A section heading inside the expander body — the same inset as [`entry_row`],
/// bolder, so a group of related controls reads as one thing.
fn section_heading(label: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .margin_top(8)
        .margin_bottom(2)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["heading"])
        .build()
}

/// A wrapped muted explainer line inside the expander body, at [`entry_row`]'s
/// inset.
fn hint_label(label: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(label)
        .wrap(true)
        .xalign(0.0)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["fauna-muted"])
        .build()
}

/// A captioned text-entry row for the expander body: a small caption above the
/// entry, both inset to match the libadwaita boxed-list padding.
fn entry_row(label: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();
    let lbl = gtk::Label::builder()
        .label(label)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    row.append(&lbl);
    row.append(child);
    row
}

/// Present the destructive delete confirmation, then call
/// `DevicesMachine::delete_folder` on confirm. Mirrors the snapshot
/// prune/delete `adw::AlertDialog` pattern.
fn confirm_delete(btn: &gtk::Button, name: &str, machine: &Arc<DevicesMachine>) {
    let machine = Arc::clone(machine);
    let name_owned = name.to_string();
    crate::confirm_dialog::present_confirm(
        btn,
        crate::confirm_dialog::ConfirmSpec::new(
            strings::devices::DELETE_CONFIRM_TITLE,
            crate::confirm_dialog::ConfirmBody::Text(&strings::devices::delete_confirm_body(name)),
            "delete",
            strings::devices::DELETE_FOLDER,
            crate::confirm_dialog::CANCEL,
        )
        .with_confirm_id(ids::FOLDER_DELETE_CONFIRM)
        .with_wire_kind("fauna.folders.delete"),
        move || {
            let machine = Arc::clone(&machine);
            let name = name_owned.clone();
            // `delete_folder` is non-cascading: a set with snapshots returns an
            // error the machine surfaces in `error-message`. On success it
            // self-refreshes so the row disappears.
            async_helper::run_on_tokio(async move { machine.delete_folder(name).await }, |_| {});
        },
    );
}

/// Present the declassify confirmation (`folder-audience-public-confirm`, phase
/// 4 slice 4d — `ui/folders.md` § Audience and website serving), then flip the
/// folder to `public` on confirm. Rides [`crate::confirm_dialog::present_confirm`], which
/// is how every arm-then-confirm on this page is shaped: tui renders its
/// confirm inline in the row body and web as an in-page overlay, each
/// mirroring its OWN delete confirm, and linux's is a dialog for the same
/// reason.
///
/// **Both consequences are stated, because each surprised readers on its own:**
/// names and paths go public too (they become the address of each file), and
/// flipping back re-seals only FUTURE content — anything published while the
/// folder was public should be treated as public for good.
///
/// `dropdown` was ALREADY snapped back to the current audience by the caller
/// before this dialog was presented (see the arm branch), which is what keeps
/// the select painting the folder's real audience for as long as the confirm is
/// unanswered. So — unlike the residency twin before this slice — there is
/// nothing to reset on cancel: nothing moved. It is still taken by reference
/// because the dialog needs an anchor widget, and the picker is the widget
/// that has one.
fn confirm_public_audience(
    dropdown: &gtk::DropDown,
    name: &str,
    machine: &Arc<DevicesMachine>,
    fauna_client: &Rc<FaunaClient>,
) {
    let machine = Arc::clone(machine);
    let fauna_client = Rc::clone(fauna_client);
    let name_owned = name.to_string();
    crate::confirm_dialog::present_confirm(
        dropdown,
        crate::confirm_dialog::ConfirmSpec::new(
            strings::devices::DECLASSIFY_TITLE,
            crate::confirm_dialog::ConfirmBody::Text(&format!(
                "{}\n\n{}",
                strings::devices::DECLASSIFY_BODY,
                strings::devices::DECLASSIFY_IRREVERSIBLE,
            )),
            "confirm",
            strings::devices::DECLASSIFY_CONFIRM,
            crate::confirm_dialog::CANCEL,
        )
        .with_confirm_id(ids::FOLDER_AUDIENCE_PUBLIC_CONFIRM)
        .with_wire_kind("fauna.folders.update"),
        move || {
            // Both outcomes self-correct through the machine's `on_changed()`:
            // a successful write refreshes and the row repaints as Public, a
            // failed one leaves the snapshot (and so the select) where it was
            // and surfaces the nest's own refusal text — which names the repair
            // ("turn off WebDAV serving first" and kin) and is the actionable
            // half.
            fauna_client.set_folder_audience(
                Arc::clone(&machine),
                &name_owned,
                fauna_folders_machine::AUDIENCE_PUBLIC,
            );
        },
    );
}

/// Present the destructive content-residency confirmation (file-sync.md §
/// Content residency — the flip to Metadata-only deletes the nest's copy of
/// the folder's content), then call `DevicesMachine::set_folder_residency`
/// with `"metadata_only"` on confirm. Rides [`crate::confirm_dialog::present_confirm`] —
/// the `folder-audience-public-confirm` shape tui documents, since linux has
/// no inline row-body confirm.
///
/// `dropdown` was ALREADY snapped back to the current residency by the caller
/// before this dialog was presented — GTK commits a `DropDown` pick
/// immediately, unlike a framework that re-renders off state, and the contract
/// is that the select keeps painting the CURRENT value until the confirm is
/// answered (`file-sync.md` § Content residency, the audience-confirm pattern).
/// So there is nothing to reset on cancel: nothing moved. It is still taken by
/// reference because the dialog needs an anchor widget, and the picker is the
/// widget that has one.
fn confirm_residency(dropdown: &gtk::DropDown, name: &str, machine: &Arc<DevicesMachine>) {
    let machine = Arc::clone(machine);
    let name_owned = name.to_string();
    crate::confirm_dialog::present_confirm(
        dropdown,
        crate::confirm_dialog::ConfirmSpec::new(
            strings::devices::RESIDENCY_CONFIRM_TITLE,
            crate::confirm_dialog::ConfirmBody::Text(strings::devices::RESIDENCY_CONFIRM_BODY),
            "confirm",
            strings::devices::RESIDENCY_CONFIRM,
            crate::confirm_dialog::CANCEL,
        )
        .with_confirm_id(ids::FOLDER_RESIDENCY_CONFIRM)
        .with_wire_kind("fauna.folders.update"),
        move || {
            let machine = Arc::clone(&machine);
            let name = name_owned.clone();
            // Both outcomes self-correct through `on_changed()` — `set_error`
            // fires it exactly like `refresh` does, so a FAILED write rebuilds
            // the row off the UNCHANGED snapshot (still Full) and a successful
            // one off the new one (Metadata-only). Touching `dropdown` here
            // would race that rebuild — the delete/paths idiom's `|_| {}`.
            async_helper::run_on_tokio(
                async move {
                    machine
                        .set_folder_residency(
                            name,
                            fauna_folders_machine::RESIDENCY_METADATA_ONLY.to_string(),
                        )
                        .await
                },
                |_| {},
            );
        },
    );
}

/// Which of a seat's three flags one `folder-place-*` checkbox owns. The flag
/// *meanings* live once, in `fauna_protocol::folders::PlaceFlags`; this pairs
/// each with its element id, its label, and the shared wire key
/// [`fauna_client_folders::folders::toggled`] takes.
///
/// The element-id suffix is hyphenated and the wire key underscored — they
/// differ for `applies-deletes` alone, and passing one where the other belongs
/// is a silent no-write (the same trap web's table calls out).
const PLACE_FLAG_BOXES: [(&str, &str, &str, PlaceRowFlag); 3] = [
    (
        ids::FOLDER_PLACE_ORIGINATES,
        strings::devices::wizard::PLACE_ORIGINATES,
        folders_wire::PLACE_FLAG_ORIGINATES,
        PlaceRowFlag::Originates,
    ),
    (
        ids::FOLDER_PLACE_ACCEPTS,
        strings::devices::wizard::PLACE_ACCEPTS,
        folders_wire::PLACE_FLAG_ACCEPTS,
        PlaceRowFlag::Accepts,
    ),
    (
        ids::FOLDER_PLACE_APPLIES_DELETES,
        strings::devices::wizard::PLACE_APPLIES_DELETES,
        folders_wire::PLACE_FLAG_APPLIES_DELETES,
        PlaceRowFlag::AppliesDeletes,
    ),
];

/// Which field of a projected [`PlaceRow`] a checkbox paints. Read-only: the
/// *write* is composed by the shared `toggled`, never by flipping a field here.
#[derive(Clone, Copy)]
enum PlaceRowFlag {
    Originates,
    Accepts,
    AppliesDeletes,
}

impl PlaceRowFlag {
    fn read(self, row: &PlaceRow) -> bool {
        match self {
            Self::Originates => row.originates,
            Self::Accepts => row.accepts,
            Self::AppliesDeletes => row.applies_deletes,
        }
    }
}

/// Fill in the device-place editor for the expander row whose title equals
/// `name`, once `fauna.folders.members.list` returns — one `folder-place-row`
/// per enrolled seat, each carrying the same three flag checkboxes the wizard's
/// enrollment step paints, edited in place through `fauna.folders.places.set`
/// (`docs/goal/ui/folders.md` § Implementation status today). An "empty" note
/// stands in when the set has no enrolled devices yet.
///
/// The rows come from the shared `place_rows` projection — the one
/// flags-only rule, which this app must not re-derive — and
/// a toggle writes the WHOLE point (composed by the shared `toggled`) and then
/// RE-READS the roster, so the boxes settle on the nest's answer rather than an
/// optimistic local flip.
pub fn populate_folder_members(
    list_box: &gtk::ListBox,
    name: &str,
    members: &[FolderMember],
    machine: &Arc<DevicesMachine>,
    fauna_client: &Rc<FaunaClient>,
) {
    let Some(members_box) =
        find_tagged_widget(list_box, name, MEMBERS_BOX).and_then(|w| w.downcast::<gtk::Box>().ok())
    else {
        return;
    };
    while let Some(child) = members_box.first_child() {
        members_box.remove(&child);
    }

    if members.is_empty() {
        let empty = gtk::Label::builder()
            .label(strings::devices::NO_DEVICES_ENROLLED)
            .halign(gtk::Align::Start)
            .css_classes(["dim-label", "caption"])
            .build();
        members_box.append(&empty);
        return;
    }

    let heading = gtk::Label::builder()
        .label(strings::devices::FOLDER_PLACES_TITLE)
        .halign(gtk::Align::Start)
        .css_classes(["heading", "caption"])
        .build();
    members_box.append(&heading);

    // One shared projection, not a per-app read of the roster: `place_rows`
    // applies the flags-only rule, marks the seat neither
    // door could read, and names each seat from the machine's unsealed device
    // roster (`members.list` carries no plaintext name for a user-named device).
    // The indices it hands back ARE the e2e addresses (`folder-place-row[j]`),
    // so this must not filter or re-sort them.
    let devices = machine.snapshot().devices;
    for place in fauna_devices_machine::place_rows(members, &devices) {
        let item = gtk::Box::new(gtk::Orientation::Vertical, 2);
        // Plain Boxes default to AT-SPI role Generic, which the Linux bridge can
        // omit from the tree — make the indexed row discoverable, and set the
        // role BEFORE the id (mirrors `folder-device-activity-item`).
        item.set_accessible_role(gtk::AccessibleRole::Group);
        set_test_id(&item, ids::FOLDER_PLACE_ROW);

        let label_lbl = gtk::Label::builder()
            .label(&place.label)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .css_classes(["caption"])
            .build();
        item.append(&label_lbl);

        for (id, label, wire, kind) in PLACE_FLAG_BOXES {
            let on = kind.read(&place);
            let flag_box = gtk::CheckButton::with_label(label);
            flag_box.set_margin_start(12);
            // Active BEFORE the handler is connected: `set_active` fires
            // `connect_toggled` too, and this initial paint is not a gesture.
            flag_box.set_active(on);
            set_test_id(&flag_box, id);
            // The `get_attr(id, "state")` contract every flag checkbox
            // carries. Without the explicit marker the agent falls back to
            // the widget's live state and answers `"true"`/`"false"`, which
            // is not the `"on"`/`"off"` the cross-app test asserts.
            crate::testid::set_test_attr(&flag_box, "state", if on { "on" } else { "off" });
            crate::offline_gate::declare_wire_kind(&flag_box, "fauna.folders.places.set");
            {
                let machine = Arc::clone(machine);
                let client = Rc::clone(fauna_client);
                let folder = name.to_string();
                let place = place.clone();
                flag_box.connect_toggled(move |cb| {
                    if cb.is_active() == kind.read(&place) {
                        return; // a repaint echo, not a user gesture
                    }
                    // The point applies WHOLE — `toggled` composes it, so
                    // the two boxes the user did not touch cannot be dropped
                    // on the way to the wire. `None` means an unknown flag
                    // id, and there is no partial edit to fall back to.
                    let Some(next) = folders_wire::toggled(&place, wire) else {
                        return;
                    };
                    let machine = Arc::clone(&machine);
                    let client = Rc::clone(&client);
                    let folder_write = folder.clone();
                    let folder_reread = folder.clone();
                    async_helper::run_on_tokio(
                        async move {
                            machine
                                .set_folder_place(
                                    folder_write,
                                    next.device_id,
                                    next.originates,
                                    next.accepts,
                                    next.applies_deletes,
                                )
                                .await
                        },
                        // Repaint from the NEST, never the local flip: the
                        // per-seat flags are not on the page snapshot the
                        // write already refreshed, so the roster read is the
                        // only thing that can answer. A failed write lands
                        // here too, and re-reads back to the unchanged truth.
                        move |()| client.fetch_folder_members(&folder_reread),
                    );
                });
            }
            item.append(&flag_box);
        }

        members_box.append(&item);
    }
}

/// Fill in the per-set **device-activity** roster for the expander row whose
/// title equals `name`, once `fauna.folders.devices` returns — on first
/// expand AND on every subsequent `fauna.sync.changed` push while the row stays
/// expanded (`app.rs`'s `PushEvent::SyncChanged` arm calls
/// `FaunaClient::fetch_folder_devices` again, gated on
/// [`expanded_folder_row_where`] so a collapsed row's roster is never wastefully
/// re-fetched). That live-update path is the entire point of this feature: it
/// is what makes web's remote-change nudge e2e-pinnable
/// (`docs/goal/behavior/file-sync.md` § Implementation status today).
///
/// Replaces the loading placeholder with one `folder-device-activity-item` row
/// per device (`folder-device-activity-label` + `folder-device-activity-count`),
/// or `NO_DEVICE_ACTIVITY` when nothing has been recorded yet. Same shape as
/// [`populate_folder_members`], just re-runnable in place — this one repaints
/// on every call rather than only once, since a push can fire many times while
/// the row stays open.
pub fn populate_folder_devices(
    list_box: &gtk::ListBox,
    name: &str,
    devices: &[fauna_client_folders::folders::FolderDevice],
) {
    let Some(devices_box) = find_tagged_widget(list_box, name, DEVICE_ACTIVITY_BOX)
        .and_then(|w| w.downcast::<gtk::Box>().ok())
    else {
        return;
    };
    while let Some(child) = devices_box.first_child() {
        devices_box.remove(&child);
    }

    if devices.is_empty() {
        let empty = gtk::Label::builder()
            .label(strings::devices::NO_DEVICE_ACTIVITY)
            .halign(gtk::Align::Start)
            .css_classes(["dim-label", "caption"])
            .build();
        devices_box.append(&empty);
        return;
    }

    for d in devices {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // Plain Boxes default to AT-SPI role Generic, which the Linux bridge can
        // omit from the tree — make the indexed item discoverable (mirrors
        // `folder-member-item` / `media-item`).
        item.set_accessible_role(gtk::AccessibleRole::Group);
        set_test_id(&item, ids::FOLDER_DEVICE_ACTIVITY_ITEM);

        let label_lbl = gtk::Label::builder()
            .label(&d.label)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["caption"])
            .build();
        set_test_id(&label_lbl, ids::FOLDER_DEVICE_ACTIVITY_LABEL);
        item.append(&label_lbl);

        // "Changes" caption — declares what the number means; the inline peer
        // of web's `<th>{t.devices.col_changes}</th>` column header (GTK has no
        // table here, so the caption rides per-row instead of a shared header).
        let changes_caption = gtk::Label::builder()
            .label(strings::devices::COL_CHANGES)
            .css_classes(["dim-label", "caption"])
            .build();
        item.append(&changes_caption);

        let count_lbl = gtk::Label::builder()
            .label(d.change_count.to_string())
            .css_classes(["caption"])
            .build();
        set_test_id(&count_lbl, ids::FOLDER_DEVICE_ACTIVITY_COUNT);
        item.append(&count_lbl);

        devices_box.append(&item);
    }
}

/// Build the per-set "Device activity" section: a header + the roster box
/// (`DEVICE_ACTIVITY_BOX`) `populate_folder_devices` fills in once
/// `fauna.folders.devices` returns. Owner rows only (mirrors the member
/// roster and "Shared with" section above/below it) — a writer-member row
/// ([`build_writer_member_folder_row`]) carries none of these row-detail
/// reads, only the folder-binding widget.
fn build_device_activity_section() -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();

    let title = gtk::Label::builder()
        .label(strings::devices::DEVICE_ACTIVITY)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    section.append(&title);

    let devices_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    devices_box.set_widget_name(DEVICE_ACTIVITY_BOX);
    let loading = gtk::Label::builder()
        .label(strings::common::LOADING)
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    devices_box.append(&loading);
    section.append(&devices_box);

    section
}

/// Build the (initially hidden) destination-places section — filled in by
/// [`populate_folder_destinations`] once `fauna.backup.destination.list`
/// returns for this row's `folder_id`. Starts empty/hidden rather than a
/// loading placeholder: the common case (no destination enrolled at all)
/// must paint nothing, and a placeholder would just have to be un-painted
/// again on arrival for that case anyway.
fn build_destination_places_section() -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .visible(false)
        .build();
    section.set_widget_name(DESTINATION_PLACES_BOX);
    section
}

/// Fill in the per-set **destination places** for the expander row whose
/// title equals `name`, once `fauna.backup.destination.list` returns (on
/// first expand, and again after every attach/detach — the section always
/// repaints from the nest's own answer, never an optimistic flip, mirroring
/// tui's `Outcome::FolderDestinationsLoaded` posture). Renders one
/// `folder-destination-row` per ATTACHED destination (label + detach
/// button), then — while at least one enrolled destination remains
/// unattached — the `folder-destination-attach-select` +
/// `folder-destination-attach-button` pair. `places.is_empty()` (no
/// destination enrolled at all) hides the whole section: an affordance that
/// cannot work must not paint (`backup-destinations.md` § Ordinary-folder
/// coverage).
pub fn populate_folder_destinations(
    list_box: &gtk::ListBox,
    name: &str,
    folder_id: i64,
    places: &[fauna_client_config::FolderDestinationPlace],
    client: &Rc<FaunaClient>,
) {
    let Some(section) = find_tagged_widget(list_box, name, DESTINATION_PLACES_BOX)
        .and_then(|w| w.downcast::<gtk::Box>().ok())
    else {
        return;
    };
    while let Some(child) = section.first_child() {
        section.remove(&child);
    }

    if places.is_empty() {
        section.set_visible(false);
        return;
    }
    section.set_visible(true);

    let title = gtk::Label::builder()
        .label(strings::devices::FOLDER_DESTINATIONS_TITLE)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    section.append(&title);

    for place in places.iter().filter(|p| p.attached) {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // Plain Boxes default to AT-SPI role Generic, which the Linux bridge
        // can omit from the tree — make the indexed item discoverable
        // (mirrors `folder-member-item` / `folder-device-activity-item`).
        item.set_accessible_role(gtk::AccessibleRole::Group);
        set_test_id(&item, ids::FOLDER_DESTINATION_ROW);

        let label_lbl = gtk::Label::builder()
            .label(&place.label)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["caption"])
            .build();
        item.append(&label_lbl);

        let detach_btn = gtk::Button::builder()
            .label(strings::devices::FOLDER_DESTINATION_DETACH)
            .css_classes(["flat", "destructive-action"])
            .build();
        set_test_id(&detach_btn, ids::FOLDER_DESTINATION_DETACH_BUTTON);
        crate::offline_gate::declare_wire_kind(
            &detach_btn,
            "fauna.backup.destination.detach_folder",
        );
        {
            let client = Rc::clone(client);
            let name = name.to_string();
            let destination_id = place.destination_id.clone();
            // Always `Some` on an attached row (the attach reply's own
            // read-back) — the detach sequence's config-row key, never
            // re-derived here.
            let folder_set = place.folder_set.clone().unwrap_or_default();
            detach_btn.connect_clicked(move |_| {
                client.detach_folder_destination(&name, folder_id, &destination_id, &folder_set);
            });
        }
        item.append(&detach_btn);

        section.append(&item);
    }

    let attachable: Vec<&fauna_client_config::FolderDestinationPlace> =
        places.iter().filter(|p| !p.attached).collect();
    if attachable.is_empty() {
        return;
    }

    let attach_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let destination_ids: Vec<String> = attachable
        .iter()
        .map(|p| p.destination_id.clone())
        .collect();
    let id_refs: Vec<&str> = destination_ids.iter().map(String::as_str).collect();
    // Same value/label DropDown split as `folder-conflict-policy-select`: the
    // model strings are the wire values `select(id, value)` drives (here, the
    // opaque `destination_id`s themselves), the expression only re-maps each
    // to its display label.
    let labels: std::collections::HashMap<String, String> = attachable
        .iter()
        .map(|p| (p.destination_id.clone(), p.label.clone()))
        .collect();
    let attach_dropdown = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&id_refs))
        .build();
    let label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        gtk::glib::closure!(move |item: gtk::StringObject| {
            let id = item.string();
            labels
                .get(id.as_str())
                .cloned()
                .unwrap_or_else(|| id.to_string())
        }),
    );
    attach_dropdown.set_expression(Some(&label_expr));
    set_test_id(&attach_dropdown, ids::FOLDER_DESTINATION_ATTACH_SELECT);
    attach_row.append(&attach_dropdown);

    let attach_btn = gtk::Button::builder()
        .label(strings::devices::FOLDER_DESTINATION_ATTACH)
        .css_classes(["flat"])
        .build();
    set_test_id(&attach_btn, ids::FOLDER_DESTINATION_ATTACH_BUTTON);
    crate::offline_gate::declare_wire_kind(&attach_btn, "fauna.backup.destination.attach_folder");
    {
        let client = Rc::clone(client);
        let name = name.to_string();
        let dropdown = attach_dropdown.clone();
        attach_btn.connect_clicked(move |_| {
            let Some(destination_id) = destination_ids.get(dropdown.selected() as usize) else {
                return;
            };
            client.attach_folder_destination(&name, folder_id, destination_id);
        });
    }
    attach_row.append(&attach_btn);
    section.append(&attach_row);
}

/// The inputs of the `folder-writer-published-warning` decision for ONE set — its
/// NORMALIZED audience and whether it is paywalled — captured from the machine's
/// snapshot at the moment a surface is built (`docs/goal/ui/folders.md` § Sharing).
///
/// The reach test is the shared `fauna_folders_machine::writer_grant_reach`
/// (`public` or paywalled ⇒ a writer changes what people OUTSIDE the set read);
/// this only feeds it and resolves the sentence it picks, so linux never
/// re-derives which sentence applies. State-based, not event-based: the list is
/// rebuilt from the snapshot on every change and an expanded row re-reads its
/// roster, so a grant-then-publish and a publish-then-grant both land here with
/// the current audience. The audience is normalized exactly as the audience select
/// is, so an unparseable column can never claim the set is world-readable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterReach {
    audience: String,
    paywalled: bool,
}

impl WriterReach {
    /// The inputs for `fs` as it stands in this snapshot.
    pub fn of(fs: &FolderSummary) -> Self {
        Self {
            audience: fauna_folders_machine::normalize_audience(
                &fs.audience,
                fs.mls_group_id.is_some(),
            ),
            paywalled: fs.web_paywall_tier.is_some(),
        }
    }

    /// The inputs for the set called `name` in the machine's CURRENT snapshot. A
    /// set the snapshot no longer lists reaches nobody — the fail-closed reading
    /// (no warning is the quiet direction; a wrong "this is public" is not).
    pub fn for_set(machine: &DevicesMachine, name: &str) -> Self {
        machine
            .snapshot()
            .folders
            .iter()
            .find(|f| f.name == name)
            .map(Self::of)
            .unwrap_or(Self {
                audience: "private".to_string(),
                paywalled: false,
            })
    }

    /// The warning sentence for a grant of `access`, or `None` when the grant
    /// reaches nobody outside the set (a reader; an unpublished set).
    pub fn warning(&self, access: &str) -> Option<String> {
        fauna_folders_machine::writer_grant_reach(access, &self.audience, self.paywalled)
            .map(|text| text.resolve(strings::lookup))
    }

    /// A `folder-writer-published-warning` label for a grant of `access` — hidden
    /// (but present, like the uncapped warning beside it) when there is nothing to
    /// say; [`Self::refresh`] repaints it when the picked access changes.
    fn label(&self, access: &str) -> gtk::Label {
        let label = gtk::Label::builder()
            .css_classes(["warning", "caption"])
            .wrap(true)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .build();
        set_test_id(&label, ids::FOLDER_WRITER_PUBLISHED_WARNING);
        self.refresh(&label, access);
        label
    }

    /// Repaint `label` for a grant of `access`.
    fn refresh(&self, label: &gtk::Label, access: &str) {
        match self.warning(access) {
            Some(text) => {
                label.set_label(&text);
                label.set_visible(true);
            }
            None => label.set_visible(false),
        }
    }
}

/// Build the owner-side "Shared with" section for an expanded `folder-row`
/// (`docs/goal/ui/folders.md` § Sharing): a header + `folder-share-button`, and
/// the actor-roster box (`SHARED_WITH_BOX`) filled by `populate_folder_actors`
/// after the async `members.list_actors` read.
fn build_shared_with_section(
    name: &str,
    machine: &Arc<DevicesMachine>,
    fauna_client: &Rc<FaunaClient>,
) -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::builder()
        .label(strings::devices::SHARED_WITH)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["dim-label", "caption"])
        .build();
    header.append(&title);

    let share_btn = gtk::Button::builder()
        .label(strings::devices::SHARE_BUTTON)
        .css_classes(["flat"])
        .build();
    set_test_id(&share_btn, ids::FOLDER_SHARE_BUTTON);
    {
        let name = name.to_string();
        let machine = Arc::clone(machine);
        let client = Rc::clone(fauna_client);
        share_btn.connect_clicked(move |btn| open_share_dialog(btn, &name, &machine, &client));
    }
    header.append(&share_btn);
    section.append(&header);

    // Actor roster — filled by `populate_folder_actors` after the async read.
    let shared_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    shared_box.set_widget_name(SHARED_WITH_BOX);
    section.append(&shared_box);

    section
}

/// Open the share dialog: the REUSED `RecipientPicker` (priority #2 — no new picker
/// IDs) inside a confirm dialog whose Share button is `folder-share-confirm`. On
/// confirm, resolve the typed bare handle + share the set
/// (`FaunaClient::share_folder`).
fn open_share_dialog(
    btn: &gtk::Button,
    name: &str,
    machine: &Arc<DevicesMachine>,
    client: &Rc<FaunaClient>,
) {
    let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let picker = RecipientPicker::new(|_text| {}, || {});

    #[allow(deprecated)]
    let dialog =
        adw::MessageDialog::new(parent.as_ref(), Some(strings::devices::SHARE_BUTTON), None);

    // Recipient picker + the share-time access grant (multi-writer Phase 1;
    // folders.md § Sharing): `folder-share-role-select` (Reader default)
    // from the shared `member_access_options()` catalog, plus the
    // uncapped-writer warning — a share-time writer grant carries no cap, so
    // picking Writer always shows it (advisory; the owner can set a cap on the
    // member row afterwards).
    let container = gtk::Box::new(gtk::Orientation::Vertical, 8);
    container.append(&picker.root);
    let access_opts = fauna_folders_machine::member_access_options();
    let access_values: Vec<String> = access_opts.iter().map(|o| o.value.clone()).collect();
    let access_value_refs: Vec<&str> = access_values.iter().map(String::as_str).collect();
    let role_dd = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&access_value_refs))
        .build();
    let access_label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        gtk::glib::closure!(|item: gtk::StringObject| {
            fauna_folders_machine::member_access_label(item.string().as_str())
                .resolve(strings::lookup)
        }),
    );
    role_dd.set_expression(Some(&access_label_expr));
    set_test_id(&role_dd, ids::FOLDER_SHARE_ROLE_SELECT);
    let role_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let role_lbl = gtk::Label::builder()
        .label(strings::devices::MEMBER_ACCESS)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();
    role_row.append(&role_lbl);
    role_row.append(&role_dd);
    container.append(&role_row);
    let warning_lbl = gtk::Label::builder()
        .label(strings::devices::WRITER_UNCAPPED_WARNING)
        .css_classes(["warning", "caption"])
        .wrap(true)
        .visible(false)
        .build();
    set_test_id(&warning_lbl, ids::FOLDER_WRITER_UNCAPPED_WARNING);
    container.append(&warning_lbl);
    // The published-folder writer warning — the SAME state-based decision the
    // member row asks, read from the set's audience as it stands NOW (the dialog
    // is modal, so it cannot change under it) and stacked with the quota warning
    // above rather than replacing it.
    let reach = WriterReach::for_set(machine, name);
    let published_lbl = reach.label("reader");
    container.append(&published_lbl);
    {
        let access_values = access_values.clone();
        let warning_lbl = warning_lbl.clone();
        let published_lbl = published_lbl.clone();
        role_dd.connect_selected_notify(move |dd| {
            let access = access_values
                .get(dd.selected() as usize)
                .map(String::as_str)
                .unwrap_or("reader");
            warning_lbl.set_visible(access == "writer");
            reach.refresh(&published_lbl, access);
        });
    }
    dialog.set_extra_child(Some(&container));
    #[allow(deprecated)]
    {
        dialog.add_response("cancel", strings::common::CANCEL);
        dialog.add_response("share", strings::devices::SHARE_BUTTON);
        dialog.set_response_appearance("share", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("share"));
        dialog.set_close_response("cancel");
    }

    // Tag the Share response button with the e2e id once the button tree is
    // materialised (same idiom as `confirm_delete`'s `folder-delete-confirm`).
    let dialog_for_id = dialog.clone();
    glib::idle_add_local_once(move || {
        crate::testid::tag_response_button(
            dialog_for_id.upcast_ref::<gtk::Widget>(),
            strings::devices::SHARE_BUTTON,
            ids::FOLDER_SHARE_CONFIRM,
        );
        // The share ceremony's declared leg is the roster write it exists to
        // make (`share_set` → `fauna.folders.share`), and it is the CONFIRM's
        // gesture: `folder-share-button` opens this dialog, and
        // `folder-share-role-select` inside it only stages the access value.
        if let Some(confirm) = crate::testid::find_by_test_id(
            dialog_for_id.upcast_ref::<gtk::Widget>(),
            ids::FOLDER_SHARE_CONFIRM,
        )
        .and_then(|w| w.downcast::<gtk::Button>().ok())
        {
            crate::offline_gate::declare_wire_kind(&confirm, "fauna.folders.share");
        }
    });

    let client = Rc::clone(client);
    let machine = Arc::clone(machine);
    let name = name.to_string();
    #[allow(deprecated)]
    dialog.connect_response(None, move |_dialog, response| {
        if response == "share" {
            let handle = picker.input.text().trim().to_string();
            if !handle.is_empty() {
                // The picked share-time grant: `None` for the Reader default
                // (absent role row = reader), `Some("writer")` for read-write.
                let access = access_values
                    .get(role_dd.selected() as usize)
                    .filter(|v| v.as_str() == "writer")
                    .cloned();
                client.share_folder(Arc::clone(&machine), &name, &handle, access);
            }
        }
    });

    dialog.present();
}

/// Fill the cross-user "Shared with" roster for the expander row titled `name`
/// after `fauna.folders.members.list_actors` returns, and update the
/// `folder-shared-badge`. Renders each `role == "member"` actor as a
/// `folder-member-item` (`folder-member-handle` + `folder-member-status`
/// "Active" + `folder-member-remove-button`). `channel_id` (the set's derived
/// `ChannelId`, hex) backs the remove buttons; empty/`None` ⇒ owner-only (no
/// members, badge hidden). "Pending" is a future optimistic state — the nest read
/// reports only actors the share reached, so every returned member reads "Active".
pub fn populate_folder_actors(
    list_box: &gtk::ListBox,
    name: &str,
    members: &[FolderActorMember],
    channel_id: Option<&str>,
    client: &Rc<FaunaClient>,
    reach: &WriterReach,
) {
    // Only actors with role "member" render (the owner isn't listed) — the
    // shared derivation site backs both the count and the render loop below.
    let shared_with = fauna_client_folders::member_actors(members);
    let member_count = shared_with.len();

    // Badge (header suffix) — "Shared · N" when shared, hidden otherwise.
    if let Some(badge) = find_tagged_widget(list_box, name, SHARED_BADGE_ID)
        .and_then(|w| w.downcast::<gtk::Label>().ok())
    {
        if member_count > 0 {
            badge.set_label(&strings::devices::shared_badge(&member_count.to_string()));
            badge.set_visible(true);
        } else {
            badge.set_visible(false);
        }
    }

    let Some(shared_box) = find_tagged_widget(list_box, name, SHARED_WITH_BOX)
        .and_then(|w| w.downcast::<gtk::Box>().ok())
    else {
        return;
    };
    while let Some(child) = shared_box.first_child() {
        shared_box.remove(&child);
    }

    if member_count == 0 {
        let empty = gtk::Label::builder()
            .label(strings::devices::NOT_SHARED_YET)
            .halign(gtk::Align::Start)
            .css_classes(["dim-label", "caption"])
            .build();
        shared_box.append(&empty);
        return;
    }

    for m in shared_with {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // Plain Boxes default to AT-SPI role Generic, which the Linux bridge can
        // omit from the tree — make the indexed item discoverable (mirrors
        // `media-item`, `views/media/item.rs`).
        item.set_accessible_role(gtk::AccessibleRole::Group);
        set_test_id(&item, ids::FOLDER_MEMBER_ITEM);

        // The canonical handle-else-`short_id` rule (`value-formatting.md`
        // § Account display label) — never re-derive it here: the local
        // fallback this replaces rendered the raw 64-hex actor id.
        let display = fauna_core::format::account_display_label(Some(&m.handle), &m.actor_id);
        let handle_lbl = gtk::Label::builder()
            .label(&display)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["caption"])
            .build();
        set_test_id(&handle_lbl, ids::FOLDER_MEMBER_HANDLE);
        item.append(&handle_lbl);

        let status_lbl = gtk::Label::builder()
            .label(strings::common::ACTIVE)
            .css_classes(["dim-label", "caption"])
            .build();
        set_test_id(&status_lbl, ids::FOLDER_MEMBER_STATUS);
        item.append(&status_lbl);

        // ── Per-member access (multi-writer Phase 1; folders.md § Sharing) ──
        // Role select + byte-cap input + the uncapped-writer warning, all
        // driven by the shared `member_access_options()` catalog and the nest's
        // authoritative role row (`m.access` / `m.byte_cap`; absent = reader).
        // Same value/label DropDown split as `folder-conflict-policy-select`:
        // the MODEL strings are the wire values the `select(id, value)` e2e
        // contract drives.
        let access_opts = fauna_folders_machine::member_access_options();
        let access_values: Vec<String> = access_opts.iter().map(|o| o.value.clone()).collect();
        let access_value_refs: Vec<&str> = access_values.iter().map(String::as_str).collect();
        let role_dd = gtk::DropDown::builder()
            .model(&gtk::StringList::new(&access_value_refs))
            .build();
        let access_label_expr = gtk::ClosureExpression::new::<String>(
            &[] as &[gtk::Expression],
            gtk::glib::closure!(|item: gtk::StringObject| {
                fauna_folders_machine::member_access_label(item.string().as_str())
                    .resolve(strings::lookup)
            }),
        );
        role_dd.set_expression(Some(&access_label_expr));
        set_test_id(&role_dd, ids::FOLDER_MEMBER_ROLE_SELECT);
        crate::offline_gate::declare_wire_kind(&role_dd, "fauna.folders.members.set_access");
        let current_access = m.access.as_deref().unwrap_or("reader");
        if let Some(idx) = access_values.iter().position(|v| v == current_access) {
            role_dd.set_selected(idx as u32);
        }
        item.append(&role_dd);

        let cap_entry = gtk::Entry::builder()
            .placeholder_text(strings::devices::MEMBER_BYTE_CAP_PLACEHOLDER)
            .width_chars(10)
            .build();
        set_test_id(&cap_entry, ids::FOLDER_MEMBER_CAP_INPUT);
        // Not a staging field HERE: activating the entry commits the whole
        // (access, cap) pair through the same `set_access` upsert the role
        // select uses (tui carries the cap as a text buffer and commits it
        // through its role gesture instead, so it has no kind to declare).
        crate::offline_gate::declare_wire_kind(&cap_entry, "fauna.folders.members.set_access");
        if let Some(cap) = m.byte_cap {
            cap_entry.set_text(&cap.to_string());
        }
        item.append(&cap_entry);

        let warning_lbl = gtk::Label::builder()
            .label(strings::devices::WRITER_UNCAPPED_WARNING)
            .css_classes(["warning", "caption"])
            .visible(current_access == "writer" && m.byte_cap.is_none())
            .build();
        set_test_id(&warning_lbl, ids::FOLDER_WRITER_UNCAPPED_WARNING);
        item.append(&warning_lbl);

        // The published-folder writer warning — independent of the cap (a byte
        // cap bounds the owner's quota, not what the writer can change for the
        // people outside the set) and stacked with the quota warning above. The
        // shared decision picks the sentence; only the picked access moves it.
        let published_lbl = reach.label(current_access);
        item.append(&published_lbl);

        // Both edits write the full (access, cap) pair — `set_access` upserts
        // the whole row, so sending one half would silently clear the other.
        {
            let client = Rc::clone(client);
            let name = name.to_string();
            let member_hex = m.actor_id.clone();
            let channel = channel_id.map(str::to_string);
            let access_values = access_values.clone();
            let cap_entry_for_role = cap_entry.clone();
            let warning_for_role = warning_lbl.clone();
            let published_for_role = published_lbl.clone();
            let reach = reach.clone();
            role_dd.connect_selected_notify(move |dd| {
                let Some(value) = access_values.get(dd.selected() as usize) else {
                    return;
                };
                let cap = fauna_core::format::parse_count_i64(&cap_entry_for_role.text());
                warning_for_role.set_visible(value == "writer" && cap.is_none());
                reach.refresh(&published_for_role, value);
                client.set_folder_member_access(&name, channel.as_deref(), &member_hex, value, cap);
            });
        }
        {
            let client = Rc::clone(client);
            let name = name.to_string();
            let member_hex = m.actor_id.clone();
            let channel = channel_id.map(str::to_string);
            let access_values = access_values.clone();
            let role_dd_for_cap = role_dd.clone();
            let warning_for_cap = warning_lbl.clone();
            cap_entry.connect_activate(move |entry| {
                let cap = fauna_core::format::parse_count_i64(&entry.text());
                let access = access_values
                    .get(role_dd_for_cap.selected() as usize)
                    .cloned()
                    .unwrap_or_else(|| "reader".to_string());
                warning_for_cap.set_visible(access == "writer" && cap.is_none());
                client.set_folder_member_access(
                    &name,
                    channel.as_deref(),
                    &member_hex,
                    &access,
                    cap,
                );
            });
        }

        let remove_btn = gtk::Button::builder()
            .label(strings::common::REMOVE)
            .css_classes(["flat", "destructive-action"])
            .build();
        set_test_id(&remove_btn, ids::FOLDER_MEMBER_REMOVE_BUTTON);
        // The roster write the eviction exists to make; the MLS evict + content
        // key rotation `remove_member` runs beside it are group work, not a kind.
        crate::offline_gate::declare_wire_kind(&remove_btn, "fauna.folders.members.remove");
        match channel_id {
            Some(cid) => {
                let name = name.to_string();
                let cid = cid.to_string();
                let member_hex = m.actor_id.clone();
                let client = Rc::clone(client);
                remove_btn.connect_clicked(move |_| {
                    client.remove_folder_member(&name, &cid, &member_hex);
                });
            }
            // No channel id ⇒ can't address the set for removal (shouldn't happen
            // while members are present); leave the button inert.
            None => remove_btn.set_sensitive(false),
        }
        item.append(&remove_btn);

        shared_box.append(&item);
    }
}

// ── Recipient-side "Shared with you" pending-share section ───────────────────
//
// A page-level section (like the conflicts group) listing the staged ("knocked")
// cross-user folder shares a *stranger* sent, awaiting accept/decline
// (`folders.md` § Sharing — Recipient side: "the share lands as a
// `folder-pending-share` … you `folder-share-accept-button` or
// `folder-share-decline-button`"). A *contact's* share auto-joins off the chat
// rail (the `NestFolderGate`, wired in `conv_backend`) and never appears here.
// Fed by `FaunaClient::fetch_folder_pending_shares` → `FolderPendingSharesLoaded`
// → [`populate_pending_shares`] (not `DevicesMachine` state — a page-level read,
// like the owner-side actor roster).

/// Build the page-level **"Sync defaults"** section — today one control, the
/// global default conflict policy for NEW folders
/// (`sync-default-conflict-policy-select`; file-sync.md § Conflicts, policy;
/// user-approved home 2026-07-11). Persisted in `fauna.state.sync-prefs` via the
/// shared `preference_surfaces` (the muted-words direct idiom —
/// no machine earns its keep for a one-field seam); the wizard stamps the value
/// onto creates via `FolderWizardMachine::set_default_conflict_policy`.
/// Existing sets are untouched — each row's `folder-conflict-policy-select`
/// stays authoritative.
///
/// Loads on every map (page nav) and saves on change; a save failure surfaces
/// on the section subtitle (the page `error-message` belongs to the
/// `DevicesMachine`, which doesn't own this preference).
/// `client` is `None` only in widget unit tests, which assert the ui.yaml IDs
/// this group exposes and have no authenticated `FaunaClient` to give it. The
/// widget tree is identical either way; without a client the load-on-map and
/// save-on-change handlers are simply not wired (there is nothing to talk to).
pub fn build_sync_defaults_section(client: Option<&Rc<FaunaClient>>) -> adw::PreferencesGroup {
    // Values + labels from the shared `conflict_policy_options()` catalog —
    // the same source as the per-set select above and every app's picker.
    let policy_opts = fauna_folders_machine::conflict_policy_options();
    let policy_values: Vec<String> = policy_opts.iter().map(|o| o.value.clone()).collect();
    let policy_value_refs: Vec<&str> = policy_values.iter().map(String::as_str).collect();
    let dropdown = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&policy_value_refs))
        .valign(gtk::Align::Center)
        .build();
    let label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        gtk::glib::closure!(|item: gtk::StringObject| {
            fauna_folders_machine::conflict_policy_label(item.string().as_str())
                .resolve(strings::lookup)
        }),
    );
    dropdown.set_expression(Some(&label_expr));
    set_test_id(&dropdown, ids::SYNC_DEFAULT_CONFLICT_POLICY_SELECT);
    // The account-wide default lives in `fauna.state.sync-prefs`, so this one is
    // the plane write — unlike the per-set select above, which is a folder-row
    // column (`folders.update`).
    crate::offline_gate::declare_wire_kind(&dropdown, "fauna.account.state.put");

    let row = adw::ActionRow::builder()
        .title(strings::devices::DEFAULT_CONFLICT_POLICY)
        .build();
    row.add_suffix(&dropdown);
    let group = adw::PreferencesGroup::builder()
        .title(strings::devices::SYNC_DEFAULTS)
        .build();
    group.add(&row);

    // True while a programmatic `set_selected` runs, so the change handler
    // never echoes a load back as a save (the conflict select's
    // connect-after-preselect rule can't apply here — the load is async).
    let updating = Rc::new(Cell::new(false));

    // Load the persisted preference on every map (page nav). Absent (None — no
    // preference) renders as "auto", the equivalent outcome for new sets.
    if let Some(client) = client {
        let rt = client.runtime_handle();
        let dropdown = dropdown.clone();
        let updating = Rc::clone(&updating);
        let policy_values = policy_values.clone();
        group.connect_map(move |_| {
            let store = crate::account_runtime::handle_source();
            let dropdown = dropdown.clone();
            let updating = Rc::clone(&updating);
            let policy_values = policy_values.clone();
            async_helper::spawn_with_snapshot(
                &rt,
                move || async move { load_default_conflict_policy(store).await },
                move |loaded| {
                    let value = loaded.unwrap_or(None).unwrap_or_else(|| "auto".into());
                    if let Some(idx) = policy_values.iter().position(|v| *v == value) {
                        updating.set(true);
                        dropdown.set_selected(idx as u32);
                        updating.set(false);
                    }
                },
            );
        });
    }

    // Save on change.
    if let Some(client) = client {
        let rt = client.runtime_handle();
        let updating = Rc::clone(&updating);
        dropdown.connect_selected_notify(move |dd| {
            if updating.get() {
                return;
            }
            let Some(value) = policy_values.get(dd.selected() as usize) else {
                return;
            };
            let store = crate::account_runtime::handle_source();
            let value = value.to_string();
            async_helper::spawn_with_snapshot(
                &rt,
                move || async move { save_default_conflict_policy(store, value).await },
                |result: Result<(), String>| {
                    if let Err(e) = result {
                        tracing::warn!("save default conflict policy: {e}");
                    }
                },
            );
        });
    }

    group
}

// Thin error-bridging over the shared
// `fauna_sync_engine::preference_surfaces::{load,save}_sync_prefs`, over the
// account store (waited for when the page opens before the assembly lands;
// `config-dissolution.md` § The `__config` dissolution schedule → *The closure
// order*, steps (1) and (5)); this used to be one of sync-prefs' four
// hand-written copies (priority #4 — resolve drift, don't match it).
pub(super) async fn load_default_conflict_policy(
    store: fauna_sync_engine::account_runtime::SeatAccountStore,
) -> Result<Option<String>, String> {
    fauna_sync_engine::preference_surfaces::load_sync_prefs(&store)
        .await
        .map_err(fauna_sync_engine::preference_surfaces::plane_failure)
}

async fn save_default_conflict_policy(
    store: fauna_sync_engine::account_runtime::SeatAccountStore,
    value: String,
) -> Result<(), String> {
    fauna_sync_engine::preference_surfaces::save_sync_prefs(&store, Some(&value))
        .await
        .map_err(fauna_sync_engine::preference_surfaces::plane_failure)?;
    Ok(())
}

/// Build the "Shared with you" pending-share section — a `PreferencesGroup` over a
/// `ListBox`, hidden by [`populate_pending_shares`] whenever no shares are staged
/// (mirrors [`super::conflicts::build_conflicts_section`]).
/// The widgets the **"Folders you follow"** section hands back to the shell, so
/// the render loop can repaint the rows and the click handlers can be wired
/// where the `FaunaClient` is available (the `folder-add-button` idiom).
pub struct FollowedSection {
    pub group: adw::PreferencesGroup,
    /// The followed rows (`folder-followed-item`, indexed).
    pub list: gtk::ListBox,
    /// Opens the follow form. ALWAYS present, never gated on a non-empty list.
    pub follow_btn: gtk::Button,
    /// The armed form — hidden until `folder-follow-button` is pressed.
    pub form: gtk::Box,
    /// The owner half: the REUSED `recipient-picker-input`.
    pub handle_entry: gtk::Entry,
    pub name_entry: gtk::Entry,
    pub confirm_btn: gtk::Button,
}

/// Build the **"Folders you follow"** section — page-level, below the owner's
/// own rows (`ui/folders.md` § Following a public folder).
///
/// ⚠ Followed folders are a list SEPARATE from `folder-row`, and that separation
/// is the point rather than a layout choice: a followed folder has no group, no
/// roster, no local seat and no binding — a follower holds no keys and never
/// binds (`file-sync.md` § Multi-writer, the readers-never-bind rule) — and it
/// carries a *status* those rows cannot hold. Rendering one as a `folder-row`
/// would offer an expander full of controls that cannot apply to it.
///
/// ⚠ The section is **always offered**, even with no follows: the button is how
/// a user gets their first one, so gating it on a non-empty list would make it
/// unreachable. Only the *form* is armed rather than always painted.
///
/// The form REUSES `recipient-picker-input` for the handle half, never
/// re-minting a picker (priority #2, exactly as the share flow does).
pub fn build_followed_section() -> FollowedSection {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();

    let group = adw::PreferencesGroup::builder()
        .title(strings::devices::FOLLOWED_FOLDERS_SECTION)
        .build();

    let follow_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .label(strings::devices::FOLLOW_PUBLIC_FOLDER)
        .css_classes(["flat"])
        .build();
    set_test_id(&follow_btn, ids::FOLDER_FOLLOW_BUTTON);
    group.set_header_suffix(Some(&follow_btn));

    group.add(&hint_label(strings::devices::FOLLOW_PUBLIC_FOLDER_HINT));

    // ── The armed form ──────────────────────────────────────────────────
    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .visible(false)
        .build();

    // The owner half REUSES the shared picker's input id — no second picker.
    let handle_entry = gtk::Entry::builder()
        .placeholder_text(strings::conversations::unified::RECIPIENT_PICKER_PLACEHOLDER)
        .build();
    set_test_id(&handle_entry, ids::RECIPIENT_PICKER_INPUT);
    form.append(&entry_row(
        strings::conversations::unified::RECIPIENT_PICKER_PLACEHOLDER,
        &handle_entry,
    ));

    let name_entry = gtk::Entry::builder().build();
    set_test_id(&name_entry, ids::FOLDER_FOLLOW_NAME_INPUT);
    form.append(&entry_row(
        strings::devices::FOLLOW_FOLDER_NAME,
        &name_entry,
    ));
    form.append(&hint_label(strings::devices::FOLLOW_FOLDER_NAME_HINT));

    let confirm_btn = gtk::Button::builder()
        .label(strings::devices::FOLLOW_CONFIRM)
        .css_classes(["suggested-action"])
        .halign(gtk::Align::Start)
        .margin_start(12)
        .margin_bottom(4)
        .build();
    set_test_id(&confirm_btn, ids::FOLDER_FOLLOW_CONFIRM);
    // The follow's declared wire kind — the first public fetch is what pins the
    // folder_id, so the confirm is OnlineOnly and the offline gate must know it
    // (mirrors tui's `Action::ConfirmFolderFollow` → fauna.folders.public.fetch).
    crate::offline_gate::declare_wire_kind(&confirm_btn, "fauna.folders.public.fetch");
    form.append(&confirm_btn);

    group.add(&form);
    group.add(&list);

    FollowedSection {
        group,
        list,
        follow_btn,
        form,
        handle_entry,
        name_entry,
        confirm_btn,
    }
}

/// Unfollow's wording. Its own string because an unfollow that fails has nothing
/// to do with "no such public folder" — the record is local and the write is to
/// the user's own account store.
fn unfollow_error_text(e: fauna_client_folders::follow_ops::FollowOpError) -> String {
    strings::devices::error_unfollow_failed(&e.to_string())
}

/// Paint a follow-family outcome onto the page's `error-message`: clear it on
/// success, set the mapped wording on failure. A failure NEVER passes silently
/// (e2e convention 11 / convention 2 — the page must diagnose itself).
fn report_follow_outcome(
    outcome: Result<(), fauna_client_folders::follow_ops::FollowOpError>,
    error_label: &gtk::Label,
    to_text: fn(fauna_client_folders::follow_ops::FollowOpError) -> String,
) {
    match outcome {
        Ok(()) => crate::settings::render_error_label(error_label, None),
        Err(e) => crate::settings::render_error_label(error_label, Some(&to_text(e))),
    }
}

/// The real unfollow action, for [`update_followed_list`]'s callback: run the
/// shared recipe, repaint from the machine, and surface any failure on the
/// page's `error-message`.
pub fn unfollow_handler(
    machine: &Arc<DevicesMachine>,
    error_label: &gtk::Label,
) -> impl Fn(String, i64) + Clone + 'static {
    let machine = Arc::clone(machine);
    let error_label = error_label.clone();
    move |home: String, folder_id: i64| {
        let machine = Arc::clone(&machine);
        let error_label = error_label.clone();
        async_helper::run_on_tokio(
            async move {
                let store = crate::account_runtime::follows_seam();
                let outcome = fauna_client_folders::follow_ops::unfollow_public_folder(
                    &*store, &home, folder_id,
                )
                .await;
                // Refresh REGARDLESS: unfollow is idempotent, so the follows
                // the re-read reports are truth — repainting from them is
                // never wrong.
                machine.refresh().await;
                outcome
            },
            move |outcome| {
                report_follow_outcome(outcome, &error_label, unfollow_error_text);
            },
        );
    }
}

/// Wire the follow form's two buttons: `folder-follow-button` arms the form,
/// `folder-follow-confirm` runs the shared recipe and repaints from the machine.
///
/// Lives here rather than in `build_followed_section` because it needs the
/// authenticated `FaunaClient` + the machine, exactly like `folder-add-button`'s
/// handler does.
pub fn wire_followed_section(
    section: &FollowedSection,
    machine: &Arc<DevicesMachine>,
    client: &Rc<FaunaClient>,
    error_label: &gtk::Label,
) {
    {
        let form = section.form.clone();
        let handle_entry = section.handle_entry.clone();
        let name_entry = section.name_entry.clone();
        section.follow_btn.connect_clicked(move |_| {
            handle_entry.set_text("");
            name_entry.set_text("");
            form.set_visible(true);
            handle_entry.grab_focus();
        });
    }

    let machine = Arc::clone(machine);
    let nest = Arc::clone(client.nest_rpc());
    let error_label = error_label.clone();
    let form = section.form.clone();
    let handle_entry = section.handle_entry.clone();
    let name_entry = section.name_entry.clone();
    section.confirm_btn.connect_clicked(move |_| {
        let owner = handle_entry.text().trim().to_string();
        let folder_name = name_entry.text().trim().to_string();
        // Refusing a blank box here rather than sending a blank address keeps
        // the nest's one-answer refusal meaningful: an empty query would come
        // back "not found" and read as "no such folder" rather than "you left a
        // box empty". (`follow_ops` refuses it again — this is the UX half.)
        if owner.is_empty() || folder_name.is_empty() {
            return;
        }
        let nest = Arc::clone(&nest);
        let machine = Arc::clone(&machine);
        let error_label = error_label.clone();
        let form = form.clone();
        let handle_entry = handle_entry.clone();
        let name_entry = name_entry.clone();
        async_helper::run_on_tokio(
            async move {
                let store = crate::account_runtime::follows_seam();
                let outcome = fauna_client_folders::follow_ops::follow_public_folder(
                    nest,
                    &*store,
                    &owner,
                    &folder_name,
                )
                .await;
                // Repaint from the machine so the new row arrives with its
                // availability resolved by the SOURCE, never from the record
                // this call happens to hold.
                machine.refresh().await;
                outcome
            },
            move |outcome| {
                let ok = outcome.is_ok();
                report_follow_outcome(
                    outcome.map(|_| ()),
                    &error_label,
                    fauna_client_folders::follow_ops::follow_error_text,
                );
                // Close the form only on success — a failed follow keeps what
                // the user typed, so a typo is one edit away from a retry.
                if ok {
                    form.set_visible(false);
                    handle_entry.set_text("");
                    name_entry.set_text("");
                }
            },
        );
    });
}

/// Rebuild the followed-folder rows from `snapshot.followed`.
///
/// `available == false` is the **revoke** — the owner flipped the audience back
/// or deleted the folder — and the row stays visible and loud until the user
/// removes it, because a re-flip resumes it under the same `folder_id`. It is
/// deliberately NOT a liveness indicator: a transport fault keeps a row
/// *Following*, since a dropped connection must not read as a revoke
/// (`public_follow::availability_from_probe` owns that rule).
/// `on_unfollow` receives the row's `(home_nest_url, folder_id)` — the follow's
/// pinned identity, never its display name. Taking the action as a callback (the
/// `custody::update_custody_list` shape) is what keeps this renderer unit-
/// testable: the rows are pure widgets, and only the caller needs a live
/// `FaunaClient`.
pub fn update_followed_list(
    list_box: &gtk::ListBox,
    followed: &[fauna_devices_machine::FollowedFolderSummary],
    on_unfollow: impl Fn(String, i64) + Clone + 'static,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    for f in followed {
        list_box.append(&build_followed_row(f, on_unfollow.clone()));
    }
}

/// One `folder-followed-item` row: name + owner + Public provenance badge +
/// `folder-followed-status` + `folder-unfollow-button`.
fn build_followed_row(
    f: &fauna_devices_machine::FollowedFolderSummary,
    on_unfollow: impl Fn(String, i64) + 'static,
) -> gtk::ListBoxRow {
    // Indexed Box → give it AT-SPI role Group before `set_test_id` (a plain Box
    // is role Generic, which the Linux bridge can omit — same as
    // `folder-member-item` / `folder-pending-share` / `media-item`).
    let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    item.set_accessible_role(gtk::AccessibleRole::Group);
    item.set_margin_top(12);
    item.set_margin_bottom(12);
    item.set_margin_start(12);
    item.set_margin_end(12);
    set_test_id(&item, ids::FOLDER_FOLLOWED_ITEM);

    let name = gtk::Label::builder()
        .label(&f.display_name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    item.append(&name);

    // WHOSE folder it is rides the row's own text (name + owner + badge +
    // status) — this `gtk::Box` reads back as its descendant labels joined, so
    // the one read every app offers carries it. `owner_display` is the shared
    // precomputed label (the handle while it still names the owner, else the
    // id's short form), painted as given, never re-derived here.
    let owner = gtk::Label::builder()
        .label(strings::devices::followed_owner(&f.owner_display))
        .css_classes(["fauna-muted"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    item.append(&owner);

    // The provenance badge: this row came from somewhere public, which is why it
    // has no keys and no seat.
    let badge = gtk::Label::builder()
        .label(strings::devices::FOLLOWED_PUBLIC_BADGE)
        .css_classes(["fauna-muted"])
        .build();
    item.append(&badge);

    let status = gtk::Label::builder()
        .label(if f.available {
            strings::devices::FOLLOWED_STATUS_FOLLOWING
        } else {
            strings::devices::FOLLOWED_STATUS_UNAVAILABLE
        })
        .css_classes(["fauna-muted"])
        .build();
    set_test_id(&status, ids::FOLDER_FOLLOWED_STATUS);
    item.append(&status);

    if !f.available {
        // ⚠ Names BOTH causes (unshared / removed) because the follower
        // genuinely cannot tell them apart — the home nest folds them — and the
        // difference does not change what the user can do about it.
        let hint = gtk::Label::builder()
            .label(strings::devices::FOLLOWED_UNAVAILABLE_HINT)
            .wrap(true)
            .xalign(0.0)
            .css_classes(["fauna-muted"])
            .build();
        item.append(&hint);
    }

    let unfollow = gtk::Button::builder()
        .label(strings::devices::UNFOLLOW_FOLDER)
        .css_classes(["destructive-action"])
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&unfollow, ids::FOLDER_UNFOLLOW_BUTTON);
    // A purely LOCAL removal, but it still writes the user's own sealed config,
    // so the gate needs its kind (tui declares the same one).
    crate::offline_gate::declare_wire_kind(&unfollow, "fauna.account.state.put");
    {
        let home = f.home_nest_url.clone();
        let folder_id = f.folder_id;
        unfollow.connect_clicked(move |_| on_unfollow(home.clone(), folder_id));
    }
    item.append(&unfollow);

    gtk::ListBoxRow::builder().child(&item).build()
}

pub fn build_pending_shares_section() -> (adw::PreferencesGroup, gtk::ListBox) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let group = adw::PreferencesGroup::builder()
        .title(strings::devices::SHARED_WITH_YOU)
        .visible(false)
        .build();
    group.add(&list);
    (group, list)
}

/// The page-level **peer-transfer surface** (`p2p.md` § Cross-user shared-set
/// transfer, row 338; the six ids user-approved 2026-08-18). Render-only —
/// the plane has no gestures here; severance stays the member-remove / leave
/// buttons on the set rows (walk rule 1).
#[cfg(feature = "p2p-share")]
#[derive(Clone)]
pub struct ShareTransferHandles {
    pub group: adw::PreferencesGroup,
    /// `share-serve-status` — rule-5 transparency: is this device serving,
    /// and why not when it is not.
    status_label: gtk::Label,
    /// `share-transfer-list` — the rows container, present only while the
    /// plane is up or has activity (ui.yaml's stated presence rule).
    list_box: gtk::Box,
}

/// Build the peer-transfer surface. Starts **hidden**: nothing renders before
/// the share glue created its cell this session, because a never-started
/// plane has nothing honest to say (mirrors tui's `share_transfer_elements`,
/// which returns no elements at all for an absent cell).
#[cfg(feature = "p2p-share")]
pub fn build_share_transfer_section() -> ShareTransferHandles {
    let group = adw::PreferencesGroup::builder()
        .title(strings::folders::SHARE_TRANSFER_SECTION)
        .visible(false)
        .build();

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();

    let status_label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    set_test_id(&status_label, ids::SHARE_SERVE_STATUS);
    body.append(&status_label);

    let list_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .visible(false)
        .build();
    // A plain Box defaults to AT-SPI role Generic, which the Linux bridge can
    // omit from the tree — this container carries an id the e2e scopes into,
    // so it must be discoverable (the `folder-device-activity-item` rule,
    // applied to a container).
    list_box.set_accessible_role(gtk::AccessibleRole::Group);
    set_test_id(&list_box, ids::SHARE_TRANSFER_LIST);
    body.append(&list_box);

    group.add(&body);
    ShareTransferHandles {
        group,
        status_label,
        list_box,
    }
}

/// Repaint the peer-transfer surface from the share plane's state cell.
///
/// `None` — no plane started this session (signed out, a seam absent, the
/// feature off) — paints **nothing at all**: the section stays hidden rather
/// than claiming "no shared folders to serve", which is a different fact.
/// With a state, `share-serve-status` always reads, and the row list is
/// present exactly while the plane is serving or has recorded activity.
#[cfg(feature = "p2p-share")]
pub fn render_share_transfers(
    handles: &ShareTransferHandles,
    state: Option<&crate::share_glue::SharePlaneState>,
) {
    let Some(state) = state else {
        handles.group.set_visible(false);
        return;
    };
    handles.group.set_visible(true);
    handles
        .status_label
        .set_label(&crate::share_glue::serve_status_text(state.status));

    while let Some(child) = handles.list_box.first_child() {
        handles.list_box.remove(&child);
    }
    let listed = matches!(state.status, crate::share_glue::ServeStatus::Serving(_))
        || !state.outcomes.is_empty();
    handles.list_box.set_visible(listed);
    if !listed {
        return;
    }

    for outcome in &state.outcomes {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // Same rule as the container above: an indexed item in a plain Box
        // needs the Group role BEFORE its id, or the bridge can omit it.
        item.set_accessible_role(gtk::AccessibleRole::Group);
        set_test_id(&item, ids::SHARE_TRANSFER_ITEM);

        let name = gtk::Label::builder()
            .label(crate::share_glue::transfer_name_text(outcome))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["caption"])
            .build();
        set_test_id(&name, ids::SHARE_TRANSFER_NAME);
        item.append(&name);

        let progress = gtk::Label::builder()
            .label(crate::share_glue::transfer_progress_text(outcome))
            .css_classes(["dim-label", "caption"])
            .build();
        set_test_id(&progress, ids::SHARE_TRANSFER_PROGRESS);
        item.append(&progress);

        let state_lbl = gtk::Label::builder()
            .label(crate::share_glue::transfer_state_text(outcome))
            .css_classes(["caption"])
            .build();
        set_test_id(&state_lbl, ids::SHARE_TRANSFER_STATE);
        item.append(&state_lbl);

        handles.list_box.append(&item);
    }
}

/// The widgets the co-present offline-share panel (`p2p.md` § Offline share
/// initiation, row 334) hands back to [`wire_offline_share_section`] and to
/// `app.rs`'s `DataMessage` handlers — [`render_offline_share`] is the only
/// place that decides what they show, always from the shared
/// [`fauna_client_capabilities::group_ceremony_view::OfflineShareView`]
/// projection, never from these fields directly.
#[cfg(feature = "p2p-share")]
#[derive(Clone)]
pub struct OfflineShareHandles {
    pub group: adw::PreferencesGroup,
    /// The two entry buttons, visible only while no panel is open.
    closed_box: gtk::Box,
    share_btn: gtk::Button,
    receive_btn: gtk::Button,
    /// The open panel, visible only while a panel IS open.
    open_box: gtk::Box,
    own_code_label: gtk::Label,
    peer_input: gtk::Entry,
    /// Typing guidance for a malformed/own/empty compare code — chrome beside
    /// the input, never `error-message` (convention 2 reserves that for what
    /// an action actually did).
    code_hint_label: gtk::Label,
    /// Begin (initiator) and Expect (recipient) — exactly one of the two is
    /// ever shown at once; [`render_offline_share`] toggles which.
    begin_btn: gtk::Button,
    expect_btn: gtk::Button,
    status_label: gtk::Label,
    cancel_btn: gtk::Button,
}

/// Build the co-present offline-share panel — a `PreferencesGroup` with two
/// mutually-exclusive faces ([`render_offline_share`] toggles which): the
/// closed face's two entry buttons, and the open face's own-code display +
/// compare-code input + Begin/Expect + status + Cancel. The eight ids are
/// user-approved 2026-08-17 (`fauna-client-capabilities`'s
/// `group_ceremony_view` module docs).
///
/// Deliberately built with no `Rc<RefCell<OfflineShareState>>` yet — that
/// cell is the caller's job (`build_devices_and_folders_pages`), since it
/// must be reachable from BOTH this section's own click wiring and `app.rs`'s
/// `DataMessage` handlers (the invitation-row accept/decline buttons need the
/// same seat, `populate_group_invitations`).
#[cfg(feature = "p2p-share")]
pub fn build_offline_share_section() -> OfflineShareHandles {
    let group = adw::PreferencesGroup::builder()
        .title(strings::folders::OFFLINE_SHARE_SECTION)
        .build();

    let closed_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();
    let share_btn = gtk::Button::builder()
        .label(strings::folders::OFFLINE_SHARE_START)
        .build();
    set_test_id(&share_btn, ids::OFFLINE_SHARE_BUTTON);
    closed_box.append(&share_btn);
    let receive_btn = gtk::Button::builder()
        .label(strings::folders::OFFLINE_SHARE_RECEIVE)
        .build();
    set_test_id(&receive_btn, ids::OFFLINE_RECEIVE_BUTTON);
    closed_box.append(&receive_btn);
    group.add(&closed_box);

    let open_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .visible(false)
        .build();

    let own_code_label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    set_test_id(&own_code_label, ids::OFFLINE_SHARE_OWN_CODE);
    open_box.append(&entry_row(
        strings::folders::OFFLINE_SHARE_OWN_CODE_LABEL,
        &own_code_label,
    ));
    // The safety sentence: the one place a user learns that handing the code
    // over IN PERSON is the mechanism, and that the addressing candidates at
    // the end are part of it (`p2p.md` § Offline share initiation → contract
    // point 1, *The compare code carries the addressing*).
    open_box.append(&hint_label(strings::folders::OFFLINE_SHARE_OWN_CODE_HELP));

    let peer_input = gtk::Entry::builder().build();
    set_test_id(&peer_input, ids::OFFLINE_SHARE_PEER_CODE_INPUT);
    open_box.append(&entry_row(
        strings::folders::OFFLINE_SHARE_PEER_CODE_LABEL,
        &peer_input,
    ));

    let code_hint_label = hint_label("");
    code_hint_label.set_visible(false);
    open_box.append(&code_hint_label);

    let begin_btn = gtk::Button::builder()
        .label(strings::folders::OFFLINE_SHARE_BEGIN)
        .css_classes(["suggested-action"])
        .halign(gtk::Align::Start)
        .margin_start(12)
        .build();
    set_test_id(&begin_btn, ids::OFFLINE_SHARE_BEGIN_BUTTON);
    open_box.append(&begin_btn);

    let expect_btn = gtk::Button::builder()
        .label(strings::folders::OFFLINE_SHARE_EXPECT)
        .css_classes(["suggested-action"])
        .halign(gtk::Align::Start)
        .margin_start(12)
        .build();
    set_test_id(&expect_btn, ids::OFFLINE_RECEIVE_EXPECT_BUTTON);
    open_box.append(&expect_btn);

    let status_label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .margin_start(12)
        .margin_top(4)
        .css_classes(["dim-label", "caption"])
        .build();
    set_test_id(&status_label, ids::OFFLINE_SHARE_STATUS);
    open_box.append(&status_label);

    let cancel_btn = gtk::Button::builder()
        .label(strings::common::CANCEL)
        .css_classes(["flat"])
        .halign(gtk::Align::Start)
        .margin_start(12)
        .margin_bottom(4)
        .build();
    set_test_id(&cancel_btn, ids::OFFLINE_SHARE_CANCEL_BUTTON);
    open_box.append(&cancel_btn);

    group.add(&open_box);

    OfflineShareHandles {
        group,
        closed_box,
        share_btn,
        receive_btn,
        open_box,
        own_code_label,
        peer_input,
        code_hint_label,
        begin_btn,
        expect_btn,
        status_label,
        cancel_btn,
    }
}

/// Repaint the offline-share panel from the shared
/// [`fauna_client_capabilities::group_ceremony_view::OfflineShareView`]
/// projection — every element decision comes from there, never these fields
/// directly (mirrors tui's `offline_share_elements`).
#[cfg(feature = "p2p-share")]
pub fn render_offline_share(
    handles: &OfflineShareHandles,
    view: &fauna_client_capabilities::group_ceremony_view::OfflineShareView,
) {
    use fauna_client_capabilities::group_ceremony_view::OfflineSharePanel;

    handles.group.set_visible(view.available);
    if !view.available {
        return;
    }

    handles.closed_box.set_visible(view.shows_entry_buttons());
    handles.open_box.set_visible(view.shows_code_widgets());
    if !view.shows_code_widgets() {
        return;
    }

    handles.own_code_label.set_label(&view.own_code);
    // Resync the entry's text only when it genuinely differs from the view —
    // `set_text` on every render would fight the caret/selection while typing.
    if handles.peer_input.text() != view.peer_code {
        handles.peer_input.set_text(&view.peer_code);
    }

    match view.peer_actor() {
        Ok(_) => handles.code_hint_label.set_visible(false),
        Err(why) => match crate::offline_share::code_error_text(why) {
            Some(hint) => {
                handles.code_hint_label.set_label(&hint);
                handles.code_hint_label.set_visible(true);
            }
            None => handles.code_hint_label.set_visible(false),
        },
    }

    match view.panel {
        OfflineSharePanel::Initiate => {
            handles.begin_btn.set_visible(true);
            handles.begin_btn.set_sensitive(view.can_begin());
            handles.expect_btn.set_visible(false);
        }
        OfflineSharePanel::Receive => {
            handles.expect_btn.set_visible(true);
            handles.expect_btn.set_sensitive(view.can_expect());
            handles.begin_btn.set_visible(false);
        }
        // `shows_code_widgets()` already returned above for the closed panel.
        OfflineSharePanel::Closed => {}
    }

    handles
        .status_label
        .set_label(&crate::offline_share::status_text(view.status));
    handles.cancel_btn.set_visible(view.shows_cancel());
}

/// Wire the co-present offline-share panel's click handlers — the mod.rs-local
/// idiom [`wire_followed_section`] already uses, generalized to a shared
/// `Rc<RefCell<OfflineShareState>>` because `app.rs`'s `DataMessage` handlers
/// (`GroupSharesLoaded` / `OfflineShareSeatBound` / `OfflineShareProgressed` /
/// `OfflineShareFailed`) mutate the SAME state and must repaint through the
/// SAME `handles`.
#[cfg(feature = "p2p-share")]
pub fn wire_offline_share_section(
    handles: &OfflineShareHandles,
    state: &Rc<RefCell<crate::offline_share::OfflineShareState>>,
    client: &Rc<FaunaClient>,
) {
    use fauna_client_capabilities::group_ceremony_view::{CeremonyStatus, OfflineSharePanel};

    // `offline-share-button` / `offline-receive-button` — open the panel.
    // Opening while ALREADY bound is a pure flip: the session's seat slot
    // hands the existing seat straight back rather than opening a second
    // listener (one actor-keyed endpoint per session). The typed code is
    // cleared on every open — it belongs to one co-present sitting.
    for (btn, panel) in [
        (&handles.share_btn, OfflineSharePanel::Initiate),
        (&handles.receive_btn, OfflineSharePanel::Receive),
    ] {
        let state = Rc::clone(state);
        let client = Rc::clone(client);
        let handles = handles.clone();
        btn.connect_clicked(move |_| {
            let session_seat = {
                let mut s = state.borrow_mut();
                s.panel = panel;
                s.peer_code_input.clear();
                s.status = CeremonyStatus::Idle;
                s.session_seat.clone()
            };
            render_offline_share(&handles, &state.borrow().view());
            client.bind_offline_share_seat(session_seat);
        });
    }

    // `offline-share-peer-code-input` — live, so Begin/Expect's sensitivity
    // and the malformed-code hint track every keystroke.
    {
        let state = Rc::clone(state);
        let handles = handles.clone();
        handles.peer_input.clone().connect_changed(move |entry| {
            state.borrow_mut().peer_code_input = entry.text().to_string();
            render_offline_share(&handles, &state.borrow().view());
        });
    }

    // `offline-share-begin-button` — the whole initiator walk. The typed code
    // is parsed HERE, so an unparseable one never reaches the client; the
    // button is disabled in that state anyway, and this is the belt to that
    // braces. No optimistic status change — `initiate` drives the whole walk
    // to its final status in one round trip (mirrors tui's own no-interim-
    // progress shape); the DataMessage handler repaints on completion.
    {
        let state = Rc::clone(state);
        let client = Rc::clone(client);
        handles.begin_btn.connect_clicked(move |_| {
            let outcome = {
                let s = state.borrow();
                match (s.seat(), crate::offline_share::code_from_input(&s)) {
                    (Some(seat), Ok(peer)) => Some((seat, peer)),
                    _ => None,
                }
            };
            if let Some((seat, peer)) = outcome {
                client.begin_offline_share(seat, peer);
            }
        });
    }

    // `offline-receive-expect-button` — the receive act. Synchronous: minting
    // the expectation is an in-memory write on the live seat, true the
    // instant the user says so, before the initiator's offer can arrive.
    {
        let state = Rc::clone(state);
        let client = Rc::clone(client);
        let handles = handles.clone();
        handles.expect_btn.clone().connect_clicked(move |_| {
            let outcome = {
                let s = state.borrow();
                match (s.seat(), crate::offline_share::peer_from_input(&s)) {
                    (Some(seat), Ok(peer)) => Some((seat, peer)),
                    _ => None,
                }
            };
            match outcome {
                Some((seat, peer)) => {
                    client.expect_offline_share(&seat, peer);
                    state.borrow_mut().status = CeremonyStatus::Expecting;
                }
                // The button is disabled in both of these, so reaching here is
                // the belt to that braces — never a silent drop (convention 11).
                None => state.borrow_mut().status = CeremonyStatus::Failed,
            }
            render_offline_share(&handles, &state.borrow().view());
        });
    }

    // `offline-share-cancel-button` — close the panel, and on the recipient
    // side withdraw the expectation (rule 6: the user changed their mind).
    // The SEAT stays bound: the listener is this session's, not this
    // ceremony's, and re-binding an endpoint per cancel would be churn.
    {
        let state = Rc::clone(state);
        let client = Rc::clone(client);
        let handles = handles.clone();
        handles.cancel_btn.clone().connect_clicked(move |_| {
            {
                let s = state.borrow();
                if s.panel == OfflineSharePanel::Receive
                    && let (Some(seat), Ok(peer)) =
                        (s.seat(), crate::offline_share::peer_from_input(&s))
                {
                    client.cancel_offline_share_expectation(&seat, &peer);
                }
            }
            {
                let mut s = state.borrow_mut();
                s.panel = OfflineSharePanel::Closed;
                s.peer_code_input.clear();
                s.status = CeremonyStatus::Idle;
            }
            render_offline_share(&handles, &state.borrow().view());
        });
    }

    // Initial paint — the default state (closed, unavailable until `init`
    // runs at auth) so the section starts hidden rather than un-rendered.
    render_offline_share(handles, &state.borrow().view());
}

/// Rebuild the M2 "Shared with you" half of the `folder-pending-share` list.
///
/// Does **not** touch the enclosing `group`'s visibility or clear rows the
/// co-present ceremony appended — the co-present ceremony's own consent-card
/// invitations continue this SAME indexed list ([`populate_group_invitations`],
/// called right after; the consent card "reuses the knock
/// trio", so a user sees ONE list of things awaiting an answer). The caller
/// decides visibility once both halves are known — see
/// `app.rs`'s `FolderPendingSharesLoaded` / `GroupSharesLoaded` handlers.
///
/// Each M2 share renders as an indexed `folder-pending-share` (a "Shared by
/// ‹who›" label + `folder-share-accept-button` / `folder-share-decline-button`,
/// both addressing the durable row by `inbox_id`).
pub fn populate_pending_shares(
    list_box: &gtk::ListBox,
    shares: &[crate::client::PendingShareView],
    client: &Rc<FaunaClient>,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    for s in shares {
        list_box.append(&build_pending_share_row(s, client));
    }
}

/// Append the co-present ceremony's consent-card invitations onto the SAME
/// `folder-pending-share` list [`populate_pending_shares`] just rebuilt,
/// continuing its index (no explicit index attribute — list position IS the
/// index, same as every other row family here).
///
/// Both gestures need the SEAT the invitation's own panel bound — the
/// ceremony record lives on it, shared with the listener that ingested the
/// offer — so a card rendered without one (the brake refused, or no panel
/// opened this session) simply has no click effect, the same silence the two
/// entry buttons keep in that state (mirrors tui's
/// `Op::AcceptGroupShare`/`Op::DeclineGroupShare`).
#[cfg(feature = "p2p-share")]
pub fn populate_group_invitations(
    list_box: &gtk::ListBox,
    invitations: &[crate::offline_share::PendingGroupShareView],
    client: &Rc<FaunaClient>,
    state: &Rc<RefCell<crate::offline_share::OfflineShareState>>,
) {
    for inv in invitations {
        list_box.append(&build_group_invitation_row(inv, client, state));
    }
}

/// One `folder-pending-share` card for a co-present ceremony invitation — the
/// set is nameless in v1, so the card names the two things that ARE known:
/// who is handing it over, and the short scope id both people can see on
/// their own screens. Mirrors [`build_pending_share_row`]'s shape exactly.
#[cfg(feature = "p2p-share")]
fn build_group_invitation_row(
    inv: &crate::offline_share::PendingGroupShareView,
    client: &Rc<FaunaClient>,
    state: &Rc<RefCell<crate::offline_share::OfflineShareState>>,
) -> gtk::ListBoxRow {
    let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    item.set_accessible_role(gtk::AccessibleRole::Group);
    item.set_margin_top(12);
    item.set_margin_bottom(12);
    item.set_margin_start(12);
    item.set_margin_end(12);
    set_test_id(&item, ids::FOLDER_PENDING_SHARE);

    let who_lbl = gtk::Label::builder()
        .label(strings::folders::offline_share_from(
            &inv.initiator,
            &inv.short_id,
        ))
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["caption"])
        .build();
    item.append(&who_lbl);

    let accept_btn = gtk::Button::builder()
        .label(strings::common::ACCEPT)
        .css_classes(["flat", "suggested-action"])
        .build();
    set_test_id(&accept_btn, ids::FOLDER_SHARE_ACCEPT_BUTTON);
    {
        let client = Rc::clone(client);
        let state = Rc::clone(state);
        let scope_id = inv.scope_id;
        accept_btn.connect_clicked(move |_| {
            let seat = state.borrow().seat();
            if let Some(seat) = seat {
                client.consent_group_share(seat, scope_id);
            }
        });
    }
    item.append(&accept_btn);

    let decline_btn = gtk::Button::builder()
        .label(strings::common::DECLINE)
        .css_classes(["flat"])
        .build();
    set_test_id(&decline_btn, ids::FOLDER_SHARE_DECLINE_BUTTON);
    {
        let client = Rc::clone(client);
        let state = Rc::clone(state);
        let scope_id = inv.scope_id;
        decline_btn.connect_clicked(move |_| {
            let seat = state.borrow().seat();
            if let Some(seat) = seat {
                client.decline_group_share(seat, scope_id);
            }
        });
    }
    item.append(&decline_btn);

    gtk::ListBoxRow::builder()
        .child(&item)
        .activatable(false)
        .build()
}

/// One `folder-pending-share` card: the sharer identity (the shared
/// `shared_by_display` label; unknown-sharer i18n fallback) + accept/decline.
fn build_pending_share_row(
    share: &crate::client::PendingShareView,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    // Indexed Box → give it AT-SPI role Group before `set_test_id` (a plain Box is
    // role Generic, which the Linux bridge can omit — same as `folder-member-item`
    // / `media-item`).
    let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    item.set_accessible_role(gtk::AccessibleRole::Group);
    item.set_margin_top(12);
    item.set_margin_bottom(12);
    item.set_margin_start(12);
    item.set_margin_end(12);
    set_test_id(&item, ids::FOLDER_PENDING_SHARE);

    // "Shared by ‹who›" — the shared pre-computed label (handle, else canonical
    // short id; `value-formatting.md` § Account display label). Empty only for a
    // fully unstamped cross-nest share — the one locale-dependent branch left to
    // the client.
    let who = if share.shared_by_display.is_empty() {
        strings::common::UNKNOWN.to_string()
    } else {
        share.shared_by_display.clone()
    };
    let who_lbl = gtk::Label::builder()
        .label(strings::devices::shared_by(&who))
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["caption"])
        .build();
    item.append(&who_lbl);

    let accept_btn = gtk::Button::builder()
        .label(strings::common::ACCEPT)
        .css_classes(["flat", "suggested-action"])
        .build();
    set_test_id(&accept_btn, ids::FOLDER_SHARE_ACCEPT_BUTTON);
    // Accept joins the staged Welcome and then acks the durable inbox row; the
    // ack is the write that retires the offer, so both arms of this pair
    // declare it (decline is the bare ack).
    crate::offline_gate::declare_wire_kind(&accept_btn, "fauna.inbox.ack");
    {
        let client = Rc::clone(client);
        let inbox_id = share.inbox_id;
        accept_btn.connect_clicked(move |_| {
            client.accept_folder_share(inbox_id);
        });
    }
    item.append(&accept_btn);

    let decline_btn = gtk::Button::builder()
        .label(strings::common::DECLINE)
        .css_classes(["flat"])
        .build();
    set_test_id(&decline_btn, ids::FOLDER_SHARE_DECLINE_BUTTON);
    crate::offline_gate::declare_wire_kind(&decline_btn, "fauna.inbox.ack");
    {
        let client = Rc::clone(client);
        let inbox_id = share.inbox_id;
        decline_btn.connect_clicked(move |_| {
            client.decline_folder_share(inbox_id);
        });
    }
    item.append(&decline_btn);

    gtk::ListBoxRow::builder()
        .child(&item)
        .activatable(false)
        .build()
}

/// The titles of the folder rows currently expanded, so
/// [`update_folder_list`]'s rebuild can re-open them.
///
/// Title, not index: the rebuild is driven by a fresh snapshot, so a set may
/// have been added, removed or re-ordered in between — only the name survives
/// that.
fn expanded_folder_titles(list_box: &gtk::ListBox) -> Vec<String> {
    let mut open = Vec::new();
    let mut child = list_box.first_child();
    while let Some(w) = child {
        if let Some(row) = w.downcast_ref::<adw::ExpanderRow>()
            && row.is_expanded()
        {
            open.push(row.title().to_string());
        }
        child = w.next_sibling();
    }
    open
}

/// Re-open `row` if its set was open before the rebuild.
///
/// Call it AFTER the row is appended: `set_expanded` fires
/// `connect_expanded_notify`, and the lazy roster reads that handler starts
/// address the row by name through the list.
fn restore_expansion(row: &adw::ExpanderRow, expanded_titles: &[String]) {
    if expanded_titles.iter().any(|t| t == row.title().as_str()) {
        row.set_expanded(true);
    }
}

/// The title of the currently EXPANDED `adw::ExpanderRow` whose title satisfies
/// `names` — the gate `app.rs`'s `PushEvent::SyncChanged` arm uses before
/// re-fetching device activity for a pushed set, matching by the push's hash
/// address (`SyncChangedPayload::names_set`), since a sealed set's nudge names
/// no plaintext. Device activity paints only inside the expander body (unlike
/// the actor badge, it has nothing visible while collapsed), so fetching for a
/// collapsed row would just write into a hidden widget nobody's looking at —
/// the same "don't do wasted work for a row nobody sees" judgment the eager
/// actor-fetch above already makes, applied the other way: there
/// `mls_group_id.is_some()` decides because the badge IS visible collapsed;
/// here nothing is, so the row's live expanded state decides instead (mirrors
/// web's `if (expandedFs) void loadDeviceActivity(...)`). `None` when no
/// expanded `ExpanderRow` matches (a read-only shared-with-me row, or the set
/// isn't rendered yet) — the fail-safe direction, since there is nothing to
/// refresh either way.
pub fn expanded_folder_row_where(
    list_box: &gtk::ListBox,
    names: impl Fn(&str) -> bool,
) -> Option<String> {
    let mut child = list_box.first_child();
    while let Some(w) = child {
        if let Some(row) = w.downcast_ref::<adw::ExpanderRow>()
            && row.is_expanded()
            && names(row.title().as_str())
        {
            return Some(row.title().to_string());
        }
        child = w.next_sibling();
    }
    None
}

/// Find the widget named `tag` inside the expander row whose title equals `name`,
/// depth-first per row. Used for the internal roster-box tags (`MEMBERS_BOX` /
/// `SHARED_WITH_BOX`) and the `folder-shared-badge` suffix (whose `set_test_id`
/// doubles as its widget name).
fn find_tagged_widget(list_box: &gtk::ListBox, name: &str, tag: &str) -> Option<gtk::Widget> {
    let mut child = list_box.first_child();
    while let Some(w) = child {
        if let Some(row) = w.downcast_ref::<adw::ExpanderRow>()
            && row.title() == name
            && let Some(found) =
                crate::testid::find_by_test_id(row.upcast_ref::<gtk::Widget>(), tag)
        {
            return Some(found);
        }
        child = w.next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(home: Option<&str>, gid: Option<&str>) -> FolderSummary {
        FolderSummary {
            name: "docs".into(),
            home_nest_url: home.map(str::to_string),
            mls_group_id: gid.map(str::to_string),
            ..Default::default()
        }
    }

    // ── The published-folder writer warning's inputs (`ui/folders.md` § Sharing) ──
    // The decision is the shared `writer_grant_reach`; these pin that linux feeds
    // it the row's NORMALIZED audience and paywall and paints its answer — the
    // reach test itself is pinned once, in `fauna-folders-machine`.

    fn set(audience: &str, gid: Option<&str>, tier: Option<&str>) -> FolderSummary {
        FolderSummary {
            name: "site".into(),
            audience: audience.into(),
            mls_group_id: gid.map(str::to_string),
            web_paywall_tier: tier.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn a_public_set_warns_a_writer_and_names_anyone() {
        let reach = WriterReach::of(&set("public", None, None));
        assert_eq!(
            reach.warning("writer").as_deref(),
            Some(strings::devices::WRITER_PUBLIC_WARNING)
        );
    }

    #[test]
    fn a_paywalled_set_warns_a_writer_and_names_subscribers() {
        let reach = WriterReach::of(&set("shared", Some("aa"), Some("gold")));
        assert_eq!(
            reach.warning("writer").as_deref(),
            Some(strings::devices::WRITER_PAYWALLED_WARNING)
        );
    }

    #[test]
    fn a_reader_grant_or_an_unpublished_set_carries_no_published_warning() {
        assert!(
            WriterReach::of(&set("public", None, None))
                .warning("reader")
                .is_none()
        );
        assert!(
            WriterReach::of(&set("shared", Some("aa"), None))
                .warning("writer")
                .is_none()
        );
        assert!(
            WriterReach::of(&set("private", None, None))
                .warning("writer")
                .is_none()
        );
    }

    /// Fail-closed: a column this binary cannot parse must never claim the set is
    /// world-readable (the audience normalization is what guarantees it).
    #[test]
    fn an_unparseable_audience_never_claims_the_set_is_public() {
        assert!(
            WriterReach::of(&set("", None, None))
                .warning("writer")
                .is_none()
        );
        assert!(
            WriterReach::of(&set("wat", Some("aa"), None))
                .warning("writer")
                .is_none()
        );
    }

    /// An own-nest row binds optimistically — no verify context.
    #[test]
    fn a_same_nest_row_has_no_foreign_bind_context() {
        assert!(foreign_bind_for(&summary(None, Some(&hex::encode(b"gid")))).is_none());
    }

    /// A foreign row must address the set by the SAME derived channel its home
    /// nest knows it by, or every bind on a real grant is refused.
    #[test]
    fn a_foreign_row_addresses_the_set_by_its_derived_channel() {
        let raw = b"raw-openmls-group-id".to_vec();
        let bind = foreign_bind_for(&summary(
            Some("https://home.example"),
            Some(&hex::encode(&raw)),
        ))
        .expect("a foreign row with a valid group id binds cross-nest");
        assert_eq!(bind.home_nest_url, "https://home.example");
        assert_eq!(
            bind.channel_id_hex,
            hex::encode(fauna_mls::types::ChannelId::from_group_id(&raw).0)
        );
    }

    /// Fail closed: a foreign row whose channel cannot be derived offers no
    /// binding at all (the caller maps `None` to the reader's answer). Such a set
    /// is unaddressable by every federated kind, so a bind could never verify.
    #[test]
    fn an_underivable_foreign_row_fails_closed() {
        assert!(foreign_bind_for(&summary(Some("https://home.example"), None)).is_none());
        assert!(
            foreign_bind_for(&summary(Some("https://home.example"), Some("not-hex-zz"))).is_none()
        );
    }

    // ── The rebuild must not close the row the user is working in ──────
    //
    // `update_folder_list` clears and rebuilds the whole list on every snapshot
    // change, and a fresh `AdwExpanderRow` starts collapsed — so every write
    // that refreshes the machine used to take the expanded body's controls off
    // screen instead of repainting them. These pin the restore.

    fn list_with(titles: &[(&str, bool)]) -> gtk::ListBox {
        let list = gtk::ListBox::new();
        for (title, expanded) in titles {
            let row = adw::ExpanderRow::builder().title(*title).build();
            row.set_expanded(*expanded);
            list.append(&row);
        }
        list
    }

    /// Only the OPEN rows are remembered — a collapsed row must not be forced
    /// open by the next refresh.
    #[test]
    fn only_the_expanded_rows_are_carried_across_a_rebuild() {
        crate::testid::run_on_gtk_thread(|| {
            let list = list_with(&[("docs", true), ("photos", false), ("music", true)]);
            assert_eq!(
                expanded_folder_titles(&list),
                vec!["docs".to_string(), "music".to_string()],
            );
        });
    }

    /// The carry is by NAME, not position: the rebuild is driven by a fresh
    /// snapshot, which may have added, removed or re-ordered sets in between.
    /// Restoring by index would open a row the user never opened.
    #[test]
    fn expansion_is_restored_by_name_not_by_position() {
        crate::testid::run_on_gtk_thread(|| {
            let open = expanded_folder_titles(&list_with(&[("docs", false), ("photos", true)]));

            // The set that was open has moved to index 0, and a set that was
            // never open now sits where it used to be.
            let photos = adw::ExpanderRow::builder().title("photos").build();
            let docs = adw::ExpanderRow::builder().title("docs").build();
            restore_expansion(&photos, &open);
            restore_expansion(&docs, &open);

            assert!(
                photos.is_expanded(),
                "the open set stays open after a re-order"
            );
            assert!(
                !docs.is_expanded(),
                "a closed set must not inherit an index"
            );
        });
    }

    /// A set the user had open that is GONE from the new snapshot simply has no
    /// row to restore — the restore must not resurrect or mis-target it.
    #[test]
    fn a_vanished_set_restores_nothing() {
        crate::testid::run_on_gtk_thread(|| {
            let open = expanded_folder_titles(&list_with(&[("deleted-set", true)]));
            let survivor = adw::ExpanderRow::builder().title("docs").build();
            restore_expansion(&survivor, &open);
            assert!(!survivor.is_expanded());
        });
    }
}
