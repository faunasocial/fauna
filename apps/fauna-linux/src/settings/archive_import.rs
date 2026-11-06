//! The Settings → Import from other services wizard (linux — the first
//! trickle-down leg behind tui, the lead app).
//!
//! Where a person brings a Facebook or Instagram export archive into Fauna — a
//! six-screen wizard (Source → Archive → Scope → Confirm → Progress → Done).
//! Target behavior: `docs/goal/behavior/archive-import.md` § The wizard and its
//! machine. UX/IDs: `tests/e2e-unified/ui.yaml` `archive-import` page + its
//! `archive-import-category-progress-list` component.
//!
//! This layer holds **no** business logic: it is a dumb renderer of the shared
//! [`ArchiveImportSnapshot`] and a dispatcher of [`ArchiveImportAction`]; the
//! FSM, the parser, the folder writes, the authoring and the page's token words
//! (`archive_import_category_label` and its siblings) and every line it paints
//! (`fauna_archive_import_machine::text`) are shared Rust
//! (priority #2). Direct siblings: `settings/mail_export.rs` and
//! `settings/mail_import.rs` (this page's GTK shape) and
//! `apps/fauna-tui/src/settings/archive_import.rs` (this page's behavior — the
//! text each element paints is that module's, decision for decision).
//!
//! # One element set, shown conditionally by step
//!
//! Every step's widgets are built once into the tree and `render()` shows only
//! the active step's group, the `mail_export.rs` pattern — including its
//! reason for building every non-Source group `visible(false)` at
//! construction: the `wizard-back-button` / `wizard-next-button` ids repeat
//! across steps and are disambiguated purely by step gating
//! (`automation::find` prunes non-showing subtrees).
//!
//! # The archive is a PATH here too
//!
//! The Archive step is the path field plus `archive-import-archive-open-button`
//! (tui's shape), which commits the path draft and dispatches `OpenArchive` as
//! one ordered dispatch. The text fields are drafts committed at the transition
//! (Open archive; Next off Scope), the `mail_import.rs` reasoning: a
//! per-keystroke dispatch could reorder behind the transition's own.
//!
//! # Two reads the driver makes with NO wait
//!
//! The agent acks a gesture when the GTK handler returns, before the dispatch's
//! render comes back, so two reads are answered from local, synchronous truth
//! in the handler itself (the `mail_import.rs` reasoning):
//!
//! - **Pick a platform, then read its help.** The help and the export link are
//!   pure functions of the pick ([`paint_source`]).
//! - **Toggle a category, then read its `state`.** The marker class is moved
//!   inside `connect_toggled` ([`mark_row_state`]).
//!
//! # Three buttons and what they honestly do
//!
//! View imported leaves for the feed (it carries no per-source filter yet);
//! Review skipped is a page-local toggle revealing the skip log on the Done
//! screen; Use profile details is offered, never applied silently — enabled
//! while the run's end found a profile in the archive and the tap has not
//! applied it yet, and the tap dispatches `ApplyArchiveProfile` (tui's shape).

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_archive_import_machine::text as compose;
use fauna_archive_import_machine::{
    AUDIENCE_MODES, ArchiveImportAction, ArchiveImportMachine, ArchiveImportSnapshot,
    ArchiveImportStep, ArchiveSourceKind, AudienceMode, CategoryRow, RunState, SOURCE_KINDS,
};
use fauna_core::localized::ComposedText;

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::archive_import as S;
use crate::testid::set_test_id;

/// One Scope category row's painted fields: token, count, importable, selected.
type CategoryKeyRow = (String, u64, bool, bool);

thread_local! {
    /// The live page's machine — read by the `archive_import_{pause,hold}_after`
    /// agent commands (`lib.rs`), the one cross-module reach into this page,
    /// tui's `settings::archive_import_machine` shape. Replaced whenever the
    /// settings shell is rebuilt for a new login.
    static MACHINE: RefCell<Option<Arc<ArchiveImportMachine>>> = const { RefCell::new(None) };
}

/// The live page's machine, if the page built one (signed in, and the glue did
/// not refuse).
pub fn current_machine() -> Option<Arc<ArchiveImportMachine>> {
    MACHINE.with(|cell| cell.borrow().clone())
}

/// Handles to the widgets the snapshot renders into.
#[derive(Clone)]
struct ArchiveWidgets {
    error_label: gtk::Label,
    unavailable_label: gtk::Label,

    // Step 1 — Source.
    source_group: adw::PreferencesGroup,
    source_picker: gtk::DropDown,
    source_help: gtk::Label,
    source_export_link: gtk::Button,

    // Step 2 — Archive.
    archive_group: adw::PreferencesGroup,
    path_input: gtk::Entry,
    archive_summary: gtk::Label,
    archive_summary_row: adw::ActionRow,

    // Step 3 — Scope.
    scope_group: adw::PreferencesGroup,
    category_box: gtk::Box,
    category_key: Rc<RefCell<Vec<CategoryKeyRow>>>,
    audience_picker: gtk::DropDown,
    audience_summary: gtk::Label,
    hidden_tiers_note: gtk::Label,
    date_from_input: gtk::Entry,
    date_to_input: gtk::Entry,

    // Step 4 — Confirm.
    confirm_group: adw::PreferencesGroup,
    confirm_summary: gtk::Label,

    // Step 5 — Progress.
    progress_group: adw::PreferencesGroup,
    progress_summary: gtk::Label,
    progress_bar: gtk::ProgressBar,
    error_log: gtk::Label,
    progress_list_group: adw::PreferencesGroup,
    progress_rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    progress_back_row: adw::ActionRow,

    // Step 6 — Done.
    done_group: adw::PreferencesGroup,
    done_summary: gtk::Label,
    done_error_log: gtk::Label,
    done_error_log_row: adw::ActionRow,
    folder_link: gtk::Button,
    profile_prefill_button: gtk::Button,
}

/// Everything the page's handlers + render need. `Rc`-shared into every closure.
struct ArchiveCtx {
    machine: Arc<ArchiveImportMachine>,
    rt: tokio::runtime::Handle,
    /// Set while `render()` programmatically updates a picker, so its
    /// `selected_notify` handler doesn't echo the change back as a dispatch.
    syncing: Cell<bool>,
    /// Whether the Progress repaint tick is already running — one at a time.
    ticking: Cell<bool>,
    /// Whether Review skipped has revealed the skip log on the Done screen —
    /// page-local, tui's `ArchiveImportState::show_skipped`; cleared whenever
    /// the wizard is off the Done step.
    show_skipped: Cell<bool>,
    /// The last snapshot rendered — what a picker handler paints its
    /// synchronous answer from.
    last: RefCell<Option<ArchiveImportSnapshot>>,
    w: ArchiveWidgets,
}

/// The interaction buttons threaded into [`wire_machine`].
struct WizardButtons {
    source_next: gtk::Button,
    archive_open: gtk::Button,
    archive_back: gtk::Button,
    archive_next: gtk::Button,
    scope_back: gtk::Button,
    scope_next: gtk::Button,
    confirm_back: gtk::Button,
    start_button: gtk::Button,
    pause_button: gtk::Button,
    resume_button: gtk::Button,
    cancel_button: gtk::Button,
    progress_back: gtk::Button,
    view_imported_button: gtk::Button,
    review_skipped_button: gtk::Button,
    done_back: gtk::Button,
}

/// A tagged button.
fn button(id: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&button, id);
    button
}

/// A Back + Next row, the wizard's own nav pair on one step.
fn nav_row(
    group: &adw::PreferencesGroup,
    next_label: &str,
    next_id: &str,
) -> (gtk::Button, gtk::Button) {
    let back = button(ids::WIZARD_BACK_BUTTON, S::BACK);
    let next = button(next_id, next_label);
    let row = adw::ActionRow::builder().activatable(false).build();
    row.add_suffix(&back);
    row.add_suffix(&next);
    group.add(&row);
    (back, next)
}

/// A wrapping, left-aligned label carrying `id`.
fn text_label(id: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .use_markup(false)
        .build();
    set_test_id(&label, id);
    label
}

/// An `adw::ActionRow` holding one tagged text `gtk::Entry`.
fn entry_row(group: &adw::PreferencesGroup, id: &str, title: &str) -> gtk::Entry {
    let input = gtk::Entry::builder()
        .placeholder_text(title)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    set_test_id(&input, id);
    let row = adw::ActionRow::builder()
        .title(title)
        .activatable(false)
        .build();
    row.add_suffix(&input);
    group.add(&row);
    input
}

/// A one-label row (its child carries the id), shown by default.
fn label_row(group: &adw::PreferencesGroup, label: &gtk::Label) -> adw::ActionRow {
    let row = adw::ActionRow::builder().activatable(false).build();
    row.set_child(Some(label));
    group.add(&row);
    row
}

fn source_kind_label(kind: ArchiveSourceKind) -> String {
    fauna_archive_import_machine::archive_import_source_kind_label(kind)
}

fn audience_mode_label(mode: AudienceMode) -> String {
    fauna_archive_import_machine::archive_import_audience_mode_label(mode)
        .resolve(crate::i18n::strings::lookup)
}

/// A line the shared machine crate composed (`fauna_archive_import_machine::text`).
fn composed(text: &ComposedText) -> String {
    text.resolve(crate::i18n::strings::lookup)
}

fn source_kind_at(index: u32) -> ArchiveSourceKind {
    SOURCE_KINDS
        .get(index as usize)
        .copied()
        .unwrap_or(ArchiveSourceKind::Facebook)
}

fn source_kind_index(kind: ArchiveSourceKind) -> u32 {
    SOURCE_KINDS.iter().position(|k| *k == kind).unwrap_or(0) as u32
}

fn audience_mode_at(index: u32) -> AudienceMode {
    AUDIENCE_MODES
        .get(index as usize)
        .copied()
        .unwrap_or(AudienceMode::Original)
}

fn audience_mode_index(mode: AudienceMode) -> u32 {
    AUDIENCE_MODES.iter().position(|m| *m == mode).unwrap_or(0) as u32
}

/// Step 1's deep link: the snapshot's derived `source_export_url`, or the
/// shared constant for `kind` (before the first hydrate, or in the picker's
/// handler before the dispatch lands). Never typed by anyone, so it is safe to
/// hand to the OS opener.
fn export_url(snap: Option<&ArchiveImportSnapshot>, kind: ArchiveSourceKind) -> String {
    snap.filter(|s| s.source_kind == kind)
        .map(compose::source_export_url)
        .unwrap_or_else(|| kind.export_request_url().to_string())
}

/// Paint step 1's help and link for `kind` — a pure function of the pick, so
/// the picker's own handler answers before its dispatch returns.
fn paint_source(w: &ArchiveWidgets, snap: Option<&ArchiveImportSnapshot>, kind: ArchiveSourceKind) {
    w.source_help.set_text(
        &fauna_archive_import_machine::archive_import_source_help(kind)
            .resolve(crate::i18n::strings::lookup),
    );
    w.source_export_link
        .set_label(&composed(&compose::archive_import_source_link(
            &export_url(snap, kind),
        )));
}

/// The Scope step's audience sentence for `mode` — what known vs. unknown
/// audiences become, or the Only-me override.
fn audience_text(snap: Option<&ArchiveImportSnapshot>, mode: AudienceMode) -> String {
    composed(&compose::audience_summary(
        snap.and_then(|s| s.summary.as_ref()),
        mode,
    ))
}

/// Step 2's summary line, or `None` before an archive is indexed.
fn archive_summary_text(snap: &ArchiveImportSnapshot) -> Option<String> {
    snap.summary
        .as_ref()
        .map(|summary| composed(&compose::archive_summary(summary)))
}

/// Step 5's summary: the run's state and counters, then elapsed + ETA.
fn progress_text(snap: &ArchiveImportSnapshot) -> String {
    composed(&compose::progress_summary(snap))
}

/// Move a category row's `state` marker class — run inside `connect_toggled`
/// so a read straight after the click sees the new state (module docs).
fn mark_row_state(widget: &impl IsA<gtk::Widget>, state: &str) {
    for old in ["on", "off", "kept"] {
        widget
            .as_ref()
            .remove_css_class(&format!("test-attr-state-{old}"));
    }
    widget
        .as_ref()
        .add_css_class(&format!("test-attr-state-{state}"));
}

/// The fields a Scope category row paints from — the rebuild key.
fn category_key(rows: &[CategoryRow]) -> Vec<CategoryKeyRow> {
    rows.iter()
        .map(|r| (r.token.clone(), r.count, r.importable, r.selected))
        .collect()
}

/// Build the "Import from other services" wizard page.
pub fn build_archive_import_page() -> gtk::Box {
    let (page, widgets, buttons) = build_page_parts();
    if let Some(ctx) = wire_machine(widgets.clone(), buttons) {
        // Every visit re-reads the archive folder (tui's per-visit
        // `HydrateArchiveImport`): an import started on another device, or
        // before a restart, is found and offered for resume. A fresh visit
        // also drops the page-local drafts (tui's `reset_form`).
        page.connect_map(move |_| {
            ctx.w.path_input.set_text("");
            ctx.w.date_from_input.set_text("");
            ctx.w.date_to_input.set_text("");
            ctx.show_skipped.set(false);
            hydrate_and_render(&ctx);
        });
    } else if crate::settings::get_client().is_some() {
        // Signed in, but the glue refused (no identity key, no device id): the
        // page states the fact and offers nothing it could not commit.
        show_unavailable(&widgets);
    }
    crate::testid::wrap_page_with_heading(S::TITLE, ids::PAGE_HEADING, &page)
}

/// Hide every step and paint the unavailable sentence.
fn show_unavailable(w: &ArchiveWidgets) {
    for group in [
        &w.source_group,
        &w.archive_group,
        &w.scope_group,
        &w.confirm_group,
        &w.progress_group,
        &w.progress_list_group,
        &w.done_group,
    ] {
        group.set_visible(false);
    }
    w.unavailable_label.set_visible(true);
}

/// The widget tree, before it is wired to a machine — split out so a unit test
/// can build it without a live client.
fn build_page_parts() -> (adw::PreferencesPage, ArchiveWidgets, WizardButtons) {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("document-open-symbolic")
        .build();

    // --- Page-level error + the unavailable sentence (heading painted by
    // --- `wrap_page_with_heading`) ---
    let top_group = adw::PreferencesGroup::builder()
        .description(S::DESCRIPTION)
        .build();
    let error_label = gtk::Label::builder().visible(false).wrap(true).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);
    let unavailable_label = gtk::Label::builder()
        .label(S::UNAVAILABLE)
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    top_group.add(&unavailable_label);
    page.add(&top_group);

    // === Step 1 — Source =====================================================
    let source_group = adw::PreferencesGroup::builder()
        .title(S::SOURCE_TITLE)
        .build();
    let source_options: Vec<String> = SOURCE_KINDS.into_iter().map(source_kind_label).collect();
    let source_refs: Vec<&str> = source_options.iter().map(String::as_str).collect();
    let source_picker = gtk::DropDown::from_strings(&source_refs);
    source_picker.set_valign(gtk::Align::Center);
    set_test_id(&source_picker, ids::ARCHIVE_IMPORT_SOURCE_PICKER);
    let source_picker_row = adw::ActionRow::builder().title(S::SOURCE_TITLE).build();
    source_picker_row.add_suffix(&source_picker);
    source_group.add(&source_picker_row);
    let source_help = text_label(ids::ARCHIVE_IMPORT_SOURCE_HELP);
    label_row(&source_group, &source_help);
    let source_export_link = button(ids::ARCHIVE_IMPORT_SOURCE_EXPORT_LINK, "");
    source_export_link.set_halign(gtk::Align::Start);
    source_export_link.add_css_class("flat");
    let link_row = adw::ActionRow::builder().activatable(false).build();
    link_row.set_child(Some(&source_export_link));
    source_group.add(&link_row);
    let source_next = button(ids::WIZARD_NEXT_BUTTON, S::NEXT);
    let source_nav = adw::ActionRow::builder().activatable(false).build();
    source_nav.add_suffix(&source_next);
    source_group.add(&source_nav);
    page.add(&source_group);

    // === Step 2 — Archive ====================================================
    let archive_group = adw::PreferencesGroup::builder()
        .title(S::ARCHIVE_TITLE)
        .visible(false)
        .build();
    let path_input = entry_row(
        &archive_group,
        ids::ARCHIVE_IMPORT_ARCHIVE_PATH,
        S::ARCHIVE_PATH_PLACEHOLDER,
    );
    let archive_open = button(
        ids::ARCHIVE_IMPORT_ARCHIVE_OPEN_BUTTON,
        S::ARCHIVE_OPEN_BUTTON,
    );
    let open_row = adw::ActionRow::builder().activatable(false).build();
    open_row.add_suffix(&archive_open);
    archive_group.add(&open_row);
    let archive_summary = text_label(ids::ARCHIVE_IMPORT_ARCHIVE_SUMMARY);
    let archive_summary_row = label_row(&archive_group, &archive_summary);
    archive_summary_row.set_visible(false);
    let (archive_back, archive_next) = nav_row(&archive_group, S::NEXT, ids::WIZARD_NEXT_BUTTON);
    page.add(&archive_group);

    // === Step 3 — Scope ======================================================
    let scope_group = adw::PreferencesGroup::builder()
        .title(S::SCOPE_TITLE)
        .visible(false)
        .build();
    let categories_label = text_label(ids::ARCHIVE_IMPORT_SCOPE_CATEGORIES);
    categories_label.set_text(S::SCOPE_CATEGORIES_LABEL);
    label_row(&scope_group, &categories_label);
    let category_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_start(12)
        .margin_end(12)
        .build();
    let category_row = adw::ActionRow::builder().activatable(false).build();
    category_row.set_child(Some(&category_box));
    scope_group.add(&category_row);
    let audience_options: Vec<String> = AUDIENCE_MODES
        .into_iter()
        .map(audience_mode_label)
        .collect();
    let audience_refs: Vec<&str> = audience_options.iter().map(String::as_str).collect();
    let audience_picker = gtk::DropDown::from_strings(&audience_refs);
    audience_picker.set_valign(gtk::Align::Center);
    set_test_id(&audience_picker, ids::ARCHIVE_IMPORT_SCOPE_AUDIENCE_MODE);
    let audience_row = adw::ActionRow::builder()
        .title(S::SCOPE_AUDIENCE_MODE_LABEL)
        .build();
    audience_row.add_suffix(&audience_picker);
    scope_group.add(&audience_row);
    let audience_summary = text_label(ids::ARCHIVE_IMPORT_SCOPE_AUDIENCE_SUMMARY);
    label_row(&scope_group, &audience_summary);
    let hidden_tiers_note = gtk::Label::builder()
        .label(S::SCOPE_HIDDEN_TIERS_UNAVAILABLE)
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    hidden_tiers_note.add_css_class("dim-label");
    scope_group.add(&hidden_tiers_note);
    let date_from_input = entry_row(
        &scope_group,
        ids::ARCHIVE_IMPORT_SCOPE_DATE_FROM,
        S::SCOPE_DATE_FROM_PLACEHOLDER,
    );
    let date_to_input = entry_row(
        &scope_group,
        ids::ARCHIVE_IMPORT_SCOPE_DATE_TO,
        S::SCOPE_DATE_TO_PLACEHOLDER,
    );
    let (scope_back, scope_next) = nav_row(&scope_group, S::NEXT, ids::WIZARD_NEXT_BUTTON);
    page.add(&scope_group);

    // === Step 4 — Confirm ====================================================
    let confirm_group = adw::PreferencesGroup::builder()
        .title(S::CONFIRM_TITLE)
        .visible(false)
        .build();
    let confirm_summary = text_label(ids::ARCHIVE_IMPORT_CONFIRM_SUMMARY);
    label_row(&confirm_group, &confirm_summary);
    let (confirm_back, start_button) = nav_row(
        &confirm_group,
        S::START_BUTTON,
        ids::ARCHIVE_IMPORT_START_BUTTON,
    );
    page.add(&confirm_group);

    // === Step 5 — Progress ===================================================
    let progress_group = adw::PreferencesGroup::builder()
        .title(S::PROGRESS_TITLE)
        .visible(false)
        .build();
    let progress_summary = text_label(ids::ARCHIVE_IMPORT_PROGRESS_SUMMARY);
    label_row(&progress_group, &progress_summary);
    let progress_bar = gtk::ProgressBar::builder()
        .show_text(true)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    set_test_id(&progress_bar, ids::ARCHIVE_IMPORT_PROGRESS_BAR);
    let bar_row = adw::ActionRow::builder().activatable(false).build();
    bar_row.set_child(Some(&progress_bar));
    progress_group.add(&bar_row);
    let pause_button = button(ids::ARCHIVE_IMPORT_PAUSE_BUTTON, S::PAUSE_BUTTON);
    let resume_button = button(ids::ARCHIVE_IMPORT_RESUME_BUTTON, S::RESUME_BUTTON);
    let cancel_button = button(ids::ARCHIVE_IMPORT_CANCEL_BUTTON, S::CANCEL_BUTTON);
    let controls_row = adw::ActionRow::builder().activatable(false).build();
    controls_row.add_suffix(&pause_button);
    controls_row.add_suffix(&resume_button);
    controls_row.add_suffix(&cancel_button);
    progress_group.add(&controls_row);
    let error_log = super::blank_value_marker(ids::ARCHIVE_IMPORT_ERROR_LOG);
    let error_log_row = adw::ActionRow::builder()
        .title(S::ERROR_LOG_TITLE)
        .activatable(false)
        .build();
    error_log_row.add_suffix(&error_log);
    progress_group.add(&error_log_row);
    let progress_back = button(ids::WIZARD_BACK_BUTTON, S::BACK);
    let progress_back_row = adw::ActionRow::builder()
        .activatable(false)
        .visible(false)
        .build();
    progress_back_row.add_suffix(&progress_back);
    progress_group.add(&progress_back_row);
    page.add(&progress_group);

    // The per-category rows — the `archive-import-category-progress-list`
    // component, flat like tui's and the mail wizards' progress rows.
    let progress_list_group = adw::PreferencesGroup::builder()
        .title(S::PROGRESS_TITLE)
        .visible(false)
        .build();
    progress_list_group.set_header_suffix(Some(&super::marker(
        ids::ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST,
    )));
    page.add(&progress_list_group);

    // === Step 6 — Done =======================================================
    let done_group = adw::PreferencesGroup::builder()
        .title(S::DONE_TITLE)
        .visible(false)
        .build();
    let done_summary = text_label(ids::ARCHIVE_IMPORT_DONE_SUMMARY);
    label_row(&done_group, &done_summary);
    let view_imported_button = button(
        ids::ARCHIVE_IMPORT_VIEW_IMPORTED_BUTTON,
        S::VIEW_IMPORTED_BUTTON,
    );
    let review_skipped_button = button(
        ids::ARCHIVE_IMPORT_REVIEW_SKIPPED_BUTTON,
        S::REVIEW_SKIPPED_BUTTON,
    );
    // Offered, never applied silently (module docs) — `render` enables it.
    let profile_prefill_button = button(
        ids::ARCHIVE_IMPORT_PROFILE_PREFILL_BUTTON,
        S::PROFILE_PREFILL_BUTTON,
    );
    profile_prefill_button.set_sensitive(false);
    let done_nav_row = adw::ActionRow::builder().activatable(false).build();
    done_nav_row.add_suffix(&view_imported_button);
    done_nav_row.add_suffix(&review_skipped_button);
    done_nav_row.add_suffix(&profile_prefill_button);
    done_group.add(&done_nav_row);
    // A second `archive-import-error-log`, the Done screen's (the Progress
    // step's own is hidden by then and pruned by `automation::find`).
    let done_error_log = super::blank_value_marker(ids::ARCHIVE_IMPORT_ERROR_LOG);
    let done_error_log_row = adw::ActionRow::builder()
        .title(S::ERROR_LOG_TITLE)
        .activatable(false)
        .visible(false)
        .build();
    done_error_log_row.add_suffix(&done_error_log);
    done_group.add(&done_error_log_row);
    let folder_link = button(ids::ARCHIVE_IMPORT_FOLDER_LINK, "");
    folder_link.set_halign(gtk::Align::Start);
    folder_link.add_css_class("flat");
    let folder_row = adw::ActionRow::builder().activatable(false).build();
    folder_row.set_child(Some(&folder_link));
    done_group.add(&folder_row);
    let done_back = button(ids::WIZARD_BACK_BUTTON, S::BACK);
    let done_back_row = adw::ActionRow::builder().activatable(false).build();
    done_back_row.add_suffix(&done_back);
    done_group.add(&done_back_row);
    page.add(&done_group);

    let widgets = ArchiveWidgets {
        error_label,
        unavailable_label,
        source_group,
        source_picker,
        source_help,
        source_export_link,
        archive_group,
        path_input,
        archive_summary,
        archive_summary_row,
        scope_group,
        category_box,
        category_key: Rc::new(RefCell::new(Vec::new())),
        audience_picker,
        audience_summary,
        hidden_tiers_note,
        date_from_input,
        date_to_input,
        confirm_group,
        confirm_summary,
        progress_group,
        progress_summary,
        progress_bar,
        error_log,
        progress_list_group,
        progress_rows: Rc::new(RefCell::new(Vec::new())),
        progress_back_row,
        done_group,
        done_summary,
        done_error_log,
        done_error_log_row,
        folder_link,
        profile_prefill_button,
    };
    // Pre-hydrate the wizard shows step 1 for the machine's default platform:
    // the client-side steps do not depend on the nest.
    paint_source(&widgets, None, ArchiveSourceKind::Facebook);
    widgets
        .audience_summary
        .set_text(&audience_text(None, AudienceMode::Original));
    let buttons = WizardButtons {
        source_next,
        archive_open,
        archive_back,
        archive_next,
        scope_back,
        scope_next,
        confirm_back,
        start_button,
        pause_button,
        resume_button,
        cancel_button,
        progress_back,
        view_imported_button,
        review_skipped_button,
        done_back,
    };
    (page, widgets, buttons)
}

/// Connect the page to the shared `ArchiveImportMachine` and wire every
/// interaction. `None` when there is no signed-in client (the unit test), or
/// when the glue refused to build a machine.
fn wire_machine(widgets: ArchiveWidgets, b: WizardButtons) -> Option<Rc<ArchiveCtx>> {
    let client = crate::settings::get_client()?;
    let actor_id = client.actor_id()?;
    let machine = match crate::archive_glue::build_archive_import_machine(
        client.nest_rpc().clone(),
        &actor_id,
    ) {
        Ok(machine) => Arc::new(machine),
        Err(reason) => {
            tracing::warn!("[settings/archive_import] build machine: {reason}");
            return None;
        }
    };
    MACHINE.with(|cell| *cell.borrow_mut() = Some(Arc::clone(&machine)));

    let ctx = Rc::new(ArchiveCtx {
        machine,
        rt: client.runtime_handle(),
        syncing: Cell::new(false),
        ticking: Cell::new(false),
        show_skipped: Cell::new(false),
        last: RefCell::new(None),
        w: widgets,
    });

    // Platform picker → SelectSource; help + link painted synchronously.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .source_picker
            .clone()
            .connect_selected_notify(move |dd| {
                if ctx.syncing.get() {
                    return;
                }
                let kind = source_kind_at(dd.selected());
                paint_source(&ctx.w, ctx.last.borrow().as_ref(), kind);
                dispatch_actions(&ctx, vec![ArchiveImportAction::SelectSource { kind }]);
            });
    }
    // Audience mode → SetAudienceMode; the sentence painted synchronously.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .audience_picker
            .clone()
            .connect_selected_notify(move |dd| {
                if ctx.syncing.get() {
                    return;
                }
                let mode = audience_mode_at(dd.selected());
                ctx.w
                    .audience_summary
                    .set_text(&audience_text(ctx.last.borrow().as_ref(), mode));
                dispatch_actions(&ctx, vec![ArchiveImportAction::SetAudienceMode { mode }]);
            });
    }
    // Step 1's link: purely local — the address is the shared constant the
    // step paints, handed to the app's one OS opener. Resolved from the
    // PICKER, exactly as `paint_source` resolved the label: a click right after
    // a pick lands before that pick's dispatch returns, and the snapshot then
    // still names the previous platform — reading it opened the wrong page
    // under a label naming the right one.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.source_export_link.clone().connect_clicked(move |_| {
            let kind = source_kind_at(ctx.w.source_picker.selected());
            crate::url_opener::open(&export_url(ctx.last.borrow().as_ref(), kind));
        });
    }
    // Open archive: commit the path draft, then open — one ordered dispatch.
    {
        let ctx = Rc::clone(&ctx);
        b.archive_open.connect_clicked(move |_| {
            let value = ctx.w.path_input.text().trim().to_string();
            dispatch_actions(
                &ctx,
                vec![
                    ArchiveImportAction::SetArchivePath { value },
                    ArchiveImportAction::OpenArchive,
                ],
            );
        });
    }
    // Next off Scope commits both date drafts first, so the Confirm summary can
    // never describe a scope the machine has not got.
    {
        let ctx = Rc::clone(&ctx);
        b.scope_next.connect_clicked(move |_| {
            let from = ctx.w.date_from_input.text().trim().to_string();
            let to = ctx.w.date_to_input.text().trim().to_string();
            dispatch_actions(
                &ctx,
                vec![
                    ArchiveImportAction::SetDateFrom { value: from },
                    ArchiveImportAction::SetDateTo { value: to },
                    ArchiveImportAction::Next,
                ],
            );
        });
    }
    wire_dispatch(&ctx, &b.source_next, ArchiveImportAction::Next);
    wire_dispatch(&ctx, &b.archive_next, ArchiveImportAction::Next);
    for back in [
        &b.archive_back,
        &b.scope_back,
        &b.confirm_back,
        &b.progress_back,
        &b.done_back,
    ] {
        wire_dispatch(&ctx, back, ArchiveImportAction::Back);
    }
    wire_dispatch(&ctx, &b.start_button, ArchiveImportAction::Start);
    wire_dispatch(&ctx, &b.pause_button, ArchiveImportAction::Pause);
    wire_dispatch(&ctx, &b.resume_button, ArchiveImportAction::Resume);
    // Cancel is destructive — two-click inline confirm. Already-imported
    // records are kept.
    {
        let ctx = Rc::clone(&ctx);
        super::wire_two_click(
            &b.cancel_button,
            S::CANCEL_BUTTON,
            crate::i18n::strings::common::CONFIRM_Q,
            true,
            |_| {},
            |_| {},
            move || dispatch_actions(&ctx, vec![ArchiveImportAction::Cancel]),
        );
    }
    // Done: the feed, the skip log, the folder.
    // View imported — the Feed on the imported-posts selection for the
    // archive's platform (`archive-import.md` step 6; `feed.md` § State & data
    // shape → *The imported-posts selection*): the shared
    // `FeedManager::select_imported`, then the feed page.
    {
        let ctx = Rc::clone(&ctx);
        b.view_imported_button.connect_clicked(move |btn| {
            let token = ctx
                .last
                .borrow()
                .as_ref()
                .map(|s| s.source_kind.platform().token().to_string());
            if let (Some(token), Some(manager)) = (token, crate::feed::host::manager()) {
                ctx.rt
                    .spawn(async move { manager.select_imported(token).await });
            }
            crate::app::navigate_to_section(btn, "feed");
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        b.review_skipped_button.connect_clicked(move |_| {
            ctx.show_skipped.set(!ctx.show_skipped.get());
            ctx.w.done_error_log_row.set_visible(ctx.show_skipped.get());
        });
    }
    wire_dispatch(
        &ctx,
        &ctx.w.profile_prefill_button,
        ArchiveImportAction::ApplyArchiveProfile,
    );
    ctx.w.folder_link.clone().connect_clicked(|btn| {
        crate::app::navigate_to_settings_subpage(btn, "folders");
    });

    hydrate_and_render(&ctx);
    Some(ctx)
}

/// Wire a button to a single fire-and-render action.
fn wire_dispatch(ctx: &Rc<ArchiveCtx>, button: &gtk::Button, action: ArchiveImportAction) {
    let ctx = Rc::clone(ctx);
    button.connect_clicked(move |_| dispatch_actions(&ctx, vec![action.clone()]));
}

fn hydrate_and_render(ctx: &Rc<ArchiveCtx>) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            // A hydrate failure is not fatal: the machine stamps its own
            // snapshot's `error`, and the wizard still opens on Source.
            if let Err(e) = machine.hydrate().await {
                tracing::warn!("[settings/archive_import] hydrate: {e:?}");
            }
            machine.snapshot()
        },
        move |snap| after_snapshot(&ctx_render, &snap, false),
    );
}

/// Dispatch `actions` in order, all awaited before the one re-read — so a
/// Scope→Confirm advance never observes a half-committed scope. Each `Result`
/// is dropped deliberately: a refusal arrives on the snapshot's `error`.
fn dispatch_actions(ctx: &Rc<ArchiveCtx>, actions: Vec<ArchiveImportAction>) {
    let should_run = actions
        .iter()
        .any(|a| matches!(a, ArchiveImportAction::Start | ArchiveImportAction::Resume));
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            for action in actions {
                let _ = machine.dispatch(action).await;
            }
            machine.snapshot()
        },
        move |snap| after_snapshot(&ctx_render, &snap, should_run),
    );
}

/// Render `snap`, and — when it followed a `Start`/`Resume` that actually landed
/// a `Running` run — spawn the parse/author loop (never on a rejected one).
fn after_snapshot(ctx: &Rc<ArchiveCtx>, snap: &ArchiveImportSnapshot, should_run: bool) {
    render(ctx, snap);
    if should_run && snap.run_state == Some(RunState::Running) {
        // Fire-and-forget: `run_import` mutates the machine's own snapshot as
        // it goes, and the tick below is what repaints it.
        let machine = Arc::clone(&ctx.machine);
        // Phase two (`archive-import.md` § Linking and merging, rulings 4 and
        // 11): the links ride the run itself — once attached, a run that
        // completes sets the Done step's link counts and announces its
        // platform, so nothing is glued onto the spawn but the repaint of the
        // Contacts rows afterwards. A login with no links runs with no round.
        let linked = crate::conversations::conv_backend::archive_links()
            .map(|(links, _)| machine.attach_links(links))
            .is_some();
        ctx.rt.spawn(async move {
            if let Err(e) = machine.run_import().await {
                tracing::warn!("[settings/archive_import] run_import: {e:?}");
            }
            if linked {
                crate::archive_glue::mark_views_dirty();
            }
        });
    }
    if snap.run_state == Some(RunState::Running) {
        start_progress_tick(ctx);
    }
}

/// Whether the Progress repaint tick keeps running: only while a run is live on
/// the Progress step — it paints the snapshot that ends it (Done, a pause, a
/// cancel) and stops.
fn progress_tick_continues(still_current: bool, snap: &ArchiveImportSnapshot) -> bool {
    still_current
        && snap.step == ArchiveImportStep::Progress
        && snap.run_state == Some(RunState::Running)
}

/// Repaint from the machine while `run_import` runs — a pure, synchronous
/// snapshot read on the GTK thread, no RPC (`mail_export.rs`'s tick).
fn start_progress_tick(ctx: &Rc<ArchiveCtx>) {
    if ctx.ticking.get() {
        return;
    }
    ctx.ticking.set(true);
    let ctx = Rc::clone(ctx);
    let generation = crate::actor_scope::current_generation();
    glib::timeout_add_local(
        std::time::Duration::from_millis(u64::from(super::mail_import::PROGRESS_TICK_MS)),
        move || {
            let current = crate::actor_scope::poll_still_current(generation);
            if !current {
                // The outgoing account's page and machine: paint nothing.
                ctx.ticking.set(false);
                return glib::ControlFlow::Break;
            }
            let snap = ctx.machine.snapshot();
            render(&ctx, &snap);
            if progress_tick_continues(current, &snap) {
                glib::ControlFlow::Continue
            } else {
                ctx.ticking.set(false);
                glib::ControlFlow::Break
            }
        },
    );
}

/// Render an `ArchiveImportSnapshot` into the page widgets (GTK main thread).
fn render(ctx: &Rc<ArchiveCtx>, snap: &ArchiveImportSnapshot) {
    let w = &ctx.w;
    *ctx.last.borrow_mut() = Some(snap.clone());

    super::render_error_label(&w.error_label, snap.error.as_deref());

    let step = snap.step;
    w.source_group
        .set_visible(step == ArchiveImportStep::Source);
    w.archive_group
        .set_visible(step == ArchiveImportStep::Archive);
    w.scope_group.set_visible(step == ArchiveImportStep::Scope);
    w.confirm_group
        .set_visible(step == ArchiveImportStep::Confirm);
    w.progress_group
        .set_visible(step == ArchiveImportStep::Progress);
    w.progress_list_group
        .set_visible(step == ArchiveImportStep::Progress);
    w.done_group.set_visible(step == ArchiveImportStep::Done);
    if step != ArchiveImportStep::Done {
        ctx.show_skipped.set(false);
    }

    // Pickers (echo-guarded).
    ctx.syncing.set(true);
    let kind_index = source_kind_index(snap.source_kind);
    if w.source_picker.selected() != kind_index {
        w.source_picker.set_selected(kind_index);
    }
    let mode_index = audience_mode_index(snap.audience_mode);
    if w.audience_picker.selected() != mode_index {
        w.audience_picker.set_selected(mode_index);
    }
    ctx.syncing.set(false);

    // Step 1.
    paint_source(w, Some(snap), snap.source_kind);

    // Step 2.
    let summary = archive_summary_text(snap);
    w.archive_summary_row.set_visible(summary.is_some());
    w.archive_summary
        .set_text(summary.as_deref().unwrap_or_default());

    // Step 3.
    render_category_rows(ctx, snap);
    w.audience_summary
        .set_text(&audience_text(Some(snap), snap.audience_mode));
    w.hidden_tiers_note
        .set_visible(snap.nest_supports_hidden_tiers == Some(false));

    // Step 4.
    w.confirm_summary
        .set_text(&composed(&compose::confirm_summary(
            snap.confirm_records,
            snap.confirm_bytes,
        )));

    // Step 5.
    w.progress_summary.set_text(&progress_text(snap));
    let fraction = compose::progress_fraction(snap);
    w.progress_bar.set_fraction(fraction);
    w.progress_bar
        .set_text(Some(&format!("{:.0}%", fraction * 100.0)));
    crate::testid::set_test_attr_text(&w.progress_bar, "fraction", &format!("{fraction:.4}"));
    let skip_log = snap.skip_log.join("\n");
    w.error_log.set_text(&skip_log);
    render_progress_rows(w, snap);
    // A concluded run may be left (the machine starts a fresh wizard on Back);
    // a running, paused or errored one is resumable and has no Back.
    w.progress_back_row.set_visible(matches!(
        snap.run_state,
        Some(RunState::Cancelled | RunState::Completed)
    ));

    // Step 6.
    w.done_summary
        .set_text(&composed(&compose::done_summary(snap)));
    w.done_error_log.set_text(&skip_log);
    w.done_error_log_row.set_visible(ctx.show_skipped.get());
    w.profile_prefill_button
        .set_sensitive(snap.archive_profile.is_some() && !snap.profile_applied);
    w.folder_link.set_label(&composed(&compose::folder_link(
        snap.folder_name.as_deref().unwrap_or_default(),
    )));
}

/// Rebuild the Scope step's category rows when what they paint changed: an
/// importable category is a checkbox, a model-only one a label marked `kept`
/// (kept in the folder, never selectable — § What each category becomes).
fn render_category_rows(ctx: &Rc<ArchiveCtx>, snap: &ArchiveImportSnapshot) {
    let w = &ctx.w;
    let key = category_key(&snap.categories);
    if *w.category_key.borrow() == key {
        return;
    }
    while let Some(child) = w.category_box.first_child() {
        w.category_box.remove(&child);
    }
    for row in &snap.categories {
        let line = composed(&compose::scope_row(row));
        if row.importable {
            let cb = gtk::CheckButton::with_label(&line);
            cb.set_active(row.selected);
            set_test_id(&cb, ids::ARCHIVE_IMPORT_SCOPE_CATEGORY_ITEM);
            mark_row_state(&cb, if row.selected { "on" } else { "off" });
            let ctx = Rc::clone(ctx);
            let token = row.token.clone();
            cb.connect_toggled(move |cb| {
                let selected = cb.is_active();
                mark_row_state(cb, if selected { "on" } else { "off" });
                // Keep the rebuild key in step, so the dispatch's render does
                // not tear down the row the user just toggled.
                if let Some(entry) = ctx
                    .w
                    .category_key
                    .borrow_mut()
                    .iter_mut()
                    .find(|e| e.0 == token)
                {
                    entry.3 = selected;
                }
                dispatch_actions(
                    &ctx,
                    vec![ArchiveImportAction::ToggleCategory {
                        token: token.clone(),
                        selected,
                    }],
                );
            });
            w.category_box.append(&cb);
        } else {
            let kept = gtk::Label::builder()
                .label(line.as_str())
                .xalign(0.0)
                .wrap(true)
                .build();
            set_test_id(&kept, ids::ARCHIVE_IMPORT_SCOPE_CATEGORY_ITEM);
            mark_row_state(&kept, "kept");
            w.category_box.append(&kept);
        }
    }
    *w.category_key.borrow_mut() = key;
}

/// The Progress step's per-category rows — the selected, importable categories
/// only, each an item with its name and `imported/count` progress.
fn render_progress_rows(w: &ArchiveWidgets, snap: &ArchiveImportSnapshot) {
    let mut rows = w.progress_rows.borrow_mut();
    for row in rows.drain(..) {
        w.progress_list_group.remove(&row);
    }
    for c in compose::page_text(snap).progress_rows {
        let label = c.label.resolve(crate::i18n::strings::lookup);
        let row = adw::ActionRow::builder().title(&label).build();
        row.add_prefix(&super::value_marker(
            ids::ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM,
            &label,
        ));
        row.add_suffix(&super::value_marker(
            ids::ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM_NAME,
            &label,
        ));
        let progress = composed(&c.progress);
        row.set_subtitle(&progress);
        row.add_suffix(&super::value_marker(
            ids::ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM_PROGRESS,
            &progress,
        ));
        w.progress_list_group.add(&row);
        rows.push(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_archive_import_machine::ArchiveSummaryView;

    fn summary() -> ArchiveSummaryView {
        ArchiveSummaryView {
            platform_label: "Facebook".into(),
            owner_display_name: "Test Owner".into(),
            first_at: 1_580_000_000_000_000,
            last_at: 1_650_000_002_000_000,
            media_bytes: 62,
            archive_bytes: 4_096,
            known_audience: 3,
            unknown_audience: 2,
        }
    }

    #[test]
    fn the_pickers_round_trip_their_positions() {
        for kind in SOURCE_KINDS {
            assert_eq!(source_kind_at(source_kind_index(kind)), kind);
        }
        for mode in AUDIENCE_MODES {
            assert_eq!(audience_mode_at(audience_mode_index(mode)), mode);
        }
        assert_eq!(source_kind_at(99), ArchiveSourceKind::Facebook);
        assert_eq!(audience_mode_at(99), AudienceMode::Original);
    }

    #[test]
    fn the_export_link_follows_the_pick_before_the_snapshot_catches_up() {
        let mut snap = ArchiveImportSnapshot::empty();
        snap.source_kind = ArchiveSourceKind::Facebook;
        snap.source_export_url = "https://example.test/facebook".into();
        assert_eq!(
            export_url(Some(&snap), ArchiveSourceKind::Facebook),
            "https://example.test/facebook"
        );
        assert_eq!(
            export_url(Some(&snap), ArchiveSourceKind::Instagram),
            ArchiveSourceKind::Instagram.export_request_url(),
            "a stale snapshot never answers for a freshly picked platform"
        );
        assert_eq!(
            export_url(None, ArchiveSourceKind::Facebook),
            ArchiveSourceKind::Facebook.export_request_url()
        );
    }

    #[test]
    fn the_archive_summary_renders_dates_sizes_and_the_undated_form() {
        let mut snap = ArchiveImportSnapshot::empty();
        assert_eq!(archive_summary_text(&snap), None);
        snap.summary = Some(summary());
        let text = archive_summary_text(&snap).unwrap();
        assert!(
            text.starts_with("Facebook · Test Owner · 2020-01-26 – 2022-04-15 · "),
            "{text}"
        );
        snap.summary = Some(ArchiveSummaryView {
            first_at: 0,
            last_at: 0,
            ..summary()
        });
        assert!(
            archive_summary_text(&snap)
                .unwrap()
                .contains("no dated records")
        );
    }

    #[test]
    fn the_audience_sentence_states_known_vs_unknown_and_only_me_overrides_it() {
        let mut snap = ArchiveImportSnapshot::empty();
        snap.summary = Some(summary());
        assert_eq!(
            audience_text(Some(&snap), AudienceMode::Original),
            S::scope_audience_summary_fmt("3", "2")
        );
        assert_eq!(
            audience_text(Some(&snap), AudienceMode::OnlyMe),
            S::SCOPE_AUDIENCE_SUMMARY_ONLY_ME
        );
    }

    #[test]
    fn the_progress_line_counts_settled_records_and_carries_the_timing() {
        let mut snap = ArchiveImportSnapshot::empty();
        snap.total = 4;
        snap.imported = 2;
        snap.skipped = 1;
        snap.run_state = Some(RunState::Running);
        snap.elapsed_secs = 125;
        snap.eta_secs = Some(3_600);
        assert!((compose::progress_fraction(&snap) - 0.75).abs() < 1e-9);
        let text = progress_text(&snap);
        assert!(
            text.starts_with(&S::progress_summary_fmt(S::STATE_RUNNING, "2", "4", "1")),
            "{text}"
        );
        assert!(text.ends_with("2m elapsed · about 1h 0m left"), "{text}");
        snap.eta_secs = None;
        assert!(progress_text(&snap).ends_with("2m elapsed · estimating the time left"));
    }

    #[test]
    fn the_tick_stops_once_the_run_is_no_longer_live_on_progress() {
        let mut snap = ArchiveImportSnapshot::empty();
        snap.step = ArchiveImportStep::Progress;
        snap.run_state = Some(RunState::Running);
        assert!(progress_tick_continues(true, &snap));
        assert!(!progress_tick_continues(false, &snap), "a retired account");
        snap.run_state = Some(RunState::Paused);
        assert!(!progress_tick_continues(true, &snap));
        snap.run_state = Some(RunState::Running);
        snap.step = ArchiveImportStep::Done;
        assert!(!progress_tick_continues(true, &snap));
    }

    #[test]
    fn the_category_rebuild_key_tracks_what_a_row_paints() {
        let row = |token: &str, selected| CategoryRow {
            token: token.into(),
            count: 4,
            importable: true,
            selected,
            imported: 0,
            skipped: 0,
        };
        let a = category_key(&[row("posts", true)]);
        let mut progressed = row("posts", true);
        progressed.imported = 3;
        assert_eq!(
            a,
            category_key(&[progressed]),
            "progress alone never rebuilds the Scope rows"
        );
        assert_ne!(a, category_key(&[row("posts", false)]));
    }
}
