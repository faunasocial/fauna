//! The user-facing "Import mailbox" wizard page (linux — the first
//! trickle-down leg behind tui, the lead app).
//!
//! Where a person pulls their existing mail from a foreign IMAP server
//! (Gmail / Outlook / iCloud / generic) into their Fauna mailbox — a
//! five-screen wizard (Source → Scope → Confirm → Progress → Done). Target
//! behavior: `docs/goal/behavior/mailbox-migration.md` § UX shape / § Wizard
//! steps / § Credential handling. UX/IDs: `tests/e2e-unified/ui.yaml`
//! `mail-import` page + its `mail-import-mailbox-progress-list` component.
//!
//! Per `mailbox-migration.md` § Where logic lives this layer holds **no**
//! business logic — it is a dumb renderer of [`MailImportSnapshot`] and a
//! dispatcher of [`MailImportAction`]; the wizard FSM (step transitions,
//! provider presets, mailbox default-selection, the fetch-drive loop) lives in
//! the shared `fauna_client_mail_settings::import` machine (priority #2/#4).
//! Direct siblings: `settings/mail_export.rs` (this page's GTK shape) and
//! `apps/fauna-tui/src/settings/mail_import.rs` (this page's behavior).
//!
//! # Unlike `mail_export.rs`, the backend is REAL
//!
//! Every Import RPC has shipped since 2026-07-08 (§ Implementation status
//! today), and both machine seams (`MailImportNest`, `ImportSourceNest`) are
//! real, not stubs. So `Start`/`Pause`/`Resume`/`Cancel` drive a genuine
//! `import_sessions` row, and `Connect` opens a genuine session against the
//! foreign IMAP source. A rejection (bad credentials, a locked source, an
//! over-quota mailbox, …) is a real nest/source answer bridged onto
//! `error-message` by the snapshot's `error` — never a fake-green, never a
//! dropped command (testing.md point 11).
//!
//! # One element set, shown conditionally by step
//!
//! All five steps' `mail-import-*` widgets are built once into the tree (so
//! every ui.yaml ID is statically present), and `render()` shows only the
//! active step's group — the `mail_export.rs` pattern. Two consequences worth
//! stating, because this page is the first linux wizard where they bite:
//!
//! - **The `wizard-back-button` / `wizard-next-button` ids repeat across
//!   steps** (Scope has Back+Next, Confirm has Back+Start). That is safe
//!   *because* of the step gating: `automation::find` prunes non-showing
//!   subtrees, so exactly one of each is findable at a time — the same
//!   mechanism that keeps a background stack page's `error-message` from
//!   shadowing the live one. So the non-Source groups are built
//!   `visible(false)` **at construction** rather than being left visible until
//!   the first render hides them: that window — build to first hydrate — would
//!   otherwise have two of each id addressable, a real flake.
//! - Step visibility is the ONLY thing gating them, so a group must never be
//!   left visible for two steps.
//!
//! # Two reads the driver makes with NO wait, and what they cost
//!
//! The automation agent acks a command as soon as the GTK handler returns, but
//! a dispatch hops onto the tokio runtime and renders only when it comes back.
//! Everywhere the e2e walk reads immediately after acting, the page must
//! therefore answer from local, synchronous truth — the async render then
//! re-applies the same thing:
//!
//! - **Pick provider, then type into its field.** `_fill_generic_source`
//!   selects Generic IMAP and types into `mail-import-source-host` with nothing
//!   in between; a hidden row is pruned by `find`, so the type would 404.
//!   [`apply_source_visibility`] is a pure function of the pick and runs inside
//!   the picker's own handler.
//! - **Toggle a mailbox, then read its `state`.** The marker class always wins
//!   over the widget's live state in `automation::agent::attr`, so leaving it
//!   to the dispatch's render reports the PREVIOUS selection — stale, not
//!   merely early. [`mark_row_state`] runs inside `connect_toggled`.
//!
//! Note the third read needs nothing: because the text fields are drafts
//! committed at the transition (below), typing then clicking Connect crosses no
//! async boundary at all.
//!
//! # Per-provider Source-step field visibility
//!
//! The table is `mailbox-migration.md` § Wizard steps step 1's "Required
//! fields", as transcribed by the tui lead app's module docs:
//!
//! - **Gmail / iCloud** show `source-username` + `source-app-password` (plus
//!   the provider's app-password help line) — host/port/tls-mode stay on the
//!   provider preset the machine's `SelectSourceKind` arm applies.
//! - **Outlook** shows the OAuth button **and** the IMAP-fallback fields
//!   (host/port/tls-mode/username/password) — ui.yaml's own "(+ Outlook
//!   fallback)" annotation on those four ids is what settles this.
//! - **Generic** shows the IMAP fields alone; no app-password, no OAuth.
//!
//! `mail-import-source-oauth-button` paints (ui.yaml requires the element
//! exist) but is never actuable (`sensitive(false)`) — no app wires the
//! Microsoft Graph dance yet (`mailbox-migration.md`'s own "Not in scope"
//! list). The IMAP fallback fields beside it are the real, working path for an
//! Outlook account today.
//!
//! # The Entries ARE the draft buffers, committed at the transition
//!
//! Unlike `mail_export.rs`'s scope fields — which dispatch a `Set*` on every
//! keystroke — this page reads each `gtk::Entry`'s text at the transition and
//! commits the whole Source (or Scope) form as ONE ordered multi-action
//! dispatch, the tui `connect_actions` / `scope_next_actions` shape. Two
//! reasons, and the first is a correctness one:
//!
//! 1. A per-keystroke dispatch spawns one task per character; clicking Connect
//!    spawns another that can be ordered *before* the last keystroke's — which
//!    on this page means logging into the source with a truncated password.
//!    Committing at the transition makes the form atomic by construction.
//! 2. `render()` therefore never writes back into a text Entry, so the caret
//!    never jumps and a failed-connect retry keeps the credentials on screen
//!    ("the user can retry from this screen without re-entering credentials",
//!    § Wizard steps step 2).
//!
//! # `mail-import-scope-mailbox-mapping` is informational only
//!
//! No `MailImportAction` exists to change the destination mailbox mapping —
//! the doc names no control for it either (§ Wizard steps step 3: "1:1 by
//! default … source mailboxes with names matching no Fauna standard mailbox
//! land as user-created mailboxes named after the source"). Painted as a plain
//! visible label describing the rule.
//!
//! # The two Done-step buttons have no machine action
//!
//! `mail-import-view-imported-button` / `mail-import-review-skipped-button`
//! are not backed by any `MailImportAction` — the shared machine never wired a
//! "view inbox" / "skip log" RPC. Both navigate to Conversations (where mail
//! lives), the tui lead app's own resolution: a real navigation, not a stub,
//! but "Review skipped" cannot deep-link to a skip-log page that exists
//! nowhere in the app yet (the error log lives inline on the Progress screen
//! instead).

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_mail_settings::{
    DEFAULT_MAX_SIZE_MB, ImportSessionState, ImportSourceKind, ImportStep, ImportTlsMode,
    MailImportAction, MailImportMachine, MailImportSnapshot, SOURCE_KINDS, TLS_MODES,
    import_source_kind_label, import_tls_mode_label,
};
use fauna_core::secret::SecretString;

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::mail_import as S;
use crate::testid::set_test_id;

/// How often a Progress screen re-reads its machine while a drive loop runs —
/// this page's `run_import` and the export page's `run_export` alike (one
/// cadence for both wizards). A pure, cheap, synchronous snapshot read — the tui
/// `periodic_tick` shape, which exists for the same reason: the drive loop
/// mutates the machine's own snapshot in the background and nothing else would
/// repaint it.
pub(super) const PROGRESS_TICK_MS: u32 = 400;

/// Handles to the widgets the snapshot renders into.
#[derive(Clone)]
struct ImportWidgets {
    error_label: gtk::Label,

    // Step 1 — Source.
    source_group: adw::PreferencesGroup,
    source_picker: gtk::DropDown,
    username_row: adw::ActionRow,
    username_input: gtk::Entry,
    app_password_row: adw::ActionRow,
    app_password_input: gtk::Entry,
    app_password_help: gtk::Label,
    oauth_row: adw::ActionRow,
    host_row: adw::ActionRow,
    host_input: gtk::Entry,
    port_row: adw::ActionRow,
    port_input: gtk::Entry,
    tls_mode_row: adw::ActionRow,
    tls_mode_picker: gtk::DropDown,
    password_row: adw::ActionRow,
    password_input: gtk::Entry,

    // Step 2 — Scope.
    mailboxes_group: adw::PreferencesGroup,
    mailbox_rows: Rc<std::cell::RefCell<Vec<gtk::CheckButton>>>,
    mailboxes_placeholder: adw::ActionRow,
    scope_group: adw::PreferencesGroup,
    date_from_input: gtk::Entry,
    max_size_input: gtk::Entry,

    // Step 3 — Confirm.
    confirm_group: adw::PreferencesGroup,
    confirm_summary: gtk::Label,

    // Step 4 — Progress.
    progress_group: adw::PreferencesGroup,
    progress_summary: gtk::Label,
    progress_bar: gtk::ProgressBar,
    error_log: gtk::Label,
    mailbox_progress_group: adw::PreferencesGroup,
    mailbox_progress_rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>>,

    // Step 5 — Done.
    done_group: adw::PreferencesGroup,
    done_summary: gtk::Label,
}

/// Everything the page's handlers + render need. `Rc`-shared into every closure.
struct ImportCtx {
    machine: Arc<MailImportMachine>,
    rt: tokio::runtime::Handle,
    /// Set while `render()` programmatically updates a picker, so its
    /// `selected_notify` handler doesn't echo the change back as a dispatch.
    syncing: Cell<bool>,
    /// Whether the Progress repaint tick is already running — one at a time.
    ticking: Cell<bool>,
    w: ImportWidgets,
}

/// A tagged wizard nav button (`wizard-back-button` / `wizard-next-button` —
/// real ui.yaml elements on this page, unlike `mail_export.rs`'s untagged
/// pair).
fn wizard_button(id: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&button, id);
    button
}

/// An `adw::ActionRow` holding one tagged text `gtk::Entry`.
fn entry_row(id: &str, title: &str, placeholder: &str) -> (adw::ActionRow, gtk::Entry) {
    let input = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&input, id);
    let row = adw::ActionRow::builder()
        .title(title)
        .activatable(false)
        .build();
    row.add_suffix(&input);
    (row, input)
}

fn source_kind_label(kind: ImportSourceKind) -> String {
    import_source_kind_label(kind).resolve(crate::i18n::strings::lookup)
}

fn tls_mode_label(mode: ImportTlsMode) -> String {
    import_tls_mode_label(mode).resolve(crate::i18n::strings::lookup)
}

/// `ImportSourceKind` for a DropDown position. An out-of-range index keeps the
/// ratified default (Gmail) rather than panicking — the
/// `mail_export::format_at` contract.
fn source_kind_at(index: u32) -> ImportSourceKind {
    SOURCE_KINDS
        .get(index as usize)
        .copied()
        .unwrap_or(ImportSourceKind::Gmail)
}

/// DropDown position for an `ImportSourceKind`.
fn source_kind_index(kind: ImportSourceKind) -> u32 {
    SOURCE_KINDS.iter().position(|k| *k == kind).unwrap_or(0) as u32
}

/// `ImportTlsMode` for a DropDown position; out-of-range keeps Implicit.
fn tls_mode_at(index: u32) -> ImportTlsMode {
    TLS_MODES
        .get(index as usize)
        .copied()
        .unwrap_or(ImportTlsMode::Implicit)
}

/// DropDown position for an `ImportTlsMode`.
fn tls_mode_index(mode: ImportTlsMode) -> u32 {
    TLS_MODES.iter().position(|m| *m == mode).unwrap_or(0) as u32
}

/// Build the "Import mailbox" wizard page.
pub fn build_mail_import_page() -> gtk::Box {
    let (page, widgets, buttons) = build_page_parts();
    wire_machine(widgets, buttons);
    crate::testid::wrap_page_with_heading(S::TITLE, ids::PAGE_HEADING, &page)
}

/// The widget tree, before it is wired to a machine.
///
/// Split out of [`build_mail_import_page`] so tests can hold the
/// [`ImportWidgets`] and drive the two pure render helpers
/// ([`apply_source_visibility`], [`mark_row_state`]) directly. Wiring needs a
/// live client, which a unit test does not have — without this split the
/// per-provider field table would have no test at all.
fn build_page_parts() -> (adw::PreferencesPage, ImportWidgets, WizardButtons) {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("mail-send-receive-symbolic")
        .build();

    // --- Page-level error (always visible group; the heading is painted by
    // --- `wrap_page_with_heading` below) ---
    let top_group = adw::PreferencesGroup::builder()
        .description(S::DESCRIPTION)
        .build();
    let error_label = gtk::Label::builder().visible(false).wrap(true).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);
    page.add(&top_group);

    // === Step 1 — Source =====================================================
    let source_group = adw::PreferencesGroup::builder()
        .title(S::SOURCE_TITLE)
        .build();

    let source_options: Vec<String> = SOURCE_KINDS.into_iter().map(source_kind_label).collect();
    let source_refs: Vec<&str> = source_options.iter().map(String::as_str).collect();
    let source_picker = gtk::DropDown::from_strings(&source_refs);
    source_picker.set_valign(gtk::Align::Center);
    set_test_id(&source_picker, ids::MAIL_IMPORT_SOURCE_PICKER);
    let source_picker_row = adw::ActionRow::builder().title(S::SOURCE_TITLE).build();
    source_picker_row.add_suffix(&source_picker);
    source_group.add(&source_picker_row);

    // Painted for every provider kind — nothing in ui.yaml scopes
    // `mail-import-source-username` to a subset the way the other four are
    // annotated.
    let (username_row, username_input) = entry_row(
        ids::MAIL_IMPORT_SOURCE_USERNAME,
        S::SOURCE_USERNAME_PLACEHOLDER,
        S::SOURCE_USERNAME_PLACEHOLDER,
    );
    source_group.add(&username_row);

    // Gmail / iCloud.
    let (app_password_row, app_password_input) = entry_row(
        ids::MAIL_IMPORT_SOURCE_APP_PASSWORD,
        S::SOURCE_APP_PASSWORD_LABEL,
        S::SOURCE_APP_PASSWORD_LABEL,
    );
    app_password_input.set_visibility(false);
    source_group.add(&app_password_row);
    let app_password_help = gtk::Label::builder()
        .label(S::SOURCE_APP_PASSWORD_HELP_GMAIL)
        .wrap(true)
        .xalign(0.0)
        .build();
    app_password_help.add_css_class("dim-label");
    source_group.add(&app_password_help);

    // Outlook — the OAuth start, painted but never actuable (module docs).
    let oauth_button = gtk::Button::builder()
        .label(S::SOURCE_OAUTH_BUTTON)
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    set_test_id(&oauth_button, ids::MAIL_IMPORT_SOURCE_OAUTH_BUTTON);
    let oauth_row = adw::ActionRow::builder().activatable(false).build();
    oauth_row.add_suffix(&oauth_button);
    source_group.add(&oauth_row);

    // Generic (+ Outlook fallback).
    let (host_row, host_input) = entry_row(
        ids::MAIL_IMPORT_SOURCE_HOST,
        S::SOURCE_HOST_PLACEHOLDER,
        S::SOURCE_HOST_PLACEHOLDER,
    );
    source_group.add(&host_row);
    let (port_row, port_input) = entry_row(
        ids::MAIL_IMPORT_SOURCE_PORT,
        S::SOURCE_PORT_PLACEHOLDER,
        S::SOURCE_PORT_PLACEHOLDER,
    );
    source_group.add(&port_row);
    let tls_options: Vec<String> = TLS_MODES.into_iter().map(tls_mode_label).collect();
    let tls_refs: Vec<&str> = tls_options.iter().map(String::as_str).collect();
    let tls_mode_picker = gtk::DropDown::from_strings(&tls_refs);
    tls_mode_picker.set_valign(gtk::Align::Center);
    set_test_id(&tls_mode_picker, ids::MAIL_IMPORT_SOURCE_TLS_MODE);
    let tls_mode_row = adw::ActionRow::builder().title(S::TLS_IMPLICIT).build();
    tls_mode_row.add_suffix(&tls_mode_picker);
    source_group.add(&tls_mode_row);
    let (password_row, password_input) = entry_row(
        ids::MAIL_IMPORT_SOURCE_PASSWORD,
        S::SOURCE_PASSWORD_PLACEHOLDER,
        S::SOURCE_PASSWORD_PLACEHOLDER,
    );
    password_input.set_visibility(false);
    source_group.add(&password_row);

    let connect_button = gtk::Button::builder()
        .label(S::CONNECT_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&connect_button, ids::MAIL_IMPORT_CONNECT_BUTTON);
    let connect_row = adw::ActionRow::builder().activatable(false).build();
    connect_row.add_suffix(&connect_button);
    source_group.add(&connect_row);
    page.add(&source_group);

    // === Step 2 — Scope ======================================================
    // The multi-select container's id rides the group's header marker; the
    // per-mailbox rows are rebuilt from the snapshot at render time.
    let mailboxes_group = adw::PreferencesGroup::builder()
        .title(S::SCOPE_MAILBOXES_LABEL)
        .visible(false)
        .build();
    mailboxes_group.set_header_suffix(Some(&super::marker(ids::MAIL_IMPORT_SCOPE_MAILBOXES)));
    let mailboxes_placeholder = adw::ActionRow::builder()
        .title(S::SCOPE_MAILBOXES_EMPTY)
        .build();
    mailboxes_group.add(&mailboxes_placeholder);
    page.add(&mailboxes_group);

    let scope_group = adw::PreferencesGroup::builder()
        .title(S::SCOPE_TITLE)
        .visible(false)
        .build();
    let (date_from_row, date_from_input) = entry_row(
        ids::MAIL_IMPORT_SCOPE_DATE_FROM,
        S::SCOPE_DATE_FROM_PLACEHOLDER,
        S::SCOPE_DATE_FROM_PLACEHOLDER,
    );
    scope_group.add(&date_from_row);
    let (max_size_row, max_size_input) = entry_row(
        ids::MAIL_IMPORT_SCOPE_MAX_SIZE,
        S::SCOPE_MAX_SIZE_LABEL,
        S::SCOPE_MAX_SIZE_LABEL,
    );
    max_size_input.set_text(DEFAULT_MAX_SIZE_MB);
    scope_group.add(&max_size_row);
    // Informational only — no action changes the mapping (module docs).
    let mapping_label = gtk::Label::builder()
        .label(S::SCOPE_MAILBOX_MAPPING_LABEL)
        .wrap(true)
        .xalign(0.0)
        .build();
    mapping_label.add_css_class("dim-label");
    set_test_id(&mapping_label, ids::MAIL_IMPORT_SCOPE_MAILBOX_MAPPING);
    scope_group.add(&mapping_label);

    let scope_back = wizard_button(ids::WIZARD_BACK_BUTTON, S::BACK);
    let scope_next = wizard_button(ids::WIZARD_NEXT_BUTTON, S::NEXT);
    let scope_nav_row = adw::ActionRow::builder().activatable(false).build();
    scope_nav_row.add_suffix(&scope_back);
    scope_nav_row.add_suffix(&scope_next);
    scope_group.add(&scope_nav_row);
    page.add(&scope_group);

    // === Step 3 — Confirm ====================================================
    let confirm_group = adw::PreferencesGroup::builder()
        .title(S::CONFIRM_TITLE)
        .visible(false)
        .build();
    let confirm_summary = super::blank_value_marker(ids::MAIL_IMPORT_CONFIRM_SUMMARY);
    let confirm_row = adw::ActionRow::builder().title(S::CONFIRM_TITLE).build();
    confirm_row.add_suffix(&confirm_summary);
    confirm_group.add(&confirm_row);
    let start_button = gtk::Button::builder()
        .label(S::START_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&start_button, ids::MAIL_IMPORT_START_BUTTON);
    let confirm_back = wizard_button(ids::WIZARD_BACK_BUTTON, S::BACK);
    let confirm_nav_row = adw::ActionRow::builder().activatable(false).build();
    confirm_nav_row.add_suffix(&confirm_back);
    confirm_nav_row.add_suffix(&start_button);
    confirm_group.add(&confirm_nav_row);
    page.add(&confirm_group);

    // === Step 4 — Progress ===================================================
    let progress_group = adw::PreferencesGroup::builder()
        .title(S::PROGRESS_TITLE)
        .visible(false)
        .build();
    let progress_summary = super::blank_value_marker(ids::MAIL_IMPORT_PROGRESS_SUMMARY);
    let progress_summary_row = adw::ActionRow::builder().title(S::PROGRESS_TITLE).build();
    progress_summary_row.add_suffix(&progress_summary);
    progress_group.add(&progress_summary_row);
    let progress_bar = gtk::ProgressBar::builder()
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    set_test_id(&progress_bar, ids::MAIL_IMPORT_PROGRESS_BAR);
    progress_group.add(&progress_bar);

    let pause_button = gtk::Button::builder()
        .label(S::PAUSE_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&pause_button, ids::MAIL_IMPORT_PAUSE_BUTTON);
    let resume_button = gtk::Button::builder()
        .label(S::RESUME_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&resume_button, ids::MAIL_IMPORT_RESUME_BUTTON);
    let cancel_button = gtk::Button::builder()
        .label(S::CANCEL_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&cancel_button, ids::MAIL_IMPORT_CANCEL_BUTTON);
    let progress_controls_row = adw::ActionRow::builder().activatable(false).build();
    progress_controls_row.add_suffix(&pause_button);
    progress_controls_row.add_suffix(&resume_button);
    progress_controls_row.add_suffix(&cancel_button);
    progress_group.add(&progress_controls_row);

    let error_log = super::blank_value_marker(ids::MAIL_IMPORT_ERROR_LOG);
    let error_log_row = adw::ActionRow::builder().title(S::ERROR_LOG_TITLE).build();
    error_log_row.add_suffix(&error_log);
    progress_group.add(&error_log_row);

    let mailbox_progress_group = adw::PreferencesGroup::new();
    mailbox_progress_group
        .set_header_suffix(Some(&super::marker(ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST)));
    progress_group.add(&mailbox_progress_group);
    page.add(&progress_group);

    // === Step 5 — Done =======================================================
    let done_group = adw::PreferencesGroup::builder()
        .title(S::DONE_TITLE)
        .visible(false)
        .build();
    let done_summary = super::blank_value_marker(ids::MAIL_IMPORT_DONE_SUMMARY);
    let done_summary_row = adw::ActionRow::builder().title(S::DONE_TITLE).build();
    done_summary_row.add_suffix(&done_summary);
    done_group.add(&done_summary_row);
    let view_imported_button = gtk::Button::builder()
        .label(S::VIEW_IMPORTED_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&view_imported_button, ids::MAIL_IMPORT_VIEW_IMPORTED_BUTTON);
    let review_skipped_button = gtk::Button::builder()
        .label(S::REVIEW_SKIPPED_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(
        &review_skipped_button,
        ids::MAIL_IMPORT_REVIEW_SKIPPED_BUTTON,
    );
    let done_nav_row = adw::ActionRow::builder().activatable(false).build();
    done_nav_row.add_suffix(&view_imported_button);
    done_nav_row.add_suffix(&review_skipped_button);
    done_group.add(&done_nav_row);
    page.add(&done_group);

    let widgets = ImportWidgets {
        error_label,
        source_group,
        source_picker,
        username_row,
        username_input,
        app_password_row,
        app_password_input,
        app_password_help,
        oauth_row,
        host_row,
        host_input,
        port_row,
        port_input,
        tls_mode_row,
        tls_mode_picker,
        password_row,
        password_input,
        mailboxes_group,
        mailbox_rows: Rc::new(std::cell::RefCell::new(Vec::new())),
        mailboxes_placeholder,
        scope_group,
        date_from_input,
        max_size_input,
        confirm_group,
        confirm_summary,
        progress_group,
        progress_summary,
        progress_bar,
        error_log,
        mailbox_progress_group,
        mailbox_progress_rows: Rc::new(std::cell::RefCell::new(Vec::new())),
        done_group,
        done_summary,
    };
    let buttons = WizardButtons {
        connect_button,
        scope_back,
        scope_next,
        confirm_back,
        start_button,
        pause_button,
        resume_button,
        cancel_button,
        view_imported_button,
        review_skipped_button,
    };
    (page, widgets, buttons)
}

/// The interaction buttons threaded into `wire_machine` (kept out of
/// `ImportWidgets`, which only holds widgets `render()` writes into).
struct WizardButtons {
    connect_button: gtk::Button,
    scope_back: gtk::Button,
    scope_next: gtk::Button,
    confirm_back: gtk::Button,
    start_button: gtk::Button,
    pause_button: gtk::Button,
    resume_button: gtk::Button,
    cancel_button: gtk::Button,
    view_imported_button: gtk::Button,
    review_skipped_button: gtk::Button,
}

/// Connect the page to the shared `MailImportMachine`, hydrate on mount, and
/// wire every interaction. No-op when no client is available (the unit test).
fn wire_machine(widgets: ImportWidgets, b: WizardButtons) {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        None => return,
    };
    let machine = Arc::new(crate::mail_glue::build_mail_import_machine(&client));

    let ctx = Rc::new(ImportCtx {
        machine,
        rt: client.runtime_handle(),
        syncing: Cell::new(false),
        ticking: Cell::new(false),
        w: widgets,
    });

    hydrate_and_render(&ctx);

    // Provider picker → SelectSourceKind (skip render()'s echo). This one IS a
    // live dispatch, not a draft: the machine applies the provider preset
    // (host/port/tls_mode), so it must see the pick immediately.
    //
    // ⚠ The visibility flip is applied SYNCHRONOUSLY here, not left to the
    // dispatch's own render. `dispatch_actions` hops onto the tokio runtime and
    // renders when it comes back, but the automation agent acks a `select` as
    // soon as the GTK handler returns — so a driver that picks Generic IMAP and
    // immediately types into `mail-import-source-host` (which is exactly what
    // `test_mail_import.py::_fill_generic_source` does, with no wait between
    // the two) would find that row still hidden and 404. Which kind's fields
    // show is a pure function of the local pick, so it needs no round trip;
    // `render()` re-applies the same function from the snapshot afterwards.
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
                apply_source_visibility(&ctx.w, kind);
                dispatch_actions(&ctx, vec![MailImportAction::SelectSourceKind { kind }]);
            });
    }
    // TLS mode → SetTlsMode (echo-guarded); a cheap client-only mutation, but
    // dispatched live so the preset/override distinction stays visible.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .tls_mode_picker
            .clone()
            .connect_selected_notify(move |dd| {
                if ctx.syncing.get() {
                    return;
                }
                dispatch_actions(
                    &ctx,
                    vec![MailImportAction::SetTlsMode {
                        mode: tls_mode_at(dd.selected()),
                    }],
                );
            });
    }

    // Step 1→2 — commit the whole Source form, then Connect, in one op.
    {
        let ctx = Rc::clone(&ctx);
        b.connect_button
            .connect_clicked(move |_| dispatch_actions(&ctx, connect_actions(&ctx)));
    }
    // Step 2→3 — commit both Scope drafts, then advance.
    {
        let ctx = Rc::clone(&ctx);
        b.scope_next
            .connect_clicked(move |_| dispatch_actions(&ctx, scope_next_actions(&ctx)));
    }

    wire_dispatch(&ctx, &b.scope_back, MailImportAction::Back);
    wire_dispatch(&ctx, &b.confirm_back, MailImportAction::Back);
    wire_dispatch(&ctx, &b.start_button, MailImportAction::Start);
    wire_dispatch(&ctx, &b.pause_button, MailImportAction::Pause);
    wire_dispatch(&ctx, &b.resume_button, MailImportAction::Resume);

    // The two Done deep-links have no machine action — both go to
    // Conversations, where mail lives (module docs).
    for button in [&b.view_imported_button, &b.review_skipped_button] {
        button.connect_clicked(|btn| {
            crate::app::navigate_to_section(btn, "conversations");
        });
    }

    // Cancel is destructive — two-click inline confirm (already-imported
    // messages are kept, § UX shape step 5).
    super::wire_two_click(
        &b.cancel_button,
        S::CANCEL_BUTTON,
        crate::i18n::strings::common::CONFIRM_Q,
        true,
        |_| {},
        |_| {},
        move || dispatch_actions(&ctx, vec![MailImportAction::Cancel]),
    );
}

/// Wire a button to a single fire-and-render action.
fn wire_dispatch(ctx: &Rc<ImportCtx>, button: &gtk::Button, action: MailImportAction) {
    let ctx = Rc::clone(ctx);
    button.connect_clicked(move |_| dispatch_actions(&ctx, vec![action.clone()]));
}

/// Step 1→2: commit whichever Source-step fields the current kind actually
/// shows (the module docs' per-provider table), then Connect — one op. The
/// wire-action shape itself is `fauna_client_mail_settings::connect_actions`
/// (shared with tui); this side's own job is locating the strings — GTK
/// keeps the password / app-password as two separate `Entry` widgets, so the
/// kind-based routing to whichever one the picked provider shows happens
/// here, before the shared fn ever sees a single resolved secret.
fn connect_actions(ctx: &Rc<ImportCtx>) -> Vec<MailImportAction> {
    let w = &ctx.w;
    let kind = source_kind_at(w.source_picker.selected());
    let secret = match kind {
        ImportSourceKind::Gmail | ImportSourceKind::ICloud => w.app_password_input.text(),
        _ => w.password_input.text(),
    };
    fauna_client_mail_settings::connect_actions(
        kind,
        &w.host_input.text(),
        &w.port_input.text(),
        &w.username_input.text(),
        SecretString::from(secret.to_string()),
    )
}

/// Step 2→3: commit both Scope-step drafts, then advance. The wire-action
/// shape itself is `fauna_client_mail_settings::scope_next_actions` (shared
/// with tui); this side's own job is just locating the strings — same
/// division of labor as [`connect_actions`].
fn scope_next_actions(ctx: &Rc<ImportCtx>) -> Vec<MailImportAction> {
    let w = &ctx.w;
    fauna_client_mail_settings::scope_next_actions(
        &w.date_from_input.text(),
        &w.max_size_input.text(),
    )
}

fn hydrate_and_render(ctx: &Rc<ImportCtx>) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.hydrate().await;
            machine.snapshot()
        },
        move |snap| after_snapshot(&ctx_render, &snap, false, false),
    );
}

/// Re-seed the Source-step host/port drafts from the snapshot a
/// `SelectSourceKind` dispatch just wrote — apple's `syncDraftsFromSnapshot`
/// shape. Nothing did this before, so picking Outlook left the host field
/// empty and `connect_actions` sent it verbatim. Unconditional, matching
/// apple: a Generic pick after Gmail paints `imap.gmail.com` (no `Generic`
/// preset to overwrite it with) rather than blanking the field — a known,
/// accepted quirk, not something to special-case here.
fn seed_source_drafts(w: &ImportWidgets, snap: &MailImportSnapshot) {
    w.host_input.set_text(&snap.host);
    w.port_input.set_text(&snap.port.to_string());
}

/// Dispatch `actions` in order, awaiting each before the single re-read, so a
/// Scope→Confirm advance can never observe a half-committed scope. Each
/// `Result` is deliberately dropped — a dispatch failure arrives on the
/// snapshot's `error`, never here (the tui `MailImportDispatch` reasoning).
fn dispatch_actions(ctx: &Rc<ImportCtx>, actions: Vec<MailImportAction>) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    let should_run = actions
        .iter()
        .any(|a| matches!(a, MailImportAction::Start | MailImportAction::Resume));
    // Same introspection as `should_run`: a `SelectSourceKind` dispatch's
    // post-dispatch snapshot is what re-seeds the Source-step host/port
    // drafts (`seed_source_drafts`'s doc comment) — never the general render
    // fold, which every other dispatch and the progress tick also go through.
    let kind_changed = actions
        .iter()
        .any(|a| matches!(a, MailImportAction::SelectSourceKind { .. }));
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            for action in actions {
                let _ = machine.dispatch(action).await;
            }
            machine.snapshot()
        },
        move |snap| after_snapshot(&ctx_render, &snap, should_run, kind_changed),
    );
}

/// Render `snap`, and — when this snapshot followed a `Start`/`Resume` that
/// actually landed a `Running` session — spawn the fetch-drive loop and the
/// Progress repaint tick.
fn after_snapshot(
    ctx: &Rc<ImportCtx>,
    snap: &MailImportSnapshot,
    should_run: bool,
    kind_changed: bool,
) {
    if kind_changed {
        seed_source_drafts(&ctx.w, snap);
    }
    render(ctx, snap);
    if should_run && snap.session_state == Some(ImportSessionState::Running) {
        // Fire-and-forget: `run_import` mutates the machine's own snapshot as
        // it goes, and the tick below is what repaints it.
        let machine = Arc::clone(&ctx.machine);
        ctx.rt.spawn(async move {
            if let Err(e) = machine.run_import().await {
                tracing::warn!("[settings/mail_import] run_import: {e:?}");
            }
        });
        start_progress_tick(ctx);
    }
}

/// Repaint the Progress screen from the machine while `run_import` runs.
///
/// A pure, cheap, synchronous snapshot read on the GTK thread — no RPC, no
/// dispatch — the tui `RefreshMailImportProgress` op's shape. Stops as soon as
/// the wizard leaves the Progress step (Done, or a Cancel that unwinds it).
fn start_progress_tick(ctx: &Rc<ImportCtx>) {
    if ctx.ticking.get() {
        return;
    }
    ctx.ticking.set(true);
    let ctx = Rc::clone(ctx);
    glib::timeout_add_local(
        std::time::Duration::from_millis(u64::from(PROGRESS_TICK_MS)),
        move || {
            let snap = ctx.machine.snapshot();
            let keep_going = snap.step == ImportStep::Progress;
            render(&ctx, &snap);
            if !keep_going {
                ctx.ticking.set(false);
            }
            if keep_going {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        },
    );
}

/// Carry a mailbox row's selection on an explicit `state` marker class.
///
/// `automation::agent::attr` prefers a `test-attr-state-*` class over the
/// widget's own live state and answers the class verbatim — which is the only
/// way this row can report the `on`/`off` the cross-app
/// `MailImportActions.mailbox_selected` asserts (an unmarked `CheckButton`
/// reads back `true`/`false`). Because the class WINS, it must never be allowed
/// to lag the button: always re-mark in the same turn the state changes.
fn mark_row_state(cb: &gtk::CheckButton, selected: bool) {
    cb.remove_css_class("test-attr-state-on");
    cb.remove_css_class("test-attr-state-off");
    cb.add_css_class(if selected {
        "test-attr-state-on"
    } else {
        "test-attr-state-off"
    });
}

/// Show exactly the Source-step fields `kind` calls for (the module docs'
/// per-provider table).
///
/// A pure function of the provider pick, deliberately: it is called both from
/// `render()` (with the snapshot's kind) and SYNCHRONOUSLY from the picker's
/// own handler (with the freshly-picked kind), so revealing a field never waits
/// on a round trip. See that handler for the driver-visible reason.
fn apply_source_visibility(w: &ImportWidgets, kind: ImportSourceKind) {
    let app_password_kind = matches!(kind, ImportSourceKind::Gmail | ImportSourceKind::ICloud);
    let imap_fields_kind = matches!(kind, ImportSourceKind::Outlook | ImportSourceKind::Generic);
    // Painted for every kind — nothing in ui.yaml scopes `source-username` to a
    // subset the way the other four are annotated.
    w.username_row.set_visible(true);
    w.app_password_row.set_visible(app_password_kind);
    w.app_password_help.set_visible(app_password_kind);
    w.app_password_help
        .set_label(if kind == ImportSourceKind::Gmail {
            S::SOURCE_APP_PASSWORD_HELP_GMAIL
        } else {
            S::SOURCE_APP_PASSWORD_HELP_ICLOUD
        });
    w.oauth_row.set_visible(kind == ImportSourceKind::Outlook);
    w.host_row.set_visible(imap_fields_kind);
    w.port_row.set_visible(imap_fields_kind);
    w.tls_mode_row.set_visible(imap_fields_kind);
    w.password_row.set_visible(imap_fields_kind);
}

/// Render a `MailImportSnapshot` into the page widgets (GTK main thread).
fn render(ctx: &Rc<ImportCtx>, snap: &MailImportSnapshot) {
    let w = &ctx.w;

    super::render_error_label(&w.error_label, snap.error.as_deref());

    // Step-gated group visibility (the Scope step spans two groups). Exactly
    // one step is visible at a time — the `wizard-*-button` ids repeat across
    // steps and this is what disambiguates them (module docs).
    w.source_group.set_visible(snap.step == ImportStep::Source);
    w.mailboxes_group
        .set_visible(snap.step == ImportStep::Scope);
    w.scope_group.set_visible(snap.step == ImportStep::Scope);
    w.confirm_group
        .set_visible(snap.step == ImportStep::Confirm);
    w.progress_group
        .set_visible(snap.step == ImportStep::Progress);
    w.done_group.set_visible(snap.step == ImportStep::Done);

    // Pickers (echo-guarded).
    ctx.syncing.set(true);
    if w.source_picker.selected() != source_kind_index(snap.source_kind) {
        w.source_picker
            .set_selected(source_kind_index(snap.source_kind));
    }
    if w.tls_mode_picker.selected() != tls_mode_index(snap.tls_mode) {
        w.tls_mode_picker
            .set_selected(tls_mode_index(snap.tls_mode));
    }
    ctx.syncing.set(false);

    apply_source_visibility(w, snap.source_kind);

    // Mailbox multi-select: rebuild the CheckButtons from the snapshot. Each
    // row carries the indexed `mail-import-scope-mailbox-item` id, its name as
    // text, and an explicit `state` marker class — the `on`/`off` contract the
    // cross-app action layer reads (an unmarked CheckButton would read back
    // `true`/`false`).
    {
        let mut rows = w.mailbox_rows.borrow_mut();
        for cb in rows.drain(..) {
            w.mailboxes_group.remove(&cb);
        }
        for mb in &snap.mailboxes {
            let cb = gtk::CheckButton::with_label(&mb.name);
            cb.set_active(mb.selected);
            set_test_id(&cb, ids::MAIL_IMPORT_SCOPE_MAILBOX_ITEM);
            mark_row_state(&cb, mb.selected);
            {
                let ctx = Rc::clone(ctx);
                let name = mb.name.clone();
                cb.connect_toggled(move |cb| {
                    // ⚠ Re-mark SYNCHRONOUSLY from the button's own new state,
                    // for the `select`-then-type reason spelled out on the
                    // provider picker: the agent acks a click as soon as this
                    // handler returns, and `test_mail_import.py` asserts
                    // `mailbox_selected(..)` immediately after `toggle_mailbox`,
                    // with no wait. Leaving the marker to the dispatch's render
                    // means that read sees the PREVIOUS state — and, because
                    // the marker class always wins over the widget's live
                    // state, it would read stale rather than merely early.
                    mark_row_state(cb, cb.is_active());
                    dispatch_actions(
                        &ctx,
                        vec![MailImportAction::ToggleMailbox {
                            mailbox: name.clone(),
                        }],
                    );
                });
            }
            w.mailboxes_group.add(&cb);
            rows.push(cb);
        }
        w.mailboxes_placeholder
            .set_visible(snap.mailboxes.is_empty());
    }

    // Confirm summary.
    w.confirm_summary.set_text(&confirm_text(snap));

    // Progress fields.
    w.progress_summary.set_text(&progress_text(snap));
    let frac =
        fauna_core::format::quota_fraction(snap.imported_count as i64, snap.total_count as i64);
    w.progress_bar.set_fraction(frac);
    w.error_log.set_text(&snap.error_log.join("\n"));
    {
        let mut rows = w.mailbox_progress_rows.borrow_mut();
        for row in rows.drain(..) {
            w.mailbox_progress_group.remove(&row);
        }
        // The machine tracks only GLOBAL imported/skipped/errored counts, not a
        // per-mailbox breakdown (`MailImportSnapshot` has no `mailbox_progress`
        // field the way export's does) — so each row shows its planned message
        // count, not a live per-mailbox fraction. The tui lead app's own
        // accurate-to-what-exists simplification, not a gap this page adds.
        for mb in snap.mailboxes.iter().filter(|m| m.selected) {
            let row = adw::ActionRow::builder().title(&mb.name).build();
            row.add_prefix(&super::marker(ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM));
            let name_marker =
                super::blank_value_marker(ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME);
            name_marker.set_text(&mb.name);
            row.add_suffix(&name_marker);
            let prog_marker =
                super::blank_value_marker(ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS);
            prog_marker.set_text(&S::progress_row_fmt(&mb.message_count.to_string()));
            row.add_suffix(&prog_marker);
            w.mailbox_progress_group.add(&row);
            rows.push(row);
        }
    }

    // Done fields.
    w.done_summary.set_text(&done_text(snap));
}

/// The Confirm summary: the source, the selected-mailbox count, and their
/// summed `message_count` estimate (§ Wizard steps step 4 — no estimated-bytes
/// or wall-clock field exists on the snapshot, so neither is claimed here).
fn confirm_text(snap: &MailImportSnapshot) -> String {
    let selected = snap.mailboxes.iter().filter(|m| m.selected).count();
    let messages: u64 = snap
        .mailboxes
        .iter()
        .filter(|m| m.selected)
        .map(|m| u64::from(m.message_count))
        .sum();
    S::confirm_summary_fmt(
        &source_kind_label(snap.source_kind),
        &selected.to_string(),
        &messages.to_string(),
    )
}

fn progress_text(snap: &MailImportSnapshot) -> String {
    S::progress_summary_fmt(
        &snap.imported_count.to_string(),
        &snap.total_count.to_string(),
        &snap.skipped_count.to_string(),
        &snap.errored_count.to_string(),
    )
}

fn done_text(snap: &MailImportSnapshot) -> String {
    S::done_summary_fmt(
        &snap.imported_count.to_string(),
        &snap.skipped_count.to_string(),
        &snap.errored_count.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;
    use fauna_client_mail_settings::SourceMailboxOption;
    use fauna_client_mail_settings::testing::a_mail_import_snapshot;

    fn a_snapshot() -> MailImportSnapshot {
        a_mail_import_snapshot(ImportStep::Confirm)
    }

    /// The three wizard summary lines render through the GENERATED i18n
    /// templates, not hard-coded English — the `mail_export.rs` pin, applied to
    /// this page from its first commit so the duplication had to
    /// clean up on the export twin can never start here. The **rendered text**
    /// is asserted literally AND against `S::*_summary_fmt` called directly, so
    /// swapping in a local `format!` goes red.
    #[test]
    fn summary_lines_render_through_the_i18n_templates() {
        for key in [
            "mail_import.confirm_summary_fmt",
            "mail_import.progress_summary_fmt",
            "mail_import.done_summary_fmt",
            "mail_import.progress_row_fmt",
        ] {
            assert!(
                crate::i18n::strings::lookup(key).is_some(),
                "{key} must resolve — a summary line rendered off a missing key is \
                 hard-coded English wearing an i18n costume"
            );
        }

        let mut snap = a_snapshot();
        snap.mailboxes = vec![
            SourceMailboxOption {
                name: "INBOX".into(),
                selected: true,
                message_count: 7,
            },
            SourceMailboxOption {
                name: "Trash".into(),
                selected: false,
                message_count: 99,
            },
        ];
        // Only SELECTED mailboxes count toward either figure.
        assert_eq!(confirm_text(&snap), "Gmail · 1 mailbox(es) · 7 messages");
        assert_eq!(
            confirm_text(&snap),
            S::confirm_summary_fmt(&source_kind_label(snap.source_kind), "1", "7")
        );

        let mut snap = a_snapshot();
        snap.imported_count = 3;
        snap.total_count = 10;
        snap.skipped_count = 1;
        snap.errored_count = 2;
        assert_eq!(progress_text(&snap), "3 of 10 · 1 skipped · 2 errored");
        assert_eq!(
            progress_text(&snap),
            S::progress_summary_fmt("3", "10", "1", "2")
        );

        let mut snap = a_snapshot();
        snap.imported_count = 12;
        snap.skipped_count = 4;
        snap.errored_count = 1;
        assert_eq!(done_text(&snap), "12 imported · 4 skipped · 1 errored");
        assert_eq!(done_text(&snap), S::done_summary_fmt("12", "4", "1"));
    }

    /// The module docs' per-provider field table, as code.
    ///
    /// Also the pin on the SYNCHRONOUS half: this is a pure function of the
    /// pick precisely so the picker's handler can call it before dispatching,
    /// and `test_mail_import.py::_fill_generic_source` types into
    /// `mail-import-source-host` with no wait after choosing Generic IMAP.
    /// Route the reveal through a round trip again and that walk 404s.
    #[test]
    fn each_provider_reveals_exactly_its_own_source_fields() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (_page, w, _buttons) = build_page_parts();

            // Gmail / iCloud: app-password + its help line, no IMAP fields.
            for kind in [ImportSourceKind::Gmail, ImportSourceKind::ICloud] {
                apply_source_visibility(&w, kind);
                assert!(w.username_row.is_visible(), "{kind:?} must show username");
                assert!(
                    w.app_password_row.is_visible(),
                    "{kind:?} wants app-password"
                );
                assert!(
                    w.app_password_help.is_visible(),
                    "{kind:?} wants the help line"
                );
                assert!(!w.oauth_row.is_visible(), "{kind:?} has no OAuth button");
                for (name, row) in [
                    ("host", &w.host_row),
                    ("port", &w.port_row),
                    ("tls-mode", &w.tls_mode_row),
                    ("password", &w.password_row),
                ] {
                    assert!(!row.is_visible(), "{kind:?} must not show {name}");
                }
            }
            // The help line is provider-specific, not one generic sentence.
            apply_source_visibility(&w, ImportSourceKind::Gmail);
            assert_eq!(
                w.app_password_help.label(),
                S::SOURCE_APP_PASSWORD_HELP_GMAIL
            );
            apply_source_visibility(&w, ImportSourceKind::ICloud);
            assert_eq!(
                w.app_password_help.label(),
                S::SOURCE_APP_PASSWORD_HELP_ICLOUD
            );

            // Outlook: the OAuth button AND the IMAP fallback beside it.
            apply_source_visibility(&w, ImportSourceKind::Outlook);
            assert!(w.oauth_row.is_visible(), "Outlook shows the OAuth start");
            assert!(
                !w.app_password_row.is_visible(),
                "Outlook has no app-password"
            );
            for (name, row) in [
                ("host", &w.host_row),
                ("port", &w.port_row),
                ("tls-mode", &w.tls_mode_row),
                ("password", &w.password_row),
            ] {
                assert!(row.is_visible(), "Outlook's IMAP fallback needs {name}");
            }

            // Generic: the IMAP fields alone.
            apply_source_visibility(&w, ImportSourceKind::Generic);
            assert!(!w.oauth_row.is_visible(), "Generic has no OAuth button");
            assert!(
                !w.app_password_row.is_visible(),
                "Generic has no app-password"
            );
            assert!(
                !w.app_password_help.is_visible(),
                "Generic has no help line"
            );
            for (name, row) in [
                ("host", &w.host_row),
                ("port", &w.port_row),
                ("tls-mode", &w.tls_mode_row),
                ("password", &w.password_row),
            ] {
                assert!(row.is_visible(), "Generic requires {name}");
            }
        });
    }

    /// Picking Outlook painted an empty host/port, because nothing
    /// re-seeded the Source-step drafts from the snapshot `SelectSourceKind`
    /// just wrote — so `connect_actions` sent the caller's stale/empty value
    /// verbatim and the connection attempt dialed nothing. Pins
    /// the fix: the seed is unconditional (a stale draft from a previous kind never
    /// survives a kind change), matching apple's `syncDraftsFromSnapshot`.
    #[test]
    fn selecting_a_source_kind_seeds_the_host_and_port_drafts() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (_page, w, _buttons) = build_page_parts();
            w.host_input.set_text("stale.example.com");
            w.port_input.set_text("2525");

            let mut snap = a_mail_import_snapshot(ImportStep::Source);
            snap.source_kind = ImportSourceKind::Outlook;
            snap.host = "outlook.office365.com".to_string();
            snap.port = 993;
            seed_source_drafts(&w, &snap);

            assert_eq!(w.host_input.text(), "outlook.office365.com");
            assert_eq!(w.port_input.text(), "993");
        });
    }

    /// The `state` marker class REPLACES, never accumulates.
    ///
    /// `automation::agent::attr` returns the first `test-attr-state-*` class it
    /// finds and that answer wins over the widget's own state, so a row left
    /// carrying both classes — or carrying a stale one after a toggle — reports
    /// a selection the user does not have. `test_mail_import.py` reads this
    /// immediately after `toggle_mailbox`, with no wait.
    #[test]
    fn the_row_state_marker_replaces_rather_than_accumulates() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let cb = gtk::CheckButton::with_label("INBOX");

            mark_row_state(&cb, true);
            let classes: Vec<String> = cb.css_classes().iter().map(|c| c.to_string()).collect();
            assert!(classes.iter().any(|c| c == "test-attr-state-on"));
            assert!(!classes.iter().any(|c| c == "test-attr-state-off"));

            mark_row_state(&cb, false);
            let classes: Vec<String> = cb.css_classes().iter().map(|c| c.to_string()).collect();
            assert!(
                classes.iter().any(|c| c == "test-attr-state-off"),
                "off must be marked; have {classes:?}"
            );
            assert!(
                !classes.iter().any(|c| c == "test-attr-state-on"),
                "the previous on-marker must be GONE — the agent answers whichever \
                 it finds first, so a leftover reports a stale selection; have {classes:?}"
            );
        });
    }

    /// Both picker maps round-trip, and an out-of-range index falls back to the
    /// ratified default rather than panicking (`format_at`'s contract).
    #[test]
    fn picker_index_maps_round_trip() {
        for kind in SOURCE_KINDS {
            assert_eq!(source_kind_at(source_kind_index(kind)), kind);
        }
        for mode in TLS_MODES {
            assert_eq!(tls_mode_at(tls_mode_index(mode)), mode);
        }
        assert_eq!(source_kind_at(99), ImportSourceKind::Gmail);
        assert_eq!(tls_mode_at(99), ImportTlsMode::Implicit);
    }

    /// The Import page exposes every static ui.yaml ID for the `mail-import`
    /// wizard, including the two SHARED `wizard-*-button` ids. The indexed
    /// `mail-import-scope-mailbox-item` rows and the
    /// `mail-import-mailbox-progress-list-item*` rows are added from the
    /// snapshot at render time, so they're not asserted here.
    #[test]
    fn import_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let page = build_mail_import_page();
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "error-message",
                "mail-import-source-picker",
                "mail-import-source-app-password",
                "mail-import-source-oauth-button",
                "mail-import-source-host",
                "mail-import-source-port",
                "mail-import-source-tls-mode",
                "mail-import-source-username",
                "mail-import-source-password",
                "mail-import-connect-button",
                "mail-import-scope-mailboxes",
                "mail-import-scope-date-from",
                "mail-import-scope-max-size",
                "mail-import-scope-mailbox-mapping",
                "wizard-next-button",
                "wizard-back-button",
                "mail-import-confirm-summary",
                "mail-import-start-button",
                "mail-import-progress-summary",
                "mail-import-progress-bar",
                "mail-import-pause-button",
                "mail-import-resume-button",
                "mail-import-cancel-button",
                "mail-import-error-log",
                "mail-import-done-summary",
                "mail-import-view-imported-button",
                "mail-import-review-skipped-button",
                "mail-import-mailbox-progress-list",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }

    /// Only ONE step's group is visible at a time — the invariant that makes
    /// the repeated `wizard-back-button` / `wizard-next-button` ids
    /// unambiguous to `automation::find` (which prunes non-showing subtrees).
    ///
    /// Asserted at BUILD time, before any render: an ambiguous window between
    /// build and first hydrate would be a real flake, which is why the
    /// non-Source groups are built `visible(false)` rather than merely hidden
    /// on the first render.
    #[test]
    fn only_the_source_step_is_visible_before_the_first_render() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let page = build_mail_import_page();
            // Both wizard nav ids exist twice in the tree (Scope + Confirm)...
            let names = widget_names(&page);
            assert_eq!(
                names.iter().filter(|n| *n == "wizard-back-button").count(),
                2,
                "Scope and Confirm each carry their own Back"
            );
            // ...but only the Source step's widgets are *showing*, so the
            // automation finder sees neither of them yet.
            let root: gtk::Widget = page.clone().upcast();
            assert!(
                crate::automation::find::find_in(&root, "mail-import-connect-button").is_some(),
                "the Source step must be the visible one at build time"
            );
            for id in [
                "wizard-back-button",
                "wizard-next-button",
                "mail-import-start-button",
                "mail-import-pause-button",
                "mail-import-done-summary",
            ] {
                assert!(
                    crate::automation::find::find_in(&root, id).is_none(),
                    "{id} must not be findable before its step is active",
                );
            }
        });
    }
}
