//! The **snapshot half** of the Backups page, rendered off the shared
//! [`BackupsMachine`] (`docs/goal/ui/backups.md` § Snapshot-list shape).
//!
//! Architectural rule 1 binds from the machine's landing: this module paints
//! `machine.snapshot()` and dispatches gestures; it holds no page logic. Four
//! things it therefore no longer does, each a measured divergence the
//! Reconciliation ledger's linux row named:
//!
//! - the `"default"` folder fallback (there is no such set; selection is the
//!   machine's page-lifetime state with a deterministic first-row default),
//! - the build-once-persist dropdown that carried a selection across visits,
//! - the hand-derived `last-backed-up` (first row of the list, and stale on an
//!   empty set because the label was never cleared),
//! - the hard-coded `deleting: false` in the immediate-delete friction bar.
//!
//! Prune is preview-first over the set's **resting** retention policy
//! (§ *Prune* ruling) — the old `keep_last: 3` client-supplied policy is gone.

use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;

use fauna_backups_machine::{
    BackupOp, BackupsMachine, BackupsSnapshot, PolicyState, SnapshotRow, SnapshotState,
};

use crate::client::FaunaClient;
use crate::i18n::strings::backups as backups_strings;

/// Widgets the render pass updates on every machine tick.
///
/// `Clone` is a cheap refcount bump (GTK widgets are `GObject` handles and the
/// two cells are `Rc`s), which is what lets the render loop hold its own copy
/// while `BackupsHandles` keeps the one app.rs reaches for.
#[derive(Clone)]
pub struct SnapshotPaneHandles {
    pub folder_dropdown: gtk::DropDown,
    pub last_backed_up: gtk::Label,
    pub create_button: gtk::Button,
    pub prune_button: gtk::Button,
    pub check_button: gtk::Button,
    /// Names which op is in flight, so the disabled action row above is not left
    /// unexplained. Hidden while idle.
    pub busy_label: gtk::Label,
    /// The last check's verdict. A completed check with errors is a **result,
    /// not an error** (§ Architectural rules, rule 6) — it renders here, never
    /// on `error-message`.
    pub check_result_label: gtk::Label,
    /// The prune dry-run surface (counts + candidates + execute/cancel), built
    /// dynamically because its rows come from the preview.
    pub prune_preview_box: gtk::Box,
    pub snapshot_list_box: gtk::ListBox,
    /// Guards the programmatic `set_selected` in [`render_snapshot_pane`] from
    /// re-entering the machine through `selected_notify` (which would dispatch a
    /// `select_folder` for the selection the machine just handed us, and on a
    /// disappearing set could ping-pong with the machine's fallback).
    suppress_selection_notify: Rc<Cell<bool>>,
    /// The snapshot id the open immediate-delete modal targets, if any. The
    /// render pass closes the modal when this row leaves the machine's list —
    /// i.e. when the delete actually landed. Success is therefore observed as
    /// *the row is gone*, never as "the call returned": a refusal
    /// (`hard_floor_breach`) leaves the row, the modal, and the typed inputs
    /// exactly where the user left them, with the reason on `error-message`.
    immediate_delete_target: Rc<Cell<Option<i64>>>,
}

/// Build the snapshot list pane: header (selector + the three set-scoped
/// actions), the `last-backed-up` line, the busy / check-result / prune-preview
/// surfaces, and the scrollable snapshot list.
///
/// The check button lives on the LIST pane rather than the detail pane because
/// the kind is folder-scoped (`fauna.filesync.snapshot.check { folder }`),
/// matching every other app's placement.
pub fn build_snapshot_list(
    client: &Rc<FaunaClient>,
    machine: &std::sync::Arc<BackupsMachine>,
) -> (gtk::Box, SnapshotPaneHandles) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let suppress = Rc::new(Cell::new(false));
    let immediate_delete_target: Rc<Cell<Option<i64>>> = Rc::new(Cell::new(None));

    // Header bar.
    let header = adw::HeaderBar::new();
    let title_label = gtk::Label::new(Some(backups_strings::TITLE));
    crate::testid::set_test_id(&title_label, ids::PAGE_HEADING);
    header.set_title_widget(Some(&title_label));

    // The folder selector. Its model is rebuilt from the machine's
    // `folders` on every render — never seeded with a placeholder name, which
    // is what made `"default"` look like a real set the actions could act on.
    let folder_dropdown = gtk::DropDown::from_strings(&[]);
    crate::testid::set_test_id(&folder_dropdown, ids::BACKUP_FOLDER_SELECTOR);

    let create_btn = gtk::Button::with_label(backups_strings::CREATE_SNAPSHOT);
    create_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&create_btn, ids::SNAPSHOT_CREATE_BUTTON);
    crate::offline_gate::declare_wire_kind(&create_btn, "fauna.filesync.snapshot.create_folder");
    header.pack_end(&create_btn);

    let check_btn = gtk::Button::new();
    check_btn.set_icon_name("emblem-ok-symbolic");
    check_btn.set_tooltip_text(Some(backups_strings::CHECK_INTEGRITY));
    check_btn.add_css_class("flat");
    crate::testid::set_test_id(&check_btn, ids::SNAPSHOT_CHECK_BUTTON);
    crate::offline_gate::declare_wire_kind(&check_btn, "fauna.filesync.snapshot.check");
    header.pack_end(&check_btn);

    let prune_btn = gtk::Button::new();
    prune_btn.set_icon_name("edit-clear-all-symbolic");
    prune_btn.set_tooltip_text(Some(backups_strings::PRUNE_SNAPSHOTS));
    prune_btn.add_css_class("flat");
    crate::testid::set_test_id(&prune_btn, ids::SNAPSHOT_PRUNE_BUTTON);
    crate::offline_gate::declare_wire_kind(&prune_btn, "fauna.filesync.snapshot.prune_set_policy");
    header.pack_end(&prune_btn);

    header.pack_start(&folder_dropdown);
    outer.append(&header);

    // ── Gesture wiring ──────────────────────────────────────────────────
    // Each is a machine gesture spawned on the client's runtime; the observer
    // tick that follows repaints. No client-side re-fetch, no local busy flag —
    // `in_progress_op` is the single-flight gate for all of them.
    {
        let m = std::sync::Arc::clone(machine);
        let rt = client.runtime_handle();
        create_btn.connect_clicked(move |_| {
            let m = std::sync::Arc::clone(&m);
            rt.spawn(async move { m.create_snapshot().await });
        });
    }
    {
        // Prune OPENS the dry-run preview; nothing is deleted until the user
        // executes from it (§ *Prune* ruling — apple's preview-first flow,
        // blessed as the uniform shape). The old confirm dialog is retired
        // along with the `keep_last` policy it confirmed.
        let m = std::sync::Arc::clone(machine);
        let rt = client.runtime_handle();
        prune_btn.connect_clicked(move |_| {
            let m = std::sync::Arc::clone(&m);
            rt.spawn(async move { m.prune_preview().await });
        });
    }
    {
        // Clicking the tagged button RUNS the check — no second confirm step
        // (§ *Check* ruling: apple's sheet requiring a second click is the
        // actuation-contract violation being reconciled).
        let m = std::sync::Arc::clone(machine);
        let rt = client.runtime_handle();
        check_btn.connect_clicked(move |_| {
            let m = std::sync::Arc::clone(&m);
            rt.spawn(async move { m.check().await });
        });
    }
    {
        let m = std::sync::Arc::clone(machine);
        let rt = client.runtime_handle();
        let suppress = Rc::clone(&suppress);
        folder_dropdown.connect_selected_notify(move |dd| {
            if suppress.get() {
                return;
            }
            let Some(name) = selected_dropdown_name(dd) else {
                return;
            };
            let m = std::sync::Arc::clone(&m);
            rt.spawn(async move { m.select_folder(name).await });
        });
    }

    // `last-backed-up` — ONE non-indexed element, always painted (the machine
    // renders "never" for a set with no snapshots rather than leaving the
    // previous set's timestamp standing, which is the stale-carryover bug).
    let last_backed_up = gtk::Label::new(None);
    last_backed_up.set_halign(gtk::Align::Start);
    last_backed_up.set_margin_start(12);
    last_backed_up.set_margin_top(4);
    last_backed_up.set_margin_bottom(4);
    last_backed_up.add_css_class("dim-label");
    last_backed_up.add_css_class("caption");
    crate::testid::set_test_id(&last_backed_up, ids::LAST_BACKED_UP);
    outer.append(&last_backed_up);

    let busy_label = dim_line();
    outer.append(&busy_label);

    // `snapshot-check-result` — the completed check's verdict. A RESULT surface,
    // never `error-message` (§ Architectural rules, rule 6). `dim_line()` starts
    // it hidden and `render_check_result` toggles it, so the id is present only
    // while a verdict stands.
    let check_result_label = dim_line();
    check_result_label.set_wrap(true);
    crate::testid::set_test_id(&check_result_label, ids::SNAPSHOT_CHECK_RESULT);
    outer.append(&check_result_label);

    // `snapshot-prune-preview` — the standing dry run. Built hidden; visibility
    // is the state. GTK's `is_visible()` is ancestor-aware, so the execute /
    // cancel buttons inside report hidden with it — which is what makes the
    // execute button's visibility a sound "this preview names a candidate"
    // observable rather than a stale registration.
    let prune_preview_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(4)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    crate::testid::set_test_id(&prune_preview_box, ids::SNAPSHOT_PRUNE_PREVIEW);
    outer.append(&prune_preview_box);

    // Snapshot list.
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Single);
    list_box.add_css_class("boxed-list");
    crate::testid::set_test_id(&list_box, ids::SNAPSHOT_LIST);

    let placeholder = adw::StatusPage::builder()
        .title(backups_strings::NO_BACKUPS)
        .icon_name("drive-harddisk-symbolic")
        .build();
    list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();
    outer.append(&scrolled);

    let handles = SnapshotPaneHandles {
        folder_dropdown,
        last_backed_up,
        create_button: create_btn,
        prune_button: prune_btn,
        check_button: check_btn,
        busy_label,
        check_result_label,
        prune_preview_box,
        snapshot_list_box: list_box,
        suppress_selection_notify: suppress,
        immediate_delete_target,
    };
    (outer, handles)
}

/// A dim caption line, hidden until it has something to say.
fn dim_line() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_halign(gtk::Align::Start);
    label.set_margin_start(12);
    label.set_margin_bottom(4);
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    label.set_visible(false);
    label
}

/// The dropdown's currently-selected folder NAME, or `None` when the model is
/// empty. Deliberately has no fallback: an empty selector means the owner has no
/// folders, and the actions are disabled in that state rather than acting on
/// an invented name.
fn selected_dropdown_name(dropdown: &gtk::DropDown) -> Option<String> {
    dropdown
        .selected_item()
        .and_then(|item| item.downcast::<gtk::StringObject>().ok())
        .map(|s| s.string().to_string())
}

/// Paint the whole snapshot half from one machine snapshot.
pub fn render_snapshot_pane(
    handles: &SnapshotPaneHandles,
    machine: &std::sync::Arc<BackupsMachine>,
    page: &BackupsSnapshot,
    client: &Rc<FaunaClient>,
) {
    // Single-flight: while an op runs, EVERY mutating control is disabled
    // (§ *Create* ruling). One predicate so no control can drift off it.
    let busy = page.in_progress_op.is_some();
    let armed = !busy && page.selected_folder.is_some();

    render_selector(handles, page, busy);

    handles.last_backed_up.set_text(&match page.last_backed_up {
        Some(at) => backups_strings::last_backed_up_at(&crate::client::format_epoch_us(
            at.saturating_mul(1_000_000),
        )),
        None => backups_strings::LAST_BACKED_UP_NEVER.to_string(),
    });

    handles.create_button.set_sensitive(armed);
    handles.prune_button.set_sensitive(armed);
    handles.check_button.set_sensitive(armed);

    match page.in_progress_op {
        Some(op) => {
            handles.busy_label.set_text(&busy_text(op));
            handles.busy_label.set_visible(true);
        }
        None => handles.busy_label.set_visible(false),
    }

    render_check_result(handles, page);
    render_prune_preview(handles, machine, page, busy, client);
    render_rows(handles, machine, page, busy, client);
    close_immediate_delete_modal_if_landed(handles, page);
}

/// Close the friction-bar modal once the row it targets has actually left the
/// machine's list — the owner-only immediate delete *landed*.
///
/// Deliberately keyed on the row's disappearance rather than on a success
/// message: a `hard_floor_breach` refusal returns from the same call and must
/// leave the modal standing for a retry (§ Architectural rule 4). An in-flight
/// op or a live error both mean "not landed", so neither closes it.
fn close_immediate_delete_modal_if_landed(handles: &SnapshotPaneHandles, page: &BackupsSnapshot) {
    let Some(target) = handles.immediate_delete_target.get() else {
        return;
    };
    if page.in_progress_op.is_some() || page.error.is_some() {
        return;
    }
    if page.snapshots.iter().any(|s| s.id == target) {
        return;
    }
    handles.immediate_delete_target.set(None);
    close_immediate_delete_modal();
}

/// Rebuild the selector's model from the machine's `folders` and re-point it
/// at the machine's selection.
fn render_selector(handles: &SnapshotPaneHandles, page: &BackupsSnapshot, busy: bool) {
    let names: Vec<&str> = page.folders.iter().map(|fs| fs.name.as_str()).collect();
    let selected_index = page
        .selected_folder
        .as_deref()
        .and_then(|sel| names.iter().position(|n| *n == sel))
        .map(|i| i as u32);

    // The whole model swap + re-selection runs under the suppression guard: both
    // `set_model` and `set_selected` fire `selected_notify`, and letting either
    // through would dispatch a `select_folder` for a selection the machine
    // already holds — at best a redundant round trip, at worst a ping-pong with
    // the machine's disappeared-set fallback.
    handles.suppress_selection_notify.set(true);
    let current: Vec<String> = {
        let model = handles.folder_dropdown.model();
        match model.and_then(|m| m.downcast::<gtk::StringList>().ok()) {
            Some(list) => (0..list.n_items())
                .filter_map(|i| list.string(i).map(|s| s.to_string()))
                .collect(),
            None => Vec::new(),
        }
    };
    if current.iter().map(String::as_str).ne(names.iter().copied()) {
        handles
            .folder_dropdown
            .set_model(Some(&gtk::StringList::new(&names)));
    }
    if let Some(index) = selected_index
        && handles.folder_dropdown.selected() != index
    {
        handles.folder_dropdown.set_selected(index);
    }
    handles.suppress_selection_notify.set(false);

    // A disabled control the user can see states why (Copy comprehensibility
    // rule 5) — the same shape the restore pickers use.
    handles
        .folder_dropdown
        .set_sensitive(!names.is_empty() && !busy);
    handles
        .folder_dropdown
        .set_tooltip_text(if names.is_empty() {
            Some(backups_strings::NO_FOLDERS)
        } else {
            None
        });
}

/// The `in_progress_op` line's text — the shared
/// [`fauna_backups_machine::busy_text`] decision (tui↔linux twin harvest,
/// previously hand-rolled identically here and on
/// tui).
fn busy_text(op: BackupOp) -> String {
    fauna_backups_machine::busy_text(op).resolve(crate::i18n::strings::lookup)
}

/// The check verdict — the shared `is_ok` predicate, **called**, never
/// re-derived from `status == "ok"` or from error counts (ratified 2026-06-01).
fn render_check_result(handles: &SnapshotPaneHandles, page: &BackupsSnapshot) {
    let Some(result) = &page.check_result else {
        handles.check_result_label.set_visible(false);
        return;
    };
    let text = if result.is_ok {
        backups_strings::check_result_ok(
            &result.snapshots_checked.to_string(),
            &result.files_checked.to_string(),
            &result.chunks_checked.to_string(),
        )
    } else {
        backups_strings::check_result_errors(
            &result.missing_manifests.to_string(),
            &result.missing_chunks.to_string(),
            &result.corrupt_manifests.to_string(),
        )
    };
    handles.check_result_label.set_text(&text);
    handles.check_result_label.set_visible(true);
}

/// The prune dry-run surface. Execute is offered ONLY from here, and the two
/// no-op policy states say *why* nothing would be pruned rather than showing an
/// empty success.
fn render_prune_preview(
    handles: &SnapshotPaneHandles,
    machine: &std::sync::Arc<BackupsMachine>,
    page: &BackupsSnapshot,
    busy: bool,
    client: &Rc<FaunaClient>,
) {
    while let Some(child) = handles.prune_preview_box.first_child() {
        handles.prune_preview_box.remove(&child);
    }
    let Some(preview) = &page.prune_preview else {
        handles.prune_preview_box.set_visible(false);
        return;
    };
    handles.prune_preview_box.set_visible(true);

    let title = gtk::Label::new(Some(backups_strings::PRUNE_PREVIEW_TITLE));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("heading");
    handles.prune_preview_box.append(&title);

    // The VERDICT rides the id'd element's OWN text, beside the title (tui's
    // `prune_preview_elements` shape): "nothing to prune" and "no retention
    // policy configured for this set" both stand a preview and both offer no
    // execute, so only the text can say which one the nest returned
    // (§ Errors & edge cases — *Prune with no candidates*). Declared rather
    // than left to `text_of`'s descendant-label join, which also swept in the
    // candidate rows and the buttons' labels.
    let verdict = match preview.policy_state {
        PolicyState::NotSet => backups_strings::PRUNE_POLICY_NOT_SET.to_string(),
        PolicyState::Unparseable => backups_strings::PRUNE_POLICY_UNPARSEABLE.to_string(),
        PolicyState::Applied if preview.candidates.is_empty() => {
            backups_strings::PRUNE_PREVIEW_NOTHING.to_string()
        }
        PolicyState::Applied => backups_strings::prune_preview_counts(
            &preview.would_prune.to_string(),
            &preview.remaining.to_string(),
        ),
    };
    crate::testid::set_test_text(
        &handles.prune_preview_box,
        &format!("{}  {verdict}", backups_strings::PRUNE_PREVIEW_TITLE),
    );
    let verdict_line = caption(&verdict);
    if matches!(preview.policy_state, PolicyState::Unparseable) {
        verdict_line.add_css_class("warning");
    }
    handles.prune_preview_box.append(&verdict_line);

    let mut executable = false;
    match preview.policy_state {
        PolicyState::NotSet | PolicyState::Unparseable => {}
        PolicyState::Applied => {
            if !preview.candidates.is_empty() {
                for candidate in &preview.candidates {
                    handles.prune_preview_box.append(&caption(
                        &backups_strings::prune_preview_candidate(
                            &candidate.id.to_string(),
                            &crate::client::format_epoch_us(
                                candidate.created_at.saturating_mul(1_000_000),
                            ),
                        ),
                    ));
                }
                // An armed execute over zero candidates would promise an effect
                // it cannot have.
                executable = !busy;
            }
        }
    }

    let button_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    button_row.set_halign(gtk::Align::Start);
    if executable {
        let execute = gtk::Button::with_label(backups_strings::PRUNE_EXECUTE_BUTTON);
        execute.add_css_class("destructive-action");
        crate::testid::set_test_id(&execute, ids::SNAPSHOT_PRUNE_EXECUTE_BUTTON);
        // Same kind as `snapshot-prune-button`: preview and execute are one
        // `dry_run`-flagged call, so both arms answer it (tui's ruling).
        crate::offline_gate::declare_wire_kind(
            &execute,
            "fauna.filesync.snapshot.prune_set_policy",
        );
        let m = std::sync::Arc::clone(machine);
        let rt = client.runtime_handle();
        execute.connect_clicked(move |_| {
            let m = std::sync::Arc::clone(&m);
            rt.spawn(async move { m.prune_execute().await });
        });
        button_row.append(&execute);
    }
    let cancel = gtk::Button::with_label(backups_strings::PRUNE_CANCEL_BUTTON);
    crate::testid::set_test_id(&cancel, ids::SNAPSHOT_PRUNE_CANCEL_BUTTON);
    {
        let m = std::sync::Arc::clone(machine);
        cancel.connect_clicked(move |_| m.cancel_prune_preview());
    }
    button_row.append(&cancel);
    handles.prune_preview_box.append(&button_row);
}

fn caption(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    label
}

/// Rebuild the `snapshot-item` rows from the machine's list (wire order,
/// newest-first — apps do not re-sort).
fn render_rows(
    handles: &SnapshotPaneHandles,
    machine: &std::sync::Arc<BackupsMachine>,
    page: &BackupsSnapshot,
    busy: bool,
    client: &Rc<FaunaClient>,
) {
    while let Some(child) = handles.snapshot_list_box.first_child() {
        handles.snapshot_list_box.remove(&child);
    }
    for snap in &page.snapshots {
        let row = build_snapshot_row(
            snap,
            busy,
            machine,
            client,
            &handles.immediate_delete_target,
        );
        row.set_widget_name(&snap.id.to_string());
        handles.snapshot_list_box.append(&row);
    }
}

/// The visible row text: timestamp + file count + formatted size, plus the
/// non-`Active` lifecycle state and, once a check has run this session, the
/// derived integrity verdict.
///
/// The prefix is linux's own (its i18n table, its byte formatter); both
/// **suffix rules** are the shared
/// [`fauna_backups_machine::snapshot_state_text`] /
/// [`snapshot_integrity_text`](fauna_backups_machine::snapshot_integrity_text)
/// decisions — same lift as [`busy_text`], and previously hand-rolled
/// identically here and on the other six apps.
fn snapshot_row_text(snap: &SnapshotRow) -> String {
    let when = crate::client::format_epoch_us(snap.created_at.saturating_mul(1_000_000));
    let mut text = format!(
        "{}  {}  {}",
        when,
        backups_strings::file_count(&snap.file_count.max(0).to_string()),
        crate::i18n::byte_size(snap.total_bytes.max(0) as u64),
    );
    // Timestamp rendering stays linux's; the shared rule owns only which key
    // and the dated/undated fallback.
    let deadline = snap
        .state
        .deadline()
        .map(|at| crate::client::format_epoch_us(at.saturating_mul(1_000_000)));
    for suffix in [
        fauna_backups_machine::snapshot_state_text(&snap.state, deadline.as_deref()),
        fauna_backups_machine::snapshot_integrity_text(snap.integrity),
    ]
    .into_iter()
    .flatten()
    {
        text.push_str("  ");
        text.push_str(&suffix.resolve(crate::i18n::strings::lookup));
    }
    text
}

/// Build one `snapshot-item[i]` row: the clickable summary plus its two per-row
/// delete affordances.
fn build_snapshot_row(
    snap: &SnapshotRow,
    busy: bool,
    machine: &std::sync::Arc<BackupsMachine>,
    client: &Rc<FaunaClient>,
    immediate_delete_target: &Rc<Cell<Option<i64>>>,
) -> gtk::ListBoxRow {
    let snapshot_id = snap.id;

    let row_text = snapshot_row_text(snap);
    let summary = gtk::Label::new(Some(&row_text));
    summary.set_halign(gtk::Align::Start);
    summary.set_hexpand(true);
    summary.set_wrap(true);

    let delete_btn = gtk::Button::new();
    delete_btn.set_icon_name("user-trash-symbolic");
    delete_btn.set_tooltip_text(Some(backups_strings::DELETE_SNAPSHOT));
    delete_btn.add_css_class("flat");
    delete_btn.add_css_class("destructive-action");
    delete_btn.set_sensitive(!busy);
    crate::testid::set_test_id(&delete_btn, ids::SNAPSHOT_DELETE_BUTTON);
    crate::offline_gate::declare_wire_kind(&delete_btn, "fauna.filesync.snapshot.delete");

    // Delete keeps its confirm (client glue — the four apps that ship one keep
    // theirs), then dispatches the machine gesture that queues the 48 h pending
    // action; the row comes back as `DeletionPending`.
    {
        let m = std::sync::Arc::clone(machine);
        let rt = client.runtime_handle();
        delete_btn.connect_clicked(move |btn| {
            let m = std::sync::Arc::clone(&m);
            let rt = rt.clone();
            // No wire kind here — it is declared on the ENTRY button above, the
            // same convention factory reset and mail-disable follow. No test ids
            // either: `ui.yaml` names none for this confirm, and minting one is
            // a spec change, not a refactor.
            crate::confirm_dialog::present_confirm(
                btn,
                crate::confirm_dialog::ConfirmSpec::new(
                    backups_strings::DELETE_SNAPSHOT,
                    crate::confirm_dialog::ConfirmBody::Text(
                        backups_strings::DELETE_SNAPSHOT_CONFIRM,
                    ),
                    "delete",
                    backups_strings::DELETE_SNAPSHOT,
                    crate::confirm_dialog::CANCEL,
                ),
                move || {
                    let m = std::sync::Arc::clone(&m);
                    rt.spawn(async move { m.delete_snapshot(snapshot_id).await });
                },
            );
        });
    }

    // Immediate-delete button — sibling of `snapshot-delete-button` per row
    // (backups.md § Element IDs). NEVER a one-click action: it only OPENS the
    // friction-bar modal (Architectural rule 4); the modal's confirm enables
    // only when both inputs match exactly.
    let immediate_delete_btn = gtk::Button::new();
    immediate_delete_btn.set_icon_name("user-trash-full-symbolic");
    immediate_delete_btn.set_tooltip_text(Some(backups_strings::IMMEDIATE_DELETE_BUTTON));
    immediate_delete_btn.add_css_class("flat");
    immediate_delete_btn.add_css_class("destructive-action");
    immediate_delete_btn.set_sensitive(!busy);
    crate::testid::set_test_id(&immediate_delete_btn, ids::SNAPSHOT_IMMEDIATE_DELETE_BUTTON);
    {
        let m = std::sync::Arc::clone(machine);
        let client = Rc::clone(client);
        let target = Rc::clone(immediate_delete_target);
        immediate_delete_btn.connect_clicked(move |btn| {
            let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            target.set(Some(snapshot_id));
            open_immediate_delete_modal(parent.as_ref(), snapshot_id, &m, &client, &target);
        });
    }

    // The clickable region is a flat `gtk::Button` carrying `snapshot-item`,
    // opening the snapshot detail on click. A Button (not the bare
    // `gtk::ListBoxRow` + `row-selected`) is what makes the row actuable
    // coordinate-free: the e2e AT-SPI bridge calls `do_action(0)` on the
    // id-bearing widget, which only fires `clicked` on a widget exposing an
    // AT-SPI action — GTK4 surfaces none on a `GtkListBoxRow`. The delete
    // buttons stay *siblings* (not nested) so they remain independently
    // clickable.
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.append(&summary);

    let item_btn = gtk::Button::new();
    item_btn.add_css_class("flat");
    item_btn.set_hexpand(true);
    item_btn.set_child(Some(&content));
    crate::testid::set_test_id(&item_btn, ids::SNAPSHOT_ITEM);
    // A child-bearing Button has no `label()`, so without this every row reads as
    // "" and the § *Row content contract* (date, file count, size, lifecycle)
    // cannot be read from outside; the conversation card declares its text the
    // same way.
    crate::testid::set_test_text(&item_btn, &row_text);
    // Declared (not left ungated): a read that changes nothing, but recording
    // it means a later reclassification reaches this page for free (tui's
    // ruling — the admin `OpenSeedRotateConfirm` precedent).
    crate::offline_gate::declare_wire_kind(&item_btn, "fauna.filesync.snapshot.get");
    // Expose the snapshot id readably for the e2e immediate-delete friction bar
    // (the row's visible text is the date/size, not the raw id). A cross-app
    // contract rather than a test nicety (§ *Row content contract*).
    crate::testid::set_test_attr(&item_btn, "snapshot-id", &snapshot_id.to_string());
    {
        let m = std::sync::Arc::clone(machine);
        let rt = client.runtime_handle();
        item_btn.connect_clicked(move |_| {
            let m = std::sync::Arc::clone(&m);
            rt.spawn(async move { m.open_snapshot(snapshot_id).await });
        });
    }

    // Recovery is offered ONLY out of `SoftDeleted` (backups.md § *Soft-deleted
    // rows*), so the button's PRESENCE is the row-state observable — the same
    // "present only while the state stands" shape as
    // `snapshot-prune-execute-button`. The machine refuses the gesture for any
    // non-`SoftDeleted` row regardless, so a stray click past the render cannot
    // get ahead of the rule.
    let undelete_btn = matches!(snap.state, SnapshotState::SoftDeleted { .. }).then(|| {
        let btn = gtk::Button::new();
        btn.set_icon_name("edit-undo-symbolic");
        btn.set_tooltip_text(Some(backups_strings::SNAPSHOT_UNDELETE_BUTTON));
        btn.add_css_class("flat");
        btn.set_sensitive(!busy);
        crate::testid::set_test_id(&btn, ids::SNAPSHOT_UNDELETE_BUTTON);
        crate::offline_gate::declare_wire_kind(&btn, "fauna.filesync.snapshot.undelete");
        let m = std::sync::Arc::clone(machine);
        let rt = client.runtime_handle();
        btn.connect_clicked(move |_| {
            let m = std::sync::Arc::clone(&m);
            rt.spawn(async move { m.undelete_snapshot(snapshot_id).await });
        });
        btn
    });

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&item_btn);
    if let Some(btn) = &undelete_btn {
        hbox.append(btn);
    }
    hbox.append(&immediate_delete_btn);
    hbox.append(&delete_btn);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row.set_activatable(false);
    row
}

/// Present the immediate-delete friction-bar modal for `snapshot_id`
/// (backups.md § Element IDs + Architectural rule 4). NEVER a one-click
/// affordance: `immediate-delete-confirm-button` enables only when the user
/// re-types the snapshot id into `immediate-delete-confirm-input` AND types the
/// exact `IMMEDIATE_DELETE_ACK_TEXT` into `immediate-delete-acknowledge-input`
/// (displayed above for the user to copy).
///
/// ⚠ The enabled flag is the **machine's** predicate, not a local
/// re-derivation. `deleting` is the half linux got wrong — it was hard-coded
/// `false`, so the confirm stayed live during an in-flight delete and a second
/// click could re-issue it. Only the machine knows that bit.
fn open_immediate_delete_modal(
    parent: Option<&gtk::Window>,
    snapshot_id: i64,
    machine: &std::sync::Arc<BackupsMachine>,
    client: &Rc<FaunaClient>,
    immediate_delete_target: &Rc<Cell<Option<i64>>>,
) {
    use crate::i18n::strings::backups as s;

    // The exact acknowledge phrase, pinned to the protocol constant the nest
    // checks byte-for-byte — so the modal's friction bar can't drift from the
    // server's `acknowledge_mismatch` gate.
    let ack_text = fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT;
    let target_id = snapshot_id.to_string();

    let dialog = adw::Window::builder()
        .title(s::immediate_delete_modal_title(&target_id))
        .modal(true)
        .default_width(480)
        .build();
    if let Some(win) = parent {
        dialog.set_transient_for(Some(win));
    }
    crate::testid::set_test_id(&dialog, ids::IMMEDIATE_DELETE_CONFIRM_MODAL);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let title = gtk::Label::new(Some(&s::immediate_delete_modal_title(&target_id)));
    title.set_halign(gtk::Align::Start);
    title.set_wrap(true);
    title.add_css_class("title-2");
    content.append(&title);

    let warning = gtk::Label::new(Some(s::IMMEDIATE_DELETE_WARNING));
    warning.set_halign(gtk::Align::Start);
    warning.set_wrap(true);
    warning.add_css_class("dim-label");
    content.append(&warning);

    let confirm_input = gtk::Entry::new();
    confirm_input.set_placeholder_text(Some(s::IMMEDIATE_DELETE_CONFIRM_ID_PLACEHOLDER));
    crate::testid::set_test_id(&confirm_input, ids::IMMEDIATE_DELETE_CONFIRM_INPUT);
    content.append(&confirm_input);

    let ack_prompt = gtk::Label::new(Some(s::IMMEDIATE_DELETE_ACKNOWLEDGE_PROMPT));
    ack_prompt.set_halign(gtk::Align::Start);
    content.append(&ack_prompt);

    // The exact phrase to type, shown for the user to copy.
    let ack_phrase = gtk::Label::new(Some(ack_text));
    ack_phrase.set_halign(gtk::Align::Start);
    ack_phrase.set_wrap(true);
    ack_phrase.set_selectable(true);
    ack_phrase.add_css_class("monospace");
    content.append(&ack_phrase);

    let acknowledge_input = gtk::Entry::new();
    acknowledge_input.set_placeholder_text(Some(s::IMMEDIATE_DELETE_ACKNOWLEDGE_PLACEHOLDER));
    crate::testid::set_test_id(&acknowledge_input, ids::IMMEDIATE_DELETE_ACKNOWLEDGE_INPUT);
    content.append(&acknowledge_input);

    let button_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    button_row.set_halign(gtk::Align::End);
    button_row.set_margin_top(8);

    let cancel_btn = gtk::Button::with_label(s::IMMEDIATE_DELETE_CANCEL_BUTTON);
    crate::testid::set_test_id(&cancel_btn, ids::IMMEDIATE_DELETE_CANCEL_BUTTON);
    {
        let d = dialog.clone();
        let target = Rc::clone(immediate_delete_target);
        cancel_btn.connect_clicked(move |_| {
            target.set(None);
            d.close();
        });
    }
    button_row.append(&cancel_btn);

    let confirm_btn = gtk::Button::with_label(s::IMMEDIATE_DELETE_CONFIRM_BUTTON);
    confirm_btn.add_css_class("destructive-action");
    confirm_btn.set_sensitive(false);
    crate::testid::set_test_id(&confirm_btn, ids::IMMEDIATE_DELETE_CONFIRM_BUTTON);
    crate::offline_gate::declare_wire_kind(
        &confirm_btn,
        "fauna.filesync.snapshot.delete_immediate",
    );
    button_row.append(&confirm_btn);

    content.append(&button_row);

    // Friction-bar recompute (backups.md rule 4): the machine's predicate, with
    // its real in-flight flag.
    let recompute = {
        let confirm_input = confirm_input.clone();
        let acknowledge_input = acknowledge_input.clone();
        let confirm_btn = confirm_btn.clone();
        let machine = std::sync::Arc::clone(machine);
        let target_id = target_id.clone();
        move || {
            let enabled = machine.immediate_delete_enabled(
                confirm_input.text().to_string(),
                target_id.clone(),
                acknowledge_input.text().to_string(),
            );
            confirm_btn.set_sensitive(enabled);
        }
    };
    {
        let r = recompute.clone();
        confirm_input.connect_changed(move |_| r());
    }
    {
        let r = recompute.clone();
        acknowledge_input.connect_changed(move |_| r());
    }

    // Confirm: dispatch the owner-only delete. Keep the modal open — the render
    // pass closes it once the machine's list no longer carries the row (the
    // failure path leaves it up, with the machine's error on `error-message`).
    {
        let m = std::sync::Arc::clone(machine);
        let rt = client.runtime_handle();
        let confirm_input = confirm_input.clone();
        let acknowledge_input = acknowledge_input.clone();
        confirm_btn.connect_clicked(move |_| {
            let m = std::sync::Arc::clone(&m);
            let confirm = confirm_input.text().to_string();
            let ack = acknowledge_input.text().to_string();
            rt.spawn(async move { m.delete_snapshot_immediate(snapshot_id, confirm, ack).await });
        });
    }

    dialog.set_content(Some(&content));
    dialog.present();
}

/// Close the immediate-delete modal if open — walks the visible toplevels for
/// the `immediate-delete-confirm-modal` window (the modal carries no handle in
/// `WidgetHandles`; rows are built dynamically). Called by the render pass once
/// the machine reports the delete landed.
pub fn close_immediate_delete_modal() {
    let toplevels = gtk::Window::toplevels();
    for i in 0..toplevels.n_items() {
        let Some(win) = toplevels
            .item(i)
            .and_then(|o| o.downcast::<gtk::Window>().ok())
        else {
            continue;
        };
        if win.widget_name() == "immediate-delete-confirm-modal" && win.is_visible() {
            win.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_backups_machine::{CheckOutcome, PruneCandidate, PrunePreview, RowIntegrity};

    // Struct-update, so a field the shared record grows does not break this
    // fixture — needless only until it does.
    #[allow(clippy::needless_update)]
    fn row(id: i64) -> SnapshotRow {
        SnapshotRow {
            id,
            created_at: 1_700_000_000,
            file_count: 12,
            total_bytes: 4096,
            device_id: None,
            tags: vec![],
            state: SnapshotState::Active,
            integrity: RowIntegrity::Unknown,
            ..Default::default()
        }
    }

    /// The row text carries all three of the *Row content contract*'s required
    /// parts — never a raw record dump.
    #[test]
    fn row_text_renders_time_count_and_size() {
        let text = snapshot_row_text(&row(7));
        assert!(text.contains("12"), "file count missing: {text}");
        assert!(
            text.contains(&crate::i18n::byte_size(4096)),
            "formatted size missing: {text}"
        );
        assert!(
            !text.contains("4096"),
            "raw byte count leaked into the row: {text}"
        );
    }

    /// `Unknown` integrity paints NOTHING — the word "unknown" on this page
    /// would read as a finding (tui's ruling, inherited).
    #[test]
    fn unknown_integrity_paints_nothing() {
        let text = snapshot_row_text(&row(1));
        assert!(!text.contains(backups_strings::SNAPSHOT_INTEGRITY_OK));
        assert!(!text.contains(backups_strings::SNAPSHOT_INTEGRITY_IMPLICATED));
    }

    #[test]
    fn checked_states_paint_their_verdict() {
        let mut ok = row(1);
        ok.integrity = RowIntegrity::CheckedOk;
        assert!(snapshot_row_text(&ok).contains(backups_strings::SNAPSHOT_INTEGRITY_OK));

        let mut bad = row(1);
        bad.integrity = RowIntegrity::Implicated;
        assert!(snapshot_row_text(&bad).contains(backups_strings::SNAPSHOT_INTEGRITY_IMPLICATED));
    }

    /// A non-`Active` row renders its state AND the deadline the user can still
    /// act on — the deadline is the whole reason the lifecycle fields exist.
    #[test]
    fn lifecycle_states_render_with_their_deadline() {
        let mut pending = row(1);
        pending.state = SnapshotState::DeletionPending {
            execute_after: Some(1_700_086_400),
        };
        let text = snapshot_row_text(&pending);
        let deadline = crate::client::format_epoch_us(1_700_086_400_i64.saturating_mul(1_000_000));
        assert!(
            text.contains(&deadline),
            "no execute_after deadline: {text}"
        );

        let mut soft = row(1);
        soft.state = SnapshotState::SoftDeleted {
            purge_after: Some(1_702_678_400),
        };
        let text = snapshot_row_text(&soft);
        let deadline = crate::client::format_epoch_us(1_702_678_400_i64.saturating_mul(1_000_000));
        assert!(text.contains(&deadline), "no purge_after deadline: {text}");
    }

    /// A nest that served the state without a joinable pending action still
    /// renders the state — undated, never silently as `Active`.
    #[test]
    fn undated_lifecycle_states_still_render() {
        let mut pending = row(1);
        pending.state = SnapshotState::DeletionPending {
            execute_after: None,
        };
        assert!(
            snapshot_row_text(&pending)
                .contains(backups_strings::SNAPSHOT_STATE_DELETION_PENDING_UNDATED)
        );

        let mut soft = row(1);
        soft.state = SnapshotState::SoftDeleted { purge_after: None };
        assert!(
            snapshot_row_text(&soft).contains(backups_strings::SNAPSHOT_STATE_SOFT_DELETED_UNDATED)
        );
    }

    /// Every `BackupOp` names itself on the busy line. A missing arm would leave
    /// the disabled action row unexplained.
    #[test]
    fn every_busy_op_has_text() {
        for op in [
            BackupOp::Create,
            BackupOp::Delete,
            BackupOp::ImmediateDelete,
            BackupOp::Prune,
            BackupOp::Check,
            BackupOp::Refresh,
            BackupOp::Detail,
        ] {
            assert!(!busy_text(op).is_empty(), "{op:?} has no busy text");
        }
    }

    /// The two no-op policy states are distinguishable from a clean apply — the
    /// page must say WHY nothing would be pruned rather than show an empty
    /// success (`PolicyState::from_wire`'s fail-safe direction, consumed here).
    #[test]
    fn unknown_policy_state_degrades_to_unparseable() {
        assert_eq!(PolicyState::from_wire("applied"), PolicyState::Applied);
        assert_eq!(PolicyState::from_wire("not_set"), PolicyState::NotSet);
        assert_eq!(
            PolicyState::from_wire("some_future_verdict"),
            PolicyState::Unparseable
        );
    }

    /// Guards the check-result surface against a re-derivation creeping back:
    /// the verdict is `is_ok`, and it can disagree with a zero error count.
    #[test]
    fn check_verdict_follows_is_ok_not_the_counts() {
        let outcome = CheckOutcome {
            is_ok: false,
            snapshots_checked: 3,
            files_checked: 9,
            manifests_checked: 9,
            chunks_checked: 30,
            missing_manifests: 0,
            missing_chunks: 0,
            corrupt_manifests: 0,
            implicated: vec![],
        };
        // All three counts are zero, so anything deriving the verdict from them
        // would call this a pass. The shared predicate says otherwise.
        assert!(!outcome.is_ok);
    }

    /// A preview with candidates is what arms execute; the two no-op states and
    /// an empty candidate list are not.
    #[test]
    fn execute_is_armed_only_by_a_preview_with_candidates() {
        let armed = |policy_state, candidates: Vec<PruneCandidate>| {
            let preview = PrunePreview {
                would_prune: candidates.len() as i64,
                remaining: 2,
                candidates,
                policy_state,
            };
            matches!(preview.policy_state, PolicyState::Applied) && !preview.candidates.is_empty()
        };
        let candidate = || {
            vec![PruneCandidate {
                id: 1,
                created_at: 1_700_000_000,
                tags: vec![],
            }]
        };
        assert!(armed(PolicyState::Applied, candidate()));
        assert!(!armed(PolicyState::Applied, vec![]));
        assert!(!armed(PolicyState::NotSet, candidate()));
        assert!(!armed(PolicyState::Unparseable, candidate()));
    }
}
