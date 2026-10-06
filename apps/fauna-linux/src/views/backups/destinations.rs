//! Backup-destination management section on the Backups page — the
//! *cross-location* destination list (add / edit / remove a nest the owner
//! administers to which their reserved folders are replicated as ciphertext
//! under the owner's `BackupKey`). Spec: `docs/goal/ui/backups.md` § Manage
//! backup destinations + § State & data shape (ratified 2026-06-14).
//!
//! Per priority #2 this layer holds **no** enroll/persist logic — it resolves
//! the candidate (native-specific) and sequences shared-Rust calls:
//!   * `segment_backup::resolve_and_enroll_destination` (`libs/fauna-sync-engine`)
//!     — add's whole sequence in one call: connects to the candidate as the
//!     owner's stable cross-nest actor (the `connect` performs
//!     `fauna.auth.handshake`, so success IS the reachability + authorization
//!     proof), reads `fauna.nest.info`, then runs the shared enroll (grant +
//!     writer-grant register + destination register, then the atomic
//!     `fauna.account.state.put` — see `fauna_client_config::backup_enroll`'s docs
//!     for the five-step ordering).
//!   * `segment_backup::edit_destination` — edit's whole sequence in one call
//!     too: load, re-verify identity only on a URL-pointing-at-a-different-nest
//!     change (via the dropping wrapper `resolve_destination` internally — it
//!     must not repeat add's grant/register side effects), then CAS + merge
//!     via the `edit_backup_destination` mutate helper.
//!   * `fauna_client_config::deregister_backup_destination` — remove's
//!     symmetric two-step (destination-side revoke + the atomic
//!     `fauna.account.state.put`). No destination-side state beyond the writer grant
//!     is mutated synchronously, so a crash mid-enroll/mid-removal is
//!     recoverable; the coordinator reconciles provisioning + deregistration
//!     on its next pass.
//!
//! AT-SPI discoverability follows the backups-page idiom (`restore.rs`): a
//! status row is a `gtk::Box` with role `Group` carrying its test id on the
//! accessible Description; the add/edit dialog and the remove-confirm dialog are
//! **inline reveals** (a `Box` toggled visible) rather than `adw` modals, so
//! they stay in the reachable tree — same idiom `settings/linked_nests.rs` uses.
//!
//! ## Live per-destination status read
//!
//! The per-row `backup-destination-last-upload-time` / `-backlog-count` render
//! the **nest's** `fauna.backup.status` projection through the shared
//! `fauna_client_config::read_backup_status` — the same read windows, apple,
//! android, web and tui perform (`backups.md` § Per-destination status read).
//! It is a plain WS-RPC call on the app's tokio runtime; there is no local
//! `segment-backup.sqlite` and no `!Send` coordinator to thread around any more
//! (leg (d), 2026-07-24 — see `load_destination_statuses`). The read runs every
//! time the section becomes visible (`connect_map`, the first show included — a
//! nest-side pass advances these numbers while the user sits elsewhere) and
//! after every add/edit/remove; never while the page is hidden (see `wire`).
//!
//! ## This page does not upload — the source nest does
//!
//! Since the slice-5 flip (2026-07-29) this app ships **no** segment-backup
//! upload driver: the owner's source nest is the sole writer, sweeping every
//! enrolled owner with each app asleep (`message-segment-store.md`
//! § Cross-location backup protocol). So `last_upload_time` is a timestamp only
//! the nest can have written, and enrolling a destination here is complete once
//! `resolve_and_enroll_destination` has registered it with the source nest —
//! there is no local loop to (re)build afterwards.
//!
//! The `fauna-sync-engine` items used below are the destination resolve/enroll
//! entry points (`resolve_and_enroll_destination`, `resolve_destination`),
//! which the add/edit flow uses to verify a URL points at a real, reachable
//! nest — the client `BackupCoordinator` itself was deleted 2026-08-17.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_backup::audit::{DestinationAuditRecord, run_audit_pass};
use fauna_client_config::{
    BackupStateStore, CustodianEnrollment, deregister_backup_destination, enroll_client_custodian,
    keep_backup_destination_at_rest, load_backup_state_refiled, read_backup_status,
    reenroll_custodian_after_reseed,
};
use fauna_core::data::{
    BackupDestination, DESTINATION_KIND_CLIENT_DEVICE, DESTINATION_KIND_NEST, DestinationKind,
    DestinationUnattestedMark, every_destination_is_a_client_device,
};
// The status rows are the nest's projection since the leg-(d) repoint. The wire
// row is a field-for-field mirror of the retired source-side
// `fauna_sync_engine::segment_backup::BackupDestinationStatus`, so the alias
// keeps the rendering + label helpers below (and their tests) unchanged.
use fauna_protocol::backup::BackupDestinationStatusItem as BackupDestinationStatus;

use crate::async_helper::{hydrate_with_retry, spawn_with_snapshot};
use crate::client::{FaunaClient, bound_source_nest};
use crate::i18n::strings::backups as s;
use crate::testid::set_test_id;

/// One async round-trip's result: the freshly-persisted destination list, or an
/// error string surfaced in the page `error-message`.
type MutationResult = Result<Vec<BackupDestination>, String>;

/// One read of this box's `fauna.state.backup`: the destination rows and the
/// succession-ledger marks beside them. The two travel together because a
/// row's `backup-destination-unattested-mark` is a function of both
/// ([`DestinationUnattestedMark::row_is_raised`]) — the row alone carries no
/// mark since the legacy stamp was retired.
type LoadedDestinations = Result<(Vec<BackupDestination>, Vec<DestinationUnattestedMark>), String>;

/// Widget handles the render + event closures need.
struct Widgets {
    add_button: gtk::Button,
    /// `backup-destination-add-modal` — the add/edit dialog (inline reveal).
    form: gtk::Box,
    form_title: gtk::Label,
    /// `backup-destination-kind-select` — which kind the *add* dialog composes.
    kind_select: crate::wire_kind_dropdown::WireKindDropdown,
    url_input: gtk::Entry,
    name_input: gtk::Entry,
    /// `backup-destination-capacity-input` — the client-device kind's only knob,
    /// visible for that kind alone.
    capacity_input: gtk::Entry,
    /// The § Threat model statement the custodian opt-in owes the user.
    /// Untagged — glue copy, not a new automatable id.
    custodian_note: gtk::Label,
    confirm_button: gtk::Button,
    cancel_button: gtk::Button,
    /// `backup-sole-client-destination-warning` — painted only while every
    /// configured destination is one of the owner's own devices.
    sole_client_warning: gtk::Label,
    error_label: gtk::Label,
    /// The section root — used only to re-read the status on becoming visible
    /// (see the `connect_map` in [`wire`]).
    root: gtk::Box,
    /// Container the indexed `backup-destination-status-row`s are rebuilt into.
    list: gtk::Box,
    placeholder: gtk::Label,
    /// `backup-destination-remove-confirm-modal` (inline reveal).
    remove_modal: gtk::Box,
    remove_confirm_button: gtk::Button,
    remove_cancel_button: gtk::Button,
    /// `backup-destination-remove-reclaim-checkbox` — inside [`Self::remove_modal`],
    /// visible only while removing a client-device-kind row (`backups.md` §
    /// Manage backup destinations → *Reclaim this device's copy*).
    remove_reclaim_checkbox: gtk::CheckButton,
    /// `backup-orphaned-store-row` — painted only while [`Ctx::orphaned_store`]
    /// is `Some`.
    orphaned_row: gtk::Box,
    orphaned_label: gtk::Label,
    /// `backup-destination-reclaim-button`, inside [`Self::orphaned_row`].
    reclaim_button: gtk::Button,
    /// `backup-reclaim-confirm-modal` (inline reveal) + its trio.
    reclaim_modal: gtk::Box,
    reclaim_confirm_button: gtk::Button,
    reclaim_cancel_button: gtk::Button,
    /// `backup-destination-reseed-button` inside [`Self::orphaned_row`], ahead
    /// of the reclaim (`backups.md` § Restore after losing the nest).
    orphaned_reseed_button: gtk::Button,
    /// Every painted `backup-destination-reseed-button` — the orphaned row's
    /// and this device's own status row's — so a run in flight disarms them all.
    reseed_buttons: RefCell<Vec<gtk::Button>>,
    /// `backup-destination-reseed-confirm-modal` (inline reveal) + its pair.
    reseed_modal: gtk::Box,
    reseed_confirm_button: gtk::Button,
    reseed_cancel_button: gtk::Button,
    /// `backup-destination-reseed-result` — the running line, then the shared
    /// `result_lines`; absent until a run starts.
    reseed_result: gtk::Label,
    rows: RefCell<Vec<gtk::Box>>,
    /// Container the indexed `backup-audit-alert` banners are rebuilt into.
    /// Lives at the **top of the page** rather than inside this section (see
    /// [`build_destinations_section`]) — it is the loud surface, and a data-loss
    /// warning the user has to scroll to find is not loud.
    alerts: gtk::Box,
    alert_labels: RefCell<Vec<gtk::Label>>,
}

/// Everything the handlers + render need.
struct Ctx {
    client: Rc<FaunaClient>,
    rt: tokio::runtime::Handle,
    /// `Some(destination_id)` while the form is editing an existing row; `None`
    /// while adding.
    editing: RefCell<Option<String>>,
    /// `Some(destination_id)` of the destination the remove-confirm dialog is
    /// armed for.
    removing: RefCell<Option<String>>,
    /// The last audit pass's per-destination records, keyed by position in the
    /// configured list. Cached here because the audit and the status read are
    /// **independent** round trips (see [`refresh_audit`]) — whichever lands
    /// second re-renders the rows against whatever the other already produced.
    audit: RefCell<Vec<DestinationAuditRecord>>,
    /// The destination set + status map the rows were last built from. Kept so
    /// the audit — which lands on its own schedule — can repaint the rows
    /// without re-reading either.
    last_destinations: RefCell<Vec<BackupDestination>>,
    last_statuses: RefCell<HashMap<String, BackupDestinationStatus>>,
    /// The succession-ledger marks the rows were last built from — kept beside
    /// the two above for the same repaint, so an audit landing later cannot
    /// drop a raised row's review mark.
    last_marks: RefCell<Vec<DestinationUnattestedMark>>,
    /// `Some(bytes)` while this device holds a sealed custodian store no
    /// destination row claims — the `backup-orphaned-store-row` verdict
    /// (`fauna_core::data::custodian_store_is_orphaned`), re-measured after
    /// every mutation and on mount/re-map ([`refresh_orphaned_store`]) —
    /// **never on a render path**, since it costs an agent IPC round trip.
    orphaned_store: RefCell<Option<u64>>,
    /// A re-seed is running: every `backup-destination-reseed-button` is
    /// disarmed and a second confirm is a no-op.
    reseed_running: std::cell::Cell<bool>,
    w: Widgets,
}

/// Build the backup-destinations management section.
///
/// Returns `(alerts, section, error_label)`. The **section** lives on the
/// Backups page beneath the snapshot/restore surfaces, as before. The **alerts**
/// box is the indexed `backup-audit-alert` container, which the caller mounts at
/// the top of the page, outside the scroller: `backups.md` § Audit-alert surface
/// puts the banner "at the top of the page", and an alert that says a backup is
/// not keeping up has to be seen without scrolling past the snapshot list to
/// find it. Both are driven from this module's one `Ctx`, so the audit has a
/// single owner.
///
/// The **error_label** is the Backups page's single `error-message` element
/// (e2e Rule 2). It is built here because this section was the page's first
/// error-reporting surface, and it is *returned* rather than duplicated so the
/// snapshot half's `BackupsMachine` errors land on the same element — one page,
/// one `error-message`, whichever half failed.
pub fn build_destinations_section(client: &Rc<FaunaClient>) -> (gtk::Box, gtk::Box, gtk::Label) {
    // Empty and invisible until a failing verdict lands — the
    // `restore-divergence-banner` idiom (renders only when there is something to
    // say), so a healthy page is silent.
    let alerts = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();

    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();

    // -- Header: title + add button + page-level error -----------------------
    let title = gtk::Label::new(Some(s::BACKUP_DESTINATIONS_TITLE));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("title-4");
    outer.append(&title);

    let desc = gtk::Label::new(Some(s::BACKUP_DESTINATIONS_DESC));
    desc.set_halign(gtk::Align::Start);
    desc.set_wrap(true);
    desc.add_css_class("dim-label");
    desc.add_css_class("caption");
    outer.append(&desc);

    let add_button = gtk::Button::with_label(s::BACKUP_DESTINATION_ADD_BUTTON);
    add_button.add_css_class("suggested-action");
    add_button.set_halign(gtk::Align::Start);
    set_test_id(&add_button, ids::BACKUP_DESTINATION_ADD_BUTTON);
    outer.append(&add_button);

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    outer.append(&error_label);

    // -- Add/edit dialog (inline reveal) -------------------------------------
    let f = build_form();
    outer.append(&f.form);

    // -- The sole-client durability warning ----------------------------------
    // Above the rows it is about, and painted only while true — the
    // `backup-audit-alert` idiom. Deliberately NOT an alert: nothing is failing,
    // the durability story is just weaker than the user may assume
    // (`backups.md` § Third destination kind → Durability + labeling).
    let sole_client_warning = gtk::Label::new(Some(s::BACKUP_SOLE_CLIENT_DESTINATION_WARNING));
    sole_client_warning.set_halign(gtk::Align::Start);
    sole_client_warning.set_wrap(true);
    sole_client_warning.set_visible(false);
    sole_client_warning.add_css_class("warning");
    set_test_id(
        &sole_client_warning,
        ids::BACKUP_SOLE_CLIENT_DESTINATION_WARNING,
    );
    outer.append(&sole_client_warning);

    // -- Status-row list -----------------------------------------------------
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    let placeholder = gtk::Label::new(Some(s::BACKUP_DESTINATIONS_EMPTY));
    placeholder.set_halign(gtk::Align::Start);
    placeholder.add_css_class("dim-label");
    list.append(&placeholder);
    outer.append(&list);

    // -- Orphaned-store row (`backups.md` § Manage backup destinations →
    // *Reclaim this device's copy*) — below the list, above the remove-confirm
    // dialog: a property of this device, not of any one destination row.
    let (orphaned_row, orphaned_label, orphaned_reseed_button, reclaim_button) =
        build_orphaned_row();
    outer.append(&orphaned_row);
    let (reclaim_modal, reclaim_confirm_button, reclaim_cancel_button) = build_reclaim_modal();
    outer.append(&reclaim_modal);
    // -- Re-seed (`backups.md` § Restore after losing the nest) -------------
    let (reseed_modal, reseed_confirm_button, reseed_cancel_button) = build_reseed_modal();
    outer.append(&reseed_modal);
    let reseed_result = build_reseed_result();
    outer.append(&reseed_result);

    // -- Remove-confirm dialog (inline reveal) -------------------------------
    let (remove_modal, remove_confirm_button, remove_cancel_button, remove_reclaim_checkbox) =
        build_remove_modal();
    outer.append(&remove_modal);

    let widgets = Widgets {
        add_button,
        form: f.form,
        form_title: f.form_title,
        kind_select: f.kind_select,
        url_input: f.url_input,
        name_input: f.name_input,
        capacity_input: f.capacity_input,
        custodian_note: f.custodian_note,
        confirm_button: f.confirm_button,
        cancel_button: f.cancel_button,
        sole_client_warning,
        error_label: error_label.clone(),
        root: outer.clone(),
        list,
        placeholder,
        remove_modal,
        remove_confirm_button,
        remove_cancel_button,
        remove_reclaim_checkbox,
        orphaned_row,
        orphaned_label,
        reclaim_button,
        reclaim_modal,
        reclaim_confirm_button,
        reclaim_cancel_button,
        orphaned_reseed_button,
        reseed_buttons: RefCell::new(Vec::new()),
        reseed_modal,
        reseed_confirm_button,
        reseed_cancel_button,
        reseed_result,
        rows: RefCell::new(Vec::new()),
        alerts: alerts.clone(),
        alert_labels: RefCell::new(Vec::new()),
    };
    wire(client, widgets);
    (alerts, outer, error_label)
}

/// Build the `backup-destination-kind-select` dropdown over the shared
/// catalog ([`fauna_core::format::backup_destination_kind_options`]), so
/// paint order and the option text are decided once for all 7 apps; in
/// particular the option a user picks is literally the same `LocalizedText`
/// the resulting row's `backup-destination-kind-badge` renders, and this
/// shell cannot drift them apart.
///
/// The model strings are the wire values the `select(id, value)` e2e
/// contract drives, and the label closure derives the painted text from the
/// wire value — never the reverse, which is what left this control
/// unreachable by wire value.
fn build_kind_select() -> crate::wire_kind_dropdown::WireKindDropdown {
    let values: Vec<String> = fauna_core::format::backup_destination_kind_options()
        .into_iter()
        .map(|o| o.value)
        .collect();
    crate::wire_kind_dropdown::WireKindDropdown::build(
        values,
        ids::BACKUP_DESTINATION_KIND_SELECT,
        crate::i18n::backup_destination_kind,
    )
}

/// Every widget [`build_form`] hands back. A struct rather than the 10-tuple
/// this became when the kind branch landed — the positional form was already at
/// six, and a reader had to count commas to learn which `gtk::Entry` was which.
struct FormWidgets {
    form: gtk::Box,
    form_title: gtk::Label,
    kind_select: crate::wire_kind_dropdown::WireKindDropdown,
    url_input: gtk::Entry,
    name_input: gtk::Entry,
    capacity_input: gtk::Entry,
    custodian_note: gtk::Label,
    confirm_button: gtk::Button,
    cancel_button: gtk::Button,
}

/// Build the inline add/edit dialog. The same dialog serves add and edit (the
/// `Ctx::editing` flag distinguishes them — `backups.md` § Manage backup
/// destinations → Edit) and both implemented kinds ([`sync_form_for_kind`]
/// swaps the per-kind fields).
fn build_form() -> FormWidgets {
    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&form, ids::BACKUP_DESTINATION_ADD_MODAL);

    let form_title = gtk::Label::new(Some(s::BACKUP_DESTINATION_FORM_ADD_TITLE));
    form_title.set_halign(gtk::Align::Start);
    form_title.add_css_class("heading");
    form.append(&form_title);

    // The kind select, above the fields it swaps. Its label is a plain caption
    // rather than a tagged element — ui.yaml scopes an id to the control.
    let kind_caption = gtk::Label::new(Some(s::BACKUP_DESTINATION_KIND_SELECT_LABEL));
    kind_caption.set_halign(gtk::Align::Start);
    kind_caption.add_css_class("dim-label");
    kind_caption.add_css_class("caption");
    form.append(&kind_caption);

    let kind_select = build_kind_select();
    kind_select.dd.set_halign(gtk::Align::Start);
    form.append(&kind_select.dd);

    // The URL is the nest kind's field — ui.yaml scopes it "nest kind only". A
    // custodian has no address at all (`backups.md` § Custodian contract,
    // question 1), so painting an empty URL box for it would invite the user to
    // type one nothing could ever use.
    let url_input = gtk::Entry::builder()
        .placeholder_text(s::BACKUP_DESTINATION_URL_PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(&url_input, ids::BACKUP_DESTINATION_URL_INPUT);
    form.append(&url_input);

    let name_input = gtk::Entry::builder()
        .placeholder_text(s::BACKUP_DESTINATION_NAME_PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(&name_input, ids::BACKUP_DESTINATION_NAME_INPUT);
    form.append(&name_input);

    // The capacity cap — the client-device kind's only knob, so it paints for
    // that kind alone (ui.yaml: "this-device kind only").
    let capacity_input = gtk::Entry::builder()
        .placeholder_text(s::BACKUP_DESTINATION_CAPACITY_PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(&capacity_input, ids::BACKUP_DESTINATION_CAPACITY_INPUT);
    form.append(&capacity_input);

    // The honest statement `backups.md` § Threat model requires **at opt-in**: a
    // complete offline corpus is a materially different exposure from an
    // ordinary logged-in device, and the user must see it here rather than
    // discover it later.
    let custodian_note = gtk::Label::new(Some(s::BACKUP_DESTINATION_CUSTODIAN_EXPOSURE));
    custodian_note.set_halign(gtk::Align::Start);
    custodian_note.set_wrap(true);
    custodian_note.add_css_class("dim-label");
    custodian_note.add_css_class("caption");
    form.append(&custodian_note);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel_button = gtk::Button::with_label(s::BACKUP_DESTINATION_ADD_CANCEL);
    set_test_id(&cancel_button, ids::BACKUP_DESTINATION_ADD_CANCEL_BUTTON);
    let confirm_button = gtk::Button::with_label(s::BACKUP_DESTINATION_ADD_CONFIRM);
    confirm_button.add_css_class("suggested-action");
    set_test_id(&confirm_button, ids::BACKUP_DESTINATION_ADD_CONFIRM_BUTTON);
    buttons.append(&cancel_button);
    buttons.append(&confirm_button);
    form.append(&buttons);

    let widgets = FormWidgets {
        form,
        form_title,
        kind_select,
        url_input,
        name_input,
        capacity_input,
        custodian_note,
        confirm_button,
        cancel_button,
    };
    // The dialog's initial per-kind state comes from the swap rule itself rather
    // than from hand-set `.visible()` flags — one statement of "which fields does
    // this kind have", not a build-time copy that can drift from it.
    widgets.sync_kind_fields();
    widgets
}

/// Show the fields the selected kind actually has, and hide the ones it does
/// not. The swap — rather than a disabled-but-present URL box — is the ratified
/// shape: a custodian has **no address at all**, so an empty URL box would
/// invite the user to type one nothing could ever use.
///
/// Stated once here and forwarded from both carriers of these widgets
/// ([`Widgets`] while the page is live, [`FormWidgets`] straight out of
/// [`build_form`]), so a widget test drives the *production* rule rather than a
/// test-only re-derivation of it.
fn apply_kind_swap(
    kind_select: &crate::wire_kind_dropdown::WireKindDropdown,
    url_input: &gtk::Entry,
    capacity_input: &gtk::Entry,
    custodian_note: &gtk::Label,
) {
    // Never a blank kind, which `kind_view()` would project `Inert` — the
    // out-of-range fallback is nest, never the client-device kind.
    let custodian =
        kind_select.selected_kind(DESTINATION_KIND_NEST) == DESTINATION_KIND_CLIENT_DEVICE;
    url_input.set_visible(!custodian);
    capacity_input.set_visible(custodian);
    custodian_note.set_visible(custodian);
}

impl Widgets {
    fn sync_kind_fields(&self) {
        apply_kind_swap(
            &self.kind_select,
            &self.url_input,
            &self.capacity_input,
            &self.custodian_note,
        );
    }
}

impl FormWidgets {
    fn sync_kind_fields(&self) {
        apply_kind_swap(
            &self.kind_select,
            &self.url_input,
            &self.capacity_input,
            &self.custodian_note,
        );
    }
}

/// Build the inline remove-confirm dialog.
fn build_remove_modal() -> (gtk::Box, gtk::Button, gtk::Button, gtk::CheckButton) {
    let modal = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&modal, ids::BACKUP_DESTINATION_REMOVE_CONFIRM_MODAL);

    let prompt = gtk::Label::new(Some(s::BACKUP_DESTINATION_REMOVE_CONFIRM_TITLE));
    prompt.set_halign(gtk::Align::Start);
    prompt.set_wrap(true);
    modal.append(&prompt);

    // `backup-destination-remove-reclaim-checkbox` (`backups.md` § Manage
    // backup destinations → *Reclaim this device's copy*) — the opt-in to ALSO
    // free this device's sealed store in the same gesture. Visible only while
    // removing a client-device-kind row (toggled by the remove button's own
    // handler, per row.kind — never every kind, since only that kind can name
    // this device as its custodian). Unticked by default: the checkbox is an
    // intent about THIS removal, and a tick surviving a cancelled dialog would
    // delete an offline copy nobody asked about in this gesture.
    let reclaim_checkbox =
        gtk::CheckButton::with_label(s::BACKUP_DESTINATION_REMOVE_RECLAIM_CHECKBOX);
    reclaim_checkbox.set_visible(false);
    reclaim_checkbox.set_active(false);
    set_test_id(
        &reclaim_checkbox,
        ids::BACKUP_DESTINATION_REMOVE_RECLAIM_CHECKBOX,
    );
    modal.append(&reclaim_checkbox);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label(s::BACKUP_DESTINATION_REMOVE_CANCEL_BUTTON);
    set_test_id(&cancel, ids::BACKUP_DESTINATION_REMOVE_CANCEL_BUTTON);
    let confirm = gtk::Button::with_label(s::BACKUP_DESTINATION_REMOVE_CONFIRM_BUTTON);
    confirm.add_css_class("destructive-action");
    set_test_id(&confirm, ids::BACKUP_DESTINATION_REMOVE_CONFIRM_BUTTON);
    crate::offline_gate::declare_wire_kind(&confirm, "fauna.backup.destination.remove");
    buttons.append(&cancel);
    buttons.append(&confirm);
    modal.append(&buttons);

    (modal, confirm, cancel, reclaim_checkbox)
}

/// `backup-orphaned-store-row` — painted only while [`Ctx::orphaned_store`] is
/// `Some` (`backups.md` § Manage backup destinations → *Reclaim this device's
/// copy*). Carries [`ids::BACKUP_DESTINATION_RECLAIM_BUTTON`], which arms
/// [`build_reclaim_modal`]'s confirm rather than reaching the agent directly —
/// reclaiming ends this device's standalone-restore property, the reason for
/// the modal — and, ahead of it, [`ids::BACKUP_DESTINATION_RESEED_BUTTON`]
/// (`backups.md` § Restore after losing the nest): on a rebuilt nest this row
/// is where the owner's only copy is offered back, and the destructive gesture
/// beside it must not be the first one met.
fn build_orphaned_row() -> (gtk::Box, gtk::Label, gtk::Button, gtk::Button) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::BACKUP_ORPHANED_STORE_ROW);

    let label = gtk::Label::builder()
        .wrap(true)
        .halign(gtk::Align::Start)
        .build();
    label.set_hexpand(true);
    row.append(&label);

    let reseed_button = build_reseed_button();
    row.append(&reseed_button);

    let reclaim_button = gtk::Button::with_label(s::BACKUP_DESTINATION_RECLAIM_BUTTON);
    reclaim_button.set_valign(gtk::Align::Center);
    reclaim_button.add_css_class("destructive-action");
    set_test_id(&reclaim_button, ids::BACKUP_DESTINATION_RECLAIM_BUTTON);
    row.append(&reclaim_button);

    (row, label, reseed_button, reclaim_button)
}

/// One `backup-destination-reseed-button`. It arms [`build_reseed_modal`]'s
/// confirm; it never reaches the agent directly.
fn build_reseed_button() -> gtk::Button {
    let button = gtk::Button::with_label(s::BACKUP_DESTINATION_RESEED_BUTTON);
    button.set_valign(gtk::Align::Center);
    button.add_css_class("suggested-action");
    set_test_id(&button, ids::BACKUP_DESTINATION_RESEED_BUTTON);
    button
}

/// `backup-destination-reseed-confirm-modal` (inline reveal) — the plain
/// confirm pair, the reclaim modal's idiom. The body states the consequence
/// where the decision is made; ui.yaml scopes no id to it.
fn build_reseed_modal() -> (gtk::Box, gtk::Button, gtk::Button) {
    let modal = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&modal, ids::BACKUP_DESTINATION_RESEED_CONFIRM_MODAL);

    let title = gtk::Label::new(Some(s::BACKUP_RESEED_CONFIRM_TITLE));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("heading");
    modal.append(&title);

    let body = gtk::Label::new(Some(s::BACKUP_RESEED_CONFIRM_BODY));
    body.set_halign(gtk::Align::Start);
    body.set_wrap(true);
    modal.append(&body);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label(s::BACKUP_RESEED_CANCEL_BUTTON);
    set_test_id(&cancel, ids::BACKUP_DESTINATION_RESEED_CANCEL_BUTTON);
    let confirm = gtk::Button::with_label(s::BACKUP_RESEED_CONFIRM_BUTTON);
    confirm.add_css_class("suggested-action");
    set_test_id(&confirm, ids::BACKUP_DESTINATION_RESEED_CONFIRM_BUTTON);
    // No wire kind, as for the reclaim: the confirm reaches the sync agent over
    // local IPC, and the agent's own connections carry the ceremony.
    buttons.append(&cancel);
    buttons.append(&confirm);
    modal.append(&buttons);

    (modal, confirm, cancel)
}

/// `backup-destination-reseed-result` — hidden until a run starts.
fn build_reseed_result() -> gtk::Label {
    let label = gtk::Label::builder()
        .wrap(true)
        .halign(gtk::Align::Start)
        .visible(false)
        .build();
    set_test_id(&label, ids::BACKUP_DESTINATION_RESEED_RESULT);
    label
}

/// `backup-reclaim-confirm-modal` (inline reveal) — the plain confirm trio,
/// deliberately no re-type: the store is re-buildable on re-enrollment, unlike
/// the declassify-style confirms elsewhere on this app. Mirrors
/// [`build_remove_modal`].
fn build_reclaim_modal() -> (gtk::Box, gtk::Button, gtk::Button) {
    let modal = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&modal, ids::BACKUP_RECLAIM_CONFIRM_MODAL);

    let title = gtk::Label::new(Some(s::BACKUP_RECLAIM_CONFIRM_TITLE));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("heading");
    modal.append(&title);

    let body = gtk::Label::new(Some(s::BACKUP_RECLAIM_CONFIRM_BODY));
    body.set_halign(gtk::Align::Start);
    body.set_wrap(true);
    modal.append(&body);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label(s::BACKUP_RECLAIM_CANCEL_BUTTON);
    set_test_id(&cancel, ids::BACKUP_RECLAIM_CANCEL_BUTTON);
    let confirm = gtk::Button::with_label(s::BACKUP_RECLAIM_CONFIRM_BUTTON);
    confirm.add_css_class("destructive-action");
    set_test_id(&confirm, ids::BACKUP_RECLAIM_CONFIRM_BUTTON);
    // No wire kind: the reclaim reaches the sync agent over local IPC, not the
    // nest — there is nothing for the offline gate to protect, and the whole
    // premise of this copy is that it reads with no nest reachable at all
    // (`backup-destinations.md` § Standalone restore).
    buttons.append(&cancel);
    buttons.append(&confirm);
    modal.append(&buttons);

    (modal, confirm, cancel)
}

/// Wire the section to the shared persist/resolve calls, load + render on mount
/// (with live status), and connect every interaction. No-op (static placeholder)
/// when no client is registered (the unit test).
fn wire(client: &Rc<FaunaClient>, widgets: Widgets) {
    let ctx = Rc::new(Ctx {
        client: Rc::clone(client),
        rt: client.runtime_handle(),
        editing: RefCell::new(None),
        removing: RefCell::new(None),
        audit: RefCell::new(Vec::new()),
        last_destinations: RefCell::new(Vec::new()),
        last_statuses: RefCell::new(HashMap::new()),
        last_marks: RefCell::new(Vec::new()),
        orphaned_store: RefCell::new(None),
        reseed_running: std::cell::Cell::new(false),
        w: widgets,
    });

    // No read at build time — the `connect_map` below is the ONLY load
    // trigger, and it fires on the page's first show too. The section is built
    // into the content stack with every authenticated window, so a build-time
    // `refresh` read `fauna.backup.status` at every sign-in with the page
    // hidden, and that read HEALS a missing enrollment (`read_backup_status`).
    // After an identity succession the window rebuild landed it ahead of the
    // aftermath's own re-grant leg, which then found nothing owed and painted no
    // progress line — the one witness that leg ran (`succession-aftermath.md` §
    // Implementation status today: "the mount heal" is a page VISIT, as on tui,
    // which reads this page only when it is opened).

    // The `backup_audit_run_now` agent command drives *this* — the production
    // pass and the production render — so an e2e proof exercises the same code
    // path a user's page visit does.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    {
        let ctx_poke = Rc::clone(&ctx);
        crate::backup_audit::set_rerun_hook(Rc::new(move || refresh_audit(&ctx_poke)));
    }

    // Read the status every time the page becomes visible — the first show
    // included, which makes this the section's only load trigger besides a
    // mutation (see above for why there is no build-time read).
    //
    // This section is built **once** per window into the app's content
    // `GtkStack` (`app.rs::build_backups_view` → `stack.add_named(…,
    // "backups")`); switching pages only unmaps it. Before this hook a
    // build-time read was the only refresh besides an add/edit/remove. That made the page **untruthful about the
    // very thing leg (d) repointed it at**: `last_upload_time` is advanced by the
    // *nest's* own backup pass, which runs with every app asleep
    // (`backup-restore.md` § Background Tasks), so an owner who enrolled a
    // destination and simply navigated away and back kept reading the
    // never-synced baseline indefinitely — until they happened to mutate a row.
    //
    // Same shape as the Feed split's `connect_map` re-list, and the sibling of
    // the `"backups"` arm in `app.rs`'s `connect_visible_child_name_notify`
    // (which refreshes the snapshot/restore surfaces but has no reach into this
    // section's `Ctx`). Kept local so the section owns its own freshness.
    {
        let ctx_map = Rc::clone(&ctx);
        let root = ctx.w.root.clone();
        root.connect_map(move |_| {
            refresh(&ctx_map);
            // The audit rides the same trigger. It is debounced to
            // `AUDIT_MIN_INTERVAL` in shared Rust, so paging back and forth
            // costs one store read, not one round trip per destination.
            refresh_audit(&ctx_map);
        });
    }

    // Add → open the form in add mode.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.add_button.clone().connect_clicked(move |_| {
            *ctx.editing.borrow_mut() = None;
            ctx.w.url_input.set_text("");
            ctx.w.name_input.set_text("");
            ctx.w.capacity_input.set_text("");
            ctx.w.url_input.set_sensitive(true);
            // The kind is editable only while adding, and resets to the catalog
            // default (nest — the kind that actually satisfies "off-site").
            ctx.w.kind_select.dd.set_sensitive(true);
            ctx.w.kind_select.dd.set_selected(0);
            ctx.w.sync_kind_fields();
            // The offline gate: `confirm_button` is a composite control whose
            // ceremony is decided by kind + editing state (rule 4 — declare
            // again once the paint decides). Explicit here rather than relying
            // on the kind-select handler below: `set_selected(0)` is a no-op
            // (no `notify::selected`) when the dropdown was already on index 0.
            crate::offline_gate::declare_wire_kind(
                &ctx.w.confirm_button,
                "fauna.backup.nest_key.grant",
            );
            ctx.w
                .form_title
                .set_text(s::BACKUP_DESTINATION_FORM_ADD_TITLE);
            ctx.w.error_label.set_visible(false);
            ctx.w.remove_modal.set_visible(false);
            ctx.w.form.set_visible(true);
        });
    }

    // Kind change → swap the per-kind fields and, while adding, redeclare
    // which enroll ceremony `confirm_button` now binds — the kind select is
    // add-only (edit paints it disabled), so this must not clobber the Edit
    // declaration the edit-open handler below sets.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .kind_select
            .dd
            .clone()
            .connect_selected_notify(move |_| {
                ctx.w.sync_kind_fields();
                if ctx.editing.borrow().is_none() {
                    let custodian = ctx.w.kind_select.selected_kind(DESTINATION_KIND_NEST)
                        == DESTINATION_KIND_CLIENT_DEVICE;
                    let kind = if custodian {
                        "fauna.backup.destination.register"
                    } else {
                        "fauna.backup.nest_key.grant"
                    };
                    crate::offline_gate::declare_wire_kind(&ctx.w.confirm_button, kind);
                }
            });
    }

    // Cancel → hide the form.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.cancel_button.clone().connect_clicked(move |_| {
            *ctx.editing.borrow_mut() = None;
            ctx.w.form.set_visible(false);
        });
    }

    // Confirm (button or Enter in either entry) → add or edit.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .confirm_button
            .clone()
            .connect_clicked(move |_| submit_form(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .url_input
            .clone()
            .connect_activate(move |_| submit_form(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .name_input
            .clone()
            .connect_activate(move |_| submit_form(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .capacity_input
            .clone()
            .connect_activate(move |_| submit_form(&ctx));
    }

    // Remove-confirm cancel → hide. The reclaim tick is an intent about THIS
    // removal (`build_remove_modal`'s doc), so a cancel clears it too — it must
    // never survive to arm the NEXT removal it was never asked about.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .remove_cancel_button
            .clone()
            .connect_clicked(move |_| {
                *ctx.removing.borrow_mut() = None;
                ctx.w.remove_modal.set_visible(false);
                ctx.w.remove_reclaim_checkbox.set_active(false);
            });
    }
    // Remove-confirm confirm → dispatch remove, carrying the reclaim opt-in.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .remove_confirm_button
            .clone()
            .connect_clicked(move |_| {
                let id = ctx.removing.borrow().clone();
                if let Some(id) = id {
                    let reclaim = ctx.w.remove_reclaim_checkbox.is_active();
                    ctx.w.remove_modal.set_visible(false);
                    ctx.w.remove_reclaim_checkbox.set_active(false);
                    dispatch(&ctx, Mutation::Remove { id, reclaim });
                }
            });
    }

    // The orphaned-store row's reclaim button → arm the plain confirm modal.
    // Guarded on `orphaned_store` (the keyboard-actuation backstop behind the
    // render guard — every other stale actuation on this page costs a no-op,
    // this one would delete the owner's only offline copy).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.reclaim_button.clone().connect_clicked(move |_| {
            if ctx.orphaned_store.borrow().is_some() {
                ctx.w.reclaim_modal.set_visible(true);
            }
        });
    }
    // Reclaim-confirm cancel → hide, nothing changed.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .reclaim_cancel_button
            .clone()
            .connect_clicked(move |_| {
                ctx.w.reclaim_modal.set_visible(false);
            });
    }
    // Reclaim-confirm confirm → free this device's whole sealed store.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .reclaim_confirm_button
            .clone()
            .connect_clicked(move |_| {
                ctx.w.reclaim_modal.set_visible(false);
                // Re-checked at press time, not trusted from the paint: the
                // modal can outlive the verdict that opened it (a refresh
                // landing between the two re-enrolls this device, say).
                if ctx.orphaned_store.borrow().is_some() {
                    dispatch_reclaim(&ctx);
                }
            });
    }

    // The orphaned row's re-seed button → arm the plain confirm. (A status
    // row's button is wired where the row is built, to the same handler.)
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .orphaned_reseed_button
            .clone()
            .connect_clicked(move |_| open_reseed_confirm(&ctx));
    }
    // Re-seed cancel → hide, nothing changed.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .reseed_cancel_button
            .clone()
            .connect_clicked(move |_| ctx.w.reseed_modal.set_visible(false));
    }
    // Re-seed confirm → run the ceremony.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .reseed_confirm_button
            .clone()
            .connect_clicked(move |_| {
                ctx.w.reseed_modal.set_visible(false);
                // Re-checked at press time, not trusted from the paint.
                if !ctx.reseed_running.get() && !reseed_rows(&ctx).is_empty() {
                    dispatch_reseed(&ctx);
                }
            });
    }
}

/// The five persisting actions the section dispatches.
enum Mutation {
    Add {
        url: String,
        name: String,
    },
    /// Enroll **this** device as a client custodian — a different sequence, not
    /// a variant of `Add` with a blank URL: no destination to resolve, no
    /// second nest to connect to, and no `NestBackupKey` grant
    /// (`backups.md` § Third destination kind → Enrollment).
    EnrollCustodian {
        name: String,
        capacity_cap_bytes: Option<u64>,
    },
    Edit {
        id: String,
        url: String,
        name: String,
    },
    Remove {
        id: String,
        /// The `backup-destination-remove-reclaim-checkbox` tick: also free
        /// this device's sealed store, once the removal itself lands
        /// (`backups.md` § Manage backup destinations → *Reclaim this
        /// device's copy*). Meaningless (ignored) unless the removed row was
        /// this device's own client-device destination — the checkbox is
        /// hidden for every other kind, so `dispatch` need not re-check.
        reclaim: bool,
    },
    /// `backup-destination-keep-button`: the owner adjudicates a row the
    /// succession aftermath raised as theirs (`succession-aftermath.md` §
    /// Adjudicating what the aftermath carries across).
    Keep {
        id: String,
    },
}

/// Read the form and dispatch the mutation its kind + `Ctx::editing` select.
fn submit_form(ctx: &Rc<Ctx>) {
    let name = ctx.w.name_input.text().trim().to_string();
    let editing = ctx.editing.borrow().clone();

    // The kind select is add-only: edit paints it disabled and never reads it
    // back, because re-pointing a live row would keep a `destination_id` whose
    // registry row and grants describe the other kind (§ Create / edit /
    // remove — the kind is not an editable property).
    if editing.is_none()
        && ctx.w.kind_select.selected_kind(DESTINATION_KIND_NEST) == DESTINATION_KIND_CLIENT_DEVICE
    {
        // The cap is the kind's only knob, and a blank one is a real choice:
        // `CustodianEnrollment::capacity_cap_bytes: None` is uncapped. A
        // non-blank one that cannot be read is a refusal the user sees, never a
        // substituted default — guessing a cap is how a device's disk fills.
        let typed = ctx.w.capacity_input.text().trim().to_string();
        let capacity_cap_bytes = if typed.is_empty() {
            None
        } else {
            match fauna_core::format::parse_byte_size(&typed) {
                Some(bytes) => Some(bytes),
                None => {
                    ctx.w.error_label.add_css_class("error");
                    crate::settings::render_error_label(
                        &ctx.w.error_label,
                        Some(s::BACKUP_DESTINATION_CAPACITY_INVALID),
                    );
                    return;
                }
            }
        };
        // No interim "Resolving…": there is nothing to resolve — the whole
        // point of the kind is that it has no address.
        crate::settings::render_error_label(&ctx.w.error_label, None);
        dispatch(
            ctx,
            Mutation::EnrollCustodian {
                name,
                capacity_cap_bytes,
            },
        );
        return;
    }

    let url = ctx.w.url_input.text().trim().to_string();
    if url.is_empty() {
        return;
    }
    let mutation = match editing {
        Some(id) => Mutation::Edit { id, url, name },
        None => Mutation::Add { url, name },
    };
    // Resolving a destination is a network round-trip; show interim status.
    ctx.w.error_label.remove_css_class("error");
    crate::settings::render_error_label(&ctx.w.error_label, Some(s::BACKUP_DESTINATION_RESOLVING));
    dispatch(ctx, mutation);
}

/// Load the current destination list **and** the per-destination status, then
/// render the rows. Used on mount and after every add/edit/remove.
///
/// **Repointed 2026-07-24 (slice-4 leg (d)):** the status comes from the
/// **nest's** `fauna.backup.status` projection via the shared
/// `fauna_client_config::read_backup_status`, not from a local upload
/// coordinator. That deleted three things this function used to need:
/// the always-on-driver fast path (there is no local `segment-backup.sqlite` to
/// share an opener with any more), the dedicated worker thread (the coordinator
/// was `!Send`; a WS-RPC read is not), and the ephemeral-coordinator fallback.
/// linux now reads exactly what windows/apple/android/web read.
///
/// **The read runs on the tokio runtime, never on the GTK main context.** Both
/// halves of it are tokio-bound — `NestClient::request` wraps every call in
/// `tokio::time::timeout`, and [`hydrate_with_retry`]'s backoff is
/// `tokio::time::sleep` — and this process holds no runtime enter-guard on the
/// GTK thread, so polling either from a bare `glib::spawn_future_local` panics
/// the task ("there is no timer running"), silently killing the render. That is
/// exactly what the leg-(d) repoint did between 2026-07-24 and this fix: the
/// enroll succeeded, the error label was cleared, and the rows then never
/// appeared — `test_backup_destination_crud[linux]` red with an empty
/// `error-message`. Hence [`spawn_with_snapshot`], the same runtime hop every
/// other async read in this app uses: produce on `ctx.rt`, render on the GTK
/// thread. The only work allowed inside a `spawn_future_local` here is awaiting
/// the `async_channel` that helper already owns.
fn refresh(ctx: &Rc<Ctx>) {
    let nest = ctx.client.nest_rpc().clone();
    let secret_hex = ctx.client.secret_hex().to_string();
    let ctx_render = Rc::clone(ctx);

    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            // Kept: load_destinations composes the bound-box proof with an
            // account-plane read, not a single NestClient RPC (transport.md §
            // Request lifecycle step 3's note).
            let (destinations, marks) =
                match hydrate_with_retry(|| load_destinations(nest.clone())).await {
                    Ok(loaded) => loaded,
                    // A failed read is *unknown*, not empty. Swallowing it here
                    // (the pre-fix `unwrap_or_default`) told an owner with
                    // destinations configured that they had none, and made a
                    // genuine load failure indistinguishable from a fresh account.
                    Err(e) => return StatusLoad::failed(e),
                };
            // Zero destinations ⇒ no status rows (they render only when ≥1 is
            // configured — `backups.md` § Per-destination status read), and the
            // shared read declines to enroll an owner who configured nothing, so
            // skip the round-trip entirely.
            if destinations.is_empty() {
                return StatusLoad::loaded(destinations, marks, HashMap::new());
            }
            let statuses = match (secret_from_hex(&secret_hex), bound_source_nest(&nest).await) {
                (Ok(secret), Ok(source_nest)) => {
                    read_backup_status(nest, backup_seam().as_ref(), secret, source_nest)
                        .await
                        .map(|reply| {
                            reply
                                .destinations
                                .into_iter()
                                .map(|r| (r.destination_id.clone(), r))
                                .collect()
                        })
                        // Unlike the destination list above, the *status* read
                        // degrades to the not-yet-backed-up baseline ("never" / "0
                        // queued"): the rows themselves are known, so they must
                        // still render rather than being hidden behind an error.
                        .unwrap_or_default()
                }
                _ => HashMap::new(),
            };
            StatusLoad::loaded(destinations, marks, statuses)
        },
        move |load| render_load(&ctx_render, load),
    );
}

/// Run one client-side audit pass and render its verdicts.
///
/// **Deliberately a separate round trip from [`refresh`].** The audit opens the
/// client's *own* authenticated connection to every configured destination, so a
/// destination that is slow or unreachable costs a connect timeout — folding it
/// into the status read would make the whole row list wait on the least
/// available destination, and the rows are the page's primary content. Two
/// independent reads, each rendering as it lands ([`render_audit`] and
/// [`render_load`] both re-render the rows against whatever the other has
/// already produced).
///
/// Same runtime discipline as [`refresh`]: everything real happens on `ctx.rt`,
/// and the `spawn_future_local` side only awaits the channel
/// (`async_helper.rs` module docs — a `tokio::time::timeout` polled without an
/// enter-guard panics the task and silently kills the render).
fn refresh_audit(ctx: &Rc<Ctx>) {
    let nest = ctx.client.nest_rpc().clone();
    let secret_hex = ctx.client.secret_hex().to_string();
    // This session's own actor — never a fresh `active_actor_id_hex()` read,
    // which can name a different account than the one this process serves on
    // a bound (secondary) launch (`account-scoping.md:818-837`). Cached on
    // `FaunaClient`, so this is free.
    let actor_id_hex = ctx.client.actor_id().unwrap_or_default();
    let ctx_render = Rc::clone(ctx);
    let now = now_secs();
    // The agent seam and this device's sync id, as `refresh_orphaned_store`
    // resolves them.
    let own_custodian = crate::sync_agent::provisioner_and_runtime()
        .map(|(provisioner, _rt)| provisioner)
        .zip(crate::sync::device_id().ok())
        .map(|(provisioner, id)| (provisioner, fauna_core::hex32::encode(&id)));

    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let Ok(secret) = secret_from_hex(&secret_hex) else {
                return Vec::new();
            };
            // The owner's own nest — dialled by the pass only to settle a
            // ledger regression a destination served (the generation pin,
            // `fauna_client_backup::audit::SourceLedgerVouch`) or to read its
            // rotation chain when a seat names its predecessor; the identity
            // the list rests under is the one a seat is carried to.
            let source_nest_url = nest.nest_url();
            let Ok(source_nest) = bound_source_nest(&nest).await else {
                return Vec::new();
            };
            let index_nest = Arc::clone(&nest);
            // The audited set is the client's OWN pinned destination list, not
            // the source nest's registry — a destination the source has
            // "forgotten" must still be audited, and still alert
            // (`fauna_client_backup::audit::audit_all`).
            let Ok((destinations, _marks)) = load_destinations(nest).await else {
                return Vec::new();
            };
            if destinations.is_empty() {
                return Vec::new();
            }
            // The covered-folder mirror plane's population anchor is this
            // device's synced replica of each covered folder: the sync
            // engine's `ReplicaFolderIndex` over the agent's state dir for THIS
            // session's actor (`<config>/fauna/sync/<actor>/`, the dir
            // `sync::sync_state_dir` names) and the bound source nest, read
            // once for the pass.
            let folder_index = fauna_sync_engine::segment_backup::bound_replica_folder_index(
                &index_nest,
                Some(fauna_sync_engine::segment_backup::local_agent_state_dir(
                    &actor_id_hex,
                )),
            )
            .await;
            let connector = fauna_client_pair::native_backup_destination_connector(secret);
            // The inclusion arm reads sampled bytes from the DESTINATION's own
            // blob routes and opens them under the owner's derived
            // `NestBackupKey` — so a pass means "fresh AND the sampled records
            // are really there and openable", not merely "fresh".
            let inclusion = fauna_client_pair::native_backup_inclusion_source(secret, folder_index);
            let store = crate::backup_audit::store(&actor_id_hex);
            // This device's own custodian store, read from the agent that
            // hosts it: its standing source regressions fold into the row
            // that assigns this device. No agent, no device id yet, or an
            // agent that did not answer is "no store was read" — the row's
            // record stands as it was.
            let own_custodian = match own_custodian {
                Some((provisioner, device_hex)) => {
                    provisioner.own_custodian_store(&device_hex).await
                }
                None => None,
            };
            let pass = run_audit_pass(
                connector.as_ref(),
                inclusion.as_ref(),
                &store,
                &destinations,
                &source_nest_url,
                &source_nest,
                own_custodian.as_ref(),
                now,
            )
            .await;
            for degradation in &pass.degradations {
                tracing::warn!("backup audit: {degradation}");
            }
            pass.records
        },
        move |records| render_audit(&ctx_render, records),
    );
}

/// Re-measure [`Ctx::orphaned_store`] — the agent's own read of this device's
/// sealed custodian store, judged against the destination rows [`refresh`]
/// most recently loaded, via the shared
/// [`fauna_core::data::custodian_store_is_orphaned`] (`backups.md` § Manage
/// backup destinations → *Reclaim this device's copy*). Deliberately **not**
/// `custodian_assignment_for(..).is_none()` — that answers `None` for two rows
/// naming this device, and re-deriving it here would offer to delete live
/// custody.
///
/// **Deliberately never computed on a render path** — it costs an agent IPC
/// round trip on top of the disk walk the agent does — so it rides the same
/// three triggers [`refresh`]/[`refresh_audit`] do: mount, re-map, and after
/// every mutation ([`apply`]). It reaches them through [`render_load`], the one
/// place where the destination rows it judges against are known to be current;
/// calling it beside a [`refresh`] instead judges the PREVIOUS list.
///
/// Every failure (no agent installed on this platform, no device id yet, a
/// refusing agent) answers "not orphaned" — the conservative direction for a
/// gesture that deletes the owner's only offline copy: not knowing must never
/// paint the reclaim button.
fn refresh_orphaned_store(ctx: &Rc<Ctx>) {
    let Some((provisioner, rt)) = crate::sync_agent::provisioner_and_runtime() else {
        render_orphaned_row(ctx, None);
        return;
    };
    let Ok(device_id) = crate::sync::device_id() else {
        render_orphaned_row(ctx, None);
        return;
    };
    let device_hex = fauna_core::hex32::encode(&device_id);
    let destinations = ctx.last_destinations.borrow().clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &rt,
        move || async move {
            let Ok(info) = provisioner.custodian_store().await else {
                return None;
            };
            fauna_core::data::custodian_store_is_orphaned(
                destinations.iter().map(BackupDestination::custodian_row),
                &device_hex,
                info.bytes > 0,
            )
            .then_some(info.bytes)
        },
        move |verdict| render_orphaned_row(&ctx_render, verdict),
    );
}

/// Paint `backup-orphaned-store-row` from the cached [`Ctx::orphaned_store`]
/// verdict — never recomputes it (see [`refresh_orphaned_store`]).
fn render_orphaned_row(ctx: &Rc<Ctx>, verdict: Option<u64>) {
    *ctx.orphaned_store.borrow_mut() = verdict;
    match verdict {
        Some(bytes) => {
            ctx.w
                .orphaned_label
                .set_text(&fauna_core::format::orphaned_store_text(
                    bytes,
                    fauna_i18n::strings::lookup,
                ));
            ctx.w.orphaned_row.set_visible(true);
        }
        None => ctx.w.orphaned_row.set_visible(false),
    }
    ctx.w
        .orphaned_reseed_button
        .set_visible(reseed_rows(ctx).contains(&None));
    ctx.w
        .orphaned_reseed_button
        .set_sensitive(!ctx.reseed_running.get());
}

/// This device's sync id, hex — what a client-device row names its custodian
/// by. Empty when it cannot be read, which matches no row.
fn this_device_hex() -> String {
    crate::sync::device_id()
        .map(|d| fauna_core::hex32::encode(&d))
        .unwrap_or_default()
}

/// Where `backup-destination-reseed-button` paints: `None` for the orphaned
/// store's row, `Some(id)` for a destination row. The shared
/// `fauna_client_backup::reseed::reseed_sources` decides which rows are
/// sources at all; this keeps the ones whose delivery leg this build can run,
/// so a nest-kind row joins by its leg becoming built, never by a change here.
/// The same rule tui's `reseed_rows` applies.
fn reseed_rows(ctx: &Ctx) -> Vec<Option<String>> {
    fauna_client_backup::reseed::reseed_sources(
        &ctx.last_destinations.borrow(),
        &this_device_hex(),
        ctx.orphaned_store.borrow().is_some(),
    )
    .into_iter()
    .filter(|s| s.leg.is_built())
    .map(|s| s.destination_id)
    .collect()
}

/// Arm the re-seed confirm — a no-op while a run is in flight or when no row
/// offers the gesture (the keyboard-actuation backstop behind the paint).
fn open_reseed_confirm(ctx: &Rc<Ctx>) {
    if ctx.reseed_running.get() || reseed_rows(ctx).is_empty() {
        return;
    }
    ctx.w.reclaim_modal.set_visible(false);
    ctx.w.reseed_modal.set_visible(true);
}

/// Arm or disarm every painted `backup-destination-reseed-button`.
fn paint_reseed_buttons(ctx: &Ctx) {
    let armed = !ctx.reseed_running.get();
    ctx.w.orphaned_reseed_button.set_sensitive(armed);
    for button in ctx.w.reseed_buttons.borrow().iter() {
        button.set_sensitive(armed);
    }
}

/// Unix seconds, the clock every audit comparison is made against — plus the
/// e2e clock offset, which is zero in every real run
/// (`fauna_client_backup::audit_clock::clock_offset_secs`).
fn now_secs() -> i64 {
    fauna_client_backup::audit_clock::now_secs()
}

/// Cache one audit pass's records and repaint everything that reads them: the
/// per-row `backup-destination-last-audit-time`, and the indexed
/// `backup-audit-alert` banners.
fn render_audit(ctx: &Rc<Ctx>, records: Vec<DestinationAuditRecord>) {
    *ctx.audit.borrow_mut() = records;
    render_alerts(ctx);
    // The rows already on screen were built before this pass landed; rebuild
    // them so each picks up its "Last checked" text.
    let destinations: Vec<BackupDestination> = ctx.last_destinations.borrow().clone();
    if !destinations.is_empty() {
        let statuses = ctx.last_statuses.borrow().clone();
        let marks = ctx.last_marks.borrow().clone();
        render_rows(ctx, &destinations, &marks, &statuses);
    }
}

/// The owner-side `backup-audit-alert` banner texts, in record order: for each
/// record, one banner per reason `DestinationAuditRecord::alert_reasons(now)`
/// yields — the standing verdict's, then at most one open source-regression
/// recovery window. Pure, so the render is testable without GTK.
fn owner_alert_banners(
    records: &[DestinationAuditRecord],
    destinations: &[BackupDestination],
    now: i64,
) -> Vec<String> {
    records
        .iter()
        .flat_map(|record| {
            // Name the destination the way every other row does (shared
            // fallback to the URL host when it has no display name).
            let dest_label = destinations
                .iter()
                .find(|d| d.destination_id == record.state.destination_id)
                .map(destination_label)
                .unwrap_or_else(|| record.state.destination_id.clone());
            record
                .alert_reasons(now)
                .into_iter()
                .map(move |reason| crate::i18n::backup_audit_alert(reason, &dest_label))
        })
        .collect()
}

/// Rebuild the `backup-audit-alert` banners — one per reason standing against a
/// destination, none at all when everything is healthy.
///
/// Which reasons are loud is **not** decided here: `DestinationAuditRecord::
/// alert_reasons(now)` is the single shared answer (`AuditVerdict::is_alerting`
/// is defined through the verdict half of it), so this client cannot drift into
/// alerting on, say, a transient `Unreachable` — the laptop-on-a-plane case the
/// loop deliberately keeps quiet.
fn render_alerts(ctx: &Rc<Ctx>) {
    let mut labels = ctx.w.alert_labels.borrow_mut();
    for label in labels.drain(..) {
        ctx.w.alerts.remove(&label);
    }

    let destinations = ctx.last_destinations.borrow();
    let statuses = ctx.last_statuses.borrow();

    // A client-device custodian reporting its OWN copy as failing — the only
    // failure signal that exists for a kind the owner-side loop can never
    // sample (`backup-destinations.md` § Custodian contract, question 4).
    for dest in self_reported_alert_destinations(&destinations, &statuses) {
        let banner = gtk::Label::new(Some(&crate::i18n::backup_audit_alert(
            fauna_core::format::BackupAuditAlertReason::SelfReported,
            &destination_label(dest),
        )));
        banner.set_halign(gtk::Align::Start);
        banner.set_wrap(true);
        banner.add_css_class("error");
        set_test_id(&banner, ids::BACKUP_AUDIT_ALERT);
        ctx.w.alerts.append(&banner);
        labels.push(banner);
    }

    for text in owner_alert_banners(&ctx.audit.borrow(), &destinations, now_secs()) {
        let banner = gtk::Label::new(Some(&text));
        banner.set_halign(gtk::Align::Start);
        banner.set_wrap(true);
        banner.add_css_class("error");
        set_test_id(&banner, ids::BACKUP_AUDIT_ALERT);
        ctx.w.alerts.append(&banner);
        labels.push(banner);
    }
    ctx.w.alerts.set_visible(!labels.is_empty());
}

/// One [`refresh`] round-trip's result, handed from the tokio runtime to the
/// GTK thread. `load_error` is `Some` when the destination list itself could
/// not be read — see [`StatusLoad::failed`].
struct StatusLoad {
    destinations: Vec<BackupDestination>,
    marks: Vec<DestinationUnattestedMark>,
    statuses: HashMap<String, BackupDestinationStatus>,
    load_error: Option<String>,
}

impl StatusLoad {
    fn loaded(
        destinations: Vec<BackupDestination>,
        marks: Vec<DestinationUnattestedMark>,
        statuses: HashMap<String, BackupDestinationStatus>,
    ) -> Self {
        Self {
            destinations,
            marks,
            statuses,
            load_error: None,
        }
    }

    /// The destination list could not be read. Carries no rows — the caller
    /// leaves whatever is on screen alone rather than asserting an empty set.
    fn failed(error: String) -> Self {
        Self {
            destinations: Vec::new(),
            marks: Vec::new(),
            statuses: HashMap::new(),
            load_error: Some(error),
        }
    }
}

/// Render a [`refresh`] outcome (GTK main thread). A load failure surfaces in
/// the page `error-message` (Rule 2) with the existing rows and the "no
/// destinations" placeholder both left alone; a successful load clears the
/// error and rebuilds the rows.
fn render_load(ctx: &Rc<Ctx>, load: StatusLoad) {
    match load.load_error {
        Some(msg) => {
            ctx.w.error_label.add_css_class("error");
            crate::settings::render_error_label(&ctx.w.error_label, Some(&msg));
            // Not "you have no backups" — we do not know what you have.
            ctx.w.placeholder.set_visible(false);
        }
        None => {
            crate::settings::render_error_label(&ctx.w.error_label, None);
            render_rows(ctx, &load.destinations, &load.marks, &load.statuses);
            // Re-label the banners: the two reads are independent, so an audit
            // that landed first named its destinations by raw id (nothing knew
            // their display names yet). Now that the list is in, redraw.
            render_alerts(ctx);
            // The orphaned-store verdict rides HERE, not beside `refresh`'s
            // call sites, because it is judged AGAINST the destination rows —
            // and `render_rows` above is what makes them current. Calling it
            // next to `refresh(..)` instead reads `last_destinations` before
            // this round trip has landed, i.e. the PRE-mutation list, which is
            // the one case that matters: after a client-device removal the
            // stale list still names this device, `custodian_store_is_orphaned`
            // answers false, and `backup-orphaned-store-row` never paints for
            // the gesture that creates the orphan (caught by
            // `test_backups.py::test_removing_a_custodian_without_the_opt_in_\
            // leaves_a_reclaimable_orphaned_store` on 2026-09-20; tui avoids it
            // by deriving the verdict inside the same async load — see
            // `apps/fauna-tui/src/backups.rs`'s `orphaned_store_bytes`). The
            // trigger set is unchanged (mount, re-map, every mutation): each is
            // a `refresh` call, and every `refresh` ends here.
            //
            // The load-failure arm above deliberately does NOT re-measure: with
            // the destination list unknown, the conservative direction for a
            // gesture that deletes the owner's only offline copy is to leave
            // the row exactly as it was, never to newly paint it.
            refresh_orphaned_store(ctx);
        }
    }
}

/// Run a mutation on the tokio runtime, then apply the result on the GTK thread.
fn dispatch(ctx: &Rc<Ctx>, mutation: Mutation) {
    ctx.w.add_button.set_sensitive(false);
    ctx.w.confirm_button.set_sensitive(false);
    let nest = ctx.client.nest_rpc().clone();
    let secret_hex = ctx.client.secret_hex().to_string();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            match mutation {
                Mutation::Add { url, name } => add_destination(nest, secret_hex, url, name).await,
                Mutation::EnrollCustodian {
                    name,
                    capacity_cap_bytes,
                } => enroll_custodian(nest, name, capacity_cap_bytes).await,
                Mutation::Edit { id, url, name } => {
                    edit_destination(nest, secret_hex, id, url, name).await
                }
                Mutation::Remove { id, reclaim } => {
                    remove_destination_and_maybe_reclaim(nest, id, reclaim).await
                }
                Mutation::Keep { id } => keep_destination(nest, id).await,
            }
        },
        move |result| apply(&ctx_render, result.into(), /*close_form=*/ true),
    );
}

/// The GTK-thread outcome of a mutation: an error to surface, or `None` on
/// success. Since the slice-5 flip there is no upload driver to rebuild from
/// the freshly-persisted set (see [`apply`]'s comment) — success just
/// re-renders live from `fauna.backup.status`, so this carries no data.
struct Applied {
    error: Option<String>,
}
impl From<MutationResult> for Applied {
    fn from(r: MutationResult) -> Self {
        match r {
            Ok(_destinations) => Applied { error: None },
            Err(e) => Applied { error: Some(e) },
        }
    }
}

/// Render a mutation outcome (GTK main thread). On success the form closes (when
/// `close_form`), the error clears, and the rows re-render with the freshly
/// loaded destination list + live status ([`refresh`]); on error the message
/// shows and the form stays open so the user can fix the input.
fn apply(ctx: &Rc<Ctx>, applied: Applied, close_form: bool) {
    ctx.w.add_button.set_sensitive(true);
    ctx.w.confirm_button.set_sensitive(true);

    match applied.error {
        Some(msg) => {
            ctx.w.error_label.add_css_class("error");
            crate::settings::render_error_label(&ctx.w.error_label, Some(&msg));
        }
        None => {
            crate::settings::render_error_label(&ctx.w.error_label, None);
            if close_form {
                ctx.w.form.set_visible(false);
                *ctx.editing.borrow_mut() = None;
            }
            // No upload driver to rebuild since the slice-5 flip: the source
            // nest is the segment-backup writer, and `enroll_backup_destination`
            // already registered this destination with it, so the next
            // `NestBackupWorker` sweep picks it up with no client involvement.
            // Re-render with live status straight from `fauna.backup.status`.
            refresh(ctx);
            // Re-audit after a mutation: adding a destination gives it a
            // "never" record, and removing one must clear its banner — the
            // shared `merge_outcomes` does both, but only once a pass runs.
            refresh_audit(ctx);
            // The orphaned-store verdict re-measures after every mutation too,
            // not only Remove — EnrollCustodian and Add can both re-claim a
            // store this device previously orphaned, and the row must stop
            // offering to delete it in the same repaint (`backups.md` § Manage
            // backup destinations → *Reclaim this device's copy*). It is not
            // called here: `refresh` above ends in `render_load`, which runs it
            // over the list that round trip actually loaded. See there for why
            // the call cannot live beside this one.
        }
    }
}

/// Rebuild the status-row list from the destination set, each row carrying its
/// live status (keyed by `destination_id`; absent ⇒ the not-yet-backed-up
/// baseline).
fn render_rows(
    ctx: &Rc<Ctx>,
    destinations: &[BackupDestination],
    marks: &[DestinationUnattestedMark],
    statuses: &HashMap<String, BackupDestinationStatus>,
) {
    // One row per enrolled destination — coverage rows (per-folder
    // mirror-set rows sharing a destination_id) are folded away here, at the
    // render boundary, so a destination with N covered folders paints once
    // rather than N+1 times (`fauna_core::data::
    // distinct_destinations`'s own doc). `refresh_audit`'s own `load_
    // destinations` call stays on the raw list, which its `attached_mirror_
    // sets` needs.
    let destinations = fauna_core::data::distinct_destinations(destinations);
    let destinations = destinations.as_slice();

    // Remember what the rows were built from, so an audit pass landing later can
    // repaint them (and name its banners' destinations) without re-reading.
    *ctx.last_destinations.borrow_mut() = destinations.to_vec();
    *ctx.last_statuses.borrow_mut() = statuses.clone();
    *ctx.last_marks.borrow_mut() = marks.to_vec();

    let reseed_rows = reseed_rows(ctx);
    ctx.w.reseed_buttons.borrow_mut().clear();
    let audit = ctx.audit.borrow();
    let mut rows = ctx.w.rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.list.remove(&row);
    }
    for dest in destinations {
        let status = statuses.get(&dest.destination_id);
        let record = audit
            .iter()
            .find(|r| r.state.destination_id == dest.destination_id);
        let reseed_here = reseed_rows.contains(&Some(dest.destination_id.clone()));
        let raised = DestinationUnattestedMark::row_is_raised(marks, dest);
        let row = build_status_row(ctx, dest, status, record, reseed_here, raised);
        ctx.w.list.append(&row);
        rows.push(row);
    }
    ctx.w.placeholder.set_visible(destinations.is_empty());
    // The standing "every copy you have is on one of your own devices" warning.
    // The predicate is shared (`fauna_core::data`) rather than re-derived here:
    // its Inert arm decides whether a user who *is* covered gets told they are
    // not, and that is a policy answer, not a rendering one.
    ctx.w
        .sole_client_warning
        .set_visible(every_destination_is_a_client_device(destinations));
}

/// `backup-destination-last-upload-time` text for a row's live status, via the
/// shared [`fauna_client_backup::row_text::destination_last_upload_text`] —
/// the field extraction and the resolve both moved there (2026-08-21) because
/// this body and tui's were byte-identical; this leaves only "now".
fn last_upload_text(status: Option<&BackupDestinationStatus>) -> String {
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    fauna_client_backup::row_text::destination_last_upload_text(status, now_ms)
}

/// `backup-destination-backlog-count` text for a row's live status, via the
/// shared [`fauna_client_backup::row_text::destination_backlog_text`]. Absent
/// ⇒ "0 queued" (nothing read yet / nothing queued).
fn backlog_text(status: Option<&BackupDestinationStatus>) -> String {
    fauna_client_backup::row_text::destination_backlog_text(status)
}

/// `backup-destination-last-audit-time` text for a row: when this client's own
/// audit last **passed** against the destination, via the shared
/// [`fauna_client_backup::row_text::destination_last_audit_text`]. `None` (no
/// pass yet, or no audit has run this launch) ⇒ "never".
///
/// Note what this is *not*: the row above it (`last_upload_time`) is the source
/// nest reporting on its own uploads, and this one is the only line on the page
/// that neither the source nor the destination gets to assert.
fn last_audit_text(record: Option<&DestinationAuditRecord>) -> String {
    // The same clock the pass was judged against ([`now_secs`]), so a stamp this
    // client wrote can never read as being in the future.
    let now_ms = now_secs().saturating_mul(1_000);
    fauna_client_backup::row_text::destination_last_audit_text(record, now_ms)
}

/// `backup-destination-last-audit-time` text for a **client-device custodian**
/// row: when that device's own self-audit last **passed**, via the shared
/// [`fauna_client_backup::row_text::destination_self_audit_text`]. A custodian
/// has no address for the owner-side loop to reach, so this cell carries the
/// device's own verdict instead — `None` reads as *not yet*, never as a pass.
fn self_audit_text(status: Option<&BackupDestinationStatus>) -> String {
    let now_ms = now_secs().saturating_mul(1_000);
    fauna_client_backup::row_text::destination_self_audit_text(status, now_ms)
}

/// `backup-destination-last-audit-time` cell text for a row, dispatched on the
/// row's kind (`backups.md` § Audit-alert surface → *The client-device arm*).
/// A client-device row carries its own self-audit; every other kind keeps the
/// owner-side independent check.
fn audit_cell_text(
    dest: &BackupDestination,
    status: Option<&BackupDestinationStatus>,
    record: Option<&DestinationAuditRecord>,
) -> String {
    if matches!(dest.kind_view(), DestinationKind::ClientDevice { .. }) {
        self_audit_text(status)
    } else {
        last_audit_text(record)
    }
}

/// Client-device destinations whose own last self-audit is loud, via the
/// shared `backup_self_audit_is_alerting` predicate — pure data, no GTK, so it
/// is directly unit-testable the way [`render_alerts`] itself is not.
fn self_reported_alert_destinations<'a>(
    destinations: &'a [BackupDestination],
    statuses: &HashMap<String, BackupDestinationStatus>,
) -> Vec<&'a BackupDestination> {
    destinations
        .iter()
        .filter(|d| {
            fauna_core::format::backup_self_audit_is_alerting(
                statuses
                    .get(&d.destination_id)
                    .and_then(|s| s.audit_state.as_deref()),
            )
        })
        .collect()
}

/// Build one `backup-destination-status-row` for `dest`, rendering its live
/// `last_upload_time` / `backlog_count` (or the not-yet-backed-up baseline when
/// `status` is `None`) plus its `last-audit-time` (or "never" when this client
/// has not yet audited it). `raised` — the shared
/// [`DestinationUnattestedMark::row_is_raised`] verdict — adds the
/// post-succession review mark and its Keep.
fn build_status_row(
    ctx: &Rc<Ctx>,
    dest: &BackupDestination,
    status: Option<&BackupDestinationStatus>,
    record: Option<&DestinationAuditRecord>,
    reseed_here: bool,
    raised: bool,
) -> gtk::Box {
    let label = destination_label(dest);

    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    // Description = test id (findable); Label = the destination name.
    row.update_property(&[
        gtk::accessible::Property::Description("backup-destination-status-row"),
        gtk::accessible::Property::Label(label.as_str()),
    ]);
    row.set_widget_name("backup-destination-status-row");
    row.set_tooltip_text(Some(&label));

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    let name_label = gtk::Label::new(Some(&label));
    name_label.set_halign(gtk::Align::Start);
    name_label.add_css_class("body");
    info.append(&name_label);

    // kind-badge — the visible half of "a client custodian never silently
    // satisfies *you have an off-site backup*" (`backups.md` § Durability +
    // labeling). Reads the row's own discriminator through the shared label, so
    // an unknown kind a newer client wrote renders as itself rather than
    // masquerading as a nest.
    let kind_badge = gtk::Label::new(Some(&crate::i18n::backup_destination_kind(&dest.kind)));
    kind_badge.set_halign(gtk::Align::Start);
    kind_badge.add_css_class("dim-label");
    kind_badge.add_css_class("caption");
    set_test_id(&kind_badge, ids::BACKUP_DESTINATION_KIND_BADGE);
    info.append(&kind_badge);

    // usage — client-device rows only (ui.yaml). A nest row has no cap and no
    // held-bytes report, so the element is absent rather than empty.
    if matches!(dest.kind_view(), DestinationKind::ClientDevice { .. }) {
        let usage = gtk::Label::new(Some(&crate::i18n::backup_usage(
            status.and_then(|s| s.held_bytes),
            dest.capacity_cap_bytes,
            status.and_then(|s| s.cap_state.as_deref()),
        )));
        usage.set_halign(gtk::Align::Start);
        usage.add_css_class("dim-label");
        usage.add_css_class("caption");
        set_test_id(&usage, ids::BACKUP_DESTINATION_USAGE);
        info.append(&usage);
    }

    // last-upload-time — live max-manifest-sync time, "never" until one mirrors.
    let last_upload = gtk::Label::new(Some(&last_upload_text(status)));
    last_upload.set_halign(gtk::Align::Start);
    last_upload.add_css_class("dim-label");
    last_upload.add_css_class("caption");
    set_test_id(&last_upload, ids::BACKUP_DESTINATION_LAST_UPLOAD_TIME);
    info.append(&last_upload);

    // backlog-count — live count of source segments not yet uploaded here.
    let backlog = gtk::Label::new(Some(&backlog_text(status)));
    backlog.set_halign(gtk::Align::Start);
    backlog.add_css_class("dim-label");
    backlog.add_css_class("caption");
    set_test_id(&backlog, ids::BACKUP_DESTINATION_BACKLOG_COUNT);
    info.append(&backlog);

    // last-audit-time — the client's own independent check for a nest row,
    // "never" until one passes; a client-device row carries its own self-audit
    // instead (`backups.md` § Audit-alert surface → *The client-device arm*).
    let last_audit = gtk::Label::new(Some(&audit_cell_text(dest, status, record)));
    last_audit.set_halign(gtk::Align::Start);
    last_audit.add_css_class("dim-label");
    last_audit.add_css_class("caption");
    set_test_id(&last_audit, ids::BACKUP_DESTINATION_LAST_AUDIT_TIME);
    info.append(&last_audit);

    // The post-succession review mark — present only while this row is actually
    // raised (`succession-aftermath.md` § Adjudicating what the aftermath
    // carries across). Absent rather than empty on an ordinary row: in a
    // healthy account every destination is the owner's own, and a
    // permanently-rendered mark would train the user straight past the one
    // succession that matters. tui's twin is `status_row`.
    if raised {
        info.append(&build_unattested_mark());
    }

    row.append(&info);

    // Keep — the mark's adjudicating half. Remove is deliberately NOT
    // re-rendered: the row already carries `backup-destination-remove-button`
    // below, so Keep joins the affordance that exists instead of minting a
    // second removal path.
    if raised {
        let keep = build_keep_button();
        let ctx = Rc::clone(ctx);
        let id = dest.destination_id.clone();
        keep.connect_clicked(move |_| dispatch(&ctx, Mutation::Keep { id: id.clone() }));
        row.append(&keep);
    }

    // Re-seed — on this device's own custodian row only (`reseed_rows`).
    if reseed_here {
        let reseed = build_reseed_button();
        reseed.set_sensitive(!ctx.reseed_running.get());
        let ctx_click = Rc::clone(ctx);
        reseed.connect_clicked(move |_| open_reseed_confirm(&ctx_click));
        ctx.w.reseed_buttons.borrow_mut().push(reseed.clone());
        row.append(&reseed);
    }

    // Edit → reopen the form prefilled for this destination.
    let edit = gtk::Button::with_label(s::BACKUP_DESTINATION_EDIT_BUTTON);
    edit.set_valign(gtk::Align::Center);
    set_test_id(&edit, ids::BACKUP_DESTINATION_EDIT_BUTTON);
    {
        let ctx = Rc::clone(ctx);
        let id = dest.destination_id.clone();
        let url = dest.destination_nest_url.clone();
        let name = dest.display_name.clone().unwrap_or_default();
        let kind = dest.kind.clone();
        // Round-trips through the shared `parse_byte_size` unchanged, so a cap
        // the user never touches is re-read exactly as they typed it.
        let capacity = dest
            .capacity_cap_bytes
            .map(crate::i18n::byte_size)
            .unwrap_or_default();
        edit.connect_clicked(move |_| {
            *ctx.editing.borrow_mut() = Some(id.clone());
            ctx.w.url_input.set_text(&url);
            ctx.w.name_input.set_text(&name);
            ctx.w.capacity_input.set_text(&capacity);
            // Prefilled from the row and painted **disabled** rather than
            // hidden, so the row's kind stays legible while renaming it; the
            // kind itself is not an editable property (§ Create / edit /
            // remove), and `submit_form` never reads this control while editing.
            ctx.w.kind_select.select_kind(&kind);
            ctx.w.kind_select.dd.set_sensitive(false);
            ctx.w.sync_kind_fields();
            // The offline gate: editing is a rename/re-point of the owner's own
            // config document, so it stays live as `fauna.account.state.put` — the
            // paint deciding `confirm_button`'s third ceremony (rule 4).
            // Explicit rather than relying on the kind-select handler above:
            // `select_kind` may be a no-op if this row's kind is already
            // selected, and even when it does fire, `ctx.editing` is already
            // `Some` by then so that handler declines to touch this control.
            crate::offline_gate::declare_wire_kind(
                &ctx.w.confirm_button,
                "fauna.account.state.put",
            );
            ctx.w
                .form_title
                .set_text(s::BACKUP_DESTINATION_FORM_EDIT_TITLE);
            ctx.w.error_label.set_visible(false);
            ctx.w.remove_modal.set_visible(false);
            ctx.w.form.set_visible(true);
        });
    }
    row.append(&edit);

    // Remove → arm the remove-confirm dialog for this destination.
    let remove = gtk::Button::with_label(s::BACKUP_DESTINATION_REMOVE_BUTTON);
    remove.set_valign(gtk::Align::Center);
    remove.add_css_class("destructive-action");
    set_test_id(&remove, ids::BACKUP_DESTINATION_REMOVE_BUTTON);
    {
        let ctx = Rc::clone(ctx);
        let id = dest.destination_id.clone();
        // `backup-destination-remove-reclaim-checkbox` is offered only on THIS
        // device's own client-device row — the one kind `custodian_row` can
        // name this device as custodian of (`backups.md` § Manage backup
        // destinations → *Reclaim this device's copy*). Captured here rather
        // than looked up when the modal opens: `dest` is already the row this
        // click is for.
        let is_client_device = fauna_core::data::row_is_a_client_device(dest.custodian_row());
        remove.connect_clicked(move |_| {
            *ctx.removing.borrow_mut() = Some(id.clone());
            ctx.w.form.set_visible(false);
            ctx.w.remove_modal.set_visible(true);
            ctx.w.remove_reclaim_checkbox.set_visible(is_client_device);
            ctx.w.remove_reclaim_checkbox.set_active(false);
        });
    }
    row.append(&remove);

    row
}

/// `backup-destination-unattested-mark` — the review copy on a raised row.
fn build_unattested_mark() -> gtk::Label {
    let mark = gtk::Label::new(Some(s::BACKUP_DESTINATION_UNATTESTED_MARK));
    mark.set_halign(gtk::Align::Start);
    mark.set_xalign(0.0);
    mark.set_wrap(true);
    mark.add_css_class("caption");
    set_test_id(&mark, ids::BACKUP_DESTINATION_UNATTESTED_MARK);
    mark
}

/// `backup-destination-keep-button` — the verdict is an at-rest write to the
/// owner's own `fauna.state.backup`, so it is gated as one.
fn build_keep_button() -> gtk::Button {
    let keep = gtk::Button::with_label(s::BACKUP_DESTINATION_KEEP_BUTTON);
    keep.set_valign(gtk::Align::Center);
    set_test_id(&keep, ids::BACKUP_DESTINATION_KEEP_BUTTON);
    crate::offline_gate::declare_wire_kind(&keep, "fauna.account.state.put");
    keep
}

/// The row's human label: the `display_name`, else the destination URL's host
/// (a destination with no name falls back).
fn destination_label(dest: &BackupDestination) -> String {
    fauna_core::format::backup_destination_label(
        dest.display_name.as_deref(),
        &dest.destination_nest_url,
    )
}

// ── Shared-call sequencing (the only logic here; everything is shared Rust) ──
//
// These run on the tokio runtime, so they take only `Send` inputs — the
// `Arc<NestClient>` WS handle + the owner's hex secret (a `String`). The page's
// `Rc<FaunaClient>` is `!Send` and never crosses the spawn boundary; the
// `nest`/`secret_hex` are extracted on the GTK thread first (mirrors how
// `settings/linked_nests.rs` captures its `Arc<Machine>`, not the `Rc`).

type Nest = Arc<fauna_client::NestClient>;

fn secret_from_hex(secret_hex: &str) -> Result<[u8; 32], String> {
    fauna_core::hex32::decode(secret_hex).map_err(|e| format!("decode secret: {e}"))
}

/// The account plane's `fauna.state.backup` door every read and write below
/// goes through (`account_runtime::backup_seam`).
fn backup_seam() -> Arc<dyn BackupStateStore> {
    crate::account_runtime::backup_seam()
}

/// This box's destination list, read from the account plane
/// (`fauna.state.backup`, re-filing a list still keyed under an older box id).
/// The plane read needs no owner secret.
async fn load_destinations(nest: Nest) -> LoadedDestinations {
    let source_nest = bound_source_nest(&nest).await?;
    let state = load_backup_state_refiled(backup_seam().as_ref(), &nest, source_nest)
        .await
        .map_err(|e| e.to_string())?;
    Ok((state.backup.destinations, state.marks))
}

/// Keep a raised row: record the owner's `Kept` verdict on its open mark. The
/// write is shared (`keep_backup_destination_at_rest`) so it rides the one
/// `fauna.state.backup` write door and a concurrent device cannot undo it; the
/// nest is read only to prove which box's list the mark sits on. A no-op keep
/// (another device already adjudicated the row) is not an error — the row is
/// no longer raised either way. Mirrors tui's `keep_destination`.
async fn keep_destination(nest: Nest, id: String) -> MutationResult {
    let source_nest = bound_source_nest(&nest).await?;
    keep_backup_destination_at_rest(backup_seam().as_ref(), source_nest, &id)
        .await
        .map(|_| Vec::new())
        .map_err(|e| e.to_string())
}

async fn add_destination(
    nest: Nest,
    secret_hex: String,
    url: String,
    name: String,
) -> MutationResult {
    let secret = secret_from_hex(&secret_hex)?;
    // The writer the destination authorizes is the id this connection proved —
    // and the box whose list the new row lands in.
    let source_nest_id = bound_source_nest(&nest).await?;
    // Resolve + enroll in one shared call — backup_enroll.rs § Ordering and
    // crash-safety owns the sequence; blank name ⇒ the destination's handle
    // domain, per backups.md § State & data shape → Create step 1.
    fauna_sync_engine::segment_backup::resolve_and_enroll_destination(
        nest,
        backup_seam().as_ref(),
        secret,
        url,
        name,
        source_nest_id,
    )
    .await
}

/// Enroll this device as a client custodian — the shared three-step sequence.
///
/// **No resolve step and no connection to a second nest**, unlike
/// [`add_destination`]: a custodian has no address to resolve, which is the
/// property that makes the kind pull rather than be pushed to
/// (`backups.md` § Custodian contract, question 2). The whole sequence — the
/// registry write, the `fauna.state.backup` write, the crash-safety ordering, and the
/// deliberate absence of a `NestBackupKey` grant — belongs to
/// [`enroll_client_custodian`]; this supplies only what the *shell* knows.
///
/// The device id is read here, on the runtime, rather than on the GTK gesture
/// path: [`crate::sync::device_id`] opens the sync engines' `device.db`, and it
/// must be **that** id — the same one this box's file-sync engines present — or
/// the source nest keys the custodian's status row on a device nothing drives.
/// Its absence is surfaced, never defaulted: the shared enroll refuses a blank
/// id for exactly that reason, so this reaches the same refusal by the same path
/// rather than inventing a second message for it.
async fn enroll_custodian(
    nest: Nest,
    name: String,
    capacity_cap_bytes: Option<u64>,
) -> MutationResult {
    let source_nest = bound_source_nest(&nest).await?;
    let custodian_device_id = crate::sync::device_id()
        .map(|d| fauna_core::hex32::encode(&d))
        .inspect_err(|e| tracing::warn!("backups: could not read this device's sync id: {e}"))
        .unwrap_or_default();
    enroll_client_custodian(
        nest,
        backup_seam().as_ref(),
        source_nest,
        CustodianEnrollment {
            destination_id: uuid::Uuid::new_v4().to_string(),
            custodian_device_id,
            // Blank name ⇒ the device id, so a row is never nameless; the
            // fallback lives inside the shared enroll.
            display_name: name,
            capacity_cap_bytes,
        },
    )
    .await
    .map_err(|e| e.to_string())
}

async fn edit_destination(
    nest: Nest,
    secret_hex: String,
    id: String,
    url: String,
    name: String,
) -> MutationResult {
    let secret = secret_from_hex(&secret_hex)?;
    let source_nest = bound_source_nest(&nest).await?;
    let state = fauna_sync_engine::segment_backup::edit_destination(
        backup_seam().as_ref(),
        source_nest,
        secret,
        id,
        url,
        name,
    )
    .await?;
    Ok(state.backup.destinations)
}

async fn remove_destination(nest: Nest, id: String) -> MutationResult {
    let source_nest = bound_source_nest(&nest).await?;
    // Deregisters from the source nest's own registry, then drops the
    // `fauna.state.backup` row (the single atomic decision point). No *destination*-side call — the
    // coordinator reconciles the offsite deregistration on its next pass
    // (backups.md § Remove); it does not revoke the destination-side
    // nest-writer grant either (backup_enroll.rs module docs).
    deregister_backup_destination(nest, backup_seam().as_ref(), source_nest, &id)
        .await
        .map_err(|e| e.to_string())
}

/// [`remove_destination`], then — when `reclaim` is set — free this device's
/// whole sealed custodian store in the same gesture
/// (`backup-destination-remove-reclaim-checkbox`, `backups.md` § Manage
/// backup destinations → *Reclaim this device's copy*). Mirrors tui's
/// `Op::Remove` ordering.
///
/// **The reclaim runs AFTER the deregister, never before**: the removal is
/// what makes the store orphaned, and freeing it first would leave a live
/// custody row pointing at bytes already gone if the deregister then failed.
async fn remove_destination_and_maybe_reclaim(
    nest: Nest,
    id: String,
    reclaim: bool,
) -> MutationResult {
    let destinations = remove_destination(nest, id).await?;
    if !reclaim {
        return Ok(destinations);
    }
    // `ctx.rt` (the caller's spawn runtime) and the agent's own runtime are the
    // SAME handle (`sync_agent::install` seeds `AgentUi::rt` from
    // `FaunaClient::runtime_handle()`), so awaiting the provisioner here needs
    // no runtime hop.
    let Some((provisioner, _rt)) = crate::sync_agent::provisioner_and_runtime() else {
        // No agent on this platform: nothing to reclaim, and the removal
        // itself still landed — an absent agent must never read as a removal
        // failure.
        return Ok(destinations);
    };
    match provisioner.reclaim_custodian_store().await {
        Ok(outcome) if outcome.still_hosting => Err(s::BACKUP_RECLAIM_AFTER_REMOVE_FAILED
            .replace("{reason}", s::BACKUP_RECLAIM_STILL_HOSTING)),
        // The removal DID land, so the message says so — reporting a bare
        // reclaim failure over a list that still paints the removed row is the
        // one reading a user cannot recover from. The surviving store's own
        // `backup-orphaned-store-row` is both the honest state and the way to
        // retry.
        Err(e) => Err(s::BACKUP_RECLAIM_AFTER_REMOVE_FAILED.replace("{reason}", &e.to_string())),
        Ok(_) => Ok(destinations),
    }
}

/// Free this device's whole sealed custodian store — the confirmed
/// standalone `backup-destination-reclaim-button` action (not tied to a
/// remove). Mirrors tui's `Op::ReclaimStore`.
fn dispatch_reclaim(ctx: &Rc<Ctx>) {
    let Some((provisioner, rt)) = crate::sync_agent::provisioner_and_runtime() else {
        // `convention 11`: a command this app cannot honour fails loudly,
        // never silently — reachable only if the row painted between the
        // agent tearing down and this click, an unlikely but real race.
        ctx.w.error_label.add_css_class("error");
        crate::settings::render_error_label(&ctx.w.error_label, Some(s::BACKUP_RECLAIM_NO_AGENT));
        return;
    };
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &rt,
        move || async move { provisioner.reclaim_custodian_store().await },
        move |result| {
            // A refusal is a reported outcome, not an error (the agent's own
            // replica had not finished stopping, so nothing was deleted) — the
            // row is still there, because the store still is.
            let error = match result {
                Ok(outcome) if outcome.still_hosting => {
                    Some(s::BACKUP_RECLAIM_STILL_HOSTING.to_string())
                }
                Ok(_) => None,
                Err(e) => Some(e.to_string()),
            };
            match &error {
                Some(_) => ctx_render.w.error_label.add_css_class("error"),
                None => refresh_orphaned_store(&ctx_render),
            }
            crate::settings::render_error_label(&ctx_render.w.error_label, error.as_deref());
        },
    );
}

/// How a re-seed ended, handed from the runtime to the GTK thread.
enum ReseedDone {
    /// The ceremony stopped before its verdict; nothing was made live.
    Stopped(String),
    /// The driver's verdict, plus the post-ceremony re-enrollment's failure
    /// when it ran and failed.
    Finished {
        result: fauna_client_backup::reseed::ReseedOutcome,
        reenroll_error: Option<String>,
    },
}

/// The confirmed re-seed (`backups.md` § Restore after losing the nest): the
/// agent's job through the shared `await_agent_reseed`, then the shared
/// post-ceremony duty `reenroll_custodian_after_reseed`. Nothing about the
/// ceremony's order is decided here; tui runs the same two calls.
fn dispatch_reseed(ctx: &Rc<Ctx>) {
    let Some((provisioner, rt)) = crate::sync_agent::provisioner_and_runtime() else {
        ctx.w.error_label.add_css_class("error");
        crate::settings::render_error_label(&ctx.w.error_label, Some(s::BACKUP_RESEED_NO_AGENT));
        return;
    };
    let secret = match secret_from_hex(ctx.client.secret_hex()) {
        Ok(secret) => secret,
        Err(e) => {
            ctx.w.error_label.add_css_class("error");
            crate::settings::render_error_label(
                &ctx.w.error_label,
                Some(&s::BACKUP_RESEED_FAILED.replace("{reason}", &e)),
            );
            return;
        }
    };
    let nest = ctx.client.nest_rpc().clone();
    let device_hex = this_device_hex();

    ctx.reseed_running.set(true);
    paint_reseed_buttons(ctx);
    crate::settings::render_error_label(&ctx.w.error_label, None);
    ctx.w.reseed_result.set_text(s::BACKUP_RESEED_RUNNING);
    ctx.w.reseed_result.set_visible(true);

    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &rt,
        move || async move {
            use fauna_client_sync::reseed_wire::{AGENT_RESEED_POLL, await_agent_reseed};
            // Held in its own zeroize-on-drop type; the one bare copy is the
            // IPC frame's, which zeroizes itself too.
            let key = fauna_core::crypto::NestBackupKey::derive(&secret);
            // The target pre-create, here in the seed-holding app
            // (`writer-signed-change-records.md` ruling (7)(a)(i)).
            let files = fauna_client_folders::FoldersClient::new(nest.clone());
            let custody = crate::account_runtime::folder_key_store();
            let prepare = |names: Vec<String>| async move {
                fauna_client_folders::prepare_reseed_targets_logged(&files, &*custody, &names)
                    .await;
            };
            let result = await_agent_reseed(
                provisioner.as_ref(),
                prepare,
                key.to_bytes(),
                AGENT_RESEED_POLL,
            )
            .await;
            drop(key);
            let result = match result {
                Ok(result) => result,
                Err(stop) => return ReseedDone::Stopped(stop.to_string()),
            };
            // The re-enroll judges against this box's list as the account
            // plane holds it now, not the page's last paint.
            let store = backup_seam();
            let reenroll_error = match bound_source_nest(&nest).await {
                Err(e) => Some(e),
                Ok(source_nest) => match store.backup_state(source_nest).await {
                    Err(e) => Some(e.to_string()),
                    Ok(state) => reenroll_custodian_after_reseed(
                        nest,
                        store.as_ref(),
                        source_nest,
                        &result,
                        &device_hex,
                        &state.backup.destinations,
                        || uuid::Uuid::new_v4().to_string(),
                    )
                    .await
                    .and_then(Result::err)
                    .map(|e| e.to_string()),
                },
            };
            ReseedDone::Finished {
                result,
                reenroll_error,
            }
        },
        move |done| render_reseed(&ctx_render, done),
    );
}

/// Paint a re-seed's end (GTK main thread).
fn render_reseed(ctx: &Rc<Ctx>, done: ReseedDone) {
    ctx.reseed_running.set(false);
    paint_reseed_buttons(ctx);
    match done {
        ReseedDone::Stopped(reason) => {
            ctx.w.reseed_result.set_visible(false);
            ctx.w.error_label.add_css_class("error");
            crate::settings::render_error_label(
                &ctx.w.error_label,
                Some(&s::BACKUP_RESEED_FAILED.replace("{reason}", &reason)),
            );
        }
        ReseedDone::Finished {
            result,
            reenroll_error,
        } => {
            let text = fauna_client_backup::reseed::result_lines(&result)
                .iter()
                .map(|line| line.resolve_nested(crate::i18n::strings::lookup))
                .collect::<Vec<_>>()
                .join("\n");
            ctx.w.reseed_result.set_text(&text);
            ctx.w.reseed_result.set_visible(true);
            match reenroll_error {
                // The data IS back; say so, and say what did not happen. No
                // refresh: its successful load would clear this message.
                Some(reason) => {
                    ctx.w.error_label.add_css_class("error");
                    crate::settings::render_error_label(
                        &ctx.w.error_label,
                        Some(&s::BACKUP_RESEED_REENROLL_FAILED.replace("{reason}", &reason)),
                    );
                }
                // Re-list: the re-enrolled row appears and the orphaned-store
                // verdict is re-measured against it.
                None => refresh(ctx),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_builds_without_client() {
        crate::testid::run_on_gtk_thread(|| {
            // No registered client → static placeholder, but every ui.yaml ID present.
            let _ = build_form();
            let _ = build_remove_modal();
            let _ = build_orphaned_row();
            let _ = build_reclaim_modal();
            let _ = build_reseed_modal();
            let _ = build_reseed_result();
        });
    }

    /// The post-succession review pair carries its ui.yaml ids and the shared
    /// copy, and Keep is offline-gated as the at-rest write it is
    /// (`succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across). Whether a row is raised is the shared
    /// `DestinationUnattestedMark::row_is_raised`, pinned in `fauna-core`.
    #[test]
    fn the_review_pair_carries_its_ids_and_copy() {
        crate::testid::run_on_gtk_thread(|| {
            crate::offline_gate::reset_for_test("connected");
            let mark = build_unattested_mark();
            assert!(
                crate::testid::find_by_test_id(&mark, ids::BACKUP_DESTINATION_UNATTESTED_MARK)
                    .is_some()
            );
            assert_eq!(mark.text(), s::BACKUP_DESTINATION_UNATTESTED_MARK);
            let keep = build_keep_button();
            assert!(
                crate::testid::find_by_test_id(&keep, ids::BACKUP_DESTINATION_KEEP_BUTTON)
                    .is_some()
            );
            assert_eq!(
                keep.label().as_deref(),
                Some(s::BACKUP_DESTINATION_KEEP_BUTTON)
            );
            assert!(keep.is_sensitive(), "a connected page arms Keep");
            // The process has one GTK thread, so the gate's state outlives this
            // body: leave it offline, as `walk.rs`'s fixtures expect.
            crate::offline_gate::reset_for_test("disconnected");
        });
    }

    /// The orphaned row offers the restore ahead of the reclaim, and the
    /// confirm modal paints both of its ui.yaml ids (`backups.md` § Restore
    /// after losing the nest).
    #[test]
    fn the_orphaned_row_offers_the_reseed_ahead_of_the_reclaim() {
        crate::testid::run_on_gtk_thread(|| {
            let (row, _, _, _) = build_orphaned_row();
            let names = crate::testid::widget_names(&row);
            let reseed = names
                .iter()
                .position(|n| n == "backup-destination-reseed-button")
                .expect("the orphaned row carries the re-seed button");
            let reclaim = names
                .iter()
                .position(|n| n == "backup-destination-reclaim-button")
                .expect("the orphaned row carries the reclaim button");
            assert!(reseed < reclaim, "restore first, reclaim second: {names:?}");

            let (modal, _, _) = build_reseed_modal();
            let names = crate::testid::widget_names(&modal);
            for id in [
                "backup-destination-reseed-confirm-modal",
                "backup-destination-reseed-confirm-button",
                "backup-destination-reseed-cancel-button",
            ] {
                assert!(names.contains(&id.to_string()), "{id} must be present");
            }
        });
    }

    /// The dialog paints every id ui.yaml scopes to it, including the two the
    /// client-custodian kind added.
    #[test]
    fn the_form_exposes_its_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let f = build_form();
            let names = crate::testid::widget_names(&f.form);
            for id in [
                "backup-destination-add-modal",
                "backup-destination-kind-select",
                "backup-destination-url-input",
                "backup-destination-name-input",
                "backup-destination-capacity-input",
                "backup-destination-add-confirm-button",
                "backup-destination-add-cancel-button",
            ] {
                assert!(names.contains(&id.to_string()), "{id} must be present");
            }
        });
    }

    /// Picking "This device" swaps the dialog's per-kind fields: a custodian has
    /// no address at all (`backups.md` § Custodian contract, question 1), so the
    /// URL box must not be offered — and the capacity cap, the kind's only knob,
    /// must be.
    #[test]
    fn the_kind_select_swaps_the_url_box_for_the_capacity_cap() {
        crate::testid::run_on_gtk_thread(|| {
            let f = build_form();
            // `WidgetExt::is_visible` is the EFFECTIVE visibility (the widget and
            // every ancestor), and the dialog is an inline reveal that starts
            // hidden — so open it, which is also the only state a user sees these
            // fields in.
            f.form.set_visible(true);
            f.sync_kind_fields();
            // Nest is the default, because it is the kind that actually
            // satisfies "off-site".
            assert_eq!(f.kind_select.selected_kind(DESTINATION_KIND_NEST), "nest");
            assert!(
                WidgetExt::is_visible(&f.url_input),
                "the nest kind has an address"
            );
            assert!(!WidgetExt::is_visible(&f.capacity_input));
            assert!(!WidgetExt::is_visible(&f.custodian_note));

            f.kind_select.select_kind(DESTINATION_KIND_CLIENT_DEVICE);
            f.sync_kind_fields();
            assert!(
                !WidgetExt::is_visible(&f.url_input),
                "a custodian has no address, so no URL box may be offered"
            );
            assert!(WidgetExt::is_visible(&f.capacity_input));
            // § Threat model requires the honest statement AT opt-in.
            assert!(
                WidgetExt::is_visible(&f.custodian_note),
                "the full-offline-corpus exposure must be stated where the user opts in"
            );
        });
    }

    /// An unrecognised kind (a newer client wrote the row) must not be silently
    /// rewritten into one this build does implement. `select_kind` leaves the
    /// selection alone, and the edit path paints the control disabled and never
    /// reads it back — the two halves of "the kind is not an editable property".
    #[test]
    fn an_unknown_kind_does_not_repoint_the_select() {
        crate::testid::run_on_gtk_thread(|| {
            let f = build_form();
            f.kind_select.select_kind(DESTINATION_KIND_CLIENT_DEVICE);
            let before = f.kind_select.dd.selected();
            f.kind_select.select_kind("s3");
            assert_eq!(
                f.kind_select.dd.selected(),
                before,
                "an unimplemented kind must not move the selection onto an implemented one"
            );
        });
    }

    /// The select's options are the shared catalog's, so the option a user picks
    /// is the same text the resulting row's badge renders.
    #[test]
    fn the_kind_select_offers_exactly_the_shared_catalog() {
        crate::testid::run_on_gtk_thread(|| {
            let f = build_form();
            let catalog: Vec<String> = fauna_core::format::backup_destination_kind_options()
                .into_iter()
                .map(|o| o.value)
                .collect();
            assert_eq!(f.kind_select.values(), catalog.as_slice());
        });
    }

    fn custodian_row(id: &str, cap: Option<u64>) -> BackupDestination {
        BackupDestination {
            destination_id: id.to_string(),
            kind: DESTINATION_KIND_CLIENT_DEVICE.to_string(),
            custodian_device_id: Some("dead".repeat(16)),
            capacity_cap_bytes: cap,
            display_name: Some(id.to_string()),
            ..Default::default()
        }
    }

    /// A capacity the shell cannot read is a refusal the user sees, never a
    /// substituted default — guessing a cap is how a device's disk fills. The
    /// parse itself is shared (`parse_byte_size`); what this pins is that linux
    /// **reads** it rather than defaulting.
    #[test]
    fn an_unreadable_capacity_has_a_message_and_a_readable_one_round_trips() {
        // The shared parser is the inverse of `byte_size`, so its units are
        // 1024-based under the familiar labels — 50 GB is 50 GiB of bytes.
        assert_eq!(
            fauna_core::format::parse_byte_size("50 GB"),
            Some(50 * 1024 * 1024 * 1024)
        );
        assert_eq!(fauna_core::format::parse_byte_size("banana"), None);
        // The refusal the submit path surfaces on `error-message`.
        assert!(!s::BACKUP_DESTINATION_CAPACITY_INVALID.is_empty());
        // A stored cap re-reads as text the shared parser accepts again, which is
        // what makes the edit dialog's prefill lossless.
        let typed = crate::i18n::byte_size(50 * 1024 * 1024 * 1024);
        assert_eq!(
            fauna_core::format::parse_byte_size(&typed),
            Some(50 * 1024 * 1024 * 1024)
        );
    }

    /// The sole-client warning tracks whether any off-site copy exists at all —
    /// the predicate is shared, so this pins that linux asks it rather than
    /// re-deriving the Inert arm.
    #[test]
    fn the_sole_client_warning_predicate_is_the_shared_one() {
        let mine = custodian_row("laptop", Some(1 << 30));
        let offsite = BackupDestination {
            destination_id: "friend".into(),
            destination_nest_url: "https://nest.example".into(),
            ..Default::default()
        };
        assert!(every_destination_is_a_client_device(std::slice::from_ref(
            &mine
        )));
        assert!(!every_destination_is_a_client_device(&[mine, offsite]));
        assert!(!every_destination_is_a_client_device(&[]));
    }

    /// A custodian row is badged and carries usage; a nest row is badged and does
    /// not. The badge is on **every** row — that is what makes "this is not
    /// off-site" visible rather than inferred from an absent URL column.
    #[test]
    fn a_custodian_row_is_badged_and_carries_usage_where_a_nest_row_does_not() {
        let nest_badge = crate::i18n::backup_destination_kind("nest");
        let device_badge = crate::i18n::backup_destination_kind(DESTINATION_KIND_CLIENT_DEVICE);
        assert_ne!(nest_badge, device_badge);
        // An unknown kind renders AS ITSELF rather than collapsing into "nest" —
        // the user needs to see what their older build cannot drive.
        let unknown = crate::i18n::backup_destination_kind("s3");
        assert!(unknown.contains("s3"), "got {unknown:?}");
        assert_ne!(unknown, nest_badge);
    }

    /// Cap-reached is READ from `cap_state`, never inferred from `held >= cap`:
    /// a pass that stopped at its cap ends *below* it, so inference would render
    /// a stalled backup as healthy-with-room.
    #[test]
    fn usage_reads_cap_reached_rather_than_inferring_it() {
        let stopped_below_cap = crate::i18n::backup_usage(
            Some(900),
            Some(1_000),
            Some(fauna_core::data::CAP_STATE_REACHED),
        );
        let genuinely_ok =
            crate::i18n::backup_usage(Some(900), Some(1_000), Some(fauna_core::data::CAP_STATE_OK));
        assert_ne!(
            stopped_below_cap, genuinely_ok,
            "identical byte counts must still read differently when the pass stopped at its cap"
        );
    }

    fn status(last_upload_time: Option<u64>, backlog_count: u32) -> BackupDestinationStatus {
        BackupDestinationStatus {
            destination_id: "d1".to_string(),
            last_upload_time,
            backlog_count,
            // Wire tail: the row is the nest's projection since the leg-(d)
            // repoint, so struct-update keeps this fixture merge-clean as the
            // wire type grows a field.
            ..Default::default()
        }
    }

    #[test]
    fn last_upload_text_never_when_no_status_or_no_upload() {
        // No status read yet, an explicit `None`, and the degenerate epoch-0 all
        // render the "never" baseline.
        assert_eq!(
            last_upload_text(None),
            s::BACKUP_DESTINATION_LAST_UPLOAD_NEVER
        );
        assert_eq!(
            last_upload_text(Some(&status(None, 0))),
            s::BACKUP_DESTINATION_LAST_UPLOAD_NEVER
        );
        assert_eq!(
            last_upload_text(Some(&status(Some(0), 0))),
            s::BACKUP_DESTINATION_LAST_UPLOAD_NEVER
        );
    }

    #[test]
    fn last_upload_text_live_when_uploaded() {
        // A real timestamp takes the live branch (relative-time text is non-
        // deterministic, so assert only that it is no longer "never").
        let live = last_upload_text(Some(&status(Some(1_700_000_000), 0)));
        assert_ne!(live, s::BACKUP_DESTINATION_LAST_UPLOAD_NEVER);
    }

    #[test]
    fn backlog_text_counts() {
        assert_eq!(backlog_text(None), s::backup_destination_backlog("0"));
        assert_eq!(
            backlog_text(Some(&status(None, 0))),
            s::backup_destination_backlog("0")
        );
        assert_eq!(
            backlog_text(Some(&status(None, 7))),
            s::backup_destination_backlog("7")
        );
    }

    // ── the client-device arm of the audit surface ───────────────────────
    //
    // A custodian has no address, so the owner-side loop can never sample it:
    // its audit answer arrives on the STATUS ROW as the device's own verdict
    // (`backups.md` § Audit-alert surface → *The client-device arm*).

    fn status_with_audit(
        audit_state: Option<&str>,
        last_audit_passed_at: Option<u64>,
    ) -> BackupDestinationStatus {
        BackupDestinationStatus {
            destination_id: "d1".to_string(),
            audit_state: audit_state.map(str::to_string),
            last_audit_passed_at,
            ..Default::default()
        }
    }

    /// **A custodian that has never self-audited renders ABSENCE, not a
    /// verdict.** Reading silence as a pass would render an unverified copy as
    /// verified; reading it as a failure would raise a fleet-wide false
    /// data-loss alarm for every custodian that has not self-audited yet.
    #[test]
    fn self_audit_text_absent_renders_not_yet() {
        assert_eq!(
            self_audit_text(None),
            s::BACKUP_DESTINATION_LAST_SELF_AUDIT_NEVER
        );
        assert_eq!(
            self_audit_text(Some(&status_with_audit(None, None))),
            s::BACKUP_DESTINATION_LAST_SELF_AUDIT_NEVER
        );
    }

    /// The custodian's cell never borrows the owner-side wording — rendering
    /// "Last checked" over a self-report would let it wear the words of an
    /// independent verification it never received.
    #[test]
    fn self_audit_text_never_wears_the_owner_side_wording() {
        let text = self_audit_text(Some(&status_with_audit(
            Some(fauna_core::data::AUDIT_STATE_OK),
            u64::try_from(now_secs()).ok(),
        )));
        assert_ne!(text, s::BACKUP_DESTINATION_LAST_SELF_AUDIT_NEVER);
        assert!(text.starts_with("Self-checked:"), "got {text:?}");
        assert!(!text.contains("Last checked"), "got {text:?}");
    }

    /// A **reported failure is loud** — the only failure signal that exists for
    /// a kind the owner cannot sample.
    #[test]
    fn a_custodian_reporting_its_own_failure_is_flagged_for_a_banner() {
        let dest = custodian_row("d1", None);
        let status = status_with_audit(
            Some(fauna_core::data::AUDIT_STATE_FAILED),
            // Deliberately stale: the last time it PASSED. A failure never
            // advances that clock, so a flagged row must not read as fresh.
            Some(1_700_000_000),
        );
        let mut statuses = HashMap::new();
        statuses.insert("d1".to_string(), status.clone());
        let flagged = self_reported_alert_destinations(std::slice::from_ref(&dest), &statuses);
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].destination_id, "d1");

        // The row must not read as fresh: it renders the stale PASSED time, not
        // "checked seconds ago" and not "not yet". Red-verified against
        // PROBE-71-A (flipping the fixture above to `now_secs()`).
        let audit_time = self_audit_text(Some(&status));
        assert!(
            audit_time.starts_with("Self-checked:"),
            "got {audit_time:?}"
        );
        assert!(audit_time.contains("2023"), "got {audit_time:?}");
        assert_ne!(audit_time, s::BACKUP_DESTINATION_LAST_SELF_AUDIT_NEVER);
    }

    /// An unrecognised verdict a NEWER client wrote stays quiet — the
    /// conservative direction is the shared predicate's, not each app's.
    #[test]
    fn an_unrecognised_reported_verdict_stays_quiet() {
        let dest = custodian_row("d1", None);
        let mut statuses = HashMap::new();
        statuses.insert(
            "d1".to_string(),
            status_with_audit(Some("degraded-in-some-newer-way"), None),
        );
        assert!(
            self_reported_alert_destinations(std::slice::from_ref(&dest), &statuses).is_empty()
        );
    }

    /// A nest row is untouched by all of the above: its cell still carries
    /// this client's own independent check, dispatched by kind rather than by
    /// a re-derived guess.
    #[test]
    fn audit_cell_text_dispatches_on_kind() {
        let nest = BackupDestination {
            destination_id: "d1".to_string(),
            kind: DESTINATION_KIND_NEST.to_string(),
            destination_nest_url: "https://a.example".to_string(),
            ..Default::default()
        };
        assert_eq!(
            audit_cell_text(&nest, None, None),
            s::BACKUP_DESTINATION_LAST_AUDIT_NEVER
        );

        let custodian = custodian_row("d2", None);
        let status = status_with_audit(
            Some(fauna_core::data::AUDIT_STATE_OK),
            u64::try_from(now_secs()).ok(),
        );
        let custodian_text = audit_cell_text(&custodian, Some(&status), None);
        assert_ne!(custodian_text, s::BACKUP_DESTINATION_LAST_AUDIT_NEVER);
    }

    /// A passed record carrying one accepted regression whose window closes at
    /// `recoverable_until`.
    fn regressed_record(
        destination_id: &str,
        now: i64,
        recoverable_until: i64,
    ) -> DestinationAuditRecord {
        let mut record = DestinationAuditRecord::never(destination_id);
        record.state.last_passed_at = Some(now);
        record.verdict = Some(fauna_client_backup::audit::AuditVerdict::Passed);
        record.state.accepted_regressions.insert(
            "__mail/ledger".into(),
            fauna_client_backup::audit::AcceptedRegression {
                pinned: 40,
                served: 30,
                observed_at: now,
                floored_at: None,
                recoverable_until: Some(recoverable_until),
            },
        );
        record
    }

    fn named_destination(id: &str, name: &str) -> BackupDestination {
        BackupDestination {
            destination_id: id.to_string(),
            display_name: Some(name.to_string()),
            ..Default::default()
        }
    }

    /// An open recovery window paints the fifth reason, naming the destination
    /// and the days left — on a destination whose audit PASSED.
    #[test]
    fn an_open_recovery_window_paints_a_named_banner() {
        let now = 1_800_000_000;
        let banners = owner_alert_banners(
            &[regressed_record("d1", now, now + 10 * 24 * 60 * 60 + 60)],
            &[named_destination("d1", "Attic")],
            now,
        );
        assert_eq!(banners.len(), 1, "got {banners:?}");
        assert!(banners[0].contains("Attic"), "got {:?}", banners[0]);
        assert!(banners[0].contains("10 more days"), "got {:?}", banners[0]);
    }

    /// A closed window paints nothing: the notice expires by itself.
    #[test]
    fn a_closed_recovery_window_paints_nothing() {
        let now = 1_800_000_000;
        let banners = owner_alert_banners(
            &[regressed_record("d1", now, now - 1)],
            &[named_destination("d1", "Attic")],
            now,
        );
        assert!(banners.is_empty(), "got {banners:?}");
    }

    /// A standing alerting verdict and an open window are two reasons: both paint.
    #[test]
    fn a_standing_verdict_and_an_open_window_paint_two_banners() {
        let now = 1_800_000_000;
        let mut record = regressed_record("d1", now, now + 3 * 24 * 60 * 60);
        record.verdict = Some(fauna_client_backup::audit::AuditVerdict::Overdue {
            since_secs: 9 * 24 * 60 * 60,
        });
        let banners = owner_alert_banners(&[record], &[named_destination("d1", "Attic")], now);
        assert_eq!(banners.len(), 2, "got {banners:?}");
    }
}
